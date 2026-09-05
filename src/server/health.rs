//! Health probes for container orchestrators and uptime checks.
//!
//! Two endpoints with deliberately different meanings:
//!
//! - `GET /health` is *liveness*: the process is up and serving. It never touches the database,
//!   because the Docker `HEALTHCHECK` restarts the container on failure — and a probe that flips
//!   under pool saturation would restart the server in the middle of the error storm it exists
//!   to record.
//! - `GET /ready` is *readiness*: it round-trips a query to PostgreSQL, so a load balancer can
//!   stop routing to an instance whose database is unreachable. Nothing should restart on it.
//!
//! Both responses are deliberately generic — these endpoints are unauthenticated, so they must
//! never leak connection strings or error detail (the specifics go to the log instead).

use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::server::state::AppState;

/// Budget for the readiness probe query.
///
/// Must stay comfortably under the prober's own timeout. Without it the query would inherit the
/// pool's acquire timeout, so an unreachable database would hang the probe rather than answering
/// `503` promptly.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

pub fn health_router() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
}

async fn health() -> Response {
    (StatusCode::OK, "ok").into_response()
}

async fn ready(state: AppState) -> Response {
    let probe = sqlx::query!("SELECT 1 as one").fetch_one(&state.db.pool);

    match tokio::time::timeout(PROBE_TIMEOUT, probe).await {
        Ok(Ok(_)) => (StatusCode::OK, "ready").into_response(),
        Ok(Err(e)) => {
            tracing::error!("readiness check failed: database error: {e}");
            (StatusCode::SERVICE_UNAVAILABLE, "not ready").into_response()
        }
        Err(_) => {
            tracing::error!(
                "readiness check failed: database did not respond within {PROBE_TIMEOUT:?}"
            );
            (StatusCode::SERVICE_UNAVAILABLE, "not ready").into_response()
        }
    }
}
