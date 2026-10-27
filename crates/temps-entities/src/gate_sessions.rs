// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One sign-in to a signed-in address (ADR 065 D8), tied to the console
//! session that created it and expiring at the earlier of 8 hours and that
//! session's expiry.
//!
//! No foreign keys: a revoked session is listed in the route snapshot until
//! it expires, so the row must outlive the console session and the user.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "gate_sessions")]
pub struct Model {
    /// Random, the `sid` in the gate token.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// `custom:<id>` or `environment:<id>`.
    pub address_id: String,
    pub user_id: i32,
    /// `sessions.id` of the console session that signed in.
    pub console_session_id: i32,
    pub created_at: DBDateTime,
    pub expires_at: DBDateTime,
    /// Set by logout on the address or the console's "Sign out of apps".
    pub revoked_at: Option<DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
