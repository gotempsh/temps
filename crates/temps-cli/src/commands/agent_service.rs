// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `temps agent service`: run the joined worker agent as a systemd service,
//! so it survives logouts and reboots (on macOS a launchd LaunchAgent, see
//! `agent_launchd`). The enrollment token and mTLS key stay
//! in the owner-only agent data directory; the unit only points at it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use clap::{Args, Subcommand};

const SERVICE_NAME: &str = "temps-agent.service";
const UNIT_PATH: &str = "/etc/systemd/system/temps-agent.service";
const MANAGED_MARKER: &str = "# Managed by Temps. Do not edit by hand.";
/// Loads the network policy boot guard before Docker restarts workloads.
const GUARD_SERVICE_NAME: &str = "temps-netpol-guard.service";
const GUARD_UNIT_PATH: &str = "/etc/systemd/system/temps-netpol-guard.service";
/// Where `nft` usually lives; the guard unit names it by absolute path.
const NFT_CANDIDATES: [&str; 3] = ["/usr/sbin/nft", "/sbin/nft", "/usr/bin/nft"];

/// Run the agent as a systemd service
#[derive(Subcommand, Debug, Clone)]
pub enum AgentServiceCommand {
    /// Install temps-agent.service and start it (run `temps join` first)
    Install(InstallArgs),
    /// Stop and remove temps-agent.service (on macOS, the LaunchAgent)
    Uninstall(UninstallArgs),
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

#[derive(Args, Debug, Clone)]
pub struct UninstallArgs {
    /// macOS only: also remove the mesh sidecar and this worker's mesh
    /// interface from the Docker app's VM
    #[arg(long)]
    pub remove_mesh: bool,
}

impl AgentServiceCommand {
    pub fn execute(&self) -> anyhow::Result<()> {
        // A Mac runs the agent as a launchd LaunchAgent (ADR 090).
        if cfg!(target_os = "macos") {
            return super::agent_launchd::execute(self);
        }
        match self {
            Self::Install(args) => install(args),
            Self::Uninstall(_) => uninstall(),
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
    let nft = NFT_CANDIDATES
        .iter()
        .map(Path::new)
        .find(|candidate| candidate.is_file());
    let guard_unit = match nft {
        Some(nft) => match render_guard_unit(nft, &data_dir) {
            Ok(unit) => Some(unit),
            Err(error) => {
                eprintln!("Warning: the network policy boot guard is not installed: {error}");
                None
            }
        },
        None => {
            eprintln!(
                "Warning: nft was not found, so the network policy boot guard is not \
                 installed. Without it, a node in `enforce` leaves restricted workloads \
                 unguarded between a reboot and the agent's first policy pass. Install \
                 nftables and run this again."
            );
            None
        }
    };
    if args.dry_run {
        print!("{unit}");
        if let Some(guard_unit) = &guard_unit {
            print!("\n{guard_unit}");
        }
        return Ok(());
    }

    require_root()?;
    require_systemd()?;
    warn_about_host(&config);
    refuse_unsafe_paths(ServiceAction::Install, &binary, &data_dir, &config)
        .map_err(|(_, error)| error)?;
    refuse_foreign_unit(Path::new(UNIT_PATH))?;
    let temporary = Path::new(UNIT_PATH).with_extension("service.tmp");
    std::fs::write(&temporary, unit)?;
    set_mode(&temporary, 0o644)?;
    std::fs::rename(&temporary, UNIT_PATH)?;
    if let Some(guard_unit) = guard_unit {
        refuse_foreign_unit(Path::new(GUARD_UNIT_PATH))?;
        let temporary = Path::new(GUARD_UNIT_PATH).with_extension("service.tmp");
        std::fs::write(&temporary, guard_unit)?;
        set_mode(&temporary, 0o644)?;
        std::fs::rename(&temporary, GUARD_UNIT_PATH)?;
    }
    systemctl(&["daemon-reload"])?;
    if Path::new(GUARD_UNIT_PATH).exists() {
        // Enabled, not started: it only matters at boot, before Docker.
        systemctl(&["enable", GUARD_SERVICE_NAME])?;
    }
    systemctl(&["enable", SERVICE_NAME])?;
    systemctl(&["restart", SERVICE_NAME])?;
    println!("Installed and started {SERVICE_NAME}.");
    println!("  Status: temps agent service status");
    println!("  Logs:   journalctl -u {SERVICE_NAME} -f");
    Ok(())
}

/// The checks `temps join` stops on, as warnings (ADR 059): the agent is
/// about to start, and what would keep its overlay or ingress down is
/// cheaper to read here than in its logs.
fn warn_about_host(config: &Path) {
    use super::host_preflight::{assess, gather, Purpose};

    let saved: Option<temps_node_agent::AgentConfig> = std::fs::read_to_string(config)
        .ok()
        .and_then(|data| serde_json::from_str(&data).ok());
    // Without a readable config the mode is unknown: check what a public
    // listener would need rather than miss a held port.
    let ingress = saved.as_ref().map_or_else(
        || temps_core::worker_ingress::LocalIngress {
            mode: temps_core::worker_ingress::IngressMode::Fixed,
            ..Default::default()
        },
        temps_node_agent::AgentConfig::local_ingress,
    );
    let (ports, serves_public) = super::host_preflight::served_ports(&ingress);
    let facts = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(gather(saved.as_ref())),
        Err(error) => {
            eprintln!("Warning: could not check this machine before installing ({error}).");
            return;
        }
    };
    assess(&facts, Purpose::ServiceInstall, &ports, serves_public).print_warnings();
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
    let guard = Path::new(GUARD_UNIT_PATH);
    if guard.exists() {
        refuse_foreign_unit(guard)?;
        let _ = Command::new("systemctl")
            .args(["disable", GUARD_SERVICE_NAME])
            .status();
        std::fs::remove_file(guard)?;
    }
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
# Stop only the agent, not the sandbox VMs it started: Firecracker and
# microsandbox VMMs are its child processes, and the default
# KillMode=control-group would take every running sandbox down with each
# `systemctl restart`. The restarted agent finds them again by name.
KillMode=process
UMask=0077

[Install]
WantedBy=multi-user.target
"
    ))
}

/// The unit that loads the network policy boot guard the agent keeps in
/// `data_dir` (see `temps_node_agent::netpol_guard`), before Docker restarts any
/// container. Nothing happens when the file is absent: the node does not
/// enforce. A failure to load it does not hold Docker back.
fn render_guard_unit(nft: &Path, data_dir: &Path) -> anyhow::Result<String> {
    let nft = systemd_program(nft)?;
    let guard = data_dir.join(temps_node_agent::netpol_guard::GUARD_FILE);
    let quoted = systemd_quoted(&guard)?;
    // `ConditionPathExists=` takes the path unquoted.
    if quoted != guard.to_string_lossy() || quoted.contains(char::is_whitespace) {
        anyhow::bail!(
            "{} needs quoting, which ConditionPathExists= cannot express; use a data directory \
             without spaces, quotes, backslashes or '%'",
            guard.display()
        );
    }
    Ok(format!(
        "{MANAGED_MARKER}
[Unit]
Description=Temps network policy boot guard
Documentation=https://temps.sh/docs/multi-node
DefaultDependencies=no
After=local-fs.target
Before=docker.service network-pre.target
Wants=network-pre.target
ConditionPathExists={quoted}

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=\"{nft}\" -f \"{quoted}\"

[Install]
WantedBy=multi-user.target docker.service
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

/// What the ownership check guards, for the error's wording: `temps agent
/// service install`, or `temps join` restarting the installed service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceAction {
    Install,
    Restart,
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

/// For a command run as root that edits the agent's files (`temps agent
/// labels`, `temps agent ingress`): the first of `dir` (canonical) and its
/// ancestors that someone other than root could change, and why. Whoever
/// controls such a directory could swap files under a root process, turning
/// its reads and writes into theirs.
#[cfg(unix)]
pub(super) fn not_root_controlled(dir: &Path) -> anyhow::Result<Option<String>> {
    use std::os::unix::fs::MetadataExt;
    let lookup =
        |path: &Path| std::fs::metadata(path).map(|meta| (meta.uid(), meta.gid(), meta.mode()));
    Ok(first_unsafe_component(dir, lookup)?.map(|(path, problem)| {
        let why = match problem {
            UnsafeOwnership::NotOwnedByRoot { uid } => format!("is owned by uid {uid}, not root"),
            UnsafeOwnership::WritableByGroup { gid, mode } => {
                format!("is writable by group {gid} (mode {mode:04o})")
            }
            UnsafeOwnership::WritableByOthers { mode } => {
                format!("is writable by everyone (mode {mode:04o})")
            }
        };
        format!("{} {why}", path.display())
    }))
}

#[cfg(not(unix))]
pub(super) fn not_root_controlled(_dir: &Path) -> anyhow::Result<Option<String>> {
    Ok(None)
}

/// The refusal for `unsafe_path`, found while checking `checked` as `role`.
fn unsafe_path_error(
    action: ServiceAction,
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
             `sudo install -D -o root -g root -m 0755 {} {SAFE_BINARY}`, then {}.",
            checked.display(),
            match action {
                ServiceAction::Install => format!("rerun with `--binary {SAFE_BINARY}`"),
                ServiceAction::Restart =>
                    format!("reinstall the service with `--binary {SAFE_BINARY}`"),
            }
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
    let refusal = match action {
        ServiceAction::Install => "refusing to install",
        ServiceAction::Restart => "join will not restart",
    };
    anyhow::anyhow!(
        "{refusal} {SERVICE_NAME}: {}{within} {why}. {fix}",
        unsafe_path.display()
    )
}

/// Check `path` (canonical) and its ancestors for [`ownership_problem`]s.
fn require_root_owned(
    action: ServiceAction,
    role: TrustedPath,
    path: &Path,
    lookup: impl Fn(&Path) -> std::io::Result<(u32, u32, u32)>,
) -> anyhow::Result<()> {
    match first_unsafe_component(path, lookup)? {
        Some((unsafe_path, problem)) => Err(unsafe_path_error(
            action,
            role,
            path,
            &unsafe_path,
            &problem,
        )),
        None => Ok(()),
    }
}

/// Refuse to point a root service at a binary, data directory or agent
/// config that anyone but root could change (or swap through a writable
/// directory above it). All three paths are canonical.
/// Where the ownership error suggests installing the binary.
const SAFE_BINARY: &str = "/opt/temps/bin/temps";

/// Refuse to point a root service at a binary, data directory or agent
/// config that anyone but root could change (or swap through a writable
/// directory above it). All three paths are canonical. The error comes
/// with the path that failed, so a caller can tailor its fix.
#[cfg(unix)]
fn refuse_unsafe_paths(
    action: ServiceAction,
    binary: &Path,
    data_dir: &Path,
    config: &Path,
) -> Result<(), (TrustedPath, anyhow::Error)> {
    use std::os::unix::fs::MetadataExt;
    let lookup =
        |path: &Path| std::fs::metadata(path).map(|meta| (meta.uid(), meta.gid(), meta.mode()));
    require_root_owned(action, TrustedPath::Binary, binary, lookup)
        .map_err(|error| (TrustedPath::Binary, error))?;
    require_root_owned(action, TrustedPath::DataDir, data_dir, lookup)
        .map_err(|error| (TrustedPath::DataDir, error))?;
    // agent.json may itself be a symlink out of the data directory.
    let config = std::fs::canonicalize(config).map_err(|error| {
        (
            TrustedPath::AgentConfig,
            anyhow::anyhow!(
                "the agent config {} is unusable ({error})",
                config.display()
            ),
        )
    })?;
    require_root_owned(action, TrustedPath::AgentConfig, &config, lookup)
        .map_err(|error| (TrustedPath::AgentConfig, error))
}

#[cfg(not(unix))]
fn refuse_unsafe_paths(
    _action: ServiceAction,
    _binary: &Path,
    _data_dir: &Path,
    _config: &Path,
) -> Result<(), (TrustedPath, anyhow::Error)> {
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

/// The commands for the agent service on this platform, as the operator
/// would type them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentService {
    /// How the service is named in messages.
    pub name: String,
    /// Restart it onto the config on disk.
    pub restart: String,
    /// Start it again after someone turned it off.
    pub start: String,
    /// Its most recent log lines.
    pub logs: String,
}

/// What `temps join` found, and did, about the agent service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceAfterJoin {
    /// No Temps-managed service runs from this data directory: whoever starts
    /// the agent next reads the new config.
    NotInstalled,
    /// The managed service exists but is stopped (and, on Linux, disabled):
    /// someone turned it off on purpose, so join leaves it off.
    Stopped(AgentService),
    /// The managed service was running or enabled, was restarted on the new
    /// config, and stayed up.
    Restarted(AgentService),
    /// The managed service exists but runs on the old config: a check before
    /// the restart refused it, the restart failed, or it did not stay up.
    /// `fix` is the command that puts it on this config once `reason` is
    /// dealt with, when there is one.
    NotRestarted {
        service: AgentService,
        reason: String,
        fix: Option<String>,
    },
}

/// How long a restarted agent must stay up before join calls the restart
/// good. Long enough for a config the agent rejects at start to show, and
/// shorter than the services' restart delays (systemd's 5s `RestartSec`,
/// launchd's 10s throttle), so a crash cannot hide behind a respawn.
pub(super) const STAY_UP_WINDOW: Duration = Duration::from_secs(4);
pub(super) const STAY_UP_INTERVAL: Duration = Duration::from_millis(500);
/// How long `systemctl restart` may take: the unit's 30s stop timeout plus
/// the start.
const RESTART_TIMEOUT: Duration = Duration::from_secs(90);
/// How long a read-only `systemctl` query may take.
const QUERY_TIMEOUT: Duration = Duration::from_secs(15);
/// How long one stay-up sample may take, so the watch ends near its window.
pub(super) const SAMPLE_TIMEOUT: Duration = Duration::from_secs(3);

/// The agent reads `agent.json` only at start. After `temps join` rewrites it,
/// a service that was already running keeps serving the cluster this machine
/// left (with that cluster's certificates in memory) while the new control
/// plane sees a worker that registered and then went silent. So a join ends
/// by restarting the managed service that runs from this data directory, if
/// it was running or enabled: the systemd unit on Linux, the LaunchAgent on
/// macOS. A unit or plist Temps did not write, or one for another data
/// directory, is never touched. Never fails: by now the join token is spent
/// and the config saved, so every problem becomes an outcome to print.
/// Blocks for a few seconds while it watches the restarted service.
pub fn restart_after_join(data_dir: &Path) -> ServiceAfterJoin {
    // `temps agent service install` writes the canonical path.
    let Ok(data_dir) = std::fs::canonicalize(data_dir) else {
        return ServiceAfterJoin::NotInstalled;
    };
    if cfg!(target_os = "macos") {
        return super::agent_launchd::restart_after_join(&data_dir);
    }
    restart_unit_after_join(&data_dir)
}

fn systemd_service() -> AgentService {
    AgentService {
        name: SERVICE_NAME.to_string(),
        restart: format!("sudo systemctl restart {SERVICE_NAME}"),
        start: format!("sudo systemctl enable --now {SERVICE_NAME}"),
        logs: format!("sudo journalctl -u {SERVICE_NAME} -n 50 --no-pager"),
    }
}

fn restart_unit_after_join(data_dir: &Path) -> ServiceAfterJoin {
    if !cfg!(target_os = "linux") || !Path::new("/run/systemd/system").is_dir() {
        return ServiceAfterJoin::NotInstalled;
    }
    let Ok(unit) = std::fs::read_to_string(UNIT_PATH) else {
        return ServiceAfterJoin::NotInstalled;
    };
    let service = systemd_service();
    let reinstall = format!(
        "sudo temps agent service install --data-dir {}",
        shell_quoted(data_dir)
    );
    let refuse = |reason: String, fix: Option<String>| ServiceAfterJoin::NotRestarted {
        service: systemd_service(),
        reason,
        fix,
    };
    let binary = match classify_unit(&unit, data_dir) {
        UnitFile::Foreign => return ServiceAfterJoin::NotInstalled,
        UnitFile::Refused(reason) => return refuse(reason, Some(reinstall)),
        UnitFile::Ours { binary } => binary,
    };
    let show = match systemctl_output(
        &[
            "show",
            SERVICE_NAME,
            "-p",
            "FragmentPath",
            "-p",
            "DropInPaths",
            "-p",
            "NeedDaemonReload",
            "-p",
            "ActiveState",
            "-p",
            "UnitFileState",
        ],
        QUERY_TIMEOUT,
    ) {
        Ok(show) => show,
        Err(error) => return refuse(error, Some(service.restart.clone())),
    };
    let loaded = LoadedUnit::parse(&show);
    if let Err(reason) = loaded.check(UNIT_PATH, |path| std::fs::read_to_string(path)) {
        return refuse(reason, Some(service.restart.clone()));
    }
    if !loaded.wanted() {
        return ServiceAfterJoin::Stopped(service);
    }
    // SAFETY: geteuid has no preconditions and cannot fail.
    if unsafe { libc::geteuid() } != 0 {
        return refuse(
            format!("this did not run as root, and only root restarts {SERVICE_NAME}"),
            Some(service.restart.clone()),
        );
    }
    if let Err(reason) = check_joined_config(data_dir) {
        return refuse(reason, Some(service.restart.clone()));
    }
    // Install wrote the canonical path; a symlink there now could point
    // somewhere the ownership check below would not cover.
    match std::fs::canonicalize(&binary) {
        Ok(canonical) if canonical == binary && canonical.is_file() => {}
        Ok(canonical) => {
            return refuse(
                format!(
                    "{UNIT_PATH} runs {}, which now resolves to {}",
                    binary.display(),
                    canonical.display()
                ),
                Some(reinstall),
            )
        }
        Err(error) => {
            return refuse(
                format!(
                    "{UNIT_PATH} runs {}, which is unusable ({error})",
                    binary.display()
                ),
                Some(reinstall),
            )
        }
    }
    // The same ownership check install makes: the restarted agent runs as
    // root, so a binary or config another user could swap must not start.
    if let Err((role, error)) = refuse_unsafe_paths(
        ServiceAction::Restart,
        &binary,
        data_dir,
        &data_dir.join("agent.json"),
    ) {
        // An unsafe binary needs a safe copy first (the reason says how), then
        // a unit pointing at it; the other paths need only their ownership
        // fixed before install rewrites the unit.
        let fix = match role {
            TrustedPath::Binary => format!("{reinstall} --binary {SAFE_BINARY}"),
            TrustedPath::DataDir | TrustedPath::AgentConfig => reinstall,
        };
        return refuse(error.to_string(), Some(fix));
    }
    if let Err(error) = systemctl_output(
        &["restart", "--no-ask-password", SERVICE_NAME],
        RESTART_TIMEOUT,
    ) {
        return refuse(
            format!("{error} (systemd may still finish the restart; check `temps agent service status`)"),
            Some(service.restart.clone()),
        );
    }
    let active = || {
        systemctl_output(
            &["show", SERVICE_NAME, "-p", "ActiveState", "--value"],
            SAMPLE_TIMEOUT,
        )
        .is_ok_and(|state| state.trim() == "active")
    };
    if !stays_up(active, STAY_UP_WINDOW, STAY_UP_INTERVAL) {
        return refuse(
            format!(
                "it restarted but did not stay up for {}s, so the agent likely rejects this \
                 config; see its logs: {}",
                STAY_UP_WINDOW.as_secs(),
                service.logs
            ),
            Some(service.restart.clone()),
        );
    }
    ServiceAfterJoin::Restarted(service)
}

/// What join may do with the unit file at [`UNIT_PATH`].
#[derive(Debug, PartialEq, Eq)]
enum UnitFile {
    /// Not written by Temps: never touched.
    Foreign,
    /// Temps' unit, exactly as this version writes it for the data
    /// directory, running `binary`.
    Ours { binary: PathBuf },
    /// Temps' unit, but join must not restart it, for this reason; `temps
    /// agent service install` rewrites it.
    Refused(String),
}

fn classify_unit(unit: &str, data_dir: &Path) -> UnitFile {
    if !unit.lines().any(|line| line == MANAGED_MARKER) {
        return UnitFile::Foreign;
    }
    // A Temps unit for another data directory would keep running the old
    // config: say so rather than print the hint for a machine without one.
    match unit_data_dir(unit) {
        Some(installed) if installed == data_dir => {}
        installed => {
            return UnitFile::Refused(format!(
                "it runs the agent from {}, but this join saved its config in {}, so join left \
                 it alone and it keeps the old config",
                installed.map_or_else(
                    || "an unknown directory".to_string(),
                    |dir| dir.display().to_string()
                ),
                data_dir.display()
            ))
        }
    }
    // Only the unit exactly as this Temps writes it: a hand-edited one, or
    // one from an older Temps (without KillMode=process, a restart would take
    // every sandbox VM down), is rewritten by install instead.
    let Some(binary) = unit_binary(unit) else {
        return UnitFile::Refused(format!("{UNIT_PATH} has no ExecStart Temps can read"));
    };
    if render_unit(&binary, data_dir).ok().as_deref() != Some(unit) {
        return UnitFile::Refused(format!(
            "{UNIT_PATH} differs from the unit this version of Temps writes (edited by hand, or \
             installed by an older Temps)"
        ));
    }
    UnitFile::Ours { binary }
}

/// The data directory a unit written by [`render_unit`] runs the agent
/// with, unescaped.
fn unit_data_dir(unit: &str) -> Option<PathBuf> {
    let quoted = unit
        .lines()
        .find_map(|line| line.strip_prefix("Environment=\"TEMPS_DATA_DIR="))?;
    systemd_unquoted(quoted, "").map(PathBuf::from)
}

/// `value` for a POSIX shell command line.
pub(super) fn shell_quoted(value: &Path) -> String {
    let text = value.to_string_lossy();
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+:@%".contains(c));
    if plain {
        text.into_owned()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

/// The binary a unit written by [`render_unit`] runs, unescaped.
fn unit_binary(unit: &str) -> Option<PathBuf> {
    let quoted = unit
        .lines()
        .find_map(|line| line.strip_prefix("ExecStart=\""))?;
    systemd_unquoted(quoted, " agent").map(PathBuf::from)
}

/// Undo [`systemd_quoted`] on the text after an opening `"`: the value up to
/// the closing `"`, which must be followed by exactly `rest`.
fn systemd_unquoted(quoted: &str, rest: &str) -> Option<String> {
    let mut value = String::new();
    let mut chars = quoted.chars();
    loop {
        match chars.next()? {
            '"' => break,
            '\\' => value.push(chars.next()?),
            '%' => {
                if chars.next()? != '%' {
                    return None;
                }
                value.push('%');
            }
            other => value.push(other),
        }
    }
    (chars.as_str() == rest).then_some(value)
}

/// What systemd has loaded for the agent unit, from `systemctl show`.
#[derive(Debug, Default, PartialEq, Eq)]
struct LoadedUnit {
    fragment_path: String,
    drop_in_paths: Vec<String>,
    need_daemon_reload: bool,
    active_state: String,
    unit_file_state: String,
}

impl LoadedUnit {
    fn parse(show: &str) -> Self {
        let mut loaded = Self::default();
        for line in show.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key {
                "FragmentPath" => loaded.fragment_path = value.to_string(),
                "DropInPaths" => {
                    loaded.drop_in_paths = value.split_whitespace().map(str::to_string).collect()
                }
                "NeedDaemonReload" => loaded.need_daemon_reload = value == "yes",
                "ActiveState" => loaded.active_state = value.to_string(),
                "UnitFileState" => loaded.unit_file_state = value.to_string(),
                _ => {}
            }
        }
        loaded
    }

    /// Whether a restart would run the unit Temps wrote, as written: loaded
    /// from `unit_path` (not shadowed by a unit elsewhere), current with the
    /// file, and with no drop-in changing what runs, as whom, or from which
    /// data directory.
    fn check(
        &self,
        unit_path: &str,
        read: impl Fn(&str) -> std::io::Result<String>,
    ) -> Result<(), String> {
        if self.fragment_path.is_empty() {
            return Err(format!(
                "systemd has not loaded {SERVICE_NAME}; run `sudo systemctl daemon-reload` first"
            ));
        }
        if self.fragment_path != unit_path {
            return Err(format!(
                "systemd loads {SERVICE_NAME} from '{}', not from the unit Temps installed at \
                 {unit_path}, so join leaves it alone",
                self.fragment_path
            ));
        }
        if self.need_daemon_reload {
            return Err(format!(
                "{unit_path} changed since systemd loaded it; run `sudo systemctl daemon-reload` \
                 first"
            ));
        }
        for drop_in in &self.drop_in_paths {
            let contents = read(drop_in)
                .map_err(|error| format!("could not read the drop-in {drop_in}: {error}"))?;
            if drop_in_overrides(&contents) {
                return Err(format!(
                    "the drop-in {drop_in} changes what {SERVICE_NAME} runs or its data \
                     directory, so join leaves it alone"
                ));
            }
        }
        Ok(())
    }

    /// Running, starting, or enabled to start at boot.
    fn wanted(&self) -> bool {
        matches!(
            self.active_state.as_str(),
            "active" | "activating" | "reloading"
        ) || matches!(self.unit_file_state.as_str(), "enabled" | "enabled-runtime")
    }
}

/// Whether a drop-in sets anything beyond tuning: join restarts as root,
/// so only resource limits, restart and timeout policy, ordering, and
/// environment variables other than `TEMPS_DATA_DIR` pass. Anything else
/// (the program, hooks such as `ExecStartPre`, the user, mounts, the
/// working directory, an `EnvironmentFile`) leaves the restart to the
/// operator.
fn drop_in_overrides(contents: &str) -> bool {
    const ALLOWED: [&str; 34] = [
        "Description",
        "Documentation",
        "After",
        "Before",
        "Wants",
        "Requires",
        "StartLimitIntervalSec",
        "StartLimitBurst",
        "Restart",
        "RestartSec",
        "TimeoutStartSec",
        "TimeoutStopSec",
        "TimeoutSec",
        // Fedora ships a drop-in for every service setting
        // `TimeoutStopFailureMode=abort`.
        "TimeoutStopFailureMode",
        "TimeoutStartFailureMode",
        "TimeoutAbortSec",
        "Nice",
        "OOMScoreAdjust",
        "CPUQuota",
        "CPUWeight",
        "CPUShares",
        "IOWeight",
        "MemoryMin",
        "MemoryLow",
        "MemoryHigh",
        "MemoryMax",
        "MemorySwapMax",
        "TasksMax",
        "LimitNOFILE",
        "LimitNPROC",
        "LimitCORE",
        "LimitMEMLOCK",
        "Environment",
        "WantedBy",
    ];
    // systemd joins a line ending in `\` with the next one.
    let joined = contents.replace("\\\n", " ");
    joined.lines().any(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with(['#', ';', '[']) {
            return false;
        }
        let Some((key, value)) = line.split_once('=') else {
            return true;
        };
        let key = key.trim();
        // An empty `Environment=` clears every earlier assignment, the
        // unit's TEMPS_DATA_DIR included.
        !ALLOWED.contains(&key)
            || (key == "Environment"
                && (value.trim().is_empty() || value.contains("TEMPS_DATA_DIR")))
    })
}

/// Whether the agent can start on the config join just saved: what
/// `temps agent` needs from `agent.json` before it serves anything.
pub(super) fn check_joined_config(data_dir: &Path) -> Result<(), String> {
    let path = data_dir.join("agent.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let config: temps_node_agent::AgentConfig = serde_json::from_str(&text)
        .map_err(|error| format!("{} does not parse: {error}", path.display()))?;
    if config.private_address.is_none() {
        return Err(format!("{} has no private_address", path.display()));
    }
    let files = [
        ("certificate", &config.tls_cert_path),
        ("key", &config.tls_key_path),
        ("cluster CA", &config.cluster_ca_path),
    ];
    for (what, file) in files {
        match file {
            Some(file) if !file.is_file() => {
                return Err(format!(
                    "{} names the TLS {what} {}, which is missing",
                    path.display(),
                    file.display()
                ))
            }
            None if config.require_mtls => {
                return Err(format!(
                    "{} requires mTLS but names no TLS {what}",
                    path.display()
                ))
            }
            _ => {}
        }
    }
    Ok(())
}

/// Whether `up` holds on every sample for `window`.
pub(super) fn stays_up(mut up: impl FnMut() -> bool, window: Duration, interval: Duration) -> bool {
    let deadline = Instant::now() + window;
    loop {
        if !up() {
            return false;
        }
        if Instant::now() >= deadline {
            return true;
        }
        std::thread::sleep(interval);
    }
}

/// A read-only `systemctl` query, bounded by [`QUERY_TIMEOUT`].
pub(super) fn systemctl_query(args: &[&str]) -> Result<String, String> {
    systemctl_output(args, QUERY_TIMEOUT)
}

/// Run `systemctl args`, killed after `timeout`, and return its stdout.
fn systemctl_output(args: &[&str], timeout: Duration) -> Result<String, String> {
    // By absolute path: join runs as root, maybe with the caller's PATH.
    let program = ["/usr/bin/systemctl", "/bin/systemctl"]
        .into_iter()
        .find(|candidate| Path::new(candidate).is_file())
        .ok_or_else(|| "systemctl is not in /usr/bin or /bin".to_string())?;
    let mut command = Command::new(program);
    command.args(args);
    let rendered = format!("systemctl {}", args.join(" "));
    match temps_wireguard::vm::output_within(command, None, timeout) {
        Ok(Some(output)) if output.status.success() => {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        Ok(Some(output)) => Err(format!(
            "`{rendered}` failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Ok(None) => Err(format!(
            "`{rendered}` did not finish within {}s and was stopped",
            timeout.as_secs()
        )),
        Err(error) => Err(format!("could not run `{rendered}`: {error}")),
    }
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

/// The start hint's lines. A Mac joined with its Docker app (ADR 090) runs
/// the agent as a LaunchAgent, as the user; any other Mac join has no
/// service to install (the LaunchAgent needs the Docker app's sidecar), so
/// it only gets the foreground command; Linux gets the systemd service.
pub fn start_agent_hint(mac: bool, mac_worker: bool) -> Vec<&'static str> {
    let mut lines = vec!["Start the worker:"];
    if !mac {
        lines.push(
            "  temps agent service install   as a systemd service, restarted on failure and at boot (root)",
        );
    } else if mac_worker {
        lines.push(
            "  temps agent service install   as a LaunchAgent, as you (no sudo), restarted on exit and at login",
        );
    }
    lines.push("  temps agent                   in the foreground, or under your own supervisor");
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_start_hint_names_the_platform_service() {
        let linux = start_agent_hint(false, false);
        assert!(
            linux[1].contains("systemd") && linux[1].contains("(root)"),
            "{linux:?}"
        );
        let mac = start_agent_hint(true, true);
        assert!(
            mac[1].contains("LaunchAgent") && mac[1].contains("no sudo"),
            "{mac:?}"
        );
        assert!(!mac.iter().any(|line| line.contains("systemd")), "{mac:?}");

        // A direct Mac join has no Docker app sidecar for the LaunchAgent.
        let direct = start_agent_hint(true, false);
        assert!(
            !direct.iter().any(|line| line.contains("service install")),
            "{direct:?}"
        );
        assert!(
            direct.iter().any(|line| line.contains("foreground")),
            "{direct:?}"
        );
    }

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
    fn join_restarts_only_the_managed_unit_for_its_own_data_dir() {
        let binary = Path::new("/usr/local/bin/temps");
        let unit = render_unit(binary, Path::new("/root/.temps")).unwrap();
        assert_eq!(
            classify_unit(&unit, Path::new("/root/.temps")),
            UnitFile::Ours {
                binary: binary.to_path_buf()
            }
        );
        let foreign = unit.replacen(MANAGED_MARKER, "# mine", 1);
        assert_eq!(
            classify_unit(&foreign, Path::new("/root/.temps")),
            UnitFile::Foreign
        );

        // The incident's shape with a moved data dir: Temps' unit keeps the
        // old config, so join warns instead of printing the install hint.
        for other in ["/srv/temps", "/root/.temps/sub"] {
            let UnitFile::Refused(reason) = classify_unit(&unit, Path::new(other)) else {
                panic!("a unit for /root/.temps must not run {other}");
            };
            assert!(
                reason.contains("/root/.temps") && reason.contains(other),
                "{reason}"
            );
        }

        // Hand edits and older templates are left to install.
        for edited in [
            unit.replace("KillMode=process\n", ""),
            unit.replace(
                "UMask=0077\n",
                "UMask=0077\nEnvironment=\"TEMPS_DATA_DIR=/srv/other\"\n",
            ),
            unit.replace(
                "ExecStart=\"/usr/local/bin/temps\" agent\n",
                "ExecStart=\"/usr/local/bin/temps\" agent\nExecStartPre=/tmp/x\n",
            ),
        ] {
            assert!(
                matches!(
                    classify_unit(&edited, Path::new("/root/.temps")),
                    UnitFile::Refused(_)
                ),
                "{edited}"
            );
        }
        assert_eq!(
            unit_data_dir(&render_unit(binary, Path::new("/srv/a \"b\" 100%")).unwrap()),
            Some(PathBuf::from("/srv/a \"b\" 100%"))
        );
    }

    #[test]
    fn commands_join_prints_are_shell_quoted() {
        assert_eq!(shell_quoted(Path::new("/root/.temps")), "/root/.temps");
        assert_eq!(shell_quoted(Path::new("/srv/my temps")), "'/srv/my temps'");
        assert_eq!(shell_quoted(Path::new("/srv/it's")), r"'/srv/it'\''s'");
        assert_eq!(shell_quoted(Path::new("/srv/$(x)")), "'/srv/$(x)'");
    }

    #[test]
    fn join_reads_back_the_binary_the_unit_runs() {
        let unit =
            render_unit(Path::new("/usr/local/bin/temps"), Path::new("/root/.temps")).unwrap();
        assert_eq!(
            unit_binary(&unit),
            Some(PathBuf::from("/usr/local/bin/temps"))
        );
        let odd = Path::new("/opt/te\"mp%s\\x/temps");
        let unit = render_unit(odd, Path::new("/root/.temps")).unwrap();
        assert_eq!(unit_binary(&unit).as_deref(), Some(odd));
        assert_eq!(unit_binary("ExecStart=/bin/sh -c evil"), None);
        assert_eq!(unit_binary("ExecStart=\"/bin/sh\" -c evil"), None);
    }

    fn loaded(fragment: &str, drop_ins: &str) -> LoadedUnit {
        LoadedUnit::parse(&format!(
            "FragmentPath={fragment}\nDropInPaths={drop_ins}\nNeedDaemonReload=no\n\
             ActiveState=active\nUnitFileState=enabled\n"
        ))
    }

    #[test]
    fn join_restarts_only_the_unit_systemd_loaded_from_the_temps_file() {
        let no_files = |path: &str| -> std::io::Result<String> {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                path.to_string(),
            ))
        };
        let ours = loaded(UNIT_PATH, "");
        assert!(ours.wanted());
        assert_eq!(ours.check(UNIT_PATH, no_files), Ok(()));

        // A unit elsewhere (e.g. a transient one) shadows the file Temps wrote.
        let shadowed = loaded("/run/systemd/transient/temps-agent.service", "");
        let error = shadowed.check(UNIT_PATH, no_files).unwrap_err();
        assert!(error.contains("/run/systemd/transient"), "{error}");

        let mut stale = loaded(UNIT_PATH, "");
        stale.need_daemon_reload = true;
        assert!(stale
            .check(UNIT_PATH, no_files)
            .unwrap_err()
            .contains("daemon-reload"));

        // A drop-in limiting memory is fine; one moving the data dir is not.
        let drop_in = "/etc/systemd/system/temps-agent.service.d/override.conf";
        let with_drop_in = loaded(UNIT_PATH, drop_in);
        let limits =
            |_: &str| Ok("[Service]\nMemoryMax=1G\nEnvironment=RUST_LOG=debug\n".to_string());
        assert_eq!(with_drop_in.check(UNIT_PATH, limits), Ok(()));
        let moved =
            |_: &str| Ok("[Service]\nEnvironment=\"TEMPS_DATA_DIR=/srv/other\"\n".to_string());
        assert!(with_drop_in
            .check(UNIT_PATH, moved)
            .unwrap_err()
            .contains(drop_in));
        assert!(
            with_drop_in.check(UNIT_PATH, no_files).is_err(),
            "an unreadable drop-in"
        );
        for key in [
            "ExecStart=",
            "ExecStart=/tmp/evil agent",
            "ExecStartPre=/tmp/evil",
            "User=nobody",
            "EnvironmentFile=/tmp/env",
            "BindPaths=/srv/x:/root/.temps",
            "WorkingDirectory=/tmp",
            "PassEnvironment=TEMPS_DATA_DIR",
            "Environment=A=1 \\\n  TEMPS_DATA_DIR=/srv/x",
            "Environment=",
            "Environment=  ",
            "UnsetEnvironment=TEMPS_DATA_DIR",
            "not a setting",
        ] {
            assert!(drop_in_overrides(&format!("[Service]\n{key}\n")), "{key}");
        }
        assert!(!drop_in_overrides(
            "# tuning\n[Unit]\nStartLimitBurst=10\n\n[Service]\nLimitNOFILE=65536\n; done\n"
        ));
        // Fedora's drop-in for every service (service.d/10-timeout-abort.conf).
        assert!(!drop_in_overrides(
            "# Abort services that do not stop in time.\n[Service]\nTimeoutStopFailureMode=abort\n"
        ));
        let off =
            LoadedUnit::parse("FragmentPath=x\nActiveState=inactive\nUnitFileState=disabled\n");
        assert!(!off.wanted(), "stopped and disabled stays off");
        let enabled = LoadedUnit::parse("ActiveState=failed\nUnitFileState=enabled\n");
        assert!(enabled.wanted(), "enabled but crashed on the old config");
    }

    #[test]
    fn join_checks_the_saved_config_before_restarting() {
        let dir = tempfile::tempdir().unwrap();
        let error = check_joined_config(dir.path()).unwrap_err();
        assert!(error.contains("agent.json"), "{error}");

        std::fs::write(dir.path().join("agent.json"), "{not json").unwrap();
        assert!(check_joined_config(dir.path())
            .unwrap_err()
            .contains("does not parse"));

        let cert = dir.path().join("node.crt");
        let config = |cert_path: &Path, require_mtls: bool| temps_node_agent::AgentConfig {
            listen_address: "10.0.0.5:3100".into(),
            token: "t".into(),
            node_name: "worker".into(),
            control_plane_url: "https://cp.example.com".into(),
            node_id: 1,
            labels: serde_json::json!({}),
            dns_data_dir: dir.path().join("dns"),
            tls_cert_path: Some(cert_path.to_path_buf()),
            tls_key_path: None,
            cluster_ca_path: None,
            require_mtls,
            underlay_dev: None,
            underlay_mtu: None,
            private_address: Some("10.0.0.5".into()),
            public_ingress_address: None,
            public_ingress_http_port: 80,
            public_ingress_https_port: 443,
            public_ingress_private_key: None,
            ingress: None,
            mesh_key_dir: dir.path().join("wireguard"),
            wg_endpoint: None,
            control_plane_trust: None,
            control_plane_legacy_cert_path: None,
            join_url: None,
            overlay_network_name: None,
        };
        let save = |config: &temps_node_agent::AgentConfig| {
            std::fs::write(
                dir.path().join("agent.json"),
                serde_json::to_string(config).unwrap(),
            )
            .unwrap();
        };
        save(&config(&cert, false));
        let error = check_joined_config(dir.path()).unwrap_err();
        assert!(
            error.contains("node.crt") && error.contains("missing"),
            "{error}"
        );

        std::fs::write(&cert, "pem").unwrap();
        assert_eq!(check_joined_config(dir.path()), Ok(()));

        save(&config(&cert, true));
        let error = check_joined_config(dir.path()).unwrap_err();
        assert!(error.contains("requires mTLS"), "{error}");
    }

    #[test]
    fn a_restart_counts_only_when_the_service_stays_up() {
        let window = Duration::from_millis(50);
        let interval = Duration::from_millis(5);
        assert!(stays_up(|| true, window, interval));
        let mut samples = 0;
        assert!(!stays_up(
            || {
                samples += 1;
                samples < 3
            },
            window,
            interval
        ));
        assert_eq!(samples, 3, "stops at the first sample that is down");
    }

    /// A restart of the agent must not kill the sandbox VMs it started.
    #[test]
    fn the_guard_unit_loads_the_guard_before_docker_only_when_present() {
        let unit =
            render_guard_unit(Path::new("/usr/sbin/nft"), Path::new("/root/.temps")).unwrap();
        assert!(unit.lines().any(|line| line == MANAGED_MARKER));
        assert!(unit.contains("Before=docker.service"));
        assert!(unit.contains("WantedBy=multi-user.target docker.service"));
        assert!(unit.contains("ConditionPathExists=/root/.temps/netpol-boot-guard.nft"));
        assert!(
            unit.contains("ExecStart=\"/usr/sbin/nft\" -f \"/root/.temps/netpol-boot-guard.nft\"")
        );
        assert!(unit.contains("DefaultDependencies=no"));
        assert!(
            render_guard_unit(Path::new("/usr/sbin/nft"), Path::new("/srv/my data")).is_err(),
            "a path ConditionPathExists= cannot spell is refused"
        );
    }

    #[test]
    fn the_unit_stops_only_the_agent_so_sandbox_vms_survive_a_restart() {
        let unit =
            render_unit(Path::new("/usr/local/bin/temps"), Path::new("/root/.temps")).unwrap();
        let service = unit
            .split("[Service]")
            .nth(1)
            .and_then(|rest| rest.split("[Install]").next())
            .unwrap();
        let kill_modes: Vec<&str> = service
            .lines()
            .filter(|line| line.starts_with("KillMode="))
            .collect();
        assert_eq!(kill_modes, ["KillMode=process"], "{unit}");
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
        assert!(require_root_owned(
            ServiceAction::Install,
            TrustedPath::Binary,
            binary,
            table(&safe)
        )
        .is_ok());

        // A root-owned binary in a user's directory can be swapped by that user.
        let in_home = [
            ("/", 0, 0o40755),
            ("/home", 0, 0o40755),
            ("/home/dev", 1000, 0o40755),
            ("/home/dev/temps", 0, 0o100755),
        ];
        let error = require_root_owned(
            ServiceAction::Install,
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
            ServiceAction::Install,
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
            ServiceAction::Install,
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
            ServiceAction::Install,
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
        let (role, error) =
            refuse_unsafe_paths(ServiceAction::Install, &binary, &data_dir, &config).unwrap_err();
        let error = error.to_string();
        assert_eq!(role, TrustedPath::Binary, "{error}");
        assert!(
            error.starts_with("refusing to install temps-agent.service"),
            "{error}"
        );
        assert!(error.contains("rerun with `--binary"), "{error}");

        // The same refusal when join would restart the service, worded for
        // join: there is no install to rerun.
        let (role, error) =
            refuse_unsafe_paths(ServiceAction::Restart, &binary, &data_dir, &config).unwrap_err();
        let error = error.to_string();
        assert_eq!(role, TrustedPath::Binary, "{error}");
        assert!(
            error.starts_with("join will not restart temps-agent.service"),
            "{error}"
        );
        assert!(
            error.contains("reinstall the service with `--binary /opt/temps/bin/temps`"),
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
