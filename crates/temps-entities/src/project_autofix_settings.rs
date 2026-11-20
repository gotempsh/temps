// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-project autofix policy (ADR-054). A project without a row has autofix
//! off; the row is created the first time someone changes the settings.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "project_autofix_settings")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub project_id: i32,
    /// `off`, `propose` or `auto_merge`.
    pub mode: String,
    /// Confidence level an `auto_merge` fix must reach (0-5).
    pub min_confidence: i16,
    /// Glob list; a fix touching any of them is never merged by policy.
    pub protected_paths: serde_json::Value,
    /// Command the server runs to verify a fix. `None` → the reproduction the
    /// agent reports.
    pub test_command: Option<String>,
    /// Halts all automatic incidents of the project.
    pub paused: bool,
    /// User automatic incidents act as: their workspace, sandbox and Git
    /// connection. Set to whoever last changed the settings.
    pub owner_user_id: Option<i32>,
    /// Workspace incident chats go to. `None` → found or created on first use.
    pub application_id: Option<i64>,
    /// AI gateway provider and model for incident chats. `None` → the
    /// assistant's default.
    pub ai_provider: Option<String>,
    pub ai_model: Option<String>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
