// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One attempt to fix one error group (ADR-054).
//!
//! Lifecycle of [`Model::status`]:
//!   `open` (queued) → `working` → `awaiting_review` → `merged` | `applied` | `rejected`
//!   any open status → `failed` | `stopped`
//! A closed incident keeps its transcript, timeline and decision; its sandbox
//! is deleted, and [`Model::sandbox_id`] is cleared once the deletion is
//! confirmed.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "autofix_incidents")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub public_id: String,
    pub project_id: i32,
    pub error_group_id: i32,
    pub environment_id: Option<i32>,
    /// The crashing deployment; decides where the source comes from.
    pub deployment_id: Option<i32>,
    /// The error group's title when the incident opened.
    pub title: String,
    /// `manual` or `automatic`.
    pub trigger: String,
    pub status: String,
    /// `queued`, `prepare`, `reproduce`, `verify`, `deliver` or `done`.
    pub phase: String,
    /// `git`, `uploaded_source` or `report_only`.
    pub source_kind: Option<String>,
    /// `pull_request`, `patch` or `report`.
    pub delivery: Option<String>,
    pub owner_user_id: i32,
    pub application_id: Option<i64>,
    pub conversation_id: Option<i64>,
    /// Public id of the incident's sandbox while it exists.
    pub sandbox_id: Option<String>,
    /// Commit SHA, or `bundle:<id>` for uploaded source.
    pub base_revision: Option<String>,
    pub head_revision: Option<String>,
    pub branch: Option<String>,
    pub pr_url: Option<String>,
    pub pr_number: Option<i32>,
    /// Unified diff of the fix, kept after the sandbox is deleted.
    pub patch: Option<String>,
    /// Root cause and fix, as the agent reported them.
    pub summary: Option<String>,
    pub confidence: Option<i16>,
    /// Every confidence signal with its value and source.
    pub evidence: Option<serde_json::Value>,
    pub failure_reason: Option<String>,
    /// `merged`, `applied` or `rejected`.
    pub decision: Option<String>,
    /// `None` with a decision means the policy decided.
    pub decided_by_user_id: Option<i32>,
    pub decided_at: Option<DBDateTime>,
    pub attempts: i32,
    pub closed_at: Option<DBDateTime>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
