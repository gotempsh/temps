// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Saved log searches (ADR 073 D1): a named, per-project log query that
//! the console, log alerts and dashboard tiles reference by id.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "log_saved_searches")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// Owning project; the query is always scoped to it.
    pub project_id: i32,
    /// Unique per project.
    pub name: String,
    pub description: Option<String>,
    /// The filter set, typed as `temps_log_aggregator::SavedSearchQuery` in
    /// the service layer: source, levels, envs, services, container ids, node
    /// ids, deployment, free text and attribute predicates.
    #[sea_orm(column_type = "JsonBinary")]
    pub query: Json,
    /// Relative time range for the console (`15m`, `1h`, `24h`, `7d`).
    /// Default `1h`.
    pub time_range: String,
    /// Pinned searches sort first in the console. Default `false`.
    pub pinned: bool,
    /// User who saved it, `None` once that user is deleted.
    pub created_by: Option<i32>,
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
        belongs_to = "super::users::Entity",
        from = "Column::CreatedBy",
        to = "super::users::Column::Id",
        on_update = "NoAction",
        on_delete = "SetNull"
    )]
    CreatedBy,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        let now = chrono::Utc::now();
        if insert && self.created_at.is_not_set() {
            self.created_at = Set(now);
        }
        self.updated_at = Set(now);
        Ok(self)
    }
}
