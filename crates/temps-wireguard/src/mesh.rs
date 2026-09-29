// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Managed WireGuard mesh between cluster nodes.
//!
//! Every node (the control plane included) runs one kernel WireGuard
//! interface, [`MESH_INTERFACE`], with a private mesh address. That address is
//! the node's overlay *underlay*: VXLAN runs over this interface, so nodes that
//! only share public IPs still get a private, encrypted path to each other.
//!
//! Keys are generated on the node and never leave it: the private key lives in
//! a `0600` file under the node's data directory, and only the public key is
//! sent to the control plane, over the agent's authenticated channel.
//!
//! The kernel backend is used because the interface must outlive the process
//! that configures it (an agent restart must not drop cross-node traffic) and
//! because the userspace backend is far slower for data-plane traffic.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};

use crate::WireGuardError;

/// Interface name every node uses for the mesh.
pub const MESH_INTERFACE: &str = "temps-wg0";

/// Seconds between keepalives. Keeps NAT and stateful-firewall mappings open
/// so a node behind NAT stays reachable once it has spoken first.
pub const PERSISTENT_KEEPALIVE_SECS: u16 = 25;

/// Largest interface MTU: the wg-quick value for a 1500-byte path. Hosts on
/// smaller paths get less (see [`mesh_mtu_for`]).
pub const MESH_MTU: u32 = 1420;

const PRIVATE_KEY_FILE: &str = "private.key";

/// A node's mesh identity.
#[derive(Clone)]
pub struct MeshKey {
    private_key: String,
    public_key: String,
}

impl std::fmt::Debug for MeshKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeshKey")
            .field("public_key", &self.public_key)
            .finish_non_exhaustive()
    }
}

impl MeshKey {
    /// Load the private key from `dir/private.key`, creating it (mode `0600`,
    /// directory `0700`) on first use.
    pub fn load_or_create(dir: &Path) -> Result<Self, WireGuardError> {
        let path = dir.join(PRIVATE_KEY_FILE);
        match std::fs::read_to_string(&path) {
            Ok(contents) => Self::from_private_key(contents.trim()).map_err(|error| {
                WireGuardError::InvalidConfig(format!(
                    "{} does not hold a valid WireGuard private key ({error}); \
                     delete it to generate a new key for this node",
                    path.display()
                ))
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let key = Self::generate()?;
                write_private_file(dir, &path, &key.private_key)?;
                Ok(key)
            }
            Err(error) => Err(WireGuardError::Io(error)),
        }
    }

    /// The key in `dir/private.key`, without creating one. `None` when the
    /// file does not exist.
    pub fn load(dir: &Path) -> Result<Option<Self>, WireGuardError> {
        let path = dir.join(PRIVATE_KEY_FILE);
        match std::fs::read_to_string(&path) {
            Ok(contents) => Self::from_private_key(contents.trim())
                .map(Some)
                .map_err(|error| {
                    WireGuardError::InvalidConfig(format!(
                        "{} does not hold a valid WireGuard private key ({error})",
                        path.display()
                    ))
                }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(WireGuardError::Io(error)),
        }
    }

    fn generate() -> Result<Self, WireGuardError> {
        let secret = temps_core::ecies::generate_x25519_static_secret().map_err(|error| {
            WireGuardError::OperationFailed {
                operation: "generate WireGuard key".to_string(),
                reason: error.to_string(),
            }
        })?;
        let public = x25519_dalek::PublicKey::from(&secret);
        Ok(Self {
            private_key: BASE64.encode(secret.to_bytes()),
            public_key: BASE64.encode(public.as_bytes()),
        })
    }

    fn from_private_key(encoded: &str) -> Result<Self, WireGuardError> {
        let bytes: [u8; 32] = decode_key(encoded)?;
        let secret = x25519_dalek::StaticSecret::from(bytes);
        let public = x25519_dalek::PublicKey::from(&secret);
        Ok(Self {
            private_key: encoded.to_string(),
            public_key: BASE64.encode(public.as_bytes()),
        })
    }

    pub fn public_key(&self) -> &str {
        &self.public_key
    }
}

/// Whether `encoded` is a base64 WireGuard key (32 bytes).
pub fn is_valid_public_key(encoded: &str) -> bool {
    decode_key(encoded).is_ok()
}

fn decode_key(encoded: &str) -> Result<[u8; 32], WireGuardError> {
    let bytes = BASE64
        .decode(encoded)
        .map_err(|error| WireGuardError::InvalidConfig(format!("key is not base64: {error}")))?;
    bytes.try_into().map_err(|bytes: Vec<u8>| {
        WireGuardError::InvalidConfig(format!("key is {} bytes, expected 32", bytes.len()))
    })
}

fn write_private_file(dir: &Path, path: &Path, contents: &str) -> Result<(), WireGuardError> {
    create_private_dir(dir)?;
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    std::fs::write(path, contents)?;
    Ok(())
}

/// Create `dir` (and any missing parents) owner-only from the start, and
/// tighten it if it already existed with a looser mode.
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)
}

/// A private key as the kernel reports it back. X25519 clamps the scalar on
/// import (clears the low 3 bits and the top bit, sets bit 254), so a key
/// generated without clamping reads back as different base64 while being the
/// same key.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn clamped_private_key(encoded: &str) -> Option<String> {
    let mut bytes: [u8; 32] = BASE64.decode(encoded).ok()?.try_into().ok()?;
    bytes[0] &= 248;
    bytes[31] &= 127;
    bytes[31] |= 64;
    Some(BASE64.encode(bytes))
}

/// Directory holding a node's mesh key under its data directory.
pub fn key_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("wireguard")
}

/// The local end of the mesh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshInterface {
    pub address: Ipv4Addr,
    pub prefix_len: u8,
    pub listen_port: u16,
    /// See [`mesh_mtu_for`].
    pub mtu: u32,
}

/// WireGuard's worst-case per-packet overhead (IPv6 outer header), as
/// wg-quick assumes.
pub const WIREGUARD_OVERHEAD: u32 = 80;

/// The mesh interface MTU for a host whose path to its peers carries
/// `path_mtu`-byte packets: that minus WireGuard's overhead, capped at the
/// standard [`MESH_MTU`] (internet paths are 1500) and never below 1280.
pub fn mesh_mtu_for(path_mtu: u32) -> u32 {
    path_mtu
        .saturating_sub(WIREGUARD_OVERHEAD)
        .clamp(1280, MESH_MTU)
}

/// Another node as this node's WireGuard peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshPeer {
    pub public_key: String,
    /// Where the peer's WireGuard socket is reachable. `None` for a peer that
    /// has no reachable address (it must dial us; keepalives keep it alive).
    pub endpoint: Option<SocketAddr>,
    /// The peer's mesh address. Only this `/32` is routed to the peer:
    /// overlay traffic is VXLAN between mesh addresses, never raw compute
    /// CIDRs.
    pub address: Ipv4Addr,
    /// When the peer is the mesh hub (ADR 048 D4): the mesh addresses of the
    /// members this node cannot reach directly, whose traffic it relays.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relayed: Vec<Ipv4Addr>,
}

impl MeshPeer {
    /// The `/32`s routed to this peer: its own address, then the members it
    /// relays for us.
    pub fn allowed_ips(&self) -> Vec<String> {
        std::iter::once(self.address)
            .chain(self.relayed.iter().copied())
            .map(|address| format!("{address}/32"))
            .collect()
    }
}

/// Handshake state for one configured peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshPeerStatus {
    pub public_key: String,
    pub endpoint: Option<SocketAddr>,
    pub last_handshake: Option<SystemTime>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

/// [`MESH_INTERFACE`] as the kernel holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshInterfaceState {
    /// Derived from the interface's private key.
    pub public_key: Option<String>,
    pub listen_port: u16,
    /// IPv4 addresses, as `a.b.c.d/len`.
    pub addresses: Vec<String>,
    pub mtu: u32,
    pub up: bool,
    pub peers: Vec<MeshPeerStatus>,
}

/// What [`reconcile_peers`] changed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PeerChanges {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
}

impl PeerChanges {
    pub fn is_empty(&self) -> bool {
        self.added == 0 && self.updated == 0 && self.removed == 0
    }
}

/// WireGuard drops a session this long after its last handshake
/// (REJECT_AFTER_TIME); a handshake inside it means the path works.
pub const LIVE_HANDSHAKE: std::time::Duration = std::time::Duration::from_secs(180);

/// A peer as the interface currently holds it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone)]
struct CurrentPeer {
    endpoint: Option<SocketAddr>,
    allowed_ips: Vec<String>,
    /// Handshook within [`LIVE_HANDSHAKE`].
    live: bool,
}

/// The desired peer set diffed against what the interface holds.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn plan_peer_changes<'a>(
    current: &HashMap<String, CurrentPeer>,
    desired: &'a [MeshPeer],
) -> (Vec<&'a MeshPeer>, Vec<String>, PeerChanges) {
    let mut changes = PeerChanges::default();
    let mut to_set = Vec::new();
    for peer in desired {
        let mut wanted_ips = peer.allowed_ips();
        wanted_ips.sort();
        match current.get(&peer.public_key) {
            None => {
                changes.added += 1;
                to_set.push(peer);
            }
            Some(held) => {
                // WireGuard moves a peer's endpoint to wherever its
                // authenticated packets come from (a NAT mapping, a new IP).
                // While that path is live it beats the address the peer
                // registered, so only a dead or never-set path is rewritten.
                let endpoint_differs =
                    peer.endpoint.is_some() && held.endpoint != peer.endpoint && !held.live;
                let mut held_ips = held.allowed_ips.clone();
                held_ips.sort();
                if endpoint_differs || held_ips != wanted_ips {
                    changes.updated += 1;
                    to_set.push(peer);
                }
            }
        }
    }
    let to_remove: Vec<String> = current
        .keys()
        .filter(|key| !desired.iter().any(|peer| &peer.public_key == *key))
        .cloned()
        .collect();
    changes.removed = to_remove.len();
    (to_set, to_remove, changes)
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use defguard_wireguard_rs::{
        key::Key, net::IpAddrMask, peer::Peer, InterfaceConfiguration, Kernel, WGApi,
        WireguardInterfaceApi,
    };

    fn api() -> Result<WGApi<Kernel>, WireGuardError> {
        WGApi::<Kernel>::new(MESH_INTERFACE).map_err(|error| {
            WireGuardError::InterfaceError(format!(
                "cannot open WireGuard interface {MESH_INTERFACE}: {error}"
            ))
        })
    }

    fn parse_key(encoded: &str) -> Result<Key, WireGuardError> {
        Key::try_from(encoded).map_err(|error| {
            WireGuardError::InvalidConfig(format!("invalid WireGuard key: {error:?}"))
        })
    }

    /// `ip` output for the mesh interface; used where defguard has no read API
    /// (interface addresses) and to bring an existing link up.
    fn ip(args: &[&str]) -> Result<String, WireGuardError> {
        let output = std::process::Command::new("ip")
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|error| WireGuardError::OperationFailed {
                operation: format!("ip {}", args.join(" ")),
                reason: error.to_string(),
            })?;
        if !output.status.success() {
            return Err(WireGuardError::OperationFailed {
                operation: format!("ip {}", args.join(" ")),
                reason: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// The IPv4 addresses on the interface, as `a.b.c.d/len`.
    fn interface_addresses() -> Result<Vec<String>, WireGuardError> {
        let listing = ip(&["-4", "-o", "addr", "show", "dev", MESH_INTERFACE])?;
        Ok(listing
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                fields.find(|field| *field == "inet")?;
                fields.next().map(str::to_string)
            })
            .collect())
    }

    /// The interface MTU from sysfs.
    fn interface_mtu() -> Result<u32, WireGuardError> {
        let path = format!("/sys/class/net/{MESH_INTERFACE}/mtu");
        let value = std::fs::read_to_string(&path)?;
        value.trim().parse().map_err(|error| {
            WireGuardError::InterfaceError(format!("unreadable MTU in {path}: {error}"))
        })
    }

    pub fn ensure_interface(
        interface: &MeshInterface,
        key: &MeshKey,
    ) -> Result<bool, WireGuardError> {
        let mut api = api()?;
        let address: IpAddrMask = format!("{}/{}", interface.address, interface.prefix_len)
            .parse()
            .map_err(|error| {
                WireGuardError::InvalidConfig(format!("invalid mesh address: {error}"))
            })?;
        match api.read_interface_data() {
            Ok(host) => {
                // configure_interface flushes the addresses and replaces the
                // whole peer set, dropping every tunnel and every endpoint the
                // kernel learned from a roaming peer. A restart that finds the
                // interface already right must leave it alone.
                let settled = host.private_key.map(|current| current.to_string())
                    == clamped_private_key(&key.private_key)
                    && host.listen_port == interface.listen_port
                    && interface_addresses()? == vec![address.to_string()];
                if settled {
                    let mtu_changed = interface_mtu()? != interface.mtu;
                    if mtu_changed {
                        // Settable in place, unlike key/address: no peer loss.
                        ip(&[
                            "link",
                            "set",
                            "dev",
                            MESH_INTERFACE,
                            "mtu",
                            &interface.mtu.to_string(),
                        ])?;
                    }
                    ip(&["link", "set", "dev", MESH_INTERFACE, "up"])?;
                    return Ok(mtu_changed);
                }
            }
            Err(_) => {
                api.create_interface().map_err(|error| {
                    WireGuardError::InterfaceError(format!(
                        "cannot create WireGuard interface {MESH_INTERFACE}: {error}. \
                         The kernel needs WireGuard support (Linux 5.6+ or the \
                         wireguard module) and this process needs CAP_NET_ADMIN"
                    ))
                })?;
            }
        }
        // Sets key, port, MTU and the one address, and clears the peers; the
        // caller reconciles peers straight after.
        api.configure_interface(&InterfaceConfiguration {
            name: MESH_INTERFACE.to_string(),
            prvkey: key.private_key.clone(),
            addresses: vec![address],
            port: interface.listen_port,
            peers: Vec::new(),
            mtu: Some(interface.mtu),
            fwmark: None,
        })
        .map_err(|error| {
            WireGuardError::InterfaceError(format!(
                "cannot configure WireGuard interface {MESH_INTERFACE}: {error}"
            ))
        })?;
        ip(&["link", "set", "dev", MESH_INTERFACE, "up"])?;
        Ok(true)
    }

    pub fn reconcile_peers(desired: &[MeshPeer]) -> Result<PeerChanges, WireGuardError> {
        let api = api()?;
        let host = api.read_interface_data().map_err(|error| {
            WireGuardError::InterfaceError(format!(
                "cannot read WireGuard interface {MESH_INTERFACE}: {error}"
            ))
        })?;
        let now = SystemTime::now();
        let current: HashMap<String, CurrentPeer> = host
            .peers
            .values()
            .map(|peer| {
                let live = peer
                    .last_handshake
                    .and_then(|at| now.duration_since(at).ok())
                    .is_some_and(|age| age < LIVE_HANDSHAKE);
                (
                    peer.public_key.to_string(),
                    CurrentPeer {
                        endpoint: peer.endpoint,
                        allowed_ips: peer.allowed_ips.iter().map(|ip| ip.to_string()).collect(),
                        live,
                    },
                )
            })
            .collect();
        let (to_set, to_remove, changes) = plan_peer_changes(&current, desired);
        for public_key in to_remove {
            api.remove_peer(&parse_key(&public_key)?).map_err(|error| {
                WireGuardError::OperationFailed {
                    operation: format!("remove WireGuard peer {public_key}"),
                    reason: error.to_string(),
                }
            })?;
        }
        for desired in to_set {
            let mut peer = Peer::new(parse_key(&desired.public_key)?);
            peer.endpoint = desired.endpoint;
            peer.persistent_keepalive_interval = Some(PERSISTENT_KEEPALIVE_SECS);
            peer.allowed_ips = desired
                .allowed_ips()
                .iter()
                .map(|ip| ip.parse())
                .collect::<Result<_, _>>()
                .map_err(|error| {
                    WireGuardError::InvalidConfig(format!("invalid peer address: {error}"))
                })?;
            api.configure_peer(&peer)
                .map_err(|error| WireGuardError::OperationFailed {
                    operation: format!("configure WireGuard peer {}", desired.public_key),
                    reason: error.to_string(),
                })?;
        }
        Ok(changes)
    }

    pub fn interface_state() -> Result<Option<MeshInterfaceState>, WireGuardError> {
        if !Path::new("/sys/class/net").join(MESH_INTERFACE).exists() {
            return Ok(None);
        }
        let host = api()?.read_interface_data().map_err(|error| {
            WireGuardError::InterfaceError(format!(
                "cannot read WireGuard interface {MESH_INTERFACE}: {error}"
            ))
        })?;
        let operstate =
            std::fs::read_to_string(format!("/sys/class/net/{MESH_INTERFACE}/operstate"))?;
        let flags = std::fs::read_to_string(format!("/sys/class/net/{MESH_INTERFACE}/flags"))?;
        // WireGuard links report operstate "unknown" while up; IFF_UP is 0x1.
        let up = operstate.trim() == "up"
            || u32::from_str_radix(flags.trim().trim_start_matches("0x"), 16)
                .is_ok_and(|flags| flags & 1 == 1);
        Ok(Some(MeshInterfaceState {
            public_key: host
                .private_key
                .and_then(|key| MeshKey::from_private_key(&key.to_string()).ok())
                .map(|key| key.public_key),
            listen_port: host.listen_port,
            addresses: interface_addresses()?,
            mtu: interface_mtu()?,
            up,
            peers: peer_status()?,
        }))
    }

    pub fn peer_status() -> Result<Vec<MeshPeerStatus>, WireGuardError> {
        let host = api()?.read_interface_data().map_err(|error| {
            WireGuardError::InterfaceError(format!(
                "cannot read WireGuard interface {MESH_INTERFACE}: {error}"
            ))
        })?;
        Ok(host
            .peers
            .values()
            .map(|peer| MeshPeerStatus {
                public_key: peer.public_key.to_string(),
                endpoint: peer.endpoint,
                last_handshake: peer
                    .last_handshake
                    .filter(|time| *time > SystemTime::UNIX_EPOCH),
                rx_bytes: peer.rx_bytes,
                tx_bytes: peer.tx_bytes,
            })
            .collect())
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::*;

    fn unsupported() -> WireGuardError {
        WireGuardError::InterfaceError(
            "the managed WireGuard mesh needs Linux kernel WireGuard; \
             cluster nodes must run Linux"
                .to_string(),
        )
    }

    pub fn ensure_interface(_: &MeshInterface, _: &MeshKey) -> Result<bool, WireGuardError> {
        Err(unsupported())
    }

    pub fn reconcile_peers(_: &[MeshPeer]) -> Result<PeerChanges, WireGuardError> {
        Err(unsupported())
    }

    pub fn peer_status() -> Result<Vec<MeshPeerStatus>, WireGuardError> {
        Err(unsupported())
    }

    pub fn interface_state() -> Result<Option<MeshInterfaceState>, WireGuardError> {
        Err(unsupported())
    }
}

/// Create [`MESH_INTERFACE`] if missing and make sure it is up with this key,
/// address, port and MTU. An interface that already matches is left
/// untouched, so its tunnels survive a restart; one that differs is
/// reconfigured and loses its peers until the next [`reconcile_peers`].
///
/// Returns whether anything changed. A recreated interface also means the
/// kernel deleted every VXLAN device stacked on it, and a new MTU changes
/// the overlay's: either way the overlay on top must be rebuilt.
pub fn ensure_interface(interface: &MeshInterface, key: &MeshKey) -> Result<bool, WireGuardError> {
    imp::ensure_interface(interface, key)
}

/// Make the interface's peers exactly `desired`: add missing peers, update
/// changed ones, remove peers no longer in the cluster (revocation).
pub fn reconcile_peers(desired: &[MeshPeer]) -> Result<PeerChanges, WireGuardError> {
    imp::reconcile_peers(desired)
}

/// Handshake state of every configured peer.
pub fn peer_status() -> Result<Vec<MeshPeerStatus>, WireGuardError> {
    imp::peer_status()
}

/// The mesh interface as the kernel holds it; `None` when it does not exist.
pub fn interface_state() -> Result<Option<MeshInterfaceState>, WireGuardError> {
    imp::interface_state()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(endpoint: Option<SocketAddr>, allowed_ips: Vec<String>, live: bool) -> CurrentPeer {
        CurrentPeer {
            endpoint,
            allowed_ips,
            live,
        }
    }

    fn peer(key: &str, address: [u8; 4], endpoint: Option<&str>) -> MeshPeer {
        MeshPeer {
            public_key: key.to_string(),
            endpoint: endpoint.map(|value| value.parse().unwrap()),
            address: Ipv4Addr::from(address),
            relayed: Vec::new(),
        }
    }

    #[test]
    fn key_is_created_once_with_owner_only_permissions() {
        let dir = std::env::temp_dir().join(format!("temps-wg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let first = MeshKey::load_or_create(&dir).unwrap();
        let second = MeshKey::load_or_create(&dir).unwrap();

        assert_eq!(first.public_key(), second.public_key());
        assert!(is_valid_public_key(first.public_key()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(PRIVATE_KEY_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_corrupt_key_file_is_reported_not_replaced() {
        let dir = std::env::temp_dir().join(format!("temps-wg-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(PRIVATE_KEY_FILE), "not-a-key").unwrap();

        let error = MeshKey::load_or_create(&dir).unwrap_err().to_string();

        assert!(error.contains("delete it"), "{error}");
        assert_eq!(
            std::fs::read_to_string(dir.join(PRIVATE_KEY_FILE)).unwrap(),
            "not-a-key"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mesh_mtu_leaves_room_for_wireguard_on_the_path() {
        assert_eq!(mesh_mtu_for(1500), 1420);
        assert_eq!(mesh_mtu_for(1460), 1380, "GCP");
        assert_eq!(mesh_mtu_for(1450), 1370, "OpenStack, VXLAN-backed clouds");
        assert_eq!(mesh_mtu_for(9000), 1420, "jumbo LAN, internet peers");
        assert_eq!(mesh_mtu_for(1300), 1280, "never below IPv6's minimum");
    }

    #[test]
    fn a_private_key_compares_in_its_clamped_form() {
        // All bits set: clamping must clear bits 0-2 and 255 and keep 254.
        let unclamped = BASE64.encode([0xffu8; 32]);
        let clamped = clamped_private_key(&unclamped).unwrap();
        let bytes = BASE64.decode(&clamped).unwrap();
        assert_eq!(bytes[0], 0xf8);
        assert_eq!(bytes[31], 0x7f);
        assert_eq!(bytes[1..31], [0xffu8; 30]);
        // The kernel's copy is already clamped; clamping again is a no-op.
        assert_eq!(clamped_private_key(&clamped).unwrap(), clamped);
        assert_eq!(clamped_private_key("not base64"), None);
    }

    #[test]
    fn rejects_malformed_public_keys() {
        assert!(!is_valid_public_key("abc"));
        assert!(!is_valid_public_key(&BASE64.encode([0u8; 31])));
        assert!(is_valid_public_key(&BASE64.encode([7u8; 32])));
    }

    #[test]
    fn plans_add_update_and_remove() {
        let current = HashMap::from([
            (
                "keep".to_string(),
                held(
                    Some("203.0.113.1:51820".parse().unwrap()),
                    vec!["10.201.0.2/32".to_string()],
                    false,
                ),
            ),
            (
                "moved".to_string(),
                held(
                    Some("203.0.113.2:51820".parse().unwrap()),
                    vec!["10.201.0.3/32".to_string()],
                    false,
                ),
            ),
            (
                "gone".to_string(),
                held(None, vec!["10.201.0.4/32".to_string()], false),
            ),
        ]);
        let desired = vec![
            peer("keep", [10, 201, 0, 2], Some("203.0.113.1:51820")),
            peer("moved", [10, 201, 0, 3], Some("198.51.100.9:51820")),
            peer("new", [10, 201, 0, 5], Some("198.51.100.10:51820")),
        ];

        let (to_set, to_remove, changes) = plan_peer_changes(&current, &desired);

        assert_eq!(
            to_set
                .iter()
                .map(|p| p.public_key.as_str())
                .collect::<Vec<_>>(),
            vec!["moved", "new"]
        );
        assert_eq!(to_remove, vec!["gone".to_string()]);
        assert_eq!(
            changes,
            PeerChanges {
                added: 1,
                updated: 1,
                removed: 1
            }
        );
    }

    #[test]
    fn a_hub_carries_the_members_it_relays() {
        let mut hub = peer("hub", [10, 201, 0, 1], Some("198.51.100.1:51820"));
        hub.relayed = vec![Ipv4Addr::new(10, 201, 0, 9), Ipv4Addr::new(10, 201, 0, 7)];
        assert_eq!(
            hub.allowed_ips(),
            vec!["10.201.0.1/32", "10.201.0.9/32", "10.201.0.7/32"]
        );

        // The kernel lists allowed IPs in its own order.
        let current = HashMap::from([(
            "hub".to_string(),
            held(
                Some("198.51.100.1:51820".parse().unwrap()),
                vec![
                    "10.201.0.7/32".to_string(),
                    "10.201.0.1/32".to_string(),
                    "10.201.0.9/32".to_string(),
                ],
                true,
            ),
        )]);
        let desired = vec![hub.clone()];
        let (to_set, _, changes) = plan_peer_changes(&current, &desired);
        assert!(to_set.is_empty(), "same set, other order");
        assert!(changes.is_empty());

        // A member stops being relayed (its direct link came back).
        hub.relayed.pop();
        let desired = vec![hub];
        let (to_set, _, changes) = plan_peer_changes(&current, &desired);
        assert_eq!(to_set.len(), 1);
        assert_eq!(changes.updated, 1);
    }

    #[test]
    fn a_roamed_endpoint_is_kept_when_we_have_none_to_offer() {
        let current = HashMap::from([(
            "natted".to_string(),
            held(
                Some("198.51.100.7:40000".parse().unwrap()),
                vec!["10.201.0.9/32".to_string()],
                false,
            ),
        )]);
        let desired = vec![peer("natted", [10, 201, 0, 9], None)];

        let (to_set, to_remove, changes) = plan_peer_changes(&current, &desired);

        assert!(to_set.is_empty());
        assert!(to_remove.is_empty());
        assert!(changes.is_empty());
    }

    #[test]
    fn a_live_roamed_endpoint_beats_the_registered_one() {
        // The control plane moved, or the peer sits behind NAT: its packets
        // arrive from somewhere other than the endpoint it registered.
        let roamed: SocketAddr = "198.51.100.7:40000".parse().unwrap();
        let desired = vec![peer("natted", [10, 201, 0, 9], Some("10.0.0.9:51820"))];

        let live = HashMap::from([(
            "natted".to_string(),
            held(Some(roamed), vec!["10.201.0.9/32".to_string()], true),
        )]);
        let (to_set, _, changes) = plan_peer_changes(&live, &desired);
        assert!(to_set.is_empty(), "a working path is left alone");
        assert!(changes.is_empty());

        let stale = HashMap::from([(
            "natted".to_string(),
            held(Some(roamed), vec!["10.201.0.9/32".to_string()], false),
        )]);
        let (to_set, _, changes) = plan_peer_changes(&stale, &desired);
        assert_eq!(
            to_set.len(),
            1,
            "a dead path falls back to the registered endpoint"
        );
        assert_eq!(changes.updated, 1);
    }
}
