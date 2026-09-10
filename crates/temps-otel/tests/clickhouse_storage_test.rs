// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

use chrono::{Duration, Utc};
use sea_orm::{DatabaseBackend, MockDatabase};

use temps_otel::storage::clickhouse::{ClickHouseOtelConfig, ClickHouseOtelStorage};
use temps_otel::storage::timescaledb::TimescaleDbStorage;
use temps_otel::storage::OtelStorage;
use temps_otel::types::{
    AggregationTemporality, MetricAggregation, MetricPoint, MetricQuery, MetricType, ResourceInfo,
    SpanKind, SpanRecord, SpanStatusCode,
};

#[derive(::clickhouse::Row, serde::Deserialize)]
struct ChTypeNameRow {
    type_name: String,
}

/// Start a real ClickHouse testcontainer, wait for it to accept queries, and
/// apply the OTel CH migrations. Returns the connected config + container
/// handle, or `None` when Docker is unavailable (test should skip).
async fn start_ch_container() -> Option<(ClickHouseOtelConfig, Box<dyn std::any::Any + Send>)> {
    use testcontainers::{
        core::{wait::HttpWaitStrategy, ContainerPort, WaitFor},
        runners::AsyncRunner,
        GenericImage, ImageExt,
    };

    // This storage suite exercises the production-generation ClickHouse used
    // by the schema benchmarks. decode_to_store_test remains pinned to 24.8,
    // so together they cover both ends of the supported server range. Newer
    // ClickHouse versions expose bare empty arrays as Array(Nothing).
    let image = GenericImage::new("clickhouse/clickhouse-server", "26.2.5")
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
        // empty CLICKHOUSE_PASSWORD leaves `default` unauthenticatable).
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

    // Prove this test server exposes the production failure mode. A regression
    // to a bare [] projection will therefore fail when the clickhouse client
    // parses the Array(Nothing) response header into a typed Vec<T> field.
    let empty_array_type = probe
        .query("SELECT toTypeName([]) AS type_name")
        .fetch_one::<ChTypeNameRow>()
        .await
        .expect("query empty-array ClickHouse type");
    assert_eq!(empty_array_type.type_name, "Array(Nothing)");

    // Apply OTel CH migrations (spans + metrics). This MUST succeed — it is the
    // entire reason for the test. Assert loudly on failure.
    temps_otel::storage::clickhouse::migrations::apply_migrations(&probe, "temps_otel_test")
        .await
        .expect("apply_migrations failed against testcontainer ClickHouse");

    Some((config, Box::new(container)))
}

/// Container handle + a connected `ClickHouseOtelStorage`. Returns `None` when
/// Docker is unavailable so the test can skip without failing CI.
async fn setup() -> Option<(ClickHouseOtelStorage, Box<dyn std::any::Any + Send>)> {
    let (config, container) = start_ch_container().await?;

    // The inner Timescale storage is never exercised by the metric methods under
    // test; a MockDatabase satisfies the constructor without a Postgres server.
    // The trace-refs test IS an exception: `get_trace_ref_projects` unions the
    // ClickHouse rows with the inner Postgres table, so a stack of empty query
    // results is queued for those lookups (unused results are harmless to the
    // metric tests, which never touch the mock).
    let empty_pg_result = || Vec::<std::collections::BTreeMap<String, sea_orm::Value>>::new();
    let mock_db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(std::iter::repeat_with(empty_pg_result).take(8))
        .into_connection();
    let inner = Arc::new(TimescaleDbStorage::new(Arc::new(mock_db), None));

    let storage = ClickHouseOtelStorage::new(
        config,
        inner,
        Arc::new(temps_core::FixedRetentionResolver),
        None,
    );
    Some((storage, container))
}

/// Same as [`setup`], but the inner `TimescaleDbStorage` is configured with a
/// real per-project quota limit (`quota_bytes_per_project`), and its
/// `MockDatabase` is pre-loaded with `logs_rows` queued responses to
/// `hypertable_bytes_for_project("otel_log_events", ...)` — one per
/// `get_storage_quota`/`check_quota` call the test makes. Used by the
/// storage-quota regression test below, which needs `get_storage_quota` to
/// take its real (non-early-exit) path.
async fn setup_with_quota(
    limit_bytes: u64,
    logs_bytes_per_call: i64,
    calls: usize,
) -> Option<(ClickHouseOtelStorage, Box<dyn std::any::Any + Send>)> {
    let (config, container) = start_ch_container().await?;

    let log_row =
        move || BTreeMap::from([("total_bytes", sea_orm::Value::from(logs_bytes_per_call))]);
    let mock_db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(std::iter::repeat_with(move || vec![log_row()]).take(calls))
        .into_connection();
    let inner = Arc::new(TimescaleDbStorage::with_config(
        Arc::new(mock_db),
        None,
        7,
        Some(limit_bytes),
        None,
    ));

    let storage = ClickHouseOtelStorage::new(
        config,
        inner,
        Arc::new(temps_core::FixedRetentionResolver),
        None,
    );
    Some((storage, container))
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

/// Round-trip for the ADR-027 cross-project trace ref index on the ClickHouse
/// backend (`0006_trace_refs.sql`): record refs, re-record the same pair
/// (must dedupe, not duplicate), and look them up per trace_id. The lookup
/// unions with the inner Postgres storage — the queued-empty MockDatabase in
/// `setup()` stands in for a drained legacy table.
#[tokio::test]
async fn trace_refs_roundtrip_record_and_lookup() {
    let Some((storage, _container)) = setup().await else {
        return; // Docker unavailable — skip gracefully.
    };

    let t1 = "4bf92f3577b34da6a3ce929d0e0e4736".to_string();
    let t2 = "abcdef1234567890abcdef1234567890".to_string();

    storage
        .record_trace_refs(&[t1.clone(), t2.clone()], 1)
        .await
        .expect("record refs for project 1");
    // Re-recording an existing (trace_id, project_id) pair must be a no-op
    // at read time (GROUP BY + ReplacingMergeTree dedup).
    storage
        .record_trace_refs(std::slice::from_ref(&t1), 1)
        .await
        .expect("re-record same pair");
    storage
        .record_trace_refs(std::slice::from_ref(&t1), 2)
        .await
        .expect("record ref for project 2");

    let refs = storage
        .get_trace_ref_projects(&t1)
        .await
        .expect("lookup shared trace");
    let mut projects: Vec<i32> = refs.iter().map(|r| r.project_id).collect();
    projects.sort_unstable();
    assert_eq!(projects, vec![1, 2], "one entry per project, no duplicates");
    for r in &refs {
        // Sanity: first_seen decoded from DateTime64(3) into a real timestamp.
        assert!(r.first_seen.timestamp() > 1_500_000_000);
    }

    let refs_t2 = storage
        .get_trace_ref_projects(&t2)
        .await
        .expect("lookup single-project trace");
    assert_eq!(refs_t2.len(), 1);
    assert_eq!(refs_t2[0].project_id, 1);

    // Unknown trace_id resolves to an empty list, never an error.
    let none = storage
        .get_trace_ref_projects("00000000000000000000000000000000")
        .await
        .expect("lookup unknown trace");
    assert!(none.is_empty());
}

// ── Storage quota (ClickHouse-native span/metric accounting) ───────────────

/// Cheap xorshift64-based pseudo-random hex string generator. Used to give
/// each test span's attribute values high entropy (unlike a fixed repeated
/// filler string, which ZSTD — the codec ClickHouse compresses these parts
/// with — collapses to a near-zero footprint regardless of row count,
/// making it useless for asserting on real on-disk bytes).
fn pseudo_random_hex(seed: u64, len: usize) -> String {
    let mut state = seed ^ 0x9E37_79B9_7F4A_7C15;
    let mut out = String::with_capacity(len + 16);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push_str(&format!("{state:016x}"));
    }
    out.truncate(len);
    out
}

/// Build a sample SpanRecord with a sizeable, high-entropy `attributes`
/// payload, so a batch of these produces real, measurable (not
/// compressed-away) ClickHouse part bytes.
fn quota_test_span(project_id: i32, index: u32) -> SpanRecord {
    let start = Utc::now() - Duration::milliseconds(10);
    let mut attributes = BTreeMap::new();
    for key_idx in 0..20u32 {
        let seed = (index as u64) * 1000 + key_idx as u64;
        attributes.insert(
            format!("attribute.key.number.{key_idx}"),
            pseudo_random_hex(seed, 200),
        );
    }
    SpanRecord {
        project_id,
        deployment_id: None,
        resource: ResourceInfo {
            service_name: "quota-test-service".into(),
            service_version: Some("1.0.0".into()),
            deployment_environment: Some("test".into()),
            attributes: BTreeMap::new(),
        },
        trace_id: format!("{index:032x}"),
        span_id: format!("{index:016x}"),
        parent_span_id: None,
        name: "quota-load-span".into(),
        kind: SpanKind::Internal,
        start_time: start,
        end_time: start + Duration::milliseconds(10),
        duration_ms: 10.0,
        status_code: SpanStatusCode::Ok,
        status_message: String::new(),
        attributes,
        events: vec![],
    }
}

/// Regression test for the ClickHouse-backed storage-quota gap: before this
/// fix, `ClickHouseOtelStorage::get_storage_quota`/`check_quota` delegated
/// straight to the inner `TimescaleDbStorage`, which sums Postgres
/// `otel_spans`/`otel_metrics`/`otel_log_events` hypertables — tables a
/// ClickHouse-enabled install never writes span/metric rows into (`spans`
/// and `metrics` are ClickHouse-native; see `store_spans`/`store_metrics`).
/// Quota enforcement was therefore silently inert for every CH-backed
/// project regardless of real ingested volume.
///
/// This test proves the fix against a REAL ClickHouse testcontainer: insert
/// real span rows via `store_spans` (the production ingest path, not a
/// synthetic byte count), and assert `get_storage_quota`/`check_quota`
/// actually track that ClickHouse-native volume.
#[tokio::test]
async fn test_storage_quota_tracks_real_clickhouse_span_volume() {
    let project_id = 777;

    // 4 get_storage_quota/check_quota calls below each consume one queued
    // mock logs-share response (see setup_with_quota): the empty-project
    // check, the post-ingest get_storage_quota, check_quota's internal
    // get_storage_quota, and the unrelated-project check.
    let Some((storage, _container)) = setup_with_quota(300 * 1024, 0, 4).await else {
        return; // Docker unavailable — skip gracefully.
    };

    // ── Phase 1: before any data, with a quota configured, usage is zero. ──
    let empty_quota = storage
        .get_storage_quota(project_id)
        .await
        .expect("get_storage_quota on empty project");
    assert_eq!(
        empty_quota.total_bytes, 0,
        "no spans/metrics ingested yet for this project"
    );

    // ── Phase 2: insert real span volume, then a tiny limit must trip. ──
    // Each span carries ~4KB of high-entropy (pseudo-random) attributes (20
    // keys x 200 bytes) -- unlike a repeated filler string, this survives
    // ZSTD compression well enough that ~500 of them push real ClickHouse
    // part bytes comfortably past the 300KB test limit below.
    // `logs_bytes_per_call` is mocked to 0 (see `setup_with_quota`) so every
    // byte this test observes comes from the ClickHouse-native
    // spans/metrics accounting under test, not the inner Postgres delegate.
    let spans: Vec<SpanRecord> = (0..500u32)
        .map(|i| quota_test_span(project_id, i))
        .collect();
    let stored = storage
        .store_spans(spans)
        .await
        .expect("store_spans should succeed");
    assert_eq!(stored, 500);

    let quota = storage
        .get_storage_quota(project_id)
        .await
        .expect("get_storage_quota after real ingest");
    assert!(
        quota.total_bytes > 300 * 1024,
        "expected ClickHouse-native total_bytes to exceed the 300KB test limit after \
         inserting ~500 spans with ~4KB of high-entropy attributes each (got {} bytes) -- \
         this is exactly the regression this fix protects against: a plain delegation to \
         the inner TimescaleDbStorage would report ~0 bytes here, since the \
         ClickHouse-backed Postgres otel_spans table never receives these rows",
        quota.total_bytes
    );
    assert!(
        quota.usage_pct >= 100.0,
        "usage_pct should have crossed 100% of the 300KB limit, got {}",
        quota.usage_pct
    );

    let exceeded = storage
        .check_quota(project_id)
        .await
        .expect("check_quota after real ingest");
    assert!(
        exceeded,
        "check_quota must trip once real ClickHouse-ingested span volume exceeds the \
         configured limit"
    );

    // A different, unrelated project sees none of this volume.
    let other_quota = storage
        .get_storage_quota(999_999)
        .await
        .expect("get_storage_quota for unrelated project");
    assert_eq!(
        other_quota.total_bytes, 0,
        "an unrelated project must not see this project's ClickHouse-ingested bytes"
    );
}
