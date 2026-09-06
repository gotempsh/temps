// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-041 Phase B1 acceptance: the durable outbox under load.
//!
//! These are **load-based acceptance criteria, not coverage**. Phase B1 is the
//! prerequisite that has to hold before any project can be set Cloud-primary,
//! because promoting a transport to a primary write path without proving its
//! throughput, durability and overflow semantics is how you build a data-loss
//! machine with a reassuring status page.
//!
//! # The target rate, and why this one
//!
//! **500 spans/second sustained for 20 seconds, draining to empty.**
//!
//! Justified against realistic instance load rather than picked for
//! convenience:
//!
//! - The reference deployment is a 3 vCPU / 4 GB box. A busy small instance
//!   runs on the order of 50 fully-traced requests/second, and a typical
//!   auto-instrumented request produces ~10 spans (inbound server span, a
//!   couple of database calls, one or two outbound HTTP calls, some internal
//!   work). 50 × 10 = 500 spans/second.
//! - The existing in-memory spool tops out at ~33 spans/second in steady state
//!   (500 spans per 15-second flush tick — ADR-041 Finding 3, measured from
//!   `link.rs`'s `BATCH_SIZE` and `flusher.rs`'s `BASE_INTERVAL`). The target is
//!   **15× that ceiling**, which is the margin that makes the drain-until-idle
//!   change worth making at all.
//! - 20 seconds is long enough that a per-batch cost would compound visibly:
//!   10,000 spans is 20 wire batches and, at the enqueue side, tens of
//!   multi-row inserts. A one-shot burst would hide exactly the amortised costs
//!   this is meant to expose.
//!
//! The bottleneck being measured is the *instance* side — the durable enqueue
//! on the ingest path and the drain loop — so the Cloud side is a local stub.
//! What `/v1/telemetry` will sustain from one instance over a real network is
//! explicitly an open question in ADR-041 §3b and cannot be answered here.
//!
//! # Docker
//!
//! Every test skips gracefully when no container runtime is available, per
//! CLAUDE.md — never `#[ignore]`.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{extract::State, routing::post, Json, Router};
use sea_orm::{ConnectionTrait, DatabaseBackend, FromQueryResult, Statement};
use temps_cloud_client::outbox_worker::drain_until_idle;
use temps_cloud_client::{
    BackendUrl, CloudFeatureSwitches, CloudLink, SpanOutbox, OUTBOX_BATCH_SIZE,
};
use temps_cloud_protocol::{SpanRecord, TelemetryBatch};
use uuid::Uuid;

/// Spans per second the outbox must sustain. See the module docs.
const TARGET_SPANS_PER_SECOND: usize = 500;
/// How long the sustained-rate test runs.
const SUSTAIN_SECONDS: usize = 20;
/// Spans per producer batch, matching a realistic OTLP export batch.
const PRODUCER_BATCH: usize = 50;

const TEST_PROJECT: i32 = 7;

// ── Cloud stub ─────────────────────────────────────────────────────────────

#[derive(Clone, Default)]
struct Stub {
    /// When true, `/v1/telemetry` answers 503 — "Cloud is down".
    down: Arc<AtomicBool>,
    received_spans: Arc<AtomicUsize>,
    received_batches: Arc<AtomicUsize>,
    largest_batch: Arc<AtomicU64>,
}

async fn serve(stub: Stub) -> Option<String> {
    let app = Router::new()
        .route(
            "/v1/enroll",
            post(|| async {
                Json(serde_json::json!({
                    "tenant_id": Uuid::new_v4(),
                    "instance_token": "inst_outbox_load_test"
                }))
            }),
        )
        .route(
            "/v1/telemetry",
            post(
                |State(stub): State<Stub>, Json(batch): Json<TelemetryBatch>| async move {
                    if stub.down.load(Ordering::SeqCst) {
                        return (
                            axum::http::StatusCode::SERVICE_UNAVAILABLE,
                            Json(serde_json::json!({"detail": "stub is down"})),
                        );
                    }
                    let spans = batch.spans.len();
                    stub.received_spans.fetch_add(spans, Ordering::SeqCst);
                    stub.received_batches.fetch_add(1, Ordering::SeqCst);
                    stub.largest_batch.fetch_max(spans as u64, Ordering::SeqCst);
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
            eprintln!("skipping outbox load test: sandbox denied TCP bind");
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
    db: Arc<sea_orm::DatabaseConnection>,
    link: Arc<CloudLink>,
    stub: Stub,
    _state_dir: tempfile::TempDir,
}

impl Harness {
    /// `None` means the environment cannot run this test (no Docker, or no TCP
    /// bind), and the caller returns rather than failing.
    async fn start() -> Option<Self> {
        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                eprintln!("skipping outbox load test: no test database ({error})");
                return None;
            }
        };
        let db = test_db.db.clone();

        // A real project row, at the id the queue is loaded against. The
        // shipping worker refuses to claim rows whose project no longer exists —
        // that is what stops a deleted project's telemetry still being exported
        // — so a load test against a phantom project id would measure a queue
        // that never drains.
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO projects (id, name, repo_name, repo_owner, directory, main_branch, \
             preset, created_at, updated_at, slug, cloud_telemetry_fidelity, \
             cloud_telemetry_attribute_allowlist, cloud_telemetry_write_mode) \
             VALUES ($1, 'outbox-load-test', 'repo', 'owner', '.', 'main', 'nodejs', now(), \
             now(), 'outbox-load-test', 'queryable', \
             ARRAY['http.route']::text[], 'cloud')",
            vec![TEST_PROJECT.into()],
        ))
        .await
        .expect("the load test's project must insert");

        let stub = Stub::default();
        let backend = serve(stub.clone()).await?;

        let state_dir = tempfile::tempdir().expect("temporary directory");
        let link = Arc::new(CloudLink::load_for_loopback_development(
            state_dir.path().to_path_buf(),
            "outbox-load-test",
        ));
        link.configure(
            BackendUrl::loopback_development(&backend).expect("stub backend URL must be accepted"),
        )
        .expect("link must configure");
        link.enroll("load-test-code")
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

    fn outbox(&self, max_bytes: u64) -> Arc<SpanOutbox> {
        Arc::new(SpanOutbox::new(self.db.clone(), max_bytes))
    }

    async fn row_count(&self, state: &str) -> i64 {
        #[derive(FromQueryResult)]
        struct Counted {
            n: i64,
        }
        Counted::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT COUNT(*)::bigint AS n FROM cloud_telemetry_outbox \
             WHERE entity_type = 'span' AND state = $1",
            vec![state.into()],
        ))
        .one(self.db.as_ref())
        .await
        .expect("outbox count must be readable")
        .map_or(0, |row| row.n)
    }

    async fn gap_windows(
        &self,
    ) -> Vec<(
        chrono::DateTime<chrono::Utc>,
        chrono::DateTime<chrono::Utc>,
        i64,
    )> {
        #[derive(FromQueryResult)]
        struct Gap {
            started_at: chrono::DateTime<chrono::Utc>,
            ended_at: chrono::DateTime<chrono::Utc>,
            dropped_spans: i64,
        }
        Gap::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT started_at, ended_at, dropped_spans FROM telemetry_gap_windows \
             ORDER BY started_at",
            vec![],
        ))
        .all(self.db.as_ref())
        .await
        .expect("gap windows must be readable")
        .into_iter()
        .map(|gap| (gap.started_at, gap.ended_at, gap.dropped_spans))
        .collect()
    }

    /// Truncate the queue between phases of one test without rebuilding the
    /// whole container.
    async fn reset_queue(&self) {
        self.db
            .execute(Statement::from_string(
                DatabaseBackend::Postgres,
                "DELETE FROM cloud_telemetry_outbox".to_string(),
            ))
            .await
            .expect("queue must be resettable");
    }
}

/// A `Queryable`-fidelity span, sized like a real one.
///
/// Deliberately not minimal: a 40-byte span would make the byte cap and the
/// throughput numbers meaningless. Attribute keys and values are the shape an
/// allowlisted HTTP span actually carries.
fn span(i: usize) -> SpanRecord {
    SpanRecord {
        trace_id: format!("{:032x}", i),
        span_id: format!("{:016x}", i),
        name: "GET /api/v1/orders/{id}".to_string(),
        ts_millis: chrono::Utc::now().timestamp_millis(),
        duration_ms: 12.5,
        attributes: [
            ("http.route".to_string(), "/api/v1/orders/{id}".to_string()),
            ("http.method".to_string(), "GET".to_string()),
            ("http.status_code".to_string(), "200".to_string()),
        ]
        .into_iter()
        .collect(),
        project_ref: "b6f0c8a1e2d34f5a9c7b1e8d0a2f4c6b8d0e2a4c6f8b0d2e4a6c8f0b2d4e6a8c".to_string(),
        service_name: Some("orders-api".to_string()),
        span_kind: Some("SERVER".to_string()),
        status_code: Some("OK".to_string()),
        parent_span_id: Some(format!("{:016x}", i.saturating_sub(1))),
        environment: Some("production".to_string()),
    }
}

fn batch(start: usize, count: usize) -> Vec<SpanRecord> {
    (start..start + count).map(span).collect()
}

// ── 1. Sustained rate with Cloud reachable, draining to empty ──────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sustains_the_target_rate_with_cloud_reachable_and_drains_to_empty() {
    let Some(harness) = Harness::start().await else {
        return;
    };
    // 512 MiB, the shipped default — the cap must not be what makes this pass.
    let outbox = harness.outbox(temps_core::DEFAULT_CLOUD_TELEMETRY_OUTBOX_MAX_BYTES);

    let total_spans = TARGET_SPANS_PER_SECOND * SUSTAIN_SECONDS;
    let batches_per_second = TARGET_SPANS_PER_SECOND / PRODUCER_BATCH;
    let tick = Duration::from_millis(1000 / batches_per_second as u64);

    // Drain concurrently with production, which is the real shape: the worker
    // is not waiting for the producer to finish.
    let drain_link = harness.link.clone();
    let drain_outbox = outbox.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let drain_stop = stop.clone();
    let drainer = tokio::spawn(async move {
        while !drain_stop.load(Ordering::SeqCst) {
            drain_until_idle(&drain_link, &drain_outbox, 40).await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // Final catch-up after production stops.
        for _ in 0..200 {
            let outcome = drain_until_idle(&drain_link, &drain_outbox, 40).await;
            if matches!(outcome, temps_cloud_client::DrainOutcome::Idle) {
                break;
            }
        }
    });

    let started = Instant::now();
    let mut produced = 0usize;
    let mut next_tick = started;
    while produced < total_spans {
        let outcome = outbox
            .enqueue(TEST_PROJECT, &batch(produced, PRODUCER_BATCH))
            .await
            .expect("enqueue must not fail with Cloud reachable");
        assert_eq!(
            outcome.refused, 0,
            "nothing may be refused below the byte cap"
        );
        produced += outcome.accepted;

        next_tick += tick;
        let now = Instant::now();
        if next_tick > now {
            tokio::time::sleep(next_tick - now).await;
        }
    }
    let production_elapsed = started.elapsed();

    stop.store(true, Ordering::SeqCst);
    drainer.await.expect("drain task must not panic");

    let achieved_rate = produced as f64 / production_elapsed.as_secs_f64();
    println!(
        "enqueued {produced} spans in {:.2}s ({achieved_rate:.0} spans/s); \
         Cloud received {} spans in {} batches (largest {})",
        production_elapsed.as_secs_f64(),
        harness.stub.received_spans.load(Ordering::SeqCst),
        harness.stub.received_batches.load(Ordering::SeqCst),
        harness.stub.largest_batch.load(Ordering::SeqCst),
    );

    assert_eq!(produced, total_spans, "every span must be accepted");
    assert!(
        achieved_rate >= TARGET_SPANS_PER_SECOND as f64 * 0.9,
        "sustained rate {achieved_rate:.0} spans/s fell below 90% of the \
         {TARGET_SPANS_PER_SECOND} spans/s target"
    );

    // The queue must reach empty, not merely stop growing. A transport that
    // keeps up on average but never catches up is a queue that eventually hits
    // the cap and turns an outage into a gap.
    let stats = outbox.stats().await.expect("stats must be readable");
    assert_eq!(
        stats.pending_rows, 0,
        "the queue must drain to empty, not merely keep pace"
    );
    assert_eq!(
        harness.stub.received_spans.load(Ordering::SeqCst),
        total_spans,
        "Cloud must receive exactly what was enqueued — no loss, no duplication"
    );
    assert_eq!(
        stats.dead_letter_rows, 0,
        "nothing may dead-letter while Cloud is healthy"
    );
    assert!(
        harness.stub.largest_batch.load(Ordering::SeqCst) <= OUTBOX_BATCH_SIZE as u64,
        "a wire batch must never exceed the configured batch size"
    );
    assert!(
        harness.gap_windows().await.is_empty(),
        "no gap may be recorded while Cloud is up"
    );
}

// ── 2. Cloud down: zero loss until the cap, then exact accounting ──────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_cloud_down_nothing_is_lost_until_the_byte_cap() {
    let Some(harness) = Harness::start().await else {
        return;
    };
    harness.stub.down.store(true, Ordering::SeqCst);

    // A cap large enough to hold everything this phase produces, so any loss
    // here would be a bug rather than the policy.
    let outbox = harness.outbox(64 * 1024 * 1024);

    let mut produced = 0usize;
    for i in 0..40 {
        let outcome = outbox
            .enqueue(TEST_PROJECT, &batch(i * PRODUCER_BATCH, PRODUCER_BATCH))
            .await
            .expect("enqueue must not fail while Cloud is down");
        assert_eq!(
            outcome.refused, 0,
            "below the cap, a Cloud outage must cost nothing"
        );
        produced += outcome.accepted;
    }

    // Attempting to ship while Cloud is down must keep every row.
    let outcome = drain_until_idle(&harness.link, &outbox, 5).await;
    assert!(
        matches!(outcome, temps_cloud_client::DrainOutcome::Failed { .. }),
        "a failed shipment must be reported as failed, not as progress: {outcome:?}"
    );

    let stats = outbox.stats().await.expect("stats must be readable");
    assert_eq!(
        stats.pending_rows, produced as i64,
        "every span must still be queued after a failed shipment"
    );
    assert_eq!(
        outbox.dropped_spans(),
        0,
        "nothing may be dropped below the cap"
    );
    assert!(
        harness.gap_windows().await.is_empty(),
        "no gap window may exist while nothing has been dropped"
    );

    // The oldest-unshipped age has to be a real number the operator can watch,
    // not `None` on a non-empty queue.
    assert!(
        stats.oldest_pending_age_secs.is_some(),
        "a non-empty queue must report the age of its oldest span"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn at_the_cap_drops_are_accounted_exactly_and_a_gap_window_is_recorded() {
    let Some(harness) = Harness::start().await else {
        return;
    };
    harness.stub.down.store(true, Ordering::SeqCst);

    // Deliberately small so the cap is reached in a few batches and the
    // arithmetic is checkable by hand.
    let cap_bytes = 64 * 1024;
    let outbox = harness.outbox(cap_bytes);

    let before = chrono::Utc::now();
    let mut accepted = 0usize;
    let mut refused = 0usize;
    for i in 0..40 {
        let outcome = outbox
            .enqueue(TEST_PROJECT, &batch(i * PRODUCER_BATCH, PRODUCER_BATCH))
            .await
            .expect("enqueue must answer even at the cap");
        accepted += outcome.accepted;
        refused += outcome.refused;
    }
    let after = chrono::Utc::now();

    assert!(accepted > 0, "the queue must accept up to its cap");
    assert!(refused > 0, "the cap must actually bind in this test");
    assert_eq!(
        accepted + refused,
        40 * PRODUCER_BATCH,
        "every span must be accounted for as accepted or refused — never neither"
    );
    assert_eq!(
        outbox.dropped_spans(),
        refused as u64,
        "the lifetime drop counter must match what was refused, exactly"
    );

    let rows = harness.row_count("pending").await;
    assert_eq!(
        rows, accepted as i64,
        "the durable queue must hold exactly what was accepted"
    );
    let stats = outbox.stats().await.expect("stats must be readable");
    assert!(
        stats.pending_bytes <= cap_bytes as i64,
        "the queue must never exceed its byte cap: {} > {cap_bytes}",
        stats.pending_bytes
    );

    // The gap: one contiguous window, correctly bounded, with an exact count.
    let gaps = harness.gap_windows().await;
    assert_eq!(
        gaps.len(),
        1,
        "a continuous outage must coalesce into ONE gap window, not one per batch"
    );
    let (started_at, ended_at, dropped) = gaps[0];
    assert_eq!(
        dropped, refused as i64,
        "the gap must count every refused span"
    );
    assert!(
        started_at >= before && started_at <= after,
        "the gap must start when the first span was refused"
    );
    assert!(
        ended_at >= started_at && ended_at <= after,
        "the gap must end when the last span was refused, and never before it started"
    );
}

// ── 3. Restart mid-outage ──────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn everything_enqueued_before_a_restart_still_ships_after_it() {
    // The property the existing in-memory spool does NOT have, and the whole
    // reason Phase B1 exists.
    let Some(harness) = Harness::start().await else {
        return;
    };
    harness.stub.down.store(true, Ordering::SeqCst);

    let queued = {
        // "Before the restart": one `SpanOutbox` value, dropped at the end of
        // this block along with every byte of in-process state it held.
        let outbox = harness.outbox(64 * 1024 * 1024);
        let mut queued = 0usize;
        for i in 0..20 {
            queued += outbox
                .enqueue(TEST_PROJECT, &batch(i * PRODUCER_BATCH, PRODUCER_BATCH))
                .await
                .expect("enqueue must succeed")
                .accepted;
        }
        // A failed attempt before the "crash", so the rows carry an attempt
        // count too — a restart must not lose that either.
        drain_until_idle(&harness.link, &outbox, 2).await;
        queued
    };

    assert!(queued > 0);

    // "After the restart": a brand-new `SpanOutbox`, with zeroed counters and
    // no memory of what came before. Everything it knows comes off disk.
    let restarted = harness.outbox(64 * 1024 * 1024);
    let stats = restarted
        .resync()
        .await
        .expect("a restarted outbox must be able to read its own queue");
    assert_eq!(
        stats.pending_rows, queued as i64,
        "a restart must not lose a single queued span"
    );
    assert!(
        stats.pending_bytes > 0,
        "the restarted outbox must recover its byte accounting from disk, not from zero"
    );

    // Cloud comes back; everything ships.
    harness.stub.down.store(false, Ordering::SeqCst);
    for _ in 0..50 {
        if matches!(
            drain_until_idle(&harness.link, &restarted, 40).await,
            temps_cloud_client::DrainOutcome::Idle
        ) {
            break;
        }
    }

    assert_eq!(
        harness.stub.received_spans.load(Ordering::SeqCst),
        queued,
        "every span enqueued before the restart must ship after it"
    );
    assert_eq!(
        restarted.stats().await.expect("stats").pending_rows,
        0,
        "the queue must be empty once everything has shipped"
    );
}

// ── 4. Producer handoff: no whole-batch drops ──────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_producer_handoff_never_drops_a_whole_batch() {
    // The existing mirror's producer handoff is an 8-slot `try_send` channel
    // that discards *whole batches* when full, drained only by a flush or a
    // status read. For a mirror that is acceptable degradation; on a primary
    // path it is unaccounted loss at the very first hop. The outbox has no such
    // channel — the enqueue is a durable write on the ingest path — and this
    // asserts it by hammering it from many concurrent producers with nothing
    // draining at all.
    let Some(harness) = Harness::start().await else {
        return;
    };
    let outbox = harness.outbox(temps_core::DEFAULT_CLOUD_TELEMETRY_OUTBOX_MAX_BYTES);

    const PRODUCERS: usize = 16;
    const BATCHES_EACH: usize = 20;

    let mut tasks = Vec::new();
    for producer in 0..PRODUCERS {
        let outbox = outbox.clone();
        tasks.push(tokio::spawn(async move {
            let mut accepted = 0usize;
            for b in 0..BATCHES_EACH {
                let start = producer * 100_000 + b * PRODUCER_BATCH;
                let outcome = outbox
                    .enqueue(TEST_PROJECT, &batch(start, PRODUCER_BATCH))
                    .await
                    .expect("a concurrent enqueue must not fail");
                assert_eq!(
                    outcome.refused, 0,
                    "no batch may be shed at the producer handoff"
                );
                accepted += outcome.accepted;
            }
            accepted
        }));
    }

    let mut total = 0usize;
    for task in tasks {
        total += task.await.expect("producer must not panic");
    }

    let expected = PRODUCERS * BATCHES_EACH * PRODUCER_BATCH;
    assert_eq!(total, expected, "every offered span must be accepted");
    assert_eq!(
        harness.row_count("pending").await,
        expected as i64,
        "every accepted span must be durable, with nothing lost between \
         producers"
    );
    assert_eq!(
        outbox.dropped_spans(),
        0,
        "the producer handoff must drop nothing at all"
    );
}

// ── 5. Bounded memory: the queue's cost is disk, not RAM ───────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deep_queue_costs_disk_and_not_memory() {
    let Some(harness) = Harness::start().await else {
        return;
    };
    harness.stub.down.store(true, Ordering::SeqCst);
    let outbox = harness.outbox(temps_core::DEFAULT_CLOUD_TELEMETRY_OUTBOX_MAX_BYTES);

    // Build a queue several times deeper than one wire batch.
    let deep = OUTBOX_BATCH_SIZE as usize * 8;
    let mut queued = 0usize;
    while queued < deep {
        queued += outbox
            .enqueue(TEST_PROJECT, &batch(queued, PRODUCER_BATCH))
            .await
            .expect("enqueue must succeed")
            .accepted;
    }

    let stats = outbox.stats().await.expect("stats must be readable");
    assert_eq!(stats.pending_rows, queued as i64);
    assert!(
        stats.pending_bytes > 0,
        "the queue's size must be measurable in bytes on disk"
    );

    // The worker only ever materialises one batch at a time, however deep the
    // queue is. This is the property the in-memory spool cannot have at any
    // size: there, a deep backlog *is* resident memory.
    let claimed = outbox
        .claim(OUTBOX_BATCH_SIZE)
        .await
        .expect("claim must succeed");
    assert!(
        claimed.len() <= OUTBOX_BATCH_SIZE as usize,
        "a claim must never materialise more than one batch: {} rows",
        claimed.len()
    );
    assert!(
        claimed.len() < queued,
        "a claim must be a bounded slice of the queue, not the whole thing \
         ({} of {queued})",
        claimed.len()
    );

    // And the rest is still on disk, untouched.
    assert_eq!(
        harness.row_count("pending").await,
        queued as i64,
        "claiming must not remove rows — only an acknowledgement may"
    );

    // Ordering is FIFO: a primary path must not reorder a customer's telemetry.
    let claimed_ids: Vec<i64> = claimed.iter().map(|row| row.id).collect();
    let mut sorted = claimed_ids.clone();
    sorted.sort_unstable();
    assert_eq!(
        claimed_ids, sorted,
        "claims must come back in enqueue order"
    );

    harness.reset_queue().await;
}
