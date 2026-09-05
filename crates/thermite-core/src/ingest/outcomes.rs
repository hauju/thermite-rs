//! Ingest outcome accounting: what happened to everything a client sent.
//!
//! Ingest drops things by design — over-quota events, unsupported item types, unparseable
//! payloads — and a drop that leaves no trace is indistinguishable from an SDK that never sent
//! anything. Every ingest request therefore records its outcomes into the `ingest_outcomes`
//! rollup, which the stats API surfaces as the "dropped" figures.
//!
//! Counts are accumulated per request and flushed in one batch (at most one upsert per outcome
//! kind), not written per item: an envelope can carry hundreds of events, and per-item upserts
//! would double the statement count of the whole endpoint.
//!
//! Two of the kinds carry a detail after a colon — `unsupported:transaction`,
//! `client_discarded:queue_overflow` — because the bare count cannot tell you whether to care:
//! an SDK flushing transactions thermite does not store is nothing to act on, an SDK whose queue
//! is overflowing is. Both details come off the wire, so both are mapped through a fixed set with
//! an `other` catch-all: the outcome is part of this rollup's primary key, and an unbounded key
//! is the cardinality problem `issue_tags` already had to solve.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::error::AppResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The event item was digested (a duplicate delivered again still counts — the SDK's send
    /// succeeded).
    Accepted,
    /// Rejected by the per-project quota. Counted once per rejected *request*: a mid-envelope
    /// rejection abandons the remaining items unparsed, so their number is unknown. The signal
    /// is "this is non-zero", not an exact event count.
    OverQuota,
    /// A payload that failed to parse.
    Invalid,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Accepted => "accepted",
            Outcome::OverQuota => "over_quota",
            Outcome::Invalid => "invalid",
        }
    }
}

/// Per-request accumulator. Bump during processing, flush once at the end — including on the
/// early-return error paths, or the rejection the flush exists to count would go unrecorded.
#[derive(Debug, Default)]
pub struct Counts(BTreeMap<&'static str, i64>);

impl Counts {
    pub fn bump(&mut self, outcome: Outcome) {
        self.add(outcome.as_str(), 1);
    }

    /// An item type thermite does not store (transactions, logs, attachments, …), accepted with
    /// a `200` and dropped. The type is recorded alongside the outcome, since "2 unsupported"
    /// on its own says nothing about whether anything was lost.
    pub fn bump_unsupported(&mut self, item_type: &str) {
        self.add(unsupported_key(item_type), 1);
    }

    /// Events the SDK itself discarded before sending, reported back in a client report. Adds
    /// the SDK's own count, not one: a report summarizes a period.
    pub fn add_client_discarded(&mut self, reason: &str, quantity: i64) {
        self.add(client_discarded_key(reason), quantity);
    }

    fn add(&mut self, outcome: &'static str, count: i64) {
        *self.0.entry(outcome).or_default() += count;
    }

    /// Writes the accumulated counters into the rollup, bucketed on `received_at`'s hour.
    ///
    /// Not transactional with the digests it counts: a request that dies mid-envelope on an
    /// infrastructure error loses its counters, which is acceptable for a chart and not worth a
    /// transaction spanning every item in the envelope.
    pub async fn flush(
        self,
        db: &PgPool,
        project_id: i64,
        received_at: DateTime<Utc>,
    ) -> AppResult<()> {
        for (outcome, count) in self.0 {
            record_key(db, project_id, outcome, count, received_at).await?;
        }
        Ok(())
    }
}

/// Item types we knowingly drop, spelled out so the rollup can say which one. Anything else
/// collapses to `unsupported:other` — the item type is client-controlled.
fn unsupported_key(item_type: &str) -> &'static str {
    match item_type {
        "transaction" => "unsupported:transaction",
        "span" => "unsupported:span",
        "log" | "otel_log" => "unsupported:log",
        "attachment" => "unsupported:attachment",
        "profile" | "profile_chunk" => "unsupported:profile",
        "replay_event" | "replay_recording" => "unsupported:replay",
        "user_report" | "feedback" => "unsupported:feedback",
        "statsd" | "metric_buckets" => "unsupported:metrics",
        _ => "unsupported:other",
    }
}

/// The SDK-side discard reasons defined by the protocol. Anything else collapses to
/// `client_discarded:other` — the reason is client-controlled.
fn client_discarded_key(reason: &str) -> &'static str {
    match reason {
        "queue_overflow" => "client_discarded:queue_overflow",
        "cache_overflow" => "client_discarded:cache_overflow",
        "buffer_overflow" => "client_discarded:buffer_overflow",
        "backpressure" => "client_discarded:backpressure",
        "ratelimit_backoff" => "client_discarded:ratelimit_backoff",
        "network_error" => "client_discarded:network_error",
        "send_error" => "client_discarded:send_error",
        "sample_rate" => "client_discarded:sample_rate",
        "before_send" => "client_discarded:before_send",
        "event_processor" => "client_discarded:event_processor",
        "internal_sdk_error" => "client_discarded:internal_sdk_error",
        _ => "client_discarded:other",
    }
}

/// One direct upsert, for paths where no accumulator is in flight (the pre-body quota check).
pub async fn record(
    db: &PgPool,
    project_id: i64,
    outcome: Outcome,
    count: i64,
    received_at: DateTime<Utc>,
) -> AppResult<()> {
    record_key(db, project_id, outcome.as_str(), count, received_at).await
}

async fn record_key(
    db: &PgPool,
    project_id: i64,
    outcome: &str,
    count: i64,
    received_at: DateTime<Utc>,
) -> AppResult<()> {
    sqlx::query(
        "insert into ingest_outcomes (project_id, bucket, outcome, count)
         values ($1, date_trunc('hour', $2::timestamptz), $3, $4)
         on conflict (project_id, bucket, outcome)
         do update set count = ingest_outcomes.count + excluded.count",
    )
    .bind(project_id)
    .bind(received_at)
    .bind(outcome)
    .bind(count)
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_accumulate_per_outcome() {
        let mut counts = Counts::default();
        counts.bump(Outcome::Accepted);
        counts.bump(Outcome::Accepted);
        counts.bump_unsupported("transaction");
        counts.add_client_discarded("queue_overflow", 23);
        counts.add_client_discarded("queue_overflow", 2);

        assert_eq!(counts.0["accepted"], 2);
        assert_eq!(counts.0["unsupported:transaction"], 1);
        assert_eq!(counts.0["client_discarded:queue_overflow"], 25);
        assert!(!counts.0.contains_key("over_quota"));
    }

    /// Both details come off the wire, so an unrecognised one must not mint a new rollup key.
    #[test]
    fn unknown_details_collapse_to_other() {
        let mut counts = Counts::default();
        counts.bump_unsupported("whatever_sentry_ships_next");
        counts.add_client_discarded("nonsense", 1);

        assert_eq!(counts.0["unsupported:other"], 1);
        assert_eq!(counts.0["client_discarded:other"], 1);
    }
}
