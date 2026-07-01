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

    fn user_id(&self) -> i32 {
        self.context.user_id
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

    fn user_id(&self) -> i32 {
        self.context.user_id
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

    fn user_id(&self) -> i32 {
        self.context.user_id
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

    fn user_id(&self) -> i32 {
        self.context.user_id
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

    fn user_id(&self) -> i32 {
        self.context.user_id
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

    fn user_id(&self) -> i32 {
        self.context.user_id
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
