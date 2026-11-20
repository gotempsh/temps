// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The daily salt of cookieless visitor ids (ADR 109 D3).
//!
//! One row per UTC day, shared by every node that ingests so they all derive
//! the same id for the same visitor. Only the current day's row is kept: an
//! older salt is deleted, after which ids derived from it cannot be recomputed
//! from stored data.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "analytics_identity_salts")]
pub struct Model {
    /// UTC day the salt is used for.
    #[sea_orm(primary_key, auto_increment = false)]
    pub day: Date,
    /// 32 random bytes, encrypted at rest with `EncryptionService`.
    #[serde(skip_serializing)]
    pub salt: String,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
