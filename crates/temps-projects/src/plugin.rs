use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::services::custom_domains::CustomDomainService;
use crate::services::project::ProjectService;

/// Projects Plugin for managing project lifecycle and configurations
pub struct ProjectsPlugin;

impl ProjectsPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ProjectsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for ProjectsPlugin {
    fn name(&self) -> &'static str {
        "projects"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Get required dependencies from the service registry
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let queue_service = context.require_service::<dyn temps_core::JobQueue>();
            let config_service = context.require_service::<temps_config::ConfigService>();
            let external_service_manager =
                context.require_service::<temps_providers::ExternalServiceManager>();
            let git_provider_manager = context.require_service::<temps_git::GitProviderManager>();
            let environment_service =
                context.require_service::<temps_environments::EnvironmentService>();

            // Create ProjectService
            let project_service = Arc::new(ProjectService::new(
                db.clone(),
                queue_service,
                config_service,
                external_service_manager,
                git_provider_manager,
                environment_service,
            ));
            context.register_service(project_service);

            // Create CustomDomainService
            let custom_domain_service = Arc::new(CustomDomainService::new(db.clone()));
            context.register_service(custom_domain_service);

            tracing::debug!("Projects plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let project_service = context.require_service::<ProjectService>();
        let custom_domain_service = context.require_service::<CustomDomainService>();
        let audit_service = context.require_service::<dyn temps_core::AuditLogger>();
        let app_state = Arc::new(crate::handlers::AppState {
            project_service,
            custom_domain_service,
            audit_service,
        });
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
    async fn test_projects_plugin_name() {
        let projects_plugin = ProjectsPlugin::new();
        assert_eq!(projects_plugin.name(), "projects");
    }

    #[tokio::test]
    async fn test_projects_plugin_default() {
        let projects_plugin = ProjectsPlugin;
        assert_eq!(projects_plugin.name(), "projects");
    }
}
