// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A node's latest report of its WireGuard handshakes (ADR 048 D4).

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "node_mesh_reports")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub node_id: i32,
    /// Peer public key → Unix time of the last completed handshake, as the
    /// control plane's clock places it. Peers never handshaken are absent.
    pub handshakes: Json,
    pub reported_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
