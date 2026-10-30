// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A deleted project whose telemetry rows are still being removed.
//!
//! Inserted in the same transaction that deletes the `projects` row; the
//! project data purger removes the project's rows from the high-volume
//! telemetry tables and then sets `completed_at`.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "project_data_purges")]
pub struct Model {
    /// The deleted project's id. No foreign key: the project row is gone.
    #[sea_orm(primary_key, auto_increment = false)]
    pub project_id: i32,
    pub project_name: String,
    pub requested_at: DateTimeUtc,
    pub next_attempt_at: DateTimeUtc,
    pub lease_until: Option<DateTimeUtc>,
    pub passes: i32,
    pub consecutive_failures: i32,
    pub rows_deleted: i64,
    pub last_error: Option<String>,
    pub completed_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
