//! Alerts delivery gave up on, and the retry — the one alert-related thing a person does by hand.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;

use crate::alerts::{self, DeadLetter};
use crate::error::{AppError, AppResult};
use crate::state::ThermiteState;

pub async fn dead(State(state): State<ThermiteState>) -> AppResult<Json<Vec<DeadLetter>>> {
    Ok(Json(alerts::dead_lettered(&state.db).await?))
}

/// 404 rather than a silent no-op when there is nothing dead-lettered under that id: a retry
/// that "worked" on an alert that was never abandoned would mislead.
pub async fn retry(
    State(state): State<ThermiteState>,
    Path(id): Path<i64>,
) -> AppResult<StatusCode> {
    if alerts::retry(&state.db, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound)
    }
}
