// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "access_vaults")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub project_id: Option<i32>,
    pub name: String,
    pub retired_at: Option<temps_core::DBDateTime>,
    pub service_ids: Vec<String>,
    /// Existing approvals before this upgrade instant may use original project links.
    pub legacy_approval_before: Option<temps_core::DBDateTime>,
    /// Original membership, captured before the first post-upgrade membership edit.
    pub legacy_service_ids: Option<Vec<String>>,
    /// Removed members cannot regain their old standalone consent when readded.
    pub legacy_removed_service_ids: Vec<String>,
    pub version: i32,
    pub created_at: temps_core::DBDateTime,
    pub updated_at: temps_core::DBDateTime,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
