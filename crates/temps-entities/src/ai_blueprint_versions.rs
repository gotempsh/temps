// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One version of a workspace's architecture blueprint. The document never
//! changes once written; deciding a version only changes its status.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "ai_blueprint_versions")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub blueprint_id: i64,
    /// 1, 2, 3… within the blueprint.
    pub version: i32,
    /// The design: summary, components and flows.
    pub document: serde_json::Value,
    /// What this version changes, in a sentence.
    pub change_summary: String,
    /// `proposed`, `accepted`, `rejected` or `superseded`.
    pub status: String,
    /// The chat whose model proposed it; `None` when the user wrote it.
    pub proposed_in: Option<i64>,
    pub created_by: i32,
    pub decided_by: Option<i32>,
    pub decided_at: Option<DBDateTime>,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
