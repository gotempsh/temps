// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Uploaded source archives used by Git-less preset builds.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "source_bundles")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: i32,
    pub archive_path: String,
    pub original_filename: Option<String>,
    pub content_type: String,
    pub size_bytes: i64,
    pub checksum: String,
    pub directory: String,
    pub preset: String,
    pub metadata: Option<serde_json::Value>,
    pub uploaded_at: DBDateTime,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id"
    )]
    Project,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        if insert {
            let now = chrono::Utc::now();
            if self.created_at.is_not_set() {
                self.created_at = Set(now);
            }
            if self.uploaded_at.is_not_set() {
                self.uploaded_at = Set(now);
            }
        }
        Ok(self)
    }
}
