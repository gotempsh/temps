//! Blob Plugin implementation for TempsPlugin trait

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use temps_core::AuditLogger;
use temps_providers::externalsvc::RustfsService;
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::handlers::{configure_routes, BlobApiDoc, BlobAppState};
use crate::services::BlobService;

/// RustFS service name used for the Blob plugin
const BLOB_RUSTFS_SERVICE_NAME: &str = "temps-blob";

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
            // version tracking, and automatic upgrades
            let rustfs_service = Arc::new(RustfsService::new(
                BLOB_RUSTFS_SERVICE_NAME.to_string(),
                docker,
                encryption_service,
            ));

            // Create BlobService that uses RustfsService
            let blob_service = Arc::new(BlobService::new(rustfs_service.clone()));

            // Register services
            context.register_service(rustfs_service);
            context.register_service(blob_service);

            tracing::debug!("Blob plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get services from context
        let blob_service = context.require_service::<BlobService>();
        let rustfs_service = context.require_service::<RustfsService>();
        let audit_service = context.require_service::<dyn AuditLogger>();

        // Create app state
        let app_state = Arc::new(BlobAppState {
            blob_service,
            rustfs_service,
            audit_service,
        });

        // Configure routes with state
        let routes = configure_routes().with_state(app_state);

        Some(PluginRoutes { router: routes })
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
        let plugin = BlobPlugin::default();
        assert_eq!(plugin.name(), "blob");
    }
}
