// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What `temps join` and `temps agent service install` check on the host
//! before they change anything (ADR 059).
//!
//! A host that was a worker of another cluster keeps that cluster's overlay
//! network, its agent config and whatever holds ports 80/443. `temps join`
//! used to find out only after it had spent the enrollment token; now it
//! looks first. Gathering ([`gather`]) is separate from deciding
//! ([`assess`]) so the decisions are unit-tested without a host.

use std::net::SocketAddr;

use temps_network::overlay_name::{self, DEFAULT_OVERLAY_NETWORK_NAME};

/// A Docker network on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostNetwork {
    pub name: String,
    pub subnets: Vec<String>,
}

/// A listening TCP socket on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listener {
    pub address: SocketAddr,
    /// The process holding it, when it could be found.
    pub process: Option<String>,
}

/// The agent config a previous `temps join` left on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedJoin {
    /// Where the agent reaches the control plane.
    pub control_plane_url: String,
    /// The address that join was given, when it recorded it.
    pub join_url: Option<String>,
    /// The overlay network name that join asked for.
    pub overlay_network_name: Option<String>,
    /// The subnet of the overlay the agent last applied (its network
    /// snapshot), which marks a network of that subnet as this node's own.
    pub applied_subnet: Option<String>,
    /// The overlay network name the control plane last sent the agent (its
    /// network snapshot). Stands in for `overlay_network_name` when the
    /// saved config lost it.
    pub applied_overlay_network_name: Option<String>,
    /// SHA-256 fingerprint of the cluster CA the agent pins, when its config
    /// names one that can be read.
    pub ca_fingerprint: Option<String>,
    /// The control plane's WireGuard mesh key in the agent's last network
    /// snapshot.
    pub control_plane_wg_public_key: Option<String>,
}

/// What identifies a control plane (ADR 060): the cluster CA every worker
/// pins and its WireGuard mesh key. Its URL does not: one is reachable under
/// several, and every cluster's mesh uses the same control-plane address.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct PlaneIdentity {
    pub ca_fingerprint: Option<String>,
    pub control_plane_wg_public_key: Option<String>,
}

/// Whether the control plane being joined is the one the saved agent config
/// belongs to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PlaneMatch {
    /// Its cluster CA, or else its mesh key, is the saved one.
    Same,
    /// It is another control plane.
    Different,
    /// Nothing could be compared, or nothing was saved.
    #[default]
    Unknown,
}

/// What comparing the saved control plane with the target found.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PlaneVerdict {
    Same,
    /// Which identity differs, and how.
    Different(String),
    /// Why it could not be told.
    Unknown(String),
}

fn compare_planes(
    saved: &SavedJoin,
    target: Option<&Result<PlaneIdentity, String>>,
) -> PlaneVerdict {
    let identity = match target {
        Some(Ok(identity)) => identity,
        Some(Err(reason)) => return PlaneVerdict::Unknown(reason.clone()),
        None => {
            return PlaneVerdict::Unknown("the control plane was not asked who it is".to_string())
        }
    };
    if let (Some(saved_ca), Some(target_ca)) = (&saved.ca_fingerprint, &identity.ca_fingerprint) {
        return if saved_ca.trim().eq_ignore_ascii_case(target_ca.trim()) {
            PlaneVerdict::Same
        } else {
            PlaneVerdict::Different(format!(
                "its cluster CA is {target_ca}; this machine's agent pins {saved_ca}"
            ))
        };
    }
    // A pinned CA is the identity: a target that does not announce one is
    // not told apart by its mesh key, which any control plane can repeat.
    if let (None, Some(saved_key), Some(target_key)) = (
        &saved.ca_fingerprint,
        &saved.control_plane_wg_public_key,
        &identity.control_plane_wg_public_key,
    ) {
        return if saved_key.trim() == target_key.trim() {
            PlaneVerdict::Same
        } else {
            PlaneVerdict::Different(format!(
                "its WireGuard mesh key is {target_key}; this machine's mesh peers with \
                 {saved_key}"
            ))
        };
    }
    let saved_has = match (&saved.ca_fingerprint, &saved.control_plane_wg_public_key) {
        (None, None) => "records neither a cluster CA nor a control-plane mesh key",
        (Some(_), None) => "records only a cluster CA",
        (None, Some(_)) => "records only a control-plane mesh key",
        (Some(_), Some(_)) => "records a cluster CA and a control-plane mesh key",
    };
    let target_has = match (
        &identity.ca_fingerprint,
        &identity.control_plane_wg_public_key,
    ) {
        (None, None) => "announced neither",
        (Some(_), None) => "announced only its cluster CA",
        (None, Some(_)) => "announced only its mesh key",
        (Some(_), Some(_)) => "announced both",
    };
    PlaneVerdict::Unknown(format!(
        "there is nothing to compare: this machine's agent config {saved_has}, and the control \
         plane {target_has}"
    ))
}

/// What the checks look at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostFacts {
    pub linux: bool,
    /// Root, or `CAP_NET_ADMIN` in the effective set.
    pub network_admin: bool,
    pub saved: Option<SavedJoin>,
    /// Who the control plane being joined says it is, or why that is not
    /// known. `None` when it was not asked.
    pub target_identity: Option<Result<PlaneIdentity, String>>,
    pub networks: Vec<HostNetwork>,
    /// Network interface names (`/sys/class/net`).
    pub interfaces: Vec<String>,
    pub listeners: Vec<Listener>,
}

/// One thing a check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    /// Joining changes the host network, which this process may not do.
    NeedsRoot,
    /// The host is already joined to another control plane; `evidence`
    /// says which identity differs (ADR 060).
    OtherControlPlane {
        saved: String,
        target: String,
        evidence: String,
    },
    /// The host is joined to a control plane at another address, and whether
    /// it is the same control plane could not be told (ADR 060).
    ControlPlaneUnverified {
        saved: String,
        target: String,
        reason: String,
    },
    /// The overlay name is taken by a network this node does not own; the
    /// node uses `chosen` instead.
    OverlayNameTaken {
        name: String,
        subnets: Vec<String>,
        chosen: Option<String>,
    },
    /// A port the worker serves on is already held. `serves_public` is
    /// whether the worker's ingress mode listens for public traffic; with
    /// `off` only the overlay's internal proxy needs the port.
    PortInUse {
        port: u16,
        address: SocketAddr,
        process: Option<String>,
        serves_public: bool,
    },
}

impl Finding {
    /// Whether `temps join` stops on it. `--force` overrides only a saved
    /// join for another control plane.
    pub fn blocks_join(&self, force: bool) -> bool {
        match self {
            Self::NeedsRoot => true,
            Self::OtherControlPlane { .. } | Self::ControlPlaneUnverified { .. } => !force,
            // Only `temps join` decides names, and it always finds one.
            Self::OverlayNameTaken { .. } => false,
            Self::PortInUse { .. } => false,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::NeedsRoot => "joining sets up this machine's network (WireGuard, the overlay \
                 bridge and VXLAN device), which needs root or CAP_NET_ADMIN. Run it with sudo: \
                 `sudo temps join ...`. The join token was not used."
                .to_string(),
            Self::OtherControlPlane {
                saved,
                target,
                evidence,
            } => format!(
                "this machine is already a worker of another control plane (joined at {saved}; \
                 its agent config is in {}): {target} is not that one, {evidence}. Joining it \
                 replaces that config and the old cluster loses this worker. Pass --force to \
                 do it anyway. The join token was not used.",
                crate::commands::agent::agent_data_dir()
                    .join("agent.json")
                    .display()
            ),
            Self::ControlPlaneUnverified {
                saved,
                target,
                reason,
            } => format!(
                "this machine is already a worker of the control plane it joined at {saved} \
                 (its agent config is in {}), and it could not be determined whether {target} \
                 is that same control plane: {reason}. If it is, pass --force. If it is not, \
                 --force moves this worker there and the old cluster loses it. The join token \
                 was not used.",
                crate::commands::agent::agent_data_dir()
                    .join("agent.json")
                    .display()
            ),
            Self::OverlayNameTaken {
                name,
                subnets,
                chosen,
            } => {
                let subnets = if subnets.is_empty() {
                    "no subnet".to_string()
                } else {
                    subnets.join(", ")
                };
                match chosen {
                    Some(chosen) => format!(
                        "this machine already has a network named '{name}' ({subnets}) that is \
                         not this node's, often left by another cluster it belonged to. Temps \
                         leaves it alone and this node's overlay uses '{chosen}' instead."
                    ),
                    None => format!(
                        "this machine already has a network named '{name}' ({subnets}) that is \
                         not this node's overlay, so the agent cannot bring its overlay up. \
                         Re-run `sudo temps join`, which picks a free name, or remove '{name}' \
                         if nothing uses it any more (`docker network rm {name}`)."
                    ),
                }
            }
            Self::PortInUse {
                port,
                address,
                process,
                serves_public,
            } => {
                let holder = process
                    .as_deref()
                    .map(|process| format!(" by {process}"))
                    .unwrap_or_default();
                if *serves_public {
                    format!(
                        "port {port} is already in use on {address}{holder}. The worker serves \
                         public traffic on 80/443 and the overlay's internal proxy on port 80 \
                         of its bridge address; while it is held they fail to start. Stop what \
                         holds it, or use `temps agent ingress` to move public traffic to other \
                         ports."
                    )
                } else if *port == 80 {
                    format!(
                        "port {port} is already in use on {address}{holder}. The worker takes \
                         no public traffic, but the overlay's internal proxy serves port 80 of \
                         its bridge address and fails to start while it is held. Stop what \
                         holds it."
                    )
                } else {
                    format!(
                        "port {port} is already in use on {address}{holder}. The worker's \
                         private listener (ingress `private`) serves HTTPS on port {port} of \
                         its private address and fails to start while it is held. Stop what \
                         holds it."
                    )
                }
            }
        }
    }
}

/// What the checks decided.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Assessment {
    pub findings: Vec<Finding>,
    /// The overlay network name to register with, when not the cluster's.
    pub overlay_network_name: Option<String>,
    /// Whether the target is the control plane the saved join belongs to.
    pub plane: PlaneMatch,
}

impl Assessment {
    /// The first finding that stops `temps join`.
    pub fn blocking(&self, force: bool) -> Option<&Finding> {
        self.findings
            .iter()
            .find(|finding| finding.blocks_join(force))
    }

    /// Print every finding as a warning.
    pub fn print_warnings(&self) {
        for finding in &self.findings {
            eprintln!("Warning: {}", finding.message());
        }
    }
}

fn same_url(a: &str, b: &str) -> bool {
    a.trim().trim_end_matches('/') == b.trim().trim_end_matches('/')
}

/// Which command is checking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose<'a> {
    /// `temps join`, with the control plane address it was given (`None` for
    /// a pairing code, which carries none).
    Join { target: Option<&'a str> },
    /// `temps agent service install`, for the saved join. It changes no name
    /// and stops on nothing; every finding is a warning.
    ServiceInstall,
}

/// The ports a worker with `ingress` serves on, sorted, and whether its
/// ingress mode listens for public traffic. Port 80 always: the overlay's
/// internal proxy binds it on the bridge address. `off` binds no public
/// listener, so 443 and the ingress ports cannot stop it. `private` binds
/// only its fixed private ports (ADR 064 D5), and takes no public traffic.
pub fn served_ports(ingress: &temps_core::worker_ingress::LocalIngress) -> (Vec<u16>, bool) {
    use temps_core::worker_ingress::{IngressMode, PRIVATE_HTTPS_PORT, PRIVATE_HTTP_PORT};
    let (mut ports, serves_public) = match ingress.mode {
        IngressMode::Off => (vec![80], false),
        IngressMode::Private => (vec![80, PRIVATE_HTTP_PORT, PRIVATE_HTTPS_PORT], false),
        IngressMode::Fixed | IngressMode::Delegated => {
            (vec![80, 443, ingress.http_port, ingress.https_port], true)
        }
    };
    ports.sort_unstable();
    ports.dedup();
    (ports, serves_public)
}

/// Decide from `facts`. `ports` are the ports the worker will serve on;
/// `serves_public` whether its ingress mode takes public traffic on them.
pub fn assess(
    facts: &HostFacts,
    purpose: Purpose<'_>,
    ports: &[u16],
    serves_public: bool,
) -> Assessment {
    let mut assessment = Assessment::default();
    if facts.linux && !facts.network_admin {
        assessment.findings.push(Finding::NeedsRoot);
    }

    let target = match purpose {
        Purpose::Join { target } => target,
        Purpose::ServiceInstall => None,
    };
    let same_cluster = match (&facts.saved, target) {
        (Some(_), _) if purpose == Purpose::ServiceInstall => true,
        (Some(saved), Some(target)) => {
            let saved_at = saved
                .join_url
                .clone()
                .unwrap_or_else(|| saved.control_plane_url.clone());
            // The identity decides when it can be compared (ADR 060); the
            // address only when it cannot, and then a differing address
            // says "unknown", not "another cluster".
            match compare_planes(saved, facts.target_identity.as_ref()) {
                PlaneVerdict::Same => {
                    assessment.plane = PlaneMatch::Same;
                    true
                }
                PlaneVerdict::Different(evidence) => {
                    assessment.plane = PlaneMatch::Different;
                    assessment.findings.push(Finding::OtherControlPlane {
                        saved: saved_at,
                        target: target.to_string(),
                        evidence,
                    });
                    false
                }
                PlaneVerdict::Unknown(reason) => {
                    let same = same_url(&saved.control_plane_url, target)
                        || saved
                            .join_url
                            .as_deref()
                            .is_some_and(|join_url| same_url(join_url, target));
                    if !same {
                        assessment.findings.push(Finding::ControlPlaneUnverified {
                            saved: saved_at,
                            target: target.to_string(),
                            reason,
                        });
                    }
                    same
                }
            }
        }
        // A pairing code names no address to compare with.
        (Some(_), None) | (None, _) => false,
    };

    // The name this node runs when installing the service or rejoining:
    // what its join asked for, else what the control plane last sent the
    // agent, else the default.
    let own_name = facts
        .saved
        .as_ref()
        .filter(|_| same_cluster)
        .and_then(|saved| {
            saved
                .overlay_network_name
                .clone()
                .or_else(|| saved.applied_overlay_network_name.clone())
        });
    let preferred = own_name
        .clone()
        .unwrap_or_else(|| DEFAULT_OVERLAY_NETWORK_NAME.to_string());
    // A network of the preferred name is this node's own when this is the
    // same cluster and it carries the subnet the agent last applied.
    let own_subnet = facts
        .saved
        .as_ref()
        .filter(|_| same_cluster)
        .and_then(|saved| saved.applied_subnet.as_deref());
    let conflicting = facts.networks.iter().find(|network| {
        network.name == preferred
            && !own_subnet.is_some_and(|own| network.subnets.iter().any(|subnet| subnet == own))
    });
    if let Some(network) = conflicting {
        let mut taken: Vec<String> = facts
            .networks
            .iter()
            .map(|network| network.name.clone())
            .collect();
        taken.extend(facts.interfaces.iter().cloned());
        let chosen = match purpose {
            Purpose::Join { .. } => {
                overlay_name::first_free_overlay_network_name(&preferred, &taken)
            }
            Purpose::ServiceInstall => None,
        };
        assessment.overlay_network_name = chosen.clone();
        assessment.findings.push(Finding::OverlayNameTaken {
            name: network.name.clone(),
            subnets: network.subnets.clone(),
            chosen,
        });
    } else if matches!(purpose, Purpose::Join { .. }) {
        // A re-join keeps the name this node already runs. Registering none
        // would save a config without it, and the next check would take the
        // other cluster's `temps0` beside it for this node's own.
        assessment.overlay_network_name = own_name;
    }

    for port in ports {
        if let Some(listener) = facts.listeners.iter().find(|listener| {
            listener.address.port() == *port
                // Reinstalling the service: the agent it replaces holds its
                // own ports. `temps join` already warned about another one.
                && !(purpose == Purpose::ServiceInstall && is_temps_process(listener))
        }) {
            assessment.findings.push(Finding::PortInUse {
                port: *port,
                address: listener.address,
                process: listener.process.clone(),
                serves_public,
            });
        }
    }
    assessment
}

fn is_temps_process(listener: &Listener) -> bool {
    listener
        .process
        .as_deref()
        .is_some_and(|process| process.starts_with("temps "))
}

/// Parse `/proc/net/tcp` or `/proc/net/tcp6`: the listening sockets and
/// their inodes.
pub fn parse_proc_net_tcp(contents: &str) -> Vec<(SocketAddr, u64)> {
    contents
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            // sl local_address rem_address st tx:rx tr:when retrnsmt uid timeout inode
            if fields.len() < 10 || fields[3] != "0A" {
                return None;
            }
            let address = parse_hex_socket(fields[1])?;
            let inode = fields[9].parse().ok()?;
            Some((address, inode))
        })
        .collect()
}

/// `0100007F:0050` (IPv4) or 32 hex digits (IPv6), each 32-bit word in host
/// byte order as the kernel prints it, then the port in hex.
fn parse_hex_socket(field: &str) -> Option<SocketAddr> {
    let (ip, port) = field.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let ip: std::net::IpAddr = match ip.len() {
        8 => std::net::Ipv4Addr::from(u32::from_str_radix(ip, 16).ok()?.to_le_bytes()).into(),
        32 => {
            let mut bytes = [0u8; 16];
            for (word, chunk) in ip.as_bytes().chunks(8).enumerate() {
                let value = u32::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
                bytes[word * 4..word * 4 + 4].copy_from_slice(&value.to_le_bytes());
            }
            std::net::Ipv6Addr::from(bytes).into()
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

/// Whether the hex `CapEff:` line of `/proc/self/status` grants
/// `CAP_NET_ADMIN` (bit 12).
pub fn has_cap_net_admin(proc_status: &str) -> bool {
    const CAP_NET_ADMIN: u32 = 12;
    proc_status
        .lines()
        .find_map(|line| line.strip_prefix("CapEff:"))
        .and_then(|value| u64::from_str_radix(value.trim(), 16).ok())
        .is_some_and(|caps| caps & (1 << CAP_NET_ADMIN) != 0)
}

/// Look at the host. Every part is best-effort: what cannot be read is left
/// out rather than failing the command.
pub async fn gather(saved: Option<&temps_node_agent::AgentConfig>) -> HostFacts {
    let linux = cfg!(target_os = "linux");
    // SAFETY: geteuid has no preconditions and cannot fail.
    let root = unsafe { libc::geteuid() } == 0;
    let network_admin = root
        || std::fs::read_to_string("/proc/self/status")
            .map(|status| has_cap_net_admin(&status))
            .unwrap_or(false);
    let saved = saved.map(|config| SavedJoin {
        control_plane_url: config.control_plane_url.clone(),
        join_url: config.join_url.clone(),
        overlay_network_name: config.overlay_network_name.clone(),
        applied_subnet: applied_alloc(&config.mesh_key_dir, "compute_cidr"),
        applied_overlay_network_name: applied_alloc(&config.mesh_key_dir, "overlay_network_name"),
        ca_fingerprint: config
            .cluster_ca_path
            .as_deref()
            .and_then(saved_ca_fingerprint),
        control_plane_wg_public_key: saved_control_plane_key(&config.mesh_key_dir),
    });
    HostFacts {
        linux,
        network_admin,
        saved,
        target_identity: None,
        networks: docker_networks().await,
        interfaces: interface_names(),
        listeners: if linux { listeners() } else { Vec::new() },
    }
}

/// The fingerprint of the cluster CA certificate at `path`.
fn saved_ca_fingerprint(path: &std::path::Path) -> Option<String> {
    let pem = std::fs::read_to_string(path).ok()?;
    temps_core::node_pki::ca_fingerprint_sha256(&pem).ok()
}

/// The control plane's mesh key in the agent's last network snapshot.
fn saved_control_plane_key(mesh_key_dir: &std::path::Path) -> Option<String> {
    let data = std::fs::read(mesh_key_dir.join("network-snapshot.json")).ok()?;
    let snapshot: serde_json::Value = serde_json::from_slice(&data).ok()?;
    control_plane_key_in_snapshot(&snapshot)
}

fn control_plane_key_in_snapshot(snapshot: &serde_json::Value) -> Option<String> {
    snapshot
        .pointer("/wireguard/peers")?
        .as_array()?
        .iter()
        .find(|peer| peer.get("name").and_then(|name| name.as_str()) == Some("control-plane"))?
        .get("public_key")?
        .as_str()
        .map(str::to_string)
}

/// Ask the control plane at `target` who it is (ADR 060), before the join
/// token is sent anywhere. Public data, and only compared: the answer never
/// decides what is trusted. Every failure is the reason identity is unknown.
pub async fn fetch_plane_identity(target: &str) -> Result<PlaneIdentity, String> {
    let url =
        crate::commands::api_url::management_api_url(target, "/internal/nodes/plane-identity");
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|error| format!("could not build an HTTP client: {error}"))?;
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|error| format!("could not reach {url}: {error}"))?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(format!(
            "the control plane answered 404 for {url}, so it predates identity checks; upgrade \
             it"
        ));
    }
    if !status.is_success() {
        return Err(format!("the control plane answered {status} for {url}"));
    }
    response
        .json::<PlaneIdentity>()
        .await
        .map_err(|error| format!("the answer from {url} is not a control-plane identity: {error}"))
}

/// A field of this node's allocation in the agent's last network snapshot.
fn applied_alloc(mesh_key_dir: &std::path::Path, field: &str) -> Option<String> {
    let data = std::fs::read(mesh_key_dir.join("network-snapshot.json")).ok()?;
    let snapshot: serde_json::Value = serde_json::from_slice(&data).ok()?;
    snapshot
        .get("alloc")?
        .get(field)?
        .as_str()
        .map(str::to_string)
}

async fn docker_networks() -> Vec<HostNetwork> {
    let Ok(docker) = bollard::Docker::connect_with_defaults() else {
        return Vec::new();
    };
    let listed = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        docker.list_networks(None::<bollard::query_parameters::ListNetworksOptions>),
    )
    .await;
    let Ok(Ok(networks)) = listed else {
        return Vec::new();
    };
    networks
        .into_iter()
        .filter_map(|network| {
            let subnets = network
                .ipam
                .and_then(|ipam| ipam.config)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|config| config.subnet)
                .collect();
            Some(HostNetwork {
                name: network.name?,
                subnets,
            })
        })
        .collect()
}

fn interface_names() -> Vec<String> {
    std::fs::read_dir("/sys/class/net")
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default()
}

fn listeners() -> Vec<Listener> {
    let mut sockets = Vec::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        if let Ok(contents) = std::fs::read_to_string(table) {
            sockets.extend(parse_proc_net_tcp(&contents));
        }
    }
    let owners = socket_owners();
    sockets
        .into_iter()
        .map(|(address, inode)| Listener {
            address,
            process: owners
                .iter()
                .find(|(owned, _)| *owned == inode)
                .map(|(_, process)| process.clone()),
        })
        .collect()
}

/// `(socket inode, "name (pid N)")` for every socket a process holds open.
/// Needs root to see other users' processes; without it, owners are simply
/// unknown.
fn socket_owners() -> Vec<(u64, String)> {
    let Ok(processes) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut owners = Vec::new();
    for process in processes.filter_map(Result::ok) {
        let Some(pid) = process
            .file_name()
            .to_str()
            .and_then(|pid| pid.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(process.path().join("fd")) else {
            continue;
        };
        let name = std::fs::read_to_string(process.path().join("comm"))
            .map(|comm| comm.trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        for fd in fds.filter_map(Result::ok) {
            let Ok(link) = std::fs::read_link(fd.path()) else {
                continue;
            };
            let inode = link
                .to_str()
                .and_then(|link| link.strip_prefix("socket:["))
                .and_then(|rest| rest.strip_suffix(']'))
                .and_then(|inode| inode.parse::<u64>().ok());
            if let Some(inode) = inode {
                owners.push((inode, format!("{name} (pid {pid})")));
            }
        }
    }
    owners
}

#[cfg(test)]
mod tests {
    use super::*;

    fn network(name: &str, subnet: &str) -> HostNetwork {
        HostNetwork {
            name: name.into(),
            subnets: vec![subnet.into()],
        }
    }

    fn saved(url: &str) -> SavedJoin {
        SavedJoin {
            control_plane_url: url.into(),
            join_url: None,
            overlay_network_name: None,
            applied_subnet: None,
            applied_overlay_network_name: None,
            ca_fingerprint: None,
            control_plane_wg_public_key: None,
        }
    }

    fn identity(ca: Option<&str>, key: Option<&str>) -> Option<Result<PlaneIdentity, String>> {
        Some(Ok(PlaneIdentity {
            ca_fingerprint: ca.map(str::to_string),
            control_plane_wg_public_key: key.map(str::to_string),
        }))
    }

    /// An agent joined outbound saved only the mesh address, which every
    /// cluster's control plane shares.
    fn joined_over_the_mesh(ca: Option<&str>, key: Option<&str>) -> SavedJoin {
        let mut previous = saved("https://10.201.0.1:51820");
        previous.ca_fingerprint = ca.map(str::to_string);
        previous.control_plane_wg_public_key = key.map(str::to_string);
        previous
    }

    #[test]
    fn the_same_control_plane_under_another_address_is_recognised_by_its_ca() {
        let facts = HostFacts {
            saved: Some(joined_over_the_mesh(Some("ABCD"), None)),
            target_identity: identity(Some("abcd"), Some("cp-key")),
            ..admin()
        };
        let assessment = assess(&facts, join("https://cp.example"), &[], true);
        assert_eq!(assessment.plane, PlaneMatch::Same);
        assert_eq!(assessment.blocking(false), None);
        assert!(assessment.findings.is_empty(), "{:?}", assessment.findings);
    }

    #[test]
    fn without_a_saved_ca_the_mesh_key_identifies_the_control_plane() {
        let facts = HostFacts {
            saved: Some(joined_over_the_mesh(None, Some("cp-key"))),
            target_identity: identity(Some("abcd"), Some("cp-key")),
            ..admin()
        };
        assert_eq!(
            assess(&facts, join("https://cp.example"), &[], true).plane,
            PlaneMatch::Same
        );

        let other = HostFacts {
            saved: Some(joined_over_the_mesh(None, Some("cp-key"))),
            target_identity: identity(None, Some("other-key")),
            ..admin()
        };
        let assessment = assess(&other, join("https://cp.example"), &[], true);
        assert_eq!(assessment.plane, PlaneMatch::Different);
        let Some(Finding::OtherControlPlane { evidence, .. }) = assessment.blocking(false) else {
            panic!("{:?}", assessment.findings);
        };
        assert!(evidence.contains("other-key"), "{evidence}");
    }

    /// A pinned CA is not traded for the mesh key: a control plane that
    /// omits its CA but repeats the saved (public) mesh key is not known to
    /// be the same one, so it is never handed this node's agent token.
    #[test]
    fn a_target_without_a_ca_is_not_matched_by_mesh_key_against_a_pinned_ca() {
        let facts = HostFacts {
            saved: Some(joined_over_the_mesh(Some("abcd"), Some("cp-key"))),
            target_identity: identity(None, Some("cp-key")),
            ..admin()
        };
        assert_eq!(
            assess(&facts, join("https://cp.example"), &[], true).plane,
            PlaneMatch::Unknown
        );
    }

    /// The identity outranks the address: a different CA at the saved URL
    /// is another control plane (a reinstall), and the message says which.
    #[test]
    fn another_ca_is_another_control_plane_even_at_the_saved_address() {
        let mut previous = saved("https://cp.example");
        previous.ca_fingerprint = Some("abcd".into());
        let facts = HostFacts {
            saved: Some(previous),
            target_identity: identity(Some("ef01"), None),
            ..admin()
        };
        let assessment = assess(&facts, join("https://cp.example"), &[], true);
        assert_eq!(assessment.plane, PlaneMatch::Different);
        let blocking = assessment.blocking(false).expect("blocks without --force");
        let message = blocking.message();
        assert!(
            message.contains("ef01") && message.contains("abcd"),
            "{message}"
        );
        assert_eq!(assessment.blocking(true), None);
    }

    /// Identity that cannot be determined is reported as exactly that, with
    /// why, never as another cluster.
    #[test]
    fn an_unknown_identity_says_so_instead_of_claiming_another_cluster() {
        let facts = HostFacts {
            saved: Some(joined_over_the_mesh(None, None)),
            target_identity: Some(Err("the control plane answered 404".into())),
            ..admin()
        };
        let assessment = assess(&facts, join("https://cp.example"), &[], true);
        assert_eq!(assessment.plane, PlaneMatch::Unknown);
        let Some(Finding::ControlPlaneUnverified { reason, .. }) = assessment.blocking(false)
        else {
            panic!("{:?}", assessment.findings);
        };
        assert!(reason.contains("404"), "{reason}");
        let message = assessment.blocking(false).unwrap().message();
        assert!(message.contains("could not be determined"), "{message}");
        assert!(!message.contains("another control plane"), "{message}");
        assert_eq!(assessment.blocking(true), None);

        // Nothing comparable on either side: also unknown, and why.
        let facts = HostFacts {
            saved: Some(joined_over_the_mesh(Some("abcd"), None)),
            target_identity: identity(None, Some("cp-key")),
            ..admin()
        };
        let assessment = assess(&facts, join("https://cp.example"), &[], true);
        let Some(Finding::ControlPlaneUnverified { reason, .. }) = assessment.blocking(false)
        else {
            panic!("{:?}", assessment.findings);
        };
        assert!(reason.contains("only a cluster CA"), "{reason}");
        assert!(reason.contains("only its mesh key"), "{reason}");
    }

    #[test]
    fn reads_the_control_plane_key_from_a_network_snapshot() {
        let snapshot = serde_json::json!({
            "wireguard": {
                "cidr": "10.201.0.0/24",
                "peers": [
                    { "name": "edge-2", "public_key": "peer-key" },
                    { "name": "control-plane", "public_key": "cp-key" }
                ]
            }
        });
        assert_eq!(
            control_plane_key_in_snapshot(&snapshot).as_deref(),
            Some("cp-key")
        );
        assert_eq!(
            control_plane_key_in_snapshot(&serde_json::json!({ "peers": [] })),
            None
        );
    }

    fn join(target: &str) -> Purpose<'_> {
        Purpose::Join {
            target: Some(target),
        }
    }

    fn admin() -> HostFacts {
        HostFacts {
            linux: true,
            network_admin: true,
            ..HostFacts::default()
        }
    }

    #[test]
    fn a_clean_host_passes() {
        let assessment = assess(&admin(), join("https://cp.example"), &[80, 443], true);
        assert_eq!(assessment, Assessment::default());
    }

    #[test]
    fn without_network_admin_the_join_stops_before_the_token_is_used() {
        let facts = HostFacts {
            network_admin: false,
            ..admin()
        };
        let assessment = assess(&facts, join("https://cp.example"), &[], true);
        assert_eq!(assessment.blocking(true), Some(&Finding::NeedsRoot));
        assert!(Finding::NeedsRoot.message().contains("sudo"));
        // Not on another OS: nothing there needs it.
        let mac = HostFacts {
            linux: false,
            ..facts
        };
        assert_eq!(
            assess(&mac, join("https://cp.example"), &[], true).blocking(false),
            None
        );
    }

    #[test]
    fn a_join_for_another_control_plane_needs_force() {
        let facts = HostFacts {
            saved: Some(saved("https://old.example")),
            ..admin()
        };
        let assessment = assess(&facts, join("https://new.example"), &[], true);
        // Nothing identifies either control plane: unknown, not "another".
        assert!(matches!(
            assessment.blocking(false),
            Some(Finding::ControlPlaneUnverified { .. })
        ));
        assert_eq!(assessment.blocking(true), None);
    }

    #[test]
    fn a_rejoin_of_the_same_cluster_needs_no_force() {
        let mut previous = saved("https://10.201.0.1:3100");
        previous.join_url = Some("https://cp.example/".into());
        let facts = HostFacts {
            saved: Some(previous),
            ..admin()
        };
        assert_eq!(
            assess(&facts, join("https://cp.example"), &[], true).blocking(false),
            None
        );
    }

    #[test]
    fn another_clusters_overlay_gets_a_free_name_beside_it() {
        let facts = HostFacts {
            networks: vec![
                network("temps0", "172.20.3.0/24"),
                network("temps1", "10.9.0.0/24"),
            ],
            ..admin()
        };
        let assessment = assess(&facts, join("https://cp.example"), &[], true);
        assert_eq!(assessment.overlay_network_name.as_deref(), Some("temps2"));
        assert_eq!(
            assessment.findings,
            vec![Finding::OverlayNameTaken {
                name: "temps0".into(),
                subnets: vec!["172.20.3.0/24".into()],
                chosen: Some("temps2".into()),
            }]
        );
        assert_eq!(assessment.blocking(false), None);
        assert!(assessment.findings[0].message().contains("'temps2'"));
    }

    #[test]
    fn this_nodes_own_overlay_is_kept_on_a_rejoin() {
        let mut previous = saved("https://cp.example");
        previous.applied_subnet = Some("10.202.4.0/24".into());
        let facts = HostFacts {
            saved: Some(previous),
            networks: vec![network("temps0", "10.202.4.0/24")],
            ..admin()
        };
        assert_eq!(
            assess(&facts, join("https://cp.example"), &[], true),
            Assessment::default()
        );
    }

    #[test]
    fn a_rejoin_registers_the_name_this_node_already_runs() {
        let mut previous = saved("https://cp.example");
        previous.overlay_network_name = Some("temps1".into());
        previous.applied_subnet = Some("10.205.4.0/24".into());
        let facts = HostFacts {
            saved: Some(previous),
            networks: vec![
                network("temps0", "10.202.0.0/24"),
                network("temps1", "10.205.4.0/24"),
            ],
            ..admin()
        };
        let assessment = assess(&facts, join("https://cp.example"), &[], true);
        assert_eq!(assessment.overlay_network_name.as_deref(), Some("temps1"));
        assert_eq!(assessment.findings, vec![]);
    }

    #[test]
    fn a_config_that_lost_the_name_falls_back_to_the_applied_one() {
        // A re-join before this fix saved no overlay name; the snapshot
        // still says `temps1`, so `temps0` is the other cluster's.
        let mut previous = saved("https://cp.example");
        previous.applied_subnet = Some("10.205.4.0/24".into());
        previous.applied_overlay_network_name = Some("temps1".into());
        let facts = HostFacts {
            saved: Some(previous),
            networks: vec![
                network("temps0", "10.202.0.0/24"),
                network("temps1", "10.205.4.0/24"),
            ],
            ..admin()
        };
        assert_eq!(
            assess(&facts, Purpose::ServiceInstall, &[], true),
            Assessment::default()
        );
        let rejoin = assess(&facts, join("https://cp.example"), &[], true);
        assert_eq!(rejoin.overlay_network_name.as_deref(), Some("temps1"));
        assert_eq!(rejoin.findings, vec![]);
    }

    #[test]
    fn the_same_subnet_from_another_cluster_is_not_ours() {
        let mut previous = saved("https://old.example");
        previous.applied_subnet = Some("10.202.4.0/24".into());
        let facts = HostFacts {
            saved: Some(previous),
            networks: vec![network("temps0", "10.202.4.0/24")],
            ..admin()
        };
        let assessment = assess(&facts, join("https://new.example"), &[], true);
        assert_eq!(assessment.overlay_network_name.as_deref(), Some("temps1"));
    }

    #[test]
    fn a_leftover_interface_also_counts_as_taken() {
        let facts = HostFacts {
            networks: vec![network("temps0", "172.20.3.0/24")],
            interfaces: vec!["br-temps1".into()],
            ..admin()
        };
        let assessment = assess(&facts, Purpose::Join { target: None }, &[], true);
        assert_eq!(assessment.overlay_network_name.as_deref(), Some("temps2"));
    }

    #[test]
    fn service_install_warns_about_the_name_it_was_joined_with_but_renames_nothing() {
        let mut previous = saved("https://cp.example");
        previous.overlay_network_name = Some("temps1".into());
        let facts = HostFacts {
            saved: Some(previous.clone()),
            // The other cluster's network is expected beside ours.
            networks: vec![network("temps0", "172.20.3.0/24")],
            ..admin()
        };
        assert_eq!(
            assess(&facts, Purpose::ServiceInstall, &[], true),
            Assessment::default()
        );

        let taken = HostFacts {
            saved: Some(previous),
            networks: vec![network("temps1", "172.20.9.0/24")],
            ..admin()
        };
        let assessment = assess(&taken, Purpose::ServiceInstall, &[], true);
        assert_eq!(assessment.overlay_network_name, None);
        assert!(matches!(
            assessment.findings.as_slice(),
            [Finding::OverlayNameTaken { chosen: None, .. }]
        ));
    }

    #[test]
    fn held_ports_are_reported_with_their_process() {
        let facts = HostFacts {
            listeners: vec![
                Listener {
                    address: "0.0.0.0:80".parse().unwrap(),
                    process: Some("nginx (pid 42)".into()),
                },
                Listener {
                    address: "127.0.0.1:5432".parse().unwrap(),
                    process: None,
                },
            ],
            ..admin()
        };
        let assessment = assess(&facts, join("https://cp.example"), &[80, 443], true);
        assert_eq!(
            assessment.findings,
            vec![Finding::PortInUse {
                port: 80,
                address: "0.0.0.0:80".parse().unwrap(),
                process: Some("nginx (pid 42)".into()),
                serves_public: true,
            }]
        );
        assert_eq!(assessment.blocking(false), None);
        assert!(assessment.findings[0].message().contains("nginx (pid 42)"));
        assert!(assessment.findings[0]
            .message()
            .contains("public traffic on 80/443"));
    }

    /// Ingress `off` binds no public listener: 443 held cannot stop the
    /// worker, and the message about 80 does not blame public traffic.
    #[test]
    fn ingress_off_checks_only_the_overlay_proxy_port() {
        use temps_core::worker_ingress::{IngressMode, LocalIngress};
        let off = LocalIngress::default();
        assert_eq!(served_ports(&off), (vec![80], false));
        let fixed = LocalIngress {
            mode: IngressMode::Fixed,
            http_port: 8080,
            https_port: 8443,
            ..LocalIngress::default()
        };
        assert_eq!(served_ports(&fixed), (vec![80, 443, 8080, 8443], true));
        // `private` serves its own fixed ports, never the public ingress
        // ports, and takes no public traffic on them.
        let private = LocalIngress {
            mode: IngressMode::Private,
            http_port: 8080,
            https_port: 8443,
            ..LocalIngress::default()
        };
        assert_eq!(served_ports(&private), (vec![80, 443], false));

        let facts = HostFacts {
            listeners: vec![
                Listener {
                    address: "0.0.0.0:80".parse().unwrap(),
                    process: None,
                },
                Listener {
                    address: "0.0.0.0:443".parse().unwrap(),
                    process: None,
                },
            ],
            ..admin()
        };
        let (ports, serves_public) = served_ports(&off);
        let assessment = assess(&facts, Purpose::ServiceInstall, &ports, serves_public);
        assert!(matches!(
            assessment.findings.as_slice(),
            [Finding::PortInUse {
                port: 80,
                serves_public: false,
                ..
            }]
        ));
        let message = assessment.findings[0].message();
        assert!(message.contains("no public traffic"), "{message}");
        assert!(!message.contains("80/443"), "{message}");
    }

    #[test]
    fn reinstalling_the_service_ignores_the_ports_its_own_agent_holds() {
        let facts = HostFacts {
            saved: Some(saved("https://cp.example")),
            listeners: vec![Listener {
                address: "0.0.0.0:443".parse().unwrap(),
                process: Some("temps (pid 7)".into()),
            }],
            ..admin()
        };
        assert_eq!(
            assess(&facts, Purpose::ServiceInstall, &[80, 443], true),
            Assessment::default()
        );
        assert_eq!(
            assess(&facts, join("https://cp.example"), &[443], true)
                .findings
                .len(),
            1
        );
    }

    #[test]
    fn parses_listening_sockets_from_proc() {
        let tcp = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   \
            0: 00000000:0050 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 12345 1 0 100 0 0 10 0\n   \
            1: 0100007F:1F90 0100007F:D431 01 00000000:00000000 00:00000000 00000000     0        0 999 1 0 100 0 0 10 0\n";
        assert_eq!(
            parse_proc_net_tcp(tcp),
            vec![("0.0.0.0:80".parse().unwrap(), 12345)]
        );
        let tcp6 = "header\n   \
            0: 00000000000000000000000001000000:01BB 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 777 1\n";
        assert_eq!(
            parse_proc_net_tcp(tcp6),
            vec![("[::1]:443".parse().unwrap(), 777)]
        );
    }

    #[test]
    fn reads_cap_net_admin_from_the_effective_set() {
        assert!(has_cap_net_admin(
            "Name:\ttemps\nCapEff:\t0000000000001000\n"
        ));
        assert!(has_cap_net_admin("CapEff:\t000001ffffffffff\n"));
        assert!(!has_cap_net_admin("CapEff:\t0000000000000000\n"));
        assert!(!has_cap_net_admin("CapPrm:\t0000000000001000\n"));
    }
}
