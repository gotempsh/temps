// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Operator rules (ADR 066, D2 and I2): an address range or a fingerprint
//! with a verdict, platform-wide or for one project. `allow` wins over
//! every list.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "shield_operator_entries")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// `platform` or `project`.
    pub scope: String,
    pub project_id: Option<i32>,
    /// A CIDR network, as text; exactly one of `network` and `ja4` is set.
    pub network: Option<String>,
    pub ja4: Option<String>,
    /// `allow`, `log`, `challenge` or `block`.
    pub verdict: String,
    pub note: Option<String>,
    pub created_by: Option<i32>,
    pub created_at: DBDateTime,
    pub expires_at: Option<DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
