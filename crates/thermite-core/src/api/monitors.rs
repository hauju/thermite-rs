//! Cron monitors of a project: what is scheduled, and did it run?
//!
//! Read-only. Monitors are created and configured by the jobs themselves through the check-in
//! protocol (see [`crate::monitors`]), so there is nothing here to POST.

use axum::Json;
use axum::extract::{Path, State};
use sqlx::PgPool;

use crate::error::{AppError, AppResult};
use crate::monitors::Monitor;
use crate::state::ThermiteState;

pub async fn list(
    State(state): State<ThermiteState>,
    Path(slug): Path<String>,
) -> AppResult<Json<Vec<Monitor>>> {
    Ok(Json(of_project(&state.db, &slug).await?))
}

/// Shared with the dashboard and the MCP tool.
pub async fn of_project(db: &PgPool, slug: &str) -> AppResult<Vec<Monitor>> {
    let project: Option<(i64,)> = sqlx::query_as("select id from projects where slug = $1")
        .bind(slug)
        .fetch_optional(db)
        .await?;
    let Some((project_id,)) = project else {
        return Err(AppError::NotFound);
    };

    crate::monitors::list(db, project_id).await
}
