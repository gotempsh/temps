pub mod anthropic;
pub mod gemini;
pub mod openai_compat;
mod openai_responses;

use async_trait::async_trait;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio_stream::Stream;

use crate::error::AiGatewayError;
use crate::types::{
    ChatCompletionRequest, ChatCompletionResponse, EmbeddingRequest, EmbeddingResponse, ModelInfo,
};

/// Capabilities that a provider adapter can support.
/// Used to advertise what a provider can do so the gateway
/// can reject unsupported requests early.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderCapability {
    ChatCompletion,
    ChatCompletionStreaming,
    Embeddings,
    ToolUse,
    Vision,
    JsonMode,
}

/// Provider-level metadata returned alongside every provider instance.
#[derive(Debug, Clone)]
pub struct ProviderInfo {
    /// Canonical provider ID: "openai", "anthropic", "xai", etc.
    pub id: &'static str,
    /// Human display name
    pub display_name: &'static str,
    /// Default upstream base URL
    pub default_base_url: &'static str,
    /// Features this adapter supports
    pub capabilities: &'static [ProviderCapability],
}

/// The core provider trait. Each AI provider (OpenAI, Anthropic, Gemini, etc.)
/// implements this trait to handle request/response translation.
///
/// OpenAI-compatible providers (xAI) share the same
/// `OpenAiCompatProvider` implementation — only the base URL and API key differ.
#[async_trait]
pub trait AiProvider: Send + Sync {
    /// Static metadata about this provider
    fn info(&self) -> &ProviderInfo;

    /// Whether this provider can handle the given model name
    fn supports_model(&self, model: &str) -> bool;

    /// List models available from this provider
    fn available_models(&self) -> Vec<ModelInfo>;

    /// Execute a chat completion (non-streaming).
    /// The gateway passes the OpenAI-format request; the provider translates
    /// to its native format, calls the upstream, and translates back to OpenAI format.
    async fn chat_completion(
        &self,
        api_key: &str,
        base_url: Option<&str>,
        request: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, AiGatewayError>;

    /// Execute a streaming chat completion.
    /// Returns a stream of SSE-formatted bytes in OpenAI's `data: {...}\n\n` format.
    /// The final chunk must be `data: [DONE]\n\n`.
    async fn chat_completion_stream(
        &self,
        api_key: &str,
        base_url: Option<&str>,
        request: &ChatCompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<Bytes, AiGatewayError>> + Send>>, AiGatewayError>;

    /// Execute an embeddings request (optional — providers that don't support
    /// embeddings return `ModelNotFound`).
    async fn embeddings(
        &self,
        api_key: &str,
        base_url: Option<&str>,
        request: &EmbeddingRequest,
    ) -> Result<EmbeddingResponse, AiGatewayError> {
        let _ = (api_key, base_url, request);
        Err(AiGatewayError::ModelNotFound {
            model: "embeddings".to_string(),
        })
    }
}

/// Route a model name to the provider ID that should handle it.
pub fn route_model_to_provider(model: &str) -> Option<&'static str> {
    let model_lower = model.to_lowercase();

    if model_lower.starts_with("gpt-")
        || model_lower.starts_with("o1")
        || model_lower.starts_with("o3")
        || model_lower.starts_with("o4")
        || model_lower.starts_with("text-embedding-")
        || model_lower.starts_with("dall-e")
        || model_lower.starts_with("chatgpt-")
        || model_lower.starts_with("codex-")
    {
        return Some("openai");
    }

    if model_lower.starts_with("claude-") {
        return Some("anthropic");
    }

    if model_lower.starts_with("grok-") {
        return Some("xai");
    }

    if model_lower.starts_with("gemini-") {
        return Some("gemini");
    }

    None
}

/// OpenAI-compatible providers reuse the same adapter with different config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub provider_id: String,
    pub api_key: String,
    pub base_url: String,
}

/// DNS resolver used by every provider HTTP client. It rejects an entire DNS
/// answer when any address is private or otherwise non-public, so a custom
/// provider hostname cannot rebind to metadata or an internal service between
/// URL validation and connect time.
#[derive(Debug)]
struct ExternalOnlyResolver;

impl reqwest::dns::Resolve for ExternalOnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|error| -> Box<dyn std::error::Error + Send + Sync> { Box::new(error) })?
                .collect();
            if addresses.is_empty() {
                return Err(format!("provider hostname '{host}' resolved to no addresses").into());
            }
            for address in &addresses {
                let result = match address.ip() {
                    std::net::IpAddr::V4(ip) => temps_core::url_validation::validate_ipv4(&ip),
                    std::net::IpAddr::V6(ip) => temps_core::url_validation::validate_ipv6(&ip),
                };
                if result.is_err() {
                    return Err(format!(
                        "provider hostname '{host}' resolved to a blocked internal address"
                    )
                    .into());
                }
            }
            let addresses: reqwest::dns::Addrs = Box::new(addresses.into_iter());
            Ok(addresses)
        })
    }
}

/// Build the hardened client shared by inference and model discovery.
/// Redirects are disabled because an otherwise-public provider endpoint must
/// not forward an API key to an attacker-selected internal redirect target.
pub(crate) fn external_http_client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .dns_resolver(Arc::new(ExternalOnlyResolver))
        .build()
        .expect("provider HTTP client configuration is valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_route_openai_models() {
        assert_eq!(route_model_to_provider("gpt-4o"), Some("openai"));
        assert_eq!(route_model_to_provider("gpt-4o-mini"), Some("openai"));
        assert_eq!(route_model_to_provider("gpt-3.5-turbo"), Some("openai"));
        assert_eq!(route_model_to_provider("o1-preview"), Some("openai"));
        assert_eq!(route_model_to_provider("o3-mini"), Some("openai"));
        assert_eq!(
            route_model_to_provider("text-embedding-3-small"),
            Some("openai")
        );
    }

    #[test]
    fn test_route_anthropic_models() {
        assert_eq!(
            route_model_to_provider("claude-sonnet-4-6"),
            Some("anthropic")
        );
        assert_eq!(
            route_model_to_provider("claude-haiku-4-5"),
            Some("anthropic")
        );
    }

    #[test]
    fn test_route_other_providers() {
        assert_eq!(route_model_to_provider("grok-3"), Some("xai"));
        assert_eq!(route_model_to_provider("gemini-3.1-pro"), Some("gemini"));
    }

    #[test]
    fn test_route_unknown_model() {
        assert_eq!(route_model_to_provider("unknown-model"), None);
        assert_eq!(route_model_to_provider("llama-3"), None);
    }

    #[test]
    fn test_route_case_insensitive() {
        assert_eq!(route_model_to_provider("GPT-4o"), Some("openai"));
        assert_eq!(
            route_model_to_provider("Claude-Sonnet-4"),
            Some("anthropic")
        );
    }
}
