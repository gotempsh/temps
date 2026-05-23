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

use crate::audit::LoginAudit;
use crate::oidc_errors::OidcError;
use crate::oidc_service::OidcService;
use crate::oidc_types::{
    provider_to_response, CreateOidcProviderRequest, CreateOidcRoleMappingRequest,
    OidcProviderResponse, OidcProviderSummary, OidcRoleMappingResponse, OidcTestConnectionResponse,
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
    path = "/auth/oidc/login/{provider_id}",
    params(
        ("provider_id" = i32, Path, description = "OIDC provider ID"),
        OidcLoginQuery
    ),
    responses(
        (status = 302, description = "Redirect to IdP authorize URL"),
        (status = 404, description = "Provider not found"),
        (status = 503, description = "OIDC provider unreachable")
    ),
    tag = "Authentication"
)]
pub async fn start_oidc_login(
    State(state): State<Arc<AuthState>>,
    Path(provider_id): Path<i32>,
    Query(query): Query<OidcLoginQuery>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<Redirect, Problem> {
    let redirect_uri = format!(
        "{}/api/auth/oidc/callback",
        metadata.base_url.trim_end_matches('/')
    );
    let login = state
        .oidc_service
        .start_login(provider_id, &redirect_uri, query.return_to)
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
        let reason = query.error_description.unwrap_or(err);
        warn!("OIDC callback returned provider error: {}", reason);
        return redirect_login_error(&reason);
    }

    let (code, state_param) = match (query.code, query.state) {
        (Some(code), Some(state_param)) => (code, state_param),
        _ => return redirect_login_error("missing code or state"),
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
            redirect_login_error(&err.to_string())
        }
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

        return Ok((headers, Redirect::to("/mfa-verify")).into_response());
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
        (status = 409, description = "Provider already exists")
    ),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn create_oidc_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Json(request): Json<CreateOidcProviderRequest>,
) -> Result<(StatusCode, Json<OidcProviderResponse>), Problem> {
    permission_guard!(auth, SettingsWrite);
    let provider = state.oidc_service.create_provider(request).await?;
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
    Path(provider_id): Path<i32>,
    Json(request): Json<UpdateOidcProviderRequest>,
) -> Result<Json<OidcProviderResponse>, Problem> {
    permission_guard!(auth, SettingsWrite);
    let provider = state
        .oidc_service
        .update_provider(provider_id, request)
        .await?;
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
    Path(provider_id): Path<i32>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, SettingsWrite);
    state.oidc_service.delete_provider(provider_id).await?;
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
    Path(provider_id): Path<i32>,
    Json(request): Json<CreateOidcRoleMappingRequest>,
) -> Result<(StatusCode, Json<OidcRoleMappingResponse>), Problem> {
    permission_guard!(auth, SettingsWrite);
    let mapping = state
        .oidc_service
        .create_role_mapping(provider_id, request)
        .await?;
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
    Path(mapping_id): Path<i32>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, SettingsWrite);
    state.oidc_service.delete_role_mapping(mapping_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_public_providers,
        start_oidc_login,
        oidc_callback,
        create_oidc_provider,
        list_oidc_providers,
        update_oidc_provider,
        delete_oidc_provider,
        test_oidc_provider,
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
