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
        self.local.rebuild_image().await
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
        self.local.rebuild_image_with_progress(on_progress).await
    }
}

// ── Database-backed resolver ────────────────────────────────────────────

/// Node statuses whose agent is expected to answer. `draining`/`drained`
/// nodes keep serving the sandboxes they already host — draining only stops
/// new placement.
pub const REACHABLE_NODE_STATUSES: &[&str] = &["active", "draining", "drained"];

struct CachedNode {
    address: String,
    token_encrypted: Option<String>,
    defaults: RemoteSandboxDefaults,
    /// The client trusts this cluster CA; a rotated CA needs a new client.
    cluster_ca_cert_pem: Option<String>,
    provider: Arc<dyn SandboxProvider>,
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
    cache: RwLock<HashMap<i32, CachedNode>>,
}

impl DbRemoteNodeResolver {
    pub fn new(
        db: Arc<DatabaseConnection>,
        config_service: Arc<temps_config::ConfigService>,
        encryption_service: Arc<temps_core::EncryptionService>,
    ) -> Self {
        Self {
            db,
            config_service,
            encryption_service,
            cache: RwLock::new(HashMap::new()),
        }
    }

    fn unavailable(node_id: i32, node_name: Option<&str>, reason: String) -> AgentError {
        AgentError::SandboxNodeUnavailable {
            node_id,
            node_name: node_name.unwrap_or("unknown").to_string(),
            reason,
        }
    }
}

#[async_trait]
impl RemoteNodeResolver for DbRemoteNodeResolver {
    async fn provider_for(&self, node_id: i32) -> Result<Arc<dyn SandboxProvider>, AgentError> {
        let node = temps_entities::nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?
            .ok_or_else(|| {
                Self::unavailable(node_id, None, "the node no longer exists".to_string())
            })?;

        if !REACHABLE_NODE_STATUSES.contains(&node.status.as_str()) {
            return Err(Self::unavailable(
                node.id,
                Some(&node.name),
                format!(
                    "the node is {}; sandboxes on it are unreachable until it reconnects",
                    node.status
                ),
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
        let defaults = sandbox_defaults(&settings.agent_sandbox);
        let cluster_ca_cert_pem = settings.multi_node.cluster_ca_cert_pem.clone();

        if let Some(cached) = self.cache.read().await.get(&node_id) {
            if cached.address == node.address
                && cached.token_encrypted == node.token_encrypted
                && cached.defaults == defaults
                && cached.cluster_ca_cert_pem == cluster_ca_cert_pem
            {
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
        let client = temps_deployments::cluster_ca::build_node_http_client(
            &node.address,
            self.config_service.as_ref(),
            self.encryption_service.as_ref(),
            None,
        )
        .await
        .map_err(|e| Self::unavailable(node.id, Some(&node.name), e.to_string()))?;

        let provider: Arc<dyn SandboxProvider> = Arc::new(
            RemoteSandboxProvider::new(
                node.id,
                node.name.clone(),
                node.address.clone(),
                token,
                client,
            )
            .with_defaults(defaults.clone()),
        );
        self.cache.write().await.insert(
            node_id,
            CachedNode {
                address: node.address,
                token_encrypted: node.token_encrypted,
                defaults,
                cluster_ca_cert_pem,
                provider: provider.clone(),
            },
        );
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
