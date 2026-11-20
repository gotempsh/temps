// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A goal the Temps assistant tracks for a conversation and heals on failure.
//!
//! Lifecycle of [`Model::status`]:
//!   `active` (watching) → `achieved`
//!                       → `healing` (a fix turn is running) → `active` once the
//!                         assistant re-targets the check, or `needs_you`
//!   `needs_you` is still watched at a low rate: a fix the user approves later
//!   resolves it without anyone touching the goal.
//!   `failed` (attempts or time exhausted) and `cancelled` are terminal.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "ai_goals")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// URL-safe opaque id used in the API.
    pub public_id: String,
    pub conversation_id: i64,
    /// The user the goal acts for. Every check and fix turn runs with this
    /// user's own permissions.
    pub owner_user_id: i32,
    pub title: String,
    /// `operation_id` of the read-only API operation that observes the goal.
    pub check_operation: String,
    /// Flat parameters for [`Self::check_operation`].
    pub check_params: serde_json::Value,
    /// Dot path into the response body, e.g. `status`.
    pub success_path: String,
    /// Values at [`Self::success_path`] that mean the goal is achieved.
    pub success_values: serde_json::Value,
    /// Values at [`Self::success_path`] that mean the goal failed.
    pub failure_values: serde_json::Value,
    pub status: String,
    /// Fix turns started so far.
    pub attempts: i32,
    pub max_attempts: i32,
    pub last_observation: Option<String>,
    pub last_checked_at: Option<DBDateTime>,
    pub next_check_at: DBDateTime,
    /// The goal stops being watched, as failed, after this instant.
    pub deadline_at: DBDateTime,
    /// Browser session the goal was created from. Fix turns re-derive the
    /// owner's authorization from it, so they stop when the session ends.
    pub auth_session_id: Option<i32>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
