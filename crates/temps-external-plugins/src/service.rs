//! Service layer for external plugin management.
//!
//! Orchestrates plugin lifecycle (discovery, proxy creation) and provides
//! a clean API consumed by the handler and plugin layers.

use std::sync::Arc;

use axum::Router;
use temps_core::external_plugin::PluginManifest;
use tracing::{debug, info};

use crate::manager::{ExternalPluginConfig, ExternalPluginManager};
use crate::proxy;

/// Service that manages the external plugin lifecycle and provides data
/// to the handler layer.
pub struct ExternalPluginsService {
    manager: Arc<ExternalPluginManager>,
    /// Cached manifests from discovery (set after `discover_and_start`)
    manifests: Vec<PluginManifest>,
}

impl ExternalPluginsService {
    /// Create the service and immediately discover + start all plugins.
    pub async fn new(config: ExternalPluginConfig) -> Self {
        let manager = Arc::new(ExternalPluginManager::new(config));
        let manifests = manager.discover_and_start().await;

        if !manifests.is_empty() {
            info!(
                "Loaded {} external plugin(s): {}",
                manifests.len(),
                manifests
                    .iter()
                    .map(|m| m.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        } else {
            debug!("No external plugins discovered");
        }

        Self { manager, manifests }
    }

    /// Get the cached list of plugin manifests.
    pub fn manifests(&self) -> &[PluginManifest] {
        &self.manifests
    }

    /// Build a router with proxy routes for all running plugins.
    ///
    /// Returns a router with:
    /// - `/x/{plugin_name}/*` — reverse proxy for each plugin
    pub async fn build_proxy_router(&self) -> Router {
        let mut router = Router::new();

        for manifest in &self.manifests {
            if let Some(proxy) = self.manager.proxy_for(&manifest.name).await {
                let proxy_router = proxy::create_plugin_proxy_router(proxy);
                let prefix = format!("/x/{}", manifest.name);
                debug!(
                    plugin = %manifest.name,
                    prefix = %prefix,
                    "Mounting external plugin proxy"
                );
                router = router.nest(&prefix, proxy_router);
            }
        }

        router
    }

    /// Shut down all external plugins gracefully.
    pub async fn shutdown_all(&self) {
        self.manager.shutdown_all().await;
    }

    /// Get a reference to the underlying manager.
    pub fn manager(&self) -> &Arc<ExternalPluginManager> {
        &self.manager
    }
}
