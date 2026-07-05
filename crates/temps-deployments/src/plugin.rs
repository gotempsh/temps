use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use utoipa::{openapi::OpenApi, OpenApi as UtoimaOpenApi};

use crate::{
    handlers,
    services::{
        DeploymentGateSlot, DeploymentService, JobProcessorService, WorkflowExecutionService,
    },
    WorkflowPlanner,
};

/// Deployments Plugin for managing deployment operations
pub struct DeploymentsPlugin {
    /// Handle to the job processor's `deployment_gate` slot, captured in
    /// `register_services` (before the processor is moved into its spawned
    /// task) and written into from `initialize_plugin_services`, which runs
    /// only after every plugin has registered its services. See
    /// `JobProcessorService::deployment_gate` for why this two-phase
    /// handoff is needed: `register_services` runs in plugin-registration
    /// order, and this plugin registers (and starts its processor) before
    /// any later-registered plugin gets a chance to provide a gate.
    deployment_gate_slot: tokio::sync::OnceCell<DeploymentGateSlot>,
}

impl DeploymentsPlugin {
    pub fn new() -> Self {
        Self {
            deployment_gate_slot: tokio::sync::OnceCell::new(),
        }
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
            let encryption_service = context.require_service::<temps_core::EncryptionService>();

            // Anonymous telemetry reporter (optional). Defaults to a no-op when
            // the TelemetryPlugin isn't registered, so deploys never depend on it.
            let telemetry = context
                .get_service::<dyn temps_core::telemetry::TelemetryReporter>()
                .unwrap_or_else(|| Arc::new(temps_core::telemetry::NoopTelemetryReporter));

            // Create DeploymentService
            let deployment_service = Arc::new(DeploymentService::new(
                db.clone(),
                log_service.clone(),
                config_service.clone(),
                queue_service.clone(),
                docker_log_service,
                deployer.clone(),
                encryption_service.clone(),
            ));
            // Wire telemetry for deploy-funnel events (rollback_triggered).
            deployment_service.set_telemetry(telemetry.clone());
            context.register_service(deployment_service.clone());

            // Also register as DeploymentCanceller trait for temps-environments
            let deployment_canceller =
                deployment_service.clone() as Arc<dyn temps_core::DeploymentCanceller>;
            context.register_service(deployment_canceller);

            // Remote container log source — lets the log-aggregator collect logs
            // from containers on remote worker nodes into searchable history. The
            // aggregator picks this up via get_service and runs its reconcile
            // loop; single-node setups simply have nothing to collect.
            let remote_log_source = Arc::new(crate::services::RemoteLogSourceImpl::new(
                db.clone(),
                config_service.clone(),
                encryption_service.clone(),
            ))
                as Arc<dyn temps_log_aggregator::RemoteContainerLogSource>;
            context.register_service(remote_log_source);

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

            // Get encryption service for deployment token encryption (needed by cron service and workflow planner)
            let encryption_service = context.require_service::<temps_core::EncryptionService>();

            // Create DeploymentTokenService for cron secret retrieval
            let deployment_token_service = Arc::new(
                crate::services::deployment_token_service::DeploymentTokenService::new(
                    db.clone(),
                    encryption_service.clone(),
                ),
            );
            // Register so other plugins (e.g. workspace) can resolve it via the service registry.
            context.register_service(deployment_token_service.clone());

            // Create DatabaseCronConfigService to manage cron jobs
            let database_cron_service = Arc::new(crate::services::DatabaseCronConfigService::new(
                db.clone(),
                queue_service.clone(),
                deployment_token_service.clone(),
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

            // Start Docker cleanup scheduler in background (nightly cleanup at 2 AM UTC)
            let cas_dir = config_service.data_dir().join("cas");
            let cleanup_file_store: Arc<dyn temps_file_store::FileStore> =
                Arc::new(temps_file_store::fs_store::FsFileStore::new(cas_dir));
            let docker_cleanup = Arc::new(
                crate::services::DockerCleanupService::new(
                    Arc::new(crate::services::DefaultDockerClient),
                    db.clone(),
                    cleanup_file_store,
                )
                .with_static_dir(config_service.static_dir()),
            );
            tokio::spawn({
                let cleanup_service = docker_cleanup.clone();
                async move {
                    tracing::debug!("Starting Docker cleanup scheduler");
                    cleanup_service.start_cleanup_scheduler().await;
                }
            });

            // Get screenshot service (required)
            let screenshot_service =
                context.require_service::<temps_screenshots::ScreenshotService>();

            // Get static deployer (required)
            let static_deployer =
                context.require_service::<dyn temps_deployer::static_deployer::StaticDeployer>();

            // Create Docker client for container operations
            let docker = Arc::new(
                bollard::Docker::connect_with_local_defaults()
                    .expect("Failed to connect to Docker"),
            );
            // Create WorkflowExecutionService
            let workflow_execution_service = Arc::new(WorkflowExecutionService::new(
                db.clone(),
                queue_service.clone(),
                git_provider,
                image_builder,
                deployer,
                static_deployer,
                log_service.clone(),
                cron_service,
                context
                    .get_service::<dyn crate::jobs::AgentSyncService>()
                    .unwrap_or_else(|| Arc::new(crate::jobs::NoOpAgentSyncService)),
                config_service.clone(),
                screenshot_service,
                docker,
            ));

            // Wire SourceMapService for auto-capture during deployments (optional)
            if let Some(source_map_service) =
                context.get_service::<temps_error_tracking::services::SourceMapService>()
            {
                workflow_execution_service.set_source_map_service(source_map_service);
                tracing::debug!("Source map service wired into workflow execution service");
            }

            // Wire NodeScheduler for multi-node deployments
            let node_service = Arc::new(crate::services::NodeService::new(db.clone()));
            let node_scheduler = Arc::new(crate::services::NodeScheduler::new(node_service));
            workflow_execution_service.set_node_scheduler(node_scheduler);

            // Wire encryption service for decrypting node tokens during remote deployments
            if let Some(encryption_service) = context.get_service::<temps_core::EncryptionService>()
            {
                workflow_execution_service.set_encryption_service(encryption_service);
            }
            tracing::debug!("Node scheduler wired into workflow execution service");

            // Wire content-addressable file store for static asset deduplication
            {
                let cas_dir = config_service.data_dir().join("cas");
                let file_store: Arc<dyn temps_file_store::FileStore> =
                    Arc::new(temps_file_store::fs_store::FsFileStore::new(cas_dir));
                workflow_execution_service.set_file_store(file_store);
                tracing::debug!("File store wired into workflow execution service");
            }

            // Wire telemetry for deploy-funnel events (deploy_attempted,
            // deploy_succeeded, deploy_failed, first_deploy_succeeded).
            workflow_execution_service.set_telemetry(telemetry.clone());
            tracing::debug!("Telemetry wired into workflow execution service");

            // Get ExternalServiceManager for accessing external service env vars
            let external_service_manager =
                context.require_service::<temps_providers::ExternalServiceManager>();

            // Get DSN service for automatic Sentry DSN generation (required)
            let dsn_service = context.require_service::<temps_error_tracking::DSNService>();

            // Wire the shared environment-variable resolver into DeploymentService
            // so the inline promote/rollback deploy paths resolve env from the
            // selected environment (the SAME set as a normal deploy) instead of
            // starting the reused image with no config. See services::env_resolver.
            let env_resolver = Arc::new(crate::services::env_resolver::DeploymentEnvResolver {
                db: db.clone(),
                encryption_service: encryption_service.clone(),
                config_service: config_service.clone(),
                external_service_manager: external_service_manager.clone(),
                dsn_service: dsn_service.clone(),
                deployment_token_service: deployment_token_service.clone(),
            });
            deployment_service.set_env_resolver(env_resolver);

            // Create JobProcessor with workflow execution capability
            let job_receiver = queue_service.subscribe();
            let workflow_planner = Arc::new(WorkflowPlanner::new(
                db.clone(),
                log_service.clone(),
                external_service_manager.clone(),
                config_service.clone(),
                dsn_service,
                encryption_service,
            ));

            // Clone workflow_execution_service before passing to job processor
            // (the job processor takes ownership, but we need to register it too)
            let workflow_execution_service_for_processor = workflow_execution_service.clone();

            let mut job_processor = JobProcessorService::with_external_service_manager(
                db,
                job_receiver,
                queue_service.clone(),
                workflow_execution_service_for_processor,
                workflow_planner,
                git_provider_manager,
            );

            // Capture a handle to the job processor's gate slot before it's
            // moved into the spawned task below. Any plugin that registers
            // after this one would still be unregistered at this point, so
            // looking the gate up here with get_service would always find
            // nothing — initialize_plugin_services (below) does the actual
            // lookup once every plugin has registered.
            if self
                .deployment_gate_slot
                .set(job_processor.deployment_gate_handle())
                .is_err()
            {
                unreachable!("register_services runs exactly once per plugin instance");
            }

            // Start the job processor in a background task
            tokio::spawn(async move {
                tracing::debug!("Starting deployment job processor");
                if let Err(e) = job_processor.run().await {
                    tracing::error!("Deployment job processor error: {}", e);
                }
            });

            tracing::debug!("Deployment job processor started successfully");

            // Get the db connection for RemoteDeploymentService
            let db_for_remote = context.require_service::<sea_orm::DatabaseConnection>();

            // Create RemoteDeploymentService
            let remote_deployment_service =
                Arc::new(crate::services::RemoteDeploymentService::new(db_for_remote));
            context.register_service(remote_deployment_service);

            // Register WorkflowExecutionService for use in remote deployments
            context.register_service(workflow_execution_service);

            tracing::debug!("Deployments plugin services registered successfully");
            Ok(())
        })
    }

    fn initialize_plugin_services<'a>(
        &'a self,
        context: &'a PluginContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Runs after every plugin has registered its services, so this
            // is the first point at which an optional DeploymentGate (e.g.
            // from a plugin implementing manual approvals) can actually be
            // found.
            if let Some(slot) = self.deployment_gate_slot.get() {
                if let Some(gate) = context.get_service::<dyn temps_core::DeploymentGate>() {
                    *slot.write().await = Some(gate);
                    tracing::debug!("Deployment gate wired into job processor");
                }
            }
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let deployment_service = context.require_service::<DeploymentService>();
        let log_service = context.require_service::<temps_logs::LogService>();
        let cron_service = context.require_service::<crate::services::DatabaseCronConfigService>();

        // Optional. configure_routes runs only after every plugin's
        // initialize_plugin_services has completed (see PluginManager::initialize_plugins),
        // so unlike register_services this get_service call reliably finds
        // a gate registered by any plugin.
        let deployment_gate = context.get_service::<dyn temps_core::DeploymentGate>();

        // Create external deployment manager for handling external images and operations
        let external_deployment_manager =
            Arc::new(crate::services::ExternalDeploymentManager::new());

        // Get RemoteDeploymentService for handling remote deployments
        let remote_deployment_service =
            context.require_service::<crate::services::RemoteDeploymentService>();

        // Get services needed for remote deployment triggering
        let db = context.require_service::<sea_orm::DatabaseConnection>();
        let queue_service = context.require_service::<dyn temps_core::JobQueue>();
        let config_service = context.require_service::<temps_config::ConfigService>();
        let external_service_manager =
            context.require_service::<temps_providers::ExternalServiceManager>();
        let dsn_service = context.require_service::<temps_error_tracking::DSNService>();
        let encryption_service = context.require_service::<temps_core::EncryptionService>();

        // Create WorkflowPlanner for remote deployments
        let workflow_planner = Arc::new(WorkflowPlanner::new(
            db.clone(),
            log_service.clone(),
            external_service_manager,
            config_service.clone(),
            dsn_service,
            encryption_service,
        ));

        // Get WorkflowExecutionService
        let workflow_executor = context.require_service::<WorkflowExecutionService>();

        // Get DeploymentTokenService for deployment-token management routes
        let deployment_token_service =
            context.require_service::<crate::services::deployment_token_service::DeploymentTokenService>();

        // Get ImageBuilder for uploading Docker image tarballs
        let image_builder = context.require_service::<dyn temps_deployer::ImageBuilder>();

        // Get BlobService for static bundle uploads
        let blob_service = context.require_service::<temps_blob::BlobService>();

        // Get audit service for logging write operations
        let audit_service = context.require_service::<dyn temps_core::AuditLogger>();

        // Get data directory for local file storage
        let data_dir = config_service.data_dir();

        // Create NodeService for admin node routes (list/get with session auth)
        let node_service = Arc::new(crate::services::NodeService::new(db.clone()));

        // Re-fetch encryption service for AppState (the first ref was moved into WorkflowPlanner)
        let encryption_service = context.require_service::<temps_core::EncryptionService>();

        // Docker client for container exec/terminal
        let docker_for_exec = Arc::new(
            bollard::Docker::connect_with_local_defaults()
                .expect("Failed to connect to Docker for container exec"),
        );

        let app_state = Arc::new(handlers::types::AppState {
            deployment_service,
            log_service,
            cron_service,
            external_deployment_manager,
            remote_deployment_service,
            db,
            workflow_planner,
            workflow_executor,
            queue_service,
            blob_service,
            data_dir,
            image_builder,
            audit_service,
            node_service,
            encryption_service,
            config_service: config_service.clone(),
            docker: docker_for_exec,
            deployment_gate,
        });

        let deployments_routes = handlers::deployments::configure_routes();
        let cron_routes = handlers::crons::configure_routes();
        let external_images_routes = handlers::external_images::configure_routes();
        let remote_deployments_routes = handlers::remote_deployments::configure_routes();
        let admin_node_routes = handlers::nodes::configure_admin_routes();
        let deployment_token_routes = handlers::deployment_tokens::configure_routes().with_state(
            Arc::new(handlers::deployment_tokens::DeploymentTokenAppState {
                deployment_token_service,
            }),
        );

        let routes = deployments_routes
            .merge(cron_routes)
            .merge(external_images_routes)
            .merge(remote_deployments_routes)
            .merge(admin_node_routes)
            .with_state(app_state)
            .merge(deployment_token_routes);

        Some(PluginRoutes::new(routes))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        let deployments_schema =
            <handlers::deployments::DeploymentsApiDoc as UtoimaOpenApi>::openapi();
        let cron_schema = <handlers::crons::CronApiDoc as UtoimaOpenApi>::openapi();
        let external_images_schema =
            <handlers::external_images::ExternalImagesApiDoc as UtoimaOpenApi>::openapi();
        let remote_deployments_schema =
            <handlers::remote_deployments::RemoteDeploymentsApiDoc as UtoimaOpenApi>::openapi();
        let nodes_schema = <handlers::nodes::NodesApiDoc as UtoimaOpenApi>::openapi();
        let deployment_tokens_schema =
            <handlers::deployment_tokens::DeploymentTokensApiDoc as UtoimaOpenApi>::openapi();

        Some(temps_core::openapi::merge_openapi_schemas(
            deployments_schema,
            vec![
                cron_schema,
                external_images_schema,
                remote_deployments_schema,
                nodes_schema,
                deployment_tokens_schema,
            ],
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
        let deployments_plugin = DeploymentsPlugin::default();
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

    #[test]
    fn test_openapi_schema_includes_deployment_token_routes() {
        let plugin = DeploymentsPlugin::new();
        let schema = plugin
            .openapi_schema()
            .expect("deployments plugin should expose OpenAPI schema");
        let paths = schema.paths.paths;

        assert!(
            paths.contains_key("/projects/{project_id}/deployment-tokens"),
            "deployment token collection route should be exposed in plugin OpenAPI schema"
        );
        assert!(
            paths.contains_key("/projects/{project_id}/deployment-tokens/{token_id}"),
            "deployment token item route should be exposed in plugin OpenAPI schema"
        );
    }
}
