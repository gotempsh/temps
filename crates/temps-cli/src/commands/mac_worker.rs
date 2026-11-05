// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `temps join` on a Mac (ADR 090): find the Docker app, check its VM can
//! hold the worker's end of the mesh, and start the mesh sidecar there.
//! See `temps_node_agent::mac_worker` for what the sidecar does and why.

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use temps_node_agent::mac_worker::{
    self, classify_docker_app, DockerApp, DockerEndpoint, MacWorker, DEFAULT_EGRESS_PORT,
};

/// The Docker app a Mac joins with, found before anything else runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacJoinContext {
    pub endpoint: DockerEndpoint,
    pub app: DockerApp,
    pub colima_profile: Option<String>,
    pub vm_architecture: String,
}

/// Where Docker apps install their CLI, searched before `PATH`.
const DOCKER_CLI_CANDIDATES: [&str; 3] = [
    "/opt/homebrew/bin/docker",
    "/usr/local/bin/docker",
    "/Applications/Docker.app/Contents/Resources/bin/docker",
];

fn home() -> Option<PathBuf> {
    dirs::home_dir()
}

/// The `docker` CLI: a Docker app's usual location, then `PATH`.
fn find_docker_cli() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = DOCKER_CLI_CANDIDATES.iter().map(PathBuf::from).collect();
    if let Some(home) = home() {
        candidates.push(home.join(".orbstack/bin/docker"));
        candidates.push(home.join(".rd/bin/docker"));
    }
    #[allow(
        clippy::disallowed_methods,
        reason = "bootstrap: locating the docker CLI the user runs, once, at join"
    )]
    let path = std::env::var_os("PATH");
    if let Some(path) = path {
        candidates.extend(std::env::split_paths(&path).map(|dir| dir.join("docker")));
    }
    candidates.into_iter().find(|candidate| candidate.is_file())
}

/// How long a Docker CLI call at join may take: a Docker app whose socket
/// is still there but which no longer answers must fail with a hint, not hang.
const DETECT_TIMEOUT: Duration = Duration::from_secs(20);

fn run(cli: &Path, args: &[&str]) -> Option<String> {
    let mut command = std::process::Command::new(cli);
    command.args(args);
    let output = temps_wireguard::vm::output_within(command, None, DETECT_TIMEOUT).ok()??;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Find the Docker app this Mac uses: `DOCKER_HOST` if set, else the
/// current Docker context. Fails with what to do when Docker is missing or
/// not running.
pub fn detect() -> anyhow::Result<MacJoinContext> {
    let cli = find_docker_cli().ok_or_else(|| {
        anyhow::anyhow!(
            "No `docker` command found. A Mac worker runs its containers in a Docker app: \
             install one (Docker Desktop, OrbStack, colima or Rancher Desktop), start it, and \
             run `temps join` again."
        )
    })?;
    #[allow(
        clippy::disallowed_methods,
        reason = "bootstrap: the Docker endpoint the user chose for this shell, read once at \
                  join and persisted in mac-worker.json"
    )]
    let explicit = std::env::var("DOCKER_HOST")
        .ok()
        .filter(|host| !host.is_empty());
    let host = match explicit {
        Some(host) => host,
        None => run(
            &cli,
            &[
                "context",
                "inspect",
                "--format",
                "{{.Endpoints.docker.Host}}",
            ],
        )
        .filter(|host| !host.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Could not read the current Docker context with `{} context inspect`. Start \
                 your Docker app, check `docker ps` works, and run `temps join` again.",
                cli.display()
            )
        })?,
    };
    if !host.starts_with("unix://") {
        anyhow::bail!(
            "The Docker endpoint is {host}, not a local Docker app. A Mac worker needs the \
             Docker app on this Mac (its containers run in that app's VM): switch to it with \
             `docker context use <its context>` (see `docker context ls`)."
        );
    }
    let cli_text = cli.to_string_lossy().into_owned();
    let info = run(
        &cli,
        &[
            "--host",
            &host,
            "info",
            "--format",
            "{{.OperatingSystem}}|{{.Name}}|{{.Architecture}}",
        ],
    );
    let (os, name, arch) = match info
        .as_deref()
        .map(|info| info.split('|').collect::<Vec<_>>())
    {
        Some(fields) if fields.len() == 3 => (
            fields[0].to_string(),
            fields[1].to_string(),
            fields[2].to_string(),
        ),
        _ => {
            let (app, profile) = classify_docker_app(&host, "", "");
            anyhow::bail!(
                "{} is not running (no answer from {host}). Start it with `{}`, wait until \
                 `docker ps` works, and run `temps join` again.",
                app.display_name(),
                app.start_hint(profile.as_deref())
            );
        }
    };
    let (app, colima_profile) = classify_docker_app(&host, &os, &name);
    Ok(MacJoinContext {
        endpoint: DockerEndpoint {
            cli: cli_text,
            host,
        },
        app,
        colima_profile,
        vm_architecture: arch,
    })
}

/// Normalise `aarch64`/`arm64` and `x86_64`/`amd64`.
fn same_architecture(mac: &str, vm: &str) -> bool {
    fn norm(arch: &str) -> &str {
        match arch {
            "arm64" | "aarch64" => "arm64",
            "x86_64" | "amd64" => "amd64",
            other => other,
        }
    }
    norm(mac) == norm(vm)
}

/// Check the VM can hold the worker's end of the mesh, before the join
/// token is spent. Builds the sidecar image as a side effect.
pub fn preflight(context: &MacJoinContext) -> anyhow::Result<()> {
    let app = context.app.display_name();
    println!(
        "Docker app: {app}{} ({}), VM architecture {}.",
        context
            .colima_profile
            .as_deref()
            .map(|profile| format!(", profile {profile}"))
            .unwrap_or_default(),
        context.endpoint.host,
        context.vm_architecture
    );
    if !same_architecture(std::env::consts::ARCH, &context.vm_architecture) {
        println!(
            "Note: this Mac is {} but {app}'s VM is {}: workloads run as {} images (emulated \
             where the VM is).",
            std::env::consts::ARCH,
            context.vm_architecture,
            context.vm_architecture
        );
    }
    if context.app == DockerApp::DockerDesktop {
        println!(
            "Docker Desktop: if `temps join` cannot reach the control plane after the mesh is \
             up, turn on Settings > Resources > Network > \"Enable host networking\"."
        );
    }
    println!("Checking {app}'s VM (kernel WireGuard, nftables)...");
    let capabilities = mac_worker::probe_vm(&context.endpoint).map_err(|error| {
        anyhow::anyhow!(
            "Could not run a check container in {app}'s VM: {error}. If Docker Desktop's \
             Enhanced Container Isolation is on, it refuses host-network containers with \
             NET_ADMIN, which a Mac worker needs: turn it off for this machine."
        )
    })?;
    if !capabilities.kernel_wireguard {
        anyhow::bail!(
            "{app}'s VM kernel cannot create a WireGuard interface, which a Mac worker's mesh \
             needs. Update {app} (current Docker Desktop, OrbStack, colima and Rancher Desktop \
             VMs ship WireGuard), then run `temps join` again. Nothing was set up."
        );
    }
    if !capabilities.nftables {
        anyhow::bail!(
            "{app}'s VM kernel has no working nftables, which the mesh lockdown needs (it keeps \
             the tunnel from being a way into the VM). Update {app}, then run `temps join` \
             again. Nothing was set up."
        );
    }
    println!("{app}'s VM supports WireGuard and nftables.");
    check_loopback_forwarding(context)?;
    Ok(())
}

/// Check that the Docker app forwards a listener on its VM's loopback to
/// this Mac's loopback, which is how the agent reaches the control plane
/// (Docker Desktop only does it with host networking on). Checked before
/// the join token is spent.
fn check_loopback_forwarding(context: &MacJoinContext) -> anyhow::Result<()> {
    let app = context.app.display_name();
    println!("Checking that {app} forwards its VM's loopback to this Mac...");
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr())
        .map_err(|error| anyhow::anyhow!("could not pick a free port on this Mac: {error}"))?
        .port();
    mac_worker::start_forwarding_probe(&context.endpoint, port).map_err(|error| {
        anyhow::anyhow!("could not start the forwarding check in {app}'s VM: {error}")
    })?;
    let reached = probe_answers(port, Duration::from_secs(30));
    mac_worker::stop_forwarding_probe(&context.endpoint, port);
    if !reached {
        anyhow::bail!(
            "{app} does not forward its VM's loopback to this Mac (nothing answered on \
             127.0.0.1:{port}), which is how the agent reaches the control plane. {} Nothing \
             was set up and the join token was not used.",
            context.app.host_network_hint()
        );
    }
    Ok(())
}

/// Whether the forwarding probe answers on this Mac's `port` within `limit`.
fn probe_answers(port: u16, limit: Duration) -> bool {
    use std::io::Read;
    let deadline = std::time::Instant::now() + limit;
    let address = std::net::SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    while std::time::Instant::now() < deadline {
        if let Ok(mut stream) =
            std::net::TcpStream::connect_timeout(&address, Duration::from_secs(2))
        {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
            let mut reply = [0u8; 64];
            if let Ok(read) = stream.read(&mut reply) {
                if String::from_utf8_lossy(&reply[..read])
                    .contains(mac_worker::FORWARDING_PROBE_REPLY)
                {
                    return true;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

/// The agent port of `--agent-address` (127.0.0.1:3100 by default).
pub fn agent_port(agent_address: &str) -> anyhow::Result<u16> {
    let address: std::net::SocketAddr = agent_address.parse().map_err(|error| {
        anyhow::anyhow!("--agent-address {agent_address:?} is not ip:port: {error}")
    })?;
    // The sidecar reaches the Mac through the Docker app's IPv4 host
    // gateway, so the agent must listen on IPv4 loopback, not `::1`.
    if address.ip() != std::net::IpAddr::V4(Ipv4Addr::LOCALHOST) {
        anyhow::bail!(
            "On a Mac, the agent listens on this Mac's IPv4 loopback only (the mesh sidecar \
             forwards the control plane to it); pass --agent-address 127.0.0.1:<port>, not \
             {address}."
        );
    }
    Ok(address.port())
}

/// The sidecar a join started. Until [`JoinedSidecar::keep`], dropping it
/// (the join failed after the sidecar started) removes the sidecar and the
/// mesh interface when this join created them; a sidecar that was already
/// there belongs to a worker joined before, whose saved record
/// `temps agent service uninstall --remove-mesh` uses.
pub struct JoinedSidecar {
    pub worker: MacWorker,
    created: bool,
    kept: bool,
}

impl JoinedSidecar {
    /// The join succeeded: leave the sidecar running.
    pub fn keep(mut self) -> MacWorker {
        self.kept = true;
        self.worker.clone()
    }
}

impl Drop for JoinedSidecar {
    fn drop(&mut self) {
        if self.kept || !self.created {
            return;
        }
        let app = self.worker.docker_app.display_name();
        let endpoint = self.worker.endpoint();
        let removed = mac_worker::remove_sidecar(&endpoint)
            .and_then(|_| mac_worker::remove_mesh_interface(&endpoint));
        match removed {
            Ok(()) => println!(
                "The join failed: removed the mesh sidecar and interface it created in {app}'s VM."
            ),
            Err(error) => println!(
                "The join failed, and removing the mesh sidecar it created in {app}'s VM failed \
                 too ({error}): remove it with `docker rm -f {}`.",
                mac_worker::SIDECAR_CONTAINER
            ),
        }
    }
}

/// Start the mesh sidecar for this node and check it reaches the agent's
/// port on the Mac.
pub async fn start_sidecar(
    context: &MacJoinContext,
    mesh_address: Ipv4Addr,
    control_plane_address: Ipv4Addr,
    node_api_port: u16,
    agent_port: u16,
) -> anyhow::Result<JoinedSidecar> {
    let worker = MacWorker {
        docker_app: context.app,
        colima_profile: context.colima_profile.clone(),
        docker_cli: context.endpoint.cli.clone(),
        docker_host: context.endpoint.host.clone(),
        vm_architecture: context.vm_architecture.clone(),
        mesh_address,
        control_plane_address,
        node_api_port,
        agent_port,
        egress_port: DEFAULT_EGRESS_PORT,
    };
    let app = context.app.display_name();
    let started = worker.clone();
    let action = tokio::task::spawn_blocking(move || mac_worker::ensure_sidecar(&started))
        .await?
        .map_err(|error| {
            anyhow::anyhow!("could not start the mesh sidecar in {app}'s VM: {error}")
        })?;
    println!(
        "Mesh sidecar ({}) {} in {app}'s VM.",
        mac_worker::SIDECAR_CONTAINER,
        match action {
            mac_worker::SidecarAction::AlreadyRunning => "already running",
            mac_worker::SidecarAction::Started => "started",
            mac_worker::SidecarAction::Created => "created",
            mac_worker::SidecarAction::Replaced => "re-created",
        }
    );
    let sidecar = JoinedSidecar {
        worker,
        created: action == mac_worker::SidecarAction::Created,
        kept: false,
    };
    // The control plane's calls to the agent come in through the sidecar to
    // this Mac's loopback. Prove that path with a short-lived listener on the
    // agent port (an agent already running there answers just as well).
    let probe = std::net::TcpListener::bind(("127.0.0.1", agent_port)).ok();
    let endpoint = context.endpoint.clone();
    let reached =
        tokio::task::spawn_blocking(move || mac_worker::sidecar_reaches_mac(&endpoint, agent_port))
            .await??;
    drop(probe);
    if !reached {
        anyhow::bail!(
            "The mesh sidecar in {app}'s VM cannot reach this Mac's loopback on port \
             {agent_port} (through host-gateway), which is how the control plane reaches the \
             agent. Update {app}; nothing was registered."
        );
    }
    Ok(sidecar)
}

/// Wait until the Docker app forwards the sidecar's egress port to this
/// Mac's loopback (lima-based apps notice a new listener within seconds).
pub async fn wait_for_egress(worker: &MacWorker) -> anyhow::Result<()> {
    let target = ("127.0.0.1", worker.egress_port);
    for _ in 0..30 {
        if let Ok(Ok(_)) = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::net::TcpStream::connect(target),
        )
        .await
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    anyhow::bail!(
        "This Mac cannot reach the mesh sidecar's port 127.0.0.1:{} in the Docker app's VM, \
         which is how the agent reaches the control plane. {}",
        worker.egress_port,
        worker.docker_app.host_network_hint()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn architectures_compare_by_family() {
        assert!(same_architecture("aarch64", "aarch64"));
        assert!(same_architecture("aarch64", "arm64"));
        assert!(same_architecture("x86_64", "amd64"));
        assert!(!same_architecture("aarch64", "x86_64"));
    }

    #[test]
    fn the_agent_listens_on_loopback_only() {
        assert_eq!(agent_port("127.0.0.1:3100").unwrap(), 3100);
        let error = agent_port("0.0.0.0:3100").unwrap_err().to_string();
        assert!(error.contains("loopback"), "{error}");
        // The sidecar's path to the Mac is IPv4: `::1` would never be reached.
        assert!(agent_port("[::1]:3100").is_err());
        assert!(agent_port("127.0.0.2:3100").is_err());
        assert!(agent_port("nonsense").is_err());
    }
}
