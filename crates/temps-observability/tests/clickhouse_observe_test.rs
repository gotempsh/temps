//! Regression test for the Observe page on ClickHouse-enabled servers.
//!
//! When `TEMPS_CLICKHOUSE_*` is configured, proxy logs and OTel spans are
//! WRITTEN to ClickHouse (via `ClickHouseProxyLogStore` / `ClickHouseOtelStorage`).
//! The original `ObservabilityService` read the Postgres tables directly, so
//! the unified Observe feed silently returned zero events on such servers.
//! This test wires the merge service exactly like `configure_routes` does on a
//! ClickHouse-enabled server — reads dispatched through the storage backends —
//! seeds data through the REAL ClickHouse write paths, and asserts the feed
//! returns it.
//!
//! Spins up a `clickhouse/clickhouse-server` testcontainer. If Docker is not
//! reachable the test skips gracefully (per CLAUDE.md: Docker tests must NEVER
//! be `#[ignore]`d — they detect unavailability at runtime and return).
//!
//! The inner Postgres connections are sea-orm `MockDatabase`s: the Request and
//! Span fetchers under test read only ClickHouse. The error and revenue
//! fetchers are always Postgres-backed, so the service's own mock is primed
//! with empty result sets — they resolve to empty streams rather than erroring
//! when a test enables all four kinds.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{Duration, Utc};
use sea_orm::{DatabaseBackend, MockDatabase};

use temps_observability::{EventFilters, EventKind, ObservabilityEvent, ObservabilityService};
use temps_otel::storage::clickhouse::{ClickHouseOtelConfig, ClickHouseOtelStorage};
use temps_otel::storage::timescaledb::TimescaleDbStorage;
use temps_otel::storage::OtelStorage;
use temps_otel::types::{ResourceInfo, SpanKind, SpanRecord, SpanStatusCode};
use temps_proxy::service::proxy_log_service::{CreateProxyLogRequest, ProxyLogService};
use temps_proxy::storage::{ClickHouseProxyLogConfig, ClickHouseProxyLogStore, ProxyLogStorage};

const PROJECT_ID: i32 = 4242;
const CH_DB: &str = "temps_observe_test";

struct ChTestEnv {
    service: ObservabilityService,
    /// Keeps the container alive for the duration of the test.
    _container: Box<dyn std::any::Any + Send>,
}

/// Start ClickHouse, apply BOTH migration sets (proxy logs + otel), and build
/// an `ObservabilityService` whose request/span reads dispatch to ClickHouse —
/// the exact wiring a `TEMPS_CLICKHOUSE_*`-configured server produces.
async fn setup() -> Option<ChTestEnv> {
    use testcontainers::{
        core::{wait::HttpWaitStrategy, ContainerPort, WaitFor},
        runners::AsyncRunner,
        GenericImage, ImageExt,
    };

    let image = GenericImage::new("clickhouse/clickhouse-server", "26.2.5")
        .with_exposed_port(ContainerPort::Tcp(8123))
        // The clickhouse-server image never logs "Ready for connections" to
        // stdout/stderr — wait on HTTP /ping instead (see
        // temps-otel/tests/clickhouse_storage_test.rs).
        .with_wait_for(WaitFor::http(
            HttpWaitStrategy::new("/ping")
                .with_port(ContainerPort::Tcp(8123))
                .with_expected_status_code(200u16),
        ))
        .with_env_var("CLICKHOUSE_DB", CH_DB)
        .with_env_var("CLICKHOUSE_PASSWORD", "test");

    let container = match image.start().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Skipping ClickHouse observe test: cannot start container ({e})");
            return None;
        }
    };
    let host_port = match container.get_host_port_ipv4(8123).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Skipping ClickHouse observe test: cannot get host port ({e})");
            return None;
        }
    };
    let url = format!("http://127.0.0.1:{host_port}");

    let probe = ::clickhouse::Client::default()
        .with_url(&url)
        .with_database(CH_DB)
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
        eprintln!("Skipping ClickHouse observe test: server never became ready ({last_err})");
        return None;
    }

    temps_otel::storage::clickhouse::migrations::apply_migrations(&probe, CH_DB)
        .await
        .expect("otel CH migrations failed");
    temps_proxy::storage::clickhouse_migrations::apply_migrations(&probe, CH_DB)
        .await
        .expect("proxy-log CH migrations failed");

    // Inner Postgres handles are mocks — never queried by the paths under test.
    let mock_pg = || Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

    let ch_proxy_store = Arc::new(ClickHouseProxyLogStore::new(
        ClickHouseProxyLogConfig::new(&url, CH_DB, "default", "test"),
        Arc::new(temps_core::FixedRetentionResolver),
    ));
    // Seed proxy logs through the REAL ClickHouse write path.
    ch_proxy_store
        .write_batch(vec![
            proxy_log_entry("req-ch-1", "/api/orders", 200, None),
            proxy_log_entry("req-ch-2", "/api/orders/17", 500, None),
            proxy_log_entry("req-ch-bot", "/robots.txt", 200, Some("GPTBot")),
        ])
        .await
        .expect("write_batch to ClickHouse");

    let geo = Arc::new(temps_geo::GeoIpService::Mock(
        temps_geo::MockGeoIpService::new(),
    ));
    let ip_service = Arc::new(temps_geo::IpAddressService::new(mock_pg(), geo));
    let proxy_logs = Arc::new(ProxyLogService::with_storage(
        mock_pg(),
        ip_service,
        ch_proxy_store,
    ));

    let inner_tsdb = Arc::new(TimescaleDbStorage::new(mock_pg(), None));
    let ch_otel = Arc::new(ClickHouseOtelStorage::new(
        ClickHouseOtelConfig::new(&url, CH_DB, "default", "test"),
        inner_tsdb,
        Arc::new(temps_core::FixedRetentionResolver),
        None,
    ));
    // Seed one trace (root + child) through the REAL span write path.
    ch_otel
        .store_spans(vec![
            span(
                "trace-obs-000000000000000000000001",
                "root000000000001",
                None,
                "GET /api/orders",
            ),
            span(
                "trace-obs-000000000000000000000001",
                "child00000000001",
                Some("root000000000001"),
                "SELECT orders",
            ),
        ])
        .await
        .expect("store_spans to ClickHouse");

    // The service's own Postgres handle backs the error and revenue fetchers.
    // Those kinds have nothing seeded here, but a bare MockDatabase errors with
    // "`query_results` buffer is empty" the moment they run — which would make
    // any test that enables all four kinds fail on the harness rather than on
    // the behaviour under test. Queue empty result sets so they resolve to
    // empty streams instead.
    let empty_pg_result = || Vec::<BTreeMap<String, sea_orm::Value>>::new();
    let service_db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(std::iter::repeat_with(empty_pg_result).take(8))
            .into_connection(),
    );

    let service = ObservabilityService::new(service_db, proxy_logs, ch_otel);
    Some(ChTestEnv {
        service,
        _container: Box::new(container),
    })
}

fn proxy_log_entry(
    request_id: &str,
    path: &str,
    status_code: i16,
    bot: Option<&str>,
) -> CreateProxyLogRequest {
    CreateProxyLogRequest {
        method: "GET".into(),
        path: path.into(),
        query_string: None,
        host: "observe.test".into(),
        status_code,
        response_time_ms: Some(12),
        request_source: "proxy".into(),
        is_system_request: false,
        routing_status: "routed".into(),
        project_id: Some(PROJECT_ID),
        environment_id: Some(1),
        deployment_id: Some(1),
        session_id: None,
        visitor_id: None,
        visitor_uuid: None,
        session_uuid: None,
        container_id: None,
        upstream_host: None,
        error_message: None,
        client_ip: Some("203.0.113.9".into()),
        user_agent: Some("curl/8".into()),
        referrer: None,
        request_id: request_id.into(),
        ip_geolocation_id: None,
        browser: None,
        browser_version: None,
        operating_system: None,
        device_type: None,
        is_bot: Some(bot.is_some()),
        bot_name: bot.map(Into::into),
        request_size_bytes: Some(128),
        response_size_bytes: Some(1024),
        cache_status: None,
        request_headers: Some(serde_json::json!({"host": "observe.test"})),
        response_headers: None,
        trace_id: None,
        error_group_id: None,
    }
}

fn span(trace_id: &str, span_id: &str, parent: Option<&str>, name: &str) -> SpanRecord {
    let start = Utc::now() - Duration::minutes(1);
    SpanRecord {
        project_id: PROJECT_ID,
        deployment_id: Some(1),
        resource: ResourceInfo {
            service_name: "orders-api".into(),
            service_version: Some("1.0.0".into()),
            deployment_environment: Some("production".into()),
            attributes: BTreeMap::new(),
        },
        trace_id: trace_id.into(),
        span_id: span_id.into(),
        parent_span_id: parent.map(Into::into),
        name: name.into(),
        kind: SpanKind::Server,
        start_time: start,
        end_time: start + Duration::milliseconds(42),
        duration_ms: 42.0,
        status_code: SpanStatusCode::Ok,
        status_message: String::new(),
        attributes: {
            let mut m = BTreeMap::new();
            m.insert("http.method".to_string(), "GET".to_string());
            m
        },
        events: vec![],
    }
}

fn filters(kinds: &[EventKind], search: Option<&str>, hide_bots: Option<bool>) -> EventFilters {
    EventFilters {
        project_id: PROJECT_ID,
        kinds: kinds.iter().copied().collect(),
        from: Some(Utc::now() - Duration::hours(1)),
        to: Some(Utc::now() + Duration::minutes(5)),
        deployment_id: None,
        environment_id: None,
        search: search.map(Into::into),
        limit: 50,
        hide_bots,
    }
}

#[tokio::test]
async fn observe_feed_reads_requests_and_spans_from_clickhouse() {
    let Some(env) = setup().await else { return };

    let events = env
        .service
        .query(filters(&[EventKind::Request, EventKind::Span], None, None))
        .await
        .expect("merged query against ClickHouse");

    // THE regression assertion: with the original Postgres-only fetchers this
    // came back empty on a ClickHouse-enabled server.
    let requests: Vec<_> = events
        .iter()
        .filter(|e| e.kind() == EventKind::Request)
        .collect();
    let spans: Vec<_> = events
        .iter()
        .filter(|e| e.kind() == EventKind::Span)
        .collect();
    assert_eq!(requests.len(), 3, "all seeded proxy logs must surface");
    assert_eq!(spans.len(), 1, "root-only default: one row per trace");

    // Request identity is the backend-agnostic request_id (ClickHouse rows
    // have no serial PK — the old i64 id would have been 0 for every row).
    let ObservabilityEvent::Request(req) = requests[0] else {
        panic!("expected request row");
    };
    assert!(req.id.starts_with("req-ch-"), "id must be the request_id");

    // Span identity is the {trace_id}:{span_id} composite, and only the ROOT
    // span of the trace appears in the default feed.
    let ObservabilityEvent::Span(span_row) = spans[0] else {
        panic!("expected span row");
    };
    assert_eq!(
        span_row.id,
        "trace-obs-000000000000000000000001:root000000000001"
    );
    assert_eq!(span_row.operation, "GET /api/orders");
    assert_eq!(span_row.service, "orders-api");

    // Bot filtering: hide_bots=true must drop the GPTBot row only.
    let hidden = env
        .service
        .query(filters(&[EventKind::Request], None, Some(true)))
        .await
        .expect("hide_bots query");
    assert_eq!(hidden.len(), 2, "bot row must be excluded");

    // A name search widens spans beyond roots so child spans are discoverable.
    let searched = env
        .service
        .query(filters(&[EventKind::Span], Some("SELECT"), None))
        .await
        .expect("span search query");
    assert_eq!(searched.len(), 1);
    let ObservabilityEvent::Span(child) = &searched[0] else {
        panic!("expected span row");
    };
    assert_eq!(child.operation, "SELECT orders");

    // fetch_full resolves the composite span id through the trace lookup.
    let full = env
        .service
        .fetch_full(
            PROJECT_ID,
            EventKind::Span,
            "trace-obs-000000000000000000000001:child00000000001",
            None,
        )
        .await
        .expect("fetch_full span");
    let temps_observability::service::FullEvent::Span(full_span) = full else {
        panic!("expected span full event");
    };
    assert_eq!(full_span.operation, "SELECT orders");

    // fetch_full resolves requests by request_id and enforces tenancy.
    let full_req = env
        .service
        .fetch_full(PROJECT_ID, EventKind::Request, "req-ch-2", None)
        .await
        .expect("fetch_full request");
    let temps_observability::service::FullEvent::Request(fr) = full_req else {
        panic!("expected request full event");
    };
    assert_eq!(fr.id, "req-ch-2");
    assert_eq!(fr.status, 500);

    let wrong_project = env
        .service
        .fetch_full(PROJECT_ID + 1, EventKind::Request, "req-ch-2", None)
        .await;
    assert!(
        wrong_project.is_err(),
        "request_id lookups must not leak across projects"
    );
}

/// The per-kind fetchers run concurrently via `try_join!` rather than being
/// awaited one after another, which changed how a DISABLED kind is represented:
/// it used to contribute nothing to the merge input, and now contributes an
/// empty stream. `merge_desc_by_ts` therefore has to tolerate empty cursors
/// interleaved with populated ones — if it did not, asking for a subset of
/// kinds would drop rows or reorder them wrongly, and the feed would look
/// subtly wrong rather than fail outright.
///
/// Errors also still short-circuit: `try_join!` propagates the first failure,
/// matching the previous `?`-per-fetcher behaviour, so one broken store fails
/// the request instead of silently returning a partial feed.
#[tokio::test]
async fn observe_feed_merges_correctly_when_only_some_kinds_are_enabled() {
    let Some(env) = setup().await else { return };

    // All four kinds: the baseline the subsets are checked against.
    let all = env
        .service
        .query(filters(
            &[
                EventKind::Request,
                EventKind::Error,
                EventKind::Revenue,
                EventKind::Span,
            ],
            None,
            None,
        ))
        .await
        .expect("all-kinds query");

    // Requests only — three empty streams (error, revenue, span) join the merge.
    let requests_only = env
        .service
        .query(filters(&[EventKind::Request], None, None))
        .await
        .expect("requests-only query");
    assert!(
        requests_only.iter().all(|e| e.kind() == EventKind::Request),
        "a disabled kind must not surface"
    );
    assert_eq!(
        requests_only.len(),
        3,
        "empty streams from disabled kinds must not swallow rows"
    );

    // Spans only — same, with the populated stream in a different slot, so a
    // positional bug in the merge input can't pass both cases.
    let spans_only = env
        .service
        .query(filters(&[EventKind::Span], None, None))
        .await
        .expect("spans-only query");
    assert!(spans_only.iter().all(|e| e.kind() == EventKind::Span));
    assert_eq!(spans_only.len(), 1);

    // The subsets must partition the full result, not merely be contained in it.
    assert_eq!(
        requests_only.len() + spans_only.len(),
        all.len(),
        "enabling every kind must yield exactly the union of the subsets \
         (errors and revenue live in Postgres and are unseeded here)"
    );

    // Descending-by-ts ordering has to survive the empty cursors.
    let timestamps: Vec<_> = requests_only.iter().map(|e| e.ts()).collect();
    let mut sorted = timestamps.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(timestamps, sorted, "feed must stay newest-first");
}

/// The feed must be bounded even when the client sends no `from`.
///
/// `from` is an optional query parameter and the request/span stores take the
/// range verbatim, so an omitted date used to mean "scan the whole table". The
/// web UI always sends a range, but the API cannot depend on that. A window
/// wider than the shared cap is refused outright rather than served slowly.
#[tokio::test]
async fn observe_feed_bounds_the_window_and_rejects_an_over_wide_one() {
    let Some(env) = setup().await else { return };

    // No `from` at all: still returns the seeded rows (they are recent), and
    // crucially does not error — the default window is applied silently.
    let mut open = filters(&[EventKind::Request], None, None);
    open.from = None;
    open.to = None;
    let events = env
        .service
        .query(open)
        .await
        .expect("an omitted range must be defaulted, not rejected");
    assert_eq!(events.len(), 3, "recent rows are inside the default window");

    // A range wider than the cap is a client error, not a slow query.
    let mut too_wide = filters(&[EventKind::Request], None, None);
    too_wide.from = Some(Utc::now() - Duration::days(60));
    too_wide.to = None;
    let err = env
        .service
        .query(too_wide)
        .await
        .expect_err("60 days must exceed the cap");

    // Must be the variant the handler maps to 400 — a client mistake surfacing
    // as a 500 would be misleading.
    let temps_observability::ObservabilityError::InvalidTimeWindow(msg) = err else {
        panic!("expected InvalidTimeWindow so the handler returns 400, got {err:?}");
    };
    assert!(msg.contains("maximum"), "{msg}");
    assert!(msg.contains("still"), "must name the workaround: {msg}");

    // `EventFilters::project_id` is a required `i32` — every call into this
    // feed is already scoped to one project — so a 30-day window (exactly the
    // "Last 30 days" preset ObserveFilterBar.tsx ships) must be ALLOWED, not
    // rejected like the unscoped 60-day request above. This is the regression
    // this test guards: the feed's own shipped time-range option must not 400.
    let mut thirty_days = filters(&[EventKind::Request], None, None);
    thirty_days.from = Some(Utc::now() - Duration::days(30));
    thirty_days.to = None;
    env.service
        .query(thirty_days)
        .await
        .expect("a 30-day project-scoped window must not be rejected");
}
