//! The Sentry-compatible ingest endpoints.
//!
//! Wire contract verified against sentry-rust 0.49 and <https://develop.sentry.dev>:
//!
//! - `POST /api/{project_id}/envelope/` — the modern endpoint. `project_id` must be an integer;
//!   SDKs parse it out of the DSN path.
//! - `POST /api/{project_id}/store/` — a bare event payload, for older SDKs.
//! - Credentials arrive as `?sentry_key=`, as `X-Sentry-Auth`, or as a `dsn` envelope header,
//!   checked in that order. Browser SDKs cannot always set headers, hence the query parameter.
//! - Success is `200` with `{"id": "<32-hex event id>"}`.

pub mod digest;
pub mod otlp;
pub mod outcomes;
pub mod ratelimit;
pub mod releases;
pub mod sessions;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::post;
use serde::Deserialize;
use serde_json::{Value, json};
use tower_http::cors::{Any, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use uuid::Uuid;

use crate::auth;
use crate::error::{AppError, AppResult};
use crate::monitors;
use crate::protocol::{client_report, compression, envelope, scrub};
use crate::state::ThermiteState;

pub fn routes(state: ThermiteState) -> Router<ThermiteState> {
    let max_body = state.config.max_envelope_bytes;

    Router::new()
        .route("/api/{project_id}/envelope/", post(envelope_endpoint))
        .route("/api/{project_id}/store/", post(store_endpoint))
        // OTLP logs, for services already instrumented with OpenTelemetry. Same credential and
        // same quota as the endpoints above; see `ingest::otlp`.
        .route("/api/{project_id}/otlp/v1/logs", post(otlp::logs_endpoint))
        .layer(RequestBodyLimitLayer::new(max_body))
        // Browser SDKs send cross-origin, so preflight has to succeed and the backoff headers have
        // to be readable by JavaScript or the SDK cannot honor them.
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any)
                .expose_headers([
                    header::RETRY_AFTER,
                    header::HeaderName::from_static("x-sentry-error"),
                    header::HeaderName::from_static("x-sentry-rate-limits"),
                ]),
        )
}

#[derive(Debug, Deserialize)]
pub struct IngestQuery {
    sentry_key: Option<String>,
}

struct Project {
    id: i64,
    /// The component label of the key this request authenticated with, synthesized
    /// onto its events as a `component` tag. None for an unlabeled (default) key.
    component: Option<String>,
}

async fn envelope_endpoint(
    State(state): State<ThermiteState>,
    Path(project_id): Path<i64>,
    Query(query): Query<IngestQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<impl IntoResponse> {
    let received_at = chrono::Utc::now();

    let project = authorize(
        &state,
        project_id,
        &query,
        &headers,
        Some(&body),
        received_at,
    )
    .await?;

    let body = decode_body(&state, &headers, &body)?;
    let parsed = envelope::parse(&body).map_err(|e| AppError::BadRequest(e.to_string()))?;

    // Fall back to the envelope's event id so the response carries something meaningful even for an
    // envelope with no event item (a session-only flush, say).
    let mut response_id = parsed
        .headers
        .event_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok());

    let mut dropped = Vec::new();
    let mut counts = outcomes::Counts::default();
    for item in &parsed.items {
        match item.item_type() {
            // Cron check-ins ride the same endpoint and credential as errors (see
            // `thermite_core::monitors`).
            "check_in" => record_check_in(&state, project.id, item, &mut counts).await?,

            // Sessions are the release-health denominator (see `thermite_core::ingest::sessions`).
            // They are folded into a rollup rather than stored, and never become events — a
            // session is not something that went wrong. They carry no outcome either: outcomes
            // account for events, and routine session flushes would drown the drop counts in
            // noise.
            "session" | "sessions" => {
                sessions::record(&state.db, project.id, item, received_at).await?
            }

            // Client reports are the SDK's own drop counters: what it discarded before sending,
            // and otherwise the one loss nothing here can see (see `protocol::client_report`).
            // They cost no quota — nothing is stored, and an SDK reporting its losses must not be
            // charged for saying so.
            "client_report" => record_client_report(project.id, item, &mut counts),

            "event" => {
                // One unit of quota per event. Hitting the limit mid-envelope returns 429 with
                // whatever was already digested left committed — each digest is its own
                // transaction, and an accepted event is durable by design. The SDK retries the
                // envelope, and the event ids it already delivered deduplicate on the way back in.
                if let Err(e) = check_quota(&state, project.id) {
                    counts.bump(outcomes::Outcome::OverQuota);
                    counts.flush(&state.db, project.id, received_at).await?;
                    return Err(e);
                }

                if let Some(event_id) =
                    digest_event(&state, &project, item, received_at, &mut counts).await?
                {
                    response_id = Some(event_id);
                }
            }

            // Transactions, logs, attachments: not stored yet. Rejecting the envelope over them
            // would break every real SDK, so they are counted and dropped — under a key naming
            // the type, because which one it was decides whether it matters.
            other => {
                counts.bump_unsupported(other);
                dropped.push(other.to_string());
            }
        }
    }

    counts.flush(&state.db, project.id, received_at).await?;

    if !dropped.is_empty() {
        tracing::debug!(project_id = project.id, items = ?dropped, "dropped unsupported envelope items");
    }

    Ok(accepted(response_id))
}

/// Authenticates the request and rejects an already-exhausted project, both before any work
/// proportional to the body.
///
/// The envelope's own `dsn` header is the last credential source, but it sits on the first line, so
/// recovering it costs one header-sized prefix rather than the whole body. Decoding and parsing
/// first — as this used to — let an unauthenticated request expand a small compressed body into an
/// item list several orders of magnitude larger, for any project id, valid or not.
///
/// The quota check charges nothing: the quota is charged per event item, because one envelope can
/// carry many events and charging per request let a single one store hundreds for the price of one.
/// The rejection is still recorded as an outcome — one constant-cost upsert, not body-proportional
/// work — or over-quota drops would be invisible on the dashboard.
async fn authorize(
    state: &ThermiteState,
    project_id: i64,
    query: &IngestQuery,
    headers: &HeaderMap,
    body: Option<&Bytes>,
    received_at: chrono::DateTime<chrono::Utc>,
) -> AppResult<Project> {
    let key = credential(query, headers)
        .or_else(|| body.and_then(|body| dsn_key_from_prefix(state, headers, body)))
        .ok_or_else(|| AppError::Unauthorized("missing sentry_key".into()))?;

    let project = authenticate(state, project_id, &key).await?;

    if let Some(retry_after) = state.rate_limiter.exhausted(project.id) {
        outcomes::record(
            &state.db,
            project.id,
            outcomes::Outcome::OverQuota,
            1,
            received_at,
        )
        .await?;
        return Err(AppError::RateLimited { retry_after });
    }

    Ok(project)
}

async fn record_check_in(
    state: &ThermiteState,
    project_id: i64,
    item: &envelope::Item<'_>,
    counts: &mut outcomes::Counts,
) -> AppResult<()> {
    match serde_json::from_slice::<monitors::CheckIn>(item.payload) {
        Ok(check_in) => {
            monitors::record_check_in(&state.db, project_id, &check_in).await?;
        }
        Err(e) => {
            counts.bump(outcomes::Outcome::Invalid);
            tracing::warn!(project_id, error = %e, "skipping unparseable check-in item");
        }
    }

    Ok(())
}

fn record_client_report(project_id: i64, item: &envelope::Item<'_>, counts: &mut outcomes::Counts) {
    match serde_json::from_slice::<client_report::ClientReport>(item.payload) {
        Ok(report) => {
            for (reason, quantity) in report.event_discards() {
                counts.add_client_discarded(reason, quantity);
            }
        }
        Err(e) => {
            counts.bump(outcomes::Outcome::Invalid);
            tracing::warn!(project_id, error = %e, "skipping unparseable client report");
        }
    }
}

/// Digests one event item, returning its event id, or `None` when the item was unparseable and
/// skipped — one malformed item must not discard the rest of the envelope.
async fn digest_event(
    state: &ThermiteState,
    project: &Project,
    item: &envelope::Item<'_>,
    received_at: chrono::DateTime<chrono::Utc>,
    counts: &mut outcomes::Counts,
) -> AppResult<Option<Uuid>> {
    let mut payload: Value = match serde_json::from_slice(item.payload) {
        Ok(payload) => payload,
        Err(e) => {
            counts.bump(outcomes::Outcome::Invalid);
            tracing::warn!(project_id = project.id, error = %e, "skipping unparseable event item");
            return Ok(None);
        }
    };

    // Scrub before digest so a credential in the payload is never written at all.
    scrub::scrub(&mut payload, &state.config.scrub_fields);

    let digested = digest::digest(
        &state.db,
        project.id,
        project.component.as_deref(),
        &payload,
        received_at,
    )
    .await?;

    counts.bump(outcomes::Outcome::Accepted);
    log_digested(project.id, &digested);

    Ok(Some(digested.event_id))
}

async fn store_endpoint(
    State(state): State<ThermiteState>,
    Path(project_id): Path<i64>,
    Query(query): Query<IngestQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<impl IntoResponse> {
    let received_at = chrono::Utc::now();

    let key = credential(&query, &headers)
        .ok_or_else(|| AppError::Unauthorized("missing sentry_key".into()))?;
    let project = authenticate(&state, project_id, &key).await?;
    if let Err(e) = check_quota(&state, project.id) {
        outcomes::record(
            &state.db,
            project.id,
            outcomes::Outcome::OverQuota,
            1,
            received_at,
        )
        .await?;
        return Err(e);
    }

    let body = decode_body(&state, &headers, &body)?;
    let mut payload: Value = match serde_json::from_slice(&body) {
        Ok(payload) => payload,
        Err(e) => {
            outcomes::record(
                &state.db,
                project.id,
                outcomes::Outcome::Invalid,
                1,
                received_at,
            )
            .await?;
            return Err(AppError::BadRequest(format!("invalid event payload: {e}")));
        }
    };

    scrub::scrub(&mut payload, &state.config.scrub_fields);

    let digested = digest::digest(
        &state.db,
        project.id,
        project.component.as_deref(),
        &payload,
        received_at,
    )
    .await?;
    outcomes::record(
        &state.db,
        project.id,
        outcomes::Outcome::Accepted,
        1,
        received_at,
    )
    .await?;
    log_digested(project.id, &digested);

    Ok(accepted(Some(digested.event_id)))
}

/// The success response every Sentry SDK expects: `200` with the event id, hex, no dashes.
fn accepted(event_id: Option<Uuid>) -> impl IntoResponse {
    let id = event_id.unwrap_or_else(Uuid::new_v4).simple().to_string();
    (StatusCode::OK, axum::Json(json!({ "id": id })))
}

fn decode_body(state: &ThermiteState, headers: &HeaderMap, body: &Bytes) -> AppResult<Vec<u8>> {
    compression::decode(
        body,
        content_encoding(headers),
        state.config.max_envelope_bytes,
    )
    .map_err(|e| match e {
        compression::DecodeError::TooLarge { .. } => AppError::PayloadTooLarge,
        other => AppError::BadRequest(other.to_string()),
    })
}

fn content_encoding(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
}

/// How much of the body may be decoded before the caller is authenticated.
///
/// Only the envelope's first line is needed, and that line is one JSON object carrying an event id,
/// a DSN and a timestamp. 8 KiB is far more than any real SDK puts there, and small enough that the
/// pre-auth decode cannot be used as an amplifier.
const PREAUTH_PREFIX_BYTES: usize = 8 * 1024;

/// Recovers the `dsn` credential from the envelope's first line.
///
/// Returns `None` when the prefix cannot be decoded or carries no usable DSN. The caller then
/// rejects the request as unauthenticated; it must not decode the rest of the body to look harder.
fn dsn_key_from_prefix(state: &ThermiteState, headers: &HeaderMap, body: &Bytes) -> Option<String> {
    let limit = PREAUTH_PREFIX_BYTES.min(state.config.max_envelope_bytes);
    let prefix = compression::decode_prefix(body, content_encoding(headers), limit)?;

    envelope::parse_envelope_headers(&prefix)
        .ok()?
        .dsn
        .as_deref()
        .and_then(auth::sentry_key_from_dsn)
}

/// Resolves the DSN public key from the query string, then the `X-Sentry-Auth` header.
fn credential(query: &IngestQuery, headers: &HeaderMap) -> Option<String> {
    if let Some(key) = query.sentry_key.as_deref().filter(|k| !k.is_empty()) {
        return Some(key.to_string());
    }

    headers
        .get("x-sentry-auth")
        .and_then(|v| v.to_str().ok())
        .and_then(auth::sentry_key_from_auth_header)
}

/// Looks up the project by id *and* key together.
///
/// A wrong key is reported as 401 rather than 404 so SDKs treat it as fatal and stop retrying. We
/// do not check that the DSN's own project id matches the path, matching Bugsink — the key is the
/// credential, and the path is just routing. Keys live in `project_keys` (the original
/// `projects.public_key` is seeded there by migration), and a labeled key stamps its component
/// onto everything ingested through it.
async fn authenticate(state: &ThermiteState, project_id: i64, key: &str) -> AppResult<Project> {
    let row: Option<(i64, Option<String>)> = sqlx::query_as(
        "select k.project_id, k.label
           from project_keys k
          where k.project_id = $1 and k.public_key = $2",
    )
    .bind(project_id)
    .bind(key)
    .fetch_optional(&state.db)
    .await?;

    match row {
        Some((id, component)) => Ok(Project { id, component }),
        None => Err(AppError::Unauthorized(
            "invalid project id or sentry_key".into(),
        )),
    }
}

fn check_quota(state: &ThermiteState, project_id: i64) -> AppResult<()> {
    state
        .rate_limiter
        .check(project_id)
        .map_err(|retry_after| AppError::RateLimited { retry_after })
}

fn log_digested(project_id: i64, digested: &digest::Digested) {
    if digested.new_issue {
        tracing::info!(
            project_id,
            issue_id = digested.issue_id,
            event_id = %digested.event_id,
            "new issue"
        );
    } else if !digested.stored {
        tracing::debug!(
            project_id,
            event_id = %digested.event_id,
            "duplicate event ignored"
        );
    }
}
