//! TempsPlugin implementation for external plugin management.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::extract::Request;
use axum::response::IntoResponse;
use axum::Router;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use temps_core::JobQueue;
use tokio::sync::RwLock;
use tower::ServiceExt;
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::handler::{self, ExternalPluginsApiDoc, ExternalPluginsAppState};
use crate::manager::ExternalPluginConfig;
use crate::service::ExternalPluginsService;
use tracing::{debug, warn};

/// Swappable proxy router holder.
///
/// The `Arc<RwLock<Router>>` is read on every proxied request and written to
/// during [`ExternalPluginsService::reload_plugins`]. This allows hot-swapping
/// plugin proxy routes without restarting the server.
pub(crate) struct DynamicPluginRouter {
    pub inner: Arc<RwLock<Router>>,
}

/// External plugins plugin — discovers, manages, and proxies standalone
/// binary plugins following the TempsPlugin lifecycle.
pub struct ExternalPluginsPlugin {
    config: ExternalPluginConfig,
    /// OpenAPI schemas collected from running external plugins during startup.
    /// Populated synchronously at the end of `register_services()` so it is
    /// available when the synchronous `openapi_schema()` trait method is called.
    cached_schemas: std::sync::Mutex<Option<Vec<(String, utoipa::openapi::OpenApi)>>>,
}

impl ExternalPluginsPlugin {
    pub fn new(config: ExternalPluginConfig) -> Self {
        Self {
            config,
            cached_schemas: std::sync::Mutex::new(None),
        }
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
            // Try to get the JobQueue from the service registry (registered by queue plugin).
            // This is optional — event delivery is disabled if no queue is available.
            let queue: Option<Arc<dyn JobQueue>> = context.get_service::<dyn JobQueue>();

            // Get the database connection for the platform channel.
            let db = context.require_service::<sea_orm::DatabaseConnection>();

            // Create the service — this discovers and starts all external plugins,
            // and starts the event listener if plugins subscribe to events.
            let service =
                Arc::new(ExternalPluginsService::new(self.config.clone(), queue, db).await);

            // Get the swappable router reference. On reload, the service writes
            // a new Router into this Arc<RwLock<Router>> and subsequent requests
            // pick it up immediately.
            let dynamic_router = service.proxy_router();

            // Register the handler app state
            let app_state = Arc::new(ExternalPluginsAppState {
                service: service.clone(),
            });

            // Cache the OpenAPI schemas synchronously so they are available when
            // the synchronous openapi_schema() trait method is called later.
            let schemas = service.manager().openapi_schemas().await;
            {
                let mut cache = self.cached_schemas.lock().unwrap();
                *cache = Some(schemas);
            }

            context.register_service(service);
            context.register_service(app_state);
            context.register_service(Arc::new(DynamicPluginRouter {
                inner: dynamic_router,
            }));

            tracing::debug!("External plugins services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let app_state = context.require_service::<ExternalPluginsAppState>();
        let dynamic = context.require_service::<DynamicPluginRouter>();

        // Build the listing + admin routes (/x/plugins, /x/plugins/reload)
        let listing_router = handler::configure_routes().with_state((*app_state).clone());

        // Create a dynamic routing layer that reads the swappable proxy router
        // on every request. When reload_plugins() swaps the inner Router, all
        // subsequent requests are routed to the new plugins.
        let router_ref = dynamic.inner.clone();
        let dynamic_proxy = Router::new().fallback(move |request: Request| {
            let router_ref = router_ref.clone();
            async move {
                let router = router_ref.read().await.clone();
                router.oneshot(request).await.into_response()
            }
        });

        let combined_router = listing_router.merge(dynamic_proxy);

        Some(PluginRoutes::new(combined_router))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        // Start with the base admin API doc
        let mut combined = ExternalPluginsApiDoc::openapi();

        // Get the cached schemas (populated during register_services)
        let schemas = match self.cached_schemas.lock() {
            Ok(cache) => cache.clone(),
            Err(e) => {
                warn!("Failed to lock cached schemas: {}", e);
                return Some(combined);
            }
        };

        let schemas = match schemas {
            Some(s) => s,
            None => {
                debug!("No cached schemas available yet");
                return Some(combined);
            }
        };

        // Merge each external plugin's schema with path prefixing
        for (plugin_name, plugin_schema) in schemas {
            let prefix = format!("/x/{}", plugin_name);
            debug!(plugin = %plugin_name, prefix = %prefix, "Merging external plugin OpenAPI schema");

            // Prefix all paths in the plugin schema
            let mut prefixed_schema = plugin_schema;
            let mut new_paths = std::collections::BTreeMap::new();
            let old_paths = std::mem::take(&mut prefixed_schema.paths.paths);
            for (path, path_item) in old_paths {
                let prefixed_path = format!("{}{}", prefix, path);
                new_paths.insert(prefixed_path, path_item);
            }
            prefixed_schema.paths.paths = new_paths;

            // Merge using the core merge function
            combined = temps_core::openapi::merge_openapi_schemas(combined, vec![prefixed_schema]);
        }

        Some(combined)
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
