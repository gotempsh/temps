// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "static_asset_cache")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// URL path as the browser requests it (e.g., "_next/static/chunks/main-abc123.js")
    pub url_path: String,
    /// SHA-256 content hash pointing to the blob in the CAS
    pub content_hash: String,
    /// Project this asset belongs to
    pub project_id: i32,
    /// Environment this asset belongs to
    pub environment_id: i32,
    /// Deployment that produced this asset
    pub deployment_id: i32,
    /// File size in bytes
    pub size_bytes: i64,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
