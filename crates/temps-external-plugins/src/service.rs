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

use crate::catalog::{CatalogError, RegistryClient, RegistryPlugin, VerifiedRegistry};
use crate::event_listener::PluginEventListener;
use crate::install::{
    normalize_digest, platform_target, validate_plugin_name, validate_release_for_install,
    validate_version, InstallError, PluginInstaller,
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
    /// Serializes every read/compare/write mutation of the persisted registry
    /// trust state. Catalogue requests can overlap on the network, but they
    /// must be committed in a single monotonic order with installations.
    registry_state: tokio::sync::Mutex<()>,
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
    #[error("Plugin '{name}' has no active installation to uninstall")]
    NotInstalled { name: String },
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
            registry_state: tokio::sync::Mutex::new(()),
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
            registry_state: tokio::sync::Mutex::new(()),
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
        let registry_config = self.manager.config().registry.clone();
        let client = RegistryClient::new(registry_config.clone())?;
        let keyset = client.fetch_keyset().await?;
        self.accept_catalog_keyset(registry_config.clone(), &keyset)
            .await?;
        let registry = client.fetch_with_keyset(keyset).await?;
        // Catalogue access is the explicit network refresh boundary. Record
        // both the keyset generation and catalogue revision on first view and
        // every later view so discovery and reload remain fully local without
        // sacrificing rollback protection.
        self.accept_catalog_state(registry_config, &registry)
            .await?;
        Ok(registry)
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
        self.select_from_registry(name, registry)
    }

    fn select_from_registry(
        &self,
        name: &str,
        registry: VerifiedRegistry,
    ) -> Result<SelectedPlugin, ExternalPluginsError> {
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
        validate_release_for_install(&plugin, release, &self.manager.config().registry)?;
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
        {
            let _registry_state = self.registry_state.lock().await;
            installer
                .accept_registry_revision(&self.manager.config().plugins_dir, &selected.registry)
                .await?;
        }
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

    /// Deactivate only the named plugin, preserving all releases and data.
    pub async fn uninstall_plugin(&self, name: &str) -> Result<(), ExternalPluginsError> {
        validate_plugin_name(name)?;
        let _lifecycle = self.lifecycle.lock().await;
        if self.closing.load(Ordering::Acquire) {
            return Err(ExternalPluginsError::ShuttingDown);
        }
        if !PluginInstaller::deactivate(&self.manager.config().plugins_dir, name).await? {
            return Err(ExternalPluginsError::NotInstalled {
                name: name.to_string(),
            });
        }
        self.manager.shutdown_plugin(name).await;
        self.refresh_runtime_surfaces().await;
        Ok(())
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

    async fn accept_catalog_state(
        &self,
        registry_config: crate::catalog::RegistryConfig,
        registry: &VerifiedRegistry,
    ) -> Result<(), ExternalPluginsError> {
        // Match installation's lock order. This prevents a newly learned
        // revocation from committing halfway through launch of a binary that
        // was selected under the preceding keyset.
        let _lifecycle = self.lifecycle.lock().await;
        if self.closing.load(Ordering::Acquire) {
            return Err(ExternalPluginsError::ShuttingDown);
        }
        self.accept_registry_state(registry_config, registry).await
    }

    async fn accept_catalog_keyset(
        &self,
        registry_config: crate::catalog::RegistryConfig,
        keyset: &crate::trust::VerifiedKeyset,
    ) -> Result<(), ExternalPluginsError> {
        let _lifecycle = self.lifecycle.lock().await;
        if self.closing.load(Ordering::Acquire) {
            return Err(ExternalPluginsError::ShuttingDown);
        }
        let _registry_state = self.registry_state.lock().await;
        PluginInstaller::new(registry_config)?
            .refresh_keyset(&self.manager.config().plugins_dir, keyset)
            .await?;
        Ok(())
    }

    async fn accept_registry_state(
        &self,
        registry_config: crate::catalog::RegistryConfig,
        registry: &VerifiedRegistry,
    ) -> Result<(), ExternalPluginsError> {
        let _registry_state = self.registry_state.lock().await;
        PluginInstaller::new(registry_config)?
            .accept_registry_revision(&self.manager.config().plugins_dir, registry)
            .await?;
        Ok(())
    }

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
    use base64::Engine as _;
    use ed25519_dalek::{Signer as _, SigningKey};
    use sha2::{Digest as _, Sha256};
    use std::collections::BTreeMap;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    async fn serve_artifact_once(body: Vec<u8>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind artifact fixture");
        let address = listener.local_addr().expect("artifact fixture address");
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept artifact request");
            let mut request = [0u8; 2048];
            let _ = stream
                .read(&mut request)
                .await
                .expect("read artifact request");
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(headers.as_bytes())
                .await
                .expect("write artifact headers");
            stream.write_all(&body).await.expect("write artifact body");
        });
        format!("http://{address}/plugin")
    }

    async fn serve_json_once(body: Vec<u8>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind catalogue fixture");
        let address = listener.local_addr().expect("catalogue fixture address");
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept catalogue request");
            let mut request = [0u8; 2048];
            let _ = stream
                .read(&mut request)
                .await
                .expect("read catalogue request");
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(headers.as_bytes())
                .await
                .expect("write catalogue headers");
            stream.write_all(&body).await.expect("write catalogue body");
        });
        format!("http://{address}/plugins")
    }

    #[cfg(unix)]
    fn protocol_v2_fixture(name: &str, version: &str) -> Vec<u8> {
        let manifest = PluginManifest::builder(name, version).build();
        let hello = serde_json::to_string(&temps_core::external_plugin::HandshakeMessage::Hello(
            temps_core::external_plugin::PluginHello {
                protocol_version: temps_core::external_plugin::EXTERNAL_PLUGIN_PROTOCOL_VERSION,
                manifest: Box::new(manifest),
            },
        ))
        .expect("serialize fixture hello");
        let ready = serde_json::to_string(&temps_core::external_plugin::HandshakeMessage::Ready(
            temps_core::external_plugin::PluginReady {
                ready: true,
                has_ui: false,
                protocol_version: temps_core::external_plugin::EXTERNAL_PLUGIN_PROTOCOL_VERSION,
                openapi: None,
            },
        ))
        .expect("serialize fixture ready");
        let hello_literal = serde_json::to_string(&hello).expect("quote fixture hello");
        let ready_literal = serde_json::to_string(&ready).expect("quote fixture ready");
        format!(
            r#"#!/usr/bin/python3
import base64
import hashlib
import json
import socket
import sys

HELLO = {hello_literal}
READY = {ready_literal}
args = dict(zip(sys.argv[1::2], sys.argv[2::2]))
print(HELLO, flush=True)
launch = json.loads(sys.stdin.readline())
server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
server.bind(args["--socket-path"])
server.listen(1)
print(READY, flush=True)
connection, _ = server.accept()
request = b""
while b"\r\n\r\n" not in request:
    request += connection.recv(4096)
lines = request.decode("latin1").split("\r\n")
headers = {{}}
for line in lines[1:]:
    if ":" in line:
        key, value = line.split(":", 1)
        headers[key.lower()] = value.strip()
if headers.get("x-temps-auth-signature") != launch["auth_secret"]:
    sys.exit(7)
websocket_key = headers["sec-websocket-key"]
accept = base64.b64encode(hashlib.sha1((websocket_key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode()
connection.sendall(("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: " + accept + "\r\n\r\n").encode())
try:
    while connection.recv(4096):
        pass
except Exception:
    pass
"#
        )
        .into_bytes()
    }

    fn selected_plugin(
        url: String,
        bytes: &[u8],
        name: &str,
        version: &str,
        revision: u64,
        signing: &SigningKey,
    ) -> SelectedPlugin {
        let platform = platform_target().expect("supported test platform");
        let sha256 = hex::encode(Sha256::digest(bytes));
        let plugin = RegistryPlugin {
            name: name.to_string(),
            title: "Fixture plugin".to_string(),
            summary: "fixture".to_string(),
            description: "fixture".to_string(),
            author: "Temps Contributors".to_string(),
            category: "test".to_string(),
            keywords: vec!["test".to_string()],
            logo_url: None,
            repository: None,
            docs_url: None,
            version: version.to_string(),
            platforms: BTreeMap::from([(
                platform.clone(),
                crate::catalog::PlatformRelease {
                    url,
                    sha256: sha256.clone(),
                    npm: None,
                },
            )]),
        };
        let document = crate::catalog::RegistryDocument {
            schema_version: 1,
            revision,
            issued_at: chrono::Utc::now() - chrono::Duration::minutes(1),
            expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            plugins: vec![plugin.clone()],
        };
        let payload = serde_json::to_vec(&document).expect("serialize fixture registry");
        let envelope = crate::catalog::RegistryEnvelope {
            key_id: "fixture-key".to_string(),
            payload: base64::engine::general_purpose::STANDARD.encode(&payload),
            signature: base64::engine::general_purpose::STANDARD.encode(
                signing
                    .sign(&crate::trust::signature_message(
                        crate::trust::CATALOG_SIGNATURE_DOMAIN,
                        &payload,
                    ))
                    .to_bytes(),
            ),
        };
        let keyset = crate::trust::VerifiedKeyset::test_fixture(
            "fixture-key",
            signing.verifying_key().to_bytes(),
        )
        .1;
        SelectedPlugin {
            registry: VerifiedRegistry {
                keyset,
                envelope,
                document,
            },
            plugin,
            identity: ReleaseIdentity {
                name: name.to_string(),
                version: version.to_string(),
                platform,
                sha256,
                signer_key_id: "fixture-key".to_string(),
                registry_source: "http://127.0.0.1/fixture".to_string(),
            },
        }
    }

    fn registry_revision(
        revision: u64,
        keyset_generation: u64,
        signing: &SigningKey,
    ) -> VerifiedRegistry {
        let mut selected = selected_plugin(
            "http://127.0.0.1/plugin".to_string(),
            b"fixture",
            "fixture-plugin",
            "1.0.0",
            revision,
            signing,
        );
        selected.registry.keyset = crate::trust::VerifiedKeyset::test_fixture_with_keys(
            vec![(
                "fixture-key".to_string(),
                signing.verifying_key().to_bytes(),
                crate::trust::CatalogKeyStatus::Active,
            )],
            keyset_generation,
        )
        .1;
        selected.registry
    }
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

    #[test]
    fn production_registry_has_embedded_offline_root_threshold() {
        let service = service();
        let registry = &service.manager().config().registry;
        assert_eq!(registry.root_trust.threshold, 2);
        assert_eq!(registry.root_trust.keys.len(), 3);
    }

    #[test]
    fn selection_accepts_canonical_npm_release() {
        let signing = SigningKey::from_bytes(&[71; 32]);
        let mut registry = registry_revision(1, 1, &signing);
        let release = registry.document.plugins[0]
            .platforms
            .values_mut()
            .next()
            .expect("platform release");
        release.url = "https://registry.npmjs.org/@temps/fixture-plugin/-/fixture-plugin-1.0.0.tgz"
            .to_string();
        release.npm = Some(crate::catalog::NpmRelease {
            name: "@temps/fixture-plugin".to_string(),
            version: "1.0.0".to_string(),
            integrity: format!(
                "sha512-{}",
                base64::engine::general_purpose::STANDARD.encode([0u8; 64])
            ),
            binary_path: "package/plugin".to_string(),
        });
        let selected = service()
            .select_from_registry("fixture-plugin", registry)
            .expect("canonical npm release selected");
        assert_eq!(selected.identity.version, "1.0.0");
    }

    #[test]
    fn selection_rejects_unsafe_npm_url_and_direct_only_release() {
        let signing = SigningKey::from_bytes(&[72; 32]);
        let mut registry = registry_revision(1, 1, &signing);
        assert!(matches!(
            service().select_from_registry("fixture-plugin", registry.clone()),
            Err(ExternalPluginsError::Install(
                InstallError::InvalidNpmRelease { .. }
            ))
        ));
        let release = registry.document.plugins[0]
            .platforms
            .values_mut()
            .next()
            .expect("platform release");
        release.url =
            "https://registry.npmjs.org/@temps/fixture-plugin/-/fixture-plugin-1.0.0.tgz?token=x"
                .to_string();
        release.npm = Some(crate::catalog::NpmRelease {
            name: "@temps/fixture-plugin".to_string(),
            version: "1.0.0".to_string(),
            integrity: format!(
                "sha512-{}",
                base64::engine::general_purpose::STANDARD.encode([0u8; 64])
            ),
            binary_path: "package/plugin".to_string(),
        });
        assert!(matches!(
            service().select_from_registry("fixture-plugin", registry),
            Err(ExternalPluginsError::Install(
                InstallError::UnsafeArtifactUrl { .. }
            ))
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
    async fn catalog_view_persists_first_trust_state_and_rejects_rollbacks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let signing = SigningKey::from_bytes(&[63; 32]);
        let registry = registry_revision(7, 2, &signing);
        let url = serve_json_once(
            serde_json::to_vec(&registry.envelope).expect("serialize catalogue envelope"),
        )
        .await;
        let registry_config = crate::catalog::RegistryConfig::local_with_generation(
            url,
            "fixture-key",
            signing.verifying_key().to_bytes(),
            2,
        );
        let config = ExternalPluginConfig::new(
            temp.path().to_path_buf(),
            "postgres://localhost/test".to_string(),
        )
        .with_registry(registry_config.clone());
        let service = ExternalPluginsService::new_empty(
            config.clone(),
            None,
            Arc::new(
                sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
            ),
        );

        service
            .catalog()
            .await
            .expect("first catalogue view must persist authenticated trust state");
        assert!(config.plugins_dir.join("registry-state.json").is_file());

        let revision_error = service
            .accept_registry_state(registry_config.clone(), &registry_revision(6, 2, &signing))
            .await
            .expect_err("an older catalogue revision must be rejected after viewing");
        assert!(matches!(
            revision_error,
            ExternalPluginsError::Install(InstallError::RegistryRollback {
                received: 6,
                highest: 7
            })
        ));

        let keyset_error = service
            .accept_registry_state(registry_config, &registry_revision(8, 1, &signing))
            .await
            .expect_err("an older keyset generation must be rejected after viewing");
        assert!(matches!(
            keyset_error,
            ExternalPluginsError::Install(InstallError::KeysetRollback {
                received: 1,
                highest: 2
            })
        ));
    }

    #[tokio::test]
    async fn failed_catalog_still_persists_fresh_revocation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let signing = SigningKey::from_bytes(&[66; 32]);
        let url = "http://127.0.0.1:1/invalid-catalogue".to_string();
        let mut registry_config = crate::catalog::RegistryConfig::local(
            url,
            "fixture-key",
            signing.verifying_key().to_bytes(),
        );
        let replacement = SigningKey::from_bytes(&[67; 32]);
        let (_, revoked) = crate::trust::VerifiedKeyset::test_fixture_with_keys(
            vec![
                (
                    "fixture-key".to_string(),
                    signing.verifying_key().to_bytes(),
                    crate::trust::CatalogKeyStatus::Revoked,
                ),
                (
                    "replacement-key".to_string(),
                    replacement.verifying_key().to_bytes(),
                    crate::trust::CatalogKeyStatus::Active,
                ),
            ],
            2,
        );
        let config = ExternalPluginConfig::new(
            temp.path().to_path_buf(),
            "postgres://localhost/test".to_string(),
        )
        .with_registry(registry_config.clone());
        let service = ExternalPluginsService::new_empty(
            config.clone(),
            None,
            Arc::new(
                sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
            ),
        );
        let binary = b"fixture binary";
        let artifact_url = serve_artifact_once(binary.to_vec()).await;
        let old =
            selected_plugin(artifact_url, binary, "fixture-plugin", "1.0.0", 7, &signing).registry;
        service
            .accept_registry_state(registry_config.clone(), &old)
            .await
            .expect("seed old catalogue");
        let installer = PluginInstaller::new(registry_config.clone()).expect("installer");
        let candidate = installer
            .prepare(&config.plugins_dir, &old, &old.document.plugins[0])
            .await
            .expect("prepare active fixture");
        installer
            .activate(&candidate)
            .await
            .expect("activate fixture");
        registry_config = registry_config.with_test_keyset(revoked.clone());
        let refreshed = ExternalPluginsService::new_empty(
            config.clone().with_registry(registry_config.clone()),
            None,
            Arc::new(
                sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
            ),
        );
        assert!(matches!(
            refreshed.catalog().await,
            Err(ExternalPluginsError::Catalog(_))
        ));
        let state: serde_json::Value = serde_json::from_slice(
            &std::fs::read(config.plugins_dir.join("registry-state.json")).expect("state"),
        )
        .expect("valid state");
        assert_eq!(state["highest_revision"], 7);
        assert_eq!(state["highest_keyset_generation"], 2);
        assert!(matches!(
            &crate::install::discover_active(&config.plugins_dir, &registry_config).await[..],
            [Err(InstallError::InvalidReceipt { reason, .. })]
                if reason.contains("key status is Revoked")
        ));
        assert!(matches!(
            crate::catalog::verify_envelope(
                old.envelope,
                revoked,
                &registry_config.url,
                crate::trust::KeysetUse::FreshCatalog
            ),
            Err(crate::catalog::CatalogError::Trust(_))
        ));
    }

    #[tokio::test]
    async fn unauthenticated_keyset_cannot_replace_persisted_trust() {
        let temp = tempfile::tempdir().expect("tempdir");
        let signing = SigningKey::from_bytes(&[68; 32]);
        let registry_config = crate::catalog::RegistryConfig::local(
            "http://127.0.0.1:1/catalog".to_string(),
            "fixture-key",
            signing.verifying_key().to_bytes(),
        );
        let config = ExternalPluginConfig::new(
            temp.path().to_path_buf(),
            "postgres://localhost/test".to_string(),
        )
        .with_registry(registry_config.clone());
        let service = ExternalPluginsService::new_empty(
            config.clone(),
            None,
            Arc::new(
                sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
            ),
        );
        service
            .accept_registry_state(registry_config.clone(), &registry_revision(7, 1, &signing))
            .await
            .expect("seed state");
        let state_path = config.plugins_dir.join("registry-state.json");
        let before = std::fs::read(&state_path).expect("read state");
        let mut invalid = crate::trust::VerifiedKeyset::test_fixture_with_keys(
            vec![(
                "fixture-key".to_string(),
                signing.verifying_key().to_bytes(),
                crate::trust::CatalogKeyStatus::Active,
            )],
            2,
        )
        .1
        .envelope;
        invalid.payload.push('x');
        assert!(crate::trust::VerifiedKeyset::verify(
            invalid,
            &registry_config.root_trust,
            chrono::Utc::now(),
            crate::trust::KeysetUse::FreshCatalog,
        )
        .is_err());
        assert_eq!(
            std::fs::read(state_path).expect("read unchanged state"),
            before
        );
    }

    #[tokio::test]
    async fn failed_first_catalogue_keeps_keyset_generation_floor() {
        let temp = tempfile::tempdir().expect("tempdir");
        let signing = SigningKey::from_bytes(&[69; 32]);
        let config = crate::catalog::RegistryConfig::local_with_generation(
            "http://127.0.0.1:1/unavailable".to_string(),
            "fixture-key",
            signing.verifying_key().to_bytes(),
            2,
        );
        let service = ExternalPluginsService::new_empty(
            ExternalPluginConfig::new(
                temp.path().to_path_buf(),
                "postgres://localhost/test".to_string(),
            )
            .with_registry(config.clone()),
            None,
            Arc::new(
                sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
            ),
        );
        assert!(matches!(
            service.catalog().await,
            Err(ExternalPluginsError::Catalog(_))
        ));
        let stale = crate::trust::VerifiedKeyset::test_fixture(
            "fixture-key",
            signing.verifying_key().to_bytes(),
        )
        .1;
        assert!(matches!(
            service.accept_catalog_keyset(config, &stale).await,
            Err(ExternalPluginsError::Install(
                InstallError::KeysetRollback {
                    received: 1,
                    highest: 2
                }
            ))
        ));
    }

    #[tokio::test]
    async fn concurrent_out_of_order_registry_updates_cannot_roll_back_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let signing = SigningKey::from_bytes(&[64; 32]);
        let registry_config = crate::catalog::RegistryConfig::local(
            "http://127.0.0.1/plugins".to_string(),
            "fixture-key",
            signing.verifying_key().to_bytes(),
        );
        let config = ExternalPluginConfig::new(
            temp.path().to_path_buf(),
            "postgres://localhost/test".to_string(),
        )
        .with_registry(registry_config.clone());
        let service = Arc::new(ExternalPluginsService::new_empty(
            config,
            None,
            Arc::new(
                sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
            ),
        ));
        service
            .accept_registry_state(registry_config.clone(), &registry_revision(1, 1, &signing))
            .await
            .expect("seed registry state");

        // Queue generation 3 first and generation 2 second while holding the
        // state lock. Tokio's mutex is FIFO, so this deterministically models
        // the dangerous response order where the stale response finishes last.
        let gate = service.registry_state.lock().await;
        let newest_service = service.clone();
        let newest_config = registry_config.clone();
        let newest_registry = registry_revision(3, 3, &signing);
        let newest = tokio::spawn(async move {
            newest_service
                .accept_registry_state(newest_config, &newest_registry)
                .await
        });
        tokio::task::yield_now().await;
        let stale_service = service.clone();
        let stale_registry = registry_revision(2, 2, &signing);
        let stale = tokio::spawn(async move {
            stale_service
                .accept_registry_state(registry_config, &stale_registry)
                .await
        });
        tokio::task::yield_now().await;
        drop(gate);

        newest
            .await
            .expect("newest update task")
            .expect("newest update must be accepted");
        let error = stale
            .await
            .expect("stale update task")
            .expect_err("stale response must not overwrite the newer state");
        assert!(matches!(
            error,
            ExternalPluginsError::Install(InstallError::KeysetRollback {
                received: 2,
                highest: 3
            })
        ));
    }

    #[tokio::test]
    async fn catalog_trust_commit_waits_for_in_progress_install_lifecycle() {
        let temp = tempfile::tempdir().expect("tempdir");
        let signing = SigningKey::from_bytes(&[65; 32]);
        let registry_config = crate::catalog::RegistryConfig::local(
            "http://127.0.0.1/plugins".to_string(),
            "fixture-key",
            signing.verifying_key().to_bytes(),
        );
        let config = ExternalPluginConfig::new(
            temp.path().to_path_buf(),
            "postgres://localhost/test".to_string(),
        )
        .with_registry(registry_config.clone());
        let service = Arc::new(ExternalPluginsService::new_empty(
            config,
            None,
            Arc::new(
                sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
            ),
        ));
        service
            .accept_registry_state(registry_config.clone(), &registry_revision(1, 1, &signing))
            .await
            .expect("seed registry state");

        // install_selected owns this guard from trust acceptance through
        // candidate activation and promotion. A catalogue response learned in
        // parallel must not commit until that complete lifecycle ends.
        let install_lifecycle = service.lifecycle.lock().await;
        let catalogue_service = service.clone();
        let newer_keyset = registry_revision(2, 2, &signing).keyset;
        let mut catalogue_commit = tokio::spawn(async move {
            catalogue_service
                .accept_catalog_keyset(registry_config, &newer_keyset)
                .await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut catalogue_commit)
                .await
                .is_err(),
            "catalogue trust update must wait for the active install lifecycle"
        );

        drop(install_lifecycle);
        catalogue_commit
            .await
            .expect("catalogue update task")
            .expect("catalogue update must commit after installation finishes");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn protocol_v2_install_activates_and_promotes_verified_binary() {
        if !std::path::Path::new("/usr/bin/python3").exists() {
            eprintln!("skipping protocol fixture: /usr/bin/python3 is unavailable");
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let signing = SigningKey::from_bytes(&[61; 32]);
        let binary = protocol_v2_fixture("fixture-plugin", "1.0.0");
        let url = serve_artifact_once(binary.clone()).await;
        let registry = crate::catalog::RegistryConfig::local(
            url.clone(),
            "fixture-key",
            signing.verifying_key().to_bytes(),
        );
        let mut config = ExternalPluginConfig::new(
            temp.path().to_path_buf(),
            "postgres://localhost/test".to_string(),
        )
        .with_registry(registry);
        config.sockets_dir = temp.path().join("sockets");
        std::fs::create_dir_all(&config.sockets_dir).expect("fixture socket directory");
        let service = ExternalPluginsService::new_empty(
            config.clone(),
            None,
            Arc::new(
                sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
            ),
        );

        let outcome = service
            .install_selected(selected_plugin(
                url,
                &binary,
                "fixture-plugin",
                "1.0.0",
                1,
                &signing,
            ))
            .await
            .expect("protocol-v2 fixture must install");

        assert_eq!(outcome.name, "fixture-plugin");
        assert_eq!(service.manifests().await[0].version, "1.0.0");
        assert!(config
            .plugins_dir
            .join("fixture-plugin/active.json")
            .is_file());
        let plugin_root = config.plugins_dir.join("fixture-plugin");
        let release_count_before = std::fs::read_dir(&plugin_root)
            .expect("plugin root")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .count();
        service
            .uninstall_plugin("fixture-plugin")
            .await
            .expect("uninstall");
        assert!(!plugin_root.join("active.json").exists());
        assert!(service.manifests().await.is_empty());
        assert!(!service.manager().is_running("fixture-plugin").await);
        assert_eq!(
            std::fs::read_dir(&plugin_root)
                .expect("preserved plugin root")
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                .count(),
            release_count_before,
            "all release and plugin data directories remain intact"
        );
        let reload = service.reload_plugins().await.expect("reload");
        assert!(
            reload.manifests.is_empty(),
            "uninstalled plugin must not resurrect"
        );
        assert!(
            reload.failures.is_empty(),
            "retained releases are not broken active installs"
        );
        service.shutdown_all().await;
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn rejected_upgrade_preserves_active_record_and_running_process() {
        if !std::path::Path::new("/usr/bin/python3").exists() {
            eprintln!("skipping protocol fixture: /usr/bin/python3 is unavailable");
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let signing = SigningKey::from_bytes(&[62; 32]);
        let first_binary = protocol_v2_fixture("fixture-plugin", "1.0.0");
        let first_url = serve_artifact_once(first_binary.clone()).await;
        let registry = crate::catalog::RegistryConfig::local(
            first_url.clone(),
            "fixture-key",
            signing.verifying_key().to_bytes(),
        );
        let mut config = ExternalPluginConfig::new(
            temp.path().to_path_buf(),
            "postgres://localhost/test".to_string(),
        )
        .with_registry(registry);
        config.sockets_dir = temp.path().join("sockets");
        std::fs::create_dir_all(&config.sockets_dir).expect("fixture socket directory");
        let service = ExternalPluginsService::new_empty(
            config.clone(),
            None,
            Arc::new(
                sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
            ),
        );
        service
            .install_selected(selected_plugin(
                first_url,
                &first_binary,
                "fixture-plugin",
                "1.0.0",
                1,
                &signing,
            ))
            .await
            .expect("initial plugin must install");
        let active_path = config.plugins_dir.join("fixture-plugin/active.json");
        let active_before = std::fs::read(&active_path).expect("initial active record");

        let rejected_binary = protocol_v2_fixture("different-plugin", "2.0.0");
        let rejected_url = serve_artifact_once(rejected_binary.clone()).await;
        let error = service
            .install_selected(selected_plugin(
                rejected_url,
                &rejected_binary,
                "fixture-plugin",
                "2.0.0",
                2,
                &signing,
            ))
            .await
            .expect_err("signed and declared identities must match");

        assert!(matches!(
            error,
            ExternalPluginsError::CandidateRejected { .. }
        ));
        assert_eq!(
            std::fs::read(&active_path).expect("preserved active record"),
            active_before
        );
        assert_eq!(service.manifests().await[0].version, "1.0.0");
        let install_directories = std::fs::read_dir(config.plugins_dir.join("fixture-plugin"))
            .expect("plugin root")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .count();
        assert_eq!(install_directories, 1, "rejected candidate must be removed");
        service.shutdown_all().await;
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
                    npm: None,
                },
            )]),
        };
        let selected = SelectedPlugin {
            registry: VerifiedRegistry {
                keyset: crate::trust::VerifiedKeyset::test_fixture(
                    "test-key",
                    SigningKey::from_bytes(&[42; 32]).verifying_key().to_bytes(),
                )
                .1,
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
