//! MCP event subscriptions: the storage and pure logic behind the experimental `events/*`
//! extension (webhook delivery, Standard Webhooks signing).
//!
//! A spike: an MCP client subscribes to `issue.created` or `issue.regressed`, and the application
//! delivers a summary of an issue to its callback when an operator fires one by hand. Nothing
//! here decides *when* to fire, and nothing here talks HTTP or knows who the caller is — the
//! principal is an opaque string the application passes in, and delivery is the application's.
//!
//! What lives here is the part that has to agree with the client byte for byte: the
//! deterministic subscription id, the canonical form of the arguments it is derived from, the
//! bounds on the signing secret, and the signature itself.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use crate::error::{AppError, AppResult};

pub const ISSUE_CREATED: &str = "issue.created";
pub const ISSUE_REGRESSED: &str = "issue.regressed";

/// Every event this server offers.
pub const EVENT_NAMES: [&str; 2] = [ISSUE_CREATED, ISSUE_REGRESSED];

/// The largest delivery body the extension allows.
pub const MAX_PAYLOAD_BYTES: usize = 256 * 1024;

const DEFAULT_TTL_MS: u64 = 24 * 60 * 60 * 1000;
const MIN_TTL_MS: u64 = 60 * 60 * 1000;
const MAX_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// The decoded signing secret must be 24 to 64 bytes.
const MIN_SECRET_BYTES: usize = 24;
const MAX_SECRET_BYTES: usize = 64;

/// The `events/list` definitions.
///
/// `data` is a summary on purpose: the subscriber fetches the rest with the `get_issue` tool,
/// and nothing in a payload addresses the model.
pub fn definitions() -> Value {
    let input_schema = json!({
        "type": "object",
        "properties": {
            "project": {
                "type": "string",
                "description": "Project slug, as shown by list_projects. Omit for every project."
            }
        },
        "additionalProperties": false
    });
    let payload_schema = json!({
        "type": "object",
        "properties": {
            "issue_id": { "type": "integer" },
            "project": { "type": "string" },
            "title": { "type": "string" },
            "culprit": { "type": ["string", "null"] },
            "level": { "type": "string" },
            "release": { "type": ["string", "null"] },
            "url": { "type": "string" }
        },
        "required": ["issue_id", "project", "title", "culprit", "level", "release", "url"],
        "additionalProperties": false
    });

    json!([
        {
            "name": ISSUE_CREATED,
            "description": "An error was seen for the first time and opened a new issue.",
            "delivery": ["webhook"],
            "inputSchema": input_schema,
            "payloadSchema": payload_schema,
        },
        {
            "name": ISSUE_REGRESSED,
            "description": "A resolved issue happened again and was reopened as a regression.",
            "delivery": ["webhook"],
            "inputSchema": input_schema,
            "payloadSchema": payload_schema,
        }
    ])
}

/// Validates subscription arguments against the input schema and returns their normal form.
///
/// Absent, `null` and `{}` are the same subscription (every project), and so is a `project` of
/// `null` — normalising here is what makes them derive the same id.
pub fn normalize_arguments(arguments: Option<&Value>) -> Result<Value, String> {
    let object = match arguments {
        None | Some(Value::Null) => return Ok(json!({})),
        Some(Value::Object(object)) => object,
        Some(_) => return Err("arguments must be an object".to_string()),
    };

    let mut normal = Map::new();
    for (key, value) in object {
        match (key.as_str(), value) {
            ("project", Value::Null) => {}
            ("project", Value::String(slug)) if !slug.is_empty() => {
                normal.insert("project".to_string(), Value::String(slug.clone()));
            }
            ("project", _) => return Err("project must be a non-empty string".to_string()),
            (other, _) => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Value::Object(normal))
}

/// Serialises `value` with object keys sorted at every depth, so two argument objects that are
/// equal as JSON are equal as text whatever order their keys arrived in.
pub fn canonical_json(value: &Value) -> String {
    fn write(value: &Value, out: &mut String) {
        match value {
            Value::Object(object) => {
                let mut keys: Vec<&String> = object.keys().collect();
                keys.sort();
                out.push('{');
                for (i, key) in keys.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&Value::String(key.clone()).to_string());
                    out.push(':');
                    write(&object[key], out);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            scalar => out.push_str(&scalar.to_string()),
        }
    }

    let mut out = String::new();
    write(value, &mut out);
    out
}

/// The subscription id: a truncated SHA-256 over (principal, callback URL, event name,
/// canonical arguments). Deterministic, so a repeated subscribe refreshes one row; and
/// principal-scoped, so one caller can never address another's subscription.
///
/// Each field is length-prefixed, so no choice of values can shift a boundary between two of
/// them and collide with a different key.
pub fn subscription_id(
    principal: &str,
    callback_url: &str,
    name: &str,
    arguments: &Value,
) -> String {
    let mut hasher = Sha256::new();
    for field in [principal, callback_url, name, &canonical_json(arguments)] {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field.as_bytes());
    }
    let digest = hasher.finalize();
    let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
    format!("sub_{hex}")
}

/// Decodes a `whsec_<base64>` secret into the HMAC key, enforcing the 24–64 byte bound.
pub fn decode_secret(secret: &str) -> Result<Vec<u8>, String> {
    let encoded = secret
        .strip_prefix("whsec_")
        .ok_or_else(|| "secret must start with whsec_".to_string())?;
    let key = BASE64
        .decode(encoded)
        .map_err(|_| "secret is not valid base64".to_string())?;
    if !(MIN_SECRET_BYTES..=MAX_SECRET_BYTES).contains(&key.len()) {
        return Err(format!(
            "secret must decode to {MIN_SECRET_BYTES}-{MAX_SECRET_BYTES} bytes, got {}",
            key.len()
        ));
    }
    Ok(key)
}

/// The Standard Webhooks `webhook-signature` value: `v1,` + base64 HMAC-SHA256 over
/// `"{id}.{timestamp}.{body}"`. `body` must be the exact bytes sent.
pub fn sign(key: &[u8], message_id: &str, timestamp: i64, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts a key of any length");
    mac.update(message_id.as_bytes());
    mac.update(b".");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    format!("v1,{}", BASE64.encode(mac.finalize().into_bytes()))
}

/// Equality whose running time does not depend on where the inputs first differ.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A 32-byte key for encrypting stored signing secrets, derived from the session secret when
/// the instance has no dedicated encryption key. The label keeps it from ever equalling a key
/// derived from the same secret for anything else.
pub fn derive_secret_key(session_secret: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"thermite/mcp-events/subscription-secret/v1\0");
    hasher.update(session_secret);
    hasher.finalize().into()
}

/// The lease a subscription is granted, in seconds: 24 hours by default, a requested `ttlMs`
/// clamped to between one hour and seven days.
pub fn ttl_seconds(requested_ms: Option<u64>) -> i64 {
    let ms = requested_ms.map_or(DEFAULT_TTL_MS, |ms| ms.clamp(MIN_TTL_MS, MAX_TTL_MS));
    (ms / 1000) as i64
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

/// A subscription to write. Written only after its callback passed verification.
#[derive(Debug)]
pub struct NewSubscription<'a> {
    pub id: &'a str,
    pub principal: &'a str,
    pub event_name: &'a str,
    /// In the form [`normalize_arguments`] returns.
    pub arguments: &'a Value,
    pub callback_url: &'a str,
    pub secret_ciphertext: &'a str,
    pub ttl_seconds: i64,
}

/// Creates the subscription or refreshes it: a new expiry and the secret replaced. Returns the
/// new expiry.
pub async fn upsert(db: &PgPool, new: &NewSubscription<'_>) -> AppResult<DateTime<Utc>> {
    let expires_at: Option<DateTime<Utc>> = sqlx::query_scalar(
        "insert into event_subscriptions
                (id, principal, event_name, arguments, callback_url, secret_ciphertext, expires_at)
         values ($1, $2, $3, $4, $5, $6, now() + make_interval(secs => $7))
         on conflict (id) do update
            set secret_ciphertext = excluded.secret_ciphertext,
                expires_at        = excluded.expires_at,
                updated_at        = now()
          where event_subscriptions.principal = excluded.principal
         returning expires_at",
    )
    .bind(new.id)
    .bind(new.principal)
    .bind(new.event_name)
    .bind(new.arguments)
    .bind(new.callback_url)
    .bind(new.secret_ciphertext)
    .bind(new.ttl_seconds as f64)
    .fetch_optional(db)
    .await?;

    // Only a hash collision across principals lands here.
    expires_at.ok_or_else(|| AppError::BadRequest("subscription id conflict".to_string()))
}

/// The stored secret of a subscription, for a refresh that did not send one.
pub async fn stored_secret(db: &PgPool, id: &str, principal: &str) -> AppResult<Option<String>> {
    Ok(sqlx::query_scalar(
        "select secret_ciphertext from event_subscriptions where id = $1 and principal = $2",
    )
    .bind(id)
    .bind(principal)
    .fetch_optional(db)
    .await?)
}

/// Removes a subscription of `principal`'s. Idempotent: removing one that does not exist, or
/// that belongs to someone else, succeeds and changes nothing.
pub async fn unsubscribe(db: &PgPool, id: &str, principal: &str) -> AppResult<()> {
    sqlx::query("delete from event_subscriptions where id = $1 and principal = $2")
        .bind(id)
        .bind(principal)
        .execute(db)
        .await?;
    Ok(())
}

/// Removes a subscription whose callback answered `410 Gone`.
pub async fn delete(db: &PgPool, id: &str) -> AppResult<()> {
    sqlx::query("delete from event_subscriptions where id = $1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// A live subscription, as delivery needs it.
#[derive(Debug, sqlx::FromRow)]
pub struct Subscription {
    pub id: String,
    pub callback_url: String,
    pub secret_ciphertext: String,
}

/// Every unexpired subscription to `event_name` that covers `project`.
pub async fn matching(
    db: &PgPool,
    event_name: &str,
    project: &str,
) -> AppResult<Vec<Subscription>> {
    Ok(sqlx::query_as(
        "select id, callback_url, secret_ciphertext
           from event_subscriptions
          where event_name = $1
            and expires_at > now()
            and (arguments->>'project' is null or arguments->>'project' = $2)
          order by created_at, id",
    )
    .bind(event_name)
    .bind(project)
    .fetch_all(db)
    .await?)
}

/// The `data` of a delivered event, minus the URL the application adds.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct IssueSummary {
    pub issue_id: i64,
    pub project: String,
    pub title: String,
    pub culprit: Option<String>,
    pub level: String,
    /// From the most recent event, as the alert payload reads it.
    pub release: Option<String>,
}

pub async fn issue_summary(db: &PgPool, issue_id: i64) -> AppResult<IssueSummary> {
    sqlx::query_as(
        "select i.id as issue_id, p.slug as project, i.title, i.culprit, i.level,
                (select e.release from events e where e.issue_id = i.id
                  order by e.timestamp desc, e.id desc limit 1) as release
           from issues i
           join projects p on p.id = i.project_id
          where i.id = $1",
    )
    .bind(issue_id)
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound)
}

/// The event's `data`: the summary plus a link to the issue page.
pub fn payload(summary: &IssueSummary, base_url: &str) -> Value {
    json!({
        "issue_id": summary.issue_id,
        "project": summary.project,
        "title": summary.title,
        "culprit": summary.culprit,
        "level": summary.level,
        "release": summary.release,
        "url": format!("{}/issues/{}", base_url.trim_end_matches('/'), summary.issue_id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_order_does_not_change_the_id() {
        let a: Value =
            serde_json::from_str(r#"{"project":"web","x":{"b":1,"a":[2,{"d":3,"c":4}]}}"#).unwrap();
        let b: Value =
            serde_json::from_str(r#"{"x":{"a":[2,{"c":4,"d":3}],"b":1},"project":"web"}"#).unwrap();
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(
            subscription_id("user:1", "https://cb.test/x", ISSUE_CREATED, &a),
            subscription_id("user:1", "https://cb.test/x", ISSUE_CREATED, &b),
        );
    }

    #[test]
    fn every_part_of_the_key_changes_the_id() {
        let args = json!({ "project": "web" });
        let base = subscription_id("user:1", "https://cb.test/x", ISSUE_CREATED, &args);
        assert!(base.starts_with("sub_") && base.len() == 4 + 32, "{base}");
        for other in [
            subscription_id("user:2", "https://cb.test/x", ISSUE_CREATED, &args),
            subscription_id("user:1", "https://cb.test/y", ISSUE_CREATED, &args),
            subscription_id("user:1", "https://cb.test/x", ISSUE_REGRESSED, &args),
            subscription_id("user:1", "https://cb.test/x", ISSUE_CREATED, &json!({})),
        ] {
            assert_ne!(base, other);
        }
    }

    #[test]
    fn absent_null_and_empty_arguments_are_one_subscription() {
        let normal = json!({});
        assert_eq!(normalize_arguments(None).unwrap(), normal);
        assert_eq!(normalize_arguments(Some(&Value::Null)).unwrap(), normal);
        assert_eq!(normalize_arguments(Some(&json!({}))).unwrap(), normal);
        assert_eq!(
            normalize_arguments(Some(&json!({ "project": null }))).unwrap(),
            normal
        );
        assert_eq!(
            normalize_arguments(Some(&json!({ "project": "web" }))).unwrap(),
            json!({ "project": "web" })
        );
        assert!(normalize_arguments(Some(&json!({ "project": 3 }))).is_err());
        assert!(normalize_arguments(Some(&json!({ "project": "" }))).is_err());
        assert!(normalize_arguments(Some(&json!({ "other": "x" }))).is_err());
        assert!(normalize_arguments(Some(&json!(["web"]))).is_err());
    }

    #[test]
    fn secret_length_is_bounded() {
        let secret = |n: usize| format!("whsec_{}", BASE64.encode(vec![7u8; n]));
        assert!(decode_secret(&secret(23)).is_err());
        assert_eq!(decode_secret(&secret(24)).unwrap().len(), 24);
        assert_eq!(decode_secret(&secret(64)).unwrap().len(), 64);
        assert!(decode_secret(&secret(65)).is_err());
        assert!(
            decode_secret(&BASE64.encode([7u8; 32])).is_err(),
            "prefix is required"
        );
        assert!(decode_secret("whsec_not base64!").is_err());
    }

    /// The Standard Webhooks reference vector (standard-webhooks' own test suite).
    #[test]
    fn signature_matches_the_standard_webhooks_vector() {
        let key = decode_secret("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw").unwrap();
        assert_eq!(
            sign(
                &key,
                "msg_p5jXN8AQM9LWM0D4loKWxJek",
                1614265330,
                br#"{"test": 2432232314}"#
            ),
            "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE="
        );
    }

    #[test]
    fn constant_time_eq_is_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    #[test]
    fn ttl_defaults_and_clamps() {
        assert_eq!(ttl_seconds(None), 24 * 3600);
        assert_eq!(ttl_seconds(Some(1)), 3600);
        assert_eq!(ttl_seconds(Some(2 * 3600 * 1000)), 2 * 3600);
        assert_eq!(ttl_seconds(Some(u64::MAX)), 7 * 24 * 3600);
    }

    #[test]
    fn derived_key_is_stable_and_secret_dependent() {
        assert_eq!(derive_secret_key(b"a"), derive_secret_key(b"a"));
        assert_ne!(derive_secret_key(b"a"), derive_secret_key(b"b"));
    }
}
