//! TempsPlugin implementation for external plugin management.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::handler::{self, ExternalPluginsApiDoc, ExternalPluginsAppState};
use crate::manager::ExternalPluginConfig;
use crate::service::ExternalPluginsService;

/// External plugins plugin — discovers, manages, and proxies standalone
/// binary plugins following the TempsPlugin lifecycle.
pub struct ExternalPluginsPlugin {
    config: ExternalPluginConfig,
}

impl ExternalPluginsPlugin {
    pub fn new(config: ExternalPluginConfig) -> Self {
        Self { config }
    }
}

impl TempsPlugin for ExternalPluginsPlugin {
    fn name(&self) -> &'static str {
        "external-plugins"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Create the service — this discovers and starts all external plugins
            let service = Arc::new(ExternalPluginsService::new(self.config.clone()).await);

            // Register the handler app state
            let app_state = Arc::new(ExternalPluginsAppState {
                service: service.clone(),
            });

            context.register_service(service);
            context.register_service(app_state);

            tracing::debug!("External plugins services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let app_state = context.require_service::<ExternalPluginsAppState>();
        let service = context.require_service::<ExternalPluginsService>();

        // Build the listing route (/x/plugins)
        let listing_router = handler::configure_routes().with_state((*app_state).clone());

        // Build per-plugin proxy routes (/x/{plugin_name}/*)
        // We need a tokio runtime for the async proxy_for calls — but configure_routes is sync.
        // Instead, pre-build the proxy router during service registration.
        // For now, build it synchronously using the cached manifests.
        let mut combined_router = listing_router;

        // Use a blocking approach to build proxy routes from cached manifests
        for manifest in service.manifests() {
            let rt = tokio::runtime::Handle::current();
            if let Some(proxy) = rt.block_on(service.manager().proxy_for(&manifest.name)) {
                let proxy_router = crate::proxy::create_plugin_proxy_router(proxy);
                let prefix = format!("/x/{}", manifest.name);
                tracing::debug!(
                    plugin = %manifest.name,
                    prefix = %prefix,
                    "Mounting external plugin proxy"
                );
                combined_router = combined_router.nest(&prefix, proxy_router);
            }
        }

        Some(PluginRoutes::new(combined_router))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(ExternalPluginsApiDoc::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_plugin_name() {
        let config = ExternalPluginConfig::new(
            PathBuf::from("/tmp/test"),
            "postgres://localhost/test".to_string(),
        );
        let plugin = ExternalPluginsPlugin::new(config);
        assert_eq!(plugin.name(), "external-plugins");
    }

    #[test]
    fn test_plugin_openapi_schema() {
        let config = ExternalPluginConfig::new(
            PathBuf::from("/tmp/test"),
            "postgres://localhost/test".to_string(),
        );
        let plugin = ExternalPluginsPlugin::new(config);
        let schema = plugin.openapi_schema();
        assert!(schema.is_some());
        let spec = schema.unwrap();
        assert!(spec.paths.paths.contains_key("/x/plugins"));
    }
}
