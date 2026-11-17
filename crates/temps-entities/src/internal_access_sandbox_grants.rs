// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! An internal access grant with a sandbox caller (ADR 105): the target
//! project lets one sandbox call its `*.temps.local` routes through the
//! internal proxy.
//!
//! A `None` target environment means every environment of the target
//! project, present and future. The database refuses duplicates, cascades
//! hard deletes, and removes a grant when its sandbox is destroyed, its
//! project is fenced for deletion or its environment is soft-deleted.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "internal_access_sandbox_grants")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// The project being called, which owns the grant.
    pub target_project_id: i32,
    /// `None`: every environment of the target project.
    pub target_environment_id: Option<i32>,
    /// `sandboxes.id` of the caller (never exposed; responses name the
    /// sandbox by its public id).
    pub sandbox_id: i32,
    pub description: Option<String>,
    pub created_by: Option<i32>,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "crate::projects::Entity",
        from = "Column::TargetProjectId",
        to = "crate::projects::Column::Id",
        on_delete = "Cascade"
    )]
    TargetProject,
    #[sea_orm(
        belongs_to = "crate::sandboxes::Entity",
        from = "Column::SandboxId",
        to = "crate::sandboxes::Column::Id",
        on_delete = "Cascade"
    )]
    Sandbox,
}

impl ActiveModelBehavior for ActiveModel {}
