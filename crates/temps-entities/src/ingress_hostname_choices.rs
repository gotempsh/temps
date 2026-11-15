// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! An operator's choice of tunnel for one hostname (ADR 058 D9). See
//! `m20261021_000001_ingress_hostname_choices`.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "ingress_hostname_choices")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub hostname: String,
    /// The tunnel to publish through; `None` keeps the hostname off tunnels.
    pub connector_id: Option<i32>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
