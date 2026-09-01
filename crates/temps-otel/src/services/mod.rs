// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Core OTel services.

pub mod anomaly_preview;
/// Cross-project trace discovery service (ADR-027 Phases 0 and 2).
pub mod cross_project;
pub mod dashboard_service;
/// Span attribute facet registration and cache management.
pub mod facet_service;
pub mod health_service;
pub mod metric_alert_evaluator;
pub mod metric_alert_service;
pub mod otel_service;
/// `temps_core::TraceReader` impl for `OtelService` (storage-agnostic read API).
pub mod trace_reader;

pub use cross_project::{
    AnnotatedSpan, CrossProjectTraceError, CrossProjectTraceService, ProjectRef, SiblingRef,
    TraceHintMsg, TraceProjectRef, UnifiedTrace,
};
pub use dashboard_service::MetricDashboardService;
pub use facet_service::{
    FacetBackendKind, FacetCache, FacetError, FacetInfo, FacetService, FacetStatus,
};
pub use health_service::HealthComputeService;
pub use metric_alert_evaluator::MetricAlertEvaluator;
pub use metric_alert_service::MetricAlertService;
pub use otel_service::OtelService;
