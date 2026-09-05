use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::server::db::Database;
use crate::server::router;
use crate::server::test_support::{seed_api_key, seed_user, test_state};

const PUBLIC_KEY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// Serve the real stack on a loopback port, exactly as `main` does.
async fn serve(pool: PgPool) -> String {
    serve_state(test_state(Database::from_pool(pool))).await
}

async fn serve_state(state: crate::server::state::AppState) -> String {
    let app = router::build(Router::new(), state).await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    format!("http://{addr}")
}

/// Serve with the per-project ingest quota forced down to one event per minute, so a test
/// can trip it with two requests.
async fn serve_with_tiny_quota(pool: PgPool) -> String {
    let mut state = test_state(Database::from_pool(pool));
    let config = thermite_core::Config {
        base_url: "http://localhost:8099".into(),
        max_envelope_bytes: 20 * 1024 * 1024,
        rate_limit_per_minute: Some(1),
        scrub_fields: thermite_core::protocol::scrub::ScrubList::new([]),
    };
    state.thermite_ingest = thermite_core::ThermiteState::new(state.db.ingest_pool.clone(), config);
    serve_state(state).await
}

async fn create_project(pool: &PgPool, slug: &str) -> i64 {
    let id: i64 = sqlx::query_scalar(
        "insert into projects (slug, name, public_key) values ($1, $1, $2) returning id",
    )
    .bind(slug)
    .bind(PUBLIC_KEY)
    .fetch_one(pool)
    .await
    .unwrap();
    // Ingest authenticates against project_keys, so the fixture seeds it too.
    sqlx::query("insert into project_keys (project_id, public_key) values ($1, $2)")
        .bind(id)
        .bind(PUBLIC_KEY)
        .execute(pool)
        .await
        .unwrap();
    id
}

/// A one-event envelope, framed the way an SDK sends it.
fn envelope(event: Value) -> String {
    let payload = event.to_string();
    format!(
        "{{}}\n{{\"type\":\"event\",\"length\":{}}}\n{payload}\n",
        payload.len()
    )
}

fn error_event(event_id: &str, exception_type: &str, value: &str) -> Value {
    json!({
        "event_id": event_id,
        "timestamp": "2026-07-29T10:00:00Z",
        "platform": "rust",
        "level": "error",
        "release": "9f8e7d6c",
        "exception": { "values": [{
            "type": exception_type,
            "value": value,
            "stacktrace": { "frames": [
                { "function": "handle", "filename": "handler.rs", "lineno": 42, "in_app": true },
            ]}
        }]}
    })
}

async fn ingest(base: &str, project_id: i64, event: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/api/{project_id}/envelope/"))
        .header(
            "X-Sentry-Auth",
            format!("Sentry sentry_key={PUBLIC_KEY}, sentry_version=7"),
        )
        // As a browser SDK would: CORS response headers are only emitted for requests that
        // carry an Origin, and the 429-contract test needs to see them.
        .header("Origin", "https://app.example.test")
        .body(envelope(event))
        .send()
        .await
        .unwrap()
}

#[test]
fn ingest_paths_are_recognised_and_nothing_else_is() {
    use super::is_ingest_path;

    assert!(is_ingest_path("/api/1/envelope/"));
    assert!(is_ingest_path("/api/42/store/"));
    assert!(is_ingest_path("/api/42/envelope"));
    // An OTel collector exports on a schedule and needs the same SDK-readable 429 the envelope
    // endpoints get; the global backstop's plain-text one carries no Retry-After.
    assert!(is_ingest_path("/api/42/otlp/v1/logs"));

    // The read API, and anything else, must stay behind the global backstop.
    assert!(!is_ingest_path("/api/v1/issues"));
    assert!(!is_ingest_path("/api/1x/envelope/"));
    assert!(!is_ingest_path("/api//envelope/"));
    assert!(!is_ingest_path("/api/1/envelope/extra"));
    assert!(!is_ingest_path("/api/1/otlp/v1/traces"));
    assert!(!is_ingest_path("/health"));
}

/// The documented contract — `429` + `Retry-After` + `X-Sentry-Rate-Limits`, readable
/// cross-origin — asserted through the *full* middleware stack, where a backstop layered
/// outside the ingest CORS layer would silently strip all three.
#[sqlx::test]
async fn the_ingest_429_keeps_its_contract_through_the_middleware_stack(pool: PgPool) {
    let project_id = create_project(&pool, "demo").await;
    let base = serve_with_tiny_quota(pool.clone()).await;

    let first = ingest(
        &base,
        project_id,
        error_event(&"1".repeat(32), "ValueError", "a"),
    )
    .await;
    assert_eq!(first.status(), 200);

    let second = ingest(
        &base,
        project_id,
        error_event(&"2".repeat(32), "ValueError", "b"),
    )
    .await;
    assert_eq!(second.status(), 429);

    let headers = second.headers();
    assert!(
        headers.get("retry-after").is_some(),
        "SDKs need the backoff hint: {headers:?}"
    );
    assert!(
        headers.get("x-sentry-rate-limits").is_some(),
        "SDKs read the rate-limit categories: {headers:?}"
    );
    assert!(
        headers.get("access-control-allow-origin").is_some(),
        "a browser SDK cannot read the rejection without CORS: {headers:?}"
    );
}

#[sqlx::test]
async fn sdks_reach_ingest_without_any_application_credential(pool: PgPool) {
    // The whole point of the split: an SDK has a DSN key and nothing else. If ingest ever
    // ended up behind the session or API-key layer, every client would silently stop
    // reporting.
    let project_id = create_project(&pool, "demo").await;
    let base = serve(pool.clone()).await;

    let response = ingest(
        &base,
        project_id,
        error_event(&"1".repeat(32), "ValueError", "bad input"),
    )
    .await;

    assert_eq!(response.status(), 200);
    let count: i64 = sqlx::query_scalar("select count(*) from events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test]
async fn a_wrong_dsn_key_is_rejected(pool: PgPool) {
    let project_id = create_project(&pool, "demo").await;
    let base = serve(pool.clone()).await;

    let response = reqwest::Client::new()
        .post(format!("{base}/api/{project_id}/envelope/"))
        .header("X-Sentry-Auth", "Sentry sentry_key=wrong, sentry_version=7")
        .body(envelope(error_event(&"1".repeat(32), "E", "v")))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 401);
}

#[sqlx::test]
async fn the_read_api_refuses_anonymous_callers(pool: PgPool) {
    create_project(&pool, "demo").await;
    let base = serve(pool.clone()).await;

    for path in [
        "/api/v1/projects",
        "/api/v1/projects/demo/issues",
        "/api/v1/projects/demo/stats",
        "/api/v1/triage/pending",
        "/api/v1/issues/1",
    ] {
        let response = reqwest::Client::new()
            .get(format!("{base}{path}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401, "{path} was reachable without a key");
    }
}

#[sqlx::test]
async fn an_api_key_opens_the_read_api(pool: PgPool) {
    let db = Database::from_pool(pool.clone());
    let project_id = create_project(&pool, "demo").await;
    let user = seed_user(&db, "operator").await;
    let token = seed_api_key(&db, user).await;
    let base = serve(pool.clone()).await;

    ingest(
        &base,
        project_id,
        error_event(&"1".repeat(32), "DatabaseError", "pool timeout"),
    )
    .await;

    let response = reqwest::Client::new()
        .get(format!("{base}/api/v1/projects/demo/issues"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    let issues: Value = response.json().await.unwrap();
    assert_eq!(issues[0]["title"], "DatabaseError: pool timeout");
    // The sparkline the dashboard renders comes back with the row.
    assert_eq!(issues[0]["counts"].as_array().unwrap().len(), 24);
}

#[sqlx::test]
async fn a_revoked_key_stops_working(pool: PgPool) {
    let db = Database::from_pool(pool.clone());
    create_project(&pool, "demo").await;
    let user = seed_user(&db, "operator").await;
    let token = seed_api_key(&db, user).await;
    let base = serve(pool.clone()).await;

    sqlx::query("update api_keys set revoked_at = now()")
        .execute(&pool)
        .await
        .unwrap();

    let response = reqwest::Client::new()
        .get(format!("{base}/api/v1/projects"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
}

#[sqlx::test]
async fn a_project_can_be_created_through_the_api_and_immediately_reported_into(pool: PgPool) {
    // The bootstrap path: without this there is no way to obtain a DSN.
    let db = Database::from_pool(pool.clone());
    let user = seed_user(&db, "operator").await;
    let token = seed_api_key(&db, user).await;
    let base = serve(pool.clone()).await;

    let created: Value = reqwest::Client::new()
        .post(format!("{base}/api/v1/projects"))
        .bearer_auth(&token)
        .json(&json!({ "slug": "checkout", "name": "Checkout Service" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(created["slug"], "checkout");
    let dsn = created["dsn"].as_str().unwrap();
    let key = dsn.split("://").nth(1).unwrap().split('@').next().unwrap();
    let project_id = created["id"].as_i64().unwrap();

    // The DSN it handed out actually works.
    let event = error_event(&"1".repeat(32), "ValueError", "bad input");
    let payload = event.to_string();
    let response = reqwest::Client::new()
        .post(format!("{base}/api/{project_id}/envelope/"))
        .header(
            "X-Sentry-Auth",
            format!("Sentry sentry_key={key}, sentry_version=7"),
        )
        .body(format!(
            "{{}}\n{{\"type\":\"event\",\"length\":{}}}\n{payload}\n",
            payload.len()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    let count: i64 = sqlx::query_scalar("select count(*) from events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test]
async fn a_duplicate_slug_is_refused(pool: PgPool) {
    let db = Database::from_pool(pool.clone());
    let user = seed_user(&db, "operator").await;
    let token = seed_api_key(&db, user).await;
    let base = serve(pool.clone()).await;

    for expected in [201, 400] {
        let response = reqwest::Client::new()
            .post(format!("{base}/api/v1/projects"))
            .bearer_auth(&token)
            .json(&json!({ "slug": "checkout" }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
}

#[sqlx::test]
async fn an_unmodified_sentry_sdk_reports_into_thermite(pool: PgPool) {
    // The compatibility proof: no hand-built envelopes, just the real client over TCP.
    let project_id = create_project(&pool, "demo").await;
    let base = serve(pool.clone()).await;
    let dsn = format!(
        "http://{PUBLIC_KEY}@{}/{project_id}",
        base.trim_start_matches("http://")
    );

    tokio::task::spawn_blocking(move || {
        let mut options = sentry_sdk::ClientOptions::default();
        options.dsn = Some(dsn.parse().expect("invalid DSN"));

        // `apply_defaults` installs the HTTP transport; without it the client silently
        // has no transport and drops every event.
        let client = Arc::new(sentry_sdk::Client::from_config(sentry_sdk::apply_defaults(
            options,
        )));
        let hub = sentry_sdk::Hub::new(Some(client.clone()), Arc::new(Default::default()));

        hub.capture_message("payment gateway unreachable", sentry_sdk::Level::Error);
        assert!(
            client.flush(Some(Duration::from_secs(10))),
            "the SDK failed to flush to thermite"
        );
    })
    .await
    .unwrap();

    let title: String = sqlx::query_scalar("select title from issues")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(title, "Log Message: payment gateway unreachable");
}

#[sqlx::test]
async fn an_unmodified_sentry_sdk_reports_release_health(pool: PgPool) {
    // Same proof for sessions: the SDK's own session tracking, over TCP, with nothing on our
    // side hand-building the envelope. This is what would break silently if the item type,
    // the `attrs.release` path or the init/terminal split ever stopped matching the wire.
    let project_id = create_project(&pool, "demo").await;
    let base = serve(pool.clone()).await;
    let dsn = format!(
        "http://{PUBLIC_KEY}@{}/{project_id}",
        base.trim_start_matches("http://")
    );

    tokio::task::spawn_blocking(move || {
        let mut options = sentry_sdk::ClientOptions::default();
        options.dsn = Some(dsn.parse().expect("invalid DSN"));
        // Release health is release-scoped by definition; without this the SDK still sends
        // sessions and ingest drops every one of them.
        options.release = Some("1.4.2".into());
        options.auto_session_tracking = true;
        options.session_mode = sentry_sdk::SessionMode::Request;

        let client = Arc::new(sentry_sdk::Client::from_config(sentry_sdk::apply_defaults(
            options,
        )));
        let hub = sentry_sdk::Hub::new(Some(client.clone()), Arc::new(Default::default()));

        hub.start_session();
        hub.capture_message("payment gateway unreachable", sentry_sdk::Level::Error);
        hub.end_session();

        assert!(
            client.flush(Some(Duration::from_secs(10))),
            "the SDK failed to flush to thermite"
        );
    })
    .await
    .unwrap();

    let (sessions, errored): (i64, i64) = sqlx::query_as(
        "select coalesce(sum(sc.sessions), 0)::bigint, coalesce(sum(sc.errored), 0)::bigint
           from session_counts sc
           join releases r on r.id = sc.release_id
          where r.version = '1.4.2'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(
        sessions, 1,
        "the SDK's session should reach the rollup once"
    );
    assert_eq!(errored, 1, "the session captured an error before ending");
}
