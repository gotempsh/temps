// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "error_alert_rules")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: i32,
    pub name: String,
    pub trigger_type: String,
    pub trigger_config: serde_json::Value,
    pub environment_filter: Option<i32>,
    pub error_level_filter: Option<String>,
    pub notification_priority: String,
    pub cooldown_minutes: i32,
    pub enabled: bool,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id",
        on_delete = "Cascade"
    )]
    Projects,
    #[sea_orm(has_many = "super::error_alert_fires::Entity")]
    ErrorAlertFires,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Projects.def()
    }
}

impl Related<super::error_alert_fires::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ErrorAlertFires.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
