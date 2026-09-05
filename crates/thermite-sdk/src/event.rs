//! The event payload, cut down to the fields thermite reads.
//!
//! Field order in this file is the field order on the wire: `serde`'s derive emits struct fields in
//! declaration order, and the envelope tests assert exact bytes. Reordering fields here changes
//! the wire output, so keep additions at the end of whichever group they belong to.
//!
//! Every optional field is skipped when empty. A Sentry event is mostly absent fields, and sending
//! `"transaction": null` on every event is bytes on the wire and a column of nulls in the stored
//! payload.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Serialize, Serializer};
use serde_json::Value;
use uuid::Uuid;

/// What thermite records as the event's platform.
///
/// Sentry uses this to pick a stack-trace renderer. `rust` is honest even under wasm — the frames,
/// where there are any, are Rust frames.
pub const PLATFORM: &str = "rust";

/// Severity. These five are exactly what `thermite_core::protocol::event::level` accepts; anything
/// else it silently reads as `error`, so there is no sixth variant worth having.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Fatal,
    #[default]
    Error,
    Warning,
    Info,
    Debug,
}

/// Sentry's repeated-interface wrapper: `{"values": [...]}`.
///
/// Thermite also accepts a bare list or a bare object for these, but the wrapped form is what every
/// real SDK sends and therefore the form best covered by its tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Values<T> {
    pub values: Vec<T>,
}

impl<T> Values<T> {
    pub fn one(value: T) -> Self {
        Self {
            values: vec![value],
        }
    }
}

/// A log message. Thermite reads `/logentry/message` before it falls back to a bare `message`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LogEntry {
    pub message: String,
}

/// Who hit the error.
///
/// No `ip_address`: thermite scrubs PII on ingest and deliberately stopped deriving a user key from
/// the request IP, so sending one would only feed a field designed to throw it away.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct User {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

impl User {
    /// True when there is nothing here worth sending. `user_key` reads `id`, `username` and
    /// `email` in that order and yields nothing if all three are absent.
    pub fn is_empty(&self) -> bool {
        self.id.is_none() && self.username.is_none() && self.email.is_none()
    }
}

/// One stack frame.
///
/// The field names are the ones thermite's view model reads in `src/models/errors.rs`; a frame
/// spelled differently renders blank rather than failing, which is the worst kind of wrong.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Frame {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abs_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lineno: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colno: Option<u32>,
    /// Whether this frame is the user's own code. Load-bearing: `crash_location` prefers the
    /// innermost in-app frame, and that choice becomes the issue's culprit.
    pub in_app: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_line: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pre_context: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub post_context: Vec<String>,
}

/// Frames, ordered outermost-first — thermite walks them in reverse to find the crash site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Stacktrace {
    pub frames: Vec<Frame>,
}

/// How the error was captured.
///
/// `synthetic` is the one field with teeth: it tells thermite the SDK invented this exception to
/// wrap a bare value, so the crashing function should title the issue instead of the made-up type.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Mechanism {
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthetic: Option<bool>,
}

/// One exception in the chain. The *last* one is what thermite groups on: it is the one raised
/// closest to the failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Exception {
    pub r#type: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stacktrace: Option<Stacktrace>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mechanism: Option<Mechanism>,
}

impl Exception {
    pub fn new(r#type: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            r#type: r#type.into(),
            value: value.into(),
            module: None,
            stacktrace: None,
            mechanism: None,
        }
    }
}

/// One breadcrumb — a thing that happened before the error.
///
/// Thermite does not index these, but the issue detail page renders them and the `get_issue` MCP
/// tool promises them, so an SDK that drops them degrades triage rather than saving anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Breadcrumb {
    pub timestamp: DateTime<Utc>,
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    pub level: Level,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub data: BTreeMap<String, Value>,
}

impl Breadcrumb {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            timestamp: Utc::now(),
            r#type: "default".to_string(),
            category: None,
            level: Level::Info,
            message: Some(message.into()),
            data: BTreeMap::new(),
        }
    }
}

/// A Sentry event, containing what thermite reads and nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Event {
    #[serde(serialize_with = "unhyphenated")]
    pub event_id: Uuid,
    /// When the error happened. Thermite clamps this to `(received_at - 30 days, received_at + 1h]`
    /// — a client clock outside that window silently becomes the arrival time.
    pub timestamp: DateTime<Utc>,
    pub platform: &'static str,
    pub level: Level,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logger: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transaction: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logentry: Option<LogEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exception: Option<Values<Exception>>,
    /// Overrides grouping outright. A part reading `{{ default }}` is substituted with the key
    /// thermite would have computed, which is how you refine grouping instead of replacing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<User>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub tags: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub contexts: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breadcrumbs: Option<Values<Breadcrumb>>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl Event {
    /// An empty event stamped now, with a fresh id.
    pub fn new() -> Self {
        Self {
            event_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            platform: PLATFORM,
            level: Level::Error,
            logger: None,
            release: None,
            environment: None,
            server_name: None,
            transaction: None,
            logentry: None,
            exception: None,
            fingerprint: None,
            user: None,
            tags: BTreeMap::new(),
            contexts: BTreeMap::new(),
            breadcrumbs: None,
            extra: BTreeMap::new(),
        }
    }

    /// A log message. Titles as `"Log Message: <first line>"`.
    pub fn message(message: impl Into<String>, level: Level) -> Self {
        Self {
            level,
            logentry: Some(LogEntry {
                message: message.into(),
            }),
            ..Self::new()
        }
    }

    /// An exception. Titles and groups as `"<type>: <normalized value>"`.
    pub fn exception(exception: Exception) -> Self {
        Self {
            exception: Some(Values::one(exception)),
            ..Self::new()
        }
    }

    /// An error and everything it wraps, as an exception chain.
    ///
    /// Ordered root cause first, matching sentry-rust. Thermite groups on the *last* exception, so
    /// this puts the outermost error — the one the calling code actually saw — in the title, and
    /// keeps the causes underneath it for reading.
    pub fn from_error(error: &dyn std::error::Error) -> Self {
        let mut values = vec![exception_from_error(error)];

        let mut source = error.source();
        while let Some(error) = source {
            values.push(exception_from_error(error));
            source = error.source();
        }
        values.reverse();

        Self {
            exception: Some(Values { values }),
            ..Self::new()
        }
    }
}

fn exception_from_error(error: &dyn std::error::Error) -> Exception {
    Exception::new(type_from_debug(&format!("{error:?}")), error.to_string())
}

/// The leading identifier of a `Debug` rendering, which for a derived `Debug` is the type name.
///
/// `&dyn Error` has erased the concrete type, so `std::any::type_name` yields `dyn Error` and there
/// is nothing else to read. sentry-rust parses the same string for the same reason. An error whose
/// `Debug` is hand-written to start with something else gets a worse type here, not a wrong event:
/// the value still carries the full message.
fn type_from_debug(debug: &str) -> String {
    let name: String = debug
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    if name.is_empty() {
        return "Error".to_string();
    }
    name
}

impl Default for Event {
    fn default() -> Self {
        Self::new()
    }
}

/// Sentry's `event_id` is 32 hex characters with no dashes, and thermite echoes that form back in
/// its ingest response. `uuid`'s own `Serialize` would emit the hyphenated form.
pub(crate) fn unhyphenated<S: Serializer>(id: &Uuid, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&id.simple().to_string())
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn json(event: &Event) -> Value {
        serde_json::to_value(event).unwrap()
    }

    #[test]
    fn an_event_id_serializes_without_hyphens() {
        let mut event = Event::new();
        event.event_id = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();

        assert_eq!(
            json(&event)["event_id"],
            Value::from("00000000000040008000000000000001")
        );
    }

    /// Absent fields are absent, not null. Thermite reads `str_field` with an empty-string filter,
    /// so a null would be harmless — but it is bytes on every event forever.
    #[test]
    fn empty_fields_are_omitted_entirely() {
        let object = json(&Event::new());
        let object = object.as_object().unwrap();

        for key in [
            "logger",
            "release",
            "environment",
            "server_name",
            "transaction",
            "exception",
            "fingerprint",
            "user",
            "tags",
            "contexts",
            "breadcrumbs",
            "extra",
        ] {
            assert!(!object.contains_key(key), "{key} should be omitted");
        }
    }

    /// The four fields ingest always indexes have to be present on the barest possible event.
    #[test]
    fn the_indexed_core_is_always_present() {
        let object = json(&Event::new());

        assert!(object.get("event_id").is_some());
        assert!(object.get("timestamp").is_some());
        assert_eq!(object["platform"], Value::from("rust"));
        assert_eq!(object["level"], Value::from("error"));
    }

    #[test]
    fn a_message_lands_where_type_and_value_looks_for_it() {
        let event = Event::message("payment gateway unreachable", Level::Warning);

        assert_eq!(
            json(&event)["logentry"]["message"],
            Value::from("payment gateway unreachable")
        );
        assert_eq!(json(&event)["level"], Value::from("warning"));
    }

    #[test]
    fn an_exception_lands_in_the_values_wrapper() {
        let event = Event::exception(Exception::new("ValueError", "bad input"));
        let object = json(&event);

        assert_eq!(
            object["exception"]["values"][0]["type"],
            Value::from("ValueError")
        );
        assert_eq!(
            object["exception"]["values"][0]["value"],
            Value::from("bad input")
        );
    }

    /// Frames carry the names the issue detail page reads. A frame with `in_app` missing would
    /// render, but `crash_location` treats absent as false and picks a different culprit.
    #[test]
    fn a_frame_serializes_with_in_app_always_present() {
        let frame = Frame {
            function: Some("charge".to_string()),
            filename: Some("src/billing.rs".to_string()),
            lineno: Some(42),
            in_app: true,
            ..Frame::default()
        };
        let object = serde_json::to_value(&frame).unwrap();

        assert_eq!(object["function"], Value::from("charge"));
        assert_eq!(object["filename"], Value::from("src/billing.rs"));
        assert_eq!(object["lineno"], Value::from(42));
        assert_eq!(object["in_app"], Value::from(true));
        assert!(object.get("colno").is_none());
        assert!(object.get("pre_context").is_none());
    }

    /// The chain reads root-cause first so the *last* value — the one thermite titles and groups
    /// on — is the error the calling code actually saw.
    #[test]
    fn an_error_chain_puts_the_root_cause_first() {
        #[derive(Debug)]
        struct Inner;
        #[derive(Debug)]
        struct Outer(Inner);

        impl std::fmt::Display for Inner {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "connection refused")
            }
        }
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "could not charge the card")
            }
        }
        impl std::error::Error for Inner {}
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        let event = Event::from_error(&Outer(Inner));
        let values = &event.exception.unwrap().values;

        assert_eq!(values.len(), 2);
        assert_eq!(values[0].r#type, "Inner");
        assert_eq!(values[0].value, "connection refused");
        assert_eq!(values[1].r#type, "Outer");
        assert_eq!(values[1].value, "could not charge the card");
    }

    #[test]
    fn a_type_name_comes_off_the_front_of_the_debug_rendering() {
        assert_eq!(type_from_debug("ValueError { field: 1 }"), "ValueError");
        assert_eq!(type_from_debug("Io(Custom { .. })"), "Io");
        assert_eq!(type_from_debug("Plain"), "Plain");
        // An `anyhow::msg` debugs as a quoted string, which names no type at all.
        assert_eq!(type_from_debug("\"something went wrong\""), "Error");
    }

    #[test]
    fn a_timestamp_serializes_as_rfc3339() {
        let mut event = Event::new();
        event.timestamp = Utc.with_ymd_and_hms(2026, 8, 29, 10, 0, 0).unwrap();

        assert_eq!(
            json(&event)["timestamp"],
            Value::from("2026-08-29T10:00:00Z")
        );
    }
}
