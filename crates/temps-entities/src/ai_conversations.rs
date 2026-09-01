// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Persistent AI debugging conversation (ADR-023).
//!
//! One row per resumable chat, scoped to a project and a polymorphic
//! `(context_type, context_id)` — `deployment` first, then `alert`/`error_group`.
//! The turns live in [`crate::ai_messages`].

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "ai_conversations")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// URL-safe opaque id used in the API.
    pub public_id: String,
    pub project_id: i32,
    /// `"deployment" | "alert" | "error_group" | "general"`.
    pub context_type: String,
    /// The attached entity's id (ints stringified).
    pub context_id: String,
    pub title: Option<String>,
    /// `"active" | "archived"`.
    pub status: String,
    pub created_by: Option<i32>,
    /// Seed refs (log_ids, deployment state) + e.g. autofixer_run_id on hand-off.
    pub metadata: Option<serde_json::Value>,
    pub created_at: DBDateTime,
    pub last_activity_at: DBDateTime,
    /// Provider selected when the conversation was created: `gateway` or an
    /// agent CLI catalog id. Immutable for the conversation lifetime.
    pub ai_provider: String,
    /// Model selected with the provider when the conversation was created.
    pub ai_model: String,
    /// Provider-specific reasoning effort/variant, immutable for this chat.
    pub ai_thinking_level: Option<String>,
    /// Provider-specific permission/agent mode, immutable for this chat.
    pub ai_permission_mode: String,
    /// Claude CLI session UUID from the `system/init` or `result` event
    /// (`--input-format stream-json` interactive mode, ADR-038 Phase 2).
    /// Used to resume the session via `claude --resume <session_id>`.
    /// `None` for conversations that have never used the interactive CLI path,
    /// or for conversations backed by other providers (Codex, OpenCode, BYOK).
    pub cli_session_id: Option<String>,
    /// Stable fingerprint of the effective tool/catalog and provider contract
    /// used to create `cli_session_id`. A session is resumable only when this
    /// matches the contract assembled for the current turn.
    pub cli_session_fingerprint: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
