//! Parser for the Sentry envelope format.
//!
//! Grammar, per <https://develop.sentry.dev/sdk/data-model/envelopes/>:
//!
//! ```text
//! Envelope = Headers { "\n" Item } [ "\n" ] ;
//! Item     = Headers "\n" Payload ;
//! ```
//!
//! Headers are single-line UTF-8 JSON objects. An item payload is either exactly `length` bytes
//! (when the item header declares one) or everything up to the next newline.
//!
//! This parser borrows payloads out of the input buffer rather than copying them. We buffer the
//! whole request body instead of streaming it because the request size is capped well below the
//! 100 MiB envelope ceiling — that ceiling exists for attachments and minidumps, which we don't
//! accept yet.

use serde::Deserialize;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("envelope is empty")]
    Empty,

    #[error("{0} header is not valid UTF-8")]
    NotUtf8(&'static str),

    #[error("{context} header is not valid JSON: {reason}")]
    NotJson {
        context: &'static str,
        reason: String,
    },

    #[error("item declares length {declared} but only {available} bytes remain")]
    TruncatedItem { declared: usize, available: usize },

    #[error("item with an explicit length is not terminated by a newline")]
    UnterminatedItem,

    #[error("envelope declares more than {limit} items")]
    TooManyItems { limit: usize },
}

/// Maximum items accepted in one envelope.
///
/// Sentry's own limit is 100; this is deliberately looser so no real SDK batch is refused. The cap
/// exists because the item list is the parser's only unbounded allocation — each entry costs about
/// 100 bytes, so without a ceiling a body of minimal 4-byte items (`{}\n\n`) turns into an item
/// list some four orders of magnitude larger than the bytes that produced it.
pub const MAX_ITEMS: usize = 1000;

/// Envelope-level headers. Unknown keys are ignored; unknown keys are legal and we have no reason
/// to round-trip them.
#[derive(Debug, Default, Deserialize)]
pub struct EnvelopeHeaders {
    pub event_id: Option<String>,
    /// Present when the SDK self-authenticates the envelope rather than sending `X-Sentry-Auth`.
    pub dsn: Option<String>,
    pub sent_at: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ItemHeaders {
    /// Item type: `event`, `session`, `transaction`, `attachment`, … Absent in malformed input.
    #[serde(default)]
    pub r#type: Option<String>,
    pub length: Option<usize>,
    pub content_type: Option<String>,
    pub filename: Option<String>,
}

#[derive(Debug)]
pub struct Item<'a> {
    pub headers: ItemHeaders,
    pub payload: &'a [u8],
}

impl Item<'_> {
    pub fn item_type(&self) -> &str {
        self.headers.r#type.as_deref().unwrap_or("")
    }
}

#[derive(Debug)]
pub struct Envelope<'a> {
    pub headers: EnvelopeHeaders,
    pub items: Vec<Item<'a>>,
}

pub fn parse(input: &[u8]) -> Result<Envelope<'_>, ParseError> {
    let mut cursor = Cursor { input, pos: 0 };
    let headers = read_envelope_headers(&mut cursor)?;

    let mut items = Vec::new();
    while let Some(line) = cursor.read_line() {
        // A trailing newline after the last item leaves an empty line; that is legal.
        if line.is_empty() {
            continue;
        }

        // Checked before the item is parsed, so an over-long envelope is refused rather than
        // partially built. Exactly `MAX_ITEMS` items are accepted.
        if items.len() >= MAX_ITEMS {
            return Err(ParseError::TooManyItems { limit: MAX_ITEMS });
        }

        let item_headers: ItemHeaders = parse_headers(line, "item")?;

        let payload = match item_headers.length {
            Some(length) => {
                let payload = cursor.take(length).ok_or(ParseError::TruncatedItem {
                    declared: length,
                    available: cursor.remaining(),
                })?;
                // The payload must be followed by a newline, or be at EOF.
                match cursor.read_line() {
                    None | Some([]) => {}
                    Some(_) => return Err(ParseError::UnterminatedItem),
                }
                payload
            }
            // Without a declared length the payload runs to the next newline, which means it
            // cannot itself contain one.
            None => cursor.read_line().unwrap_or(&[]),
        };

        items.push(Item {
            headers: item_headers,
            payload,
        });
    }

    Ok(Envelope { headers, items })
}

/// Parses only the envelope's own headers, leaving the item list untouched.
///
/// The `dsn` credential lives on the first line, so this is all that needs to be read before
/// authenticating. Kept separate from [`parse`] deliberately: building the item list is work
/// proportional to the whole body, and must not happen on behalf of a caller who has not
/// authenticated yet. Callers pass a prefix, so a header line longer than that prefix fails to
/// parse here — which correctly denies the request rather than decoding more of the body.
pub fn parse_envelope_headers(input: &[u8]) -> Result<EnvelopeHeaders, ParseError> {
    read_envelope_headers(&mut Cursor { input, pos: 0 })
}

fn read_envelope_headers(cursor: &mut Cursor<'_>) -> Result<EnvelopeHeaders, ParseError> {
    let header_line = cursor.read_line().ok_or(ParseError::Empty)?;
    if header_line.is_empty() {
        return Err(ParseError::Empty);
    }
    parse_headers(header_line, "envelope")
}

fn parse_headers<'de, T: Deserialize<'de>>(
    line: &'de [u8],
    context: &'static str,
) -> Result<T, ParseError> {
    // Validate UTF-8 explicitly so a bad encoding is reported as such rather than as bad JSON.
    std::str::from_utf8(line).map_err(|_| ParseError::NotUtf8(context))?;

    serde_json::from_slice(line).map_err(|e| ParseError::NotJson {
        context,
        reason: e.to_string(),
    })
}

struct Cursor<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    /// Reads up to the next `\n`, consuming it. Returns `None` at end of input.
    fn read_line(&mut self) -> Option<&'a [u8]> {
        if self.pos >= self.input.len() {
            return None;
        }

        let rest = &self.input[self.pos..];
        match rest.iter().position(|&b| b == b'\n') {
            Some(idx) => {
                self.pos += idx + 1;
                Some(&rest[..idx])
            }
            None => {
                self.pos = self.input.len();
                Some(rest)
            }
        }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        if end > self.input.len() {
            return None;
        }
        let slice = &self.input[self.pos..end];
        self.pos = end;
        Some(slice)
    }

    fn remaining(&self) -> usize {
        self.input.len().saturating_sub(self.pos)
    }
}

#[cfg(test)]
#[path = "envelope_tests.rs"]
mod tests;
