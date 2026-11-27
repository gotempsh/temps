// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! External plugin manager.
//!
//! A plugin runs as an ordinary Temps service. This module attaches to the
//! running container of that service, keeps the plugins it is attached to,
//! and lists the ones it is not. It starts no executable and no container.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use temps_plugin_protocol::PluginManifest;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use sea_orm::DatabaseConnection;
use utoipa::openapi::OpenApi;

use crate::channel::PluginChannel;
use crate::endpoint::PluginEndpoint;
use crate::proxy::PluginProxy;

/// A plugin Temps is attached to.
pub struct ExternalPluginProcess {
    /// The manifest the plugin answered with.
    pub manifest: PluginManifest,
    /// What identifies the build that is running: it changes with every
    /// deployment of the plugin's service.
    pub build: String,
    /// The address of the Temps service the plugin runs as.
    pub endpoint: PluginEndpoint,
    /// The secret this instance and the plugin both derive from the
    /// service's deployment token.
    auth_secret: String,
    /// The service the plugin runs as. The deployment owns its container:
    /// this host starts, stops and removes nothing.
    service: services::AttachedService,
    /// Whether the plugin has UI assets
    pub has_ui: bool,
    /// Bidirectional channel for platform queries and event delivery
    pub channel: Option<PluginChannel>,
    /// OpenAPI schema for the plugin's API endpoints (if it provided one)
    pub openapi_schema: Option<OpenApi>,
}

/// What `manifest` asks for that a plugin is never given, in words an
/// administrator reads.
pub(crate) fn privileged_access(manifest: &PluginManifest) -> Option<&'static str> {
    if manifest.requires_db {
        Some("the control-plane database")
    } else if manifest.requires_host_data_access {
        Some("the instance data directory")
    } else {
        None
    }
}

#[derive(Debug, Clone)]
pub struct PluginLoadFailure {
    pub plugin: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Default)]
pub struct PluginReloadResult {
    pub manifests: Vec<PluginManifest>,
    pub failures: Vec<PluginLoadFailure>,
}

/// Configuration for the external plugin manager.
#[derive(Debug, Clone)]
pub struct ExternalPluginConfig {
    /// Whether this process has durable local storage. Only the record of
    /// plugins an earlier version installed as executables needs it.
    pub persistent_installations: bool,
    /// Where earlier versions installed plugin executables, and where the
    /// signed registry's trust state is kept.
    pub plugins_dir: PathBuf,
    /// Base URL at which this instance's API answers. `None` when the caller
    /// did not supply one.
    pub host_api_url: Option<String>,
    /// Signed registry trust and network policy.
    pub registry: crate::catalog::RegistryConfig,
}

impl ExternalPluginConfig {
    /// Create a config with default settings for a given data directory.
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            persistent_installations: true,
            plugins_dir: data_dir.join("plugins"),
            host_api_url: None,
            registry: crate::catalog::RegistryConfig::default(),
        }
    }

    /// Inject an authenticated registry configuration. The default has no
    /// trust anchors and therefore accepts no registry document.
    pub fn with_registry(mut self, registry: crate::catalog::RegistryConfig) -> Self {
        self.registry = registry;
        self
    }

    /// Record where the proxy listens, so plugins can be told their own
    /// externally-reachable base URL.
    ///
    /// A bind address is not a URL: `0.0.0.0` means "every interface" to a
    /// listener but nothing to a client, so it is rewritten to loopback.
    /// Callers that terminate TLS or sit behind another proxy should set the
    /// instance's public URL in settings instead of relying on this.
    pub fn with_proxy_address(mut self, address: &str) -> Self {
        let address = address.trim();
        if address.is_empty() {
            return self;
        }
        let host_port = match address.rsplit_once(':') {
            Some((host, port)) if host == "0.0.0.0" || host == "[::]" || host.is_empty() => {
                format!("127.0.0.1:{port}")
            }
            _ => address.to_string(),
        };
        self.host_api_url = Some(format!("http://{host_port}"));
        self
    }
}

/// The plugins this instance is attached to.
///
/// This runs inside the main Temps process and handles:
/// - Attaching to the Temps service each plugin runs as
/// - Following its deployments, restarts and idle stops
/// - Providing proxy targets for the axum router
/// - Listing plugins an earlier version installed as executables
pub struct ExternalPluginManager {
    pub(crate) watch_ready: Arc<tokio::sync::Notify>,
    config: ExternalPluginConfig,
    /// Plugins this instance is attached to, keyed by plugin name
    plugins: Arc<RwLock<HashMap<String, ExternalPluginProcess>>>,
    /// Database connection for serving channel requests
    db: Arc<DatabaseConnection>,
    /// Late-bound bridge into the platform's own HTTP router, shared with
    /// every channel. Empty until the console finishes assembling the
    /// router — see [`crate::channel::HostApiSlot`].
    host_api: Arc<crate::channel::HostApiSlot>,
    /// Instance key material for minting per-caller actor tokens. Set by the
    /// console; `None` leaves plugins without one, so their API calls fail
    /// closed rather than being attributed to nobody.
    actor_crypto: crate::proxy::ActorCryptoSlot,
    ai_service: Arc<RwLock<Option<Arc<dyn temps_ai::AiService>>>>,
    audit_service: Arc<RwLock<Option<Arc<dyn temps_core::AuditLogger>>>>,
    /// Why each plugin Temps is not attached to failed its last attempt, in
    /// words an administrator can be shown. Cleared when it attaches.
    start_failures: Arc<RwLock<HashMap<String, String>>>,
    /// Plugins that run as Temps services: what attaching needs, and the
    /// ones this host is not attached to right now.
    services: services::ServicePlugins,
    /// Plugins installed as executables by an earlier version. Listed with
    /// the reason they do not run; never started.
    retired: Arc<RwLock<Vec<crate::retired::RetiredPlugin>>>,
}

impl ExternalPluginManager {
    pub fn new(config: ExternalPluginConfig, db: Arc<DatabaseConnection>) -> Self {
        Self {
            config,
            plugins: Arc::new(RwLock::new(HashMap::new())),
            db,
            host_api: Arc::new(crate::channel::HostApiSlot::new(None)),
            actor_crypto: crate::proxy::ActorCryptoSlot::default(),
            ai_service: Arc::new(RwLock::new(None)),
            watch_ready: Default::default(),
            audit_service: Arc::new(RwLock::new(None)),
            start_failures: Arc::new(RwLock::new(HashMap::new())),
            services: services::ServicePlugins::default(),
            retired: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Install the bridge plugins use to call the platform's own HTTP API.
    ///
    /// Called by the console once its router is assembled. Until then the
    /// slot is empty and an `ApiCall` is answered with an error naming the
    /// gap, rather than being silently dropped.
    pub async fn set_host_api(&self, bridge: Arc<dyn crate::channel::HostApiBridge>) {
        *self.host_api.write().await = Some(bridge);
    }

    /// Supply the key material used to mint per-caller actor tokens.
    pub async fn set_actor_crypto(&self, crypto: Arc<temps_core::CookieCrypto>) {
        // Every proxy already holds a clone of this slot, so mounted routes
        // start minting immediately — no rebuild, no ordering requirement.
        self.actor_crypto.set(crypto);
    }

    pub async fn set_ai_service(&self, service: Arc<dyn temps_ai::AiService>) {
        *self.ai_service.write().await = Some(service);
    }

    pub async fn ai_available(&self) -> bool {
        match self.ai_service.read().await.clone() {
            Some(service) => service.is_available().await,
            None => false,
        }
    }

    pub async fn set_audit_service(&self, service: Arc<dyn temps_core::AuditLogger>) {
        *self.audit_service.write().await = Some(service);
    }

    /// Attach to every plugin this instance has.
    ///
    /// Returns the manifests of the plugins it attached to.
    pub async fn discover_and_start(&self) -> Vec<PluginManifest> {
        self.discover_and_start_report().await.manifests
    }

    /// Attach to every plugin this instance has. A plugin runs as a Temps
    /// service; one installed as an executable is listed, never started.
    async fn discover_and_start_report(&self) -> PluginReloadResult {
        self.retire_installed_executables().await;
        // A plugin that runs as a service needs nothing on this host's disk,
        // so it also runs on a control plane without persistent storage.
        self.start_service_plugins().await
    }

    /// Record the plugins an earlier version installed as executables.
    ///
    /// A plugin runs only as a Temps service. An installed executable is
    /// never started: it is listed with the reason until an administrator
    /// removes it. Its installation and its data are left where they are.
    async fn retire_installed_executables(&self) {
        if !self.config.persistent_installations {
            self.retired.write().await.clear();
            return;
        }
        let retired = crate::retired::installed_executables(&self.config.plugins_dir).await;
        for plugin in &retired {
            warn!(
                plugin = %plugin.name,
                version = plugin.version.as_deref().unwrap_or("unknown"),
                "Not starting a plugin installed as an executable: plugins run only as Temps services"
            );
        }
        *self.retired.write().await = retired;
    }

    /// Plugins installed as executables, which are not started any more.
    pub async fn retired_plugins(&self) -> Vec<crate::retired::RetiredPlugin> {
        self.retired.read().await.clone()
    }

    /// Stop listing `plugin_name` as an installed executable.
    pub(crate) async fn forget_retired(&self, plugin_name: &str) {
        self.retired
            .write()
            .await
            .retain(|plugin| plugin.name != plugin_name);
    }

    /// Get the manifests of every plugin that can serve a request: the
    /// running ones, and the ones whose service is stopped or reconnecting
    /// and is attached to again by the next request.
    pub async fn manifests(&self) -> Vec<PluginManifest> {
        let mut manifests: Vec<PluginManifest> = self
            .plugins
            .read()
            .await
            .values()
            .map(|p| p.manifest.clone())
            .collect();
        manifests.extend(self.services.dormant_manifests().await);
        manifests
    }

    /// Get OpenAPI schemas from all running plugins.
    ///
    /// Returns a map of plugin name -> OpenAPI schema.
    pub async fn openapi_schemas(&self) -> Vec<(String, OpenApi)> {
        self.runtime_openapi_schemas()
            .await
            .into_iter()
            .map(|(name, _, schema)| (name, schema))
            .collect()
    }

    /// Read each schema and the identity of its build from the same attached plugin.
    pub(crate) async fn runtime_openapi_schemas(&self) -> Vec<(String, String, OpenApi)> {
        self.plugins
            .read()
            .await
            .iter()
            .filter_map(|(name, process)| {
                process
                    .openapi_schema
                    .as_ref()
                    .map(|schema| (name.clone(), process.build.clone(), schema.clone()))
            })
            .collect()
    }

    pub(crate) async fn declared_watch_sources(&self) -> Vec<crate::watches::WatchSource> {
        let plugins = self.plugins.read().await;
        let mut sources = Vec::new();
        for (name, process) in plugins.iter() {
            for event in process
                .manifest
                .published_events
                .iter()
                .filter(|event| event.is_valid())
            {
                sources.push(crate::watches::WatchSource {
                    plugin: name.clone(),
                    id: event.event_type.clone(),
                    label: event.title.clone(),
                    digest: process.build.clone(),
                    declaration: event.clone(),
                });
            }
        }
        sources
    }

    /// Create a PluginProxy for a given plugin name.
    pub async fn proxy_for(&self, plugin_name: &str) -> Option<PluginProxy> {
        self.service_proxy_for(plugin_name).await
    }

    /// Where a running plugin answers.
    pub async fn endpoint_for(&self, plugin_name: &str) -> Option<PluginEndpoint> {
        self.plugins
            .read()
            .await
            .get(plugin_name)
            .map(|p| p.endpoint.clone())
    }

    /// The assertion secret for internal event delivery.
    pub(crate) async fn auth_secret_for(&self, plugin_name: &str) -> Option<String> {
        self.plugins
            .read()
            .await
            .get(plugin_name)
            .map(|process| process.auth_secret.clone())
    }

    /// Send an event to a specific plugin over its channel.
    ///
    /// Returns `true` if the event was delivered, `false` if the plugin
    /// has no channel or the channel is dead.
    pub async fn send_event_via_channel(
        &self,
        plugin_name: &str,
        event: &temps_plugin_protocol::PluginEvent,
    ) -> bool {
        let plugins = self.plugins.read().await;
        if let Some(process) = plugins.get(plugin_name) {
            if let Some(ref channel) = process.channel {
                if channel.is_alive() {
                    return channel.send_event(event.clone()).is_ok();
                }
            }
        }
        false
    }

    pub async fn event_delivery_allowed(&self, plugin_name: &str) -> bool {
        let plugins = self.plugins.read().await;
        match plugins
            .get(plugin_name)
            .and_then(|process| process.channel.as_ref())
        {
            Some(channel) => channel.can_receive_events().await,
            None => false,
        }
    }

    /// Check if a plugin is running.
    pub async fn is_running(&self, plugin_name: &str) -> bool {
        self.plugins.read().await.contains_key(plugin_name)
    }

    /// Why Temps is not attached to a plugin, if its last attempt failed.
    pub async fn start_failure(&self, plugin_name: &str) -> Option<String> {
        self.start_failures.read().await.get(plugin_name).cloned()
    }

    /// Every plugin whose last attempt to attach failed, with the reason.
    pub async fn start_failures(&self) -> HashMap<String, String> {
        self.start_failures.read().await.clone()
    }

    /// Detach from every plugin. Their services keep running.
    pub async fn shutdown_all(&self) {
        self.services.forget_all().await;
        let mut plugins = self.plugins.write().await;
        for (name, _process) in plugins.drain() {
            info!(plugin = %name, "Detaching from external plugin");
        }
    }

    /// Detach from a specific plugin. Its service keeps running.
    pub async fn shutdown_plugin(&self, plugin_name: &str) {
        self.start_failures.write().await.remove(plugin_name);
        self.services.forget(plugin_name).await;
        if self.plugins.write().await.remove(plugin_name).is_some() {
            info!(plugin = %plugin_name, "Detached from external plugin");
        } else {
            debug!(plugin = %plugin_name, "Plugin was not attached");
        }
    }

    /// Detach from every plugin and attach again, to whatever each service
    /// runs now.
    pub async fn reload_all(&self) -> PluginReloadResult {
        info!("Reloading all external plugins");
        self.shutdown_all().await;
        self.discover_and_start_report().await
    }

    /// Attach again to a single plugin by name.
    ///
    /// Returns the new manifest on success, or the reason on failure.
    pub async fn reload_plugin(&self, plugin_name: &str) -> Result<PluginManifest, String> {
        let Some(binding) = self.service_binding_of(plugin_name).await else {
            return Err(format!(
                "Plugin '{plugin_name}' is not added from a service; cannot reload"
            ));
        };
        self.attach_service(binding, Some(plugin_name))
            .await
            .map_err(|error| error.public_reason())
    }

    /// Get the config.
    pub fn config(&self) -> &ExternalPluginConfig {
        &self.config
    }
}

pub(crate) mod services;
pub use services::ServicePluginState;

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn mock_db() -> Arc<DatabaseConnection> {
        Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection())
    }

    fn install_executable(plugins_dir: &std::path::Path, name: &str, version: &str) {
        let directory = plugins_dir.join(name);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("active.json"),
            format!(r#"{{"version":"{version}","directory":"{version}-0000"}}"#),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn test_manager_creation() {
        let root = tempfile::tempdir().unwrap();
        let manager =
            ExternalPluginManager::new(ExternalPluginConfig::new(root.path().into()), mock_db());

        assert!(manager.manifests().await.is_empty());
        assert!(manager.retired_plugins().await.is_empty());
        assert_eq!(manager.config().plugins_dir, root.path().join("plugins"));
    }

    #[tokio::test]
    async fn an_installed_executable_is_listed_and_never_started() {
        let root = tempfile::tempdir().unwrap();
        let config = ExternalPluginConfig::new(root.path().into());
        install_executable(&config.plugins_dir, "reports", "1.4.0");
        let manager = ExternalPluginManager::new(config, mock_db());

        let started = manager.discover_and_start().await;

        assert!(started.is_empty());
        assert!(!manager.is_running("reports").await);
        assert!(manager.proxy_for("reports").await.is_none());
        assert_eq!(
            manager.retired_plugins().await,
            vec![crate::retired::RetiredPlugin {
                name: "reports".into(),
                version: Some("1.4.0".into()),
            }]
        );
    }

    #[tokio::test]
    async fn reloading_reports_no_failure_for_an_installed_executable_and_forgets_a_removed_one() {
        let root = tempfile::tempdir().unwrap();
        let config = ExternalPluginConfig::new(root.path().into());
        install_executable(&config.plugins_dir, "reports", "1.4.0");
        let active = config.plugins_dir.join("reports").join("active.json");
        let manager = ExternalPluginManager::new(config, mock_db());

        // Not running is what an installed executable is now, not a failure
        // an administrator could fix by reloading.
        let first = manager.reload_all().await;
        assert!(first.manifests.is_empty());
        assert!(first.failures.is_empty());
        assert_eq!(manager.retired_plugins().await.len(), 1);

        std::fs::remove_file(active).unwrap();
        manager.reload_all().await;
        assert!(manager.retired_plugins().await.is_empty());
    }

    #[tokio::test]
    async fn a_process_without_durable_storage_lists_no_installed_executable() {
        let root = tempfile::tempdir().unwrap();
        let mut config = ExternalPluginConfig::new(root.path().into());
        config.persistent_installations = false;
        install_executable(&config.plugins_dir, "reports", "1.4.0");
        let manager = ExternalPluginManager::new(config, mock_db());

        manager.discover_and_start().await;

        assert!(manager.retired_plugins().await.is_empty());
    }

    #[tokio::test]
    async fn reloading_a_plugin_that_was_not_added_from_a_service_says_so() {
        let root = tempfile::tempdir().unwrap();
        let manager =
            ExternalPluginManager::new(ExternalPluginConfig::new(root.path().into()), mock_db());

        let reason = manager.reload_plugin("reports").await.unwrap_err();

        assert_eq!(
            reason,
            "Plugin 'reports' is not added from a service; cannot reload"
        );
    }
}
