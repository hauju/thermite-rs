//! The issue_tags rollup: what ingest records, what the filters and endpoints read back.

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

async fn json_of(db: &PgPool, request: Request<Body>, expected: StatusCode) -> Value {
    let response = send(state(db.clone()), request).await;
    assert_status(&response, expected);
    body_json(response).await
}

/// An event with an environment and SDK tags, distinguished by exception value.
fn tagged_event(event_id: &str, value: &str, environment: &str, browser: &str) -> Value {
    let mut event = error_event(event_id, "ValueError", value);
    event["environment"] = json!(environment);
    event["tags"] = json!({ "browser": browser });
    event
}

async fn ingest(db: &PgPool, project_id: i64, event: Value) {
    let body = envelope(json!({}), &[(json!({ "type": "event" }), event)]);
    let response = send(
        state(db.clone()),
        envelope_request(project_id, PUBLIC_KEY, body),
    )
    .await;
    assert_status(&response, StatusCode::OK);
}

#[sqlx::test(migrations = "../../migrations")]
async fn ingest_records_the_tag_distribution_on_the_issue(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    // Two events, same issue, one tag value shared and one differing.
    ingest(
        &db,
        project_id,
        tagged_event(&"1".repeat(32), "boom", "production", "firefox"),
    )
    .await;
    ingest(
        &db,
        project_id,
        tagged_event(&"2".repeat(32), "boom", "production", "chrome"),
    )
    .await;

    let issues = json_of(&db, get("/api/v1/projects/demo/issues"), StatusCode::OK).await;
    let issue_id = issues[0]["id"].as_i64().unwrap();

    let detail = json_of(
        &db,
        get(&format!("/api/v1/issues/{issue_id}")),
        StatusCode::OK,
    )
    .await;

    let tags = detail["tags"].as_array().unwrap();
    let find = |key: &str, value: &str| {
        tags.iter()
            .find(|t| t["key"] == key && t["value"] == value)
            .unwrap_or_else(|| panic!("missing {key}:{value} in {tags:?}"))
    };

    // The promoted field was synthesized into a tag and counted across both events.
    assert_eq!(find("environment", "production")["times_seen"], 2);
    assert_eq!(find("browser", "firefox")["times_seen"], 1);
    assert_eq!(find("browser", "chrome")["times_seen"], 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_sdk_retry_does_not_inflate_tag_counts(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let event = tagged_event(&"1".repeat(32), "boom", "production", "firefox");
    ingest(&db, project_id, event.clone()).await;
    ingest(&db, project_id, event).await;

    let count: i64 = sqlx::query_scalar(
        "select times_seen from issue_tags where key = 'environment' and value = 'production'",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn issues_filter_by_environment_and_by_tag(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    ingest(
        &db,
        project_id,
        tagged_event(&"1".repeat(32), "prod boom", "production", "firefox"),
    )
    .await;
    ingest(
        &db,
        project_id,
        tagged_event(&"2".repeat(32), "staging boom", "staging", "chrome"),
    )
    .await;

    let all = json_of(&db, get("/api/v1/projects/demo/issues"), StatusCode::OK).await;
    assert_eq!(all.as_array().unwrap().len(), 2);

    let production = json_of(
        &db,
        get("/api/v1/projects/demo/issues?environment=production"),
        StatusCode::OK,
    )
    .await;
    let production = production.as_array().unwrap();
    assert_eq!(production.len(), 1);
    assert_eq!(production[0]["title"], "ValueError: prod boom");

    let chrome = json_of(
        &db,
        get("/api/v1/projects/demo/issues?tag=browser:chrome"),
        StatusCode::OK,
    )
    .await;
    let chrome = chrome.as_array().unwrap();
    assert_eq!(chrome.len(), 1);
    assert_eq!(chrome[0]["title"], "ValueError: staging boom");

    // Both filters together must intersect, not shadow one another.
    let none = json_of(
        &db,
        get("/api/v1/projects/demo/issues?environment=production&tag=browser:chrome"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(none.as_array().unwrap().len(), 0);

    // A filter without a colon cannot mean anything; refuse it rather than matching nothing.
    let response = send(
        state(db.clone()),
        get("/api/v1/projects/demo/issues?tag=malformed"),
    )
    .await;
    assert_status(&response, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrations = "../../migrations")]
async fn tag_values_are_listed_per_project_with_summed_counts(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    // Two *different* issues both reporting from production, one from staging.
    ingest(
        &db,
        project_id,
        tagged_event(&"1".repeat(32), "boom", "production", "firefox"),
    )
    .await;
    ingest(
        &db,
        project_id,
        tagged_event(&"2".repeat(32), "other boom", "production", "firefox"),
    )
    .await;
    ingest(
        &db,
        project_id,
        tagged_event(&"3".repeat(32), "boom", "staging", "firefox"),
    )
    .await;

    let values = json_of(
        &db,
        get("/api/v1/projects/demo/tags/environment"),
        StatusCode::OK,
    )
    .await;
    let values = values.as_array().unwrap();

    // Most events first, counts summed across issues.
    assert_eq!(values[0]["value"], "production");
    assert_eq!(values[0]["times_seen"], 2);
    assert_eq!(values[1]["value"], "staging");
    assert_eq!(values[1]["times_seen"], 1);

    let response = send(
        state(db.clone()),
        get("/api/v1/projects/nope/tags/environment"),
    )
    .await;
    assert_status(&response, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn users_affected_counts_distinct_users_not_events(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let with_user = |event_id: &str, user: Value| {
        let mut event = error_event(event_id, "ValueError", "boom");
        event["user"] = user;
        event
    };

    // Three events, two distinct users — the same user twice must count once.
    ingest(
        &db,
        project_id,
        with_user(&"1".repeat(32), json!({ "id": 1 })),
    )
    .await;
    ingest(
        &db,
        project_id,
        with_user(&"2".repeat(32), json!({ "id": 1 })),
    )
    .await;
    ingest(
        &db,
        project_id,
        with_user(&"3".repeat(32), json!({ "id": 2 })),
    )
    .await;
    // An event with no user context affects the event count only.
    ingest(
        &db,
        project_id,
        error_event(&"4".repeat(32), "ValueError", "boom"),
    )
    .await;

    let issues = json_of(&db, get("/api/v1/projects/demo/issues"), StatusCode::OK).await;
    assert_eq!(issues[0]["times_seen"], 4);
    assert_eq!(issues[0]["users_affected"], 2);

    let issue_id = issues[0]["id"].as_i64().unwrap();
    let detail = json_of(
        &db,
        get(&format!("/api/v1/issues/{issue_id}")),
        StatusCode::OK,
    )
    .await;
    assert_eq!(detail["users_affected"], 2);

    // The distribution names the affected users, so "which users" needs no second query.
    assert!(
        detail["tags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["key"] == "user" && t["value"] == "id:1" && t["times_seen"] == 2)
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_tag_distribution_outlives_the_events_it_describes(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    ingest(
        &db,
        project_id,
        tagged_event(&"1".repeat(32), "boom", "production", "firefox"),
    )
    .await;

    // Retention evicting every stored event must not erase the distribution — it is a rollup,
    // maintained at ingest for exactly this moment.
    sqlx::query("delete from events")
        .execute(&db)
        .await
        .unwrap();

    let issues = json_of(&db, get("/api/v1/projects/demo/issues"), StatusCode::OK).await;
    let issue_id = issues[0]["id"].as_i64().unwrap();
    let detail = json_of(
        &db,
        get(&format!("/api/v1/issues/{issue_id}")),
        StatusCode::OK,
    )
    .await;

    assert!(detail["latest_event"].is_null());
    assert!(
        detail["tags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["key"] == "environment" && t["value"] == "production")
    );

    // And the environment filter still finds the issue.
    let filtered = json_of(
        &db,
        get("/api/v1/projects/demo/issues?environment=production"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(filtered.as_array().unwrap().len(), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn distinct_users_advance_the_counter_and_repeats_do_not(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    for (event_id, user_id) in [
        (&"1".repeat(32), "alice"),
        (&"2".repeat(32), "bob"),
        (&"3".repeat(32), "alice"), // returning user, same issue
    ] {
        let mut event = error_event(event_id, "ValueError", "boom");
        event["user"] = json!({ "id": user_id });
        ingest(&db, project_id, event).await;
    }

    let issues = json_of(&db, get("/api/v1/projects/demo/issues"), StatusCode::OK).await;
    assert_eq!(issues[0]["users_affected"], json!(2));

    // The API value comes from the issues counter, not count(*) over the rollup.
    let (counter,): (i64,) = sqlx::query_as("select users_affected from issues")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(counter, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_ip_only_user_counts_nobody_and_leaves_no_tag_row(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let mut event = error_event(&"1".repeat(32), "ValueError", "boom");
    event["user"] = json!({ "ip_address": "203.0.113.4" });
    ingest(&db, project_id, event).await;

    let issues = json_of(&db, get("/api/v1/projects/demo/issues"), StatusCode::OK).await;
    assert_eq!(issues[0]["users_affected"], json!(0));

    // The IP must not become a permanent rollup row retention never erases.
    let (user_rows,): (i64,) = sqlx::query_as("select count(*) from issue_tags where key = 'user'")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(user_rows, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_key_at_its_cardinality_cap_accepts_no_new_values_but_keeps_counting_known_ones(
    db: PgPool,
) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    // One event opens the issue and plants browser=known.
    ingest(
        &db,
        project_id,
        tagged_event(&"1".repeat(32), "boom", "production", "known"),
    )
    .await;
    let (issue_id,): (i64,) = sqlx::query_as("select id from issues")
        .fetch_one(&db)
        .await
        .unwrap();

    // Fill the browser key to its cap of 1000 distinct values (999 seeded + `known`).
    sqlx::query(
        "insert into issue_tags (project_id, issue_id, key, value, times_seen, last_seen)
         select $1, $2, 'browser', 'seed-' || n, 1, now()
           from generate_series(1, 999) as n",
    )
    .bind(project_id)
    .bind(issue_id)
    .execute(&db)
    .await
    .unwrap();

    // A new value bounces off the cap; a known one still counts.
    ingest(
        &db,
        project_id,
        tagged_event(&"2".repeat(32), "boom", "production", "novel"),
    )
    .await;
    ingest(
        &db,
        project_id,
        tagged_event(&"3".repeat(32), "boom", "production", "known"),
    )
    .await;

    let rows: Vec<(String, i64)> = sqlx::query_as(
        "select value, times_seen from issue_tags
          where key = 'browser' and value in ('known', 'novel')",
    )
    .fetch_all(&db)
    .await
    .unwrap();

    assert_eq!(rows, vec![("known".to_string(), 2)]);

    let (distinct,): (i64,) =
        sqlx::query_as("select count(*) from issue_tags where key = 'browser'")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(distinct, 1000, "the cap must hold");

    // Uncapped keys on the same events are unaffected.
    let (env_seen,): (i64,) = sqlx::query_as(
        "select times_seen from issue_tags where key = 'environment' and value = 'production'",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(env_seen, 3);
}
