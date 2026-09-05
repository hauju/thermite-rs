//! Reading OTLP protobuf.
//!
//! The messages here are hand-written rather than generated, and carry only the fields thermite
//! reads — the same choice `protocol::event` makes about Sentry's schema, for the same reason. A
//! field we do not declare is skipped by prost's own unknown-field handling, so a newer collector
//! sending more of the schema is not an error.
//!
//! The container messages (`LogsData`, `ResourceLogs`, `ScopeLogs`) are walked by hand instead,
//! and that is deliberate. Derived decoding grows a `Vec<LogRecord>` with no ceiling, and an empty
//! record costs two bytes on the wire against roughly 150 in memory — a 75x amplifier, which is
//! exactly the shape of the item-list problem `envelope::parse` caps with `MAX_ITEMS`. The budget
//! has to bound the decode, not the result, so it lives in the walk.

use bytes::{Buf, Bytes};
use prost::encoding::{DecodeContext, WireType, decode_key, decode_varint, skip_field};
use prost::{DecodeError, Message};

use super::{Group, Record};

/// Log records accepted from one request.
///
/// Generous, because a collector forwarding everything sends batches of which most records are
/// below the severity floor and dropped here. The OTel SDKs' own default export batch is 512, so
/// this is twenty times what a well-behaved client sends.
pub const MAX_RECORDS: usize = 10_000;

/// Resource and scope groups accepted, per level.
///
/// A real batch carries one resource and a handful of scopes. This is not a limit anything
/// legitimate approaches; it exists so the group vectors cannot become the amplifier the record
/// vector would otherwise be.
const MAX_GROUPS: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("not valid OTLP protobuf: {0}")]
    Malformed(#[from] DecodeError),

    #[error("request declares more than {MAX_RECORDS} log records")]
    TooManyRecords,

    #[error("a field runs past the end of the message")]
    Truncated,
}

// Field numbers, from opentelemetry/proto/logs/v1/logs.proto.
const LOGS_DATA_RESOURCE_LOGS: u32 = 1;
const RESOURCE_LOGS_RESOURCE: u32 = 1;
const RESOURCE_LOGS_SCOPE_LOGS: u32 = 2;
const SCOPE_LOGS_LOG_RECORDS: u32 = 2;

/// Reads an `ExportLogsServiceRequest`, whose body is a `LogsData`.
pub fn decode(body: &[u8]) -> Result<Vec<Group>, ParseError> {
    let mut buf = Bytes::copy_from_slice(body);
    let mut budget = MAX_RECORDS;
    let mut groups = Vec::new();

    for mut resource_logs in fields(&mut buf, LOGS_DATA_RESOURCE_LOGS, MAX_GROUPS)? {
        let mut group = Group::default();

        for (tag, mut field) in tagged_fields(&mut resource_logs, MAX_GROUPS)? {
            match tag {
                RESOURCE_LOGS_RESOURCE => {
                    group.resource = super::attributes(&ResourceMessage::decode(field)?.attributes);
                }
                RESOURCE_LOGS_SCOPE_LOGS => {
                    for record in fields(&mut field, SCOPE_LOGS_LOG_RECORDS, budget)? {
                        budget = budget.checked_sub(1).ok_or(ParseError::TooManyRecords)?;
                        group
                            .records
                            .push(record_from(LogRecordMessage::decode(record)?));
                    }
                }
                _ => {}
            }
        }

        groups.push(group);
    }

    Ok(groups)
}

/// Every length-delimited field carrying `tag`, in order, at most `budget` of them.
///
/// Returning `Bytes` rather than a slice keeps this a refcount bump per field instead of a copy.
fn fields(buf: &mut Bytes, tag: u32, budget: usize) -> Result<Vec<Bytes>, ParseError> {
    Ok(tagged_fields(buf, budget)?
        .into_iter()
        .filter(|(found, _)| *found == tag)
        .map(|(_, field)| field)
        .collect())
}

/// Every length-delimited field, with its tag. Fields of any other wire type are skipped, which is
/// what makes an unknown scalar added to a future version of the schema harmless.
fn tagged_fields(buf: &mut Bytes, budget: usize) -> Result<Vec<(u32, Bytes)>, ParseError> {
    let context = DecodeContext::default();
    let mut found = Vec::new();

    while buf.has_remaining() {
        let (tag, wire_type) = decode_key(buf)?;

        if wire_type != WireType::LengthDelimited {
            skip_field(wire_type, tag, buf, context.clone())?;
            continue;
        }

        let length = decode_varint(buf)? as usize;
        if length > buf.remaining() {
            return Err(ParseError::Truncated);
        }

        // Checked before the push, so the cap bounds the allocation rather than describing it
        // after the fact.
        if found.len() >= budget {
            return Err(ParseError::TooManyRecords);
        }
        found.push((tag, buf.copy_to_bytes(length)));
    }

    Ok(found)
}

fn record_from(message: LogRecordMessage) -> Record {
    Record {
        // `time_unix_nano` is when the event happened and may be absent; `observed_time_unix_nano`
        // is when the collector saw it, which is the better fallback than "now".
        time_unix_nano: match message.time_unix_nano {
            0 => message.observed_time_unix_nano,
            time => time,
        },
        severity_number: message.severity_number,
        body: message.body.and_then(|value| value.into_string()),
        attributes: super::attributes(&message.attributes),
        trace_id: hex(&message.trace_id),
        span_id: hex(&message.span_id),
    }
}

fn hex(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    Some(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[derive(Clone, PartialEq, Message)]
struct ResourceMessage {
    #[prost(message, repeated, tag = "1")]
    attributes: Vec<KeyValueMessage>,
}

#[derive(Clone, PartialEq, Message)]
pub(super) struct LogRecordMessage {
    #[prost(fixed64, tag = "1")]
    time_unix_nano: u64,
    #[prost(int32, tag = "2")]
    severity_number: i32,
    #[prost(message, optional, tag = "5")]
    body: Option<AnyValueMessage>,
    #[prost(message, repeated, tag = "6")]
    attributes: Vec<KeyValueMessage>,
    #[prost(bytes = "vec", tag = "9")]
    trace_id: Vec<u8>,
    #[prost(bytes = "vec", tag = "10")]
    span_id: Vec<u8>,
    #[prost(fixed64, tag = "11")]
    observed_time_unix_nano: u64,
}

#[derive(Clone, PartialEq, Message)]
pub(super) struct KeyValueMessage {
    #[prost(string, tag = "1")]
    pub key: String,
    #[prost(message, optional, tag = "2")]
    pub value: Option<AnyValueMessage>,
}

/// An attribute value.
///
/// Only the four scalar arms are declared. `array_value`, `kvlist_value` and `bytes_value` are
/// skipped as unknown fields: an attribute thermite would have to flatten to display is not worth
/// the recursion budget on a parser reachable from the internet.
#[derive(Clone, PartialEq, Message)]
pub(super) struct AnyValueMessage {
    #[prost(string, optional, tag = "1")]
    string_value: Option<String>,
    #[prost(bool, optional, tag = "2")]
    bool_value: Option<bool>,
    #[prost(int64, optional, tag = "3")]
    int_value: Option<i64>,
    #[prost(double, optional, tag = "4")]
    double_value: Option<f64>,
}

impl AnyValueMessage {
    pub(super) fn into_json(self) -> Option<serde_json::Value> {
        if let Some(value) = self.string_value {
            return Some(value.into());
        }
        if let Some(value) = self.bool_value {
            return Some(value.into());
        }
        if let Some(value) = self.int_value {
            return Some(value.into());
        }
        self.double_value.map(Into::into)
    }

    fn into_string(self) -> Option<String> {
        match self.into_json()? {
            serde_json::Value::String(text) => Some(text),
            other => Some(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use prost::Message as _;

    use super::*;

    /// Builds a `LogsData` the way a collector would, using the same message definitions in
    /// reverse. Encoding is not what is under test — the walk and the caps are.
    fn encode(records: Vec<LogRecordMessage>, resource: Vec<KeyValueMessage>) -> Vec<u8> {
        #[derive(Clone, PartialEq, Message)]
        struct ScopeLogs {
            #[prost(message, repeated, tag = "2")]
            log_records: Vec<LogRecordMessage>,
        }
        #[derive(Clone, PartialEq, Message)]
        struct ResourceLogs {
            #[prost(message, optional, tag = "1")]
            resource: Option<ResourceMessage>,
            #[prost(message, repeated, tag = "2")]
            scope_logs: Vec<ScopeLogs>,
        }
        #[derive(Clone, PartialEq, Message)]
        struct LogsData {
            #[prost(message, repeated, tag = "1")]
            resource_logs: Vec<ResourceLogs>,
        }

        LogsData {
            resource_logs: vec![ResourceLogs {
                resource: Some(ResourceMessage {
                    attributes: resource,
                }),
                scope_logs: vec![ScopeLogs {
                    log_records: records,
                }],
            }],
        }
        .encode_to_vec()
    }

    fn record(severity: i32, body: &str) -> LogRecordMessage {
        LogRecordMessage {
            time_unix_nano: 1_756_000_000_000_000_000,
            severity_number: severity,
            body: Some(AnyValueMessage {
                string_value: Some(body.to_string()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn attribute(key: &str, value: &str) -> KeyValueMessage {
        KeyValueMessage {
            key: key.to_string(),
            value: Some(AnyValueMessage {
                string_value: Some(value.to_string()),
                ..Default::default()
            }),
        }
    }

    #[test]
    fn reads_records_and_their_resource() {
        let body = encode(
            vec![record(17, "payment gateway unreachable")],
            vec![attribute("service.name", "checkout")],
        );

        let groups = decode(&body).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(
            groups[0].resource["service.name"],
            serde_json::Value::from("checkout")
        );

        let record = &groups[0].records[0];
        assert_eq!(record.severity_number, 17);
        assert_eq!(record.body.as_deref(), Some("payment gateway unreachable"));
        assert_eq!(record.time_unix_nano, 1_756_000_000_000_000_000);
    }

    /// The cap has to bound the decode rather than describe it afterwards: an empty record is two
    /// bytes on the wire against about 150 in memory, so an uncapped decode turns a small body
    /// into a large allocation.
    #[test]
    fn refuses_a_batch_past_the_record_cap() {
        let body = encode(
            (0..MAX_RECORDS + 1).map(|_| record(17, "x")).collect(),
            Vec::new(),
        );

        assert!(matches!(decode(&body), Err(ParseError::TooManyRecords)));
    }

    #[test]
    fn accepts_a_batch_at_the_cap() {
        let body = encode(
            (0..MAX_RECORDS).map(|_| record(17, "x")).collect(),
            Vec::new(),
        );

        assert_eq!(decode(&body).unwrap()[0].records.len(), MAX_RECORDS);
    }

    /// A collector one schema version ahead sends fields we do not declare. Refusing those would
    /// make every upgrade of somebody else's collector an outage here.
    #[test]
    fn ignores_fields_it_does_not_know() {
        #[derive(Clone, PartialEq, Message)]
        struct FutureLogsData {
            #[prost(message, repeated, tag = "1")]
            resource_logs: Vec<FutureResourceLogs>,
            #[prost(string, tag = "99")]
            something_new: String,
        }
        #[derive(Clone, PartialEq, Message)]
        struct FutureResourceLogs {
            #[prost(message, repeated, tag = "2")]
            scope_logs: Vec<FutureScopeLogs>,
            #[prost(string, tag = "3")]
            schema_url: String,
        }
        #[derive(Clone, PartialEq, Message)]
        struct FutureScopeLogs {
            #[prost(message, repeated, tag = "2")]
            log_records: Vec<LogRecordMessage>,
            #[prost(uint64, tag = "77")]
            also_new: u64,
        }

        let body = FutureLogsData {
            resource_logs: vec![FutureResourceLogs {
                scope_logs: vec![FutureScopeLogs {
                    log_records: vec![record(17, "still readable")],
                    also_new: 42,
                }],
                schema_url: "https://example.test/schema".to_string(),
            }],
            something_new: "ignored".to_string(),
        }
        .encode_to_vec();

        let groups = decode(&body).unwrap();
        assert_eq!(groups[0].records[0].body.as_deref(), Some("still readable"));
    }

    /// The same ceiling the JSON reader applies, through the shared flattening helper.
    #[test]
    fn attributes_stop_at_the_cap() {
        let mut crowded = record(17, "x");
        crowded.attributes = (0..super::super::MAX_ATTRIBUTES + 50)
            .map(|n| attribute(&format!("k{n:04}"), "v"))
            .collect();

        let body = encode(vec![crowded], Vec::new());
        let groups = decode(&body).unwrap();

        assert_eq!(
            groups[0].records[0].attributes.len(),
            super::super::MAX_ATTRIBUTES
        );
    }

    #[test]
    fn a_truncated_body_is_an_error_not_a_panic() {
        let body = encode(vec![record(17, "x")], Vec::new());

        assert!(decode(&body[..body.len() - 3]).is_err());
    }

    #[test]
    fn trace_ids_come_out_as_hex() {
        let mut with_ids = record(17, "x");
        with_ids.trace_id = vec![0x4b, 0xf9, 0x2f, 0x35];
        with_ids.span_id = vec![0x00, 0xff];

        let body = encode(vec![with_ids], Vec::new());
        let groups = decode(&body).unwrap();

        assert_eq!(groups[0].records[0].trace_id.as_deref(), Some("4bf92f35"));
        assert_eq!(groups[0].records[0].span_id.as_deref(), Some("00ff"));
    }

    /// `time_unix_nano` is optional in the schema. Falling back to the collector's observation is
    /// closer than the arrival time, which is what `digest` would otherwise use.
    #[test]
    fn falls_back_to_the_observed_time() {
        let mut observed_only = record(17, "x");
        observed_only.time_unix_nano = 0;
        observed_only.observed_time_unix_nano = 1_756_000_000_000_000_000;

        let body = encode(vec![observed_only], Vec::new());

        assert_eq!(
            decode(&body).unwrap()[0].records[0].time_unix_nano,
            1_756_000_000_000_000_000
        );
    }
}
