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

pub use temps_wireguard::mesh::{key_dir, MeshPeer, MESH_INTERFACE};

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
    #[error("WireGuard endpoint {value:?} is invalid: {reason}")]
    InvalidEndpoint { value: String, reason: String },
    #[error("WireGuard public key is not a base64 32-byte key")]
    InvalidPublicKey,
    #[error("that WireGuard public key already belongs to another node")]
    PublicKeyInUse,
    #[error("node {0} not found")]
    NodeNotFound(i32),
    #[error("stored mesh data for {what} is invalid: {reason}")]
    Corrupt { what: String, reason: String },
    #[error("database error: {0}")]
    Database(String),
}

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
    if !cidr.network().is_private() {
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
    let ip = endpoint.ip();
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
    Ok(endpoint)
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
    use std::collections::HashSet;
    use std::sync::Arc;

    use sea_orm::{
        ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait,
        QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
    };
    use temps_entities::{network_config, nodes};

    impl From<sea_orm::DbErr> for MeshError {
        fn from(error: sea_orm::DbErr) -> Self {
            MeshError::Database(error.to_string())
        }
    }

    /// Mesh settings as stored in `network_config`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct MeshSettings {
        pub cidr: Ipv4Net,
        pub port: u16,
    }

    impl MeshSettings {
        pub fn control_plane_address(&self) -> Ipv4Addr {
            control_plane_mesh_address(self.cidr)
        }
    }

    fn settings_from(cfg: &network_config::Model) -> Result<Option<MeshSettings>, MeshError> {
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
        Ok(Some(MeshSettings {
            cidr: parse_mesh_cidr(&cfg.wireguard_cidr, pool)?,
            port: parse_mesh_port(cfg.wireguard_port)?,
        }))
    }

    /// `None` when the mesh is off.
    pub async fn load_settings(db: &DatabaseConnection) -> Result<Option<MeshSettings>, MeshError> {
        let cfg = network_config::Entity::find_by_id(1)
            .one(db)
            .await?
            .ok_or_else(|| MeshError::Corrupt {
                what: "network_config".into(),
                reason: "singleton row missing".into(),
            })?;
        settings_from(&cfg)
    }

    /// Turn the mesh on (idempotent). The pool can only change while no node
    /// holds a mesh address, because addresses are already in use as
    /// underlays.
    pub async fn enable(
        db: &DatabaseConnection,
        cidr: Option<&str>,
        port: Option<u16>,
    ) -> Result<MeshSettings, MeshError> {
        let txn = db.begin().await?;
        let cfg = network_config::Entity::find_by_id(1)
            .lock_exclusive()
            .one(&txn)
            .await?
            .ok_or_else(|| MeshError::Corrupt {
                what: "network_config".into(),
                reason: "singleton row missing".into(),
            })?;
        let pool: Ipv4Net =
            cfg.compute_pool_cidr
                .parse()
                .map_err(|error: ipnet::AddrParseError| MeshError::Corrupt {
                    what: "compute_pool_cidr".into(),
                    reason: error.to_string(),
                })?;
        let requested = parse_mesh_cidr(cidr.unwrap_or(&cfg.wireguard_cidr), pool)?;
        let requested_port = port.unwrap_or(parse_mesh_port(cfg.wireguard_port)?);
        let current = parse_mesh_cidr(&cfg.wireguard_cidr, pool)?;
        if requested != current {
            let assigned = nodes::Entity::find()
                .filter(nodes::Column::MeshWgAddress.is_not_null())
                .count(&txn)
                .await?;
            if assigned > 0 {
                return Err(MeshError::InvalidCidr {
                    value: requested.to_string(),
                    reason: format!(
                        "{assigned} node(s) already hold addresses in {current}; \
                         the mesh pool cannot change once nodes use it"
                    ),
                });
            }
        }
        let mut active: network_config::ActiveModel = cfg.into();
        active.wireguard_enabled = Set(true);
        active.wireguard_cidr = Set(requested.to_string());
        active.wireguard_port = Set(i32::from(requested_port));
        active.updated_at = Set(chrono::Utc::now());
        active.update(&txn).await?;
        txn.commit().await?;
        Ok(MeshSettings {
            cidr: requested,
            port: requested_port,
        })
    }

    /// Record the control plane's public key and endpoint so workers can
    /// peer with it.
    pub async fn publish_control_plane(
        db: &DatabaseConnection,
        public_key: &str,
        endpoint: SocketAddr,
    ) -> Result<(), MeshError> {
        let cfg = network_config::Entity::find_by_id(1)
            .one(db)
            .await?
            .ok_or_else(|| MeshError::Corrupt {
                what: "network_config".into(),
                reason: "singleton row missing".into(),
            })?;
        let endpoint = endpoint.to_string();
        if cfg.control_plane_wg_public_key.as_deref() == Some(public_key)
            && cfg.control_plane_wg_endpoint.as_deref() == Some(endpoint.as_str())
        {
            return Ok(());
        }
        let mut active: network_config::ActiveModel = cfg.into();
        active.control_plane_wg_public_key = Set(Some(public_key.to_string()));
        active.control_plane_wg_endpoint = Set(Some(endpoint));
        active.updated_at = Set(chrono::Utc::now());
        active.update(db).await?;
        Ok(())
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
        if !temps_wireguard::mesh::is_valid_public_key(public_key) {
            return Err(MeshError::InvalidPublicKey);
        }
        let txn = db.begin().await?;
        let cfg = network_config::Entity::find_by_id(1)
            .lock_exclusive()
            .one(&txn)
            .await?
            .ok_or_else(|| MeshError::Corrupt {
                what: "network_config".into(),
                reason: "singleton row missing".into(),
            })?;
        let settings = settings_from(&cfg)?.ok_or(MeshError::Disabled)?;
        let node = nodes::Entity::find_by_id(node_id)
            .one(&txn)
            .await?
            .ok_or(MeshError::NodeNotFound(node_id))?;

        let key_owner = nodes::Entity::find()
            .filter(nodes::Column::MeshWgPublicKey.eq(public_key))
            .filter(nodes::Column::Id.ne(node_id))
            .one(&txn)
            .await?;
        if key_owner.is_some() || cfg.control_plane_wg_public_key.as_deref() == Some(public_key) {
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
        let address = match current {
            Some(address) => address,
            None => {
                let taken: HashSet<Ipv4Addr> = nodes::Entity::find()
                    .filter(nodes::Column::MeshWgAddress.is_not_null())
                    .all(&txn)
                    .await?
                    .into_iter()
                    .filter_map(|row| row.mesh_wg_address?.parse().ok())
                    .collect();
                next_mesh_address(settings.cidr, &taken)?
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
            active.update(&txn).await?;
        }
        txn.commit().await?;
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
    pub async fn peers(
        db: &DatabaseConnection,
        excluding_node: Option<i32>,
    ) -> Result<Vec<NamedMeshPeer>, MeshError> {
        let cfg = network_config::Entity::find_by_id(1)
            .one(db)
            .await?
            .ok_or_else(|| MeshError::Corrupt {
                what: "network_config".into(),
                reason: "singleton row missing".into(),
            })?;
        let Some(settings) = settings_from(&cfg)? else {
            return Ok(Vec::new());
        };
        let mut peers = Vec::new();
        if let (Some(public_key), Some(endpoint)) = (
            cfg.control_plane_wg_public_key.as_deref(),
            cfg.control_plane_wg_endpoint.as_deref(),
        ) {
            if excluding_node.is_some() {
                peers.push(NamedMeshPeer {
                    name: "control-plane".to_string(),
                    peer: MeshPeer {
                        public_key: public_key.to_string(),
                        endpoint: endpoint.parse().ok(),
                        address: settings.control_plane_address(),
                    },
                });
            }
        }
        let mut query = nodes::Entity::find()
            .filter(nodes::Column::MeshWgPublicKey.is_not_null())
            .filter(nodes::Column::MeshWgAddress.is_not_null())
            .order_by_asc(nodes::Column::Id);
        if let Some(node_id) = excluding_node {
            query = query.filter(nodes::Column::Id.ne(node_id));
        }
        for row in query.all(db).await? {
            let (Some(public_key), Some(address)) = (row.mesh_wg_public_key, row.mesh_wg_address)
            else {
                continue;
            };
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
                },
            });
        }
        Ok(peers)
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
