// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! How the mesh routes one pair of members (ADR 048 D4).

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "mesh_links")]
pub struct Model {
    /// The lower of the two members' public keys.
    #[sea_orm(primary_key, auto_increment = false)]
    pub key_a: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub key_b: String,
    /// Routed through the hub because the direct link never came up.
    pub via_hub: bool,
    /// When the pair took its current route.
    pub since: DBDateTime,
    /// The endpoints the route was decided with.
    pub endpoint_a: Option<String>,
    pub endpoint_b: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
