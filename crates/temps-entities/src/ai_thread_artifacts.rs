// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "ai_thread_artifacts")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub public_id: String,
    pub conversation_id: i64,
    pub application_id: i64,
    pub kind: String,
    pub schema_version: i32,
    pub title: Option<String>,
    pub payload: serde_json::Value,
    pub status: String,
    pub created_by: i32,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
