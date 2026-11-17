// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where a volume's owner mounts it (ADR-055 §12). One path per volume.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "workload_volume_attachments")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub volume_id: i64,
    /// Absolute path inside the instance.
    pub mount_path: String,
    pub read_only: bool,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::workload_volumes::Entity",
        from = "Column::VolumeId",
        to = "super::workload_volumes::Column::Id",
        on_delete = "Cascade"
    )]
    Volume,
}

impl Related<super::workload_volumes::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Volume.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
