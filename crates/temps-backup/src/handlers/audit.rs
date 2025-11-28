use anyhow::Result;
use serde::Serialize;
use std::collections::HashMap;
pub use temps_core::AuditContext;
use temps_core::AuditOperation;

// Re-export AuditContext from temps_audit

// S3 Source audit structs
#[derive(Debug, Clone, Serialize)]
pub struct S3SourceCreatedAudit {
    pub context: AuditContext,
    pub source_id: i32,
    pub name: String,
    pub bucket_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct S3SourceUpdatedAudit {
    pub context: AuditContext,
    pub source_id: i32,
    pub name: String,
    pub bucket_name: String,
    pub updated_fields: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct S3SourceDeletedAudit {
    pub context: AuditContext,
    pub source_id: i32,
    pub name: String,
    pub bucket_name: String,
}

// Backup audit structs
#[derive(Debug, Clone, Serialize)]
pub struct BackupScheduleStatusChangedAudit {
    pub context: AuditContext,
    pub schedule_id: i32,
    pub name: String,
    pub new_status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BackupRunAudit {
    pub context: AuditContext,
    pub source_id: i32,
    pub source_name: String,
    pub backup_id: String,
    pub backup_type: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExternalServiceBackupRunAudit {
    pub context: AuditContext,
    pub service_id: i32,
    pub service_name: String,
    pub service_type: String,
    pub backup_id: i32,
    pub backup_type: String,
}

// Implement AuditOperation for S3 Source audit structs
impl AuditOperation for S3SourceCreatedAudit {
    fn operation_type(&self) -> String {
        "S3_SOURCE_CREATED".to_string()
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
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

impl AuditOperation for S3SourceUpdatedAudit {
    fn operation_type(&self) -> String {
        "S3_SOURCE_UPDATED".to_string()
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
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

impl AuditOperation for S3SourceDeletedAudit {
    fn operation_type(&self) -> String {
        "S3_SOURCE_DELETED".to_string()
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
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

// Implement AuditOperation for backup audit structs
impl AuditOperation for BackupScheduleStatusChangedAudit {
    fn operation_type(&self) -> String {
        "BACKUP_SCHEDULE_STATUS_CHANGED".to_string()
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
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

impl AuditOperation for BackupRunAudit {
    fn operation_type(&self) -> String {
        "BACKUP_RUN".to_string()
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
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

impl AuditOperation for ExternalServiceBackupRunAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_BACKUP_RUN".to_string()
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
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}
