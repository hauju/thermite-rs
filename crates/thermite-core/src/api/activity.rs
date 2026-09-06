//! An issue's history: status changes, reopens, analyses and notes, in the order they happened.
//!
//! Status changes and regressions are written to `issue_activity` as they happen; analyses and
//! notes already live in `issue_analyses` and are merged in on read, so nothing is stored twice.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::PgPool;

use crate::error::AppResult;

/// One entry of an issue's history.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Activity {
    /// `status`, `regression`, `analysis` or `note`.
    pub kind: String,
    /// Who did it: a user's name, `api`, an agent's source — or nothing, for ingest.
    pub actor: Option<String>,
    /// `status`: `from`, `to`, `in_next_release`, `release` (the anchor, when resolved until the
    /// next one). `regression`: `release`, `regressed_from`. `analysis` / `note`: `summary`,
    /// `confidence`, `fix_url`.
    pub detail: Value,
    pub created_at: DateTime<Utc>,
}

/// Everything that happened to an issue, oldest first.
pub async fn for_issue(db: &PgPool, issue_id: i64) -> AppResult<Vec<Activity>> {
    let rows: Vec<Activity> = sqlx::query_as(
        "select kind, actor, detail, created_at
           from issue_activity
          where issue_id = $1
         union all
         select case when metadata->>'kind' = 'note' then 'note' else 'analysis' end as kind,
                source as actor,
                jsonb_strip_nulls(jsonb_build_object(
                    'summary', summary,
                    'confidence', confidence,
                    'fix_url', fix_url
                )) as detail,
                created_at
           from issue_analyses
          where issue_id = $1
          order by created_at, kind",
    )
    .bind(issue_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Records a status change somebody made. Nothing is written when nothing changed, so a repeated
/// "resolve" is not a line in the history.
pub async fn record_status(
    conn: &mut sqlx::PgConnection,
    issue_id: i64,
    actor: Option<&str>,
    from: &str,
    to: &str,
    in_next_release: bool,
    release: Option<&str>,
) -> AppResult<()> {
    if from == to && !in_next_release {
        return Ok(());
    }
    sqlx::query(
        "insert into issue_activity (issue_id, kind, actor, detail)
         values ($1, 'status', $2, jsonb_strip_nulls(jsonb_build_object(
                    'from', $3::text, 'to', $4::text,
                    'in_next_release', $5::bool, 'release', $6::text)))",
    )
    .bind(issue_id)
    .bind(actor)
    .bind(from)
    .bind(to)
    .bind(in_next_release)
    .bind(release)
    .execute(conn)
    .await?;
    Ok(())
}

/// Records that ingest reopened a resolved issue: which release it came back in, and which one
/// the fix had been verified against.
pub async fn record_regression(
    conn: &mut sqlx::PgConnection,
    issue_id: i64,
    release_id: Option<i64>,
    regressed_from_release_id: Option<i64>,
) -> AppResult<()> {
    sqlx::query(
        "insert into issue_activity (issue_id, kind, actor, detail)
         values ($1, 'regression', null, jsonb_strip_nulls(jsonb_build_object(
                    'release', (select version from releases where id = $2),
                    'regressed_from', (select version from releases where id = $3))))",
    )
    .bind(issue_id)
    .bind(release_id)
    .bind(regressed_from_release_id)
    .execute(conn)
    .await?;
    Ok(())
}
