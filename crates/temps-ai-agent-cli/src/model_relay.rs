// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Turn-scoped model relay for sandboxed development harnesses.
//!
//! The harness receives an opaque, short-lived bearer and a provider-compatible
//! base URL. The real provider credential stays in this process and is attached
//! only after the relay has authenticated the capability and allowlisted the
//! upstream method and path.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
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
const MAX_MODEL_RESPONSE_BYTES_PER_REQUEST: u64 = 16 * 1024 * 1024;
const MAX_MODEL_RESPONSE_BYTES_PER_TURN: u64 = 64 * 1024 * 1024;
const MODEL_REQUEST_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const ANTHROPIC_REQUEST_HEADERS: &[&str] = &[
    "accept",
    "anthropic-beta",
    "anthropic-version",
    "content-type",
    "user-agent",
];
const CODEX_REQUEST_HEADERS: &[&str] = &[
    "accept",
    "content-type",
    "openai-beta",
    "originator",
    "session-id",
    "thread-id",
    "traceparent",
    "user-agent",
    "version",
    "x-client-request-id",
    "x-codex-parent-thread-id",
    "x-codex-turn-metadata",
    "x-codex-turn-state",
    "x-codex-window-id",
    "x-codex-window-number",
    "x-openai-subagent",
];
const ALLOWED_RESPONSE_HEADERS: &[&str] = &[
    "content-type",
    "etag",
    "request-id",
    "retry-after",
    "x-codex-turn-state",
    "x-should-retry",
];
const ANTHROPIC_API_BASE_URL: &str = "https://api.anthropic.com";
const OPENAI_API_BASE_URL: &str = "https://api.openai.com/v1";
const CODEX_CHATGPT_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

/// Provider credential resolved for one active turn. Relay credentials remain
/// host-only; validated OpenCode auth is handed to its private runtime store
/// because the CLI owns OAuth refresh rotation.
/// Deliberately has no `Debug`, `Clone`, or serialization implementation.
pub enum SandboxProviderCredential {
    AnthropicApiKey(String),
    ClaudeOauthToken(String),
    OpenAiApiKey(String),
    CodexChatGpt {
        access_token: String,
        account_id: String,
    },
    OpenCodeAuthJson {
        contents: Vec<u8>,
        providers: Vec<String>,
    },
}

/// Material resolved immediately before a sandbox turn. The control-plane URL
/// is not a secret; the provider credential is consumed into the relay and is
/// never exposed through the workspace, prompt, logs, or an API response.
pub struct SandboxHarnessCredentials {
    pub(crate) provider_credential: SandboxProviderCredential,
    pub(crate) internal_api_url: String,
}

impl SandboxHarnessCredentials {
    pub(crate) fn redaction_values(&self) -> Vec<String> {
        match &self.provider_credential {
            SandboxProviderCredential::OpenCodeAuthJson { contents, .. } => {
                crate::service::native_opencode_redaction_values(contents)
            }
            _ => Vec::new(),
        }
    }

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

    pub fn openai_api_key(value: impl Into<String>, internal_api_url: impl Into<String>) -> Self {
        Self {
            provider_credential: SandboxProviderCredential::OpenAiApiKey(value.into()),
            internal_api_url: internal_api_url.into(),
        }
    }

    pub fn codex_chatgpt(
        access_token: impl Into<String>,
        account_id: impl Into<String>,
        internal_api_url: impl Into<String>,
    ) -> Self {
        Self {
            provider_credential: SandboxProviderCredential::CodexChatGpt {
                access_token: access_token.into(),
                account_id: account_id.into(),
            },
            internal_api_url: internal_api_url.into(),
        }
    }

    pub fn opencode_auth_json(
        contents: Vec<u8>,
        providers: Vec<String>,
        internal_api_url: impl Into<String>,
    ) -> Self {
        Self {
            provider_credential: SandboxProviderCredential::OpenCodeAuthJson {
                contents,
                providers,
            },
            internal_api_url: internal_api_url.into(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SandboxModelRelay {
    pub base_url: String,
    pub bearer: String,
    pub provider_id: Option<&'static str>,
    pub native_opencode_auth: Option<(Vec<u8>, Vec<String>)>,
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
    selected_model: Option<String>,
    remaining_requests: Arc<AtomicU32>,
    remaining_response_bytes: Arc<AtomicU64>,
    request_slot: Arc<tokio::sync::Semaphore>,
    successful_model_catalog_request: Arc<AtomicBool>,
    revocation: tokio::sync::watch::Sender<bool>,
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
        if relay_base_url.trim().is_empty() {
            return Err(AiError::Provider {
                purpose: "chat.application.model_relay".to_string(),
                reason: "the sandbox model relay base URL is empty".to_string(),
            });
        }
        let selected_model = match provider {
            "claude_cli" => Some(resolve_claude_model(selected_model)?),
            "codex_cli" => selected_model
                .map(str::trim)
                .filter(|model| !model.is_empty() && *model != "default")
                .map(str::to_string),
            "opencode" => match &credentials.provider_credential {
                SandboxProviderCredential::OpenCodeAuthJson { .. } => selected_model
                    .map(str::trim)
                    .filter(|model| !model.is_empty() && *model != "default")
                    .map(|model| {
                        resolve_opencode_model(Some(model), &credentials.provider_credential)
                    })
                    .transpose()?,
                _ => Some(resolve_opencode_model(
                    selected_model,
                    &credentials.provider_credential,
                )?),
            },
            other => {
                return Err(AiError::Provider {
                    purpose: "chat.application.model_relay".to_string(),
                    reason: format!(
                        "secure model relay is not implemented for development harness '{other}'"
                    ),
                })
            }
        };
        validate_provider_credential(provider, &credentials.provider_credential)?;
        let relay_provider_id = if provider == "opencode" {
            match &credentials.provider_credential {
                SandboxProviderCredential::AnthropicApiKey(_) => Some("anthropic"),
                SandboxProviderCredential::OpenAiApiKey(_) => Some("openai"),
                _ => None,
            }
        } else {
            None
        };
        let native_opencode_auth = match &credentials.provider_credential {
            SandboxProviderCredential::OpenCodeAuthJson {
                contents,
                providers,
            } => Some((contents.clone(), providers.clone())),
            _ => None,
        };
        let relay_id = uuid::Uuid::new_v4().simple().to_string();
        let bearer = format!(
            "tmodel_{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let (revocation, _) = tokio::sync::watch::channel(false);
        let successful_model_catalog_request = Arc::new(AtomicBool::new(false));
        let entry = RelayEntry {
            bearer: bearer.clone(),
            credential: credentials.provider_credential,
            principal_id,
            selected_model,
            remaining_requests: Arc::new(AtomicU32::new(MAX_MODEL_REQUESTS_PER_TURN)),
            remaining_response_bytes: Arc::new(AtomicU64::new(MAX_MODEL_RESPONSE_BYTES_PER_TURN)),
            request_slot: Arc::new(tokio::sync::Semaphore::new(
                MAX_CONCURRENT_MODEL_REQUESTS_PER_TURN,
            )),
            successful_model_catalog_request: successful_model_catalog_request.clone(),
            revocation: revocation.clone(),
            expires_at: Instant::now() + lifetime,
        };
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(relay_id.clone(), entry);
        let relay = SandboxModelRelay {
            base_url: format!("{}/{relay_id}", relay_base_url.trim_end_matches('/')),
            bearer,
            provider_id: relay_provider_id,
            native_opencode_auth,
        };
        let guard = SandboxModelRelayGuard {
            entries: self.entries.clone(),
            relay_id,
            successful_model_catalog_request,
            revocation,
        };
        Ok((relay, guard))
    }

    #[allow(clippy::too_many_arguments)]
    async fn relay(
        &self,
        relay_id: &str,
        bearer: &str,
        method: Method,
        path: &str,
        query: Option<&str>,
        headers: HeaderMap,
        body: Body,
    ) -> Result<Response, RelayError> {
        let normalized_path = path.trim_start_matches('/');
        let (
            credential,
            expires_at,
            selected_model,
            remaining_requests,
            remaining_response_bytes,
            request_slot,
            successful_model_catalog_request,
            mut revocation,
        ) = {
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
                SandboxProviderCredential::OpenAiApiKey(value) => {
                    RequestCredential::OpenAiApiKey(value.clone())
                }
                SandboxProviderCredential::CodexChatGpt {
                    access_token,
                    account_id,
                } => RequestCredential::CodexChatGpt {
                    access_token: access_token.clone(),
                    account_id: account_id.clone(),
                },
                SandboxProviderCredential::OpenCodeAuthJson { .. } => {
                    return Err(RelayError::CredentialMismatch)
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
                entry.remaining_response_bytes.clone(),
                entry.request_slot.clone(),
                entry.successful_model_catalog_request.clone(),
                entry.revocation.subscribe(),
            )
        };
        if expires_at <= Instant::now() {
            return Err(RelayError::Unauthorized);
        }
        let request_kind = classify_request(&credential, &method, normalized_path, query)?;
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
        let remaining_lifetime = expires_at.saturating_duration_since(Instant::now());
        if remaining_lifetime.is_zero() {
            return Err(RelayError::Unauthorized);
        }
        let request_deadline =
            tokio::time::Instant::now() + remaining_lifetime.min(MODEL_REQUEST_TIMEOUT);
        let bytes = tokio::select! {
            bytes = to_bytes(body, MAX_MODEL_REQUEST_BYTES) => {
                bytes.map_err(|_| RelayError::RequestTooLarge)?
            }
            changed = revocation.changed() => {
                let _ = changed;
                return Err(RelayError::Unauthorized);
            }
            _ = tokio::time::sleep_until(request_deadline) => {
                return Err(RelayError::Unauthorized);
            }
        };
        let bytes = match request_kind {
            RelayRequestKind::Anthropic | RelayRequestKind::CodexResponse => {
                normalize_model_request(
                    &bytes,
                    request_kind,
                    match request_kind {
                        RelayRequestKind::Anthropic => normalized_path == "v1/messages",
                        _ => !matches!(credential, RequestCredential::CodexChatGpt { .. }),
                    },
                    selected_model
                        .as_deref()
                        .ok_or(RelayError::ModelNotSelected)?,
                )?
            }
            RelayRequestKind::CodexModels => bytes.to_vec(),
        };
        let url = upstream_url(&request_kind, &credential, normalized_path, query)?;
        let mut request = self.client.request(method, url).body(bytes);
        let allowed_headers = match request_kind {
            RelayRequestKind::Anthropic => ANTHROPIC_REQUEST_HEADERS,
            RelayRequestKind::CodexResponse | RelayRequestKind::CodexModels => {
                CODEX_REQUEST_HEADERS
            }
        };
        for name in allowed_headers {
            if let Some(value) = headers.get(*name) {
                request = request.header(*name, value);
            }
        }
        request = match credential {
            RequestCredential::AnthropicApiKey(value) => request.header("x-api-key", value),
            RequestCredential::ClaudeOauthToken(value) => {
                request.header(header::AUTHORIZATION, format!("Bearer {value}"))
            }
            RequestCredential::OpenAiApiKey(value) => {
                request.header(header::AUTHORIZATION, format!("Bearer {value}"))
            }
            RequestCredential::CodexChatGpt {
                access_token,
                account_id,
            } => request
                .header(header::AUTHORIZATION, format!("Bearer {access_token}"))
                .header("chatgpt-account-id", account_id),
        };
        let upstream = tokio::select! {
            response = request.send() => response.map_err(RelayError::Upstream)?,
            changed = revocation.changed() => {
                let _ = changed;
                return Err(RelayError::Unauthorized);
            }
            _ = tokio::time::sleep_until(request_deadline) => {
                return Err(RelayError::Unauthorized);
            }
        };
        let status = upstream.status();
        if matches!(request_kind, RelayRequestKind::CodexModels) && status.is_success() {
            successful_model_catalog_request.store(true, Ordering::Release);
        }
        let response_headers = upstream.headers().clone();
        let upstream_stream: std::pin::Pin<
            Box<dyn Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send>,
        > = Box::pin(upstream.bytes_stream());
        let stream = futures::stream::unfold(
            (
                upstream_stream,
                request_permit,
                revocation,
                request_deadline,
                remaining_response_bytes,
                MAX_MODEL_RESPONSE_BYTES_PER_REQUEST,
                false,
            ),
            |(mut upstream, permit, mut revocation, deadline, turn_bytes, response_bytes, done)| async move {
                if done || *revocation.borrow() || tokio::time::Instant::now() >= deadline {
                    return None;
                }
                let chunk = tokio::select! {
                    chunk = upstream.next() => chunk,
                    changed = revocation.changed() => {
                        let _ = changed;
                        None
                    }
                    _ = tokio::time::sleep_until(deadline) => None,
                }?;
                let (chunk, next_response_bytes, next_done) = match chunk {
                    Ok(bytes) => {
                        let size = bytes.len() as u64;
                        let turn_budget_available = turn_bytes
                            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                                remaining.checked_sub(size)
                            })
                            .is_ok();
                        if size > response_bytes || !turn_budget_available {
                            (
                                Err(std::io::Error::other(
                                    "model relay response exceeded its byte budget",
                                )),
                                0,
                                true,
                            )
                        } else {
                            (Ok(bytes), response_bytes - size, false)
                        }
                    }
                    Err(error) => (
                        Err(std::io::Error::other(format!(
                            "model relay stream: {error}"
                        ))),
                        response_bytes,
                        true,
                    ),
                };
                Some((
                    chunk,
                    (
                        upstream,
                        permit,
                        revocation,
                        deadline,
                        turn_bytes,
                        next_response_bytes,
                        next_done,
                    ),
                ))
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
    request_kind: RelayRequestKind,
    supports_response_options: bool,
    selected_model: &str,
) -> Result<Vec<u8>, RelayError> {
    let mut payload =
        serde_json::from_slice::<serde_json::Value>(bytes).map_err(|_| RelayError::InvalidJson)?;
    let object = payload.as_object_mut().ok_or(RelayError::InvalidJson)?;
    object.insert(
        "model".to_string(),
        serde_json::Value::String(selected_model.to_string()),
    );
    let output_limit_field = match request_kind {
        RelayRequestKind::Anthropic if supports_response_options => Some("max_tokens"),
        RelayRequestKind::Anthropic => None,
        RelayRequestKind::CodexResponse if supports_response_options => Some("max_output_tokens"),
        RelayRequestKind::CodexResponse => None,
        RelayRequestKind::CodexModels => None,
    };
    if let Some(field) = output_limit_field {
        let max_tokens = object
            .get(field)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(MAX_OUTPUT_TOKENS_PER_REQUEST)
            .min(MAX_OUTPUT_TOKENS_PER_REQUEST);
        object.insert(
            field.to_string(),
            serde_json::Value::Number(max_tokens.into()),
        );
    }
    if matches!(request_kind, RelayRequestKind::CodexResponse) {
        if supports_response_options {
            object.insert("background".to_string(), serde_json::Value::Bool(false));
        } else {
            // ChatGPT's Codex endpoint rejects these Responses API options.
            // It runs foreground requests; relay byte/time/request budgets
            // remain enforced independently of upstream token-limit support.
            object.remove("max_output_tokens");
            object.remove("background");
        }
    }
    serde_json::to_vec(&payload).map_err(|_| RelayError::InvalidJson)
}

fn validate_provider_credential(
    provider: &str,
    credential: &SandboxProviderCredential,
) -> Result<(), AiError> {
    let compatible = matches!(
        (provider, credential),
        (
            "claude_cli",
            SandboxProviderCredential::AnthropicApiKey(_)
                | SandboxProviderCredential::ClaudeOauthToken(_)
        ) | (
            "codex_cli",
            SandboxProviderCredential::OpenAiApiKey(_)
                | SandboxProviderCredential::CodexChatGpt { .. }
        ) | (
            "opencode",
            SandboxProviderCredential::AnthropicApiKey(_)
                | SandboxProviderCredential::OpenAiApiKey(_)
                | SandboxProviderCredential::OpenCodeAuthJson { .. }
        )
    );
    if compatible {
        Ok(())
    } else {
        Err(AiError::Provider {
            purpose: "chat.application.model_relay".to_string(),
            reason: format!(
                "the saved credential is not compatible with development harness '{provider}'"
            ),
        })
    }
}

fn resolve_opencode_model(
    selected_model: Option<&str>,
    credential: &SandboxProviderCredential,
) -> Result<String, AiError> {
    let model = selected_model
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "default")
        .ok_or_else(|| AiError::Provider {
            purpose: "chat.application.model_relay".to_string(),
            reason: "OpenCode sandbox execution requires an explicit provider/model selection"
                .to_string(),
        })?;
    let expected =
        match credential {
            SandboxProviderCredential::AnthropicApiKey(_) => "anthropic/",
            SandboxProviderCredential::OpenAiApiKey(_) => "openai/",
            SandboxProviderCredential::OpenCodeAuthJson { providers, .. } => {
                let prefix = model
                    .split_once('/')
                    .map(|entry| entry.0)
                    .unwrap_or_default();
                if providers.iter().any(|provider| provider == prefix) {
                    return Ok(model.to_string());
                }
                return Err(AiError::Provider {
                    purpose: "chat.application.model_relay".to_string(),
                    reason: format!(
                        "OpenCode model '{model}' does not match any saved native auth provider"
                    ),
                });
            }
            _ => return Err(AiError::Provider {
                purpose: "chat.application.model_relay".to_string(),
                reason:
                    "OpenCode sandbox execution supports only saved Anthropic or OpenAI API keys"
                        .to_string(),
            }),
        };
    if !model.starts_with(expected)
        || model.len() == expected.len()
        || model.contains(['?', '#', '\\'])
    {
        return Err(AiError::Provider {
            purpose: "chat.application.model_relay".to_string(),
            reason: format!(
                "OpenCode model '{model}' does not match the saved {} API-key provider",
                expected.trim_end_matches('/')
            ),
        });
    }
    Ok(model.to_string())
}

#[derive(Clone, Copy)]
enum RelayRequestKind {
    Anthropic,
    CodexResponse,
    CodexModels,
}

fn classify_request(
    credential: &RequestCredential,
    method: &Method,
    path: &str,
    query: Option<&str>,
) -> Result<RelayRequestKind, RelayError> {
    match credential {
        RequestCredential::AnthropicApiKey(_) | RequestCredential::ClaudeOauthToken(_) => {
            if method != Method::POST {
                return Err(RelayError::MethodNotAllowed);
            }
            if query.is_some_and(|query| query != "beta=true") {
                return Err(RelayError::QueryNotAllowed);
            }
            if matches!(path, "v1/messages" | "v1/messages/count_tokens") {
                Ok(RelayRequestKind::Anthropic)
            } else {
                Err(RelayError::PathNotAllowed)
            }
        }
        RequestCredential::OpenAiApiKey(_) | RequestCredential::CodexChatGpt { .. } => {
            match (method, path) {
                (&Method::POST, "responses") if query.is_none() => {
                    Ok(RelayRequestKind::CodexResponse)
                }
                (&Method::GET, "models") if valid_models_query(query) => {
                    Ok(RelayRequestKind::CodexModels)
                }
                (&Method::GET, "models") => Err(RelayError::QueryNotAllowed),
                (&Method::POST, _) | (&Method::GET, _) => Err(RelayError::PathNotAllowed),
                _ => Err(RelayError::MethodNotAllowed),
            }
        }
    }
}

fn valid_models_query(query: Option<&str>) -> bool {
    query.is_none_or(|query| {
        let Some(value) = query.strip_prefix("client_version=") else {
            return false;
        };
        !value.is_empty()
            && value.len() <= 64
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+')
            })
    })
}

fn upstream_url(
    request_kind: &RelayRequestKind,
    credential: &RequestCredential,
    path: &str,
    query: Option<&str>,
) -> Result<String, RelayError> {
    match request_kind {
        RelayRequestKind::Anthropic => match credential {
            RequestCredential::AnthropicApiKey(_) | RequestCredential::ClaudeOauthToken(_) => {
                Ok(query.map_or_else(
                    || format!("{ANTHROPIC_API_BASE_URL}/{path}"),
                    |query| format!("{ANTHROPIC_API_BASE_URL}/{path}?{query}"),
                ))
            }
            _ => Err(RelayError::CredentialMismatch),
        },
        RelayRequestKind::CodexResponse => {
            let base = match credential {
                RequestCredential::OpenAiApiKey(_) => OPENAI_API_BASE_URL,
                RequestCredential::CodexChatGpt { .. } => CODEX_CHATGPT_BASE_URL,
                _ => return Err(RelayError::CredentialMismatch),
            };
            Ok(format!("{base}/responses"))
        }
        RelayRequestKind::CodexModels => {
            let base = match credential {
                RequestCredential::OpenAiApiKey(_) => OPENAI_API_BASE_URL,
                RequestCredential::CodexChatGpt { .. } => CODEX_CHATGPT_BASE_URL,
                _ => return Err(RelayError::CredentialMismatch),
            };
            Ok(query.map_or_else(
                || format!("{base}/models"),
                |query| format!("{base}/models?{query}"),
            ))
        }
    }
}

fn resolve_claude_model(selected_model: Option<&str>) -> Result<String, AiError> {
    let model = match selected_model.map(str::trim) {
        None | Some("") | Some("default") | Some("sonnet") => "claude-sonnet-5",
        Some("opus") => "claude-opus-5",
        // `[1m]` values are account-aware Claude CLI context selectors, not
        // Anthropic Messages API model identifiers. The CLI keeps the selector
        // for its launch; the relay pins the upstream body to the corresponding
        // existing concrete family.
        Some("sonnet[1m]") => "claude-sonnet-5",
        Some("opus[1m]") => "claude-opus-5",
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
    OpenAiApiKey(String),
    CodexChatGpt {
        access_token: String,
        account_id: String,
    },
}

pub(crate) struct SandboxModelRelayGuard {
    entries: Arc<Mutex<HashMap<String, RelayEntry>>>,
    relay_id: String,
    successful_model_catalog_request: Arc<AtomicBool>,
    revocation: tokio::sync::watch::Sender<bool>,
}

impl SandboxModelRelayGuard {
    pub(crate) fn model_catalog_succeeded(&self) -> bool {
        self.successful_model_catalog_request
            .load(Ordering::Acquire)
    }
}

impl Drop for SandboxModelRelayGuard {
    fn drop(&mut self) {
        let _ = self.revocation.send(true);
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
    #[error("sandbox model relay method is not allowed for this path")]
    MethodNotAllowed,
    #[error("sandbox model relay path is not allowed")]
    PathNotAllowed,
    #[error("sandbox model relay query is not allowed")]
    QueryNotAllowed,
    #[error("sandbox model relay requires an authorized model selection")]
    ModelNotSelected,
    #[error("sandbox model relay credential does not match the allowed upstream")]
    CredentialMismatch,
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
            Self::QueryNotAllowed | Self::ModelNotSelected => StatusCode::BAD_REQUEST,
            Self::CredentialMismatch => StatusCode::BAD_GATEWAY,
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
    let query = parts.uri.query().map(str::to_string);
    service
        .relay(
            &relay_id,
            &bearer,
            parts.method,
            &path,
            query.as_deref(),
            parts.headers,
            body,
        )
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
            provider_id: None,
            native_opencode_auth: None,
        };
        let debug = format!("{relay:?}");
        assert!(!debug.contains("tmodel_secret"));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn opencode_model_must_match_the_server_held_api_key() {
        assert_eq!(
            resolve_opencode_model(
                Some("anthropic/claude-sonnet-4"),
                &SandboxProviderCredential::AnthropicApiKey("secret".into())
            )
            .unwrap(),
            "anthropic/claude-sonnet-4"
        );
        assert!(resolve_opencode_model(
            Some("openai/gpt-5"),
            &SandboxProviderCredential::AnthropicApiKey("secret".into())
        )
        .is_err());
        assert!(resolve_opencode_model(
            Some("anthropic/claude-sonnet-4"),
            &SandboxProviderCredential::ClaudeOauthToken("secret".into())
        )
        .is_err());
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
    fn consecutive_turns_for_the_same_principal_use_the_newly_resolved_credential() {
        let service = SandboxModelRelayService::new().unwrap();
        let (first_relay, first_guard) = service
            .register(
                "claude_cli",
                7,
                None,
                SandboxHarnessCredentials::claude_oauth_token(
                    "first-turn-token",
                    "https://temps.example.test",
                ),
                "http://sandbox-relay.test/.temps/model-relay",
                Duration::from_secs(60),
            )
            .unwrap();
        let first_relay_id = first_relay.base_url.rsplit('/').next().unwrap();

        {
            let entries = service.entries.lock().unwrap();
            assert!(matches!(
                &entries.get(first_relay_id).unwrap().credential,
                SandboxProviderCredential::ClaudeOauthToken(value)
                    if value == "first-turn-token"
            ));
        }
        drop(first_guard);

        let (second_relay, _second_guard) = service
            .register(
                "claude_cli",
                7,
                None,
                SandboxHarnessCredentials::claude_oauth_token(
                    "second-turn-token",
                    "https://temps.example.test",
                ),
                "http://sandbox-relay.test/.temps/model-relay",
                Duration::from_secs(60),
            )
            .unwrap();
        let second_relay_id = second_relay.base_url.rsplit('/').next().unwrap();
        let entries = service.entries.lock().unwrap();

        assert_ne!(first_relay_id, second_relay_id);
        assert!(!entries.contains_key(first_relay_id));
        assert!(matches!(
            &entries.get(second_relay_id).unwrap().credential,
            SandboxProviderCredential::ClaudeOauthToken(value)
                if value == "second-turn-token"
        ));
    }

    #[test]
    fn unsupported_harness_fails_closed() {
        let service = SandboxModelRelayService::new().unwrap();
        let credentials = SandboxHarnessCredentials::anthropic_api_key(
            "upstream-secret",
            "https://temps.example.test",
        );
        let result = service.register(
            "opencode",
            7,
            None,
            credentials,
            "http://sandbox-relay.test/.temps/model-relay",
            Duration::from_secs(60),
        );
        assert!(result.is_err());
    }

    #[test]
    fn codex_harness_accepts_only_codex_credentials() {
        let service = SandboxModelRelayService::new().unwrap();
        let credentials = SandboxHarnessCredentials::codex_chatgpt(
            "access-token",
            "account-id",
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
        assert!(result.is_ok());

        let wrong_credentials = SandboxHarnessCredentials::anthropic_api_key(
            "anthropic-key",
            "https://temps.example.test",
        );
        assert!(service
            .register(
                "codex_cli",
                7,
                None,
                wrong_credentials,
                "http://sandbox-relay.test/.temps/model-relay",
                Duration::from_secs(60),
            )
            .is_err());
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
                None,
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
                None,
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
                None,
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
            RelayRequestKind::Anthropic,
            true,
            "claude-sonnet-5",
        )
        .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&normalized).unwrap();

        assert_eq!(payload["model"], "claude-sonnet-5");
        assert_eq!(payload["max_tokens"], MAX_OUTPUT_TOKENS_PER_REQUEST);
    }

    #[test]
    fn codex_response_limits_are_enforced_and_background_work_is_disabled() {
        for requested in [
            serde_json::Value::Null,
            serde_json::json!(999999),
            serde_json::json!(1024),
        ] {
            let normalized = normalize_model_request(
                &serde_json::to_vec(&serde_json::json!({
                    "model": "unapproved", "max_output_tokens": requested, "background": true,
                }))
                .unwrap(),
                RelayRequestKind::CodexResponse,
                true,
                "gpt-5.6-sol",
            )
            .unwrap();
            let payload: serde_json::Value = serde_json::from_slice(&normalized).unwrap();
            assert_eq!(payload["model"], "gpt-5.6-sol");
            assert_eq!(payload["background"], false);
            assert_eq!(
                payload["max_output_tokens"],
                requested
                    .as_u64()
                    .unwrap_or(MAX_OUTPUT_TOKENS_PER_REQUEST)
                    .min(MAX_OUTPUT_TOKENS_PER_REQUEST)
            );
        }
    }

    #[test]
    fn chatgpt_codex_does_not_receive_unsupported_response_options() {
        let normalized = normalize_model_request(
            br#"{"model":"other","max_output_tokens":1000,"background":true}"#,
            RelayRequestKind::CodexResponse,
            false,
            "gpt-5.6-sol",
        )
        .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&normalized).unwrap();
        assert_eq!(payload["model"], "gpt-5.6-sol");
        assert!(payload.get("max_output_tokens").is_none());
        assert!(payload.get("background").is_none());
    }

    #[test]
    fn relay_resolves_default_and_aliases_to_concrete_models() {
        assert_eq!(resolve_claude_model(None).unwrap(), "claude-sonnet-5");
        assert_eq!(resolve_claude_model(Some("opus")).unwrap(), "claude-opus-5");
        assert_eq!(
            resolve_claude_model(Some("opus[1m]")).unwrap(),
            "claude-opus-5"
        );
        assert_eq!(
            resolve_claude_model(Some("sonnet[1m]")).unwrap(),
            "claude-sonnet-5"
        );
        assert!(resolve_claude_model(Some("unbounded-provider-model")).is_err());
    }

    #[test]
    fn claude_one_million_selector_never_reaches_the_upstream_model_field() {
        for (selector, concrete) in [
            ("opus[1m]", "claude-opus-5"),
            ("sonnet[1m]", "claude-sonnet-5"),
        ] {
            let selected = resolve_claude_model(Some(selector)).unwrap();
            let request = serde_json::json!({ "model": selector, "max_tokens": 1024 });
            let normalized = normalize_model_request(
                &serde_json::to_vec(&request).unwrap(),
                RelayRequestKind::Anthropic,
                true,
                &selected,
            )
            .unwrap();
            let payload: serde_json::Value = serde_json::from_slice(&normalized).unwrap();
            assert_eq!(payload["model"], concrete);
            assert_eq!(payload["max_tokens"], 1024);
            assert!(!normalized
                .windows(b"[1m]".len())
                .any(|part| part == b"[1m]"));
        }
    }

    #[test]
    fn anthropic_relay_allows_only_the_cli_beta_query() {
        let credential = RequestCredential::ClaudeOauthToken("access-token".to_string());
        for path in ["v1/messages", "v1/messages/count_tokens"] {
            let kind =
                classify_request(&credential, &Method::POST, path, Some("beta=true")).unwrap();
            assert!(matches!(kind, RelayRequestKind::Anthropic));
            assert_eq!(
                upstream_url(&kind, &credential, path, Some("beta=true")).unwrap(),
                format!("{ANTHROPIC_API_BASE_URL}/{path}?beta=true")
            );
        }

        for query in [
            "beta=false",
            "beta=true&beta=true",
            "other=true",
            "beta=true&other=true",
        ] {
            assert!(matches!(
                classify_request(&credential, &Method::POST, "v1/messages", Some(query)),
                Err(RelayError::QueryNotAllowed)
            ));
        }

        let codex = RequestCredential::OpenAiApiKey("api-key".to_string());
        assert!(matches!(
            classify_request(&codex, &Method::POST, "responses", Some("beta=true")),
            Err(RelayError::PathNotAllowed)
        ));
    }

    #[test]
    fn codex_relay_allows_only_models_and_responses_routes() {
        let credential = RequestCredential::CodexChatGpt {
            access_token: "access-token".to_string(),
            account_id: "account-id".to_string(),
        };
        assert!(matches!(
            classify_request(
                &credential,
                &Method::GET,
                "models",
                Some("client_version=0.153.4")
            ),
            Ok(RelayRequestKind::CodexModels)
        ));
        assert!(matches!(
            classify_request(&credential, &Method::POST, "responses", None),
            Ok(RelayRequestKind::CodexResponse)
        ));
        assert!(matches!(
            classify_request(&credential, &Method::GET, "accounts", None),
            Err(RelayError::PathNotAllowed)
        ));
        assert!(matches!(
            classify_request(
                &credential,
                &Method::GET,
                "models",
                Some("redirect=https://attacker.test")
            ),
            Err(RelayError::QueryNotAllowed)
        ));
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
