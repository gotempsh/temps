// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-environment durable workflow settings (ADR-106 §2, §5).
//!
//! One optional row per environment; an environment without one uses the
//! column defaults (mirrored by `WorkflowSettings::default()` in
//! `temps-workflows`). Every knob is a column so it can be changed per
//! environment through the API, audited, without a restart.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "workflow_environment_settings")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub environment_id: i32,
    pub project_id: i32,
    /// Compose service that hosts the workflow handler. `NULL`: the
    /// environment's main (single-container) service.
    pub host_service: Option<String>,
    pub drain_timeout_secs: i32,
    /// Longest one invocation may run before it is abandoned. Default 900.
    pub invocation_deadline_secs: i32,
    /// Lease length; heartbeats and journal writes extend it. Default 120.
    pub lease_secs: i32,
    /// Runs of this environment executing at once; more stay queued. Default 50.
    pub max_concurrent_runs: i32,
    /// Non-terminal runs allowed; a start beyond it is rejected. Default 10000.
    pub max_active_runs: i32,
    /// Journal entries one run may record. Default 1000.
    pub max_steps_per_run: i32,
    /// Payload bytes one run's journal may hold. Default 64 MiB.
    pub max_journal_bytes_per_run: i64,
    /// Payloads up to this size are stored inline; larger ones go to blob
    /// storage. Default 256 KiB.
    pub inline_payload_limit_bytes: i32,
    /// Largest payload accepted at all. Default 4 MiB.
    pub max_payload_bytes: i32,
    /// Finished runs and events older than this are deleted. Default 30.
    pub retention_days: i32,
    /// Consecutive invocation failures or lease expiries before a run is
    /// marked `stuck`. Default 8.
    pub stuck_after_failures: i32,
    /// Reserved for the opt-in managed Postgres journal (ADR-106 §5, M3).
    pub managed_store_service_id: Option<i32>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::environments::Entity",
        from = "Column::EnvironmentId",
        to = "super::environments::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Environment,
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Project,
}

impl Related<super::environments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Environment.def()
    }
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
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
