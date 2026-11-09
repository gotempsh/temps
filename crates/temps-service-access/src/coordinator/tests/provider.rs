// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use async_trait::async_trait;
use std::{collections::HashMap, path::Path, sync::Mutex};
use temps_agent_core::{error::AgentError, OnEventCallback};
use temps_sandbox_providers::{
    service_access::{ServiceAccessCommand, ServiceAccessReply, ServiceAccessTarget},
    KillSignal, SandboxBackend, SandboxCreateConfig, SandboxExecResult, SandboxHandle,
    SandboxProvider,
};
use temps_service_access_runtime::{HostAudit, SandboxBinding, SessionDelivery, WorkerControl};

pub(super) struct Provider {
    pub worker: WorkerControl,
    pub incarnation: Uuid,
    pub binding: Mutex<Option<(Arc<SandboxBinding>, u64)>>,
    pub wrong_ack: AtomicBool,
    pub calls: AtomicUsize,
}
impl Provider {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            worker: WorkerControl::new(Arc::new(HostAudit)),
            incarnation: Uuid::new_v4(),
            binding: Mutex::new(None),
            wrong_ack: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
        })
    }
    fn handle(&self) -> SandboxHandle {
        SandboxHandle {
            node_id: None,
            sandbox_id: "fixture-container".into(),
            sandbox_name: "temps-sandbox-sbx_dispatch".into(),
            work_dir: "/workspace".into(),
            backend: SandboxBackend::Docker,
            image: String::new(),
            conditions: vec![],
        }
    }
    fn target(&self) -> ServiceAccessTarget {
        ServiceAccessTarget {
            incarnation: self.incarnation.to_string(),
            container_id: "fixture-container".into(),
        }
    }
}
#[async_trait]
impl SandboxProvider for Provider {
    async fn service_access(
        &self,
        handle: &SandboxHandle,
        command: ServiceAccessCommand,
    ) -> Result<ServiceAccessReply, AgentError> {
        assert_eq!(handle.sandbox_id, self.handle().sandbox_id);
        self.calls.fetch_add(1, Ordering::SeqCst);
        match command {
            ServiceAccessCommand::Inspect => Ok(ServiceAccessReply::Inspect {
                target: self.target(),
                worker: self.worker.identity(),
                connected: false,
                binding_version: None,
            }),
            ServiceAccessCommand::Apply { expected, delivery } => {
                assert_eq!(expected, self.target());
                let boot = self.worker.identity().boot_id;
                self.worker.configure(delivery.authority).await.unwrap();
                let version = self
                    .worker
                    .install(boot, &delivery.policy.into_snapshot().unwrap())
                    .await
                    .unwrap();
                let previous = self.binding.lock().unwrap().clone();
                let binding = if let Some((binding, prior_version)) = previous {
                    assert!(delivery.version > prior_version);
                    self.worker
                        .update(
                            boot,
                            &binding,
                            SessionDelivery::binding_update(delivery.session, delivery.version),
                        )
                        .await
                        .unwrap();
                    binding
                } else {
                    self.worker
                        .bind(
                            boot,
                            SessionDelivery::binding_spec(
                                expected.incarnation.clone(),
                                delivery.session,
                                delivery.version,
                            ),
                            version,
                        )
                        .await
                        .unwrap()
                };
                binding.ready(version).await.unwrap();
                *self.binding.lock().unwrap() = Some((binding, delivery.version));
                Ok(ServiceAccessReply::Applied {
                    target: expected,
                    snapshot_version: version,
                    binding_version: delivery.version
                        + u64::from(self.wrong_ack.load(Ordering::SeqCst)),
                })
            }
            ServiceAccessCommand::Retire { expected, boot_id } => {
                assert_eq!(expected, self.target());
                assert_eq!(boot_id, self.worker.identity().boot_id);
                let previous = self.binding.lock().unwrap().take();
                if let Some((binding, _)) = previous {
                    binding
                        .revoke_and_wait(std::time::Duration::from_secs(5))
                        .await
                        .unwrap();
                }
                Ok(ServiceAccessReply::Retired { target: expected })
            }
        }
    }
    async fn recover_by_name_on(
        &self,
        node: Option<i32>,
        name: &str,
    ) -> Result<Option<SandboxHandle>, AgentError> {
        assert_eq!(node, None);
        assert_eq!(name, self.handle().sandbox_name);
        Ok(Some(self.handle()))
    }
    async fn create(&self, _: SandboxCreateConfig) -> Result<SandboxHandle, AgentError> {
        panic!("unexpected create")
    }
    async fn exec(
        &self,
        _: &SandboxHandle,
        _: Vec<String>,
        _: HashMap<String, String>,
        _: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        panic!("unexpected exec")
    }
    async fn is_alive(&self, _: &SandboxHandle) -> Result<bool, AgentError> {
        panic!("unexpected liveness")
    }
    async fn write_file(
        &self,
        _: &SandboxHandle,
        _: &str,
        _: &[u8],
        _: u32,
    ) -> Result<(), AgentError> {
        panic!("unexpected file write")
    }
    async fn read_file(&self, _: &SandboxHandle, _: &str) -> Result<Vec<u8>, AgentError> {
        panic!("unexpected file read")
    }
    async fn write_directory(
        &self,
        _: &SandboxHandle,
        _: &Path,
        _: &str,
    ) -> Result<(), AgentError> {
        panic!("unexpected upload")
    }
    async fn kill_processes(
        &self,
        _: &SandboxHandle,
        _: &str,
        _: KillSignal,
    ) -> Result<(), AgentError> {
        panic!("unexpected process kill")
    }
    async fn destroy(&self, _: &SandboxHandle, _: bool) -> Result<(), AgentError> {
        panic!("unexpected destroy")
    }
    async fn recover(&self, _: i32) -> Result<Option<SandboxHandle>, AgentError> {
        panic!("unexpected legacy recovery")
    }
    fn name(&self) -> &str {
        "test"
    }
    async fn is_available(&self) -> bool {
        true
    }
    async fn image_status(&self) -> Result<(bool, String), AgentError> {
        panic!("unexpected image lookup")
    }
    async fn rebuild_image(&self) -> Result<String, AgentError> {
        panic!("unexpected image build")
    }
}
