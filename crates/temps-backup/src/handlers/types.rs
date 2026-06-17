use sea_orm::DatabaseConnection;
use std::sync::Arc;
use temps_backup_core::BackupExecutor;
use temps_core::AuditLogger;
use temps_providers::postgres_upgrade_service::PostgresUpgradeService;

use crate::services::{BackupService, RestoreService};

/// Application state shared across all backup HTTP handlers.
pub struct BackupAppState {
    pub backup_service: Arc<BackupService>,
    pub restore_service: Arc<RestoreService>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub pg_upgrade_service: Arc<PostgresUpgradeService>,
    pub db: Arc<DatabaseConnection>,
    /// In-process backup executor. Replaces the queue-based runner that
    /// previously sat between trigger paths and engines.
    pub backup_executor: Arc<BackupExecutor>,
    pub telemetry: std::sync::Arc<dyn temps_core::telemetry::TelemetryReporter>,
}

pub fn create_backup_app_state(
    backup_service: Arc<BackupService>,
    restore_service: Arc<RestoreService>,
    audit_service: Arc<dyn AuditLogger>,
    pg_upgrade_service: Arc<PostgresUpgradeService>,
    db: Arc<DatabaseConnection>,
    backup_executor: Arc<BackupExecutor>,
    telemetry: std::sync::Arc<dyn temps_core::telemetry::TelemetryReporter>,
) -> Arc<BackupAppState> {
    Arc::new(BackupAppState {
        backup_service,
        restore_service,
        audit_service,
        pg_upgrade_service,
        db,
        backup_executor,
        telemetry,
    })
}
