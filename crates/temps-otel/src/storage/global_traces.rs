// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Global reads use one ordered query per storage source, never one per project.
//! Rows are streamed through a bounded merge; a deep offset does not buffer its
//! preceding pages in the instance or issue additional page queries.
use super::StorageResult;
use crate::{
    error::{OtelError, StorageErrorKind},
    types::*,
};
use chrono::{DateTime, Utc};
use futures::{Stream, TryStreamExt};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, StreamTrait};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, pin::Pin, sync::Arc};
use utoipa::ToSchema;

#[derive(Debug, Clone)]
pub struct TraceReadScope {
    pub project_id: i32,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub cloud: bool,
    /// The cutover that narrowed this project's requested window, if any.
    pub window_clamped_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct GlobalTraceQuery {
    pub filter: TraceQuery,
    pub scopes: Vec<TraceReadScope>,
    pub summaries: bool,
    /// Use the local Postgres one-row-per-trace table when semantics allow it.
    /// This remains enabled for both halves of a mixed local/Cloud read so
    /// every source uses window membership plus lifetime aggregate values.
    /// Adding an unrelated project therefore cannot change an existing trace's
    /// values or compare incompatible sort keys in global pagination.
    pub use_preaggregated_summaries: bool,
    pub source_offset: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GlobalTraceSummary {
    pub project_id: i32,
    pub project_name: String,
    pub project_slug: String,
    #[serde(flatten)]
    pub trace: TraceSummary,
}

/// Common storage projection; also preserves the raw span payload on span reads.
#[derive(Debug, Clone, Deserialize, clickhouse::Row)]
pub struct GlobalTraceRow {
    pub project_id: i32,
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: String,
    pub name: String,
    pub service_name: String,
    pub environment: String,
    pub kind: String,
    pub status: String,
    pub start_ms: i64,
    pub duration: f64,
    pub span_count: i64,
    pub error_count: i64,
    pub attributes: String,
    pub events: String,
    pub status_message: String,
}

/// Cloud pages carry their exact total on each returned row so the expensive
/// lifetime aggregation runs once instead of once for `COUNT` and again for
/// the page. The SQL limit keeps this decoded buffer page-bounded.
#[derive(Debug, Deserialize, clickhouse::Row)]
struct CloudGlobalTraceRow {
    project_id: i32,
    trace_id: String,
    span_id: String,
    parent_span_id: String,
    name: String,
    service_name: String,
    environment: String,
    kind: String,
    status: String,
    start_ms: i64,
    duration: f64,
    span_count: i64,
    error_count: i64,
    attributes: String,
    events: String,
    status_message: String,
    total: u64,
}

impl CloudGlobalTraceRow {
    fn into_parts(self) -> (GlobalTraceRow, u64) {
        let total = self.total;
        (
            GlobalTraceRow {
                project_id: self.project_id,
                trace_id: self.trace_id,
                span_id: self.span_id,
                parent_span_id: self.parent_span_id,
                name: self.name,
                service_name: self.service_name,
                environment: self.environment,
                kind: self.kind,
                status: self.status,
                start_ms: self.start_ms,
                duration: self.duration,
                span_count: self.span_count,
                error_count: self.error_count,
                attributes: self.attributes,
                events: self.events,
                status_message: self.status_message,
            },
            total,
        )
    }
}
impl GlobalTraceRow {
    pub fn span(self) -> StorageResult<SpanRecord> {
        let start_time = DateTime::from_timestamp_millis(self.start_ms)
            .ok_or_else(|| invalid("Invalid trace timestamp"))?;
        Ok(SpanRecord {
            project_id: self.project_id,
            deployment_id: None,
            trace_id: self.trace_id,
            span_id: self.span_id,
            parent_span_id: (!self.parent_span_id.is_empty()).then_some(self.parent_span_id),
            name: self.name,
            kind: parse_kind(&self.kind),
            start_time,
            end_time: start_time + chrono::Duration::microseconds((self.duration * 1000.0) as i64),
            duration_ms: self.duration,
            status_code: parse_status(&self.status),
            status_message: self.status_message,
            attributes: serde_json::from_str(&self.attributes)
                .map_err(|e| invalid(&e.to_string()))?,
            events: serde_json::from_str(&self.events).map_err(|e| invalid(&e.to_string()))?,
            resource: ResourceInfo {
                service_name: self.service_name,
                deployment_environment: (!self.environment.is_empty()).then_some(self.environment),
                ..Default::default()
            },
        })
    }
    pub fn summary(self, name: String, slug: String) -> StorageResult<GlobalTraceSummary> {
        Ok(GlobalTraceSummary {
            project_id: self.project_id,
            project_name: name,
            project_slug: slug,
            trace: TraceSummary {
                trace_id: self.trace_id,
                root_span_name: self.name,
                service_name: self.service_name,
                deployment_environment: (!self.environment.is_empty()).then_some(self.environment),
                kind: parse_kind(&self.kind),
                status_code: parse_status(&self.status),
                start_time: DateTime::from_timestamp_millis(self.start_ms)
                    .ok_or_else(|| invalid("Invalid trace timestamp"))?,
                duration_ms: self.duration,
                span_count: self.span_count,
                error_count: self.error_count,
            },
        })
    }
}
fn parse_kind(s: &str) -> SpanKind {
    match s.to_ascii_uppercase().as_str() {
        "SERVER" => SpanKind::Server,
        "CLIENT" => SpanKind::Client,
        "PRODUCER" => SpanKind::Producer,
        "CONSUMER" => SpanKind::Consumer,
        _ => SpanKind::Internal,
    }
}
fn parse_status(s: &str) -> SpanStatusCode {
    match s.to_ascii_uppercase().as_str() {
        "ERROR" => SpanStatusCode::Error,
        "OK" => SpanStatusCode::Ok,
        _ => SpanStatusCode::Unset,
    }
}
pub fn invalid(message: &str) -> OtelError {
    OtelError::Validation {
        message: message.into(),
    }
}
fn storage(error: impl std::fmt::Display) -> OtelError {
    OtelError::Storage {
        message: format!("Global trace query failed: {error}"),
        kind: StorageErrorKind::ClickHouseOther,
    }
}

pub struct GlobalTraceStream {
    pub total: u64,
    pub rows: Pin<Box<dyn Stream<Item = StorageResult<GlobalTraceRow>> + Send>>,
}
impl GlobalTraceStream {
    pub fn empty() -> Self {
        Self {
            total: 0,
            rows: Box::pin(futures::stream::empty()),
        }
    }
}
pub struct GlobalTracePage {
    pub data: Vec<GlobalTraceRow>,
    pub total: u64,
}

/// Two sorted storage cursors, at most one lookahead per source plus the page.
pub async fn merge(
    mut sources: Vec<GlobalTraceStream>,
    q: &GlobalTraceQuery,
) -> StorageResult<GlobalTracePage> {
    let total = sources.iter().map(|s| s.total).sum();
    let mut heads = Vec::with_capacity(sources.len());
    for s in &mut sources {
        heads.push(s.rows.try_next().await?);
    }
    let mut data = Vec::new();
    let offset = q.filter.offset.unwrap_or(0).saturating_sub(q.source_offset);
    let limit = q.filter.limit.unwrap_or(20).clamp(1, 100);
    for seen in 0..offset.saturating_add(limit) {
        let next = heads
            .iter()
            .enumerate()
            .filter_map(|(i, h)| h.as_ref().map(|r| (i, r)))
            .min_by(|(_, a), (_, b)| compare(a, b, &q.filter))
            .map(|(i, _)| i);
        let Some(i) = next else { break };
        let row = heads[i]
            .take()
            .ok_or_else(|| invalid("Selected global trace source has no pending row"))?;
        if seen >= offset {
            data.push(row);
        }
        if seen + 1 < offset.saturating_add(limit) {
            heads[i] = sources[i].rows.try_next().await?;
        }
    }
    Ok(GlobalTracePage { data, total })
}
fn compare(a: &GlobalTraceRow, b: &GlobalTraceRow, q: &TraceQuery) -> std::cmp::Ordering {
    let order = match q.sort_by {
        TraceSortField::Duration => a.duration.total_cmp(&b.duration),
        TraceSortField::StartTime => a.start_ms.cmp(&b.start_ms),
    };
    let order = if q.sort_order == SortOrder::Desc {
        order.reverse()
    } else {
        order
    };
    order
        .then(a.project_id.cmp(&b.project_id))
        .then(a.trace_id.cmp(&b.trace_id))
        .then(a.span_id.cmp(&b.span_id))
}

#[derive(Clone, Copy, PartialEq)]
pub enum Dialect {
    Postgres,
    ClickHouse,
    Cloud,
}
#[derive(Clone)]
enum Bind {
    Text(String),
    Int(i64),
    Float(f64),
}
struct Sql {
    body: String,
    binds: Vec<Bind>,
}

/// Maximum number of Cloud traces whose lifetime spans one request may expand.
/// The proxy separately caps rows read, memory, results, and execution time;
/// this client-side gate makes the fan-out explicit before the historical join.
const MAX_CLOUD_LIFETIME_CANDIDATES: u64 = 5_000;

/// Build the indexed Postgres trace-summary query.
///
/// The project-scoped trace list already maintains one row per trace in
/// `otel_trace_summaries`. Global reads must use the same table: rebuilding
/// every summary from `otel_spans` makes a small page proportional to the
/// number of spans in every accessible project.
fn build_postgres_summaries(q: &GlobalTraceQuery) -> StorageResult<Sql> {
    let mut binds = Vec::new();
    let mut bind = |value: Bind| {
        binds.push(value);
        format!("${}", binds.len())
    };
    let scopes = q
        .scopes
        .iter()
        .map(|scope| {
            let project_id = bind(Bind::Int(scope.project_id as i64));
            let from = bind(Bind::Text(scope.from.to_rfc3339()));
            let to = bind(Bind::Text(scope.to.to_rfc3339()));
            (
                format!("(ts.project_id = {project_id} AND ts.last_span_start_time >= {from}::timestamptz AND ts.start_time <= {to}::timestamptz)"),
                format!("(window_span.project_id = {project_id} AND window_span.start_time >= {from}::timestamptz AND window_span.start_time <= {to}::timestamptz)"),
            )
        })
        .collect::<Vec<_>>();
    let summary_scope = scopes
        .iter()
        .map(|(summary, _)| summary.as_str())
        .collect::<Vec<_>>()
        .join(" OR ");
    let span_scope = scopes
        .iter()
        .map(|(_, span)| span.as_str())
        .collect::<Vec<_>>()
        .join(" OR ");

    // Match the project trace-list semantics: any error makes the trace an
    // error; every other summary is OK. Do not look up raw spans per result —
    // compressed Timescale chunks cannot seek by trace_id, so that would turn
    // a 100-row page into 100 decompression scans.
    let status = "CASE WHEN ts.error_count > 0 THEN 'ERROR' ELSE 'OK' END";
    Ok(Sql {
        body: format!(
            "SELECT ts.project_id, ts.trace_id, ''::text AS span_id, ''::text AS parent_span_id, ts.root_span_name AS name, ts.service_name, COALESCE(ts.deployment_environment, '') AS environment, ts.kind, {status} AS status, FLOOR(EXTRACT(EPOCH FROM ts.start_time) * 1000)::bigint AS start_ms, ts.duration_ms AS duration, ts.span_count, ts.error_count, '{{}}'::text AS attributes, '[]'::text AS events, ''::text AS status_message FROM otel_trace_summaries ts WHERE ({summary_scope}) AND (ts.project_id, ts.trace_id) IN (SELECT window_span.project_id, window_span.trace_id FROM otel_spans window_span WHERE {span_scope} GROUP BY window_span.project_id, window_span.trace_id)"
        ),
        binds,
    })
}

/// Whether an unfiltered summary response may expose whole-trace values.
///
/// Every source participating in a merged page must make this decision the
/// same way. Otherwise duration/start ordering changes at the storage boundary.
fn can_use_lifetime_summaries(q: &GlobalTraceQuery) -> bool {
    q.summaries
        && q.use_preaggregated_summaries
        // Filtered global queries retain the existing any-matching-span
        // semantics. The summary row stores root/whole-trace fields, so using
        // it for these filters would change membership at service, deployment,
        // duration, or status boundaries.
        && q.filter.trace_id.is_none()
        && q.filter.service_name.is_none()
        && q.filter.status.is_none()
        && q.filter.min_duration_ms.is_none()
        && q.filter.deployment_id.is_none()
        && q.filter.environment_id.is_none()
        && q.filter
            .attributes
            .as_ref()
            .is_none_or(std::collections::BTreeMap::is_empty)
        && q.filter.name_pattern.as_ref().is_none_or(String::is_empty)
}

/// Build Cloud summaries with window membership and lifetime values.
fn build_cloud_lifetime_summaries(
    q: &GlobalTraceQuery,
    refs: &BTreeMap<i32, String>,
) -> StorageResult<Sql> {
    let mut binds = Vec::new();
    let mut bind = |value: Bind| {
        binds.push(value);
        "?".to_string()
    };
    let mut mapping = Vec::with_capacity(q.scopes.len());
    for scope in &q.scopes {
        let project_ref = refs
            .get(&scope.project_id)
            .ok_or_else(|| invalid("Missing Cloud project scope"))?;
        mapping.push(format!(
            "WHEN {} THEN {}",
            bind(Bind::Text(project_ref.clone())),
            scope.project_id
        ));
    }
    let mut membership = Vec::with_capacity(q.scopes.len());
    for scope in &q.scopes {
        let project_ref = refs
            .get(&scope.project_id)
            .ok_or_else(|| invalid("Missing Cloud project scope"))?;
        membership.push(format!(
            "(project_ref = {} AND toUnixTimestamp64Milli(ts) >= {} AND toUnixTimestamp64Milli(ts) <= {})",
            bind(Bind::Text(project_ref.clone())),
            bind(Bind::Int(scope.from.timestamp_millis())),
            bind(Bind::Int(scope.to.timestamp_millis()))
        ));
    }
    let membership = if membership.is_empty() {
        "FALSE".to_string()
    } else {
        membership.join(" OR ")
    };
    let pick = |field: &str| {
        format!("argMax(raw.{field}, tuple(raw.parent_span_id = '', raw.duration, raw.span_id))")
    };
    let body = format!(
        "WITH candidates AS (SELECT project_ref, trace_id FROM telemetry_spans WHERE {membership} GROUP BY project_ref, trace_id), raw AS (SELECT toInt32(CASE span.project_ref {} ELSE 0 END) AS project_id, span.trace_id, span.span_id, COALESCE(span.parent_span_id, '') AS parent_span_id, span.name, span.service_name, COALESCE(span.environment, '') AS environment, span.span_kind AS kind, upper(span.status_code) AS status, toUnixTimestamp64Milli(span.ts) AS start_ms, span.duration_ms AS duration FROM telemetry_spans AS span INNER JOIN candidates AS candidate ON candidate.project_ref = span.project_ref AND candidate.trace_id = span.trace_id), grouped AS (SELECT project_id, trace_id, '' AS span_id, '' AS parent_span_id, {} AS name, {} AS service_name, {} AS environment, {} AS kind, CASE WHEN countIf(raw.status = 'ERROR') > 0 THEN 'ERROR' ELSE 'OK' END AS status, MIN(raw.start_ms) AS start_ms, MAX(raw.duration) AS duration, toInt64(count()) AS span_count, toInt64(countIf(raw.status = 'ERROR')) AS error_count, '{{}}' AS attributes, '[]' AS events, '' AS status_message FROM raw GROUP BY project_id, trace_id) SELECT * FROM grouped",
        mapping.join(" "),
        pick("name"),
        pick("service_name"),
        pick("environment"),
        pick("kind")
    );
    Ok(Sql { body, binds })
}

fn build_cloud_lifetime_candidate_count(
    q: &GlobalTraceQuery,
    refs: &BTreeMap<i32, String>,
) -> StorageResult<Sql> {
    let mut binds = Vec::new();
    let mut bind = |value: Bind| {
        binds.push(value);
        "?".to_string()
    };
    let membership = q
        .scopes
        .iter()
        .map(|scope| {
            let project_ref = refs
                .get(&scope.project_id)
                .ok_or_else(|| invalid("Missing Cloud project scope"))?;
            Ok(format!(
                "(project_ref = {} AND toUnixTimestamp64Milli(ts) >= {} AND toUnixTimestamp64Milli(ts) <= {})",
                bind(Bind::Text(project_ref.clone())),
                bind(Bind::Int(scope.from.timestamp_millis())),
                bind(Bind::Int(scope.to.timestamp_millis()))
            ))
        })
        .collect::<StorageResult<Vec<_>>>()?
        .join(" OR ");
    Ok(Sql {
        body: format!(
            "SELECT uniqExact(tuple(project_ref, trace_id)) FROM telemetry_spans WHERE {membership}"
        ),
        binds,
    })
}

pub(crate) async fn trace_summary_rebuild_pending(db: &DatabaseConnection) -> StorageResult<bool> {
    let state_exists = db
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT to_regclass('otel_trace_summary_rebuild_state') IS NOT NULL AS present"
                .to_string(),
        ))
        .await?
        .and_then(|row| row.try_get::<bool>("", "present").ok())
        .unwrap_or(false);
    if !state_exists {
        return Ok(false);
    }
    Ok(db
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT EXISTS (SELECT 1 FROM otel_trace_summary_rebuild_state \
             WHERE NOT completed) AS pending"
                .to_string(),
        ))
        .await?
        .and_then(|row| row.try_get::<bool>("", "pending").ok())
        .unwrap_or(false))
}
fn build(
    q: &GlobalTraceQuery,
    dialect: Dialect,
    refs: &BTreeMap<i32, String>,
) -> StorageResult<Sql> {
    let pg = dialect == Dialect::Postgres;
    let cloud = dialect == Dialect::Cloud;
    let mut binds = Vec::new();
    let mut bind = |v: Bind| {
        binds.push(v);
        if pg {
            format!("${}", binds.len())
        } else {
            "?".into()
        }
    };
    let mut scope_sql = Vec::new();
    let mut mapping = Vec::new();
    for s in &q.scopes {
        if cloud {
            let key = refs
                .get(&s.project_id)
                .ok_or_else(|| invalid("Missing Cloud project scope"))?;
            // This mapping contains only server-derived pseudonyms and numeric local IDs.
            mapping.push(format!(
                "WHEN {} THEN {}",
                bind(Bind::Text(key.clone())),
                s.project_id
            ));
        }
    }
    let project = if cloud {
        format!("toInt32(CASE project_ref {} ELSE 0 END)", mapping.join(" "))
    } else {
        "project_id".into()
    };
    for s in &q.scopes {
        let id = if cloud {
            bind(Bind::Text(refs[&s.project_id].clone()))
        } else {
            bind(Bind::Int(s.project_id as i64))
        };
        let col = if cloud { "project_ref" } else { "project_id" };
        let ts = if cloud {
            "toUnixTimestamp64Milli(ts)"
        } else if pg {
            "FLOOR(EXTRACT(EPOCH FROM start_time) * 1000)::bigint"
        } else {
            "toUnixTimestamp64Milli(start_time)"
        };
        scope_sql.push(format!(
            "({col} = {id} AND {ts} >= {} AND {ts} <= {})",
            bind(Bind::Int(s.from.timestamp_millis())),
            bind(Bind::Int(s.to.timestamp_millis()))
        ));
    }
    let scope = if scope_sql.is_empty() {
        "FALSE".into()
    } else {
        scope_sql.join(" OR ")
    };
    let ts = if cloud {
        "toUnixTimestamp64Milli(ts)"
    } else if pg {
        "FLOOR(EXTRACT(EPOCH FROM start_time) * 1000)::bigint"
    } else {
        "toUnixTimestamp64Milli(start_time)"
    };
    let env = if cloud {
        "environment"
    } else {
        "deployment_environment"
    };
    let kind = if cloud { "span_kind" } else { "kind" };
    let attrs = if cloud {
        "'{}'"
    } else if pg {
        "attributes::text"
    } else {
        "attributes"
    };
    let events = if cloud {
        "'[]'"
    } else if pg {
        "events::text"
    } else {
        "events"
    };
    let message = if cloud { "''" } else { "status_message" };
    let table = if cloud {
        "telemetry_spans"
    } else if pg {
        "otel_spans"
    } else {
        "spans"
    };
    let mut predicates = vec![format!("({scope})")];
    let f = &q.filter;
    for (column, value) in [("trace_id", &f.trace_id), ("service_name", &f.service_name)] {
        if let Some(v) = value {
            predicates.push(format!("{column} = {}", bind(Bind::Text(v.clone()))));
        }
    }
    if let Some(v) = &f.name_pattern {
        predicates.push(format!("name ILIKE {}", bind(Bind::Text(format!("%{v}%")))));
    }
    if let Some(v) = f.deployment_id {
        if cloud {
            return Err(invalid(
                "Deployment filters are unavailable for Cloud traces",
            ));
        }
        predicates.push(format!("deployment_id = {}", bind(Bind::Int(v as i64))));
    }
    if f.environment_id.is_some() {
        return Err(invalid(
            "Use the project trace endpoint for environment ID filters",
        ));
    }
    if let Some(attrs) = &f.attributes {
        for (k, v) in attrs {
            if cloud {
                return Err(invalid(
                    "Attribute filters are unavailable for Cloud traces",
                ));
            }
            let key = bind(Bind::Text(k.clone()));
            let value = bind(Bind::Text(v.clone()));
            predicates.push(if pg {
                format!("attributes->>{key} = {value}")
            } else {
                format!("JSONExtractString(attributes, {key}) = {value}")
            });
        }
    }
    let mut projection = format!("SELECT {project} AS project_id, trace_id, span_id, COALESCE(parent_span_id, '') AS parent_span_id, name, service_name, COALESCE({env}, '') AS environment, {kind} AS kind, upper(status_code) AS status, {ts} AS start_ms, duration_ms AS duration, {attrs} AS attributes, {events} AS events, {message} AS status_message FROM {table} WHERE {}", predicates.join(" AND "));
    if dialect == Dialect::ClickHouse {
        projection.push_str(" ORDER BY _version DESC LIMIT 1 BY project_id, trace_id, span_id");
    }
    let mut having = Vec::new();
    if let Some(v) = f.min_duration_ms {
        having.push(format!("duration >= {}", bind(Bind::Float(v))));
    }
    if let Some(v) = f.status {
        having.push(format!("status = {}", bind(Bind::Text(v.to_string()))));
    }
    let filter = if having.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", having.join(" AND "))
    };
    let body = if q.summaries {
        let pick = |field: &str| {
            if pg {
                format!("(array_agg(raw.{field} ORDER BY (raw.parent_span_id = '') DESC, raw.duration DESC, raw.span_id))[1]")
            } else {
                format!("argMax(raw.{field}, tuple(raw.parent_span_id = '', raw.duration, raw.span_id))")
            }
        };
        let counts = if pg {
            "COUNT(*)::bigint AS span_count, COUNT(*) FILTER (WHERE raw.status = 'ERROR')::bigint AS error_count"
        } else {
            "toInt64(count()) AS span_count, toInt64(countIf(raw.status = 'ERROR')) AS error_count"
        };
        format!("WITH raw AS ({projection}), grouped AS (SELECT project_id, trace_id, '' AS span_id, '' AS parent_span_id, {} AS name, {} AS service_name, {} AS environment, {} AS kind, CASE WHEN {} > 0 THEN 'ERROR' ELSE {} END AS status, MIN(raw.start_ms) AS start_ms, MAX(raw.duration) AS duration, {counts}, '{{}}' AS attributes, '[]' AS events, '' AS status_message FROM raw GROUP BY project_id, trace_id) SELECT * FROM grouped{filter}", pick("name"),pick("service_name"),pick("environment"),pick("kind"),if pg { "COUNT(*) FILTER (WHERE raw.status = 'ERROR')" } else { "countIf(raw.status = 'ERROR')" },pick("status"))
    } else {
        let count = if pg { "1::bigint" } else { "toInt64(1)" };
        let error = if pg {
            "CASE WHEN status = 'ERROR' THEN 1::bigint ELSE 0::bigint END"
        } else {
            "toInt64(status = 'ERROR')"
        };
        format!("WITH raw AS ({projection}) SELECT project_id, trace_id, span_id, parent_span_id, name, service_name, environment, kind, status, start_ms, duration, {count} AS span_count, {error} AS error_count, attributes, events, status_message FROM raw{filter}")
    };
    Ok(Sql { body, binds })
}
fn ordered(sql: &Sql, q: &GlobalTraceQuery) -> String {
    let field = if q.filter.sort_by == TraceSortField::Duration {
        "duration"
    } else {
        "start_ms"
    };
    format!(
        "{} ORDER BY {field} {}, project_id, trace_id, span_id LIMIT {} OFFSET {}",
        sql.body,
        q.filter.sort_order.as_sql(),
        q.filter
            .offset
            .unwrap_or(0)
            .saturating_add(q.filter.limit.unwrap_or(20).clamp(1, 100))
            .saturating_sub(q.source_offset),
        q.source_offset
    )
}

fn cloud_ordered_with_total(sql: &Sql, q: &GlobalTraceQuery) -> String {
    let field = if q.filter.sort_by == TraceSortField::Duration {
        "duration"
    } else {
        "start_ms"
    };
    format!(
        "SELECT page.*, count() OVER () AS total FROM ({}) AS page ORDER BY {field} {}, project_id, trace_id, span_id LIMIT {} OFFSET {}",
        sql.body,
        q.filter.sort_order.as_sql(),
        q.filter
            .offset
            .unwrap_or(0)
            .saturating_add(q.filter.limit.unwrap_or(20).clamp(1, 100))
            .saturating_sub(q.source_offset),
        q.source_offset
    )
}
fn ch_query(client: &clickhouse::Client, sql: &str, binds: &[Bind]) -> clickhouse::query::Query {
    let mut query = client.query(sql);
    for b in binds {
        query = match b {
            Bind::Text(v) => query.bind(v),
            Bind::Int(v) => query.bind(v),
            Bind::Float(v) => query.bind(v),
        };
    }
    query
}
pub async fn clickhouse(
    client: &clickhouse::Client,
    q: &GlobalTraceQuery,
    refs: Option<&BTreeMap<i32, String>>,
) -> StorageResult<GlobalTraceStream> {
    if q.scopes.is_empty() {
        return Ok(GlobalTraceStream::empty());
    }
    let cloud_lifetime_total = if let Some(refs) = refs.filter(|_| can_use_lifetime_summaries(q)) {
        let count = build_cloud_lifetime_candidate_count(q, refs)?;
        let total = temps_cloud_client::query::within_query_budget(
            ch_query(client, &count.body, &count.binds).fetch_one::<u64>(),
        )
        .await
        .map_err(|error| OtelError::Storage {
            message: format!(
                "Temps Cloud trace-candidate count exceeded its wall-clock budget: {error}"
            ),
            kind: StorageErrorKind::ClickHouseTimeout,
        })?
        .map_err(storage)?;
        if total > MAX_CLOUD_LIFETIME_CANDIDATES {
            return Err(invalid(&format!(
                "Cloud trace summary query matched {total} traces; the safe limit is \
                 {MAX_CLOUD_LIFETIME_CANDIDATES}. Choose a shorter time range or fewer projects."
            )));
        }
        Some(total)
    } else {
        None
    };
    let empty = BTreeMap::new();
    let sql = if let (Some(refs), Some(_)) = (refs, cloud_lifetime_total) {
        build_cloud_lifetime_summaries(q, refs)?
    } else {
        build(
            q,
            if refs.is_some() {
                Dialect::Cloud
            } else {
                Dialect::ClickHouse
            },
            refs.unwrap_or(&empty),
        )?
    };
    if let Some(total) = cloud_lifetime_total {
        let page_sql = ordered(&sql, q);
        let rows = temps_cloud_client::query::within_query_budget(
            ch_query(client, &page_sql, &sql.binds).fetch_all::<GlobalTraceRow>(),
        )
        .await
        .map_err(|error| OtelError::Storage {
            message: format!(
                "Temps Cloud lifetime trace page exceeded its wall-clock budget: {error}"
            ),
            kind: StorageErrorKind::ClickHouseTimeout,
        })?
        .map_err(storage)?
        .into_iter()
        .map(Ok)
        .collect::<Vec<_>>();
        return Ok(GlobalTraceStream {
            total,
            rows: Box::pin(futures::stream::iter(rows)),
        });
    }
    if refs.is_some() {
        let page_sql = cloud_ordered_with_total(&sql, q);
        let fetched = temps_cloud_client::query::within_query_budget(
            ch_query(client, &page_sql, &sql.binds).fetch_all::<CloudGlobalTraceRow>(),
        )
        .await
        .map_err(|error| OtelError::Storage {
            message: format!(
                "Temps Cloud global trace query exceeded its wall-clock budget: {error}"
            ),
            kind: StorageErrorKind::ClickHouseTimeout,
        })?
        .map_err(storage)?;
        let mut total = None;
        let rows = fetched
            .into_iter()
            .map(|row| {
                let (row, row_total) = row.into_parts();
                total = Some(row_total);
                Ok(row)
            })
            .collect::<Vec<_>>();
        let total = if let Some(total) = total {
            total
        } else {
            // An offset beyond the final row carries no window-count value.
            // Keep exact totals for that rare page, under the same hard budget.
            let count_sql = format!("SELECT count() FROM ({})", sql.body);
            temps_cloud_client::query::within_query_budget(
                ch_query(client, &count_sql, &sql.binds).fetch_one::<u64>(),
            )
            .await
            .map_err(|error| OtelError::Storage {
                message: format!(
                    "Temps Cloud global trace count exceeded its wall-clock budget: {error}"
                ),
                kind: StorageErrorKind::ClickHouseTimeout,
            })?
            .map_err(storage)?
        };
        return Ok(GlobalTraceStream {
            total,
            rows: Box::pin(futures::stream::iter(rows)),
        });
    }
    let count_sql = format!("SELECT count() FROM ({})", sql.body);
    let total = ch_query(client, &count_sql, &sql.binds)
        .fetch_one::<u64>()
        .await
        .map_err(storage)?;
    let cursor = ch_query(client, &ordered(&sql, q), &sql.binds)
        .fetch::<GlobalTraceRow>()
        .map_err(storage)?;
    let rows = futures::stream::try_unfold(cursor, |mut cursor| async move {
        Ok(cursor
            .next()
            .await
            .map_err(storage)?
            .map(|row| (row, cursor)))
    });
    Ok(GlobalTraceStream {
        total,
        rows: Box::pin(rows),
    })
}
pub async fn postgres(
    db: Arc<DatabaseConnection>,
    q: &GlobalTraceQuery,
) -> StorageResult<GlobalTraceStream> {
    if q.scopes.is_empty() {
        return Ok(GlobalTraceStream::empty());
    }
    let use_summaries =
        can_use_lifetime_summaries(q) && !trace_summary_rebuild_pending(&db).await?;
    let sql = if use_summaries {
        build_postgres_summaries(q)?
    } else {
        build(q, Dialect::Postgres, &BTreeMap::new())?
    };
    let values: Vec<sea_orm::Value> = sql
        .binds
        .iter()
        .map(|b| match b {
            Bind::Text(v) => v.clone().into(),
            Bind::Int(v) => (*v).into(),
            Bind::Float(v) => (*v).into(),
        })
        .collect();
    let count = db
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!(
                "SELECT COUNT(*)::bigint AS total FROM ({}) counted",
                sql.body
            ),
            values.clone(),
        ))
        .await?
        .ok_or_else(|| invalid("Missing trace count"))?;
    let total: i64 = count.try_get("", "total")?;
    let statement =
        Statement::from_sql_and_values(DatabaseBackend::Postgres, ordered(&sql, q), values);
    // The task owns the DB lifetime. Dropping the response closes this bounded
    // channel and cancels the database cursor, including while no rows arrive.
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    tokio::spawn(async move {
        let work = async {
            let mut stream = db.stream(statement).await?;
            while let Some(row) = stream.try_next().await? {
                macro_rules! get {
                    ($name:ident) => {
                        row.try_get("", stringify!($name))?
                    };
                }
                let item = GlobalTraceRow {
                    project_id: get!(project_id),
                    trace_id: get!(trace_id),
                    span_id: get!(span_id),
                    parent_span_id: get!(parent_span_id),
                    name: get!(name),
                    service_name: get!(service_name),
                    environment: get!(environment),
                    kind: get!(kind),
                    status: get!(status),
                    start_ms: get!(start_ms),
                    duration: get!(duration),
                    span_count: get!(span_count),
                    error_count: get!(error_count),
                    attributes: get!(attributes),
                    events: get!(events),
                    status_message: get!(status_message),
                };
                if tx.send(Ok(item)).await.is_err() {
                    break;
                }
            }
            Ok::<_, OtelError>(())
        };
        tokio::select! { _ = tx.closed() => {}, result = work => { if let Err(e) = result { let _ = tx.send(Err(e)).await; } } }
    });
    Ok(GlobalTraceStream {
        total: total as u64,
        rows: Box::pin(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|v| (v, rx))
        })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(project_id: i32, n: i64) -> GlobalTraceRow {
        GlobalTraceRow {
            project_id,
            trace_id: format!("trace-{n:04}"),
            span_id: String::new(),
            parent_span_id: String::new(),
            name: "GET /items".into(),
            service_name: "api".into(),
            environment: String::new(),
            kind: "SERVER".into(),
            status: "OK".into(),
            start_ms: n,
            duration: n as f64,
            span_count: 1,
            error_count: 0,
            attributes: "{}".into(),
            events: "[]".into(),
            status_message: String::new(),
        }
    }
    fn query() -> GlobalTraceQuery {
        GlobalTraceQuery {
            filter: TraceQuery {
                limit: Some(20),
                offset: Some(800),
                sort_order: SortOrder::Asc,
                ..Default::default()
            },
            scopes: vec![],
            summaries: true,
            use_preaggregated_summaries: true,
            source_offset: 0,
        }
    }
    #[tokio::test]
    async fn mixed_sources_stream_a_deep_page_with_exact_global_total() {
        let a = GlobalTraceStream {
            total: 500,
            rows: Box::pin(futures::stream::iter(
                (0..1000).step_by(2).map(|n| Ok(row(1, n))),
            )),
        };
        let b = GlobalTraceStream {
            total: 500,
            rows: Box::pin(futures::stream::iter(
                (1..1000).step_by(2).map(|n| Ok(row(2, n))),
            )),
        };
        let page = merge(vec![a, b], &query()).await.unwrap();
        assert_eq!(page.total, 1000);
        assert_eq!(page.data.len(), 20);
        assert_eq!(page.data[0].start_ms, 800);
        assert_eq!(page.data[19].start_ms, 819);
    }
    #[tokio::test]
    async fn a_source_failure_never_returns_a_partial_global_page() {
        let good = GlobalTraceStream {
            total: 1,
            rows: Box::pin(futures::stream::iter([Ok(row(1, 0))])),
        };
        let bad = GlobalTraceStream {
            total: 1,
            rows: Box::pin(futures::stream::iter([Err(invalid("unavailable"))])),
        };
        assert!(merge(vec![good, bad], &query()).await.is_err());
    }
    #[test]
    fn each_scope_is_bound_and_single_source_pagination_is_pushed_into_sql() {
        let mut q = query();
        q.source_offset = 800;
        let from = DateTime::from_timestamp_millis(1000).unwrap();
        let to = DateTime::from_timestamp_millis(2000).unwrap();
        q.scopes = (1..=105)
            .map(|project_id| TraceReadScope {
                project_id,
                from,
                to,
                cloud: false,
                window_clamped_at: None,
            })
            .collect();
        let sql = build(&q, Dialect::ClickHouse, &BTreeMap::new()).unwrap();
        assert_eq!(sql.binds.len(), 315);
        assert_eq!(sql.body.matches('?').count(), 315);
        assert!(ordered(&sql, &q).ends_with("LIMIT 20 OFFSET 800"));
        assert!(sql.body.contains("GROUP BY project_id, trace_id"));
        assert!(!sql.body.contains("FINAL"));
    }

    #[test]
    fn postgres_global_summaries_use_the_preaggregated_table() {
        let mut q = query();
        q.source_offset = 800;
        q.scopes = (1..=105)
            .map(|project_id| TraceReadScope {
                project_id,
                from: DateTime::from_timestamp_millis(1000).unwrap(),
                to: DateTime::from_timestamp_millis(2000).unwrap(),
                cloud: false,
                window_clamped_at: None,
            })
            .collect();

        let sql = build_postgres_summaries(&q).unwrap();

        assert_eq!(sql.binds.len(), 315);
        assert!(sql.body.contains("FROM otel_trace_summaries ts"));
        assert_eq!(sql.body.matches("GROUP BY").count(), 1);
        assert!(!sql.body.contains("FROM otel_spans span GROUP BY"));
        assert!(sql
            .body
            .contains("ts.last_span_start_time >= $2::timestamptz"));
        assert!(sql.body.contains("ts.start_time <= $3::timestamptz"));
        assert!(sql.body.contains("IN (SELECT window_span.project_id"));
        assert!(sql
            .body
            .contains("window_span.start_time >= $2::timestamptz"));
        assert!(sql
            .body
            .contains("window_span.start_time <= $3::timestamptz"));
        assert!(ordered(&sql, &q).contains("ORDER BY start_ms ASC"));
        assert!(ordered(&sql, &q).ends_with("LIMIT 20 OFFSET 800"));
    }

    #[test]
    fn cloud_global_summaries_use_window_membership_and_lifetime_values() {
        let mut q = query();
        q.source_offset = 0;
        q.scopes = (1..=2)
            .map(|project_id| TraceReadScope {
                project_id,
                from: DateTime::from_timestamp_millis(1000).unwrap(),
                to: DateTime::from_timestamp_millis(2000).unwrap(),
                cloud: true,
                window_clamped_at: None,
            })
            .collect();
        let refs = BTreeMap::from([(1, "project-a".into()), (2, "project-b".into())]);

        let count = build_cloud_lifetime_candidate_count(&q, &refs).unwrap();
        assert_eq!(count.binds.len(), 6);
        assert!(count
            .body
            .starts_with("SELECT uniqExact(tuple(project_ref, trace_id))"));

        let sql = build_cloud_lifetime_summaries(&q, &refs).unwrap();

        assert_eq!(sql.binds.len(), 8);
        assert_eq!(sql.body.matches('?').count(), 8);
        assert!(sql
            .body
            .contains("candidates AS (SELECT project_ref, trace_id FROM telemetry_spans"));
        assert!(sql
            .body
            .contains("toUnixTimestamp64Milli(ts) >= ? AND toUnixTimestamp64Milli(ts) <= ?"));
        assert!(sql.body.contains(
            "INNER JOIN candidates AS candidate ON candidate.project_ref = span.project_ref AND candidate.trace_id = span.trace_id"
        ));
        assert!(sql.body.contains("MIN(raw.start_ms) AS start_ms"));
        assert!(sql.body.contains("MAX(raw.duration) AS duration"));
        assert!(sql.body.contains("toInt64(count()) AS span_count"));
        assert!(sql.body.contains(
            "CASE WHEN countIf(raw.status = 'ERROR') > 0 THEN 'ERROR' ELSE 'OK' END AS status"
        ));
        assert!(ordered(&sql, &q).ends_with("LIMIT 820 OFFSET 0"));
        let page = cloud_ordered_with_total(&sql, &q);
        assert!(page.contains("count() OVER () AS total"));
        assert!(page.ends_with("LIMIT 820 OFFSET 0"));
    }

    #[test]
    fn filtered_queries_keep_the_exact_raw_span_path() {
        let mut q = query();
        q.filter.name_pattern = Some("checkout".into());
        assert!(!can_use_lifetime_summaries(&q));

        q.filter.name_pattern = None;
        q.filter
            .attributes
            .get_or_insert_default()
            .insert("http.method".into(), "GET".into());
        assert!(!can_use_lifetime_summaries(&q));

        q.filter.attributes = None;
        q.filter.status = Some(SpanStatusCode::Unset);
        assert!(!can_use_lifetime_summaries(&q));

        q.filter.status = None;
        q.filter.service_name = Some("worker".into());
        assert!(!can_use_lifetime_summaries(&q));

        q.filter.service_name = None;
        q.filter.min_duration_ms = Some(500.0);
        assert!(!can_use_lifetime_summaries(&q));

        q.filter.min_duration_ms = None;
        q.use_preaggregated_summaries = false;
        assert!(!can_use_lifetime_summaries(&q));
    }
}
