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
//! `cloud_span()` at the owning project's consented fidelity. The column names
//! below are that struct's field names, one for one, because the only shape
//! Cloud can be storing is the shape it was sent.
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
use chrono::{TimeZone, Utc};
use temps_cloud_client::CloudLink;

use crate::error::{OtelError, StorageErrorKind};
use crate::storage::cloud_routed::CloudSpanSource;
use crate::storage::StorageResult;
use crate::types::{
    ResourceInfo, SpanKind, SpanRecord, SpanStats, SpanStatsQuery, SpanStatusCode, TraceQuery,
    TraceSummary,
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

/// One row as Cloud stores it — the wire projection, field for field.
#[derive(Debug, clickhouse::Row, serde::Deserialize)]
struct CloudSpanRow {
    trace_id: String,
    span_id: String,
    name: String,
    ts_millis: i64,
    duration_ms: f64,
    service_name: String,
    span_kind: String,
    status_code: String,
    parent_span_id: String,
    environment: String,
}

impl CloudSpanRow {
    /// Rehydrate into the local span shape the console already renders.
    ///
    /// Fields Cloud does not carry are left at their empty values rather than
    /// invented. `project_id` is supplied by the caller, which is the only side
    /// that knows it — Cloud only ever saw a pseudonym.
    fn into_span(self, project_id: i32) -> SpanRecord {
        let start_time = Utc
            .timestamp_millis_opt(self.ts_millis)
            .single()
            .unwrap_or_else(Utc::now);
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
            // The `Queryable` projection ships an allowlisted attribute subset
            // and this reader does not re-request it: rendering a partial
            // attribute map as if it were the whole one is worse than showing
            // none, because a missing key reads as "the app did not set it".
            attributes: BTreeMap::new(),
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
    if query.start_time.is_some() {
        clauses.push("ts_millis >= ?".into());
    }
    if query.end_time.is_some() {
        clauses.push("ts_millis <= ?".into());
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
    async fn query_spans(&self, query: TraceQuery) -> StorageResult<Vec<SpanRecord>> {
        let project_ref = self.project_ref(query.project_id)?;
        let client = self.client()?;
        let sql = format!(
            "SELECT trace_id, span_id, name, ts_millis, duration_ms, service_name, span_kind, \
                    status_code, parent_span_id, environment \
             FROM {CLOUD_SPANS_TABLE} WHERE {} ORDER BY ts_millis DESC LIMIT {} OFFSET {}",
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
        // Summaries are an aggregate over the same rows. Fetching the spans and
        // folding them here keeps one SQL surface against a schema this repo
        // does not own, at the cost of a wider read — bounded by `MAX_ROWS`.
        let spans = self
            .query_spans(TraceQuery {
                limit: Some(bounded_limit(query.limit).saturating_mul(20).min(MAX_ROWS)),
                offset: None,
                root_only: false,
                ..query.clone()
            })
            .await?;
        Ok(summarize(spans, bounded_limit(query.limit) as usize))
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
        self.query_spans(TraceQuery {
            project_id,
            trace_id: Some(trace_id.to_string()),
            limit: Some(MAX_ROWS),
            ..Default::default()
        })
        .await
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

/// Fold spans into one summary per trace.
///
/// Pure, so the aggregation the console renders is testable without Cloud.
fn summarize(spans: Vec<SpanRecord>, limit: usize) -> Vec<TraceSummary> {
    let mut by_trace: BTreeMap<String, Vec<SpanRecord>> = BTreeMap::new();
    for span in spans {
        by_trace
            .entry(span.trace_id.clone())
            .or_default()
            .push(span);
    }

    let mut summaries: Vec<TraceSummary> = by_trace
        .into_iter()
        .filter_map(|(trace_id, spans)| {
            let root = spans
                .iter()
                .find(|span| span.parent_span_id.is_none())
                .or_else(|| spans.first())?;
            let start_time = spans
                .iter()
                .map(|span| span.start_time)
                .min()
                .unwrap_or(root.start_time);
            let duration_ms = spans
                .iter()
                .map(|span| span.duration_ms)
                .fold(0.0_f64, f64::max);
            let error_count = spans
                .iter()
                .filter(|span| span.status_code == SpanStatusCode::Error)
                .count() as i64;
            Some(TraceSummary {
                trace_id,
                root_span_name: root.name.clone(),
                service_name: root.resource.service_name.clone(),
                deployment_environment: root.resource.deployment_environment.clone(),
                kind: root.kind,
                status_code: root.status_code,
                start_time,
                duration_ms,
                span_count: spans.len() as i64,
                error_count,
            })
        })
        .collect();

    // Newest trace first, matching every local backend's default ordering.
    summaries.sort_by_key(|summary| std::cmp::Reverse(summary.start_time));
    summaries.truncate(limit);
    summaries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(trace: &str, id: &str, parent: Option<&str>, status: SpanStatusCode) -> SpanRecord {
        let now = Utc::now();
        SpanRecord {
            project_id: 7,
            deployment_id: None,
            resource: ResourceInfo::default(),
            trace_id: trace.into(),
            span_id: id.into(),
            parent_span_id: parent.map(str::to_string),
            name: format!("op-{id}"),
            kind: SpanKind::Server,
            start_time: now,
            end_time: now,
            duration_ms: 10.0,
            status_code: status,
            status_message: String::new(),
            attributes: BTreeMap::new(),
            events: Vec::new(),
        }
    }

    #[test]
    fn a_trace_summary_counts_its_spans_and_its_errors() {
        let spans = vec![
            span("t1", "a", None, SpanStatusCode::Ok),
            span("t1", "b", Some("a"), SpanStatusCode::Error),
            span("t1", "c", Some("a"), SpanStatusCode::Ok),
        ];
        let summaries = summarize(spans, 10);

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].span_count, 3);
        assert_eq!(summaries[0].error_count, 1);
        assert_eq!(
            summaries[0].root_span_name, "op-a",
            "the parentless span is the root"
        );
    }

    #[test]
    fn a_trace_whose_root_did_not_ship_still_produces_a_summary() {
        // A partial trace is a real thing to render — refusing to summarise it
        // would make the whole trace disappear rather than showing what exists.
        let spans = vec![span("t1", "b", Some("missing"), SpanStatusCode::Ok)];
        let summaries = summarize(spans, 10);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].span_count, 1);
    }

    #[test]
    fn summaries_are_newest_first_and_bounded_by_the_limit() {
        let mut spans = Vec::new();
        for i in 0..10 {
            let mut s = span(&format!("t{i}"), "a", None, SpanStatusCode::Ok);
            s.start_time = Utc::now() - chrono::Duration::minutes(i);
            spans.push(s);
        }
        let summaries = summarize(spans, 3);

        assert_eq!(summaries.len(), 3);
        assert!(summaries[0].start_time >= summaries[1].start_time);
        assert!(summaries[1].start_time >= summaries[2].start_time);
    }

    #[test]
    fn an_empty_result_summarises_to_nothing_rather_than_panicking() {
        assert!(summarize(Vec::new(), 10).is_empty());
    }

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
            ts_millis: 1_700_000_000_000,
            duration_ms: 12.5,
            service_name: "api".into(),
            span_kind: "server".into(),
            status_code: "ok".into(),
            parent_span_id: String::new(),
            environment: "production".into(),
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
        assert!(
            span.attributes.is_empty(),
            "a partial attribute map must not be rendered as a complete one"
        );
    }
}
