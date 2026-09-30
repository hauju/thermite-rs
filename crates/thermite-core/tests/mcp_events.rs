//! MCP event subscriptions: the upsert is idempotent and refreshes, unsubscribe is scoped to its
//! principal, and firing matches only live subscriptions that cover the issue's project.

mod common;

use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use thermite_core::mcp_events::{self, ISSUE_CREATED, ISSUE_REGRESSED, NewSubscription};

const CALLBACK: &str = "https://receiver.example/cb";

/// Subscribes `principal` and returns the id and new expiry.
async fn subscribe(
    db: &PgPool,
    principal: &str,
    name: &str,
    arguments: Value,
    ttl_seconds: i64,
) -> (String, chrono::DateTime<chrono::Utc>) {
    let arguments = mcp_events::normalize_arguments(Some(&arguments)).unwrap();
    let id = mcp_events::subscription_id(principal, CALLBACK, name, &arguments);
    let expires_at = mcp_events::upsert(
        db,
        &NewSubscription {
            id: &id,
            principal,
            event_name: name,
            arguments: &arguments,
            callback_url: CALLBACK,
            secret_ciphertext: "ciphertext",
            ttl_seconds,
        },
    )
    .await
    .unwrap();
    (id, expires_at)
}

async fn count(db: &PgPool) -> i64 {
    sqlx::query_scalar("select count(*) from event_subscriptions")
        .fetch_one(db)
        .await
        .unwrap()
}

fn ids(subscriptions: &[mcp_events::Subscription]) -> Vec<&str> {
    subscriptions.iter().map(|s| s.id.as_str()).collect()
}

#[sqlx::test(migrations = "../../migrations")]
async fn subscribing_twice_is_one_subscription_and_refreshes_it(db: PgPool) {
    let (first, first_expiry) = subscribe(&db, "user:a", ISSUE_CREATED, json!({}), 3600).await;
    let (second, second_expiry) =
        subscribe(&db, "user:a", ISSUE_CREATED, json!({}), 7 * 24 * 3600).await;

    assert_eq!(first, second);
    assert_eq!(count(&db).await, 1);
    assert!(second_expiry > first_expiry, "refresh must extend expiry");
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_refresh_replaces_the_secret(db: PgPool) {
    let (id, _) = subscribe(&db, "user:a", ISSUE_CREATED, json!({}), 3600).await;
    mcp_events::upsert(
        &db,
        &NewSubscription {
            id: &id,
            principal: "user:a",
            event_name: ISSUE_CREATED,
            arguments: &json!({}),
            callback_url: CALLBACK,
            secret_ciphertext: "rotated",
            ttl_seconds: 3600,
        },
    )
    .await
    .unwrap();

    assert_eq!(
        mcp_events::stored_secret(&db, &id, "user:a")
            .await
            .unwrap()
            .as_deref(),
        Some("rotated")
    );
    assert_eq!(
        mcp_events::stored_secret(&db, &id, "user:b").await.unwrap(),
        None
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn unsubscribe_is_idempotent_and_scoped_to_the_principal(db: PgPool) {
    let (id, _) = subscribe(&db, "user:a", ISSUE_CREATED, json!({}), 3600).await;

    // Someone else cannot remove it, even knowing the id.
    mcp_events::unsubscribe(&db, &id, "user:b").await.unwrap();
    assert_eq!(count(&db).await, 1);

    mcp_events::unsubscribe(&db, &id, "user:a").await.unwrap();
    assert_eq!(count(&db).await, 0);
    mcp_events::unsubscribe(&db, &id, "user:a").await.unwrap();
    assert_eq!(count(&db).await, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn expired_subscriptions_are_not_matched(db: PgPool) {
    let (live, _) = subscribe(&db, "user:a", ISSUE_CREATED, json!({}), 3600).await;
    let (expired, _) = subscribe(&db, "user:b", ISSUE_CREATED, json!({}), 3600).await;
    sqlx::query(
        "update event_subscriptions set expires_at = now() - interval '1 second' where id = $1",
    )
    .bind(&expired)
    .execute(&db)
    .await
    .unwrap();

    let matched = mcp_events::matching(&db, ISSUE_CREATED, "web")
        .await
        .unwrap();
    assert_eq!(ids(&matched), vec![live.as_str()]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_project_filter_and_event_name_decide_what_matches(db: PgPool) {
    let (everything, _) = subscribe(&db, "user:a", ISSUE_CREATED, json!({}), 3600).await;
    let (web, _) = subscribe(
        &db,
        "user:a",
        ISSUE_CREATED,
        json!({ "project": "web" }),
        3600,
    )
    .await;
    subscribe(
        &db,
        "user:a",
        ISSUE_CREATED,
        json!({ "project": "worker" }),
        3600,
    )
    .await;
    subscribe(&db, "user:a", ISSUE_REGRESSED, json!({}), 3600).await;

    let mut matched = ids(&mcp_events::matching(&db, ISSUE_CREATED, "web")
        .await
        .unwrap())
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    matched.sort();
    let mut expected = vec![everything, web];
    expected.sort();
    assert_eq!(matched, expected);
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_issue_summary_carries_the_project_and_latest_release(db: PgPool) {
    let project_id = create_project(&db, "web", PUBLIC_KEY).await;
    let body = envelope(
        json!({}),
        &[(
            json!({ "type": "event" }),
            json!({
                "message": "boom",
                "level": "error",
                "release": "abc123",
                "exception": { "values": [{ "type": "ValueError", "value": "boom" }] }
            }),
        )],
    );
    let response = send(
        state(db.clone()),
        envelope_request(project_id, PUBLIC_KEY, body),
    )
    .await;
    assert_status(&response, axum::http::StatusCode::OK);

    let issue_id: i64 = sqlx::query_scalar("select id from issues")
        .fetch_one(&db)
        .await
        .unwrap();
    let summary = mcp_events::issue_summary(&db, issue_id).await.unwrap();
    assert_eq!(summary.project, "web");
    assert_eq!(summary.release.as_deref(), Some("abc123"));

    let data = mcp_events::payload(&summary, "https://thermite.test/");
    assert_eq!(
        data["url"],
        format!("https://thermite.test/issues/{issue_id}")
    );

    assert!(matches!(
        mcp_events::issue_summary(&db, issue_id + 1000).await,
        Err(thermite_core::AppError::NotFound)
    ));
}
