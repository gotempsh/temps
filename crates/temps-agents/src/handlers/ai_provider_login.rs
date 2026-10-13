// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Console sign-in for subscription-backed AI CLIs.
//!
//! `POST /settings/ai-providers/{id}/login` starts the provider CLI's own
//! sign-in; the operator opens the returned link, and for Claude pastes the
//! code back through `.../login/code`. `GET .../login` reports progress and
//! `.../login/cancel` stops it. The CLI owns the credential store; Temps keeps
//! only an access-token snapshot (see `ai_cli::credential_snapshot`).
//!
//! Every endpoint requires `SettingsWrite` plus the provider's host access
//! requirement from the catalog, including `GET`: the response carries the
//! sign-in link and device code, and whoever holds them can attach their own
//! account to this server.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::post,
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use temps_auth::{permission_guard, RequireAuth};
use temps_core::audit::AuditContext;
use temps_core::problemdetails::{self, Problem};
use temps_core::RequestMetadata;

use crate::ai_cli::catalog::find_provider;
use crate::ai_cli::login::LoginError;
use crate::handlers::ai_providers::{
    ensure_provider_runtime_permission, write_provider_settings_audit, ProviderCredentialSavedAudit,
};
use crate::handlers::AppState;
use crate::services::provider_login_service::{
    LoginActor, LoginCredentialSink, LoginSessionView, StoreLoginCredential,
};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/settings/ai-providers/{provider_id}/login",
            post(start_ai_provider_login).get(get_ai_provider_login),
        )
        .route(
            "/settings/ai-providers/{provider_id}/login/code",
            post(submit_ai_provider_login_code),
        )
        .route(
            "/settings/ai-providers/{provider_id}/login/cancel",
            post(cancel_ai_provider_login),
        )
}

/// Body for `POST /settings/ai-providers/{provider_id}/login/code`.
#[derive(Deserialize, ToSchema)]
pub struct SubmitLoginCodeRequest {
    /// The authorization code shown by the provider after approving access.
    /// Never logged and never echoed back.
    pub code: String,
}

impl std::fmt::Debug for SubmitLoginCodeRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubmitLoginCodeRequest")
            .field("code", &"[redacted]")
            .finish()
    }
}

/// Response wrapper kept separate from the service view so the wire shape is
/// owned by the handler layer.
#[derive(Debug, Serialize, ToSchema)]
pub struct AiProviderLoginResponse {
    #[serde(flatten)]
    pub session: LoginSessionView,
}

impl From<LoginError> for Problem {
    fn from(error: LoginError) -> Self {
        match error {
            LoginError::NotSupported { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Sign-in Not Supported")
                .with_detail(error.to_string()),
            LoginError::UnknownProvider { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Unknown AI Provider")
                .with_detail(error.to_string()),
            LoginError::NotInstalled { .. } => {
                problemdetails::new(StatusCode::UNPROCESSABLE_ENTITY)
                    .with_title("AI CLI Not Installed")
                    .with_detail(error.to_string())
            }
            LoginError::AlreadyInProgress { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Sign-in Already In Progress")
                .with_detail(error.to_string()),
            LoginError::ConcurrencyLimit { .. } => {
                problemdetails::new(StatusCode::TOO_MANY_REQUESTS)
                    .with_title("Too Many Sign-ins")
                    .with_detail(error.to_string())
            }
            LoginError::NoSession { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("No Sign-in In Progress")
                .with_detail(error.to_string()),
            LoginError::NotWaitingForCode { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Sign-in Not Waiting For A Code")
                .with_detail(error.to_string()),
            LoginError::InvalidCode { .. } | LoginError::InvalidUrl { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Sign-in Input")
                    .with_detail(error.to_string())
            }
            LoginError::StateDirectoryInaccessible { .. } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("Sign-in Directory Unavailable")
                    .with_detail(error.to_string())
            }
            LoginError::Timeout { .. } => problemdetails::new(StatusCode::GATEWAY_TIMEOUT)
                .with_title("Sign-in Timed Out")
                .with_detail(error.to_string()),
            LoginError::Cancelled { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Sign-in Cancelled")
                .with_detail(error.to_string()),
            LoginError::Spawn { .. }
            | LoginError::NoChallenge { .. }
            | LoginError::DisallowedHost { .. }
            | LoginError::CliRejected { .. }
            | LoginError::NotCompleted { .. }
            | LoginError::NotAuthenticated { .. }
            | LoginError::CredentialFileMissing { .. }
            | LoginError::CredentialImport { .. }
            | LoginError::Io { .. } => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Sign-in Failed")
                .with_detail(error.to_string()),
        }
    }
}

/// Authorize a sign-in operation for `provider_id`: settings write access and
/// the catalog's host access requirement for running that CLI on this host.
fn authorize(auth: &temps_auth::AuthContext, provider_id: &str) -> Result<(), Problem> {
    permission_guard!(auth, SettingsWrite);
    let provider = find_provider(provider_id).ok_or_else(|| {
        Problem::from(LoginError::UnknownProvider {
            provider_id: provider_id.to_string(),
        })
    })?;
    ensure_provider_runtime_permission(auth, provider)
}

fn no_store(session: LoginSessionView) -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(AiProviderLoginResponse { session }),
    )
}

/// Start signing an AI provider's CLI in to a subscription.
///
/// Returns immediately in the `starting` phase; poll `GET` for the link.
#[utoipa::path(
    tag = "Agents",
    post,
    path = "/settings/ai-providers/{provider_id}/login",
    params(("provider_id" = String, Path, description = "AI provider ID")),
    responses(
        (status = 202, description = "Sign-in started", body = AiProviderLoginResponse),
        (status = 400, description = "Provider has no console sign-in"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Unknown provider"),
        (status = 409, description = "A sign-in is already in progress"),
        (status = 429, description = "Too many sign-ins running"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn start_ai_provider_login(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(provider_id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    authorize(&auth, &provider_id)?;

    let session = app_state
        .provider_login
        .start(
            &provider_id,
            LoginActor {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
        )
        .await?;
    write_provider_settings_audit(&app_state, &auth, &metadata, &provider_id, "login-started")
        .await;
    tracing::info!(
        provider_id = %provider_id,
        session_id = %session.session_id,
        user_id = auth.user_id(),
        "AI provider sign-in started"
    );
    Ok((
        StatusCode::ACCEPTED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(AiProviderLoginResponse { session }),
    ))
}

/// Current sign-in state for a provider: phase, link, device code, elapsed time.
#[utoipa::path(
    tag = "Agents",
    get,
    path = "/settings/ai-providers/{provider_id}/login",
    params(("provider_id" = String, Path, description = "AI provider ID")),
    responses(
        (status = 200, description = "Sign-in state", body = AiProviderLoginResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "No sign-in has been started"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_ai_provider_login(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(provider_id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    authorize(&auth, &provider_id)?;
    let session = app_state
        .provider_login
        .status(&provider_id)
        .ok_or_else(|| LoginError::NoSession {
            provider_id: provider_id.clone(),
        })?;
    Ok(no_store(session))
}

/// Hand the provider's authorization code to the waiting CLI.
#[utoipa::path(
    tag = "Agents",
    post,
    path = "/settings/ai-providers/{provider_id}/login/code",
    params(("provider_id" = String, Path, description = "AI provider ID")),
    request_body = SubmitLoginCodeRequest,
    responses(
        (status = 200, description = "Code submitted", body = AiProviderLoginResponse),
        (status = 400, description = "Invalid code"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "No sign-in in progress"),
        (status = 409, description = "Sign-in is not waiting for a code"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn submit_ai_provider_login_code(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(provider_id): Path<String>,
    Json(request): Json<SubmitLoginCodeRequest>,
) -> Result<impl IntoResponse, Problem> {
    authorize(&auth, &provider_id)?;
    let session = app_state
        .provider_login
        .submit_code(&provider_id, &request.code)
        .await?;
    // The audit trail records that a code was submitted, never its value.
    write_provider_settings_audit(
        &app_state,
        &auth,
        &metadata,
        &provider_id,
        "login-code-submitted",
    )
    .await;
    Ok(no_store(session))
}

/// Cancel an in-progress sign-in and stop its CLI process.
#[utoipa::path(
    tag = "Agents",
    post,
    path = "/settings/ai-providers/{provider_id}/login/cancel",
    params(("provider_id" = String, Path, description = "AI provider ID")),
    responses(
        (status = 200, description = "Sign-in cancelled", body = AiProviderLoginResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "No sign-in has been started"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn cancel_ai_provider_login(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(provider_id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    authorize(&auth, &provider_id)?;
    let session = app_state.provider_login.cancel(&provider_id).await?;
    write_provider_settings_audit(
        &app_state,
        &auth,
        &metadata,
        &provider_id,
        "login-cancelled",
    )
    .await;
    Ok(no_store(session))
}

/// Persists a finished sign-in: encrypts the access-token snapshot into the
/// provider's settings row, records the audit entry for the operator who
/// started it and invalidates cached capabilities.
pub struct SettingsLoginSink {
    config_service: Arc<temps_config::ConfigService>,
    encryption_service: Arc<temps_core::EncryptionService>,
    audit_service: Arc<dyn temps_core::AuditLogger>,
    ai_service: OnceLock<Arc<dyn temps_ai::AiService>>,
}

impl SettingsLoginSink {
    pub fn new(
        config_service: Arc<temps_config::ConfigService>,
        encryption_service: Arc<temps_core::EncryptionService>,
        audit_service: Arc<dyn temps_core::AuditLogger>,
    ) -> Self {
        Self {
            config_service,
            encryption_service,
            audit_service,
            ai_service: OnceLock::new(),
        }
    }

    /// The AI registry is registered after this plugin; bind it once it exists
    /// so a finished sign-in refreshes model capabilities.
    pub fn bind_ai_service(&self, ai_service: Arc<dyn temps_ai::AiService>) {
        let _ = self.ai_service.set(ai_service);
    }
}

#[async_trait]
impl LoginCredentialSink for SettingsLoginSink {
    async fn store(&self, request: StoreLoginCredential<'_>) -> Result<(), LoginError> {
        let encrypted = self
            .encryption_service
            .encrypt_string(request.credential)
            .map_err(|error| LoginError::CredentialImport {
                provider_id: request.provider_id.to_string(),
                reason: format!("could not encrypt the credential: {error}"),
            })?;

        self.config_service
            .update_agent_provider_credential(
                request.provider_id,
                request.auth_type,
                &encrypted,
                // The CLI itself confirmed the sign-in with the provider.
                true,
                None,
                None,
            )
            .await
            .map_err(|error| LoginError::CredentialImport {
                provider_id: request.provider_id.to_string(),
                reason: format!("could not save the credential to settings: {error}"),
            })?;

        if let Some(ai_service) = self.ai_service.get() {
            ai_service
                .invalidate_capabilities_for(Some(request.provider_id))
                .await;
        }

        if let Err(error) = self
            .audit_service
            .create_audit_log(&ProviderCredentialSavedAudit {
                context: AuditContext {
                    user_id: request.actor.user_id,
                    ip_address: request.actor.ip_address.clone(),
                    user_agent: request.actor.user_agent.clone(),
                },
                provider_id: request.provider_id.to_string(),
                auth_type: request.auth_type.to_string(),
                source: "subscription-login".to_string(),
            })
            .await
        {
            tracing::error!(
                provider_id = request.provider_id,
                %error,
                "failed to write AI provider sign-in audit log"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_request_debug_never_prints_the_code() {
        let request = SubmitLoginCodeRequest {
            code: "super-secret-code".to_string(),
        };
        assert!(!format!("{request:?}").contains("super-secret-code"));
    }

    #[test]
    fn conflicts_and_limits_map_to_distinct_statuses() {
        let status = |error: LoginError| Problem::from(error).status_code;
        assert_eq!(
            status(LoginError::AlreadyInProgress {
                provider_id: "claude_cli".into()
            }),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(LoginError::ConcurrencyLimit { limit: 4 }),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            status(LoginError::NoSession {
                provider_id: "claude_cli".into()
            }),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status(LoginError::InvalidCode {
                reason: "empty".into()
            }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status(LoginError::NotSupported {
                provider_id: "opencode".into()
            }),
            StatusCode::BAD_REQUEST
        );
    }
}
