// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A node that runs a replica of an ingress connector, and what it reported.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "ingress_connector_nodes")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub connector_id: i32,
    pub node_id: i32,
    /// The driver the node chose: `process`, `container` or `library:<name>`.
    pub driver: Option<String>,
    /// `pending`, `starting`, `ready`, `degraded` or `down`.
    pub state: String,
    pub ready_connections: Option<i32>,
    /// The provider's id for this replica (cloudflared's connector id).
    pub connector_ref: Option<String>,
    pub last_error: Option<String>,
    pub reported_at: Option<DBDateTime>,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
