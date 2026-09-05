//! Reading OTLP/JSON.
//!
//! A second encoding of the same messages, and not a mechanical one. OTLP/JSON renames every field
//! to camelCase, writes 64-bit integers as decimal *strings* because JSON numbers cannot hold
//! them, spells the `AnyValue` union as a one-key object (`{"stringValue": "…"}`), and — deviating
//! from proto3 JSON on purpose — encodes trace and span ids as hex rather than base64.
//!
//! So this is a reader, not a `Deserialize` derive over the protobuf types. It produces the same
//! [`Group`] values `proto` does, and `equivalent_encodings_produce_the_same_records` in the
//! parent module is what keeps the two from drifting.

use serde_json::Value;

use super::{Group, Record};

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("not valid JSON: {0}")]
    Malformed(#[from] serde_json::Error),

    #[error("request declares more than {0} log records")]
    TooManyRecords(usize),
}

pub fn decode(body: &[u8], max_records: usize) -> Result<Vec<Group>, ParseError> {
    let root: Value = serde_json::from_slice(body)?;
    let mut budget = max_records;
    let mut groups = Vec::new();

    for resource_logs in array(&root, "resourceLogs") {
        let mut group = Group {
            resource: attributes(resource_logs.pointer("/resource/attributes")),
            records: Vec::new(),
        };

        for scope_logs in array(resource_logs, "scopeLogs") {
            for record in array(scope_logs, "logRecords") {
                budget = budget
                    .checked_sub(1)
                    .ok_or(ParseError::TooManyRecords(max_records))?;
                group.records.push(record_from(record));
            }
        }

        groups.push(group);
    }

    Ok(groups)
}

fn record_from(record: &Value) -> Record {
    Record {
        // `timeUnixNano` is when it happened, `observedTimeUnixNano` when the collector saw it.
        // Both are decimal strings, and both may be absent.
        time_unix_nano: nanos(record, "timeUnixNano")
            .or_else(|| nanos(record, "observedTimeUnixNano"))
            .unwrap_or(0),
        severity_number: severity(record),
        body: record
            .get("body")
            .and_then(any_value)
            .map(|value| match value {
                Value::String(text) => text,
                other => other.to_string(),
            }),
        attributes: attributes(record.get("attributes")),
        trace_id: hex_id(record, "traceId"),
        span_id: hex_id(record, "spanId"),
    }
}

/// `severityNumber` is a proto enum, which JSON may write as a number *or* as its name.
fn severity(record: &Value) -> i32 {
    match record.get("severityNumber") {
        Some(Value::Number(number)) => number.as_i64().unwrap_or(0) as i32,
        Some(Value::String(name)) => severity_from_name(name),
        _ => 0,
    }
}

/// The generated enum names, e.g. `SEVERITY_NUMBER_ERROR2` for 18.
fn severity_from_name(name: &str) -> i32 {
    let Some(rest) = name.strip_prefix("SEVERITY_NUMBER_") else {
        return 0;
    };

    // Every name is a level plus an optional 2, 3 or 4 — `ERROR`, `ERROR2`, `ERROR3`, `ERROR4`.
    let (level, offset) = match rest.strip_suffix(['2', '3', '4']) {
        Some(level) => (level, rest.as_bytes()[rest.len() - 1] - b'1'),
        None => (rest, 0),
    };

    let base = match level {
        "TRACE" => 1,
        "DEBUG" => 5,
        "INFO" => 9,
        "WARN" => 13,
        "ERROR" => 17,
        "FATAL" => 21,
        _ => return 0,
    };
    base + i32::from(offset)
}

/// A 64-bit field, which OTLP/JSON writes as a decimal string. Some encoders send a number
/// regardless, which is readable up to 2^53 and is accepted rather than refused.
fn nanos(record: &Value, key: &str) -> Option<u64> {
    match record.get(key)? {
        Value::String(digits) => digits.parse().ok().filter(|nanos| *nanos > 0),
        Value::Number(number) => number.as_u64().filter(|nanos| *nanos > 0),
        _ => None,
    }
}

fn hex_id(record: &Value, key: &str) -> Option<String> {
    record
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_string)
}

fn array<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
}

/// `[{"key": …, "value": {"stringValue": …}}, …]` flattened to a map.
fn attributes(attributes: Option<&Value>) -> std::collections::BTreeMap<String, Value> {
    let Some(Value::Array(entries)) = attributes else {
        return Default::default();
    };

    entries
        .iter()
        .take(super::MAX_ATTRIBUTES)
        .filter_map(|entry| {
            let key = entry.get("key")?.as_str()?;
            let value = entry.get("value").and_then(any_value)?;
            Some((key.to_string(), value))
        })
        .collect()
}

/// The `AnyValue` union, spelled as an object with one key naming the arm.
///
/// Only the scalar arms are read, matching the protobuf reader: an attribute that has to be
/// flattened to display is not worth the recursion on a parser reachable from the internet.
fn any_value(value: &Value) -> Option<Value> {
    if let Some(text) = value.get("stringValue") {
        return Some(text.clone());
    }
    if let Some(flag) = value.get("boolValue") {
        return Some(flag.clone());
    }
    if let Some(number) = value.get("intValue") {
        // Written as a decimal string, like every other 64-bit field.
        return match number {
            Value::String(digits) => digits.parse::<i64>().ok().map(Value::from),
            other => Some(other.clone()),
        };
    }
    value.get("doubleValue").cloned()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn reads_records_and_their_resource() {
        let body = json!({
            "resourceLogs": [{
                "resource": { "attributes": [
                    { "key": "service.name", "value": { "stringValue": "checkout" } },
                ]},
                "scopeLogs": [{ "logRecords": [{
                    "timeUnixNano": "1756000000000000000",
                    "severityNumber": 17,
                    "body": { "stringValue": "payment gateway unreachable" },
                    "attributes": [{ "key": "attempt", "value": { "intValue": "3" } }],
                }]}],
            }]
        });

        let groups = decode(body.to_string().as_bytes(), 100).unwrap();
        assert_eq!(groups[0].resource["service.name"], Value::from("checkout"));

        let record = &groups[0].records[0];
        assert_eq!(record.time_unix_nano, 1_756_000_000_000_000_000);
        assert_eq!(record.severity_number, 17);
        assert_eq!(record.body.as_deref(), Some("payment gateway unreachable"));
        assert_eq!(record.attributes["attempt"], Value::from(3));
    }

    /// Proto3 JSON allows an enum as either its number or its name, and real exporters send both.
    #[test]
    fn reads_a_severity_written_as_its_name() {
        assert_eq!(severity_from_name("SEVERITY_NUMBER_ERROR"), 17);
        assert_eq!(severity_from_name("SEVERITY_NUMBER_ERROR2"), 18);
        assert_eq!(severity_from_name("SEVERITY_NUMBER_FATAL4"), 24);
        assert_eq!(severity_from_name("SEVERITY_NUMBER_INFO"), 9);
        assert_eq!(severity_from_name("SEVERITY_NUMBER_UNSPECIFIED"), 0);
        assert_eq!(severity_from_name("nonsense"), 0);
    }

    #[test]
    fn accepts_a_64_bit_field_sent_as_a_number() {
        let body = json!({ "resourceLogs": [{ "scopeLogs": [{ "logRecords": [
            { "timeUnixNano": 1756000000000000000i64, "severityNumber": 17 },
        ]}]}]});

        let groups = decode(body.to_string().as_bytes(), 100).unwrap();
        assert_eq!(
            groups[0].records[0].time_unix_nano,
            1_756_000_000_000_000_000
        );
    }

    #[test]
    fn falls_back_to_the_observed_time() {
        let body = json!({ "resourceLogs": [{ "scopeLogs": [{ "logRecords": [
            { "observedTimeUnixNano": "1756000000000000000", "severityNumber": 17 },
        ]}]}]});

        let groups = decode(body.to_string().as_bytes(), 100).unwrap();
        assert_eq!(
            groups[0].records[0].time_unix_nano,
            1_756_000_000_000_000_000
        );
    }

    #[test]
    fn refuses_a_batch_past_the_record_cap() {
        let records: Vec<Value> = (0..3).map(|_| json!({ "severityNumber": 17 })).collect();
        let body = json!({ "resourceLogs": [{ "scopeLogs": [{ "logRecords": records }]}]});

        assert!(matches!(
            decode(body.to_string().as_bytes(), 2),
            Err(ParseError::TooManyRecords(2))
        ));
    }

    /// A trace id that is not hex is dropped rather than passed through: it lands in a context
    /// field an operator will paste into another tool, so a malformed one is worse than none.
    #[test]
    fn drops_a_trace_id_that_is_not_hex() {
        let body = json!({ "resourceLogs": [{ "scopeLogs": [{ "logRecords": [
            { "severityNumber": 17, "traceId": "4bf92f35", "spanId": "not hex!" },
        ]}]}]});

        let groups = decode(body.to_string().as_bytes(), 100).unwrap();
        assert_eq!(groups[0].records[0].trace_id.as_deref(), Some("4bf92f35"));
        assert_eq!(groups[0].records[0].span_id, None);
    }

    /// Attribute sets are client-controlled, so the map a record builds needs the same kind of
    /// ceiling the record count has.
    #[test]
    fn attributes_stop_at_the_cap() {
        let many: Vec<Value> = (0..super::super::MAX_ATTRIBUTES + 50)
            .map(|n| json!({ "key": format!("k{n:04}"), "value": { "stringValue": "v" } }))
            .collect();
        let body = json!({ "resourceLogs": [{ "scopeLogs": [{ "logRecords": [
            { "severityNumber": 17, "attributes": many },
        ]}]}]});

        let groups = decode(body.to_string().as_bytes(), 100).unwrap();
        assert_eq!(
            groups[0].records[0].attributes.len(),
            super::super::MAX_ATTRIBUTES
        );
    }

    #[test]
    fn an_empty_request_reads_as_nothing() {
        assert!(decode(b"{}", 100).unwrap().is_empty());
        assert!(decode(br#"{"resourceLogs":[]}"#, 100).unwrap().is_empty());
    }

    #[test]
    fn malformed_json_is_an_error() {
        assert!(matches!(
            decode(b"{not json", 100),
            Err(ParseError::Malformed(_))
        ));
    }
}
