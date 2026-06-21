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

use crate::error::AiGatewayError;
use crate::handlers::types::AiGatewayAppState;
use crate::services::gateway_service::{ByokOverride, CredentialType};
use crate::services::usage_service::AiRequestContext;
use crate::services::UsageService;
use crate::types::*;

/// Extract BYOK overrides from request headers.
fn extract_byok(headers: &HeaderMap) -> ByokOverride {
    ByokOverride {
        api_key: headers
            .get("x-provider-api-key")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string()),
        base_url: headers
            .get("x-provider-base-url")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string()),
    }
}

/// Extract AI request context (conversation, tags, trace) from request headers.
fn extract_ai_context(headers: &HeaderMap) -> AiRequestContext {
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

fn credential_type_str(ct: CredentialType) -> &'static str {
    match ct {
        CredentialType::System => "system",
        CredentialType::Byok => "byok",
    }
}

// ============================================================================
// Streaming usage extraction
// ============================================================================

/// Extract usage info from an SSE `data: {...}` line.
/// OpenAI sends usage in the final chunk when `stream_options.include_usage` is set.
/// Anthropic's translator already puts usage in `message_delta` chunks.
/// Returns `(prompt_tokens, completion_tokens)` if found.
fn extract_usage_from_sse_line(line: &str) -> Option<(i64, i64)> {
    let json_str = line.strip_prefix("data: ")?.trim();
    if json_str == "[DONE]" {
        return None;
    }
    let parsed: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let usage = parsed.get("usage")?;
    let prompt = usage
        .get("prompt_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let completion = usage
        .get("completion_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if prompt > 0 || completion > 0 {
        Some((prompt, completion))
    } else {
        None
    }
}

/// Wraps an upstream SSE byte stream to transparently intercept usage data
/// from the final chunks, then logs it after the stream ends.
#[allow(clippy::too_many_arguments)]
fn wrap_stream_with_usage_tracking(
    inner: std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<Bytes, AiGatewayError>> + Send>,
    >,
    usage_service: Arc<UsageService>,
    user_id: i32,
    provider: String,
    model: String,
    start: Instant,
    is_byok: bool,
    ai_context: AiRequestContext,
) -> std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<Bytes, AiGatewayError>> + Send>> {
    use tokio_stream::StreamExt;

    let prompt_tokens = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let completion_tokens = Arc::new(std::sync::atomic::AtomicI64::new(0));
    // Buffer for incomplete SSE lines split across chunks
    let line_buf = Arc::new(std::sync::Mutex::new(String::new()));

    let pt = prompt_tokens.clone();
    let ct = completion_tokens.clone();
    let lb = line_buf.clone();

    let mapped = inner.map(move |result| {
        if let Ok(ref bytes) = result {
            if let Ok(text) = std::str::from_utf8(bytes) {
                let mut buf = lb.lock().unwrap_or_else(|e| e.into_inner());
                buf.push_str(text);

                // Process complete lines
                while let Some(newline_pos) = buf.find('\n') {
                    let line: String = buf.drain(..=newline_pos).collect();
                    let line = line.trim();
                    if let Some((p, c)) = extract_usage_from_sse_line(line) {
                        pt.store(p, std::sync::atomic::Ordering::Relaxed);
                        ct.store(c, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        }
        result
    });

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
                            Some(user_id),
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

fn error_to_response(error: AiGatewayError) -> impl IntoResponse {
    let (status, body) = match &error {
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
    let ai_context = extract_ai_context(&headers);
    let start = Instant::now();
    let model = request.model.clone();
    let is_streaming = request.stream;
    let user_id = auth.user_id();

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
                    user_id = user_id,
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
                                Some(user_id),
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
                    user_id = user_id,
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

    match app_state.gateway_service.list_models().await {
        Ok(models) => Ok((StatusCode::OK, Json(models)).into_response()),
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

    let byok = extract_byok(&headers);

    match app_state.gateway_service.embeddings(&request, &byok).await {
        Ok((response, cred_type)) => {
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
