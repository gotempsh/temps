// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, DatabaseBackend, DatabaseConnection, FromQueryResult, Set, Statement,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_entities::ai_usage_logs;
use utoipa::ToSchema;

use crate::error::AiGatewayError;

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
}

#[derive(Debug, Serialize, ToSchema)]
pub struct UsageLogEntry {
    pub id: i64,
    pub timestamp: String,
    pub provider: String,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub latency_ms: i32,
    pub estimated_cost_microcents: i64,
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
}

#[derive(Debug, FromQueryResult)]
struct UsageLogRow {
    id: Option<i64>,
    timestamp: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    latency_ms: Option<i32>,
    estimated_cost_microcents: Option<i64>,
    status: Option<i16>,
    is_streaming: Option<bool>,
    is_byok: Option<bool>,
    conversation_id: Option<String>,
    tags: Option<String>,
    request_id: Option<String>,
    trace_id: Option<String>,
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
        context: &AiRequestContext,
    ) -> Result<(), AiGatewayError> {
        let record = ai_usage_logs::ActiveModel {
            timestamp: Set(chrono::Utc::now()),
            user_id: Set(user_id),
            provider: Set(provider.to_string()),
            model: Set(model.to_string()),
            input_tokens: Set(input_tokens),
            output_tokens: Set(output_tokens),
            latency_ms: Set(latency_ms),
            estimated_cost_microcents: Set(estimated_cost_microcents),
            status: Set(status),
            is_streaming: Set(is_streaming),
            is_byok: Set(is_byok),
            conversation_id: Set(context.conversation_id.clone()),
            tags: Set(context.tags.clone()),
            request_id: Set(context.request_id.clone()),
            trace_id: Set(context.trace_id.clone()),
            ..Default::default()
        };

        record.insert(self.db.as_ref()).await?;
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
                COUNT(*) FILTER (WHERE is_byok = true) as byok_count
            FROM ai_usage_logs
            WHERE {}"#,
            where_clause
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
                COALESCE(AVG(latency_ms)::float8, 0) as avg_latency_ms
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
                latency_ms,
                estimated_cost_microcents,
                status,
                is_streaming,
                is_byok,
                conversation_id,
                array_to_string(tags, ',') as tags,
                request_id,
                trace_id
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
                latency_ms,
                estimated_cost_microcents,
                status,
                is_streaming,
                is_byok,
                conversation_id,
                array_to_string(tags, ',') as tags,
                request_id,
                trace_id
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
        latency_ms: r.latency_ms.unwrap_or(0),
        estimated_cost_microcents: r.estimated_cost_microcents.unwrap_or(0),
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shift_params_basic() {
        assert_eq!(shift_params("$1 AND $2", 1), "$2 AND $3");
    }

    #[test]
    fn test_shift_params_no_params() {
        assert_eq!(shift_params("SELECT 1", 5), "SELECT 1");
    }

    #[test]
    fn test_shift_params_multiple() {
        assert_eq!(
            shift_params("$1 = ANY(tags) AND $2 < $3", 2),
            "$3 = ANY(tags) AND $4 < $5"
        );
    }

    #[test]
    fn test_shift_params_preserves_dollar_without_number() {
        assert_eq!(shift_params("$$ BEGIN END $$", 1), "$$ BEGIN END $$");
    }

    #[test]
    fn test_build_filter_clause_time_range_only() {
        let db = sea_orm::DatabaseConnection::Disconnected;
        let service = UsageService::new(Arc::new(db));
        let from = Utc::now() - chrono::Duration::hours(1);
        let to = Utc::now();
        let filter = UsageFilter::default();

        let (clause, values) = service.build_filter_clause(from, to, &filter);
        assert_eq!(clause, "timestamp >= $1 AND timestamp < $2");
        assert_eq!(values.len(), 2);
    }

    #[test]
    fn test_build_filter_clause_with_user_id() {
        let db = sea_orm::DatabaseConnection::Disconnected;
        let service = UsageService::new(Arc::new(db));
        let from = Utc::now() - chrono::Duration::hours(1);
        let to = Utc::now();
        let filter = UsageFilter {
            user_id: Some(42),
            ..Default::default()
        };

        let (clause, values) = service.build_filter_clause(from, to, &filter);
        assert!(clause.contains("user_id = $3"));
        assert_eq!(values.len(), 3);
    }

    #[test]
    fn test_build_filter_clause_with_tags() {
        let db = sea_orm::DatabaseConnection::Disconnected;
        let service = UsageService::new(Arc::new(db));
        let from = Utc::now() - chrono::Duration::hours(1);
        let to = Utc::now();
        let filter = UsageFilter {
            tags: Some("agent:support,env:prod".to_string()),
            ..Default::default()
        };

        let (clause, values) = service.build_filter_clause(from, to, &filter);
        assert!(clause.contains("$3 = ANY(tags)"));
        assert!(clause.contains("$4 = ANY(tags)"));
        assert_eq!(values.len(), 4);
    }

    #[test]
    fn test_build_filter_clause_all_filters() {
        let db = sea_orm::DatabaseConnection::Disconnected;
        let service = UsageService::new(Arc::new(db));
        let from = Utc::now() - chrono::Duration::hours(1);
        let to = Utc::now();
        let filter = UsageFilter {
            user_id: Some(42),
            conversation_id: Some("conv_abc".to_string()),
            tags: Some("agent:support".to_string()),
            model: Some("claude-sonnet-4-6".to_string()),
            provider: Some("anthropic".to_string()),
            ..Default::default()
        };

        let (clause, values) = service.build_filter_clause(from, to, &filter);
        assert!(clause.contains("user_id = $3"));
        assert!(clause.contains("conversation_id = $4"));
        assert!(clause.contains("$5 = ANY(tags)"));
        assert!(clause.contains("model = $6"));
        assert!(clause.contains("provider = $7"));
        assert_eq!(values.len(), 7);
    }

    #[test]
    fn test_build_filter_clause_with_status_and_cost_bounds() {
        let db = sea_orm::DatabaseConnection::Disconnected;
        let service = UsageService::new(Arc::new(db));
        let from = Utc::now() - chrono::Duration::hours(1);
        let to = Utc::now();
        let filter = UsageFilter {
            provider: Some("openai".to_string()),
            status: Some(429),
            cost_gte: Some(100),
            cost_lt: Some(50_000),
            ..Default::default()
        };

        let (clause, values) = service.build_filter_clause(from, to, &filter);
        // $1/$2 are the time range; provider, status, cost_gte, cost_lt follow.
        assert!(clause.contains("provider = $3"));
        assert!(clause.contains("status = $4"));
        assert!(clause.contains("estimated_cost_microcents >= $5"));
        assert!(clause.contains("estimated_cost_microcents < $6"));
        assert_eq!(values.len(), 6);
    }

    #[test]
    fn test_build_filter_clause_with_token_bounds() {
        let db = sea_orm::DatabaseConnection::Disconnected;
        let service = UsageService::new(Arc::new(db));
        let from = Utc::now() - chrono::Duration::hours(1);
        let to = Utc::now();
        let filter = UsageFilter {
            tokens_gte: Some(500),
            tokens_lt: Some(10_000),
            ..Default::default()
        };

        let (clause, values) = service.build_filter_clause(from, to, &filter);
        assert!(clause.contains("(input_tokens + output_tokens) >= $3"));
        assert!(clause.contains("(input_tokens + output_tokens) < $4"));
        assert_eq!(values.len(), 4);
    }

    #[test]
    fn test_usage_log_from_row_with_context() {
        let row = UsageLogRow {
            id: Some(1),
            timestamp: Some("2026-03-10T00:00:00Z".to_string()),
            provider: Some("anthropic".to_string()),
            model: Some("claude-sonnet-4-6".to_string()),
            input_tokens: Some(100),
            output_tokens: Some(50),
            latency_ms: Some(500),
            estimated_cost_microcents: Some(10),
            status: Some(200),
            is_streaming: Some(false),
            is_byok: Some(false),
            conversation_id: Some("conv_123".to_string()),
            tags: Some("agent:support,env:prod".to_string()),
            request_id: Some("req_abc".to_string()),
            trace_id: Some("trace_xyz".to_string()),
        };

        let entry = usage_log_from_row(row);
        assert_eq!(entry.conversation_id, Some("conv_123".to_string()));
        assert_eq!(entry.tags, vec!["agent:support", "env:prod"]);
        assert_eq!(entry.request_id, Some("req_abc".to_string()));
        assert_eq!(entry.trace_id, Some("trace_xyz".to_string()));
    }

    #[test]
    fn test_usage_log_from_row_empty_tags() {
        let row = UsageLogRow {
            id: Some(1),
            timestamp: Some("2026-03-10T00:00:00Z".to_string()),
            provider: Some("openai".to_string()),
            model: Some("gpt-4o".to_string()),
            input_tokens: Some(100),
            output_tokens: Some(50),
            latency_ms: Some(200),
            estimated_cost_microcents: Some(5),
            status: Some(200),
            is_streaming: Some(true),
            is_byok: Some(true),
            conversation_id: None,
            tags: Some("".to_string()),
            request_id: None,
            trace_id: None,
        };

        let entry = usage_log_from_row(row);
        assert!(entry.conversation_id.is_none());
        assert!(entry.tags.is_empty());
        assert!(entry.request_id.is_none());
        assert!(entry.trace_id.is_none());
    }

    #[test]
    fn test_ai_request_context_default() {
        let ctx = AiRequestContext::default();
        assert!(ctx.conversation_id.is_none());
        assert!(ctx.tags.is_empty());
        assert!(ctx.request_id.is_none());
        assert!(ctx.trace_id.is_none());
    }

    #[test]
    fn test_usage_filter_default() {
        let filter = UsageFilter::default();
        assert!(filter.user_id.is_none());
        assert!(filter.conversation_id.is_none());
        assert!(filter.tags.is_none());
        assert!(filter.model.is_none());
        assert!(filter.provider.is_none());
    }

    #[test]
    fn test_usage_log_entry_serialization_omits_none_fields() {
        let entry = UsageLogEntry {
            id: 1,
            timestamp: "2026-03-10T00:00:00Z".to_string(),
            provider: "anthropic".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            input_tokens: 100,
            output_tokens: 50,
            latency_ms: 500,
            estimated_cost_microcents: 10,
            status: 200,
            is_streaming: false,
            is_byok: false,
            conversation_id: None,
            tags: vec![],
            request_id: None,
            trace_id: None,
        };

        let json = serde_json::to_string(&entry).unwrap();
        assert!(!json.contains("conversation_id"));
        assert!(!json.contains("request_id"));
        assert!(!json.contains("trace_id"));
    }

    #[test]
    fn test_usage_log_entry_serialization_includes_context() {
        let entry = UsageLogEntry {
            id: 1,
            timestamp: "2026-03-10T00:00:00Z".to_string(),
            provider: "anthropic".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            input_tokens: 100,
            output_tokens: 50,
            latency_ms: 500,
            estimated_cost_microcents: 10,
            status: 200,
            is_streaming: false,
            is_byok: false,
            conversation_id: Some("conv_abc".to_string()),
            tags: vec!["agent:support".to_string()],
            request_id: Some("req_123".to_string()),
            trace_id: Some("trace_456".to_string()),
        };

        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("conv_abc"));
        assert!(json.contains("agent:support"));
        assert!(json.contains("req_123"));
        assert!(json.contains("trace_456"));
    }

    #[test]
    fn test_conversation_summary_serialization() {
        let summary = ConversationSummary {
            conversation_id: "conv_abc".to_string(),
            message_count: 5,
            total_input_tokens: 1000,
            total_output_tokens: 500,
            total_tokens: 1500,
            total_cost_microcents: 100,
            avg_latency_ms: 450.5,
            models_used: vec!["claude-sonnet-4-6".to_string(), "gpt-4o".to_string()],
            first_at: "2026-03-10T00:00:00Z".to_string(),
            last_at: "2026-03-10T01:00:00Z".to_string(),
        };

        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("conv_abc"));
        assert!(json.contains("\"message_count\":5"));
        assert!(json.contains("claude-sonnet-4-6"));
        assert!(json.contains("gpt-4o"));
    }
}
