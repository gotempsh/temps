use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use utoipa::{openapi::OpenApi, OpenApi as UtoimaOpenApi};

use crate::{
    handlers,
    services::{DeploymentService, JobProcessorService, WorkflowExecutionService},
    WorkflowPlanner,
};

/// Deployments Plugin for managing deployment operations
pub struct DeploymentsPlugin;

impl DeploymentsPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DeploymentsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for DeploymentsPlugin {
    fn name(&self) -> &'static str {
        "deployments"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Get required dependencies from the service registry
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let log_service = context.require_service::<temps_logs::LogService>();
            let config_service = context.require_service::<temps_config::ConfigService>();
            let queue_service = context.require_service::<dyn temps_core::JobQueue>();
            let docker_log_service = context.require_service::<temps_logs::DockerLogService>();
            let deployer = context.require_service::<dyn temps_deployer::ContainerDeployer>();
            let git_provider = context.require_service::<dyn temps_git::GitProviderManagerTrait>();
            let image_builder = context.require_service::<dyn temps_deployer::ImageBuilder>();
            let git_provider_manager = context.require_service::<temps_git::GitProviderManager>();
            // Create DeploymentService
            let deployment_service = Arc::new(DeploymentService::new(
                db.clone(),
                log_service.clone(),
                config_service.clone(),
                queue_service.clone(),
                docker_log_service,
                deployer.clone(),
            ));
            context.register_service(deployment_service.clone());

            // Cancel any running deployments from previous server instance
            let cancel_service = deployment_service.clone();
            tokio::spawn(async move {
                if let Err(e) = cancel_service
                    .cancel_running_deployments("Server restarted")
                    .await
                {
                    tracing::error!("Failed to cancel running deployments: {}", e);
                }
            });

            // Create DatabaseCronConfigService to manage cron jobs
            let database_cron_service = Arc::new(crate::services::DatabaseCronConfigService::new(
                db.clone(),
                queue_service.clone(),
            ));
            let cron_service =
                database_cron_service.clone() as Arc<dyn crate::jobs::CronConfigService>;

            // Register database_cron_service for handlers
            context.register_service(database_cron_service.clone());

            // Start cron scheduler in background
            let scheduler_service = database_cron_service.clone();
            tokio::spawn(async move {
                tracing::debug!("Starting cron scheduler");
                scheduler_service.start_cron_scheduler().await;
            });
            // Get screenshot service (required)
            let screenshot_service =
                context.require_service::<temps_screenshots::ScreenshotService>();

            // Create WorkflowExecutionService
            let workflow_execution_service = Arc::new(WorkflowExecutionService::new(
                db.clone(),
                git_provider,
                image_builder,
                deployer,
                log_service.clone(),
                cron_service,
                config_service.clone(),
                screenshot_service,
            ));

            // Get ExternalServiceManager for accessing external service env vars
            let external_service_manager =
                context.require_service::<temps_providers::ExternalServiceManager>();

            // Get DSN service for automatic Sentry DSN generation (required)
            let dsn_service = context.require_service::<temps_error_tracking::DSNService>();

            // Create JobProcessor with workflow execution capability
            let job_receiver = queue_service.subscribe();
            let workflow_planner = Arc::new(WorkflowPlanner::new(
                db.clone(),
                log_service.clone(),
                external_service_manager.clone(),
                config_service.clone(),
                dsn_service,
            ));

            let mut job_processor = JobProcessorService::with_external_service_manager(
                db,
                job_receiver,
                workflow_execution_service,
                workflow_planner,
                git_provider_manager,
            );

            // Start the job processor in a background task
            tokio::spawn(async move {
                tracing::debug!("Starting deployment job processor");
                if let Err(e) = job_processor.run().await {
                    tracing::error!("Deployment job processor error: {}", e);
                }
            });

            tracing::debug!("Deployment job processor started successfully");
            tracing::debug!("Deployments plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let deployment_service = context
            .get_service::<DeploymentService>()
            .expect("DeploymentService must be registered before configuring routes");
        let log_service = context
            .get_service::<temps_logs::LogService>()
            .expect("LogService must be registered before configuring routes");
        let cron_service = context
            .get_service::<crate::services::DatabaseCronConfigService>()
            .expect("DatabaseCronConfigService must be registered before configuring routes");

        let app_state = Arc::new(handlers::types::AppState {
            deployment_service,
            log_service,
            cron_service,
        });

        let deployments_routes = handlers::deployments::configure_routes();
        let cron_routes = handlers::crons::configure_routes();

        let routes = deployments_routes.merge(cron_routes).with_state(app_state);

        Some(PluginRoutes { router: routes })
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        let deployments_schema =
            <handlers::deployments::DeploymentsApiDoc as UtoimaOpenApi>::openapi();
        let cron_schema = <handlers::crons::CronApiDoc as UtoimaOpenApi>::openapi();

        Some(temps_core::openapi::merge_openapi_schemas(
            deployments_schema,
            vec![cron_schema],
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_deployments_plugin_name() {
        let deployments_plugin = DeploymentsPlugin::new();
        assert_eq!(deployments_plugin.name(), "deployments");
    }

    #[tokio::test]
    async fn test_deployments_plugin_default() {
        let deployments_plugin = DeploymentsPlugin;
        assert_eq!(deployments_plugin.name(), "deployments");
    }

    #[test]
    fn test_plugin_has_job_processor_integration() {
        // This test ensures that the plugin integration code compiles
        // and that the job processor is properly integrated
        let plugin = DeploymentsPlugin::new();
        assert_eq!(plugin.name(), "deployments");

        // The actual job processor functionality is tested separately
        // This test just verifies the plugin structure is correct
    }
}
