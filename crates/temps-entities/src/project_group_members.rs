// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// Places a project inside a [`super::project_groups`] row. `project_id` is
/// the primary key, so a project belongs to at most one group.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "project_group_members")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub project_id: i32,
    pub group_id: i32,
    /// Display order within the group (ascending).
    pub position: i32,
    /// Whether the project follows the group's repository and branch; the
    /// group writes them through to the project's own git settings.
    pub inherits_repository: bool,
    pub added_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::project_groups::Entity",
        from = "Column::GroupId",
        to = "super::project_groups::Column::Id",
        on_delete = "Cascade"
    )]
    Group,
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id",
        on_delete = "Cascade"
    )]
    Project,
}

impl Related<super::project_groups::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Group.def()
    }
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
