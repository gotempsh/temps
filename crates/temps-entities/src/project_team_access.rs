// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// Which teams may access which projects, and with what role.
///
/// A project with **zero** rows here is unrestricted — every user holding
/// the relevant instance-wide permission can reach it. Adding the first
/// grant is what makes a project team-gated. See
/// `temps_teams::TeamProjectAccessChecker` for the full resolution.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "project_team_access")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: i32,
    pub team_id: i32,
    /// `owner | admin | deployer | viewer` — see [`super::TeamRole`].
    pub role: String,
    pub granted_by: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::teams::Entity",
        from = "Column::TeamId",
        to = "super::teams::Column::Id",
        on_delete = "Cascade"
    )]
    Team,
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id",
        on_delete = "Cascade"
    )]
    Project,
}

impl Related<super::teams::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Team.def()
    }
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
