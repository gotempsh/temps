// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// An optional parent that gathers several projects into one unit (e.g. a
/// frontend, an API and a worker). Membership lives in
/// [`super::project_group_members`]; a project with no membership row is
/// ungrouped.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "project_groups")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    #[sea_orm(unique)]
    pub slug: String,
    pub description: Option<String>,
    /// Repository members can inherit (monorepos). `repo_owner`, `repo_name`
    /// and `main_branch` are set together or not at all; the connection can
    /// be missing on its own after the git connection was deleted.
    pub git_provider_connection_id: Option<i32>,
    pub repo_owner: Option<String>,
    pub repo_name: Option<String>,
    pub main_branch: Option<String>,
    /// `github` or `gitlab` when the repository is public and read without a
    /// connection; never set together with `git_provider_connection_id`.
    pub public_provider: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::project_group_members::Entity")]
    Members,
}

impl Related<super::project_group_members::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Members.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
