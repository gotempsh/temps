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
    response::IntoResponse,
    routing::{get, patch, post},
    Json, Router,
};
use futures::future::join_all;
use sea_orm::{ActiveModelTrait, EntityTrait, Set};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use utoipa::{IntoParams, ToSchema};

use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::Problem;

use crate::ai_cli::catalog::{find_provider, CredentialFormat, PROVIDER_CATALOG};
use crate::error::AgentError;
use crate::handlers::AppState;

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
    /// `live`, `cache`, `stale_cache`, or `bootstrap`.
    pub model_source: String,
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
    /// Run account-aware model discovery before returning. Normal catalog
    /// reads intentionally use cached/bootstrap models so chat first paint is
    /// not held hostage by provider CLIs that can take 10-15 seconds.
    #[serde(default)]
    pub refresh_models: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveCredentialRequest {
    /// Auth flavor id (must match one of the provider's catalog entries).
    pub auth_type: String,
    /// Plaintext credential body (API key, OAuth token, or full config file
    /// contents). Encrypted with `EncryptionService` before being persisted
    /// inside the `agent_sandbox.providers` JSON map.
    pub credential: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SaveCredentialResponse {
    pub saved: bool,
    pub provider_id: String,
    pub auth_type: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ActivateProviderResponse {
    pub default_provider: String,
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
            "/settings/ai-providers/{provider_id}/activate",
            post(activate_ai_provider),
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

    let providers = join_all(PROVIDER_CATALOG.iter().map(|entry| {
        provider_catalog_dto(
            entry,
            sandbox.provider_config(entry.id),
            query.refresh_models,
        )
    }))
    .await;

    Ok(Json(ProviderCatalogResponse {
        default_provider: sandbox.default_provider,
        providers,
    }))
}

const PROVIDER_STATUS_TIMEOUT: Duration = Duration::from_secs(5);

async fn provider_catalog_dto(
    entry: &'static crate::ai_cli::catalog::ProviderCatalogEntry,
    provider_cfg: temps_core::ProviderConfig,
    refresh_models: bool,
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
        Some(provider) => {
            let status = if refresh_models {
                crate::ai_cli::get_status_cached(provider.as_ref(), true, PROVIDER_STATUS_TIMEOUT)
                    .await
            } else {
                crate::ai_cli::cached_status(entry.id).await
            };
            let Some(status) = status else {
                return provider_catalog_dto_from_runtime(
                    entry,
                    provider_cfg,
                    credential_saved,
                    current_auth_type,
                    false,
                    None,
                    Some(
                        "Host harness status has not been checked yet. Refresh to check it.".into(),
                    ),
                    None,
                    Vec::new(),
                    "bootstrap",
                    None,
                );
            };
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
            } else if refresh_models {
                Some(
                    crate::ai_cli::discover_model_capabilities_cached(
                        provider.as_ref(),
                        identity,
                        true,
                    )
                    .await,
                )
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
                    .unwrap_or("unavailable"),
                snapshot.as_ref().and_then(|snapshot| {
                    (!snapshot.models.is_empty()).then(|| snapshot.refreshed_at.to_rfc3339())
                }),
            )
        }
        None => (false, None, None, None, Vec::new(), "unavailable", None),
    };
    provider_catalog_dto_from_runtime(
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
    )
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
    discovered_source: &str,
    models_refreshed_at: Option<String>,
) -> ProviderCatalogDto {
    let workspace_ready = entry.workspace_chat_supported && credential_saved;
    let workspace_readiness_hint = if workspace_ready {
        None
    } else if !entry.workspace_chat_supported {
        Some(format!(
            "{} is available for host workflows, but its secure persistent-workspace relay is not implemented yet.",
            entry.name
        ))
    } else {
        Some(format!(
            "Save a {} credential to run this harness inside a persistent workspace.",
            entry.name
        ))
    };
    let (runtime_models, model_source) = if discovered_models.is_empty() {
        (
            bootstrap_runtime_models(entry.models),
            "bootstrap".to_string(),
        )
    } else {
        (
            discovered_models
                .into_iter()
                .map(runtime_model_capability)
                .collect(),
            discovered_source.to_string(),
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

    let encrypted = app_state
        .encryption_service
        .encrypt_string(&request.credential)
        .map_err(|e| {
            Problem::from(AgentError::EncryptionError {
                message: format!("Failed to encrypt credential: {}", e),
            })
        })?;

    // Read-modify-write the settings.data JSON. We only touch
    // `agent_sandbox.providers[provider_id]` so unrelated keys are preserved.
    let record = temps_entities::settings::Entity::find_by_id(1)
        .one(app_state.db.as_ref())
        .await
        .map_err(|e| Problem::from(AgentError::Database(e)))?;

    let mut settings_data = record
        .map(|r| r.data)
        .unwrap_or_else(|| serde_json::json!({}));

    let sandbox_value = settings_data
        .as_object_mut()
        .and_then(|m| {
            m.entry("agent_sandbox".to_string())
                .or_insert_with(|| serde_json::json!({}))
                .as_object_mut()
        })
        .ok_or_else(|| {
            Problem::from(AgentError::Validation {
                message: "agent_sandbox settings is not a JSON object".into(),
            })
        })?;

    let providers_value = sandbox_value
        .entry("providers".to_string())
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| {
            Problem::from(AgentError::Validation {
                message: "agent_sandbox.providers is not a JSON object".into(),
            })
        })?;

    // Preserve any fields we don't own (e.g. a previously-saved
    // `default_model`, or future per-provider extras) by merging on top of
    // the existing entry instead of replacing it outright.
    let existing = providers_value
        .get(&provider_id)
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let mut merged = existing.as_object().cloned().unwrap_or_default();
    merged.insert(
        "auth_type".into(),
        serde_json::Value::String(request.auth_type.clone()),
    );
    merged.insert(
        "credentials_encrypted".into(),
        serde_json::Value::String(encrypted),
    );
    merged
        .entry("default_model".to_string())
        .or_insert(serde_json::Value::Null);
    merged
        .entry("extra".to_string())
        .or_insert(serde_json::Value::Null);
    providers_value.insert(provider_id.clone(), serde_json::Value::Object(merged));

    let active = temps_entities::settings::ActiveModel {
        id: Set(1),
        data: Set(settings_data),
        ..Default::default()
    };
    active
        .update(app_state.db.as_ref())
        .await
        .map_err(|e| Problem::from(AgentError::Database(e)))?;

    Ok(Json(SaveCredentialResponse {
        saved: true,
        provider_id,
        auth_type: request.auth_type,
    }))
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
    Path(provider_id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    if find_provider(&provider_id).is_none() {
        return Err(Problem::from(AgentError::Validation {
            message: format!("Unknown AI provider '{}'", provider_id),
        }));
    }

    let record = temps_entities::settings::Entity::find_by_id(1)
        .one(app_state.db.as_ref())
        .await
        .map_err(|e| Problem::from(AgentError::Database(e)))?;

    let mut settings_data = record
        .map(|r| r.data)
        .unwrap_or_else(|| serde_json::json!({}));

    let sandbox_value = settings_data
        .as_object_mut()
        .and_then(|m| {
            m.entry("agent_sandbox".to_string())
                .or_insert_with(|| serde_json::json!({}))
                .as_object_mut()
        })
        .ok_or_else(|| {
            Problem::from(AgentError::Validation {
                message: "agent_sandbox settings is not a JSON object".into(),
            })
        })?;

    let has_credential = sandbox_value
        .get("providers")
        .and_then(|v| v.as_object())
        .and_then(|m| m.get(&provider_id))
        .and_then(|v| v.get("credentials_encrypted"))
        .map(|v| !v.is_null())
        .unwrap_or(false);

    if !has_credential {
        return Err(Problem::from(AgentError::Validation {
            message: format!(
                "Provider '{}' has no saved credential — configure it first before activating",
                provider_id
            ),
        }));
    }

    sandbox_value.insert(
        "default_provider".to_string(),
        serde_json::Value::String(provider_id.clone()),
    );

    let active = temps_entities::settings::ActiveModel {
        id: Set(1),
        data: Set(settings_data),
        ..Default::default()
    };
    active
        .update(app_state.db.as_ref())
        .await
        .map_err(|e| Problem::from(AgentError::Database(e)))?;

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

    let record = temps_entities::settings::Entity::find_by_id(1)
        .one(app_state.db.as_ref())
        .await
        .map_err(|e| Problem::from(AgentError::Database(e)))?;

    let mut settings_data = record
        .map(|r| r.data)
        .unwrap_or_else(|| serde_json::json!({}));

    let sandbox_value = settings_data
        .as_object_mut()
        .and_then(|m| {
            m.entry("agent_sandbox".to_string())
                .or_insert_with(|| serde_json::json!({}))
                .as_object_mut()
        })
        .ok_or_else(|| {
            Problem::from(AgentError::Validation {
                message: "agent_sandbox settings is not a JSON object".into(),
            })
        })?;

    let providers_value = sandbox_value
        .entry("providers".to_string())
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| {
            Problem::from(AgentError::Validation {
                message: "agent_sandbox.providers is not a JSON object".into(),
            })
        })?;

    // Read-modify-write: merge `default_model` on top of the existing
    // provider entry so we don't clobber `credentials_encrypted` etc.
    let existing = providers_value
        .get(&provider_id)
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let mut merged = existing.as_object().cloned().unwrap_or_default();
    merged.insert(
        "default_model".to_string(),
        match &new_model {
            Some(m) => serde_json::Value::String(m.clone()),
            None => serde_json::Value::Null,
        },
    );
    // Turn caps: omitted → leave the stored value alone; 0 → clear; n → set.
    for (key, value) in [
        ("max_turns_analysis", request.max_turns_analysis),
        ("max_turns_fix", request.max_turns_fix),
        ("max_turns_feedback", request.max_turns_feedback),
    ] {
        match value {
            None => {}
            Some(0) => {
                merged.insert(key.to_string(), serde_json::Value::Null);
            }
            Some(v) => {
                merged.insert(key.to_string(), serde_json::Value::from(v));
            }
        }
    }
    // Fill in required fields if this is the first write for the provider.
    merged
        .entry("auth_type".to_string())
        .or_insert_with(|| serde_json::Value::String(provider.default_flavor().id.to_string()));
    merged
        .entry("extra".to_string())
        .or_insert(serde_json::Value::Null);
    // Capture the effective stored values for the response before handing
    // the object to the settings blob.
    let stored_turns = |key: &str| merged.get(key).and_then(|v| v.as_i64()).map(|v| v as i32);
    let effective_max_turns_analysis = stored_turns("max_turns_analysis");
    let effective_max_turns_fix = stored_turns("max_turns_fix");
    let effective_max_turns_feedback = stored_turns("max_turns_feedback");
    providers_value.insert(provider_id.clone(), serde_json::Value::Object(merged));

    let active = temps_entities::settings::ActiveModel {
        id: Set(1),
        data: Set(settings_data),
        ..Default::default()
    };
    active
        .update(app_state.db.as_ref())
        .await
        .map_err(|e| Problem::from(AgentError::Database(e)))?;

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

    #[test]
    fn provider_catalog_reads_do_not_refresh_models_by_default() {
        let query = serde_json::from_value::<ListAiProvidersQuery>(serde_json::json!({}))
            .expect("default query");
        assert!(!query.refresh_models);
        assert!(!query.catalog_only);

        let refresh = serde_json::from_value::<ListAiProvidersQuery>(
            serde_json::json!({ "refresh_models": true, "catalog_only": true }),
        )
        .expect("refresh query");
        assert!(refresh.refresh_models);
        assert!(refresh.catalog_only);
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
            "bootstrap",
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
            "bootstrap",
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
            "bootstrap",
            None,
        );
        assert!(!codex_configured.workspace_ready);
        assert!(codex_configured
            .workspace_readiness_hint
            .as_deref()
            .is_some_and(|hint| hint.contains("secure persistent-workspace relay")));
    }
}
