// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Capability and summary-preference endpoints for API gateway inference.
//!
//! `GET /api/ai/provider-status`  — returns the current routing preference and
//!   availability state so the UI can onboard instead of disappearing.
//!
//! Host-authenticated development harnesses are intentionally absent: they are
//! selected by an application thread and run through the agent runtime.

use anyhow::Result as AnyhowResult;
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use temps_auth::permission_guard;
use temps_auth::permissions::Permission;
use temps_auth::RequireAuth;
use temps_core::problemdetails::{Problem, ProblemDetails};
use temps_core::{problemdetails, AuditContext, AuditOperation, RequestMetadata};
use utoipa::{OpenApi, ToSchema};

use crate::handlers::types::AiGatewayAppState;
use crate::services::ProviderPreferenceError;

// ============================================================================
// Error conversion
// ============================================================================

impl From<ProviderPreferenceError> for Problem {
    fn from(error: ProviderPreferenceError) -> Self {
        match error {
            ProviderPreferenceError::Database(_) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

// ============================================================================
// OpenAPI schema
// ============================================================================

#[derive(OpenApi)]
#[openapi(
    paths(
        get_ai_provider_status,
        refresh_ai_provider_status,
        update_ai_summary_preference
    ),
    components(schemas(
        AiProviderStatusResponse,
        AvailableAiProviderDto,
        AiModelOptionDto,
        AiSelectOptionDto,
        UpdateAiSummaryPreferenceRequest,
        AiSummaryPreferenceDto,
    )),
    info(
        title = "AI Provider Status API",
        description = "Inspect API gateway capability and summary defaults",
        version = "1.0.0"
    ),
    tags(
        (name = "AI Provider Status", description = "Gateway capability and summary-default endpoints")
    )
)]
pub struct AiProviderStatusApiDoc;

pub fn configure_provider_status_routes() -> Router<Arc<AiGatewayAppState>> {
    Router::new()
        .route("/ai/provider-status", get(get_ai_provider_status))
        .route(
            "/ai/provider-status/refresh",
            post(refresh_ai_provider_status),
        )
        .route("/ai/summary-preference", put(update_ai_summary_preference))
}

// ============================================================================
// DTOs
// ============================================================================

/// Current API-gateway availability for this instance.
///
/// The `configured` field drives the UI onboarding state: when `false` the UI
/// must show _exactly what is missing_ (`reason`) and _where to fix it_
/// (`setup_path`), not hide the feature.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AiProviderStatusResponse {
    /// Whether the active provider is ready to serve requests.
    pub configured: bool,
    /// Human-readable explanation of why `configured` is `false`.
    pub reason: Option<String>,
    /// Console path the operator should visit to fix the missing configuration.
    pub setup_path: Option<String>,
    /// Whether at least one active BYOK provider key exists.
    pub gateway_available: bool,
    /// Gateway providers available to API-backed chat and summaries. Credential
    /// source is descriptive metadata only and never contains secret values.
    pub available_providers: Vec<AvailableAiProviderDto>,
    /// Whether the active adapter's normalized realtime contract exposes tool
    /// events. Kept under the legacy field name for API compatibility.
    pub supports_interactive_tools: bool,
    /// Health of normalized mid-turn user interactions, or `null` when the
    /// active adapter does not advertise them. Kept for API compatibility.
    pub interactive_bridge_status: Option<String>,
    /// Instance-wide defaults inherited by all server-authored AI summaries.
    pub summary_preference: AiSummaryPreferenceDto,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct AiSummaryPreferenceDto {
    /// Normalized gateway route (`gateway_key:{id}`). `null` inherits the
    /// active gateway key.
    pub provider_id: Option<String>,
    /// `null` uses the selected provider's default model.
    pub model: Option<String>,
    /// `null` uses the selected model's default reasoning depth.
    pub thinking_level: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AvailableAiProviderDto {
    pub id: String,
    pub name: String,
    /// `configured_key` for an encrypted gateway key.
    pub auth_source: String,
    pub models: Vec<AiModelOptionDto>,
    pub default_model_id: Option<String>,
    /// `ready` when the model list was loaded, `unavailable` when the provider
    /// can still run with its own default but live discovery failed.
    pub model_discovery_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_discovery_error: Option<String>,
    pub permission_modes: Vec<AiSelectOptionDto>,
    pub default_permission_mode_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AiModelOptionDto {
    pub id: String,
    pub name: String,
    pub thinking_options: Vec<AiSelectOptionDto>,
    /// Model-specific reasoning options valid while project-chat function
    /// tools are attached. Omitted when the normal options also apply.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_thinking_options: Option<Vec<AiSelectOptionDto>>,
    pub default_thinking_option_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AiSelectOptionDto {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

fn provider_option(capabilities: temps_ai::ProviderCapabilities) -> AvailableAiProviderDto {
    let models = capabilities
        .models
        .into_iter()
        .map(|model| {
            let map_option = |option: temps_ai::SelectOption| AiSelectOptionDto {
                id: option.id,
                name: option.name,
                description: option.description,
            };
            AiModelOptionDto {
                id: model.id,
                name: model.name,
                thinking_options: model.thinking_modes.into_iter().map(&map_option).collect(),
                tool_thinking_options: model
                    .tool_thinking_modes
                    .map(|options| options.into_iter().map(map_option).collect()),
                default_thinking_option_id: model.default_thinking_mode_id,
            }
        })
        .collect::<Vec<_>>();
    let model_discovery_status = if models.is_empty() {
        "unavailable"
    } else {
        "ready"
    };
    let model_discovery_error = models.is_empty().then(|| {
        format!(
            "Could not query {} for its current model list. The CLI default remains usable; retry provider discovery to load model controls.",
            capabilities.name
        )
    });
    AvailableAiProviderDto {
        id: capabilities.id,
        name: capabilities.name,
        auth_source: match capabilities.auth_source {
            temps_ai::ProviderAuthSource::ConfiguredKey => "configured_key",
            temps_ai::ProviderAuthSource::HostEnvironment => "host_environment",
        }
        .to_string(),
        models,
        default_model_id: capabilities.default_model_id,
        model_discovery_status: model_discovery_status.to_string(),
        model_discovery_error,
        permission_modes: capabilities
            .permission_modes
            .into_iter()
            .map(|mode| AiSelectOptionDto {
                id: mode.id,
                name: mode.name,
                description: mode.description,
            })
            .collect(),
        default_permission_mode_id: capabilities.default_permission_mode_id,
    }
}

const PROVIDER_STATUS_CACHE_TTL: Duration = Duration::from_secs(60);

struct CachedProviderStatus {
    cached_at: Instant,
    response: AiProviderStatusResponse,
}

/// Short-lived capability snapshot. CLI authentication/model discovery can
/// involve several subprocesses, so it must never run on every page render.
pub struct AiProviderStatusCache {
    value: tokio::sync::RwLock<Option<CachedProviderStatus>>,
    refresh: tokio::sync::Mutex<()>,
    generation: AtomicU64,
}

impl Default for AiProviderStatusCache {
    fn default() -> Self {
        Self {
            value: tokio::sync::RwLock::new(None),
            refresh: tokio::sync::Mutex::new(()),
            generation: AtomicU64::new(0),
        }
    }
}

impl AiProviderStatusCache {
    async fn get(&self) -> Option<AiProviderStatusResponse> {
        self.value.read().await.as_ref().and_then(|cached| {
            (cached.cached_at.elapsed() < PROVIDER_STATUS_CACHE_TTL)
                .then(|| cached.response.clone())
        })
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    async fn store_if_current(&self, generation: u64, response: AiProviderStatusResponse) -> bool {
        let mut value = self.value.write().await;
        if self.generation() != generation {
            return false;
        }
        *value = Some(CachedProviderStatus {
            cached_at: Instant::now(),
            response,
        });
        true
    }

    pub async fn invalidate(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        *self.value.write().await = None;
    }
}

/// Replace the instance-wide defaults inherited by every AI summary.
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateAiSummaryPreferenceRequest {
    pub provider_id: Option<String>,
    pub model: Option<String>,
    pub thinking_level: Option<String>,
}

// ============================================================================
// Audit event
// ============================================================================

#[derive(Debug, Clone, Serialize)]
struct AiSummaryPreferenceUpdatedAudit {
    context: AuditContext,
    provider_id: Option<String>,
}

impl AuditOperation for AiSummaryPreferenceUpdatedAudit {
    fn operation_type(&self) -> String {
        "ai_gateway.summary_preference.updated".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> AnyhowResult<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {e}"))
    }
}

// ============================================================================
// Shared helper
// ============================================================================

/// Assemble the full `AiProviderStatusResponse` from gateway configuration and
/// live gateway-key checks.
async fn build_status_response(
    app_state: &AiGatewayAppState,
) -> Result<AiProviderStatusResponse, Problem> {
    let preference = app_state
        .provider_preference_service
        .get("instance")
        .await
        .map_err(Problem::from)?;
    // The API gateway never routes to an ambient host harness. Those are
    // available only through the agent runtime selected by a chat thread.
    // Determine gateway availability: at least one active provider key exists.
    let active_keys = app_state
        .provider_key_service
        .list_active()
        .await
        .map_err(Problem::from)?;
    let gateway_available = !active_keys.is_empty();
    let mut available_providers = Vec::new();
    for key in &active_keys {
        let catalog_models = app_state
            .provider_model_service
            .list_for_key(key.id)
            .await
            .map_err(Problem::from)?;
        let mut model_ids: Vec<String> = catalog_models
            .into_iter()
            .filter(|model| model.is_available && model.is_enabled)
            .map(|model| model.model_id)
            .filter(|id| !id.starts_with("text-embedding-"))
            .collect();
        // Upgrade/bootstrap fallback only: a key created before the inventory
        // migration remains immediately usable while its persisted catalog is
        // seeded/refreshed in the background.
        if model_ids.is_empty() {
            model_ids = app_state
                .gateway_service
                .available_models_for_provider(&key.provider)
                .into_iter()
                .map(|model| model.id)
                .filter(|id| !id.starts_with("text-embedding-"))
                .collect();
        }
        if let Some(default_model) = key.default_model.as_deref().filter(|m| !m.is_empty()) {
            if !model_ids.iter().any(|model| model == default_model) {
                model_ids.insert(0, default_model.to_string());
            }
        }
        available_providers.push(provider_option(
            crate::services::gateway_provider_capabilities(
                format!("gateway_key:{}", key.id),
                key.display_name.clone(),
                &key.provider,
                key.default_model.clone(),
                model_ids,
            ),
        ));
    }
    let (configured, reason, setup_path) = if gateway_available {
        (true, None, None)
    } else {
        (
            false,
            Some("No AI provider key is configured".to_string()),
            Some("/ai-gateway".to_string()),
        )
    };

    // Legacy response fields are now derived from the provider-neutral
    // realtime contract. The old Claude-only bridge toggle no longer controls
    // whether chat gets tools, streaming, cancellation, or interactions.
    let supports_interactive_tools = true;
    let interactive_bridge_status = None;

    Ok(AiProviderStatusResponse {
        configured,
        reason,
        setup_path,
        gateway_available,
        available_providers,
        supports_interactive_tools,
        interactive_bridge_status,
        summary_preference: preference
            .map(|row| AiSummaryPreferenceDto {
                provider_id: row.summary_provider_id,
                model: row.summary_model,
                thinking_level: row.summary_thinking_level,
            })
            .unwrap_or_default(),
    })
}

async fn cached_status_response(
    app_state: &AiGatewayAppState,
) -> Result<AiProviderStatusResponse, Problem> {
    if let Some(response) = app_state.provider_status_cache.get().await {
        return Ok(response);
    }

    // Single-flight cold/expired refresh: concurrent page mounts wait for the
    // same probe instead of each launching their own CLI processes.
    let _refresh = app_state.provider_status_cache.refresh.lock().await;
    if let Some(response) = app_state.provider_status_cache.get().await {
        return Ok(response);
    }
    let generation = app_state.provider_status_cache.generation();
    let response = build_status_response(app_state).await?;
    app_state
        .provider_status_cache
        .store_if_current(generation, response.clone())
        .await;
    Ok(response)
}

// ============================================================================
// Handlers
// ============================================================================

#[utoipa::path(
    tag = "AI Provider Status",
    get,
    path = "/ai/provider-status",
    responses(
        (status = 200, description = "Current provider preference and availability", body = AiProviderStatusResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn get_ai_provider_status(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
) -> Result<impl IntoResponse, Problem> {
    // Chat is a project feature, not an AI-provider administration surface.
    // The response exposes capability metadata only (never credentials), so a
    // project reader may load the composer even without AiGatewayRead.
    if !can_read_provider_status(&auth) {
        return Err(problemdetails::new(StatusCode::FORBIDDEN)
            .with_title("Insufficient Permissions")
            .with_detail(
                "Reading AI chat provider capabilities requires ProjectsRead or AiGatewayRead",
            ));
    }

    let response = cached_status_response(&app_state).await?;
    Ok(Json(response))
}

fn can_read_provider_status(auth: &temps_auth::AuthContext) -> bool {
    provider_status_permission_granted(
        auth.has_permission(&Permission::AiGatewayRead),
        auth.has_permission(&Permission::ProjectsRead),
    )
}

fn provider_status_permission_granted(ai_gateway_read: bool, projects_read: bool) -> bool {
    ai_gateway_read || projects_read
}

#[utoipa::path(
    tag = "AI Provider Status",
    post,
    path = "/ai/provider-status/refresh",
    responses(
        (status = 200, description = "Fresh provider authentication and model capability snapshot", body = AiProviderStatusResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Provider refresh failed", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn refresh_ai_provider_status(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
) -> Result<impl IntoResponse, Problem> {
    // A forced refresh rebuilds gateway-key capability metadata. Keep it
    // behind the settings write permission so read-only users cannot use the
    // endpoint to repeatedly consume provider resources.
    permission_guard!(auth, AiGatewayWrite);

    app_state.provider_status_cache.invalidate().await;
    let response = cached_status_response(&app_state).await?;
    Ok(Json(response))
}

#[utoipa::path(
    tag = "AI Provider Status",
    put,
    path = "/ai/summary-preference",
    request_body = UpdateAiSummaryPreferenceRequest,
    responses(
        (status = 200, description = "Updated summary routing defaults", body = AiProviderStatusResponse),
        (status = 400, description = "Unsupported provider, model, or thinking level", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn update_ai_summary_preference(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    axum::extract::Extension(metadata): axum::extract::Extension<RequestMetadata>,
    Json(request): Json<UpdateAiSummaryPreferenceRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayWrite);

    let status = cached_status_response(&app_state).await?;
    validate_summary_preference(&status, &request)?;
    app_state
        .provider_preference_service
        .set_summary_preference(
            request.provider_id.clone(),
            request.model.clone(),
            request.thinking_level.clone(),
        )
        .await
        .map_err(Problem::from)?;
    app_state.provider_status_cache.invalidate().await;

    let audit = AiSummaryPreferenceUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        provider_id: request.provider_id.clone(),
    };
    if let Err(error) = app_state.audit_service.create_audit_log(&audit).await {
        tracing::error!(%error, "Failed to create audit log for AI summary preference update");
    }

    cached_status_response(&app_state).await.map(Json)
}

fn validate_summary_preference(
    status: &AiProviderStatusResponse,
    request: &UpdateAiSummaryPreferenceRequest,
) -> Result<(), Problem> {
    let Some(provider_id) = request.provider_id.as_deref() else {
        if request.model.is_some() || request.thinking_level.is_some() {
            return Err(summary_preference_validation_problem(
                "Select a summary provider before choosing a model or thinking level",
            ));
        }
        return Ok(());
    };
    let provider = status
        .available_providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .ok_or_else(|| {
            summary_preference_validation_problem(format!(
                "AI summary provider '{provider_id}' is not configured and available"
            ))
        })?;

    let selected_model = request
        .model
        .as_deref()
        .or(provider.default_model_id.as_deref());
    let model = match selected_model {
        Some(model_id) => Some(
            provider
                .models
                .iter()
                .find(|model| model.id == model_id)
                .ok_or_else(|| {
                    summary_preference_validation_problem(format!(
                        "Model '{model_id}' is not available from provider '{}'",
                        provider.name
                    ))
                })?,
        ),
        None => None,
    };
    if let Some(thinking) = request.thinking_level.as_deref() {
        let model = model.ok_or_else(|| {
            summary_preference_validation_problem(
                "Choose a discovered model before setting a thinking level",
            )
        })?;
        if !model
            .thinking_options
            .iter()
            .any(|option| option.id == thinking)
        {
            return Err(summary_preference_validation_problem(format!(
                "Thinking level '{thinking}' is not supported by model '{}'",
                model.id
            )));
        }
    }
    Ok(())
}

fn summary_preference_validation_problem(detail: impl Into<String>) -> Problem {
    problemdetails::new(StatusCode::BAD_REQUEST)
        .with_title("Invalid AI Summary Preference")
        .with_detail(detail.into())
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn cached_response() -> AiProviderStatusResponse {
        AiProviderStatusResponse {
            configured: true,
            reason: None,
            setup_path: None,
            gateway_available: true,
            available_providers: Vec::new(),
            supports_interactive_tools: true,
            interactive_bridge_status: None,
            summary_preference: AiSummaryPreferenceDto::default(),
        }
    }

    #[tokio::test]
    async fn provider_status_cache_returns_and_invalidates_snapshot() {
        let cache = AiProviderStatusCache::default();
        cache
            .store_if_current(cache.generation(), cached_response())
            .await;
        assert!(cache.get().await.is_some());
        cache.invalidate().await;
        assert!(cache.get().await.is_none());
    }

    #[tokio::test]
    async fn invalidation_discards_an_in_flight_refresh_result() {
        let cache = AiProviderStatusCache::default();
        let refresh_generation = cache.generation();
        cache.invalidate().await;
        assert!(
            !cache
                .store_if_current(refresh_generation, cached_response())
                .await
        );
        assert!(cache.get().await.is_none());
    }

    #[tokio::test]
    async fn provider_status_cache_expires_old_snapshot() {
        let cache = AiProviderStatusCache::default();
        *cache.value.write().await = Some(CachedProviderStatus {
            cached_at: Instant::now() - PROVIDER_STATUS_CACHE_TTL,
            response: cached_response(),
        });
        assert!(cache.get().await.is_none());
    }

    #[test]
    fn chat_project_readers_can_load_provider_capabilities() {
        assert!(provider_status_permission_granted(false, true));
        assert!(provider_status_permission_granted(true, false));
        assert!(!provider_status_permission_granted(false, false));
    }

    #[test]
    fn summary_preference_is_validated_against_live_provider_capabilities() {
        let mut status = cached_response();
        status.available_providers.push(AvailableAiProviderDto {
            id: "gateway_key:7".to_string(),
            name: "OpenAI".to_string(),
            auth_source: "configured_key".to_string(),
            models: vec![AiModelOptionDto {
                id: "gpt-5.6".to_string(),
                name: "GPT-5.6".to_string(),
                thinking_options: vec![AiSelectOptionDto {
                    id: "high".to_string(),
                    name: "High".to_string(),
                    description: None,
                }],
                tool_thinking_options: None,
                default_thinking_option_id: Some("high".to_string()),
            }],
            default_model_id: Some("gpt-5.6".to_string()),
            model_discovery_status: "ready".to_string(),
            model_discovery_error: None,
            permission_modes: Vec::new(),
            default_permission_mode_id: None,
        });

        assert!(validate_summary_preference(
            &status,
            &UpdateAiSummaryPreferenceRequest {
                provider_id: Some("gateway_key:7".to_string()),
                model: Some("gpt-5.6".to_string()),
                thinking_level: Some("high".to_string()),
            }
        )
        .is_ok());
        assert!(validate_summary_preference(
            &status,
            &UpdateAiSummaryPreferenceRequest {
                provider_id: Some("gateway_key:7".to_string()),
                model: Some("invented-model".to_string()),
                thinking_level: None,
            }
        )
        .is_err());
        assert!(validate_summary_preference(
            &status,
            &UpdateAiSummaryPreferenceRequest {
                provider_id: Some("gateway_key:7".to_string()),
                model: Some("gpt-5.6".to_string()),
                thinking_level: Some("invented".to_string()),
            }
        )
        .is_err());
    }
}
