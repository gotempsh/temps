use std::sync::Arc;

use axum::{
    extract::{Extension, Path, Query, State},
    http::{header::SET_COOKIE, HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use cookie::Cookie;
use serde::{Deserialize, Serialize};
use temps_core::problemdetails::Problem;
use temps_core::{AuditContext, RequestMetadata};
use tracing::{error, warn};
use utoipa::{IntoParams, OpenApi, ToSchema};

use crate::audit::{
    LoginAudit, OidcProviderCreatedAudit, OidcProviderDeletedAudit, OidcProviderUpdatedAudit,
    OidcRoleMappingCreatedAudit, OidcRoleMappingDeletedAudit,
};
use crate::oidc_errors::OidcError;
use crate::oidc_service::OidcService;
use crate::oidc_types::{
    provider_to_response, provider_user_to_response, CreateOidcProviderRequest,
    CreateOidcRoleMappingRequest, OidcProviderResponse, OidcProviderSummary,
    OidcProviderUserResponse, OidcRoleMappingResponse, OidcTestConnectionResponse,
    UpdateOidcProviderRequest,
};
use crate::permission_guard;
use crate::state::AuthState;
use crate::RequireAuth;

#[derive(Debug, Deserialize, IntoParams)]
pub struct OidcCallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct OidcLoginQuery {
    pub return_to: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct OidcProvidersListResponse {
    pub providers: Vec<OidcProviderSummary>,
}

pub fn configure_oidc_routes() -> Router<Arc<AuthState>> {
    Router::new()
        .route("/auth/oidc/providers", get(list_public_providers))
        // NOTE: `/auth/oidc/login/{slug}` is registered in `handlers.rs`
        // inside the rate-limited /auth/* router group. Do NOT add it here
        // — Axum will panic with "Overlapping method route" at startup.
        .route("/admin/oidc/providers", post(create_oidc_provider))
        .route("/admin/oidc/providers", get(list_oidc_providers))
        .route(
            "/admin/oidc/providers/{provider_id}",
            axum::routing::patch(update_oidc_provider),
        )
        .route(
            "/admin/oidc/providers/{provider_id}",
            axum::routing::delete(delete_oidc_provider),
        )
        .route(
            "/admin/oidc/providers/{provider_id}/test",
            post(test_oidc_provider),
        )
        .route(
            "/admin/oidc/providers/{provider_id}/users",
            get(list_oidc_provider_users),
        )
        .route(
            "/admin/oidc/providers/{provider_id}/role-mappings",
            get(list_oidc_role_mappings),
        )
        .route(
            "/admin/oidc/providers/{provider_id}/role-mappings",
            post(create_oidc_role_mapping),
        )
        .route(
            "/admin/oidc/role-mappings/{mapping_id}",
            axum::routing::delete(delete_oidc_role_mapping),
        )
}

#[utoipa::path(
    get,
    path = "/auth/oidc/providers",
    responses(
        (status = 200, description = "Enabled OIDC providers for login page", body = OidcProvidersListResponse)
    ),
    tag = "Authentication"
)]
pub async fn list_public_providers(
    State(state): State<Arc<AuthState>>,
) -> Result<Json<OidcProvidersListResponse>, Problem> {
    let providers = state.oidc_service.list_enabled_providers().await?;
    Ok(Json(OidcProvidersListResponse { providers }))
}

#[utoipa::path(
    get,
    path = "/auth/oidc/login/{slug}",
    params(
        ("slug" = String, Path, description = "OIDC provider slug (from /email-status or /auth/oidc/providers)"),
        OidcLoginQuery
    ),
    responses(
        (status = 302, description = "Redirect to IdP authorize URL"),
        (status = 404, description = "Provider not found"),
        (status = 503, description = "OIDC provider unreachable")
    ),
    tag = "Authentication"
)]
pub async fn start_oidc_login_by_slug(
    State(state): State<Arc<AuthState>>,
    Path(slug): Path<String>,
    Query(query): Query<OidcLoginQuery>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<Redirect, Problem> {
    let provider = state.oidc_service.get_provider_by_slug(&slug).await?;
    let redirect_uri = format!(
        "{}/api/auth/oidc/callback",
        metadata.base_url.trim_end_matches('/')
    );
    let login = state
        .oidc_service
        .start_login(provider.id, &redirect_uri, query.return_to)
        .await?;
    Ok(Redirect::temporary(&login.authorize_url))
}

#[utoipa::path(
    get,
    path = "/auth/oidc/callback",
    params(OidcCallbackQuery),
    responses(
        (status = 302, description = "Redirect to app with session cookie or login error"),
    ),
    tag = "Authentication"
)]
pub async fn oidc_callback(
    State(state): State<Arc<AuthState>>,
    Query(query): Query<OidcCallbackQuery>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Response {
    if let Some(err) = query.error {
        // The IdP's `error` + `error_description` can carry detailed
        // internal state ("user account locked since 2025-01-01", an
        // internal IP address, a tenant-policy reason, etc.). Echoing
        // the raw text into the redirect's `?reason=` query param puts
        // it in the browser address bar, the history database, and any
        // `Referer` header sent on subsequent navigations — a leak
        // path a malicious or merely chatty IdP can exploit.
        //
        // Surface a short opaque code to the browser; keep the
        // raw description in the server log where only operators see
        // it. The login page maps `idp_error` to a friendly message.
        let raw = query
            .error_description
            .clone()
            .unwrap_or_else(|| err.clone());
        warn!(
            target: "temps_auth::oidc",
            error = %err,
            error_description = %raw,
            "OIDC callback returned provider error"
        );
        return redirect_login_error("idp_error");
    }

    let (code, state_param) = match (query.code, query.state) {
        (Some(code), Some(state_param)) => (code, state_param),
        _ => {
            // Same opaque-code policy as the rest of this handler:
            // never let internal details (here, "what query params
            // arrived from the IdP") reach the browser URL bar.
            warn!(
                target: "temps_auth::oidc",
                "OIDC callback missing code or state query parameter"
            );
            return redirect_login_error("callback_invalid");
        }
    };

    match complete_oidc_login(&state, &metadata, &code, &state_param).await {
        Ok(response) => response,
        Err(err) => {
            // Distinguish potentially-abusive probes from ordinary failures.
            // `StateNotFound` means an attacker guessed (or replayed) a state
            // token that never existed; `StateExpired` is the same shape but
            // for stale tokens. Both are interesting to a SOC even though we
            // return the same generic error to the browser.
            match &err {
                OidcError::StateNotFound { .. } => {
                    warn!(
                        target: "temps_auth::oidc::abuse",
                        ip = %metadata.ip_address,
                        user_agent = %metadata.user_agent,
                        "OIDC callback with unknown state token (possible probe / replay): {}",
                        err
                    );
                }
                OidcError::StateExpired { age_secs, .. } => {
                    warn!(
                        target: "temps_auth::oidc::abuse",
                        ip = %metadata.ip_address,
                        user_agent = %metadata.user_agent,
                        age_secs = age_secs,
                        "OIDC callback with expired state token: {}",
                        err
                    );
                }
                _ => {
                    warn!("OIDC callback failed: {}", err);
                }
            }
            // Don't reflect the raw error text into the browser URL.
            // The full error already went to the server log above; the
            // user sees a short, stable code that the login page can
            // translate into a friendly message (and that won't leak
            // IdP response bodies / discovery details into history,
            // referrers, or shared screenshots).
            redirect_login_error(login_error_code_for(&err))
        }
    }
}

/// Map an `OidcError` into a short, stable, user-safe code that the
/// login page can render as a friendly message. Anything the operator
/// actually needs to debug is already in the server log via `warn!`
/// in the callback handler; this is the public face of the failure.
fn login_error_code_for(err: &OidcError) -> &'static str {
    match err {
        OidcError::StateNotFound { .. } => "state_invalid",
        OidcError::StateExpired { .. } => "state_expired",
        OidcError::DiscoveryFailed { .. } => "idp_unreachable",
        OidcError::TokenExchangeFailed { .. } => "idp_rejected_code",
        OidcError::IdTokenInvalid { .. } => "id_token_invalid",
        OidcError::EmailClaimMissing => "email_missing",
        OidcError::EmailNotVerified { .. } => "email_not_verified",
        OidcError::UserNotProvisioned { .. } => "user_not_provisioned",
        OidcError::ProviderDisabled { .. } => "provider_disabled",
        OidcError::ProviderNotFound { .. } => "provider_not_found",
        OidcError::NoProviderConfigured => "no_provider_configured",
        OidcError::InvalidIssuer { .. } => "issuer_invalid",
        OidcError::InvalidReturnTo => "return_to_invalid",
        OidcError::InvalidRole { .. } => "role_invalid",
        OidcError::RoleMappingNotFound { .. } => "role_mapping_not_found",
        OidcError::ProviderAlreadyExists { .. } => "provider_conflict",
        OidcError::Database(_) => "internal_error",
    }
}

async fn complete_oidc_login(
    state: &AuthState,
    metadata: &RequestMetadata,
    code: &str,
    state_param: &str,
) -> Result<Response, OidcError> {
    let login_state = state.oidc_service.consume_login_state(state_param).await?;
    let provider = state
        .oidc_service
        .get_provider(login_state.provider_id)
        .await?;

    // Re-check `enabled` on the callback path. `start_login` already
    // checks it before issuing the authorize URL, but an admin can
    // disable a provider while a user has an in-flight login state
    // (up to `LOGIN_STATE_TTL_MINUTES` later). Without this guard,
    // an admin who disables a provider to revoke SSO access during
    // an incident still lets every in-flight session complete.
    if !provider.enabled {
        return Err(OidcError::ProviderDisabled {
            provider_id: login_state.provider_id,
        });
    }

    let redirect_uri = format!(
        "{}/api/auth/oidc/callback",
        metadata.base_url.trim_end_matches('/')
    );

    let claims = state
        .oidc_service
        .exchange_code(&provider, &redirect_uri, code, &login_state)
        .await?;
    let resolved = state
        .oidc_service
        .resolve_user(login_state.provider_id, &claims.claims, &claims.raw_claims)
        .await?;
    let user = resolved.user;

    let return_to = OidcService::sanitize_return_to(login_state.return_to);

    if user.mfa_enabled {
        let mfa_token = state
            .auth_service
            .create_mfa_session(user.id)
            .await
            .map_err(|e| OidcError::DiscoveryFailed {
                issuer: provider.issuer_url.clone(),
                reason: format!("failed to create MFA session: {e}"),
            })?;
        let encrypted_token =
            state
                .cookie_crypto
                .encrypt(&mfa_token)
                .map_err(|e| OidcError::DiscoveryFailed {
                    issuer: provider.issuer_url.clone(),
                    reason: format!("failed to encrypt MFA token: {e}"),
                })?;

        let mut headers = HeaderMap::new();
        let mfa_cookie = Cookie::build(("mfa_session", encrypted_token))
            .http_only(true)
            .path("/")
            .max_age(cookie::time::Duration::minutes(5))
            .same_site(cookie::SameSite::Strict)
            .secure(metadata.is_secure)
            .build();
        let cookie_header =
            mfa_cookie
                .to_string()
                .parse()
                .map_err(|e| OidcError::DiscoveryFailed {
                    issuer: provider.issuer_url.clone(),
                    reason: format!("failed to build MFA cookie header: {e}"),
                })?;
        headers.insert(SET_COOKIE, cookie_header);

        // Audit the SSO leg of an MFA-gated login here. The MFA-verify
        // endpoint emits its own follow-up audit on success; together they
        // tell the full story (SSO ok → MFA challenge issued → MFA verified).
        // Without this row, an attacker who stops at MFA never appears in
        // the audit log even though they completed a full IdP login.
        if let Err(e) = state
            .audit_service
            .create_audit_log(&LoginAudit {
                context: AuditContext {
                    user_id: user.id,
                    ip_address: Some(metadata.ip_address.to_string()),
                    user_agent: metadata.user_agent.as_str().to_string(),
                },
                success: true,
                login_method: "oidc-mfa-pending".to_string(),
            })
            .await
        {
            error!("Failed to create OIDC MFA-pending audit log: {}", e);
        }

        // Carry the IdP-supplied return_to through the MFA step.
        // `OidcService::sanitize_return_to` already enforced that
        // this is a same-origin relative path, so URL-encoding it
        // as a query param can't be turned into an open redirect.
        // The frontend's `MfaVerify` page prefers this query value
        // over its sessionStorage fallback — important because
        // sessionStorage doesn't survive a tab switch or a private
        // window, both of which are common when the user reaches
        // the IdP through an external link.
        let target = if return_to == "/dashboard" {
            "/mfa-verify".to_string()
        } else {
            format!("/mfa-verify?return_to={}", urlencoding::encode(&return_to))
        };
        return Ok((headers, Redirect::to(&target)).into_response());
    }

    let session_token = state
        .auth_service
        .create_session(user.id)
        .await
        .map_err(|e| OidcError::DiscoveryFailed {
            issuer: provider.issuer_url.clone(),
            reason: format!("failed to create session: {e}"),
        })?;
    let encrypted_token =
        state
            .cookie_crypto
            .encrypt(&session_token)
            .map_err(|e| OidcError::DiscoveryFailed {
                issuer: provider.issuer_url.clone(),
                reason: format!("failed to encrypt session token: {e}"),
            })?;
    let headers = state
        .auth_service
        .create_session_cookie(&encrypted_token, metadata.is_secure);

    if let Err(e) = state
        .audit_service
        .create_audit_log(&LoginAudit {
            context: AuditContext {
                user_id: user.id,
                ip_address: Some(metadata.ip_address.to_string()),
                user_agent: metadata.user_agent.as_str().to_string(),
            },
            success: true,
            login_method: "oidc".to_string(),
        })
        .await
    {
        error!("Failed to create OIDC login audit log: {}", e);
    }

    Ok((headers, Redirect::to(&return_to)).into_response())
}

fn redirect_login_error(reason: &str) -> Response {
    let encoded = urlencoding::encode(reason);
    Redirect::to(&format!("/login?error=oidc_failed&reason={encoded}")).into_response()
}

#[utoipa::path(
    post,
    path = "/admin/oidc/providers",
    request_body = CreateOidcProviderRequest,
    responses(
        (status = 201, description = "OIDC provider created", body = OidcProviderResponse),
        (status = 409, description = "Another OIDC provider already uses that name")
    ),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn create_oidc_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateOidcProviderRequest>,
) -> Result<(StatusCode, Json<OidcProviderResponse>), Problem> {
    permission_guard!(auth, SettingsWrite);
    let provider = state.oidc_service.create_provider(request).await?;

    if let Err(e) = state
        .audit_service
        .create_audit_log(&OidcProviderCreatedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.to_string()),
                user_agent: metadata.user_agent.as_str().to_string(),
            },
            provider_id: provider.id,
            name: provider.name.clone(),
            issuer_url: provider.issuer_url.clone(),
            template: provider.template.clone(),
            enabled: provider.enabled,
            jit_provisioning: provider.jit_provisioning,
            trust_idp_email: provider.trust_idp_email,
        })
        .await
    {
        error!("Failed to create OIDC provider audit log: {}", e);
    }

    // Emit anonymous telemetry. The `provider` property is allowlisted against
    // known type labels so free-form admin-supplied names are never transmitted.
    const KNOWN_OIDC_TEMPLATES: &[&str] = &[
        "generic", "google", "github", "keycloak", "okta", "auth0", "azure-ad",
    ];
    let provider_label = KNOWN_OIDC_TEMPLATES
        .iter()
        .find(|t| **t == provider.template.as_str())
        .copied();
    state.telemetry.report(
        temps_core::TelemetryEvent::new(temps_core::TelemetryEventKind::OidcProviderConfigured)
            .with_opt("provider", provider_label),
    );

    Ok((StatusCode::CREATED, Json(provider_to_response(&provider))))
}

#[utoipa::path(
    get,
    path = "/admin/oidc/providers",
    responses(
        (status = 200, description = "OIDC providers", body = Vec<OidcProviderResponse>)
    ),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn list_oidc_providers(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
) -> Result<Json<Vec<OidcProviderResponse>>, Problem> {
    permission_guard!(auth, SettingsWrite);
    let providers = state.oidc_service.list_providers().await?;
    Ok(Json(providers.iter().map(provider_to_response).collect()))
}

#[utoipa::path(
    patch,
    path = "/admin/oidc/providers/{provider_id}",
    request_body = UpdateOidcProviderRequest,
    responses(
        (status = 200, description = "OIDC provider updated", body = OidcProviderResponse)
    ),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn update_oidc_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(provider_id): Path<i32>,
    Json(request): Json<UpdateOidcProviderRequest>,
) -> Result<Json<OidcProviderResponse>, Problem> {
    permission_guard!(auth, SettingsWrite);

    // Capture which fields the PATCH touched *before* moving the
    // request into the service. The audit row is most useful when an
    // auditor can answer "was the client_secret rotated?" without
    // diffing the row history themselves. We don't log the new
    // values here; the provider row is the source of truth.
    let mut fields_changed = Vec::new();
    if request.name.is_some() {
        fields_changed.push("name".to_string());
    }
    if request.issuer_url.is_some() {
        fields_changed.push("issuer_url".to_string());
    }
    if request.client_id.is_some() {
        fields_changed.push("client_id".to_string());
    }
    if request.client_secret.is_some() {
        fields_changed.push("client_secret".to_string());
    }
    if request.scopes.is_some() {
        fields_changed.push("scopes".to_string());
    }
    if request.jit_provisioning.is_some() {
        fields_changed.push("jit_provisioning".to_string());
    }
    if request.enabled.is_some() {
        fields_changed.push("enabled".to_string());
    }
    if request.template.is_some() {
        fields_changed.push("template".to_string());
    }
    if request.group_claim.is_some() {
        fields_changed.push("group_claim".to_string());
    }
    if request.role_claim.is_some() {
        fields_changed.push("role_claim".to_string());
    }
    if request.default_role.is_some() {
        fields_changed.push("default_role".to_string());
    }
    if request.trust_idp_email.is_some() {
        fields_changed.push("trust_idp_email".to_string());
    }

    let provider = state
        .oidc_service
        .update_provider(provider_id, request)
        .await?;

    if let Err(e) = state
        .audit_service
        .create_audit_log(&OidcProviderUpdatedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.to_string()),
                user_agent: metadata.user_agent.as_str().to_string(),
            },
            provider_id: provider.id,
            name: provider.name.clone(),
            fields_changed,
        })
        .await
    {
        error!("Failed to create OIDC provider update audit log: {}", e);
    }

    Ok(Json(provider_to_response(&provider)))
}

#[utoipa::path(
    delete,
    path = "/admin/oidc/providers/{provider_id}",
    responses((status = 204, description = "OIDC provider deleted")),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn delete_oidc_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(provider_id): Path<i32>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, SettingsWrite);

    // Snapshot identity *before* deletion so the audit row carries
    // the provider name + issuer even after the row is gone. We
    // tolerate a 404 here only by letting it propagate through the
    // delete call itself.
    let provider = state.oidc_service.get_provider(provider_id).await?;

    state.oidc_service.delete_provider(provider_id).await?;

    if let Err(e) = state
        .audit_service
        .create_audit_log(&OidcProviderDeletedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.to_string()),
                user_agent: metadata.user_agent.as_str().to_string(),
            },
            provider_id: provider.id,
            name: provider.name,
            issuer_url: provider.issuer_url,
        })
        .await
    {
        error!("Failed to create OIDC provider delete audit log: {}", e);
    }

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/admin/oidc/providers/{provider_id}/test",
    responses(
        (status = 200, description = "Connection test result", body = OidcTestConnectionResponse)
    ),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn test_oidc_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Path(provider_id): Path<i32>,
) -> Result<Json<OidcTestConnectionResponse>, Problem> {
    permission_guard!(auth, SettingsWrite);
    match state.oidc_service.test_connection(provider_id).await {
        Ok(message) => Ok(Json(OidcTestConnectionResponse {
            success: true,
            message,
        })),
        Err(err) => Ok(Json(OidcTestConnectionResponse {
            success: false,
            message: err.to_string(),
        })),
    }
}

#[utoipa::path(
    get,
    path = "/admin/oidc/providers/{provider_id}/users",
    params(
        ("provider_id" = i32, Path, description = "OIDC provider ID")
    ),
    responses(
        (status = 200, description = "Users authenticated via this OIDC provider", body = Vec<OidcProviderUserResponse>),
        (status = 404, description = "Provider not found")
    ),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn list_oidc_provider_users(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Path(provider_id): Path<i32>,
) -> Result<Json<Vec<OidcProviderUserResponse>>, Problem> {
    permission_guard!(auth, SettingsWrite);
    let users = state
        .oidc_service
        .list_users_for_provider(provider_id)
        .await?;
    Ok(Json(users.iter().map(provider_user_to_response).collect()))
}

#[utoipa::path(
    get,
    path = "/admin/oidc/providers/{provider_id}/role-mappings",
    responses(
        (status = 200, description = "OIDC role mappings", body = Vec<OidcRoleMappingResponse>)
    ),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn list_oidc_role_mappings(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Path(provider_id): Path<i32>,
) -> Result<Json<Vec<OidcRoleMappingResponse>>, Problem> {
    permission_guard!(auth, SettingsWrite);
    let mappings = state.oidc_service.list_role_mappings(provider_id).await?;
    Ok(Json(mappings))
}

#[utoipa::path(
    post,
    path = "/admin/oidc/providers/{provider_id}/role-mappings",
    request_body = CreateOidcRoleMappingRequest,
    responses(
        (status = 201, description = "Role mapping created", body = OidcRoleMappingResponse)
    ),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn create_oidc_role_mapping(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(provider_id): Path<i32>,
    Json(request): Json<CreateOidcRoleMappingRequest>,
) -> Result<(StatusCode, Json<OidcRoleMappingResponse>), Problem> {
    permission_guard!(auth, SettingsWrite);
    let mapping = state
        .oidc_service
        .create_role_mapping(provider_id, request)
        .await?;

    if let Err(e) = state
        .audit_service
        .create_audit_log(&OidcRoleMappingCreatedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.to_string()),
                user_agent: metadata.user_agent.as_str().to_string(),
            },
            provider_id,
            mapping_id: mapping.id,
            idp_group: mapping.idp_group.clone(),
            role: mapping.role.clone(),
            priority: mapping.priority,
        })
        .await
    {
        error!("Failed to create OIDC role mapping audit log: {}", e);
    }

    Ok((StatusCode::CREATED, Json(mapping)))
}

#[utoipa::path(
    delete,
    path = "/admin/oidc/role-mappings/{mapping_id}",
    responses((status = 204, description = "Role mapping deleted")),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn delete_oidc_role_mapping(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(mapping_id): Path<i32>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, SettingsWrite);
    state.oidc_service.delete_role_mapping(mapping_id).await?;

    if let Err(e) = state
        .audit_service
        .create_audit_log(&OidcRoleMappingDeletedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.to_string()),
                user_agent: metadata.user_agent.as_str().to_string(),
            },
            mapping_id,
        })
        .await
    {
        error!("Failed to create OIDC role mapping delete audit log: {}", e);
    }

    Ok(StatusCode::NO_CONTENT)
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_public_providers,
        start_oidc_login_by_slug,
        oidc_callback,
        create_oidc_provider,
        list_oidc_providers,
        update_oidc_provider,
        delete_oidc_provider,
        test_oidc_provider,
        list_oidc_provider_users,
        list_oidc_role_mappings,
        create_oidc_role_mapping,
        delete_oidc_role_mapping,
    ),
    components(
        schemas(
            OidcProvidersListResponse,
            CreateOidcProviderRequest,
            OidcProviderResponse,
            OidcProviderSummary,
            OidcProviderUserResponse,
            UpdateOidcProviderRequest,
            OidcTestConnectionResponse,
            OidcRoleMappingResponse,
            CreateOidcRoleMappingRequest,
        )
    ),
    tags(
        (name = "Authentication", description = "Authentication and authorization endpoints")
    )
)]
pub struct OidcApiDoc;
