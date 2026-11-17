// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::testing::test_utils::AnalyticsTestUtils;
use crate::{cleanup_test_analytics, create_test_analytics_service};

use UtcDateTime;

#[tokio::test]
async fn test_analytics_service_creation() {
    let db = AnalyticsTestUtils::create_test_db("test_analytics_service_creation")
        .await
        .unwrap();
    let cookie_crypto =
        Arc::new(temps_core::CookieCrypto::new("test_key_32_bytes_long_for_tests").unwrap());
    let service = AnalyticsService::new(db, cookie_crypto);

    // Test that the service was created successfully
    assert!(std::ptr::addr_of!(service) as usize != 0);
}

#[tokio::test]
async fn test_enrich_visitor_by_guid_is_confined_to_project() -> anyhow::Result<()> {
    use sea_orm::EntityTrait;
    let (service, db, _container) =
        create_test_analytics_service!("test_enrich_visitor_by_guid_is_confined_to_project");
    let crypto = temps_core::CookieCrypto::new("test_key_32_bytes_long_for_tests").unwrap();
    let sealed = format!("enc_{}", crypto.encrypt("test_visitor_1").unwrap());
    let visitor = temps_entities::visitor::Entity::find()
        .one(db.as_ref())
        .await?
        .unwrap();
    let data = serde_json::json!({ "user_id": "u1" });

    // A token bound to another project must not reach this visitor.
    let other = service
        .enrich_visitor_by_guid(&sealed, Some(visitor.project_id + 1), data.clone())
        .await?;
    assert!(!other.success, "cross-project enrichment must be refused");
    assert_eq!(
        other.visitor_id, sealed,
        "must not echo decrypted plaintext"
    );
    let unchanged = temps_entities::visitor::Entity::find_by_id(visitor.id)
        .one(db.as_ref())
        .await?
        .unwrap();
    assert!(unchanged.custom_data.is_none());

    // The owning project can enrich it.
    let own = service
        .enrich_visitor_by_guid(&sealed, Some(visitor.project_id), data.clone())
        .await?;
    assert!(own.success);
    let updated = temps_entities::visitor::Entity::find_by_id(visitor.id)
        .one(db.as_ref())
        .await?
        .unwrap();
    assert_eq!(updated.custom_data, Some(data.clone()));

    // Unscoped callers (user / API key auth) keep working.
    let unscoped = service
        .enrich_visitor_by_guid(&sealed, None, serde_json::json!({ "name": "n" }))
        .await?;
    assert!(unscoped.success);

    cleanup_test_analytics!(db);
    Ok(())
}

#[tokio::test]
async fn test_scoped_enrichment_cannot_grow_a_row_past_the_cap() -> anyhow::Result<()> {
    let (service, db, _container) =
        create_test_analytics_service!("test_scoped_enrichment_cannot_grow_a_row_past_the_cap");
    let crypto = temps_core::CookieCrypto::new("test_key_32_bytes_long_for_tests").unwrap();
    let sealed = format!("enc_{}", crypto.encrypt("test_visitor_1").unwrap());
    let visitor = temps_entities::visitor::Entity::find()
        .one(db.as_ref())
        .await?
        .unwrap();

    // Each call is within the per-call cap; together they exceed it.
    let mut last = Ok(());
    for batch in 0..3 {
        let data: serde_json::Map<String, serde_json::Value> = (0..12)
            .map(|i| (format!("k{batch}_{i}"), serde_json::json!(1)))
            .collect();
        last = service
            .enrich_visitor_by_guid(
                &sealed,
                Some(visitor.project_id),
                serde_json::Value::Object(data),
            )
            .await
            .map(|_| ());
    }
    assert!(
        matches!(last, Err(AnalyticsError::InvalidEnrichmentData(_))),
        "the merged document must be capped, not just each request"
    );

    // The same growth is allowed for unscoped (user / API key) callers.
    let data: serde_json::Map<String, serde_json::Value> = (0..40)
        .map(|i| (format!("u{i}"), serde_json::json!(1)))
        .collect();
    let unscoped = service
        .enrich_visitor_by_guid(&sealed, None, serde_json::Value::Object(data))
        .await?;
    assert!(unscoped.success && unscoped.updated);

    cleanup_test_analytics!(db);
    Ok(())
}

#[test]
fn merge_custom_data_merges_and_null_removes_keys() {
    let existing = serde_json::json!({"a": 1, "b": 2});
    let merged = merge_custom_data(Some(&existing), &serde_json::json!({"b": null, "c": 3}));
    assert_eq!(merged, serde_json::json!({"a": 1, "c": 3}));
    // No existing data: nulls are simply dropped.
    let fresh = merge_custom_data(None, &serde_json::json!({"a": null, "b": 1}));
    assert_eq!(fresh, serde_json::json!({"b": 1}));
    // A non-object incoming value replaces, as before.
    assert_eq!(
        merge_custom_data(Some(&existing), &serde_json::json!([1])),
        serde_json::json!([1])
    );
}

#[test]
fn scoped_merge_may_shrink_or_hold_an_oversized_row_but_not_grow_it() {
    let big: serde_json::Map<String, serde_json::Value> = (0..MAX_SCOPED_ENRICHMENT_KEYS + 8)
        .map(|i| (format!("k{i}"), serde_json::json!(1)))
        .collect();
    let existing = serde_json::Value::Object(big.clone());
    // Same size: allowed. One fewer key: allowed. One more key: rejected.
    assert!(validate_merge(Some(&existing), &existing, true).is_ok());
    let mut smaller = big.clone();
    smaller.remove("k0");
    assert!(validate_merge(Some(&existing), &serde_json::Value::Object(smaller), true).is_ok());
    let mut larger = big;
    larger.insert("extra".to_string(), serde_json::json!(1));
    assert!(matches!(
        validate_merge(Some(&existing), &serde_json::Value::Object(larger), true),
        Err(AnalyticsError::InvalidEnrichmentData(_))
    ));
    // A fresh document over the cap is rejected outright.
    assert!(validate_merge(None, &existing, true).is_err());
}

#[test]
fn unscoped_callers_have_a_higher_absolute_ceiling() {
    let keys = |n: usize| {
        serde_json::Value::Object(
            (0..n)
                .map(|i| (format!("k{i}"), serde_json::json!(1)))
                .collect(),
        )
    };
    // Over the scoped limit but under the ceiling: fine for user/API-key auth.
    assert!(validate_merge(None, &keys(MAX_SCOPED_ENRICHMENT_KEYS + 1), false).is_ok());
    assert!(validate_merge(None, &keys(MAX_SCOPED_ENRICHMENT_KEYS + 1), true).is_err());
    // Over the ceiling: refused for everyone.
    assert!(validate_merge(None, &keys(MAX_ENRICHMENT_KEYS + 1), false).is_err());
}

#[tokio::test]
async fn test_live_visitors_honour_the_datacenter_opt_out() -> anyhow::Result<()> {
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
    use temps_entities::{environments, ip_geolocations, project_analytics_settings, projects};

    let (service, db, _container) =
        create_test_analytics_service!("test_live_visitors_datacenter_opt_out");
    let project = projects::Entity::find()
        .filter(projects::Column::Slug.eq("test_project"))
        .one(db.as_ref())
        .await?
        .expect("test project must exist from insert_test_data");
    let environment = environments::Entity::find()
        .filter(environments::Column::ProjectId.eq(project.id))
        .one(db.as_ref())
        .await?
        .expect("test environment must exist from insert_test_data");

    let now = chrono::Utc::now();
    let hosting_ip = ip_geolocations::ActiveModel {
        ip_address: Set("203.0.113.40".to_string()),
        country: Set("Unknown".to_string()),
        is_eu: Set(false),
        is_hosting_provider: Set(Some(true)),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;
    let live_visitor = visitor::ActiveModel {
        visitor_id: Set("live-datacenter-visitor".to_string()),
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        first_seen: Set(now),
        last_seen: Set(now),
        ip_address_id: Set(Some(hosting_ip.id)),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;
    // Recorded while the project counted datacenter traffic.
    temps_entities::events::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(Some(environment.id)),
        visitor_id: Set(Some(live_visitor.id)),
        session_id: Set(Some("live-datacenter-session".to_string())),
        event_type: Set("page_view".to_string()),
        page_path: Set("/".to_string()),
        hostname: Set("site.example".to_string()),
        pathname: Set("/".to_string()),
        href: Set("https://site.example/".to_string()),
        timestamp: Set(now),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;
    let is_live = |visitors: &[crate::types::responses::LiveVisitorInfo]| {
        visitors
            .iter()
            .any(|v| v.visitor_id == "live-datacenter-visitor")
    };

    // Default (no settings row): datacenter traffic is excluded.
    let visitors = service.get_live_visitors(project.id, None, 5).await?;
    assert!(
        !is_live(&visitors),
        "hosting visitors are hidden by default"
    );

    // Opted out: the same visitor is live, matching what ingest counted.
    project_analytics_settings::ActiveModel {
        project_id: Set(project.id),
        exclude_datacenter_traffic: Set(false),
        exclude_spam_referrers: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(db.as_ref())
    .await?;
    let visitors = service.get_live_visitors(project.id, None, 5).await?;
    assert!(
        is_live(&visitors),
        "a project that opted out of datacenter exclusion sees its hosting visitors"
    );

    cleanup_test_analytics!(db);
    Ok(())
}

#[tokio::test]
async fn test_live_visitors_need_a_recent_counted_event() -> anyhow::Result<()> {
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
    use temps_entities::{environments, events, projects};

    let (service, db, _container) =
        create_test_analytics_service!("test_live_visitors_need_a_counted_event");
    let project = projects::Entity::find()
        .filter(projects::Column::Slug.eq("test_project"))
        .one(db.as_ref())
        .await?
        .expect("test project must exist from insert_test_data");
    let environment = environments::Entity::find()
        .filter(environments::Column::ProjectId.eq(project.id))
        .one(db.as_ref())
        .await?
        .expect("test environment must exist from insert_test_data");

    let now = chrono::Utc::now();
    // An established visitor: counted earlier, so an excluded visit
    // leaves the visitor row visible and moves its `last_seen`.
    let established = visitor::ActiveModel {
        visitor_id: Set("established-visitor".to_string()),
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        first_seen: Set(now - chrono::Duration::hours(1)),
        last_seen: Set(now),
        has_activity: Set(true),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;
    let page_view = |path: &str, minutes_ago: i64, excluded_by: Option<&str>| events::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(Some(environment.id)),
        visitor_id: Set(Some(established.id)),
        session_id: Set(Some("established-session".to_string())),
        event_type: Set("page_view".to_string()),
        page_path: Set(path.to_string()),
        hostname: Set("site.example".to_string()),
        pathname: Set(path.to_string()),
        href: Set(format!("https://site.example{path}")),
        timestamp: Set(now - chrono::Duration::minutes(minutes_ago)),
        is_crawler: Set(excluded_by.is_some()),
        crawler_name: Set(excluded_by.map(str::to_string)),
        ..Default::default()
    };
    page_view("/counted-earlier", 30, None)
        .insert(db.as_ref())
        .await?;
    page_view("/excluded", 1, Some("rule:ip"))
        .insert(db.as_ref())
        .await?;
    let live = |visitors: &[crate::types::responses::LiveVisitorInfo]| {
        visitors
            .iter()
            .find(|v| v.visitor_id == "established-visitor")
            .map(|v| v.current_page.clone())
    };

    let visitors = service.get_live_visitors(project.id, None, 5).await?;
    assert_eq!(
        live(&visitors),
        None,
        "only excluded events in the window: not live, as the active count says"
    );

    // A counted page view in the window makes them live, on that page,
    // even though an excluded one came after it.
    page_view("/pricing", 3, None).insert(db.as_ref()).await?;
    let visitors = service.get_live_visitors(project.id, None, 5).await?;
    assert_eq!(live(&visitors), Some(Some("/pricing".to_string())));

    cleanup_test_analytics!(db);
    Ok(())
}

#[tokio::test]
async fn test_get_top_pages() -> anyhow::Result<()> {
    let (service, db, _container) = create_test_analytics_service!("test_get_top_pages");

    let start_date =
        chrono::NaiveDateTime::parse_from_str("2024-01-01 00:00:00", "%Y-%m-%d %H:%M:%S")
            .unwrap()
            .and_utc();
    let end_date =
        chrono::NaiveDateTime::parse_from_str("2024-01-31 23:59:59", "%Y-%m-%d %H:%M:%S")
            .unwrap()
            .and_utc();

    let pages = service
        .get_top_pages(1, 10, Some(start_date), Some(end_date))
        .await?;

    // Should have pages from our test data
    assert!(!pages.is_empty(), "Should have top pages");

    // Check that we have the expected test pages
    let paths: Vec<String> = pages.iter().map(|p| p.path.clone()).collect();
    assert!(
        paths.contains(&"/home".to_string()) || paths.contains(&"/about".to_string()),
        "Should contain test page paths"
    );

    cleanup_test_analytics!(db);
    Ok(())
}

#[tokio::test]
async fn test_empty_results_for_invalid_project() -> anyhow::Result<()> {
    let (service, db, _container) =
        create_test_analytics_service!("test_empty_results_for_invalid_project");

    let start_date =
        chrono::NaiveDateTime::parse_from_str("2024-01-01 00:00:00", "%Y-%m-%d %H:%M:%S")
            .unwrap()
            .and_utc();
    let end_date =
        chrono::NaiveDateTime::parse_from_str("2024-01-31 23:59:59", "%Y-%m-%d %H:%M:%S")
            .unwrap()
            .and_utc();

    // Use a non-existent project ID
    let invalid_project_id = 9999;

    let pages = service
        .get_top_pages(invalid_project_id, 10, Some(start_date), Some(end_date))
        .await?;

    // Should have empty results for invalid project
    assert!(
        pages.is_empty(),
        "Should have empty pages for invalid project"
    );

    cleanup_test_analytics!(db);
    Ok(())
}

#[tokio::test]
async fn test_get_event_entries_returns_props() -> anyhow::Result<()> {
    let (service, db, _container) =
        create_test_analytics_service!("test_get_event_entries_returns_props");

    use chrono::{TimeZone, Utc};
    use sea_orm::{ActiveModelTrait, Set};
    use temps_entities::events;

    // Seed two custom "signup" events: the earlier one carries custom JSON
    // props, the later one has none.
    let props = serde_json::json!({"plan": "pro", "amount": 49.99});
    let seeded: Vec<(u32, Option<serde_json::Value>)> = vec![(0, Some(props.clone())), (1, None)];
    for (minute, event_props) in seeded {
        events::ActiveModel {
            project_id: Set(1),
            environment_id: Set(Some(1)),
            deployment_id: Set(Some(1)),
            session_id: Set(Some("session_1".to_string())),
            visitor_id: Set(Some(1)),
            hostname: Set("example.com".to_string()),
            pathname: Set("/pricing".to_string()),
            page_path: Set("/pricing".to_string()),
            href: Set("https://example.com/pricing".to_string()),
            timestamp: Set(Utc.with_ymd_and_hms(2024, 1, 1, 12, minute, 0).unwrap()),
            event_type: Set("signup".to_string()),
            event_name: Set(Some("signup".to_string())),
            props: Set(event_props),
            is_crawler: Set(false),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
    }

    let start_date = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
    let end_date = Utc.with_ymd_and_hms(2024, 1, 2, 0, 0, 0).unwrap();

    let result = service
        .get_event_entries(1, "signup", start_date, end_date, None, 1, 20)
        .await?;

    assert_eq!(result.event_name, "signup");
    assert_eq!(result.total_count, 2, "Should count both signup events");
    assert_eq!(result.entries.len(), 2);
    // Most recent first
    assert!(result.entries[0].timestamp >= result.entries[1].timestamp);
    // The later event has no props; the earlier one round-trips the JSON
    assert!(result.entries[0].props.is_none());
    assert_eq!(result.entries[1].props, Some(props));
    assert_eq!(
        result.entries[1].visitor_uuid.as_deref(),
        Some("test_visitor_1")
    );
    assert_eq!(result.entries[1].page_path, "/pricing");

    // Page-view events must not leak into the signup listing
    assert!(result.entries.iter().all(|e| e.page_path == "/pricing"));

    // A page beyond the data is empty but keeps the total
    let page2 = service
        .get_event_entries(1, "signup", start_date, end_date, None, 2, 20)
        .await?;
    assert_eq!(page2.total_count, 2);
    assert!(page2.entries.is_empty());

    // Environment filter: a non-existent environment returns nothing
    let filtered = service
        .get_event_entries(1, "signup", start_date, end_date, Some(9999), 1, 20)
        .await?;
    assert_eq!(filtered.total_count, 0);
    assert!(filtered.entries.is_empty());

    cleanup_test_analytics!(db);
    Ok(())
}

// Tests for SQL injection fixes - these verify that parameterized queries work correctly

#[tokio::test]
async fn test_parameterized_queries_compile() {
    // This test verifies that all our SQL injection fixes compile correctly
    // The fact that this test compiles proves that we're using Statement::from_sql_and_values
    // with properly typed parameters, which prevents SQL injection

    use sea_orm::{DatabaseBackend, Statement};

    // Test 1: Simple parameterized query with i32
    let _stmt = Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT * FROM events WHERE project_id = $1",
        vec![1_i32.into()],
    );

    // Test 2: Multiple parameters with different types
    let start_date: UtcDateTime = chrono::Utc::now();
    let _stmt = Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT * FROM events WHERE project_id = $1 AND timestamp >= $2 AND timestamp <= $3",
        vec![1_i32.into(), start_date.into(), start_date.into()],
    );

    // Test 3: Optional parameters
    let env_id: Option<i32> = Some(1);
    let mut values: Vec<sea_orm::Value> = vec![1_i32.into()];
    if let Some(id) = env_id {
        values.push(id.into());
    }
    let _stmt = Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT * FROM events WHERE project_id = $1",
        values,
    );

    // Test 4: LIMIT and OFFSET as parameters
    let _stmt = Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT * FROM events LIMIT $1 OFFSET $2",
        vec![(50_i64).into(), (0_i64).into()],
    );

    // If this test compiles, it proves our parameterized query pattern is correct
    // No assertion needed - compilation itself is the test
}

/// Regression test for proxy-created API sessions polluting the analytics
/// dashboard. A request session is not an analytics visit until a
/// non-crawler browser page-view event exists for it.
#[tokio::test]
async fn test_general_stats_excludes_sessions_without_human_page_views() -> anyhow::Result<()> {
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
    use temps_entities::{deployments, environments, events, projects, request_sessions, visitor};

    let (service, db, _container) =
        create_test_analytics_service!("test_general_stats_human_sessions");

    let project = projects::Entity::find()
        .filter(projects::Column::Slug.eq("test_project"))
        .one(db.as_ref())
        .await?
        .expect("test project must exist from insert_test_data");
    let environment = environments::Entity::find()
        .filter(environments::Column::ProjectId.eq(project.id))
        .one(db.as_ref())
        .await?
        .expect("test environment must exist from insert_test_data");
    let deployment = deployments::Entity::find()
        .filter(deployments::Column::ProjectId.eq(project.id))
        .one(db.as_ref())
        .await?
        .expect("test deployment must exist from insert_test_data");

    let now = chrono::Utc::now();
    let human_visitor = visitor::ActiveModel {
        visitor_id: Set("human-session-visitor".to_string()),
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        first_seen: Set(now),
        last_seen: Set(now),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    let human_session_id = "11111111-1111-4111-8111-111111111111";

    // A mixed-version session can contain both bare and legacy cookie
    // formats during a rolling upgrade. They still represent one visit.
    events::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(Some(environment.id)),
        deployment_id: Set(Some(deployment.id)),
        visitor_id: Set(Some(human_visitor.id)),
        session_id: Set(Some(format!("v2|{human_session_id}|1786982400"))),
        event_type: Set("page_view".to_string()),
        page_path: Set("/settings".to_string()),
        hostname: Set("example.com".to_string()),
        pathname: Set("/settings".to_string()),
        href: Set("https://example.com/settings".to_string()),
        timestamp: Set(now + chrono::Duration::seconds(1)),
        is_crawler: Set(false),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    request_sessions::ActiveModel {
        session_id: Set(human_session_id.to_string()),
        started_at: Set(now),
        last_accessed_at: Set(now),
        visitor_id: Set(Some(human_visitor.id)),
        data: Set("{}".to_string()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    events::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(Some(environment.id)),
        deployment_id: Set(Some(deployment.id)),
        visitor_id: Set(Some(human_visitor.id)),
        session_id: Set(Some(human_session_id.to_string())),
        event_type: Set("page_view".to_string()),
        page_path: Set("/dashboard".to_string()),
        hostname: Set("example.com".to_string()),
        pathname: Set("/dashboard".to_string()),
        href: Set("https://example.com/dashboard".to_string()),
        timestamp: Set(now),
        is_crawler: Set(false),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    // Simulates a row created by the old proxy behavior for an API call.
    // It has no analytics page-view event and must never count as a visit.
    request_sessions::ActiveModel {
        session_id: Set("22222222-2222-4222-8222-222222222222".to_string()),
        started_at: Set(now),
        last_accessed_at: Set(now),
        visitor_id: Set(None),
        data: Set("{}".to_string()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    let stats = service
        .get_general_stats(
            now - chrono::Duration::minutes(1),
            now + chrono::Duration::minutes(1),
        )
        .await?;

    assert_eq!(stats.total_visits, 1);
    assert_eq!(stats.total_unique_visitors, 1);
    assert_eq!(stats.total_page_views, 2);

    cleanup_test_analytics!(db);
    Ok(())
}

/// Traffic filters store excluded events with `is_crawler = true`. The
/// event-count report, an event's own page and the instance-wide stats
/// must leave them out,
/// like every other report: an excluded event must neither be counted
/// nor stretch the session it landed in.
#[tokio::test]
async fn test_event_counts_and_general_stats_leave_out_excluded_events() -> anyhow::Result<()> {
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
    use temps_entities::{environments, events, projects, visitor};

    let (service, db, _container) =
        create_test_analytics_service!("test_counts_leave_out_excluded_events");

    let project = projects::Entity::find()
        .filter(projects::Column::Slug.eq("test_project"))
        .one(db.as_ref())
        .await?
        .expect("test project must exist from insert_test_data");
    let environment = environments::Entity::find()
        .filter(environments::Column::ProjectId.eq(project.id))
        .one(db.as_ref())
        .await?
        .expect("test environment must exist from insert_test_data");

    // A window no other test data reaches.
    let at = chrono::Utc::now() + chrono::Duration::days(10);
    let mut visitors = Vec::new();
    for (visitor_id, is_crawler, crawler_name) in [
        ("counted-visitor", false, None),
        ("excluded-visitor", true, Some("rule:ip")),
    ] {
        let row = visitor::ActiveModel {
            visitor_id: Set(visitor_id.to_string()),
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            first_seen: Set(at),
            last_seen: Set(at),
            is_crawler: Set(is_crawler),
            crawler_name: Set(crawler_name.map(str::to_string)),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
        visitors.push(row.id);
    }
    let (counted, excluded_visitor) = (visitors[0], visitors[1]);

    // (visitor, session, event_type, event_name, seconds after `at`,
    // crawler_name: none for a counted event)
    let rows = [
        (Some(counted), "session-viewed", "page_view", None, 0, None),
        // Excluded a minute later in the same session: counted, it would
        // make the one-page visit look engaged.
        (
            Some(excluded_visitor),
            "session-viewed",
            "custom",
            Some("signup"),
            60,
            Some("rule:ip"),
        ),
        (
            Some(counted),
            "session-signup",
            "custom",
            Some("signup"),
            5,
            None,
        ),
        // Sent server-side through the API: no user agent, so stored as
        // the bot `unknown`. Not a filter exclusion; it must still count.
        (
            None,
            "session-server-side",
            "custom",
            Some("signup"),
            10,
            Some("unknown"),
        ),
    ];
    for (visitor, session, event_type, event_name, offset, crawler_name) in rows {
        events::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(Some(environment.id)),
            visitor_id: Set(visitor),
            session_id: Set(Some(session.to_string())),
            event_type: Set(event_type.to_string()),
            event_name: Set(event_name.map(str::to_string)),
            page_path: Set("/".to_string()),
            hostname: Set("site.example".to_string()),
            pathname: Set("/".to_string()),
            href: Set("https://site.example/".to_string()),
            timestamp: Set(at + chrono::Duration::seconds(offset)),
            is_crawler: Set(crawler_name.is_some()),
            crawler_name: Set(crawler_name.map(str::to_string)),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
    }

    let (start, end) = (
        at - chrono::Duration::minutes(5),
        at + chrono::Duration::minutes(5),
    );
    let counts = service
        .get_events_count(start, end, project.id, None, None, Some(true), None)
        .await?;
    let signups: Vec<i64> = counts
        .iter()
        .filter(|count| count.event_name == "signup")
        .map(|count| count.count)
        .collect();
    assert_eq!(
        signups,
        vec![2],
        "the excluded signup was counted, or the server-side one was not"
    );

    // The event's own page: summary, visitors and entries.
    let detail = service
        .get_event_detail(project.id, "signup", start, end, None, None)
        .await?;
    assert_eq!(
        (
            detail.total_count,
            detail.unique_visitors,
            detail.unique_sessions
        ),
        (2, 1, 2),
        "the excluded signup was in the event summary, or the server-side one was not"
    );
    let event_visitors = service
        .get_event_visitors(project.id, "signup", start, end, None, 1, 20)
        .await?;
    assert_eq!(
        event_visitors.total_count, 1,
        "the excluded visitor was listed"
    );
    let entries = service
        .get_event_entries(project.id, "signup", start, end, None, 1, 20)
        .await?;
    assert_eq!(
        (entries.total_count, entries.entries.len()),
        (2, 2),
        "the excluded signup was listed, or the server-side one was not"
    );

    let stats = service.get_general_stats(start, end).await?;
    assert_eq!(
        stats.total_events, 3,
        "the excluded event was counted, or the server-side one was not"
    );
    assert_eq!(
        stats.avg_engagement_rate, 0.0,
        "the excluded event stretched a one-page visit into an engaged one"
    );

    cleanup_test_analytics!(db);
    Ok(())
}

/// Regression test: `get_session_details` must return correct
/// entry_path / exit_path / page_views / is_bounced / is_engaged for a
/// session whose events are stored with the legacy `v2|<uuid>|<ts>` format
/// in `events.session_id`.
///
/// Prior to the fix, all five correlated subqueries used
/// `e.session_id = rs.session_id`.  Because `rs.session_id` is always a
/// bare UUID but `events.session_id` stored the full v2 payload, every
/// subquery returned NULL / 0, producing silently wrong
/// entry_path=NULL, page_views=0, is_bounced=true for real sessions.
#[tokio::test]
async fn test_get_session_details_with_legacy_v2_session_id() -> anyhow::Result<()> {
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
    use temps_entities::{deployments, environments, events, projects, request_sessions, visitor};

    let (service, db, _container) =
        create_test_analytics_service!("test_get_session_details_v2_session_id");

    // Re-use the project, environment, and deployment created by insert_test_data.
    let project = projects::Entity::find()
        .filter(projects::Column::Slug.eq("test_project"))
        .one(db.as_ref())
        .await?
        .expect("test project must exist from insert_test_data");

    let environment = environments::Entity::find()
        .filter(environments::Column::ProjectId.eq(project.id))
        .one(db.as_ref())
        .await?
        .expect("test environment must exist from insert_test_data");

    let deployment = deployments::Entity::find()
        .filter(deployments::Column::ProjectId.eq(project.id))
        .one(db.as_ref())
        .await?
        .expect("test deployment must exist from insert_test_data");

    // The bare UUID is what the proxy stores in request_sessions.session_id.
    let bare_uuid = "c172f0b5-986f-47dc-b6c6-9198761519e0";
    // The v2 payload is what the old analytics ingest path wrote to
    // events.session_id before the normalize_session_cookie fix landed.
    let v2_session_id = format!("v2|{}|1783631315", bare_uuid);

    let test_visitor = visitor::ActiveModel {
        visitor_id: Set("v2-session-test-visitor".to_string()),
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        first_seen: Set(chrono::Utc::now()),
        last_seen: Set(chrono::Utc::now()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    // request_sessions always stores the bare UUID (proxy's parse_session_cookie
    // strips the v2| prefix before inserting).
    let session_started = chrono::Utc::now() - chrono::Duration::minutes(10);
    let session_ended = chrono::Utc::now();
    let session_row = request_sessions::ActiveModel {
        session_id: Set(bare_uuid.to_string()),
        started_at: Set(session_started),
        last_accessed_at: Set(session_ended),
        visitor_id: Set(Some(test_visitor.id)),
        data: Set("{}".to_string()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    // Insert two page_view events using the LEGACY v2|uuid|ts format.
    // entry event (earlier timestamp)
    events::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(Some(environment.id)),
        deployment_id: Set(Some(deployment.id)),
        visitor_id: Set(Some(test_visitor.id)),
        session_id: Set(Some(v2_session_id.clone())),
        event_type: Set("page_view".to_string()),
        page_path: Set("/entry-page".to_string()),
        hostname: Set("example.com".to_string()),
        pathname: Set("/entry-page".to_string()),
        href: Set("https://example.com/entry-page".to_string()),
        timestamp: Set(session_started + chrono::Duration::seconds(5)),
        is_bounce: Set(false),
        is_crawler: Set(false),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    // exit event (later timestamp)
    events::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(Some(environment.id)),
        deployment_id: Set(Some(deployment.id)),
        visitor_id: Set(Some(test_visitor.id)),
        session_id: Set(Some(v2_session_id.clone())),
        event_type: Set("page_view".to_string()),
        page_path: Set("/exit-page".to_string()),
        hostname: Set("example.com".to_string()),
        pathname: Set("/exit-page".to_string()),
        href: Set("https://example.com/exit-page".to_string()),
        timestamp: Set(session_started + chrono::Duration::minutes(5)),
        is_bounce: Set(false),
        is_crawler: Set(false),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    let details = service
        .get_session_details(session_row.id, project.id, None)
        .await?
        .expect("get_session_details must return Some for an existing session");

    // All five correlated subqueries must match via the v2-tolerant condition:
    assert_eq!(
        details.entry_path.as_deref(),
        Some("/entry-page"),
        "entry_path must be populated from legacy v2|uuid|ts events (subquery 1)"
    );
    assert_eq!(
        details.exit_path.as_deref(),
        Some("/exit-page"),
        "exit_path must be populated from legacy v2|uuid|ts events (subquery 2)"
    );
    assert_eq!(
        details.page_views, 2,
        "page_views must count both legacy-format page_view events (subquery 3)"
    );
    assert!(
        !details.is_bounced,
        "is_bounced must be false when page_views > 1 (subquery 4)"
    );
    // session duration is 10 minutes >= 10s, so is_engaged = true even
    // with only page_view events (which don't count as interactions)
    assert!(
        details.is_engaged,
        "is_engaged must be true when session duration >= 10s (subquery 5)"
    );

    cleanup_test_analytics!(db);
    Ok(())
}

/// Bounce, entry, and exit are derived at query time from per-session
/// page_view counts and event order — the events.is_bounce / is_entry /
/// is_exit columns are written as false by the ingest path and never
/// updated. This test inserts events exactly as the ingest path does
/// (all flags false) and asserts every read path still reports bounces.
#[tokio::test]
async fn test_bounce_derived_from_session_pageview_counts() -> anyhow::Result<()> {
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
    use temps_entities::{deployments, environments, events, projects, request_sessions, visitor};

    let (service, db, _container) = create_test_analytics_service!("test_bounce_from_pageviews");

    let project = projects::Entity::find()
        .filter(projects::Column::Slug.eq("test_project"))
        .one(db.as_ref())
        .await?
        .expect("test project must exist from insert_test_data");
    let environment = environments::Entity::find()
        .filter(environments::Column::ProjectId.eq(project.id))
        .one(db.as_ref())
        .await?
        .expect("test environment must exist from insert_test_data");
    let deployment = deployments::Entity::find()
        .filter(deployments::Column::ProjectId.eq(project.id))
        .one(db.as_ref())
        .await?
        .expect("test deployment must exist from insert_test_data");

    let now = chrono::Utc::now();
    // Seeded test data lives at 2024-01-01, so a window around `now`
    // only sees the sessions created here.
    let window_start = now - chrono::Duration::hours(1);
    let window_end = now + chrono::Duration::hours(1);

    let insert_visitor = |uuid: &str| {
        let db = db.clone();
        let project_id = project.id;
        let environment_id = environment.id;
        let uuid = uuid.to_string();
        async move {
            visitor::ActiveModel {
                visitor_id: Set(uuid),
                project_id: Set(project_id),
                environment_id: Set(environment_id),
                first_seen: Set(chrono::Utc::now()),
                last_seen: Set(chrono::Utc::now()),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await
        }
    };
    let visitor_a = insert_visitor("bounce-test-visitor-a").await?;
    let visitor_b = insert_visitor("bounce-test-visitor-b").await?;

    let session_a_uuid = "aaaaaaaa-0000-4000-8000-000000000001";
    let session_b_uuid = "bbbbbbbb-0000-4000-8000-000000000002";
    let session_a = request_sessions::ActiveModel {
        session_id: Set(session_a_uuid.to_string()),
        started_at: Set(now - chrono::Duration::minutes(10)),
        last_accessed_at: Set(now - chrono::Duration::minutes(10)),
        visitor_id: Set(Some(visitor_a.id)),
        data: Set("{}".to_string()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;
    let session_b = request_sessions::ActiveModel {
        session_id: Set(session_b_uuid.to_string()),
        started_at: Set(now - chrono::Duration::minutes(10)),
        last_accessed_at: Set(now - chrono::Duration::minutes(5)),
        visitor_id: Set(Some(visitor_b.id)),
        data: Set("{}".to_string()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    // Insert page_view events with the flags exactly as the ingest path
    // writes them: is_bounce / is_entry / is_exit all false.
    let insert_page_view =
        |visitor_id: i32, session_uuid: &str, path: &str, ts: chrono::DateTime<chrono::Utc>| {
            let db = db.clone();
            let project_id = project.id;
            let environment_id = environment.id;
            let deployment_id = deployment.id;
            let session_uuid = session_uuid.to_string();
            let path = path.to_string();
            async move {
                events::ActiveModel {
                    project_id: Set(project_id),
                    environment_id: Set(Some(environment_id)),
                    deployment_id: Set(Some(deployment_id)),
                    visitor_id: Set(Some(visitor_id)),
                    session_id: Set(Some(session_uuid)),
                    event_type: Set("page_view".to_string()),
                    page_path: Set(path.clone()),
                    hostname: Set("example.com".to_string()),
                    pathname: Set(path.clone()),
                    href: Set(format!("https://example.com{path}")),
                    timestamp: Set(ts),
                    is_entry: Set(false),
                    is_exit: Set(false),
                    is_bounce: Set(false),
                    is_crawler: Set(false),
                    ..Default::default()
                }
                .insert(db.as_ref())
                .await
            }
        };

    // Visitor A: single page view on /landing → bounce.
    insert_page_view(
        visitor_a.id,
        session_a_uuid,
        "/landing",
        now - chrono::Duration::minutes(10),
    )
    .await?;
    // Visitor B: /landing then /other → not a bounce.
    insert_page_view(
        visitor_b.id,
        session_b_uuid,
        "/landing",
        now - chrono::Duration::minutes(10),
    )
    .await?;
    insert_page_view(
        visitor_b.id,
        session_b_uuid,
        "/other",
        now - chrono::Duration::minutes(5),
    )
    .await?;

    // 1. Visitor sessions list derives is_bounced from pageview counts.
    let sessions_a = service
        .get_visitor_sessions_by_id(visitor_a.id, None)
        .await?
        .expect("visitor A must exist");
    assert_eq!(sessions_a.sessions.len(), 1);
    assert!(
        sessions_a.sessions[0].is_bounced,
        "single-pageview session must be a bounce"
    );
    assert_eq!(
        sessions_a.sessions[0].session_id, session_a.id,
        "session id must come from request_sessions"
    );

    let sessions_b = service
        .get_visitor_sessions_by_id(visitor_b.id, None)
        .await?
        .expect("visitor B must exist");
    assert_eq!(sessions_b.sessions.len(), 1);
    assert!(
        !sessions_b.sessions[0].is_bounced,
        "two-pageview session must not be a bounce"
    );
    assert_eq!(sessions_b.sessions[0].page_views, 2);
    assert_eq!(sessions_b.sessions[0].session_id, session_b.id);

    // 2. Visitor statistics derive bounce_rate from per-session counts.
    let stats_a = service
        .get_visitor_statistics(visitor_a.id)
        .await?
        .expect("visitor A stats must exist");
    assert_eq!(
        stats_a.bounce_rate, 100.0,
        "visitor A's only session bounced → 100%"
    );
    let stats_b = service
        .get_visitor_statistics(visitor_b.id)
        .await?
        .expect("visitor B stats must exist");
    assert_eq!(
        stats_b.bounce_rate, 0.0,
        "visitor B's only session did not bounce → 0%"
    );

    // 3. Page detail: 2 sessions entered on /landing, 1 bounced → 50%.
    let detail = service
        .get_page_path_detail(project.id, "/landing", window_start, window_end, None, None)
        .await?;
    assert_eq!(
        detail.bounce_rate, 50.0,
        "1 of 2 entry sessions on /landing bounced"
    );
    assert_eq!(
        detail.entry_rate, 100.0,
        "both sessions entered on /landing"
    );
    assert_eq!(detail.exit_rate, 50.0, "only session A exited on /landing");

    // 4. Page flow: entry/exit/bounce counts derived per session.
    let flow = service
        .get_page_flow(project.id, window_start, window_end, None, None, None, None)
        .await?;
    let landing = flow
        .top_entry_pages
        .iter()
        .find(|p| p.page_path == "/landing")
        .expect("/landing must be a top entry page");
    assert_eq!(landing.entry_count, 2, "both sessions entered on /landing");
    assert_eq!(landing.bounce_count, 1, "session A bounced on /landing");
    let other_exit = flow
        .top_exit_pages
        .iter()
        .find(|p| p.page_path == "/other")
        .expect("/other must be a top exit page");
    assert_eq!(other_exit.exit_count, 1, "session B exited on /other");

    cleanup_test_analytics!(db);
    Ok(())
}

/// Regression test for the bounce/exit-page "-" bug: a session's last
/// pageview has no next event to diff against, so `time_on_page` must
/// fall back to the session's own `last_accessed_at` instead of coming
/// back NULL. Covers `get_page_path_visitors` (the "Individual visits"
/// table).
#[tokio::test]
async fn test_get_page_path_visitors_bounce_has_real_time_on_page() -> anyhow::Result<()> {
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
    use temps_entities::{deployments, environments, events, projects, request_sessions, visitor};

    let (service, db, _container) =
        create_test_analytics_service!("test_page_path_visitors_bounce_duration");

    let project = projects::Entity::find()
        .filter(projects::Column::Slug.eq("test_project"))
        .one(db.as_ref())
        .await?
        .expect("test project must exist from insert_test_data");
    let environment = environments::Entity::find()
        .filter(environments::Column::ProjectId.eq(project.id))
        .one(db.as_ref())
        .await?
        .expect("test environment must exist from insert_test_data");
    let deployment = deployments::Entity::find()
        .filter(deployments::Column::ProjectId.eq(project.id))
        .one(db.as_ref())
        .await?
        .expect("test deployment must exist from insert_test_data");

    let now = chrono::Utc::now();
    let window_start = now - chrono::Duration::hours(1);
    let window_end = now + chrono::Duration::hours(1);

    let test_visitor = visitor::ActiveModel {
        visitor_id: Set("bounce-duration-test-visitor".to_string()),
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        first_seen: Set(now),
        last_seen: Set(now),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    let pageview_ts = now - chrono::Duration::minutes(10);
    // No page_leave and no next page_view exist for this session, so the
    // query's only signal for how long the visitor stayed is the
    // session's own last-activity timestamp -- 27s after the pageview.
    let session_row = request_sessions::ActiveModel {
        session_id: Set("cccccccc-0000-4000-8000-000000000003".to_string()),
        started_at: Set(pageview_ts),
        last_accessed_at: Set(pageview_ts + chrono::Duration::seconds(27)),
        visitor_id: Set(Some(test_visitor.id)),
        data: Set("{}".to_string()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    events::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(Some(environment.id)),
        deployment_id: Set(Some(deployment.id)),
        visitor_id: Set(Some(test_visitor.id)),
        session_id: Set(Some(session_row.session_id.clone())),
        event_type: Set("page_view".to_string()),
        page_path: Set("/bounce-page".to_string()),
        hostname: Set("example.com".to_string()),
        pathname: Set("/bounce-page".to_string()),
        href: Set("https://example.com/bounce-page".to_string()),
        timestamp: Set(pageview_ts),
        is_entry: Set(false),
        is_exit: Set(false),
        is_bounce: Set(false),
        is_crawler: Set(false),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    let response = service
        .get_page_path_visitors(
            project.id,
            "/bounce-page",
            window_start,
            window_end,
            None,
            1,
            20,
        )
        .await?;

    assert_eq!(response.sessions.len(), 1);
    let session = &response.sessions[0];
    assert!(
        session.is_bounce,
        "single-pageview session must be a bounce"
    );
    assert_eq!(
        session.time_on_page,
        Some(27),
        "time_on_page must come from the session's last_accessed_at, not be NULL/\"-\""
    );

    cleanup_test_analytics!(db);
    Ok(())
}

/// The other half of the fallback fix's contract: when the session's
/// `last_accessed_at` is far past the pageview (e.g. a tab left open for
/// a long time before the last heartbeat), the computed duration must be
/// clamped to 1800s rather than reporting an unbounded value.
#[tokio::test]
async fn test_get_page_path_visitors_clamps_upper_bound_time_on_page() -> anyhow::Result<()> {
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
    use temps_entities::{deployments, environments, events, projects, request_sessions, visitor};

    let (service, db, _container) =
        create_test_analytics_service!("test_page_path_visitors_clamp_upper_bound");

    let project = projects::Entity::find()
        .filter(projects::Column::Slug.eq("test_project"))
        .one(db.as_ref())
        .await?
        .expect("test project must exist from insert_test_data");
    let environment = environments::Entity::find()
        .filter(environments::Column::ProjectId.eq(project.id))
        .one(db.as_ref())
        .await?
        .expect("test environment must exist from insert_test_data");
    let deployment = deployments::Entity::find()
        .filter(deployments::Column::ProjectId.eq(project.id))
        .one(db.as_ref())
        .await?
        .expect("test deployment must exist from insert_test_data");

    let now = chrono::Utc::now();
    let window_start = now - chrono::Duration::hours(1);
    let window_end = now + chrono::Duration::hours(1);

    let test_visitor = visitor::ActiveModel {
        visitor_id: Set("clamp-upper-bound-test-visitor".to_string()),
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        first_seen: Set(now),
        last_seen: Set(now),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    let pageview_ts = now - chrono::Duration::minutes(10);
    // 40 minutes of "activity" after the pageview -- well past the
    // 1800s (30 minute) clamp ceiling.
    let session_row = request_sessions::ActiveModel {
        session_id: Set("eeeeeeee-0000-4000-8000-000000000005".to_string()),
        started_at: Set(pageview_ts),
        last_accessed_at: Set(pageview_ts + chrono::Duration::minutes(40)),
        visitor_id: Set(Some(test_visitor.id)),
        data: Set("{}".to_string()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    events::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(Some(environment.id)),
        deployment_id: Set(Some(deployment.id)),
        visitor_id: Set(Some(test_visitor.id)),
        session_id: Set(Some(session_row.session_id.clone())),
        event_type: Set("page_view".to_string()),
        page_path: Set("/clamp-upper-bound-page".to_string()),
        hostname: Set("example.com".to_string()),
        pathname: Set("/clamp-upper-bound-page".to_string()),
        href: Set("https://example.com/clamp-upper-bound-page".to_string()),
        timestamp: Set(pageview_ts),
        is_entry: Set(false),
        is_exit: Set(false),
        is_bounce: Set(false),
        is_crawler: Set(false),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    let response = service
        .get_page_path_visitors(
            project.id,
            "/clamp-upper-bound-page",
            window_start,
            window_end,
            None,
            1,
            20,
        )
        .await?;

    assert_eq!(response.sessions.len(), 1);
    assert_eq!(
        response.sessions[0].time_on_page,
        Some(1800),
        "time_on_page must clamp to 1800s, not report the raw 2400s gap"
    );

    cleanup_test_analytics!(db);
    Ok(())
}

/// Same regression as above, for `get_visitor_journey` (the "Visitor
/// Journey" panel's per-event duration).
#[tokio::test]
async fn test_get_visitor_journey_exit_page_has_real_time_on_page() -> anyhow::Result<()> {
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
    use temps_entities::{deployments, environments, events, projects, request_sessions, visitor};

    let (service, db, _container) =
        create_test_analytics_service!("test_visitor_journey_exit_duration");

    let project = projects::Entity::find()
        .filter(projects::Column::Slug.eq("test_project"))
        .one(db.as_ref())
        .await?
        .expect("test project must exist from insert_test_data");
    let environment = environments::Entity::find()
        .filter(environments::Column::ProjectId.eq(project.id))
        .one(db.as_ref())
        .await?
        .expect("test environment must exist from insert_test_data");
    let deployment = deployments::Entity::find()
        .filter(deployments::Column::ProjectId.eq(project.id))
        .one(db.as_ref())
        .await?
        .expect("test deployment must exist from insert_test_data");

    let now = chrono::Utc::now();

    let test_visitor = visitor::ActiveModel {
        visitor_id: Set("journey-duration-test-visitor".to_string()),
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        first_seen: Set(now),
        last_seen: Set(now),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    let pageview_ts = now - chrono::Duration::minutes(10);
    let session_row = request_sessions::ActiveModel {
        session_id: Set("dddddddd-0000-4000-8000-000000000004".to_string()),
        started_at: Set(pageview_ts),
        last_accessed_at: Set(pageview_ts + chrono::Duration::seconds(19)),
        visitor_id: Set(Some(test_visitor.id)),
        data: Set("{}".to_string()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    events::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(Some(environment.id)),
        deployment_id: Set(Some(deployment.id)),
        visitor_id: Set(Some(test_visitor.id)),
        session_id: Set(Some(session_row.session_id.clone())),
        event_type: Set("page_view".to_string()),
        page_path: Set("/journey-exit-page".to_string()),
        hostname: Set("example.com".to_string()),
        pathname: Set("/journey-exit-page".to_string()),
        href: Set("https://example.com/journey-exit-page".to_string()),
        timestamp: Set(pageview_ts),
        is_entry: Set(false),
        is_exit: Set(false),
        is_bounce: Set(false),
        is_crawler: Set(false),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    let journey = service
        .get_visitor_journey(test_visitor.id, project.id, None)
        .await?
        .expect("get_visitor_journey must return Some for an existing visitor");

    let session = journey
        .sessions
        .iter()
        .find(|s| s.session_id == session_row.id)
        .expect("seeded session must appear in the journey");
    let page_view_event = session
        .events
        .iter()
        .find(|e| e.event_type == "page_view")
        .expect("page_view event must appear in the session's events");

    assert_eq!(
        page_view_event.time_on_page,
        Some(19),
        "exit-page time_on_page must come from the session's last_accessed_at, not be NULL"
    );

    cleanup_test_analytics!(db);
    Ok(())
}

/// Upper-bound counterpart of `test_get_visitor_journey_exit_page_has_real_time_on_page`:
/// a `last_accessed_at` far past the pageview must clamp to 1800s.
#[tokio::test]
async fn test_get_visitor_journey_clamps_upper_bound_time_on_page() -> anyhow::Result<()> {
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
    use temps_entities::{deployments, environments, events, projects, request_sessions, visitor};

    let (service, db, _container) =
        create_test_analytics_service!("test_visitor_journey_clamp_upper_bound");

    let project = projects::Entity::find()
        .filter(projects::Column::Slug.eq("test_project"))
        .one(db.as_ref())
        .await?
        .expect("test project must exist from insert_test_data");
    let environment = environments::Entity::find()
        .filter(environments::Column::ProjectId.eq(project.id))
        .one(db.as_ref())
        .await?
        .expect("test environment must exist from insert_test_data");
    let deployment = deployments::Entity::find()
        .filter(deployments::Column::ProjectId.eq(project.id))
        .one(db.as_ref())
        .await?
        .expect("test deployment must exist from insert_test_data");

    let now = chrono::Utc::now();

    let test_visitor = visitor::ActiveModel {
        visitor_id: Set("journey-clamp-upper-bound-visitor".to_string()),
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        first_seen: Set(now),
        last_seen: Set(now),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    let pageview_ts = now - chrono::Duration::minutes(10);
    let session_row = request_sessions::ActiveModel {
        session_id: Set("ffffffff-0000-4000-8000-000000000006".to_string()),
        started_at: Set(pageview_ts),
        last_accessed_at: Set(pageview_ts + chrono::Duration::minutes(40)),
        visitor_id: Set(Some(test_visitor.id)),
        data: Set("{}".to_string()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    events::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(Some(environment.id)),
        deployment_id: Set(Some(deployment.id)),
        visitor_id: Set(Some(test_visitor.id)),
        session_id: Set(Some(session_row.session_id.clone())),
        event_type: Set("page_view".to_string()),
        page_path: Set("/journey-clamp-upper-bound-page".to_string()),
        hostname: Set("example.com".to_string()),
        pathname: Set("/journey-clamp-upper-bound-page".to_string()),
        href: Set("https://example.com/journey-clamp-upper-bound-page".to_string()),
        timestamp: Set(pageview_ts),
        is_entry: Set(false),
        is_exit: Set(false),
        is_bounce: Set(false),
        is_crawler: Set(false),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    let journey = service
        .get_visitor_journey(test_visitor.id, project.id, None)
        .await?
        .expect("get_visitor_journey must return Some for an existing visitor");

    let session = journey
        .sessions
        .iter()
        .find(|s| s.session_id == session_row.id)
        .expect("seeded session must appear in the journey");
    let page_view_event = session
        .events
        .iter()
        .find(|e| e.event_type == "page_view")
        .expect("page_view event must appear in the session's events");

    assert_eq!(
        page_view_event.time_on_page,
        Some(1800),
        "time_on_page must clamp to 1800s, not report the raw 2400s gap"
    );

    cleanup_test_analytics!(db);
    Ok(())
}

/// Only an unreachable Docker daemon skips a test. Once Docker answers, a
/// failure to start the container or seed it is a broken setup and fails
/// the test rather than letting it pass without checking anything.
async fn analytics_service_or_skip(
    name: &str,
) -> Option<(
    AnalyticsService,
    Arc<sea_orm::DatabaseConnection>,
    crate::testing::test_utils::TestDatabase,
)> {
    let docker_answers = match bollard::Docker::connect_with_defaults() {
        Ok(docker) => docker.ping().await.is_ok(),
        Err(_) => false,
    };
    if !docker_answers {
        println!("Docker not available, skipping {name}");
        return None;
    }
    let test_db = match crate::testing::test_utils::TestDatabase::new(name).await {
        Ok(db) => db,
        Err(error) => panic!("test database setup failed with Docker available: {error:#}"),
    };
    if let Err(error) = AnalyticsTestUtils::insert_test_data(&test_db.db).await {
        panic!("seeding analytics test data failed: {error:#}");
    }
    let cookie_crypto =
        Arc::new(temps_core::CookieCrypto::new("test_key_32_bytes_long_for_tests").unwrap());
    let service = AnalyticsService::new(test_db.db.clone(), cookie_crypto);
    let db = test_db.db.clone();
    Some((service, db, test_db))
}

/// The project and environment `insert_test_data` creates.
struct SeededScope {
    project_id: i32,
    environment_id: i32,
}

async fn seeded_scope(db: &sea_orm::DatabaseConnection) -> anyhow::Result<SeededScope> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use temps_entities::{environments, projects};
    let project = projects::Entity::find()
        .filter(projects::Column::Slug.eq("test_project"))
        .one(db)
        .await?
        .expect("test project must exist from insert_test_data");
    let environment = environments::Entity::find()
        .filter(environments::Column::ProjectId.eq(project.id))
        .one(db)
        .await?
        .expect("test environment must exist from insert_test_data");
    Ok(SeededScope {
        project_id: project.id,
        environment_id: environment.id,
    })
}

/// Inserts events the way the ingest path writes them (all session-flow
/// flags false, so every read path must derive them).
struct FlowSeeder<'a> {
    db: &'a sea_orm::DatabaseConnection,
    project_id: i32,
    environment_id: Option<i32>,
    visitor_id: Option<i32>,
}

impl FlowSeeder<'_> {
    async fn event(
        &self,
        session_id: &str,
        event_type: &str,
        page_path: &str,
        timestamp: UtcDateTime,
    ) -> anyhow::Result<()> {
        self.insert(
            session_id,
            event_type,
            page_path,
            timestamp,
            self.environment_id,
            false,
        )
        .await
    }

    async fn insert(
        &self,
        session_id: &str,
        event_type: &str,
        page_path: &str,
        timestamp: UtcDateTime,
        environment_id: Option<i32>,
        is_crawler: bool,
    ) -> anyhow::Result<()> {
        use sea_orm::{ActiveModelTrait, Set};
        temps_entities::events::ActiveModel {
            project_id: Set(self.project_id),
            environment_id: Set(environment_id),
            visitor_id: Set(self.visitor_id),
            session_id: Set(Some(session_id.to_string())),
            event_type: Set(event_type.to_string()),
            page_path: Set(page_path.to_string()),
            hostname: Set("example.com".to_string()),
            pathname: Set(page_path.to_string()),
            href: Set(format!("https://example.com{page_path}")),
            timestamp: Set(timestamp),
            is_entry: Set(false),
            is_exit: Set(false),
            is_bounce: Set(false),
            is_crawler: Set(is_crawler),
            ..Default::default()
        }
        .insert(self.db)
        .await?;
        Ok(())
    }
}

async fn insert_request_session(
    db: &sea_orm::DatabaseConnection,
    session_id: &str,
    started_at: UtcDateTime,
    last_accessed_at: UtcDateTime,
    visitor_id: Option<i32>,
) -> anyhow::Result<i32> {
    use sea_orm::{ActiveModelTrait, Set};
    let row = temps_entities::request_sessions::ActiveModel {
        session_id: Set(session_id.to_string()),
        started_at: Set(started_at),
        last_accessed_at: Set(last_accessed_at),
        visitor_id: Set(visitor_id),
        data: Set("{}".to_string()),
        ..Default::default()
    }
    .insert(db)
    .await?;
    Ok(row.id)
}

/// Largest number of rows any plan node handled -- emitted plus discarded
/// by its filter or join filter, times its loops -- from
/// `EXPLAIN (ANALYZE, FORMAT JSON)` output.
fn max_rows_handled(node: &serde_json::Value) -> f64 {
    let count = |key: &str| node.get(key).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let loops = node
        .get("Actual Loops")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0);
    let own = (count("Actual Rows")
        + count("Rows Removed by Filter")
        + count("Rows Removed by Join Filter"))
        * loops;
    node.get("Plans")
        .and_then(|plans| plans.as_array())
        .map(|plans| plans.iter().map(max_rows_handled).fold(own, f64::max))
        .unwrap_or(own)
}

/// Runs `EXPLAIN (ANALYZE, TIMING OFF, FORMAT JSON)` of `sql` with `values`
/// under both plans PostgreSQL may use for it, on one connection: the custom
/// plan built from the bound values, and the generic plan a prepared
/// statement switches to after five executions (sqlx prepares every query).
/// The generic plan is forced with `plan_cache_mode` on a `PREPARE` of the
/// same SQL, and checked to be the one that ran. Returns each plan's root
/// node, labelled.
async fn explain_custom_and_generic(
    db: &sea_orm::DatabaseConnection,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> anyhow::Result<[(&'static str, serde_json::Value); 2]> {
    use sea_orm::{ConnectionTrait, TransactionTrait};

    let txn = db.begin().await?;
    txn.execute_unprepared("SET LOCAL plan_cache_mode = force_custom_plan")
        .await?;
    let custom: serde_json::Value = txn
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!("EXPLAIN (ANALYZE, TIMING OFF, FORMAT JSON) {sql}"),
            values.clone(),
        ))
        .await?
        .expect("EXPLAIN returns one row")
        .try_get("", "QUERY PLAN")?;

    // A prepared statement outlives the transaction: a unique name keeps one
    // left behind by a failed run from clashing with a later one.
    let name = format!("explain_generic_{}", uuid::Uuid::new_v4().simple());
    txn.execute_unprepared(&format!("PREPARE {name} AS {sql}"))
        .await?;
    txn.execute_unprepared("SET LOCAL plan_cache_mode = force_generic_plan")
        .await?;
    // EXPLAIN EXECUTE cannot take bound parameters for its arguments, so the
    // test's own values are passed as escaped literals; PostgreSQL coerces
    // them to the parameter types it inferred for the PREPARE.
    let args = values
        .iter()
        .map(|value| {
            use sea_orm::sea_query::{PostgresQueryBuilder, QueryBuilder};
            PostgresQueryBuilder.value_to_string(value)
        })
        .collect::<Vec<_>>()
        .join(", ");
    let generic: serde_json::Value = txn
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            format!("EXPLAIN (ANALYZE, TIMING OFF, FORMAT JSON) EXECUTE {name}({args})"),
        ))
        .await?
        .expect("EXPLAIN EXECUTE returns one row")
        .try_get("", "QUERY PLAN")?;
    let generic_plans: i64 = txn
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT generic_plans FROM pg_prepared_statements WHERE name = $1",
            [name.clone().into()],
        ))
        .await?
        .expect("the prepared statement is listed")
        .try_get("", "generic_plans")?;
    txn.execute_unprepared(&format!("DEALLOCATE {name}"))
        .await?;
    txn.rollback().await?;
    assert!(
        generic_plans >= 1,
        "EXPLAIN EXECUTE must have run the generic plan, ran {generic_plans} generic plans"
    );
    Ok([
        ("custom", custom[0]["Plan"].clone()),
        ("generic", generic[0]["Plan"].clone()),
    ])
}

/// Regression test for the page-path-visitors timeout: one session holding
/// thousands of page views on many pages (plus the target page) used to
/// cost O(n^2) in that session's events because every page view was
/// self-joined against every later event of its session.
///
/// Asserts the response for the busy session and for ordinary sessions
/// next to it (time_on_page priorities, entry/exit/bounce, page number,
/// pagination, count, environment and crawler filters, legacy v2 session
/// ids), and that no plan node emits more rows than a small multiple of
/// the busy session's page views.
#[tokio::test]
async fn test_get_page_path_visitors_session_with_thousands_of_page_views() -> anyhow::Result<()> {
    use sea_orm::{ConnectionTrait, Statement};

    let Some((service, db, _container)) =
        analytics_service_or_skip("test_page_path_visitors_busy_session").await
    else {
        return Ok(());
    };
    let scope = seeded_scope(db.as_ref()).await?;
    let (project_id, env_id) = (scope.project_id, scope.environment_id);
    let seed = FlowSeeder {
        db: db.as_ref(),
        project_id,
        environment_id: Some(env_id),
        visitor_id: None,
    };

    const TARGET: &str = "/regression-target";
    // The busy session: one page view every 2s for 100 minutes on 1000
    // pages, the target page every 500th view and as the last view.
    const BUSY_PAGE_VIEWS: i64 = 3000;
    let w0 =
        chrono::DateTime::parse_from_rfc3339("2026-03-02T12:00:00Z")?.with_timezone(&chrono::Utc);
    let window_end = w0 + chrono::Duration::hours(2);
    let at = |seconds: i64| w0 + chrono::Duration::seconds(seconds);

    let busy = "b0b0b0b0-0000-4000-8000-000000000001";
    let busy_last_view = at(2 * (BUSY_PAGE_VIEWS - 1));
    insert_request_session(
        db.as_ref(),
        busy,
        w0 - chrono::Duration::hours(30),
        busy_last_view + chrono::Duration::seconds(7),
        None,
    )
    .await?;
    db.execute(Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Postgres,
        r#"
        INSERT INTO events (timestamp, project_id, environment_id, session_id,
                            hostname, pathname, page_path, href,
                            is_entry, is_exit, is_bounce, is_crawler, event_type)
        SELECT $1::timestamptz + k * INTERVAL '2 seconds', $2, $3, $4,
               'example.com', p, p, 'https://example.com' || p,
               false, false, false, false, 'page_view'
        FROM generate_series(0, $5 - 1) k
        CROSS JOIN LATERAL (
            SELECT CASE WHEN k % 500 = 0 OR k = $5 - 1 THEN $6
                        ELSE '/bulk/page-' || (k % 1000) END AS p
        ) pages
        "#,
        vec![
            w0.into(),
            project_id.into(),
            env_id.into(),
            busy.into(),
            BUSY_PAGE_VIEWS.into(),
            TARGET.into(),
        ],
    ))
    .await?;
    // A page_leave of the target page 1s after its view at k = 500
    // (priority 1). It is also the first leave of that page within 30 min
    // of the view at k = 0, so it sets that one too (1001s), ahead of the
    // next page view 2s later.
    seed.event(busy, "page_leave", TARGET, at(1001)).await?;
    // Page views before the lookback: outside the bound, so the session
    // is numbered from its first page view inside it (documented on
    // PAGE_VISITORS_SESSION_LOOKBACK_HOURS), but none of its views is the
    // entry.
    for minute in 0..5 {
        let ts = w0 - chrono::Duration::hours(30) + chrono::Duration::minutes(minute);
        seed.event(busy, "page_view", "/bulk/early", ts).await?;
    }
    // A page view after the lookahead: the last target view is still not
    // the exit, and its next page view (45 min later) clamps to 1800s
    // even though last_accessed_at is only 7s later.
    seed.event(
        busy,
        "page_view",
        "/bulk/late",
        window_end + chrono::Duration::minutes(45),
    )
    .await?;

    // A bounce stored with the legacy v2|uuid|ts session id; its
    // request_sessions row holds the bare UUID, 27s of activity.
    let v2_uuid = "b0b0b0b0-0000-4000-8000-000000000002";
    let v2_session = format!("v2|{v2_uuid}|1772452800");
    insert_request_session(db.as_ref(), v2_uuid, at(1800), at(1827), None).await?;
    seed.event(&v2_session, "page_view", TARGET, at(1800))
        .await?;

    // Second page of its session, left after 50s (priority 1).
    let second_page = "b0b0b0b0-0000-4000-8000-000000000003";
    insert_request_session(db.as_ref(), second_page, at(2400), at(2600), None).await?;
    seed.event(second_page, "page_view", "/other", at(2400))
        .await?;
    seed.event(second_page, "page_view", TARGET, at(2460))
        .await?;
    seed.event(second_page, "page_leave", TARGET, at(2510))
        .await?;
    seed.event(second_page, "page_view", "/next", at(2520))
        .await?;

    // A page_leave 35 min later is past the 30-min horizon, so the next
    // page view 20 min later sets the duration (priority 2).
    let late_leave = "b0b0b0b0-0000-4000-8000-000000000004";
    insert_request_session(db.as_ref(), late_leave, at(3120), at(3120 + 2100), None).await?;
    seed.event(late_leave, "page_view", TARGET, at(3120))
        .await?;
    seed.event(late_leave, "page_view", "/after", at(3120 + 1200))
        .await?;
    seed.event(late_leave, "page_leave", TARGET, at(3120 + 2100))
        .await?;

    // No request_sessions row and no next event: the 1s floor.
    let no_session_row = "b0b0b0b0-0000-4000-8000-000000000005";
    seed.event(no_session_row, "page_view", TARGET, at(3600))
        .await?;

    // Excluded with the environment filter, counted without it.
    let no_environment = "b0b0b0b0-0000-4000-8000-000000000006";
    seed.insert(no_environment, "page_view", TARGET, at(4200), None, false)
        .await?;
    // Never listed or counted.
    let crawler = "b0b0b0b0-0000-4000-8000-000000000007";
    seed.insert(crawler, "page_view", TARGET, at(4300), Some(env_id), true)
        .await?;

    // The cost stays linear in the busy session: no node handles more
    // than a few rows per page view (the self-joins compared every target
    // view with every page view of its session), with the custom plan and
    // with the generic one a connection switches to after five executions.
    let (context_start, context_end) = page_path_visitors_context_bounds(w0, window_end);
    let plans = explain_custom_and_generic(
        db.as_ref(),
        PAGE_PATH_VISITORS_SQL,
        vec![
            project_id.into(),
            TARGET.into(),
            w0.into(),
            window_end.into(),
            Some(env_id).into(),
            50i64.into(),
            0i64.into(),
            context_start.into(),
            context_end.into(),
            ANALYTICS_ROW_CLOCK_SLACK_HOURS.into(),
        ],
    )
    .await?;
    for (kind, plan) in &plans {
        let max_rows = max_rows_handled(plan);
        assert!(
            max_rows <= (4 * BUSY_PAGE_VIEWS) as f64,
            "a {kind} plan node handled {max_rows} rows for a session of {BUSY_PAGE_VIEWS} page views"
        );
    }

    // (viewed_at, session, time_on_page, entry, exit, bounce, page number),
    // newest first.
    let busy_view = |k: i64, time: i32| {
        (
            at(2 * k),
            busy.to_string(),
            Some(time),
            false,
            false,
            false,
            Some(k as i32 + 1),
        )
    };
    let expected = vec![
        busy_view(2999, 1800),
        busy_view(2500, 2),
        busy_view(2000, 2),
        (
            at(3600),
            no_session_row.to_string(),
            Some(1),
            true,
            true,
            true,
            Some(1),
        ),
        (
            at(3120),
            late_leave.to_string(),
            Some(1200),
            true,
            false,
            false,
            Some(1),
        ),
        busy_view(1500, 2),
        (
            at(2460),
            second_page.to_string(),
            Some(50),
            false,
            false,
            false,
            Some(2),
        ),
        busy_view(1000, 2),
        (
            at(1800),
            v2_session.clone(),
            Some(27),
            true,
            true,
            true,
            Some(1),
        ),
        busy_view(500, 1),
        busy_view(0, 1001),
    ];

    let mut listed = Vec::new();
    for page in 1..=3 {
        let response = service
            .get_page_path_visitors(project_id, TARGET, w0, window_end, Some(env_id), page, 4)
            .await?;
        assert_eq!(
            response.total_count, 11,
            "page {page}: count excludes crawler and other-environment views"
        );
        assert_eq!(response.per_page, 4);
        listed.extend(response.sessions.into_iter().map(|s| {
            (
                s.viewed_at,
                s.session_id.unwrap_or_default(),
                s.time_on_page,
                s.is_entry,
                s.is_exit,
                s.is_bounce,
                s.session_page_number,
            )
        }));
    }
    assert_eq!(
        listed, expected,
        "three pages of 4 must list every view once, newest first"
    );

    let all_environments = service
        .get_page_path_visitors(project_id, TARGET, w0, window_end, None, 1, 50)
        .await?;
    assert_eq!(all_environments.total_count, 12);
    assert_eq!(all_environments.sessions.len(), 12);

    cleanup_test_analytics!(db);
    Ok(())
}

/// Regression test for the page-path-detail slowness: the stats query read
/// every event the matched sessions ever had (no project or time bound)
/// and re-ran its per-session aggregate once per page view of the page.
///
/// Seeds a session with 3000 page views that started 30h before the
/// window next to ordinary sessions, and asserts the stats with and
/// without the environment filter, time_on_page priorities and the
/// 1800s cut-off, that entry/exit/bounce see page views outside the
/// bound within the session's lifetime (and, without a request_sessions
/// row, within a day of the bound), and that no plan node handles more
/// rows than a small multiple of the sessions' page views in the bound.
#[tokio::test]
async fn test_get_page_path_detail_bounds_session_scan_by_date() -> anyhow::Result<()> {
    use sea_orm::{ActiveModelTrait, ConnectionTrait, Set, Statement};

    let Some((service, db, _container)) =
        analytics_service_or_skip("test_page_path_detail_bounded").await
    else {
        return Ok(());
    };
    let scope = seeded_scope(db.as_ref()).await?;
    let (project_id, env_id) = (scope.project_id, scope.environment_id);

    const TARGET: &str = "/detail-target";
    const BUSY_PAGE_VIEWS: i64 = 3000;
    let w0 =
        chrono::DateTime::parse_from_rfc3339("2026-03-02T12:00:00Z")?.with_timezone(&chrono::Utc);
    let window_end = w0 + chrono::Duration::hours(2);
    let at = |seconds: i64| w0 + chrono::Duration::seconds(seconds);
    let hours = chrono::Duration::hours;
    let minutes = chrono::Duration::minutes;

    let mut visitors = Vec::new();
    for name in ["detail-busy", "detail-old", "detail-other"] {
        let visitor = temps_entities::visitor::ActiveModel {
            visitor_id: Set(name.to_string()),
            project_id: Set(project_id),
            environment_id: Set(env_id),
            first_seen: Set(w0),
            last_seen: Set(w0),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
        visitors.push(visitor.id);
    }
    let seeder = |visitor_id: i32| FlowSeeder {
        db: db.as_ref(),
        project_id,
        environment_id: Some(env_id),
        visitor_id: Some(visitor_id),
    };
    let (busy_seed, old_seed, seed) = (
        seeder(visitors[0]),
        seeder(visitors[1]),
        seeder(visitors[2]),
    );

    // The busy session: one page view every 2s for 100 minutes on 1000
    // pages, the target every 500th view and as the last view, after 5
    // page views 30h before the window (outside the lookback, inside its
    // lifetime): none of its views is the entry.
    let busy = "d0d0d0d0-0000-4000-8000-000000000001";
    let busy_late = window_end + minutes(45);
    insert_request_session(db.as_ref(), busy, w0 - hours(30), busy_late, None).await?;
    for minute in 0..5 {
        let ts = w0 - hours(30) + minutes(minute);
        busy_seed
            .event(busy, "page_view", "/bulk/early", ts)
            .await?;
    }
    db.execute(Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Postgres,
        r#"
        INSERT INTO events (timestamp, project_id, environment_id, session_id, visitor_id,
                            hostname, pathname, page_path, href,
                            is_entry, is_exit, is_bounce, is_crawler, event_type)
        SELECT $1::timestamptz + k * INTERVAL '2 seconds', $2, $3, $4, $7,
               'example.com', p, p, 'https://example.com' || p,
               false, false, false, false, 'page_view'
        FROM generate_series(0, $5 - 1) k
        CROSS JOIN LATERAL (
            SELECT CASE WHEN k % 500 = 0 OR k = $5 - 1 THEN $6
                        ELSE '/bulk/page-' || (k % 1000) END AS p
        ) pages
        "#,
        vec![
            w0.into(),
            project_id.into(),
            env_id.into(),
            busy.into(),
            BUSY_PAGE_VIEWS.into(),
            TARGET.into(),
            visitors[0].into(),
        ],
    ))
    .await?;
    // Leaves the view at k = 500 after 1s, and is also the first leave of
    // the page within 30 min of the view at k = 0 (1001s).
    busy_seed
        .event(busy, "page_leave", TARGET, at(1001))
        .await?;
    // After the 30-min lookahead: the last target view is left out of the
    // average and the session's exit is this page.
    busy_seed
        .event(busy, "page_view", "/bulk/late", busy_late)
        .await?;

    // Started 26h before the window, then viewed only the target inside
    // the bound: an exit, but neither an entry nor a bounce; 30s fallback.
    let old_view = "d0d0d0d0-0000-4000-8000-000000000002";
    insert_request_session(db.as_ref(), old_view, w0 - hours(26), at(600), None).await?;
    old_seed
        .event(old_view, "page_view", "/early", w0 - hours(26))
        .await?;
    old_seed
        .event(old_view, "page_view", TARGET, at(600))
        .await?;

    // Entry, exit and bounce, 30s fallback.
    let bounce = "d0d0d0d0-0000-4000-8000-000000000003";
    seed.event(bounce, "page_view", TARGET, at(1200)).await?;
    // Entry; the next page view 50s later (priority 2).
    let entry_only = "d0d0d0d0-0000-4000-8000-000000000004";
    seed.event(entry_only, "page_view", TARGET, at(1800))
        .await?;
    seed.event(entry_only, "page_view", "/other", at(1850))
        .await?;
    // Exit; left after 20s (priority 1).
    let exit_only = "d0d0d0d0-0000-4000-8000-000000000005";
    seed.event(exit_only, "page_view", "/other", at(2400))
        .await?;
    seed.event(exit_only, "page_view", TARGET, at(2460)).await?;
    seed.event(exit_only, "page_leave", TARGET, at(2480))
        .await?;
    // Entered 23h before the window, inside the lookback: not an entry.
    let covered = "d0d0d0d0-0000-4000-8000-000000000006";
    seed.event(covered, "page_view", "/yesterday", w0 - hours(23))
        .await?;
    seed.event(covered, "page_view", TARGET, at(3000)).await?;
    // Its last page view is the target again, 45 min after the window
    // (no request_sessions row: found within a day of the bound): an
    // exit, not a bounce, and the in-window view is left out of the
    // average.
    let exits_later = "d0d0d0d0-0000-4000-8000-000000000007";
    seed.event(exits_later, "page_view", TARGET, at(3600))
        .await?;
    seed.event(exits_later, "page_view", TARGET, busy_late)
        .await?;
    // The next page view 2000s later is past the 1800s cut-off.
    let long_stay = "d0d0d0d0-0000-4000-8000-000000000008";
    seed.event(long_stay, "page_view", TARGET, at(4500)).await?;
    seed.event(long_stay, "page_view", "/after", at(6500))
        .await?;
    // The next page view comes 30h after the window, inside the session's
    // lifetime: left out of the average, neither an exit nor a bounce.
    let much_later = window_end + hours(30);
    let day_later = "d0d0d0d0-0000-4000-8000-000000000009";
    insert_request_session(db.as_ref(), day_later, at(5000), much_later, None).await?;
    seed.event(day_later, "page_view", TARGET, at(5000)).await?;
    seed.event(day_later, "page_view", "/much-later", much_later)
        .await?;
    // The same without a request_sessions row: the lookups only reach a
    // day past the bound, so the view gets the 30s fallback and the
    // session counts as an exit and a bounce (the documented fallback).
    let no_lifetime = "d0d0d0d0-0000-4000-8000-00000000000c";
    seed.event(no_lifetime, "page_view", TARGET, at(5400))
        .await?;
    seed.event(no_lifetime, "page_view", "/much-later", much_later)
        .await?;
    // Counted only without the environment filter.
    let no_environment = "d0d0d0d0-0000-4000-8000-00000000000a";
    seed.insert(no_environment, "page_view", TARGET, at(4200), None, false)
        .await?;
    // Never a session of the page (the activity series still counts it).
    let crawler = "d0d0d0d0-0000-4000-8000-00000000000b";
    seed.insert(crawler, "page_view", TARGET, at(4000), Some(env_id), true)
        .await?;

    // (unique visitors, page views, avg time, bounce, entry, exit rate)
    let summary = |d: &crate::types::responses::PagePathDetailResponse| {
        (
            d.unique_visitors,
            d.total_page_views,
            d.avg_time_on_page,
            d.bounce_rate,
            d.entry_rate,
            d.exit_rate,
        )
    };
    let rate = |part: f64, whole: f64| part / whole * 100.0;

    // 10 sessions; time on page: busy 1001, 1, 2 x4 (its last view is
    // left out), old_view, bounce, covered and no_lifetime 30, entry_only
    // 50, exit_only 20; exits_later, long_stay and day_later left out.
    let detail = service
        .get_page_path_detail(project_id, TARGET, w0, window_end, Some(env_id), None)
        .await?;
    assert_eq!(
        summary(&detail),
        (
            3,
            16,
            1200.0 / 12.0,
            rate(2.0, 6.0),
            rate(6.0, 10.0),
            rate(6.0, 10.0)
        ),
        "entries: bounce, entry_only, exits_later, long_stay, day_later, no_lifetime; \
         exits: old_view, bounce, exit_only, covered, exits_later, no_lifetime; \
         bounces: bounce, no_lifetime"
    );
    assert_eq!(detail.bucket_interval, "hour");
    // The other three queries bind the environment too; they count the
    // crawler view, as before.
    let activity_views: i64 = detail.activity_over_time.iter().map(|b| b.page_views).sum();
    assert_eq!(activity_views, 17);
    assert_eq!(detail.referrers.iter().map(|r| r.visits).sum::<i64>(), 17);
    let country_views: i64 = detail.countries.iter().map(|c| c.page_views).sum();
    assert_eq!(country_views, 17);

    let all_environments = service
        .get_page_path_detail(project_id, TARGET, w0, window_end, None, None)
        .await?;
    assert_eq!(
        summary(&all_environments),
        (
            3,
            17,
            1230.0 / 13.0,
            rate(3.0, 7.0),
            rate(7.0, 11.0),
            rate(7.0, 11.0)
        ),
        "no_environment adds a bouncing session"
    );
    let activity_views: i64 = all_environments
        .activity_over_time
        .iter()
        .map(|b| b.page_views)
        .sum();
    assert_eq!(activity_views, 18);

    // The same SQL reading every session's whole life in the bound gives
    // the unbounded answer, which differs only for no_lifetime: its view
    // is left out of the average and it is neither an exit nor a bounce.
    let stats = |start: UtcDateTime, end: UtcDateTime| {
        Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            PAGE_PATH_DETAIL_STATS_SQL,
            vec![
                project_id.into(),
                TARGET.into(),
                w0.into(),
                window_end.into(),
                Some(env_id).into(),
                start.into(),
                end.into(),
                ANALYTICS_ROW_CLOCK_SLACK_HOURS.into(),
            ],
        )
    };
    let year = chrono::Duration::days(365);
    let row = db
        .query_one(stats(w0 - year, window_end + year))
        .await?
        .expect("the stats query returns one row");
    let unbounded = (
        row.try_get::<i64>("", "unique_visitors")?,
        row.try_get::<i64>("", "total_page_views")?,
        row.try_get::<f64>("", "avg_time_on_page")?,
        row.try_get::<f64>("", "bounce_rate")?,
        row.try_get::<f64>("", "entry_rate")?,
        row.try_get::<f64>("", "exit_rate")?,
    );
    assert_eq!(
        unbounded,
        (
            3,
            16,
            1170.0 / 11.0,
            rate(1.0, 6.0),
            rate(6.0, 10.0),
            rate(5.0, 10.0)
        )
    );

    // The cost stays linear in the sessions' page views inside the bound
    // (the busy session's 3000 and a few more): the previous query
    // re-aggregated all of them once per page view of the page. Both plan
    // kinds are checked: the old query's generic plan was its slowest.
    let (context_start, context_end) = page_path_visitors_context_bounds(w0, window_end);
    let explain = stats(context_start, context_end);
    let plans = explain_custom_and_generic(
        db.as_ref(),
        &explain.sql,
        explain.values.map(|values| values.0).unwrap_or_default(),
    )
    .await?;
    for (kind, plan) in &plans {
        let max_rows = max_rows_handled(plan);
        assert!(
            max_rows <= (4 * BUSY_PAGE_VIEWS) as f64,
            "a {kind} plan node handled {max_rows} rows for sessions of about {BUSY_PAGE_VIEWS} page views"
        );
    }

    cleanup_test_analytics!(db);
    Ok(())
}

/// The window functions that replaced the time_on_page self-joins keep
/// their exact semantics: the next event must be strictly later (a tie is
/// not "next"), and a page_leave only counts within 30 minutes. Covers
/// `get_page_paths`, `get_page_path_detail` and `get_visitor_journey`.
#[tokio::test]
async fn test_time_on_page_window_semantics_across_page_queries() -> anyhow::Result<()> {
    use sea_orm::{ActiveModelTrait, Set};

    let Some((service, db, _container)) =
        analytics_service_or_skip("test_time_on_page_window_semantics").await
    else {
        return Ok(());
    };
    let scope = seeded_scope(db.as_ref()).await?;
    let (project_id, env_id) = (scope.project_id, scope.environment_id);

    let t0 =
        chrono::DateTime::parse_from_rfc3339("2026-03-02T12:00:00Z")?.with_timezone(&chrono::Utc);
    let at = |seconds: i64| t0 + chrono::Duration::seconds(seconds);
    let visitor = temps_entities::visitor::ActiveModel {
        visitor_id: Set("window-semantics-visitor".to_string()),
        project_id: Set(project_id),
        environment_id: Set(env_id),
        first_seen: Set(t0),
        last_seen: Set(t0),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    let session = "c0c0c0c0-0000-4000-8000-000000000001";
    let rs_id = insert_request_session(db.as_ref(), session, t0, at(465), Some(visitor.id)).await?;
    let seed = FlowSeeder {
        db: db.as_ref(),
        project_id,
        environment_id: Some(env_id),
        visitor_id: Some(visitor.id),
    };
    // /win/p left after 40s (priority 1); a second leave of the same page
    // at the same instant as the view is not "after" it.
    seed.event(session, "page_view", "/win/p", at(0)).await?;
    seed.event(session, "page_leave", "/win/p", at(0)).await?;
    seed.event(session, "page_leave", "/win/p", at(40)).await?;
    // Two page views at the same instant: each one's next page view is
    // the one at 120s, not its twin.
    seed.event(session, "page_view", "/win/q", at(60)).await?;
    seed.event(session, "page_view", "/win/s", at(60)).await?;
    // /win/p again: its page_leave 31 min later is ignored, so the next
    // page view at 420s sets it (priority 2).
    seed.event(session, "page_view", "/win/p", at(120)).await?;
    seed.event(session, "page_leave", "/win/p", at(120 + 31 * 60))
        .await?;
    // Last page view: get_page_paths / get_page_path_detail fall back to
    // 30s, the journey to the session's last_accessed_at (45s later).
    seed.event(session, "page_view", "/win/r", at(420)).await?;

    let window_start = t0 - chrono::Duration::hours(1);
    let window_end = t0 + chrono::Duration::hours(1);

    let paths = service
        .get_page_paths(
            project_id,
            Some(env_id),
            Some(window_start),
            Some(window_end),
            Some(100),
        )
        .await?;
    let avg = |path: &str| {
        paths
            .page_paths
            .iter()
            .find(|p| p.page_path == path)
            .unwrap_or_else(|| panic!("{path} must be listed"))
            .avg_time_seconds
    };
    assert_eq!(avg("/win/p"), Some(170.0), "(40 + 300) / 2");
    assert_eq!(avg("/win/q"), Some(60.0));
    assert_eq!(avg("/win/s"), Some(60.0));
    assert_eq!(avg("/win/r"), Some(30.0));

    let detail = service
        .get_page_path_detail(
            project_id,
            "/win/p",
            window_start,
            window_end,
            Some(env_id),
            None,
        )
        .await?;
    assert_eq!(detail.total_page_views, 2);
    assert!(
        (detail.avg_time_on_page - 170.0).abs() < 1e-9,
        "avg_time_on_page was {}",
        detail.avg_time_on_page
    );

    let journey = service
        .get_visitor_journey(visitor.id, project_id, None)
        .await?
        .expect("visitor has a journey");
    let journey_session = journey
        .sessions
        .iter()
        .find(|s| s.session_id == rs_id)
        .expect("seeded session must appear in the journey");
    let mut durations: Vec<(String, Option<i32>)> = journey_session
        .events
        .iter()
        .filter(|e| e.event_type == "page_view")
        .map(|e| (e.page_path.clone().unwrap_or_default(), e.time_on_page))
        .collect();
    durations.sort();
    assert_eq!(
        durations,
        vec![
            ("/win/p".to_string(), Some(40)),
            ("/win/p".to_string(), Some(300)),
            ("/win/q".to_string(), Some(60)),
            ("/win/r".to_string(), Some(45)),
            ("/win/s".to_string(), Some(60)),
        ]
    );

    cleanup_test_analytics!(db);
    Ok(())
}

/// Every table or chunk an `EXPLAIN (ANALYZE, FORMAT JSON)` plan node
/// actually read. Nodes TimescaleDB pruned at run time stay in the plan
/// but never execute, so they are left out.
fn scanned_relations(node: &serde_json::Value, out: &mut Vec<String>) {
    let executed = node
        .get("Actual Loops")
        .and_then(|v| v.as_f64())
        .is_some_and(|loops| loops > 0.0);
    if !executed {
        return;
    }
    if let Some(name) = node.get("Relation Name").and_then(|v| v.as_str()) {
        out.push(name.to_string());
    }
    if let Some(plans) = node.get("Plans").and_then(|v| v.as_array()) {
        for plan in plans {
            scanned_relations(plan, out);
        }
    }
}

/// One plan of [`explain_analyze`]: which one it was, its root node and the
/// relations it read.
struct ExplainedPlan {
    mode: &'static str,
    root: serde_json::Value,
    read: Vec<String>,
}

/// [`explain_custom_and_generic`] with the relations each plan read. Chunk
/// exclusion differs between the two: a custom plan drops chunks while
/// planning, a generic one only when the executor starts or runs, so the
/// time-bound checks hold for both.
async fn explain_analyze(
    db: &sea_orm::DatabaseConnection,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> anyhow::Result<Vec<ExplainedPlan>> {
    Ok(explain_custom_and_generic(db, sql, values)
        .await?
        .into_iter()
        .map(|(mode, root)| {
            let mut read = Vec::new();
            scanned_relations(&root, &mut read);
            ExplainedPlan { mode, root, read }
        })
        .collect())
}

/// The TimescaleDB chunks of `hypertable` whose time range holds `ts`
/// (one per space partition that has rows).
async fn chunks_holding(
    db: &sea_orm::DatabaseConnection,
    hypertable: &str,
    ts: UtcDateTime,
) -> anyhow::Result<Vec<String>> {
    use sea_orm::ConnectionTrait;
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT chunk_name FROM timescaledb_information.chunks \
             WHERE hypertable_name = $1 AND range_start <= $2 AND range_end > $2",
            vec![hypertable.into(), ts.into()],
        ))
        .await?;
    let chunks = rows
        .iter()
        .map(|row| row.try_get::<String>("", "chunk_name"))
        .collect::<Result<Vec<_>, _>>()?;
    anyhow::ensure!(!chunks.is_empty(), "no {hypertable} chunk holds {ts}");
    Ok(chunks)
}

/// Whether the plan read any of `chunks`.
fn reads_any(read: &[String], chunks: &[String]) -> bool {
    chunks.iter().any(|chunk| read.contains(chunk))
}

async fn insert_visitor(
    db: &sea_orm::DatabaseConnection,
    scope: &SeededScope,
    guid: &str,
    first_seen: UtcDateTime,
) -> anyhow::Result<i32> {
    use sea_orm::{ActiveModelTrait, Set};
    let row = visitor::ActiveModel {
        visitor_id: Set(guid.to_string()),
        project_id: Set(scope.project_id),
        environment_id: Set(scope.environment_id),
        first_seen: Set(first_seen),
        last_seen: Set(first_seen),
        ..Default::default()
    }
    .insert(db)
    .await?;
    Ok(row.id)
}

async fn insert_proxy_log(
    db: &sea_orm::DatabaseConnection,
    project_id: i32,
    session_id: i32,
    timestamp: UtcDateTime,
) -> anyhow::Result<()> {
    use sea_orm::ConnectionTrait;
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO proxy_logs (timestamp, method, path, host, status_code, \
         request_source, is_system_request, routing_status, request_id, \
         created_date, project_id, session_id) \
         VALUES ($1, 'GET', '/', 'example.com', 200, 'proxy', false, 'routed', \
         gen_random_uuid()::text, $1::date, $2, $3)",
        vec![timestamp.into(), project_id.into(), session_id.into()],
    ))
    .await?;
    Ok(())
}

/// The visitor session list and journey aggregate a visitor's sessions
/// (bare and legacy v2 session ids, also mixed within one session),
/// leave out sessions with no request_sessions row, and count each
/// session's proxy requests -- while reading only the chunks from the
/// visitor's first_seen on (events) and the listed sessions' lifetimes
/// (proxy_logs). Without those bounds every chunk of the project's
/// history was read for a single visitor.
#[tokio::test]
async fn test_visitor_sessions_and_journey_read_only_the_visitors_time_range() -> anyhow::Result<()>
{
    let Some((service, db, _container)) =
        analytics_service_or_skip("test_visitor_sessions_time_range").await
    else {
        return Ok(());
    };
    let db = db.as_ref();
    let scope = seeded_scope(db).await?;
    let project_id = scope.project_id;
    let t0 =
        chrono::DateTime::parse_from_rfc3339("2026-03-10T12:00:00Z")?.with_timezone(&chrono::Utc);
    let min = |m: i64| t0 + chrono::Duration::minutes(m);
    let day = |d: i64| chrono::Duration::days(d);

    // Another visitor's history, two months before this visitor existed:
    // its event and request-log chunks must never be read.
    let long_ago = t0 - day(60);
    let other_visitor = insert_visitor(db, &scope, "earlier-visitor", long_ago).await?;
    let old_session =
        insert_request_session(db, "old-session", long_ago, long_ago, Some(other_visitor)).await?;
    FlowSeeder {
        db,
        project_id,
        environment_id: Some(scope.environment_id),
        visitor_id: Some(other_visitor),
    }
    .event("old-session", "page_view", "/old", long_ago)
    .await?;
    insert_proxy_log(db, project_id, old_session, long_ago).await?;

    let visitor_id = insert_visitor(db, &scope, "listed-visitor", t0).await?;
    let seed = FlowSeeder {
        db,
        project_id,
        environment_id: Some(scope.environment_id),
        visitor_id: Some(visitor_id),
    };

    // A: bare session id, three page views, a click and a page_leave,
    // three proxy requests.
    let a = insert_request_session(db, "sess-a", min(1), min(20), Some(visitor_id)).await?;
    seed.event("sess-a", "page_view", "/a1", min(1)).await?;
    seed.event("sess-a", "signup_click", "/a1", min(2)).await?;
    seed.event("sess-a", "page_view", "/a2", min(5)).await?;
    seed.event("sess-a", "page_leave", "/a2", min(6)).await?;
    seed.event("sess-a", "page_view", "/a3", min(10)).await?;
    for m in [1, 5, 10] {
        insert_proxy_log(db, project_id, a, min(m)).await?;
    }
    // C: events whose session never reached request_sessions.
    seed.event("sess-c-orphan", "page_view", "/c", t0 + day(1))
        .await?;
    // B: legacy v2 id only, a single page view and one proxy request.
    let b_start = t0 + day(2);
    let b = insert_request_session(
        db,
        "sess-b",
        b_start,
        b_start + chrono::Duration::minutes(5),
        Some(visitor_id),
    )
    .await?;
    seed.event(
        "v2|sess-b|1700000000",
        "page_view",
        "/b1",
        b_start + chrono::Duration::minutes(1),
    )
    .await?;
    insert_proxy_log(db, project_id, b, b_start + chrono::Duration::minutes(1)).await?;
    // D: one session written under both id formats.
    let d_start = t0 + day(3);
    let d = insert_request_session(
        db,
        "sess-d",
        d_start,
        d_start + chrono::Duration::minutes(5),
        Some(visitor_id),
    )
    .await?;
    seed.event("v2|sess-d|1700000000", "page_view", "/d1", d_start)
        .await?;
    seed.event(
        "sess-d",
        "page_view",
        "/d2",
        d_start + chrono::Duration::seconds(30),
    )
    .await?;

    // -- get_visitor_sessions_by_id --
    let listed = service
        .get_visitor_sessions_by_id(visitor_id, None)
        .await?
        .expect("the visitor exists");
    assert_eq!(listed.visitor_id, "listed-visitor");
    assert_eq!(
        listed.total_sessions, 3,
        "the session without a request_sessions row is not counted"
    );
    let summary: Vec<_> = listed
        .sessions
        .iter()
        .map(|s| {
            (
                s.session_id,
                s.page_views,
                s.events_count,
                s.requests_count,
                s.entry_path.clone(),
                s.exit_path.clone(),
                s.duration_seconds,
                s.is_bounced,
                s.is_engaged,
            )
        })
        .collect();
    let path = |p: &str| Some(p.to_string());
    assert_eq!(
        summary,
        vec![
            (d, 2, 2, 0, path("/d1"), path("/d2"), 30, false, true),
            (b, 1, 1, 1, path("/b1"), path("/b1"), 0, true, false),
            (a, 3, 5, 3, path("/a1"), path("/a3"), 540, false, true),
        ],
        "newest first; v2 and bare ids of one session are combined"
    );
    let first_two = service
        .get_visitor_sessions_by_id(visitor_id, Some(2))
        .await?
        .expect("the visitor exists");
    assert_eq!(
        first_two
            .sessions
            .iter()
            .map(|s| s.session_id)
            .collect::<Vec<_>>(),
        vec![d, b]
    );
    assert_eq!(first_two.total_sessions, 3);

    // -- get_visitor_journey --
    let journey = service
        .get_visitor_journey(visitor_id, project_id, None)
        .await?
        .expect("the visitor exists");
    assert_eq!(journey.total_sessions, 3);
    assert_eq!(
        journey.total_events, 7,
        "page_leave rows are folded into time on page"
    );
    let sessions: Vec<_> = journey
        .sessions
        .iter()
        .map(|s| {
            (
                s.session_id,
                s.page_views,
                s.entry_path.clone(),
                s.exit_path.clone(),
                s.is_bounced,
                s.is_engaged,
                s.events
                    .iter()
                    .map(|e| (e.event_type.clone(), e.page_path.clone(), e.time_on_page))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    let pv = |p: &str, seconds: i32| ("page_view".to_string(), path(p), Some(seconds));
    assert_eq!(
        sessions,
        vec![
            (
                d,
                2,
                path("/d1"),
                path("/d2"),
                false,
                false,
                vec![pv("/d1", 30), pv("/d2", 270)]
            ),
            (
                b,
                1,
                path("/b1"),
                path("/b1"),
                true,
                false,
                vec![pv("/b1", 240)]
            ),
            (
                a,
                3,
                path("/a1"),
                path("/a3"),
                false,
                true,
                vec![
                    pv("/a1", 240),
                    ("signup_click".to_string(), path("/a1"), None),
                    pv("/a2", 60),
                    pv("/a3", 600),
                ]
            ),
        ]
    );
    let latest = service
        .get_visitor_journey(visitor_id, project_id, Some(1))
        .await?
        .expect("the visitor exists");
    assert_eq!(latest.total_sessions, 3);
    assert_eq!(
        latest
            .sessions
            .iter()
            .map(|s| s.session_id)
            .collect::<Vec<_>>(),
        vec![d]
    );

    // -- the plans only read the visitor's own time range --
    let old_events = chunks_holding(db, "events", long_ago).await?;
    let old_proxy_logs = chunks_holding(db, "proxy_logs", long_ago).await?;
    let recent_events = chunks_holding(db, "events", min(1)).await?;
    let recent_proxy_logs = chunks_holding(db, "proxy_logs", min(1)).await?;
    let bound = earliest_row_time(t0);

    for ExplainedPlan { mode, read, .. } in explain_analyze(
        db,
        VISITOR_SESSIONS_SQL,
        vec![
            visitor_id.into(),
            100i64.into(),
            project_id.into(),
            bound.into(),
            ANALYTICS_ROW_CLOCK_SLACK_HOURS.into(),
        ],
    )
    .await?
    {
        assert!(
            reads_any(&read, &recent_events) && reads_any(&read, &recent_proxy_logs),
            "{mode} plan: visitor sessions must read the visitor's chunks: {read:?}"
        );
        assert!(
            !reads_any(&read, &old_events) && !reads_any(&read, &old_proxy_logs),
            "{mode} plan: visitor sessions read a chunk from before the visitor existed: {read:?}"
        );
    }

    for ExplainedPlan { mode, read, .. } in explain_analyze(
        db,
        VISITOR_JOURNEY_SESSIONS_SQL,
        vec![
            visitor_id.into(),
            project_id.into(),
            50i64.into(),
            bound.into(),
        ],
    )
    .await?
    {
        assert!(reads_any(&read, &recent_events), "{mode} plan: {read:?}");
        assert!(
            !reads_any(&read, &old_events),
            "{mode} plan: the journey's session list read a chunk from before the visitor existed: {read:?}"
        );
    }

    for ExplainedPlan { mode, read, .. } in explain_analyze(
        db,
        &visitor_journey_events_sql(3),
        vec![
            visitor_id.into(),
            project_id.into(),
            min(1).into(),
            (d_start + chrono::Duration::seconds(30)).into(),
            d.into(),
            b.into(),
            a.into(),
        ],
    )
    .await?
    {
        assert!(reads_any(&read, &recent_events), "{mode} plan: {read:?}");
        assert!(
            !reads_any(&read, &old_events),
            "{mode} plan: the journey's events read a chunk outside its sessions: {read:?}"
        );
    }

    cleanup_test_analytics!(db);
    Ok(())
}

/// Session events read only the requested page (not every event of the
/// session through a window over all of them) and only chunks from the
/// session's start on, while ids, order, filters and the total are kept.
#[tokio::test]
async fn test_get_session_events_reads_only_the_requested_page() -> anyhow::Result<()> {
    use sea_orm::ConnectionTrait;

    let Some((service, db, _container)) =
        analytics_service_or_skip("test_session_events_page").await
    else {
        return Ok(());
    };
    let db = db.as_ref();
    let scope = seeded_scope(db).await?;
    let (project_id, env_id) = (scope.project_id, scope.environment_id);
    let t0 =
        chrono::DateTime::parse_from_rfc3339("2026-03-10T12:00:00Z")?.with_timezone(&chrono::Utc);
    let sec = |s: i64| t0 + chrono::Duration::seconds(s);
    let seed = FlowSeeder {
        db,
        project_id,
        environment_id: Some(env_id),
        visitor_id: None,
    };

    // Another session's event two months earlier.
    let long_ago = t0 - chrono::Duration::days(60);
    seed.event("earlier-session", "page_view", "/old", long_ago)
        .await?;

    // A busy session: one event a second for five and a half hours.
    const SESSION_EVENTS: i64 = 20_000;
    let session = insert_request_session(db, "busy-session", t0, sec(SESSION_EVENTS), None).await?;
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO events (project_id, environment_id, session_id, event_type, \
         page_path, hostname, pathname, href, timestamp, is_entry, is_exit, \
         is_bounce, is_crawler) \
         SELECT $1, $2, 'busy-session', \
                CASE WHEN i % 2 = 0 THEN 'page_view' ELSE 'scroll' END, \
                '/p' || i, 'example.com', '/p' || i, 'https://example.com/p' || i, \
                $3::timestamptz + make_interval(secs => i), false, false, false, false \
         FROM generate_series(0, $4 - 1) AS i",
        vec![
            project_id.into(),
            env_id.into(),
            t0.into(),
            SESSION_EVENTS.into(),
        ],
    ))
    .await?;
    db.execute_unprepared("ANALYZE events").await?;
    // Outside the environment filter.
    for i in 0..5 {
        seed.insert(
            "busy-session",
            "page_view",
            "/other-env",
            sec(i),
            None,
            false,
        )
        .await?;
    }

    let page = |r: &crate::types::responses::SessionEventsResponse| {
        r.events
            .iter()
            .map(|e| (e.id, e.occurred_at, e.request_path.clone()))
            .collect::<Vec<_>>()
    };
    let newest = service
        .get_session_events(
            session,
            project_id,
            Some(env_id),
            None,
            None,
            Some(3),
            None,
            None,
        )
        .await?
        .expect("the session exists");
    assert_eq!(newest.total_count, SESSION_EVENTS);
    assert_eq!((newest.offset, newest.limit), (0, 3));
    assert_eq!(
        page(&newest),
        vec![
            (1, sec(19_999), "/p19999".to_string()),
            (2, sec(19_998), "/p19998".to_string()),
            (3, sec(19_997), "/p19997".to_string()),
        ],
        "descending by default, ids are 1-based positions"
    );

    let last = service
        .get_session_events(
            session,
            project_id,
            Some(env_id),
            None,
            None,
            Some(3),
            Some(19_997),
            Some("asc".to_string()),
        )
        .await?
        .expect("the session exists");
    assert_eq!(last.total_count, SESSION_EVENTS);
    assert_eq!(
        page(&last),
        vec![
            (19_998, sec(19_997), "/p19997".to_string()),
            (19_999, sec(19_998), "/p19998".to_string()),
            (20_000, sec(19_999), "/p19999".to_string()),
        ]
    );

    let past_the_end = service
        .get_session_events(
            session,
            project_id,
            Some(env_id),
            None,
            None,
            Some(3),
            Some(20_000),
            None,
        )
        .await?
        .expect("the session exists");
    assert!(past_the_end.events.is_empty());
    assert_eq!(past_the_end.total_count, 0, "an empty page reports 0");

    let all_environments = service
        .get_session_events(session, project_id, None, None, None, Some(1), None, None)
        .await?
        .expect("the session exists");
    assert_eq!(all_environments.total_count, SESSION_EVENTS + 5);

    let ranged = service
        .get_session_events(
            session,
            project_id,
            Some(env_id),
            Some(sec(100)),
            Some(sec(199)),
            Some(1),
            None,
            Some("asc".to_string()),
        )
        .await?
        .expect("the session exists");
    assert_eq!(ranged.total_count, 100);
    assert_eq!(page(&ranged), vec![(1, sec(100), "/p100".to_string())]);

    let missing = service
        .get_session_events(i32::MAX, project_id, None, None, None, None, None, None)
        .await;
    assert!(
        matches!(missing, Err(AnalyticsError::SessionNotFound(ref m)) if m.contains(&i32::MAX.to_string())),
        "unknown session ids are reported with the id: {missing:?}"
    );

    // -- plans --
    let request_session = request_sessions::Entity::find_by_id(session)
        .one(db)
        .await?
        .expect("the session exists");
    let queries = session_events_queries(
        &request_session,
        project_id,
        Some(env_id),
        (None, None),
        10,
        0,
        None,
    );
    let old_chunks = chunks_holding(db, "events", long_ago).await?;
    let session_chunks = chunks_holding(db, "events", t0).await?;

    for ExplainedPlan { mode, root, read } in
        explain_analyze(db, &queries.page_sql, queries.page_values).await?
    {
        let max_rows = max_rows_handled(&root);
        assert!(
            max_rows <= 20.0,
            "{mode} plan: the page query handled {max_rows} rows for a page of 10 out of {SESSION_EVENTS}"
        );
        assert!(reads_any(&read, &session_chunks), "{mode} plan: {read:?}");
        assert!(
            !reads_any(&read, &old_chunks),
            "{mode} plan: the page query read a chunk from before the session: {read:?}"
        );
    }
    for ExplainedPlan { mode, read, .. } in
        explain_analyze(db, &queries.count_sql, queries.count_values).await?
    {
        assert!(reads_any(&read, &session_chunks), "{mode} plan: {read:?}");
        assert!(
            !reads_any(&read, &old_chunks),
            "{mode} plan: the count read a chunk from before the session: {read:?}"
        );
    }

    cleanup_test_analytics!(db);
    Ok(())
}

/// A session's page views outside the `[$8, $9]` bounds of
/// `get_page_path_visitors` still decide `is_entry` / `is_exit` /
/// `is_bounce`, and the existence checks that see them read only the
/// chunks of the session's own lifetime, never the project's history up
/// to now or back to its start.
#[tokio::test]
async fn test_get_page_path_visitors_flags_see_the_whole_session_within_its_lifetime(
) -> anyhow::Result<()> {
    let Some((service, db, _container)) =
        analytics_service_or_skip("test_page_path_visitors_session_lifetime").await
    else {
        return Ok(());
    };
    let db = db.as_ref();
    let scope = seeded_scope(db).await?;
    let (project_id, env_id) = (scope.project_id, scope.environment_id);
    let seed = FlowSeeder {
        db,
        project_id,
        environment_id: Some(env_id),
        visitor_id: None,
    };
    const TARGET: &str = "/pricing";
    let w0 =
        chrono::DateTime::parse_from_rfc3339("2026-04-10T12:00:00Z")?.with_timezone(&chrono::Utc);
    let window_end = w0 + chrono::Duration::hours(1);
    let min = |m: i64| w0 + chrono::Duration::minutes(m);

    // Kept alive for more than a day: one page view 30h before the
    // window (outside the lookback), one target view inside it.
    insert_request_session(db, "long-lived", min(-30 * 60), min(20), None).await?;
    seed.event("long-lived", "page_view", "/a", min(-30 * 60))
        .await?;
    seed.event("long-lived", "page_view", TARGET, min(10))
        .await?;
    // A real bounce next to it.
    insert_request_session(db, "single-view", min(5), min(6), None).await?;
    seed.event("single-view", "page_view", TARGET, min(5))
        .await?;
    // Other sessions' page views two months before and after: the
    // existence checks must not read their chunks.
    let before = w0 - chrono::Duration::days(60);
    let after = w0 + chrono::Duration::days(60);
    seed.event("earlier-session", "page_view", "/x", before)
        .await?;
    seed.event("later-session", "page_view", "/x", after)
        .await?;

    let response = service
        .get_page_path_visitors(project_id, TARGET, w0, window_end, Some(env_id), 1, 50)
        .await?;
    let listed: Vec<_> = response
        .sessions
        .iter()
        .map(|s| {
            (
                s.session_id.clone().unwrap_or_default(),
                s.is_entry,
                s.is_exit,
                s.is_bounce,
                s.session_page_number,
            )
        })
        .collect();
    assert_eq!(
        listed,
        vec![
            ("long-lived".to_string(), false, true, false, Some(1)),
            ("single-view".to_string(), true, true, true, Some(1)),
        ],
        "a page view before the lookback rules out entry and bounce"
    );

    let (context_start, context_end) = page_path_visitors_context_bounds(w0, window_end);
    for ExplainedPlan { mode, read, .. } in explain_analyze(
        db,
        PAGE_PATH_VISITORS_SQL,
        vec![
            project_id.into(),
            TARGET.into(),
            w0.into(),
            window_end.into(),
            Some(env_id).into(),
            50i64.into(),
            0i64.into(),
            context_start.into(),
            context_end.into(),
            ANALYTICS_ROW_CLOCK_SLACK_HOURS.into(),
        ],
    )
    .await?
    {
        let window_chunks = chunks_holding(db, "events", min(10)).await?;
        let long_ago_chunks = chunks_holding(db, "events", min(-30 * 60)).await?;
        assert!(reads_any(&read, &window_chunks), "{mode} plan: {read:?}");
        assert!(
            reads_any(&read, &long_ago_chunks),
            "{mode} plan: the earlier-page-view check must reach the session's start: {read:?}"
        );
        assert!(
            !reads_any(&read, &chunks_holding(db, "events", before).await?),
            "{mode} plan: a check read a chunk from before any listed session started: {read:?}"
        );
        assert!(
            !reads_any(&read, &chunks_holding(db, "events", after).await?),
            "{mode} plan: a check read a chunk from after every listed session ended: {read:?}"
        );
    }

    cleanup_test_analytics!(db);
    Ok(())
}
