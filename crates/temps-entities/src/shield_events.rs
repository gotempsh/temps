// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shield events without ClickHouse (ADR 066, I8): one row per decision
//! other than `pass`, kept 7 days. With ClickHouse they go to its
//! `shield_events` table instead.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "shield_events")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub timestamp: DBDateTime,
    pub project_id: i32,
    pub environment_id: i32,
    pub node_id: Option<i32>,
    pub request_id: String,
    /// `temps_shield::EventKind` as text.
    pub kind: String,
    pub ip: String,
    pub ja4: Option<String>,
    pub verdict: String,
    pub mode: String,
    #[sea_orm(column_type = "JsonBinary")]
    pub reasons: Json,
    pub signature: Option<String>,
    pub host: String,
    pub path: String,
    pub user_agent: String,
    pub difficulty: Option<i16>,
    pub solve_ms: Option<i32>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
