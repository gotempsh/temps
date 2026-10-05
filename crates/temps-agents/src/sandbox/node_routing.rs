// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Node-routing sandbox provider (ADR-048).
//!
//! Wraps the host's own provider (Docker / Firecracker / the ADR-029
//! backend router) and adds worker nodes. Consumers keep holding exactly one
//! `Arc<dyn SandboxProvider>` (ADR-010); this impl dispatches per call:
//!
//! * `create` / `create_from_snapshot` by `SandboxCreateConfig::node_id`;
//! * every handle-based method by the `SandboxHandle::node_id` stamped when
//!   the sandbox was created or recovered;
//! * host-level methods (image status, rootfs, `recover(run_id)`) always go
//!   to the local provider — agent runs and the image cache are local.
//!
//! Every trait method is overridden, including the ones with default
//! bodies: a default body running on the router would bypass the owning
//! provider's own override (e.g. Docker's `exec_as_root`).

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, EntityTrait};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use super::remote::{RemoteSandboxDefaults, RemoteSandboxProvider};
use super::{
    KillSignal, OnStreamEventCallback, PtyAttachment, RootfsGcReport, RootfsReport,
    RuntimeCompatibility, SandboxBackend, SandboxCreateConfig, SandboxExecResult, SandboxHandle,
    SandboxProvider, SnapshotArtifact,
};
use crate::ai_cli::OnEventCallback;
use crate::error::AgentError;

/// Resolves a worker node id to a provider that drives sandboxes on it.
#[async_trait]
pub trait RemoteNodeResolver: Send + Sync {
    /// Provider for sandboxes on `node_id`. Fails with
    /// `AgentError::SandboxNodeUnavailable` when the node does not exist or
    /// cannot currently be reached (offline, pending enrollment).
    async fn provider_for(&self, node_id: i32) -> Result<Arc<dyn SandboxProvider>, AgentError>;
}

pub struct NodeRoutingSandboxProvider {
    local: Arc<dyn SandboxProvider>,
    resolver: Arc<dyn RemoteNodeResolver>,
}

impl NodeRoutingSandboxProvider {
    pub fn new(local: Arc<dyn SandboxProvider>, resolver: Arc<dyn RemoteNodeResolver>) -> Self {
        Self { local, resolver }
    }

    /// The host's own provider, for callers that need host-level behaviour.
    pub fn local(&self) -> &Arc<dyn SandboxProvider> {
        &self.local
    }

    async fn for_node(&self, node_id: Option<i32>) -> Result<Arc<dyn SandboxProvider>, AgentError> {
        match node_id {
            None => Ok(self.local.clone()),
            Some(id) => self.resolver.provider_for(id).await,
        }
    }

    async fn owner_of(
        &self,
        handle: &SandboxHandle,
    ) -> Result<Arc<dyn SandboxProvider>, AgentError> {
        self.for_node(handle.node_id).await
    }

    fn stamp(handle: SandboxHandle, node_id: Option<i32>) -> SandboxHandle {
        SandboxHandle { node_id, ..handle }
    }
}

#[async_trait]
impl SandboxProvider for NodeRoutingSandboxProvider {
    async fn create(&self, config: SandboxCreateConfig) -> Result<SandboxHandle, AgentError> {
        let node_id = config.node_id;
        let handle = self.for_node(node_id).await?.create(config).await?;
        Ok(Self::stamp(handle, node_id))
    }

    async fn image_identity(&self, handle: &SandboxHandle) -> Result<String, AgentError> {
        self.owner_of(handle).await?.image_identity(handle).await
    }

    async fn check_agent_runtime(
        &self,
        handle: &SandboxHandle,
    ) -> Result<RuntimeCompatibility, AgentError> {
        self.owner_of(handle)
            .await?
            .check_agent_runtime(handle)
            .await
    }

    async fn recover_agent_harness(
        &self,
        handle: &SandboxHandle,
        epoch: u64,
    ) -> Result<(), AgentError> {
        self.owner_of(handle)
            .await?
            .recover_agent_harness(handle, epoch)
            .await
    }

    async fn model_relay_base_url(
        &self,
        handle: &SandboxHandle,
        control_plane_url: &str,
    ) -> Result<String, AgentError> {
        self.owner_of(handle)
            .await?
            .model_relay_base_url(handle, control_plane_url)
            .await
    }

    async fn git_relay_base_url(
        &self,
        handle: &SandboxHandle,
        control_plane_url: &str,
    ) -> Result<String, AgentError> {
        self.owner_of(handle)
            .await?
            .git_relay_base_url(handle, control_plane_url)
            .await
    }

    async fn harness_mcp_url(
        &self,
        handle: &SandboxHandle,
        control_plane_url: &str,
        registered_url: &str,
    ) -> Result<String, AgentError> {
        self.owner_of(handle)
            .await?
            .harness_mcp_url(handle, control_plane_url, registered_url)
            .await
    }

    async fn configure_application_network(
        &self,
        handle: &SandboxHandle,
        network_name: &str,
        service_containers: &[String],
    ) -> Result<(), AgentError> {
        self.owner_of(handle)
            .await?
            .configure_application_network(handle, network_name, service_containers)
            .await
    }

    async fn connect_agent_runtime(
        &self,
        handle: &SandboxHandle,
    ) -> Result<PtyAttachment, AgentError> {
        self.owner_of(handle)
            .await?
            .connect_agent_runtime(handle)
            .await
    }

    async fn exec(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        self.owner_of(handle)
            .await?
            .exec(handle, cmd, env, on_output)
            .await
    }

    async fn exec_as_root(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        self.owner_of(handle)
            .await?
            .exec_as_root(handle, cmd, env, on_output)
            .await
    }

    async fn exec_as_user(
        &self,
        handle: &SandboxHandle,
        user: &str,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        self.owner_of(handle)
            .await?
            .exec_as_user(handle, user, cmd, env, on_output)
            .await
    }

    async fn exec_streamed(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_event: Option<OnStreamEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        self.owner_of(handle)
            .await?
            .exec_streamed(handle, cmd, env, on_event)
            .await
    }

    async fn is_alive(&self, handle: &SandboxHandle) -> Result<bool, AgentError> {
        self.owner_of(handle).await?.is_alive(handle).await
    }

    async fn write_file(
        &self,
        handle: &SandboxHandle,
        path: &str,
        contents: &[u8],
        mode: u32,
    ) -> Result<(), AgentError> {
        self.owner_of(handle)
            .await?
            .write_file(handle, path, contents, mode)
            .await
    }

    async fn read_file(&self, handle: &SandboxHandle, path: &str) -> Result<Vec<u8>, AgentError> {
        self.owner_of(handle).await?.read_file(handle, path).await
    }

    async fn read_file_bounded(
        &self,
        handle: &SandboxHandle,
        path: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>, AgentError> {
        self.owner_of(handle)
            .await?
            .read_file_bounded(handle, path, max_bytes)
            .await
    }

    async fn write_directory(
        &self,
        handle: &SandboxHandle,
        local_dir: &std::path::Path,
        target_path: &str,
    ) -> Result<(), AgentError> {
        self.owner_of(handle)
            .await?
            .write_directory(handle, local_dir, target_path)
            .await
    }

    async fn kill_processes(
        &self,
        handle: &SandboxHandle,
        pattern: &str,
        signal: KillSignal,
    ) -> Result<(), AgentError> {
        self.owner_of(handle)
            .await?
            .kill_processes(handle, pattern, signal)
            .await
    }

    async fn fence_process_trees(
        &self,
        handle: &SandboxHandle,
        patterns: &[&str],
    ) -> Result<(), AgentError> {
        self.owner_of(handle)
            .await?
            .fence_process_trees(handle, patterns)
            .await
    }

    async fn destroy(&self, handle: &SandboxHandle, purge_volumes: bool) -> Result<(), AgentError> {
        self.owner_of(handle)
            .await?
            .destroy(handle, purge_volumes)
            .await
    }

    async fn stop(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        self.owner_of(handle).await?.stop(handle).await
    }

    async fn start(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        self.owner_of(handle).await?.start(handle).await
    }

    async fn restart(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        self.owner_of(handle).await?.restart(handle).await
    }

    async fn resize_disk(
        &self,
        handle: &SandboxHandle,
        new_size_mb: u64,
    ) -> Result<(), AgentError> {
        self.owner_of(handle)
            .await?
            .resize_disk(handle, new_size_mb)
            .await
    }

    async fn recover(&self, run_id: i32) -> Result<Option<SandboxHandle>, AgentError> {
        self.local.recover(run_id).await
    }

    async fn recover_by_name(
        &self,
        container_name: &str,
    ) -> Result<Option<SandboxHandle>, AgentError> {
        self.local.recover_by_name(container_name).await
    }

    async fn recover_by_name_on(
        &self,
        node_id: Option<i32>,
        container_name: &str,
    ) -> Result<Option<SandboxHandle>, AgentError> {
        let recovered = self
            .for_node(node_id)
            .await?
            .recover_by_name(container_name)
            .await?;
        Ok(recovered.map(|h| Self::stamp(h, node_id)))
    }

    fn supports_backend(&self, backend: SandboxBackend) -> bool {
        self.local.supports_backend(backend)
    }

    async fn attach_pty(&self, handle: &SandboxHandle) -> Result<PtyAttachment, AgentError> {
        self.owner_of(handle).await?.attach_pty(handle).await
    }

    async fn take_snapshot(
        &self,
        handle: &SandboxHandle,
        label: Option<String>,
        max_size_bytes: u64,
    ) -> Result<SnapshotArtifact, AgentError> {
        self.owner_of(handle)
            .await?
            .take_snapshot(handle, label, max_size_bytes)
            .await
    }

    async fn create_from_snapshot(
        &self,
        artifact: &SnapshotArtifact,
        config: SandboxCreateConfig,
    ) -> Result<SandboxHandle, AgentError> {
        let node_id = config.node_id;
        let handle = self
            .for_node(node_id)
            .await?
            .create_from_snapshot(artifact, config)
            .await?;
        Ok(Self::stamp(handle, node_id))
    }

    async fn delete_image(&self, image_ref: &str) -> Result<(), AgentError> {
        self.local.delete_image(image_ref).await
    }

    fn name(&self) -> &str {
        self.local.name()
    }

    async fn is_available(&self) -> bool {
        self.local.is_available().await
    }

    async fn image_status(&self) -> Result<(bool, String), AgentError> {
        self.local.image_status().await
    }

    async fn rebuild_image(&self) -> Result<String, AgentError> {
        let image = self.local.rebuild_image().await?;
        tracing::info!(image = %image, "{}", worker_image_note(&image));
        Ok(image)
    }

    async fn rootfs_report(&self) -> Result<RootfsReport, AgentError> {
        self.local.rootfs_report().await
    }

    async fn gc_rootfs(&self) -> Result<RootfsGcReport, AgentError> {
        self.local.gc_rootfs().await
    }

    async fn rebuild_image_with_progress(
        &self,
        on_progress: tokio::sync::mpsc::Sender<String>,
    ) -> Result<String, AgentError> {
        let image = self
            .local
            .rebuild_image_with_progress(on_progress.clone())
            .await?;
        let note = worker_image_note(&image);
        tracing::info!(image = %image, "{}", note);
        // Best effort: the caller may have stopped listening.
        let _ = on_progress.send(note).await;
        Ok(image)
    }
}

/// The image rebuild only runs on the control plane (ADR-048): say what that
/// means for worker nodes, so it is not silent.
fn worker_image_note(image: &str) -> String {
    format!(
        "Rebuilt {image} on the control plane only. Worker nodes are not rebuilt: a worker \
         keeps using the copy of this image it already has, and a worker without one pulls \
         (or builds) it the next time it creates a sandbox. To refresh a worker, remove the \
         image on it (`docker image rm {image}`); its next sandbox fetches the current one."
    )
}

// ── Database-backed resolver ────────────────────────────────────────────

/// Node statuses whose agent is expected to answer. `draining`/`drained`
/// nodes keep serving the sandboxes they already host — draining only stops
/// new placement.
pub const REACHABLE_NODE_STATUSES: &[&str] = &["active", "draining", "drained"];

/// Whether a node agent address is mTLS (`https://`). Sandbox calls carry
/// the node token, environment variables and file contents, so they are
/// never made over plain `http://`.
pub fn is_https_address(address: &str) -> bool {
    // The same check the mTLS client builder uses, so a node this accepts
    // always gets the cluster CA and client identity.
    temps_deployments::cluster_ca::is_https_address(address)
}

/// Everything a node's client is built from. A change in any of it (node
/// re-joined at another address or with another token, sandbox defaults
/// edited, cluster CA rotated) needs a new client.
#[derive(Debug, Clone, PartialEq)]
struct NodeClientKey {
    address: String,
    token_encrypted: Option<String>,
    defaults: RemoteSandboxDefaults,
    /// The client trusts this cluster CA.
    cluster_ca_cert_pem: Option<String>,
}

impl NodeClientKey {
    fn new(node: &temps_entities::nodes::Model, settings: &temps_core::AppSettings) -> Self {
        Self {
            address: node.address.clone(),
            token_encrypted: node.token_encrypted.clone(),
            defaults: sandbox_defaults(&settings.agent_sandbox),
            cluster_ca_cert_pem: settings.multi_node.cluster_ca_cert_pem.clone(),
        }
    }
}

struct CachedNode {
    key: NodeClientKey,
    provider: Arc<dyn SandboxProvider>,
}

/// Builds the provider that drives sandboxes on one node, from its
/// decrypted agent token.
#[async_trait]
trait NodeProviderFactory: Send + Sync {
    async fn build(
        &self,
        node: &temps_entities::nodes::Model,
        token: String,
        defaults: RemoteSandboxDefaults,
    ) -> Result<Arc<dyn SandboxProvider>, String>;
}

/// The production factory: an mTLS client built exactly like remote
/// deployments build theirs.
struct MtlsProviderFactory {
    config_service: Arc<temps_config::ConfigService>,
    encryption_service: Arc<temps_core::EncryptionService>,
}

#[async_trait]
impl NodeProviderFactory for MtlsProviderFactory {
    async fn build(
        &self,
        node: &temps_entities::nodes::Model,
        token: String,
        defaults: RemoteSandboxDefaults,
    ) -> Result<Arc<dyn SandboxProvider>, String> {
        let client = temps_deployments::cluster_ca::build_node_http_client(
            &node.address,
            self.config_service.as_ref(),
            self.encryption_service.as_ref(),
            None,
        )
        .await
        .map_err(|e| e.to_string())?;
        Ok(Arc::new(
            RemoteSandboxProvider::new(
                node.id,
                node.name.clone(),
                node.address.clone(),
                token,
                client,
            )
            .with_defaults(defaults),
        ))
    }
}

/// The control plane's configured sandbox defaults (image, CPU, memory,
/// network mode),
/// resolved the same way `DockerSandboxConfig` resolves them locally.
fn sandbox_defaults(settings: &temps_core::AgentSandboxSettings) -> RemoteSandboxDefaults {
    let image = if settings.runtime == "custom" && !settings.custom_image.is_empty() {
        settings.custom_image.clone()
    } else {
        super::docker::image_name_for_runtime(&settings.runtime)
    };
    RemoteSandboxDefaults {
        image: Some(image),
        cpu_limit: Some(settings.cpu_limit),
        memory_limit_mb: Some(settings.memory_limit_mb),
        network_mode: Some(settings.network_mode.clone()),
    }
}

/// Resolves worker nodes from the `nodes` table, decrypting the node token
/// and building an mTLS-capable client exactly like remote deployments do.
pub struct DbRemoteNodeResolver {
    db: Arc<DatabaseConnection>,
    config_service: Arc<temps_config::ConfigService>,
    encryption_service: Arc<temps_core::EncryptionService>,
    factory: Arc<dyn NodeProviderFactory>,
    cache: RwLock<HashMap<i32, CachedNode>>,
}

impl DbRemoteNodeResolver {
    pub fn new(
        db: Arc<DatabaseConnection>,
        config_service: Arc<temps_config::ConfigService>,
        encryption_service: Arc<temps_core::EncryptionService>,
    ) -> Self {
        let factory = Arc::new(MtlsProviderFactory {
            config_service: config_service.clone(),
            encryption_service: encryption_service.clone(),
        });
        Self {
            db,
            config_service,
            encryption_service,
            factory,
            cache: RwLock::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    fn with_factory(mut self, factory: Arc<dyn NodeProviderFactory>) -> Self {
        self.factory = factory;
        self
    }

    fn unavailable(node_id: i32, node_name: Option<&str>, reason: String) -> AgentError {
        AgentError::SandboxNodeUnavailable {
            node_id,
            node_name: node_name.unwrap_or("unknown").to_string(),
            reason,
        }
    }

    /// Drop a node's cached client: it was removed, or can no longer be
    /// routed to.
    async fn forget(&self, node_id: i32) {
        if self.cache.write().await.remove(&node_id).is_some() {
            tracing::debug!(node_id, "sandbox node resolver: dropped cached client");
        }
    }

    /// Cache a freshly built client, and drop the clients of nodes that
    /// were removed or stopped being routable since (one query, only when a
    /// client is built, which is rare).
    async fn remember(&self, node_id: i32, key: NodeClientKey, provider: Arc<dyn SandboxProvider>) {
        use sea_orm::{ColumnTrait, QueryFilter};
        let routable = temps_entities::nodes::Entity::find()
            .filter(temps_entities::nodes::Column::Status.is_in(REACHABLE_NODE_STATUSES.to_vec()))
            .all(self.db.as_ref())
            .await;
        let mut cache = self.cache.write().await;
        match routable {
            Ok(nodes) => {
                let ids: std::collections::HashSet<i32> = nodes
                    .iter()
                    .filter(|n| is_https_address(&n.address))
                    .map(|n| n.id)
                    .collect();
                let before = cache.len();
                cache.retain(|id, _| ids.contains(id));
                if cache.len() < before {
                    tracing::debug!(
                        dropped = before - cache.len(),
                        "sandbox node resolver: dropped clients of removed or unroutable nodes"
                    );
                }
            }
            Err(e) => tracing::debug!(
                error = %e,
                "sandbox node resolver: could not list nodes to prune cached clients; \
                 keeping them until the next rebuild"
            ),
        }
        cache.insert(node_id, CachedNode { key, provider });
    }
}

#[async_trait]
impl RemoteNodeResolver for DbRemoteNodeResolver {
    async fn provider_for(&self, node_id: i32) -> Result<Arc<dyn SandboxProvider>, AgentError> {
        let Some(node) = temps_entities::nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?
        else {
            self.forget(node_id).await;
            return Err(Self::unavailable(
                node_id,
                None,
                "the node no longer exists".to_string(),
            ));
        };

        if !REACHABLE_NODE_STATUSES.contains(&node.status.as_str()) {
            self.forget(node.id).await;
            return Err(Self::unavailable(
                node.id,
                Some(&node.name),
                format!(
                    "the node is {}; sandboxes on it are unreachable until it reconnects",
                    node.status
                ),
            ));
        }

        // Sandbox calls carry the node token, environment variables and
        // file contents: never in cleartext.
        if !is_https_address(&node.address) {
            self.forget(node.id).await;
            return Err(Self::unavailable(
                node.id,
                Some(&node.name),
                // The address itself is not repeated: this error can reach
                // non-admin sandbox owners, and the address is internal.
                "the node's agent address uses plain http://, and sandbox calls carry \
                 tokens, environment variables and file contents; sandboxes need an https \
                 (mTLS) node address. Re-join the node with `temps join` to give it one"
                    .to_string(),
            ));
        }

        // Fail closed: the defaults carry the operator's network mode, and a
        // worker's built-in default is full network access.
        let settings = self.config_service.get_settings().await.map_err(|e| {
            Self::unavailable(
                node.id,
                Some(&node.name),
                format!(
                    "could not load the sandbox settings to apply on the node: {}",
                    e
                ),
            )
        })?;
        let key = NodeClientKey::new(&node, &settings);

        if let Some(cached) = self.cache.read().await.get(&node_id) {
            if cached.key == key {
                return Ok(cached.provider.clone());
            }
        }

        let encrypted = node.token_encrypted.clone().ok_or_else(|| {
            Self::unavailable(
                node.id,
                Some(&node.name),
                "the node has no stored agent token; re-join it with `temps join`".to_string(),
            )
        })?;
        let token = self
            .encryption_service
            .decrypt(&encrypted)
            .map_err(|e| e.to_string())
            .and_then(|bytes| String::from_utf8(bytes).map_err(|e| e.to_string()))
            .map_err(|e| {
                Self::unavailable(
                    node.id,
                    Some(&node.name),
                    format!("the stored agent token could not be decrypted: {}", e),
                )
            })?;
        let provider = self
            .factory
            .build(&node, token, key.defaults.clone())
            .await
            .map_err(|reason| Self::unavailable(node.id, Some(&node.name), reason))?;
        self.remember(node_id, key, provider.clone()).await;
        Ok(provider)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::Duration;

    /// Records which "host" each call landed on.
    struct Recorder {
        label: &'static str,
        calls: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl SandboxProvider for Recorder {
        async fn create(&self, config: SandboxCreateConfig) -> Result<SandboxHandle, AgentError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("{}:create", self.label));
            Ok(SandboxHandle {
                node_id: None,
                sandbox_id: format!("{}-{}", self.label, config.run_id),
                sandbox_name: "temps-sandbox-x".into(),
                work_dir: PathBuf::from("/w"),
                backend: SandboxBackend::Docker,
                image: String::new(),
            })
        }
        async fn exec(
            &self,
            _handle: &SandboxHandle,
            _cmd: Vec<String>,
            _env: HashMap<String, String>,
            _on_output: Option<OnEventCallback>,
        ) -> Result<SandboxExecResult, AgentError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("{}:exec", self.label));
            Ok(SandboxExecResult {
                exit_code: 0,
                stdout: self.label.into(),
                stderr: String::new(),
            })
        }
        async fn is_alive(&self, _h: &SandboxHandle) -> Result<bool, AgentError> {
            Ok(true)
        }
        async fn write_file(
            &self,
            _h: &SandboxHandle,
            _p: &str,
            _c: &[u8],
            _m: u32,
        ) -> Result<(), AgentError> {
            Ok(())
        }
        async fn read_file(&self, _h: &SandboxHandle, _p: &str) -> Result<Vec<u8>, AgentError> {
            Ok(vec![])
        }
        async fn write_directory(
            &self,
            _h: &SandboxHandle,
            _d: &std::path::Path,
            _t: &str,
        ) -> Result<(), AgentError> {
            Ok(())
        }
        async fn kill_processes(
            &self,
            _h: &SandboxHandle,
            _p: &str,
            _s: KillSignal,
        ) -> Result<(), AgentError> {
            Ok(())
        }
        async fn destroy(&self, _h: &SandboxHandle, _p: bool) -> Result<(), AgentError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("{}:destroy", self.label));
            Ok(())
        }
        async fn recover(&self, _run_id: i32) -> Result<Option<SandboxHandle>, AgentError> {
            Ok(None)
        }
        async fn recover_by_name(&self, name: &str) -> Result<Option<SandboxHandle>, AgentError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("{}:recover", self.label));
            Ok(Some(SandboxHandle {
                node_id: None,
                sandbox_id: "c".into(),
                sandbox_name: name.into(),
                work_dir: PathBuf::from("/w"),
                backend: SandboxBackend::Docker,
                image: String::new(),
            }))
        }
        fn name(&self) -> &str {
            self.label
        }
        async fn is_available(&self) -> bool {
            true
        }
        async fn image_status(&self) -> Result<(bool, String), AgentError> {
            Ok((true, String::new()))
        }
        async fn rebuild_image(&self) -> Result<String, AgentError> {
            Ok(String::new())
        }
    }

    struct FixedResolver {
        nodes: HashMap<i32, Arc<dyn SandboxProvider>>,
    }

    #[async_trait]
    impl RemoteNodeResolver for FixedResolver {
        async fn provider_for(&self, node_id: i32) -> Result<Arc<dyn SandboxProvider>, AgentError> {
            self.nodes
                .get(&node_id)
                .cloned()
                .ok_or(AgentError::SandboxNodeUnavailable {
                    node_id,
                    node_name: "gone".into(),
                    reason: "offline".into(),
                })
        }
    }

    fn config(node_id: Option<i32>) -> SandboxCreateConfig {
        SandboxCreateConfig {
            node_id,
            run_id: 1,
            container_name_override: Some("x".into()),
            host_work_dir: PathBuf::from("/tmp/x"),
            workspace_volume: None,
            image: None,
            cpu_limit: None,
            memory_limit_mb: None,
            pids_limit: None,
            disk_size_mb: None,
            network_mode: None,
            env_vars: HashMap::new(),
            idle_timeout: Duration::from_secs(60),
            backend: None,
            owner_user_id: None,
        }
    }

    fn router(calls: &Arc<Mutex<Vec<String>>>) -> NodeRoutingSandboxProvider {
        let local: Arc<dyn SandboxProvider> = Arc::new(Recorder {
            label: "local",
            calls: calls.clone(),
        });
        let worker: Arc<dyn SandboxProvider> = Arc::new(Recorder {
            label: "worker7",
            calls: calls.clone(),
        });
        NodeRoutingSandboxProvider::new(
            local,
            Arc::new(FixedResolver {
                nodes: HashMap::from([(7, worker)]),
            }),
        )
    }

    #[tokio::test]
    async fn create_and_handle_calls_follow_the_node() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let router = router(&calls);

        let remote = router.create(config(Some(7))).await.unwrap();
        assert_eq!(remote.node_id, Some(7));
        let out = router
            .exec(&remote, vec!["true".into()], HashMap::new(), None)
            .await
            .unwrap();
        assert_eq!(out.stdout, "worker7");
        router.destroy(&remote, false).await.unwrap();

        let local = router.create(config(None)).await.unwrap();
        assert_eq!(local.node_id, None);
        let out = router
            .exec(&local, vec!["true".into()], HashMap::new(), None)
            .await
            .unwrap();
        assert_eq!(out.stdout, "local");

        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                "worker7:create",
                "worker7:exec",
                "worker7:destroy",
                "local:create",
                "local:exec"
            ]
        );
    }

    #[tokio::test]
    async fn unreachable_node_is_an_error_not_a_local_fallback() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let router = router(&calls);
        let err = router.create(config(Some(99))).await.unwrap_err();
        assert!(matches!(
            err,
            AgentError::SandboxNodeUnavailable { node_id: 99, .. }
        ));
        assert!(calls.lock().unwrap().is_empty(), "nothing may run locally");
    }

    fn worker_handle(node_id: Option<i32>) -> SandboxHandle {
        SandboxHandle {
            node_id,
            sandbox_id: "c".into(),
            sandbox_name: "temps-sandbox-x".into(),
            work_dir: PathBuf::from("/w"),
            backend: SandboxBackend::Docker,
            image: String::new(),
        }
    }

    fn assert_unreachable<T>(result: Result<T, AgentError>, call: &str) {
        match result {
            Err(AgentError::SandboxNodeUnavailable { node_id: 99, .. }) => {}
            Err(other) => panic!("{call}: expected the node to be unavailable, got {other:?}"),
            Ok(_) => panic!("{call}: succeeded on a node that cannot be resolved"),
        }
    }

    /// A handle on a node that cannot be resolved fails every call, and none
    /// of them falls back to the control plane's provider.
    #[tokio::test]
    async fn unreachable_node_runs_no_handle_call_locally() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let router = router(&calls);
        let h = worker_handle(Some(99));

        assert_unreachable(
            router
                .exec(&h, vec!["true".into()], HashMap::new(), None)
                .await,
            "exec",
        );
        assert_unreachable(
            router
                .exec_as_root(&h, vec!["true".into()], HashMap::new(), None)
                .await,
            "exec_as_root",
        );
        assert_unreachable(router.destroy(&h, true).await, "destroy");
        assert_unreachable(router.read_file(&h, "/etc/hosts").await, "read_file");
        assert_unreachable(
            router.write_file(&h, "/x", b"secret", 0o600).await,
            "write_file",
        );
        assert_unreachable(router.is_alive(&h).await, "is_alive");
        assert_unreachable(router.stop(&h).await, "stop");
        assert_unreachable(router.start(&h).await, "start");
        assert_unreachable(
            router.kill_processes(&h, "node", KillSignal::Term).await,
            "kill_processes",
        );
        assert_unreachable(
            router.recover_by_name_on(Some(99), "temps-sandbox-x").await,
            "recover_by_name_on",
        );

        assert!(
            calls.lock().unwrap().is_empty(),
            "nothing may run locally: {:?}",
            calls.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn rebuild_reports_that_workers_keep_their_image() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let router = router(&calls);
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);

        router.rebuild_image_with_progress(tx).await.unwrap();

        let mut lines = Vec::new();
        while let Ok(line) = rx.try_recv() {
            lines.push(line);
        }
        let note = lines.last().expect("a closing note");
        assert!(note.contains("control plane only"), "{note}");
        assert!(note.contains("Worker nodes are not rebuilt"), "{note}");
    }

    #[test]
    fn https_addresses_are_recognised() {
        assert!(is_https_address("https://10.0.0.1:3100"));
        assert!(is_https_address(" HTTPS://node:3100"));
        assert!(!is_https_address("http://10.0.0.1:3100"));
        assert!(!is_https_address("10.0.0.1:3100"));
        assert!(!is_https_address(""));
    }

    // ── DbRemoteNodeResolver ────────────────────────────────────────────

    use sea_orm::{DatabaseBackend, DbErr, MockDatabase};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use temps_entities::{nodes, settings};

    fn encryption() -> Arc<temps_core::EncryptionService> {
        Arc::new(temps_core::EncryptionService::new_from_password("test"))
    }

    fn node(id: i32, status: &str, address: &str, token: &str) -> nodes::Model {
        let now = chrono::Utc::now();
        nodes::Model {
            architecture: None,
            id,
            name: format!("worker-{id}"),
            token_hash: "hash".to_string(),
            token_encrypted: Some(encryption().encrypt_string(token).expect("encrypt token")),
            address: address.to_string(),
            private_address: "10.100.0.7".to_string(),
            public_endpoint: None,
            wg_public_key: None,
            mesh_wg_public_key: None,
            mesh_wg_endpoint: None,
            mesh_wg_address: None,
            role: "worker".to_string(),
            status: status.to_string(),
            labels: serde_json::json!({}),
            capacity: serde_json::json!({}),
            last_heartbeat: Some(now),
            edge_public_key: None,
            compute_cidr: None,
            underlay_address: None,
            failover_at: None,
            dns_resolver_running: None,
            dns_resolver_tasks_alive: None,
            dns_resolver_last_sync_at: None,
            dns_resolver_consecutive_failures: 0,
            dns_resolver_last_error: None,
            dns_resolver_record_count: None,
            public_ingress_enabled: false,
            public_ingress_running: None,
            public_ingress_last_error: None,
            public_ingress_certificate_count: None,
            public_ingress_route_count: None,
            public_ingress_unsupported_route_count: None,
            public_ingress_unsupported_reasons: serde_json::json!([]),
            created_at: now,
            updated_at: now,
        }
    }

    fn settings_row() -> settings::Model {
        let now = chrono::Utc::now();
        settings::Model {
            id: 1,
            data: temps_core::AppSettings::default().to_json(),
            created_at: now,
            updated_at: now,
        }
    }

    fn server_config() -> temps_config::ServerConfig {
        temps_config::ServerConfig {
            address: "127.0.0.1:0".into(),
            database_url: "postgres://test".into(),
            tls_address: None,
            console_address: "127.0.0.1:0".into(),
            console_admin_address: None,
            admin_allowed_ips: Vec::new(),
            admin_allowed_hosts: Vec::new(),
            admin_trust_forwarded_for: false,
            data_dir: PathBuf::from("/tmp/temps-node-routing-tests"),
            auth_secret: "default-32-byte-key-for-testing!".into(),
            encryption_key: "another-32-byte-key-for-testing!".into(),
            api_base_url: "/api".into(),
            postgres_max_connections: None,
            postgres_min_connections: None,
            postgres_connect_timeout_secs: None,
            postgres_acquire_timeout_secs: None,
            postgres_idle_timeout_secs: None,
            postgres_max_lifetime_secs: None,
            clickhouse_url: None,
            clickhouse_database: None,
            clickhouse_user: None,
            clickhouse_password: None,
            docker_extra_networks: Vec::new(),
        }
    }

    /// Counts builds and records the token each one got.
    #[derive(Default)]
    struct CountingFactory {
        builds: AtomicUsize,
        tokens: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl NodeProviderFactory for CountingFactory {
        async fn build(
            &self,
            node: &nodes::Model,
            token: String,
            _defaults: RemoteSandboxDefaults,
        ) -> Result<Arc<dyn SandboxProvider>, String> {
            self.builds.fetch_add(1, Ordering::SeqCst);
            self.tokens.lock().unwrap().push(token);
            let label: &'static str = if node.address.contains("10.0.0.2") {
                "rejoined"
            } else {
                "first"
            };
            Ok(Arc::new(Recorder {
                label,
                calls: Arc::new(Mutex::new(Vec::new())),
            }))
        }
    }

    fn resolver(db: MockDatabase, factory: Arc<CountingFactory>) -> DbRemoteNodeResolver {
        let db = Arc::new(db.into_connection());
        let config = Arc::new(temps_config::ConfigService::new(
            Arc::new(server_config()),
            db.clone(),
        ));
        DbRemoteNodeResolver::new(db, config, encryption()).with_factory(factory)
    }

    fn unavailable_reason(err: AgentError) -> String {
        match err {
            AgentError::SandboxNodeUnavailable { reason, .. } => reason,
            other => panic!("expected SandboxNodeUnavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolver_refuses_nodes_that_are_not_routable() {
        for status in ["offline", "pending", "removed"] {
            let factory = Arc::new(CountingFactory::default());
            let r =
                resolver(
                    MockDatabase::new(DatabaseBackend::Postgres).append_query_results([vec![
                        node(7, status, "https://10.0.0.1:3100", "t"),
                    ]]),
                    factory.clone(),
                );
            let reason = unavailable_reason(r.provider_for(7).await.err().expect("refused"));
            assert!(reason.contains(status), "{status}: {reason}");
            assert_eq!(factory.builds.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn resolver_serves_draining_and_drained_nodes() {
        for status in ["draining", "drained"] {
            let factory = Arc::new(CountingFactory::default());
            let n = node(7, status, "https://10.0.0.1:3100", "t");
            let r = resolver(
                MockDatabase::new(DatabaseBackend::Postgres)
                    .append_query_results([vec![n.clone()]])
                    .append_query_results([vec![settings_row()]])
                    .append_query_results([vec![n]]),
                factory.clone(),
            );
            r.provider_for(7)
                .await
                .expect("existing sandboxes stay reachable");
            assert_eq!(factory.builds.load(Ordering::SeqCst), 1, "{status}");
        }
    }

    /// Sandbox calls carry the node token, env vars and file contents.
    #[tokio::test]
    async fn resolver_refuses_plain_http_nodes() {
        let factory = Arc::new(CountingFactory::default());
        let r = resolver(
            MockDatabase::new(DatabaseBackend::Postgres).append_query_results([vec![node(
                7,
                "active",
                "http://10.0.0.1:3100",
                "t",
            )]]),
            factory.clone(),
        );
        let reason = unavailable_reason(r.provider_for(7).await.err().expect("refused"));
        assert!(reason.contains("http://"), "{reason}");
        assert!(reason.contains("https (mTLS)"), "{reason}");
        // The error can reach non-admin sandbox owners: no internal address.
        assert!(!reason.contains("10.0.0.1"), "{reason}");
        assert_eq!(factory.builds.load(Ordering::SeqCst), 0);
    }

    /// The defaults carry the operator's network mode: a node is not used
    /// with the worker's built-in (full network) defaults.
    #[tokio::test]
    async fn resolver_fails_closed_when_settings_cannot_be_loaded() {
        let factory = Arc::new(CountingFactory::default());
        let r = resolver(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![node(7, "active", "https://10.0.0.1:3100", "t")]])
                .append_query_errors([DbErr::Custom("settings table unavailable".into())]),
            factory.clone(),
        );
        let reason = unavailable_reason(r.provider_for(7).await.err().expect("refused"));
        assert!(
            reason.contains("could not load the sandbox settings"),
            "{reason}"
        );
        assert_eq!(factory.builds.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn resolver_reuses_the_client_until_the_node_changes() {
        let factory = Arc::new(CountingFactory::default());
        let first = node(7, "active", "https://10.0.0.1:3100", "token-1");
        let mut moved = first.clone();
        moved.address = "https://10.0.0.2:3100".to_string();
        let mut rotated = moved.clone();
        rotated.token_encrypted = Some(encryption().encrypt_string("token-2").expect("encrypt"));
        let r = resolver(
            MockDatabase::new(DatabaseBackend::Postgres)
                // 1st call: node, settings (then cached), prune listing.
                .append_query_results([vec![first.clone()]])
                .append_query_results([vec![settings_row()]])
                .append_query_results([vec![first.clone()]])
                // 2nd call: unchanged node → cached client.
                .append_query_results([vec![first]])
                // 3rd call: re-joined at another address → rebuilt.
                .append_query_results([vec![moved.clone()]])
                .append_query_results([vec![moved]])
                // 4th call: rotated token → rebuilt.
                .append_query_results([vec![rotated.clone()]])
                .append_query_results([vec![rotated]]),
            factory.clone(),
        );

        assert_eq!(r.provider_for(7).await.expect("first").name(), "first");
        assert_eq!(r.provider_for(7).await.expect("cached").name(), "first");
        assert_eq!(factory.builds.load(Ordering::SeqCst), 1);
        assert_eq!(r.provider_for(7).await.expect("moved").name(), "rejoined");
        assert_eq!(factory.builds.load(Ordering::SeqCst), 2);
        r.provider_for(7).await.expect("rotated");
        assert_eq!(factory.builds.load(Ordering::SeqCst), 3);
        assert_eq!(
            *factory.tokens.lock().unwrap(),
            vec!["token-1", "token-1", "token-2"]
        );
    }

    #[test]
    fn client_key_changes_with_defaults_and_cluster_ca() {
        let n = node(7, "active", "https://10.0.0.1:3100", "t");
        let base = temps_core::AppSettings::default();
        let key = NodeClientKey::new(&n, &base);
        assert_eq!(key, NodeClientKey::new(&n, &base));

        let mut network = base.clone();
        network.agent_sandbox.network_mode = "none".to_string();
        assert_ne!(key, NodeClientKey::new(&n, &network), "network mode");

        let mut image = base.clone();
        image.agent_sandbox.runtime = "custom".to_string();
        image.agent_sandbox.custom_image = "registry.example.test/sandbox:2".to_string();
        assert_ne!(key, NodeClientKey::new(&n, &image), "image");

        let mut ca = base.clone();
        ca.multi_node.cluster_ca_cert_pem = Some("rotated-ca".to_string());
        assert_ne!(key, NodeClientKey::new(&n, &ca), "cluster CA");
    }

    #[tokio::test]
    async fn resolver_drops_clients_of_removed_nodes() {
        let factory = Arc::new(CountingFactory::default());
        let seven = node(7, "active", "https://10.0.0.1:3100", "t");
        let eight = node(8, "active", "https://10.0.0.3:3100", "t");
        let r = resolver(
            MockDatabase::new(DatabaseBackend::Postgres)
                // Node 7 resolved and cached.
                .append_query_results([vec![seven.clone()]])
                .append_query_results([vec![settings_row()]])
                .append_query_results([vec![seven.clone(), eight.clone()]])
                // Node 8 resolved; meanwhile node 7 was removed, so the
                // prune listing no longer has it.
                .append_query_results([vec![eight.clone()]])
                .append_query_results([vec![eight]])
                // Node 7 again: gone.
                .append_query_results([Vec::<nodes::Model>::new()]),
            factory.clone(),
        );

        r.provider_for(7).await.expect("seven");
        assert!(r.cache.read().await.contains_key(&7));
        r.provider_for(8).await.expect("eight");
        assert!(
            !r.cache.read().await.contains_key(&7),
            "a removed node's client is dropped when another is built"
        );
        let reason = unavailable_reason(r.provider_for(7).await.err().expect("gone"));
        assert!(reason.contains("no longer exists"), "{reason}");
        assert_eq!(r.cache.read().await.len(), 1);
    }

    #[tokio::test]
    async fn resolver_forgets_a_node_that_stops_being_routable() {
        let factory = Arc::new(CountingFactory::default());
        let active = node(7, "active", "https://10.0.0.1:3100", "t");
        let offline = node(7, "offline", "https://10.0.0.1:3100", "t");
        let r = resolver(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![active.clone()]])
                .append_query_results([vec![settings_row()]])
                .append_query_results([vec![active]])
                .append_query_results([vec![offline]]),
            factory,
        );

        r.provider_for(7).await.expect("active");
        assert!(r.cache.read().await.contains_key(&7));
        r.provider_for(7).await.err().expect("offline");
        assert!(r.cache.read().await.is_empty());
    }

    #[tokio::test]
    async fn recovery_asks_the_owning_node_and_stamps_it() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let router = router(&calls);
        let h = router
            .recover_by_name_on(Some(7), "temps-sandbox-x")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(h.node_id, Some(7));
        let h = router
            .recover_by_name_on(None, "temps-sandbox-x")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(h.node_id, None);
        assert_eq!(
            *calls.lock().unwrap(),
            vec!["worker7:recover", "local:recover"]
        );
    }
}
