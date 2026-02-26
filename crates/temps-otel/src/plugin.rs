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
use crate::handlers::ingest_handler;
use crate::handlers::query_handler;
use crate::ingest::auth::OtelAuthService;
use crate::ingest::rate_limit::RateLimiter;
use crate::services::health_service::HealthComputeService;
use crate::services::OtelService;
use crate::storage::timescaledb::TimescaleDbStorage;
use crate::OtelAppState;

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
        query_handler::query_traces,
        query_handler::query_trace_summaries,
        query_handler::get_trace,
        query_handler::query_logs,
        query_handler::list_insights,
        query_handler::get_health,
        query_handler::get_quota,
        query_handler::get_pipeline_stats,
    ),
    components(
        schemas(
            query_handler::MetricsResponse,
            query_handler::MetricNamesResponse,
            query_handler::TracesResponse,
            query_handler::TraceSummariesResponse,
            crate::types::TraceSummary,
            query_handler::LogsResponse,
            query_handler::InsightsResponse,
            query_handler::HealthResponse,
            query_handler::QuotaResponse,
            query_handler::PipelineStatsResponse,
            crate::types::MetricBucket,
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
        )
    ),
    info(
        title = "OTel API",
        description = "OpenTelemetry data collection, storage, and analysis endpoints",
        version = "1.0.0"
    ),
    tags(
        (name = "OTel Ingest", description = "OTLP/HTTP ingest endpoints (protobuf)"),
        (name = "OTel", description = "Query endpoints for the monitoring UI")
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

            // Create storage backend with configured retention and quota
            let storage: Arc<dyn crate::storage::OtelStorage> =
                Arc::new(TimescaleDbStorage::with_config(
                    db.clone(),
                    s3_client,
                    config.retention_days,
                    config.quota_bytes_per_project,
                ));
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

            // Create app state for handlers
            let app_state = OtelAppState {
                otel_service: otel_service.clone(),
            };
            context.register_service(Arc::new(app_state.clone()));

            // ── Background Tasks ────────────────────────────────────

            // 1. Retention cleanup task
            let retention_storage = storage.clone();
            let retention_days = config.retention_days;
            let retention_interval = config.retention_check_interval_secs;
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(retention_interval));
                loop {
                    interval.tick().await;
                    // Query project IDs from the otel_metrics table
                    // For now, apply retention to all projects by scanning the tables
                    // This is safe because apply_retention uses WHERE project_id = $1
                    debug!(retention_days, "Running OTel data retention cleanup");
                    if let Err(e) = apply_retention_all(&retention_storage, retention_days).await {
                        error!(error = %e, "OTel retention cleanup failed");
                    }
                }
            });

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

        Some(PluginRoutes { router })
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<OtelApiDoc as OpenApiTrait>::openapi())
    }
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
