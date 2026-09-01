// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "postgres_major_upgrades")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub service_id: i32,
    pub from_version: String,
    pub to_version: String,
    pub from_image: String,
    pub to_image: String,
    /// pending | running | completed | failed | cancelled
    pub status: String,
    /// pre_backup | snapshot | dump | new_container | restore | swap | analyze | completed
    pub phase: String,
    /// FK to backups(id) — the mandatory pre-upgrade snapshot.
    /// SET NULL on backup deletion so the upgrade row survives audit history.
    pub pre_upgrade_backup_id: Option<i32>,
    /// UUID for the JSONL log file in temps-logs.
    pub log_id: String,
    /// Name of the renamed old PGDATA volume; null until swap phase.
    /// Kept for 7 days post-completion so the user can restore if the new
    /// version misbehaves in subtle ways.
    pub rollback_volume_name: Option<String>,
    pub rollback_volume_expires_at: Option<DBDateTime>,
    pub error_message: Option<String>,
    pub attempt: i32,
    pub started_at: Option<DBDateTime>,
    pub finished_at: Option<DBDateTime>,
    pub created_by: i32,
    pub created_at: DBDateTime,
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
        belongs_to = "super::backups::Entity",
        from = "Column::PreUpgradeBackupId",
        to = "super::backups::Column::Id",
        on_delete = "SetNull"
    )]
    PreUpgradeBackup,
    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::CreatedBy",
        to = "super::users::Column::Id"
    )]
    CreatedBy,
}

impl Related<super::external_services::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Service.def()
    }
}

impl Related<super::backups::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::PreUpgradeBackup.def()
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
        if insert && self.created_at.is_not_set() {
            self.created_at = Set(chrono::Utc::now());
        }
        Ok(self)
    }
}
