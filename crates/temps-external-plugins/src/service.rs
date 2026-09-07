// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Service layer for external plugin management.
//!
//! Orchestrates plugin lifecycle (discovery, proxy creation, event delivery)
//! and provides a clean API consumed by the handler and plugin layers.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::Router;
use temps_core::external_plugin::PluginManifest;
use temps_core::JobQueue;
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{debug, error, info};

use crate::catalog::{
    validate_url, CatalogError, RegistryClient, RegistryPlugin, VerifiedRegistry,
};
use crate::event_listener::PluginEventListener;
use crate::install::{
    normalize_digest, platform_target, validate_plugin_name, validate_version, InstallError,
    PluginInstaller,
};
use crate::manager::{ExternalPluginConfig, ExternalPluginManager, PluginReloadResult};
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
    /// Serializes discovery, reload, and install/promotion lifecycles.
    lifecycle: tokio::sync::Mutex<()>,
    /// Set before shutdown waits for the lifecycle lock so queued mutations
    /// cannot start after shutdown was requested.
    closing: AtomicBool,
}

#[derive(Debug, Error)]
pub enum ExternalPluginsError {
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error(transparent)]
    Install(#[from] InstallError),
    #[error("Plugin '{name}' is not present in the authenticated registry document")]
    NotInRegistry { name: String },
    #[error("Authenticated registry document contains duplicate entries for plugin '{name}'")]
    DuplicateRegistryEntry { name: String },
    #[error("Plugin '{name}' v{version} failed protocol identity/ready verification; previous active version was preserved: {reason}")]
    CandidateRejected {
        name: String,
        version: String,
        reason: String,
    },
    #[error("External plugin service is shutting down and cannot accept lifecycle changes")]
    ShuttingDown,
}

#[derive(Debug, Clone)]
pub struct InstallOutcome {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub sha256: String,
    pub signer_key_id: String,
    pub registry_source: String,
}

#[derive(Debug, Clone)]
pub struct ReleaseIdentity {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub sha256: String,
    pub signer_key_id: String,
    pub registry_source: String,
}

pub struct SelectedPlugin {
    registry: VerifiedRegistry,
    plugin: RegistryPlugin,
    pub identity: ReleaseIdentity,
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
            lifecycle: tokio::sync::Mutex::new(()),
            closing: AtomicBool::new(false),
        }
    }

    /// Spawn a background task that runs initial plugin discovery + start,
    /// then swaps the resulting proxy router in. Safe to call once on a
    /// freshly-constructed shell from [`new_empty`](Self::new_empty).
    pub fn start_background_discovery(self: Arc<Self>) {
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
            lifecycle: tokio::sync::Mutex::new(()),
            closing: AtomicBool::new(false),
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

        // Reload all plugins via manager (shutdown + re-discover + re-start)
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

        Ok(result)
    }

    /// Fetch and authenticate the complete remote catalogue.
    pub async fn catalog(&self) -> Result<VerifiedRegistry, ExternalPluginsError> {
        let client = RegistryClient::new(self.manager.config().registry.clone())?;
        Ok(client.fetch().await?)
    }

    /// Resolve the exact signed release identity without downloading or
    /// executing it. Handlers use this boundary to durably audit which bytes
    /// are about to run before candidate execution starts.
    pub async fn select_plugin(&self, name: &str) -> Result<SelectedPlugin, ExternalPluginsError> {
        validate_plugin_name(name)?;
        if self.closing.load(Ordering::Acquire) {
            return Err(ExternalPluginsError::ShuttingDown);
        }
        let registry = self.catalog().await?;
        let mut matches = registry
            .document
            .plugins
            .iter()
            .filter(|plugin| plugin.name == name);
        let plugin = matches
            .next()
            .ok_or_else(|| ExternalPluginsError::NotInRegistry {
                name: name.to_string(),
            })?;
        if matches.next().is_some() {
            return Err(ExternalPluginsError::DuplicateRegistryEntry {
                name: name.to_string(),
            });
        }
        let plugin = plugin.clone();
        validate_version(&plugin.name, &plugin.version)?;
        let platform = platform_target()?;
        let release = plugin
            .platforms
            .get(&platform)
            .ok_or_else(|| InstallError::NoRelease {
                plugin: plugin.name.clone(),
                version: plugin.version.clone(),
                platform: platform.clone(),
            })?;
        let sha256 = normalize_digest(&plugin.name, &plugin.version, &release.sha256)?;
        validate_url(&release.url, &self.manager.config().registry, false).map_err(|_| {
            InstallError::UnsafeArtifactUrl {
                plugin: plugin.name.clone(),
                url: release.url.clone(),
            }
        })?;
        let identity = ReleaseIdentity {
            name: plugin.name.clone(),
            version: plugin.version.clone(),
            platform,
            sha256,
            signer_key_id: registry.envelope.key_id.clone(),
            registry_source: self.manager.config().registry.url.clone(),
        };
        Ok(SelectedPlugin {
            registry,
            plugin,
            identity,
        })
    }

    /// Install and activate one preselected signed release without disrupting
    /// its healthy process until the candidate completes its handshake.
    pub async fn install_selected(
        &self,
        selected: SelectedPlugin,
    ) -> Result<InstallOutcome, ExternalPluginsError> {
        let _lifecycle = self.lifecycle.lock().await;
        if self.closing.load(Ordering::Acquire) {
            return Err(ExternalPluginsError::ShuttingDown);
        }

        let installer = PluginInstaller::new(self.manager.config().registry.clone())?;
        installer
            .accept_registry_revision(
                &self.manager.config().plugins_dir,
                selected.registry.document.revision,
            )
            .await?;
        let candidate = installer
            .prepare(
                &self.manager.config().plugins_dir,
                &selected.registry,
                &selected.plugin,
            )
            .await?;
        let pending = match self
            .manager
            .prepare_candidate(
                &candidate.name,
                &candidate.version,
                &candidate.sha256,
                &candidate.binary_path,
            )
            .await
        {
            Ok(pending) => pending,
            Err(reason) => {
                if let Err(cleanup_error) = installer.discard(&candidate).await {
                    tracing::warn!(
                        plugin = %candidate.name,
                        error = %cleanup_error,
                        "Failed to remove rejected plugin candidate"
                    );
                }
                return Err(ExternalPluginsError::CandidateRejected {
                    name: candidate.name.clone(),
                    version: candidate.version.clone(),
                    reason,
                });
            }
        };

        if let Err(error) = installer.activate(&candidate).await {
            self.manager.discard_candidate(pending).await;
            if let Err(cleanup_error) = installer.discard(&candidate).await {
                tracing::warn!(
                    plugin = %candidate.name,
                    error = %cleanup_error,
                    "Failed to remove uncommitted plugin candidate"
                );
            }
            return Err(error.into());
        }
        self.manager.promote_candidate(pending).await;
        self.refresh_runtime_surfaces().await;

        Ok(InstallOutcome {
            name: candidate.name,
            version: candidate.version,
            platform: candidate.platform,
            sha256: candidate.sha256,
            signer_key_id: selected.identity.signer_key_id,
            registry_source: selected.identity.registry_source,
        })
    }

    /// Shut down all external plugins gracefully.
    pub async fn shutdown_all(&self) {
        self.closing.store(true, Ordering::Release);
        let _lifecycle = self.lifecycle.lock().await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn service() -> ExternalPluginsService {
        let database = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        ExternalPluginsService::new_empty(
            ExternalPluginConfig::new(
                std::env::temp_dir().join("temps-external-plugin-service-tests"),
                "postgres://localhost/test".to_string(),
            ),
            None,
            database,
        )
    }

    #[tokio::test]
    async fn catalog_fails_closed_without_production_trust_anchor() {
        let error = service()
            .catalog()
            .await
            .expect_err("unsigned registry access must be unavailable");
        assert!(matches!(
            error,
            ExternalPluginsError::Catalog(CatalogError::TrustNotConfigured { .. })
        ));
    }

    #[tokio::test]
    async fn install_rejects_path_traversal_before_network_access() {
        let error = match service().select_plugin("../../escape").await {
            Ok(_) => panic!("path traversal must be rejected"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            ExternalPluginsError::Install(InstallError::UnsafePluginName { .. })
        ));
    }

    #[tokio::test]
    async fn shutdown_rejects_later_installs_before_registry_access() {
        let service = service();
        service.shutdown_all().await;
        let error = match service.select_plugin("safe").await {
            Ok(_) => panic!("closed service must reject installs"),
            Err(error) => error,
        };
        assert!(matches!(error, ExternalPluginsError::ShuttingDown));
    }

    #[tokio::test]
    async fn test_install_selected_queued_during_shutdown_is_rejected_without_writes() {
        // Arrange: hold the lifecycle lock so shutdown and install both queue,
        // then wait until shutdown has atomically closed admission.
        let temp = tempfile::tempdir().expect("tempdir");
        let config = ExternalPluginConfig::new(
            temp.path().to_path_buf(),
            "postgres://localhost/test".to_string(),
        );
        let plugins_dir = config.plugins_dir.clone();
        let database = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let service = Arc::new(ExternalPluginsService::new_empty(config, None, database));
        let lifecycle = service.lifecycle.lock().await;
        let shutdown_service = service.clone();
        let shutdown = tokio::spawn(async move { shutdown_service.shutdown_all().await });
        while !service.closing.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
        let platform = platform_target().expect("supported test platform");
        let plugin = RegistryPlugin {
            name: "safe-plugin".to_string(),
            title: "Safe plugin".to_string(),
            summary: "test".to_string(),
            description: "test".to_string(),
            author: "Temps Contributors".to_string(),
            category: "test".to_string(),
            keywords: vec!["test".to_string()],
            logo_url: None,
            repository: None,
            docs_url: None,
            version: "1.0.0".to_string(),
            platforms: BTreeMap::from([(
                platform.clone(),
                crate::catalog::PlatformRelease {
                    url: "https://registry.temps.sh/safe-plugin".to_string(),
                    sha256: "00".repeat(32),
                },
            )]),
        };
        let selected = SelectedPlugin {
            registry: VerifiedRegistry {
                envelope: crate::catalog::RegistryEnvelope {
                    key_id: "test-key".to_string(),
                    payload: String::new(),
                    signature: String::new(),
                },
                document: crate::catalog::RegistryDocument {
                    schema_version: 1,
                    revision: 1,
                    issued_at: chrono::Utc::now(),
                    expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
                    plugins: vec![plugin.clone()],
                },
            },
            identity: ReleaseIdentity {
                name: plugin.name.clone(),
                version: plugin.version.clone(),
                platform,
                sha256: "00".repeat(32),
                signer_key_id: "test-key".to_string(),
                registry_source: "https://registry.temps.sh/api/plugins".to_string(),
            },
            plugin,
        };
        let install_service = service.clone();
        let install = tokio::spawn(async move { install_service.install_selected(selected).await });

        // Act
        drop(lifecycle);
        shutdown.await.expect("shutdown task");
        let error = install
            .await
            .expect("install task")
            .expect_err("shutdown must reject queued installation");

        // Assert
        assert!(matches!(error, ExternalPluginsError::ShuttingDown));
        assert!(
            !plugins_dir.exists(),
            "a rejected queued install must not create plugin state"
        );
    }
}
