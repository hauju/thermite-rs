//! Accessors over a raw Sentry event payload.
//!
//! The event schema is very large and mostly irrelevant to us: we need a handful of fields for
//! grouping and for the indexed columns, and everything else is stored and served verbatim. So
//! rather than modelling the schema, these functions read what they need out of a
//! `serde_json::Value`, the way Bugsink's `get_path` helpers do.

use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;
use uuid::Uuid;

/// Maximum length of an exception type, matching Sentry's trim.
const MAX_TYPE_LEN: usize = 128;
/// Maximum length of an exception value or log message, matching Sentry's trim.
const MAX_VALUE_LEN: usize = 1024;

/// A clock this far ahead of ours is wrong. Without this, one client with a broken clock pins its
/// issue to the top of a `last_seen`-ordered list indefinitely.
const MAX_CLOCK_SKEW_SECS: i64 = 3600;

/// How far into the past a client-supplied timestamp is honoured (Sentry rejects events older
/// than 30 days outright). The timestamp mints hourly `event_counts` buckets, so without a floor
/// one authenticated client could backdate its way to millions of permanent rollup rows per
/// issue — and rows bucketed before the retention window would never be charted anyway.
const MAX_BACKDATE_DAYS: i64 = 30;

/// The event's id, or a fresh one when it is missing or unparseable.
///
/// Never fails: an event with a broken id is still worth keeping, and the ingest response has to
/// return an id regardless.
pub fn event_id(event: &Value) -> Uuid {
    event
        .get("event_id")
        .and_then(Value::as_str)
        .and_then(|raw| Uuid::parse_str(raw).ok())
        .unwrap_or_else(Uuid::new_v4)
}

/// When the error happened, per the SDK, bounded to (received_at - 30 days, received_at + 1h].
///
/// Falls back to `received_at` when absent, unparseable, or outside those bounds.
pub fn timestamp(event: &Value, received_at: DateTime<Utc>) -> DateTime<Utc> {
    clamp(parse_timestamp(event.get("timestamp")), received_at)
}

/// Reads a Sentry timestamp: epoch seconds, or a datetime string with or without a zone.
///
/// Shared with session items, which carry `started` and `timestamp` in the same encodings.
pub fn parse_timestamp(value: Option<&Value>) -> Option<DateTime<Utc>> {
    match value {
        // Epoch seconds, integer or float.
        Some(Value::Number(n)) => n
            .as_f64()
            .map(|secs| Utc.timestamp_nanos((secs * 1e9) as i64)),
        Some(Value::String(s)) => parse_timestamp_str(s),
        _ => None,
    }
}

/// Bounds a client-supplied timestamp to `(received_at - 30 days, received_at + 1h]`.
///
/// Falls back to `received_at` when absent or outside those bounds. Every client-supplied time
/// that mints an hourly rollup bucket has to come through here — `event_counts` and
/// `session_counts` both — or one authenticated client can backdate its way to millions of
/// permanent rows, and rows bucketed outside the retention window are never swept.
pub fn clamp(parsed: Option<DateTime<Utc>>, received_at: DateTime<Utc>) -> DateTime<Utc> {
    match parsed {
        Some(ts)
            if ts <= received_at + chrono::Duration::seconds(MAX_CLOCK_SKEW_SECS)
                && ts >= received_at - chrono::Duration::days(MAX_BACKDATE_DAYS) =>
        {
            ts
        }
        _ => received_at,
    }
}

fn parse_timestamp_str(raw: &str) -> Option<DateTime<Utc>> {
    if let Ok(ts) = DateTime::parse_from_rfc3339(raw) {
        return Some(ts.with_timezone(&Utc));
    }
    // Some SDKs omit the timezone; Sentry reads those as UTC.
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(raw, format) {
            return Some(naive.and_utc());
        }
    }
    // Epoch seconds sent as a string.
    raw.parse::<f64>()
        .ok()
        .map(|secs| Utc.timestamp_nanos((secs * 1e9) as i64))
}

/// Severity. Defaults to `error`, per the event payload spec.
pub fn level(event: &Value) -> &str {
    match event.get("level").and_then(Value::as_str) {
        Some(level @ ("fatal" | "error" | "warning" | "info" | "debug")) => level,
        _ => "error",
    }
}

/// Reads a top-level string field, treating empty strings as absent.
pub fn str_field<'a>(event: &'a Value, key: &str) -> Option<&'a str> {
    event
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Normalizes Sentry's three interchangeable list shapes: `{"values": [...]}`, a bare list, and a
/// single bare object.
pub fn values_of(container: Option<&Value>) -> Vec<&Value> {
    match container {
        Some(value @ Value::Object(map)) => match map.get("values") {
            Some(Value::Array(values)) => values.iter().collect(),
            // A bare object is a one-element list.
            _ => vec![value],
        },
        Some(Value::Array(values)) => values.iter().collect(),
        _ => Vec::new(),
    }
}

/// The exception that best represents the event: the last in the chain, which is the one raised
/// closest to the failure.
pub fn main_exception(event: &Value) -> Option<&Value> {
    values_of(event.get("exception"))
        .into_iter()
        .rfind(|v| v.is_object())
}

/// The exception type and value used for both the title and the grouping key.
///
/// Mirrors Bugsink's `get_type_and_value_for_data`, with one deviation: an exception container that
/// is present but holds no exceptions (`{"exception": {"values": []}}`) falls through to the log
/// message here, where upstream yields the title `<unknown>`. If such an event carries a message,
/// using it is strictly more informative.
pub fn type_and_value(event: &Value) -> (String, String) {
    let has_exception = !values_of(event.get("exception")).is_empty();

    if has_exception {
        return exception_type_and_value(event);
    }
    log_message_type_and_value(event)
}

fn exception_type_and_value(event: &Value) -> (String, String) {
    let Some(exception) = main_exception(event) else {
        return ("<unknown>".to_string(), String::new());
    };

    // A synthetic exception carries no meaningful type — the SDK invented it to wrap a bare value
    // — so the crashing function identifies the problem instead.
    let synthetic = exception
        .pointer("/mechanism/synthetic")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if synthetic {
        return match crash_location(event).1 {
            Some(function) => (trim(&function, MAX_TYPE_LEN), String::new()),
            None => ("<unknown>".to_string(), String::new()),
        };
    }

    let exception_type = exception
        .get("type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Error");

    let value = exception.get("value").and_then(Value::as_str).unwrap_or("");

    (
        trim(exception_type, MAX_TYPE_LEN),
        trim(value, MAX_VALUE_LEN),
    )
}

fn log_message_type_and_value(event: &Value) -> (String, String) {
    let message = [
        "/logentry/message",
        "/logentry/formatted",
        "/message/message",
        "/message/formatted",
    ]
    .iter()
    .find_map(|path| event.pointer(path).and_then(Value::as_str))
    .or_else(|| event.get("message").and_then(Value::as_str))
    .map(str::trim)
    .filter(|s| !s.is_empty());

    match message {
        Some(message) => {
            let first_line = message.lines().next().unwrap_or("");
            ("Log Message".to_string(), trim(first_line, MAX_VALUE_LEN))
        }
        None => ("Log Message".to_string(), "<no log message>".to_string()),
    }
}

/// A human-readable title, `"Type: value"` or just `"Type"`.
pub fn title(exception_type: &str, value: &str) -> String {
    let first_line = value.lines().next().unwrap_or("");
    if first_line.is_empty() {
        return exception_type.to_string();
    }
    format!("{exception_type}: {first_line}")
}

/// `(file, function)` of the frame that best represents where the crash happened.
///
/// Mirrors Bugsink's `get_crash_location`: walk the frames from innermost outward, prefer the first
/// in-app frame, and fall back to the innermost frame with a usable function name.
pub fn crash_location(event: &Value) -> (Option<String>, Option<String>) {
    let Some(frames) = frames(event) else {
        return (None, None);
    };

    let mut fallback = None;

    // Frames are ordered outermost-first, so walking in reverse starts where the crash happened.
    for frame in frames.into_iter().rev() {
        if !frame.is_object() {
            continue;
        }

        // A frame whose function was stripped identifies nothing.
        let function = frame.get("function").and_then(Value::as_str);
        if matches!(function, None | Some("<redacted>") | Some("<unknown>")) {
            continue;
        }

        if frame
            .get("in_app")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return frame_location(frame);
        }
        if fallback.is_none() {
            fallback = Some(frame);
        }
    }

    fallback.map(frame_location).unwrap_or((None, None))
}

fn frames(event: &Value) -> Option<Vec<&Value>> {
    let from_exception = main_exception(event)
        .and_then(|e| e.pointer("/stacktrace/frames"))
        .and_then(Value::as_array);

    let from_event = event
        .pointer("/stacktrace/frames")
        .and_then(Value::as_array);

    if let Some(frames) = from_exception.or(from_event)
        && !frames.is_empty()
    {
        return Some(frames.iter().collect());
    }

    // Single-threaded events sometimes carry the stack under `threads` instead.
    let threads = values_of(event.get("threads"));
    if threads.len() == 1 {
        let frames = threads[0]
            .pointer("/stacktrace/frames")
            .and_then(Value::as_array)?;
        if !frames.is_empty() {
            return Some(frames.iter().collect());
        }
    }

    None
}

fn frame_location(frame: &Value) -> (Option<String>, Option<String>) {
    let file = frame
        .get("filename")
        .and_then(Value::as_str)
        .or_else(|| frame.get("abs_path").and_then(Value::as_str))
        .map(str::to_string);

    let function = frame
        .get("function")
        .and_then(Value::as_str)
        .map(str::to_string);

    (file, function)
}

/// Sentry's `culprit`: where the error came from, in the form `file in function`.
pub fn culprit(event: &Value) -> Option<String> {
    match crash_location(event) {
        (Some(file), Some(function)) => Some(format!("{file} in {function}")),
        (Some(file), None) => Some(file),
        (None, Some(function)) => Some(function),
        (None, None) => str_field(event, "transaction").map(str::to_string),
    }
}

/// Tag length cap, matching Sentry's. Applies to keys and values alike.
const MAX_TAG_CHARS: usize = 200;
/// Tags kept per event. Synthesized tags come first, so an SDK sending hundreds of tags cannot
/// push `environment` out.
const MAX_TAGS: usize = 50;

/// The event's tags, with the promoted fields (`environment`, `release`, `server_name`,
/// `transaction`) synthesized in front — so "filter by environment" is the same mechanism as
/// filtering by any SDK-supplied tag.
///
/// SDKs send `tags` as a map, a list of `[key, value]` pairs, or a list of `{"key", "value"}`
/// objects. Scalar values are stringified; anything else is skipped. First occurrence of a key
/// wins, which lets the promoted fields shadow an SDK tag of the same name.
pub fn tags(event: &Value) -> Vec<(String, String)> {
    let mut seen = std::collections::HashSet::new();
    let mut tags = Vec::new();

    let mut push = |key: &str, value: &str| {
        let key = trim(key.trim(), MAX_TAG_CHARS);
        let value = trim(value.trim(), MAX_TAG_CHARS);
        if !key.is_empty() && !value.is_empty() && tags.len() < MAX_TAGS && seen.insert(key.clone())
        {
            tags.push((key, value));
        }
    };

    for key in ["environment", "release", "server_name", "transaction"] {
        if let Some(value) = str_field(event, key) {
            push(key, value);
        }
    }
    if let Some(user) = user_key(event) {
        push("user", &user);
    }

    let scalar = |value: &Value| match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    };

    match event.get("tags") {
        Some(Value::Object(map)) => {
            for (key, value) in map {
                if let Some(value) = scalar(value) {
                    push(key, &value);
                }
            }
        }
        Some(Value::Array(entries)) => {
            for entry in entries {
                match entry {
                    Value::Array(pair) if pair.len() == 2 => {
                        if let (Some(key), Some(value)) = (pair[0].as_str(), scalar(&pair[1])) {
                            push(key, &value);
                        }
                    }
                    Value::Object(map) => {
                        if let (Some(key), Some(value)) = (
                            map.get("key").and_then(Value::as_str),
                            map.get("value").and_then(&scalar),
                        ) {
                            push(key, &value);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }

    tags
}

/// The identity of the user the event happened to, as Sentry's `sentry:user` tag renders it:
/// the strongest identifier present, prefixed so `id:42` and `username:42` stay distinct.
///
/// This is what "users affected" counts. Precedence matters — an SDK usually sends several
/// fields, and counting by a weak one when a real id is available miscounts.
///
/// `ip_address` is deliberately *not* an identity: a busy site's issue would grow one permanent
/// `issue_tags` row per distinct visitor IP (unbounded storage), retention would never erase
/// those IPs, and an IP is a poor proxy for a user anyway — NAT undercounts, dual-homing
/// overcounts. Events whose user is only an IP count zero users affected.
pub fn user_key(event: &Value) -> Option<String> {
    let user = event.get("user")?.as_object()?;

    for (field, prefix) in [("id", "id"), ("username", "username"), ("email", "email")] {
        let value = match user.get(field) {
            Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
            Some(Value::Number(n)) => n.to_string(),
            _ => continue,
        };
        return Some(format!("{prefix}:{value}"));
    }

    None
}

/// Fingerprint parts accepted, matching Sentry's own limit.
///
/// The array is client-controlled and each `{{ default }}` part expands to a full grouping key —
/// a regex scan over the exception value plus a ~1 KiB string. Without a cap, a ~25 KB gzipped
/// body of 1.3M placeholders costs gigabytes of allocation and seconds of CPU, twice over
/// (the collect and the join are live simultaneously). More than 32 parts is not a grouping
/// strategy anyone has; it is only ever an amplifier.
const MAX_FINGERPRINT_PARTS: usize = 32;

/// Characters kept per fingerprint part. A grouping key is a discriminator, not a payload.
const MAX_FINGERPRINT_PART_CHARS: usize = 256;

/// The SDK-supplied fingerprint, if any.
pub fn fingerprint(event: &Value) -> Option<Vec<String>> {
    let values = event.get("fingerprint")?.as_array()?;

    let parts: Vec<String> = values
        .iter()
        .take(MAX_FINGERPRINT_PARTS)
        .filter_map(|v| match v {
            Value::String(s) => Some(trim(s, MAX_FINGERPRINT_PART_CHARS)),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
        .collect();

    (!parts.is_empty()).then_some(parts)
}

/// Truncates to `max` characters. Character- rather than byte-based, so it cannot split a
/// multi-byte character.
fn trim(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

#[cfg(test)]
#[path = "event_tests.rs"]
mod tests;
