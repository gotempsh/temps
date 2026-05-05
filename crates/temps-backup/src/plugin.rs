use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use tracing;
use tracing::error;
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::{
    handlers::{self, create_backup_app_state, BackupAppState},
    services::{reconcile_orphan_backups, BackupService, RestoreService},
};
use temps_providers::externalsvc::postgres_upgrade::{
    PostgresContainerLifecycle, PreUpgradeBackupProvider,
};
use temps_providers::postgres_lifecycle::PostgresLifecycleAdapter;
use temps_providers::postgres_upgrade_service::PostgresUpgradeService;

/// Backup Plugin for managing backup operations and schedules
pub struct BackupPlugin;

impl BackupPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BackupPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for BackupPlugin {
    fn name(&self) -> &'static str {
        "backup"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Get required dependencies from the service registry
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let external_service_manager =
                context.require_service::<temps_providers::ExternalServiceManager>();
            let notification_service =
                context.require_service::<temps_notifications::NotificationService>();
            let config_service = context.require_service::<temps_config::ConfigService>();
            let encryption_service = context.require_service::<temps_core::EncryptionService>();

            // Create BackupService
            let backup_service = Arc::new(BackupService::new(
                db.clone(),
                external_service_manager.clone(),
                notification_service,
                config_service.clone(),
                encryption_service.clone(),
            ));
            context.register_service(backup_service.clone());

            // Create RestoreService — orchestrates generic restore across
            // all engines via the ExternalService trait.
            let restore_service = Arc::new(RestoreService::new(
                db.clone(),
                external_service_manager.clone(),
                encryption_service.clone(),
            ));
            context.register_service(restore_service.clone());

            // Get AuditService dependency from other plugins
            let audit_service = context.require_service::<dyn temps_core::AuditLogger>();

            // Build the Postgres major-upgrade service. It needs Docker + the
            // log service (owned by temps-logs) and treats BackupService as
            // the pre-upgrade backup provider via a trait, avoiding a
            // temps-providers -> temps-backup circular dependency.
            let docker = context.require_service::<bollard::Docker>();
            let log_service = context.require_service::<temps_logs::LogService>();
            let backup_provider: Arc<dyn PreUpgradeBackupProvider> = backup_service.clone();
            let lifecycle: Arc<dyn PostgresContainerLifecycle> =
                Arc::new(PostgresLifecycleAdapter::new(
                    db.clone(),
                    docker.clone(),
                    external_service_manager.clone(),
                    encryption_service.clone(),
                ));
            let pg_upgrade_service = Arc::new(PostgresUpgradeService::new(
                db.clone(),
                docker,
                backup_provider,
                lifecycle,
                log_service,
            ));
            context.register_service(pg_upgrade_service.clone());

            // Create BackupAppState for handlers
            let backup_app_state = create_backup_app_state(
                backup_service,
                restore_service,
                audit_service,
                pg_upgrade_service,
                db.clone(),
            )
            .await;
            context.register_service(backup_app_state);

            tracing::debug!("Backup plugin services registered successfully");
            Ok(())
        })
    }

    fn initialize_plugin_services<'a>(
        &'a self,
        context: &'a PluginContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Crash recovery for backups: if `temps serve` restarts mid-backup,
            // the heartbeat task dies with it and the parent + external_service
            // backup rows would stay in `state='running'` forever. Sweep them
            // once at boot and mark them failed so the UI surfaces the truth.
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            if let Err(e) = reconcile_orphan_backups(db.as_ref()).await {
                error!(
                    "Backup orphan reconciliation failed at startup (will not retry until next boot): {}",
                    e
                );
            }

            // Crash recovery: if `temps serve` restarts while an upgrade is
            // mid-flight, the tokio task driving it is gone. Rows stay in
            // `pending`/`running` until we re-spawn an orchestrator for each.
            // Phases are idempotent, so resuming is safe; a short log line
            // records the resumption for the user.
            let pg_upgrade_service =
                context.require_service::<temps_providers::postgres_upgrade_service::PostgresUpgradeService>();

            match pg_upgrade_service.resume_active_upgrades().await {
                Ok(n) if n > 0 => {
                    tracing::info!(resumed = n, "resumed Postgres major upgrades after restart");
                }
                Ok(_) => {}
                Err(e) => {
                    // Don't fail server boot over a resume failure — surface
                    // it loudly and move on so the rest of the platform starts.
                    error!("Failed to resume Postgres major upgrades on boot: {}", e);
                }
            }

            // Rollback volume retention sweep. `phase_snapshot` renames the
            // pre-upgrade PGDATA volume and stamps a 7-day expiry; without a
            // sweeper, expired volumes leak forever. Hourly is plenty — the
            // retention window is measured in days.
            let sweeper = pg_upgrade_service.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
                // First tick fires immediately; use it for a one-shot startup
                // pass so a server restart inside the retention window doesn't
                // have to wait an hour to reclaim disk.
                loop {
                    tick.tick().await;
                    match sweeper.sweep_expired_rollback_volumes().await {
                        Ok(0) => {}
                        Ok(n) => tracing::info!(
                            removed = n,
                            "swept expired Postgres-upgrade rollback volumes"
                        ),
                        Err(e) => {
                            error!("Rollback-volume sweep failed (will retry next tick): {}", e)
                        }
                    }
                }
            });

            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get the BackupAppState
        let backup_app_state = context.require_service::<BackupAppState>();

        // Merge backup + pg-upgrade + restore routes under a single state
        // so all handler modules share the same auth/audit/service wiring.
        let routes = handlers::configure_routes()
            .merge(handlers::pg_upgrade_handler::configure_routes())
            .merge(handlers::restore_handler::configure_routes())
            .with_state(backup_app_state);

        Some(PluginRoutes { router: routes })
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        let mut doc = <handlers::backup_handler::BackupApiDoc as OpenApiTrait>::openapi();
        doc.merge(<handlers::pg_upgrade_handler::PgUpgradeApiDoc as OpenApiTrait>::openapi());
        doc.merge(<handlers::restore_handler::RestoreApiDoc as OpenApiTrait>::openapi());
        Some(doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_backup_plugin_name() {
        let backup_plugin = BackupPlugin::new();
        assert_eq!(backup_plugin.name(), "backup");
    }

    #[tokio::test]
    async fn test_backup_plugin_default() {
        let backup_plugin = BackupPlugin;
        assert_eq!(backup_plugin.name(), "backup");
    }
}
