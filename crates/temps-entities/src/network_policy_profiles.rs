// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A workload's egress/ingress profile when it differs from its kind's
//! default (sandboxes restricted, every other kind open).

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "network_policy_profiles")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub subject_kind: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub subject_id: i64,
    /// `open | restricted | none`; `None` = the kind's default.
    pub egress: Option<String>,
    pub ingress: Option<String>,
    pub updated_by: Option<i32>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
