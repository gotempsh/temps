// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! AI provider catalog + per-provider credential management.
//!
//! Two responsibilities:
//!
//!   1. **Expose the catalog** so the settings UI can render one card per
//!      provider (install command, auth flavors, env var names) without
//!      duplicating the catalog in TypeScript.
//!
//!   2. **Save per-provider credentials** into the JSON-only `providers`
//!      map on `agent_sandbox` settings. No DB migration is required for new
//!      providers — they simply appear once a catalog entry exists.
//!
//! The legacy `/settings/agent-token` endpoint still exists in `trigger.rs`
//! and writes the deprecated flat `api_key_encrypted` field. New UI calls
//! this handler instead, which writes into `providers[id]` so each provider
//! keeps its own credential.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, patch, post},
    Extension, Json, Router,
};
use sea_orm::EntityTrait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::{IntoParams, ToSchema};

use temps_auth::{permission_guard, Permission, RequireAuth};
use temps_core::audit::{AuditContext, AuditOperation};
use temps_core::problemdetails::{self, Problem};
use temps_core::RequestMetadata;

use crate::ai_cli::catalog::{
    find_provider, CredentialFormat, HostAccessRequirement, PROVIDER_CATALOG,
};
use crate::error::AgentError;
use crate::handlers::AppState;
use crate::services::provider_credential_service::{
    discover_local_credential, discover_local_credential_summary, LocalCredentialSummary,
};

// ── DTOs ──────────────────────────────────────────────────────────────────────

/// One auth flavor surfaced to the UI. Mirrors `AuthFlavor` in the catalog
/// but without the seed-path / env-var fields the frontend doesn't need
/// (those are server-side only — exposing them just bloats the response).
#[derive(Debug, Serialize, ToSchema)]
pub struct AuthFlavorDto {
    pub id: String,
    pub label: String,
    pub description: String,
    /// `api_key`, `oauth_token`, or `config_file` — drives which input UI
    /// the settings page renders (single-line vs. multi-line textarea).
    pub format: String,
    /// For `api_key` format: the env var name that will be set inside the
    /// sandbox. Useful for showing the user "we'll set OPENAI_API_KEY" so
    /// they know what their key controls.
    pub env_var: Option<String>,
}

/// One catalog entry rendered for the settings UI.
#[derive(Debug, Serialize, ToSchema)]
pub struct ProviderCatalogDto {
    pub id: String,
    pub name: String,
    pub install_command: String,
    pub auth_command: String,
    pub auth_flavors: Vec<AuthFlavorDto>,
    /// Model ids this provider accepts, in display order. The first entry is
    /// the recommended default. Empty when the provider doesn't expose model
    /// selection (e.g. OpenCode), which the UI uses to hide the dropdown.
    pub models: Vec<String>,
    /// Full normalized runtime capabilities used by application chat. Unlike
    /// `models`, this preserves resolved display names and reasoning choices.
    pub runtime_models: Vec<temps_ai::ModelCapability>,
    /// Explicit default used when a new harness thread is created.
    pub default_runtime_model_id: Option<String>,
    /// Provider-native permission modes available to sandboxed harness turns.
    pub permission_modes: Vec<temps_ai::SelectOption>,
    pub default_permission_mode_id: String,
    /// True when a credential is currently saved for this provider in the
    /// settings JSON. Lets the UI render "Configured" badges without the
    /// frontend having to inspect the encrypted blob.
    pub credential_saved: bool,
    /// `verified`, `unverified`, or `not_saved`; never implies model access.
    pub credential_verification_status: String,
    pub verification_hint: Option<String>,
    /// Currently saved auth flavor id (when `credential_saved` is true).
    /// `None` when no credential is saved yet.
    pub current_auth_type: Option<String>,
    /// Currently saved default model id for this provider, if one was
    /// picked. `None` means "use the CLI's own default" — the UI renders
    /// that as "Use provider default".
    pub default_model: Option<String>,
    /// Default max turns for the autofixer analysis phase. `None` = built-in
    /// default (10). Only enforced for CLIs with a turn flag (Claude Code).
    pub max_turns_analysis: Option<i32>,
    /// Default max turns for the autofixer fix phase. `None` = built-in
    /// default (20).
    pub max_turns_fix: Option<i32>,
    /// Default max turns for autofixer feedback rounds. `None` = built-in
    /// default (10).
    pub max_turns_feedback: Option<i32>,
    /// True when this provider's CLI supports enforcing a turn cap. False
    /// for Codex/OpenCode, which run to completion — the UI labels their
    /// max-turns inputs accordingly.
    pub supports_max_turns: bool,
    /// True when the CLI is installed AND authenticated on **this host** —
    /// the machine running the Temps server process. This is a completely
    /// different signal from `credential_saved`: that field is about a
    /// credential available to the server-side, turn-scoped workspace relay.
    /// Persistent workspace chat never inherits the host's ambient CLI
    /// session. A provider can show
    /// `credential_saved: true` and `host_authenticated: false` at the same
    /// time.
    pub host_authenticated: bool,
    /// Authentication mechanism reported by the CLI running in the Temps
    /// process environment (for example `chatgpt_subscription` or
    /// `host_auth_store`). Never contains credential material.
    pub host_auth_method: Option<String>,
    /// Installed CLI version used as part of the model-cache identity.
    pub host_version: Option<String>,
    /// Whether the catalog is live, cached, stale, or a bootstrap fallback.
    pub model_source: temps_ai::ModelCatalogSource,
    /// Time of the last successful account-aware CLI model discovery.
    pub models_refreshed_at: Option<String>,
    /// Explains why `host_authenticated` is false (not installed vs.
    /// installed-but-not-authenticated), or `None` when it's true.
    pub host_auth_hint: Option<String>,
    /// True only when this provider can execute inside a persistent Temps
    /// workspace with a saved credential and a secure turn-scoped relay.
    /// This is the authoritative signal for workspace harness pickers.
    pub workspace_ready: bool,
    /// Actionable explanation when `workspace_ready` is false.
    pub workspace_readiness_hint: Option<String>,
    /// Importable credential already used by this host's CLI, if one can be
    /// copied directly into encrypted Temps settings. Contains metadata only.
    pub local_credential: Option<LocalCredentialDto>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProviderCatalogResponse {
    /// Active provider id from `agent_sandbox.default_provider`. The settings
    /// UI uses this to highlight which card is the active one.
    pub default_provider: String,
    pub providers: Vec<ProviderCatalogDto>,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListAiProvidersQuery {
    /// Return only static/cached catalog metadata. This path deliberately
    /// skips the settings-row read as well as CLI probes and is used for chat
    /// first paint; an authenticated refresh follows in the background.
    #[serde(default)]
    pub catalog_only: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveCredentialRequest {
    /// Auth flavor id (must match one of the provider's catalog entries).
    pub auth_type: String,
    /// Plaintext credential body (API key, OAuth token, or full config file
    /// contents). Encrypted with `EncryptionService` before being persisted
    /// inside the `agent_sandbox.providers` JSON map.
    pub credential: String,
    /// OpenCode model to test, in `provider/model` form. Omit to use the
    /// built-in minimal probe model. A successfully verified explicit model
    /// becomes this provider's workspace default.
    pub verification_model: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct VerifySavedCredentialRequest {
    /// OpenCode model in `provider/model` form, used to verify the saved secret.
    pub verification_model: String,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ImportLocalCredentialQuery {
    /// OpenCode model to use for the verification request, in `provider/model` form.
    pub verification_model: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SaveCredentialResponse {
    pub saved: bool,
    pub credential_verification_status: String,
    pub verification_hint: Option<String>,
    pub provider_id: String,
    pub auth_type: String,
    pub provider: ProviderCatalogDto,
}

/// Metadata about a credential that Temps can import from the server process
/// user's existing CLI login. This never includes a path or credential value.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LocalCredentialDto {
    pub auth_type: String,
    /// Stable machine-readable source: `environment` or `host_auth_store`.
    pub source: String,
    pub label: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ImportLocalCredentialResponse {
    pub saved: bool,
    pub credential_verification_status: String,
    pub verification_hint: Option<String>,
    pub provider_id: String,
    pub auth_type: String,
    pub source: String,
    pub workspace_ready: bool,
    pub provider: ProviderCatalogDto,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ActivateProviderResponse {
    pub default_provider: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RefreshProviderModelsResponse {
    pub provider_id: String,
    pub runtime_models: Vec<temps_ai::ModelCapability>,
    pub default_runtime_model_id: Option<String>,
    pub model_source: temps_ai::ModelCatalogSource,
    pub models_refreshed_at: Option<String>,
}

fn uses_workspace_model_discovery(
    provider: &crate::ai_cli::catalog::ProviderCatalogEntry,
    config: &temps_core::ProviderConfig,
) -> bool {
    provider.workspace_chat_supported && config.credentials_encrypted.is_some()
}

fn should_discover_local_credentials(catalog_only: bool, is_system_admin: bool) -> bool {
    !catalog_only && is_system_admin
}

#[derive(Debug, Clone, Serialize)]
struct ProviderModelsRefreshedAudit {
    context: AuditContext,
    provider_id: String,
    model_count: usize,
    model_source: temps_ai::ModelCatalogSource,
}

#[derive(Debug, Clone, Serialize)]
struct ProviderCredentialSavedAudit {
    context: AuditContext,
    provider_id: String,
    auth_type: String,
    source: String,
}

#[derive(Debug, Clone, Serialize)]
struct ProviderSettingsChangedAudit {
    context: AuditContext,
    provider_id: String,
    change: String,
}

impl AuditOperation for ProviderSettingsChangedAudit {
    fn operation_type(&self) -> String {
        "AI_PROVIDER_SETTINGS_CHANGED".into()
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
    fn serialize(&self) -> temps_core::anyhow::Result<String> {
        serde_json::to_string(self).map_err(|error| {
            temps_core::anyhow::anyhow!("failed to serialize provider settings audit: {error}")
        })
    }
}

impl AuditOperation for ProviderCredentialSavedAudit {
    fn operation_type(&self) -> String {
        "AI_PROVIDER_CREDENTIAL_SAVED".to_string()
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

    fn serialize(&self) -> temps_core::anyhow::Result<String> {
        serde_json::to_string(self).map_err(|error| {
            temps_core::anyhow::anyhow!("failed to serialize provider credential audit: {error}")
        })
    }
}

impl AuditOperation for ProviderModelsRefreshedAudit {
    fn operation_type(&self) -> String {
        "AI_PROVIDER_MODELS_REFRESHED".to_string()
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

    fn serialize(&self) -> temps_core::anyhow::Result<String> {
        serde_json::to_string(self).map_err(|error| {
            temps_core::anyhow::anyhow!("failed to serialize model refresh audit: {error}")
        })
    }
}

/// Body for `PATCH /settings/ai-providers/{provider_id}` — updates
/// provider-scoped settings (just the default model for now) without
/// touching the credential. Keeping credentials out of this shape means
/// the UI can auto-save model changes on select, without forcing the user
/// to re-paste their token or config file.
/// Name-spaced schema name avoids an OpenAPI collision with
/// `temps-notifications::UpdateProviderRequest`, which has different fields.
/// Both are exposed as `utoipa::ToSchema`; without the override the merged
/// OpenAPI doc would silently shadow one struct with the other and break
/// generated CLI/web clients.
#[derive(Debug, Deserialize, ToSchema)]
#[schema(as = UpdateAiProviderRequest)]
pub struct UpdateProviderRequest {
    /// New default model id. `None` or an empty string clears the stored
    /// value so the CLI falls back to its own default.
    #[serde(default)]
    pub default_model: Option<String>,
    /// Default max turns for the autofixer analysis phase (1–200). `0`
    /// clears the stored value (built-in default applies); omitted/`None`
    /// leaves the current value unchanged — so a PATCH that only updates
    /// `default_model` doesn't wipe the turn settings.
    #[serde(default)]
    pub max_turns_analysis: Option<i32>,
    /// Default max turns for the autofixer fix phase (1–200). `0` clears;
    /// omitted leaves unchanged.
    #[serde(default)]
    pub max_turns_fix: Option<i32>,
    /// Default max turns for autofixer feedback rounds (1–200). `0` clears;
    /// omitted leaves unchanged.
    #[serde(default)]
    pub max_turns_feedback: Option<i32>,
}

#[derive(Debug, Serialize, ToSchema)]
#[schema(as = UpdateAiProviderResponse)]
pub struct UpdateProviderResponse {
    pub provider_id: String,
    pub default_model: Option<String>,
    pub max_turns_analysis: Option<i32>,
    pub max_turns_fix: Option<i32>,
    pub max_turns_feedback: Option<i32>,
}

// ── Routes ────────────────────────────────────────────────────────────────────

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/settings/ai-providers", get(list_ai_providers))
        .route(
            "/settings/ai-providers/{provider_id}",
            patch(update_ai_provider),
        )
        .route(
            "/settings/ai-providers/{provider_id}/credential",
            post(save_ai_provider_credential),
        )
        .route(
            "/settings/ai-providers/{provider_id}/credential/import-local",
            post(import_local_ai_provider_credential),
        )
        .route(
            "/settings/ai-providers/{provider_id}/credential/verify-saved",
            post(verify_saved_ai_provider_credential),
        )
        .route(
            "/settings/ai-providers/{provider_id}/activate",
            post(activate_ai_provider),
        )
        .route(
            "/settings/ai-providers/{provider_id}/models/refresh",
            post(refresh_ai_provider_models),
        )
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// List the AI provider catalog. Includes per-provider "is a credential
/// configured?" so the settings UI can render configured/not-configured
/// badges without leaking the encrypted credential.
#[utoipa::path(
    tag = "Agents",
    get,
    path = "/settings/ai-providers",
    params(ListAiProvidersQuery),
    responses(
        (status = 200, body = ProviderCatalogResponse),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_ai_providers(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<ListAiProvidersQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);

    let sandbox = if query.catalog_only {
        temps_core::AgentSandboxSettings::default()
    } else {
        load_agent_sandbox(&app_state).await?
    };

    let ai_service = app_state.ai_service.clone();
    let principal_id = auth.user_id();
    let can_discover_local_credentials = auth.has_permission(&Permission::SystemAdmin);
    let providers = futures::future::join_all(PROVIDER_CATALOG.iter().map(|entry| {
        let provider_config = sandbox.provider_config(entry.id);
        let ai_service = ai_service.clone();
        async move {
            let local_credential = if !should_discover_local_credentials(
                query.catalog_only,
                can_discover_local_credentials,
            ) {
                None
            } else {
                match discover_local_credential_summary(entry).await {
                    Ok(credential) => credential,
                    Err(error) => {
                        tracing::warn!(
                            provider_id = entry.id,
                            %error,
                            "could not inspect local AI provider credential"
                        );
                        None
                    }
                }
            };
            let workspace_snapshot = if entry.workspace_chat_supported
                && provider_config.credentials_encrypted.is_some()
            {
                match ai_service {
                    Some(service) => service
                        .capabilities_snapshot_for_principal(
                            Some(entry.id),
                            principal_id,
                            temps_ai::RefreshPolicy::Cached,
                        )
                        .await
                        .ok(),
                    None => None,
                }
            } else {
                None
            };
            provider_catalog_dto(entry, provider_config, workspace_snapshot, local_credential).await
        }
    }))
    .await;

    Ok(Json(ProviderCatalogResponse {
        default_provider: sandbox.default_provider,
        providers,
    }))
}

/// Refresh exactly one provider's account-aware model inventory. This is an
/// explicit operation because it starts a CLI process (and, for Claude
/// workspaces, creates or wakes the user's persistent sandbox). Authorization
/// therefore matches the provider's chat execution requirement instead of
/// requiring unrelated settings mutation access. The AI service applies a
/// provider-scoped single-flight and cooldown before doing that work.
#[utoipa::path(
    tag = "Agents",
    post,
    path = "/settings/ai-providers/{provider_id}/models/refresh",
    params(("provider_id" = String, Path, description = "AI provider ID")),
    responses(
        (status = 200, body = RefreshProviderModelsResponse),
        (status = 400, description = "Unknown provider"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Provider execution permission required"),
        (status = 503, description = "Provider model discovery unavailable"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn refresh_ai_provider_models(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(provider_id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    let provider = find_provider(&provider_id).ok_or_else(|| {
        Problem::from(AgentError::Validation {
            message: format!("Unknown AI provider '{provider_id}'"),
        })
    })?;
    ensure_provider_runtime_permission(&auth, provider)?;
    let ai_service = app_state.ai_service.as_ref().ok_or_else(|| {
        problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
            .with_title("AI provider refresh unavailable")
            .with_detail("The AI provider service is not configured on this Temps instance.")
    })?;
    let sandbox = load_agent_sandbox(&app_state).await?;
    let provider_config = sandbox.provider_config(&provider_id);
    let workspace_discovery = uses_workspace_model_discovery(provider, &provider_config);
    if workspace_discovery {
        ensure_workspace_model_discovery_permission(&auth)?;
    }
    let snapshot_result = if workspace_discovery {
        ai_service
            .capabilities_snapshot_for_principal(
                Some(&provider_id),
                auth.user_id(),
                temps_ai::RefreshPolicy::Refresh,
            )
            .await
    } else {
        ai_service
            .capabilities_snapshot_for(Some(&provider_id), temps_ai::RefreshPolicy::Refresh)
            .await
    };
    let snapshot = snapshot_result.map_err(|error| {
        let (error_kind, error_purpose) = match &error {
            temps_ai::AiError::NotAvailable => ("not_available", None),
            temps_ai::AiError::NoModel { purpose } => ("no_model", Some(purpose.as_str())),
            temps_ai::AiError::Provider { purpose, .. } => ("provider", Some(purpose.as_str())),
            temps_ai::AiError::RetainedHarnessDiagnostic { purpose, .. } => {
                ("retained_harness", Some(purpose.as_str()))
            }
            temps_ai::AiError::CredentialVerification { .. } => ("credential_verification", None),
        };
        tracing::warn!(
            provider_id,
            user_id = auth.user_id(),
            workspace_discovery,
            error_kind,
            error_purpose,
            "AI provider model refresh failed"
        );
        let detail = provider_model_refresh_error_detail(
            provider,
            &provider_id,
            workspace_discovery,
            &error,
        );
        problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
            .with_title("Could not refresh provider models")
            .with_detail(detail)
    })?;
    let response = RefreshProviderModelsResponse {
        provider_id: provider_id.clone(),
        default_runtime_model_id: snapshot.capabilities.default_model_id.clone(),
        runtime_models: snapshot.capabilities.models,
        model_source: snapshot.model_source,
        models_refreshed_at: snapshot.models_refreshed_at,
    };
    if let Err(error) = app_state
        .audit_service
        .create_audit_log(&ProviderModelsRefreshedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            provider_id,
            model_count: response.runtime_models.len(),
            model_source: response.model_source,
        })
        .await
    {
        tracing::error!(%error, "failed to write AI provider model refresh audit log");
    }
    Ok(Json(response))
}

fn ensure_provider_runtime_permission(
    auth: &temps_auth::AuthContext,
    provider: &crate::ai_cli::catalog::ProviderCatalogEntry,
) -> Result<(), Problem> {
    match provider.host_access_requirement {
        HostAccessRequirement::AiGatewayWrite => permission_guard!(auth, AiGatewayWrite),
        HostAccessRequirement::SystemAdmin => permission_guard!(auth, SystemAdmin),
    }
    Ok(())
}

fn ensure_workspace_model_discovery_permission(
    auth: &temps_auth::AuthContext,
) -> Result<(), Problem> {
    permission_guard!(auth, SandboxesWrite);
    permission_guard!(auth, SandboxesExec);
    Ok(())
}

fn provider_model_refresh_error_detail(
    provider: &crate::ai_cli::catalog::ProviderCatalogEntry,
    provider_id: &str,
    workspace_discovery: bool,
    error: &temps_ai::AiError,
) -> String {
    if matches!(error, temps_ai::AiError::NotAvailable) {
        return format!(
            "{} is not installed and authenticated on the Temps host. Use the install and authentication commands shown above, then retry.",
            provider.name
        );
    }
    if workspace_discovery {
        return format!(
            "Temps could not start or inspect your persistent workspace, so {} models could not be resolved. Retry after the workspace is available; if it remains unavailable, contact your Temps administrator.",
            provider.name
        );
    }
    format!("Temps could not refresh the model inventory for '{provider_id}'. Check the provider authentication and retry.")
}

async fn provider_catalog_dto(
    entry: &'static crate::ai_cli::catalog::ProviderCatalogEntry,
    provider_cfg: temps_core::ProviderConfig,
    workspace_snapshot: Option<temps_ai::ProviderCapabilitiesSnapshot>,
    local_credential: Option<LocalCredentialSummary>,
) -> ProviderCatalogDto {
    let credential_saved = provider_cfg.credentials_encrypted.is_some();
    let current_auth_type = if credential_saved {
        Some(provider_cfg.auth_type.clone())
    } else {
        None
    };

    let (
        host_authenticated,
        host_auth_method,
        host_auth_hint,
        host_version,
        discovered_models,
        discovered_source,
        models_refreshed_at,
    ) = match crate::ai_cli::create_provider(entry.id) {
        Some(_provider) => {
            let status = crate::ai_cli::cached_status(entry.id).await;
            match status {
                Some(status) => {
                    let authenticated = status.installed && status.authenticated;
                    let version = status.version.clone();
                    let identity = format!(
                        "{}|{}|{}|{}",
                        version.as_deref().unwrap_or("unknown"),
                        status.auth_method.as_deref().unwrap_or("unknown"),
                        status.email.as_deref().unwrap_or("unknown"),
                        status.subscription_type.as_deref().unwrap_or("unknown")
                    );
                    let snapshot = if !status.installed {
                        None
                    } else {
                        crate::ai_cli::cached_model_capabilities(entry.id, &identity).await
                    };
                    let models = snapshot
                        .as_ref()
                        .map(|snapshot| snapshot.models.clone())
                        .unwrap_or_default();
                    (
                        authenticated,
                        status.auth_method,
                        if authenticated {
                            None
                        } else {
                            status.setup_hint
                        },
                        version,
                        models,
                        snapshot
                            .as_ref()
                            .map(|snapshot| snapshot.source)
                            .unwrap_or(temps_ai::ModelCatalogSource::Bootstrap),
                        snapshot.as_ref().and_then(|snapshot| {
                            (!snapshot.models.is_empty())
                                .then(|| snapshot.refreshed_at.to_rfc3339())
                        }),
                    )
                }
                None => (
                    false,
                    None,
                    Some(
                        "Host harness status has not been checked yet. Refresh to check it.".into(),
                    ),
                    None,
                    Vec::new(),
                    temps_ai::ModelCatalogSource::Bootstrap,
                    None,
                ),
            }
        }
        None => (
            false,
            None,
            None,
            None,
            Vec::new(),
            temps_ai::ModelCatalogSource::Bootstrap,
            None,
        ),
    };
    let mut dto = provider_catalog_dto_from_runtime(
        entry,
        provider_cfg,
        credential_saved,
        current_auth_type,
        host_authenticated,
        host_auth_method,
        host_auth_hint,
        host_version,
        discovered_models,
        discovered_source,
        models_refreshed_at,
        local_credential,
    );
    if let Some(snapshot) = workspace_snapshot {
        dto.models = snapshot
            .capabilities
            .models
            .iter()
            .map(|model| model.id.clone())
            .collect();
        dto.runtime_models = snapshot.capabilities.models;
        dto.default_runtime_model_id = dto
            .default_model
            .clone()
            .filter(|saved| dto.models.iter().any(|model| model == saved))
            .or(snapshot.capabilities.default_model_id);
        dto.model_source = snapshot.model_source;
        dto.models_refreshed_at = snapshot.models_refreshed_at;
        if entry.id == "opencode" && dto.workspace_ready {
            dto.workspace_ready = true;
            dto.workspace_readiness_hint = None;
        }
    }
    dto
}

#[allow(clippy::too_many_arguments)]
fn provider_catalog_dto_from_runtime(
    entry: &crate::ai_cli::catalog::ProviderCatalogEntry,
    provider_cfg: temps_core::ProviderConfig,
    credential_saved: bool,
    current_auth_type: Option<String>,
    host_authenticated: bool,
    host_auth_method: Option<String>,
    host_auth_hint: Option<String>,
    host_version: Option<String>,
    discovered_models: Vec<crate::ai_cli::AiCliModelCapability>,
    discovered_source: temps_ai::ModelCatalogSource,
    models_refreshed_at: Option<String>,
    local_credential: Option<LocalCredentialSummary>,
) -> ProviderCatalogDto {
    // OpenCode config files require semantic validation and a successful
    // native runtime probe. An encrypted blob alone must never be presented
    // as executable.
    let credential_verified = provider_cfg
        .extra
        .get("credential_verified")
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    let verified_opencode = entry.id == "opencode" && credential_verified;
    let credential_verification_status = if !credential_saved {
        "not_saved"
    } else if credential_verified {
        "verified"
    } else {
        "unverified"
    };
    let verification_hint = (credential_saved && !credential_verified && entry.id == "opencode")
        .then(|| "Saved, but Temps could not confirm a model response. Select an available model and verify it before using this workspace harness.".to_string());
    let workspace_ready = entry.workspace_chat_supported
        && credential_saved
        && (entry.id != "opencode" || verified_opencode);
    let workspace_readiness_hint = if workspace_ready {
        None
    } else if !entry.workspace_chat_supported {
        Some(format!(
            "{} is available for host workflows, but its secure persistent-workspace relay is not implemented yet.",
            entry.name
        ))
    } else if entry.id == "opencode" && credential_saved {
        verification_hint.clone()
    } else {
        Some(format!(
            "Save a {} credential to run this harness inside a persistent workspace.",
            entry.name
        ))
    };
    let (runtime_models, model_source) = if discovered_models.is_empty() {
        (
            bootstrap_runtime_models(entry.models),
            temps_ai::ModelCatalogSource::Bootstrap,
        )
    } else {
        (
            discovered_models
                .into_iter()
                .map(runtime_model_capability)
                .collect(),
            discovered_source,
        )
    };
    let models = runtime_models
        .iter()
        .map(|model| model.id.clone())
        .collect();
    let default_runtime_model_id = provider_cfg
        .default_model
        .clone()
        .filter(|model| runtime_models.iter().any(|option| option.id == *model))
        .or_else(|| runtime_models.first().map(|model| model.id.clone()));
    let permission_modes = entry
        .permission_modes
        .iter()
        .map(|mode| temps_ai::SelectOption {
            id: mode.id.to_string(),
            name: mode.name.to_string(),
            description: Some(mode.description.to_string()),
        })
        .collect();

    ProviderCatalogDto {
        id: entry.id.to_string(),
        name: entry.name.to_string(),
        install_command: entry.install_command.to_string(),
        auth_command: entry.auth_command.to_string(),
        auth_flavors: entry
            .auth_flavors
            .iter()
            .map(|f| AuthFlavorDto {
                id: f.id.to_string(),
                label: f.label.to_string(),
                description: f.description.to_string(),
                format: match f.format {
                    CredentialFormat::ApiKey => "api_key".to_string(),
                    CredentialFormat::OauthToken => "oauth_token".to_string(),
                    CredentialFormat::ConfigFile => "config_file".to_string(),
                },
                env_var: if matches!(f.format, CredentialFormat::ApiKey) {
                    Some(f.env_var.to_string())
                } else {
                    None
                },
            })
            .collect(),
        models,
        runtime_models,
        default_runtime_model_id,
        permission_modes,
        default_permission_mode_id: entry.default_permission_mode_id.to_string(),
        credential_saved,
        credential_verification_status: credential_verification_status.to_string(),
        verification_hint,
        current_auth_type,
        default_model: provider_cfg.default_model.clone(),
        max_turns_analysis: provider_cfg.max_turns_analysis,
        max_turns_fix: provider_cfg.max_turns_fix,
        max_turns_feedback: provider_cfg.max_turns_feedback,
        // Only Claude Code has a --max-turns flag today; Codex and
        // OpenCode run to completion regardless of these settings.
        supports_max_turns: entry.id == "claude_cli",
        host_authenticated,
        host_auth_method,
        host_version,
        model_source,
        models_refreshed_at,
        host_auth_hint,
        workspace_ready,
        workspace_readiness_hint,
        local_credential: local_credential.map(|credential| LocalCredentialDto {
            auth_type: credential.auth_type,
            source: credential.source.as_str().to_string(),
            label: credential.source.label().to_string(),
        }),
    }
}

/// Save (or replace) a provider's credential. The credential is encrypted
/// with `EncryptionService` and stored inside
/// `agent_sandbox.providers[provider_id].credentials_encrypted`.
///
/// The plaintext shape depends on the flavor's `credential_format`:
///   - `ApiKey` / `OauthToken`: the key/token string.
///   - `ConfigFile`: the full file body (e.g. OpenCode's `auth.json`).
#[utoipa::path(
    tag = "Agents",
    post,
    path = "/settings/ai-providers/{provider_id}/credential",
    params(("provider_id" = String, Path, description = "AI provider ID")),
    request_body = SaveCredentialRequest,
    responses(
        (status = 200, body = SaveCredentialResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn save_ai_provider_credential(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(provider_id): Path<String>,
    Json(request): Json<SaveCredentialRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    // Validate against the catalog before touching the database — keeps
    // bad data out of the JSON column.
    let provider = find_provider(&provider_id).ok_or_else(|| {
        Problem::from(AgentError::Validation {
            message: format!("Unknown AI provider '{}'", provider_id),
        })
    })?;
    if provider.flavor(&request.auth_type).is_none() {
        return Err(Problem::from(AgentError::Validation {
            message: format!(
                "Provider '{}' does not support auth_type '{}'",
                provider_id, request.auth_type
            ),
        }));
    }
    if request.credential.trim().is_empty() {
        return Err(Problem::from(AgentError::Validation {
            message: "Credential cannot be empty".into(),
        }));
    }
    if request.verification_model.is_some() && provider_id != "opencode" {
        return Err(Problem::from(AgentError::Validation {
            message: "An explicit verification model is currently supported only for OpenCode"
                .into(),
        }));
    }

    let verification = verify_candidate(
        &app_state,
        &auth,
        &provider_id,
        &request.auth_type,
        &request.credential,
        request.verification_model.as_deref(),
    )
    .await?;

    let encrypted = app_state
        .encryption_service
        .encrypt_string(&request.credential)
        .map_err(|e| {
            Problem::from(AgentError::EncryptionError {
                message: format!("Failed to encrypt credential: {}", e),
            })
        })?;

    persist_provider_credential_and_invalidate(
        app_state.platform_config_service.as_ref(),
        &provider_id,
        &request.auth_type,
        encrypted,
        verification,
        (verification == CredentialVerification::Verified)
            .then_some(request.verification_model.as_deref())
            .flatten(),
        None,
    )
    .await
    .map_err(provider_credential_persist_problem)?;
    if let Some(ai_service) = &app_state.ai_service {
        ai_service
            .invalidate_capabilities_for(Some(&provider_id))
            .await;
    }

    write_provider_credential_audit(
        &app_state,
        &auth,
        &metadata,
        &provider_id,
        &request.auth_type,
        "manual",
    )
    .await;

    let sandbox = load_agent_sandbox(&app_state).await?;
    let provider_dto =
        provider_catalog_dto(provider, sandbox.provider_config(&provider_id), None, None).await;
    Ok(Json(SaveCredentialResponse {
        saved: true,
        credential_verification_status: verification.as_str().to_string(),
        verification_hint: provider_dto.verification_hint.clone(),
        provider_id,
        auth_type: request.auth_type,
        provider: provider_dto,
    }))
}

/// Import the credential already used by this provider's CLI on the Temps
/// host. Discovery and encryption happen entirely server-side; the plaintext
/// credential is never serialized into the response or browser state.
#[utoipa::path(
    tag = "Agents",
    post,
    path = "/settings/ai-providers/{provider_id}/credential/import-local",
    params(("provider_id" = String, Path, description = "AI provider ID"), ImportLocalCredentialQuery),
    responses(
        (status = 200, body = ImportLocalCredentialResponse),
        (status = 400, description = "Unknown provider or invalid local credential"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "System administrator permission required"),
        (status = 404, description = "No importable local credential found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn import_local_ai_provider_credential(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(provider_id): Path<String>,
    Query(query): Query<ImportLocalCredentialQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    permission_guard!(auth, SystemAdmin);

    let provider = find_provider(&provider_id).ok_or_else(|| {
        Problem::from(AgentError::Validation {
            message: format!("Unknown AI provider '{provider_id}'"),
        })
    })?;
    if query.verification_model.is_some() && provider_id != "opencode" {
        return Err(Problem::from(AgentError::Validation {
            message: "An explicit verification model is currently supported only for OpenCode"
                .into(),
        }));
    }
    let discovered = discover_local_credential(provider)
        .await
        .map_err(|error| {
            problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Local AI credential could not be imported")
                .with_detail(error.to_string())
        })?
        .ok_or_else(|| {
            problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("No local AI credential found")
                .with_detail(format!(
                    "Temps could not find an importable {} credential for the operating-system user running this server. Authenticate the host CLI or save a credential manually.",
                    provider.name
                ))
        })?;

    let verification = verify_candidate(
        &app_state,
        &auth,
        &provider_id,
        &discovered.auth_type,
        &discovered.credential,
        query.verification_model.as_deref(),
    )
    .await?;

    let encrypted = app_state
        .encryption_service
        .encrypt_string(&discovered.credential)
        .map_err(|error| {
            Problem::from(AgentError::EncryptionError {
                message: format!(
                    "Failed to encrypt imported credential for provider '{provider_id}': {error}"
                ),
            })
        })?;
    persist_provider_credential_and_invalidate(
        app_state.platform_config_service.as_ref(),
        &provider_id,
        &discovered.auth_type,
        encrypted,
        verification,
        (verification == CredentialVerification::Verified)
            .then_some(query.verification_model.as_deref())
            .flatten(),
        None,
    )
    .await
    .map_err(provider_credential_persist_problem)?;
    if let Some(ai_service) = &app_state.ai_service {
        ai_service
            .invalidate_capabilities_for(Some(&provider_id))
            .await;
    }
    write_provider_credential_audit(
        &app_state,
        &auth,
        &metadata,
        &provider_id,
        &discovered.auth_type,
        discovered.source.as_str(),
    )
    .await;

    let sandbox = load_agent_sandbox(&app_state).await?;
    let provider_dto =
        provider_catalog_dto(provider, sandbox.provider_config(&provider_id), None, None).await;
    Ok(Json(ImportLocalCredentialResponse {
        saved: true,
        credential_verification_status: verification.as_str().to_string(),
        verification_hint: provider_dto.verification_hint.clone(),
        provider_id,
        auth_type: discovered.auth_type,
        source: discovered.source.as_str().to_string(),
        workspace_ready: provider_dto.workspace_ready,
        provider: provider_dto,
    }))
}

/// Verify a previously imported OpenCode credential against a user-selected
/// model without asking the browser to receive or resubmit the secret.
#[utoipa::path(
    tag = "Agents",
    post,
    path = "/settings/ai-providers/{provider_id}/credential/verify-saved",
    params(("provider_id" = String, Path, description = "AI provider ID")),
    request_body = VerifySavedCredentialRequest,
    responses(
        (status = 200, body = SaveCredentialResponse),
        (status = 400, description = "Invalid model or credential rejected"),
        (status = 404, description = "No saved credential"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn verify_saved_ai_provider_credential(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(provider_id): Path<String>,
    Json(request): Json<VerifySavedCredentialRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    if provider_id != "opencode" {
        return Err(Problem::from(AgentError::Validation {
            message: "Saved-credential model verification is currently supported only for OpenCode"
                .into(),
        }));
    }
    let provider = find_provider(&provider_id).ok_or_else(|| {
        Problem::from(AgentError::Validation {
            message: format!("Unknown AI provider '{provider_id}'"),
        })
    })?;
    let sandbox = load_agent_sandbox(&app_state).await?;
    let config = sandbox.provider_config(&provider_id);
    let encrypted = config.credentials_encrypted.as_ref().ok_or_else(|| {
        problemdetails::new(StatusCode::NOT_FOUND)
            .with_title("No saved AI credential")
            .with_detail(format!(
                "Provider '{provider_id}' has no saved credential to verify."
            ))
    })?;
    let credential = app_state
        .encryption_service
        .decrypt_string(encrypted)
        .map_err(|_| {
            problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                .with_title("Saved credential unavailable")
                .with_detail(format!(
                    "Provider '{provider_id}' saved credential could not be decrypted."
                ))
        })?;
    let auth_type = config.auth_type.clone();
    let verification = verify_candidate(
        &app_state,
        &auth,
        &provider_id,
        &auth_type,
        &credential,
        Some(&request.verification_model),
    )
    .await?;
    persist_provider_credential_and_invalidate(
        app_state.platform_config_service.as_ref(),
        &provider_id,
        &auth_type,
        encrypted.clone(),
        verification,
        (verification == CredentialVerification::Verified)
            .then_some(request.verification_model.as_str()),
        Some((&auth_type, encrypted.as_str())),
    )
    .await
    .map_err(provider_credential_persist_problem)?;
    if let Some(ai_service) = &app_state.ai_service {
        ai_service
            .invalidate_capabilities_for(Some(&provider_id))
            .await;
    }
    write_provider_credential_audit(
        &app_state,
        &auth,
        &metadata,
        &provider_id,
        &auth_type,
        "verify-saved",
    )
    .await;
    let sandbox = load_agent_sandbox(&app_state).await?;
    let provider_dto =
        provider_catalog_dto(provider, sandbox.provider_config(&provider_id), None, None).await;
    Ok(Json(SaveCredentialResponse {
        saved: true,
        credential_verification_status: verification.as_str().to_string(),
        verification_hint: provider_dto.verification_hint.clone(),
        provider_id,
        auth_type,
        provider: provider_dto,
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CredentialVerification {
    Verified,
    Unverified,
}

impl CredentialVerification {
    fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Unverified => "unverified",
        }
    }
}

fn opencode_probe_is_inconclusive(provider_id: &str, error: &temps_ai::AiError) -> bool {
    provider_id == "opencode"
        && (matches!(error, temps_ai::AiError::CredentialVerification { .. })
            || matches!(error,
                temps_ai::AiError::Provider { purpose, .. }
                    if matches!(purpose.as_str(), "provider.credentials.verify" | "provider.credentials.verify.model" | "provider.credentials.verify.allowance")
            ))
}

fn credential_verification_log_fields(error: &temps_ai::AiError) -> (&'static str, &'static str) {
    match error {
        temps_ai::AiError::CredentialVerification {
            stage, diagnostic, ..
        } => (
            match stage {
                temps_ai::CredentialVerificationStage::SandboxUnavailable => "sandbox_unavailable",
                temps_ai::CredentialVerificationStage::SandboxStartup => "sandbox_startup",
                temps_ai::CredentialVerificationStage::SandboxStartupTimeout => {
                    "sandbox_startup_timeout"
                }
                temps_ai::CredentialVerificationStage::RelayUnavailable => "relay_unavailable",
                temps_ai::CredentialVerificationStage::CapabilityStaging => "capability_staging",
                temps_ai::CredentialVerificationStage::HarnessExecution => "harness_execution",
                temps_ai::CredentialVerificationStage::HarnessTimeout => "harness_timeout",
                temps_ai::CredentialVerificationStage::HarnessIncompatible => {
                    "harness_incompatible"
                }
            },
            match diagnostic {
                temps_ai::CredentialVerificationDiagnostic::RuntimeUnavailable => {
                    "runtime_unavailable"
                }
                temps_ai::CredentialVerificationDiagnostic::ImageUnavailable => "image_unavailable",
                temps_ai::CredentialVerificationDiagnostic::NetworkUnavailable => {
                    "network_unavailable"
                }
                temps_ai::CredentialVerificationDiagnostic::OperationFailed => "operation_failed",
            },
        ),
        temps_ai::AiError::Provider { purpose, .. } => (
            "provider",
            match purpose.as_str() {
                "provider.credentials.verify.invalid" => "invalid",
                "provider.credentials.verify.auth" => "auth",
                "provider.credentials.verify.allowance" => "allowance",
                "provider.credentials.verify.model" => "model",
                _ => "other",
            },
        ),
        temps_ai::AiError::NotAvailable => ("service", "not_available"),
        temps_ai::AiError::NoModel { .. } => ("service", "no_model"),
        temps_ai::AiError::RetainedHarnessDiagnostic { .. } => ("service", "retained_diagnostic"),
    }
}

async fn verify_candidate(
    app_state: &Arc<AppState>,
    auth: &temps_auth::AuthContext,
    provider_id: &str,
    auth_type: &str,
    credential: &str,
    verification_model: Option<&str>,
) -> Result<CredentialVerification, Problem> {
    ensure_workspace_model_discovery_permission(auth)?;
    let ai_service = app_state.ai_service.as_ref().ok_or_else(|| {
        problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
            .with_title("Credential verification unavailable")
            .with_detail("The secure AI harness service is not configured on this Temps instance.")
    })?;
    match ai_service
        .verify_candidate_credential_with_model(
            provider_id,
            auth_type,
            credential,
            auth.user_id(),
            verification_model,
        )
        .await
    {
        Ok(()) => Ok(CredentialVerification::Verified),
        Err(error) if opencode_probe_is_inconclusive(provider_id, &error) => {
            let (failure_stage, diagnostic) = credential_verification_log_fields(&error);
            tracing::warn!(
                provider_id,
                auth_type,
                failure_stage,
                diagnostic,
                "OpenCode credential saved without model verification"
            );
            Ok(CredentialVerification::Unverified)
        }
        Err(error) => {
            let (failure_stage, diagnostic) = credential_verification_log_fields(&error);
            tracing::warn!(
                provider_id,
                auth_type,
                failure_stage,
                diagnostic,
                "candidate credential verification failed"
            );
            Err(credential_verification_problem(
                provider_id,
                auth_type,
                &error,
            ))
        }
    }
}

fn credential_verification_problem(
    provider_id: &str,
    auth_type: &str,
    error: &temps_ai::AiError,
) -> Problem {
    let (status, title, guidance) = credential_verification_failure(error);
    problemdetails::new(status)
        .with_title(title)
        .with_detail(format!("Provider '{provider_id}' ({auth_type}): {guidance} A previously saved credential was not changed."))
}

fn credential_verification_failure(
    error: &temps_ai::AiError,
) -> (StatusCode, &'static str, &'static str) {
    match error {
        temps_ai::AiError::CredentialVerification {
            stage,
            diagnostic,
            ..
        } => match (stage, diagnostic) {
            (
                temps_ai::CredentialVerificationStage::SandboxStartup,
                temps_ai::CredentialVerificationDiagnostic::ImageUnavailable,
            ) => (
                StatusCode::SERVICE_UNAVAILABLE, "Verification sandbox image unavailable", "Temps could not validate this credential because the configured managed sandbox image is unavailable. Check the image configuration and registry access, then retry; the credential has not been classified as invalid.",
            ),
            (
                temps_ai::CredentialVerificationStage::SandboxStartup,
                temps_ai::CredentialVerificationDiagnostic::RuntimeUnavailable,
            ) => (
                StatusCode::SERVICE_UNAVAILABLE, "Verification sandbox runtime unavailable", "Temps could not validate this credential because the configured sandbox runtime is unavailable. Restore the runtime and retry; the credential has not been classified as invalid.",
            ),
            (temps_ai::CredentialVerificationStage::SandboxUnavailable, _) => (
                StatusCode::SERVICE_UNAVAILABLE, "Verification sandbox unavailable", "Temps could not validate this credential because an isolated Docker or VM runtime is unavailable. Configure the sandbox runtime and retry; the credential has not been classified as invalid.",
            ),
            (temps_ai::CredentialVerificationStage::SandboxStartup, _) => (
                StatusCode::SERVICE_UNAVAILABLE, "Verification sandbox could not start", "Temps could not validate this credential because the isolated sandbox failed to start. Check the sandbox runtime logs and retry; the credential has not been classified as invalid.",
            ),
            (temps_ai::CredentialVerificationStage::SandboxStartupTimeout, _) => (
                StatusCode::GATEWAY_TIMEOUT, "Verification sandbox startup timed out", "Temps could not validate this credential before the isolated sandbox startup deadline. Check sandbox runtime capacity and retry; the credential has not been classified as invalid.",
            ),
            (temps_ai::CredentialVerificationStage::RelayUnavailable, _) => (
                StatusCode::SERVICE_UNAVAILABLE, "Verification relay unavailable", "Temps could not validate this credential because the model relay or upstream provider was unavailable. Check relay connectivity and provider status, then retry; the credential has not been classified as invalid.",
            ),
            (temps_ai::CredentialVerificationStage::CapabilityStaging, _) => (
                StatusCode::SERVICE_UNAVAILABLE, "Verification setup failed", "Temps could not validate this credential because the temporary relay capability could not be staged in the sandbox. Check sandbox file access and retry; the credential has not been classified as invalid.",
            ),
            (temps_ai::CredentialVerificationStage::HarnessExecution, _) => (
                StatusCode::SERVICE_UNAVAILABLE, "Verification harness could not run", "Temps could not validate this credential because the native verification command could not be executed. Check the sandbox runtime and installed harness, then retry; the credential has not been classified as invalid.",
            ),
            (temps_ai::CredentialVerificationStage::HarnessTimeout, _) => (
                StatusCode::GATEWAY_TIMEOUT, "Verification harness timed out", "Temps could not validate this credential before the verification deadline. Check sandbox and model-relay availability, then retry; the credential has not been classified as invalid.",
            ),
            (temps_ai::CredentialVerificationStage::HarnessIncompatible, _) => (
                StatusCode::SERVICE_UNAVAILABLE, "Verification harness is incompatible", "Temps could not validate this credential because the sandbox image is missing a compatible native harness or required safe flags. Update the managed sandbox image and retry; the credential has not been classified as invalid.",
            ),
        },
        temps_ai::AiError::Provider { purpose, .. } if purpose == "provider.credentials.verify.invalid" => (
            StatusCode::BAD_REQUEST, "Invalid provider credential", "The credential is malformed or uses an unsupported authentication format. Check its contents and retry.",
        ),
        temps_ai::AiError::Provider { purpose, .. } if purpose == "provider.credentials.verify.auth" => (
            StatusCode::BAD_REQUEST, "Credential rejected by provider", "The provider rejected this credential or denied model access. Refresh the login or key and retry.",
        ),
        temps_ai::AiError::Provider { purpose, .. } if purpose == "provider.credentials.verify.allowance" => (
            StatusCode::TOO_MANY_REQUESTS, "Provider allowance unavailable", "The account has insufficient allowance or is rate limited. Restore allowance and retry.",
        ),
        temps_ai::AiError::Provider { purpose, .. } if purpose == "provider.credentials.verify.model" => (
            StatusCode::BAD_REQUEST, "Harness model request failed", "The harness could not complete a minimal model request with this credential. Check its model access and retry.",
        ),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE, "Harness verification unavailable", "Temps could not complete the isolated harness check. Check sandbox/runtime availability and retry.",
        ),
    }
}

async fn write_provider_credential_audit(
    app_state: &Arc<AppState>,
    auth: &temps_auth::AuthContext,
    metadata: &RequestMetadata,
    provider_id: &str,
    auth_type: &str,
    source: &str,
) {
    if let Err(error) = app_state
        .audit_service
        .create_audit_log(&ProviderCredentialSavedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            provider_id: provider_id.to_string(),
            auth_type: auth_type.to_string(),
            source: source.to_string(),
        })
        .await
    {
        tracing::error!(
            provider_id,
            %error,
            "failed to write AI provider credential audit log"
        );
    }
}

async fn write_provider_settings_audit(
    app_state: &Arc<AppState>,
    auth: &temps_auth::AuthContext,
    metadata: &RequestMetadata,
    provider_id: &str,
    change: &str,
) {
    if let Err(error) = app_state
        .audit_service
        .create_audit_log(&ProviderSettingsChangedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            provider_id: provider_id.into(),
            change: change.into(),
        })
        .await
    {
        tracing::error!(provider_id, %error, "failed to write AI provider settings audit log");
    }
}

/// Persist a provider credential and make the successful write a strict cache
/// freshness boundary. Keeping both operations in one function prevents a
/// future handler edit from accidentally restoring the old-token-for-one-turn
/// behavior.
async fn persist_provider_credential_and_invalidate(
    platform_config_service: &temps_config::ConfigService,
    provider_id: &str,
    auth_type: &str,
    encrypted: String,
    verification: CredentialVerification,
    verified_default_model: Option<&str>,
    expected: Option<(&str, &str)>,
) -> Result<(), temps_config::ConfigServiceError> {
    platform_config_service
        .update_agent_provider_credential(
            provider_id,
            auth_type,
            &encrypted,
            verification == CredentialVerification::Verified,
            verified_default_model,
            expected,
        )
        .await
}

fn provider_credential_persist_problem(error: temps_config::ConfigServiceError) -> Problem {
    match error {
        temps_config::ConfigServiceError::ProviderCredentialChanged { provider_id } =>
            problemdetails::new(StatusCode::CONFLICT)
                .with_title("Saved AI credential changed")
                .with_detail(format!("Provider '{provider_id}' credential changed while verification ran. Retry against the current saved credential.")),
        temps_config::ConfigServiceError::InvalidConfiguration { details } =>
            problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid AI provider settings")
                .with_detail(details),
        other => {
            tracing::error!(error_kind = ?std::mem::discriminant(&other), "AI credential settings update failed");
            problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                .with_title("AI credential could not be saved")
                .with_detail("The settings database could not save this provider credential. Retry or contact your Temps administrator.")
        },
    }
}

/// Activate a provider as the platform-wide default. Refuses to activate a
/// provider that doesn't have a credential saved yet — the UI enforces the
/// same rule on the button, but we re-check server-side so a stale tab
/// can't bypass it.
#[utoipa::path(
    tag = "Agents",
    post,
    path = "/settings/ai-providers/{provider_id}/activate",
    params(("provider_id" = String, Path, description = "AI provider ID")),
    responses(
        (status = 200, body = ActivateProviderResponse),
        (status = 400, description = "Provider not configured"),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn activate_ai_provider(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(provider_id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    if find_provider(&provider_id).is_none() {
        return Err(Problem::from(AgentError::Validation {
            message: format!("Unknown AI provider '{}'", provider_id),
        }));
    }

    app_state
        .platform_config_service
        .activate_agent_provider(&provider_id)
        .await
        .map_err(provider_credential_persist_problem)?;
    write_provider_settings_audit(&app_state, &auth, &metadata, &provider_id, "activate").await;

    Ok(Json(ActivateProviderResponse {
        default_provider: provider_id,
    }))
}

/// Update provider-scoped settings without touching the saved credential.
/// Today that means just `default_model`; future per-provider settings
/// (base URL overrides, request headers, etc.) can land here too without
/// changing the shape of `save_credential`.
#[utoipa::path(
    tag = "Agents",
    patch,
    path = "/settings/ai-providers/{provider_id}",
    params(("provider_id" = String, Path, description = "AI provider ID")),
    request_body = UpdateProviderRequest,
    responses(
        (status = 200, body = UpdateProviderResponse),
        (status = 400, description = "Unknown provider"),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_ai_provider(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(provider_id): Path<String>,
    Json(request): Json<UpdateProviderRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    let provider = find_provider(&provider_id).ok_or_else(|| {
        Problem::from(AgentError::Validation {
            message: format!("Unknown AI provider '{}'", provider_id),
        })
    })?;

    // Validate turn caps up front: 0 = clear, 1..=200 = set, else reject.
    for (field, value) in [
        ("max_turns_analysis", request.max_turns_analysis),
        ("max_turns_fix", request.max_turns_fix),
        ("max_turns_feedback", request.max_turns_feedback),
    ] {
        if let Some(v) = value {
            if v != 0 && !(1..=200).contains(&v) {
                return Err(Problem::from(AgentError::Validation {
                    message: format!(
                        "{} for provider '{}' must be between 1 and 200 (or 0 to clear), got {}",
                        field, provider_id, v
                    ),
                }));
            }
        }
    }

    // Normalize the incoming model: empty string → None (clear the field).
    // The catalog `models` list is a convenience, not an allowlist — CLIs
    // (especially free-form ones like OpenCode) evolve faster than this
    // table, so we accept unknown ids. But the stored value is read back at
    // run time and, for OpenCode, interpolated into a `bash -lc` string, so
    // we reject shell metacharacters and cap length here to close the stored
    // command-injection vector. Real model ids use only [A-Za-z0-9._/-:].
    let new_model = match request.default_model.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(m) => {
            if m.len() > 128 {
                return Err(Problem::from(AgentError::Validation {
                    message: format!(
                        "model id is too long ({} chars, max 128) for provider '{}'",
                        m.len(),
                        provider_id
                    ),
                }));
            }
            if !m
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-' | ':'))
            {
                return Err(Problem::from(AgentError::Validation {
                    message: format!(
                        "model id '{}' for provider '{}' contains invalid characters (allowed: letters, digits, . _ / - :)",
                        m, provider_id
                    ),
                }));
            }
            Some(m.to_string())
        }
    };

    let [effective_max_turns_analysis, effective_max_turns_fix, effective_max_turns_feedback] =
        app_state
            .platform_config_service
            .update_agent_provider_preferences(
                &provider_id,
                provider.default_flavor().id,
                new_model.as_deref(),
                [
                    request.max_turns_analysis,
                    request.max_turns_fix,
                    request.max_turns_feedback,
                ],
            )
            .await
            .map_err(provider_credential_persist_problem)?;
    write_provider_settings_audit(&app_state, &auth, &metadata, &provider_id, "preferences").await;

    Ok(Json(UpdateProviderResponse {
        provider_id,
        default_model: new_model,
        max_turns_analysis: effective_max_turns_analysis,
        max_turns_fix: effective_max_turns_fix,
        max_turns_feedback: effective_max_turns_feedback,
    }))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Read `agent_sandbox` from settings, deserializing to the typed struct so
/// `provider_config()` and `default_provider` work correctly. Returns the
/// default settings when no row exists yet.
async fn load_agent_sandbox(
    app_state: &Arc<AppState>,
) -> Result<temps_core::AgentSandboxSettings, Problem> {
    let record = temps_entities::settings::Entity::find_by_id(1)
        .one(app_state.db.as_ref())
        .await
        .map_err(|e| Problem::from(AgentError::Database(e)))?;

    let sandbox = record
        .as_ref()
        .and_then(|r| r.data.get("agent_sandbox"))
        .and_then(|v| serde_json::from_value::<temps_core::AgentSandboxSettings>(v.clone()).ok())
        .unwrap_or_default();

    Ok(sandbox)
}

fn runtime_model_capability(
    model: crate::ai_cli::AiCliModelCapability,
) -> temps_ai::ModelCapability {
    let default_thinking_mode_id = model.default_reasoning_option;
    temps_ai::ModelCapability {
        id: model.id,
        name: model.name,
        thinking_modes: model
            .reasoning_options
            .into_iter()
            .map(|id| temps_ai::SelectOption {
                name: runtime_option_name(&id),
                id,
                description: Some("Supported by this model".to_string()),
            })
            .collect(),
        tool_thinking_modes: None,
        default_thinking_mode_id,
    }
}

fn bootstrap_runtime_models(models: &[&str]) -> Vec<temps_ai::ModelCapability> {
    models
        .iter()
        .map(|model| temps_ai::ModelCapability {
            id: (*model).to_string(),
            name: runtime_option_name(model),
            thinking_modes: Vec::new(),
            tool_thinking_modes: None,
            default_thinking_mode_id: None,
        })
        .collect()
}

fn runtime_option_name(id: &str) -> String {
    match id {
        "xhigh" => "Extra high".to_string(),
        value => {
            let mut characters = value.chars();
            characters
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + characters.as_str())
                .unwrap_or_default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_config::ServerConfig;
    use temps_core::{AppSettings, ProviderConfig};

    #[test]
    fn candidate_verification_errors_are_safe_and_actionable() {
        let auth = temps_ai::AiError::Provider {
            purpose: "provider.credentials.verify.auth".into(),
            reason: "secret-shaped-provider-response-must-not-be-rendered".into(),
        };
        let (status, title, guidance) = credential_verification_failure(&auth);
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(title, "Credential rejected by provider");
        assert!(!guidance.contains("secret-shaped"));
        let runtime = temps_ai::AiError::Provider {
            purpose: "provider.credentials.verify".into(),
            reason: "sandbox unavailable".into(),
        };
        assert_eq!(
            credential_verification_failure(&runtime).0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        let quota = temps_ai::AiError::Provider {
            purpose: "provider.credentials.verify.allowance".into(),
            reason: "rate limited".into(),
        };
        assert_eq!(
            credential_verification_failure(&quota).0,
            StatusCode::TOO_MANY_REQUESTS
        );

        let secret = "candidate-secret-must-never-leak";
        let cases = [
            (
                temps_ai::CredentialVerificationStage::SandboxUnavailable,
                temps_ai::CredentialVerificationDiagnostic::RuntimeUnavailable,
                StatusCode::SERVICE_UNAVAILABLE,
                "Verification sandbox unavailable",
            ),
            (
                temps_ai::CredentialVerificationStage::SandboxStartup,
                temps_ai::CredentialVerificationDiagnostic::ImageUnavailable,
                StatusCode::SERVICE_UNAVAILABLE,
                "Verification sandbox image unavailable",
            ),
            (
                temps_ai::CredentialVerificationStage::SandboxStartup,
                temps_ai::CredentialVerificationDiagnostic::RuntimeUnavailable,
                StatusCode::SERVICE_UNAVAILABLE,
                "Verification sandbox runtime unavailable",
            ),
            (
                temps_ai::CredentialVerificationStage::SandboxStartupTimeout,
                temps_ai::CredentialVerificationDiagnostic::OperationFailed,
                StatusCode::GATEWAY_TIMEOUT,
                "Verification sandbox startup timed out",
            ),
            (
                temps_ai::CredentialVerificationStage::CapabilityStaging,
                temps_ai::CredentialVerificationDiagnostic::OperationFailed,
                StatusCode::SERVICE_UNAVAILABLE,
                "Verification setup failed",
            ),
            (
                temps_ai::CredentialVerificationStage::RelayUnavailable,
                temps_ai::CredentialVerificationDiagnostic::NetworkUnavailable,
                StatusCode::SERVICE_UNAVAILABLE,
                "Verification relay unavailable",
            ),
            (
                temps_ai::CredentialVerificationStage::HarnessTimeout,
                temps_ai::CredentialVerificationDiagnostic::OperationFailed,
                StatusCode::GATEWAY_TIMEOUT,
                "Verification harness timed out",
            ),
            (
                temps_ai::CredentialVerificationStage::HarnessExecution,
                temps_ai::CredentialVerificationDiagnostic::OperationFailed,
                StatusCode::SERVICE_UNAVAILABLE,
                "Verification harness could not run",
            ),
            (
                temps_ai::CredentialVerificationStage::HarnessIncompatible,
                temps_ai::CredentialVerificationDiagnostic::OperationFailed,
                StatusCode::SERVICE_UNAVAILABLE,
                "Verification harness is incompatible",
            ),
        ];
        for (stage, diagnostic, expected_status, expected_title) in cases {
            let error = temps_ai::AiError::CredentialVerification {
                provider: secret.into(),
                stage,
                diagnostic,
            };
            let (status, title, guidance) = credential_verification_failure(&error);
            assert_eq!(status, expected_status);
            assert_eq!(title, expected_title);
            assert!(guidance.contains("could not validate this credential"));
            assert!(guidance.contains("has not been classified as invalid"));
            assert!(!guidance.contains(secret));
            let (logged_stage, logged_diagnostic) = credential_verification_log_fields(&error);
            assert_ne!(logged_stage, "service");
            assert_ne!(logged_diagnostic, secret);
        }
    }

    #[tokio::test]
    async fn verification_problem_response_is_sanitized_and_preserves_actionable_detail() {
        let error = temps_ai::AiError::CredentialVerification {
            provider: "candidate-secret-must-never-leak".into(),
            stage: temps_ai::CredentialVerificationStage::RelayUnavailable,
            diagnostic: temps_ai::CredentialVerificationDiagnostic::NetworkUnavailable,
        };
        let response =
            credential_verification_problem("codex_cli", "oauth", &error).into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(response.into_body(), 16 * 1024)
            .await
            .expect("problem body");
        let body = String::from_utf8(body.to_vec()).expect("UTF-8 problem body");
        assert!(body.contains("Verification relay unavailable"));
        assert!(body.contains("could not validate this credential"));
        assert!(body.contains("has not been classified as invalid"));
        assert!(!body.contains("candidate-secret-must-never-leak"));
    }

    #[test]
    fn activating_without_a_credential_remains_a_bad_request() {
        let problem = provider_credential_persist_problem(
            temps_config::ConfigServiceError::InvalidConfiguration {
                details: "Provider 'opencode' has no saved credential".into(),
            },
        );
        let response = problem.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn only_opencode_probe_failures_can_be_saved_unverified() {
        for purpose in [
            "provider.credentials.verify",
            "provider.credentials.verify.model",
            "provider.credentials.verify.allowance",
        ] {
            let error = temps_ai::AiError::Provider {
                purpose: purpose.into(),
                reason: "secret must stay private".into(),
            };
            assert!(
                opencode_probe_is_inconclusive("opencode", &error),
                "{purpose}"
            );
            assert!(
                !opencode_probe_is_inconclusive("claude_cli", &error),
                "{purpose}"
            );
            assert!(
                !opencode_probe_is_inconclusive("codex_cli", &error),
                "{purpose}"
            );
        }
        let infrastructure_error = temps_ai::AiError::CredentialVerification {
            provider: "opencode".into(),
            stage: temps_ai::CredentialVerificationStage::SandboxStartup,
            diagnostic: temps_ai::CredentialVerificationDiagnostic::OperationFailed,
        };
        assert!(opencode_probe_is_inconclusive(
            "opencode",
            &infrastructure_error
        ));
        assert!(!opencode_probe_is_inconclusive(
            "codex_cli",
            &infrastructure_error
        ));
        for purpose in [
            "provider.credentials.verify.auth",
            "provider.credentials.verify.invalid",
            "provider.credentials.verify.setup",
            "provider.credentials.verify.cleanup",
            "chat.application.credentials",
        ] {
            let error = temps_ai::AiError::Provider {
                purpose: purpose.into(),
                reason: "invalid".into(),
            };
            assert!(
                !opencode_probe_is_inconclusive("opencode", &error),
                "{purpose}"
            );
        }
    }

    #[test]
    fn unverified_opencode_is_visible_but_not_workspace_ready() {
        let opencode = find_provider("opencode").expect("OpenCode catalog entry");
        let config = ProviderConfig {
            extra: serde_json::json!({ "credential_verified": false }),
            ..ProviderConfig::default()
        };
        let dto = provider_catalog_dto_from_runtime(
            opencode,
            config,
            true,
            Some("config_file".into()),
            false,
            None,
            None,
            None,
            Vec::new(),
            temps_ai::ModelCatalogSource::Bootstrap,
            None,
            None,
        );
        assert!(dto.credential_saved);
        assert_eq!(dto.credential_verification_status, "unverified");
        assert!(!dto.workspace_ready);
        assert!(dto
            .verification_hint
            .as_deref()
            .is_some_and(|hint| hint.contains("Select an available model")));
    }

    #[test]
    fn local_import_can_request_a_specific_opencode_model_without_a_credential_body() {
        let query: ImportLocalCredentialQuery = serde_json::from_value(serde_json::json!({
            "verification_model": "anthropic/claude-sonnet-4-5"
        }))
        .expect("model-only import query");
        assert_eq!(
            query.verification_model.as_deref(),
            Some("anthropic/claude-sonnet-4-5")
        );
        let default_query: ImportLocalCredentialQuery =
            serde_json::from_value(serde_json::json!({}))
                .expect("existing bodyless import remains valid");
        assert!(default_query.verification_model.is_none());
    }

    #[test]
    fn verify_saved_request_accepts_only_a_model_not_a_secret() {
        let request: VerifySavedCredentialRequest = serde_json::from_value(serde_json::json!({
            "verification_model": "anthropic/claude-sonnet-4-5"
        }))
        .expect("model-only verification request");
        assert_eq!(request.verification_model, "anthropic/claude-sonnet-4-5");
        assert!(
            serde_json::from_value::<VerifySavedCredentialRequest>(serde_json::json!({})).is_err()
        );
    }

    fn provider_settings_row(encrypted: &str) -> temps_entities::settings::Model {
        let mut settings = AppSettings::default();
        settings.agent_sandbox.providers.insert(
            "claude_cli".to_string(),
            ProviderConfig {
                auth_type: "subscription".to_string(),
                credentials_encrypted: Some(encrypted.to_string()),
                ..ProviderConfig::default()
            },
        );
        temps_entities::settings::Model {
            id: 1,
            data: settings.to_json(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn provider_test_server_config() -> Arc<ServerConfig> {
        Arc::new(
            ServerConfig::new(
                "127.0.0.1:3000".to_string(),
                "postgresql://test".to_string(),
                None,
                Some("127.0.0.1:8000".to_string()),
            )
            .expect("valid test server config"),
        )
    }

    #[tokio::test]
    async fn verify_saved_cannot_restore_a_replaced_credential() {
        let replacement = provider_settings_row("encrypted-new-token");
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([[replacement]])
                .into_connection(),
        );
        let service = temps_config::ConfigService::new(provider_test_server_config(), db.clone());
        let error = persist_provider_credential_and_invalidate(
            &service,
            "claude_cli",
            "subscription",
            "encrypted-old-token".into(),
            CredentialVerification::Verified,
            Some("anthropic/claude-sonnet-4-5"),
            Some(("subscription", "encrypted-old-token")),
        )
        .await
        .expect_err("concurrent replacement must conflict");
        assert!(
            matches!(error, temps_config::ConfigServiceError::ProviderCredentialChanged { provider_id } if provider_id == "claude_cli")
        );
        drop(service);
        let transactions = Arc::try_unwrap(db)
            .expect("release db")
            .into_transaction_log();
        let sql = transactions
            .iter()
            .flat_map(|transaction| transaction.statements())
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !sql.contains("UPDATE \"settings\""),
            "stale verifier must not write the old credential"
        );
        assert!(!sql.contains("encrypted-old-token"));
    }

    #[test]
    fn provider_catalog_reads_never_accept_a_process_spawning_refresh_flag() {
        let query = serde_json::from_value::<ListAiProvidersQuery>(serde_json::json!({}))
            .expect("default query");
        assert!(!query.catalog_only);

        let ignored_legacy_refresh = serde_json::from_value::<ListAiProvidersQuery>(
            serde_json::json!({ "refresh_models": true, "catalog_only": true }),
        )
        .expect("unknown query fields are harmless");
        assert!(ignored_legacy_refresh.catalog_only);
    }

    #[test]
    fn local_credential_discovery_requires_a_full_admin_catalog_request() {
        assert!(should_discover_local_credentials(false, true));
        assert!(!should_discover_local_credentials(true, true));
        assert!(!should_discover_local_credentials(false, false));
        assert!(!should_discover_local_credentials(true, false));
    }

    #[tokio::test]
    async fn saving_provider_credential_invalidates_a_primed_old_credential() {
        let old_row = provider_settings_row("encrypted-old-token");
        let new_row = provider_settings_row("encrypted-new-token");
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([[old_row.clone()], [old_row], [new_row.clone()], [new_row]])
                .append_exec_results([sea_orm::MockExecResult {
                    last_insert_id: 1,
                    rows_affected: 1,
                }])
                .into_connection(),
        );
        let config_service =
            temps_config::ConfigService::new(provider_test_server_config(), db.clone());

        let cached = config_service
            .get_settings()
            .await
            .expect("prime settings cache");
        assert_eq!(
            cached
                .agent_sandbox
                .provider_config("claude_cli")
                .credentials_encrypted
                .as_deref(),
            Some("encrypted-old-token")
        );

        persist_provider_credential_and_invalidate(
            &config_service,
            "claude_cli",
            "subscription",
            "encrypted-new-token".to_string(),
            CredentialVerification::Verified,
            Some("anthropic/claude-sonnet-4-5"),
            None,
        )
        .await
        .expect("save replacement credential");

        let refreshed = config_service
            .get_settings()
            .await
            .expect("resolve settings for next turn");
        assert_eq!(
            refreshed
                .agent_sandbox
                .provider_config("claude_cli")
                .credentials_encrypted
                .as_deref(),
            Some("encrypted-new-token"),
            "the first resolver read after save must not return the primed credential"
        );

        drop(config_service);
        let statements = Arc::try_unwrap(db)
            .expect("test should release database connection")
            .into_transaction_log();
        let sql = statements
            .iter()
            .flat_map(|transaction| transaction.statements())
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            sql.contains("encrypted-new-token"),
            "the persisted settings update must contain the replacement credential"
        );
        assert!(
            sql.contains("anthropic/claude-sonnet-4-5"),
            "verified model must be persisted as default"
        );
    }

    #[test]
    fn runtime_capabilities_preserve_resolved_model_names() {
        let capability = runtime_model_capability(crate::ai_cli::AiCliModelCapability {
            id: "default".to_string(),
            name: "Opus 5".to_string(),
            reasoning_options: vec!["medium".to_string(), "xhigh".to_string()],
            default_reasoning_option: Some("medium".to_string()),
        });

        assert_eq!(capability.id, "default");
        assert_eq!(capability.name, "Opus 5");
        assert_eq!(capability.thinking_modes[0].name, "Medium");
        assert_eq!(capability.thinking_modes[1].name, "Extra high");
        assert_eq!(
            capability.default_thinking_mode_id.as_deref(),
            Some("medium")
        );
    }

    #[test]
    fn bootstrap_models_are_explicit_instead_of_default_sentinels() {
        let models = bootstrap_runtime_models(&["sonnet", "opus"]);
        assert_eq!(models[0].id, "sonnet");
        assert_eq!(models[0].name, "Sonnet");
    }

    #[test]
    fn cold_claude_catalog_does_not_advertise_unverified_models() {
        let claude =
            crate::ai_cli::catalog::find_provider("claude_cli").expect("Claude catalog entry");
        let dto = provider_catalog_dto_from_runtime(
            claude,
            temps_core::ProviderConfig::default(),
            true,
            Some("subscription".to_string()),
            false,
            None,
            None,
            None,
            Vec::new(),
            temps_ai::ModelCatalogSource::Bootstrap,
            None,
            None,
        );

        assert!(dto.models.is_empty());
        assert!(dto.runtime_models.is_empty());
        assert!(dto.default_runtime_model_id.is_none());
    }

    #[tokio::test]
    async fn workspace_models_overlay_a_cold_host_status_cache() {
        crate::ai_cli::invalidate_status_cache().await;
        let claude =
            crate::ai_cli::catalog::find_provider("claude_cli").expect("Claude catalog entry");
        let provider_config = temps_core::ProviderConfig {
            auth_type: "subscription".to_string(),
            credentials_encrypted: Some("encrypted-token".to_string()),
            ..Default::default()
        };
        let capabilities = crate::ai_cli::provider_capabilities_from_models(
            "claude_cli",
            vec![crate::ai_cli::AiCliModelCapability {
                id: "account-model".to_string(),
                name: "Account model".to_string(),
                reasoning_options: Vec::new(),
                default_reasoning_option: None,
            }],
        )
        .expect("Claude capability contract");

        let dto = provider_catalog_dto(
            claude,
            provider_config,
            Some(temps_ai::ProviderCapabilitiesSnapshot {
                capabilities,
                model_source: temps_ai::ModelCatalogSource::Live,
                models_refreshed_at: Some("2026-09-08T00:00:00Z".to_string()),
            }),
            None,
        )
        .await;

        assert_eq!(dto.model_source, temps_ai::ModelCatalogSource::Live);
        assert_eq!(dto.models, vec!["account-model"]);
        assert_eq!(
            dto.models_refreshed_at.as_deref(),
            Some("2026-09-08T00:00:00Z")
        );
    }

    #[test]
    fn workspace_capable_providers_discover_models_only_with_a_saved_credential() {
        let claude =
            crate::ai_cli::catalog::find_provider("claude_cli").expect("Claude catalog entry");
        assert!(!uses_workspace_model_discovery(
            claude,
            &temps_core::ProviderConfig::default()
        ));

        let configured = temps_core::ProviderConfig {
            credentials_encrypted: Some("encrypted-token".to_string()),
            ..temps_core::ProviderConfig::default()
        };
        assert!(uses_workspace_model_discovery(claude, &configured));

        let codex =
            crate::ai_cli::catalog::find_provider("codex_cli").expect("Codex catalog entry");
        assert!(uses_workspace_model_discovery(codex, &configured));
    }

    #[test]
    fn model_refresh_uses_the_same_permission_as_provider_execution() {
        let now = chrono::Utc::now();
        let user = temps_entities::users::Model {
            id: 42,
            name: "Harness User".to_string(),
            email: "harness-user@example.com".to_string(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        };
        let auth = |permissions| {
            temps_auth::AuthContext::new_api_key(
                user.clone(),
                None,
                Some(permissions),
                "provider-refresh-test".to_string(),
                1,
            )
        };
        let claude = find_provider("claude_cli").expect("Claude catalog entry");
        let codex = find_provider("codex_cli").expect("Codex catalog entry");

        assert!(ensure_provider_runtime_permission(
            &auth(vec![Permission::AiGatewayWrite]),
            claude
        )
        .is_ok());
        assert!(
            ensure_provider_runtime_permission(&auth(vec![Permission::SettingsRead]), claude)
                .is_err()
        );
        assert!(
            ensure_provider_runtime_permission(&auth(vec![Permission::SystemAdmin]), codex).is_ok()
        );
        assert!(
            ensure_provider_runtime_permission(&auth(vec![Permission::AiGatewayWrite]), codex)
                .is_err()
        );
        assert!(ensure_workspace_model_discovery_permission(&auth(vec![
            Permission::SandboxesWrite,
            Permission::SandboxesExec,
        ]))
        .is_ok());
        assert!(ensure_workspace_model_discovery_permission(&auth(vec![
            Permission::SandboxesWrite,
        ]))
        .is_err());
    }

    #[test]
    fn workspace_refresh_errors_do_not_expose_sandbox_internals() {
        let claude =
            crate::ai_cli::catalog::find_provider("claude_cli").expect("Claude catalog entry");
        let raw_error = temps_ai::AiError::Provider {
            purpose: "provider.capabilities.workspace".to_string(),
            reason: "create application network 'private-name': all predefined address pools have been fully subnetted"
                .to_string(),
        };

        let detail = provider_model_refresh_error_detail(claude, "claude_cli", true, &raw_error);

        assert!(detail.contains("persistent workspace"));
        assert!(detail.contains("Claude Code models"));
        assert!(!detail.contains("private-name"));
        assert!(!detail.contains("address pools"));
    }

    #[test]
    fn host_refresh_errors_keep_provider_context_without_raw_cli_output() {
        let claude =
            crate::ai_cli::catalog::find_provider("claude_cli").expect("Claude catalog entry");
        let raw_error = temps_ai::AiError::Provider {
            purpose: "provider.capabilities".to_string(),
            reason: "authorization: Bearer secret-value".to_string(),
        };

        let detail = provider_model_refresh_error_detail(claude, "claude_cli", false, &raw_error);

        assert!(detail.contains("claude_cli"));
        assert!(!detail.contains("secret-value"));
        assert!(!detail.contains("Bearer"));
    }

    #[test]
    fn workspace_readiness_requires_a_supported_relay_and_saved_credential() {
        let claude =
            crate::ai_cli::catalog::find_provider("claude_cli").expect("Claude catalog entry");
        let configured = provider_catalog_dto_from_runtime(
            claude,
            temps_core::ProviderConfig::default(),
            true,
            Some("subscription".to_string()),
            false,
            None,
            Some("Host authentication is not used by workspaces.".to_string()),
            None,
            Vec::new(),
            temps_ai::ModelCatalogSource::Bootstrap,
            None,
            None,
        );
        assert!(configured.workspace_ready);
        assert!(configured.workspace_readiness_hint.is_none());

        let host_only = provider_catalog_dto_from_runtime(
            claude,
            temps_core::ProviderConfig::default(),
            false,
            None,
            true,
            Some("claude_subscription".to_string()),
            None,
            Some("1.0.0".to_string()),
            Vec::new(),
            temps_ai::ModelCatalogSource::Bootstrap,
            None,
            None,
        );
        assert!(!host_only.workspace_ready);
        assert!(host_only
            .workspace_readiness_hint
            .as_deref()
            .is_some_and(|hint| hint.contains("Save a Claude Code credential")));

        let codex =
            crate::ai_cli::catalog::find_provider("codex_cli").expect("Codex catalog entry");
        let codex_configured = provider_catalog_dto_from_runtime(
            codex,
            temps_core::ProviderConfig::default(),
            true,
            Some("api_key".to_string()),
            true,
            Some("chatgpt_subscription".to_string()),
            None,
            Some("1.0.0".to_string()),
            Vec::new(),
            temps_ai::ModelCatalogSource::Bootstrap,
            None,
            None,
        );
        assert!(codex_configured.workspace_ready);
        assert!(codex_configured.workspace_readiness_hint.is_none());
    }
}
