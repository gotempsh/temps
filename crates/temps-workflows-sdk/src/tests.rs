// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
#[derive(Default)]
struct Recorder {
    entries: Mutex<Vec<Value>>,
    outage: bool,
}
#[async_trait]
impl Transport for Recorder {
    async fn call(
        &self,
        _method: &str,
        path: &str,
        _token: &str,
        body: Option<Value>,
        _retry: bool,
    ) -> Result<Value, Error> {
        if self.outage {
            return Err(Error::Api {
                operation: "record".into(),
                status: 503,
            });
        }
        if path.contains("journal?") {
            return Ok(
                json!({"entries":self.entries.lock().unwrap().clone(),"next_sequence":null}),
            );
        }
        let body = body.unwrap();
        self.entries.lock().unwrap().push(body.clone());
        Ok(json!({"entry":body}))
    }
}
fn request() -> Invocation {
    let now = Utc::now();
    Invocation {
        protocol_version: 1,
        run_id: "00000000-0000-0000-0000-000000000001".into(),
        workflow: "orders".into(),
        attempt: 1,
        lease: 1,
        now,
        deadline_at: now + Duration::minutes(10),
        lease_expires_at: now + Duration::minutes(2),
        input: None,
        journal: vec![],
        journal_next_sequence: None,
    }
}
struct Charge(Arc<AtomicUsize>);
#[async_trait]
impl Workflow for Charge {
    fn name(&self) -> &str {
        "orders"
    }
    async fn run(&self, c: &mut Context) -> Result<Value, Error> {
        c.step("charge", StepOptions::default(), |_| async {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"ok":true}))
        })
        .await
    }
}
#[tokio::test]
async fn replay_reads_the_next_page_without_repeating_effects_and_checks_names() {
    let counter = Arc::new(AtomicUsize::new(0));
    let workflow = Charge(counter.clone());
    let transport = Arc::new(Recorder::default());
    let result = executor::run(&workflow, request(), "synthetic".into(), transport.clone()).await;
    assert!(matches!(result.1, Some(Outcome::Completed { .. })));
    let mut replay = request();
    replay.journal_next_sequence = Some(0);
    executor::run(
        &workflow,
        replay.clone(),
        "synthetic".into(),
        transport.clone(),
    )
    .await;
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    transport.entries.lock().unwrap()[0]["name"] = json!("old");
    assert!(matches!(
        executor::run(&workflow, replay, "synthetic".into(), transport)
            .await
            .1,
        Some(Outcome::Incompatible { sequence: 0, .. })
    ));
}
struct Swallow;
#[async_trait]
impl Workflow for Swallow {
    fn name(&self) -> &str {
        "orders"
    }
    async fn run(&self, c: &mut Context) -> Result<Value, Error> {
        let _ = c.sleep("pause", std::time::Duration::from_secs(10)).await;
        Ok(json!("must not complete"))
    }
}
#[tokio::test]
async fn swallowed_suspension_and_outages_cannot_complete_a_run() {
    let result = executor::run(
        &Swallow,
        request(),
        "synthetic".into(),
        Arc::new(Recorder::default()),
    )
    .await;
    assert!(matches!(result.1, Some(Outcome::Sleep { .. })));
    let result = executor::run(
        &Swallow,
        request(),
        "synthetic".into(),
        Arc::new(Recorder {
            outage: true,
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(result.0, axum::http::StatusCode::SERVICE_UNAVAILABLE);
    assert!(result.1.is_none());
}
#[tokio::test]
async fn handler_binds_token_to_environment_body_run_and_lease() {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use ed25519_dalek::Signer;
    use sha2::{Digest, Sha256};
    use tower::ServiceExt;
    let signing = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
    let counter = Arc::new(AtomicUsize::new(0));
    let executor = Executor::with_key(
        InvocationKey {
            algorithm: "ed25519".into(),
            public_key: URL_SAFE_NO_PAD.encode(signing.verifying_key().to_bytes()),
            project_id: 1,
            environment_id: 2,
        },
        Arc::new(Recorder::default()),
        vec![Arc::new(Charge(counter.clone()))],
    )
    .unwrap();
    let app = executor.router();
    let invocation = request();
    let body = serde_json::to_vec(&invocation).unwrap();
    for (pid, eid, lease, digest, status) in [
        (3, 2, 1, hex::encode(Sha256::digest(&body)), 401),
        (1, 4, 1, hex::encode(Sha256::digest(&body)), 401),
        (1, 2, 2, hex::encode(Sha256::digest(&body)), 401),
        (1, 2, 1, "wrong".into(), 401),
        (1, 2, 1, hex::encode(Sha256::digest(&body)), 200),
    ] {
        let claims = json!({"v":1,"typ":"invoke","pid":pid,"eid":eid,"run":invocation.run_id,"lease":lease,"exp":Utc::now().timestamp()+60,"body":digest});
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let message = format!("twi1.{payload}");
        let token = format!(
            "{message}.{}",
            URL_SAFE_NO_PAD.encode(signing.sign(message.as_bytes()).to_bytes())
        );
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/.well-known/temps/workflows/invoke")
                    .header("authorization", format!("Bearer {token}"))
                    .body(axum::body::Body::from(body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status);
    }
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

struct Fails;
#[async_trait]
impl Workflow for Fails {
    fn name(&self) -> &str {
        "orders"
    }
    async fn run(&self, c: &mut Context) -> Result<Value, Error> {
        c.step(
            "flaky",
            StepOptions {
                retries: 1,
                ..Default::default()
            },
            |attempt| async move {
                Err(Error::Step {
                    name: "flaky".into(),
                    message: format!("attempt {attempt}"),
                })
            },
        )
        .await
    }
}
#[tokio::test]
async fn errors_retry_then_exhaust_their_budget() {
    let transport = Arc::new(Recorder::default());
    assert!(matches!(
        executor::run(&Fails, request(), "synthetic".into(), transport.clone())
            .await
            .1,
        Some(Outcome::Retry { sequence: 0, .. })
    ));
    let mut req = request();
    req.journal =
        vec![serde_json::from_value(transport.entries.lock().unwrap()[0].clone()).unwrap()];
    assert!(matches!(
        executor::run(&Fails, req, "synthetic".into(), transport)
            .await
            .1,
        Some(Outcome::Failed { .. })
    ));
}
#[tokio::test]
async fn event_timeout_and_deterministic_values_replay_without_api_calls() {
    let transport = Arc::new(Recorder::default());
    let mut c = Context::new(request(), "synthetic".into(), transport.clone());
    let now = c.now("clock").await.unwrap();
    let random = c.random("random").await.unwrap();
    assert!(random.as_f64().is_some_and(|v| (0.0..1.0).contains(&v)));
    let mut req = request();
    req.journal = transport
        .entries
        .lock()
        .unwrap()
        .iter()
        .cloned()
        .map(|v| serde_json::from_value(v).unwrap())
        .collect();
    let mut c = Context::new(
        req,
        "synthetic".into(),
        Arc::new(Recorder {
            outage: true,
            ..Default::default()
        }),
    );
    assert_eq!(c.now("clock").await.unwrap(), now);
    assert_eq!(c.random("random").await.unwrap(), random);
    let mut req = request();
    req.journal = vec![Entry {
        sequence: 0,
        name: "approval".into(),
        kind: "wait".into(),
        status: "pending".into(),
        attempts: 1,
        wake_at: Some(Utc::now() - Duration::seconds(1)),
        result: None,
        error: None,
    }];
    let mut c = Context::new(req, "synthetic".into(), transport);
    assert_eq!(
        c.wait_for_event("approval", "paid", None, None)
            .await
            .unwrap(),
        json!({"received":false,"payload":null})
    );
}
#[tokio::test]
async fn child_and_send_event_results_replay_with_the_same_sequence() {
    let transport = Arc::new(Recorder {
        outage: true,
        ..Default::default()
    });
    let mut req = request();
    req.journal = vec![
        Entry {
            sequence: 0,
            name: "child".into(),
            kind: "child".into(),
            status: "completed".into(),
            attempts: 1,
            result: Some(json!({"status":"completed","output":7})),
            error: None,
            wake_at: None,
        },
        Entry {
            sequence: 1,
            name: "sent".into(),
            kind: "send_event".into(),
            status: "completed".into(),
            attempts: 1,
            result: Some(json!({"event_id":9})),
            error: None,
            wake_at: None,
        },
    ];
    let mut c = Context::new(req, "synthetic".into(), transport);
    assert_eq!(c.child("child", "square", None).await.unwrap()["output"], 7);
    assert_eq!(
        c.send_event(
            "sent",
            Event {
                name: "paid".into(),
                key: None,
                payload: None
            }
        )
        .await
        .unwrap()["event_id"],
        9
    );
}

struct Overflow;
#[async_trait]
impl Workflow for Overflow {
    fn name(&self) -> &str {
        "orders"
    }
    async fn run(&self, c: &mut Context) -> Result<Value, Error> {
        assert!(matches!(
            c.sleep("overflow", std::time::Duration::MAX).await,
            Err(Error::Configuration { .. })
        ));
        assert!(matches!(
            c.wait_for_event("overflow", "event", None, Some(std::time::Duration::MAX))
                .await,
            Err(Error::Configuration { .. })
        ));
        Ok(Value::Null)
    }
}
#[tokio::test]
async fn excessive_durations_return_errors_without_panicking() {
    let result = executor::run(
        &Overflow,
        request(),
        "synthetic".into(),
        Arc::new(Recorder::default()),
    )
    .await;
    assert!(matches!(result.1, Some(Outcome::Completed { .. })));
}

struct Empty;
#[async_trait]
impl Workflow for Empty {
    fn name(&self) -> &str {
        "orders"
    }
    async fn run(&self, _: &mut Context) -> Result<Value, Error> {
        Ok(json!("done"))
    }
}
#[tokio::test]
async fn removed_wait_cannot_complete_including_unread_pages() {
    for paged in [false, true] {
        let mut req = request();
        if paged {
            req.journal_next_sequence = Some(0);
        } else {
            req.journal=serde_json::from_value(json!([{"sequence":0,"name":"approval","kind":"wait","status":"pending","attempts":1}])).unwrap();
        }
        let result =
            executor::run(&Empty, req, "token".into(), Arc::new(Recorder::default())).await;
        assert!(matches!(
            result.1,
            Some(Outcome::Incompatible { sequence: 0, .. })
        ));
    }
}
#[tokio::test]
async fn retry_backoff_honors_configured_ten_minutes() {
    let req = request();
    let now = req.now;
    let mut context = Context::new(req, "token".into(), Arc::new(Recorder::default()));
    let options = StepOptions {
        initial_backoff: std::time::Duration::from_secs(600),
        max_backoff: std::time::Duration::from_secs(600),
        ..Default::default()
    };
    let result = context
        .step("retry", options, |_| async {
            Err(Error::Step {
                name: "retry".into(),
                message: "transient".into(),
            })
        })
        .await;
    assert!(matches!(result, Err(Error::Control)));
    match context.outcome.unwrap() {
        Outcome::Retry { at, .. } => assert!((at - now).num_seconds() >= 600),
        other => panic!("expected retry, got {other:?}"),
    }
}
