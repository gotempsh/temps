// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Turn-scoped model relay for sandboxed development harnesses.
//!
//! The harness receives an opaque, short-lived bearer and an Anthropic-compatible
//! base URL. The real provider credential stays in this process and is attached
//! only after the relay has authenticated the capability and allowlisted the
//! upstream path.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::{to_bytes, Body};
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use futures::{Stream, StreamExt};

use temps_ai::AiError;

const MAX_MODEL_REQUEST_BYTES: usize = 16 * 1024 * 1024;
const MAX_MODEL_REQUESTS_PER_TURN: u32 = 256;
const MAX_CONCURRENT_MODEL_REQUESTS_PER_TURN: usize = 4;
const MAX_OUTPUT_TOKENS_PER_REQUEST: u64 = 32_768;
const MODEL_REQUEST_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const ALLOWED_REQUEST_HEADERS: &[&str] = &[
    "accept",
    "anthropic-beta",
    "anthropic-version",
    "content-type",
    "user-agent",
];
const ALLOWED_RESPONSE_HEADERS: &[&str] = &[
    "content-type",
    "request-id",
    "retry-after",
    "x-should-retry",
];

/// Provider credential retained only in host memory for one active turn.
/// Deliberately has no `Debug`, `Clone`, or serialization implementation.
pub enum SandboxProviderCredential {
    AnthropicApiKey(String),
    ClaudeOauthToken(String),
}

/// Material resolved immediately before a sandbox turn. The control-plane URL
/// is not a secret; the provider credential is consumed into the relay and is
/// never written to the sandbox filesystem or process environment.
pub struct SandboxHarnessCredentials {
    pub(crate) provider_credential: SandboxProviderCredential,
    pub(crate) internal_api_url: String,
}

impl SandboxHarnessCredentials {
    pub fn anthropic_api_key(
        value: impl Into<String>,
        internal_api_url: impl Into<String>,
    ) -> Self {
        Self {
            provider_credential: SandboxProviderCredential::AnthropicApiKey(value.into()),
            internal_api_url: internal_api_url.into(),
        }
    }

    pub fn claude_oauth_token(
        value: impl Into<String>,
        internal_api_url: impl Into<String>,
    ) -> Self {
        Self {
            provider_credential: SandboxProviderCredential::ClaudeOauthToken(value.into()),
            internal_api_url: internal_api_url.into(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SandboxModelRelay {
    pub base_url: String,
    pub bearer: String,
}

impl std::fmt::Debug for SandboxModelRelay {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SandboxModelRelay")
            .field("base_url", &self.base_url)
            .field("bearer", &"[REDACTED]")
            .finish()
    }
}

struct RelayEntry {
    bearer: String,
    credential: SandboxProviderCredential,
    principal_id: i32,
    selected_model: String,
    remaining_requests: Arc<AtomicU32>,
    request_slot: Arc<tokio::sync::Semaphore>,
    expires_at: Instant,
}

/// Host-side registry for active model relay capabilities.
pub struct SandboxModelRelayService {
    client: reqwest::Client,
    entries: Arc<Mutex<HashMap<String, RelayEntry>>>,
}

impl SandboxModelRelayService {
    pub fn new() -> Result<Self, AiError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| AiError::Provider {
                purpose: "chat.application.model_relay".to_string(),
                reason: format!("could not initialize the sandbox model relay: {error}"),
            })?;
        Ok(Self {
            client,
            entries: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub(crate) fn register(
        &self,
        provider: &str,
        principal_id: i32,
        selected_model: Option<&str>,
        credentials: SandboxHarnessCredentials,
        relay_base_url: &str,
        lifetime: Duration,
    ) -> Result<(SandboxModelRelay, SandboxModelRelayGuard), AiError> {
        if provider != "claude_cli" {
            return Err(AiError::Provider {
                purpose: "chat.application.model_relay".to_string(),
                reason: format!(
                    "secure model relay is not implemented for development harness '{provider}'"
                ),
            });
        }
        if relay_base_url.trim().is_empty() {
            return Err(AiError::Provider {
                purpose: "chat.application.model_relay".to_string(),
                reason: "the sandbox model relay base URL is empty".to_string(),
            });
        }
        let selected_model = resolve_claude_model(selected_model)?;
        let relay_id = uuid::Uuid::new_v4().simple().to_string();
        let bearer = format!(
            "tmodel_{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let entry = RelayEntry {
            bearer: bearer.clone(),
            credential: credentials.provider_credential,
            principal_id,
            selected_model,
            remaining_requests: Arc::new(AtomicU32::new(MAX_MODEL_REQUESTS_PER_TURN)),
            request_slot: Arc::new(tokio::sync::Semaphore::new(
                MAX_CONCURRENT_MODEL_REQUESTS_PER_TURN,
            )),
            expires_at: Instant::now() + lifetime,
        };
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(relay_id.clone(), entry);
        let relay = SandboxModelRelay {
            base_url: format!("{}/{relay_id}", relay_base_url.trim_end_matches('/')),
            bearer,
        };
        let guard = SandboxModelRelayGuard {
            entries: self.entries.clone(),
            relay_id,
        };
        Ok((relay, guard))
    }

    async fn relay(
        &self,
        relay_id: &str,
        bearer: &str,
        method: Method,
        path: &str,
        headers: HeaderMap,
        body: Body,
    ) -> Result<Response, RelayError> {
        let normalized_path = path.trim_start_matches('/');
        let (credential, expires_at, selected_model, remaining_requests, request_slot) = {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(entry) = entries.get(relay_id) else {
                return Err(RelayError::Unauthorized);
            };
            if entry.expires_at <= Instant::now() {
                entries.remove(relay_id);
                return Err(RelayError::Unauthorized);
            }
            if !constant_time_eq(entry.bearer.as_bytes(), bearer.as_bytes()) {
                return Err(RelayError::Unauthorized);
            }
            let credential = match &entry.credential {
                SandboxProviderCredential::AnthropicApiKey(value) => {
                    RequestCredential::AnthropicApiKey(value.clone())
                }
                SandboxProviderCredential::ClaudeOauthToken(value) => {
                    RequestCredential::ClaudeOauthToken(value.clone())
                }
            };
            tracing::debug!(
                principal_id = entry.principal_id,
                relay_id,
                path = normalized_path,
                "authorized sandbox model relay request"
            );
            (
                credential,
                entry.expires_at,
                entry.selected_model.clone(),
                entry.remaining_requests.clone(),
                entry.request_slot.clone(),
            )
        };
        if expires_at <= Instant::now() {
            return Err(RelayError::Unauthorized);
        }
        if method != Method::POST {
            return Err(RelayError::MethodNotAllowed);
        }
        if !matches!(normalized_path, "v1/messages" | "v1/messages/count_tokens") {
            return Err(RelayError::PathNotAllowed);
        }
        if remaining_requests
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                remaining.checked_sub(1)
            })
            .is_err()
        {
            return Err(RelayError::RequestBudgetExhausted);
        }
        let request_permit = request_slot
            .try_acquire_owned()
            .map_err(|_| RelayError::TooManyConcurrentRequests)?;
        let bytes = to_bytes(body, MAX_MODEL_REQUEST_BYTES)
            .await
            .map_err(|_| RelayError::RequestTooLarge)?;
        let bytes = normalize_model_request(&bytes, normalized_path, &selected_model)?;
        let url = format!("https://api.anthropic.com/{normalized_path}");
        let mut request = self.client.request(method, url).body(bytes);
        for name in ALLOWED_REQUEST_HEADERS {
            if let Some(value) = headers.get(*name) {
                request = request.header(*name, value);
            }
        }
        request = match credential {
            RequestCredential::AnthropicApiKey(value) => request.header("x-api-key", value),
            RequestCredential::ClaudeOauthToken(value) => {
                request.header(header::AUTHORIZATION, format!("Bearer {value}"))
            }
        };
        let upstream = request
            .timeout(MODEL_REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(RelayError::Upstream)?;
        let status = upstream.status();
        let response_headers = upstream.headers().clone();
        let upstream_stream: std::pin::Pin<
            Box<dyn Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send>,
        > = Box::pin(upstream.bytes_stream());
        let stream = futures::stream::unfold(
            (upstream_stream, request_permit),
            |(mut upstream, permit)| async move {
                upstream.next().await.map(|chunk| {
                    (
                        chunk.map_err(|error| {
                            std::io::Error::other(format!("model relay stream: {error}"))
                        }),
                        (upstream, permit),
                    )
                })
            },
        );
        let mut response = Response::builder().status(status);
        for name in ALLOWED_RESPONSE_HEADERS {
            if let Some(value) = response_headers.get(*name) {
                response = response.header(*name, value);
            }
        }
        response
            .body(Body::from_stream(stream))
            .map_err(|_| RelayError::ResponseBuild)
    }
}

fn normalize_model_request(
    bytes: &[u8],
    path: &str,
    selected_model: &str,
) -> Result<Vec<u8>, RelayError> {
    let mut payload =
        serde_json::from_slice::<serde_json::Value>(bytes).map_err(|_| RelayError::InvalidJson)?;
    let object = payload.as_object_mut().ok_or(RelayError::InvalidJson)?;
    object.insert(
        "model".to_string(),
        serde_json::Value::String(selected_model.to_string()),
    );
    if path == "v1/messages" {
        let max_tokens = object
            .get("max_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(MAX_OUTPUT_TOKENS_PER_REQUEST)
            .min(MAX_OUTPUT_TOKENS_PER_REQUEST);
        object.insert(
            "max_tokens".to_string(),
            serde_json::Value::Number(max_tokens.into()),
        );
    }
    serde_json::to_vec(&payload).map_err(|_| RelayError::InvalidJson)
}

fn resolve_claude_model(selected_model: Option<&str>) -> Result<String, AiError> {
    let model = match selected_model.map(str::trim) {
        None | Some("") | Some("default") | Some("sonnet") => "claude-sonnet-5",
        Some("opus") => "claude-opus-5",
        Some("haiku") => "claude-haiku-4-5",
        Some(model) if model.starts_with("claude-") => model,
        Some(model) => {
            return Err(AiError::Provider {
                purpose: "chat.application.model_relay".to_string(),
                reason: format!(
                    "model '{model}' is not an allowed concrete Claude model for sandbox execution"
                ),
            })
        }
    };
    Ok(model.to_string())
}

enum RequestCredential {
    AnthropicApiKey(String),
    ClaudeOauthToken(String),
}

pub(crate) struct SandboxModelRelayGuard {
    entries: Arc<Mutex<HashMap<String, RelayEntry>>>,
    relay_id: String,
}

impl Drop for SandboxModelRelayGuard {
    fn drop(&mut self) {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.relay_id);
    }
}

#[derive(Debug, thiserror::Error)]
enum RelayError {
    #[error("sandbox model relay is not authorized")]
    Unauthorized,
    #[error("sandbox model relay only accepts POST")]
    MethodNotAllowed,
    #[error("sandbox model relay path is not allowed")]
    PathNotAllowed,
    #[error("sandbox model relay request exceeds 16 MiB")]
    RequestTooLarge,
    #[error("sandbox model relay request body must be a JSON object")]
    InvalidJson,
    #[error("sandbox model relay exhausted its per-turn request budget")]
    RequestBudgetExhausted,
    #[error("sandbox model relay concurrency limit reached")]
    TooManyConcurrentRequests,
    #[error("sandbox model relay upstream request failed: {0}")]
    Upstream(reqwest::Error),
    #[error("sandbox model relay could not build the upstream response")]
    ResponseBuild,
}

impl IntoResponse for RelayError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Self::PathNotAllowed => StatusCode::NOT_FOUND,
            Self::RequestTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::InvalidJson => StatusCode::BAD_REQUEST,
            Self::RequestBudgetExhausted => StatusCode::TOO_MANY_REQUESTS,
            Self::TooManyConcurrentRequests => StatusCode::TOO_MANY_REQUESTS,
            Self::Upstream(_) | Self::ResponseBuild => StatusCode::BAD_GATEWAY,
        };
        (status, self.to_string()).into_response()
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

async fn relay_handler(
    State(service): State<Arc<SandboxModelRelayService>>,
    Path((relay_id, path)): Path<(String, String)>,
    request: Request<Body>,
) -> Response {
    let (parts, body) = request.into_parts();
    let bearer = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default()
        .to_string();
    service
        .relay(&relay_id, &bearer, parts.method, &path, parts.headers, body)
        .await
        .unwrap_or_else(IntoResponse::into_response)
}

pub fn sandbox_model_relay_routes() -> Router<Arc<SandboxModelRelayService>> {
    Router::new().route(
        "/ai/sandbox-models/{relay_id}/{*path}",
        any(relay_handler).layer(DefaultBodyLimit::max(MAX_MODEL_REQUEST_BYTES)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_debug_redacts_bearer() {
        let relay = SandboxModelRelay {
            base_url: "https://example.test/api/ai/sandbox-models/id".to_string(),
            bearer: "tmodel_secret".to_string(),
        };
        let debug = format!("{relay:?}");
        assert!(!debug.contains("tmodel_secret"));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn guard_invalidates_capability() {
        let service = SandboxModelRelayService::new().unwrap();
        let credentials = SandboxHarnessCredentials::anthropic_api_key(
            "upstream-secret",
            "https://temps.example.test",
        );
        let (relay, guard) = service
            .register(
                "claude_cli",
                7,
                Some("claude-sonnet-5"),
                credentials,
                "http://sandbox-relay.test/.temps/model-relay",
                Duration::from_secs(60),
            )
            .unwrap();
        assert_eq!(service.entries.lock().unwrap().len(), 1);
        assert!(relay.bearer.starts_with("tmodel_"));
        drop(guard);
        assert!(service.entries.lock().unwrap().is_empty());
    }

    #[test]
    fn unsupported_harness_fails_closed() {
        let service = SandboxModelRelayService::new().unwrap();
        let credentials = SandboxHarnessCredentials::anthropic_api_key(
            "upstream-secret",
            "https://temps.example.test",
        );
        let result = service.register(
            "codex_cli",
            7,
            None,
            credentials,
            "http://sandbox-relay.test/.temps/model-relay",
            Duration::from_secs(60),
        );
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn relay_rejects_wrong_bearer_before_contacting_upstream() {
        let service = SandboxModelRelayService::new().unwrap();
        let credentials = SandboxHarnessCredentials::anthropic_api_key(
            "upstream-secret",
            "https://temps.example.test",
        );
        let (relay, _guard) = service
            .register(
                "claude_cli",
                7,
                None,
                credentials,
                "http://sandbox-relay.test/.temps/model-relay",
                Duration::from_secs(60),
            )
            .unwrap();
        let relay_id = relay.base_url.rsplit('/').next().unwrap();

        let result = service
            .relay(
                relay_id,
                "wrong-token",
                Method::POST,
                "v1/messages",
                HeaderMap::new(),
                Body::empty(),
            )
            .await;

        assert!(matches!(result, Err(RelayError::Unauthorized)));
    }

    #[tokio::test]
    async fn relay_rejects_non_anthropic_paths_before_contacting_upstream() {
        let service = SandboxModelRelayService::new().unwrap();
        let credentials = SandboxHarnessCredentials::anthropic_api_key(
            "upstream-secret",
            "https://temps.example.test",
        );
        let (relay, _guard) = service
            .register(
                "claude_cli",
                7,
                None,
                credentials,
                "http://sandbox-relay.test/.temps/model-relay",
                Duration::from_secs(60),
            )
            .unwrap();
        let relay_id = relay.base_url.rsplit('/').next().unwrap();

        let result = service
            .relay(
                relay_id,
                &relay.bearer,
                Method::POST,
                "v1/organizations",
                HeaderMap::new(),
                Body::empty(),
            )
            .await;

        assert!(matches!(result, Err(RelayError::PathNotAllowed)));
    }

    #[tokio::test]
    async fn expired_relay_capability_fails_closed() {
        let service = SandboxModelRelayService::new().unwrap();
        let credentials = SandboxHarnessCredentials::anthropic_api_key(
            "upstream-secret",
            "https://temps.example.test",
        );
        let (relay, _guard) = service
            .register(
                "claude_cli",
                7,
                None,
                credentials,
                "http://sandbox-relay.test/.temps/model-relay",
                Duration::ZERO,
            )
            .unwrap();
        let relay_id = relay.base_url.rsplit('/').next().unwrap();

        let result = service
            .relay(
                relay_id,
                &relay.bearer,
                Method::POST,
                "v1/messages",
                HeaderMap::new(),
                Body::empty(),
            )
            .await;

        assert!(matches!(result, Err(RelayError::Unauthorized)));
    }

    #[test]
    fn model_request_is_pinned_and_output_tokens_are_clamped() {
        let normalized = normalize_model_request(
            br#"{"model":"other","max_tokens":999999,"messages":[]}"#,
            "v1/messages",
            "claude-sonnet-5",
        )
        .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&normalized).unwrap();

        assert_eq!(payload["model"], "claude-sonnet-5");
        assert_eq!(payload["max_tokens"], MAX_OUTPUT_TOKENS_PER_REQUEST);
    }

    #[test]
    fn relay_resolves_default_and_aliases_to_concrete_models() {
        assert_eq!(resolve_claude_model(None).unwrap(), "claude-sonnet-5");
        assert_eq!(resolve_claude_model(Some("opus")).unwrap(), "claude-opus-5");
        assert!(resolve_claude_model(Some("unbounded-provider-model")).is_err());
    }

    #[test]
    fn relay_uses_the_sandbox_visible_base_instead_of_the_private_control_plane() {
        let service = SandboxModelRelayService::new().unwrap();
        let credentials = SandboxHarnessCredentials::anthropic_api_key(
            "upstream-secret",
            "http://host.docker.internal:8080",
        );
        let (relay, _guard) = service
            .register(
                "claude_cli",
                7,
                None,
                credentials,
                "http://temps-sandbox-egress-proxy:3128/.temps/model-relay",
                Duration::from_secs(60),
            )
            .unwrap();

        assert!(relay
            .base_url
            .starts_with("http://temps-sandbox-egress-proxy:3128/.temps/model-relay/"));
        assert!(!relay.base_url.contains("host.docker.internal"));
    }
}
