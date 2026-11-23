// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseBackend, DatabaseConnection, EntityTrait,
    FromQueryResult, QueryFilter, QueryOrder, QuerySelect, Set, Statement,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_entities::{ai_provider_keys, ai_provider_models, ai_usage_logs};
use utoipa::ToSchema;

use crate::error::AiGatewayError;
use crate::types::{ModelDetails, PromptTokensDetails};

/// How many catalogue rows for one model id are inspected for a price. The
/// same model can be listed under several provider keys; the newest entries
/// that carry a price win.
const PRICE_LOOKUP_ROWS: u64 = 5;

/// USD per million tokens is numerically the same as microcents per token
/// (1 USD = 1,000,000 microcents). A request costs fresh input at the input
/// price, cached input at the cache-read price, cache writes at the
/// cache-write price, and output at the output price.
///
/// `input_tokens` is the whole prompt, so the cached parts are carved out of
/// it rather than added to it. A catalogue that publishes no cache price
/// prices those tokens as ordinary input: that overstates a cache hit but
/// never hides spend.
///
/// Returns `None` when the catalogue publishes no price for the model, so the
/// caller can tell "unpriced" from "free".
pub(crate) fn cost_microcents_from_details(
    details: &ModelDetails,
    input_tokens: i64,
    output_tokens: i64,
    cache: PromptTokensDetails,
) -> Option<i64> {
    let input_price = details.input_price_per_million?;
    let output_price = details.output_price_per_million?;
    if !input_price.is_finite() || !output_price.is_finite() {
        return None;
    }
    let usable = |price: Option<f64>| price.filter(|p| p.is_finite()).unwrap_or(input_price);
    let read_price = usable(details.cache_read_price_per_million);
    let write_price = usable(details.cache_write_price_per_million);

    let input_tokens = input_tokens.max(0);
    let read = cache.cached_tokens.clamp(0, input_tokens);
    let write = cache.cache_write_tokens.clamp(0, input_tokens - read);
    let fresh = input_tokens - read - write;
    let cost = fresh as f64 * input_price
        + read as f64 * read_price
        + write as f64 * write_price
        + output_tokens.max(0) as f64 * output_price;
    // Saturate rather than wrap on an absurd product.
    Some(cost.round().clamp(0.0, i64::MAX as f64) as i64)
}

/// What a usage record's token counts describe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UsageCount {
    /// One request: its prompt length decides long-context pricing.
    OneRequest,
    /// The total of a batch of requests. The prompt length of each is not
    /// known, so the total is never compared with a long-context threshold
    /// (see `pricing::estimate_batch_total_cost_microcents`).
    BatchTotal,
}

/// SQL condition for a usage row that used tokens but has no cost from a
/// known price, shared with every other report that counts them. Mirrors
/// `is_unpriced`.
const UNPRICED_SQL: &str = temps_entities::ai_usage_logs::UNPRICED_CONDITION;

/// Whether a usage row used tokens but has no cost from a known price; the
/// same rule as `UNPRICED_SQL`.
fn is_unpriced(
    price_known: Option<bool>,
    cost_microcents: i64,
    input_tokens: i64,
    output_tokens: i64,
) -> bool {
    (input_tokens > 0 || output_tokens > 0)
        && match price_known {
            Some(known) => !known,
            None => cost_microcents <= 0,
        }
}

/// The cost to record for a request the model catalogue priced.
///
/// The catalogue publishes one input and one output price, with no
/// long-context tier, so a long prompt to a model the built-in price list
/// bills higher above a threshold is raised to the listed long-context cost.
/// A self-hosted server's model ids are the operator's own names and are
/// never matched against vendor price lists.
///
/// A batch total is not raised: its requests' prompt lengths are not known.
fn catalogue_cost_microcents(
    provider: &str,
    model: &str,
    catalogue_cost: i64,
    input_tokens: i64,
    output_tokens: i64,
    cache: PromptTokensDetails,
    count: UsageCount,
) -> i64 {
    if crate::self_hosted::is_self_hosted(provider) || count == UsageCount::BatchTotal {
        return catalogue_cost;
    }
    crate::services::pricing::long_context_cost_microcents(
        model,
        input_tokens,
        output_tokens,
        cache,
    )
    .map_or(catalogue_cost, |long| catalogue_cost.max(long))
}

/// Cost of a request whose model the catalogue publishes no price for.
///
/// A vendor model goes to the built-in price list (with the runtime model
/// catalog applied), which charges a model it does not list at the most
/// expensive listed rate, so an unknown model can never be a way around a
/// spend cap. That figure is a ceiling, not a price: the caller records it with
/// no known price (see `pricing::is_priced`), so reports flag the model as
/// "price unknown" instead of presenting the ceiling as its cost. A self-hosted server has no vendor price
/// to approximate: its model ids are whatever the operator named them, and
/// charging them as the dearest cloud model reports a bill nobody pays. It is
/// recorded at zero with no known price (`None`), which reports read as "no
/// published price" rather than as free.
fn unlisted_cost_microcents(
    provider: &str,
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
    cache: PromptTokensDetails,
    count: UsageCount,
) -> Option<i64> {
    use crate::services::pricing;
    if crate::self_hosted::is_self_hosted(provider) {
        return None;
    }
    Some(match count {
        UsageCount::OneRequest => {
            pricing::estimate_cost_microcents(model, input_tokens, output_tokens, cache)
        }
        UsageCount::BatchTotal => {
            pricing::estimate_batch_total_cost_microcents(model, input_tokens, output_tokens, cache)
        }
    })
}

/// The cost to record for a request no caller priced, and whether a known
/// price gave it. `catalogue` is the key catalogue's price for the request
/// (`None` when it publishes none).
pub(crate) fn recorded_cost_microcents(
    provider: &str,
    model: &str,
    catalogue: Option<i64>,
    input_tokens: i64,
    output_tokens: i64,
    cache: PromptTokensDetails,
    count: UsageCount,
) -> (i64, bool) {
    match catalogue {
        Some(cost) => (
            catalogue_cost_microcents(
                provider,
                model,
                cost,
                input_tokens,
                output_tokens,
                cache,
                count,
            ),
            true,
        ),
        None => match unlisted_cost_microcents(
            provider,
            model,
            input_tokens,
            output_tokens,
            cache,
            count,
        ) {
            Some(cost) => (cost, crate::services::pricing::is_priced(model)),
            None => (0, false),
        },
    }
}

// ============================================================================
// Response structs
// ============================================================================

#[derive(Debug, Serialize, ToSchema)]
pub struct UsageSummary {
    pub total_requests: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_tokens: i64,
    pub avg_latency_ms: f64,
    pub total_cost_microcents: i64,
    pub error_count: i64,
    pub streaming_count: i64,
    pub byok_count: i64,
    /// Requests that used tokens but whose cost no known price gave: logged
    /// before costs were estimated, or served by a model with no published
    /// price (a self-hosted server). A model with a published price of zero
    /// is priced, not counted here. `total_cost_microcents` leaves these out,
    /// so a client shows the total as partial, or unknown when every request
    /// is unpriced, rather than as free.
    pub unpriced_requests: i64,
    /// Requests that used tokens and whose cost a known price gave, zero
    /// included. With `unpriced_requests` it tells a window with no priced
    /// request (no total to show) from one whose total is partial; requests
    /// that used no tokens (failures) are in neither.
    pub priced_requests: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProviderUsage {
    pub provider: String,
    pub request_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub avg_latency_ms: f64,
    pub error_count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TimeseriesBucket {
    /// ISO 8601 timestamp
    pub bucket: String,
    pub request_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub avg_latency_ms: f64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ModelUsage {
    pub model: String,
    pub provider: String,
    pub request_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub avg_latency_ms: f64,
    /// Sum of the recorded cost of the model's requests in the window, in
    /// microcents (1e-6 USD). Clients show this rather than recomputing from
    /// list prices, which cannot see cache prices or long-context rates. It
    /// includes requests made with a caller-supplied provider key, so it is
    /// not the figure a project's budget is measured against (that is
    /// `ProjectUsage::operator_cost_microcents`).
    pub estimated_cost_microcents: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct UsageLogEntry {
    pub id: i64,
    pub timestamp: String,
    pub provider: String,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Part of `input_tokens` read from the provider's prompt cache.
    pub cache_read_tokens: i64,
    /// Part of `input_tokens` written to the provider's prompt cache.
    pub cache_write_tokens: i64,
    pub latency_ms: i32,
    pub estimated_cost_microcents: i64,
    /// The request used tokens but no known price gave its cost, so
    /// `estimated_cost_microcents` (zero) is not a price: show it as unknown,
    /// not as free.
    pub unpriced: bool,
    pub status: i16,
    pub is_streaming: bool,
    pub is_byok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Project the call is attributed to (deployment-token callers only)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<i32>,
    /// Deployment token the call is attributed to
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_id: Option<i32>,
}

/// Per-project spend and volume, for cost reporting.
#[derive(Debug, Serialize, ToSchema)]
pub struct ProjectUsage {
    pub project_id: i32,
    pub request_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Estimated cost in microcents (1/10000 of a cent), from the model price list
    pub estimated_cost_microcents: i64,
    /// The part of `estimated_cost_microcents` the operator pays for: requests
    /// made with a caller-supplied provider key are excluded. This is the
    /// figure a project's monthly budget is measured against.
    pub operator_cost_microcents: i64,
    /// The project's effective monthly budget after inheritance, in microcents;
    /// `null` when it has no cap. Left `None` by the usage service and filled in
    /// by the handler, which owns the link to the limits service.
    pub monthly_budget_microcents: Option<i64>,
    /// Requests served with a caller-supplied provider key (not operator spend)
    pub byok_request_count: i64,
    pub error_count: i64,
}

/// A page of recent usage log entries plus the total count for pagination.
#[derive(Debug, Serialize, ToSchema)]
pub struct UsageLogPage {
    /// The usage log entries for the requested page.
    pub entries: Vec<UsageLogEntry>,
    /// Total number of entries matching the filter (across all pages).
    pub total: i64,
}

/// A conversation summary grouping related AI invocations.
#[derive(Debug, Serialize, ToSchema)]
pub struct ConversationSummary {
    pub conversation_id: String,
    pub message_count: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_tokens: i64,
    pub total_cost_microcents: i64,
    pub avg_latency_ms: f64,
    pub models_used: Vec<String>,
    pub first_at: String,
    pub last_at: String,
}

/// Optional metadata the caller can attach to an AI gateway request.
#[derive(Debug, Clone, Default)]
pub struct AiRequestContext {
    pub conversation_id: Option<String>,
    pub tags: Vec<String>,
    pub request_id: Option<String>,
    pub trace_id: Option<String>,
    /// Project the caller's deployment token belongs to. Set by the handler
    /// from the authenticated principal, never from request headers.
    pub project_id: Option<i32>,
    /// Deployment token that made the call.
    pub token_id: Option<i32>,
}

impl AiRequestContext {
    /// Attribute the request to the deployment token that authenticated it.
    /// Human callers (sessions, API keys) are attributed by `user_id` instead.
    pub fn attributed_to(mut self, auth: &temps_authz::AuthContext) -> Self {
        if let Some(token) = auth.deployment_token_info() {
            self.project_id = Some(token.project_id);
            self.token_id = Some(token.token_id);
        }
        self
    }
}

/// Filters for querying AI usage data.
///
/// Cost bounds are expressed in microcents (the unit stored in
/// `estimated_cost_microcents`). At most one of `gte`/`gt` and one of
/// `lte`/`lt` is meaningful per query; if both are set the stricter wins
/// naturally because they are ANDead together.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct UsageFilter {
    pub user_id: Option<i32>,
    /// Restrict to usage attributed to this project (deployment-token callers).
    pub project_id: Option<i32>,
    pub conversation_id: Option<String>,
    /// Comma-separated tags to filter by (AND logic).
    pub tags: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    /// Filter by HTTP status code (exact match).
    pub status: Option<i16>,
    /// Cost greater-than-or-equal, in microcents.
    pub cost_gte: Option<i64>,
    /// Cost strictly greater-than, in microcents.
    pub cost_gt: Option<i64>,
    /// Cost less-than-or-equal, in microcents.
    pub cost_lte: Option<i64>,
    /// Cost strictly less-than, in microcents.
    pub cost_lt: Option<i64>,
    /// Total tokens (input + output) greater-than-or-equal.
    pub tokens_gte: Option<i64>,
    /// Total tokens (input + output) strictly greater-than.
    pub tokens_gt: Option<i64>,
    /// Total tokens (input + output) less-than-or-equal.
    pub tokens_lte: Option<i64>,
    /// Total tokens (input + output) strictly less-than.
    pub tokens_lt: Option<i64>,
}

// ============================================================================
// Internal query result structs (FromQueryResult)
// ============================================================================

#[derive(Debug, FromQueryResult)]
struct SummaryRow {
    total_requests: Option<i64>,
    total_input_tokens: Option<i64>,
    total_output_tokens: Option<i64>,
    total_tokens: Option<i64>,
    avg_latency_ms: Option<f64>,
    total_cost_microcents: Option<i64>,
    error_count: Option<i64>,
    streaming_count: Option<i64>,
    byok_count: Option<i64>,
    unpriced_requests: Option<i64>,
    priced_requests: Option<i64>,
}

#[derive(Debug, FromQueryResult)]
struct ProviderUsageRow {
    provider: Option<String>,
    request_count: Option<i64>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    avg_latency_ms: Option<f64>,
    error_count: Option<i64>,
}

#[derive(Debug, FromQueryResult)]
struct TimeseriesBucketRow {
    bucket: Option<String>,
    request_count: Option<i64>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    avg_latency_ms: Option<f64>,
}

#[derive(Debug, FromQueryResult)]
struct ModelUsageRow {
    model: Option<String>,
    provider: Option<String>,
    request_count: Option<i64>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    total_tokens: Option<i64>,
    avg_latency_ms: Option<f64>,
    estimated_cost_microcents: Option<i64>,
}

#[derive(Debug, FromQueryResult)]
struct UsageLogRow {
    id: Option<i64>,
    timestamp: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_read_tokens: Option<i64>,
    cache_write_tokens: Option<i64>,
    latency_ms: Option<i32>,
    estimated_cost_microcents: Option<i64>,
    status: Option<i16>,
    is_streaming: Option<bool>,
    is_byok: Option<bool>,
    conversation_id: Option<String>,
    tags: Option<String>,
    request_id: Option<String>,
    trace_id: Option<String>,
    project_id: Option<i32>,
    token_id: Option<i32>,
    price_known: Option<bool>,
}

#[derive(Debug, FromQueryResult)]
struct ProjectUsageRow {
    project_id: Option<i32>,
    request_count: Option<i64>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    estimated_cost_microcents: Option<i64>,
    operator_cost_microcents: Option<i64>,
    byok_request_count: Option<i64>,
    error_count: Option<i64>,
}

#[derive(Debug, FromQueryResult)]
struct SpendRow {
    spend: Option<i64>,
}

#[derive(Debug, FromQueryResult)]
struct CountRow {
    count: Option<i64>,
}

#[derive(Debug, FromQueryResult)]
struct ConversationSummaryRow {
    conversation_id: Option<String>,
    message_count: Option<i64>,
    total_input_tokens: Option<i64>,
    total_output_tokens: Option<i64>,
    total_tokens: Option<i64>,
    total_cost_microcents: Option<i64>,
    avg_latency_ms: Option<f64>,
    models_used: Option<String>,
    first_at: Option<String>,
    last_at: Option<String>,
}

// ============================================================================
// Service
// ============================================================================

pub struct UsageService {
    db: Arc<DatabaseConnection>,
}

impl UsageService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn log_usage(
        &self,
        user_id: Option<i32>,
        provider: &str,
        model: &str,
        input_tokens: i64,
        output_tokens: i64,
        latency_ms: i32,
        estimated_cost_microcents: i64,
        status: i16,
        is_streaming: bool,
        is_byok: bool,
    ) -> Result<(), AiGatewayError> {
        self.log_usage_with_context(
            user_id,
            provider,
            model,
            input_tokens,
            output_tokens,
            latency_ms,
            estimated_cost_microcents,
            status,
            is_streaming,
            is_byok,
            PromptTokensDetails::default(),
            &AiRequestContext::default(),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn log_usage_with_context(
        &self,
        user_id: Option<i32>,
        provider: &str,
        model: &str,
        input_tokens: i64,
        output_tokens: i64,
        latency_ms: i32,
        estimated_cost_microcents: i64,
        status: i16,
        is_streaming: bool,
        is_byok: bool,
        cache: PromptTokensDetails,
        context: &AiRequestContext,
    ) -> Result<(), AiGatewayError> {
        self.log_usage_on_connection(
            self.db.as_ref(),
            user_id,
            provider,
            model,
            input_tokens,
            output_tokens,
            latency_ms,
            estimated_cost_microcents,
            status,
            is_streaming,
            is_byok,
            cache,
            UsageCount::OneRequest,
            context,
        )
        .await
    }

    /// Estimate a request's cost from the model catalogue's published price,
    /// read from entries of the serving `provider`'s keys only.
    /// `None` when the catalogue publishes no price for the model. A failed
    /// lookup is logged and treated as no price: usage logging must never fail
    /// a request over a missing estimate.
    async fn estimate_cost_microcents<C: sea_orm::ConnectionTrait>(
        &self,
        connection: &C,
        provider: &str,
        model: &str,
        input_tokens: i64,
        output_tokens: i64,
        cache: PromptTokensDetails,
    ) -> Option<i64> {
        if input_tokens <= 0 && output_tokens <= 0 {
            return Some(0);
        }
        // Only the serving provider's catalogue entries: a self-hosted
        // server's ids are the operator's own names, so its `gpt-5.4` must not
        // take a vendor key's price (and be recorded as priced), while its own
        // priced entries still apply. Scoped by provider rather than key so a
        // request on the caller's own key is priced from its vendor's catalogue.
        let rows = ai_provider_models::Entity::find()
            .inner_join(ai_provider_keys::Entity)
            .filter(ai_provider_keys::Column::Provider.eq(provider))
            .filter(ai_provider_models::Column::ModelId.eq(model))
            .filter(ai_provider_models::Column::Metadata.is_not_null())
            .order_by_desc(ai_provider_models::Column::UpdatedAt)
            .limit(PRICE_LOOKUP_ROWS)
            .all(connection)
            .await;
        match rows {
            Ok(rows) => rows
                .into_iter()
                .filter_map(|row| row.metadata)
                .filter_map(|metadata| serde_json::from_value::<ModelDetails>(metadata).ok())
                .find_map(|details| {
                    cost_microcents_from_details(&details, input_tokens, output_tokens, cache)
                }),
            Err(error) => {
                tracing::warn!(model, %error, "could not look up a model price in the catalogue");
                None
            }
        }
    }

    /// Write usage on the caller's transaction so batch accounting and its
    /// idempotency marker commit together.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn log_usage_on_connection<C: sea_orm::ConnectionTrait>(
        &self,
        connection: &C,
        user_id: Option<i32>,
        provider: &str,
        model: &str,
        input_tokens: i64,
        output_tokens: i64,
        latency_ms: i32,
        estimated_cost_microcents: i64,
        status: i16,
        is_streaming: bool,
        is_byok: bool,
        cache: PromptTokensDetails,
        count: UsageCount,
        context: &AiRequestContext,
    ) -> Result<(), AiGatewayError> {
        // Callers that already priced the request keep their figure. Everything
        // else is priced here, once, so every path records the same estimate
        // and spend caps and per-project reports have a cost to work with:
        // from the model catalogue first (it carries cache prices; a long
        // prompt is raised to the listed long-context cost), then from
        // the built-in price list (see `unlisted_cost_microcents`); a model no
        // price covers is charged the worst case and recorded unpriced. BYOK
        // requests are priced too; reports separate them as non-operator cost.
        // A batch total is never priced by long-context tiers, since its
        // requests' prompt lengths are not known. Whether a known price gave
        // the cost is recorded, so a free model is not reported as unpriced.
        let (estimated_cost_microcents, price_known) = if estimated_cost_microcents > 0 {
            (estimated_cost_microcents, true)
        } else {
            let catalogue = self
                .estimate_cost_microcents(
                    connection,
                    provider,
                    model,
                    input_tokens,
                    output_tokens,
                    cache,
                )
                .await;
            recorded_cost_microcents(
                provider,
                model,
                catalogue,
                input_tokens,
                output_tokens,
                cache,
                count,
            )
        };
        let record = ai_usage_logs::ActiveModel {
            timestamp: Set(chrono::Utc::now()),
            user_id: Set(user_id),
            provider: Set(provider.to_string()),
            model: Set(model.to_string()),
            input_tokens: Set(input_tokens),
            output_tokens: Set(output_tokens),
            cache_read_tokens: Set(cache.cached_tokens.clamp(0, input_tokens.max(0))),
            cache_write_tokens: Set(cache.cache_write_tokens.clamp(0, input_tokens.max(0))),
            latency_ms: Set(latency_ms),
            estimated_cost_microcents: Set(estimated_cost_microcents),
            status: Set(status),
            is_streaming: Set(is_streaming),
            is_byok: Set(is_byok),
            conversation_id: Set(context.conversation_id.clone()),
            tags: Set(context.tags.clone()),
            request_id: Set(context.request_id.clone()),
            trace_id: Set(context.trace_id.clone()),
            project_id: Set(context.project_id),
            token_id: Set(context.token_id),
            price_known: Set(Some(price_known)),
            ..Default::default()
        };

        record.insert(connection).await?;
        Ok(())
    }

    /// Get usage summary (totals) for a time range with optional filters.
    pub async fn get_summary(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<UsageSummary, AiGatewayError> {
        self.get_summary_filtered(from, to, &UsageFilter::default())
            .await
    }

    /// Get usage summary with filters.
    pub async fn get_summary_filtered(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        filter: &UsageFilter,
    ) -> Result<UsageSummary, AiGatewayError> {
        let (where_clause, values) = self.build_filter_clause(from, to, filter);

        let sql = format!(
            r#"SELECT
                COUNT(*) as total_requests,
                COALESCE(SUM(input_tokens), 0)::INT8 as total_input_tokens,
                COALESCE(SUM(output_tokens), 0)::INT8 as total_output_tokens,
                COALESCE(SUM(input_tokens + output_tokens), 0)::INT8 as total_tokens,
                COALESCE(AVG(latency_ms)::float8, 0) as avg_latency_ms,
                COALESCE(SUM(estimated_cost_microcents), 0)::INT8 as total_cost_microcents,
                COUNT(*) FILTER (WHERE status >= 400) as error_count,
                COUNT(*) FILTER (WHERE is_streaming = true) as streaming_count,
                COUNT(*) FILTER (WHERE is_byok = true) as byok_count,
                COUNT(*) FILTER (WHERE {unpriced}) as unpriced_requests,
                COUNT(*) FILTER (
                    WHERE (input_tokens > 0 OR output_tokens > 0) AND NOT ({unpriced})
                ) as priced_requests
            FROM ai_usage_logs
            WHERE {where_clause}"#,
            unpriced = UNPRICED_SQL,
        );

        let row = SummaryRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            values,
        ))
        .one(self.db.as_ref())
        .await?;

        let row = row.unwrap_or(SummaryRow {
            total_requests: Some(0),
            total_input_tokens: Some(0),
            total_output_tokens: Some(0),
            total_tokens: Some(0),
            avg_latency_ms: Some(0.0),
            total_cost_microcents: Some(0),
            error_count: Some(0),
            streaming_count: Some(0),
            byok_count: Some(0),
            unpriced_requests: Some(0),
            priced_requests: Some(0),
        });

        Ok(UsageSummary {
            total_requests: row.total_requests.unwrap_or(0),
            total_input_tokens: row.total_input_tokens.unwrap_or(0),
            total_output_tokens: row.total_output_tokens.unwrap_or(0),
            total_tokens: row.total_tokens.unwrap_or(0),
            avg_latency_ms: row.avg_latency_ms.unwrap_or(0.0),
            total_cost_microcents: row.total_cost_microcents.unwrap_or(0),
            error_count: row.error_count.unwrap_or(0),
            streaming_count: row.streaming_count.unwrap_or(0),
            byok_count: row.byok_count.unwrap_or(0),
            unpriced_requests: row.unpriced_requests.unwrap_or(0),
            priced_requests: row.priced_requests.unwrap_or(0),
        })
    }

    /// Get usage broken down by provider for a time range.
    pub async fn get_by_provider(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<ProviderUsage>, AiGatewayError> {
        self.get_by_provider_filtered(from, to, &UsageFilter::default())
            .await
    }

    /// Get usage broken down by provider with filters.
    pub async fn get_by_provider_filtered(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        filter: &UsageFilter,
    ) -> Result<Vec<ProviderUsage>, AiGatewayError> {
        let (where_clause, values) = self.build_filter_clause(from, to, filter);

        let sql = format!(
            r#"SELECT
                provider,
                COUNT(*) as request_count,
                COALESCE(SUM(input_tokens), 0)::INT8 as input_tokens,
                COALESCE(SUM(output_tokens), 0)::INT8 as output_tokens,
                COALESCE(AVG(latency_ms)::float8, 0) as avg_latency_ms,
                COUNT(*) FILTER (WHERE status >= 400) as error_count
            FROM ai_usage_logs
            WHERE {}
            GROUP BY provider
            ORDER BY request_count DESC"#,
            where_clause
        );

        let rows = ProviderUsageRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            values,
        ))
        .all(self.db.as_ref())
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| ProviderUsage {
                provider: r.provider.unwrap_or_default(),
                request_count: r.request_count.unwrap_or(0),
                input_tokens: r.input_tokens.unwrap_or(0),
                output_tokens: r.output_tokens.unwrap_or(0),
                avg_latency_ms: r.avg_latency_ms.unwrap_or(0.0),
                error_count: r.error_count.unwrap_or(0),
            })
            .collect())
    }

    /// Spend and volume per project for a time range, most expensive first.
    ///
    /// Only usage attributed to a project (deployment-token callers) appears;
    /// rows from before attribution existed have no project and are excluded.
    /// Bounded by `limit` so a large instance cannot return an unbounded set.
    pub async fn get_by_project(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        limit: u64,
    ) -> Result<Vec<ProjectUsage>, AiGatewayError> {
        let rows = ProjectUsageRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"SELECT
                project_id,
                COUNT(*) as request_count,
                COALESCE(SUM(input_tokens), 0)::INT8 as input_tokens,
                COALESCE(SUM(output_tokens), 0)::INT8 as output_tokens,
                COALESCE(SUM(estimated_cost_microcents), 0)::INT8 as estimated_cost_microcents,
                COALESCE(SUM(estimated_cost_microcents) FILTER (WHERE is_byok = false), 0)::INT8 as operator_cost_microcents,
                COUNT(*) FILTER (WHERE is_byok = true) as byok_request_count,
                COUNT(*) FILTER (WHERE status >= 400) as error_count
            FROM ai_usage_logs
            WHERE project_id IS NOT NULL AND timestamp >= $1 AND timestamp <= $2
            GROUP BY project_id
            ORDER BY estimated_cost_microcents DESC, request_count DESC
            LIMIT $3"#,
            [from.into(), to.into(), (limit as i64).into()],
        ))
        .all(self.db.as_ref())
        .await?;

        Ok(rows
            .into_iter()
            .filter_map(|r| {
                Some(ProjectUsage {
                    project_id: r.project_id?,
                    request_count: r.request_count.unwrap_or(0),
                    input_tokens: r.input_tokens.unwrap_or(0),
                    output_tokens: r.output_tokens.unwrap_or(0),
                    estimated_cost_microcents: r.estimated_cost_microcents.unwrap_or(0),
                    operator_cost_microcents: r.operator_cost_microcents.unwrap_or(0),
                    monthly_budget_microcents: None,
                    byok_request_count: r.byok_request_count.unwrap_or(0),
                    error_count: r.error_count.unwrap_or(0),
                })
            })
            .collect())
    }

    /// The provider name behind a route such as `gateway_key:7` (the key's
    /// provider, e.g. `openai`), so usage rows group with those logged by the
    /// request handlers. Anything that is not a resolvable key route is
    /// returned unchanged.
    pub async fn provider_name_for_route(&self, route: &str) -> String {
        let Some(key_id) = route
            .strip_prefix("gateway_key:")
            .and_then(|raw| raw.parse::<i32>().ok())
        else {
            return route.to_string();
        };
        match temps_entities::ai_provider_keys::Entity::find_by_id(key_id)
            .one(self.db.as_ref())
            .await
        {
            Ok(Some(key)) => key.provider,
            Ok(None) => route.to_string(),
            Err(error) => {
                tracing::warn!(key_id, %error, "Could not resolve the provider of an AI key");
                route.to_string()
            }
        }
    }

    /// Operator-funded spend of one project since `since`, in microcents.
    /// Requests made with a caller-supplied provider key cost the operator
    /// nothing and are excluded. Served by `idx_ai_usage_logs_project_timestamp`.
    pub async fn project_spend_since(
        &self,
        project_id: i32,
        since: DateTime<Utc>,
    ) -> Result<i64, AiGatewayError> {
        let row = SpendRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"SELECT COALESCE(SUM(estimated_cost_microcents), 0)::INT8 as spend
            FROM ai_usage_logs
            WHERE project_id = $1 AND timestamp >= $2 AND is_byok = false"#,
            [project_id.into(), since.into()],
        ))
        .one(self.db.as_ref())
        .await?;
        Ok(row.and_then(|r| r.spend).unwrap_or(0))
    }

    /// Get time-series usage data bucketed by interval.
    pub async fn get_timeseries(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        bucket: &str,
    ) -> Result<Vec<TimeseriesBucket>, AiGatewayError> {
        self.get_timeseries_filtered(from, to, bucket, &UsageFilter::default())
            .await
    }

    /// Get time-series usage data with filters.
    pub async fn get_timeseries_filtered(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        bucket: &str,
        filter: &UsageFilter,
    ) -> Result<Vec<TimeseriesBucket>, AiGatewayError> {
        let interval = match bucket {
            "hour" => "1 hour",
            "day" => "1 day",
            "week" => "1 week",
            _ => {
                return Err(AiGatewayError::Validation {
                    message: format!(
                        "Invalid bucket '{}': must be 'hour', 'day', or 'week'",
                        bucket
                    ),
                })
            }
        };

        let (where_clause, mut values) = self.build_filter_clause(from, to, filter);
        // Prepend the interval as $1, shift other params
        let mut all_values = vec![sea_orm::Value::from(interval)];
        all_values.append(&mut values);

        // Rewrite the where clause to shift parameter numbers by 1
        let shifted_where = shift_params(&where_clause, 1);

        let sql = format!(
            r#"SELECT bucket::text, request_count, input_tokens, output_tokens, avg_latency_ms
            FROM (
                SELECT time_bucket($1::interval, timestamp) as bucket,
                       COUNT(*) as request_count,
                       COALESCE(SUM(input_tokens), 0)::INT8 as input_tokens,
                       COALESCE(SUM(output_tokens), 0)::INT8 as output_tokens,
                       COALESCE(AVG(latency_ms)::float8, 0) as avg_latency_ms
                FROM ai_usage_logs
                WHERE {}
                GROUP BY time_bucket($1::interval, timestamp)
            ) sub
            ORDER BY bucket ASC"#,
            shifted_where
        );

        let rows = TimeseriesBucketRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            all_values,
        ))
        .all(self.db.as_ref())
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| TimeseriesBucket {
                bucket: r.bucket.unwrap_or_default(),
                request_count: r.request_count.unwrap_or(0),
                input_tokens: r.input_tokens.unwrap_or(0),
                output_tokens: r.output_tokens.unwrap_or(0),
                avg_latency_ms: r.avg_latency_ms.unwrap_or(0.0),
            })
            .collect())
    }

    /// Get top models by request count.
    pub async fn get_top_models(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        limit: u64,
    ) -> Result<Vec<ModelUsage>, AiGatewayError> {
        self.get_top_models_filtered(from, to, limit, &UsageFilter::default())
            .await
    }

    /// Get top models with filters.
    pub async fn get_top_models_filtered(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        limit: u64,
        filter: &UsageFilter,
    ) -> Result<Vec<ModelUsage>, AiGatewayError> {
        let (where_clause, mut values) = self.build_filter_clause(from, to, filter);
        let next_param = values.len() + 1;
        values.push((limit as i64).into());

        let sql = format!(
            r#"SELECT
                model,
                provider,
                COUNT(*) as request_count,
                COALESCE(SUM(input_tokens), 0)::INT8 as input_tokens,
                COALESCE(SUM(output_tokens), 0)::INT8 as output_tokens,
                COALESCE(SUM(input_tokens + output_tokens), 0)::INT8 as total_tokens,
                COALESCE(AVG(latency_ms)::float8, 0) as avg_latency_ms,
                COALESCE(SUM(estimated_cost_microcents), 0)::INT8 as estimated_cost_microcents
            FROM ai_usage_logs
            WHERE {}
            GROUP BY model, provider
            ORDER BY request_count DESC
            LIMIT ${}"#,
            where_clause, next_param
        );

        let rows = ModelUsageRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            values,
        ))
        .all(self.db.as_ref())
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| ModelUsage {
                model: r.model.unwrap_or_default(),
                provider: r.provider.unwrap_or_default(),
                request_count: r.request_count.unwrap_or(0),
                input_tokens: r.input_tokens.unwrap_or(0),
                output_tokens: r.output_tokens.unwrap_or(0),
                total_tokens: r.total_tokens.unwrap_or(0),
                avg_latency_ms: r.avg_latency_ms.unwrap_or(0.0),
                estimated_cost_microcents: r.estimated_cost_microcents.unwrap_or(0),
            })
            .collect())
    }

    /// Get recent usage log entries.
    pub async fn get_recent(&self, limit: u64) -> Result<Vec<UsageLogEntry>, AiGatewayError> {
        Ok(self
            .get_recent_filtered(limit, 0, &UsageFilter::default())
            .await?
            .entries)
    }

    /// Get a page of recent usage log entries with filters, plus the total count.
    pub async fn get_recent_filtered(
        &self,
        limit: u64,
        offset: u64,
        filter: &UsageFilter,
    ) -> Result<UsageLogPage, AiGatewayError> {
        // Use a wide time range for "recent" queries
        let to = Utc::now();
        let from = to - chrono::Duration::days(365);

        // Total count for pagination -- the filter clause and its bound values are
        // identical to the page query, so build them once and reuse for both.
        let (where_clause, base_values) = self.build_filter_clause(from, to, filter);

        let count_sql = format!(
            "SELECT COUNT(*) as count FROM ai_usage_logs WHERE {}",
            where_clause
        );
        let total = CountRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &count_sql,
            base_values.clone(),
        ))
        .one(self.db.as_ref())
        .await?
        .and_then(|r| r.count)
        .unwrap_or(0);

        let mut values = base_values;
        let limit_param = values.len() + 1;
        values.push((limit as i64).into());
        let offset_param = values.len() + 1;
        values.push((offset as i64).into());

        let sql = format!(
            r#"SELECT
                id,
                timestamp::text,
                provider,
                model,
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
                latency_ms,
                estimated_cost_microcents,
                status,
                is_streaming,
                is_byok,
                conversation_id,
                array_to_string(tags, ',') as tags,
                request_id,
                trace_id,
                project_id,
                token_id,
                price_known
            FROM ai_usage_logs
            WHERE {}
            ORDER BY timestamp DESC
            LIMIT ${} OFFSET ${}"#,
            where_clause, limit_param, offset_param
        );

        let rows = UsageLogRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            values,
        ))
        .all(self.db.as_ref())
        .await?;

        Ok(UsageLogPage {
            entries: rows.into_iter().map(usage_log_from_row).collect(),
            total,
        })
    }

    /// List conversations with aggregated stats.
    pub async fn get_conversations(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        filter: &UsageFilter,
        limit: u64,
    ) -> Result<Vec<ConversationSummary>, AiGatewayError> {
        let (where_clause, mut values) = self.build_filter_clause(from, to, filter);
        let next_param = values.len() + 1;
        values.push((limit as i64).into());

        let sql = format!(
            r#"SELECT
                conversation_id,
                COUNT(*) as message_count,
                COALESCE(SUM(input_tokens), 0)::INT8 as total_input_tokens,
                COALESCE(SUM(output_tokens), 0)::INT8 as total_output_tokens,
                COALESCE(SUM(input_tokens + output_tokens), 0)::INT8 as total_tokens,
                COALESCE(SUM(estimated_cost_microcents), 0)::INT8 as total_cost_microcents,
                COALESCE(AVG(latency_ms)::float8, 0) as avg_latency_ms,
                string_agg(DISTINCT model, ',') as models_used,
                MIN(timestamp)::text as first_at,
                MAX(timestamp)::text as last_at
            FROM ai_usage_logs
            WHERE {} AND conversation_id IS NOT NULL
            GROUP BY conversation_id
            ORDER BY MAX(timestamp) DESC
            LIMIT ${}"#,
            where_clause, next_param
        );

        let rows = ConversationSummaryRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            values,
        ))
        .all(self.db.as_ref())
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| ConversationSummary {
                conversation_id: r.conversation_id.unwrap_or_default(),
                message_count: r.message_count.unwrap_or(0),
                total_input_tokens: r.total_input_tokens.unwrap_or(0),
                total_output_tokens: r.total_output_tokens.unwrap_or(0),
                total_tokens: r.total_tokens.unwrap_or(0),
                total_cost_microcents: r.total_cost_microcents.unwrap_or(0),
                avg_latency_ms: r.avg_latency_ms.unwrap_or(0.0),
                models_used: r
                    .models_used
                    .unwrap_or_default()
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect(),
                first_at: r.first_at.unwrap_or_default(),
                last_at: r.last_at.unwrap_or_default(),
            })
            .collect())
    }

    /// Get all invocations for a specific conversation.
    ///
    /// `caller_user_id`: `None` runs an unscoped (admin) lookup; `Some(id)`
    /// restricts results to invocations owned by that user, so ordinary
    /// callers cannot read another user's conversation by guessing its ID.
    pub async fn get_conversation_detail(
        &self,
        conversation_id: &str,
        limit: u64,
        caller_user_id: Option<i32>,
    ) -> Result<Vec<UsageLogEntry>, AiGatewayError> {
        let rows = UsageLogRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"SELECT
                id,
                timestamp::text,
                provider,
                model,
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
                latency_ms,
                estimated_cost_microcents,
                status,
                is_streaming,
                is_byok,
                conversation_id,
                array_to_string(tags, ',') as tags,
                request_id,
                trace_id,
                project_id,
                token_id,
                price_known
            FROM ai_usage_logs
            WHERE conversation_id = $1
              AND ($3::int IS NULL OR user_id = $3)
            ORDER BY timestamp ASC
            LIMIT $2"#,
            [
                conversation_id.into(),
                (limit as i64).into(),
                caller_user_id.into(),
            ],
        ))
        .all(self.db.as_ref())
        .await?;

        Ok(rows.into_iter().map(usage_log_from_row).collect())
    }

    /// Build a parameterized WHERE clause from time range + filters.
    /// Returns (clause_string, values_vec) with $1, $2, ... placeholders.
    fn build_filter_clause(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        filter: &UsageFilter,
    ) -> (String, Vec<sea_orm::Value>) {
        let mut conditions = vec!["timestamp >= $1".to_string(), "timestamp < $2".to_string()];
        let mut values: Vec<sea_orm::Value> = vec![from.into(), to.into()];
        let mut param_idx = 3;

        if let Some(user_id) = filter.user_id {
            conditions.push(format!("user_id = ${}", param_idx));
            values.push(user_id.into());
            param_idx += 1;
        }

        if let Some(project_id) = filter.project_id {
            conditions.push(format!("project_id = ${}", param_idx));
            values.push(project_id.into());
            param_idx += 1;
        }

        if let Some(ref conv_id) = filter.conversation_id {
            conditions.push(format!("conversation_id = ${}", param_idx));
            values.push(conv_id.clone().into());
            param_idx += 1;
        }

        if let Some(ref tags_str) = filter.tags {
            let tags: Vec<String> = tags_str
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
            for tag in tags {
                conditions.push(format!("${} = ANY(tags)", param_idx));
                values.push(tag.into());
                param_idx += 1;
            }
        }

        if let Some(ref model) = filter.model {
            conditions.push(format!("model = ${}", param_idx));
            values.push(model.clone().into());
            param_idx += 1;
        }

        if let Some(ref provider) = filter.provider {
            conditions.push(format!("provider = ${}", param_idx));
            values.push(provider.clone().into());
            param_idx += 1;
        }

        if let Some(status) = filter.status {
            conditions.push(format!("status = ${}", param_idx));
            values.push(status.into());
            param_idx += 1;
        }

        if let Some(cost) = filter.cost_gte {
            conditions.push(format!("estimated_cost_microcents >= ${}", param_idx));
            values.push(cost.into());
            param_idx += 1;
        }

        if let Some(cost) = filter.cost_gt {
            conditions.push(format!("estimated_cost_microcents > ${}", param_idx));
            values.push(cost.into());
            param_idx += 1;
        }

        if let Some(cost) = filter.cost_lte {
            conditions.push(format!("estimated_cost_microcents <= ${}", param_idx));
            values.push(cost.into());
            param_idx += 1;
        }

        if let Some(cost) = filter.cost_lt {
            conditions.push(format!("estimated_cost_microcents < ${}", param_idx));
            values.push(cost.into());
            param_idx += 1;
        }

        if let Some(tokens) = filter.tokens_gte {
            conditions.push(format!("(input_tokens + output_tokens) >= ${}", param_idx));
            values.push(tokens.into());
            param_idx += 1;
        }

        if let Some(tokens) = filter.tokens_gt {
            conditions.push(format!("(input_tokens + output_tokens) > ${}", param_idx));
            values.push(tokens.into());
            param_idx += 1;
        }

        if let Some(tokens) = filter.tokens_lte {
            conditions.push(format!("(input_tokens + output_tokens) <= ${}", param_idx));
            values.push(tokens.into());
            param_idx += 1;
        }

        if let Some(tokens) = filter.tokens_lt {
            conditions.push(format!("(input_tokens + output_tokens) < ${}", param_idx));
            values.push(tokens.into());
            param_idx += 1;
        }

        let _ = param_idx; // param_idx is reserved for any future conditions
        (conditions.join(" AND "), values)
    }
}

/// Shift all `$N` parameter placeholders in a SQL fragment by `offset`.
fn shift_params(sql: &str, offset: usize) -> String {
    let mut result = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '$' {
            let mut num_str = String::new();
            while let Some(&digit) = chars.peek() {
                if digit.is_ascii_digit() {
                    num_str.push(digit);
                    chars.next();
                } else {
                    break;
                }
            }
            if let Ok(n) = num_str.parse::<usize>() {
                result.push('$');
                result.push_str(&(n + offset).to_string());
            } else {
                result.push('$');
                result.push_str(&num_str);
            }
        } else {
            result.push(ch);
        }
    }
    result
}

fn usage_log_from_row(r: UsageLogRow) -> UsageLogEntry {
    UsageLogEntry {
        id: r.id.unwrap_or(0),
        timestamp: r.timestamp.unwrap_or_default(),
        provider: r.provider.unwrap_or_default(),
        model: r.model.unwrap_or_default(),
        input_tokens: r.input_tokens.unwrap_or(0),
        output_tokens: r.output_tokens.unwrap_or(0),
        cache_read_tokens: r.cache_read_tokens.unwrap_or(0),
        cache_write_tokens: r.cache_write_tokens.unwrap_or(0),
        latency_ms: r.latency_ms.unwrap_or(0),
        estimated_cost_microcents: r.estimated_cost_microcents.unwrap_or(0),
        unpriced: is_unpriced(
            r.price_known,
            r.estimated_cost_microcents.unwrap_or(0),
            r.input_tokens.unwrap_or(0),
            r.output_tokens.unwrap_or(0),
        ),
        status: r.status.unwrap_or(0),
        is_streaming: r.is_streaming.unwrap_or(false),
        is_byok: r.is_byok.unwrap_or(false),
        conversation_id: r.conversation_id,
        tags: r
            .tags
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect(),
        request_id: r.request_id,
        trace_id: r.trace_id,
        project_id: r.project_id,
        token_id: r.token_id,
    }
}

#[cfg(test)]
mod tests;
