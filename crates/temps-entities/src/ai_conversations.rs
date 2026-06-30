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
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
