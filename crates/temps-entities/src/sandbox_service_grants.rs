// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;

// Internal storage only: never serialize this model into a response or audit.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "sandbox_service_grants")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub project_id: i32,
    pub service_id: String,
    pub assignment_id: Option<String>,
    pub sandbox_id: i32,
    pub service_version: i32,
    pub policy_snapshot: Json,
    pub created_by: Option<i32>,
    pub version: i32,
    pub created_at: temps_core::DBDateTime,
    pub expires_at: temps_core::DBDateTime,
    pub revoked_at: Option<temps_core::DBDateTime>,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Project,
    #[sea_orm(
        belongs_to = "super::sandbox_access_services::Entity",
        from = "Column::ServiceId",
        to = "super::sandbox_access_services::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Service,
}
impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}
impl ActiveModelBehavior for ActiveModel {}

impl Related<super::sandbox_access_services::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Service.def()
    }
}
