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

/// Audit record emitted when a backup schedule is updated via PATCH.
/// `fields_changed` lists the names of fields that were present in the request
/// (i.e., the caller intended to change them), for forensic purposes.
#[derive(Debug, Clone, Serialize)]
pub struct BackupScheduleUpdatedAudit {
    pub context: AuditContext,
    pub schedule_id: i32,
    pub schedule_name: String,
    /// Names of fields that were present in the PATCH body.
    pub fields_changed: Vec<String>,
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
pub struct BackupDeletedAudit {
    pub context: AuditContext,
    pub backup_id: String,
    pub s3_source_id: i32,
    pub s3_location: String,
    pub outcome: String,
    pub failure_reason: Option<String>,
    pub deleted_objects: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BackupRetentionCleanupAudit {
    pub context: AuditContext,
    pub requested_backup_ids: Vec<String>,
    pub requested_backup_ids_truncated: bool,
    pub expired: u64,
    pub deleted: u64,
    pub failed: u64,
    pub deleted_backup_ids: Vec<String>,
    pub deleted_backup_ids_truncated: bool,
    pub partially_deleted_backup_ids: Vec<String>,
    pub partially_deleted_backup_ids_truncated: bool,
    pub outcome: String,
    pub failure_reason: Option<String>,
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

#[derive(Debug, Clone, Serialize)]
pub struct RestoreRunAudit {
    pub context: AuditContext,
    pub restore_run_id: i32,
    pub source_service_id: i32,
    pub source_service_name: String,
    pub service_type: String,
    pub source_backup_id: i32,
    pub mode: String,
    pub target_service_name: Option<String>,
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
impl AuditOperation for BackupScheduleUpdatedAudit {
    fn operation_type(&self) -> String {
        "BACKUP_SCHEDULE_UPDATED".to_string()
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

impl AuditOperation for BackupDeletedAudit {
    fn operation_type(&self) -> String {
        "BACKUP_DELETED".to_string()
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

impl AuditOperation for BackupRetentionCleanupAudit {
    fn operation_type(&self) -> String {
        "BACKUP_RETENTION_CLEANUP".to_string()
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

/// Audit record emitted when external services are attached to a backup
/// schedule via `POST /api/backups/schedules/{id}/services`.
#[derive(Debug, Clone, Serialize)]
pub struct ScheduleServicesAttachedAudit {
    pub context: AuditContext,
    pub schedule_id: i32,
    /// IDs that the caller requested be attached (after dedup).
    pub requested_service_ids: Vec<i32>,
    /// Number of rows actually inserted (post `ON CONFLICT DO NOTHING`).
    pub inserted_count: u64,
}

impl AuditOperation for ScheduleServicesAttachedAudit {
    fn operation_type(&self) -> String {
        "BACKUP_SCHEDULE_SERVICES_ATTACHED".to_string()
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

/// Audit record emitted when an external service is detached from a backup
/// schedule via `DELETE /api/backups/schedules/{id}/services/{service_id}`.
#[derive(Debug, Clone, Serialize)]
pub struct ScheduleServiceDetachedAudit {
    pub context: AuditContext,
    pub schedule_id: i32,
    pub service_id: i32,
    /// Whether a row was actually removed (false ⇒ idempotent no-op).
    pub removed: bool,
}

impl AuditOperation for ScheduleServiceDetachedAudit {
    fn operation_type(&self) -> String {
        "BACKUP_SCHEDULE_SERVICE_DETACHED".to_string()
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

/// Audit record emitted when an operator triggers an immediate (manual) run of
/// a backup schedule via `POST /api/backups/schedules/{id}/run`.
#[derive(Debug, Clone, Serialize)]
pub struct ScheduleRunNowAudit {
    pub context: AuditContext,
    /// ID of the schedule that was triggered.
    pub schedule_id: i32,
    /// Human-readable name of the schedule.
    pub schedule_name: String,
    /// ID of the newly created backup row.
    pub backup_id: i32,
    /// Job ID assigned by the runner.
    pub job_id: i64,
}

impl AuditOperation for ScheduleRunNowAudit {
    fn operation_type(&self) -> String {
        "BACKUP_SCHEDULE_RUN_NOW".to_string()
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

impl AuditOperation for RestoreRunAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_RESTORE_RUN".to_string()
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
