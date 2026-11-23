// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::BTreeMap;

use super::source::{parse_catalog, validate_source_url, MAX_CATALOG_BYTES, MAX_CATALOG_MODELS};
use super::*;
use crate::services::pricing::{build_pricing, CATALOG_TEST_LOCK};

// ============================================================================
// The catalog published in the repository
// ============================================================================

const PUBLISHED_CATALOG: &str = include_str!("../../../catalog/model-catalog.json");
const PUBLISHED_CATALOG_PATH: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/catalog/model-catalog.json");

/// models.dev's id for a Temps provider.
fn catalog_provider_id(provider: &str) -> &str {
    match provider {
        "gemini" => "google",
        other => other,
    }
}

fn provider_name(provider: &str) -> &str {
    match provider {
        "anthropic" => "Anthropic",
        "openai" => "OpenAI",
        "gemini" => "Google",
        "xai" => "xAI",
        "openrouter" => "OpenRouter",
        other => other,
    }
}

/// The published catalog as generated from the bundled price table.
fn generated_catalog() -> String {
    let mut providers: BTreeMap<String, BTreeMap<String, serde_json::Value>> = BTreeMap::new();
    let mut names = BTreeMap::new();
    for row in build_pricing() {
        let provider = catalog_provider_id(&row.provider).to_string();
        names.insert(provider.clone(), provider_name(&row.provider).to_string());
        let mut cost = serde_json::Map::new();
        cost.insert("input".into(), row.input_per_million.into());
        cost.insert("output".into(), row.output_per_million.into());
        if let Some(read) = row.cache_hit_per_million {
            cost.insert("cache_read".into(), read.into());
        }
        if let Some(write) = row.cache_write_5m_per_million {
            cost.insert("cache_write".into(), write.into());
        }
        let mut model = serde_json::Map::new();
        model.insert("id".into(), row.model.clone().into());
        model.insert("name".into(), row.display_name.clone().into());
        model.insert("cost".into(), serde_json::Value::Object(cost));
        if row.deprecated {
            model.insert("deprecated".into(), true.into());
        }
        providers
            .entry(provider)
            .or_default()
            .insert(row.model, serde_json::Value::Object(model));
    }
    let mut document = serde_json::Map::new();
    for (provider, models) in providers {
        let mut entry = serde_json::Map::new();
        entry.insert("id".into(), provider.clone().into());
        entry.insert(
            "name".into(),
            names.get(&provider).cloned().unwrap_or_default().into(),
        );
        entry.insert(
            "models".into(),
            serde_json::Value::Object(models.into_iter().collect()),
        );
        document.insert(provider, serde_json::Value::Object(entry));
    }
    let mut text = serde_json::to_string_pretty(&serde_json::Value::Object(document)).unwrap();
    text.push('\n');
    text
}

/// The published file is generated, never edited by hand. When it is out of
/// date this test rewrites it and fails, so a price change shows up as a diff
/// to review and commit, and CI fails until it is committed.
#[test]
fn the_published_catalog_matches_the_bundled_price_table() {
    let generated = generated_catalog();
    if PUBLISHED_CATALOG != generated {
        std::fs::write(PUBLISHED_CATALOG_PATH, &generated).unwrap();
        panic!(
            "crates/temps-ai-gateway/catalog/model-catalog.json was out of date with the bundled \
             price table and has been regenerated: review and commit it"
        );
    }
}

#[test]
fn the_published_catalog_parses_to_every_bundled_price() {
    let parsed = parse_catalog(DEFAULT_CATALOG_URL, PUBLISHED_CATALOG.as_bytes()).unwrap();
    assert_eq!(parsed.skipped, 0);
    let bundled = build_pricing();
    assert_eq!(parsed.models.len(), bundled.len());
    for row in bundled {
        let model = parsed
            .models
            .iter()
            .find(|model| model.model_id == row.model && model.provider == row.provider)
            .unwrap_or_else(|| panic!("{} {} is published", row.provider, row.model));
        assert_eq!(model.input_per_million, Some(row.input_per_million));
        assert_eq!(model.output_per_million, Some(row.output_per_million));
        assert_eq!(model.cache_read_per_million, row.cache_hit_per_million);
        assert_eq!(model.deprecated, row.deprecated);
    }
}

#[test]
fn the_published_catalog_fits_well_within_the_size_limit() {
    assert!(PUBLISHED_CATALOG.len() < MAX_CATALOG_BYTES / 16);
}

// ============================================================================
// Parsing and validation
// ============================================================================

const SAMPLE: &str = r#"{
  "anthropic": {
    "id": "anthropic",
    "models": {
      "example-claude-next": {
        "id": "example-claude-next",
        "name": "Example Claude Next",
        "attachment": true,
        "reasoning": true,
        "tool_call": true,
        "release_date": "2026-12-01",
        "limit": { "context": 1000000, "output": 128000 },
        "cost": { "input": 4, "output": 20, "cache_read": 0.4, "cache_write": 5,
                  "context_over_200k": { "input": 8, "output": 30 } }
      },
      "example-retired": { "name": "Retired", "status": "deprecated",
                            "cost": { "input": 1, "output": 2 } },
      "example-negative": { "cost": { "input": -1, "output": 2 } },
      "example-string-price": { "cost": { "input": "1.00", "output": 2 } },
      "bad id with spaces": { "cost": { "input": 1, "output": 2 } }
    }
  },
  "google": {
    "models": {
      "example-gemini-next": {
        "modalities": { "input": ["text", "image"], "output": ["text"] },
        "limit": { "context": 0, "output": 999999999999 }
      }
    }
  },
  "some-other-provider": {
    "models": { "ignored-model": { "cost": { "input": "not a number" } } },
    "anything": [1, 2, 3]
  }
}"#;

#[test]
fn a_models_dev_document_is_read_for_supported_providers_only() {
    let parsed = parse_catalog("https://catalog.example.com/api.json", SAMPLE.as_bytes()).unwrap();
    // Three anthropic entries are rejected; the unsupported provider is
    // skipped without being read, so it is not counted.
    assert_eq!(parsed.skipped, 3);
    let ids: Vec<(&str, &str)> = parsed
        .models
        .iter()
        .map(|model| (model.provider.as_str(), model.model_id.as_str()))
        .collect();
    assert_eq!(
        ids,
        vec![
            ("anthropic", "example-claude-next"),
            ("anthropic", "example-retired"),
            ("gemini", "example-gemini-next"),
        ]
    );

    let next = &parsed.models[0];
    assert_eq!(next.display_name, "Example Claude Next");
    assert_eq!(next.input_per_million, Some(4.0));
    assert_eq!(next.output_per_million, Some(20.0));
    assert_eq!(next.cache_read_per_million, Some(0.4));
    assert_eq!(next.cache_write_per_million, Some(5.0));
    assert_eq!(next.context_window, Some(1_000_000));
    assert_eq!(next.max_output_tokens, Some(128_000));
    assert!(next.supports_tools && next.supports_vision && next.supports_reasoning);
    assert_eq!(next.release_date.as_deref(), Some("2026-12-01"));
    assert!(next.price().is_some());

    let retired = &parsed.models[1];
    assert!(retired.deprecated);

    let gemini = &parsed.models[2];
    assert_eq!(gemini.display_name, "example-gemini-next");
    assert!(gemini.supports_vision);
    // Out-of-range limits are dropped, not stored.
    assert_eq!(gemini.context_window, None);
    assert_eq!(gemini.max_output_tokens, None);
    // No price published: listed, but never priced.
    assert!(gemini.price().is_none());
}

#[test]
fn a_document_that_is_not_a_catalog_is_rejected_with_its_url() {
    let url = "https://catalog.example.com/api.json";
    for body in ["[]", "\"text\"", "{", "<html></html>"] {
        match parse_catalog(url, body.as_bytes()) {
            Err(ModelCatalogError::Parse { url: got, .. }) => assert_eq!(got, url),
            other => panic!("{body:?} parsed as {other:?}"),
        }
    }
    match parse_catalog(url, br#"{"some-other-provider": {"models": {}}}"#) {
        Err(ModelCatalogError::Empty { skipped: 0, .. }) => {}
        other => panic!("an empty catalog parsed as {other:?}"),
    }
    match parse_catalog(
        url,
        br#"{"openai": {"models": {"x": {"cost": {"input": -5}}}}}"#,
    ) {
        Err(ModelCatalogError::Empty { skipped: 1, .. }) => {}
        other => panic!("a catalog of invalid entries parsed as {other:?}"),
    }
}

#[test]
fn a_catalog_with_too_many_models_is_rejected() {
    let models: Vec<String> = (0..=MAX_CATALOG_MODELS)
        .map(|i| format!(r#""m-{i}": {{}}"#))
        .collect();
    let body = format!(r#"{{"openai": {{"models": {{{}}}}}}}"#, models.join(","));
    assert!(matches!(
        parse_catalog("https://catalog.example.com/api.json", body.as_bytes()),
        Err(ModelCatalogError::TooManyEntries { limit, .. }) if limit == MAX_CATALOG_MODELS
    ));
}

#[test]
fn only_public_https_urls_without_credentials_are_catalog_sources() {
    assert!(validate_source_url("https://catalog.example.com/api.json").is_ok());
    assert!(validate_source_url(DEFAULT_CATALOG_URL).is_ok());
    for url in [
        "http://catalog.example.com/api.json",
        "ftp://catalog.example.com/api.json",
        "https://user:secret@catalog.example.com/api.json",
        "https://token@catalog.example.com/api.json",
        "https://127.0.0.1/api.json",
        "https://localhost/api.json",
        "https://10.0.0.5/api.json",
        "https://169.254.169.254/latest/meta-data",
        "https://[::1]/api.json",
        "not a url",
    ] {
        match validate_source_url(url) {
            Err(ModelCatalogError::InvalidSourceUrl { url: got, .. }) => assert_eq!(got, url),
            other => panic!("{url} was accepted: {other:?}"),
        }
    }
    let long = format!("https://catalog.example.com/{}", "a".repeat(2100));
    assert!(validate_source_url(&long).is_err());
}

#[test]
fn settings_updates_are_validated_before_anything_is_written() {
    let interval = |hours| ModelCatalogSettingsUpdate {
        refresh_interval_hours: Some(hours),
        ..Default::default()
    };
    assert!(validate_update(&interval(1)).is_ok());
    assert!(validate_update(&interval(168)).is_ok());
    for hours in [0, -1, 169] {
        assert!(matches!(
            validate_update(&interval(hours)),
            Err(ModelCatalogError::InvalidSettings {
                field: "refresh_interval_hours",
                ..
            })
        ));
    }
    let url = |url: &str| ModelCatalogSettingsUpdate {
        source_url: Some(url.to_string()),
        ..Default::default()
    };
    assert_eq!(validate_update(&url("")).unwrap(), Some(None));
    assert_eq!(validate_update(&url("  ")).unwrap(), Some(None));
    assert_eq!(
        validate_update(&url("https://catalog.example.com/api.json")).unwrap(),
        Some(Some("https://catalog.example.com/api.json".to_string()))
    );
    assert!(matches!(
        validate_update(&url("http://catalog.example.com/api.json")),
        Err(ModelCatalogError::InvalidSourceUrl { .. })
    ));
    assert_eq!(
        validate_update(&ModelCatalogSettingsUpdate::default()).unwrap(),
        None
    );
}

// ============================================================================
// Scheduling
// ============================================================================

fn at(hours_ago: i64, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    Some(now - chrono::Duration::hours(hours_ago))
}

#[test]
fn a_catalog_never_refreshed_is_due_at_once_and_then_every_interval() {
    let now = Utc::now();
    let mut row = default_settings_row();
    assert!(refresh_due(&row, now));

    row.last_refreshed_at = at(23, now);
    row.last_attempt_at = row.last_refreshed_at;
    assert!(!refresh_due(&row, now));
    assert_eq!(next_refresh_at(&row, now), at(-1, now));

    row.last_refreshed_at = at(24, now);
    assert!(refresh_due(&row, now));

    row.auto_refresh = false;
    assert!(!refresh_due(&row, now));
    assert_eq!(next_refresh_at(&row, now), None);
}

#[test]
fn a_failed_refresh_waits_before_trying_again() {
    let now = Utc::now();
    let mut row = default_settings_row();
    row.last_error = Some("HTTP 503".into());
    row.last_attempt_at = Some(now - chrono::Duration::minutes(30));
    assert!(!refresh_due(&row, now), "retried within the hour");
    row.last_attempt_at = Some(now - chrono::Duration::minutes(61));
    assert!(refresh_due(&row, now));
}

#[test]
fn provider_discovery_follows_the_interval_from_boot() {
    let now = Utc::now();
    let mut row = default_settings_row();
    // The plugin discovers every key at startup.
    assert!(!discovery_due(&row, now, now - chrono::Duration::hours(1)));
    assert!(discovery_due(&row, now, now - chrono::Duration::hours(25)));
    row.last_discovery_at = at(2, now);
    assert!(!discovery_due(&row, now, now - chrono::Duration::hours(25)));
    row.last_discovery_at = at(24, now);
    assert!(discovery_due(&row, now, now - chrono::Duration::hours(30)));
    row.discover_provider_models = false;
    assert!(!discovery_due(&row, now, now - chrono::Duration::hours(30)));
}

// ============================================================================
// Fetching
// ============================================================================

#[tokio::test]
async fn a_fetch_is_bounded_by_size_and_reports_http_failures() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ok.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/big.json"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b' '; MAX_CATALOG_BYTES + 1]))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/missing.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let client = ProviderHttpClient::unrestricted_for_tests();
    let url = |path: &str| url::Url::parse(&format!("{}{path}", server.uri())).unwrap();

    let body = source::fetch_catalog(&client, &url("/ok.json"))
        .await
        .unwrap();
    assert_eq!(body, SAMPLE.as_bytes());
    assert!(matches!(
        source::fetch_catalog(&client, &url("/big.json")).await,
        Err(ModelCatalogError::TooLarge { limit_bytes, .. }) if limit_bytes == MAX_CATALOG_BYTES
    ));
    assert!(matches!(
        source::fetch_catalog(&client, &url("/missing.json")).await,
        Err(ModelCatalogError::HttpStatus { status: 404, .. })
    ));
}

// ============================================================================
// Storage, on a real database
// ============================================================================

async fn test_database() -> Option<temps_test_utils::TestDatabase> {
    match temps_test_utils::TestDatabase::with_migrations().await {
        Ok(db) => Some(db),
        Err(error) if temps_test_utils::is_container_runtime_unavailable(&format!("{error:#}")) => {
            println!("Skipping: no container runtime for the PostgreSQL test database: {error:#}");
            None
        }
        Err(error) => panic!("test database setup failed: {error:#}"),
    }
}

fn service_on(db: Arc<DatabaseConnection>) -> ModelCatalogService {
    let encryption = Arc::new(
        temps_core::EncryptionService::new(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .unwrap(),
    );
    let keys = Arc::new(ProviderKeyService::new(db.clone(), encryption));
    let gateway = Arc::new(crate::services::GatewayService::new(keys.clone()));
    let models = Arc::new(ProviderModelService::new(db.clone(), keys.clone(), gateway));
    ModelCatalogService::new(db, keys, models)
}

fn model(provider: &str, id: &str, price: Option<(f64, f64)>) -> CatalogModel {
    CatalogModel {
        provider: provider.into(),
        model_id: id.into(),
        display_name: id.into(),
        input_per_million: price.map(|p| p.0),
        output_per_million: price.map(|p| p.1),
        cache_read_per_million: None,
        cache_write_per_million: None,
        context_window: Some(200_000),
        max_output_tokens: None,
        supports_tools: true,
        supports_vision: false,
        supports_reasoning: false,
        deprecated: false,
        release_date: Some("2026-12-01".into()),
    }
}

#[tokio::test]
async fn settings_default_until_changed_and_changes_report_their_fields() {
    let Some(test_db) = test_database().await else {
        return;
    };
    let service = service_on(test_db.connection_arc());

    let defaults = service.settings().await.unwrap();
    assert_eq!(defaults.source_url, None);
    assert_eq!(defaults.effective_source_url, DEFAULT_CATALOG_URL);
    assert!(defaults.auto_refresh && defaults.discover_provider_models);
    assert_eq!(
        defaults.refresh_interval_hours,
        DEFAULT_REFRESH_INTERVAL_HOURS
    );

    let (saved, changed) = service
        .update_settings(ModelCatalogSettingsUpdate {
            source_url: Some("https://catalog.example.com/api.json".into()),
            auto_refresh: Some(false),
            refresh_interval_hours: Some(DEFAULT_REFRESH_INTERVAL_HOURS),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(changed, vec!["source_url", "auto_refresh"]);
    assert_eq!(
        saved.effective_source_url,
        "https://catalog.example.com/api.json"
    );
    assert!(!saved.auto_refresh);

    // An invalid change writes nothing.
    assert!(service
        .update_settings(ModelCatalogSettingsUpdate {
            source_url: Some("http://10.0.0.5/api.json".into()),
            auto_refresh: Some(true),
            ..Default::default()
        })
        .await
        .is_err());
    assert!(!service.settings().await.unwrap().auto_refresh);

    let (reset, changed) = service
        .update_settings(ModelCatalogSettingsUpdate {
            source_url: Some(String::new()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(changed, vec!["source_url"]);
    assert_eq!(reset.effective_source_url, DEFAULT_CATALOG_URL);
}

#[tokio::test]
async fn a_stored_catalog_replaces_the_previous_one_and_survives_a_restart() {
    let Some(test_db) = test_database().await else {
        return;
    };
    let _serial = CATALOG_TEST_LOCK.lock().await;
    let service = service_on(test_db.connection_arc());
    let url = "https://catalog.example.com/api.json";
    let first = ParsedCatalog {
        models: vec![
            model("openai", "example-stored-first", Some((1.0, 2.0))),
            model("openai", "example-stored-unpriced", None),
        ],
        skipped: 1,
    };
    let (row, applied) = service.store_and_apply(&first, url).await.unwrap();
    assert_eq!(applied, 1);
    assert_eq!((row.entry_count, row.skipped_count), (2, 1));
    assert_eq!(row.last_source_url.as_deref(), Some(url));
    assert!(row.last_error.is_none() && row.last_refreshed_at.is_some());
    assert!(pricing::is_priced("example-stored-first"));
    assert!(!pricing::is_priced("example-stored-unpriced"));

    let second = ParsedCatalog {
        models: vec![model(
            "anthropic",
            "example-stored-second",
            Some((3.0, 4.0)),
        )],
        skipped: 0,
    };
    service.store_and_apply(&second, url).await.unwrap();
    let page = service
        .list_entries(ModelCatalogEntryFilter::default(), 1, 20)
        .await
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].model_id, "example-stored-second");
    assert!(!pricing::is_priced("example-stored-first"));

    // A restart applies the stored catalog again.
    pricing::install_catalog_prices(&[]);
    assert!(!pricing::is_priced("example-stored-second"));
    assert_eq!(service.load_stored().await.unwrap(), 1);
    assert!(pricing::is_priced("example-stored-second"));

    let status = service.status().await.unwrap();
    assert_eq!(status.entry_count, 1);
    assert!(status.applied_price_count >= 1);
    assert!(!status.refreshing);
    pricing::install_catalog_prices(&[]);
}

#[tokio::test]
async fn stored_entries_are_filtered_searched_and_paginated() {
    let Some(test_db) = test_database().await else {
        return;
    };
    let _serial = CATALOG_TEST_LOCK.lock().await;
    let service = service_on(test_db.connection_arc());
    let models = (0..25)
        .map(|i| model("openai", &format!("example-page-{i:02}"), None))
        .chain([model("anthropic", "example-search_target%", None)])
        .collect();
    service
        .store_and_apply(
            &ParsedCatalog { models, skipped: 0 },
            "https://catalog.example.com/a",
        )
        .await
        .unwrap();

    let first = service
        .list_entries(ModelCatalogEntryFilter::default(), 1, 20)
        .await
        .unwrap();
    assert_eq!((first.total, first.items.len()), (26, 20));
    let second = service
        .list_entries(ModelCatalogEntryFilter::default(), 2, 20)
        .await
        .unwrap();
    assert_eq!(second.items.len(), 6);
    let capped = service
        .list_entries(ModelCatalogEntryFilter::default(), 1, 1_000)
        .await
        .unwrap();
    assert_eq!(capped.page_size, 100);

    let anthropic = service
        .list_entries(
            ModelCatalogEntryFilter {
                provider: Some("anthropic".into()),
                search: None,
            },
            1,
            20,
        )
        .await
        .unwrap();
    assert_eq!(anthropic.total, 1);
    // `%` and `_` in a search are literal characters, not wildcards.
    let literal = service
        .list_entries(
            ModelCatalogEntryFilter {
                provider: None,
                search: Some("SEARCH_TARGET%".into()),
            },
            1,
            20,
        )
        .await
        .unwrap();
    assert_eq!(literal.total, 1);
    let wildcard = service
        .list_entries(
            ModelCatalogEntryFilter {
                provider: None,
                search: Some("page_%".into()),
            },
            1,
            20,
        )
        .await
        .unwrap();
    assert_eq!(wildcard.total, 0);
    pricing::install_catalog_prices(&[]);
}

#[tokio::test]
async fn enabled_models_no_price_covers_are_reported_and_self_hosted_ones_are_not() {
    use sea_orm::ConnectionTrait;
    let Some(test_db) = test_database().await else {
        return;
    };
    let db = test_db.connection_arc();
    db.execute_unprepared(
        "INSERT INTO ai_provider_keys (id, provider, display_name, api_key_encrypted, is_active) VALUES \
         (901, 'openrouter', 'Router', 'enc', true), \
         (902, 'openai_compatible', 'Local', 'enc', true), \
         (903, 'openai', 'Disabled key', 'enc', false)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "INSERT INTO ai_provider_models \
         (provider_key_id, model_id, display_name, source, is_available, is_enabled, metadata) VALUES \
         (901, 'vendor/example-unknown', 'Unknown', 'discovered', true, true, NULL), \
         (901, 'vendor/example-priced', 'Priced', 'discovered', true, true, \
          '{\"input_price_per_million\": 1.0, \"output_price_per_million\": 2.0}'), \
         (901, 'anthropic/claude-haiku-5.5', 'Listed', 'discovered', true, true, NULL), \
         (901, 'vendor/example-disabled', 'Disabled', 'discovered', true, false, NULL), \
         (902, 'local-example', 'Local', 'discovered', true, true, NULL), \
         (903, 'example-inactive-key', 'Inactive', 'manual', true, true, NULL)",
    )
    .await
    .unwrap();
    let report = service_on(db).unpriced_models().await.unwrap();
    assert_eq!(report.total, 1);
    assert_eq!(
        report.models,
        vec![UnpricedModel {
            provider_key_id: 901,
            provider_key_name: "Router".into(),
            provider: "openrouter".into(),
            model_id: "vendor/example-unknown".into(),
        }]
    );
}
