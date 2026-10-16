// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Whether a restart of the Temps service takes its sandbox VMs down.
//!
//! Firecracker and microsandbox VMMs are children of `temps serve`, so they
//! live in the service's cgroup. systemd's default `KillMode=control-group`
//! kills everything in that cgroup on `systemctl restart`, VMs included. The
//! `temps serve` unit is written by the operator, so Temps cannot fix it; it
//! can say so at startup, with the exact change, instead of leaving the
//! operator to find out from a dead terminal.

// The systemd probing is Linux-only; the parsing helpers it uses are
// platform-independent and unit-tested everywhere.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::time::Duration;

/// How long `systemctl show` may take before the check gives up silently.
const SYSTEMCTL_DEADLINE: Duration = Duration::from_secs(3);

/// The systemd service unit `cgroup` (the contents of `/proc/<pid>/cgroup`)
/// places the process in, if any. A process in a nested cgroup below the
/// unit's own, or outside systemd, has none.
fn unit_from_cgroup(cgroup: &str) -> Option<String> {
    cgroup.lines().find_map(|line| {
        let unit = line.rsplit('/').next()?;
        let valid = unit.ends_with(".service")
            && !unit.starts_with('-')
            && unit
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@' | ':'));
        valid.then(|| unit.to_string())
    })
}

/// What to tell the operator for a unit with this `KillMode`, or `None` when
/// a restart leaves the VMs running (`process`, `none`) or the mode is not
/// one systemd documents (nothing reliable to say).
fn kill_mode_advice(unit: &str, kill_mode: &str) -> Option<String> {
    match kill_mode {
        "control-group" | "mixed" => Some(format!(
            "{unit} has KillMode={kill_mode}, so `systemctl restart {unit}` kills every running \
             Firecracker/microsandbox VM along with Temps. Run `sudo systemctl edit {unit}`, add \
             `[Service]` and `KillMode=process`, then restart it: VMs keep running across \
             restarts and Temps finds them again by name."
        )),
        _ => None,
    }
}

/// Log a warning when this process runs in a systemd service whose restart
/// would kill its sandbox VMs. Silent when Temps is not under systemd, when
/// `systemctl` is missing or slow, or when the unit already spares the VMs:
/// this is advice, never a failure.
#[cfg(target_os = "linux")]
pub async fn warn_if_restart_kills_vms() {
    let Ok(cgroup) = tokio::fs::read_to_string("/proc/self/cgroup").await else {
        return;
    };
    let Some(unit) = unit_from_cgroup(&cgroup) else {
        return;
    };
    let output = tokio::time::timeout(
        SYSTEMCTL_DEADLINE,
        tokio::process::Command::new("systemctl")
            .args(["show", "--property=KillMode", "--value", unit.as_str()])
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let kill_mode = match output {
        Ok(Ok(output)) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        _ => {
            tracing::debug!(unit = %unit, "could not read KillMode from systemd; skipping the VM survival check");
            return;
        }
    };
    match kill_mode_advice(&unit, &kill_mode) {
        Some(advice) => tracing::warn!(unit = %unit, kill_mode = %kill_mode, "{advice}"),
        None => tracing::info!(
            unit = %unit,
            kill_mode = %kill_mode,
            "Sandbox VMs survive a restart of this service"
        ),
    }
}

#[cfg(not(target_os = "linux"))]
pub async fn warn_if_restart_kills_vms() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_service_cgroup_names_its_unit() {
        assert_eq!(
            unit_from_cgroup("0::/system.slice/temps.service\n").as_deref(),
            Some("temps.service")
        );
        // cgroup v1: the systemd hierarchy is one of several lines.
        let v1 =
            "12:memory:/system.slice/temps.service\n1:name=systemd:/system.slice/temps.service\n";
        assert_eq!(unit_from_cgroup(v1).as_deref(), Some("temps.service"));
    }

    #[test]
    fn a_process_outside_a_service_unit_has_none() {
        // A shell in a session scope, and a nested cgroup below a unit.
        assert_eq!(
            unit_from_cgroup("0::/user.slice/user-1000.slice/session-3.scope\n"),
            None
        );
        assert_eq!(
            unit_from_cgroup("0::/system.slice/temps.service/payload\n"),
            None
        );
        assert_eq!(unit_from_cgroup(""), None);
    }

    #[test]
    fn a_unit_name_that_could_be_a_flag_or_shell_is_refused() {
        assert_eq!(unit_from_cgroup("0::/system.slice/-x.service\n"), None);
        assert_eq!(unit_from_cgroup("0::/system.slice/a b.service\n"), None);
        assert_eq!(unit_from_cgroup("0::/system.slice/a;b.service\n"), None);
    }

    #[test]
    fn only_modes_that_kill_the_whole_group_get_advice() {
        let advice = kill_mode_advice("temps.service", "control-group").unwrap();
        assert!(
            advice.contains("sudo systemctl edit temps.service"),
            "{advice}"
        );
        assert!(advice.contains("KillMode=process"), "{advice}");
        assert!(kill_mode_advice("temps.service", "mixed").is_some());
        assert_eq!(kill_mode_advice("temps.service", "process"), None);
        assert_eq!(kill_mode_advice("temps.service", "none"), None);
        assert_eq!(kill_mode_advice("temps.service", ""), None);
    }
}
