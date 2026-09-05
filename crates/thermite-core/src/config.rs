use std::env;

use crate::protocol::scrub::ScrubList;

/// Ingest tuning. The database URL and bind address belong to the application, not here.
#[derive(Debug, Clone)]
pub struct Config {
    /// Public base URL, used to build DSNs. Never has a trailing slash.
    pub base_url: String,
    pub max_envelope_bytes: usize,
    /// Events per project per rolling minute. `None` disables rate limiting.
    pub rate_limit_per_minute: Option<u32>,
    /// Keys scrubbed from stored event payloads: the defaults plus `THERMITE_SCRUB_FIELDS`.
    pub scrub_fields: ScrubList,
}

impl Config {
    /// Reads ingest tuning from the environment, taking `base_url` from the application so DSNs
    /// and the dashboard agree on the deployment's public address.
    ///
    /// Malformed numbers fall back to the default rather than failing startup: a typo in a tuning
    /// variable should not stop a deployment from accepting errors.
    pub fn from_env(base_url: &str) -> Self {
        let rate_limit = parse_env("THERMITE_RATE_LIMIT_PER_MINUTE", 3000u32);
        let scrub_extra = env::var("THERMITE_SCRUB_FIELDS").unwrap_or_default();

        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            max_envelope_bytes: parse_env("THERMITE_MAX_ENVELOPE_BYTES", 20 * 1024 * 1024),
            rate_limit_per_minute: (rate_limit > 0).then_some(rate_limit),
            scrub_fields: ScrubList::new(scrub_extra.split(',').map(str::trim)),
        }
    }

    /// The DSN an SDK should be configured with to report into `project_id`.
    pub fn dsn(&self, public_key: &str, project_id: i64) -> String {
        // Sentry SDKs parse `scheme://public_key@host[:port][/path]/project_id`, so the key is
        // spliced in after the scheme rather than appended.
        match self.base_url.split_once("://") {
            Some((scheme, rest)) => format!("{scheme}://{public_key}@{rest}/{project_id}"),
            None => format!("{}/{}", self.base_url, project_id),
        }
    }
}

fn parse_env<T: std::str::FromStr>(key: &str, default: T) -> T {
    let Ok(raw) = env::var(key) else {
        return default;
    };

    raw.trim().parse().unwrap_or_else(|_| {
        tracing::warn!(key, value = %raw, "ignoring unparseable value, using the default");
        default
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(base_url: &str) -> Config {
        Config {
            base_url: base_url.trim_end_matches('/').to_string(),
            max_envelope_bytes: 0,
            rate_limit_per_minute: None,
            scrub_fields: ScrubList::new([]),
        }
    }

    #[test]
    fn dsn_splices_key_after_scheme() {
        assert_eq!(
            config("http://localhost:9000").dsn("abc123", 1),
            "http://abc123@localhost:9000/1"
        );
    }

    #[test]
    fn dsn_preserves_sub_path() {
        assert_eq!(
            config("https://errors.example.com/thermite/").dsn("abc123", 42),
            "https://abc123@errors.example.com/thermite/42"
        );
    }
}
