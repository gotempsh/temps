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
    sea_query::Expr, ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use temps_entities::{node_pairings, nodes};

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

/// Pairings in progress at once. Each one holds a mesh address and is dialed
/// every few seconds until it expires; no real cluster adds this many nodes
/// in one half hour, so the cap only stops runaway creation.
pub const MAX_PENDING: usize = 20;

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
    let cfg = crate::mesh::lock_config(&txn).await?;
    let settings = crate::mesh::settings_from(&cfg)?.ok_or(MeshError::Disabled)?;
    let pending = node_pairings::Entity::find()
        .filter(node_pairings::Column::Status.is_in(PENDING))
        .count(&txn)
        .await?;
    if pending >= MAX_PENDING as u64 {
        return Err(MeshError::TooManyPairings { limit: MAX_PENDING });
    }
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

/// Take the pairing for one dialing attempt, for `lease`: `false` when
/// another control-plane process sharing the database is dialing it, or it
/// stopped waiting.
pub async fn claim(
    db: &DatabaseConnection,
    id: i32,
    lease: std::time::Duration,
) -> Result<bool, MeshError> {
    let now = chrono::Utc::now();
    let until = now + chrono::Duration::from_std(lease).unwrap_or(chrono::Duration::seconds(30));
    let claimed = node_pairings::Entity::update_many()
        .col_expr(node_pairings::Column::DialingUntil, Expr::value(until))
        .filter(node_pairings::Column::Id.eq(id))
        .filter(node_pairings::Column::Status.eq(STATUS_WAITING))
        .filter(
            sea_orm::Condition::any()
                .add(node_pairings::Column::DialingUntil.is_null())
                .add(node_pairings::Column::DialingUntil.lt(now)),
        )
        .exec(db)
        .await?;
    Ok(claimed.rows_affected == 1)
}

/// Give up the pairing after an attempt, so the next one starts at once.
pub async fn release(db: &DatabaseConnection, id: i32) -> Result<(), MeshError> {
    node_pairings::Entity::update_many()
        .col_expr(
            node_pairings::Column::DialingUntil,
            Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
        )
        .filter(node_pairings::Column::Id.eq(id))
        .exec(db)
        .await?;
    Ok(())
}

/// Cancel a pending pairing, releasing its address. `false` when it is not
/// pending (already finished, expired or cancelled).
pub async fn cancel(db: &DatabaseConnection, id: i32) -> Result<bool, MeshError> {
    cancel_where(db, id, false).await
}

/// [`cancel`], but only while no node has registered with the pairing: one
/// a node registered with (its `node_id` is set, though it may not have
/// completed yet) is left to finish. For cancelling a pairing the operator
/// did not cancel themselves, e.g. that of an interrupted SSH enrollment.
pub async fn cancel_unclaimed(db: &DatabaseConnection, id: i32) -> Result<bool, MeshError> {
    cancel_where(db, id, true).await
}

async fn cancel_where(
    db: &DatabaseConnection,
    id: i32,
    unclaimed_only: bool,
) -> Result<bool, MeshError> {
    let mut update = node_pairings::Entity::update_many()
        .col_expr(node_pairings::Column::Status, Expr::value(STATUS_CANCELLED))
        .col_expr(
            node_pairings::Column::UpdatedAt,
            Expr::value(chrono::Utc::now()),
        )
        .filter(node_pairings::Column::Id.eq(id))
        .filter(node_pairings::Column::Status.is_in(PENDING));
    if unclaimed_only {
        update = update.filter(node_pairings::Column::NodeId.is_null());
    }
    let result = update.exec(db).await?;
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

/// Record that the node's key was refused, and why (for the operator). The
/// refusal outlives later attempts' errors until a key is accepted.
pub async fn record_rejection(
    db: &DatabaseConnection,
    id: i32,
    message: &str,
) -> Result<(), MeshError> {
    let now = chrono::Utc::now();
    node_pairings::Entity::update_many()
        .col_expr(
            node_pairings::Column::LastRejection,
            Expr::value(message.to_string()),
        )
        .col_expr(
            node_pairings::Column::LastError,
            Expr::value(message.to_string()),
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
/// belong to the control plane, a node or another pending pairing, and the
/// pairing must still be waiting ([`MeshError::PairingClosed`] otherwise).
pub async fn record_key(
    db: &DatabaseConnection,
    id: i32,
    public_key: &str,
) -> Result<(), MeshError> {
    if !temps_wireguard::mesh::is_valid_public_key(public_key) {
        return Err(MeshError::InvalidPublicKey);
    }
    let txn = db.begin().await?;
    let cfg = crate::mesh::lock_config(&txn).await?;
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
    let updated = node_pairings::Entity::update_many()
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
        .col_expr(
            node_pairings::Column::LastRejection,
            Expr::value(Option::<String>::None),
        )
        .col_expr(node_pairings::Column::UpdatedAt, Expr::value(now))
        .filter(node_pairings::Column::Id.eq(id))
        .filter(node_pairings::Column::Status.eq(STATUS_WAITING))
        // Past its deadline even if the expiry sweep has not run yet.
        .filter(node_pairings::Column::ExpiresAt.gt(now))
        .exec(&txn)
        .await?;
    if updated.rows_affected == 0 {
        return Err(MeshError::PairingClosed);
    }
    txn.commit().await?;
    Ok(())
}

/// Whether registering a node with `enrollment_token_id` can complete its
/// pairing: checked before the node is created, so a pairing that cannot
/// complete is refused up front instead of leaving a half-registered node.
/// `Ok(())` when the token was not minted for a pairing.
pub async fn check_linkable(
    db: &DatabaseConnection,
    enrollment_token_id: i32,
) -> Result<(), MeshError> {
    let Some(pairing) = node_pairings::Entity::find()
        .filter(node_pairings::Column::EnrollmentTokenId.eq(enrollment_token_id))
        .one(db)
        .await?
    else {
        return Ok(());
    };
    if pairing.status != STATUS_KEY_RECEIVED {
        return Err(MeshError::PairingClosed);
    }
    let Some(public_key) = pairing.public_key.as_deref() else {
        return Err(MeshError::PairingClosed);
    };
    let cfg = crate::mesh::load_config(db).await?;
    let taken = cfg.control_plane_wg_public_key.as_deref() == Some(public_key)
        || nodes::Entity::find()
            .filter(nodes::Column::MeshWgPublicKey.eq(public_key))
            .one(db)
            .await?
            .is_some();
    if taken {
        return Err(MeshError::PublicKeyInUse);
    }
    Ok(())
}

/// The pairing a node registered with, by its enrollment token: link it to
/// the node and hand the node its key, address and endpoint, in one
/// transaction (a failure leaves the pairing as it was). `None` when the
/// token was not minted for a pairing.
pub async fn link_node(
    db: &std::sync::Arc<DatabaseConnection>,
    enrollment_token_id: i32,
    node_id: i32,
) -> Result<Option<node_pairings::Model>, MeshError> {
    let txn = db.begin().await?;
    // Locked for the rest of the transaction, and its state checked again
    // under the lock: [`check_linkable`] ran before the node was created, and
    // a cancellation or revocation (an operator, or recovery of an
    // interrupted SSH enrollment) may have landed since. A cancel that runs
    // after this commits matches nothing, since it only touches pairings no
    // node holds.
    let Some(pairing) = node_pairings::Entity::find()
        .filter(node_pairings::Column::EnrollmentTokenId.eq(enrollment_token_id))
        .lock_exclusive()
        .one(&txn)
        .await?
    else {
        return Ok(None);
    };
    if !linkable(&pairing, node_id) {
        tracing::warn!(
            pairing = pairing.id,
            enrollment_token_id,
            node_id,
            status = %pairing.status,
            linked_node_id = ?pairing.node_id,
            "refused to link a node to a pairing that is no longer waiting for it"
        );
        return Err(MeshError::PairingClosed);
    }
    let mut active: node_pairings::ActiveModel = pairing.clone().into();
    active.node_id = Set(Some(node_id));
    active.updated_at = Set(chrono::Utc::now());
    active.update(&txn).await?;
    if let (Some(public_key), true) = (
        pairing.public_key.as_deref(),
        pairing.status == STATUS_KEY_RECEIVED,
    ) {
        let endpoint: SocketAddr =
            pairing
                .node_endpoint
                .parse()
                .map_err(|error: std::net::AddrParseError| MeshError::Corrupt {
                    what: format!("pairing {} node_endpoint", pairing.id),
                    reason: error.to_string(),
                })?;
        crate::mesh::register_node_in(&txn, node_id, public_key, endpoint).await?;
    }
    txn.commit().await?;
    Ok(Some(pairing))
}

/// Whether `node_id` may take `pairing`: it received the node's key and no
/// other node holds it. A cancelled, expired or completed pairing never
/// links.
fn linkable(pairing: &node_pairings::Model, node_id: i32) -> bool {
    pairing.status == STATUS_KEY_RECEIVED && pairing.node_id.is_none_or(|held| held == node_id)
}

/// Whether a pending pairing other than `node_id`'s own holds `public_key`.
pub(crate) async fn held_by_other_pairing<C: sea_orm::ConnectionTrait>(
    txn: &C,
    public_key: &str,
    node_id: i32,
) -> Result<bool, MeshError> {
    Ok(node_pairings::Entity::find()
        .filter(node_pairings::Column::PublicKey.eq(public_key))
        .filter(node_pairings::Column::Status.is_in(PENDING))
        .filter(
            sea_orm::Condition::any()
                .add(node_pairings::Column::NodeId.is_null())
                .add(node_pairings::Column::NodeId.ne(node_id)),
        )
        .one(txn)
        .await?
        .is_some())
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
