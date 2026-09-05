//! Grading the fixes agents propose, against what production did next.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;

const REPO: &str = "https://github.com/hauju/thermite-rs";

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

fn json_request(method: &str, uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
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

/// A project that has declared its repository, which is what lets a fix link be accepted at all.
async fn setup(db: &PgPool) -> i64 {
    let project_id = create_project(db, "demo", PUBLIC_KEY).await;
    let response = send(
        state(db.clone()),
        json_request(
            "PUT",
            "/api/v1/projects/demo/repo",
            json!({ "repo_url": REPO }),
        ),
    )
    .await;
    assert_status(&response, StatusCode::OK);
    project_id
}

/// One error, tagged with the release it happened in — which is what ties an issue to a release.
async fn ingest_at(db: &PgPool, project_id: i64, event_id: &str, ty: &str, release: &str) {
    let mut event = error_event(event_id, ty, "boom");
    event["release"] = json!(release);
    let body = envelope(json!({}), &[(json!({ "type": "event" }), event)]);
    let response = send(
        state(db.clone()),
        envelope_request(project_id, PUBLIC_KEY, body),
    )
    .await;
    assert_status(&response, StatusCode::OK);
}

async fn issue_id_of(db: &PgPool, exception_type: &str) -> i64 {
    sqlx::query_scalar("select id from issues where exception_type = $1")
        .bind(exception_type)
        .fetch_one(db)
        .await
        .unwrap()
}

async fn propose_fix(db: &PgPool, issue_id: i64, source: &str) {
    let response = send(
        state(db.clone()),
        json_request(
            "POST",
            &format!("/api/v1/issues/{issue_id}/analyses"),
            json!({
                "source": source,
                "summary": "Bound the pool wait",
                "fix_url": format!("{REPO}/pull/12"),
            }),
        ),
    )
    .await;
    assert_status(&response, StatusCode::CREATED);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_fix_nothing_has_shipped_since_is_not_yet_a_win(db: PgPool) {
    let project_id = setup(&db).await;
    ingest_at(&db, project_id, &"1".repeat(32), "ValueError", "1.0.0").await;
    propose_fix(&db, issue_id_of(&db, "ValueError").await, "claude-code").await;

    let record = json_of(&db, get("/api/v1/fixes"), StatusCode::OK).await;
    assert_eq!(record["fixes"][0]["verdict"], json!("pending"));
    assert_eq!(record["fixes"][0]["releases_since"], json!(0));
    // And it does not count for or against the agent while it is unjudged.
    assert_eq!(record["by_source"][0]["pending"], json!(1));
    assert_eq!(record["by_source"][0]["hold_rate"], Value::Null);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_fix_the_issue_stayed_quiet_across_held(db: PgPool) {
    let project_id = setup(&db).await;
    ingest_at(&db, project_id, &"1".repeat(32), "ValueError", "1.0.0").await;
    propose_fix(&db, issue_id_of(&db, "ValueError").await, "claude-code").await;

    // A later release ships and this issue is not in it — the other error is what proves the
    // release exists, rather than the fixed issue reappearing.
    ingest_at(&db, project_id, &"2".repeat(32), "KeyError", "1.1.0").await;

    let record = json_of(&db, get("/api/v1/fixes"), StatusCode::OK).await;
    assert_eq!(record["fixes"][0]["verdict"], json!("held"));
    assert_eq!(record["by_source"][0]["held"], json!(1));
    assert_eq!(record["by_source"][0]["hold_rate"], json!(1.0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_issue_that_comes_back_in_a_newer_release_sinks_the_fix(db: PgPool) {
    let project_id = setup(&db).await;
    ingest_at(&db, project_id, &"1".repeat(32), "ValueError", "1.0.0").await;
    propose_fix(&db, issue_id_of(&db, "ValueError").await, "claude-code").await;

    // The same bug, in a release that first appeared after the fix was proposed.
    ingest_at(&db, project_id, &"2".repeat(32), "ValueError", "1.1.0").await;

    let record = json_of(&db, get("/api/v1/fixes"), StatusCode::OK).await;
    assert_eq!(record["fixes"][0]["verdict"], json!("regressed"));
    assert_eq!(record["fixes"][0]["regressed_in"], json!("1.1.0"));
    assert_eq!(record["by_source"][0]["hold_rate"], json!(0.0));
}

/// The verdict has to reach the page a human reads, not just the agent-facing tally.
#[sqlx::test(migrations = "../../migrations")]
async fn the_issue_page_shows_what_became_of_the_fix(db: PgPool) {
    let project_id = setup(&db).await;
    ingest_at(&db, project_id, &"1".repeat(32), "ValueError", "1.0.0").await;
    let issue_id = issue_id_of(&db, "ValueError").await;
    propose_fix(&db, issue_id, "claude-code").await;
    ingest_at(&db, project_id, &"2".repeat(32), "ValueError", "1.1.0").await;

    let issue = json_of(
        &db,
        get(&format!("/api/v1/issues/{issue_id}")),
        StatusCode::OK,
    )
    .await;
    assert_eq!(issue["analyses"][0]["fix_verdict"], json!("regressed"));
    assert_eq!(issue["analyses"][0]["regressed_in"], json!("1.1.0"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_diagnosis_with_no_fix_is_not_graded(db: PgPool) {
    let project_id = setup(&db).await;
    ingest_at(&db, project_id, &"1".repeat(32), "ValueError", "1.0.0").await;
    let issue_id = issue_id_of(&db, "ValueError").await;

    let response = send(
        state(db.clone()),
        json_request(
            "POST",
            &format!("/api/v1/issues/{issue_id}/analyses"),
            json!({ "source": "claude-code", "summary": "Still looking" }),
        ),
    )
    .await;
    assert_status(&response, StatusCode::CREATED);

    let issue = json_of(
        &db,
        get(&format!("/api/v1/issues/{issue_id}")),
        StatusCode::OK,
    )
    .await;
    assert_eq!(issue["analyses"][0]["fix_verdict"], Value::Null);

    // Nothing was proposed, so there is nothing to score.
    let record = json_of(&db, get("/api/v1/fixes"), StatusCode::OK).await;
    assert_eq!(record["fixes"].as_array().unwrap().len(), 0);
    assert_eq!(record["by_source"].as_array().unwrap().len(), 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_record_can_be_read_for_one_project(db: PgPool) {
    let project_id = setup(&db).await;
    ingest_at(&db, project_id, &"1".repeat(32), "ValueError", "1.0.0").await;
    propose_fix(&db, issue_id_of(&db, "ValueError").await, "claude-code").await;

    let record = json_of(&db, get("/api/v1/projects/demo/fixes"), StatusCode::OK).await;
    assert_eq!(record["fixes"].as_array().unwrap().len(), 1);

    let response = send(state(db.clone()), get("/api/v1/projects/nope/fixes")).await;
    assert_status(&response, StatusCode::NOT_FOUND);
}
