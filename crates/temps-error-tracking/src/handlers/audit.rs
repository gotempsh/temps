// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Audit types for error-tracking write operations.
//!
//! Uploading and deleting application source is a write on sensitive data
//! (customer source code stored at rest), so both operations emit an audit log.
//! Triage changes to an error group (status / assignee) are writes too and are
//! audited the same way.

use anyhow::Result;
use serde::Serialize;
pub use temps_core::AuditContext;
use temps_core::AuditOperation;

/// Audit event for uploading a source file (native symbolication).
#[derive(Debug, Clone, Serialize)]
pub struct SourceFileUploadedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub release: String,
    pub file_path: String,
    pub size_bytes: i64,
}

/// Audit event for deleting all source files for a release.
#[derive(Debug, Clone, Serialize)]
pub struct SourceFilesDeletedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub release: String,
    pub deleted_count: u64,
}

/// Audit event for a triage update on an error group (status and/or assignee).
#[derive(Debug, Clone, Serialize)]
pub struct ErrorGroupUpdatedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub group_id: i32,
    /// Status the group was set to.
    pub status: String,
    /// Assignee change as requested: `None` leaves it unchanged, `Some("")`
    /// clears it, any other value sets it.
    pub assigned_to: Option<String>,
}

impl AuditOperation for ErrorGroupUpdatedAudit {
    fn operation_type(&self) -> String {
        "ERROR_GROUP_UPDATED".to_string()
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

impl AuditOperation for SourceFileUploadedAudit {
    fn operation_type(&self) -> String {
        "SOURCE_FILE_UPLOADED".to_string()
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

impl AuditOperation for SourceFilesDeletedAudit {
    fn operation_type(&self) -> String {
        "SOURCE_FILES_DELETED".to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_group_updated_audit_serializes_ids_and_change() {
        let audit = ErrorGroupUpdatedAudit {
            context: AuditContext {
                user_id: 7,
                ip_address: Some("10.0.0.1".to_string()),
                user_agent: "test-agent".to_string(),
            },
            project_id: 3,
            group_id: 42,
            status: "resolved".to_string(),
            assigned_to: Some(String::new()),
        };

        assert_eq!(audit.operation_type(), "ERROR_GROUP_UPDATED");
        assert_eq!(AuditOperation::user_id(&audit), Some(7));
        assert_eq!(audit.ip_address().as_deref(), Some("10.0.0.1"));
        let json: serde_json::Value =
            serde_json::from_str(&AuditOperation::serialize(&audit).expect("serialize"))
                .expect("valid json");
        assert_eq!(json["project_id"], 3);
        assert_eq!(json["group_id"], 42);
        assert_eq!(json["status"], "resolved");
        assert_eq!(json["assigned_to"], "");
    }
}
