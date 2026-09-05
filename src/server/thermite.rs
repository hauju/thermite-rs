//! Mounting `thermite-core` into the application.
//!
//! Two routers with deliberately different exposure:
//!
//! - **Ingest** authenticates with a DSN public key and must be reachable by anything on the
//!   internet. It carries no session, no CSRF, and no API key.
//! - **The read and triage API** is for operators and agents, so it is wrapped in the same
//!   [`ApiAuth`] check the rest of the application uses — `oat_` keys or a FerrisKey JWT.
//!
//! `thermite-core` supplies the read routes unauthenticated on purpose: it has no business
//! knowing about sessions or OAuth, and inverting the dependency would drag all of that into it.

use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Router};

use crate::models::AppError;
use crate::server::api_auth;
use crate::server::security::{self, IpRateLimiter};
use crate::server::state::AppState;

/// Per-IP ceiling for the ingest endpoints. This is the only per-IP gate on ingest — the global
/// backstop skips these paths (see [`is_ingest_path`]) precisely so this limiter, with its
/// SDK-readable rejection, is the one that answers.
const INGEST_REQUESTS_PER_IP_PER_MINUTE: u32 = 6000;

/// The SDK-facing ingest endpoints, plus a per-IP quota.
///
/// The per-project quota inside `thermite-core` bounds how much any one project can store; this
/// one bounds how much traffic a single source can cost us before we even look at the payload.
pub fn ingest_router(state: AppState, trust_proxy_headers: bool) -> Router {
    let limiter = IpRateLimiter::per_minute(INGEST_REQUESTS_PER_IP_PER_MINUTE, trust_proxy_headers);

    // The ingest-pool state, so an error storm cannot exhaust the interactive pool.
    thermite_core::ingest_routes(state.thermite_ingest.clone())
        .layer(axum::middleware::from_fn(ingest_ip_rate_limit))
        .layer(Extension(limiter))
}

/// True for the SDK-facing ingest paths (`/api/{integer}/envelope/`, `/store/`, `/otlp/v1/logs`).
///
/// The global per-IP backstop consults this to stay out of the way: its plain-text 429 carries
/// none of the headers below, and it sits outside the ingest CORS layer, so a browser SDK could
/// not even read the rejection.
pub fn is_ingest_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/api/") else {
        return false;
    };
    let Some((project_id, endpoint)) = rest.split_once('/') else {
        return false;
    };

    !project_id.is_empty()
        && project_id.bytes().all(|b| b.is_ascii_digit())
        && matches!(
            endpoint,
            "envelope/" | "store/" | "envelope" | "store" | "otlp/v1/logs"
        )
}

/// Enforces the ingest per-IP quota with the documented Sentry rejection, not the plain one:
/// `Retry-After` and `X-Sentry-Rate-Limits` are what make an SDK back off instead of hammering,
/// and the CORS headers are what let a browser SDK see the response at all — the ingest router's
/// own CorsLayer sits below this middleware and never processes a short-circuited rejection.
async fn ingest_ip_rate_limit(
    Extension(limiter): Extension<IpRateLimiter>,
    request: Request,
    next: Next,
) -> Response {
    let peer = security::peer_addr(&request);
    if security::within_quota(&limiter, request.headers(), peer).await {
        next.run(request).await
    } else {
        ingest_rate_limited()
    }
}

fn ingest_rate_limited() -> Response {
    let mut response = (StatusCode::TOO_MANY_REQUESTS, "Too many requests").into_response();

    let headers = response.headers_mut();
    // One limiter window: the earliest moment the bucket could have drained.
    headers.insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
    // `retry_after:categories:scope:reason_code`, as in thermite-core's per-project 429.
    headers.insert(
        "x-sentry-rate-limits",
        HeaderValue::from_static("60:error:ip:too_many_requests"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("retry-after, x-sentry-error, x-sentry-rate-limits"),
    );

    response
}

/// The read and triage API, behind the application's API authentication.
pub fn api_router(state: AppState) -> Router {
    thermite_core::api_routes(state.thermite.clone())
        .layer(axum::middleware::from_fn(require_api_auth))
}

/// Rejects anything without a valid `oat_` API key or FerrisKey JWT.
///
/// Checked once at the edge rather than per handler, so a new route cannot be added unprotected by
/// forgetting an extractor.
async fn require_api_auth(
    Extension(state): Extension<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    api_auth::authenticate(&state, request.headers()).await?;
    Ok(next.run(request).await)
}

#[cfg(test)]
#[path = "thermite_tests.rs"]
mod tests;
