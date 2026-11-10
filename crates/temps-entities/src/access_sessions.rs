// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "access_sessions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub project_id: Option<i32>,
    pub vault_id: String,
    pub link_id: Option<String>,
    pub bundle_id: String,
    pub bundle_name: String,
    pub bundle_version: i32,
    pub consumer_kind: String,
    pub consumer_id: String,
    pub sandbox_id: i32,
    pub created_by: Option<i32>,
    pub consent: Json,
    pub grant_ids: Vec<String>,
    pub created_at: temps_core::DBDateTime,
    pub expires_at: temps_core::DBDateTime,
    pub revoked_at: Option<temps_core::DBDateTime>,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::sandboxes::Entity",
        from = "Column::SandboxId",
        to = "super::sandboxes::Column::Id"
    )]
    Sandbox,
}
impl Related<super::sandboxes::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Sandbox.def()
    }
}
impl ActiveModelBehavior for ActiveModel {}
