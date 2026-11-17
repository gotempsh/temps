// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Historical audit scope, retained even when either resource is deleted.
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "access_vault_service_history")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub vault_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub service_id: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
