// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use bytes::Bytes;
use std::sync::Arc;
use std::time::Instant;
use temps_auth::permission_guard;
use temps_auth::RequireAuth;
use temps_core::problemdetails::Problem;
use tracing::{debug, error, info};
use utoipa::OpenApi;

use super::sse_usage::{SseUsage, UsageKind};
use crate::error::AiGatewayError;
use crate::handlers::types::AiGatewayAppState;
use crate::services::gateway_service::{ByokOverride, CredentialType};
use crate::services::usage_service::AiRequestContext;
use crate::services::UsageService;
use crate::types::*;

/// Extract BYOK overrides from request headers.
pub(crate) fn extract_byok(headers: &HeaderMap) -> ByokOverride {
    ByokOverride {
        api_key: headers
            .get("x-provider-api-key")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string()),
        base_url: headers
            .get("x-provider-base-url")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string()),
        system_key_id: None,
    }
}

/// Extract AI request context (conversation, tags, trace) from request headers.
pub(crate) fn extract_ai_context(headers: &HeaderMap) -> AiRequestContext {
    AiRequestContext {
        conversation_id: headers
            .get("x-conversation-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string()),
        tags: headers
            .get("x-tags")
            .and_then(|v| v.to_str().ok())
            .map(|s| {
                s.split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        request_id: headers
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string()),
        trace_id: headers
            .get("traceparent")
            .and_then(|v| v.to_str().ok())
            .and_then(|tp| {
                // W3C traceparent: {version}-{trace-id}-{parent-id}-{flags}
                tp.split('-').nth(1).map(String::from)
            }),
    }
}

pub(crate) fn credential_type_str(ct: CredentialType) -> &'static str {
    match ct {
        CredentialType::System => "system",
        CredentialType::Byok => "byok",
    }
}

pub(crate) fn reject_deployment_token_base_url(
    auth: &temps_auth::AuthContext,
    byok: &ByokOverride,
) -> Option<AiGatewayError> {
    if auth.is_deployment_token() && byok.base_url.is_some() {
        return Some(AiGatewayError::InvalidProviderUrl {
            reason: "X-Provider-Base-URL is not allowed for deployment tokens".to_string(),
        });
    }

    None
}

// ============================================================================
// Streaming usage extraction
// ============================================================================

#[cfg(test)]
fn extract_usage_from_sse_line(line: &str) -> Option<(i64, i64)> {
    extract_line(line, UsageKind::ChatCompletions)
}

#[cfg(test)]
fn extract_responses_usage_from_sse_line(line: &str) -> Option<(i64, i64)> {
    extract_line(line, UsageKind::Responses)
}

#[cfg(test)]
fn extract_line(line: &str, kind: UsageKind) -> Option<(i64, i64)> {
    let mut parser = SseUsage::new(kind);
    parser.push(line.as_bytes(), |_, _| {});
    parser.finish_line()
}

/// Wraps an upstream SSE byte stream to transparently intercept usage data
/// from the final chunks, then logs it after the stream ends. Only usage fields
/// and bounded JSON structure are retained, even for very large output events.
#[allow(clippy::too_many_arguments)]
pub(super) fn wrap_stream_with_usage_tracking(
    inner: std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<Bytes, AiGatewayError>> + Send>,
    >,
    usage_service: Arc<UsageService>,
    user_id: Option<i32>,
    provider: String,
    model: String,
    start: Instant,
    is_byok: bool,
    ai_context: AiRequestContext,
    usage_kind: UsageKind,
) -> std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<Bytes, AiGatewayError>> + Send>> {
    use tokio_stream::StreamExt;

    let prompt_tokens = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let completion_tokens = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let pt = prompt_tokens.clone();
    let ct = completion_tokens.clone();
    let mapped = async_stream::stream! {
        let mut inner = inner;
        let mut parser = SseUsage::new(usage_kind);
        while let Some(result) = inner.next().await {
            if let Ok(ref bytes) = result {
                parser.push(bytes, |input, output| {
                    pt.store(input, std::sync::atomic::Ordering::Relaxed);
                    ct.store(output, std::sync::atomic::Ordering::Relaxed);
                });
            }
            yield result;
        }
        // Providers normally terminate SSE lines with LF. Retain accounting
        // for a complete final JSON event even when the last LF is omitted.
        if let Some((input, output)) = parser.finish_line() {
            pt.store(input, std::sync::atomic::Ordering::Relaxed);
            ct.store(output, std::sync::atomic::Ordering::Relaxed);
        }
    };

    // When the stream ends, log usage
    let pt_final = prompt_tokens;
    let ct_final = completion_tokens;

    let with_cleanup = StreamWithCleanup {
        inner: Box::pin(mapped),
        on_drop: Some(Box::new(move || {
            let input = pt_final.load(std::sync::atomic::Ordering::Relaxed);
            let output = ct_final.load(std::sync::atomic::Ordering::Relaxed);
            let latency_ms = start.elapsed().as_millis() as i32;

            if input > 0 || output > 0 {
                tokio::spawn(async move {
                    if let Err(e) = usage_service
                        .log_usage_with_context(
                            user_id,
                            &provider,
                            &model,
                            input,
                            output,
                            latency_ms,
                            0,
                            200,
                            true, // streaming
                            is_byok,
                            &ai_context,
                        )
                        .await
                    {
                        error!(error = %e, "Failed to log streaming AI usage");
                    }
                });
            } else {
                debug!(
                    model = model,
                    "Streaming request completed without usage data"
                );
            }
        })),
    };

    Box::pin(with_cleanup)
}

/// A stream wrapper that calls a cleanup function when the stream is dropped/exhausted.
struct StreamWithCleanup {
    inner:
        std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<Bytes, AiGatewayError>> + Send>>,
    on_drop: Option<Box<dyn FnOnce() + Send>>,
}

impl tokio_stream::Stream for StreamWithCleanup {
    type Item = Result<Bytes, AiGatewayError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let result = self.inner.as_mut().poll_next(cx);
        if let std::task::Poll::Ready(None) = &result {
            // Stream exhausted — fire cleanup
            if let Some(f) = self.on_drop.take() {
                f();
            }
        }
        result
    }
}

impl Drop for StreamWithCleanup {
    fn drop(&mut self) {
        // Also fire cleanup on early drop (client disconnect)
        if let Some(f) = self.on_drop.take() {
            f();
        }
    }
}

// ============================================================================
// OpenAPI schema
// ============================================================================

#[derive(OpenApi)]
#[openapi(
    paths(chat_completions, list_models, embeddings),
    components(schemas(
        ChatCompletionRequest,
        ChatCompletionResponse,
        ChatCompletionChoice,
        ChatMessage,
        MessageContent,
        ContentPart,
        StopSequence,
        UsageInfo,
        ModelListResponse,
        ModelInfo,
        EmbeddingRequest,
        EmbeddingInput,
        EmbeddingResponse,
        EmbeddingData,
        EmbeddingUsage,
        OpenAiErrorResponse,
        OpenAiError,
    )),
    info(
        title = "AI Gateway API",
        description = "OpenAI-compatible AI gateway that routes requests to configured providers",
        version = "1.0.0"
    ),
    tags(
        (name = "AI Gateway", description = "OpenAI-compatible chat, embeddings, and model endpoints")
    )
)]
pub struct AiGatewayApiDoc;

pub fn configure_gateway_routes() -> Router<Arc<AiGatewayAppState>> {
    Router::new()
        .route("/ai/v1/chat/completions", post(chat_completions))
        .route("/ai/v1/models", get(list_models))
        .route("/ai/v1/embeddings", post(embeddings))
}

// ============================================================================
// Error conversion to OpenAI-compatible JSON errors
// ============================================================================

pub(crate) fn error_to_response(error: AiGatewayError) -> impl IntoResponse {
    let (status, body) = match &error {
        AiGatewayError::UploadTooLarge { .. } => (
            StatusCode::PAYLOAD_TOO_LARGE,
            OpenAiErrorResponse::invalid_request(error.to_string(), "file_too_large"),
        ),
        AiGatewayError::UploadCapacity => (
            StatusCode::TOO_MANY_REQUESTS,
            OpenAiErrorResponse::invalid_request(error.to_string(), "upload_capacity"),
        ),
        AiGatewayError::ModelNotFound { model } => (
            StatusCode::NOT_FOUND,
            OpenAiErrorResponse::invalid_request(
                format!(
                    "Model '{}' not found. No provider configured for this model.",
                    model
                ),
                "model_not_found",
            ),
        ),
        AiGatewayError::ProviderNotConfigured { provider } => (
            StatusCode::NOT_FOUND,
            OpenAiErrorResponse::invalid_request(
                format!(
                    "Provider '{}' requires an API key. Configure it in Settings -> AI Gateway.",
                    provider
                ),
                "model_not_found",
            ),
        ),
        AiGatewayError::ModelNotAllowed { model, .. } => (
            StatusCode::FORBIDDEN,
            OpenAiErrorResponse::invalid_request(
                format!("Model '{}' is not allowed for this scope.", model),
                "model_not_allowed",
            ),
        ),
        AiGatewayError::Validation { message } => (
            StatusCode::BAD_REQUEST,
            OpenAiErrorResponse::invalid_request(message, "invalid_request"),
        ),
        AiGatewayError::UpstreamError {
            status, message, ..
        } => {
            let http_status = StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY);
            (
                http_status,
                OpenAiErrorResponse::server_error(message, "upstream_error"),
            )
        }
        AiGatewayError::InvalidProviderUrl { reason } => (
            StatusCode::BAD_REQUEST,
            OpenAiErrorResponse::invalid_request(
                format!("Invalid X-Provider-Base-URL: {}", reason),
                "invalid_provider_url",
            ),
        ),
        AiGatewayError::UnsupportedEndpoint { .. } => (
            StatusCode::BAD_REQUEST,
            OpenAiErrorResponse::invalid_request(error.to_string(), "unsupported_endpoint"),
        ),
        AiGatewayError::ObjectNotFound { .. } => (
            StatusCode::NOT_FOUND,
            OpenAiErrorResponse::invalid_request(error.to_string(), "not_found"),
        ),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            OpenAiErrorResponse::server_error(error.to_string(), "internal_error"),
        ),
    };

    (status, Json(body))
}

// ============================================================================
// Handlers
// ============================================================================

#[utoipa::path(
    tag = "AI Gateway",
    post,
    path = "/ai/v1/chat/completions",
    request_body = ChatCompletionRequest,
    responses(
        (status = 200, description = "Chat completion response", body = ChatCompletionResponse),
        (status = 400, description = "Invalid request", body = OpenAiErrorResponse),
        (status = 401, description = "Unauthorized", body = OpenAiErrorResponse),
        (status = 404, description = "Model not found", body = OpenAiErrorResponse),
        (status = 500, description = "Internal error", body = OpenAiErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
async fn chat_completions(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    headers: HeaderMap,
    Json(request): Json<ChatCompletionRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayExecute);

    if request.model.is_empty() {
        return Ok(error_to_response(AiGatewayError::Validation {
            message: "model field is required".to_string(),
        })
        .into_response());
    }

    if request.messages.is_empty() {
        return Ok(error_to_response(AiGatewayError::Validation {
            message: "messages array cannot be empty".to_string(),
        })
        .into_response());
    }

    // Validate message count to prevent abuse
    if request.messages.len() > 500 {
        return Ok(error_to_response(AiGatewayError::Validation {
            message: format!(
                "messages array has {} items, maximum is 500",
                request.messages.len()
            ),
        })
        .into_response());
    }

    // Validate message roles
    let valid_roles = ["system", "user", "assistant", "tool"];
    for (i, msg) in request.messages.iter().enumerate() {
        if !valid_roles.contains(&msg.role.as_str()) {
            return Ok(error_to_response(AiGatewayError::Validation {
                message: format!(
                    "messages[{}].role '{}' is invalid. Must be one of: system, user, assistant, tool",
                    i, msg.role
                ),
            })
            .into_response());
        }
    }

    let byok = extract_byok(&headers);
    if let Some(error) = reject_deployment_token_base_url(&auth, &byok) {
        return Ok(error_to_response(error).into_response());
    }
    let ai_context = extract_ai_context(&headers);
    let start = Instant::now();
    let model = request.model.clone();
    let is_streaming = request.stream;
    // None for deployment tokens (machine callers) so usage rows store NULL
    // instead of falsely attributing all deployed-app traffic to user id 0.
    let user_id = auth.user_id_opt();

    if is_streaming {
        match app_state
            .gateway_service
            .chat_completion_stream(&request, &byok)
            .await
        {
            Ok((stream, cred_type)) => {
                let provider_id =
                    crate::providers::route_model_to_provider(&model).unwrap_or("unknown");

                info!(
                    model = model,
                    user_id = ?user_id,
                    streaming = true,
                    credential_type = credential_type_str(cred_type),
                    "AI gateway streaming request started"
                );

                // Once-per-instance: "the AI gateway has been used here", not
                // "an AI request happened". Guard so it fires once (carrying the
                // provider of that first request), not on every request.
                app_state.telemetry.report_once(
                    "ai_gateway_first_request",
                    temps_core::telemetry::TelemetryEvent::new(
                        temps_core::telemetry::TelemetryEventKind::AiGatewayFirstRequest,
                    )
                    .with("provider", provider_id),
                );

                let wrapped = wrap_stream_with_usage_tracking(
                    stream,
                    app_state.usage_service.clone(),
                    user_id,
                    provider_id.to_string(),
                    model.clone(),
                    start,
                    cred_type == CredentialType::Byok,
                    ai_context.clone(),
                    UsageKind::ChatCompletions,
                );
                let body = Body::from_stream(wrapped);

                let resp = axum::response::Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "text/event-stream")
                    .header("cache-control", "no-cache")
                    .header("connection", "keep-alive")
                    .header("x-temps-provider", provider_id)
                    .header("x-temps-credential-type", credential_type_str(cred_type))
                    .body(body);
                match resp {
                    Ok(r) => Ok(r.into_response()),
                    Err(e) => {
                        error!(error = %e, "Failed to build streaming response");
                        Ok(error_to_response(AiGatewayError::Internal {
                            message: "Failed to build streaming response".to_string(),
                        })
                        .into_response())
                    }
                }
            }
            Err(e) => {
                error!(model = model, error = %e, "AI gateway streaming request failed");
                Ok(error_to_response(e).into_response())
            }
        }
    } else {
        match app_state
            .gateway_service
            .chat_completion(&request, &byok)
            .await
        {
            Ok((response, cred_type)) => {
                let latency = start.elapsed();
                let provider_id =
                    crate::providers::route_model_to_provider(&model).unwrap_or("unknown");

                // Log usage asynchronously (don't block the response)
                if let Some(ref usage) = response.usage {
                    let usage_service = app_state.usage_service.clone();
                    let model_clone = model.clone();
                    let provider_clone = provider_id.to_string();
                    let input = usage.prompt_tokens;
                    let output = usage.completion_tokens;
                    let latency_ms = latency.as_millis() as i32;
                    let is_byok = cred_type == CredentialType::Byok;
                    let ctx = ai_context.clone();
                    tokio::spawn(async move {
                        if let Err(e) = usage_service
                            .log_usage_with_context(
                                user_id,
                                &provider_clone,
                                &model_clone,
                                input,
                                output,
                                latency_ms,
                                0,
                                200,
                                false, // non-streaming path
                                is_byok,
                                &ctx,
                            )
                            .await
                        {
                            error!(error = %e, "Failed to log AI usage");
                        }
                    });
                }

                info!(
                    model = model,
                    user_id = ?user_id,
                    latency_ms = latency.as_millis() as u64,
                    credential_type = credential_type_str(cred_type),
                    "AI gateway request completed"
                );

                // Once-per-instance: "the AI gateway has been used here", not
                // "an AI request happened". Guard so it fires once (carrying the
                // provider of that first request), not on every request.
                app_state.telemetry.report_once(
                    "ai_gateway_first_request",
                    temps_core::telemetry::TelemetryEvent::new(
                        temps_core::telemetry::TelemetryEventKind::AiGatewayFirstRequest,
                    )
                    .with("provider", provider_id),
                );

                let mut response_builder = axum::response::Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "application/json")
                    .header("x-temps-provider", provider_id)
                    .header("x-temps-credential-type", credential_type_str(cred_type));

                if let Some(ref usage) = response.usage {
                    response_builder = response_builder.header(
                        "x-temps-tokens-used",
                        format!(
                            "prompt={},completion={},total={}",
                            usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
                        ),
                    );
                }

                let body = match serde_json::to_vec(&response) {
                    Ok(b) => b,
                    Err(e) => {
                        error!(error = %e, "Failed to serialize chat completion response");
                        return Ok(error_to_response(AiGatewayError::Internal {
                            message: "Failed to serialize response".to_string(),
                        })
                        .into_response());
                    }
                };
                match response_builder.body(Body::from(body)) {
                    Ok(r) => Ok(r.into_response()),
                    Err(e) => {
                        error!(error = %e, "Failed to build response");
                        Ok(error_to_response(AiGatewayError::Internal {
                            message: "Failed to build response".to_string(),
                        })
                        .into_response())
                    }
                }
            }
            Err(e) => {
                let latency = start.elapsed();
                error!(
                    model = model,
                    latency_ms = latency.as_millis() as u64,
                    error = %e,
                    "AI gateway request failed"
                );
                Ok(error_to_response(e).into_response())
            }
        }
    }
}

#[utoipa::path(
    tag = "AI Gateway",
    get,
    path = "/ai/v1/models",
    responses(
        (status = 200, description = "List of available models", body = ModelListResponse),
        (status = 401, description = "Unauthorized", body = OpenAiErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
async fn list_models(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayRead);

    match app_state.provider_model_service.list_active_catalog().await {
        Ok(models) => Ok((
            StatusCode::OK,
            Json(ModelListResponse {
                object: "list".to_string(),
                data: models,
            }),
        )
            .into_response()),
        Err(e) => Ok(error_to_response(e).into_response()),
    }
}

#[utoipa::path(
    tag = "AI Gateway",
    post,
    path = "/ai/v1/embeddings",
    request_body = EmbeddingRequest,
    responses(
        (status = 200, description = "Embedding response", body = EmbeddingResponse),
        (status = 400, description = "Invalid request", body = OpenAiErrorResponse),
        (status = 401, description = "Unauthorized", body = OpenAiErrorResponse),
        (status = 404, description = "Model not found", body = OpenAiErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
async fn embeddings(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    headers: HeaderMap,
    Json(request): Json<EmbeddingRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayExecute);

    if request.model.is_empty() {
        return Ok(error_to_response(AiGatewayError::Validation {
            message: "model field is required".to_string(),
        })
        .into_response());
    }

    match &request.input {
        EmbeddingInput::Single(s) if s.is_empty() => {
            return Ok(error_to_response(AiGatewayError::Validation {
                message: "input cannot be empty".to_string(),
            })
            .into_response());
        }
        EmbeddingInput::Multiple(items) if items.is_empty() => {
            return Ok(error_to_response(AiGatewayError::Validation {
                message: "input array cannot be empty".to_string(),
            })
            .into_response());
        }
        EmbeddingInput::Multiple(items) if items.len() > 2048 => {
            return Ok(error_to_response(AiGatewayError::Validation {
                message: format!("input array has {} items, maximum is 2048", items.len()),
            })
            .into_response());
        }
        _ => {}
    }

    let byok = extract_byok(&headers);
    if let Some(error) = reject_deployment_token_base_url(&auth, &byok) {
        return Ok(error_to_response(error).into_response());
    }
    let ai_context = extract_ai_context(&headers);
    let start = Instant::now();
    let user_id = auth.user_id_opt();

    match app_state.gateway_service.embeddings(&request, &byok).await {
        Ok((response, cred_type)) => {
            let latency = start.elapsed();
            let provider_id =
                crate::providers::route_model_to_provider(&request.model).unwrap_or("unknown");

            // Log usage asynchronously (don't block the response). Embeddings
            // only consume prompt tokens; there is no completion output.
            {
                let usage_service = app_state.usage_service.clone();
                let model = request.model.clone();
                let provider = provider_id.to_string();
                let input_tokens = response.usage.prompt_tokens;
                let latency_ms = latency.as_millis() as i32;
                let is_byok = cred_type == CredentialType::Byok;
                let ctx = ai_context.clone();
                tokio::spawn(async move {
                    if let Err(e) = usage_service
                        .log_usage_with_context(
                            user_id,
                            &provider,
                            &model,
                            input_tokens,
                            0,
                            latency_ms,
                            0,
                            200,
                            false,
                            is_byok,
                            &ctx,
                        )
                        .await
                    {
                        error!(error = %e, "Failed to log AI embedding usage");
                    }
                });
            }

            info!(
                model = request.model,
                user_id = ?user_id,
                latency_ms = latency.as_millis() as u64,
                credential_type = credential_type_str(cred_type),
                "AI gateway embedding request completed"
            );

            let resp = axum::response::Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/json")
                .header("x-temps-credential-type", credential_type_str(cred_type));

            let body = match serde_json::to_vec(&response) {
                Ok(b) => b,
                Err(e) => {
                    error!(error = %e, "Failed to serialize embedding response");
                    return Ok(error_to_response(AiGatewayError::Internal {
                        message: "Failed to serialize response".to_string(),
                    })
                    .into_response());
                }
            };
            match resp.body(Body::from(body)) {
                Ok(r) => Ok(r.into_response()),
                Err(e) => {
                    error!(error = %e, "Failed to build embedding response");
                    Ok(error_to_response(AiGatewayError::Internal {
                        message: "Failed to build response".to_string(),
                    })
                    .into_response())
                }
            }
        }
        Err(e) => Ok(error_to_response(e).into_response()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to build a chat completion request
    fn sample_chat_request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("Hello".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            }],
            stream: false,
            temperature: None,
            max_tokens: None,
            top_p: None,
            stop: None,
            n: None,
            tools: None,
            tool_choice: None,
            response_format: None,
            frequency_penalty: None,
            presence_penalty: None,
            seed: None,
            user: None,
            extra: None,
        }
    }

    #[test]
    fn test_error_to_response_model_not_found() {
        let err = AiGatewayError::ModelNotFound {
            model: "unknown-model".to_string(),
        };
        // Just verify it doesn't panic
        let _ = error_to_response(err);
    }

    #[test]
    fn test_error_to_response_provider_not_configured() {
        let err = AiGatewayError::ProviderNotConfigured {
            provider: "openai".to_string(),
        };
        let _ = error_to_response(err);
    }

    #[test]
    fn test_error_to_response_upstream_error() {
        let err = AiGatewayError::UpstreamError {
            model: "gpt-4o".to_string(),
            status: 429,
            message: "Rate limited".to_string(),
        };
        let _ = error_to_response(err);
    }

    #[test]
    fn test_error_to_response_validation() {
        let err = AiGatewayError::Validation {
            message: "model is required".to_string(),
        };
        let _ = error_to_response(err);
    }

    #[test]
    fn test_openai_error_response_format() {
        let err = OpenAiErrorResponse::invalid_request("bad request", "invalid_model");
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("invalid_request_error"));
        assert!(json.contains("bad request"));
        assert!(json.contains("invalid_model"));
    }

    #[test]
    fn test_sample_chat_request_serialization() {
        let req = sample_chat_request();
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("gpt-4o"));
        assert!(json.contains("Hello"));
        // Should not contain None fields
        assert!(!json.contains("temperature"));
        assert!(!json.contains("max_tokens"));
    }

    #[test]
    fn test_chat_request_deserialization_minimal() {
        let json = r#"{"model":"gpt-4o","messages":[{"role":"user","content":"Hi"}]}"#;
        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.model, "gpt-4o");
        assert_eq!(req.messages.len(), 1);
        assert!(!req.stream); // defaults to false
    }

    #[test]
    fn test_chat_request_deserialization_full() {
        let json = r#"{
            "model": "gpt-4o",
            "messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "user", "content": "Hello"}
            ],
            "stream": true,
            "temperature": 0.7,
            "max_tokens": 500,
            "top_p": 0.9,
            "stop": ["\n"],
            "n": 1
        }"#;
        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.model, "gpt-4o");
        assert_eq!(req.messages.len(), 2);
        assert!(req.stream);
        assert_eq!(req.temperature, Some(0.7));
        assert_eq!(req.max_tokens, Some(500));
    }

    #[test]
    fn test_chat_response_serialization() {
        let response = ChatCompletionResponse {
            id: "chatcmpl-123".to_string(),
            object: "chat.completion".to_string(),
            created: 1710028800,
            model: "gpt-4o".to_string(),
            choices: vec![ChatCompletionChoice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: Some(MessageContent::Text("Hello!".to_string())),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: Some(UsageInfo {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("chat.completion"));
        assert!(json.contains("Hello!"));
        assert!(json.contains("\"stop\""));
        assert!(json.contains("prompt_tokens"));
    }

    #[test]
    fn test_multipart_content_deserialization() {
        let json = r#"{
            "model": "gpt-4o",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "What's in this image?"},
                    {"type": "image_url", "image_url": {"url": "https://example.com/img.png"}}
                ]
            }]
        }"#;

        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.messages.len(), 1);
        match &req.messages[0].content {
            Some(MessageContent::Parts(parts)) => {
                assert_eq!(parts.len(), 2);
                assert_eq!(parts[0].r#type, "text");
                assert_eq!(parts[0].text, Some("What's in this image?".to_string()));
            }
            _ => panic!("Expected Parts content"),
        }
    }

    #[test]
    fn test_embedding_request_deserialization() {
        let json = r#"{"model":"text-embedding-3-small","input":"Hello world"}"#;
        let req: EmbeddingRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.model, "text-embedding-3-small");
    }

    #[test]
    fn test_embedding_request_array_input() {
        let json = r#"{"model":"text-embedding-3-small","input":["Hello","World"]}"#;
        let req: EmbeddingRequest = serde_json::from_str(json).unwrap();
        match req.input {
            EmbeddingInput::Multiple(v) => assert_eq!(v.len(), 2),
            _ => panic!("Expected multiple inputs"),
        }
    }

    #[test]
    fn test_model_list_response() {
        let response = ModelListResponse {
            object: "list".to_string(),
            data: vec![ModelInfo {
                id: "gpt-4o".to_string(),
                object: "model".to_string(),
                owned_by: "openai".to_string(),
            }],
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"list\""));
        assert!(json.contains("gpt-4o"));
    }

    #[test]
    fn test_stop_sequence_single() {
        let json = r#"{"model":"gpt-4o","messages":[{"role":"user","content":"Hi"}],"stop":"\n"}"#;
        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(req.stop, Some(StopSequence::Single(_))));
    }

    #[test]
    fn test_stop_sequence_multiple() {
        let json = r####"{"model":"gpt-4o","messages":[{"role":"user","content":"Hi"}],"stop":["\n","###"]}"####;
        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
        match req.stop {
            Some(StopSequence::Multiple(v)) => assert_eq!(v.len(), 2),
            _ => panic!("Expected multiple stop sequences"),
        }
    }

    #[test]
    fn test_extract_byok_no_headers() {
        let headers = HeaderMap::new();
        let byok = extract_byok(&headers);
        assert!(byok.api_key.is_none());
        assert!(byok.base_url.is_none());
    }

    #[test]
    fn test_extract_byok_with_api_key() {
        let mut headers = HeaderMap::new();
        headers.insert("x-provider-api-key", "sk-user-key-123".parse().unwrap());
        let byok = extract_byok(&headers);
        assert_eq!(byok.api_key.as_deref(), Some("sk-user-key-123"));
        assert!(byok.base_url.is_none());
    }

    #[test]
    fn test_extract_byok_with_both_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-provider-api-key", "sk-user-key-123".parse().unwrap());
        headers.insert(
            "x-provider-base-url",
            "https://custom.openai.azure.com".parse().unwrap(),
        );
        let byok = extract_byok(&headers);
        assert_eq!(byok.api_key.as_deref(), Some("sk-user-key-123"));
        assert_eq!(
            byok.base_url.as_deref(),
            Some("https://custom.openai.azure.com")
        );
    }

    #[test]
    fn test_credential_type_str_values() {
        assert_eq!(credential_type_str(CredentialType::System), "system");
        assert_eq!(credential_type_str(CredentialType::Byok), "byok");
    }
    #[test]
    fn test_deployment_token_rejects_custom_byok_base_url() {
        let auth = temps_auth::AuthContext::new_deployment_token(
            7,
            None,
            None,
            1,
            "deployment-token".to_string(),
            vec![temps_entities::deployment_tokens::DeploymentTokenPermission::AiGatewayExecute],
        );
        let byok = ByokOverride {
            api_key: Some("sk-user-key-123".to_string()),
            base_url: Some("https://custom.openai.azure.com".to_string()),
            system_key_id: None,
        };

        let error = reject_deployment_token_base_url(&auth, &byok)
            .expect("deployment tokens must not set provider base URLs");
        assert!(matches!(error, AiGatewayError::InvalidProviderUrl { .. }));
    }

    #[test]
    fn test_deployment_token_allows_byok_without_custom_base_url() {
        let auth = temps_auth::AuthContext::new_deployment_token(
            7,
            None,
            None,
            1,
            "deployment-token".to_string(),
            vec![temps_entities::deployment_tokens::DeploymentTokenPermission::AiGatewayExecute],
        );
        let byok = ByokOverride {
            api_key: Some("sk-user-key-123".to_string()),
            base_url: None,
            system_key_id: None,
        };

        assert!(reject_deployment_token_base_url(&auth, &byok).is_none());
    }

    #[test]
    fn test_extract_usage_from_sse_openai_final_chunk() {
        let line = r#"data: {"id":"chatcmpl-abc","object":"chat.completion.chunk","choices":[],"usage":{"prompt_tokens":42,"completion_tokens":18,"total_tokens":60}}"#;
        let result = extract_usage_from_sse_line(line);
        assert_eq!(result, Some((42, 18)));
    }

    #[test]
    fn test_extract_usage_from_sse_anthropic_message_delta() {
        let line = r#"data: {"id":"msg_abc","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":0,"completion_tokens":25,"total_tokens":25}}"#;
        let result = extract_usage_from_sse_line(line);
        assert_eq!(result, Some((0, 25)));
    }

    #[test]
    fn test_extract_usage_from_sse_done_line() {
        let line = "data: [DONE]";
        assert_eq!(extract_usage_from_sse_line(line), None);
    }

    #[test]
    fn test_extract_usage_from_sse_no_usage() {
        let line = r#"data: {"id":"chatcmpl-abc","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"Hello"}}]}"#;
        assert_eq!(extract_usage_from_sse_line(line), None);
    }

    #[test]
    fn test_extract_usage_from_sse_not_data_line() {
        let line = "event: message";
        assert_eq!(extract_usage_from_sse_line(line), None);
    }

    #[test]
    fn test_extract_usage_from_sse_zero_tokens_ignored() {
        let line = r#"data: {"usage":{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0}}"#;
        assert_eq!(extract_usage_from_sse_line(line), None);
    }

    #[test]
    fn test_extract_responses_usage_from_completed_event() {
        let line = r#"data: {"type":"response.completed","sequence_number":9,"response":{"id":"resp_1","object":"response","status":"completed","usage":{"input_tokens":120,"input_tokens_details":{"cached_tokens":64},"output_tokens":30,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":150}}}"#;
        assert_eq!(extract_responses_usage_from_sse_line(line), Some((120, 30)));
    }

    #[test]
    fn test_extract_responses_usage_from_incomplete_event() {
        let line = r#"data: {"type":"response.incomplete","response":{"usage":{"input_tokens":50,"output_tokens":4096}}}"#;
        assert_eq!(
            extract_responses_usage_from_sse_line(line),
            Some((50, 4096))
        );
    }

    #[test]
    fn test_extract_responses_usage_ignores_deltas_and_other_lines() {
        assert_eq!(
            extract_responses_usage_from_sse_line(
                r#"data: {"type":"response.output_text.delta","delta":"usage"}"#
            ),
            None
        );
        // An in-progress snapshot has usage: null and is not terminal.
        assert_eq!(
            extract_responses_usage_from_sse_line(
                r#"data: {"type":"response.created","response":{"usage":null}}"#
            ),
            None
        );
        assert_eq!(
            extract_responses_usage_from_sse_line("event: response.completed"),
            None
        );
    }

    #[tokio::test]
    async fn test_usage_tracker_forwards_bytes_split_inside_a_character() {
        use tokio_stream::StreamExt;
        // "é" is two bytes; split the stream between them, mid-line. The
        // tracker buffers bytes, so the caller still gets every byte.
        let line = "data: {\"type\":\"response.completed\",\"response\":{\"output_text\":\"caf\u{e9}\",\"usage\":{\"input_tokens\":7,\"output_tokens\":3}}}\n\n";
        let bytes = line.as_bytes();
        let split = line.find('\u{e9}').expect("accented character") + 1;
        assert_eq!(
            extract_responses_usage_from_sse_line(line.lines().next().unwrap_or("")),
            Some((7, 3))
        );

        let chunks: Vec<Result<Bytes, AiGatewayError>> = vec![
            Ok(Bytes::copy_from_slice(&bytes[..split])),
            Ok(Bytes::copy_from_slice(&bytes[split..])),
        ];
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let wrapped = wrap_stream_with_usage_tracking(
            Box::pin(tokio_stream::iter(chunks)),
            Arc::new(UsageService::new(db)),
            None,
            "openai".into(),
            "gpt-6-luna".into(),
            Instant::now(),
            false,
            AiRequestContext::default(),
            UsageKind::Responses,
        );
        let forwarded: Vec<u8> = wrapped
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .flat_map(|chunk| chunk.expect("chunk").to_vec())
            .collect();
        assert_eq!(forwarded, bytes);
    }

    #[tokio::test]
    async fn oversized_terminal_events_forward_unchanged_and_log_usage_once() {
        use sea_orm::{DatabaseBackend, MockDatabase};
        use temps_entities::ai_usage_logs;
        use tokio_stream::StreamExt;

        for (index, event_type) in [
            "response.completed",
            "response.incomplete",
            "response.failed",
        ]
        .into_iter()
        .enumerate()
        {
            for is_byok in [false, true] {
                let usage = "\"usage\":{\"input_tokens\":74122,\"output_tokens\":32989}";
                // Exercise counts both before and after >32 MiB of output.
                let prefix = if index == 1 {
                    format!("data: {{\"response\":{{{usage},\"output_text\":\"")
                } else {
                    format!("data: {{\"type\":\"{event_type}\",\"response\":{{\"output_text\":\"")
                };
                let suffix = if index == 1 {
                    format!("\"}},\"type\":\"{event_type}\"}}\r\n\n")
                } else {
                    // Also handle a complete final event without a trailing LF.
                    format!("\",{usage}}}}}")
                };
                let prefix = Bytes::from(prefix);
                let suffix = Bytes::from(suffix);
                let repeated = Bytes::from(vec![b'x'; 4096]);
                let chunks = 8193;
                let source_prefix = prefix.clone();
                let source_suffix = suffix.clone();
                let source_repeated = repeated.clone();
                let source = async_stream::stream! {
                    yield Ok(source_prefix);
                    for _ in 0..chunks {
                        yield Ok(source_repeated.clone());
                    }
                    yield Ok(source_suffix);
                };
                let db = Arc::new(
                    MockDatabase::new(DatabaseBackend::Postgres)
                        .append_query_results([vec![ai_usage_logs::Model {
                            id: 1,
                            timestamp: chrono::Utc::now(),
                            user_id: Some(42),
                            provider: "openai".into(),
                            model: "streaming-test-model".into(),
                            input_tokens: 74122,
                            output_tokens: 32989,
                            latency_ms: 0,
                            estimated_cost_microcents: 0,
                            status: 200,
                            is_streaming: true,
                            is_byok,
                            conversation_id: None,
                            tags: vec![],
                            request_id: None,
                            trace_id: None,
                        }]])
                        .into_connection(),
                );
                let mut wrapped = wrap_stream_with_usage_tracking(
                    Box::pin(source),
                    Arc::new(UsageService::new(db.clone())),
                    Some(42),
                    "openai".into(),
                    "streaming-test-model".into(),
                    Instant::now(),
                    is_byok,
                    AiRequestContext::default(),
                    UsageKind::Responses,
                );
                let mut forwarded = 0;
                while let Some(chunk) = wrapped.next().await {
                    let chunk = chunk.expect("upstream bytes");
                    let expected = if forwarded == 0 {
                        &prefix
                    } else if forwarded <= chunks {
                        &repeated
                    } else {
                        &suffix
                    };
                    assert_eq!(&chunk, expected, "{event_type}, BYOK={is_byok}");
                    forwarded += 1;
                }
                assert_eq!(forwarded, chunks + 2);
                drop(wrapped);
                tokio::time::timeout(std::time::Duration::from_secs(2), async {
                    while Arc::strong_count(&db) > 1 {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("usage insert finished");
                let transactions = Arc::try_unwrap(db).unwrap().into_transaction_log();
                assert_eq!(transactions.len(), 1, "exactly one usage insert");
                let statements = transactions[0].statements();
                assert_eq!(statements.len(), 1);
                let statement = &statements[0];
                assert!(statement.sql.starts_with("INSERT INTO \"ai_usage_logs\""));
                let values = &statement.values.as_ref().expect("insert bindings").0;
                assert_eq!(values[4], sea_orm::Value::BigInt(Some(74122)));
                assert_eq!(values[5], sea_orm::Value::BigInt(Some(32989)));
                assert_eq!(values[9], sea_orm::Value::Bool(Some(true)));
                assert_eq!(values[10], sea_orm::Value::Bool(Some(is_byok)));
            }
        }
    }

    #[test]
    fn test_extract_ai_context_empty_headers() {
        let headers = HeaderMap::new();
        let ctx = extract_ai_context(&headers);
        assert!(ctx.conversation_id.is_none());
        assert!(ctx.tags.is_empty());
        assert!(ctx.request_id.is_none());
        assert!(ctx.trace_id.is_none());
    }

    #[test]
    fn test_extract_ai_context_conversation_id() {
        let mut headers = HeaderMap::new();
        headers.insert("x-conversation-id", "conv_abc123".parse().unwrap());
        let ctx = extract_ai_context(&headers);
        assert_eq!(ctx.conversation_id.as_deref(), Some("conv_abc123"));
    }

    #[test]
    fn test_extract_ai_context_tags() {
        let mut headers = HeaderMap::new();
        headers.insert("x-tags", "agent:support, env:prod".parse().unwrap());
        let ctx = extract_ai_context(&headers);
        assert_eq!(ctx.tags, vec!["agent:support", "env:prod"]);
    }

    #[test]
    fn test_extract_ai_context_tags_trims_whitespace() {
        let mut headers = HeaderMap::new();
        headers.insert("x-tags", " foo , bar , baz ".parse().unwrap());
        let ctx = extract_ai_context(&headers);
        assert_eq!(ctx.tags, vec!["foo", "bar", "baz"]);
    }

    #[test]
    fn test_extract_ai_context_tags_filters_empty() {
        let mut headers = HeaderMap::new();
        headers.insert("x-tags", "foo,,bar,".parse().unwrap());
        let ctx = extract_ai_context(&headers);
        assert_eq!(ctx.tags, vec!["foo", "bar"]);
    }

    #[test]
    fn test_extract_ai_context_request_id() {
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", "req_xyz789".parse().unwrap());
        let ctx = extract_ai_context(&headers);
        assert_eq!(ctx.request_id.as_deref(), Some("req_xyz789"));
    }

    #[test]
    fn test_extract_ai_context_traceparent() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "traceparent",
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"
                .parse()
                .unwrap(),
        );
        let ctx = extract_ai_context(&headers);
        assert_eq!(
            ctx.trace_id.as_deref(),
            Some("0af7651916cd43dd8448eb211c80319c")
        );
    }

    #[test]
    fn test_extract_ai_context_invalid_traceparent() {
        let mut headers = HeaderMap::new();
        headers.insert("traceparent", "not-valid".parse().unwrap());
        let ctx = extract_ai_context(&headers);
        // "not-valid" split by '-': ["not", "valid"] -> nth(1) = "valid"
        assert_eq!(ctx.trace_id.as_deref(), Some("valid"));
    }

    #[test]
    fn test_extract_ai_context_all_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-conversation-id", "conv_123".parse().unwrap());
        headers.insert("x-tags", "agent:bot,tier:premium".parse().unwrap());
        headers.insert("x-request-id", "req_456".parse().unwrap());
        headers.insert(
            "traceparent",
            "00-abcdef1234567890abcdef1234567890-1234567890abcdef-01"
                .parse()
                .unwrap(),
        );
        let ctx = extract_ai_context(&headers);
        assert_eq!(ctx.conversation_id.as_deref(), Some("conv_123"));
        assert_eq!(ctx.tags, vec!["agent:bot", "tier:premium"]);
        assert_eq!(ctx.request_id.as_deref(), Some("req_456"));
        assert_eq!(
            ctx.trace_id.as_deref(),
            Some("abcdef1234567890abcdef1234567890")
        );
    }
}
