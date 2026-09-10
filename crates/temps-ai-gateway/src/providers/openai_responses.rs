// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Translation between Temps' OpenAI-compatible Chat Completions contract and
//! OpenAI's Responses API.
//!
//! The gateway keeps Chat Completions as its stable provider-neutral wire
//! format. Newer OpenAI models can still use `/responses` upstream: this module
//! translates requests, non-streaming responses, and typed Responses SSE events
//! back into the existing contract.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bytes::Bytes;
use moka::future::Cache;
use serde_json::{Map, Value};

use crate::error::AiGatewayError;
use crate::types::{
    ChatCompletionChoice, ChatCompletionChunk, ChatCompletionChunkChoice, ChatCompletionDelta,
    ChatCompletionRequest, ChatCompletionResponse, ChatMessage, MessageContent, UsageInfo,
};

const MAX_REASONING_ITEMS: usize = 32;
const MAX_REASONING_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub(crate) struct ReasoningContext {
    pub upstream_call_id: String,
    pub items: Vec<Value>,
}

pub(crate) type ReasoningContextCache = Cache<String, Arc<ReasoningContext>>;

pub(crate) fn new_reasoning_context_cache() -> ReasoningContextCache {
    Cache::builder()
        // A project-chat turn is capped at one hour. Retain provider context
        // for twice that window so a slow tool loop cannot lose continuity;
        // turns themselves are transient and are cancelled on process restart.
        // 512 × 64 KiB caps cached provider context at roughly 32 MiB.
        .max_capacity(512)
        .time_to_live(std::time::Duration::from_secs(2 * 60 * 60))
        .build()
}

pub(crate) fn reasoning_cache_key(namespace: &str, gateway_call_id: &str) -> String {
    format!("{namespace}:{gateway_call_id}")
}

pub(crate) async fn build_request(
    request: &ChatCompletionRequest,
    reasoning_cache: &ReasoningContextCache,
    cache_namespace: &str,
    stream: bool,
) -> Result<Value, AiGatewayError> {
    let mut instructions = Vec::new();
    let mut input = Vec::new();

    for message in &request.messages {
        if matches!(message.role.as_str(), "system" | "developer") {
            if let Some(text) = message.content.as_ref().and_then(MessageContent::as_text) {
                if !text.is_empty() {
                    instructions.push(text.to_string());
                }
            }
            continue;
        }

        if message.role == "tool" {
            let gateway_call_id = message.tool_call_id.as_deref().ok_or_else(|| {
                translation_error("tool message is missing tool_call_id for Responses input")
            })?;
            let cached = reasoning_cache
                .get(&reasoning_cache_key(cache_namespace, gateway_call_id))
                .await;
            let call_id = cached
                .as_ref()
                .map(|context| context.upstream_call_id.as_str())
                .unwrap_or(gateway_call_id);
            input.push(serde_json::json!({
                "type": "function_call_output",
                "call_id": call_id,
                "output": message_text(message),
            }));
            continue;
        }

        if message.role == "assistant" {
            if let Some(text) = message.content.as_ref().and_then(MessageContent::as_text) {
                if !text.is_empty() {
                    input.push(serde_json::json!({
                        "type": "message",
                        "role": "assistant",
                        "status": "completed",
                        "content": [{
                            "type": "output_text",
                            "text": text,
                            "annotations": []
                        }]
                    }));
                }
            }
            if let Some(tool_calls) = &message.tool_calls {
                let mut reasoning_replayed = false;
                for tool_call in tool_calls {
                    let gateway_call_id = tool_call
                        .get("id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| translation_error("assistant tool call is missing id"))?;
                    let cached = reasoning_cache
                        .get(&reasoning_cache_key(cache_namespace, gateway_call_id))
                        .await;
                    if !reasoning_replayed {
                        if let Some(context) = &cached {
                            input.extend(context.items.clone());
                            reasoning_replayed = true;
                        }
                    }
                    let call_id = cached
                        .as_ref()
                        .map(|context| context.upstream_call_id.as_str())
                        .unwrap_or(gateway_call_id);
                    let function = tool_call.get("function").ok_or_else(|| {
                        translation_error("assistant tool call is missing function")
                    })?;
                    let name = function
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            translation_error("assistant tool call is missing function.name")
                        })?;
                    let arguments = function
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("{}");
                    input.push(serde_json::json!({
                        "type": "function_call",
                        "call_id": call_id,
                        "name": name,
                        "arguments": arguments,
                    }));
                }
            }
            continue;
        }

        input.push(response_message_item(message)?);
    }

    let mut body = serde_json::json!({
        "model": &request.model,
        "input": input,
        "stream": stream,
        // Keep provider state out of OpenAI storage while retaining encrypted
        // reasoning context locally for the immediate tool round-trip.
        "store": false,
        "include": ["reasoning.encrypted_content"],
    });
    if !instructions.is_empty() {
        body["instructions"] = Value::String(instructions.join("\n\n"));
    }
    if let Some(max_tokens) = request.max_tokens {
        body["max_output_tokens"] = serde_json::json!(max_tokens);
    } else if let Some(max_tokens) = request
        .extra
        .as_ref()
        .and_then(|extra| extra.get("max_completion_tokens"))
    {
        body["max_output_tokens"] = max_tokens.clone();
    }
    if let Some(temperature) = request.temperature {
        body["temperature"] = serde_json::json!(temperature);
    }
    if let Some(top_p) = request.top_p {
        body["top_p"] = serde_json::json!(top_p);
    }
    if let Some(user) = &request.user {
        body["safety_identifier"] = Value::String(user.clone());
    }
    if let Some(effort) = request
        .extra
        .as_ref()
        .and_then(|extra| extra.get("reasoning_effort"))
        .and_then(Value::as_str)
        .filter(|effort| *effort != "default")
    {
        body["reasoning"] = serde_json::json!({ "effort": effort });
    }
    if let Some(tools) = &request.tools {
        body["tools"] = Value::Array(
            tools
                .iter()
                .map(response_tool)
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    if let Some(tool_choice) = &request.tool_choice {
        body["tool_choice"] = response_tool_choice(tool_choice)?;
    }
    if let Some(format) = &request.response_format {
        body["text"] = serde_json::json!({ "format": response_text_format(format)? });
    }
    Ok(body)
}

fn response_message_item(message: &ChatMessage) -> Result<Value, AiGatewayError> {
    let content = match message.content.as_ref() {
        Some(MessageContent::Text(text)) => {
            vec![serde_json::json!({ "type": "input_text", "text": text })]
        }
        Some(MessageContent::Parts(parts)) => parts
            .iter()
            .filter_map(|part| match part.r#type.as_str() {
                "text" => part
                    .text
                    .as_ref()
                    .map(|text| serde_json::json!({ "type": "input_text", "text": text })),
                "image_url" => part.image_url.as_ref().map(|image| {
                    let image_url = image.get("url").cloned().unwrap_or_else(|| image.clone());
                    serde_json::json!({ "type": "input_image", "image_url": image_url })
                }),
                _ => None,
            })
            .collect(),
        None => Vec::new(),
    };
    Ok(serde_json::json!({
        "type": "message",
        "role": &message.role,
        "content": content,
    }))
}

fn message_text(message: &ChatMessage) -> String {
    message
        .content
        .as_ref()
        .and_then(MessageContent::as_text)
        .unwrap_or_default()
        .to_string()
}

fn response_tool(tool: &Value) -> Result<Value, AiGatewayError> {
    let function = tool
        .get("function")
        .ok_or_else(|| translation_error("function tool is missing function definition"))?;
    let name = function
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| translation_error("function tool is missing name"))?;
    let mut translated = Map::new();
    translated.insert("type".to_string(), Value::String("function".to_string()));
    translated.insert("name".to_string(), Value::String(name.to_string()));
    translated.insert(
        "parameters".to_string(),
        function
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({"type": "object"})),
    );
    if let Some(description) = function.get("description") {
        translated.insert("description".to_string(), description.clone());
    }
    translated.insert(
        "strict".to_string(),
        function
            .get("strict")
            .cloned()
            .unwrap_or(Value::Bool(false)),
    );
    Ok(Value::Object(translated))
}

fn response_tool_choice(choice: &Value) -> Result<Value, AiGatewayError> {
    if choice.is_string() {
        return Ok(choice.clone());
    }
    let function = choice
        .get("function")
        .ok_or_else(|| translation_error("named tool_choice is missing its function definition"))?;
    let name = function
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| translation_error("named tool_choice is missing function.name"))?;
    Ok(serde_json::json!({ "type": "function", "name": name }))
}

fn response_text_format(format: &Value) -> Result<Value, AiGatewayError> {
    match format.get("type").and_then(Value::as_str) {
        Some("json_schema") => {
            let schema = format.get("json_schema").ok_or_else(|| {
                translation_error("json_schema response format is missing json_schema")
            })?;
            Ok(serde_json::json!({
                "type": "json_schema",
                "name": schema.get("name").cloned().unwrap_or_else(|| Value::String("temps_response".to_string())),
                "strict": schema.get("strict").cloned().unwrap_or(Value::Bool(true)),
                "schema": schema.get("schema").cloned().unwrap_or_else(|| serde_json::json!({"type": "object"})),
            }))
        }
        Some("json_object") => Ok(serde_json::json!({ "type": "json_object" })),
        Some("text") | None => Ok(serde_json::json!({ "type": "text" })),
        Some(other) => Err(translation_error(format!(
            "unsupported Responses text format '{other}'"
        ))),
    }
}

pub(crate) async fn translate_response(
    response: Value,
    reasoning_cache: &ReasoningContextCache,
    cache_namespace: &str,
) -> Result<ChatCompletionResponse, AiGatewayError> {
    let id = string_field(&response, "id")?;
    let model = string_field(&response, "model")?;
    let created = response
        .get("created_at")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| chrono::Utc::now().timestamp());
    let output = response
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| translation_error("Responses result is missing output items"))?;
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    let mut call_mappings = Vec::new();
    for item in output {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => append_output_text(item, &mut text),
            Some("function_call") => {
                let upstream_call_id = string_field(item, "call_id")?;
                let gateway_call_id = uuid::Uuid::new_v4().to_string();
                tool_calls.push(function_call_to_chat(item, &gateway_call_id)?);
                call_mappings.push((gateway_call_id, upstream_call_id));
            }
            _ => {}
        }
    }
    cache_reasoning_context(output, &call_mappings, reasoning_cache, cache_namespace).await?;

    let finish_reason = if !tool_calls.is_empty() {
        "tool_calls"
    } else if response.get("status").and_then(Value::as_str) == Some("incomplete") {
        "length"
    } else {
        "stop"
    };
    Ok(ChatCompletionResponse {
        id,
        object: "chat.completion".to_string(),
        created,
        model,
        choices: vec![ChatCompletionChoice {
            index: 0,
            message: ChatMessage {
                role: "assistant".to_string(),
                content: (!text.is_empty()).then_some(MessageContent::Text(text)),
                name: None,
                tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
                tool_call_id: None,
            },
            finish_reason: Some(finish_reason.to_string()),
        }],
        usage: response.get("usage").map(response_usage),
    })
}

fn append_output_text(item: &Value, text: &mut String) {
    if let Some(content) = item.get("content").and_then(Value::as_array) {
        for part in content {
            if matches!(
                part.get("type").and_then(Value::as_str),
                Some("output_text") | Some("refusal")
            ) {
                if let Some(delta) = part.get("text").and_then(Value::as_str) {
                    text.push_str(delta);
                } else if let Some(delta) = part.get("refusal").and_then(Value::as_str) {
                    text.push_str(delta);
                }
            }
        }
    }
}

fn function_call_to_chat(item: &Value, gateway_call_id: &str) -> Result<Value, AiGatewayError> {
    Ok(serde_json::json!({
        "id": gateway_call_id,
        "type": "function",
        "function": {
            "name": string_field(item, "name")?,
            "arguments": item.get("arguments").and_then(Value::as_str).unwrap_or("{}"),
        }
    }))
}

async fn cache_reasoning_context(
    output: &[Value],
    call_mappings: &[(String, String)],
    cache: &ReasoningContextCache,
    cache_namespace: &str,
) -> Result<(), AiGatewayError> {
    let reasoning = bounded_reasoning_context(output)?;
    for (gateway_call_id, upstream_call_id) in call_mappings {
        cache
            .insert(
                reasoning_cache_key(cache_namespace, gateway_call_id),
                Arc::new(ReasoningContext {
                    upstream_call_id: upstream_call_id.clone(),
                    items: reasoning.clone(),
                }),
            )
            .await;
    }
    Ok(())
}

fn bounded_reasoning_context(output: &[Value]) -> Result<Vec<Value>, AiGatewayError> {
    let reasoning = output
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
        .take(MAX_REASONING_ITEMS + 1)
        .cloned()
        .collect::<Vec<_>>();
    if reasoning.len() > MAX_REASONING_ITEMS {
        return Err(translation_error(
            "Responses reasoning context exceeds item limit",
        ));
    }
    let encoded_size = serde_json::to_vec(&reasoning)
        .map_err(|error| translation_error(format!("failed to size reasoning context: {error}")))?
        .len();
    if encoded_size > MAX_REASONING_BYTES {
        return Err(translation_error(
            "Responses reasoning context exceeds byte limit",
        ));
    }
    Ok(reasoning)
}

fn response_usage(value: &Value) -> UsageInfo {
    let prompt_tokens = value
        .get("input_tokens")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let completion_tokens = value
        .get("output_tokens")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    UsageInfo {
        prompt_tokens,
        completion_tokens,
        total_tokens: value
            .get("total_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(prompt_tokens + completion_tokens),
    }
}

fn string_field(value: &Value, field: &'static str) -> Result<String, AiGatewayError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| translation_error(format!("Responses payload is missing '{field}'")))
}

fn translation_error(reason: impl Into<String>) -> AiGatewayError {
    AiGatewayError::TranslationError {
        provider: "openai".to_string(),
        reason: reason.into(),
    }
}

#[derive(Default)]
pub(crate) struct ResponsesStreamTranslator {
    id: String,
    model: String,
    created: i64,
    calls: HashMap<usize, (String, String, String)>,
    argument_lengths: HashMap<usize, usize>,
    emitted_calls: HashSet<usize>,
}

pub(crate) struct TranslatedEvent {
    pub frames: Vec<Bytes>,
    pub reasoning_context: Option<StreamReasoningContext>,
    pub terminal: bool,
}

type StreamReasoningContext = (Vec<(String, String)>, Vec<Value>);

impl ResponsesStreamTranslator {
    pub fn translate(&mut self, event: &Value) -> Result<TranslatedEvent, AiGatewayError> {
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut frames = Vec::new();
        let mut reasoning_context = None;
        let mut terminal = false;

        match event_type {
            "response.created" | "response.in_progress" => {
                if let Some(response) = event.get("response") {
                    self.capture_response_identity(response);
                }
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                let delta = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !delta.is_empty() {
                    frames.push(self.chunk(Some(delta), None, None)?);
                }
            }
            "response.output_item.added" => {
                let index = event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or_default() as usize;
                if let Some(item) = event.get("item") {
                    if item.get("type").and_then(Value::as_str) == Some("function_call") {
                        let upstream_call_id = string_field(item, "call_id")?;
                        let gateway_call_id = uuid::Uuid::new_v4().to_string();
                        let name = string_field(item, "name")?;
                        self.calls.insert(
                            index,
                            (gateway_call_id.clone(), upstream_call_id, name.clone()),
                        );
                        self.emitted_calls.insert(index);
                        frames.push(self.chunk(
                            None,
                            Some(serde_json::json!({
                                "index": index,
                                "id": gateway_call_id,
                                "type": "function",
                                "function": { "name": name, "arguments": "" }
                            })),
                            None,
                        )?);
                    }
                }
            }
            "response.function_call_arguments.delta" => {
                let index = event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or_default() as usize;
                let delta = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                *self.argument_lengths.entry(index).or_default() += delta.len();
                if !delta.is_empty() {
                    frames.push(self.chunk(
                        None,
                        Some(serde_json::json!({
                            "index": index,
                            "function": { "arguments": delta }
                        })),
                        None,
                    )?);
                }
            }
            "response.function_call_arguments.done" => {
                let index = event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or_default() as usize;
                let arguments = event
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let emitted = self
                    .argument_lengths
                    .get(&index)
                    .copied()
                    .unwrap_or_default();
                if emitted == 0 && !arguments.is_empty() {
                    frames.push(self.chunk(
                        None,
                        Some(serde_json::json!({
                            "index": index,
                            "function": { "arguments": arguments }
                        })),
                        None,
                    )?);
                }
            }
            "response.completed" | "response.incomplete" => {
                let response = event.get("response").ok_or_else(|| {
                    translation_error("terminal Responses event is missing response")
                })?;
                self.capture_response_identity(response);
                let output = response
                    .get("output")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let mut call_mappings = Vec::new();
                for (index, item) in output.iter().enumerate() {
                    if let Some("function_call") = item.get("type").and_then(Value::as_str) {
                        let upstream_call_id = string_field(item, "call_id")?;
                        let gateway_call_id = self
                            .calls
                            .get(&index)
                            .map(|(gateway_call_id, _, _)| gateway_call_id.clone())
                            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                        call_mappings.push((gateway_call_id.clone(), upstream_call_id.clone()));
                        if !self.emitted_calls.contains(&index) {
                            frames.push(self.chunk(
                                None,
                                Some(serde_json::json!({
                                    "index": index,
                                    "id": gateway_call_id,
                                    "type": "function",
                                    "function": {
                                        "name": string_field(item, "name")?,
                                        "arguments": item.get("arguments").and_then(Value::as_str).unwrap_or("{}")
                                    }
                                })),
                                None,
                            )?);
                        }
                    }
                }
                let finish = if !self.calls.is_empty() || !call_mappings.is_empty() {
                    "tool_calls"
                } else if event_type == "response.incomplete" {
                    "length"
                } else {
                    "stop"
                };
                if !call_mappings.is_empty() {
                    reasoning_context = Some((call_mappings, bounded_reasoning_context(&output)?));
                }
                frames.push(self.chunk(None, None, Some((finish, response.get("usage"))))?);
                frames.push(Bytes::from_static(b"data: [DONE]\n\n"));
                terminal = true;
            }
            "response.failed" | "error" => {
                let message = event
                    .pointer("/response/error/message")
                    .or_else(|| event.pointer("/error/message"))
                    .or_else(|| event.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("OpenAI Responses stream failed");
                return Err(AiGatewayError::StreamError {
                    model: self.model.clone(),
                    reason: message.to_string(),
                });
            }
            _ => {}
        }

        Ok(TranslatedEvent {
            frames,
            reasoning_context,
            terminal,
        })
    }

    fn capture_response_identity(&mut self, response: &Value) {
        if let Some(id) = response.get("id").and_then(Value::as_str) {
            self.id = id.to_string();
        }
        if let Some(model) = response.get("model").and_then(Value::as_str) {
            self.model = model.to_string();
        }
        if let Some(created) = response.get("created_at").and_then(Value::as_i64) {
            self.created = created;
        }
    }

    fn chunk(
        &self,
        content: Option<&str>,
        tool_call: Option<Value>,
        finish: Option<(&str, Option<&Value>)>,
    ) -> Result<Bytes, AiGatewayError> {
        let (finish_reason, usage) = match finish {
            Some((reason, usage)) => (Some(reason.to_string()), usage.map(response_usage)),
            None => (None, None),
        };
        let chunk = ChatCompletionChunk {
            id: if self.id.is_empty() {
                "response-pending".to_string()
            } else {
                self.id.clone()
            },
            object: "chat.completion.chunk".to_string(),
            created: self.created,
            model: self.model.clone(),
            choices: vec![ChatCompletionChunkChoice {
                index: 0,
                delta: ChatCompletionDelta {
                    role: None,
                    content: content.map(str::to_string),
                    tool_calls: tool_call.map(|call| vec![call]),
                },
                finish_reason,
            }],
            usage,
        };
        let payload = serde_json::to_string(&chunk).map_err(|error| {
            translation_error(format!("failed to encode translated stream chunk: {error}"))
        })?;
        Ok(Bytes::from(format!("data: {payload}\n\n")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "gpt-5.6-luna".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("What is 2+2?".to_string())),
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
                    "name": "lookup",
                    "description": "Look up project data",
                    "parameters": {"type": "object", "properties": {}}
                }
            })]),
            tool_choice: None,
            response_format: None,
            frequency_penalty: None,
            presence_penalty: None,
            seed: None,
            user: None,
            extra: Some(Map::from_iter([(
                "reasoning_effort".to_string(),
                Value::String("medium".to_string()),
            )])),
        }
    }

    #[tokio::test]
    async fn request_translation_preserves_tools_and_reasoning() {
        let body = build_request(&request(), &new_reasoning_context_cache(), "test", true)
            .await
            .expect("request should translate");
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert_eq!(body["reasoning"]["effort"], "medium");
        assert_eq!(body["tools"][0]["name"], "lookup");
        assert_eq!(body["tools"][0]["strict"], false);
        assert_eq!(body["max_output_tokens"], 200);
    }

    #[tokio::test]
    async fn reasoning_items_are_replayed_for_tool_outputs() {
        let cache = new_reasoning_context_cache();
        cache
            .insert(
                reasoning_cache_key("test", "gateway_call_1"),
                Arc::new(ReasoningContext {
                    upstream_call_id: "call_1".to_string(),
                    items: vec![serde_json::json!({
                        "type": "reasoning",
                        "id": "rs_1",
                        "encrypted_content": "opaque"
                    })],
                }),
            )
            .await;
        let mut req = request();
        req.messages = vec![
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                name: None,
                tool_calls: Some(vec![serde_json::json!({
                    "id": "gateway_call_1",
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
                tool_call_id: Some("gateway_call_1".to_string()),
            },
        ];

        let body = build_request(&req, &cache, "test", false).await.unwrap();
        assert_eq!(body["input"][0]["type"], "reasoning");
        assert_eq!(body["input"][1]["type"], "function_call");
        assert_eq!(body["input"][2]["type"], "function_call_output");
        assert_eq!(body["input"][2]["call_id"], "call_1");
    }

    #[tokio::test]
    async fn response_translation_returns_text_tools_and_caches_reasoning() {
        let cache = new_reasoning_context_cache();
        let translated = translate_response(
            serde_json::json!({
                "id": "resp_1",
                "created_at": 123,
                "model": "gpt-5.6-luna",
                "status": "completed",
                "output": [
                    {"type": "reasoning", "id": "rs_1", "encrypted_content": "opaque"},
                    {"type": "message", "content": [{"type": "output_text", "text": "Checking"}]},
                    {"type": "function_call", "call_id": "call_1", "name": "lookup", "arguments": "{}"}
                ],
                "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}
            }),
            &cache,
            "test",
        )
        .await
        .unwrap();
        assert_eq!(
            translated.choices[0].finish_reason.as_deref(),
            Some("tool_calls")
        );
        assert_eq!(
            translated.choices[0]
                .message
                .content
                .as_ref()
                .and_then(MessageContent::as_text),
            Some("Checking")
        );
        let gateway_call_id = translated.choices[0].message.tool_calls.as_ref().unwrap()[0]["id"]
            .as_str()
            .expect("gateway call id");
        assert_ne!(gateway_call_id, "call_1");
        let cached = cache
            .get(&reasoning_cache_key("test", gateway_call_id))
            .await
            .expect("reasoning mapping");
        assert_eq!(cached.upstream_call_id, "call_1");
        assert_eq!(cached.items[0]["id"], "rs_1");
    }

    #[test]
    fn stream_events_translate_to_chat_chunks() {
        let mut translator = ResponsesStreamTranslator::default();
        translator
            .translate(&serde_json::json!({
                "type": "response.created",
                "response": {"id": "resp_1", "model": "gpt-5.6-luna", "created_at": 123}
            }))
            .unwrap();
        let text = translator
            .translate(&serde_json::json!({
                "type": "response.output_text.delta",
                "delta": "Hell"
            }))
            .unwrap();
        assert!(String::from_utf8_lossy(&text.frames[0]).contains("Hell"));

        let call = translator
            .translate(&serde_json::json!({
                "type": "response.output_item.added",
                "output_index": 1,
                "item": {"type": "function_call", "call_id": "call_1", "name": "lookup", "arguments": ""}
            }))
            .unwrap();
        assert!(!String::from_utf8_lossy(&call.frames[0]).contains("call_1"));

        let done = translator
            .translate(&serde_json::json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_1", "model": "gpt-5.6-luna", "created_at": 123,
                    "output": [
                        {"type": "reasoning", "id": "rs_1", "encrypted_content": "opaque"},
                        {"type": "function_call", "call_id": "call_1", "name": "lookup", "arguments": "{}"}
                    ],
                    "usage": {"input_tokens": 4, "output_tokens": 2, "total_tokens": 6}
                }
            }))
            .unwrap();
        assert!(done.terminal);
        let mappings = done.reasoning_context.unwrap().0;
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].1, "call_1");
        assert_ne!(mappings[0].0, "call_1");
        assert!(String::from_utf8_lossy(done.frames.last().unwrap()).contains("[DONE]"));
    }
}
