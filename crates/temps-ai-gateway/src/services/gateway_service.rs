// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use bytes::Bytes;
use futures_util::StreamExt as FuturesStreamExt;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use tokio_stream::Stream;
use tracing::{debug, warn};

use crate::error::AiGatewayError;
use crate::providers::anthropic::AnthropicProvider;
use crate::providers::gemini::GeminiProvider;
use crate::providers::openai_compat::OpenAiCompatProvider;
use crate::providers::{external_http_client, route_model_to_provider, AiProvider};
use crate::services::provider_key_service::ProviderKeyService;
use crate::types::*;

/// Indicates how the provider API key was resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialType {
    /// Key was loaded from the Temps database (admin-configured).
    System,
    /// Key was provided by the caller via `X-Provider-Api-Key` header.
    Byok,
}

/// Optional overrides the caller can supply to bring their own key.
#[derive(Debug, Clone, Default)]
pub struct ByokOverride {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    /// Exact administrator-configured key selected by a pinned conversation.
    /// Missing or inactive IDs fail closed; they never fall back to another key.
    pub system_key_id: Option<i32>,
}

/// The core gateway service that routes requests to the appropriate provider,
/// handles key decryption, and coordinates the entire request lifecycle.
pub struct GatewayService {
    provider_key_service: Arc<ProviderKeyService>,
    providers: HashMap<&'static str, Box<dyn AiProvider>>,
    cloud_link: Option<Arc<temps_cloud_client::CloudLink>>,
}

impl GatewayService {
    pub fn new(provider_key_service: Arc<ProviderKeyService>) -> Self {
        let mut providers: HashMap<&'static str, Box<dyn AiProvider>> = HashMap::new();

        providers.insert("openai", Box::new(OpenAiCompatProvider::openai()));
        providers.insert("anthropic", Box::new(AnthropicProvider::new()));
        providers.insert("xai", Box::new(OpenAiCompatProvider::xai()));
        providers.insert("gemini", Box::new(GeminiProvider::new()));
        Self {
            provider_key_service,
            providers,
            cloud_link: None,
        }
    }

    pub fn with_cloud_link(
        mut self,
        cloud_link: Option<Arc<temps_cloud_client::CloudLink>>,
    ) -> Self {
        self.cloud_link = cloud_link;
        self
    }

    pub async fn managed_model(&self) -> Option<String> {
        let link = self.cloud_link.as_ref()?;
        let capability = link.managed_ai_capability().await.ok()?;
        capability
            .configured
            .then_some(capability.managed_model?)
            .filter(|model| !model.is_empty())
    }

    /// Models advertised by the adapter for one configured provider key.
    pub fn available_models_for_provider(&self, provider_id: &str) -> Vec<ModelInfo> {
        self.providers
            .get(provider_id)
            .map(|provider| provider.available_models())
            .unwrap_or_default()
    }

    /// Discover the models actually visible to one provider key. Static
    /// adapter models are only a bootstrap fallback; this is the source used
    /// by explicit catalog refreshes.
    pub async fn discover_models_for_key(
        &self,
        key_id: i32,
    ) -> Result<Vec<ModelInfo>, AiGatewayError> {
        let key = self.provider_key_service.get_by_id(key_id).await?;
        let provider = self.providers.get(key.provider.as_str()).ok_or_else(|| {
            AiGatewayError::ProviderNotConfigured {
                provider: key.provider.clone(),
            }
        })?;
        if let Some(base_url) = key.base_url.as_deref() {
            temps_core::url_validation::validate_external_url(base_url).map_err(|error| {
                AiGatewayError::InvalidProviderUrl {
                    reason: error.to_string(),
                }
            })?;
        }
        let api_key = self
            .provider_key_service
            .decrypt_api_key(&key.api_key_encrypted)?;
        let base = key
            .base_url
            .as_deref()
            .unwrap_or(provider.info().default_base_url)
            .trim_end_matches('/');
        let client = external_http_client(std::time::Duration::from_secs(30));

        let response = match key.provider.as_str() {
            "openai" | "xai" => {
                client
                    .get(format!("{base}/models"))
                    .bearer_auth(&api_key)
                    .send()
                    .await?
            }
            "anthropic" => {
                client
                    .get(format!("{base}/v1/models?limit=1000"))
                    .header("x-api-key", &api_key)
                    .header("anthropic-version", "2023-06-01")
                    .send()
                    .await?
            }
            "gemini" => {
                client
                    .get(format!("{base}/v1beta/models"))
                    .query(&[("pageSize", "1000"), ("key", api_key.as_str())])
                    .send()
                    .await?
            }
            _ => {
                return Err(AiGatewayError::ProviderNotConfigured {
                    provider: key.provider,
                })
            }
        };
        let status = response.status();
        if !status.is_success() {
            return Err(AiGatewayError::UpstreamError {
                model: "model-catalog".to_string(),
                status: status.as_u16(),
                message: "Provider rejected model discovery".to_string(),
            });
        }
        const MAX_MODEL_CATALOG_BYTES: usize = 1024 * 1024;
        if response
            .content_length()
            .is_some_and(|size| size > MAX_MODEL_CATALOG_BYTES as u64)
        {
            return Err(AiGatewayError::TranslationError {
                provider: key.provider.clone(),
                reason: "Provider model catalog exceeded the 1 MiB limit".to_string(),
            });
        }
        let mut response_stream = response.bytes_stream();
        let mut response_bytes = Vec::new();
        while let Some(chunk) = response_stream.next().await {
            let chunk = chunk?;
            if response_bytes.len().saturating_add(chunk.len()) > MAX_MODEL_CATALOG_BYTES {
                return Err(AiGatewayError::TranslationError {
                    provider: key.provider.clone(),
                    reason: "Provider model catalog exceeded the 1 MiB limit".to_string(),
                });
            }
            response_bytes.extend_from_slice(&chunk);
        }
        let body: serde_json::Value = serde_json::from_slice(&response_bytes).map_err(|error| {
            AiGatewayError::TranslationError {
                provider: key.provider.clone(),
                reason: format!("Failed to parse provider model catalog: {error}"),
            }
        })?;
        let entries = if key.provider == "gemini" {
            body.get("models")
        } else {
            body.get("data")
        }
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| AiGatewayError::TranslationError {
            provider: key.provider.clone(),
            reason: "Provider model catalog did not contain a model array".to_string(),
        })?;

        let mut models: Vec<ModelInfo> = entries
            .iter()
            .filter(|entry| {
                key.provider != "gemini"
                    || entry
                        .get("supportedGenerationMethods")
                        .and_then(serde_json::Value::as_array)
                        .is_none_or(|methods| {
                            methods.iter().any(|method| method == "generateContent")
                        })
            })
            .filter_map(|entry| {
                let raw_id = entry.get("id").or_else(|| entry.get("name"))?.as_str()?;
                let id = raw_id.strip_prefix("models/").unwrap_or(raw_id).to_string();
                if !provider.supports_model(&id) {
                    return None;
                }
                let owned_by = entry
                    .get("owned_by")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(key.provider.as_str())
                    .to_string();
                Some(ModelInfo {
                    id,
                    object: "model".to_string(),
                    owned_by,
                })
            })
            .take(5_000)
            .collect();
        models.sort_by(|left, right| left.id.cmp(&right.id));
        models.dedup_by(|left, right| left.id == right.id);
        Ok(models)
    }

    /// Route a model name to the provider and resolve the API key.
    ///
    /// When `byok.api_key` is set, the caller-supplied key is used directly
    /// and no database lookup is performed. Otherwise the admin-configured
    /// system key is decrypted from the database.
    async fn resolve_provider_and_key(
        &self,
        model: &str,
        byok: &ByokOverride,
    ) -> Result<(&dyn AiProvider, String, Option<String>, CredentialType), AiGatewayError> {
        let provider_id =
            route_model_to_provider(model).ok_or_else(|| AiGatewayError::ModelNotFound {
                model: model.to_string(),
            })?;

        let provider = self.providers.get(provider_id).ok_or_else(|| {
            AiGatewayError::ProviderNotConfigured {
                provider: provider_id.to_string(),
            }
        })?;

        // BYOK: caller supplied their own key — skip DB lookup entirely
        if let Some(ref user_key) = byok.api_key {
            // Validate the caller-supplied base URL to prevent SSRF.
            // An authenticated user must not be able to point the gateway at
            // internal services (cloud metadata, Docker daemon, private subnets, etc.).
            if let Some(ref base_url) = byok.base_url {
                temps_core::url_validation::validate_external_url(base_url).map_err(|e| {
                    AiGatewayError::InvalidProviderUrl {
                        reason: e.to_string(),
                    }
                })?;
            }

            debug!(
                provider = provider_id,
                model = model,
                credential_type = "byok",
                "Routing request to provider (BYOK)"
            );
            return Ok((
                provider.as_ref(),
                user_key.clone(),
                byok.base_url.clone(),
                CredentialType::Byok,
            ));
        }

        // System key: look up from database
        let key_record = if let Some(key_id) = byok.system_key_id {
            let key = self.provider_key_service.get_by_id(key_id).await?;
            if !key.is_active {
                return Err(AiGatewayError::Validation {
                    message: format!("AI provider key {key_id} is inactive"),
                });
            }
            if key.provider != provider_id {
                return Err(AiGatewayError::Validation {
                    message: format!(
                        "AI provider key {key_id} is configured for '{}' and cannot serve model '{model}'",
                        key.provider
                    ),
                });
            }
            key
        } else {
            self.provider_key_service
                .get_active_by_provider(provider_id)
                .await?
                .ok_or_else(|| AiGatewayError::ProviderNotConfigured {
                    provider: provider_id.to_string(),
                })?
        };

        self.provider_key_service
            .ensure_model_selectable(key_record.id, model)
            .await?;

        let decrypted_key = self
            .provider_key_service
            .decrypt_api_key(&key_record.api_key_encrypted)?;

        if let Some(base_url) = key_record.base_url.as_deref() {
            temps_core::url_validation::validate_external_url(base_url).map_err(|error| {
                AiGatewayError::InvalidProviderUrl {
                    reason: error.to_string(),
                }
            })?;
        }

        debug!(
            provider = provider_id,
            model = model,
            credential_type = "system",
            "Routing request to provider"
        );

        Ok((
            provider.as_ref(),
            decrypted_key,
            key_record.base_url,
            CredentialType::System,
        ))
    }

    /// Execute a chat completion request (non-streaming)
    pub async fn chat_completion(
        &self,
        request: &ChatCompletionRequest,
        byok: &ByokOverride,
    ) -> Result<(ChatCompletionResponse, CredentialType), AiGatewayError> {
        if request.model == "temps-cloud" {
            return self.cloud_chat_completion(request).await;
        }
        let (provider, api_key, base_url, cred_type) =
            self.resolve_provider_and_key(&request.model, byok).await?;

        let response = provider
            .chat_completion(&api_key, base_url.as_deref(), request)
            .await?;

        Ok((response, cred_type))
    }

    /// Execute a streaming chat completion request
    pub async fn chat_completion_stream(
        &self,
        request: &ChatCompletionRequest,
        byok: &ByokOverride,
    ) -> Result<
        (
            Pin<Box<dyn Stream<Item = Result<Bytes, AiGatewayError>> + Send>>,
            CredentialType,
        ),
        AiGatewayError,
    > {
        if request.model == "temps-cloud" {
            let (response, credential) = self.cloud_chat_completion(request).await?;
            let choice = response.choices.first().cloned();
            let first = serde_json::json!({
                "id": response.id,
                "object": "chat.completion.chunk",
                "created": response.created,
                "model": response.model,
                "choices": choice.into_iter().map(|choice| serde_json::json!({
                    "index": choice.index,
                    "delta": {
                        "role": choice.message.role,
                        "content": choice.message.content,
                        "tool_calls": choice.message.tool_calls,
                    },
                    "finish_reason": choice.finish_reason,
                })).collect::<Vec<_>>(),
                "usage": response.usage,
            });
            let payload = format!(
                "data: {}\n\ndata: [DONE]\n\n",
                serde_json::to_string(&first).map_err(|error| {
                    AiGatewayError::TranslationError {
                        provider: "temps-cloud".into(),
                        reason: error.to_string(),
                    }
                })?
            );
            let stream = tokio_stream::iter(vec![Ok(Bytes::from(payload))]);
            return Ok((Box::pin(stream), credential));
        }
        let (provider, api_key, base_url, cred_type) =
            self.resolve_provider_and_key(&request.model, byok).await?;

        let stream = provider
            .chat_completion_stream(&api_key, base_url.as_deref(), request)
            .await?;

        Ok((stream, cred_type))
    }

    async fn cloud_chat_completion(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<(ChatCompletionResponse, CredentialType), AiGatewayError> {
        let link =
            self.cloud_link
                .as_ref()
                .ok_or_else(|| AiGatewayError::ProviderNotConfigured {
                    provider: "temps-cloud".into(),
                })?;
        let mut body =
            serde_json::to_value(request).map_err(|error| AiGatewayError::TranslationError {
                provider: "temps-cloud".into(),
                reason: format!("could not encode managed request: {error}"),
            })?;
        if let Some(object) = body.as_object_mut() {
            object.insert("stream".into(), false.into());
        }
        let response = link
            .managed_ai_chat(&temps_cloud_protocol::ManagedAiChatRequest {
                request_id: uuid::Uuid::new_v4(),
                requested_at: chrono::Utc::now(),
                body,
            })
            .await
            .map_err(|error| AiGatewayError::UpstreamError {
                model: "temps-cloud".into(),
                status: 502,
                message: error.to_string(),
            })?;
        let completion = serde_json::from_value(response.body).map_err(|error| {
            AiGatewayError::TranslationError {
                provider: "temps-cloud".into(),
                reason: format!("Cloud returned an invalid OpenAI completion: {error}"),
            }
        })?;
        Ok((completion, CredentialType::System))
    }

    /// Execute an embeddings request
    pub async fn embeddings(
        &self,
        request: &EmbeddingRequest,
        byok: &ByokOverride,
    ) -> Result<(EmbeddingResponse, CredentialType), AiGatewayError> {
        let (provider, api_key, base_url, cred_type) =
            self.resolve_provider_and_key(&request.model, byok).await?;

        let response = provider
            .embeddings(&api_key, base_url.as_deref(), request)
            .await?;

        Ok((response, cred_type))
    }

    /// Send a minimal chat completion to verify a provider API key works.
    /// Uses the cheapest model for the provider and a small `max_tokens`.
    pub async fn test_provider(
        &self,
        provider_id: &str,
        api_key: &str,
        base_url: Option<&str>,
    ) -> Result<(), AiGatewayError> {
        if let Some(base_url) = base_url {
            temps_core::url_validation::validate_external_url(base_url).map_err(|error| {
                AiGatewayError::InvalidProviderUrl {
                    reason: error.to_string(),
                }
            })?;
        }
        let provider = self.providers.get(provider_id).ok_or_else(|| {
            AiGatewayError::ProviderNotConfigured {
                provider: provider_id.to_string(),
            }
        })?;

        // Pick the cheapest/smallest model for the test
        let test_model = match provider_id {
            "openai" => "gpt-5-nano",
            "anthropic" => "claude-haiku-4-5",
            "xai" => "grok-4.5",
            "gemini" => "gemini-3.5-flash-lite",
            _ => {
                return Err(AiGatewayError::ProviderNotConfigured {
                    provider: provider_id.to_string(),
                })
            }
        };

        let request = ChatCompletionRequest {
            model: test_model.to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("Say ok".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            }],
            stream: false,
            temperature: None,
            max_tokens: Some(20),
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

        provider
            .chat_completion(api_key, base_url, &request)
            .await?;

        Ok(())
    }

    /// List all available models from all configured (active) providers
    pub async fn list_models(&self) -> Result<ModelListResponse, AiGatewayError> {
        let active_keys = self.provider_key_service.list_active().await?;
        let mut models = Vec::new();

        for key in &active_keys {
            if let Some(provider) = self.providers.get(key.provider.as_str()) {
                models.extend(provider.available_models());
            } else {
                warn!(
                    provider = key.provider,
                    "Active key for unknown provider, skipping"
                );
            }
        }

        Ok(ModelListResponse {
            object: "list".to_string(),
            data: models,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{http::StatusCode, routing::post, Json, Router};
    use futures_util::StreamExt;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_cloud_client::{BackendUrl, CloudLink};
    use temps_cloud_protocol::{EnrollResponse, ManagedAiChatRequest};
    use temps_core::url_validation::validate_external_url;
    use temps_core::EncryptionService;

    async fn enroll_stub() -> Json<EnrollResponse> {
        Json(EnrollResponse {
            tenant_id: uuid::Uuid::new_v4(),
            account_email: Some("operator@example.com".into()),
            instance_token: "instance-token".into(),
            capabilities: Vec::new(),
        })
    }

    async fn cloud_chat_stub(Json(request): Json<ManagedAiChatRequest>) -> Json<serde_json::Value> {
        Json(serde_json::json!({
            "request_id": request.request_id,
            "settled_credits": 1,
            "provider": "test-provider",
            "model": "test-model",
            "body": {
                "id": "managed-completion",
                "object": "chat.completion",
                "created": 1,
                "model": "test-model",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "Cloud is healthy."},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 7, "completion_tokens": 4, "total_tokens": 11}
            }
        }))
    }

    async fn cloud_chat_unavailable_stub() -> (StatusCode, Json<serde_json::Value>) {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"detail": "temporary managed inference outage"})),
        )
    }

    async fn cloud_chat_malformed_stub(
        Json(request): Json<ManagedAiChatRequest>,
    ) -> Json<serde_json::Value> {
        Json(serde_json::json!({
            "request_id": request.request_id,
            "settled_credits": 0,
            "provider": "test-provider",
            "model": "test-model",
            "body": {"unexpected": "not an OpenAI completion"}
        }))
    }

    async fn managed_gateway_with_route(
        route: axum::routing::MethodRouter,
    ) -> Option<(GatewayService, tempfile::TempDir)> {
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("sandbox denied TCP bind; skipping managed AI gateway test");
                return None;
            }
            Err(error) => panic!("bind managed AI Cloud stub: {error}"),
        };
        let address = listener.local_addr().expect("Cloud stub address");
        let app = Router::new()
            .route("/v1/enroll", post(enroll_stub))
            .route("/v1/ai/chat/completions", route);
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve Cloud stub");
        });
        let temp = tempfile::tempdir().expect("temporary Cloud state");
        let link = Arc::new(CloudLink::load_for_loopback_development(
            temp.path().to_path_buf(),
            "test-agent",
        ));
        link.configure(
            BackendUrl::loopback_development(&format!("http://{address}"))
                .expect("loopback Cloud URL"),
        )
        .expect("configure Cloud link");
        link.enroll("TEST-CODE")
            .await
            .expect("enroll test instance");
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let encryption = Arc::new(
            EncryptionService::new("01234567890123456789012345678901").expect("test encryption"),
        );
        let provider_keys = Arc::new(ProviderKeyService::new(db, encryption));
        Some((
            GatewayService::new(provider_keys).with_cloud_link(Some(link)),
            temp,
        ))
    }

    fn managed_chat_request(stream: bool) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "temps-cloud".into(),
            messages: vec![ChatMessage {
                role: "user".into(),
                content: Some(MessageContent::Text("Is Cloud healthy?".into())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            }],
            stream,
            temperature: None,
            max_tokens: Some(32),
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

    #[tokio::test]
    async fn managed_cloud_completion_uses_the_linked_instance_path() {
        let Some((gateway, _state_dir)) = managed_gateway_with_route(post(cloud_chat_stub)).await
        else {
            return;
        };

        let (response, credential) = gateway
            .chat_completion(&managed_chat_request(false), &ByokOverride::default())
            .await
            .expect("managed completion");
        assert_eq!(credential, CredentialType::System);
        assert_eq!(response.id, "managed-completion");
        assert_eq!(response.model, "test-model");
    }

    #[tokio::test]
    async fn test_managed_cloud_stream_emits_openai_chunk_and_done_sentinel() {
        let Some((gateway, _state_dir)) = managed_gateway_with_route(post(cloud_chat_stub)).await
        else {
            return;
        };

        let (mut stream, credential) = gateway
            .chat_completion_stream(&managed_chat_request(true), &ByokOverride::default())
            .await
            .expect("managed stream");
        let mut payload = Vec::new();
        while let Some(chunk) = stream.next().await {
            payload.extend_from_slice(&chunk.expect("valid SSE chunk"));
        }
        let payload = String::from_utf8(payload).expect("SSE is UTF-8");

        assert_eq!(credential, CredentialType::System);
        assert!(payload.starts_with("data: {"));
        assert!(payload.contains("\"object\":\"chat.completion.chunk\""));
        assert!(payload.contains("Cloud is healthy."));
        assert!(payload.ends_with("data: [DONE]\n\n"));
    }

    #[tokio::test]
    async fn test_managed_cloud_upstream_outage_returns_contextual_gateway_error() {
        let Some((gateway, _state_dir)) =
            managed_gateway_with_route(post(cloud_chat_unavailable_stub)).await
        else {
            return;
        };

        let error = gateway
            .chat_completion(&managed_chat_request(false), &ByokOverride::default())
            .await
            .expect_err("persistent Cloud outage must not produce a completion");

        match error {
            AiGatewayError::UpstreamError {
                model,
                status,
                message,
            } => {
                assert_eq!(model, "temps-cloud");
                assert_eq!(status, 502);
                assert!(message.contains("503") || message.contains("unavailable"));
            }
            other => panic!("expected contextual upstream error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_managed_cloud_malformed_completion_is_rejected_without_panicking() {
        let Some((gateway, _state_dir)) =
            managed_gateway_with_route(post(cloud_chat_malformed_stub)).await
        else {
            return;
        };

        let error = gateway
            .chat_completion(&managed_chat_request(false), &ByokOverride::default())
            .await
            .expect_err("malformed Cloud response must be rejected");

        match error {
            AiGatewayError::TranslationError { provider, reason } => {
                assert_eq!(provider, "temps-cloud");
                assert!(reason.contains("invalid OpenAI completion"));
            }
            other => panic!("expected translation error, got {other:?}"),
        }
    }

    // -------------------------------------------------------------------------
    // Fix #3 — SSRF guard: validate_external_url rejects every dangerous URL
    // that a BYOK caller might inject via X-Provider-Base-URL.
    // We call the helper directly because wiring up a full GatewayService
    // requires a real DatabaseConnection; the guard lives one call-site above,
    // so unit-testing the validator is equivalent and faster.
    // -------------------------------------------------------------------------

    #[test]
    fn test_byok_base_url_rejects_cloud_metadata() {
        // AWS/GCP/Azure IMDS endpoint — the most critical SSRF target
        assert!(
            validate_external_url("http://169.254.169.254/").is_err(),
            "Cloud metadata service must be rejected"
        );
        assert!(
            validate_external_url(
                "http://169.254.169.254/latest/meta-data/iam/security-credentials/role"
            )
            .is_err(),
            "Cloud metadata deep path must be rejected"
        );
    }

    #[test]
    fn test_byok_base_url_rejects_localhost() {
        assert!(
            validate_external_url("http://localhost:8080").is_err(),
            "localhost must be rejected"
        );
        assert!(
            validate_external_url("http://localhost/").is_err(),
            "localhost root must be rejected"
        );
    }

    #[test]
    fn test_byok_base_url_rejects_loopback_ip() {
        assert!(
            validate_external_url("http://127.0.0.1").is_err(),
            "IPv4 loopback must be rejected"
        );
        assert!(
            validate_external_url("http://127.0.0.1:2375/").is_err(),
            "Docker daemon on loopback must be rejected"
        );
    }

    #[test]
    fn test_byok_base_url_rejects_rfc1918_private() {
        assert!(
            validate_external_url("http://10.0.0.1").is_err(),
            "RFC 1918 10.x must be rejected"
        );
        assert!(
            validate_external_url("http://192.168.1.100/v1").is_err(),
            "RFC 1918 192.168.x must be rejected"
        );
        assert!(
            validate_external_url("http://172.16.0.1/api").is_err(),
            "RFC 1918 172.16.x must be rejected"
        );
    }

    #[test]
    fn test_byok_base_url_accepts_public_providers() {
        // These are valid external provider base URLs and must not be blocked
        assert!(
            validate_external_url("https://api.anthropic.com").is_ok(),
            "Anthropic API must be accepted"
        );
        assert!(
            validate_external_url("https://api.openai.com/v1").is_ok(),
            "OpenAI API must be accepted"
        );
        assert!(
            validate_external_url("https://openrouter.ai/api/v1").is_ok(),
            "OpenRouter must be accepted"
        );
    }

    #[test]
    fn test_byok_base_url_rejects_non_http_scheme() {
        assert!(
            validate_external_url("ftp://external.example.com/").is_err(),
            "FTP scheme must be rejected"
        );
        assert!(
            validate_external_url("file:///etc/passwd").is_err(),
            "file:// scheme must be rejected"
        );
    }

    // -------------------------------------------------------------------------
    // Existing tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_gateway_service_has_all_providers() {
        // We can't create a full GatewayService without a ProviderKeyService,
        // but we can verify the provider registry logic
        let providers: Vec<&str> = vec!["openai", "anthropic", "xai", "gemini"];

        for provider_id in &providers {
            assert!(
                route_model_to_provider(match *provider_id {
                    "openai" => "gpt-4o",
                    "anthropic" => "claude-sonnet-4-6",
                    "xai" => "grok-3",
                    "gemini" => "gemini-3.1-pro",
                    _ => unreachable!(),
                })
                .is_some(),
                "Model routing failed for provider: {}",
                provider_id
            );
        }
    }

    #[test]
    fn test_byok_override_default_is_empty() {
        let byok = ByokOverride::default();
        assert!(byok.api_key.is_none());
        assert!(byok.base_url.is_none());
    }

    #[test]
    fn test_credential_type_equality() {
        assert_eq!(CredentialType::System, CredentialType::System);
        assert_eq!(CredentialType::Byok, CredentialType::Byok);
        assert_ne!(CredentialType::System, CredentialType::Byok);
    }
}
