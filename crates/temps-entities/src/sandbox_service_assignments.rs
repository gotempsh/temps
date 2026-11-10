// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use sea_orm::entity::prelude::*;
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "sandbox_service_assignments")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub project_id: i32,
    pub sandbox_id: i32,
    pub service_id: String,
    pub service_version: i32,
    pub requested_by: Option<i32>,
    pub approved_by: Option<i32>,
    pub approved_at: Option<temps_core::DBDateTime>,
    /// None means sandbox lifetime. Only explicit approval can change this.
    pub expires_at: Option<temps_core::DBDateTime>,
    pub revoked_at: Option<temps_core::DBDateTime>,
    pub version: i32,
    pub grant_id: Option<String>,
    /// None removes terminal approvals from the due-work range without deleting history.
    pub next_reconcile_at: Option<temps_core::DBDateTime>,
    pub created_at: temps_core::DBDateTime,
    pub updated_at: temps_core::DBDateTime,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::sandbox_access_services::Entity",
        from = "Column::ServiceId",
        to = "super::sandbox_access_services::Column::Id"
    )]
    Service,
}
impl Related<super::sandbox_access_services::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Service.def()
    }
}
impl ActiveModelBehavior for ActiveModel {}
