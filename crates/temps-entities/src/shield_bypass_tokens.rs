// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bypass tokens (ADR 066, D7 and I2): monitors, CI and API clients that
//! Shield never challenges. Only the token's SHA-256 is stored.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "shield_bypass_tokens")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub project_id: i32,
    /// `None`: valid for every environment of the project.
    pub environment_id: Option<i32>,
    pub name: String,
    /// Hex SHA-256 of the whole token.
    #[serde(skip)]
    pub token_hash: String,
    pub created_by: Option<i32>,
    pub created_at: DBDateTime,
    pub last_used_at: Option<DBDateTime>,
    pub revoked_at: Option<DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id",
        on_delete = "Cascade"
    )]
    Project,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
