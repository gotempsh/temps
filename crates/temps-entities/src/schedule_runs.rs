// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! SeaORM entity for the `schedule_runs` table.
//!
//! One row per scheduler tick (cron-triggered) or "Run now" click (manual).
//! Child `backups` rows created during the fan-out are linked via
//! `backups.schedule_run_id`. The aggregate state of the run is computed
//! at read time from the child backup states — no `aggregate_state` column
//! is stored here to avoid sync-bug surface area.
//!
//! Migration: `m20260516_000001_create_schedule_runs`.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "schedule_runs")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    /// FK to `backup_schedules.id`. `ON DELETE CASCADE`.
    pub schedule_id: i32,
    /// How the run was triggered: `"cron"` for scheduled ticks,
    /// `"manual"` for "Run now" API calls.
    pub triggered_by: String,
    /// FK to `users.id` when `triggered_by = "manual"`. `NULL` for cron runs.
    /// `ON DELETE SET NULL`.
    pub triggered_by_user_id: Option<i32>,
    /// When the run row was inserted (= when fan-out started).
    pub started_at: DBDateTime,
    /// When all child backups reached a terminal state. `NULL` while any
    /// child is still `"pending"` or `"running"`. Written by the
    /// `mark_schedule_run_finished_if_done` helper in `temps-backup-core`.
    pub finished_at: Option<DBDateTime>,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::backup_schedules::Entity",
        from = "Column::ScheduleId",
        to = "super::backup_schedules::Column::Id"
    )]
    BackupSchedule,
    #[sea_orm(has_many = "super::backups::Entity")]
    Backups,
}

impl Related<super::backup_schedules::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::BackupSchedule.def()
    }
}

impl Related<super::backups::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Backups.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
