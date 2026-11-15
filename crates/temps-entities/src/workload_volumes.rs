// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A volume (ADR-055 §12): a first-class resource owned by a product object
//! (`environment/3`, `external_service/7`). Only `temps-workload-plane`
//! converts rows to `temps_workload_volumes::Volume`.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "workload_volumes")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub owner_kind: String,
    pub owner_id: i64,
    /// Unique per owner among live volumes; what a mount names.
    pub name: String,
    /// `named`, `block_image` or `preopened_dir`.
    pub backing: String,
    /// The backing's own name: the Docker volume name, or a path.
    pub backing_ref: String,
    /// The node holding the data. `None` until first created; `0` is the
    /// control plane.
    pub node_id: Option<i32>,
    pub size_mb: Option<i64>,
    /// Keep the backing when the volume (or its owner) is deleted.
    pub retain_on_delete: bool,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    pub deleted_at: Option<DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::workload_volume_attachments::Entity")]
    Attachments,
}

impl Related<super::workload_volume_attachments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Attachments.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
