// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `TempsLogin` grants the console has signed (ADR-055 §11).
//!
//! The grant itself is a signed token the ingress verifies with no database
//! call; this row exists so a grant can be revoked before it expires. A
//! revoked, unexpired grant's id travels in the route snapshot, and the
//! ingress refuses it from then on. Inserts never reload routes (one happens
//! per login); setting `revoked_at` does.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "ingress_login_grants")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub user_id: i32,
    pub owner_kind: String,
    pub owner_id: i64,
    /// Lowercase, no port.
    pub host: String,
    pub permission: String,
    /// When the last session cookie this grant can buy expires.
    pub expires_at: DBDateTime,
    pub revoked_at: Option<DBDateTime>,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "crate::users::Entity",
        from = "Column::UserId",
        to = "crate::users::Column::Id",
        on_delete = "Cascade"
    )]
    User,
}

impl Related<crate::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
