// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "autopilot_runs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: i32,
    pub config_id: i32,
    pub trigger_type: String,
    pub trigger_source_id: Option<i32>,
    pub trigger_source_type: Option<String>,
    pub status: String,
    pub branch_name: Option<String>,
    pub commit_sha: Option<String>,
    pub pr_url: Option<String>,
    pub pr_number: Option<i32>,
    pub preview_url: Option<String>,
    pub preview_deployment_id: Option<i32>,
    pub error_message: Option<String>,
    pub ai_output: Option<String>,
    pub ai_reasoning: Option<String>,
    pub ai_model: Option<String>,
    pub tokens_input: i32,
    pub tokens_output: i32,
    pub estimated_cost_cents: i32,
    pub files_changed: i32,
    pub started_at: Option<DBDateTime>,
    pub completed_at: Option<DBDateTime>,
    pub created_at: DBDateTime,
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
        belongs_to = "super::autopilot_configs::Entity",
        from = "Column::ConfigId",
        to = "super::autopilot_configs::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    AutopilotConfig,
    #[sea_orm(has_many = "super::autopilot_run_logs::Entity")]
    AutopilotRunLogs,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl Related<super::autopilot_configs::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::AutopilotConfig.def()
    }
}

impl Related<super::autopilot_run_logs::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::AutopilotRunLogs.def()
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

        Ok(self)
    }
}
