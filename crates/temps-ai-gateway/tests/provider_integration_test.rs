// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Integration tests for AI Gateway provider adapters.
//!
//! These tests spin up mock HTTP servers simulating real provider APIs
//! and exercise the full provider adapter pipeline:
//!   Request → Provider translate → HTTP call → Response translate → OpenAI format
//!
//! No real API keys are needed. Tests verify:
//! - OpenAI-compatible wire format is preserved end-to-end
//! - Anthropic translation (request + response + streaming)
//! - Gemini translation (request + response + streaming)
//! - Error propagation from upstream 4xx/5xx
//! - SSE streaming format correctness

mod mock_provider_servers;

use mock_provider_servers::{MockAnthropicServer, MockGeminiServer, MockOpenAiServer};
use temps_ai_gateway::providers::anthropic::AnthropicProvider;
use temps_ai_gateway::providers::gemini::GeminiProvider;
use temps_ai_gateway::providers::openai_compat::OpenAiCompatProvider;
use temps_ai_gateway::providers::AiProvider;
use temps_ai_gateway::types::*;
use tokio_stream::StreamExt;

// ============================================================================
// Test helpers
// ============================================================================

fn simple_chat_request(model: &str) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text("What is 2+2?".to_string())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }],
        stream: false,
        temperature: Some(0.7),
        max_tokens: Some(100),
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

fn chat_request_with_system(model: &str) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![
            ChatMessage {
                role: "system".to_string(),
                content: Some(MessageContent::Text("You are a math tutor.".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("What is 2+2?".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
        ],
        stream: false,
        temperature: None,
        max_tokens: Some(200),
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

fn multi_turn_chat_request(model: &str) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("Hi".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: Some(MessageContent::Text("Hello! How can I help?".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("What is 2+2?".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
        ],
        stream: false,
        temperature: None,
        max_tokens: Some(100),
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

fn streaming_chat_request(model: &str) -> ChatCompletionRequest {
    let mut req = simple_chat_request(model);
    req.stream = true;
    req
}

fn embedding_request(model: &str) -> EmbeddingRequest {
    EmbeddingRequest {
        model: model.to_string(),
        input: EmbeddingInput::Single("Hello world".to_string()),
        encoding_format: None,
        dimensions: None,
    }
}

fn embedding_request_multiple(model: &str) -> EmbeddingRequest {
    EmbeddingRequest {
        model: model.to_string(),
        input: EmbeddingInput::Multiple(vec!["Hello".to_string(), "World".to_string()]),
        encoding_format: None,
        dimensions: None,
    }
}

/// Collect all SSE data chunks from a streaming response into a Vec of strings
async fn collect_sse_chunks(
    stream: std::pin::Pin<
        Box<
            dyn tokio_stream::Stream<
                    Item = Result<bytes::Bytes, temps_ai_gateway::error::AiGatewayError>,
                > + Send,
        >,
    >,
) -> Vec<String> {
    let mut chunks = Vec::new();
    tokio::pin!(stream);
    while let Some(result) = stream.next().await {
        let bytes = result.expect("Stream chunk should not be an error");
        chunks.push(String::from_utf8_lossy(&bytes).to_string());
    }
    chunks
}

/// Parse SSE data lines into ChatCompletionChunk objects
fn parse_sse_chunks(raw_chunks: &[String]) -> Vec<ChatCompletionChunk> {
    let mut parsed = Vec::new();
    for chunk in raw_chunks {
        for line in chunk.lines() {
            if let Some(data) = line.strip_prefix("data: ") {
                if data == "[DONE]" {
                    continue;
                }
                if let Ok(chunk) = serde_json::from_str::<ChatCompletionChunk>(data) {
                    parsed.push(chunk);
                }
            }
        }
    }
    parsed
}

// ============================================================================
// OpenAI Provider Integration Tests
// ============================================================================

#[tokio::test]
async fn test_openai_chat_completion_basic() {
    let server = MockOpenAiServer::start().await;
    let provider = OpenAiCompatProvider::openai();
    let request = simple_chat_request("gpt-4o");

    let response = provider
        .chat_completion("sk-test-key", Some(&server.base_url), &request)
        .await
        .unwrap();

    // Verify OpenAI response format
    assert_eq!(response.object, "chat.completion");
    assert!(response.id.starts_with("chatcmpl-"));
    assert_eq!(response.model, "gpt-4o");
    assert_eq!(response.choices.len(), 1);
    assert_eq!(response.choices[0].index, 0);
    assert_eq!(response.choices[0].message.role, "assistant");
    assert_eq!(response.choices[0].finish_reason, Some("stop".to_string()));

    // Verify content is present
    let content = response.choices[0]
        .message
        .content
        .as_ref()
        .unwrap()
        .as_text()
        .unwrap();
    assert!(content.contains("Mock response to: What is 2+2?"));

    // Verify usage info
    let usage = response.usage.unwrap();
    assert!(usage.prompt_tokens > 0);
    assert!(usage.completion_tokens > 0);
    assert_eq!(
        usage.total_tokens,
        usage.prompt_tokens + usage.completion_tokens
    );
}

#[tokio::test]
async fn test_openai_chat_completion_with_system_message() {
    let server = MockOpenAiServer::start().await;
    let provider = OpenAiCompatProvider::openai();
    let request = chat_request_with_system("gpt-4o");

    let response = provider
        .chat_completion("sk-test-key", Some(&server.base_url), &request)
        .await
        .unwrap();

    assert_eq!(response.choices.len(), 1);
    assert_eq!(response.choices[0].message.role, "assistant");
}

#[tokio::test]
async fn test_openai_chat_completion_multi_turn() {
    let server = MockOpenAiServer::start().await;
    let provider = OpenAiCompatProvider::openai();
    let request = multi_turn_chat_request("gpt-4o");

    let response = provider
        .chat_completion("sk-test-key", Some(&server.base_url), &request)
        .await
        .unwrap();

    assert_eq!(response.choices.len(), 1);
    let content = response.choices[0]
        .message
        .content
        .as_ref()
        .unwrap()
        .as_text()
        .unwrap();
    assert!(content.contains("Mock response to: What is 2+2?"));
}

#[tokio::test]
async fn test_openai_streaming_chat_completion() {
    let server = MockOpenAiServer::start().await;
    let provider = OpenAiCompatProvider::openai();
    let request = streaming_chat_request("gpt-4o");

    let stream = provider
        .chat_completion_stream("sk-test-key", Some(&server.base_url), &request)
        .await
        .unwrap();

    let raw_chunks = collect_sse_chunks(stream).await;
    assert!(!raw_chunks.is_empty(), "Should have received SSE chunks");

    // Concatenate all raw chunks and check for [DONE]
    let full_stream = raw_chunks.join("");
    assert!(
        full_stream.contains("data: "),
        "SSE chunks should contain 'data: ' prefix"
    );
    assert!(
        full_stream.contains("[DONE]"),
        "Stream should end with [DONE]"
    );

    // Parse into structured chunks
    let chunks = parse_sse_chunks(&raw_chunks);
    assert!(!chunks.is_empty(), "Should have parsed SSE chunks");

    // First chunk should have role
    assert_eq!(
        chunks[0].choices[0].delta.role,
        Some("assistant".to_string())
    );
    assert_eq!(chunks[0].object, "chat.completion.chunk");

    // Content chunks should have text
    let full_content: String = chunks
        .iter()
        .filter_map(|c| c.choices[0].delta.content.as_ref())
        .cloned()
        .collect();
    assert!(
        full_content.contains("Hello"),
        "Streamed content should include 'Hello'"
    );

    // Should have a chunk with finish_reason
    let has_stop = chunks
        .iter()
        .any(|c| c.choices[0].finish_reason == Some("stop".to_string()));
    assert!(has_stop, "Stream should include a stop finish_reason chunk");
}

#[tokio::test]
async fn test_openai_responses_supports_reasoning_with_tools() {
    let server = MockOpenAiServer::start().await;
    let provider = OpenAiCompatProvider::openai();
    let mut request = simple_chat_request("gpt-5.6-luna");
    request.tools = Some(vec![serde_json::json!({
        "type": "function",
        "function": {
            "name": "lookup",
            "description": "Look up project data",
            "parameters": {"type": "object", "properties": {}}
        }
    })]);
    request.extra = Some(serde_json::Map::from_iter([(
        "reasoning_effort".to_string(),
        serde_json::json!("medium"),
    )]));

    let response = provider
        .chat_completion("sk-test-key", Some(&server.base_url), &request)
        .await
        .expect("modern OpenAI model should use Responses transport");

    assert_eq!(response.id, "resp_mock");
    assert_eq!(
        response.choices[0].finish_reason.as_deref(),
        Some("tool_calls")
    );
    let gateway_call_id = response.choices[0].message.tool_calls.as_ref().unwrap()[0]["id"]
        .as_str()
        .expect("gateway call id")
        .to_string();
    assert_ne!(gateway_call_id, "call_mock");

    let mut follow_up = simple_chat_request("gpt-5.6-luna");
    follow_up.messages = vec![
        ChatMessage {
            role: "assistant".to_string(),
            content: None,
            name: None,
            tool_calls: Some(vec![serde_json::json!({
                "id": gateway_call_id.clone(),
                "type": "function",
                "function": {"name": "lookup", "arguments": "{}"}
            })]),
            tool_call_id: None,
        },
        ChatMessage {
            role: "tool".to_string(),
            content: Some(MessageContent::Text("4".to_string())),
            name: None,
            tool_calls: None,
            tool_call_id: Some(gateway_call_id),
        },
    ];
    let follow_up_response = provider
        .chat_completion("sk-test-key", Some(&server.base_url), &follow_up)
        .await
        .expect("gateway should restore upstream call id and reasoning context");
    assert_eq!(
        follow_up_response.choices[0]
            .message
            .content
            .as_ref()
            .and_then(MessageContent::as_text),
        Some("Tool result accepted")
    );
}

#[tokio::test]
async fn test_openai_responses_stream_translates_partial_text() {
    let server = MockOpenAiServer::start().await;
    let provider = OpenAiCompatProvider::openai();
    let request = streaming_chat_request("gpt-5.6-luna");

    let stream = provider
        .chat_completion_stream("sk-test-key", Some(&server.base_url), &request)
        .await
        .expect("Responses stream should start");
    let raw_chunks = collect_sse_chunks(stream).await;
    let full_stream = raw_chunks.join("");
    assert!(full_stream.contains("[DONE]"));
    let chunks = parse_sse_chunks(&raw_chunks);
    let content = chunks
        .iter()
        .filter_map(|chunk| chunk.choices[0].delta.content.as_deref())
        .collect::<String>();
    assert_eq!(content, "Hello from Responses");
    assert!(chunks
        .iter()
        .any(|chunk| chunk.choices[0].finish_reason.as_deref() == Some("stop")));
}

#[tokio::test]
async fn test_openai_embeddings() {
    let server = MockOpenAiServer::start().await;
    let provider = OpenAiCompatProvider::openai();
    let request = embedding_request("text-embedding-3-small");

    let response = provider
        .embeddings("sk-test-key", Some(&server.base_url), &request)
        .await
        .unwrap();

    assert_eq!(response.object, "list");
    assert_eq!(response.data.len(), 1);
    assert_eq!(response.data[0].object, "embedding");
    assert_eq!(response.data[0].index, 0);
    assert!(!response.data[0].embedding.is_empty());
    assert_eq!(response.model, "text-embedding-3-small");
    assert!(response.usage.prompt_tokens > 0);
}

#[tokio::test]
async fn test_openai_embeddings_multiple_inputs() {
    let server = MockOpenAiServer::start().await;
    let provider = OpenAiCompatProvider::openai();
    let request = embedding_request_multiple("text-embedding-3-small");

    let response = provider
        .embeddings("sk-test-key", Some(&server.base_url), &request)
        .await
        .unwrap();

    assert_eq!(response.data.len(), 2);
    assert_eq!(response.data[0].index, 0);
    assert_eq!(response.data[1].index, 1);
}

#[tokio::test]
async fn test_openai_upstream_rate_limit_error() {
    let server = MockOpenAiServer::start().await;
    let provider = OpenAiCompatProvider::openai();
    let request = simple_chat_request("gpt-4o");

    let result = provider
        .chat_completion("sk-trigger-429", Some(&server.base_url), &request)
        .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        temps_ai_gateway::error::AiGatewayError::UpstreamError {
            status, message, ..
        } => {
            assert_eq!(status, 429);
            assert!(message.contains("Rate limit"));
        }
        other => panic!("Expected UpstreamError, got: {:?}", other),
    }
}

#[tokio::test]
async fn test_openai_upstream_server_error() {
    let server = MockOpenAiServer::start().await;
    let provider = OpenAiCompatProvider::openai();
    let request = simple_chat_request("gpt-4o");

    let result = provider
        .chat_completion("sk-trigger-500", Some(&server.base_url), &request)
        .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        temps_ai_gateway::error::AiGatewayError::UpstreamError { status, .. } => {
            assert_eq!(status, 500);
        }
        other => panic!("Expected UpstreamError, got: {:?}", other),
    }
}

// ============================================================================
// xAI Provider Integration Tests (OpenAI-compatible)
// ============================================================================

#[tokio::test]
async fn test_xai_chat_completion_via_openai_compat() {
    let server = MockOpenAiServer::start().await;
    let provider = OpenAiCompatProvider::xai();
    let request = simple_chat_request("grok-3");

    let response = provider
        .chat_completion("xai-test-key", Some(&server.base_url), &request)
        .await
        .unwrap();

    assert_eq!(response.object, "chat.completion");
    assert_eq!(response.choices.len(), 1);
    assert_eq!(response.choices[0].message.role, "assistant");
}

#[tokio::test]
async fn test_xai_streaming_via_openai_compat() {
    let server = MockOpenAiServer::start().await;
    let provider = OpenAiCompatProvider::xai();
    let request = streaming_chat_request("grok-3");

    let stream = provider
        .chat_completion_stream("xai-test-key", Some(&server.base_url), &request)
        .await
        .unwrap();

    let raw_chunks = collect_sse_chunks(stream).await;
    assert!(!raw_chunks.is_empty());
    let full = raw_chunks.join("");
    assert!(full.contains("[DONE]"));
}

#[tokio::test]
async fn test_xai_embeddings_not_supported() {
    let provider = OpenAiCompatProvider::xai();
    let request = embedding_request("grok-3");

    let result = provider.embeddings("xai-test-key", None, &request).await;

    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        temps_ai_gateway::error::AiGatewayError::ModelNotFound { .. }
    ));
}

// ============================================================================
// Anthropic Provider Integration Tests
// ============================================================================

#[tokio::test]
async fn test_anthropic_chat_completion_basic() {
    let server = MockAnthropicServer::start().await;
    let provider = AnthropicProvider::new();
    let request = simple_chat_request("claude-sonnet-4-20250514");

    let response = provider
        .chat_completion("sk-ant-test-key", Some(&server.base_url), &request)
        .await
        .unwrap();

    // Must come back in OpenAI format
    assert_eq!(response.object, "chat.completion");
    assert!(response.id.starts_with("chatcmpl-"));
    assert_eq!(response.choices.len(), 1);
    assert_eq!(response.choices[0].message.role, "assistant");
    assert_eq!(response.choices[0].finish_reason, Some("stop".to_string()));

    let content = response.choices[0]
        .message
        .content
        .as_ref()
        .unwrap()
        .as_text()
        .unwrap();
    assert!(content.contains("Mock Anthropic response"));

    let usage = response.usage.unwrap();
    assert!(usage.prompt_tokens > 0);
    assert!(usage.completion_tokens > 0);
}

#[tokio::test]
async fn test_anthropic_chat_completion_with_system_message() {
    let server = MockAnthropicServer::start().await;
    let provider = AnthropicProvider::new();
    let request = chat_request_with_system("claude-sonnet-4-20250514");

    let response = provider
        .chat_completion("sk-ant-test-key", Some(&server.base_url), &request)
        .await
        .unwrap();

    // System messages should be extracted and sent as top-level `system` field
    // The mock server accepts it — success means translation worked
    assert_eq!(response.choices.len(), 1);
    assert_eq!(response.choices[0].message.role, "assistant");
}

#[tokio::test]
async fn test_anthropic_chat_completion_multi_turn() {
    let server = MockAnthropicServer::start().await;
    let provider = AnthropicProvider::new();
    let request = multi_turn_chat_request("claude-sonnet-4-20250514");

    let response = provider
        .chat_completion("sk-ant-test-key", Some(&server.base_url), &request)
        .await
        .unwrap();

    assert_eq!(response.choices.len(), 1);
}

#[tokio::test]
async fn test_anthropic_streaming_chat_completion() {
    let server = MockAnthropicServer::start().await;
    let provider = AnthropicProvider::new();
    let request = streaming_chat_request("claude-sonnet-4-20250514");

    let stream = provider
        .chat_completion_stream("sk-ant-test-key", Some(&server.base_url), &request)
        .await
        .unwrap();

    let raw_chunks = collect_sse_chunks(stream).await;
    assert!(!raw_chunks.is_empty(), "Should receive SSE chunks");

    // Parse into OpenAI-format chunks
    let chunks = parse_sse_chunks(&raw_chunks);
    assert!(!chunks.is_empty(), "Should parse SSE chunks");

    // All chunks must be in OpenAI format
    for chunk in &chunks {
        assert_eq!(chunk.object, "chat.completion.chunk");
        assert!(chunk.id.starts_with("chatcmpl-"));
    }

    // First chunk should have assistant role (from Anthropic's message_start)
    assert_eq!(
        chunks[0].choices[0].delta.role,
        Some("assistant".to_string())
    );

    // Should have content chunks (from content_block_delta translation)
    let full_content: String = chunks
        .iter()
        .filter_map(|c| c.choices[0].delta.content.as_ref())
        .cloned()
        .collect();
    assert!(
        !full_content.is_empty(),
        "Translated stream should have content"
    );
    assert!(
        full_content.contains("Hello"),
        "Streamed content should include translated text"
    );

    // Should end with [DONE] in the raw data
    let full_raw = raw_chunks.join("");
    assert!(
        full_raw.contains("[DONE]"),
        "Translated stream should end with [DONE]"
    );
}

#[tokio::test]
async fn test_anthropic_stop_reason_translation() {
    // end_turn -> stop, max_tokens -> length
    // This is verified by the non-streaming test above checking finish_reason == "stop"
    let server = MockAnthropicServer::start().await;
    let provider = AnthropicProvider::new();
    let request = simple_chat_request("claude-sonnet-4-20250514");

    let response = provider
        .chat_completion("sk-ant-test-key", Some(&server.base_url), &request)
        .await
        .unwrap();

    // Mock server returns "end_turn", translation should convert to "stop"
    assert_eq!(
        response.choices[0].finish_reason,
        Some("stop".to_string()),
        "Anthropic 'end_turn' should be translated to OpenAI 'stop'"
    );
}

// ============================================================================
// Gemini Provider Integration Tests
// ============================================================================

#[tokio::test]
async fn test_gemini_chat_completion_basic() {
    let server = MockGeminiServer::start().await;
    let provider = GeminiProvider::new();
    let request = simple_chat_request("gemini-2.5-pro");

    let response = provider
        .chat_completion("gemini-test-key", Some(&server.base_url), &request)
        .await
        .unwrap();

    // Must come back in OpenAI format
    assert_eq!(response.object, "chat.completion");
    assert!(response.id.starts_with("chatcmpl-gemini-"));
    assert_eq!(response.choices.len(), 1);
    assert_eq!(response.choices[0].message.role, "assistant");
    assert_eq!(response.choices[0].finish_reason, Some("stop".to_string()));

    let content = response.choices[0]
        .message
        .content
        .as_ref()
        .unwrap()
        .as_text()
        .unwrap();
    assert!(content.contains("Mock Gemini response"));

    let usage = response.usage.unwrap();
    assert!(usage.prompt_tokens > 0);
    assert!(usage.completion_tokens > 0);
}

#[tokio::test]
async fn test_gemini_chat_completion_with_system_message() {
    let server = MockGeminiServer::start().await;
    let provider = GeminiProvider::new();
    let request = chat_request_with_system("gemini-2.5-pro");

    let response = provider
        .chat_completion("gemini-test-key", Some(&server.base_url), &request)
        .await
        .unwrap();

    // System messages should be sent as systemInstruction
    assert_eq!(response.choices.len(), 1);
    assert_eq!(response.choices[0].message.role, "assistant");
}

#[tokio::test]
async fn test_gemini_streaming_chat_completion() {
    let server = MockGeminiServer::start().await;
    let provider = GeminiProvider::new();
    let request = streaming_chat_request("gemini-2.5-pro");

    let stream = provider
        .chat_completion_stream("gemini-test-key", Some(&server.base_url), &request)
        .await
        .unwrap();

    let raw_chunks = collect_sse_chunks(stream).await;
    assert!(!raw_chunks.is_empty(), "Should receive SSE chunks");

    // Parse into OpenAI-format chunks
    let chunks = parse_sse_chunks(&raw_chunks);
    assert!(!chunks.is_empty(), "Should parse SSE chunks");

    // All chunks must be OpenAI format
    for chunk in &chunks {
        assert_eq!(chunk.object, "chat.completion.chunk");
    }

    // First content chunk should have role set
    assert_eq!(
        chunks[0].choices[0].delta.role,
        Some("assistant".to_string()),
        "First Gemini chunk should set assistant role"
    );

    // Should have content
    let full_content: String = chunks
        .iter()
        .filter_map(|c| c.choices[0].delta.content.as_ref())
        .cloned()
        .collect();
    assert!(!full_content.is_empty(), "Should have streamed content");

    // Should end with [DONE]
    let full_raw = raw_chunks.join("");
    assert!(full_raw.contains("[DONE]"), "Stream should end with [DONE]");
}

#[tokio::test]
async fn test_gemini_finish_reason_translation() {
    let server = MockGeminiServer::start().await;
    let provider = GeminiProvider::new();
    let request = simple_chat_request("gemini-2.5-pro");

    let response = provider
        .chat_completion("gemini-test-key", Some(&server.base_url), &request)
        .await
        .unwrap();

    // Mock returns "STOP", should be translated to "stop"
    assert_eq!(
        response.choices[0].finish_reason,
        Some("stop".to_string()),
        "Gemini 'STOP' should be translated to OpenAI 'stop'"
    );
}

#[tokio::test]
async fn test_gemini_usage_metadata_translation() {
    let server = MockGeminiServer::start().await;
    let provider = GeminiProvider::new();
    let request = simple_chat_request("gemini-2.5-pro");

    let response = provider
        .chat_completion("gemini-test-key", Some(&server.base_url), &request)
        .await
        .unwrap();

    let usage = response.usage.unwrap();
    // Gemini uses promptTokenCount/candidatesTokenCount/totalTokenCount
    // These should be mapped to prompt_tokens/completion_tokens/total_tokens
    assert_eq!(usage.prompt_tokens, 8);
    assert_eq!(usage.completion_tokens, 10);
    assert_eq!(usage.total_tokens, 18);
}

// ============================================================================
// OpenAI Wire Compatibility Tests
//
// These verify that a standard `openai` SDK would work unmodified with
// our gateway by checking exact field names, types, and structure.
// ============================================================================

#[tokio::test]
async fn test_openai_wire_compat_response_fields() {
    let server = MockOpenAiServer::start().await;
    let provider = OpenAiCompatProvider::openai();
    let request = simple_chat_request("gpt-4o");

    let response = provider
        .chat_completion("sk-test-key", Some(&server.base_url), &request)
        .await
        .unwrap();

    // Serialize to JSON and verify exact field names match OpenAI spec
    let json = serde_json::to_value(&response).unwrap();

    assert!(json.get("id").is_some(), "Response must have 'id' field");
    assert!(
        json.get("object").is_some(),
        "Response must have 'object' field"
    );
    assert!(
        json.get("created").is_some(),
        "Response must have 'created' field"
    );
    assert!(
        json.get("model").is_some(),
        "Response must have 'model' field"
    );
    assert!(
        json.get("choices").is_some(),
        "Response must have 'choices' field"
    );

    let choice = &json["choices"][0];
    assert!(choice.get("index").is_some(), "Choice must have 'index'");
    assert!(
        choice.get("message").is_some(),
        "Choice must have 'message'"
    );
    assert!(
        choice.get("finish_reason").is_some(),
        "Choice must have 'finish_reason'"
    );

    let message = &choice["message"];
    assert!(message.get("role").is_some(), "Message must have 'role'");
    assert!(
        message.get("content").is_some(),
        "Message must have 'content'"
    );

    // Usage must have exact field names
    let usage = &json["usage"];
    assert!(
        usage.get("prompt_tokens").is_some(),
        "Usage must have 'prompt_tokens'"
    );
    assert!(
        usage.get("completion_tokens").is_some(),
        "Usage must have 'completion_tokens'"
    );
    assert!(
        usage.get("total_tokens").is_some(),
        "Usage must have 'total_tokens'"
    );
}

#[tokio::test]
async fn test_openai_wire_compat_streaming_fields() {
    let server = MockOpenAiServer::start().await;
    let provider = OpenAiCompatProvider::openai();
    let request = streaming_chat_request("gpt-4o");

    let stream = provider
        .chat_completion_stream("sk-test-key", Some(&server.base_url), &request)
        .await
        .unwrap();

    let raw_chunks = collect_sse_chunks(stream).await;
    let chunks = parse_sse_chunks(&raw_chunks);
    assert!(!chunks.is_empty());

    // Verify streaming chunk format matches OpenAI spec exactly
    let first_chunk = serde_json::to_value(&chunks[0]).unwrap();
    assert!(first_chunk.get("id").is_some(), "Chunk must have 'id'");
    assert_eq!(
        first_chunk["object"].as_str().unwrap(),
        "chat.completion.chunk"
    );
    assert!(
        first_chunk.get("created").is_some(),
        "Chunk must have 'created'"
    );
    assert!(
        first_chunk.get("model").is_some(),
        "Chunk must have 'model'"
    );
    assert!(
        first_chunk.get("choices").is_some(),
        "Chunk must have 'choices'"
    );

    let choice = &first_chunk["choices"][0];
    assert!(
        choice.get("index").is_some(),
        "Chunk choice must have 'index'"
    );
    assert!(
        choice.get("delta").is_some(),
        "Chunk choice must have 'delta' (not 'message')"
    );
}

#[tokio::test]
async fn test_openai_wire_compat_embedding_response_fields() {
    let server = MockOpenAiServer::start().await;
    let provider = OpenAiCompatProvider::openai();
    let request = embedding_request("text-embedding-3-small");

    let response = provider
        .embeddings("sk-test-key", Some(&server.base_url), &request)
        .await
        .unwrap();

    let json = serde_json::to_value(&response).unwrap();

    assert_eq!(json["object"].as_str().unwrap(), "list");
    assert!(json.get("data").is_some());
    assert!(json.get("model").is_some());
    assert!(json.get("usage").is_some());

    let data = &json["data"][0];
    assert_eq!(data["object"].as_str().unwrap(), "embedding");
    assert!(data.get("embedding").is_some());
    assert!(data.get("index").is_some());

    let usage = &json["usage"];
    assert!(usage.get("prompt_tokens").is_some());
    assert!(usage.get("total_tokens").is_some());
}

#[tokio::test]
async fn test_openai_wire_compat_request_roundtrip() {
    // Verify that a JSON request in the exact format an openai SDK would send
    // can be deserialized, sent through the provider, and return a valid response
    let raw_request = r#"{
        "model": "gpt-4o",
        "messages": [
            {"role": "system", "content": "You are a helpful assistant."},
            {"role": "user", "content": "Say hello"}
        ],
        "temperature": 0.7,
        "max_tokens": 100,
        "stream": false
    }"#;

    let request: ChatCompletionRequest =
        serde_json::from_str(raw_request).expect("SDK-format request must deserialize");

    let server = MockOpenAiServer::start().await;
    let provider = OpenAiCompatProvider::openai();

    let response = provider
        .chat_completion("sk-test-key", Some(&server.base_url), &request)
        .await
        .unwrap();

    // Re-serialize to JSON — this is what the SDK would receive
    let response_json = serde_json::to_string(&response).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&response_json).unwrap();

    // SDK expects these exact fields
    assert!(parsed["id"].is_string());
    assert_eq!(parsed["object"], "chat.completion");
    assert!(parsed["created"].is_number());
    assert!(parsed["choices"].is_array());
    assert!(parsed["choices"][0]["message"]["content"].is_string());
}

#[tokio::test]
async fn test_anthropic_response_conforms_to_openai_format() {
    // Verify that Anthropic responses are translated to pass OpenAI SDK validation
    let server = MockAnthropicServer::start().await;
    let provider = AnthropicProvider::new();
    let request = simple_chat_request("claude-sonnet-4-20250514");

    let response = provider
        .chat_completion("sk-ant-test-key", Some(&server.base_url), &request)
        .await
        .unwrap();

    let json = serde_json::to_value(&response).unwrap();

    // Must have all OpenAI-required fields
    assert!(json["id"].is_string());
    assert_eq!(json["object"], "chat.completion");
    assert!(json["created"].is_number());
    assert!(json["choices"].is_array());
    assert!(json["choices"][0]["message"]["role"].is_string());
    assert!(json["choices"][0]["message"]["content"].is_string());
    assert!(json["choices"][0]["finish_reason"].is_string());
    assert!(json["usage"]["prompt_tokens"].is_number());
    assert!(json["usage"]["completion_tokens"].is_number());
    assert!(json["usage"]["total_tokens"].is_number());
}

#[tokio::test]
async fn test_gemini_response_conforms_to_openai_format() {
    let server = MockGeminiServer::start().await;
    let provider = GeminiProvider::new();
    let request = simple_chat_request("gemini-2.5-pro");

    let response = provider
        .chat_completion("gemini-test-key", Some(&server.base_url), &request)
        .await
        .unwrap();

    let json = serde_json::to_value(&response).unwrap();

    assert!(json["id"].is_string());
    assert_eq!(json["object"], "chat.completion");
    assert!(json["created"].is_number());
    assert!(json["choices"].is_array());
    assert!(json["choices"][0]["message"]["role"].is_string());
    assert!(json["choices"][0]["message"]["content"].is_string());
    assert!(json["choices"][0]["finish_reason"].is_string());
    assert!(json["usage"]["prompt_tokens"].is_number());
    assert!(json["usage"]["completion_tokens"].is_number());
    assert!(json["usage"]["total_tokens"].is_number());
}

// ============================================================================
// Cross-provider consistency tests
// ============================================================================

#[tokio::test]
async fn test_all_providers_return_consistent_response_shape() {
    let openai_server = MockOpenAiServer::start().await;
    let anthropic_server = MockAnthropicServer::start().await;
    let gemini_server = MockGeminiServer::start().await;

    let openai = OpenAiCompatProvider::openai();
    let anthropic = AnthropicProvider::new();
    let gemini = GeminiProvider::new();

    let openai_resp = openai
        .chat_completion(
            "sk-test",
            Some(&openai_server.base_url),
            &simple_chat_request("gpt-4o"),
        )
        .await
        .unwrap();

    let anthropic_resp = anthropic
        .chat_completion(
            "sk-ant-test",
            Some(&anthropic_server.base_url),
            &simple_chat_request("claude-sonnet-4-20250514"),
        )
        .await
        .unwrap();

    let gemini_resp = gemini
        .chat_completion(
            "gemini-key",
            Some(&gemini_server.base_url),
            &simple_chat_request("gemini-2.5-pro"),
        )
        .await
        .unwrap();

    // All must have the same structural shape
    for (name, resp) in [
        ("openai", &openai_resp),
        ("anthropic", &anthropic_resp),
        ("gemini", &gemini_resp),
    ] {
        assert_eq!(
            resp.object, "chat.completion",
            "{} response.object should be 'chat.completion'",
            name
        );
        assert!(
            !resp.id.is_empty(),
            "{} response.id should not be empty",
            name
        );
        assert!(
            resp.created > 0,
            "{} response.created should be positive",
            name
        );
        assert_eq!(resp.choices.len(), 1, "{} should have 1 choice", name);
        assert_eq!(
            resp.choices[0].message.role, "assistant",
            "{} choice.message.role should be 'assistant'",
            name
        );
        assert!(
            resp.choices[0].message.content.is_some(),
            "{} choice.message.content should be present",
            name
        );
        assert!(
            resp.choices[0].finish_reason.is_some(),
            "{} choice.finish_reason should be present",
            name
        );
        assert!(resp.usage.is_some(), "{} usage should be present", name);

        let usage = resp.usage.as_ref().unwrap();
        assert!(
            usage.prompt_tokens > 0,
            "{} prompt_tokens should be positive",
            name
        );
        assert!(
            usage.completion_tokens > 0,
            "{} completion_tokens should be positive",
            name
        );
        assert!(
            usage.total_tokens > 0,
            "{} total_tokens should be positive",
            name
        );
    }
}

#[tokio::test]
async fn test_all_providers_streaming_produces_openai_sse_format() {
    let openai_server = MockOpenAiServer::start().await;
    let anthropic_server = MockAnthropicServer::start().await;
    let gemini_server = MockGeminiServer::start().await;

    let providers: Vec<(&str, Box<dyn AiProvider>, String, &str)> = vec![
        (
            "openai",
            Box::new(OpenAiCompatProvider::openai()),
            openai_server.base_url.clone(),
            "gpt-4o",
        ),
        (
            "anthropic",
            Box::new(AnthropicProvider::new()),
            anthropic_server.base_url.clone(),
            "claude-sonnet-4-20250514",
        ),
        (
            "gemini",
            Box::new(GeminiProvider::new()),
            gemini_server.base_url.clone(),
            "gemini-2.5-pro",
        ),
    ];

    for (name, provider, base_url, model) in &providers {
        let request = streaming_chat_request(model);

        let api_key = match *name {
            "openai" => "sk-test",
            "anthropic" => "sk-ant-test",
            "gemini" => "gemini-key",
            _ => "test-key",
        };

        let stream = provider
            .chat_completion_stream(api_key, Some(base_url), &request)
            .await
            .unwrap_or_else(|e| panic!("{}: stream failed: {:?}", name, e));

        let raw_chunks = collect_sse_chunks(stream).await;
        assert!(!raw_chunks.is_empty(), "{}: should have chunks", name);

        let chunks = parse_sse_chunks(&raw_chunks);
        assert!(!chunks.is_empty(), "{}: should parse chunks", name);

        // Every provider's chunks must be valid OpenAI SSE format
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(
                chunk.object, "chat.completion.chunk",
                "{} chunk[{}].object must be 'chat.completion.chunk'",
                name, i
            );
            assert!(
                !chunk.id.is_empty(),
                "{} chunk[{}].id must not be empty",
                name,
                i
            );
            assert!(
                chunk.created > 0,
                "{} chunk[{}].created must be positive",
                name,
                i
            );
            assert_eq!(
                chunk.choices.len(),
                1,
                "{} chunk[{}] must have 1 choice",
                name,
                i
            );
        }

        // First chunk should have role
        assert_eq!(
            chunks[0].choices[0].delta.role,
            Some("assistant".to_string()),
            "{}: first chunk should have assistant role",
            name
        );

        // Stream should end with [DONE]
        let full_raw = raw_chunks.join("");
        assert!(
            full_raw.contains("[DONE]"),
            "{}: stream must end with [DONE]",
            name
        );
    }
}

// ============================================================================
// Tool calling integration tests
// ============================================================================

fn tool_calling_request(model: &str) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(
                "What's the weather in London?".to_string(),
            )),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }],
        stream: false,
        temperature: None,
        max_tokens: Some(200),
        top_p: None,
        stop: None,
        n: None,
        tools: Some(vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "Get current weather for a location",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "location": {"type": "string", "description": "City name"}
                    },
                    "required": ["location"]
                }
            }
        })]),
        tool_choice: None,
        response_format: None,
        frequency_penalty: None,
        presence_penalty: None,
        seed: None,
        user: None,
        extra: None,
    }
}

#[tokio::test]
async fn test_openai_tool_calling_response() {
    let server = MockOpenAiServer::start().await;
    let provider = OpenAiCompatProvider::openai();

    let request = tool_calling_request("gpt-4o");
    let result = provider
        .chat_completion("sk-test-tools", Some(&server.base_url), &request)
        .await;

    assert!(result.is_ok(), "OpenAI tool call should succeed");
    let response = result.unwrap();

    assert_eq!(response.choices.len(), 1);
    let choice = &response.choices[0];

    // finish_reason should be "tool_calls"
    assert_eq!(choice.finish_reason, Some("tool_calls".to_string()));

    // message.tool_calls should be present
    let tool_calls = choice.message.tool_calls.as_ref().unwrap();
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0]["function"]["name"], "get_weather");
    assert_eq!(tool_calls[0]["type"], "function");

    // arguments should be valid JSON
    let args_str = tool_calls[0]["function"]["arguments"].as_str().unwrap();
    let args: serde_json::Value = serde_json::from_str(args_str).unwrap();
    assert_eq!(args["location"], "London");
}

#[tokio::test]
async fn test_anthropic_tool_calling_translated_to_openai_format() {
    let server = MockAnthropicServer::start().await;
    let provider = AnthropicProvider::new();

    let request = tool_calling_request("claude-sonnet-4-20250514");
    let result = provider
        .chat_completion("test-key", Some(&server.base_url), &request)
        .await;

    assert!(
        result.is_ok(),
        "Anthropic tool call should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap();

    assert_eq!(response.choices.len(), 1);
    let choice = &response.choices[0];

    // finish_reason: "tool_use" in Anthropic → "tool_calls" in OpenAI
    assert_eq!(choice.finish_reason, Some("tool_calls".to_string()));

    // tool_calls should be translated to OpenAI format
    let tool_calls = choice.message.tool_calls.as_ref().unwrap();
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0]["function"]["name"], "get_weather");
    assert_eq!(tool_calls[0]["type"], "function");

    // arguments should be stringified JSON
    let args_str = tool_calls[0]["function"]["arguments"].as_str().unwrap();
    let args: serde_json::Value = serde_json::from_str(args_str).unwrap();
    assert_eq!(args["location"], "London");
}

#[tokio::test]
async fn test_gemini_tool_calling_translated_to_openai_format() {
    let server = MockGeminiServer::start().await;
    let provider = GeminiProvider::new();

    let request = tool_calling_request("gemini-2.5-pro");
    let result = provider
        .chat_completion("test-key", Some(&server.base_url), &request)
        .await;

    assert!(
        result.is_ok(),
        "Gemini tool call should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap();

    assert_eq!(response.choices.len(), 1);
    let choice = &response.choices[0];

    // Gemini returns STOP for function calls, translated to "tool_calls"
    assert_eq!(choice.finish_reason, Some("tool_calls".to_string()));

    // tool_calls should be in OpenAI format
    let tool_calls = choice.message.tool_calls.as_ref().unwrap();
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0]["function"]["name"], "get_weather");
    assert_eq!(tool_calls[0]["type"], "function");

    // arguments should be stringified JSON
    let args_str = tool_calls[0]["function"]["arguments"].as_str().unwrap();
    let args: serde_json::Value = serde_json::from_str(args_str).unwrap();
    assert_eq!(args["location"], "London");
}

#[tokio::test]
async fn test_all_providers_tool_call_response_shape_is_consistent() {
    let openai_server = MockOpenAiServer::start().await;
    let anthropic_server = MockAnthropicServer::start().await;
    let gemini_server = MockGeminiServer::start().await;

    let openai = OpenAiCompatProvider::openai();
    let anthropic = AnthropicProvider::new();
    let gemini = GeminiProvider::new();

    let openai_resp = openai
        .chat_completion(
            "sk-test-tools",
            Some(&openai_server.base_url),
            &tool_calling_request("gpt-4o"),
        )
        .await
        .unwrap();

    let anthropic_resp = anthropic
        .chat_completion(
            "test-key",
            Some(&anthropic_server.base_url),
            &tool_calling_request("claude-sonnet-4-20250514"),
        )
        .await
        .unwrap();

    let gemini_resp = gemini
        .chat_completion(
            "test-key",
            Some(&gemini_server.base_url),
            &tool_calling_request("gemini-2.5-pro"),
        )
        .await
        .unwrap();

    // All providers should return "tool_calls" as finish_reason
    for (name, resp) in [
        ("OpenAI", &openai_resp),
        ("Anthropic", &anthropic_resp),
        ("Gemini", &gemini_resp),
    ] {
        assert_eq!(
            resp.choices[0].finish_reason,
            Some("tool_calls".to_string()),
            "{} finish_reason should be 'tool_calls'",
            name
        );

        let tool_calls = resp.choices[0]
            .message
            .tool_calls
            .as_ref()
            .unwrap_or_else(|| panic!("{} should have tool_calls", name));
        assert!(
            !tool_calls.is_empty(),
            "{} tool_calls should not be empty",
            name
        );

        // Each tool call should have id, type, function.name, function.arguments
        let tc = &tool_calls[0];
        assert!(tc.get("id").is_some(), "{} tool_call should have id", name);
        assert_eq!(
            tc["type"], "function",
            "{} tool_call type should be function",
            name
        );
        assert!(
            tc["function"]["name"].is_string(),
            "{} function.name should be a string",
            name
        );
        assert!(
            tc["function"]["arguments"].is_string(),
            "{} function.arguments should be a string (not object)",
            name
        );
    }
}

// ============================================================================
// SDK tolerance tests
// ============================================================================

#[tokio::test]
async fn test_request_with_extra_sdk_fields_is_accepted() {
    // OpenAI SDK sends fields like stream_options, logprobs etc.
    // Our gateway should accept them without error
    let raw_json = serde_json::json!({
        "model": "gpt-4o",
        "messages": [{"role": "user", "content": "Hi"}],
        "stream": false,
        "stream_options": {"include_usage": true},
        "logprobs": false,
        "top_logprobs": null,
        "parallel_tool_calls": true,
        "logit_bias": {},
        "max_tokens": 100
    });

    let result: Result<ChatCompletionRequest, _> = serde_json::from_value(raw_json);
    assert!(
        result.is_ok(),
        "Should accept extra SDK fields: {:?}",
        result.err()
    );
    let req = result.unwrap();
    assert_eq!(req.model, "gpt-4o");
    assert_eq!(req.max_tokens, Some(100));
}
