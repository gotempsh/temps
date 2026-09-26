// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// Durable, queryable state for exporting a local backup to one Cloud tenant.
///
/// The local `backups` row remains authoritative. This table only records the
/// progress of the optional Cloud copy so discovery never needs to scan or
/// cast legacy free-form backup metadata.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "cloud_backup_mirror_states")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub backup_id: i32,
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    pub schema_version: i32,
    pub outcome: String,
    pub classification: String,
    pub reason: Option<String>,
    /// Consecutive transient attempts, used to preserve exponential backoff
    /// across process restarts. Terminal outcomes reset this to zero.
    pub attempt_count: i32,
    pub retry_after: Option<DBDateTime>,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::backups::Entity",
        from = "Column::BackupId",
        to = "super::backups::Column::Id",
        on_delete = "Cascade"
    )]
    Backup,
}

impl Related<super::backups::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Backup.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
