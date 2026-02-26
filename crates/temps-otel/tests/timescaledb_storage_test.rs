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
    MetricPoint {
        project_id,
        deployment_id: None,
        resource: test_resource(),
        metric_name: name.into(),
        metric_type: MetricType::Gauge,
        unit: "ms".into(),
        timestamp: Utc::now(),
        value: Some(value),
        histogram_count: None,
        histogram_sum: None,
        histogram_min: None,
        histogram_max: None,
        histogram_bounds: None,
        histogram_bucket_counts: None,
        attributes: BTreeMap::new(),
    }
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

    let quota = storage.get_storage_quota(1).await.unwrap();
    assert_eq!(quota.project_id, 1);
    assert_eq!(quota.limit_bytes, 10 * 1024 * 1024 * 1024); // 10 GB
    assert!(quota.usage_pct >= 0.0);

    let exceeded = storage.check_quota(1).await.unwrap();
    assert!(!exceeded); // Fresh DB, should not be exceeded
}

// ── Retention (no old data to delete) ───────────────────────────────

#[tokio::test]
async fn test_apply_retention_no_old_data() {
    let Some((_db, storage)) = setup_storage().await else {
        return;
    };

    let deleted = storage.apply_retention(1).await.unwrap();
    assert_eq!(deleted, 0);
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
