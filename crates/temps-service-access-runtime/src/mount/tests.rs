// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use std::os::unix::fs::symlink;

fn private_root() -> TempDir {
    tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in("/tmp")
        .unwrap()
}

#[test]
fn unretained_mount_is_cleaned_up_on_failure_or_drop() {
    let root = private_root();
    let mount = MountDirectory::create(root.path()).unwrap();
    let path = mount.outer.clone();
    assert!(path.exists());
    assert_eq!(
        fs::metadata(mount.guest_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    drop(mount);
    assert!(!path.exists());
    assert!(root.path().exists());
}

#[test]
fn retained_inode_survives_restart_and_has_only_one_host_owner() {
    let root = private_root();
    let unrelated = root.path().join("unrelated");
    fs::write(&unrelated, "preserve").unwrap();
    let mut mount = MountDirectory::create(root.path()).unwrap();
    let id = mount.id().to_string();
    let path = mount.guest_path().to_path_buf();
    let inode = fs::metadata(&path).unwrap().ino();
    mount.retain();
    assert!(MountDirectory::recover(root.path(), &id).is_err());
    drop(mount);
    let recovered = MountDirectory::recover(root.path(), &id).unwrap();
    assert_eq!(recovered.guest_path(), path);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    recovered.remove().unwrap();
    assert!(!path.exists());
    assert_eq!(fs::read_to_string(unrelated).unwrap(), "preserve");
}

#[test]
fn recovery_rejects_missing_paths_traversal_and_symlinks_without_mutation() {
    let root = private_root();
    for id in [
        "../escape",
        "/tmp/escape",
        "",
        "not-a-mount",
        "00000000000000000000000000000000-../escape",
    ] {
        assert!(MountDirectory::recover(root.path(), id).is_err());
    }
    let missing = format!("{}-missing", Uuid::new_v4().simple());
    assert!(MountDirectory::recover(root.path(), &missing).is_err());
    assert!(!root.path().join(&missing).exists());
    let foreign = tempfile::tempdir().unwrap();
    symlink(foreign.path(), root.path().join(&missing)).unwrap();
    assert!(MountDirectory::recover(root.path(), &missing).is_err());
    assert!(fs::read_dir(foreign.path()).unwrap().next().is_none());
}

#[test]
fn ownership_markers_and_guest_mount_must_not_be_missing_or_replaceable() {
    let root = private_root();
    let mut mount = MountDirectory::create(root.path()).unwrap();
    let id = mount.id().to_string();
    let outer = mount.outer.clone();
    mount.retain();
    drop(mount);
    fs::remove_file(outer.join("lease")).unwrap();
    assert!(MountDirectory::recover(root.path(), &id).is_err());
    assert!(!outer.join("lease").exists());
    let foreign = root.path().join("foreign");
    fs::write(&foreign, "preserve").unwrap();
    symlink(&foreign, outer.join("lease")).unwrap();
    assert!(MountDirectory::recover(root.path(), &id).is_err());
    assert_eq!(fs::read_to_string(&foreign).unwrap(), "preserve");
    fs::remove_file(outer.join("lease")).unwrap();
    let lease = MountDirectory::lease(&outer, &id, true).unwrap();
    drop(lease);
    fs::set_permissions(outer.join("guest"), fs::Permissions::from_mode(0o777)).unwrap();
    assert!(MountDirectory::recover(root.path(), &id).is_err());
    fs::remove_dir(outer.join("guest")).unwrap();
    symlink(root.path(), outer.join("guest")).unwrap();
    assert!(MountDirectory::recover(root.path(), &id).is_err());
}

#[test]
fn nonprivate_roots_and_unexpected_endpoint_files_are_not_overwritten() {
    let root = private_root();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(MountDirectory::create(root.path()).is_err());
    assert!(fs::read_dir(root.path()).unwrap().next().is_none());
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let mount = MountDirectory::create(root.path()).unwrap();
    let socket = mount.guest_path().join("proxy.sock");
    fs::write(&socket, "preserve").unwrap();
    assert!(mount.clear_socket().is_err());
    assert_eq!(fs::read_to_string(&socket).unwrap(), "preserve");
    fs::remove_file(&socket).unwrap();
    symlink(root.path().join("other"), &socket).unwrap();
    assert!(mount.clear_socket().is_err());
    assert!(fs::symlink_metadata(socket)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[tokio::test]
async fn recovery_removes_a_stale_socket_only_after_claiming_the_mount() {
    let root = private_root();
    let mut mount = MountDirectory::create(root.path()).unwrap();
    let id = mount.id().to_string();
    let socket = mount.guest_path().join("proxy.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    mount.retain();
    assert!(MountDirectory::recover(root.path(), &id).is_err());
    drop(listener);
    drop(mount);
    assert!(socket.exists());
    let recovered = MountDirectory::recover(root.path(), &id).unwrap();
    recovered.clear_socket().unwrap();
    assert!(!socket.exists());
    recovered.remove().unwrap();
}
