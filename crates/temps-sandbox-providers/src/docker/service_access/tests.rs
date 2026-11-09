// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use serde_json::json;
use temps_docker_capture::{FakeDocker, Method};

fn root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("access-provider-")
        .tempdir_in("/tmp")
        .unwrap()
}

fn inspect(pending: &PendingMount) -> bollard::models::ContainerInspectResponse {
    serde_json::from_value(json!({
        "Id": "owned-container", "State": {"Running": true, "StartedAt": "2026-01-01T00:00:00Z"},
        "Config": {"Labels": {SANDBOX_CONTAINER_LABEL: "true", MOUNT_LABEL: pending.attachment.mount_id().unwrap()}},
        "Mounts": [{"Type": "bind", "RW": false, "Destination": GUEST_DIRECTORY, "Source": pending.attachment.directory()}]
    })).unwrap()
}

#[tokio::test]
async fn mount_is_read_only_and_recovers_with_a_new_incarnation() {
    let root = root();
    let mounts = AccessMounts::new(root.path().join("mounts"));
    let fake = FakeDocker::start().await.unwrap();
    let mut pending = mounts.reserve().unwrap();
    let path = pending.attachment.directory().to_owned();
    let id = pending.attachment.mount_id().unwrap().to_owned();
    let mut body = bollard::models::ContainerCreateBody::default();
    pending
        .configure(&mut body, Arc::new(fake.docker()), "owned-name")
        .unwrap();
    let host = body.host_config.unwrap();
    let mount = &host.mounts.unwrap()[0];
    assert_eq!(mount.target.as_deref(), Some(GUEST_DIRECTORY));
    assert_eq!(mount.read_only, Some(true));
    assert_eq!(
        host.restart_policy.unwrap().name,
        Some(bollard::models::RestartPolicyNameEnum::NO)
    );
    let info = inspect(&pending);
    let first = mounts.adopt(&info, pending).unwrap();
    assert_eq!(mounts.target(&info).unwrap(), first);
    mounts.close(&first.container_id).await.unwrap();
    assert!(
        path.exists(),
        "stopped containers keep their bind-mount inode"
    );
    assert!(mounts.target(&info).is_err());
    let second = mounts.recover(&info).unwrap();
    assert_ne!(first.incarnation, second.incarnation);
    assert_eq!(first.container_id, second.container_id);
    mounts.close(&second.container_id).await.unwrap();
    mounts.remove_detached(&id).unwrap();
    assert!(!path.exists());
}

#[tokio::test]
async fn wrong_container_mount_or_start_identity_is_rejected() {
    let root = root();
    let mounts = AccessMounts::new(root.path().join("mounts"));
    let mut pending = mounts.reserve().unwrap();
    pending.attachment.retain_mount();
    let info = inspect(&pending);
    mounts.adopt(&info, pending).unwrap();
    for variant in 0..5 {
        let mut wrong = info.clone();
        match variant {
            0 => wrong.state.as_mut().unwrap().started_at = Some("2026-01-02T00:00:00Z".into()),
            1 => wrong.mounts.as_mut().unwrap()[0].rw = Some(true),
            2 => wrong.mounts.as_mut().unwrap()[0].source = Some("/tmp/another-sandbox".into()),
            3 => wrong
                .config
                .as_mut()
                .unwrap()
                .labels
                .as_mut()
                .unwrap()
                .remove(SANDBOX_CONTAINER_LABEL)
                .map(|_| ())
                .unwrap(),
            _ => wrong.state.as_mut().unwrap().running = Some(false),
        }
        assert!(mounts.target(&wrong).is_err(), "variant {variant}");
    }
    mounts.close("owned-container").await.unwrap();
    mounts.remove_detached(mount_id(&info).unwrap()).unwrap();
}

#[tokio::test]
async fn pending_mount_is_removed_without_docker_before_configuration() {
    let root = root();
    let mounts = AccessMounts::new(root.path().join("mounts"));
    let pending = mounts.reserve().unwrap();
    let path = pending.attachment.directory().to_owned();
    drop(pending);
    assert!(!path.exists());
    assert_eq!(mounts.permits.available_permits(), MAX_WORKER_BINDINGS);
}

#[tokio::test]
async fn cancelled_configured_mount_removes_only_its_owned_container() {
    for replacement in [false, true] {
        let root = root();
        let mounts = AccessMounts::new(root.path().join("mounts"));
        let fake = FakeDocker::start().await.unwrap();
        let mut pending = mounts.reserve().unwrap();
        let path = pending.attachment.directory().to_owned();
        pending
            .configure(
                &mut Default::default(),
                Arc::new(fake.docker()),
                "owned-name",
            )
            .unwrap();
        let mut info = inspect(&pending);
        if replacement {
            info.config
                .as_mut()
                .unwrap()
                .labels
                .as_mut()
                .unwrap()
                .remove(MOUNT_LABEL);
        }
        fake.respond(
            Method::GET,
            "/containers/owned-name/json",
            200,
            serde_json::to_value(info).unwrap(),
        );
        fake.respond(
            Method::DELETE,
            "/containers/owned-container",
            204,
            json!(null),
        );
        drop(pending);
        tokio::time::timeout(Duration::from_secs(3), async {
            while path.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            fake.calls().iter().filter(|c| c.method == "DELETE").count(),
            usize::from(!replacement)
        );
        assert_eq!(mounts.permits.available_permits(), MAX_WORKER_BINDINGS);
    }
}

#[tokio::test]
async fn failed_cleanup_preserves_mount_until_container_removal_is_confirmed() {
    let root = root();
    let mounts = AccessMounts::new(root.path().join("mounts"));
    let fake = FakeDocker::start().await.unwrap();
    let mut pending = mounts.reserve().unwrap();
    let path = pending.attachment.directory().to_owned();
    let id = pending.attachment.mount_id().unwrap().to_owned();
    pending
        .configure(
            &mut Default::default(),
            Arc::new(fake.docker()),
            "owned-name",
        )
        .unwrap();
    fake.respond(
        Method::GET,
        "/containers/owned-name/json",
        500,
        json!({"message":"unavailable"}),
    );
    drop(pending);
    tokio::time::timeout(Duration::from_secs(3), async {
        while mounts.permits.available_permits() != MAX_WORKER_BINDINGS {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(path.exists());
    assert!(!path.join("proxy.sock").exists());
    assert!(!fake.calls().iter().any(|c| c.method == "DELETE"));
    mounts.remove_detached(&id).unwrap();
}

#[test]
fn lifecycle_operations_exclude_recovery_until_stop_finishes() {
    let root = root();
    let mounts = AccessMounts::new(root.path().join("mounts"));
    let operation = mounts.operation("first").unwrap();
    assert!(mounts.operation("first").is_err());
    let other = mounts.operation("second").unwrap();
    drop(operation);
    assert!(mounts.operation("first").is_ok());
    drop(other);
}

#[tokio::test]
async fn retirement_is_fenced_and_stop_failure_still_closes_access() {
    let root = root();
    let fake = FakeDocker::start().await.unwrap();
    let provider =
        DockerSandboxProvider::new(Arc::new(fake.docker()), DockerSandboxConfig::default())
            .with_service_access_root(root.path().join("mounts"));
    let mounts = provider.service_access.as_ref().unwrap();
    let mut pending = mounts.reserve().unwrap();
    pending.attachment.retain_mount();
    let info = inspect(&pending);
    let first = mounts.adopt(&info, pending).unwrap();
    provider.retire_service_access_target(&first).await.unwrap();
    provider.retire_service_access_target(&first).await.unwrap();
    let second = mounts.recover(&info).unwrap();
    assert!(provider.retire_service_access_target(&first).await.is_err());
    assert_eq!(mounts.target(&info).unwrap(), second);
    fake.respond(
        Method::POST,
        "/containers/owned-container/stop",
        500,
        json!({"message": "unavailable"}),
    );
    let handle = SandboxHandle {
        node_id: None,
        sandbox_id: second.container_id,
        sandbox_name: "owned-name".into(),
        work_dir: PathBuf::from("/workspace"),
        backend: SandboxBackend::Docker,
        image: String::new(),
        conditions: vec![],
    };
    assert!(provider.stop(&handle).await.is_err());
    assert!(mounts.target(&info).is_err());
    mounts.remove_detached(mount_id(&info).unwrap()).unwrap();
}

#[tokio::test]
async fn reaper_preserves_mounted_or_leased_directories_and_removes_abandoned_ones() {
    let root = root();
    let mounts = AccessMounts::new(root.path().join("mounts"));
    let live = mounts.reserve().unwrap();
    let mut stopped = mounts.reserve().unwrap();
    let stopped_id = stopped.attachment.mount_id().unwrap().to_owned();
    let stopped_path = stopped.attachment.directory().to_owned();
    stopped.attachment.retain_mount();
    drop(stopped);
    let mut abandoned = mounts.reserve().unwrap();
    let abandoned_path = abandoned.attachment.directory().to_owned();
    abandoned.attachment.retain_mount();
    drop(abandoned);
    let referenced = std::collections::HashSet::from([stopped_id]);
    assert_eq!(mounts.reap_detached(&referenced).unwrap(), 1);
    assert!(!abandoned_path.exists());
    assert!(stopped_path.exists());
    assert!(live.attachment.directory().exists());
    assert_eq!(mounts.reap_detached(&Default::default()).unwrap(), 1);
    assert!(!stopped_path.exists());
}

#[tokio::test]
async fn unavailable_optional_mount_does_not_block_ordinary_sandbox_creation() {
    let root = root();
    let not_a_directory = root.path().join("file");
    std::fs::write(&not_a_directory, b"preserve").unwrap();
    let mounts = AccessMounts::new(not_a_directory.clone());
    let fake = FakeDocker::start().await.unwrap();
    assert!(mounts
        .reserve_for_image(Arc::new(fake.docker()), "alpine:3.20")
        .await
        .unwrap()
        .is_none());
    assert!(fake.calls().is_empty());
    assert_eq!(std::fs::read(not_a_directory).unwrap(), b"preserve");
}
