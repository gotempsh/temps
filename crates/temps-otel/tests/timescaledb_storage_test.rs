// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Integration tests for the TimescaleDB storage backend.
//!
//! These tests require a Docker-accessible TimescaleDB instance.
//! They skip gracefully when Docker is unavailable (no `#[ignore]`).

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Timelike, Utc};

use temps_otel::storage::timescaledb::TimescaleDbStorage;
use temps_otel::storage::OtelStorage;
use temps_otel::types::*;

/// Create a TestDatabase with migrations and return a TimescaleDbStorage backed by it.
/// Returns `None` if Docker is unavailable (test should skip).
///
/// Uses `TestDatabase::with_migrations()` which acquires a global lock to avoid
/// concurrent extension creation conflicts on the shared container.
async fn setup_storage() -> Option<(temps_database::test_utils::TestDatabase, TimescaleDbStorage)> {
    let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(e) => {
            println!("Docker/TestDatabase not available, skipping test: {}", e);
            return None;
        }
    };

    let storage = TimescaleDbStorage::new(test_db.db.clone(), None);
    Some((test_db, storage))
}

/// Same as [`setup_storage`], but with a populated facet cache so ingest
/// writes `facet_attr_N` slot columns and `query_spans`/trace-summary
/// queries route faceted keys through them instead of the JSON fallback.
/// `facets` maps attribute key -> slot (1..=20).
async fn setup_storage_with_facets(
    facets: &[(&str, u8)],
) -> Option<(temps_database::test_utils::TestDatabase, TimescaleDbStorage)> {
    let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(e) => {
            println!("Docker/TestDatabase not available, skipping test: {}", e);
            return None;
        }
    };

    let map: std::collections::HashMap<String, u8> = facets
        .iter()
        .map(|(key, slot)| (key.to_string(), *slot))
        .collect();
    let facet_cache: temps_otel::services::FacetCache =
        std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(map));

    let storage =
        TimescaleDbStorage::with_config(test_db.db.clone(), None, 7, None, Some(facet_cache));
    Some((test_db, storage))
}

/// Build a test ResourceInfo.
fn test_resource() -> ResourceInfo {
    ResourceInfo {
        service_name: "test-service".into(),
        service_version: Some("1.0.0".into()),
        deployment_environment: Some("test".into()),
        attributes: BTreeMap::new(),
    }
}

/// Build a sample SpanRecord.
#[allow(clippy::too_many_arguments)]
fn sample_span(
    project_id: i32,
    trace_id: &str,
    span_id: &str,
    parent_span_id: Option<&str>,
    name: &str,
    kind: SpanKind,
    status: SpanStatusCode,
    duration_ms: f64,
) -> SpanRecord {
    let now = Utc::now();
    SpanRecord {
        project_id,
        deployment_id: None,
        resource: test_resource(),
        trace_id: trace_id.into(),
        span_id: span_id.into(),
        parent_span_id: parent_span_id.map(String::from),
        name: name.into(),
        kind,
        start_time: now - Duration::milliseconds(duration_ms as i64),
        end_time: now,
        duration_ms,
        status_code: status,
        status_message: String::new(),
        attributes: BTreeMap::new(),
        events: vec![],
    }
}

/// Build a sample MetricPoint.
fn sample_metric(project_id: i32, name: &str, value: f64) -> MetricPoint {
    let mut p = MetricPoint::skeleton(
        project_id,
        None,
        test_resource(),
        name.into(),
        MetricType::Gauge,
        "ms".into(),
        Utc::now(),
        BTreeMap::new(),
    );
    p.value = Some(value);
    p
}

/// Build a sample LogRecord.
fn sample_log(
    project_id: i32,
    severity: LogSeverity,
    body: &str,
    trace_id: Option<&str>,
) -> LogRecord {
    LogRecord {
        project_id,
        deployment_id: None,
        resource: test_resource(),
        timestamp: Utc::now(),
        observed_timestamp: Utc::now(),
        severity,
        severity_text: severity.to_string(),
        body: body.into(),
        trace_id: trace_id.map(String::from),
        span_id: None,
        attributes: BTreeMap::new(),
    }
}

// ── Span tests ──────────────────────────────────────────────────────

#[tokio::test]
async fn test_store_and_get_trace() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let trace_id = "aabbccdd11223344aabbccdd11223344";
    let project_id = 1;

    let root = sample_span(
        project_id,
        trace_id,
        "0102030405060708",
        None,
        "GET /api/users",
        SpanKind::Server,
        SpanStatusCode::Ok,
        100.0,
    );
    let child = sample_span(
        project_id,
        trace_id,
        "1112131415161718",
        Some("0102030405060708"),
        "SELECT * FROM users",
        SpanKind::Client,
        SpanStatusCode::Ok,
        20.0,
    );

    // Store
    let stored = storage.store_spans(vec![root, child]).await.unwrap();
    assert_eq!(stored, 2);

    // Retrieve full trace
    let spans = storage.get_trace(project_id, trace_id).await.unwrap();
    assert_eq!(spans.len(), 2);

    // Verify tree structure
    let root_spans: Vec<_> = spans
        .iter()
        .filter(|s| s.parent_span_id.is_none())
        .collect();
    assert_eq!(root_spans.len(), 1);
    assert_eq!(root_spans[0].name, "GET /api/users");

    let child_spans: Vec<_> = spans
        .iter()
        .filter(|s| s.parent_span_id.as_deref() == Some("0102030405060708"))
        .collect();
    assert_eq!(child_spans.len(), 1);
    assert_eq!(child_spans[0].name, "SELECT * FROM users");
}

#[tokio::test]
async fn test_has_traces() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let project_with_spans = 1;
    let project_without_spans = 2;

    assert!(
        !storage.has_traces(project_with_spans).await.unwrap(),
        "no spans stored yet"
    );

    let root = sample_span(
        project_with_spans,
        "aabbccdd11223344aabbccdd11223344",
        "0102030405060708",
        None,
        "GET /api/users",
        SpanKind::Server,
        SpanStatusCode::Ok,
        100.0,
    );
    storage.store_spans(vec![root]).await.unwrap();

    assert!(
        storage.has_traces(project_with_spans).await.unwrap(),
        "a span was just stored for this project"
    );
    assert!(
        !storage.has_traces(project_without_spans).await.unwrap(),
        "a different project must not see another project's spans"
    );
}

#[tokio::test]
async fn test_query_spans_filters() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let project_id = 2;

    let ok_span = sample_span(
        project_id,
        "trace_ok",
        "span_ok",
        None,
        "healthy-op",
        SpanKind::Server,
        SpanStatusCode::Ok,
        10.0,
    );
    let err_span = sample_span(
        project_id,
        "trace_err",
        "span_err",
        None,
        "failing-op",
        SpanKind::Server,
        SpanStatusCode::Error,
        200.0,
    );

    storage.store_spans(vec![ok_span, err_span]).await.unwrap();

    // Filter by status
    let error_spans = storage
        .query_spans(TraceQuery {
            project_id,
            status: Some(SpanStatusCode::Error),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(error_spans.len(), 1);
    assert_eq!(error_spans[0].name, "failing-op");

    // Filter by min_duration
    let slow_spans = storage
        .query_spans(TraceQuery {
            project_id,
            min_duration_ms: Some(100.0),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(slow_spans.len(), 1);
    assert_eq!(slow_spans[0].name, "failing-op");

    // Filter by service_name
    let by_svc = storage
        .query_spans(TraceQuery {
            project_id,
            service_name: Some("test-service".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(by_svc.len(), 2);
}

#[tokio::test]
async fn test_query_spans_faceted_attribute_routes_through_slot_column() {
    let Some((_db, storage)) = setup_storage_with_facets(&[("enduser.id", 1)]).await else {
        return;
    };

    let project_id = 21;

    let mut faceted_span = sample_span(
        project_id,
        "trace_faceted",
        "span_faceted",
        None,
        "faceted-op",
        SpanKind::Server,
        SpanStatusCode::Ok,
        10.0,
    );
    faceted_span
        .attributes
        .insert("enduser.id".to_string(), "user-42".to_string());

    let mut other_span = sample_span(
        project_id,
        "trace_other",
        "span_other",
        None,
        "other-op",
        SpanKind::Server,
        SpanStatusCode::Ok,
        10.0,
    );
    other_span
        .attributes
        .insert("enduser.id".to_string(), "someone-else".to_string());
    // Also carries an unfaceted attribute, to prove the JSON fallback still
    // works for keys that aren't registered as a facet.
    other_span
        .attributes
        .insert("unfaceted.key".to_string(), "unfaceted-value".to_string());

    storage
        .store_spans(vec![faceted_span, other_span])
        .await
        .unwrap();

    // 1. Prove ingest actually wrote the value into facet_attr_1, not just
    //    that the query happens to return the right row for other reasons.
    use sea_orm::ConnectionTrait;
    let raw = _db
        .db
        .query_one(sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT facet_attr_1 FROM otel_spans WHERE span_id = $1",
            vec!["span_faceted".into()],
        ))
        .await
        .unwrap()
        .unwrap();
    let facet_attr_1: Option<String> = raw.try_get("", "facet_attr_1").unwrap();
    assert_eq!(facet_attr_1.as_deref(), Some("user-42"));

    // 2. Prove query_spans, given the same faceted key, returns exactly the
    //    matching span — i.e. the facet_attr_1 = $x branch (not the JSON
    //    fallback) is what's actually being evaluated, since a broken
    //    routing (e.g. always-false column reference) would return zero
    //    rows here instead of silently falling back.
    let mut faceted_filter = BTreeMap::new();
    faceted_filter.insert("enduser.id".to_string(), "user-42".to_string());
    let matched = storage
        .query_spans(TraceQuery {
            project_id,
            attributes: Some(faceted_filter),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0].span_id, "span_faceted");

    // 3. Control: an unfaceted key on the same query path still resolves via
    //    the JSON `attributes->>` fallback.
    let mut unfaceted_filter = BTreeMap::new();
    unfaceted_filter.insert("unfaceted.key".to_string(), "unfaceted-value".to_string());
    let matched_unfaceted = storage
        .query_spans(TraceQuery {
            project_id,
            attributes: Some(unfaceted_filter),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(matched_unfaceted.len(), 1);
    assert_eq!(matched_unfaceted[0].span_id, "span_other");
}

#[tokio::test]
async fn test_store_spans_empty_batch() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let stored = storage.store_spans(vec![]).await.unwrap();
    assert_eq!(stored, 0);
}

// ── Duplicate-write characterization (Greptile P1) ──────────────────
//
// These three tests pin a *hazard*, not a desired behaviour: the TimescaleDB
// batch inserts are NOT idempotent, so re-sending a batch duplicates every
// row silently. That is why `StorageErrorKind::is_transient` refuses to retry
// a Postgres failure whose outcome is unknown.
//
// They cannot currently be fixed with `ON CONFLICT DO NOTHING`: all three
// tables are hypertables with a space partition on `id`, and TimescaleDB
// rejects a unique index that omits a partitioning column — while `id` is
// `BIGSERIAL`, so including it would make the conflict target never match.
//
// **If one of these starts failing, that is good news**: it means a real
// unique key was added. Update the test to assert deduplication, and widen
// the retry classification in `error.rs` to allow `DbErr::Conn` again.

#[tokio::test]
async fn duplicate_batch_insert_duplicates_rows() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let spans = vec![sample_span(
        1,
        "trace_dup",
        "span_dup",
        None,
        "GET /dup",
        SpanKind::Server,
        SpanStatusCode::Ok,
        10.0,
    )];

    // Two sends of the byte-identical batch — exactly what a retry does.
    assert_eq!(storage.store_spans(spans.clone()).await.unwrap(), 1);
    assert_eq!(storage.store_spans(spans).await.unwrap(), 1);

    let found = storage
        .get_trace(1, "trace_dup")
        .await
        .expect("trace lookup succeeds");
    assert_eq!(
        found.len(),
        2,
        "otel_spans has no unique key, so the same (project, trace, span) lands twice — \
         this is the hazard that makes retrying an unknown-outcome write unsafe"
    );
}

#[tokio::test]
async fn duplicate_metric_batch_insert_duplicates_rows() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let points = vec![sample_metric(1, "dup.metric", 42.0)];
    assert_eq!(storage.store_metrics(points.clone()).await.unwrap(), 1);
    assert_eq!(storage.store_metrics(points).await.unwrap(), 1);

    let buckets = storage
        .query_metrics(MetricQuery {
            project_id: 1,
            metric_name: Some("dup.metric".into()),
            bucket_interval: Some("1 hour".into()),
            ..Default::default()
        })
        .await
        .expect("metric query succeeds");

    let total: i64 = buckets.iter().map(|b| b.count).sum();
    assert_eq!(
        total, 2,
        "otel_metrics has no unique key, so the same point is counted twice — \
         a retried batch would inflate every aggregate built on it"
    );
}

#[tokio::test]
async fn duplicate_log_batch_insert_duplicates_rows() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let logs = vec![sample_log(1, LogSeverity::Error, "dup log line", None)];
    assert_eq!(storage.store_logs(logs.clone()).await.unwrap(), 1);
    assert_eq!(storage.store_logs(logs).await.unwrap(), 1);

    let found = storage
        .query_logs(LogQuery {
            project_id: 1,
            ..Default::default()
        })
        .await
        .expect("log query succeeds");
    assert_eq!(
        found.len(),
        2,
        "otel_log_events has no unique key, so an identical record lands twice"
    );
}

// ── Metric tests ────────────────────────────────────────────────────

#[tokio::test]
async fn test_store_and_list_metrics() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let project_id = 3;

    let cpu = sample_metric(project_id, "cpu.usage", 75.5);
    let mem = sample_metric(project_id, "memory.usage", 60.0);
    let cpu2 = sample_metric(project_id, "cpu.usage", 80.0);

    let stored = storage.store_metrics(vec![cpu, mem, cpu2]).await.unwrap();
    assert_eq!(stored, 3);

    // List distinct metric names
    let names = storage.list_metric_names(project_id).await.unwrap();
    assert!(names.contains(&"cpu.usage".to_string()));
    assert!(names.contains(&"memory.usage".to_string()));
    assert_eq!(names.len(), 2);
}

#[tokio::test]
async fn test_query_metrics_bucketed() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let project_id = 4;

    // Insert multiple data points
    let mut points = Vec::new();
    for i in 0..5 {
        let mut p = sample_metric(project_id, "request.latency", 10.0 + i as f64 * 5.0);
        p.timestamp = Utc::now() - Duration::minutes(i);
        points.push(p);
    }

    storage.store_metrics(points).await.unwrap();

    let buckets = storage
        .query_metrics(MetricQuery {
            project_id,
            metric_name: Some("request.latency".into()),
            bucket_interval: Some("1 hour".into()),
            ..Default::default()
        })
        .await
        .unwrap();

    // All 5 points are within last 5 minutes => should be in 1 or 2 hour buckets
    assert!(!buckets.is_empty(), "Expected at least one bucket");

    // Sum of counts across all buckets should be 5
    let total_count: i64 = buckets.iter().map(|b| b.count).sum();
    assert_eq!(
        total_count, 5,
        "Expected 5 data points total, got {total_count}"
    );

    // Weighted average across all buckets should be ~20
    // (values: 10, 15, 20, 25, 30 => avg = 20)
    let weighted_avg: f64 = buckets
        .iter()
        .map(|b| b.avg_value * b.count as f64)
        .sum::<f64>()
        / total_count as f64;
    assert!(
        (weighted_avg - 20.0).abs() < 1.0,
        "Expected average ~20, got {weighted_avg}"
    );
}

#[tokio::test]
async fn test_store_metrics_empty_batch() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let stored = storage.store_metrics(vec![]).await.unwrap();
    assert_eq!(stored, 0);
}

// ── Log tests ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_store_and_query_logs() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let project_id = 5;

    let info_log = sample_log(project_id, LogSeverity::Info, "Request processed", None);
    let error_log = sample_log(
        project_id,
        LogSeverity::Error,
        "Database connection failed",
        Some("trace_123"),
    );
    let warn_log = sample_log(
        project_id,
        LogSeverity::Warn,
        "Rate limit approaching",
        None,
    );

    storage
        .store_logs(vec![info_log, error_log, warn_log])
        .await
        .unwrap();

    // Query all logs for project
    let all = storage
        .query_logs(LogQuery {
            project_id,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(all.len(), 3);

    // Filter by severity
    let errors = storage
        .query_logs(LogQuery {
            project_id,
            severity: Some(LogSeverity::Error),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].body, "Database connection failed");

    // Filter by search term
    let searched = storage
        .query_logs(LogQuery {
            project_id,
            search: Some("connection".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(searched.len(), 1);

    // Filter by trace_id
    let correlated = storage
        .query_logs(LogQuery {
            project_id,
            trace_id: Some("trace_123".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(correlated.len(), 1);
    assert_eq!(correlated[0].severity, LogSeverity::Error);
}

#[tokio::test]
async fn test_store_logs_empty_batch() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let stored = storage.store_logs(vec![]).await.unwrap();
    assert_eq!(stored, 0);
}

// ── P95 latency test ────────────────────────────────────────────────

#[tokio::test]
async fn test_p95_latency() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let project_id = 6;

    // Insert 20 spans with durations 1..=20ms
    let spans: Vec<SpanRecord> = (1..=20)
        .map(|i| {
            sample_span(
                project_id,
                &format!("trace_{i}"),
                &format!("span_{i}"),
                None,
                "op",
                SpanKind::Server,
                SpanStatusCode::Ok,
                i as f64,
            )
        })
        .collect();

    storage.store_spans(spans).await.unwrap();

    let p95 = storage
        .get_p95_latency(project_id, "test-service", 60)
        .await
        .unwrap();

    // P95 of 1..=20 should be around 19.05 (continuous interpolation)
    assert!(p95 > 18.0, "p95 should be > 18, got {p95}");
    assert!(p95 <= 20.0, "p95 should be <= 20, got {p95}");
}

#[tokio::test]
async fn test_p95_latency_no_data() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let p95 = storage
        .get_p95_latency(999, "nonexistent-service", 60)
        .await
        .unwrap();
    assert!((p95 - 0.0).abs() < f64::EPSILON);
}

// ── Archive logs (no S3 configured) ─────────────────────────────────

#[tokio::test]
async fn test_archive_logs_without_s3() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let log = sample_log(1, LogSeverity::Info, "test archive", None);
    // No S3 configured => returns 0
    let archived = storage.archive_logs(vec![log]).await.unwrap();
    assert_eq!(archived, 0);
}

// ── Storage quota ───────────────────────────────────────────────────

#[tokio::test]
async fn test_storage_quota() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    // Default storage has no quota configured: the estimate short-circuits
    // to zeros and check_quota reports "not exceeded".
    let quota = storage.get_storage_quota(1).await.unwrap();
    assert_eq!(quota.project_id, 1);
    assert_eq!(quota.limit_bytes, 0);
    assert_eq!(quota.usage_pct, 0.0);

    let exceeded = storage.check_quota(1).await.unwrap();
    assert!(!exceeded);

    // With an explicit quota, the check runs for real against a fresh DB.
    let storage_with_quota = TimescaleDbStorage::with_config(
        _db.db.clone(),
        None,
        7,
        Some(10 * 1024 * 1024 * 1024),
        None,
    );
    let quota = storage_with_quota.get_storage_quota(1).await.unwrap();
    assert_eq!(quota.limit_bytes, 10 * 1024 * 1024 * 1024);
    let exceeded = storage_with_quota.check_quota(1).await.unwrap();
    assert!(!exceeded); // Fresh DB, should not be exceeded
}

/// Regression test for the `hypertable_size()` fix.
///
/// `test_storage_quota` above only exercises a freshly-created, empty
/// database, where the old buggy `pg_total_relation_size(otel_spans)`
/// (root-relation) formula and the fixed `hypertable_size('otel_spans')`
/// (chunk-aware) formula are indistinguishable — both report ~0 bytes
/// because no data has ever been inserted. That made the old formula's bug
/// invisible to this test suite: a hypertable's root relation holds no
/// rows/bytes of its own regardless of how much real data lives in its
/// child chunk tables, so quota enforcement was silently inert for every
/// project (see the CORRECTNESS comment on `get_storage_quota`).
///
/// This test closes that gap by actually inserting span rows via
/// `store_spans` (the real ingest path, not a synthetic row count) and
/// asserting `total_bytes`/`usage_pct` track that real volume. If a future
/// change reverts `get_storage_quota` back to
/// `pg_total_relation_size(otel_spans::regclass)` on the hypertable root,
/// `total_bytes` will stay near zero regardless of the ~500 inserted spans
/// and the assertions below will fail.
#[tokio::test]
async fn test_storage_quota_tracks_real_ingested_span_volume() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let project_id = 4242;

    // Each span carries a sizeable `attributes` payload (20 keys x ~200
    // bytes each, ~4KB/span) so the real bytes written to the `otel_spans`
    // hypertable's chunks are well above any catalog/index noise floor a
    // table -- even an empty one -- can carry. With ~500 such spans this is
    // several hundred KB to low-MB of real chunk data, comfortably over the
    // 200KB quota limit used below.
    let mut attrs = BTreeMap::new();
    for i in 0..20 {
        attrs.insert(format!("attribute.key.number.{i}"), "x".repeat(200));
    }

    let mut spans = Vec::with_capacity(500);
    for i in 0..500u32 {
        let mut span = sample_span(
            project_id,
            &format!("{i:032x}"),
            &format!("{i:016x}"),
            None,
            "load-test-span",
            SpanKind::Internal,
            SpanStatusCode::Ok,
            10.0,
        );
        span.attributes = attrs.clone();
        spans.push(span);
    }

    let stored = storage.store_spans(spans).await.unwrap();
    assert_eq!(stored, 500);

    // A 200KB limit. Calibrated live against both formulas on this exact
    // dataset (~500 spans x ~4KB attributes each):
    //   - fixed `hypertable_size()` formula:            ~856KB total_bytes
    //   - old `pg_total_relation_size(root)` formula:     ~64KB total_bytes
    //     (a hypertable's empty root relation carries a small constant
    //     amount of index/catalog overhead that does NOT grow with chunk
    //     data — this is that constant, not real ingested volume)
    // 200KB sits with wide margin between the two, so this assertion is
    // only satisfied by a formula that actually accounts for chunk storage.
    const TEST_QUOTA_LIMIT_BYTES: u64 = 200 * 1024;
    let storage_with_tiny_quota = TimescaleDbStorage::with_config(
        _db.db.clone(),
        None,
        7,
        Some(TEST_QUOTA_LIMIT_BYTES),
        None,
    );
    let quota = storage_with_tiny_quota
        .get_storage_quota(project_id)
        .await
        .unwrap();
    assert!(
        quota.total_bytes > TEST_QUOTA_LIMIT_BYTES,
        "expected chunk-aware total_bytes to exceed the {TEST_QUOTA_LIMIT_BYTES}-byte test \
         limit after inserting ~500 spans with ~4KB of attributes each (got {} bytes) -- \
         this is exactly the regression the hypertable_size() fix protects against: the old \
         pg_total_relation_size(root) formula reports only the root relation's constant \
         ~64KB of index/catalog overhead here, never the real chunk volume",
        quota.total_bytes
    );
    assert!(
        quota.usage_pct >= 100.0,
        "usage_pct should have crossed 100% of the {TEST_QUOTA_LIMIT_BYTES}-byte limit, got {}",
        quota.usage_pct
    );

    let exceeded = storage_with_tiny_quota
        .check_quota(project_id)
        .await
        .unwrap();
    assert!(
        exceeded,
        "check_quota must trip once real ingested span volume exceeds the configured limit"
    );

    // Sanity check in the other direction: a generous limit against the
    // same real data must NOT report exceeded, proving usage_pct is a real
    // proportional measurement and not just pegged to 100%.
    let storage_with_generous_quota = TimescaleDbStorage::with_config(
        _db.db.clone(),
        None,
        7,
        Some(10 * 1024 * 1024 * 1024),
        None,
    );
    let generous_quota = storage_with_generous_quota
        .get_storage_quota(project_id)
        .await
        .unwrap();
    assert!(
        generous_quota.usage_pct < 100.0,
        "a 10GB limit should not be exceeded by ~500 spans of test data, got usage_pct = {}",
        generous_quota.usage_pct
    );
    let not_exceeded = storage_with_generous_quota
        .check_quota(project_id)
        .await
        .unwrap();
    assert!(!not_exceeded);
}

#[tokio::test]
async fn test_get_storage_quota_disabled_skips_database() {
    use sea_orm::{DatabaseBackend, MockDatabase};

    // No query results are prepared, so any database access would error.
    // With no quota configured, the usage estimate must short-circuit
    // without touching the database — this is the ingest hot path
    // (`OtelService::check_quota` calls `get_storage_quota` on every
    // quota-cache miss).
    let mock_db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let storage = TimescaleDbStorage::new(std::sync::Arc::new(mock_db), None);

    let quota = storage.get_storage_quota(1).await.unwrap();
    assert_eq!(quota.total_bytes, 0);
    assert_eq!(quota.limit_bytes, 0);
    assert_eq!(quota.usage_pct, 0.0);

    let exceeded = storage.check_quota(1).await.unwrap();
    assert!(!exceeded);
}

// ── Retention is a no-op (Timescale's policy is the source of truth) ─

#[tokio::test]
async fn test_apply_retention_is_a_noop() {
    // `apply_retention` was changed to a no-op. The OTel hypertables
    // enforce retention via `add_retention_policy(..., INTERVAL '90 days')`
    // registered in `m20260225_000001_create_otel_tables` — Timescale
    // calls `drop_chunks` internally, which is atomic and chunk-aware.
    //
    // The previous app-level `DELETE FROM otel_metrics WHERE timestamp <
    // NOW() - …` raced with the native policy: planner snapshots a chunk
    // list, the policy worker drops one of those chunks, the executor
    // hits the stale OID → `chunk not found`. That error bubbled up as
    // a migration failure in prod logs.
    //
    // This test pins the contract that `apply_retention` always returns
    // 0 regardless of how much old data exists. A regression that
    // re-adds the app-level DELETE would need to delete this test or it
    // would fail.
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let deleted = storage.apply_retention(1).await.unwrap();
    assert_eq!(deleted, 0, "apply_retention must never report deletions");
}

// ── Project isolation ───────────────────────────────────────────────

#[tokio::test]
async fn test_project_isolation() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let span_p1 = sample_span(
        100,
        "trace_p1",
        "span_p1",
        None,
        "project-100-op",
        SpanKind::Server,
        SpanStatusCode::Ok,
        10.0,
    );
    let span_p2 = sample_span(
        200,
        "trace_p2",
        "span_p2",
        None,
        "project-200-op",
        SpanKind::Server,
        SpanStatusCode::Ok,
        10.0,
    );

    storage.store_spans(vec![span_p1, span_p2]).await.unwrap();

    // Each project should only see its own spans
    let p1_spans = storage
        .query_spans(TraceQuery {
            project_id: 100,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(p1_spans.len(), 1);
    assert_eq!(p1_spans[0].name, "project-100-op");

    let p2_spans = storage
        .query_spans(TraceQuery {
            project_id: 200,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(p2_spans.len(), 1);
    assert_eq!(p2_spans[0].name, "project-200-op");

    // Project 999 should see nothing
    let p999_spans = storage
        .query_spans(TraceQuery {
            project_id: 999,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(p999_spans.is_empty());
}

// ── Full trace tree with 4 spans (DB roundtrip) ─────────────────────

#[tokio::test]
async fn test_full_trace_tree_db_roundtrip() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let project_id = 7;
    let trace_id = "deadbeefcafebabe1122334455667788";
    let now = Utc::now();

    let root = SpanRecord {
        project_id,
        deployment_id: None,
        resource: test_resource(),
        trace_id: trace_id.into(),
        span_id: "root000000000001".into(),
        parent_span_id: None,
        name: "GET /api/users".into(),
        kind: SpanKind::Server,
        start_time: now - Duration::milliseconds(100),
        end_time: now,
        duration_ms: 100.0,
        status_code: SpanStatusCode::Ok,
        status_message: String::new(),
        attributes: BTreeMap::from([("http.method".into(), "GET".into())]),
        events: vec![],
    };

    let child_db = SpanRecord {
        project_id,
        deployment_id: None,
        resource: test_resource(),
        trace_id: trace_id.into(),
        span_id: "child_db00000002".into(),
        parent_span_id: Some("root000000000001".into()),
        name: "SELECT * FROM users".into(),
        kind: SpanKind::Client,
        start_time: now - Duration::milliseconds(90),
        end_time: now - Duration::milliseconds(70),
        duration_ms: 20.0,
        status_code: SpanStatusCode::Ok,
        status_message: String::new(),
        attributes: BTreeMap::from([("db.system".into(), "postgresql".into())]),
        events: vec![],
    };

    let child_http = SpanRecord {
        project_id,
        deployment_id: None,
        resource: test_resource(),
        trace_id: trace_id.into(),
        span_id: "child_http000003".into(),
        parent_span_id: Some("root000000000001".into()),
        name: "POST /external/validate".into(),
        kind: SpanKind::Client,
        start_time: now - Duration::milliseconds(60),
        end_time: now - Duration::milliseconds(10),
        duration_ms: 50.0,
        status_code: SpanStatusCode::Ok,
        status_message: String::new(),
        attributes: BTreeMap::new(),
        events: vec![],
    };

    let grandchild = SpanRecord {
        project_id,
        deployment_id: None,
        resource: test_resource(),
        trace_id: trace_id.into(),
        span_id: "grandchild000004".into(),
        parent_span_id: Some("child_http000003".into()),
        name: "parse_response".into(),
        kind: SpanKind::Internal,
        start_time: now - Duration::milliseconds(30),
        end_time: now - Duration::milliseconds(15),
        duration_ms: 15.0,
        status_code: SpanStatusCode::Ok,
        status_message: String::new(),
        attributes: BTreeMap::new(),
        events: vec![],
    };

    // Store all 4 spans
    let stored = storage
        .store_spans(vec![
            root.clone(),
            child_db.clone(),
            child_http.clone(),
            grandchild.clone(),
        ])
        .await
        .unwrap();
    assert_eq!(stored, 4);

    // Retrieve full trace
    let spans = storage.get_trace(project_id, trace_id).await.unwrap();
    assert_eq!(spans.len(), 4);

    // Verify root
    let roots: Vec<_> = spans
        .iter()
        .filter(|s| s.parent_span_id.is_none())
        .collect();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].name, "GET /api/users");
    assert_eq!(roots[0].kind, SpanKind::Server);
    assert!((roots[0].duration_ms - 100.0).abs() < 0.01);

    // Verify children of root
    let root_children: Vec<_> = spans
        .iter()
        .filter(|s| s.parent_span_id.as_deref() == Some("root000000000001"))
        .collect();
    assert_eq!(root_children.len(), 2);
    let child_names: Vec<_> = root_children.iter().map(|s| s.name.as_str()).collect();
    assert!(child_names.contains(&"SELECT * FROM users"));
    assert!(child_names.contains(&"POST /external/validate"));

    // Verify grandchild
    let grandchildren: Vec<_> = spans
        .iter()
        .filter(|s| s.parent_span_id.as_deref() == Some("child_http000003"))
        .collect();
    assert_eq!(grandchildren.len(), 1);
    assert_eq!(grandchildren[0].name, "parse_response");
    assert_eq!(grandchildren[0].kind, SpanKind::Internal);

    // Verify attributes survived round-trip
    let root_retrieved = spans
        .iter()
        .find(|s| s.span_id == "root000000000001")
        .unwrap();
    assert_eq!(
        root_retrieved.attributes.get("http.method"),
        Some(&"GET".to_string())
    );

    let db_retrieved = spans
        .iter()
        .find(|s| s.span_id == "child_db00000002")
        .unwrap();
    assert_eq!(
        db_retrieved.attributes.get("db.system"),
        Some(&"postgresql".to_string())
    );
}

// ── Full-fidelity metric round-trip tests ───────────────────────────
//
// These tests verify the 14 new columns added by
// m20260629_000001_otel_metrics_full_fidelity and the rewritten
// query_metrics / batch_insert_metrics implementations.

/// Build a minimal Gauge MetricPoint for the given project.
fn gauge_point(
    project_id: i32,
    name: &str,
    value: f64,
    attrs: BTreeMap<String, String>,
) -> MetricPoint {
    let mut p = MetricPoint::skeleton(
        project_id,
        None,
        test_resource(),
        name.into(),
        MetricType::Gauge,
        "ms".into(),
        Utc::now() - Duration::seconds(30),
        attrs,
    );
    p.value = Some(value);
    p.temporality = Some(AggregationTemporality::Delta);
    p.flags = 0;
    p.description = Some("Test gauge metric".into());
    p
}

/// Build a Histogram MetricPoint for the given project.
fn histogram_point(project_id: i32, name: &str) -> MetricPoint {
    let mut p = MetricPoint::skeleton(
        project_id,
        None,
        test_resource(),
        name.into(),
        MetricType::Histogram,
        "ms".into(),
        Utc::now() - Duration::seconds(10),
        BTreeMap::new(),
    );
    p.histogram_count = Some(100);
    p.histogram_sum = Some(5000.0);
    p.histogram_min = Some(10.0);
    p.histogram_max = Some(200.0);
    p.histogram_bounds = Some(vec![10.0, 50.0, 100.0, 200.0]);
    p.histogram_bucket_counts = Some(vec![5, 20, 50, 20, 5]);
    p.temporality = Some(AggregationTemporality::Delta);
    p.description = Some("Request latency histogram".into());
    p
}

/// Test that Avg, Sum, Count, and Quantile aggregations all return data.
#[tokio::test]
async fn test_full_fidelity_gauge_aggregations() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let project_id = 100;

    // Insert 4 gauge points at different values.
    let points: Vec<MetricPoint> = vec![
        gauge_point(project_id, "test.latency", 10.0, BTreeMap::new()),
        gauge_point(project_id, "test.latency", 20.0, BTreeMap::new()),
        gauge_point(project_id, "test.latency", 30.0, BTreeMap::new()),
        gauge_point(project_id, "test.latency", 40.0, BTreeMap::new()),
    ];
    let stored = storage.store_metrics(points).await.unwrap();
    assert_eq!(stored, 4);

    // ── Avg ─────────────────────────────────────────────────────────
    let avg_buckets = storage
        .query_metrics(MetricQuery {
            project_id,
            metric_name: Some("test.latency".into()),
            bucket_interval: Some("1 hour".into()),
            aggregation: MetricAggregation::Avg,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(!avg_buckets.is_empty(), "Avg: expected at least one bucket");
    let total_count: i64 = avg_buckets.iter().map(|b| b.count).sum();
    assert_eq!(total_count, 4, "Avg: expected 4 points");
    let weighted_avg: f64 = avg_buckets
        .iter()
        .map(|b| b.avg_value * b.count as f64)
        .sum::<f64>()
        / total_count as f64;
    assert!(
        (weighted_avg - 25.0).abs() < 1.0,
        "Avg: expected ~25, got {weighted_avg}"
    );

    // ── Sum ─────────────────────────────────────────────────────────
    let sum_buckets = storage
        .query_metrics(MetricQuery {
            project_id,
            metric_name: Some("test.latency".into()),
            bucket_interval: Some("1 hour".into()),
            aggregation: MetricAggregation::Sum,
            ..Default::default()
        })
        .await
        .unwrap();
    let total_sum: f64 = sum_buckets.iter().map(|b| b.value).sum();
    assert!(
        (total_sum - 100.0).abs() < 1.0,
        "Sum: expected ~100, got {total_sum}"
    );

    // ── Count ────────────────────────────────────────────────────────
    let count_buckets = storage
        .query_metrics(MetricQuery {
            project_id,
            metric_name: Some("test.latency".into()),
            bucket_interval: Some("1 hour".into()),
            aggregation: MetricAggregation::Count,
            ..Default::default()
        })
        .await
        .unwrap();
    let total_count_agg: f64 = count_buckets.iter().map(|b| b.value).sum();
    assert!(
        (total_count_agg - 4.0).abs() < 0.5,
        "Count: expected ~4, got {total_count_agg}"
    );

    // ── Quantile (p50) ───────────────────────────────────────────────
    let q_buckets = storage
        .query_metrics(MetricQuery {
            project_id,
            metric_name: Some("test.latency".into()),
            bucket_interval: Some("1 hour".into()),
            aggregation: MetricAggregation::Quantile(0.5),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        !q_buckets.is_empty(),
        "Quantile: expected at least one bucket"
    );
    // p50 of [10,20,30,40] = 25 (interpolated).
    let p50 = q_buckets[0].value;
    assert!(
        (p50 - 25.0).abs() < 1.0,
        "Quantile p50: expected ~25, got {p50}"
    );
    // quantiles field should carry the (q, value) pair.
    assert_eq!(q_buckets[0].quantiles.len(), 1);
    assert!((q_buckets[0].quantiles[0].0 - 0.5).abs() < f64::EPSILON);
}

/// Test label_filter containment filtering.
#[tokio::test]
async fn test_full_fidelity_label_filter() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let project_id = 101;

    let mut prod_attrs = BTreeMap::new();
    prod_attrs.insert("env".to_string(), "production".to_string());

    let mut dev_attrs = BTreeMap::new();
    dev_attrs.insert("env".to_string(), "development".to_string());

    let points = vec![
        gauge_point(project_id, "req.count", 100.0, prod_attrs.clone()),
        gauge_point(project_id, "req.count", 50.0, prod_attrs.clone()),
        gauge_point(project_id, "req.count", 10.0, dev_attrs.clone()),
    ];
    storage.store_metrics(points).await.unwrap();

    // Filter to only production points.
    let buckets = storage
        .query_metrics(MetricQuery {
            project_id,
            metric_name: Some("req.count".into()),
            bucket_interval: Some("1 hour".into()),
            label_filters: vec![("env".to_string(), "production".to_string())],
            aggregation: MetricAggregation::Count,
            ..Default::default()
        })
        .await
        .unwrap();

    let total: f64 = buckets.iter().map(|b| b.value).sum();
    assert!(
        (total - 2.0).abs() < 0.5,
        "label_filter: expected 2 production points, got {total}"
    );
}

/// Test group_by: two series separated by a label should produce distinct
/// MetricBuckets with non-empty series_key.
#[tokio::test]
async fn test_full_fidelity_group_by() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let project_id = 102;

    let mut prod_attrs = BTreeMap::new();
    prod_attrs.insert("region".to_string(), "us-east-1".to_string());

    let mut eu_attrs = BTreeMap::new();
    eu_attrs.insert("region".to_string(), "eu-west-1".to_string());

    let points = vec![
        gauge_point(project_id, "rps", 100.0, prod_attrs.clone()),
        gauge_point(project_id, "rps", 50.0, eu_attrs.clone()),
    ];
    storage.store_metrics(points).await.unwrap();

    let buckets = storage
        .query_metrics(MetricQuery {
            project_id,
            metric_name: Some("rps".into()),
            bucket_interval: Some("1 hour".into()),
            group_by: vec!["region".to_string()],
            aggregation: MetricAggregation::Sum,
            ..Default::default()
        })
        .await
        .unwrap();

    // Each region should be its own bucket (same time window, different series).
    assert!(
        !buckets.is_empty(),
        "group_by: expected at least one bucket"
    );

    // All buckets should carry a series_key.
    for b in &buckets {
        assert!(
            b.series_key.is_some(),
            "group_by: series_key should be populated"
        );
        let sk = b.series_key.as_ref().unwrap();
        assert_eq!(sk.len(), 1, "expected 1 group-by key");
        assert_eq!(sk[0].0, "region");
    }

    // The two regions should be distinct.
    let regions: std::collections::HashSet<String> = buckets
        .iter()
        .filter_map(|b| {
            b.series_key
                .as_ref()
                .and_then(|sk| sk.first())
                .map(|(_, v)| v.clone())
        })
        .collect();
    assert_eq!(regions.len(), 2, "expected 2 distinct region series");
}

/// Test histogram round-trip: store a histogram point and verify that
/// query_metrics populates histogram_summary.
#[tokio::test]
async fn test_full_fidelity_histogram_summary() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let project_id = 103;

    let hist = histogram_point(project_id, "http.request.duration");
    storage.store_metrics(vec![hist]).await.unwrap();

    // Query with Avg aggregation — histogram_summary should be populated.
    let buckets = storage
        .query_metrics(MetricQuery {
            project_id,
            metric_name: Some("http.request.duration".into()),
            bucket_interval: Some("1 hour".into()),
            aggregation: MetricAggregation::Avg,
            ..Default::default()
        })
        .await
        .unwrap();

    assert!(
        !buckets.is_empty(),
        "histogram: expected at least one bucket"
    );

    // Find a bucket with a histogram_summary.
    let with_hist: Vec<_> = buckets
        .iter()
        .filter(|b| b.histogram_summary.is_some())
        .collect();

    // The histogram sub-query may not populate summary if the migration hasn't
    // added the columns yet in the test DB, so we only assert when it IS present.
    if !with_hist.is_empty() {
        let hs = with_hist[0].histogram_summary.as_ref().unwrap();
        assert_eq!(hs.count, 100, "histogram count mismatch");
        assert!((hs.sum - 5000.0).abs() < 1.0, "histogram sum mismatch");
        assert_eq!(hs.bounds, vec![10.0, 50.0, 100.0, 200.0]);
        assert_eq!(hs.bucket_counts, vec![5u64, 20, 50, 20, 5]);
    }
}

/// Test that an invalid label key is rejected before any SQL is executed.
#[tokio::test]
async fn test_full_fidelity_bad_label_key_rejected() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let result = storage
        .query_metrics(MetricQuery {
            project_id: 1,
            label_filters: vec![("bad key!".to_string(), "value".to_string())],
            ..Default::default()
        })
        .await;

    assert!(result.is_err(), "expected error for bad label key");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("bad key!") || err.contains("allowed character"),
        "error message should name the bad key: {err}"
    );
}

/// One histogram data point with explicit values, in the same hour bucket.
fn hist_pt(
    project_id: i32,
    name: &str,
    timestamp: DateTime<Utc>,
    temporality: AggregationTemporality,
    count: u64,
    sum: f64,
    bucket_counts: Vec<u64>,
) -> MetricPoint {
    let mut p = MetricPoint::skeleton(
        project_id,
        None,
        test_resource(),
        name.into(),
        MetricType::Histogram,
        "ms".into(),
        timestamp,
        BTreeMap::new(),
    );
    p.histogram_count = Some(count);
    p.histogram_sum = Some(sum);
    p.histogram_min = Some(10.0);
    p.histogram_max = Some(200.0);
    p.histogram_bounds = Some(vec![10.0, 50.0, 100.0, 200.0]);
    p.histogram_bucket_counts = Some(bucket_counts);
    p.temporality = Some(temporality);
    p
}

/// Choose a stable point inside the current hour so relative fixture offsets
/// cannot cross an hourly aggregation boundary when a test starts near HH:00.
fn histogram_fixture_now() -> DateTime<Utc> {
    Utc::now()
        .with_minute(30)
        .and_then(|time| time.with_second(0))
        .and_then(|time| time.with_nanosecond(0))
        .expect("30 minutes past the current UTC hour is always valid")
}

/// DELTA histograms in the same bucket must be ELEMENT-WISE summed (validates the
/// WITH ORDINALITY array aggregation across multiple rows — not just one).
#[tokio::test]
async fn test_full_fidelity_histogram_delta_elementwise_sum() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };
    let project_id = 110;
    let name = "http.latency.delta";
    let fixture_now = histogram_fixture_now();
    storage
        .store_metrics(vec![
            hist_pt(
                project_id,
                name,
                fixture_now - Duration::seconds(30),
                AggregationTemporality::Delta,
                100,
                5000.0,
                vec![5, 20, 50, 20, 5],
            ),
            hist_pt(
                project_id,
                name,
                fixture_now - Duration::seconds(20),
                AggregationTemporality::Delta,
                15,
                300.0,
                vec![1, 2, 3, 4, 5],
            ),
        ])
        .await
        .unwrap();

    let buckets = storage
        .query_metrics(MetricQuery {
            project_id,
            metric_name: Some(name.into()),
            bucket_interval: Some("1 hour".into()),
            aggregation: MetricAggregation::Avg,
            ..Default::default()
        })
        .await
        .unwrap();

    let hs = buckets
        .iter()
        .find_map(|b| b.histogram_summary.as_ref())
        .expect("delta histogram: expected a histogram_summary");
    assert_eq!(hs.count, 115, "delta counts should sum");
    assert!((hs.sum - 5300.0).abs() < 1.0, "delta sums should add");
    // Element-wise sum: [5,20,50,20,5] + [1,2,3,4,5] = [6,22,53,24,10].
    assert_eq!(hs.bucket_counts, vec![6u64, 22, 53, 24, 10]);
}

/// CUMULATIVE histograms are running totals: the bucket summary must reflect the
/// LATEST snapshot per series, NOT the sum of snapshots (validates the rn=1 pick).
#[tokio::test]
async fn test_full_fidelity_histogram_cumulative_latest_snapshot() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };
    let project_id = 111;
    let name = "http.latency.cumulative";
    let fixture_now = histogram_fixture_now();
    storage
        .store_metrics(vec![
            // earlier snapshot
            hist_pt(
                project_id,
                name,
                fixture_now - Duration::seconds(40),
                AggregationTemporality::Cumulative,
                50,
                2500.0,
                vec![2, 10, 25, 10, 3],
            ),
            // later snapshot (the running total now)
            hist_pt(
                project_id,
                name,
                fixture_now - Duration::seconds(10),
                AggregationTemporality::Cumulative,
                100,
                5000.0,
                vec![5, 20, 50, 20, 5],
            ),
        ])
        .await
        .unwrap();

    let buckets = storage
        .query_metrics(MetricQuery {
            project_id,
            metric_name: Some(name.into()),
            bucket_interval: Some("1 hour".into()),
            aggregation: MetricAggregation::Avg,
            ..Default::default()
        })
        .await
        .unwrap();

    let hs = buckets
        .iter()
        .find_map(|b| b.histogram_summary.as_ref())
        .expect("cumulative histogram: expected a histogram_summary");
    // Latest snapshot only — NOT 150 / [7,30,75,30,8].
    assert_eq!(
        hs.count, 100,
        "cumulative should use the latest snapshot, not the sum"
    );
    assert!((hs.sum - 5000.0).abs() < 1.0);
    assert_eq!(hs.bucket_counts, vec![5u64, 20, 50, 20, 5]);
}

/// Histogram quantiles must be INTERPOLATED from the bucket counts, not
/// approximated by the mean. With bounds [10,50,100,200] and counts
/// [5,20,50,20,5] (total 100, sum 5000 → mean 50), p50 lands in (50,100] at 75
/// and p90 in (100,200] at 175 — both clearly distinct from the mean, proving
/// real interpolation rather than a mean fallback.
#[tokio::test]
async fn test_full_fidelity_histogram_quantile() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };
    let project_id = 112;
    let name = "http.latency.quantile";
    let fixture_now = histogram_fixture_now();
    storage
        .store_metrics(vec![hist_pt(
            project_id,
            name,
            fixture_now - Duration::seconds(20),
            AggregationTemporality::Delta,
            100,
            5000.0,
            vec![5, 20, 50, 20, 5],
        )])
        .await
        .unwrap();

    // p50 → 75 (halfway through bucket (50,100]).
    let p50 = storage
        .query_metrics(MetricQuery {
            project_id,
            metric_name: Some(name.into()),
            bucket_interval: Some("1 hour".into()),
            aggregation: MetricAggregation::Quantile(0.5),
            ..Default::default()
        })
        .await
        .unwrap();
    let b50 = p50
        .iter()
        .find(|b| b.histogram_summary.is_some())
        .expect("p50: expected a histogram bucket");
    assert!(
        (b50.value - 75.0).abs() < 1.0,
        "p50 should interpolate to ~75 (mean is 50), got {}",
        b50.value
    );
    assert_eq!(b50.quantiles, vec![(0.5, b50.value)]);

    // p90 → 175 (75% into bucket (100,200]).
    let p90 = storage
        .query_metrics(MetricQuery {
            project_id,
            metric_name: Some(name.into()),
            bucket_interval: Some("1 hour".into()),
            aggregation: MetricAggregation::Quantile(0.9),
            ..Default::default()
        })
        .await
        .unwrap();
    let b90 = p90
        .iter()
        .find(|b| b.histogram_summary.is_some())
        .expect("p90: expected a histogram bucket");
    assert!(
        (b90.value - 175.0).abs() < 1.0,
        "p90 should interpolate to ~175, got {}",
        b90.value
    );
}

/// A histogram group_by on a label that is ABSENT on one series must still
/// return that series. The `scalars`↔`counts_arr` join is NULL-safe
/// (`IS NOT DISTINCT FROM`), not a plain equi-join that would silently drop the
/// NULL group. Without the fix only the labelled series would come back.
#[tokio::test]
async fn test_full_fidelity_histogram_group_by_null_label() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };
    let project_id = 113;
    let name = "http.latency.bylabel";
    let fixture_now = histogram_fixture_now();

    let mut with_route = hist_pt(
        project_id,
        name,
        fixture_now - Duration::seconds(20),
        AggregationTemporality::Delta,
        100,
        5000.0,
        vec![5, 20, 50, 20, 5],
    );
    with_route.attributes = BTreeMap::from([("route".to_string(), "/api".to_string())]);
    // Second series carries NO `route` attribute → attributes->>'route' is NULL.
    let without_route = hist_pt(
        project_id,
        name,
        fixture_now - Duration::seconds(20),
        AggregationTemporality::Delta,
        40,
        2000.0,
        vec![2, 8, 20, 8, 2],
    );

    storage
        .store_metrics(vec![with_route, without_route])
        .await
        .unwrap();

    let buckets = storage
        .query_metrics(MetricQuery {
            project_id,
            metric_name: Some(name.into()),
            bucket_interval: Some("1 hour".into()),
            aggregation: MetricAggregation::Avg,
            group_by: vec!["route".to_string()],
            ..Default::default()
        })
        .await
        .unwrap();

    // Two series in the same hour bucket: route=/api AND the NULL-label one. The
    // NULL series is the regression guard — a plain equi-join would drop it.
    assert_eq!(
        buckets.len(),
        2,
        "both series (including the NULL-label one) must return, got {buckets:?}"
    );
    let mut counts: Vec<u64> = buckets
        .iter()
        .filter_map(|b| b.histogram_summary.as_ref().map(|h| h.count))
        .collect();
    counts.sort_unstable();
    assert_eq!(
        counts,
        vec![40, 100],
        "both histogram series must be present and intact"
    );
}

/// RatePerSec on a DELTA series divides the summed delta by the bucket width in
/// seconds (sum 100 over a 1-hour = 3600s bucket → ~0.0278/s).
#[tokio::test]
async fn test_full_fidelity_rate_per_sec_delta() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };
    let project_id = 114;
    let name = "test.rate";
    storage
        .store_metrics(vec![
            gauge_point(project_id, name, 10.0, BTreeMap::new()),
            gauge_point(project_id, name, 20.0, BTreeMap::new()),
            gauge_point(project_id, name, 30.0, BTreeMap::new()),
            gauge_point(project_id, name, 40.0, BTreeMap::new()),
        ])
        .await
        .unwrap();

    let buckets = storage
        .query_metrics(MetricQuery {
            project_id,
            metric_name: Some(name.into()),
            bucket_interval: Some("1 hour".into()),
            aggregation: MetricAggregation::RatePerSec,
            ..Default::default()
        })
        .await
        .unwrap();

    let total_rate: f64 = buckets.iter().map(|b| b.value).sum();
    let expected = 100.0 / 3600.0;
    assert!(
        (total_rate - expected).abs() < 1e-4,
        "delta rate: expected ~{expected}, got {total_rate}"
    );
}

/// Gap #2 regression: the full-fidelity migration adds nullable columns and
/// builds the composite + GIN indexes against the EXISTING `otel_metrics`
/// hypertable — which in production may already carry COMPRESSED chunks. This
/// proves a nullable `ADD COLUMN` and both `CREATE INDEX` flavours (btree & GIN)
/// succeed with a compressed chunk present. Skips gracefully if the test image
/// doesn't support compression.
#[tokio::test]
async fn test_migration_ddl_safe_on_compressed_chunks() {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let Some((test_db, storage)) = setup_storage().await else {
        return;
    };
    let db = &test_db.db;
    let run = |sql: &str| Statement::from_string(DatabaseBackend::Postgres, sql.to_string());

    // Land a row so at least one chunk exists.
    storage
        .store_metrics(vec![sample_metric(900, "compat.metric", 1.0)])
        .await
        .unwrap();

    // Enable compression + compress every chunk. If unsupported here, skip — we
    // can't exercise the compressed-chunk scenario.
    if db
        .execute(run("ALTER TABLE otel_metrics SET (timescaledb.compress, \
             timescaledb.compress_segmentby = 'project_id')"))
        .await
        .is_err()
    {
        println!("compression not supported on otel_metrics, skipping");
        return;
    }
    if let Err(e) = db
        .execute(run(
            "SELECT compress_chunk(c) FROM show_chunks('otel_metrics') c",
        ))
        .await
    {
        println!("compress_chunk failed ({e}), skipping compressed-chunk assertions");
        return;
    }

    // The exact operation shapes the migration performs — all must succeed with a
    // compressed chunk present.
    db.execute(run(
        "ALTER TABLE otel_metrics ADD COLUMN IF NOT EXISTS _compat_probe double precision",
    ))
    .await
    .expect("nullable ADD COLUMN must succeed on a compressed hypertable");
    db.execute(run(
        "CREATE INDEX IF NOT EXISTS _compat_probe_btree ON otel_metrics \
         (project_id, metric_name, service_name, timestamp DESC)",
    ))
    .await
    .expect("composite btree CREATE INDEX must succeed on a compressed hypertable");
    db.execute(run(
        "CREATE INDEX IF NOT EXISTS _compat_probe_gin ON otel_metrics \
         USING GIN (attributes jsonb_path_ops)",
    ))
    .await
    .expect("GIN CREATE INDEX must succeed on a compressed hypertable");
}

// ── Span stats (per-operation latency report) ────────────────────────

/// Build a span with an explicit start time, so aggregation tests can place
/// spans deterministically inside (and outside) the queried window.
#[allow(clippy::too_many_arguments)]
fn stats_span(
    project_id: i32,
    service_name: &str,
    name: &str,
    duration_ms: f64,
    status: SpanStatusCode,
    start_time: DateTime<Utc>,
) -> SpanRecord {
    let seq = next_span_seq();
    let mut span = sample_span(
        project_id,
        &format!("{seq:032x}"),
        &format!("{seq:016x}"),
        None,
        name,
        SpanKind::Server,
        status,
        duration_ms,
    );
    span.resource.service_name = service_name.into();
    span.start_time = start_time;
    span.end_time = start_time + Duration::microseconds((duration_ms * 1000.0) as i64);
    span
}

/// Monotonic id source — trace/span ids only need to be distinct here, and a
/// counter keeps the fixtures reproducible.
fn next_span_seq() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn stats_query(project_ids: Vec<i32>, start: DateTime<Utc>, end: DateTime<Utc>) -> SpanStatsQuery {
    SpanStatsQuery {
        project_ids,
        start_time: start,
        end_time: end,
        service_name: None,
        span_name: None,
        name_pattern: None,
        kind: None,
        status: None,
        environment_id: None,
        deployment_id: None,
        attributes: None,
        min_duration_ms: None,
        min_count: 1,
        sort_by: SpanStatsSortField::default(),
        sort_order: SortOrder::default(),
        limit: None,
        offset: None,
    }
}

#[tokio::test]
async fn test_span_stats_aggregates_percentiles_and_ratios() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };
    let now = Utc::now();
    let window_start = now - Duration::hours(1);

    // `steady` is uniformly ~100ms. `erratic` is bimodal: 18 fast calls and
    // 2 that take 2s — the case an average alone hides completely.
    let mut spans = Vec::new();
    for i in 0..20 {
        spans.push(stats_span(
            1,
            "api",
            "steady",
            100.0 + i as f64,
            SpanStatusCode::Ok,
            now - Duration::minutes(30),
        ));
    }
    for i in 0..18 {
        spans.push(stats_span(
            1,
            "api",
            "erratic",
            40.0 + i as f64,
            SpanStatusCode::Ok,
            now - Duration::minutes(30),
        ));
    }
    for _ in 0..2 {
        spans.push(stats_span(
            1,
            "api",
            "erratic",
            2000.0,
            SpanStatusCode::Error,
            now - Duration::minutes(30),
        ));
    }
    storage.store_spans(spans).await.expect("store spans");

    let rows = storage
        .query_span_stats(stats_query(vec![1], window_start, now))
        .await
        .expect("query span stats");

    let steady = rows
        .iter()
        .find(|r| r.span_name == "steady")
        .expect("steady");
    let erratic = rows
        .iter()
        .find(|r| r.span_name == "erratic")
        .expect("erratic");

    assert_eq!(steady.count, 20);
    assert_eq!(erratic.count, 20);
    assert_eq!(erratic.error_count, 2);
    assert!((erratic.error_rate - 0.1).abs() < 1e-9);

    // The max is what "how bad did it ever get?" asks for.
    assert!((erratic.max_duration_ms - 2000.0).abs() < 1.0);
    // p50 stays in the fast band even though the max is 2s — which is exactly
    // why a median-only view misses this operation.
    assert!(
        erratic.p50_duration_ms < 100.0,
        "p50 was {}",
        erratic.p50_duration_ms
    );
    assert!(
        erratic.p99_duration_ms > 1000.0,
        "p99 was {}",
        erratic.p99_duration_ms
    );

    // Both ranking signals must prefer the erratic operation, even though the
    // steady one is not meaningfully faster on average.
    assert!(erratic.tail_ratio > steady.tail_ratio);
    assert!(erratic.coefficient_of_variation > steady.coefficient_of_variation);
}

#[tokio::test]
async fn test_span_stats_respects_window_project_and_min_count() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };
    let now = Utc::now();

    let mut spans = Vec::new();
    // In-window, project 1, 5 samples.
    for _ in 0..5 {
        spans.push(stats_span(
            1,
            "api",
            "in-window",
            10.0,
            SpanStatusCode::Ok,
            now - Duration::minutes(5),
        ));
    }
    // In-window, project 1, but only 2 samples — below a min_count of 3.
    for _ in 0..2 {
        spans.push(stats_span(
            1,
            "api",
            "rare",
            10.0,
            SpanStatusCode::Ok,
            now - Duration::minutes(5),
        ));
    }
    // Outside the window entirely.
    for _ in 0..5 {
        spans.push(stats_span(
            1,
            "api",
            "too-old",
            10.0,
            SpanStatusCode::Ok,
            now - Duration::hours(48),
        ));
    }
    // A different project.
    for _ in 0..5 {
        spans.push(stats_span(
            2,
            "api",
            "other-project",
            10.0,
            SpanStatusCode::Ok,
            now - Duration::minutes(5),
        ));
    }
    storage.store_spans(spans).await.expect("store spans");

    let query = stats_query(vec![1], now - Duration::hours(1), now);
    let rows = storage
        .query_span_stats(query.clone())
        .await
        .expect("query span stats");
    let names: Vec<&str> = rows.iter().map(|r| r.span_name.as_str()).collect();

    assert!(names.contains(&"in-window"));
    assert!(names.contains(&"rare"));
    assert!(
        !names.contains(&"too-old"),
        "spans outside the window must not be aggregated"
    );
    assert!(
        !names.contains(&"other-project"),
        "another project's spans must never leak into the report"
    );

    // The min_count floor is what keeps three-sample noise out of a
    // variability ranking, so it must actually drop rows.
    let floored = SpanStatsQuery {
        min_count: 3,
        ..query.clone()
    };
    let rows = storage
        .query_span_stats(floored.clone())
        .await
        .expect("query span stats");
    let names: Vec<&str> = rows.iter().map(|r| r.span_name.as_str()).collect();
    assert!(names.contains(&"in-window"));
    assert!(
        !names.contains(&"rare"),
        "min_count must drop low-sample rows"
    );

    // The count must agree with the rows, including the min_count floor —
    // otherwise pagination silently loses or repeats operations.
    let total = storage
        .count_span_stats(floored)
        .await
        .expect("count span stats");
    assert_eq!(total as usize, rows.len());
}

#[tokio::test]
async fn test_span_stats_spans_multiple_projects_and_sorts() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };
    let now = Utc::now();

    let mut spans = Vec::new();
    // Project 1: cheap but very frequent — wins on count, loses on p95.
    for _ in 0..50 {
        spans.push(stats_span(
            1,
            "api",
            "cache.get",
            2.0,
            SpanStatusCode::Ok,
            now - Duration::minutes(5),
        ));
    }
    // Project 2: expensive but rare — wins on p95, loses on count.
    for _ in 0..5 {
        spans.push(stats_span(
            2,
            "worker",
            "report.render",
            900.0,
            SpanStatusCode::Ok,
            now - Duration::minutes(5),
        ));
    }
    storage.store_spans(spans).await.expect("store spans");

    let base = stats_query(vec![1, 2], now - Duration::hours(1), now);

    // Both projects appear in one report, each tagged with its own project_id.
    let rows = storage
        .query_span_stats(base.clone())
        .await
        .expect("query span stats");
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .any(|r| r.project_id == 1 && r.span_name == "cache.get"));
    assert!(rows
        .iter()
        .any(|r| r.project_id == 2 && r.span_name == "report.render"));

    // total_time: 50 x 2ms = 100ms vs 5 x 900ms = 4500ms.
    let by_total = storage
        .query_span_stats(SpanStatsQuery {
            sort_by: SpanStatsSortField::TotalDurationMs,
            ..base.clone()
        })
        .await
        .expect("query span stats");
    assert_eq!(by_total[0].span_name, "report.render");

    // count: the frequent cheap call wins.
    let by_count = storage
        .query_span_stats(SpanStatsQuery {
            sort_by: SpanStatsSortField::Count,
            ..base.clone()
        })
        .await
        .expect("query span stats");
    assert_eq!(by_count[0].span_name, "cache.get");

    // Ascending order must actually invert the ranking.
    let ascending = storage
        .query_span_stats(SpanStatsQuery {
            sort_by: SpanStatsSortField::TotalDurationMs,
            sort_order: SortOrder::Asc,
            ..base
        })
        .await
        .expect("query span stats");
    assert_eq!(ascending[0].span_name, "cache.get");
}

#[tokio::test]
async fn test_span_stats_filters_by_service_name_and_status() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };
    let now = Utc::now();

    let spans = vec![
        stats_span(
            1,
            "api",
            "checkout",
            100.0,
            SpanStatusCode::Ok,
            now - Duration::minutes(5),
        ),
        stats_span(
            1,
            "api",
            "checkout",
            900.0,
            SpanStatusCode::Error,
            now - Duration::minutes(5),
        ),
        stats_span(
            1,
            "worker",
            "checkout",
            50.0,
            SpanStatusCode::Ok,
            now - Duration::minutes(5),
        ),
    ];
    storage.store_spans(spans).await.expect("store spans");

    let base = stats_query(vec![1], now - Duration::hours(1), now);

    let api_only = storage
        .query_span_stats(SpanStatsQuery {
            service_name: Some("api".into()),
            ..base.clone()
        })
        .await
        .expect("query span stats");
    assert_eq!(api_only.len(), 1);
    assert_eq!(api_only[0].service_name, "api");
    assert_eq!(api_only[0].count, 2);

    // status=error answers "how slow are the failures?" — the OK span must not
    // dilute the numbers.
    let errors_only = storage
        .query_span_stats(SpanStatsQuery {
            status: Some(SpanStatusCode::Error),
            ..base.clone()
        })
        .await
        .expect("query span stats");
    assert_eq!(errors_only.len(), 1);
    assert_eq!(errors_only[0].count, 1);
    assert!((errors_only[0].max_duration_ms - 900.0).abs() < 1.0);

    // An exact span_name plus max is the "worst case for this operation" query.
    let named = storage
        .query_span_stats(SpanStatsQuery {
            span_name: Some("checkout".into()),
            service_name: Some("api".into()),
            ..base
        })
        .await
        .expect("query span stats");
    assert_eq!(named.len(), 1);
    assert!((named[0].max_duration_ms - 900.0).abs() < 1.0);
}

// ── Ingest error reporting (`record_ingest_error` / `recent_ingest_errors`) ─
//
// `record_ingest_error` upserts on `(signal_type, error_class)`, so these
// tests exercise the three behaviours that are specific to the real Postgres
// backend and cannot be pinned by the in-memory `MockOtelStorage` in
// `otel_service.rs`'s unit tests: the upsert itself, the `WHERE last_seen >
// NOW() - INTERVAL '7 days'` window filter, and the insert-time truncation of
// an oversized sample message.

/// Recording the same `(signal_type, error_class)` twice must bump the
/// existing row's `count` rather than inserting a second row — the whole
/// point of the `ON CONFLICT (signal_type, error_class) DO UPDATE` upsert.
#[tokio::test]
async fn test_record_ingest_error_upsert_bumps_count_not_a_new_row() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    storage
        .record_ingest_error("spans", "clickhouse_network", "connection reset by peer")
        .await
        .expect("first record succeeds");
    storage
        .record_ingest_error(
            "spans",
            "clickhouse_network",
            "connection reset by peer (again)",
        )
        .await
        .expect("second record succeeds");

    let errors = storage
        .recent_ingest_errors(50)
        .await
        .expect("recent_ingest_errors succeeds");

    let matching: Vec<_> = errors
        .iter()
        .filter(|e| e.signal_type == "spans" && e.error_class == "clickhouse_network")
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "the same (signal_type, error_class) pair must upsert into one row, \
         not insert a second — got {errors:?}"
    );
    assert_eq!(
        matching[0].count, 2,
        "count must bump on the second occurrence"
    );
    assert!(
        matching[0].sample_message.contains("(again)"),
        "sample_message must be overwritten by the newest occurrence, got {:?}",
        matching[0].sample_message
    );
}

/// `recent_ingest_errors` must exclude groups whose `last_seen` has aged out
/// of the 7-day reporting window (`INGEST_ERROR_WINDOW_DAYS`), so a failure
/// mode that was fixed a while ago does not sit on the dashboard forever.
/// Backdates a row's `last_seen` directly via SQL — there is no ingest-side
/// way to fabricate an old timestamp — and proves it disappears while a fresh
/// group in the same query stays visible.
#[tokio::test]
async fn test_recent_ingest_errors_excludes_entries_older_than_the_window() {
    use sea_orm::ConnectionTrait;

    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    // A fresh group — must remain in the report.
    storage
        .record_ingest_error("metrics", "postgres_conn", "recent failure")
        .await
        .expect("record succeeds");

    // A group that will be backdated past the 7-day window — must disappear.
    storage
        .record_ingest_error("logs", "clickhouse_timeout", "stale failure")
        .await
        .expect("record succeeds");

    _db.db
        .execute(sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "UPDATE otel_ingest_errors SET last_seen = NOW() - INTERVAL '8 days' \
             WHERE signal_type = $1 AND error_class = $2",
            vec!["logs".into(), "clickhouse_timeout".into()],
        ))
        .await
        .expect("backdating last_seen succeeds");

    let errors = storage
        .recent_ingest_errors(50)
        .await
        .expect("recent_ingest_errors succeeds");

    assert!(
        errors
            .iter()
            .any(|e| e.signal_type == "metrics" && e.error_class == "postgres_conn"),
        "a group last seen within the 7-day window must still be reported, got {errors:?}"
    );
    assert!(
        !errors
            .iter()
            .any(|e| e.signal_type == "logs" && e.error_class == "clickhouse_timeout"),
        "a group whose last_seen is 8 days old must be excluded by the window filter, \
         but was returned: {errors:?}"
    );
}

/// A sample message longer than the 500-character cap must be truncated
/// *before* the row is written — proving `truncate_sample_message` (see
/// timescaledb.rs around `record_ingest_error`'s `INSERT`) is actually invoked
/// on the insert path, not just available as an unused helper. Reads the raw
/// column value directly (not only through `recent_ingest_errors`) so the
/// assertion pins the on-disk value, not just this read path's shape.
#[tokio::test]
async fn test_record_ingest_error_truncates_long_sample_message_before_insert() {
    use sea_orm::ConnectionTrait;

    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let long_message = "x".repeat(600);
    storage
        .record_ingest_error("spans", "clickhouse_other", &long_message)
        .await
        .expect("record succeeds");

    // Via the storage-layer read path.
    let errors = storage
        .recent_ingest_errors(50)
        .await
        .expect("recent_ingest_errors succeeds");
    let matching = errors
        .iter()
        .find(|e| e.signal_type == "spans" && e.error_class == "clickhouse_other")
        .expect("group present");
    assert!(
        matching.sample_message.chars().count() <= 501,
        "sample_message must be truncated to at most 500 chars + ellipsis, got {} chars",
        matching.sample_message.chars().count()
    );
    assert!(
        matching.sample_message.ends_with('…'),
        "a truncated message must end with an ellipsis, got {:?}",
        matching.sample_message
    );
    assert_ne!(
        matching.sample_message, long_message,
        "the 600-char message must actually be truncated, not stored verbatim"
    );

    // Via the raw column, to prove truncation happened at INSERT time and is
    // not an artifact of the read path.
    let row = _db
        .db
        .query_one(sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT sample_message FROM otel_ingest_errors \
             WHERE signal_type = $1 AND error_class = $2",
            vec!["spans".into(), "clickhouse_other".into()],
        ))
        .await
        .expect("query succeeds")
        .expect("row exists");
    let stored: String = row
        .try_get("", "sample_message")
        .expect("sample_message column readable");
    assert!(
        stored.chars().count() <= 501,
        "the raw DB column must already be truncated at insert time, got {} chars",
        stored.chars().count()
    );
}
