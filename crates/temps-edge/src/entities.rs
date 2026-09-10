// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sea-ORM entity for the edge_requests table (local SQLite).

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "edge_requests")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub timestamp: String,
    pub domain: String,
    pub path: String,
    pub method: String,
    pub status: i32,
    pub cache_status: String,
    pub bytes_sent: i64,
    pub origin_latency_ms: f64,
    pub region: Option<String>,
    pub is_immutable: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
