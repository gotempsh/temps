// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sea-ORM entity for the `backup_alerts` table.
//!
//! Backup alerts represent two failure modes that are otherwise invisible:
//!
//! - `overdue_schedule`: a `backup_schedules` row has `enabled=true` and
//!   `next_run < NOW() - 1h`. The scheduler never fired.
//! - `stalled_job`: a `backups` row has `state='pending'` and
//!   `created_at < NOW() - 1h`. The queue consumer never dispatched it.
//!
//! Rows are inserted by `temps_backup::services::alerts::sweep_backup_alerts`
//! and automatically resolved when the condition clears.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// A single open or resolved backup alert.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "backup_alerts")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// `"overdue_schedule"` or `"stalled_job"`.
    pub kind: String,
    /// FK to `backup_schedules.id`. Set for `overdue_schedule` alerts; `None`
    /// for `stalled_job` alerts.
    pub schedule_id: Option<i32>,
    /// `"warning"` (default) or `"critical"` (condition persisted > 6 hours).
    pub severity: String,
    /// Human-readable description surfaced in the UI banner.
    pub message: String,
    /// Timestamp when the alert was opened by the watcher.
    pub opened_at: DBDateTime,
    /// `None` while the alert is open. Set to the resolution timestamp by the
    /// watcher when the triggering condition clears.
    pub resolved_at: Option<DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::backup_schedules::Entity",
        from = "Column::ScheduleId",
        to = "super::backup_schedules::Column::Id"
    )]
    BackupSchedule,
}

impl Related<super::backup_schedules::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::BackupSchedule.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
