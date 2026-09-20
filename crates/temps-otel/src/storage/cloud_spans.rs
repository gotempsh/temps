// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The Cloud half of the span read path (ADR-040 §2/§4, ADR-041 §8).
//!
//! Reads this tenant's spans back out of Temps Cloud through the read proxy
//! `temps_cloud_client::query` already points a stock `clickhouse::Client` at.
//!
//! # The schema contract, and why it is written down here
//!
//! The Cloud-side schema is not in this repository. What *is* in this
//! repository — and is therefore the only contract this side owns — is exactly
//! what leaves the machine: `temps_cloud_protocol::SpanRecord`, built by
//! `cloud_span()` at the owning project's consented fidelity. Most column
//! names below are that struct's field names, one for one — **except the
//! timestamp**: the wire carries `ts_millis` (an epoch-millisecond integer),
//! but Cloud's ingest path (`temps-app-ingest`) converts it into a
//! `DateTime64(3)` column named `ts` before storing it, so it can partition
//! and TTL on it. A query against this table has to use the storage name and
//! type (`ts`, `DateTime64`), not the wire name and type — this file learned
//! that the hard way once already; see `git blame` on [`CloudSpanRow`].
//!
//! If the two ever diverge, every query fails upstream and nothing on this side
//! can explain why — the same failure mode
//! [`temps_cloud_client::query::CLOUD_TELEMETRY_DATABASE`] documents. That is
//! why the constants are named, commented and in one place rather than inlined
//! into query strings.
//!
//! # Scoping
//!
//! Cloud never learns a local project id. Rows are scoped by `project_ref` —
//! `HMAC(instance_token, "project\0" || project_id)` — which this side computes
//! with [`temps_cloud_client::CloudLink::pseudonymize_telemetry_id`], the same
//! function that produced it on the way out. There is deliberately no second
//! derivation of that value.
//!
//! # Read-only, and bounded
//!
//! The proxy rejects anything that is not a read with `400`, and every call
//! here goes through `within_query_budget` because the `clickhouse` client
//! carries no wall-clock timeout of its own. A slow Cloud must never become the
//! instance's latency.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use temps_cloud_client::CloudLink;

use crate::error::{OtelError, StorageErrorKind};
use crate::storage::cloud_routed::CloudSpanSource;
use crate::storage::StorageResult;
use crate::types::{
    GenAiSpanDetail, GenAiTraceSummary, ResourceInfo, SpanKind, SpanRecord, SpanStats,
    SpanStatsQuery, SpanStatusCode, TraceQuery, TraceSummary,
};

/// Cloud-side table holding mirrored spans.
///
/// Must match what Temps Cloud actually names it. See the module docs.
pub const CLOUD_SPANS_TABLE: &str = "telemetry_spans";

/// Hard ceiling on rows returned by one Cloud span query.
///
/// Cloud bounds its own query cost server-side, but nothing bounds what this
/// side *buffers* out of a successful response. A page the console asked 50
/// rows for must not be able to materialise a million on a 4 GB box because a
/// filter was dropped somewhere.
const MAX_ROWS: u64 = 5_000;

#[derive(Debug, clickhouse::Row, serde::Deserialize)]
struct CloudGenAiSummaryRow {
    trace_id: String,
    root_span_name: String,
    service_name: String,
    gen_ai_system: String,
    gen_ai_model: String,
    gen_ai_operation: String,
    start_time_ms: i64,
    max_duration_ms: f64,
    span_count: u64,
    error_count: u64,
    total_input_tokens: i64,
    total_output_tokens: i64,
    total_cache_creation_input_tokens: i64,
    total_cache_read_input_tokens: i64,
}

fn genai_filters(query: &TraceQuery) -> String {
    let mut clauses = vec![
        "project_ref = ?".to_string(),
        "ts >= fromUnixTimestamp64Milli(?)".into(),
        "ts <= fromUnixTimestamp64Milli(?)".into(),
    ];
    if query.service_name.is_some() {
        clauses.push("spans.service_name = ?".into());
    }
    clauses.join(" AND ")
}

fn genai_having(query: &TraceQuery) -> String {
    let mut clauses = vec!["countIf(mapContains(attributes, 'gen_ai.provider.name') OR mapContains(attributes, 'gen_ai.system')) > 0".to_string()];
    if query
        .attributes
        .as_ref()
        .and_then(|a| a.get("gen_ai.system"))
        .is_some()
    {
        clauses.push("countIf(coalesce(nullIf(attributes['gen_ai.provider.name'], ''), attributes['gen_ai.system']) = ?) > 0".into());
    }
    if query
        .attributes
        .as_ref()
        .and_then(|a| a.get("gen_ai.request.model"))
        .is_some()
    {
        clauses.push("countIf(attributes['gen_ai.request.model'] = ?) > 0".into());
    }
    clauses.join(" AND ")
}

fn bind_genai(
    mut q: clickhouse::query::Query,
    project_ref: &str,
    query: &TraceQuery,
) -> StorageResult<clickhouse::query::Query> {
    let start = query.start_time.ok_or_else(|| OtelError::Validation {
        message: "Cloud GenAI query requires start_time".into(),
    })?;
    let end = query.end_time.ok_or_else(|| OtelError::Validation {
        message: "Cloud GenAI query requires end_time".into(),
    })?;
    q = q
        .bind(project_ref)
        .bind(start.timestamp_millis())
        .bind(end.timestamp_millis());
    if let Some(service) = &query.service_name {
        q = q.bind(service.clone());
    }
    if let Some(attrs) = &query.attributes {
        if let Some(provider) = attrs.get("gen_ai.system") {
            q = q.bind(provider.clone());
        }
        if let Some(model) = attrs.get("gen_ai.request.model") {
            q = q.bind(model.clone());
        }
    }
    Ok(q)
}

fn genai_summary_sql(query: &TraceQuery, count: bool) -> String {
    let select = if count {
        "SELECT count() FROM (SELECT trace_id".to_string()
    } else {
        "SELECT trace_id, argMax(name, if(parent_span_id = '', 1e15, duration_ms)) AS root_span_name, \
         argMax(service_name, if(parent_span_id = '', 1e15, duration_ms)) AS service_name, \
         argMaxIf(coalesce(nullIf(attributes['gen_ai.provider.name'], ''), attributes['gen_ai.system']), ts, mapContains(attributes, 'gen_ai.provider.name') OR mapContains(attributes, 'gen_ai.system')) AS gen_ai_system, \
         argMaxIf(attributes['gen_ai.request.model'], ts, mapContains(attributes, 'gen_ai.request.model')) AS gen_ai_model, \
         argMaxIf(attributes['gen_ai.operation.name'], ts, mapContains(attributes, 'gen_ai.operation.name')) AS gen_ai_operation, \
         toUnixTimestamp64Milli(min(ts)) AS start_time_ms, max(duration_ms) AS max_duration_ms, \
         count() AS span_count, countIf(status_code = 'ERROR' OR status_code = 'error') AS error_count, \
         sum(toInt64OrZero(coalesce(nullIf(attributes['gen_ai.usage.input_tokens'], ''), attributes['gen_ai.usage.prompt_tokens']))) AS total_input_tokens, \
         sum(toInt64OrZero(coalesce(nullIf(attributes['gen_ai.usage.output_tokens'], ''), attributes['gen_ai.usage.completion_tokens']))) AS total_output_tokens, \
         sum(toInt64OrZero(attributes['gen_ai.usage.cache_creation.input_tokens'])) AS total_cache_creation_input_tokens, \
         sum(toInt64OrZero(attributes['gen_ai.usage.cache_read.input_tokens'])) AS total_cache_read_input_tokens".into()
    };
    let tail = if count {
        ")".to_string()
    } else {
        format!(
            " ORDER BY start_time_ms DESC LIMIT {} OFFSET {}",
            query.limit.unwrap_or(50).clamp(1, 100),
            query.offset.unwrap_or(0)
        )
    };
    format!(
        "{select} FROM {CLOUD_SPANS_TABLE} AS spans WHERE {} GROUP BY trace_id HAVING {}{tail}",
        genai_filters(query),
        genai_having(query)
    )
}

/// Reads spans back from Temps Cloud.
pub struct CloudTelemetrySpanSource {
    link: Arc<CloudLink>,
}

impl CloudTelemetrySpanSource {
    pub fn new(link: Arc<CloudLink>) -> Self {
        Self { link }
    }

    /// The pseudonym Cloud knows this project by.
    fn project_ref(&self, project_id: i32) -> StorageResult<String> {
        self.link
            .pseudonymize_telemetry_id("project", &project_id.to_string())
            .map_err(|error| OtelError::Storage {
                message: format!(
                    "Could not derive the Temps Cloud scoping key for project {project_id}: \
                     {error}. Cloud-held telemetry for this project cannot be read until the \
                     link is healthy again."
                ),
                kind: StorageErrorKind::Precondition,
            })
    }

    fn client(&self) -> StorageResult<clickhouse::Client> {
        self.link
            .clickhouse_query_client()
            .map_err(|error| OtelError::Storage {
                message: format!("Temps Cloud telemetry read is unavailable: {error}"),
                kind: StorageErrorKind::Precondition,
            })
    }

    /// Run a Cloud read under the shared wall-clock budget, flattening the two
    /// failure layers into one storage error.
    ///
    /// Deliberately does **not** fall back to the local store on failure. For a
    /// Cloud-primary project the local store has no post-cutover spans, so a
    /// fallback would answer an empty `200` that is indistinguishable from
    /// "nothing happened" — the exact thing ADR-040 §3's no-silent-fallback
    /// contract forbids.
    async fn run<T, F>(&self, what: &str, query: F) -> StorageResult<T>
    where
        F: std::future::Future<Output = Result<T, clickhouse::error::Error>>,
    {
        match temps_cloud_client::query::within_query_budget(query).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(OtelError::Storage {
                message: format!("Temps Cloud rejected the {what} query: {error}"),
                kind: StorageErrorKind::ClickHouseOther,
            }),
            Err(error) => Err(OtelError::Storage {
                message: format!("Temps Cloud did not answer the {what} query in time: {error}"),
                kind: StorageErrorKind::ClickHouseTimeout,
            }),
        }
    }
}

/// One row as Cloud stores it.
///
/// Field for field with the wire `SpanRecord`, except `ts`: Cloud stores the
/// wire's `ts_millis` integer as an actual `DateTime64(3)` column named `ts`
/// (see the module docs), so this decodes it the same way
/// `temps-app-ingest`'s `SpanRow` encodes it.
#[derive(Debug, clickhouse::Row, serde::Deserialize)]
struct CloudSpanRow {
    trace_id: String,
    span_id: String,
    name: String,
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    ts: DateTime<Utc>,
    duration_ms: f64,
    service_name: String,
    span_kind: String,
    status_code: String,
    parent_span_id: String,
    environment: String,
    attributes: BTreeMap<String, String>,
}

impl CloudSpanRow {
    /// Rehydrate into the local span shape the console already renders.
    ///
    /// Fields Cloud does not carry are left at their empty values rather than
    /// invented. `project_id` is supplied by the caller, which is the only side
    /// that knows it — Cloud only ever saw a pseudonym.
    fn into_span(self, project_id: i32) -> SpanRecord {
        let start_time = self.ts;
        SpanRecord {
            project_id,
            trace_id: self.trace_id,
            span_id: self.span_id,
            parent_span_id: (!self.parent_span_id.is_empty()).then_some(self.parent_span_id),
            name: self.name,
            kind: parse_kind(&self.span_kind),
            start_time,
            end_time: start_time
                + chrono::Duration::microseconds((self.duration_ms * 1_000.0) as i64),
            duration_ms: self.duration_ms,
            status_code: parse_status(&self.status_code),
            status_message: String::new(),
            // Cloud stores only attributes consented to by the project's
            // projection. Keep that subset for GenAI detail and filters.
            attributes: self.attributes,
            events: Vec::new(),
            resource: ResourceInfo {
                service_name: self.service_name,
                deployment_environment: (!self.environment.is_empty()).then_some(self.environment),
                ..Default::default()
            },
            deployment_id: None,
        }
    }
}

fn parse_kind(value: &str) -> SpanKind {
    match value.to_ascii_lowercase().as_str() {
        "server" => SpanKind::Server,
        "client" => SpanKind::Client,
        "producer" => SpanKind::Producer,
        "consumer" => SpanKind::Consumer,
        _ => SpanKind::Internal,
    }
}

fn parse_status(value: &str) -> SpanStatusCode {
    match value.to_ascii_lowercase().as_str() {
        "ok" => SpanStatusCode::Ok,
        "error" => SpanStatusCode::Error,
        _ => SpanStatusCode::Unset,
    }
}

/// Build the shared `WHERE` clause for a trace query.
///
/// Every value is bound with `?`, never interpolated: a span name filter is
/// user input, and the read proxy forwards the statement verbatim.
fn trace_filters(query: &TraceQuery) -> String {
    let mut clauses = vec!["project_ref = ?".to_string()];
    if query.trace_id.is_some() {
        clauses.push("trace_id = ?".into());
    }
    if query.service_name.is_some() {
        clauses.push("service_name = ?".into());
    }
    if query.status.is_some() {
        clauses.push("status_code = ?".into());
    }
    if query.min_duration_ms.is_some() {
        clauses.push("duration_ms >= ?".into());
    }
    // Compare the DateTime64 column directly so ClickHouse can prune parts.
    if query.start_time.is_some() {
        clauses.push("ts >= fromUnixTimestamp64Milli(?)".into());
    }
    if query.end_time.is_some() {
        clauses.push("ts <= fromUnixTimestamp64Milli(?)".into());
    }
    if query.root_only {
        clauses.push("parent_span_id = ''".into());
    }
    clauses.join(" AND ")
}

/// Bind the values `trace_filters` left placeholders for, in the same order.
fn bind_trace_filters(
    mut cursor: clickhouse::query::Query,
    project_ref: &str,
    query: &TraceQuery,
) -> clickhouse::query::Query {
    cursor = cursor.bind(project_ref);
    if let Some(trace_id) = &query.trace_id {
        cursor = cursor.bind(trace_id.clone());
    }
    if let Some(service_name) = &query.service_name {
        cursor = cursor.bind(service_name.clone());
    }
    if let Some(status) = query.status {
        cursor = cursor.bind(status.to_string());
    }
    if let Some(min_duration) = query.min_duration_ms {
        cursor = cursor.bind(min_duration);
    }
    if let Some(start) = query.start_time {
        cursor = cursor.bind(start.timestamp_millis());
    }
    if let Some(end) = query.end_time {
        cursor = cursor.bind(end.timestamp_millis());
    }
    cursor
}

fn bounded_limit(limit: Option<u64>) -> u64 {
    limit.unwrap_or(100).clamp(1, MAX_ROWS)
}

#[async_trait]
impl CloudSpanSource for CloudTelemetrySpanSource {
    async fn global_lifetime_candidate_count(
        &self,
        query: super::global_traces::GlobalTraceQuery,
    ) -> StorageResult<Option<u64>> {
        let refs = query
            .scopes
            .iter()
            .map(|scope| Ok((scope.project_id, self.project_ref(scope.project_id)?)))
            .collect::<StorageResult<BTreeMap<_, _>>>()?;
        Ok(Some(
            super::global_traces::cloud_lifetime_candidate_count(&self.client()?, &query, &refs)
                .await?,
        ))
    }

    async fn global_trace_stream(
        &self,
        query: super::global_traces::GlobalTraceQuery,
    ) -> StorageResult<super::global_traces::GlobalTraceStream> {
        let refs = query
            .scopes
            .iter()
            .map(|s| Ok((s.project_id, self.project_ref(s.project_id)?)))
            .collect::<StorageResult<BTreeMap<_, _>>>()?;
        super::global_traces::clickhouse(&self.client()?, &query, Some(&refs)).await
    }
    async fn query_spans(&self, query: TraceQuery) -> StorageResult<Vec<SpanRecord>> {
        let project_ref = self.project_ref(query.project_id)?;
        let client = self.client()?;
        let sql = format!(
            "SELECT trace_id, span_id, name, ts, duration_ms, service_name, span_kind, \
                    status_code, parent_span_id, environment, attributes \
             FROM {CLOUD_SPANS_TABLE} WHERE {} ORDER BY ts DESC LIMIT {} OFFSET {}",
            trace_filters(&query),
            bounded_limit(query.limit),
            query.offset.unwrap_or(0),
        );
        let cursor = bind_trace_filters(client.query(&sql), &project_ref, &query);
        let rows = self.run("span", cursor.fetch_all::<CloudSpanRow>()).await?;
        Ok(rows
            .into_iter()
            .map(|row| row.into_span(query.project_id))
            .collect())
    }

    async fn query_trace_summaries(&self, query: TraceQuery) -> StorageResult<Vec<TraceSummary>> {
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let scope = super::global_traces::TraceReadScope {
                project_id: query.project_id,
                from: query.start_time.unwrap_or(DateTime::<Utc>::UNIX_EPOCH),
                to: query.end_time.unwrap_or_else(Utc::now),
                cloud: true,
                window_clamped_at: None,
            };
            let q = super::global_traces::GlobalTraceQuery {
                source_offset: query.offset.unwrap_or(0),
                filter: query,
                scopes: vec![scope],
                summaries: true,
                use_preaggregated_summaries: false,
                lifetime_candidate_total: None,
            };
            let stream = self.global_trace_stream(q.clone()).await?;
            super::global_traces::merge(vec![stream], &q)
                .await?
                .data
                .into_iter()
                .map(|r| r.summary(String::new(), String::new()).map(|s| s.trace))
                .collect()
        })
        .await
        .map_err(|_| OtelError::Storage {
            message: "Temps Cloud trace summary query exceeded its time budget".into(),
            kind: StorageErrorKind::ClickHouseTimeout,
        })?
    }

    async fn count_traces(&self, query: TraceQuery) -> StorageResult<u64> {
        let project_ref = self.project_ref(query.project_id)?;
        let client = self.client()?;
        let sql = format!(
            "SELECT uniqExact(trace_id) FROM {CLOUD_SPANS_TABLE} WHERE {}",
            trace_filters(&query)
        );
        let cursor = bind_trace_filters(client.query(&sql), &project_ref, &query);
        self.run("trace count", cursor.fetch_one::<u64>()).await
    }

    async fn has_traces(&self, project_id: i32) -> StorageResult<bool> {
        let project_ref = self.project_ref(project_id)?;
        let client = self.client()?;
        let sql = format!(
            "SELECT count() FROM (SELECT 1 FROM {CLOUD_SPANS_TABLE} WHERE project_ref = ? LIMIT 1)"
        );
        let count = self
            .run(
                "existence",
                client.query(&sql).bind(project_ref).fetch_one::<u64>(),
            )
            .await?;
        Ok(count > 0)
    }

    async fn get_trace(&self, project_id: i32, trace_id: &str) -> StorageResult<Vec<SpanRecord>> {
        let end = Utc::now();
        self.get_trace_in_window(TraceQuery {
            project_id,
            trace_id: Some(trace_id.into()),
            start_time: Some(end - chrono::Duration::hours(24)),
            end_time: Some(end),
            limit: Some(MAX_ROWS),
            ..Default::default()
        })
        .await
    }

    async fn get_trace_in_window(&self, query: TraceQuery) -> StorageResult<Vec<SpanRecord>> {
        self.query_spans(query).await
    }

    async fn query_genai_trace_summaries(
        &self,
        query: TraceQuery,
    ) -> StorageResult<Vec<GenAiTraceSummary>> {
        let project_ref = self.project_ref(query.project_id)?;
        let client = self.client()?;
        let sql = genai_summary_sql(&query, false);
        let cursor = bind_genai(client.query(&sql), &project_ref, &query)?;
        let rows = self
            .run(
                "GenAI trace summaries",
                cursor.fetch_all::<CloudGenAiSummaryRow>(),
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| GenAiTraceSummary {
                trace_id: row.trace_id,
                root_span_name: row.root_span_name,
                service_name: row.service_name,
                gen_ai_system: (!row.gen_ai_system.is_empty()).then_some(row.gen_ai_system),
                gen_ai_model: (!row.gen_ai_model.is_empty()).then_some(row.gen_ai_model),
                gen_ai_operation: (!row.gen_ai_operation.is_empty())
                    .then_some(row.gen_ai_operation),
                start_time: DateTime::from_timestamp_millis(row.start_time_ms).unwrap_or_default(),
                duration_ms: row.max_duration_ms,
                span_count: row.span_count as i64,
                error_count: row.error_count as i64,
                total_input_tokens: (row.total_input_tokens != 0).then_some(row.total_input_tokens),
                total_output_tokens: (row.total_output_tokens != 0)
                    .then_some(row.total_output_tokens),
                total_cache_creation_input_tokens: (row.total_cache_creation_input_tokens != 0)
                    .then_some(row.total_cache_creation_input_tokens),
                total_cache_read_input_tokens: (row.total_cache_read_input_tokens != 0)
                    .then_some(row.total_cache_read_input_tokens),
            })
            .collect())
    }

    async fn count_genai_traces(&self, query: TraceQuery) -> StorageResult<u64> {
        let project_ref = self.project_ref(query.project_id)?;
        let client = self.client()?;
        let sql = genai_summary_sql(&query, true);
        let cursor = bind_genai(client.query(&sql), &project_ref, &query)?;
        self.run("GenAI trace count", cursor.fetch_one::<u64>())
            .await
    }

    async fn get_genai_trace_spans_in_window(
        &self,
        query: TraceQuery,
    ) -> StorageResult<Vec<GenAiSpanDetail>> {
        let spans = self.query_spans(query).await?;
        Ok(spans
            .into_iter()
            .map(|span| {
                GenAiSpanDetail::from_span_attrs(
                    span.span_id,
                    span.parent_span_id,
                    span.name,
                    span.kind,
                    span.start_time,
                    span.duration_ms,
                    span.status_code,
                    span.attributes,
                )
            })
            .collect())
    }

    async fn query_span_stats(&self, query: SpanStatsQuery) -> StorageResult<Vec<SpanStats>> {
        // Deliberately not implemented against Cloud in this phase. Answering
        // an empty list would tell the operator their Cloud-primary services
        // have no slow operations, which is a claim, not an absence — and a
        // wrong one. ADR-040 §5 keeps span stats in scope for the read path;
        // until the Cloud-side aggregate contract exists, this says so.
        Err(OtelError::Validation {
            message: format!(
                "Operation latency statistics are not yet available for Cloud-primary projects \
                 ({} project(s) requested). Their spans are stored in Temps Cloud, which does \
                 not expose the per-operation aggregate this report needs. Set a project's \
                 telemetry write mode back to `local` to use this report for it.",
                query.project_ids.len()
            ),
        })
    }

    async fn count_span_stats(&self, query: SpanStatsQuery) -> StorageResult<u64> {
        // Same reasoning as `query_span_stats`: a `0` here would render as "no
        // operations", which is a different and false statement.
        self.query_span_stats(query).await.map(|_| 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_row_limit_is_bounded_however_the_caller_asks() {
        assert_eq!(bounded_limit(None), 100);
        assert_eq!(bounded_limit(Some(0)), 1, "zero must not mean 'no rows'");
        assert_eq!(bounded_limit(Some(u64::MAX)), MAX_ROWS);
        assert_eq!(bounded_limit(Some(50)), 50);
    }

    #[test]
    fn every_filter_contributes_exactly_one_placeholder() {
        // The bind order in `bind_trace_filters` must match the placeholder
        // order in `trace_filters`; a mismatch would silently filter on the
        // wrong column, which is a correctness bug no test of either half alone
        // would catch.
        let query = TraceQuery {
            project_id: 7,
            trace_id: Some("t".into()),
            service_name: Some("s".into()),
            status: Some(SpanStatusCode::Error),
            min_duration_ms: Some(5.0),
            start_time: Some(Utc::now()),
            end_time: Some(Utc::now()),
            ..Default::default()
        };
        let clause = trace_filters(&query);
        assert_eq!(
            clause.matches('?').count(),
            7,
            "project_ref plus six filters: {clause}"
        );
    }

    #[test]
    fn an_unfiltered_query_still_scopes_to_the_project() {
        // The one clause that must never be optional. Without it a query would
        // read every project in the tenant.
        let clause = trace_filters(&TraceQuery::default());
        assert_eq!(clause, "project_ref = ?");
    }

    #[test]
    fn root_only_filters_on_the_empty_parent_sentinel_without_a_placeholder() {
        let clause = trace_filters(&TraceQuery {
            root_only: true,
            ..Default::default()
        });
        assert!(clause.contains("parent_span_id = ''"));
        assert_eq!(clause.matches('?').count(), 1);
    }

    #[test]
    fn span_kind_and_status_fall_back_rather_than_failing_on_an_unknown_value() {
        // Cloud may hold a value written by a newer instance. An unparsable
        // kind must not drop the span from the trace tree.
        assert_eq!(parse_kind("SERVER"), SpanKind::Server);
        assert_eq!(parse_kind("something-new"), SpanKind::Internal);
        assert_eq!(parse_status("ERROR"), SpanStatusCode::Error);
        assert_eq!(parse_status("something-new"), SpanStatusCode::Unset);
    }

    #[test]
    fn a_row_rehydrates_with_the_local_project_id_cloud_never_saw() {
        let row = CloudSpanRow {
            trace_id: "t".into(),
            span_id: "s".into(),
            name: "GET /".into(),
            ts: DateTime::from_timestamp_millis(1_700_000_000_000).expect("valid millis"),
            duration_ms: 12.5,
            service_name: "api".into(),
            span_kind: "server".into(),
            status_code: "ok".into(),
            parent_span_id: String::new(),
            environment: "production".into(),
            attributes: BTreeMap::from([(
                "gen_ai.provider.name".into(),
                "synthetic-provider".into(),
            )]),
        };
        let span = row.into_span(42);

        assert_eq!(span.project_id, 42);
        assert_eq!(span.resource.service_name, "api");
        assert_eq!(
            span.resource.deployment_environment.as_deref(),
            Some("production")
        );
        assert!(
            span.parent_span_id.is_none(),
            "the empty-string sentinel is a root, not a parent named ''"
        );
        assert_eq!(
            span.attributes
                .get("gen_ai.provider.name")
                .map(String::as_str),
            Some("synthetic-provider")
        );
    }

    #[test]
    fn cloud_time_filter_compares_datetime64_column_directly() {
        let query = TraceQuery {
            start_time: DateTime::from_timestamp_millis(1_700_000_000_000),
            end_time: DateTime::from_timestamp_millis(1_700_000_060_000),
            ..Default::default()
        };
        let sql = trace_filters(&query);
        assert!(sql.contains("ts >= fromUnixTimestamp64Milli(?)"));
        assert!(sql.contains("ts <= fromUnixTimestamp64Milli(?)"));
        assert!(!sql.contains("toUnixTimestamp64Milli(ts)"));
    }

    #[test]
    fn genai_summary_pages_traces_and_filters_provider_model() {
        let query = TraceQuery {
            start_time: DateTime::from_timestamp_millis(1_700_000_000_000),
            end_time: DateTime::from_timestamp_millis(1_700_000_060_000),
            attributes: Some(BTreeMap::from([
                ("gen_ai.system".into(), "synthetic-provider".into()),
                ("gen_ai.request.model".into(), "synthetic-model".into()),
            ])),
            limit: Some(20),
            offset: Some(40),
            ..Default::default()
        };
        let sql = genai_summary_sql(&query, false);
        assert!(sql.contains("GROUP BY trace_id HAVING"));
        assert!(sql.contains("countIf(coalesce(nullIf(attributes['gen_ai.provider.name']"));
        assert!(sql.contains("countIf(attributes['gen_ai.request.model'] = ?)"));
        assert!(sql.ends_with("LIMIT 20 OFFSET 40"));
        assert!(genai_summary_sql(&query, true).starts_with("SELECT count() FROM (SELECT trace_id"));
    }

    #[tokio::test]
    async fn clickhouse_cloud_genai_queries_decode_consent_and_page_traces() {
        use testcontainers::{
            core::{wait::HttpWaitStrategy, ContainerPort, WaitFor},
            runners::AsyncRunner,
            GenericImage, ImageExt,
        };

        let image = GenericImage::new("clickhouse/clickhouse-server", "24.8")
            .with_exposed_port(ContainerPort::Tcp(8123))
            .with_wait_for(WaitFor::http(
                HttpWaitStrategy::new("/ping")
                    .with_port(ContainerPort::Tcp(8123))
                    .with_expected_status_code(200u16),
            ))
            .with_env_var("CLICKHOUSE_PASSWORD", "test");
        let container = match image.start().await {
            Ok(container) => container,
            Err(error) => {
                eprintln!("Skipping Cloud GenAI ClickHouse test: Docker unavailable ({error})");
                return;
            }
        };
        let port = container
            .get_host_port_ipv4(8123)
            .await
            .expect("mapped HTTP port");
        let client = clickhouse::Client::default()
            .with_url(format!("http://127.0.0.1:{port}"))
            .with_user("default")
            .with_password("test");
        client.query("CREATE TABLE telemetry_spans (project_ref String, trace_id String, span_id String, name String, ts DateTime64(3), duration_ms Float64, service_name String, span_kind String, status_code String, parent_span_id String, environment String, attributes Map(LowCardinality(String), String)) ENGINE = MergeTree ORDER BY (project_ref, ts, trace_id)")
            .execute().await.expect("create Cloud-compatible spans table");
        let insert = "INSERT INTO telemetry_spans VALUES \
            ('synthetic-ref','trace-a','span-a','chat',fromUnixTimestamp64Milli(1700000000000),12,'api','client','ok','','dev',map('gen_ai.provider.name','synthetic-provider','gen_ai.request.model','synthetic-model','gen_ai.usage.input_tokens','11')), \
            ('synthetic-ref','trace-a','span-b','tool',fromUnixTimestamp64Milli(1700000001000),3,'api','internal','ok','span-a','dev',map('gen_ai.usage.output_tokens','7')), \
            ('synthetic-ref','trace-b','span-c','chat',fromUnixTimestamp64Milli(1700000002000),8,'api','client','error','','dev',map('gen_ai.system','synthetic-provider','gen_ai.request.model','synthetic-model','gen_ai.usage.prompt_tokens','5')), \
            ('other-ref','trace-c','span-d','chat',fromUnixTimestamp64Milli(1700000003000),9,'api','client','ok','','dev',map('gen_ai.provider.name','synthetic-provider'))";
        client
            .query(insert)
            .execute()
            .await
            .expect("insert synthetic Cloud spans");

        let query = TraceQuery {
            project_id: 7,
            service_name: Some("api".into()),
            start_time: DateTime::from_timestamp_millis(1_699_999_999_000),
            end_time: DateTime::from_timestamp_millis(1_700_000_010_000),
            attributes: Some(BTreeMap::from([
                ("gen_ai.system".into(), "synthetic-provider".into()),
                ("gen_ai.request.model".into(), "synthetic-model".into()),
            ])),
            limit: Some(1),
            offset: Some(0),
            ..Default::default()
        };
        let count_sql = genai_summary_sql(&query, true);
        let count = bind_genai(client.query(&count_sql), "synthetic-ref", &query)
            .expect("bounded count query")
            .fetch_one::<u64>()
            .await
            .expect("Cloud count SQL executes");
        assert_eq!(count, 2, "count distinct traces, scoped to project");
        let list_sql = genai_summary_sql(&query, false);
        let page = bind_genai(client.query(&list_sql), "synthetic-ref", &query)
            .expect("bounded list query")
            .fetch_all::<CloudGenAiSummaryRow>()
            .await
            .expect("Cloud GenAI SQL and row decode");
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].trace_id, "trace-b");
        assert_eq!(page[0].total_input_tokens, 5);
        let next = TraceQuery {
            offset: Some(1),
            ..query.clone()
        };
        let page = bind_genai(
            client.query(&genai_summary_sql(&next, false)),
            "synthetic-ref",
            &next,
        )
        .expect("second page query")
        .fetch_all::<CloudGenAiSummaryRow>()
        .await
        .expect("second trace page");
        assert_eq!(page[0].trace_id, "trace-a");
        assert_eq!(page[0].span_count, 2);
        assert_eq!(page[0].total_input_tokens, 11);
        assert_eq!(page[0].total_output_tokens, 7);

        let detail = TraceQuery {
            trace_id: Some("trace-a".into()),
            ..query
        };
        let sql = format!("SELECT trace_id, span_id, name, ts, duration_ms, service_name, span_kind, status_code, parent_span_id, environment, attributes FROM telemetry_spans WHERE {} ORDER BY ts", trace_filters(&detail));
        let rows = bind_trace_filters(client.query(&sql), "synthetic-ref", &detail)
            .fetch_all::<CloudSpanRow>()
            .await
            .expect("Cloud Map attributes decode");
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0]
                .attributes
                .get("gen_ai.provider.name")
                .map(String::as_str),
            Some("synthetic-provider")
        );
    }
}
