// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::permission_guard;
use temps_auth::permissions::Permission;
use temps_auth::{AuthContext, RequireAuth};
use temps_core::problemdetails;
use temps_core::problemdetails::{Problem, ProblemDetails};
use temps_core::secret_string::SecretString;
use temps_core::RequestMetadata;
use utoipa::{OpenApi, ToSchema};

use crate::error::AiGatewayError;
use crate::handlers::provider_audit::{
    audit_provider_key, audit_provider_model, PROVIDER_KEY_CREATED, PROVIDER_KEY_DELETED,
    PROVIDER_KEY_UPDATED, PROVIDER_MODELS_REFRESHED, PROVIDER_MODEL_ADDED, PROVIDER_MODEL_DELETED,
    PROVIDER_MODEL_UPDATED,
};
use crate::handlers::types::AiGatewayAppState;
use crate::provider_url::KeyTrust;
use crate::self_hosted::{is_self_hosted, SelfHostedCapabilities};
use crate::services::{NewProviderKey, ProviderKeyUpdate, TrustChange, TrustRequest};
use crate::types::DiscoveredModel;

// ============================================================================
// Error conversion
// ============================================================================

impl From<AiGatewayError> for Problem {
    fn from(error: AiGatewayError) -> Self {
        match error {
            AiGatewayError::ProviderKeyNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Provider Key Not Found")
                    .with_detail(error.to_string())
            }
            AiGatewayError::Validation { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Validation Error")
                .with_detail(error.to_string()),
            AiGatewayError::Database(_) | AiGatewayError::Encryption(_) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
            AiGatewayError::UpstreamTimeout { .. } => {
                problemdetails::new(StatusCode::GATEWAY_TIMEOUT)
                    .with_title("AI Provider Timed Out")
                    .with_detail(error.to_string())
            }
            AiGatewayError::HttpClient(_) => problemdetails::new(StatusCode::BAD_GATEWAY)
                .with_title("AI Provider Unavailable")
                .with_detail(error.to_string()),
            AiGatewayError::UpstreamMalformedResponse { ref provider, .. } => {
                problemdetails::new(StatusCode::BAD_GATEWAY)
                    .with_title("AI Provider Returned An Unreadable Response")
                    .with_detail(format!(
                        "Provider '{provider}' returned a response Temps could not read"
                    ))
            }
            AiGatewayError::ProviderNotConfigured { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Provider Not Configured")
                    .with_detail(error.to_string())
            }
            AiGatewayError::ModelNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Model Not Found")
                .with_detail(error.to_string()),
            AiGatewayError::ModelNotAllowed { .. } => problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Model Not Allowed")
                .with_detail(error.to_string()),
            AiGatewayError::UpstreamError {
                ref model,
                status,
                ref message,
            } => {
                let http_status =
                    StatusCode::from_u16(crate::error::upstream_failure_status(status))
                        .unwrap_or(StatusCode::BAD_GATEWAY);
                problemdetails::new(http_status)
                    .with_title("Upstream Provider Error")
                    .with_detail(crate::error::upstream_failure_message(
                        model, status, message,
                    ))
            }
            AiGatewayError::TranslationError { .. } => problemdetails::new(StatusCode::BAD_GATEWAY)
                .with_title("Translation Error")
                .with_detail(error.to_string()),
            AiGatewayError::StreamError { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Stream Error")
                    .with_detail(error.to_string())
            }
            AiGatewayError::Internal { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Error")
                    .with_detail(error.to_string())
            }
            AiGatewayError::InvalidProviderUrl { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Provider URL")
                    .with_detail(error.to_string())
            }
            AiGatewayError::UnsupportedEndpoint { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Endpoint Not Supported For Model")
                    .with_detail(error.to_string())
            }
            AiGatewayError::UploadTooLarge { .. } => {
                problemdetails::new(StatusCode::PAYLOAD_TOO_LARGE)
                    .with_title("Batch input file too large")
                    .with_detail(error.to_string())
            }
            AiGatewayError::UploadCapacity => problemdetails::new(StatusCode::TOO_MANY_REQUESTS)
                .with_title("Batch upload capacity reached")
                .with_detail(error.to_string()),
            AiGatewayError::ObjectNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Object Not Found")
                .with_detail(error.to_string()),
            AiGatewayError::AmbiguousModel { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Ambiguous Model")
                .with_detail(error.to_string()),
            AiGatewayError::SelfHostedModelUnavailable { .. } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("Self-hosted Model Unavailable")
                    .with_detail(error.to_string())
            }
            AiGatewayError::CapabilityNotDeclared { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Capability Not Declared")
                    .with_detail(error.to_string())
            }
            AiGatewayError::RateLimited { .. } => {
                problemdetails::new(StatusCode::TOO_MANY_REQUESTS)
                    .with_title("AI Gateway Rate Limit Exceeded")
                    .with_detail(error.to_string())
            }
            AiGatewayError::BudgetExceeded { .. } => {
                problemdetails::new(StatusCode::TOO_MANY_REQUESTS)
                    .with_title("AI Gateway Budget Exceeded")
                    .with_detail(error.to_string())
            }
            AiGatewayError::ProjectNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Project Not Found")
                .with_detail(error.to_string()),
        }
    }
}

// ============================================================================
// OpenAPI schema
// ============================================================================

#[derive(OpenApi)]
#[openapi(
    paths(
        list_provider_keys,
        get_provider_key,
        create_provider_key,
        update_provider_key,
        delete_provider_key,
        test_provider_key_inline,
        test_provider_key_by_id,
        discover_provider_models,
        refresh_provider_models,
        add_provider_model,
        update_provider_model,
        delete_provider_model,
    ),
    components(schemas(
        ProviderKeyResponse,
        CreateProviderKeyRequest,
        UpdateProviderKeyRequest,
        TestProviderKeyRequest,
        TestProviderKeyResponse,
        ProviderDetailResponse,
        ProviderModelResponse,
        AddProviderModelRequest,
        UpdateProviderModelRequest,
        DiscoverProviderModelsRequest,
        DiscoverProviderModelsResponse,
        DiscoveredModelResponse,
        SelfHostedCapabilities,
    )),
    info(
        title = "AI Gateway Admin API",
        description = "Manage AI provider API keys and gateway configuration",
        version = "1.0.0"
    ),
    tags(
        (name = "AI Gateway Admin", description = "Provider key management endpoints")
    )
)]
pub struct AiGatewayAdminApiDoc;

pub fn configure_admin_routes() -> Router<Arc<AiGatewayAppState>> {
    Router::new()
        .route("/ai/providers", get(list_provider_keys))
        .route("/ai/providers", post(create_provider_key))
        .route("/ai/providers/{id}", get(get_provider_key))
        .route("/ai/providers/{id}", patch(update_provider_key))
        .route("/ai/providers/{id}", delete(delete_provider_key))
        .route("/ai/providers/test", post(test_provider_key_inline))
        .route("/ai/providers/discover", post(discover_provider_models))
        .route("/ai/providers/{id}/test", post(test_provider_key_by_id))
        .route(
            "/ai/providers/{id}/models/refresh",
            post(refresh_provider_models),
        )
        .route("/ai/providers/{id}/models", post(add_provider_model))
        .route(
            "/ai/providers/{id}/models/{model_row_id}",
            patch(update_provider_model).delete(delete_provider_model),
        )
}

// ============================================================================
// DTOs
// ============================================================================

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProviderKeyResponse {
    pub id: i32,
    pub provider: String,
    pub display_name: String,
    /// Masked API key (only last 4 chars visible)
    pub api_key_masked: String,
    pub base_url: Option<String>,
    /// Model id this provider serves (NULL → per-provider default).
    pub default_model: Option<String>,
    pub is_active: bool,
    /// Exact private `ip:port` this key may reach without that address being
    /// in the instance-wide trusted private networks. `None` when not set.
    pub trusted_private_address: Option<String>,
    /// When the per-key trust was set (ISO 8601).
    pub trusted_private_address_set_at: Option<String>,
    /// User who set the per-key trust.
    pub trusted_private_address_set_by: Option<i32>,
    /// Declared capabilities of a self-hosted key; `None` for vendor keys.
    pub capabilities: Option<SelfHostedCapabilities>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProviderModelResponse {
    pub id: i32,
    pub provider_key_id: i32,
    pub model_id: String,
    pub display_name: String,
    pub source: String,
    pub is_available: bool,
    pub is_enabled: bool,
    pub owned_by: Option<String>,
    /// What the provider's catalogue says about the model: name, context
    /// window, input/output modalities, prices and tool support. Absent for
    /// manual models and for providers whose catalogue lists only ids.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<crate::types::ModelDetails>,
    pub last_seen_at: Option<String>,
    pub updated_at: String,
}

impl From<temps_entities::ai_provider_models::Model> for ProviderModelResponse {
    fn from(model: temps_entities::ai_provider_models::Model) -> Self {
        Self {
            id: model.id,
            provider_key_id: model.provider_key_id,
            model_id: model.model_id,
            display_name: model.display_name,
            source: model.source,
            is_available: model.is_available,
            is_enabled: model.is_enabled,
            owned_by: model.owned_by,
            details: model
                .metadata
                .and_then(|metadata| serde_json::from_value(metadata).ok()),
            last_seen_at: model.last_seen_at.map(|value| value.to_rfc3339()),
            updated_at: model.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProviderDetailResponse {
    pub key: ProviderKeyResponse,
    pub models: Vec<ProviderModelResponse>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AddProviderModelRequest {
    pub model_id: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateProviderModelRequest {
    pub is_enabled: bool,
}

impl From<temps_entities::ai_provider_keys::Model> for ProviderKeyResponse {
    fn from(model: temps_entities::ai_provider_keys::Model) -> Self {
        let capabilities = is_self_hosted(&model.provider)
            .then(|| SelfHostedCapabilities::from_stored(model.capabilities.as_ref()));
        Self {
            id: model.id,
            provider: model.provider,
            display_name: model.display_name,
            api_key_masked: "***".to_string(), // Never expose encrypted key
            base_url: model.base_url,
            default_model: model.default_model,
            is_active: model.is_active,
            trusted_private_address: model.trusted_private_address,
            trusted_private_address_set_at: model
                .trusted_private_address_set_at
                .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
            trusted_private_address_set_by: model.trusted_private_address_set_by,
            capabilities,
            created_at: model.created_at.to_rfc3339(),
            updated_at: model.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateProviderKeyRequest {
    /// "openai", "anthropic", "xai", "gemini", "openrouter", or
    /// "openai_compatible" for a self-hosted OpenAI-compatible server.
    pub provider: String,
    pub display_name: String,
    /// Provider API key. Optional only for self-hosted providers that run
    /// without authentication.
    #[serde(default)]
    pub api_key: SecretString,
    /// Required for "openai_compatible", e.g. "http://10.0.0.5:8000/v1".
    pub base_url: Option<String>,
    /// Model id to pin for this provider (e.g. "gpt-4o-mini"). Required for
    /// "openai_compatible": it must be one of the models the server lists
    /// (see `POST /ai/providers/discover`), and is used to verify the key.
    pub default_model: Option<String>,
    /// Trust the exact private `ip:port` of `base_url` for this key only,
    /// without adding it to the instance-wide trusted private networks.
    /// Requires the `settings:write` permission; the base URL must use an IP
    /// address. Cloud metadata and link-local addresses can never be trusted.
    #[serde(default)]
    pub trust_private_address: bool,
    /// Declared capabilities of a self-hosted key (defaults: streaming and
    /// tools on, embeddings and Responses API off).
    pub capabilities: Option<SelfHostedCapabilities>,
}

/// Deserialize a PATCH field that distinguishes three states:
/// absent (`None`), present-null (`Some(None)` → clear), and present-value
/// (`Some(Some(v))` → set). Plain `Option<Option<T>>` cannot do this because
/// serde collapses an explicit JSON `null` to the outer `None`, making "clear
/// this field" indistinguishable from "field omitted". Pair with
/// `#[serde(default, deserialize_with = "deserialize_optional_field")]`.
fn deserialize_optional_field<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    // serde only invokes this when the key is present, so wrap whatever we parse
    // — including an explicit `null` — in an outer `Some`.
    Ok(Some(Option::<T>::deserialize(deserializer)?))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateProviderKeyRequest {
    pub display_name: Option<String>,
    /// New API key. An empty string removes it, for self-hosted keys only.
    pub api_key: Option<SecretString>,
    /// Double-Option: absent = leave unchanged, present-null = clear (revert to
    /// the provider's default endpoint), present-value = set. Moving the base
    /// URL to another address drops the per-key private-address trust.
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    pub base_url: Option<Option<String>>,
    /// Double-Option: absent = leave unchanged, present-null = clear the pinned
    /// model (revert to the per-provider default), present-value = set.
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    pub default_model: Option<Option<String>>,
    pub is_active: Option<bool>,
    /// `true` trusts the exact private `ip:port` of the (new) base URL for
    /// this key only (requires `settings:write`); `false` removes the trust.
    pub trust_private_address: Option<bool>,
    /// Declared capabilities; self-hosted keys only.
    pub capabilities: Option<SelfHostedCapabilities>,
}

/// The endpoint and model a replacement key is checked against: those the same
/// request sets, otherwise the ones already on the key. A field the request
/// clears is checked as cleared, which is how the key will then be used.
fn replacement_check_target<'a>(
    current: &'a temps_entities::ai_provider_keys::Model,
    request: &'a UpdateProviderKeyRequest,
) -> (Option<&'a str>, Option<&'a str>) {
    let base_url = match request.base_url.as_ref() {
        Some(requested) => requested.as_deref(),
        None => current.base_url.as_deref(),
    };
    let default_model = match request.default_model.as_ref() {
        Some(requested) => requested.as_deref(),
        None => current.default_model.as_deref(),
    };
    (base_url, default_model)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct TestProviderKeyRequest {
    /// Provider ID: "openai", "anthropic", "xai", "gemini", "openrouter",
    /// "openai_compatible"
    pub provider: String,
    /// The raw API key to test (may be empty for a self-hosted provider)
    #[serde(default)]
    pub api_key: SecretString,
    /// Optional custom base URL (required for "openai_compatible")
    pub base_url: Option<String>,
    /// Model to send the test completion to. Required for
    /// "openai_compatible"; defaults to a small vendor model otherwise.
    pub default_model: Option<String>,
    /// Test as if the base URL's exact private address were trusted on the
    /// key. Requires `settings:write`.
    #[serde(default)]
    pub trust_private_address: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TestProviderKeyResponse {
    pub success: bool,
    pub provider: String,
    /// Error message if the test failed
    pub error: Option<String>,
    /// Response time in milliseconds
    pub latency_ms: u64,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct DiscoverProviderModelsRequest {
    /// Provider ID, e.g. "openai_compatible"
    pub provider: String,
    /// API key, if the server requires one
    #[serde(default)]
    pub api_key: SecretString,
    /// Base URL of the server (required for "openai_compatible")
    pub base_url: Option<String>,
    /// Discover as if the base URL's exact private address were trusted on
    /// the key. Requires `settings:write`.
    #[serde(default)]
    pub trust_private_address: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DiscoveredModelResponse {
    pub id: String,
    pub owned_by: String,
    /// The model's human-readable name, when the server's listing gives one.
    pub name: Option<String>,
}

impl From<DiscoveredModel> for DiscoveredModelResponse {
    fn from(model: DiscoveredModel) -> Self {
        Self {
            id: model.info.id,
            owned_by: model.info.owned_by,
            name: model.details.and_then(|details| details.name),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DiscoverProviderModelsResponse {
    pub provider: String,
    /// Models the provider lists, sorted by id
    pub models: Vec<DiscoveredModelResponse>,
}

/// Per-key private-address trust lifts the SSRF guard for one address, so it
/// needs the permission that governs the instance-wide trusted networks too.
fn require_trust_permission(auth: &AuthContext) -> Result<(), Problem> {
    if auth.has_permission(&Permission::SettingsWrite) {
        return Ok(());
    }
    Err(problemdetails::new(StatusCode::FORBIDDEN)
        .with_title("Insufficient Permissions")
        .with_detail(format!(
            "Trusting a private address on an AI provider key requires the {} permission \
             (an administrator), because it lifts the outbound SSRF protection for that address",
            Permission::SettingsWrite
        )))
}

/// The per-key trust a request asks for, checking permission and deriving the
/// exact address from the request's own base URL.
fn requested_key_trust(
    auth: &AuthContext,
    trust_private_address: bool,
    base_url: Option<&str>,
) -> Result<Option<KeyTrust>, Problem> {
    if !trust_private_address {
        return Ok(None);
    }
    require_trust_permission(auth)?;
    let base_url = base_url.ok_or_else(|| AiGatewayError::Validation {
        message: "Trusting a private address requires a base URL".to_string(),
    })?;
    Ok(Some(KeyTrust::for_base_url(base_url)?))
}

/// Errors from verifying a new key. A blocked base URL is reported as is,
/// with its hint, so the form explains what to change (#1410); everything
/// else reads as a failed verification.
///
/// A vendor provider (OpenAI, Anthropic, ...) given a custom base URL is
/// verified with that vendor's own model names, so pointing it at another
/// vendor's OpenAI-compatible API (DeepSeek, Groq, ...) fails with an
/// unknown-model error. `custom_vendor_base_url` carries the provider id in
/// that case so the message says which provider to use instead. The hint is
/// only added when the server rejected the request itself (400, 404, 422),
/// which is how an unknown model comes back; a rejected key (401/403), a
/// server failure or an unreachable host has another fix.
fn verification_error(
    error: AiGatewayError,
    custom_vendor_base_url: Option<&str>,
) -> AiGatewayError {
    match error {
        AiGatewayError::InvalidProviderUrl { .. } => error,
        other => {
            let model_rejected = matches!(
                other,
                AiGatewayError::UpstreamError {
                    status: 400 | 404 | 422,
                    ..
                }
            );
            let mut message = format!("API key verification failed: {}", other.summary());
            if let Some(provider) = custom_vendor_base_url.filter(|_| model_rejected) {
                message.push_str(&format!(
                    ". The custom base URL on a '{provider}' key must serve {provider}'s own \
                     models; for any other API that speaks the OpenAI protocol (DeepSeek, \
                     Groq, Together, vLLM, ...) add an OpenAI-compatible provider instead, \
                     which lists the server's models at /models"
                ));
            }
            AiGatewayError::Validation { message }
        }
    }
}

// ============================================================================
// Handlers
// ============================================================================

#[utoipa::path(
    tag = "AI Gateway Admin",
    get,
    path = "/ai/providers",
    responses(
        (status = 200, description = "List of provider keys", body = Vec<ProviderKeyResponse>),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn list_provider_keys(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayRead);

    let keys = app_state.provider_key_service.list().await?;
    let responses: Vec<ProviderKeyResponse> = keys.into_iter().map(Into::into).collect();

    Ok(Json(responses))
}

#[utoipa::path(
    tag = "AI Gateway Admin",
    get,
    path = "/ai/providers/{id}",
    params(("id" = i32, Path, description = "Provider key ID")),
    responses((status = 200, body = ProviderDetailResponse), (status = 404, body = ProblemDetails)),
    security(("bearer_auth" = []))
)]
async fn get_provider_key(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayRead);
    let key = app_state.provider_key_service.get_by_id(id).await?;
    let models = app_state.provider_model_service.list_for_key(id).await?;
    Ok(Json(ProviderDetailResponse {
        key: key.into(),
        models: models.into_iter().map(Into::into).collect(),
    }))
}

#[utoipa::path(
    tag = "AI Gateway Admin",
    post,
    path = "/ai/providers",
    request_body = CreateProviderKeyRequest,
    responses(
        (status = 201, description = "Provider key created", body = ProviderKeyResponse),
        (status = 400, description = "Validation error", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn create_provider_key(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateProviderKeyRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayWrite);

    let base_url = request
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty());
    let key_trust = requested_key_trust(&auth, request.trust_private_address, base_url)?;
    let self_hosted = is_self_hosted(&request.provider);
    if self_hosted && base_url.is_none() {
        return Err(AiGatewayError::Validation {
            message: "A base URL is required for an OpenAI-compatible provider, \
                      e.g. https://api.deepseek.com/v1 or http://10.0.0.5:8000/v1"
                .to_string(),
        }
        .into());
    }

    // A self-hosted server has no vendor default model: list its models
    // first and verify with the one the administrator picked from them.
    let (discovered, default_model) = if self_hosted {
        let listing = app_state
            .gateway_service
            .discover_self_hosted_with_default(
                request.api_key.expose(),
                base_url,
                request.default_model.as_deref(),
                key_trust,
            )
            .await?;
        (Some(listing.models), Some(listing.default_model))
    } else {
        (None, request.default_model.clone())
    };

    // Verify the key works before persisting it. Fails fast with a 400 so
    // the UI doesn't end up with a stored-but-broken credential. The base
    // URL is validated (with DNS) before anything is sent.
    app_state
        .gateway_service
        .test_provider(
            &request.provider,
            request.api_key.expose(),
            base_url,
            default_model.as_deref(),
            key_trust,
        )
        .await
        .map_err(|error| {
            verification_error(
                error,
                base_url
                    .filter(|_| !self_hosted)
                    .map(|_| request.provider.as_str()),
            )
        })?;

    let mut key = app_state
        .provider_key_service
        .create(NewProviderKey {
            provider: &request.provider,
            display_name: &request.display_name,
            api_key: request.api_key.expose(),
            base_url,
            default_model: None,
            trust: key_trust.map(|_| TrustRequest {
                set_by: auth.user_id_opt(),
            }),
            capabilities: request.capabilities,
        })
        .await?;
    if let Some(models) = discovered {
        if let Err(error) = app_state
            .provider_model_service
            .apply_discovered(key.id, models)
            .await
        {
            app_state.provider_key_service.delete(key.id).await?;
            return Err(error.into());
        }
    } else {
        app_state
            .provider_model_service
            .seed_bootstrap(&key)
            .await?;
        if let Err(error) = app_state.provider_model_service.refresh(key.id).await {
            tracing::warn!(provider_key_id = key.id, error = %error, "Initial provider model refresh failed; bootstrap catalog remains available");
        }
    }
    if let Some(default_model) = default_model.as_deref() {
        if let Err(error) = app_state
            .provider_model_service
            .ensure_selectable(key.id, default_model)
            .await
        {
            // Roll back the just-created key; model rows cascade with it.
            app_state.provider_key_service.delete(key.id).await?;
            return Err(error.into());
        }
        key = app_state
            .provider_key_service
            .update(
                key.id,
                ProviderKeyUpdate {
                    default_model: Some(Some(default_model)),
                    ..Default::default()
                },
            )
            .await?;
    }
    app_state.provider_status_cache.invalidate().await;

    let mut changed_fields = vec!["provider", "display_name", "api_key"];
    if key.base_url.is_some() {
        changed_fields.push("base_url");
    }
    if key.default_model.is_some() {
        changed_fields.push("default_model");
    }
    if key.trusted_private_address.is_some() {
        changed_fields.push("trusted_private_address");
    }
    if key.capabilities.is_some() {
        changed_fields.push("capabilities");
    }
    audit_provider_key(
        &app_state,
        &auth,
        &metadata,
        PROVIDER_KEY_CREATED,
        &key,
        None,
        changed_fields,
    )
    .await;

    Ok((StatusCode::CREATED, Json(ProviderKeyResponse::from(key))))
}

#[utoipa::path(
    tag = "AI Gateway Admin",
    post,
    path = "/ai/providers/discover",
    request_body = DiscoverProviderModelsRequest,
    responses(
        (status = 200, description = "Models the provider lists", body = DiscoverProviderModelsResponse),
        (status = 400, description = "Invalid or blocked base URL, or the provider could not be listed", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn discover_provider_models(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Json(request): Json<DiscoverProviderModelsRequest>,
) -> Result<impl IntoResponse, Problem> {
    // Sends the supplied key to the base URL, so it needs write access like
    // creating a key does.
    permission_guard!(auth, AiGatewayWrite);
    let base_url = request
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty());
    let key_trust = requested_key_trust(&auth, request.trust_private_address, base_url)?;
    let models = app_state
        .gateway_service
        .discover_model_catalog(
            &request.provider,
            request.api_key.expose(),
            base_url,
            key_trust,
        )
        .await
        .map_err(|error| match error {
            AiGatewayError::InvalidProviderUrl { .. }
            | AiGatewayError::ProviderNotConfigured { .. } => error,
            other => AiGatewayError::Validation {
                message: format!("Could not list the provider's models: {}", other.summary()),
            },
        })?;
    Ok(Json(DiscoverProviderModelsResponse {
        provider: request.provider,
        models: models.into_iter().map(Into::into).collect(),
    }))
}

#[utoipa::path(
    tag = "AI Gateway Admin",
    post,
    path = "/ai/providers/{id}/models/refresh",
    params(("id" = i32, Path, description = "Provider key ID")),
    responses((status = 200, body = Vec<ProviderModelResponse>), (status = 404, body = ProblemDetails)),
    security(("bearer_auth" = []))
)]
async fn refresh_provider_models(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayWrite);
    let models = app_state.provider_model_service.refresh(id).await?;
    app_state.provider_status_cache.invalidate().await;
    audit_provider_model(
        &app_state,
        &auth,
        &metadata,
        PROVIDER_MODELS_REFRESHED,
        id,
        None,
        None,
        Some(models.len()),
    )
    .await;
    Ok(Json(
        models
            .into_iter()
            .map(ProviderModelResponse::from)
            .collect::<Vec<_>>(),
    ))
}

#[utoipa::path(
    tag = "AI Gateway Admin",
    post,
    path = "/ai/providers/{id}/models",
    params(("id" = i32, Path, description = "Provider key ID")),
    request_body = AddProviderModelRequest,
    responses((status = 201, body = ProviderModelResponse), (status = 400, body = ProblemDetails)),
    security(("bearer_auth" = []))
)]
async fn add_provider_model(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<i32>,
    Json(request): Json<AddProviderModelRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayWrite);
    let model = app_state
        .provider_model_service
        .add_manual(id, &request.model_id, request.display_name.as_deref())
        .await?;
    app_state.provider_status_cache.invalidate().await;
    audit_provider_model(
        &app_state,
        &auth,
        &metadata,
        PROVIDER_MODEL_ADDED,
        id,
        Some(model.model_id.clone()),
        Some(model.is_enabled),
        None,
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(ProviderModelResponse::from(model)),
    ))
}

#[utoipa::path(
    tag = "AI Gateway Admin",
    patch,
    path = "/ai/providers/{id}/models/{model_row_id}",
    params(("id" = i32, Path), ("model_row_id" = i32, Path)),
    request_body = UpdateProviderModelRequest,
    responses((status = 200, body = ProviderModelResponse), (status = 404, body = ProblemDetails)),
    security(("bearer_auth" = []))
)]
async fn update_provider_model(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((id, model_row_id)): Path<(i32, i32)>,
    Json(request): Json<UpdateProviderModelRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayWrite);
    let model = app_state
        .provider_model_service
        .set_enabled(id, model_row_id, request.is_enabled)
        .await?;
    app_state.provider_status_cache.invalidate().await;
    audit_provider_model(
        &app_state,
        &auth,
        &metadata,
        PROVIDER_MODEL_UPDATED,
        id,
        Some(model.model_id.clone()),
        Some(model.is_enabled),
        None,
    )
    .await;
    Ok(Json(ProviderModelResponse::from(model)))
}

#[utoipa::path(
    tag = "AI Gateway Admin",
    delete,
    path = "/ai/providers/{id}/models/{model_row_id}",
    params(("id" = i32, Path), ("model_row_id" = i32, Path)),
    responses((status = 204), (status = 400, body = ProblemDetails)),
    security(("bearer_auth" = []))
)]
async fn delete_provider_model(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((id, model_row_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayWrite);
    app_state
        .provider_model_service
        .delete_manual(id, model_row_id)
        .await?;
    app_state.provider_status_cache.invalidate().await;
    audit_provider_model(
        &app_state,
        &auth,
        &metadata,
        PROVIDER_MODEL_DELETED,
        id,
        Some(format!("row {model_row_id}")),
        None,
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    tag = "AI Gateway Admin",
    patch,
    path = "/ai/providers/{id}",
    request_body = UpdateProviderKeyRequest,
    responses(
        (status = 200, description = "Provider key updated", body = ProviderKeyResponse),
        (status = 400, description = "Validation error, or a new API key that failed verification (the current key is kept)", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Not found", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn update_provider_key(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<i32>,
    Json(request): Json<UpdateProviderKeyRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayWrite);

    let trust = match request.trust_private_address {
        Some(true) => {
            require_trust_permission(&auth)?;
            Some(TrustChange::Set(TrustRequest {
                set_by: auth.user_id_opt(),
            }))
        }
        // Removing the trust only narrows what the key can reach.
        Some(false) => Some(TrustChange::Clear),
        None => None,
    };

    if let Some(Some(model_id)) = request.default_model.as_ref() {
        app_state
            .provider_model_service
            .ensure_selectable(id, model_id)
            .await?;
    }

    let before = app_state.provider_key_service.get_by_id(id).await?;

    // A replacement key is verified like a new one, against the endpoint and
    // model it will be used with, before it overwrites a working key.
    if let Some(api_key) = request.api_key.as_ref() {
        let (base_url, default_model) = replacement_check_target(&before, &request);
        let check_trust = match request.trust_private_address {
            Some(trust) => requested_key_trust(&auth, trust, base_url)?,
            None => KeyTrust::from_stored(before.trusted_private_address.as_deref()),
        };
        app_state
            .gateway_service
            .test_provider(
                &before.provider,
                api_key.expose(),
                base_url,
                default_model,
                check_trust,
            )
            .await
            .map_err(|error| match error {
                AiGatewayError::InvalidProviderUrl { .. } => error,
                other => AiGatewayError::Validation {
                    message: format!(
                        "API key verification failed for provider key {id} ('{}'); the current key was kept: {}",
                        before.display_name,
                        other.summary()
                    ),
                },
            })?;
    }

    let key = app_state
        .provider_key_service
        .update(
            id,
            ProviderKeyUpdate {
                display_name: request.display_name.as_deref(),
                api_key: request.api_key.as_ref().map(SecretString::expose),
                base_url: request.base_url.as_ref().map(|o| o.as_deref()),
                default_model: request.default_model.as_ref().map(|o| o.as_deref()),
                is_active: request.is_active,
                trust,
                capabilities: request.capabilities,
            },
        )
        .await?;
    app_state.provider_status_cache.invalidate().await;

    let mut changed_fields = Vec::new();
    for (field, present) in [
        ("display_name", request.display_name.is_some()),
        ("api_key", request.api_key.is_some()),
        ("base_url", request.base_url.is_some()),
        ("default_model", request.default_model.is_some()),
        ("is_active", request.is_active.is_some()),
        (
            "trusted_private_address",
            request.trust_private_address.is_some(),
        ),
        ("capabilities", request.capabilities.is_some()),
    ] {
        if present {
            changed_fields.push(field);
        }
    }
    audit_provider_key(
        &app_state,
        &auth,
        &metadata,
        PROVIDER_KEY_UPDATED,
        &key,
        Some(&before),
        changed_fields,
    )
    .await;

    Ok(Json(ProviderKeyResponse::from(key)))
}

#[utoipa::path(
    tag = "AI Gateway Admin",
    delete,
    path = "/ai/providers/{id}",
    responses(
        (status = 204, description = "Provider key deleted"),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Not found", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn delete_provider_key(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayWrite);

    let key = app_state.provider_key_service.get_by_id(id).await?;
    app_state.provider_key_service.delete(id).await?;
    app_state.provider_status_cache.invalidate().await;
    audit_provider_key(
        &app_state,
        &auth,
        &metadata,
        PROVIDER_KEY_DELETED,
        &key,
        None,
        Vec::new(),
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    tag = "AI Gateway Admin",
    post,
    path = "/ai/providers/test",
    request_body = TestProviderKeyRequest,
    responses(
        (status = 200, description = "Test result", body = TestProviderKeyResponse),
        (status = 400, description = "Validation error", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn test_provider_key_inline(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Json(request): Json<TestProviderKeyRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayWrite);
    let base_url = request
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty());
    let key_trust = requested_key_trust(&auth, request.trust_private_address, base_url)?;

    let start = std::time::Instant::now();
    let result = app_state
        .gateway_service
        .test_provider(
            &request.provider,
            request.api_key.expose(),
            base_url,
            request.default_model.as_deref(),
            key_trust,
        )
        .await;
    let latency_ms = start.elapsed().as_millis() as u64;

    let response = match result {
        Ok(()) => TestProviderKeyResponse {
            success: true,
            provider: request.provider,
            error: None,
            latency_ms,
        },
        Err(e) => TestProviderKeyResponse {
            success: false,
            provider: request.provider,
            error: Some(e.summary()),
            latency_ms,
        },
    };

    Ok(Json(response))
}

#[utoipa::path(
    tag = "AI Gateway Admin",
    post,
    path = "/ai/providers/{id}/test",
    responses(
        (status = 200, description = "Test result", body = TestProviderKeyResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Provider key not found", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn test_provider_key_by_id(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    // Write, not Read: this decrypts a stored provider credential and spends
    // it on an outbound call. A read-only principal must not be able to drive
    // use of secret credentials -- the same bar as the inline test and
    // discover endpoints, which also exercise a key against a provider.
    permission_guard!(auth, AiGatewayWrite);

    // get_by_id already returns Err(ProviderKeyNotFound) if not found
    let key_record = app_state.provider_key_service.get_by_id(id).await?;

    let decrypted_key = app_state
        .provider_key_service
        .decrypt_api_key(&key_record.api_key_encrypted)?;

    let provider_name = key_record.provider.clone();
    let start = std::time::Instant::now();
    let result = app_state
        .gateway_service
        .test_provider(
            &key_record.provider,
            &decrypted_key,
            key_record.base_url.as_deref(),
            key_record.default_model.as_deref(),
            KeyTrust::from_stored(key_record.trusted_private_address.as_deref()),
        )
        .await;
    let latency_ms = start.elapsed().as_millis() as u64;

    let response = match result {
        Ok(()) => TestProviderKeyResponse {
            success: true,
            provider: provider_name,
            error: None,
            latency_ms,
        },
        Err(e) => TestProviderKeyResponse {
            success: false,
            provider: provider_name,
            error: Some(e.summary()),
            latency_ms,
        },
    };

    Ok(Json(response))
}

#[cfg(test)]
mod patch_deser_tests {
    use super::UpdateProviderKeyRequest;

    #[test]
    fn base_url_present_value_sets() {
        let r: UpdateProviderKeyRequest =
            serde_json::from_str(r#"{"base_url":"http://localhost:11434/v1"}"#).unwrap();
        assert_eq!(
            r.base_url,
            Some(Some("http://localhost:11434/v1".to_string()))
        );
    }

    #[test]
    fn base_url_explicit_null_clears() {
        let r: UpdateProviderKeyRequest = serde_json::from_str(r#"{"base_url":null}"#).unwrap();
        // The fix: explicit null is now distinguishable from absent.
        assert_eq!(r.base_url, Some(None));
    }

    #[test]
    fn base_url_absent_is_unchanged() {
        let r: UpdateProviderKeyRequest = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(r.base_url, None);
    }

    #[test]
    fn default_model_three_states() {
        let set: UpdateProviderKeyRequest =
            serde_json::from_str(r#"{"default_model":"gpt-5-mini"}"#).unwrap();
        assert_eq!(set.default_model, Some(Some("gpt-5-mini".to_string())));

        let clear: UpdateProviderKeyRequest =
            serde_json::from_str(r#"{"default_model":null}"#).unwrap();
        assert_eq!(clear.default_model, Some(None));

        let absent: UpdateProviderKeyRequest = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(absent.default_model, None);
    }
}

#[cfg(test)]
mod replacement_check_tests {
    use super::{replacement_check_target, UpdateProviderKeyRequest};
    use chrono::Utc;
    use temps_entities::ai_provider_keys;

    fn key(base_url: Option<&str>, default_model: Option<&str>) -> ai_provider_keys::Model {
        ai_provider_keys::Model {
            id: 7,
            provider: "openai".to_string(),
            display_name: "Team key".to_string(),
            api_key_encrypted: "encrypted".to_string(),
            base_url: base_url.map(str::to_string),
            default_model: default_model.map(str::to_string),
            is_active: true,
            trusted_private_address: None,
            trusted_private_address_set_by: None,
            trusted_private_address_set_at: None,
            capabilities: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn request(json: &str) -> UpdateProviderKeyRequest {
        serde_json::from_str(json).expect("a valid update request")
    }

    #[test]
    fn a_new_key_alone_is_checked_against_the_keys_own_endpoint_and_model() {
        let current = key(Some("https://llm.example.test/v1"), Some("small-model"));
        let update = request(r#"{"api_key":"sk-new"}"#);
        assert_eq!(
            replacement_check_target(&current, &update),
            (Some("https://llm.example.test/v1"), Some("small-model"))
        );
    }

    #[test]
    fn an_endpoint_and_model_changed_with_the_key_are_the_ones_checked() {
        let current = key(Some("https://old.example.test/v1"), Some("old-model"));
        let update = request(
            r#"{"api_key":"sk-new","base_url":"https://new.example.test/v1","default_model":"new-model"}"#,
        );
        assert_eq!(
            replacement_check_target(&current, &update),
            (Some("https://new.example.test/v1"), Some("new-model"))
        );
    }

    #[test]
    fn a_cleared_endpoint_or_model_is_checked_as_cleared() {
        let current = key(Some("https://old.example.test/v1"), Some("old-model"));
        let update = request(r#"{"api_key":"sk-new","base_url":null,"default_model":null}"#);
        assert_eq!(replacement_check_target(&current, &update), (None, None));
    }

    #[test]
    fn a_replacement_key_never_prints_through_debug() {
        let update = request(r#"{"api_key":"sk-secret-value"}"#);
        let rendered = format!("{update:?}");
        assert!(!rendered.contains("sk-secret-value"), "{rendered}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upstream_error() -> AiGatewayError {
        upstream_status(400, "unknown model")
    }

    fn upstream_status(status: u16, message: &str) -> AiGatewayError {
        AiGatewayError::UpstreamError {
            model: "gpt-5-nano".to_string(),
            status,
            message: message.to_string(),
        }
    }

    #[test]
    fn verification_error_hints_only_when_the_model_was_rejected() {
        let message = |error: AiGatewayError| match verification_error(error, Some("openai")) {
            AiGatewayError::Validation { message } => message,
            other => panic!("expected a validation error, got {other:?}"),
        };
        assert!(message(upstream_status(404, "model_not_found")).contains("OpenAI-compatible"));
        // A wrong key, a failing server or an unreachable host is fixed
        // elsewhere, not by changing the provider type.
        for error in [
            upstream_status(401, "invalid api key"),
            upstream_status(503, "overloaded"),
            AiGatewayError::HttpClient("connection refused".to_string()),
        ] {
            let message = message(error);
            assert!(!message.contains("OpenAI-compatible"), "{message}");
        }
    }

    #[test]
    fn verification_error_suggests_openai_compatible_for_vendor_with_custom_base_url() {
        let error = verification_error(upstream_error(), Some("openai"));

        let AiGatewayError::Validation { message } = error else {
            panic!("expected a validation error, got {error:?}");
        };
        assert!(
            message.starts_with("API key verification failed:"),
            "{message}"
        );
        assert!(message.contains("gpt-5-nano"), "{message}");
        assert!(
            message.contains("'openai' key must serve openai's own models"),
            "{message}"
        );
        assert!(
            message.contains("add an OpenAI-compatible provider"),
            "{message}"
        );
    }

    #[test]
    fn verification_error_without_custom_base_url_has_no_hint() {
        let error = verification_error(upstream_error(), None);

        let AiGatewayError::Validation { message } = error else {
            panic!("expected a validation error, got {error:?}");
        };
        assert!(
            message.starts_with("API key verification failed:"),
            "{message}"
        );
        assert!(!message.contains("OpenAI-compatible"), "{message}");
    }
}
