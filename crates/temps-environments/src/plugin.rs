use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::services::environment_service::EnvironmentService;
use crate::services::secret_service::SecretService;
use crate::EnvVarService;

/// Environments Plugin for managing environment lifecycle and configurations
pub struct EnvironmentsPlugin;

impl EnvironmentsPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for EnvironmentsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for EnvironmentsPlugin {
    fn name(&self) -> &'static str {
        "environments"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Get required dependencies from the service registry
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let config_service = context.require_service::<temps_config::ConfigService>();
            let queue_service = context.require_service::<dyn temps_core::JobQueue>();

            // Create EnvironmentService with queue service
            let environment_service = Arc::new(
                EnvironmentService::new(db.clone(), config_service)
                    .with_queue_service(queue_service),
            );
            context.register_service(environment_service);
            let encryption_service = context.require_service::<temps_core::EncryptionService>();
            let env_var_service =
                Arc::new(EnvVarService::new(db.clone(), encryption_service.clone()));
            context.register_service(env_var_service);
            let secret_service = Arc::new(SecretService::new(db.clone(), encryption_service));
            context.register_service(secret_service);
            tracing::debug!("Environments plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let environment_service = context.require_service::<EnvironmentService>();
        let audit_service = context.require_service::<dyn temps_core::AuditLogger>();
        let env_var_service = context.require_service::<EnvVarService>();
        let secret_service = context.require_service::<SecretService>();
        let deployment_service = context.require_service::<dyn temps_core::DeploymentCanceller>();
        let on_demand_waker = context.get_service::<dyn temps_core::OnDemandWaker>();
        let integration_env_provider =
            context.get_service::<dyn temps_core::ProjectEnvVarsProvider>();

        let app_state = crate::handlers::create_environment_app_state(
            environment_service,
            env_var_service,
            secret_service,
            audit_service,
            deployment_service,
            on_demand_waker,
            integration_env_provider,
        );

        let routes = crate::handlers::configure_routes().with_state(app_state);
        Some(PluginRoutes { router: routes })
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<crate::handlers::ApiDoc as OpenApiTrait>::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_environments_plugin_name() {
        let environments_plugin = EnvironmentsPlugin::new();
        assert_eq!(environments_plugin.name(), "environments");
    }

    #[tokio::test]
    async fn test_environments_plugin_default() {
        let environments_plugin = EnvironmentsPlugin;
        assert_eq!(environments_plugin.name(), "environments");
    }
}
