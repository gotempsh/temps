// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Transactions and standalone Sentry spans -> native SpanRecord batches.
//! Baseline: sentry-javascript 10.17.0 / 11.8.0 transaction serialization.
//! No error issue is created for a transaction. Source status/op, measurements,
//! tags and data remain attributes; parent IDs are never invented.

use super::{envelope::EnvelopeError, logs};
use crate::providers::AuthContext;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{value::RawValue, Value};
use std::collections::BTreeMap;
use temps_otel::types::{ResourceInfo, SpanKind, SpanRecord, SpanStatusCode};

#[derive(Deserialize)]
struct Trace {
    trace_id: String,
    span_id: String,
    parent_span_id: Option<String>,
    op: Option<String>,
    description: Option<String>,
    status: Option<String>,
    origin: Option<String>,
    links: Option<Value>,
    #[serde(default)]
    data: BTreeMap<String, Value>,
}
#[derive(Deserialize)]
struct Contexts {
    trace: Trace,
}
#[derive(Deserialize)]
struct Transaction<'a> {
    transaction: String,
    #[serde(borrow)]
    start_timestamp: &'a RawValue,
    #[serde(borrow)]
    timestamp: &'a RawValue,
    contexts: Contexts,
    #[serde(default, borrow, deserialize_with = "logs::bounded_logs")]
    spans: Vec<&'a RawValue>,
    release: Option<String>,
    environment: Option<String>,
    #[serde(default)]
    tags: BTreeMap<String, Value>,
    measurements: Option<Value>,
}
#[derive(Deserialize)]
struct Span<'a> {
    #[serde(flatten)]
    trace: Trace,
    #[serde(borrow)]
    start_timestamp: &'a RawValue,
    #[serde(borrow)]
    timestamp: &'a RawValue,
    #[serde(default)]
    tags: BTreeMap<String, Value>,
}
fn invalid() -> EnvelopeError {
    EnvelopeError::InvalidPayload("invalid or unsupported Sentry transaction/span".into())
}

pub(super) fn time(raw: &RawValue) -> Result<DateTime<Utc>, EnvelopeError> {
    if raw.get().len() > 64 {
        return Err(invalid());
    }
    if raw.get().starts_with('"') {
        let text: String = serde_json::from_str(raw.get()).map_err(|_| invalid())?;
        DateTime::parse_from_rfc3339(&text)
            .map(|t| t.to_utc())
            .map_err(|_| invalid())
    } else {
        logs::timestamp(raw.get()).ok_or_else(invalid)
    }
}

fn attributes(values: BTreeMap<String, Value>) -> Result<BTreeMap<String, String>, EnvelopeError> {
    if values.len() > 128 {
        return Err(invalid());
    }
    values
        .into_iter()
        .map(|(key, value)| {
            let text = if let Value::String(s) = value {
                s
            } else {
                value.to_string()
            };
            if key.len() > 128
                || key.starts_with("temps.sentry.")
                || key.contains('\0')
                || text.len() > 16 * 1024
                || text.contains('\0')
            {
                return Err(invalid());
            }
            Ok((key, text))
        })
        .collect()
}

fn map(
    mut trace: Trace,
    start: &RawValue,
    end: &RawValue,
    name: Option<String>,
    tags: BTreeMap<String, Value>,
    resource: ResourceInfo,
    auth: &AuthContext,
) -> Result<SpanRecord, EnvelopeError> {
    if !logs::valid_id(&trace.trace_id, 32)
        || !logs::valid_id(&trace.span_id, 16)
        || trace
            .parent_span_id
            .as_ref()
            .is_some_and(|id| !logs::valid_id(id, 16))
    {
        return Err(invalid());
    }
    // Python SDK 2.42.1 serializes a child span's status as tags.status;
    // context/root status and explicit top-level status keep precedence.
    if trace.status.is_none() {
        trace.status = tags
            .get("status")
            .and_then(Value::as_str)
            .filter(|status| {
                matches!(
                    *status,
                    "ok" | "unknown"
                        | "unknown_error"
                        | "cancelled"
                        | "invalid_argument"
                        | "deadline_exceeded"
                        | "not_found"
                        | "already_exists"
                        | "permission_denied"
                        | "resource_exhausted"
                        | "failed_precondition"
                        | "aborted"
                        | "out_of_range"
                        | "unimplemented"
                        | "internal_error"
                        | "unavailable"
                        | "data_loss"
                        | "unauthenticated"
                )
            })
            .map(str::to_owned);
    }
    let start_time = time(start)?;
    let end_time = time(end)?;
    if end_time < start_time
        || end_time.signed_duration_since(start_time) > chrono::Duration::days(7)
    {
        return Err(invalid());
    }
    let name = name
        .or(trace.description)
        .or_else(|| trace.op.clone())
        .unwrap_or_else(|| "sentry.span".into());
    if name.len() > 1024
        || name.contains('\0')
        || trace
            .status
            .as_ref()
            .is_some_and(|s| s.len() > 256 || s.contains('\0'))
        || std::iter::once(Some(resource.service_name.as_str()))
            .chain([
                resource.service_version.as_deref(),
                resource.deployment_environment.as_deref(),
            ])
            .flatten()
            .any(|s| s.len() > 16 * 1024 || s.contains('\0'))
    {
        return Err(invalid());
    }
    let mut attributes = attributes(trace.data)?;
    if let Some(links) = trace.links {
        let values = links.as_array().ok_or_else(invalid)?;
        if values.len() > 128 || links.to_string().len() > 32 * 1024 {
            return Err(invalid());
        }
        attributes.insert("temps.sentry.links".into(), links.to_string());
    }
    // The native map is flat; retain data before a tag with the same name
    // wins the searchable key. Tags retain their source representation too.
    let source_tags = serde_json::to_string(&tags).map_err(|_| invalid())?;
    if tags.keys().any(|key| attributes.contains_key(key)) {
        attributes.insert(
            "temps.sentry.data".into(),
            serde_json::to_string(&attributes).map_err(|_| invalid())?,
        );
    }
    attributes.extend(self::attributes(tags)?);
    attributes.insert("temps.sentry.tags".into(), source_tags);
    attributes.insert("temps.sentry.start_timestamp".into(), start.get().into());
    attributes.insert("temps.sentry.timestamp".into(), end.get().into());
    if let Some(id) = auth.environment_id {
        attributes.insert("temps.sentry.environment_id".into(), id.to_string());
    }
    let status_code = match trace.status.as_deref() {
        Some("ok") => SpanStatusCode::Ok,
        None | Some("unknown") => SpanStatusCode::Unset,
        _ => SpanStatusCode::Error,
    };
    let status_message = trace.status.unwrap_or_default();
    let kind = match attributes.get("sentry.kind").map(String::as_str) {
        Some("1" | "internal") => SpanKind::Internal,
        Some("2" | "server") => SpanKind::Server,
        Some("3" | "client") => SpanKind::Client,
        Some("4" | "producer") => SpanKind::Producer,
        Some("5" | "consumer") => SpanKind::Consumer,
        _ => SpanKind::Unspecified,
    };
    if let Some(origin) = trace.origin {
        if origin.len() > 256 || origin.contains('\0') {
            return Err(invalid());
        }
        attributes.insert("sentry.origin".into(), origin);
    }
    if let Some(op) = trace.op {
        if op.len() > 256 || op.contains('\0') {
            return Err(invalid());
        }
        attributes.insert("sentry.op".into(), op);
    }
    Ok(SpanRecord {
        ingest_identity: None,
        project_id: auth.project_id,
        deployment_id: auth.deployment_id,
        resource,
        trace_id: trace.trace_id.to_ascii_lowercase(),
        span_id: trace.span_id.to_ascii_lowercase(),
        parent_span_id: trace.parent_span_id.map(|id| id.to_ascii_lowercase()),
        name,
        kind,
        start_time,
        end_time,
        duration_ms: (end_time - start_time)
            .num_microseconds()
            .ok_or_else(invalid)? as f64
            / 1000.0,
        status_code,
        status_message,
        attributes,
        events: vec![],
    })
}

pub fn transaction(payload: &[u8], auth: &AuthContext) -> Result<Vec<SpanRecord>, EnvelopeError> {
    let event: Transaction<'_> = serde_json::from_slice(payload).map_err(|_| invalid())?;
    if event.spans.len() >= 1000 {
        return Err(invalid());
    }
    let resource = ResourceInfo {
        service_name: event
            .contexts
            .trace
            .data
            .get("service.name")
            .and_then(Value::as_str)
            .unwrap_or("sentry")
            .into(),
        service_version: event.release,
        deployment_environment: event.environment,
        ..Default::default()
    };
    let trace_id = event.contexts.trace.trace_id.to_ascii_lowercase();
    let mut root = map(
        event.contexts.trace,
        event.start_timestamp,
        event.timestamp,
        Some(event.transaction),
        event.tags,
        resource.clone(),
        auth,
    )?;
    if let Some(measurements) = event.measurements {
        if measurements.to_string().len() > 32 * 1024 {
            return Err(invalid());
        }
        root.attributes
            .insert("temps.sentry.measurements".into(), measurements.to_string());
    }
    let mut spans = Vec::with_capacity(event.spans.len() + 1);
    spans.push(root);
    for raw in event.spans {
        let span: Span<'_> = serde_json::from_str(raw.get()).map_err(|_| invalid())?;
        if span.trace.trace_id.to_ascii_lowercase() != trace_id {
            return Err(invalid());
        }
        spans.push(map(
            span.trace,
            span.start_timestamp,
            span.timestamp,
            None,
            span.tags,
            resource.clone(),
            auth,
        )?);
    }
    Ok(spans)
}

pub fn standalone(payload: &[u8], auth: &AuthContext) -> Result<SpanRecord, EnvelopeError> {
    let span: Span<'_> = serde_json::from_slice(payload).map_err(|_| invalid())?;
    let resource = ResourceInfo {
        service_name: span
            .trace
            .data
            .get("service.name")
            .and_then(Value::as_str)
            .unwrap_or("sentry")
            .into(),
        service_version: span
            .trace
            .data
            .get("sentry.release")
            .and_then(Value::as_str)
            .map(str::to_owned),
        deployment_environment: span
            .trace
            .data
            .get("sentry.environment")
            .and_then(Value::as_str)
            .map(str::to_owned),
        ..Default::default()
    };
    map(
        span.trace,
        span.start_timestamp,
        span.timestamp,
        None,
        span.tags,
        resource,
        auth,
    )
}

#[derive(Deserialize)]
struct StreamedSpan<'a> {
    name: String,
    trace_id: String,
    span_id: String,
    parent_span_id: Option<String>,
    status: Option<String>,
    is_segment: Option<bool>,
    links: Option<Value>,
    #[serde(borrow)]
    start_timestamp: &'a RawValue,
    #[serde(borrow)]
    end_timestamp: &'a RawValue,
    #[serde(default, borrow, deserialize_with = "logs::bounded_attributes")]
    attributes: BTreeMap<String, &'a RawValue>,
}

/// Modern span.v2 batches used by default in sentry-javascript 11.8.0.
pub fn streamed(
    header: &super::envelope::ItemHeader,
    payload: &[u8],
    auth: &AuthContext,
) -> Result<Vec<SpanRecord>, EnvelopeError> {
    if header.content_type.as_deref() != Some("application/vnd.sentry.items.span.v2+json")
        || header.item_count.is_none_or(|n| n > 1000)
    {
        return Err(invalid());
    }
    let batch: logs::Batch<'_> = serde_json::from_slice(payload).map_err(|_| invalid())?;
    if batch.version != Some(2) || header.item_count != Some(batch.items.len()) {
        return Err(invalid());
    }
    super::inference::validate(batch.ingest_settings.as_ref())?;
    let mut records = Vec::with_capacity(batch.items.len());
    for raw in batch.items {
        let span: StreamedSpan<'_> = serde_json::from_str(raw.get()).map_err(|_| invalid())?;
        let mut attrs = logs::decode_attributes_with_arrays(&span.attributes, true)?;
        let resource = ResourceInfo {
            service_name: attrs
                .get("service.name")
                .cloned()
                .unwrap_or_else(|| "sentry".into()),
            service_version: attrs.get("sentry.release").cloned(),
            deployment_environment: attrs.get("sentry.environment").cloned(),
            ..Default::default()
        };
        let trace = Trace {
            trace_id: span.trace_id,
            span_id: span.span_id,
            parent_span_id: span.parent_span_id,
            op: attrs.get("sentry.op").cloned(),
            description: None,
            status: span.status,
            origin: attrs.get("sentry.origin").cloned(),
            links: span.links,
            data: attrs
                .get("sentry.kind")
                .map(|kind| BTreeMap::from([("sentry.kind".into(), Value::String(kind.clone()))]))
                .unwrap_or_default(),
        };
        let mut record = map(
            trace,
            span.start_timestamp,
            span.end_timestamp,
            Some(span.name),
            BTreeMap::new(),
            resource,
            auth,
        )?;
        attrs.insert(
            "temps.sentry.attributes".into(),
            serde_json::to_string(&span.attributes).map_err(|_| invalid())?,
        );
        if let Some(segment) = span.is_segment {
            attrs.insert("temps.sentry.is_segment".into(), segment.to_string());
        }
        if record.status_code == SpanStatusCode::Error {
            if let Some(message) = attrs.get("sentry.status_message") {
                record.status_message = message.clone();
            }
        }
        record.attributes.extend(attrs);
        records.push(record);
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn auth() -> AuthContext {
        AuthContext {
            project_id: 41,
            environment_id: Some(42),
            deployment_id: Some(43),
            rate_limit_per_minute: None,
        }
    }
    fn payload() -> Value {
        serde_json::json!({"transaction":"Synthetic root","start_timestamp":1790000000.1234567,"timestamp":1790000001.25,"release":"synthetic-release","environment":"test","contexts":{"trace":{"trace_id":"11111111111111111111111111111111","span_id":"2222222222222222","op":"http.server","status":"ok"}},"spans":[{"trace_id":"11111111111111111111111111111111","span_id":"3333333333333333","parent_span_id":"2222222222222222","start_timestamp":1790000000.5,"timestamp":1790000001.0,"description":"Synthetic child","data":{"count":7},"status":"internal_error"}]})
    }
    #[test]
    fn legacy_python_child_status_and_span_origins_are_preserved() {
        let mut event = payload();
        event["contexts"]["trace"]["origin"] = serde_json::json!("manual");
        event["spans"][0].as_object_mut().unwrap().remove("status");
        event["spans"][0]["tags"] = serde_json::json!({"status":"internal_error"});
        let records = transaction(&serde_json::to_vec(&event).unwrap(), &auth()).unwrap();
        assert_eq!(records[0].attributes["sentry.origin"], "manual");
        assert_eq!(records[1].status_code, SpanStatusCode::Error);
        assert_eq!(records[1].status_message, "internal_error");
        event["spans"][0]["status"] = serde_json::json!("ok");
        assert_eq!(
            transaction(&serde_json::to_vec(&event).unwrap(), &auth()).unwrap()[1].status_code,
            SpanStatusCode::Ok
        );
        event["spans"][0].as_object_mut().unwrap().remove("status");
        event["spans"][0]["tags"]["status"] = serde_json::json!("custom tag");
        assert_eq!(
            transaction(&serde_json::to_vec(&event).unwrap(), &auth()).unwrap()[1].status_code,
            SpanStatusCode::Unset
        );
    }
    #[test]
    fn transactions_preserve_tree_resource_status_and_scope() {
        let spans = transaction(&serde_json::to_vec(&payload()).unwrap(), &auth()).unwrap();
        assert_eq!(spans.len(), 2);
        assert_eq!(
            spans[1].parent_span_id.as_deref(),
            Some(spans[0].span_id.as_str())
        );
        assert_eq!(
            spans[1].resource.service_version.as_deref(),
            Some("synthetic-release")
        );
        assert_eq!(spans[1].deployment_id, Some(43));
        assert_eq!(spans[1].status_code, SpanStatusCode::Error);
        assert_eq!(spans[0].duration_ms, 1126.543);
        assert_eq!(spans[1].attributes["count"], "7");
        let mut collision = payload();
        collision["spans"][0]["tags"] = serde_json::json!({"count":"synthetic-tag"});
        let mapped = transaction(&serde_json::to_vec(&collision).unwrap(), &auth()).unwrap();
        assert_eq!(mapped[1].attributes["count"], "synthetic-tag");
        assert!(mapped[1].attributes["temps.sentry.data"].contains("7"));
        let mut bad = payload();
        bad["spans"][0]["trace_id"] = serde_json::json!("99999999999999999999999999999999");
        assert!(transaction(&serde_json::to_vec(&bad).unwrap(), &auth()).is_err());
        bad = payload();
        bad["release"] = serde_json::json!("synthetic\0release");
        assert!(transaction(&serde_json::to_vec(&bad).unwrap(), &auth()).is_err());
        bad = payload();
        bad["timestamp"] = serde_json::json!(1);
        assert!(transaction(&serde_json::to_vec(&bad).unwrap(), &auth()).is_err());
    }
    #[test]
    fn modern_streamed_spans_preserve_typed_attributes_and_validate_batch_contract() {
        let header=serde_json::from_value(serde_json::json!({"type":"span","item_count":1,"content_type":"application/vnd.sentry.items.span.v2+json"})).unwrap();
        let mut payload = serde_json::json!({"version":2,"items":[{"name":"Synthetic streamed span","trace_id":"11111111111111111111111111111111","span_id":"2222222222222222","start_timestamp":1790000000,"end_timestamp":1790000001,"is_segment":true,"status":"error","links":[{"trace_id":"33333333333333333333333333333333","span_id":"4444444444444444"}],"attributes":{"sentry.kind":{"type":"integer","value":2},"sentry.status_message":{"type":"string","value":"deadline_exceeded"},"sentry.sdk.integrations":{"type":"array","value":["SpanStreaming"]},"flag":{"type":"boolean","value":true},"sentry.release":{"type":"string","value":"synthetic-release"}}}]});
        let span = streamed(&header, &serde_json::to_vec(&payload).unwrap(), &auth())
            .unwrap()
            .remove(0);
        assert_eq!(span.attributes["flag"], "true");
        assert_eq!(span.kind, SpanKind::Server);
        assert_eq!(span.status_code, SpanStatusCode::Error);
        assert_eq!(span.status_message, "deadline_exceeded");
        assert!(span.attributes["temps.sentry.links"].contains("4444444444444444"));
        assert_eq!(
            span.resource.service_version.as_deref(),
            Some("synthetic-release")
        );
        assert!(span.attributes["temps.sentry.attributes"].contains("SpanStreaming"));
        payload["version"] = serde_json::json!(3);
        assert!(streamed(&header, &serde_json::to_vec(&payload).unwrap(), &auth()).is_err());
    }
}
