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
fn ordered(sql: &str, q: &GlobalTraceQuery) -> String {
    let field = if q.filter.sort_by == TraceSortField::Duration {
        "duration"
    } else {
        "start_ms"
    };
    format!(
        "{sql} ORDER BY {field} {}, project_id, trace_id, span_id LIMIT {} OFFSET {}",
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
    let empty = BTreeMap::new();
    let sql = build(
        q,
        if refs.is_some() {
            Dialect::Cloud
        } else {
            Dialect::ClickHouse
        },
        refs.unwrap_or(&empty),
    )?;
    let count_sql = format!("SELECT count() FROM ({})", sql.body);
    let total = ch_query(client, &count_sql, &sql.binds)
        .fetch_one::<u64>()
        .await
        .map_err(storage)?;
    let cursor = ch_query(client, &ordered(&sql.body, q), &sql.binds)
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
    let sql = build(q, Dialect::Postgres, &BTreeMap::new())?;
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
        Statement::from_sql_and_values(DatabaseBackend::Postgres, ordered(&sql.body, q), values);
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
        assert!(ordered(&sql.body, &q).ends_with("LIMIT 20 OFFSET 800"));
        assert!(sql.body.contains("GROUP BY project_id, trace_id"));
        assert!(!sql.body.contains("FINAL"));
    }
}
