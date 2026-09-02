// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
pub mod memory;
pub mod plugin;
pub mod proto;
pub mod relay;
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
    /// Service managing OTel span attribute facets (Postgres config + ClickHouse
    /// DDL mutations + shared in-memory cache for the hot ingest/query paths).
    pub facet_service: std::sync::Arc<crate::services::FacetService>,
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
    /// Bounded sender for ADR-027 Phase 0 cross-project trace hint writes.
    ///
    /// After a successful span ingest, `do_ingest_traces` fires a
    /// `TraceHintMsg` here (non-blocking `try_send`).  A dedicated background
    /// consumer calls `CrossProjectTraceService::record_hint` to persist the
    /// `(trace_id, project_id)` discovery rows.  When the channel is full the
    /// hint is silently dropped — hint loss is non-fatal because a subsequent
    /// ingest batch for the same pair will re-insert via `ON CONFLICT DO NOTHING`.
    pub trace_hint_tx:
        Option<tokio::sync::mpsc::Sender<crate::services::cross_project::TraceHintMsg>>,
    /// Cross-project trace discovery service (ADR-027 Phases 1 & 2).
    ///
    /// Backs the `GET /otel/traces/cross-project/{trace_id}` (Phase 1 sibling
    /// banner) and `GET /otel/global/traces/{trace_id}` (Phase 2 unified
    /// waterfall) query handlers.
    pub cross_project_service: std::sync::Arc<crate::services::CrossProjectTraceService>,
    /// Bounded sender for fire-and-forget relay of decoded OTLP batches.
    ///
    /// After a successful decode in each `do_ingest_*` function, the handler
    /// sends an [`crate::relay::OtelRelayMessage`] here via `try_send` (never
    /// blocking). A dedicated background consumer calls
    /// [`crate::relay::OtelRelaySlot::relay`], which dispatches to whichever
    /// [`crate::relay::OtelRelay`] implementation was registered by a plugin
    /// (defaulting to the no-op). When the channel is full the batch is
    /// dropped and a warning is emitted — relay loss is non-fatal and
    /// must never add latency to the OTLP HTTP response.
    pub otel_relay_tx: Option<crate::relay::OtelRelayQueueSender>,
    /// Optional checker for team-based project access (human sessions only).
    pub project_access_checker: Option<std::sync::Arc<dyn temps_core::ProjectAccessChecker>>,
    /// Read side of the out-of-process Cloud telemetry backfill's progress
    /// record (ADR-040 §1).
    ///
    /// The backfill runs under `temps backfill cloud-telemetry`, not in this
    /// process, so this is how the Console learns a run exists at all. Required
    /// rather than optional: an endpoint that sometimes cannot answer "is a
    /// backfill running" is worse than one that always can.
    pub cloud_backfill_progress:
        std::sync::Arc<crate::services::cloud_backfill_progress::CloudBackfillProgressService>,
    /// Per-project telemetry write mode, its §1 gate and its interval ledger
    /// (ADR-041).
    ///
    /// Required rather than optional, for the same reason
    /// `cloud_backfill_progress` is: the write-mode control renders in every
    /// project's settings, including on an instance that has never linked Cloud
    /// — where it must onboard rather than disappear. An endpoint that
    /// sometimes cannot answer "where do this project's spans go" would make
    /// that impossible.
    pub telemetry_write_modes: std::sync::Arc<crate::services::TelemetryWriteModeService>,
    /// The Cloud link, read only for the §1 gate's prerequisites (linked,
    /// telemetry switch on, credential accepted) and for rendering them.
    ///
    /// `None` on a build that wires no Cloud integration at all, which resolves
    /// to "not linked" — the safe answer, and the one that produces an
    /// onboarding state rather than an error.
    pub cloud_link: Option<std::sync::Arc<temps_cloud_client::CloudLink>>,
    /// Durable state of bulk Cloud-telemetry activation jobs (ADR-042 §8).
    ///
    /// Required rather than optional, for the same reason
    /// `telemetry_write_modes` is: the activation card renders on every
    /// instance, including one that has never linked Cloud, and an endpoint
    /// that sometimes cannot answer "is an activation running" would make that
    /// impossible.
    pub bulk_activation: std::sync::Arc<crate::services::CloudBulkActivationService>,
    /// Where an activation estimate reads local span history from.
    ///
    /// The same source the worker ships from — chosen once, so the quote and
    /// the shipment can never disagree about which table holds the history.
    /// `None` on a build with no Cloud link, where the estimate answers
    /// `configured: false` rather than erroring.
    pub cloud_backfill_source: Option<std::sync::Arc<crate::services::CloudBackfillSource>>,
    /// HMAC key for the ADR-042 `plan_token`, derived from the instance's
    /// master encryption key under its own domain.
    ///
    /// Derived rather than generated per process so an operator who reads an
    /// estimate, gets interrupted and comes back inside the token's TTL is not
    /// forced to re-estimate by an unrelated restart.
    pub plan_signing_key: std::sync::Arc<[u8; 32]>,
}
