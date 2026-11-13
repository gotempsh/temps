// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use sea_orm::EntityTrait;
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

    fn required_services(&self) -> Vec<temps_core::plugin::RequiredService> {
        use temps_core::plugin::RequiredService;
        vec![
            RequiredService::of::<sea_orm::DatabaseConnection>(),
            RequiredService::of::<temps_core::EncryptionService>(),
            RequiredService::of::<temps_core::DockerHandle>(),
        ]
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
            let docker_handle = context.require_service::<temps_core::DockerHandle>();

            // Create ExternalServiceManager. The DnsRegistry is constructed
            // here (not pulled from the registry) because it's a thin wrapper
            // over the same DatabaseConnection — going through the registry
            // would force a plugin-init ordering constraint with no benefit.
            let dns_registry = Arc::new(temps_dns::DnsRegistry::new(db.clone()));
            let local_workloads = temps_core::policy_or_default(
                context.get_service::<temps_core::LocalWorkloadPolicy>(),
            );
            let mut external_service_manager = ExternalServiceManager::new_with_handle(
                db.clone(),
                encryption_service.clone(),
                docker_handle.clone(),
                local_workloads.local_workloads_enabled(),
                dns_registry,
            );
            // The control plane's cluster resolver slot (ADR-024). Registered
            // by `temps serve`; filled only while cluster DNS is enabled and
            // the resolver answers, so an unregistered or empty slot simply
            // leaves local cluster members on Docker's embedded DNS.
            if let Some(slot) = context.get_service::<std::sync::RwLock<Option<std::net::IpAddr>>>()
            {
                external_service_manager = external_service_manager.with_cluster_dns_slot(slot);
            }
            // ADR-102: the one place a worker's S3 credential is derived,
            // shared by the manager and the backup engines. The Cloud
            // plugin's minter is handed over at initialisation.
            let worker_credentials = Arc::new(crate::worker_s3_scope::WorkerCredentialScoper::new(
                encryption_service.clone(),
            ));
            context.register_service(worker_credentials.clone());
            let external_service_manager =
                Arc::new(external_service_manager.with_worker_credentials(worker_credentials));
            context.register_service(external_service_manager.clone());

            // Imports of external databases into managed services. Registered
            // on every process so the console can always explain whether an
            // import is possible here.
            let data_import_service = Arc::new(crate::data_import::DataImportService::new(
                db.clone(),
                external_service_manager.clone(),
                docker_handle,
            ));
            context.register_service(data_import_service.clone());

            let sandbox_runtime_credentials: Arc<
                dyn temps_core::SandboxRuntimeCredentialsProvider,
            > = external_service_manager.clone();
            context.register_service(sandbox_runtime_credentials);

            // Register the cross-crate ProjectEnvVarsProvider so the environments
            // plugin can assemble the resolved (manual + integration) env-var view
            // without depending on this crate.
            let env_vars_provider: Arc<dyn temps_core::ProjectEnvVarsProvider> = Arc::new(
                ExternalServicesEnvProvider::new(external_service_manager.clone(), db.clone()),
            );
            context.register_service(env_vars_provider);

            // ADR-102: a worker PostgreSQL archives with a short-lived
            // credential scoped to its own repository; keep it fresh. Runs on
            // every profile: a control plane without local workloads is
            // exactly the one whose databases are on workers.
            external_service_manager.spawn_worker_archive_credential_refresh();

            // Managed-service containers live on THIS host's Docker daemon.
            // A process that runs no local workloads has none to reconcile, so
            // both background sweeps below are skipped entirely rather than
            // left to retry against an absent daemon. The HTTP surface stays
            // registered so the console can still list what exists and explain
            // why provisioning is unavailable here.
            // The healthcheck overrides every managed container is created
            // with, loaded before routes serve so no create races them. Also
            // on a process without local workloads: the workload specs of
            // worker databases are computed here. Reads only rows with an
            // override, so a default install reads none. Reloaded
            // periodically after that, so what another process sharing the
            // database saves reaches the containers this one creates.
            let healthcheck_defaults = Arc::new(crate::services::HealthcheckDefaultsService::new(
                context.require_service::<temps_config::ConfigService>(),
                external_service_manager.clone(),
            ));
            match healthcheck_defaults.reload_registry().await {
                Ok(services) => {
                    tracing::info!(services, "Loaded managed service healthcheck overrides")
                }
                Err(error) => tracing::error!(
                    error = %error,
                    "Could not load the managed service healthcheck overrides; new containers \
                     use the instance default until the next refresh"
                ),
            }
            healthcheck_defaults.spawn_registry_refresh();
            context.register_service(healthcheck_defaults);

            if !local_workloads.local_workloads_enabled() {
                tracing::info!(
                    profile = local_workloads.profile(),
                    "local workloads are disabled for this process; not starting managed-service \
                     cluster reconcilers or standalone-service DNS reconciliation"
                );
                tracing::debug!("Providers plugin services registered successfully");
                return Ok(());
            }

            // Imports left `running` by a previous process have no owner any
            // more. Snapshot them now, before this process can start one, so
            // a fresh import is never mistaken for an orphan; then stop their
            // helpers and mark them interrupted in the background.
            // The snapshot must be taken here, before routes serve, so a
            // failed read is retried a few times in place rather than later.
            let mut snapshot = crate::data_import::active_import_run_ids(db.as_ref()).await;
            for delay_secs in [1, 2, 4] {
                if snapshot.is_ok() {
                    break;
                }
                tracing::warn!(
                    retry_in_secs = delay_secs,
                    "Could not list data imports interrupted by a restart; retrying"
                );
                tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
                snapshot = crate::data_import::active_import_run_ids(db.as_ref()).await;
            }
            match snapshot {
                Ok(run_ids) if run_ids.is_empty() => {}
                Ok(run_ids) => {
                    let data_import_service = data_import_service.clone();
                    tokio::spawn(async move {
                        let settled = data_import_service.reconcile_interrupted(&run_ids).await;
                        tracing::info!(
                            found = run_ids.len(),
                            settled,
                            "Reconciled data imports interrupted by a restart"
                        );
                    });
                }
                Err(error) => tracing::error!(
                    error = %error,
                    "Could not list data imports interrupted by a restart; they stay running \
                     until the next start"
                ),
            }

            // ADR-055 Phase 4: which control-plane databases are created
            // from their workload spec (`enforce`), then the bounded shadow
            // pass for `shadow` ones. Both read only rows whose mode is not
            // `legacy`, so a default install does no work here.
            // The registry is loaded before routes serve, so no create races it.
            if let Err(error) = external_service_manager.load_workload_registry().await {
                tracing::error!(
                    error = %error,
                    "Could not load which databases are in workload mode 'enforce'; their \
                     engines use the legacy create request until the next start"
                );
            }
            external_service_manager.spawn_workload_shadow_loop();

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

            // Multi-node networking can be enabled or repaired while the
            // control plane keeps running (`temps network setup-multi-node`).
            // Re-publish standalone service records periodically so existing
            // control-plane PostgreSQL/Redis/etc. containers are attached to
            // the newly-created overlay without a process restart.
            let manager_for_dns = external_service_manager.clone();
            let db_for_dns = db.clone();
            tokio::spawn(async move {
                loop {
                    let overlay_ready = match temps_entities::network_config::Entity::find_by_id(1)
                        .one(db_for_dns.as_ref())
                        .await
                    {
                        Ok(Some(config)) => config.control_plane_overlay_ready,
                        Ok(None) => false,
                        Err(error) => {
                            tracing::warn!(
                                error = %error,
                                "Could not inspect control-plane overlay readiness"
                            );
                            false
                        }
                    };
                    if !overlay_ready {
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                        continue;
                    }
                    match manager_for_dns.list_services().await {
                        Ok(services) => {
                            for service in services {
                                if let Err(error) = manager_for_dns
                                    .register_standalone_service_dns(service.id)
                                    .await
                                {
                                    tracing::warn!(
                                        service_id = service.id,
                                        service_name = %service.name,
                                        error = %error,
                                        "Could not reconcile standalone managed-service DNS"
                                    );
                                }
                            }
                        }
                        Err(error) => tracing::warn!(
                            error = %error,
                            "Could not list managed services for DNS reconciliation"
                        ),
                    }
                    // Service create/start paths publish immediately. This is
                    // only a bounded recovery sweep for runtime enablement or
                    // external Docker drift, not a hot polling path.
                    tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                }
            });

            tracing::debug!("Providers plugin services registered successfully");
            Ok(())
        })
    }

    fn initialize_plugin_services<'a>(
        &'a self,
        context: &'a PluginContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // ADR-102: a managed source's worker credentials are minted by
            // Temps Cloud. `get_service`: a build without the Cloud plugin
            // has no managed source, and refuses one with the reason.
            let worker_credentials =
                context.require_service::<crate::worker_s3_scope::WorkerCredentialScoper>();
            match context.get_service::<dyn temps_core::ManagedBackupScopedCredentials>() {
                Some(minter) => worker_credentials.set_managed_minter(minter),
                None => tracing::info!(
                    "Cloud plugin not registered; worker jobs on a Temps Cloud managed source \
                     are refused (no scoped credential can be minted)"
                ),
            }
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
        let worker_metrics_status =
            context.get_service::<temps_metrics::WorkerMetricsStatusBoard>();

        // DB connection for direct queries (alert rules CRUD, etc.)
        let db = context.require_service::<sea_orm::DatabaseConnection>();

        // API key service — needed to provision si_ ingest keys for OTLP-push services.
        let api_key_service = context.require_service::<temps_auth::ApiKeyService>();

        // Config service — resolves the internal URL containers push OTLP to.
        let config_service = context.get_service::<temps_config::ConfigService>();

        // Telemetry reporter — no-op when telemetry isn't registered.
        let telemetry = context
            .get_service::<dyn temps_core::telemetry::TelemetryReporter>()
            .unwrap_or_else(|| Arc::new(temps_core::telemetry::NoopTelemetryReporter));

        // Create QueryService
        let query_service = Arc::new(crate::QueryService::new(external_service_manager.clone()));

        let project_access_checker = context.get_service::<dyn temps_core::ProjectAccessChecker>();
        let application_network_reconciler =
            context.get_service::<dyn temps_core::ApplicationDataNetworkReconciler>();
        let data_import_service =
            context.require_service::<crate::data_import::DataImportService>();
        let sensitive_action_authorizer =
            context.require_service::<dyn temps_core::SensitiveActionAuthorizer>();
        let notification_service =
            context.require_service::<dyn temps_core::notifications::NotificationService>();

        // Create AppState for handlers
        let app_state = Arc::new(AppState {
            external_service_manager,
            audit_service,
            query_service,
            health_monitor,
            metrics_store,
            worker_metrics_status,
            db,
            api_key_service,
            config_service,
            telemetry,
            project_access_checker,
            application_network_reconciler,
            data_import_service,
            sensitive_action_authorizer,
            notification_service,
        });

        // Configure routes with the app state
        let providers_routes = handlers::configure_routes().with_state(app_state.clone());
        let pg_stat_routes = crate::handlers::pg_stat_statements_handlers::configure_routes()
            .with_state(app_state.clone());
        let data_import_routes =
            crate::handlers::data_import_handlers::configure_routes().with_state(app_state.clone());
        let workload_routes =
            crate::handlers::workload_handlers::configure_routes().with_state(app_state.clone());
        let server_tls_routes =
            crate::handlers::server_tls_handlers::configure_routes().with_state(app_state.clone());
        let healthcheck_defaults =
            context.require_service::<crate::services::HealthcheckDefaultsService>();
        let healthcheck_routes = crate::handlers::healthcheck_handlers::configure_routes()
            .with_state(Arc::new(
                crate::handlers::healthcheck_handlers::HealthcheckState {
                    app: app_state,
                    defaults: healthcheck_defaults,
                },
            ));

        let router = providers_routes
            .merge(pg_stat_routes)
            .merge(data_import_routes)
            .merge(workload_routes)
            .merge(server_tls_routes)
            .merge(healthcheck_routes);
        Some(PluginRoutes::new(router))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        use temps_core::openapi::merge_openapi_schemas;
        let base = <handlers::ExternalServiceApiDoc as OpenApiTrait>::openapi();
        use crate::handlers::pg_stat_statements_handlers::PgStatStatementsApiDoc;
        let pg_stat = <PgStatStatementsApiDoc as OpenApiTrait>::openapi();
        use crate::handlers::data_import_handlers::DataImportApiDoc;
        let data_import = <DataImportApiDoc as OpenApiTrait>::openapi();
        use crate::handlers::workload_handlers::WorkloadModeApiDoc;
        let workload = <WorkloadModeApiDoc as OpenApiTrait>::openapi();
        use crate::handlers::server_tls_handlers::ServerTlsApiDoc;
        let server_tls = <ServerTlsApiDoc as OpenApiTrait>::openapi();
        use crate::handlers::healthcheck_handlers::HealthcheckApiDoc;
        let healthcheck = <HealthcheckApiDoc as OpenApiTrait>::openapi();
        Some(merge_openapi_schemas(
            base,
            vec![pg_stat, data_import, workload, server_tls, healthcheck],
        ))
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
