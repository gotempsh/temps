//! Import plugin for Temps plugin system

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use utoipa::{openapi::OpenApi, OpenApi as UtoimaOpenApi};

use crate::{handlers, services::ImportOrchestrator};

/// Import plugin for managing workload imports
pub struct ImportPlugin;

impl ImportPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ImportPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for ImportPlugin {
    fn name(&self) -> &'static str {
        "import"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Get required dependencies
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let git_provider_manager = context.require_service::<temps_git::GitProviderManager>();
            let project_service = context.require_service::<temps_projects::ProjectService>();
            let deployment_service =
                context.require_service::<temps_deployments::DeploymentService>();

            // Create import orchestrator with all required services
            let mut orchestrator = ImportOrchestrator::new(
                db.clone(),
                git_provider_manager,
                project_service,
                deployment_service,
            );

            // Register Docker importer if available
            match temps_import_docker::DockerImporter::new() {
                Ok(docker_importer) => {
                    orchestrator.register_importer(Arc::new(docker_importer));
                    tracing::info!("Docker importer registered successfully");
                }
                Err(e) => {
                    tracing::warn!("Failed to initialize Docker importer: {}. Docker imports will not be available.", e);
                }
            }

            let orchestrator = Arc::new(orchestrator);
            context.register_service(orchestrator);

            tracing::debug!("Import plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let import_orchestrator = context
            .get_service::<ImportOrchestrator>()
            .expect("ImportOrchestrator must be registered before configuring routes");

        let app_state = Arc::new(handlers::types::AppState {
            import_orchestrator,
        });

        let routes = handlers::configure_routes().with_state(app_state);

        Some(PluginRoutes { router: routes })
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<handlers::ImportApiDoc as UtoimaOpenApi>::openapi())
    }
}
