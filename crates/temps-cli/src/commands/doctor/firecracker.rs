// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The Firecracker section of `temps doctor`: shown once `temps firecracker
//! setup` has provisioned the data directory, with the command that fixes
//! each problem. The facts are read from the host first and judged by
//! [`assess`], which is pure so every verdict is tested.

use std::io::Read;
use std::path::{Path, PathBuf};

use super::CheckResult;
use crate::commands::firecracker::{
    BRIDGE_NAME, FIRECRACKER_VERSION, KERNEL_VERSION, LAUNCHER_CONFIG_PATH, LAUNCHER_UNIT_NAME,
    TAP_NAME_PREFIX,
};

/// What setup recorded, read from `firecracker/state.json`.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Deserialize)]
pub(super) struct RecordedSetup {
    #[serde(default)]
    pub firecracker_version: String,
    #[serde(default)]
    pub kernel_version: String,
    #[serde(default)]
    pub network_ready: bool,
    #[serde(default)]
    pub tap_count: u32,
    #[serde(default)]
    pub launcher_socket: Option<String>,
}

/// Whether the launcher's root-owned copy of a binary matches the one it
/// was staged from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CopyState {
    Current,
    Stale,
    Unreadable(String),
}

/// Everything the section judges, gathered from the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Facts {
    pub data_dir: PathBuf,
    pub recorded: RecordedSetup,
    /// Why the server could not use the backend, as the server itself
    /// decides it (checked as the user running doctor).
    pub unavailable_reason: Option<String>,
    /// `systemctl is-active` of the launcher unit.
    pub launcher_active: bool,
    /// The launcher's copy of `temps` against the binary running doctor.
    pub temps_copy: Option<CopyState>,
    /// The launcher's copies of `firecracker` and `jailer` against the ones
    /// setup installed in the data directory.
    pub vmm_copies: Option<CopyState>,
    pub bridge_present: bool,
    /// TAP devices present out of the `tap_count` setup created.
    pub taps_present: u32,
}

/// The data directory's setup state, or `None` when Firecracker was never
/// set up there (the section is then left out).
pub(super) fn recorded_setup(data_dir: &Path) -> Option<RecordedSetup> {
    let raw = std::fs::read(data_dir.join("firecracker/state.json")).ok()?;
    Some(serde_json::from_slice(&raw).unwrap_or_default())
}

pub(super) fn gather(data_dir: &Path, recorded: RecordedSetup) -> Facts {
    let config = temps_agents::sandbox::firecracker::FirecrackerSandboxConfig::from_data_dir(
        data_dir.to_path_buf(),
    );
    let launcher_active = std::process::Command::new("systemctl")
        .args(["is-active", "--quiet", LAUNCHER_UNIT_NAME])
        .status()
        .is_ok_and(|status| status.success());
    let launcher = std::fs::read(LAUNCHER_CONFIG_PATH).ok().and_then(|raw| {
        serde_json::from_slice::<temps_agents::sandbox::fc_launcher::LauncherConfig>(&raw).ok()
    });
    let (temps_copy, vmm_copies) = match &launcher {
        Some(launcher) => {
            let libdir = launcher
                .firecracker_bin
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default();
            let temps_copy = match std::env::current_exe() {
                Ok(exe) => same_file_contents(&exe, &libdir.join("temps")),
                Err(e) => CopyState::Unreadable(format!("cannot locate this temps binary: {e}")),
            };
            let bin = data_dir.join("firecracker/bin");
            let vmm = [
                (bin.join("firecracker"), launcher.firecracker_bin.clone()),
                (bin.join("jailer"), launcher.jailer_bin.clone()),
            ]
            .iter()
            .map(|(installed, copy)| same_file_contents(installed, copy))
            .fold(CopyState::Current, |worst, next| match (worst, next) {
                (CopyState::Unreadable(e), _) | (_, CopyState::Unreadable(e)) => {
                    CopyState::Unreadable(e)
                }
                (CopyState::Stale, _) | (_, CopyState::Stale) => CopyState::Stale,
                _ => CopyState::Current,
            });
            (Some(temps_copy), Some(vmm))
        }
        None => (None, None),
    };
    let net = Path::new("/sys/class/net");
    let taps_present = (0..recorded.tap_count)
        .filter(|i| net.join(format!("{TAP_NAME_PREFIX}{i}")).exists())
        .count() as u32;
    Facts {
        data_dir: data_dir.to_path_buf(),
        unavailable_reason: temps_agents::sandbox::firecracker::firecracker_unavailable_reason(
            &config,
        ),
        launcher_active,
        temps_copy,
        vmm_copies,
        bridge_present: net.join(BRIDGE_NAME).exists(),
        taps_present,
        recorded,
    }
}

/// Whether `a` and `b` hold the same bytes, read in chunks so comparing the
/// temps binary does not load it into memory.
fn same_file_contents(a: &Path, b: &Path) -> CopyState {
    let open = |path: &Path| {
        std::fs::File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
    };
    let (mut fa, mut fb) = match (open(a), open(b)) {
        (Ok(fa), Ok(fb)) => (fa, fb),
        (Err(e), _) | (_, Err(e)) => return CopyState::Unreadable(e),
    };
    match (fa.metadata(), fb.metadata()) {
        (Ok(ma), Ok(mb)) if ma.len() != mb.len() => return CopyState::Stale,
        (Ok(_), Ok(_)) => {}
        (Err(e), _) | (_, Err(e)) => return CopyState::Unreadable(e.to_string()),
    }
    let mut ba = vec![0u8; 1 << 16];
    let mut bb = vec![0u8; 1 << 16];
    loop {
        let na = match fa.read(&mut ba) {
            Ok(n) => n,
            Err(e) => return CopyState::Unreadable(format!("read {}: {e}", a.display())),
        };
        if na == 0 {
            return CopyState::Current;
        }
        if let Err(e) = fb.read_exact(&mut bb[..na]) {
            return CopyState::Unreadable(format!("read {}: {e}", b.display()));
        }
        if ba[..na] != bb[..na] {
            return CopyState::Stale;
        }
    }
}

/// The section's checks, each failing one with the command that fixes it.
pub(super) fn assess(facts: &Facts) -> Vec<(String, CheckResult)> {
    let setup = format!(
        "sudo temps firecracker setup --data-dir {}",
        facts.data_dir.display()
    );
    let mut checks = Vec::new();

    checks.push((
        "Backend".to_string(),
        match &facts.unavailable_reason {
            None => CheckResult::Pass("usable by this user; every VM runs under the jailer".into()),
            Some(reason) => CheckResult::Fail(reason.clone()),
        },
    ));

    let pinned = facts.recorded.firecracker_version == FIRECRACKER_VERSION
        && facts.recorded.kernel_version == KERNEL_VERSION;
    checks.push((
        "Versions".to_string(),
        if pinned {
            CheckResult::Pass(format!(
                "Firecracker {FIRECRACKER_VERSION}, guest kernel {KERNEL_VERSION}"
            ))
        } else {
            CheckResult::Warn(format!(
                "installed Firecracker {} with guest kernel {}, but this temps pins {} with {}; \
                 run `{setup}`",
                or_unknown(&facts.recorded.firecracker_version),
                or_unknown(&facts.recorded.kernel_version),
                FIRECRACKER_VERSION,
                KERNEL_VERSION
            ))
        },
    ));

    if facts.recorded.launcher_socket.is_none() {
        checks.push((
            "Jailer launcher".to_string(),
            CheckResult::Fail(format!(
                "not installed, so microVMs cannot run: they only run under the jailer. Run \
                 `{setup}`"
            )),
        ));
    } else {
        checks.push((
            "Jailer launcher".to_string(),
            if facts.launcher_active {
                CheckResult::Pass(format!("{LAUNCHER_UNIT_NAME} is running"))
            } else {
                CheckResult::Fail(format!(
                    "{LAUNCHER_UNIT_NAME} is not running, so no microVM can start; run `sudo \
                     systemctl start {LAUNCHER_UNIT_NAME}` (see `journalctl -u \
                     {LAUNCHER_UNIT_NAME}`), or `{setup}` to reinstall it"
                ))
            },
        ));
        for (label, copy, stale) in [
            (
                "Launcher's temps",
                &facts.temps_copy,
                "is not this temps binary, so the launcher still runs the version from before \
                 an upgrade",
            ),
            (
                "Launcher's Firecracker",
                &facts.vmm_copies,
                "differs from what setup installed in the data directory",
            ),
        ] {
            let result = match copy {
                Some(CopyState::Current) => CheckResult::Pass("current".into()),
                Some(CopyState::Stale) => CheckResult::Warn(format!("{stale}; run `{setup}`")),
                Some(CopyState::Unreadable(e)) => {
                    CheckResult::Warn(format!("could not compare: {e}"))
                }
                None => CheckResult::Fail(format!(
                    "{LAUNCHER_CONFIG_PATH} cannot be read; run `{setup}` to reinstall the \
                     launcher"
                )),
            };
            checks.push((label.to_string(), result));
        }
    }

    let network = if !facts.recorded.network_ready {
        CheckResult::Fail(format!(
            "setup did not finish the network stage; run `sudo temps firecracker setup \
             --network-only --data-dir {}`",
            facts.data_dir.display()
        ))
    } else if !facts.bridge_present || facts.taps_present < facts.recorded.tap_count {
        // The bridge, NAT rules and TAP devices do not survive a reboot.
        CheckResult::Fail(format!(
            "bridge {BRIDGE_NAME} {} and {} of {} TAP devices are present (the host may have \
             rebooted); run `sudo temps firecracker setup --network-only --data-dir {}`",
            if facts.bridge_present {
                "is up"
            } else {
                "is missing"
            },
            facts.taps_present,
            facts.recorded.tap_count,
            facts.data_dir.display()
        ))
    } else {
        CheckResult::Pass(format!(
            "bridge {BRIDGE_NAME} and {} TAP devices",
            facts.taps_present
        ))
    };
    checks.push(("Network".to_string(), network));
    checks
}

fn or_unknown(value: &str) -> &str {
    if value.is_empty() {
        "(unknown)"
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy() -> Facts {
        Facts {
            data_dir: PathBuf::from("/srv/temps"),
            recorded: RecordedSetup {
                firecracker_version: FIRECRACKER_VERSION.into(),
                kernel_version: KERNEL_VERSION.into(),
                network_ready: true,
                tap_count: 2,
                launcher_socket: Some("/run/temps-fc/launcher.sock".into()),
            },
            unavailable_reason: None,
            launcher_active: true,
            temps_copy: Some(CopyState::Current),
            vmm_copies: Some(CopyState::Current),
            bridge_present: true,
            taps_present: 2,
        }
    }

    fn verdict<'a>(checks: &'a [(String, CheckResult)], label: &str) -> &'a CheckResult {
        &checks
            .iter()
            .find(|(l, _)| l == label)
            .unwrap_or_else(|| panic!("no {label} check in {checks:?}"))
            .1
    }

    fn message(result: &CheckResult) -> &str {
        match result {
            CheckResult::Pass(m)
            | CheckResult::Warn(m)
            | CheckResult::Fail(m)
            | CheckResult::Info(m) => m,
        }
    }

    #[test]
    fn a_healthy_host_passes_every_check() {
        let checks = assess(&healthy());
        assert!(
            checks
                .iter()
                .all(|(_, r)| matches!(r, CheckResult::Pass(_))),
            "{checks:?}"
        );
    }

    #[test]
    fn a_host_without_the_launcher_fails_with_the_setup_command() {
        let mut facts = healthy();
        facts.recorded.launcher_socket = None;
        facts.temps_copy = None;
        facts.vmm_copies = None;
        let checks = assess(&facts);
        let launcher = verdict(&checks, "Jailer launcher");
        assert!(matches!(launcher, CheckResult::Fail(_)), "{launcher:?}");
        assert!(
            message(launcher).contains("sudo temps firecracker setup --data-dir /srv/temps"),
            "{launcher:?}"
        );
        assert!(
            !checks.iter().any(|(l, _)| l == "Launcher's temps"),
            "no copy to compare without a launcher"
        );
    }

    #[test]
    fn an_upgrade_the_launcher_has_not_caught_up_with_is_reported() {
        let mut facts = healthy();
        facts.temps_copy = Some(CopyState::Stale);
        let checks = assess(&facts);
        let copy = verdict(&checks, "Launcher's temps");
        assert!(matches!(copy, CheckResult::Warn(_)), "{copy:?}");
        assert!(message(copy).contains("before an upgrade"), "{copy:?}");
        assert!(
            message(copy).contains("sudo temps firecracker setup"),
            "{copy:?}"
        );
    }

    #[test]
    fn a_stopped_launcher_fails_and_says_how_to_start_it() {
        let mut facts = healthy();
        facts.launcher_active = false;
        let launcher = assess(&facts);
        let launcher = verdict(&launcher, "Jailer launcher");
        assert!(matches!(launcher, CheckResult::Fail(_)), "{launcher:?}");
        assert!(
            message(launcher).contains("systemctl start temps-fc-launcher.service"),
            "{launcher:?}"
        );
    }

    #[test]
    fn a_network_lost_to_a_reboot_points_at_the_network_stage() {
        let mut facts = healthy();
        facts.bridge_present = false;
        facts.taps_present = 0;
        let checks = assess(&facts);
        let network = verdict(&checks, "Network");
        assert!(matches!(network, CheckResult::Fail(_)), "{network:?}");
        assert!(message(network).contains("is missing"), "{network:?}");
        assert!(message(network).contains("--network-only"), "{network:?}");
    }

    #[test]
    fn versions_older_than_this_temps_pins_ask_for_setup() {
        let mut facts = healthy();
        facts.recorded.firecracker_version = "v1.10.0".into();
        let checks = assess(&facts);
        let versions = verdict(&checks, "Versions");
        assert!(matches!(versions, CheckResult::Warn(_)), "{versions:?}");
        assert!(message(versions).contains("v1.10.0"), "{versions:?}");
        assert!(
            message(versions).contains(FIRECRACKER_VERSION),
            "{versions:?}"
        );
    }

    #[test]
    fn the_servers_own_reason_is_what_the_backend_check_reports() {
        let mut facts = healthy();
        facts.unavailable_reason = Some("no guest kernel under /srv/temps".into());
        let checks = assess(&facts);
        assert_eq!(
            message(verdict(&checks, "Backend")),
            "no guest kernel under /srv/temps"
        );
    }

    #[test]
    fn copies_are_compared_by_their_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (
            dir.path().join("a"),
            dir.path().join("b"),
            dir.path().join("c"),
        );
        std::fs::write(&a, vec![7u8; 200_000]).unwrap();
        std::fs::write(&b, vec![7u8; 200_000]).unwrap();
        let mut differs = vec![7u8; 200_000];
        differs[150_000] = 8;
        std::fs::write(&c, differs).unwrap();
        assert_eq!(same_file_contents(&a, &b), CopyState::Current);
        assert_eq!(same_file_contents(&a, &c), CopyState::Stale);
        assert!(matches!(
            same_file_contents(&a, &dir.path().join("missing")),
            CopyState::Unreadable(_)
        ));
    }

    #[test]
    fn a_data_dir_without_firecracker_has_no_section() {
        let dir = tempfile::tempdir().unwrap();
        assert!(recorded_setup(dir.path()).is_none());
        std::fs::create_dir_all(dir.path().join("firecracker")).unwrap();
        std::fs::write(
            dir.path().join("firecracker/state.json"),
            br#"{"firecracker_version":"v1.16.1","network_ready":true,"tap_count":4}"#,
        )
        .unwrap();
        let recorded = recorded_setup(dir.path()).expect("provisioned");
        assert_eq!(recorded.tap_count, 4);
        assert_eq!(recorded.launcher_socket, None);
    }
}
