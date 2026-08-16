//! Audit types for error-tracking source-file operations.
//!
//! Uploading and deleting application source is a write on sensitive data
//! (customer source code stored at rest), so both operations emit an audit log.

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
