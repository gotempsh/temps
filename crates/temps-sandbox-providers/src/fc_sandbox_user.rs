// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Host side of the Firecracker sandbox user (`temps_vm_agent::sandbox_user`).
//!
//! A sandbox VM boots with the sandbox user on its kernel command line, the
//! guest agent prepares it at boot, and the provider asks once the agent
//! answers whether it is ready. A ready user is recorded next to the VM
//! ([`SANDBOX_USER_FILE`]), and every default command then runs as it:
//! `uid:gid` [`SANDBOX_UID`]:[`SANDBOX_GID`], `HOME` [`SANDBOX_HOME`], the
//! same identity a Docker sandbox runs as. Root stays for the guest agent's
//! own maintenance and for an explicit `exec_as_root`.
//!
//! Workload (deployment) VMs never get the user: they run as the image's
//! own `USER`, resolved by the workload driver.

use std::path::Path;

use temps_agent_core::sandbox_user::{SANDBOX_GID, SANDBOX_HOME, SANDBOX_UID, SANDBOX_USER};
use temps_vm_agent::{Response, SandboxUser, SandboxUserStatus};

use crate::firecracker::FC_SANDBOX_NAME_PREFIX;
use crate::workload_vm::WORKLOAD_VM_ID_PREFIX;

/// File in a VM's directory recording the sandbox user its agent prepared.
/// Absent: commands run as root (a workload VM, or a sandbox restored from
/// a snapshot whose agent predates the sandbox user).
pub(crate) const SANDBOX_USER_FILE: &str = "sandbox-user.json";

/// The user every Firecracker sandbox runs as.
pub(crate) fn sandbox_user() -> SandboxUser {
    SandboxUser {
        uid: SANDBOX_UID,
        gid: SANDBOX_GID,
        name: SANDBOX_USER.to_string(),
        home: SANDBOX_HOME.to_string(),
    }
}

/// Whether the VM named `vm_name` is a sandbox (runs as the sandbox user)
/// rather than a workload VM (runs as its image's `USER`).
pub(crate) fn is_sandbox_vm(vm_name: &str) -> bool {
    vm_name
        .strip_prefix(FC_SANDBOX_NAME_PREFIX)
        .is_some_and(|id| !id.starts_with(WORKLOAD_VM_ID_PREFIX))
}

/// The kernel command-line parameter a VM boots with, if it is a sandbox.
pub(crate) fn boot_arg_for(vm_name: &str) -> Option<String> {
    is_sandbox_vm(vm_name).then(|| sandbox_user().boot_arg())
}

/// What the guest said about the sandbox user after boot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UserCheck {
    /// The user is ready; commands run as it.
    Ready(SandboxUser),
    /// The guest agent predates the request (a sandbox restored from an
    /// older snapshot carries its own agent). Commands keep running as
    /// root, as they did when the snapshot was taken.
    OlderAgent,
    /// The agent saw no user on its command line. Not expected for a
    /// sandbox; commands run as root.
    NotRequested,
}

/// Interpret the agent's answer to `Request::SandboxUser`. An error is the
/// reason the sandbox cannot run as its user, which fails the create.
pub(crate) fn interpret(response: Response) -> Result<UserCheck, String> {
    let wanted = sandbox_user();
    match response {
        Response::SandboxUser(SandboxUserStatus::Ready { user }) if user == wanted => {
            Ok(UserCheck::Ready(user))
        }
        Response::SandboxUser(SandboxUserStatus::Ready { user }) => Err(format!(
            "the guest prepared {}:{} ({}) instead of the sandbox user {}:{} ({})",
            user.uid, user.gid, user.name, wanted.uid, wanted.gid, wanted.name
        )),
        Response::SandboxUser(SandboxUserStatus::Failed { reason }) => Err(reason),
        Response::SandboxUser(SandboxUserStatus::NotRequested) => Ok(UserCheck::NotRequested),
        Response::Err { message } if crate::firecracker::is_unknown_request(&message) => {
            Ok(UserCheck::OlderAgent)
        }
        Response::Err { message } => Err(message),
        other => Err(format!("unexpected agent response: {other:?}")),
    }
}

/// Record (or clear) the user the VM in `vm_dir` runs commands as.
pub(crate) fn record(vm_dir: &Path, user: Option<&SandboxUser>) -> std::io::Result<()> {
    let path = vm_dir.join(SANDBOX_USER_FILE);
    match user {
        Some(user) => {
            let json = serde_json::to_vec(user).map_err(std::io::Error::other)?;
            std::fs::write(path, json)
        }
        None => match std::fs::remove_file(path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        },
    }
}

/// The uid default commands run as in the VM in `vm_dir`: the recorded
/// sandbox user, or `None` (root) when none is recorded.
pub(crate) fn recorded_uid(vm_dir: &Path) -> Option<u32> {
    let data = std::fs::read(vm_dir.join(SANDBOX_USER_FILE)).ok()?;
    serde_json::from_slice::<SandboxUser>(&data)
        .ok()
        .map(|user| user.uid)
}

/// The uid for `exec_as_user(user)`. The guest agent runs commands by uid,
/// so a name it cannot map without the image's `passwd` is refused rather
/// than run as someone else.
pub(crate) fn exec_uid_for(user: &str) -> Result<u32, String> {
    let spec = user.trim();
    let (name, group) = match spec.split_once(':') {
        Some((name, group)) => (name, Some(group)),
        None => (spec, None),
    };
    let uid = match name {
        "root" => 0,
        name if name == SANDBOX_USER => SANDBOX_UID,
        name => name.parse::<u32>().map_err(|_| {
            format!(
                "Firecracker sandboxes run commands by uid; '{spec}' names no uid (use a \
                 number, 'root' or '{SANDBOX_USER}')"
            )
        })?,
    };
    // The agent runs a uid with its own group (the sandbox user's, or
    // gid = uid), so a different group is refused rather than ignored.
    if let Some(group) = group {
        let expected = if uid == SANDBOX_UID { SANDBOX_GID } else { uid };
        let gid = match group {
            "root" => 0,
            group if group == SANDBOX_USER => SANDBOX_GID,
            group => group.parse::<u32>().map_err(|_| {
                format!("Firecracker sandboxes run commands by gid; '{spec}' names no gid")
            })?,
        };
        if gid != expected {
            return Err(format!(
                "Firecracker sandboxes run uid {uid} with gid {expected}; '{spec}' asks for gid \
                 {gid}"
            ));
        }
    }
    Ok(uid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandboxes_get_the_user_and_workload_vms_do_not() {
        assert!(is_sandbox_vm(&format!("{FC_SANDBOX_NAME_PREFIX}42")));
        assert!(is_sandbox_vm(&format!(
            "{FC_SANDBOX_NAME_PREFIX}sandbox-my-app"
        )));
        let workload = format!("{FC_SANDBOX_NAME_PREFIX}{WORKLOAD_VM_ID_PREFIX}my-app-web-1");
        assert!(!is_sandbox_vm(&workload));
        assert_eq!(boot_arg_for(&workload), None);
        assert_eq!(
            boot_arg_for(&format!("{FC_SANDBOX_NAME_PREFIX}42")).as_deref(),
            Some("temps.sandbox_user=1000:1000:temps:/home/temps")
        );
    }

    #[test]
    fn the_boot_argument_names_the_docker_sandbox_identity() {
        let user = sandbox_user();
        assert_eq!((user.uid, user.gid), (SANDBOX_UID, SANDBOX_GID));
        assert_eq!(user.home, SANDBOX_HOME);
        let arg = user.boot_arg();
        assert_eq!(SandboxUser::from_cmdline(&arg), Some(Ok(user)));
    }

    #[test]
    fn a_ready_user_is_accepted_and_anything_else_explains_itself() {
        assert_eq!(
            interpret(Response::SandboxUser(SandboxUserStatus::Ready {
                user: sandbox_user()
            })),
            Ok(UserCheck::Ready(sandbox_user()))
        );
        let mut other = sandbox_user();
        other.uid = 1001;
        let error = interpret(Response::SandboxUser(SandboxUserStatus::Ready {
            user: other,
        }))
        .unwrap_err();
        assert!(error.contains("1001:1000"), "{error}");

        let reason = "the image already has a user 'temps' with uid 1001; the sandbox needs \
                      'temps' to be uid 1000";
        assert_eq!(
            interpret(Response::SandboxUser(SandboxUserStatus::Failed {
                reason: reason.to_string()
            })),
            Err(reason.to_string())
        );
        assert_eq!(
            interpret(Response::SandboxUser(SandboxUserStatus::NotRequested)),
            Ok(UserCheck::NotRequested)
        );
        assert_eq!(
            interpret(Response::Err {
                message: "bad request: unknown variant `sandbox_user`, expected one of `ping`"
                    .to_string()
            }),
            Ok(UserCheck::OlderAgent)
        );
        assert_eq!(
            interpret(Response::Err {
                message: "boom".to_string()
            }),
            Err("boom".to_string())
        );
        assert!(interpret(Response::Pong).is_err());
    }

    #[test]
    fn the_recorded_user_decides_the_default_exec_uid() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            recorded_uid(dir.path()),
            None,
            "nothing recorded runs as root"
        );
        record(dir.path(), Some(&sandbox_user())).unwrap();
        assert_eq!(recorded_uid(dir.path()), Some(SANDBOX_UID));
        record(dir.path(), None).unwrap();
        assert_eq!(recorded_uid(dir.path()), None);
        record(dir.path(), None).unwrap();

        std::fs::write(dir.path().join(SANDBOX_USER_FILE), "not json").unwrap();
        assert_eq!(recorded_uid(dir.path()), None);
    }

    #[test]
    fn exec_as_user_maps_names_and_numbers_to_a_uid() {
        for (spec, uid) in [
            ("root", 0),
            ("0", 0),
            ("0:0", 0),
            ("root:root", 0),
            ("temps", SANDBOX_UID),
            ("temps:temps", SANDBOX_UID),
            ("1000:1000", SANDBOX_UID),
            ("4242", 4242),
            ("4242:4242", 4242),
        ] {
            assert_eq!(exec_uid_for(spec), Ok(uid), "{spec}");
        }
        assert!(exec_uid_for("node").unwrap_err().contains("names no uid"));
        assert!(exec_uid_for("1000:0").unwrap_err().contains("gid 1000"));
        assert!(exec_uid_for("4242:staff")
            .unwrap_err()
            .contains("names no gid"));
    }
}
