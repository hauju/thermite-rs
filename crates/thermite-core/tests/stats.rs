//! Dashboard numbers: the rollup, the time series, and the sparklines.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;

async fn setup(db: &PgPool) -> i64 {
    create_project(db, "demo", PUBLIC_KEY).await
}

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

async fn ingest(db: &PgPool, project_id: i64, event: Value) {
    let body = envelope(json!({}), &[(json!({ "type": "event" }), event)]);
    let response = send(
        state(db.clone()),
        envelope_request(project_id, PUBLIC_KEY, body),
    )
    .await;
    assert_status(&response, StatusCode::OK);
}

/// An event that happened `hours_ago`, so a window can be exercised without waiting.
async fn ingest_at(db: &PgPool, project_id: i64, event_id: &str, ty: &str, hours_ago: i64) {
    let mut event = error_event(event_id, ty, "boom");
    let at = chrono::Utc::now() - chrono::Duration::hours(hours_ago);
    event["timestamp"] = json!(at.to_rfc3339());
    ingest(db, project_id, event).await;
}

async fn rollup_total(db: &PgPool) -> i64 {
    sqlx::query_scalar("select coalesce(sum(count), 0)::bigint from event_counts")
        .fetch_one(db)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn every_stored_event_lands_in_the_rollup(db: PgPool) {
    let project_id = setup(&db).await;

    for i in 0..3 {
        ingest_at(
            &db,
            project_id,
            &format!("{i}{}", "1".repeat(31)),
            "IOError",
            0,
        )
        .await;
    }

    assert_eq!(rollup_total(&db).await, 3);
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_sdk_retry_does_not_inflate_the_graph(db: PgPool) {
    let project_id = setup(&db).await;
    let event = error_event(&"1".repeat(32), "IOError", "boom");

    for _ in 0..4 {
        ingest(&db, project_id, event.clone()).await;
    }

    assert_eq!(
        rollup_total(&db).await,
        1,
        "a redelivered event must not be counted again"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn counts_are_bucketed_by_hour_and_split_by_level(db: PgPool) {
    let project_id = setup(&db).await;

    // Two errors in the current hour, one warning three hours back.
    ingest_at(&db, project_id, &"1".repeat(32), "IOError", 0).await;
    ingest_at(&db, project_id, &"2".repeat(32), "IOError", 0).await;

    let mut warning = error_event(&"3".repeat(32), "SlowQuery", "boom");
    warning["level"] = json!("warning");
    warning["timestamp"] = json!((chrono::Utc::now() - chrono::Duration::hours(3)).to_rfc3339());
    ingest(&db, project_id, warning).await;

    let rows: Vec<(String, i64)> = sqlx::query_as(
        "select level, sum(count)::bigint from event_counts group by level order by level",
    )
    .fetch_all(&db)
    .await
    .unwrap();

    assert_eq!(rows, vec![("error".into(), 2), ("warning".into(), 1)]);

    let buckets: i64 = sqlx::query_scalar("select count(distinct bucket) from event_counts")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(
        buckets, 2,
        "events three hours apart belong to different buckets"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_series_is_continuous_and_zero_filled(db: PgPool) {
    let project_id = setup(&db).await;
    ingest_at(&db, project_id, &"1".repeat(32), "IOError", 0).await;

    let stats = json_of(&db, get("/api/v1/projects/demo/stats"), StatusCode::OK).await;
    let series = stats["series"].as_array().unwrap();

    assert_eq!(stats["window"], json!("24h"));
    assert_eq!(stats["resolution"], json!("1h"));
    // A chart should never have to reason about gaps.
    assert_eq!(series.len(), 24);
    assert_eq!(
        series.iter().filter(|b| b["count"] == json!(0)).count(),
        23,
        "only the current bucket should have events"
    );
    assert_eq!(series.last().unwrap()["count"], json!(1));
    assert_eq!(series.last().unwrap()["levels"]["error"], json!(1));
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_series_places_events_in_the_right_bucket(db: PgPool) {
    let project_id = setup(&db).await;

    ingest_at(&db, project_id, &"1".repeat(32), "IOError", 0).await;
    ingest_at(&db, project_id, &"2".repeat(32), "IOError", 5).await;
    ingest_at(&db, project_id, &"3".repeat(32), "IOError", 5).await;

    let stats = json_of(&db, get("/api/v1/projects/demo/stats"), StatusCode::OK).await;
    let series = stats["series"].as_array().unwrap();

    // Oldest first, so the current hour is last and five hours back is six from the end.
    assert_eq!(series[23]["count"], json!(1));
    assert_eq!(series[18]["count"], json!(2));
    assert_eq!(stats["totals"]["events"], json!(3));
}

#[sqlx::test(migrations = "../../migrations")]
async fn events_older_than_the_window_are_excluded(db: PgPool) {
    let project_id = setup(&db).await;

    ingest_at(&db, project_id, &"1".repeat(32), "IOError", 0).await;
    ingest_at(&db, project_id, &"2".repeat(32), "OldError", 48).await;

    let day = json_of(
        &db,
        get("/api/v1/projects/demo/stats?window=24h"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(day["totals"]["events"], json!(1));

    // The wider window sees both, and switches to daily buckets.
    let month = json_of(
        &db,
        get("/api/v1/projects/demo/stats?window=30d"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(month["totals"]["events"], json!(2));
    assert_eq!(month["resolution"], json!("1d"));
    assert_eq!(month["series"].as_array().unwrap().len(), 30);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_week_window_uses_hourly_buckets(db: PgPool) {
    let project_id = setup(&db).await;
    ingest_at(&db, project_id, &"1".repeat(32), "IOError", 0).await;

    let week = json_of(
        &db,
        get("/api/v1/projects/demo/stats?window=7d"),
        StatusCode::OK,
    )
    .await;

    assert_eq!(week["resolution"], json!("1h"));
    assert_eq!(week["series"].as_array().unwrap().len(), 24 * 7);
}

#[sqlx::test(migrations = "../../migrations")]
async fn totals_report_the_current_state_of_the_project(db: PgPool) {
    let project_id = setup(&db).await;

    ingest_at(&db, project_id, &"1".repeat(32), "IOError", 0).await;
    ingest_at(&db, project_id, &"2".repeat(32), "KeyError", 0).await;
    ingest_at(&db, project_id, &"3".repeat(32), "TypeError", 0).await;

    // One resolved, then it comes back.
    sqlx::query("update issues set status = 'resolved' where exception_type = 'TypeError'")
        .execute(&db)
        .await
        .unwrap();
    ingest_at(&db, project_id, &"4".repeat(32), "TypeError", 0).await;

    // One new but already dealt with — it must not count as new.
    ingest_at(&db, project_id, &"5".repeat(32), "NameError", 0).await;
    sqlx::query("update issues set status = 'resolved' where exception_type = 'NameError'")
        .execute(&db)
        .await
        .unwrap();

    let stats = json_of(&db, get("/api/v1/projects/demo/stats"), StatusCode::OK).await;
    let totals = &stats["totals"];

    assert_eq!(totals["events"], json!(5));
    assert_eq!(totals["unresolved_issues"], json!(3));
    assert_eq!(totals["new_issues"], json!(3));
    assert_eq!(totals["regressions"], json!(1));
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_headline_total_always_matches_the_chart(db: PgPool) {
    let project_id = setup(&db).await;
    for (i, hours_ago) in [0i64, 2, 2, 7, 19].iter().enumerate() {
        ingest_at(
            &db,
            project_id,
            &format!("{i}{}", "1".repeat(31)),
            "IOError",
            *hours_ago,
        )
        .await;
    }

    let stats = json_of(&db, get("/api/v1/projects/demo/stats"), StatusCode::OK).await;
    let charted: i64 = stats["series"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["count"].as_i64().unwrap())
        .sum();

    assert_eq!(stats["totals"]["events"].as_i64().unwrap(), charted);
    assert_eq!(charted, 5);
}

#[sqlx::test(migrations = "../../migrations")]
async fn stats_reject_an_unknown_window_and_an_unknown_project(db: PgPool) {
    setup(&db).await;

    let response = send(
        state(db.clone()),
        get("/api/v1/projects/demo/stats?window=1h"),
    )
    .await;
    assert_status(&response, StatusCode::BAD_REQUEST);

    let response = send(state(db.clone()), get("/api/v1/projects/nope/stats")).await;
    assert_status(&response, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_project_with_no_events_still_returns_a_full_empty_chart(db: PgPool) {
    setup(&db).await;

    let stats = json_of(&db, get("/api/v1/projects/demo/stats"), StatusCode::OK).await;

    assert_eq!(stats["series"].as_array().unwrap().len(), 24);
    assert_eq!(stats["totals"]["events"], json!(0));
    assert_eq!(stats["totals"]["unresolved_issues"], json!(0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn every_issue_row_carries_a_sparkline(db: PgPool) {
    let project_id = setup(&db).await;

    ingest_at(&db, project_id, &"1".repeat(32), "IOError", 0).await;
    ingest_at(&db, project_id, &"2".repeat(32), "IOError", 3).await;
    ingest_at(&db, project_id, &"3".repeat(32), "KeyError", 0).await;

    let issues = json_of(&db, get("/api/v1/projects/demo/issues"), StatusCode::OK).await;
    let rows = issues.as_array().unwrap();
    assert_eq!(rows.len(), 2);

    for row in rows {
        let counts = row["counts"].as_array().unwrap();
        assert_eq!(counts.len(), 24, "sparkline must cover the whole window");
    }

    let io_error = rows
        .iter()
        .find(|r| r["exception_type"] == json!("IOError"))
        .unwrap();
    let counts: Vec<i64> = io_error["counts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();

    assert_eq!(counts.iter().sum::<i64>(), 2);
    assert_eq!(counts[23], 1, "one event in the current hour");
    assert_eq!(counts[20], 1, "one event three hours back");
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_issue_with_no_recent_events_gets_a_flat_sparkline(db: PgPool) {
    let project_id = setup(&db).await;
    ingest_at(&db, project_id, &"1".repeat(32), "OldError", 48).await;

    let issues = json_of(&db, get("/api/v1/projects/demo/issues"), StatusCode::OK).await;
    let counts = issues[0]["counts"].as_array().unwrap();

    assert_eq!(counts.len(), 24);
    assert!(counts.iter().all(|c| c == &json!(0)));
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_project_list_event_count_comes_from_the_rollup(db: PgPool) {
    let project_id = setup(&db).await;

    ingest_at(&db, project_id, &"1".repeat(32), "IOError", 0).await;
    ingest_at(&db, project_id, &"2".repeat(32), "IOError", 5).await;
    // Outside the 24h window.
    ingest_at(&db, project_id, &"3".repeat(32), "IOError", 40).await;

    let projects = json_of(&db, get("/api/v1/projects"), StatusCode::OK).await;

    assert_eq!(projects[0]["events_last_24h"], json!(2));
    assert_eq!(projects[0]["unresolved_issues"], json!(1));
}

#[sqlx::test(migrations = "../../migrations")]
async fn counts_are_scoped_to_their_project(db: PgPool) {
    let project_id = setup(&db).await;
    let other_id = create_project(&db, "other", "cccccccccccccccccccccccccccccccc").await;

    ingest_at(&db, project_id, &"1".repeat(32), "IOError", 0).await;

    // A current timestamp, not `error_event`'s fixed one, so the event stays inside the stats
    // window as the calendar moves on.
    let mut event = error_event(&"2".repeat(32), "OtherError", "elsewhere");
    event["timestamp"] = json!(chrono::Utc::now().to_rfc3339());
    let body = envelope(json!({}), &[(json!({ "type": "event" }), event)]);
    assert_status(
        &send(
            state(db.clone()),
            envelope_request(other_id, "cccccccccccccccccccccccccccccccc", body),
        )
        .await,
        StatusCode::OK,
    );

    let demo = json_of(&db, get("/api/v1/projects/demo/stats"), StatusCode::OK).await;
    assert_eq!(demo["totals"]["events"], json!(1));

    let other = json_of(&db, get("/api/v1/projects/other/stats"), StatusCode::OK).await;
    assert_eq!(other["totals"]["events"], json!(1));
}

/// Dropped events surface in the stats response, so quota losses are visible on the dashboard.
#[sqlx::test(migrations = "../../migrations")]
async fn dropped_events_appear_in_the_stats(db: PgPool) {
    let project_id = setup(&db).await;

    let mut config = common::config();
    config.rate_limit_per_minute = Some(1);
    let limited = thermite_core::state::ThermiteState::new(db.clone(), config);

    // One event fits the quota, the second is dropped.
    for id in ["1", "2"] {
        let body = envelope(
            json!({}),
            &[(
                json!({ "type": "event" }),
                error_event(&id.repeat(32), "ValueError", "boom"),
            )],
        );
        send(
            limited.clone(),
            envelope_request(project_id, PUBLIC_KEY, body),
        )
        .await;
    }

    let stats = json_of(&db, get("/api/v1/projects/demo/stats"), StatusCode::OK).await;

    assert_eq!(stats["totals"]["events"], 1);
    assert_eq!(stats["totals"]["dropped"], 1);
    assert_eq!(stats["totals"]["dropped_by_reason"]["over_quota"], 1);

    // The drop lands in the series too (bucketed on arrival, so in the newest bucket).
    let series = stats["series"].as_array().unwrap();
    let dropped_total: i64 = series.iter().map(|b| b["dropped"].as_i64().unwrap()).sum();
    assert_eq!(dropped_total, 1);
}
