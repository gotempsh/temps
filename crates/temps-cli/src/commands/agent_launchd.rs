// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `temps agent service` on macOS (ADR 090): a launchd LaunchAgent, the
//! counterpart of the systemd unit on Linux.
//!
//! A LaunchAgent rather than a LaunchDaemon: a Mac worker's containers run in
//! the user's Docker app (Docker Desktop, OrbStack, colima, Rancher Desktop),
//! which runs in the user's login session, with its socket under the user's
//! home. Before login there is no Docker to drive, and a root daemon would
//! have to reach into a user's home for a socket it does not own. The agent
//! runs as the user who joined, starts at login, and launchd restarts it
//! whenever it exits.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::agent_service::{AgentService, AgentServiceCommand, InstallArgs, ServiceAfterJoin};

/// The launchd label, and the plist's file name.
pub const LABEL: &str = "sh.temps.agent";
const MANAGED_MARKER: &str = "Managed by Temps. Do not edit by hand.";
/// Seconds launchd waits before starting the agent again after it exits.
const THROTTLE_SECS: u32 = 10;

fn home() -> anyhow::Result<PathBuf> {
    dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot find your home directory"))
}

pub fn plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

pub fn log_path(home: &Path) -> PathBuf {
    home.join("Library/Logs/Temps/agent.log")
}

fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

fn domain() -> String {
    format!("gui/{}", uid())
}

fn service_target() -> String {
    format!("{}/{LABEL}", domain())
}

pub fn execute(command: &AgentServiceCommand) -> anyhow::Result<()> {
    match command {
        AgentServiceCommand::Install(args) => install(args),
        AgentServiceCommand::Uninstall(args) => uninstall(args.remove_mesh),
        AgentServiceCommand::Status => status(),
    }
}

/// `value` escaped for a plist `<string>`.
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// The LaunchAgent: `binary agent` with the agent's data directory, the
/// Docker CLI's directory on `PATH` (launchd's default `PATH` has none of
/// the places Docker apps install it), restarted whenever it exits.
pub fn render_plist(
    binary: &Path,
    data_dir: &Path,
    docker_cli: &Path,
    log: &Path,
) -> anyhow::Result<String> {
    let text = |path: &Path| -> anyhow::Result<String> {
        let value = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("{} is not valid UTF-8", path.display()))?;
        if !path.is_absolute() {
            anyhow::bail!("{value} is not an absolute path");
        }
        if value.chars().any(char::is_control) {
            anyhow::bail!("paths cannot contain control characters");
        }
        Ok(xml_escape(value))
    };
    let docker_dir = docker_cli
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no directory", docker_cli.display()))?;
    let path_env = format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", text(docker_dir)?);
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- {MANAGED_MARKER} -->
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{binary}</string>
    <string>agent</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>TEMPS_DATA_DIR</key>
    <string>{data_dir}</string>
    <key>PATH</key>
    <string>{path_env}</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ThrottleInterval</key>
  <integer>{THROTTLE_SECS}</integer>
  <key>ProcessType</key>
  <string>Standard</string>
  <key>Umask</key>
  <integer>63</integer>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
        binary = text(binary)?,
        data_dir = text(data_dir)?,
        log = text(log)?,
    ))
}

/// Whether someone other than this user (or root) could change `path`: an
/// ancestor owned by another user, group-writable by a group other than
/// `wheel` or `admin` (whose members can already act as root on macOS), or
/// world-writable without the sticky bit.
fn unsafe_component(path: &Path) -> anyhow::Result<Option<String>> {
    use std::os::unix::fs::MetadataExt;
    for component in path.ancestors() {
        let meta = std::fs::metadata(component)
            .map_err(|error| anyhow::anyhow!("could not check {}: {error}", component.display()))?;
        if !path_is_safe(meta.uid(), meta.gid(), meta.mode(), uid()) {
            return Ok(Some(component.display().to_string()));
        }
    }
    Ok(None)
}

/// macOS's `wheel` and `admin` groups: their members can act as root.
const ROOT_EQUIVALENT_GROUPS: [u32; 2] = [0, 80];

fn path_is_safe(owner: u32, group: u32, mode: u32, me: u32) -> bool {
    let sticky = mode & 0o1000 != 0;
    let owner_ok = owner == me || owner == 0;
    let group_writable = mode & 0o020 != 0 && !sticky && !ROOT_EQUIVALENT_GROUPS.contains(&group);
    let world_writable = mode & 0o002 != 0 && !sticky;
    owner_ok && !group_writable && !world_writable
}

/// The data directory an installed plist runs the agent with, so uninstall
/// and status act on the worker that was installed, not on the current
/// shell's default.
fn installed_data_dir(plist: &str) -> Option<PathBuf> {
    let after_key = plist.split("<key>TEMPS_DATA_DIR</key>").nth(1)?;
    let value = after_key
        .split("<string>")
        .nth(1)?
        .split("</string>")
        .next()?;
    let value = value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&");
    Some(PathBuf::from(value))
}

/// How long one `launchctl` call may take: bootstrapping or booting out a
/// LaunchAgent takes well under a second, so a call still running after
/// this is stuck.
const LAUNCHCTL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn launchctl(args: &[&str]) -> anyhow::Result<std::process::Output> {
    launchctl_within(args, LAUNCHCTL_TIMEOUT)
}

fn launchctl_within(
    args: &[&str],
    timeout: std::time::Duration,
) -> anyhow::Result<std::process::Output> {
    let mut command = Command::new("/bin/launchctl");
    command.args(args);
    output_within_deadline(command, timeout)
}

/// Run `command` to completion, killing it and failing if it takes longer
/// than `timeout`, so a service command never waits forever.
fn output_within_deadline(
    command: Command,
    timeout: std::time::Duration,
) -> anyhow::Result<std::process::Output> {
    let program = command.get_program().to_string_lossy().into_owned();
    let args: Vec<String> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    match temps_wireguard::vm::output_within(command, None, timeout) {
        Ok(Some(output)) => Ok(output),
        Ok(None) => anyhow::bail!(
            "`{program} {}` did not finish within {}s and was stopped",
            args.join(" "),
            timeout.as_secs_f32()
        ),
        Err(error) => anyhow::bail!("could not run `{program} {}`: {error}", args.join(" ")),
    }
}

fn install(args: &InstallArgs) -> anyhow::Result<()> {
    if uid() == 0 {
        anyhow::bail!(
            "Run `temps agent service install` without sudo on a Mac: the agent runs as you, \
             next to your Docker app, as a LaunchAgent in your login session."
        );
    }
    let home = home()?;
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
    if !data_dir.join("agent.json").is_file() {
        anyhow::bail!(
            "{} is missing: run `temps join` on this Mac first",
            data_dir.join("agent.json").display()
        );
    }
    let worker = temps_node_agent::mac_worker::MacWorker::load(&data_dir)?.ok_or_else(|| {
        anyhow::anyhow!(
            "{} is missing: this Mac joined without its Docker app's mesh sidecar (an older \
             Temps, or --private-address). Run `temps join` again with this version.",
            temps_node_agent::mac_worker::MacWorker::path(&data_dir).display()
        )
    })?;
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
    for (what, path) in [
        ("Temps binary", &binary),
        ("agent data directory", &data_dir),
    ] {
        if let Some(unsafe_path) = unsafe_component(path)? {
            anyhow::bail!(
                "The {what} {} is not safe to run from: {unsafe_path} can be changed by another \
                 user, who could then run code as you. Move it somewhere only you (or root) can \
                 write, e.g. `brew install` or ~/.temps/bin, and pass --binary.",
                path.display()
            );
        }
    }
    let log = log_path(&home);
    let plist = render_plist(&binary, &data_dir, Path::new(&worker.docker_cli), &log)?;
    if args.dry_run {
        print!("{plist}");
        return Ok(());
    }
    let path = plist_path(&home);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        if !existing.contains(MANAGED_MARKER) {
            anyhow::bail!(
                "{} exists and was not written by Temps; move it away and run this again",
                path.display()
            );
        }
    }
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Unload a previous copy first: bootstrap refuses a loaded label.
    let _ = launchctl(&["bootout", &service_target()]);
    let temporary = path.with_extension("plist.tmp");
    std::fs::write(&temporary, plist)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o644))?;
    }
    std::fs::rename(&temporary, &path)?;
    let path_text = path.to_string_lossy().into_owned();
    let loaded = launchctl(&["bootstrap", &domain(), &path_text])?;
    if !loaded.status.success() {
        anyhow::bail!(
            "launchctl bootstrap {} {} failed: {}",
            domain(),
            path.display(),
            String::from_utf8_lossy(&loaded.stderr).trim()
        );
    }
    let _ = launchctl(&["enable", &service_target()]);
    let kicked = launchctl(&["kickstart", "-k", &service_target()])?;
    if !kicked.status.success() {
        anyhow::bail!(
            "launchctl kickstart {} failed: {}",
            service_target(),
            String::from_utf8_lossy(&kicked.stderr).trim()
        );
    }
    println!("Installed and started the {LABEL} LaunchAgent (starts at login, restarts on exit).");
    println!("  Plist:  {}", path.display());
    println!("  Status: temps agent service status");
    println!("  Logs:   tail -f {}", log.display());
    println!(
        "  The agent needs {} running; start it at login so the worker comes back after a \
         restart.",
        worker.docker_app.display_name()
    );
    Ok(())
}

fn uninstall(remove_mesh: bool) -> anyhow::Result<()> {
    let home = home()?;
    let path = plist_path(&home);
    let mut data_dir = super::agent::agent_data_dir();
    if path.exists() {
        let existing = std::fs::read_to_string(&path)?;
        if let Some(installed) = installed_data_dir(&existing) {
            data_dir = installed;
        }
        if !existing.contains(MANAGED_MARKER) {
            anyhow::bail!(
                "{} was not written by Temps; leaving it alone",
                path.display()
            );
        }
        let _ = launchctl(&["bootout", &service_target()]);
        std::fs::remove_file(&path)?;
        println!("Removed the {LABEL} LaunchAgent.");
    } else {
        println!("The {LABEL} LaunchAgent is not installed.");
    }
    if remove_mesh {
        match temps_node_agent::mac_worker::MacWorker::load(&data_dir)? {
            Some(worker) => {
                let endpoint = worker.endpoint();
                temps_node_agent::mac_worker::remove_sidecar(&endpoint)?;
                temps_node_agent::mac_worker::remove_mesh_interface(&endpoint)?;
                println!(
                    "Removed the mesh sidecar and this worker's mesh interface from {}'s VM.",
                    worker.docker_app.display_name()
                );
            }
            None => println!(
                "No {}: no mesh sidecar to remove.",
                temps_node_agent::mac_worker::MacWorker::path(&data_dir).display()
            ),
        }
    } else {
        println!(
            "The mesh sidecar keeps running in the Docker app's VM; pass --remove-mesh to remove \
             it too."
        );
    }
    Ok(())
}

fn status() -> anyhow::Result<()> {
    let printed = launchctl(&["print", &service_target()])?;
    if !printed.status.success() {
        println!("The {LABEL} LaunchAgent is not loaded (run `temps agent service install`).");
    } else {
        let text = String::from_utf8_lossy(&printed.stdout);
        for line in text.lines() {
            let line = line.trim();
            if ["state =", "pid =", "last exit code =", "runs ="]
                .iter()
                .any(|key| line.starts_with(key))
            {
                println!("  {line}");
            }
        }
    }
    let data_dir = std::fs::read_to_string(plist_path(&home()?))
        .ok()
        .and_then(|plist| installed_data_dir(&plist))
        .unwrap_or_else(super::agent::agent_data_dir);
    if let Some(worker) = temps_node_agent::mac_worker::MacWorker::load(&data_dir)? {
        let endpoint = worker.endpoint();
        let output = temps_node_agent::mac_worker::docker(
            &endpoint,
            &[
                "container".into(),
                "inspect".into(),
                "--format".into(),
                "{{.State.Status}} since {{.State.StartedAt}}".into(),
                temps_node_agent::mac_worker::SIDECAR_CONTAINER.into(),
            ],
            None,
        )?;
        let state = if output.status.success() {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        } else {
            format!(
                "not found ({} not running?)",
                worker.docker_app.display_name()
            )
        };
        println!(
            "  mesh sidecar ({}): {state}",
            temps_node_agent::mac_worker::SIDECAR_CONTAINER
        );
        println!("  mesh address: {}", worker.mesh_address);
    }
    println!("  Logs: tail -f {}", log_path(&home()?).display());
    Ok(())
}

fn launch_agent(home: &Path, as_root: bool) -> AgentService {
    // Root has no GUI domain of its own: name the user's in the command.
    let target = if as_root {
        format!("gui/$(id -u)/{LABEL}")
    } else {
        service_target()
    };
    AgentService {
        name: format!("the {LABEL} LaunchAgent"),
        restart: format!("launchctl kickstart -k {target}"),
        start: "temps agent service install".to_string(),
        logs: format!(
            "tail -n 50 {}",
            super::agent_service::shell_quoted(&log_path(home))
        ),
    }
}

/// Whether `plist` carries the marker [`render_plist`] writes.
fn plist_is_managed(plist: &str) -> bool {
    let marker = format!("<!-- {MANAGED_MARKER} -->");
    plist.lines().any(|line| line == marker)
}

/// The binary a plist written by [`render_plist`] runs.
fn plist_binary(plist: &str) -> Option<PathBuf> {
    let value = plist
        .split("<key>ProgramArguments</key>")
        .nth(1)?
        .split("<string>")
        .nth(1)?
        .split("</string>")
        .next()?;
    Some(PathBuf::from(
        value
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    ))
}

/// What launchd has loaded for the agent, from `launchctl print`.
#[derive(Debug, Default, PartialEq, Eq)]
struct LoadedJob {
    path: Option<String>,
    program: Option<String>,
    state: Option<String>,
    pid: Option<String>,
    data_dir: Option<String>,
}

impl LoadedJob {
    fn parse(print: &str) -> Self {
        let mut job = Self::default();
        let mut in_environment = false;
        for line in print.lines() {
            // Top-level fields are indented by exactly one tab.
            let Some(field) = line.strip_prefix('\t') else {
                continue;
            };
            if in_environment {
                if field == "}" {
                    in_environment = false;
                } else if let Some(value) = field.trim().strip_prefix("TEMPS_DATA_DIR => ") {
                    job.data_dir = Some(value.to_string());
                }
                continue;
            }
            if field.starts_with('\t') {
                continue;
            }
            if field == "environment = {" {
                in_environment = true;
            } else if let Some(value) = field.strip_prefix("path = ") {
                job.path = Some(value.to_string());
            } else if let Some(value) = field.strip_prefix("program = ") {
                job.program = Some(value.to_string());
            } else if let Some(value) = field.strip_prefix("state = ") {
                job.state = Some(value.to_string());
            } else if let Some(value) = field.strip_prefix("pid = ") {
                job.pid = Some(value.to_string());
            }
        }
        job
    }

    /// Whether launchd runs the plist at `plist` for `data_dir`, running
    /// `binary`: the job a kickstart restarts is the definition launchd
    /// loaded, which may no longer match the file, so the binary checked
    /// before the restart must be the one launchd runs.
    fn check(&self, plist: &Path, data_dir: &Path, binary: &Path) -> Result<(), String> {
        let plist = plist.to_string_lossy();
        if self.path.as_deref() != Some(plist.as_ref()) {
            return Err(format!(
                "launchd loaded {LABEL} from '{}', not from {plist}, so join leaves it alone",
                self.path.as_deref().unwrap_or("an unknown path")
            ));
        }
        let binary = binary.to_string_lossy();
        if self.program.as_deref() != Some(binary.as_ref()) {
            return Err(format!(
                "the loaded {LABEL} runs '{}', not {binary} as its plist says; run `temps agent \
                 service install` to reload it",
                self.program.as_deref().unwrap_or("an unknown program")
            ));
        }
        let data_dir = data_dir.to_string_lossy();
        if self.data_dir.as_deref() != Some(data_dir.as_ref()) {
            return Err(format!(
                "the loaded {LABEL} runs from '{}', not {data_dir}; run `temps agent service \
                 install` to reload it",
                self.data_dir
                    .as_deref()
                    .unwrap_or("an unknown data directory")
            ));
        }
        Ok(())
    }
}

/// [`super::agent_service::restart_after_join`] on macOS: kickstart the
/// LaunchAgent when it is the plist Temps wrote for `data_dir` and launchd
/// has it loaded, after checking the agent can start on the new config, and
/// watch that it stays up.
pub fn restart_after_join(data_dir: &Path) -> ServiceAfterJoin {
    let Ok(home) = home() else {
        return ServiceAfterJoin::NotInstalled;
    };
    // Root has no LaunchAgent of its own and sudo points HOME at root's, so
    // a root join cannot see the user's: say so instead of guessing.
    if uid() == 0 {
        return ServiceAfterJoin::NotRestarted {
            service: launch_agent(&home, true),
            reason: "this ran as root, so it used root's agent config, and cannot reach a \
                     LaunchAgent, which runs as you. On a Mac, run `temps join` and `temps agent \
                     labels` without sudo"
                .to_string(),
            fix: None,
        };
    }
    let path = plist_path(&home);
    let Ok(plist) = std::fs::read_to_string(&path) else {
        return ServiceAfterJoin::NotInstalled;
    };
    if !plist_is_managed(&plist) {
        return ServiceAfterJoin::NotInstalled;
    }
    let service = launch_agent(&home, false);
    let reinstall = format!(
        "temps agent service install --data-dir {}",
        super::agent_service::shell_quoted(data_dir)
    );
    let refuse = |reason: String, fix: &str| ServiceAfterJoin::NotRestarted {
        service: launch_agent(&home, false),
        reason,
        fix: Some(fix.to_string()),
    };
    match installed_data_dir(&plist) {
        Some(installed) if installed == data_dir => {}
        installed => {
            return refuse(
                format!(
                    "it runs the agent from {}, but the config being applied is in {}, so it \
                     was left alone and it keeps the old config",
                    installed.map_or_else(
                        || "an unknown directory".to_string(),
                        |dir| dir.display().to_string()
                    ),
                    data_dir.display()
                ),
                &reinstall,
            )
        }
    }
    let Some(binary) = plist_binary(&plist) else {
        return refuse(format!("{} names no program", path.display()), &reinstall);
    };
    let loaded = match launchctl(&["print", &service_target()]) {
        Ok(output) if output.status.success() => {
            LoadedJob::parse(&String::from_utf8_lossy(&output.stdout))
        }
        // Not loaded: booted out, so someone turned it off.
        Ok(_) => return ServiceAfterJoin::Stopped(service),
        Err(error) => return refuse(error.to_string(), &service.restart),
    };
    if let Err(reason) = loaded.check(&path, data_dir, &binary) {
        return refuse(reason, &reinstall);
    }
    if let Err(reason) = super::agent_service::check_joined_config(data_dir) {
        return refuse(reason, &service.restart);
    }
    match temps_node_agent::mac_worker::MacWorker::load(data_dir) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return refuse(
                format!(
                    "{} is missing: the LaunchAgent needs the Docker app's mesh sidecar, which a \
                 join with --private-address does not set up",
                    temps_node_agent::mac_worker::MacWorker::path(data_dir).display()
                ),
                &service.restart,
            )
        }
        Err(error) => return refuse(error.to_string(), &service.restart),
    }
    // Install wrote the canonical path; a symlink there now could point
    // somewhere the ownership check below would not cover.
    match std::fs::canonicalize(&binary) {
        Ok(canonical) if canonical == binary && canonical.is_file() => {}
        Ok(canonical) => {
            return refuse(
                format!(
                    "{} runs {}, which now resolves to {}",
                    path.display(),
                    binary.display(),
                    canonical.display()
                ),
                &reinstall,
            )
        }
        Err(error) => {
            return refuse(
                format!(
                    "{} runs {}, which is unusable ({error})",
                    path.display(),
                    binary.display()
                ),
                &reinstall,
            )
        }
    }
    // The same check install makes, against a binary swapped since.
    match unsafe_component(&binary) {
        Ok(None) => {}
        Ok(Some(unsafe_path)) => {
            return refuse(
                format!(
                    "join will not restart it: {} is not safe to run, {unsafe_path} can be \
                     changed by another user",
                    binary.display()
                ),
                &reinstall,
            )
        }
        Err(error) => return refuse(error.to_string(), &service.restart),
    }
    match launchctl(&["kickstart", "-k", &service_target()]) {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            return refuse(
                format!(
                    "`launchctl kickstart -k {}` failed: {}",
                    service_target(),
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
                &service.restart,
            )
        }
        Err(error) => return refuse(error.to_string(), &service.restart),
    }
    // Running, under one pid: launchd respawns a crashed agent only after
    // its throttle interval, longer than the window.
    let mut first_pid: Option<String> = None;
    let running = || {
        let Ok(output) = launchctl_within(
            &["print", &service_target()],
            super::agent_service::SAMPLE_TIMEOUT,
        ) else {
            return false;
        };
        let job = LoadedJob::parse(&String::from_utf8_lossy(&output.stdout));
        if !output.status.success() || job.state.as_deref() != Some("running") {
            return false;
        }
        match (&first_pid, job.pid) {
            (None, pid) => {
                first_pid = pid;
                true
            }
            (Some(first), pid) => pid.as_ref() == Some(first),
        }
    };
    if !super::agent_service::stays_up(
        running,
        super::agent_service::STAY_UP_WINDOW,
        super::agent_service::STAY_UP_INTERVAL,
    ) {
        return refuse(
            format!(
                "it restarted but did not stay up for {}s, so the agent likely rejects this \
                 config; see its logs: {}",
                super::agent_service::STAY_UP_WINDOW.as_secs(),
                service.logs
            ),
            &service.restart,
        );
    }
    ServiceAfterJoin::Restarted(service)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stuck_service_command_is_stopped_at_the_deadline() {
        let mut command = Command::new("/bin/sleep");
        command.arg("30");
        let started = std::time::Instant::now();
        let error = output_within_deadline(command, std::time::Duration::from_millis(200))
            .expect_err("a command past its deadline fails");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        let message = error.to_string();
        assert!(message.contains("/bin/sleep 30"), "{message}");
        assert!(message.contains("did not finish within"), "{message}");

        let mut command = Command::new("/bin/echo");
        command.arg("ok");
        let output = output_within_deadline(command, std::time::Duration::from_secs(10))
            .expect("a quick command finishes");
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "ok");
    }

    #[test]
    fn the_plist_runs_the_agent_with_its_data_dir_and_restarts_it() {
        let plist = render_plist(
            Path::new("/opt/homebrew/bin/temps"),
            Path::new("/Users/someone/.temps"),
            Path::new("/opt/homebrew/bin/docker"),
            Path::new("/Users/someone/Library/Logs/Temps/agent.log"),
        )
        .unwrap();
        assert!(plist.contains("<string>sh.temps.agent</string>"));
        assert!(
            plist.contains("<string>/opt/homebrew/bin/temps</string>\n    <string>agent</string>")
        );
        assert!(
            plist.contains("<key>TEMPS_DATA_DIR</key>\n    <string>/Users/someone/.temps</string>")
        );
        assert!(plist.contains("<string>/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n  <true/>"));
        assert!(plist.contains("<key>KeepAlive</key>\n  <true/>"));
        assert!(plist.contains(MANAGED_MARKER));
    }

    #[test]
    fn only_paths_this_user_or_root_controls_are_trusted() {
        assert!(path_is_safe(501, 20, 0o755, 501));
        assert!(path_is_safe(0, 80, 0o775, 501), "group-writable by admin");
        assert!(path_is_safe(501, 0, 0o775, 501), "group-writable by wheel");
        assert!(
            path_is_safe(0, 0, 0o1777, 501),
            "a sticky /tmp-like ancestor"
        );
        assert!(!path_is_safe(502, 20, 0o755, 501), "another user's file");
        assert!(!path_is_safe(501, 20, 0o777, 501), "world-writable");
        assert!(
            !path_is_safe(501, 20, 0o775, 501),
            "group-writable by a shared group (staff): another member could swap the binary"
        );
    }

    #[test]
    fn join_restarts_only_the_temps_plist_for_its_own_data_dir() {
        let plist = render_plist(
            Path::new("/Users/someone/.temps/bin/te&mps"),
            Path::new("/Users/someone/.temps"),
            Path::new("/opt/homebrew/bin/docker"),
            Path::new("/Users/someone/Library/Logs/Temps/agent.log"),
        )
        .unwrap();
        assert!(plist_is_managed(&plist));
        assert_eq!(
            installed_data_dir(&plist),
            Some(PathBuf::from("/Users/someone/.temps"))
        );
        let foreign = plist.replacen(MANAGED_MARKER, "mine", 1);
        assert!(!plist_is_managed(&foreign));
        // The marker as text elsewhere is not the comment install writes.
        let mention = foreign.replace("<dict>", &format!("<dict><!-- x {MANAGED_MARKER} -->"));
        assert!(!plist_is_managed(&mention));
        assert_eq!(
            plist_binary(&plist),
            Some(PathBuf::from("/Users/someone/.temps/bin/te&mps"))
        );
    }

    #[test]
    fn join_restarts_only_the_job_launchd_loaded_from_that_plist() {
        let print = "gui/501/sh.temps.agent = {\n\
                     \tactive count = 1\n\
                     \tpath = /Users/someone/Library/LaunchAgents/sh.temps.agent.plist\n\
                     \ttype = LaunchAgent\n\
                     \tstate = running\n\
                     \tprogram = /opt/homebrew/bin/temps\n\
                     \n\
                     \tdefault environment = {\n\
                     \t\tTEMPS_DATA_DIR => /decoy\n\
                     \t}\n\
                     \n\
                     \tenvironment = {\n\
                     \t\tPATH => /opt/homebrew/bin:/usr/bin:/bin\n\
                     \t\tTEMPS_DATA_DIR => /Users/someone/.temps\n\
                     \t}\n\
                     \n\
                     \tpid = 4242\n\
                     \tendpoints = {\n\
                     \t\tstate = decoy\n\
                     \t}\n\
                     }\n";
        let job = LoadedJob::parse(print);
        assert_eq!(job.state.as_deref(), Some("running"));
        assert_eq!(job.pid.as_deref(), Some("4242"));
        assert_eq!(job.data_dir.as_deref(), Some("/Users/someone/.temps"));
        assert_eq!(job.program.as_deref(), Some("/opt/homebrew/bin/temps"));
        let plist = Path::new("/Users/someone/Library/LaunchAgents/sh.temps.agent.plist");
        let home = Path::new("/Users/someone/.temps");
        let binary = Path::new("/opt/homebrew/bin/temps");
        assert_eq!(job.check(plist, home, binary), Ok(()));
        let error = job
            .check(plist, Path::new("/Users/someone/other"), binary)
            .unwrap_err();
        assert!(error.contains("/Users/someone/.temps"), "{error}");
        let error = job
            .check(
                Path::new("/Users/other/Library/LaunchAgents/sh.temps.agent.plist"),
                home,
                binary,
            )
            .unwrap_err();
        assert!(error.contains("join leaves it alone"), "{error}");
        // The plist on disk names another binary than the one launchd loaded:
        // the binary join checked is not the one a kickstart would run.
        let error = job
            .check(plist, home, Path::new("/Users/someone/bin/temps"))
            .unwrap_err();
        assert!(error.contains("/opt/homebrew/bin/temps"), "{error}");
        assert_eq!(LoadedJob::parse(""), LoadedJob::default());
    }

    #[test]
    fn plist_values_are_escaped() {
        let plist = render_plist(
            Path::new("/Users/a&b/<temps>"),
            Path::new("/Users/a&b/.temps"),
            Path::new("/usr/local/bin/docker"),
            Path::new("/tmp/agent.log"),
        )
        .unwrap();
        assert!(plist.contains("/Users/a&amp;b/&lt;temps&gt;"));
        assert!(!plist.contains("a&b"));
        // Uninstall reads the data directory back from the installed plist.
        assert_eq!(
            installed_data_dir(&plist),
            Some(PathBuf::from("/Users/a&b/.temps"))
        );
        assert_eq!(installed_data_dir("<plist></plist>"), None);
    }

    #[test]
    fn relative_or_control_character_paths_are_refused() {
        assert!(render_plist(
            Path::new("temps"),
            Path::new("/d"),
            Path::new("/usr/local/bin/docker"),
            Path::new("/l")
        )
        .is_err());
        assert!(render_plist(
            Path::new("/bin/te\nmps"),
            Path::new("/d"),
            Path::new("/usr/local/bin/docker"),
            Path::new("/l")
        )
        .is_err());
    }
}
