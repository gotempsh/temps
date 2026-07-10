use std::sync::Arc;

use axum::{
    extract::{Extension, Form, Path, Query, State},
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

use crate::permission_guard;
use crate::saml_errors::SamlError;
use crate::saml_types::{
    saml_provider_to_response, saml_provider_user_to_response, saml_role_mapping_to_response,
    CreateSamlProviderRequest, CreateSamlRoleMappingRequest, SamlProviderResponse,
    SamlProviderSummary, SamlProviderUserResponse, SamlRoleMappingResponse,
    SamlTestConnectionResponse, UpdateSamlProviderRequest,
};
use crate::state::AuthState;
use crate::RequireAuth;

#[derive(Debug, Deserialize, IntoParams)]
pub struct SamlLoginQuery {
    pub return_to: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SamlAcsForm {
    #[serde(rename = "SAMLResponse")]
    pub saml_response: String,
    #[serde(rename = "RelayState")]
    pub relay_state: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SamlProvidersListResponse {
    pub providers: Vec<SamlProviderSummary>,
}

fn acs_url(metadata: &RequestMetadata) -> String {
    format!(
        "{}/api/auth/saml/acs",
        metadata.base_url.trim_end_matches('/')
    )
}

/// Admin CRUD + the public provider-list route. `/auth/saml/login/{slug}`
/// and `/auth/saml/acs` are registered in `handlers.rs` inside the
/// rate-limited `/auth/*` group instead -- same split as OIDC, see the
/// note on `oidc_handler::configure_oidc_routes`.
pub fn configure_saml_routes() -> Router<Arc<AuthState>> {
    Router::new()
        .route("/auth/saml/providers", get(list_public_providers))
        .route("/auth/saml/metadata/{slug}", get(saml_sp_metadata))
        .route("/admin/saml/providers", post(create_saml_provider))
        .route("/admin/saml/providers", get(list_saml_providers))
        .route(
            "/admin/saml/providers/{provider_id}",
            axum::routing::patch(update_saml_provider),
        )
        .route(
            "/admin/saml/providers/{provider_id}",
            axum::routing::delete(delete_saml_provider),
        )
        .route(
            "/admin/saml/providers/{provider_id}/test",
            post(test_saml_provider),
        )
        .route(
            "/admin/saml/providers/{provider_id}/refresh-metadata",
            post(refresh_saml_metadata),
        )
        .route(
            "/admin/saml/providers/{provider_id}/users",
            get(list_saml_provider_users),
        )
        .route(
            "/admin/saml/providers/{provider_id}/role-mappings",
            get(list_saml_role_mappings),
        )
        .route(
            "/admin/saml/providers/{provider_id}/role-mappings",
            post(create_saml_role_mapping),
        )
        .route(
            "/admin/saml/role-mappings/{mapping_id}",
            axum::routing::delete(delete_saml_role_mapping),
        )
}

#[utoipa::path(
    get,
    path = "/auth/saml/providers",
    responses((status = 200, description = "Enabled SAML providers for login page", body = SamlProvidersListResponse)),
    tag = "Authentication"
)]
pub async fn list_public_providers(
    State(state): State<Arc<AuthState>>,
) -> Result<Json<SamlProvidersListResponse>, Problem> {
    let providers = state.saml_service.list_enabled_providers().await?;
    Ok(Json(SamlProvidersListResponse { providers }))
}

#[utoipa::path(
    get,
    path = "/auth/saml/login/{slug}",
    params(("slug" = String, Path, description = "SAML provider slug"), SamlLoginQuery),
    responses(
        (status = 302, description = "Redirect to IdP SSO URL"),
        (status = 404, description = "Provider not found"),
    ),
    tag = "Authentication"
)]
pub async fn start_saml_login_by_slug(
    State(state): State<Arc<AuthState>>,
    Path(slug): Path<String>,
    Query(query): Query<SamlLoginQuery>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<Redirect, Problem> {
    let provider = state.saml_service.get_provider_by_slug(&slug).await?;
    let start = state
        .saml_service
        .start_login(provider.id, &acs_url(&metadata), query.return_to)
        .await?;
    Ok(Redirect::temporary(&start.redirect_url))
}

#[utoipa::path(
    post,
    path = "/auth/saml/acs",
    responses((status = 302, description = "Redirect to app with session cookie or login error")),
    tag = "Authentication"
)]
pub async fn saml_acs(
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Form(form): Form<SamlAcsForm>,
) -> Response {
    match complete_saml_login(&state, &metadata, &form).await {
        Ok(response) => response,
        Err(err) => {
            match &err {
                SamlError::StateNotFound { .. } => {
                    warn!(
                        target: "temps_auth::saml::abuse",
                        ip = %metadata.ip_address,
                        user_agent = %metadata.user_agent,
                        "SAML ACS with unknown relay state (possible probe / replay): {}",
                        err
                    );
                }
                SamlError::StateExpired { age_secs, .. } => {
                    warn!(
                        target: "temps_auth::saml::abuse",
                        ip = %metadata.ip_address,
                        user_agent = %metadata.user_agent,
                        age_secs = age_secs,
                        "SAML ACS with expired relay state: {}",
                        err
                    );
                }
                _ => {
                    warn!("SAML ACS failed: {}", err);
                }
            }
            redirect_login_error(login_error_code_for(&err))
        }
    }
}

fn login_error_code_for(err: &SamlError) -> &'static str {
    match err {
        SamlError::StateNotFound { .. } => "state_invalid",
        SamlError::StateExpired { .. } => "state_expired",
        SamlError::ProviderNotFound { .. } => "provider_not_found",
        SamlError::ProviderDisabled { .. } => "provider_disabled",
        SamlError::AssertionValidationFailed { .. } => "assertion_invalid",
        SamlError::ResponseParseFailed { .. } => "response_invalid",
        SamlError::NameIdMissing => "name_id_missing",
        SamlError::EmailMissing => "email_missing",
        SamlError::EncryptedAssertionNotSupported => "encrypted_assertion_unsupported",
        SamlError::UserNotProvisioned { .. } => "user_not_provisioned",
        SamlError::EmailNotTrusted { .. } => "email_not_trusted",
        SamlError::InvalidReturnTo => "return_to_invalid",
        SamlError::InvalidCert { .. }
        | SamlError::InvalidMetadata { .. }
        | SamlError::MetadataFetchFailed { .. }
        | SamlError::NoMetadataUrl { .. }
        | SamlError::MetadataUrlNotAllowed { .. }
        | SamlError::ProviderAlreadyExists { .. }
        | SamlError::RoleMappingNotFound { .. }
        | SamlError::InvalidRole { .. } => "internal_error",
        SamlError::Database(_) => "internal_error",
    }
}

async fn complete_saml_login(
    state: &AuthState,
    metadata: &RequestMetadata,
    form: &SamlAcsForm,
) -> Result<Response, SamlError> {
    let login_state = state
        .saml_service
        .consume_login_state(&form.relay_state)
        .await?;
    let provider = state
        .saml_service
        .get_provider(login_state.provider_id)
        .await?;

    let resolved = state
        .saml_service
        .process_acs_response(
            &provider,
            &login_state,
            &form.saml_response,
            &acs_url(metadata),
        )
        .await?;
    let user = resolved.user;

    let return_to = crate::oidc_service::OidcService::sanitize_return_to(login_state.return_to);

    if user.mfa_enabled {
        let mfa_token = state
            .auth_service
            .create_mfa_session(user.id)
            .await
            .map_err(|e| SamlError::AssertionValidationFailed {
                reason: format!("failed to create MFA session: {e}"),
            })?;
        let encrypted_token = state.cookie_crypto.encrypt(&mfa_token).map_err(|e| {
            SamlError::AssertionValidationFailed {
                reason: format!("failed to encrypt MFA token: {e}"),
            }
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
                .map_err(|e| SamlError::AssertionValidationFailed {
                    reason: format!("failed to build MFA cookie header: {e}"),
                })?;
        headers.insert(SET_COOKIE, cookie_header);

        if let Err(e) = state
            .audit_service
            .create_audit_log(&crate::audit::LoginAudit {
                context: AuditContext {
                    user_id: user.id,
                    ip_address: Some(metadata.ip_address.to_string()),
                    user_agent: metadata.user_agent.as_str().to_string(),
                },
                success: true,
                login_method: "saml-mfa-pending".to_string(),
            })
            .await
        {
            error!("Failed to create SAML MFA-pending audit log: {}", e);
        }

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
        .map_err(|e| SamlError::AssertionValidationFailed {
            reason: format!("failed to create session: {e}"),
        })?;
    let encrypted_token = state.cookie_crypto.encrypt(&session_token).map_err(|e| {
        SamlError::AssertionValidationFailed {
            reason: format!("failed to encrypt session token: {e}"),
        }
    })?;
    let headers = state
        .auth_service
        .create_session_cookie(&encrypted_token, metadata.is_secure);

    if let Err(e) = state
        .audit_service
        .create_audit_log(&crate::audit::LoginAudit {
            context: AuditContext {
                user_id: user.id,
                ip_address: Some(metadata.ip_address.to_string()),
                user_agent: metadata.user_agent.as_str().to_string(),
            },
            success: true,
            login_method: "saml".to_string(),
        })
        .await
    {
        error!("Failed to create SAML login audit log: {}", e);
    }

    Ok((headers, Redirect::to(&return_to)).into_response())
}

fn redirect_login_error(reason: &str) -> Response {
    let encoded = urlencoding::encode(reason);
    Redirect::to(&format!("/login?error=saml_failed&reason={encoded}")).into_response()
}

#[utoipa::path(
    get,
    path = "/auth/saml/metadata/{slug}",
    params(("slug" = String, Path, description = "SAML provider slug")),
    responses((status = 200, description = "SP metadata XML"), (status = 404, description = "Provider not found")),
    tag = "Authentication"
)]
pub async fn saml_sp_metadata(
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(slug): Path<String>,
) -> Result<Response, Problem> {
    let provider = state.saml_service.get_provider_by_slug(&slug).await?;
    let xml = state
        .saml_service
        .sp_metadata_xml(&provider, &acs_url(&metadata))?;
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            "application/samlmetadata+xml",
        )],
        xml,
    )
        .into_response())
}

#[utoipa::path(
    post,
    path = "/admin/saml/providers",
    request_body = CreateSamlProviderRequest,
    responses(
        (status = 201, description = "SAML provider created", body = SamlProviderResponse),
        (status = 409, description = "Another SAML provider already uses that name")
    ),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn create_saml_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateSamlProviderRequest>,
) -> Result<(StatusCode, Json<SamlProviderResponse>), Problem> {
    permission_guard!(auth, SettingsWrite);
    let provider = state
        .saml_service
        .create_provider(request, &metadata.base_url)
        .await?;

    if let Err(e) = state
        .audit_service
        .create_audit_log(&crate::audit::SamlProviderCreatedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.to_string()),
                user_agent: metadata.user_agent.as_str().to_string(),
            },
            provider_id: provider.id,
            name: provider.name.clone(),
            idp_entity_id: provider.idp_entity_id.clone(),
            template: provider.template.clone(),
            enabled: provider.enabled,
            jit_provisioning: provider.jit_provisioning,
            trust_idp_email: provider.trust_idp_email,
        })
        .await
    {
        error!("Failed to create SAML provider audit log: {}", e);
    }

    Ok((
        StatusCode::CREATED,
        Json(saml_provider_to_response(&provider)),
    ))
}

#[utoipa::path(
    get,
    path = "/admin/saml/providers",
    responses((status = 200, description = "SAML providers", body = Vec<SamlProviderResponse>)),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn list_saml_providers(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
) -> Result<Json<Vec<SamlProviderResponse>>, Problem> {
    permission_guard!(auth, SettingsWrite);
    let providers = state.saml_service.list_providers().await?;
    Ok(Json(
        providers.iter().map(saml_provider_to_response).collect(),
    ))
}

#[utoipa::path(
    patch,
    path = "/admin/saml/providers/{provider_id}",
    request_body = UpdateSamlProviderRequest,
    responses((status = 200, description = "SAML provider updated", body = SamlProviderResponse)),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn update_saml_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(provider_id): Path<i32>,
    Json(request): Json<UpdateSamlProviderRequest>,
) -> Result<Json<SamlProviderResponse>, Problem> {
    permission_guard!(auth, SettingsWrite);

    let mut fields_changed = Vec::new();
    if request.name.is_some() {
        fields_changed.push("name".to_string());
    }
    if request.idp_entity_id.is_some() {
        fields_changed.push("idp_entity_id".to_string());
    }
    if request.idp_sso_url.is_some() {
        fields_changed.push("idp_sso_url".to_string());
    }
    if request.idp_x509_cert.is_some() {
        fields_changed.push("idp_x509_cert".to_string());
    }
    if request.jit_provisioning.is_some() {
        fields_changed.push("jit_provisioning".to_string());
    }
    if request.enabled.is_some() {
        fields_changed.push("enabled".to_string());
    }
    if request.trust_idp_email.is_some() {
        fields_changed.push("trust_idp_email".to_string());
    }

    let provider = state
        .saml_service
        .update_provider(provider_id, request)
        .await?;

    if let Err(e) = state
        .audit_service
        .create_audit_log(&crate::audit::SamlProviderUpdatedAudit {
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
        error!("Failed to create SAML provider update audit log: {}", e);
    }

    Ok(Json(saml_provider_to_response(&provider)))
}

#[utoipa::path(
    delete,
    path = "/admin/saml/providers/{provider_id}",
    responses((status = 204, description = "SAML provider deleted")),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn delete_saml_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(provider_id): Path<i32>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, SettingsWrite);

    let provider = state.saml_service.get_provider(provider_id).await?;
    state.saml_service.delete_provider(provider_id).await?;

    if let Err(e) = state
        .audit_service
        .create_audit_log(&crate::audit::SamlProviderDeletedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.to_string()),
                user_agent: metadata.user_agent.as_str().to_string(),
            },
            provider_id: provider.id,
            name: provider.name,
            idp_entity_id: provider.idp_entity_id,
        })
        .await
    {
        error!("Failed to create SAML provider delete audit log: {}", e);
    }

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/admin/saml/providers/{provider_id}/test",
    responses((status = 200, description = "Connection test result", body = SamlTestConnectionResponse)),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn test_saml_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Path(provider_id): Path<i32>,
) -> Result<Json<SamlTestConnectionResponse>, Problem> {
    permission_guard!(auth, SettingsWrite);
    match state.saml_service.test_connection(provider_id).await {
        Ok(message) => Ok(Json(SamlTestConnectionResponse {
            success: true,
            message,
        })),
        Err(err) => Ok(Json(SamlTestConnectionResponse {
            success: false,
            message: err.to_string(),
        })),
    }
}

#[utoipa::path(
    post,
    path = "/admin/saml/providers/{provider_id}/refresh-metadata",
    responses(
        (status = 200, description = "IdP metadata refreshed", body = SamlProviderResponse),
        (status = 422, description = "Provider has no idp_metadata_url configured")
    ),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn refresh_saml_metadata(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(provider_id): Path<i32>,
) -> Result<Json<SamlProviderResponse>, Problem> {
    permission_guard!(auth, SettingsWrite);
    let provider = state.saml_service.refresh_metadata(provider_id).await?;

    if let Err(e) = state
        .audit_service
        .create_audit_log(&crate::audit::SamlProviderUpdatedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.to_string()),
                user_agent: metadata.user_agent.as_str().to_string(),
            },
            provider_id: provider.id,
            name: provider.name.clone(),
            fields_changed: vec![
                "idp_entity_id".to_string(),
                "idp_sso_url".to_string(),
                "idp_x509_cert".to_string(),
            ],
        })
        .await
    {
        error!("Failed to create SAML metadata refresh audit log: {}", e);
    }

    Ok(Json(saml_provider_to_response(&provider)))
}

#[utoipa::path(
    get,
    path = "/admin/saml/providers/{provider_id}/users",
    params(("provider_id" = i32, Path, description = "SAML provider ID")),
    responses(
        (status = 200, description = "Users authenticated via this SAML provider", body = Vec<SamlProviderUserResponse>),
        (status = 404, description = "Provider not found")
    ),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn list_saml_provider_users(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Path(provider_id): Path<i32>,
) -> Result<Json<Vec<SamlProviderUserResponse>>, Problem> {
    permission_guard!(auth, SettingsWrite);
    let users = state
        .saml_service
        .list_users_for_provider(provider_id)
        .await?;
    Ok(Json(
        users.iter().map(saml_provider_user_to_response).collect(),
    ))
}

#[utoipa::path(
    get,
    path = "/admin/saml/providers/{provider_id}/role-mappings",
    responses((status = 200, description = "SAML role mappings", body = Vec<SamlRoleMappingResponse>)),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn list_saml_role_mappings(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Path(provider_id): Path<i32>,
) -> Result<Json<Vec<SamlRoleMappingResponse>>, Problem> {
    permission_guard!(auth, SettingsWrite);
    let mappings = state.saml_service.list_role_mappings(provider_id).await?;
    Ok(Json(
        mappings.iter().map(saml_role_mapping_to_response).collect(),
    ))
}

#[utoipa::path(
    post,
    path = "/admin/saml/providers/{provider_id}/role-mappings",
    request_body = CreateSamlRoleMappingRequest,
    responses((status = 201, description = "Role mapping created", body = SamlRoleMappingResponse)),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn create_saml_role_mapping(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(provider_id): Path<i32>,
    Json(request): Json<CreateSamlRoleMappingRequest>,
) -> Result<(StatusCode, Json<SamlRoleMappingResponse>), Problem> {
    permission_guard!(auth, SettingsWrite);
    let mapping = state
        .saml_service
        .create_role_mapping(provider_id, request)
        .await?;

    if let Err(e) = state
        .audit_service
        .create_audit_log(&crate::audit::SamlRoleMappingCreatedAudit {
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
        error!("Failed to create SAML role mapping audit log: {}", e);
    }

    Ok((
        StatusCode::CREATED,
        Json(saml_role_mapping_to_response(&mapping)),
    ))
}

#[utoipa::path(
    delete,
    path = "/admin/saml/role-mappings/{mapping_id}",
    responses((status = 204, description = "Role mapping deleted")),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn delete_saml_role_mapping(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(mapping_id): Path<i32>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, SettingsWrite);
    state.saml_service.delete_role_mapping(mapping_id).await?;

    if let Err(e) = state
        .audit_service
        .create_audit_log(&crate::audit::SamlRoleMappingDeletedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.to_string()),
                user_agent: metadata.user_agent.as_str().to_string(),
            },
            mapping_id,
        })
        .await
    {
        error!("Failed to create SAML role mapping delete audit log: {}", e);
    }

    Ok(StatusCode::NO_CONTENT)
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_public_providers,
        start_saml_login_by_slug,
        saml_acs,
        saml_sp_metadata,
        create_saml_provider,
        list_saml_providers,
        update_saml_provider,
        delete_saml_provider,
        test_saml_provider,
        refresh_saml_metadata,
        list_saml_provider_users,
        list_saml_role_mappings,
        create_saml_role_mapping,
        delete_saml_role_mapping,
    ),
    components(
        schemas(
            SamlProvidersListResponse,
            CreateSamlProviderRequest,
            SamlProviderResponse,
            SamlProviderSummary,
            SamlProviderUserResponse,
            UpdateSamlProviderRequest,
            SamlTestConnectionResponse,
            SamlRoleMappingResponse,
            CreateSamlRoleMappingRequest,
        )
    ),
    tags((name = "Authentication", description = "Authentication and authorization endpoints"))
)]
pub struct SamlApiDoc;
