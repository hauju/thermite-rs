//! Retention: what gets dropped, and — more importantly — what does not.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use thermite_core::retention::{Policy, sweep};

fn policy(max_age_days: Option<i64>, max_events_per_project: Option<i64>) -> Policy {
    Policy {
        max_age_days,
        max_events_per_project,
        // Deliberately tiny so the batching loop runs several times in these tests; a batch larger
        // than the fixture would never exercise it.
        batch: 100,
    }
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

/// Ingests `n` events for one issue, then backdates their `received_at` by `days_ago`.
///
/// `received_at` is set by the database on insert, so ageing has to be simulated after the fact.
async fn ingest_aged(db: &PgPool, project_id: i64, prefix: char, n: usize, days_ago: i64) {
    for i in 0..n {
        let event_id = format!("{prefix}{:031x}", i);
        ingest(
            db,
            project_id,
            error_event(&event_id, "IOError", "connection reset"),
        )
        .await;
    }

    sqlx::query(
        "update events set received_at = now() - make_interval(days => $1)
          where received_at > now() - interval '1 minute'",
    )
    .bind(days_ago as i32)
    .execute(db)
    .await
    .unwrap();
}

async fn count(db: &PgPool, table: &str) -> i64 {
    let sql = match table {
        "events" => "select count(*) from events",
        "issues" => "select count(*) from issues",
        "event_counts" => "select count(*) from event_counts",
        "session_counts" => "select count(*) from session_counts",
        "ingest_outcomes" => "select count(*) from ingest_outcomes",
        "issue_analyses" => "select count(*) from issue_analyses",
        "notifications" => "select count(*) from notifications",
        other => panic!("unknown table {other:?}"),
    };
    sqlx::query_scalar(sql).fetch_one(db).await.unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn age_drops_old_events_and_keeps_recent_ones(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    ingest_aged(&db, project_id, 'a', 3, 120).await;
    ingest_aged(&db, project_id, 'b', 2, 10).await;
    assert_eq!(count(&db, "events").await, 5);

    let swept = sweep(&db, &policy(Some(90), None)).await.unwrap();

    assert_eq!(swept.events_by_age, 3);
    assert_eq!(count(&db, "events").await, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn age_is_measured_from_receipt_not_from_the_client_timestamp(db: PgPool) {
    // A client controls `timestamp`. If retention keyed on it, an SDK with a broken or hostile clock
    // could have its events evicted on arrival — or keep them forever.
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let mut ancient = error_event(&"1".repeat(32), "IOError", "boom");
    ancient["timestamp"] = json!("1971-01-01T00:00:00Z");
    ingest(&db, project_id, ancient).await;

    let swept = sweep(&db, &policy(Some(90), None)).await.unwrap();

    assert_eq!(
        swept.events_by_age, 0,
        "a 1971 timestamp must not evict a just-received event"
    );
    assert_eq!(count(&db, "events").await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn quota_keeps_the_newest_events_and_drops_the_rest(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    for i in 0..10 {
        let mut event = error_event(&format!("{i}{}", "1".repeat(31)), "IOError", "boom");
        event["timestamp"] = json!(format!("2026-07-{:02}T10:00:00Z", 10 + i));
        ingest(&db, project_id, event).await;
    }

    let swept = sweep(&db, &policy(None, Some(4))).await.unwrap();

    assert_eq!(swept.events_by_quota, 6);
    assert_eq!(count(&db, "events").await, 4);

    // The four kept are the four most recently received.
    let kept: Vec<String> =
        sqlx::query_scalar("select event_id::text from events order by received_at desc, id desc")
            .fetch_all(&db)
            .await
            .unwrap();
    assert_eq!(kept.len(), 4);
    assert!(
        kept.iter().all(|id| id.starts_with(['6', '7', '8', '9'])),
        "expected the last four ingested, got {kept:?}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_project_under_quota_is_left_alone(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    ingest_aged(&db, project_id, 'a', 3, 1).await;

    let swept = sweep(&db, &policy(None, Some(100))).await.unwrap();

    assert_eq!(swept.events_by_quota, 0);
    assert_eq!(count(&db, "events").await, 3);
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_quota_is_per_project(db: PgPool) {
    let noisy = create_project(&db, "noisy", PUBLIC_KEY).await;
    let quiet = create_project(&db, "quiet", "cccccccccccccccccccccccccccccccc").await;

    ingest_aged(&db, noisy, 'a', 8, 1).await;
    for i in 0..2 {
        let body = envelope(
            json!({}),
            &[(
                json!({ "type": "event" }),
                error_event(&format!("b{:031x}", i), "IOError", "boom"),
            )],
        );
        assert_status(
            &send(
                state(db.clone()),
                envelope_request(quiet, "cccccccccccccccccccccccccccccccc", body),
            )
            .await,
            StatusCode::OK,
        );
    }

    sweep(&db, &policy(None, Some(3))).await.unwrap();

    let per_project: Vec<(String, i64)> = sqlx::query_as(
        "select p.slug, count(e.id) from projects p
           left join events e on e.project_id = p.id
          group by p.slug order by p.slug",
    )
    .fetch_all(&db)
    .await
    .unwrap();

    assert_eq!(
        per_project,
        vec![("noisy".to_string(), 3), ("quiet".to_string(), 2)],
        "a noisy project must not consume a quiet one's allowance"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn history_outlives_the_events_it_describes(db: PgPool) {
    // The point of maintaining the rollup during ingest rather than counting rows on read: the
    // charts and the issue's own counters have to survive eviction, or retention would silently
    // rewrite the past.
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    ingest_aged(&db, project_id, 'a', 5, 120).await;

    let issue_id: i64 = sqlx::query_scalar("select id from issues")
        .fetch_one(&db)
        .await
        .unwrap();
    sqlx::query(
        "insert into issue_analyses (issue_id, source, summary) values ($1, 'claude-code', 'root cause')",
    )
    .bind(issue_id)
    .execute(&db)
    .await
    .unwrap();

    let counts_before = count(&db, "event_counts").await;
    sweep(&db, &policy(Some(90), None)).await.unwrap();

    assert_eq!(
        count(&db, "events").await,
        0,
        "the events themselves should be gone"
    );
    assert_eq!(count(&db, "issues").await, 1, "the issue must remain");
    assert_eq!(
        count(&db, "event_counts").await,
        counts_before,
        "charts must survive"
    );
    assert_eq!(
        count(&db, "issue_analyses").await,
        1,
        "agent findings must survive"
    );

    // And `times_seen` still reports what actually happened.
    let times_seen: i64 = sqlx::query_scalar("select times_seen from issues")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(times_seen, 5);
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_issue_with_every_event_evicted_still_serves(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    ingest_aged(&db, project_id, 'a', 2, 120).await;
    sweep(&db, &policy(Some(90), None)).await.unwrap();

    let issue_id: i64 = sqlx::query_scalar("select id from issues")
        .fetch_one(&db)
        .await
        .unwrap();

    let response = send(
        state(db.clone()),
        axum::http::Request::builder()
            .method("GET")
            .uri(format!("/api/v1/issues/{issue_id}"))
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await;

    assert_status(&response, StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["latest_event"], json!(null));
    assert_eq!(body["times_seen"], json!(2));
    assert_eq!(body["analyses"], json!([]));
}

#[sqlx::test(migrations = "../../migrations")]
async fn acked_triage_items_are_dropped_but_unacked_ones_are_never_touched(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    ingest_aged(&db, project_id, 'a', 1, 1).await;
    ingest(
        &db,
        project_id,
        error_event(&"b".repeat(32), "KeyError", "missing"),
    )
    .await;
    assert_eq!(count(&db, "notifications").await, 2);

    // One acked long ago, one never looked at.
    sqlx::query("update notifications set acked_at = now() - interval '60 days' where id = 1")
        .execute(&db)
        .await
        .unwrap();

    let swept = sweep(&db, &policy(Some(90), None)).await.unwrap();

    assert_eq!(swept.notifications, 1);
    let remaining: Vec<(i64, Option<chrono::DateTime<chrono::Utc>>)> =
        sqlx::query_as("select id, acked_at from notifications")
            .fetch_all(&db)
            .await
            .unwrap();
    assert_eq!(remaining.len(), 1);
    assert!(
        remaining[0].1.is_none(),
        "an unacked notification is work nobody did; dropping it hides that"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_recently_acked_item_is_kept(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    ingest_aged(&db, project_id, 'a', 1, 1).await;
    sqlx::query("update notifications set acked_at = now()")
        .execute(&db)
        .await
        .unwrap();

    let swept = sweep(&db, &policy(Some(90), None)).await.unwrap();

    assert_eq!(swept.notifications, 0);
    assert_eq!(count(&db, "notifications").await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_disabled_policy_deletes_nothing(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    ingest_aged(&db, project_id, 'a', 4, 3650).await;

    let swept = sweep(&db, &policy(None, None)).await.unwrap();

    assert_eq!(swept.total(), 0);
    assert_eq!(
        count(&db, "events").await,
        4,
        "ten-year-old events survive when retention is off"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn both_rules_apply_in_one_pass(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    ingest_aged(&db, project_id, 'a', 4, 120).await; // dropped by age
    ingest_aged(&db, project_id, 'b', 6, 1).await; // trimmed by quota

    let swept = sweep(&db, &policy(Some(90), Some(2))).await.unwrap();

    assert_eq!(swept.events_by_age, 4);
    assert_eq!(swept.events_by_quota, 4);
    assert_eq!(count(&db, "events").await, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn sweeping_is_idempotent(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    ingest_aged(&db, project_id, 'a', 3, 120).await;

    let first = sweep(&db, &policy(Some(90), Some(10))).await.unwrap();
    let second = sweep(&db, &policy(Some(90), Some(10))).await.unwrap();

    assert_eq!(first.events_by_age, 3);
    assert_eq!(
        second.total(),
        0,
        "a second pass over swept data must be a no-op"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_project_with_no_events_is_handled(db: PgPool) {
    create_project(&db, "empty", PUBLIC_KEY).await;

    let swept = sweep(&db, &policy(Some(90), Some(10))).await.unwrap();

    assert_eq!(swept.total(), 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn rollup_rows_age_out_but_the_issue_counters_do_not(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    // Two users on one issue, in two different hourly buckets.
    for (event_id, user, ts) in [
        (&"1".repeat(32), "alice", hours_ago(1)),
        (&"2".repeat(32), "ghost", hours_ago(5)),
    ] {
        let mut event = error_event(event_id, "IOError", "connection reset");
        event["user"] = json!({ "id": user });
        event["timestamp"] = json!(ts);
        ingest(&db, project_id, event).await;
    }

    // Age ghost's rollup rows past the window, as if it last happened four months ago.
    sqlx::query(
        "update issue_tags set last_seen = now() - interval '120 days' where value = 'id:ghost'",
    )
    .execute(&db)
    .await
    .unwrap();
    sqlx::query(
        "update event_counts set bucket = now() - interval '120 days'
          where bucket < now() - interval '2 hours'",
    )
    .execute(&db)
    .await
    .unwrap();

    let swept = sweep(&db, &policy(Some(90), None)).await.unwrap();

    assert_eq!(swept.tag_values, 1, "ghost's tag row should age out");
    assert_eq!(swept.count_buckets, 1, "the old bucket should age out");

    let tag_values: Vec<String> =
        sqlx::query_scalar("select value from issue_tags where key = 'user'")
            .fetch_all(&db)
            .await
            .unwrap();
    assert_eq!(tag_values, vec!["id:alice".to_string()]);

    // The permanent history on the issue row is untouched by the pruning.
    let (users_affected, times_seen): (i64, i64) =
        sqlx::query_as("select users_affected, times_seen from issues")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(
        users_affected, 2,
        "the counter must not decay with the rows"
    );
    assert_eq!(times_seen, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn session_buckets_age_out_with_the_policy(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let body = envelope(
        json!({}),
        &[(
            json!({ "type": "sessions" }),
            json!({
                "attrs": { "release": "1.4.2" },
                "aggregates": [
                    { "started": hours_ago(1), "exited": 5 },
                    { "started": hours_ago(5), "exited": 5 },
                ]
            }),
        )],
    );
    assert_status(
        &send(
            state(db.clone()),
            envelope_request(project_id, PUBLIC_KEY, body),
        )
        .await,
        StatusCode::OK,
    );
    assert_eq!(count(&db, "session_counts").await, 2);

    // Age the older bucket past the window.
    sqlx::query(
        "update session_counts set bucket = now() - interval '120 days'
          where bucket < now() - interval '2 hours'",
    )
    .execute(&db)
    .await
    .unwrap();

    let swept = sweep(&db, &policy(Some(90), None)).await.unwrap();

    // The rollup grows with release cardinality rather than with time, so leaving it unswept would
    // mean retention never actually bounds disk.
    assert_eq!(swept.session_buckets, 1);
    assert_eq!(count(&db, "session_counts").await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn outcome_buckets_age_out_with_the_policy(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    // Two accepted events land one outcome bucket for the current hour.
    ingest_aged(&db, project_id, 'a', 2, 0).await;
    assert_eq!(count(&db, "ingest_outcomes").await, 1);

    // Plus one aged past the window.
    sqlx::query(
        "insert into ingest_outcomes (project_id, bucket, outcome, count)
         values ($1, date_trunc('hour', now() - interval '120 days'), 'over_quota', 7)",
    )
    .bind(project_id)
    .execute(&db)
    .await
    .unwrap();

    let swept = sweep(&db, &policy(Some(90), None)).await.unwrap();

    assert_eq!(swept.outcome_buckets, 1);
    assert_eq!(count(&db, "ingest_outcomes").await, 1);
}
