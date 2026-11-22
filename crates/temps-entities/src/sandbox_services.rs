// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// A service a sandbox is linked to (ADR-051): the sandbox counterpart of
/// `project_services`. No credential is stored; they are derived or issued
/// from the service whenever a command runs.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "sandbox_services")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub sandbox_id: i32,
    pub service_id: i32,
    /// The project the link was made through. It names the sandbox's database
    /// and bucket after the project; empty for a link made to a service
    /// directly.
    pub project_id: Option<i32>,
    pub created_by: Option<i32>,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::sandboxes::Entity",
        from = "Column::SandboxId",
        to = "super::sandboxes::Column::Id"
    )]
    Sandbox,
    #[sea_orm(
        belongs_to = "super::external_services::Entity",
        from = "Column::ServiceId",
        to = "super::external_services::Column::Id"
    )]
    ExternalService,
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id"
    )]
    Project,
}

impl Related<super::sandboxes::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Sandbox.def()
    }
}

impl Related<super::external_services::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ExternalService.def()
    }
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
