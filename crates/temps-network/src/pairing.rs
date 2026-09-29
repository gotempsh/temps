// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pending one-paste node pairings (ADR 048 D2b): the control plane's
//! record of a node it will dial, from the operator creating the pairing
//! until the node registers over the mesh.
//!
//! A pairing reserves a mesh address, so it is serialized on the same
//! `network_config` row lock as mesh registration. Once the node's public
//! key arrives the control plane peers with it (see [`crate::mesh::peers`]);
//! the node then registers with the pairing's enrollment token, which links
//! the pairing to the node, and the node takes over the key and address.

use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr};

use ipnet::Ipv4Net;
use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, Set, TransactionTrait,
};
use temps_entities::{network_config, node_pairings, nodes};

use crate::mesh::{check_endpoint_outside_pools, next_mesh_address, MeshError};

/// Waiting for the node's key.
pub const STATUS_WAITING: &str = "waiting";
/// Key received; the control plane peers with the node.
pub const STATUS_KEY_RECEIVED: &str = "key_received";
/// The node registered and owns the key and address.
pub const STATUS_COMPLETED: &str = "completed";
pub const STATUS_EXPIRED: &str = "expired";
pub const STATUS_CANCELLED: &str = "cancelled";
/// Statuses that hold an address (and, once received, a key).
pub const PENDING: [&str; 2] = [STATUS_WAITING, STATUS_KEY_RECEIVED];

/// What the operator asked for.
pub struct NewPairing {
    pub pairing_id: String,
    pub name: String,
    pub node_endpoint: SocketAddr,
    pub secret_encrypted: String,
    pub enrollment_token_id: i32,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub created_by_user_id: Option<i32>,
}

/// Mesh addresses held by nodes or pending pairings. Call inside the
/// `network_config` row lock.
pub(crate) async fn taken_addresses<C: sea_orm::ConnectionTrait>(
    db: &C,
) -> Result<HashSet<Ipv4Addr>, MeshError> {
    let mut taken: HashSet<Ipv4Addr> = nodes::Entity::find()
        .filter(nodes::Column::MeshWgAddress.is_not_null())
        .all(db)
        .await?
        .into_iter()
        .filter_map(|row| row.mesh_wg_address?.parse().ok())
        .collect();
    taken.extend(
        node_pairings::Entity::find()
            .filter(node_pairings::Column::Status.is_in(PENDING))
            .all(db)
            .await?
            .into_iter()
            .filter_map(|row| row.mesh_address.parse::<Ipv4Addr>().ok()),
    );
    Ok(taken)
}

/// Create a pairing and reserve its mesh address.
pub async fn create(
    db: &DatabaseConnection,
    new: NewPairing,
) -> Result<node_pairings::Model, MeshError> {
    let txn = db.begin().await?;
    let cfg = network_config::Entity::find_by_id(1)
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| MeshError::Corrupt {
            what: "network_config".into(),
            reason: "singleton row missing".into(),
        })?;
    let settings = crate::mesh::settings_from(&cfg)?.ok_or(MeshError::Disabled)?;
    check_endpoint_outside_pools(
        new.node_endpoint,
        settings.cidr,
        cfg.compute_pool_cidr.parse::<Ipv4Net>().ok(),
    )?;
    let address = next_mesh_address(settings.cidr, &taken_addresses(&txn).await?)?;
    let now = chrono::Utc::now();
    let model = node_pairings::ActiveModel {
        pairing_id: Set(new.pairing_id),
        name: Set(new.name),
        node_endpoint: Set(new.node_endpoint.to_string()),
        mesh_address: Set(address.to_string()),
        secret_encrypted: Set(new.secret_encrypted),
        enrollment_token_id: Set(new.enrollment_token_id),
        public_key: Set(None),
        status: Set(STATUS_WAITING.to_string()),
        last_error: Set(None),
        last_attempt_at: Set(None),
        key_received_at: Set(None),
        expires_at: Set(new.expires_at),
        node_id: Set(None),
        created_by_user_id: Set(new.created_by_user_id),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(&txn)
    .await?;
    txn.commit().await?;
    Ok(model)
}

/// The most recent pairings, newest first.
pub async fn list(db: &DatabaseConnection) -> Result<Vec<node_pairings::Model>, MeshError> {
    Ok(node_pairings::Entity::find()
        .order_by_desc(node_pairings::Column::Id)
        .limit(50)
        .all(db)
        .await?)
}

pub async fn get(
    db: &DatabaseConnection,
    id: i32,
) -> Result<Option<node_pairings::Model>, MeshError> {
    Ok(node_pairings::Entity::find_by_id(id).one(db).await?)
}

/// Pairings the control plane should be dialing now.
pub async fn due(db: &DatabaseConnection) -> Result<Vec<node_pairings::Model>, MeshError> {
    Ok(node_pairings::Entity::find()
        .filter(node_pairings::Column::Status.eq(STATUS_WAITING))
        .filter(node_pairings::Column::ExpiresAt.gt(chrono::Utc::now()))
        .order_by_asc(node_pairings::Column::Id)
        .all(db)
        .await?)
}

/// Cancel a pending pairing, releasing its address. `false` when it is not
/// pending (already finished, expired or cancelled).
pub async fn cancel(db: &DatabaseConnection, id: i32) -> Result<bool, MeshError> {
    let result = node_pairings::Entity::update_many()
        .col_expr(node_pairings::Column::Status, Expr::value(STATUS_CANCELLED))
        .col_expr(
            node_pairings::Column::UpdatedAt,
            Expr::value(chrono::Utc::now()),
        )
        .filter(node_pairings::Column::Id.eq(id))
        .filter(node_pairings::Column::Status.is_in(PENDING))
        .exec(db)
        .await?;
    Ok(result.rows_affected > 0)
}

/// Mark every pending pairing past its expiry as expired.
pub async fn expire_stale(db: &DatabaseConnection) -> Result<u64, MeshError> {
    let now = chrono::Utc::now();
    let result = node_pairings::Entity::update_many()
        .col_expr(node_pairings::Column::Status, Expr::value(STATUS_EXPIRED))
        .col_expr(node_pairings::Column::UpdatedAt, Expr::value(now))
        .filter(node_pairings::Column::Status.is_in(PENDING))
        .filter(node_pairings::Column::ExpiresAt.lte(now))
        .exec(db)
        .await?;
    Ok(result.rows_affected)
}

/// Note a failed dialing attempt for the operator (the status stays
/// `waiting`; the control plane keeps trying until the pairing expires).
pub async fn record_attempt(
    db: &DatabaseConnection,
    id: i32,
    error: Option<&str>,
) -> Result<(), MeshError> {
    let now = chrono::Utc::now();
    node_pairings::Entity::update_many()
        .col_expr(
            node_pairings::Column::LastError,
            Expr::value(error.map(str::to_string)),
        )
        .col_expr(node_pairings::Column::LastAttemptAt, Expr::value(now))
        .col_expr(node_pairings::Column::UpdatedAt, Expr::value(now))
        .filter(node_pairings::Column::Id.eq(id))
        .filter(node_pairings::Column::Status.eq(STATUS_WAITING))
        .exec(db)
        .await?;
    Ok(())
}

/// Record the node's public key from a verified exchange. The key must not
/// belong to the control plane, a node or another pending pairing.
pub async fn record_key(
    db: &DatabaseConnection,
    id: i32,
    public_key: &str,
) -> Result<(), MeshError> {
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
    let in_use = cfg.control_plane_wg_public_key.as_deref() == Some(public_key)
        || nodes::Entity::find()
            .filter(nodes::Column::MeshWgPublicKey.eq(public_key))
            .one(&txn)
            .await?
            .is_some()
        || node_pairings::Entity::find()
            .filter(node_pairings::Column::PublicKey.eq(public_key))
            .filter(node_pairings::Column::Status.is_in(PENDING))
            .filter(node_pairings::Column::Id.ne(id))
            .one(&txn)
            .await?
            .is_some();
    if in_use {
        return Err(MeshError::PublicKeyInUse);
    }
    let now = chrono::Utc::now();
    node_pairings::Entity::update_many()
        .col_expr(
            node_pairings::Column::PublicKey,
            Expr::value(public_key.to_string()),
        )
        .col_expr(
            node_pairings::Column::Status,
            Expr::value(STATUS_KEY_RECEIVED),
        )
        .col_expr(node_pairings::Column::KeyReceivedAt, Expr::value(now))
        .col_expr(
            node_pairings::Column::LastError,
            Expr::value(Option::<String>::None),
        )
        .col_expr(node_pairings::Column::UpdatedAt, Expr::value(now))
        .filter(node_pairings::Column::Id.eq(id))
        .filter(node_pairings::Column::Status.eq(STATUS_WAITING))
        .exec(&txn)
        .await?;
    txn.commit().await?;
    Ok(())
}

/// The pairing a node registered with, by its enrollment token: link it to
/// the node and hand the node its key, address and endpoint. `None` when the
/// token was not minted for a pairing.
pub async fn link_node(
    db: &std::sync::Arc<DatabaseConnection>,
    enrollment_token_id: i32,
    node_id: i32,
) -> Result<Option<node_pairings::Model>, MeshError> {
    let Some(pairing) = node_pairings::Entity::find()
        .filter(node_pairings::Column::EnrollmentTokenId.eq(enrollment_token_id))
        .one(db.as_ref())
        .await?
    else {
        return Ok(None);
    };
    let mut active: node_pairings::ActiveModel = pairing.clone().into();
    active.node_id = Set(Some(node_id));
    active.updated_at = Set(chrono::Utc::now());
    active.update(db.as_ref()).await?;
    let (Some(public_key), true) = (
        pairing.public_key.as_deref(),
        pairing.status == STATUS_KEY_RECEIVED,
    ) else {
        return Ok(Some(pairing));
    };
    let endpoint: SocketAddr =
        pairing
            .node_endpoint
            .parse()
            .map_err(|error: std::net::AddrParseError| MeshError::Corrupt {
                what: format!("pairing {} node_endpoint", pairing.id),
                reason: error.to_string(),
            })?;
    crate::mesh::register_node(db, node_id, public_key, endpoint).await?;
    Ok(Some(pairing))
}

/// Inside [`crate::mesh::register_node`]'s transaction: the address a
/// pairing linked to `node_id` reserved for `public_key`, completing the
/// pairing. `None` when there is no such pairing.
pub(crate) async fn adopt_for_node<C: sea_orm::ConnectionTrait>(
    txn: &C,
    node_id: i32,
    public_key: &str,
) -> Result<Option<Ipv4Addr>, MeshError> {
    let Some(pairing) = node_pairings::Entity::find()
        .filter(node_pairings::Column::NodeId.eq(node_id))
        .filter(node_pairings::Column::PublicKey.eq(public_key))
        .filter(node_pairings::Column::Status.eq(STATUS_KEY_RECEIVED))
        .one(txn)
        .await?
    else {
        return Ok(None);
    };
    let address = pairing
        .mesh_address
        .parse::<Ipv4Addr>()
        .map_err(|error| MeshError::Corrupt {
            what: format!("pairing {} mesh_address", pairing.id),
            reason: error.to_string(),
        })?;
    let mut active: node_pairings::ActiveModel = pairing.into();
    active.status = Set(STATUS_COMPLETED.to_string());
    active.updated_at = Set(chrono::Utc::now());
    active.update(txn).await?;
    Ok(Some(address))
}

/// Pairings the control plane peers with: key received, not expired.
pub(crate) async fn peering<C: sea_orm::ConnectionTrait>(
    db: &C,
) -> Result<Vec<node_pairings::Model>, MeshError> {
    Ok(node_pairings::Entity::find()
        .filter(node_pairings::Column::Status.eq(STATUS_KEY_RECEIVED))
        .filter(node_pairings::Column::ExpiresAt.gt(chrono::Utc::now()))
        .order_by_asc(node_pairings::Column::Id)
        .all(db)
        .await?)
}
