//! Session payloads — the release-health half of the Sentry protocol.
//!
//! Two item types carry the same information at different resolutions:
//!
//! - `session` — one session, sent repeatedly as it progresses. The *same* `sid` arrives on `init`,
//!   possibly again while running, and once more when it ends.
//! - `sessions` — a pre-aggregated batch of counters, sent by SDKs in request mode where one
//!   process handles far too many sessions to report individually.
//!
//! The counting rule for the first form is the load-bearing part of this module. A session is
//! reported several times, so counting every update would multiply the totals; keeping a row per
//! `sid` to deduplicate would be unbounded storage for a number that is only ever read as a rate.
//! Instead we count only the updates that are sent **exactly once per session**:
//!
//! | Signal                         | Counts as |
//! |--------------------------------|-----------|
//! | `init: true`                   | one session started |
//! | `status: "crashed"`            | one crash |
//! | `status: "abnormal"`           | one abnormal end |
//! | `status: "exited"`, `errors>0` | one errored session |
//!
//! `status: "ok"` updates contribute nothing — they say the session is still running, which the
//! `init` that opened it already told us.
//!
//! The known imprecision: an SDK that retries an envelope counts its sessions twice. Sentry has
//! the same property, and a dedup table for a rate metric costs more than the error it removes.

use serde::Deserialize;
use serde_json::Value;

/// What one session item contributes to a release's counters.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Delta {
    pub sessions: i64,
    pub errored: i64,
    pub crashed: i64,
    pub abnormal: i64,
}

impl Delta {
    /// True when this item changes nothing — an `ok` heartbeat, or an empty aggregate.
    pub fn is_empty(&self) -> bool {
        *self == Delta::default()
    }

    /// Folds another item's contribution in, for a batch that repeats a bucket.
    pub fn merge(&mut self, other: Delta) {
        self.sessions += other.sessions;
        self.errored += other.errored;
        self.crashed += other.crashed;
        self.abnormal += other.abnormal;
    }
}

/// Attributes shared by both item types. The release is the only one we key on.
#[derive(Debug, Default, Deserialize)]
pub struct Attrs {
    #[serde(default)]
    pub release: Option<String>,
    #[serde(default)]
    pub environment: Option<String>,
}

/// One session update (`type: session`).
///
/// `errors` and the aggregate counters are unsigned on purpose: a negative count would *subtract*
/// from the rollup, and serde rejecting the item outright is the right answer for a payload that
/// could only be hostile.
#[derive(Debug, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub init: bool,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub errors: u32,
    /// When the session began. Bucketing keys on this, never on `timestamp`.
    #[serde(default)]
    pub started: Option<Value>,
    /// When this particular update was produced.
    #[serde(default)]
    pub timestamp: Option<Value>,
    #[serde(default)]
    pub attrs: Attrs,
}

impl Session {
    pub fn delta(&self) -> Delta {
        let status = self.status.as_deref().unwrap_or("ok");

        Delta {
            sessions: i64::from(self.init),
            errored: i64::from(status == "exited" && self.errors > 0),
            crashed: i64::from(status == "crashed"),
            abnormal: i64::from(status == "abnormal"),
        }
    }

    /// The timestamp this session's counters belong to: its start, falling back to the update's
    /// own time when the SDK omits one.
    pub fn started_at(&self) -> Option<Value> {
        self.started.clone().or_else(|| self.timestamp.clone())
    }
}

/// A pre-aggregated batch (`type: sessions`).
#[derive(Debug, Deserialize)]
pub struct Aggregates {
    #[serde(default)]
    pub attrs: Attrs,
    #[serde(default)]
    pub aggregates: Vec<Aggregate>,
}

/// One bucket of a batch. The four counters are disjoint, so their sum is the session total —
/// there is no separate total field to read.
#[derive(Debug, Deserialize)]
pub struct Aggregate {
    #[serde(default)]
    pub started: Option<Value>,
    #[serde(default)]
    pub exited: u32,
    #[serde(default)]
    pub errored: u32,
    #[serde(default)]
    pub crashed: u32,
    #[serde(default)]
    pub abnormal: u32,
}

impl Aggregate {
    pub fn delta(&self) -> Delta {
        Delta {
            sessions: i64::from(self.exited)
                + i64::from(self.errored)
                + i64::from(self.crashed)
                + i64::from(self.abnormal),
            errored: i64::from(self.errored),
            crashed: i64::from(self.crashed),
            abnormal: i64::from(self.abnormal),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn session(value: serde_json::Value) -> Session {
        serde_json::from_value(value).expect("valid session")
    }

    #[test]
    fn an_init_opens_exactly_one_session() {
        let delta = session(json!({ "init": true, "status": "ok" })).delta();

        assert_eq!(
            delta,
            Delta {
                sessions: 1,
                ..Delta::default()
            }
        );
    }

    #[test]
    fn a_running_session_contributes_nothing() {
        // The update that says "still going" must not count a second session, or a long-lived
        // process reports as many sessions as it sends heartbeats.
        assert!(session(json!({ "status": "ok" })).delta().is_empty());
    }

    #[test]
    fn a_session_that_starts_and_crashes_in_one_update_counts_both() {
        let delta = session(json!({ "init": true, "status": "crashed" })).delta();

        assert_eq!(
            delta,
            Delta {
                sessions: 1,
                crashed: 1,
                ..Delta::default()
            }
        );
    }

    #[test]
    fn a_crash_reported_after_the_init_counts_only_the_crash() {
        // init and the terminal update are separate items; counting the session twice would put
        // the crash-free rate above 100% for a release where everything crashed.
        let opened = session(json!({ "init": true, "status": "ok" })).delta();
        let crashed = session(json!({ "status": "crashed" })).delta();

        assert_eq!(opened.sessions + crashed.sessions, 1);
        assert_eq!(opened.crashed + crashed.crashed, 1);
    }

    #[test]
    fn a_clean_exit_is_neither_errored_nor_crashed() {
        assert!(
            session(json!({ "status": "exited", "errors": 0 }))
                .delta()
                .is_empty()
        );
    }

    #[test]
    fn an_exit_with_errors_is_errored_but_not_crashed() {
        let delta = session(json!({ "status": "exited", "errors": 3 })).delta();

        assert_eq!(
            delta,
            Delta {
                errored: 1,
                ..Delta::default()
            }
        );
    }

    #[test]
    fn an_abnormal_end_is_counted_separately_from_a_crash() {
        let delta = session(json!({ "status": "abnormal" })).delta();

        assert_eq!(
            delta,
            Delta {
                abnormal: 1,
                ..Delta::default()
            }
        );
    }

    #[test]
    fn a_missing_status_is_treated_as_still_running() {
        assert!(session(json!({})).delta().is_empty());
    }

    #[test]
    fn aggregate_totals_are_the_sum_of_the_four_disjoint_counters() {
        let aggregate: Aggregate = serde_json::from_value(
            json!({ "exited": 5, "errored": 2, "crashed": 1, "abnormal": 1 }),
        )
        .expect("valid aggregate");

        assert_eq!(
            aggregate.delta(),
            Delta {
                sessions: 9,
                errored: 2,
                crashed: 1,
                abnormal: 1
            }
        );
    }

    #[test]
    fn a_negative_counter_is_rejected_rather_than_subtracted() {
        let parsed: Result<Aggregate, _> = serde_json::from_value(json!({ "crashed": -100 }));

        assert!(
            parsed.is_err(),
            "a negative count must not decrement the rollup"
        );
    }

    #[test]
    fn bucketing_keys_on_the_start_not_the_update() {
        let session = session(
            json!({ "started": "2026-01-01T10:59:00Z", "timestamp": "2026-01-01T11:01:00Z" }),
        );

        assert_eq!(session.started_at(), Some(json!("2026-01-01T10:59:00Z")));
    }

    #[test]
    fn a_session_without_a_start_falls_back_to_the_update_time() {
        let session = session(json!({ "timestamp": "2026-01-01T11:01:00Z" }));

        assert_eq!(session.started_at(), Some(json!("2026-01-01T11:01:00Z")));
    }
}
