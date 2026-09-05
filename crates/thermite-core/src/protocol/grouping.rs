//! Deciding which issue an event belongs to.
//!
//! Follows Bugsink's `bugsink-v2` mechanism: an explicit fingerprint wins, otherwise the key is the
//! exception type plus its normalized value. Note that v2 deliberately dropped `transaction` from
//! the key — v1 grouped on `"title ⋄ transaction"`, which split one bug across every route that
//! could trigger it.

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::protocol::event;
use crate::protocol::normalize::normalize_message_for_grouping;

/// Sentry's placeholder for "insert the default grouping key here". Its presence in a fingerprint
/// means the SDK wants to *refine* default grouping rather than replace it.
const DEFAULT_PLACEHOLDER: &str = "{{ default }}";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grouping {
    /// The human-readable key the hash is derived from. Kept for debugging — a fingerprint hash
    /// alone is impossible to reason about.
    pub key: String,
    pub hash: Vec<u8>,
    pub title: String,
    pub culprit: Option<String>,
    pub exception_type: String,
    pub exception_value: String,
}

pub fn group(event: &Value) -> Grouping {
    let (exception_type, exception_value) = event::type_and_value(event);
    let title = event::title(&exception_type, &exception_value);

    let key = match event::fingerprint(event) {
        // An explicit fingerprint is honored verbatim, unless it asks for the default to be mixed
        // in — in which case the default key is substituted for the placeholder. Computed once
        // and reused: it is the same string for every part, and normalizing per part turns a
        // repeated placeholder into a regex scan per repetition.
        Some(parts) => {
            let default = parts
                .iter()
                .any(|part| part.trim() == DEFAULT_PLACEHOLDER)
                .then(|| default_key(&exception_type, &exception_value));

            parts
                .iter()
                .map(
                    |part| match (part.trim() == DEFAULT_PLACEHOLDER, &default) {
                        (true, Some(default)) => default.as_str(),
                        _ => part.as_str(),
                    },
                )
                .collect::<Vec<_>>()
                .join("|")
        }
        None => default_key(&exception_type, &exception_value),
    };

    Grouping {
        hash: Sha256::digest(key.as_bytes()).to_vec(),
        key,
        title,
        culprit: event::culprit(event),
        exception_type,
        exception_value,
    }
}

fn default_key(exception_type: &str, exception_value: &str) -> String {
    let normalized = normalize_message_for_grouping(exception_value);
    event::title(exception_type, &normalized)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn exception(exception_type: &str, value: &str) -> Value {
        json!({ "exception": { "values": [{ "type": exception_type, "value": value }] } })
    }

    #[test]
    fn same_error_groups_together() {
        let a = group(&exception("ValueError", "bad input"));
        let b = group(&exception("ValueError", "bad input"));

        assert_eq!(a.hash, b.hash);
        assert_eq!(a.key, "ValueError: bad input");
    }

    #[test]
    fn different_exception_types_do_not_group() {
        let a = group(&exception("ValueError", "bad input"));
        let b = group(&exception("TypeError", "bad input"));

        assert_ne!(a.hash, b.hash);
    }

    #[test]
    fn values_differing_only_in_variables_group_together() {
        // The whole reason normalization exists.
        let a = group(&exception("IOError", "failed to reach 10.0.0.5 after 30s"));
        let b = group(&exception("IOError", "failed to reach 10.0.0.9 after 45s"));

        assert_eq!(a.hash, b.hash);
        assert_eq!(a.key, "IOError: failed to reach <ip> after <duration>");
        // The title keeps the real values; only the grouping key is normalized.
        assert_eq!(a.title, "IOError: failed to reach 10.0.0.5 after 30s");
        assert_ne!(a.title, b.title);
    }

    #[test]
    fn the_same_error_from_different_transactions_still_groups() {
        // v1 included the transaction in the key, which split one bug per route.
        let mut a = exception("KeyError", "user_id");
        a["transaction"] = json!("GET /users");
        let mut b = exception("KeyError", "user_id");
        b["transaction"] = json!("POST /orders");

        assert_eq!(group(&a).hash, group(&b).hash);
    }

    #[test]
    fn an_explicit_fingerprint_overrides_the_default() {
        let mut event = exception("ValueError", "bad input");
        event["fingerprint"] = json!(["my-custom-group"]);

        let grouping = group(&event);
        assert_eq!(grouping.key, "my-custom-group");
        // The title is still derived from the exception, so the issue stays readable.
        assert_eq!(grouping.title, "ValueError: bad input");
    }

    #[test]
    fn an_explicit_fingerprint_collapses_unrelated_errors() {
        let mut a = exception("ValueError", "one");
        a["fingerprint"] = json!(["shared"]);
        let mut b = exception("TypeError", "two");
        b["fingerprint"] = json!(["shared"]);

        assert_eq!(group(&a).hash, group(&b).hash);
    }

    #[test]
    fn multi_part_fingerprints_are_joined() {
        let mut event = exception("ValueError", "bad input");
        event["fingerprint"] = json!(["service", "checkout"]);

        assert_eq!(group(&event).key, "service|checkout");
    }

    #[test]
    fn the_default_placeholder_is_substituted_not_taken_literally() {
        let mut with_placeholder = exception("ValueError", "bad input");
        with_placeholder["fingerprint"] = json!(["{{ default }}", "tenant-7"]);

        let grouping = group(&with_placeholder);
        assert_eq!(grouping.key, "ValueError: bad input|tenant-7");

        // Still distinct from the plain default, and from another tenant.
        assert_ne!(
            grouping.hash,
            group(&exception("ValueError", "bad input")).hash
        );

        let mut other_tenant = exception("ValueError", "bad input");
        other_tenant["fingerprint"] = json!(["{{ default }}", "tenant-8"]);
        assert_ne!(grouping.hash, group(&other_tenant).hash);
    }

    #[test]
    fn log_messages_group_by_normalized_text() {
        let a = group(&json!({ "message": "cache miss for key abc-123" }));
        let b = group(&json!({ "message": "cache miss for key abc-456" }));

        assert_eq!(a.hash, b.hash);
        // The `-` is consumed as a minus sign by the int pattern, hence `abc<int>` not `abc-<int>`.
        assert_eq!(a.key, "Log Message: cache miss for key abc<int>");
    }

    #[test]
    fn different_log_messages_do_not_group() {
        let a = group(&json!({ "message": "cache miss" }));
        let b = group(&json!({ "message": "cache stampede" }));

        assert_ne!(a.hash, b.hash);
    }

    #[test]
    fn an_exception_with_no_value_groups_on_its_type() {
        let grouping = group(&exception("BrokenPipeError", ""));

        assert_eq!(grouping.key, "BrokenPipeError");
        assert_eq!(grouping.title, "BrokenPipeError");
    }

    #[test]
    fn records_culprit_and_exception_parts_for_display() {
        let event = json!({
            "exception": { "values": [{
                "type": "ValueError",
                "value": "bad input",
                "stacktrace": { "frames": [
                    { "function": "handle", "filename": "app.rs", "in_app": true },
                ]}
            }]}
        });

        let grouping = group(&event);
        assert_eq!(grouping.culprit.as_deref(), Some("app.rs in handle"));
        assert_eq!(grouping.exception_type, "ValueError");
        assert_eq!(grouping.exception_value, "bad input");
    }

    #[test]
    fn hash_is_sha256_of_the_key() {
        let grouping = group(&exception("ValueError", "bad input"));

        assert_eq!(grouping.hash.len(), 32);
        assert_eq!(
            grouping.hash,
            Sha256::digest(grouping.key.as_bytes()).to_vec()
        );
    }

    #[test]
    fn an_empty_event_still_groups() {
        // Never panic on junk; a garbage event is better stored than dropped.
        let grouping = group(&json!({}));
        assert_eq!(grouping.key, "Log Message: <no log message>");
        assert_eq!(grouping.hash.len(), 32);
    }

    /// The HI-8 amplifier, bounded: a payload built to make `{{ default }}` expansion expensive
    /// must cost a fixed amount, not a multiple of what the client sent.
    #[test]
    fn a_flood_of_default_placeholders_is_bounded() {
        let event = json!({
            "exception": { "values": [{
                "type": "ValueError",
                "value": "x".repeat(1024),
            }]},
            "fingerprint": vec!["{{ default }}"; 100_000],
        });

        let started = std::time::Instant::now();
        let grouping = group(&event);
        let elapsed = started.elapsed();

        // 32 parts, each at most the default key: the key cannot exceed a few KiB however many
        // placeholders arrived.
        assert!(
            grouping.key.len() < 64 * 1024,
            "key grew to {} bytes",
            grouping.key.len()
        );
        assert!(
            elapsed < std::time::Duration::from_millis(250),
            "expansion took {elapsed:?}; the normalize scan is running per part again"
        );
    }
}
