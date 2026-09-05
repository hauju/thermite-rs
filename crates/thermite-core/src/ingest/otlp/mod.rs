//! OTLP logs ingest: `POST /api/{project_id}/otlp/v1/logs`.
//!
//! A second *reader*, not a second pipeline. An OTel log record is converted to the Sentry-shaped
//! payload the rest of ingest already takes (`convert`), and from there grouping, the digest, the
//! rollups, retention, alerting and triage are all the code that was already there. Nothing past
//! `convert` knows an event arrived over OTLP.
//!
//! **Only `ERROR` and above is stored.** A collector is usually forwarding an application's whole
//! log stream, so the severity floor is the difference between this being error tracking and being
//! a log sink with a per-line issue. Records below the floor are dropped with a `200` and counted
//! as `unsupported:log`, the same treatment a transaction gets on the envelope endpoint — a drop
//! that leaves no trace is indistinguishable from a client that never sent anything.
//!
//! Credentials are the project's DSN key, exactly as for envelopes: `?sentry_key=` on the endpoint
//! URL, or `X-Sentry-Auth`. An OTel exporter can set either — the endpoint is configured as a full
//! URL, and `OTEL_EXPORTER_OTLP_HEADERS` sets the header. There is no envelope here, so the `dsn`
//! header that is an envelope's last credential source does not exist.
//!
//! Both OTLP/HTTP encodings are accepted, because the SDKs disagree about which to default to:
//! `http/protobuf` is the spec's default and what most language SDKs send, while the JS SDK and
//! several collectors send JSON.

mod convert;
mod json;
mod proto;

use std::collections::BTreeMap;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use serde_json::{Map, Value};

use super::{IngestQuery, Project, authorize, check_quota, digest, log_digested, outcomes};
use crate::error::{AppError, AppResult};
use crate::protocol::scrub;
use crate::state::ThermiteState;

/// Attributes read from one resource or record.
///
/// Attribute sets are client-controlled, and both readers build a map per group. The cap is the
/// same reasoning as `MAX_RECORDS`: a bound on what a small body can allocate.
const MAX_ATTRIBUTES: usize = 128;

/// One resource's records. Mirrors OTLP's own shape, which keeps the resource attributes off every
/// record rather than cloned onto each of them.
#[derive(Debug, Default)]
pub struct Group {
    pub resource: BTreeMap<String, Value>,
    pub records: Vec<Record>,
}

/// One log record, reduced to what thermite reads.
#[derive(Debug)]
pub struct Record {
    /// Nanoseconds since the epoch, or zero when the record carried no time.
    pub time_unix_nano: u64,
    /// OTel's 1-24 severity scale. Zero means unspecified.
    pub severity_number: i32,
    pub body: Option<String>,
    pub attributes: BTreeMap<String, Value>,
    /// Hex, as OTLP/JSON spells it — the protobuf reader converts.
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
}

pub async fn logs_endpoint(
    State(state): State<ThermiteState>,
    Path(project_id): Path<i64>,
    Query(query): Query<IngestQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<impl IntoResponse> {
    let received_at = chrono::Utc::now();

    // No body is passed: authentication precedes decoding here as it does for envelopes, and OTLP
    // has no in-body credential to recover, so there is nothing to look for.
    let project = authorize(&state, project_id, &query, &headers, None, received_at).await?;

    // Decoded before the encoding is sniffed. An OTel exporter compresses by default, and a
    // gzip body starts with 0x1f — so sniffing the bytes as they arrived makes the fallback below
    // useless for precisely the requests it exists to catch.
    let body = super::decode_body(&state, &headers, &body)?;
    let json = is_json(&headers, &body);

    let groups = if json {
        json::decode(&body, proto::MAX_RECORDS).map_err(bad_request)?
    } else {
        proto::decode(&body).map_err(bad_request)?
    };

    let mut counts = outcomes::Counts::default();

    for group in &groups {
        let resource: Map<String, Value> = group
            .resource
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();

        for record in &group.records {
            if !record.is_error() {
                // Everything a collector forwards that is not an error. Counted, because "we are
                // dropping 40k records an hour" is worth seeing on the dashboard.
                counts.bump_unsupported("log");
                continue;
            }

            // One unit of quota per stored record, as for one event item in an envelope. Hitting
            // the limit mid-batch returns 429 with what was already digested left committed; the
            // exporter retries the batch.
            if let Err(e) = check_quota(&state, project.id) {
                counts.bump(outcomes::Outcome::OverQuota);
                counts.flush(&state.db, project.id, received_at).await?;
                return Err(e);
            }

            digest_record(
                &state,
                &project,
                record,
                &resource,
                received_at,
                &mut counts,
            )
            .await?;
        }
    }

    counts.flush(&state.db, project.id, received_at).await?;

    Ok(accepted(json))
}

async fn digest_record(
    state: &ThermiteState,
    project: &Project,
    record: &Record,
    resource: &Map<String, Value>,
    received_at: chrono::DateTime<chrono::Utc>,
    counts: &mut outcomes::Counts,
) -> AppResult<()> {
    let mut payload = convert::to_event(record, resource);

    // Scrub before digest so a credential in an attribute is never written at all — the same
    // ordering the envelope path uses, and the reason this is not done inside `convert`.
    scrub::scrub(&mut payload, &state.config.scrub_fields);

    let digested = digest::digest(
        &state.db,
        project.id,
        project.component.as_deref(),
        &payload,
        received_at,
    )
    .await?;

    counts.bump(outcomes::Outcome::Accepted);
    log_digested(project.id, &digested);
    Ok(())
}

/// Which encoding the body is in.
///
/// `Content-Type` decides, since it is the only thing that distinguishes them and every OTLP
/// exporter sets it. The leading-brace check is the fallback for an exporter that does not: a
/// protobuf `LogsData` begins with the key byte `0x0a`, never `{`, so the two cannot be confused.
///
/// Takes the *decoded* body. Sniffing what arrived on the wire reads the compression header
/// instead of the payload, which is a fallback that never fires for a compressed request.
fn is_json(headers: &HeaderMap, body: &[u8]) -> bool {
    let declared = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("json"));

    declared || body.first() == Some(&b'{')
}

/// OTLP's success response is an `ExportLogsServiceResponse`, which is empty when nothing was
/// rejected: `{}` in JSON, zero bytes in protobuf. Answering in the encoding the request used is
/// what keeps a protobuf exporter from trying to parse JSON.
///
/// Records dropped below the severity floor are deliberately *not* reported as
/// `partial_success.rejected_log_records`: they were not rejected, they were filtered, and a
/// collector told otherwise logs a warning on every export.
fn accepted(json: bool) -> impl IntoResponse {
    let (content_type, body) = match json {
        true => ("application/json", "{}".as_bytes()),
        false => ("application/x-protobuf", [].as_slice()),
    };

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, content_type)],
        body.to_vec(),
    )
}

fn bad_request(error: impl std::fmt::Display) -> AppError {
    AppError::BadRequest(error.to_string())
}

/// Flattens protobuf `KeyValue`s into the map both readers produce.
fn attributes(attributes: &[proto::KeyValueMessage]) -> BTreeMap<String, Value> {
    attributes
        .iter()
        .take(MAX_ATTRIBUTES)
        .filter_map(|attribute| {
            let value = attribute.value.clone()?.into_json()?;
            Some((attribute.key.clone(), value))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use prost::Message;
    use serde_json::json;

    use super::*;

    /// The two encodings are read by two hand-written readers, so nothing but a test keeps them
    /// producing the same thing. This is the OTLP counterpart of the SDK parity test: one logical
    /// record, both encodings, identical events out.
    #[test]
    fn equivalent_encodings_produce_the_same_records() {
        #[derive(Clone, PartialEq, Message)]
        struct AnyValueMsg {
            #[prost(string, optional, tag = "1")]
            string_value: Option<String>,
        }
        #[derive(Clone, PartialEq, Message)]
        struct KeyValueMsg {
            #[prost(string, tag = "1")]
            key: String,
            #[prost(message, optional, tag = "2")]
            value: Option<AnyValueMsg>,
        }
        #[derive(Clone, PartialEq, Message)]
        struct LogRecordMsg {
            #[prost(fixed64, tag = "1")]
            time_unix_nano: u64,
            #[prost(int32, tag = "2")]
            severity_number: i32,
            #[prost(message, optional, tag = "5")]
            body: Option<AnyValueMsg>,
            #[prost(message, repeated, tag = "6")]
            attributes: Vec<KeyValueMsg>,
            #[prost(bytes = "vec", tag = "9")]
            trace_id: Vec<u8>,
        }
        #[derive(Clone, PartialEq, Message)]
        struct ResourceMsg {
            #[prost(message, repeated, tag = "1")]
            attributes: Vec<KeyValueMsg>,
        }
        #[derive(Clone, PartialEq, Message)]
        struct ScopeLogsMsg {
            #[prost(message, repeated, tag = "2")]
            log_records: Vec<LogRecordMsg>,
        }
        #[derive(Clone, PartialEq, Message)]
        struct ResourceLogsMsg {
            #[prost(message, optional, tag = "1")]
            resource: Option<ResourceMsg>,
            #[prost(message, repeated, tag = "2")]
            scope_logs: Vec<ScopeLogsMsg>,
        }
        #[derive(Clone, PartialEq, Message)]
        struct LogsDataMsg {
            #[prost(message, repeated, tag = "1")]
            resource_logs: Vec<ResourceLogsMsg>,
        }

        let string = |text: &str| {
            Some(AnyValueMsg {
                string_value: Some(text.to_string()),
            })
        };
        let pair = |key: &str, value: &str| KeyValueMsg {
            key: key.to_string(),
            value: string(value),
        };

        let protobuf = LogsDataMsg {
            resource_logs: vec![ResourceLogsMsg {
                resource: Some(ResourceMsg {
                    attributes: vec![
                        pair("service.name", "checkout"),
                        pair("service.version", "1.4.2"),
                    ],
                }),
                scope_logs: vec![ScopeLogsMsg {
                    log_records: vec![LogRecordMsg {
                        time_unix_nano: 1_756_000_000_000_000_000,
                        severity_number: 17,
                        body: string("payment gateway unreachable"),
                        attributes: vec![
                            pair("exception.type", "ConnectionError"),
                            pair("exception.message", "connection refused"),
                        ],
                        trace_id: vec![0x4b, 0xf9, 0x2f, 0x35],
                    }],
                }],
            }],
        }
        .encode_to_vec();

        let as_json = json!({
            "resourceLogs": [{
                "resource": { "attributes": [
                    { "key": "service.name", "value": { "stringValue": "checkout" } },
                    { "key": "service.version", "value": { "stringValue": "1.4.2" } },
                ]},
                "scopeLogs": [{ "logRecords": [{
                    "timeUnixNano": "1756000000000000000",
                    "severityNumber": 17,
                    "body": { "stringValue": "payment gateway unreachable" },
                    "attributes": [
                        { "key": "exception.type", "value": { "stringValue": "ConnectionError" } },
                        { "key": "exception.message", "value": { "stringValue": "connection refused" } },
                    ],
                    "traceId": "4bf92f35",
                }]}],
            }]
        })
        .to_string();

        let event_of = |groups: Vec<Group>| {
            let group = groups.into_iter().next().unwrap();
            let resource: Map<String, Value> = group.resource.into_iter().collect();
            convert::to_event(&group.records[0], &resource)
        };

        assert_eq!(
            event_of(proto::decode(&protobuf).unwrap()),
            event_of(json::decode(as_json.as_bytes(), 100).unwrap()),
        );
    }

    #[test]
    fn the_encoding_is_taken_from_the_content_type() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        assert!(is_json(&headers, b"\x0a\x00"));

        headers.insert(
            header::CONTENT_TYPE,
            "application/x-protobuf".parse().unwrap(),
        );
        assert!(!is_json(&headers, b"\x0a\x00"));
    }

    /// A protobuf `LogsData` starts with the key byte for field 1, so a leading brace can only be
    /// JSON — which makes an exporter that forgets the header work anyway.
    #[test]
    fn a_leading_brace_is_json_whatever_the_header_says() {
        assert!(is_json(&HeaderMap::new(), b"{}"));
        assert!(!is_json(&HeaderMap::new(), b"\x0a\x00"));
    }
}
