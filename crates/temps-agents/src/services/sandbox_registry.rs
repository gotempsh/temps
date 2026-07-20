use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::ai_cli::OnEventCallback;
use crate::error::AgentError;
use crate::sandbox::{SandboxCreateConfig, SandboxExecResult, SandboxHandle, SandboxProvider};

/// Registry that maps run IDs to active sandbox handles. This is critical for
/// the autofixer's multi-phase workflow where a container must persist across
/// analysis → fix → PR creation phases.
pub struct SandboxRegistry {
    provider: Arc<dyn SandboxProvider>,
    sandboxes: RwLock<HashMap<i32, SandboxHandle>>,
}

impl SandboxRegistry {
    pub fn new(provider: Arc<dyn SandboxProvider>) -> Self {
        Self {
            provider,
            sandboxes: RwLock::new(HashMap::new()),
        }
    }

    /// Get a reference to the underlying sandbox provider.
    pub fn provider(&self) -> &dyn SandboxProvider {
        self.provider.as_ref()
    }

    /// Get a cloneable Arc to the underlying sandbox provider.
    /// Needed when the provider must outlive a borrow (e.g. spawned tasks).
    pub fn provider_arc(&self) -> Arc<dyn SandboxProvider> {
        self.provider.clone()
    }

    /// Check if a sandbox exists for this run.
    pub async fn has_sandbox(&self, run_id: i32) -> bool {
        self.sandboxes.read().await.contains_key(&run_id)
    }

    /// Get an existing sandbox or create a new one for the given run.
    pub async fn get_or_create(
        &self,
        config: SandboxCreateConfig,
    ) -> Result<SandboxHandle, AgentError> {
        let run_id = config.run_id;

        // Check if we already have a sandbox for this run
        {
            let sandboxes = self.sandboxes.read().await;
            if let Some(handle) = sandboxes.get(&run_id) {
                // Verify it's still alive
                if self.provider.is_alive(handle).await.unwrap_or(false) {
                    return Ok(handle.clone());
                }
                // Dead sandbox — will recreate below
            }
        }

        // Try to recover from a server restart before creating new
        if let Some(recovered) = self.provider.recover(run_id).await? {
            tracing::info!(
                "Recovered sandbox {} for run {} from {}",
                recovered.sandbox_name,
                run_id,
                self.provider.name()
            );
            let mut sandboxes = self.sandboxes.write().await;
            sandboxes.insert(run_id, recovered.clone());
            return Ok(recovered);
        }

        // Create a new sandbox
        let handle = self.provider.create(config).await?;
        let mut sandboxes = self.sandboxes.write().await;
        sandboxes.insert(run_id, handle.clone());

        Ok(handle)
    }

    /// Get the sandbox for a run, returning an error if not found.
    pub async fn get(&self, run_id: i32) -> Result<SandboxHandle, AgentError> {
        let sandboxes = self.sandboxes.read().await;
        let handle = sandboxes
            .get(&run_id)
            .ok_or(AgentError::SandboxNotFound { run_id })?;

        // Verify it's still alive
        if !self.provider.is_alive(handle).await.unwrap_or(false) {
            return Err(AgentError::SandboxNotFound { run_id });
        }

        Ok(handle.clone())
    }

    /// Write a file into a run's sandbox at the given absolute path.
    pub async fn write_file(
        &self,
        run_id: i32,
        path: &str,
        contents: &[u8],
        mode: u32,
    ) -> Result<(), AgentError> {
        let handle = self.get(run_id).await?;
        self.provider
            .write_file(&handle, path, contents, mode)
            .await
    }

    /// Read a file from a run's sandbox at the given absolute path.
    pub async fn read_file(&self, run_id: i32, path: &str) -> Result<Vec<u8>, AgentError> {
        let handle = self.get(run_id).await?;
        self.provider.read_file(&handle, path).await
    }

    /// Write an entire local directory tree into a run's sandbox.
    pub async fn write_directory(
        &self,
        run_id: i32,
        local_dir: &std::path::Path,
        target_path: &str,
    ) -> Result<(), AgentError> {
        let handle = self.get(run_id).await?;
        self.provider
            .write_directory(&handle, local_dir, target_path)
            .await
    }

    /// Execute a command in a run's sandbox.
    pub async fn exec(
        &self,
        run_id: i32,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        let handle = self.get(run_id).await?;
        self.provider.exec(&handle, cmd, env, on_output).await
    }

    /// Destroy and release the sandbox for a run.
    pub async fn release(&self, run_id: i32) -> Result<(), AgentError> {
        let handle = {
            let mut sandboxes = self.sandboxes.write().await;
            sandboxes.remove(&run_id)
        };

        if let Some(handle) = handle {
            // Agent runs are ephemeral — purge the home volume too.
            if let Err(e) = self.provider.destroy(&handle, true).await {
                tracing::warn!(
                    "Failed to destroy sandbox {} for run {}: {}",
                    handle.sandbox_name,
                    run_id,
                    e
                );
            }
        }

        Ok(())
    }

    /// Recover all sandboxes for active (non-terminal) run IDs.
    /// Called on server startup.
    pub async fn recover_runs(&self, active_run_ids: &[i32]) -> usize {
        let mut recovered = 0;

        for &run_id in active_run_ids {
            match self.provider.recover(run_id).await {
                Ok(Some(handle)) => {
                    tracing::info!(
                        "Recovered sandbox {} for run {}",
                        handle.sandbox_name,
                        run_id
                    );
                    let mut sandboxes = self.sandboxes.write().await;
                    sandboxes.insert(run_id, handle);
                    recovered += 1;
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!("Failed to recover sandbox for run {}: {}", run_id, e);
                }
            }
        }

        if recovered > 0 {
            tracing::info!(
                "Recovered {} sandbox(es) on startup via {} provider",
                recovered,
                self.provider.name()
            );
        }

        recovered
    }

    /// Get the provider name (for logging).
    pub fn provider_name(&self) -> &str {
        self.provider.name()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::local::LocalSandboxProvider;
    use std::time::Duration;

    fn test_config(run_id: i32) -> SandboxCreateConfig {
        let work_dir = std::env::temp_dir().join(format!("test-registry-{}", run_id));
        SandboxCreateConfig {
            run_id,
            container_name_override: None,
            host_work_dir: work_dir,
            workspace_volume: None,
            image: None,
            cpu_limit: None,
            memory_limit_mb: None,
            pids_limit: None,
            disk_size_mb: None,
            network_mode: None,
            env_vars: HashMap::new(),
            idle_timeout: Duration::from_secs(3600),
            backend: None,
        }
    }

    #[tokio::test]
    async fn test_registry_get_or_create_creates_sandbox() {
        let provider = Arc::new(LocalSandboxProvider::new());
        let registry = SandboxRegistry::new(provider);

        let config = test_config(1);
        let work_dir = config.host_work_dir.clone();
        tokio::fs::create_dir_all(&work_dir).await.unwrap();

        let handle = registry.get_or_create(config).await.unwrap();
        assert_eq!(handle.sandbox_name, "local-sandbox-1");

        let _ = tokio::fs::remove_dir_all(&work_dir).await;
    }

    #[tokio::test]
    async fn test_registry_get_or_create_returns_existing() {
        let provider = Arc::new(LocalSandboxProvider::new());
        let registry = SandboxRegistry::new(provider);

        let config1 = test_config(2);
        let work_dir = config1.host_work_dir.clone();
        tokio::fs::create_dir_all(&work_dir).await.unwrap();

        let handle1 = registry.get_or_create(config1).await.unwrap();

        let config2 = test_config(2);
        let handle2 = registry.get_or_create(config2).await.unwrap();

        assert_eq!(handle1.sandbox_id, handle2.sandbox_id);

        let _ = tokio::fs::remove_dir_all(&work_dir).await;
    }

    #[tokio::test]
    async fn test_registry_get_not_found() {
        let provider = Arc::new(LocalSandboxProvider::new());
        let registry = SandboxRegistry::new(provider);

        let result = registry.get(999).await;
        assert!(matches!(
            result,
            Err(AgentError::SandboxNotFound { run_id: 999 })
        ));
    }

    #[tokio::test]
    async fn test_registry_release_removes_sandbox() {
        let provider = Arc::new(LocalSandboxProvider::new());
        let registry = SandboxRegistry::new(provider);

        let config = test_config(3);
        let work_dir = config.host_work_dir.clone();
        tokio::fs::create_dir_all(&work_dir).await.unwrap();

        registry.get_or_create(config).await.unwrap();

        // Sandbox should exist
        assert!(registry.get(3).await.is_ok());

        // Release it
        registry.release(3).await.unwrap();

        // Should no longer exist in registry
        assert!(matches!(
            registry.get(3).await,
            Err(AgentError::SandboxNotFound { run_id: 3 })
        ));

        let _ = tokio::fs::remove_dir_all(&work_dir).await;
    }

    #[tokio::test]
    async fn test_registry_exec_in_sandbox() {
        let provider = Arc::new(LocalSandboxProvider::new());
        let registry = SandboxRegistry::new(provider);

        let config = test_config(4);
        let work_dir = config.host_work_dir.clone();
        tokio::fs::create_dir_all(&work_dir).await.unwrap();

        registry.get_or_create(config).await.unwrap();

        let result = registry
            .exec(
                4,
                vec!["echo".to_string(), "registry test".to_string()],
                HashMap::new(),
                None,
            )
            .await
            .unwrap();

        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("registry test"));

        let _ = tokio::fs::remove_dir_all(&work_dir).await;
    }

    #[tokio::test]
    async fn test_registry_provider_name() {
        let provider = Arc::new(LocalSandboxProvider::new());
        let registry = SandboxRegistry::new(provider);
        assert_eq!(registry.provider_name(), "local");
    }
}
