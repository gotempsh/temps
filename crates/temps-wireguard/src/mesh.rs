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

/// Interface MTU. WireGuard over IPv4 adds 60 bytes and over IPv6 80; 1420 is
/// the wg-quick default for a 1500-byte path.
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
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
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

/// The desired peer set diffed against what the interface holds.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn plan_peer_changes<'a>(
    current: &HashMap<String, (Option<SocketAddr>, Vec<String>)>,
    desired: &'a [MeshPeer],
) -> (Vec<&'a MeshPeer>, Vec<String>, PeerChanges) {
    let mut changes = PeerChanges::default();
    let mut to_set = Vec::new();
    for peer in desired {
        let wanted_ips = vec![format!("{}/32", peer.address)];
        match current.get(&peer.public_key) {
            None => {
                changes.added += 1;
                to_set.push(peer);
            }
            Some((endpoint, allowed_ips)) => {
                // An endpoint the kernel learned from a roaming peer is fine
                // when we have none to offer; only a different known endpoint
                // or different allowed IPs need a write.
                let endpoint_differs = peer.endpoint.is_some() && *endpoint != peer.endpoint;
                if endpoint_differs || *allowed_ips != wanted_ips {
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

    pub fn ensure_interface(
        interface: &MeshInterface,
        key: &MeshKey,
    ) -> Result<(), WireGuardError> {
        let mut api = api()?;
        if api.read_interface_data().is_err() {
            api.create_interface().map_err(|error| {
                WireGuardError::InterfaceError(format!(
                    "cannot create WireGuard interface {MESH_INTERFACE}: {error}. \
                     The kernel needs WireGuard support (Linux 5.6+ or the \
                     wireguard module) and this process needs CAP_NET_ADMIN"
                ))
            })?;
        }
        let address: IpAddrMask = format!("{}/{}", interface.address, interface.prefix_len)
            .parse()
            .map_err(|error| {
                WireGuardError::InvalidConfig(format!("invalid mesh address: {error}"))
            })?;
        // configure_interface replaces key, port, MTU and addresses; it adds
        // (never removes) peers, so existing tunnels survive a restart.
        api.configure_interface(&InterfaceConfiguration {
            name: MESH_INTERFACE.to_string(),
            prvkey: key.private_key.clone(),
            addresses: vec![address],
            port: interface.listen_port,
            peers: Vec::new(),
            mtu: Some(MESH_MTU),
            fwmark: None,
        })
        .map_err(|error| {
            WireGuardError::InterfaceError(format!(
                "cannot configure WireGuard interface {MESH_INTERFACE}: {error}"
            ))
        })
    }

    pub fn reconcile_peers(desired: &[MeshPeer]) -> Result<PeerChanges, WireGuardError> {
        let api = api()?;
        let host = api.read_interface_data().map_err(|error| {
            WireGuardError::InterfaceError(format!(
                "cannot read WireGuard interface {MESH_INTERFACE}: {error}"
            ))
        })?;
        let current: HashMap<String, (Option<SocketAddr>, Vec<String>)> = host
            .peers
            .values()
            .map(|peer| {
                (
                    peer.public_key.to_string(),
                    (
                        peer.endpoint,
                        peer.allowed_ips.iter().map(|ip| ip.to_string()).collect(),
                    ),
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
            peer.allowed_ips =
                vec![format!("{}/32", desired.address).parse().map_err(|error| {
                    WireGuardError::InvalidConfig(format!("invalid peer address: {error}"))
                })?];
            api.configure_peer(&peer)
                .map_err(|error| WireGuardError::OperationFailed {
                    operation: format!("configure WireGuard peer {}", desired.public_key),
                    reason: error.to_string(),
                })?;
        }
        Ok(changes)
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

    pub fn ensure_interface(_: &MeshInterface, _: &MeshKey) -> Result<(), WireGuardError> {
        Err(unsupported())
    }

    pub fn reconcile_peers(_: &[MeshPeer]) -> Result<PeerChanges, WireGuardError> {
        Err(unsupported())
    }

    pub fn peer_status() -> Result<Vec<MeshPeerStatus>, WireGuardError> {
        Err(unsupported())
    }
}

/// Create [`MESH_INTERFACE`] if missing and set its key, address, port and
/// MTU. Existing peers are kept.
pub fn ensure_interface(interface: &MeshInterface, key: &MeshKey) -> Result<(), WireGuardError> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(key: &str, address: [u8; 4], endpoint: Option<&str>) -> MeshPeer {
        MeshPeer {
            public_key: key.to_string(),
            endpoint: endpoint.map(|value| value.parse().unwrap()),
            address: Ipv4Addr::from(address),
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
                (
                    Some("203.0.113.1:51820".parse().unwrap()),
                    vec!["10.201.0.2/32".to_string()],
                ),
            ),
            (
                "moved".to_string(),
                (
                    Some("203.0.113.2:51820".parse().unwrap()),
                    vec!["10.201.0.3/32".to_string()],
                ),
            ),
            (
                "gone".to_string(),
                (None, vec!["10.201.0.4/32".to_string()]),
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
    fn a_roamed_endpoint_is_kept_when_we_have_none_to_offer() {
        let current = HashMap::from([(
            "natted".to_string(),
            (
                Some("198.51.100.7:40000".parse().unwrap()),
                vec!["10.201.0.9/32".to_string()],
            ),
        )]);
        let desired = vec![peer("natted", [10, 201, 0, 9], None)];

        let (to_set, to_remove, changes) = plan_peer_changes(&current, &desired);

        assert!(to_set.is_empty());
        assert!(to_remove.is_empty());
        assert!(changes.is_empty());
    }
}
