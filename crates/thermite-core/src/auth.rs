//! Ingest credentials.
//!
//! Only the SDK-facing side lives here. Operator and agent credentials — API keys, sessions,
//! OAuth — belong to the application crate; this crate has no opinion on them.

/// Parses an `X-Sentry-Auth` header value into its key/value pairs.
///
/// The format is `Sentry key=value, key=value`, for example:
/// `Sentry sentry_key=abc123, sentry_version=7, sentry_client=sentry.rust/0.49.0`
///
/// Returns an empty vec for anything malformed. A broken header is indistinguishable from a
/// missing one as far as we are concerned — both mean "no credentials" — and an SDK sending
/// garbage should get a clean 401 rather than a 500.
pub fn parse_sentry_auth(value: &str) -> Vec<(&str, &str)> {
    let Some(pairs) = value.strip_prefix("Sentry ") else {
        return Vec::new();
    };

    pairs
        .split(',')
        .filter_map(|pair| {
            let (k, v) = pair.trim().split_once('=')?;
            Some((k.trim(), v.trim()))
        })
        .collect()
}

/// Extracts `sentry_key` from an `X-Sentry-Auth` header value.
pub fn sentry_key_from_auth_header(value: &str) -> Option<String> {
    parse_sentry_auth(value)
        .into_iter()
        .find(|(k, _)| *k == "sentry_key")
        .map(|(_, v)| v.to_string())
        .filter(|key| !key.is_empty())
}

/// Extracts the public key from a full DSN, i.e. the userinfo of
/// `scheme://public_key@host/project_id`.
///
/// Some SDKs (and the browser tunnel) put a whole DSN in the envelope header instead of sending an
/// auth header.
pub fn sentry_key_from_dsn(dsn: &str) -> Option<String> {
    let parsed = url::Url::parse(dsn).ok()?;
    let username = parsed.username();
    (!username.is_empty()).then(|| username.to_string())
}

/// Generates a new DSN public key. 32 hex characters, matching the shape Sentry uses.
///
/// Not a secret in the usual sense: it only grants the ability to send events to one project.
pub fn generate_public_key() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_sentry_rust_header() {
        let header = "Sentry sentry_key=abc123, sentry_version=7, sentry_client=sentry.rust/0.49.0";

        assert_eq!(
            parse_sentry_auth(header),
            vec![
                ("sentry_key", "abc123"),
                ("sentry_version", "7"),
                ("sentry_client", "sentry.rust/0.49.0"),
            ]
        );
        assert_eq!(
            sentry_key_from_auth_header(header).as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn tolerates_a_trailing_secret_key() {
        // sentry_secret is deprecated but old SDKs still send it.
        let header = "Sentry sentry_key=pub, sentry_secret=sec, sentry_version=7";
        assert_eq!(sentry_key_from_auth_header(header).as_deref(), Some("pub"));
    }

    #[test]
    fn malformed_headers_yield_no_credentials() {
        for header in [
            "",
            "Sentry",
            "Basic dXNlcjpwYXNz",
            "Sentry garbage",
            "Sentry =",
            "Sentry sentry_key=",
            "sentry_key=abc123",
        ] {
            assert_eq!(
                sentry_key_from_auth_header(header),
                None,
                "expected no key from {header:?}"
            );
        }
    }

    #[test]
    fn values_containing_equals_are_kept_whole() {
        let header = "Sentry sentry_key=abc, sentry_client=my/sdk=1.0";
        assert_eq!(
            parse_sentry_auth(header).last(),
            Some(&("sentry_client", "my/sdk=1.0"))
        );
    }

    #[test]
    fn extracts_key_from_dsn() {
        assert_eq!(
            sentry_key_from_dsn("http://abc123@localhost:9000/1").as_deref(),
            Some("abc123")
        );
        assert_eq!(
            sentry_key_from_dsn("https://abc123@errors.example.com/thermite/42").as_deref(),
            Some("abc123")
        );
        assert_eq!(sentry_key_from_dsn("http://localhost:9000/1"), None);
        assert_eq!(sentry_key_from_dsn("not a url"), None);
    }

    #[test]
    fn generated_public_keys_have_the_expected_shape() {
        let key = generate_public_key();
        assert_eq!(key.len(), 32);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(generate_public_key(), generate_public_key());
    }
}
