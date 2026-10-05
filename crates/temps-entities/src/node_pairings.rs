// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A pending one-paste node pairing (ADR 048 D2b).

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "node_pairings")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// Base64url pairing id carried in every pairing message.
    pub pairing_id: String,
    /// Name the node registers under (the enrollment token is bound to it).
    pub name: String,
    /// `ip:port` the control plane dials: the node's WireGuard endpoint.
    pub node_endpoint: String,
    /// Mesh address reserved for the node.
    pub mesh_address: String,
    /// The pairing secret, encrypted with the installation key.
    #[serde(skip_serializing)]
    pub secret_encrypted: String,
    /// The single-use enrollment token the node registers with.
    pub enrollment_token_id: i32,
    /// The node's WireGuard public key, once the exchange succeeded.
    pub public_key: Option<String>,
    /// `waiting`, `key_received`, `completed`, `expired` or `cancelled`.
    pub status: String,
    /// Why the last attempt failed, for the operator.
    pub last_error: Option<String>,
    /// Why the control plane last refused the node's key; kept until a key
    /// is accepted, unlike `last_error`.
    pub last_rejection: Option<String>,
    pub last_attempt_at: Option<DBDateTime>,
    pub key_received_at: Option<DBDateTime>,
    pub expires_at: DBDateTime,
    /// The node that registered with this pairing's token.
    pub node_id: Option<i32>,
    pub created_by_user_id: Option<i32>,
    /// A control-plane process dialing the node holds the pairing until then.
    pub dialing_until: Option<DBDateTime>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
