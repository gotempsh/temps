// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// A single restore operation run by the generic RestoreOrchestrator.
/// Covers every engine (Postgres, Redis, MongoDB, S3/RustFS) so the UI
/// and API can observe progress uniformly.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "restore_runs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// FK to `backups(id)`. RESTRICT on delete so you can't delete a backup
    /// that is (or was) the source of a restore history entry.
    pub source_backup_id: i32,
    /// The service whose backup is being restored. For `in_place`, this is
    /// also the target. For `new_service`/`pitr`, this is the template.
    pub source_service_id: i32,
    /// Populated for `new_service`/`pitr` modes once the fresh service row
    /// is created by the orchestrator. Null for `in_place`.
    /// SetNull on delete preserves restore history after the new service
    /// is decommissioned.
    pub target_service_id: Option<i32>,
    /// Desired name for the new service (only relevant for new_service/pitr
    /// modes). Captured at request time so we can retry with the same name.
    pub target_service_name: Option<String>,
    /// "in_place" | "new_service" | "pitr"
    pub mode: String,
    /// "pending" | "running" | "completed" | "failed" | "cancelled"
    pub status: String,
    /// "prepare" | "provision" | "restore" | "recover" | "verify" | "completed" | "failed"
    pub phase: String,
    /// PITR target serialized as JSON (Time | Xid | Lsn | Name).
    /// Null for non-PITR modes.
    pub recovery_target: Option<serde_json::Value>,
    /// Optional parameter overrides for the new service (port, memory, etc.).
    /// For new_service mode; defaults to `{}`.
    pub parameter_overrides: serde_json::Value,
    /// Engine-specific resume state so a failed run can pick up from the
    /// last completed sub-step (e.g., WAL-G backup name, partial object keys).
    pub resume_token: Option<serde_json::Value>,
    /// UUID for the JSONL log stream in temps-logs.
    pub log_id: String,
    pub error_message: Option<String>,
    pub attempt: i32,
    pub started_at: Option<DBDateTime>,
    pub finished_at: Option<DBDateTime>,
    pub created_by: i32,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::backups::Entity",
        from = "Column::SourceBackupId",
        to = "super::backups::Column::Id",
        on_delete = "Restrict"
    )]
    SourceBackup,
    #[sea_orm(
        belongs_to = "super::external_services::Entity",
        from = "Column::SourceServiceId",
        to = "super::external_services::Column::Id",
        on_delete = "Cascade"
    )]
    SourceService,
    #[sea_orm(
        belongs_to = "super::external_services::Entity",
        from = "Column::TargetServiceId",
        to = "super::external_services::Column::Id",
        on_delete = "SetNull"
    )]
    TargetService,
    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::CreatedBy",
        to = "super::users::Column::Id"
    )]
    CreatedBy,
}

impl Related<super::backups::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::SourceBackup.def()
    }
}

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::CreatedBy.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        let now = chrono::Utc::now();
        if insert && self.created_at.is_not_set() {
            self.created_at = Set(now);
        }
        // Always bump updated_at — callers that want to preserve it can
        // explicitly Set() before save, though no caller currently does.
        self.updated_at = Set(now);
        Ok(self)
    }
}
