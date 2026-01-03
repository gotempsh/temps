//! Audit types for KV service management operations

use anyhow::Result;
use serde::Serialize;
pub use temps_core::AuditContext;
use temps_core::AuditOperation;

/// Audit event for enabling the KV service
#[derive(Debug, Clone, Serialize)]
pub struct KvServiceEnabledAudit {
    pub context: AuditContext,
    pub service_name: String,
    pub docker_image: Option<String>,
    pub version: Option<String>,
}

/// Audit event for disabling the KV service
#[derive(Debug, Clone, Serialize)]
pub struct KvServiceDisabledAudit {
    pub context: AuditContext,
    pub service_name: String,
}

impl AuditOperation for KvServiceEnabledAudit {
    fn operation_type(&self) -> String {
        "KV_SERVICE_ENABLED".to_string()
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

impl AuditOperation for KvServiceDisabledAudit {
    fn operation_type(&self) -> String {
        "KV_SERVICE_DISABLED".to_string()
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
