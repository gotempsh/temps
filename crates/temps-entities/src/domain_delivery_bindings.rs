// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "domain_delivery_bindings")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub hostname: String,
    pub project_id: i32,
    pub environment_id: i32,
    pub custom_domain_id: i32,
    pub profile_id: i32,
    pub profile_source: String,
    pub dns_provider_id: i32,
    pub zone: String,
    pub origin_target: String,
    pub record_type: String,
    pub proxied: bool,
    pub status: String,
    pub last_error: Option<String>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    pub applied_at: Option<DBDateTime>,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
