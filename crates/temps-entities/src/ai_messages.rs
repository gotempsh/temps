//! A single turn in an AI debugging conversation (ADR-023).
//!
//! Append-only; ordered by `(conversation_id, created_at)`. The full set is
//! replayed as history on each turn (our DB is the source of truth).

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "ai_messages")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub conversation_id: i64,
    /// `"system" | "user" | "assistant" | "tool"`.
    pub role: String,
    pub content: String,
    /// Structured diagnosis, tool calls, citations, seed refs.
    pub metadata: Option<serde_json::Value>,
    pub tokens_in: Option<i32>,
    pub tokens_out: Option<i32>,
    pub cost_microcents: Option<i64>,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
