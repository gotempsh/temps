//! Core OTel services.

pub mod anomaly_preview;
pub mod dashboard_service;
pub mod health_service;
pub mod metric_alert_evaluator;
pub mod metric_alert_service;
pub mod otel_service;
/// `temps_core::TraceReader` impl for `OtelService` (storage-agnostic read API).
pub mod trace_reader;

pub use dashboard_service::MetricDashboardService;
pub use health_service::HealthComputeService;
pub use metric_alert_evaluator::MetricAlertEvaluator;
pub use metric_alert_service::MetricAlertService;
pub use otel_service::OtelService;
