// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
#[cfg(unix)]
#[path = "traffic_tests.rs"]
mod traffic;
use tokenless_control::service::{
    policy::{CreateCredential, CreateRule},
    sessions::{MintSession, SessionBinding},
};
use tokenless_control::{
    cipher::AesGcmCipher, envelope::Envelope, memory::MemoryStore, model::Node,
    probe::DisabledProber, store::RbacAuthorizer, Caller, ControlConfig, ControlServices,
    ServiceStores,
};
use tokenless_core::{
    audit::AuditEvent,
    authz::{Principal, Role},
    ids::{RuleId, VaultId},
    seal::{encode_public_key, SealPublicKey, SealSecretKey},
};

struct Audit;
#[async_trait::async_trait]
impl AuditShipper for Audit {
    async fn ship(&self, _: &[AuditEvent]) -> Result<(), DataPlaneError> {
        Ok(())
    }
}
struct Fixture {
    control: ControlServices,
    caller: Caller,
    node: Node,
    vault: VaultId,
    rule: RuleId,
    proxy: WorkerProxy,
}

#[cfg(unix)]
#[tokio::test]
async fn authenticated_worker_registration_enforces_node_binding_and_shutdown() {
    let f = Fixture::new().await;
    let worker = WorkerControl::new(Arc::new(Audit));
    let identity = worker.identity();
    let registered = f
        .control
        .nodes
        .register_embedded(
            &f.caller,
            "registered-worker",
            &encode_public_key(&SealPublicKey::from(identity.seal_public_key)),
        )
        .await
        .unwrap();
    worker
        .configure(WorkerAuthority {
            boot_id: identity.boot_id,
            node_id: registered.node.id.0,
            snapshot_verify_key: registered.snapshot_verify_key.to_bytes(),
        })
        .await
        .unwrap();
    // A snapshot sealed for another node is not usable even though the same
    // control plane signed it. An old boot cannot deliver a new snapshot.
    let wrong_node = f.control.nodes.build_snapshot(&f.node).await.unwrap();
    assert!(worker.install(identity.boot_id, &wrong_node).await.is_err());
    let token = f
        .control
        .sessions
        .mint(
            &f.caller,
            &f.vault,
            MintSession {
                rule_ids: vec![f.rule],
                node_id: Some(registered.node.id),
                binding: Some(SessionBinding {
                    id: "sandbox:test-incarnation".into(),
                    generation: 1,
                }),
                strict: true,
                ttl_secs: Some(60),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .token;
    let snapshot = f
        .control
        .nodes
        .build_snapshot(&registered.node)
        .await
        .unwrap();
    assert!(worker
        .install(uuid::Uuid::new_v4(), &snapshot)
        .await
        .is_err());
    let mut tampered = snapshot.clone();
    tampered.body.push(' ');
    assert!(worker
        .install_with_certificate(identity.boot_id, &tampered)
        .await
        .is_err());
    let (version, certificate) = worker
        .install_with_certificate(identity.boot_id, &snapshot)
        .await
        .unwrap();
    let binding = worker
        .bind(identity.boot_id, f.spec(token), version)
        .await
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let attachment = UnixAttachment::bind(root.path(), binding).unwrap();
    attachment.publish_certificate(&certificate).unwrap();
    assert!(!attachment.directory().join("github-git").exists());
    attachment.publish_github_git_route().unwrap();
    assert!(attachment.directory().join("github-git").exists());
    attachment.publish_certificate(&certificate).unwrap();
    assert!(attachment.directory().join("github-git").exists());
    let published = std::fs::read(attachment.directory().join("ca.pem")).unwrap();
    let certs = rustls_pemfile::certs(&mut published.as_slice())
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(certs.len(), 1);
    assert!(!String::from_utf8_lossy(&published).contains("PRIVATE KEY"));
    let guest = idle_guest_tunnel(&attachment).await;
    drop(worker);
    assert_guest_closed(guest).await;
}
impl Fixture {
    async fn new() -> Self {
        Self::with_lease(30).await
    }
    async fn with_lease(lease_secs: u32) -> Self {
        Self::with_transport(
            lease_secs,
            "api.example.com",
            DataPlaneConfig::default(),
            false,
        )
        .await
    }

    async fn with_transport(
        lease_secs: u32,
        host: &str,
        transport: DataPlaneConfig,
        fixture_loopback: bool,
    ) -> Self {
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
                lease_secs,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let caller = Caller::service(Principal::service("test-host", Role::Admin, None));
        let mut settings = control.policy.get_settings(&caller).await.unwrap();
        settings.unmatched_host_policy = UnmatchedHostPolicy::Deny;
        if fixture_loopback {
            settings.egress.allow = vec!["127.0.0.1/32".parse().unwrap()];
        }
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
                    host_override: vec![host.into()],
                    fields: [("token".into(), "test-credential".into())].into(),
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
        let seal_secret = SealSecretKey::random_from_rng(rand08::rngs::OsRng);
        let registration = control
            .nodes
            .register_embedded(
                &caller,
                "test-worker",
                &encode_public_key(&SealPublicKey::from(&seal_secret)),
            )
            .await
            .unwrap();
        let proxy = WorkerProxy::start_config(
            NodeKeys {
                node_id: registration.node.id,
                seal_secret,
                snapshot_key: registration.snapshot_verify_key,
            },
            Arc::new(Audit),
            transport,
        )
        .await
        .unwrap();
        Self {
            control,
            caller,
            node: registration.node,
            vault: vault.id,
            rule: rule.id,
            proxy,
        }
    }
    async fn mint(&self, strict: bool) -> String {
        self.control
            .sessions
            .mint(
                &self.caller,
                &self.vault,
                MintSession {
                    rule_ids: vec![self.rule],
                    node_id: Some(self.node.id),
                    binding: Some(SessionBinding {
                        id: "sandbox:test-incarnation".into(),
                        generation: 1,
                    }),
                    strict,
                    ttl_secs: Some(60),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .token
    }
    fn spec(&self, token: String) -> BindingSpec {
        BindingSpec {
            binding_id: "sandbox:test-incarnation".into(),
            generation: 1,
            sessions: vec![token],
            version: 1,
            max_connections: 8,
        }
    }
    async fn install(&self) -> u64 {
        self.proxy
            .install(&self.control.nodes.build_snapshot(&self.node).await.unwrap())
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn worker_binds_only_strict_sessions_and_acknowledges_revocation() {
    let f = Fixture::new().await;
    let token = f.mint(true).await;
    let version = f.install().await;
    let binding = f.proxy.bind(f.spec(token), version).await.unwrap();
    let receipt = binding
        .revoke_and_wait(Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(receipt.snapshot_version, Some(version));
    assert_eq!(receipt.connections_closed, 0);
}

#[tokio::test]
async fn worker_refuses_grace_period_and_spoofed_incarnation() {
    let f = Fixture::new().await;
    let token = f.mint(false).await;
    let version = f.install().await;
    assert!(matches!(
        f.proxy.bind(f.spec(token), version).await,
        Err(RuntimeError::Invalid { .. })
    ));
    let token = f.mint(true).await;
    let version = f.install().await;
    let mut spec = f.spec(token);
    spec.generation = 2;
    assert!(matches!(
        f.proxy.bind(spec, version).await,
        Err(RuntimeError::DataPlane { .. })
    ));
}

#[tokio::test]
async fn worker_refuses_missing_policy_bad_signature_and_stale_snapshots() {
    let f = Fixture::new().await;
    let token = f.mint(true).await;
    assert!(f.proxy.bind(f.spec(token), 1).await.is_err());
    let snapshot = f.control.nodes.build_snapshot(&f.node).await.unwrap();
    f.proxy.install(&snapshot).await.unwrap();
    assert!(f.proxy.install(&snapshot).await.is_err());
    let mut forged = snapshot;
    forged.body.push(' ');
    assert!(matches!(
        f.proxy.install(&forged).await,
        Err(RuntimeError::Snapshot(_))
    ));
}

#[tokio::test]
async fn worker_bounds_connection_admission() {
    let f = Fixture::new().await;
    let token = f.mint(true).await;
    let version = f.install().await;
    for limit in [0, MAX_SANDBOX_CONNECTIONS + 1] {
        let mut spec = f.spec(token.clone());
        spec.max_connections = limit;
        assert!(matches!(
            f.proxy.bind(spec, version).await,
            Err(RuntimeError::Invalid { .. })
        ));
    }
}

#[tokio::test]
async fn updates_cannot_weaken_expiry_or_cross_workers() {
    let f = Fixture::new().await;
    let token = f.mint(true).await;
    let version = f.install().await;
    let binding = f.proxy.bind(f.spec(token), version).await.unwrap();
    let nonstrict = f.mint(false).await;
    assert!(f
        .proxy
        .update(
            &binding,
            BindingUpdate {
                version: 2,
                sessions: vec![nonstrict]
            }
        )
        .await
        .is_err());
    let other = Fixture::new().await;
    let token = f.mint(true).await;
    assert!(other
        .proxy
        .update(
            &binding,
            BindingUpdate {
                version: 2,
                sessions: vec![token.clone()]
            }
        )
        .await
        .is_err());
    f.proxy
        .update(
            &binding,
            BindingUpdate {
                version: 2,
                sessions: vec![token.clone()],
            },
        )
        .await
        .unwrap();
    assert_eq!(binding.inner.version(), 2);
    assert!(f
        .proxy
        .update(
            &binding,
            BindingUpdate {
                version: 2,
                sessions: vec![token]
            }
        )
        .await
        .is_err());
}

#[tokio::test]
async fn worker_refuses_unrestricted_proxy_policy() {
    let f = Fixture::new().await;
    let mut settings = f.control.policy.get_settings(&f.caller).await.unwrap();
    settings.unmatched_host_policy = UnmatchedHostPolicy::Passthrough;
    f.control
        .policy
        .put_settings(&f.caller, settings)
        .await
        .unwrap();
    let snapshot = f.control.nodes.build_snapshot(&f.node).await.unwrap();
    assert!(matches!(
        f.proxy.install(&snapshot).await,
        Err(RuntimeError::Invalid { .. })
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn attachment_is_isolated_and_removes_only_its_owned_directory() {
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let f = Fixture::new().await;
    let token = f.mint(true).await;
    let version = f.install().await;
    let binding = f.proxy.bind(f.spec(token), version).await.unwrap();
    let root = tempfile::tempdir().unwrap();
    let untouched = root.path().join("existing");
    std::fs::write(&untouched, "keep").unwrap();
    let attachment = UnixAttachment::bind(root.path(), Arc::clone(&binding)).unwrap();
    let path = attachment.directory().join("proxy.sock");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o666
    );
    let mut guest = tokio::net::UnixStream::connect(&path).await.unwrap();
    guest
        .write_all(b"CONNECT other.example.com:443 HTTP/1.1\r\nHost: other.example.com:443\r\n\r\n")
        .await
        .unwrap();
    let mut response = [0u8; 1024];
    let count = tokio::time::timeout(Duration::from_secs(2), guest.read(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&response[..count]).contains("403"));
    attachment
        .revoke_and_wait(Duration::from_secs(2))
        .await
        .unwrap();
    assert!(binding.status().revoked);
    drop(attachment);
    assert!(!path.exists());
    assert_eq!(std::fs::read_to_string(untouched).unwrap(), "keep");
}

#[cfg(unix)]
#[tokio::test]
async fn attachment_drop_cuts_access_and_creation_failure_preserves_parent() {
    let f = Fixture::new().await;
    let token = f.mint(true).await;
    let version = f.install().await;
    let binding = f.proxy.bind(f.spec(token), version).await.unwrap();
    let root = tempfile::tempdir().unwrap();
    assert!(UnixAttachment::bind(&root.path().join("missing"), Arc::clone(&binding)).is_err());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    let attachment = UnixAttachment::bind(root.path(), Arc::clone(&binding)).unwrap();
    drop(attachment);
    assert!(binding.status().revoked);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn worker_bounds_policy_lease_and_snapshot_size() {
    let f = Fixture::with_lease(MAX_POLICY_LEASE_SECONDS + 1).await;
    let mut snapshot = f.control.nodes.build_snapshot(&f.node).await.unwrap();
    assert!(matches!(
        f.proxy.install(&snapshot).await,
        Err(RuntimeError::Invalid { .. })
    ));
    snapshot.body = " ".repeat(MAX_SNAPSHOT_BYTES + 1);
    assert!(matches!(
        f.proxy.install(&snapshot).await,
        Err(RuntimeError::Invalid { .. })
    ));
}

#[tokio::test]
async fn worker_rejects_empty_multiple_and_oversized_sessions() {
    let f = Fixture::new().await;
    let token = f.mint(true).await;
    let version = f.install().await;
    for sessions in [
        vec![],
        vec![token.clone(), token.clone()],
        vec!["x".repeat(32 * 1024 + 1)],
    ] {
        let mut spec = f.spec(token.clone());
        spec.sessions = sessions;
        assert!(matches!(
            f.proxy.bind(spec, version).await,
            Err(RuntimeError::Invalid { .. })
        ));
    }
}

#[tokio::test]
async fn worker_releases_admission_after_binding_drop() {
    let mut f = Fixture::new().await;
    f.proxy.bindings = Arc::new(Semaphore::new(1));
    let token = f.mint(true).await;
    let version = f.install().await;
    let binding = f.proxy.bind(f.spec(token.clone()), version).await.unwrap();
    assert!(matches!(
        f.proxy.bind(f.spec(token.clone()), version).await,
        Err(RuntimeError::Invalid { .. })
    ));
    drop(binding);
    assert!(f.proxy.bind(f.spec(token), version).await.is_ok());
}

#[cfg(unix)]
async fn idle_guest_tunnel(attachment: &UnixAttachment) -> tokio::net::UnixStream {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut guest = tokio::net::UnixStream::connect(attachment.directory().join("proxy.sock"))
        .await
        .unwrap();
    guest
        .write_all(b"CONNECT api.example.com:443 HTTP/1.1\r\nHost: api.example.com:443\r\n\r\n")
        .await
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(2), async {
        let mut response = Vec::new();
        while !response.ends_with(b"\r\n\r\n") {
            assert!(response.len() < 4096);
            response.push(guest.read_u8().await.unwrap());
        }
        response
    })
    .await
    .unwrap();
    assert!(response.starts_with(b"HTTP/1.1 200"));
    guest
}

#[cfg(unix)]
async fn assert_guest_closed(mut guest: tokio::net::UnixStream) {
    use tokio::io::AsyncReadExt;
    let mut byte = [0];
    let result = tokio::time::timeout(Duration::from_secs(4), guest.read(&mut byte))
        .await
        .expect("idle guest tunnel outlived its enforcement deadline");
    assert!(
        matches!(result, Ok(0)) || result.is_err(),
        "guest received unexpected traffic"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn revocation_receipt_cuts_a_guest_stalled_before_tls() {
    let f = Fixture::new().await;
    let token = f.mint(true).await;
    let version = f.install().await;
    let binding = f.proxy.bind(f.spec(token), version).await.unwrap();
    let root = tempfile::tempdir().unwrap();
    let attachment = UnixAttachment::bind(root.path(), binding).unwrap();
    let guest = idle_guest_tunnel(&attachment).await;
    attachment
        .revoke_and_wait(Duration::from_secs(2))
        .await
        .unwrap();
    assert_guest_closed(guest).await;
}

#[cfg(unix)]
#[tokio::test]
async fn expired_policy_lease_cuts_a_guest_without_a_control_plane() {
    let f = Fixture::with_lease(2).await;
    let token = f.mint(true).await;
    let version = f.install().await;
    let binding = f.proxy.bind(f.spec(token), version).await.unwrap();
    let root = tempfile::tempdir().unwrap();
    let attachment = UnixAttachment::bind(root.path(), binding).unwrap();
    let guest = idle_guest_tunnel(&attachment).await;
    assert_guest_closed(guest).await;
}

#[cfg(unix)]
#[tokio::test]
async fn stopping_the_worker_cuts_guest_attachments() {
    let f = Fixture::new().await;
    let token = f.mint(true).await;
    let version = f.install().await;
    let binding = f.proxy.bind(f.spec(token), version).await.unwrap();
    let root = tempfile::tempdir().unwrap();
    let attachment = UnixAttachment::bind(root.path(), binding).unwrap();
    let guest = idle_guest_tunnel(&attachment).await;
    drop(f.proxy);
    assert_guest_closed(guest).await;
}

#[cfg(unix)]
#[tokio::test]
async fn reserved_attachment_denies_until_activation_then_closes_permanently() {
    let f = Fixture::new().await;
    let token = f.mint(true).await;
    let version = f.install().await;
    let binding = f.proxy.bind(f.spec(token), version).await.unwrap();
    let root = tempfile::tempdir().unwrap();
    let attachment = UnixAttachment::reserve(root.path(), binding.id().into()).unwrap();
    let path = attachment.directory().join("proxy.sock");
    let guest = tokio::net::UnixStream::connect(&path).await.unwrap();
    assert_guest_closed(guest).await;
    attachment.activate(Arc::clone(&binding)).unwrap();
    attachment.activate(Arc::clone(&binding)).unwrap(); // idempotent delivery
    let guest = idle_guest_tunnel(&attachment).await;
    attachment
        .revoke_and_wait(Duration::from_secs(2))
        .await
        .unwrap();
    assert_guest_closed(guest).await;
    assert!(attachment.activate(binding).is_err());
    drop(attachment);
    assert!(!path.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn reserved_attachment_rejects_another_sandbox_and_closed_pending_activation() {
    let f = Fixture::new().await;
    let token = f.mint(true).await;
    let version = f.install().await;
    let binding = f.proxy.bind(f.spec(token), version).await.unwrap();
    let root = tempfile::tempdir().unwrap();
    let other = UnixAttachment::reserve(root.path(), "sandbox:other-incarnation".into()).unwrap();
    assert!(other.activate(Arc::clone(&binding)).is_err());
    let guest = tokio::net::UnixStream::connect(other.directory().join("proxy.sock"))
        .await
        .unwrap();
    assert_guest_closed(guest).await;
    let pending = UnixAttachment::reserve(root.path(), binding.id().into()).unwrap();
    let cut = pending
        .revoke_and_wait(Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(cut.connections_closed, 0);
    assert!(pending.activate(binding).is_err());
    assert!(UnixAttachment::reserve(root.path(), String::new()).is_err());
    assert!(UnixAttachment::reserve(root.path(), "x".repeat(257)).is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn mounted_restart_preserves_the_directory_but_requires_a_new_incarnation_grant() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let f = Fixture::new().await;
    let token = f.mint(true).await;
    let version = f.install().await;
    let old = f.proxy.bind(f.spec(token), version).await.unwrap();
    let root = tempfile::tempdir_in("/tmp").unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let directory = MountDirectory::create(root.path()).unwrap();
    let id = directory.id().to_string();
    let path = directory.guest_path().to_path_buf();
    let inode = std::fs::metadata(&path).unwrap().ino();
    let mut attachment = UnixAttachment::reserve_mounted(directory, old.id().into()).unwrap();
    assert_eq!(attachment.mount_id(), Some(id.as_str()));
    attachment.retain_mount();
    attachment.activate(Arc::clone(&old)).unwrap();
    let guest = idle_guest_tunnel(&attachment).await;
    drop(attachment);
    assert_guest_closed(guest).await;
    assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
    let directory = MountDirectory::recover(root.path(), &id).unwrap();
    let new_id = "sandbox:new-incarnation";
    let attachment = UnixAttachment::reserve_mounted(directory, new_id.into()).unwrap();
    assert!(attachment.activate(old).is_err());
    assert_guest_closed(
        tokio::net::UnixStream::connect(path.join("proxy.sock"))
            .await
            .unwrap(),
    )
    .await;
    let token = f
        .control
        .sessions
        .mint(
            &f.caller,
            &f.vault,
            MintSession {
                rule_ids: vec![f.rule],
                node_id: Some(f.node.id),
                binding: Some(SessionBinding {
                    id: new_id.into(),
                    generation: 1,
                }),
                strict: true,
                ttl_secs: Some(60),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .token;
    let version = f.install().await;
    let mut spec = f.spec(token);
    spec.binding_id = new_id.into();
    let fresh = f.proxy.bind(spec, version).await.unwrap();
    attachment.activate(fresh).unwrap();
    let guest = idle_guest_tunnel(&attachment).await;
    attachment
        .revoke_and_wait(Duration::from_secs(2))
        .await
        .unwrap();
    assert_guest_closed(guest).await;
    drop(attachment);
    MountDirectory::recover(root.path(), &id)
        .unwrap()
        .remove()
        .unwrap();
    assert!(!path.exists());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_long_host_data_directory_binds_through_the_short_guest_mount_path() {
    use std::os::unix::fs::{symlink, DirBuilderExt};
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let long = root.path().join("host-data-".repeat(16));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&long)
        .unwrap();
    let mount = MountDirectory::create(&long).unwrap();
    let attachment = UnixAttachment::reserve_mounted(mount, "sandbox:long-path".into()).unwrap();
    assert!(attachment.directory().as_os_str().len() > 108);
    // A symlink provides the same short pathname resolution that a Docker
    // bind mount does. The listener lives at the long host path's inode.
    let guest_path = root.path().join("guest");
    symlink(attachment.directory(), &guest_path).unwrap();
    let guest = tokio::net::UnixStream::connect(guest_path.join("proxy.sock"))
        .await
        .unwrap();
    assert_guest_closed(guest).await;
}

#[tokio::test(start_paused = true)]
async fn worker_policy_replay_is_idempotent_without_renewing_lease_or_reinstalling_old_policy() {
    let f = Fixture::new().await;
    let worker = WorkerControl::new(Arc::new(Audit));
    let identity = worker.identity();
    let registered = f
        .control
        .nodes
        .register_embedded(
            &f.caller,
            "replay-worker",
            &encode_public_key(&SealPublicKey::from(identity.seal_public_key)),
        )
        .await
        .unwrap();
    worker
        .configure(WorkerAuthority {
            boot_id: identity.boot_id,
            node_id: registered.node.id.0,
            snapshot_verify_key: registered.snapshot_verify_key.to_bytes(),
        })
        .await
        .unwrap();
    let first = f
        .control
        .nodes
        .build_snapshot(&registered.node)
        .await
        .unwrap();
    let version = worker.install(identity.boot_id, &first).await.unwrap();
    assert_eq!(
        worker.install(identity.boot_id, &first).await.unwrap(),
        version
    );
    let mut tampered = first.clone();
    tampered.body.push(' ');
    assert!(worker.install(identity.boot_id, &tampered).await.is_err());
    let second = f
        .control
        .nodes
        .build_snapshot(&registered.node)
        .await
        .unwrap();
    let next = worker.install(identity.boot_id, &second).await.unwrap();
    assert!(next > version);
    assert!(worker.install(identity.boot_id, &first).await.is_err());
    tokio::time::advance(Duration::from_secs(29)).await;
    assert_eq!(
        worker.install(identity.boot_id, &second).await.unwrap(),
        next
    );
    tokio::time::advance(Duration::from_secs(2)).await;
    // Wall time has not advanced, but a replay cannot reset the host deadline.
    assert!(worker.install(identity.boot_id, &second).await.is_err());
}

#[tokio::test]
async fn worker_rejects_an_overflowing_signed_lease_without_panicking() {
    let f = Fixture::new().await;
    let snapshot = f.control.nodes.build_snapshot(&f.node).await.unwrap();
    let mut body = snapshot.verify(&f.proxy.snapshot_key).unwrap();
    body.issued_at = chrono::DateTime::<Utc>::MAX_UTC;
    let key = ed25519_dalek::SigningKey::generate(&mut rand08::rngs::OsRng);
    let worker = WorkerControl::new(Arc::new(Audit));
    let identity = worker.identity();
    worker
        .configure(WorkerAuthority {
            boot_id: identity.boot_id,
            node_id: body.node_id.0,
            snapshot_verify_key: key.verifying_key().to_bytes(),
        })
        .await
        .unwrap();
    let signed = tokenless_core::snapshot::SignedSnapshot::sign(&body, &key).unwrap();
    let error = worker.install(identity.boot_id, &signed).await.unwrap_err();
    assert!(error.to_string().contains("supported time range"));
}
