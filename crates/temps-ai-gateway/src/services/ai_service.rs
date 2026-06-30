//! ADR-022: the gateway-backed implementation of the general [`AiService`]
//! foundation.
//!
//! Wraps [`GatewayService`] so every internal AI call inherits provider-key
//! resolution, model routing, and per-scope rate/cost governance. Structured
//! output rides the gateway's existing `response_format` plumbing. Best-effort:
//! returns [`AiError`] rather than panicking; callers add the timeout.

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use tracing::debug;

use temps_ai::{
    AiError, AiRequest, AiResponse, AiService, ChatMessage, ChatStreamDelta, ChatTool,
    ChatTurnRequest, ChatTurnResponse, ChatTurnStream, TokenStream, ToolCall,
};

use crate::services::{ByokOverride, GatewayService};
use crate::types::{
    ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse, MessageContent,
};

/// Gateway-backed [`AiService`] (ADR-022).
pub struct GatewayAiService {
    gateway: Arc<GatewayService>,
    db: Arc<DatabaseConnection>,
}

impl GatewayAiService {
    pub fn new(gateway: Arc<GatewayService>, db: Arc<DatabaseConnection>) -> Self {
        Self { gateway, db }
    }

    /// Resolve the model to use: an explicit per-call `model`, else the first
    /// entry of `allowed_models` for a `project:{id}` config if present, else the
    /// instance-scope config. `None` when nothing names a concrete model.
    async fn resolve_model(
        &self,
        project_id: Option<i32>,
        explicit: Option<&str>,
    ) -> Option<String> {
        if let Some(m) = explicit {
            if !m.is_empty() {
                return Some(m.to_string());
            }
        }
        let mut scopes: Vec<String> = Vec::new();
        if let Some(pid) = project_id {
            scopes.push(format!("project:{pid}"));
        }
        scopes.push("instance".to_string());

        let rows = temps_entities::ai_gateway_config::Entity::find()
            .filter(temps_entities::ai_gateway_config::Column::Scope.is_in(scopes.clone()))
            .all(self.db.as_ref())
            .await
            .ok()?;
        for scope in scopes {
            if let Some(row) = rows.iter().find(|r| r.scope == scope) {
                if let Some(model) = first_model(row.allowed_models.as_ref()) {
                    return Some(model);
                }
            }
        }

        // No allow-list configured (the common case). Use the first active
        // provider key: prefer the model the operator pinned on it in the AI
        // Providers UI (`default_model`), else a sensible per-provider default —
        // so AI works as soon as a key is added, with no separate step.
        let key = temps_entities::ai_provider_keys::Entity::find()
            .filter(temps_entities::ai_provider_keys::Column::IsActive.eq(true))
            .one(self.db.as_ref())
            .await
            .ok()??;
        if let Some(m) = key
            .default_model
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(m.to_string());
        }
        default_model_for_provider(&key.provider)
    }
}

/// A safe, low-cost default chat model per provider, used when no allow-list
/// names one. The prefix routes back to the provider via `route_model_to_provider`.
fn default_model_for_provider(provider: &str) -> Option<String> {
    let model = match provider {
        "openai" => "gpt-4o-mini",
        "anthropic" => "claude-3-5-haiku-latest",
        "gemini" => "gemini-1.5-flash",
        "xai" => "grok-2-latest",
        _ => return None,
    };
    Some(model.to_string())
}

/// First model id from an `allowed_models` JSON array (`["a","b"]` -> "a").
/// `None`/non-array/empty -> `None` (NULL means "all allowed", which names no
/// specific model to use for internal calls).
fn first_model(allowed_models: Option<&serde_json::Value>) -> Option<String> {
    allowed_models?
        .as_array()?
        .iter()
        .find_map(|v| v.as_str())
        .map(str::to_string)
}

/// Build the OpenAI-style `response_format` for a requested JSON Schema. Gemini
/// maps this to JSON mode; OpenAI-compatible providers honour `json_schema`.
fn response_format_for(schema: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "type": "json_schema",
        "json_schema": { "name": "temps_response", "schema": schema, "strict": true }
    })
}

/// Pull the assistant text out of the first choice.
fn first_text(resp: &ChatCompletionResponse) -> Option<String> {
    let choice = resp.choices.first()?;
    match choice.message.content.as_ref()? {
        MessageContent::Text(s) => Some(s.clone()),
        MessageContent::Parts(_) => None,
    }
}

/// Render one of our flat [`ChatMessage`]s as an OpenAI-format message value,
/// preserving tool-call / tool-result shape for the agentic loop.
fn message_to_json(m: &ChatMessage) -> serde_json::Value {
    if let Some(tool_call_id) = &m.tool_call_id {
        return serde_json::json!({
            "role": "tool",
            "tool_call_id": tool_call_id,
            "content": m.content,
        });
    }
    if let Some(tool_calls) = &m.tool_calls {
        let calls: Vec<serde_json::Value> = tool_calls
            .iter()
            .map(|tc| {
                serde_json::json!({
                    "id": tc.id,
                    "type": "function",
                    "function": { "name": tc.name, "arguments": tc.arguments },
                })
            })
            .collect();
        let content = if m.content.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::json!(m.content)
        };
        return serde_json::json!({
            "role": "assistant",
            "content": content,
            "tool_calls": calls,
        });
    }
    serde_json::json!({ "role": m.role, "content": m.content })
}

/// OpenAI "function" tool schema for one [`ChatTool`].
fn tool_to_json(t: &ChatTool) -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": t.name,
            "description": t.description,
            "parameters": t.parameters,
        },
    })
}

/// Merge one SSE chunk's `delta.tool_calls` fragments into the per-`index`
/// accumulator. The first fragment for an index carries `id` + `function.name`;
/// later fragments append `function.arguments` text. Each entry is
/// `(id, name, arguments-so-far)`.
fn accumulate_tool_call_deltas(
    pending: &mut Vec<(String, String, String)>,
    deltas: &[serde_json::Value],
) {
    for tc in deltas {
        let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
        while pending.len() <= idx {
            pending.push((String::new(), String::new(), String::new()));
        }
        let slot = &mut pending[idx];
        if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
            if !id.is_empty() {
                slot.0 = id.to_string();
            }
        }
        if let Some(func) = tc.get("function") {
            if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                if !name.is_empty() {
                    slot.1 = name.to_string();
                }
            }
            if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                slot.2.push_str(args);
            }
        }
    }
}

/// Drain the per-index accumulator into fully-assembled [`ToolCall`]s, skipping
/// empty slots and defaulting empty arguments to `{}`.
fn assemble_tool_calls(pending: &mut Vec<(String, String, String)>) -> Vec<ToolCall> {
    pending
        .drain(..)
        .filter(|(id, name, _)| !id.is_empty() || !name.is_empty())
        .map(|(id, name, arguments)| ToolCall {
            id,
            name,
            arguments: if arguments.is_empty() {
                "{}".to_string()
            } else {
                arguments
            },
        })
        .collect()
}

/// Parse one OpenAI tool-call value (`{id, function:{name, arguments}}`).
fn parse_tool_call(v: &serde_json::Value) -> Option<ToolCall> {
    let id = v.get("id")?.as_str()?.to_string();
    let function = v.get("function")?;
    let name = function.get("name")?.as_str()?.to_string();
    let arguments = function
        .get("arguments")
        .and_then(|a| a.as_str())
        .unwrap_or("{}")
        .to_string();
    Some(ToolCall {
        id,
        name,
        arguments,
    })
}

#[async_trait]
impl AiService for GatewayAiService {
    async fn is_available(&self) -> bool {
        self.resolve_model(None, None).await.is_some()
    }

    async fn complete(&self, request: AiRequest) -> Result<AiResponse, AiError> {
        let model = self
            .resolve_model(request.project_id, request.model.as_deref())
            .await
            .ok_or_else(|| AiError::NoModel {
                purpose: request.purpose.clone(),
            })?;

        let mut messages = Vec::new();
        if let Some(system) = &request.system {
            messages.push(serde_json::json!({"role": "system", "content": system}));
        }
        messages.push(serde_json::json!({"role": "user", "content": request.prompt}));

        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
        });
        if let Some(mt) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(mt);
        }
        if let Some(t) = request.temperature {
            body["temperature"] = serde_json::json!(t);
        }
        let wants_json = request.response_schema.is_some();
        if let Some(schema) = &request.response_schema {
            body["response_format"] = response_format_for(schema);
        }

        let chat_req: ChatCompletionRequest =
            serde_json::from_value(body).map_err(|e| AiError::Provider {
                purpose: request.purpose.clone(),
                reason: format!("malformed request: {e}"),
            })?;

        let (resp, _cred) = self
            .gateway
            .chat_completion(&chat_req, &ByokOverride::default())
            .await
            .map_err(|e| {
                debug!(error = %e, purpose = request.purpose, model, "AI completion failed");
                AiError::Provider {
                    purpose: request.purpose.clone(),
                    reason: e.to_string(),
                }
            })?;

        let text = first_text(&resp).unwrap_or_default();
        let json = if wants_json {
            temps_ai::extract_json_block(&text)
        } else {
            None
        };
        Ok(AiResponse {
            text: text.trim().to_string(),
            json,
            model,
        })
    }

    async fn chat(&self, request: ChatTurnRequest) -> Result<ChatTurnResponse, AiError> {
        let model = self
            .resolve_model(request.project_id, request.model.as_deref())
            .await
            .ok_or_else(|| AiError::NoModel {
                purpose: request.purpose.clone(),
            })?;

        let messages: Vec<serde_json::Value> =
            request.messages.iter().map(message_to_json).collect();
        let mut body = serde_json::json!({ "model": model, "messages": messages });
        if !request.tools.is_empty() {
            body["tools"] =
                serde_json::Value::Array(request.tools.iter().map(tool_to_json).collect());
        }
        if let Some(mt) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(mt);
        }
        if let Some(t) = request.temperature {
            body["temperature"] = serde_json::json!(t);
        }

        let chat_req: ChatCompletionRequest =
            serde_json::from_value(body).map_err(|e| AiError::Provider {
                purpose: request.purpose.clone(),
                reason: format!("malformed request: {e}"),
            })?;

        let (resp, _cred) = self
            .gateway
            .chat_completion(&chat_req, &ByokOverride::default())
            .await
            .map_err(|e| AiError::Provider {
                purpose: request.purpose.clone(),
                reason: e.to_string(),
            })?;

        let choice = resp.choices.into_iter().next();
        let (content, tool_calls) = match choice {
            Some(c) => {
                let content = c
                    .message
                    .content
                    .as_ref()
                    .and_then(|mc| mc.as_text())
                    .map(str::to_string)
                    .filter(|s| !s.is_empty());
                let tool_calls = c
                    .message
                    .tool_calls
                    .unwrap_or_default()
                    .iter()
                    .filter_map(parse_tool_call)
                    .collect();
                (content, tool_calls)
            }
            None => (None, Vec::new()),
        };
        Ok(ChatTurnResponse {
            content,
            tool_calls,
        })
    }

    async fn chat_stream(&self, request: ChatTurnRequest) -> Result<TokenStream, AiError> {
        let model = self
            .resolve_model(request.project_id, request.model.as_deref())
            .await
            .ok_or_else(|| AiError::NoModel {
                purpose: request.purpose.clone(),
            })?;

        let messages: Vec<serde_json::Value> = request
            .messages
            .iter()
            .map(|m| serde_json::json!({ "role": m.role, "content": m.content }))
            .collect();
        let mut body = serde_json::json!({ "model": model, "messages": messages, "stream": true });
        if let Some(mt) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(mt);
        }
        if let Some(t) = request.temperature {
            body["temperature"] = serde_json::json!(t);
        }
        let chat_req: ChatCompletionRequest =
            serde_json::from_value(body).map_err(|e| AiError::Provider {
                purpose: request.purpose.clone(),
                reason: format!("malformed request: {e}"),
            })?;

        let purpose = request.purpose.clone();
        let (byte_stream, _cred) = self
            .gateway
            .chat_completion_stream(&chat_req, &ByokOverride::default())
            .await
            .map_err(|e| AiError::Provider {
                purpose: purpose.clone(),
                reason: e.to_string(),
            })?;

        // Parse the gateway's OpenAI-format SSE byte stream into assistant text
        // deltas. `data:` lines may split across byte chunks, so buffer to line
        // boundaries; `data: [DONE]` terminates.
        let token_stream = async_stream::stream! {
            let mut byte_stream = byte_stream;
            let mut buf = String::new();
            while let Some(item) = byte_stream.next().await {
                match item {
                    Ok(bytes) => {
                        buf.push_str(&String::from_utf8_lossy(&bytes));
                        while let Some(nl) = buf.find('\n') {
                            let line: String = buf.drain(..=nl).collect();
                            let line = line.trim();
                            let Some(data) = line.strip_prefix("data:") else { continue };
                            let data = data.trim();
                            if data == "[DONE]" {
                                return;
                            }
                            if let Ok(chunk) = serde_json::from_str::<ChatCompletionChunk>(data) {
                                if let Some(content) = chunk
                                    .choices
                                    .first()
                                    .and_then(|c| c.delta.content.as_ref())
                                {
                                    if !content.is_empty() {
                                        yield Ok(content.clone());
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        yield Err(AiError::Provider {
                            purpose: purpose.clone(),
                            reason: e.to_string(),
                        });
                        return;
                    }
                }
            }
        };
        Ok(Box::pin(token_stream))
    }

    async fn chat_stream_turn(&self, request: ChatTurnRequest) -> Result<ChatTurnStream, AiError> {
        let model = self
            .resolve_model(request.project_id, request.model.as_deref())
            .await
            .ok_or_else(|| AiError::NoModel {
                purpose: request.purpose.clone(),
            })?;

        // Full message serialization (tool-call / tool-result shape preserved) +
        // the tool schemas, so the model can stream tool calls inline — unlike the
        // text-only `chat_stream`, which drops both.
        let messages: Vec<serde_json::Value> =
            request.messages.iter().map(message_to_json).collect();
        let mut body = serde_json::json!({ "model": model, "messages": messages, "stream": true });
        if !request.tools.is_empty() {
            body["tools"] =
                serde_json::Value::Array(request.tools.iter().map(tool_to_json).collect());
        }
        if let Some(mt) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(mt);
        }
        if let Some(t) = request.temperature {
            body["temperature"] = serde_json::json!(t);
        }
        let chat_req: ChatCompletionRequest =
            serde_json::from_value(body).map_err(|e| AiError::Provider {
                purpose: request.purpose.clone(),
                reason: format!("malformed request: {e}"),
            })?;

        let purpose = request.purpose.clone();
        tracing::debug!(
            "chat_stream_turn: model={model} tools={} purpose={purpose}",
            chat_req.tools.as_ref().map(|t| t.len()).unwrap_or(0)
        );
        let (byte_stream, _cred) = self
            .gateway
            .chat_completion_stream(&chat_req, &ByokOverride::default())
            .await
            .map_err(|e| AiError::Provider {
                purpose: purpose.clone(),
                reason: e.to_string(),
            })?;

        // Parse the OpenAI-format SSE byte stream into interleaved text + tool-call
        // deltas. Tool calls arrive incrementally: the first delta for a given
        // `index` carries `id` + `function.name`, and later deltas append
        // `function.arguments` fragments. We accumulate per-index and emit a
        // fully-assembled `ToolCall` once the stream finishes (`finish_reason:
        // tool_calls`, or `[DONE]`/end). `data:` lines can split across byte
        // chunks, so buffer to line boundaries.
        let delta_stream = async_stream::stream! {
            let mut byte_stream = byte_stream;
            let mut buf = String::new();
            // (id, name, arguments-so-far) accumulated per tool-call `index`.
            let mut pending: Vec<(String, String, String)> = Vec::new();

            while let Some(item) = byte_stream.next().await {
                match item {
                    Ok(bytes) => {
                        buf.push_str(&String::from_utf8_lossy(&bytes));
                        while let Some(nl) = buf.find('\n') {
                            let line: String = buf.drain(..=nl).collect();
                            let line = line.trim();
                            let Some(data) = line.strip_prefix("data:") else { continue };
                            let data = data.trim();
                            if data == "[DONE]" {
                                // Flush any accumulated tool calls and finish.
                                for tc in assemble_tool_calls(&mut pending) {
                                    yield Ok(ChatStreamDelta::ToolCall(tc));
                                }
                                return;
                            }
                            let chunk = match serde_json::from_str::<ChatCompletionChunk>(data) {
                                Ok(c) => c,
                                Err(e) => {
                                    tracing::warn!(
                                        "chat_stream_turn: unparsed SSE data ({e}): {}",
                                        data.chars().take(300).collect::<String>()
                                    );
                                    continue;
                                }
                            };
                            let Some(choice) = chunk.choices.first() else { continue };
                            // Text delta -> stream immediately.
                            if let Some(content) = choice.delta.content.as_ref() {
                                if !content.is_empty() {
                                    yield Ok(ChatStreamDelta::Text(content.clone()));
                                }
                            }
                            // Tool-call deltas -> accumulate by index.
                            if let Some(tcs) = choice.delta.tool_calls.as_ref() {
                                accumulate_tool_call_deltas(&mut pending, tcs);
                            }
                            // Some providers signal completion via finish_reason
                            // without a trailing [DONE]; flush there too.
                            if choice
                                .finish_reason
                                .as_deref()
                                .is_some_and(|r| r == "tool_calls")
                            {
                                for tc in assemble_tool_calls(&mut pending) {
                                    yield Ok(ChatStreamDelta::ToolCall(tc));
                                }
                            }
                        }
                    }
                    Err(e) => {
                        yield Err(AiError::Provider {
                            purpose: purpose.clone(),
                            reason: e.to_string(),
                        });
                        return;
                    }
                }
            }
            // Stream ended without an explicit terminator — flush any remainder.
            for tc in assemble_tool_calls(&mut pending) {
                yield Ok(ChatStreamDelta::ToolCall(tc));
            }
        };
        Ok(Box::pin(delta_stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase};
    use temps_core::EncryptionService;

    use crate::services::ProviderKeyService;

    /// Build a [`GatewayAiService`] over the given mock connection. The gateway
    /// half is never exercised by `resolve_model`, but the struct still needs
    /// one; wire a throwaway encryption key + provider-key service through.
    fn service_over(db: DatabaseConnection) -> GatewayAiService {
        let db = Arc::new(db);
        let encryption =
            Arc::new(EncryptionService::new("01234567890123456789012345678901").unwrap());
        let provider_keys = Arc::new(ProviderKeyService::new(db.clone(), encryption));
        let gateway = Arc::new(GatewayService::new(provider_keys));
        GatewayAiService::new(gateway, db)
    }

    fn config_row(
        scope: &str,
        allowed: Option<serde_json::Value>,
    ) -> temps_entities::ai_gateway_config::Model {
        temps_entities::ai_gateway_config::Model {
            id: 1,
            scope: scope.to_string(),
            allowed_models: allowed,
            max_requests_per_minute: None,
            max_cost_per_month_microcents: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn active_key(provider: &str) -> temps_entities::ai_provider_keys::Model {
        temps_entities::ai_provider_keys::Model {
            id: 1,
            provider: provider.to_string(),
            display_name: format!("{provider} key"),
            api_key_encrypted: "enc".to_string(),
            base_url: None,
            default_model: None,
            is_active: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn chat_response(content: Option<MessageContent>) -> ChatCompletionResponse {
        ChatCompletionResponse {
            id: "resp_1".to_string(),
            object: "chat.completion".to_string(),
            created: 0,
            model: "gpt-4o-mini".to_string(),
            choices: vec![crate::types::ChatCompletionChoice {
                index: 0,
                // `crate::types::ChatMessage` (wire shape) — distinct from the
                // `temps_ai::ChatMessage` imported at module scope.
                message: crate::types::ChatMessage {
                    role: "assistant".to_string(),
                    content,
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: None,
        }
    }

    #[tokio::test]
    async fn test_resolve_model_explicit_wins_without_db_lookup() {
        // No query results queued: an explicit, non-empty model must short-circuit
        // before any database access. (A DB hit here would panic the mock.)
        let svc = service_over(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let model = svc.resolve_model(Some(7), Some("gpt-4.1")).await;
        assert_eq!(model, Some("gpt-4.1".to_string()));
    }

    #[tokio::test]
    async fn test_resolve_model_empty_explicit_falls_through_to_default() {
        // Empty explicit string is ignored; no allow-list config exists, so it
        // falls back to the default model for the first active provider key.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // ai_gateway_config: nothing configured
            .append_query_results(vec![Vec::<temps_entities::ai_gateway_config::Model>::new()])
            // ai_provider_keys: one active anthropic key
            .append_query_results(vec![vec![active_key("anthropic")]])
            .into_connection();
        let svc = service_over(db);
        let model = svc.resolve_model(None, Some("")).await;
        assert_eq!(model, Some("claude-3-5-haiku-latest".to_string()));
    }

    #[tokio::test]
    async fn test_resolve_model_allowlist_first_entry_wins() {
        // A project-scoped allow-list names a concrete model; its first entry is
        // used and no provider-key fallback query is performed.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![config_row(
                "project:7",
                Some(serde_json::json!(["gpt-4o", "gpt-4o-mini"])),
            )]])
            .into_connection();
        let svc = service_over(db);
        let model = svc.resolve_model(Some(7), None).await;
        assert_eq!(model, Some("gpt-4o".to_string()));
    }

    #[tokio::test]
    async fn test_resolve_model_default_for_first_active_key() {
        // No allow-list -> default model for the first active provider key.
        for (provider, expected) in [
            ("openai", "gpt-4o-mini"),
            ("anthropic", "claude-3-5-haiku-latest"),
            ("gemini", "gemini-1.5-flash"),
            ("xai", "grok-2-latest"),
        ] {
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<temps_entities::ai_gateway_config::Model>::new()])
                .append_query_results(vec![vec![active_key(provider)]])
                .into_connection();
            let svc = service_over(db);
            let model = svc.resolve_model(None, None).await;
            assert_eq!(model, Some(expected.to_string()), "provider {provider}");
        }
    }

    #[tokio::test]
    async fn test_resolve_model_prefers_key_default_model() {
        // An operator-pinned `default_model` on the active key beats the
        // hardcoded per-provider default.
        let mut key = active_key("openai");
        key.default_model = Some("gpt-qwen3".to_string());
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<temps_entities::ai_gateway_config::Model>::new()])
            .append_query_results(vec![vec![key]])
            .into_connection();
        let svc = service_over(db);
        assert_eq!(
            svc.resolve_model(None, None).await,
            Some("gpt-qwen3".to_string())
        );
    }

    #[tokio::test]
    async fn test_resolve_model_blank_key_default_falls_through() {
        // A whitespace-only pinned model is ignored -> per-provider default.
        let mut key = active_key("openai");
        key.default_model = Some("   ".to_string());
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<temps_entities::ai_gateway_config::Model>::new()])
            .append_query_results(vec![vec![key]])
            .into_connection();
        let svc = service_over(db);
        assert_eq!(
            svc.resolve_model(None, None).await,
            Some("gpt-4o-mini".to_string())
        );
    }

    #[tokio::test]
    async fn test_resolve_model_none_when_no_active_key() {
        // No allow-list and no active provider key -> nothing names a model.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<temps_entities::ai_gateway_config::Model>::new()])
            .append_query_results(vec![Vec::<temps_entities::ai_provider_keys::Model>::new()])
            .into_connection();
        let svc = service_over(db);
        assert_eq!(svc.resolve_model(None, None).await, None);
    }

    #[tokio::test]
    async fn test_resolve_model_none_for_unknown_provider() {
        // Active key exists but its provider has no default mapping -> None.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<temps_entities::ai_gateway_config::Model>::new()])
            .append_query_results(vec![vec![active_key("custom")]])
            .into_connection();
        let svc = service_over(db);
        assert_eq!(svc.resolve_model(None, None).await, None);
    }

    #[tokio::test]
    async fn test_resolve_model_null_allowlist_falls_through_to_default() {
        // A config row with NULL allowed_models ("all allowed") names no specific
        // model, so resolution falls through to the provider-key default.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![config_row("instance", None)]])
            .append_query_results(vec![vec![active_key("openai")]])
            .into_connection();
        let svc = service_over(db);
        let model = svc.resolve_model(None, None).await;
        assert_eq!(model, Some("gpt-4o-mini".to_string()));
    }

    #[test]
    fn test_default_model_for_provider_mapping() {
        assert_eq!(
            default_model_for_provider("openai"),
            Some("gpt-4o-mini".to_string())
        );
        assert_eq!(
            default_model_for_provider("anthropic"),
            Some("claude-3-5-haiku-latest".to_string())
        );
        assert_eq!(
            default_model_for_provider("gemini"),
            Some("gemini-1.5-flash".to_string())
        );
        assert_eq!(
            default_model_for_provider("xai"),
            Some("grok-2-latest".to_string())
        );
        // Unknown / custom providers have no built-in default.
        assert_eq!(default_model_for_provider("custom"), None);
        assert_eq!(default_model_for_provider(""), None);
    }

    #[test]
    fn test_first_text_extraction() {
        // Plain text content -> extracted verbatim.
        let resp = chat_response(Some(MessageContent::Text("hello world".to_string())));
        assert_eq!(first_text(&resp), Some("hello world".to_string()));

        // Multi-part content is not flattened here -> None.
        let parts = chat_response(Some(MessageContent::Parts(vec![
            crate::types::ContentPart {
                r#type: "text".to_string(),
                text: Some("ignored".to_string()),
                image_url: None,
            },
        ])));
        assert_eq!(first_text(&parts), None);

        // Null content -> None.
        let empty = chat_response(None);
        assert_eq!(first_text(&empty), None);

        // No choices -> None, not a panic.
        let mut no_choices = chat_response(Some(MessageContent::Text("x".to_string())));
        no_choices.choices.clear();
        assert_eq!(first_text(&no_choices), None);
    }

    #[test]
    fn test_first_model() {
        assert_eq!(
            first_model(Some(&serde_json::json!(["gpt-4o-mini", "x"]))),
            Some("gpt-4o-mini".to_string())
        );
        assert_eq!(first_model(Some(&serde_json::json!([]))), None);
        assert_eq!(first_model(None), None);
    }

    #[test]
    fn test_response_format_for() {
        let rf = response_format_for(&serde_json::json!({"type": "object"}));
        assert_eq!(rf["type"], "json_schema");
        assert_eq!(rf["json_schema"]["schema"]["type"], "object");
        assert_eq!(rf["json_schema"]["strict"], true);
    }

    #[test]
    fn test_parse_tool_call() {
        let v = serde_json::json!({
            "id": "call_1",
            "type": "function",
            "function": { "name": "read_repo_file", "arguments": "{\"path\":\"tsconfig.json\"}" }
        });
        let tc = parse_tool_call(&v).expect("parses");
        assert_eq!(tc.id, "call_1");
        assert_eq!(tc.name, "read_repo_file");
        assert_eq!(tc.arguments, "{\"path\":\"tsconfig.json\"}");
        // Missing function → None, not a panic.
        assert!(parse_tool_call(&serde_json::json!({"id": "x"})).is_none());
    }

    #[test]
    fn test_message_to_json_tool_shapes() {
        // tool-result message
        let tool_msg = ChatMessage::tool("call_1", "file contents");
        let j = message_to_json(&tool_msg);
        assert_eq!(j["role"], "tool");
        assert_eq!(j["tool_call_id"], "call_1");
        assert_eq!(j["content"], "file contents");

        // assistant message carrying a tool call
        let asst = ChatMessage {
            role: "assistant".into(),
            content: String::new(),
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                name: "read_repo_file".into(),
                arguments: "{}".into(),
            }]),
            tool_call_id: None,
        };
        let j = message_to_json(&asst);
        assert_eq!(j["role"], "assistant");
        assert!(j["content"].is_null());
        assert_eq!(j["tool_calls"][0]["function"]["name"], "read_repo_file");
        assert_eq!(j["tool_calls"][0]["type"], "function");

        // plain message
        let plain = ChatMessage::user("hi");
        let j = message_to_json(&plain);
        assert_eq!(j["role"], "user");
        assert_eq!(j["content"], "hi");
        assert!(j.get("tool_calls").is_none());
    }
}
