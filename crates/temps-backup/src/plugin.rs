use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;
use tracing;

use crate::{
    services::BackupService,
    handlers::{self, BackupAppState, create_backup_app_state},
};

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
            let external_service_manager = context.require_service::<temps_providers::ExternalServiceManager>();
            let notification_service = context.require_service::<temps_notifications::NotificationService>();
            let config_service = context.require_service::<temps_config::ConfigService>();
            let encryption_service = context.require_service::<temps_core::EncryptionService>();

            // Create BackupService
            let backup_service = Arc::new(BackupService::new(
                db.clone(),
                external_service_manager,
                notification_service,
                config_service.clone(),
                encryption_service.clone(),
            ));
            context.register_service(backup_service.clone());

            // Get AuditService dependency from other plugins
            let audit_service = context.require_service::<dyn temps_core::AuditLogger>();

            // Create BackupAppState for handlers
            let backup_app_state = create_backup_app_state(
                backup_service,
                audit_service,
            ).await;
            context.register_service(backup_app_state);

            tracing::debug!("Backup plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get the BackupAppState
        let backup_app_state = context.require_service::<BackupAppState>();

        // Configure routes
        let backup_routes = handlers::configure_routes().with_state(backup_app_state);

        Some(PluginRoutes {
            router: backup_routes,
        })
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<handlers::backup_handler::BackupApiDoc as OpenApiTrait>::openapi())
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
        let backup_plugin = BackupPlugin::default();
        assert_eq!(backup_plugin.name(), "backup");
    }
}