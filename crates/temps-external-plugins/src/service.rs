// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Service layer for external plugin management.
//!
//! Orchestrates plugin lifecycle (discovery, proxy creation, event delivery)
//! and provides a clean API consumed by the handler and plugin layers.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::Router;
use temps_core::JobQueue;
use temps_plugin_protocol::PluginManifest;
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{debug, error, info};

use crate::event_listener::PluginEventListener;
use crate::manager::{ExternalPluginConfig, ExternalPluginManager, PluginReloadResult};
use crate::proxy;

/// Service that manages the external plugin lifecycle and provides data
/// to the handler layer.
pub struct ExternalPluginsService {
    pub(crate) manager: Arc<ExternalPluginManager>,
    pub(crate) db: Arc<sea_orm::DatabaseConnection>,
    pub(crate) ai_tools: Arc<temps_ai_api_tools::PluginToolsRegistry>,
    /// Cached manifests from discovery — refreshed on reload.
    manifests: RwLock<Vec<PluginManifest>>,
    /// Event listener that delivers platform events to subscribing plugins
    event_listener: RwLock<Option<PluginEventListener>>,
    /// Optional job queue for event delivery (stored for reload)
    queue: Option<Arc<dyn JobQueue>>,
    /// Swappable proxy router — rebuilt on reload so new/removed plugins
    /// are reflected without restarting the server.
    proxy_router: Arc<RwLock<Router>>,
    /// Serializes discovery, reload, adding and removing plugins.
    pub(crate) lifecycle: Arc<tokio::sync::Mutex<()>>,
    pub(crate) ai_recovery: crate::ai_tools::ConsentRecovery,
    source_catalog: crate::source_catalog::SourceCatalog,
    /// Set before shutdown waits for the lifecycle lock so queued mutations
    /// cannot start after shutdown was requested.
    pub(crate) watch_host: std::sync::OnceLock<crate::watches::WatchHost>,
    pub(crate) closing: Arc<AtomicBool>,
}

#[derive(Debug, Error)]
pub enum ExternalPluginsError {
    #[error(transparent)]
    Grant(#[from] crate::grants::GrantError),
    #[error("'{name}' is not a plugin name: a plugin name is lowercase letters, digits and inner hyphens, at most 64 characters")]
    InvalidPluginName { name: String },
    #[error("External plugin service is shutting down and cannot accept lifecycle changes")]
    ShuttingDown,
    #[error("Plugin '{name}' is not installed")]
    NotInstalled { name: String },
    /// The record of a plugin an earlier version installed as an executable
    /// could not be removed, so the plugin is still listed.
    #[error("The installation record of plugin '{name}' at {path} could not be removed: {reason}")]
    RetiredRecord {
        name: String,
        path: String,
        reason: String,
    },
    /// Attaching to the service a plugin runs as failed.
    #[error(transparent)]
    Service(#[from] crate::service_runtime::ServiceAttachError),
    #[error("Could not read the services plugins run as: {source}")]
    ServiceLookup { source: sea_orm::DbErr },
}

impl ExternalPluginsService {
    pub async fn plugin_grants(
        &self,
        name: &str,
    ) -> Result<crate::grants::PluginGrants, crate::grants::GrantError> {
        crate::grants::PluginGrantService::new(self.db.clone())
            .get(name)
            .await
    }

    pub async fn update_plugin_grants(
        &self,
        name: &str,
        config: crate::grants::PluginGrantConfig,
    ) -> Result<crate::grants::PluginGrants, crate::grants::GrantError> {
        crate::grants::PluginGrantService::new(self.db.clone())
            .update(name, config)
            .await
    }

    pub async fn update_plugin_grants_for_actor(
        &self,
        name: &str,
        actor_id: &str,
        config: crate::grants::PluginGrantConfig,
    ) -> Result<crate::grants::PluginGrants, crate::grants::GrantError> {
        crate::grants::PluginGrantService::new(self.db.clone())
            .update_for_actor(name, actor_id, config)
            .await
    }

    /// What the plugin's container may connect to: the outbound profile of
    /// the environment it runs in. `None` when the plugin names no service,
    /// or when the environment keeps the default, which is open.
    pub async fn plugin_outbound_profile(
        &self,
        name: &str,
    ) -> Result<Option<String>, ExternalPluginsError> {
        use sea_orm::EntityTrait;
        let Some(binding) = self.manager.service_binding_of(name).await else {
            return Ok(None);
        };
        let profile = temps_entities::network_policy_profiles::Entity::find_by_id((
            "deployment".to_string(),
            i64::from(binding.environment_id),
        ))
        .one(self.db.as_ref())
        .await
        .map_err(|source| ExternalPluginsError::ServiceLookup { source })?;
        Ok(profile.and_then(|profile| profile.egress))
    }

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
        let manager = ExternalPluginManager::new(config, db.clone()).shared();
        Self {
            manager,
            db,
            manifests: RwLock::new(Vec::new()),
            event_listener: RwLock::new(None),
            queue,
            proxy_router: Arc::new(RwLock::new(Self::empty_proxy_router())),
            lifecycle: Arc::new(tokio::sync::Mutex::new(())),
            ai_recovery: crate::ai_tools::ConsentRecovery::default(),
            source_catalog: crate::source_catalog::SourceCatalog::default(),
            watch_host: Default::default(),
            closing: Arc::new(AtomicBool::new(false)),
            ai_tools: Arc::new(temps_ai_api_tools::PluginToolsRegistry::default()),
        }
    }

    /// Spawn a background task that runs initial plugin discovery + start,
    /// then swaps the resulting proxy router in. Safe to call once on a
    /// freshly-constructed shell from [`new_empty`](Self::new_empty).
    pub fn start_background_discovery(self: Arc<Self>) {
        self.follow_service_plugins();
        tokio::spawn(async move {
            let _lifecycle = self.lifecycle.lock().await;
            if self.closing.load(Ordering::Acquire) {
                return;
            }
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
            self.rebuild_ai_tools().await;
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
        let manager = ExternalPluginManager::new(config, db.clone()).shared();
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

        let service = Self {
            manager,
            db,
            manifests: RwLock::new(manifests),
            event_listener: RwLock::new(event_listener),
            queue,
            proxy_router: Arc::new(RwLock::new(proxy_router)),
            lifecycle: Arc::new(tokio::sync::Mutex::new(())),
            ai_recovery: crate::ai_tools::ConsentRecovery::default(),
            source_catalog: crate::source_catalog::SourceCatalog::default(),
            watch_host: Default::default(),
            closing: Arc::new(AtomicBool::new(false)),
            ai_tools: Arc::new(temps_ai_api_tools::PluginToolsRegistry::default()),
        };
        service.rebuild_ai_tools().await;
        service
    }

    /// Get a snapshot of the current plugin manifests.
    pub async fn manifests(&self) -> Vec<PluginManifest> {
        self.manifests.read().await.clone()
    }

    pub async fn repository_catalog(
        &self,
    ) -> Result<
        Vec<crate::source_catalog::RepositoryCatalogPlugin>,
        crate::source_catalog::SourceCatalogError,
    > {
        self.source_catalog.list().await
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
    /// Returns every successful manifest and every verification/start failure.
    pub async fn reload_plugins(&self) -> Result<PluginReloadResult, ExternalPluginsError> {
        let _lifecycle = self.lifecycle.lock().await;
        if self.closing.load(Ordering::Acquire) {
            return Err(ExternalPluginsError::ShuttingDown);
        }
        // Stop event listener
        {
            let mut listener = self.event_listener.write().await;
            if let Some(l) = listener.take() {
                l.stop().await;
            }
        }

        self.ai_tools
            .publish(temps_ai_api_tools::PluginToolsIndex::default());
        // Detach from every plugin and attach again
        let result = self.manager.reload_all().await;
        let new_manifests = &result.manifests;

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
        let new_router = Self::build_proxy_router_from(&self.manager, new_manifests).await;
        {
            let mut router = self.proxy_router.write().await;
            *router = new_router;
        }

        // Restart event listener
        {
            let new_listener =
                Self::start_event_listener(&self.manager, new_manifests, self.queue.as_ref()).await;
            let mut listener = self.event_listener.write().await;
            *listener = new_listener;
        }

        // Update cached manifests
        {
            let mut manifests = self.manifests.write().await;
            *manifests = new_manifests.clone();
        }

        self.rebuild_ai_tools().await;
        Ok(result)
    }

    /// Remove a plugin.
    ///
    /// A plugin that runs as a service is detached and its actor revoked; the
    /// service keeps running. A plugin an earlier version installed as an
    /// executable is taken off the list and its actor revoked; its releases
    /// and the files it stored are kept.
    pub async fn uninstall_plugin(&self, name: &str) -> Result<(), ExternalPluginsError> {
        if !crate::naming::valid_plugin_name(name) {
            return Err(ExternalPluginsError::InvalidPluginName { name: name.into() });
        }
        let _lifecycle = self.lifecycle.lock().await;
        if self.closing.load(Ordering::Acquire) {
            return Err(ExternalPluginsError::ShuttingDown);
        }
        if self.manager.is_service_plugin(name).await {
            return self.remove_service_plugin(name).await;
        }
        let plugins_dir = &self.manager.config().plugins_dir;
        let record = |error: std::io::Error| ExternalPluginsError::RetiredRecord {
            name: name.into(),
            path: plugins_dir.join(name).display().to_string(),
            reason: error.to_string(),
        };
        if !crate::retired::is_installed(plugins_dir, name)
            .await
            .map_err(record)?
        {
            return Err(ExternalPluginsError::NotInstalled { name: name.into() });
        }
        // Invalidate before the cancellable write: a lost acknowledgement can
        // mean revocation committed. Recovery reloads authoritative consent.
        self.ai_tools
            .publish(self.ai_tools.snapshot().without_plugin(name));
        self.schedule_consent_recovery();
        // Revoke before removing the record. A database failure leaves the
        // plugin listed, so a plugin added later under this name cannot
        // inherit what this one was granted.
        crate::grants::PluginGrantService::new(self.db.clone())
            .revoke_actor(name)
            .await?;
        crate::retired::remove(plugins_dir, name)
            .await
            .map_err(record)?;
        self.manager.forget_retired(name).await;
        Ok(())
    }

    /// Shut down all external plugins gracefully.
    pub async fn shutdown_all(&self) {
        self.closing.store(true, Ordering::Release);
        self.manager.stop_supervising_services();
        self.manager.watch_ready.notify_one();
        self.ai_recovery.cancel();
        self.ai_tools
            .publish(temps_ai_api_tools::PluginToolsIndex::default());
        let _lifecycle = self.lifecycle.lock().await;
        self.ai_recovery.cancel();
        // A lifecycle operation already holding the lock may have published
        // after the early revocation. Revoke its snapshot before stopping.
        self.ai_tools
            .publish(temps_ai_api_tools::PluginToolsIndex::default());
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
    /// The proxy router with no plugin mounted.
    ///
    /// It is reached through a catch-all fallback on the API router (see
    /// `DynamicPluginRouter`), so every API path no other route matches ends
    /// here: answer those with the route-not-found problem rather than axum's
    /// empty 404, which a client cannot tell from a missing object.
    pub(crate) fn empty_proxy_router() -> Router {
        Router::new().fallback(temps_core::api_problems::api_route_not_found)
    }

    async fn build_proxy_router_from(
        manager: &ExternalPluginManager,
        manifests: &[PluginManifest],
    ) -> Router {
        let mut router = Self::empty_proxy_router();

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

    async fn refresh_runtime_surfaces(&self) {
        {
            let mut listener = self.event_listener.write().await;
            if let Some(listener) = listener.take() {
                listener.stop().await;
            }
        }
        let manifests = self.manager.manifests().await;
        let router = Self::build_proxy_router_from(&self.manager, &manifests).await;
        let listener =
            Self::start_event_listener(&self.manager, &manifests, self.queue.as_ref()).await;
        *self.proxy_router.write().await = router;
        *self.event_listener.write().await = listener;
        *self.manifests.write().await = manifests;
        self.rebuild_ai_tools().await;
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

mod plugin_services;
pub use plugin_services::ServicePluginView;

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, DbErr, MockDatabase, MockExecResult};

    /// A server an earlier version installed `reports` 1.4.0 on as an
    /// executable, with one release and a file the plugin stored.
    fn upgraded_server(database: MockDatabase) -> (tempfile::TempDir, ExternalPluginsService) {
        let root = tempfile::tempdir().unwrap();
        let config = ExternalPluginConfig::new(root.path().to_path_buf());
        let installation = config.plugins_dir.join("reports");
        std::fs::create_dir_all(installation.join("1.4.0-abc")).unwrap();
        std::fs::write(installation.join("1.4.0-abc").join("plugin"), "binary").unwrap();
        std::fs::write(
            installation.join("active.json"),
            r#"{"version":"1.4.0","directory":"1.4.0-abc"}"#,
        )
        .unwrap();
        let service =
            ExternalPluginsService::new_empty(config, None, Arc::new(database.into_connection()));
        (root, service)
    }

    #[tokio::test]
    async fn removing_an_installed_executable_revokes_it_and_keeps_its_files() {
        let (root, service) = upgraded_server(
            MockDatabase::new(DatabaseBackend::Postgres).append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }]),
        );
        service.manager.discover_and_start().await;
        assert_eq!(service.manager.retired_plugins().await.len(), 1);
        assert!(service.manifests().await.is_empty());

        service.uninstall_plugin("reports").await.unwrap();

        assert!(service.manager.retired_plugins().await.is_empty());
        let installation = root.path().join("plugins").join("reports");
        assert!(!installation.join("active.json").exists());
        assert!(installation.join("1.4.0-abc").join("plugin").is_file());
        // It is gone: removing it again says so.
        assert!(matches!(
            service.uninstall_plugin("reports").await,
            Err(ExternalPluginsError::NotInstalled { name }) if name == "reports"
        ));
    }

    #[tokio::test]
    async fn a_plugin_whose_permissions_cannot_be_revoked_stays_listed() {
        let (root, service) = upgraded_server(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_errors([DbErr::Custom("revocation unavailable".into())]),
        );
        service.manager.discover_and_start().await;

        let error = service.uninstall_plugin("reports").await.unwrap_err();

        assert!(matches!(error, ExternalPluginsError::Grant(_)), "{error}");
        assert!(root
            .path()
            .join("plugins")
            .join("reports")
            .join("active.json")
            .is_file());
        assert_eq!(service.manager.retired_plugins().await.len(), 1);
    }

    #[tokio::test]
    async fn removing_a_plugin_that_is_not_there_or_not_a_name_is_refused_without_a_write() {
        // No database result is queued: a write would fail the test.
        let (_root, service) = upgraded_server(MockDatabase::new(DatabaseBackend::Postgres));

        assert!(matches!(
            service.uninstall_plugin("exports").await,
            Err(ExternalPluginsError::NotInstalled { name }) if name == "exports"
        ));
        assert!(matches!(
            service.uninstall_plugin("../reports").await,
            Err(ExternalPluginsError::InvalidPluginName { name }) if name == "../reports"
        ));
    }

    #[tokio::test]
    async fn plugins_reload_on_a_server_without_durable_storage() {
        let root = tempfile::tempdir().unwrap();
        let mut config = ExternalPluginConfig::new(root.path().to_path_buf());
        // A plugin is a service, so nothing of it lives on this server's disk.
        config.persistent_installations = false;
        let service = ExternalPluginsService::new_empty(
            config,
            None,
            Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        );

        let result = service.reload_plugins().await.unwrap();

        assert!(result.manifests.is_empty());
        assert!(result.failures.is_empty());
    }

    #[tokio::test]
    async fn nothing_is_removed_once_the_service_is_shutting_down() {
        let (root, service) = upgraded_server(MockDatabase::new(DatabaseBackend::Postgres));
        service.shutdown_all().await;

        assert!(matches!(
            service.uninstall_plugin("reports").await,
            Err(ExternalPluginsError::ShuttingDown)
        ));
        assert!(root
            .path()
            .join("plugins")
            .join("reports")
            .join("active.json")
            .is_file());
    }
}
