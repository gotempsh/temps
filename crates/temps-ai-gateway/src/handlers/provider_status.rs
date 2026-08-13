//! Capability and preference endpoints for the ADR-037 provider switching seam.
//!
//! `GET /api/ai/provider-status`  — returns the current routing preference and
//!   availability state so the UI can onboard instead of disappearing.
//!
//! `PUT /api/ai/provider-preference` — lets an operator switch the instance-level
//!   routing preference between the BYOK gateway and a subscription-backed agent
//!   CLI; emits an audit event on every write.

use anyhow::Result as AnyhowResult;
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
            ProviderPreferenceError::Validation { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Validation Error")
                    .with_detail(error.to_string())
            }
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
        update_ai_provider_preference
    ),
    components(schemas(
        AiProviderStatusResponse,
        AvailableAiProviderDto,
        AiModelOptionDto,
        AiSelectOptionDto,
        AiCliStatusDto,
        UpdateProviderPreferenceRequest,
    )),
    info(
        title = "AI Provider Status API",
        description = "Inspect and update the instance-level AI provider preference (ADR-037)",
        version = "1.0.0"
    ),
    tags(
        (name = "AI Provider Status", description = "Provider preference and availability endpoints")
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
        .route(
            "/ai/provider-preference",
            put(update_ai_provider_preference),
        )
}

// ============================================================================
// DTOs
// ============================================================================

/// Current AI provider routing preference and availability for this instance.
///
/// The `configured` field drives the UI onboarding state: when `false` the UI
/// must show _exactly what is missing_ (`reason`) and _where to fix it_
/// (`setup_path`), not hide the feature.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AiProviderStatusResponse {
    /// Active preference: `"gateway"` (BYOK) or `"agent_cli"` (subscription).
    pub active_provider_type: String,
    /// Catalog id of the active agent CLI provider, or `null` when
    /// `active_provider_type` is `"gateway"`.
    pub agent_cli_provider_id: Option<String>,
    /// Whether the active provider is ready to serve requests.
    pub configured: bool,
    /// Human-readable explanation of why `configured` is `false`.
    pub reason: Option<String>,
    /// Console path the operator should visit to fix the missing configuration.
    pub setup_path: Option<String>,
    /// Whether at least one active BYOK provider key exists.
    pub gateway_available: bool,
    /// Providers a chat user may choose for a new conversation. Authentication
    /// source is descriptive metadata only and never contains credentials.
    pub available_providers: Vec<AvailableAiProviderDto>,
    /// Live status of the selected agent CLI, or `null` when
    /// `active_provider_type` is `"gateway"`.
    pub agent_cli_status: Option<AiCliStatusDto>,
    /// Whether the active adapter's normalized realtime contract exposes tool
    /// events. Kept under the legacy field name for API compatibility.
    pub supports_interactive_tools: bool,
    /// Health of normalized mid-turn user interactions, or `null` when the
    /// active adapter does not advertise them. Kept for API compatibility.
    pub interactive_bridge_status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AvailableAiProviderDto {
    pub id: String,
    pub name: String,
    /// `configured_key` for the gateway or `host_environment` for an ambient
    /// CLI login discovered in the Temps process environment.
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
        .map(|model| AiModelOptionDto {
            id: model.id,
            name: model.name,
            thinking_options: model
                .thinking_modes
                .into_iter()
                .map(|option| AiSelectOptionDto {
                    id: option.id,
                    name: option.name,
                    description: option.description,
                })
                .collect(),
            default_thinking_option_id: model.default_thinking_mode_id,
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

/// Mirrors `temps_agents::ai_cli::AiCliStatus` with utoipa `ToSchema` added.
/// `AiCliStatus` itself does not derive `ToSchema`, so this local projection is
/// used for OpenAPI generation only — the fields are identical.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AiCliStatusDto {
    pub provider: String,
    pub installed: bool,
    pub version: Option<String>,
    pub authenticated: bool,
    pub auth_method: Option<String>,
    pub subscription_type: Option<String>,
    /// Instructions for the operator when not installed or not authenticated.
    pub setup_hint: Option<String>,
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

impl From<temps_agents::ai_cli::AiCliStatus> for AiCliStatusDto {
    fn from(s: temps_agents::ai_cli::AiCliStatus) -> Self {
        Self {
            provider: s.provider,
            installed: s.installed,
            version: s.version,
            authenticated: s.authenticated,
            auth_method: s.auth_method,
            subscription_type: s.subscription_type,
            setup_hint: s.setup_hint,
        }
    }
}

/// Request body for `PUT /api/ai/provider-preference`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateProviderPreferenceRequest {
    /// `"gateway"` or `"agent_cli"`.
    pub provider_type: String,
    /// Required when `provider_type` is `"agent_cli"`.
    pub agent_cli_provider_id: Option<String>,
    /// Deprecated compatibility field. Adapter realtime behavior is derived
    /// from its capability contract and cannot be toggled independently.
    pub interactive_bridge_enabled: Option<bool>,
}

// ============================================================================
// Audit event
// ============================================================================

#[derive(Debug, Clone, Serialize)]
struct ProviderPreferenceUpdatedAudit {
    context: AuditContext,
    scope: String,
    provider_type: String,
    agent_cli_provider_id: Option<String>,
}

impl AuditOperation for ProviderPreferenceUpdatedAudit {
    fn operation_type(&self) -> String {
        "ai_gateway.provider_preference.updated".to_string()
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

/// Assemble the full `AiProviderStatusResponse` from the current preference row
/// and live provider checks.  Shared between GET and the PUT response so the
/// two handlers never diverge.
async fn build_status_response(
    app_state: &AiGatewayAppState,
) -> Result<AiProviderStatusResponse, Problem> {
    // Chat may advertise and execute a host CLI only after an administrator
    // explicitly selected it. Ambient authentication proves availability, not
    // authorization to expose the host process to conversation prompts.
    let preference = app_state
        .provider_preference_service
        .get("instance")
        .await
        .map_err(Problem::from)?;
    let agent_cli_provider_id = preference
        .as_ref()
        .filter(|row| row.provider_type == "agent_cli")
        .and_then(|row| row.agent_cli_provider_id.clone());
    let active_provider_type = if agent_cli_provider_id.is_some() {
        "agent_cli".to_string()
    } else {
        "gateway".to_string()
    };

    // Determine gateway availability: at least one active provider key exists.
    let active_keys = app_state
        .provider_key_service
        .list_active()
        .await
        .map_err(Problem::from)?;
    let gateway_available = !active_keys.is_empty();
    let mut available_providers = Vec::new();
    for key in &active_keys {
        let adapter_models = app_state
            .gateway_service
            .available_models_for_provider(&key.provider);
        let mut model_ids: Vec<String> = adapter_models
            .into_iter()
            .map(|m| m.id)
            .filter(|id| !id.starts_with("text-embedding-"))
            .collect();
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
    // Probe independent CLIs concurrently. A cold cache is bounded by the
    // slowest provider rather than the sum of every subprocess timeout.
    let cli_probes =
        futures_util::future::join_all(temps_agents::ai_cli::PROVIDER_CATALOG.iter().map(
            |registration| async move {
                let provider = temps_agents::ai_cli::create_provider(registration.id)?;
                let (status, capabilities) =
                    tokio::join!(provider.get_status(), provider.capabilities());
                let realtime = capabilities
                    .as_ref()
                    .map(|capabilities| capabilities.realtime.clone());
                let option = capabilities.map(provider_option);
                Some((registration.id.to_string(), status, option, realtime))
            },
        ))
        .await;
    let mut cli_statuses = HashMap::new();
    let mut cli_realtime = HashMap::new();
    for (provider_id, status, option, realtime) in cli_probes.into_iter().flatten() {
        if status.installed && status.authenticated {
            if let Some(option) = option {
                available_providers.push(option);
            }
        }
        if let Some(realtime) = realtime {
            cli_realtime.insert(provider_id.clone(), realtime);
        }
        cli_statuses.insert(provider_id, status);
    }

    let (configured, reason, setup_path, agent_cli_status) = if active_provider_type == "agent_cli"
    {
        let provider_id = agent_cli_provider_id.as_deref().unwrap_or("");
        match cli_statuses.get(provider_id).cloned() {
            Some(status) => {
                let configured = status.installed && status.authenticated;
                let reason = if !configured {
                    Some(if !status.installed {
                        format!(
                            "The {} CLI is not installed on the Temps host",
                            status.provider
                        )
                    } else {
                        format!(
                            "The {} CLI is installed but not authenticated",
                            status.provider
                        )
                    })
                } else {
                    None
                };
                let setup_path = if !configured {
                    Some("/agent-sandbox/providers".to_string())
                } else {
                    None
                };
                let dto = AiCliStatusDto::from(status);
                (configured, reason, setup_path, Some(dto))
            }
            None => {
                // Stored id doesn't match any known provider — surface as
                // misconfigured so the operator can fix it.
                (
                        false,
                        Some(format!(
                            "Unknown agent CLI provider '{}' — update the preference to a valid provider id",
                            provider_id
                        )),
                        Some("/agent-sandbox/providers".to_string()),
                        None,
                    )
            }
        }
    } else {
        // Gateway path.
        let (configured, reason, setup_path) = if gateway_available {
            (true, None, None)
        } else {
            (
                false,
                Some("No AI provider key is configured".to_string()),
                Some("/ai-gateway".to_string()),
            )
        };
        (configured, reason, setup_path, None)
    };

    // Legacy response fields are now derived from the provider-neutral
    // realtime contract. The old Claude-only bridge toggle no longer controls
    // whether chat gets tools, streaming, cancellation, or interactions.
    let active_realtime = agent_cli_provider_id
        .as_deref()
        .and_then(|provider| cli_realtime.get(provider));
    let supports_interactive_tools = active_provider_type == "gateway"
        || active_realtime.is_some_and(|realtime| realtime.tool_events);
    let interactive_bridge_status = active_realtime
        .filter(|realtime| realtime.user_interactions)
        .map(|_| {
            if configured {
                "healthy".to_string()
            } else {
                "unavailable".to_string()
            }
        });

    Ok(AiProviderStatusResponse {
        active_provider_type,
        agent_cli_provider_id,
        configured,
        reason,
        setup_path,
        gateway_available,
        available_providers,
        agent_cli_status,
        supports_interactive_tools,
        interactive_bridge_status,
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
    // A forced refresh launches authenticated host CLI subprocesses. Keep it
    // behind the settings write permission so read-only users cannot use the
    // endpoint to repeatedly consume host resources.
    permission_guard!(auth, AiGatewayWrite);

    app_state.provider_status_cache.invalidate().await;
    let response = cached_status_response(&app_state).await?;
    Ok(Json(response))
}

#[utoipa::path(
    tag = "AI Provider Status",
    put,
    path = "/ai/provider-preference",
    request_body = UpdateProviderPreferenceRequest,
    responses(
        (status = 200, description = "Updated provider preference and availability", body = AiProviderStatusResponse),
        (status = 400, description = "Validation error", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn update_ai_provider_preference(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    axum::extract::Extension(metadata): axum::extract::Extension<RequestMetadata>,
    Json(request): Json<UpdateProviderPreferenceRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayWrite);

    app_state
        .provider_preference_service
        .set(
            "instance",
            request.provider_type.clone(),
            request.agent_cli_provider_id.clone(),
            request.interactive_bridge_enabled,
        )
        .await
        .map_err(Problem::from)?;
    app_state.provider_status_cache.invalidate().await;

    // Audit the write — failure is logged but does not fail the request.
    let audit = ProviderPreferenceUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        scope: "instance".to_string(),
        provider_type: request.provider_type.clone(),
        agent_cli_provider_id: request.agent_cli_provider_id.clone(),
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        tracing::error!(
            provider_type = %request.provider_type,
            "Failed to create audit log for provider preference update: {e}"
        );
    }

    let response = cached_status_response(&app_state).await?;
    Ok(Json(response))
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn cached_response() -> AiProviderStatusResponse {
        AiProviderStatusResponse {
            active_provider_type: "gateway".to_string(),
            agent_cli_provider_id: None,
            configured: true,
            reason: None,
            setup_path: None,
            gateway_available: true,
            available_providers: Vec::new(),
            agent_cli_status: None,
            supports_interactive_tools: true,
            interactive_bridge_status: None,
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
}
