// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// An application workspace's durable authorization to use one synced Git repository.
/// Credentials remain owned by `git_provider_connections`; this row stores only its opaque id.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "ai_application_git_bindings")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub application_id: i64,
    pub project_id: i32,
    pub connection_id: i32,
    pub repository_id: i32,
    pub repository_url: String,
    pub remote_name: String,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
