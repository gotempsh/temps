use bytes::Bytes;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use tokio_stream::Stream;
use tracing::{debug, warn};

use crate::error::AiGatewayError;
use crate::providers::anthropic::AnthropicProvider;
use crate::providers::gemini::GeminiProvider;
use crate::providers::openai_compat::OpenAiCompatProvider;
use crate::providers::{route_model_to_provider, AiProvider};
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
}

/// The core gateway service that routes requests to the appropriate provider,
/// handles key decryption, and coordinates the entire request lifecycle.
pub struct GatewayService {
    provider_key_service: Arc<ProviderKeyService>,
    providers: HashMap<&'static str, Box<dyn AiProvider>>,
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
        }
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
        let key_record = self
            .provider_key_service
            .get_active_by_provider(provider_id)
            .await?
            .ok_or_else(|| AiGatewayError::ProviderNotConfigured {
                provider: provider_id.to_string(),
            })?;

        let decrypted_key = self
            .provider_key_service
            .decrypt_api_key(&key_record.api_key_encrypted)?;

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
        let (provider, api_key, base_url, cred_type) =
            self.resolve_provider_and_key(&request.model, byok).await?;

        let stream = provider
            .chat_completion_stream(&api_key, base_url.as_deref(), request)
            .await?;

        Ok((stream, cred_type))
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
        let provider = self.providers.get(provider_id).ok_or_else(|| {
            AiGatewayError::ProviderNotConfigured {
                provider: provider_id.to_string(),
            }
        })?;

        // Pick the cheapest/smallest model for the test
        let test_model = match provider_id {
            "openai" => "gpt-5-nano",
            "anthropic" => "claude-haiku-4-5",
            "xai" => "grok-4-1-fast-non-reasoning",
            "gemini" => "gemini-2.5-flash-lite",
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
    use temps_core::url_validation::validate_external_url;

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
