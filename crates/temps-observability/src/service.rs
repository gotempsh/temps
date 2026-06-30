//! Merge service: parallel-fetches per-kind row streams from the source
//! tables, maps each to the unified `ObservabilityEvent` wire type
//! (truncating heavy fields server-side), and k-way merges them by
//! `ts DESC` into a single page.
//!
//! Per-kind queries are kept independent so a future kind addition only
//! requires a new fetcher; the merge logic is in `filters::merge_desc_by_ts`.
//!
//! NOTE on `Log` and `Span` kinds: log rows live in `temps-log-aggregator`'s
//! file/S3 chunk store, and OTel spans are in a raw TimescaleDB hypertable
//! without a Sea-ORM model. Both currently return empty streams from this
//! service. The wire contract is complete and the UI renders them without
//! changes once the fetchers are wired (separate follow-up).

use std::sync::Arc;

use sea_orm::{
    ColumnTrait, DatabaseBackend, DatabaseConnection, EntityTrait, FromQueryResult, QueryFilter,
    QueryOrder, QuerySelect, Statement,
};
use temps_entities::{error_events, proxy_logs, revenue_events};

use crate::error::ObservabilityError;
use crate::filters::{merge_desc_by_ts, EventFilters};
use crate::types::{
    project_headers, truncate_attributes, truncate_stacktrace, ErrorRow, EventKind,
    ObservabilityEvent, RequestRow, RevenueRow, SpanRow,
};

/// One un-truncated row, returned by the `/full/{type}/{id}` endpoint when
/// the user clicks "Show full". Same shape as the list rows, but with the
/// raw heavy fields restored (no truncation flags) so the side panel can
/// render the long form.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FullEvent {
    Request(FullRequest),
    Error(FullError),
    Revenue(crate::types::RevenueRow),
    /// `SpanRow.attributes` is the truncated form; re-fetching returns
    /// the same shape so the panel has a stable contract.
    Span(crate::types::SpanRow),
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct FullRequest {
    pub id: i64,
    pub ts: chrono::DateTime<chrono::Utc>,
    pub deployment_id: Option<i32>,
    pub environment_id: Option<i32>,
    pub trace_id: Option<String>,
    pub error_group_id: Option<i32>,
    pub method: String,
    pub host: String,
    pub path: String,
    pub status: i16,
    pub latency_ms: Option<i32>,
    pub client_ip: Option<String>,
    pub user_agent: Option<String>,
    pub referrer: Option<String>,
    pub request_headers: Option<serde_json::Value>,
    pub response_headers: Option<serde_json::Value>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct FullError {
    pub id: i64,
    pub ts: chrono::DateTime<chrono::Utc>,
    pub deployment_id: Option<i32>,
    pub environment_id: Option<i32>,
    pub trace_id: Option<String>,
    pub error_group_id: i32,
    pub fingerprint: String,
    pub error_class: String,
    pub message: Option<String>,
    /// Full JSONB blob from `error_events.data` — stack trace, breadcrumbs,
    /// request context, everything. Schema is documented per source SDK.
    pub data: Option<serde_json::Value>,
}

pub struct ObservabilityService {
    db: Arc<DatabaseConnection>,
}

impl ObservabilityService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Fetch one merged page of events.
    ///
    /// Strategy:
    ///   1. Validate the filter struct.
    ///   2. For each enabled kind in `filters.kinds`, run an independent
    ///      Sea-ORM query (LIMIT = page size) returning rows already mapped
    ///      to the `ObservabilityEvent` wire type — no follow-up fetches.
    ///   3. k-way merge the streams by ts DESC and trim to `filters.limit`.
    pub async fn query(
        &self,
        filters: EventFilters,
    ) -> Result<Vec<ObservabilityEvent>, ObservabilityError> {
        filters.validate()?;

        let mut streams: Vec<Vec<ObservabilityEvent>> = Vec::new();

        if filters.kinds.contains(&EventKind::Request) {
            streams.push(self.fetch_requests(&filters).await?);
        }
        if filters.kinds.contains(&EventKind::Error) {
            streams.push(self.fetch_errors(&filters).await?);
        }
        if filters.kinds.contains(&EventKind::Revenue) {
            streams.push(self.fetch_revenue(&filters).await?);
        }
        if filters.kinds.contains(&EventKind::Span) {
            streams.push(self.fetch_spans(&filters).await?);
        }

        Ok(merge_desc_by_ts(streams, filters.limit as usize))
    }

    /// Fetch the un-truncated form of a single event by `(kind, id)`.
    /// Used by the side panel "Show full" action.
    pub async fn fetch_full(
        &self,
        project_id: i32,
        kind: EventKind,
        id: &str,
    ) -> Result<FullEvent, ObservabilityError> {
        match kind {
            EventKind::Request => {
                let pk: i32 = id.parse().map_err(|_| ObservabilityError::EventNotFound {
                    project_id,
                    kind: "request".into(),
                    event_id: id.into(),
                })?;
                let row = proxy_logs::Entity::find_by_id(pk)
                    .filter(proxy_logs::Column::ProjectId.eq(project_id))
                    .one(self.db.as_ref())
                    .await?
                    .ok_or_else(|| ObservabilityError::EventNotFound {
                        project_id,
                        kind: "request".into(),
                        event_id: id.into(),
                    })?;
                Ok(FullEvent::Request(FullRequest {
                    id: row.id as i64,
                    ts: row.timestamp,
                    deployment_id: row.deployment_id,
                    environment_id: row.environment_id,
                    trace_id: row.trace_id,
                    error_group_id: row.error_group_id,
                    method: row.method,
                    host: row.host,
                    path: row.path,
                    status: row.status_code,
                    latency_ms: row.response_time_ms,
                    client_ip: row.client_ip,
                    user_agent: row.user_agent,
                    referrer: row.referrer,
                    request_headers: row.request_headers,
                    response_headers: row.response_headers,
                }))
            }
            EventKind::Error => {
                let pk: i64 = id.parse().map_err(|_| ObservabilityError::EventNotFound {
                    project_id,
                    kind: "error".into(),
                    event_id: id.into(),
                })?;
                let row = error_events::Entity::find_by_id(pk)
                    .filter(error_events::Column::ProjectId.eq(project_id))
                    .one(self.db.as_ref())
                    .await?
                    .ok_or_else(|| ObservabilityError::EventNotFound {
                        project_id,
                        kind: "error".into(),
                        event_id: id.into(),
                    })?;
                Ok(FullEvent::Error(FullError {
                    id: row.id,
                    ts: row.timestamp,
                    deployment_id: row.deployment_id,
                    environment_id: row.environment_id,
                    trace_id: row.trace_id_indexed,
                    error_group_id: row.error_group_id,
                    fingerprint: row.fingerprint_hash,
                    error_class: row.exception_type,
                    message: row.exception_value,
                    data: row.data,
                }))
            }
            EventKind::Revenue => {
                let pk: i64 = id.parse().map_err(|_| ObservabilityError::EventNotFound {
                    project_id,
                    kind: "revenue".into(),
                    event_id: id.into(),
                })?;
                let row = revenue_events::Entity::find_by_id(pk)
                    .filter(revenue_events::Column::ProjectId.eq(project_id))
                    .one(self.db.as_ref())
                    .await?
                    .ok_or_else(|| ObservabilityError::EventNotFound {
                        project_id,
                        kind: "revenue".into(),
                        event_id: id.into(),
                    })?;
                let ObservabilityEvent::Revenue(rev) = map_revenue(row) else {
                    unreachable!("map_revenue returns Revenue variant");
                };
                Ok(FullEvent::Revenue(rev))
            }
            EventKind::Span => {
                let row = self.find_span_by_id(project_id, id).await?.ok_or_else(|| {
                    ObservabilityError::EventNotFound {
                        project_id,
                        kind: "span".into(),
                        event_id: id.into(),
                    }
                })?;
                let ObservabilityEvent::Span(full) = map_span(row) else {
                    unreachable!("map_span returns Span variant");
                };
                Ok(FullEvent::Span(full))
            }
        }
    }

    async fn fetch_requests(
        &self,
        filters: &EventFilters,
    ) -> Result<Vec<ObservabilityEvent>, ObservabilityError> {
        let mut q =
            proxy_logs::Entity::find().filter(proxy_logs::Column::ProjectId.eq(filters.project_id));
        if let Some(from) = filters.from {
            q = q.filter(proxy_logs::Column::Timestamp.gte(from));
        }
        if let Some(to) = filters.to {
            q = q.filter(proxy_logs::Column::Timestamp.lte(to));
        }
        if let Some(d) = filters.deployment_id {
            q = q.filter(proxy_logs::Column::DeploymentId.eq(d));
        }
        if let Some(e) = filters.environment_id {
            q = q.filter(proxy_logs::Column::EnvironmentId.eq(e));
        }
        if let Some(ref s) = filters.search {
            q = q.filter(proxy_logs::Column::Path.contains(s));
        }
        if let Some(hide) = filters.hide_bots {
            if hide {
                // Treat NULL `is_bot` as not-a-bot so we don't hide rows
                // that simply lack detection metadata (older rows).
                q = q.filter(
                    proxy_logs::Column::IsBot
                        .eq(false)
                        .or(proxy_logs::Column::IsBot.is_null()),
                );
            } else {
                q = q.filter(proxy_logs::Column::IsBot.eq(true));
            }
        }
        let rows = q
            .order_by_desc(proxy_logs::Column::Timestamp)
            .limit(filters.limit)
            .all(self.db.as_ref())
            .await?;

        Ok(rows.into_iter().map(map_request).collect())
    }

    async fn fetch_errors(
        &self,
        filters: &EventFilters,
    ) -> Result<Vec<ObservabilityEvent>, ObservabilityError> {
        let mut q = error_events::Entity::find()
            .filter(error_events::Column::ProjectId.eq(filters.project_id));
        if let Some(from) = filters.from {
            q = q.filter(error_events::Column::Timestamp.gte(from));
        }
        if let Some(to) = filters.to {
            q = q.filter(error_events::Column::Timestamp.lte(to));
        }
        if let Some(d) = filters.deployment_id {
            q = q.filter(error_events::Column::DeploymentId.eq(d));
        }
        if let Some(e) = filters.environment_id {
            q = q.filter(error_events::Column::EnvironmentId.eq(e));
        }
        if let Some(ref s) = filters.search {
            q = q.filter(error_events::Column::ExceptionType.contains(s));
        }
        let rows = q
            .order_by_desc(error_events::Column::Timestamp)
            .limit(filters.limit)
            .all(self.db.as_ref())
            .await?;

        Ok(rows.into_iter().map(map_error).collect())
    }

    async fn fetch_revenue(
        &self,
        filters: &EventFilters,
    ) -> Result<Vec<ObservabilityEvent>, ObservabilityError> {
        let mut q = revenue_events::Entity::find()
            .filter(revenue_events::Column::ProjectId.eq(filters.project_id));
        if let Some(from) = filters.from {
            q = q.filter(revenue_events::Column::OccurredAt.gte(from));
        }
        if let Some(to) = filters.to {
            q = q.filter(revenue_events::Column::OccurredAt.lte(to));
        }
        if let Some(d) = filters.deployment_id {
            q = q.filter(revenue_events::Column::DeploymentId.eq(d));
        }
        if let Some(e) = filters.environment_id {
            q = q.filter(revenue_events::Column::EnvironmentId.eq(e));
        }
        if let Some(ref s) = filters.search {
            q = q.filter(revenue_events::Column::EventType.contains(s));
        }
        let rows = q
            .order_by_desc(revenue_events::Column::OccurredAt)
            .limit(filters.limit)
            .all(self.db.as_ref())
            .await?;

        Ok(rows.into_iter().map(map_revenue).collect())
    }

    /// Fetch spans from the raw `otel_spans` TimescaleDB hypertable. No
    /// Sea-ORM entity exists for it; we hydrate rows via `FromQueryResult`
    /// instead. Spans are even higher-volume than logs — gated client-side.
    ///
    /// By default only ROOT spans are returned (`parent_span_id IS NULL`) — one
    /// row per trace (the "parent" of the trace), not every child span. The
    /// unified feed is a high-level activity stream; surfacing every internal
    /// span (`executing api route`, `resolve page components`, …) would bury the
    /// signal. (Decode stores a root span's parent as NULL — see otel decode; the
    /// storage layer likewise identifies roots via `parent_span_id IS NULL`.)
    ///
    /// EXCEPTION: when the user is searching by name, the root restriction is
    /// dropped so the search matches ANY span — including child spans like
    /// `SELECT guestbook` or `fetch POST …`, which is precisely what someone
    /// grepping span names is looking for. The root-only path is backed by the
    /// partial index `idx_otel_spans_root_project_start`; the search path is
    /// bounded by the time window (chunk pruning) + `name ILIKE`.
    async fn fetch_spans(
        &self,
        filters: &EventFilters,
    ) -> Result<Vec<ObservabilityEvent>, ObservabilityError> {
        let mut sql = String::from(
            "SELECT id, project_id, deployment_id, service_name, trace_id, span_id, \
             parent_span_id, name, status_code, start_time, duration_ms, attributes \
             FROM otel_spans WHERE project_id = $1",
        );
        let mut values: Vec<sea_orm::Value> = vec![filters.project_id.into()];
        let mut next_param = 2usize;

        // Roots only by default (one row per trace); a name search widens to all
        // spans so child-span names are discoverable.
        if filters.search.is_none() {
            sql.push_str(" AND parent_span_id IS NULL");
        }

        if let Some(from) = filters.from {
            sql.push_str(&format!(" AND start_time >= ${}", next_param));
            values.push(from.into());
            next_param += 1;
        }
        if let Some(to) = filters.to {
            sql.push_str(&format!(" AND start_time <= ${}", next_param));
            values.push(to.into());
            next_param += 1;
        }
        if let Some(d) = filters.deployment_id {
            sql.push_str(&format!(" AND deployment_id = ${}", next_param));
            values.push(d.into());
            next_param += 1;
        }
        if let Some(ref s) = filters.search {
            sql.push_str(&format!(" AND name ILIKE ${}", next_param));
            values.push(format!("%{}%", s).into());
            next_param += 1;
        }
        sql.push_str(&format!(" ORDER BY start_time DESC LIMIT ${}", next_param));
        values.push(sea_orm::Value::BigInt(Some(filters.limit as i64)));

        let stmt = Statement::from_sql_and_values(DatabaseBackend::Postgres, &sql, values);
        let rows = SpanRecord::find_by_statement(stmt)
            .all(self.db.as_ref())
            .await?;

        Ok(rows.into_iter().map(map_span).collect())
    }

    async fn find_span_by_id(
        &self,
        project_id: i32,
        id: &str,
    ) -> Result<Option<SpanRecord>, ObservabilityError> {
        let pk: i64 = id.parse().map_err(|_| ObservabilityError::EventNotFound {
            project_id,
            kind: "span".into(),
            event_id: id.into(),
        })?;
        let stmt = Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id, project_id, deployment_id, service_name, trace_id, span_id, \
             parent_span_id, name, status_code, start_time, duration_ms, attributes \
             FROM otel_spans WHERE project_id = $1 AND id = $2 LIMIT 1",
            vec![project_id.into(), pk.into()],
        );
        let row = SpanRecord::find_by_statement(stmt)
            .one(self.db.as_ref())
            .await?;
        Ok(row)
    }
}

/// Raw row from `otel_spans` — no Sea-ORM entity exists for the
/// hypertable, so we hydrate via `FromQueryResult` directly.
#[derive(Debug, FromQueryResult)]
struct SpanRecord {
    id: i64,
    #[allow(dead_code)]
    project_id: i32,
    deployment_id: Option<i32>,
    service_name: String,
    trace_id: String,
    span_id: String,
    parent_span_id: Option<String>,
    name: String,
    status_code: String,
    start_time: chrono::DateTime<chrono::Utc>,
    duration_ms: f64,
    attributes: serde_json::Value,
}

// ──────────────────────────────────────────────────────────────────────────
// Per-kind row mappers (free functions so we can unit-test without DB)
// ──────────────────────────────────────────────────────────────────────────

pub fn map_request(m: proxy_logs::Model) -> ObservabilityEvent {
    let (req_headers, req_dropped) = project_headers(m.request_headers.as_ref());
    let (resp_headers, resp_dropped) = project_headers(m.response_headers.as_ref());
    ObservabilityEvent::Request(RequestRow {
        id: m.id as i64,
        ts: m.timestamp,
        deployment_id: m.deployment_id,
        environment_id: m.environment_id,
        trace_id: m.trace_id,
        error_group_id: m.error_group_id,
        method: m.method,
        host: m.host,
        path: m.path,
        query_string: m.query_string,
        status: m.status_code,
        latency_ms: m.response_time_ms,
        client_ip: m.client_ip,
        country: None,
        user_agent: m.user_agent,
        referrer: m.referrer,
        request_headers: req_headers,
        response_headers: resp_headers,
        headers_truncated: req_dropped || resp_dropped,
    })
}

pub fn map_error(m: error_events::Model) -> ObservabilityEvent {
    // Pull stack frames out of the JSONB blob if they're there. Both the
    // Sentry-wrapped and internal layouts use a `stack_trace` array; we
    // probe both points.
    let stack_raw = m
        .data
        .as_ref()
        .and_then(|d| d.pointer("/stack_trace"))
        .or_else(|| {
            m.data
                .as_ref()
                .and_then(|d| d.pointer("/sentry/exception/values/0/stacktrace/frames"))
        })
        .cloned();
    let (stacktrace_preview, stacktrace_truncated) = truncate_stacktrace(stack_raw.as_ref());

    // Resolve the human-readable message. The top-level `exception_value`
    // column is frequently NULL even when the original payload has plenty
    // of context — Sentry SDKs that emit `captureMessage()` store the
    // text under `data.sentry.message` or `data.message`, and some SDKs
    // bury it in the first breadcrumb. Probe in priority order so the
    // Observe row always shows real text instead of bare "Error:".
    let resolved_message = m
        .exception_value
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| extract_error_message(m.data.as_ref()));

    ObservabilityEvent::Error(ErrorRow {
        id: m.id,
        ts: m.timestamp,
        deployment_id: m.deployment_id,
        environment_id: m.environment_id,
        trace_id: m.trace_id_indexed,
        error_group_id: m.error_group_id,
        fingerprint: m.fingerprint_hash,
        error_class: m.exception_type,
        message: resolved_message,
        stacktrace_preview,
        stacktrace_truncated,
    })
}

/// Probe the JSONB blob for a human-readable error message. Looks at the
/// most common SDK-emitted shapes; returns the first non-empty hit.
///
/// Sentry SDKs put the text in different places depending on whether the
/// event was emitted via `captureException` (→ exception.values[].value)
/// or `captureMessage` (→ logentry.formatted, sometimes message). We
/// probe both wrapped (`/sentry/...`) and top-level layouts because the
/// ingestion pipeline switches between them based on whether the raw
/// event was preserved.
fn extract_error_message(data: Option<&serde_json::Value>) -> Option<String> {
    let data = data?;
    let candidates = [
        // Sentry SDK formatted message (captureMessage path) — wrapped
        "/sentry/logentry/formatted",
        "/sentry/logentry/message",
        "/sentry/message",
        "/sentry/exception/values/0/value",
        // Top-level Sentry payload (no wrapper)
        "/logentry/formatted",
        "/logentry/message",
        "/message",
        "/exception/values/0/value",
        // Internal / generic fallbacks
        "/log_message",
    ];
    for path in candidates {
        if let Some(s) = data.pointer(path).and_then(|v| v.as_str()) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

pub fn map_revenue(m: revenue_events::Model) -> ObservabilityEvent {
    ObservabilityEvent::Revenue(RevenueRow {
        id: m.id,
        ts: m.occurred_at,
        deployment_id: m.deployment_id,
        environment_id: m.environment_id,
        trace_id: m.trace_id,
        provider: m.provider,
        event_type: m.event_type,
        customer_ref: m.customer_ref,
        amount_minor: m.amount_minor,
        currency: m.currency,
    })
}

fn map_span(s: SpanRecord) -> ObservabilityEvent {
    let (attributes, attributes_truncated) = truncate_attributes(Some(&s.attributes));
    ObservabilityEvent::Span(SpanRow {
        id: s.id.to_string(),
        ts: s.start_time,
        deployment_id: s.deployment_id,
        environment_id: None,
        trace_id: s.trace_id,
        span_id: s.span_id,
        parent_span_id: s.parent_span_id,
        service: s.service_name,
        operation: s.name,
        duration_ms: Some(s.duration_ms),
        status: Some(s.status_code),
        attributes,
        attributes_truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    fn proxy_log_model() -> proxy_logs::Model {
        proxy_logs::Model {
            id: 7,
            timestamp: Utc::now(),
            method: "GET".into(),
            path: "/api".into(),
            query_string: None,
            host: "x.test".into(),
            status_code: 200,
            response_time_ms: Some(12),
            request_source: "proxy".into(),
            is_system_request: false,
            routing_status: "routed".into(),
            project_id: Some(1),
            environment_id: Some(2),
            deployment_id: Some(3),
            container_id: None,
            upstream_host: None,
            error_message: None,
            client_ip: Some("1.2.3.4".into()),
            user_agent: Some("curl".into()),
            referrer: None,
            request_id: "req-1".into(),
            ip_geolocation_id: None,
            browser: None,
            browser_version: None,
            operating_system: None,
            device_type: None,
            is_bot: None,
            bot_name: None,
            request_size_bytes: None,
            response_size_bytes: None,
            cache_status: None,
            request_headers: Some(json!({"host": "x.test", "x-secret": "shh"})),
            response_headers: Some(json!({"content-type": "text/html"})),
            created_date: Utc::now().date_naive(),
            session_id: None,
            visitor_id: None,
            trace_id: Some("4bf92f3577b34da6a3ce929d0e0e4736".into()),
            error_group_id: Some(99),
        }
    }

    #[test]
    fn map_request_keeps_essentials_and_truncates_headers() {
        let ev = map_request(proxy_log_model());
        let ObservabilityEvent::Request(r) = ev else {
            panic!("expected request");
        };
        assert_eq!(r.id, 7);
        assert_eq!(r.method, "GET");
        assert_eq!(
            r.trace_id.as_deref(),
            Some("4bf92f3577b34da6a3ce929d0e0e4736")
        );
        assert_eq!(r.error_group_id, Some(99));
        assert!(r.headers_truncated, "x-secret should have been dropped");
        assert!(r.request_headers.as_object().unwrap().contains_key("host"));
        assert!(!r
            .request_headers
            .as_object()
            .unwrap()
            .contains_key("x-secret"));
    }

    #[test]
    fn map_error_promotes_trace_id_and_truncates_stack() {
        let frames: Vec<serde_json::Value> = (0..20)
            .map(|i| json!({"function": format!("fn_{}", i)}))
            .collect();
        let model = error_events::Model {
            id: 42,
            error_group_id: 1,
            project_id: 7,
            environment_id: None,
            deployment_id: None,
            visitor_id: None,
            ip_geolocation_id: None,
            fingerprint_hash: "abc".into(),
            timestamp: Utc::now(),
            exception_type: "TypeError".into(),
            exception_value: Some("boom".into()),
            source: Some("custom".into()),
            data: Some(json!({"stack_trace": frames})),
            trace_id_indexed: Some("4bf92f3577b34da6a3ce929d0e0e4736".into()),
            created_at: Utc::now(),
        };
        let ev = map_error(model);
        let ObservabilityEvent::Error(e) = ev else {
            panic!("expected error");
        };
        assert_eq!(e.error_class, "TypeError");
        assert_eq!(
            e.trace_id.as_deref(),
            Some("4bf92f3577b34da6a3ce929d0e0e4736")
        );
        assert!(e.stacktrace_truncated);
        assert_eq!(e.stacktrace_preview.as_array().unwrap().len(), 5);
    }

    #[test]
    fn map_error_handles_missing_stack_trace() {
        let model = error_events::Model {
            id: 1,
            error_group_id: 1,
            project_id: 1,
            environment_id: None,
            deployment_id: None,
            visitor_id: None,
            ip_geolocation_id: None,
            fingerprint_hash: "x".into(),
            timestamp: Utc::now(),
            exception_type: "Error".into(),
            exception_value: None,
            source: None,
            data: None,
            trace_id_indexed: None,
            created_at: Utc::now(),
        };
        let ev = map_error(model);
        let ObservabilityEvent::Error(e) = ev else {
            panic!("expected error");
        };
        assert!(!e.stacktrace_truncated);
        assert_eq!(e.stacktrace_preview, json!([]));
    }

    #[test]
    fn map_revenue_carries_amount_and_currency() {
        let model = revenue_events::Model {
            id: 5,
            project_id: 1,
            integration_id: 1,
            provider: "stripe".into(),
            provider_event_id: "evt_1".into(),
            event_type: "invoice.paid".into(),
            customer_ref: Some("cus_x".into()),
            subscription_ref: None,
            subscription_status: None,
            mrr_minor: None,
            amount_minor: Some(4200),
            currency: Some("usd".into()),
            occurred_at: Utc::now(),
            payload: json!({}),
            created_at: Utc::now(),
            price_id: None,
            product_id: None,
            deployment_id: Some(3),
            environment_id: Some(2),
            trace_id: None,
        };
        let ev = map_revenue(model);
        let ObservabilityEvent::Revenue(r) = ev else {
            panic!("expected revenue");
        };
        assert_eq!(r.amount_minor, Some(4200));
        assert_eq!(r.currency.as_deref(), Some("usd"));
        assert_eq!(r.deployment_id, Some(3));
    }
}
