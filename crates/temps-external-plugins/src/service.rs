//! Service layer for external plugin management.
//!
//! Orchestrates plugin lifecycle (discovery, proxy creation, event delivery)
//! and provides a clean API consumed by the handler and plugin layers.

use std::sync::Arc;

use axum::Router;
use temps_core::external_plugin::PluginManifest;
use temps_core::JobQueue;
use tokio::sync::RwLock;
use tracing::{debug, error, info};

use crate::event_listener::PluginEventListener;
use crate::manager::{ExternalPluginConfig, ExternalPluginManager};
use crate::proxy;

/// Service that manages the external plugin lifecycle and provides data
/// to the handler layer.
pub struct ExternalPluginsService {
    manager: Arc<ExternalPluginManager>,
    /// Cached manifests from discovery — refreshed on reload.
    manifests: RwLock<Vec<PluginManifest>>,
    /// Event listener that delivers platform events to subscribing plugins
    event_listener: RwLock<Option<PluginEventListener>>,
    /// Optional job queue for event delivery (stored for reload)
    queue: Option<Arc<dyn JobQueue>>,
    /// Swappable proxy router — rebuilt on reload so new/removed plugins
    /// are reflected without restarting the server.
    proxy_router: Arc<RwLock<Router>>,
}

impl ExternalPluginsService {
    /// Create a "shell" service with no discovered plugins yet.
    ///
    /// This returns immediately — plugin discovery (which can take up to
    /// `handshake_timeout` per binary) does not run. Call
    /// [`start_background_discovery`](Self::start_background_discovery) on
    /// the resulting `Arc<Self>` to populate manifests and the proxy router
    /// in a background task. Until that task completes, proxied requests
    /// for `/x/<plugin>/...` will 404, which is the same outcome as the
    /// plugin never having been started.
    pub fn new_empty(
        config: ExternalPluginConfig,
        queue: Option<Arc<dyn JobQueue>>,
        db: Arc<sea_orm::DatabaseConnection>,
    ) -> Self {
        let manager = Arc::new(ExternalPluginManager::new(config, db));
        Self {
            manager,
            manifests: RwLock::new(Vec::new()),
            event_listener: RwLock::new(None),
            queue,
            proxy_router: Arc::new(RwLock::new(Router::new())),
        }
    }

    /// Spawn a background task that runs initial plugin discovery + start,
    /// then swaps the resulting proxy router in. Safe to call once on a
    /// freshly-constructed shell from [`new_empty`](Self::new_empty).
    pub fn start_background_discovery(self: Arc<Self>) {
        tokio::spawn(async move {
            let manifests = self.manager.discover_and_start().await;

            if !manifests.is_empty() {
                info!(
                    "Loaded {} external plugin(s) in background: {}",
                    manifests.len(),
                    manifests
                        .iter()
                        .map(|m| m.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            } else {
                debug!("No external plugins discovered (background)");
            }

            let new_listener =
                Self::start_event_listener(&self.manager, &manifests, self.queue.as_ref()).await;
            let new_router = Self::build_proxy_router_from(&self.manager, &manifests).await;

            {
                let mut router = self.proxy_router.write().await;
                *router = new_router;
            }
            {
                let mut listener = self.event_listener.write().await;
                *listener = new_listener;
            }
            {
                let mut cached = self.manifests.write().await;
                *cached = manifests;
            }
        });
    }

    /// Create the service and immediately discover + start all plugins.
    ///
    /// If a `JobQueue` is provided and any discovered plugins subscribe to
    /// events, a [`PluginEventListener`] is started automatically.
    pub async fn new(
        config: ExternalPluginConfig,
        queue: Option<Arc<dyn JobQueue>>,
        db: Arc<sea_orm::DatabaseConnection>,
    ) -> Self {
        let manager = Arc::new(ExternalPluginManager::new(config, db));
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

        // Start the event listener if any plugin subscribes to events
        let event_listener = Self::start_event_listener(&manager, &manifests, queue.as_ref()).await;

        // Build the initial proxy router
        let proxy_router = Self::build_proxy_router_from(&manager, &manifests).await;

        Self {
            manager,
            manifests: RwLock::new(manifests),
            event_listener: RwLock::new(event_listener),
            queue,
            proxy_router: Arc::new(RwLock::new(proxy_router)),
        }
    }

    /// Get a snapshot of the current plugin manifests.
    pub async fn manifests(&self) -> Vec<PluginManifest> {
        self.manifests.read().await.clone()
    }

    /// Get the swappable proxy router reference.
    ///
    /// The routing layer holds an `Arc` to this and reads it per-request,
    /// so swapping the inner `Router` via [`reload_plugins`] takes effect
    /// immediately for new requests.
    pub fn proxy_router(&self) -> Arc<RwLock<Router>> {
        self.proxy_router.clone()
    }

    /// Build the initial proxy router (used once during startup for the
    /// pre-built router pattern).
    pub async fn build_initial_proxy_router(&self) -> Router {
        self.proxy_router.read().await.clone()
    }

    /// Reload all external plugins.
    ///
    /// 1. Stops the event listener
    /// 2. Shuts down all running plugin processes
    /// 3. Re-scans the plugins directory and starts all discovered binaries
    /// 4. Rebuilds the proxy router
    /// 5. Restarts the event listener if needed
    ///
    /// Returns the manifests of all successfully started plugins.
    pub async fn reload_plugins(&self) -> Vec<PluginManifest> {
        // Stop event listener
        {
            let mut listener = self.event_listener.write().await;
            if let Some(l) = listener.take() {
                l.stop().await;
            }
        }

        // Reload all plugins via manager (shutdown + re-discover + re-start)
        let new_manifests = self.manager.reload_all().await;

        info!(
            "Reloaded {} external plugin(s): {}",
            new_manifests.len(),
            new_manifests
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );

        // Rebuild proxy router and swap it in
        let new_router = Self::build_proxy_router_from(&self.manager, &new_manifests).await;
        {
            let mut router = self.proxy_router.write().await;
            *router = new_router;
        }

        // Restart event listener
        {
            let new_listener =
                Self::start_event_listener(&self.manager, &new_manifests, self.queue.as_ref())
                    .await;
            let mut listener = self.event_listener.write().await;
            *listener = new_listener;
        }

        // Update cached manifests
        {
            let mut manifests = self.manifests.write().await;
            *manifests = new_manifests.clone();
        }

        new_manifests
    }

    /// Shut down all external plugins gracefully.
    pub async fn shutdown_all(&self) {
        let mut listener = self.event_listener.write().await;
        if let Some(l) = listener.take() {
            l.stop().await;
        }
        self.manager.shutdown_all().await;
    }

    /// Get a reference to the underlying manager.
    pub fn manager(&self) -> &Arc<ExternalPluginManager> {
        &self.manager
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    /// Build a proxy router from a set of manifests.
    async fn build_proxy_router_from(
        manager: &ExternalPluginManager,
        manifests: &[PluginManifest],
    ) -> Router {
        let mut router = Router::new();

        for manifest in manifests {
            if let Some(proxy) = manager.proxy_for(&manifest.name).await {
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

    /// Start event listener if any plugins subscribe to events.
    async fn start_event_listener(
        manager: &Arc<ExternalPluginManager>,
        manifests: &[PluginManifest],
        queue: Option<&Arc<dyn JobQueue>>,
    ) -> Option<PluginEventListener> {
        let has_event_subscribers = manifests.iter().any(|m| !m.events.is_empty());
        if !has_event_subscribers {
            return None;
        }

        let queue = match queue {
            Some(q) => q.clone(),
            None => {
                debug!(
                    "Plugins subscribe to events but no JobQueue provided — event delivery disabled"
                );
                return None;
            }
        };

        let listener = PluginEventListener::new(manager.clone(), queue);
        if let Err(e) = listener.start().await {
            error!("Failed to start plugin event listener: {}", e);
            None
        } else {
            info!(
                "Plugin event listener started for {} subscribing plugin(s)",
                manifests.iter().filter(|m| !m.events.is_empty()).count()
            );
            Some(listener)
        }
    }
}
