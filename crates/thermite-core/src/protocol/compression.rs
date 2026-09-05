//! Request body decompression.
//!
//! SDKs vary: sentry-rust sends envelopes uncompressed, sentry-python and the JavaScript SDKs send
//! gzip. `Content-Type` is not reliable (sentry-rust sets none), so the encoding is taken purely
//! from `Content-Encoding`.

use std::io::Read;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("unsupported Content-Encoding: {0}")]
    UnsupportedEncoding(String),

    #[error("malformed {encoding} body: {source}")]
    Malformed {
        encoding: &'static str,
        source: std::io::Error,
    },

    #[error("decompressed body exceeds the {limit} byte limit")]
    TooLarge { limit: usize },
}

/// Decompresses `body` according to `content_encoding`.
///
/// `limit` caps the *decompressed* size. Without it a small compressed body could expand to
/// exhaust memory, since the request-size limit only bounds what arrives on the wire.
pub fn decode(
    body: &[u8],
    content_encoding: Option<&str>,
    limit: usize,
) -> Result<Vec<u8>, DecodeError> {
    let encoding = content_encoding.unwrap_or("").trim();

    match encoding {
        "" | "identity" => {
            if body.len() > limit {
                return Err(DecodeError::TooLarge { limit });
            }
            Ok(body.to_vec())
        }
        "gzip" | "x-gzip" => read_capped(flate2::read::GzDecoder::new(body), "gzip", limit),
        "deflate" => read_capped(flate2::read::ZlibDecoder::new(body), "deflate", limit),
        "br" => read_capped(brotli::Decompressor::new(body, 8192), "br", limit),
        "zstd" => {
            let decoder = zstd::stream::read::Decoder::new(body).map_err(|source| {
                DecodeError::Malformed {
                    encoding: "zstd",
                    source,
                }
            })?;
            read_capped(decoder, "zstd", limit)
        }
        other => Err(DecodeError::UnsupportedEncoding(other.to_string())),
    }
}

/// Decompresses at most `limit` bytes, truncating rather than erroring when the body is longer.
///
/// Used to read the envelope's first line *before* authenticating. The `dsn` envelope header is a
/// valid credential source, but decoding a whole body for a caller who has not authenticated is a
/// memory-amplification lever, so only a header-sized prefix is ever decoded there.
///
/// Returns `None` when nothing can be decoded. A caller that finds no credential in the prefix must
/// reject the request as unauthenticated — it must not fall back to decoding the remainder.
pub fn decode_prefix(body: &[u8], content_encoding: Option<&str>, limit: usize) -> Option<Vec<u8>> {
    let encoding = content_encoding.unwrap_or("").trim();

    match encoding {
        "" | "identity" => Some(body[..body.len().min(limit)].to_vec()),
        "gzip" | "x-gzip" => read_prefix(flate2::read::GzDecoder::new(body), limit),
        "deflate" => read_prefix(flate2::read::ZlibDecoder::new(body), limit),
        "br" => read_prefix(brotli::Decompressor::new(body, 8192), limit),
        "zstd" => read_prefix(zstd::stream::read::Decoder::new(body).ok()?, limit),
        _ => None,
    }
}

/// Reads up to `limit` bytes, keeping whatever arrived if the stream then fails.
///
/// Cutting a compressed stream short is expected here, so a decoder error after some output has
/// been produced is not fatal: the prefix is all the caller wants.
fn read_prefix<R: Read>(reader: R, limit: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    match reader.take(limit as u64).read_to_end(&mut out) {
        Ok(_) => Some(out),
        Err(_) if !out.is_empty() => Some(out),
        Err(_) => None,
    }
}

/// Reads at most `limit` bytes, erroring rather than truncating if the stream is longer.
fn read_capped<R: Read>(
    reader: R,
    encoding: &'static str,
    limit: usize,
) -> Result<Vec<u8>, DecodeError> {
    let mut out = Vec::new();
    // Read one byte past the limit so we can tell "exactly at the limit" from "over it".
    let read = reader
        .take(limit as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|source| DecodeError::Malformed { encoding, source })?;

    if read > limit {
        return Err(DecodeError::TooLarge { limit });
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    const LIMIT: usize = 1024 * 1024;

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    fn deflate(data: &[u8]) -> Vec<u8> {
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn identity_and_absent_encoding_pass_through() {
        for encoding in [None, Some(""), Some("identity")] {
            assert_eq!(decode(b"hello", encoding, LIMIT).unwrap(), b"hello");
        }
    }

    #[test]
    fn round_trips_gzip() {
        let body = gzip(b"an envelope");
        assert_eq!(decode(&body, Some("gzip"), LIMIT).unwrap(), b"an envelope");
        // Some older clients send x-gzip.
        assert_eq!(
            decode(&body, Some("x-gzip"), LIMIT).unwrap(),
            b"an envelope"
        );
    }

    #[test]
    fn round_trips_deflate() {
        let body = deflate(b"an envelope");
        assert_eq!(
            decode(&body, Some("deflate"), LIMIT).unwrap(),
            b"an envelope"
        );
    }

    #[test]
    fn round_trips_zstd() {
        let body = zstd::encode_all(&b"an envelope"[..], 0).unwrap();
        assert_eq!(decode(&body, Some("zstd"), LIMIT).unwrap(), b"an envelope");
    }

    #[test]
    fn round_trips_brotli() {
        let mut body = Vec::new();
        let mut encoder = brotli::CompressorWriter::new(&mut body, 4096, 5, 22);
        encoder.write_all(b"an envelope").unwrap();
        drop(encoder);

        assert_eq!(decode(&body, Some("br"), LIMIT).unwrap(), b"an envelope");
    }

    #[test]
    fn encoding_value_is_trimmed() {
        let body = gzip(b"hi");
        assert_eq!(decode(&body, Some(" gzip "), LIMIT).unwrap(), b"hi");
    }

    #[test]
    fn unsupported_encoding_is_rejected() {
        let err = decode(b"x", Some("snappy"), LIMIT).unwrap_err();
        assert!(matches!(err, DecodeError::UnsupportedEncoding(e) if e == "snappy"));
    }

    #[test]
    fn malformed_compressed_body_is_rejected() {
        let err = decode(b"definitely not gzip", Some("gzip"), LIMIT).unwrap_err();
        assert!(matches!(
            err,
            DecodeError::Malformed {
                encoding: "gzip",
                ..
            }
        ));
    }

    #[test]
    fn identity_body_over_the_limit_is_rejected() {
        let err = decode(&[0u8; 100], None, 10).unwrap_err();
        assert!(matches!(err, DecodeError::TooLarge { limit: 10 }));
    }

    #[test]
    fn decompression_bomb_is_rejected_at_the_limit() {
        // 10 MiB of zeroes compresses to a few KiB; the wire-size limit would not catch it.
        let body = gzip(&vec![0u8; 10 * 1024 * 1024]);
        assert!(body.len() < 100 * 1024);

        let err = decode(&body, Some("gzip"), 1024).unwrap_err();
        assert!(matches!(err, DecodeError::TooLarge { limit: 1024 }));
    }

    #[test]
    fn a_body_exactly_at_the_limit_is_accepted() {
        let body = gzip(&vec![b'a'; 1000]);
        assert_eq!(decode(&body, Some("gzip"), 1000).unwrap().len(), 1000);
    }

    #[test]
    fn decode_prefix_truncates_instead_of_erroring() {
        // This is the pre-auth path, so a body longer than the prefix must yield the prefix rather
        // than the `TooLarge` error `decode` would return.
        let body = gzip(b"first line\nsecond line\n");
        let prefix = decode_prefix(&body, Some("gzip"), 11).unwrap();
        assert_eq!(prefix, b"first line\n");
    }

    #[test]
    fn decode_prefix_bounds_a_decompression_bomb() {
        // The whole point: 10 MiB of zeroes must cost the prefix budget, not 10 MiB.
        let body = gzip(&vec![0u8; 10 * 1024 * 1024]);
        assert_eq!(
            decode_prefix(&body, Some("gzip"), 8192).unwrap().len(),
            8192
        );
    }

    #[test]
    fn decode_prefix_handles_every_supported_encoding() {
        let plain = b"{\"dsn\":\"http://key@host/1\"}\n";

        let mut brotli_body = Vec::new();
        let mut encoder = brotli::CompressorWriter::new(&mut brotli_body, 4096, 5, 22);
        encoder.write_all(plain).unwrap();
        drop(encoder);

        let bodies = [
            (None, plain.to_vec()),
            (Some("identity"), plain.to_vec()),
            (Some("gzip"), gzip(plain)),
            (Some("deflate"), deflate(plain)),
            (Some("zstd"), zstd::encode_all(&plain[..], 0).unwrap()),
            (Some("br"), brotli_body),
        ];

        for (encoding, body) in bodies {
            assert_eq!(
                decode_prefix(&body, encoding, 8192).as_deref(),
                Some(&plain[..]),
                "failed for {encoding:?}"
            );
        }
    }

    #[test]
    fn decode_prefix_rejects_what_it_cannot_decode() {
        // No credential can be recovered, so the caller must treat the request as unauthenticated.
        assert!(decode_prefix(b"not gzip at all", Some("gzip"), 8192).is_none());
        assert!(decode_prefix(b"x", Some("snappy"), 8192).is_none());
    }

    #[test]
    fn decode_prefix_shorter_than_the_limit_returns_everything() {
        let body = gzip(b"tiny");
        assert_eq!(decode_prefix(&body, Some("gzip"), 8192).unwrap(), b"tiny");
    }
}
