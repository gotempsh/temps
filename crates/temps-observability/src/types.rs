//! The wire shape for the unified Observe page.
//!
//! Every row carries its own `type` discriminator and all data the UI needs
//! to render both the row AND its detail panel without a follow-up fetch
//! (the "no extra request" guarantee in the design plan). Heavy fields
//! (stack frames, headers, span attributes, log messages) are truncated
//! server-side with a `truncated` flag so a list page stays under a few
//! hundred KB even when 100 fat error rows show up; the client only fetches
//! the un-truncated form when the user explicitly clicks "Show full".

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Maximum stack frames embedded in `ErrorRow.stacktrace_preview`.
pub const STACKTRACE_PREVIEW_FRAMES: usize = 5;
/// Maximum span attributes embedded in `SpanRow.attributes`.
pub const SPAN_ATTRIBUTE_PREVIEW_KEYS: usize = 20;
/// Header keys whitelisted into the `*_headers` previews. Anything else is
/// dropped from the preview payload but is still available via the
/// un-truncate endpoint.
pub const HEADER_WHITELIST: &[&str] = &[
    "host",
    "user-agent",
    "referer",
    "referrer",
    "content-type",
    "content-length",
    "accept",
    "accept-encoding",
    "x-forwarded-for",
    "cache-control",
];

/// Discriminated union of every row that can appear in the Observe list.
///
/// Serializes to `{ "type": "request" | "span" | ... , ...rest }` so the UI
/// can switch on `event.type` without ambiguity.
///
/// **No `Log` variant**: runtime stdout/stderr lines live on a dedicated
/// Logs page rather than Observe. Logs are too high-volume to interleave
/// with business signals (requests, errors, revenue) without dominating
/// the timeline, and they have their own retention/storage constraints
/// (TimescaleDB hypertable + chunked file/S3 store) that don't compose
/// with the merge service's per-kind LIMIT strategy.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ObservabilityEvent {
    Request(RequestRow),
    Span(SpanRow),
    Error(ErrorRow),
    Revenue(RevenueRow),
}

impl ObservabilityEvent {
    /// The sort key used by the merge service. All variants must expose
    /// `ts` so a k-way merge can interleave them.
    pub fn ts(&self) -> DateTime<Utc> {
        match self {
            ObservabilityEvent::Request(r) => r.ts,
            ObservabilityEvent::Span(r) => r.ts,
            ObservabilityEvent::Error(r) => r.ts,
            ObservabilityEvent::Revenue(r) => r.ts,
        }
    }

    /// Stable string discriminator — used by the cursor and by the
    /// `/full/{type}/{id}` endpoint route.
    pub fn kind(&self) -> EventKind {
        match self {
            ObservabilityEvent::Request(_) => EventKind::Request,
            ObservabilityEvent::Span(_) => EventKind::Span,
            ObservabilityEvent::Error(_) => EventKind::Error,
            ObservabilityEvent::Revenue(_) => EventKind::Revenue,
        }
    }
}

/// Tag enum for filter parameters and routing. Matches the variant
/// discriminator used by `ObservabilityEvent`.
#[derive(Copy, Clone, Debug, Serialize, Deserialize, ToSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Request,
    Span,
    Error,
    Revenue,
}

impl EventKind {
    pub const ALL: [EventKind; 4] = [
        EventKind::Request,
        EventKind::Span,
        EventKind::Error,
        EventKind::Revenue,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Request => "request",
            EventKind::Span => "span",
            EventKind::Error => "error",
            EventKind::Revenue => "revenue",
        }
    }

    pub fn parse(s: &str) -> Option<EventKind> {
        match s {
            "request" => Some(EventKind::Request),
            "span" => Some(EventKind::Span),
            "error" => Some(EventKind::Error),
            "revenue" => Some(EventKind::Revenue),
            _ => None,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Per-kind row types
// ──────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct RequestRow {
    /// The request's unique `request_id` (assigned by the proxy). Used as the
    /// row identity instead of the storage PK because the ClickHouse backend
    /// has no serial id (rows come back with `id = 0`) while `request_id` is
    /// unique and present on both backends.
    pub id: String,
    pub ts: DateTime<Utc>,
    pub deployment_id: Option<i32>,
    pub environment_id: Option<i32>,
    pub trace_id: Option<String>,
    pub error_group_id: Option<i32>,
    pub method: String,
    pub host: String,
    pub path: String,
    pub query_string: Option<String>,
    pub status: i16,
    pub latency_ms: Option<i32>,
    pub client_ip: Option<String>,
    pub country: Option<String>,
    pub user_agent: Option<String>,
    pub referrer: Option<String>,
    pub request_headers: serde_json::Value,
    pub response_headers: serde_json::Value,
    pub headers_truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct SpanRow {
    pub id: String,
    pub ts: DateTime<Utc>,
    pub deployment_id: Option<i32>,
    pub environment_id: Option<i32>,
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub service: String,
    pub operation: String,
    pub duration_ms: Option<f64>,
    pub status: Option<String>,
    pub attributes: serde_json::Value,
    pub attributes_truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ErrorRow {
    pub id: i64,
    pub ts: DateTime<Utc>,
    pub deployment_id: Option<i32>,
    pub environment_id: Option<i32>,
    pub trace_id: Option<String>,
    pub error_group_id: i32,
    pub fingerprint: String,
    pub error_class: String,
    pub message: Option<String>,
    pub stacktrace_preview: serde_json::Value,
    pub stacktrace_truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct RevenueRow {
    pub id: i64,
    pub ts: DateTime<Utc>,
    pub deployment_id: Option<i32>,
    pub environment_id: Option<i32>,
    pub trace_id: Option<String>,
    pub provider: String,
    pub event_type: String,
    pub customer_ref: Option<String>,
    pub amount_minor: Option<i64>,
    pub currency: Option<String>,
}

// ──────────────────────────────────────────────────────────────────────────
// Truncation helpers (tested in isolation)
// ──────────────────────────────────────────────────────────────────────────

/// Filter a JSON header object down to the whitelist. Accepts any JSON
/// shape the database produces and is forgiving when the value isn't an
/// object (returns an empty object + truncated=false in that case so the
/// UI doesn't crash on legacy rows).
pub fn project_headers(raw: Option<&serde_json::Value>) -> (serde_json::Value, bool) {
    let Some(v) = raw else {
        return (serde_json::Value::Object(serde_json::Map::new()), false);
    };
    let Some(map) = v.as_object() else {
        return (serde_json::Value::Object(serde_json::Map::new()), false);
    };
    let mut out = serde_json::Map::new();
    let mut dropped = false;
    for (k, v) in map.iter() {
        if HEADER_WHITELIST.iter().any(|w| w.eq_ignore_ascii_case(k)) {
            out.insert(k.clone(), v.clone());
        } else {
            dropped = true;
        }
    }
    (serde_json::Value::Object(out), dropped)
}

/// Keep at most `STACKTRACE_PREVIEW_FRAMES` frames from a JSON array of
/// stack frames, preserving order. Tolerates non-array input.
pub fn truncate_stacktrace(raw: Option<&serde_json::Value>) -> (serde_json::Value, bool) {
    let Some(v) = raw else {
        return (serde_json::Value::Array(Vec::new()), false);
    };
    let Some(arr) = v.as_array() else {
        return (serde_json::Value::Array(Vec::new()), false);
    };
    if arr.len() <= STACKTRACE_PREVIEW_FRAMES {
        return (serde_json::Value::Array(arr.clone()), false);
    }
    (
        serde_json::Value::Array(arr[..STACKTRACE_PREVIEW_FRAMES].to_vec()),
        true,
    )
}

/// Keep at most `SPAN_ATTRIBUTE_PREVIEW_KEYS` keys from a JSON object,
/// alphabetized so the preview is deterministic across repeated requests.
pub fn truncate_attributes(raw: Option<&serde_json::Value>) -> (serde_json::Value, bool) {
    let Some(v) = raw else {
        return (serde_json::Value::Object(serde_json::Map::new()), false);
    };
    let Some(obj) = v.as_object() else {
        return (serde_json::Value::Object(serde_json::Map::new()), false);
    };
    if obj.len() <= SPAN_ATTRIBUTE_PREVIEW_KEYS {
        return (serde_json::Value::Object(obj.clone()), false);
    }
    let mut keys: Vec<&String> = obj.keys().collect();
    keys.sort();
    let mut out = serde_json::Map::new();
    for k in keys.into_iter().take(SPAN_ATTRIBUTE_PREVIEW_KEYS) {
        out.insert(k.clone(), obj[k].clone());
    }
    (serde_json::Value::Object(out), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    // ── Discriminated union shape ────────────────────────────────────────

    #[test]
    fn request_event_serializes_with_type_tag() {
        let ev = ObservabilityEvent::Request(RequestRow {
            id: "req-7".into(),
            ts: ts(),
            deployment_id: None,
            environment_id: None,
            trace_id: Some("4bf92f3577b34da6a3ce929d0e0e4736".into()),
            error_group_id: None,
            method: "GET".into(),
            host: "x.test".into(),
            path: "/".into(),
            query_string: None,
            status: 200,
            latency_ms: Some(12),
            client_ip: None,
            country: None,
            user_agent: None,
            referrer: None,
            request_headers: serde_json::json!({}),
            response_headers: serde_json::json!({}),
            headers_truncated: false,
        });
        assert_eq!(serde_json::to_value(&ev).unwrap()["type"], "request");
    }

    #[test]
    fn round_trips_through_json() {
        let ev = ObservabilityEvent::Revenue(RevenueRow {
            id: 1,
            ts: ts(),
            deployment_id: None,
            environment_id: None,
            trace_id: None,
            provider: "stripe".into(),
            event_type: "invoice.paid".into(),
            customer_ref: Some("cus_x".into()),
            amount_minor: Some(4200),
            currency: Some("usd".into()),
        });
        let json = serde_json::to_string(&ev).unwrap();
        let back: ObservabilityEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(ev, back);
    }

    #[test]
    fn ts_accessor_returns_per_variant_timestamp() {
        let ev = ObservabilityEvent::Revenue(RevenueRow {
            id: 1,
            ts: ts(),
            deployment_id: None,
            environment_id: None,
            trace_id: None,
            provider: "stripe".into(),
            event_type: "invoice.paid".into(),
            customer_ref: None,
            amount_minor: None,
            currency: None,
        });
        assert_eq!(ev.ts(), ts());
        assert_eq!(ev.kind(), EventKind::Revenue);
    }

    // ── EventKind ────────────────────────────────────────────────────────

    #[test]
    fn event_kind_round_trips() {
        for k in EventKind::ALL {
            assert_eq!(EventKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(EventKind::parse("nope"), None);
    }

    // ── Truncation helpers ───────────────────────────────────────────────

    #[test]
    fn project_headers_keeps_only_whitelist() {
        let raw = serde_json::json!({
            "host": "x.test",
            "user-agent": "curl",
            "x-secret": "redacted",
        });
        let (out, dropped) = project_headers(Some(&raw));
        assert!(dropped);
        let obj = out.as_object().unwrap();
        assert!(obj.contains_key("host"));
        assert!(obj.contains_key("user-agent"));
        assert!(!obj.contains_key("x-secret"));
    }

    #[test]
    fn project_headers_returns_empty_for_non_object() {
        let raw = serde_json::json!("not an object");
        let (out, dropped) = project_headers(Some(&raw));
        assert_eq!(out, serde_json::json!({}));
        assert!(!dropped);
    }

    #[test]
    fn project_headers_handles_none() {
        let (out, dropped) = project_headers(None);
        assert_eq!(out, serde_json::json!({}));
        assert!(!dropped);
    }

    #[test]
    fn project_headers_no_drop_when_all_whitelisted() {
        let raw = serde_json::json!({"host": "x", "accept": "*/*"});
        let (_, dropped) = project_headers(Some(&raw));
        assert!(!dropped);
    }

    #[test]
    fn truncate_stacktrace_caps_at_preview_size() {
        let frames: Vec<serde_json::Value> = (0..20)
            .map(|i| serde_json::json!({"function": format!("fn_{}", i)}))
            .collect();
        let raw = serde_json::Value::Array(frames);
        let (out, truncated) = truncate_stacktrace(Some(&raw));
        assert!(truncated);
        assert_eq!(out.as_array().unwrap().len(), STACKTRACE_PREVIEW_FRAMES);
        // Order preserved
        assert_eq!(out.as_array().unwrap()[0]["function"], "fn_0");
    }

    #[test]
    fn truncate_stacktrace_no_op_when_small() {
        let raw = serde_json::json!([{"function": "main"}]);
        let (_, truncated) = truncate_stacktrace(Some(&raw));
        assert!(!truncated);
    }

    #[test]
    fn truncate_attributes_caps_and_alphabetizes() {
        let mut obj = serde_json::Map::new();
        for i in 0..30 {
            obj.insert(format!("attr_{:02}", i), serde_json::json!(i));
        }
        let raw = serde_json::Value::Object(obj);
        let (out, truncated) = truncate_attributes(Some(&raw));
        assert!(truncated);
        let obj = out.as_object().unwrap();
        assert_eq!(obj.len(), SPAN_ATTRIBUTE_PREVIEW_KEYS);
        // Alphabetized — first key should be attr_00, last should be attr_19
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort();
        assert_eq!(keys.first().unwrap().as_str(), "attr_00");
    }

    #[test]
    fn truncate_attributes_no_op_when_small() {
        let raw = serde_json::json!({"k": "v"});
        let (_, truncated) = truncate_attributes(Some(&raw));
        assert!(!truncated);
    }
}
