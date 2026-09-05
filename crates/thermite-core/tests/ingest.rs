//! Ingest endpoint behaviour against a real database.
//!
//! `#[sqlx::test]` creates a fresh database per test and runs the migrations, so these are
//! independent and can run in parallel.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::*;
use serde_json::json;
use sqlx::PgPool;
use std::io::Write;
use thermite_core::state::ThermiteState;

#[sqlx::test(migrations = "../../migrations")]
async fn accepts_an_envelope_and_opens_an_issue(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let body = envelope(
        json!({ "event_id": "9ec79c33ec9942ab8353589fcb2e04dc" }),
        &[(
            json!({ "type": "event" }),
            error_event(
                "9ec79c33ec9942ab8353589fcb2e04dc",
                "ValueError",
                "bad input",
            ),
        )],
    );

    let response = send(
        state(db.clone()),
        envelope_request(project_id, PUBLIC_KEY, body),
    )
    .await;

    assert_status(&response, StatusCode::OK);
    assert_eq!(
        body_json(response).await,
        json!({ "id": "9ec79c33ec9942ab8353589fcb2e04dc" })
    );

    let issues = issues(&db).await;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].title, "ValueError: bad input");
    // The innermost in-app frame.
    assert_eq!(issues[0].culprit.as_deref(), Some("handler.rs in handle"));
    assert_eq!(issues[0].times_seen, 1);
    assert_eq!(count(&db, "events").await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn stores_the_full_payload_and_promotes_indexed_columns(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let mut event = error_event(
        "9ec79c33ec9942ab8353589fcb2e04dc",
        "ValueError",
        "bad input",
    );
    event["release"] = json!("1.4.2");
    event["environment"] = json!("production");
    event["transaction"] = json!("GET /users");
    event["server_name"] = json!("web-01");
    event["tags"] = json!({ "region": "eu-central-1" });

    let body = envelope(json!({}), &[(json!({ "type": "event" }), event)]);
    let response = send(
        state(db.clone()),
        envelope_request(project_id, PUBLIC_KEY, body),
    )
    .await;
    assert_status(&response, StatusCode::OK);

    let row: (String, String, String, String, serde_json::Value) =
        sqlx::query_as("select release, environment, transaction, server_name, data from events")
            .fetch_one(&db)
            .await
            .unwrap();

    assert_eq!(row.0, "1.4.2");
    assert_eq!(row.1, "production");
    assert_eq!(row.2, "GET /users");
    assert_eq!(row.3, "web-01");
    // Anything not promoted to a column is still recoverable from `data`.
    assert_eq!(row.4["tags"]["region"], json!("eu-central-1"));
    assert_eq!(
        row.4["exception"]["values"][0]["stacktrace"]["frames"][1]["lineno"],
        json!(42)
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn credentials_in_the_payload_are_scrubbed_before_storage(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let mut event = error_event(
        "9ec79c33ec9942ab8353589fcb2e04dc",
        "ValueError",
        "bad input",
    );
    event["request"] = json!({
        "url": "https://app.example.com/login",
        "headers": {
            "Authorization": "Bearer live-token",
            "Cookie": "session=live-cookie",
            "Accept": "application/json"
        },
        "cookies": { "session": "live-cookie" }
    });
    event["user"] = json!({ "id": "42", "email": "alice@example.com" });
    event["extra"] = json!({ "password": "hunter2", "attempt": 3 });

    let body = envelope(json!({}), &[(json!({ "type": "event" }), event)]);
    let response = send(
        state(db.clone()),
        envelope_request(project_id, PUBLIC_KEY, body),
    )
    .await;
    assert_status(&response, StatusCode::OK);

    let (data,): (serde_json::Value,) = sqlx::query_as("select data from events")
        .fetch_one(&db)
        .await
        .unwrap();

    assert_eq!(data["request"]["headers"]["Authorization"], "[Filtered]");
    assert_eq!(data["request"]["headers"]["Cookie"], "[Filtered]");
    assert_eq!(data["request"]["cookies"], "[Filtered]");
    assert_eq!(data["extra"]["password"], "[Filtered]");
    // Non-sensitive context survives.
    assert_eq!(data["request"]["headers"]["Accept"], "application/json");
    assert_eq!(data["request"]["url"], "https://app.example.com/login");
    assert_eq!(data["extra"]["attempt"], json!(3));
    assert_eq!(data["user"]["email"], "alice@example.com");
    // The raw secrets appear nowhere in the stored row.
    let stored = data.to_string();
    assert!(!stored.contains("live-token"));
    assert!(!stored.contains("live-cookie"));
    assert!(!stored.contains("hunter2"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_store_endpoint_scrubs_too(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let mut event = error_event(
        "9ec79c33ec9942ab8353589fcb2e04dc",
        "ValueError",
        "bad input",
    );
    event["extra"] = json!({ "api_key": "live-key" });

    let request = Request::builder()
        .method("POST")
        .uri(format!("/api/{project_id}/store/"))
        .header(
            "X-Sentry-Auth",
            format!("Sentry sentry_key={PUBLIC_KEY}, sentry_version=7"),
        )
        .body(Body::from(event.to_string()))
        .unwrap();

    let response = send(state(db.clone()), request).await;
    assert_status(&response, StatusCode::OK);

    let (data,): (serde_json::Value,) = sqlx::query_as("select data from events")
        .fetch_one(&db)
        .await
        .unwrap();

    assert_eq!(data["extra"]["api_key"], "[Filtered]");
    assert!(!data.to_string().contains("live-key"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_same_error_twice_becomes_one_issue_with_two_events(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    for event_id in [
        "11111111111111111111111111111111",
        "22222222222222222222222222222222",
    ] {
        let body = envelope(
            json!({}),
            &[(
                json!({ "type": "event" }),
                error_event(event_id, "ValueError", "bad input"),
            )],
        );
        let response = send(
            state(db.clone()),
            envelope_request(project_id, PUBLIC_KEY, body),
        )
        .await;
        assert_status(&response, StatusCode::OK);
    }

    let issues = issues(&db).await;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].times_seen, 2);
    assert_eq!(count(&db, "events").await, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn errors_differing_only_in_variable_data_share_an_issue(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    for (event_id, value) in [
        (
            "11111111111111111111111111111111",
            "timeout talking to 10.0.0.5 after 30s",
        ),
        (
            "22222222222222222222222222222222",
            "timeout talking to 10.0.0.9 after 45s",
        ),
    ] {
        let body = envelope(
            json!({}),
            &[(
                json!({ "type": "event" }),
                error_event(event_id, "IOError", value),
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
    }

    let issues = issues(&db).await;
    assert_eq!(issues.len(), 1, "variable data should not split the issue");
    assert_eq!(issues[0].times_seen, 2);
    // The title reflects the most recent occurrence, with its real values.
    assert_eq!(
        issues[0].title,
        "IOError: timeout talking to 10.0.0.9 after 45s"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn different_errors_become_different_issues(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    for (event_id, exception_type) in [
        ("11111111111111111111111111111111", "ValueError"),
        ("22222222222222222222222222222222", "TypeError"),
    ] {
        let body = envelope(
            json!({}),
            &[(
                json!({ "type": "event" }),
                error_event(event_id, exception_type, "bad input"),
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
    }

    assert_eq!(issues(&db).await.len(), 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_redelivered_event_id_is_not_counted_twice(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let event = error_event(
        "9ec79c33ec9942ab8353589fcb2e04dc",
        "ValueError",
        "bad input",
    );
    let body = envelope(json!({}), &[(json!({ "type": "event" }), event)]);

    // An SDK that did not see our first response retries the identical envelope.
    for _ in 0..3 {
        let response = send(
            state(db.clone()),
            envelope_request(project_id, PUBLIC_KEY, body.clone()),
        )
        .await;
        assert_status(&response, StatusCode::OK);
    }

    let issues = issues(&db).await;
    assert_eq!(
        issues[0].times_seen, 1,
        "retries must not inflate the count"
    );
    assert_eq!(count(&db, "events").await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_explicit_fingerprint_collapses_distinct_errors(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    for (event_id, exception_type) in [
        ("11111111111111111111111111111111", "ValueError"),
        ("22222222222222222222222222222222", "TypeError"),
    ] {
        let mut event = error_event(event_id, exception_type, "whatever");
        event["fingerprint"] = json!(["payment-flow"]);
        let body = envelope(json!({}), &[(json!({ "type": "event" }), event)]);
        assert_status(
            &send(
                state(db.clone()),
                envelope_request(project_id, PUBLIC_KEY, body),
            )
            .await,
            StatusCode::OK,
        );
    }

    let issues = issues(&db).await;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].times_seen, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn out_of_order_delivery_keeps_the_newest_details_and_the_oldest_first_seen(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    // Deliver the newer event first, then an older one. The two values differ only in a duration,
    // so they normalize alike and land in the same issue.
    let (newer, older) = (hours_ago(1), hours_ago(4));
    for (event_id, timestamp, value) in [
        (
            "11111111111111111111111111111111",
            newer.clone(),
            "timeout after 30s",
        ),
        (
            "22222222222222222222222222222222",
            older.clone(),
            "timeout after 45s",
        ),
    ] {
        let mut event = error_event(event_id, "IOError", value);
        event["timestamp"] = json!(timestamp);
        let body = envelope(json!({}), &[(json!({ "type": "event" }), event)]);
        assert_status(
            &send(
                state(db.clone()),
                envelope_request(project_id, PUBLIC_KEY, body),
            )
            .await,
            StatusCode::OK,
        );
    }

    assert_eq!(
        count(&db, "issues").await,
        1,
        "the events should have grouped"
    );

    let row: (
        String,
        chrono::DateTime<chrono::Utc>,
        chrono::DateTime<chrono::Utc>,
    ) = sqlx::query_as("select title, first_seen, last_seen from issues")
        .fetch_one(&db)
        .await
        .unwrap();

    // The late-arriving older event must not overwrite the display title.
    assert_eq!(row.0, "IOError: timeout after 30s");
    assert_eq!(row.1.to_rfc3339(), older);
    assert_eq!(row.2.to_rfc3339(), newer);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_session_only_envelope_is_accepted_and_stores_nothing(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let body = envelope(
        json!({ "event_id": "9ec79c33ec9942ab8353589fcb2e04dc" }),
        &[
            (
                json!({ "type": "session" }),
                json!({ "sid": "abc", "status": "ok" }),
            ),
            (
                json!({ "type": "client_report" }),
                json!({ "discarded_events": [] }),
            ),
        ],
    );

    let response = send(
        state(db.clone()),
        envelope_request(project_id, PUBLIC_KEY, body),
    )
    .await;

    // Rejecting these would make real SDKs treat their whole envelope as failed.
    assert_status(&response, StatusCode::OK);
    assert_eq!(
        body_json(response).await,
        json!({ "id": "9ec79c33ec9942ab8353589fcb2e04dc" })
    );
    assert_eq!(count(&db, "events").await, 0);
    assert_eq!(count(&db, "issues").await, 0);
    // The session carries no release, so it is not release health either — see `session_health`.
    assert_eq!(count(&db, "session_counts").await, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_event_alongside_unsupported_items_is_still_stored(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let body = envelope(
        json!({}),
        &[
            (json!({ "type": "session" }), json!({ "sid": "abc" })),
            (
                json!({ "type": "event" }),
                error_event(
                    "9ec79c33ec9942ab8353589fcb2e04dc",
                    "ValueError",
                    "bad input",
                ),
            ),
            (json!({ "type": "attachment" }), json!("ignored")),
        ],
    );

    let response = send(
        state(db.clone()),
        envelope_request(project_id, PUBLIC_KEY, body),
    )
    .await;

    assert_status(&response, StatusCode::OK);
    assert_eq!(count(&db, "events").await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_malformed_event_item_does_not_discard_its_neighbours(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    // Built by hand: the first event item is not valid JSON.
    let good = error_event(
        "9ec79c33ec9942ab8353589fcb2e04dc",
        "ValueError",
        "bad input",
    )
    .to_string();
    let body = format!(
        "{{}}\n{{\"type\":\"event\",\"length\":9}}\nnot json!\n{{\"type\":\"event\",\"length\":{}}}\n{good}\n",
        good.len()
    );

    let response = send(
        state(db.clone()),
        envelope_request(project_id, PUBLIC_KEY, body),
    )
    .await;

    assert_status(&response, StatusCode::OK);
    assert_eq!(count(&db, "events").await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn accepts_a_gzipped_envelope(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let plain = envelope(
        json!({}),
        &[(
            json!({ "type": "event" }),
            error_event(
                "9ec79c33ec9942ab8353589fcb2e04dc",
                "ValueError",
                "bad input",
            ),
        )],
    );

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(plain.as_bytes()).unwrap();
    let compressed = encoder.finish().unwrap();

    let request = Request::builder()
        .method("POST")
        .uri(format!("/api/{project_id}/envelope/"))
        .header(
            "X-Sentry-Auth",
            format!("Sentry sentry_key={PUBLIC_KEY}, sentry_version=7"),
        )
        .header(header::CONTENT_ENCODING, "gzip")
        .body(Body::from(compressed))
        .unwrap();

    assert_status(&send(state(db.clone()), request).await, StatusCode::OK);
    assert_eq!(count(&db, "events").await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn authenticates_via_the_sentry_key_query_parameter(db: PgPool) {
    // Browser SDKs cannot always set request headers.
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let body = envelope(
        json!({}),
        &[(
            json!({ "type": "event" }),
            error_event(
                "9ec79c33ec9942ab8353589fcb2e04dc",
                "ValueError",
                "bad input",
            ),
        )],
    );

    let request = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/{project_id}/envelope/?sentry_key={PUBLIC_KEY}"
        ))
        .body(Body::from(body))
        .unwrap();

    assert_status(&send(state(db.clone()), request).await, StatusCode::OK);
    assert_eq!(count(&db, "events").await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn authenticates_via_the_dsn_in_the_envelope_header(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let body = envelope(
        json!({ "dsn": format!("http://{PUBLIC_KEY}@localhost:9000/{project_id}") }),
        &[(
            json!({ "type": "event" }),
            error_event(
                "9ec79c33ec9942ab8353589fcb2e04dc",
                "ValueError",
                "bad input",
            ),
        )],
    );

    let request = Request::builder()
        .method("POST")
        .uri(format!("/api/{project_id}/envelope/"))
        .body(Body::from(body))
        .unwrap();

    assert_status(&send(state(db.clone()), request).await, StatusCode::OK);
    assert_eq!(count(&db, "events").await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn rejects_a_wrong_key_with_401_so_sdks_stop_retrying(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let body = envelope(
        json!({}),
        &[(
            json!({ "type": "event" }),
            error_event(
                "9ec79c33ec9942ab8353589fcb2e04dc",
                "ValueError",
                "bad input",
            ),
        )],
    );

    let response = send(
        state(db.clone()),
        envelope_request(project_id, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", body),
    )
    .await;

    assert_status(&response, StatusCode::UNAUTHORIZED);
    // SDKs surface this header in their debug logs; it is how a developer diagnoses a bad DSN.
    assert!(response.headers().contains_key("x-sentry-error"));
    assert_eq!(count(&db, "events").await, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn rejects_an_unknown_project_with_401(db: PgPool) {
    create_project(&db, "demo", PUBLIC_KEY).await;
    let body = envelope(json!({}), &[]);

    let response = send(state(db.clone()), envelope_request(9999, PUBLIC_KEY, body)).await;

    assert_status(&response, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../../migrations")]
async fn rejects_a_request_with_no_credentials(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let request = Request::builder()
        .method("POST")
        .uri(format!("/api/{project_id}/envelope/"))
        .body(Body::from(envelope(json!({}), &[])))
        .unwrap();

    assert_status(&send(state(db), request).await, StatusCode::UNAUTHORIZED);
}

/// gzips `body` at maximum compression, the way an attacker building an amplifier would.
fn gzip_max(body: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(body).unwrap();
    encoder.finish().unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn authenticates_via_the_dsn_in_a_gzipped_envelope_header(db: PgPool) {
    // The `dsn` credential now comes out of a decoded *prefix* rather than the fully parsed body,
    // so it has to keep working when the body is compressed.
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let plain = envelope(
        json!({ "dsn": format!("http://{PUBLIC_KEY}@localhost:9000/{project_id}") }),
        &[(
            json!({ "type": "event" }),
            error_event(
                "9ec79c33ec9942ab8353589fcb2e04dc",
                "ValueError",
                "bad input",
            ),
        )],
    );

    let request = Request::builder()
        .method("POST")
        .uri(format!("/api/{project_id}/envelope/"))
        .header(header::CONTENT_ENCODING, "gzip")
        .body(Body::from(gzip_max(plain.as_bytes())))
        .unwrap();

    assert_status(&send(state(db.clone()), request).await, StatusCode::OK);
    assert_eq!(count(&db, "events").await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_unauthenticated_compression_bomb_is_refused_before_it_is_expanded(db: PgPool) {
    // The amplification this ordering exists to prevent: a small gzip body whose decompressed form
    // is a huge list of minimal envelope items. With no credential anywhere — not in the query, not
    // in a header, not in the envelope's own `dsn` — the request must be refused on the credential
    // check, before the body is decoded or the item list is built.
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let mut plain = String::from("{}\n");
    plain.push_str(&"{}\n\n".repeat(2_000_000));
    let compressed = gzip_max(plain.as_bytes());
    assert!(
        compressed.len() < 64 * 1024,
        "the point is a tiny body: {} bytes",
        compressed.len()
    );

    let request = Request::builder()
        .method("POST")
        .uri(format!("/api/{project_id}/envelope/"))
        .header(header::CONTENT_ENCODING, "gzip")
        .body(Body::from(compressed))
        .unwrap();

    assert_status(
        &send(state(db.clone()), request).await,
        StatusCode::UNAUTHORIZED,
    );
    assert_eq!(count(&db, "events").await, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_wrong_key_is_refused_before_the_body_is_decoded(db: PgPool) {
    // A body that would fail to decode returns 401, not 400: the credential is checked first, so a
    // caller who cannot authenticate never learns anything about how their body was handled.
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let request = Request::builder()
        .method("POST")
        .uri(format!("/api/{project_id}/envelope/"))
        .header(
            "X-Sentry-Auth",
            "Sentry sentry_key=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb, sentry_version=7",
        )
        .header(header::CONTENT_ENCODING, "gzip")
        .body(Body::from("definitely not gzip"))
        .unwrap();

    assert_status(&send(state(db), request).await, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../../migrations")]
async fn rejects_an_envelope_carrying_more_items_than_the_cap(db: PgPool) {
    // Authenticated, so this exercises the parser's item ceiling rather than the credential check.
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let mut body = String::from("{}\n");
    body.push_str(&"{}\n\n".repeat(thermite_core::protocol::envelope::MAX_ITEMS + 1));

    let response = send(
        state(db.clone()),
        envelope_request(project_id, PUBLIC_KEY, body),
    )
    .await;

    assert_status(&response, StatusCode::BAD_REQUEST);
    assert_eq!(count(&db, "events").await, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn rejects_a_malformed_envelope(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let response = send(
        state(db),
        envelope_request(project_id, PUBLIC_KEY, "this is not an envelope"),
    )
    .await;

    assert_status(&response, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrations = "../../migrations")]
async fn enforces_the_per_project_quota_with_a_429_and_backoff_headers(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let other_id = create_project(&db, "other", "cccccccccccccccccccccccccccccccc").await;

    let mut config = config();
    config.rate_limit_per_minute = Some(1);
    let state = ThermiteState::new(db.clone(), config);

    let body = |event_id: &str| {
        envelope(
            json!({}),
            &[(
                json!({ "type": "event" }),
                error_event(event_id, "ValueError", "bad input"),
            )],
        )
    };

    let first = send(
        state.clone(),
        envelope_request(
            project_id,
            PUBLIC_KEY,
            body("11111111111111111111111111111111"),
        ),
    )
    .await;
    assert_status(&first, StatusCode::OK);

    let second = send(
        state.clone(),
        envelope_request(
            project_id,
            PUBLIC_KEY,
            body("22222222222222222222222222222222"),
        ),
    )
    .await;
    assert_status(&second, StatusCode::TOO_MANY_REQUESTS);
    assert!(second.headers().contains_key(header::RETRY_AFTER));
    let limits = second.headers()["x-sentry-rate-limits"].to_str().unwrap();
    assert!(
        limits.contains(":error:project:"),
        "unexpected header: {limits}"
    );

    // The quota is per project, so a different project is unaffected.
    let other = send(
        state,
        envelope_request(
            other_id,
            "cccccccccccccccccccccccccccccccc",
            body("33333333333333333333333333333333"),
        ),
    )
    .await;
    assert_status(&other, StatusCode::OK);

    assert_eq!(count(&db, "events").await, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_legacy_store_endpoint_accepts_a_bare_event(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let event = error_event(
        "9ec79c33ec9942ab8353589fcb2e04dc",
        "ValueError",
        "bad input",
    );

    let request = Request::builder()
        .method("POST")
        .uri(format!("/api/{project_id}/store/"))
        .header(
            "X-Sentry-Auth",
            format!("Sentry sentry_key={PUBLIC_KEY}, sentry_version=7"),
        )
        .body(Body::from(event.to_string()))
        .unwrap();

    let response = send(state(db.clone()), request).await;

    assert_status(&response, StatusCode::OK);
    assert_eq!(
        body_json(response).await,
        json!({ "id": "9ec79c33ec9942ab8353589fcb2e04dc" })
    );
    assert_eq!(issues(&db).await[0].title, "ValueError: bad input");
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_event_without_an_id_still_gets_one_back(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let body = envelope(
        json!({}),
        &[(
            json!({ "type": "event" }),
            json!({ "message": "no id here" }),
        )],
    );

    let response = send(
        state(db.clone()),
        envelope_request(project_id, PUBLIC_KEY, body),
    )
    .await;

    assert_status(&response, StatusCode::OK);
    let id = body_json(response).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(id.len(), 32, "expected a 32-char hex id, got {id:?}");
    assert_eq!(count(&db, "events").await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn preflight_succeeds_for_browser_sdks(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let request = Request::builder()
        .method("OPTIONS")
        .uri(format!("/api/{project_id}/envelope/"))
        .header("Origin", "https://app.example.com")
        .header("Access-Control-Request-Method", "POST")
        .header("Access-Control-Request-Headers", "x-sentry-auth")
        .body(Body::empty())
        .unwrap();

    let response = send(state(db), request).await;

    assert!(
        response.status().is_success(),
        "preflight failed with {}",
        response.status()
    );
    assert!(
        response
            .headers()
            .contains_key("access-control-allow-origin")
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_quota_is_charged_per_event_not_per_envelope(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let mut config = config();
    config.rate_limit_per_minute = Some(3);
    let state = ThermiteState::new(db.clone(), config);

    // One envelope carrying five events against a quota of three. Charging per request would
    // store all five for the price of one — the whole point of the quota is that it bounds
    // *events*, which are what cost storage.
    let items: Vec<(serde_json::Value, serde_json::Value)> = (0..5)
        .map(|i| {
            (
                json!({ "type": "event" }),
                error_event(&format!("{i}{}", "1".repeat(31)), "ValueError", "boom"),
            )
        })
        .collect();

    let response = send(
        state.clone(),
        envelope_request(project_id, PUBLIC_KEY, envelope(json!({}), &items)),
    )
    .await;

    // Over quota mid-envelope: the SDK is told to back off, with the Sentry headers intact.
    assert_status(&response, StatusCode::TOO_MANY_REQUESTS);
    assert!(response.headers().get("retry-after").is_some());
    assert!(response.headers().get("x-sentry-rate-limits").is_some());

    // What was accepted before the limit stays committed — each digest is its own transaction,
    // and an accepted event is durable by design.
    assert_eq!(count(&db, "events").await, 3);

    // And the next request is refused before the body is even decoded.
    let again = send(
        state.clone(),
        envelope_request(
            project_id,
            PUBLIC_KEY,
            envelope(
                json!({}),
                &[(
                    json!({ "type": "event" }),
                    error_event(&"9".repeat(32), "ValueError", "boom"),
                )],
            ),
        ),
    )
    .await;
    assert_status(&again, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(count(&db, "events").await, 3);
}

#[sqlx::test(migrations = "../../migrations")]
async fn non_event_items_cost_no_quota(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let mut config = config();
    config.rate_limit_per_minute = Some(1);
    let state = ThermiteState::new(db.clone(), config);

    // Sessions are folded into the release-health rollup rather than stored as events, and cost no
    // quota; charging for them would let an SDK's routine session flush exhaust the budget its
    // errors need.
    let sessions = envelope(
        json!({}),
        &[
            (json!({ "type": "session" }), json!({ "sid": "a" })),
            (json!({ "type": "session" }), json!({ "sid": "b" })),
        ],
    );
    assert_status(
        &send(
            state.clone(),
            envelope_request(project_id, PUBLIC_KEY, sessions),
        )
        .await,
        StatusCode::OK,
    );

    // The one unit of quota is still available for a real event.
    let event = envelope(
        json!({}),
        &[(
            json!({ "type": "event" }),
            error_event(&"1".repeat(32), "ValueError", "boom"),
        )],
    );
    assert_status(
        &send(
            state.clone(),
            envelope_request(project_id, PUBLIC_KEY, event),
        )
        .await,
        StatusCode::OK,
    );
    assert_eq!(count(&db, "events").await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_labeled_key_stamps_its_component_onto_the_issue(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let key = thermite_core::api::admin::create_project_key(&db, &config(), "demo", "worker")
        .await
        .expect("failed to create labeled key");
    let worker_key = thermite_core::auth::sentry_key_from_dsn(&key.dsn).unwrap();

    let body = envelope(
        json!({}),
        &[(
            json!({ "type": "event" }),
            error_event(&"a".repeat(32), "ValueError", "boom"),
        )],
    );
    let response = send(
        state(db.clone()),
        envelope_request(project_id, &worker_key, body),
    )
    .await;
    assert_status(&response, StatusCode::OK);

    // Same project, same issue stream — the label is a tag, not a second project.
    let issues = issues(&db).await;
    assert_eq!(issues.len(), 1);
    let component: Option<(String,)> =
        sqlx::query_as("select value from issue_tags where issue_id = $1 and key = 'component'")
            .bind(issues[0].id)
            .fetch_optional(&db)
            .await
            .unwrap();
    assert_eq!(component, Some(("worker".to_string(),)));
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_default_key_stamps_no_component(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let body = envelope(
        json!({}),
        &[(
            json!({ "type": "event" }),
            error_event(&"b".repeat(32), "ValueError", "boom"),
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

    let count: i64 = sqlx::query_scalar("select count(*) from issue_tags where key = 'component'")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_explicit_sdk_component_tag_wins_over_the_key_label(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let key = thermite_core::api::admin::create_project_key(&db, &config(), "demo", "worker")
        .await
        .unwrap();
    let worker_key = thermite_core::auth::sentry_key_from_dsn(&key.dsn).unwrap();

    let mut event = error_event(&"c".repeat(32), "ValueError", "boom");
    event["tags"] = json!({ "component": "custom" });
    let body = envelope(json!({}), &[(json!({ "type": "event" }), event)]);
    assert_status(
        &send(
            state(db.clone()),
            envelope_request(project_id, &worker_key, body),
        )
        .await,
        StatusCode::OK,
    );

    let values: Vec<(String,)> =
        sqlx::query_as("select value from issue_tags where key = 'component'")
            .fetch_all(&db)
            .await
            .unwrap();
    assert_eq!(values, vec![("custom".to_string(),)]);
}

/// Every kind of drop leaves a countable trace, or "am I silently losing events?" has no answer.
#[sqlx::test(migrations = "../../migrations")]
async fn every_drop_is_recorded_as_an_ingest_outcome(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let mut config = config();
    config.rate_limit_per_minute = Some(3);
    let state = ThermiteState::new(db.clone(), config);

    // Hand-built rather than through `envelope()`, because one item's payload must not be JSON.
    let mut body = String::from("{}\n");
    let tx = json!({ "spans": [] }).to_string();
    body.push_str(&format!(
        "{}\n{tx}\n",
        json!({ "type": "transaction", "length": tx.len() })
    ));
    let bad = "not json";
    body.push_str(&format!(
        "{}\n{bad}\n",
        json!({ "type": "event", "length": bad.len() })
    ));
    let event = error_event(&"1".repeat(32), "ValueError", "boom").to_string();
    body.push_str(&format!(
        "{}\n{event}\n",
        json!({ "type": "event", "length": event.len() })
    ));

    // Unsupported and invalid items are dropped with a 200; the valid event lands. The invalid
    // item still cost a quota unit (the quota is charged before the payload is parsed), so two
    // of three units are now spent.
    let response = send(
        state.clone(),
        envelope_request(project_id, PUBLIC_KEY, body),
    )
    .await;
    assert_status(&response, StatusCode::OK);

    // Two more events against the one remaining unit: the first is accepted, the second is
    // rejected mid-envelope.
    let two = envelope(
        json!({}),
        &[
            (
                json!({ "type": "event" }),
                error_event(&"2".repeat(32), "ValueError", "boom"),
            ),
            (
                json!({ "type": "event" }),
                error_event(&"3".repeat(32), "ValueError", "boom"),
            ),
        ],
    );
    let response = send(state.clone(), envelope_request(project_id, PUBLIC_KEY, two)).await;
    assert_status(&response, StatusCode::TOO_MANY_REQUESTS);

    // And a further request is refused by the pre-body check — which must also count.
    let again = envelope(
        json!({}),
        &[(
            json!({ "type": "event" }),
            error_event(&"4".repeat(32), "ValueError", "boom"),
        )],
    );
    let response = send(state, envelope_request(project_id, PUBLIC_KEY, again)).await;
    assert_status(&response, StatusCode::TOO_MANY_REQUESTS);

    let outcomes: Vec<(String, i64)> = sqlx::query_as(
        "select outcome, sum(count)::bigint from ingest_outcomes
          where project_id = $1 group by outcome order by outcome",
    )
    .bind(project_id)
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(
        outcomes,
        vec![
            ("accepted".to_string(), 2),
            ("invalid".to_string(), 1),
            ("over_quota".to_string(), 2),
            ("unsupported:transaction".to_string(), 1),
        ]
    );
}

/// A client report is the SDK's own drop counter: it costs no quota, stores nothing, and lands in
/// the outcome rollup so events lost inside the SDK are visible next to the ones lost here.
#[sqlx::test(migrations = "../../migrations")]
async fn client_reports_are_counted_as_drops(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let state = ThermiteState::new(db.clone(), config());

    let body = envelope(
        json!({}),
        &[
            (
                json!({ "type": "client_report" }),
                json!({
                    "timestamp": "2026-08-27T10:00:00Z",
                    "discarded_events": [
                        { "reason": "queue_overflow", "category": "error", "quantity": 23 },
                        // Not stored here, so not a loss here.
                        { "reason": "sample_rate", "category": "transaction", "quantity": 100 },
                    ],
                    "rate_limited_events": [
                        { "reason": "ratelimit_backoff", "category": "error", "quantity": 2 },
                    ],
                }),
            ),
            (
                json!({ "type": "event" }),
                error_event(&"1".repeat(32), "ValueError", "boom"),
            ),
        ],
    );

    let response = send(state, envelope_request(project_id, PUBLIC_KEY, body)).await;
    assert_status(&response, StatusCode::OK);

    let outcomes: Vec<(String, i64)> = sqlx::query_as(
        "select outcome, sum(count)::bigint from ingest_outcomes
          where project_id = $1 group by outcome order by outcome",
    )
    .bind(project_id)
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(
        outcomes,
        vec![
            ("accepted".to_string(), 1),
            ("client_discarded:queue_overflow".to_string(), 23),
            ("client_discarded:ratelimit_backoff".to_string(), 2),
        ]
    );

    // The report itself is not an event: nothing was stored for it.
    let events: (i64,) = sqlx::query_as("select count(*) from events where project_id = $1")
        .bind(project_id)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(events.0, 1);
}
