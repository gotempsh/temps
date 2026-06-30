//! Real-ClickHouse integration test for the OTel metrics storage path.
//!
//! Spins up a `clickhouse/clickhouse-server` testcontainer, runs the OTel CH
//! migrations (which create the `metrics` table via `0003_metrics.sql`), then
//! round-trips a Gauge and a Histogram `MetricPoint` through the native
//! `ClickHouseOtelStorage` metric methods:
//!
//!   store_metrics -> query_metrics + list_metric_names
//!
//! If Docker is not reachable the test skips gracefully (per CLAUDE.md: Docker
//! tests must NEVER be `#[ignore]`d — they detect unavailability at runtime and
//! return).
//!
//! The inner `TimescaleDbStorage` is wired to a sea-orm `MockDatabase` because
//! the metric methods under test (`store_metrics`, `query_metrics`,
//! `list_metric_names`) read/write only ClickHouse and never touch the inner
//! Postgres storage. No Postgres container is required.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::Utc;
use sea_orm::{DatabaseBackend, MockDatabase};

use temps_otel::storage::clickhouse::{ClickHouseOtelConfig, ClickHouseOtelStorage};
use temps_otel::storage::timescaledb::TimescaleDbStorage;
use temps_otel::storage::OtelStorage;
use temps_otel::types::{
    AggregationTemporality, MetricAggregation, MetricPoint, MetricQuery, MetricType, ResourceInfo,
};

/// Container handle + a connected `ClickHouseOtelStorage`. Returns `None` when
/// Docker is unavailable so the test can skip without failing CI.
async fn setup() -> Option<(ClickHouseOtelStorage, Box<dyn std::any::Any + Send>)> {
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
        .with_env_var("CLICKHOUSE_DB", "temps_otel_test")
        // Do NOT set CLICKHOUSE_USER=default (the image's user-init then rejects
        // the pre-existing default user) and do NOT use an empty password (an
        // empty CLICKHOUSE_PASSWORD leaves `default` unauthenticatable in 24.8).
        .with_env_var("CLICKHOUSE_PASSWORD", "test");

    let container = match image.start().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Skipping ClickHouse OTel metrics test: cannot start container ({e})");
            return None;
        }
    };

    let host_port = match container.get_host_port_ipv4(8123).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Skipping ClickHouse OTel metrics test: cannot get host port ({e})");
            return None;
        }
    };

    let url = format!("http://127.0.0.1:{host_port}");
    let config = ClickHouseOtelConfig::new(&url, "temps_otel_test", "default", "test");

    // A bare client for the migration runner + readiness probe.
    let probe = ::clickhouse::Client::default()
        .with_url(&url)
        .with_database("temps_otel_test")
        .with_user("default")
        .with_password("test");

    // Wait until the HTTP listener actually accepts queries.
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
        eprintln!("Skipping ClickHouse OTel metrics test: server never became ready ({last_err})");
        return None;
    }

    // Apply OTel CH migrations (spans + metrics). This MUST succeed — it is the
    // entire reason for the test. Assert loudly on failure.
    temps_otel::storage::clickhouse::migrations::apply_migrations(&probe, "temps_otel_test")
        .await
        .expect("apply_migrations failed against testcontainer ClickHouse");

    // The inner Timescale storage is never exercised by the metric methods under
    // test; a MockDatabase satisfies the constructor without a Postgres server.
    let mock_db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let inner = Arc::new(TimescaleDbStorage::new(Arc::new(mock_db), None));

    let storage = ClickHouseOtelStorage::new(config, inner);
    Some((storage, Box::new(container)))
}

fn gauge_point() -> MetricPoint {
    let mut p = MetricPoint::skeleton(
        101,
        Some(9),
        ResourceInfo {
            service_name: "checkout".into(),
            service_version: Some("2.0.0".into()),
            deployment_environment: Some("production".into()),
            attributes: BTreeMap::new(),
        },
        "http.server.active_requests".into(),
        MetricType::Gauge,
        "1".into(),
        Utc::now(),
        {
            let mut m = BTreeMap::new();
            m.insert("http.method".into(), "POST".into());
            m
        },
    );
    p.value = Some(7.0);
    p
}

fn histogram_point() -> MetricPoint {
    let mut p = MetricPoint::skeleton(
        101,
        Some(9),
        ResourceInfo {
            service_name: "checkout".into(),
            service_version: Some("2.0.0".into()),
            deployment_environment: Some("production".into()),
            attributes: BTreeMap::new(),
        },
        "http.server.duration".into(),
        MetricType::Histogram,
        "ms".into(),
        Utc::now(),
        BTreeMap::new(),
    );
    p.histogram_count = Some(4);
    p.histogram_sum = Some(400.0);
    p.histogram_min = Some(10.0);
    p.histogram_max = Some(200.0);
    p.histogram_bounds = Some(vec![0.0, 50.0, 100.0]);
    p.histogram_bucket_counts = Some(vec![1, 1, 1, 1]);
    // Synthetic scalar value (mean) so query_metrics has a number to aggregate.
    p.value = Some(100.0);
    p
}

#[tokio::test]
async fn metrics_roundtrip_store_query_list() {
    let Some((storage, _container)) = setup().await else {
        return; // Docker unavailable — skip gracefully.
    };

    // 1. Store a Gauge + Histogram point.
    let stored = storage
        .store_metrics(vec![gauge_point(), histogram_point()])
        .await
        .expect("store_metrics should succeed");
    assert_eq!(stored, 2, "both points should be written");

    // 2. list_metric_names returns both distinct names, sorted.
    let names = storage
        .list_metric_names(101)
        .await
        .expect("list_metric_names should succeed");
    assert_eq!(
        names,
        vec![
            "http.server.active_requests".to_string(),
            "http.server.duration".to_string(),
        ],
        "distinct metric names should round-trip, sorted"
    );

    // A different project sees nothing.
    let other = storage
        .list_metric_names(999)
        .await
        .expect("list_metric_names for empty project should succeed");
    assert!(other.is_empty(), "other project must see no metrics");

    // 3. query_metrics on the gauge returns a bucket with the stored value.
    let buckets = storage
        .query_metrics(MetricQuery {
            project_id: 101,
            metric_name: Some("http.server.active_requests".into()),
            bucket_interval: Some("1 hour".into()),
            ..Default::default()
        })
        .await
        .expect("query_metrics should succeed");
    assert_eq!(buckets.len(), 1, "one gauge point -> one bucket");
    let b = &buckets[0];
    assert_eq!(b.count, 1);
    assert!((b.avg_value - 7.0).abs() < f64::EPSILON);
    assert!((b.min_value - 7.0).abs() < f64::EPSILON);
    assert!((b.max_value - 7.0).abs() < f64::EPSILON);
}

/// A gauge for `request.latency` carrying a single `http.method` label and a
/// scalar value, used to exercise group_by / label_filters / aggregation.
fn labelled_gauge(project_id: i32, method: &str, value: f64) -> MetricPoint {
    let mut p = MetricPoint::skeleton(
        project_id,
        Some(9),
        ResourceInfo {
            service_name: "api".into(),
            service_version: Some("1.0.0".into()),
            deployment_environment: Some("production".into()),
            attributes: BTreeMap::new(),
        },
        "request.latency".into(),
        MetricType::Gauge,
        "ms".into(),
        Utc::now(),
        {
            let mut m = BTreeMap::new();
            m.insert("http.method".into(), method.to_string());
            m
        },
    );
    p.value = Some(value);
    p
}

#[tokio::test]
async fn query_metrics_aggregation_group_by_and_label_filter() {
    let Some((storage, _container)) = setup().await else {
        return; // Docker unavailable — skip gracefully.
    };

    // GET: values 10, 30 (avg 20, max 30); POST: value 100.
    // Give each point a distinct millisecond timestamp: the two GET points share a
    // series (project+metric+service+labels), so without distinct timestamps the
    // ReplacingMergeTree series key (… , timestamp, attributes_hash) would treat
    // them as one and keep only the latest. All three stay within the same hour
    // bucket, so the grouped query still yields exactly two series.
    let base = Utc::now();
    let mut points = vec![
        labelled_gauge(2002, "GET", 10.0),
        labelled_gauge(2002, "GET", 30.0),
        labelled_gauge(2002, "POST", 100.0),
    ];
    points[0].timestamp = base;
    points[1].timestamp = base + chrono::Duration::milliseconds(1);
    points[2].timestamp = base + chrono::Duration::milliseconds(2);
    let stored = storage
        .store_metrics(points)
        .await
        .expect("store_metrics should succeed");
    assert_eq!(stored, 3);

    // 1. max aggregation, no grouping → single bucket, max across all = 100.
    let buckets = storage
        .query_metrics(MetricQuery {
            project_id: 2002,
            metric_name: Some("request.latency".into()),
            bucket_interval: Some("1 hour".into()),
            aggregation: MetricAggregation::Max,
            ..Default::default()
        })
        .await
        .expect("max query should succeed");
    assert_eq!(buckets.len(), 1, "one time bucket");
    assert!(
        (buckets[0].value - 100.0).abs() < f64::EPSILON,
        "max value should be 100, got {}",
        buckets[0].value
    );

    // 2. avg aggregation grouped by http.method → two series.
    let grouped = storage
        .query_metrics(MetricQuery {
            project_id: 2002,
            metric_name: Some("request.latency".into()),
            bucket_interval: Some("1 hour".into()),
            aggregation: MetricAggregation::Avg,
            group_by: vec!["http.method".into()],
            ..Default::default()
        })
        .await
        .expect("grouped query should succeed");
    assert_eq!(grouped.len(), 2, "one series per distinct method");
    for b in &grouped {
        let key = b
            .series_key
            .as_ref()
            .expect("grouped bucket must carry a series_key");
        assert_eq!(key.len(), 1);
        assert_eq!(key[0].0, "http.method");
        match key[0].1.as_str() {
            "GET" => assert!((b.value - 20.0).abs() < 1e-9, "GET avg should be 20"),
            "POST" => assert!((b.value - 100.0).abs() < 1e-9, "POST avg should be 100"),
            other => panic!("unexpected method label: {other}"),
        }
    }

    // 3. label_filters narrows to GET only → avg 20 over a single ungrouped series.
    let filtered = storage
        .query_metrics(MetricQuery {
            project_id: 2002,
            metric_name: Some("request.latency".into()),
            bucket_interval: Some("1 hour".into()),
            aggregation: MetricAggregation::Avg,
            label_filters: vec![("http.method".into(), "GET".into())],
            ..Default::default()
        })
        .await
        .expect("filtered query should succeed");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].count, 2, "only the two GET points");
    assert!((filtered[0].value - 20.0).abs() < 1e-9);

    // 4. p95 quantile aggregation populates the quantiles vec.
    let quant = storage
        .query_metrics(MetricQuery {
            project_id: 2002,
            metric_name: Some("request.latency".into()),
            bucket_interval: Some("1 hour".into()),
            aggregation: MetricAggregation::Quantile(0.95),
            ..Default::default()
        })
        .await
        .expect("quantile query should succeed");
    assert_eq!(quant.len(), 1);
    assert_eq!(quant[0].quantiles.len(), 1, "one (q, value) pair");
    assert!((quant[0].quantiles[0].0 - 0.95).abs() < f64::EPSILON);
}

#[tokio::test]
async fn query_metrics_rejects_disallowed_label_key() {
    let Some((storage, _container)) = setup().await else {
        return; // Docker unavailable — skip gracefully.
    };

    // A group_by key with forbidden characters must be rejected before SQL.
    let err = storage
        .query_metrics(MetricQuery {
            project_id: 2002,
            group_by: vec!["evil key; DROP".into()],
            ..Default::default()
        })
        .await;
    assert!(err.is_err(), "disallowed label key must be rejected");
}

#[tokio::test]
async fn store_metrics_drops_disallowed_name() {
    let Some((storage, _container)) = setup().await else {
        return; // Docker unavailable — skip gracefully.
    };

    // A metric whose name contains a SQL-injection payload must be dropped at
    // the trust boundary and never written.
    let mut bad = gauge_point();
    bad.metric_name = "evil'; DROP TABLE metrics; --".into();
    bad.project_id = 555;

    let stored = storage
        .store_metrics(vec![bad])
        .await
        .expect("store_metrics should not error on a dropped point");
    assert_eq!(stored, 0, "the disallowed-name point must be dropped");

    let names = storage
        .list_metric_names(555)
        .await
        .expect("list_metric_names should succeed");
    assert!(names.is_empty(), "nothing should have been written");
}

/// A Sum (counter) point with an explicit temporality + scalar value.
fn counter_point(
    project_id: i32,
    metric_name: &str,
    temporality: AggregationTemporality,
    value: f64,
    ts: chrono::DateTime<Utc>,
) -> MetricPoint {
    let mut p = MetricPoint::skeleton(
        project_id,
        Some(9),
        ResourceInfo {
            service_name: "api".into(),
            service_version: Some("1.0.0".into()),
            deployment_environment: Some("production".into()),
            attributes: BTreeMap::new(),
        },
        metric_name.into(),
        MetricType::Sum,
        "1".into(),
        ts,
        BTreeMap::new(),
    );
    p.value = Some(value);
    p.temporality = Some(temporality);
    p.is_monotonic = Some(true);
    p
}

#[tokio::test]
async fn query_metrics_rate_respects_temporality() {
    let Some((storage, _container)) = setup().await else {
        return; // Docker unavailable — skip gracefully.
    };

    let base = Utc::now();
    // Cumulative counter: raw running total 100 -> 130 within one hour; the
    // per-second rate is the within-bucket increase (130 - 100) / 3600.
    // Delta counter: per-interval increments 10 and 20; the rate is the SUM
    // (10 + 20) / 3600 — NOT max-min (which would be 10/3600). This is the
    // discriminating case that proves temporality is honoured.
    let points = vec![
        counter_point(
            3003,
            "cumulative.req",
            AggregationTemporality::Cumulative,
            100.0,
            base,
        ),
        counter_point(
            3003,
            "cumulative.req",
            AggregationTemporality::Cumulative,
            130.0,
            base + chrono::Duration::milliseconds(1),
        ),
        counter_point(3003, "delta.req", AggregationTemporality::Delta, 10.0, base),
        counter_point(
            3003,
            "delta.req",
            AggregationTemporality::Delta,
            20.0,
            base + chrono::Duration::milliseconds(1),
        ),
    ];
    assert_eq!(storage.store_metrics(points).await.expect("store"), 4);

    let secs = 3600.0;
    let rate_query = |name: &str| MetricQuery {
        project_id: 3003,
        metric_name: Some(name.into()),
        bucket_interval: Some("1 hour".into()),
        aggregation: MetricAggregation::RatePerSec,
        ..Default::default()
    };

    let cumulative = storage
        .query_metrics(rate_query("cumulative.req"))
        .await
        .expect("cumulative rate query");
    assert_eq!(cumulative.len(), 1);
    assert!(
        (cumulative[0].value - 30.0 / secs).abs() < 1e-9,
        "cumulative rate should be (130-100)/3600, got {}",
        cumulative[0].value
    );

    let delta = storage
        .query_metrics(rate_query("delta.req"))
        .await
        .expect("delta rate query");
    assert_eq!(delta.len(), 1);
    assert!(
        (delta[0].value - 30.0 / secs).abs() < 1e-9,
        "delta rate should be (10+20)/3600 (sum, not max-min), got {}",
        delta[0].value
    );
}

#[tokio::test]
async fn query_metrics_histogram_summary_aggregates_buckets() {
    let Some((storage, _container)) = setup().await else {
        return; // Docker unavailable — skip gracefully.
    };

    let base = Utc::now();
    let mk = |count: u64, sum: f64, buckets: Vec<u64>, ts: chrono::DateTime<Utc>| {
        let mut p = MetricPoint::skeleton(
            4004,
            Some(9),
            ResourceInfo {
                service_name: "api".into(),
                service_version: Some("1.0.0".into()),
                deployment_environment: Some("production".into()),
                attributes: BTreeMap::new(),
            },
            "http.server.duration".into(),
            MetricType::Histogram,
            "ms".into(),
            ts,
            BTreeMap::new(),
        );
        p.histogram_count = Some(count);
        p.histogram_sum = Some(sum);
        p.histogram_min = Some(1.0);
        p.histogram_max = Some(240.0);
        p.histogram_bounds = Some(vec![10.0, 100.0, 250.0]);
        p.histogram_bucket_counts = Some(buckets);
        p.value = Some(sum / count as f64); // synthetic mean
        p
    };
    // Two histogram points for the same series within one hour, same bounds.
    let stored = storage
        .store_metrics(vec![
            mk(4, 100.0, vec![1, 1, 1, 1], base),
            mk(
                6,
                200.0,
                vec![1, 2, 3, 0],
                base + chrono::Duration::milliseconds(1),
            ),
        ])
        .await
        .expect("store");
    assert_eq!(stored, 2);

    let buckets = storage
        .query_metrics(MetricQuery {
            project_id: 4004,
            metric_name: Some("http.server.duration".into()),
            bucket_interval: Some("1 hour".into()),
            aggregation: MetricAggregation::Avg,
            ..Default::default()
        })
        .await
        .expect("histogram query");
    assert_eq!(buckets.len(), 1);
    let hs = buckets[0]
        .histogram_summary
        .as_ref()
        .expect("histogram_summary must be populated for a histogram metric");
    assert_eq!(hs.count, 10, "observation counts summed across the window");
    assert!((hs.sum - 300.0).abs() < f64::EPSILON);
    assert_eq!(hs.bounds, vec![10.0, 100.0, 250.0]);
    assert_eq!(
        hs.bucket_counts,
        vec![2, 3, 4, 1],
        "bucket counts summed element-wise"
    );
    assert_eq!(hs.min, Some(1.0));
    assert_eq!(hs.max, Some(240.0));
}

#[tokio::test]
async fn query_metrics_cumulative_histogram_uses_latest_not_sum() {
    let Some((storage, _container)) = setup().await else {
        return; // Docker unavailable — skip gracefully.
    };

    let base = Utc::now();
    let mk = |method: &str, count: u64, buckets: Vec<u64>, ts: chrono::DateTime<Utc>| {
        let mut p = MetricPoint::skeleton(
            5005,
            Some(9),
            ResourceInfo {
                service_name: "api".into(),
                service_version: Some("1.0.0".into()),
                deployment_environment: Some("production".into()),
                attributes: BTreeMap::new(),
            },
            "http.server.duration".into(),
            MetricType::Histogram,
            "ms".into(),
            ts,
            {
                let mut m = BTreeMap::new();
                m.insert("http.method".into(), method.to_string());
                m
            },
        );
        p.temporality = Some(AggregationTemporality::Cumulative);
        p.histogram_count = Some(count);
        p.histogram_sum = Some(count as f64 * 10.0);
        p.histogram_min = Some(1.0);
        p.histogram_max = Some(99.0);
        p.histogram_bounds = Some(vec![10.0, 100.0]); // 3 buckets incl +Inf
        p.histogram_bucket_counts = Some(buckets);
        p.value = Some(10.0);
        p
    };

    // CUMULATIVE histograms re-exported within one window (counts are running
    // totals). GET grows 10 -> 20 -> 30 across three exports; POST grows 25 -> 50
    // across two. A correct read must take each series' LATEST snapshot — never
    // sum the re-exports — then sum across the two series.
    let stored = storage
        .store_metrics(vec![
            mk("GET", 10, vec![4, 4, 2], base),
            mk(
                "GET",
                20,
                vec![8, 8, 4],
                base + chrono::Duration::milliseconds(1),
            ),
            mk(
                "GET",
                30,
                vec![12, 12, 6],
                base + chrono::Duration::milliseconds(2),
            ),
            mk(
                "POST",
                25,
                vec![10, 10, 5],
                base + chrono::Duration::milliseconds(1),
            ),
            mk(
                "POST",
                50,
                vec![20, 20, 10],
                base + chrono::Duration::milliseconds(3),
            ),
        ])
        .await
        .expect("store");
    assert_eq!(stored, 5);

    let buckets = storage
        .query_metrics(MetricQuery {
            project_id: 5005,
            metric_name: Some("http.server.duration".into()),
            bucket_interval: Some("1 hour".into()),
            aggregation: MetricAggregation::Avg,
            ..Default::default()
        })
        .await
        .expect("histogram query");
    assert_eq!(buckets.len(), 1);
    let hs = buckets[0]
        .histogram_summary
        .as_ref()
        .expect("histogram_summary populated");
    // GET latest count 30 + POST latest count 50 = 80 (NOT 10+20+30+25+50=135).
    assert_eq!(
        hs.count, 80,
        "cumulative re-exports must collapse to per-series latest, then sum series"
    );
    // GET latest [12,12,6] + POST latest [20,20,10] = [32,32,16].
    assert_eq!(
        hs.bucket_counts,
        vec![32, 32, 16],
        "per-series latest buckets summed across series"
    );
}
