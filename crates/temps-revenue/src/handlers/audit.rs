//! Audit event structs for revenue integration management.
//!
//! Audit logging is mandatory for every write operation (CLAUDE.md). These
//! structs serialize to JSON and are persisted by `temps-audit` through the
//! shared `AuditLogger` trait.

use anyhow::Result;
use serde::Serialize;
use temps_core::{AuditContext, AuditOperation};

#[derive(Debug, Clone, Serialize)]
pub struct RevenueIntegrationCreatedAudit {
    pub context: AuditContext,
    pub integration_id: i32,
    pub project_id: i32,
    pub provider: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RevenueIntegrationDeletedAudit {
    pub context: AuditContext,
    pub integration_id: i32,
    pub project_id: i32,
    pub provider: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RevenueIntegrationTokenRotatedAudit {
    pub context: AuditContext,
    pub integration_id: i32,
    pub project_id: i32,
    pub provider: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RevenueIntegrationSecretRotatedAudit {
    pub context: AuditContext,
    pub integration_id: i32,
    pub project_id: i32,
    pub provider: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RevenueIntegrationConfigUpdatedAudit {
    pub context: AuditContext,
    pub integration_id: i32,
    pub project_id: i32,
    pub provider: String,
    /// True when the operator cleared the config back to accept-everything.
    pub cleared: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RevenueCsvImportedAudit {
    pub context: AuditContext,
    pub integration_id: i32,
    pub project_id: i32,
    pub provider: String,
    /// One of "subscriptions" or "invoices" so the audit trail makes it
    /// obvious which export was uploaded.
    pub kind: String,
    pub rows_read: usize,
    pub inserted: usize,
    pub updated: usize,
    pub skipped: usize,
}

macro_rules! impl_audit_operation {
    ($ty:ty, $op:literal) => {
        impl AuditOperation for $ty {
            fn operation_type(&self) -> String {
                $op.to_string()
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
    };
}

impl_audit_operation!(
    RevenueIntegrationCreatedAudit,
    "REVENUE_INTEGRATION_CREATED"
);
impl_audit_operation!(
    RevenueIntegrationDeletedAudit,
    "REVENUE_INTEGRATION_DELETED"
);
impl_audit_operation!(
    RevenueIntegrationTokenRotatedAudit,
    "REVENUE_INTEGRATION_TOKEN_ROTATED"
);
impl_audit_operation!(
    RevenueIntegrationSecretRotatedAudit,
    "REVENUE_INTEGRATION_SECRET_ROTATED"
);
impl_audit_operation!(
    RevenueIntegrationConfigUpdatedAudit,
    "REVENUE_INTEGRATION_CONFIG_UPDATED"
);
impl_audit_operation!(RevenueCsvImportedAudit, "REVENUE_CSV_IMPORTED");
