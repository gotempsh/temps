//! Integration test: write rows of multiple kinds and verify the merge
//! service interleaves them correctly with kind filters and time bounds.
//!
//! Mirrors the project's existing integration-test pattern (TestDatabase
//! with migrations, scoped per-test schema).

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use sea_orm::{ActiveModelTrait, DatabaseConnection, Set};
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use temps_database::test_utils::TestDatabase;
use temps_entities::{error_events, error_groups, projects, proxy_logs, revenue_events};
use temps_observability::{
    service::FullEvent, EventFilters, EventKind, ObservabilityError, ObservabilityEvent,
    ObservabilityService,
};
use uuid::Uuid;

async fn create_test_project(db: &Arc<DatabaseConnection>) -> i32 {
    use temps_entities::preset::Preset;
    let slug = format!("obs-{}", Uuid::new_v4());
    let p = projects::ActiveModel {
        name: Set("Obs Test".into()),
        repo_name: Set("r".into()),
        repo_owner: Set("o".into()),
        directory: Set("/".into()),
        main_branch: Set("main".into()),
        slug: Set(slug),
        preset: Set(Preset::NextJs),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    p.insert(db.as_ref()).await.expect("insert project").id
}

async fn create_error_group(db: &Arc<DatabaseConnection>, project_id: i32) -> i32 {
    let g = error_groups::ActiveModel {
        project_id: Set(project_id),
        title: Set("TypeError: x".into()),
        error_type: Set("TypeError".into()),
        first_seen: Set(Utc::now()),
        last_seen: Set(Utc::now()),
        total_count: Set(1),
        status: Set("unresolved".into()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    g.insert(db.as_ref()).await.expect("insert group").id
}

#[tokio::test]
async fn merge_returns_rows_in_descending_ts_across_kinds() {
    let test_db = TestDatabase::with_migrations()
        .await
        .expect("test db with migrations");
    let db = test_db.connection_arc();

    let project_id = create_test_project(&db).await;
    let group_id = create_error_group(&db, project_id).await;

    // Three events at descending timestamps, one per kind.
    let t_request = Utc.with_ymd_and_hms(2026, 5, 1, 12, 3, 0).unwrap();
    let t_error = Utc.with_ymd_and_hms(2026, 5, 1, 12, 2, 0).unwrap();
    let t_revenue = Utc.with_ymd_and_hms(2026, 5, 1, 12, 1, 0).unwrap();

    proxy_logs::ActiveModel {
        timestamp: Set(t_request),
        method: Set("GET".into()),
        path: Set("/api".into()),
        host: Set("x.test".into()),
        status_code: Set(200),
        response_time_ms: Set(Some(12)),
        request_source: Set("proxy".into()),
        is_system_request: Set(false),
        routing_status: Set("routed".into()),
        project_id: Set(Some(project_id)),
        request_id: Set("req-1".into()),
        created_date: Set(t_request.date_naive()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert proxy log");

    error_events::ActiveModel {
        error_group_id: Set(group_id),
        project_id: Set(project_id),
        fingerprint_hash: Set("abc".into()),
        timestamp: Set(t_error),
        exception_type: Set("TypeError".into()),
        exception_value: Set(Some("boom".into())),
        source: Set(Some("custom".into())),
        created_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert error event");

    revenue_events::ActiveModel {
        project_id: Set(project_id),
        integration_id: Set(0),
        provider: Set("stripe".into()),
        provider_event_id: Set(format!("evt_{}", Uuid::new_v4())),
        event_type: Set("invoice.paid".into()),
        amount_minor: Set(Some(4200)),
        currency: Set(Some("usd".into())),
        occurred_at: Set(t_revenue),
        payload: Set(serde_json::json!({})),
        created_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert revenue event");

    let service = ObservabilityService::new(db.clone());
    let events = service
        .query(EventFilters {
            project_id,
            kinds: EventKind::ALL.iter().copied().collect(),
            from: None,
            to: None,
            deployment_id: None,
            environment_id: None,
            search: None,
            limit: 50,
            hide_bots: None,
        })
        .await
        .expect("query");

    assert_eq!(events.len(), 3);
    // Strictly descending by ts
    assert!(events[0].ts() >= events[1].ts());
    assert!(events[1].ts() >= events[2].ts());
    // Latest is the request, oldest is revenue
    assert_eq!(events[0].kind(), EventKind::Request);
    assert_eq!(events[2].kind(), EventKind::Revenue);
}

#[tokio::test]
async fn kinds_filter_excludes_unselected_tables() {
    let test_db = TestDatabase::with_migrations().await.expect("test db");
    let db = test_db.connection_arc();
    let project_id = create_test_project(&db).await;
    let group_id = create_error_group(&db, project_id).await;

    let now = Utc::now();
    proxy_logs::ActiveModel {
        timestamp: Set(now),
        method: Set("GET".into()),
        path: Set("/x".into()),
        host: Set("h".into()),
        status_code: Set(200),
        request_source: Set("proxy".into()),
        is_system_request: Set(false),
        routing_status: Set("routed".into()),
        project_id: Set(Some(project_id)),
        request_id: Set("r".into()),
        created_date: Set(now.date_naive()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert");
    error_events::ActiveModel {
        error_group_id: Set(group_id),
        project_id: Set(project_id),
        fingerprint_hash: Set("fp".into()),
        timestamp: Set(now),
        exception_type: Set("Boom".into()),
        source: Set(Some("custom".into())),
        created_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert error");

    let service = ObservabilityService::new(db.clone());

    let only_errors = service
        .query(EventFilters {
            project_id,
            kinds: [EventKind::Error].iter().copied().collect(),
            from: None,
            to: None,
            deployment_id: None,
            environment_id: None,
            search: None,
            limit: 50,
            hide_bots: None,
        })
        .await
        .expect("query errors only");

    assert!(only_errors.iter().all(|e| e.kind() == EventKind::Error));
}

#[tokio::test]
async fn time_range_filters_old_rows() {
    let test_db = TestDatabase::with_migrations().await.expect("test db");
    let db = test_db.connection_arc();
    let project_id = create_test_project(&db).await;

    let new = Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap();
    let old = new - Duration::days(30);

    for (ts, req_id) in [(new, "new"), (old, "old")] {
        proxy_logs::ActiveModel {
            timestamp: Set(ts),
            method: Set("GET".into()),
            path: Set(format!("/{}", req_id)),
            host: Set("h".into()),
            status_code: Set(200),
            request_source: Set("proxy".into()),
            is_system_request: Set(false),
            routing_status: Set("routed".into()),
            project_id: Set(Some(project_id)),
            request_id: Set(req_id.into()),
            created_date: Set(ts.date_naive()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert");
    }

    let service = ObservabilityService::new(db.clone());
    let recent = service
        .query(EventFilters {
            project_id,
            kinds: [EventKind::Request].iter().copied().collect(),
            from: Some(new - Duration::hours(1)),
            to: Some(new + Duration::hours(1)),
            deployment_id: None,
            environment_id: None,
            search: None,
            limit: 50,
            hide_bots: None,
        })
        .await
        .expect("query");

    assert_eq!(recent.len(), 1, "old row should be filtered out");
    let ObservabilityEvent::Request(r) = &recent[0] else {
        panic!("expected request");
    };
    assert_eq!(r.path, "/new");
}

#[tokio::test]
async fn fetch_full_returns_untruncated_request() {
    let test_db = TestDatabase::with_migrations().await.expect("test db");
    let db = test_db.connection_arc();
    let project_id = create_test_project(&db).await;

    let now = Utc::now();
    let inserted = proxy_logs::ActiveModel {
        timestamp: Set(now),
        method: Set("POST".into()),
        path: Set("/checkout".into()),
        host: Set("h".into()),
        status_code: Set(500),
        request_source: Set("proxy".into()),
        is_system_request: Set(false),
        routing_status: Set("routed".into()),
        project_id: Set(Some(project_id)),
        request_id: Set("rid".into()),
        // Headers larger than the whitelist would survive truncation
        request_headers: Set(Some(serde_json::json!({
            "host": "h",
            "x-secret-1": "a",
            "x-secret-2": "b",
        }))),
        created_date: Set(now.date_naive()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert");

    let service = ObservabilityService::new(db.clone());
    let full = service
        .fetch_full(project_id, EventKind::Request, &inserted.id.to_string())
        .await
        .expect("fetch full");

    let FullEvent::Request(req) = full else {
        panic!("expected Request");
    };
    // Full payload preserves the un-whitelisted headers — proves we returned
    // the un-truncated form.
    let headers = req.request_headers.expect("headers");
    let obj = headers.as_object().unwrap();
    assert!(obj.contains_key("x-secret-1"));
    assert!(obj.contains_key("x-secret-2"));
}

#[tokio::test]
async fn fetch_full_404s_for_unknown_id() {
    let test_db = TestDatabase::with_migrations().await.expect("test db");
    let db = test_db.connection_arc();
    let project_id = create_test_project(&db).await;
    let service = ObservabilityService::new(db.clone());

    let err = service
        .fetch_full(project_id, EventKind::Request, "999999")
        .await
        .unwrap_err();
    assert!(matches!(err, ObservabilityError::EventNotFound { .. }));
}

#[tokio::test]
async fn fetch_full_404s_for_wrong_project() {
    let test_db = TestDatabase::with_migrations().await.expect("test db");
    let db = test_db.connection_arc();
    let project_a = create_test_project(&db).await;
    let project_b = create_test_project(&db).await;

    let now = Utc::now();
    let inserted = proxy_logs::ActiveModel {
        timestamp: Set(now),
        method: Set("GET".into()),
        path: Set("/x".into()),
        host: Set("h".into()),
        status_code: Set(200),
        request_source: Set("proxy".into()),
        is_system_request: Set(false),
        routing_status: Set("routed".into()),
        project_id: Set(Some(project_a)),
        request_id: Set("r".into()),
        created_date: Set(now.date_naive()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert");

    let service = ObservabilityService::new(db.clone());
    // Same id, but wrong project — must not leak across project boundaries.
    let err = service
        .fetch_full(project_b, EventKind::Request, &inserted.id.to_string())
        .await
        .unwrap_err();
    assert!(matches!(err, ObservabilityError::EventNotFound { .. }));
}

#[tokio::test]
async fn fetch_full_rejects_malformed_id() {
    let test_db = TestDatabase::with_migrations().await.expect("test db");
    let db = test_db.connection_arc();
    let service = ObservabilityService::new(db.clone());

    let err = service
        .fetch_full(1, EventKind::Request, "not-an-int")
        .await
        .unwrap_err();
    assert!(matches!(err, ObservabilityError::EventNotFound { .. }));
}

#[tokio::test]
async fn fetch_spans_returns_span_rows_from_otel_table() {
    let test_db = TestDatabase::with_migrations()
        .await
        .expect("test db with migrations");
    let db = test_db.connection_arc();
    let project_id = create_test_project(&db).await;

    // otel_spans has no Sea-ORM entity — insert raw via SQL, then query
    // through the merge service.
    let t = Utc.with_ymd_and_hms(2026, 5, 1, 12, 6, 0).unwrap();
    let trace_id = "deadbeefdeadbeefdeadbeefdeadbeef";
    let span_id = "1111111111111111";
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO otel_spans \
         (project_id, deployment_id, service_name, trace_id, span_id, parent_span_id, \
          name, kind, start_time, end_time, duration_ms, status_code, status_message, \
          attributes, events) \
         VALUES ($1, NULL, $2, $3, $4, NULL, $5, 'SERVER', $6, $7, $8, 'OK', '', \
                 '{\"http.method\":\"GET\"}'::jsonb, '[]'::jsonb)",
        vec![
            project_id.into(),
            "checkout".into(),
            trace_id.into(),
            span_id.into(),
            "GET /checkout".into(),
            t.into(),
            (t + Duration::milliseconds(42)).into(),
            42.0_f64.into(),
        ],
    ))
    .await
    .expect("insert otel_spans");

    let service = ObservabilityService::new(db.clone());
    let events = service
        .query(EventFilters {
            project_id,
            kinds: [EventKind::Span].iter().copied().collect(),
            from: None,
            to: None,
            deployment_id: None,
            environment_id: None,
            search: None,
            limit: 50,
            hide_bots: None,
        })
        .await
        .expect("query spans");

    assert_eq!(events.len(), 1);
    let ObservabilityEvent::Span(row) = &events[0] else {
        panic!("expected span row");
    };
    assert_eq!(row.service, "checkout");
    assert_eq!(row.operation, "GET /checkout");
    assert_eq!(row.trace_id, trace_id);
    assert_eq!(row.span_id, span_id);
    assert_eq!(row.duration_ms, Some(42.0));
    assert_eq!(row.status.as_deref(), Some("OK"));
    assert_eq!(
        row.attributes.get("http.method").and_then(|v| v.as_str()),
        Some("GET"),
    );
}

#[tokio::test]
async fn merge_interleaves_spans_with_other_kinds() {
    let test_db = TestDatabase::with_migrations()
        .await
        .expect("test db with migrations");
    let db = test_db.connection_arc();
    let project_id = create_test_project(&db).await;
    let group_id = create_error_group(&db, project_id).await;

    // Four events at strictly descending timestamps, one per kind, so the
    // k-way merge is forced to interleave across all four sources.
    let t_request = Utc.with_ymd_and_hms(2026, 5, 1, 12, 5, 0).unwrap();
    let t_span = Utc.with_ymd_and_hms(2026, 5, 1, 12, 3, 0).unwrap();
    let t_error = Utc.with_ymd_and_hms(2026, 5, 1, 12, 2, 0).unwrap();
    let t_revenue = Utc.with_ymd_and_hms(2026, 5, 1, 12, 1, 0).unwrap();

    proxy_logs::ActiveModel {
        timestamp: Set(t_request),
        method: Set("GET".into()),
        path: Set("/all".into()),
        host: Set("x.test".into()),
        status_code: Set(200),
        request_source: Set("proxy".into()),
        is_system_request: Set(false),
        routing_status: Set("routed".into()),
        project_id: Set(Some(project_id)),
        request_id: Set("req-all".into()),
        created_date: Set(t_request.date_naive()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert proxy log");

    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO otel_spans \
         (project_id, deployment_id, service_name, trace_id, span_id, parent_span_id, \
          name, kind, start_time, end_time, duration_ms, status_code, status_message, \
          attributes, events) \
         VALUES ($1, NULL, 'web', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', \
                 '2222222222222222', NULL, 'GET /', 'SERVER', $2, $3, 12.0, \
                 'OK', '', '{}'::jsonb, '[]'::jsonb)",
        vec![
            project_id.into(),
            t_span.into(),
            (t_span + Duration::milliseconds(12)).into(),
        ],
    ))
    .await
    .expect("insert span");

    error_events::ActiveModel {
        error_group_id: Set(group_id),
        project_id: Set(project_id),
        fingerprint_hash: Set("merge-fp".into()),
        timestamp: Set(t_error),
        exception_type: Set("Boom".into()),
        source: Set(Some("custom".into())),
        created_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert error");

    revenue_events::ActiveModel {
        project_id: Set(project_id),
        integration_id: Set(0),
        provider: Set("stripe".into()),
        provider_event_id: Set(format!("evt_{}", Uuid::new_v4())),
        event_type: Set("invoice.paid".into()),
        amount_minor: Set(Some(1000)),
        currency: Set(Some("usd".into())),
        occurred_at: Set(t_revenue),
        payload: Set(serde_json::json!({})),
        created_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert revenue");

    let service = ObservabilityService::new(db.clone());
    let events = service
        .query(EventFilters {
            project_id,
            kinds: EventKind::ALL.iter().copied().collect(),
            from: None,
            to: None,
            deployment_id: None,
            environment_id: None,
            search: None,
            limit: 50,
            hide_bots: None,
        })
        .await
        .expect("merge query");

    assert_eq!(events.len(), 4);
    // Strictly descending order — proves the k-way merge picked the right
    // head from each of the four streams.
    let kinds: Vec<EventKind> = events.iter().map(|e| e.kind()).collect();
    assert_eq!(
        kinds,
        vec![
            EventKind::Request,
            EventKind::Span,
            EventKind::Error,
            EventKind::Revenue,
        ]
    );
    for w in events.windows(2) {
        assert!(
            w[0].ts() >= w[1].ts(),
            "merge order broke between {:?} and {:?}",
            w[0].kind(),
            w[1].kind()
        );
    }
}
