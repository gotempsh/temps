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
            let external_service_manager =
                context.require_service::<temps_providers::services::ExternalServiceManager>();
            let config_service = context.require_service::<temps_config::ConfigService>();
            let docker = context.require_service::<bollard::Docker>();

            let resource_executor = Arc::new(crate::services::ResourceExecutor::new(
                external_service_manager,
                Arc::new(temps_projects::services::CustomDomainService::new(
                    db.clone(),
                )),
                docker,
            ));

            // Create import orchestrator with all required services
            let mut orchestrator = ImportOrchestrator::new(
                db.clone(),
                git_provider_manager,
                project_service,
                deployment_service,
                resource_executor,
                config_service,
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

            // Register Kubernetes importer (stateless — cluster credentials
            // arrive per-request as a kubeconfig, so construction cannot fail)
            orchestrator
                .register_importer(Arc::new(temps_import_kubernetes::KubernetesImporter::new()));
            tracing::info!("Kubernetes importer registered successfully");

            // Register Coolify importer (stateless — instance URL + API token
            // arrive per-request as credentials, so construction cannot fail)
            orchestrator.register_importer(Arc::new(temps_import_coolify::CoolifyImporter::new()));
            tracing::info!("Coolify importer registered successfully");

            // Register Dokploy importer (stateless — instance URL + API key
            // arrive per-request as credentials, so construction cannot fail)
            orchestrator.register_importer(Arc::new(temps_import_dokploy::DokployImporter::new()));
            tracing::info!("Dokploy importer registered successfully");

            // Register CapRover importer (stateless — instance URL + password
            // arrive per-request as credentials, so construction cannot fail)
            orchestrator
                .register_importer(Arc::new(temps_import_caprover::CaproverImporter::new()));
            tracing::info!("CapRover importer registered successfully");

            // Register Portainer importer (stateless — instance URL +
            // password arrive per-request, so construction cannot fail)
            orchestrator
                .register_importer(Arc::new(temps_import_portainer::PortainerImporter::new()));
            tracing::info!("Portainer importer registered successfully");

            // Register Kamal importer (reads config/deploy.yml supplied
            // per-request — Kamal has no control plane to contact)
            orchestrator.register_importer(Arc::new(temps_import_kamal::KamalImporter::new()));
            tracing::info!("Kamal importer registered successfully");

            let orchestrator = Arc::new(orchestrator);
            context.register_service(orchestrator);

            tracing::debug!("Import plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let import_orchestrator = context.require_service::<ImportOrchestrator>();
        let audit_service = context.require_service::<dyn temps_core::AuditLogger>();

        let app_state = Arc::new(handlers::types::AppState {
            import_orchestrator,
            audit_service,
        });

        let routes = handlers::configure_routes().with_state(app_state);

        Some(PluginRoutes::new(routes))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<handlers::ImportApiDoc as UtoimaOpenApi>::openapi())
    }
}
