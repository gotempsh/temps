// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The last runtime AI model catalog that fetched and validated (ADR 113).
//!
//! Replaced as a whole on each successful refresh. An entry overrides the
//! bundled price table for its model; the bundled table still prices every
//! model the catalog does not list. Prices are USD per 1M tokens.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "ai_model_catalog_entries")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Temps provider id (`openai`, `anthropic`, `gemini`, `xai`,
    /// `openrouter`).
    pub provider: String,
    pub model_id: String,
    pub display_name: String,
    pub input_per_million: Option<f64>,
    pub output_per_million: Option<f64>,
    pub cache_read_per_million: Option<f64>,
    pub cache_write_per_million: Option<f64>,
    pub context_window: Option<i64>,
    pub max_output_tokens: Option<i64>,
    /// `null` when the catalog does not say.
    pub supports_tools: Option<bool>,
    pub supports_vision: Option<bool>,
    pub supports_reasoning: Option<bool>,
    pub deprecated: bool,
    /// Release date as the catalog states it (`YYYY-MM-DD` or `YYYY-MM`).
    pub release_date: Option<String>,
    /// The catalog URL the entry came from.
    pub source_url: String,
    pub fetched_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
