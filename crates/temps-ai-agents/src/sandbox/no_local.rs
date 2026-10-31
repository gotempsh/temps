// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The host sandbox provider of a control plane that runs no workloads
//! (ADR 063).
//!
//! It is the `local` half of [`super::node_routing::NodeRoutingSandboxProvider`]
//! on a `--profile control-plane` server, so sandboxes on worker nodes keep
//! working exactly as ADR-048 describes. Anything addressed to this host —
//! an agent run, an application workspace, an image build, a row with no
//! node — gets [`AgentError::NoLocalSandboxHost`].
//!
//! It exists so the server never has to choose between "no sandbox API at
//! all" and an unsandboxed fallback. It never touches Docker, the
//! filesystem or a hypervisor. Creating anything fails loudly; recovery and
//! orphan cleanup find nothing, because nothing can have been created here
//! by this provider.

use async_trait::async_trait;
use std::collections::HashMap;

use super::{
    KillSignal, SandboxBackend, SandboxCreateConfig, SandboxExecResult, SandboxHandle,
    SandboxProvider,
};
use crate::ai_cli::OnEventCallback;
use crate::error::AgentError;

/// `name()` of [`NoLocalSandboxProvider`].
pub const NO_LOCAL_PROVIDER_NAME: &str = "none (control plane runs no workloads)";

/// Refuses every sandbox on this host; see the module docs.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoLocalSandboxProvider;

impl NoLocalSandboxProvider {
    pub fn new() -> Self {
        Self
    }

    fn refuse<T>(operation: &str) -> Result<T, AgentError> {
        Err(AgentError::NoLocalSandboxHost {
            operation: operation.to_string(),
        })
    }
}

#[async_trait]
impl SandboxProvider for NoLocalSandboxProvider {
    async fn create(&self, config: SandboxCreateConfig) -> Result<SandboxHandle, AgentError> {
        let operation = if config.run_id > 0 {
            format!("Agent run {}", config.run_id)
        } else {
            "This sandbox".to_string()
        };
        Self::refuse(&operation)
    }

    async fn exec(
        &self,
        _handle: &SandboxHandle,
        _cmd: Vec<String>,
        _env: HashMap<String, String>,
        _on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        Self::refuse("Running a command")
    }

    async fn is_alive(&self, _handle: &SandboxHandle) -> Result<bool, AgentError> {
        // Nothing runs here, so no handle addressed to this host is alive.
        Ok(false)
    }

    async fn write_file(
        &self,
        _handle: &SandboxHandle,
        _path: &str,
        _contents: &[u8],
        _mode: u32,
    ) -> Result<(), AgentError> {
        Self::refuse("Writing a file")
    }

    async fn read_file(&self, _handle: &SandboxHandle, _path: &str) -> Result<Vec<u8>, AgentError> {
        Self::refuse("Reading a file")
    }

    async fn write_directory(
        &self,
        _handle: &SandboxHandle,
        _local_dir: &std::path::Path,
        _target_path: &str,
    ) -> Result<(), AgentError> {
        Self::refuse("Copying a directory")
    }

    async fn kill_processes(
        &self,
        _handle: &SandboxHandle,
        _pattern: &str,
        _signal: KillSignal,
    ) -> Result<(), AgentError> {
        Self::refuse("Stopping processes")
    }

    async fn destroy(
        &self,
        _handle: &SandboxHandle,
        _purge_volumes: bool,
    ) -> Result<(), AgentError> {
        // Nothing can exist here, so there is nothing to destroy. Succeeding
        // lets a row left from before the instance was re-profiled be
        // cleaned up instead of becoming undeletable.
        Ok(())
    }

    async fn recover(&self, _run_id: i32) -> Result<Option<SandboxHandle>, AgentError> {
        Ok(None)
    }

    async fn recover_by_name(
        &self,
        _container_name: &str,
    ) -> Result<Option<SandboxHandle>, AgentError> {
        Ok(None)
    }

    fn supports_backend(&self, _backend: SandboxBackend) -> bool {
        false
    }

    fn name(&self) -> &str {
        NO_LOCAL_PROVIDER_NAME
    }

    async fn is_available(&self) -> bool {
        false
    }

    async fn image_status(&self) -> Result<(bool, String), AgentError> {
        Self::refuse("Checking the sandbox image")
    }

    async fn rebuild_image(&self) -> Result<String, AgentError> {
        Self::refuse("Building the sandbox image")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::node_routing::{NodeRoutingSandboxProvider, RemoteNodeResolver};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    fn handle(node_id: Option<i32>) -> SandboxHandle {
        SandboxHandle {
            node_id,
            sandbox_id: "c1".into(),
            sandbox_name: "temps-sandbox-1".into(),
            work_dir: PathBuf::from("/workspace"),
            backend: SandboxBackend::Docker,
            image: String::new(),
            conditions: Vec::new(),
        }
    }

    fn config(run_id: i32, node_id: Option<i32>) -> SandboxCreateConfig {
        SandboxCreateConfig {
            node_id,
            run_id,
            container_name_override: Some("sbx".into()),
            host_work_dir: PathBuf::from("/nonexistent"),
            workspace_volume: None,
            image: None,
            cpu_limit: None,
            memory_limit_mb: None,
            pids_limit: None,
            disk_size_mb: None,
            allow_docker: false,
            network_mode: None,
            env_vars: HashMap::new(),
            idle_timeout: Duration::from_secs(60),
            backend: None,
            owner_user_id: None,
            egress_policy: Default::default(),
        }
    }

    fn assert_refused<T>(result: Result<T, AgentError>) {
        match result {
            Err(AgentError::NoLocalSandboxHost { .. }) => {}
            Err(other) => panic!("expected NoLocalSandboxHost, got {other:?}"),
            Ok(_) => panic!("expected NoLocalSandboxHost, got Ok"),
        }
    }

    #[tokio::test]
    async fn every_operation_on_this_host_is_refused_with_the_typed_error() {
        let provider = NoLocalSandboxProvider::new();
        let h = handle(None);
        assert_refused(provider.create(config(0, None)).await);
        assert_refused(
            provider
                .exec(&h, vec!["true".into()], HashMap::new(), None)
                .await,
        );
        assert_refused(provider.write_file(&h, "/x", b"x", 0o600).await);
        assert_refused(provider.read_file(&h, "/x").await);
        assert_refused(
            provider
                .write_directory(&h, std::path::Path::new("/tmp"), "/x")
                .await,
        );
        assert_refused(provider.kill_processes(&h, "x", KillSignal::Term).await);
        assert_refused(provider.image_status().await);
        assert_refused(provider.rebuild_image().await);
    }

    #[tokio::test]
    async fn it_advertises_nothing_and_cleanup_paths_are_quiet() {
        let provider = NoLocalSandboxProvider::new();
        assert!(!provider.is_available().await);
        for backend in [
            SandboxBackend::Docker,
            SandboxBackend::Firecracker,
            SandboxBackend::Microsandbox,
            SandboxBackend::Local,
        ] {
            assert!(!provider.supports_backend(backend), "{backend:?}");
        }
        assert!(provider.recover(1).await.unwrap().is_none());
        assert!(provider
            .recover_by_name("temps-sandbox-1")
            .await
            .unwrap()
            .is_none());
        assert!(!provider.is_alive(&handle(None)).await.unwrap());
        provider.destroy(&handle(None), true).await.unwrap();
        provider
            .release_orphaned_resources("temps-sandbox-1")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn an_agent_run_refusal_names_the_run() {
        let err = NoLocalSandboxProvider::new()
            .create(config(42, None))
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(
            message.starts_with("Agent run 42 needs a sandbox"),
            "{message}"
        );
        assert!(message.contains("runs no workloads"), "{message}");
    }

    /// A worker that creates anything it is asked to.
    struct Worker;

    #[async_trait]
    impl SandboxProvider for Worker {
        async fn create(&self, config: SandboxCreateConfig) -> Result<SandboxHandle, AgentError> {
            let mut h = handle(None);
            h.sandbox_name = config.container_name_override.unwrap_or_default();
            Ok(h)
        }
        async fn exec(
            &self,
            _h: &SandboxHandle,
            _c: Vec<String>,
            _e: HashMap<String, String>,
            _o: Option<OnEventCallback>,
        ) -> Result<SandboxExecResult, AgentError> {
            unreachable!("not exercised")
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
            Ok(Vec::new())
        }
        async fn write_directory(
            &self,
            _h: &SandboxHandle,
            _l: &std::path::Path,
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
            Ok(())
        }
        async fn recover(&self, _run_id: i32) -> Result<Option<SandboxHandle>, AgentError> {
            Ok(None)
        }
        fn name(&self) -> &str {
            "worker"
        }
        async fn is_available(&self) -> bool {
            true
        }
        async fn image_status(&self) -> Result<(bool, String), AgentError> {
            Ok((true, "ok".into()))
        }
        async fn rebuild_image(&self) -> Result<String, AgentError> {
            Ok("ok".into())
        }
    }

    struct OneWorker;

    #[async_trait]
    impl RemoteNodeResolver for OneWorker {
        async fn provider_for(&self, node_id: i32) -> Result<Arc<dyn SandboxProvider>, AgentError> {
            assert_eq!(node_id, 7);
            Ok(Arc::new(Worker))
        }
    }

    #[tokio::test]
    async fn behind_the_node_router_workers_still_run_sandboxes_and_this_host_refuses() {
        let router = NodeRoutingSandboxProvider::new(
            Arc::new(NoLocalSandboxProvider::new()),
            Arc::new(OneWorker),
        );
        let on_worker = router.create(config(0, Some(7))).await.unwrap();
        assert_eq!(on_worker.node_id, Some(7));
        assert!(router.is_alive(&on_worker).await.unwrap());

        assert_refused(router.create(config(0, None)).await);
        assert_refused(router.create(config(5, None)).await);
        // Host-level questions are about this host, which runs nothing.
        assert!(!router.is_available().await);
        assert!(!router.supports_backend(SandboxBackend::Docker));
        assert_eq!(router.name(), NO_LOCAL_PROVIDER_NAME);
    }
}
