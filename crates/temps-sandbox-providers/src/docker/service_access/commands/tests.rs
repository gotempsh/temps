// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::SandboxProvider;
use serde_json::json;
use temps_docker_capture::{FakeDocker, Method};
use temps_service_access_runtime::{PolicyEnvelope, WorkerAuthority, WorkerIdentity};
use tokenless_control::{
    cipher::AesGcmCipher,
    envelope::Envelope,
    memory::MemoryStore,
    probe::DisabledProber,
    service::{
        policy::{CreateCredential, CreateRule},
        sessions::{MintSession, SessionBinding},
    },
    store::RbacAuthorizer,
    Caller, ControlConfig, ControlServices, ServiceStores,
};
use tokenless_core::{
    authz::{Principal, Role},
    seal::{encode_public_key, SealPublicKey},
    snapshot::UnmatchedHostPolicy,
};

async fn deliveries(
    worker: &WorkerIdentity,
    incarnation: &str,
) -> (SessionDelivery, SessionDelivery) {
    let store = Arc::new(MemoryStore::new());
    let cipher = Arc::new(
        Envelope::open(&AesGcmCipher::generate_key(), store.clone())
            .await
            .unwrap(),
    );
    let control = ControlServices::new(
        ServiceStores {
            credentials: store.clone(),
            policy: store.clone(),
            audit: store.clone(),
            nodes: store,
        },
        Arc::new(RbacAuthorizer),
        cipher,
        Arc::new(DisabledProber),
        ControlConfig {
            lease_secs: 30,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let caller = Caller::service(Principal::service("test-host", Role::Admin, None));
    let mut settings = control.policy.get_settings(&caller).await.unwrap();
    settings.unmatched_host_policy = UnmatchedHostPolicy::Deny;
    control
        .policy
        .put_settings(&caller, settings)
        .await
        .unwrap();
    let vault = control
        .policy
        .create_vault(&caller, "test-project")
        .await
        .unwrap();
    let credential = control
        .policy
        .create_credential(
            &caller,
            &vault.id,
            CreateCredential {
                name: "test-service".into(),
                template_id: "generic-bearer".into(),
                template_version: None,
                host_override: vec!["api.example.com".into()],
                fields: [("token".into(), "credential-sentinel".into())].into(),
                custom_headers: vec![],
                substitutions: vec![],
                not_after: None,
            },
        )
        .await
        .unwrap();
    let rule = control
        .policy
        .create_rule(
            &caller,
            &vault.id,
            CreateRule {
                name: "test-rule".into(),
                credential_id: credential.id,
                hosts: None,
                paths: None,
                methods: None,
                mcp_tools: None,
                git: None,
            },
        )
        .await
        .unwrap();
    let registered = control
        .nodes
        .register_embedded(
            &caller,
            "test-worker",
            &encode_public_key(&SealPublicKey::from(worker.seal_public_key)),
        )
        .await
        .unwrap();
    let session = control
        .sessions
        .mint(
            &caller,
            &vault.id,
            MintSession {
                rule_ids: vec![rule.id],
                node_id: Some(registered.node.id),
                strict: true,
                ttl_secs: Some(60),
                binding: Some(SessionBinding {
                    id: incarnation.into(),
                    generation: 1,
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let snapshot = control
        .nodes
        .build_snapshot(&registered.node)
        .await
        .unwrap();
    let first = SessionDelivery {
        github_git: false,
        authority: WorkerAuthority {
            boot_id: worker.boot_id,
            node_id: registered.node.id.0,
            snapshot_verify_key: registered.snapshot_verify_key.to_bytes(),
        },
        policy: PolicyEnvelope::from(snapshot),
        session: session.token,
        version: 1,
    };
    let mut second = first.clone();
    second.version = 2;
    second.policy = control
        .nodes
        .build_snapshot(&registered.node)
        .await
        .unwrap()
        .into();
    (first, second)
}

struct Fixture {
    provider: DockerSandboxProvider,
    handle: SandboxHandle,
    _docker: FakeDocker,
    _root: tempfile::TempDir,
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("access-rpc-")
            .tempdir_in("/tmp")
            .unwrap();
        let docker = FakeDocker::start().await.unwrap();
        let provider =
            DockerSandboxProvider::new(Arc::new(docker.docker()), DockerSandboxConfig::default())
                .with_service_access_root(root.path().join("mounts"));
        let mounts = provider.service_access.as_ref().unwrap();
        let mut pending = mounts.reserve().unwrap();
        pending.attachment.retain_mount();
        let info: bollard::models::ContainerInspectResponse = serde_json::from_value(json!({
            "Id": "owned-container", "State": {"Running": true, "StartedAt": "2026-01-01T00:00:00Z"},
            "HostConfig": {"NetworkMode": "none"},
            "Config": {"Labels": {SANDBOX_CONTAINER_LABEL: "true", MOUNT_LABEL: pending.attachment.mount_id().unwrap()}},
            "Mounts": [{"Type": "bind", "RW": false, "Destination": GUEST_DIRECTORY, "Source": pending.attachment.directory()}]
        })).unwrap();
        mounts.adopt(&info, pending).unwrap();
        docker.respond(
            Method::GET,
            "/containers/owned-container/json",
            200,
            serde_json::to_value(info).unwrap(),
        );
        Self {
            provider,
            handle: SandboxHandle {
                node_id: None,
                sandbox_id: "owned-container".into(),
                sandbox_name: "temps-sandbox-test".into(),
                work_dir: PathBuf::from("/workspace"),
                backend: SandboxBackend::Docker,
                image: String::new(),
                conditions: vec![],
            },
            _docker: docker,
            _root: root,
        }
    }
    async fn inspect(&self) -> (ServiceAccessTarget, WorkerIdentity, bool, Option<u64>) {
        let ServiceAccessReply::Inspect {
            target,
            worker,
            connected,
            binding_version,
        } = self
            .provider
            .service_access(&self.handle, ServiceAccessCommand::Inspect)
            .await
            .unwrap()
        else {
            panic!("inspect receipt")
        };
        (target, worker, connected, binding_version)
    }
    async fn apply(
        &self,
        target: &ServiceAccessTarget,
        delivery: SessionDelivery,
    ) -> Result<ServiceAccessReply, AgentError> {
        self.provider
            .service_access(
                &self.handle,
                ServiceAccessCommand::Apply {
                    expected: target.clone(),
                    delivery: Box::new(delivery),
                },
            )
            .await
    }
}

#[tokio::test]
async fn worker_command_applies_only_current_identity_and_retires_before_receipt() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let f = Fixture::new().await;
    let (target, worker, connected, version) = f.inspect().await;
    assert!(!connected);
    assert_eq!(version, None);
    let (delivery, updated) = deliveries(&worker, &target.incarnation).await;
    let mut wrong = target.clone();
    wrong.incarnation = Uuid::new_v4().to_string();
    assert!(f.apply(&wrong, delivery.clone()).await.is_err());
    let mut wrong_boot = delivery.clone();
    wrong_boot.authority.boot_id = Uuid::new_v4();
    assert!(f.apply(&target, wrong_boot).await.is_err());
    assert!(matches!(
        f.apply(&target, delivery.clone()).await.unwrap(),
        ServiceAccessReply::Applied {
            binding_version: 1,
            ..
        }
    ));
    let (_, _, connected, version) = f.inspect().await;
    assert!(connected);
    assert_eq!(version, Some(1));
    assert!(f.apply(&target, delivery.clone()).await.is_err());
    assert!(matches!(
        f.apply(&target, updated).await.unwrap(),
        ServiceAccessReply::Applied {
            binding_version: 2,
            ..
        }
    ));
    let (current, _, connected, version) = f.inspect().await;
    assert_eq!(current, target);
    assert!(connected);
    assert_eq!(version, Some(2));
    let mount = f
        .provider
        .service_access
        .as_ref()
        .unwrap()
        .entries()
        .get(&target.container_id)
        .unwrap()
        .attachment
        .directory()
        .to_owned();
    let ca = std::fs::read_to_string(mount.join("ca.pem")).unwrap();
    assert!(ca.starts_with("-----BEGIN CERTIFICATE-----"));
    for secret in [
        "PRIVATE KEY",
        "credential-sentinel",
        delivery.session.as_str(),
    ] {
        assert!(!ca.contains(secret));
    }
    let mut guest = tokio::net::UnixStream::connect(mount.join("proxy.sock"))
        .await
        .unwrap();
    guest
        .write_all(b"CONNECT api.example.com:443 HTTP/1.1\r\nHost: api.example.com:443\r\n\r\n")
        .await
        .unwrap();
    let mut response = [0; 1024];
    let size = tokio::time::timeout(Duration::from_secs(3), guest.read(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&response[..size]).contains("200"));
    let receipt = f
        .provider
        .service_access(
            &f.handle,
            ServiceAccessCommand::Retire {
                expected: target.clone(),
                boot_id: worker.boot_id,
            },
        )
        .await
        .unwrap();
    assert!(matches!(receipt, ServiceAccessReply::Retired { .. }));
    let closed = tokio::time::timeout(Duration::from_secs(3), guest.read(&mut response))
        .await
        .unwrap();
    assert!(matches!(closed, Ok(0) | Err(_)));
    let (fresh, _, connected, version) = f.inspect().await;
    assert_ne!(fresh.incarnation, target.incarnation);
    assert!(!connected);
    assert_eq!(version, None);
    assert!(f.apply(&target, delivery).await.is_err());
}

#[tokio::test]
async fn abandoned_delivery_revokes_binding_and_forces_fresh_incarnation() {
    let f = Fixture::new().await;
    let (target, worker, _, _) = f.inspect().await;
    f.apply(&target, deliveries(&worker, &target.incarnation).await.0)
        .await
        .unwrap();
    let mounts = f.provider.service_access.as_ref().unwrap();
    let binding = mounts
        .entries()
        .get(&target.container_id)
        .unwrap()
        .binding
        .as_ref()
        .unwrap()
        .0
        .clone();
    let guard = DeliveryGuard(Some(binding.clone()));
    drop(guard);
    assert!(binding.status().revoked);
    let (fresh, _, connected, version) = f.inspect().await;
    assert_ne!(fresh.incarnation, target.incarnation);
    assert!(!connected);
    assert_eq!(version, None);
}

#[tokio::test]
async fn invalid_session_update_closes_previously_active_binding() {
    let f = Fixture::new().await;
    let (target, worker, _, _) = f.inspect().await;
    let (first, mut second) = deliveries(&worker, &target.incarnation).await;
    f.apply(&target, first).await.unwrap();
    second.session = "invalid-session-sentinel".into();
    let error = f.apply(&target, second).await.unwrap_err().to_string();
    assert!(!error.contains("invalid-session-sentinel"));
    let (current, _, connected, version) = f.inspect().await;
    assert_ne!(current, target);
    assert!(!connected);
    assert_eq!(version, None);
}

#[tokio::test]
async fn obsolete_boot_retirement_completes_without_cutting_current_binding() {
    let f = Fixture::new().await;
    let (target, worker, _, _) = f.inspect().await;
    f.apply(&target, deliveries(&worker, &target.incarnation).await.0)
        .await
        .unwrap();
    let mut obsolete = target.clone();
    obsolete.incarnation = Uuid::new_v4().to_string();
    let old_boot = Uuid::new_v4();
    for _ in 0..2 {
        let reply = f
            .provider
            .service_access(
                &f.handle,
                ServiceAccessCommand::Retire {
                    expected: obsolete.clone(),
                    boot_id: old_boot,
                },
            )
            .await
            .unwrap();
        assert!(
            matches!(reply, ServiceAccessReply::Retired { target: retired } if retired == obsolete)
        );
        let (current, _, connected, version) = f.inspect().await;
        assert_eq!(current, target);
        assert!(connected);
        assert_eq!(version, Some(1));
    }
    obsolete.container_id = "another-container".into();
    assert!(f
        .provider
        .service_access(
            &f.handle,
            ServiceAccessCommand::Retire {
                expected: obsolete,
                boot_id: old_boot,
            }
        )
        .await
        .is_err());
}

#[tokio::test]
async fn binding_renewal_can_reference_the_installed_policy_without_resending_it() {
    let f = Fixture::new().await;
    let (target, worker, _, _) = f.inspect().await;
    let (delivery, _) = deliveries(&worker, &target.incarnation).await;
    let mut reference = delivery.clone();
    reference.policy.body.clear();
    assert!(f.apply(&target, reference.clone()).await.is_err());
    let ServiceAccessReply::Applied {
        snapshot_version, ..
    } = f.apply(&target, delivery).await.unwrap()
    else {
        panic!("expected apply receipt")
    };
    reference.version = 2;
    assert!(matches!(f.apply(&target, reference).await.unwrap(),
        ServiceAccessReply::Applied { snapshot_version: version, binding_version: 2, .. } if version == snapshot_version));
    let (_, _, connected, version) = f.inspect().await;
    assert!(connected);
    assert_eq!(version, Some(2));
}
