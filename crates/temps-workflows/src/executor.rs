// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP calls from Temps to an executor (the SDK inside the user's service):
//! `invoke` and `manifest`, each signed with an invocation token, with a
//! timeout and a bounded response body.
//!
//! Errors carry a fixed [`ExecutorCallError::category`] for metrics and a
//! message for the run's `blocked_reason`; neither ever includes a response
//! body, which may echo payloads.

use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use temps_core::ExecutorEndpoint;
use thiserror::Error;
use uuid::Uuid;

use crate::protocol::{
    Manifest, OutcomeRequest, INVOKE_PATH, MANIFEST_PATH, PROTOCOL_HEADER, PROTOCOL_VERSION,
};
use crate::token::{sha256_hex, InvocationClaims, InvocationSigner, TokenPurpose};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub const MANIFEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Largest manifest accepted.
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
/// Tokens stay valid this long past the invocation deadline, so the
/// executor's last journal write is not rejected for a few seconds of skew.
pub const TOKEN_GRACE_SECS: i64 = 60;

#[derive(Debug, Error)]
pub enum ExecutorCallError {
    #[error("could not connect to the executor at {url}: {reason}")]
    Connect { url: String, reason: String },
    #[error("the executor at {url} did not answer within {seconds}s")]
    Timeout { url: String, seconds: u64 },
    #[error("the executor at {url} answered HTTP {status}{hint}")]
    Status {
        url: String,
        status: u16,
        hint: &'static str,
    },
    #[error("the executor at {url} sent a response over the {limit}-byte limit")]
    TooLarge { url: String, limit: usize },
    #[error("the executor at {url} sent a response that is not protocol version {PROTOCOL_VERSION}: {reason}")]
    Protocol { url: String, reason: String },
    #[error("could not sign the request: {reason}")]
    Signing { reason: String },
}

impl ExecutorCallError {
    /// A fixed category, for metric labels.
    pub fn category(&self) -> &'static str {
        match self {
            ExecutorCallError::Connect { .. } => "connect",
            ExecutorCallError::Timeout { .. } => "timeout",
            ExecutorCallError::Status { .. } => "http_status",
            ExecutorCallError::TooLarge { .. } => "too_large",
            ExecutorCallError::Protocol { .. } => "protocol",
            ExecutorCallError::Signing { .. } => "signing",
        }
    }
}

/// What an executor answered to an invoke.
#[derive(Debug, Clone, PartialEq)]
pub enum InvokeReply {
    /// `200` with the outcome in the body.
    Outcome(OutcomeRequest),
    /// `202`: the executor reports the outcome with a callback.
    Accepted,
}

/// The invocation to send; `body` is the serialized `InvokeRequest`.
pub struct Invocation<'a> {
    pub project_id: i32,
    pub environment_id: i32,
    pub run_id: Uuid,
    pub lease: i64,
    pub deadline_at: DateTime<Utc>,
    pub body: Vec<u8>,
    pub max_response_bytes: usize,
    pub endpoint: &'a ExecutorEndpoint,
}

pub struct ExecutorClient {
    http: reqwest::Client,
}

impl ExecutorClient {
    pub fn new() -> Result<Self, ExecutorCallError> {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            // Executors are internal addresses; never follow a redirect off them.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| ExecutorCallError::Signing {
                reason: format!("HTTP client could not be built: {error}"),
            })?;
        Ok(Self { http })
    }

    pub async fn manifest(
        &self,
        signer: &InvocationSigner,
        project_id: i32,
        environment_id: i32,
        endpoint: &ExecutorEndpoint,
    ) -> Result<Manifest, ExecutorCallError> {
        let url = format!("{}{MANIFEST_PATH}", endpoint.base_url.trim_end_matches('/'));
        let token = signer
            .sign(&InvocationClaims {
                v: 1,
                typ: TokenPurpose::Manifest,
                pid: project_id,
                eid: environment_id,
                run: None,
                lease: None,
                exp: Utc::now().timestamp() + MANIFEST_TIMEOUT.as_secs() as i64 + TOKEN_GRACE_SECS,
                body: None,
            })
            .map_err(|error| ExecutorCallError::Signing {
                reason: error.to_string(),
            })?;
        let response = self
            .http
            .get(&url)
            .bearer_auth(token)
            .header(PROTOCOL_HEADER, PROTOCOL_VERSION.to_string())
            .timeout(MANIFEST_TIMEOUT)
            .send()
            .await
            .map_err(|error| send_error(&url, &error, MANIFEST_TIMEOUT))?;
        let status = response.status();
        if !status.is_success() {
            return Err(status_error(&url, status));
        }
        let body = read_limited(&url, response, MAX_MANIFEST_BYTES, MANIFEST_TIMEOUT).await?;
        let manifest: Manifest =
            serde_json::from_slice(&body).map_err(|error| ExecutorCallError::Protocol {
                url: url.clone(),
                reason: format!("manifest did not parse: {error}"),
            })?;
        if manifest.protocol_version != PROTOCOL_VERSION {
            return Err(ExecutorCallError::Protocol {
                url,
                reason: format!(
                    "the SDK speaks protocol version {}",
                    manifest.protocol_version
                ),
            });
        }
        Ok(manifest)
    }

    pub async fn invoke(
        &self,
        signer: &InvocationSigner,
        invocation: Invocation<'_>,
    ) -> Result<InvokeReply, ExecutorCallError> {
        let url = format!(
            "{}{INVOKE_PATH}",
            invocation.endpoint.base_url.trim_end_matches('/')
        );
        let token = signer
            .sign(&InvocationClaims {
                v: 1,
                typ: TokenPurpose::Invoke,
                pid: invocation.project_id,
                eid: invocation.environment_id,
                run: Some(invocation.run_id),
                lease: Some(invocation.lease),
                exp: invocation.deadline_at.timestamp() + TOKEN_GRACE_SECS,
                body: Some(sha256_hex(&invocation.body)),
            })
            .map_err(|error| ExecutorCallError::Signing {
                reason: error.to_string(),
            })?;
        let timeout = (invocation.deadline_at - Utc::now())
            .to_std()
            .unwrap_or(Duration::from_secs(1))
            .max(Duration::from_secs(1));
        let response = self
            .http
            .post(&url)
            .bearer_auth(token)
            .header(PROTOCOL_HEADER, PROTOCOL_VERSION.to_string())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .timeout(timeout)
            .body(invocation.body)
            .send()
            .await
            .map_err(|error| send_error(&url, &error, timeout))?;
        let status = response.status();
        if status == StatusCode::ACCEPTED {
            return Ok(InvokeReply::Accepted);
        }
        if !status.is_success() {
            return Err(status_error(&url, status));
        }
        let body = read_limited(&url, response, invocation.max_response_bytes, timeout).await?;
        let outcome: OutcomeRequest =
            serde_json::from_slice(&body).map_err(|error| ExecutorCallError::Protocol {
                url,
                reason: format!("the invoke response is not an outcome: {error}"),
            })?;
        Ok(InvokeReply::Outcome(outcome))
    }
}

fn send_error(url: &str, error: &reqwest::Error, timeout: Duration) -> ExecutorCallError {
    if error.is_timeout() {
        ExecutorCallError::Timeout {
            url: url.to_string(),
            seconds: timeout.as_secs(),
        }
    } else {
        ExecutorCallError::Connect {
            url: url.to_string(),
            reason: error_chain(error),
        }
    }
}

fn error_chain(error: &dyn std::error::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

fn status_error(url: &str, status: StatusCode) -> ExecutorCallError {
    let hint = match status {
        StatusCode::NOT_FOUND => {
            " (the service does not serve the Temps workflows endpoint; is the SDK handler mounted?)"
        }
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            " (the SDK rejected the invocation token; check TEMPS_API_URL points at this Temps)"
        }
        StatusCode::CONFLICT => " (the SDK found the run's lease already moved on)",
        _ => "",
    };
    ExecutorCallError::Status {
        url: url.to_string(),
        status: status.as_u16(),
        hint,
    }
}

async fn read_limited(
    url: &str,
    mut response: reqwest::Response,
    limit: usize,
    timeout: Duration,
) -> Result<Vec<u8>, ExecutorCallError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(ExecutorCallError::TooLarge {
            url: url.to_string(),
            limit,
        });
    }
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if body.len() + chunk.len() > limit {
                    return Err(ExecutorCallError::TooLarge {
                        url: url.to_string(),
                        limit,
                    });
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => return Ok(body),
            Err(error) => return Err(send_error(url, &error, timeout)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::get, routing::post, Json, Router};
    use temps_core::EncryptionService;

    async fn serve(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        format!("http://{address}")
    }

    fn endpoint(base_url: String) -> ExecutorEndpoint {
        ExecutorEndpoint {
            base_url,
            container_id: 1,
            node_id: None,
        }
    }

    fn signer() -> InvocationSigner {
        InvocationSigner::new(&EncryptionService::new("12345678901234567890123456789012").unwrap())
    }

    #[tokio::test]
    async fn the_manifest_is_fetched_with_a_signed_manifest_token() {
        let verifier = std::sync::Arc::new(signer());
        let check = verifier.clone();
        let base = serve(Router::new().route(
            MANIFEST_PATH,
            get(move |headers: axum::http::HeaderMap| {
                let check = check.clone();
                async move {
                    let token = headers["authorization"]
                        .to_str()
                        .unwrap()
                        .trim_start_matches("Bearer ")
                        .to_string();
                    let claims = check.verify(&token, Utc::now()).unwrap();
                    assert_eq!(claims.typ, TokenPurpose::Manifest);
                    assert_eq!((claims.pid, claims.eid), (3, 4));
                    Json(serde_json::json!({
                        "protocol_version": 1,
                        "workflows": [{"name": "orders", "concurrency": 2}]
                    }))
                }
            }),
        ))
        .await;
        let client = ExecutorClient::new().unwrap();
        let manifest = client
            .manifest(&verifier, 3, 4, &endpoint(base))
            .await
            .unwrap();
        assert_eq!(manifest.workflows[0].name, "orders");
        assert_eq!(manifest.workflows[0].concurrency, Some(2));
    }

    #[tokio::test]
    async fn a_missing_handler_explains_itself() {
        let base = serve(Router::new()).await;
        let error = ExecutorClient::new()
            .unwrap()
            .manifest(&signer(), 1, 1, &endpoint(base))
            .await
            .unwrap_err();
        assert_eq!(error.category(), "http_status");
        assert!(
            error.to_string().contains("is the SDK handler mounted"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn invoke_binds_the_token_to_the_body_and_bounds_the_reply() {
        let verifier = std::sync::Arc::new(signer());
        let check = verifier.clone();
        let base = serve(Router::new().route(
            INVOKE_PATH,
            post(
                move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
                    let check = check.clone();
                    async move {
                        let token = headers["authorization"]
                            .to_str()
                            .unwrap()
                            .trim_start_matches("Bearer ")
                            .to_string();
                        let claims = check.verify(&token, Utc::now()).unwrap();
                        assert_eq!(claims.body.as_deref(), Some(sha256_hex(&body).as_str()));
                        assert_eq!(claims.lease, Some(9));
                        if body.as_ref() == b"{\"big\":true}" {
                            return "x".repeat(4096);
                        }
                        r#"{"outcome":{"type":"completed","output":1}}"#.to_string()
                    }
                },
            ),
        ))
        .await;
        let client = ExecutorClient::new().unwrap();
        let target = endpoint(base);
        let invocation = |body: &[u8]| Invocation {
            project_id: 1,
            environment_id: 2,
            run_id: Uuid::new_v4(),
            lease: 9,
            deadline_at: Utc::now() + chrono::Duration::seconds(30),
            body: body.to_vec(),
            max_response_bytes: 1024,
            endpoint: &target,
        };
        let reply = client.invoke(&verifier, invocation(b"{}")).await.unwrap();
        assert!(matches!(reply, InvokeReply::Outcome(_)));
        let error = client
            .invoke(&verifier, invocation(b"{\"big\":true}"))
            .await
            .unwrap_err();
        assert_eq!(error.category(), "too_large");
    }

    #[tokio::test]
    async fn an_unreachable_executor_is_a_connect_error() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let error = ExecutorClient::new()
            .unwrap()
            .manifest(&signer(), 1, 1, &endpoint(format!("http://{address}")))
            .await
            .unwrap_err();
        assert_eq!(error.category(), "connect");
    }
}
