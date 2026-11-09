// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Host-resolved incarnation, never an identity supplied by a guest.
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "sandbox_access_runtimes")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub project_id: i32,
    pub sandbox_id: i32,
    /// None denotes this control plane's local worker, not an arbitrary worker.
    pub node_id: Option<i32>,
    /// Immutable provider identity including its start/restore generation.
    pub provider_identity: String,
    /// None identifies an old, never-authorized runtime intent.
    pub authority_id: Option<Uuid>,
    pub provider_backend: Option<String>,
    pub provider_name: Option<String>,
    pub lease_expires_at: Option<temps_core::DBDateTime>,
    pub desired_version: i64,
    pub applied_version: i64,
    /// Closed incarnations never accept further grants.
    pub closed_at: Option<temps_core::DBDateTime>,
    pub created_at: temps_core::DBDateTime,
    pub updated_at: temps_core::DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id",
        on_delete = "Cascade"
    )]
    Project,
    #[sea_orm(
        belongs_to = "super::sandboxes::Entity",
        from = "Column::SandboxId",
        to = "super::sandboxes::Column::Id",
        on_delete = "Cascade"
    )]
    Sandbox,
    #[sea_orm(
        belongs_to = "super::nodes::Entity",
        from = "Column::NodeId",
        to = "super::nodes::Column::Id",
        on_delete = "Restrict"
    )]
    Node,
}
impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}
impl Related<super::sandboxes::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Sandbox.def()
    }
}
impl Related<super::nodes::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Node.def()
    }
}
impl ActiveModelBehavior for ActiveModel {}
