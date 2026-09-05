//! Dropping old events so a self-hosted instance does not grow until the disk fills.
//!
//! Two rules, whichever bites first:
//!
//! - **Age** — nothing older than `max_age_days`. Predictable ("we keep 90 days") but on its own it
//!   does not bound disk: a traffic spike fills it inside the window.
//! - **Per-project quota** — at most `max_events_per_project`, oldest evicted. This is the rule that
//!   actually bounds disk, which is why age alone is not enough.
//!
//! # What survives
//!
//! Deleting events does **not** erase history *within the retention window*. The issue row (with
//! `times_seen`, `first_seen`, `last_seen`, `users_affected`) and any agent analyses outlive
//! everything — "this bug happened 40,000 times over three months" stays true forever. The
//! rollups (`event_counts`, `issue_tags`, `session_counts`, `ingest_outcomes`) outlive the *events* they summarize, which is why they
//! are maintained during ingest rather than computed from `events` on read — but their rows are
//! themselves swept once older than the age policy. They grow with data cardinality (one row per
//! distinct tag value, per hourly bucket), not with time, so "rollups are forever" would mean
//! retention never actually bounds disk; and the tag rollup carries user identifiers, which
//! `THERMITE_RETENTION_DAYS` must genuinely erase for the setting to mean what it says.
//!
//! An issue whose events have all been evicted simply has no latest event; the API returns
//! `latest_event: null` rather than failing.
//!
//! # Two things done deliberately
//!
//! Deletes are **batched**. A single `delete` over millions of rows holds locks and bloats the table
//! long enough to stall ingest, which is the one thing retention must not do.
//!
//! Age is keyed on `received_at`, never the event's own `timestamp`. `timestamp` is client-supplied:
//! an SDK sending 1970 would have its events evicted on arrival, and one sending 2099 would keep
//! them forever. `received_at` is when we took on the storage cost and cannot be forged.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::error::AppResult;

/// How much history to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Drop events received longer ago than this. `None` disables the age rule.
    pub max_age_days: Option<i64>,
    /// Keep at most this many events per project. `None` disables the quota rule.
    pub max_events_per_project: Option<i64>,
    /// Rows per delete statement.
    pub batch: i64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            max_age_days: Some(90),
            max_events_per_project: Some(100_000),
            batch: 5_000,
        }
    }
}

impl Policy {
    pub fn from_env() -> Self {
        let default = Self::default();

        // 0 means "no limit" for both rules, so a deployment can opt out of either without
        // needing a separate on/off variable.
        let age = env_i64("THERMITE_RETENTION_DAYS", 90);
        let quota = env_i64("THERMITE_MAX_EVENTS_PER_PROJECT", 100_000);

        Self {
            max_age_days: (age > 0).then_some(age),
            max_events_per_project: (quota > 0).then_some(quota),
            batch: env_i64("THERMITE_RETENTION_BATCH", default.batch).clamp(100, 50_000),
        }
    }

    fn disabled(&self) -> bool {
        self.max_age_days.is_none() && self.max_events_per_project.is_none()
    }
}

fn env_i64(key: &str, default: i64) -> i64 {
    let Ok(raw) = std::env::var(key) else {
        return default;
    };

    raw.trim().parse().unwrap_or_else(|_| {
        tracing::warn!(key, value = %raw, "ignoring unparseable value, using the default");
        default
    })
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Swept {
    pub events_by_age: u64,
    pub events_by_quota: u64,
    pub notifications: u64,
    pub tag_values: u64,
    pub count_buckets: u64,
    pub session_buckets: u64,
    pub outcome_buckets: u64,
}

impl Swept {
    pub fn total(&self) -> u64 {
        self.events_by_age
            + self.events_by_quota
            + self.notifications
            + self.tag_values
            + self.count_buckets
            + self.session_buckets
            + self.outcome_buckets
    }
}

/// Runs one full pass.
pub async fn sweep(db: &PgPool, policy: &Policy) -> AppResult<Swept> {
    let mut swept = Swept::default();

    if policy.disabled() {
        return Ok(swept);
    }

    // Per project rather than one global statement: both rules are per-project, and the
    // (project_id, received_at desc) index only helps when the project is pinned.
    let projects: Vec<(i64,)> = sqlx::query_as("select id from projects order by id")
        .fetch_all(db)
        .await?;

    for (project_id,) in projects {
        if let Some(days) = policy.max_age_days {
            let cutoff = Utc::now() - chrono::Duration::days(days);
            swept.events_by_age += delete_older_than(db, project_id, cutoff, policy.batch).await?;
        }

        if let Some(keep) = policy.max_events_per_project {
            swept.events_by_quota += enforce_quota(db, project_id, keep, policy.batch).await?;
        }
    }

    // The rollups age out too (see the module docs) — globally rather than per project, since
    // the age cutoff is the same for everyone.
    if let Some(days) = policy.max_age_days {
        let cutoff = Utc::now() - chrono::Duration::days(days);
        swept.tag_values += sweep_tag_values(db, cutoff, policy.batch).await?;
        swept.count_buckets += sweep_count_buckets(db, cutoff, policy.batch).await?;
        swept.session_buckets += sweep_session_buckets(db, cutoff, policy.batch).await?;
        swept.outcome_buckets += sweep_outcome_buckets(db, cutoff, policy.batch).await?;
    }

    swept.notifications += sweep_notifications(db, policy.batch).await?;

    Ok(swept)
}

/// Drops tag values not seen since the cutoff. This is also the erasure path for the user
/// identifiers the rollup carries; `issues.users_affected` keeps the historical count.
async fn sweep_tag_values(db: &PgPool, cutoff: DateTime<Utc>, batch: i64) -> AppResult<u64> {
    let mut deleted = 0;

    loop {
        let affected = sqlx::query(
            "delete from issue_tags
              where (issue_id, key, value) in (
                    select issue_id, key, value from issue_tags
                     where last_seen < $1
                     limit $2
              )",
        )
        .bind(cutoff)
        .bind(batch)
        .execute(db)
        .await?
        .rows_affected();

        deleted += affected;

        if affected < batch as u64 {
            return Ok(deleted);
        }
    }
}

/// Drops hourly count buckets older than the cutoff. Charts never look back further than the
/// retention window, and the permanent totals live on the issue row.
async fn sweep_count_buckets(db: &PgPool, cutoff: DateTime<Utc>, batch: i64) -> AppResult<u64> {
    let mut deleted = 0;

    loop {
        let affected = sqlx::query(
            "delete from event_counts
              where (issue_id, bucket, level) in (
                    select issue_id, bucket, level from event_counts
                     where bucket < $1
                     limit $2
              )",
        )
        .bind(cutoff)
        .bind(batch)
        .execute(db)
        .await?
        .rows_affected();

        deleted += affected;

        if affected < batch as u64 {
            return Ok(deleted);
        }
    }
}

/// Drops hourly session buckets older than the cutoff.
///
/// Purely a disk bound, not an erasure path: the rollup counts sessions and carries nothing about
/// who ran them. That is also why crash-free *users* is not built on it.
async fn sweep_session_buckets(db: &PgPool, cutoff: DateTime<Utc>, batch: i64) -> AppResult<u64> {
    let mut deleted = 0;

    loop {
        let affected = sqlx::query(
            "delete from session_counts
              where (release_id, bucket) in (
                    select release_id, bucket from session_counts
                     where bucket < $1
                     limit $2
              )",
        )
        .bind(cutoff)
        .bind(batch)
        .execute(db)
        .await?
        .rows_affected();

        deleted += affected;

        if affected < batch as u64 {
            return Ok(deleted);
        }
    }
}

/// Drops hourly ingest-outcome buckets older than the cutoff. A disk bound like the session
/// sweep: the rollup counts requests and carries nothing about their contents.
async fn sweep_outcome_buckets(db: &PgPool, cutoff: DateTime<Utc>, batch: i64) -> AppResult<u64> {
    let mut deleted = 0;

    loop {
        let affected = sqlx::query(
            "delete from ingest_outcomes
              where (project_id, bucket, outcome) in (
                    select project_id, bucket, outcome from ingest_outcomes
                     where bucket < $1
                     limit $2
              )",
        )
        .bind(cutoff)
        .bind(batch)
        .execute(db)
        .await?
        .rows_affected();

        deleted += affected;

        if affected < batch as u64 {
            return Ok(deleted);
        }
    }
}

/// Deletes one project's events received before `cutoff`.
async fn delete_older_than(
    db: &PgPool,
    project_id: i64,
    cutoff: DateTime<Utc>,
    batch: i64,
) -> AppResult<u64> {
    let mut deleted = 0;

    loop {
        let affected = sqlx::query(
            "delete from events
              where id in (
                    select id from events
                     where project_id = $1 and received_at < $2
                     order by received_at, id
                     limit $3
              )",
        )
        .bind(project_id)
        .bind(cutoff)
        .bind(batch)
        .execute(db)
        .await?
        .rows_affected();

        deleted += affected;

        // A short batch means the tail is gone.
        if affected < batch as u64 {
            return Ok(deleted);
        }
    }
}

/// Trims one project down to its newest `keep` events.
async fn enforce_quota(db: &PgPool, project_id: i64, keep: i64, batch: i64) -> AppResult<u64> {
    // The row sitting exactly at the quota boundary. Everything ordered below it goes. Finding the
    // boundary once and deleting relative to it beats a growing OFFSET on every batch, and the
    // boundary cannot move while we only delete below it.
    let boundary: Option<(DateTime<Utc>, i64)> = sqlx::query_as(
        "select received_at, id from events
          where project_id = $1
          order by received_at desc, id desc
          offset $2 limit 1",
    )
    .bind(project_id)
    .bind(keep)
    .fetch_optional(db)
    .await?;

    let Some((at, id)) = boundary else {
        // Fewer events than the quota; nothing to do.
        return Ok(0);
    };

    let mut deleted = 0;

    loop {
        let affected = sqlx::query(
            "delete from events
              where id in (
                    select id from events
                     where project_id = $1 and (received_at, id) <= ($2, $3)
                     order by received_at, id
                     limit $4
              )",
        )
        .bind(project_id)
        .bind(at)
        .bind(id)
        .bind(batch)
        .execute(db)
        .await?
        .rows_affected();

        deleted += affected;

        if affected < batch as u64 {
            return Ok(deleted);
        }
    }
}

/// Drops acked triage notifications once they are old enough to be uninteresting.
///
/// Unacked rows are never touched, however old: an unacked notification is work nobody has done, and
/// silently discarding it would lose the only record that an issue was never looked at.
async fn sweep_notifications(db: &PgPool, batch: i64) -> AppResult<u64> {
    let mut deleted = 0;

    loop {
        let affected = sqlx::query(
            "delete from notifications
              where id in (
                    select id from notifications
                     where acked_at is not null and acked_at < now() - interval '30 days'
                     order by id
                     limit $1
              )",
        )
        .bind(batch)
        .execute(db)
        .await?
        .rows_affected();

        deleted += affected;

        if affected < batch as u64 {
            return Ok(deleted);
        }
    }
}

/// Sweeps on an interval, forever.
///
/// Failures are logged and retried on the next tick rather than propagated: retention falling behind
/// is a capacity problem, not a reason to take the process down.
pub fn spawn(db: PgPool, policy: Policy, every: Duration) {
    if policy.disabled() {
        tracing::info!("retention disabled; events are kept indefinitely");
        return;
    }

    tracing::info!(
        max_age_days = ?policy.max_age_days,
        max_events_per_project = ?policy.max_events_per_project,
        "retention enabled"
    );

    tokio::spawn(async move {
        loop {
            match sweep(&db, &policy).await {
                Ok(swept) if swept.total() > 0 => tracing::info!(
                    events_by_age = swept.events_by_age,
                    events_by_quota = swept.events_by_quota,
                    notifications = swept.notifications,
                    tag_values = swept.tag_values,
                    count_buckets = swept.count_buckets,
                    "retention sweep complete"
                ),
                Ok(_) => tracing::debug!("retention sweep: nothing to drop"),
                Err(error) => tracing::error!(%error, "retention sweep failed"),
            }

            tokio::time::sleep(every).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_means_no_limit_rather_than_keep_nothing() {
        // The alternative reading — "keep 0 days" — would delete everything on the first sweep,
        // which is the worst possible interpretation of an ambiguous config value.
        unsafe {
            std::env::set_var("THERMITE_RETENTION_DAYS", "0");
            std::env::set_var("THERMITE_MAX_EVENTS_PER_PROJECT", "0");
        }

        let policy = Policy::from_env();
        assert_eq!(policy.max_age_days, None);
        assert_eq!(policy.max_events_per_project, None);
        assert!(policy.disabled());

        unsafe {
            std::env::remove_var("THERMITE_RETENTION_DAYS");
            std::env::remove_var("THERMITE_MAX_EVENTS_PER_PROJECT");
        }
    }

    #[test]
    fn defaults_keep_a_bounded_amount_of_history() {
        let policy = Policy::default();
        assert_eq!(policy.max_age_days, Some(90));
        assert_eq!(policy.max_events_per_project, Some(100_000));
        assert!(!policy.disabled());
    }

    #[test]
    fn batch_size_is_clamped_to_something_survivable() {
        unsafe { std::env::set_var("THERMITE_RETENTION_BATCH", "1") }
        assert_eq!(Policy::from_env().batch, 100);

        unsafe { std::env::set_var("THERMITE_RETENTION_BATCH", "10000000") }
        assert_eq!(Policy::from_env().batch, 50_000);

        unsafe { std::env::remove_var("THERMITE_RETENTION_BATCH") }
    }
}
