//! Turning an OTel log record into the event payload the rest of ingest already understands.
//!
//! Everything downstream — grouping, the digest, the rollups, retention, alerting, triage — takes a
//! Sentry-shaped `serde_json::Value`. So OTLP is a second *reader*, not a second pipeline: the
//! conversion ends here and nothing past this file knows an event arrived over OTLP.
//!
//! What OTel gives up in the trade is stack frames. `exception.stacktrace` is a single
//! language-specific string — a Python traceback, a Java `printStackTrace` — with no structure to
//! recover without a parser per language. It is kept verbatim in `extra`, where the issue page and
//! the `get_issue` MCP tool both surface it, and the issue's culprit stays empty.

use serde_json::{Map, Value, json};

use super::Record;

/// The severity at which a record becomes an event.
///
/// OTel numbers severity 1-24: 1-4 TRACE, 5-8 DEBUG, 9-12 INFO, 13-16 WARN, 17-20 ERROR, 21-24
/// FATAL. Thermite stores errors, so the floor is ERROR — a collector forwarding an application's
/// whole log stream must not turn every info line into an issue.
pub const SEVERITY_ERROR: i32 = 17;

/// The severity at which a record is reported as `fatal` rather than `error`.
const SEVERITY_FATAL: i32 = 21;

/// Attribute keys carrying an exception, from OTel's semantic conventions.
///
/// `exception.stacktrace` has no constant because nothing singles it out: it is one of the
/// attributes that falls through to `extra`, which is exactly where it should end up.
const EXCEPTION_TYPE: &str = "exception.type";
const EXCEPTION_MESSAGE: &str = "exception.message";

impl Record {
    /// Whether this record is worth storing.
    ///
    /// Severity is the rule, with one exception: a record carrying `exception.type` is an error
    /// whatever its severity says. Instrumentation that records a caught exception at INFO is
    /// common, and dropping the most structured records on the strength of a number the SDK may
    /// never have set would be the wrong way round.
    pub fn is_error(&self) -> bool {
        self.severity_number >= SEVERITY_ERROR || self.attributes.contains_key(EXCEPTION_TYPE)
    }
}

/// The Sentry-shaped payload for one record.
///
/// No `event_id`: `protocol::event::event_id` mints one when it is absent, and OTel has no id to
/// carry over — a log record's identity is its position in a stream. That means a retried OTLP
/// batch stores its records twice, where a retried Sentry envelope deduplicates. The alternative
/// is hashing the record into an id, which would deduplicate genuinely repeated errors out of
/// existence, and an inflated count is the less wrong of the two.
pub fn to_event(record: &Record, resource: &Map<String, Value>) -> Value {
    let mut event = Map::new();

    event.insert("timestamp".into(), timestamp(record.time_unix_nano));
    event.insert("platform".into(), platform(resource).into());
    event.insert(
        "level".into(),
        if record.severity_number >= SEVERITY_FATAL {
            "fatal".into()
        } else {
            "error".into()
        },
    );

    // OTel's resource is the closest thing it has to the fields thermite indexes. `service.name`
    // becomes the `component` tag rather than the project: one collector fans many services into
    // one project, which is the same shape a labelled DSN key gives a Sentry SDK.
    insert_str(&mut event, "release", resource.get("service.version"));
    insert_str(
        &mut event,
        "environment",
        resource
            .get("deployment.environment.name")
            .or_else(|| resource.get("deployment.environment")),
    );
    insert_str(&mut event, "server_name", resource.get("host.name"));
    if let Some(service) = resource.get("service.name").and_then(Value::as_str) {
        event.insert("tags".into(), json!({ "component": service }));
    }

    match exception(record) {
        Some(exception) => {
            event.insert("exception".into(), json!({ "values": [exception] }));
        }
        // No exception interface, so the body is the whole event: grouping falls through to the
        // log-message path and the issue is titled `Log Message: …`.
        None => {
            let message = record.body.clone().unwrap_or_default();
            event.insert("logentry".into(), json!({ "message": message }));
        }
    }

    if let Some(trace) = trace(record) {
        event.insert("contexts".into(), json!({ "trace": trace }));
    }

    let extra = extra(record);
    if !extra.is_empty() {
        event.insert("extra".into(), Value::Object(extra));
    }

    Value::Object(event)
}

/// Nanoseconds since the epoch, as the RFC 3339 string every other reader here produces.
///
/// Zero means the record carried no time at all, in which case leaving the field out lets
/// `protocol::event::timestamp` fall back to the arrival time.
fn timestamp(nanos: u64) -> Value {
    if nanos == 0 {
        return Value::Null;
    }
    chrono::DateTime::from_timestamp_nanos(nanos as i64)
        .to_rfc3339()
        .into()
}

/// The platform, from `telemetry.sdk.language`.
///
/// Sentry uses this to pick a stack-trace renderer, and OTel's language names line up with its
/// platform names for everything that matters. `other` is a valid Sentry platform and the honest
/// answer for anything unrecognised.
fn platform(resource: &Map<String, Value>) -> &'static str {
    match resource
        .get("telemetry.sdk.language")
        .and_then(Value::as_str)
    {
        Some("python") => "python",
        Some("java") => "java",
        Some("go") => "go",
        Some("ruby") => "ruby",
        Some("php") => "php",
        Some("rust") => "rust",
        Some("dotnet") => "csharp",
        Some("nodejs" | "webjs") => "javascript",
        _ => "other",
    }
}

/// The exception interface, when the record carries OTel's exception attributes.
///
/// `exception.type` alone is enough: it is what grouping keys on, and a type with no message
/// groups fine. A message with no type is not — it would group every exception in the service
/// together — so that case falls through to the log-message path instead.
fn exception(record: &Record) -> Option<Value> {
    let exception_type = record.attributes.get(EXCEPTION_TYPE)?.as_str()?;

    // The record's body is often the same text as `exception.message`, and where the message is
    // missing it is the better description of the two.
    let value = record
        .attributes
        .get(EXCEPTION_MESSAGE)
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| record.body.clone())
        .unwrap_or_default();

    Some(json!({
        "type": exception_type,
        "value": value,
        "mechanism": { "type": "otlp", "handled": false },
    }))
}

fn trace(record: &Record) -> Option<Value> {
    let mut trace = Map::new();
    for (key, id) in [("trace_id", &record.trace_id), ("span_id", &record.span_id)] {
        if let Some(id) = id {
            trace.insert(key.to_string(), id.clone().into());
        }
    }

    (!trace.is_empty()).then_some(Value::Object(trace))
}

/// Everything else the record carried.
///
/// Deliberately not tags: attribute keys and values are unbounded and client-controlled, and
/// `issue_tags` is a rollup that outlives the events it summarises. `extra` is stored with the
/// event and dies with it.
fn extra(record: &Record) -> Map<String, Value> {
    let mut extra: Map<String, Value> = record
        .attributes
        .iter()
        .filter(|(key, _)| key.as_str() != EXCEPTION_TYPE && key.as_str() != EXCEPTION_MESSAGE)
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();

    // The body, when it did not already become the event's message. Losing it would throw away
    // the human-readable half of an exception record.
    if record.attributes.contains_key(EXCEPTION_TYPE)
        && let Some(body) = &record.body
    {
        extra.entry("body").or_insert_with(|| body.clone().into());
    }

    extra
}

fn insert_str(target: &mut Map<String, Value>, key: &str, value: Option<&Value>) {
    if let Some(text) = value.and_then(Value::as_str).map(str::trim)
        && !text.is_empty()
    {
        target.insert(key.to_string(), text.into());
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    const EXCEPTION_STACKTRACE: &str = "exception.stacktrace";

    fn record(severity: i32) -> Record {
        Record {
            time_unix_nano: 1_756_000_000_000_000_000,
            severity_number: severity,
            body: Some("payment gateway unreachable".into()),
            attributes: BTreeMap::new(),
            trace_id: None,
            span_id: None,
        }
    }

    fn resource(pairs: &[(&str, &str)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), Value::from(*value)))
            .collect()
    }

    /// A collector forwarding an application's whole log stream must not turn every info line into
    /// an issue — the floor is the only thing standing between OTLP ingest and that.
    #[test]
    fn only_error_and_above_is_stored() {
        assert!(!record(9).is_error(), "INFO");
        assert!(!record(13).is_error(), "WARN");
        assert!(!record(16).is_error(), "WARN4");
        assert!(record(17).is_error(), "ERROR");
        assert!(record(21).is_error(), "FATAL");
    }

    /// Instrumentation recording a caught exception at INFO is common, and those are the most
    /// structured records there are.
    #[test]
    fn a_record_carrying_an_exception_is_an_error_whatever_its_severity() {
        let mut info = record(9);
        info.attributes
            .insert(EXCEPTION_TYPE.into(), "ValueError".into());

        assert!(info.is_error());
    }

    #[test]
    fn a_bare_log_record_becomes_a_log_message_event() {
        let event = to_event(&record(17), &resource(&[]));

        assert_eq!(
            event["logentry"]["message"],
            Value::from("payment gateway unreachable")
        );
        assert_eq!(event["level"], Value::from("error"));
        assert!(event.get("exception").is_none());
    }

    #[test]
    fn fatal_records_report_as_fatal() {
        assert_eq!(to_event(&record(21), &resource(&[]))["level"], "fatal");
    }

    #[test]
    fn exception_attributes_become_the_exception_interface() {
        let mut with_exception = record(17);
        with_exception
            .attributes
            .insert(EXCEPTION_TYPE.into(), "ConnectionError".into());
        with_exception
            .attributes
            .insert(EXCEPTION_MESSAGE.into(), "connection refused".into());
        with_exception.attributes.insert(
            EXCEPTION_STACKTRACE.into(),
            "Traceback…\n  File \"a.py\", line 1".into(),
        );

        let event = to_event(&with_exception, &resource(&[]));
        let exception = &event["exception"]["values"][0];

        assert_eq!(exception["type"], Value::from("ConnectionError"));
        assert_eq!(exception["value"], Value::from("connection refused"));
        assert_eq!(exception["mechanism"]["handled"], Value::from(false));

        // No frames to recover, so the stack stays readable where both the issue page and
        // `get_issue` surface it.
        assert!(
            event["extra"][EXCEPTION_STACKTRACE]
                .as_str()
                .unwrap()
                .contains("Traceback")
        );
        // The body is not lost to the exception taking over the title.
        assert_eq!(
            event["extra"]["body"],
            Value::from("payment gateway unreachable")
        );
    }

    /// A message with no type would group every exception in a service into one issue, which is
    /// worse than titling it as a log message.
    #[test]
    fn a_message_without_a_type_is_not_an_exception() {
        let mut message_only = record(17);
        message_only
            .attributes
            .insert(EXCEPTION_MESSAGE.into(), "connection refused".into());

        let event = to_event(&message_only, &resource(&[]));
        assert!(event.get("exception").is_none());
        assert!(event.get("logentry").is_some());
    }

    #[test]
    fn resource_attributes_fill_in_the_indexed_fields() {
        let event = to_event(
            &record(17),
            &resource(&[
                ("service.name", "checkout"),
                ("service.version", "1.4.2"),
                ("deployment.environment.name", "production"),
                ("host.name", "worker-1"),
                ("telemetry.sdk.language", "python"),
            ]),
        );

        assert_eq!(event["release"], Value::from("1.4.2"));
        assert_eq!(event["environment"], Value::from("production"));
        assert_eq!(event["server_name"], Value::from("worker-1"));
        assert_eq!(event["platform"], Value::from("python"));
        // One collector, many services, one project — split by the same tag a labelled DSN key
        // stamps on.
        assert_eq!(event["tags"]["component"], Value::from("checkout"));
    }

    /// The attribute was renamed; collectors in the field send both spellings.
    #[test]
    fn the_older_environment_attribute_still_works() {
        let event = to_event(
            &record(17),
            &resource(&[("deployment.environment", "staging")]),
        );

        assert_eq!(event["environment"], Value::from("staging"));
    }

    #[test]
    fn trace_ids_land_in_the_trace_context() {
        let mut traced = record(17);
        traced.trace_id = Some("4bf92f3577b34da6".into());
        traced.span_id = Some("00f067aa0ba902b7".into());

        let event = to_event(&traced, &resource(&[]));
        assert_eq!(
            event["contexts"]["trace"]["trace_id"],
            Value::from("4bf92f3577b34da6")
        );
        assert_eq!(
            event["contexts"]["trace"]["span_id"],
            Value::from("00f067aa0ba902b7")
        );
    }

    /// Attributes are client-controlled and unbounded, and `issue_tags` outlives the events it
    /// summarises — which is exactly the cardinality problem that rollup already had to solve.
    #[test]
    fn record_attributes_become_extra_and_never_tags() {
        let mut attributed = record(17);
        attributed
            .attributes
            .insert("http.route".into(), "/charge".into());
        attributed.attributes.insert("attempt".into(), 3.into());

        let event = to_event(&attributed, &resource(&[]));

        assert_eq!(event["extra"]["http.route"], Value::from("/charge"));
        assert_eq!(event["extra"]["attempt"], Value::from(3));
        assert!(event.get("tags").is_none());
    }

    #[test]
    fn a_record_with_no_time_leaves_the_field_for_ingest_to_fill() {
        let mut untimed = record(17);
        untimed.time_unix_nano = 0;

        assert_eq!(to_event(&untimed, &resource(&[]))["timestamp"], Value::Null);
    }

    #[test]
    fn a_time_becomes_an_rfc3339_string() {
        let event = to_event(&record(17), &resource(&[]));

        assert!(
            event["timestamp"].as_str().unwrap().starts_with("2025-"),
            "unexpected timestamp: {:?}",
            event["timestamp"]
        );
    }
}
