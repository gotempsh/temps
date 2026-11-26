// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The check interval a monitor had before each change of it.
//!
//! Uptime coverage reads this so a completed window keeps the coverage it had
//! when it ended: a check is credited with the interval it ran under, which
//! is the `previous_interval_seconds` of the first change after it, or the
//! monitor's current interval when nothing changed since.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "status_monitor_interval_changes")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub monitor_id: i32,
    /// The normalised interval in effect up to `changed_at`.
    pub previous_interval_seconds: i32,
    /// When the new interval took effect.
    pub changed_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::status_monitors::Entity",
        from = "Column::MonitorId",
        to = "super::status_monitors::Column::Id",
        on_delete = "Cascade"
    )]
    StatusMonitor,
}

impl Related<super::status_monitors::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::StatusMonitor.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
