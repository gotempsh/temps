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
            let external_service_manager = Arc::new(ExternalServiceManager::new_with_handle(
                db.clone(),
                encryption_service.clone(),
                docker_handle.clone(),
                local_workloads.local_workloads_enabled(),
                dns_registry,
            ));
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

            // Managed-service containers live on THIS host's Docker daemon.
            // A process that runs no local workloads has none to reconcile, so
            // both background sweeps below are skipped entirely rather than
            // left to retry against an absent daemon. The HTTP surface stays
            // registered so the console can still list what exists and explain
            // why provisioning is unavailable here.
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
            crate::handlers::data_import_handlers::configure_routes().with_state(app_state);

        let router = providers_routes
            .merge(pg_stat_routes)
            .merge(data_import_routes);
        Some(PluginRoutes::new(router))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        use temps_core::openapi::merge_openapi_schemas;
        let base = <handlers::ExternalServiceApiDoc as OpenApiTrait>::openapi();
        use crate::handlers::pg_stat_statements_handlers::PgStatStatementsApiDoc;
        let pg_stat = <PgStatStatementsApiDoc as OpenApiTrait>::openapi();
        use crate::handlers::data_import_handlers::DataImportApiDoc;
        let data_import = <DataImportApiDoc as OpenApiTrait>::openapi();
        Some(merge_openapi_schemas(base, vec![pg_stat, data_import]))
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
