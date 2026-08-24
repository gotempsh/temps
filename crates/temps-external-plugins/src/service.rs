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
    /// Install the bridge plugins use to call the platform's own HTTP API.
    ///
    /// The console builds its router *after* plugins start (the router
    /// contains this crate's routes), so this is how the two are joined up
    /// once both exist.
    pub async fn set_host_api(&self, bridge: Arc<dyn crate::channel::HostApiBridge>) {
        self.manager.set_host_api(bridge).await;
    }

    /// Supply the key material used to mint per-caller actor tokens.
    pub async fn set_actor_crypto(&self, crypto: Arc<temps_core::CookieCrypto>) {
        self.manager.set_actor_crypto(crypto).await;
    }

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

    /// Start or reload a single plugin.
    ///
    /// `plugin_name` is the plugin's own manifest-declared identity (e.g.
    /// `"vibetemps"`) — `ExternalPluginManager` indexes running processes by
    /// exactly this string, so it is what `is_running`/`reload_plugin` must
    /// be called with. `binary_name` is the filename inside the plugins
    /// directory, used only to locate the binary when starting it fresh.
    /// Passing the same value for both is a bug: the binary filename and the
    /// plugin's declared name are not required to match (and for VibeTemps
    /// they don't — `temps-vibetemps-plugin` vs `vibetemps`), so keying the
    /// running-process lookup on the binary filename makes an already-running
    /// plugin permanently report as not running, and makes install/reload
    /// silently leak the old process instead of shutting it down (a fresh
    /// `insert` into the manager's process map overwrites the old entry
    /// without ever calling `.shutdown()` on it).
    ///
    /// If the plugin is already running, it is restarted in-place (shutdown +
    /// re-start from the same binary path). If it is not yet running, the
    /// plugins directory's `binary_name` file is started fresh. After a
    /// successful start the proxy router is rebuilt so the new plugin becomes
    /// reachable immediately without a full server restart.
    ///
    /// Returns the new manifest on success, or an error string on failure.
    pub async fn start_or_reload_plugin(
        &self,
        plugin_name: &str,
        binary_name: &str,
    ) -> Result<temps_core::external_plugin::PluginManifest, String> {
        let result = if self.manager.is_running(plugin_name).await {
            self.manager.reload_plugin(plugin_name).await
        } else {
            // Plugin binary was just installed — start it fresh from the
            // known binary filename inside the plugins directory.
            let binary_path = self.manager.config().plugins_dir.join(binary_name);
            self.manager.start_plugin_by_path(&binary_path).await
        };

        // `start_plugin` indexes the process under the name the *running
        // binary* declares in its own manifest, but every later lookup —
        // `is_running`, `reload_plugin`, the status endpoint — uses
        // `plugin_name`. If the two disagree the process is live but
        // unreachable through the only key we ever query: status reports
        // "not configured" forever, and the next install takes the
        // fresh-start branch and leaks another process on top.
        //
        // Fail loudly instead. The process is shut down via the name it
        // actually registered under, so a wrong registry entry leaves
        // nothing running rather than an orphan nobody can address.
        if let Ok(ref manifest) = result {
            if let Some(message) = identity_mismatch(plugin_name, &manifest.name, binary_name) {
                error!(
                    expected = %plugin_name,
                    declared = %manifest.name,
                    "Plugin declares a different name than its registry entry; shutting it down"
                );
                self.manager.shutdown_plugin(&manifest.name).await;
                return Err(message);
            }
        }

        if let Ok(ref manifest) = result {
            // Rebuild the proxy router so the (re)started plugin is immediately reachable.
            let manifests = self.manager.manifests().await;
            let new_router = Self::build_proxy_router_from(&self.manager, &manifests).await;
            {
                let mut router = self.proxy_router.write().await;
                *router = new_router;
            }
            {
                let mut cached = self.manifests.write().await;
                *cached = manifests;
            }
            info!(plugin = %manifest.name, version = %manifest.version, "Plugin started/reloaded via install flow");
        }

        result
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

/// Decide whether a freshly-started plugin registered under a usable identity.
///
/// `start_plugin` indexes the process under the name the running binary
/// declares in its own manifest, while `is_running`, `reload_plugin` and the
/// status endpoint all look it up by the name this instance knows it as. When
/// those disagree the process is live but unaddressable, so the only safe
/// outcome is to refuse it.
///
/// Split out from `start_or_reload_plugin` so the rule is testable without
/// spawning a real plugin process — this identity conflation has already
/// produced two separate bugs in this code path (first binary-filename vs
/// manifest-name, then registry-declared vs locally-known), and it is silent
/// every time: everything reports success while status reports "not
/// installed".
///
/// Returns `None` when the identities agree, or the operator-facing
/// explanation when they don't.
fn identity_mismatch(expected: &str, declared: &str, binary_name: &str) -> Option<String> {
    if expected == declared {
        return None;
    }
    Some(format!(
        "Plugin binary '{binary_name}' declares the name '{declared}', but this instance knows \
         it as '{expected}'. The process was shut down because it would otherwise run \
         unreachable — status would report it as not installed and reinstalling would start a \
         second copy. This is a packaging mismatch: the registry entry and the plugin's own \
         manifest must agree."
    ))
}

#[cfg(test)]
mod tests {
    use super::identity_mismatch;

    #[test]
    fn matching_identities_are_accepted() {
        assert!(identity_mismatch("vibetemps", "vibetemps", "temps-vibetemps-plugin").is_none());
    }

    #[test]
    fn a_differing_declared_name_is_refused_and_explained() {
        let message = identity_mismatch("vibetemps", "something-else", "temps-vibetemps-plugin")
            .expect("a name the manager will index differently must be refused");
        // Both identities belong in the message: the operator has to know
        // which side to correct, and "install failed" alone doesn't say.
        assert!(message.contains("vibetemps"), "{message}");
        assert!(message.contains("something-else"), "{message}");
        assert!(message.contains("temps-vibetemps-plugin"), "{message}");
    }

    /// Names are compared exactly. The manager's process map is a plain
    /// `HashMap` lookup, so a case or whitespace variant is a different key
    /// and would strand the process just as effectively.
    #[test]
    fn near_miss_names_are_still_a_mismatch() {
        for declared in ["VibeTemps", "vibetemps ", " vibetemps"] {
            assert!(
                identity_mismatch("vibetemps", declared, "bin").is_some(),
                "{declared:?} must not be treated as equal to \"vibetemps\""
            );
        }
    }
}
