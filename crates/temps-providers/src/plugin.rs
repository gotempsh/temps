use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::handlers::{handlers, types::AppState};
use crate::services::ExternalServiceManager;

/// Providers Plugin for managing external service integrations
pub struct ProvidersPlugin;

impl ProvidersPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ProvidersPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for ProvidersPlugin {
    fn name(&self) -> &'static str {
        "providers"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Get required dependencies from the service registry
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let encryption_service = context.require_service::<temps_core::EncryptionService>();
            // AuditService should already be registered by the audit plugin
            let docker = context.require_service::<bollard::Docker>();

            // Create ExternalServiceManager
            let external_service_manager = Arc::new(ExternalServiceManager::new(
                db.clone(),
                encryption_service.clone(),
                docker,
            ));
            context.register_service(external_service_manager);

            tracing::debug!("Providers plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get the services from the plugin context
        let external_service_manager = context.require_service::<ExternalServiceManager>();
        let audit_service = context.require_service::<dyn temps_core::AuditLogger>();

        // Create AppState for handlers
        let app_state = Arc::new(AppState {
            external_service_manager,
            audit_service,
        });

        // Configure routes with the app state
        let providers_routes = handlers::configure_routes().with_state(app_state);

        Some(PluginRoutes {
            router: providers_routes,
        })
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<handlers::ExternalServiceApiDoc as OpenApiTrait>::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_providers_plugin_name() {
        let providers_plugin = ProvidersPlugin::new();
        assert_eq!(providers_plugin.name(), "providers");
    }

    #[tokio::test]
    async fn test_providers_plugin_default() {
        let providers_plugin = ProvidersPlugin::default();
        assert_eq!(providers_plugin.name(), "providers");
    }
}
