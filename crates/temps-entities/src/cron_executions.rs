// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "cron_executions")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub cron_id: i32,
    pub executed_at: DBDateTime,
    pub url: String,
    pub status_code: i32,
    pub headers: String,
    pub response_time_ms: i32,
    pub error_message: Option<String>,
    /// The workload a `job` run executes as. NULL for `http` runs.
    pub workload_id: Option<i64>,
    /// `running | succeeded | failed | skipped` for `job` runs; NULL for
    /// `http` runs, whose outcome is `status_code`.
    pub state: Option<String>,
    /// Exit code of the run's last attempt.
    pub exit_code: Option<i32>,
    /// Attempts made (1 + retries).
    pub attempt: Option<i32>,
    pub finished_at: Option<DBDateTime>,
    /// The last lines of the run's output, bounded.
    pub output_tail: Option<String>,
}

/// `cron_executions.state` values for `job` runs.
pub const STATE_RUNNING: &str = "running";
pub const STATE_SUCCEEDED: &str = "succeeded";
pub const STATE_FAILED: &str = "failed";
pub const STATE_SKIPPED: &str = "skipped";

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::crons::Entity",
        from = "Column::CronId",
        to = "super::crons::Column::Id"
    )]
    Cron,
}

impl Related<super::crons::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Cron.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
