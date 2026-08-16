//! Audit operations for metric dashboard write endpoints.

use anyhow::Result;
use serde::Serialize;
pub use temps_core::AuditContext;
use temps_core::AuditOperation;

/// Audit event for creating a metric dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct OtelDashboardCreatedAudit {
    pub context: AuditContext,
    pub dashboard_id: i32,
    pub project_id: i32,
    pub name: String,
}

/// Audit event for updating a metric dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct OtelDashboardUpdatedAudit {
    pub context: AuditContext,
    pub dashboard_id: i32,
    pub project_id: i32,
}

/// Audit event for deleting a metric dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct OtelDashboardDeletedAudit {
    pub context: AuditContext,
    pub dashboard_id: i32,
    pub project_id: i32,
}

impl AuditOperation for OtelDashboardCreatedAudit {
    fn operation_type(&self) -> String {
        "OTEL_DASHBOARD_CREATED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {}", e))
    }
}

impl AuditOperation for OtelDashboardUpdatedAudit {
    fn operation_type(&self) -> String {
        "OTEL_DASHBOARD_UPDATED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {}", e))
    }
}

impl AuditOperation for OtelDashboardDeletedAudit {
    fn operation_type(&self) -> String {
        "OTEL_DASHBOARD_DELETED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {}", e))
    }
}

// ── Cross-project trace read audit events (ADR-027 Phase 1 & 2) ────

/// Audit event for querying sibling projects for a cross-project trace.
///
/// Emitted by `GET /otel/traces/cross-project/{trace_id}` **before** the
/// database lookup so that every cross-project discovery attempt is recorded
/// even when the query subsequently fails.
#[derive(Debug, Clone, Serialize)]
pub struct CrossProjectTraceSiblingsReadAudit {
    pub context: AuditContext,
    /// 32-char lowercase hex trace_id being queried.
    pub trace_id: String,
}

impl AuditOperation for CrossProjectTraceSiblingsReadAudit {
    fn operation_type(&self) -> String {
        "CROSS_PROJECT_TRACE_SIBLINGS_READ".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {}", e))
    }
}

/// Audit event for fetching a unified cross-project trace waterfall.
///
/// Emitted by `GET /otel/global/traces/{trace_id}` **before** the fan-out
/// queries execute so that the attempt is logged even on partial failures.
#[derive(Debug, Clone, Serialize)]
pub struct UnifiedTraceReadAudit {
    pub context: AuditContext,
    /// 32-char lowercase hex trace_id being assembled.
    pub trace_id: String,
}

impl AuditOperation for UnifiedTraceReadAudit {
    fn operation_type(&self) -> String {
        "UNIFIED_TRACE_READ".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {}", e))
    }
}

// ── Facet audit events ────────────────────────────────────────────────

/// Audit event for registering a new OTel span attribute facet.
#[derive(Debug, Clone, Serialize)]
pub struct FacetCreatedAudit {
    pub context: AuditContext,
    /// The OTel attribute key that was registered.
    pub attribute_key: String,
    /// The slot column index assigned (1..=20).
    pub slot: u8,
}

/// Audit event for deleting an OTel span attribute facet.
#[derive(Debug, Clone, Serialize)]
pub struct FacetDeletedAudit {
    pub context: AuditContext,
    /// The OTel attribute key that was removed.
    pub attribute_key: String,
}

/// Audit event for retrying a failed OTel span attribute facet backfill.
#[derive(Debug, Clone, Serialize)]
pub struct FacetBackfillRetriedAudit {
    pub context: AuditContext,
    /// The OTel attribute key whose backfill is being retried.
    pub attribute_key: String,
}

impl AuditOperation for FacetCreatedAudit {
    fn operation_type(&self) -> String {
        "OTEL_FACET_CREATED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {}", e))
    }
}

impl AuditOperation for FacetDeletedAudit {
    fn operation_type(&self) -> String {
        "OTEL_FACET_DELETED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {}", e))
    }
}

impl AuditOperation for FacetBackfillRetriedAudit {
    fn operation_type(&self) -> String {
        "OTEL_FACET_BACKFILL_RETRIED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {}", e))
    }
}

// ── Metric alert rule audit events ──────────────────────────────────

/// Audit event for creating a metric alert rule.
#[derive(Debug, Clone, Serialize)]
pub struct OtelMetricAlertCreatedAudit {
    pub context: AuditContext,
    pub rule_id: i32,
    pub project_id: i32,
    pub name: String,
}

/// Audit event for updating a metric alert rule.
#[derive(Debug, Clone, Serialize)]
pub struct OtelMetricAlertUpdatedAudit {
    pub context: AuditContext,
    pub rule_id: i32,
    pub project_id: i32,
}

/// Audit event for deleting a metric alert rule.
#[derive(Debug, Clone, Serialize)]
pub struct OtelMetricAlertDeletedAudit {
    pub context: AuditContext,
    pub rule_id: i32,
    pub project_id: i32,
}

impl AuditOperation for OtelMetricAlertCreatedAudit {
    fn operation_type(&self) -> String {
        "OTEL_METRIC_ALERT_CREATED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {}", e))
    }
}

impl AuditOperation for OtelMetricAlertUpdatedAudit {
    fn operation_type(&self) -> String {
        "OTEL_METRIC_ALERT_UPDATED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {}", e))
    }
}

impl AuditOperation for OtelMetricAlertDeletedAudit {
    fn operation_type(&self) -> String {
        "OTEL_METRIC_ALERT_DELETED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {}", e))
    }
}
