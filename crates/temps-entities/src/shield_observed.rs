// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What the fleet learned (ADR 066, D9): addresses and fingerprints
//! promoted from events, each until `expires_at`.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "shield_observed")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// `ip` or `ja4`.
    pub kind: String,
    /// The address or the fingerprint.
    pub subject: String,
    /// `challenge` or `block`.
    pub verdict: String,
    /// Why it was promoted, as the console shows it.
    pub reason: String,
    pub first_seen: DBDateTime,
    pub last_seen: DBDateTime,
    pub hits: i64,
    pub expires_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
