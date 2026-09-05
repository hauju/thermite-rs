//! The alert half of the notifications outbox: rows waiting to be delivered to a *human*.
//!
//! Agents drain the same outbox through `api::triage`, but with different columns — an agent
//! acking its triage work says nothing about whether anyone was told, so the two consumers do not
//! share state. This module only knows which rows need delivering; what "delivering" means
//! (email, a webhook) is the application's business, which is why nothing here touches SMTP or
//! HTTP.
//!
//! Four properties, each the answer to a way alerts used to be lost or multiplied:
//!
//! - **At-least-once**: a row is fully marked only after every configured channel succeeded, so a
//!   crash in between re-sends rather than losing the alert.
//! - **Per-channel success** (`alert_email_at`, `alert_webhook_at`): a healthy channel is not
//!   re-sent every minute because a sibling keeps failing.
//! - **Backoff and a dead-letter** (`alert_attempts`, `alert_failed_at`): a permanently
//!   undeliverable row stops occupying the head of the queue instead of blocking everything
//!   behind it forever.
//! - **A lease** (`alert_lease_until`, the triage queue's pattern): with N replicas, exactly one
//!   delivers a given row per interval instead of all N.
//!
//! Eligibility starts at a durable **backlog floor** — the moment alerting was first enabled —
//! not a rolling window: enabling alerting on a months-old instance must not flood the recipient,
//! but an outage longer than any fixed window must lose nothing.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::error::AppResult;

/// How long a claim suppresses rival deliverers. Long enough for a slow SMTP conversation, short
/// enough that a crashed replica's work is retaken promptly.
const LEASE_MINUTES: i64 = 5;

/// Everything an alert message needs, joined in one query.
#[derive(Debug, sqlx::FromRow)]
pub struct Alert {
    pub id: i64,
    /// `new_issue` or `regression`.
    pub kind: String,
    pub created_at: DateTime<Utc>,

    pub issue_id: i64,
    pub project_slug: String,
    pub title: String,
    pub culprit: Option<String>,
    pub level: String,
    pub times_seen: i64,

    pub release: Option<String>,
    pub environment: Option<String>,

    /// Channels that already succeeded on an earlier attempt — skip them, don't re-send.
    pub email_done: bool,
    pub webhook_done: bool,

    /// Per-project routing overrides. Set, they replace the instance-wide recipients for this
    /// alert; null falls back to the global configuration.
    pub project_alert_email: Option<String>,
    pub project_alert_webhook: Option<String>,
}

/// A delivery channel, for recording partial success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Email,
    Webhook,
}

/// Records the backlog floor, once. Rows created before it are never offered. Must run before
/// the first [`claim`]; without a floor nothing is eligible at all — failing closed, so a missed
/// call cannot flood anyone.
pub async fn ensure_backlog_floor(db: &PgPool) -> AppResult<()> {
    sqlx::query(
        "insert into alert_state (id, backlog_floor) values (1, now()) on conflict (id) do nothing",
    )
    .execute(db)
    .await?;
    Ok(())
}

/// Rides the floor along with the clock while alerting is entirely unconfigured — no global
/// channel (`globally_configured` is the application's word for that) and no per-project
/// override anywhere. The moment configuration appears the floor stops moving, so "enable
/// alerting months into an instance's life" floods nobody, while an *outage* of a configured
/// channel (which does not move the floor) still loses nothing.
pub async fn advance_floor_while_unconfigured(
    db: &PgPool,
    globally_configured: bool,
) -> AppResult<()> {
    sqlx::query(
        "update alert_state
            set backlog_floor = now()
          where not $1
            and not exists (
                select 1 from projects
                 where alert_email is not null or alert_webhook is not null
            )",
    )
    .bind(globally_configured)
    .execute(db)
    .await?;
    Ok(())
}

/// Claims up to `limit` deliverable alerts, oldest first, leasing them against rival replicas.
///
/// Deliverable means: not fully alerted, not dead-lettered, not under a live lease, past its
/// backoff, created after the backlog floor — and *addressable*: when no global channel is
/// configured (`globally_configured` false), only projects with their own routing override are
/// claimed, so rows nobody could deliver are not burned through the attempt counter. A claimed
/// row the caller never resolves (crash, hang) is re-offered when its lease expires.
pub async fn claim(db: &PgPool, limit: i64, globally_configured: bool) -> AppResult<Vec<Alert>> {
    let alerts: Vec<Alert> = sqlx::query_as(
        "with claimed as (
             update notifications
                set alert_lease_until = now() + make_interval(secs => $2)
              where id in (
                    select n.id from notifications n
                      join projects p on p.id = n.project_id
                     where n.alerted_at is null
                       and n.alert_failed_at is null
                       and (n.alert_lease_until is null or n.alert_lease_until < now())
                       and (n.alert_next_attempt_at is null or n.alert_next_attempt_at <= now())
                       and n.created_at >= coalesce(
                               (select backlog_floor from alert_state), 'infinity')
                       and ($3 or p.alert_email is not null or p.alert_webhook is not null)
                     order by n.created_at
                     limit $1
                     for update of n skip locked
              )
              returning *
         )
         select n.id, n.kind, n.created_at,
                i.id as issue_id, p.slug as project_slug,
                i.title, i.culprit, i.level, i.times_seen,
                (select e.release     from events e where e.issue_id = i.id
                  order by e.timestamp desc, e.id desc limit 1) as release,
                (select e.environment from events e where e.issue_id = i.id
                  order by e.timestamp desc, e.id desc limit 1) as environment,
                n.alert_email_at   is not null as email_done,
                n.alert_webhook_at is not null as webhook_done,
                p.alert_email   as project_alert_email,
                p.alert_webhook as project_alert_webhook
           from claimed n
           join issues   i on i.id = n.issue_id
           join projects p on p.id = n.project_id
          order by n.created_at",
    )
    .bind(limit.clamp(1, 500))
    .bind((LEASE_MINUTES * 60) as f64)
    .bind(globally_configured)
    .fetch_all(db)
    .await?;

    Ok(alerts)
}

/// Records that one channel took the alert, so a retry of its siblings will not re-send it.
/// Idempotent, and keeps the first delivery time.
pub async fn mark_channel(db: &PgPool, id: i64, channel: Channel) -> AppResult<()> {
    let sql = match channel {
        Channel::Email => {
            "update notifications set alert_email_at = coalesce(alert_email_at, now()) where id = $1"
        }
        Channel::Webhook => {
            "update notifications set alert_webhook_at = coalesce(alert_webhook_at, now()) where id = $1"
        }
    };
    sqlx::query(sql).bind(id).execute(db).await?;
    Ok(())
}

/// Records that an alert was fully delivered. Idempotent, and keeps the first delivery time.
pub async fn mark_alerted(db: &PgPool, id: i64) -> AppResult<()> {
    sqlx::query("update notifications set alerted_at = coalesce(alerted_at, now()) where id = $1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Records a failed attempt: exponential backoff (1, 2, 4 … capped at 60 minutes), the lease
/// released so the retry is not also gated on it, and the row dead-lettered once `max_attempts`
/// is spent. Returns `true` when this failure was the one that dead-lettered the row — the
/// caller's cue to log loudly, because from here on nobody will be told.
pub async fn record_failure(db: &PgPool, id: i64, max_attempts: i32) -> AppResult<bool> {
    let (_, failed): (i32, bool) = sqlx::query_as(
        "update notifications
            set alert_attempts        = alert_attempts + 1,
                alert_next_attempt_at = now() + least(
                    power(2, alert_attempts) * interval '1 minute', interval '1 hour'),
                alert_failed_at       = case when alert_attempts + 1 >= $2 then now() end,
                alert_lease_until     = null
          where id = $1
          returning alert_attempts, alert_failed_at is not null",
    )
    .bind(id)
    .bind(max_attempts.max(1))
    .fetch_one(db)
    .await?;

    Ok(failed)
}
