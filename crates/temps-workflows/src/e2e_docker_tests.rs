// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! End to end: the real engine drives a worker in a container.
//!
//! The worker is the SDK's `scripts/e2e-worker.ts`, bundled for Node and run
//! in a `node` container. It reaches the engine's public API on the host
//! (`host.docker.internal`), and the engine invokes it on its published port.
//! Each step body counts its executions on the host, so the counts survive the
//! container. The test covers:
//!
//! - a replica killed in the middle of a step: a second replica takes over and
//!   the steps recorded before the kill do not run again;
//! - a sleep, woken early by the operator's resume;
//! - a wait for an event, delivered by key;
//! - a retry from a step, re-running only that step.
//!
//! Skips, with the reason, when Docker or `bun` is missing.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use serde_json::json;
use temps_core::{
    AuditLogger, AuditOperation, AuditResult, EncryptionService, ExecutorEndpoint,
    ExecutorEndpointSet, ExecutorEndpoints, ExecutorEndpointsError, NoopProjectMetricsSink,
};
use testcontainers::core::{ContainerPort, Host, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use uuid::Uuid;

use crate::engine::WorkflowEngine;
use crate::handlers::{configure_public_routes, WorkflowsAppState};
use crate::metrics::WorkflowMetrics;
use crate::payload::PayloadCodec;
use crate::protocol::JournalEntry;
use crate::scheduler::{SchedulerConfig, WorkflowScheduler};
use crate::service::{StartRun, WorkflowService};
use crate::store::{PgWorkflowStore, RunRow, Starter, WorkflowStore};
use crate::test_support::{make_due, test_environment, TestEnvironment};
use crate::token::InvocationSigner;

const SDK_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../sdks/node/packages/workflows"
);
const NODE_IMAGE: &str = "node";
const NODE_TAG: &str = "24-bookworm-slim";
const WORKER_PORT: u16 = 8080;

/// The replica the engine invokes; the test moves it to the second container.
struct Replica(Mutex<String>);

#[async_trait]
impl ExecutorEndpoints for Replica {
    async fn endpoints(
        &self,
        _project_id: i32,
        _environment_id: i32,
        _service: Option<&str>,
    ) -> Result<ExecutorEndpointSet, ExecutorEndpointsError> {
        Ok(ExecutorEndpointSet {
            deployment_id: Some(1),
            endpoints: vec![ExecutorEndpoint {
                base_url: self.0.lock().unwrap().clone(),
                container_id: 1,
                node_id: None,
            }],
            unavailable_reason: None,
        })
    }
}

struct NoAudit;

#[async_trait]
impl AuditLogger for NoAudit {
    async fn create_audit_log(&self, _operation: &dyn AuditOperation) -> AuditResult<()> {
        Ok(())
    }
}

/// What the worker reports to the host: step executions and the gate.
#[derive(Clone, Default)]
struct Probe {
    effects: Arc<Mutex<HashMap<String, u64>>>,
    gate: Arc<AtomicBool>,
}

impl Probe {
    fn count(&self, step: &str) -> u64 {
        self.effects.lock().unwrap().get(step).copied().unwrap_or(0)
    }
}

async fn record_effect(State(probe): State<Probe>, Path(step): Path<String>) -> Json<u64> {
    let mut effects = probe.effects.lock().unwrap();
    let count = effects.entry(step).or_insert(0);
    *count += 1;
    Json(*count)
}

async fn gate(State(probe): State<Probe>) -> Json<bool> {
    Json(probe.gate.load(Ordering::SeqCst))
}

fn bun_available() -> bool {
    std::process::Command::new("bun")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Bundle the worker for Node: Node cannot load the SDK's TypeScript sources.
async fn bundle_worker(out_dir: &std::path::Path) -> Vec<u8> {
    let outfile = out_dir.join("worker.mjs");
    let output = tokio::process::Command::new("bun")
        .args([
            "build",
            "scripts/e2e-worker.ts",
            "--target=node",
            "--external",
            "@opentelemetry/api",
            "--outfile",
        ])
        .arg(&outfile)
        .current_dir(SDK_DIR)
        .output()
        .await
        .expect("bun runs");
    assert!(
        output.status.success(),
        "bundling the e2e worker failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::read(&outfile).expect("the bundled worker")
}

struct Harness {
    env: TestEnvironment,
    service: Arc<WorkflowService>,
    engine: Arc<WorkflowEngine>,
    store: Arc<PgWorkflowStore>,
    scheduler: Arc<WorkflowScheduler>,
    replica: Arc<Replica>,
    probe: Probe,
    api_port: u16,
    worker: Vec<u8>,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Harness {
    async fn run(&self, run_id: Uuid) -> RunRow {
        self.store.get_run(run_id).await.unwrap().unwrap()
    }

    async fn journal(&self, run: &RunRow) -> Vec<JournalEntry> {
        let settings = self.store.settings(run.environment_id).await.unwrap();
        self.engine
            .journal_entries(run, 0, &settings)
            .await
            .unwrap()
            .0
    }

    /// Dispatch until the run reaches `status`, failing after 60 s.
    async fn drive_until(&self, run_id: Uuid, status: &str) -> RunRow {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            self.scheduler.dispatch().await;
            let run = self.run(run_id).await;
            if run.status == status {
                return run;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "run {run_id} did not reach {status} within 60 s; it is {} ({:?})",
                run.status,
                run.blocked_reason
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Start a worker container and point the engine at it.
    async fn start_worker(&self, credential: &str, name: &str) -> ContainerAsync<GenericImage> {
        let container = GenericImage::new(NODE_IMAGE, NODE_TAG)
            .with_exposed_port(ContainerPort::Tcp(WORKER_PORT))
            .with_wait_for(WaitFor::message_on_stdout("e2e worker listening"))
            .with_container_name(name)
            .with_host("host.docker.internal", Host::HostGateway)
            .with_copy_to("/worker/worker.mjs", self.worker.clone())
            .with_env_var(
                "TEMPS_API_URL",
                format!("http://host.docker.internal:{}/api", self.api_port),
            )
            .with_env_var(
                "E2E_CONTROL_URL",
                format!("http://host.docker.internal:{}/__e2e", self.api_port),
            )
            .with_env_var("TEMPS_WORKFLOWS_TOKEN", credential)
            .with_env_var("PORT", WORKER_PORT.to_string())
            .with_cmd(["node", "/worker/worker.mjs"])
            .with_startup_timeout(Duration::from_secs(120))
            .start()
            .await
            .expect("the worker container starts");
        let port = container
            .get_host_port_ipv4(ContainerPort::Tcp(WORKER_PORT))
            .await
            .expect("the worker's published port");
        *self.replica.0.lock().unwrap() = format!("http://127.0.0.1:{port}");
        // A new replica serves the workflows: read its manifest afresh.
        self.engine.invalidate_manifest(self.env.environment_id);
        container
    }
}

async fn harness(test: &str) -> Option<Harness> {
    if !bun_available() {
        println!("bun is not installed, skipping {test}");
        return None;
    }
    let env = test_environment(test).await?;
    let bundle_dir = tempfile_dir();
    let worker = bundle_worker(&bundle_dir).await;
    let _ = std::fs::remove_dir_all(&bundle_dir);

    let encryption = Arc::new(EncryptionService::new("12345678901234567890123456789012").unwrap());
    let store = Arc::new(PgWorkflowStore::new(env.conn.clone()));
    let codec = Arc::new(PayloadCodec::new(encryption.clone(), None));
    let signer = Arc::new(InvocationSigner::new(&encryption));
    let metrics = Arc::new(WorkflowMetrics::new(Arc::new(NoopProjectMetricsSink)));
    let replica = Arc::new(Replica(Mutex::new(String::new())));
    let engine = Arc::new(WorkflowEngine::new(
        store.clone(),
        codec.clone(),
        signer.clone(),
        replica.clone(),
        crate::executor::ExecutorClient::new().unwrap(),
        metrics.clone(),
    ));
    let service = Arc::new(WorkflowService::new(
        store.clone(),
        engine.clone(),
        codec.clone(),
        metrics.clone(),
    ));
    let scheduler = Arc::new(WorkflowScheduler::new(
        engine.clone(),
        store.clone() as Arc<dyn WorkflowStore>,
        codec,
        metrics,
        None,
        SchedulerConfig::default(),
    ));
    let state = Arc::new(WorkflowsAppState {
        service: service.clone(),
        engine: engine.clone(),
        invocation_key: crate::protocol::InvocationKey::of(&signer),
        audit_service: Arc::new(NoAudit),
        project_access_checker: None,
    });
    let probe = Probe::default();
    let app = Router::new()
        .nest("/api", configure_public_routes().with_state(state))
        .nest(
            "/__e2e",
            Router::new()
                .route("/effects/{step}", post(record_effect))
                .route("/gate", get(gate))
                .with_state(probe.clone()),
        );
    // Every interface: the container reaches it through the Docker host gateway.
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let api_port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    Some(Harness {
        env,
        service,
        engine,
        store,
        scheduler,
        replica,
        probe,
        api_port,
        worker,
        server,
    })
}

fn tempfile_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("temps-workflows-e2e-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn shape(journal: &[JournalEntry]) -> Vec<(i32, String, String)> {
    journal
        .iter()
        .map(|entry| {
            (
                entry.sequence,
                entry.name.clone(),
                entry.status.as_str().to_string(),
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_run_survives_a_killed_replica_sleeps_waits_and_retries_from_a_step() {
    let Some(h) = harness("the workflows e2e test").await else {
        return;
    };
    let (pid, eid) = (h.env.project_id, h.env.environment_id);
    let credential = h
        .service
        .rotate_credential(pid, eid, Some(h.env.user_id))
        .await
        .unwrap();
    let suffix = Uuid::new_v4().simple().to_string();

    // ── A replica dies in the middle of `charge` ──────────────────────
    let first = h
        .start_worker(&credential.token, &format!("temps-wf-e2e-a-{suffix}"))
        .await;
    let (run, created) = h
        .service
        .start(
            pid,
            eid,
            StartRun {
                workflow: "e2e-order".into(),
                input: Some(json!({ "order": "order-1" })),
                idempotency_key: None,
            },
            Starter::User(h.env.user_id),
        )
        .await
        .unwrap();
    assert!(created);
    let run_id = run.id;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while h.probe.count("charge") == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the first replica never entered charge; the run is {}",
            h.run(run_id).await.status
        );
        h.scheduler.dispatch().await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    first.stop_with_timeout(Some(0)).await.unwrap();
    first.rm().await.unwrap();

    // The invocation fails with the replica; if the engine had already been
    // answered 202, the lease expiry would hand the run back instead.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while h.run(run_id).await.status == "running" && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if h.run(run_id).await.status == "running" {
        h.env
            .conn
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE workflow_runs SET lease_expires_at = now() - interval '1 second' WHERE id = $1",
                vec![run_id.into()],
            ))
            .await
            .unwrap();
        assert_eq!(h.scheduler.reap().await, 1);
    }
    let handed_back = h.run(run_id).await;
    assert!(
        matches!(handed_back.status.as_str(), "retrying" | "queued"),
        "the run was not handed back after the kill: {}",
        handed_back.status
    );

    // ── A second replica takes over ───────────────────────────────────
    h.probe.gate.store(true, Ordering::SeqCst);
    let second = h
        .start_worker(&credential.token, &format!("temps-wf-e2e-b-{suffix}"))
        .await;
    make_due(&h.env.conn, run_id).await;
    let sleeping = h.drive_until(run_id, "sleeping").await;
    assert_eq!(
        h.probe.count("reserve"),
        1,
        "a recorded step ran again after the kill"
    );
    assert_eq!(
        h.probe.count("charge"),
        2,
        "the step in flight at the kill runs once more"
    );
    let journal = h.journal(&sleeping).await;
    assert_eq!(
        shape(&journal),
        vec![
            (0, "reserve".into(), "completed".into()),
            (1, "charge".into(), "completed".into()),
            (2, "cool-off".into(), "pending".into()),
        ]
    );
    assert_eq!(journal[0].result, Some(json!(1)));
    assert_eq!(
        journal[1].result,
        Some(json!(2)),
        "the second replica's charge is the one kept"
    );

    // ── The operator wakes the sleep; the run waits for its event ─────
    h.service.resume(pid, eid, run_id).await.unwrap();
    let waiting = h.drive_until(run_id, "waiting").await;
    assert_eq!(waiting.awaited_event_name.as_deref(), Some("order.shipped"));
    let (_, other) = h
        .service
        .send_event(pid, eid, "order.shipped", Some("order-2"), None, "api")
        .await
        .unwrap();
    assert_eq!(other.delivered, 0, "an event for another key woke the run");
    let (_, delivery) = h
        .service
        .send_event(
            pid,
            eid,
            "order.shipped",
            Some("order-1"),
            Some(&json!({ "carrier": "generic" })),
            "api",
        )
        .await
        .unwrap();
    assert_eq!(delivery.delivered, 1);
    h.drive_until(run_id, "completed").await;
    let (_, output, _) = h.service.run_result(pid, eid, run_id).await.unwrap();
    assert_eq!(
        output,
        Some(json!({
            "reservation": 1,
            "charge": 2,
            "shipped": { "received": true, "payload": { "carrier": "generic" } },
            "notified": 1,
        }))
    );

    // ── Retry from the last step: only it runs again ──────────────────
    let completed = h.run(run_id).await;
    let notify = h
        .journal(&completed)
        .await
        .iter()
        .find(|entry| entry.name == "notify")
        .map(|entry| entry.sequence)
        .expect("notify is journaled");
    h.service
        .retry_from(pid, eid, run_id, Some(notify))
        .await
        .unwrap();
    h.drive_until(run_id, "completed").await;
    assert_eq!(h.probe.count("notify"), 2);
    assert_eq!(h.probe.count("reserve"), 1);
    assert_eq!(h.probe.count("charge"), 2);
    let (_, output, _) = h.service.run_result(pid, eid, run_id).await.unwrap();
    assert_eq!(
        output.as_ref().and_then(|output| output.get("notified")),
        Some(&json!(2))
    );

    second.stop_with_timeout(Some(0)).await.unwrap();
    second.rm().await.unwrap();
    h.server.abort();
}
