// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::time::Duration;
use tokio_stream::Stream;

use crate::error::AiGatewayError;
use crate::providers::{external_http_client, AiProvider, ProviderCapability, ProviderInfo};
use crate::types::*;

pub struct GeminiProvider {
    info: ProviderInfo,
    client: reqwest::Client,
}

impl Default for GeminiProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl GeminiProvider {
    pub fn new() -> Self {
        let client = external_http_client(Duration::from_secs(300));

        Self {
            info: ProviderInfo {
                id: "gemini",
                display_name: "Google Gemini",
                default_base_url: "https://generativelanguage.googleapis.com",
                capabilities: &[
                    ProviderCapability::ChatCompletion,
                    ProviderCapability::ChatCompletionStreaming,
                    ProviderCapability::Embeddings,
                    ProviderCapability::ToolUse,
                    ProviderCapability::Vision,
                    ProviderCapability::JsonMode,
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

    /// Translate OpenAI-format request to Gemini generateContent format
    fn translate_request(request: &ChatCompletionRequest) -> GeminiRequest {
        let mut system_instruction = None;
        let mut contents = Vec::new();

        for msg in &request.messages {
            if msg.role == "system" {
                let text = msg
                    .content
                    .as_ref()
                    .and_then(|c| c.as_text())
                    .unwrap_or("")
                    .to_string();
                system_instruction = Some(GeminiContent {
                    role: None,
                    parts: vec![GeminiPart::Text { text }],
                });
                continue;
            }

            // Handle tool result messages (role: "tool" in OpenAI → functionResponse in Gemini)
            if msg.role == "tool" {
                let tool_call_id = msg.tool_call_id.clone().unwrap_or_default();
                let result_text = msg
                    .content
                    .as_ref()
                    .and_then(|c| c.as_text())
                    .unwrap_or("")
                    .to_string();

                // Try to parse as JSON, fall back to wrapping in {"result": "..."}
                let response_value: serde_json::Value = serde_json::from_str(&result_text)
                    .unwrap_or_else(|_| serde_json::json!({"result": result_text}));

                contents.push(GeminiContent {
                    role: Some("user".to_string()),
                    parts: vec![GeminiPart::FunctionResponse {
                        function_response: GeminiFunctionResponse {
                            name: tool_call_id,
                            response: response_value,
                        },
                    }],
                });
                continue;
            }

            let role = match msg.role.as_str() {
                "assistant" => "model",
                _ => "user",
            };

            let mut parts = Vec::new();

            // Translate content
            if let Some(content) = &msg.content {
                match content {
                    MessageContent::Text(text) => {
                        if !text.is_empty() {
                            parts.push(GeminiPart::Text { text: text.clone() });
                        }
                    }
                    MessageContent::Parts(content_parts) => {
                        for part in content_parts {
                            match part.r#type.as_str() {
                                "text" => {
                                    parts.push(GeminiPart::Text {
                                        text: part.text.clone().unwrap_or_default(),
                                    });
                                }
                                "image_url" => {
                                    if let Some(image_url) = &part.image_url {
                                        if let Some(gemini_part) =
                                            Self::translate_image_url(image_url)
                                        {
                                            parts.push(gemini_part);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }

            // For assistant messages, translate tool_calls → functionCall parts
            if msg.role == "assistant" {
                if let Some(tool_calls) = &msg.tool_calls {
                    for tc in tool_calls {
                        if let Some(function) = tc.get("function") {
                            let name = function
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let arguments_str = function
                                .get("arguments")
                                .and_then(|v| v.as_str())
                                .unwrap_or("{}");
                            let args: serde_json::Value = serde_json::from_str(arguments_str)
                                .unwrap_or(serde_json::json!({}));

                            parts.push(GeminiPart::FunctionCall {
                                function_call: GeminiFunctionCall { name, args },
                            });
                        }
                    }
                }
            }

            if parts.is_empty() {
                parts.push(GeminiPart::Text {
                    text: String::new(),
                });
            }

            contents.push(GeminiContent {
                role: Some(role.to_string()),
                parts,
            });
        }

        let generation_config = GeminiGenerationConfig {
            max_output_tokens: request.max_tokens,
            temperature: (!request.model.starts_with("gemini-3"))
                .then_some(request.temperature)
                .flatten(),
            top_p: (!request.model.starts_with("gemini-3"))
                .then_some(request.top_p)
                .flatten(),
            thinking_config: request
                .extra
                .as_ref()
                .and_then(|extra| extra.get("reasoning_effort"))
                .and_then(serde_json::Value::as_str)
                .filter(|_| request.model.starts_with("gemini-3"))
                .map(|level| GeminiThinkingConfig {
                    thinking_level: level.to_string(),
                }),
            stop_sequences: request.stop.as_ref().map(|s| match s {
                StopSequence::Single(s) => vec![s.clone()],
                StopSequence::Multiple(v) => v.clone(),
            }),
            response_mime_type: Self::translate_response_format(&request.response_format),
        };

        // Translate OpenAI tools → Gemini function declarations
        let tools = request.tools.as_ref().map(|openai_tools| {
            let declarations: Vec<GeminiFunctionDeclaration> = openai_tools
                .iter()
                .filter_map(|t| {
                    let function = t.get("function")?;
                    Some(GeminiFunctionDeclaration {
                        name: function.get("name")?.as_str()?.to_string(),
                        description: function
                            .get("description")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        parameters: function
                            .get("parameters")
                            .cloned()
                            .unwrap_or(serde_json::json!({"type": "object", "properties": {}})),
                    })
                })
                .collect();

            vec![GeminiToolConfig {
                function_declarations: declarations,
            }]
        });

        // Translate tool_choice
        let tool_config = request.tool_choice.as_ref().and_then(|tc| {
            if let Some(s) = tc.as_str() {
                match s {
                    "auto" => Some(GeminiToolBehavior {
                        function_calling_config: GeminiFunctionCallingConfig {
                            mode: "AUTO".to_string(),
                            allowed_function_names: None,
                        },
                    }),
                    "none" => Some(GeminiToolBehavior {
                        function_calling_config: GeminiFunctionCallingConfig {
                            mode: "NONE".to_string(),
                            allowed_function_names: None,
                        },
                    }),
                    "required" => Some(GeminiToolBehavior {
                        function_calling_config: GeminiFunctionCallingConfig {
                            mode: "ANY".to_string(),
                            allowed_function_names: None,
                        },
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
                Some(GeminiToolBehavior {
                    function_calling_config: GeminiFunctionCallingConfig {
                        mode: "ANY".to_string(),
                        allowed_function_names: name.map(|n| vec![n]),
                    },
                })
            } else {
                None
            }
        });

        GeminiRequest {
            contents,
            system_instruction,
            generation_config: Some(generation_config),
            tools,
            tool_config,
        }
    }

    /// Translate OpenAI response_format to Gemini response_mime_type
    fn translate_response_format(response_format: &Option<serde_json::Value>) -> Option<String> {
        let rf = response_format.as_ref()?;
        let rf_type = rf.get("type").and_then(|v| v.as_str())?;
        match rf_type {
            "json_object" | "json_schema" => Some("application/json".to_string()),
            _ => None,
        }
    }

    /// Translate an OpenAI image_url content part to Gemini inlineData part.
    /// Supports base64 data URIs and HTTP(S) URLs.
    fn translate_image_url(image_url: &serde_json::Value) -> Option<GeminiPart> {
        let url = image_url.get("url").and_then(|v| v.as_str())?;

        if url.starts_with("data:") {
            // data:image/png;base64,<data>
            let after_data = url.strip_prefix("data:")?;
            let (mime_type, rest) = after_data.split_once(';')?;
            let base64_data = rest.strip_prefix("base64,")?;
            Some(GeminiPart::InlineData {
                inline_data: GeminiInlineData {
                    mime_type: mime_type.to_string(),
                    data: base64_data.to_string(),
                },
            })
        } else if url.starts_with("http://") || url.starts_with("https://") {
            // Gemini supports file URIs via fileData, but for simplicity
            // we use the URL directly via fileData
            Some(GeminiPart::FileData {
                file_data: GeminiFileData {
                    mime_type: guess_mime_from_url(url),
                    file_uri: url.to_string(),
                },
            })
        } else {
            None
        }
    }

    /// Translate Gemini response to OpenAI format
    fn translate_response(
        response: GeminiResponse,
        model: &str,
    ) -> Result<ChatCompletionResponse, AiGatewayError> {
        let candidate =
            response
                .candidates
                .first()
                .ok_or_else(|| AiGatewayError::TranslationError {
                    provider: "gemini".to_string(),
                    reason: "No candidates in Gemini response".to_string(),
                })?;

        let mut text_content = String::new();
        let mut tool_calls: Vec<serde_json::Value> = Vec::new();
        let mut tool_call_index = 0;

        for part in &candidate.content.parts {
            match part {
                GeminiPart::Text { text } => {
                    text_content.push_str(text);
                }
                GeminiPart::FunctionCall { function_call } => {
                    let args_str = serde_json::to_string(&function_call.args)
                        .unwrap_or_else(|_| "{}".to_string());
                    tool_calls.push(serde_json::json!({
                        "id": format!("call_{}", uuid::Uuid::new_v4().to_string().replace('-', "")),
                        "type": "function",
                        "function": {
                            "name": function_call.name,
                            "arguments": args_str
                        }
                    }));
                    tool_call_index += 1;
                }
                _ => {}
            }
        }

        let finish_reason = match candidate.finish_reason.as_deref() {
            Some("STOP") => {
                if !tool_calls.is_empty() {
                    Some("tool_calls".to_string())
                } else {
                    Some("stop".to_string())
                }
            }
            Some("MAX_TOKENS") => Some("length".to_string()),
            Some("SAFETY") => Some("content_filter".to_string()),
            other => other.map(|s| s.to_lowercase()),
        };

        let usage = response.usage_metadata.map(|u| UsageInfo {
            prompt_tokens: u.prompt_token_count,
            completion_tokens: u.candidates_token_count,
            total_tokens: u.total_token_count,
        });

        let message_content = if text_content.is_empty() && !tool_calls.is_empty() {
            None
        } else {
            Some(MessageContent::Text(text_content))
        };

        let tool_calls_opt = if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        };

        // Suppress unused variable warning
        let _ = tool_call_index;

        Ok(ChatCompletionResponse {
            id: format!("chatcmpl-gemini-{}", uuid::Uuid::new_v4()),
            object: "chat.completion".to_string(),
            created: chrono::Utc::now().timestamp(),
            model: model.to_string(),
            choices: vec![ChatCompletionChoice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: message_content,
                    name: None,
                    tool_calls: tool_calls_opt,
                    tool_call_id: None,
                },
                finish_reason,
            }],
            usage,
        })
    }

    /// Translate a single Gemini streaming chunk to OpenAI SSE events
    fn translate_stream_chunk(
        gemini_chunk: &GeminiResponse,
        model: &str,
        request_id: &str,
    ) -> Vec<Bytes> {
        let mut events = Vec::new();

        if let Some(candidate) = gemini_chunk.candidates.first() {
            let mut text_content = String::new();
            let mut tool_call_chunks: Vec<serde_json::Value> = Vec::new();

            for part in &candidate.content.parts {
                match part {
                    GeminiPart::Text { text } => {
                        text_content.push_str(text);
                    }
                    GeminiPart::FunctionCall { function_call } => {
                        let args_str = serde_json::to_string(&function_call.args)
                            .unwrap_or_else(|_| "{}".to_string());
                        let call_id =
                            format!("call_{}", uuid::Uuid::new_v4().to_string().replace('-', ""));
                        tool_call_chunks.push(serde_json::json!({
                            "index": tool_call_chunks.len(),
                            "id": call_id,
                            "type": "function",
                            "function": {
                                "name": function_call.name,
                                "arguments": args_str
                            }
                        }));
                    }
                    _ => {}
                }
            }

            let has_tool_calls = !tool_call_chunks.is_empty();

            if !text_content.is_empty() {
                let chunk = ChatCompletionChunk {
                    id: format!("chatcmpl-{}", request_id),
                    object: "chat.completion.chunk".to_string(),
                    created: chrono::Utc::now().timestamp(),
                    model: model.to_string(),
                    choices: vec![ChatCompletionChunkChoice {
                        index: 0,
                        delta: ChatCompletionDelta {
                            role: None,
                            content: Some(text_content),
                            tool_calls: None,
                        },
                        finish_reason: None,
                    }],
                    usage: None,
                };
                if let Ok(json) = serde_json::to_string(&chunk) {
                    events.push(Bytes::from(format!("data: {}\n\n", json)));
                }
            }

            if has_tool_calls {
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
                            tool_calls: Some(tool_call_chunks),
                        },
                        finish_reason: None,
                    }],
                    usage: None,
                };
                if let Ok(json) = serde_json::to_string(&chunk) {
                    events.push(Bytes::from(format!("data: {}\n\n", json)));
                }
            }

            // If there's a finish reason, send a final chunk
            if let Some(finish_reason) = &candidate.finish_reason {
                let mapped = match finish_reason.as_str() {
                    "STOP" => {
                        if has_tool_calls {
                            "tool_calls"
                        } else {
                            "stop"
                        }
                    }
                    "MAX_TOKENS" => "length",
                    "SAFETY" => "content_filter",
                    other => other,
                };

                // Include usage data from the final chunk if available
                let usage = gemini_chunk.usage_metadata.as_ref().map(|u| UsageInfo {
                    prompt_tokens: u.prompt_token_count,
                    completion_tokens: u.candidates_token_count,
                    total_tokens: u.total_token_count,
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
                        finish_reason: Some(mapped.to_string()),
                    }],
                    usage,
                };
                if let Ok(json) = serde_json::to_string(&chunk) {
                    events.push(Bytes::from(format!("data: {}\n\n", json)));
                }
            }
        }

        events
    }
}

/// Guess MIME type from URL extension
fn guess_mime_from_url(url: &str) -> String {
    let lower = url.to_lowercase();
    if lower.contains(".png") {
        "image/png".to_string()
    } else if lower.contains(".gif") {
        "image/gif".to_string()
    } else if lower.contains(".webp") {
        "image/webp".to_string()
    } else {
        // Default to JPEG
        "image/jpeg".to_string()
    }
}

#[async_trait]
impl AiProvider for GeminiProvider {
    fn info(&self) -> &ProviderInfo {
        &self.info
    }

    fn supports_model(&self, model: &str) -> bool {
        model.to_lowercase().starts_with("gemini-")
    }

    fn available_models(&self) -> Vec<ModelInfo> {
        let models = [
            "gemini-3.6-flash",
            "gemini-3.5-flash",
            "gemini-3.5-flash-lite",
            "gemini-3.1-pro",
            "gemini-3.1-flash-lite",
            "gemini-3-flash",
            "gemini-2.5-pro",
            "gemini-2.5-flash",
            "gemini-2.5-flash-lite",
            "gemini-2-flash",
            "gemini-2-flash-lite",
        ];
        models
            .iter()
            .map(|id| ModelInfo {
                id: id.to_string(),
                object: "model".to_string(),
                owned_by: "google".to_string(),
            })
            .collect()
    }

    async fn chat_completion(
        &self,
        api_key: &str,
        base_url: Option<&str>,
        request: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, AiGatewayError> {
        let base = self.resolve_base_url(base_url);
        let url = format!(
            "{}/v1beta/models/{}:generateContent?key={}",
            base, request.model, api_key
        );

        let gemini_request = Self::translate_request(request);

        let response = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .json(&gemini_request)
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

        let gemini_response: GeminiResponse =
            response
                .json()
                .await
                .map_err(|e| AiGatewayError::TranslationError {
                    provider: "gemini".to_string(),
                    reason: format!("Failed to parse Gemini response: {}", e),
                })?;

        Self::translate_response(gemini_response, &request.model)
    }

    async fn chat_completion_stream(
        &self,
        api_key: &str,
        base_url: Option<&str>,
        request: &ChatCompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<Bytes, AiGatewayError>> + Send>>, AiGatewayError>
    {
        let base = self.resolve_base_url(base_url);
        let url = format!(
            "{}/v1beta/models/{}:streamGenerateContent?alt=sse&key={}",
            base, request.model, api_key
        );

        let gemini_request = Self::translate_request(request);

        let response = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .json(&gemini_request)
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

        let model = request.model.clone();
        let request_id = uuid::Uuid::new_v4().to_string();
        let stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut sent_role = false;

        let translated = async_stream::stream! {
            use tokio_stream::StreamExt;
            let mut stream = stream;

            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        let text = String::from_utf8_lossy(&chunk);
                        buffer.push_str(&text);

                        while let Some(data_start) = buffer.find("data: ") {
                            let data_content_start = data_start + 6;
                            if let Some(end) = find_json_end(&buffer[data_content_start..]) {
                                let json_str = buffer[data_content_start..data_content_start + end].to_string();
                                buffer = buffer[data_content_start + end..].to_string();

                                if let Ok(gemini_chunk) = serde_json::from_str::<GeminiResponse>(&json_str) {
                                    // Send role chunk first time
                                    if !sent_role {
                                        sent_role = true;
                                        let role_chunk = ChatCompletionChunk {
                                            id: format!("chatcmpl-{}", request_id),
                                            object: "chat.completion.chunk".to_string(),
                                            created: chrono::Utc::now().timestamp(),
                                            model: model.clone(),
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
                                        if let Ok(json) = serde_json::to_string(&role_chunk) {
                                            yield Ok(Bytes::from(format!("data: {}\n\n", json)));
                                        }
                                    }

                                    // Use the shared translation helper
                                    let events = GeminiProvider::translate_stream_chunk(
                                        &gemini_chunk, &model, &request_id,
                                    );
                                    for event in events {
                                        yield Ok(event);
                                    }
                                }
                            } else {
                                break; // Incomplete JSON, wait for more data
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

            // Send final DONE
            yield Ok(Bytes::from("data: [DONE]\n\n"));
        };

        Ok(Box::pin(translated))
    }
}

/// Find the end of a JSON object by counting braces
fn find_json_end(s: &str) -> Option<usize> {
    let mut depth = 0;
    let mut in_string = false;
    let mut escaped = false;

    for (i, ch) in s.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '{' if !in_string => depth += 1,
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

// ============================================================================
// Gemini-native types (internal)
// ============================================================================

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiRequest {
    contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<GeminiGenerationConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<GeminiToolConfig>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_config: Option<GeminiToolBehavior>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GeminiContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum GeminiPart {
    Text {
        text: String,
    },
    FunctionCall {
        #[serde(rename = "functionCall")]
        function_call: GeminiFunctionCall,
    },
    FunctionResponse {
        #[serde(rename = "functionResponse")]
        function_response: GeminiFunctionResponse,
    },
    InlineData {
        #[serde(rename = "inlineData")]
        inline_data: GeminiInlineData,
    },
    FileData {
        #[serde(rename = "fileData")]
        file_data: GeminiFileData,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct GeminiFunctionCall {
    name: String,
    args: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct GeminiFunctionResponse {
    name: String,
    response: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiInlineData {
    mime_type: String,
    data: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiFileData {
    mime_type: String,
    file_uri: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiGenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking_config: Option<GeminiThinkingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_sequences: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_mime_type: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiThinkingConfig {
    thinking_level: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiToolConfig {
    function_declarations: Vec<GeminiFunctionDeclaration>,
}

#[derive(Debug, Serialize)]
struct GeminiFunctionDeclaration {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiToolBehavior {
    function_calling_config: GeminiFunctionCallingConfig,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiFunctionCallingConfig {
    mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    allowed_function_names: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiResponse {
    candidates: Vec<GeminiCandidate>,
    #[serde(default)]
    usage_metadata: Option<GeminiUsageMetadata>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiCandidate {
    content: GeminiContent,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiUsageMetadata {
    prompt_token_count: i64,
    candidates_token_count: i64,
    total_token_count: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_request(messages: Vec<ChatMessage>) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "gemini-2.5-pro".to_string(),
            messages,
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
    fn test_gemini_provider_info() {
        let provider = GeminiProvider::new();
        assert_eq!(provider.info().id, "gemini");
        assert!(provider.supports_model("gemini-2.5-pro"));
        assert!(!provider.supports_model("gpt-4o"));
    }

    #[test]
    fn test_translate_request_basic() {
        let request = ChatCompletionRequest {
            model: "gemini-2.5-pro".to_string(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: Some(MessageContent::Text("Be helpful.".to_string())),
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
            temperature: Some(0.5),
            max_tokens: Some(1000),
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

        let translated = GeminiProvider::translate_request(&request);

        assert!(translated.system_instruction.is_some());
        assert_eq!(translated.contents.len(), 1);
        assert_eq!(translated.contents[0].role.as_deref(), Some("user"));
        let config = translated.generation_config.unwrap();
        assert_eq!(config.max_output_tokens, Some(1000));
        assert_eq!(config.temperature, Some(0.5));
    }

    #[test]
    fn test_translate_request_maps_gemini_3_thinking_and_omits_sampling() {
        let mut request = make_request(Vec::new());
        request.model = "gemini-3.6-flash".to_string();
        request.temperature = Some(0.2);
        request.top_p = Some(0.9);
        request.extra = Some(serde_json::Map::from_iter([(
            "reasoning_effort".to_string(),
            serde_json::json!("low"),
        )]));

        let translated = GeminiProvider::translate_request(&request);
        let value = serde_json::to_value(translated).expect("request serializes");

        assert_eq!(
            value["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "low"
        );
        assert!(value["generationConfig"].get("temperature").is_none());
        assert!(value["generationConfig"].get("topP").is_none());
    }

    #[test]
    fn test_translate_response_basic() {
        let response = GeminiResponse {
            candidates: vec![GeminiCandidate {
                content: GeminiContent {
                    role: Some("model".to_string()),
                    parts: vec![GeminiPart::Text {
                        text: "Hello there!".to_string(),
                    }],
                },
                finish_reason: Some("STOP".to_string()),
            }],
            usage_metadata: Some(GeminiUsageMetadata {
                prompt_token_count: 5,
                candidates_token_count: 3,
                total_token_count: 8,
            }),
        };

        let translated = GeminiProvider::translate_response(response, "gemini-2.5-pro").unwrap();

        assert_eq!(translated.choices.len(), 1);
        assert_eq!(
            translated.choices[0].finish_reason,
            Some("stop".to_string())
        );
        let usage = translated.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 5);
        assert_eq!(usage.completion_tokens, 3);
    }

    #[test]
    fn test_find_json_end() {
        assert_eq!(find_json_end(r#"{"a": 1}"#), Some(8));
        assert_eq!(find_json_end(r#"{"a": {"b": 2}}"#), Some(15));
        assert_eq!(find_json_end(r#"{"a": "}"}"#), Some(10));
        assert_eq!(find_json_end(r#"{"incomplete"#), None);
    }

    #[test]
    fn test_role_mapping() {
        let request = make_request(vec![
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("Hi".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: Some(MessageContent::Text("Hello".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
        ]);

        let translated = GeminiProvider::translate_request(&request);
        assert_eq!(translated.contents[0].role.as_deref(), Some("user"));
        assert_eq!(translated.contents[1].role.as_deref(), Some("model"));
    }

    #[test]
    fn test_translate_request_with_tools() {
        let mut request = make_request(vec![ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text("What's the weather?".to_string())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }]);
        request.tools = Some(vec![serde_json::json!({
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
        })]);

        let translated = GeminiProvider::translate_request(&request);

        assert!(translated.tools.is_some());
        let tools = translated.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].function_declarations.len(), 1);
        assert_eq!(tools[0].function_declarations[0].name, "get_weather");
        assert_eq!(
            tools[0].function_declarations[0].description,
            "Get current weather"
        );
    }

    #[test]
    fn test_translate_request_with_tool_result() {
        let request = make_request(vec![
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
                        "arguments": "{\"location\":\"London\"}"
                    }
                })]),
                tool_call_id: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some(MessageContent::Text("Sunny, 22C".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: Some("call_123".to_string()),
            },
        ]);

        let translated = GeminiProvider::translate_request(&request);

        // user message, model message with functionCall, user message with functionResponse
        assert_eq!(translated.contents.len(), 3);

        // Check the assistant message has a functionCall part
        let model_msg = &translated.contents[1];
        assert_eq!(model_msg.role.as_deref(), Some("model"));
        let has_function_call = model_msg
            .parts
            .iter()
            .any(|p| matches!(p, GeminiPart::FunctionCall { .. }));
        assert!(has_function_call, "model message should have functionCall");

        // Check the tool result is a functionResponse
        let tool_msg = &translated.contents[2];
        assert_eq!(tool_msg.role.as_deref(), Some("user"));
        let has_function_response = tool_msg
            .parts
            .iter()
            .any(|p| matches!(p, GeminiPart::FunctionResponse { .. }));
        assert!(
            has_function_response,
            "tool message should have functionResponse"
        );
    }

    #[test]
    fn test_translate_response_with_function_call() {
        let response = GeminiResponse {
            candidates: vec![GeminiCandidate {
                content: GeminiContent {
                    role: Some("model".to_string()),
                    parts: vec![GeminiPart::FunctionCall {
                        function_call: GeminiFunctionCall {
                            name: "get_weather".to_string(),
                            args: serde_json::json!({"location": "London"}),
                        },
                    }],
                },
                finish_reason: Some("STOP".to_string()),
            }],
            usage_metadata: None,
        };

        let translated = GeminiProvider::translate_response(response, "gemini-2.5-pro").unwrap();

        assert_eq!(
            translated.choices[0].finish_reason,
            Some("tool_calls".to_string())
        );
        assert!(translated.choices[0].message.content.is_none());
        let tool_calls = translated.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(
            tool_calls[0]["function"]["name"].as_str().unwrap(),
            "get_weather"
        );
        let args: serde_json::Value =
            serde_json::from_str(tool_calls[0]["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["location"], "London");
    }

    #[test]
    fn test_translate_request_with_image_base64() {
        let request = make_request(vec![ChatMessage {
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
        }]);

        let translated = GeminiProvider::translate_request(&request);
        let parts = &translated.contents[0].parts;
        assert_eq!(parts.len(), 2);

        // Check text part
        assert!(matches!(&parts[0], GeminiPart::Text { text } if text == "What's in this image?"));

        // Check image part
        match &parts[1] {
            GeminiPart::InlineData { inline_data } => {
                assert_eq!(inline_data.mime_type, "image/png");
                assert_eq!(inline_data.data, "iVBORw0KGgo=");
            }
            other => panic!("Expected InlineData, got {:?}", other),
        }
    }

    #[test]
    fn test_translate_request_with_image_url() {
        let request = make_request(vec![ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Parts(vec![ContentPart {
                r#type: "image_url".to_string(),
                text: None,
                image_url: Some(serde_json::json!({
                    "url": "https://example.com/photo.png"
                })),
            }])),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }]);

        let translated = GeminiProvider::translate_request(&request);
        let parts = &translated.contents[0].parts;
        assert_eq!(parts.len(), 1);

        match &parts[0] {
            GeminiPart::FileData { file_data } => {
                assert_eq!(file_data.mime_type, "image/png");
                assert_eq!(file_data.file_uri, "https://example.com/photo.png");
            }
            other => panic!("Expected FileData, got {:?}", other),
        }
    }

    #[test]
    fn test_tool_choice_auto() {
        let mut request = make_request(vec![ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text("Hi".to_string())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }]);
        request.tool_choice = Some(serde_json::json!("auto"));

        let translated = GeminiProvider::translate_request(&request);
        let config = translated.tool_config.unwrap();
        assert_eq!(config.function_calling_config.mode, "AUTO");
    }

    #[test]
    fn test_tool_choice_required() {
        let mut request = make_request(vec![ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text("Hi".to_string())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }]);
        request.tool_choice = Some(serde_json::json!("required"));

        let translated = GeminiProvider::translate_request(&request);
        let config = translated.tool_config.unwrap();
        assert_eq!(config.function_calling_config.mode, "ANY");
    }

    #[test]
    fn test_tool_choice_none() {
        let mut request = make_request(vec![ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text("Hi".to_string())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }]);
        request.tool_choice = Some(serde_json::json!("none"));

        let translated = GeminiProvider::translate_request(&request);
        let config = translated.tool_config.unwrap();
        assert_eq!(config.function_calling_config.mode, "NONE");
    }

    #[test]
    fn test_tool_choice_specific_function() {
        let mut request = make_request(vec![ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text("Hi".to_string())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }]);
        request.tool_choice = Some(serde_json::json!({
            "type": "function",
            "function": {"name": "get_weather"}
        }));

        let translated = GeminiProvider::translate_request(&request);
        let config = translated.tool_config.unwrap();
        assert_eq!(config.function_calling_config.mode, "ANY");
        assert_eq!(
            config.function_calling_config.allowed_function_names,
            Some(vec!["get_weather".to_string()])
        );
    }

    #[test]
    fn test_response_format_json() {
        let mut request = make_request(vec![ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text("Give me JSON".to_string())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }]);
        request.response_format = Some(serde_json::json!({"type": "json_object"}));

        let translated = GeminiProvider::translate_request(&request);
        let config = translated.generation_config.unwrap();
        assert_eq!(
            config.response_mime_type,
            Some("application/json".to_string())
        );
    }

    #[test]
    fn test_translate_stream_chunk_with_function_call() {
        let chunk = GeminiResponse {
            candidates: vec![GeminiCandidate {
                content: GeminiContent {
                    role: Some("model".to_string()),
                    parts: vec![GeminiPart::FunctionCall {
                        function_call: GeminiFunctionCall {
                            name: "get_weather".to_string(),
                            args: serde_json::json!({"location": "Paris"}),
                        },
                    }],
                },
                finish_reason: Some("STOP".to_string()),
            }],
            usage_metadata: None,
        };

        let events = GeminiProvider::translate_stream_chunk(&chunk, "gemini-2.5-pro", "test-id");

        // Should have a tool_calls chunk + a finish_reason chunk
        assert!(events.len() >= 2);

        let first_event = String::from_utf8_lossy(&events[0]);
        assert!(first_event.contains("tool_calls"));
        assert!(first_event.contains("get_weather"));

        let last_event = String::from_utf8_lossy(&events[events.len() - 1]);
        assert!(last_event.contains("tool_calls")); // finish_reason should be "tool_calls"
    }

    #[test]
    fn test_translate_stream_chunk_includes_usage_metadata() {
        let chunk = GeminiResponse {
            candidates: vec![GeminiCandidate {
                content: GeminiContent {
                    role: Some("model".to_string()),
                    parts: vec![GeminiPart::Text {
                        text: "Hello!".to_string(),
                    }],
                },
                finish_reason: Some("STOP".to_string()),
            }],
            usage_metadata: Some(GeminiUsageMetadata {
                prompt_token_count: 10,
                candidates_token_count: 5,
                total_token_count: 15,
            }),
        };

        let events = GeminiProvider::translate_stream_chunk(&chunk, "gemini-2.5-flash", "test-id");

        // Should have a text chunk + a finish_reason chunk with usage
        assert!(events.len() >= 2);

        let last_event = String::from_utf8_lossy(&events[events.len() - 1]);
        assert!(last_event.contains("\"usage\""));
        assert!(last_event.contains("\"prompt_tokens\":10"));
        assert!(last_event.contains("\"completion_tokens\":5"));
        assert!(last_event.contains("\"total_tokens\":15"));
    }

    #[test]
    fn test_translate_stream_chunk_no_usage_without_metadata() {
        let chunk = GeminiResponse {
            candidates: vec![GeminiCandidate {
                content: GeminiContent {
                    role: Some("model".to_string()),
                    parts: vec![GeminiPart::Text {
                        text: "Hello!".to_string(),
                    }],
                },
                finish_reason: Some("STOP".to_string()),
            }],
            usage_metadata: None,
        };

        let events = GeminiProvider::translate_stream_chunk(&chunk, "gemini-2.5-flash", "test-id");
        let last_event = String::from_utf8_lossy(&events[events.len() - 1]);
        // usage should not appear (skip_serializing_if = None)
        assert!(!last_event.contains("\"prompt_tokens\""));
    }

    #[test]
    fn test_guess_mime_from_url() {
        assert_eq!(
            guess_mime_from_url("https://example.com/img.png"),
            "image/png"
        );
        assert_eq!(
            guess_mime_from_url("https://example.com/img.gif"),
            "image/gif"
        );
        assert_eq!(
            guess_mime_from_url("https://example.com/img.webp"),
            "image/webp"
        );
        assert_eq!(
            guess_mime_from_url("https://example.com/img.jpg"),
            "image/jpeg"
        );
        assert_eq!(
            guess_mime_from_url("https://example.com/unknown"),
            "image/jpeg"
        );
    }
}
