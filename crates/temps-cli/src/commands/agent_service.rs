// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `temps agent service`: run the joined worker agent as a systemd service,
//! so it survives logouts and reboots. The enrollment token and mTLS key stay
//! in the owner-only agent data directory; the unit only points at it.

use std::path::{Path, PathBuf};
use std::process::Command;

use clap::{Args, Subcommand};

const SERVICE_NAME: &str = "temps-agent.service";
const UNIT_PATH: &str = "/etc/systemd/system/temps-agent.service";
const MANAGED_MARKER: &str = "# Managed by Temps. Do not edit by hand.";

/// Run the agent as a systemd service
#[derive(Subcommand, Debug, Clone)]
pub enum AgentServiceCommand {
    /// Install temps-agent.service and start it (run `temps join` first)
    Install(InstallArgs),
    /// Stop and remove temps-agent.service
    Uninstall,
    /// Show the service's status
    Status,
}

#[derive(Args, Debug, Clone)]
pub struct InstallArgs {
    /// Temps binary the service runs (default: this binary)
    #[arg(long)]
    pub binary: Option<PathBuf>,
    /// Directory holding agent.json (default: TEMPS_DATA_DIR or ~/.temps)
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
    /// Print the unit without changing the machine
    #[arg(long)]
    pub dry_run: bool,
}

impl AgentServiceCommand {
    pub fn execute(&self) -> anyhow::Result<()> {
        match self {
            Self::Install(args) => install(args),
            Self::Uninstall => uninstall(),
            Self::Status => {
                let status = Command::new("systemctl")
                    .args(["status", SERVICE_NAME, "--no-pager"])
                    .status()
                    .map_err(|error| anyhow::anyhow!("could not run systemctl: {error}"))?;
                // 3 = inactive; still a valid answer to "what is its status".
                if !status.success() && status.code() != Some(3) {
                    anyhow::bail!("systemctl status failed ({status})");
                }
                Ok(())
            }
        }
    }
}

fn install(args: &InstallArgs) -> anyhow::Result<()> {
    let data_dir = args
        .data_dir
        .clone()
        .unwrap_or_else(super::agent::agent_data_dir);
    let data_dir = std::fs::canonicalize(&data_dir).map_err(|error| {
        anyhow::anyhow!(
            "the agent data directory {} is unusable ({error}); run `temps join` first or pass \
             --data-dir",
            data_dir.display()
        )
    })?;
    let config = data_dir.join("agent.json");
    if std::fs::metadata(&config)
        .map(|meta| meta.len())
        .unwrap_or(0)
        == 0
    {
        anyhow::bail!(
            "{} is missing or empty: run `temps join` on this machine first",
            config.display()
        );
    }
    let binary = match &args.binary {
        Some(binary) => binary.clone(),
        None => std::env::current_exe().map_err(|error| {
            anyhow::anyhow!("could not locate this binary ({error}); pass --binary")
        })?,
    };
    let binary = std::fs::canonicalize(&binary).map_err(|error| {
        anyhow::anyhow!(
            "the Temps binary {} is unusable ({error})",
            binary.display()
        )
    })?;
    let unit = render_unit(&binary, &data_dir)?;
    if args.dry_run {
        print!("{unit}");
        return Ok(());
    }

    require_root()?;
    require_systemd()?;
    refuse_unsafe_paths(&binary, &data_dir, &config)?;
    refuse_foreign_unit(Path::new(UNIT_PATH))?;
    let temporary = Path::new(UNIT_PATH).with_extension("service.tmp");
    std::fs::write(&temporary, unit)?;
    set_mode(&temporary, 0o644)?;
    std::fs::rename(&temporary, UNIT_PATH)?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", SERVICE_NAME])?;
    systemctl(&["restart", SERVICE_NAME])?;
    println!("Installed and started {SERVICE_NAME}.");
    println!("  Status: temps agent service status");
    println!("  Logs:   journalctl -u {SERVICE_NAME} -f");
    Ok(())
}

fn uninstall() -> anyhow::Result<()> {
    require_root()?;
    require_systemd()?;
    let unit = Path::new(UNIT_PATH);
    if !unit.exists() {
        println!("{SERVICE_NAME} is not installed.");
        return Ok(());
    }
    refuse_foreign_unit(unit)?;
    // Stopping a unit that is not running is not an error worth failing on.
    let _ = Command::new("systemctl")
        .args(["disable", "--now", SERVICE_NAME])
        .status();
    std::fs::remove_file(unit)?;
    systemctl(&["daemon-reload"])?;
    println!("Removed {SERVICE_NAME}.");
    Ok(())
}

/// The systemd unit for the agent, running `binary agent` with `data_dir`.
fn render_unit(binary: &Path, data_dir: &Path) -> anyhow::Result<String> {
    let binary = systemd_program(binary)?;
    let data_dir = systemd_quoted(data_dir)?;
    Ok(format!(
        "{MANAGED_MARKER}
[Unit]
Description=Temps worker agent
Documentation=https://temps.sh/docs/multi-node
Wants=network-online.target
After=network-online.target docker.service
Requires=docker.service

[Service]
Type=simple
Environment=\"TEMPS_DATA_DIR={data_dir}\"
ExecStart=\"{binary}\" agent
Restart=on-failure
RestartSec=5s
TimeoutStopSec=30s
KillSignal=SIGTERM
UMask=0077

[Install]
WantedBy=multi-user.target
"
    ))
}

/// `path` as the program of a double-quoted `ExecStart=`: [`systemd_quoted`],
/// and free of `$`.
///
/// systemd substitutes `$VAR`/`${VAR}` in command lines (argv, including
/// argv[0]) but runs the program path as written, so no escaping of `$`
/// (`$$` included) names the same file in both. Refusing it is the only
/// spelling that cannot run something other than the binary asked for.
/// `Environment=` values take `$` literally, so [`systemd_quoted`] alone is
/// right there.
fn systemd_program(path: &Path) -> anyhow::Result<String> {
    let quoted = systemd_quoted(path)?;
    if quoted.contains('$') {
        anyhow::bail!(
            "{quoted} contains '$', which systemd expands in ExecStart. Put the Temps binary \
             under a path without '$' and pass it with --binary"
        );
    }
    Ok(quoted)
}

/// `path` escaped for a double-quoted systemd value: `\` and `"` for the
/// quoting, `%` for specifier expansion.
fn systemd_quoted(path: &Path) -> anyhow::Result<String> {
    let value = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("{} is not valid UTF-8", path.display()))?;
    if !path.is_absolute() {
        anyhow::bail!("{value} is not an absolute path");
    }
    if value.chars().any(char::is_control) {
        anyhow::bail!("paths cannot contain control characters");
    }
    Ok(value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%"))
}

/// Why a path a root service depends on could be changed by someone other
/// than root.
#[derive(Debug, Clone, PartialEq, Eq)]
enum UnsafeOwnership {
    NotOwnedByRoot { uid: u32 },
    WritableByGroup { gid: u32, mode: u32 },
    WritableByOthers { mode: u32 },
}

/// Whether a file or directory with owner `uid`, group `gid` and permission
/// bits `mode` is safe for a root service to trust: only root may change it.
/// A group-writable entry is fine when the group is root's own (gid 0).
fn ownership_problem(uid: u32, gid: u32, mode: u32) -> Option<UnsafeOwnership> {
    if uid != 0 {
        Some(UnsafeOwnership::NotOwnedByRoot { uid })
    } else if mode & 0o002 != 0 {
        Some(UnsafeOwnership::WritableByOthers {
            mode: mode & 0o7777,
        })
    } else if mode & 0o020 != 0 && gid != 0 {
        Some(UnsafeOwnership::WritableByGroup {
            gid,
            mode: mode & 0o7777,
        })
    } else {
        None
    }
}

/// What a trusted path is, for the error and its fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrustedPath {
    Binary,
    DataDir,
    AgentConfig,
}

/// The first of `path` and its ancestors (up to `/`) that someone other
/// than root could change, by `lookup` (owner uid, group gid and mode). `path` must be
/// canonical: a symlink among its components would be checked instead of
/// its target. Whoever can write a directory can replace what is in it, so
/// every ancestor counts, not only the file.
fn first_unsafe_component(
    path: &Path,
    lookup: impl Fn(&Path) -> std::io::Result<(u32, u32, u32)>,
) -> anyhow::Result<Option<(PathBuf, UnsafeOwnership)>> {
    for component in path.ancestors() {
        let (uid, gid, mode) = lookup(component).map_err(|error| {
            anyhow::anyhow!("could not check who owns {}: {error}", component.display())
        })?;
        if let Some(problem) = ownership_problem(uid, gid, mode) {
            return Ok(Some((component.to_path_buf(), problem)));
        }
    }
    Ok(None)
}

/// The refusal for `unsafe_path`, found while checking `checked` as `role`.
fn unsafe_path_error(
    role: TrustedPath,
    checked: &Path,
    unsafe_path: &Path,
    problem: &UnsafeOwnership,
) -> anyhow::Error {
    let why = match problem {
        UnsafeOwnership::NotOwnedByRoot { uid } => format!("is owned by uid {uid}, not root"),
        UnsafeOwnership::WritableByGroup { gid, mode } => {
            format!("is writable by group {gid} (mode {mode:04o})")
        }
        UnsafeOwnership::WritableByOthers { mode } => {
            format!("is writable by everyone (mode {mode:04o})")
        }
    };
    let within = if unsafe_path == checked {
        String::new()
    } else {
        format!(" (on the path to {})", checked.display())
    };
    let fix = match role {
        TrustedPath::Binary => format!(
            "The service runs as root, so whoever can change that path can run code as root. \
             Install the binary where only root can write, e.g. \
             `sudo install -D -o root -g root -m 0755 {} /opt/temps/bin/temps`, then rerun with \
             `--binary /opt/temps/bin/temps`.",
            checked.display()
        ),
        TrustedPath::DataDir => format!(
            "The service runs as root and trusts the agent's configuration, so whoever can change \
             it controls a root process. Join this machine as root (`sudo temps join ...` keeps \
             the data in /root/.temps), or make the directory root's: \
             `sudo chown -R root:root {dir} && sudo chmod -R go-w {dir}`, and make sure no \
             directory above it is writable by others.",
            dir = checked.display()
        ),
        TrustedPath::AgentConfig => format!(
            "The service runs as root and trusts this configuration, so whoever can change it \
             controls a root process. Make it root's: `sudo chown root:root {file} && sudo chmod \
             go-w {file}`, and make sure no directory above it is writable by others.",
            file = checked.display()
        ),
    };
    anyhow::anyhow!(
        "refusing to install {SERVICE_NAME}: {}{within} {why}. {fix}",
        unsafe_path.display()
    )
}

/// Check `path` (canonical) and its ancestors for [`ownership_problem`]s.
fn require_root_owned(
    role: TrustedPath,
    path: &Path,
    lookup: impl Fn(&Path) -> std::io::Result<(u32, u32, u32)>,
) -> anyhow::Result<()> {
    match first_unsafe_component(path, lookup)? {
        Some((unsafe_path, problem)) => Err(unsafe_path_error(role, path, &unsafe_path, &problem)),
        None => Ok(()),
    }
}

/// Refuse to point a root service at a binary, data directory or agent
/// config that anyone but root could change (or swap through a writable
/// directory above it). All three paths are canonical.
#[cfg(unix)]
fn refuse_unsafe_paths(binary: &Path, data_dir: &Path, config: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let lookup =
        |path: &Path| std::fs::metadata(path).map(|meta| (meta.uid(), meta.gid(), meta.mode()));
    require_root_owned(TrustedPath::Binary, binary, lookup)?;
    require_root_owned(TrustedPath::DataDir, data_dir, lookup)?;
    // agent.json may itself be a symlink out of the data directory.
    let config = std::fs::canonicalize(config).map_err(|error| {
        anyhow::anyhow!(
            "the agent config {} is unusable ({error})",
            config.display()
        )
    })?;
    require_root_owned(TrustedPath::AgentConfig, &config, lookup)
}

#[cfg(not(unix))]
fn refuse_unsafe_paths(_binary: &Path, _data_dir: &Path, _config: &Path) -> anyhow::Result<()> {
    Ok(())
}

fn refuse_foreign_unit(unit: &Path) -> anyhow::Result<()> {
    match std::fs::read_to_string(unit) {
        Ok(contents) if !contents.lines().any(|line| line == MANAGED_MARKER) => {
            anyhow::bail!(
                "refusing to replace {}: it is not managed by Temps",
                unit.display()
            )
        }
        _ => Ok(()),
    }
}

fn require_root() -> anyhow::Result<()> {
    // SAFETY: geteuid has no preconditions and cannot fail.
    if unsafe { libc::geteuid() } != 0 {
        anyhow::bail!("this needs root; rerun with sudo");
    }
    Ok(())
}

fn require_systemd() -> anyhow::Result<()> {
    if !cfg!(target_os = "linux") {
        anyhow::bail!("the agent service is a systemd unit; this needs Linux");
    }
    if !Path::new("/run/systemd/system").is_dir() {
        anyhow::bail!(
            "this machine does not run systemd, so the agent cannot be installed as a service. \
             Run `temps agent` under your own supervisor (it must restart it on failure and at \
             boot)."
        );
    }
    Ok(())
}

fn systemctl(args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new("systemctl")
        .args(args)
        .status()
        .map_err(|error| anyhow::anyhow!("could not run systemctl: {error}"))?;
    if !status.success() {
        anyhow::bail!("systemctl {} failed ({status})", args.join(" "));
    }
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unit_runs_the_agent_with_its_data_dir_and_is_marked_managed() {
        let unit =
            render_unit(Path::new("/usr/local/bin/temps"), Path::new("/root/.temps")).unwrap();
        assert!(unit.starts_with(MANAGED_MARKER));
        assert!(unit.contains("Environment=\"TEMPS_DATA_DIR=/root/.temps\""));
        assert!(unit.contains("ExecStart=\"/usr/local/bin/temps\" agent"));
        assert!(unit.contains("Requires=docker.service"));
    }

    #[test]
    fn paths_are_escaped_for_systemd_and_must_be_absolute() {
        assert_eq!(
            systemd_quoted(Path::new("/opt/100%\"x\\")).unwrap(),
            "/opt/100%%\\\"x\\\\"
        );
        assert!(systemd_quoted(Path::new("relative/temps")).is_err());
        assert!(systemd_quoted(Path::new("/opt/a\nb")).is_err());
    }

    #[test]
    fn a_dollar_is_literal_in_environment_and_refused_in_exec_start() {
        // Environment= does no variable expansion: `$` stays as written.
        assert_eq!(
            systemd_quoted(Path::new("/srv/$HOME/.temps")).unwrap(),
            "/srv/$HOME/.temps"
        );
        let unit = render_unit(Path::new("/usr/local/bin/temps"), Path::new("/srv/$x")).unwrap();
        assert!(unit.contains("Environment=\"TEMPS_DATA_DIR=/srv/$x\""));
        // ExecStart= expands `$VAR`, so a program path with `$` is refused
        // rather than written in a form that may run something else.
        for binary in ["/opt/$HOME/temps", "/opt/${PATH}/temps", "/opt/a$$b/temps"] {
            let error = render_unit(Path::new(binary), Path::new("/root/.temps")).unwrap_err();
            assert!(error.to_string().contains("--binary"), "{binary}: {error}");
        }
        assert_eq!(
            systemd_program(Path::new("/opt/100%/temps")).unwrap(),
            "/opt/100%%/temps"
        );
    }

    #[test]
    fn only_root_owned_paths_writable_by_root_alone_are_trusted() {
        assert_eq!(ownership_problem(0, 0, 0o100755), None);
        assert_eq!(ownership_problem(0, 0, 0o040700), None);
        assert_eq!(
            ownership_problem(1000, 0, 0o100755),
            Some(UnsafeOwnership::NotOwnedByRoot { uid: 1000 })
        );
        assert_eq!(
            ownership_problem(0, 50, 0o100775),
            Some(UnsafeOwnership::WritableByGroup {
                gid: 50,
                mode: 0o775
            }),
            "a directory a non-root group can write (e.g. root:staff 2775) is not trusted"
        );
        assert_eq!(
            ownership_problem(0, 0, 0o042775),
            None,
            "group write is fine when the group is root's own"
        );
        assert_eq!(
            ownership_problem(0, 0, 0o100777),
            Some(UnsafeOwnership::WritableByOthers { mode: 0o777 })
        );
        assert_eq!(
            ownership_problem(0, 0, 0o041777),
            Some(UnsafeOwnership::WritableByOthers { mode: 0o1777 }),
            "a sticky world-writable directory such as /tmp is not trusted either"
        );
    }

    /// An ownership table standing in for the filesystem. Every entry's
    /// group is a non-root one (gid 50), so group write is never excused.
    fn table<'a>(
        entries: &'a [(&'a str, u32, u32)],
    ) -> impl Fn(&Path) -> std::io::Result<(u32, u32, u32)> + 'a {
        move |path: &Path| {
            entries
                .iter()
                .find(|(entry, _, _)| Path::new(entry) == path)
                .map(|(_, uid, mode)| (*uid, 50, *mode))
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
        }
    }

    #[test]
    fn every_directory_above_the_binary_counts() {
        let safe = [
            ("/", 0, 0o40755),
            ("/usr", 0, 0o40755),
            ("/usr/local", 0, 0o40755),
            ("/usr/local/bin", 0, 0o40755),
            ("/usr/local/bin/temps", 0, 0o100755),
        ];
        let binary = Path::new("/usr/local/bin/temps");
        assert!(require_root_owned(TrustedPath::Binary, binary, table(&safe)).is_ok());

        // A root-owned binary in a user's directory can be swapped by that user.
        let in_home = [
            ("/", 0, 0o40755),
            ("/home", 0, 0o40755),
            ("/home/dev", 1000, 0o40755),
            ("/home/dev/temps", 0, 0o100755),
        ];
        let error = require_root_owned(
            TrustedPath::Binary,
            Path::new("/home/dev/temps"),
            table(&in_home),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("/home/dev (on the path to /home/dev/temps)"),
            "{error}"
        );
        assert!(error.contains("uid 1000"), "{error}");
        assert!(error.contains("/opt/temps/bin/temps"), "{error}");

        let group_writable = [
            ("/", 0, 0o40755),
            ("/opt", 0, 0o40775),
            ("/opt/temps", 0, 0o100755),
        ];
        let error = require_root_owned(
            TrustedPath::Binary,
            Path::new("/opt/temps"),
            table(&group_writable),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("/opt (on the path"), "{error}");
        assert!(error.contains("mode 0775"), "{error}");
    }

    #[test]
    fn a_data_dir_others_can_write_is_refused_with_its_fix() {
        let entries = [
            ("/", 0, 0o40755),
            ("/home", 0, 0o40755),
            ("/home/dev", 1000, 0o40700),
            ("/home/dev/.temps", 1000, 0o40700),
        ];
        let error = require_root_owned(
            TrustedPath::DataDir,
            Path::new("/home/dev/.temps"),
            table(&entries),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.starts_with(
                "refusing to install temps-agent.service: /home/dev/.temps is owned by uid 1000"
            ),
            "{error}"
        );
        assert!(
            error.contains("sudo chown -R root:root /home/dev/.temps"),
            "{error}"
        );
    }

    #[test]
    fn a_path_that_cannot_be_checked_is_refused() {
        let error = require_root_owned(
            TrustedPath::Binary,
            Path::new("/usr/local/bin/temps"),
            table(&[]),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("could not check who owns /usr/local/bin/temps"),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_temporary_directory_is_not_trusted_for_a_root_service() {
        // Run against the real filesystem: a test's temp dir is owned by the
        // user running the tests, or sits under a world-writable /tmp.
        // SAFETY: geteuid has no preconditions and cannot fail.
        if unsafe { libc::geteuid() } == 0 {
            println!("running as root: the temp dir may be trusted, skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let data_dir = std::fs::canonicalize(dir.path()).unwrap();
        let config = data_dir.join("agent.json");
        std::fs::write(&config, "{}").unwrap();
        let binary = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        let error = refuse_unsafe_paths(&binary, &data_dir, &config)
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with("refusing to install temps-agent.service"),
            "{error}"
        );
    }

    #[test]
    fn only_units_temps_wrote_are_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let unit = dir.path().join("temps-agent.service");
        assert!(refuse_foreign_unit(&unit).is_ok(), "a missing unit is fine");
        std::fs::write(&unit, "[Unit]\nDescription=someone else's\n").unwrap();
        assert!(refuse_foreign_unit(&unit).is_err());
        std::fs::write(&unit, format!("{MANAGED_MARKER}\n[Unit]\n")).unwrap();
        assert!(refuse_foreign_unit(&unit).is_ok());
    }
}
