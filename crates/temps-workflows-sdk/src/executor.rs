// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::{Client, Context, Error, Invocation, InvocationKey, Outcome, Transport};
use async_trait::async_trait;
use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::Utc;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

#[async_trait]
pub trait Workflow: Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> Option<&str> {
        None
    }
    fn concurrency(&self) -> Option<u32> {
        None
    }
    async fn run(&self, context: &mut Context) -> Result<Value, Error>;
}
pub struct Executor {
    workflows: BTreeMap<String, Arc<dyn Workflow>>,
    key: InvocationKey,
    public: VerifyingKey,
    transport: Arc<dyn Transport>,
}
#[derive(Deserialize)]
struct Claims {
    v: u8,
    typ: String,
    pid: i32,
    eid: i32,
    exp: i64,
    run: Option<String>,
    lease: Option<i64>,
    body: Option<String>,
}
impl Executor {
    /// Load the environment-bound invocation key. Recreate after master-key rotation.
    pub async fn new(client: &Client, workflows: Vec<Arc<dyn Workflow>>) -> Result<Self, Error> {
        let reply = client
            .transport
            .call(
                "GET",
                "/workflows/v1/invocation-key",
                &client.token,
                None,
                true,
            )
            .await?;
        let key = serde_json::from_value(reply).map_err(|_| Error::Encoding {
            operation: "read invocation key".into(),
        })?;
        Self::with_key(key, client.transport.clone(), workflows)
    }
    pub fn with_key(
        key: InvocationKey,
        transport: Arc<dyn Transport>,
        workflows: Vec<Arc<dyn Workflow>>,
    ) -> Result<Self, Error> {
        let invalid = || Error::Configuration {
            reason: "invalid Ed25519 invocation key".into(),
        };
        let raw = URL_SAFE_NO_PAD
            .decode(&key.public_key)
            .map_err(|_| invalid())?;
        let bytes: [u8; 32] = raw.try_into().map_err(|_| invalid())?;
        let public = VerifyingKey::from_bytes(&bytes).map_err(|_| invalid())?;
        if key.algorithm != "ed25519" || key.project_id <= 0 || key.environment_id <= 0 {
            return Err(invalid());
        }
        let mut defined = BTreeMap::new();
        for workflow in workflows {
            crate::runtime::valid_name(workflow.name(), 128)?;
            if workflow
                .concurrency()
                .is_some_and(|value| !(1..=1_000_000).contains(&value))
            {
                return Err(Error::Configuration {
                    reason: format!(
                        "workflow '{}' concurrency must be 1..1000000",
                        workflow.name()
                    ),
                });
            }
            let name = workflow.name().to_string();
            if defined.insert(name.clone(), workflow).is_some() {
                return Err(Error::Configuration {
                    reason: format!("duplicate workflow '{name}'"),
                });
            }
        }
        Ok(Self {
            workflows: defined,
            key,
            public,
            transport,
        })
    }
    pub fn router(self) -> Router {
        Router::new()
            .route("/.well-known/temps/workflows/manifest", get(manifest))
            .route("/.well-known/temps/workflows/invoke", post(invoke))
            .with_state(Arc::new(self))
    }
    fn verify(&self, headers: &HeaderMap, purpose: &str) -> Result<(String, Claims), Error> {
        let header = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or(Error::Authentication)?;
        let (scheme, token) = header.split_once(' ').ok_or(Error::Authentication)?;
        if !scheme.eq_ignore_ascii_case("bearer") || token.len() > 4096 {
            return Err(Error::Authentication);
        }
        let mut parts = token.split('.');
        let (Some("twi1"), Some(payload), Some(signature), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(Error::Authentication);
        };
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| Error::Authentication)?;
        let signature = Signature::from_slice(&signature).map_err(|_| Error::Authentication)?;
        self.public
            .verify_strict(format!("twi1.{payload}").as_bytes(), &signature)
            .map_err(|_| Error::Authentication)?;
        let payload = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| Error::Authentication)?;
        let claims: Claims = serde_json::from_slice(&payload).map_err(|_| Error::Authentication)?;
        if claims.v != 1
            || claims.typ != purpose
            || claims.pid != self.key.project_id
            || claims.eid != self.key.environment_id
            || claims.exp <= Utc::now().timestamp()
        {
            return Err(Error::Authentication);
        }
        Ok((token.into(), claims))
    }
}
fn problem(status: StatusCode, title: &str) -> Response {
    (
        status,
        Json(json!({"title":title,"status":status.as_u16()})),
    )
        .into_response()
}
async fn manifest(State(executor): State<Arc<Executor>>, headers: HeaderMap) -> Response {
    if executor.verify(&headers, "manifest").is_err() {
        return problem(StatusCode::UNAUTHORIZED, "Invalid invocation token");
    }
    let workflows = executor
        .workflows
        .values()
        .map(|w| json!({"name":w.name(),"version":w.version(),"concurrency":w.concurrency()}))
        .collect::<Vec<_>>();
    Json(json!({"protocol_version":1,"sdk":"rust/0.1.0","workflows":workflows})).into_response()
}
async fn invoke(State(executor): State<Arc<Executor>>, headers: HeaderMap, body: Body) -> Response {
    let Ok((token, claims)) = executor.verify(&headers, "invoke") else {
        return problem(StatusCode::UNAUTHORIZED, "Invalid invocation token");
    };
    let bytes = match tokio::time::timeout(
        Duration::from_secs(30),
        axum::body::to_bytes(body, 64 * 1024 * 1024),
    )
    .await
    {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => {
            return problem(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Invocation body exceeds 64 MiB",
            )
        }
        Err(_) => {
            return problem(
                StatusCode::REQUEST_TIMEOUT,
                "Invocation body read timed out",
            )
        }
    };
    if claims.body.as_deref() != Some(&hex::encode(Sha256::digest(&bytes))) {
        return problem(
            StatusCode::UNAUTHORIZED,
            "Invocation body signature mismatch",
        );
    }
    let request: Invocation = match serde_json::from_slice(&bytes) {
        Ok(request) => request,
        Err(_) => return problem(StatusCode::BAD_REQUEST, "Invalid workflow invocation"),
    };
    if request.protocol_version != 1
        || claims.run.as_deref() != Some(&request.run_id)
        || claims.lease != Some(request.lease)
    {
        return problem(StatusCode::UNAUTHORIZED, "Invocation scope mismatch");
    }
    let Some(workflow) = executor.workflows.get(&request.workflow) else {
        return problem(StatusCode::NOT_FOUND, "Workflow not defined");
    };
    let (status, outcome) = run(
        workflow.as_ref(),
        request,
        token,
        executor.transport.clone(),
    )
    .await;
    match outcome {
        Some(outcome) => (status, Json(json!({"outcome":outcome}))).into_response(),
        None => status.into_response(),
    }
}
struct Pulse(tokio::task::JoinHandle<()>);
impl Drop for Pulse {
    fn drop(&mut self) {
        self.0.abort();
    }
}
pub(crate) async fn run(
    workflow: &dyn Workflow,
    request: Invocation,
    token: String,
    transport: Arc<dyn Transport>,
) -> (StatusCode, Option<Outcome>) {
    let deadline = (request.deadline_at - request.now)
        .to_std()
        .unwrap_or_default();
    let interval = Duration::from_secs(
        ((request.lease_expires_at - request.now).num_seconds() / 3).max(1) as u64,
    );
    let mut context = Context::new(request, token, transport);
    let stopped = context.stopped.clone();
    let transport = context.transport.clone();
    let token = context.token.clone();
    let path = format!("/workflow-runs/{}/heartbeat", context.run_id);
    let pulse = Pulse(tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            match transport
                .call("POST", &path, &token, Some(json!({})), true)
                .await
            {
                Ok(reply) if reply.get("cancelled") == Some(&Value::Bool(true)) => {
                    stopped.store(202, Ordering::Release);
                    break;
                }
                Err(Error::Api { status: 409, .. }) => {
                    stopped.store(202, Ordering::Release);
                    break;
                }
                _ => {}
            }
        }
    }));
    let result = tokio::time::timeout(deadline, workflow.run(&mut context)).await;
    drop(pulse);
    let stopped = context.stopped.load(Ordering::Acquire);
    if stopped != 0 {
        return (
            StatusCode::from_u16(stopped).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
            None,
        );
    }
    if let Some(outcome) = context.outcome {
        return (StatusCode::OK, Some(outcome));
    }
    if result.as_ref().is_ok_and(|result| result.is_ok()) {
        if let Some(sequence) = context.unread_sequence() {
            return (
                StatusCode::OK,
                Some(Outcome::Incompatible {
                    sequence,
                    expected: "recorded journal entry".into(),
                    found: "end of workflow".into(),
                }),
            );
        }
    }
    match result {
        Ok(Ok(output)) => (StatusCode::OK, Some(Outcome::Completed { output })),
        Ok(Err(Error::Control)) | Err(_) => (StatusCode::SERVICE_UNAVAILABLE, None),
        Ok(Err(error)) => (
            StatusCode::OK,
            Some(Outcome::Failed {
                error: error.to_string().chars().take(16000).collect(),
                sequence: match error {
                    Error::RecordedStep { sequence, .. } => Some(sequence),
                    _ => None,
                },
            }),
        ),
    }
}
