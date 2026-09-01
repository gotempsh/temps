// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "agent_run_logs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub run_id: i32,
    pub level: String,
    pub message: String,
    #[sea_orm(column_type = "JsonBinary")]
    pub metadata: Option<serde_json::Value>,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::agent_runs::Entity",
        from = "Column::RunId",
        to = "super::agent_runs::Column::Id",
        on_delete = "Cascade"
    )]
    Run,
}

impl Related<super::agent_runs::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Run.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        if insert && self.created_at.is_not_set() {
            self.created_at = Set(chrono::Utc::now());
        }
        Ok(self)
    }
}
