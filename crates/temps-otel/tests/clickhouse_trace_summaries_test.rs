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
