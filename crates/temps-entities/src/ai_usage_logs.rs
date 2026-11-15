// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "ai_usage_logs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub timestamp: DBDateTime,
    pub user_id: Option<i32>,
    pub provider: String,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Part of `input_tokens` the provider served from its prompt cache
    pub cache_read_tokens: i64,
    /// Part of `input_tokens` the provider wrote to its prompt cache
    pub cache_write_tokens: i64,
    pub latency_ms: i32,
    /// Estimated cost in microcents (1/10000 of a cent)
    pub estimated_cost_microcents: i64,
    /// HTTP status code returned to the caller
    pub status: i16,
    /// Whether this was a streaming request
    pub is_streaming: bool,
    /// Whether the caller used their own API key (BYOK) instead of the system key
    pub is_byok: bool,
    /// Groups related requests into a conversation
    pub conversation_id: Option<String>,
    /// User-supplied tags for filtering
    pub tags: Vec<String>,
    /// Caller-supplied request correlation ID
    pub request_id: Option<String>,
    /// Links to OTel trace_id for waterfall views
    pub trace_id: Option<String>,
    /// Project that made the call. Set for deployment-token callers, whose
    /// `user_id` is NULL, and for project-scoped structured output; NULL for
    /// other human callers and rows older than the column.
    pub project_id: Option<i32>,
    /// Deployment token that made the call. NULL for non-deployment callers.
    pub token_id: Option<i32>,
    /// Whether `estimated_cost_microcents` came from a known price, so a free
    /// request (a published zero price) is told apart from an unpriced one (a
    /// self-hosted model no price list covers). NULL for rows older than the
    /// column, which are priced exactly when their cost is positive.
    pub price_known: Option<bool>,
}

/// SQL condition for a row that used tokens but whose cost no known price
/// gave: `price_known` is false (a self-hosted model no price list covers),
/// or, for a row older than the column, its cost is not positive. A model
/// with a published price of zero is priced. Every report that counts
/// unpriced requests uses this, so they agree. It is never NULL (`IS FALSE`,
/// not `= false`), so its negation selects exactly the other rows.
pub const UNPRICED_CONDITION: &str = "((input_tokens > 0 OR output_tokens > 0) \
     AND (price_known IS FALSE OR (price_known IS NULL AND estimated_cost_microcents <= 0)))";

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
