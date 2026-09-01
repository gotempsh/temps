// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Blob Plugin implementation for TempsPlugin trait

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use temps_core::AuditLogger;
use temps_providers::externalsvc::{
    legacy_managed_instance_names, managed_instance_name, ExternalService, RustfsService,
    ServiceType,
};
use temps_providers::ExternalServiceManager;
use tracing::{debug, info, warn};
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::handlers::{configure_routes, BlobApiDoc, BlobAppState};
use crate::services::BlobService;

/// RustFS service name used for the Blob plugin
const BLOB_RUSTFS_SERVICE_NAME: &str = "temps-blob";

/// Report a RustFS container left behind by the earlier managed-service
/// naming split (#495).
///
/// Installs that enabled Blob before that fix carry a second, unused RustFS
/// container under the old prefixed name. Nothing on the platform references
/// it — every lookup goes through `external_services`, which only ever had one
/// row — so it holds a port and its memory until someone removes it by hand.
/// Say so, with the container name and the command, instead of leaving the
/// operator to find it with `docker ps`.
///
/// Detection only. `ExternalServiceManager::delete_service` removes the
/// duplicate, where the operator has asked for the service to go away;
/// removing a container that owns a data volume on an unattended boot is not
/// this function's call to make.
async fn report_legacy_duplicate_containers(
    docker: Arc<bollard::Docker>,
    encryption_service: Arc<temps_core::EncryptionService>,
) {
    for legacy_name in legacy_managed_instance_names(BLOB_RUSTFS_SERVICE_NAME, ServiceType::Blob) {
        // Ask the engine for the container name rather than re-deriving
        // `rustfs-{name}` here — re-deriving it in a second place is the bug
        // this whole change is about.
        let container_name = RustfsService::new(
            legacy_name,
            Arc::clone(&docker),
            Arc::clone(&encryption_service),
        )
        .get_container_name();

        if docker
            .inspect_container(
                &container_name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .is_ok()
        {
            warn!(
                "Found an unused duplicate Blob container '{}' from an older Temps version. \
                 The Blob service reads and writes 'rustfs-{}', so '{}' holds no data the \
                 platform can reach and is only consuming a port and memory. It will be \
                 removed if you delete the Blob service; to clear it now, run: \
                 docker rm -f {} && docker volume rm rustfs_{}_data rustfs_{}_logs",
                container_name,
                BLOB_RUSTFS_SERVICE_NAME,
                container_name,
                container_name,
                container_name.trim_start_matches("rustfs-"),
                container_name.trim_start_matches("rustfs-"),
            );
        }
    }
}

/// Blob Plugin for file storage operations
pub struct BlobPlugin;

impl BlobPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BlobPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for BlobPlugin {
    fn name(&self) -> &'static str {
        "blob"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Get Docker client and encryption service from registry
            let docker = context.require_service::<bollard::Docker>();
            let encryption_service = context.require_service::<temps_core::EncryptionService>();

            // Create RustfsService from temps-providers
            // This leverages temps-providers' container lifecycle management,
            // version tracking, and automatic upgrades.
            //
            // The instance name comes from `managed_instance_name` rather than
            // being spelled out here, so this plugin and
            // `ExternalServiceManager::create_service_instance` provably
            // target the same container. When they disagreed, enabling Blob
            // and restarting produced two RustFS containers and deleting the
            // service removed the wrong one (#495).
            let rustfs_service = Arc::new(RustfsService::new(
                managed_instance_name(BLOB_RUSTFS_SERVICE_NAME, ServiceType::Blob),
                docker,
                encryption_service,
            ));

            // Create BlobService that uses RustfsService
            let blob_service = Arc::new(BlobService::new(rustfs_service.clone()));

            // Register services
            context.register_service(rustfs_service);
            context.register_service(blob_service);

            debug!("Blob plugin services registered successfully");
            Ok(())
        })
    }

    fn initialize_plugin_services<'a>(
        &'a self,
        context: &'a PluginContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            debug!("Initializing Blob plugin services (deferred to background)...");

            // Container start is deferred to a background task so plugin
            // initialization (and thus console API readiness) doesn't block
            // on Docker pull/start. Until the container is up, blob requests
            // will return errors — that's the same behavior as before, just
            // surfaced sooner.
            let rustfs_service = context.require_service::<RustfsService>();
            let external_service_manager = context.require_service::<ExternalServiceManager>();
            let docker = context.require_service::<bollard::Docker>();
            let encryption_service = context.require_service::<temps_core::EncryptionService>();

            tokio::spawn(async move {
                report_legacy_duplicate_containers(docker, encryption_service).await;

                match external_service_manager
                    .get_service_by_name(BLOB_RUSTFS_SERVICE_NAME)
                    .await
                {
                    Ok(service_model) => {
                        if service_model.status == "stopped" {
                            info!(
                                "Blob service '{}' is stopped, skipping initialization. \
                                 Enable the service via the API to use it.",
                                BLOB_RUSTFS_SERVICE_NAME
                            );
                            return;
                        }

                        info!(
                            "Found Blob service '{}' in database (id: {}, status: {}), loading configuration...",
                            BLOB_RUSTFS_SERVICE_NAME, service_model.id, service_model.status
                        );

                        // Goes through the manager rather than calling
                        // `init()` directly so the parameters the engine
                        // infers — notably the port it adopts from the
                        // running container — are written back to the row.
                        // `health_probe` reads that stored port, so dropping
                        // it here makes the health monitor probe a port the
                        // service no longer uses.
                        match external_service_manager
                            .initialize_plugin_service(service_model.id, rustfs_service.as_ref())
                            .await
                        {
                            Ok(()) => {
                                info!(
                                    "Blob RustFS service '{}' initialized successfully from database config",
                                    BLOB_RUSTFS_SERVICE_NAME
                                );
                                if let Err(e) = rustfs_service.start().await {
                                    warn!(
                                        "Failed to start Blob RustFS container (may already be running): {}",
                                        e
                                    );
                                }
                            }
                            Err(e) => {
                                warn!(
                                    "Failed to initialize Blob RustFS service from database config: {}. \
                                     The service will need to be created via the API.",
                                    e
                                );
                            }
                        }
                    }
                    Err(_) => {
                        debug!(
                            "Blob service '{}' not found in database. \
                             It will be created when first accessed via the API.",
                            BLOB_RUSTFS_SERVICE_NAME
                        );
                    }
                }
            });

            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get services from context
        let blob_service = context.require_service::<BlobService>();
        let rustfs_service = context.require_service::<RustfsService>();
        let external_service_manager = context.require_service::<ExternalServiceManager>();
        let audit_service = context.require_service::<dyn AuditLogger>();
        // Optional team-based access checker (present only when a plugin
        // registers one); confines data-plane access to reachable projects.
        let project_access_checker = context.get_service::<dyn temps_core::ProjectAccessChecker>();

        // Create app state
        let app_state = Arc::new(BlobAppState {
            blob_service,
            rustfs_service,
            external_service_manager,
            audit_service,
            project_access_checker,
        });

        // Configure routes with state
        let routes = configure_routes().with_state(app_state);

        Some(PluginRoutes::new(routes))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<BlobApiDoc as OpenApiTrait>::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_blob_plugin_name() {
        let plugin = BlobPlugin::new();
        assert_eq!(plugin.name(), "blob");
    }

    #[tokio::test]
    async fn test_blob_plugin_default() {
        let plugin = BlobPlugin;
        assert_eq!(plugin.name(), "blob");
    }
}
