// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use async_trait::async_trait;
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};
use temps_agent_core::OnEventCallback;
use temps_sandbox_providers::{KillSignal, SandboxCreateConfig, SandboxExecResult};
use temps_service_access_runtime::{PolicyEnvelope, WorkerAuthority, WorkerIdentity};

struct Provider {
    handle: Mutex<Option<SandboxHandle>>,
    reply: Mutex<ServiceAccessReply>,
    calls: Mutex<Vec<(SandboxHandle, ServiceAccessCommand)>>,
    recoveries: Mutex<Vec<(Option<i32>, String)>>,
    hang: AtomicBool,
    fail: AtomicBool,
    capacity: Semaphore,
    delay: Duration,
}
#[async_trait]
impl SandboxProvider for Provider {
    async fn service_access(
        &self,
        handle: &SandboxHandle,
        command: ServiceAccessCommand,
    ) -> Result<ServiceAccessReply, AgentError> {
        let _permit = self
            .capacity
            .try_acquire()
            .map_err(|_| AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: "worker request capacity exceeded".into(),
            })?;
        self.calls.lock().unwrap().push((handle.clone(), command));
        tokio::time::sleep(self.delay).await;
        if self.hang.load(Ordering::Relaxed) {
            std::future::pending::<()>().await;
        }
        if self.fail.load(Ordering::Relaxed) {
            return Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: "sensitive-provider-detail".into(),
            });
        }
        Ok(self.reply.lock().unwrap().clone())
    }
    async fn recover_by_name_on(
        &self,
        node_id: Option<i32>,
        name: &str,
    ) -> Result<Option<SandboxHandle>, AgentError> {
        self.recoveries.lock().unwrap().push((node_id, name.into()));
        Ok(self
            .handle
            .lock()
            .unwrap()
            .clone()
            .filter(|handle| handle.sandbox_name == format!("temps-sandbox-{name}")))
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
fn sandbox() -> sandboxes::Model {
    let now = chrono::Utc::now();
    sandboxes::Model {
        id: 42,
        public_id: "sbx_test".into(),
        user_id: Some(1),
        agent_run_id: None,
        name: "test".into(),
        status: "running".into(),
        lifecycle: "workspace".into(),
        project_id: Some(7),
        source_repo_url: None,
        image: None,
        work_dir: "/workspace".into(),
        timeout_secs: 900,
        metadata: None,
        backend: Some("docker".into()),
        node_id: Some(7),
        egress_policy: Default::default(),
        created_at: now,
        last_activity_at: now,
        expires_at: now + chrono::Duration::seconds(900),
        preview_password_hash: None,
        preview_password_hint: None,
        preview_access: Default::default(),
        preview_allowed_cidrs: vec![],
    }
}
fn target() -> StoredTarget {
    StoredTarget::new(
        Uuid::new_v4(),
        Some(7),
        Some(Uuid::new_v4()),
        Some("original-container"),
        Some("temps-sandbox-test"),
        Some("docker"),
    )
    .unwrap()
}
fn provider(target: &StoredTarget) -> Arc<Provider> {
    Arc::new(Provider {
        handle: Mutex::new(Some(target.handle.clone())),
        reply: Mutex::new(ServiceAccessReply::Inspect {
            target: target.target.clone(),
            worker: WorkerIdentity {
                boot_id: target.boot_id,
                seal_public_key: [1; 32],
            },
            connected: false,
            binding_version: None,
        }),
        calls: Mutex::new(vec![]),
        recoveries: Mutex::new(vec![]),
        hang: AtomicBool::new(false),
        fail: AtomicBool::new(false),
        capacity: Semaphore::new(2),
        delay: Duration::ZERO,
    })
}
fn delivery(target: &StoredTarget) -> SessionDelivery {
    SessionDelivery {
        github_git: false,
        authority: WorkerAuthority {
            boot_id: target.boot_id,
            node_id: Uuid::new_v4(),
            snapshot_verify_key: [1; 32],
        },
        policy: PolicyEnvelope {
            body: "sealed-fixture".into(),
            signature: "signature".into(),
            key_id: "key".into(),
        },
        session: "signed-fixture-session".into(),
        version: 2,
    }
}
#[tokio::test]
async fn inspection_uses_the_agent_run_label_instead_of_the_public_id() {
    let mut target = target();
    target.handle.sandbox_name = "temps-sandbox-123".into();
    let provider = provider(&target);
    let transport = WorkerTransport::new(provider.clone());
    let mut sandbox = sandbox();
    sandbox.agent_run_id = Some(123);
    let observed = transport.inspect(&sandbox).await.unwrap();
    assert_eq!(observed.container_name, "temps-sandbox-123");
    assert_eq!(
        provider.recoveries.lock().unwrap().as_slice(),
        &[(Some(7), "123".into())]
    );
}

#[tokio::test]
async fn inspection_preserves_owner_and_checks_recovered_identity() {
    let target = target();
    let provider = provider(&target);
    let transport = WorkerTransport::new(provider.clone());
    let observed = transport.inspect(&sandbox()).await.unwrap();
    assert_eq!(observed.incarnation.to_string(), target.target.incarnation);
    assert_eq!(observed.node_id, Some(7));
    assert_eq!(
        provider.recoveries.lock().unwrap().as_slice(),
        &[(Some(7), "test".into())]
    );
    for handle in [
        None,
        Some(SandboxHandle {
            node_id: None,
            ..target.handle.clone()
        }),
        Some(SandboxHandle {
            backend: SandboxBackend::Firecracker,
            ..target.handle.clone()
        }),
        Some(SandboxHandle {
            sandbox_name: "replacement".into(),
            ..target.handle.clone()
        }),
    ] {
        *provider.handle.lock().unwrap() = handle;
        assert!(transport.inspect(&sandbox()).await.is_err());
    }
    // No command was sent after a bad/missing owner recovery, and no retry
    // attempted another host.
    assert_eq!(provider.calls.lock().unwrap().len(), 1);
    assert_eq!(provider.recoveries.lock().unwrap().len(), 5);
}
#[tokio::test]
async fn inspection_rejects_wrong_worker_receipts_and_masks_provider_errors() {
    let target = target();
    let provider = provider(&target);
    let transport = WorkerTransport::new(provider.clone());
    for reply in [
        ServiceAccessReply::Retired {
            target: target.target.clone(),
        },
        ServiceAccessReply::Inspect {
            target: ServiceAccessTarget {
                container_id: "replacement".into(),
                ..target.target.clone()
            },
            worker: WorkerIdentity {
                boot_id: target.boot_id,
                seal_public_key: [1; 32],
            },
            connected: false,
            binding_version: None,
        },
        ServiceAccessReply::Inspect {
            target: ServiceAccessTarget {
                incarnation: Uuid::nil().to_string(),
                ..target.target.clone()
            },
            worker: WorkerIdentity {
                boot_id: target.boot_id,
                seal_public_key: [1; 32],
            },
            connected: false,
            binding_version: None,
        },
        ServiceAccessReply::Inspect {
            target: target.target.clone(),
            worker: WorkerIdentity {
                boot_id: target.boot_id,
                seal_public_key: [0; 32],
            },
            connected: false,
            binding_version: None,
        },
    ] {
        *provider.reply.lock().unwrap() = reply;
        assert!(transport.inspect(&sandbox()).await.is_err());
    }
    provider.fail.store(true, Ordering::Relaxed);
    let error = transport.inspect(&sandbox()).await.unwrap_err();
    assert!(error.to_string().contains("sbx_test"));
    assert!(!error.to_string().contains("sensitive-provider-detail"));
    assert!(std::error::Error::source(&error).is_some());
}
#[tokio::test]
async fn apply_requires_exact_target_snapshot_and_binding_receipt() {
    let target = target();
    let provider = provider(&target);
    let transport = WorkerTransport::new(provider.clone());
    *provider.reply.lock().unwrap() = ServiceAccessReply::Applied {
        target: target.target.clone(),
        snapshot_version: 3,
        binding_version: 2,
    };
    transport
        .apply(&target, 3, delivery(&target))
        .await
        .unwrap();
    for reply in [
        ServiceAccessReply::Applied {
            target: target.target.clone(),
            snapshot_version: 2,
            binding_version: 2,
        },
        ServiceAccessReply::Applied {
            target: target.target.clone(),
            snapshot_version: 3,
            binding_version: 1,
        },
        ServiceAccessReply::Applied {
            target: ServiceAccessTarget {
                container_id: "replacement".into(),
                ..target.target.clone()
            },
            snapshot_version: 3,
            binding_version: 2,
        },
        ServiceAccessReply::Retired {
            target: target.target.clone(),
        },
    ] {
        *provider.reply.lock().unwrap() = reply;
        assert!(transport
            .apply(&target, 3, delivery(&target))
            .await
            .is_err());
    }
    let before = provider.calls.lock().unwrap().len();
    let mut wrong = delivery(&target);
    wrong.authority.boot_id = Uuid::new_v4();
    assert!(transport.apply(&target, 3, wrong).await.is_err());
    assert!(transport
        .apply(&target, 0, delivery(&target))
        .await
        .is_err());
    assert_eq!(provider.calls.lock().unwrap().len(), before);
    assert!(provider.recoveries.lock().unwrap().is_empty());
}
#[tokio::test]
async fn deletion_retirement_never_resolves_a_replacement_by_name() {
    let target = target();
    let provider = provider(&target);
    let transport = WorkerTransport::new(provider.clone());
    let now = chrono::Utc::now();
    let command = sandbox_access_delivery::Model {
        id: 1,
        runtime_id: Uuid::parse_str(&target.target.incarnation).unwrap(),
        node_id: Some(7),
        worker_boot_id: Some(target.boot_id),
        provider_identity: Some(target.handle.sandbox_id.clone()),
        provider_backend: Some("docker".into()),
        provider_name: Some(target.handle.sandbox_name.clone()),
        desired_version: 4,
        operation: sandbox_access_delivery::DeliveryOperation::Retire,
        attempts: 1,
        available_at: now,
        claim_id: None,
        claimed_until: None,
        completed_at: None,
        created_at: now,
    };
    let stored = StoredTarget::from_command(&command).unwrap();
    provider.handle.lock().unwrap().as_mut().unwrap().sandbox_id = "replacement".into();
    *provider.reply.lock().unwrap() = ServiceAccessReply::Retired {
        target: target.target.clone(),
    };
    transport.retire(&stored).await.unwrap();
    assert!(provider.recoveries.lock().unwrap().is_empty());
    {
        let calls = provider.calls.lock().unwrap();
        assert_eq!(calls[0].0.sandbox_id, "original-container");
        assert_eq!(calls[0].0.node_id, Some(7));
        assert!(
            matches!(&calls[0].1,ServiceAccessCommand::Retire { boot_id,.. } if *boot_id==target.boot_id)
        );
    }
    *provider.reply.lock().unwrap() = ServiceAccessReply::Retired {
        target: ServiceAccessTarget {
            incarnation: Uuid::new_v4().to_string(),
            ..target.target.clone()
        },
    };
    assert!(transport.retire(&stored).await.is_err());
}
#[tokio::test(start_paused = true)]
async fn timeouts_and_saturation_release_admission_without_unbounded_waiting() {
    let target = target();
    let provider = provider(&target);
    let transport = WorkerTransport::new(provider.clone());
    provider.hang.store(true, Ordering::Relaxed);
    let sandbox = sandbox();
    let (a, b, c, d, e) = tokio::join!(
        transport.inspect(&sandbox),
        transport.inspect(&sandbox),
        transport.inspect(&sandbox),
        transport.inspect(&sandbox),
        transport.inspect(&sandbox)
    );
    let errors = [
        a.unwrap_err(),
        b.unwrap_err(),
        c.unwrap_err(),
        d.unwrap_err(),
        e.unwrap_err(),
    ];
    assert_eq!(
        errors
            .iter()
            .filter(|e| matches!(e, TransportError::Timeout { .. }))
            .count(),
        4
    );
    assert_eq!(
        errors
            .iter()
            .filter(|e| matches!(e, TransportError::Busy { .. }))
            .count(),
        1
    );
    provider.hang.store(false, Ordering::Relaxed);
    transport.inspect(&sandbox).await.unwrap();
}
#[test]
fn stored_targets_reject_legacy_or_malformed_identity() {
    for (boot, container, backend) in [
        (None, Some("container"), Some("docker")),
        (Some(Uuid::nil()), Some("container"), Some("docker")),
        (Some(Uuid::new_v4()), None, Some("docker")),
        (Some(Uuid::new_v4()), Some("container"), Some("local")),
        (Some(Uuid::new_v4()), Some("bad\nidentity"), Some("docker")),
    ] {
        assert!(StoredTarget::new(
            Uuid::new_v4(),
            Some(7),
            boot,
            container,
            Some("sandbox"),
            backend
        )
        .is_err());
    }
}

#[tokio::test(start_paused = true)]
async fn inspection_saturation_does_not_block_retirement_delivery() {
    let observed = target();
    let provider = provider(&observed);
    let transport = WorkerTransport::new(provider.clone());
    let _inspections = transport.slots.acquire_many(4).await.unwrap();
    *provider.reply.lock().unwrap() = ServiceAccessReply::Retired {
        target: observed.target.clone(),
    };
    transport.retire(&observed).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn four_inspections_queue_within_the_same_worker_capacity() {
    let target = target();
    let mut provider = provider(&target);
    Arc::get_mut(&mut provider).unwrap().delay = Duration::from_secs(4);
    let transport = WorkerTransport::new(provider.clone());
    let sandbox = sandbox();
    let (a, b, c, d) = tokio::join!(
        transport.inspect(&sandbox),
        transport.inspect(&sandbox),
        transport.inspect(&sandbox),
        transport.inspect(&sandbox),
    );
    for result in [a, b, c, d] {
        result.unwrap();
    }
    assert_eq!(provider.calls.lock().unwrap().len(), 4);
    assert_eq!(provider.capacity.available_permits(), 2);
}

#[tokio::test(start_paused = true)]
async fn inspections_apply_and_retire_share_worker_admission() {
    let target = target();
    let mut provider = provider(&target);
    Arc::get_mut(&mut provider).unwrap().delay = Duration::from_millis(100);
    let transport = WorkerTransport::new(provider.clone());
    let sandbox = sandbox();
    let (inspection, another, apply, retire) = tokio::join!(
        transport.inspect(&sandbox),
        transport.inspect(&sandbox),
        transport.apply(&target, 1, delivery(&target)),
        transport.retire(&target),
    );
    inspection.unwrap();
    another.unwrap();
    // This fixture returns an Inspect receipt for every command. Reaching the
    // receipt validation proves apply and retire were admitted, not rejected.
    assert!(matches!(
        apply,
        Err(TransportError::Invalid {
            operation: "apply",
            ..
        })
    ));
    assert!(matches!(
        retire,
        Err(TransportError::Invalid {
            operation: "retire",
            ..
        })
    ));
    assert_eq!(provider.calls.lock().unwrap().len(), 4);
    assert_eq!(provider.capacity.available_permits(), 2);
}
