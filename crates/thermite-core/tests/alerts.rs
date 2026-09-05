//! The alert queue: which outbox rows are offered for delivery, and how the lease, backoff,
//! dead-letter and backlog floor behave.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use thermite_core::alerts::{self, Channel};

async fn ingest(db: &PgPool, project_id: i64, mut event: Value) {
    event["environment"] = json!("production");
    event["release"] = json!("abc123");
    let body = envelope(json!({}), &[(json!({ "type": "event" }), event)]);
    let response = send(
        state(db.clone()),
        envelope_request(project_id, PUBLIC_KEY, body),
    )
    .await;
    assert_status(&response, StatusCode::OK);
}

fn post(uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// Lets a leased or backed-off row become claimable again, as the passage of time would.
async fn expire_holds(db: &PgPool) {
    sqlx::query(
        "update notifications
            set alert_lease_until = null, alert_next_attempt_at = null",
    )
    .execute(db)
    .await
    .unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_new_issue_is_claimed_exactly_once(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    alerts::ensure_backlog_floor(&db).await.unwrap();
    ingest(
        &db,
        project_id,
        error_event(&"1".repeat(32), "ValueError", "boom"),
    )
    .await;

    let claimed = alerts::claim(&db, 50, true).await.unwrap();
    assert_eq!(claimed.len(), 1);

    let alert = &claimed[0];
    assert_eq!(alert.kind, "new_issue");
    assert_eq!(alert.project_slug, "demo");
    assert_eq!(alert.title, "ValueError: boom");
    assert_eq!(alert.environment.as_deref(), Some("production"));
    assert_eq!(alert.release.as_deref(), Some("abc123"));
    assert!(!alert.email_done);
    assert!(!alert.webhook_done);

    // The claim leases the row, so a rival replica polling now gets nothing.
    assert!(alerts::claim(&db, 50, true).await.unwrap().is_empty());

    // A second event on a known issue queues nothing new.
    ingest(
        &db,
        project_id,
        error_event(&"2".repeat(32), "ValueError", "boom"),
    )
    .await;
    assert!(alerts::claim(&db, 50, true).await.unwrap().is_empty());

    // Marking is what settles the row; doing it twice is harmless, and an expired lease then
    // re-offers nothing.
    alerts::mark_alerted(&db, alert.id).await.unwrap();
    alerts::mark_alerted(&db, alert.id).await.unwrap();
    expire_holds(&db).await;
    assert!(alerts::claim(&db, 50, true).await.unwrap().is_empty());
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_expired_lease_re_offers_the_row(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    alerts::ensure_backlog_floor(&db).await.unwrap();
    ingest(
        &db,
        project_id,
        error_event(&"1".repeat(32), "ValueError", "boom"),
    )
    .await;

    assert_eq!(alerts::claim(&db, 50, true).await.unwrap().len(), 1);
    assert!(alerts::claim(&db, 50, true).await.unwrap().is_empty());

    // The claimer crashed; its lease runs out; the work is retaken.
    sqlx::query("update notifications set alert_lease_until = now() - interval '1 second'")
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(alerts::claim(&db, 50, true).await.unwrap().len(), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_regression_is_offered_again(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    alerts::ensure_backlog_floor(&db).await.unwrap();
    ingest(
        &db,
        project_id,
        error_event(&"1".repeat(32), "ValueError", "boom"),
    )
    .await;

    let first = &alerts::claim(&db, 50, true).await.unwrap()[0];
    let issue_id = first.issue_id;
    alerts::mark_alerted(&db, first.id).await.unwrap();

    let response = send(
        state(db.clone()),
        post(
            &format!("/api/v1/issues/{issue_id}/status"),
            json!({ "status": "resolved" }),
        ),
    )
    .await;
    assert_status(&response, StatusCode::OK);

    ingest(
        &db,
        project_id,
        error_event(&"2".repeat(32), "ValueError", "boom"),
    )
    .await;

    let claimed = alerts::claim(&db, 50, true).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].kind, "regression");
    assert_eq!(claimed[0].issue_id, issue_id);
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_agent_acking_its_triage_work_does_not_silence_the_alert(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    alerts::ensure_backlog_floor(&db).await.unwrap();
    ingest(
        &db,
        project_id,
        error_event(&"1".repeat(32), "ValueError", "boom"),
    )
    .await;

    let notification_id: i64 = sqlx::query_scalar("select id from notifications")
        .fetch_one(&db)
        .await
        .unwrap();

    // The agent does its triage loop to completion...
    let response = send(
        state(db.clone()),
        post(&format!("/api/v1/triage/{notification_id}/ack"), json!({})),
    )
    .await;
    assert_status(&response, StatusCode::OK);

    // ...and the human still gets told: the two consumers of the outbox are independent.
    assert_eq!(alerts::claim(&db, 50, true).await.unwrap().len(), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn enabling_alerting_late_does_not_flood_with_old_news(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    ingest(
        &db,
        project_id,
        error_event(&"1".repeat(32), "ValueError", "boom"),
    )
    .await;

    // An instance that ran for months without alerting has a backlog like this; only then is
    // alerting switched on, recording the floor.
    sqlx::query("update notifications set created_at = now() - interval '2 days'")
        .execute(&db)
        .await
        .unwrap();
    alerts::ensure_backlog_floor(&db).await.unwrap();

    assert!(alerts::claim(&db, 50, true).await.unwrap().is_empty());

    // And with no floor recorded at all, nothing is eligible — failing closed.
    sqlx::query("delete from alert_state")
        .execute(&db)
        .await
        .unwrap();
    assert!(alerts::claim(&db, 50, true).await.unwrap().is_empty());
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_outage_longer_than_a_day_loses_nothing(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    alerts::ensure_backlog_floor(&db).await.unwrap();
    ingest(
        &db,
        project_id,
        error_event(&"1".repeat(32), "ValueError", "boom"),
    )
    .await;

    // The receiver was down for two days after the alert was queued. The old rolling 24h window
    // silently discarded exactly this row.
    sqlx::query("update alert_state set backlog_floor = now() - interval '10 days'")
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("update notifications set created_at = now() - interval '2 days'")
        .execute(&db)
        .await
        .unwrap();

    assert_eq!(alerts::claim(&db, 50, true).await.unwrap().len(), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn failures_back_off_and_eventually_dead_letter(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    alerts::ensure_backlog_floor(&db).await.unwrap();
    ingest(
        &db,
        project_id,
        error_event(&"1".repeat(32), "ValueError", "boom"),
    )
    .await;

    let alert_id = alerts::claim(&db, 50, true).await.unwrap()[0].id;

    // First failure: backed off, not dead yet, and not claimable right now.
    assert!(!alerts::record_failure(&db, alert_id, 3).await.unwrap());
    assert!(alerts::claim(&db, 50, true).await.unwrap().is_empty());

    // Time passes; it is offered again.
    expire_holds(&db).await;
    assert_eq!(alerts::claim(&db, 50, true).await.unwrap().len(), 1);

    // The remaining attempts burn out; the third is the dead-lettering one.
    assert!(!alerts::record_failure(&db, alert_id, 3).await.unwrap());
    assert!(alerts::record_failure(&db, alert_id, 3).await.unwrap());

    // A dead row stays dead, however much time passes.
    expire_holds(&db).await;
    assert!(alerts::claim(&db, 50, true).await.unwrap().is_empty());
}

#[sqlx::test(migrations = "../../migrations")]
async fn channel_success_survives_into_the_retry(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    alerts::ensure_backlog_floor(&db).await.unwrap();
    ingest(
        &db,
        project_id,
        error_event(&"1".repeat(32), "ValueError", "boom"),
    )
    .await;

    let alert_id = alerts::claim(&db, 50, true).await.unwrap()[0].id;

    // The webhook took it, the email did not: partial success, then a failed attempt.
    alerts::mark_channel(&db, alert_id, Channel::Webhook)
        .await
        .unwrap();
    alerts::record_failure(&db, alert_id, 10).await.unwrap();

    expire_holds(&db).await;
    let retry = &alerts::claim(&db, 50, true).await.unwrap()[0];
    assert!(retry.webhook_done, "the retry must know not to re-send it");
    assert!(!retry.email_done);
}

#[sqlx::test(migrations = "../../migrations")]
async fn without_a_global_channel_only_projects_with_routing_are_claimed(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    alerts::ensure_backlog_floor(&db).await.unwrap();
    ingest(
        &db,
        project_id,
        error_event(&"1".repeat(32), "ValueError", "boom"),
    )
    .await;

    // No global channel and no override: the row is not claimable — burning its attempt
    // counter on undeliverable work would dead-letter it before anyone could be told.
    assert!(alerts::claim(&db, 50, false).await.unwrap().is_empty());

    // A project-level override makes it deliverable, and the claim carries the routing.
    sqlx::query("update projects set alert_webhook = 'https://hooks.example.test/demo'")
        .execute(&db)
        .await
        .unwrap();
    let claimed = alerts::claim(&db, 50, false).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(
        claimed[0].project_alert_webhook.as_deref(),
        Some("https://hooks.example.test/demo")
    );
    assert_eq!(claimed[0].project_alert_email, None);
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_floor_rides_along_until_any_channel_is_configured(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    alerts::ensure_backlog_floor(&db).await.unwrap();
    ingest(
        &db,
        project_id,
        error_event(&"1".repeat(32), "ValueError", "boom"),
    )
    .await;

    // Nothing configured anywhere: the floor advances past the queued row, so configuring
    // alerting later starts clean instead of replaying a backlog.
    alerts::advance_floor_while_unconfigured(&db, false)
        .await
        .unwrap();
    assert!(alerts::claim(&db, 50, true).await.unwrap().is_empty());

    // With a project override present the floor holds still — from here on, an undelivered
    // row is an outage to recover from, not backlog to discard.
    ingest(
        &db,
        project_id,
        error_event(&"2".repeat(32), "OtherError", "boom"),
    )
    .await;
    sqlx::query("update projects set alert_webhook = 'https://hooks.example.test/demo'")
        .execute(&db)
        .await
        .unwrap();
    alerts::advance_floor_while_unconfigured(&db, false)
        .await
        .unwrap();
    assert_eq!(alerts::claim(&db, 50, false).await.unwrap().len(), 1);
}
