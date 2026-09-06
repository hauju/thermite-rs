//! Turning an accepted event payload into an issue and an event row.
//!
//! Runs synchronously inside the request, before the SDK is acknowledged. Nothing is buffered
//! outside Postgres, so a crash or restart cannot lose an event we have already accepted, and a
//! slow database applies backpressure to the SDK (which buffers and retries on its own).

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppResult;
use crate::ingest::releases;
use crate::protocol::{event, grouping};

#[derive(Debug, PartialEq, Eq)]
pub struct Digested {
    pub event_id: Uuid,
    pub issue_id: i64,
    /// False when this event id was already stored, i.e. an SDK retry we deduplicated.
    pub stored: bool,
    /// True when this event opened a new issue.
    pub new_issue: bool,
    /// Set when this event queued a notification for an agent to pick up.
    pub notified: Option<Trigger>,
}

/// Why an issue was queued for triage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// First time this fingerprint has been seen.
    NewIssue,
    /// An issue that had been resolved started happening again.
    Regression,
}

impl Trigger {
    fn as_str(self) -> &'static str {
        match self {
            Trigger::NewIssue => "new_issue",
            Trigger::Regression => "regression",
        }
    }
}

/// Distinct values stored per (issue, tag key). Tag cardinality is data-controlled — a client can
/// send a unique value per event — and issue_tags survives retention, so without a cap the rollup
/// is unbounded permanent storage. Past the cap, known values still count up; new ones are
/// dropped. For the `user` key this freezes `users_affected` at the cap.
const MAX_TAG_VALUES_PER_KEY: i64 = 1000;

pub async fn digest(
    db: &PgPool,
    project_id: i64,
    component: Option<&str>,
    payload: &Value,
    received_at: DateTime<Utc>,
) -> AppResult<Digested> {
    let event_id = event::event_id(payload);
    let timestamp = event::timestamp(payload, received_at);
    let level = event::level(payload);
    let grouping = grouping::group(payload);
    let release = event::str_field(payload, "release");

    let mut tx = db.begin().await?;

    // 0. Record the release. The row's id is the release ordering ("first reported later" =
    //    newer), which is what resolved-until-next-release compares against. Past the per-project
    //    release cap this resolves to `None`, and the event is treated as having no release rather
    //    than growing the table forever.
    let release_id: Option<i64> = match release {
        Some(version) => releases::resolve(&mut tx, project_id, version).await?,
        None => None,
    };

    let issue = open_issue(&mut tx, project_id, payload, &grouping, level, timestamp).await?;
    let stored = store_event(
        &mut tx,
        project_id,
        issue.id,
        payload,
        &grouping,
        level,
        event_id,
        timestamp,
        received_at,
    )
    .await?;

    // Whether this event is allowed to reopen a resolved issue. A plain resolve reopens on any
    // recurrence. Resolved-until-next-release reopens only for a release *first seen after* the
    // one it was resolved in — traffic from the broken deploy that is still out there is not
    // news — or for an event with no release, which cannot be proven old.
    let reopens = match (issue.resolved_in_release_id, release_id) {
        (None, _) | (Some(_), None) => true,
        (Some(resolved_in), Some(event_release)) => event_release > resolved_in,
    };

    // Only a genuinely new event advances anything. Every rollup below is guarded by this, so an
    // SDK retry can neither inflate a counter nor move a display field.
    if stored {
        advance_issue(&mut tx, issue.id, &grouping, level, timestamp, reopens).await?;
        bump_event_counts(&mut tx, project_id, issue.id, level, timestamp).await?;
        roll_up_tags(&mut tx, project_id, issue.id, component, payload, timestamp).await?;
    }

    // Queue the issue for triage, in this same transaction. That is the whole point of the outbox:
    // there is no window in which an issue exists but nothing knows to look at it. A duplicate
    // event triggers nothing, so SDK retries cannot spam the queue.
    let notified = match (stored, issue.new_issue, issue.status.as_str()) {
        (true, true, _) => Some(Trigger::NewIssue),
        (true, false, "resolved") if reopens => Some(Trigger::Regression),
        _ => None,
    };

    if let Some(trigger) = notified {
        sqlx::query("insert into notifications (project_id, issue_id, kind) values ($1, $2, $3)")
            .bind(project_id)
            .bind(issue.id)
            .bind(trigger.as_str())
            .execute(&mut *tx)
            .await?;
        // The history line for a reopen, in the same transaction as the reopen itself.
        if matches!(trigger, Trigger::Regression) {
            crate::api::activity::record_regression(
                &mut tx,
                issue.id,
                release_id,
                issue.resolved_in_release_id,
            )
            .await?;
        }
    }

    tx.commit().await?;

    Ok(Digested {
        event_id,
        issue_id: issue.id,
        stored,
        new_issue: issue.new_issue,
        notified,
    })
}

// region:    --- Support

/// The issue this event belongs to, as it stood *before* the event was applied.
#[derive(sqlx::FromRow)]
struct OpenedIssue {
    id: i64,
    /// True when this event opened the issue rather than joining an existing one.
    new_issue: bool,
    /// Status before this event, which is what tells us whether this is a regression.
    status: String,
    resolved_in_release_id: Option<i64>,
}

/// Finds or opens the issue for `grouping`, mutating nothing on an existing one — we do not yet
/// know whether this event is new or a retry of one already stored.
///
/// The no-op `set` exists only because `on conflict do nothing` would return no row, and we need
/// the id. `xmax = 0` distinguishes an inserted row from an updated one, and `status` comes back
/// unchanged by the no-op update, so it is the status *before* this event.
async fn open_issue(
    conn: &mut sqlx::PgConnection,
    project_id: i64,
    payload: &Value,
    grouping: &grouping::Grouping,
    level: &str,
    timestamp: DateTime<Utc>,
) -> AppResult<OpenedIssue> {
    let issue = sqlx::query_as(
        "insert into issues (
             project_id, fingerprint_hash, title, culprit, exception_type, exception_value,
             level, platform, first_seen, last_seen, times_seen
         )
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9, 0)
         on conflict (project_id, fingerprint_hash) do update
             set project_id = issues.project_id
         returning id, (xmax = 0) as new_issue, status, resolved_in_release_id",
    )
    .bind(project_id)
    .bind(&grouping.hash)
    .bind(&grouping.title)
    .bind(&grouping.culprit)
    .bind(&grouping.exception_type)
    .bind(&grouping.exception_value)
    .bind(level)
    .bind(event::str_field(payload, "platform"))
    .bind(timestamp)
    .fetch_one(conn)
    .await?;

    Ok(issue)
}

/// Stores the event row, returning false when the SDK already delivered this event id — a retry,
/// which must not be counted twice.
#[allow(clippy::too_many_arguments)]
async fn store_event(
    conn: &mut sqlx::PgConnection,
    project_id: i64,
    issue_id: i64,
    payload: &Value,
    grouping: &grouping::Grouping,
    level: &str,
    event_id: Uuid,
    timestamp: DateTime<Utc>,
    received_at: DateTime<Utc>,
) -> AppResult<bool> {
    let stored: Option<(i64,)> = sqlx::query_as(
        "insert into events (
             event_id, project_id, issue_id, timestamp, received_at, level, platform,
             environment, release, transaction, server_name, message, data
         )
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
         on conflict (project_id, event_id) do nothing
         returning id",
    )
    .bind(event_id)
    .bind(project_id)
    .bind(issue_id)
    .bind(timestamp)
    .bind(received_at)
    .bind(level)
    .bind(event::str_field(payload, "platform"))
    .bind(event::str_field(payload, "environment"))
    .bind(event::str_field(payload, "release"))
    .bind(event::str_field(payload, "transaction"))
    .bind(event::str_field(payload, "server_name"))
    .bind(&grouping.title)
    .bind(payload)
    .fetch_optional(conn)
    .await?;

    Ok(stored.is_some())
}

/// Advances the issue's counters and display fields for a newly stored event.
///
/// Every `set` expression below sees the pre-update row, so `$2 >= last_seen` means "this event is
/// the newest we have seen" — which keeps an out-of-order or backfilled delivery from overwriting
/// the display fields with older details.
///
/// A resolved issue that sees traffic again is reopened (subject to `reopens`), and reopening
/// moves the release marker to `regressed_from_release_id`: the release the fix was verified
/// against is the regression's last known good, which is what lets an agent diff good..bad instead
/// of reading the whole repo. An ignored one stays ignored — that is what ignoring it meant.
async fn advance_issue(
    conn: &mut sqlx::PgConnection,
    issue_id: i64,
    grouping: &grouping::Grouping,
    level: &str,
    timestamp: DateTime<Utc>,
    reopens: bool,
) -> AppResult<()> {
    sqlx::query(
        "update issues
            set times_seen = times_seen + 1,
                first_seen = least(first_seen, $2),
                last_seen  = greatest(last_seen, $2),
                status     = case when status = 'resolved' and $8 then 'unresolved' else status end,
                regressed_from_release_id
                           = case when status = 'resolved' and $8 then resolved_in_release_id
                                  else regressed_from_release_id end,
                resolved_in_release_id
                           = case when status = 'resolved' and $8 then null
                                  else resolved_in_release_id end,
                title           = case when $2 >= last_seen then $3 else title end,
                culprit         = case when $2 >= last_seen then $4 else culprit end,
                exception_type  = case when $2 >= last_seen then $5 else exception_type end,
                exception_value = case when $2 >= last_seen then $6 else exception_value end,
                level           = case when $2 >= last_seen then $7 else level end
          where id = $1",
    )
    .bind(issue_id)
    .bind(timestamp)
    .bind(&grouping.title)
    .bind(&grouping.culprit)
    .bind(&grouping.exception_type)
    .bind(&grouping.exception_value)
    .bind(level)
    .bind(reopens)
    .execute(conn)
    .await?;

    Ok(())
}

/// Bumps the hourly rollup that every chart and sparkline reads.
async fn bump_event_counts(
    conn: &mut sqlx::PgConnection,
    project_id: i64,
    issue_id: i64,
    level: &str,
    timestamp: DateTime<Utc>,
) -> AppResult<()> {
    sqlx::query(
        "insert into event_counts (project_id, issue_id, bucket, level, count)
         values ($1, $2, date_trunc('hour', $3::timestamptz), $4, 1)
         on conflict (issue_id, bucket, level)
             do update set count = event_counts.count + 1",
    )
    .bind(project_id)
    .bind(issue_id)
    .bind(timestamp)
    .bind(level)
    .execute(conn)
    .await?;

    Ok(())
}

/// Bumps the per-issue tag rollup, which is what "filter issues by environment" reads.
///
/// Maintained here rather than computed from `events` because events are dropped by retention and
/// tag distributions must outlive them. A never-before-seen value is only inserted while the key is
/// under its cardinality cap (known values keep counting past it); `xmax = 0` reports which rows
/// are genuinely new, and a new `user` row advances the users_affected counter — the issue list
/// reads that counter, never count(*) over this table.
async fn roll_up_tags(
    conn: &mut sqlx::PgConnection,
    project_id: i64,
    issue_id: i64,
    component: Option<&str>,
    payload: &Value,
    timestamp: DateTime<Utc>,
) -> AppResult<()> {
    let mut tags = event::tags(payload);

    // The component comes from the DSN key the event authenticated with, not the payload, so it is
    // synthesized here. Operator-named at key creation, so its cardinality is bounded by
    // configuration rather than the cap alone. An SDK tag of the same name wins — explicit beats
    // inferred.
    if let Some(component) = component
        && !tags.iter().any(|(k, _)| k == "component")
    {
        tags.push(("component".to_string(), component.to_string()));
    }

    if tags.is_empty() {
        return Ok(());
    }

    let (keys, values): (Vec<String>, Vec<String>) = tags.into_iter().unzip();
    let touched: Vec<(String, bool)> = sqlx::query_as(
        "insert into issue_tags (project_id, issue_id, key, value, times_seen, last_seen)
         select $1, $2, key, value, 1, $3
           from unnest($4::text[], $5::text[]) as tags (key, value)
          where exists (select 1 from issue_tags cur
                         where cur.issue_id = $2
                           and cur.key = tags.key and cur.value = tags.value)
             or (select count(*) from issue_tags cur
                  where cur.issue_id = $2 and cur.key = tags.key) < $6
         on conflict (issue_id, key, value) do update
             set times_seen = issue_tags.times_seen + 1,
                 last_seen  = greatest(issue_tags.last_seen, excluded.last_seen)
         returning key, (xmax = 0) as inserted",
    )
    .bind(project_id)
    .bind(issue_id)
    .bind(timestamp)
    .bind(&keys)
    .bind(&values)
    .bind(MAX_TAG_VALUES_PER_KEY)
    .fetch_all(&mut *conn)
    .await?;

    let new_users = touched
        .iter()
        .filter(|(key, inserted)| *inserted && key == "user")
        .count() as i64;
    if new_users > 0 {
        sqlx::query("update issues set users_affected = users_affected + $2 where id = $1")
            .bind(issue_id)
            .bind(new_users)
            .execute(conn)
            .await?;
    }

    Ok(())
}

// endregion: --- Support
