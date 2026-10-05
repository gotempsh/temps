// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Managed WireGuard mesh as the overlay underlay (see `temps_wireguard::mesh`
//! for the data plane).
//!
//! The control plane owns the address plan: it takes the first host of
//! `network_config.wireguard_cidr`, and hands each node the lowest free
//! address when the node's agent registers its public key. A node's mesh
//! address then becomes its `underlay_address`, so the existing VXLAN overlay
//! runs over WireGuard without further changes.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use ipnet::Ipv4Net;
use thiserror::Error;

pub use temps_wireguard::mesh::{
    interface_state, key_dir, mesh_mtu_for, peer_status, MeshInterfaceState, MeshKey, MeshPeer,
    MeshPeerStatus, LIVE_HANDSHAKE, MESH_INTERFACE,
};
pub use temps_wireguard::WireGuardError;

use crate::error::NetworkError;

/// What may arrive over the mesh interface; see [`ensure_lockdown`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshLockdown {
    /// The overlay's VXLAN port, the one service every peer needs.
    pub vxlan_port: u16,
    /// The mesh pool. Any member may reach published container ports: the
    /// control plane's proxy and health checks, and ingress nodes forwarding
    /// app traffic to containers on other nodes (every node may take ingress
    /// for any domain, ADR-020). Nothing else on a host is reachable over the
    /// mesh.
    pub mesh: Ipv4Net,
    /// On the control plane only: the node API port (ADR 048 D3), which
    /// mesh members reach on the control plane's mesh address.
    pub node_api_port: Option<u16>,
    /// This host is the mesh hub (ADR 048 D4): it forwards traffic between
    /// mesh members, from the tunnel back into the tunnel, and nothing else.
    pub relay: bool,
}

/// Install the nftables lockdown for [`MESH_INTERFACE`] unless it is already
/// current; returns whether it (re)installed. Must run before the interface
/// is created: the lockdown is what keeps the tunnel from being a way into
/// the host, so it never depends on the overlay having bootstrapped.
#[cfg(target_os = "linux")]
pub async fn ensure_lockdown(lockdown: &MeshLockdown) -> Result<bool, NetworkError> {
    crate::linux::firewall::ensure_mesh_lockdown(lockdown).await
}

#[cfg(not(target_os = "linux"))]
pub async fn ensure_lockdown(_lockdown: &MeshLockdown) -> Result<bool, NetworkError> {
    Err(NetworkError::UnsupportedPlatform {
        target: std::env::consts::OS,
    })
}

/// Relay mesh traffic between members (the mesh hub, ADR 048 D4), or stop.
/// Run after the interface exists; the lockdown carries the matching
/// nftables rule.
#[cfg(target_os = "linux")]
pub async fn ensure_relay(mesh: Ipv4Net, enabled: bool) -> Result<(), NetworkError> {
    crate::linux::firewall::ensure_mesh_relay(mesh, enabled).await
}

/// Make the next [`ensure_relay`] check everything again instead of trusting
/// its last verification: call it when the mesh interface was recreated or
/// could not be reconciled.
#[cfg(target_os = "linux")]
pub fn forget_relay() {
    crate::linux::firewall::forget_mesh_relay();
}

#[cfg(not(target_os = "linux"))]
pub fn forget_relay() {}

#[cfg(not(target_os = "linux"))]
pub async fn ensure_relay(_mesh: Ipv4Net, enabled: bool) -> Result<(), NetworkError> {
    if enabled {
        return Err(NetworkError::UnsupportedPlatform {
            target: std::env::consts::OS,
        });
    }
    Ok(())
}

/// Whether a host's mesh lockdown is the one it should have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockdownState {
    Current,
    /// Installed, but for other settings or an older version.
    Outdated,
    Missing,
}

/// Inspect the mesh lockdown without changing it.
#[cfg(target_os = "linux")]
pub async fn lockdown_state(lockdown: &MeshLockdown) -> Result<LockdownState, NetworkError> {
    crate::linux::firewall::mesh_lockdown_state(lockdown).await
}

#[cfg(not(target_os = "linux"))]
pub async fn lockdown_state(_lockdown: &MeshLockdown) -> Result<LockdownState, NetworkError> {
    Err(NetworkError::UnsupportedPlatform {
        target: std::env::consts::OS,
    })
}

/// Refuse a mesh pool that would shadow one of this host's routes (a VPC or
/// VPN range, the route to the control plane).
#[cfg(target_os = "linux")]
pub async fn preflight_routes(pool: Ipv4Net) -> Result<(), NetworkError> {
    crate::linux::preflight_mesh_routes(pool).await
}

#[cfg(not(target_os = "linux"))]
pub async fn preflight_routes(_pool: Ipv4Net) -> Result<(), NetworkError> {
    Err(NetworkError::UnsupportedPlatform {
        target: std::env::consts::OS,
    })
}

/// This host's mesh interface MTU: its default-route device's MTU, lowered
/// to the operator's `configured` underlay MTU if smaller, minus WireGuard's
/// overhead (see [`mesh_mtu_for`]).
pub async fn detect_mtu(configured: Option<u32>) -> Result<u32, NetworkError> {
    let device = crate::detect_underlay_device().await?;
    let detected = crate::detect_underlay_mtu(&device).await?;
    Ok(mesh_mtu_for(
        configured.map_or(detected, |configured| configured.min(detected)),
    ))
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MeshError {
    #[error("WireGuard mesh is not enabled on this cluster")]
    Disabled,
    #[error("wireguard_cidr {value:?} is invalid: {reason}")]
    InvalidCidr { value: String, reason: String },
    #[error("wireguard_cidr {mesh} overlaps the compute pool {pool}")]
    OverlapsComputePool { mesh: Ipv4Net, pool: Ipv4Net },
    #[error("wireguard_cidr {cidr} has no free address left for another node")]
    Exhausted { cidr: Ipv4Net },
    #[error("wireguard_port {0} is outside 1..=65535")]
    InvalidPort(i32),
    #[error("wireguard_port {0} is the overlay's VXLAN port; pick another (default 51820)")]
    PortClashesWithVxlan(u16),
    #[error("the mesh {setting} cannot change: {assigned} node(s) already use {current}")]
    InUse {
        setting: &'static str,
        current: String,
        assigned: u64,
    },
    #[error("WireGuard endpoint {value:?} is invalid: {reason}")]
    InvalidEndpoint { value: String, reason: String },
    #[error("WireGuard public key is not a base64 32-byte key")]
    InvalidPublicKey,
    #[error("that WireGuard public key already belongs to another node")]
    PublicKeyInUse,
    #[error("node {0} not found")]
    NodeNotFound(i32),
    #[error("the pairing is no longer waiting for a key (cancelled, expired or already paired)")]
    PairingClosed,
    #[error("{limit} node pairings are already in progress")]
    TooManyPairings { limit: usize },
    #[error("{0} is not on the WireGuard mesh yet, so it cannot be the hub")]
    NotOnMesh(String),
    #[error("stored mesh data for {what} is invalid: {reason}")]
    Corrupt { what: String, reason: String },
    #[cfg(feature = "control_plane")]
    #[error("database error: {0}")]
    Database(#[source] DatabaseError),
}

// The database exists only on the control plane: agents build this crate
// without `control_plane`, and so without sea-orm.
#[cfg(feature = "control_plane")]
impl From<sea_orm::DbErr> for MeshError {
    fn from(error: sea_orm::DbErr) -> Self {
        MeshError::Database(DatabaseError(error))
    }
}

/// A database error kept as the source of [`MeshError::Database`] (so
/// callers can still tell `RecordNotFound` from a connection failure), and
/// compared by its message so `MeshError` stays comparable in tests.
#[cfg(feature = "control_plane")]
#[derive(Debug, Error)]
#[error(transparent)]
pub struct DatabaseError(pub sea_orm::DbErr);

#[cfg(feature = "control_plane")]
impl PartialEq for DatabaseError {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_string() == other.0.to_string()
    }
}

#[cfg(feature = "control_plane")]
impl Eq for DatabaseError {}

/// Parse and check a mesh pool. It must be private IPv4 space (it becomes
/// the underlay, which the allocator requires to be private), leave room for
/// the control plane plus at least one node, and stay clear of the compute
/// pool.
pub fn parse_mesh_cidr(value: &str, compute_pool: Ipv4Net) -> Result<Ipv4Net, MeshError> {
    let cidr: Ipv4Net = value
        .trim()
        .parse()
        .map_err(|error: ipnet::AddrParseError| MeshError::InvalidCidr {
            value: value.to_string(),
            reason: error.to_string(),
        })?;
    let cidr = cidr.trunc();
    // Both ends: 10.0.0.0/7 starts private but runs into 11/8.
    if !cidr.network().is_private() || !cidr.broadcast().is_private() {
        return Err(MeshError::InvalidCidr {
            value: value.to_string(),
            reason: "must be private IPv4 space (10/8, 172.16/12 or 192.168/16)".into(),
        });
    }
    if cidr.prefix_len() > 29 {
        return Err(MeshError::InvalidCidr {
            value: value.to_string(),
            reason: "must be /29 or larger".into(),
        });
    }
    if cidr.contains(&compute_pool.network()) || compute_pool.contains(&cidr.network()) {
        return Err(MeshError::OverlapsComputePool {
            mesh: cidr,
            pool: compute_pool,
        });
    }
    Ok(cidr)
}

pub fn parse_mesh_port(value: i32) -> Result<u16, MeshError> {
    u16::try_from(value)
        .ok()
        .filter(|port| *port > 0)
        .ok_or(MeshError::InvalidPort(value))
}

/// The control plane's mesh address: the pool's first host.
pub fn control_plane_mesh_address(cidr: Ipv4Net) -> Ipv4Addr {
    cidr.hosts().next().unwrap_or_else(|| cidr.network())
}

/// `network_config.wireguard_cidr`'s default (migration
/// `m20260928_000001_wireguard_mesh`): the pool the mesh uses unless the
/// operator picks another.
pub const DEFAULT_MESH_CIDR: Ipv4Net = Ipv4Net::new_assert(Ipv4Addr::new(10, 201, 0, 0), 16);

/// The pool whose addresses are reserved for the mesh, whether or not the
/// mesh is on: the configured `wireguard_cidr`, or [`DEFAULT_MESH_CIDR`] when
/// that does not parse. Reserved even while the mesh is off because every
/// address a node registers becomes an IP SAN on a cluster-CA leaf that is
/// never revoked: a worker that registered with `10.201.0.1` before the mesh
/// existed would otherwise hold a certificate for the control plane's future
/// mesh address, or for another member's.
pub fn reserved_mesh_pool(wireguard_cidr: &str) -> Ipv4Net {
    wireguard_cidr
        .trim()
        .parse::<Ipv4Net>()
        .map(|cidr| cidr.trunc())
        .unwrap_or(DEFAULT_MESH_CIDR)
}

/// The IPv4 address a node's registered name, address or private address
/// stands for, if it is one: a bare IP, `ip:port`, `[v6]:port` or an
/// `http(s)://` URL. IPv4-mapped IPv6 addresses count as their IPv4 address,
/// since that is the address they reach. `None` for DNS names and other IPv6
/// addresses, which can never be in the (IPv4) mesh pool.
pub fn identity_ipv4(value: &str) -> Option<Ipv4Addr> {
    let value = value.trim();
    let authority = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
        .unwrap_or(value);
    let authority = authority.split('/').next().unwrap_or(authority);
    let ip: IpAddr = match authority.parse::<SocketAddr>() {
        Ok(socket) => socket.ip(),
        Err(_) => authority
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse()
            .ok()?,
    };
    match ip.to_canonical() {
        IpAddr::V4(v4) => Some(v4),
        IpAddr::V6(_) => None,
    }
}

/// Who holds a mesh identity a registration claimed; see
/// `mesh_identity_claimed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeshIdentityHolder {
    /// The control plane's mesh address.
    ControlPlane { address: Ipv4Addr },
    /// Another address of the mesh pool, reserved whether or not the mesh
    /// is on.
    Pool { pool: Ipv4Net },
    /// The mesh address of a node.
    Node { node_id: i32, node_name: String },
    /// The mesh address a pending pairing reserved.
    Pairing {
        pairing_id: i32,
        pairing_name: String,
    },
}

impl std::fmt::Display for MeshIdentityHolder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ControlPlane { address } => {
                write!(f, "the control plane's mesh address ({address})")
            }
            Self::Pool { pool } => write!(
                f,
                "inside the WireGuard mesh pool {pool}, whose addresses the control plane \
                 assigns"
            ),
            Self::Node { node_id, node_name } => {
                write!(f, "the mesh address of node '{node_name}' (id {node_id})")
            }
            Self::Pairing {
                pairing_id,
                pairing_name,
            } => write!(
                f,
                "the mesh address reserved for pairing '{pairing_name}' (id {pairing_id})"
            ),
        }
    }
}

/// A mesh identity a registration claimed, and who holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshIdentityClaim {
    /// The value the registration claimed (name, address or private
    /// address), as given.
    pub claimed: String,
    pub holder: MeshIdentityHolder,
}

/// The first of `addresses` (claimed value, its IPv4 address) that is the
/// control plane's mesh address in `pool`, or any other address of `pool`.
pub fn pool_claim(addresses: &[(String, Ipv4Addr)], pool: Ipv4Net) -> Option<MeshIdentityClaim> {
    let control_plane = control_plane_mesh_address(pool);
    addresses
        .iter()
        .find(|(_, ip)| *ip == control_plane)
        .map(|(claimed, _)| MeshIdentityClaim {
            claimed: claimed.clone(),
            holder: MeshIdentityHolder::ControlPlane {
                address: control_plane,
            },
        })
        .or_else(|| {
            addresses
                .iter()
                .find(|(_, ip)| pool.contains(ip))
                .map(|(claimed, _)| MeshIdentityClaim {
                    claimed: claimed.clone(),
                    holder: MeshIdentityHolder::Pool { pool },
                })
        })
}

/// Lowest host address that is neither the control plane's nor taken.
pub fn next_mesh_address(
    cidr: Ipv4Net,
    taken: &std::collections::HashSet<Ipv4Addr>,
) -> Result<Ipv4Addr, MeshError> {
    let control_plane = control_plane_mesh_address(cidr);
    cidr.hosts()
        .find(|address| *address != control_plane && !taken.contains(address))
        .ok_or(MeshError::Exhausted { cidr })
}

/// Cloud instance-metadata services outside link-local space (169.254/16 is
/// already rejected): Alibaba, AWS IPv6 and GCE IPv6. Every node sends
/// handshakes to a peer's endpoint, so a node must not be able to aim the
/// whole cluster at one.
const CLOUD_METADATA_ADDRESSES: [IpAddr; 3] = [
    IpAddr::V4(Ipv4Addr::new(100, 100, 100, 200)),
    IpAddr::V6(std::net::Ipv6Addr::new(0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254)),
    IpAddr::V6(std::net::Ipv6Addr::new(0xfd20, 0xce, 0, 0, 0, 0, 0, 0x254)),
];

/// Validate a node-reported WireGuard endpoint. It must be a literal
/// `ip:port` (no DNS: the kernel needs an address, and resolving a
/// node-supplied name on the control plane would let a node steer where
/// others connect through DNS) and a unicast address other nodes could dial.
pub fn parse_endpoint(value: &str) -> Result<SocketAddr, MeshError> {
    let invalid = |reason: &str| MeshError::InvalidEndpoint {
        value: value.to_string(),
        reason: reason.to_string(),
    };
    let endpoint: SocketAddr = value
        .trim()
        .parse()
        .map_err(|_| invalid("expected ip:port, e.g. 203.0.113.10:51820"))?;
    if endpoint.port() == 0 {
        return Err(invalid("port must not be 0"));
    }
    // `::ffff:127.0.0.1` is loopback too.
    let ip = endpoint.ip().to_canonical();
    let unusable = ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || match ip {
            IpAddr::V4(v4) => v4.is_broadcast() || v4.is_link_local(),
            IpAddr::V6(v6) => v6.is_unicast_link_local(),
        };
    if unusable {
        return Err(invalid(
            "must be an address other nodes can reach (not loopback, link-local, multicast or unspecified)",
        ));
    }
    if CLOUD_METADATA_ADDRESSES.contains(&ip) {
        return Err(invalid("must not be a cloud metadata service address"));
    }
    Ok(endpoint)
}

/// A node's WireGuard endpoint is dialed over the underlay, so it can never
/// be a mesh or container address: that would route handshakes into the
/// tunnel, or at another node's workloads.
pub fn check_endpoint_outside_pools(
    endpoint: SocketAddr,
    mesh: Ipv4Net,
    compute_pool: Option<Ipv4Net>,
) -> Result<(), MeshError> {
    let IpAddr::V4(ip) = endpoint.ip() else {
        return Ok(());
    };
    if mesh.contains(&ip) || compute_pool.is_some_and(|pool| pool.contains(&ip)) {
        return Err(MeshError::InvalidEndpoint {
            value: endpoint.to_string(),
            reason: "must be the node's underlay address, not a mesh or container address".into(),
        });
    }
    Ok(())
}

/// A node's default endpoint: the address it registered with, on the mesh
/// port. That is the address the control plane already reaches it on.
pub fn default_endpoint(registered_address: &str, port: u16) -> Result<SocketAddr, MeshError> {
    let host = registered_address.trim();
    // Registered addresses may carry a port ("10.0.0.5:8443", "[fd00::1]:8443").
    let ip: IpAddr = match host.parse::<SocketAddr>() {
        Ok(socket) => socket.ip(),
        Err(_) => host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse()
            .map_err(|_| MeshError::InvalidEndpoint {
                value: host.to_string(),
                reason: "the node's registered address is not an IP; set --wg-endpoint".into(),
            })?,
    };
    parse_endpoint(&SocketAddr::new(ip, port).to_string())
}

#[cfg(feature = "control_plane")]
pub use db::*;

#[cfg(feature = "control_plane")]
mod db {
    use super::*;
    use std::sync::Arc;

    use sea_orm::{
        ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait,
        QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
    };
    use temps_entities::{network_config, nodes};

    /// The `network_config` singleton.
    pub(crate) async fn load_config<C: sea_orm::ConnectionTrait>(
        db: &C,
    ) -> Result<network_config::Model, MeshError> {
        network_config::Entity::find_by_id(1)
            .one(db)
            .await?
            .ok_or_else(missing_config)
    }

    /// The `network_config` singleton, locked for the rest of `txn`: the
    /// lock serializes address assignment and pairing changes.
    pub(crate) async fn lock_config<C: sea_orm::ConnectionTrait>(
        txn: &C,
    ) -> Result<network_config::Model, MeshError> {
        network_config::Entity::find_by_id(1)
            .lock_exclusive()
            .one(txn)
            .await?
            .ok_or_else(missing_config)
    }

    fn missing_config() -> MeshError {
        MeshError::Corrupt {
            what: "network_config".into(),
            reason: "singleton row missing".into(),
        }
    }

    /// Mesh settings as stored in `network_config`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct MeshSettings {
        pub cidr: Ipv4Net,
        pub port: u16,
        /// TCP port of the node API on the control plane's mesh address
        /// (ADR 048 D3); the mesh port number unless configured.
        pub node_api_port: u16,
    }

    impl MeshSettings {
        pub fn control_plane_address(&self) -> Ipv4Addr {
            control_plane_mesh_address(self.cidr)
        }
    }

    pub(crate) fn settings_from(
        cfg: &network_config::Model,
    ) -> Result<Option<MeshSettings>, MeshError> {
        if !cfg.wireguard_enabled {
            return Ok(None);
        }
        let pool: Ipv4Net =
            cfg.compute_pool_cidr
                .parse()
                .map_err(|error: ipnet::AddrParseError| MeshError::Corrupt {
                    what: "compute_pool_cidr".into(),
                    reason: error.to_string(),
                })?;
        let port = parse_mesh_port(cfg.wireguard_port)?;
        Ok(Some(MeshSettings {
            cidr: parse_mesh_cidr(&cfg.wireguard_cidr, pool)?,
            port,
            node_api_port: cfg
                .node_api_port
                .map(parse_mesh_port)
                .transpose()?
                .unwrap_or(port),
        }))
    }

    /// `None` when the mesh is off.
    pub async fn load_settings(db: &DatabaseConnection) -> Result<Option<MeshSettings>, MeshError> {
        let cfg = load_config(db).await?;
        settings_from(&cfg)
    }

    /// The settings workers act on: `None` until the control plane has
    /// brought up its end and published its key, so an `enable` whose
    /// control-plane setup then failed never moves workers onto a mesh whose
    /// control plane is not on it. The control plane's endpoint is optional:
    /// one that nobody can dial publishes none and dials its nodes instead.
    pub async fn settings_for_workers(
        db: &DatabaseConnection,
    ) -> Result<Option<MeshSettings>, MeshError> {
        let cfg = load_config(db).await?;
        if cfg.control_plane_wg_public_key.is_none() {
            return Ok(None);
        }
        settings_from(&cfg)
    }

    /// Turn the mesh on (idempotent). The pool can only change while no node
    /// holds a mesh address, because addresses are already in use as
    /// underlays.
    pub async fn enable(
        db: &DatabaseConnection,
        cidr: Option<&str>,
        port: Option<u16>,
        node_api_port: Option<u16>,
    ) -> Result<MeshSettings, MeshError> {
        let txn = db.begin().await?;
        let cfg = lock_config(&txn).await?;
        let pool: Ipv4Net =
            cfg.compute_pool_cidr
                .parse()
                .map_err(|error: ipnet::AddrParseError| MeshError::Corrupt {
                    what: "compute_pool_cidr".into(),
                    reason: error.to_string(),
                })?;
        let requested = parse_mesh_cidr(cidr.unwrap_or(&cfg.wireguard_cidr), pool)?;
        let current_port = parse_mesh_port(cfg.wireguard_port)?;
        let requested_port = port.unwrap_or(current_port);
        if i32::from(requested_port) == cfg.vxlan_port {
            return Err(MeshError::PortClashesWithVxlan(requested_port));
        }
        let current = parse_mesh_cidr(&cfg.wireguard_cidr, pool)?;
        // Addresses are in use as underlays and every endpoint (the control
        // plane's, --wg-endpoint values) carries the port: both are frozen
        // once any node is on the mesh.
        if requested != current || requested_port != current_port {
            // A pending pairing holds an address from the current pool and
            // its code carries the pool and port, so it pins them as firmly
            // as a node on the mesh does.
            let assigned = nodes::Entity::find()
                .filter(nodes::Column::MeshWgAddress.is_not_null())
                .count(&txn)
                .await?
                + crate::pairing::pending_count(&txn).await?;
            if assigned > 0 {
                return Err(if requested != current {
                    MeshError::InUse {
                        setting: "pool",
                        current: current.to_string(),
                        assigned,
                    }
                } else {
                    MeshError::InUse {
                        setting: "port",
                        current: current_port.to_string(),
                        assigned,
                    }
                });
            }
        }
        check_nodes_outside_pool(&txn, requested).await?;
        let node_api_port = match node_api_port {
            Some(0) => return Err(MeshError::InvalidPort(0)),
            Some(port) => Some(port),
            None => cfg.node_api_port.map(parse_mesh_port).transpose()?,
        };
        let mut active: network_config::ActiveModel = cfg.into();
        active.wireguard_enabled = Set(true);
        active.wireguard_cidr = Set(requested.to_string());
        active.wireguard_port = Set(i32::from(requested_port));
        active.node_api_port = Set(node_api_port.map(i32::from));
        active.updated_at = Set(chrono::Utc::now());
        active.update(&txn).await?;
        txn.commit().await?;
        Ok(MeshSettings {
            cidr: requested,
            port: requested_port,
            node_api_port: node_api_port.unwrap_or(requested_port),
        })
    }

    /// Refuse a mesh pool that contains an address a node registered as its
    /// own name, address or private address.
    ///
    /// The cluster CA signed that node a leaf for the address, and leaves are
    /// not revoked: it could present it as the control plane's mesh address
    /// to nodes that verify the control plane by address (agents enrolled
    /// before the reserved control-plane name existed), or as whichever
    /// member gets the address. Registration has refused addresses in the
    /// pool since the pool was reserved while the mesh is off; this catches
    /// nodes registered before that, and pools moved onto existing nodes.
    async fn check_nodes_outside_pool<C: sea_orm::ConnectionTrait>(
        txn: &C,
        pool: Ipv4Net,
    ) -> Result<(), MeshError> {
        let control_plane = control_plane_mesh_address(pool);
        for node in nodes::Entity::find()
            .order_by_asc(nodes::Column::Id)
            .all(txn)
            .await?
        {
            for (field, value) in [
                ("name", node.name.as_str()),
                ("address", node.address.as_str()),
                ("private address", node.private_address.as_str()),
            ] {
                let Some(ip) = identity_ipv4(value) else {
                    continue;
                };
                if !pool.contains(&ip) {
                    continue;
                }
                let role = if ip == control_plane {
                    "the control plane's mesh address in this pool"
                } else {
                    "inside this pool"
                };
                tracing::warn!(
                    node_id = node.id,
                    node_name = %node.name,
                    field,
                    value,
                    pool = %pool,
                    "refusing the WireGuard mesh pool: a node registered an address inside it"
                );
                return Err(MeshError::InvalidCidr {
                    value: pool.to_string(),
                    reason: format!(
                        "node '{name}' (id {id}) registered its {field} as {value}, {role}. The \
                         cluster CA signed that node a certificate for {ip}, so it could \
                         impersonate whichever mesh member holds that address. Remove the node \
                         and re-join it with its own underlay address, or choose a pool that \
                         does not contain {ip}",
                        name = node.name,
                        id = node.id,
                    ),
                });
            }
        }
        Ok(())
    }

    /// The first of `claimed` (a registering node's name and addresses) that
    /// is a mesh identity, whether or not the mesh is on: the control plane's
    /// mesh address, any address of the mesh pool, another node's mesh
    /// address or a pending pairing's. Each becomes an IP SAN on the leaf the
    /// cluster CA signs, so a node holding one could impersonate its owner.
    ///
    /// Every mesh address is inside the pool (the pool cannot move while one
    /// is assigned or reserved), so the pool check alone covers them; the
    /// node and pairing lookups keep that true for rows written before the
    /// pool was frozen for pending pairings, and name the holder.
    pub async fn mesh_identity_claimed(
        db: &DatabaseConnection,
        claimed: &[String],
    ) -> Result<Option<MeshIdentityClaim>, MeshError> {
        let addresses: Vec<(String, Ipv4Addr)> = claimed
            .iter()
            .filter_map(|value| Some((value.clone(), identity_ipv4(value)?)))
            .collect();
        if addresses.is_empty() {
            return Ok(None);
        }
        let cfg = load_config(db).await?;
        if let Some(claim) = pool_claim(&addresses, reserved_mesh_pool(&cfg.wireguard_cidr)) {
            return Ok(Some(claim));
        }
        let texts: Vec<String> = addresses.iter().map(|(_, ip)| ip.to_string()).collect();
        let claimed_text = |address: &str| {
            addresses
                .iter()
                .find(|(_, ip)| ip.to_string() == address)
                .map(|(value, _)| value.clone())
                .unwrap_or_else(|| address.to_string())
        };
        if let Some(node) = nodes::Entity::find()
            .filter(nodes::Column::MeshWgAddress.is_in(texts.clone()))
            .order_by_asc(nodes::Column::Id)
            .one(db)
            .await?
        {
            return Ok(Some(MeshIdentityClaim {
                claimed: claimed_text(node.mesh_wg_address.as_deref().unwrap_or_default()),
                holder: MeshIdentityHolder::Node {
                    node_id: node.id,
                    node_name: node.name,
                },
            }));
        }
        if let Some(pairing) = temps_entities::node_pairings::Entity::find()
            .filter(temps_entities::node_pairings::Column::MeshAddress.is_in(texts))
            .filter(temps_entities::node_pairings::Column::Status.is_in(crate::pairing::PENDING))
            .order_by_asc(temps_entities::node_pairings::Column::Id)
            .one(db)
            .await?
        {
            return Ok(Some(MeshIdentityClaim {
                claimed: claimed_text(&pairing.mesh_address),
                holder: MeshIdentityHolder::Pairing {
                    pairing_id: pairing.id,
                    pairing_name: pairing.name,
                },
            }));
        }
        Ok(None)
    }

    /// Record the control plane's public key, and the endpoint workers dial
    /// if it has one, so workers can peer with it.
    pub async fn publish_control_plane(
        db: &DatabaseConnection,
        public_key: &str,
        endpoint: Option<SocketAddr>,
    ) -> Result<(), MeshError> {
        let cfg = load_config(db).await?;
        let endpoint = endpoint.map(|endpoint| endpoint.to_string());
        if cfg.control_plane_wg_public_key.as_deref() == Some(public_key)
            && cfg.control_plane_wg_endpoint == endpoint
        {
            return Ok(());
        }
        let mut active: network_config::ActiveModel = cfg.into();
        active.control_plane_wg_public_key = Set(Some(public_key.to_string()));
        active.control_plane_wg_endpoint = Set(endpoint);
        active.updated_at = Set(chrono::Utc::now());
        active.update(db).await?;
        Ok(())
    }

    /// The control plane's end of the mesh as workers see it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct PublishedControlPlane {
        pub public_key: String,
        /// `None` when nodes cannot dial the control plane; it dials them.
        pub endpoint: Option<String>,
    }

    /// `None` until `temps serve` has brought its end up and published it;
    /// workers only move onto the mesh after that.
    pub async fn published_control_plane(
        db: &DatabaseConnection,
    ) -> Result<Option<PublishedControlPlane>, MeshError> {
        let cfg = load_config(db).await?;
        Ok(cfg
            .control_plane_wg_public_key
            .map(|public_key| PublishedControlPlane {
                public_key,
                endpoint: cfg.control_plane_wg_endpoint,
            }))
    }

    /// The port the mesh listens on, or would once enabled.
    pub async fn configured_port(db: &DatabaseConnection) -> Result<u16, MeshError> {
        let cfg = load_config(db).await?;
        parse_mesh_port(cfg.wireguard_port)
    }

    /// What a node needs to bring its end of the mesh up.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct NodeMeshRegistration {
        pub address: Ipv4Addr,
        pub prefix_len: u8,
        pub listen_port: u16,
    }

    /// Store a node's mesh public key and endpoint, assign its mesh address
    /// on first registration, and make that address its underlay.
    ///
    /// Serialized on the `network_config` row lock so two nodes registering
    /// at once never receive the same address (the unique index is the
    /// backstop).
    pub async fn register_node(
        db: &Arc<DatabaseConnection>,
        node_id: i32,
        public_key: &str,
        endpoint: SocketAddr,
    ) -> Result<NodeMeshRegistration, MeshError> {
        let txn = db.begin().await?;
        let registration = register_node_in(&txn, node_id, public_key, endpoint).await?;
        txn.commit().await?;
        Ok(registration)
    }

    /// [`register_node`] inside the caller's transaction, so a pairing can be
    /// linked and its node registered atomically.
    pub(crate) async fn register_node_in<C: sea_orm::ConnectionTrait>(
        txn: &C,
        node_id: i32,
        public_key: &str,
        endpoint: SocketAddr,
    ) -> Result<NodeMeshRegistration, MeshError> {
        if !temps_wireguard::mesh::is_valid_public_key(public_key) {
            return Err(MeshError::InvalidPublicKey);
        }
        let cfg = lock_config(txn).await?;
        let settings = settings_from(&cfg)?.ok_or(MeshError::Disabled)?;
        check_endpoint_outside_pools(
            endpoint,
            settings.cidr,
            cfg.compute_pool_cidr.parse::<Ipv4Net>().ok(),
        )?;
        let node = nodes::Entity::find_by_id(node_id)
            .one(txn)
            .await?
            .ok_or(MeshError::NodeNotFound(node_id))?;

        let key_owner = nodes::Entity::find()
            .filter(nodes::Column::MeshWgPublicKey.eq(public_key))
            .filter(nodes::Column::Id.ne(node_id))
            .one(txn)
            .await?;
        // A pending pairing holds its key too (record_key refuses a key a
        // node holds; this is the other direction), unless it is this node's
        // own pairing.
        let key_pending = crate::pairing::held_by_other_pairing(txn, public_key, node_id).await?;
        if key_owner.is_some()
            || key_pending
            || cfg.control_plane_wg_public_key.as_deref() == Some(public_key)
        {
            return Err(MeshError::PublicKeyInUse);
        }

        let current = node
            .mesh_wg_address
            .as_deref()
            .map(str::parse::<Ipv4Addr>)
            .transpose()
            .map_err(|error| MeshError::Corrupt {
                what: format!("node {node_id} mesh_wg_address"),
                reason: error.to_string(),
            })?
            .filter(|address| {
                settings.cidr.contains(address) && *address != settings.control_plane_address()
            });
        // A node that registered with a pairing's token takes the address
        // the pairing reserved for this key (ADR 048 D2b).
        let paired = crate::pairing::adopt_for_node(txn, node_id, public_key).await?;
        let address = match (current, paired) {
            (Some(address), _) => address,
            (None, Some(address)) => address,
            (None, None) => {
                next_mesh_address(settings.cidr, &crate::pairing::taken_addresses(txn).await?)?
            }
        };

        let address_text = address.to_string();
        let endpoint_text = endpoint.to_string();
        let unchanged = node.mesh_wg_public_key.as_deref() == Some(public_key)
            && node.mesh_wg_endpoint.as_deref() == Some(endpoint_text.as_str())
            && node.mesh_wg_address.as_deref() == Some(address_text.as_str())
            && node.underlay_address.as_deref() == Some(address_text.as_str());
        if !unchanged {
            let mut active: nodes::ActiveModel = node.into();
            active.mesh_wg_public_key = Set(Some(public_key.to_string()));
            active.mesh_wg_endpoint = Set(Some(endpoint_text));
            active.mesh_wg_address = Set(Some(address_text.clone()));
            active.underlay_address = Set(Some(address_text));
            active.update(txn).await?;
        }
        Ok(NodeMeshRegistration {
            address,
            prefix_len: settings.cidr.prefix_len(),
            listen_port: settings.port,
        })
    }

    /// Every mesh member except `excluding_node`: the control plane (once it
    /// published its key) and each node that registered a key. Nodes are
    /// listed whether or not they are healthy — a peer that is down simply
    /// has no handshake — but a node removed from the cluster disappears,
    /// which revokes its access on every other node's next reconcile.
    ///
    /// Members whose pair with the caller goes through the hub (ADR 048 D4)
    /// are not listed; their addresses ride on the hub's entry instead.
    pub async fn peers(
        db: &DatabaseConnection,
        excluding_node: Option<i32>,
    ) -> Result<Vec<NamedMeshPeer>, MeshError> {
        let cfg = load_config(db).await?;
        let Some(settings) = settings_from(&cfg)? else {
            return Ok(Vec::new());
        };
        let mut peers = Vec::new();
        let mut me = cfg.control_plane_wg_public_key.clone();
        let hub = crate::mesh_links::hub_from(&cfg);
        let mut hub_key = (hub == Some(crate::mesh_links::Hub::ControlPlane))
            .then(|| cfg.control_plane_wg_public_key.clone())
            .flatten();
        if let Some(public_key) = cfg.control_plane_wg_public_key.as_deref() {
            if excluding_node.is_some() {
                peers.push(NamedMeshPeer {
                    name: "control-plane".to_string(),
                    peer: MeshPeer {
                        public_key: public_key.to_string(),
                        endpoint: cfg
                            .control_plane_wg_endpoint
                            .as_deref()
                            .and_then(|endpoint| endpoint.parse().ok()),
                        address: settings.control_plane_address(),
                        relayed: Vec::new(),
                    },
                });
            }
        }
        let query = nodes::Entity::find()
            .filter(nodes::Column::MeshWgPublicKey.is_not_null())
            .filter(nodes::Column::MeshWgAddress.is_not_null())
            .order_by_asc(nodes::Column::Id);
        if excluding_node.is_none() {
            // Nodes being paired: the control plane dials them so they can
            // register over the mesh (ADR 048 D2b). Workers never see them.
            for pairing in crate::pairing::peering(db).await? {
                let (Some(public_key), Ok(address)) =
                    (pairing.public_key, pairing.mesh_address.parse())
                else {
                    continue;
                };
                peers.push(NamedMeshPeer {
                    name: format!("pairing:{}", pairing.name),
                    peer: MeshPeer {
                        public_key,
                        endpoint: pairing.node_endpoint.parse().ok(),
                        address,
                        relayed: Vec::new(),
                    },
                });
            }
        }
        for row in query.all(db).await? {
            let (Some(public_key), Some(address)) = (row.mesh_wg_public_key, row.mesh_wg_address)
            else {
                continue;
            };
            if hub == Some(crate::mesh_links::Hub::Node(row.id)) {
                hub_key = Some(public_key.clone());
            }
            if excluding_node == Some(row.id) {
                me = Some(public_key);
                continue;
            }
            let Ok(address) = address.parse() else {
                tracing::warn!(node = %row.name, "skipping mesh peer with an invalid address");
                continue;
            };
            peers.push(NamedMeshPeer {
                name: row.name,
                peer: MeshPeer {
                    public_key,
                    endpoint: row.mesh_wg_endpoint.and_then(|value| value.parse().ok()),
                    address,
                    relayed: Vec::new(),
                },
            });
        }
        let Some(me) = me else {
            return Ok(peers);
        };
        // Only this member's pairs decide its peers.
        let links = crate::mesh_links::load_links_of(db, &me).await?;
        let keyed = peers
            .into_iter()
            .map(|named| (named.peer.public_key.clone(), named))
            .collect();
        Ok(crate::mesh_links::route(
            &me,
            keyed,
            &links,
            hub_key.as_deref(),
        ))
    }

    /// A mesh peer with the node name it belongs to (for status output).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct NamedMeshPeer {
        pub name: String,
        pub peer: MeshPeer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn pool() -> Ipv4Net {
        "172.20.0.0/16".parse().unwrap()
    }

    #[test]
    fn mesh_pool_must_be_private_sized_and_clear_of_the_compute_pool() {
        assert_eq!(
            parse_mesh_cidr("10.201.0.0/16", pool()).unwrap(),
            "10.201.0.0/16".parse::<Ipv4Net>().unwrap()
        );
        assert!(matches!(
            parse_mesh_cidr("100.64.0.0/16", pool()),
            Err(MeshError::InvalidCidr { .. })
        ));
        for straddling in ["10.0.0.0/7", "192.168.0.0/15", "172.16.0.0/11"] {
            assert!(
                matches!(
                    parse_mesh_cidr(straddling, pool()),
                    Err(MeshError::InvalidCidr { .. })
                ),
                "{straddling} runs past private space"
            );
        }
        assert!(matches!(
            parse_mesh_cidr("10.201.0.0/30", pool()),
            Err(MeshError::InvalidCidr { .. })
        ));
        assert!(matches!(
            parse_mesh_cidr("172.20.8.0/24", pool()),
            Err(MeshError::OverlapsComputePool { .. })
        ));
        assert!(matches!(
            parse_mesh_cidr("172.16.0.0/12", pool()),
            Err(MeshError::OverlapsComputePool { .. })
        ));
    }

    #[test]
    fn a_node_endpoint_must_sit_outside_the_mesh_and_container_pools() {
        let mesh: Ipv4Net = "10.201.0.0/24".parse().unwrap();
        let check = |endpoint: &str| {
            check_endpoint_outside_pools(endpoint.parse().unwrap(), mesh, Some(pool()))
        };
        assert!(check("203.0.113.10:51820").is_ok());
        assert!(check("10.62.0.21:51820").is_ok());
        assert!(matches!(
            check("10.201.0.3:51820"),
            Err(MeshError::InvalidEndpoint { .. })
        ));
        assert!(matches!(
            check("172.20.4.7:51820"),
            Err(MeshError::InvalidEndpoint { .. })
        ));
    }

    #[test]
    fn control_plane_takes_the_first_host_and_nodes_the_lowest_free() {
        let cidr: Ipv4Net = "10.201.0.0/29".parse().unwrap();
        assert_eq!(
            control_plane_mesh_address(cidr),
            Ipv4Addr::new(10, 201, 0, 1)
        );

        let mut taken = HashSet::new();
        let first = next_mesh_address(cidr, &taken).unwrap();
        assert_eq!(first, Ipv4Addr::new(10, 201, 0, 2));
        taken.insert(first);
        taken.insert(Ipv4Addr::new(10, 201, 0, 4));
        assert_eq!(
            next_mesh_address(cidr, &taken).unwrap(),
            Ipv4Addr::new(10, 201, 0, 3)
        );

        // /29: hosts .1-.6, .1 is the control plane.
        let full: HashSet<_> = (2..=6)
            .map(|last| Ipv4Addr::new(10, 201, 0, last))
            .collect();
        assert_eq!(
            next_mesh_address(cidr, &full),
            Err(MeshError::Exhausted { cidr })
        );
    }

    #[test]
    fn identities_are_read_as_ipv4_addresses_in_every_registered_form() {
        let ip = Ipv4Addr::new(10, 201, 0, 1);
        for value in [
            "10.201.0.1",
            " 10.201.0.1 ",
            "10.201.0.1:3100",
            "https://10.201.0.1:3100",
            "http://10.201.0.1/api",
            "[::ffff:10.201.0.1]:3100",
            "::ffff:10.201.0.1",
        ] {
            assert_eq!(identity_ipv4(value), Some(ip), "{value}");
        }
        for value in [
            "worker-1",
            "fd00::1",
            "[fd00::1]:3100",
            "",
            "https://node.example.com",
        ] {
            assert_eq!(identity_ipv4(value), None, "{value}");
        }
    }

    #[test]
    fn the_mesh_pool_is_reserved_even_when_unset_or_unreadable() {
        assert_eq!(
            reserved_mesh_pool("10.202.0.0/24"),
            "10.202.0.0/24".parse::<Ipv4Net>().unwrap()
        );
        assert_eq!(
            reserved_mesh_pool("10.202.0.9/24"),
            "10.202.0.0/24".parse::<Ipv4Net>().unwrap()
        );
        assert_eq!(reserved_mesh_pool("garbage"), DEFAULT_MESH_CIDR);
        assert_eq!(DEFAULT_MESH_CIDR.to_string(), "10.201.0.0/16");
    }

    #[test]
    fn a_claim_on_the_control_plane_address_wins_over_one_inside_the_pool() {
        let pool: Ipv4Net = "10.201.0.0/16".parse().unwrap();
        let claimed = |values: &[&str]| -> Vec<(String, Ipv4Addr)> {
            values
                .iter()
                .filter_map(|value| Some((value.to_string(), identity_ipv4(value)?)))
                .collect()
        };
        assert_eq!(
            pool_claim(&claimed(&["10.201.3.4", "https://10.201.0.1:3100"]), pool),
            Some(MeshIdentityClaim {
                claimed: "https://10.201.0.1:3100".into(),
                holder: MeshIdentityHolder::ControlPlane {
                    address: Ipv4Addr::new(10, 201, 0, 1)
                },
            })
        );
        assert_eq!(
            pool_claim(&claimed(&["worker-1", "10.201.3.4"]), pool),
            Some(MeshIdentityClaim {
                claimed: "10.201.3.4".into(),
                holder: MeshIdentityHolder::Pool { pool },
            })
        );
        assert_eq!(
            pool_claim(&claimed(&["worker-1", "10.100.0.2", "10.202.0.1"]), pool),
            None
        );
        assert!(MeshIdentityHolder::Pool { pool }
            .to_string()
            .contains("10.201.0.0/16"));
    }

    #[test]
    fn endpoints_must_be_dialable_literal_addresses() {
        assert_eq!(
            parse_endpoint("203.0.113.10:51820").unwrap(),
            "203.0.113.10:51820".parse().unwrap()
        );
        assert!(parse_endpoint("[2001:db8::1]:51820").is_ok());
        for bad in [
            "node-a.example.com:51820",
            "203.0.113.10",
            "127.0.0.1:51820",
            "0.0.0.0:51820",
            "169.254.1.1:51820",
            "224.0.0.1:51820",
            "203.0.113.10:0",
            "100.100.100.200:51820",
            "[fd00:ec2::254]:51820",
            "[fd20:ce::254]:51820",
            "[::ffff:127.0.0.1]:51820",
            "[::ffff:169.254.169.254]:51820",
        ] {
            assert!(parse_endpoint(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn default_endpoint_reuses_the_registered_address() {
        assert_eq!(
            default_endpoint("203.0.113.10", 51820).unwrap(),
            "203.0.113.10:51820".parse().unwrap()
        );
        assert_eq!(
            default_endpoint("10.0.0.5:8443", 51820).unwrap(),
            "10.0.0.5:51820".parse().unwrap()
        );
        assert_eq!(
            default_endpoint("[fd00::5]:8443", 51820).unwrap(),
            "[fd00::5]:51820".parse().unwrap()
        );
        assert!(default_endpoint("node-a.internal", 51820).is_err());
    }
}
