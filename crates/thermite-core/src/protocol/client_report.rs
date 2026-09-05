//! Client reports — the events an SDK threw away before they ever reached us.
//!
//! An SDK discards events for its own reasons: its queue overflowed, the network was down, a
//! `before_send` hook returned nothing, a sample rate applied. It reports those counts back as a
//! `client_report` envelope item, which is the only trace such a loss ever leaves.
//!
//! Thermite folds them into the same `ingest_outcomes` rollup its own drops go to, because from
//! the dashboard they answer one question — "am I losing events?" — and an event discarded inside
//! the SDK is exactly as invisible as one dropped here. The reason distinguishes them.
//!
//! Only categories that would have become events here are counted. A discarded transaction is no
//! loss for thermite: it stores no transactions in the first place, so counting them would inflate
//! the drop figures with things that were never wanted.

use serde::Deserialize;

/// The payload of a `client_report` item.
///
/// `discarded_events` is the field every SDK sends. The other two are in the protocol and appear
/// in server-side reports; all three mean the same thing here — it never arrived.
#[derive(Debug, Default, Deserialize)]
pub struct ClientReport {
    #[serde(default)]
    discarded_events: Vec<Discarded>,
    #[serde(default)]
    rate_limited_events: Vec<Discarded>,
    #[serde(default)]
    filtered_events: Vec<Discarded>,
}

#[derive(Debug, Deserialize)]
struct Discarded {
    /// Why the SDK dropped it: `queue_overflow`, `network_error`, `before_send`, …
    #[serde(default)]
    reason: String,
    /// The data category dropped: `error`, `transaction`, `session`, …
    #[serde(default)]
    category: String,
    /// How many. A report summarizes a period, so this is routinely more than one.
    #[serde(default)]
    quantity: i64,
}

impl ClientReport {
    /// `(reason, quantity)` for the discards thermite would otherwise have stored.
    pub fn event_discards(&self) -> impl Iterator<Item = (&str, i64)> {
        self.discarded_events
            .iter()
            .chain(&self.rate_limited_events)
            .chain(&self.filtered_events)
            .filter(|d| d.quantity > 0 && is_event_category(&d.category))
            .map(|d| (d.reason.as_str(), d.quantity))
    }
}

/// Data categories that would have arrived here as events. `error` is the explicit form,
/// `default` what an SDK sends when it does not distinguish, `security` a CSP report. Everything
/// else — transactions, sessions, spans, attachments — thermite does not store.
fn is_event_category(category: &str) -> bool {
    matches!(category, "error" | "default" | "security")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: serde_json::Value) -> ClientReport {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn counts_error_discards_across_all_three_lists() {
        let report = parse(serde_json::json!({
            "timestamp": "2026-08-27T10:00:00Z",
            "discarded_events": [
                { "reason": "queue_overflow", "category": "error", "quantity": 23 },
                { "reason": "sample_rate", "category": "default", "quantity": 2 },
            ],
            "rate_limited_events": [
                { "reason": "ratelimit_backoff", "category": "error", "quantity": 5 },
            ],
        }));

        assert_eq!(
            report.event_discards().collect::<Vec<_>>(),
            vec![
                ("queue_overflow", 23),
                ("sample_rate", 2),
                ("ratelimit_backoff", 5),
            ]
        );
    }

    /// Categories thermite does not store are not losses, and a zero quantity is not news.
    #[test]
    fn skips_other_categories_and_empty_counts() {
        let report = parse(serde_json::json!({
            "discarded_events": [
                { "reason": "sample_rate", "category": "transaction", "quantity": 100 },
                { "reason": "queue_overflow", "category": "session", "quantity": 40 },
                { "reason": "network_error", "category": "error", "quantity": 0 },
            ]
        }));

        assert_eq!(report.event_discards().count(), 0);
    }

    /// A report with none of the three lists parses to nothing rather than failing the item.
    #[test]
    fn tolerates_a_report_with_no_discards() {
        let report = parse(serde_json::json!({ "timestamp": "2026-08-27T10:00:00Z" }));
        assert_eq!(report.event_discards().count(), 0);
    }
}
