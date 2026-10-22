// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "crons")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: i32,
    pub environment_id: i32,
    pub path: String,
    pub schedule: String,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    pub next_run: Option<DBDateTime>,
    pub deleted_at: Option<DBDateTime>,
    /// `http` (a GET to `path` on the running deployment, the default) or
    /// `job` (`command` run in the environment's image, ADR-055 §14). For a
    /// `job` cron, `path` holds the cron's identity key in `.temps.yaml`.
    pub mode: String,
    /// The argv a `job` cron runs. NULL for `http` crons.
    pub command: Option<Json>,
    /// `allow | forbid | replace`; what a run does while another is active.
    pub concurrency_policy: String,
    /// A run that cannot start within this many seconds of its scheduled
    /// time is skipped. NULL: always start, however late.
    pub starting_deadline_seconds: Option<i32>,
    /// Attempts after the first when a run fails.
    pub retry_limit: i32,
    /// Maximum run time. NULL takes the plane's default for jobs.
    pub timeout_seconds: Option<i32>,
    /// How long a finished run's container is kept. NULL takes the plane's
    /// default for jobs.
    pub ttl_seconds_after_finished: Option<i32>,
}

/// `crons.mode` values.
pub const MODE_HTTP: &str = "http";
pub const MODE_JOB: &str = "job";

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id"
    )]
    Project,
    #[sea_orm(
        belongs_to = "super::environments::Entity",
        from = "Column::EnvironmentId",
        to = "super::environments::Column::Id"
    )]
    Environment,
    #[sea_orm(has_many = "super::cron_executions::Entity")]
    Executions,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl Related<super::environments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Environment.def()
    }
}

impl Related<super::cron_executions::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Executions.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        let now = chrono::Utc::now();

        if insert {
            if self.created_at.is_not_set() {
                self.created_at = Set(now);
            }
            if self.updated_at.is_not_set() {
                self.updated_at = Set(now);
            }
        } else {
            self.updated_at = Set(now);
        }

        Ok(self)
    }
}
