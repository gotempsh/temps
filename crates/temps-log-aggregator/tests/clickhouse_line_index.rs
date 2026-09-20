// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Real-ClickHouse integration tests for the line index (ADR-047).
//!
//! Spins up one shared `clickhouse/clickhouse-server:25.3` testcontainer for
//! the whole file (via a `tokio::sync::OnceCell`), connects
//! `ClickHouseLineIndex` against it, and exercises the writer
//! (`LineIndexSink::index_chunk`/`forget_chunks`/`set_retention_days`) and
//! the reader (`LogAnalytics`) end to end — no mocks, real SQL against a
//! real server. If Docker is not reachable the tests skip gracefully (per
//! CLAUDE.md: Docker tests must NEVER be `#[ignore]`d).
//!
//! `ClickHouseLineIndex::connect` already issues
//! `CREATE DATABASE IF NOT EXISTS` as the first step of `apply_migrations`
//! (see `temps-clickhouse/src/migrations.rs`), and the container image is
//! also started with `CLICKHOUSE_DB` set — so this file does not create the
//! database itself over HTTP; doing so would just be a third, redundant
//! path to the same `CREATE DATABASE IF NOT EXISTS`.

use std::sync::Arc;

use chrono::{Duration, Utc};
use serde_json::json;

use temps_clickhouse::ClickHouseConfig;
use temps_log_aggregator::chunk::ChunkLabels;
use temps_log_aggregator::index::analytics::{
    AttrOp, AttrPredicate, GroupKey, LogAnalytics, Metric,
};
use temps_log_aggregator::index::clickhouse::ClickHouseLineIndex;
use temps_log_aggregator::index::{IndexOutcome, LineIndexSink};
use temps_log_aggregator::store::{FacetField, LogAccessScope, LogQuery, LogSourceKind};
use temps_log_aggregator::types::{LogLevel, LogLine, LogStream};

// ── Shared container ────────────────────────────────────────────────────

/// Start the one ClickHouse container this file shares across every test,
/// or return `None` when Docker is unavailable so callers can skip.
async fn start_container() -> Option<ClickHouseConfig> {
    use testcontainers::{
        core::{wait::HttpWaitStrategy, ContainerPort, WaitFor},
        runners::AsyncRunner,
        GenericImage, ImageExt,
    };

    let image = GenericImage::new("clickhouse/clickhouse-server", "25.3")
        .with_exposed_port(ContainerPort::Tcp(8123))
        // The clickhouse-server image writes "Ready for connections" only to
        // its in-container log file, never to stdout/stderr, so a
        // log-message wait always times out and silently skips. Wait on the
        // HTTP /ping endpoint (200 "Ok." once the server accepts queries).
        .with_wait_for(WaitFor::http(
            HttpWaitStrategy::new("/ping")
                .with_port(ContainerPort::Tcp(8123))
                .with_expected_status_code(200u16),
        ))
        .with_env_var("CLICKHOUSE_DB", "temps_test")
        // Do NOT set CLICKHOUSE_USER=default (the image's user-init then
        // rejects the pre-existing default user) and do NOT use an empty
        // password (an empty CLICKHOUSE_PASSWORD leaves `default`
        // unauthenticatable).
        .with_env_var("CLICKHOUSE_PASSWORD", "test");

    let container = match image.start().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Skipping ClickHouse line-index test: cannot start container ({e})");
            return None;
        }
    };

    let host_port = match container.get_host_port_ipv4(8123).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Skipping ClickHouse line-index test: cannot get host port ({e})");
            return None;
        }
    };

    // Leak the container handle for the lifetime of this test binary: one
    // shared container for the whole file, torn down (by testcontainers'
    // reaper) when the process exits.
    Box::leak(Box::new(container));

    let url = format!("http://127.0.0.1:{host_port}");
    let probe = ::clickhouse::Client::default()
        .with_url(&url)
        .with_database("temps_test")
        .with_user("default")
        .with_password("test");

    let mut last_err = String::new();
    for _ in 0..30 {
        match probe.query("SELECT 1").execute().await {
            Ok(_) => {
                last_err.clear();
                break;
            }
            Err(e) => {
                last_err = e.to_string();
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }
    if !last_err.is_empty() {
        eprintln!("Skipping ClickHouse line-index test: server never became ready ({last_err})");
        return None;
    }

    Some(ClickHouseConfig::new(url, "temps_test", "default", "test"))
}

/// Get (or lazily start) the shared container's config. `None` means Docker
/// is unavailable and the caller should skip.
async fn shared_config() -> Option<&'static ClickHouseConfig> {
    static CELL: tokio::sync::OnceCell<Option<ClickHouseConfig>> =
        tokio::sync::OnceCell::const_new();
    CELL.get_or_init(start_container).await.as_ref()
}

// ── Fixtures ─────────────────────────────────────────────────────────────

fn labels(
    project_id: i32,
    service: &str,
    env: &str,
    container_id: &str,
    deploy_id: i32,
) -> ChunkLabels {
    let now = Utc::now();
    ChunkLabels {
        project_id,
        external_service_id: None,
        env: env.into(),
        service: service.into(),
        container_id: container_id.into(),
        deploy_id: Some(deploy_id),
        node_id: None,
        node_name: None,
        started_at: now,
        ended_at: now,
        line_count: 0,
        level_mask: 0,
        level_counts: [0; 5],
    }
}

/// A log line carrying the canonical `request_id`/`status_code`/`duration_ms`
/// attributes plus the dynamic `worker`/`cache` attributes exercised by the
/// facet/attribute-key tests below.
#[allow(clippy::too_many_arguments)]
fn mkline(
    ts: chrono::DateTime<Utc>,
    level: LogLevel,
    worker: &str,
    cache: &str,
    request_id: &str,
    status_code: u64,
    duration_ms: f64,
) -> LogLine {
    LogLine {
        ts,
        stream: LogStream::Stdout,
        level,
        msg: "line".into(),
        fields: Some(json!({
            "worker": worker,
            "cache": cache,
            "request_id": request_id,
            "status_code": status_code,
            "duration_ms": duration_ms,
        })),
        container_id: "unused".into(),
        service: "unused".into(),
        env: "unused".into(),
        project_id: 0,
        external_service_id: None,
        deploy_id: None,
        node_id: None,
        node_name: None,
    }
}

fn scoped_query(project_ids: Vec<i32>) -> LogQuery {
    LogQuery {
        scope: LogAccessScope::Allowed {
            project_ids,
            external_service_ids: vec![],
        },
        start_time: Utc::now() - Duration::hours(1),
        end_time: Utc::now() + Duration::hours(1),
        source: LogSourceKind::Collected,
        selection: None,
        levels: vec![],
        envs: vec![],
        services: vec![],
        container_ids: vec![],
        node_ids: vec![],
        deploy_id: None,
        text: None,
        before: None,
        limit: 100,
        context_lines: 0,
        attrs: vec![],
        chunk_seqs: None,
    }
}

#[derive(::clickhouse::Row, serde::Deserialize)]
struct CountRow {
    n: u64,
}

async fn row_count(idx: &ClickHouseLineIndex, where_clause: &str) -> u64 {
    idx.client()
        .query(&format!(
            "SELECT count() AS n FROM log_lines_index WHERE {where_clause}"
        ))
        .fetch_one::<CountRow>()
        .await
        .expect("row count query")
        .n
}

// ── 1. connect: migration + idempotency ─────────────────────────────────

#[tokio::test]
async fn connect_applies_migration_and_is_idempotent() {
    let Some(cfg) = shared_config().await else {
        eprintln!("Skipping: Docker unavailable");
        return;
    };

    let idx = ClickHouseLineIndex::connect(cfg)
        .await
        .expect("first connect should apply migration 0001 and version-gate the server");
    assert!(
        idx.version().at_least(25, 3),
        "container reports {}, expected >= 25.3",
        idx.version()
    );

    let tracked_after_first = idx
        .client()
        .query(
            "SELECT count() AS n FROM _temps_ch_log_index_migrations \
             WHERE name = '0001_log_lines_index'",
        )
        .fetch_one::<CountRow>()
        .await
        .expect("query tracking table")
        .n;
    assert_eq!(
        tracked_after_first, 1,
        "migration 0001 recorded exactly once"
    );

    // Second connect against the same server must be idempotent: no
    // re-application, no duplicate tracking row (the "skipped=1" path).
    let idx2 = ClickHouseLineIndex::connect(cfg)
        .await
        .expect("second connect should be idempotent");
    assert_eq!(idx2.version(), idx.version());

    let tracked_after_second = idx2
        .client()
        .query(
            "SELECT count() AS n FROM _temps_ch_log_index_migrations \
             WHERE name = '0001_log_lines_index'",
        )
        .fetch_one::<CountRow>()
        .await
        .expect("query tracking table again")
        .n;
    assert_eq!(
        tracked_after_second, 1,
        "second connect must not duplicate the tracking row"
    );
}

// ── 2 & 3. index_chunk + analytics over the indexed data ────────────────

#[tokio::test]
async fn index_chunk_and_analytics_over_real_clickhouse() {
    let Some(cfg) = shared_config().await else {
        eprintln!("Skipping: Docker unavailable");
        return;
    };
    let idx = ClickHouseLineIndex::connect(cfg).await.expect("connect");

    let base = Utc::now();

    let labels_checkout = labels(9101, "checkout", "prod", "ch-checkout", 11);
    let lines_checkout = vec![
        mkline(base, LogLevel::Info, "1", "hit", "", 200, 12.5),
        mkline(
            base + Duration::milliseconds(1),
            LogLevel::Info,
            "2",
            "miss",
            "",
            200,
            20.0,
        ),
        mkline(
            base + Duration::milliseconds(2),
            LogLevel::Error,
            "3",
            "hit",
            "req-unique-777",
            500,
            99.9,
        ),
    ];

    let labels_worker = labels(9102, "worker-svc", "prod", "ch-worker", 12);
    let lines_worker = vec![
        mkline(
            base + Duration::milliseconds(3),
            LogLevel::Info,
            "4",
            "miss",
            "",
            200,
            5.0,
        ),
        mkline(
            base + Duration::milliseconds(4),
            LogLevel::Info,
            "5",
            "hit",
            "",
            200,
            8.0,
        ),
    ];

    let labels_api = labels(9103, "api", "staging", "ch-api", 13);
    let lines_api = vec![mkline(
        base + Duration::milliseconds(5),
        LogLevel::Warn,
        "6",
        "miss",
        "",
        404,
        3.0,
    )];

    let seg_checkout = Arc::new(lines_checkout);
    let seg_worker = Arc::new(lines_worker);
    let seg_api = Arc::new(lines_api);

    let out = idx
        .index_chunk(501, &labels_checkout, std::slice::from_ref(&seg_checkout))
        .await
        .expect("index chunk A");
    assert_eq!(out, IndexOutcome::Indexed);
    let out = idx
        .index_chunk(502, &labels_worker, std::slice::from_ref(&seg_worker))
        .await
        .expect("index chunk B");
    assert_eq!(out, IndexOutcome::Indexed);
    let out = idx
        .index_chunk(503, &labels_api, std::slice::from_ref(&seg_api))
        .await
        .expect("index chunk C");
    assert_eq!(out, IndexOutcome::Indexed);

    let total = row_count(&idx, "project_id IN (9101, 9102, 9103)").await;
    assert_eq!(total, 6, "row count must equal the number of lines indexed");

    // Re-index chunk A (the seal pipeline is idempotent, so this happens on
    // retry): duplicate rows must collapse away on merge, never double-count.
    let out = idx
        .index_chunk(501, &labels_checkout, std::slice::from_ref(&seg_checkout))
        .await
        .expect("reindex chunk A");
    assert_eq!(out, IndexOutcome::Indexed);

    idx.client()
        .query("OPTIMIZE TABLE log_lines_index FINAL")
        .execute()
        .await
        .expect("optimize table");

    let chunk_a_count = row_count(&idx, "chunk_seq = 501").await;
    assert_eq!(
        chunk_a_count, 3,
        "re-indexing the same chunk must not double its rows after a merge"
    );
    let total_after = row_count(&idx, "project_id IN (9101, 9102, 9103)").await;
    assert_eq!(
        total_after, 6,
        "total row count must stay at 6 after the merge"
    );

    // ── Analytics ────────────────────────────────────────────────────
    let query = scoped_query(vec![9101, 9102, 9103]);

    let keys = idx
        .attribute_keys(&query, 50)
        .await
        .expect("attribute_keys");
    let key_names: Vec<&str> = keys.iter().map(|k| k.value.as_str()).collect();
    assert!(
        key_names.contains(&"worker"),
        "dynamic key 'worker' must be listed: {key_names:?}"
    );
    assert!(
        key_names.contains(&"cache"),
        "dynamic key 'cache' must be listed: {key_names:?}"
    );
    for canonical in [
        "request_id",
        "status_code",
        "duration_ms",
        "trace_id",
        "span_id",
        "http_method",
        "http_route",
    ] {
        assert!(
            !key_names.contains(&canonical),
            "canonical key {canonical:?} must not appear in attribute_keys: {key_names:?}"
        );
    }

    let facets = idx
        .facets(
            &query,
            &[],
            &[
                GroupKey::Attr("worker".into()),
                GroupKey::Label(FacetField::Service),
            ],
            50,
        )
        .await
        .expect("facets");
    let worker_values = facets.get("worker").expect("worker facet present");
    assert_eq!(
        worker_values.iter().map(|v| v.count).sum::<i64>(),
        6,
        "worker facet counts must sum to the indexed line count: {worker_values:?}"
    );
    let service_values = facets.get("service").expect("service facet present");
    let service_names: std::collections::BTreeSet<&str> =
        service_values.iter().map(|v| v.value.as_str()).collect();
    assert_eq!(
        service_names,
        ["api", "checkout", "worker-svc"].into_iter().collect(),
        "service facet must list every indexed service"
    );

    let hist = idx
        .histogram(&query, &[], 3600, None, 5)
        .await
        .expect("histogram");
    let hist_total: i64 = hist.iter().map(|b| b.count).sum();
    assert_eq!(
        hist_total, 6,
        "histogram bucket counts must sum to the indexed line count"
    );

    let p95 = idx
        .aggregate(
            &query,
            &[],
            &[GroupKey::Label(FacetField::Service)],
            &Metric::P95("duration_ms".into()),
            10,
        )
        .await
        .expect("p95 aggregate");
    assert!(!p95.is_empty(), "p95 must return one row per service");
    assert!(
        p95.iter().all(|r| r.value.is_finite()),
        "p95(duration_ms) must be finite for every service: {p95:?}"
    );

    let pointers = idx
        .search_pointers(
            &query,
            &[AttrPredicate {
                key: "request_id".into(),
                op: AttrOp::Eq,
                value: Some("req-unique-777".into()),
            }],
        )
        .await
        .expect("search_pointers");
    assert_eq!(
        pointers.len(),
        1,
        "exactly one line carries this request_id: {pointers:?}"
    );
    assert_eq!(pointers[0].chunk_seq, 501);

    let matches = idx
        .matching_chunks(
            &query,
            &[AttrPredicate {
                key: "worker".into(),
                op: AttrOp::Eq,
                value: Some("3".into()),
            }],
            10,
        )
        .await
        .expect("matching_chunks");
    assert_eq!(
        matches,
        vec![501],
        "only chunk A has a line with worker = 3"
    );

    // An empty allow-list must never leak data, no matter how it is scoped.
    let denied = scoped_query(vec![]);
    let denied_matches = idx
        .matching_chunks(&denied, &[], 10)
        .await
        .expect("denied matching_chunks");
    assert!(
        denied_matches.is_empty(),
        "empty allow-list must yield no chunks"
    );
    let denied_pointers = idx
        .search_pointers(&denied, &[])
        .await
        .expect("denied search_pointers");
    assert!(
        denied_pointers.is_empty(),
        "empty allow-list must yield no pointers"
    );
}

// ── 4. forget_chunks + set_retention_days ────────────────────────────────

#[tokio::test]
async fn forget_chunks_and_retention_over_real_clickhouse() {
    let Some(cfg) = shared_config().await else {
        eprintln!("Skipping: Docker unavailable");
        return;
    };
    let idx = ClickHouseLineIndex::connect(cfg).await.expect("connect");

    let base = Utc::now();
    let chunk_labels = labels(9201, "tmp-svc", "prod", "ch-tmp", 21);
    let lines = vec![
        mkline(base, LogLevel::Info, "x", "hit", "", 200, 1.0),
        mkline(
            base + Duration::milliseconds(1),
            LogLevel::Info,
            "y",
            "miss",
            "",
            200,
            2.0,
        ),
    ];
    let seg = Arc::new(lines);
    let out = idx
        .index_chunk(601, &chunk_labels, std::slice::from_ref(&seg))
        .await
        .expect("index chunk");
    assert_eq!(out, IndexOutcome::Indexed);

    let before = row_count(&idx, "chunk_seq = 601").await;
    assert_eq!(before, 2, "chunk must be indexed before forgetting it");

    idx.forget_chunks(&[601]).await.expect("forget_chunks");

    let after = row_count(&idx, "chunk_seq = 601").await;
    assert_eq!(after, 0, "forget_chunks must remove that chunk's rows");

    // A forgotten chunk that never existed is a no-op, not an error.
    idx.forget_chunks(&[999_999])
        .await
        .expect("forget_chunks on unknown seq");
    idx.forget_chunks(&[])
        .await
        .expect("forget_chunks on empty list");

    // Retention: MODIFY TTL is applied, and re-applying the same value is a
    // metadata-only no-op (in-process short circuit).
    idx.set_retention_days(7).await.expect("set_retention_days");
    idx.set_retention_days(7)
        .await
        .expect("set_retention_days must be idempotent");

    #[derive(::clickhouse::Row, serde::Deserialize)]
    struct EngineRow {
        engine_full: String,
    }
    let engine = idx
        .client()
        .query(
            "SELECT engine_full FROM system.tables \
             WHERE database = 'temps_test' AND name = 'log_lines_index'",
        )
        .fetch_one::<EngineRow>()
        .await
        .expect("query system.tables");
    assert!(
        engine.engine_full.contains("toIntervalDay(7)"),
        "TTL must be aligned to the new retention (7 days): {}",
        engine.engine_full
    );
}
