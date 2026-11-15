// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project edge rules (ADR-089).
//!
//! `expression` and `action` hold the JSON of `temps_edge_rules::Group` and
//! `temps_edge_rules::Action`. They are kept as raw JSON here so one row
//! edited by hand into an invalid shape cannot make every other row of the
//! table unreadable; the service and the proxy decode them per rule and
//! report the rule that failed.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "project_edge_rules")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: i32,
    /// `None` applies the rule to every environment of the project.
    pub environment_id: Option<i32>,
    /// 0-based evaluation order within the project.
    pub position: i32,
    pub name: String,
    pub enabled: bool,
    /// `enforce` or `log_only`.
    pub mode: String,
    #[sea_orm(column_type = "JsonBinary")]
    pub expression: Json,
    #[sea_orm(column_type = "JsonBinary")]
    pub action: Json,
    /// Edit counter for optimistic concurrency.
    pub revision: i64,
    pub created_by: Option<i32>,
    pub updated_by: Option<i32>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "crate::projects::Entity",
        from = "Column::ProjectId",
        to = "crate::projects::Column::Id",
        on_delete = "Cascade"
    )]
    Project,
    #[sea_orm(
        belongs_to = "crate::environments::Entity",
        from = "Column::EnvironmentId",
        to = "crate::environments::Column::Id",
        on_delete = "Cascade"
    )]
    Environment,
}

impl Related<crate::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl Related<crate::environments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Environment.def()
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
        if self.updated_at.is_not_set() {
            self.updated_at = Set(now);
        }
        Ok(self)
    }
}
