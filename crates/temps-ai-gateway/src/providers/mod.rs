// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

pub mod anthropic;
pub mod gemini;
pub mod openai_compat;
pub mod openai_native;
mod openai_responses;
pub(crate) mod prompt_cache;

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

/// Name-prefix heuristic for the built-in vendor providers.
///
/// This is only a **fallback**: requests are routed by exact model ID against
/// the configured catalog first (`GatewayService::resolve_credentials`), and
/// this heuristic is consulted only when no catalog entry lists the model and
/// no self-hosted key hosts it. It never returns a self-hosted provider.
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

    // OpenRouter model ids are always `vendor/model` (e.g. "openai/gpt-4o",
    // "anthropic/claude-sonnet-5"); no native provider uses a slash, so this
    // is an unambiguous signal.
    if model_lower.contains('/') {
        return Some("openrouter");
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

/// Why the connect-time resolver refused a provider hostname. Kept as a typed
/// error so it survives reqwest's "error sending request" wrapping and can be
/// reported as an `InvalidProviderUrl` with the trusted-networks hint (#1410).
///
/// The message only describes the DNS answer, never the connection, so it
/// cannot be used to learn whether a port is open.
#[derive(Debug)]
pub(crate) struct BlockedProviderAddress {
    host: String,
    plaintext: bool,
}

impl std::fmt::Display for BlockedProviderAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.plaintext {
            write!(
                f,
                "provider hostname '{}' resolved to an address that is not a trusted private \
                 address; plain http:// is only allowed to trusted private addresses, so an API \
                 key and prompts are never sent unencrypted over the internet. Use https://, or \
                 add the address to the trusted private networks{}",
                self.host,
                crate::provider_url::untrusted_hint()
            )
        } else {
            write!(
                f,
                "provider hostname '{}' resolved to a private, loopback or otherwise internal \
                 address that is not trusted{}",
                self.host,
                crate::provider_url::untrusted_hint()
            )
        }
    }
}

impl std::error::Error for BlockedProviderAddress {}

/// Resolve `host` and require every address to satisfy `allowed`. A single
/// bad address rejects the whole DNS answer, so a hostname cannot rebind to
/// metadata or an internal service between URL validation and connect time.
async fn resolve_checked(
    host: String,
    plaintext: bool,
    allowed: fn(std::net::IpAddr) -> bool,
) -> Result<reqwest::dns::Addrs, Box<dyn std::error::Error + Send + Sync>> {
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
        .await
        .map_err(|error| -> Box<dyn std::error::Error + Send + Sync> { Box::new(error) })?
        .collect();
    if addresses.is_empty() {
        return Err(format!("provider hostname '{host}' resolved to no addresses").into());
    }
    if addresses.iter().any(|address| !allowed(address.ip())) {
        return Err(Box::new(BlockedProviderAddress { host, plaintext }));
    }
    Ok(Box::new(addresses.into_iter()))
}

/// Resolver for `https://` provider URLs: public addresses, plus private ones
/// the operator trusted instance-wide (a self-hosted model server on the LAN).
#[derive(Debug)]
struct ExternalOnlyResolver;

impl reqwest::dns::Resolve for ExternalOnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(resolve_checked(host, false, |ip| {
            temps_core::url_validation::validate_outbound_ip(ip).is_ok()
        }))
    }
}

/// Resolver for plain `http://` provider URLs: only trusted private
/// addresses. A hostname that resolves (or later rebinds) to a public address
/// is refused, so a key is never sent unencrypted over the internet.
#[derive(Debug)]
struct TrustedPrivateOnlyResolver;

impl reqwest::dns::Resolve for TrustedPrivateOnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(resolve_checked(
            host,
            true,
            temps_core::url_validation::is_trusted_private_destination,
        ))
    }
}

#[allow(
    clippy::expect_used,
    reason = "the builder only sets timeouts, a redirect policy and a resolver; with the \
              rustls backend and bundled webpki roots, build() has no failure path for this \
              configuration, and falling back to an unhardened client would bypass the SSRF \
              resolver"
)]
fn build_client<R: reqwest::dns::Resolve + 'static>(
    timeout: Duration,
    resolver: Arc<R>,
) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .dns_resolver(resolver)
        .build()
        .expect("provider HTTP client configuration is valid")
}

/// The hardened HTTP client shared by inference and model discovery.
///
/// Holds one client per transport policy and picks by URL scheme, so call
/// sites keep using `client.post(url)`:
/// * `https://` → [`ExternalOnlyResolver`]
/// * `http://`  → [`TrustedPrivateOnlyResolver`]
///
/// Redirects are disabled on both because an otherwise-allowed provider
/// endpoint must not forward an API key to an attacker-selected redirect
/// target. IP-literal hosts never reach a resolver; they are checked before
/// the request by [`crate::provider_url::check_provider_url`].
#[derive(Clone)]
pub(crate) struct ProviderHttpClient {
    tls: reqwest::Client,
    plaintext: reqwest::Client,
}

impl ProviderHttpClient {
    pub(crate) fn new(timeout: Duration) -> Self {
        Self {
            tls: build_client(timeout, Arc::new(ExternalOnlyResolver)),
            plaintext: build_client(timeout, Arc::new(TrustedPrivateOnlyResolver)),
        }
    }

    /// Unrestricted client for unit tests that talk to a loopback stub.
    #[cfg(test)]
    pub(crate) fn unrestricted_for_tests() -> Self {
        Self {
            tls: reqwest::Client::new(),
            plaintext: reqwest::Client::new(),
        }
    }

    /// Pick the client from the scheme reqwest itself will use. The URL is
    /// parsed (WHATWG rules, like reqwest) rather than prefix-matched, because
    /// forms such as `http:host/v1`, ` http://host` or `HTTP:\\host` all
    /// parse to plain `http://host`; a string check would hand them to the
    /// TLS client, whose resolver accepts public addresses. Anything that is
    /// not unambiguously `https` gets the stricter plaintext client.
    fn for_url(&self, url: &str) -> &reqwest::Client {
        match reqwest::Url::parse(url) {
            Ok(parsed) if parsed.scheme() == "https" => &self.tls,
            _ => &self.plaintext,
        }
    }

    pub(crate) fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        self.for_url(url).request(method, url)
    }

    pub(crate) fn get(&self, url: &str) -> reqwest::RequestBuilder {
        self.for_url(url).get(url)
    }

    pub(crate) fn post(&self, url: &str) -> reqwest::RequestBuilder {
        self.for_url(url).post(url)
    }
}

/// Attach `Authorization: Bearer` unless the key is empty: self-hosted
/// servers commonly run without authentication, and an empty bearer token
/// is rejected by some of them.
pub(crate) fn with_bearer(
    request: reqwest::RequestBuilder,
    api_key: &str,
) -> reqwest::RequestBuilder {
    if api_key.is_empty() {
        request
    } else {
        request.bearer_auth(api_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_is_chosen_from_the_parsed_scheme_not_a_prefix() {
        let client = ProviderHttpClient::new(Duration::from_secs(1));
        let plaintext = &client.plaintext as *const reqwest::Client;
        let tls = &client.tls as *const reqwest::Client;
        for url in [
            "http://models.example.com/v1",
            "HTTP://models.example.com/v1",
            "http:models.example.com/v1",
            "http:/models.example.com/v1",
            "http:\\\\models.example.com\\v1",
            " http://models.example.com/v1",
            "ht\ttp://models.example.com/v1",
            "not a url",
        ] {
            assert_eq!(
                client.for_url(url) as *const reqwest::Client,
                plaintext,
                "{url:?} must use the trusted-private-only client"
            );
        }
        for url in [
            "https://api.openai.com/v1",
            "HTTPS://api.openai.com/v1",
            "https:api.openai.com/v1",
        ] {
            assert_eq!(
                client.for_url(url) as *const reqwest::Client,
                tls,
                "{url:?}"
            );
        }
    }

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
    fn test_route_openrouter_models() {
        assert_eq!(route_model_to_provider("openai/gpt-4o"), Some("openrouter"));
        assert_eq!(
            route_model_to_provider("anthropic/claude-sonnet-5"),
            Some("openrouter")
        );
        assert_eq!(
            route_model_to_provider("meta-llama/llama-3.3-70b-instruct"),
            Some("openrouter")
        );
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
