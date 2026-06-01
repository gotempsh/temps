use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::env_vars_provider_impl::ExternalServicesEnvProvider;
use crate::handlers::{handlers, types::AppState};
use crate::health_monitor::ExternalServiceHealthMonitor;
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

            // Create ExternalServiceManager. The DnsRegistry is constructed
            // here (not pulled from the registry) because it's a thin wrapper
            // over the same DatabaseConnection — going through the registry
            // would force a plugin-init ordering constraint with no benefit.
            let dns_registry = Arc::new(temps_dns::DnsRegistry::new(db.clone()));
            let external_service_manager = Arc::new(ExternalServiceManager::new(
                db.clone(),
                encryption_service.clone(),
                docker,
                dns_registry,
            ));
            context.register_service(external_service_manager.clone());

            // Register the cross-crate ProjectEnvVarsProvider so the environments
            // plugin can assemble the resolved (manual + integration) env-var view
            // without depending on this crate.
            let env_vars_provider: Arc<dyn temps_core::ProjectEnvVarsProvider> = Arc::new(
                ExternalServicesEnvProvider::new(external_service_manager.clone(), db.clone()),
            );
            context.register_service(env_vars_provider);

            // Spawn role reconcilers for every cluster that's already
            // running. Without this, after a control-plane restart no
            // reconciler exists for any pre-existing cluster and the
            // role records + service_members.role drift from reality.
            let manager_for_startup = external_service_manager.clone();
            tokio::spawn(async move {
                manager_for_startup
                    .spawn_reconcilers_for_existing_clusters()
                    .await;
            });

            tracing::debug!("Providers plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get the services from the plugin context
        let external_service_manager = context.require_service::<ExternalServiceManager>();
        let audit_service = context.require_service::<dyn temps_core::AuditLogger>();

        // Optional: the background health monitor. When the server wired it
        // during startup it shows up here and the manual-health-check endpoint
        // can reuse its same code path. Otherwise the endpoint returns 503.
        let health_monitor = context.get_service::<ExternalServiceHealthMonitor>();

        // Optional: metrics store, present only when metrics collection is enabled.
        let metrics_store = context.get_service::<dyn temps_metrics::MetricsStore>();

        // DB connection for direct queries (alert rules CRUD, etc.)
        let db = context.require_service::<sea_orm::DatabaseConnection>();

        // API key service — needed to provision si_ ingest keys for OTLP-push services.
        let api_key_service = context.require_service::<temps_auth::ApiKeyService>();

        // Config service — resolves the internal URL containers push OTLP to.
        let config_service = context.get_service::<temps_config::ConfigService>();

        // Create QueryService
        let query_service = Arc::new(crate::QueryService::new(external_service_manager.clone()));

        // Create AppState for handlers
        let app_state = Arc::new(AppState {
            external_service_manager,
            audit_service,
            query_service,
            health_monitor,
            metrics_store,
            db,
            api_key_service,
            config_service,
        });

        // Configure routes with the app state
        let providers_routes = handlers::configure_routes().with_state(app_state);

        Some(PluginRoutes::new(providers_routes))
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
        let providers_plugin = ProvidersPlugin;
        assert_eq!(providers_plugin.name(), "providers");
    }
}
