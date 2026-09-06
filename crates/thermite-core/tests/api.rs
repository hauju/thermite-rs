//! Read API behaviour: authentication, filtering, and the one-round-trip issue detail an agent
//! consumes.

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

/// Ingests one event so the read API has something to serve.
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
async fn lists_projects_with_their_dsn_and_counts(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    // A current timestamp, not `error_event`'s fixed one: the 24h event count is keyed on the
    // event's own time, so a hardcoded date drifts out of the window.
    let mut event = error_event(
        "11111111111111111111111111111111",
        "ValueError",
        "bad input",
    );
    event["timestamp"] = json!(chrono::Utc::now().to_rfc3339());
    ingest(&db, project_id, event).await;

    let response = send(state(db.clone()), get("/api/v1/projects")).await;
    assert_status(&response, StatusCode::OK);

    let body = body_json(response).await;
    assert_eq!(body[0]["slug"], json!("demo"));
    assert_eq!(
        body[0]["dsn"],
        json!(format!("http://{PUBLIC_KEY}@localhost:9000/{project_id}"))
    );
    assert_eq!(body[0]["unresolved_issues"], json!(1));
    assert_eq!(body[0]["total_issues"], json!(1));
    assert_eq!(body[0]["events_last_24h"], json!(1));
}

#[sqlx::test(migrations = "../../migrations")]
async fn lists_issues_newest_first(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    for (event_id, exception_type, timestamp) in [
        (
            "11111111111111111111111111111111",
            "OldError",
            hours_ago(48),
        ),
        ("22222222222222222222222222222222", "NewError", hours_ago(2)),
    ] {
        let mut event = error_event(event_id, exception_type, "boom");
        event["timestamp"] = json!(timestamp);
        ingest(&db, project_id, event).await;
    }

    let response = send(state(db.clone()), get("/api/v1/projects/demo/issues")).await;
    assert_status(&response, StatusCode::OK);

    let body = body_json(response).await;
    assert_eq!(body.as_array().unwrap().len(), 2);
    assert_eq!(body[0]["title"], json!("NewError: boom"));
    assert_eq!(body[1]["title"], json!("OldError: boom"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn filters_issues_by_status_and_title(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    for (event_id, exception_type) in [
        ("11111111111111111111111111111111", "DatabaseError"),
        ("22222222222222222222222222222222", "TimeoutError"),
    ] {
        ingest(
            &db,
            project_id,
            error_event(event_id, exception_type, "boom"),
        )
        .await;
    }
    sqlx::query("update issues set status = 'resolved' where exception_type = 'TimeoutError'")
        .execute(&db)
        .await
        .unwrap();

    let cases = [
        ("?status=unresolved", vec!["DatabaseError: boom"]),
        ("?status=resolved", vec!["TimeoutError: boom"]),
        ("?q=timeout", vec!["TimeoutError: boom"]),
        (
            "?q=Error",
            vec!["TimeoutError: boom", "DatabaseError: boom"],
        ),
        ("?q=nothingmatches", vec![]),
        ("?status=unresolved&q=database", vec!["DatabaseError: boom"]),
    ];

    for (query, expected) in cases {
        let response = send(
            state(db.clone()),
            get(&format!("/api/v1/projects/demo/issues{query}")),
        )
        .await;
        assert_status(&response, StatusCode::OK);

        let titles: Vec<String> = body_json(response)
            .await
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["title"].as_str().unwrap().to_string())
            .collect();

        // Both issues share a last_seen, so compare as sets.
        let mut actual = titles.clone();
        let mut want: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        actual.sort();
        want.sort();
        assert_eq!(actual, want, "failed for {query}");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn rejects_an_unknown_status_and_an_unknown_project(db: PgPool) {
    create_project(&db, "demo", PUBLIC_KEY).await;

    let response = send(
        state(db.clone()),
        get("/api/v1/projects/demo/issues?status=banana"),
    )
    .await;
    assert_status(&response, StatusCode::BAD_REQUEST);

    let response = send(state(db.clone()), get("/api/v1/projects/nope/issues")).await;
    assert_status(&response, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn respects_limit_and_offset(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    for i in 0..5 {
        let event_id = format!("{i}{}", "1".repeat(31));
        ingest(
            &db,
            project_id,
            error_event(&event_id, &format!("Error{i}"), "boom"),
        )
        .await;
    }

    let page = |query: &str| {
        let db = db.clone();
        let uri = format!("/api/v1/projects/demo/issues?{query}");
        async move {
            let response = send(state(db), get(&uri)).await;
            body_json(response).await.as_array().unwrap().len()
        }
    };

    assert_eq!(page("limit=2").await, 2);
    assert_eq!(page("limit=2&offset=4").await, 1);
    assert_eq!(page("offset=10").await, 0);
    // Over-large limits are clamped rather than rejected.
    assert_eq!(page("limit=1000").await, 5);
}

#[sqlx::test(migrations = "../../migrations")]
async fn issue_detail_returns_everything_needed_to_diagnose_in_one_request(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    // A realistic event: exception chain, frames with source context, breadcrumbs, contexts, tags.
    let event = json!({
        "event_id": "9ec79c33ec9942ab8353589fcb2e04dc",
        "timestamp": "2026-07-29T10:00:00Z",
        "platform": "rust",
        "level": "error",
        "release": "1.4.2",
        "environment": "production",
        "transaction": "GET /users/{id}",
        "server_name": "web-01",
        "exception": { "values": [{
            "type": "DatabaseError",
            "value": "connection refused",
            "module": "app::db",
            "mechanism": { "type": "generic", "handled": false },
            "stacktrace": { "frames": [
                {
                    "function": "main",
                    "filename": "src/main.rs",
                    "abs_path": "/app/src/main.rs",
                    "lineno": 10,
                    "in_app": true,
                },
                {
                    "function": "query_users",
                    "filename": "src/db.rs",
                    "lineno": 42,
                    "colno": 9,
                    "in_app": true,
                    "context_line": "    let conn = pool.get()?;",
                    "pre_context": ["fn query_users() -> Result<Vec<User>> {"],
                    "post_context": ["    conn.query(SQL)", "}"],
                    "vars": { "pool_size": 0 },
                },
            ]}
        }]},
        "breadcrumbs": { "values": [
            { "type": "http", "category": "request", "message": "GET /users/7" },
            { "type": "default", "category": "db", "message": "acquiring connection" },
        ]},
        "contexts": { "runtime": { "name": "rustc", "version": "1.97.1" } },
        "tags": { "region": "eu-central-1" },
        "user": { "id": "7", "email": "a@example.com" },
        "request": { "url": "https://api.example.com/users/7", "method": "GET" },
        "sdk": { "name": "sentry.rust", "version": "0.49.0" },
        "extra": { "retry_count": 3 },
    });
    ingest(&db, project_id, event).await;

    let issue_id: i64 = sqlx::query_scalar("select id from issues")
        .fetch_one(&db)
        .await
        .unwrap();

    let response = send(
        state(db.clone()),
        get(&format!("/api/v1/issues/{issue_id}")),
    )
    .await;
    assert_status(&response, StatusCode::OK);
    let body = body_json(response).await;

    // Issue level.
    assert_eq!(body["title"], json!("DatabaseError: connection refused"));
    assert_eq!(body["culprit"], json!("src/db.rs in query_users"));
    assert_eq!(body["times_seen"], json!(1));
    assert_eq!(body["status"], json!("unresolved"));

    // Event level.
    let event = &body["latest_event"];
    assert_eq!(
        event["event_id"],
        json!("9ec79c33-ec99-42ab-8353-589fcb2e04dc")
    );
    assert_eq!(event["release"], json!("1.4.2"));
    assert_eq!(event["environment"], json!("production"));
    assert_eq!(event["transaction"], json!("GET /users/{id}"));

    // The exception chain is normalized to a plain array.
    assert!(event["exception"].is_array());
    assert_eq!(event["exception"][0]["type"], json!("DatabaseError"));
    assert_eq!(event["exception"][0]["mechanism"]["handled"], json!(false));

    // Frames arrive complete, with the source context an agent needs to reason about the code.
    let frame = &event["exception"][0]["stacktrace"]["frames"][1];
    assert_eq!(frame["function"], json!("query_users"));
    assert_eq!(frame["lineno"], json!(42));
    assert_eq!(frame["context_line"], json!("    let conn = pool.get()?;"));
    assert_eq!(
        frame["pre_context"][0],
        json!("fn query_users() -> Result<Vec<User>> {")
    );
    assert_eq!(frame["vars"]["pool_size"], json!(0));

    // Breadcrumbs are normalized to a plain array too.
    assert!(event["breadcrumbs"].is_array());
    assert_eq!(
        event["breadcrumbs"][1]["message"],
        json!("acquiring connection")
    );

    // And the remaining context blocks are passed through.
    assert_eq!(event["contexts"]["runtime"]["version"], json!("1.97.1"));
    assert_eq!(event["tags"]["region"], json!("eu-central-1"));
    assert_eq!(event["user"]["id"], json!("7"));
    assert_eq!(event["request"]["method"], json!("GET"));
    assert_eq!(event["sdk"]["name"], json!("sentry.rust"));
    assert_eq!(event["extra"]["retry_count"], json!(3));
}

#[sqlx::test(migrations = "../../migrations")]
async fn issue_detail_serves_the_most_recent_event(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    for (event_id, timestamp, value) in [
        (
            "11111111111111111111111111111111",
            hours_ago(48),
            "attempt 1",
        ),
        (
            "22222222222222222222222222222222",
            hours_ago(2),
            "attempt 2",
        ),
    ] {
        let mut event = error_event(event_id, "IOError", value);
        event["timestamp"] = json!(timestamp);
        ingest(&db, project_id, event).await;
    }

    let issue_id: i64 = sqlx::query_scalar("select id from issues")
        .fetch_one(&db)
        .await
        .unwrap();
    let response = send(
        state(db.clone()),
        get(&format!("/api/v1/issues/{issue_id}")),
    )
    .await;

    let body = body_json(response).await;
    assert_eq!(
        body["latest_event"]["event_id"],
        json!("22222222-2222-2222-2222-222222222222")
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_issue_with_no_events_has_a_null_latest_event(db: PgPool) {
    create_project(&db, "demo", PUBLIC_KEY).await;

    // Only reachable if events were deleted under a retention policy, but must not 500.
    let issue_id: i64 = sqlx::query_scalar(
        "insert into issues (project_id, fingerprint_hash, title, level, first_seen, last_seen)
         values (1, '\\x00', 'Orphan', 'error', now(), now()) returning id",
    )
    .fetch_one(&db)
    .await
    .unwrap();

    let response = send(
        state(db.clone()),
        get(&format!("/api/v1/issues/{issue_id}")),
    )
    .await;

    assert_status(&response, StatusCode::OK);
    assert_eq!(body_json(response).await["latest_event"], json!(null));
}

#[sqlx::test(migrations = "../../migrations")]
async fn lists_the_events_of_an_issue(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    // Oldest first; the newest is `newest`, which the listing must lead with.
    let newest = hours_ago(1);
    for (i, timestamp) in [hours_ago(3), hours_ago(2), newest.clone()]
        .into_iter()
        .enumerate()
    {
        let mut event = error_event(&format!("{i}{}", "1".repeat(31)), "IOError", "boom");
        event["timestamp"] = json!(timestamp);
        ingest(&db, project_id, event).await;
    }

    let issue_id: i64 = sqlx::query_scalar("select id from issues")
        .fetch_one(&db)
        .await
        .unwrap();
    let response = send(
        state(db.clone()),
        get(&format!("/api/v1/issues/{issue_id}/events?limit=2")),
    )
    .await;
    assert_status(&response, StatusCode::OK);

    let body = body_json(response).await;
    assert_eq!(body.as_array().unwrap().len(), 2);
    // Newest first. The API serializes the stored TIMESTAMPTZ, so compare instants, not strings.
    assert_eq!(
        parse_ts(body[0]["timestamp"].as_str().unwrap()),
        parse_ts(&newest)
    );
}

fn parse_ts(raw: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .expect("timestamp did not parse")
        .with_timezone(&chrono::Utc)
}

#[sqlx::test(migrations = "../../migrations")]
async fn fetches_an_event_by_the_id_the_sdk_assigned(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    ingest(
        &db,
        project_id,
        error_event(
            "9ec79c33ec9942ab8353589fcb2e04dc",
            "ValueError",
            "bad input",
        ),
    )
    .await;

    // The hex form from an application log, and the hyphenated form, both resolve.
    for id in [
        "9ec79c33ec9942ab8353589fcb2e04dc",
        "9ec79c33-ec99-42ab-8353-589fcb2e04dc",
    ] {
        let response = send(state(db.clone()), get(&format!("/api/v1/events/{id}"))).await;
        assert_status(&response, StatusCode::OK);
        assert_eq!(
            body_json(response).await["exception"][0]["type"],
            json!("ValueError")
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn missing_and_malformed_ids_are_distinguished(db: PgPool) {
    let cases = [
        ("/api/v1/issues/999999", StatusCode::NOT_FOUND),
        (
            "/api/v1/events/9ec79c33ec9942ab8353589fcb2e04dc",
            StatusCode::NOT_FOUND,
        ),
        ("/api/v1/events/not-a-uuid", StatusCode::BAD_REQUEST),
    ];

    for (uri, expected) in cases {
        let response = send(state(db.clone()), get(uri)).await;
        assert_status(&response, expected);
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn alert_routing_can_be_set_read_back_and_cleared(db: PgPool) {
    create_project(&db, "demo", PUBLIC_KEY).await;

    let put = |body: Value| {
        let db = db.clone();
        async move {
            send(
                state(db),
                Request::builder()
                    .method("PUT")
                    .uri("/api/v1/projects/demo/alerts")
                    .header("Content-Type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
        }
    };

    let response = put(json!({
        "alert_email": "oncall@example.test, lead@example.test",
        "alert_webhook": "https://hooks.example.test/demo"
    }))
    .await;
    assert_status(&response, StatusCode::OK);

    let projects = body_json(send(state(db.clone()), get("/api/v1/projects")).await).await;
    assert_eq!(
        projects[0]["alert_email"],
        json!("oncall@example.test, lead@example.test")
    );
    assert_eq!(
        projects[0]["alert_webhook"],
        json!("https://hooks.example.test/demo")
    );

    // Blank clears the override back to the instance-wide default.
    let response = put(json!({ "alert_email": "", "alert_webhook": "" })).await;
    assert_status(&response, StatusCode::OK);
    let projects = body_json(send(state(db.clone()), get("/api/v1/projects")).await).await;
    assert_eq!(projects[0]["alert_email"], json!(null));
    assert_eq!(projects[0]["alert_webhook"], json!(null));

    // Obviously broken values are refused rather than silently eating alerts.
    let response = put(json!({ "alert_webhook": "not a url" })).await;
    assert_status(&response, StatusCode::BAD_REQUEST);
    let response = put(json!({ "alert_email": "not-an-address" })).await;
    assert_status(&response, StatusCode::BAD_REQUEST);

    // An unknown project is a 404, not a silent no-op.
    let response = send(
        state(db.clone()),
        Request::builder()
            .method("PUT")
            .uri("/api/v1/projects/nope/alerts")
            .header("Content-Type", "application/json")
            .body(Body::from(json!({ "alert_email": "a@b.test" }).to_string()))
            .unwrap(),
    )
    .await;
    assert_status(&response, StatusCode::NOT_FOUND);
}

fn req(method: &str, uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn req_empty(method: &str, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn overview_flags_what_needs_attention(db: PgPool) {
    let busy = create_project(&db, "busy", PUBLIC_KEY).await;
    create_project(&db, "quiet", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").await;

    // A current event: one unresolved issue, first seen inside the window.
    let mut event = error_event(
        "11111111111111111111111111111111",
        "ValueError",
        "bad input",
    );
    event["timestamp"] = json!(chrono::Utc::now().to_rfc3339());
    ingest(&db, busy, event).await;

    // A second issue, new but already resolved — it must not flag attention.
    let mut resolved = error_event(
        "22222222222222222222222222222222",
        "NameError",
        "already handled",
    );
    resolved["timestamp"] = json!(chrono::Utc::now().to_rfc3339());
    ingest(&db, busy, resolved).await;
    sqlx::query("update issues set status = 'resolved' where exception_type = 'NameError'")
        .execute(&db)
        .await
        .unwrap();

    // A monitor whose last run was missed, and an alert the delivery loop gave up on.
    sqlx::query(
        "insert into monitors (project_id, slug, schedule_value, status)
         values ($1, 'nightly', '0 3 * * *', 'missed')",
    )
    .bind(busy)
    .execute(&db)
    .await
    .unwrap();
    sqlx::query(
        "update notifications set alert_failed_at = now()
          where issue_id = (select id from issues where exception_type = 'ValueError')",
    )
    .execute(&db)
    .await
    .unwrap();

    let body = body_json(send(state(db.clone()), get("/api/v1/overview")).await).await;

    assert_eq!(body[0]["slug"], json!("busy"));
    assert_eq!(body[0]["unresolved_issues"], json!(1));
    assert_eq!(body[0]["new_issues_24h"], json!(1));
    assert_eq!(body[0]["monitors_failing"], json!(1));
    assert_eq!(body[0]["alerts_dead_lettered"], json!(1));
    assert_eq!(body[0]["events_last_24h"], json!(2));

    // The sparkline covers 24 hourly buckets and sums to the headline number beside it.
    let series: Vec<i64> = body[0]["series"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();
    assert_eq!(series.len(), 24);
    assert_eq!(series.iter().sum::<i64>(), 2);

    // The quiet project reports all zeros rather than being omitted.
    assert_eq!(body[1]["slug"], json!("quiet"));
    assert_eq!(body[1]["unresolved_issues"], json!(0));
    assert_eq!(body[1]["new_issues_24h"], json!(0));
    assert_eq!(body[1]["monitors_failing"], json!(0));
    assert_eq!(body[1]["alerts_dead_lettered"], json!(0));
    assert_eq!(body[1]["events_last_24h"], json!(0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_project_can_be_renamed_but_not_to_blank(db: PgPool) {
    create_project(&db, "demo", PUBLIC_KEY).await;

    let response = send(
        state(db.clone()),
        req(
            "PATCH",
            "/api/v1/projects/demo",
            json!({"name": "Demo App"}),
        ),
    )
    .await;
    assert_status(&response, StatusCode::NO_CONTENT);

    let projects = body_json(send(state(db.clone()), get("/api/v1/projects")).await).await;
    assert_eq!(projects[0]["name"], json!("Demo App"));
    // The slug is the identifier in DSNs and API paths, so renaming never touches it.
    assert_eq!(projects[0]["slug"], json!("demo"));

    let response = send(
        state(db.clone()),
        req("PATCH", "/api/v1/projects/demo", json!({"name": "  "})),
    )
    .await;
    assert_status(&response, StatusCode::BAD_REQUEST);

    let response = send(
        state(db.clone()),
        req("PATCH", "/api/v1/projects/nope", json!({"name": "x"})),
    )
    .await;
    assert_status(&response, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn deleting_a_project_takes_everything_under_it(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let mut event = error_event(
        "11111111111111111111111111111111",
        "ValueError",
        "bad input",
    );
    event["timestamp"] = json!(chrono::Utc::now().to_rfc3339());
    ingest(&db, project_id, event).await;

    let response = send(
        state(db.clone()),
        req_empty("DELETE", "/api/v1/projects/demo"),
    )
    .await;
    assert_status(&response, StatusCode::NO_CONTENT);

    // The project and its children are gone, not orphaned.
    let projects = body_json(send(state(db.clone()), get("/api/v1/projects")).await).await;
    assert_eq!(projects, json!([]));
    let issues: i64 = sqlx::query_scalar("select count(*) from issues")
        .fetch_one(&db)
        .await
        .unwrap();
    let events: i64 = sqlx::query_scalar("select count(*) from events")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!((issues, events), (0, 0));

    // Its DSN stops authenticating immediately.
    let body = envelope(
        json!({}),
        &[(
            json!({ "type": "event" }),
            error_event("22222222222222222222222222222222", "ValueError", "again"),
        )],
    );
    let response = send(
        state(db.clone()),
        envelope_request(project_id, PUBLIC_KEY, body),
    )
    .await;
    assert_status(&response, StatusCode::UNAUTHORIZED);

    // Deleting again is a 404, not a silent no-op.
    let response = send(
        state(db.clone()),
        req_empty("DELETE", "/api/v1/projects/demo"),
    )
    .await;
    assert_status(&response, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_revoked_component_key_stops_authenticating(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let response = send(
        state(db.clone()),
        req(
            "POST",
            "/api/v1/projects/demo/keys",
            json!({"label": "worker"}),
        ),
    )
    .await;
    assert_status(&response, StatusCode::CREATED);
    let created = body_json(response).await;
    let worker_key =
        thermite_core::auth::sentry_key_from_dsn(created["dsn"].as_str().unwrap()).unwrap();

    // The labeled key authenticates until it is revoked.
    let body = envelope(
        json!({}),
        &[(
            json!({ "type": "event" }),
            error_event("11111111111111111111111111111111", "ValueError", "one"),
        )],
    );
    let response = send(
        state(db.clone()),
        envelope_request(project_id, &worker_key, body),
    )
    .await;
    assert_status(&response, StatusCode::OK);

    let response = send(
        state(db.clone()),
        req_empty("DELETE", "/api/v1/projects/demo/keys/worker"),
    )
    .await;
    assert_status(&response, StatusCode::NO_CONTENT);

    let body = envelope(
        json!({}),
        &[(
            json!({ "type": "event" }),
            error_event("22222222222222222222222222222222", "ValueError", "two"),
        )],
    );
    let response = send(
        state(db.clone()),
        envelope_request(project_id, &worker_key, body.clone()),
    )
    .await;
    assert_status(&response, StatusCode::UNAUTHORIZED);

    // The default (unlabeled) key is untouched by label-addressed revocation.
    let response = send(
        state(db.clone()),
        envelope_request(project_id, PUBLIC_KEY, body),
    )
    .await;
    assert_status(&response, StatusCode::OK);

    // A label that does not exist is a 404.
    let response = send(
        state(db.clone()),
        req_empty("DELETE", "/api/v1/projects/demo/keys/worker"),
    )
    .await;
    assert_status(&response, StatusCode::NOT_FOUND);
}
