// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! An internal access grant (ADR 101): the target project lets a caller
//! project, or one environment of it, call its `*.temps.local` routes through
//! the internal proxy.
//!
//! `None` environment ids mean every environment of that side, present and
//! future. The database refuses self-grants and duplicates, cascades hard
//! deletes, and removes a grant when either side's project is fenced for
//! deletion or either side's environment is soft-deleted.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "internal_access_grants")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// The project being called, which owns the grant.
    pub target_project_id: i32,
    /// `None`: every environment of the target project.
    pub target_environment_id: Option<i32>,
    pub caller_project_id: i32,
    /// `None`: every environment of the caller project.
    pub caller_environment_id: Option<i32>,
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
        belongs_to = "crate::projects::Entity",
        from = "Column::CallerProjectId",
        to = "crate::projects::Column::Id",
        on_delete = "Cascade"
    )]
    CallerProject,
}

impl ActiveModelBehavior for ActiveModel {}
