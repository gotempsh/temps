use sea_orm::DatabaseConnection;
use std::sync::Arc;
use temps_backup_core::BackupExecutor;
use temps_core::AuditLogger;
use temps_providers::postgres_upgrade_service::PostgresUpgradeService;

use crate::services::{BackupService, RestoreService};

/// Application state shared across all backup HTTP handlers.
#[derive(Clone)]
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
    /// Team/project access policy (ADR-028). `None` in OSS, where no
    /// team-access concept exists; registered by the plugin in EE. Restore is
    /// gated on it because a restore both destroys the target service's data
    /// and copies the source backup's data into it — so the caller has to be
    /// entitled to *both* services, not merely hold the instance-wide
    /// `BackupsWrite` + `ExternalServicesWrite` permissions that the default
    /// `Role::User` already carries.
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    /// MFA / step-up authorizer for security-sensitive mutations. `None` in
    /// installations that have not registered an authorizer (step-up checks
    /// become a no-op).
    pub sensitive_action_authorizer: Arc<dyn temps_core::SensitiveActionAuthorizer>,
}

// Field-for-field constructor for a public struct: every argument is a
// distinct dependency the handlers need, and collapsing them into a builder
// would only move the same list somewhere else.
#[allow(clippy::too_many_arguments)]
pub fn create_backup_app_state(
    backup_service: Arc<BackupService>,
    restore_service: Arc<RestoreService>,
    audit_service: Arc<dyn AuditLogger>,
    pg_upgrade_service: Arc<PostgresUpgradeService>,
    db: Arc<DatabaseConnection>,
    backup_executor: Arc<BackupExecutor>,
    telemetry: std::sync::Arc<dyn temps_core::telemetry::TelemetryReporter>,
    project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    sensitive_action_authorizer: Arc<dyn temps_core::SensitiveActionAuthorizer>,
) -> Arc<BackupAppState> {
    Arc::new(BackupAppState {
        backup_service,
        restore_service,
        audit_service,
        pg_upgrade_service,
        db,
        backup_executor,
        telemetry,
        project_access_checker,
        sensitive_action_authorizer,
    })
}
