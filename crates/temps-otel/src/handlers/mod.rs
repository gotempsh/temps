//! HTTP handlers for OTLP ingest and query endpoints.

pub mod audit;
pub mod dashboard_handler;
pub mod ingest_handler;
pub mod metric_alert_handler;
pub mod query_handler;

use axum::routing::{get, post};
use axum::Router;

use crate::OtelAppState;

/// Configure all OTel routes.
///
/// Ingest routes (OTLP/HTTP, header-based auth):
///   POST /otel/v1/metrics
///   POST /otel/v1/traces
///   POST /otel/v1/logs
///
/// Ingest routes (OTLP/HTTP, IDs in URL path):
///   POST /otel/v1/{project_id}/{environment_id}/{deployment_id}/metrics
///   POST /otel/v1/{project_id}/{environment_id}/{deployment_id}/traces
///   POST /otel/v1/{project_id}/{environment_id}/{deployment_id}/logs
///
/// Query routes (authenticated, for the monitoring UI):
///   GET /otel/metrics
///   GET /otel/metric-names
///   GET /otel/traces
///   GET /otel/traces/{trace_id}
///   GET /otel/logs
///   GET /otel/insights
///   GET /otel/health
///   GET /otel/quota
///   GET /otel/pipeline-stats
pub fn configure_routes() -> Router<OtelAppState> {
    Router::new()
        // OTLP ingest endpoints (header-based auth)
        .route("/otel/v1/metrics", post(ingest_handler::ingest_metrics))
        .route("/otel/v1/traces", post(ingest_handler::ingest_traces))
        .route("/otel/v1/logs", post(ingest_handler::ingest_logs))
        // OTLP ingest endpoints (project/environment/deployment in path)
        .route(
            "/otel/v1/{project_id}/{environment_id}/{deployment_id}/metrics",
            post(ingest_handler::ingest_metrics_by_path),
        )
        .route(
            "/otel/v1/{project_id}/{environment_id}/{deployment_id}/traces",
            post(ingest_handler::ingest_traces_by_path),
        )
        .route(
            "/otel/v1/{project_id}/{environment_id}/{deployment_id}/logs",
            post(ingest_handler::ingest_logs_by_path),
        )
        // Query endpoints
        .route("/otel/metrics", get(query_handler::query_metrics))
        .route(
            "/otel/metric-names/{project_id}",
            get(query_handler::list_metric_names),
        )
        .route(
            "/otel/metric-label-keys",
            get(query_handler::list_metric_label_keys),
        )
        .route(
            "/otel/metric-label-values",
            get(query_handler::list_metric_label_values),
        )
        .route("/otel/traces", get(query_handler::query_traces))
        .route(
            "/otel/trace-summaries",
            get(query_handler::query_trace_summaries),
        )
        .route(
            "/otel/traces/{project_id}/{trace_id}",
            get(query_handler::get_trace),
        )
        .route("/otel/logs", get(query_handler::query_logs))
        .route(
            "/otel/insights/{project_id}",
            get(query_handler::list_insights),
        )
        .route("/otel/health/{project_id}", get(query_handler::get_health))
        .route("/otel/quota/{project_id}", get(query_handler::get_quota))
        .route(
            "/otel/pipeline-stats",
            get(query_handler::get_pipeline_stats),
        )
        // GenAI agent activity endpoints
        .route("/otel/genai/traces", get(query_handler::query_genai_traces))
        .route(
            "/otel/genai/traces/{project_id}/{trace_id}",
            get(query_handler::get_genai_trace),
        )
        // Metric dashboards (per-project saved dashboard CRUD)
        .route(
            "/otel/dashboards",
            get(dashboard_handler::list_dashboards).post(dashboard_handler::create_dashboard),
        )
        .route(
            "/otel/dashboards/{id}",
            get(dashboard_handler::get_dashboard)
                .patch(dashboard_handler::update_dashboard)
                .delete(dashboard_handler::delete_dashboard),
        )
        // Metric alert rules (first-class metric-centric alerting)
        .route(
            "/otel/alerts",
            get(metric_alert_handler::list_alerts).post(metric_alert_handler::create_alert),
        )
        // Anomaly backtest/preview. Static segment registered before `{id}` so it
        // isn't captured as a rule id (matchit prefers static, but be explicit).
        .route(
            "/otel/alerts/preview",
            post(metric_alert_handler::preview_alert),
        )
        .route(
            "/otel/alerts/{id}",
            get(metric_alert_handler::get_alert)
                .patch(metric_alert_handler::update_alert)
                .delete(metric_alert_handler::delete_alert),
        )
}
