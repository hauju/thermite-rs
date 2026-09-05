//! Serializing an event into a Sentry envelope.
//!
//! ```text
//! Envelope = Headers { "\n" Item } [ "\n" ] ;
//! Item     = Headers "\n" Payload ;
//! ```
//!
//! Item headers may declare a `length`, and thermite's parser honours it — but it also accepts a
//! payload that simply runs to the next newline. This writes the latter form: a JSON event
//! contains no unescaped newline, so the framing is unambiguous without the length, and computing
//! one would mean serializing to a buffer first just to measure it.
//!
//! No `sent_at` header. Thermite parses it and nothing reads it, and a clock reading the receiver
//! ignores is a field that can only ever be wrong.

use serde::Serialize;
use uuid::Uuid;

use crate::dsn::Dsn;
use crate::event::Event;

/// Envelope-level headers.
///
/// A struct rather than a `json!` literal so the field order is fixed by this declaration. A
/// `serde_json::Map` orders by whichever of its two backing types the `preserve_order` feature
/// selected, and that feature is unified across the whole workspace — a dependency turning it on
/// would silently reorder these bytes.
#[derive(Serialize)]
struct EnvelopeHeaders<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    event_id: Option<String>,
    /// Duplicates the `?sentry_key=` already on the URL. It is two dozen bytes, and it is what
    /// makes the body self-authenticating if it is ever replayed or tunnelled through a proxy that
    /// drops the query string. Thermite recovers it from the first 8 KiB of the body without
    /// decoding the rest, so it costs the receiver nothing either.
    dsn: &'a str,
}

#[derive(Serialize)]
struct ItemHeaders<'a> {
    r#type: &'a str,
}

/// Frames one event as a complete envelope body, ready to POST to `Dsn::ingest_url`.
pub fn event_envelope(event: &Event, dsn: &Dsn) -> Result<Vec<u8>, serde_json::Error> {
    frame(Some(event.event_id), "event", event, dsn)
}

/// Frames one session update.
///
/// No `event_id`: a session is a counter, not an event. Thermite folds it into a rollup and keeps
/// no row to correlate an id against.
#[cfg(feature = "sessions")]
pub fn session_envelope(
    update: &crate::session::Update,
    dsn: &Dsn,
) -> Result<Vec<u8>, serde_json::Error> {
    frame(None, "session", update, dsn)
}

fn frame(
    event_id: Option<Uuid>,
    item_type: &str,
    payload: &impl Serialize,
    dsn: &Dsn,
) -> Result<Vec<u8>, serde_json::Error> {
    let headers = EnvelopeHeaders {
        event_id: event_id.map(|id| id.simple().to_string()),
        dsn: &dsn.raw,
    };

    let mut body = Vec::new();
    serde_json::to_writer(&mut body, &headers)?;
    body.push(b'\n');
    serde_json::to_writer(&mut body, &ItemHeaders { r#type: item_type })?;
    body.push(b'\n');
    serde_json::to_writer(&mut body, payload)?;
    body.push(b'\n');

    Ok(body)
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use uuid::Uuid;

    use super::*;
    use crate::event::Level;

    fn dsn() -> Dsn {
        Dsn::parse("http://abc123@localhost:9000/42").unwrap()
    }

    /// A fully determined event: fixed id, fixed clock, sorted tag map. Every value that would
    /// otherwise vary is pinned so the bytes can be asserted literally.
    fn fixture() -> Event {
        let mut event = Event::message("payment gateway unreachable", Level::Error);
        event.event_id = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
        event.timestamp = Utc.with_ymd_and_hms(2026, 8, 29, 10, 0, 0).unwrap();
        event.release = Some("1.4.2".to_string());
        event.environment = Some("production".to_string());
        event
            .tags
            .insert("component".to_string(), "billing".to_string());
        event
    }

    /// The wire contract, asserted literally.
    ///
    /// Deliberately brittle: this is the one test that fails when a field is renamed, reordered or
    /// quietly added, which is exactly the change that would otherwise reach thermite as an event
    /// that parses but groups differently. If this breaks, the question is whether the receiver
    /// still reads what it used to — not whether to update the string.
    #[test]
    fn a_known_event_serializes_to_exact_bytes() {
        let body = event_envelope(&fixture(), &dsn()).unwrap();

        assert_eq!(
            String::from_utf8(body).unwrap(),
            concat!(
                r#"{"event_id":"00000000000040008000000000000001","dsn":"http://abc123@localhost:9000/42"}"#,
                "\n",
                r#"{"type":"event"}"#,
                "\n",
                r#"{"event_id":"00000000000040008000000000000001","timestamp":"2026-08-29T10:00:00Z","platform":"rust","level":"error","release":"1.4.2","environment":"production","logentry":{"message":"payment gateway unreachable"},"tags":{"component":"billing"}}"#,
                "\n",
            )
        );
    }

    /// Three lines, and the payload is the third. The parser splits on newlines, so a payload
    /// carrying a raw one would silently truncate into a bogus fourth item.
    #[test]
    fn a_multiline_message_stays_on_one_line() {
        let mut event = fixture();
        event.logentry = Some(crate::event::LogEntry {
            message: "first line\nsecond line".to_string(),
        });

        let body = event_envelope(&event, &dsn()).unwrap();
        let text = String::from_utf8(body).unwrap();

        assert_eq!(text.lines().count(), 3);
        assert!(
            text.lines()
                .nth(2)
                .unwrap()
                .contains(r"first line\nsecond line")
        );
    }

    #[test]
    fn the_envelope_header_carries_the_event_id_and_dsn() {
        let body = event_envelope(&fixture(), &dsn()).unwrap();
        let text = String::from_utf8(body).unwrap();
        let headers: serde_json::Value =
            serde_json::from_str(text.lines().next().unwrap()).unwrap();

        assert_eq!(
            headers["event_id"],
            serde_json::Value::from("00000000000040008000000000000001")
        );
        assert_eq!(
            headers["dsn"],
            serde_json::Value::from("http://abc123@localhost:9000/42")
        );
    }
}
