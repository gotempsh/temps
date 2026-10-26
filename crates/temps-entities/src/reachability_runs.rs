// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One "Make it reachable" run (ADR 064 D4). See
//! `m20261024_000001_reachability`.
//!
//! `steps`, `journal`, `context` and `undo` are JSON documents owned by
//! `temps-ingress::reachability`, which gives them their types. A secret is
//! never written here.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "reachability_runs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
    /// `apply` today.
    pub kind: String,
    pub audience: String,
    /// The address being made reachable, when the run has one.
    pub address: Option<String>,
    /// The plan option the run carries out.
    pub option: String,
    /// The stored plan it was started from.
    pub plan_id: Option<String>,
    /// `running`, `succeeded`, `waiting`, `failed`, `interrupted`,
    /// `recovering`, `undoing`, `undone`.
    pub state: String,
    pub steps: Json,
    /// Every resource the run set out to create, oldest first: `pending`
    /// before the call, `done` with its id after, `absent` when a lookup
    /// after a crash found it was never made.
    pub journal: Json,
    /// Values the remaining steps need, minus secrets.
    pub context: Json,
    /// What undo removed and what it could not.
    pub undo: Option<Json>,
    pub error: Option<String>,
    pub live_url: Option<String>,
    pub created_by: Option<i32>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    pub finished_at: Option<DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
