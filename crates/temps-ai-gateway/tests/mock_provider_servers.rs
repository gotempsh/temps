//! Mock provider servers for integration testing.
//!
//! Provides lightweight axum-based HTTP servers that simulate:
//! - OpenAI API (chat completions, embeddings, models, streaming)
//! - Anthropic Messages API (chat, streaming)
//! - Gemini generateContent API (chat, streaming)
//!
//! Each server binds to a random port on localhost and returns canned
//! responses that match the real provider wire formats.

use axum::{
    body::Body,
    extract::Query,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde::Deserialize;
use std::net::SocketAddr;
use tokio::net::TcpListener;

// ============================================================================
// OpenAI Mock Server
// ============================================================================

#[allow(dead_code)]
pub struct MockOpenAiServer {
    pub addr: SocketAddr,
    pub base_url: String,
    _handle: tokio::task::JoinHandle<()>,
}

impl MockOpenAiServer {
    pub async fn start() -> Self {
        let app = Router::new()
            .route("/v1/chat/completions", post(openai_chat_completions))
            .route("/v1/responses", post(openai_responses))
            .route("/v1/embeddings", post(openai_embeddings));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{}/v1", addr);

        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        Self {
            addr,
            base_url,
            _handle: handle,
        }
    }
}

async fn openai_responses(
    headers: HeaderMap,
    Json(request): Json<serde_json::Value>,
) -> impl IntoResponse {
    let authenticated = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("Bearer ") && value.len() > 7);
    if !authenticated {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": {"message": "Invalid API key"}
            })),
        )
            .into_response();
    }

    let model = request
        .get("model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("gpt-5.6-luna")
        .to_string();
    let tools = request.get("tools").and_then(serde_json::Value::as_array);
    let has_tools = tools.is_some_and(|tools| !tools.is_empty());
    let function_output_call_id = request
        .get("input")
        .and_then(serde_json::Value::as_array)
        .and_then(|items| {
            items.iter().find_map(|item| {
                (item.get("type").and_then(serde_json::Value::as_str)
                    == Some("function_call_output"))
                .then(|| item.get("call_id").and_then(serde_json::Value::as_str))
                .flatten()
            })
        });
    if function_output_call_id.is_some_and(|call_id| call_id != "call_mock") {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": {"message": "gateway tool call id was not restored to the upstream id"}
            })),
        )
            .into_response();
    }
    if has_tools {
        let flattened_tool = tools
            .and_then(|tools| tools.first())
            .and_then(|tool| tool.get("name"))
            .and_then(serde_json::Value::as_str)
            == Some("lookup");
        let reasoning = request
            .pointer("/reasoning/effort")
            .and_then(serde_json::Value::as_str);
        if !flattened_tool || reasoning != Some("medium") {
            return (StatusCode::BAD_REQUEST, Json(serde_json::json!({
                "error": {"message": "Responses tools must be flattened and retain reasoning effort"}
            })))
                .into_response();
        }
    }

    if request.get("stream").and_then(serde_json::Value::as_bool) == Some(true) {
        let stream = async_stream::stream! {
            for event in [
                serde_json::json!({
                    "type": "response.created",
                    "response": {"id": "resp_mock", "model": model, "created_at": 1710000000i64}
                }),
                serde_json::json!({"type": "response.output_text.delta", "delta": "Hello"}),
                serde_json::json!({"type": "response.output_text.delta", "delta": " from Responses"}),
                serde_json::json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_mock", "model": model, "created_at": 1710000000i64,
                        "status": "completed",
                        "output": [{"type": "message", "content": [{"type": "output_text", "text": "Hello from Responses"}]}],
                        "usage": {"input_tokens": 5, "output_tokens": 3, "total_tokens": 8}
                    }
                }),
            ] {
                yield Ok::<_, std::convert::Infallible>(format!("event: {}\r\ndata: {}\r\n\r\n", event["type"].as_str().unwrap(), event));
            }
        };
        return axum::response::Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .body(Body::from_stream(stream))
            .unwrap()
            .into_response();
    }

    let output = if function_output_call_id.is_some() {
        serde_json::json!([
            {"type": "message", "content": [{"type": "output_text", "text": "Tool result accepted"}]}
        ])
    } else if has_tools {
        serde_json::json!([
            {"type": "reasoning", "id": "rs_mock", "encrypted_content": "opaque"},
            {"type": "function_call", "call_id": "call_mock", "name": "lookup", "arguments": "{}"}
        ])
    } else {
        serde_json::json!([
            {"type": "message", "content": [{"type": "output_text", "text": "Mock Responses result"}]}
        ])
    };
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "id": "resp_mock",
            "model": model,
            "created_at": 1710000000i64,
            "status": "completed",
            "output": output,
            "usage": {"input_tokens": 7, "output_tokens": 4, "total_tokens": 11}
        })),
    )
        .into_response()
}

#[derive(Deserialize)]
struct OpenAiChatRequest {
    model: String,
    messages: Vec<serde_json::Value>,
    #[serde(default)]
    stream: bool,
}

async fn openai_chat_completions(
    headers: HeaderMap,
    Json(request): Json<OpenAiChatRequest>,
) -> impl IntoResponse {
    // Validate auth header
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if !auth.starts_with("Bearer ") || auth.len() < 8 {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": {
                    "message": "Invalid API key",
                    "type": "invalid_request_error",
                    "code": "invalid_api_key"
                }
            })),
        )
            .into_response();
    }

    // Check for special test API key that triggers errors
    let api_key = &auth[7..];
    if api_key == "sk-trigger-429" {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "error": {
                    "message": "Rate limit exceeded",
                    "type": "rate_limit_error",
                    "code": "rate_limit_exceeded"
                }
            })),
        )
            .into_response();
    }

    if api_key == "sk-trigger-500" {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": {
                    "message": "Internal server error",
                    "type": "server_error",
                    "code": "internal_error"
                }
            })),
        )
            .into_response();
    }

    // Check if the request contains tools — if so, return a tool_calls response
    let has_tools = request.messages.iter().any(|m| {
        m.get("content")
            .and_then(|c| c.as_str())
            .map(|s| s.contains("weather"))
            .unwrap_or(false)
    });

    if !request.stream && has_tools && api_key == "sk-test-tools" {
        let response = serde_json::json!({
            "id": "chatcmpl-mock-tools",
            "object": "chat.completion",
            "created": 1710000000i64,
            "model": request.model,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_mock_123",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"location\":\"London\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 15,
                "completion_tokens": 10,
                "total_tokens": 25
            }
        });
        return (StatusCode::OK, Json(response)).into_response();
    }

    if request.stream {
        // Return SSE stream
        let model = request.model.clone();
        let stream = async_stream::stream! {
            // Initial chunk with role
            let chunk1 = serde_json::json!({
                "id": "chatcmpl-mock-123",
                "object": "chat.completion.chunk",
                "created": 1710000000i64,
                "model": model,
                "choices": [{
                    "index": 0,
                    "delta": { "role": "assistant" },
                    "finish_reason": null
                }]
            });
            yield Ok::<_, std::convert::Infallible>(format!("data: {}\n\n", serde_json::to_string(&chunk1).unwrap()));

            // Content chunks
            for word in ["Hello", " from", " mock", " OpenAI", "!"] {
                let chunk = serde_json::json!({
                    "id": "chatcmpl-mock-123",
                    "object": "chat.completion.chunk",
                    "created": 1710000000i64,
                    "model": model,
                    "choices": [{
                        "index": 0,
                        "delta": { "content": word },
                        "finish_reason": null
                    }]
                });
                yield Ok(format!("data: {}\n\n", serde_json::to_string(&chunk).unwrap()));
            }

            // Final chunk with finish reason
            let final_chunk = serde_json::json!({
                "id": "chatcmpl-mock-123",
                "object": "chat.completion.chunk",
                "created": 1710000000i64,
                "model": model,
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 5,
                    "total_tokens": 15
                }
            });
            yield Ok(format!("data: {}\n\n", serde_json::to_string(&final_chunk).unwrap()));
            yield Ok("data: [DONE]\n\n".to_string());
        };

        let body = Body::from_stream(stream);
        axum::response::Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .header("cache-control", "no-cache")
            .body(body)
            .unwrap()
            .into_response()
    } else {
        // Extract user message for echo-back
        let user_msg = request
            .messages
            .last()
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("Hello");

        let response = serde_json::json!({
            "id": "chatcmpl-mock-123",
            "object": "chat.completion",
            "created": 1710000000i64,
            "model": request.model,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": format!("Mock response to: {}", user_msg)
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 8,
                "total_tokens": 18
            }
        });

        (StatusCode::OK, Json(response)).into_response()
    }
}

#[derive(Deserialize)]
struct OpenAiEmbeddingRequest {
    model: String,
    input: serde_json::Value,
}

async fn openai_embeddings(
    headers: HeaderMap,
    Json(request): Json<OpenAiEmbeddingRequest>,
) -> impl IntoResponse {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !auth.starts_with("Bearer ") {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": { "message": "Invalid API key", "type": "invalid_request_error" }
            })),
        )
            .into_response();
    }

    let num_inputs = match &request.input {
        serde_json::Value::String(_) => 1,
        serde_json::Value::Array(arr) => arr.len(),
        _ => 1,
    };

    let data: Vec<serde_json::Value> = (0..num_inputs)
        .map(|i| {
            serde_json::json!({
                "object": "embedding",
                "embedding": [0.1, 0.2, 0.3, 0.4, 0.5],
                "index": i
            })
        })
        .collect();

    let response = serde_json::json!({
        "object": "list",
        "data": data,
        "model": request.model,
        "usage": {
            "prompt_tokens": 5 * num_inputs,
            "total_tokens": 5 * num_inputs
        }
    });

    (StatusCode::OK, Json(response)).into_response()
}

// ============================================================================
// Anthropic Mock Server
// ============================================================================

#[allow(dead_code)]
pub struct MockAnthropicServer {
    pub addr: SocketAddr,
    pub base_url: String,
    _handle: tokio::task::JoinHandle<()>,
}

impl MockAnthropicServer {
    pub async fn start() -> Self {
        let app = Router::new().route("/v1/messages", post(anthropic_messages));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{}", addr);

        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        Self {
            addr,
            base_url,
            _handle: handle,
        }
    }
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct AnthropicMessagesRequest {
    model: String,
    messages: Vec<serde_json::Value>,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    system: Option<String>,
    max_tokens: i64,
    #[serde(default)]
    tools: Option<Vec<serde_json::Value>>,
}

async fn anthropic_messages(
    headers: HeaderMap,
    Json(request): Json<AnthropicMessagesRequest>,
) -> impl IntoResponse {
    // Validate Anthropic-style auth
    let api_key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if api_key.is_empty() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "type": "error",
                "error": { "type": "authentication_error", "message": "Invalid API key" }
            })),
        )
            .into_response();
    }

    let _version = headers
        .get("anthropic-version")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if request.stream {
        let model = request.model.clone();
        let stream = async_stream::stream! {
            // message_start
            yield Ok::<_, std::convert::Infallible>(
                format!("event: message_start\ndata: {}\n\n",
                    serde_json::to_string(&serde_json::json!({
                        "type": "message_start",
                        "message": {
                            "id": "msg_mock_123",
                            "type": "message",
                            "role": "assistant",
                            "model": model,
                            "content": [],
                            "usage": { "input_tokens": 10, "output_tokens": 0 }
                        }
                    })).unwrap()
                )
            );

            // content_block_start
            yield Ok(format!("event: content_block_start\ndata: {}\n\n",
                serde_json::to_string(&serde_json::json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": { "type": "text", "text": "" }
                })).unwrap()
            ));

            // content_block_delta chunks
            for word in ["Hello", " from", " mock", " Anthropic", "!"] {
                yield Ok(format!("event: content_block_delta\ndata: {}\n\n",
                    serde_json::to_string(&serde_json::json!({
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": { "type": "text_delta", "text": word }
                    })).unwrap()
                ));
            }

            // content_block_stop
            yield Ok(format!("event: content_block_stop\ndata: {}\n\n",
                serde_json::to_string(&serde_json::json!({
                    "type": "content_block_stop",
                    "index": 0
                })).unwrap()
            ));

            // message_delta with stop reason and usage
            yield Ok(format!("event: message_delta\ndata: {}\n\n",
                serde_json::to_string(&serde_json::json!({
                    "type": "message_delta",
                    "delta": { "stop_reason": "end_turn" },
                    "usage": { "output_tokens": 5 }
                })).unwrap()
            ));

            // message_stop
            yield Ok(format!("event: message_stop\ndata: {}\n\n",
                serde_json::to_string(&serde_json::json!({
                    "type": "message_stop"
                })).unwrap()
            ));
        };

        let body = Body::from_stream(stream);
        axum::response::Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .body(body)
            .unwrap()
            .into_response()
    } else {
        // Check if tools are provided and request is about weather
        let is_tool_request = request.tools.is_some()
            && request.messages.iter().any(|m| {
                let content = m.get("content");
                content
                    .and_then(|c| c.as_str())
                    .map(|s| s.contains("weather"))
                    .unwrap_or_else(|| {
                        // Check content blocks array
                        content
                            .and_then(|c| c.as_array())
                            .map(|blocks| {
                                blocks.iter().any(|b| {
                                    b.get("text")
                                        .and_then(|t| t.as_str())
                                        .map(|s| s.contains("weather"))
                                        .unwrap_or(false)
                                })
                            })
                            .unwrap_or(false)
                    })
            });

        if is_tool_request {
            let response = serde_json::json!({
                "id": "msg_mock_tools",
                "type": "message",
                "role": "assistant",
                "model": request.model,
                "content": [
                    {
                        "type": "tool_use",
                        "id": "toolu_mock_123",
                        "name": "get_weather",
                        "input": {"location": "London"}
                    }
                ],
                "stop_reason": "tool_use",
                "usage": {
                    "input_tokens": 15,
                    "output_tokens": 10
                }
            });
            return (StatusCode::OK, Json(response)).into_response();
        }

        let user_msg = request
            .messages
            .last()
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("Hello");

        let response = serde_json::json!({
            "id": "msg_mock_123",
            "type": "message",
            "role": "assistant",
            "model": request.model,
            "content": [
                {
                    "type": "text",
                    "text": format!("Mock Anthropic response to: {}", user_msg)
                }
            ],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 12
            }
        });

        (StatusCode::OK, Json(response)).into_response()
    }
}

// ============================================================================
// Gemini Mock Server
// ============================================================================

#[allow(dead_code)]
pub struct MockGeminiServer {
    pub addr: SocketAddr,
    pub base_url: String,
    _handle: tokio::task::JoinHandle<()>,
}

impl MockGeminiServer {
    pub async fn start() -> Self {
        // Gemini URLs look like /v1beta/models/gemini-2.5-pro:generateContent?key=...
        // Axum can't handle {param}:suffix, so we use a catch-all and dispatch manually
        let app = Router::new().route("/v1beta/models/{*rest}", post(gemini_dispatch));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{}", addr);

        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        Self {
            addr,
            base_url,
            _handle: handle,
        }
    }
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct GeminiKeyQuery {
    key: Option<String>,
    #[serde(default)]
    alt: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct GeminiGenerateRequest {
    contents: Vec<serde_json::Value>,
    #[serde(default)]
    system_instruction: Option<serde_json::Value>,
    #[serde(default)]
    tools: Option<Vec<serde_json::Value>>,
}

async fn gemini_dispatch(
    axum::extract::Path(rest): axum::extract::Path<String>,
    Query(query): Query<GeminiKeyQuery>,
    Json(request): Json<GeminiGenerateRequest>,
) -> impl IntoResponse {
    if rest.contains(":streamGenerateContent") {
        return gemini_stream_generate_content(query, request)
            .await
            .into_response();
    }
    gemini_generate_content(query, request)
        .await
        .into_response()
}

async fn gemini_generate_content(
    query: GeminiKeyQuery,
    request: GeminiGenerateRequest,
) -> impl IntoResponse {
    if query.key.as_deref().unwrap_or("").is_empty() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": { "code": 401, "message": "API key not valid" }
            })),
        )
            .into_response();
    }

    let user_text = request
        .contents
        .last()
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array())
        .and_then(|arr| arr.first())
        .and_then(|part| part.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("Hello");

    // Check for tool use request
    let is_tool_request = request.tools.is_some() && user_text.contains("weather");
    if is_tool_request {
        let response = serde_json::json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{
                        "functionCall": {
                            "name": "get_weather",
                            "args": {"location": "London"}
                        }
                    }]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 12,
                "candidatesTokenCount": 8,
                "totalTokenCount": 20
            }
        });
        return (StatusCode::OK, Json(response)).into_response();
    }

    let response = serde_json::json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [{ "text": format!("Mock Gemini response to: {}", user_text) }]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 8,
            "candidatesTokenCount": 10,
            "totalTokenCount": 18
        }
    });

    (StatusCode::OK, Json(response)).into_response()
}

async fn gemini_stream_generate_content(
    query: GeminiKeyQuery,
    _request: GeminiGenerateRequest,
) -> impl IntoResponse {
    if query.key.as_deref().unwrap_or("").is_empty() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": { "code": 401, "message": "API key not valid" }
            })),
        )
            .into_response();
    }

    let stream = async_stream::stream! {
        // Gemini streams JSON objects prefixed with "data: "
        for (i, word) in ["Hello", " from", " mock", " Gemini", "!"].iter().enumerate() {
            let chunk = serde_json::json!({
                "candidates": [{
                    "content": {
                        "role": "model",
                        "parts": [{ "text": word }]
                    },
                    "finishReason": if i == 4 { "STOP" } else { "" }
                }],
                "usageMetadata": {
                    "promptTokenCount": 8,
                    "candidatesTokenCount": i + 1,
                    "totalTokenCount": 8 + i + 1
                }
            });
            yield Ok::<_, std::convert::Infallible>(
                format!("data: {}\n\n", serde_json::to_string(&chunk).unwrap())
            );
        }
    };

    let body = Body::from_stream(stream);
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(body)
        .unwrap()
        .into_response()
}
