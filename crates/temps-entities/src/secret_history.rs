// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Change and verification history for project secrets. Rows are written by
//! database triggers on `secrets` and `http_checks`, and by `SecretService` for
//! scope changes; values are never recorded.
use sea_orm::entity::prelude::*;
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "secret_history")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub project_id: i32,
    pub secret_id: i32,
    pub kind: String,
    pub details: Json,
    pub created_at: DateTimeUtc,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
