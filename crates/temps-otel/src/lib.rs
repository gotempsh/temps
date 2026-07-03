//! OpenTelemetry data collection, storage, and analysis for Temps.
//!
//! This crate implements:
//! - OTLP/HTTP ingest endpoints (metrics, traces, logs)
//! - Pluggable storage backend (default: TimescaleDB)
//! - Tail-based trace sampling
//! - Anomaly detection with time-aware baselines
//! - Pre-computed health summaries for the monitoring page
//! - OTel collector sidecar injection for deployed containers
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────┐    ┌──────────┐    ┌─────────────────┐
//! │ OTel SDK /   │───▶│ Ingest   │───▶│ OtelService     │
//! │ Collector    │    │ Handlers │    │ (orchestrates)  │
//! └─────────────┘    └──────────┘    └────────┬────────┘
//!                                             │
//!                    ┌────────────────────────┤
//!                    │                        │
//!              ┌─────▼──────┐         ┌──────▼────────┐
//!              │ Storage    │         │ Sampler       │
//!              │ Trait      │         │ (tail-based)  │
//!              └─────┬──────┘         └───────────────┘
//!                    │
//!              ┌─────▼──────┐
//!              │ TimescaleDB│  (or future: ClickHouse, etc.)
//!              └────────────┘
//! ```

pub mod anomaly;
pub mod detectors;
pub mod error;
pub mod handlers;
pub mod ingest;
pub mod plugin;
pub mod proto;
pub mod services;
pub mod sidecar;
pub mod storage;
pub mod types;

pub use error::OtelError;
pub use plugin::OtelPlugin;
pub use services::OtelService;
pub use storage::OtelStorage;

#[cfg(test)]
pub mod test_support;

/// Application state shared across OTel HTTP handlers.
#[derive(Clone)]
pub struct OtelAppState {
    pub otel_service: std::sync::Arc<OtelService>,
    /// Optional unified metrics store for routing OTLP metrics alongside
    /// scraper-collected metrics (DB stats, container stats, node stats).
    /// When `None`, OTLP metrics are still stored in the OTel-specific tables
    /// but are not forwarded to `service_metrics`.
    pub metrics_store: Option<std::sync::Arc<dyn temps_metrics::MetricsStore>>,
    /// Bounded sender for fire-and-forget MetricsStore writes from OTLP ingest.
    ///
    /// Using a bounded channel (rather than unbounded `tokio::spawn`) provides
    /// backpressure: when `service_metrics` writes are slow, the channel fills
    /// and `try_send` drops new batches gracefully instead of accumulating
    /// unbounded in-flight tasks and pool connections.
    ///
    /// # SECURITY(metrics-security-4): source_id trust
    ///
    /// The `deployment_id` written through this channel is derived exclusively
    /// from the authenticated token context (resolved in `resolve_ingest_context`),
    /// never from the OTLP payload body.  Any `temps.*` resource attributes in
    /// the payload are ignored by `otlp_to_store_point` — source assignment
    /// is an invariant of the ingest path, not a user-controlled field.
    pub metrics_write_tx: Option<tokio::sync::mpsc::Sender<Vec<temps_metrics::MetricPoint>>>,
    /// Service backing per-project saved metric dashboard CRUD (Postgres-backed
    /// config/metadata, distinct from the ClickHouse/Timescale `OtelStorage`).
    pub dashboard_service: std::sync::Arc<crate::services::MetricDashboardService>,
    /// Service backing first-class metric alert rule CRUD (Postgres-backed
    /// config/metadata; evaluated by the background `MetricAlertEvaluator`).
    pub metric_alert_service: std::sync::Arc<crate::services::MetricAlertService>,
    /// The background metric alert evaluator, shared so read handlers can snapshot
    /// its in-memory per-series firing state (ADR-026 Phase 3 `firing_series`).
    pub metric_alert_evaluator: std::sync::Arc<crate::services::MetricAlertEvaluator>,
    /// Audit logger for dashboard/alert write operations (best-effort, non-fatal).
    pub audit_service: std::sync::Arc<dyn temps_core::AuditLogger>,
}
