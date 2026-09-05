use axum::Json;
use axum::routing::post;
use chrono::Utc;
use serde_json::Value;
use sqlx::PgPool;

use super::*;

fn alert() -> Alert {
    Alert {
        id: 1,
        kind: "new_issue".into(),
        created_at: Utc::now(),
        issue_id: 42,
        project_slug: "api".into(),
        title: "ValueError: bad <input>".into(),
        culprit: Some("handler.rs in handle".into()),
        level: "error".into(),
        times_seen: 3,
        release: Some("abc123".into()),
        environment: Some("production".into()),
        email_done: false,
        webhook_done: false,
        project_alert_email: None,
        project_alert_webhook: None,
    }
}

#[test]
fn messages_carry_what_a_reader_needs_and_escape_markup() {
    let alert = alert();

    assert_eq!(subject(&alert), "[api] New issue: ValueError: bad <input>");

    let body = email_body(&alert, "http://localhost:8099");
    assert!(body.contains("ValueError: bad &lt;input&gt;"), "{body}");
    assert!(!body.contains("bad <input>"), "unescaped payload text");
    assert!(body.contains("http://localhost:8099/issues/42"));
    assert!(body.contains("production"));

    let payload = payload(&alert, "http://localhost:8099");
    assert_eq!(payload["url"], "http://localhost:8099/issues/42");
    assert_eq!(payload["title"], "ValueError: bad <input>");

    let regression = Alert {
        kind: "regression".into(),
        ..self::alert()
    };
    assert!(subject(&regression).contains("Regression"));
}

/// Seeds one issue with one event and its outbox notification, the way digest writes them.
/// Records the backlog floor first, as `spawn` does — without it nothing is eligible.
async fn seed_alert(pool: &PgPool) -> i64 {
    seed_alert_for(pool, "api").await
}

async fn seed_alert_for(pool: &PgPool, slug: &str) -> i64 {
    thermite_core::alerts::ensure_backlog_floor(pool)
        .await
        .unwrap();

    let project_id: i64 = sqlx::query_scalar(
        "insert into projects (slug, name, public_key) values ($1, $1, $1) returning id",
    )
    .bind(slug)
    .fetch_one(pool)
    .await
    .unwrap();

    let issue_id: i64 = sqlx::query_scalar(
        "insert into issues (project_id, fingerprint_hash, title, level, first_seen, last_seen, times_seen)
         values ($1, '\\x00', 'ValueError: bad input', 'error', now(), now(), 1) returning id",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await
    .unwrap();

    sqlx::query(
        "insert into events (event_id, project_id, issue_id, timestamp, level, environment, data)
         values (gen_random_uuid(), $1, $2, now(), 'error', 'production', '{}'::jsonb)",
    )
    .bind(project_id)
    .bind(issue_id)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "insert into notifications (project_id, issue_id, kind) values ($1, $2, 'new_issue')",
    )
    .bind(project_id)
    .bind(issue_id)
    .execute(pool)
    .await
    .unwrap();

    issue_id
}

fn webhook_sink(url: String) -> Sink {
    Sink {
        global_emails: Vec::new(),
        smtp: None,
        global_webhook: Some(url),
        http: reqwest::Client::new(),
        base_url: "http://localhost:8099".into(),
    }
}

/// A webhook receiver that records every delivery it gets.
async fn webhook_receiver() -> (String, tokio::sync::mpsc::UnboundedReceiver<Value>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let receiver = axum::Router::new().route(
        "/hook",
        post(move |Json(body): Json<Value>| {
            let tx = tx.clone();
            async move {
                tx.send(body).unwrap();
                "ok"
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, receiver).await.unwrap() });
    (format!("http://{addr}/hook"), rx)
}

#[sqlx::test]
async fn a_project_override_routes_its_alerts_away_from_the_global_webhook(pool: PgPool) {
    let (global_url, mut global_rx) = webhook_receiver().await;
    let (project_url, mut project_rx) = webhook_receiver().await;

    // Two projects: "routed" carries its own webhook, "plain" falls back to the global one.
    seed_alert_for(&pool, "routed").await;
    seed_alert_for(&pool, "plain").await;
    sqlx::query("update projects set alert_webhook = $1 where slug = 'routed'")
        .bind(&project_url)
        .execute(&pool)
        .await
        .unwrap();

    let sink = webhook_sink(global_url);
    assert_eq!(tick(&pool, &sink).await.unwrap(), 2);

    let to_project = project_rx.recv().await.unwrap();
    assert_eq!(to_project["project"], "routed");
    let to_global = global_rx.recv().await.unwrap();
    assert_eq!(to_global["project"], "plain");

    // Routing, not fan-out: each receiver saw exactly its own alert.
    assert!(project_rx.try_recv().is_err());
    assert!(global_rx.try_recv().is_err());
}

#[sqlx::test]
async fn a_project_override_works_with_no_global_channel_at_all(pool: PgPool) {
    let (project_url, mut project_rx) = webhook_receiver().await;

    seed_alert_for(&pool, "routed").await;
    sqlx::query("update projects set alert_webhook = $1 where slug = 'routed'")
        .bind(&project_url)
        .execute(&pool)
        .await
        .unwrap();

    // No global email, no global webhook — before per-project routing this loop would not
    // even have been spawned.
    let sink = Sink {
        global_emails: Vec::new(),
        smtp: None,
        global_webhook: None,
        http: reqwest::Client::new(),
        base_url: "http://localhost:8099".into(),
    };
    assert!(!sink.globally_configured());

    assert_eq!(tick(&pool, &sink).await.unwrap(), 1);
    assert_eq!(project_rx.recv().await.unwrap()["project"], "routed");
}

#[sqlx::test]
async fn a_pending_alert_reaches_the_webhook_exactly_once(pool: PgPool) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let receiver = axum::Router::new().route(
        "/hook",
        post(move |Json(body): Json<Value>| {
            let tx = tx.clone();
            async move {
                tx.send(body).unwrap();
                "ok"
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, receiver).await.unwrap() });

    let issue_id = seed_alert(&pool).await;
    let sink = webhook_sink(format!("http://{addr}/hook"));

    assert_eq!(tick(&pool, &sink).await.unwrap(), 1);

    let body = rx.recv().await.unwrap();
    assert_eq!(body["kind"], "new_issue");
    assert_eq!(body["title"], "ValueError: bad input");
    assert_eq!(body["environment"], "production");
    assert_eq!(
        body["url"],
        format!("http://localhost:8099/issues/{issue_id}")
    );

    // Delivered means delivered: the next tick must find nothing.
    assert_eq!(tick(&pool, &sink).await.unwrap(), 0);
    assert!(rx.try_recv().is_err());
}

#[sqlx::test]
async fn a_failed_delivery_is_retried_rather_than_marked(pool: PgPool) {
    // Bind-then-drop guarantees a port nothing answers on.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    seed_alert(&pool).await;
    let sink = webhook_sink(format!("http://{addr}/hook"));

    assert_eq!(tick(&pool, &sink).await.unwrap(), 0);

    let (unalerted, attempts): (i64, i32) = sqlx::query_as(
        "select count(*) filter (where alerted_at is null), max(alert_attempts)
           from notifications",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(unalerted, 1, "the row must stay pending for a later tick");
    assert_eq!(
        attempts, 1,
        "the failure must be counted toward the dead-letter"
    );
}

#[sqlx::test]
async fn a_healthy_channel_is_not_re_sent_while_its_sibling_fails(pool: PgPool) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let receiver = axum::Router::new().route(
        "/hook",
        post(move |Json(body): Json<Value>| {
            let tx = tx.clone();
            async move {
                tx.send(body).unwrap();
                "ok"
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, receiver).await.unwrap() });

    seed_alert(&pool).await;

    // Email pointed at a port nothing answers on; the webhook is healthy.
    let smtp = smtp::AsyncSmtpClientImpl::new(smtp::SmtpConfig {
        from: "thermite@example.test".into(),
        host: "127.0.0.1".into(),
        port: 1,
        user: secrecy::SecretString::from(String::new()),
        password: secrecy::SecretString::from(String::new()),
        security: smtp::SmtpSecurity::None,
    })
    .unwrap();
    let sink = Sink {
        global_emails: vec!["oncall@example.test".parse().unwrap()],
        smtp: Some(smtp),
        global_webhook: Some(format!("http://{addr}/hook")),
        http: reqwest::Client::new(),
        base_url: "http://localhost:8099".into(),
    };

    // The webhook is delivered, the email fails, the row stays open.
    assert_eq!(tick(&pool, &sink).await.unwrap(), 0);
    assert!(rx.recv().await.is_some());

    let (webhook_done, alerted): (bool, bool) = sqlx::query_as(
        "select alert_webhook_at is not null, alerted_at is not null from notifications",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(webhook_done, "the webhook's success must be recorded");
    assert!(
        !alerted,
        "the row is not settled while a channel is failing"
    );

    // An immediate retry claims nothing — the failure backed the row off — so the healthy
    // webhook is not spammed every tick.
    assert_eq!(tick(&pool, &sink).await.unwrap(), 0);
    assert!(rx.try_recv().is_err());

    // The backoff elapses and the broken recipient has been removed from the config: the
    // row completes without the webhook ever being re-sent.
    sqlx::query("update notifications set alert_next_attempt_at = null, alert_lease_until = null")
        .execute(&pool)
        .await
        .unwrap();
    let email_removed = webhook_sink(format!("http://{addr}/hook"));
    assert_eq!(tick(&pool, &email_removed).await.unwrap(), 1);
    assert!(rx.try_recv().is_err(), "the webhook already had this alert");
}
