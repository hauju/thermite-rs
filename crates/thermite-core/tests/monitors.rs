//! Cron monitoring: check-ins through the real ingest path, and what the sweeper does when a
//! scheduled job goes quiet.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use thermite_core::monitors;

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

/// Sends one `check_in` envelope item the way an SDK does.
async fn check_in(db: &PgPool, project_id: i64, payload: Value) {
    let body = envelope(json!({}), &[(json!({ "type": "check_in" }), payload)]);
    let response = send(
        state(db.clone()),
        envelope_request(project_id, PUBLIC_KEY, body),
    )
    .await;
    assert_status(&response, StatusCode::OK);
}

/// A monitor that was due `minutes` ago, past its margin — as if the clock had moved on.
async fn make_overdue(db: &PgPool, minutes: i32) {
    sqlx::query(
        "update monitors
            set next_due_at = now() - make_interval(mins => $1),
                checkin_margin_minutes = 0",
    )
    .bind(minutes)
    .execute(db)
    .await
    .unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_check_in_creates_its_monitor_and_records_the_run(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    check_in(
        &db,
        project_id,
        json!({
            "check_in_id": "11111111-1111-1111-1111-111111111111",
            "monitor_slug": "nightly-backup",
            "status": "ok",
            "duration": 12.5,
            "environment": "production",
            "monitor_config": {
                "schedule": { "type": "crontab", "value": "0 3 * * *" },
                "timezone": "Europe/Berlin",
                "checkin_margin": 10,
                "max_runtime": 30
            }
        }),
    )
    .await;

    let monitors = monitors::list(&db, project_id).await.unwrap();
    assert_eq!(monitors.len(), 1);
    let monitor = &monitors[0];
    assert_eq!(monitor.slug, "nightly-backup");
    assert_eq!(monitor.schedule_type, "crontab");
    assert_eq!(monitor.schedule_value, "0 3 * * *");
    assert_eq!(monitor.timezone, "Europe/Berlin");
    assert_eq!(monitor.checkin_margin_minutes, 10);
    assert_eq!(monitor.max_runtime_minutes, 30);
    assert_eq!(monitor.status.as_deref(), Some("ok"));
    // A completed run schedules the next one.
    assert!(monitor.next_due_at.is_some());
    assert!(monitor.last_checkin_at.is_some());

    let (status, duration): (String, Option<f64>) =
        sqlx::query_as("select status, duration_seconds from monitor_checkins")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(status, "ok");
    assert_eq!(duration, Some(12.5));

    // Check-ins are not errors: nothing lands in the issue stream for a healthy run.
    assert_eq!(count(&db, "issues").await, 0);
    assert_eq!(count(&db, "events").await, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_in_progress_run_is_closed_by_its_completion(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let id = "22222222-2222-2222-2222-222222222222";

    check_in(
        &db,
        project_id,
        json!({
            "check_in_id": id,
            "monitor_slug": "import",
            "status": "in_progress",
            "monitor_config": { "schedule": { "type": "interval", "value": 15, "unit": "minute" } }
        }),
    )
    .await;

    // An open run must not schedule the next one — a job that hangs forever would otherwise keep
    // looking punctual.
    let monitor = &monitors::list(&db, project_id).await.unwrap()[0];
    assert_eq!(monitor.status, None);
    assert_eq!(monitor.next_due_at, None);

    check_in(
        &db,
        project_id,
        json!({ "check_in_id": id, "monitor_slug": "import", "status": "error", "duration": 3.0 }),
    )
    .await;

    // Same run updated in place, not a second row.
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from monitor_checkins")
            .fetch_one(&db)
            .await
            .unwrap(),
        1
    );
    let monitor = &monitors::list(&db, project_id).await.unwrap()[0];
    assert_eq!(monitor.status.as_deref(), Some("error"));
    assert!(monitor.next_due_at.is_some());
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_missed_run_becomes_an_issue_through_the_normal_pipeline(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    check_in(
        &db,
        project_id,
        json!({
            "monitor_slug": "nightly-backup",
            "status": "ok",
            "monitor_config": { "schedule": { "type": "interval", "value": 1, "unit": "hour" } }
        }),
    )
    .await;
    make_overdue(&db, 90).await;

    let swept = monitors::sweep(&db).await.unwrap();
    assert_eq!(swept.missed, 1);
    assert_eq!(swept.timed_out, 0);

    // The whole point: a miss is an ordinary event, so grouping, the outbox and alerting apply.
    let issues = issues(&db).await;
    assert_eq!(issues.len(), 1);
    assert!(issues[0].title.starts_with("MonitorMissed:"));
    assert_eq!(issues[0].level, "error");

    let notifications: i64 = sqlx::query_scalar("select count(*) from notifications")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(notifications, 1, "a missed run must reach the alert queue");

    let monitor = &monitors::list(&db, project_id).await.unwrap()[0];
    assert_eq!(monitor.status.as_deref(), Some("missed"));

    // One issue per monitor, not one per sweep: a job broken for a week is one thing to fix.
    make_overdue(&db, 90).await;
    sqlx::query("update monitors set reported_at = null")
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(monitors::sweep(&db).await.unwrap().missed, 1);
    assert_eq!(count(&db, "issues").await, 1);
    assert_eq!(count(&db, "events").await, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_run_that_started_but_never_finished_is_a_timeout_not_a_miss(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    check_in(
        &db,
        project_id,
        json!({
            "monitor_slug": "import",
            "status": "ok",
            "monitor_config": {
                "schedule": { "type": "interval", "value": 1, "unit": "hour" },
                "max_runtime": 30
            }
        }),
    )
    .await;

    // A second run opened an hour ago and never came back.
    check_in(
        &db,
        project_id,
        json!({ "monitor_slug": "import", "status": "in_progress" }),
    )
    .await;
    sqlx::query("update monitor_checkins set created_at = now() - interval '60 minutes' where status = 'in_progress'")
        .execute(&db)
        .await
        .unwrap();
    make_overdue(&db, 30).await;

    let swept = monitors::sweep(&db).await.unwrap();
    assert_eq!(swept.timed_out, 1);
    assert_eq!(swept.missed, 0);

    // Timeouts group apart from misses — they point at a different bug.
    let issues = issues(&db).await;
    assert_eq!(issues.len(), 1);
    assert!(issues[0].title.starts_with("MonitorTimeout:"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_sweep_claims_each_overdue_monitor_once(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    check_in(
        &db,
        project_id,
        json!({
            "monitor_slug": "nightly-backup",
            "status": "ok",
            "monitor_config": { "schedule": { "type": "interval", "value": 1, "unit": "hour" } }
        }),
    )
    .await;
    make_overdue(&db, 90).await;

    assert_eq!(monitors::sweep(&db).await.unwrap().missed, 1);
    // A second sweep (or a second replica) must not re-report the same miss.
    assert_eq!(monitors::sweep(&db).await.unwrap().total(), 0);
    assert_eq!(count(&db, "events").await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_recovered_job_can_alert_again_next_time_it_breaks(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let config = json!({ "schedule": { "type": "interval", "value": 1, "unit": "hour" } });

    check_in(
        &db,
        project_id,
        json!({ "monitor_slug": "job", "status": "ok", "monitor_config": config }),
    )
    .await;
    make_overdue(&db, 90).await;
    assert_eq!(monitors::sweep(&db).await.unwrap().missed, 1);

    // It runs again: the report flag clears.
    check_in(
        &db,
        project_id,
        json!({ "monitor_slug": "job", "status": "ok" }),
    )
    .await;
    let reported: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("select reported_at from monitors")
            .fetch_one(&db)
            .await
            .unwrap();
    assert!(reported.is_none(), "recovery must re-arm the alert");

    // And breaking again is reported again.
    make_overdue(&db, 90).await;
    assert_eq!(monitors::sweep(&db).await.unwrap().missed, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_check_in_with_no_usable_schedule_creates_nothing(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    // No config at all, and no monitor by that name yet: there is nothing to be late for, so a
    // row here would be junk that can never alert.
    check_in(
        &db,
        project_id,
        json!({ "monitor_slug": "unknown-job", "status": "ok" }),
    )
    .await;
    assert!(monitors::list(&db, project_id).await.unwrap().is_empty());

    // An unparseable schedule is refused the same way.
    check_in(
        &db,
        project_id,
        json!({
            "monitor_slug": "broken",
            "status": "ok",
            "monitor_config": { "schedule": { "type": "crontab", "value": "every so often" } }
        }),
    )
    .await;
    assert!(monitors::list(&db, project_id).await.unwrap().is_empty());
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_read_api_serves_a_projects_monitors(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    check_in(
        &db,
        project_id,
        json!({
            "monitor_slug": "nightly-backup",
            "status": "ok",
            "monitor_config": { "schedule": { "type": "crontab", "value": "0 3 * * *" } }
        }),
    )
    .await;

    let response = send(state(db.clone()), get("/api/v1/projects/demo/monitors")).await;
    assert_status(&response, StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body[0]["slug"], json!("nightly-backup"));
    assert_eq!(body[0]["schedule_value"], json!("0 3 * * *"));
    assert_eq!(body[0]["status"], json!("ok"));

    let response = send(state(db.clone()), get("/api/v1/projects/nope/monitors")).await;
    assert_status(&response, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_check_in_alongside_an_event_is_handled_without_dropping_either(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let body = envelope(
        json!({}),
        &[
            (
                json!({ "type": "check_in" }),
                json!({
                    "monitor_slug": "job",
                    "status": "ok",
                    "monitor_config": { "schedule": { "type": "interval", "value": 5, "unit": "minute" } }
                }),
            ),
            (
                json!({ "type": "event" }),
                error_event(&"1".repeat(32), "ValueError", "boom"),
            ),
        ],
    );
    let response = send(
        state(db.clone()),
        envelope_request(project_id, PUBLIC_KEY, body),
    )
    .await;
    assert_status(&response, StatusCode::OK);

    assert_eq!(monitors::list(&db, project_id).await.unwrap().len(), 1);
    assert_eq!(count(&db, "events").await, 1);
}
