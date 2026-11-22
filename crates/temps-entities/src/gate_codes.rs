// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Single-use sign-in codes (ADR 065 D8): 60 seconds, bound to the address,
//! the console session and the PKCE challenge. Stored by hash; redeeming
//! deletes the row.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "gate_codes")]
pub struct Model {
    /// Hex SHA-256 of the code.
    #[sea_orm(primary_key, auto_increment = false)]
    pub code_hash: String,
    pub address_id: String,
    pub user_id: i32,
    pub console_session_id: i32,
    /// S256 PKCE challenge.
    pub code_challenge: String,
    pub expires_at: DBDateTime,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
