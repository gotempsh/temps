// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Mesh doctor (ADR 048 D9): what is wrong with this host's end of the
//! WireGuard mesh, and what fixes it.
//!
//! The caller says what the host should look like ([`Expected`]: from the
//! node's last network snapshot, or the control plane's database);
//! [`observe`] reads the host; [`diagnose`] turns both into checks, each
//! failing one with the action that fixes it. The rules live in
//! [`diagnose`] so they are testable without a kernel.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::time::{Duration, SystemTime};

use serde::Serialize;

use crate::mesh::{
    LockdownState, MeshInterfaceState, MeshKey, MeshLockdown, LIVE_HANDSHAKE, MESH_INTERFACE,
};

/// Which kind of host is being diagnosed; decides what "restart" means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostRole {
    Node,
    ControlPlane,
}

impl HostRole {
    fn service(self) -> &'static str {
        match self {
            Self::Node => "temps agent",
            Self::ControlPlane => "temps serve",
        }
    }
}

/// This host's end of the mesh as the cluster knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    pub role: HostRole,
    /// The key the cluster has for this host.
    pub public_key: String,
    pub address: Ipv4Addr,
    pub prefix_len: u8,
    pub listen_port: u16,
    /// Where other members dial this host; `None` when they cannot.
    pub endpoint: Option<SocketAddr>,
    pub peers: Vec<ExpectedPeer>,
    pub lockdown: MeshLockdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedPeer {
    pub name: String,
    pub public_key: String,
    /// Where this host dials the peer; `None` when the peer dials in.
    pub endpoint: Option<SocketAddr>,
    pub address: Ipv4Addr,
}

/// Whether something else holds the mesh port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortProbe {
    Free,
    InUse,
    Unknown(String),
}

/// What [`observe`] (and the caller, for the node API) found on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    /// Public key of this host's key file; `None` when there is none.
    pub key_file: Result<Option<String>, String>,
    pub interface: Result<Option<MeshInterfaceState>, String>,
    /// Probed only when the interface is missing.
    pub port: Option<PortProbe>,
    pub lockdown: Result<LockdownState, String>,
    /// A full-size, unfragmentable packet to the named peer: `None` when
    /// no peer had a live handshake to probe.
    pub mtu_probe: Option<(String, Result<(), String>)>,
    pub node_api: Option<NodeApiProbe>,
}

/// Whether the control plane's node API answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeApiProbe {
    /// What was dialed, for the output.
    pub target: String,
    /// `Ok(what answered)` or why nothing did.
    pub result: Result<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
    Info,
}

/// One finding: a state, plus the action that fixes it when it fails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MeshCheck {
    pub label: String,
    pub status: CheckStatus,
    pub detail: String,
    pub fix: Option<String>,
}

impl MeshCheck {
    fn new(label: impl Into<String>, status: CheckStatus, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            status,
            detail: detail.into(),
            fix: None,
        }
    }

    fn fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

/// Read this host's end of the mesh. `key_dir` holds the host's key file.
pub async fn observe(expected: &Expected, key_dir: &Path) -> Observed {
    let dir = key_dir.to_path_buf();
    let key_file = tokio::task::spawn_blocking(move || MeshKey::load(&dir))
        .await
        .map_err(|error| error.to_string())
        .and_then(|loaded| loaded.map_err(|error| error.to_string()))
        .map(|key| key.map(|key| key.public_key().to_string()));
    let interface = tokio::task::spawn_blocking(crate::mesh::interface_state)
        .await
        .map_err(|error| error.to_string())
        .and_then(|state| state.map_err(|error| error.to_string()));
    let port = match &interface {
        Ok(None) => Some(
            match tokio::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, expected.listen_port)).await {
                Ok(_) => PortProbe::Free,
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => PortProbe::InUse,
                Err(error) => PortProbe::Unknown(error.to_string()),
            },
        ),
        _ => None,
    };
    let lockdown = crate::mesh::lockdown_state(&expected.lockdown)
        .await
        .map_err(|error| error.to_string());
    let mtu_probe = match &interface {
        Ok(Some(state)) => match live_peer(expected, state, SystemTime::now()) {
            Some(peer) => Some((peer.name.clone(), probe_mtu(peer.address, state.mtu).await)),
            None => None,
        },
        _ => None,
    };
    Observed {
        key_file,
        interface,
        port,
        lockdown,
        mtu_probe,
        node_api: None,
    }
}

/// The first expected peer with a live handshake.
fn live_peer<'a>(
    expected: &'a Expected,
    state: &MeshInterfaceState,
    now: SystemTime,
) -> Option<&'a ExpectedPeer> {
    expected.peers.iter().find(|peer| {
        state
            .peers
            .iter()
            .find(|held| held.public_key == peer.public_key)
            .and_then(|held| held.last_handshake)
            .and_then(|at| now.duration_since(at).ok())
            .is_some_and(|age| age < LIVE_HANDSHAKE)
    })
}

/// Send one `mtu`-byte packet with Don't Fragment set to `address` over the
/// mesh (`ping -M do`).
async fn probe_mtu(address: Ipv4Addr, mtu: u32) -> Result<(), String> {
    // 20 bytes of IPv4 header and 8 of ICMP ride inside the MTU.
    let payload = mtu.saturating_sub(28).to_string();
    let output = tokio::process::Command::new("ping")
        .args(["-c", "1", "-W", "2", "-M", "do", "-s", &payload])
        .arg(address.to_string())
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|error| format!("could not run ping: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        Err(stderr
            .lines()
            .chain(stdout.lines())
            .find(|line| !line.trim().is_empty())
            .unwrap_or("no reply")
            .trim()
            .to_string())
    }
}

/// The checks for `expected` against `observed`, in the order an operator
/// should work through them.
pub fn diagnose(expected: &Expected, observed: &Observed, now: SystemTime) -> Vec<MeshCheck> {
    use CheckStatus::*;
    let service = expected.role.service();
    let restart = format!("Restart `{service}`: it rebuilds this host's end of the mesh.");
    let mut checks = Vec::new();

    checks.push(match &observed.key_file {
        Ok(Some(key)) if *key == expected.public_key => {
            MeshCheck::new("Mesh key", Pass, format!("public key {key}"))
        }
        Ok(Some(key)) => MeshCheck::new(
            "Mesh key",
            Fail,
            format!(
                "this host's key file holds {key}, but the cluster knows it by {}",
                expected.public_key
            ),
        )
        .fix(format!(
            "Restart `{service}` so it registers the key it holds. If the key file was copied \
             from another machine, delete it first; a new one is generated."
        )),
        Ok(None) => MeshCheck::new("Mesh key", Fail, "this host has no mesh key file").fix(
            match expected.role {
                HostRole::Node => format!("Restart `{service}`: it creates one and registers it."),
                HostRole::ControlPlane => restart.clone(),
            },
        ),
        Err(error) => MeshCheck::new("Mesh key", Fail, error.clone())
            .fix("Delete the unreadable key file and restart; a new key is generated."),
    });

    let expected_address = format!("{}/{}", expected.address, expected.prefix_len);
    let interface = match &observed.interface {
        Ok(Some(state)) => Some(state),
        Ok(None) => {
            let mut check = MeshCheck::new(
                "Interface",
                Fail,
                format!("{MESH_INTERFACE} does not exist"),
            )
            .fix(format!(
                "Start `{service}`; it creates {MESH_INTERFACE}. It needs Linux 5.6+ (or the \
                 wireguard kernel module) and root or CAP_NET_ADMIN."
            ));
            if observed.port == Some(PortProbe::InUse) {
                check.detail = format!(
                    "{MESH_INTERFACE} does not exist, and another process holds UDP {}",
                    expected.listen_port
                );
                check.fix = Some(format!(
                    "Find it with `ss -ulpn 'sport = :{port}'` and stop it (a pairing command \
                     still waiting holds it too), then start `{service}`.",
                    port = expected.listen_port
                ));
            }
            checks.push(check);
            None
        }
        Err(error) => {
            checks.push(
                MeshCheck::new(
                    "Interface",
                    Fail,
                    format!("could not read {MESH_INTERFACE}: {error}"),
                )
                .fix("Run this as root."),
            );
            None
        }
    };

    if let Some(state) = interface {
        let mut wrong = Vec::new();
        if state.public_key.as_deref() != Some(expected.public_key.as_str()) {
            wrong.push("its key is not the one the cluster knows".to_string());
        }
        if state.addresses != [expected_address.clone()] {
            wrong.push(format!(
                "its address is {} instead of {expected_address}",
                if state.addresses.is_empty() {
                    "unset".to_string()
                } else {
                    state.addresses.join(", ")
                }
            ));
        }
        if state.listen_port != expected.listen_port {
            wrong.push(format!(
                "it listens on UDP {} instead of {}",
                state.listen_port, expected.listen_port
            ));
        }
        if !state.up {
            wrong.push("it is down".to_string());
        }
        checks.push(if wrong.is_empty() {
            MeshCheck::new(
                "Interface",
                Pass,
                format!(
                    "{MESH_INTERFACE} up at {expected_address}, UDP {}, MTU {}",
                    state.listen_port, state.mtu
                ),
            )
        } else {
            MeshCheck::new(
                "Interface",
                Fail,
                format!("{MESH_INTERFACE}: {}", wrong.join("; ")),
            )
            .fix(restart.clone())
        });

        for peer in &expected.peers {
            checks.push(peer_check(expected, state, peer, now, &restart));
        }
        let unknown = state
            .peers
            .iter()
            .filter(|held| {
                !expected
                    .peers
                    .iter()
                    .any(|peer| peer.public_key == held.public_key)
            })
            .count();
        if unknown > 0 {
            checks.push(MeshCheck::new(
                "Peers",
                Info,
                format!(
                    "{unknown} peer(s) on {MESH_INTERFACE} are not in this host's last view of \
                     the cluster; `{service}` removes them on its next sync if they left"
                ),
            ));
        }

        checks.push(match &observed.mtu_probe {
            Some((peer, Ok(()))) => MeshCheck::new(
                "MTU",
                Pass,
                format!("{}-byte packets reach {peer} unfragmented", state.mtu),
            ),
            Some((peer, Err(error))) if error.starts_with("could not run ping") => MeshCheck::new(
                "MTU",
                Info,
                format!(
                    "not probed ({error}); {MESH_INTERFACE} uses MTU {}",
                    state.mtu
                ),
            )
            .fix(format!(
                "Install ping (iputils) to probe the path to {peer}."
            )),
            Some((peer, Err(error))) => MeshCheck::new(
                "MTU",
                Fail,
                format!(
                    "a {}-byte packet did not reach {peer} unfragmented: {error}",
                    state.mtu
                ),
            )
            .fix(
                "Something on the path carries smaller packets (a VPN, PPPoE, a tunnel). Set the \
                 underlay MTU (`temps join --underlay-mtu`, or `temps agent --underlay-mtu`) to \
                 what the path carries; the mesh subtracts WireGuard's overhead.",
            ),
            None => MeshCheck::new(
                "MTU",
                Info,
                format!(
                    "not probed: no peer has a live handshake; {MESH_INTERFACE} uses MTU {}",
                    state.mtu
                ),
            ),
        });
    }

    checks.push(match &observed.lockdown {
        Ok(LockdownState::Current) => MeshCheck::new(
            "Firewall",
            Pass,
            "the mesh lockdown (nftables table inet temps_mesh) is current",
        ),
        Ok(LockdownState::Outdated) => MeshCheck::new(
            "Firewall",
            Warn,
            "the mesh lockdown is installed for other settings or an older version",
        )
        .fix(restart.clone()),
        Ok(LockdownState::Missing) => MeshCheck::new(
            "Firewall",
            Fail,
            "the mesh lockdown (nftables table inet temps_mesh) is missing: every peer can reach \
             anything this host listens on",
        )
        .fix(restart.clone()),
        Err(error) => MeshCheck::new("Firewall", Warn, format!("could not inspect: {error}"))
            .fix("Run this as root on a host with nftables (`nft`)."),
    });

    if let Some(probe) = &observed.node_api {
        checks.push(match &probe.result {
            Ok(answer) => MeshCheck::new(
                "Control plane",
                Pass,
                format!("{} answered ({answer})", probe.target),
            ),
            Err(error) => {
                let fix = match expected.role {
                    HostRole::Node => format!(
                        "If the control plane's handshake above fails, fix that first. Otherwise \
                         the control plane is not serving {}: upgrade it and check its logs for \
                         \"node API\".",
                        probe.target
                    ),
                    HostRole::ControlPlane => format!(
                        "`temps serve` serves the node API once its end of the mesh is up; check \
                         its logs for \"node API\" (another process may hold {}).",
                        probe.target
                    ),
                };
                MeshCheck::new(
                    "Control plane",
                    Fail,
                    format!("{} did not answer: {error}", probe.target),
                )
                .fix(fix)
            }
        });
    }

    checks
}

fn peer_check(
    expected: &Expected,
    state: &MeshInterfaceState,
    peer: &ExpectedPeer,
    now: SystemTime,
    restart: &str,
) -> MeshCheck {
    use CheckStatus::*;
    let label = format!("Peer {}", peer.name);
    let Some(held) = state
        .peers
        .iter()
        .find(|held| held.public_key == peer.public_key)
    else {
        return MeshCheck::new(label, Fail, format!("not configured on {MESH_INTERFACE}"))
            .fix(restart.to_string());
    };
    let via = held
        .endpoint
        .map(|endpoint| format!(" via {endpoint}"))
        .unwrap_or_default();
    let age = held
        .last_handshake
        .map(|at| now.duration_since(at).unwrap_or(Duration::ZERO));
    match age {
        Some(age) if age < LIVE_HANDSHAKE => {
            MeshCheck::new(label, Pass, format!("handshake {} ago{via}", describe(age)))
        }
        Some(age) => MeshCheck::new(
            label,
            Warn,
            format!(
                "last handshake {} ago{via}; the tunnel is down",
                describe(age)
            ),
        )
        .fix(path_fix(expected, peer)),
        None => MeshCheck::new(label, Fail, "never handshook").fix(path_fix(expected, peer)),
    }
}

/// What to open so this host and `peer` can reach each other: at least one
/// of them must be dialable, and the dialed one's port open to the other.
fn path_fix(expected: &Expected, peer: &ExpectedPeer) -> String {
    let port = expected.listen_port;
    match (expected.endpoint, peer.endpoint) {
        (_, Some(peer_endpoint)) => format!(
            "This host dials {name} at {peer_endpoint}: allow UDP {peer_port} inbound on {name} \
             (its provider's firewall and host firewall) and outbound from here. If {peer_endpoint} \
             is not {name}'s public address, correct it with `temps agent --wg-endpoint` on {name}.",
            name = peer.name,
            peer_port = peer_endpoint.port()
        ),
        (Some(own), None) => format!(
            "{name} dials this host at {own}: allow UDP {port} inbound here (provider firewall \
             and host firewall).",
            name = peer.name
        ),
        (None, None) => format!(
            "Neither this host nor {name} has an address the other can dial. Give one a \
             reachable endpoint: `temps agent --wg-endpoint <public ip:{port}>` on a node, or \
             `--private-address` on the control plane.",
            name = peer.name
        ),
    }
}

fn describe(age: Duration) -> String {
    let secs = age.as_secs();
    if secs < 120 {
        format!("{secs}s")
    } else if secs < 7200 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::MeshPeerStatus;

    fn key(byte: u8) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode([byte; 32])
    }

    fn expected() -> Expected {
        Expected {
            role: HostRole::Node,
            public_key: key(1),
            address: "10.201.0.5".parse().unwrap(),
            prefix_len: 24,
            listen_port: 51820,
            endpoint: Some("198.51.100.7:51820".parse().unwrap()),
            peers: vec![ExpectedPeer {
                name: "control-plane".into(),
                public_key: key(9),
                endpoint: None,
                address: "10.201.0.1".parse().unwrap(),
            }],
            lockdown: MeshLockdown {
                vxlan_port: 4789,
                mesh: "10.201.0.0/24".parse().unwrap(),
                node_api_port: None,
                relay: false,
            },
        }
    }

    fn healthy(now: SystemTime) -> Observed {
        Observed {
            key_file: Ok(Some(key(1))),
            interface: Ok(Some(MeshInterfaceState {
                public_key: Some(key(1)),
                listen_port: 51820,
                addresses: vec!["10.201.0.5/24".into()],
                mtu: 1420,
                up: true,
                peers: vec![MeshPeerStatus {
                    public_key: key(9),
                    endpoint: Some("203.0.113.1:51820".parse().unwrap()),
                    last_handshake: Some(now - Duration::from_secs(20)),
                    rx_bytes: 1,
                    tx_bytes: 1,
                }],
            })),
            port: None,
            lockdown: Ok(LockdownState::Current),
            mtu_probe: Some(("control-plane".into(), Ok(()))),
            node_api: Some(NodeApiProbe {
                target: "https://10.201.0.1:51820".into(),
                result: Ok("HTTP 401".into()),
            }),
        }
    }

    fn status_of<'a>(checks: &'a [MeshCheck], label: &str) -> &'a MeshCheck {
        checks
            .iter()
            .find(|check| check.label == label)
            .unwrap_or_else(|| panic!("no {label} check in {checks:?}"))
    }

    #[test]
    fn a_healthy_host_passes_every_check() {
        let now = SystemTime::now();
        let checks = diagnose(&expected(), &healthy(now), now);
        assert!(
            checks.iter().all(|check| check.status == CheckStatus::Pass),
            "{checks:#?}"
        );
        assert_eq!(checks.len(), 6);
    }

    #[test]
    fn a_peer_that_dials_in_is_fixed_by_opening_this_hosts_port() {
        let now = SystemTime::now();
        let mut observed = healthy(now);
        if let Ok(Some(state)) = &mut observed.interface {
            state.peers[0].last_handshake = None;
        }
        observed.mtu_probe = None;
        let checks = diagnose(&expected(), &observed, now);
        let peer = status_of(&checks, "Peer control-plane");
        assert_eq!(peer.status, CheckStatus::Fail);
        let fix = peer.fix.as_deref().unwrap();
        assert!(
            fix.contains("allow UDP 51820 inbound here"),
            "the control plane dials this node: {fix}"
        );
        assert_eq!(status_of(&checks, "MTU").status, CheckStatus::Info);
    }

    #[test]
    fn a_peer_this_host_dials_is_fixed_on_the_peers_side() {
        let now = SystemTime::now();
        let mut expected = expected();
        expected.endpoint = None;
        expected.peers[0].endpoint = Some("203.0.113.1:51999".parse().unwrap());
        let mut observed = healthy(now);
        if let Ok(Some(state)) = &mut observed.interface {
            state.peers[0].last_handshake = Some(now - Duration::from_secs(600));
        }
        let checks = diagnose(&expected, &observed, now);
        let peer = status_of(&checks, "Peer control-plane");
        assert_eq!(peer.status, CheckStatus::Warn);
        assert!(peer.detail.contains("10m ago"), "{}", peer.detail);
        assert!(peer
            .fix
            .as_deref()
            .unwrap()
            .contains("allow UDP 51999 inbound on control-plane"));
    }

    #[test]
    fn neither_end_dialable_says_so() {
        let now = SystemTime::now();
        let mut expected = expected();
        expected.endpoint = None;
        let mut observed = healthy(now);
        if let Ok(Some(state)) = &mut observed.interface {
            state.peers[0].last_handshake = None;
        }
        let checks = diagnose(&expected, &observed, now);
        assert!(status_of(&checks, "Peer control-plane")
            .fix
            .as_deref()
            .unwrap()
            .contains("Neither this host nor control-plane"));
    }

    #[test]
    fn a_missing_interface_names_what_holds_the_port() {
        let now = SystemTime::now();
        let mut observed = healthy(now);
        observed.interface = Ok(None);
        observed.port = Some(PortProbe::InUse);
        observed.mtu_probe = None;
        let checks = diagnose(&expected(), &observed, now);
        let interface = status_of(&checks, "Interface");
        assert_eq!(interface.status, CheckStatus::Fail);
        assert!(interface.detail.contains("another process holds UDP 51820"));
        assert!(!checks
            .iter()
            .any(|check| check.label.starts_with("Peer") || check.label == "MTU"));
    }

    #[test]
    fn a_misconfigured_interface_lists_every_difference() {
        let now = SystemTime::now();
        let mut observed = healthy(now);
        if let Ok(Some(state)) = &mut observed.interface {
            state.public_key = Some(key(2));
            state.addresses = vec!["10.201.0.9/24".into()];
            state.listen_port = 51821;
        }
        let checks = diagnose(&expected(), &observed, now);
        let interface = status_of(&checks, "Interface");
        assert_eq!(interface.status, CheckStatus::Fail);
        for fragment in ["its key", "10.201.0.9/24", "UDP 51821"] {
            assert!(interface.detail.contains(fragment), "{}", interface.detail);
        }
        assert!(interface.fix.as_deref().unwrap().contains("temps agent"));
    }

    #[test]
    fn a_copied_key_file_and_a_missing_firewall_fail_with_fixes() {
        let now = SystemTime::now();
        let mut expected = expected();
        expected.role = HostRole::ControlPlane;
        let mut observed = healthy(now);
        observed.key_file = Ok(Some(key(3)));
        observed.lockdown = Ok(LockdownState::Missing);
        observed.node_api = Some(NodeApiProbe {
            target: "10.201.0.1:51820".into(),
            result: Err("connection refused".into()),
        });
        let checks = diagnose(&expected, &observed, now);
        assert_eq!(status_of(&checks, "Mesh key").status, CheckStatus::Fail);
        let firewall = status_of(&checks, "Firewall");
        assert_eq!(firewall.status, CheckStatus::Fail);
        assert!(firewall.fix.as_deref().unwrap().contains("temps serve"));
        assert_eq!(
            status_of(&checks, "Control plane").status,
            CheckStatus::Fail
        );
    }

    #[test]
    fn an_mtu_black_hole_is_a_failure_with_the_setting_to_change() {
        let now = SystemTime::now();
        let mut observed = healthy(now);
        observed.mtu_probe = Some((
            "control-plane".into(),
            Err("ping: local error: message too long, mtu=1400".into()),
        ));
        let checks = diagnose(&expected(), &observed, now);
        let mtu = status_of(&checks, "MTU");
        assert_eq!(mtu.status, CheckStatus::Fail);
        assert!(mtu.fix.as_deref().unwrap().contains("--underlay-mtu"));
    }
}
