// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a chat has cost so far, read from the AI gateway's usage log.
//!
//! Every model request a chat makes through the gateway is logged once, with
//! its token counts and an estimated cost (`ai_usage_logs`), tagged with the
//! conversation's numeric id and a `chat.*` purpose. This module only reads
//! that log: it aggregates per model, bounded by the number of distinct models
//! one chat has used, and never returns request content.
//!
//! The figures are estimates. A request whose model has no published price is
//! logged at zero cost, and is counted in `unpriced_requests` so the total is
//! never presented as complete when it is not.

use sea_orm::{
    ColumnTrait, DatabaseBackend, DatabaseConnection, DbErr, EntityTrait, FromQueryResult,
    QueryFilter, QueryOrder, QuerySelect, Statement,
};
use serde::Serialize;
use temps_entities::{ai_messages, ai_provider_models};
use utoipa::ToSchema;

/// Most distinct models reported for one chat. A chat switches models rarely;
/// the cap only bounds a pathological history.
const MAX_MODELS: i64 = 20;

/// Spend for one model within a chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ModelSpend {
    pub model: String,
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Estimated cost in microcents (1/10000 of a cent), as logged.
    pub cost_microcents: i64,
}

/// Catalogue rows read to find one model's context window. A model is listed
/// once per provider that serves it; the newest row with a window wins.
const WINDOW_LOOKUP_ROWS: u64 = 8;

/// How much of the model's context the chat fills right now: the prompt size
/// of its latest request, which is the whole conversation, the system framing
/// and the tool definitions as the model last saw them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ConversationContext {
    /// Model of that request.
    pub model: String,
    /// Prompt tokens of the latest request, as the provider counted them.
    pub tokens: i64,
    /// The model's context window in tokens, when its catalogue lists one.
    pub window: Option<i64>,
}

/// What one conversation has cost so far.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ConversationUsage {
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Estimated cost in microcents (1/10000 of a cent), summed over the
    /// requests that have a known price.
    pub cost_microcents: i64,
    /// Requests that used tokens but were logged at zero cost because the
    /// model has no known price. When this is not zero, the cost is a floor.
    pub unpriced_requests: i64,
    /// Requests made with the user's own provider key rather than the
    /// platform's, which Temps does not bill.
    pub byok_requests: i64,
    pub models: Vec<ModelSpend>,
    /// The chat's current context size. `None` before the first request, or
    /// when it could not be read: the spend above does not depend on it.
    pub context: Option<ConversationContext>,
    /// Earlier messages folded into the chat's running summary, so the model
    /// no longer sees them word for word. `0` when the chat was never compacted
    /// or the count could not be read.
    pub summarized_messages: i64,
}

#[derive(Debug, FromQueryResult)]
struct LatestRequestRow {
    model: String,
    input_tokens: i64,
}

#[derive(Debug, FromQueryResult)]
struct ModelRow {
    model: String,
    requests: i64,
    input_tokens: i64,
    output_tokens: i64,
    cost_microcents: i64,
    unpriced_requests: i64,
    byok_requests: i64,
}

/// Aggregate the usage of conversation `conversation_id` for `user_id`.
///
/// Rows are matched on the owner as well as the conversation, and only those
/// logged by a chat turn (a `chat.*` purpose tag), so neither another user's
/// requests nor the owner's own gateway calls that happen to reuse the same
/// conversation string are counted.
pub async fn conversation_usage(
    db: &DatabaseConnection,
    conversation_id: i64,
    user_id: i32,
) -> Result<ConversationUsage, DbErr> {
    let rows = ModelRow::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"SELECT
               model,
               COUNT(*)::INT8 AS requests,
               COALESCE(SUM(input_tokens), 0)::INT8 AS input_tokens,
               COALESCE(SUM(output_tokens), 0)::INT8 AS output_tokens,
               COALESCE(SUM(estimated_cost_microcents), 0)::INT8 AS cost_microcents,
               COUNT(*) FILTER (
                   WHERE estimated_cost_microcents = 0
                     AND input_tokens + output_tokens > 0
                     AND NOT is_byok
               )::INT8 AS unpriced_requests,
               COUNT(*) FILTER (WHERE is_byok)::INT8 AS byok_requests
           FROM ai_usage_logs
           WHERE conversation_id = $1
             AND user_id = $2
             AND EXISTS (SELECT 1 FROM unnest(tags) AS tag WHERE tag LIKE 'chat.%')
           GROUP BY model
           ORDER BY cost_microcents DESC, model ASC
           LIMIT $3"#,
        [
            conversation_id.to_string().into(),
            user_id.into(),
            MAX_MODELS.into(),
        ],
    ))
    .all(db)
    .await?;

    let mut usage = summarize(rows);
    // The context readout is a convenience beside the spend; a failure to read
    // it is logged and leaves the spend intact.
    usage.context = match current_context(db, conversation_id, user_id).await {
        Ok(context) => context,
        Err(error) => {
            tracing::warn!(
                conversation_id,
                "could not read the chat's context size: {error}"
            );
            None
        }
    };
    usage.summarized_messages = match summarized_messages(db, conversation_id).await {
        Ok(count) => count,
        Err(error) => {
            tracing::warn!(
                conversation_id,
                "could not read how many messages the chat has summarized: {error}"
            );
            0
        }
    };
    Ok(usage)
}

/// The prompt size of the chat's latest model request, with its model's window.
/// Only the main tool loop counts (`chat.<kind>.tools[.final]`): a title or a
/// permission check is a small side request that says nothing about the
/// conversation's size.
async fn current_context(
    db: &DatabaseConnection,
    conversation_id: i64,
    user_id: i32,
) -> Result<Option<ConversationContext>, DbErr> {
    let latest = LatestRequestRow::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"SELECT model, input_tokens
           FROM ai_usage_logs
           WHERE conversation_id = $1
             AND user_id = $2
             AND input_tokens > 0
             AND EXISTS (SELECT 1 FROM unnest(tags) AS tag WHERE tag LIKE 'chat.%.tools%')
           ORDER BY timestamp DESC, id DESC
           LIMIT 1"#,
        [conversation_id.to_string().into(), user_id.into()],
    ))
    .one(db)
    .await?;
    let Some(latest) = latest else {
        return Ok(None);
    };
    let window = context_window(db, &latest.model).await?;
    Ok(Some(ConversationContext {
        model: latest.model,
        tokens: latest.input_tokens,
        window,
    }))
}

/// The context window, in tokens, the catalogue lists for `model`.
///
/// A model is listed once per provider that serves it; the newest row that
/// carries a window wins. `None` when no row says.
pub(crate) async fn context_window(
    db: &DatabaseConnection,
    model: &str,
) -> Result<Option<i64>, DbErr> {
    let rows = ai_provider_models::Entity::find()
        .filter(ai_provider_models::Column::ModelId.eq(model))
        .filter(ai_provider_models::Column::Metadata.is_not_null())
        .order_by_desc(ai_provider_models::Column::UpdatedAt)
        .limit(WINDOW_LOOKUP_ROWS)
        .all(db)
        .await?;
    Ok(rows
        .iter()
        .filter_map(|row| row.metadata.as_ref())
        .find_map(window_from_metadata))
}

/// Messages folded into the chat's running summary so far, `0` when it has
/// never been compacted.
async fn summarized_messages(db: &DatabaseConnection, conversation_id: i64) -> Result<i64, DbErr> {
    let row = ai_messages::Entity::find()
        .filter(ai_messages::Column::ConversationId.eq(conversation_id))
        .filter(ai_messages::Column::Role.eq(crate::compaction::SUMMARY_ROLE))
        .order_by_desc(ai_messages::Column::Id)
        .one(db)
        .await?;
    Ok(row
        .and_then(|row| row.metadata)
        .and_then(|metadata| {
            metadata
                .get("folded_total")
                .and_then(serde_json::Value::as_i64)
        })
        .unwrap_or(0)
        .max(0))
}

/// A catalogue entry's `context_length`, when it is a positive whole number.
fn window_from_metadata(metadata: &serde_json::Value) -> Option<i64> {
    metadata
        .get("context_length")
        .and_then(serde_json::Value::as_i64)
        .filter(|window| *window > 0)
}

fn summarize(rows: Vec<ModelRow>) -> ConversationUsage {
    let mut usage = ConversationUsage {
        requests: 0,
        input_tokens: 0,
        output_tokens: 0,
        cost_microcents: 0,
        unpriced_requests: 0,
        byok_requests: 0,
        models: Vec::with_capacity(rows.len()),
        context: None,
        summarized_messages: 0,
    };
    for row in rows {
        usage.requests = usage.requests.saturating_add(row.requests);
        usage.input_tokens = usage.input_tokens.saturating_add(row.input_tokens);
        usage.output_tokens = usage.output_tokens.saturating_add(row.output_tokens);
        usage.cost_microcents = usage.cost_microcents.saturating_add(row.cost_microcents);
        usage.unpriced_requests = usage
            .unpriced_requests
            .saturating_add(row.unpriced_requests);
        usage.byok_requests = usage.byok_requests.saturating_add(row.byok_requests);
        usage.models.push(ModelSpend {
            model: row.model,
            requests: row.requests,
            input_tokens: row.input_tokens,
            output_tokens: row.output_tokens,
            cost_microcents: row.cost_microcents,
        });
    }
    usage
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::collections::BTreeMap;

    fn row(
        model: &str,
        requests: i64,
        cost: i64,
        unpriced: i64,
        byok: i64,
    ) -> BTreeMap<String, sea_orm::Value> {
        BTreeMap::from([
            ("model".to_string(), model.into()),
            ("requests".to_string(), requests.into()),
            ("input_tokens".to_string(), (requests * 100).into()),
            ("output_tokens".to_string(), (requests * 10).into()),
            ("cost_microcents".to_string(), cost.into()),
            ("unpriced_requests".to_string(), unpriced.into()),
            ("byok_requests".to_string(), byok.into()),
        ])
    }

    #[tokio::test]
    async fn totals_add_up_across_models_and_keep_the_per_model_split() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                row("vendor/large", 3, 9_000, 0, 0),
                row("vendor/small", 5, 500, 1, 2),
            ]])
            .into_connection();

        let usage = conversation_usage(&db, 42, 7).await.expect("usage");

        assert_eq!(usage.requests, 8);
        assert_eq!(usage.input_tokens, 800);
        assert_eq!(usage.output_tokens, 80);
        assert_eq!(usage.cost_microcents, 9_500);
        assert_eq!(usage.unpriced_requests, 1);
        assert_eq!(usage.byok_requests, 2);
        assert_eq!(usage.models.len(), 2);
        assert_eq!(usage.models[0].model, "vendor/large");
        assert_eq!(usage.models[0].cost_microcents, 9_000);
    }

    #[tokio::test]
    async fn a_chat_with_no_logged_requests_costs_nothing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<BTreeMap<String, sea_orm::Value>>::new()])
            .into_connection();

        let usage = conversation_usage(&db, 42, 7).await.expect("usage");

        assert_eq!(usage.requests, 0);
        assert_eq!(usage.cost_microcents, 0);
        assert!(usage.models.is_empty());
    }

    #[tokio::test]
    async fn the_query_is_scoped_to_the_conversation_and_its_owner() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<BTreeMap<String, sea_orm::Value>>::new()])
            .into_connection();

        conversation_usage(&db, 42, 7).await.expect("usage");

        let log = db.into_transaction_log();
        let sql = format!("{:?}", log);
        assert!(sql.contains("conversation_id = $1"));
        assert!(sql.contains("user_id = $2"));
        assert!(sql.contains("chat.%"));
        assert!(
            sql.contains("\"42\""),
            "the conversation id is matched as its decimal string"
        );
        assert!(
            sql.contains("Int(Some(7))"),
            "the owner id is bound, not interpolated"
        );
    }

    #[tokio::test]
    async fn a_database_failure_is_returned_not_swallowed() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([DbErr::Custom("connection reset".to_string())])
            .into_connection();

        let error = conversation_usage(&db, 42, 7).await.expect_err("must fail");
        assert!(error.to_string().contains("connection reset"));
    }

    fn latest_row(model: &str, input_tokens: i64) -> BTreeMap<String, sea_orm::Value> {
        BTreeMap::from([
            ("model".to_string(), model.into()),
            ("input_tokens".to_string(), input_tokens.into()),
        ])
    }

    fn catalogue_row(model: &str, metadata: serde_json::Value) -> ai_provider_models::Model {
        let now = chrono::Utc::now();
        ai_provider_models::Model {
            id: 1,
            provider_key_id: 1,
            model_id: model.to_string(),
            display_name: model.to_string(),
            source: "discovered".to_string(),
            is_available: true,
            is_enabled: true,
            owned_by: None,
            metadata: Some(metadata),
            last_seen_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn the_context_is_the_latest_request_with_its_models_window() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row("vendor/large", 3, 9_000, 0, 0)]])
            .append_query_results([vec![latest_row("vendor/large", 38_500)]])
            .append_query_results([vec![catalogue_row(
                "vendor/large",
                serde_json::json!({ "context_length": 200_000 }),
            )]])
            .into_connection();

        let usage = conversation_usage(&db, 42, 7).await.expect("usage");

        assert_eq!(
            usage.context,
            Some(ConversationContext {
                model: "vendor/large".to_string(),
                tokens: 38_500,
                window: Some(200_000),
            })
        );
        assert_eq!(usage.cost_microcents, 9_000);
    }

    #[tokio::test]
    async fn a_model_without_a_listed_window_still_reports_its_tokens() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row("vendor/large", 1, 100, 0, 0)]])
            .append_query_results([vec![latest_row("vendor/large", 1_200)]])
            .append_query_results([vec![catalogue_row("vendor/large", serde_json::json!({}))]])
            .into_connection();

        let usage = conversation_usage(&db, 42, 7).await.expect("usage");

        let context = usage.context.expect("context");
        assert_eq!(context.tokens, 1_200);
        assert_eq!(context.window, None);
    }

    #[tokio::test]
    async fn a_chat_with_no_tool_loop_request_has_no_context_yet() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row("vendor/large", 1, 100, 0, 0)]])
            .append_query_results([Vec::<BTreeMap<String, sea_orm::Value>>::new()])
            .into_connection();

        let usage = conversation_usage(&db, 42, 7).await.expect("usage");

        assert_eq!(usage.context, None);
        assert_eq!(usage.requests, 1);
    }

    #[tokio::test]
    async fn the_context_query_only_counts_the_main_loop_for_this_owner() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<BTreeMap<String, sea_orm::Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, sea_orm::Value>>::new()])
            .into_connection();

        conversation_usage(&db, 42, 7).await.expect("usage");

        let sql = format!("{:?}", db.into_transaction_log());
        assert!(sql.contains("chat.%.tools%"), "side requests are excluded");
        assert!(sql.contains("ORDER BY timestamp DESC"));
    }

    #[tokio::test]
    async fn a_failed_context_read_leaves_the_spend_intact() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row("vendor/large", 2, 500, 0, 0)]])
            .append_query_errors([DbErr::Custom("connection reset".to_string())])
            .into_connection();

        let usage = conversation_usage(&db, 42, 7).await.expect("usage");

        assert_eq!(usage.context, None);
        assert_eq!(usage.cost_microcents, 500);
    }

    #[test]
    fn a_window_is_a_positive_whole_number_in_the_catalogue_entry() {
        use serde_json::json;
        assert_eq!(
            window_from_metadata(&json!({ "context_length": 1_000_000 })),
            Some(1_000_000)
        );
        assert_eq!(window_from_metadata(&json!({ "context_length": 0 })), None);
        assert_eq!(window_from_metadata(&json!({ "context_length": -5 })), None);
        assert_eq!(
            window_from_metadata(&json!({ "context_length": "200000" })),
            None
        );
        assert_eq!(window_from_metadata(&json!({})), None);
    }
}
