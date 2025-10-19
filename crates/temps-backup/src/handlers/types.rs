use std::sync::Arc;
use temps_core::AuditLogger;

use crate::services::BackupService;

pub struct BackupAppState {
	pub backup_service: Arc<BackupService>,
	pub audit_service: Arc<dyn AuditLogger>,
}

pub async fn create_backup_app_state(
	backup_service: Arc<BackupService>,
	audit_service: Arc<dyn AuditLogger>,
) -> Arc<BackupAppState> {
	Arc::new(BackupAppState {
		backup_service,
		audit_service,
	})
}