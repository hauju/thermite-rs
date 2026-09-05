//! OTLP logs ingest, driven through the real router.
//!
//! The unit tests in `ingest::otlp` cover the two readers and the conversion. These cover what
//! only the assembled endpoint can: that a record arrives as an issue, that the severity floor
//! keeps a log stream from becoming one, that the credential and quota are the same ones envelopes
//! use, and that both encodings survive the HTTP layer.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use sqlx::PgPool;

use common::{PUBLIC_KEY, create_project, envelope, envelope_request, error_event, send, state};

/// Nanoseconds since the epoch, an hour ago.
///
/// An offset rather than a fixed instant: ingest clamps client timestamps to a 30-day window, so a
/// hardcoded date is a fixture that starts failing once the calendar moves past it.
fn nanos_ago(hours: i64) -> u64 {
    (chrono::Utc::now().timestamp() - hours * 3600) as u64 * 1_000_000_000
}

fn otlp_request(project_id: i64, key: &str, content_type: &str, body: Vec<u8>) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/api/{project_id}/otlp/v1/logs?sentry_key={key}"))
        .header("content-type", content_type)
        .body(Body::from(body))
        .expect("failed to build request")
}

fn json_request(project_id: i64, key: &str, body: Value) -> Request<Body> {
    otlp_request(
        project_id,
        key,
        "application/json",
        body.to_string().into_bytes(),
    )
}

/// One OTLP/JSON request carrying `records` under one resource.
fn logs(resource: Value, records: Vec<Value>) -> Value {
    json!({
        "resourceLogs": [{
            "resource": { "attributes": resource },
            "scopeLogs": [{ "logRecords": records }],
        }]
    })
}

fn attribute(key: &str, value: &str) -> Value {
    json!({ "key": key, "value": { "stringValue": value } })
}

fn record(severity: i32, body: &str) -> Value {
    json!({
        "timeUnixNano": nanos_ago(1).to_string(),
        "severityNumber": severity,
        "body": { "stringValue": body },
    })
}

// A minimal protobuf encoder, so the wire format the endpoint accepts is written out literally
// rather than round-tripped through the same message definitions the reader uses.

fn varint(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
    out
}

/// A length-delimited field: the key `(tag << 3) | 2`, the length, then the payload.
fn len_field(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![(tag << 3) | 2];
    out.extend(varint(payload.len() as u64));
    out.extend_from_slice(payload);
    out
}

/// A varint field: the key `(tag << 3) | 0`, then the value.
fn varint_field(tag: u8, value: u64) -> Vec<u8> {
    let mut out = vec![tag << 3];
    out.extend(varint(value));
    out
}

/// `LogsData { resource_logs: [{ scope_logs: [{ log_records: [record] }] }] }`.
fn protobuf_logs(severity: u64, body: &str) -> Vec<u8> {
    let any_value = len_field(1, body.as_bytes());
    let log_record = [varint_field(2, severity), len_field(5, &any_value)].concat();
    let scope_logs = len_field(2, &log_record);
    let resource_logs = len_field(2, &scope_logs);

    len_field(1, &resource_logs)
}

async fn issue_titles(db: &PgPool) -> Vec<String> {
    sqlx::query_scalar("select title from issues order by id")
        .fetch_all(db)
        .await
        .expect("failed to read issues")
}

async fn outcome_count(db: &PgPool, outcome: &str) -> i64 {
    sqlx::query_scalar(
        "select coalesce(sum(count), 0)::bigint from ingest_outcomes where outcome = $1",
    )
    .bind(outcome)
    .fetch_one(db)
    .await
    .expect("failed to read outcomes")
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_error_record_becomes_an_issue(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let response = send(
        state(db.clone()),
        json_request(
            project_id,
            PUBLIC_KEY,
            logs(json!([]), vec![record(17, "payment gateway unreachable")]),
        ),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        issue_titles(&db).await,
        vec!["Log Message: payment gateway unreachable"]
    );
    assert_eq!(outcome_count(&db, "accepted").await, 1);
}

/// The severity floor is the difference between error tracking and a log sink with one issue per
/// line. A collector forwards everything; only the errors may become issues.
#[sqlx::test(migrations = "../../migrations")]
async fn records_below_error_are_dropped_but_counted(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let response = send(
        state(db.clone()),
        json_request(
            project_id,
            PUBLIC_KEY,
            logs(
                json!([]),
                vec![
                    record(9, "handling request"),
                    record(13, "gateway slow"),
                    record(17, "payment gateway unreachable"),
                ],
            ),
        ),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(issue_titles(&db).await.len(), 1, "only the error is stored");
    assert_eq!(
        outcome_count(&db, "unsupported:log").await,
        2,
        "the dropped records have to leave a trace"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn exception_attributes_group_as_an_exception(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let mut with_exception = record(17, "charge failed");
    with_exception["attributes"] = json!([
        attribute("exception.type", "ConnectionError"),
        attribute("exception.message", "connection refused"),
        attribute(
            "exception.stacktrace",
            "Traceback…\n  File \"a.py\", line 1"
        ),
    ]);

    send(
        state(db.clone()),
        json_request(
            project_id,
            PUBLIC_KEY,
            logs(json!([]), vec![with_exception]),
        ),
    )
    .await;

    assert_eq!(
        issue_titles(&db).await,
        vec!["ConnectionError: connection refused"]
    );

    // OTel has no structured frames, so the stack has to survive as text where the issue page and
    // `get_issue` both surface it.
    let data: Value = sqlx::query_scalar("select data from events")
        .fetch_one(&db)
        .await
        .unwrap();
    assert!(
        data["extra"]["exception.stacktrace"]
            .as_str()
            .unwrap()
            .contains("Traceback")
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn resource_attributes_fill_in_the_indexed_fields(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    send(
        state(db.clone()),
        json_request(
            project_id,
            PUBLIC_KEY,
            logs(
                json!([
                    attribute("service.name", "checkout"),
                    attribute("service.version", "1.4.2"),
                    attribute("deployment.environment.name", "production"),
                ]),
                vec![record(17, "payment gateway unreachable")],
            ),
        ),
    )
    .await;

    let (release, environment): (Option<String>, Option<String>) =
        sqlx::query_as("select release, environment from events")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(release.as_deref(), Some("1.4.2"));
    assert_eq!(environment.as_deref(), Some("production"));

    // One collector fans many services into one project, split by the same tag a labelled DSN key
    // stamps on.
    let component: String = sqlx::query_scalar(
        "select value from issue_tags where key = 'component' order by value limit 1",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(component, "checkout");
}

/// The spec's default encoding, and what most language SDKs send.
#[sqlx::test(migrations = "../../migrations")]
async fn protobuf_bodies_are_accepted(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let response = send(
        state(db.clone()),
        otlp_request(
            project_id,
            PUBLIC_KEY,
            "application/x-protobuf",
            protobuf_logs(17, "payment gateway unreachable"),
        ),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        issue_titles(&db).await,
        vec!["Log Message: payment gateway unreachable"]
    );
}

/// A protobuf exporter parses the response as protobuf, so answering it with `{}` would look like
/// a malformed reply on every successful export.
#[sqlx::test(migrations = "../../migrations")]
async fn the_response_matches_the_request_encoding(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    for (content_type, body, expected) in [
        (
            "application/json",
            logs(json!([]), vec![record(17, "x")])
                .to_string()
                .into_bytes(),
            "application/json",
        ),
        (
            "application/x-protobuf",
            protobuf_logs(17, "x"),
            "application/x-protobuf",
        ),
    ] {
        let response = send(
            state(db.clone()),
            otlp_request(project_id, PUBLIC_KEY, content_type, body),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some(expected)
        );
    }
}

/// The same credential as every other ingest endpoint. An OTLP endpoint that authenticated
/// differently would be a second way in to protect.
#[sqlx::test(migrations = "../../migrations")]
async fn the_dsn_key_is_required(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let body = logs(json!([]), vec![record(17, "x")]);

    let wrong = send(
        state(db.clone()),
        json_request(project_id, "wrongkeywrongkeywrongkeywrongkey", body.clone()),
    )
    .await;
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

    let missing = send(
        state(db.clone()),
        Request::builder()
            .method("POST")
            .uri(format!("/api/{project_id}/otlp/v1/logs"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

    assert!(issue_titles(&db).await.is_empty());
}

/// `X-Sentry-Auth` as well as the query parameter, since an OTel exporter can only set headers
/// when `OTEL_EXPORTER_OTLP_HEADERS` is configured and only the URL otherwise.
#[sqlx::test(migrations = "../../migrations")]
async fn the_key_may_arrive_in_a_header(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let response = send(
        state(db.clone()),
        Request::builder()
            .method("POST")
            .uri(format!("/api/{project_id}/otlp/v1/logs"))
            .header("content-type", "application/json")
            .header(
                "X-Sentry-Auth",
                format!("Sentry sentry_key={PUBLIC_KEY}, sentry_version=7"),
            )
            .body(Body::from(
                logs(json!([]), vec![record(17, "x")]).to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(issue_titles(&db).await.len(), 1);
}

/// One unit of quota per stored record, as for one event item in an envelope — otherwise one
/// request could store a whole batch for the price of one.
#[sqlx::test(migrations = "../../migrations")]
async fn the_quota_is_charged_per_record(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let mut config = common::config();
    config.rate_limit_per_minute = Some(1);
    let state = thermite_core::state::ThermiteState::new(db.clone(), config);

    let response = send(
        state,
        json_request(
            project_id,
            PUBLIC_KEY,
            logs(
                json!([]),
                vec![record(17, "first error"), record(17, "second error")],
            ),
        ),
    )
    .await;

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        issue_titles(&db).await.len(),
        1,
        "what was digested before the limit stays committed"
    );
    assert_eq!(outcome_count(&db, "over_quota").await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_malformed_body_is_rejected(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let response = send(
        state(db.clone()),
        otlp_request(
            project_id,
            PUBLIC_KEY,
            "application/json",
            b"{not json".to_vec(),
        ),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

/// An empty export is what a collector sends when it has nothing to forward, and it must not look
/// like a failure.
#[sqlx::test(migrations = "../../migrations")]
async fn an_empty_export_succeeds(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let response = send(
        state(db.clone()),
        json_request(project_id, PUBLIC_KEY, json!({ "resourceLogs": [] })),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(issue_titles(&db).await.is_empty());
}

/// The cross-protocol proof: the same failure reported over both protocols is **one** issue.
///
/// This is what makes the two readers alternatives for a service rather than a decision for the
/// instance — a team on OpenTelemetry and a team on a Sentry SDK can report into one project, and
/// a bug that shows up in both does not become two issues to triage separately.
///
/// The payloads are deliberately unalike: the Sentry event carries stack frames and an explicit
/// event id, the OTLP record carries neither and spells its exception as two flat attributes.
/// Grouping keys on the exception type and its normalized value, and nothing else — so if the two
/// ever stop converging, it is because a reader stopped producing the exception interface, which
/// is the failure this test exists to catch.
#[sqlx::test(migrations = "../../migrations")]
async fn the_same_error_over_both_protocols_is_one_issue(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let sentry = send(
        state(db.clone()),
        envelope_request(
            project_id,
            PUBLIC_KEY,
            envelope(
                json!({}),
                &[(
                    json!({ "type": "event" }),
                    error_event(&"1".repeat(32), "ConnectionError", "connection refused"),
                )],
            ),
        ),
    )
    .await;
    assert_eq!(sentry.status(), StatusCode::OK);

    let mut otlp_record = record(17, "charge failed");
    otlp_record["attributes"] = json!([
        attribute("exception.type", "ConnectionError"),
        attribute("exception.message", "connection refused"),
    ]);

    let otlp = send(
        state(db.clone()),
        json_request(project_id, PUBLIC_KEY, logs(json!([]), vec![otlp_record])),
    )
    .await;
    assert_eq!(otlp.status(), StatusCode::OK);

    let (issues, times_seen): (i64, i64) =
        sqlx::query_as("select count(*)::bigint, coalesce(sum(times_seen), 0)::bigint from issues")
            .fetch_one(&db)
            .await
            .unwrap();

    assert_eq!(
        issues, 1,
        "a Sentry SDK and an OTel collector reporting the same failure must not open two issues"
    );
    assert_eq!(times_seen, 2, "each protocol contributed one event");
    assert_eq!(
        issue_titles(&db).await,
        vec!["ConnectionError: connection refused"]
    );

    // Both events are kept: they are two sightings, not one delivered twice.
    let events: i64 = sqlx::query_scalar("select count(*) from events")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(events, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn gzipped_bodies_are_accepted(db: PgPool) {
    use std::io::Write;

    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let body = logs(json!([]), vec![record(17, "payment gateway unreachable")]).to_string();

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(body.as_bytes()).unwrap();
    let gzipped = encoder.finish().unwrap();

    let response = send(
        state(db.clone()),
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/{project_id}/otlp/v1/logs?sentry_key={PUBLIC_KEY}"
            ))
            .header("content-type", "application/json")
            .header("content-encoding", "gzip")
            .body(Body::from(gzipped))
            .unwrap(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(issue_titles(&db).await.len(), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn gzipped_json_without_a_content_type_is_still_read_as_json(db: PgPool) {
    use std::io::Write;

    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;
    let body = logs(json!([]), vec![record(17, "payment gateway unreachable")]).to_string();

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(body.as_bytes()).unwrap();
    let gzipped = encoder.finish().unwrap();

    let response = send(
        state(db.clone()),
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/{project_id}/otlp/v1/logs?sentry_key={PUBLIC_KEY}"
            ))
            .header("content-encoding", "gzip")
            .body(Body::from(gzipped))
            .unwrap(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(issue_titles(&db).await.len(), 1);
}

/// Attributes go through the same scrubbing an SDK payload does, before the digest — so a
/// credential a collector forwarded is never written at all rather than deleted afterwards.
#[sqlx::test(migrations = "../../migrations")]
async fn attributes_are_scrubbed_before_they_are_stored(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let mut leaky = record(17, "connection failed");
    leaky["attributes"] = json!([
        attribute("db.password", "hunter2"),
        attribute("http.route", "/charge"),
    ]);

    send(
        state(db.clone()),
        json_request(project_id, PUBLIC_KEY, logs(json!([]), vec![leaky])),
    )
    .await;

    let data: Value = sqlx::query_scalar("select data from events")
        .fetch_one(&db)
        .await
        .unwrap();

    assert_ne!(data["extra"]["db.password"], Value::from("hunter2"));
    assert_eq!(
        data["extra"]["http.route"],
        Value::from("/charge"),
        "scrubbing must not take the harmless attributes with it"
    );
}

/// A collector aggregates several services into one export, and each carries its own resource.
/// Reading only the first would silently mislabel everything behind it.
#[sqlx::test(migrations = "../../migrations")]
async fn each_resource_group_keeps_its_own_attributes(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let body = json!({
        "resourceLogs": [
            {
                "resource": { "attributes": [attribute("service.name", "checkout")] },
                "scopeLogs": [{ "logRecords": [record(17, "checkout failed")] }],
            },
            {
                "resource": { "attributes": [attribute("service.name", "billing")] },
                "scopeLogs": [{ "logRecords": [record(17, "billing failed")] }],
            },
        ]
    });

    send(
        state(db.clone()),
        json_request(project_id, PUBLIC_KEY, body),
    )
    .await;

    let components: Vec<String> =
        sqlx::query_scalar("select value from issue_tags where key = 'component' order by value")
            .fetch_all(&db)
            .await
            .unwrap();

    assert_eq!(components, vec!["billing", "checkout"]);
}

/// `severityNumber` is optional, and an SDK that never sets it sends zero. Storing those would
/// turn every unlabelled log line into an issue.
#[sqlx::test(migrations = "../../migrations")]
async fn a_record_with_no_severity_is_dropped(db: PgPool) {
    let project_id = create_project(&db, "demo", PUBLIC_KEY).await;

    let response = send(
        state(db.clone()),
        json_request(
            project_id,
            PUBLIC_KEY,
            logs(
                json!([]),
                vec![json!({ "body": { "stringValue": "hello" } })],
            ),
        ),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(issue_titles(&db).await.is_empty());
    assert_eq!(outcome_count(&db, "unsupported:log").await, 1);
}
