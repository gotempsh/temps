// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

fn priced(input: Option<f64>, output: Option<f64>) -> ModelDetails {
    ModelDetails {
        input_price_per_million: input,
        output_price_per_million: output,
        ..ModelDetails::default()
    }
}

#[test]
fn cost_is_tokens_times_the_per_million_price_in_microcents() {
    // $3 / $15 per million tokens: 1,000 in + 200 out = $0.003 + $0.003.
    let details = priced(Some(3.0), Some(15.0));
    assert_eq!(
        cost_microcents_from_details(&details, 1_000, 200, PromptTokensDetails::default()),
        Some(6_000)
    );
    // 6,000 microcents is $0.006.
    assert_eq!(6_000.0 / 1_000_000.0, 0.006);
}

fn cached(read: i64, write: i64) -> PromptTokensDetails {
    PromptTokensDetails {
        cached_tokens: read,
        cache_write_tokens: write,
    }
}

#[test]
fn cached_input_is_priced_at_the_cache_rate_and_carved_out_of_the_prompt() {
    // $3 in / $15 out, $0.30 cache read, $3.75 cache write per million.
    let details = ModelDetails {
        cache_read_price_per_million: Some(0.3),
        cache_write_price_per_million: Some(3.75),
        ..priced(Some(3.0), Some(15.0))
    };
    // 10,000-token prompt: 8,000 cached, 1,000 written, 1,000 fresh; 200 out.
    // 1,000*3 + 8,000*0.3 + 1,000*3.75 + 200*15 = 3,000+2,400+3,750+3,000.
    assert_eq!(
        cost_microcents_from_details(&details, 10_000, 200, cached(8_000, 1_000)),
        Some(12_150)
    );
    // Uncached, the same prompt costs 10,000*3 + 3,000.
    assert_eq!(
        cost_microcents_from_details(&details, 10_000, 200, cached(0, 0)),
        Some(33_000)
    );
}

#[test]
fn a_catalogue_without_cache_prices_charges_cached_tokens_as_input() {
    let details = priced(Some(3.0), Some(15.0));
    assert_eq!(
        cost_microcents_from_details(&details, 10_000, 0, cached(8_000, 1_000)),
        cost_microcents_from_details(&details, 10_000, 0, cached(0, 0)),
    );
}

#[test]
fn cache_counts_larger_than_the_prompt_cannot_make_the_cost_negative_or_exceed_it() {
    let details = ModelDetails {
        cache_read_price_per_million: Some(0.3),
        cache_write_price_per_million: Some(3.75),
        ..priced(Some(3.0), Some(15.0))
    };
    // Claims 9,000 read + 9,000 written out of a 1,000-token prompt: the
    // read share is capped at the prompt, nothing is left to write.
    assert_eq!(
        cost_microcents_from_details(&details, 1_000, 0, cached(9_000, 9_000)),
        Some(300)
    );
    assert_eq!(
        cost_microcents_from_details(&details, 1_000, 0, cached(-5, -5)),
        Some(3_000)
    );
}

#[test]
fn a_model_without_a_published_price_is_unpriced_not_free() {
    assert_eq!(
        cost_microcents_from_details(
            &priced(None, None),
            1_000,
            200,
            PromptTokensDetails::default()
        ),
        None
    );
    assert_eq!(
        cost_microcents_from_details(
            &priced(Some(3.0), None),
            1_000,
            200,
            PromptTokensDetails::default()
        ),
        None
    );
}

#[test]
fn a_free_model_is_priced_at_zero() {
    assert_eq!(
        cost_microcents_from_details(
            &priced(Some(0.0), Some(0.0)),
            5_000,
            500,
            PromptTokensDetails::default()
        ),
        Some(0)
    );
}

#[test]
fn negative_tokens_and_non_finite_prices_cannot_produce_a_negative_or_wild_cost() {
    let details = priced(Some(3.0), Some(15.0));
    assert_eq!(
        cost_microcents_from_details(&details, -50, -50, PromptTokensDetails::default()),
        Some(0)
    );
    assert_eq!(
        cost_microcents_from_details(
            &priced(Some(f64::NAN), Some(1.0)),
            10,
            10,
            PromptTokensDetails::default()
        ),
        None
    );
    assert_eq!(
        cost_microcents_from_details(
            &priced(Some(f64::MAX), Some(f64::MAX)),
            i64::MAX,
            1,
            PromptTokensDetails::default()
        ),
        Some(i64::MAX)
    );
}

fn catalogue_row(metadata: Option<serde_json::Value>) -> ai_provider_models::Model {
    let now = chrono::Utc::now();
    ai_provider_models::Model {
        id: 1,
        provider_key_id: 1,
        model_id: "vendor/model".to_string(),
        display_name: "Model".to_string(),
        source: "discovered".to_string(),
        is_available: true,
        is_enabled: true,
        owned_by: None,
        metadata,
        last_seen_at: None,
        created_at: now,
        updated_at: now,
    }
}

fn service() -> UsageService {
    // The lookup runs on the connection it is handed, so the service's own
    // handle is not used by these tests.
    UsageService::new(Arc::new(
        sea_orm::MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
    ))
}

#[tokio::test]
async fn a_logged_request_is_priced_from_the_newest_catalogue_entry_that_has_a_price() {
    let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![
            // Newest entry publishes nothing; the next one does.
            catalogue_row(Some(serde_json::json!({ "name": "Model" }))),
            catalogue_row(Some(serde_json::json!({
                "input_price_per_million": 3.0,
                "output_price_per_million": 15.0
            }))),
        ]])
        .into_connection();

    let cost = service()
        .estimate_cost_microcents(
            &db,
            "openrouter",
            "vendor/model",
            1_000,
            200,
            PromptTokensDetails::default(),
        )
        .await;

    assert_eq!(cost, Some(6_000));
}

#[tokio::test]
async fn an_unlisted_model_has_no_catalogue_price() {
    let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<ai_provider_models::Model>::new()])
        .into_connection();

    let cost = service()
        .estimate_cost_microcents(
            &db,
            "openrouter",
            "vendor/unknown",
            1_000,
            200,
            PromptTokensDetails::default(),
        )
        .await;

    assert_eq!(cost, None);
}

#[test]
fn an_unlisted_self_hosted_model_is_recorded_as_unpriced() {
    assert_eq!(
        unlisted_cost_microcents(
            crate::self_hosted::OPENAI_COMPATIBLE,
            "local-flash",
            7_000_000,
            100_000,
            PromptTokensDetails::default(),
            UsageCount::OneRequest,
        ),
        None
    );
}

#[test]
fn an_unlisted_vendor_model_keeps_the_worst_case_price() {
    let none = PromptTokensDetails::default();
    let one = UsageCount::OneRequest;
    assert_eq!(
        unlisted_cost_microcents("openrouter", "vendor/unknown", 1_000, 200, none, one),
        Some(crate::services::pricing::estimate_cost_microcents(
            "vendor/unknown",
            1_000,
            200,
            none
        ))
    );
    assert!(
        unlisted_cost_microcents("openrouter", "vendor/unknown", 1_000, 200, none, one) > Some(0)
    );
}

#[test]
fn a_vendor_model_without_a_catalogue_price_bills_its_cache_writes() {
    // Claude Haiku 5.5 lists $0.10 input and $0.20 for a 1-hour cache
    // write per 1M tokens: 50,000 written tokens are 10,000 microcents,
    // not the 5,000 they cost as ordinary input.
    let cache = PromptTokensDetails {
        cached_tokens: 0,
        cache_write_tokens: 50_000,
    };
    assert_eq!(
        unlisted_cost_microcents(
            "anthropic",
            "claude-haiku-5-5",
            50_000,
            0,
            cache,
            UsageCount::OneRequest
        ),
        Some(10_000)
    );
}

#[tokio::test]
async fn a_failed_price_lookup_is_treated_as_no_catalogue_price() {
    let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_errors([sea_orm::DbErr::Custom("connection reset".to_string())])
        .into_connection();

    let cost = service()
        .estimate_cost_microcents(
            &db,
            "openrouter",
            "vendor/model",
            1_000,
            200,
            PromptTokensDetails::default(),
        )
        .await;

    assert_eq!(cost, None);
}

#[tokio::test]
async fn the_catalogue_lookup_reads_only_the_serving_providers_entries() {
    let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<ai_provider_models::Model>::new()])
        .into_connection();
    service()
        .estimate_cost_microcents(
            &db,
            crate::self_hosted::OPENAI_COMPATIBLE,
            "gpt-5.4",
            1_000,
            0,
            PromptTokensDetails::default(),
        )
        .await;
    let sql = format!("{:?}", db.into_transaction_log());
    // Debug output escapes the quotes in the SQL text, not in bound values.
    assert!(sql.contains(r#"INNER JOIN \"ai_provider_keys\""#), "{sql}");
    assert!(
        sql.contains(r#"\"ai_provider_keys\".\"provider\" = $1"#),
        "{sql}"
    );
    assert!(
        sql.contains(r#"String(Some("openai_compatible"))"#),
        "{sql}"
    );
}

#[tokio::test]
async fn a_self_hosted_model_does_not_take_a_vendor_keys_catalogue_price_on_a_real_database() {
    use sea_orm::ConnectionTrait;
    // Skip only when no container runtime can be reached; a failed
    // migration or schema check fails the test.
    let test_db = match temps_test_utils::TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(error) if temps_test_utils::is_container_runtime_unavailable(&format!("{error:#}")) => {
            println!("Skipping: no container runtime for the PostgreSQL test database: {error:#}");
            return;
        }
        Err(error) => panic!("test database setup failed: {error:#}"),
    };
    let db = test_db.connection_arc();
    // A vendor key that prices `gpt-5.4`, and a self-hosted key whose server
    // serves a model it also calls `gpt-5.4`, unpriced, plus one it prices.
    db.execute_unprepared(
        "INSERT INTO ai_provider_keys (id, provider, display_name, api_key_encrypted) VALUES \
         (901, 'openai', 'Vendor', 'x'), (902, 'openai_compatible', 'Local', 'x')",
    )
    .await
    .expect("keys inserted");
    db.execute_unprepared(
        "INSERT INTO ai_provider_models (provider_key_id, model_id, display_name, source, metadata) VALUES \
         (901, 'gpt-5.4', 'GPT-5.4', 'discovered', \
          '{\"input_price_per_million\": 2.5, \"output_price_per_million\": 15.0}'), \
         (902, 'gpt-5.4', 'Local GPT', 'discovered', '{\"name\": \"local\"}'), \
         (902, 'local-priced', 'Local priced', 'discovered', \
          '{\"input_price_per_million\": 1.0, \"output_price_per_million\": 1.0}')",
    )
    .await
    .expect("catalogue inserted");
    let service = UsageService::new(db.clone());
    let none = PromptTokensDetails::default();
    let self_hosted = crate::self_hosted::OPENAI_COMPATIBLE;

    assert_eq!(
        service
            .estimate_cost_microcents(db.as_ref(), self_hosted, "gpt-5.4", 1_000, 0, none)
            .await,
        None,
        "a self-hosted model must not take the vendor key's price"
    );
    assert_eq!(
        service
            .estimate_cost_microcents(db.as_ref(), "openai", "gpt-5.4", 1_000, 0, none)
            .await,
        Some(2_500)
    );
    assert_eq!(
        service
            .estimate_cost_microcents(db.as_ref(), self_hosted, "local-priced", 1_000, 0, none)
            .await,
        Some(1_000),
        "a self-hosted key's own priced entry still applies"
    );
}

#[tokio::test]
async fn a_request_with_no_tokens_skips_the_lookup() {
    // No query results queued: a lookup would error and return no price,
    // so `Some(0)` proves it returned early.
    let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let cost = service()
        .estimate_cost_microcents(
            &db,
            "openrouter",
            "vendor/model",
            0,
            0,
            PromptTokensDetails::default(),
        )
        .await;
    assert_eq!(cost, Some(0));
}

#[tokio::test]
async fn by_project_reports_operator_cost_apart_from_total_cost() {
    use sea_orm::{MockDatabase, Value};
    let row = std::collections::BTreeMap::from([
        ("project_id".to_string(), Value::Int(Some(7))),
        ("request_count".to_string(), Value::BigInt(Some(4))),
        ("input_tokens".to_string(), Value::BigInt(Some(100))),
        ("output_tokens".to_string(), Value::BigInt(Some(50))),
        (
            "estimated_cost_microcents".to_string(),
            Value::BigInt(Some(900)),
        ),
        (
            "operator_cost_microcents".to_string(),
            Value::BigInt(Some(600)),
        ),
        ("byok_request_count".to_string(), Value::BigInt(Some(1))),
        ("error_count".to_string(), Value::BigInt(Some(0))),
    ]);
    let service = UsageService::new(Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row]])
            .into_connection(),
    ));
    let now = chrono::Utc::now();
    let projects = service.get_by_project(now, now, 10).await.unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].project_id, 7);
    assert_eq!(projects[0].estimated_cost_microcents, 900);
    assert_eq!(projects[0].operator_cost_microcents, 600);
}

#[tokio::test]
async fn the_summary_counts_requests_that_have_no_recorded_cost() {
    use sea_orm::{MockDatabase, Transaction, Value};
    let row = std::collections::BTreeMap::from([
        ("total_requests".to_string(), Value::BigInt(Some(5))),
        ("total_input_tokens".to_string(), Value::BigInt(Some(500))),
        ("total_output_tokens".to_string(), Value::BigInt(Some(50))),
        ("total_tokens".to_string(), Value::BigInt(Some(550))),
        ("avg_latency_ms".to_string(), Value::Double(Some(10.0))),
        (
            "total_cost_microcents".to_string(),
            Value::BigInt(Some(700)),
        ),
        ("error_count".to_string(), Value::BigInt(Some(0))),
        ("streaming_count".to_string(), Value::BigInt(Some(0))),
        ("byok_count".to_string(), Value::BigInt(Some(0))),
        ("unpriced_requests".to_string(), Value::BigInt(Some(2))),
        ("priced_requests".to_string(), Value::BigInt(Some(3))),
    ]);
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row]])
            .into_connection(),
    );
    let service = UsageService::new(db.clone());
    let now = chrono::Utc::now();
    let summary = service.get_summary(now, now).await.unwrap();
    assert_eq!(summary.total_cost_microcents, 700);
    assert_eq!(summary.unpriced_requests, 2);
    assert_eq!(summary.priced_requests, 3);

    drop(service);
    let log: Vec<Transaction> = Arc::try_unwrap(db).unwrap().into_transaction_log();
    let sql = format!("{log:?}");
    assert!(
        sql.contains("estimated_cost_microcents <= 0") && sql.contains("price_known IS FALSE"),
        "the summary must count unpriced requests: {sql}"
    );
}

#[tokio::test]
async fn top_models_report_the_recorded_cost_of_their_requests() {
    use sea_orm::{MockDatabase, Transaction, Value};
    // A long-context Claude Haiku 5.5 request recorded at its long-context
    // rate: 200,000 x 0.50 + 1,000 x 2.50 microcents.
    let row = std::collections::BTreeMap::from([
        ("model".to_string(), Value::from("claude-haiku-5-5")),
        ("provider".to_string(), Value::from("anthropic")),
        ("request_count".to_string(), Value::BigInt(Some(1))),
        ("input_tokens".to_string(), Value::BigInt(Some(200_000))),
        ("output_tokens".to_string(), Value::BigInt(Some(1_000))),
        ("total_tokens".to_string(), Value::BigInt(Some(201_000))),
        ("avg_latency_ms".to_string(), Value::Double(Some(12.0))),
        (
            "estimated_cost_microcents".to_string(),
            Value::BigInt(Some(102_500)),
        ),
    ]);
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row]])
            .into_connection(),
    );
    let service = UsageService::new(db.clone());
    let now = chrono::Utc::now();
    let models = service.get_top_models(now, now, 10).await.unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].model, "claude-haiku-5-5");
    assert_eq!(models[0].estimated_cost_microcents, 102_500);

    drop(service);
    let log: Vec<Transaction> = Arc::try_unwrap(db).unwrap().into_transaction_log();
    let sql = format!("{log:?}");
    assert!(
        sql.contains("SUM(estimated_cost_microcents)"),
        "top models must sum the recorded cost: {sql}"
    );
}

#[tokio::test]
async fn a_key_route_resolves_to_the_key_provider() {
    use sea_orm::MockDatabase;
    let key = temps_entities::ai_provider_keys::Model {
        id: 7,
        provider: "anthropic".to_string(),
        display_name: "Anthropic".to_string(),
        api_key_encrypted: "enc".to_string(),
        base_url: None,
        default_model: None,
        is_active: true,
        trusted_private_address: None,
        trusted_private_address_set_by: None,
        trusted_private_address_set_at: None,
        capabilities: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    let service = UsageService::new(Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![key]])
            .into_connection(),
    ));
    assert_eq!(
        service.provider_name_for_route("gateway_key:7").await,
        "anthropic"
    );
}

#[tokio::test]
async fn routes_that_are_not_resolvable_keys_are_left_alone() {
    use sea_orm::MockDatabase;
    // No query results queued: a lookup that wrongly ran would error out.
    let service = UsageService::new(Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
    ));
    assert_eq!(service.provider_name_for_route("openai").await, "openai");
    assert_eq!(
        service.provider_name_for_route("gateway_key:abc").await,
        "gateway_key:abc"
    );
    // A key that no longer exists, or a failing read, falls back to the route.
    assert_eq!(
        service.provider_name_for_route("gateway_key:9").await,
        "gateway_key:9"
    );
}

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
        cache_read_tokens: Some(0),
        cache_write_tokens: Some(0),
        latency_ms: Some(500),
        estimated_cost_microcents: Some(10),
        status: Some(200),
        is_streaming: Some(false),
        is_byok: Some(false),
        conversation_id: Some("conv_123".to_string()),
        tags: Some("agent:support,env:prod".to_string()),
        request_id: Some("req_abc".to_string()),
        trace_id: Some("trace_xyz".to_string()),
        project_id: Some(7),
        token_id: Some(9),
        price_known: None,
    };

    let entry = usage_log_from_row(row);
    assert_eq!(entry.conversation_id, Some("conv_123".to_string()));
    assert_eq!(entry.tags, vec!["agent:support", "env:prod"]);
    assert_eq!(entry.request_id, Some("req_abc".to_string()));
    assert_eq!(entry.trace_id, Some("trace_xyz".to_string()));
    assert_eq!((entry.cache_read_tokens, entry.cache_write_tokens), (0, 0));
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
        cache_read_tokens: Some(0),
        cache_write_tokens: Some(0),
        latency_ms: Some(200),
        estimated_cost_microcents: Some(5),
        status: Some(200),
        is_streaming: Some(true),
        is_byok: Some(true),
        conversation_id: None,
        tags: Some("".to_string()),
        request_id: None,
        trace_id: None,
        project_id: None,
        token_id: None,
        price_known: None,
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
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        latency_ms: 500,
        estimated_cost_microcents: 10,
        status: 200,
        is_streaming: false,
        is_byok: false,
        conversation_id: None,
        tags: vec![],
        request_id: None,
        trace_id: None,
        project_id: None,
        token_id: None,
        unpriced: false,
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
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        latency_ms: 500,
        estimated_cost_microcents: 10,
        status: 200,
        is_streaming: false,
        is_byok: false,
        conversation_id: Some("conv_abc".to_string()),
        tags: vec!["agent:support".to_string()],
        request_id: Some("req_123".to_string()),
        trace_id: Some("trace_456".to_string()),
        project_id: None,
        token_id: None,
        unpriced: false,
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

fn usage_log_model(
    project_id: Option<i32>,
    token_id: Option<i32>,
) -> temps_entities::ai_usage_logs::Model {
    temps_entities::ai_usage_logs::Model {
        id: 1,
        timestamp: Utc::now(),
        user_id: None,
        provider: "openai".into(),
        model: "gpt-4o".into(),
        input_tokens: 1_000,
        output_tokens: 100,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        latency_ms: 10,
        estimated_cost_microcents: 3_500,
        status: 200,
        is_streaming: false,
        is_byok: false,
        conversation_id: None,
        tags: vec![],
        request_id: None,
        trace_id: None,
        project_id,
        token_id,
        price_known: None,
    }
}

#[tokio::test]
async fn deployment_token_usage_is_stored_with_project_and_token() {
    use sea_orm::MockDatabase;
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            // No catalogue price, so the static price table applies.
            .append_query_results([Vec::<ai_provider_models::Model>::new()])
            .append_query_results([vec![usage_log_model(Some(7), Some(11))]])
            .into_connection(),
    );
    let service = UsageService::new(db.clone());
    let context = AiRequestContext {
        project_id: Some(7),
        token_id: Some(11),
        ..Default::default()
    };

    service
        .log_usage_with_context(
            None,
            "openai",
            "gpt-4o",
            1_000,
            100,
            10,
            0,
            200,
            false,
            false,
            PromptTokensDetails::default(),
            &context,
        )
        .await
        .expect("usage insert succeeds");

    drop(service);
    let log = Arc::try_unwrap(db).unwrap().into_transaction_log();
    let sql = format!("{log:?}");
    assert!(
        sql.contains("project_id") && sql.contains("token_id"),
        "{sql}"
    );
    assert!(sql.contains("Int(Some(7))"), "project id not bound: {sql}");
    assert!(sql.contains("Int(Some(11))"), "token id not bound: {sql}");
    // gpt-4o is $2.50/M in, $10/M out: 1000 * 2.5 + 100 * 10 microcents.
    assert!(
        sql.contains("BigInt(Some(3500))"),
        "cost was not estimated: {sql}"
    );
}

#[tokio::test]
async fn human_caller_usage_is_stored_without_project_attribution() {
    use sea_orm::MockDatabase;
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<ai_provider_models::Model>::new()])
            .append_query_results([vec![usage_log_model(None, None)]])
            .into_connection(),
    );
    let service = UsageService::new(db.clone());

    service
        .log_usage(
            Some(5),
            "openai",
            "gpt-4o",
            10,
            10,
            10,
            0,
            200,
            false,
            false,
        )
        .await
        .expect("usage insert succeeds");

    drop(service);
    let log = Arc::try_unwrap(db).unwrap().into_transaction_log();
    let sql = format!("{log:?}");
    assert!(
        sql.contains("Int(None)"),
        "project/token must bind NULL: {sql}"
    );
    assert!(!sql.contains("Int(Some(7))"), "{sql}");
}

#[test]
fn a_catalogue_cost_for_a_long_prompt_is_raised_to_the_listed_long_context_cost() {
    let none = PromptTokensDetails::default();
    // gpt-5.4 at the catalogue's base $2.50 per 1M: 300,000 tokens are
    // 750,000 microcents, but the price list bills them at $5 above
    // 272,000 input tokens.
    assert_eq!(
        catalogue_cost_microcents(
            "openai",
            "gpt-5.4",
            750_000,
            300_000,
            0,
            none,
            UsageCount::OneRequest
        ),
        1_500_000
    );
    // Below the threshold, or for a model with no listed long tier, the
    // catalogue's figure stands, even when it is lower than the list's.
    assert_eq!(
        catalogue_cost_microcents(
            "openai",
            "gpt-5.4",
            100_000,
            200_000,
            0,
            none,
            UsageCount::OneRequest
        ),
        100_000
    );
    assert_eq!(
        catalogue_cost_microcents(
            "openai",
            "vendor/model",
            600_000,
            300_000,
            0,
            none,
            UsageCount::OneRequest
        ),
        600_000
    );
    // A catalogue figure above the listed long-context cost is kept.
    assert_eq!(
        catalogue_cost_microcents(
            "openai",
            "gpt-5.4",
            9_000_000,
            300_000,
            0,
            none,
            UsageCount::OneRequest
        ),
        9_000_000
    );
    // A self-hosted server's ids are not vendor models.
    assert_eq!(
        catalogue_cost_microcents(
            crate::self_hosted::OPENAI_COMPATIBLE,
            "gpt-5.4",
            750_000,
            300_000,
            0,
            none,
            UsageCount::OneRequest
        ),
        750_000
    );
}

#[test]
fn a_batch_total_is_never_priced_at_long_context_rates() {
    let none = PromptTokensDetails::default();
    let batch = UsageCount::BatchTotal;
    // 300 prompts of 1,000 tokens to gpt-5.4: a 300,000-token total, but
    // no prompt crosses 272,000 tokens. Both paths bill the base $2.50.
    assert_eq!(
        catalogue_cost_microcents("openai", "gpt-5.4", 750_000, 300_000, 0, none, batch),
        750_000
    );
    assert_eq!(
        unlisted_cost_microcents("openai", "gpt-5.4", 300_000, 0, none, batch),
        Some(750_000)
    );
    // The same total as one request is a long prompt.
    assert_eq!(
        unlisted_cost_microcents(
            "openai",
            "gpt-5.4",
            300_000,
            0,
            none,
            UsageCount::OneRequest
        ),
        Some(1_500_000)
    );
}

#[test]
fn a_zero_cost_is_unpriced_only_when_no_known_price_gave_it() {
    // A free model: a known price of zero.
    assert!(!is_unpriced(Some(true), 0, 1_000, 10));
    // A self-hosted model no price list covers.
    assert!(is_unpriced(Some(false), 0, 1_000, 10));
    // Rows older than the column: unpriced exactly when not positive.
    assert!(is_unpriced(None, 0, 1_000, 10));
    assert!(!is_unpriced(None, 5, 1_000, 10));
    // A request that used no tokens cost nothing, priced or not.
    assert!(!is_unpriced(Some(false), 0, 0, 0));
    assert!(!is_unpriced(None, 0, 0, 0));
}

#[test]
fn a_logged_free_request_is_not_shown_as_unpriced() {
    let row = |price_known| UsageLogRow {
        id: Some(1),
        timestamp: None,
        provider: Some("openrouter".to_string()),
        model: Some("vendor/free-model".to_string()),
        input_tokens: Some(1_000),
        output_tokens: Some(10),
        cache_read_tokens: None,
        cache_write_tokens: None,
        latency_ms: None,
        estimated_cost_microcents: Some(0),
        status: Some(200),
        is_streaming: None,
        is_byok: None,
        conversation_id: None,
        tags: None,
        request_id: None,
        trace_id: None,
        project_id: None,
        token_id: None,
        price_known,
    };
    assert!(!usage_log_from_row(row(Some(true))).unpriced);
    assert!(usage_log_from_row(row(Some(false))).unpriced);
    assert!(usage_log_from_row(row(None)).unpriced);
}

/// Log one request through the service and return the insert's SQL.
async fn logged_insert(
    catalogue: Vec<ai_provider_models::Model>,
    provider: &str,
    model: &str,
    input_tokens: i64,
    count: UsageCount,
) -> String {
    use sea_orm::MockDatabase;
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([catalogue])
            .append_query_results([vec![usage_log_model(None, None)]])
            .into_connection(),
    );
    let service = UsageService::new(db.clone());
    service
        .log_usage_on_connection(
            db.as_ref(),
            None,
            provider,
            model,
            input_tokens,
            0,
            0,
            0,
            200,
            false,
            false,
            PromptTokensDetails::default(),
            count,
            &AiRequestContext::default(),
        )
        .await
        .expect("usage insert succeeds");
    drop(service);
    format!("{:?}", Arc::try_unwrap(db).unwrap().into_transaction_log())
}

#[tokio::test]
async fn a_logged_request_records_whether_a_known_price_gave_its_cost() {
    // `is_streaming` and `is_byok` are false, so a bound `true` is
    // `price_known`.
    let free = logged_insert(
        vec![catalogue_row(Some(serde_json::json!({
            "input_price_per_million": 0.0,
            "output_price_per_million": 0.0
        })))],
        "openrouter",
        "vendor/model",
        1_000,
        UsageCount::OneRequest,
    )
    .await;
    assert!(free.contains("price_known"), "{free}");
    assert!(
        free.contains("Bool(Some(true))"),
        "a free model is priced: {free}"
    );
    assert!(free.contains("BigInt(Some(0))"), "{free}");

    let self_hosted = logged_insert(
        Vec::new(),
        crate::self_hosted::OPENAI_COMPATIBLE,
        "local-flash",
        1_000,
        UsageCount::OneRequest,
    )
    .await;
    assert!(
        !self_hosted.contains("Bool(Some(true))"),
        "a self-hosted model has no known price: {self_hosted}"
    );
}

#[tokio::test]
async fn a_logged_batch_total_is_priced_at_the_base_rate() {
    let catalogue = || {
        vec![catalogue_row(Some(serde_json::json!({
            "input_price_per_million": 2.5,
            "output_price_per_million": 15.0
        })))]
    };
    let batch = logged_insert(
        catalogue(),
        "openai",
        "gpt-5.4",
        300_000,
        UsageCount::BatchTotal,
    )
    .await;
    assert!(batch.contains("BigInt(Some(750000))"), "{batch}");
    assert!(!batch.contains("BigInt(Some(1500000))"), "{batch}");
    let one = logged_insert(
        catalogue(),
        "openai",
        "gpt-5.4",
        300_000,
        UsageCount::OneRequest,
    )
    .await;
    assert!(one.contains("BigInt(Some(1500000))"), "{one}");
}

#[tokio::test]
async fn the_summary_tells_free_requests_from_unpriced_ones_on_a_real_database() {
    use sea_orm::ConnectionTrait;
    // Skip only when no container runtime can be reached; a failed
    // migration or schema check fails the test.
    let test_db = match temps_test_utils::TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(error) if temps_test_utils::is_container_runtime_unavailable(&format!("{error:#}")) => {
            println!("Skipping: no container runtime for the PostgreSQL test database: {error:#}");
            return;
        }
        Err(error) => panic!("test database setup failed: {error:#}"),
    };
    let db = test_db.connection_arc();
    // (cost, price_known, input tokens): a free model, a self-hosted
    // model, a row older than the column with no cost and one with a
    // cost, and a failure that used no tokens.
    for (cost, known, tokens) in [
        ("0", "true", 1_000),
        ("0", "false", 1_000),
        ("0", "NULL", 1_000),
        ("500", "NULL", 1_000),
        ("0", "NULL", 0),
    ] {
        db.execute_unprepared(&format!(
            "INSERT INTO ai_usage_logs \
             (provider, model, input_tokens, output_tokens, estimated_cost_microcents, price_known) \
             VALUES ('openai', 'gpt-4o', {tokens}, 0, {cost}, {known})"
        ))
        .await
        .expect("usage row inserted");
    }
    let service = UsageService::new(db);
    let now = chrono::Utc::now();
    let summary = service
        .get_summary(
            now - chrono::Duration::hours(1),
            now + chrono::Duration::hours(1),
        )
        .await
        .expect("summary");

    assert_eq!(summary.total_requests, 5);
    assert_eq!(summary.total_cost_microcents, 500);
    // The self-hosted request and the old zero-cost row.
    assert_eq!(summary.unpriced_requests, 2);
    // The free request and the old row with a cost.
    assert_eq!(summary.priced_requests, 2);
}

#[tokio::test]
async fn a_long_prompt_priced_from_the_catalogue_records_the_long_context_cost() {
    use sea_orm::MockDatabase;
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![catalogue_row(Some(serde_json::json!({
                "input_price_per_million": 2.5,
                "output_price_per_million": 15.0
            })))]])
            .append_query_results([vec![usage_log_model(None, None)]])
            .into_connection(),
    );
    let service = UsageService::new(db.clone());

    service
        .log_usage(
            None, "openai", "gpt-5.4", 300_000, 0, 10, 0, 200, false, false,
        )
        .await
        .expect("usage insert succeeds");

    drop(service);
    let sql = format!("{:?}", Arc::try_unwrap(db).unwrap().into_transaction_log());
    assert!(sql.contains("BigInt(Some(1500000))"), "{sql}");
    assert!(!sql.contains("BigInt(Some(750000))"), "{sql}");
}

#[tokio::test]
async fn an_explicit_cost_is_not_overridden_by_the_estimate() {
    use sea_orm::MockDatabase;
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![usage_log_model(None, None)]])
            .into_connection(),
    );
    let service = UsageService::new(db.clone());

    service
        .log_usage(
            None, "openai", "gpt-4o", 1_000, 100, 10, 42, 200, false, false,
        )
        .await
        .expect("usage insert succeeds");

    drop(service);
    let sql = format!("{:?}", Arc::try_unwrap(db).unwrap().into_transaction_log());
    assert!(sql.contains("BigInt(Some(42))"), "{sql}");
    assert!(!sql.contains("BigInt(Some(3500))"), "{sql}");
}

#[test]
fn project_filter_adds_a_bound_condition() {
    let service = UsageService::new(Arc::new(
        sea_orm::MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
    ));
    let to = Utc::now();
    let from = to - chrono::Duration::hours(1);
    let filter = UsageFilter {
        project_id: Some(7),
        ..Default::default()
    };
    let (clause, values) = service.build_filter_clause(from, to, &filter);
    assert!(clause.contains("project_id = $3"), "{clause}");
    assert_eq!(values.len(), 3);
}
