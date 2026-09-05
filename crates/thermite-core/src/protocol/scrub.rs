//! Server-side scrubbing of credentials and secrets from event payloads.
//!
//! Sentry SDKs routinely attach request headers, cookies, form bodies and `extra` context —
//! upstream Sentry filters those server-side by default, precisely because SDKs do send live
//! session cookies and bearer tokens inside crash reports. A receiver that stores them verbatim
//! re-serves working credentials to every dashboard reader and, through MCP, into LLM context.
//!
//! Scrubbing therefore happens at ingest, before the payload is bound in `digest()`, so a
//! sensitive value is never written at all. There is no way to scrub retroactively — by then the
//! value has been persisted and possibly served.

use serde_json::Value;

/// What a scrubbed value is replaced with — the marker upstream Sentry uses.
pub const FILTERED: &str = "[Filtered]";

/// Keys whose values are always scrubbed, wherever they appear in the payload.
///
/// Matching is case-insensitive, ignores `-`/`_`, and fires on substrings, so one `api_key`
/// entry catches `X-Api-Key` and `apikey`, `cookie` catches `Set-Cookie` and `cookies`, and
/// `token` catches `access_token`. Extended (never replaced) via `THERMITE_SCRUB_FIELDS`.
const DEFAULT_DENYLIST: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "authorization",
    "cookie",
    "session",
    "credentials",
    "private_key",
];

/// The denylist pre-normalized for matching, built once at startup.
#[derive(Debug, Clone)]
pub struct ScrubList(Vec<String>);

impl ScrubList {
    /// The default denylist plus `extra` entries (from `THERMITE_SCRUB_FIELDS`). Entries that
    /// normalize to nothing are dropped — an empty pattern would match every key.
    pub fn new<'a>(extra: impl IntoIterator<Item = &'a str>) -> Self {
        Self(
            DEFAULT_DENYLIST
                .iter()
                .copied()
                .chain(extra)
                .map(normalize)
                .filter(|entry| !entry.is_empty())
                .collect(),
        )
    }

    fn matches(&self, key: &str) -> bool {
        let key = normalize(key);
        self.0.iter().any(|entry| key.contains(entry))
    }
}

fn normalize(key: &str) -> String {
    key.to_ascii_lowercase().replace(['-', '_', ' '], "")
}

/// Replaces the value of every object entry whose key matches the denylist with `"[Filtered]"`,
/// recursively. A matching key's value is replaced wholesale — an entire `cookies` object
/// disappears rather than being walked, because every cookie is a session credential.
///
/// Two-element arrays whose first element is a matching string are treated as key-value pairs:
/// the Sentry protocol allows `request.headers` and `request.cookies` as `[["Name", "value"]]`
/// pair lists as well as objects.
pub fn scrub(payload: &mut Value, list: &ScrubList) {
    match payload {
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                if list.matches(key) {
                    *value = Value::String(FILTERED.into());
                } else {
                    scrub(value, list);
                }
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                if let Value::Array(pair) = item
                    && let [Value::String(key), value] = &mut pair[..]
                    && list.matches(key)
                {
                    *value = Value::String(FILTERED.into());
                    continue;
                }
                scrub(item, list);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scrubbed(mut payload: Value) -> Value {
        scrub(&mut payload, &ScrubList::new([]));
        payload
    }

    #[test]
    fn filters_denylisted_keys() {
        let payload = scrubbed(json!({"password": "hunter2", "message": "boom"}));
        assert_eq!(payload["password"], FILTERED);
        assert_eq!(payload["message"], "boom");
    }

    #[test]
    fn matching_ignores_case_and_separators() {
        let payload = scrubbed(json!({
            "X-Api-Key": "k",
            "Set-Cookie": "s=1",
            "AUTHORIZATION": "Bearer x"
        }));
        assert_eq!(payload["X-Api-Key"], FILTERED);
        assert_eq!(payload["Set-Cookie"], FILTERED);
        assert_eq!(payload["AUTHORIZATION"], FILTERED);
    }

    #[test]
    fn matching_fires_on_substrings() {
        let payload = scrubbed(json!({
            "access_token": "t",
            "user_password": "p",
            "client_secret": "c"
        }));
        assert_eq!(payload["access_token"], FILTERED);
        assert_eq!(payload["user_password"], FILTERED);
        assert_eq!(payload["client_secret"], FILTERED);
    }

    #[test]
    fn walks_nested_objects() {
        let payload = scrubbed(json!({
            "request": {
                "headers": {"Authorization": "Bearer x", "Accept": "application/json"},
                "url": "https://example.com"
            }
        }));
        assert_eq!(payload["request"]["headers"]["Authorization"], FILTERED);
        assert_eq!(payload["request"]["headers"]["Accept"], "application/json");
        assert_eq!(payload["request"]["url"], "https://example.com");
    }

    #[test]
    fn matching_key_replaces_whole_value() {
        // `cookies` matches the `cookie` entry, so the entire object goes — every cookie is a
        // session credential, walking inside it would keep the names and lose nothing worth
        // keeping.
        let payload = scrubbed(json!({
            "request": {"cookies": {"session": "abc", "theme": "dark"}}
        }));
        assert_eq!(payload["request"]["cookies"], FILTERED);
    }

    #[test]
    fn filters_header_pair_lists() {
        let payload = scrubbed(json!({
            "request": {"headers": [["Authorization", "Bearer x"], ["Accept", "json"]]}
        }));
        assert_eq!(payload["request"]["headers"][0][1], FILTERED);
        assert_eq!(payload["request"]["headers"][1][1], "json");
    }

    #[test]
    fn keeps_grouping_fields_untouched() {
        let payload = scrubbed(json!({
            "exception": {"values": [{"type": "ValueError", "value": "boom"}]},
            "fingerprint": ["{{ default }}"],
            "logentry": {"message": "it broke"}
        }));
        assert_eq!(payload["exception"]["values"][0]["value"], "boom");
        assert_eq!(payload["fingerprint"][0], "{{ default }}");
        assert_eq!(payload["logentry"]["message"], "it broke");
    }

    #[test]
    fn extra_entries_extend_the_denylist() {
        let list = ScrubList::new(["ssn"]);
        let mut payload = json!({"ssn": "123-45-6789", "password": "p"});
        scrub(&mut payload, &list);
        assert_eq!(payload["ssn"], FILTERED);
        assert_eq!(payload["password"], FILTERED);
    }

    #[test]
    fn empty_extra_entries_are_dropped() {
        // A stray comma in THERMITE_SCRUB_FIELDS must not become a match-everything pattern.
        let list = ScrubList::new(["", "  "]);
        let mut payload = json!({"message": "fine"});
        scrub(&mut payload, &list);
        assert_eq!(payload["message"], "fine");
    }
}
