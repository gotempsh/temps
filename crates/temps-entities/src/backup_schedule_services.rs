// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Join table linking a backup schedule to the external services it targets.
//!
//! See migration `m20260519_000001_create_backup_schedule_services` for the
//! schema rationale.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "backup_schedule_services")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub schedule_id: i32,
    #[sea_orm(primary_key, auto_increment = false)]
    pub service_id: i32,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::backup_schedules::Entity",
        from = "Column::ScheduleId",
        to = "super::backup_schedules::Column::Id"
    )]
    Schedule,
    #[sea_orm(
        belongs_to = "super::external_services::Entity",
        from = "Column::ServiceId",
        to = "super::external_services::Column::Id"
    )]
    Service,
}

impl Related<super::backup_schedules::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Schedule.def()
    }
}

impl Related<super::external_services::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Service.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
