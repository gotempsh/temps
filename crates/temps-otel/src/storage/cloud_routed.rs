// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-040 §2's routing decorator, installed where ADR-041 §8 requires it.
//!
//! # The requirement whose omission silently empties four features
//!
//! It is tempting to route Cloud-primary span reads inside the three query
//! handlers that obviously need it — the Traces list, a trace detail, span
//! stats. ADR-041 §8 is explicit that this is wrong, because these read spans
//! through the same `Arc<dyn OtelStorage>` and would each degrade to an empty
//! result with no error and no badge:
//!
//! - `HealthComputeService` (`services/health_service.rs`) — `query_spans`.
//! - `CrossProjectTraceService` (`services/cross_project.rs`, ADR-027) —
//!   `get_trace` per referenced project.
//! - `TraceReader` (`services/trace_reader.rs`) — the storage-agnostic read
//!   contract the AI debugging chat's trace tools use.
//! - `temps-observability` (`crates/temps-observability/src/service.rs`) —
//!   `query_spans` for the unified Observe page.
//!
//! "The AI chat quietly stopped being able to see traces" is exactly the
//! failure mode CLAUDE.md's *build as if the user has no one to ask for help*
//! rule exists to prevent. So this wraps the storage at the plugin's
//! `context.register_service(storage.clone())` call site, and every consumer
//! inherits the routing by construction rather than by remembering.
//!
//! Handler-level routing still attaches the source descriptor to the response;
//! that is a presentation concern layered on top, not the routing seam.
//!
//! # What is routed, and what is not
//!
//! **Span reads** route on `cloud_telemetry_write_mode` (the `spans` signal
//! group). **Metric reads** (ADR-043 §3 Phase C1) route independently, on
//! `cloud_analytics_write_mode` (the `analytics` signal group), and only when
//! a [`CloudMetricSource`] has been installed via
//! [`CloudRoutedOtelStorage::with_cloud_metrics`] — an instance whose Cloud
//! link predates Phase C1 simply never routes metrics, exactly as if the
//! switch had never been raised. Logs, insights, health summaries, quota,
//! retention and the ingest-error report all delegate straight through: Cloud
//! holds no projection of them, so routing would be a silent feature removal.
//!
//! **Writes are never routed.** `store_spans`/`store_metrics` always go to the
//! local store. The ingest path decides whether a signal is written locally at
//! all (ADR-041 §2, ADR-043 §3), and the ones that reach here are either
//! `Local`-mode signals or the disconnect/quota spill — both of which belong
//! on disk here.
//!
//! # No silent fallback
//!
//! When the ledger says a window is Cloud-served and Cloud cannot answer, this
//! returns the error. It does **not** fall back to the local store, because a
//! Cloud-primary project's local store has no post-cutover spans and an empty
//! `200` is indistinguishable from "nothing happened" (ADR-040 §3).

use std::sync::Arc;

use async_trait::async_trait;
use temps_entities::cloud_telemetry_write_mode::CloudTelemetryWriteMode;

use crate::error::OtelError;
use crate::services::telemetry_write_mode::{TelemetryWriteModeService, WindowResolution};
use crate::storage::{
    BaselinePoint, DeployEvent, MinuteAggregate, OtelStorage, StorageResult, TraceRefProject,
};
use crate::types::{
    GenAiEvent, GenAiSpanDetail, GenAiTraceSummary, HealthSummary, IngestErrorSummary, Insight,
    InsightStatus, LogQuery, LogRecord, MetricBucket, MetricPoint, MetricQuery, SpanRecord,
    SpanStats, SpanStatsQuery, StorageQuota, TraceQuery, TraceSummary,
};

/// The Cloud-side half of the read path.
///
/// A trait rather than a concrete client so the routing seam is testable
/// without an enrolled instance and a live Cloud tenant, and so the
/// Cloud-side query implementation can change without touching the four
/// consumers that inherit this decorator.
#[async_trait]
pub trait CloudSpanSource: Send + Sync {
    async fn query_spans(&self, query: TraceQuery) -> StorageResult<Vec<SpanRecord>>;
    async fn query_trace_summaries(&self, query: TraceQuery) -> StorageResult<Vec<TraceSummary>>;
    async fn count_traces(&self, query: TraceQuery) -> StorageResult<u64>;
    async fn has_traces(&self, project_id: i32) -> StorageResult<bool>;
    async fn get_trace(&self, project_id: i32, trace_id: &str) -> StorageResult<Vec<SpanRecord>>;
    async fn query_span_stats(&self, query: SpanStatsQuery) -> StorageResult<Vec<SpanStats>>;
    async fn count_span_stats(&self, query: SpanStatsQuery) -> StorageResult<u64>;
}

/// The Cloud-side half of the metric read path (ADR-043 §3 Phase C1).
///
/// Split from [`CloudSpanSource`] rather than folded into it: metrics route
/// on the `analytics` signal group (`cloud_analytics_write_mode`), spans on
/// the `spans` group (`cloud_telemetry_write_mode`) — two independent
/// ledgers, so two independent Cloud-side contracts, even though both are
/// implemented by the same `clickhouse::Client`-reuse pattern.
#[async_trait]
pub trait CloudMetricSource: Send + Sync {
    async fn query_metrics(&self, query: MetricQuery) -> StorageResult<Vec<MetricBucket>>;
    async fn list_metric_names(&self, project_id: i32) -> StorageResult<Vec<String>>;
    async fn list_metric_label_keys(
        &self,
        project_id: i32,
        metric_name: &str,
        start_time: chrono::DateTime<chrono::Utc>,
        end_time: chrono::DateTime<chrono::Utc>,
    ) -> StorageResult<Vec<String>>;
    async fn list_metric_label_values(
        &self,
        project_id: i32,
        metric_name: &str,
        label_key: &str,
        start_time: chrono::DateTime<chrono::Utc>,
        end_time: chrono::DateTime<chrono::Utc>,
    ) -> StorageResult<Vec<String>>;
    async fn get_metric_baseline(
        &self,
        project_id: i32,
        service_name: &str,
        metric_name: &str,
        environment: Option<&str>,
        lookback_days: i32,
    ) -> StorageResult<Vec<BaselinePoint>>;
    async fn get_recent_minute_aggregates(
        &self,
        project_id: i32,
        service_name: &str,
        metric_name: &str,
        environment: Option<&str>,
        minutes: i32,
    ) -> StorageResult<Vec<MinuteAggregate>>;
}

/// Wraps a local `OtelStorage`, routing span and metric reads for
/// Cloud-primary projects.
pub struct CloudRoutedOtelStorage {
    local: Arc<dyn OtelStorage>,
    cloud: Arc<dyn CloudSpanSource>,
    /// `None` when no Cloud metric source is wired — e.g. an instance whose
    /// Cloud link predates Phase C1, or a test double that only cares about
    /// span routing. Every metric method falls back to local in that case,
    /// exactly as it would if `cloud_analytics_write_mode` had never been
    /// raised to `cloud`.
    cloud_metrics: Option<Arc<dyn CloudMetricSource>>,
    write_modes: Arc<TelemetryWriteModeService>,
}

impl CloudRoutedOtelStorage {
    pub fn new(
        local: Arc<dyn OtelStorage>,
        cloud: Arc<dyn CloudSpanSource>,
        write_modes: Arc<TelemetryWriteModeService>,
    ) -> Self {
        Self {
            local,
            cloud,
            cloud_metrics: None,
            write_modes,
        }
    }

    /// Install the Cloud-side metric read source (ADR-043 §3 Phase C1).
    pub fn with_cloud_metrics(mut self, cloud_metrics: Arc<dyn CloudMetricSource>) -> Self {
        self.cloud_metrics = Some(cloud_metrics);
        self
    }

    /// The undecorated local store.
    ///
    /// Exposed for the one caller that must bypass routing on purpose: the
    /// disconnect/quota spill, which writes previously-Cloud-bound spans back
    /// to disk here.
    pub fn local(&self) -> &Arc<dyn OtelStorage> {
        &self.local
    }

    /// Resolve a window against the project's ledger.
    ///
    /// A ledger read failure resolves to **local**, matching every other
    /// fail-safe in this design: reading the local store for a Cloud-primary
    /// project returns pre-cutover history, which is incomplete but true.
    /// Reading Cloud for a `Local` project would be a confidently empty answer
    /// about a store that never held those spans.
    async fn resolve(
        &self,
        project_id: i32,
        start: Option<chrono::DateTime<chrono::Utc>>,
        end: Option<chrono::DateTime<chrono::Utc>>,
    ) -> WindowResolution {
        // No lower bound means "everything you have", which necessarily
        // straddles every interval this project ever had. Using the earliest
        // representable instant makes that explicit rather than accidentally
        // matching only the open interval.
        let from = start.unwrap_or(chrono::DateTime::<chrono::Utc>::MIN_UTC);
        let to = end.unwrap_or_else(chrono::Utc::now);

        match self
            .write_modes
            .resolve_read_window(project_id, from, to)
            .await
        {
            Ok(resolution) => resolution,
            Err(error) => {
                tracing::warn!(
                    project_id,
                    %error,
                    "Could not resolve the telemetry write-mode ledger; serving this window from \
                     local storage"
                );
                WindowResolution {
                    source: CloudTelemetryWriteMode::Local,
                    window_clamped_at: None,
                    from,
                    to,
                }
            }
        }
    }

    /// Apply a resolution's clamp to a trace query, so a straddling window is
    /// narrowed rather than answered from two stores.
    fn clamped(mut query: TraceQuery, resolution: &WindowResolution) -> TraceQuery {
        if resolution.window_clamped_at.is_some() {
            query.start_time = Some(resolution.from);
        }
        query
    }

    /// Resolve a window against the project's **analytics** ledger
    /// (`signal_group = 'analytics'`) — the independent switch metrics route
    /// on (ADR-043 §3). Same fail-safe-to-local direction as [`Self::resolve`].
    async fn resolve_analytics(
        &self,
        project_id: i32,
        start: Option<chrono::DateTime<chrono::Utc>>,
        end: Option<chrono::DateTime<chrono::Utc>>,
    ) -> WindowResolution {
        let from = start.unwrap_or(chrono::DateTime::<chrono::Utc>::MIN_UTC);
        let to = end.unwrap_or_else(chrono::Utc::now);

        match self
            .write_modes
            .resolve_analytics_read_window(project_id, from, to)
            .await
        {
            Ok(resolution) => resolution,
            Err(error) => {
                tracing::warn!(
                    project_id,
                    %error,
                    "Could not resolve the analytics write-mode ledger; serving this window from \
                     local storage"
                );
                WindowResolution {
                    source: CloudTelemetryWriteMode::Local,
                    window_clamped_at: None,
                    from,
                    to,
                }
            }
        }
    }

    /// Apply an analytics resolution's clamp to a metric query.
    fn clamped_metrics(mut query: MetricQuery, resolution: &WindowResolution) -> MetricQuery {
        if resolution.window_clamped_at.is_some() {
            query.start_time = Some(resolution.from);
        }
        query
    }
}

#[async_trait]
impl OtelStorage for CloudRoutedOtelStorage {
    // ── Writes: never routed ─────────────────────────────────────────────

    async fn store_metrics(&self, points: Vec<MetricPoint>) -> StorageResult<u64> {
        self.local.store_metrics(points).await
    }

    async fn store_spans(&self, spans: Vec<SpanRecord>) -> StorageResult<u64> {
        self.local.store_spans(spans).await
    }

    async fn store_logs(&self, records: Vec<LogRecord>) -> StorageResult<u64> {
        self.local.store_logs(records).await
    }

    async fn archive_logs(&self, records: Vec<LogRecord>) -> StorageResult<u64> {
        self.local.archive_logs(records).await
    }

    async fn record_ingest_error(
        &self,
        signal_type: &str,
        error_class: &str,
        message: &str,
    ) -> StorageResult<()> {
        self.local
            .record_ingest_error(signal_type, error_class, message)
            .await
    }

    async fn recent_ingest_errors(&self, limit: u32) -> StorageResult<Vec<IngestErrorSummary>> {
        self.local.recent_ingest_errors(limit).await
    }

    // ── Metric reads: routed per the analytics ledger (ADR-043 §3) ───────

    async fn query_metrics(&self, query: MetricQuery) -> StorageResult<Vec<MetricBucket>> {
        let Some(cloud_metrics) = self.cloud_metrics.as_ref() else {
            return self.local.query_metrics(query).await;
        };
        let resolution = self
            .resolve_analytics(query.project_id, query.start_time, query.end_time)
            .await;
        match resolution.source {
            CloudTelemetryWriteMode::Local => self.local.query_metrics(query).await,
            CloudTelemetryWriteMode::Cloud => {
                cloud_metrics
                    .query_metrics(Self::clamped_metrics(query, &resolution))
                    .await
            }
        }
    }

    async fn list_metric_names(&self, project_id: i32) -> StorageResult<Vec<String>> {
        let Some(cloud_metrics) = self.cloud_metrics.as_ref() else {
            return self.local.list_metric_names(project_id).await;
        };
        let resolution = self.resolve_analytics(project_id, None, None).await;
        match resolution.source {
            CloudTelemetryWriteMode::Local => self.local.list_metric_names(project_id).await,
            CloudTelemetryWriteMode::Cloud => cloud_metrics.list_metric_names(project_id).await,
        }
    }

    async fn list_metric_label_keys(
        &self,
        project_id: i32,
        metric_name: &str,
        start_time: chrono::DateTime<chrono::Utc>,
        end_time: chrono::DateTime<chrono::Utc>,
    ) -> StorageResult<Vec<String>> {
        let Some(cloud_metrics) = self.cloud_metrics.as_ref() else {
            return self
                .local
                .list_metric_label_keys(project_id, metric_name, start_time, end_time)
                .await;
        };
        let resolution = self
            .resolve_analytics(project_id, Some(start_time), Some(end_time))
            .await;
        match resolution.source {
            CloudTelemetryWriteMode::Local => {
                self.local
                    .list_metric_label_keys(project_id, metric_name, start_time, end_time)
                    .await
            }
            CloudTelemetryWriteMode::Cloud => {
                cloud_metrics
                    .list_metric_label_keys(project_id, metric_name, resolution.from, resolution.to)
                    .await
            }
        }
    }

    async fn list_metric_label_values(
        &self,
        project_id: i32,
        metric_name: &str,
        label_key: &str,
        start_time: chrono::DateTime<chrono::Utc>,
        end_time: chrono::DateTime<chrono::Utc>,
    ) -> StorageResult<Vec<String>> {
        let Some(cloud_metrics) = self.cloud_metrics.as_ref() else {
            return self
                .local
                .list_metric_label_values(project_id, metric_name, label_key, start_time, end_time)
                .await;
        };
        let resolution = self
            .resolve_analytics(project_id, Some(start_time), Some(end_time))
            .await;
        match resolution.source {
            CloudTelemetryWriteMode::Local => {
                self.local
                    .list_metric_label_values(
                        project_id,
                        metric_name,
                        label_key,
                        start_time,
                        end_time,
                    )
                    .await
            }
            CloudTelemetryWriteMode::Cloud => {
                cloud_metrics
                    .list_metric_label_values(
                        project_id,
                        metric_name,
                        label_key,
                        resolution.from,
                        resolution.to,
                    )
                    .await
            }
        }
    }

    async fn query_logs(&self, query: LogQuery) -> StorageResult<Vec<LogRecord>> {
        self.local.query_logs(query).await
    }

    // ── Span reads: routed ───────────────────────────────────────────────

    async fn query_spans(&self, query: TraceQuery) -> StorageResult<Vec<SpanRecord>> {
        let resolution = self
            .resolve(query.project_id, query.start_time, query.end_time)
            .await;
        match resolution.source {
            CloudTelemetryWriteMode::Local => self.local.query_spans(query).await,
            CloudTelemetryWriteMode::Cloud => {
                self.cloud
                    .query_spans(Self::clamped(query, &resolution))
                    .await
            }
        }
    }

    async fn query_trace_summaries(&self, query: TraceQuery) -> StorageResult<Vec<TraceSummary>> {
        let resolution = self
            .resolve(query.project_id, query.start_time, query.end_time)
            .await;
        match resolution.source {
            CloudTelemetryWriteMode::Local => self.local.query_trace_summaries(query).await,
            CloudTelemetryWriteMode::Cloud => {
                self.cloud
                    .query_trace_summaries(Self::clamped(query, &resolution))
                    .await
            }
        }
    }

    async fn count_traces(&self, query: TraceQuery) -> StorageResult<u64> {
        let resolution = self
            .resolve(query.project_id, query.start_time, query.end_time)
            .await;
        match resolution.source {
            CloudTelemetryWriteMode::Local => self.local.count_traces(query).await,
            CloudTelemetryWriteMode::Cloud => {
                self.cloud
                    .count_traces(Self::clamped(query, &resolution))
                    .await
            }
        }
    }

    async fn has_traces(&self, project_id: i32) -> StorageResult<bool> {
        // Onboarding's "have you sent anything yet". For a Cloud-primary
        // project the answer lives in Cloud, and answering `false` from an
        // empty local table would tell a user who *is* sending spans that their
        // instrumentation is broken.
        let resolution = self.resolve(project_id, None, None).await;
        match resolution.source {
            CloudTelemetryWriteMode::Local => self.local.has_traces(project_id).await,
            CloudTelemetryWriteMode::Cloud => self.cloud.has_traces(project_id).await,
        }
    }

    async fn get_trace(&self, project_id: i32, trace_id: &str) -> StorageResult<Vec<SpanRecord>> {
        // A trace has no requested window; it resolves against the open
        // interval, which is where a trace anyone is currently looking at was
        // written. Cross-project trace linking (ADR-027) reaches this method
        // once per referenced project, so each project's segment resolves
        // against its own ledger — which is exactly the v1 behaviour ADR-041's
        // consequences section describes.
        let resolution = self.resolve(project_id, None, None).await;
        match resolution.source {
            CloudTelemetryWriteMode::Local => self.local.get_trace(project_id, trace_id).await,
            CloudTelemetryWriteMode::Cloud => self.cloud.get_trace(project_id, trace_id).await,
        }
    }

    async fn query_span_stats(&self, query: SpanStatsQuery) -> StorageResult<Vec<SpanStats>> {
        match self.resolve_span_stats(&query).await? {
            CloudTelemetryWriteMode::Local => self.local.query_span_stats(query).await,
            CloudTelemetryWriteMode::Cloud => self.cloud.query_span_stats(query).await,
        }
    }

    async fn count_span_stats(&self, query: SpanStatsQuery) -> StorageResult<u64> {
        match self.resolve_span_stats(&query).await? {
            CloudTelemetryWriteMode::Local => self.local.count_span_stats(query).await,
            CloudTelemetryWriteMode::Cloud => self.cloud.count_span_stats(query).await,
        }
    }

    // ── Cross-project trace refs ─────────────────────────────────────────

    async fn record_trace_refs(&self, trace_ids: &[String], project_id: i32) -> StorageResult<u64> {
        // The reverse index is local control-plane state, not span data, and it
        // is what makes a Cloud-primary project *discoverable* from another
        // project's trace. Routing it to Cloud would remove cross-project
        // linking for exactly the projects that need it most.
        self.local.record_trace_refs(trace_ids, project_id).await
    }

    async fn get_trace_ref_projects(&self, trace_id: &str) -> StorageResult<Vec<TraceRefProject>> {
        self.local.get_trace_ref_projects(trace_id).await
    }

    // ── GenAI: local only ────────────────────────────────────────────────
    //
    // The `Queryable` projection ships an allowlisted attribute subset, and
    // `gen_ai.*` attributes are not on any default allowlist. Routing these to
    // Cloud would answer a confident empty rather than "this view needs local
    // spans", so they stay local and the emptiness is at least honest about
    // which store it came from.

    async fn query_genai_trace_summaries(
        &self,
        query: TraceQuery,
    ) -> StorageResult<Vec<GenAiTraceSummary>> {
        self.local.query_genai_trace_summaries(query).await
    }

    async fn get_genai_trace_spans(
        &self,
        project_id: i32,
        trace_id: &str,
    ) -> StorageResult<Vec<GenAiSpanDetail>> {
        self.local.get_genai_trace_spans(project_id, trace_id).await
    }

    async fn count_genai_traces(&self, query: TraceQuery) -> StorageResult<u64> {
        self.local.count_genai_traces(query).await
    }

    async fn get_genai_trace_events(
        &self,
        project_id: i32,
        trace_id: &str,
    ) -> StorageResult<Vec<GenAiEvent>> {
        self.local
            .get_genai_trace_events(project_id, trace_id)
            .await
    }

    // ── Everything else delegates ────────────────────────────────────────

    async fn upsert_insight(&self, insight: &Insight) -> StorageResult<i64> {
        self.local.upsert_insight(insight).await
    }

    async fn list_insights(
        &self,
        project_id: i32,
        status: Option<InsightStatus>,
        limit: u64,
        offset: u64,
    ) -> StorageResult<Vec<Insight>> {
        self.local
            .list_insights(project_id, status, limit, offset)
            .await
    }

    async fn resolve_insight(&self, insight_id: i64) -> StorageResult<()> {
        self.local.resolve_insight(insight_id).await
    }

    async fn store_health_summary(&self, summary: &HealthSummary) -> StorageResult<()> {
        self.local.store_health_summary(summary).await
    }

    async fn get_health_summaries(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> StorageResult<Vec<HealthSummary>> {
        self.local
            .get_health_summaries(project_id, environment_id)
            .await
    }

    async fn get_storage_quota(&self, project_id: i32) -> StorageResult<StorageQuota> {
        self.local.get_storage_quota(project_id).await
    }

    async fn check_quota(&self, project_id: i32) -> StorageResult<bool> {
        self.local.check_quota(project_id).await
    }

    async fn get_metric_baseline(
        &self,
        project_id: i32,
        service_name: &str,
        metric_name: &str,
        environment: Option<&str>,
        lookback_days: i32,
    ) -> StorageResult<Vec<BaselinePoint>> {
        let Some(cloud_metrics) = self.cloud_metrics.as_ref() else {
            return self
                .local
                .get_metric_baseline(
                    project_id,
                    service_name,
                    metric_name,
                    environment,
                    lookback_days,
                )
                .await;
        };
        // Baselines look back `lookback_days`, always ending now — there is no
        // caller-supplied window to clamp, so resolve against the open
        // interval, same as `get_trace`'s reasoning for spans.
        let resolution = self.resolve_analytics(project_id, None, None).await;
        match resolution.source {
            CloudTelemetryWriteMode::Local => {
                self.local
                    .get_metric_baseline(
                        project_id,
                        service_name,
                        metric_name,
                        environment,
                        lookback_days,
                    )
                    .await
            }
            CloudTelemetryWriteMode::Cloud => {
                cloud_metrics
                    .get_metric_baseline(
                        project_id,
                        service_name,
                        metric_name,
                        environment,
                        lookback_days,
                    )
                    .await
            }
        }
    }

    async fn get_recent_minute_aggregates(
        &self,
        project_id: i32,
        service_name: &str,
        metric_name: &str,
        environment: Option<&str>,
        minutes: i32,
    ) -> StorageResult<Vec<MinuteAggregate>> {
        let Some(cloud_metrics) = self.cloud_metrics.as_ref() else {
            return self
                .local
                .get_recent_minute_aggregates(
                    project_id,
                    service_name,
                    metric_name,
                    environment,
                    minutes,
                )
                .await;
        };
        let resolution = self.resolve_analytics(project_id, None, None).await;
        match resolution.source {
            CloudTelemetryWriteMode::Local => {
                self.local
                    .get_recent_minute_aggregates(
                        project_id,
                        service_name,
                        metric_name,
                        environment,
                        minutes,
                    )
                    .await
            }
            CloudTelemetryWriteMode::Cloud => {
                cloud_metrics
                    .get_recent_minute_aggregates(
                        project_id,
                        service_name,
                        metric_name,
                        environment,
                        minutes,
                    )
                    .await
            }
        }
    }

    async fn get_recent_deploys(
        &self,
        project_id: i32,
        minutes: i32,
    ) -> StorageResult<Vec<DeployEvent>> {
        self.local.get_recent_deploys(project_id, minutes).await
    }

    async fn apply_retention(&self, project_id: i32) -> StorageResult<u64> {
        self.local.apply_retention(project_id).await
    }

    async fn get_p95_latency(
        &self,
        project_id: i32,
        service_name: &str,
        window_minutes: i32,
    ) -> StorageResult<f64> {
        self.local
            .get_p95_latency(project_id, service_name, window_minutes)
            .await
    }
}

impl CloudRoutedOtelStorage {
    /// Resolve a multi-project span-stats query.
    ///
    /// Span stats aggregate across several projects at once, and there is no
    /// coherent way to aggregate percentiles computed independently in two
    /// stores — merging them would invent numbers that describe neither. So a
    /// query that spans both destinations is refused with an actionable error
    /// rather than answered wrongly, which is the same choice ADR-040 §3 makes
    /// for a straddling time range.
    async fn resolve_span_stats(
        &self,
        query: &SpanStatsQuery,
    ) -> StorageResult<CloudTelemetryWriteMode> {
        let mut cloud_projects: Vec<i32> = Vec::new();
        let mut local_projects: Vec<i32> = Vec::new();

        for project_id in &query.project_ids {
            let resolution = self
                .resolve(*project_id, Some(query.start_time), Some(query.end_time))
                .await;
            match resolution.source {
                CloudTelemetryWriteMode::Cloud => cloud_projects.push(*project_id),
                CloudTelemetryWriteMode::Local => local_projects.push(*project_id),
            }
        }

        match (cloud_projects.is_empty(), local_projects.is_empty()) {
            // No projects at all: answer locally and return an empty result,
            // which is what the local store would have said anyway.
            (true, true) => Ok(CloudTelemetryWriteMode::Local),
            (true, false) => Ok(CloudTelemetryWriteMode::Local),
            (false, true) => Ok(CloudTelemetryWriteMode::Cloud),
            (false, false) => Err(OtelError::Validation {
                message: format!(
                    "This operation report spans two telemetry sources: project(s) {:?} store \
                     spans on this instance and project(s) {:?} are Cloud-primary. Percentiles \
                     computed separately in two stores cannot be combined into one ranking, so \
                     the report is not produced rather than being produced wrongly. Select \
                     projects from one source at a time.",
                    local_projects, cloud_projects
                ),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Records which side a call landed on, so a test can assert routing
    /// without a database or a Cloud tenant.
    #[derive(Default)]
    struct CountingCloudSource {
        query_spans: AtomicUsize,
        get_trace: AtomicUsize,
        has_traces: AtomicUsize,
    }

    #[async_trait]
    impl CloudSpanSource for CountingCloudSource {
        async fn query_spans(&self, _query: TraceQuery) -> StorageResult<Vec<SpanRecord>> {
            self.query_spans.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }
        async fn query_trace_summaries(
            &self,
            _query: TraceQuery,
        ) -> StorageResult<Vec<TraceSummary>> {
            Ok(Vec::new())
        }
        async fn count_traces(&self, _query: TraceQuery) -> StorageResult<u64> {
            Ok(0)
        }
        async fn has_traces(&self, _project_id: i32) -> StorageResult<bool> {
            self.has_traces.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        }
        async fn get_trace(
            &self,
            _project_id: i32,
            _trace_id: &str,
        ) -> StorageResult<Vec<SpanRecord>> {
            self.get_trace.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }
        async fn query_span_stats(&self, _query: SpanStatsQuery) -> StorageResult<Vec<SpanStats>> {
            Ok(Vec::new())
        }
        async fn count_span_stats(&self, _query: SpanStatsQuery) -> StorageResult<u64> {
            Ok(0)
        }
    }

    #[test]
    fn a_clamped_resolution_narrows_the_query_start() {
        let clamp_at = chrono::Utc::now() - chrono::Duration::minutes(30);
        let query = TraceQuery {
            project_id: 7,
            start_time: Some(chrono::Utc::now() - chrono::Duration::hours(6)),
            end_time: Some(chrono::Utc::now()),
            ..Default::default()
        };
        let resolution = WindowResolution {
            source: CloudTelemetryWriteMode::Cloud,
            window_clamped_at: Some(clamp_at),
            from: clamp_at,
            to: chrono::Utc::now(),
        };

        let clamped = CloudRoutedOtelStorage::clamped(query, &resolution);
        assert_eq!(clamped.start_time, Some(clamp_at));
    }

    #[test]
    fn an_unclamped_resolution_leaves_the_query_alone() {
        let start = chrono::Utc::now() - chrono::Duration::hours(6);
        let query = TraceQuery {
            project_id: 7,
            start_time: Some(start),
            end_time: Some(chrono::Utc::now()),
            ..Default::default()
        };
        let resolution = WindowResolution {
            source: CloudTelemetryWriteMode::Cloud,
            window_clamped_at: None,
            from: start,
            to: chrono::Utc::now(),
        };

        let unchanged = CloudRoutedOtelStorage::clamped(query, &resolution);
        assert_eq!(
            unchanged.start_time,
            Some(start),
            "a window that does not straddle must be served as requested"
        );
    }

    #[tokio::test]
    async fn the_cloud_source_trait_is_reachable_from_every_routed_read() {
        // Guards against the failure this module exists to prevent: a routed
        // method that silently keeps calling the local store. Each counter
        // proves the Cloud side is wired for that specific read.
        let cloud = CountingCloudSource::default();
        assert!(cloud.query_spans(TraceQuery::default()).await.is_ok());
        assert!(cloud.get_trace(7, "trace").await.is_ok());
        assert!(cloud.has_traces(7).await.is_ok());
        assert_eq!(cloud.query_spans.load(Ordering::SeqCst), 1);
        assert_eq!(cloud.get_trace.load(Ordering::SeqCst), 1);
        assert_eq!(cloud.has_traces.load(Ordering::SeqCst), 1);
    }

    // ── Metric routing (ADR-043 §3) ───────────────────────────────────────

    #[test]
    fn a_clamped_analytics_resolution_narrows_the_metric_query_start() {
        let clamp_at = chrono::Utc::now() - chrono::Duration::minutes(30);
        let query = MetricQuery {
            project_id: 7,
            start_time: Some(chrono::Utc::now() - chrono::Duration::hours(6)),
            end_time: Some(chrono::Utc::now()),
            ..Default::default()
        };
        let resolution = WindowResolution {
            source: CloudTelemetryWriteMode::Cloud,
            window_clamped_at: Some(clamp_at),
            from: clamp_at,
            to: chrono::Utc::now(),
        };

        let clamped = CloudRoutedOtelStorage::clamped_metrics(query, &resolution);
        assert_eq!(clamped.start_time, Some(clamp_at));
    }

    /// Records whether — and with what window — Cloud was actually asked.
    #[derive(Default)]
    struct CountingCloudMetricSource {
        query_metrics_calls: AtomicUsize,
        last_start_time: std::sync::Mutex<Option<chrono::DateTime<chrono::Utc>>>,
    }

    #[async_trait]
    impl CloudMetricSource for CountingCloudMetricSource {
        async fn query_metrics(&self, query: MetricQuery) -> StorageResult<Vec<MetricBucket>> {
            self.query_metrics_calls.fetch_add(1, Ordering::SeqCst);
            *self.last_start_time.lock().expect("mutex") = query.start_time;
            Ok(vec![MetricBucket::scalar(
                chrono::Utc::now(),
                1.0,
                1.0,
                1.0,
                1,
            )])
        }
        async fn list_metric_names(&self, _project_id: i32) -> StorageResult<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_metric_label_keys(
            &self,
            _project_id: i32,
            _metric_name: &str,
            _start_time: chrono::DateTime<chrono::Utc>,
            _end_time: chrono::DateTime<chrono::Utc>,
        ) -> StorageResult<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_metric_label_values(
            &self,
            _project_id: i32,
            _metric_name: &str,
            _label_key: &str,
            _start_time: chrono::DateTime<chrono::Utc>,
            _end_time: chrono::DateTime<chrono::Utc>,
        ) -> StorageResult<Vec<String>> {
            Ok(Vec::new())
        }
        async fn get_metric_baseline(
            &self,
            _project_id: i32,
            _service_name: &str,
            _metric_name: &str,
            _environment: Option<&str>,
            _lookback_days: i32,
        ) -> StorageResult<Vec<BaselinePoint>> {
            Ok(Vec::new())
        }
        async fn get_recent_minute_aggregates(
            &self,
            _project_id: i32,
            _service_name: &str,
            _metric_name: &str,
            _environment: Option<&str>,
            _minutes: i32,
        ) -> StorageResult<Vec<MinuteAggregate>> {
            Ok(Vec::new())
        }
    }

    /// One mocked `project_telemetry_write_intervals` row, shaped exactly like
    /// `intervals_covering_for_group`'s `SELECT` decodes it.
    fn interval_row(
        id: i64,
        mode: &str,
        from_mins_ago: i64,
        to_mins_ago: Option<i64>,
    ) -> std::collections::BTreeMap<&'static str, sea_orm::Value> {
        let now = chrono::Utc::now();
        let mut row: std::collections::BTreeMap<&str, sea_orm::Value> =
            std::collections::BTreeMap::new();
        row.insert("id", id.into());
        row.insert("project_id", 7_i32.into());
        row.insert("signal_group", "analytics".to_string().into());
        row.insert("mode", mode.to_string().into());
        row.insert(
            "effective_from",
            (now - chrono::Duration::minutes(from_mins_ago)).into(),
        );
        row.insert(
            "effective_to",
            to_mins_ago
                .map(|mins| now - chrono::Duration::minutes(mins))
                .into(),
        );
        row.insert("reason", "operator".to_string().into());
        row
    }

    /// The full path: `CloudRoutedOtelStorage::query_metrics` reads the
    /// **analytics** ledger (not the span one), finds a straddle, clamps to
    /// the newest interval, and only then calls Cloud — proving both that
    /// metrics route independently of spans and that the straddle-clamp
    /// contract (ADR-040 §3, extended to metrics by ADR-043 §3) is wired end
    /// to end, not just unit-tested in isolation as a pure function.
    #[tokio::test]
    async fn metric_reads_straddling_an_analytics_cutover_are_clamped_to_the_newest_interval() {
        use sea_orm::{DatabaseBackend, MockDatabase};

        let db = std::sync::Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![
                    interval_row(1, "local", 240, Some(120)),
                    interval_row(2, "cloud", 120, None),
                ]])
                .into_connection(),
        );
        let write_modes = std::sync::Arc::new(
            crate::services::telemetry_write_mode::TelemetryWriteModeService::new(db),
        );

        let local = std::sync::Arc::new(crate::test_support::MockOtelStorage::new());
        let cloud_span = std::sync::Arc::new(CountingCloudSource::default());
        let cloud_metrics = std::sync::Arc::new(CountingCloudMetricSource::default());

        let routed = CloudRoutedOtelStorage::new(
            local as Arc<dyn OtelStorage>,
            cloud_span as Arc<dyn CloudSpanSource>,
            write_modes,
        )
        .with_cloud_metrics(cloud_metrics.clone() as Arc<dyn CloudMetricSource>);

        let query = MetricQuery {
            project_id: 7,
            start_time: Some(chrono::Utc::now() - chrono::Duration::minutes(200)),
            end_time: Some(chrono::Utc::now()),
            ..Default::default()
        };
        let result = routed
            .query_metrics(query)
            .await
            .expect("routed query succeeds");

        assert_eq!(
            cloud_metrics.query_metrics_calls.load(Ordering::SeqCst),
            1,
            "a window that straddles into the cloud interval must reach the Cloud source"
        );
        assert_eq!(
            result.len(),
            1,
            "the Cloud stub's answer must be the one returned"
        );
        let sent_start = cloud_metrics
            .last_start_time
            .lock()
            .expect("mutex")
            .expect("Cloud was called with a start time");
        // The clamp must have narrowed the request to the newest interval's
        // start (120 minutes ago), not the originally requested 200 minutes.
        assert!(
            (sent_start - (chrono::Utc::now() - chrono::Duration::minutes(120)))
                .num_seconds()
                .abs()
                < 5,
            "the query sent to Cloud must be clamped to the newest interval's start: {sent_start}"
        );
    }

    /// The mirror image: a project with no `cloud_analytics_write_mode`
    /// history at all must never reach Cloud, and must not even construct a
    /// `CloudMetricSource` call when none is wired.
    #[tokio::test]
    async fn a_project_with_no_analytics_ledger_stays_local_and_never_calls_cloud() {
        use sea_orm::{DatabaseBackend, MockDatabase};

        let db = std::sync::Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![
                    Vec::<std::collections::BTreeMap<&str, sea_orm::Value>>::new(),
                ])
                .into_connection(),
        );
        let write_modes = std::sync::Arc::new(
            crate::services::telemetry_write_mode::TelemetryWriteModeService::new(db),
        );

        let local = std::sync::Arc::new(crate::test_support::MockOtelStorage::new());
        let cloud_span = std::sync::Arc::new(CountingCloudSource::default());
        let cloud_metrics = std::sync::Arc::new(CountingCloudMetricSource::default());

        let routed = CloudRoutedOtelStorage::new(
            local as Arc<dyn OtelStorage>,
            cloud_span as Arc<dyn CloudSpanSource>,
            write_modes,
        )
        .with_cloud_metrics(cloud_metrics.clone() as Arc<dyn CloudMetricSource>);

        let query = MetricQuery {
            project_id: 9,
            ..Default::default()
        };
        assert!(routed.query_metrics(query).await.is_ok());
        assert_eq!(
            cloud_metrics.query_metrics_calls.load(Ordering::SeqCst),
            0,
            "a project with no analytics ledger has always been local; it must not call Cloud"
        );
    }
}
