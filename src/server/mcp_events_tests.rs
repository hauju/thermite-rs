//! The events extension over the real router: discover, `events/*` on `2026-07-28`, callback
//! verification against a loopback receiver, and a signed delivery from the fire endpoint.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::http::HeaderMap;
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::server::db::Database;
use crate::server::router;
use crate::server::test_support::{seed_api_key, seed_user, test_state};

const SECRET: &str = "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw";

async fn serve(pool: PgPool, events: bool) -> String {
    let mut state = test_state(Database::from_pool(pool));
    state.config.mcp_events = events;
    let app = router::build(Router::new(), state).await;
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

async fn token(pool: &PgPool) -> String {
    let db = Database::from_pool(pool.clone());
    let user = seed_user(&db, "events").await;
    seed_api_key(&db, user).await
}

/// One stateless `2026-07-28` request, with the headers and `_meta` that version requires.
/// Returns the status, the response content type, and the JSON-RPC payload (from JSON or SSE).
async fn rpc(
    base: &str,
    token: &str,
    method: &str,
    params: Value,
) -> (reqwest::StatusCode, String, Option<Value>) {
    let mut params = params;
    params["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": { "name": "test", "version": "0" }
    });
    let res = reqwest::Client::new()
        .post(format!("{base}/mcp"))
        .header("authorization", format!("Bearer {token}"))
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", method)
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }))
        .send()
        .await
        .unwrap();
    let status = res.status();
    let content_type = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let text = res.text().await.unwrap();
    let payload = text
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .find_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
        .or_else(|| serde_json::from_str(&text).ok());
    (status, content_type, payload)
}

/// What the receiver saw: headers and the exact body bytes.
type Seen = Arc<Mutex<Vec<(HeaderMap, Vec<u8>)>>>;

/// A loopback webhook receiver. Echoes a verification challenge (or a wrong one, when
/// `honest` is false) and accepts everything else with `200`.
async fn receiver(honest: bool) -> (String, Seen) {
    let seen: Seen = Arc::default();
    let log = seen.clone();
    let app = Router::new().route(
        "/cb",
        axum::routing::post(move |headers: HeaderMap, body: axum::body::Bytes| {
            let log = log.clone();
            async move {
                log.lock().unwrap().push((headers, body.to_vec()));
                let message: Value = serde_json::from_slice(&body).unwrap();
                if message["type"] == "verification" {
                    let challenge = if honest {
                        message["challenge"].clone()
                    } else {
                        json!("wrong")
                    };
                    axum::Json(json!({ "challenge": challenge }))
                } else {
                    axum::Json(json!({}))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    // By name, so delivery goes through the resolver override rather than an IP literal.
    (format!("http://localhost:{}/cb", addr.port()), seen)
}

fn subscribe_params(url: &str) -> Value {
    json!({
        "name": "issue.created",
        "arguments": { "project": "web" },
        "delivery": { "mode": "webhook", "url": url, "secret": SECRET },
        "cursor": null
    })
}

/// A project `web` with one issue; returns the issue id.
async fn seed_issue(pool: &PgPool) -> i64 {
    let project: i64 = sqlx::query_scalar(
        "insert into projects (slug, name, public_key) values ('web', 'web', $1) returning id",
    )
    .bind("b".repeat(32))
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query_scalar(
        "insert into issues (project_id, fingerprint_hash, title, culprit, level, first_seen, last_seen)
         values ($1, '\\x01', 'ValueError: boom', 'app.main', 'error', now(), now()) returning id",
    )
    .bind(project)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn fire(base: &str, token: &str, issue_id: i64) -> Value {
    let res = reqwest::Client::new()
        .post(format!("{base}/api/v1/mcp-events/fire"))
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "issue_id": issue_id, "name": "issue.created" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    res.json().await.unwrap()
}

/// rmcp frames the discover result as SSE (`text/event-stream`); the shim must add the
/// capability inside that framing, not only to a plain JSON body.
#[sqlx::test]
async fn discover_advertises_events_when_enabled(pool: PgPool) {
    let token = token(&pool).await;
    let base = serve(pool, true).await;

    let (status, content_type, body) = rpc(&base, &token, "server/discover", json!({})).await;
    assert_eq!(status, 200);
    assert!(
        content_type.starts_with("text/event-stream"),
        "rmcp's discover framing changed: {content_type}"
    );
    let body = body.expect("discover returned no payload");
    assert_eq!(
        body["result"]["capabilities"]["events"],
        json!({}),
        "{body}"
    );
    assert!(
        body["result"]["capabilities"]["tools"].is_object(),
        "{body}"
    );
}

#[sqlx::test]
async fn events_list_returns_both_definitions(pool: PgPool) {
    let token = token(&pool).await;
    let base = serve(pool, true).await;

    let (status, _, body) = rpc(&base, &token, "events/list", json!({})).await;
    assert_eq!(status, 200);
    let body = body.expect("events/list returned no payload");
    let names: Vec<&str> = body["result"]["events"]
        .as_array()
        .unwrap_or_else(|| panic!("no events array: {body}"))
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["issue.created", "issue.regressed"]);
    assert_eq!(body["result"]["events"][0]["delivery"], json!(["webhook"]));
}

#[sqlx::test]
async fn without_the_flag_there_are_no_events(pool: PgPool) {
    let token = token(&pool).await;
    let base = serve(pool, false).await;

    let (_, _, body) = rpc(&base, &token, "server/discover", json!({})).await;
    let body = body.expect("discover returned no payload");
    assert!(body["result"]["capabilities"].is_object(), "{body}");
    assert!(
        body["result"]["capabilities"].get("events").is_none(),
        "{body}"
    );

    let (_, _, body) = rpc(&base, &token, "events/list", json!({})).await;
    let body = body.expect("events/list returned no payload");
    assert_eq!(body["error"]["code"], -32601, "{body}");

    let res = reqwest::Client::new()
        .post(format!("{base}/api/v1/mcp-events/fire"))
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "issue_id": 1, "name": "issue.created" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404, "the fire endpoint must not be mounted");
}

#[sqlx::test]
async fn subscribing_requires_a_valid_credential(pool: PgPool) {
    let base = serve(pool, true).await;
    let (url, seen) = receiver(true).await;

    let (_, _, body) = rpc(
        &base,
        "oat_not_a_real_key",
        "events/subscribe",
        subscribe_params(&url),
    )
    .await;
    let body = body.expect("no payload");
    assert!(body["error"].is_object(), "{body}");
    assert!(
        seen.lock().unwrap().is_empty(),
        "no callback may be dialled"
    );

    // No credential at all is stopped at the edge with the OAuth challenge.
    let res = reqwest::Client::new()
        .post(format!("{base}/mcp"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "events/subscribe" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
}

#[sqlx::test]
async fn a_callback_that_fails_the_challenge_is_not_subscribed(pool: PgPool) {
    let token = token(&pool).await;
    let base = serve(pool.clone(), true).await;
    let (url, seen) = receiver(false).await;

    let (_, _, body) = rpc(&base, &token, "events/subscribe", subscribe_params(&url)).await;
    let body = body.expect("no payload");
    assert_eq!(body["error"]["code"], -32015, "{body}");
    assert_eq!(
        body["error"]["data"]["reason"], "challenge_failed",
        "{body}"
    );
    assert_eq!(seen.lock().unwrap().len(), 1);

    let rows: i64 = sqlx::query_scalar("select count(*) from event_subscriptions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
}

#[sqlx::test]
async fn subscribe_verify_fire_and_unsubscribe(pool: PgPool) {
    let token = token(&pool).await;
    let issue_id = seed_issue(&pool).await;
    let base = serve(pool.clone(), true).await;
    let (url, seen) = receiver(true).await;
    let key = thermite_core::mcp_events::decode_secret(SECRET).unwrap();

    // Subscribe: the receiver gets a signed verification first.
    let (status, _, body) = rpc(&base, &token, "events/subscribe", subscribe_params(&url)).await;
    assert_eq!(status, 200);
    let body = body.expect("no payload");
    let result = &body["result"];
    let id = result["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{body}"))
        .to_string();
    assert!(id.starts_with("sub_"));
    assert!(result["refreshBefore"].as_str().unwrap().ends_with('Z'));
    assert_eq!(result["cursor"], Value::Null);
    assert_eq!(result["truncated"], false);
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let (headers, body) = &seen[0];
        let message: Value = serde_json::from_slice(body).unwrap();
        assert_eq!(message["type"], "verification");
        assert_eq!(headers["x-mcp-subscription-id"], id.as_str());
        assert!(
            headers["webhook-id"]
                .to_str()
                .unwrap()
                .starts_with("msg_verification_")
        );
    }

    // A repeat subscribe is the same subscription.
    let (_, _, again) = rpc(&base, &token, "events/subscribe", subscribe_params(&url)).await;
    assert_eq!(again.unwrap()["result"]["id"], id.as_str());

    // Fire: one delivery, signed over the exact bytes received.
    let fired = fire(&base, &token, issue_id).await;
    assert_eq!(
        fired["deliveries"],
        json!([{ "id": id, "status": 200, "accepted": true }])
    );
    {
        let seen = seen.lock().unwrap();
        let (headers, body) = seen.last().unwrap();
        let header = |name: &str| headers[name].to_str().unwrap().to_string();
        let message: Value = serde_json::from_slice(body).unwrap();

        assert_eq!(header("content-type"), "application/json");
        assert_eq!(header("x-mcp-subscription-id"), id);
        assert_eq!(header("webhook-id"), message["eventId"].as_str().unwrap());
        let timestamp: i64 = header("webhook-timestamp").parse().unwrap();
        assert_eq!(
            header("webhook-signature"),
            thermite_core::mcp_events::sign(&key, &header("webhook-id"), timestamp, body)
        );

        assert_eq!(message["name"], "issue.created");
        assert_eq!(message["cursor"], Value::Null);
        assert_eq!(message["data"]["issue_id"], issue_id);
        assert_eq!(message["data"]["project"], "web");
        assert_eq!(message["data"]["title"], "ValueError: boom");
        assert_eq!(
            message["data"]["url"],
            format!("http://localhost:8099/issues/{issue_id}")
        );
    }

    // Unsubscribe, twice: idempotent, and nothing is delivered afterwards.
    let unsubscribe = json!({
        "name": "issue.created",
        "arguments": { "project": "web" },
        "delivery": { "mode": "webhook", "url": url }
    });
    for _ in 0..2 {
        let (_, _, body) = rpc(&base, &token, "events/unsubscribe", unsubscribe.clone()).await;
        assert_eq!(body.unwrap()["result"], json!({}));
    }
    let delivered = seen.lock().unwrap().len();
    let fired = fire(&base, &token, issue_id).await;
    assert_eq!(fired["deliveries"], json!([]));
    assert_eq!(seen.lock().unwrap().len(), delivered);
}
