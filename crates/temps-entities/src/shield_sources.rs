// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A reputation list the control plane fetches (ADR 066, D2), named after a
//! feed in `temps_shield::feeds`, with its last good copy.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "shield_sources")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub name: String,
    pub enabled: bool,
    /// The `ETag` each URL last answered with, by URL.
    #[sea_orm(column_type = "JsonBinary")]
    pub etags: Json,
    /// When a control plane last claimed the fetch, successful or not.
    pub last_attempt_at: Option<DBDateTime>,
    /// When a fetch last succeeded, including an unchanged answer.
    pub last_fetched_at: Option<DBDateTime>,
    pub last_error: Option<String>,
    /// When the publisher says the list was made, else when it was fetched.
    pub published_at: Option<DBDateTime>,
    pub entries: i32,
    /// Hex SHA-256 of `list`.
    pub sha256: Option<String>,
    /// The networks, as an encoded `temps_shield::SourceList`.
    #[serde(skip)]
    pub list: Option<Vec<u8>>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
