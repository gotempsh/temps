use async_trait::async_trait;
use bytes::Bytes;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::pin::Pin;
use std::time::Duration;
use tokio_stream::Stream;

use crate::error::AiGatewayError;
use crate::providers::openai_responses::{self, ReasoningContextCache, ResponsesStreamTranslator};
use crate::providers::{external_http_client, AiProvider, ProviderCapability, ProviderInfo};
use crate::types::{
    ChatCompletionRequest, ChatCompletionResponse, EmbeddingRequest, EmbeddingResponse, ModelInfo,
};

const MAX_RESPONSES_BODY_BYTES: usize = 2 * 1024 * 1024;
const MAX_RESPONSES_SSE_EVENT_BYTES: usize = 1024 * 1024;

/// A provider adapter for any OpenAI API-compatible service.
/// OpenAI, xAI, and any compatible endpoint reuse this
/// implementation — only the `ProviderInfo` and model list differ.
pub struct OpenAiCompatProvider {
    info: ProviderInfo,
    models: Vec<ModelInfo>,
    client: reqwest::Client,
    reasoning_cache: ReasoningContextCache,
}

impl OpenAiCompatProvider {
    pub fn new(info: ProviderInfo, models: Vec<ModelInfo>) -> Self {
        let client = external_http_client(Duration::from_secs(300));

        Self {
            info,
            models,
            client,
            reasoning_cache: openai_responses::new_reasoning_context_cache(),
        }
    }

    fn resolve_base_url(&self, base_url: Option<&str>) -> String {
        base_url
            .unwrap_or(self.info.default_base_url)
            .trim_end_matches('/')
            .to_string()
    }

    fn uses_responses_api(&self, model: &str) -> bool {
        crate::services::provider_capabilities::uses_openai_responses_api(self.info.id, model)
    }

    fn reasoning_cache_namespace(&self, api_key: &str, base_url: Option<&str>) -> String {
        let mut digest = Sha256::new();
        digest.update(self.info.id.as_bytes());
        digest.update([0]);
        digest.update(self.resolve_base_url(base_url).as_bytes());
        digest.update([0]);
        digest.update(api_key.as_bytes());
        hex::encode(digest.finalize())
    }

    async fn responses_completion(
        &self,
        api_key: &str,
        base_url: Option<&str>,
        request: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, AiGatewayError> {
        let url = format!("{}/responses", self.resolve_base_url(base_url));
        let mut req = request.clone();
        req.stream = false;
        sanitize_openai_request(&mut req);
        let cache_namespace = self.reasoning_cache_namespace(api_key, base_url);
        let body =
            openai_responses::build_request(&req, &self.reasoning_cache, &cache_namespace, false)
                .await?;
        let response = self
            .client
            .post(&url)
            .bearer_auth(api_key)
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = read_limited_responses_body(response, &request.model)
                .await
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_else(|_| "Failed to read bounded response body".to_string());
            return Err(AiGatewayError::UpstreamError {
                model: request.model.clone(),
                status: status.as_u16(),
                message: body,
            });
        }
        let response_body = read_limited_responses_body(response, &request.model).await?;
        let payload =
            serde_json::from_slice::<serde_json::Value>(&response_body).map_err(|error| {
                AiGatewayError::TranslationError {
                    provider: self.info.id.to_string(),
                    reason: format!("Failed to parse Responses result: {error}"),
                }
            })?;
        openai_responses::translate_response(payload, &self.reasoning_cache, &cache_namespace).await
    }

    async fn responses_completion_stream(
        &self,
        api_key: &str,
        base_url: Option<&str>,
        request: &ChatCompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<Bytes, AiGatewayError>> + Send>>, AiGatewayError>
    {
        let url = format!("{}/responses", self.resolve_base_url(base_url));
        let mut req = request.clone();
        req.stream = true;
        sanitize_openai_request(&mut req);
        let cache_namespace = self.reasoning_cache_namespace(api_key, base_url);
        let body =
            openai_responses::build_request(&req, &self.reasoning_cache, &cache_namespace, true)
                .await?;
        let response = self
            .client
            .post(&url)
            .bearer_auth(api_key)
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = read_limited_responses_body(response, &request.model)
                .await
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_else(|_| "Failed to read bounded response body".to_string());
            return Err(AiGatewayError::UpstreamError {
                model: request.model.clone(),
                status: status.as_u16(),
                message: body,
            });
        }

        let mut upstream = response.bytes_stream();
        let cache = self.reasoning_cache.clone();
        let cache_namespace_for_stream = cache_namespace;
        let model = request.model.clone();
        let translated = async_stream::try_stream! {
            let mut buffer = Vec::new();
            let mut translator = ResponsesStreamTranslator::default();
            let mut terminal = false;
            while let Some(chunk) = upstream.next().await {
                let chunk = chunk.map_err(|error| AiGatewayError::StreamError {
                    model: model.clone(),
                    reason: error.to_string(),
                })?;
                buffer.extend_from_slice(&chunk);
                if buffer.len() > MAX_RESPONSES_SSE_EVENT_BYTES {
                    Err(AiGatewayError::StreamError {
                        model: model.clone(),
                        reason: "OpenAI Responses SSE event exceeds byte limit".to_string(),
                    })?;
                }
                while let Some(raw_event) = take_sse_event(&mut buffer) {
                    let Some(event) = parse_responses_sse_event(&raw_event)? else {
                        continue;
                    };
                    let translated_event = translator.translate(&event)?;
                    if let Some((call_mappings, reasoning)) = translated_event.reasoning_context {
                        for (gateway_call_id, upstream_call_id) in call_mappings {
                            cache
                                .insert(
                                    openai_responses::reasoning_cache_key(
                                        &cache_namespace_for_stream,
                                        &gateway_call_id,
                                    ),
                                    std::sync::Arc::new(openai_responses::ReasoningContext {
                                        upstream_call_id,
                                        items: reasoning.clone(),
                                    }),
                                )
                                .await;
                        }
                    }
                    terminal |= translated_event.terminal;
                    for frame in translated_event.frames {
                        yield frame;
                    }
                }
            }
            if !buffer.is_empty() {
                if let Some(event) = parse_responses_sse_event(&buffer)? {
                    let translated_event = translator.translate(&event)?;
                    if let Some((call_mappings, reasoning)) = translated_event.reasoning_context {
                        for (gateway_call_id, upstream_call_id) in call_mappings {
                            cache
                                .insert(
                                    openai_responses::reasoning_cache_key(
                                        &cache_namespace_for_stream,
                                        &gateway_call_id,
                                    ),
                                    std::sync::Arc::new(openai_responses::ReasoningContext {
                                        upstream_call_id,
                                        items: reasoning.clone(),
                                    }),
                                )
                                .await;
                        }
                    }
                    terminal |= translated_event.terminal;
                    for frame in translated_event.frames {
                        yield frame;
                    }
                }
            }
            if !terminal {
                Err(AiGatewayError::StreamError {
                    model,
                    reason: "OpenAI Responses stream ended before a terminal event".to_string(),
                })?;
            }
        };
        Ok(Box::pin(translated))
    }

    pub fn openai() -> Self {
        Self::new(
            ProviderInfo {
                id: "openai",
                display_name: "OpenAI",
                default_base_url: "https://api.openai.com/v1",
                capabilities: &[
                    ProviderCapability::ChatCompletion,
                    ProviderCapability::ChatCompletionStreaming,
                    ProviderCapability::Embeddings,
                    ProviderCapability::ToolUse,
                    ProviderCapability::Vision,
                    ProviderCapability::JsonMode,
                ],
            },
            vec![
                // Frontier models
                model("gpt-5.6-sol", "openai"),
                model("gpt-5.6-terra", "openai"),
                model("gpt-5.6-luna", "openai"),
                model("gpt-5.6", "openai"),
                model("gpt-5.4", "openai"),
                model("gpt-5.4-pro", "openai"),
                model("gpt-5-mini", "openai"),
                model("gpt-5-nano", "openai"),
                model("gpt-5", "openai"),
                model("gpt-4.1", "openai"),
                model("gpt-4.1-mini", "openai"),
                model("gpt-4.1-nano", "openai"),
                // Reasoning models
                model("o3", "openai"),
                model("o3-pro", "openai"),
                model("o4-mini", "openai"),
                model("o3-mini", "openai"),
                // Previous generation
                model("gpt-4o", "openai"),
                model("gpt-4o-mini", "openai"),
                // Embeddings
                model("text-embedding-3-small", "openai"),
                model("text-embedding-3-large", "openai"),
            ],
        )
    }

    pub fn xai() -> Self {
        Self::new(
            ProviderInfo {
                id: "xai",
                display_name: "xAI",
                default_base_url: "https://api.x.ai/v1",
                capabilities: &[
                    ProviderCapability::ChatCompletion,
                    ProviderCapability::ChatCompletionStreaming,
                    ProviderCapability::ToolUse,
                ],
            },
            vec![
                model("grok-4.5", "xai"),
                model("grok-4.20", "xai"),
                model("grok-4.20-0309-reasoning", "xai"),
            ],
        )
    }
}

fn model(id: &str, owned_by: &str) -> ModelInfo {
    ModelInfo {
        id: id.to_string(),
        object: "model".to_string(),
        owned_by: owned_by.to_string(),
    }
}

#[async_trait]
impl AiProvider for OpenAiCompatProvider {
    fn info(&self) -> &ProviderInfo {
        &self.info
    }

    fn supports_model(&self, model: &str) -> bool {
        let model_lower = model.to_lowercase();
        self.models
            .iter()
            .any(|m| model_lower.starts_with(m.id.to_lowercase().split('-').next().unwrap_or("")))
            || crate::providers::route_model_to_provider(model)
                .map(|p| p == self.info.id)
                .unwrap_or(false)
    }

    fn available_models(&self) -> Vec<ModelInfo> {
        self.models.clone()
    }

    async fn chat_completion(
        &self,
        api_key: &str,
        base_url: Option<&str>,
        request: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, AiGatewayError> {
        if self.uses_responses_api(&request.model) {
            return self.responses_completion(api_key, base_url, request).await;
        }
        let url = format!("{}/chat/completions", self.resolve_base_url(base_url));

        // For OpenAI-compatible providers, pass the request as-is (minus stream flag)
        let mut req = request.clone();
        req.stream = false;

        // Sanitize request for OpenAI model compatibility
        if self.info.id == "openai" {
            sanitize_openai_request(&mut req);
        }

        let response = self
            .client
            .post(&url)
            .bearer_auth(api_key)
            .json(&req)
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

        let completion: ChatCompletionResponse =
            response
                .json()
                .await
                .map_err(|e| AiGatewayError::TranslationError {
                    provider: self.info.id.to_string(),
                    reason: format!("Failed to parse response: {}", e),
                })?;

        Ok(completion)
    }

    async fn chat_completion_stream(
        &self,
        api_key: &str,
        base_url: Option<&str>,
        request: &ChatCompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<Bytes, AiGatewayError>> + Send>>, AiGatewayError>
    {
        if self.uses_responses_api(&request.model) {
            return self
                .responses_completion_stream(api_key, base_url, request)
                .await;
        }
        let url = format!("{}/chat/completions", self.resolve_base_url(base_url));

        let mut req = request.clone();
        req.stream = true;

        if self.info.id == "openai" {
            sanitize_openai_request(&mut req);
        }

        // Inject stream_options.include_usage so the final chunk includes token counts
        let extra = req.extra.get_or_insert_with(Default::default);
        extra
            .entry("stream_options")
            .or_insert_with(|| serde_json::json!({"include_usage": true}));

        let response = self
            .client
            .post(&url)
            .bearer_auth(api_key)
            .json(&req)
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

        // Stream bytes directly from upstream — already in OpenAI SSE format
        let stream = response.bytes_stream();
        let model = request.model.clone();

        let mapped = tokio_stream::StreamExt::map(stream, move |result| {
            result.map_err(|e| AiGatewayError::StreamError {
                model: model.clone(),
                reason: e.to_string(),
            })
        });

        Ok(Box::pin(mapped))
    }

    async fn embeddings(
        &self,
        api_key: &str,
        base_url: Option<&str>,
        request: &EmbeddingRequest,
    ) -> Result<EmbeddingResponse, AiGatewayError> {
        if !self
            .info
            .capabilities
            .contains(&ProviderCapability::Embeddings)
        {
            return Err(AiGatewayError::ModelNotFound {
                model: request.model.clone(),
            });
        }

        let url = format!("{}/embeddings", self.resolve_base_url(base_url));

        let response = self
            .client
            .post(&url)
            .bearer_auth(api_key)
            .json(request)
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

        let embedding: EmbeddingResponse =
            response
                .json()
                .await
                .map_err(|e| AiGatewayError::TranslationError {
                    provider: self.info.id.to_string(),
                    reason: format!("Failed to parse embedding response: {}", e),
                })?;

        Ok(embedding)
    }
}

/// Returns true if the model only accepts OpenAI's default sampling settings.
///
/// GPT-5 family models use the same reasoning controls as the o-series and
/// reject custom `temperature` values (including the low temperature used by
/// structured-output requests).
fn uses_default_sampling(model: &str) -> bool {
    let m = model.to_lowercase();
    m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") || m.starts_with("gpt-5")
}

/// Sanitize a chat completion request for OpenAI API compatibility.
///
/// - All OpenAI models: rewrite `max_tokens` → `max_completion_tokens`
/// - Reasoning models (GPT-5, o1, o3, o4-mini, etc.): strip unsupported
///   parameters (`temperature`, `top_p`, `frequency_penalty`, `presence_penalty`)
fn sanitize_openai_request(req: &mut ChatCompletionRequest) {
    // Rewrite max_tokens → max_completion_tokens for all OpenAI models
    if let Some(value) = req.max_tokens.take() {
        let extra = req.extra.get_or_insert_with(Default::default);
        extra
            .entry("max_completion_tokens")
            .or_insert_with(|| serde_json::json!(value));
    }

    // O-series models reject sampling parameters
    if uses_default_sampling(&req.model) {
        req.temperature = None;
        req.top_p = None;
        req.frequency_penalty = None;
        req.presence_penalty = None;
    }
}

async fn read_limited_responses_body(
    response: reqwest::Response,
    model: &str,
) -> Result<Vec<u8>, AiGatewayError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSES_BODY_BYTES as u64)
    {
        return Err(AiGatewayError::StreamError {
            model: model.to_string(),
            reason: "OpenAI Responses body exceeds byte limit".to_string(),
        });
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| AiGatewayError::StreamError {
            model: model.to_string(),
            reason: error.to_string(),
        })?;
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSES_BODY_BYTES {
            return Err(AiGatewayError::StreamError {
                model: model.to_string(),
                reason: "OpenAI Responses body exceeds byte limit".to_string(),
            });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn take_sse_event(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let crlf_boundary = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| (index, 4));
    let lf_boundary = buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|index| (index, 2));
    let boundary = match (crlf_boundary, lf_boundary) {
        (Some(crlf), Some(lf)) => Some(if crlf.0 <= lf.0 { crlf } else { lf }),
        (Some(boundary), None) | (None, Some(boundary)) => Some(boundary),
        (None, None) => None,
    }?;
    let event = buffer[..boundary.0].to_vec();
    buffer.drain(..boundary.0 + boundary.1);
    Some(event)
}

fn parse_responses_sse_event(raw: &[u8]) -> Result<Option<serde_json::Value>, AiGatewayError> {
    if raw.len() > MAX_RESPONSES_SSE_EVENT_BYTES {
        return Err(AiGatewayError::TranslationError {
            provider: "openai".to_string(),
            reason: "Responses stream event exceeds byte limit".to_string(),
        });
    }
    let text = std::str::from_utf8(raw).map_err(|error| AiGatewayError::TranslationError {
        provider: "openai".to_string(),
        reason: format!("Responses stream contained invalid UTF-8: {error}"),
    })?;
    let data = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim_start)
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() || data == "[DONE]" {
        return Ok(None);
    }
    serde_json::from_str(&data)
        .map(Some)
        .map_err(|error| AiGatewayError::TranslationError {
            provider: "openai".to_string(),
            reason: format!("Failed to parse Responses stream event: {error}"),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_provider_info() {
        let provider = OpenAiCompatProvider::openai();
        assert_eq!(provider.info().id, "openai");
        assert_eq!(
            provider.info().default_base_url,
            "https://api.openai.com/v1"
        );
        assert!(provider
            .info()
            .capabilities
            .contains(&ProviderCapability::ChatCompletion));
        assert!(provider
            .info()
            .capabilities
            .contains(&ProviderCapability::Embeddings));
    }

    #[test]
    fn test_xai_provider_info() {
        let provider = OpenAiCompatProvider::xai();
        assert_eq!(provider.info().id, "xai");
        assert!(!provider
            .info()
            .capabilities
            .contains(&ProviderCapability::Embeddings));
    }

    #[test]
    fn test_openai_supports_model() {
        let provider = OpenAiCompatProvider::openai();
        assert!(provider.supports_model("gpt-4o"));
        assert!(provider.supports_model("gpt-4o-mini"));
        assert!(!provider.supports_model("claude-sonnet-4-6"));
    }

    #[test]
    fn test_xai_supports_model() {
        let provider = OpenAiCompatProvider::xai();
        assert!(provider.supports_model("grok-3"));
        assert!(!provider.supports_model("gpt-4o"));
    }

    #[test]
    fn test_resolve_base_url() {
        let provider = OpenAiCompatProvider::openai();
        assert_eq!(provider.resolve_base_url(None), "https://api.openai.com/v1");
        assert_eq!(
            provider.resolve_base_url(Some("https://custom.endpoint.com/v1/")),
            "https://custom.endpoint.com/v1"
        );
    }

    #[test]
    fn test_available_models() {
        let provider = OpenAiCompatProvider::openai();
        let models = provider.available_models();
        assert!(!models.is_empty());
        assert!(models.iter().any(|m| m.id == "gpt-4o"));
        assert!(models.iter().all(|m| m.owned_by == "openai"));
    }

    fn test_request(model: &str) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: model.to_string(),
            messages: vec![],
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
    fn test_sanitize_rewrites_max_tokens() {
        let mut req = test_request("gpt-5-nano");
        req.max_tokens = Some(500);

        sanitize_openai_request(&mut req);

        assert!(req.max_tokens.is_none());
        let extra = req.extra.as_ref().unwrap();
        assert_eq!(
            extra.get("max_completion_tokens").unwrap(),
            &serde_json::json!(500)
        );

        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("max_completion_tokens"));
        assert!(!json.contains("\"max_tokens\""));
    }

    #[test]
    fn test_sanitize_noop_when_no_max_tokens() {
        let mut req = test_request("gpt-5-nano");

        sanitize_openai_request(&mut req);

        assert!(req.max_tokens.is_none());
        assert!(req.extra.is_none());
    }

    #[test]
    fn test_sanitize_preserves_existing_max_completion_tokens() {
        let mut extra = serde_json::Map::new();
        extra.insert("max_completion_tokens".to_string(), serde_json::json!(1000));

        let mut req = test_request("gpt-5-nano");
        req.max_tokens = Some(500);
        req.extra = Some(extra);

        sanitize_openai_request(&mut req);

        assert!(req.max_tokens.is_none());
        assert_eq!(
            req.extra
                .as_ref()
                .unwrap()
                .get("max_completion_tokens")
                .unwrap(),
            &serde_json::json!(1000)
        );
    }

    #[test]
    fn test_sanitize_strips_sampling_params_for_o_series() {
        let mut req = test_request("o3");
        req.temperature = Some(0.7);
        req.top_p = Some(0.9);
        req.frequency_penalty = Some(0.5);
        req.presence_penalty = Some(0.3);
        req.max_tokens = Some(500);

        sanitize_openai_request(&mut req);

        assert!(req.temperature.is_none());
        assert!(req.top_p.is_none());
        assert!(req.frequency_penalty.is_none());
        assert!(req.presence_penalty.is_none());
        assert!(req.max_tokens.is_none());
        assert_eq!(
            req.extra
                .as_ref()
                .unwrap()
                .get("max_completion_tokens")
                .unwrap(),
            &serde_json::json!(500)
        );
    }

    #[test]
    fn test_sanitize_preserves_luna_reasoning_for_responses_api() {
        let mut req = test_request("gpt-5.6-luna");
        req.tools = Some(vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "lookup",
                "parameters": {"type": "object"}
            }
        })]);
        req.extra = Some(serde_json::Map::from_iter([(
            "reasoning_effort".to_string(),
            serde_json::json!("medium"),
        )]));

        sanitize_openai_request(&mut req);

        assert_eq!(
            req.extra
                .as_ref()
                .and_then(|extra| extra.get("reasoning_effort")),
            Some(&serde_json::json!("medium"))
        );
    }

    #[test]
    fn responses_sse_parser_handles_fragmented_crlf_frames() {
        let mut buffer = b"event: response.output_text.delta\r\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Hi\"}\r\n".to_vec();
        assert!(take_sse_event(&mut buffer).is_none());
        buffer.extend_from_slice(b"\r\n");
        let raw = take_sse_event(&mut buffer).expect("complete SSE event");
        let event = parse_responses_sse_event(&raw)
            .expect("valid event")
            .expect("JSON data event");
        assert_eq!(event["delta"], "Hi");
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_sanitize_strips_sampling_params_for_o4_mini() {
        let mut req = test_request("o4-mini");
        req.temperature = Some(0.5);
        req.top_p = Some(0.8);

        sanitize_openai_request(&mut req);

        assert!(req.temperature.is_none());
        assert!(req.top_p.is_none());
    }

    #[test]
    fn test_sanitize_strips_sampling_params_for_gpt5() {
        let mut req = test_request("gpt-5-nano");
        req.temperature = Some(0.7);
        req.top_p = Some(0.9);
        req.frequency_penalty = Some(0.5);
        req.presence_penalty = Some(0.3);

        sanitize_openai_request(&mut req);

        assert!(req.temperature.is_none());
        assert!(req.top_p.is_none());
        assert!(req.frequency_penalty.is_none());
        assert!(req.presence_penalty.is_none());
    }

    #[test]
    fn test_uses_default_sampling() {
        assert!(uses_default_sampling("o3"));
        assert!(uses_default_sampling("o3-pro"));
        assert!(uses_default_sampling("o3-mini"));
        assert!(uses_default_sampling("o4-mini"));
        assert!(uses_default_sampling("o1-preview"));
        assert!(uses_default_sampling("gpt-5-nano"));
        assert!(uses_default_sampling("gpt-5.6"));
        assert!(!uses_default_sampling("gpt-4o"));
        assert!(!uses_default_sampling("grok-3"));
    }
}
