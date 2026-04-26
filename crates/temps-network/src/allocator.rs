//! Control-plane allocator for compute CIDRs and the per-node `Peer` list.
//!
//! Carved out of [`temps_entities::network_config`] + `nodes`:
//!
//! - **Pool**: read once from the singleton `network_config` row
//!   (`compute_pool_cidr`, `subnet_prefix_len`).
//! - **Allocation**: pick the lowest-numbered subnet of the configured
//!   prefix size that no other node already owns, write it to the
//!   `nodes.compute_cidr` column inside a transaction. The partial-unique
//!   index installed by `m20260427_000001_add_compute_network` guarantees
//!   no two nodes ever share a CIDR even under concurrent allocation.
//! - **Peer list**: simple `SELECT … FROM nodes WHERE compute_cidr IS NOT
//!   NULL AND id <> $caller`, mapped to [`Peer`].
//!
//! All public methods return typed errors carrying enough context (node
//! id, CIDR, pool size, exhaustion reason) to debug a misallocation from
//! a single log line.

use crate::config::Peer;
use async_trait::async_trait;
use ipnet::Ipv4Net;
use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ColumnTrait, DatabaseConnection, DbErr, EntityTrait,
    QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use temps_entities::{network_config as nc, nodes};
use thiserror::Error;
use tracing::info;
use uuid::Uuid;

/// Errors returned by the compute-network allocator.
#[derive(Debug, Error)]
pub enum AllocatorError {
    /// Node referenced by id does not exist in the `nodes` table.
    #[error("node {node_id} not found")]
    NodeNotFound { node_id: i32 },

    /// The configured pool can't fit another /N subnet.
    #[error(
        "compute pool {pool} (subnets of /{prefix_len}) is exhausted: {used_count} subnets in use"
    )]
    PoolExhausted {
        pool: Ipv4Net,
        prefix_len: u8,
        used_count: usize,
    },

    /// `network_config` row missing or malformed (e.g. invalid CIDR text).
    #[error("network_config is invalid: {reason}")]
    InvalidConfig { reason: String },

    /// The node already has a `compute_cidr` allocated.
    #[error("node {node_id} already has compute_cidr {existing}")]
    AlreadyAllocated { node_id: i32, existing: Ipv4Net },

    /// Underlay address must be set before allocation (allocator has no
    /// way to guess the right value — that's a node-registration concern).
    #[error("node {node_id} has no underlay_address; cannot allocate compute_cidr")]
    UnderlayMissing { node_id: i32 },

    /// Persisted underlay address is not a valid IP.
    #[error("node {node_id} has malformed underlay_address {raw:?}: {reason}")]
    UnderlayInvalid {
        node_id: i32,
        raw: String,
        reason: String,
    },

    /// Persisted compute_cidr is not a valid IPv4 CIDR.
    #[error("node {node_id} has malformed compute_cidr {raw:?}: {reason}")]
    ComputeCidrInvalid {
        node_id: i32,
        raw: String,
        reason: String,
    },

    /// Sea-ORM / Postgres error.
    #[error("database error: {0}")]
    Database(#[from] DbErr),
}

/// Allocator surface — trait so callers can mock it out in unit tests
/// without spinning up Postgres.
#[async_trait]
pub trait ComputeNetworkAllocator: Send + Sync {
    /// Reserve a CIDR for `node_id` and return the resulting [`NodeAlloc`].
    ///
    /// Idempotent in the sense that calling it twice for a node that
    /// already has an allocation returns [`AllocatorError::AlreadyAllocated`]
    /// rather than producing a second one. Callers should treat that as a
    /// success ("we already have one") and fetch the existing alloc with
    /// [`Self::get_alloc`].
    async fn allocate_for_node(&self, node_id: i32) -> Result<NodeAllocPersisted, AllocatorError>;

    /// Release the CIDR for `node_id` (set the column back to NULL). Safe
    /// to call when no allocation exists.
    async fn release(&self, node_id: i32) -> Result<(), AllocatorError>;

    /// Peer list as seen by `viewer_node_id` — every node with a
    /// `compute_cidr` set, excluding the viewer.
    async fn peer_list(&self, viewer_node_id: i32) -> Result<Vec<Peer>, AllocatorError>;

    /// Fetch the current allocation for a node, if any.
    async fn get_alloc(&self, node_id: i32) -> Result<Option<NodeAllocPersisted>, AllocatorError>;
}

/// Persisted form of a [`crate::NodeAlloc`] — same fields plus the
/// integer database id, which the kernel layer doesn't care about but
/// the control plane uses everywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeAllocPersisted {
    pub node_id: i32,
    /// The opaque uuid the kernel layer logs against. We synthesize a
    /// stable v5 from the integer id so `NodeAlloc.node_id` is always
    /// derivable from the database row.
    pub external_id: Uuid,
    pub compute_cidr: Ipv4Net,
    pub bridge_address: IpAddr,
    pub underlay_address: IpAddr,
}

impl From<NodeAllocPersisted> for crate::NodeAlloc {
    fn from(p: NodeAllocPersisted) -> Self {
        Self {
            node_id: p.external_id,
            compute_cidr: p.compute_cidr,
            bridge_address: p.bridge_address,
            underlay_address: p.underlay_address,
        }
    }
}

/// Postgres-backed implementation. Cheap to clone (`Arc<DatabaseConnection>`).
#[derive(Clone)]
pub struct PostgresAllocator {
    db: Arc<DatabaseConnection>,
}

impl PostgresAllocator {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl ComputeNetworkAllocator for PostgresAllocator {
    async fn allocate_for_node(&self, node_id: i32) -> Result<NodeAllocPersisted, AllocatorError> {
        let txn = self.db.begin().await?;

        // 1. Load the cluster network config (singleton row id = 1).
        let cfg = nc::Entity::find()
            .one(&txn)
            .await?
            .ok_or(AllocatorError::InvalidConfig {
                reason: "network_config singleton row missing".into(),
            })?;

        let pool =
            parse_cidr(&cfg.compute_pool_cidr).map_err(|e| AllocatorError::InvalidConfig {
                reason: format!("compute_pool_cidr: {}", e),
            })?;
        let prefix_len =
            u8::try_from(cfg.subnet_prefix_len).map_err(|_| AllocatorError::InvalidConfig {
                reason: format!("subnet_prefix_len {} out of range", cfg.subnet_prefix_len),
            })?;
        if prefix_len <= pool.prefix_len() || prefix_len > 32 {
            return Err(AllocatorError::InvalidConfig {
                reason: format!(
                    "subnet_prefix_len {} must be greater than pool prefix {} and <= 32",
                    prefix_len,
                    pool.prefix_len()
                ),
            });
        }

        // 2. Load the target node and verify preconditions.
        let node = nodes::Entity::find_by_id(node_id)
            .one(&txn)
            .await?
            .ok_or(AllocatorError::NodeNotFound { node_id })?;

        if let Some(existing) = node.compute_cidr.as_deref() {
            let parsed = parse_cidr(existing).map_err(|e| AllocatorError::ComputeCidrInvalid {
                node_id,
                raw: existing.into(),
                reason: e.to_string(),
            })?;
            return Err(AllocatorError::AlreadyAllocated {
                node_id,
                existing: parsed,
            });
        }

        let underlay_raw = node
            .underlay_address
            .clone()
            .ok_or(AllocatorError::UnderlayMissing { node_id })?;
        let underlay: IpAddr = underlay_raw
            .parse()
            .map_err(
                |e: std::net::AddrParseError| AllocatorError::UnderlayInvalid {
                    node_id,
                    raw: underlay_raw.clone(),
                    reason: e.to_string(),
                },
            )?;

        // 3. Load all currently-used CIDRs so we can pick a free one.
        let used_rows: Vec<Option<String>> = nodes::Entity::find()
            .filter(nodes::Column::ComputeCidr.is_not_null())
            .select_only()
            .column(nodes::Column::ComputeCidr)
            .into_tuple()
            .all(&txn)
            .await?;

        let mut used: Vec<Ipv4Net> = Vec::with_capacity(used_rows.len());
        for raw in used_rows.into_iter().flatten() {
            match parse_cidr(&raw) {
                Ok(c) => used.push(c),
                Err(e) => {
                    // A malformed row would silently shadow a valid free
                    // subnet; surface it loudly instead of carrying on.
                    return Err(AllocatorError::ComputeCidrInvalid {
                        node_id: 0,
                        raw,
                        reason: e.to_string(),
                    });
                }
            }
        }

        // 4. Find the lowest-numbered free subnet of `prefix_len` inside `pool`.
        let chosen =
            pick_free_subnet(pool, prefix_len, &used).ok_or(AllocatorError::PoolExhausted {
                pool,
                prefix_len,
                used_count: used.len(),
            })?;
        let bridge = bridge_address_for(&chosen);

        // 5. Persist. The partial-unique index on compute_cidr is the
        //    backstop against a concurrent allocator picking the same
        //    subnet — we'd hit `RecordNotInserted` on conflict; but
        //    inside a SERIALIZABLE-equivalent of REPEATABLE READ + the
        //    transactional select the race window is empty in practice.
        let mut active: nodes::ActiveModel = node.clone().into();
        active.compute_cidr = Set(Some(chosen.to_string()));
        active.update(&txn).await?;

        txn.commit().await?;

        let external_id = Uuid::new_v5(
            &Uuid::NAMESPACE_OID,
            format!("temps-node-{}", node_id).as_bytes(),
        );
        info!(node_id, %chosen, %bridge, "compute_cidr allocated");
        Ok(NodeAllocPersisted {
            node_id,
            external_id,
            compute_cidr: chosen,
            bridge_address: bridge,
            underlay_address: underlay,
        })
    }

    async fn release(&self, node_id: i32) -> Result<(), AllocatorError> {
        nodes::Entity::update_many()
            .col_expr(
                nodes::Column::ComputeCidr,
                Expr::value(Option::<String>::None),
            )
            .filter(nodes::Column::Id.eq(node_id))
            .exec(self.db.as_ref())
            .await?;
        info!(node_id, "compute_cidr released");
        Ok(())
    }

    async fn peer_list(&self, viewer_node_id: i32) -> Result<Vec<Peer>, AllocatorError> {
        let rows = nodes::Entity::find()
            .filter(nodes::Column::ComputeCidr.is_not_null())
            .filter(nodes::Column::UnderlayAddress.is_not_null())
            .filter(nodes::Column::Id.ne(viewer_node_id))
            .order_by_asc(nodes::Column::Id)
            .all(self.db.as_ref())
            .await?;

        let mut peers = Vec::with_capacity(rows.len());
        for row in rows {
            let id = row.id;
            // Both columns guaranteed non-NULL by the filters above; unwrap is safe in this scope.
            let cidr_raw = row.compute_cidr.unwrap_or_default();
            let underlay_raw = row.underlay_address.unwrap_or_default();
            let cidr = parse_cidr(&cidr_raw).map_err(|e| AllocatorError::ComputeCidrInvalid {
                node_id: id,
                raw: cidr_raw.clone(),
                reason: e.to_string(),
            })?;
            let underlay: IpAddr =
                underlay_raw
                    .parse()
                    .map_err(
                        |e: std::net::AddrParseError| AllocatorError::UnderlayInvalid {
                            node_id: id,
                            raw: underlay_raw.clone(),
                            reason: e.to_string(),
                        },
                    )?;
            let external_id = Uuid::new_v5(
                &Uuid::NAMESPACE_OID,
                format!("temps-node-{}", id).as_bytes(),
            );
            peers.push(Peer {
                node_id: external_id,
                compute_cidr: cidr,
                underlay_address: underlay,
            });
        }
        Ok(peers)
    }

    async fn get_alloc(&self, node_id: i32) -> Result<Option<NodeAllocPersisted>, AllocatorError> {
        let Some(node) = nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await?
        else {
            return Ok(None);
        };

        let Some(cidr_raw) = node.compute_cidr.as_deref() else {
            return Ok(None);
        };
        let cidr = parse_cidr(cidr_raw).map_err(|e| AllocatorError::ComputeCidrInvalid {
            node_id,
            raw: cidr_raw.into(),
            reason: e.to_string(),
        })?;
        let underlay_raw = node
            .underlay_address
            .clone()
            .ok_or(AllocatorError::UnderlayMissing { node_id })?;
        let underlay: IpAddr = underlay_raw
            .parse()
            .map_err(
                |e: std::net::AddrParseError| AllocatorError::UnderlayInvalid {
                    node_id,
                    raw: underlay_raw.clone(),
                    reason: e.to_string(),
                },
            )?;
        let external_id = Uuid::new_v5(
            &Uuid::NAMESPACE_OID,
            format!("temps-node-{}", node_id).as_bytes(),
        );
        Ok(Some(NodeAllocPersisted {
            node_id,
            external_id,
            compute_cidr: cidr,
            bridge_address: bridge_address_for(&cidr),
            underlay_address: underlay,
        }))
    }
}

// ---------------------------------------------------------------------------
// Pure helpers (unit-tested below; no DB / IO).
// ---------------------------------------------------------------------------

fn parse_cidr(s: &str) -> Result<Ipv4Net, ipnet::AddrParseError> {
    Ipv4Net::from_str(s)
}

/// First usable host in a /N: network address + 1.
pub(crate) fn bridge_address_for(cidr: &Ipv4Net) -> IpAddr {
    let net = cidr.network();
    let octets = net.octets();
    let bumped = u32::from_be_bytes(octets).saturating_add(1).to_be_bytes();
    IpAddr::V4(std::net::Ipv4Addr::from(bumped))
}

/// Return the lowest-numbered /prefix_len subnet of `pool` that does not
/// overlap any subnet in `used`. `None` when the pool is exhausted.
pub(crate) fn pick_free_subnet(pool: Ipv4Net, prefix_len: u8, used: &[Ipv4Net]) -> Option<Ipv4Net> {
    pool.subnets(prefix_len).ok()?.find(|candidate| {
        !used
            .iter()
            .any(|u| crate::config::cidrs_overlap(u, candidate))
    })
}

// ---------------------------------------------------------------------------
// Tests — pure helpers only. Postgres-touching tests live in
// crates/temps-network/tests/it_allocator.rs (Docker-gated).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_address_is_first_host() {
        let c = Ipv4Net::from_str("172.20.5.0/24").unwrap();
        assert_eq!(bridge_address_for(&c).to_string(), "172.20.5.1");
    }

    #[test]
    fn picks_lowest_free_subnet() {
        let pool = Ipv4Net::from_str("172.20.0.0/16").unwrap();
        let used = vec![
            Ipv4Net::from_str("172.20.0.0/24").unwrap(),
            Ipv4Net::from_str("172.20.1.0/24").unwrap(),
            Ipv4Net::from_str("172.20.3.0/24").unwrap(),
        ];
        let chosen = pick_free_subnet(pool, 24, &used).unwrap();
        assert_eq!(chosen.to_string(), "172.20.2.0/24");
    }

    #[test]
    fn skips_overlapping_supernet() {
        // If 172.20.0.0/20 is in use, /24 candidates inside it must be skipped.
        let pool = Ipv4Net::from_str("172.20.0.0/16").unwrap();
        let used = vec![Ipv4Net::from_str("172.20.0.0/20").unwrap()];
        let chosen = pick_free_subnet(pool, 24, &used).unwrap();
        assert_eq!(chosen.to_string(), "172.20.16.0/24");
    }

    #[test]
    fn returns_none_when_exhausted() {
        let pool = Ipv4Net::from_str("172.20.0.0/30").unwrap();
        let used = vec![Ipv4Net::from_str("172.20.0.0/30").unwrap()];
        assert!(pick_free_subnet(pool, 30, &used).is_none());
    }

    #[test]
    fn empty_used_picks_first() {
        let pool = Ipv4Net::from_str("10.50.0.0/16").unwrap();
        let chosen = pick_free_subnet(pool, 24, &[]).unwrap();
        assert_eq!(chosen.to_string(), "10.50.0.0/24");
    }
}
