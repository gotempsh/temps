// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A *Make it reachable* plan and the state it was computed from (ADR 064
//! D4). See `m20261024_000001_reachability`.
//!
//! `plan` is the JSON document `temps-ingress::reachability` returned;
//! `fingerprint` hashes everything that plan depends on. Holds no secret.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "reachability_plans")]
pub struct Model {
    /// A random UUID, so plan ids cannot be guessed.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub project_id: i32,
    pub environment_id: i32,
    pub audience: String,
    pub address: Option<String>,
    pub fingerprint: String,
    pub plan: Json,
    pub created_by: Option<i32>,
    pub created_at: DBDateTime,
    /// 15 minutes after `created_at`.
    pub expires_at: DBDateTime,
    /// Set by the `apply` that used it; a plan is applied once.
    pub used_at: Option<DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
