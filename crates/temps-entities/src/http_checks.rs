// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "http_checks")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: i32,
    pub env_var_id: Option<i32>,
    pub name: String,
    pub automatic_provider: Option<String>,
    pub encrypted_spec: String,
    pub encrypted_credential: Option<String>,
    pub enabled: bool,
    pub interval_seconds: i32,
    pub next_check_at: DateTimeUtc,
    pub lease_until: Option<DateTimeUtc>,
    pub lease_token: Option<String>,
    pub last_result: Option<Json>,
    pub last_checked_at: Option<DateTimeUtc>,
    pub last_notified_fingerprint: String,
    pub consecutive_unknowns: i32,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
