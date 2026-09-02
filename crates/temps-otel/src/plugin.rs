// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Plugin registration for the OTel subsystem.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use tracing::{debug, error, info, warn};
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::anomaly::detector::{AnomalyDetector, AnomalyDetectorConfig};
use crate::handlers;
use crate::handlers::dashboard_handler;
use crate::handlers::facet_handler;
use crate::handlers::ingest_handler;
use crate::handlers::metric_alert_handler;
use crate::handlers::query_handler;
use crate::ingest::auth::OtelAuthService;
use crate::ingest::rate_limit::RateLimiter;
use crate::memory::OtelMemoryProfile;
use crate::relay::OtelRelay;
use crate::services::cross_project::{prune_stale_hints, CrossProjectTraceService, TraceHintMsg};
use crate::services::facet_service::FacetService;
use crate::services::health_service::HealthComputeService;
use crate::services::OtelService;
use crate::storage::clickhouse::{ClickHouseOtelConfig, ClickHouseOtelStorage};
use crate::storage::timescaledb::TimescaleDbStorage;
use crate::OtelAppState;
use temps_metrics::{MetricsStore, TimescaleMetricsStore};

// ── Configuration ───────────────────────────────────────────────────

/// OTel subsystem configuration, read from environment variables.
///
/// All settings have sensible defaults and are optional.
#[derive(Debug, Clone)]
pub struct OtelConfig {
    // S3 archival
    pub s3_region: Option<String>,
    pub s3_endpoint: Option<String>,
    pub s3_access_key: Option<String>,
    pub s3_secret_key: Option<String>,
    pub s3_bucket: Option<String>,
    pub s3_prefix: String,

    // Retention
    pub retention_days: u32,
    pub retention_check_interval_secs: u64,

    // Rate limiting
    pub rate_limit_requests: u32,
    pub rate_limit_window_secs: u64,

    // Quota. `None` (the default) disables per-project storage quotas
    // entirely — ingest skips the quota check and its expensive per-project
    // usage estimate. Set `TEMPS_OTEL_QUOTA_GB` to opt in.
    pub quota_bytes_per_project: Option<u64>,

    // Background tasks
    pub enable_health_compute: bool,
    pub enable_anomaly_detection: bool,

    // Ingest backpressure. Process-wide operational tuning knob (not
    // per-tenant config) — see `crate::services::otel_service::DEFAULT_MAX_CONCURRENT_INGEST_REQUESTS`.
    pub max_concurrent_ingest_requests: usize,

    /// Startup-derived memory bounds shared by ingest and relay queues.
    pub memory_profile: OtelMemoryProfile,

    /// Whether the ingest concurrency was explicitly configured instead of
    /// selected from effective memory.
    pub ingest_concurrency_overridden: bool,
}

impl Default for OtelConfig {
    fn default() -> Self {
        Self {
            s3_region: None,
            s3_endpoint: None,
            s3_access_key: None,
            s3_secret_key: None,
            s3_bucket: None,
            s3_prefix: "otel-logs".to_string(),
            retention_days: 7,
            retention_check_interval_secs: 3600, // 1 hour
            rate_limit_requests: 1000,
            rate_limit_window_secs: 60,
            quota_bytes_per_project: None, // quota disabled unless configured
            enable_health_compute: true,
            enable_anomaly_detection: true,
            max_concurrent_ingest_requests:
                crate::services::otel_service::DEFAULT_MAX_CONCURRENT_INGEST_REQUESTS,
            memory_profile: OtelMemoryProfile::fallback(),
            ingest_concurrency_overridden: false,
        }
    }
}

impl OtelConfig {
    /// Read configuration from `TEMPS_OTEL_*` environment variables.
    pub fn from_env() -> Self {
        let memory_profile = OtelMemoryProfile::detect();
        let mut config = Self {
            max_concurrent_ingest_requests: memory_profile.max_concurrent_ingest_requests,
            memory_profile,
            ..Self::default()
        };

        if let Ok(v) = std::env::var("TEMPS_OTEL_S3_REGION") {
            config.s3_region = Some(v);
        }
        if let Ok(v) = std::env::var("TEMPS_OTEL_S3_ENDPOINT") {
            config.s3_endpoint = Some(v);
        }
        if let Ok(v) = std::env::var("TEMPS_OTEL_S3_ACCESS_KEY") {
            config.s3_access_key = Some(v);
        }
        if let Ok(v) = std::env::var("TEMPS_OTEL_S3_SECRET_KEY") {
            config.s3_secret_key = Some(v);
        }
        if let Ok(v) = std::env::var("TEMPS_OTEL_S3_BUCKET") {
            config.s3_bucket = Some(v);
        }
        if let Ok(v) = std::env::var("TEMPS_OTEL_S3_PREFIX") {
            config.s3_prefix = v;
        }
        if let Ok(v) = std::env::var("TEMPS_OTEL_RETENTION_DAYS") {
            if let Ok(days) = v.parse() {
                config.retention_days = days;
            }
        }
        if let Ok(v) = std::env::var("TEMPS_OTEL_RATE_LIMIT") {
            if let Ok(limit) = v.parse() {
                config.rate_limit_requests = limit;
            }
        }
        if let Ok(v) = std::env::var("TEMPS_OTEL_RATE_LIMIT_WINDOW_SECS") {
            if let Ok(secs) = v.parse() {
                config.rate_limit_window_secs = secs;
            }
        }
        if let Ok(v) = std::env::var("TEMPS_OTEL_QUOTA_GB") {
            match v.parse::<u64>() {
                // 0 keeps quotas disabled, same as leaving the var unset.
                Ok(gb) => {
                    config.quota_bytes_per_project = (gb > 0).then(|| gb * 1024 * 1024 * 1024)
                }
                Err(_) => warn!(
                    value = %v,
                    "TEMPS_OTEL_QUOTA_GB is set but is not a non-negative integer; \
                     storage quota stays disabled"
                ),
            }
        }
        if let Ok(v) = std::env::var("TEMPS_OTEL_ENABLE_HEALTH_COMPUTE") {
            config.enable_health_compute = v != "0" && v != "false";
        }
        if let Ok(v) = std::env::var("TEMPS_OTEL_ENABLE_ANOMALY_DETECTION") {
            config.enable_anomaly_detection = v != "0" && v != "false";
        }
        if let Ok(v) = std::env::var("TEMPS_OTEL_MAX_CONCURRENT_INGEST_REQUESTS") {
            match parse_max_concurrent_ingest_requests(&v) {
                Some(limit) => {
                    config.max_concurrent_ingest_requests = limit;
                    config.ingest_concurrency_overridden = true;
                }
                None => warn!(
                    value = %v,
                    "TEMPS_OTEL_MAX_CONCURRENT_INGEST_REQUESTS is set but is not a positive \
                     integer within the supported range; keeping the automatically selected ingest \
                     concurrency ceiling"
                ),
            }
        }

        config
    }

    /// Returns true if S3 archival is fully configured.
    pub fn has_s3_config(&self) -> bool {
        self.s3_region.is_some()
            && self.s3_access_key.is_some()
            && self.s3_secret_key.is_some()
            && self.s3_bucket.is_some()
    }
}

/// Parses `TEMPS_OTEL_MAX_CONCURRENT_INGEST_REQUESTS`. Returns `None` (caller
/// keeps the default) for anything that isn't a positive integer within
/// `Semaphore::MAX_PERMITS` — `Semaphore::new` asserts on that bound and would
/// otherwise panic the process at startup on a mistyped value.
///
/// A free function (rather than inline in `from_env`) so this parsing/bounds
/// logic is unit-testable without mutating process-global environment
/// variables, which the other `TEMPS_OTEL_*` fields in this file don't do.
fn parse_max_concurrent_ingest_requests(v: &str) -> Option<usize> {
    let limit = v.parse::<usize>().ok()?;
    (limit > 0 && limit <= tokio::sync::Semaphore::MAX_PERMITS).then_some(limit)
}

/// Number of counters the OTel pipeline-stats sampler publishes each cycle —
/// one per [`crate::types::PipelineStats`] field.
pub const OTEL_PIPELINE_STAT_COUNT: usize = 15;

/// How often the pipeline-stats sampler snapshots the counters, in seconds.
///
/// Public because a stored point is a *delta over this interval*, so any
/// reader charting the series has to state the unit — see
/// `PipelineHistoryResponse::sample_interval_seconds`. A chart that says
/// "12 dropped spans" without saying "per minute" is not interpretable.
pub const OTEL_STATS_SAMPLE_INTERVAL_SECS: u64 = 60;

/// Synthetic node ID of the control plane (mirrors the proxy metrics sampler).
///
/// The pipeline counters are process-wide, not per-node and not per-project,
/// so they are written against `SourceKind::Node` / id 0 — the same synthetic
/// source the `proxy.*` metrics use.
pub const CONTROL_PLANE_NODE_ID: i32 = 0;

/// Every metric name the pipeline-stats sampler writes, in display order.
///
/// The **single source of truth** shared by the writer
/// ([`pipeline_stat_deltas`]) and the reader
/// (`GET /otel/pipeline-history`). Without this, adding a counter to
/// [`crate::types::PipelineStats`] would mean editing the sampler, the query
/// handler and the UI separately — and the failure mode of forgetting one is
/// silent: a metric that is written but never charted, or charted but never
/// written. `pipeline_stat_deltas_match_metric_names` pins the two together.
///
/// Order is deliberate — received/stored/dropped triplets stay adjacent so the
/// UI can render them as one panel per signal.
pub const OTEL_PIPELINE_METRIC_NAMES: [&str; OTEL_PIPELINE_STAT_COUNT] = [
    "otel.rate_limited_requests",
    "otel.quota_exceeded_requests",
    "otel.metrics_received",
    "otel.metrics_stored",
    "otel.metrics_dropped",
    "otel.spans_received",
    "otel.spans_stored",
    "otel.spans_dropped",
    "otel.logs_received",
    "otel.logs_stored_db",
    "otel.logs_stored_s3",
    "otel.logs_dropped",
    "otel.ingest_errors",
    "otel.relay_dropped_batches",
    "otel.relay_dropped_items",
];

/// Turn a pipeline-stats snapshot plus the previous cycle's checkpoint into the
/// `(metric name, delta)` pairs the sampler writes to the metrics store.
///
/// Deltas rather than cumulative values, so a point reads as "events in the
/// last sample window" and an alert threshold like `> 10` means what an
/// operator expects. `saturating_sub` guards the one case where the sequence
/// is not monotonic: a process restart zeroes every atomic, which would
/// otherwise underflow into a nonsense value.
///
/// A free function so the naming and the arithmetic are unit-testable without
/// standing up the sampler task, a metrics store, or a 60-second timer.
fn pipeline_stat_deltas(
    snap: &crate::types::PipelineStats,
    prev: &crate::types::PipelineStats,
) -> [(&'static str, u64); OTEL_PIPELINE_STAT_COUNT] {
    [
        (
            "otel.rate_limited_requests",
            snap.rate_limited_requests
                .saturating_sub(prev.rate_limited_requests),
        ),
        (
            "otel.quota_exceeded_requests",
            snap.quota_exceeded_requests
                .saturating_sub(prev.quota_exceeded_requests),
        ),
        (
            "otel.metrics_received",
            snap.metrics_received.saturating_sub(prev.metrics_received),
        ),
        (
            "otel.metrics_stored",
            snap.metrics_stored.saturating_sub(prev.metrics_stored),
        ),
        (
            "otel.metrics_dropped",
            snap.metrics_dropped.saturating_sub(prev.metrics_dropped),
        ),
        (
            "otel.spans_received",
            snap.spans_received.saturating_sub(prev.spans_received),
        ),
        (
            "otel.spans_stored",
            snap.spans_stored.saturating_sub(prev.spans_stored),
        ),
        (
            "otel.spans_dropped",
            snap.spans_dropped.saturating_sub(prev.spans_dropped),
        ),
        (
            "otel.logs_received",
            snap.logs_received.saturating_sub(prev.logs_received),
        ),
        (
            "otel.logs_stored_db",
            snap.logs_stored_db.saturating_sub(prev.logs_stored_db),
        ),
        (
            "otel.logs_stored_s3",
            snap.logs_stored_s3.saturating_sub(prev.logs_stored_s3),
        ),
        (
            "otel.logs_dropped",
            snap.logs_dropped.saturating_sub(prev.logs_dropped),
        ),
        (
            "otel.ingest_errors",
            snap.ingest_errors.saturating_sub(prev.ingest_errors),
        ),
        (
            "otel.relay_dropped_batches",
            snap.relay_dropped_batches
                .saturating_sub(prev.relay_dropped_batches),
        ),
        (
            "otel.relay_dropped_items",
            snap.relay_dropped_items
                .saturating_sub(prev.relay_dropped_items),
        ),
    ]
}

// ── OpenAPI Schema ──────────────────────────────────────────────────

#[derive(OpenApiTrait)]
#[openapi(
    paths(
        ingest_handler::ingest_metrics,
        ingest_handler::ingest_traces,
        ingest_handler::ingest_logs,
        ingest_handler::ingest_metrics_by_path,
        ingest_handler::ingest_traces_by_path,
        ingest_handler::ingest_logs_by_path,
        query_handler::query_metrics,
        query_handler::list_metric_names,
        query_handler::list_metric_label_keys,
        query_handler::list_metric_label_values,
        query_handler::query_traces,
        query_handler::query_trace_summaries,
        query_handler::query_span_stats,
        query_handler::get_trace,
        query_handler::query_logs,
        query_handler::list_insights,
        query_handler::get_health,
        query_handler::get_quota,
        query_handler::has_traces,
        query_handler::get_pipeline_stats,
        query_handler::get_ingest_errors,
        query_handler::get_pipeline_history,
        query_handler::query_genai_traces,
        query_handler::get_genai_trace,
        query_handler::get_cross_project_trace_siblings,
        query_handler::get_unified_trace,
        facet_handler::list_facets,
        facet_handler::create_facet,
        facet_handler::delete_facet,
        facet_handler::retry_facet_backfill,
        dashboard_handler::list_dashboards,
        dashboard_handler::create_dashboard,
        dashboard_handler::get_dashboard,
        dashboard_handler::update_dashboard,
        dashboard_handler::delete_dashboard,
        metric_alert_handler::list_alerts,
        metric_alert_handler::create_alert,
        metric_alert_handler::get_alert,
        metric_alert_handler::update_alert,
        metric_alert_handler::delete_alert,
        metric_alert_handler::preview_alert,
    ),
    components(
        schemas(
            query_handler::OtelMetricsResponse,
            query_handler::OtelMetricNamesResponse,
            query_handler::OtelMetricLabelKeysResponse,
            query_handler::OtelMetricLabelValuesResponse,
            query_handler::TracesResponse,
            query_handler::TraceSummariesResponse,
            crate::types::TraceSummary,
            query_handler::SpanStatsResponse,
            crate::types::SpanStats,
            query_handler::LogsResponse,
            query_handler::InsightsResponse,
            query_handler::HealthResponse,
            query_handler::QuotaResponse,
            query_handler::HasTracesResponse,
            query_handler::PipelineStatsResponse,
        query_handler::IngestErrorsResponse,
        crate::types::IngestErrorSummary,
        query_handler::PipelineHistoryResponse,
        query_handler::PipelineSeries,
        query_handler::PipelineHistoryPoint,
            crate::types::MetricBucket,
            crate::types::HistogramSummary,
            crate::types::MetricAggregation,
            crate::types::AggregationTemporality,
            crate::types::SpanRecord,
            crate::types::SpanEvent,
            crate::types::SpanKind,
            crate::types::SpanStatusCode,
            crate::types::LogRecord,
            crate::types::LogSeverity,
            crate::types::ResourceInfo,
            crate::types::MetricType,
            crate::types::Insight,
            crate::types::InsightSeverity,
            crate::types::InsightStatus,
            crate::types::HealthSummary,
            crate::types::HealthStatus,
            crate::types::StorageQuota,
            crate::types::PipelineStats,
            query_handler::GenAiTraceSummariesResponse,
            query_handler::GenAiTraceDetailResponse,
            crate::types::GenAiTraceSummary,
            crate::types::GenAiSpanDetail,
            crate::types::GenAiEvent,
            query_handler::CrossProjectSiblingRef,
            query_handler::CrossProjectTraceResponse,
            crate::services::cross_project::UnifiedTrace,
            crate::services::cross_project::AnnotatedSpan,
            crate::services::cross_project::ProjectRef,
            crate::services::cross_project::SiblingRef,
            crate::services::cross_project::TraceProjectRef,
            facet_handler::CreateFacetRequest,
            facet_handler::FacetsResponse,
            crate::services::FacetInfo,
            crate::services::FacetStatus,
            crate::services::FacetBackendKind,
            dashboard_handler::CreateDashboardRequest,
            dashboard_handler::UpdateDashboardRequest,
            dashboard_handler::OtelDashboardResponse,
            dashboard_handler::OtelDashboardsResponse,
            crate::services::dashboard_service::DashboardLayout,
            crate::services::dashboard_service::DashboardSection,
            crate::services::dashboard_service::DashboardTile,
            metric_alert_handler::CreateMetricAlertRequest,
            metric_alert_handler::UpdateMetricAlertRequest,
            metric_alert_handler::OtelMetricAlertRuleResponse,
            metric_alert_handler::OtelMetricAlertsResponse,
            crate::services::metric_alert_evaluator::SeriesStateEntry,
            metric_alert_handler::AnomalyPreviewRequest,
            metric_alert_handler::AnomalyPreviewResponse,
            metric_alert_handler::AnomalyPreviewPointResponse,
            crate::detectors::DetectionConfig,
            crate::detectors::StaticParams,
            crate::detectors::AnomalyParams,
            crate::detectors::ForecastParams,
            crate::detectors::OutlierParams,
            crate::detectors::AutoWatchParams,
            crate::detectors::Comparator,
            crate::detectors::Direction,
            crate::detectors::Seasonality,
            crate::detectors::AnomalyAlgorithm,
            crate::detectors::ForecastAlgorithm,
            crate::detectors::OutlierAlgorithm,
        )
    ),
    info(
        title = "OTel API",
        description = "OpenTelemetry data collection, storage, and analysis endpoints",
        version = "1.0.0"
    ),
    tags(
        (name = "OTel Ingest", description = "OTLP/HTTP ingest endpoints (protobuf)"),
        (name = "OTel", description = "Query endpoints for the monitoring UI"),
        (name = "OTel Facets", description = "Span attribute facet registration (fast-filter slots)"),
        (name = "GenAI", description = "GenAI agent activity tracing endpoints")
    )
)]
pub struct OtelApiDoc;

// ── Plugin ──────────────────────────────────────────────────────────

/// OTel Plugin for Temps.
pub struct OtelPlugin {
    /// Handle to the ClickHouse storage's `RetentionResolver` slot, captured
    /// in `register_services` (before the storage is moved into `Arc<dyn
    /// OtelStorage>`) and written into from `initialize_plugin_services`,
    /// which runs only after every plugin has registered its services.
    /// `register_services` runs in plugin-registration order and this plugin
    /// registers before any later-registered plugin (e.g. one implementing
    /// per-project retention) gets a chance to provide a resolver — same
    /// two-phase handoff `DeploymentsPlugin` uses for `DeploymentGate`.
    retention_resolver_slot: tokio::sync::OnceCell<Arc<temps_core::RetentionResolverSlot>>,
    /// Handle to the `OtelRelaySlot` captured in `register_services` and
    /// written into from `initialize_plugin_services` — same two-phase
    /// handoff as `retention_resolver_slot`. The background relay consumer
    /// (spawned in `register_services`) holds its own `Arc` clone and calls
    /// `relay_slot.relay(msg)` for each batch received from `otel_relay_tx`.
    /// When no plugin provides an `Arc<dyn OtelRelay>`, the slot stays loaded
    /// with `NoopOtelRelay` and the relay loop is a cheap no-op.
    relay_slot: tokio::sync::OnceCell<Arc<crate::relay::OtelRelaySlot>>,
}

impl OtelPlugin {
    pub fn new() -> Self {
        Self {
            retention_resolver_slot: tokio::sync::OnceCell::new(),
            relay_slot: tokio::sync::OnceCell::new(),
        }
    }
}

impl Default for OtelPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for OtelPlugin {
    fn name(&self) -> &'static str {
        "otel"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let config = OtelConfig::from_env();
            info!(
                effective_memory_bytes = config.memory_profile.effective_memory_bytes,
                memory_limit_source = config.memory_profile.source.as_str(),
                max_concurrent_ingest_requests = config.max_concurrent_ingest_requests,
                ingest_concurrency_overridden = config.ingest_concurrency_overridden,
                relay_queue_max_bytes = config.memory_profile.relay_queue_max_bytes,
                external_relay_max_bytes = config.memory_profile.external_relay_max_bytes,
                "OTel startup memory limits selected"
            );
            context.register_service(Arc::new(config.memory_profile));
            let db = context.require_service::<sea_orm::DatabaseConnection>();

            // Create S3 archiver if configured
            let s3_client = if config.has_s3_config() {
                match crate::storage::timescaledb::S3LogArchiver::new(
                    config.s3_region.as_deref().unwrap_or("us-east-1"),
                    config.s3_endpoint.as_deref(),
                    config.s3_access_key.as_deref().unwrap_or_default(),
                    config.s3_secret_key.as_deref().unwrap_or_default(),
                    config.s3_bucket.clone().unwrap_or_default(),
                    config.s3_prefix.clone(),
                )
                .await
                {
                    Ok(archiver) => {
                        info!(
                            "OTel S3 log archiver configured (bucket: {})",
                            config.s3_bucket.as_deref().unwrap_or("?")
                        );
                        Some(Arc::new(archiver))
                    }
                    Err(e) => {
                        error!(
                            "Failed to create S3 log archiver: {}, log archival will be disabled",
                            e
                        );
                        None
                    }
                }
            } else {
                debug!(
                    "OTel S3 log archival not configured (set TEMPS_OTEL_S3_* env vars to enable)"
                );
                None
            };

            // ── Storage backend selection ────────────────────────────
            //
            // When all four TEMPS_CLICKHOUSE_* env vars are set,
            // ClickHouseOtelStorage is the backend for span telemetry.
            // Non-span methods (metrics, logs, insights, health, quota)
            // are always delegated to TimescaleDbStorage regardless.
            // When ClickHouse is not configured, TimescaleDbStorage is
            // used for everything — the default, unchanged path.
            let ch_config = read_clickhouse_otel_config_from_env();

            // ── Facet cache ──────────────────────────────────────────────────
            //
            // Created before both storage backends so ClickHouse, TimescaleDB
            // (whichever is the ingest/query fast-path) and FacetService
            // (create/delete) all share the same Arc. The cache starts empty;
            // FacetService loads initial data from Postgres below.
            let facet_cache: crate::services::FacetCache = Arc::new(
                arc_swap::ArcSwap::from_pointee(std::collections::HashMap::new()),
            );

            // TimescaleDbStorage is always constructed: it is the sole
            // backend when CH is disabled, and the inner delegate when
            // CH is enabled. It also needs the facet cache directly since
            // it's the default backend and handles its own slot-column
            // ingest/query when ClickHouse isn't configured.
            let timescale_storage = Arc::new(TimescaleDbStorage::with_config(
                db.clone(),
                s3_client,
                config.retention_days,
                config.quota_bytes_per_project,
                Some(facet_cache.clone()),
            ));

            let storage: Arc<dyn crate::storage::OtelStorage> = if let Some(ch_cfg) = ch_config {
                info!(
                    url = %ch_cfg.url,
                    database = %ch_cfg.database,
                    "ClickHouse OTel backend enabled (ADR-016) — applying migrations"
                );
                // Slot defaults to FixedRetentionResolver; a plugin (e.g. one
                // implementing per-project data retention policies) is wired
                // in later from `initialize_plugin_services` — see the
                // `retention_resolver_slot` field doc for why a direct
                // `get_service` call here would never find it.
                let retention_slot = Arc::new(temps_core::RetentionResolverSlot::new_default());
                let _ = self.retention_resolver_slot.set(retention_slot.clone());
                let ch_storage = Arc::new(ClickHouseOtelStorage::new(
                    ch_cfg.clone(),
                    timescale_storage,
                    retention_slot as Arc<dyn temps_core::RetentionResolver>,
                    Some(facet_cache.clone()),
                ));
                // Run migrations in a background task so plugin init
                // returns promptly. If migrations fail, the first
                // span ingest or read will surface the error.
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    let client = ch_storage.ch_client().clone();
                    let database_name = ch_cfg.database.clone();
                    handle.spawn(async move {
                        match crate::storage::clickhouse::migrations::apply_migrations(
                            &client,
                            &database_name,
                        )
                        .await
                        {
                            Ok(report) => info!(
                                applied = ?report.applied,
                                skipped_count = report.skipped.len(),
                                "ClickHouse OTel migrations applied"
                            ),
                            Err(e) => tracing::warn!(
                                error = %e,
                                "ClickHouse OTel migrations failed; \
                                 span ingest/queries will surface the error per-call"
                            ),
                        }
                    });
                } else {
                    tracing::warn!(
                        "No tokio runtime available when initializing ClickHouse OTel \
                             backend; migrations will not run. This usually means the plugin \
                             was wired during a sync init path."
                    );
                }
                ch_storage as Arc<dyn crate::storage::OtelStorage>
            } else {
                debug!(
                    "ClickHouse OTel backend disabled (TEMPS_CLICKHOUSE_* unset) — \
                         using TimescaleDB"
                );
                timescale_storage as Arc<dyn crate::storage::OtelStorage>
            };
            context.register_service(storage.clone());

            // Create auth service. Also registered in the context so
            // `configure_routes` can inject the ADR-028 ProjectAccessChecker
            // (registered by a later plugin) into the `tk_`-key ingest path.
            let auth_service = Arc::new(OtelAuthService::new(db.clone()));
            context.register_service(auth_service.clone());

            // Create rate limiter
            let rate_limiter = Arc::new(RateLimiter::new(
                config.rate_limit_requests,
                Duration::from_secs(config.rate_limit_window_secs),
            ));

            // Create the main OTel service
            let otel_service = Arc::new(OtelService::new(
                storage.clone(),
                auth_service,
                rate_limiter,
                config.max_concurrent_ingest_requests,
            ));
            context.register_service(otel_service.clone());
            // Also expose the same service behind the storage-agnostic read
            // contract so read-only consumers (e.g. the AI debugging chat in
            // `temps-ai-chat`) can query traces via `temps_core::TraceReader`
            // WITHOUT depending on this heavy crate. Absent → those consumers
            // simply offer no trace tools.
            context.register_service(otel_service.clone() as Arc<dyn temps_core::TraceReader>);

            // Build a MetricsStore pointing at the same TimescaleDB connection.
            // This forwards OTLP-pushed metrics into `service_metrics` alongside
            // scraper-collected DB/container/node metrics, unifying the data model.
            // We always create a TimescaleDB store here — if monitoring is disabled
            // the store is still valid but the scraper won't run, so no metrics
            // will appear in service_metrics from the scraper side.
            let metrics_store: Arc<dyn MetricsStore> =
                Arc::new(TimescaleMetricsStore::new(db.clone()));

            // Bounded channel for fire-and-forget MetricsStore writes from OTLP ingest.
            // The background consumer task (spawned below) drains the channel.
            // Capacity = 512 batches; try_send drops silently when full.
            let (metrics_write_tx, mut metrics_write_rx) =
                tokio::sync::mpsc::channel::<Vec<temps_metrics::MetricPoint>>(512);

            // ── ADR-027 Phase 0: Cross-project trace hint pipeline ───────────
            //
            // A bounded mpsc channel (capacity 1,000) decouples span ingest
            // latency from the hint write.  When the channel is full,
            // `do_ingest_traces` drops the hint (non-blocking try_send) and
            // warns.  The background consumer below drains the channel and
            // calls `record_hint`, which routes through the active storage
            // backend: a multi-row `INSERT … ON CONFLICT DO NOTHING` into the
            // Postgres control table, or a batched insert into the compressed
            // ClickHouse `cross_project_trace_refs` table when CH is enabled.
            let (trace_hint_tx, mut trace_hint_rx) =
                tokio::sync::mpsc::channel::<TraceHintMsg>(1000);

            // ── OtelRelay extension point ────────────────────────────────────
            //
            // Slot defaults to NoopOtelRelay; a plugin (e.g. one implementing
            // OTLP batch forwarding) is wired in later from
            // `initialize_plugin_services` — see `relay_slot` field doc for
            // why a direct `get_service` call here would never find it.
            let relay_slot = Arc::new(crate::relay::OtelRelaySlot::new_default());
            let _ = self.relay_slot.set(relay_slot.clone());

            // Count- and byte-bounded handoff for fire-and-forget relay of
            // decoded OTLP batches. Ingest handlers never wait for capacity.
            let (otel_relay_tx, mut otel_relay_rx) = crate::relay::bounded_relay_queue(
                crate::relay::RELAY_QUEUE_MAX_BATCHES,
                config.memory_profile.relay_queue_max_bytes,
            );

            let cross_project_service =
                Arc::new(CrossProjectTraceService::new(db.clone(), storage.clone()));
            context.register_service(cross_project_service.clone());

            // Metric dashboards + alert rules: Postgres-backed config/metadata
            // services plus the global audit logger for write operations.
            let dashboard_service =
                Arc::new(crate::services::MetricDashboardService::new(db.clone()));
            let metric_alert_service =
                Arc::new(crate::services::MetricAlertService::new(db.clone()));
            let audit_service = context.require_service::<dyn temps_core::AuditLogger>();

            // ── Facet service ────────────────────────────────────────────────
            //
            // Obtain a ClickHouse client for DDL mutations (backfill/clear).
            // When CH is not configured, `ch_client_for_facets` is None and
            // create/delete operations warn and skip the mutation step.
            let ch_client_for_facets: Option<::clickhouse::Client> = {
                let ch_cfg = read_clickhouse_otel_config_from_env();
                ch_cfg.map(|cfg| {
                    ::clickhouse::Client::default()
                        .with_url(&cfg.url)
                        .with_database(&cfg.database)
                        .with_user(&cfg.user)
                        .with_password(&cfg.password)
                })
            };
            let facet_service = Arc::new(FacetService::new(
                db.clone(),
                ch_client_for_facets,
                facet_cache.clone(),
            ));
            // Load initial facet→slot mapping from Postgres into the shared cache.
            // Non-fatal: if Postgres is unavailable at startup, the cache stays
            // empty and facet filtering falls back to JSONExtractString.
            if let Err(e) = facet_service.refresh_cache().await {
                warn!(
                    error = %e,
                    "Failed to load initial OTel facet cache from Postgres; \
                     facet-accelerated filtering will not be available until the next successful \
                     create/delete or server restart"
                );
            }
            context.register_service(facet_service.clone());

            // 5. Metric alert evaluator
            //
            // Builds its own AlarmService instance (separate from console.rs's)
            // wired to the same NotificationService + JobQueue, then spawns the
            // background evaluator. The two AlarmService instances keep
            // independent in-memory cooldown maps, but fire_alarm's actual
            // cooldown check queries the DB `alarms` table by type+deployment+
            // container, so duplicate suppression is still correct. OTEL rules
            // always set deployment_id=None, so collisions with the monitoring
            // evaluator are unlikely.
            let metric_alert_evaluator = {
                let notification_service =
                    context.require_service::<dyn temps_core::notifications::NotificationService>();
                let job_queue = context.require_service::<dyn temps_core::JobQueue>();
                let alarm_service = Arc::new(temps_monitoring::AlarmService::new(
                    db.clone(),
                    notification_service.clone(),
                    job_queue.clone(),
                ));
                // Dynamic per-series alarms bypass the per-rule cooldown: the
                // evaluator's per-series state machine already guarantees
                // exactly-once firing per series until it resolves (ADR-026).
                let alarm_service_dynamic = Arc::new(
                    temps_monitoring::AlarmService::new(
                        db.clone(),
                        notification_service,
                        job_queue,
                    )
                    .with_cooldown(chrono::Duration::zero()),
                );
                // ADR-022: optional general AI foundation, registered by the AI
                // gateway plugin when present. Absent -> deterministic Tier-1 text.
                let ai = context.get_service::<dyn temps_ai::AiService>();
                Arc::new(crate::services::MetricAlertEvaluator::new(
                    metric_alert_service.clone(),
                    otel_service.clone(),
                    alarm_service,
                    alarm_service_dynamic,
                    db.clone(),
                    ai,
                ))
            };

            // Create app state for handlers. The `project_access_checker` is
            // injected in `configure_routes` (after all services register).
            let app_state = OtelAppState {
                otel_service: otel_service.clone(),
                metrics_store: Some(metrics_store.clone()),
                metrics_write_tx: Some(metrics_write_tx),
                facet_service: facet_service.clone(),
                dashboard_service: dashboard_service.clone(),
                metric_alert_service: metric_alert_service.clone(),
                metric_alert_evaluator: metric_alert_evaluator.clone(),
                audit_service: audit_service.clone(),
                trace_hint_tx: Some(trace_hint_tx),
                cross_project_service: cross_project_service.clone(),
                otel_relay_tx: Some(otel_relay_tx),
                project_access_checker: None,
            };
            context.register_service(Arc::new(app_state.clone()));

            // ── Background Tasks ────────────────────────────────────

            // 1. Retention cleanup task
            //
            // `apply_retention` is now a no-op — the OTel hypertables have
            // a native `add_retention_policy(..., INTERVAL '90 days')`
            // registered in `m20260225_000001_create_otel_tables`, which
            // Timescale enforces via `drop_chunks` (atomic, chunk-aware,
            // race-free). We keep the loop here so any future per-project
            // retention logic has a hook, but it does no DB work today.
            //
            // We also skip the first `tick()` because `tokio::interval`
            // fires immediately on creation, which would race with anything
            // else still finishing during startup. Future hooks should
            // wait one full interval before their first run.
            let retention_storage = storage.clone();
            let retention_days = config.retention_days;
            let retention_interval = config.retention_check_interval_secs;
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(retention_interval));
                interval.tick().await; // discard the immediate first tick
                loop {
                    interval.tick().await;
                    debug!(retention_days, "Running OTel data retention cleanup");
                    if let Err(e) = apply_retention_all(&retention_storage, retention_days).await {
                        error!(error = %e, "OTel retention cleanup failed");
                    }
                }
            });

            // 1a2. Facet backfill/clear poller.
            //
            // Advances every non-terminal facet (pending/running/deleting) by
            // one bounded unit of work per tick — see
            // `FacetService::advance_pending_facets` for why this is a poller
            // rather than a task spawned from the create/delete HTTP handlers
            // (a handler-spawned task's progress would be lost on a process
            // restart; this poller's progress lives entirely in the
            // `otel_span_facets` row, so a restart just resumes).
            //
            // 5s keeps facet creation feeling responsive (an admin pinning an
            // attribute sees `running` within a few seconds) without adding
            // meaningful load: each tick is a handful of cheap Postgres/CH
            // status queries plus at most one bounded batch/mutation per
            // in-flight facet, and there are at most 20 facets ever (one per
            // slot).
            const FACET_POLL_INTERVAL_SECS: u64 = 5;
            let facet_poller_service = facet_service.clone();
            tokio::spawn(async move {
                let mut interval =
                    tokio::time::interval(Duration::from_secs(FACET_POLL_INTERVAL_SECS));
                interval.tick().await; // discard the immediate first tick
                loop {
                    interval.tick().await;
                    facet_poller_service.advance_pending_facets().await;
                }
            });

            // 1b. Background consumer for bounded OTLP → MetricsStore writes.
            // Drains the metrics_write_rx channel and calls write_batch one
            // batch at a time, consuming at most 1 DB connection continuously.
            // If the channel is drained and the sender is dropped, this task
            // exits cleanly.
            {
                let write_store = metrics_store.clone();
                tokio::spawn(async move {
                    info!("OTLP metrics write consumer started");
                    while let Some(batch) = metrics_write_rx.recv().await {
                        if let Err(e) = write_store.write_batch(batch).await {
                            tracing::warn!("OTLP metrics store write failed (non-fatal): {e}");
                        }
                    }
                    info!("OTLP metrics write consumer stopped (channel closed)");
                });
            }

            // 1c. ADR-027 Phase 0: cross-project trace hint writer consumer.
            //
            // Drains `trace_hint_rx` and calls `CrossProjectTraceService::record_hint`
            // for each message, issuing a single multi-row INSERT ON CONFLICT DO NOTHING.
            // Errors are warned and the loop continues — hint loss is tolerable.
            {
                let hint_svc = cross_project_service.clone();
                tokio::spawn(async move {
                    info!("Cross-project trace hint writer consumer started");
                    while let Some(msg) = trace_hint_rx.recv().await {
                        if let Err(e) = hint_svc.record_hint(msg.trace_ids, msg.project_id).await {
                            tracing::warn!(
                                project_id = msg.project_id,
                                error = %e,
                                "Cross-project trace hint write failed (non-fatal); \
                                 subsequent ingests will re-populate via ON CONFLICT DO NOTHING"
                            );
                        }
                    }
                    info!("Cross-project trace hint writer consumer stopped (channel closed)");
                });
            }

            // 1c-relay. Background consumer for the OtelRelay extension point.
            //
            // Drains `otel_relay_rx` and calls `relay_slot.relay(msg)` for
            // each batch, dispatching to whichever `OtelRelay` implementation
            // was registered by a plugin (NoopOtelRelay when none registered).
            // Errors are not possible here (relay is infallible by contract).
            // The task exits cleanly when all senders drop.
            {
                tokio::spawn(async move {
                    info!("OTel relay consumer started");
                    while let Some(msg) = otel_relay_rx.recv().await {
                        relay_slot.relay(msg).await;
                    }
                    info!("OTel relay consumer stopped (channel closed)");
                });
            }

            // 1c-stats. Background sampler: OTel pipeline stats → MetricsStore.
            //
            // Reads `otel_service.pipeline_stats()` every 60 seconds, computes
            // the delta since the previous sample, and writes one counter point
            // per field to the unified MetricsStore (SourceKind::Node, node_id 0):
            //
            //   otel.rate_limited_requests   — ingest rejections from the rate limiter
            //   otel.quota_exceeded_requests — ingest rejections from quota enforcement
            //   otel.metrics_received / _stored / _dropped
            //   otel.spans_received   / _stored / _dropped
            //   otel.logs_received    / _stored_db / _stored_s3 / _dropped
            //   otel.ingest_errors    — storage writes that failed after retries
            //
            // The received/stored/dropped triplets are what make a data-loss
            // incident visible: `dropped > 0` (or `received - stored` drifting)
            // is the signal that batches are being thrown away, which was
            // previously only observable by reading the process's own logs.
            //
            // Using delta values (not cumulative) matches the proxy metrics sampler
            // pattern: each store point represents "events in the last N seconds"
            // so the AlertEvaluator threshold (e.g. "> 10") is intuitive to an
            // operator ("more than 10 dropped spans this sample window").
            //
            // The 60-second interval is independent of `monitoring.scrape_interval_secs`
            // because the pipeline stats are process-internal counters rather than
            // externally-scraped ones; re-reading the config each cycle would add an
            // unnecessary DB round-trip on an ingest-hot path.
            //
            // Every cycle writes every point, even when the delta is zero. The
            // AlertEvaluator's `query_latest` only looks back 15 minutes
            // (`LATEST_WINDOW`), so if a burst of drops is followed by
            // silence, skipping the zero-delta write would leave that burst's
            // non-zero point as the "latest" value for up to 15 minutes,
            // keeping the alarm falsely active. Always writing — including
            // zeros — lets the metric self-resolve on the next cycle, exactly
            // like the proxy sampler's fixed-size point set does. The storage
            // cost is a fixed 18 rows per minute (15 counters plus three
            // resource-limit gauges) regardless of ingest volume.
            {
                let stats_otel_service = otel_service.clone();
                let stats_metrics_store = metrics_store.clone();
                let stats_memory_profile = config.memory_profile;
                let stats_ingest_limit = config.max_concurrent_ingest_requests;
                tokio::spawn(async move {
                    use chrono::Utc;
                    use std::collections::HashMap;
                    use temps_metrics::{MetricKind, MetricPoint, SourceKind};

                    info!(
                        "OTel pipeline stats sampler started (interval={}s)",
                        OTEL_STATS_SAMPLE_INTERVAL_SECS
                    );
                    let mut interval =
                        tokio::time::interval(Duration::from_secs(OTEL_STATS_SAMPLE_INTERVAL_SECS));
                    interval.tick().await; // discard the immediate first tick

                    // Previous-cycle checkpoint for every sampled counter. Held
                    // in one struct so `PipelineStats` and the checkpoints can
                    // never drift apart field-by-field.
                    let mut prev = crate::types::PipelineStats::default();

                    loop {
                        interval.tick().await;
                        let snap = stats_otel_service.pipeline_stats();

                        let deltas = pipeline_stat_deltas(&snap, &prev);

                        let now = Utc::now();
                        let mut points: Vec<MetricPoint> = deltas
                            .iter()
                            .map(|(name, delta)| MetricPoint {
                                time: now,
                                source_kind: SourceKind::Node,
                                source_id: CONTROL_PLANE_NODE_ID,
                                name: (*name).to_string(),
                                value: *delta as f64,
                                kind: MetricKind::Counter,
                                engine: Some("otel".to_string()),
                                environment: None,
                                node_id: Some(CONTROL_PLANE_NODE_ID),
                                labels: HashMap::new(),
                            })
                            .collect();
                        points.extend([
                            MetricPoint {
                                time: now,
                                source_kind: SourceKind::Node,
                                source_id: CONTROL_PLANE_NODE_ID,
                                name: "otel.effective_memory_limit_bytes".to_string(),
                                value: stats_memory_profile.effective_memory_bytes as f64,
                                kind: MetricKind::Gauge,
                                engine: Some("otel".to_string()),
                                environment: None,
                                node_id: Some(CONTROL_PLANE_NODE_ID),
                                labels: HashMap::new(),
                            },
                            MetricPoint {
                                time: now,
                                source_kind: SourceKind::Node,
                                source_id: CONTROL_PLANE_NODE_ID,
                                name: "otel.ingest_concurrency_limit".to_string(),
                                value: stats_ingest_limit as f64,
                                kind: MetricKind::Gauge,
                                engine: Some("otel".to_string()),
                                environment: None,
                                node_id: Some(CONTROL_PLANE_NODE_ID),
                                labels: HashMap::new(),
                            },
                            MetricPoint {
                                time: now,
                                source_kind: SourceKind::Node,
                                source_id: CONTROL_PLANE_NODE_ID,
                                name: "otel.relay_buffer_limit_bytes".to_string(),
                                value: stats_memory_profile.relay_queue_max_bytes as f64,
                                kind: MetricKind::Gauge,
                                engine: Some("otel".to_string()),
                                environment: None,
                                node_id: Some(CONTROL_PLANE_NODE_ID),
                                labels: HashMap::new(),
                            },
                        ]);
                        let point_count = points.len();

                        // Only advance the checkpoints once the write actually lands.
                        // Advancing them unconditionally would discard this cycle's
                        // deltas forever on a transient store failure; leaving them
                        // in place means the next successful write's delta widens to
                        // cover the missed cycle too, so no rejection is lost from
                        // the series.
                        if let Err(e) = stats_metrics_store.write_batch(points).await {
                            warn!(
                                error = %e,
                                "OTel pipeline stats write failed (non-fatal); \
                                 checkpoints held back so the next successful write \
                                 includes this cycle's deltas"
                            );
                        } else {
                            let spans_dropped =
                                snap.spans_dropped.saturating_sub(prev.spans_dropped);
                            let ingest_errors =
                                snap.ingest_errors.saturating_sub(prev.ingest_errors);
                            prev = snap;
                            debug!(
                                points = point_count,
                                spans_dropped,
                                ingest_errors,
                                "OTel pipeline stats sampled and written"
                            );
                        }
                    }
                });
            }

            // 1d. ADR-027 Phase 0: daily prune of POSTGRES cross_project_trace_refs
            //     rows older than 90 days (matching the OTel span TTL on both
            //     backends). Runs unconditionally: on the TimescaleDB backend it
            //     is the retention mechanism; on the ClickHouse backend (where
            //     new refs expire via native per-row TTL) it drains the legacy
            //     Postgres rows written before the cutover and becomes a no-op
            //     after one retention window.
            //
            // Deliberately uses a periodic tokio::spawn loop rather than a
            // Job enum variant to keep the scheduler dependency minimal.
            // First run is after a 24-hour delay so it doesn't compete with
            // startup DB activity.
            {
                let prune_db = db.clone();
                tokio::spawn(async move {
                    let interval = Duration::from_secs(24 * 60 * 60); // 24 hours
                    loop {
                        tokio::time::sleep(interval).await;
                        match prune_stale_hints(&prune_db).await {
                            Ok(deleted) => info!(
                                deleted,
                                "Cross-project trace hint prune completed \
                                 (rows older than 90 days removed)"
                            ),
                            Err(e) => tracing::warn!(
                                error = %e,
                                "Cross-project trace hint prune failed (non-fatal); \
                                 will retry in 24 hours"
                            ),
                        }
                    }
                });
            }

            // 2. Health compute service
            if config.enable_health_compute {
                let health_service = Arc::new(HealthComputeService::new(storage.clone()));
                tokio::spawn(async move {
                    info!("Starting OTel health compute service");
                    // Start with empty project list; the service will discover projects
                    // from stored data. In a future iteration, we could query the projects table.
                    health_service.start(vec![]).await;
                });
            }

            // 4. Anomaly detector
            if config.enable_anomaly_detection {
                let detector = Arc::new(AnomalyDetector::new(
                    storage.clone(),
                    AnomalyDetectorConfig::default(),
                ));
                tokio::spawn(async move {
                    info!("Starting OTel anomaly detector");
                    detector.start(vec![]).await;
                });
            }

            // Spawn the metric alert evaluator run loop (evaluator already created above).
            {
                let evaluator = metric_alert_evaluator;
                tokio::spawn(async move {
                    evaluator.run().await;
                });
            }

            debug!(
                retention_days = config.retention_days,
                rate_limit = config.rate_limit_requests,
                s3_enabled = config.has_s3_config(),
                "OTel plugin services registered successfully"
            );
            Ok(())
        })
    }

    fn initialize_plugin_services<'a>(
        &'a self,
        context: &'a PluginContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Runs after every plugin has registered its services, so this is
            // the first point at which an optional plugin-provided
            // RetentionResolver (e.g. from a plugin implementing per-project
            // retention) can actually be found.
            if let Some(slot) = self.retention_resolver_slot.get() {
                if let Some(resolver) = context.get_service::<dyn temps_core::RetentionResolver>() {
                    if slot.set(resolver) {
                        debug!("otel: RetentionResolver wired in from a registered plugin");
                    } else {
                        tracing::warn!(
                            "otel: RetentionResolver slot was already claimed; \
                             this plugin's resolver was NOT installed. \
                             Check plugin registration order."
                        );
                    }
                }
            }

            // Wire in an optional OtelRelay implementation registered by a
            // plugin. When no plugin registers one, the slot stays loaded with
            // NoopOtelRelay and the relay background consumer is a cheap no-op.
            if let Some(slot) = self.relay_slot.get() {
                if let Some(relay) = context.get_service::<dyn crate::relay::OtelRelay>() {
                    if slot.set(relay) {
                        debug!("otel: OtelRelay wired in from a registered plugin");
                    } else {
                        tracing::warn!(
                            "otel: OtelRelay slot was already claimed; \
                             this plugin's relay was NOT installed. \
                             Check plugin registration order."
                        );
                    }
                }
            }

            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let app_state_arc = context.require_service::<OtelAppState>();
        let mut app_state: OtelAppState = app_state_arc.as_ref().clone();
        app_state.project_access_checker =
            context.get_service::<dyn temps_core::ProjectAccessChecker>();

        // Same checker feeds the `tk_`-key ingest auth path, so team-based
        // project access is enforced on writes exactly as on reads.
        if let Some(checker) = app_state.project_access_checker.clone() {
            context
                .require_service::<OtelAuthService>()
                .set_project_access_checker(checker);
        }

        let router = handlers::configure_routes().with_state(app_state);

        Some(PluginRoutes::new(router))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<OtelApiDoc as OpenApiTrait>::openapi())
    }
}

/// Read the ClickHouse connection config for the OTel backend from the same
/// `TEMPS_CLICKHOUSE_*` environment variables that `ServerConfig` uses.
///
/// Returns `Some(config)` only when all four variables are set and non-empty
/// (fail-closed: partial configuration is treated as disabled). Returns `None`
/// when ClickHouse is not configured, preserving the default TimescaleDB path.
fn read_clickhouse_otel_config_from_env() -> Option<ClickHouseOtelConfig> {
    let url = std::env::var("TEMPS_CLICKHOUSE_URL")
        .ok()
        .filter(|s| !s.is_empty())?;
    // Database name defaults to "temps" (consistent with ServerConfig) so all
    // ClickHouse-backed telemetry shares one database. Operators set only
    // URL/USER/PASSWORD; TEMPS_CLICKHOUSE_DATABASE overrides the name if desired.
    let database = std::env::var("TEMPS_CLICKHOUSE_DATABASE")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "temps".to_string());
    let user = std::env::var("TEMPS_CLICKHOUSE_USER")
        .ok()
        .filter(|s| !s.is_empty())?;
    let password = std::env::var("TEMPS_CLICKHOUSE_PASSWORD")
        .ok()
        .filter(|s| !s.is_empty())?;
    Some(ClickHouseOtelConfig::new(url, database, user, password))
}

/// Apply retention across all projects by scanning the tables for distinct project IDs.
async fn apply_retention_all(
    storage: &Arc<dyn crate::storage::OtelStorage>,
    retention_days: u32,
) -> Result<(), crate::error::OtelError> {
    // Get distinct project IDs from metric names (lightweight query)
    // In a production system, you'd have a dedicated project registry.
    // For now, we apply retention for project_id=0 which acts as a global sweep
    // using the configured retention_days.
    let deleted = storage.apply_retention(0).await?;
    if deleted > 0 {
        info!(deleted, retention_days, "OTel retention cleanup completed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_otel_plugin_name() {
        let plugin = OtelPlugin::new();
        assert_eq!(plugin.name(), "otel");
    }

    // ── Pipeline-stats sampler ──────────────────────────────────────────

    /// Every counter in `PipelineStats` must get a series; a field added to
    /// the struct without a matching entry here is a metric that silently
    /// never gets published.
    #[test]
    fn test_pipeline_stat_deltas_covers_every_counter() {
        let zero = crate::types::PipelineStats::default();
        let snap = crate::types::PipelineStats {
            metrics_received: 1,
            metrics_stored: 2,
            metrics_dropped: 3,
            spans_received: 4,
            spans_stored: 5,
            spans_dropped: 6,
            logs_received: 7,
            logs_stored_db: 8,
            logs_stored_s3: 9,
            logs_dropped: 10,
            ingest_errors: 11,
            rate_limited_requests: 12,
            quota_exceeded_requests: 13,
            relay_dropped_batches: 14,
            relay_dropped_items: 15,
        };

        let deltas = pipeline_stat_deltas(&snap, &zero);
        assert_eq!(deltas.len(), OTEL_PIPELINE_STAT_COUNT);

        let by_name: std::collections::HashMap<&str, u64> = deltas.iter().copied().collect();
        assert_eq!(by_name.len(), OTEL_PIPELINE_STAT_COUNT, "duplicate names");
        assert_eq!(by_name["otel.metrics_received"], 1);
        assert_eq!(by_name["otel.metrics_stored"], 2);
        assert_eq!(by_name["otel.metrics_dropped"], 3);
        assert_eq!(by_name["otel.spans_received"], 4);
        assert_eq!(by_name["otel.spans_stored"], 5);
        assert_eq!(by_name["otel.spans_dropped"], 6);
        assert_eq!(by_name["otel.logs_received"], 7);
        assert_eq!(by_name["otel.logs_stored_db"], 8);
        assert_eq!(by_name["otel.logs_stored_s3"], 9);
        assert_eq!(by_name["otel.logs_dropped"], 10);
        assert_eq!(by_name["otel.ingest_errors"], 11);
        assert_eq!(by_name["otel.rate_limited_requests"], 12);
        assert_eq!(by_name["otel.quota_exceeded_requests"], 13);
        assert_eq!(by_name["otel.relay_dropped_batches"], 14);
        assert_eq!(by_name["otel.relay_dropped_items"], 15);
    }

    /// The writer's names and the reader's published list must be identical,
    /// in the same order. If this fails, the sampler is writing a series the
    /// history endpoint will never chart (or vice versa) — silently.
    #[test]
    fn test_pipeline_stat_deltas_match_metric_names() {
        let zero = crate::types::PipelineStats::default();
        let written: Vec<&str> = pipeline_stat_deltas(&zero, &zero)
            .iter()
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(written, OTEL_PIPELINE_METRIC_NAMES.to_vec());
    }

    /// Names must all share the existing `otel.` prefix so the series sit
    /// together in the metric picker alongside the two originals.
    #[test]
    fn test_pipeline_stat_delta_names_share_the_otel_prefix() {
        let zero = crate::types::PipelineStats::default();
        for (name, _) in pipeline_stat_deltas(&zero, &zero) {
            assert!(name.starts_with("otel."), "unprefixed metric name: {name}");
        }
    }

    /// Deltas are relative to the previous checkpoint, not cumulative.
    #[test]
    fn test_pipeline_stat_deltas_are_relative_to_checkpoint() {
        let prev = crate::types::PipelineStats {
            spans_received: 100,
            spans_dropped: 10,
            ..Default::default()
        };
        let snap = crate::types::PipelineStats {
            spans_received: 175,
            spans_dropped: 12,
            ..Default::default()
        };

        let by_name: std::collections::HashMap<&str, u64> =
            pipeline_stat_deltas(&snap, &prev).iter().copied().collect();
        assert_eq!(by_name["otel.spans_received"], 75);
        assert_eq!(by_name["otel.spans_dropped"], 2);
    }

    /// A quiet window must still produce a full set of zero-valued points so
    /// the AlertEvaluator's 15-minute lookback can self-resolve rather than
    /// keeping a stale burst as the "latest" value.
    #[test]
    fn test_pipeline_stat_deltas_emit_zeros_when_idle() {
        let snap = crate::types::PipelineStats {
            spans_received: 500,
            spans_stored: 500,
            ..Default::default()
        };
        let deltas = pipeline_stat_deltas(&snap, &snap);
        assert_eq!(deltas.len(), OTEL_PIPELINE_STAT_COUNT);
        assert!(deltas.iter().all(|(_, delta)| *delta == 0));
    }

    /// A process restart zeroes the atomics while the checkpoint still holds
    /// the pre-restart totals; that must clamp to 0, never underflow.
    #[test]
    fn test_pipeline_stat_deltas_clamp_on_counter_reset() {
        let prev = crate::types::PipelineStats {
            spans_received: 9_000,
            ingest_errors: 42,
            ..Default::default()
        };
        let snap = crate::types::PipelineStats::default();

        let by_name: std::collections::HashMap<&str, u64> =
            pipeline_stat_deltas(&snap, &prev).iter().copied().collect();
        assert_eq!(by_name["otel.spans_received"], 0);
        assert_eq!(by_name["otel.ingest_errors"], 0);
    }

    #[test]
    fn test_otel_plugin_default() {
        let plugin = OtelPlugin::default();
        assert_eq!(plugin.name(), "otel");
    }

    #[test]
    fn test_otel_config_default() {
        let config = OtelConfig::default();
        assert_eq!(config.retention_days, 7);
        assert_eq!(config.rate_limit_requests, 1000);
        assert_eq!(config.rate_limit_window_secs, 60);
        assert_eq!(config.quota_bytes_per_project, None);
        assert!(!config.has_s3_config());
        assert!(config.enable_health_compute);
        assert!(config.enable_anomaly_detection);
        assert_eq!(
            config.max_concurrent_ingest_requests,
            crate::services::otel_service::DEFAULT_MAX_CONCURRENT_INGEST_REQUESTS
        );
    }

    #[test]
    fn test_parse_max_concurrent_ingest_requests_accepts_positive_integers() {
        assert_eq!(parse_max_concurrent_ingest_requests("1"), Some(1));
        assert_eq!(parse_max_concurrent_ingest_requests("128"), Some(128));
    }

    #[test]
    fn test_parse_max_concurrent_ingest_requests_rejects_zero_and_garbage() {
        assert_eq!(parse_max_concurrent_ingest_requests("0"), None);
        assert_eq!(parse_max_concurrent_ingest_requests("-1"), None);
        assert_eq!(parse_max_concurrent_ingest_requests("not-a-number"), None);
        assert_eq!(parse_max_concurrent_ingest_requests(""), None);
    }

    #[test]
    fn test_parse_max_concurrent_ingest_requests_rejects_values_above_semaphore_max() {
        // A value that parses as `usize` but exceeds `Semaphore::MAX_PERMITS`
        // must fall back to the default rather than panicking `Semaphore::new`
        // at startup — the regression this fix targets.
        let too_large = (tokio::sync::Semaphore::MAX_PERMITS as u128 + 1).to_string();
        assert_eq!(parse_max_concurrent_ingest_requests(&too_large), None);
        assert_eq!(
            parse_max_concurrent_ingest_requests(&usize::MAX.to_string()),
            None
        );
        assert_eq!(
            parse_max_concurrent_ingest_requests(&tokio::sync::Semaphore::MAX_PERMITS.to_string()),
            Some(tokio::sync::Semaphore::MAX_PERMITS)
        );
    }

    #[test]
    fn test_otel_config_has_s3_config() {
        let mut config = OtelConfig::default();
        assert!(!config.has_s3_config());

        config.s3_region = Some("us-east-1".into());
        assert!(!config.has_s3_config());

        config.s3_access_key = Some("AKIA...".into());
        config.s3_secret_key = Some("secret".into());
        assert!(!config.has_s3_config());

        config.s3_bucket = Some("my-bucket".into());
        assert!(config.has_s3_config());
    }

    #[test]
    fn test_otel_openapi_schema_is_some() {
        let plugin = OtelPlugin::new();
        let schema = plugin.openapi_schema();
        assert!(schema.is_some());
    }
}
