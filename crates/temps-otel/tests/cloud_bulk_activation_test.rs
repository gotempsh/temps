// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-042 P1 invariants: the bulk Cloud-telemetry activation engine.
//!
//! Each test here encodes a property the ADR states as load-bearing.
//!
//! 1. **Kill and restart mid-job** — the phase's stated acceptance criterion.
//!    A job is interrupted partway through the first project's backfill, every
//!    object that held state is rebuilt from scratch against the same database
//!    and the same on-disk link, and the run finishes. Cloud must receive every
//!    span exactly once: not one re-shipped (the customer pays for those) and
//!    not one skipped (that is a hole in their history).
//! 2. **Skip and continue** — a project whose `cloud_telemetry_fidelity` is not
//!    `queryable` is recorded as `skipped` with a machine-readable reason, is
//!    *not* switched, and does not stop the job. The job does not raise the
//!    fidelity on the operator's behalf.
//! 3. **Instance-wide abort** — a condition that belongs to the link stops the
//!    whole job with one reason, and leaves every untouched project `pending`
//!    rather than marking 23 projects `failed` for one revoked credential.
//! 4. **Cancellation at a chunk boundary** — honoured after a chunk has been
//!    acknowledged, with the cursor intact, so cancelling costs nothing already
//!    paid for.
//! 5. **At most one active job**, and **the switch is never rolled back**.
//!
//! # Simulating a restart
//!
//! There is no process to kill inside a test, so a restart is modelled as
//! faithfully as a test can: the shutdown watch fires (exactly as `OtelPlugin`'s
//! would), the worker returns, and then *every* stateful object is rebuilt —
//! including [`CloudLink`], reloaded from its state file the way a fresh
//! process would load it. Nothing but Postgres and the data directory survives
//! across the boundary, which is precisely what makes "the job tables are the
//! only resume state" a checkable claim rather than an assertion.
//!
//! # Docker
//!
//! Every test needs Postgres and skips gracefully when no container runtime is
//! available, per CLAUDE.md — never `#[ignore]`.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{extract::State, routing::post, Json, Router};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, TryGetable, Value};
use temps_cloud_client::{BackendUrl, CloudFeatureSwitches, CloudLink};
use temps_core::DBDateTime;
use temps_core::{
    CloudTelemetryActivationTrigger, TelemetryActivationOutcome, TelemetryActivationSkipped,
};
use temps_entities::cloud_telemetry_bulk_job_projects::BulkJobProjectStatus;
use temps_entities::cloud_telemetry_bulk_jobs::{BulkJobStatus, BulkJobTrigger};
use temps_entities::cloud_telemetry_fidelity::CloudTelemetryFidelity;
use temps_entities::cloud_telemetry_write_mode::CloudTelemetryWriteMode;
use temps_otel::services::cloud_bulk_activation_worker::BulkActivationTuning;
use temps_otel::services::{
    BulkActivationCycle, BulkJobProjectPlan, CloudBackfillProgressService, CloudBackfillSource,
    CloudBulkActivationError, CloudBulkActivationService, CloudBulkActivationWorker,
    CloudPolicyCache, EnqueueBulkJobRequest, PurchaseActivationTrigger, TelemetryWriteModeService,
    ANOMALY_PAUSE_CODE, MIN_ANOMALY_BUDGET_BYTES, UNESTIMATED_PAUSE_CODE,
};
use uuid::Uuid;

/// Spans written per simulated day, per project.
///
/// Comfortably under the 500-span submission size so one chunk is exactly one
/// Cloud submission — which makes "how far did it get" a whole number of
/// chunks rather than something the test has to reason about probabilistically.
const SPANS_PER_DAY: usize = 40;

/// Days of history for the first project — five chunks, so an interruption
/// after the first one leaves plenty unshipped.
const PROJECT_ONE_DAYS: i64 = 5;

/// Longest any single wait in these tests may take before failing loudly. A
/// hung worker must fail the suite, never hang it.
const TEST_TIMEOUT: Duration = Duration::from_secs(60);

// ── A Temps Cloud stub that remembers exactly which spans it received ──────

#[derive(Clone)]
struct Stub {
    /// Every `span_id` Cloud was asked to store, in arrival order, including
    /// repeats. Duplicates are the failure this whole phase exists to prevent,
    /// so they must be *visible* rather than deduplicated on the way in.
    received: Arc<Mutex<Vec<String>>>,
    /// Artificial per-submission latency, so a test can reliably interrupt a
    /// job in the middle rather than racing a run that finishes in microseconds.
    delay: Duration,
}

impl Stub {
    fn new(delay: Duration) -> Self {
        Self {
            received: Arc::new(Mutex::new(Vec::new())),
            delay,
        }
    }

    fn received(&self) -> Vec<String> {
        self.received
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn total(&self) -> usize {
        self.received().len()
    }

    fn unique(&self) -> usize {
        self.received().into_iter().collect::<HashSet<_>>().len()
    }

    /// Span ids Cloud was asked to store more than once.
    fn duplicates(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut repeated = Vec::new();
        for span_id in self.received() {
            if !seen.insert(span_id.clone()) {
                repeated.push(span_id);
            }
        }
        repeated
    }
}

async fn serve_stub(stub: Stub) -> Option<String> {
    let app = Router::new()
        .route(
            "/v1/enroll",
            post(|| async {
                Json(serde_json::json!({
                    "tenant_id": Uuid::new_v4(),
                    "instance_token": "inst_bulk_activation_test"
                }))
            }),
        )
        .route(
            "/v1/telemetry",
            post(
                |State(stub): State<Stub>,
                 Json(batch): Json<temps_cloud_protocol::TelemetryBatch>| async move {
                    if !stub.delay.is_zero() {
                        tokio::time::sleep(stub.delay).await;
                    }
                    let spans = batch.spans.len();
                    {
                        let mut received = stub
                            .received
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        received.extend(batch.spans.iter().map(|span| span.span_id.clone()));
                    }
                    Json(serde_json::json!({
                        "submission_id": batch.submission_id,
                        "processed_spans": spans,
                        "stored_spans": spans,
                        "metered_bytes": spans as i64 * 128
                    }))
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
            eprintln!("skipping bulk Cloud activation test: sandbox denied TCP bind");
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
    state_dir: tempfile::TempDir,
    backend: String,
}

impl Harness {
    /// `None` means the environment cannot run the test.
    async fn start(delay: Duration) -> Option<Self> {
        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                eprintln!("skipping bulk Cloud activation test: no test database ({error})");
                return None;
            }
        };
        let db = test_db.db.clone();

        let stub = Stub::new(delay);
        let backend = serve_stub(stub.clone()).await?;
        let state_dir = tempfile::tempdir().expect("temporary directory");
        let link = Arc::new(CloudLink::load_for_loopback_development(
            state_dir.path().to_path_buf(),
            "bulk-activation-test",
        ));
        link.configure(
            BackendUrl::loopback_development(&backend).expect("stub backend URL must be accepted"),
        )
        .expect("link must configure");
        link.enroll("bulk-activation-code")
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
            state_dir,
            backend,
        })
    }

    /// Reload the Cloud link from its state file, exactly as a fresh process
    /// would.
    ///
    /// This is what makes the restart test mean something: if any part of the
    /// resume position lived in `CloudLink`'s memory rather than in the job
    /// tables, it would be gone here.
    fn reload_link(&self) -> Arc<CloudLink> {
        let link = Arc::new(CloudLink::load_for_loopback_development(
            self.state_dir.path().to_path_buf(),
            "bulk-activation-test",
        ));
        // Feature switches come from the settings row at startup and are not
        // part of the state file, so a real boot re-applies them too.
        link.configure(
            BackendUrl::loopback_development(&self.backend)
                .expect("stub backend URL must be accepted"),
        )
        .expect("reloaded link must configure");
        link.set_feature_switches(CloudFeatureSwitches {
            telemetry: true,
            ..Default::default()
        })
        .expect("telemetry export must be enabled");
        assert!(
            link.is_linked(),
            "a reloaded link must still hold its persisted credential"
        );
        link
    }

    fn jobs(&self) -> Arc<CloudBulkActivationService> {
        Arc::new(CloudBulkActivationService::new(self.db.clone()))
    }

    /// A worker built the way `OtelPlugin` builds one, with every dependency
    /// freshly constructed.
    fn worker(
        &self,
        jobs: Arc<CloudBulkActivationService>,
        link: Arc<CloudLink>,
    ) -> CloudBulkActivationWorker {
        self.worker_with_anomaly_factor(jobs, link, BulkActivationTuning::default().anomaly_factor)
    }

    /// The same worker with the ADR-042 §6.3 byte-budget factor scaled.
    ///
    /// Only the unestimated-project test needs this. An unmeasured project is
    /// budgeted against a 64 MiB stand-in estimate, so at the shipped default
    /// factor a test would have to write hundreds of megabytes of spans to reach
    /// the guard. Scaling the factor down moves the budget to the 64 KiB floor,
    /// which the same span volume the other anomaly test uses already clears —
    /// the arithmetic under test is unchanged, only the constant it is fed.
    fn worker_with_anomaly_factor(
        &self,
        jobs: Arc<CloudBulkActivationService>,
        link: Arc<CloudLink>,
        anomaly_factor: f32,
    ) -> CloudBulkActivationWorker {
        CloudBulkActivationWorker::new(
            jobs,
            link,
            Arc::new(TelemetryWriteModeService::new(self.db.clone())),
            Arc::new(CloudPolicyCache::new(self.db.clone())),
            Arc::new(CloudBackfillProgressService::new(self.db.clone())),
            Arc::new(CloudBackfillSource::Timescale(self.db.clone())),
        )
        .with_tuning(BulkActivationTuning {
            chunk_days: 1,
            anomaly_factor,
            ..Default::default()
        })
    }

    async fn project(&self, slug: &str, fidelity: CloudTelemetryFidelity) -> i32 {
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO projects (name, repo_name, repo_owner, directory, main_branch, \
                 preset, created_at, updated_at, slug, cloud_telemetry_fidelity, \
                 cloud_telemetry_attribute_allowlist, cloud_telemetry_write_mode) \
                 VALUES ($1, 'repo', 'owner', '.', 'main', 'nodejs', now(), now(), $1, $2, \
                 ARRAY['http.route']::text[], 'local')",
                vec![slug.into(), fidelity.to_string().into()],
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

    /// Write `count` spans one second apart starting at `day`.
    async fn insert_spans(&self, project_id: i32, prefix: &str, day: DBDateTime, count: usize) {
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO otel_spans (project_id, service_name, deployment_environment, \
                 trace_id, span_id, name, kind, start_time, end_time, duration_ms, status_code, \
                 status_message, attributes, events) \
                 SELECT $1, 'checkout', 'production', '4bf92f3577b34da6a3ce929d0e0e4736', \
                        $2 || '-' || g::text, 'GET /orders', 'SERVER', \
                        $3::timestamptz + (g || ' seconds')::interval, \
                        $3::timestamptz + (g || ' seconds')::interval, \
                        1.5, 'OK', '', '{}'::jsonb, '[]'::jsonb \
                 FROM generate_series(0, $4::int - 1) AS g",
                vec![
                    project_id.into(),
                    prefix.into(),
                    day.into(),
                    Value::Int(Some(count as i32)),
                ],
            ))
            .await
            .expect("spans must insert");
    }

    async fn scalar<T: TryGetable>(&self, sql: &str, values: Vec<Value>) -> Option<T> {
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

    /// Put a project into a write mode directly, to set up a starting state the
    /// purchase path must respect (a project already Cloud-primary is not a
    /// candidate — there is nothing to switch and its history is already there).
    async fn set_write_mode(&self, project_id: i32, mode: CloudTelemetryWriteMode) {
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE projects SET cloud_telemetry_write_mode = $2 WHERE id = $1",
                vec![project_id.into(), mode.to_string().into()],
            ))
            .await
            .expect("write mode must update");
    }

    async fn write_mode_of(&self, project_id: i32) -> String {
        self.scalar::<String>(
            "SELECT cloud_telemetry_write_mode AS v FROM projects WHERE id = $1",
            vec![project_id.into()],
        )
        .await
        .expect("project must exist")
    }

    /// Set `cancel_requested_at` the way ADR-042 P2's endpoint eventually will,
    /// straight on the column, so this phase's worker behaviour is testable
    /// before that endpoint exists.
    async fn request_cancel_directly(&self, job_id: Uuid) {
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE cloud_telemetry_bulk_jobs SET cancel_requested_at = now() WHERE id = $1",
                vec![job_id.into()],
            ))
            .await
            .expect("cancellation must record");
    }
}

fn day(offset: i64) -> DBDateTime {
    // A fixed, historical base so the window never straddles "now" and the
    // test is not sensitive to when it runs.
    let base = chrono::DateTime::parse_from_rfc3339("2026-08-01T00:00:00Z")
        .expect("base timestamp must parse")
        .with_timezone(&chrono::Utc);
    base + chrono::Duration::days(offset)
}

fn plan(project_id: i32, from: DBDateTime, to: DBDateTime, spans: u64) -> BulkJobProjectPlan {
    BulkJobProjectPlan {
        project_id,
        window_from: from,
        window_to: to,
        estimated_spans: spans,
        estimated_bytes: spans * 128,
    }
}

/// Drive a worker through one full cycle, failing loudly rather than hanging.
///
/// One call is enough by construction: `run_once` walks the whole job and only
/// returns once it has stopped for a reason. `Idle` and `Deferred` are both
/// test bugs here — the first means the job vanished, the second that the
/// job's own bookkeeping failed — so they are surfaced rather than retried into
/// a spin.
async fn run_to_completion(
    worker: &CloudBulkActivationWorker,
    shutdown: &tokio::sync::watch::Receiver<bool>,
) -> BulkActivationCycle {
    let cycle = tokio::time::timeout(TEST_TIMEOUT, worker.run_once(shutdown))
        .await
        .expect("the worker must reach a terminal cycle");
    match cycle {
        BulkActivationCycle::Idle => {
            panic!("the worker found no active job while one was expected")
        }
        BulkActivationCycle::Deferred { reason } => {
            panic!("the worker deferred the job: {reason}")
        }
        cycle => cycle,
    }
}

// ── 1. Kill and restart mid-job (ADR-042 P1 acceptance criterion) ──────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_job_killed_mid_backfill_resumes_without_re_shipping_or_losing_position() {
    // The phase's whole reason to exist. A scoped Cloud submission persists
    // nothing resumable of its own (ADR-042 P0), so if the cursor were not in
    // the job tables this test would either re-ship history the customer has
    // already paid for, or silently skip it. Both are asserted against
    // directly, per span id.
    let Some(harness) = Harness::start(Duration::from_millis(120)).await else {
        return;
    };

    let first = harness
        .project("bulk-resume-one", CloudTelemetryFidelity::Queryable)
        .await;
    let second = harness
        .project("bulk-resume-two", CloudTelemetryFidelity::Queryable)
        .await;
    for offset in 0..PROJECT_ONE_DAYS {
        harness
            .insert_spans(
                first,
                &format!("first-d{offset}"),
                day(offset),
                SPANS_PER_DAY,
            )
            .await;
    }
    harness
        .insert_spans(second, "second-d0", day(0), SPANS_PER_DAY)
        .await;
    let first_total = (PROJECT_ONE_DAYS as usize * SPANS_PER_DAY) as i64;
    let expected_total = first_total + SPANS_PER_DAY as i64;

    let jobs = harness.jobs();
    let detail = jobs
        .enqueue_job(EnqueueBulkJobRequest {
            trigger: BulkJobTrigger::Purchase,
            requested_by_user_id: None,
            plan_hash: None,
            projects: vec![
                plan(first, day(0), day(PROJECT_ONE_DAYS), first_total as u64),
                plan(second, day(0), day(1), SPANS_PER_DAY as u64),
            ],
        })
        .await
        .expect("the job must be queued");
    let job_id = detail.job.id;

    // ── The kill ──────────────────────────────────────────────────────────
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let worker = harness.worker(jobs.clone(), Arc::clone(&harness.link));
    let running = tokio::spawn(async move { worker.run_once(&shutdown_rx).await });

    // Wait until the first project has acknowledged at least one chunk but is
    // demonstrably not finished, then pull the plug.
    let shipped_before = tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            let shipped = harness
                .scalar::<i64>(
                    "SELECT spans_shipped AS v FROM cloud_telemetry_bulk_job_projects \
                     WHERE job_id = $1 AND project_id = $2",
                    vec![job_id.into(), first.into()],
                )
                .await
                .unwrap_or(0);
            if shipped > 0 && shipped < first_total {
                shutdown_tx.send(true).expect("shutdown must send");
                return shipped;
            }
            assert!(
                shipped < first_total,
                "the first project finished before the test could interrupt it; raise the stub \
                 delay or the chunk count"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the first project must ship a chunk");

    let cycle = tokio::time::timeout(TEST_TIMEOUT, running)
        .await
        .expect("the interrupted worker must return")
        .expect("the worker task must not panic");
    assert_eq!(
        cycle,
        BulkActivationCycle::Interrupted { job_id },
        "a shutdown mid-job must leave the job running, not fail it"
    );

    // The job is left exactly where a killed process would leave it.
    let interrupted = jobs.job_detail(job_id).await.expect("job must be readable");
    assert_eq!(
        interrupted.job.status,
        BulkJobStatus::Running,
        "an interrupted job stays running so the next start resumes it without reconfirmation"
    );
    let first_row = interrupted
        .projects
        .iter()
        .find(|project| project.project_id == first)
        .expect("the first project must have a row");
    assert!(
        first_row.resume_start_time.is_some(),
        "the resume cursor must be durable — it is the only copy"
    );
    assert!(
        first_row.spans_shipped > 0 && first_row.spans_shipped < first_total,
        "expected a partial first project, got {}/{first_total}",
        first_row.spans_shipped
    );
    assert_eq!(
        harness.stub.total() as i64,
        first_row.spans_shipped,
        "the recorded total must equal what Cloud actually received"
    );

    // ── The restart ───────────────────────────────────────────────────────
    // Everything stateful is rebuilt, including the link, which is reloaded
    // from its state file exactly as a fresh process would load it.
    let restarted_jobs = harness.jobs();
    let restarted_link = harness.reload_link();
    let restarted_worker = harness.worker(restarted_jobs.clone(), restarted_link);
    let (_resume_tx, resume_rx) = tokio::sync::watch::channel(false);

    let cycle = run_to_completion(&restarted_worker, &resume_rx).await;
    assert_eq!(
        cycle,
        BulkActivationCycle::Finished {
            job_id,
            projects: 2
        }
    );

    // (a) Nothing was re-shipped.
    assert_eq!(
        harness.stub.duplicates(),
        Vec::<String>::new(),
        "a resumed job must never re-send a span the customer has already paid for"
    );
    // (b) The resume picked up from the cursor rather than from the start.
    assert!(
        harness.stub.total() as i64 > shipped_before,
        "the resume must actually ship the remainder"
    );
    // (c) Everything arrived, exactly once.
    assert_eq!(
        harness.stub.unique() as i64,
        expected_total,
        "every local span in the window must reach Cloud"
    );
    assert_eq!(harness.stub.total() as i64, expected_total);

    let finished = restarted_jobs
        .job_detail(job_id)
        .await
        .expect("job must be readable");
    assert_eq!(finished.job.status, BulkJobStatus::Completed);
    assert_eq!(
        finished.job.spans_shipped, expected_total,
        "the job total must equal the sum of its projects, across the restart"
    );
    assert!(finished.job.completed_at.is_some());
    for project in &finished.projects {
        assert_eq!(
            project.status,
            BulkJobProjectStatus::Done,
            "project {} should be done",
            project.project_id
        );
    }
    assert_eq!(harness.write_mode_of(first).await, "cloud");
    assert_eq!(harness.write_mode_of(second).await, "cloud");

    // ADR-042 §8: the per-project progress surface is reused, not duplicated,
    // and it says which job drove the run.
    let driving_job = harness
        .scalar::<Uuid>(
            "SELECT bulk_job_id AS v FROM cloud_telemetry_backfills WHERE project_id = $1",
            vec![first.into()],
        )
        .await;
    assert_eq!(driving_job, Some(job_id));
}

// ── 2. Skip and continue (ADR-042 §4) ─────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_project_that_fails_the_fidelity_gate_is_skipped_and_the_job_continues() {
    // The job must never raise a project's fidelity on the operator's behalf:
    // that is a separate decision with its own cost, and doing it as an
    // invisible consequence of paying would be exactly the kind of silent
    // spend this design refuses.
    let Some(harness) = Harness::start(Duration::ZERO).await else {
        return;
    };

    let metered = harness
        .project("bulk-skip-metered", CloudTelemetryFidelity::Metered)
        .await;
    let queryable = harness
        .project("bulk-skip-queryable", CloudTelemetryFidelity::Queryable)
        .await;
    harness
        .insert_spans(metered, "metered-d0", day(0), SPANS_PER_DAY)
        .await;
    harness
        .insert_spans(queryable, "queryable-d0", day(0), SPANS_PER_DAY)
        .await;

    let jobs = harness.jobs();
    let job_id = jobs
        .enqueue_job(EnqueueBulkJobRequest {
            trigger: BulkJobTrigger::Operator,
            requested_by_user_id: None,
            plan_hash: Some("plan-hash".into()),
            projects: vec![
                plan(metered, day(0), day(1), SPANS_PER_DAY as u64),
                plan(queryable, day(0), day(1), SPANS_PER_DAY as u64),
            ],
        })
        .await
        .expect("the job must be queued")
        .job
        .id;

    let worker = harness.worker(jobs.clone(), Arc::clone(&harness.link));
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let cycle = run_to_completion(&worker, &rx).await;

    assert_eq!(
        cycle,
        BulkActivationCycle::Finished {
            job_id,
            projects: 2
        }
    );
    let detail = jobs.job_detail(job_id).await.expect("job must be readable");
    assert_eq!(
        detail.job.status,
        BulkJobStatus::Completed,
        "a skip is not a failure: it is a prerequisite the operator has to supply"
    );

    let skipped = detail
        .projects
        .iter()
        .find(|project| project.project_id == metered)
        .expect("the metered project must have a row");
    assert_eq!(skipped.status, BulkJobProjectStatus::Skipped);
    assert_eq!(
        skipped.skip_reason.as_deref(),
        Some("fidelity_not_queryable"),
        "the reason must be machine-readable so the UI can link to the fix"
    );
    assert_eq!(skipped.spans_shipped, 0);
    assert_eq!(
        harness.write_mode_of(metered).await,
        "local",
        "a skipped project must not have been switched"
    );

    let done = detail
        .projects
        .iter()
        .find(|project| project.project_id == queryable)
        .expect("the queryable project must have a row");
    assert_eq!(done.status, BulkJobProjectStatus::Done);
    assert_eq!(done.spans_shipped, SPANS_PER_DAY as i64);
    assert_eq!(harness.write_mode_of(queryable).await, "cloud");

    assert_eq!(
        harness.stub.unique(),
        SPANS_PER_DAY,
        "only the eligible project's history may leave the instance"
    );
    for span_id in harness.stub.received() {
        assert!(
            span_id.starts_with("queryable-"),
            "a skipped project's spans must never be shipped: {span_id}"
        );
    }
}

// ── 3. Instance-wide abort (ADR-042 §7) ───────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_instance_wide_failure_aborts_the_job_and_leaves_the_rest_pending() {
    // `TelemetryExportDisabled` is a property of the link, not of project 17.
    // Marking every project `failed` would bury one real cause under a pile of
    // duplicates and would make the resume look like a retry of broken work.
    let Some(harness) = Harness::start(Duration::ZERO).await else {
        return;
    };

    let first = harness
        .project("bulk-abort-one", CloudTelemetryFidelity::Queryable)
        .await;
    let second = harness
        .project("bulk-abort-two", CloudTelemetryFidelity::Queryable)
        .await;
    harness
        .insert_spans(first, "abort-one-d0", day(0), SPANS_PER_DAY)
        .await;
    harness
        .insert_spans(second, "abort-two-d0", day(0), SPANS_PER_DAY)
        .await;

    let jobs = harness.jobs();
    let job_id = jobs
        .enqueue_job(EnqueueBulkJobRequest {
            trigger: BulkJobTrigger::Operator,
            requested_by_user_id: None,
            plan_hash: None,
            projects: vec![
                plan(first, day(0), day(1), SPANS_PER_DAY as u64),
                plan(second, day(0), day(1), SPANS_PER_DAY as u64),
            ],
        })
        .await
        .expect("the job must be queued")
        .job
        .id;

    // The operator withdraws consent before the job gets its turn.
    harness
        .link
        .set_feature_switches(CloudFeatureSwitches::default())
        .expect("telemetry export must be switchable off");

    let worker = harness.worker(jobs.clone(), Arc::clone(&harness.link));
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let cycle = run_to_completion(&worker, &rx).await;

    assert!(
        matches!(cycle, BulkActivationCycle::Aborted { .. }),
        "expected an abort, got {cycle:?}"
    );

    let detail = jobs.job_detail(job_id).await.expect("job must be readable");
    assert_eq!(detail.job.status, BulkJobStatus::Aborted);
    let reason = detail
        .job
        .abort_reason
        .as_deref()
        .expect("an aborted job must say why");
    assert!(
        reason.starts_with("telemetry_export_disabled"),
        "the reason must be machine-readable first: {reason}"
    );
    assert!(
        reason.contains("/settings/cloud"),
        "and must name the page that fixes it: {reason}"
    );

    for project in &detail.projects {
        assert_eq!(
            project.status,
            BulkJobProjectStatus::Pending,
            "project {} was never attempted and must stay pending, not fail",
            project.project_id
        );
        assert!(project.last_error.is_none());
    }
    assert_eq!(harness.write_mode_of(first).await, "local");
    assert_eq!(harness.write_mode_of(second).await, "local");
    assert_eq!(
        harness.stub.total(),
        0,
        "nothing may leave an instance whose operator switched export off"
    );
}

// ── 4. Cancellation at a chunk boundary (ADR-042 §7) ──────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancellation_is_honoured_at_a_chunk_boundary_with_the_cursor_intact() {
    // ADR-042 P2 will own the endpoint that writes `cancel_requested_at`; this
    // asserts the half that has to exist first — that the worker reads it, and
    // that stopping is lossless because the cursor is already durable.
    let Some(harness) = Harness::start(Duration::from_millis(120)).await else {
        return;
    };

    let project = harness
        .project("bulk-cancel", CloudTelemetryFidelity::Queryable)
        .await;
    for offset in 0..PROJECT_ONE_DAYS {
        harness
            .insert_spans(
                project,
                &format!("cancel-d{offset}"),
                day(offset),
                SPANS_PER_DAY,
            )
            .await;
    }
    let total = (PROJECT_ONE_DAYS as usize * SPANS_PER_DAY) as i64;

    let jobs = harness.jobs();
    let job_id = jobs
        .enqueue_job(EnqueueBulkJobRequest {
            trigger: BulkJobTrigger::Operator,
            requested_by_user_id: None,
            plan_hash: None,
            projects: vec![plan(project, day(0), day(PROJECT_ONE_DAYS), total as u64)],
        })
        .await
        .expect("the job must be queued")
        .job
        .id;

    let (_tx, rx) = tokio::sync::watch::channel(false);
    let worker = harness.worker(jobs.clone(), Arc::clone(&harness.link));
    let running = tokio::spawn(async move { worker.run_once(&rx).await });

    let shipped_at_cancel = tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            let shipped = harness
                .scalar::<i64>(
                    "SELECT spans_shipped AS v FROM cloud_telemetry_bulk_job_projects \
                     WHERE job_id = $1 AND project_id = $2",
                    vec![job_id.into(), project.into()],
                )
                .await
                .unwrap_or(0);
            if shipped > 0 && shipped < total {
                harness.request_cancel_directly(job_id).await;
                return shipped;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("a chunk must be acknowledged before the cancellation");

    let cycle = tokio::time::timeout(TEST_TIMEOUT, running)
        .await
        .expect("the cancelled worker must return")
        .expect("the worker task must not panic");
    assert_eq!(cycle, BulkActivationCycle::Cancelled { job_id });

    let detail = jobs.job_detail(job_id).await.expect("job must be readable");
    assert_eq!(detail.job.status, BulkJobStatus::Cancelled);
    assert!(detail.job.cancel_requested_at.is_some());
    assert!(detail.job.completed_at.is_some());

    let row = &detail.projects[0];
    assert_eq!(
        row.status,
        BulkJobProjectStatus::Pending,
        "a cancelled project is not a failed one; it stopped cleanly at a boundary"
    );
    assert!(
        row.resume_start_time.is_some(),
        "cancel must be lossless — the cursor is what makes resuming cost nothing"
    );
    assert!(row.spans_shipped >= shipped_at_cancel);
    assert!(
        row.spans_shipped < total,
        "the cancellation must actually have stopped the run"
    );
    assert_eq!(
        harness.stub.duplicates(),
        Vec::<String>::new(),
        "cancelling mid-window must not re-send anything"
    );
    // The switch already happened and is never rolled back (ADR-042 §7).
    assert_eq!(harness.write_mode_of(project).await, "cloud");
}

// ── 5. One active job at a time (ADR-042 §8) ──────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_job_is_refused_with_the_in_flight_job_id() {
    // Submission concurrency is 1 globally, so a second job would not run
    // twice as fast — it would contend for a scope that is exclusive
    // process-wide. The caller gets the id it should be watching instead of a
    // bare conflict.
    let Some(harness) = Harness::start(Duration::ZERO).await else {
        return;
    };
    let project = harness
        .project("bulk-single", CloudTelemetryFidelity::Queryable)
        .await;

    let jobs = harness.jobs();
    let first = jobs
        .enqueue_job(EnqueueBulkJobRequest {
            trigger: BulkJobTrigger::Operator,
            requested_by_user_id: None,
            plan_hash: None,
            projects: vec![plan(project, day(0), day(1), 0)],
        })
        .await
        .expect("the first job must be queued");

    let error = jobs
        .enqueue_job(EnqueueBulkJobRequest {
            trigger: BulkJobTrigger::Operator,
            requested_by_user_id: None,
            plan_hash: None,
            projects: vec![plan(project, day(0), day(1), 0)],
        })
        .await
        .expect_err("a second concurrent job must be refused");

    match error {
        CloudBulkActivationError::JobAlreadyActive { job_id, status } => {
            assert_eq!(job_id, first.job.id);
            assert_eq!(status, BulkJobStatus::Pending);
        }
        other => panic!("expected JobAlreadyActive, got {other}"),
    }

    // The database enforces the same thing, so a race loses cleanly rather
    // than letting both callers win.
    let direct = harness
        .db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO cloud_telemetry_bulk_jobs (id, \"trigger\", status) \
             VALUES ($1, 'operator', 'running')",
            vec![Uuid::new_v4().into()],
        ))
        .await;
    assert!(
        direct.is_err(),
        "a partial unique index must forbid a second active job even from raw SQL"
    );
}

// ── 6. An instance with no Cloud link is untouched (ADR-041 §4 style) ──────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_instance_with_no_job_leaves_every_project_exactly_as_it_was() {
    // The regression guard: these two tables are additive and inert. A default
    // install — no Cloud link, no job — must behave exactly as it did before
    // this change, and the worker must find nothing to do rather than
    // inventing work.
    let Some(harness) = Harness::start(Duration::ZERO).await else {
        return;
    };
    let project = harness
        .project("bulk-inert", CloudTelemetryFidelity::Queryable)
        .await;
    harness
        .insert_spans(project, "inert-d0", day(0), SPANS_PER_DAY)
        .await;

    let jobs = harness.jobs();
    assert!(jobs
        .active_job()
        .await
        .expect("the active-job lookup must work on an empty table")
        .is_none());

    let worker = harness.worker(jobs.clone(), Arc::clone(&harness.link));
    let (_tx, rx) = tokio::sync::watch::channel(false);
    assert_eq!(worker.run_once(&rx).await, BulkActivationCycle::Idle);

    // And the same on an instance that never linked at all — the shape most
    // installs are in. `OtelPlugin` does not even spawn the worker there, but
    // if it did, it would find nothing and change nothing.
    let unlinked_dir = tempfile::tempdir().expect("temporary directory");
    let unlinked = Arc::new(CloudLink::load(
        unlinked_dir.path().to_path_buf(),
        "bulk-activation-test",
    ));
    assert!(!unlinked.is_linked());
    let unlinked_worker = harness.worker(harness.jobs(), unlinked);
    assert_eq!(
        unlinked_worker.run_once(&rx).await,
        BulkActivationCycle::Idle
    );

    assert_eq!(
        harness.write_mode_of(project).await,
        CloudTelemetryWriteMode::Local.to_string()
    );
    assert_eq!(harness.stub.total(), 0);
    let (jobs_listed, total) = jobs
        .list_jobs(None, None)
        .await
        .expect("listing must work on an empty table");
    assert!(jobs_listed.is_empty());
    assert_eq!(total, 0);
}

// ── 7. ADR-042 P2: the plan an operator confirmed is the plan that ships ───

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_estimate_an_operator_confirms_is_exactly_what_gets_shipped() {
    // The two-phase confirm only means anything if the token in the middle
    // carries the estimate itself. This walks the operator path end to end —
    // `estimate_backfill` per project, mint, verify, enqueue, run — and then
    // asserts against the Cloud stub that what left the instance is what was
    // quoted. If the token ever became a bare hash with the project list
    // re-sent alongside it, an edited list would ship a different set than the
    // one that was priced, and this is the test that would notice.
    let Some(harness) = Harness::start(Duration::ZERO).await else {
        return;
    };

    // One project the operator may switch, and one at `metered` fidelity that
    // the estimate must exclude from the plan rather than quietly raise.
    let queryable = harness
        .project("bulk-plan-queryable", CloudTelemetryFidelity::Queryable)
        .await;
    let metered = harness
        .project("bulk-plan-metered", CloudTelemetryFidelity::Metered)
        .await;
    harness
        .insert_spans(queryable, "plan-d0", day(0), SPANS_PER_DAY)
        .await;
    harness
        .insert_spans(queryable, "plan-d1", day(1), SPANS_PER_DAY)
        .await;
    harness
        .insert_spans(metered, "plan-m0", day(0), SPANS_PER_DAY)
        .await;

    let source = CloudBackfillSource::Timescale(harness.db.clone());
    let policies = CloudPolicyCache::new(harness.db.clone());
    let (window_from, window_to) = (day(0), day(2));

    // The estimate. Nothing leaves the instance here — asserted below.
    let policy = policies
        .resolve_project(queryable)
        .await
        .expect("the project's policy must resolve");
    let estimate = temps_otel::services::estimate_backfill(
        &source,
        harness.link.as_ref(),
        &policy,
        queryable,
        window_from,
        window_to,
    )
    .await
    .expect("the estimate must succeed");
    assert_eq!(
        estimate.spans,
        (SPANS_PER_DAY * 2) as u64,
        "the quote must be an exact count, not a projection"
    );
    assert!(estimate.estimated_metered_bytes > 0);
    assert_eq!(
        harness.stub.total(),
        0,
        "estimating must send nothing — that is the entire reason it is a separate call"
    );

    // The metered project is not in the plan: raising fidelity costs money and
    // changes what leaves the instance, so paying must never do it implicitly.
    let metered_policy = policies
        .resolve_project(metered)
        .await
        .expect("the metered project's policy must resolve");
    assert!(!metered_policy.fidelity.is_queryable());

    let planned = vec![BulkJobProjectPlan {
        project_id: queryable,
        window_from,
        window_to,
        estimated_spans: estimate.spans,
        estimated_bytes: estimate.estimated_metered_bytes,
    }];

    let key = [42u8; 32];
    let minted = temps_otel::services::mint_plan_token(&key, &planned, chrono::Utc::now())
        .expect("the plan must mint");

    // A token minted by another instance must not authorize a spend here.
    assert!(
        temps_otel::services::verify_plan_token(&[7u8; 32], &minted.token, chrono::Utc::now())
            .is_err(),
        "a plan signed with a different key must not verify"
    );

    let verified = temps_otel::services::verify_plan_token(&key, &minted.token, chrono::Utc::now())
        .expect("the plan must verify");
    assert_eq!(
        verified.projects, planned,
        "the submit path must receive exactly the projects, windows and estimates that were \
         quoted — it has no other source for them"
    );

    // Submit, exactly as the handler does.
    let jobs = harness.jobs();
    let detail = jobs
        .enqueue_job(EnqueueBulkJobRequest {
            trigger: BulkJobTrigger::Operator,
            // `None`, the way the handler records a principal with no user
            // row behind it. The column is a foreign key into `users`, so this
            // is the only value a test without a seeded user may use.
            requested_by_user_id: None,
            plan_hash: Some(verified.plan_hash.clone()),
            projects: verified.projects.clone(),
        })
        .await
        .expect("the confirmed plan must queue");

    assert_eq!(
        detail.job.plan_hash.as_deref(),
        Some(minted.plan_hash.as_str())
    );
    assert_eq!(
        detail.job.estimated_spans, estimate.spans as i64,
        "the job's quoted total must be the operator's quoted total"
    );
    assert_eq!(detail.projects.len(), 1);

    // Run it.
    let worker = harness.worker(jobs.clone(), Arc::clone(&harness.link));
    let (_tx, rx) = tokio::sync::watch::channel(false);
    match run_to_completion(&worker, &rx).await {
        BulkActivationCycle::Finished { job_id, projects } => {
            assert_eq!(job_id, detail.job.id);
            assert_eq!(projects, 1);
        }
        other => panic!("expected the job to finish, got {other:?}"),
    }

    let finished = jobs
        .job_detail(detail.job.id)
        .await
        .expect("the finished job must be readable");
    assert_eq!(finished.job.status, BulkJobStatus::Completed);
    assert_eq!(
        finished.job.spans_shipped, estimate.spans as i64,
        "the instance must ship exactly what it quoted — no more (the customer pays for \
         those) and no fewer (that is a hole in their history)"
    );
    assert_eq!(
        harness.stub.total(),
        SPANS_PER_DAY * 2,
        "Temps Cloud must have received exactly the quoted spans"
    );

    // The project that was in the plan switched; the one that was not is
    // untouched, still storing its spans here.
    assert_eq!(
        harness.write_mode_of(queryable).await,
        CloudTelemetryWriteMode::Cloud.to_string()
    );
    assert_eq!(
        harness.write_mode_of(metered).await,
        CloudTelemetryWriteMode::Local.to_string(),
        "a project the estimate excluded must not be switched by the job it was excluded from"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_confirmed_plan_cannot_be_reused_to_queue_a_second_job() {
    // A plan token is an authorization for one spend. Replaying it while the
    // first job is still running must name the running job rather than
    // queueing a competing one that would ship — and bill — the same history
    // twice.
    let Some(harness) = Harness::start(Duration::ZERO).await else {
        return;
    };
    let project = harness
        .project("bulk-plan-replay", CloudTelemetryFidelity::Queryable)
        .await;

    let key = [42u8; 32];
    let planned = vec![plan(project, day(0), day(1), 0)];
    let minted = temps_otel::services::mint_plan_token(&key, &planned, chrono::Utc::now())
        .expect("the plan must mint");
    let verified = temps_otel::services::verify_plan_token(&key, &minted.token, chrono::Utc::now())
        .expect("the plan must verify");

    let jobs = harness.jobs();
    let first = jobs
        .enqueue_job(EnqueueBulkJobRequest {
            trigger: BulkJobTrigger::Operator,
            // `None`, the way the handler records a principal with no user
            // row behind it. The column is a foreign key into `users`, so this
            // is the only value a test without a seeded user may use.
            requested_by_user_id: None,
            plan_hash: Some(verified.plan_hash.clone()),
            projects: verified.projects.clone(),
        })
        .await
        .expect("the first submission must queue");

    let error = jobs
        .enqueue_job(EnqueueBulkJobRequest {
            trigger: BulkJobTrigger::Operator,
            // `None`, the way the handler records a principal with no user
            // row behind it. The column is a foreign key into `users`, so this
            // is the only value a test without a seeded user may use.
            requested_by_user_id: None,
            plan_hash: Some(verified.plan_hash.clone()),
            projects: verified.projects.clone(),
        })
        .await
        .expect_err("replaying the same plan must be refused");

    match error {
        CloudBulkActivationError::JobAlreadyActive { job_id, .. } => {
            assert_eq!(
                job_id, first.job.id,
                "the caller must be handed the id it should watch or cancel"
            );
        }
        other => panic!("expected JobAlreadyActive, got {other}"),
    }
    assert_eq!(harness.stub.total(), 0);
}

// ── 8. ADR-042 P3: the purchase path spends without a confirm, not blindly ─

/// Retention wide enough to cover this file's fixed historical span windows.
///
/// The purchase path's window is "everything local storage holds", derived from
/// the retention setting rather than chosen — so a test whose spans sit at a
/// fixed date in the past has to widen retention rather than pick a window.
const TEST_RETENTION_DAYS: u32 = 3_650;

fn purchase_trigger(harness: &Harness) -> PurchaseActivationTrigger {
    PurchaseActivationTrigger::new(
        harness.jobs(),
        Arc::new(TelemetryWriteModeService::new(harness.db.clone())),
        Arc::new(CloudPolicyCache::new(harness.db.clone())),
        Arc::clone(&harness.link),
        Arc::new(CloudBackfillSource::Timescale(harness.db.clone())),
        TEST_RETENTION_DAYS,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enrolling_queues_every_local_project_with_no_plan_token_and_no_operator() {
    // The asymmetry ADR-042 §1 exists to justify: the purchase path skips the
    // estimate-gates-start step, and *only* that step. It still estimates, still
    // records who authorized it (nobody — payment did), still refuses to raise a
    // project's fidelity, and still queues an ineligible project so the operator
    // can see why its history is not on Cloud.
    let Some(harness) = Harness::start(Duration::ZERO).await else {
        return;
    };

    let queryable = harness
        .project("purchase-queryable", CloudTelemetryFidelity::Queryable)
        .await;
    let metered = harness
        .project("purchase-metered", CloudTelemetryFidelity::Metered)
        .await;
    let already_cloud = harness
        .project("purchase-already-cloud", CloudTelemetryFidelity::Queryable)
        .await;
    harness
        .set_write_mode(already_cloud, CloudTelemetryWriteMode::Cloud)
        .await;

    harness
        .insert_spans(queryable, "purchase-q0", day(0), SPANS_PER_DAY)
        .await;
    harness
        .insert_spans(metered, "purchase-m0", day(0), SPANS_PER_DAY)
        .await;
    harness
        .insert_spans(already_cloud, "purchase-c0", day(0), SPANS_PER_DAY)
        .await;

    let started = match purchase_trigger(&harness)
        .start_purchase_activation()
        .await
        .expect("queueing the purchase activation must not error")
    {
        TelemetryActivationOutcome::Started(started) => started,
        other => panic!("expected an activation to start, got {other:?}"),
    };

    assert_eq!(
        started.project_ids,
        vec![queryable, metered],
        "the scope is every project still storing spans here — a project already \
         Cloud-primary has nothing to switch and its history is already on the other side"
    );
    assert!(
        started.estimated_spans >= SPANS_PER_DAY as i64,
        "the estimate still runs on this path: it is the ETA, the audit record and the \
         anomaly guard's budget, none of which are the confirm step"
    );
    assert!(started.estimated_bytes > 0);

    let jobs = harness.jobs();
    let job_id: Uuid = started
        .batch_id
        .parse()
        .expect("the batch id must be a uuid");
    let detail = jobs.job_detail(job_id).await.expect("job must be readable");

    assert_eq!(detail.job.trigger, BulkJobTrigger::Purchase);
    assert_eq!(
        detail.job.requested_by_user_id, None,
        "ADR-042 §8: the payment is the authorization; there is no operator to attribute \
         the spend to, and inventing one would misattribute it"
    );
    assert_eq!(
        detail.job.plan_hash, None,
        "ADR-042 §9: `plan_hash` is the identity of a *confirmed* estimate. Writing one here \
         would claim a two-phase confirm that never happened"
    );
    assert_eq!(detail.projects.len(), 2);

    let metered_row = detail
        .projects
        .iter()
        .find(|project| project.project_id == metered)
        .expect("an ineligible project must still be queued");
    assert_eq!(
        (metered_row.estimated_spans, metered_row.estimated_bytes),
        (0, 0),
        "an ineligible project is not estimated: nothing would be sent for it"
    );

    // Run it, and confirm the ineligible project surfaces as a skip on the only
    // screen this path ever shows the customer.
    let worker = harness.worker(jobs.clone(), Arc::clone(&harness.link));
    let (_tx, rx) = tokio::sync::watch::channel(false);
    assert_eq!(
        run_to_completion(&worker, &rx).await,
        BulkActivationCycle::Finished {
            job_id,
            projects: 2
        }
    );

    let finished = jobs.job_detail(job_id).await.expect("job must be readable");
    assert_eq!(finished.job.status, BulkJobStatus::Completed);
    let statuses: Vec<_> = finished
        .projects
        .iter()
        .map(|project| (project.project_id, project.status))
        .collect();
    assert!(
        statuses.contains(&(queryable, BulkJobProjectStatus::Done)),
        "{statuses:?}"
    );
    assert!(
        statuses.contains(&(metered, BulkJobProjectStatus::Skipped)),
        "paying must never raise a project's fidelity on the operator's behalf: {statuses:?}"
    );

    assert_eq!(
        harness.stub.unique(),
        SPANS_PER_DAY,
        "only the eligible project's history may leave the instance"
    );
    for span_id in harness.stub.received() {
        assert!(
            span_id.starts_with("purchase-q"),
            "neither a skipped nor an already-Cloud project's spans may be shipped: {span_id}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fresh_instance_with_nothing_to_activate_queues_no_empty_job() {
    // An empty job renders as a progress card stuck at 0% with nothing behind
    // it, which on a fresh install is indistinguishable from a hang — and the
    // customer is watching this screen precisely because they just paid.
    let Some(harness) = Harness::start(Duration::ZERO).await else {
        return;
    };

    let outcome = purchase_trigger(&harness)
        .start_purchase_activation()
        .await
        .expect("an empty instance is not an error");

    assert!(
        matches!(
            outcome,
            TelemetryActivationOutcome::Skipped(TelemetryActivationSkipped::NoLocalProjects)
        ),
        "expected a no-projects skip, got {outcome:?}"
    );
    let (listed, total) = harness
        .jobs()
        .list_jobs(None, None)
        .await
        .expect("listing must work");
    assert!(listed.is_empty(), "no job row may be written");
    assert_eq!(total, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_enrollment_points_at_the_running_job_instead_of_queueing_a_rival() {
    // `POST /cloud/enroll` is callable again by anyone who can already reach it.
    // Doing so must not be able to queue a second job that ships — and bills —
    // the same history twice; submission concurrency is 1 globally anyway.
    let Some(harness) = Harness::start(Duration::ZERO).await else {
        return;
    };
    let project = harness
        .project("purchase-rerun", CloudTelemetryFidelity::Queryable)
        .await;
    harness
        .insert_spans(project, "rerun-d0", day(0), SPANS_PER_DAY)
        .await;

    let trigger = purchase_trigger(&harness);
    let first = match trigger
        .start_purchase_activation()
        .await
        .expect("the first activation must queue")
    {
        TelemetryActivationOutcome::Started(started) => started,
        other => panic!("expected an activation to start, got {other:?}"),
    };

    let second = trigger
        .start_purchase_activation()
        .await
        .expect("a repeated enrollment is not an error");

    match second {
        TelemetryActivationOutcome::Skipped(TelemetryActivationSkipped::AlreadyActive {
            batch_id,
        }) => assert_eq!(
            batch_id, first.batch_id,
            "the caller must be pointed at the job that is already spending"
        ),
        other => panic!("expected an already-active skip, got {other:?}"),
    }

    let (listed, total) = harness
        .jobs()
        .list_jobs(None, None)
        .await
        .expect("listing must work");
    assert_eq!(total, 1, "exactly one job may exist: {listed:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enrolling_without_telemetry_consent_activates_nothing() {
    // Telemetry export is explicit consent (ADR-040). Enrolling with it off is a
    // deliberate choice, and a job that shipped anyway would override it while a
    // job that immediately aborted would put a red card in front of an operator
    // whose instance is working exactly as they configured it.
    let Some(harness) = Harness::start(Duration::ZERO).await else {
        return;
    };
    let project = harness
        .project("purchase-no-consent", CloudTelemetryFidelity::Queryable)
        .await;
    harness
        .insert_spans(project, "no-consent-d0", day(0), SPANS_PER_DAY)
        .await;
    harness
        .link
        .set_feature_switches(CloudFeatureSwitches::default())
        .expect("telemetry export must switch off");

    let outcome = purchase_trigger(&harness)
        .start_purchase_activation()
        .await
        .expect("withheld consent is not an error");

    match outcome {
        TelemetryActivationOutcome::Skipped(TelemetryActivationSkipped::NotConfigured {
            reason,
            setup_path,
        }) => {
            assert!(reason.contains("switched off"), "{reason}");
            assert!(
                setup_path.is_some(),
                "the operator must be told where to fix it"
            );
        }
        other => panic!("expected a not-configured skip, got {other:?}"),
    }

    let (_listed, total) = harness
        .jobs()
        .list_jobs(None, None)
        .await
        .expect("listing must work");
    assert_eq!(total, 0);
    assert_eq!(harness.stub.total(), 0);
    assert_eq!(
        harness.write_mode_of(project).await,
        CloudTelemetryWriteMode::Local.to_string()
    );
}

// ── 9. ADR-042 §6.3 / P4: the byte-budget anomaly guard ────────────────────

/// Spans per day for the project whose estimate is wrong.
///
/// Sized so a single one-day chunk ships well past
/// [`MIN_ANOMALY_BUDGET_BYTES`] (64 KiB) even at a pessimistically small
/// serialized span size, so the guard is tested by the *factor* rather than by
/// the floor being hit incidentally.
const ANOMALY_SPANS_PER_DAY: usize = 800;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_runaway_project_is_stopped_mid_window_and_the_job_carries_on() {
    // ADR-042 §6.3: "If a project's shipped bytes exceed its estimate by more
    // than a bounded factor, the job pauses that project and surfaces it, rather
    // than running away with the customer's money."
    //
    // The two halves of that sentence are both asserted: the runaway project
    // stops with two of its three days still on this instance and unspent, and
    // the projects on either side of it in the same job finish untouched. A
    // per-project anomaly is not an instance-wide failure (§7).
    let Some(harness) = Harness::start(Duration::ZERO).await else {
        return;
    };

    // Created in order, so `id` ascends with them and the runaway is processed
    // between two honest projects rather than last.
    let before = harness
        .project("anomaly-before", CloudTelemetryFidelity::Queryable)
        .await;
    let runaway = harness
        .project("anomaly-runaway", CloudTelemetryFidelity::Queryable)
        .await;
    let after = harness
        .project("anomaly-after", CloudTelemetryFidelity::Queryable)
        .await;

    harness
        .insert_spans(before, "before-d0", day(0), SPANS_PER_DAY)
        .await;
    harness
        .insert_spans(after, "after-d0", day(0), SPANS_PER_DAY)
        .await;
    // Three chunks for the runaway. The first alone blows the budget; the
    // second and third are the history the guard must stop from shipping.
    //
    // Days 1 and 2 are offset an hour past midnight on purpose: `split_window`
    // produces chunks with an inclusive upper bound, so a span written at
    // exactly `day(n)` belongs to chunk `n-1`, and the assertions below want a
    // clean "these spans are in the chunks that never ran".
    let hour = chrono::Duration::hours(1);
    harness
        .insert_spans(runaway, "runaway-d0", day(0), ANOMALY_SPANS_PER_DAY)
        .await;
    harness
        .insert_spans(runaway, "runaway-d1", day(1) + hour, ANOMALY_SPANS_PER_DAY)
        .await;
    harness
        .insert_spans(runaway, "runaway-d2", day(2) + hour, ANOMALY_SPANS_PER_DAY)
        .await;

    let jobs = harness.jobs();
    let job_id = jobs
        .enqueue_job(EnqueueBulkJobRequest {
            trigger: BulkJobTrigger::Purchase,
            requested_by_user_id: None,
            plan_hash: None,
            projects: vec![
                plan(before, day(0), day(1), SPANS_PER_DAY as u64),
                // The bad estimate: one span's worth quoted for 2,400 spans of
                // history. This is what an order-of-magnitude estimate error
                // looks like from the worker's side.
                BulkJobProjectPlan {
                    project_id: runaway,
                    window_from: day(0),
                    window_to: day(3),
                    estimated_spans: 1,
                    estimated_bytes: 1,
                },
                plan(after, day(0), day(1), SPANS_PER_DAY as u64),
            ],
        })
        .await
        .expect("the job must be queued")
        .job
        .id;

    let worker = harness.worker(jobs.clone(), Arc::clone(&harness.link));
    let (_tx, rx) = tokio::sync::watch::channel(false);
    assert_eq!(
        run_to_completion(&worker, &rx).await,
        BulkActivationCycle::Finished {
            job_id,
            projects: 3
        },
        "a per-project anomaly must never abort the whole job"
    );

    let detail = jobs.job_detail(job_id).await.expect("job must be readable");
    assert_eq!(
        detail.job.status,
        BulkJobStatus::CompletedWithFailures,
        "the job needs a retry affordance, not a green checkmark"
    );

    let row = |project_id: i32| {
        detail
            .projects
            .iter()
            .find(|project| project.project_id == project_id)
            .unwrap_or_else(|| panic!("project {project_id} must have a row"))
    };

    // The runaway: stopped, with a reason a human can act on and most of its
    // history — and its cost — still on this instance.
    let paused = row(runaway);
    assert_eq!(paused.status, BulkJobProjectStatus::Failed);
    assert_eq!(
        paused.spans_shipped, ANOMALY_SPANS_PER_DAY as i64,
        "exactly one chunk may ship before the guard fires: the other two days are the \
         money this guard exists to not spend"
    );
    assert!(
        (paused.spans_shipped as usize) < ANOMALY_SPANS_PER_DAY * 3,
        "the point of the guard is stopping *before* the whole window ships"
    );
    assert!(
        paused.bytes_shipped as u64 > MIN_ANOMALY_BUDGET_BYTES,
        "the guard must have fired on the factor, not merely on the floor: shipped {} bytes",
        paused.bytes_shipped
    );
    let last_error = paused
        .last_error
        .as_deref()
        .expect("a paused project must say why");
    assert!(
        last_error.starts_with(&format!("{ANOMALY_PAUSE_CODE}: ")),
        "an anomaly pause must be distinguishable from a transport failure in the one column \
         both are recorded in: {last_error}"
    );
    assert!(
        last_error.contains("anomaly factor") && last_error.contains("Settings"),
        "the stored reason must survive truncation with its tuning pointer intact — that \
         sentence is the only place the operator learns this budget is theirs to change: \
         {last_error}"
    );
    assert_eq!(
        harness.write_mode_of(runaway).await,
        CloudTelemetryWriteMode::Cloud.to_string(),
        "ADR-042 §7: the switch is never rolled back — a recorded, retryable hole is honest, \
         a silently bisected timeline is not"
    );

    // The projects around it: untouched by somebody else's bad estimate.
    for (project_id, prefix) in [(before, "before-"), (after, "after-")] {
        let done = row(project_id);
        assert_eq!(
            done.status,
            BulkJobProjectStatus::Done,
            "project {project_id} must not be stopped by another project's anomaly"
        );
        assert_eq!(done.spans_shipped, SPANS_PER_DAY as i64);
        assert_eq!(
            harness.write_mode_of(project_id).await,
            CloudTelemetryWriteMode::Cloud.to_string()
        );
        assert!(
            harness
                .stub
                .received()
                .iter()
                .any(|span_id| span_id.starts_with(prefix)),
            "project {project_id}'s history must still have shipped"
        );
    }

    // And the runaway's later days really did not leave the instance.
    for span_id in harness.stub.received() {
        assert!(
            !span_id.starts_with("runaway-d1") && !span_id.starts_with("runaway-d2"),
            "the guard must stop the transfer, not merely record it afterwards: {span_id}"
        );
    }
}

/// Factor that puts an unmeasured project's budget on the 64 KiB floor.
///
/// See `Harness::worker_with_anomaly_factor`: the stand-in estimate is 64 MiB,
/// so this is what lets the same span volume the test above uses reach the
/// guard without writing hundreds of megabytes.
const UNESTIMATED_TEST_ANOMALY_FACTOR: f32 = 0.0001;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_project_with_no_estimate_is_bounded_rather_than_shipped_without_a_limit() {
    // The security fix, end to end. A zero `estimated_bytes` does not mean the
    // project is empty — it means nobody counted, which `plan_for` records
    // whenever `estimate_backfill` fails. The guard used to skip exactly those
    // projects: a zero estimate produced no budget, and "no budget" was read as
    // "never exceeds", so the one path that spends a customer's money with no
    // human confirm handed an *unbounded* allowance to precisely the projects
    // nothing was known about.
    //
    // Before the fix every one of this project's three days shipped. Now it
    // stops mid-window with the rest of its history still on this instance.
    let Some(harness) = Harness::start(Duration::ZERO).await else {
        return;
    };

    let unestimated = harness
        .project("unestimated", CloudTelemetryFidelity::Queryable)
        .await;
    let hour = chrono::Duration::hours(1);
    harness
        .insert_spans(unestimated, "unest-d0", day(0), ANOMALY_SPANS_PER_DAY)
        .await;
    harness
        .insert_spans(
            unestimated,
            "unest-d1",
            day(1) + hour,
            ANOMALY_SPANS_PER_DAY,
        )
        .await;
    harness
        .insert_spans(
            unestimated,
            "unest-d2",
            day(2) + hour,
            ANOMALY_SPANS_PER_DAY,
        )
        .await;

    let jobs = harness.jobs();
    let job_id = jobs
        .enqueue_job(EnqueueBulkJobRequest {
            trigger: BulkJobTrigger::Purchase,
            requested_by_user_id: None,
            plan_hash: None,
            // The exact shape a failed `estimate_backfill` leaves behind: a real
            // window, a real project, and no measurement of either.
            projects: vec![BulkJobProjectPlan {
                project_id: unestimated,
                window_from: day(0),
                window_to: day(3),
                estimated_spans: 0,
                estimated_bytes: 0,
            }],
        })
        .await
        .expect("the job must be queued")
        .job
        .id;

    let worker = harness.worker_with_anomaly_factor(
        jobs.clone(),
        Arc::clone(&harness.link),
        UNESTIMATED_TEST_ANOMALY_FACTOR,
    );
    let (_tx, rx) = tokio::sync::watch::channel(false);
    assert_eq!(
        run_to_completion(&worker, &rx).await,
        BulkActivationCycle::Finished {
            job_id,
            projects: 1
        },
        "a per-project budget stop is not an instance-wide abort"
    );

    let detail = jobs.job_detail(job_id).await.expect("job must be readable");
    let paused = detail
        .projects
        .first()
        .expect("the project must have a row");
    assert_eq!(
        paused.status,
        BulkJobProjectStatus::Failed,
        "an unmeasured project that runs past its budget must stop, not complete quietly"
    );
    assert_eq!(
        paused.spans_shipped, ANOMALY_SPANS_PER_DAY as i64,
        "exactly one chunk may ship before the guard fires: the other two days are the \
         unbounded spend this fix exists to prevent"
    );

    let last_error = paused
        .last_error
        .as_deref()
        .expect("a paused project must say why");
    assert!(
        last_error.starts_with(&format!("{UNESTIMATED_PAUSE_CODE}: ")),
        "the row shows estimated_bytes: 0, so the reason must say the zero is a failed \
         measurement rather than reading as a budget derived from nothing: {last_error}"
    );
    assert!(
        last_error.contains("anomaly factor") && last_error.contains("Settings"),
        "the remedy must survive truncation — it is the only place an operator alone learns \
         this budget is theirs to change: {last_error}"
    );

    // ADR-042 §7: the switch is never rolled back, and the untouched remainder
    // really did not leave the instance.
    assert_eq!(
        harness.write_mode_of(unestimated).await,
        CloudTelemetryWriteMode::Cloud.to_string()
    );
    for span_id in harness.stub.received() {
        assert!(
            !span_id.starts_with("unest-d1") && !span_id.starts_with("unest-d2"),
            "the guard must stop the transfer, not merely record it afterwards: {span_id}"
        );
    }
}
