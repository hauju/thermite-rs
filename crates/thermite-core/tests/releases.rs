//! Release-aware resolution: "resolved until the next release" and what counts as a regression.

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

fn post(uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn json_of(db: &PgPool, request: Request<Body>, expected: StatusCode) -> Value {
    let response = send(state(db.clone()), request).await;
    assert_status(&response, expected);
    body_json(response).await
}

/// Ingests one error event, optionally stamped with a release.
async fn ingest(db: &PgPool, project_id: i64, event_id: &str, release: Option<&str>) {
    let mut event = error_event(event_id, "ValueError", "boom");
    if let Some(release) = release {
        event["release"] = json!(release);
    }
    let body = envelope(json!({}), &[(json!({ "type": "event" }), event)]);
    let response = send(
        state(db.clone()),
        envelope_request(project_id, PUBLIC_KEY, body),
    )
    .await;
    assert_status(&response, StatusCode::OK);
}

async fn the_issue(db: &PgPool) -> Value {
    // No status filter: the assertions need to see the issue whether it is resolved or not.
    let issues = json_of(db, get("/api/v1/projects/demo/issues"), StatusCode::OK).await;
    issues.as_array().unwrap()[0].clone()
}

async fn resolve_until_next_release(db: &PgPool, issue_id: i64) {
    let response = send(
        state(db.clone()),
        post(
            &format!("/api/v1/issues/{issue_id}/status"),
            json!({ "status": "resolved", "in_next_release": true }),
        ),
    )
    .await;
    assert_status(&response, StatusCode::OK);
}

async fn unacked_notifications(db: &PgPool) -> i64 {
    sqlx::query_scalar("select count(*) from notifications where acked_at is null")
        .fetch_one(db)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn releases_are_recorded_once_in_first_seen_order(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    ingest(&db, project_id, &"1".repeat(32), Some("2.0")).await;
    ingest(&db, project_id, &"2".repeat(32), Some("2.0")).await;
    // Lexically smaller, but first seen later — so it is the newer release.
    ingest(&db, project_id, &"3".repeat(32), Some("1.9")).await;

    let releases: Vec<(String,)> =
        sqlx::query_as("select version from releases where project_id = $1 order by id")
            .bind(project_id)
            .fetch_all(&db)
            .await
            .unwrap();

    assert_eq!(
        releases,
        vec![("2.0".to_string(),), ("1.9".to_string(),)],
        "one row per version, ordered by first sighting"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn traffic_from_an_already_seen_release_does_not_reopen(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    ingest(&db, project_id, &"1".repeat(32), Some("2.0")).await;

    let issue = the_issue(&db).await;
    let issue_id = issue["id"].as_i64().unwrap();
    resolve_until_next_release(&db, issue_id).await;
    let baseline = unacked_notifications(&db).await;

    // The broken deploy is still out there and still reporting. Not news.
    ingest(&db, project_id, &"2".repeat(32), Some("2.0")).await;

    let issue = the_issue(&db).await;
    assert_eq!(issue["status"], "resolved", "must stay resolved");
    assert_eq!(issue["times_seen"], 2, "but the event is still counted");
    assert_eq!(
        unacked_notifications(&db).await,
        baseline,
        "and no regression is queued"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_newer_release_reopens_as_a_regression(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    ingest(&db, project_id, &"1".repeat(32), Some("2.0")).await;

    let issue_id = the_issue(&db).await["id"].as_i64().unwrap();
    resolve_until_next_release(&db, issue_id).await;
    let baseline = unacked_notifications(&db).await;

    // The fix should have shipped in this release; the error surviving it is a regression.
    ingest(&db, project_id, &"2".repeat(32), Some("2.1")).await;

    let issue = the_issue(&db).await;
    assert_eq!(issue["status"], "unresolved");
    assert_eq!(unacked_notifications(&db).await, baseline + 1);

    // Reopening cleared the marker: a plain re-resolve now reopens on any recurrence again.
    let marker: Option<i64> =
        sqlx::query_scalar("select resolved_in_release_id from issues where id = $1")
            .bind(issue_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(marker, None);

    // The triage item hands an agent the regression range: the release the fix was verified
    // against (last known good) and the release that reopened it — a diffable pair.
    let pending = json_of(&db, get("/api/v1/triage/pending"), StatusCode::OK).await;
    let item = pending
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == json!("regression"))
        .expect("a regression should be queued");
    assert_eq!(item["regressed_from_release"], json!("2.0"));
    assert_eq!(item["release"], json!("2.1"));
    assert_eq!(item["first_seen_release"], json!("2.0"));

    // The issue detail carries the same range for whoever opens it later.
    let detail = json_of(
        &db,
        get(&format!("/api/v1/issues/{issue_id}")),
        StatusCode::OK,
    )
    .await;
    assert_eq!(detail["regressed_from_release"], json!("2.0"));
    assert_eq!(detail["first_seen_release"], json!("2.0"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_plain_resolve_regression_has_no_known_good_release(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    ingest(&db, project_id, &"1".repeat(32), Some("2.0")).await;

    let issue_id = the_issue(&db).await["id"].as_i64().unwrap();
    let response = send(
        state(db.clone()),
        post(
            &format!("/api/v1/issues/{issue_id}/status"),
            json!({ "status": "resolved" }),
        ),
    )
    .await;
    assert_status(&response, StatusCode::OK);

    ingest(&db, project_id, &"2".repeat(32), Some("2.0")).await;

    // A plain resolve carries no release anchor, so the regression honestly reports no known
    // good rather than a stale one from an earlier cycle.
    let pending = json_of(&db, get("/api/v1/triage/pending"), StatusCode::OK).await;
    let regression = pending
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == json!("regression"))
        .expect("a regression should be queued");
    assert_eq!(regression["regressed_from_release"], json!(null));
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_event_without_a_release_cannot_be_proven_old_so_it_reopens(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    ingest(&db, project_id, &"1".repeat(32), Some("2.0")).await;

    let issue_id = the_issue(&db).await["id"].as_i64().unwrap();
    resolve_until_next_release(&db, issue_id).await;

    ingest(&db, project_id, &"2".repeat(32), None).await;

    assert_eq!(the_issue(&db).await["status"], "unresolved");
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_older_release_seen_before_the_resolve_does_not_reopen(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    // Both releases known before the resolve; 2.0 is the newest at resolve time.
    ingest(&db, project_id, &"1".repeat(32), Some("1.0")).await;
    ingest(&db, project_id, &"2".repeat(32), Some("2.0")).await;

    let issue_id = the_issue(&db).await["id"].as_i64().unwrap();
    resolve_until_next_release(&db, issue_id).await;

    // A laggard still on 1.0 reports again. Older than the anchor — stays resolved.
    ingest(&db, project_id, &"3".repeat(32), Some("1.0")).await;

    assert_eq!(the_issue(&db).await["status"], "resolved");
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_plain_resolve_still_reopens_on_any_recurrence(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    ingest(&db, project_id, &"1".repeat(32), Some("2.0")).await;

    let issue_id = the_issue(&db).await["id"].as_i64().unwrap();
    let response = send(
        state(db.clone()),
        post(
            &format!("/api/v1/issues/{issue_id}/status"),
            json!({ "status": "resolved" }),
        ),
    )
    .await;
    assert_status(&response, StatusCode::OK);

    // Same release, but a plain resolve carries no release semantics.
    ingest(&db, project_id, &"2".repeat(32), Some("2.0")).await;

    assert_eq!(the_issue(&db).await["status"], "unresolved");
}

#[sqlx::test(migrations = "../../migrations")]
async fn resolving_until_next_release_needs_a_release_to_anchor_on(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    ingest(&db, project_id, &"1".repeat(32), None).await;

    let issue_id = the_issue(&db).await["id"].as_i64().unwrap();
    let response = send(
        state(db.clone()),
        post(
            &format!("/api/v1/issues/{issue_id}/status"),
            json!({ "status": "resolved", "in_next_release": true }),
        ),
    )
    .await;
    // Silently degrading to a plain resolve would betray what the caller asked for.
    assert_status(&response, StatusCode::BAD_REQUEST);

    // And the flag is meaningless on anything but resolved.
    let response = send(
        state(db.clone()),
        post(
            &format!("/api/v1/issues/{issue_id}/status"),
            json!({ "status": "ignored", "in_next_release": true }),
        ),
    )
    .await;
    assert_status(&response, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_release_table_is_capped_but_known_versions_still_work(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    // A project already at the 10k release cap.
    sqlx::query(
        "insert into releases (project_id, version)
         select $1, 'seed-' || n from generate_series(1, 10000) as n",
    )
    .bind(project_id)
    .execute(&db)
    .await
    .unwrap();

    // A never-seen version bounces off the cap: the event is stored, no row is minted.
    ingest(&db, project_id, &"1".repeat(32), Some("novel-version")).await;
    let (releases,): (i64,) = sqlx::query_as("select count(*) from releases")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(releases, 10_000, "the cap must hold");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from events")
            .fetch_one(&db)
            .await
            .unwrap(),
        1,
        "the event itself is still accepted"
    );

    // A known version resolves its id as before.
    ingest(&db, project_id, &"2".repeat(32), Some("seed-1")).await;
    let (known,): (i64,) = sqlx::query_as("select count(*) from releases")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(known, 10_000);
}
