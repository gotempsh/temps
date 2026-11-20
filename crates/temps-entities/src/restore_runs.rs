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
/// `Debug` is written out below so the recorded login stays out of logs.
#[derive(Clone, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
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
    /// "pending" | "running" | "completed" | "failed" | "cancelled" | "interrupted".
    /// `interrupted` is written by startup reconciliation when the worker that
    /// owned the run died with the previous Temps process; `phase` then keeps
    /// the phase the run was in.
    pub status: String,
    /// "prepare" | "download" | "provision" | "restore" | "recover" | "verify" |
    /// "completed" | "failed". A `cancelled` or `interrupted` run keeps the
    /// phase it stopped in.
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
    /// UUID for the JSONL log stream in temps-deployment-logs.
    pub log_id: String,
    pub error_message: Option<String>,
    pub attempt: i32,
    pub started_at: Option<DBDateTime>,
    pub finished_at: Option<DBDateTime>,
    pub created_by: i32,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    /// When a user asked to cancel the run. Recorded only while the run is in
    /// a cancellable phase; the worker's move out of that phase is
    /// conditional on this being NULL, so the two cannot both win.
    pub cancel_requested_at: Option<DBDateTime>,
    /// Who asked to cancel the run.
    pub cancel_requested_by: Option<i32>,
    /// The login a worker restore's data answers to, captured before the
    /// restore was sent and encrypted with `EncryptionService`: what the
    /// target's stored login is patched to when the run settles, also after
    /// a restart. `NULL` when no worker restore recorded one. Never
    /// serialized nor debug-printed: it is a credential, sealed or not.
    #[serde(skip_serializing, default)]
    pub restored_login_encrypted: Option<String>,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestoreRun")
            .field("id", &self.id)
            .field("source_backup_id", &self.source_backup_id)
            .field("source_service_id", &self.source_service_id)
            .field("target_service_id", &self.target_service_id)
            .field("target_service_name", &self.target_service_name)
            .field("mode", &self.mode)
            .field("status", &self.status)
            .field("phase", &self.phase)
            .field("recovery_target", &self.recovery_target)
            .field("parameter_overrides", &self.parameter_overrides)
            .field("resume_token", &self.resume_token)
            .field("log_id", &self.log_id)
            .field("error_message", &self.error_message)
            .field("attempt", &self.attempt)
            .field("started_at", &self.started_at)
            .field("finished_at", &self.finished_at)
            .field("created_by", &self.created_by)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .field("cancel_requested_at", &self.cancel_requested_at)
            .field("cancel_requested_by", &self.cancel_requested_by)
            .field(
                "has_restored_login",
                &self.restored_login_encrypted.is_some(),
            )
            .finish()
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    const SEALED: &str = "sealed-login-ciphertext-1f2e3d";

    fn run() -> Model {
        Model {
            id: 7,
            source_backup_id: 3,
            source_service_id: 4,
            target_service_id: None,
            target_service_name: None,
            mode: "in_place".into(),
            status: "running".into(),
            phase: "restore".into(),
            recovery_target: None,
            parameter_overrides: serde_json::json!({}),
            resume_token: None,
            log_id: "log-1".into(),
            error_message: None,
            attempt: 1,
            started_at: None,
            finished_at: None,
            created_by: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            cancel_requested_at: None,
            cancel_requested_by: None,
            restored_login_encrypted: Some(SEALED.into()),
        }
    }

    /// The sealed login a worker restore recorded never leaves through the
    /// model's serialized form or its debug output.
    #[test]
    fn the_recorded_login_is_neither_serialized_nor_debug_printed() {
        let run = run();
        let json = serde_json::to_string(&run).expect("serialize");
        assert!(!json.contains(SEALED), "serialized: {json}");
        assert!(
            !json.contains("restored_login_encrypted"),
            "serialized: {json}"
        );
        let debug = format!("{run:?}");
        assert!(!debug.contains(SEALED), "debug: {debug}");
        assert!(debug.contains("has_restored_login: true"), "debug: {debug}");
    }
}
