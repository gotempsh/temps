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
//! 3. Smoke: boot a real microVM through the same provider the server uses,
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
#[cfg(not(target_env = "musl"))]
use {
    std::collections::HashMap,
    std::time::{Duration, Instant},
    temps_agents::sandbox::microsandbox::MicrosandboxSandboxProvider,
    temps_agents::sandbox::{SandboxBackend, SandboxCreateConfig, SandboxProvider},
};

#[cfg(not(target_env = "musl"))]
const SMOKE_LABEL: &str = "setup-smoke";
#[cfg(not(target_env = "musl"))]
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
    /// Data directory for storing configuration and runtime files
    #[arg(long, env = "TEMPS_DATA_DIR")]
    pub data_dir: Option<PathBuf>,

    /// Only report readiness; install nothing and exit non-zero if not ready
    #[arg(long)]
    pub check: bool,

    /// Skip the smoke-test VM boot after installing
    #[arg(long)]
    pub skip_smoke: bool,
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
        let data_dir = match &self.data_dir {
            Some(dir) => dir.clone(),
            None => temps_agents::sandbox::microsandbox::host_data_dir(),
        };
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
        println!("  Runtime home: {}", config.home().display());
        println!("  Runtime version: v{}", MICROSANDBOX_VERSION);
        println!();

        if self.check {
            let capability = microsandbox_capability(&data_dir);
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

    /// musl builds don't link the microsandbox SDK, so there is nothing to
    /// install; say why instead.
    #[cfg(target_env = "musl")]
    async fn install(self, config: MicrosandboxSandboxConfig) -> anyhow::Result<()> {
        let capability = microsandbox_capability(&config.data_dir);
        fail(&capability.reason.unwrap_or_default());
        anyhow::bail!("this temps build cannot run the microsandbox backend")
    }

    #[cfg(not(target_env = "musl"))]
    async fn install(self, config: MicrosandboxSandboxConfig) -> anyhow::Result<()> {
        let provider = MicrosandboxSandboxProvider::new(config)?;

        // Stage 1: a hypervisor is a hardware/OS property; installing the
        // runtime can't fix its absence, so stop before downloading.
        if let Err(reason) = provider.probe() {
            if !matches!(
                reason,
                temps_agents::sandbox::microsandbox::MicrosandboxUnavailable::RuntimeNotInstalled { .. }
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

        // Stage 3: boot a real VM through the server's provider.
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

#[cfg(not(target_env = "musl"))]
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

fn ok(message: &str) {
    println!("  {} {}", "✓".green().bold(), message);
}

#[cfg(not(target_env = "musl"))]
fn warn(message: &str) {
    println!("  {} {}", "!".yellow().bold(), message);
}

fn fail(message: &str) {
    println!("  {} {}", "✗".red().bold(), message);
}
