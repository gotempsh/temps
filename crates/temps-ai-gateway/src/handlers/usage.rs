// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use axum::{
    extract::{Path, Query, State},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::sync::Arc;
use temps_auth::permission_guard;
use temps_auth::permissions::Permission;
use temps_auth::{AuthContext, RequireAuth};
use temps_core::problemdetails::{Problem, ProblemDetails};
use utoipa::{OpenApi, ToSchema};

use crate::error::AiGatewayError;
use crate::handlers::types::AiGatewayAppState;
use crate::services::usage_service::{
    ConversationSummary, ModelUsage, ProviderUsage, TimeseriesBucket, UsageFilter, UsageLogEntry,
    UsageLogPage, UsageSummary,
};

// ============================================================================
// OpenAPI schema
// ============================================================================

#[derive(OpenApi)]
#[openapi(
    paths(
        get_usage_summary,
        get_usage_by_provider,
        get_usage_timeseries,
        get_usage_top_models,
        get_usage_recent,
        get_conversations,
        get_conversation_detail,
    ),
    components(schemas(
        UsageQueryParams,
        TimeseriesQueryParams,
        TopModelsQueryParams,
        RecentQueryParams,
        ConversationsQueryParams,
        UsageSummary,
        ProviderUsage,
        TimeseriesBucket,
        ModelUsage,
        UsageLogEntry,
        UsageLogPage,
        ConversationSummary,
        UsageFilter,
    )),
    info(
        title = "AI Gateway Usage API",
        description = "Usage analytics endpoints for the AI gateway",
        version = "1.0.0"
    ),
    tags(
        (name = "AI Gateway Usage", description = "Usage analytics and reporting endpoints")
    )
)]
pub struct AiGatewayUsageApiDoc;

pub fn configure_usage_routes() -> Router<Arc<AiGatewayAppState>> {
    Router::new()
        .route("/ai/usage/summary", get(get_usage_summary))
        .route("/ai/usage/by-provider", get(get_usage_by_provider))
        .route("/ai/usage/timeseries", get(get_usage_timeseries))
        .route("/ai/usage/top-models", get(get_usage_top_models))
        .route("/ai/usage/recent", get(get_usage_recent))
        .route("/ai/usage/conversations", get(get_conversations))
        .route(
            "/ai/usage/conversations/{conversation_id}",
            get(get_conversation_detail),
        )
}

// ============================================================================
// Query param structs
// ============================================================================

#[derive(Debug, Deserialize, ToSchema)]
pub struct UsageQueryParams {
    /// ISO 8601 start time (defaults to 24h ago)
    pub from: Option<String>,
    /// ISO 8601 end time (defaults to now)
    pub to: Option<String>,
    /// Filter by user ID
    pub user_id: Option<i32>,
    /// Filter by conversation ID
    pub conversation_id: Option<String>,
    /// Filter by tags (comma-separated, AND logic)
    pub tags: Option<String>,
    /// Filter by model name
    pub model: Option<String>,
    /// Filter by provider name
    pub provider: Option<String>,
}

impl UsageQueryParams {
    fn to_filter(&self) -> UsageFilter {
        UsageFilter {
            user_id: self.user_id,
            conversation_id: self.conversation_id.clone(),
            tags: self.tags.clone(),
            model: self.model.clone(),
            provider: self.provider.clone(),
            ..Default::default()
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct TimeseriesQueryParams {
    /// ISO 8601 start time (defaults to 24h ago)
    pub from: Option<String>,
    /// ISO 8601 end time (defaults to now)
    pub to: Option<String>,
    /// Bucket size: "hour", "day", "week" (defaults to "day")
    pub bucket: Option<String>,
    /// Filter by user ID
    pub user_id: Option<i32>,
    /// Filter by conversation ID
    pub conversation_id: Option<String>,
    /// Filter by tags (comma-separated, AND logic)
    pub tags: Option<String>,
    /// Filter by model name
    pub model: Option<String>,
    /// Filter by provider name
    pub provider: Option<String>,
}

impl TimeseriesQueryParams {
    fn to_filter(&self) -> UsageFilter {
        UsageFilter {
            user_id: self.user_id,
            conversation_id: self.conversation_id.clone(),
            tags: self.tags.clone(),
            model: self.model.clone(),
            provider: self.provider.clone(),
            ..Default::default()
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct TopModelsQueryParams {
    /// ISO 8601 start time (defaults to 24h ago)
    pub from: Option<String>,
    /// ISO 8601 end time (defaults to now)
    pub to: Option<String>,
    /// Max results (defaults to 10)
    pub limit: Option<u64>,
    /// Filter by user ID
    pub user_id: Option<i32>,
    /// Filter by tags (comma-separated, AND logic)
    pub tags: Option<String>,
}

impl TopModelsQueryParams {
    fn to_filter(&self) -> UsageFilter {
        UsageFilter {
            user_id: self.user_id,
            tags: self.tags.clone(),
            ..Default::default()
        }
    }
}

/// Page size for the recent-requests endpoint. Defaults to 20, capped at 50.
pub const RECENT_DEFAULT_LIMIT: u64 = 20;
pub const RECENT_MAX_LIMIT: u64 = 50;

#[derive(Debug, Deserialize, ToSchema)]
pub struct RecentQueryParams {
    /// Page size (defaults to 20, max 50)
    pub limit: Option<u64>,
    /// Number of results to skip for pagination (defaults to 0)
    pub offset: Option<u64>,
    /// Filter by user ID
    pub user_id: Option<i32>,
    /// Filter by conversation ID
    pub conversation_id: Option<String>,
    /// Filter by tags (comma-separated, AND logic)
    pub tags: Option<String>,
    /// Filter by model name
    pub model: Option<String>,
    /// Filter by provider name
    pub provider: Option<String>,
    /// Filter by HTTP status code (exact match)
    pub status: Option<i16>,
    /// Cost greater-than-or-equal, in microcents
    pub cost_gte: Option<i64>,
    /// Cost strictly greater-than, in microcents
    pub cost_gt: Option<i64>,
    /// Cost less-than-or-equal, in microcents
    pub cost_lte: Option<i64>,
    /// Cost strictly less-than, in microcents
    pub cost_lt: Option<i64>,
    /// Total tokens (input + output) greater-than-or-equal
    pub tokens_gte: Option<i64>,
    /// Total tokens (input + output) strictly greater-than
    pub tokens_gt: Option<i64>,
    /// Total tokens (input + output) less-than-or-equal
    pub tokens_lte: Option<i64>,
    /// Total tokens (input + output) strictly less-than
    pub tokens_lt: Option<i64>,
}

impl RecentQueryParams {
    fn to_filter(&self) -> UsageFilter {
        UsageFilter {
            user_id: self.user_id,
            conversation_id: self.conversation_id.clone(),
            tags: self.tags.clone(),
            model: self.model.clone(),
            provider: self.provider.clone(),
            status: self.status,
            cost_gte: self.cost_gte,
            cost_gt: self.cost_gt,
            cost_lte: self.cost_lte,
            cost_lt: self.cost_lt,
            tokens_gte: self.tokens_gte,
            tokens_gt: self.tokens_gt,
            tokens_lte: self.tokens_lte,
            tokens_lt: self.tokens_lt,
        }
    }

    /// Resolve the effective page size: default 20, clamped to [1, 50].
    fn resolved_limit(&self) -> u64 {
        self.limit
            .unwrap_or(RECENT_DEFAULT_LIMIT)
            .clamp(1, RECENT_MAX_LIMIT)
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ConversationsQueryParams {
    /// ISO 8601 start time (defaults to 24h ago)
    pub from: Option<String>,
    /// ISO 8601 end time (defaults to now)
    pub to: Option<String>,
    /// Max results (defaults to 50, max 100)
    pub limit: Option<u64>,
    /// Filter by user ID
    pub user_id: Option<i32>,
    /// Filter by tags (comma-separated, AND logic)
    pub tags: Option<String>,
    /// Filter by model name
    pub model: Option<String>,
}

impl ConversationsQueryParams {
    fn to_filter(&self) -> UsageFilter {
        UsageFilter {
            user_id: self.user_id,
            tags: self.tags.clone(),
            model: self.model.clone(),
            ..Default::default()
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ConversationDetailQueryParams {
    /// Max results (defaults to 100)
    pub limit: Option<u64>,
}

// ============================================================================
// Helpers
// ============================================================================

fn parse_time_range(
    from: Option<&str>,
    to: Option<&str>,
) -> Result<(DateTime<Utc>, DateTime<Utc>), AiGatewayError> {
    let now = Utc::now();
    let from = match from {
        Some(s) => DateTime::parse_from_rfc3339(s)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|_| AiGatewayError::Validation {
                message: format!("Invalid 'from' timestamp: '{}'. Use ISO 8601 format.", s),
            })?,
        None => now - chrono::Duration::hours(24),
    };
    let to = match to {
        Some(s) => DateTime::parse_from_rfc3339(s)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|_| AiGatewayError::Validation {
                message: format!("Invalid 'to' timestamp: '{}'. Use ISO 8601 format.", s),
            })?,
        None => now,
    };
    Ok((from, to))
}

/// Resolve the scope a caller is authorized to query: `Ok(None)` means the
/// caller (a `SystemAdmin`) may run instance-wide queries; `Ok(Some(id))`
/// pins the caller to their own `user_id`. Errors if the credential has no
/// user identity to scope to (e.g. a deployment token) and isn't an admin.
fn resolve_caller_scope(auth: &AuthContext) -> Result<Option<i32>, Problem> {
    if auth.has_permission(&Permission::SystemAdmin) {
        return Ok(None);
    }

    let Some(caller_id) = auth.user_id_opt() else {
        return Err(temps_core::error_builder::ErrorBuilder::new(
            ::axum::http::StatusCode::FORBIDDEN,
        )
        .type_("https://temps.sh/probs/ai-usage-scope-required")
        .title("AI Usage Scope Required")
        .detail(
            "This credential cannot query instance-wide AI usage; authenticate as a user or an instance administrator",
        )
        .permission_denial(
            temps_core::problemdetails::PermissionDenialKind::InsufficientPermission,
            Some(Permission::SystemAdmin.to_string()),
        )
        .build());
    };

    Ok(Some(caller_id))
}

/// Confine AI usage queries so ordinary callers cannot read instance-wide or
/// cross-user aggregates. Mirrors unscoped proxy-log authorization: only
/// `SystemAdmin` may omit a user scope; everyone else is pinned to their own
/// `user_id` (and cannot request someone else's).
fn scope_usage_filter_for_caller(
    auth: &AuthContext,
    filter: &mut UsageFilter,
) -> Result<(), Problem> {
    let Some(caller_id) = resolve_caller_scope(auth)? else {
        return Ok(());
    };

    match filter.user_id {
        Some(requested) if requested != caller_id => Err(
            temps_core::error_builder::ErrorBuilder::new(::axum::http::StatusCode::FORBIDDEN)
                .type_("https://temps.sh/probs/cross-user-ai-usage-denied")
                .title("Cross-User AI Usage Denied")
                .detail("You may only query AI usage for your own account")
                .permission_denial(
                    temps_core::problemdetails::PermissionDenialKind::InsufficientPermission,
                    None,
                )
                .build(),
        ),
        Some(_) => Ok(()),
        None => {
            filter.user_id = Some(caller_id);
            Ok(())
        }
    }
}

// ============================================================================
// Handlers
// ============================================================================

#[utoipa::path(
    tag = "AI Gateway Usage",
    get,
    path = "/ai/usage/summary",
    params(
        ("from" = Option<String>, Query, description = "ISO 8601 start time (defaults to 24h ago)"),
        ("to" = Option<String>, Query, description = "ISO 8601 end time (defaults to now)"),
    ),
    responses(
        (status = 200, description = "Usage summary for the time range", body = UsageSummary),
        (status = 400, description = "Invalid query parameters", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn get_usage_summary(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Query(params): Query<UsageQueryParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayRead);

    let (from, to) = parse_time_range(params.from.as_deref(), params.to.as_deref())?;
    let mut filter = params.to_filter();
    scope_usage_filter_for_caller(&auth, &mut filter)?;
    let summary = app_state
        .usage_service
        .get_summary_filtered(from, to, &filter)
        .await?;

    Ok(Json(summary))
}

#[utoipa::path(
    tag = "AI Gateway Usage",
    get,
    path = "/ai/usage/by-provider",
    params(
        ("from" = Option<String>, Query, description = "ISO 8601 start time (defaults to 24h ago)"),
        ("to" = Option<String>, Query, description = "ISO 8601 end time (defaults to now)"),
    ),
    responses(
        (status = 200, description = "Usage broken down by provider", body = Vec<ProviderUsage>),
        (status = 400, description = "Invalid query parameters", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn get_usage_by_provider(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Query(params): Query<UsageQueryParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayRead);

    let (from, to) = parse_time_range(params.from.as_deref(), params.to.as_deref())?;
    let mut filter = params.to_filter();
    scope_usage_filter_for_caller(&auth, &mut filter)?;
    let usage = app_state
        .usage_service
        .get_by_provider_filtered(from, to, &filter)
        .await?;

    Ok(Json(usage))
}

#[utoipa::path(
    tag = "AI Gateway Usage",
    get,
    path = "/ai/usage/timeseries",
    params(
        ("from" = Option<String>, Query, description = "ISO 8601 start time (defaults to 24h ago)"),
        ("to" = Option<String>, Query, description = "ISO 8601 end time (defaults to now)"),
        ("bucket" = Option<String>, Query, description = "Bucket size: hour, day, week (defaults to day)"),
    ),
    responses(
        (status = 200, description = "Time-series usage data", body = Vec<TimeseriesBucket>),
        (status = 400, description = "Invalid query parameters", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn get_usage_timeseries(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Query(params): Query<TimeseriesQueryParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayRead);

    let (from, to) = parse_time_range(params.from.as_deref(), params.to.as_deref())?;
    let bucket = params.bucket.as_deref().unwrap_or("day");
    let mut filter = params.to_filter();
    scope_usage_filter_for_caller(&auth, &mut filter)?;
    let timeseries = app_state
        .usage_service
        .get_timeseries_filtered(from, to, bucket, &filter)
        .await?;

    Ok(Json(timeseries))
}

#[utoipa::path(
    tag = "AI Gateway Usage",
    get,
    path = "/ai/usage/top-models",
    params(
        ("from" = Option<String>, Query, description = "ISO 8601 start time (defaults to 24h ago)"),
        ("to" = Option<String>, Query, description = "ISO 8601 end time (defaults to now)"),
        ("limit" = Option<u64>, Query, description = "Max results (defaults to 10)"),
    ),
    responses(
        (status = 200, description = "Top models by request count", body = Vec<ModelUsage>),
        (status = 400, description = "Invalid query parameters", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn get_usage_top_models(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Query(params): Query<TopModelsQueryParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayRead);

    let (from, to) = parse_time_range(params.from.as_deref(), params.to.as_deref())?;
    let limit = std::cmp::min(params.limit.unwrap_or(10), 100);
    let mut filter = params.to_filter();
    scope_usage_filter_for_caller(&auth, &mut filter)?;
    let models = app_state
        .usage_service
        .get_top_models_filtered(from, to, limit, &filter)
        .await?;

    Ok(Json(models))
}

#[utoipa::path(
    tag = "AI Gateway Usage",
    get,
    path = "/ai/usage/recent",
    params(
        ("limit" = Option<u64>, Query, description = "Page size (defaults to 20, max 50)"),
        ("offset" = Option<u64>, Query, description = "Number of results to skip for pagination (defaults to 0)"),
        ("provider" = Option<String>, Query, description = "Filter by provider name"),
        ("model" = Option<String>, Query, description = "Filter by model name"),
        ("status" = Option<i16>, Query, description = "Filter by HTTP status code (exact match)"),
        ("cost_gte" = Option<i64>, Query, description = "Cost greater-than-or-equal, in microcents"),
        ("cost_gt" = Option<i64>, Query, description = "Cost strictly greater-than, in microcents"),
        ("cost_lte" = Option<i64>, Query, description = "Cost less-than-or-equal, in microcents"),
        ("cost_lt" = Option<i64>, Query, description = "Cost strictly less-than, in microcents"),
        ("tokens_gte" = Option<i64>, Query, description = "Total tokens greater-than-or-equal"),
        ("tokens_gt" = Option<i64>, Query, description = "Total tokens strictly greater-than"),
        ("tokens_lte" = Option<i64>, Query, description = "Total tokens less-than-or-equal"),
        ("tokens_lt" = Option<i64>, Query, description = "Total tokens strictly less-than"),
    ),
    responses(
        (status = 200, description = "Page of recent usage log entries", body = UsageLogPage),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn get_usage_recent(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Query(params): Query<RecentQueryParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayRead);

    let limit = params.resolved_limit();
    let offset = params.offset.unwrap_or(0);
    let mut filter = params.to_filter();
    scope_usage_filter_for_caller(&auth, &mut filter)?;
    let page = app_state
        .usage_service
        .get_recent_filtered(limit, offset, &filter)
        .await?;

    Ok(Json(page))
}

#[utoipa::path(
    tag = "AI Gateway Usage",
    get,
    path = "/ai/usage/conversations",
    params(
        ("from" = Option<String>, Query, description = "ISO 8601 start time (defaults to 24h ago)"),
        ("to" = Option<String>, Query, description = "ISO 8601 end time (defaults to now)"),
        ("limit" = Option<u64>, Query, description = "Max results (defaults to 50, max 100)"),
        ("user_id" = Option<i32>, Query, description = "Filter by user ID"),
        ("tags" = Option<String>, Query, description = "Filter by tags (comma-separated)"),
        ("model" = Option<String>, Query, description = "Filter by model name"),
    ),
    responses(
        (status = 200, description = "Conversation summaries", body = Vec<ConversationSummary>),
        (status = 400, description = "Invalid query parameters", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn get_conversations(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Query(params): Query<ConversationsQueryParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayRead);

    let (from, to) = parse_time_range(params.from.as_deref(), params.to.as_deref())?;
    let limit = std::cmp::min(params.limit.unwrap_or(50), 100);
    let mut filter = params.to_filter();
    scope_usage_filter_for_caller(&auth, &mut filter)?;
    let conversations = app_state
        .usage_service
        .get_conversations(from, to, &filter, limit)
        .await?;

    Ok(Json(conversations))
}

#[utoipa::path(
    tag = "AI Gateway Usage",
    get,
    path = "/ai/usage/conversations/{conversation_id}",
    params(
        ("conversation_id" = String, Path, description = "Conversation ID"),
        ("limit" = Option<u64>, Query, description = "Max results (defaults to 100)"),
    ),
    responses(
        (status = 200, description = "Invocations within a conversation", body = Vec<UsageLogEntry>),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn get_conversation_detail(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Path(conversation_id): Path<String>,
    Query(params): Query<ConversationDetailQueryParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayRead);

    let caller_scope = resolve_caller_scope(&auth)?;
    let limit = std::cmp::min(params.limit.unwrap_or(100), 500);
    let entries = app_state
        .usage_service
        .get_conversation_detail(&conversation_id, limit, caller_scope)
        .await?;

    Ok(Json(entries))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Timelike};

    #[test]
    fn test_parse_time_range_defaults() {
        let (from, to) = parse_time_range(None, None).unwrap();
        let now = Utc::now();
        // `from` should be approximately 24h ago
        let diff = now - from;
        assert!(diff.num_hours() >= 23 && diff.num_hours() <= 25);
        // `to` should be approximately now
        let diff_to = now - to;
        assert!(diff_to.num_seconds().abs() < 5);
    }

    #[test]
    fn test_parse_time_range_with_valid_from() {
        let (from, _to) = parse_time_range(Some("2025-01-15T00:00:00Z"), None).unwrap();
        assert_eq!(from.year(), 2025);
        assert_eq!(from.month(), 1);
        assert_eq!(from.day(), 15);
    }

    #[test]
    fn test_parse_time_range_with_valid_to() {
        let (_from, to) = parse_time_range(None, Some("2025-06-01T12:00:00Z")).unwrap();
        assert_eq!(to.year(), 2025);
        assert_eq!(to.month(), 6);
        assert_eq!(to.day(), 1);
        assert_eq!(to.hour(), 12);
    }

    #[test]
    fn test_parse_time_range_both_specified() {
        let (from, to) =
            parse_time_range(Some("2025-01-01T00:00:00Z"), Some("2025-01-31T23:59:59Z")).unwrap();
        assert_eq!(from.year(), 2025);
        assert_eq!(from.month(), 1);
        assert_eq!(from.day(), 1);
        assert_eq!(to.day(), 31);
    }

    #[test]
    fn test_parse_time_range_with_timezone_offset() {
        let (from, _to) = parse_time_range(Some("2025-01-15T10:00:00+05:00"), None).unwrap();
        // Should be converted to UTC: 10:00 +05:00 = 05:00 UTC
        assert_eq!(from.hour(), 5);
    }

    #[test]
    fn test_parse_time_range_invalid_from() {
        let result = parse_time_range(Some("not-a-date"), None);
        assert!(result.is_err());
        match result.unwrap_err() {
            AiGatewayError::Validation { message } => {
                assert!(message.contains("Invalid 'from' timestamp"));
                assert!(message.contains("not-a-date"));
            }
            other => panic!("Expected Validation error, got: {:?}", other),
        }
    }

    #[test]
    fn test_parse_time_range_invalid_to() {
        let result = parse_time_range(None, Some("bad-date"));
        assert!(result.is_err());
        match result.unwrap_err() {
            AiGatewayError::Validation { message } => {
                assert!(message.contains("Invalid 'to' timestamp"));
                assert!(message.contains("bad-date"));
            }
            other => panic!("Expected Validation error, got: {:?}", other),
        }
    }

    #[test]
    fn test_parse_time_range_empty_string_from() {
        let result = parse_time_range(Some(""), None);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            AiGatewayError::Validation { .. }
        ));
    }

    #[test]
    fn test_recent_query_params_defaults() {
        let json = "{}";
        let params: RecentQueryParams = serde_json::from_str(json).unwrap();
        assert!(params.limit.is_none());
        assert!(params.offset.is_none());
        assert!(params.status.is_none());
        assert!(params.cost_gte.is_none());
        // Unspecified limit resolves to the default page size.
        assert_eq!(params.resolved_limit(), RECENT_DEFAULT_LIMIT);
    }

    #[test]
    fn test_recent_query_params_limit_clamped_to_max() {
        let params: RecentQueryParams = serde_json::from_str(r#"{"limit": 500}"#).unwrap();
        assert_eq!(params.resolved_limit(), RECENT_MAX_LIMIT);
    }

    #[test]
    fn test_recent_query_params_limit_floor_is_one() {
        let params: RecentQueryParams = serde_json::from_str(r#"{"limit": 0}"#).unwrap();
        assert_eq!(params.resolved_limit(), 1);
    }

    #[test]
    fn test_recent_query_params_filters_map_to_usage_filter() {
        let params: RecentQueryParams = serde_json::from_str(
            r#"{"provider": "openai", "status": 429, "cost_gte": 100, "cost_lt": 5000,
                "tokens_gte": 500, "tokens_lt": 10000}"#,
        )
        .unwrap();
        let filter = params.to_filter();
        assert_eq!(filter.provider.as_deref(), Some("openai"));
        assert_eq!(filter.status, Some(429));
        assert_eq!(filter.cost_gte, Some(100));
        assert_eq!(filter.cost_lt, Some(5000));
        assert_eq!(filter.cost_gt, None);
        assert_eq!(filter.tokens_gte, Some(500));
        assert_eq!(filter.tokens_lt, Some(10000));
        assert_eq!(filter.tokens_gt, None);
    }

    #[test]
    fn test_timeseries_query_params_defaults() {
        let json = "{}";
        let params: TimeseriesQueryParams = serde_json::from_str(json).unwrap();
        assert!(params.from.is_none());
        assert!(params.to.is_none());
        assert!(params.bucket.is_none());
    }

    #[test]
    fn test_top_models_query_params_with_limit() {
        let json = r#"{"limit": 5}"#;
        let params: TopModelsQueryParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.limit, Some(5));
    }

    #[test]
    fn test_usage_query_params_with_times() {
        let json = r#"{"from": "2025-01-01T00:00:00Z", "to": "2025-01-31T23:59:59Z"}"#;
        let params: UsageQueryParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.from.as_deref(), Some("2025-01-01T00:00:00Z"));
        assert_eq!(params.to.as_deref(), Some("2025-01-31T23:59:59Z"));
    }

    use axum::http::StatusCode;
    use temps_auth::permissions::Role;
    use temps_entities::users;

    fn test_auth(role: Role) -> AuthContext {
        let now = chrono::Utc::now();
        AuthContext::new_session(
            users::Model {
                id: 42,
                name: "AI Usage Tester".to_string(),
                email: "ai-usage-tester@example.com".to_string(),
                password_hash: None,
                email_verified: true,
                email_verification_token: None,
                email_verification_expires: None,
                password_reset_token: None,
                password_reset_expires: None,
                must_change_password: false,
                deleted_at: None,
                mfa_secret: None,
                mfa_enabled: false,
                mfa_recovery_codes: None,
                oidc_subject: None,
                oidc_provider_id: None,
                created_at: now,
                updated_at: now,
            },
            role,
        )
    }

    #[test]
    fn ordinary_user_unscoped_usage_query_is_pinned_to_self() {
        let auth = test_auth(Role::User);
        let mut filter = UsageFilter::default();
        scope_usage_filter_for_caller(&auth, &mut filter).expect("user may query own usage");
        assert_eq!(filter.user_id, Some(42));
    }

    #[test]
    fn ordinary_user_cannot_query_another_users_usage() {
        let auth = test_auth(Role::User);
        let mut filter = UsageFilter {
            user_id: Some(99),
            ..Default::default()
        };
        let err = scope_usage_filter_for_caller(&auth, &mut filter)
            .expect_err("cross-user usage must be denied");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
        assert_eq!(
            filter.user_id,
            Some(99),
            "rejected filter must stay unchanged"
        );
    }

    #[test]
    fn ordinary_user_may_query_own_usage_explicitly() {
        let auth = test_auth(Role::User);
        let mut filter = UsageFilter {
            user_id: Some(42),
            ..Default::default()
        };
        scope_usage_filter_for_caller(&auth, &mut filter).expect("own user_id is allowed");
        assert_eq!(filter.user_id, Some(42));
    }

    #[test]
    fn system_admin_may_run_unscoped_usage_query() {
        let auth = test_auth(Role::Admin);
        let mut filter = UsageFilter::default();
        scope_usage_filter_for_caller(&auth, &mut filter).expect("admin may query instance-wide");
        assert!(filter.user_id.is_none());
    }

    #[test]
    fn deployment_token_cannot_run_unscoped_usage_query() {
        use temps_entities::deployment_tokens::DeploymentTokenPermission;
        let auth = AuthContext::new_deployment_token(
            7,
            None,
            None,
            1,
            "tok".to_string(),
            vec![DeploymentTokenPermission::AiGatewayExecute],
        );
        let mut filter = UsageFilter::default();
        let err = scope_usage_filter_for_caller(&auth, &mut filter)
            .expect_err("deployment tokens lack a user scope");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    #[test]
    fn resolve_caller_scope_pins_ordinary_user_to_self() {
        let auth = test_auth(Role::User);
        let scope = resolve_caller_scope(&auth).expect("ordinary user has a scope");
        assert_eq!(scope, Some(42));
    }

    #[test]
    fn resolve_caller_scope_is_unscoped_for_admin() {
        let auth = test_auth(Role::Admin);
        let scope = resolve_caller_scope(&auth).expect("admin may query instance-wide");
        assert_eq!(scope, None);
    }

    #[test]
    fn resolve_caller_scope_denies_deployment_token() {
        use temps_entities::deployment_tokens::DeploymentTokenPermission;
        let auth = AuthContext::new_deployment_token(
            7,
            None,
            None,
            1,
            "tok".to_string(),
            vec![DeploymentTokenPermission::AiGatewayExecute],
        );
        let err = resolve_caller_scope(&auth).expect_err("deployment tokens lack a user scope");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }
}
