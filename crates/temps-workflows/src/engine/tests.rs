// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The engine end to end against a real database and an in-process fake
//! executor that follows the protocol the way the SDK does: it replays the
//! journal it is sent, records each new entry through the engine, and replies
//! with an outcome. Each test returns early when no database is available.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use futures::future::BoxFuture;
use serde_json::{json, Value};
use temps_core::{
    EncryptionService, ExecutorEndpoint, ExecutorEndpointSet, ExecutorEndpoints,
    ExecutorEndpointsError, NoopProjectMetricsSink,
};
use uuid::Uuid;

use super::*;
use crate::error::WorkflowError;
use crate::metrics::WorkflowMetrics;
use crate::payload::{PayloadCodec, PayloadField};
use crate::protocol::{
    InvokeRequest, Outcome, OutcomeRequest, StepWriteRequest, INVOKE_PATH, MANIFEST_PATH,
};
use crate::store::{NewRun, PgWorkflowStore, StartOutcome, Starter, WorkflowStore};
use crate::test_support::{make_due, test_environment, TestEnvironment};
use crate::types::{RunStatus, StepKind, StepStatus, WorkflowSettings};

/// What the fake executor answers.
pub(crate) enum Reply {
    Outcome(Outcome),
    Status(u16),
}

pub(crate) type Script = Arc<
    dyn Fn(Arc<WorkflowEngine>, String, InvokeRequest) -> BoxFuture<'static, Reply> + Send + Sync,
>;

struct FakeEndpoints {
    base_url: Option<String>,
}

#[async_trait]
impl ExecutorEndpoints for FakeEndpoints {
    async fn endpoints(
        &self,
        _project_id: i32,
        _environment_id: i32,
        _service: Option<&str>,
    ) -> Result<ExecutorEndpointSet, ExecutorEndpointsError> {
        Ok(match &self.base_url {
            Some(base_url) => ExecutorEndpointSet {
                deployment_id: Some(1),
                endpoints: vec![ExecutorEndpoint {
                    base_url: base_url.clone(),
                    container_id: 1,
                    node_id: None,
                }],
                unavailable_reason: None,
            },
            None => ExecutorEndpointSet::unavailable(
                None,
                "No deployment is live in this environment yet",
            ),
        })
    }
}

pub(crate) struct Harness {
    pub env: TestEnvironment,
    pub engine: Arc<WorkflowEngine>,
    pub store: Arc<PgWorkflowStore>,
    pub codec: Arc<PayloadCodec>,
}

#[derive(Clone)]
struct FakeState {
    engine: Arc<WorkflowEngine>,
    workflows: Vec<String>,
    /// The concurrency every workflow of the manifest declares.
    concurrency: Option<u32>,
    script: Script,
}

async fn manifest(State(state): State<FakeState>) -> Json<Value> {
    Json(json!({
        "protocol_version": 1,
        "sdk": "fake",
        "workflows": state.workflows.iter().map(|name| json!({"name": name, "concurrency": state.concurrency})).collect::<Vec<_>>(),
    }))
}

async fn invoke(
    State(state): State<FakeState>,
    headers: HeaderMap,
    Json(request): Json<InvokeRequest>,
) -> Response {
    let token = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .trim_start_matches("Bearer ")
        .to_string();
    match (state.script)(state.engine.clone(), token, request).await {
        Reply::Outcome(outcome) => Json(OutcomeRequest { outcome }).into_response(),
        Reply::Status(status) => StatusCode::from_u16(status)
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
            .into_response(),
    }
}

pub(crate) async fn harness(
    test: &str,
    workflows: &[&str],
    script: Script,
    live: bool,
) -> Option<Harness> {
    harness_with(test, workflows, script, live, None, None).await
}

pub(crate) async fn harness_with(
    test: &str,
    workflows: &[&str],
    script: Script,
    live: bool,
    blobs: Option<Arc<dyn temps_core::PayloadBlobStore>>,
    concurrency: Option<u32>,
) -> Option<Harness> {
    let env = test_environment(test).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let encryption = Arc::new(EncryptionService::new("12345678901234567890123456789012").unwrap());
    let store = Arc::new(PgWorkflowStore::new(env.conn.clone()));
    let codec = Arc::new(PayloadCodec::new(encryption.clone(), blobs));
    let engine = Arc::new(WorkflowEngine::new(
        store.clone(),
        codec.clone(),
        Arc::new(crate::token::InvocationSigner::new(&encryption)),
        Arc::new(FakeEndpoints {
            base_url: live.then_some(base_url),
        }),
        crate::executor::ExecutorClient::new().unwrap(),
        Arc::new(WorkflowMetrics::new(Arc::new(NoopProjectMetricsSink))),
    ));
    let state = FakeState {
        engine: engine.clone(),
        workflows: workflows.iter().map(|name| name.to_string()).collect(),
        concurrency,
        script,
    };
    let router = Router::new()
        .route(MANIFEST_PATH, get(manifest))
        .route(INVOKE_PATH, post(invoke))
        .with_state(state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Some(Harness {
        env,
        engine,
        store,
        codec,
    })
}

impl Harness {
    pub async fn start(&self, workflow: &str, input: Value) -> Uuid {
        let id = Uuid::new_v4();
        let scope = crate::payload::PayloadScope {
            project_id: self.env.project_id,
            environment_id: self.env.environment_id,
            run_id: id,
        };
        let sealed = self
            .codec
            .seal(
                &scope,
                &PayloadField::RunInput,
                &input,
                0,
                &WorkflowSettings::default(),
            )
            .await
            .unwrap();
        let outcome = self
            .store
            .insert_run(
                NewRun {
                    id,
                    project_id: self.env.project_id,
                    environment_id: self.env.environment_id,
                    workflow_name: workflow.into(),
                    input: sealed,
                    idempotency_key: None,
                    deployment_id: None,
                    starter: Starter::Machine,
                },
                10_000,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, StartOutcome::Created(_)));
        id
    }

    /// Claim and invoke every due run once.
    async fn tick(&self) -> usize {
        let claims = self.engine.claim(10).await.unwrap();
        let count = claims.len();
        for claim in claims {
            self.engine.invoke(claim).await;
        }
        count
    }

    pub async fn run(&self, id: Uuid) -> crate::store::RunRow {
        self.store.get_run(id).await.unwrap().unwrap()
    }

    async fn output(&self, id: Uuid) -> Option<Value> {
        let run = self.run(id).await;
        self.codec
            .open(
                &WorkflowEngine::scope(&run),
                &PayloadField::RunOutput,
                &run_output(&run),
                &WorkflowSettings::default(),
            )
            .await
            .unwrap()
    }
}

fn write(sequence: i32, name: &str, kind: StepKind, status: StepStatus) -> StepWriteRequest {
    StepWriteRequest {
        sequence,
        name: name.into(),
        kind,
        status,
        attempts: 1,
        result: None,
        error: None,
        wake_at: None,
        duration_ms: Some(3),
        trace_id: None,
        error_ref: None,
        event: None,
    }
}

fn completed(sequence: i32, name: &str, result: Value) -> StepWriteRequest {
    StepWriteRequest {
        result: Some(result),
        ..write(sequence, name, StepKind::Step, StepStatus::Completed)
    }
}

#[tokio::test]
async fn a_crash_after_a_step_replays_it_from_the_journal_instead_of_repeating_it() {
    let charges = Arc::new(AtomicUsize::new(0));
    let first_token = Arc::new(Mutex::new(None::<String>));
    let script: Script = {
        let charges = charges.clone();
        let first_token = first_token.clone();
        Arc::new(move |engine, token, request| {
            let charges = charges.clone();
            let first_token = first_token.clone();
            Box::pin(async move {
                if request.attempt == 2 {
                    // The lost invocation's token no longer writes.
                    let old = first_token.lock().unwrap().clone().unwrap();
                    let error = engine
                        .record_step(&old, request.run_id, completed(1, "ship", json!(1)))
                        .await
                        .unwrap_err();
                    assert!(matches!(error, WorkflowError::StaleLease { .. }), "{error}");
                }
                let charge = match request.journal.first() {
                    Some(entry) => {
                        assert_eq!(entry.name, "charge");
                        entry.result.clone().unwrap()
                    }
                    None => {
                        charges.fetch_add(1, Ordering::SeqCst);
                        let amount = request.input.as_ref().unwrap()["amount"].clone();
                        let response = engine
                            .record_step(
                                &token,
                                request.run_id,
                                completed(0, "charge", json!({ "charged": amount })),
                            )
                            .await
                            .unwrap();
                        response.entry.result.unwrap()
                    }
                };
                if request.attempt == 1 {
                    *first_token.lock().unwrap() = Some(token);
                    // The process dies before finishing.
                    return Reply::Status(500);
                }
                engine
                    .record_step(
                        &token,
                        request.run_id,
                        completed(1, "ship", json!("shipped")),
                    )
                    .await
                    .unwrap();
                Reply::Outcome(Outcome::Completed {
                    output: Some(json!({ "charge": charge })),
                })
            })
        })
    };
    let Some(h) = harness("replay test", &["orders"], script, true).await else {
        return;
    };
    let id = h.start("orders", json!({ "amount": 42 })).await;
    assert_eq!(h.tick().await, 1);
    let after_crash = h.run(id).await;
    assert_eq!(after_crash.status, "retrying");
    assert!(after_crash
        .blocked_reason
        .as_deref()
        .unwrap()
        .contains("HTTP 500"));

    make_due(&h.env.conn, id).await;
    assert_eq!(h.tick().await, 1);
    let run = h.run(id).await;
    assert_eq!(run.status, "completed", "{:?}", run.blocked_reason);
    assert_eq!(run.attempt, 2);
    assert_eq!(
        charges.load(Ordering::SeqCst),
        1,
        "a completed step ran twice"
    );
    assert_eq!(
        h.output(id).await,
        Some(json!({ "charge": { "charged": 42 } }))
    );
    assert_eq!(h.store.journal(id, 0, 10).await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_sleeping_run_suspends_and_resume_wakes_it_early() {
    let script: Script = Arc::new(|engine, token, request| {
        Box::pin(async move {
            match request.journal.first() {
                None => {
                    let until = request.now + Duration::hours(1);
                    engine
                        .record_step(
                            &token,
                            request.run_id,
                            StepWriteRequest {
                                wake_at: Some(until),
                                ..write(0, "cool-down", StepKind::Sleep, StepStatus::Pending)
                            },
                        )
                        .await
                        .unwrap();
                    Reply::Outcome(Outcome::Sleep { sequence: 0, until })
                }
                Some(entry) if entry.wake_at.unwrap() > request.now => {
                    Reply::Outcome(Outcome::Sleep {
                        sequence: 0,
                        until: entry.wake_at.unwrap(),
                    })
                }
                Some(_) => {
                    engine
                        .record_step(
                            &token,
                            request.run_id,
                            write(0, "cool-down", StepKind::Sleep, StepStatus::Completed),
                        )
                        .await
                        .unwrap();
                    Reply::Outcome(Outcome::Completed { output: None })
                }
            }
        })
    });
    let Some(h) = harness("sleep test", &["nap"], script, true).await else {
        return;
    };
    let id = h.start("nap", json!(null)).await;
    h.tick().await;
    let sleeping = h.run(id).await;
    assert_eq!(sleeping.status, "sleeping");
    assert!(sleeping.due_at.unwrap() > Utc::now() + Duration::minutes(50));
    // Not due: nothing to claim.
    assert_eq!(h.tick().await, 0);

    h.store.resume_run(id).await.unwrap().unwrap();
    assert_eq!(h.tick().await, 1);
    assert_eq!(h.run(id).await.status, "completed");
}

fn waiting_script() -> Script {
    Arc::new(|engine, token, request| {
        Box::pin(async move {
            match request.journal.first() {
                None => {
                    engine
                        .record_step(
                            &token,
                            request.run_id,
                            write(0, "approval", StepKind::Wait, StepStatus::Pending),
                        )
                        .await
                        .unwrap();
                    Reply::Outcome(Outcome::Wait {
                        sequence: 0,
                        event: "approved".into(),
                        key: Some("order-1".into()),
                        timeout_at: None,
                    })
                }
                Some(entry) if entry.status == StepStatus::Completed => {
                    Reply::Outcome(Outcome::Completed {
                        output: entry.result.clone(),
                    })
                }
                Some(_) => Reply::Outcome(Outcome::Wait {
                    sequence: 0,
                    event: "approved".into(),
                    key: Some("order-1".into()),
                    timeout_at: None,
                }),
            }
        })
    })
}

#[tokio::test]
async fn an_event_wakes_its_waiting_run_with_the_payload() {
    let Some(h) = harness("wait test", &["approval"], waiting_script(), true).await else {
        return;
    };
    let id = h.start("approval", json!(null)).await;
    h.tick().await;
    assert_eq!(h.run(id).await.status, "waiting");

    let payload = json!({ "by": "ops" });
    // Another key does not wake it.
    let other = h
        .engine
        .store_event(EventSpec {
            project_id: h.env.project_id,
            environment_id: h.env.environment_id,
            name: "approved",
            key: "order-2",
            payload: Some(&payload),
            source: "api",
            source_run_id: None,
        })
        .await
        .unwrap();
    assert_eq!(h.engine.deliver_event(&other).await.unwrap().delivered, 0);

    let event = h
        .engine
        .store_event(EventSpec {
            project_id: h.env.project_id,
            environment_id: h.env.environment_id,
            name: "approved",
            key: "order-1",
            payload: Some(&payload),
            source: "api",
            source_run_id: None,
        })
        .await
        .unwrap();
    assert_eq!(h.engine.deliver_event(&event).await.unwrap().delivered, 1);
    // Delivering again is a no-op.
    assert_eq!(h.engine.deliver_event(&event).await.unwrap().delivered, 0);
    assert_eq!(h.tick().await, 1);
    assert_eq!(h.run(id).await.status, "completed");
    assert_eq!(
        h.output(id).await,
        Some(json!({ "received": true, "payload": { "by": "ops" } }))
    );
}

#[tokio::test]
async fn an_event_sent_before_the_run_suspends_is_picked_up_when_it_does() {
    let Some(h) = harness("buffered event test", &["approval"], waiting_script(), true).await
    else {
        return;
    };
    let id = h.start("approval", json!(null)).await;
    let event = h
        .engine
        .store_event(EventSpec {
            project_id: h.env.project_id,
            environment_id: h.env.environment_id,
            name: "approved",
            key: "order-1",
            payload: None,
            source: "api",
            source_run_id: None,
        })
        .await
        .unwrap();
    // Nobody waits yet.
    assert_eq!(h.engine.deliver_event(&event).await.unwrap().delivered, 0);
    h.tick().await;
    // It suspended and got the buffered event at once.
    assert_eq!(h.run(id).await.status, "queued");
    h.tick().await;
    assert_eq!(h.run(id).await.status, "completed");
}

#[tokio::test]
async fn nothing_to_invoke_blocks_the_run_without_spending_an_attempt() {
    let script: Script = Arc::new(|_, _, _| Box::pin(async { Reply::Status(500) }));
    let Some(h) = harness("blocked test", &["orders"], script, false).await else {
        return;
    };
    let id = h.start("orders", json!(null)).await;
    h.tick().await;
    let run = h.run(id).await;
    assert_eq!(run.status, "queued");
    assert_eq!(run.attempt, 0);
    assert_eq!(run.consecutive_failures, 0);
    assert_eq!(
        run.blocked_reason.as_deref(),
        Some("No deployment is live in this environment yet")
    );
}

#[tokio::test]
async fn a_workflow_the_deployment_does_not_define_fails_the_invocation_with_why() {
    let script: Script = Arc::new(|_, _, _| Box::pin(async { Reply::Status(500) }));
    let Some(h) = harness("not served test", &["billing"], script, true).await else {
        return;
    };
    let id = h.start("orders", json!(null)).await;
    h.tick().await;
    let run = h.run(id).await;
    assert_eq!(run.status, "retrying");
    assert_eq!(run.consecutive_failures, 1);
    let reason = run.blocked_reason.unwrap();
    assert!(
        reason.contains("does not define workflow 'orders'") && reason.contains("billing"),
        "{reason}"
    );
}

#[tokio::test]
async fn an_incompatible_replay_fails_the_run_with_the_typed_error() {
    let script: Script = Arc::new(|engine, token, request| {
        Box::pin(async move {
            if request.journal.is_empty() {
                engine
                    .record_step(&token, request.run_id, completed(0, "charge", json!(1)))
                    .await
                    .unwrap();
                return Reply::Status(503);
            }
            // The new code calls another step first.
            let error = engine
                .record_step(&token, request.run_id, completed(0, "refund", json!(1)))
                .await
                .unwrap_err();
            let WorkflowError::Incompatible {
                sequence,
                expected,
                found,
                ..
            } = error
            else {
                panic!("expected an incompatible replay, got {error}");
            };
            Reply::Outcome(Outcome::Incompatible {
                sequence,
                expected,
                found,
            })
        })
    });
    let Some(h) = harness("incompatible test", &["orders"], script, true).await else {
        return;
    };
    let id = h.start("orders", json!(null)).await;
    h.tick().await;
    make_due(&h.env.conn, id).await;
    h.tick().await;
    let run = h.run(id).await;
    assert_eq!(run.status, "failed");
    assert_eq!(run.error_kind.as_deref(), Some("incompatible"));
    let message = h
        .codec
        .open(
            &WorkflowEngine::scope(&run),
            &PayloadField::RunError,
            &run_error(&run),
            &WorkflowSettings::default(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(message
        .as_str()
        .unwrap()
        .starts_with("WorkflowIncompatible { run_id: "));
}

#[tokio::test]
async fn a_cancelled_run_tells_its_heartbeat_to_stop() {
    let cancelled = Arc::new(Mutex::new(None::<bool>));
    let script: Script = {
        let cancelled = cancelled.clone();
        Arc::new(move |engine, token, request| {
            let cancelled = cancelled.clone();
            Box::pin(async move {
                let beat = engine.heartbeat(&token, request.run_id).await.unwrap();
                assert!(!beat.cancelled);
                engine.store.cancel_run(request.run_id).await.unwrap();
                let beat = engine.heartbeat(&token, request.run_id).await.unwrap();
                *cancelled.lock().unwrap() = Some(beat.cancelled);
                Reply::Outcome(Outcome::Completed { output: None })
            })
        })
    };
    let Some(h) = harness("heartbeat test", &["orders"], script, true).await else {
        return;
    };
    let id = h.start("orders", json!(null)).await;
    h.tick().await;
    assert_eq!(*cancelled.lock().unwrap(), Some(true));
    // The late outcome did not overwrite the cancellation.
    assert_eq!(h.run(id).await.status, RunStatus::Cancelled.as_str());
}

/// Collects everything logged while it is the default subscriber.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn payloads_never_reach_the_logs() {
    const SECRET_INPUT: &str = "input-secret-7f3a9c";
    const SECRET_STEP: &str = "step-secret-2b81d4";
    const SECRET_OUTPUT: &str = "output-secret-c55e10";
    const SECRET_ERROR: &str = "error-secret-91aa3f";
    let script: Script = Arc::new(|engine, token, request| {
        Box::pin(async move {
            if request.attempt == 1 {
                let failed = StepWriteRequest {
                    error: Some(SECRET_ERROR.into()),
                    ..write(0, "charge", StepKind::Step, StepStatus::Pending)
                };
                engine
                    .record_step(&token, request.run_id, failed)
                    .await
                    .unwrap();
                return Reply::Outcome(Outcome::Retry {
                    sequence: 0,
                    at: request.now,
                });
            }
            engine
                .record_step(
                    &token,
                    request.run_id,
                    completed(0, "charge", json!(SECRET_STEP)),
                )
                .await
                .unwrap();
            Reply::Outcome(Outcome::Completed {
                output: Some(json!(SECRET_OUTPUT)),
            })
        })
    });
    let Some(h) = harness("log test", &["orders"], script, true).await else {
        return;
    };
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    // With a single registered dispatcher, tracing-core caches a callsite's
    // interest from whichever thread hits it first, so a callsite first hit
    // by a parallel test (with no subscriber) would stay disabled here. A
    // second live dispatcher makes interest consult every dispatcher.
    let _second = tracing::Dispatch::new(tracing_subscriber::registry());
    tracing::callsite::rebuild_interest_cache();

    let id = h.start("orders", json!({ "card": SECRET_INPUT })).await;
    h.tick().await;
    h.tick().await;
    assert_eq!(h.run(id).await.status, "completed");

    let logs = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("workflow run completed"),
        "nothing was captured"
    );
    for secret in [SECRET_INPUT, SECRET_STEP, SECRET_OUTPUT, SECRET_ERROR] {
        assert!(!logs.contains(secret), "{secret} was logged");
    }
}

#[tokio::test]
async fn a_retried_step_write_keeps_the_blob_result_the_first_write_committed() {
    // The first write commits, its response is lost and the SDK sends the
    // same write again under the same lease.
    let script: Script = Arc::new(move |engine, token, request| {
        Box::pin(async move {
            let big = json!({ "report": "r".repeat(8 * 1024) });
            let first = engine
                .record_step(&token, request.run_id, completed(0, "report", big.clone()))
                .await
                .unwrap();
            assert!(!first.already_recorded);
            let again = engine
                .record_step(&token, request.run_id, completed(0, "report", big.clone()))
                .await
                .unwrap();
            assert!(again.already_recorded);
            assert_eq!(again.entry.result, Some(big));
            Reply::Outcome(Outcome::Completed { output: None })
        })
    });
    let blobs = Arc::new(crate::payload::tests::MemoryBlobs::default());
    let Some(h) = harness_with(
        "retried blob write test",
        &["reports"],
        script,
        true,
        Some(blobs.clone()),
        None,
    )
    .await
    else {
        return;
    };
    let settings = WorkflowSettings {
        inline_payload_limit_bytes: 1024,
        ..WorkflowSettings::default()
    };
    h.store
        .upsert_settings(h.env.project_id, h.env.environment_id, &settings)
        .await
        .unwrap();
    let id = h.start("reports", json!({})).await;
    assert_eq!(h.tick().await, 1);
    let run = h.run(id).await;
    assert_eq!(run.status, "completed", "{:?}", run.blocked_reason);
    // The journal still reads: the committed object was not deleted.
    let (entries, _) = h.engine.journal_entries(&run, 0, &settings).await.unwrap();
    assert_eq!(
        entries[0].result.as_ref().unwrap()["report"]
            .as_str()
            .unwrap()
            .len(),
        8 * 1024
    );
    assert_eq!(
        blobs.objects.lock().unwrap().len(),
        1,
        "the losing write's object was not discarded"
    );
}

#[tokio::test]
async fn a_claim_made_before_the_manifest_was_cached_still_honours_its_concurrency() {
    let invocations = Arc::new(AtomicUsize::new(0));
    let script: Script = {
        let invocations = invocations.clone();
        Arc::new(move |_engine, _token, _request| {
            invocations.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { Reply::Outcome(Outcome::Completed { output: None }) })
        })
    };
    let Some(h) = harness_with(
        "cold manifest test",
        &["exports"],
        script,
        true,
        None,
        Some(1),
    )
    .await
    else {
        return;
    };
    let first = h.start("exports", json!({})).await;
    let second = h.start("exports", json!({})).await;
    // Nothing is cached yet: the claim takes both, without the limit.
    assert_eq!(h.tick().await, 2);
    assert_eq!(
        invocations.load(Ordering::SeqCst),
        1,
        "both runs of a concurrency-1 workflow ran"
    );
    let runs = [h.run(first).await, h.run(second).await];
    let deferred = runs.iter().find(|run| run.status != "completed").unwrap();
    assert_eq!(deferred.attempt, 0, "a deferral spent an attempt");
    assert!(
        deferred
            .blocked_reason
            .as_deref()
            .unwrap()
            .contains("runs at most 1 at once"),
        "{:?}",
        deferred.blocked_reason
    );
    make_due(&h.env.conn, deferred.id).await;
    assert_eq!(h.tick().await, 1);
    assert_eq!(h.run(deferred.id).await.status, "completed");
}
