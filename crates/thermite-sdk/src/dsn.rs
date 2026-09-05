//! Parsing the DSN into the one URL the SDK actually posts to.
//!
//! A DSN is `scheme://public_key@host[:port][/path]/project_id`, which thermite generates in
//! `thermite_core::config::Config::dsn`. The `/path` part is real — a thermite mounted under a
//! sub-path emits `https://key@errors.example.com/thermite/42`, and the ingest URL has to keep
//! that prefix.
//!
//! Parsed by hand rather than with the `url` crate. `url` pulls in IDNA and Unicode tables to
//! answer questions this grammar cannot ask, and every byte of that is dead weight in a wasm
//! bundle whose entire reason for existing is being smaller than the SDK it replaces.

/// A parsed DSN, reduced to what sending needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dsn {
    /// The DSN's userinfo: not a secret in the usual sense, it only grants the ability to send
    /// events to one project.
    pub public_key: String,
    pub project_id: i64,
    /// The full ingest endpoint, credentials included.
    ///
    /// Thermite resolves credentials from `?sentry_key=` first, then `X-Sentry-Auth`, then the
    /// envelope's `dsn` header. The query parameter is the only one of the three a browser
    /// `fetch` can always set without tripping a CORS preflight, so it is the one used here.
    pub ingest_url: String,
    /// The DSN as given, for the envelope's `dsn` header.
    pub raw: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DsnError {
    #[error("DSN has no scheme, expected `<scheme>://<key>@<host>/<project_id>`")]
    NoScheme,

    #[error("DSN scheme `{0}` is not supported, expected `http` or `https`")]
    UnsupportedScheme(String),

    #[error("DSN has no public key, expected `<scheme>://<key>@<host>/<project_id>`")]
    NoPublicKey,

    #[error("DSN has no host")]
    NoHost,

    #[error("DSN does not end in a positive integer project id")]
    NoProjectId,
}

impl Dsn {
    pub fn parse(raw: &str) -> Result<Self, DsnError> {
        let raw = raw.trim();

        let (scheme, rest) = raw.split_once("://").ok_or(DsnError::NoScheme)?;
        if !matches!(scheme, "http" | "https") {
            return Err(DsnError::UnsupportedScheme(scheme.to_string()));
        }

        let (userinfo, host_path) = rest.split_once('@').ok_or(DsnError::NoPublicKey)?;

        // Pre-9 Sentry DSNs carry `public_key:secret`. The secret half has been ignored by the
        // protocol for years, and thermite's own extractor reads the userinfo the same way.
        let public_key = userinfo.split(':').next().unwrap_or_default();
        if public_key.is_empty() {
            return Err(DsnError::NoPublicKey);
        }

        // A trailing slash is not in the grammar, but it is a plausible copy-paste and costs one
        // call to tolerate.
        let (host_and_path, project) = host_path
            .trim_end_matches('/')
            .rsplit_once('/')
            .ok_or(DsnError::NoProjectId)?;

        if host_and_path.is_empty() {
            return Err(DsnError::NoHost);
        }

        // Ingest routes on an integer project id: SDKs parse it out of the DSN path, so a DSN that
        // does not end in one could never have worked.
        let project_id: i64 = project.parse().map_err(|_| DsnError::NoProjectId)?;
        if project_id <= 0 {
            return Err(DsnError::NoProjectId);
        }

        Ok(Self {
            ingest_url: format!(
                "{scheme}://{host_and_path}/api/{project_id}/envelope/?sentry_key={public_key}"
            ),
            public_key: public_key.to_string(),
            project_id,
            raw: raw.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splices_host_and_port_into_the_ingest_url() {
        let dsn = Dsn::parse("http://abc123@localhost:9000/42").unwrap();

        assert_eq!(dsn.public_key, "abc123");
        assert_eq!(dsn.project_id, 42);
        assert_eq!(
            dsn.ingest_url,
            "http://localhost:9000/api/42/envelope/?sentry_key=abc123"
        );
    }

    /// The mirror of `thermite_core::config`'s `dsn_preserves_sub_path`: a thermite mounted under
    /// a prefix emits that prefix in its DSN, and dropping it here would post to a 404.
    #[test]
    fn preserves_a_sub_path() {
        let dsn = Dsn::parse("https://abc123@errors.example.com/thermite/42").unwrap();

        assert_eq!(
            dsn.ingest_url,
            "https://errors.example.com/thermite/api/42/envelope/?sentry_key=abc123"
        );
    }

    #[test]
    fn discards_a_legacy_secret_key() {
        let dsn = Dsn::parse("https://public:secret@example.com/1").unwrap();

        assert_eq!(dsn.public_key, "public");
        assert_eq!(
            dsn.ingest_url,
            "https://example.com/api/1/envelope/?sentry_key=public"
        );
    }

    #[test]
    fn tolerates_a_trailing_slash() {
        let dsn = Dsn::parse("https://abc123@example.com/7/").unwrap();

        assert_eq!(dsn.project_id, 7);
    }

    /// The `dsn` envelope header is sent verbatim, so it has to survive parsing unchanged.
    #[test]
    fn keeps_the_raw_dsn() {
        let dsn = Dsn::parse("  http://abc123@localhost:9000/42  ").unwrap();

        assert_eq!(dsn.raw, "http://abc123@localhost:9000/42");
    }

    #[test]
    fn rejects_dsns_that_could_never_have_worked() {
        assert_eq!(Dsn::parse("not a url"), Err(DsnError::NoScheme));
        assert_eq!(
            Dsn::parse("ftp://abc123@example.com/1"),
            Err(DsnError::UnsupportedScheme("ftp".to_string()))
        );
        assert_eq!(
            Dsn::parse("https://example.com/1"),
            Err(DsnError::NoPublicKey)
        );
        assert_eq!(
            Dsn::parse("https://@example.com/1"),
            Err(DsnError::NoPublicKey)
        );
        assert_eq!(
            Dsn::parse("https://abc123@example.com/slug"),
            Err(DsnError::NoProjectId)
        );
        assert_eq!(
            Dsn::parse("https://abc123@example.com/-1"),
            Err(DsnError::NoProjectId)
        );
        assert_eq!(
            Dsn::parse("https://abc123@example.com"),
            Err(DsnError::NoProjectId)
        );
        assert_eq!(Dsn::parse("https://abc123@/1"), Err(DsnError::NoHost));
    }
}
