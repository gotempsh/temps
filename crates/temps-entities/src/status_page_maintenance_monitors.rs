// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The monitors a maintenance window covers (ADR 108 D2).

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "status_page_maintenance_monitors")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub maintenance_id: i32,
    #[sea_orm(primary_key, auto_increment = false)]
    pub monitor_id: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::status_page_maintenances::Entity",
        from = "Column::MaintenanceId",
        to = "super::status_page_maintenances::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Maintenance,
}

impl Related<super::status_page_maintenances::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Maintenance.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
