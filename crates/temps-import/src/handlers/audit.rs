//! Audit events for import operations.
//!
//! `create_plan` only reads from the source platform, but `execute_import`
//! creates real projects/services/deployments from user-supplied platform
//! credentials — exactly the kind of write operation CLAUDE.md requires an
//! audit trail for, and the only one worth recording here.

use anyhow::Result;
use serde::Serialize;
use temps_core::{AuditContext, AuditOperation};

#[derive(Debug, Clone, Serialize)]
pub struct ImportExecutedAudit {
    pub context: AuditContext,
    pub session_id: String,
    pub project_name: String,
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
    pub dry_run: bool,
    pub succeeded: bool,
}

impl AuditOperation for ImportExecutedAudit {
    fn operation_type(&self) -> String {
        "IMPORT_EXECUTED".to_string()
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
