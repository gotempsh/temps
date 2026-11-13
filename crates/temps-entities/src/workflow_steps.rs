// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One journal entry of a workflow run (ADR-103 §3, §5, §9).
//!
//! Keyed `(run_id, sequence)`: recording the same entry twice returns the
//! first recording, which is what makes results recorded exactly once.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "workflow_steps")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub run_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub sequence: i32,
    pub name: String,
    /// `step | sleep | wait | now | random | send_event`.
    pub kind: String,
    /// `pending | completed | failed`.
    pub status: String,
    pub attempts: i32,
    #[serde(skip_serializing)]
    pub result_encrypted: Option<String>,
    pub result_blob_key: Option<String>,
    pub result_bytes: i32,
    #[serde(skip_serializing)]
    pub error_encrypted: Option<String>,
    /// Plaintext size of the error, charged to the run's journal size.
    pub error_bytes: i32,
    /// The lease that recorded the entry.
    pub lease_token: i64,
    /// The event a `wait` entry consumed.
    pub event_id: Option<i64>,
    /// A `sleep`'s wake time or a `wait`'s timeout.
    pub wake_at: Option<DBDateTime>,
    pub duration_ms: Option<i64>,
    pub trace_id: Option<String>,
    pub error_ref: Option<String>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    pub completed_at: Option<DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::workflow_runs::Entity",
        from = "Column::RunId",
        to = "super::workflow_runs::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Run,
}

impl Related<super::workflow_runs::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Run.def()
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
