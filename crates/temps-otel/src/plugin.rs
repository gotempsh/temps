//! Plugin registration for the OTel subsystem.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use tracing::{debug, error, info};
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::anomaly::detector::{AnomalyDetector, AnomalyDetectorConfig};
use crate::handlers;
use crate::handlers::dashboard_handler;
use crate::handlers::ingest_handler;
use crate::handlers::metric_alert_handler;
use crate::handlers::query_handler;
use crate::ingest::auth::OtelAuthService;
use crate::ingest::rate_limit::RateLimiter;
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

    // Quota
    pub quota_bytes_per_project: u64,

    // Background tasks
    pub enable_health_compute: bool,
    pub enable_anomaly_detection: bool,
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
            quota_bytes_per_project: 10 * 1024 * 1024 * 1024, // 10 GB
            enable_health_compute: true,
            enable_anomaly_detection: true,
        }
    }
}

impl OtelConfig {
    /// Read configuration from `TEMPS_OTEL_*` environment variables.
    pub fn from_env() -> Self {
        let mut config = Self::default();

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
            if let Ok(gb) = v.parse::<u64>() {
                config.quota_bytes_per_project = gb * 1024 * 1024 * 1024;
            }
        }
        if let Ok(v) = std::env::var("TEMPS_OTEL_ENABLE_HEALTH_COMPUTE") {
            config.enable_health_compute = v != "0" && v != "false";
        }
        if let Ok(v) = std::env::var("TEMPS_OTEL_ENABLE_ANOMALY_DETECTION") {
            config.enable_anomaly_detection = v != "0" && v != "false";
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
        query_handler::get_trace,
        query_handler::query_logs,
        query_handler::list_insights,
        query_handler::get_health,
        query_handler::get_quota,
        query_handler::get_pipeline_stats,
        query_handler::query_genai_traces,
        query_handler::get_genai_trace,
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
            query_handler::LogsResponse,
            query_handler::InsightsResponse,
            query_handler::HealthResponse,
            query_handler::QuotaResponse,
            query_handler::PipelineStatsResponse,
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
        (name = "GenAI", description = "GenAI agent activity tracing endpoints")
    )
)]
pub struct OtelApiDoc;

// ── Plugin ──────────────────────────────────────────────────────────

/// OTel Plugin for Temps.
pub struct OtelPlugin;

impl OtelPlugin {
    pub fn new() -> Self {
        Self
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

            // TimescaleDbStorage is always constructed: it is the sole
            // backend when CH is disabled, and the inner delegate when
            // CH is enabled.
            let timescale_storage = Arc::new(TimescaleDbStorage::with_config(
                db.clone(),
                s3_client,
                config.retention_days,
                config.quota_bytes_per_project,
            ));

            let storage: Arc<dyn crate::storage::OtelStorage> = if let Some(ch_cfg) = ch_config {
                info!(
                    url = %ch_cfg.url,
                    database = %ch_cfg.database,
                    "ClickHouse OTel backend enabled (ADR-016) — applying migrations"
                );
                let ch_storage = Arc::new(ClickHouseOtelStorage::new(
                    ch_cfg.clone(),
                    timescale_storage,
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

            // Create auth service
            let auth_service = Arc::new(OtelAuthService::new(db.clone()));

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

            // Metric dashboards + alert rules: Postgres-backed config/metadata
            // services plus the global audit logger for write operations.
            let dashboard_service =
                Arc::new(crate::services::MetricDashboardService::new(db.clone()));
            let metric_alert_service =
                Arc::new(crate::services::MetricAlertService::new(db.clone()));
            let audit_service = context.require_service::<dyn temps_core::AuditLogger>();

            // Create app state for handlers
            let app_state = OtelAppState {
                otel_service: otel_service.clone(),
                metrics_store: Some(metrics_store.clone()),
                metrics_write_tx: Some(metrics_write_tx),
                dashboard_service: dashboard_service.clone(),
                metric_alert_service: metric_alert_service.clone(),
                audit_service: audit_service.clone(),
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
            {
                let notification_service =
                    context.require_service::<dyn temps_core::notifications::NotificationService>();
                let job_queue = context.require_service::<dyn temps_core::JobQueue>();
                let alarm_service = Arc::new(temps_monitoring::AlarmService::new(
                    db.clone(),
                    notification_service,
                    job_queue,
                ));
                // ADR-022: optional general AI foundation, registered by the AI
                // gateway plugin when present. Absent -> deterministic Tier-1 text.
                let ai = context.get_service::<dyn temps_ai::AiService>();
                let evaluator = Arc::new(crate::services::MetricAlertEvaluator::new(
                    metric_alert_service.clone(),
                    otel_service.clone(),
                    alarm_service,
                    db.clone(),
                    ai,
                ));
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

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let app_state_arc = context.require_service::<OtelAppState>();
        let app_state: OtelAppState = app_state_arc.as_ref().clone();

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

    #[test]
    fn test_otel_plugin_default() {
        let plugin = OtelPlugin;
        assert_eq!(plugin.name(), "otel");
    }

    #[test]
    fn test_otel_config_default() {
        let config = OtelConfig::default();
        assert_eq!(config.retention_days, 7);
        assert_eq!(config.rate_limit_requests, 1000);
        assert_eq!(config.rate_limit_window_secs, 60);
        assert_eq!(config.quota_bytes_per_project, 10 * 1024 * 1024 * 1024);
        assert!(!config.has_s3_config());
        assert!(config.enable_health_compute);
        assert!(config.enable_anomaly_detection);
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
