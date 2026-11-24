// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Append-only timeline of an autofix incident (ADR-054), written by the
//! server at every action.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "autofix_incident_events")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub incident_id: i64,
    /// `server`, `agent`, `policy` or `user:<id>`.
    pub actor: String,
    pub kind: String,
    pub summary: String,
    pub details: Option<serde_json::Value>,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
