//! MCP events spike: lets an MCP client (ChatGPT) subscribe to `issue.created` /
//! `issue.regressed` and receive a hand-fired event at a webhook. Behind `THERMITE_MCP_EVENTS`.
//!
//! The experimental `events/*` extension is served from three places:
//!
//! - **`events/list`, `events/subscribe`, `events/unsubscribe`** reach `McpTools::on_custom_request`
//!   as unknown methods and are answered by [`handle`], after the same authentication the tools do.
//! - **The capability** cannot be declared through rmcp: `ServerCapabilities` is closed and has no
//!   `events` field. [`advertise_events`] adds `"events": {}` to the `server/discover` result on
//!   the way out instead. rmcp answers discover as SSE (`text/event-stream`, one `data:` line per
//!   message) unless configured for JSON; both are handled.
//! - **`POST /api/v1/mcp-events/fire`** ([`fire_router`]) delivers one event for an issue to every
//!   live matching subscription. Nothing fires on its own yet: no digest hook, no outbox consumer,
//!   no retries. That is the follow-up if the live test earns it.
//!
//! Callbacks are the dangerous part: a subscriber names a URL and thermite POSTs to it. So a
//! callback must be HTTPS, every address its host resolves to must be public, the connection goes
//! to the address that was checked (the hostname kept for TLS), and redirects are never followed.
//! The check runs before every request, not only at subscribe time, so a host that later
//! re-points at a private address is refused then. There is no switch to turn it off.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Json, Router};
use chrono::{SecondsFormat, Utc};
use rmcp::ErrorData as McpError;
use rmcp::model::{CustomRequest, CustomResult, ErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thermite_core::AppError;
use thermite_core::mcp_events::{self as events, NewSubscription};

use crate::server::state::AppState;

/// Every delivery, verification included, gives up after this long.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(10);

/// How much of a verification response is read. The answer is one short JSON object.
const MAX_CHALLENGE_RESPONSE_BYTES: usize = 64 * 1024;

/// rmcp's own request-body ceiling: the discover shim buffers the body before rmcp sees it.
const MAX_MCP_REQUEST_BYTES: usize = 4 * 1024 * 1024;

// Error codes from the events extension.
const NOT_FOUND: ErrorCode = ErrorCode(-32011);
const UNSUPPORTED: ErrorCode = ErrorCode(-32014);
const CALLBACK_ENDPOINT_ERROR: ErrorCode = ErrorCode(-32015);

pub fn is_events_method(method: &str) -> bool {
    matches!(
        method,
        "events/list" | "events/subscribe" | "events/unsubscribe"
    )
}

/// Answers one `events/*` request for an authenticated `principal`.
pub async fn handle(
    state: &AppState,
    principal: &str,
    request: CustomRequest,
) -> Result<CustomResult, McpError> {
    let params = request.params.unwrap_or(Value::Null);
    let result = match request.method.as_str() {
        "events/list" => json!({ "events": events::definitions() }),
        "events/subscribe" => subscribe(state, principal, parse(params)?).await?,
        "events/unsubscribe" => unsubscribe(state, principal, parse(params)?).await?,
        other => {
            return Err(McpError::new(
                ErrorCode::METHOD_NOT_FOUND,
                other.to_string(),
                None,
            ));
        }
    };
    Ok(CustomResult::new(result))
}

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, McpError> {
    serde_json::from_value(params).map_err(|e| McpError::invalid_params(e.to_string(), None))
}

#[derive(Debug, Deserialize)]
struct SubscribeParams {
    name: String,
    #[serde(default)]
    arguments: Option<Value>,
    delivery: DeliveryParams,
    /// Requested lifetime. A float is tolerated because JSON has no integer type.
    #[serde(default, rename = "ttlMs")]
    ttl_ms: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct DeliveryParams {
    mode: String,
    url: String,
    #[serde(default)]
    secret: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UnsubscribeParams {
    name: String,
    #[serde(default)]
    arguments: Option<Value>,
    delivery: DeliveryParams,
}

/// Validates the event name, delivery mode and arguments, and returns the normal arguments.
fn check_request(name: &str, mode: &str, arguments: Option<&Value>) -> Result<Value, McpError> {
    if !events::EVENT_NAMES.contains(&name) {
        return Err(McpError::new(
            NOT_FOUND,
            format!("unknown event: {name}"),
            Some(json!({ "kind": "event" })),
        ));
    }
    if mode != "webhook" {
        return Err(McpError::new(
            UNSUPPORTED,
            format!("unsupported delivery mode: {mode}"),
            Some(json!({ "feature": "delivery.mode", "value": mode })),
        ));
    }
    events::normalize_arguments(arguments).map_err(|e| McpError::invalid_params(e, None))
}

async fn subscribe(
    state: &AppState,
    principal: &str,
    params: SubscribeParams,
) -> Result<Value, McpError> {
    let arguments = check_request(
        &params.name,
        &params.delivery.mode,
        params.arguments.as_ref(),
    )?;
    let url = params.delivery.url;
    let id = events::subscription_id(principal, &url, &params.name, &arguments);
    let db = &state.thermite.db;

    // A refresh may leave the secret out and keep the one already on file.
    let secret = match params.delivery.secret {
        Some(secret) => secret,
        None => events::stored_secret(db, &id, principal)
            .await
            .map_err(internal)?
            .and_then(|ciphertext| open_secret(state, &id, &ciphertext))
            .ok_or_else(|| McpError::invalid_params("delivery.secret is required", None))?,
    };
    let key = events::decode_secret(&secret).map_err(|e| McpError::invalid_params(e, None))?;

    let target = resolve_callback(&url, POLICY)
        .await
        .map_err(|e| McpError::invalid_params(format!("callback URL refused: {e}"), None))?;

    // Every subscribe verifies, refreshes included, and nothing is written until it passes.
    verify(&target, &id, &key).await.map_err(|failure| {
        McpError::new(
            CALLBACK_ENDPOINT_ERROR,
            format!("callback verification failed: {}", failure.reason),
            Some(json!({ "reason": failure.reason })),
        )
    })?;

    let ciphertext = seal_secret(state, &id, &secret).map_err(internal)?;
    let expires_at = events::upsert(
        db,
        &NewSubscription {
            id: &id,
            principal,
            event_name: &params.name,
            arguments: &arguments,
            callback_url: &url,
            secret_ciphertext: &ciphertext,
            ttl_seconds: events::ttl_seconds(params.ttl_ms.map(|ms| ms.max(0.0) as u64)),
        },
    )
    .await
    .map_err(internal)?;

    Ok(json!({
        "id": id,
        "refreshBefore": expires_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        "cursor": null,
        "truncated": false,
    }))
}

async fn unsubscribe(
    state: &AppState,
    principal: &str,
    params: UnsubscribeParams,
) -> Result<Value, McpError> {
    let arguments = check_request(
        &params.name,
        &params.delivery.mode,
        params.arguments.as_ref(),
    )?;
    let id = events::subscription_id(principal, &params.delivery.url, &params.name, &arguments);
    events::unsubscribe(&state.thermite.db, &id, principal)
        .await
        .map_err(internal)?;
    Ok(json!({}))
}

fn internal<E: std::fmt::Display>(error: E) -> McpError {
    McpError::internal_error(error.to_string(), None)
}

// ---------------------------------------------------------------------------
// Secrets at rest
// ---------------------------------------------------------------------------

/// `ENCRYPTION_KEY` when the instance has one, otherwise a key derived from `SESSION_SECRET`.
/// Changing either makes stored secrets unreadable; a subscriber's next refresh with its secret
/// repairs the row.
fn secret_key(state: &AppState) -> [u8; 32] {
    state
        .secrets
        .encryption_key
        .unwrap_or_else(|| events::derive_secret_key(&state.secrets.session_secret))
}

/// Encrypts a signing secret bound to its subscription id. The pinned dx-crypto has no
/// associated-data API, so the id is sealed inside the authenticated plaintext and checked on
/// open: a ciphertext copied onto another row fails exactly as AAD would make it.
fn seal_secret(state: &AppState, id: &str, secret: &str) -> Result<String, crypto::Error> {
    crypto::encryption::encrypt_secret(&format!("{id}\n{secret}"), &secret_key(state))
}

fn open_secret(state: &AppState, id: &str, ciphertext: &str) -> Option<String> {
    let plaintext = crypto::encryption::decrypt_secret(ciphertext, &secret_key(state)).ok()?;
    plaintext
        .strip_prefix(id)?
        .strip_prefix('\n')
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// Callback address guard
// ---------------------------------------------------------------------------

/// Which callbacks may be dialled. Production code can only ever get [`Policy::PublicHttps`]:
/// the permissive variant does not exist outside test builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Policy {
    PublicHttps,
    /// Also plain HTTP and loopback, for tests that run their own receiver.
    #[cfg(test)]
    LoopbackForTests,
}

#[cfg(not(test))]
const POLICY: Policy = Policy::PublicHttps;
#[cfg(test)]
const POLICY: Policy = Policy::LoopbackForTests;

/// Whether `ip` is a public unicast address a callback may be dialled at.
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_public_v4(v4),
            None => is_public_v6(v6),
        },
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0                                   // "this network", incl. unspecified
        || a == 10                             // RFC 1918
        || a == 127                            // loopback
        || (a == 100 && (64..128).contains(&b)) // CGNAT, RFC 6598
        || (a == 169 && b == 254)              // link-local
        || (a == 172 && (16..32).contains(&b)) // RFC 1918
        || (a == 192 && b == 0 && c == 0)      // IETF protocol assignments
        || (a == 192 && b == 0 && c == 2)      // TEST-NET-1
        || (a == 192 && b == 88 && c == 99)    // 6to4 relay anycast
        || (a == 192 && b == 168)              // RFC 1918
        || (a == 198 && (18..20).contains(&b)) // benchmarking
        || (a == 198 && b == 51 && c == 100)   // TEST-NET-2
        || (a == 203 && b == 0 && c == 113)    // TEST-NET-3
        || a >= 224) // multicast, reserved, broadcast
}

/// Only global unicast (`2000::/3`) is public, minus the ranges inside it that are not: this
/// rejects loopback, unspecified, unique-local, link-local, multicast, NAT64 and v4-compatible
/// addresses by construction.
fn is_public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    (s[0] & 0xe000) == 0x2000
        && !(s[0] == 0x2001 && s[1] == 0x0db8) // documentation
        && !(s[0] == 0x2001 && s[1] == 0x0000) // Teredo
        && !(s[0] == 0x2001 && s[1] == 0x0002 && s[2] == 0) // benchmarking
        && !(s[0] == 0x2001 && (s[1] & 0xfff0) == 0x0010) // ORCHID
        && s[0] != 0x2002 // 6to4: embeds an arbitrary IPv4 address
}

fn allowed_ip(ip: IpAddr, policy: Policy) -> bool {
    match policy {
        Policy::PublicHttps => is_public_ip(ip),
        #[cfg(test)]
        Policy::LoopbackForTests => is_public_ip(ip) || ip.is_loopback(),
    }
}

/// A callback that passed the guard: where to connect, and the name to present for TLS.
#[derive(Debug)]
struct Target {
    url: reqwest::Url,
    /// `None` for an IP-literal URL, which needs no resolver override.
    domain: Option<String>,
    /// Every address the host resolved to, all of them checked. The connector may use any.
    addrs: Vec<SocketAddr>,
}

/// Parses and checks a callback URL, resolving its host. Every resolved address must pass, not
/// just the one used: otherwise a host with one public and one private record passes the check
/// and is then dialled at whichever the connector picks.
async fn resolve_callback(raw: &str, policy: Policy) -> Result<Target, String> {
    let url = reqwest::Url::parse(raw).map_err(|_| "not a valid URL".to_string())?;
    let scheme_ok = match policy {
        Policy::PublicHttps => url.scheme() == "https",
        #[cfg(test)]
        Policy::LoopbackForTests => matches!(url.scheme(), "https" | "http"),
    };
    if !scheme_ok {
        return Err("must use https".to_string());
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "no port".to_string())?;

    let (domain, addrs): (Option<String>, Vec<SocketAddr>) = match url.host() {
        Some(url::Host::Ipv4(ip)) => (None, vec![SocketAddr::new(ip.into(), port)]),
        Some(url::Host::Ipv6(ip)) => (None, vec![SocketAddr::new(ip.into(), port)]),
        Some(url::Host::Domain(domain)) => {
            let addrs = tokio::net::lookup_host((domain, port))
                .await
                .map_err(|_| "host does not resolve".to_string())?
                .collect();
            (Some(domain.to_string()), addrs)
        }
        None => return Err("no host".to_string()),
    };

    if addrs.is_empty() {
        return Err("host does not resolve".to_string());
    }
    if addrs.iter().any(|a| !allowed_ip(a.ip(), policy)) {
        return Err("host resolves to a non-public address".to_string());
    }
    Ok(Target { url, domain, addrs })
}

// ---------------------------------------------------------------------------
// Delivery
// ---------------------------------------------------------------------------

/// Why a callback request failed, in the extension's `reason` vocabulary.
#[derive(Debug)]
struct Failure {
    reason: &'static str,
}

impl Failure {
    fn from_reqwest(error: &reqwest::Error) -> Self {
        let reason = if error.is_timeout() {
            "timeout"
        } else if format!("{error:?}")
            .to_ascii_lowercase()
            .contains("certificate")
        {
            "tls_error"
        } else {
            "connection_refused"
        };
        Self { reason }
    }

    fn from_status(status: StatusCode) -> Self {
        let reason = if status.is_server_error() {
            "http_5xx"
        } else {
            "http_4xx"
        };
        Self { reason }
    }
}

/// POSTs `body` to the checked address, signed, following no redirects and using no proxy.
async fn post_signed(
    target: &Target,
    subscription_id: &str,
    message_id: &str,
    key: &[u8],
    body: Vec<u8>,
) -> Result<reqwest::Response, Failure> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(CALLBACK_TIMEOUT);
    if let Some(domain) = &target.domain {
        builder = builder.resolve_to_addrs(domain, &target.addrs);
    }
    let client = builder.build().map_err(|e| Failure::from_reqwest(&e))?;

    let timestamp = Utc::now().timestamp();
    let signature = events::sign(key, message_id, timestamp, &body);
    client
        .post(target.url.clone())
        .header(header::CONTENT_TYPE, "application/json")
        .header("webhook-id", message_id)
        .header("webhook-timestamp", timestamp.to_string())
        .header("webhook-signature", signature)
        .header("X-MCP-Subscription-Id", subscription_id)
        .body(body)
        .send()
        .await
        .map_err(|e| Failure::from_reqwest(&e))
}

/// The callback must answer the single-use challenge with a 2xx echoing it.
async fn verify(target: &Target, subscription_id: &str, key: &[u8]) -> Result<(), Failure> {
    let challenge_failed = Failure {
        reason: "challenge_failed",
    };
    let token = || crypto::generate_url_safe_token(32).expect("the OS random source is available");
    let challenge = token();
    let message_id = format!("msg_verification_{}", token());
    let body = serde_json::to_vec(&json!({ "type": "verification", "challenge": challenge }))
        .expect("a JSON object serialises");

    let mut response = post_signed(target, subscription_id, &message_id, key, body).await?;
    if !response.status().is_success() {
        return Err(if response.status().is_redirection() {
            challenge_failed
        } else {
            Failure::from_status(response.status())
        });
    }

    let mut answer = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| Failure::from_reqwest(&e))?
    {
        answer.extend_from_slice(&chunk);
        if answer.len() > MAX_CHALLENGE_RESPONSE_BYTES {
            return Err(challenge_failed);
        }
    }
    let echoed = serde_json::from_slice::<Value>(&answer).ok().and_then(|v| {
        v.get("challenge")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    match echoed {
        Some(echoed) if events::constant_time_eq(echoed.as_bytes(), challenge.as_bytes()) => Ok(()),
        _ => Err(challenge_failed),
    }
}

#[derive(Debug, Deserialize)]
struct FireRequest {
    issue_id: i64,
    name: String,
}

#[derive(Debug, Serialize)]
struct DeliveryResult {
    id: String,
    /// The callback's HTTP status; `null` when no response came back.
    status: Option<u16>,
    accepted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// `POST /api/v1/mcp-events/fire`, behind the same API authentication as the rest of `/api/v1`.
pub fn fire_router() -> Router {
    Router::new()
        .route("/api/v1/mcp-events/fire", post(fire))
        .layer(axum::middleware::from_fn(
            crate::server::thermite::require_api_auth,
        ))
}

/// Delivers one event for an issue to every live subscription that covers it, once each.
async fn fire(
    Extension(state): Extension<AppState>,
    Json(request): Json<FireRequest>,
) -> Result<Json<Value>, AppError> {
    if !events::EVENT_NAMES.contains(&request.name.as_str()) {
        return Err(AppError::BadRequest(format!(
            "unknown event: {}",
            request.name
        )));
    }
    let db = &state.thermite.db;
    let summary = events::issue_summary(db, request.issue_id).await?;
    let now = Utc::now();
    let event_id = format!("evt_{}", uuid::Uuid::new_v4().simple());

    // Serialised once: every subscriber is signed over, and sent, these exact bytes.
    let body = serde_json::to_vec(&json!({
        "eventId": event_id,
        "name": request.name,
        "timestamp": now.to_rfc3339_opts(SecondsFormat::Secs, true),
        "data": events::payload(&summary, &state.config.base_url),
        "cursor": null,
    }))
    .expect("a JSON object serialises");
    if body.len() > events::MAX_PAYLOAD_BYTES {
        return Err(AppError::BadRequest(
            "event payload exceeds 256 KiB".to_string(),
        ));
    }

    let mut deliveries = Vec::new();
    for subscription in events::matching(db, &request.name, &summary.project).await? {
        let result = deliver(&state, &subscription, &event_id, &body).await;
        if result.status == Some(StatusCode::GONE.as_u16()) {
            events::delete(db, &subscription.id).await?;
        }
        deliveries.push(result);
    }

    Ok(Json(
        json!({ "event_id": event_id, "deliveries": deliveries }),
    ))
}

async fn deliver(
    state: &AppState,
    subscription: &events::Subscription,
    event_id: &str,
    body: &[u8],
) -> DeliveryResult {
    let failed = |error: String| DeliveryResult {
        id: subscription.id.clone(),
        status: None,
        accepted: false,
        error: Some(error),
    };
    let Some(key) = open_secret(state, &subscription.id, &subscription.secret_ciphertext)
        .and_then(|secret| events::decode_secret(&secret).ok())
    else {
        return failed("stored secret is unreadable".to_string());
    };
    // Re-checked on every delivery: the host may point somewhere else than at subscribe time.
    let target = match resolve_callback(&subscription.callback_url, POLICY).await {
        Ok(target) => target,
        Err(e) => return failed(format!("callback URL refused: {e}")),
    };
    match post_signed(&target, &subscription.id, event_id, &key, body.to_vec()).await {
        Ok(response) => DeliveryResult {
            id: subscription.id.clone(),
            status: Some(response.status().as_u16()),
            accepted: response.status().is_success(),
            error: None,
        },
        Err(failure) => failed(failure.reason.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Capability advertisement
// ---------------------------------------------------------------------------

/// Adds `"events": {}` to the capabilities of a `server/discover` result.
///
/// Every other request passes through untouched; only a discover response is buffered, and it
/// is one message long.
pub async fn advertise_events(request: Request, next: Next) -> Response {
    let (parts, body) = request.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_MCP_REQUEST_BYTES).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let is_discover = serde_json::from_slice::<Value>(&bytes)
        .is_ok_and(|message| message.get("method") == Some(&json!("server/discover")));

    let response = next
        .run(Request::from_parts(parts, Body::from(bytes)))
        .await;
    if !is_discover {
        return response;
    }

    let (mut parts, body) = response.into_parts();
    let bytes =
        match tokio::time::timeout(CALLBACK_TIMEOUT, axum::body::to_bytes(body, 1024 * 1024)).await
        {
            Ok(Ok(bytes)) => bytes,
            _ => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
    let is_sse = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"));
    let patched = if is_sse {
        patch_sse(&bytes)
    } else {
        patch_json(&bytes)
    };
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(patched))
}

/// Inserts the capability into a JSON-RPC response carrying `result.capabilities`.
fn add_capability(message: &mut Value) -> bool {
    match message
        .pointer_mut("/result/capabilities")
        .and_then(Value::as_object_mut)
    {
        Some(capabilities) => {
            capabilities.entry("events").or_insert_with(|| json!({}));
            true
        }
        None => false,
    }
}

fn patch_json(body: &Bytes) -> Bytes {
    if let Ok(mut message) = serde_json::from_slice::<Value>(body)
        && add_capability(&mut message)
    {
        return Bytes::from(message.to_string());
    }
    body.clone()
}

/// Rewrites every SSE `data:` line that is a discover result; all other lines (the empty
/// priming event, ids, keep-alive comments) pass through byte for byte.
fn patch_sse(body: &Bytes) -> Bytes {
    let Ok(text) = std::str::from_utf8(body) else {
        return body.clone();
    };
    let mut out = String::with_capacity(text.len() + 16);
    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        let ending = &line[content.len()..];
        if let Some(data) = content.strip_prefix("data:")
            && let Ok(mut message) = serde_json::from_str::<Value>(data.trim_start())
            && add_capability(&mut message)
        {
            out.push_str("data: ");
            out.push_str(&message.to_string());
            out.push_str(ending);
        } else {
            out.push_str(line);
        }
    }
    Bytes::from(out)
}

/// The caller's identity for subscription scoping: opaque to thermite-core.
pub fn principal(user_id: uuid::Uuid) -> String {
    format!("user:{user_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn non_public_addresses_are_refused() {
        for s in [
            "0.0.0.0",
            "127.0.0.1",
            "127.1.2.3",
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "100.127.255.255",
            "192.0.2.1",
            "198.18.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.169.254",
            "64:ff9b::a00:1",
            "2001:db8::1",
            "2002:a00:1::1",
        ] {
            assert!(!is_public_ip(ip(s)), "{s} must be refused");
        }
    }

    #[test]
    fn public_addresses_pass() {
        for s in [
            "1.1.1.1",
            "8.8.8.8",
            "100.63.255.255",
            "100.128.0.1",
            "172.15.255.255",
            "172.32.0.1",
            "2606:4700:4700::1111",
            "::ffff:1.1.1.1",
        ] {
            assert!(is_public_ip(ip(s)), "{s} must pass");
        }
    }

    #[tokio::test]
    async fn the_production_policy_refuses_http_and_loopback() {
        for url in [
            "http://example.com/cb",
            "https://127.0.0.1/cb",
            "https://[::1]/cb",
            "https://10.0.0.1/cb",
            "https://localhost/cb",
            "not a url",
        ] {
            assert!(
                resolve_callback(url, Policy::PublicHttps).await.is_err(),
                "{url} must be refused"
            );
        }
        let target = resolve_callback("https://1.1.1.1:8443/cb", Policy::PublicHttps)
            .await
            .unwrap();
        assert_eq!(target.addrs, vec!["1.1.1.1:8443".parse().unwrap()]);
        assert!(target.domain.is_none());
    }

    #[test]
    fn discover_is_patched_in_both_framings() {
        let message = json!({
            "jsonrpc": "2.0", "id": 1,
            "result": { "capabilities": { "tools": {} }, "supportedVersions": ["2026-07-28"] }
        });

        let json_body = patch_json(&Bytes::from(message.to_string()));
        let parsed: Value = serde_json::from_slice(&json_body).unwrap();
        assert_eq!(parsed["result"]["capabilities"]["events"], json!({}));
        assert_eq!(parsed["result"]["capabilities"]["tools"], json!({}));

        let sse = format!("data:\nid: 0\n\ndata: {message}\r\n\n: keep-alive\n");
        let patched = String::from_utf8(patch_sse(&Bytes::from(sse)).to_vec()).unwrap();
        assert!(patched.starts_with("data:\nid: 0\n\n"), "{patched}");
        assert!(patched.ends_with("\r\n\n: keep-alive\n"), "{patched}");
        let data = patched
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .next()
            .unwrap();
        let parsed: Value = serde_json::from_str(data.trim()).unwrap();
        assert_eq!(parsed["result"]["capabilities"]["events"], json!({}));
    }
}

#[cfg(test)]
#[path = "mcp_events_tests.rs"]
mod router_tests;
