use async_trait::async_trait;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::time::Duration;
use tokio_stream::Stream;

use crate::error::AiGatewayError;
use crate::providers::{external_http_client, AiProvider, ProviderCapability, ProviderInfo};
use crate::types::*;

const ANTHROPIC_API_VERSION: &str = "2023-06-01";

pub struct AnthropicProvider {
    info: ProviderInfo,
    client: reqwest::Client,
}

impl Default for AnthropicProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AnthropicProvider {
    pub fn new() -> Self {
        let client = external_http_client(Duration::from_secs(300));

        Self {
            info: ProviderInfo {
                id: "anthropic",
                display_name: "Anthropic",
                default_base_url: "https://api.anthropic.com",
                capabilities: &[
                    ProviderCapability::ChatCompletion,
                    ProviderCapability::ChatCompletionStreaming,
                    ProviderCapability::ToolUse,
                    ProviderCapability::Vision,
                ],
            },
            client,
        }
    }

    fn resolve_base_url(&self, base_url: Option<&str>) -> String {
        base_url
            .unwrap_or(self.info.default_base_url)
            .trim_end_matches('/')
            .to_string()
    }

    /// Translate OpenAI-format request to Anthropic Messages API format
    fn translate_request(request: &ChatCompletionRequest) -> AnthropicRequest {
        let mut system = None;
        let mut messages = Vec::new();

        for msg in &request.messages {
            if msg.role == "system" {
                if let Some(content) = &msg.content {
                    system = Some(content.as_text().unwrap_or("").to_string());
                }
                continue;
            }

            // Handle tool result messages (role: "tool" in OpenAI → tool_result block in Anthropic)
            if msg.role == "tool" {
                let tool_call_id = msg.tool_call_id.clone().unwrap_or_default();
                let result_text = msg
                    .content
                    .as_ref()
                    .and_then(|c| c.as_text())
                    .unwrap_or("")
                    .to_string();

                messages.push(AnthropicMessage {
                    role: "user".to_string(),
                    content: AnthropicContent::Blocks(vec![AnthropicContentBlock::ToolResult {
                        tool_use_id: tool_call_id,
                        content: result_text,
                    }]),
                });
                continue;
            }

            // Build content blocks
            let mut blocks = Vec::new();
            let mut has_blocks = false;

            if let Some(content) = &msg.content {
                match content {
                    MessageContent::Text(text) => {
                        if !text.is_empty() {
                            blocks.push(AnthropicContentBlock::Text { text: text.clone() });
                        }
                    }
                    MessageContent::Parts(parts) => {
                        has_blocks = true;
                        for part in parts {
                            match part.r#type.as_str() {
                                "text" => {
                                    blocks.push(AnthropicContentBlock::Text {
                                        text: part.text.clone().unwrap_or_default(),
                                    });
                                }
                                "image_url" => {
                                    if let Some(image_url) = &part.image_url {
                                        if let Some(block) = Self::translate_image_url(image_url) {
                                            blocks.push(block);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }

            // For assistant messages, also translate tool_calls → tool_use blocks
            if msg.role == "assistant" {
                if let Some(tool_calls) = &msg.tool_calls {
                    has_blocks = true;
                    for tc in tool_calls {
                        if let (Some(id), Some(function)) =
                            (tc.get("id").and_then(|v| v.as_str()), tc.get("function"))
                        {
                            let name = function
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let arguments_str = function
                                .get("arguments")
                                .and_then(|v| v.as_str())
                                .unwrap_or("{}");
                            let input: serde_json::Value = serde_json::from_str(arguments_str)
                                .unwrap_or(serde_json::json!({}));

                            blocks.push(AnthropicContentBlock::ToolUse {
                                id: id.to_string(),
                                name,
                                input,
                            });
                        }
                    }
                }
            }

            let role = match msg.role.as_str() {
                "assistant" => "assistant",
                _ => "user",
            };

            let content = if has_blocks || blocks.len() > 1 {
                AnthropicContent::Blocks(blocks)
            } else if let Some(block) = blocks.into_iter().next() {
                match block {
                    AnthropicContentBlock::Text { text } => AnthropicContent::Text(text),
                    other => AnthropicContent::Blocks(vec![other]),
                }
            } else {
                AnthropicContent::Text(String::new())
            };

            messages.push(AnthropicMessage {
                role: role.to_string(),
                content,
            });
        }

        // Translate OpenAI tools → Anthropic tools
        let tools = request.tools.as_ref().map(|openai_tools| {
            openai_tools
                .iter()
                .filter_map(|t| {
                    let function = t.get("function")?;
                    Some(AnthropicTool {
                        name: function.get("name")?.as_str()?.to_string(),
                        description: function
                            .get("description")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        input_schema: function
                            .get("parameters")
                            .cloned()
                            .unwrap_or(serde_json::json!({"type": "object", "properties": {}})),
                    })
                })
                .collect::<Vec<_>>()
        });

        // Translate tool_choice
        let tool_choice = request.tool_choice.as_ref().and_then(|tc| {
            if let Some(s) = tc.as_str() {
                match s {
                    "auto" => Some(AnthropicToolChoice {
                        r#type: "auto".to_string(),
                        name: None,
                    }),
                    "none" => None, // Anthropic doesn't have "none" — omit tools instead
                    "required" => Some(AnthropicToolChoice {
                        r#type: "any".to_string(),
                        name: None,
                    }),
                    _ => None,
                }
            } else if let Some(obj) = tc.as_object() {
                // {"type": "function", "function": {"name": "my_func"}}
                let name = obj
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .map(|s| s.to_string());
                Some(AnthropicToolChoice {
                    r#type: "tool".to_string(),
                    name,
                })
            } else {
                None
            }
        });

        let effort = request
            .extra
            .as_ref()
            .and_then(|extra| extra.get("reasoning_effort"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let adaptive_model = request.model.starts_with("claude-sonnet-5")
            || request.model.starts_with("claude-opus-5")
            || request.model.starts_with("claude-fable-5");

        AnthropicRequest {
            model: request.model.clone(),
            messages,
            system,
            max_tokens: request.max_tokens.unwrap_or(4096),
            temperature: (!adaptive_model).then_some(request.temperature).flatten(),
            top_p: (!adaptive_model).then_some(request.top_p).flatten(),
            thinking: effort
                .as_ref()
                .filter(|_| adaptive_model)
                .map(|_| AnthropicThinking { r#type: "adaptive" }),
            output_config: effort
                .filter(|_| adaptive_model)
                .map(|effort| AnthropicOutputConfig { effort }),
            stream: request.stream,
            stop_sequences: request.stop.as_ref().map(|s| match s {
                StopSequence::Single(s) => vec![s.clone()],
                StopSequence::Multiple(v) => v.clone(),
            }),
            tools,
            tool_choice,
        }
    }

    /// Translate an OpenAI image_url content part to Anthropic image block.
    /// Supports both URL references and base64 data URIs.
    fn translate_image_url(image_url: &serde_json::Value) -> Option<AnthropicContentBlock> {
        let url = image_url.get("url").and_then(|v| v.as_str())?;

        if url.starts_with("data:") {
            // data:image/jpeg;base64,/9j/4AAQ...
            let after_data = url.strip_prefix("data:")?;
            let (media_type, base64_data) = after_data.split_once(";base64,")?;
            Some(AnthropicContentBlock::Image {
                source: AnthropicImageSource {
                    r#type: "base64".to_string(),
                    media_type: media_type.to_string(),
                    data: base64_data.to_string(),
                },
            })
        } else {
            // URL reference — Anthropic supports url type directly
            Some(AnthropicContentBlock::Image {
                source: AnthropicImageSource {
                    r#type: "url".to_string(),
                    media_type: "image/jpeg".to_string(), // Anthropic infers from URL
                    data: url.to_string(),
                },
            })
        }
    }

    /// Translate Anthropic response to OpenAI format
    fn translate_response(response: AnthropicResponse, model: &str) -> ChatCompletionResponse {
        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        for block in &response.content {
            match block {
                AnthropicContentBlock::Text { text } => {
                    text_parts.push(text.clone());
                }
                AnthropicContentBlock::ToolUse { id, name, input } => {
                    tool_calls.push(serde_json::json!({
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": serde_json::to_string(input).unwrap_or_default()
                        }
                    }));
                }
                AnthropicContentBlock::Image { .. }
                | AnthropicContentBlock::ToolResult { .. }
                | AnthropicContentBlock::Thinking { .. }
                | AnthropicContentBlock::RedactedThinking { .. } => {
                    // Not expected in assistant responses
                }
            }
        }

        let content_text = text_parts.join("");
        let content = if content_text.is_empty() && !tool_calls.is_empty() {
            None
        } else {
            Some(MessageContent::Text(content_text))
        };

        let finish_reason = match response.stop_reason.as_deref() {
            Some("end_turn") => Some("stop".to_string()),
            Some("max_tokens") => Some("length".to_string()),
            Some("tool_use") => Some("tool_calls".to_string()),
            other => other.map(|s| s.to_string()),
        };

        ChatCompletionResponse {
            id: format!("chatcmpl-{}", response.id),
            object: "chat.completion".to_string(),
            created: chrono::Utc::now().timestamp(),
            model: model.to_string(),
            choices: vec![ChatCompletionChoice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content,
                    name: None,
                    tool_calls: if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls)
                    },
                    tool_call_id: None,
                },
                finish_reason,
            }],
            usage: Some(UsageInfo {
                prompt_tokens: response.usage.input_tokens,
                completion_tokens: response.usage.output_tokens,
                total_tokens: response.usage.input_tokens + response.usage.output_tokens,
            }),
        }
    }

    /// Translate Anthropic streaming SSE events to OpenAI SSE format
    fn translate_stream_line(line: &str, request_id: &str, model: &str) -> Option<String> {
        if !line.starts_with("data: ") {
            return None;
        }

        let data = &line[6..];
        let parsed: serde_json::Value = serde_json::from_str(data).ok()?;
        let event_type = parsed.get("type")?.as_str()?;

        match event_type {
            "content_block_delta" => {
                let delta = parsed.get("delta")?;
                let delta_type = delta.get("type")?.as_str()?;

                match delta_type {
                    "text_delta" => {
                        let text = delta.get("text")?.as_str()?;
                        let chunk = ChatCompletionChunk {
                            id: format!("chatcmpl-{}", request_id),
                            object: "chat.completion.chunk".to_string(),
                            created: chrono::Utc::now().timestamp(),
                            model: model.to_string(),
                            choices: vec![ChatCompletionChunkChoice {
                                index: 0,
                                delta: ChatCompletionDelta {
                                    role: None,
                                    content: Some(text.to_string()),
                                    tool_calls: None,
                                },
                                finish_reason: None,
                            }],
                            usage: None,
                        };
                        Some(format!("data: {}\n\n", serde_json::to_string(&chunk).ok()?))
                    }
                    "input_json_delta" => {
                        // Tool call argument streaming
                        let partial_json = delta.get("partial_json")?.as_str()?;
                        let index = parsed.get("index").and_then(|v| v.as_i64()).unwrap_or(0);
                        let chunk = ChatCompletionChunk {
                            id: format!("chatcmpl-{}", request_id),
                            object: "chat.completion.chunk".to_string(),
                            created: chrono::Utc::now().timestamp(),
                            model: model.to_string(),
                            choices: vec![ChatCompletionChunkChoice {
                                index: 0,
                                delta: ChatCompletionDelta {
                                    role: None,
                                    content: None,
                                    tool_calls: Some(vec![serde_json::json!({
                                        "index": index,
                                        "function": {
                                            "arguments": partial_json
                                        }
                                    })]),
                                },
                                finish_reason: None,
                            }],
                            usage: None,
                        };
                        Some(format!("data: {}\n\n", serde_json::to_string(&chunk).ok()?))
                    }
                    _ => None,
                }
            }
            "content_block_start" => {
                // Check if this is a tool_use block start
                let content_block = parsed.get("content_block")?;
                let block_type = content_block.get("type")?.as_str()?;

                if block_type == "tool_use" {
                    let id = content_block.get("id")?.as_str()?;
                    let name = content_block.get("name")?.as_str()?;
                    let index = parsed.get("index").and_then(|v| v.as_i64()).unwrap_or(0);

                    let chunk = ChatCompletionChunk {
                        id: format!("chatcmpl-{}", request_id),
                        object: "chat.completion.chunk".to_string(),
                        created: chrono::Utc::now().timestamp(),
                        model: model.to_string(),
                        choices: vec![ChatCompletionChunkChoice {
                            index: 0,
                            delta: ChatCompletionDelta {
                                role: None,
                                content: None,
                                tool_calls: Some(vec![serde_json::json!({
                                    "index": index,
                                    "id": id,
                                    "type": "function",
                                    "function": {
                                        "name": name,
                                        "arguments": ""
                                    }
                                })]),
                            },
                            finish_reason: None,
                        }],
                        usage: None,
                    };
                    Some(format!("data: {}\n\n", serde_json::to_string(&chunk).ok()?))
                } else {
                    None
                }
            }
            "message_start" => {
                let chunk = ChatCompletionChunk {
                    id: format!("chatcmpl-{}", request_id),
                    object: "chat.completion.chunk".to_string(),
                    created: chrono::Utc::now().timestamp(),
                    model: model.to_string(),
                    choices: vec![ChatCompletionChunkChoice {
                        index: 0,
                        delta: ChatCompletionDelta {
                            role: Some("assistant".to_string()),
                            content: None,
                            tool_calls: None,
                        },
                        finish_reason: None,
                    }],
                    usage: None,
                };
                Some(format!("data: {}\n\n", serde_json::to_string(&chunk).ok()?))
            }
            "message_delta" => {
                let stop_reason = parsed
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(|s| s.as_str());

                let finish_reason = match stop_reason {
                    Some("end_turn") => Some("stop".to_string()),
                    Some("max_tokens") => Some("length".to_string()),
                    Some("tool_use") => Some("tool_calls".to_string()),
                    _ => None,
                };

                let usage = parsed.get("usage").and_then(|u| {
                    Some(UsageInfo {
                        prompt_tokens: 0,
                        completion_tokens: u.get("output_tokens")?.as_i64()?,
                        total_tokens: u.get("output_tokens")?.as_i64()?,
                    })
                });

                let chunk = ChatCompletionChunk {
                    id: format!("chatcmpl-{}", request_id),
                    object: "chat.completion.chunk".to_string(),
                    created: chrono::Utc::now().timestamp(),
                    model: model.to_string(),
                    choices: vec![ChatCompletionChunkChoice {
                        index: 0,
                        delta: ChatCompletionDelta {
                            role: None,
                            content: None,
                            tool_calls: None,
                        },
                        finish_reason,
                    }],
                    usage,
                };
                Some(format!("data: {}\n\n", serde_json::to_string(&chunk).ok()?))
            }
            "message_stop" => Some("data: [DONE]\n\n".to_string()),
            _ => None,
        }
    }
}

#[async_trait]
impl AiProvider for AnthropicProvider {
    fn info(&self) -> &ProviderInfo {
        &self.info
    }

    fn supports_model(&self, model: &str) -> bool {
        model.to_lowercase().starts_with("claude-")
    }

    fn available_models(&self) -> Vec<ModelInfo> {
        vec![
            ModelInfo {
                id: "claude-opus-5".to_string(),
                object: "model".to_string(),
                owned_by: "anthropic".to_string(),
            },
            ModelInfo {
                id: "claude-sonnet-5".to_string(),
                object: "model".to_string(),
                owned_by: "anthropic".to_string(),
            },
            ModelInfo {
                id: "claude-fable-5".to_string(),
                object: "model".to_string(),
                owned_by: "anthropic".to_string(),
            },
            ModelInfo {
                id: "claude-haiku-4-5".to_string(),
                object: "model".to_string(),
                owned_by: "anthropic".to_string(),
            },
        ]
    }

    async fn chat_completion(
        &self,
        api_key: &str,
        base_url: Option<&str>,
        request: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, AiGatewayError> {
        let url = format!("{}/v1/messages", self.resolve_base_url(base_url));
        let anthropic_request = Self::translate_request(request);

        let response = self
            .client
            .post(&url)
            .header("x-api-key", api_key)
            .header("anthropic-version", ANTHROPIC_API_VERSION)
            .header("content-type", "application/json")
            .json(&anthropic_request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Failed to read response body".to_string());
            return Err(AiGatewayError::UpstreamError {
                model: request.model.clone(),
                status: status.as_u16(),
                message: body,
            });
        }

        let anthropic_response: AnthropicResponse =
            response
                .json()
                .await
                .map_err(|e| AiGatewayError::TranslationError {
                    provider: "anthropic".to_string(),
                    reason: format!("Failed to parse Anthropic response: {}", e),
                })?;

        Ok(Self::translate_response(anthropic_response, &request.model))
    }

    async fn chat_completion_stream(
        &self,
        api_key: &str,
        base_url: Option<&str>,
        request: &ChatCompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<Bytes, AiGatewayError>> + Send>>, AiGatewayError>
    {
        let url = format!("{}/v1/messages", self.resolve_base_url(base_url));
        let mut anthropic_request = Self::translate_request(request);
        anthropic_request.stream = true;

        let response = self
            .client
            .post(&url)
            .header("x-api-key", api_key)
            .header("anthropic-version", ANTHROPIC_API_VERSION)
            .header("content-type", "application/json")
            .json(&anthropic_request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Failed to read response body".to_string());
            return Err(AiGatewayError::UpstreamError {
                model: request.model.clone(),
                status: status.as_u16(),
                message: body,
            });
        }

        let request_id = uuid::Uuid::new_v4().to_string();
        let model = request.model.clone();

        let stream = response.bytes_stream();
        let mut buffer = String::new();

        let translated = async_stream::stream! {
            use tokio_stream::StreamExt;
            let mut stream = stream;

            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        let text = String::from_utf8_lossy(&chunk);
                        buffer.push_str(&text);

                        while let Some(newline_pos) = buffer.find('\n') {
                            let line = buffer[..newline_pos].trim().to_string();
                            buffer = buffer[newline_pos + 1..].to_string();

                            if line.is_empty() {
                                continue;
                            }

                            if let Some(translated) = AnthropicProvider::translate_stream_line(
                                &line, &request_id, &model
                            ) {
                                yield Ok(Bytes::from(translated));
                            }
                        }
                    }
                    Err(e) => {
                        yield Err(AiGatewayError::StreamError {
                            model: model.clone(),
                            reason: e.to_string(),
                        });
                    }
                }
            }
        };

        Ok(Box::pin(translated))
    }
}

// ============================================================================
// Anthropic-native types (internal, not exposed)
// ============================================================================

#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    max_tokens: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<AnthropicThinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_config: Option<AnthropicOutputConfig>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_sequences: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<AnthropicToolChoice>,
}

#[derive(Debug, Serialize)]
struct AnthropicThinking {
    r#type: &'static str,
}

#[derive(Debug, Serialize)]
struct AnthropicOutputConfig {
    effort: String,
}

#[derive(Debug, Serialize)]
struct AnthropicTool {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    input_schema: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct AnthropicToolChoice {
    r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AnthropicMessage {
    role: String,
    content: AnthropicContent,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum AnthropicContent {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { source: AnthropicImageSource },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
    #[serde(rename = "thinking")]
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(default)]
        signature: String,
    },
    #[serde(rename = "redacted_thinking")]
    RedactedThinking {
        #[serde(default)]
        data: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct AnthropicImageSource {
    r#type: String,
    media_type: String,
    data: String,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    id: String,
    content: Vec<AnthropicContentBlock>,
    stop_reason: Option<String>,
    usage: AnthropicUsage,
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: i64,
    output_tokens: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_request(model: &str) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: model.to_string(),
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
    fn test_anthropic_provider_info() {
        let provider = AnthropicProvider::new();
        assert_eq!(provider.info().id, "anthropic");
        assert!(provider.supports_model("claude-sonnet-4-20250514"));
        assert!(!provider.supports_model("gpt-4o"));
    }

    #[test]
    fn test_translate_request_basic() {
        let request = ChatCompletionRequest {
            model: "claude-sonnet-4-20250514".to_string(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: Some(MessageContent::Text("You are helpful.".to_string())),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: Some(MessageContent::Text("Hello".to_string())),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
            ],
            stream: false,
            temperature: Some(0.7),
            max_tokens: Some(500),
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
        };

        let translated = AnthropicProvider::translate_request(&request);

        assert_eq!(translated.model, "claude-sonnet-4-20250514");
        assert_eq!(translated.system, Some("You are helpful.".to_string()));
        assert_eq!(translated.messages.len(), 1);
        assert_eq!(translated.messages[0].role, "user");
        assert_eq!(translated.max_tokens, 500);
        assert_eq!(translated.temperature, Some(0.7));
        assert!(translated.tools.is_none());
    }

    #[test]
    fn test_translate_request_maps_claude_5_effort_and_omits_sampling() {
        let mut request = test_request("claude-sonnet-5");
        request.temperature = Some(0.2);
        request.top_p = Some(0.9);
        request.extra = Some(serde_json::Map::from_iter([(
            "reasoning_effort".to_string(),
            serde_json::json!("medium"),
        )]));

        let translated = AnthropicProvider::translate_request(&request);
        let value = serde_json::to_value(translated).expect("request serializes");

        assert_eq!(value["thinking"]["type"], "adaptive");
        assert_eq!(value["output_config"]["effort"], "medium");
        assert!(value.get("temperature").is_none());
        assert!(value.get("top_p").is_none());
    }

    #[test]
    fn test_translate_request_with_tools() {
        let request = ChatCompletionRequest {
            model: "claude-sonnet-4-20250514".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("What's the weather?".to_string())),
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
            tools: Some(vec![serde_json::json!({
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get current weather",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "location": {"type": "string"}
                        },
                        "required": ["location"]
                    }
                }
            })]),
            tool_choice: Some(serde_json::json!("auto")),
            response_format: None,
            frequency_penalty: None,
            presence_penalty: None,
            seed: None,
            user: None,
            extra: None,
        };

        let translated = AnthropicProvider::translate_request(&request);

        let tools = translated.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "get_weather");
        assert_eq!(
            tools[0].description,
            Some("Get current weather".to_string())
        );
        assert!(tools[0].input_schema.get("properties").is_some());

        let tc = translated.tool_choice.unwrap();
        assert_eq!(tc.r#type, "auto");
    }

    #[test]
    fn test_translate_request_with_tool_result() {
        let request = ChatCompletionRequest {
            model: "claude-sonnet-4-20250514".to_string(),
            messages: vec![
                ChatMessage {
                    role: "user".to_string(),
                    content: Some(MessageContent::Text("What's the weather?".to_string())),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
                ChatMessage {
                    role: "assistant".to_string(),
                    content: None,
                    name: None,
                    tool_calls: Some(vec![serde_json::json!({
                        "id": "call_123",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"location\":\"NYC\"}"
                        }
                    })]),
                    tool_call_id: None,
                },
                ChatMessage {
                    role: "tool".to_string(),
                    content: Some(MessageContent::Text("72°F, sunny".to_string())),
                    name: None,
                    tool_calls: None,
                    tool_call_id: Some("call_123".to_string()),
                },
            ],
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
        };

        let translated = AnthropicProvider::translate_request(&request);

        // user, assistant with tool_use, user with tool_result
        assert_eq!(translated.messages.len(), 3);
        assert_eq!(translated.messages[0].role, "user");
        assert_eq!(translated.messages[1].role, "assistant");
        assert_eq!(translated.messages[2].role, "user"); // tool result goes as user

        // Verify assistant message has tool_use block
        let assistant_json = serde_json::to_value(&translated.messages[1].content).unwrap();
        let blocks = assistant_json.as_array().unwrap();
        assert!(blocks
            .iter()
            .any(|b| b.get("type") == Some(&serde_json::json!("tool_use"))));

        // Verify tool result message
        let tool_result_json = serde_json::to_value(&translated.messages[2].content).unwrap();
        let result_blocks = tool_result_json.as_array().unwrap();
        assert!(result_blocks
            .iter()
            .any(|b| b.get("type") == Some(&serde_json::json!("tool_result"))));
    }

    #[test]
    fn test_translate_request_with_image() {
        let request = ChatCompletionRequest {
            model: "claude-sonnet-4-20250514".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Parts(vec![
                    ContentPart {
                        r#type: "text".to_string(),
                        text: Some("What's in this image?".to_string()),
                        image_url: None,
                    },
                    ContentPart {
                        r#type: "image_url".to_string(),
                        text: None,
                        image_url: Some(serde_json::json!({
                            "url": "data:image/png;base64,iVBORw0KGgo="
                        })),
                    },
                ])),
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
        };

        let translated = AnthropicProvider::translate_request(&request);

        let content_json = serde_json::to_value(&translated.messages[0].content).unwrap();
        let blocks = content_json.as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["source"]["type"], "base64");
        assert_eq!(blocks[1]["source"]["media_type"], "image/png");
    }

    #[test]
    fn test_translate_response_with_tool_use() {
        let response = AnthropicResponse {
            id: "msg_123".to_string(),
            content: vec![
                AnthropicContentBlock::Text {
                    text: "Let me check the weather.".to_string(),
                },
                AnthropicContentBlock::ToolUse {
                    id: "toolu_123".to_string(),
                    name: "get_weather".to_string(),
                    input: serde_json::json!({"location": "NYC"}),
                },
            ],
            stop_reason: Some("tool_use".to_string()),
            usage: AnthropicUsage {
                input_tokens: 20,
                output_tokens: 15,
            },
        };

        let translated =
            AnthropicProvider::translate_response(response, "claude-sonnet-4-20250514");

        assert_eq!(
            translated.choices[0].finish_reason,
            Some("tool_calls".to_string())
        );

        let tool_calls = translated.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["id"], "toolu_123");
        assert_eq!(tool_calls[0]["type"], "function");
        assert_eq!(tool_calls[0]["function"]["name"], "get_weather");

        // Arguments should be a JSON string
        let args_str = tool_calls[0]["function"]["arguments"].as_str().unwrap();
        let args: serde_json::Value = serde_json::from_str(args_str).unwrap();
        assert_eq!(args["location"], "NYC");

        // Text content should still be present
        let content = translated.choices[0]
            .message
            .content
            .as_ref()
            .unwrap()
            .as_text()
            .unwrap();
        assert_eq!(content, "Let me check the weather.");
    }

    #[test]
    fn test_translate_response() {
        let response = AnthropicResponse {
            id: "msg_123".to_string(),
            content: vec![AnthropicContentBlock::Text {
                text: "Hello! How can I help?".to_string(),
            }],
            stop_reason: Some("end_turn".to_string()),
            usage: AnthropicUsage {
                input_tokens: 10,
                output_tokens: 8,
            },
        };

        let translated =
            AnthropicProvider::translate_response(response, "claude-sonnet-4-20250514");

        assert_eq!(translated.object, "chat.completion");
        assert_eq!(translated.choices.len(), 1);
        assert_eq!(
            translated.choices[0].finish_reason,
            Some("stop".to_string())
        );
        assert_eq!(
            translated.choices[0].message.content,
            Some(MessageContent::Text("Hello! How can I help?".to_string()))
        );
        assert!(translated.choices[0].message.tool_calls.is_none());
        let usage = translated.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 8);
        assert_eq!(usage.total_tokens, 18);
    }

    #[test]
    fn response_with_thinking_blocks_deserializes_and_returns_visible_text() {
        let response: AnthropicResponse = serde_json::from_value(serde_json::json!({
            "id": "msg_thinking",
            "content": [
                {"type": "thinking", "thinking": "private reasoning", "signature": "sig"},
                {"type": "redacted_thinking", "data": "encrypted"},
                {"type": "text", "text": "Visible answer"}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 4, "output_tokens": 9}
        }))
        .expect("Claude thinking blocks must be accepted");

        let translated = AnthropicProvider::translate_response(response, "claude-sonnet-5");
        assert_eq!(
            translated.choices[0].message.content,
            Some(MessageContent::Text("Visible answer".to_string()))
        );
    }

    #[test]
    fn test_translate_stream_content_delta() {
        let line = r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let result =
            AnthropicProvider::translate_stream_line(line, "test-123", "claude-sonnet-4-20250514");
        assert!(result.is_some());
        let output = result.unwrap();
        assert!(output.starts_with("data: "));
        assert!(output.contains("\"content\":\"Hello\""));
    }

    #[test]
    fn test_translate_stream_tool_use_start() {
        let line = r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_123","name":"get_weather"}}"#;
        let result =
            AnthropicProvider::translate_stream_line(line, "test-123", "claude-sonnet-4-20250514");
        assert!(result.is_some());
        let output = result.unwrap();
        assert!(output.contains("tool_calls"));
        assert!(output.contains("get_weather"));
        assert!(output.contains("toolu_123"));
    }

    #[test]
    fn test_translate_stream_tool_use_delta() {
        let line = r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"loc"}}"#;
        let result =
            AnthropicProvider::translate_stream_line(line, "test-123", "claude-sonnet-4-20250514");
        assert!(result.is_some());
        let output = result.unwrap();
        assert!(output.contains("tool_calls"));
        assert!(output.contains("arguments"));
    }

    #[test]
    fn test_translate_stream_message_stop() {
        let line = r#"data: {"type":"message_stop"}"#;
        let result =
            AnthropicProvider::translate_stream_line(line, "test-123", "claude-sonnet-4-20250514");
        assert_eq!(result, Some("data: [DONE]\n\n".to_string()));
    }

    #[test]
    fn test_translate_stream_ignores_non_data_lines() {
        assert!(
            AnthropicProvider::translate_stream_line("event: message_start", "id", "model")
                .is_none()
        );
        assert!(AnthropicProvider::translate_stream_line("", "id", "model").is_none());
    }

    #[test]
    fn test_default_max_tokens() {
        let request = ChatCompletionRequest {
            model: "claude-sonnet-4-20250514".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("Hi".to_string())),
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
        };

        let translated = AnthropicProvider::translate_request(&request);
        assert_eq!(translated.max_tokens, 4096);
    }

    #[test]
    fn test_translate_image_url_base64() {
        let image_url = serde_json::json!({
            "url": "data:image/jpeg;base64,/9j/4AAQ"
        });
        let block = AnthropicProvider::translate_image_url(&image_url).unwrap();
        match block {
            AnthropicContentBlock::Image { source } => {
                assert_eq!(source.r#type, "base64");
                assert_eq!(source.media_type, "image/jpeg");
                assert_eq!(source.data, "/9j/4AAQ");
            }
            _ => panic!("Expected Image block"),
        }
    }

    #[test]
    fn test_translate_image_url_http() {
        let image_url = serde_json::json!({
            "url": "https://example.com/image.png"
        });
        let block = AnthropicProvider::translate_image_url(&image_url).unwrap();
        match block {
            AnthropicContentBlock::Image { source } => {
                assert_eq!(source.r#type, "url");
                assert_eq!(source.data, "https://example.com/image.png");
            }
            _ => panic!("Expected Image block"),
        }
    }

    #[test]
    fn test_tool_choice_required() {
        let request = ChatCompletionRequest {
            model: "claude-sonnet-4-20250514".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("Hi".to_string())),
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
            tools: Some(vec![serde_json::json!({
                "type": "function",
                "function": {"name": "test", "parameters": {}}
            })]),
            tool_choice: Some(serde_json::json!("required")),
            response_format: None,
            frequency_penalty: None,
            presence_penalty: None,
            seed: None,
            user: None,
            extra: None,
        };

        let translated = AnthropicProvider::translate_request(&request);
        let tc = translated.tool_choice.unwrap();
        assert_eq!(tc.r#type, "any"); // OpenAI "required" → Anthropic "any"
    }

    #[test]
    fn test_tool_choice_specific_function() {
        let request = ChatCompletionRequest {
            model: "claude-sonnet-4-20250514".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("Hi".to_string())),
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
            tool_choice: Some(
                serde_json::json!({"type": "function", "function": {"name": "get_weather"}}),
            ),
            response_format: None,
            frequency_penalty: None,
            presence_penalty: None,
            seed: None,
            user: None,
            extra: None,
        };

        let translated = AnthropicProvider::translate_request(&request);
        let tc = translated.tool_choice.unwrap();
        assert_eq!(tc.r#type, "tool");
        assert_eq!(tc.name, Some("get_weather".to_string()));
    }
}
