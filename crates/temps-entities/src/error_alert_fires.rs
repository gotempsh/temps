// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "error_alert_fires")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub rule_id: i32,
    pub error_group_id: i32,
    pub fired_at: DBDateTime,
    pub notification_sent: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::error_alert_rules::Entity",
        from = "Column::RuleId",
        to = "super::error_alert_rules::Column::Id",
        on_delete = "Cascade"
    )]
    ErrorAlertRules,
    #[sea_orm(
        belongs_to = "super::error_groups::Entity",
        from = "Column::ErrorGroupId",
        to = "super::error_groups::Column::Id",
        on_delete = "Cascade"
    )]
    ErrorGroups,
}

impl Related<super::error_alert_rules::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ErrorAlertRules.def()
    }
}

impl Related<super::error_groups::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ErrorGroups.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
