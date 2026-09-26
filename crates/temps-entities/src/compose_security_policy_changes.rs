// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::compose_security::ComposeSecurityPolicy;
use sea_orm::entity::prelude::*;

/// Durable grant history, written in the same transaction as the effective policy.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "compose_security_policy_changes")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub project_id: i32,
    #[sea_orm(column_type = "JsonBinary")]
    pub previous: ComposeSecurityPolicy,
    #[sea_orm(column_type = "JsonBinary")]
    pub policy: ComposeSecurityPolicy,
    pub accepted_by: i32,
    pub created_at: DateTimeUtc,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
