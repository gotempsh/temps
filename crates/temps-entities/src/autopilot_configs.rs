// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "autopilot_configs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: i32,
    pub enabled: bool,
    pub ai_provider: String,
    #[serde(skip_serializing)]
    pub api_key_encrypted: Option<String>,
    pub ai_provider_key_id: Option<i32>,
    pub daily_budget_cents: i32,
    pub max_turns_per_run: i32,
    pub cooldown_minutes: i32,
    pub trigger_on_new_error: bool,
    pub trigger_on_regression: bool,
    pub trigger_on_alarm: bool,
    pub branch_prefix: String,
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
        belongs_to = "super::ai_provider_keys::Entity",
        from = "Column::AiProviderKeyId",
        to = "super::ai_provider_keys::Column::Id",
        on_update = "NoAction",
        on_delete = "SetNull"
    )]
    AiProviderKey,
    #[sea_orm(has_many = "super::autopilot_runs::Entity")]
    AutopilotRuns,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl Related<super::ai_provider_keys::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::AiProviderKey.def()
    }
}

impl Related<super::autopilot_runs::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::AutopilotRuns.def()
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
