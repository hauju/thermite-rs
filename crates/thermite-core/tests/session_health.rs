//! Release health: sessions into the rollup, and the crash-free rate back out.
//!
//! The counting rule itself is unit-tested in `protocol::session`. These cover what only a real
//! database can show: that the rollup is keyed and bucketed the way the rate assumes.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

/// Sends session items as one envelope, the way an SDK flushes them.
async fn send_sessions(db: &PgPool, project_id: i64, items: &[(Value, Value)]) {
    let body = envelope(json!({}), items);
    let response = send(
        state(db.clone()),
        envelope_request(project_id, PUBLIC_KEY, body),
    )
    .await;
    assert_status(&response, StatusCode::OK);
}

async fn session(db: &PgPool, project_id: i64, payload: Value) {
    send_sessions(db, project_id, &[(json!({ "type": "session" }), payload)]).await;
}

/// The rollup row for a release, as (sessions, errored, crashed, abnormal).
async fn totals(db: &PgPool, version: &str) -> (i64, i64, i64, i64) {
    sqlx::query_as(
        "select coalesce(sum(sc.sessions), 0)::bigint, coalesce(sum(sc.errored), 0)::bigint,
                coalesce(sum(sc.crashed), 0)::bigint,  coalesce(sum(sc.abnormal), 0)::bigint
           from session_counts sc
           join releases r on r.id = sc.release_id
          where r.version = $1",
    )
    .bind(version)
    .fetch_one(db)
    .await
    .expect("failed to read session_counts")
}

async fn health(db: &PgPool) -> Value {
    let response = send(state(db.clone()), get("/api/v1/projects/demo/releases")).await;
    assert_status(&response, StatusCode::OK);
    body_json(response).await
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_init_records_one_session_against_its_release(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    session(
        &db,
        project_id,
        json!({
            "sid": "a", "init": true, "status": "ok",
            "started": hours_ago(1),
            "attrs": { "release": "1.4.2" }
        }),
    )
    .await;

    assert_eq!(totals(&db, "1.4.2").await, (1, 0, 0, 0));
    // The release row is minted by the session, exactly as an event would mint it.
    assert_eq!(count(&db, "releases").await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_session_updated_after_it_crashes_is_counted_once(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let started = hours_ago(1);

    // The SDK opens the session, then closes it as crashed. Two items, one session.
    session(
        &db,
        project_id,
        json!({
            "sid": "a", "init": true, "status": "ok",
            "started": started, "attrs": { "release": "1.4.2" }
        }),
    )
    .await;
    session(
        &db,
        project_id,
        json!({
            "sid": "a", "status": "crashed",
            "started": started, "attrs": { "release": "1.4.2" }
        }),
    )
    .await;

    // Counting the second item as a session too would report a 200% crash rate.
    assert_eq!(totals(&db, "1.4.2").await, (1, 0, 1, 0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_session_is_bucketed_on_its_start_not_on_the_update_that_closed_it(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    // Two hours apart, so the two updates would land in different hourly buckets if the closing
    // update's own timestamp were used — leaving a bucket with a crash and no session to divide by.
    let started = hours_ago(3);

    session(
        &db,
        project_id,
        json!({
            "sid": "a", "init": true, "status": "ok",
            "started": started.clone(), "timestamp": started.clone(),
            "attrs": { "release": "1.4.2" }
        }),
    )
    .await;
    session(
        &db,
        project_id,
        json!({
            "sid": "a", "status": "crashed",
            "started": started, "timestamp": hours_ago(1),
            "attrs": { "release": "1.4.2" }
        }),
    )
    .await;

    assert_eq!(
        count(&db, "session_counts").await,
        1,
        "both updates belong to the bucket the session started in"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_session_with_no_release_is_dropped(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    session(
        &db,
        project_id,
        json!({ "sid": "a", "init": true, "started": hours_ago(1) }),
    )
    .await;

    // Release health that cannot be attributed to a release answers no question, and a null key
    // would grow the rollup forever.
    assert_eq!(count(&db, "session_counts").await, 0);
    assert_eq!(count(&db, "releases").await, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_aggregate_batch_adds_every_disjoint_counter(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    send_sessions(
        &db,
        project_id,
        &[(
            json!({ "type": "sessions" }),
            json!({
                "attrs": { "release": "1.4.2" },
                "aggregates": [
                    { "started": hours_ago(1), "exited": 5, "errored": 2, "crashed": 1, "abnormal": 1 }
                ]
            }),
        )],
    )
    .await;

    // Total is the sum of the four; there is no separate total field in the wire format.
    assert_eq!(totals(&db, "1.4.2").await, (9, 2, 1, 1));
}

#[sqlx::test(migrations = "../../migrations")]
async fn repeated_buckets_in_one_batch_are_merged(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let started = hours_ago(1);

    send_sessions(
        &db,
        project_id,
        &[(
            json!({ "type": "sessions" }),
            json!({
                "attrs": { "release": "1.4.2" },
                "aggregates": [
                    { "started": started.clone(), "exited": 3 },
                    { "started": started, "exited": 2, "crashed": 1 }
                ]
            }),
        )],
    )
    .await;

    assert_eq!(totals(&db, "1.4.2").await, (6, 0, 1, 0));
    assert_eq!(count(&db, "session_counts").await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_crash_free_rate_is_withheld_until_there_are_enough_sessions(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    send_sessions(
        &db,
        project_id,
        &[(
            json!({ "type": "sessions" }),
            json!({
                "attrs": { "release": "0.1.0" },
                "aggregates": [{ "started": hours_ago(1), "exited": 2, "crashed": 1 }]
            }),
        )],
    )
    .await;

    let body = health(&db).await;
    let release = &body["releases"][0];

    assert_eq!(release["version"], "0.1.0");
    assert_eq!(release["sessions"], 3);
    // One crash in three sessions is 66.7% — a number that looks like a catastrophe and measures
    // nothing. Below the floor the rate is withheld rather than rendered.
    assert!(release["crash_free_rate"].is_null());
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_crash_free_rate_is_reported_once_the_sample_is_large_enough(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    send_sessions(
        &db,
        project_id,
        &[(
            json!({ "type": "sessions" }),
            json!({
                "attrs": { "release": "1.4.2" },
                "aggregates": [{ "started": hours_ago(1), "exited": 98, "crashed": 2 }]
            }),
        )],
    )
    .await;

    let body = health(&db).await;
    let release = &body["releases"][0];

    assert_eq!(release["sessions"], 100);
    assert_eq!(release["crashed"], 2);
    assert_eq!(release["crash_free_rate"], 0.98);
}

#[sqlx::test(migrations = "../../migrations")]
async fn releases_come_back_newest_first_with_a_continuous_series(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    for version in ["1.0.0", "1.1.0"] {
        session(
            &db,
            project_id,
            json!({
                "sid": version, "init": true, "started": hours_ago(1),
                "attrs": { "release": version }
            }),
        )
        .await;
    }

    let body = health(&db).await;
    let releases = body["releases"].as_array().expect("a list");

    // Newest first by first sighting, never by parsing the version string.
    assert_eq!(releases.len(), 2);
    assert_eq!(releases[0]["version"], "1.1.0");
    assert_eq!(releases[1]["version"], "1.0.0");

    // Zero-filled to the whole window, so a chart never has to reason about gaps.
    let series = releases[0]["series"].as_array().expect("a series");
    assert_eq!(series.len(), 24);
    assert_eq!(series.iter().filter_map(Value::as_i64).sum::<i64>(), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_project_nobody_reports_sessions_for_has_no_release_health(db: PgPool) {
    create_project(&db, "demo", PUBLIC_KEY).await;

    // Not an error: most SDKs never send sessions, and the dashboard keys off the empty list to
    // leave the panel off the page entirely.
    assert_eq!(health(&db).await["releases"].as_array().unwrap().len(), 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_malformed_session_does_not_discard_the_events_beside_it(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    send_sessions(
        &db,
        project_id,
        &[
            // A negative counter would subtract from the rollup, so the item is refused outright.
            (
                json!({ "type": "sessions" }),
                json!({ "attrs": { "release": "1.4.2" }, "aggregates": [{ "crashed": -5 }] }),
            ),
            (
                json!({ "type": "event" }),
                error_event(&"1".repeat(32), "ValueError", "boom"),
            ),
        ],
    )
    .await;

    assert_eq!(count(&db, "events").await, 1);
    assert_eq!(count(&db, "session_counts").await, 0);
}
