// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A compiled Shield snapshot every ingress loads (ADR 066, I3): a
//! `temps_shield::ShieldSnapshot`, postcard-encoded and zstd-compressed.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "shield_snapshots")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub revision: i64,
    pub created_at: DBDateTime,
    /// Hex SHA-256 of `body`.
    pub sha256: String,
    #[serde(skip)]
    pub body: Vec<u8>,
    /// The newest `shield_sources.updated_at` this revision was built from.
    pub sources_updated_at: Option<DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
