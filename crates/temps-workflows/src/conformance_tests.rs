// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The executor protocol conformance suite against the real engine.
//!
//! The suite lives with the TypeScript SDK (`sdks/node/packages/workflows`,
//! `src/conformance/suite.ts`) and runs there against an in-memory fake. This
//! test runs the same cases against the real engine: a real database, the
//! real store, engine and scheduler, and the real public routes (executor
//! callbacks and the app API) on a loopback port. The SDK's script
//! (`scripts/real-engine-conformance.ts`) serves the fixture workflows and
//! drives the cases, using a test-only control API mounted here to dispatch
//! due runs, let time pass and expire a lease.
//!
//! Skips, with the reason, when Docker (the database) or `bun` is missing.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use serde::Deserialize;
use temps_core::{
    AuditLogger, AuditOperation, AuditResult, EncryptionService, ExecutorEndpoint,
    ExecutorEndpointSet, ExecutorEndpoints, ExecutorEndpointsError, NoopProjectMetricsSink,
};
use uuid::Uuid;

use crate::engine::WorkflowEngine;
use crate::handlers::{configure_public_routes, WorkflowsAppState};
use crate::metrics::WorkflowMetrics;
use crate::payload::PayloadCodec;
use crate::protocol::{InvocationKeyResponse, JournalEntry};
use crate::scheduler::{SchedulerConfig, WorkflowScheduler};
use crate::service::WorkflowService;
use crate::store::{PgWorkflowStore, WorkflowStore};
use crate::test_support::test_environment;
use crate::token::InvocationSigner;

const SDK_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../sdks/node/packages/workflows"
);

struct Executor(String);

#[async_trait]
impl ExecutorEndpoints for Executor {
    async fn endpoints(
        &self,
        _project_id: i32,
        _environment_id: i32,
        _service: Option<&str>,
    ) -> Result<ExecutorEndpointSet, ExecutorEndpointsError> {
        Ok(ExecutorEndpointSet {
            deployment_id: Some(1),
            endpoints: vec![ExecutorEndpoint {
                base_url: self.0.clone(),
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

#[derive(Clone)]
struct Control {
    scheduler: Arc<WorkflowScheduler>,
    store: Arc<PgWorkflowStore>,
    engine: Arc<WorkflowEngine>,
    conn: Arc<DatabaseConnection>,
    environment_id: i32,
}

impl Control {
    /// Dispatch until nothing is due and nothing is in flight.
    async fn settle(&self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let mut quiet = 0;
        while tokio::time::Instant::now() < deadline {
            let started = self.scheduler.dispatch().await;
            if started == 0 && self.scheduler.is_idle() {
                quiet += 1;
                if quiet >= 3 {
                    return;
                }
            } else {
                quiet = 0;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("the engine did not settle within 30 s");
    }

    async fn execute(&self, sql: &str, values: Vec<sea_orm::Value>) {
        self.conn
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                values,
            ))
            .await
            .unwrap();
    }
}

async fn kick(State(control): State<Control>) -> Json<usize> {
    Json(control.scheduler.dispatch().await)
}

async fn settle(State(control): State<Control>) -> Json<bool> {
    control.settle().await;
    Json(true)
}

#[derive(Deserialize)]
struct AdvanceQuery {
    ms: i64,
}

/// Let `ms` pass for this environment's timers: due times, sleeps' wake
/// times and waits' timeouts move earlier by `ms`.
async fn advance(State(control): State<Control>, Query(query): Query<AdvanceQuery>) -> Json<bool> {
    let shift = format!("{} milliseconds", query.ms);
    control
        .execute(
            "UPDATE workflow_runs SET due_at = due_at - $2::interval \
             WHERE environment_id = $1 AND due_at IS NOT NULL \
             AND status IN ('queued', 'sleeping', 'waiting', 'retrying')",
            vec![control.environment_id.into(), shift.clone().into()],
        )
        .await;
    control
        .execute(
            "UPDATE workflow_steps s SET wake_at = s.wake_at - $2::interval \
             FROM workflow_runs r WHERE s.run_id = r.id AND r.environment_id = $1 \
             AND s.status = 'pending' AND s.wake_at IS NOT NULL",
            vec![control.environment_id.into(), shift.into()],
        )
        .await;
    control.settle().await;
    Json(true)
}

/// The lease of a running run expires, as when its replica dies.
async fn expire_lease(State(control): State<Control>, Path(run_id): Path<Uuid>) -> Json<bool> {
    control
        .execute(
            "UPDATE workflow_runs SET status = 'queued', due_at = now(), lease_expires_at = NULL \
             WHERE id = $1 AND status = 'running'",
            vec![run_id.into()],
        )
        .await;
    Json(true)
}

async fn journal(
    State(control): State<Control>,
    Path(run_id): Path<Uuid>,
) -> Json<Vec<JournalEntry>> {
    let run = control.store.get_run(run_id).await.unwrap().unwrap();
    let settings = control.store.settings(run.environment_id).await.unwrap();
    let (entries, _) = control
        .engine
        .journal_entries(&run, 0, &settings)
        .await
        .unwrap();
    Json(entries)
}

fn bun_available() -> bool {
    std::process::Command::new("bun")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_sdk_conforms_against_the_real_engine() {
    if !bun_available() {
        println!("bun is not installed, skipping the real-engine conformance suite");
        return;
    }
    let Some(env) = test_environment("real-engine conformance").await else {
        return;
    };
    let executor_port = free_port();
    let encryption = Arc::new(EncryptionService::new("12345678901234567890123456789012").unwrap());
    let store = Arc::new(PgWorkflowStore::new(env.conn.clone()));
    let codec = Arc::new(PayloadCodec::new(encryption.clone(), None));
    let signer = Arc::new(InvocationSigner::new(&encryption));
    let metrics = Arc::new(WorkflowMetrics::new(Arc::new(NoopProjectMetricsSink)));
    let engine = Arc::new(WorkflowEngine::new(
        store.clone(),
        codec.clone(),
        signer.clone(),
        Arc::new(Executor(format!("http://127.0.0.1:{executor_port}"))),
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
    let credential = service
        .rotate_credential(env.project_id, env.environment_id, Some(env.user_id))
        .await
        .unwrap();

    let state = Arc::new(WorkflowsAppState {
        service,
        engine: engine.clone(),
        invocation_key: InvocationKeyResponse {
            algorithm: "ed25519".into(),
            public_key: signer.public_key(),
            key_id: signer.key_id(),
        },
        audit_service: Arc::new(NoAudit),
        project_access_checker: None,
    });
    let control = Control {
        scheduler,
        store,
        engine,
        conn: env.conn.clone(),
        environment_id: env.environment_id,
    };
    let app = Router::new()
        .nest("/api", configure_public_routes().with_state(state))
        .nest(
            "/__test",
            Router::new()
                .route("/kick", post(kick))
                .route("/settle", post(settle))
                .route("/advance", post(advance))
                .route("/runs/{run_id}/expire-lease", post(expire_lease))
                .route("/runs/{run_id}/journal", get(journal))
                .with_state(control),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let child = tokio::process::Command::new("bun")
        .args(["run", "scripts/real-engine-conformance.ts"])
        .current_dir(SDK_DIR)
        .env("TEMPS_API_URL", format!("{api}/api"))
        .env("TEMPS_WORKFLOWS_TOKEN", &credential.token)
        .env("TEMPS_TEST_CONTROL_URL", format!("{api}/__test"))
        .env("EXECUTOR_PORT", executor_port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("bun starts");
    let output = tokio::time::timeout(Duration::from_secs(240), child.wait_with_output())
        .await
        .expect("the conformance suite finishes within 4 minutes")
        .unwrap();
    server.abort();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("{stdout}");
    assert!(
        output.status.success(),
        "the conformance suite failed against the real engine:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains(" failed=0"),
        "unexpected suite summary:\n{stdout}"
    );
}
