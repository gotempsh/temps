// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Explicit fresh authorization, never backfilled from historical intentions.
use sea_orm::entity::prelude::*;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "sandbox_access_authorizations")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub grant_id: String,
    pub request_id: Uuid,
    pub requested_ttl: i32,
    pub runtime_id: Uuid,
    pub project_id: Option<i32>,
    pub sandbox_id: i32,
    pub approved_by: Option<i32>,
    pub approved_at: temps_core::DBDateTime,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::sandbox_service_grants::Entity",
        from = "Column::GrantId",
        to = "super::sandbox_service_grants::Column::Id",
        on_delete = "Cascade"
    )]
    Grant,
    #[sea_orm(
        belongs_to = "super::sandbox_access_runtimes::Entity",
        from = "Column::RuntimeId",
        to = "super::sandbox_access_runtimes::Column::Id",
        on_delete = "Cascade"
    )]
    Runtime,
}
impl Related<super::sandbox_service_grants::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Grant.def()
    }
}
impl Related<super::sandbox_access_runtimes::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Runtime.def()
    }
}
impl ActiveModelBehavior for ActiveModel {}
