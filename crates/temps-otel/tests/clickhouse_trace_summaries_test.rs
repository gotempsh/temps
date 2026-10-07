// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Real-ClickHouse integration test for `ClickHouseOtelStorage::query_trace_summaries`
//! and `count_traces`.
//!
//! Both were rewritten to drop `FROM spans FINAL` and to resolve the page in
//! two stages, which introduced two failure modes the type system cannot see:
//!
//!   1. **Bind ordering.** The WHERE clause is now rendered TWICE — once for
//!      the outer aggregation and once inside the stage 1 subquery — so its
//!      values must be supplied twice, followed by LIMIT and OFFSET. Getting
//!      this wrong does not fail to compile and usually does not error at
//!      runtime: ClickHouse binds a limit where a project_id was meant and
//!      returns confidently wrong rows. That is why the central case sets
//!      EVERY optional filter at once instead of testing them one at a time.
//!
//!   2. **Dedup without FINAL.** `spans` is a ReplacingMergeTree, so one span
//!      can have several physical rows until the next merge. FINAL used to hide
//!      that. Stage 2 now deduplicates explicitly with
//!      `ORDER BY _version DESC LIMIT 1 BY (trace_id, span_id)` — the same row
//!      FINAL would have kept — and `count_traces` relies on
//!      `uniqExact(trace_id)` being duplicate-proof by construction.
//!      Two tests cover this, and BOTH stop merges first so the duplicates are
//!      guaranteed still present when they assert, rather than passing by
//!      accident: one for identical copies (an OTLP retry) and one for
//!      *divergent* copies (a Postgres backfill), where picking the wrong row
//!      changes the answer rather than just the row count.
//!
//! A third, subtler one: stage 1 selects `trace_id` only, so the ORDER BY must
//! be written as an aggregate expression rather than as a reference to the
//! `max_duration_ms` SELECT alias, which exists only in stage 2. Sorting by
//! duration is therefore covered explicitly — with the alias form ClickHouse
//! rejects the statement outright (UNKNOWN_IDENTIFIER).
//!
//! The `global_page_first` module at the bottom covers the same two concerns for
//! the global traces read (`global_traces::clickhouse`, local source): the
//! page is selected from narrow columns and only then hydrated, so those tests
//! pin dedup, totals, ordering, deep offsets, filters and scopes, plus a
//! memory regression that fails if the read goes back to deduplicating the
//! whole window.
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
    ResourceInfo, SortOrder, SpanKind, SpanRecord, SpanStatusCode, TraceQuery, TraceSortField,
    TraceSummary,
};

const PROJECT: i32 = 42;
/// A second project whose rows must never leak into PROJECT's results.
const OTHER_PROJECT: i32 = 99;
/// Owned exclusively by the duplicate-rows test, so its double insert cannot
/// perturb any other test sharing the container.
const DUP_PROJECT: i32 = 77;
/// Owned exclusively by the divergent-duplicate test.
const DIVERGENT_PROJECT: i32 = 88;

const DB: &str = "trace_summaries_test";

/// The shared server. Holds the container handle and its URL — deliberately NOT
/// a `clickhouse::Client`.
///
/// One container for the whole file, because every test here is project-scoped
/// and container startup dominates the runtime. But each test builds its OWN
/// client from `url`: `#[tokio::test]` gives every test its own runtime, and a
/// hyper connection pool is bound to the runtime that created its connections.
/// A client shared through a `static` hands test B a connection whose reactor
/// died with test A, which surfaces as an intermittent, test-hopping
/// `network error: client error (SendRequest)`. Clients are cheap; containers
/// are not.
struct Server {
    url: String,
    _container: Box<dyn std::any::Any + Send + Sync>,
}

static SERVER: tokio::sync::OnceCell<Option<Arc<Server>>> = tokio::sync::OnceCell::const_new();

/// Per-test handles, built fresh on the calling test's runtime.
struct Harness {
    storage: ClickHouseOtelStorage,
    probe: ::clickhouse::Client,
}

fn client(url: &str) -> ::clickhouse::Client {
    ::clickhouse::Client::default()
        .with_url(url)
        .with_database(DB)
        .with_user("default")
        .with_password("test")
}

fn storage_for(url: &str) -> ClickHouseOtelStorage {
    // Reads ClickHouse only; the inner Postgres store is never hit.
    let inner = Arc::new(TimescaleDbStorage::new(
        Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        None,
    ));
    ClickHouseOtelStorage::new(
        ClickHouseOtelConfig::new(url, DB, "default", "test"),
        inner,
        Arc::new(temps_core::FixedRetentionResolver),
        None,
    )
}

/// Spin up ClickHouse, run the OTel migrations, seed the fixture.
/// `None` when Docker is unreachable so the tests skip instead of failing CI.
async fn setup() -> Option<Arc<Server>> {
    use testcontainers::{
        core::{wait::HttpWaitStrategy, ContainerPort, WaitFor},
        runners::AsyncRunner,
        GenericImage, ImageExt,
    };

    // Pinned to 24.8 for the same reason as clickhouse_query_spans_test.rs:
    // this is the generation where projections on a ReplacingMergeTree require
    // `deduplicate_merge_projection_mode`, which 0007 sets.
    let image = GenericImage::new("clickhouse/clickhouse-server", "24.8")
        .with_exposed_port(ContainerPort::Tcp(8123))
        // The image logs "Ready for connections" only to its in-container log
        // file, so a log wait always times out; poll /ping instead.
        .with_wait_for(WaitFor::http(
            HttpWaitStrategy::new("/ping")
                .with_port(ContainerPort::Tcp(8123))
                .with_expected_status_code(200u16),
        ))
        .with_env_var("CLICKHOUSE_DB", DB)
        .with_env_var("CLICKHOUSE_PASSWORD", "test");

    let container = match image.start().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Skipping trace_summaries test: cannot start container ({e})");
            return None;
        }
    };
    let host_port = match container.get_host_port_ipv4(8123).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Skipping trace_summaries test: cannot get host port ({e})");
            return None;
        }
    };

    let url = format!("http://127.0.0.1:{host_port}");
    let probe = client(&url);

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
        eprintln!("Skipping trace_summaries test: server never became ready ({last_err})");
        return None;
    }

    temps_otel::storage::clickhouse::migrations::apply_migrations(&probe, DB)
        .await
        .expect("apply_migrations failed against testcontainer ClickHouse");

    // No test here depends on parts being merged, and the duplicate-rows test
    // depends on them NOT being merged. Stopping merges makes that test
    // deterministic rather than a race against the background merge scheduler.
    probe
        .query("SYSTEM STOP MERGES spans")
        .execute()
        .await
        .expect("stop merges");

    storage_for(&url)
        .store_spans(fixture())
        .await
        .expect("store fixture spans");

    Some(Arc::new(Server {
        url,
        _container: Box::new(container),
    }))
}

async fn harness() -> Option<Harness> {
    let server = SERVER.get_or_init(setup).await.clone()?;
    Some(Harness {
        storage: storage_for(&server.url),
        probe: client(&server.url),
    })
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
        // Trace A — newest, 2 spans, one of them an ERROR. Matches the
        // "everything" filter below.
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
        // Trace B — middle, 2 spans, entirely clean. The only trace that
        // satisfies status = Ok.
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
        span(
            PROJECT,
            "bbbb2222",
            "b002",
            Some("b001"),
            "SELECT users",
            "auth",
            SpanStatusCode::Ok,
            30,
            40.0,
            Some(8),
            &[],
        ),
        // Trace C — oldest, deliberately outside the 1h window used below.
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
        // Decoy: same shape as trace A, different project. Must never appear.
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

fn trace_ids(summaries: &[TraceSummary]) -> Vec<String> {
    summaries.iter().map(|s| s.trace_id.clone()).collect()
}

fn last_hour(project_id: i32) -> TraceQuery {
    TraceQuery {
        project_id,
        start_time: Some(Utc::now() - Duration::hours(1)),
        limit: Some(100),
        ..Default::default()
    }
}

/// The default Traces-list shape: project + time window, newest first. This is
/// the query that the projection is supposed to serve.
#[tokio::test]
async fn trace_summaries_returns_window_newest_first_with_correct_counts() {
    let Some(h) = harness().await else {
        return;
    };

    let summaries = h
        .storage
        .query_trace_summaries(last_hour(PROJECT))
        .await
        .expect("query trace summaries");

    // Trace C is 4h old — outside the window. The decoy belongs to another project.
    assert_eq!(
        trace_ids(&summaries),
        vec!["aaaa1111", "bbbb2222"],
        "expected only in-window traces for this project, newest first"
    );

    let a = &summaries[0];
    assert_eq!(a.root_span_name, "GET /api/checkout", "root span name");
    assert_eq!(a.service_name, "checkout");
    assert_eq!(a.span_count, 2, "trace A has a root and one child");
    assert_eq!(a.error_count, 1, "only the root of trace A is an ERROR");
    assert_eq!(a.status_code, SpanStatusCode::Error);
    assert_eq!(a.duration_ms, 250.0, "duration is the trace's longest span");

    let b = &summaries[1];
    assert_eq!(b.root_span_name, "POST /api/login");
    assert_eq!(b.span_count, 2);
    assert_eq!(b.error_count, 0);
    assert_eq!(b.status_code, SpanStatusCode::Ok);

    let total = h
        .storage
        .count_traces(last_hour(PROJECT))
        .await
        .expect("count traces");
    assert_eq!(total, 2, "count must agree with the page it paginates");
}

/// Sorting by duration is the case where stage 1 cannot refer to the
/// `max_duration_ms` SELECT alias, because stage 1 selects `trace_id` alone.
/// If the ORDER BY regresses to the alias form this errors, it does not merely
/// return a wrong order.
#[tokio::test]
async fn trace_summaries_sort_by_duration_orders_both_directions() {
    let Some(h) = harness().await else {
        return;
    };

    let desc = h
        .storage
        .query_trace_summaries(TraceQuery {
            sort_by: TraceSortField::Duration,
            sort_order: SortOrder::Desc,
            ..last_hour(PROJECT)
        })
        .await
        .expect("sort by duration desc");
    assert_eq!(
        trace_ids(&desc),
        vec!["aaaa1111", "bbbb2222"],
        "250ms trace before 90ms trace"
    );

    let asc = h
        .storage
        .query_trace_summaries(TraceQuery {
            sort_by: TraceSortField::Duration,
            sort_order: SortOrder::Asc,
            ..last_hour(PROJECT)
        })
        .await
        .expect("sort by duration asc");
    assert_eq!(
        trace_ids(&asc),
        vec!["bbbb2222", "aaaa1111"],
        "ascending must actually reverse, not just re-sort ties"
    );
}

/// THE bind-ordering test.
///
/// Every optional predicate is set at once, so the rendered statement carries
/// the WHERE binds twice — outer aggregation, then stage 1 subquery — followed
/// by LIMIT and OFFSET. If any value lands out of position the filters silently
/// match the wrong rows, so this asserts an exact, uniquely determined answer:
/// only trace A's root span satisfies all of these together.
#[tokio::test]
async fn trace_summaries_binds_stay_aligned_with_every_filter_set() {
    let Some(h) = harness().await else {
        return;
    };

    let everything = || TraceQuery {
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
        sort_by: TraceSortField::StartTime,
        sort_order: SortOrder::Desc,
        limit: Some(10),
        offset: Some(0),
        ..Default::default()
    };

    let summaries = h
        .storage
        .query_trace_summaries(everything())
        .await
        .expect("fully filtered query");

    assert_eq!(
        trace_ids(&summaries),
        vec!["aaaa1111"],
        "trace B is the wrong service/status, C is out of window, the decoy is \
         another project — only A survives all filters at once"
    );
    // The filter applies to spans before grouping, so the counts describe the
    // MATCHING spans only: a002 is 12ms and carries no gen_ai.system attribute.
    // This has always been the behaviour; asserting it keeps the two-stage
    // rewrite from quietly changing it.
    assert_eq!(
        summaries[0].span_count, 1,
        "only the span that matched the filter is counted"
    );
    assert_eq!(summaries[0].error_count, 1);

    let total = h
        .storage
        .count_traces(everything())
        .await
        .expect("fully filtered count");
    assert_eq!(total, 1, "count must apply the same filters as the page");
}

/// The status filter is the one shape that still needs GROUP BY + HAVING in
/// `count_traces`, so both branches of that `if` get exercised here.
#[tokio::test]
async fn trace_summaries_status_filter_partitions_traces() {
    let Some(h) = harness().await else {
        return;
    };

    let errored = h
        .storage
        .query_trace_summaries(TraceQuery {
            status: Some(SpanStatusCode::Error),
            ..last_hour(PROJECT)
        })
        .await
        .expect("status=ERROR page");
    assert_eq!(
        trace_ids(&errored),
        vec!["aaaa1111"],
        "ERROR = the trace has at least one ERROR span"
    );

    let clean = h
        .storage
        .query_trace_summaries(TraceQuery {
            status: Some(SpanStatusCode::Ok),
            ..last_hour(PROJECT)
        })
        .await
        .expect("status=OK page");
    assert_eq!(
        trace_ids(&clean),
        vec!["bbbb2222"],
        "OK = the trace has zero ERROR spans, so trace A must NOT appear"
    );

    for (status, expected) in [(SpanStatusCode::Error, 1), (SpanStatusCode::Ok, 1)] {
        let total = h
            .storage
            .count_traces(TraceQuery {
                status: Some(status),
                ..last_hour(PROJECT)
            })
            .await
            .expect("status-filtered count");
        assert_eq!(total, expected, "HAVING-shaped count for {status:?}");
    }
}

/// LIMIT and OFFSET are the last two binds, after the doubled WHERE binds —
/// the position most likely to be wrong.
#[tokio::test]
async fn trace_summaries_offset_walks_the_page() {
    let Some(h) = harness().await else {
        return;
    };

    let page = |offset: u64| TraceQuery {
        limit: Some(1),
        offset: Some(offset),
        ..last_hour(PROJECT)
    };

    let first = h
        .storage
        .query_trace_summaries(page(0))
        .await
        .expect("page 1");
    assert_eq!(trace_ids(&first), vec!["aaaa1111"]);

    let second = h
        .storage
        .query_trace_summaries(page(1))
        .await
        .expect("page 2");
    assert_eq!(
        trace_ids(&second),
        vec!["bbbb2222"],
        "offset must advance the page, not re-filter it"
    );
}

/// Without FINAL, a retried OTLP batch leaves duplicate physical rows. The
/// aggregates must not double-count them.
#[tokio::test]
async fn trace_summaries_counts_ignore_duplicate_rows_from_retried_batches() {
    let Some(h) = harness().await else {
        return;
    };

    let retried = || {
        vec![
            span(
                DUP_PROJECT,
                "eeee5555",
                "e001",
                None,
                "GET /api/retry",
                "retry",
                SpanStatusCode::Error,
                5,
                150.0,
                Some(9),
                &[],
            ),
            span(
                DUP_PROJECT,
                "eeee5555",
                "e002",
                Some("e001"),
                "SELECT things",
                "retry",
                SpanStatusCode::Ok,
                5,
                20.0,
                Some(9),
                &[],
            ),
        ]
    };

    // Same batch delivered twice, as an exporter retry would.
    h.storage.store_spans(retried()).await.expect("first batch");
    h.storage
        .store_spans(retried())
        .await
        .expect("retried batch");

    // Merges are stopped, so the duplicates are still physically present. If
    // this assertion ever fails, the rest of the test proves nothing and needs
    // revisiting rather than deleting.
    #[derive(::clickhouse::Row, serde::Deserialize)]
    struct Cnt {
        cnt: u64,
    }
    let physical = h
        .probe
        .query("SELECT count() AS cnt FROM spans WHERE project_id = ?")
        .bind(DUP_PROJECT)
        .fetch_one::<Cnt>()
        .await
        .expect("probe physical row count");
    assert_eq!(
        physical.cnt, 4,
        "expected 2 spans x 2 deliveries still unmerged"
    );

    let summaries = h
        .storage
        .query_trace_summaries(last_hour(DUP_PROJECT))
        .await
        .expect("summaries over duplicated rows");

    assert_eq!(trace_ids(&summaries), vec!["eeee5555"]);
    assert_eq!(
        summaries[0].span_count, 2,
        "uniqExact(span_id) must report distinct spans, not physical rows"
    );
    assert_eq!(
        summaries[0].error_count, 1,
        "the single ERROR span must not be counted twice"
    );

    let total = h
        .storage
        .count_traces(last_hour(DUP_PROJECT))
        .await
        .expect("count over duplicated rows");
    assert_eq!(total, 1, "uniqExact(trace_id) counts the trace once");
}

/// The case that makes `FINAL`'s removal non-trivial, and the reason stage 2
/// deduplicates explicitly rather than leaning on "duplicates are identical".
///
/// `temps-cli`'s `ch_backfill_domains` copies Postgres `otel_spans` rows into
/// this table with `_version` derived from the span's own `start_time`, which is
/// LOWER than a live row's ingest-time `_version`. Such a copy can disagree with
/// the live row about `status_code`, `name`, and more. `FINAL` resolved that by
/// keeping the highest `_version`; duplicate-insensitive aggregates would not —
/// they would blend the two, so a superseded ERROR would inflate `error_count`
/// and flip the trace's status, and `argMax` would tie-break arbitrarily on the
/// name.
///
/// Two rows for ONE span id, deliberately divergent, inserted raw so the
/// `_version`s can be pinned. Merges are stopped, so both are still present.
#[tokio::test]
async fn trace_summaries_resolves_divergent_duplicates_by_version() {
    let Some(h) = harness().await else {
        return;
    };

    let insert = |name: &str, status: &str, version: u64| {
        let sql = format!(
            "INSERT INTO spans (project_id, deployment_id, service_name, service_version, \
             deployment_environment, trace_id, span_id, parent_span_id, name, kind, \
             start_time, end_time, duration_ms, status_code, status_message, \
             attributes, events, _version) \
             SELECT {DIVERGENT_PROJECT}, 1, 'svc', '1.0.0', 'production', \
             'ffff8888', 'f001', '', '{name}', 'SERVER', \
             now() - INTERVAL 5 MINUTE, now() - INTERVAL 5 MINUTE + INTERVAL 100 MILLISECOND, \
             100.0, '{status}', '', '{{}}', '[]', {version}"
        );
        h.probe.query(&sql).execute()
    };

    // The superseded copy: lower _version, claims ERROR, different name —
    // exactly the shape a Postgres backfill produces.
    insert("GET /api/superseded", "ERROR", 1_000)
        .await
        .expect("insert superseded copy");
    // The live copy: higher _version, the truth.
    insert("GET /api/live", "OK", 2_000)
        .await
        .expect("insert live copy");

    #[derive(::clickhouse::Row, serde::Deserialize)]
    struct Cnt {
        cnt: u64,
    }
    let physical = h
        .probe
        .query("SELECT count() AS cnt FROM spans WHERE project_id = ?")
        .bind(DIVERGENT_PROJECT)
        .fetch_one::<Cnt>()
        .await
        .expect("probe physical row count");
    assert_eq!(
        physical.cnt, 2,
        "both divergent copies must still be unmerged for this test to mean anything"
    );

    let summaries = h
        .storage
        .query_trace_summaries(last_hour(DIVERGENT_PROJECT))
        .await
        .expect("summaries over divergent duplicates");

    assert_eq!(trace_ids(&summaries), vec!["ffff8888"]);
    let s = &summaries[0];
    assert_eq!(s.span_count, 1, "two physical rows are ONE span, not two");
    assert_eq!(
        s.error_count, 0,
        "the superseded ERROR copy must not count — the winning row says OK"
    );
    assert_eq!(
        s.status_code,
        SpanStatusCode::Ok,
        "trace status follows the winning copy"
    );
    assert_eq!(
        s.root_span_name, "GET /api/live",
        "argMax must not tie-break arbitrarily between divergent copies"
    );
}

#[tokio::test]
async fn has_traces_is_true_for_a_project_with_spans_and_false_otherwise() {
    let Some(h) = harness().await else {
        println!("ClickHouse not available, skipping");
        return;
    };
    h.storage
        .store_spans(fixture())
        .await
        .expect("seed fixture");

    assert!(
        h.storage
            .has_traces(PROJECT)
            .await
            .expect("has_traces(PROJECT)"),
        "PROJECT has spans in the fixture"
    );
    assert!(
        h.storage
            .has_traces(OTHER_PROJECT)
            .await
            .expect("has_traces(OTHER_PROJECT)"),
        "OTHER_PROJECT has the decoy trace in the fixture"
    );
    assert!(
        !h.storage
            .has_traces(-1)
            .await
            .expect("has_traces(unused project)"),
        "a project no span has ever named must report false"
    );
}

#[tokio::test]
async fn global_trace_pages_sort_and_paginate_across_projects_without_fanout() {
    use temps_otel::storage::global_traces::{self, GlobalTraceQuery, TraceReadScope};
    let Some(h) = harness().await else { return };
    let now = Utc::now();
    let mut records = Vec::new();
    for i in 0..45 {
        let project = if i % 2 == 0 { 601 } else { 602 };
        records.push(span(
            project,
            &format!("global-{i:03}"),
            "root",
            None,
            "GET /items",
            "api",
            SpanStatusCode::Ok,
            i + 1,
            (i + 1) as f64,
            None,
            &[],
        ));
    }
    // Same trace ID in another project is a separate summary, and hidden scope
    // rows never enter the total or affect page boundaries.
    records.push(span(
        603,
        "global-000",
        "root",
        None,
        "hidden",
        "api",
        SpanStatusCode::Error,
        0,
        999.0,
        None,
        &[],
    ));
    h.storage.store_spans(records).await.unwrap();
    let q = GlobalTraceQuery {
        filter: TraceQuery {
            start_time: Some(now - Duration::hours(2)),
            end_time: Some(now),
            limit: Some(20),
            offset: Some(20),
            ..Default::default()
        },
        scopes: [601, 602]
            .into_iter()
            .map(|project_id| TraceReadScope {
                project_id,
                from: now - Duration::hours(2),
                to: now,
                cloud: false,
                window_clamped_at: None,
            })
            .collect(),
        summaries: true,
        use_preaggregated_summaries: true,
        lifetime_candidate_total: None,
        source_offset: 0,
    };
    let page = h.storage.global_trace_page(q.clone()).await.unwrap();
    assert_eq!(page.total, 45);
    assert_eq!(page.data.len(), 20);
    assert_eq!(page.data[0].trace_id, "global-020");
    assert_eq!(page.data[19].trace_id, "global-039");
    assert_eq!(page.data[0].project_id, 601);
    let mut sorted = q.clone();
    sorted.filter.sort_by = TraceSortField::Duration;
    let page = h.storage.global_trace_page(sorted).await.unwrap();
    assert_eq!(page.data[0].trace_id, "global-024");
    let mut raw = q.clone();
    raw.summaries = false;
    let page = h.storage.global_trace_page(raw).await.unwrap();
    assert_eq!(page.data[0].span_id, "root");
    assert_eq!(page.total, 45);

    let bulk = (0..5_001)
        .map(|index| {
            span(
                601,
                &format!("global-bulk-{index:04}"),
                "root",
                None,
                "GET /bulk",
                "api",
                SpanStatusCode::Ok,
                index % 60 + 1,
                1.0,
                None,
                &[],
            )
        })
        .collect();
    h.storage.store_spans(bulk).await.unwrap();
    let mut deep = q;
    deep.filter.offset = Some(5_000);
    deep.source_offset = 0;
    let stream = global_traces::clickhouse(&h.probe, &deep, None)
        .await
        .unwrap();
    let page = global_traces::merge(vec![stream], &deep).await.unwrap();
    assert_eq!(page.total, 5_046);
    assert_eq!(page.data.len(), 20);
}

#[tokio::test]
async fn cloud_global_summaries_apply_offset_after_aggregation() {
    use temps_otel::storage::global_traces::{self, GlobalTraceQuery, TraceReadScope};
    let Some(h) = harness().await else { return };
    h.probe.query("CREATE TABLE IF NOT EXISTS telemetry_spans (project_ref String, trace_id String, span_id String, parent_span_id String, name String, service_name String, environment String, span_kind String, status_code String, ts DateTime64(3), duration_ms Float64) ENGINE=Memory").execute().await.unwrap();
    h.probe.query("INSERT INTO telemetry_spans SELECT if(number%2=0,'scope-a','scope-b'), concat('cloud-',toString(number)), 'root', '', 'GET /items', 'api', 'production', 'SERVER', 'OK', fromUnixTimestamp64Milli(1700000000000-number*1000), toFloat64(number+1) FROM numbers(45)").execute().await.unwrap();
    let from = chrono::DateTime::from_timestamp_millis(1699999900000).unwrap();
    let to = chrono::DateTime::from_timestamp_millis(1700000000001).unwrap();
    let q = GlobalTraceQuery {
        filter: TraceQuery {
            limit: Some(20),
            offset: Some(20),
            ..Default::default()
        },
        scopes: [701, 702]
            .into_iter()
            .map(|project_id| TraceReadScope {
                project_id,
                from,
                to,
                cloud: true,
                window_clamped_at: None,
            })
            .collect(),
        summaries: true,
        use_preaggregated_summaries: false,
        lifetime_candidate_total: None,
        source_offset: 20,
    };
    let refs = BTreeMap::from([(701, "scope-a".into()), (702, "scope-b".into())]);
    let stream = global_traces::clickhouse(&h.probe, &q, Some(&refs))
        .await
        .unwrap();
    let page = global_traces::merge(vec![stream], &q).await.unwrap();
    assert_eq!(page.total, 45);
    assert_eq!(page.data.len(), 20);
    assert_eq!(page.data[0].trace_id, "cloud-20");
    assert_eq!(page.data[0].project_id, 701);
    assert_eq!(page.data[19].trace_id, "cloud-39");
    let mut raw = q.clone();
    raw.summaries = false;
    let stream = global_traces::clickhouse(&h.probe, &raw, Some(&refs))
        .await
        .unwrap();
    let page = global_traces::merge(vec![stream], &raw).await.unwrap();
    assert_eq!(page.total, 45);
    assert_eq!(page.data[0].span_id, "root");
    let mut filtered = q.clone();
    filtered.filter.offset = Some(0);
    filtered.source_offset = 0;
    filtered.filter.min_duration_ms = Some(40.0);
    filtered.filter.name_pattern = Some("items".into());
    filtered.filter.service_name = Some("api".into());
    let stream = global_traces::clickhouse(&h.probe, &filtered, Some(&refs))
        .await
        .unwrap();
    let page = global_traces::merge(vec![stream], &filtered).await.unwrap();
    assert_eq!(page.total, 6);
    assert_eq!(page.data[0].trace_id, "cloud-39");

    // Membership remains window-bounded, but a qualifying trace's values use
    // all of its Cloud-held spans so they match local lifetime summaries in a
    // mixed-source merge.
    h.probe.query("INSERT INTO telemetry_spans VALUES ('scope-a', 'cloud-0', 'late-child', 'root', 'child outside window', 'worker', 'production', 'INTERNAL', 'ERROR', fromUnixTimestamp64Milli(1699999800000), 999.0)").execute().await.unwrap();
    let mut lifetime = q;
    lifetime.filter.offset = Some(0);
    lifetime.filter.limit = Some(100);
    lifetime.source_offset = 0;
    lifetime.use_preaggregated_summaries = true;
    let stream = global_traces::clickhouse(&h.probe, &lifetime, Some(&refs))
        .await
        .unwrap();
    let page = global_traces::merge(vec![stream], &lifetime).await.unwrap();
    let trace = page
        .data
        .iter()
        .find(|trace| trace.trace_id == "cloud-0")
        .expect("window member keeps its lifetime Cloud summary");
    assert_eq!(trace.name, "GET /items");
    assert_eq!(trace.start_ms, 1_699_999_800_000);
    assert_eq!(trace.duration, 999.0);
    assert_eq!(trace.span_count, 2);
    assert_eq!(trace.error_count, 1);
    assert_eq!(trace.status, "ERROR");

    let mut past_end = lifetime.clone();
    past_end.filter.offset = Some(100);
    past_end.source_offset = 100;
    let stream = global_traces::clickhouse(&h.probe, &past_end, Some(&refs))
        .await
        .unwrap();
    let page = global_traces::merge(vec![stream], &past_end).await.unwrap();
    assert!(page.data.is_empty());
    assert_eq!(page.total, 45);

    h.probe.query("INSERT INTO telemetry_spans SELECT 'scope-a', concat('over-budget-', toString(number)), 'root', '', 'GET /bulk', 'api', 'production', 'SERVER', 'OK', fromUnixTimestamp64Milli(1700000000000-number), 1.0 FROM numbers(5001)").execute().await.unwrap();
    let stream = global_traces::clickhouse(&h.probe, &lifetime, Some(&refs))
        .await
        .unwrap();
    let page = global_traces::merge(vec![stream], &lifetime).await.unwrap();
    assert_eq!(page.total, 5_046);
    assert_eq!(page.data.len(), 100);
}

// ── Global traces: page-first reads ─────────────────────────────────────────
//
// `global_traces::clickhouse` (local source) selects the page from narrow
// columns first and hydrates only the selected traces, instead of deduplicating
// and aggregating every span in the window. The tests below pin the observable
// contract of that shape — dedup, totals, ordering, deep offsets, filters,
// scopes — and the memory bound that motivated it. Each owns its project IDs so
// the shared container's other fixtures cannot leak in.

mod global_page_first {
    use super::*;
    use temps_otel::storage::global_traces::{GlobalTracePage, GlobalTraceQuery, TraceReadScope};

    const DUPLICATED: i32 = 811;
    const DIVERGENT: i32 = 812;
    const PAGED: i32 = 813;
    const FILTERED: i32 = 814;
    const WIDE_A: i32 = 815;
    const WIDE_B: i32 = 816;
    const RAW_SPANS: i32 = 817;
    const LIFETIME: i32 = 818;
    /// Outside every scope in the scopes test: must never contribute.
    const DECOY: i32 = 819;
    const FACETED: i32 = 821;
    const BULK: i32 = 820;

    fn window(projects: &[i32], hours: i64) -> GlobalTraceQuery {
        let to = Utc::now() + Duration::minutes(1);
        GlobalTraceQuery {
            filter: TraceQuery {
                start_time: Some(to - Duration::hours(hours)),
                end_time: Some(to),
                limit: Some(100),
                ..Default::default()
            },
            scopes: projects
                .iter()
                .map(|&project_id| TraceReadScope {
                    project_id,
                    from: to - Duration::hours(hours),
                    to,
                    cloud: false,
                    window_clamped_at: None,
                })
                .collect(),
            summaries: true,
            use_preaggregated_summaries: true,
            lifetime_candidate_total: None,
            source_offset: 0,
        }
    }

    async fn page(h: &Harness, q: GlobalTraceQuery) -> GlobalTracePage {
        h.storage
            .global_trace_page(q)
            .await
            .expect("global trace page")
    }

    fn ids(page: &GlobalTracePage) -> Vec<String> {
        page.data.iter().map(|r| r.trace_id.clone()).collect()
    }

    async fn insert_raw(
        h: &Harness,
        project: i32,
        (trace, span_id): (&str, &str),
        (name, status): (&str, &str),
        version: u64,
    ) {
        let sql = format!(
            "INSERT INTO spans (project_id, deployment_id, service_name, service_version, \
             deployment_environment, trace_id, span_id, parent_span_id, name, kind, \
             start_time, end_time, duration_ms, status_code, status_message, \
             attributes, events, _version) \
             SELECT {project}, 1, 'svc', '1.0.0', 'production', '{trace}', '{span_id}', '', \
             '{name}', 'SERVER', now() - INTERVAL 5 MINUTE, \
             now() - INTERVAL 5 MINUTE + INTERVAL 100 MILLISECOND, 100.0, '{status}', '', \
             '{{}}', '[]', {version}"
        );
        h.probe
            .query(&sql)
            .execute()
            .await
            .expect("insert raw span");
    }

    async fn physical_rows(h: &Harness, project: i32) -> u64 {
        #[derive(::clickhouse::Row, serde::Deserialize)]
        struct Cnt {
            cnt: u64,
        }
        h.probe
            .query("SELECT count() AS cnt FROM spans WHERE project_id = ?")
            .bind(project)
            .fetch_one::<Cnt>()
            .await
            .expect("physical row count")
            .cnt
    }

    /// A retried batch leaves duplicate physical rows; neither the page nor the
    /// total may see them.
    #[tokio::test]
    async fn retried_batches_do_not_inflate_the_total_or_the_span_counts() {
        let Some(h) = harness().await else { return };
        let batch = || {
            (0..30)
                .flat_map(|t| {
                    let trace = format!("dup-{t:03}");
                    vec![
                        span(
                            DUPLICATED,
                            &trace,
                            "root",
                            None,
                            "GET /dup",
                            "api",
                            SpanStatusCode::Ok,
                            t + 1,
                            50.0,
                            None,
                            &[],
                        ),
                        span(
                            DUPLICATED,
                            &trace,
                            "c1",
                            Some("root"),
                            "db",
                            "api",
                            SpanStatusCode::Error,
                            t + 1,
                            10.0,
                            None,
                            &[],
                        ),
                        span(
                            DUPLICATED,
                            &trace,
                            "c2",
                            Some("root"),
                            "cache",
                            "api",
                            SpanStatusCode::Ok,
                            t + 1,
                            5.0,
                            None,
                            &[],
                        ),
                    ]
                })
                .collect::<Vec<_>>()
        };
        h.storage.store_spans(batch()).await.unwrap();
        h.storage.store_spans(batch()).await.unwrap();
        assert_eq!(
            physical_rows(&h, DUPLICATED).await,
            180,
            "merges are stopped"
        );

        // Window mode (a filter that matches every span) and lifetime mode.
        let mut filtered = window(&[DUPLICATED], 6);
        filtered.filter.service_name = Some("api".into());
        for q in [window(&[DUPLICATED], 6), filtered] {
            let page = page(&h, q).await;
            assert_eq!(page.total, 30, "distinct traces, not physical rows");
            assert_eq!(page.data.len(), 30);
            assert!(page.data.iter().all(|r| r.span_count == 3));
            assert!(page.data.iter().all(|r| r.error_count == 1));
            assert_eq!(page.data[0].trace_id, "dup-000", "newest first");
        }
    }

    /// The winning copy decides every returned value, as `FINAL` would.
    #[tokio::test]
    async fn divergent_copies_resolve_to_the_highest_version() {
        let Some(h) = harness().await else { return };
        insert_raw(
            &h,
            DIVERGENT,
            ("div-0001", "d001"),
            ("GET /superseded", "ERROR"),
            1_000,
        )
        .await;
        insert_raw(
            &h,
            DIVERGENT,
            ("div-0001", "d001"),
            ("GET /live", "OK"),
            2_000,
        )
        .await;
        h.storage
            .store_spans(vec![span(
                DIVERGENT,
                "div-0002",
                "d002",
                None,
                "GET /other",
                "api",
                SpanStatusCode::Ok,
                20,
                10.0,
                None,
                &[],
            )])
            .await
            .unwrap();
        assert_eq!(physical_rows(&h, DIVERGENT).await, 3);

        // Both shapes: lifetime values, and window values behind a filter.
        let mut filtered = window(&[DIVERGENT], 2);
        filtered.filter.service_name = Some("svc".into());
        for q in [window(&[DIVERGENT], 2), filtered] {
            let page = page(&h, q).await;
            let live = page
                .data
                .iter()
                .find(|r| r.trace_id == "div-0001")
                .expect("trace with divergent copies");
            assert_eq!(live.name, "GET /live", "the winning copy names the trace");
            assert_eq!(live.span_count, 1, "two physical rows are one span");
            assert_eq!(live.error_count, 0, "the superseded ERROR must not count");
            assert_eq!(live.status, "OK");
        }
    }

    /// Paging the whole window in every sort order yields each trace exactly
    /// once, in order, with an exact total — including offsets past the first
    /// stage-1 batch.
    #[tokio::test]
    async fn deep_offsets_walk_the_window_in_every_sort_order() {
        let Some(h) = harness().await else { return };
        let traces: Vec<(String, i64, f64)> = (0..250)
            .map(|i| {
                (
                    format!("page-{i:03}"),
                    i as i64 + 1,
                    ((i * 37) % 251 + 1) as f64,
                )
            })
            .collect();
        h.storage
            .store_spans(
                traces
                    .iter()
                    .map(|(id, minutes, duration)| {
                        span(
                            PAGED,
                            id,
                            "root",
                            None,
                            "GET /p",
                            "api",
                            SpanStatusCode::Ok,
                            *minutes,
                            *duration,
                            None,
                            &[],
                        )
                    })
                    .collect(),
            )
            .await
            .unwrap();

        let mut by_start = traces.clone();
        by_start.sort_by_key(|(_, minutes, _)| *minutes);
        let mut by_duration = traces.clone();
        by_duration.sort_by(|a, b| a.2.total_cmp(&b.2));
        let reversed =
            |v: &[(String, i64, f64)]| v.iter().rev().map(|t| t.0.clone()).collect::<Vec<_>>();
        let forward = |v: &[(String, i64, f64)]| v.iter().map(|t| t.0.clone()).collect::<Vec<_>>();
        let cases = [
            (
                TraceSortField::StartTime,
                SortOrder::Desc,
                forward(&by_start),
            ),
            (
                TraceSortField::StartTime,
                SortOrder::Asc,
                reversed(&by_start),
            ),
            (
                TraceSortField::Duration,
                SortOrder::Asc,
                forward(&by_duration),
            ),
            (
                TraceSortField::Duration,
                SortOrder::Desc,
                reversed(&by_duration),
            ),
        ];
        for (sort_by, sort_order, expected) in cases {
            let mut seen = Vec::new();
            for offset in [0, 100, 200, 300] {
                let mut q = window(&[PAGED], 6);
                q.filter.sort_by = sort_by;
                q.filter.sort_order = sort_order;
                q.filter.offset = Some(offset);
                let page = page(&h, q).await;
                assert_eq!(page.total, 250, "{sort_by:?} {sort_order:?} @ {offset}");
                seen.extend(ids(&page));
            }
            assert_eq!(seen, expected, "{sort_by:?} {sort_order:?}");
        }
    }

    /// Every span-level filter, one at a time, on a window whose traces differ
    /// on each dimension. A multi-span trace checks that the counts only cover
    /// the spans that matched.
    #[tokio::test]
    async fn every_filter_selects_the_same_traces_the_single_stage_query_did() {
        let Some(h) = harness().await else { return };
        h.storage
            .store_spans(vec![
                span(
                    FILTERED,
                    "flt-a",
                    "a1",
                    None,
                    "GET /checkout",
                    "web",
                    SpanStatusCode::Ok,
                    5,
                    300.0,
                    Some(1),
                    &[("tier", "paid")],
                ),
                span(
                    FILTERED,
                    "flt-a",
                    "a2",
                    Some("a1"),
                    "SELECT orders",
                    "db",
                    SpanStatusCode::Ok,
                    5,
                    40.0,
                    Some(1),
                    &[],
                ),
                span(
                    FILTERED,
                    "flt-b",
                    "b1",
                    None,
                    "POST /login",
                    "auth",
                    SpanStatusCode::Error,
                    10,
                    80.0,
                    Some(2),
                    &[],
                ),
                span(
                    FILTERED,
                    "flt-c",
                    "c1",
                    None,
                    "GET /health",
                    "web",
                    SpanStatusCode::Ok,
                    20,
                    5.0,
                    Some(1),
                    &[("tier", "free")],
                ),
            ])
            .await
            .unwrap();
        let run = |apply: fn(&mut TraceQuery)| {
            let mut q = window(&[FILTERED], 2);
            apply(&mut q.filter);
            page(&h, q)
        };

        let all = run(|_| {}).await;
        assert_eq!(ids(&all), ["flt-a", "flt-b", "flt-c"]);
        assert_eq!(all.total, 3);
        assert_eq!(all.data[0].span_count, 2, "unfiltered: whole trace");

        let db = run(|f| f.service_name = Some("db".into())).await;
        assert_eq!((ids(&db), db.total), (vec!["flt-a".to_string()], 1));
        assert_eq!(
            db.data[0].span_count, 1,
            "only the matching span is counted"
        );
        assert_eq!(db.data[0].name, "SELECT orders");

        let login = run(|f| f.name_pattern = Some("login".into())).await;
        assert_eq!((ids(&login), login.total), (vec!["flt-b".to_string()], 1));

        let errors = run(|f| f.status = Some(SpanStatusCode::Error)).await;
        assert_eq!((ids(&errors), errors.total), (vec!["flt-b".to_string()], 1));
        let ok = run(|f| f.status = Some(SpanStatusCode::Ok)).await;
        assert_eq!(
            (ids(&ok), ok.total),
            (vec!["flt-a".to_string(), "flt-c".to_string()], 2)
        );
        let unset = run(|f| f.status = Some(SpanStatusCode::Unset)).await;
        assert_eq!((unset.data.len(), unset.total), (0, 0));

        let slow = run(|f| f.min_duration_ms = Some(100.0)).await;
        assert_eq!((ids(&slow), slow.total), (vec!["flt-a".to_string()], 1));
        let slow_db = run(|f| {
            f.min_duration_ms = Some(100.0);
            f.service_name = Some("db".into());
        })
        .await;
        assert_eq!(
            (slow_db.data.len(), slow_db.total),
            (0, 0),
            "the db span is 40ms"
        );

        let deployment = run(|f| f.deployment_id = Some(2)).await;
        assert_eq!(
            (ids(&deployment), deployment.total),
            (vec!["flt-b".to_string()], 1)
        );

        let free = run(|f| {
            f.attributes = Some(BTreeMap::from([("tier".to_string(), "free".to_string())]))
        })
        .await;
        assert_eq!((ids(&free), free.total), (vec!["flt-c".to_string()], 1));

        let one = run(|f| f.trace_id = Some("flt-c".into())).await;
        assert_eq!((ids(&one), one.total), (vec!["flt-c".to_string()], 1));
    }

    /// Each scope keeps its own window; the same trace ID in two projects is two
    /// traces; a project outside the scope never contributes.
    #[tokio::test]
    async fn scopes_apply_their_own_windows_and_stay_separate_per_project() {
        let Some(h) = harness().await else { return };
        h.storage
            .store_spans(vec![
                span(
                    WIDE_A,
                    "shared",
                    "r",
                    None,
                    "GET /a",
                    "api",
                    SpanStatusCode::Ok,
                    30,
                    10.0,
                    None,
                    &[],
                ),
                span(
                    WIDE_B,
                    "shared",
                    "r",
                    None,
                    "GET /b",
                    "api",
                    SpanStatusCode::Ok,
                    40,
                    20.0,
                    None,
                    &[],
                ),
                span(
                    WIDE_A,
                    "old-a",
                    "r",
                    None,
                    "GET /old",
                    "api",
                    SpanStatusCode::Ok,
                    150,
                    10.0,
                    None,
                    &[],
                ),
                span(
                    WIDE_B,
                    "old-b",
                    "r",
                    None,
                    "GET /old",
                    "api",
                    SpanStatusCode::Ok,
                    150,
                    10.0,
                    None,
                    &[],
                ),
                span(
                    DECOY,
                    "shared",
                    "r",
                    None,
                    "GET /hidden",
                    "api",
                    SpanStatusCode::Ok,
                    30,
                    10.0,
                    None,
                    &[],
                ),
            ])
            .await
            .unwrap();

        let mut q = window(&[WIDE_A, WIDE_B], 6);
        // WIDE_A only sees the last hour; WIDE_B sees three.
        q.scopes[0].from = q.scopes[0].to - Duration::hours(1);
        q.scopes[1].from = q.scopes[1].to - Duration::hours(3);
        let page = page(&h, q).await;
        let seen: Vec<(i32, String)> = page
            .data
            .iter()
            .map(|r| (r.project_id, r.trace_id.clone()))
            .collect();
        assert_eq!(
            seen,
            [
                (WIDE_A, "shared".to_string()),
                (WIDE_B, "shared".to_string()),
                (WIDE_B, "old-b".to_string()),
            ]
        );
        assert_eq!(page.total, 3);
        assert_eq!(page.data[0].name, "GET /a");
        assert_eq!(page.data[1].name, "GET /b");
    }

    /// Unfiltered reads under the candidate cap report whole-trace values; the
    /// moment a filter narrows the question, only matching in-window spans count.
    #[tokio::test]
    async fn lifetime_values_cover_the_whole_trace_and_window_values_do_not() {
        let Some(h) = harness().await else { return };
        h.storage
            .store_spans(vec![
                span(
                    LIFETIME,
                    "life-1",
                    "root",
                    None,
                    "GET /life",
                    "api",
                    SpanStatusCode::Ok,
                    5,
                    100.0,
                    None,
                    &[],
                ),
                span(
                    LIFETIME,
                    "life-1",
                    "late",
                    Some("root"),
                    "background",
                    "worker",
                    SpanStatusCode::Error,
                    120,
                    900.0,
                    None,
                    &[],
                ),
            ])
            .await
            .unwrap();
        let q = || {
            let mut q = window(&[LIFETIME], 1);
            q.scopes[0].from = q.scopes[0].to - Duration::hours(1);
            q
        };

        let lifetime = page(&h, q()).await;
        assert_eq!(lifetime.total, 1);
        let t = &lifetime.data[0];
        assert_eq!((t.span_count, t.error_count), (2, 1));
        assert_eq!(t.status, "ERROR");
        assert_eq!(t.duration, 900.0, "longest span of the whole trace");
        assert_eq!(t.name, "GET /life", "the root names the trace");
        assert!(t.start_ms < (Utc::now() - Duration::minutes(100)).timestamp_millis());

        let mut filtered = q();
        filtered.filter.service_name = Some("api".into());
        let window_only = page(&h, filtered).await;
        let t = &window_only.data[0];
        assert_eq!((t.span_count, t.error_count), (1, 0));
        assert_eq!(t.duration, 100.0);

        let mut outside = q();
        outside.filter.service_name = Some("worker".into());
        let none = page(&h, outside).await;
        assert_eq!(
            (none.total, none.data.len()),
            (0, 0),
            "its only worker span is outside the window"
        );
    }

    /// A faceted attribute is filtered on its indexed slot column, written at
    /// ingest by a storage that shares the facet cache; the answer matches the
    /// JSON path, which a storage without the key in its cache still uses.
    #[tokio::test]
    async fn faceted_attribute_filters_use_the_slot_column_and_agree_with_json() {
        let Some(server) = SERVER.get_or_init(setup).await.clone() else {
            return;
        };
        let cache: temps_otel::services::FacetCache = Arc::new(arc_swap::ArcSwap::from_pointee(
            std::collections::HashMap::from([("tier".to_string(), 2u8)]),
        ));
        let inner = Arc::new(TimescaleDbStorage::new(
            Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
            None,
        ));
        let faceted = ClickHouseOtelStorage::new(
            ClickHouseOtelConfig::new(&server.url, DB, "default", "test"),
            inner,
            Arc::new(temps_core::FixedRetentionResolver),
            Some(cache),
        );
        faceted
            .store_spans(vec![
                span(
                    FACETED,
                    "fac-free",
                    "r",
                    None,
                    "GET /f",
                    "api",
                    SpanStatusCode::Ok,
                    5,
                    10.0,
                    None,
                    &[("tier", "free")],
                ),
                span(
                    FACETED,
                    "fac-paid",
                    "r",
                    None,
                    "GET /p",
                    "api",
                    SpanStatusCode::Ok,
                    6,
                    10.0,
                    None,
                    &[("tier", "paid")],
                ),
                span(
                    FACETED,
                    "fac-none",
                    "r",
                    None,
                    "GET /n",
                    "api",
                    SpanStatusCode::Ok,
                    7,
                    10.0,
                    None,
                    &[("region", "eu")],
                ),
            ])
            .await
            .unwrap();

        let mut q = window(&[FACETED], 1);
        q.filter.attributes = Some(BTreeMap::from([("tier".to_string(), "free".to_string())]));
        let via_slot = faceted.global_trace_page(q.clone()).await.unwrap();
        assert_eq!(ids(&via_slot), ["fac-free"]);
        assert_eq!(via_slot.total, 1);

        // The statement really filtered on the slot column.
        let h = harness().await.unwrap();
        let used = logged_like(&h, "facet_attr_2 = 'free'").await;
        assert!(
            used >= 2,
            "the page and the total must both use facet_attr_2"
        );

        // Unfaceted key on the same data: JSON fallback, same machinery.
        let mut region = window(&[FACETED], 1);
        region.filter.attributes = Some(BTreeMap::from([("region".to_string(), "eu".to_string())]));
        let via_json = faceted.global_trace_page(region).await.unwrap();
        assert_eq!(ids(&via_json), ["fac-none"]);

        // A storage that does not know the facet falls back to JSON and agrees,
        // because the attributes blob still holds the value.
        let plain = page(&h, q).await;
        assert_eq!(ids(&plain), ids(&via_slot));
    }

    /// The raw-span listing shares the machinery: dedup, filters, sort, total.
    #[tokio::test]
    async fn raw_span_pages_dedup_filter_and_count_distinct_spans() {
        let Some(h) = harness().await else { return };
        let batch = || {
            (0..12)
                .map(|i| {
                    span(
                        RAW_SPANS,
                        &format!("raw-{i:02}"),
                        &format!("s{i:02}"),
                        None,
                        "GET /raw",
                        "api",
                        if i % 4 == 0 {
                            SpanStatusCode::Error
                        } else {
                            SpanStatusCode::Ok
                        },
                        i + 1,
                        (i + 1) as f64 * 10.0,
                        None,
                        &[("k", "v")],
                    )
                })
                .collect::<Vec<_>>()
        };
        h.storage.store_spans(batch()).await.unwrap();
        h.storage.store_spans(batch()).await.unwrap();
        assert_eq!(physical_rows(&h, RAW_SPANS).await, 24);

        let mut q = window(&[RAW_SPANS], 2);
        q.summaries = false;
        q.filter.limit = Some(5);
        q.filter.offset = Some(5);
        let second = page(&h, q.clone()).await;
        assert_eq!(second.total, 12, "distinct spans, not physical rows");
        let span_ids: Vec<_> = second.data.iter().map(|r| r.span_id.clone()).collect();
        assert_eq!(span_ids, ["s05", "s06", "s07", "s08", "s09"]);
        assert_eq!(
            second.data[0].attributes, r#"{"k":"v"}"#,
            "wide columns are hydrated"
        );

        q.filter.offset = Some(0);
        q.filter.status = Some(SpanStatusCode::Error);
        q.filter.min_duration_ms = Some(50.0);
        let errors = page(&h, q).await;
        assert_eq!(errors.total, 2, "s04 (50ms) and s08 (90ms) are slow errors");
        assert_eq!(
            errors
                .data
                .iter()
                .map(|r| r.span_id.as_str())
                .collect::<Vec<_>>(),
            ["s04", "s08"]
        );
    }

    /// The single-stage query the page-first read replaced: dedup and aggregate
    /// every span in the window, wide columns included, then cut the page.
    /// Kept verbatim (binds inlined) so the next test can show that the data it
    /// seeds is big enough to break that shape under the same memory cap.
    fn legacy_single_stage_page(project: i32, from_ms: i64, to_ms: i64) -> String {
        let pick = |field: &str| {
            format!(
                "argMax(raw.{field}, tuple(raw.parent_span_id = '', raw.duration, raw.span_id))"
            )
        };
        format!(
            "WITH raw AS (SELECT project_id AS project_id, trace_id, span_id, \
             COALESCE(parent_span_id, '') AS parent_span_id, name, service_name, \
             COALESCE(deployment_environment, '') AS environment, kind AS kind, \
             upper(status_code) AS status, toUnixTimestamp64Milli(start_time) AS start_ms, \
             duration_ms AS duration, attributes AS attributes, events AS events, \
             status_message AS status_message FROM spans \
             WHERE ((project_id = {project} AND toUnixTimestamp64Milli(start_time) >= {from_ms} \
             AND toUnixTimestamp64Milli(start_time) <= {to_ms})) \
             ORDER BY _version DESC LIMIT 1 BY project_id, trace_id, span_id), \
             grouped AS (SELECT project_id, trace_id, '' AS span_id, '' AS parent_span_id, \
             {} AS name, {} AS service_name, {} AS environment, {} AS kind, \
             CASE WHEN countIf(raw.status = 'ERROR') > 0 THEN 'ERROR' ELSE {} END AS status, \
             MIN(raw.start_ms) AS start_ms, MAX(raw.duration) AS duration, \
             toInt64(count()) AS span_count, toInt64(countIf(raw.status = 'ERROR')) AS error_count, \
             '{{}}' AS attributes, '[]' AS events, '' AS status_message \
             FROM raw GROUP BY project_id, trace_id) \
             SELECT * FROM grouped ORDER BY start_ms DESC, project_id, trace_id, span_id LIMIT 20",
            pick("name"),
            pick("service_name"),
            pick("environment"),
            pick("kind"),
            pick("status"),
        )
    }

    #[derive(::clickhouse::Row, serde::Deserialize, Debug)]
    struct Logged {
        memory_usage: u64,
        read_rows: u64,
        query_duration_ms: u64,
        projections: Vec<String>,
    }

    /// How many finished statements contain `needle` (client-side binds are
    /// already substituted into the logged text).
    async fn logged_like(h: &Harness, needle: &str) -> u64 {
        #[derive(::clickhouse::Row, serde::Deserialize)]
        struct Cnt {
            cnt: u64,
        }
        h.probe
            .query("SYSTEM FLUSH LOGS")
            .execute()
            .await
            .expect("flush logs");
        h.probe
            .query(
                "SELECT count() AS cnt FROM system.query_log \
                 WHERE type = 'QueryFinish' AND current_database = ? \
                   AND positionCaseInsensitive(query, ?) > 0 \
                   AND query NOT LIKE '%system.query_log%'",
            )
            .bind(DB)
            .bind(needle)
            .fetch_one::<Cnt>()
            .await
            .expect("count query_log")
            .cnt
    }

    /// What ClickHouse recorded for the statements that mention `project`.
    async fn logged(h: &Harness, project: i32) -> Vec<Logged> {
        h.probe
            .query("SYSTEM FLUSH LOGS")
            .execute()
            .await
            .expect("flush logs");
        h.probe
            .query(
                "SELECT memory_usage, read_rows, query_duration_ms, projections \
                 FROM system.query_log \
                 WHERE type = 'QueryFinish' AND current_database = ? \
                   AND positionCaseInsensitive(query, ?) > 0 \
                   AND query NOT LIKE '%system.query_log%' \
                   AND query NOT LIKE 'INSERT%' \
                 ORDER BY event_time_microseconds",
            )
            .bind(DB)
            .bind(format!("project_id = {project} AND"))
            .fetch_all::<Logged>()
            .await
            .expect("read query_log")
    }

    /// Peak memory of a global page must follow the page, not the window.
    ///
    /// 600k wide spans (120k traces) land in one project's current window. The
    /// legacy shape cannot finish inside 128 MiB — the `LIMIT BY` and the
    /// sort hold every deduped span, `attributes` and `events` included — while
    /// the page-first read finishes well inside it, from the projection, reading
    /// roughly one row per span in the window rather than the table.
    #[tokio::test]
    async fn window_size_does_not_drive_query_memory() {
        let Some(h) = harness().await else { return };
        // One trace in four starts inside the queried hour (600k spans); the
        // rest start 70-120 minutes ago. They share every part, as they would
        // in a merged table, so no part or partition can be skipped on time
        // alone — only a read ordered by start_time can avoid the other 1.8M.
        const SPANS: u64 = 600_000;
        const TABLE_SPANS: u64 = 2_400_000;
        h.probe
            .query(&format!(
                "INSERT INTO spans (project_id, deployment_id, service_name, service_version, \
                 deployment_environment, trace_id, span_id, parent_span_id, name, kind, \
                 start_time, end_time, duration_ms, status_code, status_message, \
                 attributes, events, _version) \
                 SELECT {BULK}, toInt32(t % 7), concat('svc-', toString(t % 20)), '1.0.0', 'production', \
                   lower(hex(sipHash128(t))), lower(hex(sipHash64(t, k))), \
                   if(k = 0, '', lower(hex(sipHash64(t, k - 1)))), \
                   concat('GET /route/', toString((t * 7 + k) % 200)), 'SERVER', \
                   now64(3) - toIntervalSecond(if(t % 4 = 0, t % 1800, 4200 + t % 3000)), \
                   now64(3) - toIntervalSecond(if(t % 4 = 0, t % 1800, 4200 + t % 3000)) \
                     + toIntervalMillisecond(10), \
                   toFloat64(sipHash64(t, k) % 5000) / 10 + k, \
                   if(sipHash64(t, k, 1) % 50 = 0, 'ERROR', 'OK'), '', \
                   concat('{{\"http.url\":\"https://example.test/route/', toString(t % 200), '&q=', \
                          lower(hex(sipHash64(t, k, 2))), lower(hex(sipHash64(t, k, 4))), \
                          '\",\"http.user_agent\":\"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36\",\
                          \"db.statement\":\"SELECT * FROM orders WHERE id = ', toString(t), '\"}}'), \
                   concat('[{{\"name\":\"log\",\"attributes\":{{\"message\":\"processed ', \
                          lower(hex(sipHash64(t, k, 3))), lower(hex(sipHash64(t, k, 5))), '\"}}}}]'), \
                   toUInt64(toUnixTimestamp64Milli(now64())) \
                 FROM (SELECT intDiv(number, 5) AS t, number % 5 AS k FROM numbers({TABLE_SPANS}))"
            ))
            .execute()
            .await
            .expect("seed bulk spans");

        let mut q = window(&[BULK], 1);
        q.filter.limit = Some(20);
        let started = std::time::Instant::now();
        let page = page(&h, q.clone()).await;
        let elapsed = started.elapsed();
        assert_eq!(
            page.total,
            SPANS / 5,
            "only traces with a span in the window count"
        );
        assert_eq!(page.data.len(), 20);
        assert!(page.data.iter().all(|r| r.span_count == 5));
        let starts: Vec<_> = page.data.iter().map(|r| r.start_ms).collect();
        assert!(
            starts.windows(2).all(|w| w[0] >= w[1]),
            "newest first: {starts:?}"
        );

        let new_queries = logged(&h, BULK).await;
        println!("page-first: {elapsed:?} {new_queries:#?}");
        assert_eq!(
            new_queries.len(),
            2,
            "one total and one page statement, nothing else"
        );
        for query in &new_queries {
            println!(
                "  {} ms, {} rows, {} MiB peak",
                query.query_duration_ms,
                query.read_rows,
                query.memory_usage >> 20
            );
            assert!(
                query.memory_usage < 96 << 20,
                "peak memory must not follow the window: {query:?}"
            );
            assert!(
                query.read_rows < TABLE_SPANS / 2,
                "reads the window (plus a few granules for the page), not the project: {query:?}"
            );
            assert!(
                query.projections.iter().any(|p| p.ends_with("proj_recent")),
                "narrow reads must be served by proj_recent: {query:?}"
            );
        }

        // Under the same cap the single-stage shape does not finish.
        let from = q.scopes[0].from.timestamp_millis();
        let to = q.scopes[0].to.timestamp_millis();
        let legacy = h
            .probe
            .query(&legacy_single_stage_page(BULK, from, to))
            .with_setting("max_memory_usage", (128u64 << 20).to_string())
            .fetch_all::<temps_otel::storage::global_traces::GlobalTraceRow>()
            .await;
        let error = legacy.expect_err("the legacy shape must exceed 128 MiB on this window");
        assert!(
            error.to_string().contains("MEMORY_LIMIT_EXCEEDED"),
            "unexpected failure: {error}"
        );
    }
}
