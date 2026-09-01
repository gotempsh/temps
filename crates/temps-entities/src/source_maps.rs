// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "source_maps")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,

    pub project_id: i32,

    /// Release version this source map belongs to (e.g., "1.0.0", "deploy-abc123")
    pub release: String,

    /// The URL path of the minified file as it appears in stack traces.
    /// Uses the ~ prefix convention (e.g., "~/assets/index-a1b2c3.js")
    pub file_path: String,

    /// Raw source map content (the .map file bytes)
    #[serde(skip_serializing)]
    pub source_map_data: Vec<u8>,

    /// Optional distribution identifier for distinguishing builds within the same release
    pub dist: Option<String>,

    /// Size of the source map in bytes
    pub size_bytes: i64,

    /// SHA256 checksum of the source map data
    pub checksum: Option<String>,

    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Projects,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Projects.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
