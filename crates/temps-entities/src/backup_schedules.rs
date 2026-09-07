// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "backup_schedules")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub backup_type: String,
    pub retention_period: i32,
    pub s3_source_id: i32,
    pub schedule_expression: String,
    pub enabled: bool,
    pub last_run: Option<DBDateTime>,
    pub next_run: Option<DBDateTime>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    pub description: Option<String>,
    pub tags: String,
    /// Optional per-schedule wall-clock timeout override (seconds).
    ///
    /// `None` means "use the engine default."
    pub max_runtime_secs: Option<i64>,
    /// When `true`, fan-out targets every external service on the host
    /// (auto-including future databases). When `false`, fan-out targets
    /// only the services attached via `backup_schedule_services`. Default
    /// is `true` so a fresh schedule "just backs up everything."
    #[sea_orm(default_value = true)]
    pub target_all_services: bool,
    /// When `true`, every run also produces a `control_plane` backup
    /// (Temps's own Postgres). When `false`, only the external service
    /// fan-out happens — useful when the operator scopes a schedule to a
    /// single DB and doesn't want the control plane lumped in.
    #[sea_orm(default_value = true)]
    pub include_control_plane: bool,
    /// Stable machine-readable marker for schedules created by Temps.
    /// User-created schedules remain `None` and are never lifecycle-managed.
    pub generated_kind: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::s3_sources::Entity",
        from = "Column::S3SourceId",
        to = "super::s3_sources::Column::Id"
    )]
    S3Source,
    #[sea_orm(has_many = "super::backup_schedule_services::Entity")]
    Services,
}

impl Related<super::s3_sources::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::S3Source.def()
    }
}

impl Related<super::backup_schedule_services::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Services.def()
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
