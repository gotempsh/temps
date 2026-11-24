// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "access_vault_links")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub vault_id: String,
    pub project_id: Option<i32>,
    pub sandbox_id: Option<i32>,
    pub created_at: temps_core::DBDateTime,
    pub revoked_at: Option<temps_core::DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::access_vaults::Entity",
        from = "Column::VaultId",
        to = "super::access_vaults::Column::Id"
    )]
    Vault,
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id"
    )]
    Project,
    #[sea_orm(
        belongs_to = "super::sandboxes::Entity",
        from = "Column::SandboxId",
        to = "super::sandboxes::Column::Id"
    )]
    Sandbox,
}
impl Related<super::access_vaults::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Vault.def()
    }
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
impl ActiveModelBehavior for ActiveModel {}
