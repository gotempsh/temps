// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::{Response, SandboxUserStatus};

fn temps() -> SandboxUser {
    SandboxUser {
        uid: 1000,
        gid: 1000,
        name: "temps".to_string(),
        home: "/home/temps".to_string(),
    }
}

/// The current process's own identity: a test can chown to it without
/// privileges.
fn me() -> SandboxUser {
    SandboxUser {
        uid: unsafe { libc::getuid() },
        gid: unsafe { libc::getgid() },
        name: "temps".to_string(),
        home: "/home/temps".to_string(),
    }
}

/// An id the test runner's own uid and gid are not, for fixture accounts
/// that must not collide with the runner (a CI service account may well be
/// 65534 or 4242).
fn an_id_other_than(taken: &[u32]) -> u32 {
    (4242..)
        .find(|id| !taken.contains(id))
        .expect("an unused id")
}

#[test]
fn the_boot_argument_round_trips_through_the_kernel_command_line() {
    let user = temps();
    assert_eq!(
        user.boot_arg(),
        "temps.sandbox_user=1000:1000:temps:/home/temps"
    );
    let cmdline = format!(
        "console=ttyS0 reboot=k panic=1 pci=off init=/sbin/temps-vm-agent {}",
        user.boot_arg()
    );
    assert_eq!(SandboxUser::from_cmdline(&cmdline), Some(Ok(user)));
}

#[test]
fn a_command_line_without_the_argument_names_no_user() {
    assert_eq!(
        SandboxUser::from_cmdline("console=ttyS0 init=/sbin/temps-vm-agent"),
        None
    );
}

#[test]
fn malformed_boot_arguments_are_refused_with_the_reason() {
    for (value, reason) in [
        ("1000:1000:temps", "expected four"),
        ("x:1000:temps:/home/temps", "uid is not a number"),
        ("1000:y:temps:/home/temps", "gid is not a number"),
        ("0:0:root:/root", "uid 0 is root"),
        ("1000:1000:Temps:/home/temps", "name must be"),
        ("1000:1000:temps:home/temps", "home must be"),
        ("1000:1000:temps:/home/../etc", "home must be"),
    ] {
        let error = SandboxUser::from_cmdline(&format!("{SANDBOX_USER_BOOT_ARG}={value}"))
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains(reason), "{value}: {error}");
        assert!(error.to_string().contains(value), "{value}: {error}");
    }
}

#[test]
fn a_missing_user_is_added_to_passwd() {
    let passwd = "root:x:0:0:root:/root:/bin/sh\nnobody:x:65534:65534::/:/sbin/nologin";
    let PasswdEntry::Added(updated) = ensure_passwd_entry(passwd, &temps()).unwrap() else {
        panic!("expected an added entry");
    };
    assert_eq!(
        updated,
        "root:x:0:0:root:/root:/bin/sh\nnobody:x:65534:65534::/:/sbin/nologin\n\
         temps:x:1000:1000::/home/temps:/bin/sh\n"
    );
    assert_eq!(
        ensure_passwd_entry(&updated, &temps()).unwrap(),
        PasswdEntry::Present,
        "a second boot changes nothing"
    );
}

#[test]
fn an_image_without_account_files_gets_them() {
    let PasswdEntry::Added(passwd) = ensure_passwd_entry("", &temps()).unwrap() else {
        panic!("expected an added entry");
    };
    assert_eq!(passwd, "temps:x:1000:1000::/home/temps:/bin/sh\n");
    assert_eq!(
        ensure_group_entry("", "temps", 1000).unwrap().as_deref(),
        Some("temps:x:1000:\n")
    );
}

#[test]
fn the_uid_under_another_name_is_reused_not_duplicated() {
    let passwd = "root:x:0:0:root:/root:/bin/sh\nnode:x:1000:1000::/home/node:/bin/sh\n";
    assert_eq!(
        ensure_passwd_entry(passwd, &temps()).unwrap(),
        PasswdEntry::PresentAs("node".to_string())
    );
    let group = "root:x:0:\nnode:x:1000:\n";
    assert_eq!(ensure_group_entry(group, "temps", 1000).unwrap(), None);
}

#[test]
fn the_name_taken_by_another_uid_is_a_typed_error() {
    let passwd = "root:x:0:0:root:/root:/bin/sh\ntemps:x:1001:1001::/home/temps:/bin/sh\n";
    let error = ensure_passwd_entry(passwd, &temps()).unwrap_err();
    assert_eq!(
        error,
        SandboxUserError::NameTakenByOtherUid {
            name: "temps".to_string(),
            existing_uid: 1001,
            wanted_uid: 1000,
        }
    );
    assert!(error.to_string().contains("uid 1001"), "{error}");

    let error = ensure_group_entry("temps:x:1001:\n", "temps", 1000).unwrap_err();
    assert!(
        matches!(
            error,
            SandboxUserError::GroupNameTakenByOtherGid {
                existing_gid: 1001,
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn comments_and_malformed_lines_are_ignored() {
    let passwd = "# comment\n\ngarbage\ntemps:x:notanumber:1000::/home/temps:/bin/sh\n";
    assert!(matches!(
        ensure_passwd_entry(passwd, &temps()).unwrap(),
        PasswdEntry::Added(_)
    ));
}

#[test]
fn exec_identity_follows_the_requested_uid() {
    let passwd = "root:x:0:0:root:/root:/bin/sh\nnode:x:1001:1001::/home/node:/bin/sh\n";
    let user = temps();
    assert_eq!(
        exec_identity(None, Some(&user), passwd),
        (None, "/root".to_string())
    );
    assert_eq!(
        exec_identity(Some(0), Some(&user), passwd),
        (Some((0, 0)), "/root".to_string())
    );
    assert_eq!(
        exec_identity(Some(1000), Some(&user), passwd),
        (Some((1000, 1000)), "/home/temps".to_string())
    );
    assert_eq!(
        exec_identity(Some(1001), Some(&user), passwd),
        (Some((1001, 1001)), "/home/node".to_string())
    );
    assert_eq!(
        exec_identity(Some(4242), None, passwd),
        (Some((4242, 4242)), "/".to_string())
    );
}

#[test]
fn writes_into_the_home_or_workspace_run_as_the_user() {
    let user = temps();
    for path in [
        "/home/temps",
        "/home/temps/.claude/settings.json",
        "/workspace",
        "/workspace/src/main.rs",
        // The same files, spelled the ways the kernel also accepts.
        "//workspace/config",
        "/workspace//src/./main.rs",
        "/home/temps/",
        "workspace/config",
    ] {
        assert_eq!(is_user_file(path, &user, "/workspace"), Ok(true), "{path}");
    }
    for path in [
        "/etc/temps/token",
        "/home/tempsx/file",
        "/workspacex",
        "/root/.bashrc",
    ] {
        assert_eq!(is_user_file(path, &user, "/workspace"), Ok(false), "{path}");
    }
    assert_eq!(
        is_user_file("/workspace/../etc/passwd", &user, "/workspace"),
        Err(OwnedWriteError::ParentComponent {
            path: "/workspace/../etc/passwd".to_string()
        })
    );
}

#[test]
fn file_ops_round_trip_through_their_arguments() {
    for op in [
        FileOp::Write {
            path: "/workspace/a b.txt".to_string(),
            mode: 0o600,
        },
        FileOp::Mkdir {
            path: "/home/temps/.config".to_string(),
            mode: 0o755,
        },
    ] {
        assert_eq!(FileOp::parse(&op.args()), Ok(op));
    }
    assert!(FileOp::parse(&["chmod".into(), "/x".into(), "644".into()]).is_err());
    assert!(FileOp::parse(&["write".into(), "/x".into(), "9z".into()]).is_err());
    assert!(FileOp::parse(&["write".into()]).is_err());
}

#[test]
fn a_file_op_writes_with_its_mode_and_creates_parents() {
    let dir = tempfile_dir();
    let path = dir.join("nested/deeper/file.txt");
    run_file_op(
        &FileOp::Write {
            path: path.display().to_string(),
            mode: 0o600,
        },
        &b"hello"[..],
        1024,
    )
    .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"hello");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let error = run_file_op(
        &FileOp::Write {
            path: dir.join("big").display().to_string(),
            mode: 0o644,
        },
        &[0u8; 16][..],
        8,
    )
    .unwrap_err();
    assert!(error.to_string().contains("exceed 8 bytes"), "{error}");
    assert!(
        !dir.join("big").exists(),
        "an oversized write leaves no file"
    );

    let sub = dir.join("made/here");
    run_file_op(
        &FileOp::Mkdir {
            path: sub.display().to_string(),
            mode: 0o750,
        },
        std::io::empty(),
        0,
    )
    .unwrap();
    assert_eq!(
        std::fs::metadata(&sub).unwrap().permissions().mode() & 0o777,
        0o750
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn prepare_adds_the_accounts_creates_the_home_and_is_idempotent() {
    let root = tempfile_dir();
    std::fs::create_dir_all(root.join("etc")).unwrap();
    let user = me();
    // Accounts that cannot collide with the test runner's own uid and gid.
    let other = an_id_other_than(&[user.uid, user.gid]);
    std::fs::write(
        root.join("etc/passwd"),
        format!("nobody:x:{other}:{other}::/:/sbin/nologin\n"),
    )
    .unwrap();
    std::fs::write(root.join("etc/group"), format!("nogroup:x:{other}:\n")).unwrap();
    std::fs::create_dir_all(root.join("workspace/src")).unwrap();
    std::fs::write(root.join("workspace/src/main.rs"), "fn main() {}").unwrap();

    let report = prepare(&root, &user, "/workspace").unwrap();
    assert!(report.added_user && report.added_group, "{report:?}");
    let passwd = std::fs::read_to_string(root.join("etc/passwd")).unwrap();
    assert!(
        passwd.ends_with(&format!(
            "temps:x:{}:{}::/home/temps:/bin/sh\n",
            user.uid, user.gid
        )),
        "{passwd}"
    );
    assert!(root.join("home/temps").is_dir());
    for path in [
        "home/temps",
        "workspace",
        "workspace/src",
        "workspace/src/main.rs",
    ] {
        let meta = std::fs::symlink_metadata(root.join(path)).unwrap();
        assert_eq!((meta.uid(), meta.gid()), (user.uid, user.gid), "{path}");
    }

    let again = prepare(&root, &user, "/workspace").unwrap();
    assert_eq!(
        again,
        PrepareReport::default(),
        "a second boot adds nothing and chowns nothing"
    );
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn prepare_refuses_a_conflicting_account_without_touching_files() {
    let root = tempfile_dir();
    std::fs::create_dir_all(root.join("etc")).unwrap();
    let user = me();
    let other = an_id_other_than(&[user.uid]);
    let passwd =
        format!("root:x:0:0:root:/root:/bin/sh\ntemps:x:{other}:{other}::/home/temps:/bin/sh\n");
    std::fs::write(root.join("etc/passwd"), &passwd).unwrap();
    // The image has no group file: the group the user would need is
    // missing too, and must not be created for a user that is refused.

    let error = prepare(&root, &user, "/workspace").unwrap_err();
    assert!(
        matches!(
            error,
            SandboxUserError::NameTakenByOtherUid { existing_uid, .. } if existing_uid == other
        ),
        "{error:?}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("etc/passwd")).unwrap(),
        passwd
    );
    assert!(
        !root.join("etc/group").exists(),
        "a refused user leaves no group behind"
    );
    assert!(!root.join("home/temps").exists());
    std::fs::remove_dir_all(&root).unwrap();
}

/// A host write replaces the file: one left read-only can be rewritten, and
/// a symlink planted at the target is replaced rather than written through.
#[test]
fn a_write_replaces_a_read_only_file_and_a_planted_symlink() {
    let dir = tempfile_dir();
    let path = dir.join("token");
    let write = |contents: &'static [u8]| {
        run_file_op(
            &FileOp::Write {
                path: path.display().to_string(),
                mode: 0o400,
            },
            contents,
            1024,
        )
    };
    write(b"first").unwrap();
    write(b"second").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"second");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o400
    );

    let elsewhere = dir.join("elsewhere");
    std::fs::write(&elsewhere, b"keep").unwrap();
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
    write(b"third").unwrap();
    assert!(!std::fs::symlink_metadata(&path)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(std::fs::read(&elsewhere).unwrap(), b"keep");
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        2,
        "no staged file is left behind"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The size limit applies to what is read: a symlink to a large file, or to
/// something that is not a file at all, is refused, not read.
#[test]
fn an_account_file_is_measured_as_opened() {
    let dir = tempfile_dir();
    assert_eq!(read_account_file(&dir.join("missing")).unwrap(), "");

    let big = dir.join("big");
    std::fs::write(&big, vec![b'x'; MAX_ACCOUNT_FILE_BYTES as usize + 1]).unwrap();
    let link = dir.join("passwd");
    std::os::unix::fs::symlink(&big, &link).unwrap();
    assert!(matches!(
        read_account_file(&link),
        Err(SandboxUserError::AccountFileTooLarge { .. })
    ));

    let to_dir = dir.join("group");
    std::os::unix::fs::symlink(&dir, &to_dir).unwrap();
    assert!(matches!(
        read_account_file(&to_dir),
        Err(SandboxUserError::AccountFileNotRegular { .. })
    ));

    let small = dir.join("small");
    std::fs::write(&small, "root:x:0:0::/root:/bin/sh\n").unwrap();
    assert_eq!(
        read_account_file(&small).unwrap(),
        "root:x:0:0::/root:/bin/sh\n"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The converter to hand back is recorded before the walk and `done` only
/// after it, so a walk cut short (by which point the agent binary may
/// already be root's) resumes for the same owner at the next boot.
#[test]
fn owner_normalisation_resumes_until_it_has_finished() {
    let dir = tempfile_dir();
    let record = dir.join("var/lib/temps-vm-agent/rootfs-owner");

    // A rootfs extracted as root: nothing to do, nothing recorded.
    assert_eq!(
        normalize_rootfs_owner(&record, (0, 0), |_| panic!("no walk")).unwrap(),
        None
    );
    assert_eq!(
        read_normalization(&record).unwrap(),
        Normalization::NotStarted
    );

    // The walk fails part way: the record still names the converter.
    let error = normalize_rootfs_owner(&record, (1000, 1000), |converter| {
        assert_eq!(converter, (1000, 1000));
        assert_eq!(
            read_normalization(&record).unwrap(),
            Normalization::Pending {
                uid: 1000,
                gid: 1000
            },
            "recorded before anything changes"
        );
        Err(SandboxUserError::Io {
            operation: "chown",
            path: "/usr/bin/env".to_string(),
            reason: "interrupted".to_string(),
        })
    })
    .unwrap_err();
    assert!(error.to_string().contains("interrupted"), "{error}");

    // Next boot: the binary is root's now, but the walk runs again for the
    // recorded converter, and finishes.
    assert_eq!(
        normalize_rootfs_owner(&record, (0, 0), |converter| {
            assert_eq!(converter, (1000, 1000));
            Ok(7)
        })
        .unwrap(),
        Some(7)
    );
    assert_eq!(read_normalization(&record).unwrap(), Normalization::Done);
    assert_eq!(
        normalize_rootfs_owner(&record, (1000, 1000), |_| panic!("already done")).unwrap(),
        None
    );

    std::fs::write(&record, "garbage").unwrap();
    assert!(matches!(
        read_normalization(&record),
        Err(SandboxUserError::BadNormalizationRecord { .. })
    ));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn chown_tree_does_not_follow_symlinks_out_of_the_tree() {
    let root = tempfile_dir();
    let outside = tempfile_dir();
    std::fs::write(outside.join("secret"), "x").unwrap();
    std::fs::create_dir_all(root.join("tree")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("tree/link")).unwrap();
    let user = me();

    chown_tree(&root.join("tree"), user.uid, user.gid).unwrap();
    // The walk lchowns the link itself and never descends into it.
    let listed: Vec<_> = std::fs::read_dir(root.join("tree")).unwrap().collect();
    assert_eq!(listed.len(), 1);
    assert!(std::fs::symlink_metadata(root.join("tree/link"))
        .unwrap()
        .file_type()
        .is_symlink());
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::remove_dir_all(&outside).unwrap();
}

#[test]
fn the_converter_walk_skips_the_users_trees_and_symlinks() {
    let root = tempfile_dir();
    for dir in ["etc", "sbin", "home/temps/.cache", "workspace/src"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    for file in [
        "etc/passwd",
        "sbin/temps-vm-agent",
        "home/temps/.bashrc",
        "workspace/src/a",
    ] {
        std::fs::write(root.join(file), "x").unwrap();
    }
    let outside = tempfile_dir();
    std::fs::write(outside.join("host-file"), "x").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("etc/escape")).unwrap();
    let me = me();

    let home = root.join("home/temps");
    let work = root.join("workspace");
    let mut seen = Vec::new();
    walk_owned_by(&root, me.uid, &[&home, &work], |path, _| {
        seen.push(path.strip_prefix(&root).unwrap().display().to_string());
        Ok(())
    })
    .unwrap();
    seen.sort();
    assert_eq!(
        seen,
        [
            "",
            "etc",
            "etc/escape",
            "etc/passwd",
            "home",
            "sbin",
            "sbin/temps-vm-agent"
        ],
        "system files are visited; the user's trees and the link's target are not"
    );

    let mut none = 0;
    walk_owned_by(&root, me.uid.wrapping_add(1), &[], |_, _| {
        none += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(none, 0, "entries owned by another uid are left alone");
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::remove_dir_all(&outside).unwrap();
}

#[test]
fn the_status_reaches_the_host_in_the_response_frame() {
    for status in [
        SandboxUserStatus::NotRequested,
        SandboxUserStatus::Ready { user: temps() },
        SandboxUserStatus::Failed {
            reason: "the image already has a user 'temps' with uid 1001".to_string(),
        },
    ] {
        let json = serde_json::to_string(&Response::SandboxUser(status.clone())).unwrap();
        let Response::SandboxUser(back) = serde_json::from_str(&json).unwrap() else {
            panic!("{json}");
        };
        assert_eq!(back, status, "{json}");
    }
}

/// A fresh, empty directory under the system temp dir.
fn tempfile_dir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "temps-vm-agent-sandbox-user-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
