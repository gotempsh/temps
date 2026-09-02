// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project-scoped, optionally environment-scoped analytics ingest keys.
//!
//! `public_key` is **public by construction** (ADR-040): it ships in a browser
//! bundle and in query strings, is stored in the clear, and is never hashed or
//! masked. It is not a secret and must never be treated as one — that is why
//! the column is named `public_key` and why there is no `secret_key`
//! counterpart carried over from `project_dsns`.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "analytics_ingest_keys")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: i32,
    /// `None` means the key is scoped to the whole project.
    pub environment_id: Option<i32>,
    pub name: String,
    pub public_key: String,
    pub is_active: bool,
    pub revoked_at: Option<DBDateTime>,
    /// `None` or `<= 0` means unlimited.
    pub rate_limit_per_minute: Option<i32>,
    /// JSON array of exact origins. `None` or `[]` means any origin.
    pub allowed_origins: Option<Json>,
    pub event_count: i64,
    pub last_used_at: Option<DBDateTime>,
    pub created_by_user_id: Option<i32>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Project,
    #[sea_orm(
        belongs_to = "super::environments::Entity",
        from = "Column::EnvironmentId",
        to = "super::environments::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Environment,
    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::CreatedByUserId",
        to = "super::users::Column::Id",
        on_update = "NoAction",
        on_delete = "SetNull"
    )]
    CreatedByUser,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl Related<super::environments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Environment.def()
    }
}

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::CreatedByUser.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        let now = chrono::Utc::now();

        if insert {
            if self.created_at.is_not_set() {
                self.created_at = Set(now);
            }
            if self.updated_at.is_not_set() {
                self.updated_at = Set(now);
            }
        } else {
            self.updated_at = Set(now);
        }

        Ok(self)
    }
}
