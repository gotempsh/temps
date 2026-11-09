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

type Binding = (Arc<SandboxBinding>, u64);

pub(crate) struct Provider {
    pub worker: WorkerControl,
    pub incarnation: Uuid,
    pub binding: Mutex<Option<(Arc<SandboxBinding>, u64)>>,
    pub wrong_ack: AtomicBool,
    pub calls: AtomicUsize,
    pub full_policies: AtomicUsize,
    pub policy_references: AtomicUsize,
    pub reject_reference: AtomicBool,
    extra_targets: Mutex<HashMap<String, ServiceAccessTarget>>,
    extra_bindings: Mutex<HashMap<String, Binding>>,
}
impl Provider {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            worker: WorkerControl::new(Arc::new(HostAudit)),
            incarnation: Uuid::new_v4(),
            binding: Mutex::new(None),
            wrong_ack: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            full_policies: AtomicUsize::new(0),
            policy_references: AtomicUsize::new(0),
            reject_reference: AtomicBool::new(false),
            extra_targets: Mutex::new(HashMap::new()),
            extra_bindings: Mutex::new(HashMap::new()),
        })
    }
    pub fn add_sandbox(&self, name: &str) {
        self.extra_targets.lock().unwrap().insert(
            format!("temps-sandbox-{name}"),
            ServiceAccessTarget {
                incarnation: Uuid::new_v4().to_string(),
                container_id: format!("container-{name}"),
            },
        );
    }
    fn target_for(&self, handle: &SandboxHandle) -> ServiceAccessTarget {
        self.extra_targets
            .lock()
            .unwrap()
            .get(&handle.sandbox_name)
            .cloned()
            .unwrap_or_else(|| self.target())
    }
    fn handle(&self) -> SandboxHandle {
        SandboxHandle {
            node_id: None,
            sandbox_id: "fixture-container".into(),
            sandbox_name: "temps-sandbox-dispatch".into(),
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
        let target = self.target_for(handle);
        assert_eq!(handle.sandbox_id, target.container_id);
        self.calls.fetch_add(1, Ordering::SeqCst);
        match command {
            ServiceAccessCommand::Inspect => Ok(ServiceAccessReply::Inspect {
                target: target.clone(),
                worker: self.worker.identity(),
                connected: false,
                binding_version: None,
            }),
            ServiceAccessCommand::Apply { expected, delivery } => {
                assert_eq!(expected, target);
                if delivery.policy.body.is_empty() {
                    self.policy_references.fetch_add(1, Ordering::SeqCst);
                    if self.reject_reference.load(Ordering::SeqCst) {
                        return Err(AgentError::Validation {
                            message: "referenced policy replaced".into(),
                        });
                    }
                } else {
                    self.full_policies.fetch_add(1, Ordering::SeqCst);
                }
                let boot = self.worker.identity().boot_id;
                self.worker.configure(delivery.authority).await.unwrap();
                let version = self
                    .worker
                    .install(boot, &delivery.policy.into_snapshot().unwrap())
                    .await
                    .unwrap();
                let primary = expected == self.target();
                let previous = if primary {
                    self.binding.lock().unwrap().clone()
                } else {
                    self.extra_bindings
                        .lock()
                        .unwrap()
                        .get(&expected.container_id)
                        .cloned()
                };
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
                if primary {
                    *self.binding.lock().unwrap() = Some((binding, delivery.version));
                } else {
                    self.extra_bindings
                        .lock()
                        .unwrap()
                        .insert(expected.container_id.clone(), (binding, delivery.version));
                }
                Ok(ServiceAccessReply::Applied {
                    target: expected,
                    snapshot_version: version,
                    binding_version: delivery.version
                        + u64::from(self.wrong_ack.load(Ordering::SeqCst)),
                })
            }
            ServiceAccessCommand::Retire { expected, boot_id } => {
                assert_eq!(expected, target);
                assert_eq!(boot_id, self.worker.identity().boot_id);
                let previous = if expected == self.target() {
                    self.binding.lock().unwrap().take()
                } else {
                    self.extra_bindings
                        .lock()
                        .unwrap()
                        .remove(&expected.container_id)
                };
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
        let full_name = format!("temps-sandbox-{name}");
        if let Some(target) = self.extra_targets.lock().unwrap().get(&full_name) {
            let mut handle = self.handle();
            handle.sandbox_name = full_name;
            handle.sandbox_id = target.container_id.clone();
            return Ok(Some(handle));
        }
        assert_eq!(full_name, self.handle().sandbox_name);
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
