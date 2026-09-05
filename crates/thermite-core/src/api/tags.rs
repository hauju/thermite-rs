//! Per-project tag values: what is there to filter by?
//!
//! `GET /api/v1/projects/{slug}/tags/environment` is what feeds an environment dropdown; any other
//! key works the same way. Read from the issue_tags rollup, so the answer survives event eviction.

use axum::Json;
use axum::extract::{Path, State};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;

use crate::error::{AppError, AppResult};
use crate::state::ThermiteState;

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct TagValue {
    pub value: String,
    pub times_seen: i64,
    pub last_seen: DateTime<Utc>,
}

pub async fn values(
    State(state): State<ThermiteState>,
    Path((slug, key)): Path<(String, String)>,
) -> AppResult<Json<Vec<TagValue>>> {
    Ok(Json(values_of(&state.db, &slug, &key).await?))
}

/// Shared with the dashboard's environment dropdown.
pub async fn values_of(db: &PgPool, slug: &str, key: &str) -> AppResult<Vec<TagValue>> {
    let project_id: Option<(i64,)> = sqlx::query_as("select id from projects where slug = $1")
        .bind(slug)
        .fetch_optional(db)
        .await?;
    let Some((project_id,)) = project_id else {
        return Err(AppError::NotFound);
    };

    // sum() because the rollup is per issue; the endpoint answers per project.
    let values: Vec<TagValue> = sqlx::query_as(
        "select value, sum(times_seen)::bigint as times_seen, max(last_seen) as last_seen
           from issue_tags
          where project_id = $1 and key = $2
          group by value
          order by times_seen desc, value
          limit 100",
    )
    .bind(project_id)
    .bind(key)
    .fetch_all(db)
    .await?;

    Ok(values)
}
