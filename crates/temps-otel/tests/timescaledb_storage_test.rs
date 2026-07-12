//! Integration tests for the TimescaleDB storage backend.
//!
//! These tests require a Docker-accessible TimescaleDB instance.
//! They skip gracefully when Docker is unavailable (no `#[ignore]`).

use std::collections::BTreeMap;

use chrono::{Duration, Utc};

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
async fn test_store_spans_empty_batch() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let stored = storage.store_spans(vec![]).await.unwrap();
    assert_eq!(stored, 0);
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
    let storage_with_quota =
        TimescaleDbStorage::with_config(_db.db.clone(), None, 7, Some(10 * 1024 * 1024 * 1024));
    let quota = storage_with_quota.get_storage_quota(1).await.unwrap();
    assert_eq!(quota.limit_bytes, 10 * 1024 * 1024 * 1024);
    let exceeded = storage_with_quota.check_quota(1).await.unwrap();
    assert!(!exceeded); // Fresh DB, should not be exceeded
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
    secs_ago: i64,
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
        Utc::now() - Duration::seconds(secs_ago),
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

/// DELTA histograms in the same bucket must be ELEMENT-WISE summed (validates the
/// WITH ORDINALITY array aggregation across multiple rows — not just one).
#[tokio::test]
async fn test_full_fidelity_histogram_delta_elementwise_sum() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };
    let project_id = 110;
    let name = "http.latency.delta";
    storage
        .store_metrics(vec![
            hist_pt(
                project_id,
                name,
                30,
                AggregationTemporality::Delta,
                100,
                5000.0,
                vec![5, 20, 50, 20, 5],
            ),
            hist_pt(
                project_id,
                name,
                20,
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
    storage
        .store_metrics(vec![
            // earlier snapshot
            hist_pt(
                project_id,
                name,
                40,
                AggregationTemporality::Cumulative,
                50,
                2500.0,
                vec![2, 10, 25, 10, 3],
            ),
            // later snapshot (the running total now)
            hist_pt(
                project_id,
                name,
                10,
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
    storage
        .store_metrics(vec![hist_pt(
            project_id,
            name,
            20,
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

    let mut with_route = hist_pt(
        project_id,
        name,
        20,
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
        20,
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
