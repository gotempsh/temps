// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Newline-delimited JSON log records, as Vector's `http` sink
//! (`framing.method = "newline_delimited"`, `encoding.codec = "json"`) and
//! Fluent Bit's `http` output (`format json_lines`) send them.
//!
//! One JSON object per line. The envelope keys this module reads are:
//!
//! - **timestamp**: `timestamp`, `@timestamp`, `time`, `ts` or `date`, as an
//!   RFC 3339 string or a number of seconds, milliseconds, microseconds or
//!   nanoseconds since the epoch (told apart by magnitude). Missing means
//!   arrival time. A timestamp more than [`MAX_PAST`] old or [`MAX_FUTURE`]
//!   ahead is replaced by arrival time and kept as the
//!   `ingest_original_timestamp` attribute, so a wrong clock cannot place
//!   lines outside retention or ahead of every live view.
//! - **service**: `service`, `service.name`, `service_name` or `app`.
//! - **stream**: `stream` equal to `stderr` marks the line as stderr.
//!
//! Level, message and attributes are read exactly as for a container's JSON
//! line ([`crate::parser::parse_json_log_object`]).
//!
//! A batch is parsed whole before anything is stored: one bad line refuses
//! the batch with its line number, so a collector never has half a batch
//! stored and the other half retried.

use chrono::{DateTime, Duration, TimeZone, Utc};
use serde_json::{Map, Value};

use crate::error::LogAggregatorError;
use crate::ingest::keys::sanitize_service;
use crate::parser::parse_json_log_object;
use crate::types::{ContainerContext, LogLine, LogStream};

/// Largest request body accepted, before parsing.
pub const MAX_BODY_BYTES: usize = 5 * 1024 * 1024;
/// Most lines in one request.
pub const MAX_LINES_PER_BATCH: usize = 10_000;
/// Largest single line.
pub const MAX_LINE_BYTES: usize = 256 * 1024;
/// Oldest timestamp kept as sent.
pub const MAX_PAST: Duration = Duration::days(7);
/// Furthest-ahead timestamp kept as sent.
pub const MAX_FUTURE: Duration = Duration::minutes(5);

const TIMESTAMP_KEYS: &[&str] = &["timestamp", "@timestamp", "time", "ts", "date"];
const SERVICE_KEYS: &[&str] = &["service", "service.name", "service_name", "app"];

/// One parsed record, before it is given its stream's labels.
#[derive(Debug, Clone)]
pub struct ParsedRecord {
    /// Sanitized service label from the record, if it named one.
    pub service: Option<String>,
    pub ts: DateTime<Utc>,
    pub stream: LogStream,
    /// The record minus its envelope keys.
    pub object: Map<String, Value>,
    /// The record as sent, the message when it has no message key.
    pub raw: String,
}

impl ParsedRecord {
    /// The stored line, labelled with its stream's context.
    pub fn into_line(self, ctx: &ContainerContext) -> LogLine {
        parse_json_log_object(self.object, &self.raw, self.ts, self.stream, ctx)
    }
}

/// Parse a request body into records, all or nothing.
pub fn parse_batch(
    body: &[u8],
    now: DateTime<Utc>,
) -> Result<Vec<ParsedRecord>, LogAggregatorError> {
    if body.len() > MAX_BODY_BYTES {
        return Err(LogAggregatorError::IngestBodyTooLarge {
            limit_bytes: MAX_BODY_BYTES,
        });
    }
    let text = std::str::from_utf8(body).map_err(|e| LogAggregatorError::IngestLineInvalid {
        line: line_of_offset(body, e.valid_up_to()),
        reason: "the body is not valid UTF-8".to_string(),
    })?;

    let mut records = Vec::new();
    for (index, raw) in text.split('\n').enumerate() {
        let line = index + 1;
        let raw = raw.trim_end_matches('\r').trim();
        if raw.is_empty() {
            continue;
        }
        if records.len() >= MAX_LINES_PER_BATCH {
            return Err(LogAggregatorError::IngestLineInvalid {
                line,
                reason: format!(
                    "a batch holds at most {MAX_LINES_PER_BATCH} lines; split the request"
                ),
            });
        }
        if raw.len() > MAX_LINE_BYTES {
            return Err(LogAggregatorError::IngestLineInvalid {
                line,
                reason: format!(
                    "the line is {} bytes; the limit is {MAX_LINE_BYTES}",
                    raw.len()
                ),
            });
        }
        let object = match serde_json::from_str::<Value>(raw) {
            Ok(Value::Object(object)) => object,
            Ok(_) => {
                return Err(LogAggregatorError::IngestLineInvalid {
                    line,
                    reason: "each line must be a JSON object".to_string(),
                })
            }
            Err(e) => {
                return Err(LogAggregatorError::IngestLineInvalid {
                    line,
                    reason: format!("not JSON: {e}"),
                })
            }
        };
        records.push(parse_record(object, raw, now));
    }
    Ok(records)
}

fn parse_record(mut object: Map<String, Value>, raw: &str, now: DateTime<Utc>) -> ParsedRecord {
    let mut ts = now;
    if let Some(value) = take_first(&mut object, TIMESTAMP_KEYS) {
        match parse_timestamp(&value) {
            Some(parsed) if parsed >= now - MAX_PAST && parsed <= now + MAX_FUTURE => ts = parsed,
            _ => {
                object.insert("ingest_original_timestamp".to_string(), value);
            }
        }
    }
    let service = take_first(&mut object, SERVICE_KEYS)
        .and_then(|value| value.as_str().and_then(sanitize_service));
    let stream = match object.get("stream").and_then(Value::as_str) {
        Some(s) if s.eq_ignore_ascii_case("stderr") => {
            object.remove("stream");
            LogStream::Stderr
        }
        Some(s) if s.eq_ignore_ascii_case("stdout") => {
            object.remove("stream");
            LogStream::Stdout
        }
        _ => LogStream::Stdout,
    };
    ParsedRecord {
        service,
        ts,
        stream,
        object,
        raw: raw.to_string(),
    }
}

fn take_first(object: &mut Map<String, Value>, keys: &[&str]) -> Option<Value> {
    keys.iter().find_map(|key| object.remove(*key))
}

/// RFC 3339 text, or epoch seconds/ms/µs/ns told apart by magnitude.
fn parse_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    match value {
        Value::String(text) => DateTime::parse_from_rfc3339(text.trim())
            .ok()
            .map(|t| t.with_timezone(&Utc))
            .or_else(|| text.trim().parse::<f64>().ok().and_then(from_epoch)),
        Value::Number(number) => number.as_f64().and_then(from_epoch),
        _ => None,
    }
}

fn from_epoch(value: f64) -> Option<DateTime<Utc>> {
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    // Seconds until the year 5138, then milliseconds, microseconds, nanos.
    let nanos = if value < 1e11 {
        value * 1e9
    } else if value < 1e14 {
        value * 1e6
    } else if value < 1e17 {
        value * 1e3
    } else {
        value
    };
    if nanos > i64::MAX as f64 {
        return None;
    }
    Some(Utc.timestamp_nanos(nanos as i64))
}

fn line_of_offset(body: &[u8], offset: usize) -> usize {
    body[..offset.min(body.len())]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
        + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LogLevel;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0)
            .single()
            .expect("valid date")
    }

    fn ctx() -> ContainerContext {
        ContainerContext {
            project_id: 7,
            external_service_id: None,
            env: "3".to_string(),
            service: "api".to_string(),
            container_id: "ingest-1-api".to_string(),
            deploy_id: None,
        }
    }

    #[test]
    fn a_vector_style_record_keeps_its_timestamp_level_message_and_attributes() {
        let body = br#"{"timestamp":"2026-11-19T11:59:30.250Z","level":"error","message":"payment failed","service":"Checkout","order_id":42}"#;
        let records = parse_batch(body, now()).expect("parses");
        assert_eq!(records.len(), 1);
        let record = records[0].clone();
        assert_eq!(record.service.as_deref(), Some("checkout"));
        assert_eq!(record.ts.to_rfc3339(), "2026-11-19T11:59:30.250+00:00");
        let line = record.into_line(&ctx());
        assert_eq!(line.level, LogLevel::Error);
        assert_eq!(line.msg, "payment failed");
        assert_eq!(line.project_id, 7);
        let fields = line.fields.expect("attributes are kept");
        assert_eq!(fields["order_id"], serde_json::json!(42));
        assert!(
            fields.get("timestamp").is_none(),
            "envelope keys are not attributes"
        );
        assert!(fields.get("service").is_none());
    }

    #[test]
    fn a_fluent_bit_style_record_with_epoch_seconds_and_stderr_parses() {
        let sent = now().timestamp() - 30;
        let body =
            format!("{{\"date\":{sent}.5,\"log\":\"disk almost full\",\"stream\":\"stderr\"}}\n");
        let records = parse_batch(body.as_bytes(), now()).expect("parses");
        let record = records[0].clone();
        assert_eq!(record.stream, LogStream::Stderr);
        assert_eq!(record.ts.timestamp(), sent);
        assert_eq!(record.ts.timestamp_subsec_millis(), 500);
        assert_eq!(record.into_line(&ctx()).msg, "disk almost full");
    }

    #[test]
    fn epoch_units_are_told_apart_by_magnitude() {
        let expected = now().timestamp();
        for value in [
            serde_json::json!(expected),
            serde_json::json!(expected * 1_000),
            serde_json::json!(expected * 1_000_000),
            serde_json::json!(expected as f64 * 1e9),
        ] {
            assert_eq!(
                parse_timestamp(&value).map(|t| t.timestamp()),
                Some(expected),
                "{value}"
            );
        }
    }

    #[test]
    fn a_timestamp_outside_the_window_becomes_arrival_time_and_is_kept() {
        let body = br#"{"ts":"1999-01-01T00:00:00Z","msg":"old"}
{"ts":"2030-01-01T00:00:00Z","msg":"future"}"#;
        let records = parse_batch(body, now()).expect("parses");
        for record in records {
            assert_eq!(record.ts, now());
            assert!(record.object.contains_key("ingest_original_timestamp"));
        }
    }

    #[test]
    fn blank_lines_and_crlf_are_ignored() {
        let body = b"{\"msg\":\"a\"}\r\n\r\n   \n{\"msg\":\"b\"}\n";
        assert_eq!(parse_batch(body, now()).expect("parses").len(), 2);
    }

    #[test]
    fn one_bad_line_refuses_the_batch_and_names_the_line() {
        for (body, line) in [
            (&b"{\"msg\":\"a\"}\nnot json\n"[..], 2),
            (&b"{\"msg\":\"a\"}\n\n[1,2]\n"[..], 3),
            (&b"\"just a string\""[..], 1),
        ] {
            match parse_batch(body, now()) {
                Err(LogAggregatorError::IngestLineInvalid { line: got, .. }) => {
                    assert_eq!(got, line)
                }
                other => panic!("unexpected: {other:?}"),
            }
        }
    }

    #[test]
    fn the_size_limits_are_enforced() {
        let huge = vec![b' '; MAX_BODY_BYTES + 1];
        assert!(matches!(
            parse_batch(&huge, now()),
            Err(LogAggregatorError::IngestBodyTooLarge { .. })
        ));

        let long_line = format!("{{\"msg\":\"{}\"}}", "x".repeat(MAX_LINE_BYTES));
        assert!(matches!(
            parse_batch(long_line.as_bytes(), now()),
            Err(LogAggregatorError::IngestLineInvalid { line: 1, .. })
        ));

        let many = "{}\n".repeat(MAX_LINES_PER_BATCH + 1);
        assert!(matches!(
            parse_batch(many.as_bytes(), now()),
            Err(LogAggregatorError::IngestLineInvalid { .. })
        ));
    }

    #[test]
    fn invalid_utf8_is_refused_with_its_line() {
        let body = b"{\"msg\":\"a\"}\n{\"msg\":\"\xff\"}";
        assert!(matches!(
            parse_batch(body, now()),
            Err(LogAggregatorError::IngestLineInvalid { line: 2, .. })
        ));
    }
}
