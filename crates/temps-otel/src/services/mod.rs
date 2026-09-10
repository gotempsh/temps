// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Core OTel services.

pub mod anomaly_preview;
/// One-shot Temps Cloud telemetry backfill (ADR-040 §1).
pub mod cloud_backfill;
/// Audit trail for the out-of-process Cloud telemetry backfill.
pub mod cloud_backfill_audit;
/// Shared progress record for the out-of-process Cloud telemetry backfill.
pub mod cloud_backfill_progress;
/// Durable state of a bulk Cloud-telemetry activation job (ADR-042 §7, §8).
pub mod cloud_bulk_activation;
/// The signed `plan_token` that binds a confirmed estimate to the job it
/// creates (ADR-042 §9).
pub mod cloud_bulk_activation_plan;
/// The background task that runs a bulk Cloud-telemetry activation job
/// (ADR-042 §2, §3, §5, §7).
pub mod cloud_bulk_activation_worker;
/// Per-project Temps Cloud telemetry egress policy (ADR-040 §1).
pub mod cloud_fidelity;
/// Handing Cloud-primary projects back to local span storage (ADR-041 §7c).
pub mod cloud_primary_fallback;
/// The purchase-triggered entry point onto the bulk activation engine
/// (ADR-042 §1a, P3).
pub mod cloud_purchase_activation;
/// Cross-project trace discovery service (ADR-027 Phases 0 and 2).
pub mod cross_project;
pub mod dashboard_service;
/// Span attribute facet registration and cache management.
pub mod facet_service;
pub mod health_service;
pub mod metric_alert_evaluator;
pub mod metric_alert_service;
pub mod otel_service;
/// Per-project telemetry write mode, its gate and its interval ledger
/// (ADR-041 §1, §7, §8).
pub mod telemetry_write_mode;
/// `temps_core::TraceReader` impl for `OtelService` (storage-agnostic read API).
pub mod trace_reader;

pub use cloud_backfill::{
    backfill_cloud_telemetry_window, count_spans_window, estimate_backfill, CloudBackfillCursor,
    CloudBackfillError, CloudBackfillEstimate, CloudBackfillReport, CloudBackfillSource,
    DEFAULT_BATCH_SIZE as CLOUD_BACKFILL_DEFAULT_BATCH_SIZE,
};
pub use cloud_backfill_audit::{
    record_backfill_audit, CloudTelemetryBackfillAudit, CloudTelemetryBackfillOutcome,
    CLOUD_TELEMETRY_BACKFILL_FINISHED, CLOUD_TELEMETRY_BACKFILL_STARTED,
};
pub use cloud_backfill_progress::{
    percent_complete, truncate_failure_reason, CloudBackfillProgressError,
    CloudBackfillProgressService, MAX_LAST_ERROR_CHARS,
};
pub use cloud_bulk_activation::{
    cursor_of, BulkAbortReason, BulkJobDetail, BulkJobProjectPlan, BulkSkipReason,
    CloudBulkActivationError, CloudBulkActivationService, EnqueueBulkJobRequest,
};
pub use cloud_bulk_activation_plan::{
    mint_plan_token, plan_hash, verify_plan_token, MintedPlan, PlanTokenError, VerifiedPlan,
    PLAN_TOKEN_KEY_DOMAIN, PLAN_TOKEN_TTL_SECONDS,
};
pub use cloud_bulk_activation_worker::{
    anomaly_byte_budget, anomaly_pause_reason, exceeds_anomaly_budget, rate_limit_pause,
    unestimated_pause_reason, BulkActivationCycle, BulkActivationTuning, BulkAnomalyFactorSource,
    BulkRateLimitSource, CloudBulkActivationWorker, ANOMALY_PAUSE_CODE, MIN_ANOMALY_BUDGET_BYTES,
    UNESTIMATED_ANOMALY_BUDGET_BASE_BYTES, UNESTIMATED_PAUSE_CODE,
};
pub use cloud_fidelity::{
    CloudPolicyCache, CloudPolicyError, CloudTelemetryPolicy, CLOUD_POLICY_CACHE_TTL,
};
pub use cloud_primary_fallback::{
    CloudPrimaryFallback, CloudWriteSuspensionObserver, OutboxSpiller,
};
pub use cloud_purchase_activation::{PurchaseActivationTrigger, MAX_PURCHASE_ACTIVATION_PROJECTS};
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
pub use telemetry_write_mode::{
    resolve_window, CloudLinkSnapshot, CloudWriteSuspension, LocalSpanStoreRequirement,
    ProjectTelemetryWriteSettings, TelemetryWriteModeError, TelemetryWriteModeService,
    WindowResolution, CLOUD_SETUP_PATH,
};
