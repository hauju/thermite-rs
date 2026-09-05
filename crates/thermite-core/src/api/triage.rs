//! The triage queue an agent drains.
//!
//! The loop is: `claim` → diagnose → `POST /issues/{id}/analyses` → `ack`.
//!
//! Claims carry a lease rather than a bare flag. A `/loop` that fires every 10 minutes while a
//! triage run takes 12 would otherwise hand the same issue to the next tick; with a lease the row
//! stays invisible until the lease expires, and an agent that dies mid-diagnosis releases its work
//! automatically instead of stranding it.

use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::api::page_size;
use crate::error::{AppError, AppResult};
use crate::state::ThermiteState;

const KINDS: [&str; 2] = ["new_issue", "regression"];

/// Default lease. Long enough for an agent to read a repo and think, short enough that a crashed
/// run is retried within one loop interval.
const DEFAULT_LEASE_SECONDS: i64 = 900;

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct TriageItem {
    pub id: i64,
    pub kind: String,
    pub created_at: DateTime<Utc>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub lease_until: Option<DateTime<Utc>>,

    pub issue_id: i64,
    pub project_id: i64,
    pub project_slug: String,
    pub title: String,
    pub culprit: Option<String>,
    pub level: String,
    pub times_seen: i64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,

    /// The release the most recent event reported. When this carries a git SHA, an agent can check
    /// out the revision that actually crashed instead of reasoning about `main`.
    pub release: Option<String>,
    /// The release the oldest retained event reported — for a new issue, the upper bound on where
    /// the bug was introduced.
    pub first_seen_release: Option<String>,
    /// For regressions: the release the issue was resolved against before it reopened — the last
    /// known good. With `release` as the bad side, `git diff <regressed_from_release>..<release>`
    /// is the change set that reintroduced the bug. Null for plain resolves, which carry no
    /// release anchor.
    pub regressed_from_release: Option<String>,
    pub environment: Option<String>,
    /// The project's repository, when one is configured. With `release` as the revision that
    /// crashed, this is the last thing an agent needs to open a pull request rather than stop at
    /// a written diagnosis.
    pub repo_url: Option<String>,
}

/// Columns shared by peek and claim. Both join through to the issue so a caller has enough to act
/// on without a second round trip.
const TRIAGE_COLUMNS: &str = "
    n.id, n.kind, n.created_at, n.claimed_at, n.lease_until,
    i.id as issue_id, i.project_id, p.slug as project_slug,
    i.title, i.culprit, i.level, i.times_seen, i.first_seen, i.last_seen,
    p.repo_url,
    (select e.release     from events e where e.issue_id = i.id
      order by e.timestamp desc, e.id desc limit 1) as release,
    (select e.release     from events e where e.issue_id = i.id
      order by e.timestamp asc,  e.id asc  limit 1) as first_seen_release,
    (select r.version from releases r
      where r.id = i.regressed_from_release_id)     as regressed_from_release,
    (select e.environment from events e where e.issue_id = i.id
      order by e.timestamp desc, e.id desc limit 1) as environment
";

#[derive(Debug, Deserialize)]
pub struct PendingQuery {
    project: Option<String>,
    kind: Option<String>,
    level: Option<String>,
    limit: Option<i64>,
}

fn validate_kind(kind: Option<&str>) -> AppResult<()> {
    if let Some(kind) = kind
        && !KINDS.contains(&kind)
    {
        return Err(AppError::BadRequest(format!(
            "unknown kind {kind:?}; expected one of {}",
            KINDS.join(", ")
        )));
    }
    Ok(())
}

/// Read-only view of what is waiting. Changes nothing — use `claim` to actually take work.
pub async fn pending(
    State(state): State<ThermiteState>,
    Query(query): Query<PendingQuery>,
) -> AppResult<Json<Vec<TriageItem>>> {
    Ok(Json(
        pending_items(
            &state.db,
            query.project.as_deref(),
            query.kind.as_deref(),
            query.level.as_deref(),
            query.limit,
        )
        .await?,
    ))
}

/// Shared with the MCP `pending_triage` tool.
pub async fn pending_items(
    db: &PgPool,
    project: Option<&str>,
    kind: Option<&str>,
    level: Option<&str>,
    limit: Option<i64>,
) -> AppResult<Vec<TriageItem>> {
    validate_kind(kind)?;

    let items: Vec<TriageItem> = sqlx::query_as(&format!(
        "select {TRIAGE_COLUMNS}
           from notifications n
           join issues   i on i.id = n.issue_id
           join projects p on p.id = n.project_id
          where n.acked_at is null
            and (n.lease_until is null or n.lease_until < now())
            and ($1::text is null or p.slug = $1)
            and ($2::text is null or n.kind = $2)
            and ($3::text is null or i.level = $3)
          order by n.created_at
          limit $4"
    ))
    .bind(project)
    .bind(kind)
    .bind(level)
    .bind(page_size(limit))
    .fetch_all(db)
    .await?;

    Ok(items)
}

#[derive(Debug, Deserialize)]
pub struct ClaimRequest {
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    /// How long the claim holds before the work becomes available again.
    #[serde(default)]
    pub lease_seconds: Option<i64>,
    /// Recorded on the row so it is visible who is working on what.
    #[serde(default)]
    pub claimed_by: Option<String>,
}

/// Atomically takes up to `limit` items off the queue.
///
/// `for update skip locked` is what makes this safe to call from several agents at once: each
/// transaction takes a disjoint set rather than blocking on the same rows.
pub async fn claim(
    State(state): State<ThermiteState>,
    Json(request): Json<ClaimRequest>,
) -> AppResult<Json<Vec<TriageItem>>> {
    Ok(Json(claim_items(&state.db, request).await?))
}

/// Shared with the MCP `claim_triage` tool.
pub async fn claim_items(db: &PgPool, request: ClaimRequest) -> AppResult<Vec<TriageItem>> {
    validate_kind(request.kind.as_deref())?;

    let lease_seconds = request
        .lease_seconds
        .unwrap_or(DEFAULT_LEASE_SECONDS)
        .clamp(30, 24 * 3600);

    let items: Vec<TriageItem> = sqlx::query_as(&format!(
        "with claimed as (
             update notifications
                set claimed_at  = now(),
                    claimed_by  = $5,
                    lease_until = now() + make_interval(secs => $6)
              where id in (
                    select n.id
                      from notifications n
                      join issues i on i.id = n.issue_id
                      join projects p on p.id = n.project_id
                     where n.acked_at is null
                       and (n.lease_until is null or n.lease_until < now())
                       and ($1::text is null or p.slug = $1)
                       and ($2::text is null or n.kind = $2)
                       and ($3::text is null or i.level = $3)
                     order by n.created_at
                     limit $4
                     for update of n skip locked
              )
             returning *
         )
         select {TRIAGE_COLUMNS}
           from claimed n
           join issues   i on i.id = n.issue_id
           join projects p on p.id = n.project_id
          order by n.created_at"
    ))
    .bind(&request.project)
    .bind(&request.kind)
    .bind(&request.level)
    .bind(page_size(request.limit))
    .bind(&request.claimed_by)
    .bind(lease_seconds as f64)
    .fetch_all(db)
    .await?;

    Ok(items)
}

#[derive(Debug, Serialize)]
pub struct AckResponse {
    pub id: i64,
    pub acked_at: DateTime<Utc>,
}

/// Marks a notification done. Idempotent: acking twice returns the original timestamp rather than
/// failing, because an agent that crashes after acking will retry.
pub async fn ack(
    State(state): State<ThermiteState>,
    Path(id): Path<i64>,
) -> AppResult<Json<AckResponse>> {
    Ok(Json(ack_item(&state.db, id).await?))
}

/// Shared with the MCP `ack_triage` tool.
pub async fn ack_item(db: &PgPool, id: i64) -> AppResult<AckResponse> {
    let row: Option<(i64, DateTime<Utc>)> = sqlx::query_as(
        "update notifications
            set acked_at = coalesce(acked_at, now())
          where id = $1
          returning id, acked_at",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;

    match row {
        Some((id, acked_at)) => Ok(AckResponse { id, acked_at }),
        None => Err(AppError::NotFound),
    }
}

/// Releases a claim without acking, so another agent can pick the work up immediately.
pub async fn release(
    State(state): State<ThermiteState>,
    Path(id): Path<i64>,
) -> AppResult<Json<TriageItem>> {
    let item: Option<TriageItem> = sqlx::query_as(&format!(
        "with released as (
             update notifications
                set claimed_at = null, claimed_by = null, lease_until = null
              where id = $1 and acked_at is null
             returning *
         )
         select {TRIAGE_COLUMNS}
           from released n
           join issues   i on i.id = n.issue_id
           join projects p on p.id = n.project_id"
    ))
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    item.map(Json).ok_or(AppError::NotFound)
}
