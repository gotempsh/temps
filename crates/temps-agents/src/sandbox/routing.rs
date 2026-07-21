//! Backend-routing sandbox provider (ADR-029 §2).
//!
//! Keeps ADR-010's invariant intact: consumers hold exactly one
//! `Arc<dyn SandboxProvider>`. This impl owns the concrete backends and
//! dispatches per call — `create` by the requested `SandboxBackend`,
//! handle-based methods by the typed `SandboxHandle.backend` the owning
//! provider stamped (no name parsing, no per-call DB lookup).
//!
//! Every trait method is overridden, including the ones with default
//! bodies — a default body running on the router would silently bypass a
//! backend's own override (e.g. Docker's `exec_as_root`).

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

use super::{
    KillSignal, OnStreamEventCallback, SandboxBackend, SandboxCreateConfig, SandboxExecResult,
    SandboxHandle, SandboxProvider,
};
use crate::ai_cli::OnEventCallback;
use crate::error::AgentError;

pub struct RoutingSandboxProvider {
    backends: HashMap<SandboxBackend, Arc<dyn SandboxProvider>>,
    default: SandboxBackend,
}

impl RoutingSandboxProvider {
    pub fn new(
        backends: HashMap<SandboxBackend, Arc<dyn SandboxProvider>>,
        default: SandboxBackend,
    ) -> Self {
        debug_assert!(backends.contains_key(&default));
        Self { backends, default }
    }

    pub fn default_backend(&self) -> SandboxBackend {
        self.default
    }

    pub fn backends(&self) -> impl Iterator<Item = (SandboxBackend, &Arc<dyn SandboxProvider>)> {
        self.backends.iter().map(|(b, p)| (*b, p))
    }

    fn get(&self, backend: SandboxBackend) -> Result<&Arc<dyn SandboxProvider>, AgentError> {
        self.backends
            .get(&backend)
            .ok_or_else(|| AgentError::SandboxCreationFailed {
                run_id: 0,
                provider: backend.to_string(),
                reason: format!(
                    "sandbox backend '{}' is not available on this host \
                     (available: {})",
                    backend,
                    self.backends
                        .keys()
                        .map(|b| b.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            })
    }

    /// Which backend owns an existing handle. Reads the typed `backend`
    /// stamped by the provider that created/recovered it — no name parsing.
    /// Falls back to the default if that backend isn't registered (e.g. a
    /// handle recovered for a backend later disabled).
    fn owner_of(&self, handle: &SandboxHandle) -> &Arc<dyn SandboxProvider> {
        self.backends
            .get(&handle.backend)
            .or_else(|| self.backends.get(&self.default))
            .expect("default backend registered")
    }

    /// Iteration order for recovery scans: default backend first, then the
    /// rest — deterministic so a name that could exist in two backends
    /// resolves stably.
    fn scan_order(&self) -> Vec<&Arc<dyn SandboxProvider>> {
        let mut order: Vec<(SandboxBackend, &Arc<dyn SandboxProvider>)> =
            self.backends.iter().map(|(b, p)| (*b, p)).collect();
        order.sort_by_key(|(b, _)| (*b != self.default, b.to_string()));
        order.into_iter().map(|(_, p)| p).collect()
    }
}

#[async_trait]
impl SandboxProvider for RoutingSandboxProvider {
    async fn create(&self, config: SandboxCreateConfig) -> Result<SandboxHandle, AgentError> {
        let backend = config.backend.unwrap_or(self.default);
        self.get(backend)?.create(config).await
    }

    async fn exec(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        self.owner_of(handle)
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
            .exec_streamed(handle, cmd, env, on_event)
            .await
    }

    async fn is_alive(&self, handle: &SandboxHandle) -> Result<bool, AgentError> {
        self.owner_of(handle).is_alive(handle).await
    }

    async fn write_file(
        &self,
        handle: &SandboxHandle,
        path: &str,
        contents: &[u8],
        mode: u32,
    ) -> Result<(), AgentError> {
        self.owner_of(handle)
            .write_file(handle, path, contents, mode)
            .await
    }

    async fn read_file(&self, handle: &SandboxHandle, path: &str) -> Result<Vec<u8>, AgentError> {
        self.owner_of(handle).read_file(handle, path).await
    }

    async fn write_directory(
        &self,
        handle: &SandboxHandle,
        local_dir: &std::path::Path,
        target_path: &str,
    ) -> Result<(), AgentError> {
        self.owner_of(handle)
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
            .kill_processes(handle, pattern, signal)
            .await
    }

    async fn destroy(&self, handle: &SandboxHandle, purge_volumes: bool) -> Result<(), AgentError> {
        self.owner_of(handle).destroy(handle, purge_volumes).await
    }

    async fn stop(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        self.owner_of(handle).stop(handle).await
    }

    async fn start(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        self.owner_of(handle).start(handle).await
    }

    async fn restart(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        self.owner_of(handle).restart(handle).await
    }

    async fn resize_disk(
        &self,
        handle: &SandboxHandle,
        new_size_mb: u64,
    ) -> Result<(), AgentError> {
        self.owner_of(handle).resize_disk(handle, new_size_mb).await
    }

    async fn recover(&self, run_id: i32) -> Result<Option<SandboxHandle>, AgentError> {
        for provider in self.scan_order() {
            if let Some(handle) = provider.recover(run_id).await? {
                return Ok(Some(handle));
            }
        }
        Ok(None)
    }

    async fn recover_by_name(
        &self,
        container_name: &str,
    ) -> Result<Option<SandboxHandle>, AgentError> {
        for provider in self.scan_order() {
            if let Some(handle) = provider.recover_by_name(container_name).await? {
                return Ok(Some(handle));
            }
        }
        Ok(None)
    }

    fn name(&self) -> &str {
        "routing"
    }

    async fn is_available(&self) -> bool {
        for provider in self.backends.values() {
            if provider.is_available().await {
                return true;
            }
        }
        false
    }

    async fn image_status(&self) -> Result<(bool, String), AgentError> {
        self.get(self.default)?.image_status().await
    }

    async fn rebuild_image(&self) -> Result<String, AgentError> {
        self.get(self.default)?.rebuild_image().await
    }

    async fn rebuild_image_with_progress(
        &self,
        on_progress: tokio::sync::mpsc::Sender<String>,
    ) -> Result<String, AgentError> {
        self.get(self.default)?
            .rebuild_image_with_progress(on_progress)
            .await
    }

    async fn rootfs_report(&self) -> Result<super::RootfsReport, AgentError> {
        // Merge every backend's report; only Firecracker returns non-empty.
        let mut merged = super::RootfsReport::default();
        for provider in self.backends.values() {
            let r = provider.rootfs_report().await?;
            merged.cache_bytes += r.cache_bytes;
            merged.vm_bytes += r.vm_bytes;
            merged.cache.extend(r.cache);
            merged.vms.extend(r.vms);
        }
        Ok(merged)
    }

    async fn gc_rootfs(&self) -> Result<super::RootfsGcReport, AgentError> {
        let mut merged = super::RootfsGcReport::default();
        for provider in self.backends.values() {
            let r = provider.gc_rootfs().await?;
            merged.freed_bytes += r.freed_bytes;
            merged.removed_digests.extend(r.removed_digests);
        }
        Ok(merged)
    }
}
