//! Shared harness for the integration tests.
//!
//! Compiled into each test binary separately, so not every helper is used by every one of them.
#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use serde_json::Value;
use sqlx::PgPool;
use thermite_core::config::Config;
use thermite_core::protocol::scrub::ScrubList;
use thermite_core::state::ThermiteState;
use tower::ServiceExt;

/// The routers under test, assembled the way the application mounts them — minus the
/// application's authentication, which lives in the app crate and is tested there.
fn router(state: ThermiteState) -> axum::Router {
    thermite_core::ingest_routes(state.clone()).merge(thermite_core::api_routes(state))
}

pub const PUBLIC_KEY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

pub fn config() -> Config {
    Config {
        base_url: "http://localhost:9000".into(),
        max_envelope_bytes: 20 * 1024 * 1024,
        rate_limit_per_minute: None,
        scrub_fields: ScrubList::new([]),
    }
}

pub fn state(db: PgPool) -> ThermiteState {
    ThermiteState::new(db, config())
}

/// Creates a project and returns its id.
///
/// Ingest authenticates against `project_keys` (as `admin::create_project` seeds it),
/// so the fixture seeds the key row too.
pub async fn create_project(db: &PgPool, slug: &str, public_key: &str) -> i64 {
    let id: i64 = sqlx::query_scalar(
        "insert into projects (slug, name, public_key) values ($1, $1, $2) returning id",
    )
    .bind(slug)
    .bind(public_key)
    .fetch_one(db)
    .await
    .expect("failed to create project");
    sqlx::query("insert into project_keys (project_id, public_key) values ($1, $2)")
        .bind(id)
        .bind(public_key)
        .execute(db)
        .await
        .expect("failed to seed project key");
    id
}

/// Drives one request through the router without binding a socket.
pub async fn send(state: ThermiteState, request: Request<Body>) -> Response<Body> {
    router(state).oneshot(request).await.expect("router failed")
}

pub async fn body_json(response: Response<Body>) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("failed to read body");
    serde_json::from_slice(&bytes).expect("body was not JSON")
}

/// A `POST /api/{id}/envelope/` request authenticated with `X-Sentry-Auth`, the way sentry-rust
/// sends it.
pub fn envelope_request(project_id: i64, public_key: &str, body: impl Into<Body>) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/api/{project_id}/envelope/"))
        .header(
            "X-Sentry-Auth",
            format!("Sentry sentry_key={public_key}, sentry_version=7, sentry_client=test/1.0"),
        )
        .body(body.into())
        .expect("failed to build request")
}

/// Builds an envelope with explicit item lengths, as SDKs do.
pub fn envelope(envelope_headers: Value, items: &[(Value, Value)]) -> String {
    let mut out = envelope_headers.to_string();
    out.push('\n');

    for (headers, payload) in items {
        let payload = payload.to_string();
        let mut headers = headers.clone();
        headers["length"] = Value::from(payload.len());

        out.push_str(&headers.to_string());
        out.push('\n');
        out.push_str(&payload);
        out.push('\n');
    }

    out
}

/// An RFC3339 timestamp `hours` before now, at whole-second precision so it round-trips through
/// TIMESTAMPTZ exactly.
///
/// Fixtures use offsets from the wall clock rather than hardcoded dates: ingest clamps
/// client-supplied timestamps to a 30-day window around `received_at`, so a hardcoded date is a
/// time bomb that starts failing the day the calendar moves past it.
pub fn hours_ago(hours: i64) -> String {
    chrono::DateTime::from_timestamp(chrono::Utc::now().timestamp() - hours * 3600, 0)
        .expect("in range")
        .to_rfc3339()
}

/// A minimal exception event payload.
pub fn error_event(event_id: &str, exception_type: &str, value: &str) -> Value {
    serde_json::json!({
        "event_id": event_id,
        "timestamp": hours_ago(1),
        "platform": "rust",
        "level": "error",
        "exception": { "values": [{
            "type": exception_type,
            "value": value,
            "stacktrace": { "frames": [
                { "function": "main", "filename": "main.rs", "lineno": 1, "in_app": true },
                { "function": "handle", "filename": "handler.rs", "lineno": 42, "in_app": true },
            ]}
        }]}
    })
}

#[derive(Debug, sqlx::FromRow, PartialEq)]
pub struct IssueRow {
    pub id: i64,
    pub title: String,
    pub culprit: Option<String>,
    pub level: String,
    pub times_seen: i64,
}

pub async fn issues(db: &PgPool) -> Vec<IssueRow> {
    sqlx::query_as("select id, title, culprit, level, times_seen from issues order by id")
        .fetch_all(db)
        .await
        .expect("failed to read issues")
}

pub async fn count(db: &PgPool, table: &str) -> i64 {
    // Literal SQL per table: sqlx 0.9 rejects interpolated query strings outright.
    let sql = match table {
        "events" => "select count(*) from events",
        "issues" => "select count(*) from issues",
        "releases" => "select count(*) from releases",
        "session_counts" => "select count(*) from session_counts",
        "issue_analyses" => "select count(*) from issue_analyses",
        other => panic!("count() does not know the table {other:?}"),
    };

    sqlx::query_scalar(sql)
        .fetch_one(db)
        .await
        .expect("failed to count")
}

pub fn assert_status(response: &Response<Body>, expected: StatusCode) {
    assert_eq!(
        response.status(),
        expected,
        "unexpected status; headers: {:?}",
        response.headers()
    );
}
