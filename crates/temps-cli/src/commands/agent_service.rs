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
