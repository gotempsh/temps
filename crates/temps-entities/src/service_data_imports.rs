// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// One "import data from an external database" run: the contents of a
/// database on a server outside Temps are copied into a database of a
/// managed service. Engine-agnostic — PostgreSQL, MariaDB and MongoDB all
/// record their runs here.
///
/// The source connection string is never stored: `source_display` is the
/// same string with its credentials masked.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "service_data_imports")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// Service the data is imported into. Cascade-deleted with it.
    pub service_id: i32,
    /// `external_services.service_type` at the time of the run.
    pub service_type: String,
    /// Database inside the service that receives the data.
    pub target_database: String,
    /// Source connection string with user and password masked.
    pub source_display: String,
    /// Database read on the source server.
    pub source_database: String,
    /// Whether an existing, non-empty target database was dropped first.
    pub replace_existing: bool,
    /// Whether the engine applies the copy in a single transaction, so a
    /// failed or interrupted run leaves no imported data behind.
    pub atomic_transfer: bool,
    /// "running" | "succeeded" | "failed" | "cancelled" | "interrupted".
    /// `interrupted` is written at startup for runs whose owning process died.
    pub status: String,
    /// "preparing_target" | "transferring" | "verifying" | "finished".
    /// Only a successful run reaches "finished"; any other ending keeps the
    /// phase the run stopped in.
    pub phase: String,
    /// Name of the helper container doing the transfer, once started. Used
    /// to cancel the run and to fence it after a restart.
    pub helper_container: Option<String>,
    /// Why the run failed, in one or two sentences, with every secret
    /// scrubbed out.
    pub error_message: Option<String>,
    /// Last lines the transfer printed (dump/restore tool output), scrubbed
    /// and bounded. Kept for successful runs too.
    pub helper_output: Option<String>,
    /// Tables/collections in the target after a successful import.
    pub target_object_count: Option<i64>,
    /// Size of the target database after a successful import.
    pub target_size_bytes: Option<i64>,
    /// Hard limit on the transfer, in seconds.
    pub timeout_seconds: i32,
    pub created_by: Option<i32>,
    pub cancel_requested_at: Option<DBDateTime>,
    pub started_at: DBDateTime,
    pub finished_at: Option<DBDateTime>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::external_services::Entity",
        from = "Column::ServiceId",
        to = "super::external_services::Column::Id",
        on_delete = "Cascade"
    )]
    Service,
    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::CreatedBy",
        to = "super::users::Column::Id",
        on_delete = "SetNull"
    )]
    CreatedBy,
}

impl Related<super::external_services::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Service.def()
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
        if insert && self.started_at.is_not_set() {
            self.started_at = Set(now);
        }
        self.updated_at = Set(now);
        Ok(self)
    }
}
