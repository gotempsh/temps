// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// A named group of users. Teams are the unit project access is granted
/// to — see [`super::project_team_access`].
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "teams")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    #[sea_orm(unique)]
    pub slug: String,
    pub description: Option<String>,
    pub created_by: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::team_members::Entity")]
    TeamMembers,
    #[sea_orm(has_many = "super::project_team_access::Entity")]
    ProjectAccess,
}

impl Related<super::team_members::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::TeamMembers.def()
    }
}

impl Related<super::project_team_access::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ProjectAccess.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
