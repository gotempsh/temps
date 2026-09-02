// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "s3_sources")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub bucket_name: String,
    pub region: String,
    pub endpoint: Option<String>,
    pub bucket_path: String,
    pub access_key_id: String,
    pub secret_key: String,
    pub force_path_style: Option<bool>,
    pub is_default: bool,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    /// Managed external service that supplies this destination, when any.
    /// Backup schedules must never target this service because doing so would
    /// recursively write a backup into itself.
    pub backing_service_id: Option<i32>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::backup_schedules::Entity")]
    BackupSchedules,
    #[sea_orm(has_many = "super::backups::Entity")]
    Backups,
    #[sea_orm(
        belongs_to = "super::external_services::Entity",
        from = "Column::BackingServiceId",
        to = "super::external_services::Column::Id"
    )]
    BackingService,
}

impl Related<super::backup_schedules::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::BackupSchedules.def()
    }
}

impl Related<super::backups::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Backups.def()
    }
}

impl Related<super::external_services::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::BackingService.def()
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
