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
    extract::{Path, State},
    response::IntoResponse,
    routing::{get, patch, post},
    Json, Router,
};
use sea_orm::{ActiveModelTrait, EntityTrait, Set};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::ToSchema;

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
    /// credential seeded into a *sandboxed autofixer container*, while this
    /// one is what actually gates whether the AI Gateway can route requests
    /// to this provider (ADR-037's `AgentCliAiService` uses only the host's
    /// ambient CLI session — ordering `claude setup-token`/`codex login` on
    /// this machine — and never reads the sandbox credential at all). A
    /// provider can show `credential_saved: true` and `host_authenticated:
    /// false` at the same time; only the latter means chat/gateway routing
    /// will actually work.
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
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProviderCatalogResponse {
    /// Active provider id from `agent_sandbox.default_provider`. The settings
    /// UI uses this to highlight which card is the active one.
    pub default_provider: String,
    pub providers: Vec<ProviderCatalogDto>,
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
    responses(
        (status = 200, body = ProviderCatalogResponse),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_ai_providers(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);

    let sandbox = load_agent_sandbox(&app_state).await?;

    let mut providers = Vec::with_capacity(PROVIDER_CATALOG.len());
    for entry in PROVIDER_CATALOG.iter() {
        let provider_cfg = sandbox.provider_config(entry.id);
        let credential_saved = provider_cfg.credentials_encrypted.is_some();
        let current_auth_type = if credential_saved {
            Some(provider_cfg.auth_type)
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
                let status = provider.get_status().await;
                let authenticated = status.installed && status.authenticated;
                let version = status.version.clone();
                let identity = format!(
                    "{}|{}|{}|{}",
                    version.as_deref().unwrap_or("unknown"),
                    status.auth_method.as_deref().unwrap_or("unknown"),
                    status.email.as_deref().unwrap_or("unknown"),
                    status.subscription_type.as_deref().unwrap_or("unknown")
                );
                let snapshot = if status.installed {
                    Some(
                        crate::ai_cli::discover_model_capabilities_cached(
                            provider.as_ref(),
                            identity,
                            false,
                        )
                        .await,
                    )
                } else {
                    None
                };
                let models = snapshot
                    .as_ref()
                    .map(|snapshot| {
                        snapshot
                            .models
                            .iter()
                            .map(|model| model.id.clone())
                            .collect()
                    })
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
        let (models, model_source) = if discovered_models.is_empty() {
            (
                entry.models.iter().map(|model| model.to_string()).collect(),
                "bootstrap".to_string(),
            )
        } else {
            (discovered_models, discovered_source.to_string())
        };

        providers.push(ProviderCatalogDto {
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
        });
    }

    Ok(Json(ProviderCatalogResponse {
        default_provider: sandbox.default_provider,
        providers,
    }))
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
