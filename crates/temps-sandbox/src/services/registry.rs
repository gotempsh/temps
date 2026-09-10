// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! In-memory map from sandbox internal ID → `SandboxHandle`. Keeps live
//! container handles for the lifetime of the server process.
//!
//! Separate from `temps-agents::SandboxRegistry` because (a) we own
//! lifecycle differently (standalone sandboxes don't tie to agent runs)
//! and (b) `release` here marks the DB row "destroyed" rather than
//! deleting per-run data.
//!
//! # Post-restart behavior
//!
//! Every lifecycle method (`start`, `stop`, `restart`, `destroy`) MUST
//! locate the underlying container even when the in-memory handle map is
//! empty — which is the normal state after any server restart. The
//! registry does this by falling back to `recover_by_name` using the
//! sandbox's `public_id`. If neither the map nor recovery finds the
//! container, the method returns `SandboxNotFound` and does nothing.
//!
//! Silent no-ops (early-return `Ok(())` when the handle is missing) are
//! forbidden — they cause the DB row to drift from provider reality (e.g.
//! "DB says running, container is actually stopped or gone"), which then
//! makes the next `exec` call fail with NotFound and the user confused.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};

use temps_agents::error::AgentError;
use temps_agents::sandbox::{
    user::SANDBOX_CHOWN, SandboxCreateConfig, SandboxHandle, SandboxProvider,
};

pub struct StandaloneSandboxRegistry {
    provider: Arc<dyn SandboxProvider>,
    handles: RwLock<HashMap<i32, SandboxHandle>>,
    /// Sandboxes whose live provider handle was adopted in this server
    /// generation. Their previous Temps process may have died while a harness
    /// exec was active, so the first managed workspace operation must fence
    /// those provider CLIs before reusing the container.
    recovered_in_this_generation: RwLock<HashSet<i32>>,
    /// Serialize the first verified fence per recovered sandbox. The
    /// generation marker remains set while the future is in flight, making
    /// cancellation fail closed; concurrent callers wait here and re-check it.
    recovery_fence_locks: Mutex<HashMap<i32, Arc<Mutex<()>>>>,
}

const STARTUP_RECOVERY_TIMEOUT: Duration = Duration::from_secs(5);

impl StandaloneSandboxRegistry {
    pub fn new(provider: Arc<dyn SandboxProvider>) -> Self {
        Self {
            provider,
            handles: RwLock::new(HashMap::new()),
            recovered_in_this_generation: RwLock::new(HashSet::new()),
            recovery_fence_locks: Mutex::new(HashMap::new()),
        }
    }

    pub fn provider(&self) -> &dyn SandboxProvider {
        self.provider.as_ref()
    }

    pub fn provider_arc(&self) -> Arc<dyn SandboxProvider> {
        self.provider.clone()
    }

    /// Create and register a new sandbox for the given internal ID.
    pub async fn create(&self, config: SandboxCreateConfig) -> Result<SandboxHandle, AgentError> {
        let id = config.run_id;
        let handle = self.provider.create(config).await?;
        self.handles.write().await.insert(id, handle.clone());
        self.recovered_in_this_generation.write().await.remove(&id);
        Ok(handle)
    }

    /// Create a sandbox from a snapshot artifact (ADR-037). Registers the
    /// handle identically to `create` — the difference is only at the
    /// provider level (image is loaded from the tarball if not present).
    pub async fn create_from_snapshot(
        &self,
        artifact: &temps_agents::sandbox::SnapshotArtifact,
        config: SandboxCreateConfig,
    ) -> Result<SandboxHandle, AgentError> {
        let id = config.run_id;
        let handle = self.provider.create_from_snapshot(artifact, config).await?;
        self.handles.write().await.insert(id, handle.clone());
        self.recovered_in_this_generation.write().await.remove(&id);
        Ok(handle)
    }

    /// Reconcile the isolated data-plane network for an application workspace.
    pub async fn configure_application_network(
        &self,
        id: i32,
        public_id: &str,
        network_name: &str,
        service_containers: &[String],
    ) -> Result<(), AgentError> {
        let handle = self.get_or_recover(id, public_id).await?;
        self.provider
            .configure_application_network(&handle, network_name, service_containers)
            .await
    }

    /// Replace compute while retaining provider-managed persistent volumes.
    /// The caller owns the durable host workspace and database state.
    pub async fn rebuild(
        &self,
        id: i32,
        public_id: &str,
        config: SandboxCreateConfig,
    ) -> Result<SandboxHandle, AgentError> {
        if let Ok(handle) = self.get_or_recover(id, public_id).await {
            self.provider.destroy(&handle, false).await?;
        }
        self.handles.write().await.remove(&id);
        let handle = self.provider.create(config).await?;
        self.handles.write().await.insert(id, handle.clone());
        self.recovered_in_this_generation.write().await.remove(&id);
        Ok(handle)
    }

    pub async fn restore(
        &self,
        id: i32,
        public_id: &str,
        artifact: &temps_agents::sandbox::SnapshotArtifact,
        config: SandboxCreateConfig,
    ) -> Result<SandboxHandle, AgentError> {
        if let Ok(handle) = self.get_or_recover(id, public_id).await {
            self.provider.destroy(&handle, false).await?;
        }
        self.handles.write().await.remove(&id);
        let handle = self.provider.create_from_snapshot(artifact, config).await?;
        self.handles.write().await.insert(id, handle.clone());
        self.recovered_in_this_generation.write().await.remove(&id);
        Ok(handle)
    }

    pub async fn normalize_workspace_path(
        &self,
        id: i32,
        public_id: &str,
        container_path: &str,
    ) -> Result<(), AgentError> {
        let handle = self.get(id, public_id).await?;
        let result = self
            .provider
            .exec_as_root(
                &handle,
                vec![
                    "chown".to_string(),
                    "-R".to_string(),
                    SANDBOX_CHOWN.to_string(),
                    container_path.to_string(),
                ],
                HashMap::new(),
                None,
            )
            .await?;
        if result.exit_code != 0 {
            return Err(AgentError::SandboxExecFailed {
                run_id: id,
                sandbox_id: public_id.to_string(),
                reason: format!(
                    "normalize workspace path ownership failed with exit {}: {}",
                    result.exit_code,
                    result.stderr.trim()
                ),
            });
        }
        Ok(())
    }

    /// Strip the `sbx_` prefix from a public ID to get the container
    /// label the provider indexes by. Docker-side container names are
    /// `temps-sandbox-<hex>` where `<hex>` is the label returned here.
    fn label_for(public_id: &str) -> &str {
        public_id.strip_prefix("sbx_").unwrap_or(public_id)
    }

    /// Core handle resolution used by every lifecycle op. Checks the
    /// in-memory map first; if that misses, asks the provider to recover
    /// the handle by container name (i.e. by `public_id`). Either way the
    /// handle is re-inserted into the map on success so subsequent calls
    /// are fast.
    ///
    /// Returns `SandboxNotFound` only when the container genuinely isn't
    /// known to the provider (inspect returned `None`). Importantly, this
    /// does NOT reject stopped containers — `recover_container` on the
    /// provider side returns handles for stopped sandboxes too, so lifecycle
    /// ops like `start` can act on them. Callers that need the container
    /// to be running (e.g. `exec`) must check separately.
    async fn get_or_recover(&self, id: i32, public_id: &str) -> Result<SandboxHandle, AgentError> {
        if let Some(h) = self.handles.read().await.get(&id).cloned() {
            return Ok(h);
        }
        let label = Self::label_for(public_id);
        match self.provider.recover_by_name(label).await? {
            Some(recovered) => {
                self.handles.write().await.insert(id, recovered.clone());
                self.recovered_in_this_generation.write().await.insert(id);
                Ok(recovered)
            }
            None => Err(AgentError::SandboxNotFound { run_id: id }),
        }
    }

    /// Look up an existing handle for use by exec/read/write. Verifies
    /// liveness — returns `SandboxNotFound` if the container exists but
    /// is stopped, because non-lifecycle operations need a running
    /// sandbox. Lifecycle operations (`start`/`stop`/`restart`/`destroy`)
    /// use `get_or_recover` directly instead.
    ///
    /// `public_id` is the full `sbx_<hex>` identifier. The registry
    /// derives the container label from it so recovery after a server
    /// restart finds the container by name.
    pub async fn get(&self, id: i32, public_id: &str) -> Result<SandboxHandle, AgentError> {
        let handle = self.get_or_recover(id, public_id).await?;
        if !self.provider.is_alive(&handle).await.unwrap_or(false) {
            return Err(AgentError::SandboxNotFound { run_id: id });
        }
        Ok(handle)
    }

    /// Remove the handle from the registry and destroy the underlying
    /// container + volumes. Uses `get_or_recover` so post-restart destroy
    /// still reaches the container (pre-fix this was a silent no-op after
    /// a restart and leaked the container + volumes on every re-deploy).
    /// Returning `Ok(())` on already-gone containers keeps the call
    /// idempotent for callers driving cleanup.
    pub async fn destroy(&self, id: i32, public_id: &str) -> Result<(), AgentError> {
        let handle = match self.get_or_recover(id, public_id).await {
            Ok(h) => h,
            Err(AgentError::SandboxNotFound { .. }) => {
                // Nothing to remove — treat as already destroyed. The
                // caller will still transition the DB row to "destroyed".
                self.handles.write().await.remove(&id);
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        if let Err(e) = self.provider.destroy(&handle, true).await {
            tracing::warn!(
                "Failed to destroy standalone sandbox {} (internal id {}): {}",
                handle.sandbox_name,
                id,
                e
            );
            return Err(e);
        }
        self.handles.write().await.remove(&id);
        self.recovered_in_this_generation.write().await.remove(&id);
        Ok(())
    }

    /// Stop the container without destroying it. For lifecycle transitions
    /// that pause a sandbox. Callers own the DB state.
    ///
    /// Falls back to `recover_by_name` when the in-memory handle is gone
    /// (e.g. after a server restart). Returns `SandboxNotFound` if no
    /// container exists — the caller must NOT transition the DB row to
    /// "stopped" in that case or the record will lie about what's running.
    pub async fn stop(&self, id: i32, public_id: &str) -> Result<(), AgentError> {
        let handle = self.get_or_recover(id, public_id).await?;
        self.provider.stop(&handle).await
    }

    /// Start a previously-stopped container without re-creating it. Same
    /// recovery behavior as `stop`: the in-memory miss is expected after
    /// a server restart, and we must actually reach the provider — a
    /// silent no-op here used to flip the DB to "running" while the
    /// container stayed stopped, which made the next exec fail with a
    /// baffling NotFound.
    pub async fn start(&self, id: i32, public_id: &str) -> Result<(), AgentError> {
        let handle = self.get_or_recover(id, public_id).await?;
        self.provider.start(&handle).await
    }

    /// Restart a container (stop + start) preserving its filesystem.
    /// Same post-restart recovery as `start`/`stop`.
    pub async fn restart(&self, id: i32, public_id: &str) -> Result<(), AgentError> {
        let handle = self.get_or_recover(id, public_id).await?;
        self.provider.restart(&handle).await
    }

    /// Grow the sandbox's root disk (Firecracker only). Same recovery.
    pub async fn resize_disk(
        &self,
        id: i32,
        public_id: &str,
        new_size_mb: u64,
    ) -> Result<(), AgentError> {
        let handle = self.get_or_recover(id, public_id).await?;
        self.provider.resize_disk(&handle, new_size_mb).await
    }

    /// Recover handles for sandboxes that were running when the server
    /// last stopped. Called on startup. Unlike `StandaloneSandboxRegistry::get`,
    /// this doesn't error on missing containers — it just skips them and
    /// returns the count of successfully recovered handles so the plugin
    /// can log a summary.
    ///
    /// `entries` pairs each sandbox's internal numeric id with its
    /// container label (hex suffix of `public_id`). The provider looks
    /// up by label; the registry keys by numeric id for in-memory lookup.
    pub async fn recover_active(&self, entries: &[(i32, String)]) -> usize {
        self.recover_active_with_timeout(entries, STARTUP_RECOVERY_TIMEOUT)
            .await
    }

    async fn recover_active_with_timeout(
        &self,
        entries: &[(i32, String)],
        timeout: Duration,
    ) -> usize {
        let mut recovered = 0;
        for (id, label) in entries {
            match tokio::time::timeout(timeout, self.provider.recover_by_name(label)).await {
                Ok(Ok(Some(handle))) => {
                    self.handles.write().await.insert(*id, handle);
                    self.recovered_in_this_generation.write().await.insert(*id);
                    recovered += 1;
                }
                Ok(Ok(None)) => {}
                Ok(Err(e)) => {
                    tracing::warn!(
                        "Failed to recover standalone sandbox for id {} ({}): {}",
                        id,
                        label,
                        e
                    );
                }
                Err(_) => {
                    tracing::warn!(
                        "Timed out after {:?} recovering standalone sandbox for id {} ({}) — \
                         startup will continue and lifecycle operations will retry recovery lazily",
                        timeout,
                        id,
                        label
                    );
                }
            }
        }
        recovered
    }

    /// Fence provider CLI processes that may have outlived the previous Temps
    /// server process. This is deliberately narrower than restarting the
    /// container: user dev servers, terminals, and other background work keep
    /// running, while only the three command shapes launched by the managed
    /// harness runtime are terminated before a new turn or model probe starts.
    ///
    /// A per-sandbox lock makes concurrent callers await the same fence. The
    /// generation marker is removed only after the provider verifies that the
    /// complete matching process trees are gone. Provider failure, timeout,
    /// or future cancellation leaves the marker intact so the next managed
    /// operation retries and fails closed until fencing succeeds.
    pub async fn fence_recovered_harness_processes(
        &self,
        id: i32,
        public_id: &str,
    ) -> Result<(), AgentError> {
        let fence_lock = {
            let mut locks = self.recovery_fence_locks.lock().await;
            locks
                .entry(id)
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _fence_guard = fence_lock.lock().await;

        if !self.recovered_in_this_generation.read().await.contains(&id) {
            return Ok(());
        }
        let handle = self.get_or_recover(id, public_id).await?;
        self.provider
            .fence_process_trees(
                &handle,
                &[
                    r"(^|/)claude --print( |$)",
                    r"(^|/)codex exec( |$)",
                    r"(^|/)opencode run( |$)",
                ],
            )
            .await?;
        self.recovered_in_this_generation.write().await.remove(&id);
        Ok(())
    }

    pub fn provider_name(&self) -> &str {
        self.provider.name()
    }
}

#[cfg(test)]
mod tests {
    //! These tests pin down the "lifecycle ops survive a server restart"
    //! invariant. The earlier version of the registry silently returned
    //! `Ok(())` when the in-memory handle map missed, which made
    //! `resume_sandbox` flip the DB row to "running" while never actually
    //! calling `docker start` on the container. The next exec then got a
    //! baffling NotFound. If any of these tests start failing, we've
    //! regressed on that invariant — please fix the registry, not the
    //! test.
    use super::*;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use temps_agents::ai_cli::OnEventCallback;
    use temps_agents::sandbox::{SandboxExecResult, SandboxHandle};

    /// Fake provider that records how many times each lifecycle method
    /// was called, and whether `recover_by_name` succeeded for a given
    /// label. Implements only what the registry touches — the full
    /// trait has many more methods but they're either never called by
    /// the registry or have default impls in the trait.
    struct FakeProvider {
        /// Containers the provider "knows about" — maps label to sandbox_id.
        /// Configure per-test via `with_known`.
        known: HashMap<String, String>,
        starts: AtomicUsize,
        stops: AtomicUsize,
        restarts: AtomicUsize,
        destroys: AtomicUsize,
        fence_patterns: std::sync::Mutex<Vec<Vec<String>>>,
        fence_attempts: AtomicUsize,
        fence_failures_remaining: AtomicUsize,
        fence_gate: Option<Arc<tokio::sync::Semaphore>>,
        recovery_delay: Duration,
    }

    impl FakeProvider {
        fn new() -> Self {
            Self {
                known: HashMap::new(),
                starts: AtomicUsize::new(0),
                stops: AtomicUsize::new(0),
                restarts: AtomicUsize::new(0),
                destroys: AtomicUsize::new(0),
                fence_patterns: std::sync::Mutex::new(Vec::new()),
                fence_attempts: AtomicUsize::new(0),
                fence_failures_remaining: AtomicUsize::new(0),
                fence_gate: None,
                recovery_delay: Duration::ZERO,
            }
        }

        fn with_known(mut self, label: &str) -> Self {
            self.known
                .insert(label.to_string(), format!("docker-id-{}", label));
            self
        }

        fn with_recovery_delay(mut self, delay: Duration) -> Self {
            self.recovery_delay = delay;
            self
        }

        fn with_fence_failures(self, failures: usize) -> Self {
            self.fence_failures_remaining
                .store(failures, Ordering::SeqCst);
            self
        }

        fn with_fence_gate(mut self, gate: Arc<tokio::sync::Semaphore>) -> Self {
            self.fence_gate = Some(gate);
            self
        }
    }

    #[async_trait]
    impl SandboxProvider for FakeProvider {
        async fn create(&self, config: SandboxCreateConfig) -> Result<SandboxHandle, AgentError> {
            Ok(SandboxHandle {
                sandbox_id: format!("docker-id-{}", config.run_id),
                sandbox_name: format!("temps-sandbox-{}", config.run_id),
                work_dir: PathBuf::from("/workspace"),
                backend: temps_agents::sandbox::SandboxBackend::Docker,
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
            Ok(SandboxExecResult {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }

        async fn is_alive(&self, _handle: &SandboxHandle) -> Result<bool, AgentError> {
            Ok(true)
        }

        async fn write_file(
            &self,
            _handle: &SandboxHandle,
            _path: &str,
            _contents: &[u8],
            _mode: u32,
        ) -> Result<(), AgentError> {
            Ok(())
        }

        async fn read_file(
            &self,
            _handle: &SandboxHandle,
            _path: &str,
        ) -> Result<Vec<u8>, AgentError> {
            Ok(Vec::new())
        }

        async fn write_directory(
            &self,
            _handle: &SandboxHandle,
            _local_dir: &std::path::Path,
            _target_path: &str,
        ) -> Result<(), AgentError> {
            Ok(())
        }

        async fn kill_processes(
            &self,
            _handle: &SandboxHandle,
            pattern: &str,
            _signal: temps_agents::sandbox::KillSignal,
        ) -> Result<(), AgentError> {
            let _ = pattern;
            Ok(())
        }

        async fn fence_process_trees(
            &self,
            _handle: &SandboxHandle,
            patterns: &[&str],
        ) -> Result<(), AgentError> {
            self.fence_attempts.fetch_add(1, Ordering::SeqCst);
            self.fence_patterns
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(
                    patterns
                        .iter()
                        .map(|pattern| (*pattern).to_string())
                        .collect(),
                );
            if let Some(gate) = &self.fence_gate {
                gate.acquire()
                    .await
                    .expect("test fence gate remains open")
                    .forget();
            }
            if self
                .fence_failures_remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                return Err(AgentError::SandboxExecFailed {
                    run_id: 42,
                    sandbox_id: "sbx_abc123".to_string(),
                    reason: "simulated strict fence failure".to_string(),
                });
            }
            Ok(())
        }

        async fn destroy(
            &self,
            _handle: &SandboxHandle,
            _purge_volumes: bool,
        ) -> Result<(), AgentError> {
            self.destroys.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn stop(&self, _handle: &SandboxHandle) -> Result<(), AgentError> {
            self.stops.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn start(&self, _handle: &SandboxHandle) -> Result<(), AgentError> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn restart(&self, _handle: &SandboxHandle) -> Result<(), AgentError> {
            self.restarts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn recover(&self, _run_id: i32) -> Result<Option<SandboxHandle>, AgentError> {
            Ok(None)
        }

        async fn recover_by_name(
            &self,
            container_name: &str,
        ) -> Result<Option<SandboxHandle>, AgentError> {
            if !self.recovery_delay.is_zero() {
                tokio::time::sleep(self.recovery_delay).await;
            }
            Ok(self.known.get(container_name).map(|id| SandboxHandle {
                sandbox_id: id.clone(),
                sandbox_name: format!("temps-sandbox-{}", container_name),
                work_dir: PathBuf::from("/workspace"),
                backend: temps_agents::sandbox::SandboxBackend::Docker,
                image: String::new(),
            }))
        }

        fn name(&self) -> &str {
            "fake"
        }

        async fn is_available(&self) -> bool {
            true
        }

        async fn image_status(&self) -> Result<(bool, String), AgentError> {
            Ok((true, "fake:latest".to_string()))
        }

        async fn rebuild_image(&self) -> Result<String, AgentError> {
            Ok("fake:latest".to_string())
        }
    }

    /// Simulate the exact post-restart condition: the container exists in
    /// Docker but the registry's in-memory handle map is empty. `start`
    /// must reach the provider via `recover_by_name` — anything else and
    /// the sandbox's DB row drifts to "running" while the container stays
    /// stopped.
    #[tokio::test]
    async fn start_after_restart_reaches_provider_via_recovery() {
        let provider = Arc::new(FakeProvider::new().with_known("abc123"));
        let reg = StandaloneSandboxRegistry::new(provider.clone());

        // Map is empty — simulates fresh process after server restart.
        reg.start(42, "sbx_abc123").await.expect("start succeeds");

        assert_eq!(
            provider.starts.load(Ordering::SeqCst),
            1,
            "start must reach the provider; silent no-ops caused \
             DB/container drift in the previous version"
        );
    }

    /// Same invariant for `stop` — the expiration sweeper relies on this
    /// when the server restarted between create and expiry.
    #[tokio::test]
    async fn stop_after_restart_reaches_provider_via_recovery() {
        let provider = Arc::new(FakeProvider::new().with_known("abc123"));
        let reg = StandaloneSandboxRegistry::new(provider.clone());

        reg.stop(42, "sbx_abc123").await.expect("stop succeeds");

        assert_eq!(provider.stops.load(Ordering::SeqCst), 1);
    }

    /// Same invariant for `restart`.
    #[tokio::test]
    async fn restart_after_restart_reaches_provider_via_recovery() {
        let provider = Arc::new(FakeProvider::new().with_known("abc123"));
        let reg = StandaloneSandboxRegistry::new(provider.clone());

        reg.restart(42, "sbx_abc123")
            .await
            .expect("restart succeeds");

        assert_eq!(provider.restarts.load(Ordering::SeqCst), 1);
    }

    /// Same invariant for `destroy`. Pre-fix, destroy after restart left
    /// the container + volumes leaking on the host while the DB row
    /// claimed "destroyed".
    #[tokio::test]
    async fn destroy_after_restart_reaches_provider_via_recovery() {
        let provider = Arc::new(FakeProvider::new().with_known("abc123"));
        let reg = StandaloneSandboxRegistry::new(provider.clone());

        reg.destroy(42, "sbx_abc123")
            .await
            .expect("destroy succeeds");

        assert_eq!(provider.destroys.load(Ordering::SeqCst), 1);
    }

    /// When the container is genuinely gone (no map entry, no provider
    /// recovery match), lifecycle ops must return `SandboxNotFound`
    /// rather than silently succeed. This is what lets callers avoid
    /// flipping the DB row to a state that doesn't match reality.
    #[tokio::test]
    async fn start_returns_not_found_when_container_truly_gone() {
        let provider = Arc::new(FakeProvider::new()); // No known containers.
        let reg = StandaloneSandboxRegistry::new(provider.clone());

        let err = reg.start(42, "sbx_abc123").await.expect_err(
            "start must not silently succeed when the \
                         container is gone — that was the original bug",
        );
        assert!(matches!(err, AgentError::SandboxNotFound { run_id: 42 }));
        assert_eq!(provider.starts.load(Ordering::SeqCst), 0);
    }

    /// `destroy` is the exception to the NotFound rule — treating an
    /// already-gone container as "already destroyed" makes the caller's
    /// cleanup idempotent. The DB row transition to "destroyed" still
    /// happens in the service layer.
    #[tokio::test]
    async fn destroy_is_idempotent_when_container_already_gone() {
        let provider = Arc::new(FakeProvider::new());
        let reg = StandaloneSandboxRegistry::new(provider.clone());

        reg.destroy(42, "sbx_abc123")
            .await
            .expect("destroy is idempotent");

        assert_eq!(
            provider.destroys.load(Ordering::SeqCst),
            0,
            "nothing to destroy at the provider layer"
        );
    }

    /// Warm-path check: when the handle IS in the map, lifecycle ops use
    /// it directly without touching `recover_by_name`. The fake
    /// provider's `known` map is empty here — if the registry fell back
    /// to recovery anyway, start would return NotFound.
    #[tokio::test]
    async fn start_uses_in_memory_handle_when_present() {
        let provider = Arc::new(FakeProvider::new()); // Empty — recovery would fail.
        let reg = StandaloneSandboxRegistry::new(provider.clone());

        // Seed the handle the way `create()` would.
        reg.handles.write().await.insert(
            42,
            SandboxHandle {
                sandbox_id: "docker-id-42".to_string(),
                sandbox_name: "temps-sandbox-abc123".to_string(),
                work_dir: PathBuf::from("/workspace"),
                backend: temps_agents::sandbox::SandboxBackend::Docker,
                image: String::new(),
            },
        );

        reg.start(42, "sbx_abc123").await.expect("start succeeds");

        assert_eq!(provider.starts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn startup_recovery_timeout_does_not_block_the_control_plane() {
        let provider = Arc::new(
            FakeProvider::new()
                .with_known("slow")
                .with_recovery_delay(Duration::from_secs(1)),
        );
        let reg = StandaloneSandboxRegistry::new(provider);
        let started = tokio::time::Instant::now();

        let recovered = reg
            .recover_active_with_timeout(&[(42, "slow".to_string())], Duration::from_millis(20))
            .await;

        assert_eq!(recovered, 0);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "sandbox recovery must not hold control-plane startup indefinitely"
        );
    }

    #[tokio::test]
    async fn first_workspace_use_fences_only_recovered_harness_processes_once() {
        let provider = Arc::new(FakeProvider::new().with_known("abc123"));
        let reg = StandaloneSandboxRegistry::new(provider.clone());

        assert_eq!(reg.recover_active(&[(42, "abc123".to_string())]).await, 1);
        reg.fence_recovered_harness_processes(42, "sbx_abc123")
            .await
            .expect("first-generation fence");
        reg.fence_recovered_harness_processes(42, "sbx_abc123")
            .await
            .expect("already-fenced workspace");

        assert_eq!(provider.restarts.load(Ordering::SeqCst), 0);
        assert_eq!(
            provider
                .fence_patterns
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_slice(),
            [[
                r"(^|/)claude --print( |$)",
                r"(^|/)codex exec( |$)",
                r"(^|/)opencode run( |$)",
            ]]
        );
    }

    #[tokio::test]
    async fn failed_recovery_fence_remains_required_until_verified() {
        let provider = Arc::new(
            FakeProvider::new()
                .with_known("abc123")
                .with_fence_failures(1),
        );
        let reg = StandaloneSandboxRegistry::new(provider.clone());

        assert_eq!(reg.recover_active(&[(42, "abc123".to_string())]).await, 1);
        reg.fence_recovered_harness_processes(42, "sbx_abc123")
            .await
            .expect_err("an unverified fence must fail closed");
        reg.fence_recovered_harness_processes(42, "sbx_abc123")
            .await
            .expect("the next use retries the fence");
        reg.fence_recovered_harness_processes(42, "sbx_abc123")
            .await
            .expect("verified fence is not repeated");

        assert_eq!(provider.fence_attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn cancelled_recovery_fence_remains_required() {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let provider = Arc::new(
            FakeProvider::new()
                .with_known("abc123")
                .with_fence_gate(gate.clone()),
        );
        let reg = Arc::new(StandaloneSandboxRegistry::new(provider.clone()));

        assert_eq!(reg.recover_active(&[(42, "abc123".to_string())]).await, 1);
        let cancelled = {
            let reg = reg.clone();
            tokio::spawn(async move {
                reg.fence_recovered_harness_processes(42, "sbx_abc123")
                    .await
            })
        };
        while provider.fence_attempts.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        cancelled.abort();
        let _ = cancelled.await;

        gate.add_permits(1);
        reg.fence_recovered_harness_processes(42, "sbx_abc123")
            .await
            .expect("cancellation must leave the fence pending");
        assert_eq!(provider.fence_attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn concurrent_first_use_shares_one_verified_fence() {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let provider = Arc::new(
            FakeProvider::new()
                .with_known("abc123")
                .with_fence_gate(gate.clone()),
        );
        let reg = Arc::new(StandaloneSandboxRegistry::new(provider.clone()));

        assert_eq!(reg.recover_active(&[(42, "abc123".to_string())]).await, 1);
        let first = {
            let reg = reg.clone();
            tokio::spawn(async move {
                reg.fence_recovered_harness_processes(42, "sbx_abc123")
                    .await
            })
        };
        let second = {
            let reg = reg.clone();
            tokio::spawn(async move {
                reg.fence_recovered_harness_processes(42, "sbx_abc123")
                    .await
            })
        };
        while provider.fence_attempts.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(provider.fence_attempts.load(Ordering::SeqCst), 1);

        gate.add_permits(1);
        first.await.expect("first fence task").expect("first fence");
        second
            .await
            .expect("second fence task")
            .expect("shared fence result");
        assert_eq!(provider.fence_attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn newly_created_workspace_does_not_run_restart_fencing() {
        let provider = Arc::new(FakeProvider::new());
        let reg = StandaloneSandboxRegistry::new(provider.clone());
        reg.create(SandboxCreateConfig {
            run_id: 42,
            container_name_override: Some("abc123".to_string()),
            host_work_dir: PathBuf::from("/workspace"),
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
            owner_user_id: Some(7),
        })
        .await
        .expect("new workspace");

        reg.fence_recovered_harness_processes(42, "sbx_abc123")
            .await
            .expect("new workspace needs no fence");

        assert!(provider
            .fence_patterns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty());
    }

    #[test]
    fn label_for_strips_sbx_prefix() {
        assert_eq!(StandaloneSandboxRegistry::label_for("sbx_abc123"), "abc123");
    }

    #[test]
    fn label_for_is_identity_when_no_prefix() {
        // Defensive: if the prefix convention ever changes, `label_for`
        // won't silently mangle the ID. It just passes it through so the
        // provider gets a real lookup key to report back on.
        assert_eq!(StandaloneSandboxRegistry::label_for("abc123"), "abc123");
    }
}
