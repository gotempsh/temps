// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `temps microsandbox` — enable the experimental microsandbox microVM
//! sandbox backend (ADR-050).
//!
//! `setup` is idempotent and needs no root:
//!
//! 1. Host: Linux with read/write `/dev/kvm`, or macOS on Apple Silicon
//!    with Hypervisor.framework.
//! 2. Runtime: the pinned `msb` + `libkrunfw` pair, installed by the
//!    microsandbox SDK into `<data_dir>/microsandbox` and verified; an
//!    existing complete pair is kept.
//! 3. Terminal agent: stage the static Linux `temps-pty-agent` that the
//!    provider writes into a guest the first time a terminal is opened
//!    (`temps sandbox shell`). Optional — sandboxes run without it, they just
//!    cannot open terminals.
//! 4. Smoke: boot a real microVM through the same provider the server uses,
//!    run a command, destroy it.
//!
//! The server registers the backend at startup when stages 1 and 2 hold,
//! so restart `temps serve` after a first-time setup.

use clap::{Args, Subcommand};
use colored::Colorize;
use std::path::PathBuf;

use temps_agents::sandbox::microsandbox::{
    microsandbox_capability, MicrosandboxSandboxConfig, MICROSANDBOX_VERSION,
};
#[cfg(not(any(target_env = "musl", all(target_os = "macos", target_arch = "x86_64"))))]
use temps_agents::sandbox::{stage_pty_agent_binary, PTY_AGENT_BINARY_NAME};

#[cfg(not(any(target_env = "musl", all(target_os = "macos", target_arch = "x86_64"))))]
use super::upgrade::{
    fetch_guest_agents_from_release, guest_agent_needs_fetch, ReleaseGuestAgents,
};
#[cfg(not(any(target_env = "musl", all(target_os = "macos", target_arch = "x86_64"))))]
use {
    std::collections::HashMap,
    std::time::{Duration, Instant},
    temps_agents::sandbox::microsandbox::MicrosandboxSandboxProvider,
    temps_agents::sandbox::{SandboxBackend, SandboxCreateConfig, SandboxProvider},
};

#[cfg(not(any(target_env = "musl", all(target_os = "macos", target_arch = "x86_64"))))]
const SMOKE_LABEL: &str = "setup-smoke";
#[cfg(not(any(target_env = "musl", all(target_os = "macos", target_arch = "x86_64"))))]
const SMOKE_IMAGE: &str = "alpine:3.20";

/// Manage the microsandbox microVM sandbox backend (experimental)
#[derive(Args)]
pub struct MicrosandboxCommand {
    #[command(subcommand)]
    pub command: MicrosandboxSubcommand,
}

#[derive(Subcommand)]
pub enum MicrosandboxSubcommand {
    /// Install the microsandbox runtime and verify it with a smoke-test VM
    Setup(MicrosandboxSetupCommand),
}

#[derive(Args)]
pub struct MicrosandboxSetupCommand {
    /// Data directory for storing configuration and runtime files. Must be
    /// the directory the server runs with (its `TEMPS_DATA_DIR`); when
    /// omitted, `~/.temps` is used, or `~/.temps/data` on installs that keep
    /// their keys there.
    #[arg(long, env = "TEMPS_DATA_DIR")]
    pub data_dir: Option<PathBuf>,

    /// Only report readiness; install nothing and exit non-zero if not ready
    #[arg(long)]
    pub check: bool,

    /// Skip the smoke-test VM boot after installing
    #[arg(long)]
    pub skip_smoke: bool,

    /// Path to the static Linux `temps-pty-agent` build for the guest
    /// architecture (the host's, on Linux; aarch64 on Apple Silicon). It is
    /// what gives sandboxes interactive terminals. Defaults to a
    /// `temps-pty-agent` file next to the running executable.
    #[arg(long)]
    pub pty_agent_bin: Option<PathBuf>,
}

impl MicrosandboxCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        match self.command {
            MicrosandboxSubcommand::Setup(cmd) => cmd.execute(),
        }
    }
}

impl MicrosandboxSetupCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(self.run())
    }

    async fn run(self) -> anyhow::Result<()> {
        let home = dirs::home_dir();
        // A running `temps serve` is the authority on its own data directory;
        // a shell inherits none of the server's TEMPS_DATA_DIR.
        let resolved =
            super::data_dir_discovery::resolve(self.data_dir.as_deref(), home.as_deref());
        let data_dir = resolved.dir.clone();
        let how = resolved.source.describe();
        let config = MicrosandboxSandboxConfig::from_data_dir(data_dir.clone());

        println!();
        println!(
            "{}",
            "  Temps microsandbox - Backend Setup (experimental)"
                .bright_white()
                .bold()
        );
        println!(
            "{}",
            "  =================================================".bright_cyan()
        );
        println!("  Data directory: {} ({})", data_dir.display(), how);
        println!("  Runtime home: {}", config.home().display());
        println!("  Runtime version: v{}", MICROSANDBOX_VERSION);
        println!();
        for warning in &resolved.warnings {
            warn(warning);
        }
        if !resolved.warnings.is_empty() {
            println!();
        }

        // The runtime is read from the *server's* data directory. A second
        // runtime under another candidate directory is the signature of a
        // setup that went somewhere the server never looks.
        if let Some(home) = &home {
            for other in other_runtime_homes(&data_dir, home) {
                warn(&format!(
                    "another microsandbox runtime exists under {}; a server started with \
                     TEMPS_DATA_DIR={} uses that one, not this one. Re-run with \
                     `--data-dir {}` if that is the server's directory",
                    other.display(),
                    other.parent().unwrap_or(&other).display(),
                    other.parent().unwrap_or(&other).display(),
                ));
            }
            println!();
        }

        if self.check {
            let capability = microsandbox_capability(&data_dir);
            report_terminal_support(&capability.terminal);
            return match capability.reason {
                None => {
                    ok("ready: the server registers this backend at startup");
                    Ok(())
                }
                Some(reason) => {
                    fail(&reason);
                    anyhow::bail!("microsandbox backend is not ready on this host")
                }
            };
        }

        self.install(config).await
    }

    /// musl and Intel macOS builds don't link the microsandbox SDK, so there
    /// is nothing to install; say why instead.
    #[cfg(any(target_env = "musl", all(target_os = "macos", target_arch = "x86_64")))]
    async fn install(self, config: MicrosandboxSandboxConfig) -> anyhow::Result<()> {
        let capability = microsandbox_capability(&config.data_dir);
        fail(&capability.reason.unwrap_or_default());
        anyhow::bail!("this temps build cannot run the microsandbox backend")
    }

    #[cfg(not(any(target_env = "musl", all(target_os = "macos", target_arch = "x86_64"))))]
    async fn install(self, config: MicrosandboxSandboxConfig) -> anyhow::Result<()> {
        let pty_agent_dest = config.pty_agent_bin();
        let provider = MicrosandboxSandboxProvider::new(config)?;

        // Stage 1: a hypervisor is a hardware/OS property; installing the
        // runtime can't fix its absence, so stop before downloading.
        if let Err(reason) = provider.probe() {
            if !matches!(
                reason,
                temps_agents::sandbox::microsandbox::MicrosandboxUnavailable::RuntimeNotInstalled { .. }
                    | temps_agents::sandbox::microsandbox::MicrosandboxUnavailable::RuntimeVersionMismatch { .. }
            ) {
                fail(&reason.to_string());
                anyhow::bail!("this host cannot run the microsandbox backend");
            }
        }
        ok("host supports hardware virtualization");

        // Stage 2: runtime pair.
        let started = Instant::now();
        let runtime = provider.install_runtime().await?;
        ok(&format!(
            "runtime ready in {:.1}s: {} ({})",
            started.elapsed().as_secs_f64(),
            runtime.msb_path.display(),
            runtime.libkrunfw_path.display()
        ));

        // Stage 3: guest terminal agent.
        let released =
            fetch_missing_pty_agent(&pty_agent_dest, self.pty_agent_bin.as_deref()).await;
        stage_pty_agent(
            &pty_agent_dest,
            self.pty_agent_bin.as_deref(),
            released
                .as_ref()
                .and_then(|r| r.path(PTY_AGENT_BINARY_NAME)),
        )?;

        // Stage 4: boot a real VM through the server's provider.
        if self.skip_smoke {
            warn("smoke test skipped");
        } else {
            smoke_test(&provider).await?;
        }

        println!();
        ok("microsandbox backend is ready; restart `temps serve` to register it");
        Ok(())
    }
}

#[cfg(not(any(target_env = "musl", all(target_os = "macos", target_arch = "x86_64"))))]
async fn smoke_test(provider: &MicrosandboxSandboxProvider) -> anyhow::Result<()> {
    let config = SandboxCreateConfig {
        run_id: 0,
        container_name_override: Some(SMOKE_LABEL.to_string()),
        // Never created: the VM's workspace is seeded from this directory,
        // and the smoke test needs an empty one.
        host_work_dir: std::env::temp_dir().join(format!(
            "temps-microsandbox-smoke-{}-empty",
            std::process::id()
        )),
        workspace_volume: None,
        image: Some(SMOKE_IMAGE.to_string()),
        cpu_limit: Some(1.0),
        memory_limit_mb: Some(256),
        pids_limit: None,
        disk_size_mb: None,
        allow_docker: false,
        network_mode: Some("none".to_string()),
        env_vars: HashMap::new(),
        idle_timeout: Duration::from_secs(60),
        backend: Some(SandboxBackend::Microsandbox),
        owner_user_id: None,
        node_id: None,
    };
    let started = Instant::now();
    let handle = provider.create(config).await?;
    let boot = started.elapsed();
    let result = provider
        .exec(
            &handle,
            vec!["uname".to_string(), "-sr".to_string()],
            HashMap::new(),
            None,
        )
        .await;
    // Always clean up the smoke VM, whatever the exec did.
    let destroyed = provider.destroy(&handle, true).await;
    let result = result?;
    destroyed?;
    if result.exit_code != 0 {
        anyhow::bail!(
            "smoke test command exited {} in {}: {}",
            result.exit_code,
            handle.sandbox_name,
            result.stderr.trim()
        );
    }
    ok(&format!(
        "smoke test VM booted in {:.2}s (incl. image pull) and ran: {}",
        boot.as_secs_f64(),
        result.stdout.trim()
    ));
    Ok(())
}

/// Fetch `temps-pty-agent` from the release matching this binary when no
/// flag, no sibling file and no earlier staging provides it. Announced, and a
/// failure only leaves the existing "not found" warning in place.
#[cfg(not(any(target_env = "musl", all(target_os = "macos", target_arch = "x86_64"))))]
async fn fetch_missing_pty_agent(
    dest: &std::path::Path,
    explicit: Option<&std::path::Path>,
) -> Option<ReleaseGuestAgents> {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join(PTY_AGENT_BINARY_NAME)));
    if !guest_agent_needs_fetch(explicit, beside.as_deref(), dest) {
        return None;
    }
    println!("  fetching temps-pty-agent from the release that matches this binary (checksum-verified)...");
    match fetch_guest_agents_from_release(&[PTY_AGENT_BINARY_NAME]).await {
        Ok(agents) if agents.path(PTY_AGENT_BINARY_NAME).is_some() => Some(agents),
        Ok(agents) => {
            warn(&format!(
                "release {} does not ship temps-pty-agent; upgrade to a release that does, \
                 or pass --pty-agent-bin <path>",
                agents.tag()
            ));
            None
        }
        Err(e) => {
            warn(&format!(
                "could not fetch temps-pty-agent from the release: {e}"
            ));
            None
        }
    }
}

/// Install the guest terminal agent at `dest` from `explicit` or, failing
/// that, a `temps-pty-agent` file next to the running executable (release
/// archives ship it there).
///
/// Optional by design: a missing binary is a warning that says how to fix
/// it, because sandboxes run fine without terminals. A file that is not a
/// Linux executable *is* an error — copying a macOS build into a Linux guest
/// would only fail later, at the first terminal.
#[cfg(not(any(target_env = "musl", all(target_os = "macos", target_arch = "x86_64"))))]
fn stage_pty_agent(
    dest: &std::path::Path,
    explicit: Option<&std::path::Path>,
    released: Option<PathBuf>,
) -> anyhow::Result<()> {
    let source = explicit
        .map(PathBuf::from)
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|d| d.join(PTY_AGENT_BINARY_NAME)))
                .filter(|p| p.exists())
        })
        .or(released);
    let Some(source) = source else {
        if dest.exists() {
            ok(&format!(
                "terminal agent already staged at {}",
                dest.display()
            ));
        } else {
            warn(
                "temps-pty-agent not found next to this executable: sandboxes run but \
                 `temps sandbox shell` is unavailable until you re-run with \
                 --pty-agent-bin <path-to-static-linux-build>",
            );
        }
        return Ok(());
    };
    stage_pty_agent_binary(&source, dest)?;
    ok(&format!(
        "terminal agent staged from {} (written into a sandbox on its first terminal)",
        source.display()
    ));
    Ok(())
}

/// Say whether terminals will work, and what to do if not. Informational:
/// the backend itself is ready without them.
fn report_terminal_support(terminal: &temps_agents::sandbox::TerminalCapability) {
    if terminal.supported {
        ok("terminals: guest terminal agent is staged (`temps sandbox shell` works)");
        return;
    }
    // `setup_command` is already the complete fix, flag included.
    println!(
        "  {} terminals unavailable: {}{}",
        "!".yellow().bold(),
        terminal.reason.as_deref().unwrap_or("unknown reason"),
        terminal
            .setup_command
            .as_deref()
            .map(|c| format!(" (run `{c}`)"))
            .unwrap_or_default()
    );
}

/// Runtime homes under the other places a Temps data directory commonly
/// lives that already hold an `msb`, excluding the one being set up.
fn other_runtime_homes(data_dir: &std::path::Path, home: &std::path::Path) -> Vec<PathBuf> {
    super::doctor::other_data_dirs_with(
        data_dir,
        home,
        &std::path::Path::new("microsandbox").join("bin").join("msb"),
    )
    .into_iter()
    .map(|dir| dir.join("microsandbox"))
    .collect()
}

fn ok(message: &str) {
    println!("  {} {}", "✓".green().bold(), message);
}

fn warn(message: &str) {
    println!("  {} {}", "!".yellow().bold(), message);
}

fn fail(message: &str) {
    println!("  {} {}", "✗".red().bold(), message);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install_msb(data_dir: &std::path::Path) {
        let bin = data_dir.join("microsandbox").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("msb"), b"").unwrap();
    }

    #[test]
    fn reports_a_runtime_installed_under_the_other_candidate_directory() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join(".temps");
        let nested = root.join("data");
        install_msb(&root);
        install_msb(&nested);

        // Setting up the nested directory must point at the stray root copy
        // and vice versa — never at the directory being set up.
        assert_eq!(
            other_runtime_homes(&nested, home.path()),
            vec![root.join("microsandbox")]
        );
        assert_eq!(
            other_runtime_homes(&root, home.path()),
            vec![nested.join("microsandbox")]
        );
    }

    #[test]
    fn single_runtime_home_produces_no_warning() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join(".temps");
        install_msb(&root);
        assert!(other_runtime_homes(&root, home.path()).is_empty());
        // An empty candidate directory (no msb) is not a runtime home.
        std::fs::create_dir_all(root.join("data").join("microsandbox")).unwrap();
        assert!(other_runtime_homes(&root, home.path()).is_empty());
    }
}
