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

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use temps_agents::error::AgentError;
use temps_agents::sandbox::{SandboxCreateConfig, SandboxHandle, SandboxProvider};

pub struct StandaloneSandboxRegistry {
    provider: Arc<dyn SandboxProvider>,
    handles: RwLock<HashMap<i32, SandboxHandle>>,
}

impl StandaloneSandboxRegistry {
    pub fn new(provider: Arc<dyn SandboxProvider>) -> Self {
        Self {
            provider,
            handles: RwLock::new(HashMap::new()),
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
        Ok(handle)
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
        let mut recovered = 0;
        for (id, label) in entries {
            match self.provider.recover_by_name(label).await {
                Ok(Some(handle)) => {
                    self.handles.write().await.insert(*id, handle);
                    recovered += 1;
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(
                        "Failed to recover standalone sandbox for id {} ({}): {}",
                        id,
                        label,
                        e
                    );
                }
            }
        }
        recovered
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
    }

    impl FakeProvider {
        fn new() -> Self {
            Self {
                known: HashMap::new(),
                starts: AtomicUsize::new(0),
                stops: AtomicUsize::new(0),
                restarts: AtomicUsize::new(0),
                destroys: AtomicUsize::new(0),
            }
        }

        fn with_known(mut self, label: &str) -> Self {
            self.known
                .insert(label.to_string(), format!("docker-id-{}", label));
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
            _pattern: &str,
            _signal: temps_agents::sandbox::KillSignal,
        ) -> Result<(), AgentError> {
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
