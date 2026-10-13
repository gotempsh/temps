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
use crate::provider_url::{check_provider_url, validate_provider_url, KeyTrust};
use crate::providers::anthropic::AnthropicProvider;
use crate::providers::gemini::GeminiProvider;
use crate::providers::openai_compat::OpenAiCompatProvider;
use crate::providers::{route_model_to_provider, with_bearer, AiProvider, ProviderHttpClient};
use crate::self_hosted::{is_self_hosted, SelfHostedCapabilities, OPENAI_COMPATIBLE};
use crate::services::provider_key_service::{CatalogMembership, ProviderKeyService};
use crate::types::*;
use temps_entities::{ai_provider_keys, ai_provider_models};

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
    /// Exact administrator-configured key selected by a pinned conversation
    /// or the `X-Temps-Provider-Key-Id` header. Missing or inactive IDs fail
    /// closed; they never fall back to another key.
    pub system_key_id: Option<i32>,
}

/// Provider and credentials resolved for one request.
#[derive(Clone)]
pub struct ResolvedCredentials {
    /// Canonical provider ID, e.g. "openai"
    pub provider_id: &'static str,
    /// Decrypted (or caller-supplied) provider API key
    pub api_key: String,
    /// Upstream base URL override; `None` means the adapter default
    pub base_url: Option<String>,
    pub credential_type: CredentialType,
    /// Administrator-configured key that was used; `None` for BYOK
    pub system_key_id: Option<i32>,
    /// Exact private address the key trusts for itself (#1411).
    pub key_trust: Option<KeyTrust>,
    /// Declared capabilities of a self-hosted key; `None` for vendor keys.
    pub self_hosted_capabilities: Option<SelfHostedCapabilities>,
}

impl ResolvedCredentials {
    /// Re-check the base URL right before sending. IP literals are fully
    /// decided here; hostnames are checked again by the client's resolver.
    pub(crate) fn check_base_url(&self) -> Result<(), AiGatewayError> {
        if let Some(base_url) = self.base_url.as_deref() {
            check_provider_url(base_url, self.key_trust)
                .map_err(|error| self.caller_facing(error))?;
        }
        Ok(())
    }

    /// A blocked base URL on an administrator-configured key is an operator
    /// problem: gateway callers (including deployment tokens) get a generic
    /// message without the configured host or network details, which are
    /// logged instead. Admins see the full reason via "Test key". BYOK
    /// callers supplied the URL themselves and get the full reason.
    pub(crate) fn caller_facing(&self, error: AiGatewayError) -> AiGatewayError {
        match (self.credential_type, self.system_key_id, error) {
            (
                CredentialType::System,
                Some(key_id),
                AiGatewayError::InvalidProviderUrl { reason },
            ) => {
                warn!(
                    provider_key_id = key_id,
                    reason = %reason,
                    "Configured AI provider base URL blocked by the outbound policy"
                );
                AiGatewayError::InvalidProviderUrl {
                    reason: format!(
                        "the base URL configured on provider key {key_id} is not allowed by this \
                         instance's outbound policy; an administrator can see why by testing the \
                         key under AI Gateway"
                    ),
                }
            }
            (_, _, error) => error,
        }
    }

    /// Refuse a request that needs a capability the self-hosted key did not
    /// declare. Vendor keys are governed by their adapters instead.
    pub(crate) fn require_capability(
        &self,
        model: &str,
        capability: &str,
        declared: impl Fn(&SelfHostedCapabilities) -> bool,
    ) -> Result<(), AiGatewayError> {
        match (&self.self_hosted_capabilities, self.system_key_id) {
            (Some(capabilities), Some(key_id)) if !declared(capabilities) => {
                Err(AiGatewayError::CapabilityNotDeclared {
                    key_id,
                    model: model.to_string(),
                    capability: capability.to_string(),
                })
            }
            _ => Ok(()),
        }
    }
}

/// Which provider and credential served a request, for usage accounting and
/// the `x-temps-provider` response header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServedBy {
    pub provider_id: &'static str,
    pub credential_type: CredentialType,
    pub system_key_id: Option<i32>,
}

impl ServedBy {
    fn from_credentials(credentials: &ResolvedCredentials) -> Self {
        Self {
            provider_id: credentials.provider_id,
            credential_type: credentials.credential_type,
            system_key_id: credentials.system_key_id,
        }
    }
}

/// Outcome of matching a model ID against the configured catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CatalogRoute {
    /// Exactly one upstream destination serves the model; this key is used.
    Key(i32),
    /// More than one distinct upstream destination serves the model.
    Ambiguous(Vec<(i32, String)>),
    /// A self-hosted key hosts the model but cannot serve it right now. The
    /// name heuristic must not be consulted: that could send it to a vendor.
    SelfHostedUnavailable { key_id: i32, reason: String },
    /// No catalog entry lists the model.
    NotInCatalog,
}

/// Identity of an upstream destination for comparing two keys' base URLs.
///
/// Only the case-insensitive parts are normalised: parsing lowercases the
/// scheme and host and drops a default port, while the path keeps its case
/// (`/Private/v1` and `/private/v1` are different endpoints) and loses only a
/// trailing slash. A URL that does not parse is compared verbatim, so it can
/// never be mistaken for another destination.
fn destination_identity(base_url: &str) -> String {
    let trimmed = base_url.trim();
    match url::Url::parse(trimmed) {
        Ok(parsed) => {
            let mut identity = format!(
                "{}://{}",
                parsed.scheme(),
                parsed.host_str().unwrap_or_default()
            );
            if let Some(port) = parsed.port() {
                identity.push_str(&format!(":{port}"));
            }
            identity.push_str(parsed.path().trim_end_matches('/'));
            if let Some(query) = parsed.query() {
                identity.push('?');
                identity.push_str(query);
            }
            identity
        }
        Err(_) => trimmed.to_string(),
    }
}

/// Decide which key serves `model` from its catalog entries (#1408).
///
/// `entries` are every catalog row whose model ID matches `model` ignoring
/// case, in any state. A row serves when it matches exactly, is enabled and
/// available, and its key is active. Two serving keys are only
/// interchangeable when they reach the same upstream destination (same
/// provider type and effective base URL) -- e.g. two OpenAI keys on the
/// default endpoint -- in which case the lowest key id is used, as before.
/// Anything else is ambiguous and refused.
///
/// A self-hosted key that lists the model but cannot serve it right now (key
/// inactive, model disabled or no longer listed, or listed with different
/// capitalisation) still "hosts" it: no other destination may serve it, and
/// the name heuristic is never consulted. Only an explicitly pinned key can
/// override that.
pub(crate) fn route_by_catalog(
    model: &str,
    entries: &[(ai_provider_models::Model, ai_provider_keys::Model)],
    default_base_url: impl Fn(&str) -> Option<&'static str>,
) -> CatalogRoute {
    let destination = |key: &ai_provider_keys::Model| {
        let base = key
            .base_url
            .clone()
            .or_else(|| default_base_url(&key.provider).map(str::to_string))
            .unwrap_or_default();
        (key.provider.clone(), destination_identity(&base))
    };
    let is_serving = |(row, key): &&(ai_provider_models::Model, ai_provider_keys::Model)| {
        row.model_id == model && key.is_active && row.is_enabled && row.is_available
    };
    let serving: Vec<&ai_provider_keys::Model> = entries
        .iter()
        .filter(is_serving)
        .map(|(_, key)| key)
        .collect();
    let hosted_elsewhere = |served: Option<&(String, String)>| {
        entries.iter().find(|entry| {
            let (_, key) = entry;
            is_self_hosted(&key.provider)
                && !is_serving(entry)
                && served.is_none_or(|served| destination(key) != *served)
        })
    };
    let unavailable = |(row, key): &(ai_provider_models::Model, ai_provider_keys::Model)| {
        let reason = if row.model_id != model {
            format!("it lists the model as '{}'", row.model_id)
        } else if !key.is_active {
            "the key is inactive".to_string()
        } else if !row.is_enabled {
            "the model is disabled on that key".to_string()
        } else {
            "the server no longer lists the model".to_string()
        };
        CatalogRoute::SelfHostedUnavailable {
            key_id: key.id,
            reason,
        }
    };

    if let Some(first) = serving.iter().min_by_key(|key| key.id) {
        let first_destination = destination(first);
        if !serving
            .iter()
            .all(|key| destination(key) == first_destination)
        {
            let mut candidates: Vec<(i32, String)> = serving
                .iter()
                .map(|key| (key.id, key.provider.clone()))
                .collect();
            candidates.sort();
            candidates.dedup();
            return CatalogRoute::Ambiguous(candidates);
        }
        if let Some(entry) = hosted_elsewhere(Some(&first_destination)) {
            return unavailable(entry);
        }
        return CatalogRoute::Key(first.id);
    }
    if let Some(entry) = hosted_elsewhere(None) {
        return unavailable(entry);
    }
    CatalogRoute::NotInCatalog
}

impl std::fmt::Debug for ResolvedCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the API key.
        f.debug_struct("ResolvedCredentials")
            .field("provider_id", &self.provider_id)
            .field("base_url", &self.base_url)
            .field("credential_type", &self.credential_type)
            .field("system_key_id", &self.system_key_id)
            .finish_non_exhaustive()
    }
}

/// The core gateway service that routes requests to the appropriate provider,
/// handles key decryption, and coordinates the entire request lifecycle.
pub struct GatewayService {
    provider_key_service: Arc<ProviderKeyService>,
    providers: HashMap<&'static str, Box<dyn AiProvider>>,
    cloud_link: Option<Arc<temps_cloud_client::CloudLink>>,
}

impl GatewayService {
    pub(crate) fn encrypt_native_key(&self, key: &str) -> Result<String, AiGatewayError> {
        self.provider_key_service.encrypt_api_key(key)
    }
    pub(crate) fn decrypt_native_key(&self, key: &str) -> Result<String, AiGatewayError> {
        self.provider_key_service.decrypt_api_key(key)
    }

    pub fn new(provider_key_service: Arc<ProviderKeyService>) -> Self {
        let mut providers: HashMap<&'static str, Box<dyn AiProvider>> = HashMap::new();

        providers.insert("openai", Box::new(OpenAiCompatProvider::openai()));
        providers.insert("anthropic", Box::new(AnthropicProvider::new()));
        providers.insert("xai", Box::new(OpenAiCompatProvider::xai()));
        providers.insert("gemini", Box::new(GeminiProvider::new()));
        providers.insert("openrouter", Box::new(OpenAiCompatProvider::openrouter()));
        providers.insert(
            OPENAI_COMPATIBLE,
            Box::new(OpenAiCompatProvider::openai_compatible()),
        );
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
        let api_key = self
            .provider_key_service
            .decrypt_api_key(&key.api_key_encrypted)?;
        self.discover_models(
            &key.provider,
            &api_key,
            key.base_url.as_deref(),
            KeyTrust::from_stored(key.trusted_private_address.as_deref()),
        )
        .await
    }

    /// List the models a provider endpoint serves, without persisting
    /// anything. Used before a key is created (so the administrator can pick
    /// a default model from a self-hosted server) and by catalog refreshes.
    ///
    /// Vendor providers keep only model IDs their adapter recognises. A
    /// self-hosted server's listing is taken as-is: its IDs are arbitrary.
    pub async fn discover_models(
        &self,
        provider_id: &str,
        api_key: &str,
        base_url: Option<&str>,
        key_trust: Option<KeyTrust>,
    ) -> Result<Vec<ModelInfo>, AiGatewayError> {
        let provider = self.providers.get(provider_id).ok_or_else(|| {
            AiGatewayError::ProviderNotConfigured {
                provider: provider_id.to_string(),
            }
        })?;
        let self_hosted = is_self_hosted(provider_id);
        if self_hosted && base_url.is_none() {
            return Err(AiGatewayError::Validation {
                message: "A base URL is required to discover models on a self-hosted provider"
                    .to_string(),
            });
        }
        if let Some(base_url) = base_url {
            validate_provider_url(base_url, key_trust).await?;
        }
        let base = base_url
            .unwrap_or(provider.info().default_base_url)
            .trim_end_matches('/');
        let client = ProviderHttpClient::new(std::time::Duration::from_secs(30));

        let response = match provider_id {
            "openai" | "xai" | "openrouter" | OPENAI_COMPATIBLE => {
                with_bearer(client.get(&format!("{base}/models")), api_key)
                    .send()
                    .await?
            }
            "anthropic" => {
                client
                    .get(&format!("{base}/v1/models?limit=1000"))
                    .header("x-api-key", api_key)
                    .header("anthropic-version", "2023-06-01")
                    .send()
                    .await?
            }
            "gemini" => {
                client
                    .get(&format!("{base}/v1beta/models"))
                    .query(&[("pageSize", "1000"), ("key", api_key)])
                    .send()
                    .await?
            }
            _ => {
                return Err(AiGatewayError::ProviderNotConfigured {
                    provider: provider_id.to_string(),
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
                provider: provider_id.to_string(),
                reason: "Provider model catalog exceeded the 1 MiB limit".to_string(),
            });
        }
        let mut response_stream = response.bytes_stream();
        let mut response_bytes = Vec::new();
        while let Some(chunk) = response_stream.next().await {
            let chunk = chunk?;
            if response_bytes.len().saturating_add(chunk.len()) > MAX_MODEL_CATALOG_BYTES {
                return Err(AiGatewayError::TranslationError {
                    provider: provider_id.to_string(),
                    reason: "Provider model catalog exceeded the 1 MiB limit".to_string(),
                });
            }
            response_bytes.extend_from_slice(&chunk);
        }
        let body: serde_json::Value = serde_json::from_slice(&response_bytes).map_err(|error| {
            AiGatewayError::TranslationError {
                provider: provider_id.to_string(),
                reason: format!("Failed to parse provider model catalog: {error}"),
            }
        })?;
        parse_model_catalog(provider_id, &body, |id| {
            self_hosted || provider.supports_model(id)
        })
    }

    /// List a self-hosted server's models and check the administrator's
    /// chosen default against that listing. A self-hosted server has no
    /// vendor default to verify with, so this runs before a key is created;
    /// the listing it returns seeds the new key's catalog.
    pub async fn discover_self_hosted_with_default(
        &self,
        api_key: &str,
        base_url: Option<&str>,
        default_model: Option<&str>,
        key_trust: Option<KeyTrust>,
    ) -> Result<SelfHostedListing, AiGatewayError> {
        let models = self
            .discover_models(OPENAI_COMPATIBLE, api_key, base_url, key_trust)
            .await
            .map_err(|error| match error {
                AiGatewayError::InvalidProviderUrl { .. } => error,
                other => AiGatewayError::Validation {
                    message: format!(
                        "Could not list models at the self-hosted server's /models endpoint: {}",
                        other.summary()
                    ),
                },
            })?;
        let default_model = choose_self_hosted_default(&models, default_model)?;
        Ok(SelfHostedListing {
            models,
            default_model,
        })
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
    ) -> Result<(&dyn AiProvider, ResolvedCredentials), AiGatewayError> {
        let resolved = self.resolve_credentials(model, byok).await?;
        let provider = self.providers.get(resolved.provider_id).ok_or_else(|| {
            AiGatewayError::ProviderNotConfigured {
                provider: resolved.provider_id.to_string(),
            }
        })?;
        resolved.check_base_url()?;
        Ok((provider.as_ref(), resolved))
    }

    fn provider_id(&self, provider: &str) -> Result<&'static str, AiGatewayError> {
        self.providers
            .get_key_value(provider)
            .map(|(id, _)| *id)
            .ok_or_else(|| AiGatewayError::ProviderNotConfigured {
                provider: provider.to_string(),
            })
    }

    /// Resolve the provider and credentials that serve `model`, without
    /// sending anything upstream.
    ///
    /// BYOK keys are used as supplied (with their base URL validated) and
    /// routed by the vendor name heuristic: the caller chose the vendor by
    /// supplying its key. System keys are routed by the configured catalog
    /// (see [`route_by_catalog`]); the name heuristic is only a fallback for
    /// models no catalog lists.
    pub async fn resolve_credentials(
        &self,
        model: &str,
        byok: &ByokOverride,
    ) -> Result<ResolvedCredentials, AiGatewayError> {
        // BYOK: caller supplied their own key — skip DB lookup entirely
        if let Some(ref user_key) = byok.api_key {
            let provider_id =
                route_model_to_provider(model).ok_or_else(|| AiGatewayError::ModelNotFound {
                    model: model.to_string(),
                })?;
            if !self.providers.contains_key(provider_id) {
                return Err(AiGatewayError::ProviderNotConfigured {
                    provider: provider_id.to_string(),
                });
            }
            // Validate the caller-supplied base URL to prevent SSRF.
            // An authenticated user must not be able to point the gateway at
            // internal services (cloud metadata, Docker daemon, private
            // subnets, etc.), nor send their key over plain http to a public
            // host.
            if let Some(ref base_url) = byok.base_url {
                check_provider_url(base_url, None)?;
            }

            debug!(
                provider = provider_id,
                model = model,
                credential_type = "byok",
                "Routing request to provider (BYOK)"
            );
            return Ok(ResolvedCredentials {
                provider_id,
                api_key: user_key.clone(),
                base_url: byok.base_url.clone(),
                credential_type: CredentialType::Byok,
                system_key_id: None,
                key_trust: None,
                self_hosted_capabilities: None,
            });
        }

        let key_record = self.select_system_key(model, byok.system_key_id).await?;
        let provider_id = self.provider_id(&key_record.provider)?;
        self.credentials_for_key(key_record, provider_id, model)
    }

    /// Pick the administrator-configured key that serves `model` (#1408).
    pub(crate) async fn select_system_key(
        &self,
        model: &str,
        pinned_key_id: Option<i32>,
    ) -> Result<ai_provider_keys::Model, AiGatewayError> {
        let entries = self
            .provider_key_service
            .catalog_entries_for_model(model)
            .await?;

        if let Some(key_id) = pinned_key_id {
            let key = self.provider_key_service.get_by_id(key_id).await?;
            if !key.is_active {
                return Err(AiGatewayError::Validation {
                    message: format!("AI provider key {key_id} is inactive"),
                });
            }
            let membership = self
                .provider_key_service
                .ensure_model_selectable(key.id, model)
                .await?;
            if membership == CatalogMembership::LegacyUncataloged {
                // A key without any catalog yet can only serve what its
                // vendor's naming says it serves -- and never a model that a
                // self-hosted key hosts.
                let hosted_elsewhere = entries
                    .iter()
                    .find(|(_, other)| is_self_hosted(&other.provider) && other.id != key.id);
                if let Some((_, host)) = hosted_elsewhere {
                    return Err(AiGatewayError::Validation {
                        message: format!(
                            "AI provider key {key_id} cannot serve model '{model}': it is hosted \
                             by self-hosted provider key {}",
                            host.id
                        ),
                    });
                }
                if is_self_hosted(&key.provider)
                    || route_model_to_provider(model) != Some(key.provider.as_str())
                {
                    return Err(AiGatewayError::Validation {
                        message: format!(
                            "AI provider key {key_id} is configured for '{}' and cannot serve \
                             model '{model}'",
                            key.provider
                        ),
                    });
                }
            }
            return Ok(key);
        }

        let route = route_by_catalog(model, &entries, |provider| {
            self.providers
                .get(provider)
                .map(|adapter| adapter.info().default_base_url)
        });
        match route {
            CatalogRoute::Key(key_id) => entries
                .into_iter()
                .map(|(_, key)| key)
                .find(|key| key.id == key_id)
                .ok_or(AiGatewayError::ProviderKeyNotFound { key_id }),
            CatalogRoute::Ambiguous(candidates) => Err(AiGatewayError::AmbiguousModel {
                model: model.to_string(),
                candidates: candidates
                    .iter()
                    .map(|(id, provider)| format!("key {id} ({provider})"))
                    .collect::<Vec<_>>()
                    .join(", "),
            }),
            CatalogRoute::SelfHostedUnavailable { key_id, reason } => {
                Err(AiGatewayError::SelfHostedModelUnavailable {
                    model: model.to_string(),
                    key_id,
                    reason,
                })
            }
            CatalogRoute::NotInCatalog => {
                // Fallback for vendor keys created before the catalog
                // existed: the name heuristic, which never names a
                // self-hosted provider.
                let provider_id = route_model_to_provider(model).ok_or_else(|| {
                    AiGatewayError::ModelNotFound {
                        model: model.to_string(),
                    }
                })?;
                let key = self
                    .provider_key_service
                    .get_active_by_provider(provider_id)
                    .await?
                    .ok_or_else(|| AiGatewayError::ProviderNotConfigured {
                        provider: provider_id.to_string(),
                    })?;
                self.provider_key_service
                    .ensure_model_selectable(key.id, model)
                    .await?;
                Ok(key)
            }
        }
    }

    fn credentials_for_key(
        &self,
        key_record: ai_provider_keys::Model,
        provider_id: &'static str,
        model: &str,
    ) -> Result<ResolvedCredentials, AiGatewayError> {
        let decrypted_key = self
            .provider_key_service
            .decrypt_api_key(&key_record.api_key_encrypted)?;
        let key_trust = KeyTrust::from_stored(key_record.trusted_private_address.as_deref());
        let self_hosted_capabilities = is_self_hosted(provider_id)
            .then(|| SelfHostedCapabilities::from_stored(key_record.capabilities.as_ref()));
        if is_self_hosted(provider_id) && key_record.base_url.is_none() {
            return Err(AiGatewayError::InvalidProviderUrl {
                reason: format!(
                    "self-hosted provider key {} has no base URL configured",
                    key_record.id
                ),
            });
        }
        let credentials = ResolvedCredentials {
            provider_id,
            api_key: decrypted_key,
            base_url: key_record.base_url,
            credential_type: CredentialType::System,
            system_key_id: Some(key_record.id),
            key_trust,
            self_hosted_capabilities,
        };
        credentials.check_base_url()?;

        debug!(
            provider = provider_id,
            provider_key_id = key_record.id,
            model = model,
            credential_type = "system",
            "Routing request to provider"
        );
        Ok(credentials)
    }

    /// Credentials of one administrator-configured key, for operating on an
    /// object the gateway already created with it (a batch input file, a
    /// batch). Fails closed when the key was deactivated or removed since.
    pub async fn system_key_credentials(
        &self,
        key_id: i32,
    ) -> Result<ResolvedCredentials, AiGatewayError> {
        let key = self.provider_key_service.get_by_id(key_id).await?;
        if !key.is_active {
            return Err(AiGatewayError::Validation {
                message: format!("AI provider key {key_id} is inactive"),
            });
        }
        let provider_id = self.provider_id(&key.provider)?;
        self.credentials_for_key(key, provider_id, "")
    }

    /// Default upstream base URL of a provider adapter, e.g.
    /// `https://api.openai.com/v1`.
    pub fn default_base_url(&self, provider_id: &str) -> Option<&'static str> {
        self.providers
            .get(provider_id)
            .map(|provider| provider.info().default_base_url)
    }

    /// Execute a chat completion request (non-streaming)
    pub async fn chat_completion(
        &self,
        request: &ChatCompletionRequest,
        byok: &ByokOverride,
    ) -> Result<(ChatCompletionResponse, ServedBy), AiGatewayError> {
        if request.model == "temps-cloud" {
            return self.cloud_chat_completion(request).await;
        }
        let (provider, credentials) = self.resolve_provider_and_key(&request.model, byok).await?;
        require_tools_capability(&credentials, request)?;

        let response = provider
            .chat_completion(
                &credentials.api_key,
                credentials.base_url.as_deref(),
                request,
            )
            .await
            .map_err(|error| credentials.caller_facing(error))?;

        Ok((response, ServedBy::from_credentials(&credentials)))
    }

    /// Execute a streaming chat completion request
    pub async fn chat_completion_stream(
        &self,
        request: &ChatCompletionRequest,
        byok: &ByokOverride,
    ) -> Result<
        (
            Pin<Box<dyn Stream<Item = Result<Bytes, AiGatewayError>> + Send>>,
            ServedBy,
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
        let (provider, credentials) = self.resolve_provider_and_key(&request.model, byok).await?;
        credentials.require_capability(&request.model, "streaming chat completions", |c| {
            c.streaming
        })?;
        require_tools_capability(&credentials, request)?;

        let stream = provider
            .chat_completion_stream(
                &credentials.api_key,
                credentials.base_url.as_deref(),
                request,
            )
            .await
            .map_err(|error| credentials.caller_facing(error))?;

        Ok((stream, ServedBy::from_credentials(&credentials)))
    }

    async fn cloud_chat_completion(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<(ChatCompletionResponse, ServedBy), AiGatewayError> {
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
        Ok((
            completion,
            ServedBy {
                provider_id: "temps-cloud",
                credential_type: CredentialType::System,
                system_key_id: None,
            },
        ))
    }

    /// Execute an embeddings request
    pub async fn embeddings(
        &self,
        request: &EmbeddingRequest,
        byok: &ByokOverride,
    ) -> Result<(EmbeddingResponse, ServedBy), AiGatewayError> {
        let (provider, credentials) = self.resolve_provider_and_key(&request.model, byok).await?;
        credentials.require_capability(&request.model, "embeddings", |c| c.embeddings)?;

        let response = provider
            .embeddings(
                &credentials.api_key,
                credentials.base_url.as_deref(),
                request,
            )
            .await
            .map_err(|error| credentials.caller_facing(error))?;

        Ok((response, ServedBy::from_credentials(&credentials)))
    }

    /// Send a minimal chat completion to verify a provider API key works.
    ///
    /// The base URL is fully validated first (with DNS resolution), so a
    /// blocked destination is reported as `InvalidProviderUrl` with the
    /// trusted-networks hint before anything is sent (#1410).
    ///
    /// Uses the configured model when supplied, otherwise the vendor's test
    /// default. Self-hosted providers have no such default: the model must be
    /// one the administrator picked from the server's own listing.
    pub async fn test_provider(
        &self,
        provider_id: &str,
        api_key: &str,
        base_url: Option<&str>,
        configured_model: Option<&str>,
        key_trust: Option<KeyTrust>,
    ) -> Result<(), AiGatewayError> {
        let provider = self.providers.get(provider_id).ok_or_else(|| {
            AiGatewayError::ProviderNotConfigured {
                provider: provider_id.to_string(),
            }
        })?;
        if let Some(base_url) = base_url {
            validate_provider_url(base_url, key_trust).await?;
        }

        let fallback_model = match provider_id {
            "openai" => Some("gpt-5-nano"),
            "anthropic" => Some("claude-haiku-4-5"),
            "xai" => Some("grok-4.5"),
            "gemini" => Some("gemini-3.5-flash-lite"),
            "openrouter" => Some("openai/gpt-4o-mini"),
            OPENAI_COMPATIBLE => {
                if base_url.is_none() {
                    return Err(AiGatewayError::Validation {
                        message: "A base URL is required for a self-hosted provider".to_string(),
                    });
                }
                None
            }
            _ => {
                return Err(AiGatewayError::ProviderNotConfigured {
                    provider: provider_id.to_string(),
                })
            }
        };
        let model = match (
            configured_model.map(str::trim).filter(|m| !m.is_empty()),
            fallback_model,
        ) {
            (Some(model), _) => model,
            (None, Some(fallback)) => fallback,
            (None, None) => {
                return Err(AiGatewayError::Validation {
                    message: "Choose a default model from the models the self-hosted server \
                              lists before verifying it"
                        .to_string(),
                })
            }
        };

        let request = ChatCompletionRequest {
            model: model.to_string(),
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

/// A self-hosted server's model listing and the default model chosen from it.
#[derive(Debug, Clone)]
pub struct SelfHostedListing {
    pub models: Vec<ModelInfo>,
    pub default_model: String,
}

/// The default model for a new self-hosted key must be one the server lists:
/// it is what the connection is verified with.
pub(crate) fn choose_self_hosted_default(
    models: &[ModelInfo],
    default_model: Option<&str>,
) -> Result<String, AiGatewayError> {
    if models.is_empty() {
        return Err(AiGatewayError::Validation {
            message: "The self-hosted server lists no models at /models; load a model on it and \
                      try again"
                .to_string(),
        });
    }
    let default_model = default_model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .ok_or_else(|| AiGatewayError::Validation {
            message: format!(
                "Choose a default model from the {} models the self-hosted server lists; it is \
                 used to verify the connection",
                models.len()
            ),
        })?;
    if !models.iter().any(|model| model.id == default_model) {
        return Err(AiGatewayError::Validation {
            message: format!(
                "Model '{default_model}' is not among the {} models the self-hosted server lists",
                models.len()
            ),
        });
    }
    Ok(default_model.to_string())
}

/// Refuse tool calling on a self-hosted key that did not declare it.
fn require_tools_capability(
    credentials: &ResolvedCredentials,
    request: &ChatCompletionRequest,
) -> Result<(), AiGatewayError> {
    if request
        .tools
        .as_ref()
        .is_some_and(|tools| !tools.is_empty())
    {
        credentials.require_capability(&request.model, "tool calling", |c| c.tools)?;
    }
    Ok(())
}

/// Turn a provider's model listing into catalog entries. `keep` decides which
/// IDs are kept (vendor adapters filter by name; self-hosted keeps all).
/// IDs that cannot be stored or routed (empty, over 255 bytes, control
/// characters) are skipped, and the result is capped at 5,000 models.
pub(crate) fn parse_model_catalog(
    provider_id: &str,
    body: &serde_json::Value,
    keep: impl Fn(&str) -> bool,
) -> Result<Vec<ModelInfo>, AiGatewayError> {
    let entries = if provider_id == "gemini" {
        body.get("models")
    } else {
        body.get("data")
    }
    .and_then(serde_json::Value::as_array)
    .ok_or_else(|| AiGatewayError::TranslationError {
        provider: provider_id.to_string(),
        reason: "Provider model catalog did not contain a model array".to_string(),
    })?;

    let mut models: Vec<ModelInfo> = entries
        .iter()
        .filter(|entry| {
            provider_id != "gemini"
                || entry
                    .get("supportedGenerationMethods")
                    .and_then(serde_json::Value::as_array)
                    .is_none_or(|methods| methods.iter().any(|method| method == "generateContent"))
        })
        .filter_map(|entry| {
            let raw_id = entry.get("id").or_else(|| entry.get("name"))?.as_str()?;
            // Gemini names models `models/<id>` but is called with `<id>`.
            // Every other provider -- above all a self-hosted server -- is
            // called with exactly the ID it lists, so that is kept verbatim.
            let id = if provider_id == "gemini" {
                raw_id.strip_prefix("models/").unwrap_or(raw_id)
            } else {
                raw_id
            };
            if id.trim().is_empty()
                || id.len() > 255
                || id.chars().any(char::is_control)
                || !keep(id)
            {
                return None;
            }
            let owned_by = entry
                .get("owned_by")
                .and_then(serde_json::Value::as_str)
                .filter(|owner| owner.len() <= 255 && !owner.chars().any(char::is_control))
                .unwrap_or(provider_id)
                .to_string();
            Some(ModelInfo {
                id: id.to_string(),
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
            instance_id: None,
            reconnected: false,
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
        assert_eq!(credential.credential_type, CredentialType::System);
        assert_eq!(credential.provider_id, "temps-cloud");
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

        assert_eq!(credential.credential_type, CredentialType::System);
        assert_eq!(credential.provider_id, "temps-cloud");
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
        let providers: Vec<&str> = vec!["openai", "anthropic", "xai", "gemini", "openrouter"];

        for provider_id in &providers {
            assert!(
                route_model_to_provider(match *provider_id {
                    "openai" => "gpt-4o",
                    "anthropic" => "claude-sonnet-4-6",
                    "xai" => "grok-3",
                    "gemini" => "gemini-3.1-pro",
                    "openrouter" => "openai/gpt-4o",
                    _ => unreachable!(),
                })
                .is_some(),
                "Model routing failed for provider: {}",
                provider_id
            );
        }
    }

    // -------------------------------------------------------------------------
    // #1408 — catalog-first routing
    // -------------------------------------------------------------------------

    fn key(
        id: i32,
        provider: &str,
        base_url: Option<&str>,
        active: bool,
    ) -> ai_provider_keys::Model {
        ai_provider_keys::Model {
            id,
            provider: provider.to_string(),
            display_name: format!("{provider} {id}"),
            api_key_encrypted: "enc".to_string(),
            base_url: base_url.map(str::to_string),
            default_model: None,
            is_active: active,
            trusted_private_address: None,
            trusted_private_address_set_by: None,
            trusted_private_address_set_at: None,
            capabilities: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn row(key_id: i32, model: &str, enabled: bool, available: bool) -> ai_provider_models::Model {
        ai_provider_models::Model {
            id: key_id * 100,
            provider_key_id: key_id,
            model_id: model.to_string(),
            display_name: model.to_string(),
            source: "discovered".to_string(),
            is_available: available,
            is_enabled: enabled,
            owned_by: None,
            metadata: None,
            last_seen_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn default_url(provider: &str) -> Option<&'static str> {
        match provider {
            "openai" => Some("https://api.openai.com/v1"),
            "openrouter" => Some("https://openrouter.ai/api/v1"),
            _ => None,
        }
    }

    const HOSTED: &str = "mlx-community/Some-Model-4bit";

    #[test]
    fn model_listed_only_by_self_hosted_key_routes_to_it_even_with_openrouter() {
        // OpenRouter's key exists but its catalog does not list the model; the
        // name heuristic would have picked OpenRouter for any `org/model` ID.
        assert_eq!(route_model_to_provider(HOSTED), Some("openrouter"));
        let entries = vec![(
            row(2, HOSTED, true, true),
            key(2, OPENAI_COMPATIBLE, Some("http://10.0.0.5:8000/v1"), true),
        )];
        assert_eq!(
            route_by_catalog(HOSTED, &entries, default_url),
            CatalogRoute::Key(2)
        );
    }

    #[test]
    fn model_served_by_two_destinations_is_ambiguous() {
        let entries = vec![
            (row(1, HOSTED, true, true), key(1, "openrouter", None, true)),
            (
                row(2, HOSTED, true, true),
                key(2, OPENAI_COMPATIBLE, Some("http://10.0.0.5:8000/v1"), true),
            ),
        ];
        assert_eq!(
            route_by_catalog(HOSTED, &entries, default_url),
            CatalogRoute::Ambiguous(vec![
                (1, "openrouter".to_string()),
                (2, OPENAI_COMPATIBLE.to_string())
            ])
        );
        // Two self-hosted servers listing the same ID are just as ambiguous.
        let entries = vec![
            (
                row(3, HOSTED, true, true),
                key(3, OPENAI_COMPATIBLE, Some("http://10.0.0.5:8000/v1"), true),
            ),
            (
                row(4, HOSTED, true, true),
                key(4, OPENAI_COMPATIBLE, Some("http://10.0.0.6:8000/v1"), true),
            ),
        ];
        assert!(matches!(
            route_by_catalog(HOSTED, &entries, default_url),
            CatalogRoute::Ambiguous(_)
        ));
    }

    #[test]
    fn keys_reaching_the_same_upstream_are_interchangeable() {
        // Two OpenAI keys on the default endpoint: a credential choice, not a
        // routing one. The lowest key id wins, deterministically.
        let entries = vec![
            (row(7, "gpt-4o", true, true), key(7, "openai", None, true)),
            (
                row(5, "gpt-4o", true, true),
                key(5, "openai", Some("https://api.openai.com/v1/"), true),
            ),
        ];
        assert_eq!(
            route_by_catalog("gpt-4o", &entries, default_url),
            CatalogRoute::Key(5)
        );
    }

    #[test]
    fn destinations_differing_only_in_path_case_are_ambiguous() {
        let entries = vec![
            (
                row(1, HOSTED, true, true),
                key(
                    1,
                    OPENAI_COMPATIBLE,
                    Some("https://models.example.test/Private/v1"),
                    true,
                ),
            ),
            (
                row(2, HOSTED, true, true),
                key(
                    2,
                    OPENAI_COMPATIBLE,
                    Some("https://models.example.test/private/v1"),
                    true,
                ),
            ),
        ];
        assert!(matches!(
            route_by_catalog(HOSTED, &entries, default_url),
            CatalogRoute::Ambiguous(_)
        ));
    }

    #[test]
    fn destination_identity_normalises_only_case_insensitive_parts() {
        assert_eq!(
            destination_identity("HTTPS://Models.Example.TEST:443/v1/"),
            destination_identity("https://models.example.test/v1")
        );
        assert_ne!(
            destination_identity("https://models.example.test/V1"),
            destination_identity("https://models.example.test/v1")
        );
        assert_ne!(
            destination_identity("http://10.0.0.5:8000/v1"),
            destination_identity("http://10.0.0.5:8001/v1")
        );
        assert_eq!(destination_identity(" not a url "), "not a url");
    }

    #[test]
    fn unavailable_self_hosted_model_never_falls_back_to_the_name_heuristic() {
        for (entry, expected_reason) in [
            (
                (
                    row(2, HOSTED, false, true),
                    key(2, OPENAI_COMPATIBLE, Some("http://10.0.0.5/v1"), true),
                ),
                "disabled",
            ),
            (
                (
                    row(2, HOSTED, true, false),
                    key(2, OPENAI_COMPATIBLE, Some("http://10.0.0.5/v1"), true),
                ),
                "no longer lists",
            ),
            (
                (
                    row(2, HOSTED, true, true),
                    key(2, OPENAI_COMPATIBLE, Some("http://10.0.0.5/v1"), false),
                ),
                "inactive",
            ),
        ] {
            match route_by_catalog(HOSTED, &[entry], default_url) {
                CatalogRoute::SelfHostedUnavailable { key_id, reason } => {
                    assert_eq!(key_id, 2);
                    assert!(reason.contains(expected_reason), "{reason}");
                }
                other => panic!("expected SelfHostedUnavailable, got {other:?}"),
            }
        }
    }

    #[test]
    fn unavailable_self_hosted_model_blocks_a_vendor_listing_the_same_id() {
        // LM Studio-style IDs can equal OpenRouter's exactly. Once the
        // self-hosted key cannot serve the model, the vendor that also lists
        // it must not silently take over.
        let shared = "qwen/qwen3-8b";
        for self_hosted in [
            (
                row(2, shared, true, true),
                key(2, OPENAI_COMPATIBLE, Some("http://10.0.0.5/v1"), false),
            ),
            (
                row(2, shared, true, false),
                key(2, OPENAI_COMPATIBLE, Some("http://10.0.0.5/v1"), true),
            ),
            (
                row(2, shared, false, true),
                key(2, OPENAI_COMPATIBLE, Some("http://10.0.0.5/v1"), true),
            ),
        ] {
            let entries = vec![
                (row(1, shared, true, true), key(1, "openrouter", None, true)),
                self_hosted,
            ];
            assert!(
                matches!(
                    route_by_catalog(shared, &entries, default_url),
                    CatalogRoute::SelfHostedUnavailable { key_id: 2, .. }
                ),
                "{entries:?}"
            );
        }
        // Two self-hosted keys on the same server stay interchangeable even
        // when one of them is inactive.
        let entries = vec![
            (
                row(3, shared, true, true),
                key(3, OPENAI_COMPATIBLE, Some("http://10.0.0.5/v1"), true),
            ),
            (
                row(4, shared, true, true),
                key(4, OPENAI_COMPATIBLE, Some("http://10.0.0.5/v1/"), false),
            ),
        ];
        assert_eq!(
            route_by_catalog(shared, &entries, default_url),
            CatalogRoute::Key(3)
        );
    }

    #[test]
    fn self_hosted_case_variant_blocks_the_name_heuristic() {
        // The server lists `Org/Model-8B`; a client asks for `org/model-8b`.
        // The heuristic would say OpenRouter -- the request must be refused.
        let entries = vec![(
            row(2, "Org/Model-8B", true, true),
            key(2, OPENAI_COMPATIBLE, Some("http://10.0.0.5/v1"), true),
        )];
        match route_by_catalog("org/model-8b", &entries, default_url) {
            CatalogRoute::SelfHostedUnavailable { key_id, reason } => {
                assert_eq!(key_id, 2);
                assert!(reason.contains("Org/Model-8B"), "{reason}");
            }
            other => panic!("expected SelfHostedUnavailable, got {other:?}"),
        }
        // Exact match still routes normally.
        assert_eq!(
            route_by_catalog("Org/Model-8B", &entries, default_url),
            CatalogRoute::Key(2)
        );
    }

    #[test]
    fn vendor_rows_that_cannot_serve_fall_through_to_the_heuristic() {
        // Only vendor rows, none serving: the existing fallback (which then
        // applies that key's own catalog rules) keeps working.
        let entries = vec![(row(1, "gpt-4o", false, true), key(1, "openai", None, true))];
        assert_eq!(
            route_by_catalog("gpt-4o", &entries, default_url),
            CatalogRoute::NotInCatalog
        );
        assert_eq!(
            route_by_catalog(HOSTED, &[], default_url),
            CatalogRoute::NotInCatalog
        );
    }

    #[test]
    fn builtin_name_heuristic_is_unchanged() {
        assert_eq!(route_model_to_provider("gpt-4o"), Some("openai"));
        assert_eq!(
            route_model_to_provider("claude-haiku-4-5"),
            Some("anthropic")
        );
        assert_eq!(
            route_model_to_provider("openai/gpt-4o-mini"),
            Some("openrouter")
        );
        assert_eq!(route_model_to_provider("llama3.2:3b"), None);
    }

    // -------------------------------------------------------------------------
    // #1409 — discovery keeps arbitrary self-hosted model IDs
    // -------------------------------------------------------------------------

    #[test]
    fn self_hosted_discovery_keeps_every_listed_model_id() {
        let long_id = "x".repeat(256);
        let body = serde_json::json!({"object": "list", "data": [
            {"id": "mlx-community/Qwen3.5-27B-8bit", "owned_by": "mlx"},
            {"id": "llama3.2:3b"},
            {"id": "BAAI/bge-small-en-v1.5"},
            {"id": "mlx-community/Qwen3.5-27B-8bit"},
            {"id": "bad\u{7}id"},
            {"id": long_id},
            {"id": ""},
            {"object": "model"}
        ]});
        let models = parse_model_catalog(OPENAI_COMPATIBLE, &body, |_| true).unwrap();
        let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "BAAI/bge-small-en-v1.5",
                "llama3.2:3b",
                "mlx-community/Qwen3.5-27B-8bit"
            ]
        );
        assert_eq!(models[2].owned_by, "mlx");
        assert_eq!(models[1].owned_by, OPENAI_COMPATIBLE);
    }

    #[test]
    fn self_hosted_discovery_keeps_a_models_prefix_but_gemini_strips_it() {
        let body = serde_json::json!({"data": [{"id": "models/my-model"}]});
        let models = parse_model_catalog(OPENAI_COMPATIBLE, &body, |_| true).unwrap();
        assert_eq!(models[0].id, "models/my-model");
        let body = serde_json::json!({"models": [{"name": "models/gemini-3.1-pro"}]});
        let models = parse_model_catalog("gemini", &body, |_| true).unwrap();
        assert_eq!(models[0].id, "gemini-3.1-pro");
    }

    #[test]
    fn self_hosted_default_must_come_from_the_listing() {
        let listed = |ids: &[&str]| {
            ids.iter()
                .map(|id| ModelInfo {
                    id: id.to_string(),
                    object: "model".to_string(),
                    owned_by: OPENAI_COMPATIBLE.to_string(),
                })
                .collect::<Vec<_>>()
        };
        let reason = |result: Result<String, AiGatewayError>| match result {
            Err(AiGatewayError::Validation { message }) => message,
            other => panic!("expected Validation, got {other:?}"),
        };
        assert!(reason(choose_self_hosted_default(&[], Some(HOSTED))).contains("lists no models"));
        let models = listed(&[HOSTED, "mlx-community/embed-small"]);
        assert!(
            reason(choose_self_hosted_default(&models, None)).contains("Choose a default model")
        );
        assert!(
            reason(choose_self_hosted_default(&models, Some("  "))).contains("Choose a default")
        );
        assert!(
            reason(choose_self_hosted_default(&models, Some("gpt-5-nano"))).contains("not among")
        );
        assert_eq!(
            choose_self_hosted_default(&models, Some(&format!(" {HOSTED} "))).unwrap(),
            HOSTED
        );
    }

    #[test]
    fn vendor_discovery_still_filters_by_the_adapter() {
        let body = serde_json::json!({"data": [{"id": "gpt-4o"}, {"id": "mlx-community/x"}]});
        let openai = OpenAiCompatProvider::openai();
        let models = parse_model_catalog("openai", &body, |id| openai.supports_model(id)).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-4o");
        assert!(matches!(
            parse_model_catalog("openai", &serde_json::json!({"models": []}), |_| true),
            Err(AiGatewayError::TranslationError { .. })
        ));
    }

    #[test]
    fn blocked_system_key_urls_are_generic_for_callers_but_byok_keeps_details() {
        let blocked = || AiGatewayError::InvalidProviderUrl {
            reason: "provider hostname 'models.internal' resolved to a private address".into(),
        };
        let mut credentials = ResolvedCredentials {
            provider_id: OPENAI_COMPATIBLE,
            api_key: String::new(),
            base_url: Some("http://models.internal:8000/v1".to_string()),
            credential_type: CredentialType::System,
            system_key_id: Some(4),
            key_trust: None,
            self_hosted_capabilities: None,
        };
        match credentials.caller_facing(blocked()) {
            AiGatewayError::InvalidProviderUrl { reason } => {
                assert!(!reason.contains("models.internal"), "{reason}");
                assert!(reason.contains("provider key 4"), "{reason}");
            }
            other => panic!("expected InvalidProviderUrl, got {other:?}"),
        }
        credentials.credential_type = CredentialType::Byok;
        credentials.system_key_id = None;
        match credentials.caller_facing(blocked()) {
            AiGatewayError::InvalidProviderUrl { reason } => {
                assert!(reason.contains("models.internal"), "{reason}")
            }
            other => panic!("expected InvalidProviderUrl, got {other:?}"),
        }
        // Other errors pass through untouched.
        assert!(matches!(
            credentials.caller_facing(AiGatewayError::UploadCapacity),
            AiGatewayError::UploadCapacity
        ));
    }

    #[test]
    fn undeclared_capability_is_refused_only_for_self_hosted_keys() {
        let mut credentials = ResolvedCredentials {
            provider_id: OPENAI_COMPATIBLE,
            api_key: String::new(),
            base_url: Some("http://10.0.0.5/v1".to_string()),
            credential_type: CredentialType::System,
            system_key_id: Some(9),
            key_trust: None,
            self_hosted_capabilities: Some(SelfHostedCapabilities::default()),
        };
        assert!(matches!(
            credentials.require_capability("m", "embeddings", |c| c.embeddings),
            Err(AiGatewayError::CapabilityNotDeclared { key_id: 9, .. })
        ));
        assert!(credentials
            .require_capability("m", "streaming", |c| c.streaming)
            .is_ok());
        credentials.self_hosted_capabilities = None;
        assert!(credentials
            .require_capability("m", "embeddings", |c| c.embeddings)
            .is_ok());
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
