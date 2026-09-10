// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Real-ClickHouse integration test for `ClickHouseOtelStorage::query_spans`.
//!
//! `query_spans` is assembled by hand: predicates are accumulated into a string
//! of `?` placeholders while the matching values are pushed onto a parallel
//! `Vec`, and the two must stay in lockstep. It then picks between two SQL
//! shapes — a single-stage read when the query is anchored on a `trace_id` (the
//! sort-key prefix already applies), and a two-stage read otherwise, where the
//! outer statement repeats the project and time predicates BEFORE the subquery
//! contributes its own binds.
//!
//! That ordering is invisible to the type system. Getting it wrong does not
//! fail to compile and usually does not error at runtime — ClickHouse happily
//! binds a limit where a project_id was meant and returns confidently wrong
//! rows. These tests exist to catch exactly that, which is why the central case
//! sets EVERY optional filter at once rather than testing them one at a time.
//!
//! Docker-dependent, and per CLAUDE.md must never be `#[ignore]`d: it detects an
//! unavailable Docker at runtime and returns.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{Duration, Utc};
use sea_orm::{DatabaseBackend, MockDatabase};

use temps_otel::storage::clickhouse::{ClickHouseOtelConfig, ClickHouseOtelStorage};
use temps_otel::storage::timescaledb::TimescaleDbStorage;
use temps_otel::storage::OtelStorage;
use temps_otel::types::{
    ResourceInfo, SortOrder, SpanKind, SpanRecord, SpanStatsQuery, SpanStatsSortField,
    SpanStatusCode, TraceQuery, TraceSortField,
};

const PROJECT: i32 = 42;
/// A second project whose rows must never leak into PROJECT's results.
const OTHER_PROJECT: i32 = 99;

/// Spin up ClickHouse, run the OTel migrations, return a connected storage.
/// `None` when Docker is unreachable so the test skips instead of failing CI.
type Harness = (
    ClickHouseOtelStorage,
    ::clickhouse::Client,
    Box<dyn std::any::Any + Send>,
);

async fn setup() -> Option<Harness> {
    use testcontainers::{
        core::{wait::HttpWaitStrategy, ContainerPort, WaitFor},
        runners::AsyncRunner,
        GenericImage, ImageExt,
    };

    // Pinned to 24.8 deliberately: this is the generation where projections on
    // a ReplacingMergeTree require `deduplicate_merge_projection_mode`, which
    // 0007_spans_recent_projection.sql sets. Testing on a newer server would
    // not exercise that constraint.
    let image = GenericImage::new("clickhouse/clickhouse-server", "24.8")
        .with_exposed_port(ContainerPort::Tcp(8123))
        // The image logs "Ready for connections" only to its in-container log
        // file, so a log wait always times out; poll /ping instead.
        .with_wait_for(WaitFor::http(
            HttpWaitStrategy::new("/ping")
                .with_port(ContainerPort::Tcp(8123))
                .with_expected_status_code(200u16),
        ))
        .with_env_var("CLICKHOUSE_DB", "spans_query_test")
        .with_env_var("CLICKHOUSE_PASSWORD", "test");

    let container = match image.start().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Skipping query_spans test: cannot start container ({e})");
            return None;
        }
    };
    let host_port = match container.get_host_port_ipv4(8123).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Skipping query_spans test: cannot get host port ({e})");
            return None;
        }
    };

    let url = format!("http://127.0.0.1:{host_port}");
    let probe = ::clickhouse::Client::default()
        .with_url(&url)
        .with_database("spans_query_test")
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
                last_err = format!("{e}");
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }
    if !last_err.is_empty() {
        eprintln!("Skipping query_spans test: server never became ready ({last_err})");
        return None;
    }

    temps_otel::storage::clickhouse::migrations::apply_migrations(&probe, "spans_query_test")
        .await
        .expect("apply_migrations failed against testcontainer ClickHouse");

    // query_spans reads ClickHouse only; the inner Postgres store is never hit.
    let inner = Arc::new(TimescaleDbStorage::new(
        Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        None,
    ));
    let storage = ClickHouseOtelStorage::new(
        ClickHouseOtelConfig::new(&url, "spans_query_test", "default", "test"),
        inner,
        Arc::new(temps_core::FixedRetentionResolver),
        None,
    );
    Some((storage, probe, Box::new(container)))
}

#[allow(clippy::too_many_arguments)]
fn span(
    project_id: i32,
    trace: &str,
    span_id: &str,
    parent: Option<&str>,
    name: &str,
    service: &str,
    status: SpanStatusCode,
    minutes_ago: i64,
    duration_ms: f64,
    deployment_id: Option<i32>,
    attributes: &[(&str, &str)],
) -> SpanRecord {
    let start = Utc::now() - Duration::minutes(minutes_ago);
    SpanRecord {
        project_id,
        deployment_id,
        resource: ResourceInfo {
            service_name: service.into(),
            service_version: Some("1.0.0".into()),
            deployment_environment: Some("production".into()),
            ..Default::default()
        },
        trace_id: trace.into(),
        span_id: span_id.into(),
        parent_span_id: parent.map(|p| p.to_string()),
        name: name.into(),
        kind: SpanKind::Server,
        start_time: start,
        end_time: start + Duration::milliseconds(duration_ms as i64),
        duration_ms,
        status_code: status,
        status_message: String::new(),
        attributes: attributes
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
        events: vec![],
    }
}

/// Fixture: three traces in PROJECT plus a decoy trace in OTHER_PROJECT.
/// Root spans are the ones with `parent = None`.
fn fixture() -> Vec<SpanRecord> {
    vec![
        // Trace A — newest root, matches the "everything" filter below.
        span(
            PROJECT,
            "aaaa1111",
            "a001",
            None,
            "GET /api/checkout",
            "checkout",
            SpanStatusCode::Error,
            5,
            250.0,
            Some(7),
            &[("gen_ai.system", "openai")],
        ),
        span(
            PROJECT,
            "aaaa1111",
            "a002",
            Some("a001"),
            "SELECT orders",
            "checkout",
            SpanStatusCode::Ok,
            5,
            12.0,
            Some(7),
            &[],
        ),
        // Trace B — middle root, wrong service/status for the "everything" filter.
        span(
            PROJECT,
            "bbbb2222",
            "b001",
            None,
            "POST /api/login",
            "auth",
            SpanStatusCode::Ok,
            30,
            90.0,
            Some(8),
            &[],
        ),
        // Trace C — oldest root, deliberately outside the 1h window used below.
        span(
            PROJECT,
            "cccc3333",
            "c001",
            None,
            "GET /api/legacy",
            "checkout",
            SpanStatusCode::Error,
            240,
            900.0,
            Some(7),
            &[("gen_ai.system", "openai")],
        ),
        // Decoy: same shape, different project. Must never appear.
        span(
            OTHER_PROJECT,
            "dddd4444",
            "d001",
            None,
            "GET /api/checkout",
            "checkout",
            SpanStatusCode::Error,
            5,
            250.0,
            Some(7),
            &[("gen_ai.system", "openai")],
        ),
    ]
}

async fn seeded() -> Option<Harness> {
    let (storage, probe, container) = setup().await?;
    storage
        .store_spans(fixture())
        .await
        .expect("store fixture spans");
    Some((storage, probe, container))
}

fn ids(spans: &[SpanRecord]) -> Vec<String> {
    spans.iter().map(|s| s.span_id.clone()).collect()
}

/// The two-stage path: no trace_id, so ordering and the LIMIT are resolved
/// against the projection and the wide rows fetched by primary key.
#[tokio::test]
async fn query_spans_two_stage_returns_roots_newest_first_within_window() {
    let Some((storage, _probe, _c)) = seeded().await else {
        return;
    };

    let spans = storage
        .query_spans(TraceQuery {
            project_id: PROJECT,
            start_time: Some(Utc::now() - Duration::hours(1)),
            root_only: true,
            limit: Some(100),
            ..Default::default()
        })
        .await
        .expect("two-stage query");

    // Trace C's root is 4h old — outside the window. The decoy belongs to
    // another project. Roots only, so a002 is excluded.
    assert_eq!(
        ids(&spans),
        vec!["a001", "b001"],
        "expected only in-window roots for this project, newest first"
    );
    assert!(
        spans.iter().all(|s| s.project_id == PROJECT),
        "another project's spans leaked into the result"
    );
}

/// The single-stage path: a trace_id anchors the query on the sort-key prefix,
/// so it must return every span of that trace, children included.
#[tokio::test]
async fn query_spans_single_stage_by_trace_id_returns_all_spans_of_the_trace() {
    let Some((storage, _probe, _c)) = seeded().await else {
        return;
    };

    let spans = storage
        .query_spans(TraceQuery {
            project_id: PROJECT,
            trace_id: Some("aaaa1111".into()),
            limit: Some(100),
            ..Default::default()
        })
        .await
        .expect("single-stage query");

    let mut got = ids(&spans);
    got.sort();
    assert_eq!(
        got,
        vec!["a001", "a002"],
        "trace lookup must return the root AND its children"
    );
}

/// THE bind-ordering test.
///
/// Every optional predicate is set at once, so the rendered statement carries
/// binds in the outer-project / outer-time / inner-project / inner-filters /
/// limit / offset sequence. If any value is bound out of position the filters
/// silently match the wrong rows, so this asserts on an exact, uniquely
/// determined answer: only trace A's root satisfies all of these together.
#[tokio::test]
async fn query_spans_binds_stay_aligned_with_every_filter_set() {
    let Some((storage, _probe, _c)) = seeded().await else {
        return;
    };

    let spans = storage
        .query_spans(TraceQuery {
            project_id: PROJECT,
            service_name: Some("checkout".into()),
            status: Some(SpanStatusCode::Error),
            min_duration_ms: Some(100.0),
            start_time: Some(Utc::now() - Duration::hours(1)),
            end_time: Some(Utc::now() + Duration::minutes(1)),
            deployment_id: Some(7),
            attributes: Some(BTreeMap::from([(
                "gen_ai.system".to_string(),
                "openai".to_string(),
            )])),
            name_pattern: Some("checkout".into()),
            root_only: true,
            limit: Some(50),
            ..Default::default()
        })
        .await
        .expect("fully-filtered query");

    assert_eq!(
        ids(&spans),
        vec!["a001"],
        "only trace A's root matches every filter simultaneously"
    );
}

/// Same predicate reached through both SQL shapes must agree. This is the
/// cross-check that the two-stage rewrite did not change semantics: adding a
/// trace_id flips the implementation from two-stage to single-stage, and the
/// row it yields must be the one the two-stage form already returned.
#[tokio::test]
async fn query_spans_two_stage_and_single_stage_agree() {
    let Some((storage, _probe, _c)) = seeded().await else {
        return;
    };

    let base = TraceQuery {
        project_id: PROJECT,
        start_time: Some(Utc::now() - Duration::hours(1)),
        root_only: true,
        limit: Some(100),
        ..Default::default()
    };

    let two_stage = storage
        .query_spans(TraceQuery { ..base.clone() })
        .await
        .expect("two-stage");
    let single_stage = storage
        .query_spans(TraceQuery {
            trace_id: Some("aaaa1111".into()),
            ..base
        })
        .await
        .expect("single-stage");

    assert_eq!(ids(&single_stage), vec!["a001"]);
    assert!(
        two_stage.iter().any(|s| s.span_id == "a001"),
        "the span the trace-anchored form returns must also be in the unanchored result"
    );

    // The wide columns must survive the two-stage round trip — the whole point
    // of stage 2 is fetching them, and a broken IN-join would blank them out.
    let a001 = two_stage
        .iter()
        .find(|s| s.span_id == "a001")
        .expect("a001 present");
    assert_eq!(a001.name, "GET /api/checkout");
    assert_eq!(a001.resource.service_name, "checkout");
    assert_eq!(a001.status_code, SpanStatusCode::Error);
    assert_eq!(a001.duration_ms, 250.0);
    assert_eq!(
        a001.attributes.get("gen_ai.system").map(String::as_str),
        Some("openai"),
        "attributes JSON must be hydrated by the outer stage"
    );
}

/// LIMIT and OFFSET are bound last in both shapes — a misplaced bind here
/// shows up as a wrong-sized page rather than an error.
#[tokio::test]
async fn query_spans_limit_and_offset_page_the_two_stage_result() {
    let Some((storage, _probe, _c)) = seeded().await else {
        return;
    };

    let q = |limit: u64, offset: u64| TraceQuery {
        project_id: PROJECT,
        root_only: true,
        limit: Some(limit),
        offset: Some(offset),
        ..Default::default()
    };

    // Three roots in this project, newest first: a001, b001, c001.
    let page1 = storage.query_spans(q(2, 0)).await.expect("page 1");
    let page2 = storage.query_spans(q(2, 2)).await.expect("page 2");

    assert_eq!(ids(&page1), vec!["a001", "b001"]);
    assert_eq!(ids(&page2), vec!["c001"]);
}

/// Sorting by duration uses a column the projection does not carry, so the
/// planner must fall back to the base table and still order correctly.
#[tokio::test]
async fn query_spans_sorts_by_duration_when_asked() {
    let Some((storage, _probe, _c)) = seeded().await else {
        return;
    };

    let spans = storage
        .query_spans(TraceQuery {
            project_id: PROJECT,
            root_only: true,
            sort_by: TraceSortField::Duration,
            limit: Some(100),
            ..Default::default()
        })
        .await
        .expect("duration-sorted query");

    // c001 900ms > a001 250ms > b001 90ms
    assert_eq!(ids(&spans), vec!["c001", "a001", "b001"]);
}

/// 0007 must actually install the projection — without it the two-stage query
/// still returns correct rows, just slowly, so no other test would notice.
#[tokio::test]
async fn migration_installs_the_recent_spans_projection() {
    let Some((_storage, probe, _c)) = setup().await else {
        return;
    };

    let ddl = probe
        .query("SHOW CREATE TABLE spans_query_test.spans")
        .fetch_one::<String>()
        .await
        .expect("SHOW CREATE TABLE");

    assert!(
        ddl.contains("PROJECTION proj_recent"),
        "proj_recent missing — the span feed silently loses its index:\n{ddl}"
    );
    assert!(
        ddl.contains("deduplicate_merge_projection_mode = 'rebuild'"),
        "ReplacingMergeTree needs this setting or ClickHouse rejects the \
         projection outright (error 344):\n{ddl}"
    );
}

/// The page must never be larger than the caller asked for, even when the table
/// holds pre-merge duplicates.
///
/// `spans` is a ReplacingMergeTree and these reads deliberately do not use
/// FINAL, so a retried OTLP batch leaves two physical rows for one
/// (project_id, trace_id, span_id) until the next merge. The two-stage query
/// caps its SUBQUERY at `limit` identity pairs, but the outer statement then
/// matches every physical row for those pairs — so without a limit of its own
/// it can hand back more rows than were requested. The single-stage shape
/// cannot drift this way because its LIMIT applies to the rows themselves.
#[tokio::test]
async fn query_spans_never_returns_more_rows_than_the_limit_despite_duplicates() {
    let Some((storage, _probe, _c)) = seeded().await else {
        return;
    };

    // Re-store the three roots verbatim: same identity, so ReplacingMergeTree
    // will eventually collapse them, but not before this read.
    let dupes: Vec<_> = fixture()
        .into_iter()
        .filter(|s| s.parent_span_id.is_none() && s.project_id == PROJECT)
        .collect();
    assert_eq!(dupes.len(), 3, "fixture should have three roots");
    storage
        .store_spans(dupes)
        .await
        .expect("store duplicate spans");

    for limit in [1u64, 2, 3] {
        let spans = storage
            .query_spans(TraceQuery {
                project_id: PROJECT,
                root_only: true,
                limit: Some(limit),
                ..Default::default()
            })
            .await
            .expect("limited query");

        assert!(
            spans.len() as u64 <= limit,
            "asked for {limit} rows, got {} — duplicates leaked past the page size",
            spans.len()
        );
    }
}

// ── query_span_stats ─────────────────────────────────────────────────
//
// The span-stats SQL is assembled the same hand-rolled way `query_spans` is —
// a placeholder string built in parallel with a bind vector — and it renders
// that WHERE clause twice, once for the list query and once for the count.
// A misalignment between the two would not fail to compile and would not
// error: it would return a page whose `total` quietly disagrees with it.
// These tests are here to catch that, plus the ClickHouse-specific hazards
// the TimescaleDB tests cannot exercise (`quantileTDigest` accuracy in the
// tail, and `nullIf` guarding the ratio sorts against division by zero).

fn stats_span(
    project_id: i32,
    service: &str,
    name: &str,
    status: SpanStatusCode,
    minutes_ago: i64,
    duration_ms: f64,
    seq: u64,
) -> SpanRecord {
    span(
        project_id,
        &format!("{seq:032x}"),
        &format!("{seq:016x}"),
        None,
        name,
        service,
        status,
        minutes_ago,
        duration_ms,
        None,
        &[],
    )
}

fn stats_query(project_ids: Vec<i32>, minutes: i64) -> SpanStatsQuery {
    SpanStatsQuery {
        project_ids,
        start_time: Utc::now() - Duration::minutes(minutes),
        end_time: Utc::now() + Duration::minutes(1),
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

/// Fixture shaped like the problem this report exists to solve:
/// `steady` is uniformly ~100ms, `erratic` is bimodal (fast band plus a 2s
/// tail), `cache.get` is trivially fast but very frequent, and OTHER_PROJECT
/// holds a decoy that must never leak in.
async fn seeded_stats() -> Option<Harness> {
    let (storage, probe, container) = setup().await?;

    let mut spans = Vec::new();
    let mut seq = 1_000u64;
    for i in 0..20 {
        seq += 1;
        spans.push(stats_span(
            PROJECT,
            "api",
            "steady",
            SpanStatusCode::Ok,
            10,
            100.0 + i as f64,
            seq,
        ));
    }
    for i in 0..18 {
        seq += 1;
        spans.push(stats_span(
            PROJECT,
            "api",
            "erratic",
            SpanStatusCode::Ok,
            10,
            40.0 + i as f64,
            seq,
        ));
    }
    for _ in 0..2 {
        seq += 1;
        spans.push(stats_span(
            PROJECT,
            "api",
            "erratic",
            SpanStatusCode::Error,
            10,
            2000.0,
            seq,
        ));
    }
    for _ in 0..60 {
        seq += 1;
        spans.push(stats_span(
            PROJECT,
            "worker",
            "cache.get",
            SpanStatusCode::Ok,
            10,
            1.0,
            seq,
        ));
    }
    // Decoy: another project, and one span far outside any window under test.
    for _ in 0..30 {
        seq += 1;
        spans.push(stats_span(
            OTHER_PROJECT,
            "api",
            "leaky",
            SpanStatusCode::Ok,
            10,
            999.0,
            seq,
        ));
    }
    seq += 1;
    spans.push(stats_span(
        PROJECT,
        "api",
        "ancient",
        SpanStatusCode::Ok,
        60 * 24 * 3,
        50.0,
        seq,
    ));

    storage.store_spans(spans).await.expect("store_spans");
    Some((storage, probe, container))
}

#[tokio::test]
async fn span_stats_aggregates_and_scopes_to_the_requested_projects_and_window() {
    let Some((storage, _probe, _c)) = seeded_stats().await else {
        return;
    };

    let rows = storage
        .query_span_stats(stats_query(vec![PROJECT], 60))
        .await
        .expect("query_span_stats");

    let names: Vec<&str> = rows.iter().map(|r| r.span_name.as_str()).collect();
    assert!(names.contains(&"steady"));
    assert!(names.contains(&"erratic"));
    assert!(names.contains(&"cache.get"));
    assert!(
        !names.contains(&"leaky"),
        "another project's spans must never appear: {names:?}"
    );
    assert!(
        !names.contains(&"ancient"),
        "spans outside the window must not be aggregated: {names:?}"
    );

    let erratic = rows.iter().find(|r| r.span_name == "erratic").unwrap();
    assert_eq!(erratic.count, 20);
    assert_eq!(erratic.error_count, 2);
    assert!((erratic.error_rate - 0.1).abs() < 1e-9);
    assert!((erratic.max_duration_ms - 2000.0).abs() < 1.0);
    // quantileTDigest is approximate, but the tail must still land in the
    // slow band — that is the entire signal this report sells.
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

    let steady = rows.iter().find(|r| r.span_name == "steady").unwrap();
    assert!(erratic.tail_ratio > steady.tail_ratio);
    assert!(erratic.coefficient_of_variation > steady.coefficient_of_variation);
}

#[tokio::test]
async fn span_stats_count_agrees_with_the_page_under_every_filter() {
    let Some((storage, _probe, _c)) = seeded_stats().await else {
        return;
    };

    // The list and count queries render the same WHERE clause independently.
    // Exercise every optional filter at once: a bind that slipped out of
    // alignment in one but not the other shows up here as a mismatch.
    let filtered = SpanStatsQuery {
        service_name: Some("api".into()),
        name_pattern: Some("err".into()),
        kind: Some(SpanKind::Server),
        status: Some(SpanStatusCode::Ok),
        min_duration_ms: Some(10.0),
        min_count: 2,
        ..stats_query(vec![PROJECT, OTHER_PROJECT], 60)
    };

    let rows = storage
        .query_span_stats(filtered.clone())
        .await
        .expect("query_span_stats");
    let total = storage
        .count_span_stats(filtered)
        .await
        .expect("count_span_stats");

    assert_eq!(total as usize, rows.len(), "count must match the page");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].span_name, "erratic");
    // status=Ok excluded the two 2s error spans, so only the fast band remains.
    assert_eq!(rows[0].count, 18);
    assert!(rows[0].max_duration_ms < 100.0);
}

#[tokio::test]
async fn span_stats_min_count_and_pagination_are_stable() {
    let Some((storage, _probe, _c)) = seeded_stats().await else {
        return;
    };

    let base = stats_query(vec![PROJECT], 60);

    // A min_count above every group's sample count must return nothing rather
    // than falling back to unfiltered rows.
    let none = storage
        .query_span_stats(SpanStatsQuery {
            min_count: 1_000,
            ..base.clone()
        })
        .await
        .expect("query_span_stats");
    assert!(none.is_empty());

    // Paging must not repeat or drop a row: page 1 + page 2 == the whole list.
    let all = storage
        .query_span_stats(base.clone())
        .await
        .expect("query_span_stats");
    assert!(all.len() >= 3);

    let first = storage
        .query_span_stats(SpanStatsQuery {
            limit: Some(2),
            ..base.clone()
        })
        .await
        .expect("query_span_stats");
    let second = storage
        .query_span_stats(SpanStatsQuery {
            limit: Some(2),
            offset: Some(2),
            ..base
        })
        .await
        .expect("query_span_stats");

    let paged: Vec<&str> = first
        .iter()
        .chain(second.iter())
        .map(|r| r.span_name.as_str())
        .collect();
    let expected: Vec<&str> = all.iter().take(4).map(|r| r.span_name.as_str()).collect();
    assert_eq!(paged, expected);
}

#[tokio::test]
async fn span_stats_sorts_by_every_field_without_dividing_by_zero() {
    let Some((storage, _probe, _c)) = seeded_stats().await else {
        return;
    };

    let base = stats_query(vec![PROJECT], 60);

    // total_time: 60 x 1ms = 60ms vs 20 x ~100ms = 2000ms+.
    let by_total = storage
        .query_span_stats(SpanStatsQuery {
            sort_by: SpanStatsSortField::TotalDurationMs,
            ..base.clone()
        })
        .await
        .expect("query_span_stats");
    assert_ne!(by_total[0].span_name, "cache.get");

    // count: the trivially cheap but frequent call must win.
    let by_count = storage
        .query_span_stats(SpanStatsQuery {
            sort_by: SpanStatsSortField::Count,
            ..base.clone()
        })
        .await
        .expect("query_span_stats");
    assert_eq!(by_count[0].span_name, "cache.get");

    // The variability rankings must surface the bimodal operation, not the
    // uniformly slower one.
    for field in [
        SpanStatsSortField::CoefficientOfVariation,
        SpanStatsSortField::TailRatio,
    ] {
        let rows = storage
            .query_span_stats(SpanStatsQuery {
                sort_by: field,
                ..base.clone()
            })
            .await
            .expect("query_span_stats");
        assert_eq!(rows[0].span_name, "erratic", "sort_by {field:?}");
    }

    // Every remaining field must at least execute: the ratio sorts divide in
    // SQL, and `cache.get` spans are uniform, so an unguarded denominator
    // would either error or sort `inf` to the top.
    for field in [
        SpanStatsSortField::P50DurationMs,
        SpanStatsSortField::P95DurationMs,
        SpanStatsSortField::P99DurationMs,
        SpanStatsSortField::MaxDurationMs,
        SpanStatsSortField::AvgDurationMs,
        SpanStatsSortField::StddevDurationMs,
        SpanStatsSortField::ErrorCount,
        SpanStatsSortField::ErrorRate,
    ] {
        for order in [SortOrder::Asc, SortOrder::Desc] {
            let rows = storage
                .query_span_stats(SpanStatsQuery {
                    sort_by: field,
                    sort_order: order,
                    ..base.clone()
                })
                .await
                .unwrap_or_else(|e| panic!("sort_by {field:?} {order:?} failed: {e}"));
            assert!(
                !rows.is_empty(),
                "sort_by {field:?} {order:?} returned none"
            );
            for row in &rows {
                assert!(
                    row.tail_ratio.is_finite() && row.coefficient_of_variation.is_finite(),
                    "non-finite ratio in {row:?}"
                );
            }
        }
    }
}

/// The two backends implement span-stats in independently-written SQL —
/// PostgreSQL's `percentile_cont`/`STDDEV_SAMP` versus ClickHouse's
/// `quantileTDigest`/`stddevSamp`, with separate filter builders and separate
/// ORDER BY expression tables. Nothing in the type system ties them together,
/// so the only way to know they answer the same question is to ask both.
///
/// Requires Docker for BOTH a ClickHouse container and a TimescaleDB one;
/// skips when either is unavailable.
#[tokio::test]
async fn span_stats_agrees_between_clickhouse_and_timescaledb() {
    let Some((ch_storage, _probe, _c)) = setup().await else {
        return;
    };
    let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Skipping cross-backend span-stats test: no TestDatabase ({e})");
            return;
        }
    };
    let ts_storage = TimescaleDbStorage::new(test_db.db.clone(), None);

    // Deterministic fixture with a wide spread of durations, so the
    // percentile implementations have something to disagree about if they are
    // going to. 200 samples keeps t-digest well inside its accuracy envelope.
    let mut spans = Vec::new();
    let mut seq = 5_000u64;
    for i in 0..200 {
        seq += 1;
        // Fast band for 90% of calls, a decade-slower tail for the rest.
        let duration = if i % 10 == 0 {
            3000.0 + i as f64
        } else {
            50.0 + (i % 7) as f64
        };
        spans.push(stats_span(
            PROJECT,
            "api",
            "mixed",
            if i % 25 == 0 {
                SpanStatusCode::Error
            } else {
                SpanStatusCode::Ok
            },
            10,
            duration,
            seq,
        ));
    }
    for i in 0..80 {
        seq += 1;
        spans.push(stats_span(
            PROJECT,
            "worker",
            "steady",
            SpanStatusCode::Ok,
            10,
            120.0 + i as f64,
            seq,
        ));
    }

    ch_storage
        .store_spans(spans.clone())
        .await
        .expect("clickhouse store_spans");
    ts_storage
        .store_spans(spans)
        .await
        .expect("timescaledb store_spans");

    for sort_by in [
        SpanStatsSortField::TotalDurationMs,
        SpanStatsSortField::P95DurationMs,
        SpanStatsSortField::Count,
        SpanStatsSortField::TailRatio,
        SpanStatsSortField::CoefficientOfVariation,
    ] {
        let query = SpanStatsQuery {
            sort_by,
            ..stats_query(vec![PROJECT], 60)
        };
        let ch = ch_storage
            .query_span_stats(query.clone())
            .await
            .expect("clickhouse query_span_stats");
        let ts = ts_storage
            .query_span_stats(query.clone())
            .await
            .expect("timescaledb query_span_stats");

        // Same ranking, in the same order.
        let ch_order: Vec<&str> = ch.iter().map(|r| r.span_name.as_str()).collect();
        let ts_order: Vec<&str> = ts.iter().map(|r| r.span_name.as_str()).collect();
        assert_eq!(
            ch_order, ts_order,
            "ranking differs for sort_by {sort_by:?}"
        );

        // Same counts, exactly. These are not approximations in either engine.
        for (c, t) in ch.iter().zip(ts.iter()) {
            assert_eq!(c.count, t.count, "count differs for {}", c.span_name);
            assert_eq!(
                c.error_count, t.error_count,
                "error_count differs for {}",
                c.span_name
            );
            let close = |a: f64, b: f64, tolerance: f64, label: &str| {
                let denominator = a.abs().max(b.abs()).max(1.0);
                assert!(
                    (a - b).abs() / denominator <= tolerance,
                    "{label} differs for {}: clickhouse {a} vs timescaledb {b}",
                    c.span_name
                );
            };
            // Exact aggregates must match to floating-point noise.
            close(c.total_duration_ms, t.total_duration_ms, 1e-6, "total");
            close(c.min_duration_ms, t.min_duration_ms, 1e-6, "min");
            close(c.max_duration_ms, t.max_duration_ms, 1e-6, "max");
            close(c.avg_duration_ms, t.avg_duration_ms, 1e-6, "avg");
            close(c.stddev_duration_ms, t.stddev_duration_ms, 1e-6, "stddev");
            // Percentiles are t-digest on one side and exact on the other, so
            // allow a real (but small) divergence — the point is that they
            // describe the same distribution, not that they are bit-identical.
            close(c.p50_duration_ms, t.p50_duration_ms, 0.05, "p50");
            close(c.p95_duration_ms, t.p95_duration_ms, 0.05, "p95");
            close(c.p99_duration_ms, t.p99_duration_ms, 0.05, "p99");
        }

        // And the totals used for pagination must agree.
        let ch_total = ch_storage
            .count_span_stats(query.clone())
            .await
            .expect("clickhouse count_span_stats");
        let ts_total = ts_storage
            .count_span_stats(query)
            .await
            .expect("timescaledb count_span_stats");
        assert_eq!(ch_total, ts_total, "total differs for sort_by {sort_by:?}");
    }
}
