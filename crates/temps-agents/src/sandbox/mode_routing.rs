// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which path serves the control plane's own sandboxes (ADR-055 phase 6).
//!
//! [`ModeRoutingSandboxProvider`] sits where the host's sandbox provider
//! always sat and reads node 0's `sandbox_driver_mode` per call:
//!
//! - `legacy`: every call goes to the old provider, unchanged (the default);
//! - `shadow`: the old provider still serves; each create is also planned by
//!   the driver facade and the plan is logged where it differs from what the
//!   old path did (another driver, an unapplied limit);
//! - `enforce`: the driver facade serves.
//!
//! Both paths dispatch on the handle's backend, so a sandbox created under
//! one mode is served under the other: an administrator can move a node
//! back to `legacy` at any time, which is the rollback.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, EntityTrait};
use temps_entities::sandbox_driver_mode::SandboxDriverMode;

use super::facade::DriverSandboxProvider;
use super::{
    KillSignal, OnStreamEventCallback, PtyAttachment, RootfsGcReport, RootfsReport,
    RuntimeCompatibility, SandboxBackend, SandboxCreateConfig, SandboxExecResult, SandboxHandle,
    SandboxProvider, SnapshotArtifact,
};
use crate::ai_cli::OnEventCallback;
use crate::error::AgentError;

/// How long a read of node 0's mode is reused, and so how long a change made
/// through the mode API takes to reach new sandbox operations on the control
/// plane (the API reports it as `propagation_secs`).
pub const CONTROL_PLANE_MODE_TTL: Duration = Duration::from_secs(5);

/// Node 0's sandbox driver mode.
#[async_trait]
pub trait ControlPlaneDriverMode: Send + Sync {
    async fn mode(&self) -> SandboxDriverMode;
}

/// Reads node 0's mode from `network_config` (the singleton row, id 1),
/// cached for [`CONTROL_PLANE_MODE_TTL`]. A failed read keeps the last known
/// mode, or `legacy` when there is none: the old path is always safe.
pub struct DbControlPlaneDriverMode {
    db: Arc<DatabaseConnection>,
    ttl: Duration,
    cached: Mutex<Option<(Instant, SandboxDriverMode)>>,
}

impl DbControlPlaneDriverMode {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db,
            ttl: CONTROL_PLANE_MODE_TTL,
            cached: Mutex::new(None),
        }
    }

    /// Forget the cached mode; the next call reads it again.
    pub fn invalidate(&self) {
        if let Ok(mut cached) = self.cached.lock() {
            *cached = None;
        }
    }

    fn fresh(&self) -> Option<SandboxDriverMode> {
        let cached = self.cached.lock().ok()?;
        cached
            .as_ref()
            .filter(|(at, _)| at.elapsed() < self.ttl)
            .map(|(_, mode)| *mode)
    }

    fn last_known(&self) -> Option<SandboxDriverMode> {
        self.cached.lock().ok()?.as_ref().map(|(_, mode)| *mode)
    }

    fn remember(&self, mode: SandboxDriverMode) {
        if let Ok(mut cached) = self.cached.lock() {
            *cached = Some((Instant::now(), mode));
        }
    }
}

#[async_trait]
impl ControlPlaneDriverMode for DbControlPlaneDriverMode {
    async fn mode(&self) -> SandboxDriverMode {
        if let Some(mode) = self.fresh() {
            return mode;
        }
        match temps_entities::network_config::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await
        {
            Ok(Some(row)) => {
                self.remember(row.control_plane_sandbox_driver_mode);
                row.control_plane_sandbox_driver_mode
            }
            Ok(None) => {
                tracing::warn!(
                    "network_config row 1 is missing; serving the control plane's sandboxes \
                     through the legacy providers"
                );
                SandboxDriverMode::Legacy
            }
            Err(error) => {
                let mode = self.last_known().unwrap_or(SandboxDriverMode::Legacy);
                tracing::warn!(
                    error = %error,
                    mode = %mode,
                    "could not read the control plane's sandbox driver mode; keeping the last \
                     known mode (legacy when none)"
                );
                mode
            }
        }
    }
}

/// Serves node 0's sandboxes through the old providers or the driver facade,
/// by node 0's mode.
pub struct ModeRoutingSandboxProvider {
    legacy: Arc<dyn SandboxProvider>,
    facade: Arc<DriverSandboxProvider>,
    /// The backend the old path uses when a request names none.
    legacy_default: SandboxBackend,
    mode: Arc<dyn ControlPlaneDriverMode>,
}

impl ModeRoutingSandboxProvider {
    pub fn new(
        legacy: Arc<dyn SandboxProvider>,
        facade: Arc<DriverSandboxProvider>,
        legacy_default: SandboxBackend,
        mode: Arc<dyn ControlPlaneDriverMode>,
    ) -> Self {
        Self {
            legacy,
            facade,
            legacy_default,
            mode,
        }
    }

    async fn serving(&self) -> (&dyn SandboxProvider, SandboxDriverMode) {
        let mode = self.mode.mode().await;
        if mode.uses_drivers() {
            (self.facade.as_ref(), mode)
        } else {
            (self.legacy.as_ref(), mode)
        }
    }

    async fn provider(&self) -> &dyn SandboxProvider {
        self.serving().await.0
    }

    /// `shadow`: what the facade would have done with a create the old path
    /// is about to serve. Logged, never acted on.
    fn shadow_plan(&self, config: &SandboxCreateConfig, operation: &str) {
        let legacy = config.backend.unwrap_or(self.legacy_default);
        let sandbox = config
            .container_name_override
            .clone()
            .unwrap_or_else(|| config.run_id.to_string());
        match self.facade.plan_create(config) {
            Ok(plan) if plan.backend == legacy && plan.conditions.is_empty() => {
                tracing::debug!(
                    sandbox = %sandbox,
                    operation,
                    driver = %plan.backend,
                    "sandbox driver shadow: the facade agrees with the legacy path"
                );
            }
            Ok(plan) => {
                let conditions: Vec<String> = plan
                    .conditions
                    .iter()
                    .map(|c| format!("{:?}={:?} {}: {}", c.kind, c.status, c.reason, c.message))
                    .collect();
                tracing::info!(
                    sandbox = %sandbox,
                    operation,
                    legacy_backend = %legacy,
                    driver = %plan.backend,
                    conditions = ?conditions,
                    "sandbox driver shadow: the facade would differ from the legacy path"
                );
            }
            Err(error) => {
                tracing::info!(
                    sandbox = %sandbox,
                    operation,
                    legacy_backend = %legacy,
                    error = %error,
                    "sandbox driver shadow: the facade would refuse this sandbox"
                );
            }
        }
    }
}

#[async_trait]
impl SandboxProvider for ModeRoutingSandboxProvider {
    async fn create(&self, config: SandboxCreateConfig) -> Result<SandboxHandle, AgentError> {
        let (provider, mode) = self.serving().await;
        if mode == SandboxDriverMode::Shadow {
            self.shadow_plan(&config, "create");
        }
        provider.create(config).await
    }

    async fn image_identity(&self, handle: &SandboxHandle) -> Result<String, AgentError> {
        self.provider().await.image_identity(handle).await
    }

    async fn check_agent_runtime(
        &self,
        handle: &SandboxHandle,
    ) -> Result<RuntimeCompatibility, AgentError> {
        self.provider().await.check_agent_runtime(handle).await
    }

    async fn recover_agent_harness(
        &self,
        handle: &SandboxHandle,
        epoch: u64,
    ) -> Result<(), AgentError> {
        self.provider()
            .await
            .recover_agent_harness(handle, epoch)
            .await
    }

    async fn model_relay_base_url(
        &self,
        handle: &SandboxHandle,
        control_plane_url: &str,
    ) -> Result<String, AgentError> {
        self.provider()
            .await
            .model_relay_base_url(handle, control_plane_url)
            .await
    }

    async fn git_relay_base_url(
        &self,
        handle: &SandboxHandle,
        control_plane_url: &str,
    ) -> Result<String, AgentError> {
        self.provider()
            .await
            .git_relay_base_url(handle, control_plane_url)
            .await
    }

    async fn harness_mcp_url(
        &self,
        handle: &SandboxHandle,
        control_plane_url: &str,
        registered_url: &str,
    ) -> Result<String, AgentError> {
        self.provider()
            .await
            .harness_mcp_url(handle, control_plane_url, registered_url)
            .await
    }

    async fn configure_application_network(
        &self,
        handle: &SandboxHandle,
        network_name: &str,
        service_containers: &[String],
    ) -> Result<(), AgentError> {
        self.provider()
            .await
            .configure_application_network(handle, network_name, service_containers)
            .await
    }

    async fn connect_agent_runtime(
        &self,
        handle: &SandboxHandle,
    ) -> Result<PtyAttachment, AgentError> {
        self.provider().await.connect_agent_runtime(handle).await
    }

    async fn connect_port(
        &self,
        handle: &SandboxHandle,
        port: u16,
    ) -> Result<PtyAttachment, AgentError> {
        self.provider().await.connect_port(handle, port).await
    }

    async fn exec(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        self.provider()
            .await
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
        self.provider()
            .await
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
        self.provider()
            .await
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
        self.provider()
            .await
            .exec_streamed(handle, cmd, env, on_event)
            .await
    }

    async fn is_alive(&self, handle: &SandboxHandle) -> Result<bool, AgentError> {
        self.provider().await.is_alive(handle).await
    }

    async fn write_file(
        &self,
        handle: &SandboxHandle,
        path: &str,
        contents: &[u8],
        mode: u32,
    ) -> Result<(), AgentError> {
        self.provider()
            .await
            .write_file(handle, path, contents, mode)
            .await
    }

    async fn read_file(&self, handle: &SandboxHandle, path: &str) -> Result<Vec<u8>, AgentError> {
        self.provider().await.read_file(handle, path).await
    }

    async fn read_file_bounded(
        &self,
        handle: &SandboxHandle,
        path: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>, AgentError> {
        self.provider()
            .await
            .read_file_bounded(handle, path, max_bytes)
            .await
    }

    async fn write_directory(
        &self,
        handle: &SandboxHandle,
        local_dir: &Path,
        target_path: &str,
    ) -> Result<(), AgentError> {
        self.provider()
            .await
            .write_directory(handle, local_dir, target_path)
            .await
    }

    async fn sync_workspace_to_host(
        &self,
        handle: &SandboxHandle,
        host_dir: &Path,
    ) -> Result<(), AgentError> {
        self.provider()
            .await
            .sync_workspace_to_host(handle, host_dir)
            .await
    }

    async fn kill_processes(
        &self,
        handle: &SandboxHandle,
        pattern: &str,
        signal: KillSignal,
    ) -> Result<(), AgentError> {
        self.provider()
            .await
            .kill_processes(handle, pattern, signal)
            .await
    }

    async fn fence_process_trees(
        &self,
        handle: &SandboxHandle,
        patterns: &[&str],
    ) -> Result<(), AgentError> {
        self.provider()
            .await
            .fence_process_trees(handle, patterns)
            .await
    }

    async fn destroy(&self, handle: &SandboxHandle, purge_volumes: bool) -> Result<(), AgentError> {
        self.provider().await.destroy(handle, purge_volumes).await
    }

    async fn stop(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        self.provider().await.stop(handle).await
    }

    async fn start(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        self.provider().await.start(handle).await
    }

    async fn restart(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        self.provider().await.restart(handle).await
    }

    async fn resize_disk(
        &self,
        handle: &SandboxHandle,
        new_size_mb: u64,
    ) -> Result<(), AgentError> {
        self.provider().await.resize_disk(handle, new_size_mb).await
    }

    async fn recover(&self, run_id: i32) -> Result<Option<SandboxHandle>, AgentError> {
        self.provider().await.recover(run_id).await
    }

    async fn recover_by_name(
        &self,
        container_name: &str,
    ) -> Result<Option<SandboxHandle>, AgentError> {
        self.provider().await.recover_by_name(container_name).await
    }

    async fn release_orphaned_resources(&self, container_name: &str) -> Result<(), AgentError> {
        self.provider()
            .await
            .release_orphaned_resources(container_name)
            .await
    }

    /// Both paths are built over the same backends, so they agree.
    fn supports_backend(&self, backend: SandboxBackend) -> bool {
        self.legacy.supports_backend(backend)
    }

    async fn attach_pty(&self, handle: &SandboxHandle) -> Result<PtyAttachment, AgentError> {
        self.provider().await.attach_pty(handle).await
    }

    async fn take_snapshot(
        &self,
        handle: &SandboxHandle,
        label: Option<String>,
        max_size_bytes: u64,
    ) -> Result<SnapshotArtifact, AgentError> {
        self.provider()
            .await
            .take_snapshot(handle, label, max_size_bytes)
            .await
    }

    async fn create_from_snapshot(
        &self,
        artifact: &SnapshotArtifact,
        config: SandboxCreateConfig,
    ) -> Result<SandboxHandle, AgentError> {
        let (provider, mode) = self.serving().await;
        if mode == SandboxDriverMode::Shadow {
            self.shadow_plan(&config, "restore");
        }
        provider.create_from_snapshot(artifact, config).await
    }

    async fn delete_image(&self, image_ref: &str) -> Result<(), AgentError> {
        self.provider().await.delete_image(image_ref).await
    }

    fn name(&self) -> &str {
        self.legacy.name()
    }

    async fn is_available(&self) -> bool {
        self.legacy.is_available().await
    }

    async fn image_status(&self) -> Result<(bool, String), AgentError> {
        self.legacy.image_status().await
    }

    async fn rebuild_image(&self) -> Result<String, AgentError> {
        self.legacy.rebuild_image().await
    }

    async fn rootfs_report(&self) -> Result<RootfsReport, AgentError> {
        self.legacy.rootfs_report().await
    }

    async fn gc_rootfs(&self) -> Result<RootfsGcReport, AgentError> {
        self.legacy.gc_rootfs().await
    }

    async fn rebuild_image_with_progress(
        &self,
        on_progress: tokio::sync::mpsc::Sender<String>,
    ) -> Result<String, AgentError> {
        self.legacy.rebuild_image_with_progress(on_progress).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::facade::tests::{config, facade, FakeBackend};
    use sea_orm::{DatabaseBackend, MockDatabase};

    struct FixedMode(Mutex<SandboxDriverMode>);

    #[async_trait]
    impl ControlPlaneDriverMode for FixedMode {
        async fn mode(&self) -> SandboxDriverMode {
            self.0
                .lock()
                .map(|m| *m)
                .unwrap_or(SandboxDriverMode::Legacy)
        }
    }

    fn router(
        mode: SandboxDriverMode,
    ) -> (
        ModeRoutingSandboxProvider,
        Arc<FakeBackend>,
        Arc<FakeBackend>,
        Arc<FixedMode>,
    ) {
        // The old path: its own backend, so a call it serves is told apart
        // from one the facade's drivers serve.
        let legacy = FakeBackend::new(SandboxBackend::Docker);
        let (facade, docker, _, _) = facade(SandboxBackend::Docker);
        let mode = Arc::new(FixedMode(Mutex::new(mode)));
        (
            ModeRoutingSandboxProvider::new(
                legacy.clone(),
                Arc::new(facade),
                SandboxBackend::Docker,
                mode.clone(),
            ),
            legacy,
            docker,
            mode,
        )
    }

    #[tokio::test]
    async fn legacy_and_shadow_serve_through_the_old_provider() {
        for mode in [SandboxDriverMode::Legacy, SandboxDriverMode::Shadow] {
            let (router, legacy, driver_backend, _) = router(mode);
            let mut cfg = config("abc");
            cfg.disk_size_mb = Some(1024);
            let handle = router.create(cfg).await.unwrap();
            assert!(
                handle.conditions.is_empty(),
                "{mode}: old path has no conditions"
            );
            router.is_alive(&handle).await.unwrap();
            assert_eq!(legacy.calls().len(), 2, "{mode}");
            assert!(driver_backend.calls().is_empty(), "{mode}");
        }
    }

    #[tokio::test]
    async fn enforce_serves_through_the_driver_facade() {
        let (router, legacy, driver_backend, _) = router(SandboxDriverMode::Enforce);
        let mut cfg = config("abc");
        cfg.disk_size_mb = Some(1024);
        let handle = router.create(cfg).await.unwrap();
        assert_eq!(
            handle.conditions.len(),
            1,
            "Docker's disk limit is reported"
        );
        router.is_alive(&handle).await.unwrap();
        assert!(legacy.calls().is_empty());
        assert_eq!(driver_backend.calls().len(), 2);
    }

    #[tokio::test]
    async fn switching_back_to_legacy_is_the_rollback() {
        let (router, legacy, driver_backend, mode) = router(SandboxDriverMode::Enforce);
        let handle = router.create(config("abc")).await.unwrap();
        if let Ok(mut m) = mode.0.lock() {
            *m = SandboxDriverMode::Legacy;
        }
        // The sandbox the facade created is served by the old path now.
        router.destroy(&handle, true).await.unwrap();
        assert_eq!(driver_backend.calls().len(), 1);
        assert_eq!(legacy.calls(), vec!["destroy temps-sandbox-abc true"]);
    }

    fn network_config_row() -> temps_entities::network_config::Model {
        temps_entities::network_config::Model {
            id: 1,
            compute_pool_cidr: "172.20.0.0/16".into(),
            subnet_prefix_len: 24,
            transport: "vxlan".into(),
            vxlan_vni: 4242,
            vxlan_port: 4789,
            underlay_mtu: 1500,
            control_plane_compute_cidr: None,
            control_plane_underlay_address: None,
            control_plane_overlay_ready: false,
            control_plane_setup_generation: 0,
            wireguard_enabled: false,
            wireguard_cidr: "10.201.0.0/16".into(),
            wireguard_port: 51820,
            control_plane_wg_public_key: None,
            control_plane_wg_endpoint: None,
            node_api_port: None,
            mesh_hub_node_id: None,
            mesh_hub_control_plane: false,
            control_plane_sandbox_driver_mode: SandboxDriverMode::Legacy,
            updated_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn the_control_plane_mode_is_read_from_network_config_and_cached() {
        let mut row = network_config_row();
        row.control_plane_sandbox_driver_mode = SandboxDriverMode::Enforce;
        // One result: a second read within the TTL must not query again.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![row]])
            .into_connection();
        let source = DbControlPlaneDriverMode::new(Arc::new(db));
        assert_eq!(source.mode().await, SandboxDriverMode::Enforce);
        assert_eq!(source.mode().await, SandboxDriverMode::Enforce);
        // Invalidation forgets the cached mode, so the next read fails (no
        // more mock results) with nothing known and falls back to legacy.
        source.invalidate();
        assert_eq!(source.mode().await, SandboxDriverMode::Legacy);
    }

    #[tokio::test]
    async fn a_failed_read_after_the_ttl_keeps_the_last_known_mode() {
        let mut row = network_config_row();
        row.control_plane_sandbox_driver_mode = SandboxDriverMode::Shadow;
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![row]])
            .into_connection();
        let mut source = DbControlPlaneDriverMode::new(Arc::new(db));
        source.ttl = Duration::ZERO;
        assert_eq!(source.mode().await, SandboxDriverMode::Shadow);
        // The TTL has expired and the second read errors: the last mode the
        // control plane saw stays in force instead of flipping to legacy.
        assert_eq!(source.mode().await, SandboxDriverMode::Shadow);
    }
}
