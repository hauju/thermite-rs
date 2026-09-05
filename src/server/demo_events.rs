//! Synthetic events for the playground page.
//!
//! These are posted to this instance's own ingest endpoint over HTTP rather than handed straight to
//! `digest()`. That is the point: the playground is a smoke test as much as a demo, so it should
//! exercise the path a real SDK takes — DSN authentication, envelope parsing, the quota check and
//! grouping — not a shortcut past it.

use serde_json::{Value, json};
use uuid::Uuid;

use crate::models::AppError;

/// Users the synthetic events are attributed to.
///
/// A fixed, small set on purpose. `user` is synthesized into the `issue_tags` rollup, which the
/// retention sweep deliberately never prunes, so inventing a fresh identity per event would make
/// this page a slow leak rather than a demo.
const USERS: &[(&str, &str, &str)] = &[
    ("1042", "avery", "203.0.113.10"),
    ("1043", "blake", "203.0.113.11"),
    ("1044", "casey", "203.0.113.12"),
    ("1045", "devon", "203.0.113.13"),
    ("1046", "emery", "203.0.113.14"),
];

/// Hosts and durations for the timeout event, so successive clicks differ in the variable parts
/// only — which is what makes them group together.
const HOSTS: &[&str] = &["10.0.0.5", "10.0.0.9", "10.0.1.24", "10.0.2.7"];
const DURATIONS: &[u32] = &[28, 31, 34, 38, 45];

/// Builds one synthetic event payload. `seq` varies the parts that should vary between clicks.
///
/// Returns `None` for an unknown kind, which the caller reports as a validation failure rather than
/// silently sending something else.
pub fn payload(kind: &str, environment: &str, release: &str, seq: usize) -> Option<Value> {
    let (user_id, username, ip) = USERS[seq % USERS.len()];
    let user = json!({
        "id": user_id,
        "username": username,
        "email": format!("{username}@example.test"),
        "ip_address": ip,
    });

    let mut event = match kind {
        "exception" => exception_event(seq),
        "db_timeout" => db_timeout_event(seq),
        "log_message" => log_message_event(seq),
        "fingerprint" => fingerprint_event(seq),
        "warning" => warning_event(seq),
        _ => return None,
    };

    // Common envelope of context every SDK attaches.
    event["event_id"] = json!(Uuid::new_v4().simple().to_string());
    event["timestamp"] = json!(chrono::Utc::now().to_rfc3339());
    event["platform"] = json!("python");
    event["environment"] = json!(environment);
    event["release"] = json!(release);
    event["server_name"] = json!(format!("web-0{}", (seq % 3) + 1));
    event["user"] = user;
    event["sdk"] = json!({ "name": "thermite.playground", "version": "1.0.0" });

    Some(event)
}

fn exception_event(seq: usize) -> Value {
    let orders = ["ord_8812", "ord_8813", "ord_8814"];
    json!({
        "level": "error",
        "transaction": "POST /checkout",
        "tags": { "payment_provider": "stripe", "region": "eu-central-1" },
        "extra": { "order_id": orders[seq % orders.len()], "cart_items": 3 },
        "breadcrumbs": {
            "values": [
                { "type": "http", "category": "request", "message": "POST /checkout", "level": "info" },
                { "type": "default", "category": "cart", "message": "loaded cart for user", "level": "info" },
                { "type": "default", "category": "payment", "message": "resolving payment method", "level": "info" },
            ]
        },
        "exception": {
            "values": [{
                "type": "TypeError",
                "value": "unsupported operand type(s) for +: 'NoneType' and 'Decimal'",
                "stacktrace": { "frames": [
                    {
                        "filename": "gunicorn/workers/sync.py", "abs_path": "/usr/lib/gunicorn/workers/sync.py",
                        "function": "handle_request", "lineno": 176, "in_app": false,
                    },
                    {
                        "filename": "checkout/views.py", "abs_path": "/app/checkout/views.py",
                        "function": "submit_order", "lineno": 88, "in_app": true,
                        "context_line": "    total = subtotal + line.discount",
                        "pre_context": ["def submit_order(request):", "    line = resolve_line(request)"],
                        "post_context": ["    return render(request, 'done.html')"],
                    },
                    {
                        "filename": "checkout/pricing.py", "abs_path": "/app/checkout/pricing.py",
                        "function": "apply_discount", "lineno": 41, "in_app": true,
                        "context_line": "    return base + discount",
                    },
                ]},
            }]
        },
    })
}

fn db_timeout_event(seq: usize) -> Value {
    let host = HOSTS[seq % HOSTS.len()];
    let seconds = DURATIONS[seq % DURATIONS.len()];

    json!({
        "level": "error",
        "transaction": "GET /api/reports",
        "tags": { "database": "primary", "region": "eu-central-1" },
        "extra": { "pool_size": 10, "waiters": 14 },
        "breadcrumbs": {
            "values": [
                { "type": "query", "category": "db", "message": "select * from reports", "level": "info" },
                { "type": "default", "category": "pool", "message": "acquiring connection", "level": "info" },
                { "type": "default", "category": "pool", "message": "pool at capacity", "level": "warning" },
            ]
        },
        "exception": {
            "values": [{
                "type": "PoolTimeout",
                // The host and duration vary; normalization removes both from the grouping key.
                "value": format!("connection to {host}:5432 timed out after {seconds}s"),
                "stacktrace": { "frames": [
                    {
                        "filename": "asyncpg/pool.py", "abs_path": "/usr/lib/asyncpg/pool.py",
                        "function": "_acquire", "lineno": 641, "in_app": false,
                    },
                    {
                        "filename": "reports/queries.py", "abs_path": "/app/reports/queries.py",
                        "function": "monthly_totals", "lineno": 57, "in_app": true,
                        "context_line": "    async with pool.acquire() as conn:",
                    },
                ]},
            }]
        },
    })
}

fn log_message_event(seq: usize) -> Value {
    let keys = ["a41f", "b72c", "c93d"];
    json!({
        "level": "error",
        "transaction": "POST /webhooks/stripe",
        "tags": { "source": "webhook" },
        // The id varies; the normalizer parameterizes it so these stay one issue.
        "message": format!(
            "failed to verify webhook signature for event evt_{} after 3 attempts",
            keys[seq % keys.len()]
        ),
    })
}

fn fingerprint_event(seq: usize) -> Value {
    // Two genuinely different exception types, deliberately pinned to one issue.
    let (kind, value) = if seq.is_multiple_of(2) {
        ("ConnectionResetError", "peer closed the connection")
    } else {
        ("ReadTimeout", "read timed out after 5s")
    };

    json!({
        "level": "error",
        "transaction": "GET /api/inventory",
        "fingerprint": ["inventory-service-unavailable"],
        "tags": { "upstream": "inventory" },
        "exception": {
            "values": [{
                "type": kind,
                "value": value,
                "stacktrace": { "frames": [{
                    "filename": "inventory/client.py", "abs_path": "/app/inventory/client.py",
                    "function": "fetch_stock", "lineno": 120, "in_app": true,
                }]},
            }]
        },
    })
}

fn warning_event(seq: usize) -> Value {
    json!({
        "level": "warning",
        "transaction": "GET /api/search",
        "tags": { "subsystem": "search" },
        "message": format!("search index is {} minutes stale, serving cached results", 12 + seq),
    })
}

/// Wraps one event payload in a Sentry envelope.
///
/// Explicit item lengths, the way SDKs send them, so the payload may contain newlines.
fn envelope(event: &Value) -> String {
    let body = event.to_string();
    let event_id = event["event_id"].as_str().unwrap_or_default();

    format!(
        "{}\n{}\n{body}\n",
        json!({ "event_id": event_id }),
        json!({ "type": "event", "length": body.len() }),
    )
}

/// Posts one synthetic event to this instance's own ingest endpoint.
///
/// `base_url` and `public_key` come from the project's DSN, so this authenticates exactly as an SDK
/// configured with that DSN would.
pub async fn send(
    client: &reqwest::Client,
    base_url: &str,
    project_id: i64,
    public_key: &str,
    event: &Value,
) -> Result<(), AppError> {
    let url = format!("{base_url}/api/{project_id}/envelope/?sentry_key={public_key}");

    let response = client
        .post(&url)
        .body(envelope(event))
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("could not reach the ingest endpoint: {e}")))?;

    if !response.status().is_success() {
        let status = response.status();
        // The ingest endpoint's own message is the useful part — a 429 means the project's quota is
        // spent, which is a legitimate answer rather than a fault.
        let detail = response.text().await.unwrap_or_default();
        return Err(AppError::Internal(format!(
            "ingest rejected the event with {status}: {}",
            detail.trim()
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::demo::DEMO_KINDS;
    use thermite_core::protocol::envelope as wire;

    #[test]
    fn every_advertised_kind_builds_a_payload() {
        // The page renders a button per DEMO_KINDS entry, so a kind with no payload would be a
        // button that always fails.
        for kind in DEMO_KINDS {
            assert!(
                payload(kind.id, "production", "1.0.0", 0).is_some(),
                "no payload for advertised kind {}",
                kind.id
            );
        }
    }

    #[test]
    fn an_unknown_kind_is_refused() {
        assert!(payload("nonsense", "production", "1.0.0", 0).is_none());
    }

    #[test]
    fn each_event_gets_a_distinct_id() {
        // Ingest dedupes on event_id, so a repeated id would silently stop incrementing times_seen.
        let a = payload("exception", "production", "1.0.0", 0).unwrap();
        let b = payload("exception", "production", "1.0.0", 0).unwrap();
        assert_ne!(a["event_id"], b["event_id"]);
    }

    #[test]
    fn the_envelope_parses_as_the_real_parser_reads_it() {
        let event = payload("db_timeout", "staging", "abc123", 0).unwrap();
        let raw = envelope(&event);

        let parsed = wire::parse(raw.as_bytes()).expect("playground envelope must be well-formed");
        assert_eq!(parsed.items.len(), 1);
        assert_eq!(parsed.items[0].item_type(), "event");

        let round_tripped: Value = serde_json::from_slice(parsed.items[0].payload).unwrap();
        assert_eq!(round_tripped["event_id"], event["event_id"]);
        assert_eq!(round_tripped["environment"], "staging");
        assert_eq!(round_tripped["release"], "abc123");
    }

    #[test]
    fn the_timeout_event_varies_only_in_its_variable_parts() {
        // The demo claims these group together; that only holds if the exception type is stable and
        // just the host and duration move.
        let first = payload("db_timeout", "production", "1.0.0", 0).unwrap();
        let second = payload("db_timeout", "production", "1.0.0", 1).unwrap();

        let value_of = |e: &Value| {
            e["exception"]["values"][0]["value"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let type_of = |e: &Value| {
            e["exception"]["values"][0]["type"]
                .as_str()
                .unwrap()
                .to_string()
        };

        assert_eq!(type_of(&first), type_of(&second));
        assert_ne!(value_of(&first), value_of(&second));
    }

    /// Drives the real router on a loopback port, so `send` is exercised against the ingest
    /// endpoint as deployed rather than against a stub.
    async fn serve(pool: sqlx::PgPool) -> String {
        use crate::server::db::Database;
        use crate::server::router;
        use crate::server::test_support::test_state;

        let state = test_state(Database::from_pool(pool));
        let app = router::build(axum::Router::new(), state).await;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        format!("http://{addr}")
    }

    const KEY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    /// Ingest authenticates against `project_keys`, so the fixture seeds it alongside
    /// the project row.
    async fn create_demo_project(pool: &sqlx::PgPool) -> i64 {
        let id: i64 = sqlx::query_scalar(
            "insert into projects (slug, name, public_key) values ($1, $1, $2) returning id",
        )
        .bind("demo")
        .bind(KEY)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query("insert into project_keys (project_id, public_key) values ($1, $2)")
            .bind(id)
            .bind(KEY)
            .execute(pool)
            .await
            .unwrap();
        id
    }

    #[sqlx::test]
    async fn a_raised_event_reaches_the_database_through_the_real_ingest_path(pool: sqlx::PgPool) {
        // The whole claim the playground page makes: clicking a button produces a stored, grouped
        // issue via DSN-authenticated ingest. Nothing here bypasses the endpoint.
        let project_id = create_demo_project(&pool).await;

        let base = serve(pool.clone()).await;
        let client = reqwest::Client::new();

        let event = payload("exception", "staging", "9f2c1ab", 0).unwrap();
        send(&client, &base, project_id, KEY, &event).await.unwrap();

        let (title, culprit, environment, release): (String, Option<String>, String, String) =
            sqlx::query_as(
                "select i.title, i.culprit, e.environment, e.release
                   from issues i join events e on e.issue_id = i.id",
            )
            .fetch_one(&pool)
            .await
            .unwrap();

        assert_eq!(
            title,
            "TypeError: unsupported operand type(s) for +: 'NoneType' and 'Decimal'"
        );
        // Proves the payload's in_app flags are shaped the way grouping expects: the culprit is the
        // innermost in-app frame, not the gunicorn frame that actually raised.
        assert_eq!(
            culprit.as_deref(),
            Some("checkout/pricing.py in apply_discount")
        );
        assert_eq!(environment, "staging");
        assert_eq!(release, "9f2c1ab");
    }

    #[sqlx::test]
    async fn repeated_timeout_events_collapse_into_one_issue(pool: sqlx::PgPool) {
        // The db_timeout button's description promises this; if the normalizer stopped folding the
        // host and duration away, the page would be lying to the reader.
        let project_id = create_demo_project(&pool).await;

        let base = serve(pool.clone()).await;
        let client = reqwest::Client::new();

        for seq in 0..5 {
            let event = payload("db_timeout", "production", "1.0.0", seq).unwrap();
            send(&client, &base, project_id, KEY, &event).await.unwrap();
        }

        let (issues, events, times_seen): (i64, i64, i64) = sqlx::query_as(
            "select (select count(*) from issues),
                    (select count(*) from events),
                    (select times_seen from issues limit 1)",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(
            issues, 1,
            "varying host and duration must not split the issue"
        );
        assert_eq!(events, 5);
        assert_eq!(times_seen, 5);
    }

    #[sqlx::test]
    async fn a_bad_key_is_reported_rather_than_silently_swallowed(pool: sqlx::PgPool) {
        // The page reports "sent N of M, then ..." on failure, which depends on `send` surfacing a
        // non-success status instead of returning Ok.
        let project_id = create_demo_project(&pool).await;

        let base = serve(pool.clone()).await;
        let event = payload("exception", "production", "1.0.0", 0).unwrap();

        let err = send(
            &reqwest::Client::new(),
            &base,
            project_id,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &event,
        )
        .await
        .expect_err("a wrong key must not look like a successful send");

        assert!(
            format!("{err:?}").contains("401"),
            "the ingest status should reach the caller: {err:?}"
        );
    }

    #[test]
    fn attributed_users_stay_within_the_fixed_set() {
        // Guards the reason this set is fixed: `user` lands in a rollup retention never prunes.
        let mut seen = std::collections::HashSet::new();
        for seq in 0..200 {
            let event = payload("exception", "production", "1.0.0", seq).unwrap();
            seen.insert(event["user"]["id"].as_str().unwrap().to_string());
        }
        assert_eq!(seen.len(), USERS.len());
    }
}
