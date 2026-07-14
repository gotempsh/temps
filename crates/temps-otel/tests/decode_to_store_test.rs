//! End-to-end OTLP decode -> native ClickHouse store fidelity test.
//!
//! This proves the full claim "metrics are being stored": it feeds a *real*
//! `ExportMetricsServiceRequest` protobuf (the exact wire shape an OTel exporter
//! emits) through [`decode_metrics_request`] and then [`OtelStorage::store_metrics`]
//! into a live ClickHouse `metrics` table, and finally reads the rows back —
//! BOTH via the public `query_metrics`/`list_metric_names` API AND by selecting
//! the raw stored columns — asserting that full OTLP fidelity survives the whole
//! path: aggregation temporality, the `is_monotonic` counter flag, explicit
//! histogram bucket bounds/counts, and the data-point attribute labels.
//!
//! If Docker / ClickHouse is unreachable the test prints a skip message and
//! returns (per CLAUDE.md Docker tests MUST NOT be `#[ignore]`d — they detect
//! unavailability at runtime and skip gracefully). When skipped, the storage
//! roundtrip is NOT executed.
//!
//! The inner `TimescaleDbStorage` is a sea-orm `MockDatabase`: the metric
//! methods under test read/write only ClickHouse and never touch Postgres.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::Utc;
use prost::Message;
use sea_orm::{DatabaseBackend, MockDatabase};

use temps_otel::ingest::decode::decode_metrics_request;
use temps_otel::proto;
use temps_otel::storage::clickhouse::{ClickHouseOtelConfig, ClickHouseOtelStorage};
use temps_otel::storage::timescaledb::TimescaleDbStorage;
use temps_otel::storage::OtelStorage;
use temps_otel::types::{
    Exemplar, MetricAggregation, MetricPoint, MetricQuery, MetricType, ResourceInfo,
};

/// A raw row read straight back out of the `metrics` table, used to prove the
/// structured columns (temporality / is_monotonic / histogram arrays /
/// attributes) round-trip — not just the aggregated scalar value.
#[derive(::clickhouse::Row, serde::Deserialize, Debug)]
struct RawMetricRow {
    metric_name: String,
    metric_type: String,
    temporality: String,
    is_monotonic: Option<u8>,
    unit: String,
    value: Option<f64>,
    histogram_count: Option<u64>,
    histogram_sum: Option<f64>,
    histogram_bounds: Vec<f64>,
    histogram_bucket_counts: Vec<u64>,
    attributes: Vec<(String, String)>,
}

/// Build a realistic multi-metric OTLP export: a Gauge, a monotonic cumulative
/// Sum (counter) and a cumulative explicit Histogram, all under one resource and
/// carrying data-point labels. Returns the protobuf-encoded bytes exactly as an
/// exporter would POST them.
fn sample_export_request() -> Vec<u8> {
    let kv = |k: &str, v: &str| proto::common::v1::KeyValue {
        key: k.into(),
        value: Some(proto::common::v1::AnyValue {
            value: Some(proto::common::v1::any_value::Value::StringValue(v.into())),
        }),
    };

    let resource = proto::resource::v1::Resource {
        attributes: vec![
            kv("service.name", "checkout"),
            kv("service.version", "2.0.0"),
            kv("deployment.environment", "production"),
        ],
        dropped_attributes_count: 0,
    };

    let gauge = proto::metrics::v1::Metric {
        name: "http.server.active_requests".into(),
        description: "In-flight requests".into(),
        unit: "1".into(),
        data: Some(proto::metrics::v1::metric::Data::Gauge(
            proto::metrics::v1::Gauge {
                data_points: vec![proto::metrics::v1::NumberDataPoint {
                    time_unix_nano: 1_700_000_000_000_000_000,
                    value: Some(proto::metrics::v1::number_data_point::Value::AsDouble(7.0)),
                    attributes: vec![kv("http.method", "POST")],
                    ..Default::default()
                }],
            },
        )),
    };

    let counter = proto::metrics::v1::Metric {
        name: "http.requests.total".into(),
        description: "Total requests".into(),
        unit: "1".into(),
        data: Some(proto::metrics::v1::metric::Data::Sum(
            proto::metrics::v1::Sum {
                data_points: vec![proto::metrics::v1::NumberDataPoint {
                    start_time_unix_nano: 1_699_000_000_000_000_000,
                    time_unix_nano: 1_700_000_000_000_000_000,
                    value: Some(proto::metrics::v1::number_data_point::Value::AsInt(4242)),
                    attributes: vec![kv("route", "/checkout")],
                    ..Default::default()
                }],
                aggregation_temporality: 2, // CUMULATIVE
                is_monotonic: true,
            },
        )),
    };

    let histogram = proto::metrics::v1::Metric {
        name: "http.server.duration".into(),
        description: String::new(),
        unit: "ms".into(),
        data: Some(proto::metrics::v1::metric::Data::Histogram(
            proto::metrics::v1::Histogram {
                data_points: vec![proto::metrics::v1::HistogramDataPoint {
                    time_unix_nano: 1_700_000_000_000_000_000,
                    count: 10,
                    sum: Some(900.0),
                    min: Some(5.0),
                    max: Some(450.0),
                    explicit_bounds: vec![10.0, 100.0, 250.0],
                    bucket_counts: vec![2, 3, 4, 1],
                    attributes: vec![kv("http.method", "GET")],
                    ..Default::default()
                }],
                aggregation_temporality: 2, // CUMULATIVE
            },
        )),
    };

    let request = proto::collector::metrics::v1::ExportMetricsServiceRequest {
        resource_metrics: vec![proto::metrics::v1::ResourceMetrics {
            resource: Some(resource),
            scope_metrics: vec![proto::metrics::v1::ScopeMetrics {
                scope: None,
                metrics: vec![gauge, counter, histogram],
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        }],
    };

    request.encode_to_vec()
}

/// Spin a ClickHouse testcontainer, apply migrations, and return a connected
/// storage plus a bare read-back client. Returns `None` when Docker is
/// unavailable so the test skips gracefully.
async fn setup() -> Option<(
    ClickHouseOtelStorage,
    ::clickhouse::Client,
    Box<dyn std::any::Any + Send>,
)> {
    use testcontainers::{
        core::{wait::HttpWaitStrategy, ContainerPort, WaitFor},
        runners::AsyncRunner,
        GenericImage, ImageExt,
    };

    let image = GenericImage::new("clickhouse/clickhouse-server", "24.8")
        .with_exposed_port(ContainerPort::Tcp(8123))
        // The clickhouse-server image writes "Ready for connections" only to its
        // in-container log file — never to stdout/stderr — so a log-message wait
        // always times out and the test silently skips. Wait on the HTTP /ping
        // endpoint (returns 200 "Ok." once the server accepts queries) instead.
        .with_wait_for(WaitFor::http(
            HttpWaitStrategy::new("/ping")
                .with_port(ContainerPort::Tcp(8123))
                .with_expected_status_code(200u16),
        ))
        .with_env_var("CLICKHOUSE_DB", "temps_otel_e2e")
        // Do NOT set CLICKHOUSE_USER=default (the image's user-init then rejects
        // the pre-existing default user) and do NOT use an empty password (an
        // empty CLICKHOUSE_PASSWORD leaves `default` unauthenticatable in 24.8).
        .with_env_var("CLICKHOUSE_PASSWORD", "test");

    let container = match image.start().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Skipping OTLP decode->store test: cannot start ClickHouse container ({e})");
            return None;
        }
    };

    let host_port = match container.get_host_port_ipv4(8123).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Skipping OTLP decode->store test: cannot get host port ({e})");
            return None;
        }
    };

    let url = format!("http://127.0.0.1:{host_port}");
    let config = ClickHouseOtelConfig::new(&url, "temps_otel_e2e", "default", "test");

    let probe = ::clickhouse::Client::default()
        .with_url(&url)
        .with_database("temps_otel_e2e")
        .with_user("default")
        .with_password("test");

    // Wait until the HTTP listener accepts queries.
    let mut last_err = String::new();
    for _ in 0..30 {
        match probe.query("SELECT 1").execute().await {
            Ok(_) => {
                last_err.clear();
                break;
            }
            Err(e) => {
                last_err = format!("{e}");
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }
    if !last_err.is_empty() {
        eprintln!("Skipping OTLP decode->store test: server never became ready ({last_err})");
        return None;
    }

    // Migrations MUST succeed — this is the whole point of the test.
    temps_otel::storage::clickhouse::migrations::apply_migrations(&probe, "temps_otel_e2e")
        .await
        .expect("apply_migrations failed against testcontainer ClickHouse");

    let mock_db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let inner = Arc::new(TimescaleDbStorage::new(Arc::new(mock_db), None));
    let storage =
        ClickHouseOtelStorage::new(config, inner, Arc::new(temps_core::FixedRetentionResolver));

    Some((storage, probe, Box::new(container)))
}

#[tokio::test]
async fn otlp_decode_to_store_preserves_full_fidelity() {
    let Some((storage, read_client, _container)) = setup().await else {
        return; // Docker unavailable — skip gracefully. Roundtrip NOT executed.
    };

    const PROJECT_ID: i32 = 777;

    // 1. Decode a real OTLP protobuf export, exactly as the ingest handler does.
    let encoded = sample_export_request();
    let mut points = decode_metrics_request(&encoded, PROJECT_ID, Some(42))
        .expect("decode_metrics_request must succeed on a valid export");
    assert_eq!(points.len(), 3, "gauge + counter + histogram = 3 points");

    // Sanity-check the decode produced the right internal types BEFORE storing,
    // so a failure localises to decode vs store.
    let by_name = |name: &str| {
        points
            .iter()
            .find(|p| p.metric_name == name)
            .unwrap_or_else(|| panic!("decoded point {name} missing"))
    };
    assert_eq!(by_name("http.requests.total").is_monotonic, Some(true));
    assert_eq!(
        by_name("http.server.duration").histogram_bucket_counts,
        Some(vec![2, 3, 4, 1])
    );

    // The sample protobuf carries fixed 2023 timestamps, but the metrics table has
    // a 90-day TTL — stored rows would be immediately TTL-expired and excluded from
    // every read. Retime each decoded point to "now" (distinct millisecond per
    // point) while preserving all other decoded fields; the read-back assertions
    // below check no timestamp, only the structured fidelity.
    let retime_base = chrono::Utc::now();
    for (i, p) in points.iter_mut().enumerate() {
        p.timestamp = retime_base + chrono::Duration::milliseconds(i as i64);
    }

    // 2. Store the decoded points natively into ClickHouse.
    let stored = storage
        .store_metrics(points)
        .await
        .expect("store_metrics must succeed");
    assert_eq!(stored, 3, "all three decoded points should be written");

    // 3a. Public API: list_metric_names returns all three, sorted.
    let names = storage
        .list_metric_names(PROJECT_ID)
        .await
        .expect("list_metric_names must succeed");
    assert_eq!(
        names,
        vec![
            "http.requests.total".to_string(),
            "http.server.active_requests".to_string(),
            "http.server.duration".to_string(),
        ],
        "all stored metric names should round-trip via the public API"
    );

    // 3b. Public API: query_metrics on the gauge returns the stored scalar.
    let gauge_buckets = storage
        .query_metrics(MetricQuery {
            project_id: PROJECT_ID,
            metric_name: Some("http.server.active_requests".into()),
            bucket_interval: Some("1 hour".into()),
            aggregation: MetricAggregation::Max,
            ..Default::default()
        })
        .await
        .expect("query_metrics must succeed");
    assert_eq!(gauge_buckets.len(), 1, "one gauge point -> one bucket");
    assert!(
        (gauge_buckets[0].value - 7.0).abs() < f64::EPSILON,
        "gauge value 7.0 must survive to query_metrics, got {}",
        gauge_buckets[0].value
    );

    // 4. Raw column read-back: prove the STRUCTURED fidelity survived, not just
    //    the aggregated scalar. Read every stored column for this project and
    //    assert temporality / is_monotonic / histogram arrays / attributes.
    //    FINAL collapses any ReplacingMergeTree duplicates.
    let rows: Vec<RawMetricRow> = read_client
        .query(
            "SELECT metric_name, metric_type, temporality, is_monotonic, unit, value, \
             histogram_count, histogram_sum, histogram_bounds, histogram_bucket_counts, attributes \
             FROM metrics FINAL WHERE project_id = ? ORDER BY metric_name",
        )
        .bind(PROJECT_ID)
        .fetch_all::<RawMetricRow>()
        .await
        .expect("raw read-back of stored metric rows must succeed");
    assert_eq!(rows.len(), 3, "three raw rows stored");

    // rows are ordered by metric_name:
    //   [0] http.requests.total (counter)
    //   [1] http.server.active_requests (gauge)
    //   [2] http.server.duration (histogram)
    let counter = &rows[0];
    assert_eq!(counter.metric_name, "http.requests.total");
    assert_eq!(counter.metric_type, "sum");
    assert_eq!(
        counter.temporality, "cumulative",
        "aggregation temporality must survive decode->store"
    );
    assert_eq!(
        counter.is_monotonic,
        Some(1),
        "is_monotonic counter flag must survive decode->store"
    );
    assert_eq!(counter.unit, "1");
    assert_eq!(counter.value, Some(4242.0));

    let gauge = &rows[1];
    assert_eq!(gauge.metric_name, "http.server.active_requests");
    assert_eq!(gauge.metric_type, "gauge");
    // Gauge carries no temporality -> sentinel; no is_monotonic.
    assert_eq!(gauge.temporality, "unspecified");
    assert_eq!(gauge.is_monotonic, None);
    assert_eq!(gauge.value, Some(7.0));
    assert_eq!(
        gauge.attributes,
        vec![("http.method".to_string(), "POST".to_string())],
        "data-point labels must survive decode->store"
    );

    let hist = &rows[2];
    assert_eq!(hist.metric_name, "http.server.duration");
    assert_eq!(hist.metric_type, "histogram");
    assert_eq!(hist.temporality, "cumulative");
    assert_eq!(hist.histogram_count, Some(10));
    assert_eq!(hist.histogram_sum, Some(900.0));
    assert_eq!(
        hist.histogram_bounds,
        vec![10.0, 100.0, 250.0],
        "explicit histogram bucket bounds must survive decode->store"
    );
    assert_eq!(
        hist.histogram_bucket_counts,
        vec![2, 3, 4, 1],
        "histogram bucket counts must survive decode->store"
    );
    assert_eq!(
        hist.attributes,
        vec![("http.method".to_string(), "GET".to_string())],
        "histogram data-point labels must survive decode->store"
    );
}

/// Raw read-back of the nested `Array(Tuple(...))` columns. These exercise the
/// trickiest RowBinary codepaths — a single field-order or type-width mismatch in
/// a nested tuple silently corrupts data — and were previously only unit-tested,
/// never inserted into a live ClickHouse.
#[derive(::clickhouse::Row, serde::Deserialize, Debug)]
struct RawStructuredRow {
    metric_name: String,
    exp_scale: Option<i32>,
    exp_zero_count: Option<u64>,
    exp_positive_offset: Option<i32>,
    exp_positive_counts: Vec<u64>,
    exp_negative_counts: Vec<u64>,
    summary_quantiles: Vec<(f64, f64)>,
    // Tuple(trace_id, span_id, value, DateTime64 -> raw i64 ms).
    exemplars: Vec<(String, String, f64, i64)>,
}

#[tokio::test]
async fn store_and_readback_exp_histogram_summary_exemplars() {
    let Some((storage, read_client, _container)) = setup().await else {
        return; // Docker unavailable — skip gracefully. Roundtrip NOT executed.
    };

    const PROJECT_ID: i32 = 888;

    // One point carrying every nested-structure column at once (storage does not
    // enforce cross-field metric-type consistency; this maximises wire coverage).
    let mut p = MetricPoint::skeleton(
        PROJECT_ID,
        Some(7),
        ResourceInfo {
            service_name: "svc".into(),
            service_version: None,
            deployment_environment: None,
            attributes: BTreeMap::new(),
        },
        "request.duration.exp".into(),
        MetricType::ExponentialHistogram,
        "ms".into(),
        Utc::now(),
        BTreeMap::new(),
    );
    p.exp_scale = Some(3);
    p.exp_zero_count = Some(5);
    p.exp_zero_threshold = Some(0.001);
    p.exp_positive_offset = Some(-2);
    p.exp_positive_counts = Some(vec![1, 4, 9, 2]);
    p.exp_negative_offset = Some(0);
    p.exp_negative_counts = Some(vec![0, 1]);
    p.summary_quantiles = Some(vec![(0.5, 12.0), (0.9, 48.0), (0.99, 99.0)]);
    p.exemplars = vec![Exemplar {
        timestamp: Utc::now(),
        value: 42.0,
        trace_id: Some("abc123".into()),
        span_id: Some("def456".into()),
        attributes: BTreeMap::new(),
    }];
    p.value = Some(7.0);

    assert_eq!(
        storage.store_metrics(vec![p]).await.expect("store_metrics"),
        1
    );

    let rows: Vec<RawStructuredRow> = read_client
        .query(
            "SELECT metric_name, exp_scale, exp_zero_count, exp_positive_offset, \
             exp_positive_counts, exp_negative_counts, summary_quantiles, exemplars \
             FROM metrics FINAL WHERE project_id = ? ORDER BY metric_name",
        )
        .bind(PROJECT_ID)
        .fetch_all::<RawStructuredRow>()
        .await
        .expect("raw read-back of nested structured columns must succeed");

    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert_eq!(r.metric_name, "request.duration.exp");
    assert_eq!(r.exp_scale, Some(3));
    assert_eq!(r.exp_zero_count, Some(5));
    assert_eq!(r.exp_positive_offset, Some(-2));
    assert_eq!(r.exp_positive_counts, vec![1, 4, 9, 2]);
    assert_eq!(r.exp_negative_counts, vec![0, 1]);
    assert_eq!(
        r.summary_quantiles,
        vec![(0.5, 12.0), (0.9, 48.0), (0.99, 99.0)],
        "Array(Tuple(Float64,Float64)) summary quantiles must survive"
    );
    assert_eq!(r.exemplars.len(), 1, "exemplar must survive to the store");
    assert_eq!(r.exemplars[0].0, "abc123", "exemplar trace_id");
    assert_eq!(r.exemplars[0].1, "def456", "exemplar span_id");
    assert!(
        (r.exemplars[0].2 - 42.0).abs() < f64::EPSILON,
        "exemplar value"
    );
}
