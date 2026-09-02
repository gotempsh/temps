// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-041 Phase B2 invariants.
//!
//! Each test here encodes a property the ADR states as load-bearing, not a
//! coverage exercise. They are grouped in the order the ADR argues them:
//!
//! 1. The **shape-A regression guard** — an instance with no Cloud link ingests
//!    spans exactly as it did before this change. This is the promise that the
//!    default install is untouched, and it is the single most important test in
//!    the change.
//! 2. The **§1 gate** — `write_mode = cloud` is refused at `metered` fidelity,
//!    while unlinked, and with the telemetry switch off, each naming a
//!    *different* fix, because those are three unrelated problems.
//! 3. The **fidelity downgrade block**, which names the write mode as the thing
//!    to change first.
//! 4. The **partition**, proven at the row level: a Cloud-primary project's
//!    ingest performs no local span write, and a `Local` project in the same
//!    batch performs exactly one.
//! 5. The **disconnect**, which flips every Cloud-primary project in one
//!    transaction, closes their ledger intervals, lands un-shippable queued
//!    spans in the local store, and lets local writes resume immediately.
//! 6. The **quota fallback**, which closes the `cloud` interval, surfaces the
//!    reason, leaves the operator's declared intent alone, and reopens the
//!    interval on recovery.
//! 7. The **straddling read**, clamped with `window_clamped_at` and never
//!    merged.
//! 8. The **read decorator**, proving every span reader — not only the two the
//!    query handlers call — is routed.
//! 9. **Facet registration** on a Cloud-primary project answering
//!    `configured: false` with a reason and a setup path, rather than silently
//!    never populating.
//!
//! # Why a local storage spy rather than the crate's `MockOtelStorage`
//!
//! `temps_otel::test_support` is `#[cfg(test)]`, so it does not exist for an
//! integration test — and it should not, since it must never ship in the
//! binary. The spy below is deliberately minimal: it records span writes and
//! answers everything else with an empty result, which is exactly the oracle
//! these tests need ("did a local span row happen, or did it not").
//!
//! # Docker
//!
//! Every test needs Postgres and skips gracefully when no container runtime is
//! available, per CLAUDE.md — never `#[ignore]`.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::{extract::State, routing::post, Json, Router};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, FromQueryResult, Statement, TryGetable,
};
use temps_cloud_client::{
    BackendUrl, CloudFeatureSwitches, CloudLink, DrainObserver, DrainOutcome, SpanOutbox,
};
use temps_entities::cloud_telemetry_fidelity::CloudTelemetryFidelity;
use temps_entities::cloud_telemetry_write_mode::CloudTelemetryWriteMode;
use temps_otel::services::cloud_fidelity::CloudPolicyCache;
use temps_otel::services::cloud_primary_fallback::{
    CloudPrimaryFallback, CloudWriteSuspensionObserver, OutboxSpiller,
};
use temps_otel::services::telemetry_write_mode::{
    CloudLinkSnapshot, TelemetrySpiller, TelemetryWriteModeError, TelemetryWriteModeService,
};
use temps_otel::services::{FacetCache, FacetService, OtelService};
use temps_otel::storage::{
    BaselinePoint, CloudRoutedOtelStorage, CloudSpanSource, DeployEvent, MinuteAggregate,
    OtelStorage, StorageResult, TraceRefProject,
};
use temps_otel::types::{
    GenAiEvent, GenAiSpanDetail, GenAiTraceSummary, HealthSummary, IngestErrorSummary, Insight,
    InsightStatus, LogQuery, LogRecord, MetricBucket, MetricPoint, MetricQuery, ResourceInfo,
    SortOrder, SpanKind, SpanRecord, SpanStats, SpanStatsQuery, SpanStatsSortField, SpanStatusCode,
    StorageQuota, TraceQuery, TraceSummary,
};
use uuid::Uuid;

/// Large enough that the byte cap never binds here — overflow behaviour is
/// Phase B1's load test, not this file's subject.
const OUTBOX_CAP: u64 = 64 * 1024 * 1024;

// ── A local span store that records what was written to it ─────────────────

#[derive(Clone, Default)]
struct LocalSpanStoreSpy {
    spans: Arc<Mutex<Vec<SpanRecord>>>,
    store_spans_calls: Arc<AtomicUsize>,
}

impl LocalSpanStoreSpy {
    fn new() -> Self {
        Self::default()
    }

    fn spans(&self) -> Vec<SpanRecord> {
        self.spans
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn store_spans_calls(&self) -> usize {
        self.store_spans_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl OtelStorage for LocalSpanStoreSpy {
    async fn store_metrics(&self, points: Vec<MetricPoint>) -> StorageResult<u64> {
        Ok(points.len() as u64)
    }

    async fn store_spans(&self, spans: Vec<SpanRecord>) -> StorageResult<u64> {
        self.store_spans_calls.fetch_add(1, Ordering::SeqCst);
        let count = spans.len() as u64;
        self.spans
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend(spans);
        Ok(count)
    }

    async fn store_logs(&self, records: Vec<LogRecord>) -> StorageResult<u64> {
        Ok(records.len() as u64)
    }

    async fn archive_logs(&self, records: Vec<LogRecord>) -> StorageResult<u64> {
        Ok(records.len() as u64)
    }

    async fn record_ingest_error(&self, _: &str, _: &str, _: &str) -> StorageResult<()> {
        Ok(())
    }

    async fn recent_ingest_errors(&self, _: u32) -> StorageResult<Vec<IngestErrorSummary>> {
        Ok(Vec::new())
    }

    async fn query_metrics(&self, _: MetricQuery) -> StorageResult<Vec<MetricBucket>> {
        Ok(Vec::new())
    }

    async fn list_metric_names(&self, _: i32) -> StorageResult<Vec<String>> {
        Ok(Vec::new())
    }

    async fn list_metric_label_keys(
        &self,
        _: i32,
        _: &str,
        _: chrono::DateTime<chrono::Utc>,
        _: chrono::DateTime<chrono::Utc>,
    ) -> StorageResult<Vec<String>> {
        Ok(Vec::new())
    }

    async fn list_metric_label_values(
        &self,
        _: i32,
        _: &str,
        _: &str,
        _: chrono::DateTime<chrono::Utc>,
        _: chrono::DateTime<chrono::Utc>,
    ) -> StorageResult<Vec<String>> {
        Ok(Vec::new())
    }

    async fn query_spans(&self, _: TraceQuery) -> StorageResult<Vec<SpanRecord>> {
        Ok(self.spans())
    }

    async fn query_trace_summaries(&self, _: TraceQuery) -> StorageResult<Vec<TraceSummary>> {
        Ok(Vec::new())
    }

    async fn count_traces(&self, _: TraceQuery) -> StorageResult<u64> {
        Ok(self.spans().len() as u64)
    }

    async fn has_traces(&self, project_id: i32) -> StorageResult<bool> {
        Ok(self.spans().iter().any(|s| s.project_id == project_id))
    }

    async fn get_trace(&self, _: i32, _: &str) -> StorageResult<Vec<SpanRecord>> {
        Ok(Vec::new())
    }

    async fn query_span_stats(&self, _: SpanStatsQuery) -> StorageResult<Vec<SpanStats>> {
        Ok(Vec::new())
    }

    async fn count_span_stats(&self, _: SpanStatsQuery) -> StorageResult<u64> {
        Ok(0)
    }

    async fn query_logs(&self, _: LogQuery) -> StorageResult<Vec<LogRecord>> {
        Ok(Vec::new())
    }

    async fn record_trace_refs(&self, trace_ids: &[String], _: i32) -> StorageResult<u64> {
        Ok(trace_ids.len() as u64)
    }

    async fn get_trace_ref_projects(&self, _: &str) -> StorageResult<Vec<TraceRefProject>> {
        Ok(Vec::new())
    }

    async fn query_genai_trace_summaries(
        &self,
        _: TraceQuery,
    ) -> StorageResult<Vec<GenAiTraceSummary>> {
        Ok(Vec::new())
    }

    async fn get_genai_trace_spans(&self, _: i32, _: &str) -> StorageResult<Vec<GenAiSpanDetail>> {
        Ok(Vec::new())
    }

    async fn count_genai_traces(&self, _: TraceQuery) -> StorageResult<u64> {
        Ok(0)
    }

    async fn get_genai_trace_events(&self, _: i32, _: &str) -> StorageResult<Vec<GenAiEvent>> {
        Ok(Vec::new())
    }

    async fn upsert_insight(&self, _: &Insight) -> StorageResult<i64> {
        Ok(0)
    }

    async fn list_insights(
        &self,
        _: i32,
        _: Option<InsightStatus>,
        _: u64,
        _: u64,
    ) -> StorageResult<Vec<Insight>> {
        Ok(Vec::new())
    }

    async fn resolve_insight(&self, _: i64) -> StorageResult<()> {
        Ok(())
    }

    async fn store_health_summary(&self, _: &HealthSummary) -> StorageResult<()> {
        Ok(())
    }

    async fn get_health_summaries(
        &self,
        _: i32,
        _: Option<i32>,
    ) -> StorageResult<Vec<HealthSummary>> {
        Ok(Vec::new())
    }

    async fn get_storage_quota(&self, project_id: i32) -> StorageResult<StorageQuota> {
        Ok(StorageQuota {
            project_id,
            metrics_bytes: 0,
            traces_bytes: 0,
            logs_bytes: 0,
            total_bytes: 0,
            limit_bytes: u64::MAX,
            usage_pct: 0.0,
        })
    }

    async fn check_quota(&self, _: i32) -> StorageResult<bool> {
        Ok(true)
    }

    async fn get_metric_baseline(
        &self,
        _: i32,
        _: &str,
        _: &str,
        _: Option<&str>,
        _: i32,
    ) -> StorageResult<Vec<BaselinePoint>> {
        Ok(Vec::new())
    }

    async fn get_recent_minute_aggregates(
        &self,
        _: i32,
        _: &str,
        _: &str,
        _: Option<&str>,
        _: i32,
    ) -> StorageResult<Vec<MinuteAggregate>> {
        Ok(Vec::new())
    }

    async fn get_recent_deploys(&self, _: i32, _: i32) -> StorageResult<Vec<DeployEvent>> {
        Ok(Vec::new())
    }

    async fn apply_retention(&self, _: i32) -> StorageResult<u64> {
        Ok(0)
    }

    async fn get_p95_latency(&self, _: i32, _: &str, _: i32) -> StorageResult<f64> {
        Ok(0.0)
    }
}

// ── Cloud stub ─────────────────────────────────────────────────────────────

/// Enrolls the link and accepts telemetry, with a switch to make it refuse.
///
/// These tests are about the *instance*'s state machine, so the Cloud side only
/// has to be credible enough for the link to consider itself usable.
#[derive(Clone, Default)]
struct Stub {
    down: Arc<AtomicBool>,
}

async fn serve_stub(stub: Stub) -> Option<String> {
    let app = Router::new()
        .route(
            "/v1/enroll",
            post(|| async {
                Json(serde_json::json!({
                    "tenant_id": Uuid::new_v4(),
                    "instance_token": "inst_write_mode_test"
                }))
            }),
        )
        .route(
            "/v1/telemetry",
            post(
                |State(stub): State<Stub>,
                 Json(batch): Json<temps_cloud_protocol::TelemetryBatch>| async move {
                    if stub.down.load(Ordering::SeqCst) {
                        return (
                            axum::http::StatusCode::SERVICE_UNAVAILABLE,
                            Json(serde_json::json!({"detail": "stub is down"})),
                        );
                    }
                    let spans = batch.spans.len();
                    (
                        axum::http::StatusCode::OK,
                        Json(serde_json::json!({
                            "submission_id": batch.submission_id,
                            "processed_spans": spans,
                            "stored_spans": spans,
                            "metered_bytes": spans * 200
                        })),
                    )
                },
            ),
        )
        .with_state(stub);

    let listener = match tokio::net::TcpListener::bind::<SocketAddr>(
        "127.0.0.1:0".parse().expect("loopback address must parse"),
    )
    .await
    {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("skipping Cloud-primary write-mode test: sandbox denied TCP bind");
            return None;
        }
        Err(error) => panic!("stub backend must bind: {error}"),
    };
    let address = listener.local_addr().expect("stub has an address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Some(format!("http://{address}"))
}

// ── Harness ────────────────────────────────────────────────────────────────

struct Harness {
    _db: temps_database::test_utils::TestDatabase,
    db: Arc<DatabaseConnection>,
    link: Arc<CloudLink>,
    stub: Stub,
    _state_dir: tempfile::TempDir,
}

impl Harness {
    /// `None` means the environment cannot run the test (no container runtime,
    /// or no TCP bind), and the caller returns instead of failing.
    async fn start() -> Option<Self> {
        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                eprintln!("skipping Cloud-primary write-mode test: no test database ({error})");
                return None;
            }
        };
        let db = test_db.db.clone();

        let stub = Stub::default();
        let backend = serve_stub(stub.clone()).await?;
        let state_dir = tempfile::tempdir().expect("temporary directory");
        let link = Arc::new(CloudLink::load_for_loopback_development(
            state_dir.path().to_path_buf(),
            "write-mode-test",
        ));
        link.configure(
            BackendUrl::loopback_development(&backend).expect("stub backend URL must be accepted"),
        )
        .expect("link must configure");
        link.enroll("write-mode-test-code")
            .await
            .expect("link must enroll");
        link.set_feature_switches(CloudFeatureSwitches {
            telemetry: true,
            ..Default::default()
        })
        .expect("telemetry export must be enabled");

        Some(Self {
            _db: test_db,
            db,
            link,
            stub,
            _state_dir: state_dir,
        })
    }

    /// Insert a project, returning its id.
    ///
    /// Both Cloud telemetry columns are written directly so a test can set up a
    /// state and assert what the *rest* of the system does with it, without
    /// depending on the gate it is sometimes testing.
    async fn project(
        &self,
        slug: &str,
        fidelity: CloudTelemetryFidelity,
        write_mode: CloudTelemetryWriteMode,
    ) -> i32 {
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO projects (name, repo_name, repo_owner, directory, main_branch, \
                 preset, created_at, updated_at, slug, cloud_telemetry_fidelity, \
                 cloud_telemetry_attribute_allowlist, cloud_telemetry_write_mode) \
                 VALUES ($1, 'repo', 'owner', '.', 'main', 'nodejs', now(), now(), $1, $2, \
                 ARRAY['http.route', 'http.method', 'http.status_code']::text[], $3)",
                vec![
                    slug.into(),
                    fidelity.to_string().into(),
                    write_mode.to_string().into(),
                ],
            ))
            .await
            .expect("project must insert");

        self.scalar::<i32>(
            "SELECT id AS v FROM projects WHERE slug = $1",
            vec![slug.into()],
        )
        .await
        .expect("the inserted project must be readable")
    }

    /// Read one value aliased as `v`.
    async fn scalar<T: TryGetable>(&self, sql: &str, values: Vec<sea_orm::Value>) -> Option<T> {
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                values,
            ))
            .await
            .expect("query must run")?;
        Some(row.try_get::<T>("", "v").expect("column `v` must decode"))
    }

    async fn write_mode_of(&self, project_id: i32) -> String {
        self.scalar::<String>(
            "SELECT cloud_telemetry_write_mode AS v FROM projects WHERE id = $1",
            vec![project_id.into()],
        )
        .await
        .expect("project must exist")
    }

    async fn fidelity_of(&self, project_id: i32) -> String {
        self.scalar::<String>(
            "SELECT cloud_telemetry_fidelity AS v FROM projects WHERE id = $1",
            vec![project_id.into()],
        )
        .await
        .expect("project must exist")
    }

    async fn outbox_rows(&self, project_id: i32) -> i64 {
        self.scalar::<i64>(
            "SELECT COUNT(*)::bigint AS v FROM cloud_span_outbox WHERE project_id = $1",
            vec![project_id.into()],
        )
        .await
        .unwrap_or(0)
    }

    /// Rows still waiting to be shipped to Cloud — the number that decides
    /// whether anything can still leave this instance.
    async fn pending_outbox_rows(&self, project_id: i32) -> i64 {
        self.scalar::<i64>(
            "SELECT COUNT(*)::bigint AS v FROM cloud_span_outbox \
             WHERE project_id = $1 AND state = 'pending'",
            vec![project_id.into()],
        )
        .await
        .unwrap_or(0)
    }

    async fn intervals(&self, project_id: i32) -> Vec<Interval> {
        Interval::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT mode, reason, effective_from, effective_to \
             FROM project_telemetry_write_intervals WHERE project_id = $1 \
             ORDER BY effective_from, id",
            vec![project_id.into()],
        ))
        .all(self.db.as_ref())
        .await
        .expect("intervals must be readable")
    }

    fn write_modes(&self) -> Arc<TelemetryWriteModeService> {
        Arc::new(TelemetryWriteModeService::new(self.db.clone()))
    }

    /// The service as the plugin actually wires it: able to drain a project's
    /// durable outbox back into local storage when it stops being Cloud-primary.
    fn write_modes_with_spiller(
        &self,
        outbox: Arc<SpanOutbox>,
        storage: LocalSpanStoreSpy,
    ) -> Arc<TelemetryWriteModeService> {
        let spiller: Arc<dyn TelemetrySpiller> = Arc::new(OutboxSpiller::new(
            outbox,
            Arc::new(storage) as Arc<dyn OtelStorage>,
        ));
        Arc::new(TelemetryWriteModeService::new(self.db.clone()).with_spiller(spiller))
    }

    fn outbox(&self) -> Arc<SpanOutbox> {
        Arc::new(SpanOutbox::new(self.db.clone(), OUTBOX_CAP))
    }

    /// An `OtelService` wired the way the plugin wires it.
    fn service(
        &self,
        storage: LocalSpanStoreSpy,
        outbox: Arc<SpanOutbox>,
        write_modes: Arc<TelemetryWriteModeService>,
        with_link: bool,
    ) -> OtelService {
        let auth = Arc::new(temps_otel::ingest::auth::OtelAuthService::new(
            self.db.clone(),
        ));
        let limiter = Arc::new(temps_otel::ingest::rate_limit::RateLimiter::new(
            100_000,
            Duration::from_secs(60),
        ));
        let service =
            OtelService::new(Arc::new(storage) as Arc<dyn OtelStorage>, auth, limiter, 64)
                .with_cloud_policy_cache(Arc::new(CloudPolicyCache::with_ttl(
                    self.db.clone(),
                    // No caching: these tests change a project's mode and then
                    // immediately ingest, and a stale hit would make a test pass or
                    // fail for a reason unrelated to what it asserts.
                    Duration::ZERO,
                )))
                .with_span_outbox(outbox)
                .with_write_mode_service(write_modes);

        if with_link {
            service.with_cloud_link(self.link.clone())
        } else {
            service
        }
    }
}

#[derive(Debug, FromQueryResult)]
struct Interval {
    mode: String,
    reason: String,
    effective_from: chrono::DateTime<chrono::Utc>,
    effective_to: Option<chrono::DateTime<chrono::Utc>>,
}

fn linked() -> CloudLinkSnapshot {
    CloudLinkSnapshot {
        linked: true,
        telemetry_enabled: true,
        credential_rejected: false,
    }
}

/// A local span, shaped like a real allowlisted HTTP server span.
fn span(project_id: i32, n: usize) -> SpanRecord {
    let start = chrono::Utc::now();
    SpanRecord {
        project_id,
        deployment_id: None,
        resource: ResourceInfo {
            service_name: "orders-api".to_string(),
            deployment_environment: Some("production".to_string()),
            ..Default::default()
        },
        trace_id: format!("{n:032x}"),
        span_id: format!("{n:016x}"),
        parent_span_id: None,
        name: "GET /api/v1/orders/{id}".to_string(),
        kind: SpanKind::Server,
        start_time: start,
        end_time: start + chrono::Duration::milliseconds(12),
        duration_ms: 12.5,
        status_code: SpanStatusCode::Ok,
        status_message: String::new(),
        attributes: [
            ("http.route".to_string(), "/api/v1/orders/{id}".to_string()),
            ("http.method".to_string(), "GET".to_string()),
            ("http.status_code".to_string(), "200".to_string()),
        ]
        .into_iter()
        .collect(),
        events: Vec::new(),
    }
}

// ── 1. Shape A: no Cloud link, no behaviour change ─────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_instance_with_no_cloud_link_ingests_spans_exactly_as_before() {
    // The promise that non-Cloud users are unaffected. Deliberately hostile: the
    // outbox, the write-mode service and the policy cache are all wired, and the
    // project row *says* `cloud`. The absence of a link alone must be enough to
    // take the pre-ADR-041 path.
    let Some(harness) = Harness::start().await else {
        return;
    };
    let project = harness
        .project(
            "shape-a",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Cloud,
        )
        .await;

    let storage = LocalSpanStoreSpy::new();
    let service = harness.service(
        storage.clone(),
        harness.outbox(),
        harness.write_modes(),
        false,
    );

    let spans: Vec<SpanRecord> = (0..25).map(|n| span(project, n)).collect();
    let stored = service
        .ingest_spans(spans)
        .await
        .expect("ingest must succeed without a Cloud link");

    assert_eq!(stored, 25, "every span is stored, as it always was");
    assert_eq!(
        storage.store_spans_calls(),
        1,
        "exactly one local store_spans call — no extra statement on the ingest path"
    );
    assert_eq!(
        storage.spans().len(),
        25,
        "every span must land in local storage"
    );
    assert_eq!(
        harness.outbox_rows(project).await,
        0,
        "an unlinked instance must never enqueue to the Cloud outbox, whatever the \
         project row says"
    );
    assert!(
        harness.intervals(project).await.is_empty(),
        "an unlinked instance must not write ledger rows for a project it never cut over"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_linked_instance_with_telemetry_off_also_takes_the_unchanged_path() {
    // The second half of shape A: linked, but the operator has the telemetry
    // switch off. Nothing may leave, and nothing may be withheld from disk.
    let Some(harness) = Harness::start().await else {
        return;
    };
    harness
        .link
        .set_feature_switches(CloudFeatureSwitches::default())
        .expect("telemetry export must be switchable off");

    let project = harness
        .project(
            "telemetry-off",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Cloud,
        )
        .await;

    let storage = LocalSpanStoreSpy::new();
    let service = harness.service(
        storage.clone(),
        harness.outbox(),
        harness.write_modes(),
        true,
    );

    let stored = service
        .ingest_spans((0..10).map(|n| span(project, n)).collect())
        .await
        .expect("ingest must succeed with telemetry export off");

    assert_eq!(stored, 10);
    assert_eq!(storage.spans().len(), 10);
    assert_eq!(
        harness.outbox_rows(project).await,
        0,
        "the telemetry switch being off must keep every span on this instance"
    );
}

// ── 2. The §1 gate: three refusals, three different fixes ──────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cloud_write_mode_is_refused_at_metered_fidelity() {
    let Some(harness) = Harness::start().await else {
        return;
    };
    let project = harness
        .project(
            "metered-project",
            CloudTelemetryFidelity::Metered,
            CloudTelemetryWriteMode::Local,
        )
        .await;
    let service = harness.write_modes();

    let error = service
        .set_write_mode(project, CloudTelemetryWriteMode::Cloud, linked())
        .await
        .expect_err("a metered project must not become Cloud-primary");

    assert!(
        matches!(
            error,
            TelemetryWriteModeError::FidelityTooLow { project_id, .. } if project_id == project
        ),
        "the refusal must name the fidelity as the problem: {error}"
    );
    let message = error.to_string();
    assert!(
        message.contains("queryable"),
        "the operator must be told which fidelity to raise it to: {message}"
    );
    assert_eq!(
        harness.write_mode_of(project).await,
        "local",
        "a refused change must not have been written"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cloud_write_mode_is_refused_while_the_instance_is_unlinked() {
    let Some(harness) = Harness::start().await else {
        return;
    };
    let project = harness
        .project(
            "unlinked-project",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;
    let service = harness.write_modes();

    let error = service
        .set_write_mode(
            project,
            CloudTelemetryWriteMode::Cloud,
            CloudLinkSnapshot::default(),
        )
        .await
        .expect_err("an unlinked instance has nowhere to put the spans");

    assert!(
        matches!(error, TelemetryWriteModeError::NotLinked { .. }),
        "the refusal must name the missing link, not the fidelity: {error}"
    );
    let message = error.to_string();
    assert!(
        message.contains("/settings/cloud"),
        "the refusal must point at the page that fixes it: {message}"
    );
    assert_eq!(harness.write_mode_of(project).await, "local");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cloud_write_mode_is_refused_while_telemetry_export_is_off() {
    let Some(harness) = Harness::start().await else {
        return;
    };
    let project = harness
        .project(
            "switch-off-project",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;
    let service = harness.write_modes();

    let error = service
        .set_write_mode(
            project,
            CloudTelemetryWriteMode::Cloud,
            CloudLinkSnapshot {
                linked: true,
                telemetry_enabled: false,
                credential_rejected: false,
            },
        )
        .await
        .expect_err("with the switch off no span would ever leave");

    assert!(
        matches!(
            error,
            TelemetryWriteModeError::TelemetryExportDisabled { .. }
        ),
        "the refusal must name the switch: {error}"
    );
    assert_eq!(harness.write_mode_of(project).await, "local");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_gate_refusals_are_distinguishable_from_each_other() {
    // Four unrelated fixes. A single "cannot enable Cloud-primary writes"
    // message would send a self-hosted operator, who has nobody to ask, hunting
    // through all of them.
    let Some(harness) = Harness::start().await else {
        return;
    };
    let service = harness.write_modes();

    let metered = harness
        .project(
            "distinguish-metered",
            CloudTelemetryFidelity::Metered,
            CloudTelemetryWriteMode::Local,
        )
        .await;
    let queryable = harness
        .project(
            "distinguish-queryable",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;

    let fidelity_error = service
        .set_write_mode(metered, CloudTelemetryWriteMode::Cloud, linked())
        .await
        .expect_err("metered is refused")
        .to_string();
    let unlinked_error = service
        .set_write_mode(
            queryable,
            CloudTelemetryWriteMode::Cloud,
            CloudLinkSnapshot::default(),
        )
        .await
        .expect_err("unlinked is refused")
        .to_string();
    let switch_error = service
        .set_write_mode(
            queryable,
            CloudTelemetryWriteMode::Cloud,
            CloudLinkSnapshot {
                linked: true,
                telemetry_enabled: false,
                credential_rejected: false,
            },
        )
        .await
        .expect_err("the switch being off is refused")
        .to_string();
    let credential_error = service
        .set_write_mode(
            queryable,
            CloudTelemetryWriteMode::Cloud,
            CloudLinkSnapshot {
                linked: true,
                telemetry_enabled: true,
                credential_rejected: true,
            },
        )
        .await
        .expect_err("a rejected credential is refused")
        .to_string();

    let all = [
        &fidelity_error,
        &unlinked_error,
        &switch_error,
        &credential_error,
    ];
    for (i, a) in all.iter().enumerate() {
        for b in all.iter().skip(i + 1) {
            assert_ne!(a, b, "two refusals with different fixes read identically");
        }
    }
    assert!(fidelity_error.contains("fidelity"), "{fidelity_error}");
    assert!(unlinked_error.contains("not linked"), "{unlinked_error}");
    assert!(switch_error.contains("switched off"), "{switch_error}");
    assert!(
        credential_error.contains("credential"),
        "{credential_error}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn setting_local_is_always_allowed_whatever_state_cloud_is_in() {
    // An operator must always be able to bring their spans back to storage they
    // control — including on an instance whose link is already gone.
    let Some(harness) = Harness::start().await else {
        return;
    };
    let project = harness
        .project(
            "back-to-local",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Cloud,
        )
        .await;
    let service = harness.write_modes();

    let settings = service
        .set_write_mode(
            project,
            CloudTelemetryWriteMode::Local,
            CloudLinkSnapshot::default(),
        )
        .await
        .expect("returning to local storage must never be refused");

    assert_eq!(settings.write_mode, CloudTelemetryWriteMode::Local);
    assert_eq!(harness.write_mode_of(project).await, "local");
}

// ── 3. Fidelity downgrade is blocked while Cloud-primary ───────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fidelity_downgrade_is_blocked_while_the_project_is_cloud_primary() {
    let Some(harness) = Harness::start().await else {
        return;
    };
    let project = harness
        .project(
            "downgrade-blocked",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Cloud,
        )
        .await;
    let service = harness.write_modes();

    let error = service
        .set_fidelity(project, CloudTelemetryFidelity::Metered, None)
        .await
        .expect_err("dropping to metered would leave the project's traces nowhere");

    assert!(
        matches!(
            error,
            TelemetryWriteModeError::FidelityDowngradeBlockedByWriteMode { .. }
        ),
        "{error}"
    );
    let message = error.to_string();
    assert!(
        message.contains("write mode") && message.contains("`cloud`"),
        "the error must name the write mode as the thing to change first: {message}"
    );
    assert_eq!(
        harness.fidelity_of(project).await,
        "queryable",
        "a blocked downgrade must not have been written"
    );

    // And once the project is back on local storage the same downgrade is fine —
    // the block is about the combination, not about the fidelity.
    service
        .set_write_mode(project, CloudTelemetryWriteMode::Local, linked())
        .await
        .expect("returning to local must succeed");
    service
        .set_fidelity(project, CloudTelemetryFidelity::Metered, None)
        .await
        .expect("the downgrade must be allowed once the project stores spans locally");
    assert_eq!(harness.fidelity_of(project).await, "metered");
}

// ── 3b. A consent withdrawal reaches the durable queue ─────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_two_step_consent_withdrawal_leaves_nothing_real_queued_for_cloud() {
    // The sequence that used to egress real span data after consent had been
    // withdrawn: `write_mode = local` (allowed, and it does not touch the
    // queue), then `fidelity = metered` (allowed, because the write-mode block
    // only fires while the mode is still `cloud`). Both requests succeed, and
    // the outbox worker then ships every already-serialized `queryable` span it
    // was holding — it never re-reads either setting.
    //
    // Each step now has to leave the queue in a state where that cannot happen.
    let Some(harness) = Harness::start().await else {
        return;
    };
    let project = harness
        .project(
            "withdraw-consent",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;

    let storage = LocalSpanStoreSpy::new();
    let outbox = harness.outbox();
    let write_modes = harness.write_modes_with_spiller(outbox.clone(), storage.clone());
    write_modes
        .set_write_mode(project, CloudTelemetryWriteMode::Cloud, linked())
        .await
        .expect("the gate must allow a queryable, linked project");

    let service = harness.service(storage.clone(), outbox.clone(), write_modes.clone(), true);
    service
        .ingest_spans((0..9).map(|n| span(project, n)).collect())
        .await
        .expect("Cloud-primary ingest must succeed");
    assert_eq!(harness.pending_outbox_rows(project).await, 9);
    assert_eq!(
        storage.spans().len(),
        0,
        "a Cloud-primary project writes no local span"
    );

    // Step 1 — back to local storage. The queue has to come with it.
    write_modes
        .set_write_mode(project, CloudTelemetryWriteMode::Local, linked())
        .await
        .expect("returning to local storage must never be refused");
    assert_eq!(
        harness.pending_outbox_rows(project).await,
        0,
        "leaving Cloud-primary must drain what was already captured, not only stop capturing more"
    );
    let spilled = storage.spans();
    assert_eq!(
        spilled.len(),
        9,
        "the queued spans must land in local storage rather than being dropped"
    );
    assert!(
        spilled
            .iter()
            .all(|s| s.project_id == project && !s.trace_id.is_empty()),
        "a spilled span must be the real span, not a placeholder"
    );

    // Step 2 — withdraw the consent tier itself. Now allowed, because there is
    // nothing left in flight that was captured under it.
    write_modes
        .set_fidelity(project, CloudTelemetryFidelity::Metered, None)
        .await
        .expect("the downgrade must be allowed once nothing captured at queryable is queued");
    assert_eq!(harness.fidelity_of(project).await, "metered");

    // And the shipping worker has nothing left it could send.
    assert!(
        outbox
            .claim(500)
            .await
            .expect("the queue must be readable")
            .is_empty(),
        "no span may still be claimable for Cloud after consent was withdrawn"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fidelity_downgrade_is_refused_while_captured_spans_are_still_queued() {
    // The other half: when the spill cannot happen, the downgrade must not
    // silently proceed. Refusing keeps the *higher* consent tier in force until
    // the data captured under it has stopped being in flight, which is a delay
    // rather than a loss — and the message has to name the depth, because
    // "try again later" with no number is a dead end.
    let Some(harness) = Harness::start().await else {
        return;
    };
    let project = harness
        .project(
            "withdraw-blocked",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;

    // Deliberately no spiller — the state an instance is in when local storage
    // refuses the write, or when the spill only got part-way through.
    let write_modes = harness.write_modes();
    write_modes
        .set_write_mode(project, CloudTelemetryWriteMode::Cloud, linked())
        .await
        .expect("the gate must allow this project");

    let storage = LocalSpanStoreSpy::new();
    let outbox = harness.outbox();
    let service = harness.service(storage.clone(), outbox.clone(), write_modes.clone(), true);
    service
        .ingest_spans((0..4).map(|n| span(project, n)).collect())
        .await
        .expect("Cloud-primary ingest must succeed");
    assert_eq!(harness.pending_outbox_rows(project).await, 4);

    write_modes
        .set_write_mode(project, CloudTelemetryWriteMode::Local, linked())
        .await
        .expect("returning to local storage must never be refused, spill or no spill");
    assert_eq!(
        harness.pending_outbox_rows(project).await,
        4,
        "without a spiller the rows stay queued — never dropped"
    );

    let error = write_modes
        .set_fidelity(project, CloudTelemetryFidelity::Metered, None)
        .await
        .expect_err("consent must not be withdrawable while spans captured under it are in flight");

    assert!(
        matches!(
            error,
            TelemetryWriteModeError::FidelityDowngradeBlockedByQueuedSpans {
                project_id,
                queued_spans,
                ..
            } if project_id == project && queued_spans == 4
        ),
        "{error}"
    );
    assert!(error.to_string().contains('4'), "{error}");
    assert_eq!(
        harness.fidelity_of(project).await,
        "queryable",
        "a refused downgrade must not have been written"
    );
}

// ── 4. The partition, proven at the row level ──────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cloud_primary_project_writes_no_local_span_and_a_local_one_writes_exactly_one() {
    let Some(harness) = Harness::start().await else {
        return;
    };
    let cloud_project = harness
        .project(
            "partition-cloud",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Cloud,
        )
        .await;
    let local_project = harness
        .project(
            "partition-local",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;

    let storage = LocalSpanStoreSpy::new();
    let outbox = harness.outbox();
    let service = harness.service(storage.clone(), outbox.clone(), harness.write_modes(), true);

    // One batch, both projects, interleaved — a mixed batch is normal traffic,
    // not a special case.
    let mut spans = Vec::new();
    for n in 0..12 {
        spans.push(span(cloud_project, n));
        spans.push(span(local_project, 1000 + n));
    }
    let accounted = service
        .ingest_spans(spans)
        .await
        .expect("a mixed batch must ingest");

    assert_eq!(accounted, 24, "every span must be accounted for");

    // Row level, both sides.
    let stored = storage.spans();
    assert_eq!(
        stored.len(),
        12,
        "only the Local project's spans may reach the local store"
    );
    assert_eq!(
        stored
            .iter()
            .filter(|s| s.project_id == cloud_project)
            .count(),
        0,
        "the Cloud-primary project performs NO local span write — the property the \
         whole ADR exists to deliver"
    );
    assert_eq!(
        stored
            .iter()
            .filter(|s| s.project_id == local_project)
            .count(),
        12,
        "the Local project's spans must all be on disk"
    );
    assert_eq!(
        storage.store_spans_calls(),
        1,
        "the Local half is written with exactly one call, as before"
    );

    assert_eq!(
        harness.outbox_rows(cloud_project).await,
        12,
        "every Cloud-primary span must be durably queued"
    );
    assert_eq!(
        harness.outbox_rows(local_project).await,
        0,
        "a Local project's spans must never enter the Cloud outbox"
    );
    assert_eq!(
        outbox
            .stats()
            .await
            .expect("stats must be readable")
            .pending_rows,
        12
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cloud_primary_project_at_metered_fidelity_still_stores_locally() {
    // The gate makes this state unreachable through the API and the database
    // `CHECK` refuses it too, but a row edited around both must not silently
    // discard spans: the ingest path's own `CloudTelemetryPolicy::is_cloud_primary`
    // requires `queryable` as well, and that third line of defence is what this
    // asserts.
    let Some(harness) = Harness::start().await else {
        return;
    };
    let project = harness
        .project(
            "hand-edited",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Cloud,
        )
        .await;
    harness
        .db
        .execute(Statement::from_string(
            DatabaseBackend::Postgres,
            "ALTER TABLE projects DROP CONSTRAINT IF EXISTS \
             projects_cloud_primary_requires_queryable"
                .to_string(),
        ))
        .await
        .expect("the constraint must be droppable inside this test's own schema");
    harness
        .db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE projects SET cloud_telemetry_fidelity = 'metered' WHERE id = $1",
            vec![project.into()],
        ))
        .await
        .expect("the hand edit must apply");

    let storage = LocalSpanStoreSpy::new();
    let service = harness.service(
        storage.clone(),
        harness.outbox(),
        harness.write_modes(),
        true,
    );

    service
        .ingest_spans((0..8).map(|n| span(project, n)).collect())
        .await
        .expect("ingest must succeed");

    assert_eq!(
        storage.spans().len(),
        8,
        "an inconsistent row must store MORE, never nothing"
    );
    assert_eq!(
        harness.outbox_rows(project).await,
        0,
        "a metered project's spans must not be queued as if they were readable"
    );
}

// ── 5. Disconnect ──────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnecting_cloud_flips_every_project_closes_the_ledger_and_spills_queued_spans() {
    let Some(harness) = Harness::start().await else {
        return;
    };
    let a = harness
        .project(
            "disconnect-a",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;
    let b = harness
        .project(
            "disconnect-b",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;

    let write_modes = harness.write_modes();
    for project in [a, b] {
        write_modes
            .set_write_mode(project, CloudTelemetryWriteMode::Cloud, linked())
            .await
            .expect("the gate must allow a queryable, linked project");
    }

    let storage = LocalSpanStoreSpy::new();
    let outbox = harness.outbox();
    let service = harness.service(storage.clone(), outbox.clone(), write_modes.clone(), true);

    service
        .ingest_spans(
            (0..6)
                .map(|n| span(a, n))
                .chain((0..4).map(|n| span(b, 100 + n)))
                .collect(),
        )
        .await
        .expect("Cloud-primary ingest must succeed");
    assert_eq!(harness.outbox_rows(a).await, 6);
    assert_eq!(harness.outbox_rows(b).await, 4);
    assert_eq!(
        storage.spans().len(),
        0,
        "nothing was written locally while both projects were Cloud-primary"
    );

    // Cloud refuses, so the final drain ships nothing and every queued span
    // takes the spill path — the branch that decides whether they exist at all.
    harness.stub.down.store(true, Ordering::SeqCst);

    let local_storage: Arc<dyn OtelStorage> = Arc::new(storage.clone());
    harness
        .link
        .set_telemetry_fallback(Arc::new(CloudPrimaryFallback::new(
            write_modes.clone(),
            outbox.clone(),
            local_storage,
            harness.link.clone(),
        )));

    // The real disconnect path: `revoke()` runs the fallback first, then tries
    // to tell Cloud. The stub has no revoke endpoint, and that failing must not
    // change any of the assertions below — which is exactly the ordering the
    // ADR requires.
    let _ = harness.link.revoke().await;

    // (a) Both projects flipped, together.
    assert_eq!(harness.write_mode_of(a).await, "local");
    assert_eq!(harness.write_mode_of(b).await, "local");

    // (b) The ledger closed the `cloud` interval and opened a `local` one that
    //     says *why*, for each project.
    for project in [a, b] {
        let intervals = harness.intervals(project).await;
        assert_eq!(
            intervals.len(),
            2,
            "one cutover and one revert, in order: {intervals:?}"
        );
        assert_eq!(intervals[0].mode, "cloud");
        assert_eq!(intervals[0].reason, "operator");
        assert!(
            intervals[0].effective_to.is_some(),
            "the cloud interval must be closed, not left open"
        );
        assert_eq!(intervals[1].mode, "local");
        assert_eq!(intervals[1].reason, "cloud_disconnected");
        assert!(
            intervals[1].effective_to.is_none(),
            "exactly one interval stays open"
        );
        assert!(
            intervals[1].effective_from >= intervals[0].effective_from,
            "the ledger must read forwards in time"
        );
    }

    // (c) The queued spans that could not ship are in local storage — not
    //     stranded in a queue nobody will drain, and not dropped.
    let spilled = storage.spans();
    assert_eq!(
        spilled.len(),
        10,
        "every queued span must land in the local store; found {}",
        spilled.len()
    );
    assert_eq!(spilled.iter().filter(|s| s.project_id == a).count(), 6);
    assert_eq!(spilled.iter().filter(|s| s.project_id == b).count(), 4);
    assert!(
        spilled
            .iter()
            .all(|s| !s.trace_id.is_empty() && !s.span_id.is_empty()),
        "a spilled span must be a real span, not a placeholder"
    );

    // (d) Local writes resume on the very next ingest.
    harness
        .link
        .disconnect()
        .expect("the local credential must be removable");
    let before = storage.spans().len();
    service
        .ingest_spans((0..3).map(|n| span(a, 900 + n)).collect())
        .await
        .expect("ingest must succeed after a disconnect");
    assert_eq!(
        storage.spans().len(),
        before + 3,
        "span writes must resume on this instance immediately after a disconnect"
    );
    assert_eq!(
        harness.outbox_rows(a).await,
        6,
        "the spilled rows stay settled in the outbox; no new row may be queued \
         after the disconnect"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disconnect_gives_a_local_home_to_every_queued_span_not_only_the_declared_ones() {
    // The spill scope has to come from the queue's contents, not from which
    // projects *declare* `write_mode = cloud`. A project switched back to local
    // while the link was still up correctly leaves its rows queued — the worker
    // would have shipped them — and it is no longer in
    // `cloud_primary_project_ids()`. Scoped by the declared mode, those rows
    // fall outside both the final drain and the spill, and after `disconnect()`
    // nothing ever claims them again: neither delivered to Cloud nor written
    // locally, which is the one outcome this path promises never to produce.
    let Some(harness) = Harness::start().await else {
        return;
    };
    let reverted = harness
        .project(
            "stranded-project",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;
    let still_cloud = harness
        .project(
            "still-cloud-project",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;

    // No spiller on this service, so the revert leaves the rows queued — which
    // is exactly the pre-disconnect state this test is about.
    let write_modes = harness.write_modes();
    for project in [reverted, still_cloud] {
        write_modes
            .set_write_mode(project, CloudTelemetryWriteMode::Cloud, linked())
            .await
            .expect("the gate must allow these projects");
    }

    let storage = LocalSpanStoreSpy::new();
    let outbox = harness.outbox();
    let service = harness.service(storage.clone(), outbox.clone(), write_modes.clone(), true);
    service
        .ingest_spans((0..7).map(|n| span(reverted, n)).collect())
        .await
        .expect("Cloud-primary ingest must succeed");
    assert_eq!(harness.pending_outbox_rows(reverted).await, 7);

    write_modes
        .set_write_mode(reverted, CloudTelemetryWriteMode::Local, linked())
        .await
        .expect("returning to local storage must never be refused");
    assert_eq!(
        harness.pending_outbox_rows(reverted).await,
        7,
        "the rows stay queued while the link is up — the worker would still ship them"
    );
    assert_eq!(
        write_modes
            .cloud_primary_project_ids()
            .await
            .expect("the declared set must be readable"),
        vec![still_cloud],
        "the reverted project is no longer in the declared Cloud-primary set — which is \
         precisely why the spill must not be scoped by it"
    );

    // Now disconnect entirely, with Cloud refusing so nothing can be drained.
    harness.stub.down.store(true, Ordering::SeqCst);
    let local_storage: Arc<dyn OtelStorage> = Arc::new(storage.clone());
    harness
        .link
        .set_telemetry_fallback(Arc::new(CloudPrimaryFallback::new(
            write_modes.clone(),
            outbox.clone(),
            local_storage,
            harness.link.clone(),
        )));
    let _ = harness.link.revoke().await;

    assert_eq!(
        harness.pending_outbox_rows(reverted).await,
        0,
        "a disconnect must leave nothing pending that nothing will ever claim again"
    );
    let spilled = storage.spans();
    assert_eq!(
        spilled.iter().filter(|s| s.project_id == reverted).count(),
        7,
        "every queued span must have a local home after the disconnect, whatever the \
         project's write mode said at the time"
    );
}

// ── 5b. A deleted project stops exporting ──────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deleted_projects_queued_spans_are_neither_exported_nor_left_behind() {
    // `cloud_span_outbox` has no foreign key to `projects` on purpose, so
    // `ON DELETE CASCADE` does not reach it. Without both halves of this — the
    // claim guard and the purge — deleting a project leaves its already
    // serialized spans queued, and the worker keeps shipping them to Cloud
    // afterwards.
    let Some(harness) = Harness::start().await else {
        return;
    };
    let doomed = harness
        .project(
            "doomed-project",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;
    let survivor = harness
        .project(
            "surviving-project",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;
    let write_modes = harness.write_modes();
    for project in [doomed, survivor] {
        write_modes
            .set_write_mode(project, CloudTelemetryWriteMode::Cloud, linked())
            .await
            .expect("the gate must allow these projects");
    }

    let storage = LocalSpanStoreSpy::new();
    let outbox = harness.outbox();
    let service = harness.service(storage.clone(), outbox.clone(), write_modes.clone(), true);
    service
        .ingest_spans(
            (0..6)
                .map(|n| span(doomed, n))
                .chain((0..3).map(|n| span(survivor, 500 + n)))
                .collect(),
        )
        .await
        .expect("Cloud-primary ingest must succeed");
    assert_eq!(harness.pending_outbox_rows(doomed).await, 6);

    // The deletion fence comes first in the real path, and must already be
    // enough to stop another span leaving on the project's behalf.
    harness
        .db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE projects SET is_deleted = true, deleted_at = now() WHERE id = $1",
            vec![doomed.into()],
        ))
        .await
        .expect("the deletion fence must apply");

    let claimed = outbox
        .claim(500)
        .await
        .expect("the queue must be claimable");
    assert!(
        claimed.iter().all(|row| row.project_id != doomed),
        "a project fenced for deletion must not have another span exported on its behalf"
    );
    assert_eq!(
        claimed.len(),
        3,
        "the surviving project's spans must still ship"
    );

    // Then the hard delete, and the purge the `ProjectDeleted` job runs.
    harness
        .db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "DELETE FROM projects WHERE id = $1",
            vec![doomed.into()],
        ))
        .await
        .expect("the project must delete");

    let purged = SpanOutbox::purge_project_rows(harness.db.as_ref(), doomed)
        .await
        .expect("the purge must run");
    assert_eq!(
        purged, 6,
        "every row the deleted project owned must be gone"
    );
    assert_eq!(harness.outbox_rows(doomed).await, 0);
    assert_eq!(
        harness.outbox_rows(survivor).await,
        3,
        "one project's deletion must not touch another's queue"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn orphaned_rows_are_swept_rather_than_consuming_the_byte_cap_forever() {
    // Defense in depth for a deletion path that never emits the job: the claim
    // guard stops an orphaned row shipping, and this stops it sitting in the
    // queue against the operator's byte cap for as long as the instance runs.
    let Some(harness) = Harness::start().await else {
        return;
    };
    let project = harness
        .project(
            "orphan-project",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;
    let write_modes = harness.write_modes();
    write_modes
        .set_write_mode(project, CloudTelemetryWriteMode::Cloud, linked())
        .await
        .expect("the gate must allow this project");

    let outbox = harness.outbox();
    harness
        .service(
            LocalSpanStoreSpy::new(),
            outbox.clone(),
            write_modes.clone(),
            true,
        )
        .ingest_spans((0..5).map(|n| span(project, n)).collect())
        .await
        .expect("Cloud-primary ingest must succeed");

    harness
        .db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "DELETE FROM projects WHERE id = $1",
            vec![project.into()],
        ))
        .await
        .expect("the project must delete");

    assert!(
        outbox
            .claim(500)
            .await
            .expect("the queue must be claimable")
            .is_empty(),
        "a row whose project no longer exists must never be shipped"
    );
    assert_eq!(
        outbox.purge_orphaned().await.expect("the sweep must run"),
        5,
        "and it must not be left occupying the queue either"
    );
    assert_eq!(harness.outbox_rows(project).await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dead_letter_keeps_its_failure_record_after_its_span_content_expires() {
    // A dead letter is the record that this instance accepted telemetry and
    // never delivered it, so the *evidence* survives an operator not looking at
    // it for a while. The span itself is real customer data that nothing will
    // ever ship, and it must not live in the database forever.
    let Some(harness) = Harness::start().await else {
        return;
    };
    let project = harness
        .project(
            "dead-letter-project",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;
    let outbox = harness.outbox();

    harness
        .db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO cloud_span_outbox \
                 (project_id, payload, payload_bytes, enqueued_at, attempts, state, settled_at, \
                  last_error) \
             VALUES ($1, '{\"trace_id\":\"aged\"}', 22, now() - INTERVAL '40 days', 10, \
                     'dead_letter', now() - INTERVAL '30 days', 'upstream refused the batch'), \
                    ($1, '{\"trace_id\":\"fresh\"}', 23, now(), 10, 'dead_letter', now(), \
                     'upstream refused the batch')",
            vec![project.into()],
        ))
        .await
        .expect("dead letters must insert");

    let redacted = outbox
        .redact_expired_dead_letters()
        .await
        .expect("the redaction must run");
    assert_eq!(redacted, 1, "only the aged dead letter may be redacted");

    let with_payload = harness
        .scalar::<i64>(
            "SELECT COUNT(*)::bigint AS v FROM cloud_span_outbox \
             WHERE project_id = $1 AND payload IS NOT NULL",
            vec![project.into()],
        )
        .await
        .unwrap_or(-1);
    assert_eq!(
        with_payload, 1,
        "the recent dead letter keeps its span; the aged one does not"
    );

    // The evidence itself is untouched — this is what an operator reads.
    let summary = outbox
        .dead_letter_summary_for_project(project)
        .await
        .expect("the summary must be readable");
    assert_eq!(
        summary.rows, 2,
        "redacting a payload must not delete the failure record"
    );
    assert_eq!(
        summary.last_error.as_deref(),
        Some("upstream refused the batch"),
        "the operator must still be able to see why delivery failed"
    );
    assert!(summary.last_settled_at.is_some());
    assert_eq!(
        harness
            .scalar::<i32>(
                "SELECT attempts AS v FROM cloud_span_outbox \
                 WHERE project_id = $1 AND payload IS NULL",
                vec![project.into()],
            )
            .await,
        Some(10),
        "the attempt count is part of the evidence and must survive redaction"
    );
}

// ── 6. Quota exhaustion and recovery ───────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quota_exhaustion_closes_the_cloud_interval_and_recovery_reopens_it() {
    let Some(harness) = Harness::start().await else {
        return;
    };
    let project = harness
        .project(
            "quota-project",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;
    let write_modes = harness.write_modes();
    write_modes
        .set_write_mode(project, CloudTelemetryWriteMode::Cloud, linked())
        .await
        .expect("the gate must allow this project");

    let observer = CloudWriteSuspensionObserver::new(write_modes.clone());
    observer
        .on_outcome(&DrainOutcome::NeedsOperator {
            spans: 0,
            batches: 1,
            detail: temps_cloud_protocol::Unavailable::QuotaExhausted {
                used_bytes: 10_000_000,
                limit_bytes: 10_000_000,
                resets_at: chrono::Utc::now() + chrono::Duration::days(3),
            },
        })
        .await;

    // (a) The reason is surfaced, not merely logged.
    assert!(
        write_modes.suspension().is_suspended(),
        "an exhausted quota must suspend Cloud-primary writes"
    );
    let detail = write_modes
        .suspension_detail()
        .expect("the operator must be able to read why");
    assert!(
        detail.contains("QuotaExhausted"),
        "the surfaced reason must name the condition: {detail}"
    );

    // (b) The ledger closed the `cloud` interval and opened a `local` one with
    //     the quota reason.
    let intervals = harness.intervals(project).await;
    assert_eq!(intervals.len(), 2, "{intervals:?}");
    assert_eq!(intervals[0].mode, "cloud");
    assert!(intervals[0].effective_to.is_some());
    assert_eq!(intervals[1].mode, "local");
    assert_eq!(intervals[1].reason, "quota_exhausted");
    assert!(intervals[1].effective_to.is_none());

    // (c) The operator's declared intent is untouched — they did not change
    //     their mind, Cloud stopped accepting.
    assert_eq!(
        harness.write_mode_of(project).await,
        "cloud",
        "a quota fallback must not rewrite the operator's declared intent"
    );

    // (d) Span writes actually resume locally: the ingest path reads the
    //     suspension flag per batch, not once per TTL.
    let storage = LocalSpanStoreSpy::new();
    let service = harness.service(storage.clone(), harness.outbox(), write_modes.clone(), true);
    service
        .ingest_spans((0..5).map(|n| span(project, n)).collect())
        .await
        .expect("ingest must succeed while Cloud writes are suspended");
    assert_eq!(
        storage.spans().len(),
        5,
        "a suspended Cloud must mean spans are stored on this instance, not dropped"
    );
    assert_eq!(
        harness.outbox_rows(project).await,
        0,
        "nothing may be queued into a Cloud that is refusing"
    );

    // (e) Recovery reopens a `cloud` interval without the operator having to
    //     remember anything.
    observer
        .on_outcome(&DrainOutcome::Drained {
            spans: 40,
            batches: 1,
        })
        .await;
    assert!(!write_modes.suspension().is_suspended());
    let intervals = harness.intervals(project).await;
    assert_eq!(intervals.len(), 3, "{intervals:?}");
    assert_eq!(intervals[2].mode, "cloud");
    assert_eq!(intervals[2].reason, "cloud_recovered");
    assert!(intervals[2].effective_to.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transient_degradation_does_not_move_a_project_between_stores() {
    // `Degraded` is explicitly transient and carries a retry hint. Falling back
    // for it would move a project's storage on every backend hiccup — churn in
    // the ledger, not safety.
    let Some(harness) = Harness::start().await else {
        return;
    };
    let project = harness
        .project(
            "degraded-project",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;
    let write_modes = harness.write_modes();
    write_modes
        .set_write_mode(project, CloudTelemetryWriteMode::Cloud, linked())
        .await
        .expect("the gate must allow this project");

    CloudWriteSuspensionObserver::new(write_modes.clone())
        .on_outcome(&DrainOutcome::NeedsOperator {
            spans: 0,
            batches: 1,
            detail: temps_cloud_protocol::Unavailable::Degraded {
                retry_after_secs: 30,
                detail: "backend is degraded".to_string(),
            },
        })
        .await;

    assert!(!write_modes.suspension().is_suspended());
    assert_eq!(
        harness.intervals(project).await.len(),
        1,
        "a transient degradation must not add a ledger interval"
    );
}

// ── 7. A straddling read is clamped, never merged ──────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_query_straddling_the_cutover_is_clamped_and_never_merged() {
    let Some(harness) = Harness::start().await else {
        return;
    };
    let project = harness
        .project(
            "straddle-project",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;
    let write_modes = harness.write_modes();

    // A closed `local` interval, then the cutover to `cloud`. Written directly
    // so the boundary instant is known exactly rather than being "roughly now".
    let cutover = chrono::Utc::now() - chrono::Duration::hours(2);
    harness
        .db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO project_telemetry_write_intervals \
             (project_id, mode, effective_from, effective_to, reason) \
             VALUES ($1, 'local', $2, $3, 'operator'), \
                    ($1, 'cloud', $3, NULL, 'operator')",
            vec![
                project.into(),
                (cutover - chrono::Duration::hours(6)).into(),
                cutover.into(),
            ],
        ))
        .await
        .expect("ledger rows must insert");

    let resolution = write_modes
        .resolve_read_window(
            project,
            cutover - chrono::Duration::hours(4),
            chrono::Utc::now(),
        )
        .await
        .expect("the window must resolve");

    assert_eq!(
        resolution.source,
        CloudTelemetryWriteMode::Cloud,
        "the newest interval the window touches decides the source"
    );
    let clamped_at = resolution
        .window_clamped_at
        .expect("a straddling window must report where it was clamped");
    assert!(
        (clamped_at - cutover).num_seconds().abs() <= 1,
        "the clamp must be at the cutover instant: {clamped_at} vs {cutover}"
    );
    assert!(
        resolution.from >= cutover - chrono::Duration::seconds(1),
        "the served window must start at the cutover, not before it"
    );

    // A window entirely inside one interval is not clamped at all.
    let inside = write_modes
        .resolve_read_window(
            project,
            cutover + chrono::Duration::minutes(5),
            chrono::Utc::now(),
        )
        .await
        .expect("the window must resolve");
    assert_eq!(inside.source, CloudTelemetryWriteMode::Cloud);
    assert!(
        inside.window_clamped_at.is_none(),
        "a window inside one interval must not claim to have been clamped"
    );

    // And history from before the cutover is still local history.
    let before = write_modes
        .resolve_read_window(
            project,
            cutover - chrono::Duration::hours(5),
            cutover - chrono::Duration::hours(1),
        )
        .await
        .expect("the window must resolve");
    assert_eq!(before.source, CloudTelemetryWriteMode::Local);
    assert!(before.window_clamped_at.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_straddle_is_detected_however_many_intervals_the_project_has_accumulated() {
    // The ledger lookup has to be a real range query. Intervals are not only
    // opened by an operator: a quota suspension closes the `cloud` interval and
    // opens a `local` one automatically, and the recovery closes that and opens
    // another, so a project on a flapping allowance accumulates them without
    // anyone touching a setting.
    //
    // Fetched newest-first under a fixed limit, the pre-cutover interval falls
    // out of the set once there are enough of them — and a window that genuinely
    // reaches back before the cutover then looks like it sits inside the newest
    // interval. The read is served from Cloud, *unclamped*, with
    // `window_clamped_at: None`, as if it were a complete answer for a period
    // Cloud never held. That is the implicit cross-boundary answer ADR-040 §3
    // forbids, and it fails silently.
    let Some(harness) = Harness::start().await else {
        return;
    };
    let project = harness
        .project(
            "many-intervals",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;

    let cutover = chrono::Utc::now() - chrono::Duration::days(20);
    harness
        .db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO project_telemetry_write_intervals \
             (project_id, mode, effective_from, effective_to, reason) \
             VALUES ($1, 'local', $2, $3, 'operator')",
            vec![
                project.into(),
                (cutover - chrono::Duration::days(30)).into(),
                cutover.into(),
            ],
        ))
        .await
        .expect("the pre-cutover local interval must insert");

    // 300 short closed `cloud` intervals since the cutover, of the shape a
    // flapping quota produces — comfortably more than any fixed fetch limit.
    let flaps = 300i64;
    harness
        .db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO project_telemetry_write_intervals \
                 (project_id, mode, effective_from, effective_to, reason) \
             SELECT $1, 'cloud', $2 + (n * INTERVAL '1 hour'), \
                    $2 + ((n + 1) * INTERVAL '1 hour'), 'quota_exhausted' \
             FROM generate_series(0, $3::bigint - 1) AS n",
            vec![project.into(), cutover.into(), flaps.into()],
        ))
        .await
        .expect("the flapping intervals must insert");
    // Plus the open one this project is in now.
    harness
        .db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO project_telemetry_write_intervals \
             (project_id, mode, effective_from, effective_to, reason) \
             VALUES ($1, 'cloud', $2, NULL, 'cloud_recovered')",
            vec![
                project.into(),
                (cutover + chrono::Duration::hours(flaps)).into(),
            ],
        ))
        .await
        .expect("the open interval must insert");

    let resolution = harness
        .write_modes()
        .resolve_read_window(
            project,
            cutover - chrono::Duration::days(10),
            chrono::Utc::now(),
        )
        .await
        .expect("the window must resolve");

    assert_eq!(resolution.source, CloudTelemetryWriteMode::Cloud);
    let clamped_at = resolution.window_clamped_at.expect(
        "a window reaching back before the cutover must report where it was cut, however \
         many intervals the project has accumulated since",
    );
    assert!(
        clamped_at >= cutover,
        "the served window must start no earlier than the cutover: {clamped_at} vs {cutover}"
    );
    assert_eq!(resolution.from, clamped_at);
}

// ── 8. Every span reader is routed ─────────────────────────────────────────

/// Counts every call the decorator forwards, per method, so "all four features
/// inherit the routing" is checkable rather than assumed.
#[derive(Default)]
struct CountingCloudSource {
    query_spans: AtomicUsize,
    query_trace_summaries: AtomicUsize,
    count_traces: AtomicUsize,
    has_traces: AtomicUsize,
    get_trace: AtomicUsize,
    query_span_stats: AtomicUsize,
    count_span_stats: AtomicUsize,
}

#[async_trait]
impl CloudSpanSource for CountingCloudSource {
    async fn query_spans(&self, _query: TraceQuery) -> StorageResult<Vec<SpanRecord>> {
        self.query_spans.fetch_add(1, Ordering::SeqCst);
        Ok(vec![span(1, 1)])
    }
    async fn query_trace_summaries(&self, _query: TraceQuery) -> StorageResult<Vec<TraceSummary>> {
        self.query_trace_summaries.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    }
    async fn count_traces(&self, _query: TraceQuery) -> StorageResult<u64> {
        self.count_traces.fetch_add(1, Ordering::SeqCst);
        Ok(7)
    }
    async fn has_traces(&self, _project_id: i32) -> StorageResult<bool> {
        self.has_traces.fetch_add(1, Ordering::SeqCst);
        Ok(true)
    }
    async fn get_trace(&self, _project_id: i32, _trace_id: &str) -> StorageResult<Vec<SpanRecord>> {
        self.get_trace.fetch_add(1, Ordering::SeqCst);
        Ok(vec![span(1, 2)])
    }
    async fn query_span_stats(&self, _query: SpanStatsQuery) -> StorageResult<Vec<SpanStats>> {
        self.query_span_stats.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    }
    async fn count_span_stats(&self, _query: SpanStatsQuery) -> StorageResult<u64> {
        self.count_span_stats.fetch_add(1, Ordering::SeqCst);
        Ok(0)
    }
}

fn trace_query(project_id: i32) -> TraceQuery {
    TraceQuery {
        project_id,
        ..Default::default()
    }
}

fn span_stats_query(project_ids: Vec<i32>) -> SpanStatsQuery {
    SpanStatsQuery {
        project_ids,
        start_time: chrono::Utc::now() - chrono::Duration::hours(1),
        end_time: chrono::Utc::now(),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_span_reader_is_served_from_cloud_for_a_cloud_primary_project() {
    // The requirement whose omission silently empties four features. The
    // decorator is installed at the plugin's `register_service` call site, so
    // HealthComputeService, CrossProjectTraceService, the `TraceReader` impl and
    // temps-observability all inherit it — which is only worth anything if the
    // decorator itself routes *every* span-reading method.
    let Some(harness) = Harness::start().await else {
        return;
    };
    let project = harness
        .project(
            "routed-project",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;
    let write_modes = harness.write_modes();
    write_modes
        .set_write_mode(project, CloudTelemetryWriteMode::Cloud, linked())
        .await
        .expect("the gate must allow this project");

    let local = LocalSpanStoreSpy::new();
    let cloud = Arc::new(CountingCloudSource::default());
    let routed = CloudRoutedOtelStorage::new(
        Arc::new(local.clone()) as Arc<dyn OtelStorage>,
        cloud.clone(),
        write_modes.clone(),
    );

    let spans = routed
        .query_spans(trace_query(project))
        .await
        .expect("query_spans must succeed");
    assert_eq!(spans.len(), 1, "the Cloud rows must be returned as-is");
    routed
        .query_trace_summaries(trace_query(project))
        .await
        .expect("query_trace_summaries must succeed");
    assert_eq!(
        routed
            .count_traces(trace_query(project))
            .await
            .expect("count_traces must succeed"),
        7
    );
    assert!(routed
        .has_traces(project)
        .await
        .expect("has_traces must succeed"));
    routed
        .get_trace(project, "abc")
        .await
        .expect("get_trace must succeed");
    routed
        .query_span_stats(span_stats_query(vec![project]))
        .await
        .expect("query_span_stats must succeed");
    routed
        .count_span_stats(span_stats_query(vec![project]))
        .await
        .expect("count_span_stats must succeed");

    for (name, count) in [
        ("query_spans", &cloud.query_spans),
        ("query_trace_summaries", &cloud.query_trace_summaries),
        ("count_traces", &cloud.count_traces),
        ("has_traces", &cloud.has_traces),
        ("get_trace", &cloud.get_trace),
        ("query_span_stats", &cloud.query_span_stats),
        ("count_span_stats", &cloud.count_span_stats),
    ] {
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "{name} must be served from Cloud for a Cloud-primary project — an \
             unrouted reader is a silently empty feature"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_local_project_is_never_served_from_cloud() {
    // The other direction of the same invariant, and the more dangerous one:
    // reading Cloud for a `Local` project would be a confidently empty answer
    // about a store that never held those spans.
    let Some(harness) = Harness::start().await else {
        return;
    };
    let project = harness
        .project(
            "local-read-project",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;

    let local = LocalSpanStoreSpy::new();
    let cloud = Arc::new(CountingCloudSource::default());
    let routed = CloudRoutedOtelStorage::new(
        Arc::new(local.clone()) as Arc<dyn OtelStorage>,
        cloud.clone(),
        harness.write_modes(),
    );

    routed
        .query_spans(trace_query(project))
        .await
        .expect("query_spans must succeed");
    routed
        .get_trace(project, "abc")
        .await
        .expect("get_trace must succeed");
    assert!(!routed
        .has_traces(project)
        .await
        .expect("has_traces must succeed"));

    assert_eq!(cloud.query_spans.load(Ordering::SeqCst), 0);
    assert_eq!(cloud.get_trace.load(Ordering::SeqCst), 0);
    assert_eq!(cloud.has_traces.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_span_stats_query_across_both_sources_is_refused_rather_than_merged() {
    let Some(harness) = Harness::start().await else {
        return;
    };
    let cloud_project = harness
        .project(
            "stats-cloud",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;
    let local_project = harness
        .project(
            "stats-local",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;
    let write_modes = harness.write_modes();
    write_modes
        .set_write_mode(cloud_project, CloudTelemetryWriteMode::Cloud, linked())
        .await
        .expect("the gate must allow this project");

    let routed = CloudRoutedOtelStorage::new(
        Arc::new(LocalSpanStoreSpy::new()) as Arc<dyn OtelStorage>,
        Arc::new(CountingCloudSource::default()),
        write_modes,
    );

    let error = routed
        .query_span_stats(span_stats_query(vec![cloud_project, local_project]))
        .await
        .expect_err("percentiles from two stores cannot be combined");

    let message = error.to_string();
    assert!(
        message.contains(&cloud_project.to_string())
            && message.contains(&local_project.to_string()),
        "the refusal must name both sides so the operator can split the query: {message}"
    );
}

// ── 9. Facet registration onboards rather than silently never populating ───

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn facet_registration_on_a_cloud_primary_project_is_not_configured_with_a_setup_path() {
    let Some(harness) = Harness::start().await else {
        return;
    };
    let cloud_project = harness
        .project(
            "facet-cloud",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;
    let local_project = harness
        .project(
            "facet-local",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;
    let write_modes = harness.write_modes();
    write_modes
        .set_write_mode(cloud_project, CloudTelemetryWriteMode::Cloud, linked())
        .await
        .expect("the gate must allow this project");

    let facets = FacetService::new(harness.db.clone(), None, FacetCache::default())
        .with_write_mode_service(write_modes.clone());

    // A named Cloud-primary project: not configured, with a reason and a path.
    let capability = facets
        .capability(Some(cloud_project))
        .await
        .expect("capability must resolve");
    assert!(
        !capability.configured,
        "a facet on a Cloud-primary project would never populate"
    );
    let reason = capability
        .reason
        .expect("`configured: false` must always carry a reason");
    assert!(
        reason.contains("Cloud-primary"),
        "the reason must say what is actually going on: {reason}"
    );
    assert!(
        capability.setup_path.is_some(),
        "the operator must be given somewhere to go"
    );
    assert!(
        capability.uncovered_project_ids.contains(&cloud_project),
        "the uncovered projects must be named, not implied"
    );

    // A Local project on the same instance still works, and is still told which
    // projects the facet will miss.
    let local_capability = facets
        .capability(Some(local_project))
        .await
        .expect("capability must resolve");
    assert!(
        local_capability.configured,
        "one Cloud-primary project must not disable facets for the rest"
    );
    assert_eq!(
        local_capability.uncovered_project_ids,
        vec![cloud_project],
        "a working facet must still name the projects it does not cover"
    );

    // And when every project is Cloud-primary the facet covers nothing at all.
    write_modes
        .set_write_mode(local_project, CloudTelemetryWriteMode::Cloud, linked())
        .await
        .expect("the gate must allow this project");
    let none_left = facets
        .capability(None)
        .await
        .expect("capability must resolve");
    assert!(
        !none_left.configured,
        "with every project Cloud-primary a facet populates for nothing"
    );
    assert!(none_left
        .reason
        .expect("a reason is required")
        .contains("Cloud-primary"));
}

// ── The decommission signal ────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_local_project_keeps_the_whole_local_span_store_required() {
    // A partial cutover yields zero resource win, and an operator will
    // reasonably believe otherwise. This is the check that stops them deleting a
    // store that is still being written to.
    let Some(harness) = Harness::start().await else {
        return;
    };
    let cloud_project = harness
        .project(
            "decommission-cloud",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;
    harness
        .project(
            "decommission-local",
            CloudTelemetryFidelity::Queryable,
            CloudTelemetryWriteMode::Local,
        )
        .await;
    let write_modes = harness.write_modes();
    write_modes
        .set_write_mode(cloud_project, CloudTelemetryWriteMode::Cloud, linked())
        .await
        .expect("the gate must allow this project");

    let requirement = write_modes
        .local_span_store_requirement(30)
        .await
        .expect("the requirement must resolve");

    assert!(requirement.required);
    assert_eq!(requirement.local_mode_projects, 1);
    assert_eq!(requirement.cloud_primary_projects, 1);
    let reason = requirement
        .reason
        .expect("`required: true` must always say why");
    assert!(
        reason.contains("one project"),
        "the reason must state the all-or-nothing rule plainly: {reason}"
    );
}
