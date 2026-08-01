//! Merge service: parallel-fetches per-kind row streams from the source
//! stores, maps each to the unified `ObservabilityEvent` wire type
//! (truncating heavy fields server-side), and k-way merges them by
//! `ts DESC` into a single page.
//!
//! Per-kind queries are kept independent so a future kind addition only
//! requires a new fetcher; the merge logic is in `filters::merge_desc_by_ts`.
//!
//! ## Storage backends
//!
//! Requests and spans are read through the SAME pluggable storage backends
//! the Requests and Traces pages use — `ProxyLogService` (dispatching to
//! [`temps_proxy::storage::ProxyLogStorage`]) and
//! [`temps_otel::storage::OtelStorage`]. When the operator configures
//! `TEMPS_CLICKHOUSE_*`, those backends read ClickHouse; otherwise
//! TimescaleDB. Reading the Postgres tables directly here (the original
//! implementation) silently returned nothing on ClickHouse-enabled servers
//! because proxy logs / OTel spans are then written to ClickHouse only.
//! Errors and revenue events always live in Postgres, so those fetchers keep
//! their direct Sea-ORM queries.
//!
//! NOTE on the `Log` kind: log rows live in `temps-log-aggregator`'s file/S3
//! chunk store and currently return an empty stream from this service. The
//! wire contract is complete and the UI renders them without changes once
//! the fetcher is wired (separate follow-up).

use std::sync::Arc;

use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use temps_entities::{error_events, proxy_logs, revenue_events};
use temps_otel::storage::OtelStorage;
use temps_otel::types::{SpanRecord, TraceQuery};
use temps_proxy::handler::proxy_logs::ProxyLogsQuery;
use temps_proxy::service::proxy_log_service::ProxyLogService;

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
    /// The request's unique `request_id` — same identity the list rows carry
    /// (backend-agnostic; ClickHouse rows have no serial PK).
    pub id: String,
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
    /// Direct Postgres access for the kinds that always live there
    /// (error_events, revenue_events).
    db: Arc<DatabaseConnection>,
    /// Backend-dispatching request-log reader (TimescaleDB or ClickHouse,
    /// selected at startup from `TEMPS_CLICKHOUSE_*`).
    proxy_logs: Arc<ProxyLogService>,
    /// Backend-dispatching OTel span reader (same selection).
    otel: Arc<dyn OtelStorage>,
}

impl ObservabilityService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        proxy_logs: Arc<ProxyLogService>,
        otel: Arc<dyn OtelStorage>,
    ) -> Self {
        Self {
            db,
            proxy_logs,
            otel,
        }
    }

    /// Fetch one merged page of events.
    ///
    /// Strategy:
    ///   1. Validate the filter struct.
    ///   2. For each enabled kind in `filters.kinds`, run an independent
    ///      query (LIMIT = page size) returning rows already mapped to the
    ///      `ObservabilityEvent` wire type — no follow-up fetches.
    ///   3. k-way merge the streams by ts DESC and trim to `filters.limit`.
    ///
    /// Step 2 runs the enabled fetchers CONCURRENTLY. They are independent —
    /// different stores entirely (proxy logs and spans go to ClickHouse when
    /// it is configured; errors and revenue to Postgres) — so awaiting them
    /// one after another made the endpoint cost the SUM of four round trips
    /// instead of the slowest one. With spans being by far the slowest kind
    /// at volume, that serialisation was a large share of the observed
    /// multi-second latency on busy projects.
    ///
    /// `try_join!` short-circuits on the first error, matching the previous
    /// `?`-per-fetcher behaviour: one failing store still fails the request
    /// rather than silently returning a partial feed.
    pub async fn query(
        &self,
        filters: EventFilters,
    ) -> Result<Vec<ObservabilityEvent>, ObservabilityError> {
        filters.validate()?;

        // Bound the window BEFORE any fetcher runs, so every kind inherits it.
        // `from` is an optional query parameter, and the span and request
        // stores take the range verbatim — `query_spans` and `list_page` would
        // otherwise scan their whole table for a caller that simply omitted a
        // date. The web UI always sends a range; the API must not depend on
        // that. Rules and measurements live in temps_core::time_window.
        //
        // The SCOPED cap applies unconditionally: `EventFilters::project_id`
        // is a required `i32`, not `Option` — every call into this feed is
        // already narrowed to one project's rows, so it gets the same 30-day
        // ceiling `list_page`/the AI-breakdown endpoints grant a project-scoped
        // caller (see `ProxyLogService::resolve_window`), matching the 30-day
        // preset the Observe picker (`ObserveFilterBar.tsx`) already ships.
        let window = temps_core::time_window::resolve_with_max(
            filters.from,
            filters.to,
            chrono::Duration::hours(temps_core::time_window::DEFAULT_LOOKBACK_HOURS),
            chrono::Duration::days(temps_core::time_window::MAX_WINDOW_DAYS_SCOPED),
        )
        .map_err(|e| ObservabilityError::InvalidTimeWindow(e.to_string()))?;
        let filters = EventFilters {
            from: Some(window.start),
            to: window.end,
            ..filters
        };

        // A disabled kind resolves to an empty stream without touching its
        // store, so the join arms stay uniform in type.
        let requests = async {
            if filters.kinds.contains(&EventKind::Request) {
                self.fetch_requests(&filters).await
            } else {
                Ok(Vec::new())
            }
        };
        let errors = async {
            if filters.kinds.contains(&EventKind::Error) {
                self.fetch_errors(&filters).await
            } else {
                Ok(Vec::new())
            }
        };
        let revenue = async {
            if filters.kinds.contains(&EventKind::Revenue) {
                self.fetch_revenue(&filters).await
            } else {
                Ok(Vec::new())
            }
        };
        let spans = async {
            if filters.kinds.contains(&EventKind::Span) {
                self.fetch_spans(&filters).await
            } else {
                Ok(Vec::new())
            }
        };

        let (requests, errors, revenue, spans) =
            tokio::try_join!(requests, errors, revenue, spans)?;

        let streams: Vec<Vec<ObservabilityEvent>> = vec![requests, errors, revenue, spans];

        Ok(merge_desc_by_ts(streams, filters.limit as usize))
    }

    /// Fetch the un-truncated form of a single event by `(kind, id)`.
    /// Used by the side panel "Show full" action.
    ///
    /// `ts` is the row's known event time (the list response carries it per
    /// row). For requests it bounds the `request_id` lookup — chunk exclusion
    /// on the TimescaleDB hypertable, partition pruning on ClickHouse —
    /// instead of scanning the whole retention window.
    pub async fn fetch_full(
        &self,
        project_id: i32,
        kind: EventKind,
        id: &str,
        ts: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<FullEvent, ObservabilityError> {
        match kind {
            EventKind::Request => {
                // Rows are identified by `request_id` (backend-agnostic; the
                // ClickHouse table has no serial PK). The storage lookup is
                // not project-scoped, so enforce the tenancy boundary here.
                let row = self
                    .proxy_logs
                    .get_by_request_id(id, ts)
                    .await
                    .map_err(|source| ObservabilityError::RequestStore { project_id, source })?
                    .filter(|m| m.project_id == Some(project_id))
                    .ok_or_else(|| ObservabilityError::EventNotFound {
                        project_id,
                        kind: "request".into(),
                        event_id: id.into(),
                    })?;
                Ok(FullEvent::Request(FullRequest {
                    id: row.request_id,
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
        let query = ProxyLogsQuery {
            project_id: Some(filters.project_id),
            environment_id: filters.environment_id,
            deployment_id: filters.deployment_id,
            path: filters.search.clone(),
            // hide_bots is tri-state: Some(true) excludes bots but keeps
            // NULL-is_bot rows (older rows without detection metadata);
            // Some(false) shows ONLY bot rows; None applies no bot filter.
            is_bot: match filters.hide_bots {
                Some(false) => Some(true),
                _ => None,
            },
            exclude_bots: match filters.hide_bots {
                Some(true) => Some(true),
                _ => None,
            },
            ..Default::default()
        };
        // list_page (not list_with_filters): the feed renders a page and no
        // total, so the pagination COUNT — exact on the hypertable, FINAL
        // merge-on-read on ClickHouse — would be wasted work on every poll.
        let rows = self
            .proxy_logs
            .list_page(filters.from, filters.to, query, filters.limit)
            .await
            .map_err(|source| ObservabilityError::RequestStore {
                project_id: filters.project_id,
                source,
            })?;

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

    /// Fetch spans through the pluggable OTel storage backend. Spans are even
    /// higher-volume than logs — gated client-side.
    ///
    /// By default only ROOT spans are returned (`TraceQuery::root_only`) — one
    /// row per trace (the "parent" of the trace), not every child span. The
    /// unified feed is a high-level activity stream; surfacing every internal
    /// span (`executing api route`, `resolve page components`, …) would bury the
    /// signal.
    ///
    /// EXCEPTION: when the user is searching by name, the root restriction is
    /// dropped so the search matches ANY span — including child spans like
    /// `SELECT guestbook` or `fetch POST …`, which is precisely what someone
    /// grepping span names is looking for.
    async fn fetch_spans(
        &self,
        filters: &EventFilters,
    ) -> Result<Vec<ObservabilityEvent>, ObservabilityError> {
        let query = TraceQuery {
            project_id: filters.project_id,
            start_time: filters.from,
            end_time: filters.to,
            // environment_id is intentionally NOT passed: the pre-storage
            // implementation never filtered spans by environment (otel_spans
            // has no env column; TimescaleDB resolves it via a deployments
            // join and ClickHouse can't resolve it at all), so passing it
            // would make the two backends disagree. Deployment scoping covers
            // the common case.
            deployment_id: filters.deployment_id,
            name_pattern: filters.search.clone(),
            // Roots only by default (one row per trace); a name search widens
            // to all spans so child-span names are discoverable.
            root_only: filters.search.is_none(),
            limit: Some(filters.limit),
            ..Default::default()
        };
        let rows = self.otel.query_spans(query).await.map_err(|source| {
            ObservabilityError::TraceStore {
                project_id: filters.project_id,
                source,
            }
        })?;

        Ok(rows.into_iter().map(map_span).collect())
    }

    /// Resolve one span by the `{trace_id}:{span_id}` composite identity the
    /// list rows emit. Fetching the whole trace and picking the span keeps the
    /// lookup on the storage backend's cheap trace-scoped path (primary-key
    /// prefix on both TimescaleDB and ClickHouse) without needing a per-row
    /// serial id, which the ClickHouse table doesn't have.
    async fn find_span_by_id(
        &self,
        project_id: i32,
        id: &str,
    ) -> Result<Option<SpanRecord>, ObservabilityError> {
        let Some((trace_id, span_id)) = id.split_once(':') else {
            return Err(ObservabilityError::EventNotFound {
                project_id,
                kind: "span".into(),
                event_id: id.into(),
            });
        };
        let spans = self
            .otel
            .get_trace(project_id, trace_id)
            .await
            .map_err(|source| ObservabilityError::TraceStore { project_id, source })?;
        Ok(spans.into_iter().find(|s| s.span_id == span_id))
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Per-kind row mappers (free functions so we can unit-test without DB)
// ──────────────────────────────────────────────────────────────────────────

pub fn map_request(m: proxy_logs::Model) -> ObservabilityEvent {
    let (req_headers, req_dropped) = project_headers(m.request_headers.as_ref());
    let (resp_headers, resp_dropped) = project_headers(m.response_headers.as_ref());
    ObservabilityEvent::Request(RequestRow {
        id: m.request_id,
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
    // The storage layer models attributes as BTreeMap<String, String>; the
    // wire type keeps the JSON-object shape the panel renders.
    let attrs_value = serde_json::Value::Object(
        s.attributes
            .into_iter()
            .map(|(k, v)| (k, serde_json::Value::String(v)))
            .collect(),
    );
    let (attributes, attributes_truncated) = truncate_attributes(Some(&attrs_value));
    ObservabilityEvent::Span(SpanRow {
        // Composite identity — the span table has no serial PK on ClickHouse,
        // and (trace_id, span_id) is unique on both backends.
        id: format!("{}:{}", s.trace_id, s.span_id),
        ts: s.start_time,
        deployment_id: s.deployment_id,
        environment_id: None,
        trace_id: s.trace_id,
        span_id: s.span_id,
        parent_span_id: s.parent_span_id,
        service: s.resource.service_name,
        operation: s.name,
        duration_ms: Some(s.duration_ms),
        status: Some(s.status_code.to_string()),
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
        // Identity is the backend-agnostic request_id, NOT the serial PK —
        // ClickHouse rows come back with id = 0, so the PK cannot key rows.
        assert_eq!(r.id, "req-1");
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
    fn map_span_uses_composite_identity_and_stringifies_attributes() {
        use std::collections::BTreeMap;
        let mut attributes = BTreeMap::new();
        attributes.insert("http.method".to_string(), "GET".to_string());
        let record = SpanRecord {
            project_id: 1,
            deployment_id: Some(3),
            resource: temps_otel::types::ResourceInfo {
                service_name: "api".into(),
                service_version: None,
                deployment_environment: Some("production".into()),
                attributes: BTreeMap::new(),
            },
            trace_id: "4bf92f3577b34da6a3ce929d0e0e4736".into(),
            span_id: "00f067aa0ba902b7".into(),
            parent_span_id: None,
            name: "GET /api".into(),
            kind: temps_otel::types::SpanKind::Server,
            start_time: Utc::now(),
            end_time: Utc::now(),
            duration_ms: 12.5,
            status_code: temps_otel::types::SpanStatusCode::Error,
            status_message: "boom".into(),
            attributes,
            events: vec![],
        };
        let ObservabilityEvent::Span(s) = map_span(record) else {
            panic!("expected span");
        };
        // (trace_id, span_id) is the only identity that exists on BOTH
        // storage backends — ClickHouse spans have no serial id.
        assert_eq!(s.id, "4bf92f3577b34da6a3ce929d0e0e4736:00f067aa0ba902b7");
        assert_eq!(s.span_id, "00f067aa0ba902b7");
        assert_eq!(s.service, "api");
        assert_eq!(s.status.as_deref(), Some("ERROR"));
        assert_eq!(s.attributes["http.method"], "GET");
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
