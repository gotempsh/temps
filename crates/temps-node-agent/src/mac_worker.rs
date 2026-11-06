// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A Mac as a Temps worker (ADR 090).
//!
//! The agent runs natively on macOS. Containers run in the Linux VM of the
//! user's Docker app, and so must the worker's end of the WireGuard mesh: the
//! mesh address is where published ports bind and where the control plane
//! dials, and macOS has no route into the VM. One small container, the mesh
//! sidecar, holds that end in the VM's host network namespace with
//! `CAP_NET_ADMIN` and nothing else. It does three things:
//!
//! - holds `temps-wg0` (configured by the agent through `docker exec`, see
//!   `temps_wireguard::vm`) and the mesh lockdown (`temps_network::vm`);
//! - forwards the agent API from `<mesh address>:<agent port>` to the agent
//!   on the Mac's loopback (`host-gateway`), so the control plane reaches it
//!   over the mesh. TLS is end to end: the sidecar sees ciphertext only, and
//!   the agent requires the control plane's client certificate;
//! - forwards the agent's own calls to the control plane from the VM's
//!   loopback (`127.0.0.1:<egress port>`, which the Docker app forwards to
//!   the Mac's loopback) to the control plane's node API on the mesh.
//!   Containers cannot reach the VM's loopback, so workloads gain no path to
//!   the control plane.
//!
//! The agent also opens its runtime health probe connections from the
//! sidecar ([`SidecarProbeDialer`]): the published ports they check bind the
//! mesh address, in the VM.
//!
//! [`MacWorker`] is what `temps join` records about this in the agent's data
//! directory (`mac-worker.json`), and what `temps agent` reads to keep the
//! sidecar running.

use std::io::Write;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub use temps_wireguard::vm::{DockerEndpoint, SIDECAR_CONTAINER};

mod probe_dialer;
pub use probe_dialer::{probe_exec_args, SidecarProbeDialer};

/// File in the agent data directory holding [`MacWorker`].
pub const CONFIG_FILE: &str = "mac-worker.json";

/// Label every container this module creates carries, so it never touches
/// a container it did not create.
pub const ROLE_LABEL: &str = "sh.temps.mac-worker.role";
const SPEC_LABEL: &str = "sh.temps.mac-worker.spec";
const SIDECAR_ROLE: &str = "mesh-sidecar";

/// The name the sidecar resolves to the Mac (`host-gateway`).
pub const MAC_HOST_ALIAS: &str = "temps-mac-host";

/// Default VM-loopback port for the agent's calls to the control plane.
pub const DEFAULT_EGRESS_PORT: u16 = 3190;

/// Longest a Docker CLI call of this module may take.
const DOCKER_TIMEOUT: Duration = Duration::from_secs(120);

/// The sidecar image, built in the VM from this Dockerfile: Alpine pinned by
/// digest, and only the tools the sidecar runs.
pub const SIDECAR_DOCKERFILE: &str = "\
# Temps mesh sidecar for macOS workers (ADR 090)
FROM alpine:3.22@sha256:2e0b81d60597f449baa56dc25d355367869d2dca4cd57e866f1e2b52af3eb55f
RUN apk add --no-cache wireguard-tools-wg iproute2 nftables socat
";

/// What the sidecar runs: the two forwarders, each restarted if it exits
/// (the inbound one cannot bind until the mesh address exists).
const FORWARDERS: &str = r#"set -u
inbound() {
  while :; do
    socat TCP-LISTEN:"$AGENT_PORT",bind="$MESH_ADDRESS",fork,reuseaddr TCP:temps-mac-host:"$AGENT_PORT",connect-timeout=10
    sleep 2
  done
}
egress() {
  while :; do
    socat TCP-LISTEN:"$EGRESS_PORT",bind=127.0.0.1,fork,reuseaddr TCP:"$CONTROL_PLANE_ADDRESS":"$NODE_API_PORT",connect-timeout=10
    sleep 2
  done
}
inbound &
egress &
wait
"#;

/// The Docker app the worker's containers run in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DockerApp {
    DockerDesktop,
    OrbStack,
    Colima,
    RancherDesktop,
    Unknown,
}

impl DockerApp {
    pub fn display_name(self) -> &'static str {
        match self {
            DockerApp::DockerDesktop => "Docker Desktop",
            DockerApp::OrbStack => "OrbStack",
            DockerApp::Colima => "colima",
            DockerApp::RancherDesktop => "Rancher Desktop",
            DockerApp::Unknown => "your Docker app",
        }
    }

    /// How to start it, for an error that says exactly what to do.
    pub fn start_hint(self, colima_profile: Option<&str>) -> String {
        match self {
            DockerApp::DockerDesktop => "open -a Docker".to_string(),
            DockerApp::OrbStack => "open -a OrbStack".to_string(),
            DockerApp::Colima => match colima_profile {
                Some(profile) if profile != "default" => format!("colima start {profile}"),
                _ => "colima start".to_string(),
            },
            DockerApp::RancherDesktop => "open -a \"Rancher Desktop\"".to_string(),
            DockerApp::Unknown => "start your Docker app".to_string(),
        }
    }

    /// What it takes for the Mac to reach a listener on the VM's loopback
    /// in the host network (the agent's path to the control plane).
    pub fn host_network_hint(self) -> &'static str {
        match self {
            DockerApp::DockerDesktop => {
                "In Docker Desktop, turn on Settings > Resources > Network > \"Enable host \
                 networking\" (Docker Desktop 4.34 or later), apply, and run `temps join` again."
            }
            _ => {
                "Your Docker app did not forward a port of a host-network container to this \
                 Mac's loopback. Update it, or use colima, OrbStack or Docker Desktop with host \
                 networking enabled."
            }
        }
    }
}

/// Which Docker app owns the daemon at `host` (`unix:///…`), from the socket
/// path and, as a fallback, what `docker info` reports (`os`, `name`).
pub fn classify_docker_app(host: &str, os: &str, name: &str) -> (DockerApp, Option<String>) {
    let path = host.strip_prefix("unix://").unwrap_or(host);
    if let Some(rest) = path.split("/.colima/").nth(1) {
        let profile = rest.split('/').next().unwrap_or("default").to_string();
        return (DockerApp::Colima, Some(profile));
    }
    if path.contains("/.orbstack/") || os.contains("OrbStack") {
        return (DockerApp::OrbStack, None);
    }
    if path.contains("/.rd/") || name.contains("rancher-desktop") {
        return (DockerApp::RancherDesktop, None);
    }
    if path.contains("/.docker/run/") || os.contains("Docker Desktop") {
        return (DockerApp::DockerDesktop, None);
    }
    if let Some(profile) = name.strip_prefix("colima") {
        let profile = profile.trim_start_matches('-');
        let profile = if profile.is_empty() {
            "default"
        } else {
            profile
        };
        return (DockerApp::Colima, Some(profile.to_string()));
    }
    (DockerApp::Unknown, None)
}

/// What a Mac worker records at join, in `<data dir>/mac-worker.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MacWorker {
    pub docker_app: DockerApp,
    /// The colima profile, when the Docker app is colima.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colima_profile: Option<String>,
    /// Absolute path of the `docker` CLI.
    pub docker_cli: String,
    /// The daemon (`unix:///…/docker.sock`).
    pub docker_host: String,
    /// The VM's architecture as Docker reports it (`aarch64`, `x86_64`).
    pub vm_architecture: String,
    /// This node's mesh address: where published ports bind, in the VM.
    pub mesh_address: Ipv4Addr,
    /// The control plane's mesh address and node API port, which the egress
    /// forwarder dials.
    pub control_plane_address: Ipv4Addr,
    pub node_api_port: u16,
    /// The agent API port, on the Mac's loopback and on the mesh address.
    pub agent_port: u16,
    /// The VM-loopback port the agent calls the control plane through.
    pub egress_port: u16,
}

impl MacWorker {
    pub fn endpoint(&self) -> DockerEndpoint {
        DockerEndpoint {
            cli: self.docker_cli.clone(),
            host: self.docker_host.clone(),
        }
    }

    /// The URL the agent calls the control plane at: the egress forwarder.
    /// TLS still verifies the control plane by its reserved name.
    pub fn control_plane_url(&self) -> String {
        format!("https://127.0.0.1:{}", self.egress_port)
    }

    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join(CONFIG_FILE)
    }

    pub fn load(data_dir: &Path) -> Result<Option<Self>, MacWorkerError> {
        let path = Self::path(data_dir);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                serde_json::from_str(&text)
                    .map(Some)
                    .map_err(|error| MacWorkerError::Config {
                        path: path.clone(),
                        reason: error.to_string(),
                    })
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(MacWorkerError::Config {
                path,
                reason: error.to_string(),
            }),
        }
    }

    pub fn save(&self, data_dir: &Path) -> Result<(), MacWorkerError> {
        let path = Self::path(data_dir);
        let text = serde_json::to_string_pretty(self).map_err(|error| MacWorkerError::Config {
            path: path.clone(),
            reason: error.to_string(),
        })?;
        let temporary = path.with_extension("json.tmp");
        let write = || -> std::io::Result<()> {
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(&temporary)?;
                file.write_all(text.as_bytes())?;
                file.sync_all()?;
            }
            #[cfg(not(unix))]
            std::fs::write(&temporary, text.as_bytes())?;
            std::fs::rename(&temporary, &path)
        };
        write().map_err(|error| MacWorkerError::Config {
            path,
            reason: error.to_string(),
        })
    }

    /// The sidecar's image tag: content-addressed, so a changed Dockerfile
    /// is a new image and an unchanged one is never rebuilt.
    pub fn image_tag() -> String {
        let digest = temps_core::crypto::sha256_hex(SIDECAR_DOCKERFILE);
        format!("temps-mac-mesh:{}", &digest[..12])
    }

    /// The environment the forwarders read. Every value is a typed address
    /// or port.
    pub fn sidecar_env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("MESH_ADDRESS", self.mesh_address.to_string()),
            ("AGENT_PORT", self.agent_port.to_string()),
            (
                "CONTROL_PLANE_ADDRESS",
                self.control_plane_address.to_string(),
            ),
            ("NODE_API_PORT", self.node_api_port.to_string()),
            ("EGRESS_PORT", self.egress_port.to_string()),
        ]
    }

    /// A fingerprint of everything the sidecar is created with: a container
    /// carrying another one is replaced.
    pub fn sidecar_spec(&self) -> String {
        let env: Vec<String> = self
            .sidecar_env()
            .into_iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        let spec = format!("{}|{}|{}", Self::image_tag(), env.join(","), FORWARDERS);
        temps_core::crypto::sha256_hex(&spec)[..16].to_string()
    }

    /// `docker run` arguments for the sidecar (after `--host <host>`).
    pub fn sidecar_run_args(&self) -> Vec<String> {
        let mut args: Vec<String> = [
            "run",
            "--detach",
            "--name",
            SIDECAR_CONTAINER,
            "--restart",
            "unless-stopped",
            // The VM's network namespace: where temps-wg0, the published
            // ports and the bridges are.
            "--network",
            "host",
            // NET_ADMIN for wg/ip/nft, and nothing else: not privileged, no
            // other capability, no device, no host path, no Docker socket.
            "--cap-drop",
            "ALL",
            "--cap-add",
            "NET_ADMIN",
            "--security-opt",
            "no-new-privileges",
            "--read-only",
            "--tmpfs",
            "/tmp:rw,noexec,nosuid,size=1m",
            "--memory",
            "64m",
            "--pids-limit",
            "128",
            "--log-opt",
            "max-size=1m",
            "--log-opt",
            "max-file=2",
        ]
        .iter()
        .map(|arg| arg.to_string())
        .collect();
        args.push("--add-host".into());
        args.push(format!("{MAC_HOST_ALIAS}:host-gateway"));
        args.push("--label".into());
        args.push(format!("{ROLE_LABEL}={SIDECAR_ROLE}"));
        args.push("--label".into());
        args.push(format!("{SPEC_LABEL}={}", self.sidecar_spec()));
        for (key, value) in self.sidecar_env() {
            args.push("--env".into());
            args.push(format!("{key}={value}"));
        }
        args.push(Self::image_tag());
        args.push("sh".into());
        args.push("-c".into());
        args.push(FORWARDERS.to_string());
        args
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MacWorkerError {
    #[error("Mac worker configuration {path}: {reason}")]
    Config { path: PathBuf, reason: String },
    #[error("`docker {operation}` against {host} failed: {reason}")]
    Docker {
        operation: String,
        host: String,
        reason: String,
    },
    #[error(
        "a container named {SIDECAR_CONTAINER} exists in the Docker app's VM but was not created \
         by Temps (it has no {ROLE_LABEL} label); rename or remove it, then run this again"
    )]
    ForeignContainer,
}

static RUNNING: std::sync::OnceLock<(DockerApp, DockerEndpoint)> = std::sync::OnceLock::new();

/// Record that this agent runs as a Mac worker, for its heartbeat and its
/// runtime health probes.
pub fn set_running(worker: &MacWorker) {
    let _ = RUNNING.set((worker.docker_app, worker.endpoint()));
}

/// How this agent's runtime health probes reach published ports: from the
/// mesh sidecar on a Mac worker (macOS has no route into the Docker app's
/// VM, where they bind), directly anywhere else.
pub fn probe_dialer() -> std::sync::Arc<dyn temps_runtime_health::ProbeDialer> {
    match RUNNING.get() {
        Some((_, endpoint)) => std::sync::Arc::new(SidecarProbeDialer::new(endpoint.clone())),
        None => std::sync::Arc::new(temps_runtime_health::TcpDialer),
    }
}

/// What the heartbeat reports under `AGENT_HOST_KEY`: `None` unless this
/// agent runs as a Mac worker.
pub fn heartbeat_host() -> Option<serde_json::Value> {
    let (app, _) = RUNNING.get()?;
    Some(serde_json::json!({
        "os": temps_core::node_address::HOST_OS_MACOS,
        "docker_app": app,
    }))
}

/// Run the Docker CLI against `endpoint`, with `stdin` when given.
pub fn docker(
    endpoint: &DockerEndpoint,
    args: &[String],
    stdin: Option<&[u8]>,
) -> Result<std::process::Output, MacWorkerError> {
    let operation = args.first().cloned().unwrap_or_default();
    let failed = |reason: String| MacWorkerError::Docker {
        operation: operation.clone(),
        host: endpoint.host.clone(),
        reason,
    };
    let mut command = Command::new(&endpoint.cli);
    command.arg("--host").arg(&endpoint.host).args(args);
    temps_wireguard::vm::output_within(command, stdin, DOCKER_TIMEOUT)
        .map_err(|error| failed(format!("cannot run {}: {error}", endpoint.cli)))?
        .ok_or_else(|| failed(format!("no answer within {}s", DOCKER_TIMEOUT.as_secs())))
}

fn docker_ok(
    endpoint: &DockerEndpoint,
    args: &[String],
    stdin: Option<&[u8]>,
) -> Result<String, MacWorkerError> {
    let output = docker(endpoint, args, stdin)?;
    if !output.status.success() {
        return Err(MacWorkerError::Docker {
            operation: args.first().cloned().unwrap_or_default(),
            host: endpoint.host.clone(),
            reason: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| arg.to_string()).collect()
}

/// Build the sidecar image in the VM unless it is already there.
pub fn ensure_image(endpoint: &DockerEndpoint) -> Result<String, MacWorkerError> {
    let tag = MacWorker::image_tag();
    let present = docker(
        endpoint,
        &strings(&["image", "inspect", "--format", "{{.Id}}", &tag]),
        None,
    )?;
    if present.status.success() {
        return Ok(tag);
    }
    tracing::info!(image = %tag, "building the mesh sidecar image in the Docker app's VM");
    docker_ok(
        endpoint,
        &strings(&["build", "--quiet", "--tag", &tag, "-"]),
        Some(SIDECAR_DOCKERFILE.as_bytes()),
    )?;
    Ok(tag)
}

/// What [`ensure_sidecar`] found and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarAction {
    AlreadyRunning,
    Started,
    Created,
    Replaced,
}

/// Make the sidecar exist, run, and match `worker`. A container of the same
/// name that Temps did not create is never touched.
pub fn ensure_sidecar(worker: &MacWorker) -> Result<SidecarAction, MacWorkerError> {
    let endpoint = worker.endpoint();
    ensure_image(&endpoint)?;
    let format = format!(
        "{{{{index .Config.Labels \"{ROLE_LABEL}\"}}}}|{{{{index .Config.Labels \"{SPEC_LABEL}\"}}}}|{{{{.State.Running}}}}"
    );
    let inspected = docker(
        &endpoint,
        &strings(&[
            "container",
            "inspect",
            "--format",
            &format,
            SIDECAR_CONTAINER,
        ]),
        None,
    )?;
    let mut replaced = false;
    if inspected.status.success() {
        let text = String::from_utf8_lossy(&inspected.stdout);
        let mut fields = text.trim().split('|');
        let (role, spec, running) = (
            fields.next().unwrap_or_default(),
            fields.next().unwrap_or_default(),
            fields.next().unwrap_or_default(),
        );
        if role != SIDECAR_ROLE {
            return Err(MacWorkerError::ForeignContainer);
        }
        if spec == worker.sidecar_spec() {
            if running == "true" {
                return Ok(SidecarAction::AlreadyRunning);
            }
            docker_ok(&endpoint, &strings(&["start", SIDECAR_CONTAINER]), None)?;
            return Ok(SidecarAction::Started);
        }
        docker_ok(
            &endpoint,
            &strings(&["rm", "--force", SIDECAR_CONTAINER]),
            None,
        )?;
        replaced = true;
    }
    docker_ok(&endpoint, &worker.sidecar_run_args(), None)?;
    Ok(if replaced {
        SidecarAction::Replaced
    } else {
        SidecarAction::Created
    })
}

/// Remove the sidecar, if it is ours. The interface it held stays in the
/// VM's kernel until [`remove_mesh_interface`] or a VM restart.
pub fn remove_sidecar(endpoint: &DockerEndpoint) -> Result<bool, MacWorkerError> {
    let format = format!("{{{{index .Config.Labels \"{ROLE_LABEL}\"}}}}");
    let inspected = docker(
        endpoint,
        &strings(&[
            "container",
            "inspect",
            "--format",
            &format,
            SIDECAR_CONTAINER,
        ]),
        None,
    )?;
    if !inspected.status.success() {
        return Ok(false);
    }
    if String::from_utf8_lossy(&inspected.stdout).trim() != SIDECAR_ROLE {
        return Err(MacWorkerError::ForeignContainer);
    }
    docker_ok(
        endpoint,
        &strings(&["rm", "--force", SIDECAR_CONTAINER]),
        None,
    )?;
    Ok(true)
}

/// Delete the mesh interface and lockdown table from the VM's kernel, using
/// a one-off container of the sidecar image (the sidecar may be gone).
pub fn remove_mesh_interface(endpoint: &DockerEndpoint) -> Result<(), MacWorkerError> {
    let tag = ensure_image(endpoint)?;
    let script = format!(
        "ip link del dev {iface} 2>/dev/null; nft delete table inet {table} 2>/dev/null; true",
        iface = temps_wireguard::mesh::MESH_INTERFACE,
        table = "temps_mesh",
    );
    docker_ok(
        endpoint,
        &strings(&[
            "run",
            "--rm",
            "--network",
            "host",
            "--cap-drop",
            "ALL",
            "--cap-add",
            "NET_ADMIN",
            "--label",
            &format!("{ROLE_LABEL}=cleanup"),
            &tag,
            "sh",
            "-c",
            &script,
        ]),
        None,
    )?;
    Ok(())
}

/// What the preflight learns about the VM, or the first thing that stops a
/// Mac worker, with what to do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmCapabilities {
    pub kernel_wireguard: bool,
    pub nftables: bool,
}

/// Probe the VM's kernel with a throwaway container of the sidecar image:
/// can it create a WireGuard link, and does nftables work.
pub fn probe_vm(endpoint: &DockerEndpoint) -> Result<VmCapabilities, MacWorkerError> {
    let tag = ensure_image(endpoint)?;
    let script = "if ip link add dev temps-wgprobe type wireguard 2>/dev/null; then \
                  ip link del dev temps-wgprobe; echo wg=yes; else echo wg=no; fi; \
                  if nft list tables >/dev/null 2>&1; then echo nft=yes; else echo nft=no; fi";
    let out = docker_ok(
        endpoint,
        &strings(&[
            "run",
            "--rm",
            "--network",
            "host",
            "--cap-drop",
            "ALL",
            "--cap-add",
            "NET_ADMIN",
            "--label",
            &format!("{ROLE_LABEL}=probe"),
            &tag,
            "sh",
            "-c",
            script,
        ]),
        None,
    )?;
    Ok(VmCapabilities {
        kernel_wireguard: out.contains("wg=yes"),
        nftables: out.contains("nft=yes"),
    })
}

/// What the forwarding probe answers, so the join knows it reached the probe
/// and not something else listening on the Mac.
pub const FORWARDING_PROBE_REPLY: &str = "temps-forwarding-probe";

/// The forwarding probe's container name for `port` in one join attempt
/// (`attempt` tells concurrent or leftover attempts on the same port apart),
/// so an attempt only ever removes its own probe.
pub fn forwarding_probe_name(port: u16, attempt: &str) -> String {
    format!("temps-mac-probe-{port}-{attempt}")
}

/// `docker run` arguments for the forwarding probe: an unprivileged
/// host-network container answering on the VM's loopback at `port`, which is
/// where the sidecar's egress forwarder will listen.
pub fn forwarding_probe_args(tag: &str, name: &str, port: u16) -> Vec<String> {
    let listen = format!("TCP-LISTEN:{port},bind=127.0.0.1,fork,reuseaddr");
    let reply = format!("SYSTEM:echo {FORWARDING_PROBE_REPLY}");
    strings(&[
        "run",
        "--detach",
        "--rm",
        "--name",
        name,
        "--network",
        "host",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--read-only",
        "--label",
        &format!("{ROLE_LABEL}=probe"),
        tag,
        "socat",
        &listen,
        &reply,
    ])
}

/// Start the forwarding probe named `name` (see [`forwarding_probe_name`]);
/// stop it with [`stop_forwarding_probe`].
pub fn start_forwarding_probe(
    endpoint: &DockerEndpoint,
    name: &str,
    port: u16,
) -> Result<(), MacWorkerError> {
    let tag = ensure_image(endpoint)?;
    docker_ok(endpoint, &forwarding_probe_args(&tag, name, port), None).map(|_| ())
}

/// Remove the forwarding probe named `name`, if it is still there.
pub fn stop_forwarding_probe(endpoint: &DockerEndpoint, name: &str) {
    let _ = docker(endpoint, &strings(&["rm", "--force", name]), None);
}

/// Whether the sidecar reaches `port` on the Mac's loopback through
/// `host-gateway` (the path the control plane's calls to the agent take).
pub fn sidecar_reaches_mac(endpoint: &DockerEndpoint, port: u16) -> Result<bool, MacWorkerError> {
    let target = format!("TCP:{MAC_HOST_ALIAS}:{port},connect-timeout=5");
    let output = docker(
        endpoint,
        &strings(&[
            "exec",
            SIDECAR_CONTAINER,
            "socat",
            "-u",
            "OPEN:/dev/null",
            &target,
        ]),
        None,
    )?;
    Ok(output.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_forwarding_probe_is_unprivileged_and_answers_on_vm_loopback_only() {
        let name = forwarding_probe_name(41234, "7-1");
        let args = forwarding_probe_args("temps-mac-mesh:abc", &name, 41234);
        let text = args.join(" ");
        assert!(text.contains("--network host"), "{text}");
        assert!(text.contains("--cap-drop ALL"), "{text}");
        assert!(!text.contains("--cap-add"), "{text}");
        assert!(!text.contains("--privileged"), "{text}");
        assert!(text.contains("--rm"), "{text}");
        assert!(text.contains("TCP-LISTEN:41234,bind=127.0.0.1,"), "{text}");
        assert!(text.contains(FORWARDING_PROBE_REPLY), "{text}");
        assert_eq!(name, "temps-mac-probe-41234-7-1");
        assert!(
            args.windows(2).any(|w| w[0] == "--name" && w[1] == name),
            "{text}"
        );
        assert_ne!(
            name,
            forwarding_probe_name(41234, "7-2"),
            "attempts get their own probe"
        );
    }

    fn worker() -> MacWorker {
        MacWorker {
            docker_app: DockerApp::Colima,
            colima_profile: Some("default".into()),
            docker_cli: "/opt/homebrew/bin/docker".into(),
            docker_host: "unix:///Users/someone/.colima/default/docker.sock".into(),
            vm_architecture: "aarch64".into(),
            mesh_address: "10.210.0.4".parse().unwrap(),
            control_plane_address: "10.210.0.1".parse().unwrap(),
            node_api_port: 3443,
            agent_port: 3100,
            egress_port: DEFAULT_EGRESS_PORT,
        }
    }

    #[test]
    fn classifies_docker_apps_by_socket_then_by_daemon() {
        assert_eq!(
            classify_docker_app(
                "unix:///Users/a/.colima/work/docker.sock",
                "Ubuntu 24.04",
                "colima-work"
            ),
            (DockerApp::Colima, Some("work".to_string()))
        );
        assert_eq!(
            classify_docker_app("unix:///Users/a/.orbstack/run/docker.sock", "", ""),
            (DockerApp::OrbStack, None)
        );
        assert_eq!(
            classify_docker_app("unix:///Users/a/.docker/run/docker.sock", "", ""),
            (DockerApp::DockerDesktop, None)
        );
        assert_eq!(
            classify_docker_app(
                "unix:///var/run/docker.sock",
                "Docker Desktop",
                "docker-desktop"
            ),
            (DockerApp::DockerDesktop, None)
        );
        assert_eq!(
            classify_docker_app(
                "unix:///Users/a/.rd/docker.sock",
                "Alpine Linux",
                "lima-rancher-desktop"
            ),
            (DockerApp::RancherDesktop, None)
        );
        assert_eq!(
            classify_docker_app("unix:///var/run/docker.sock", "Ubuntu 24.04", "colima"),
            (DockerApp::Colima, Some("default".to_string()))
        );
        assert_eq!(
            classify_docker_app("tcp://10.0.0.5:2376", "Ubuntu", "builder"),
            (DockerApp::Unknown, None)
        );
    }

    #[test]
    fn start_hints_name_the_command() {
        assert_eq!(
            DockerApp::Colima.start_hint(Some("work")),
            "colima start work"
        );
        assert_eq!(
            DockerApp::Colima.start_hint(Some("default")),
            "colima start"
        );
        assert_eq!(DockerApp::OrbStack.start_hint(None), "open -a OrbStack");
        assert!(DockerApp::DockerDesktop
            .host_network_hint()
            .contains("Enable host networking"));
    }

    #[test]
    fn the_agent_calls_the_control_plane_through_the_egress_forwarder() {
        assert_eq!(worker().control_plane_url(), "https://127.0.0.1:3190");
    }

    #[test]
    fn the_sidecar_holds_net_admin_only_and_no_host_paths() {
        let args = worker().sidecar_run_args();
        let joined = args.join(" ");
        assert!(joined.contains("--cap-drop ALL --cap-add NET_ADMIN"));
        assert!(!joined.contains("--privileged"));
        assert!(!args
            .iter()
            .any(|arg| arg == "--volume" || arg == "-v" || arg == "--mount"));
        assert!(!joined.contains("docker.sock"));
        assert!(joined.contains("--network host"));
        assert!(joined.contains("temps-mac-host:host-gateway"));
        assert!(joined.contains("--read-only"));
    }

    #[test]
    fn forwarders_bind_only_the_mesh_address_and_the_vm_loopback() {
        assert!(FORWARDERS.contains("bind=\"$MESH_ADDRESS\""));
        assert!(FORWARDERS.contains("bind=127.0.0.1"));
        assert!(!FORWARDERS.contains("0.0.0.0"));
    }

    #[test]
    fn the_spec_changes_with_any_setting() {
        let base = worker();
        let mut moved = base.clone();
        moved.mesh_address = "10.210.0.5".parse().unwrap();
        assert_ne!(base.sidecar_spec(), moved.sidecar_spec());
        assert_eq!(base.sidecar_spec(), worker().sidecar_spec());
    }

    #[test]
    fn the_image_tag_is_content_addressed() {
        let tag = MacWorker::image_tag();
        assert!(tag.starts_with("temps-mac-mesh:"));
        assert_eq!(tag.len(), "temps-mac-mesh:".len() + 12);
    }

    #[test]
    fn the_config_round_trips_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(MacWorker::load(dir.path()).unwrap(), None);
        worker().save(dir.path()).unwrap();
        assert_eq!(MacWorker::load(dir.path()).unwrap(), Some(worker()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(MacWorker::path(dir.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_corrupt_config_names_its_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(MacWorker::path(dir.path()), "{").unwrap();
        let error = MacWorker::load(dir.path()).unwrap_err().to_string();
        assert!(error.contains(CONFIG_FILE), "{error}");
    }
}
