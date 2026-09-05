use axum::Json;
use axum::extract::State;
use serde::Serialize;
use sqlx::PgPool;

use crate::config::Config;
use crate::error::{AppError, AppResult};
use crate::state::ThermiteState;

#[derive(Debug, Serialize)]
pub struct ProjectSummary {
    pub id: i64,
    pub slug: String,
    pub name: String,
    /// The DSN to configure an SDK with. Includes the public key, which is not a secret — it only
    /// grants the ability to send events.
    pub dsn: String,
    pub unresolved_issues: i64,
    pub events_last_24h: i64,
    /// Per-project alert routing; null falls back to the instance-wide configuration.
    pub alert_email: Option<String>,
    pub alert_webhook: Option<String>,
    /// Where this project's code lives. An agent triaging one of its issues gets this with the
    /// work, so it can open a pull request without being told the repository out of band.
    pub repo_url: Option<String>,
    /// Additional labeled DSNs, one per component ('worker', 'saas'). Events through one
    /// carry `component: <label>` as a filterable tag; `dsn` above stays the unlabeled
    /// default.
    pub keys: Vec<ProjectKey>,
}

#[derive(Debug, Serialize)]
pub struct ProjectKey {
    pub label: String,
    pub dsn: String,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: i64,
    slug: String,
    name: String,
    public_key: String,
    unresolved_issues: i64,
    events_last_24h: i64,
    alert_email: Option<String>,
    alert_webhook: Option<String>,
    repo_url: Option<String>,
}

pub async fn list(State(state): State<ThermiteState>) -> AppResult<Json<Vec<ProjectSummary>>> {
    Ok(Json(all(&state.db, &state.config).await?))
}

/// Shared with the MCP tool of the same name.
pub async fn all(db: &PgPool, config: &Config) -> AppResult<Vec<ProjectSummary>> {
    let rows: Vec<Row> = sqlx::query_as(
        "select p.id,
                p.slug,
                p.name,
                p.public_key,
                p.alert_email,
                p.alert_webhook,
                p.repo_url,
                coalesce(i.unresolved_issues, 0) as unresolved_issues,
                coalesce(e.events_last_24h, 0)   as events_last_24h
           from projects p
           left join (
                select project_id, count(*) as unresolved_issues
                  from issues
                 where status = 'unresolved'
                 group by project_id
           ) i on i.project_id = p.id
           -- From the rollup, not from `events`: counting raw rows here was a full table scan.
           left join (
                select project_id, sum(count)::bigint as events_last_24h
                  from event_counts
                 where bucket >= date_trunc('hour', now()) - interval '23 hours'
                 group by project_id
           ) e on e.project_id = p.id
          order by p.id",
    )
    .fetch_all(db)
    .await?;

    // The labeled component keys, grouped onto their projects below. The unlabeled seed
    // key is already the summary's `dsn`, so it is not repeated here.
    let key_rows: Vec<(i64, String, String)> = sqlx::query_as(
        "select project_id, label, public_key
           from project_keys
          where label is not null
          order by id",
    )
    .fetch_all(db)
    .await?;

    let projects = rows
        .into_iter()
        .map(|row| ProjectSummary {
            dsn: config.dsn(&row.public_key, row.id),
            keys: key_rows
                .iter()
                .filter(|(project_id, _, _)| *project_id == row.id)
                .map(|(_, label, public_key)| ProjectKey {
                    label: label.clone(),
                    dsn: config.dsn(public_key, row.id),
                })
                .collect(),
            id: row.id,
            slug: row.slug,
            name: row.name,
            unresolved_issues: row.unresolved_issues,
            events_last_24h: row.events_last_24h,
            alert_email: row.alert_email,
            alert_webhook: row.alert_webhook,
            repo_url: row.repo_url,
        })
        .collect();

    Ok(projects)
}

/// The slug of a project by id, so a detail view can link back to its list.
pub async fn slug_of(db: &PgPool, project_id: i64) -> AppResult<String> {
    let row: Option<(String,)> = sqlx::query_as("select slug from projects where id = $1")
        .bind(project_id)
        .fetch_optional(db)
        .await?;

    row.map(|(slug,)| slug).ok_or(AppError::NotFound)
}
