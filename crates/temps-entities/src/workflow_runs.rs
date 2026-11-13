// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One durable workflow run (ADR-103 §3, §5).
//!
//! Scheduling state and plaintext metadata live here; the input, output and
//! error are ciphertext (`*_encrypted`, or offloaded to `*_blob_key`) bound
//! by associated data to this run and field.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "workflow_runs")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub project_id: i32,
    pub environment_id: i32,
    pub workflow_name: String,
    /// `queued | running | sleeping | waiting | retrying | completed | failed
    /// | cancelled | stuck`.
    pub status: String,
    #[serde(skip_serializing)]
    pub input_encrypted: Option<String>,
    pub input_blob_key: Option<String>,
    pub input_bytes: i32,
    #[serde(skip_serializing)]
    pub output_encrypted: Option<String>,
    pub output_blob_key: Option<String>,
    pub output_bytes: i32,
    #[serde(skip_serializing)]
    pub error_encrypted: Option<String>,
    /// Plaintext category of the failure (`step_failed`, `incompatible`,
    /// `invocation_failed`, ...); the message itself is a payload.
    pub error_kind: Option<String>,
    pub idempotency_key: Option<String>,
    pub parent_run_id: Option<Uuid>,
    pub started_by_credential_id: Option<i32>,
    pub started_by_user_id: Option<i32>,
    /// The deployment current when the run started.
    pub deployment_id: Option<i32>,
    /// Invocations so far.
    pub attempt: i32,
    /// Fencing token: incremented on every claim; a journal write carrying an
    /// older one is rejected.
    pub lease_token: i64,
    pub lease_expires_at: Option<DBDateTime>,
    /// When the run is next runnable; `NULL` while waiting for an event
    /// without a timeout, or when terminal.
    pub due_at: Option<DBDateTime>,
    pub awaited_event_name: Option<String>,
    pub awaited_event_key: Option<String>,
    pub awaited_sequence: Option<i32>,
    pub consecutive_failures: i32,
    /// Why a due run is not being invoked (no healthy replica, ...).
    pub blocked_reason: Option<String>,
    pub journal_bytes: i64,
    pub step_count: i32,
    pub has_blob_payloads: bool,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    pub started_at: Option<DBDateTime>,
    pub finished_at: Option<DBDateTime>,
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
    #[sea_orm(has_many = "super::workflow_steps::Entity")]
    Steps,
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

impl Related<super::workflow_steps::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Steps.def()
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
