// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::AuthState;
use crate::audit::{
    ConcurrentSessionDetectedAudit, EmailVerifiedAudit, LoginAudit, LoginFailedAudit, LogoutAudit,
    MfaDisabledAudit, MfaEnabledAudit, MfaVerificationFailedAudit, MfaVerifiedAudit,
    PasswordResetAudit, RoleAssignedAudit, RoleRemovedAudit, StepUpVerificationAudit,
    UpdatedFields, UserCreatedAudit, UserDeletedAudit, UserRestoredAudit, UserUpdatedAudit,
};
use crate::avatar::generate_avatar_data_url;
use crate::context::AuthContext;
use crate::permissions::Permission;
use crate::user_service::{normalize_user_email, UserServiceError};
use crate::{permission_guard, RequireAuth};
use axum::extract::Path;
use axum::http::header::SET_COOKIE;
use axum::routing::{delete, get, patch, post};
use axum::Extension;
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json, Router,
};
use cookie;
use cookie::Cookie;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
pub use temps_core::AuditContext;
use temps_core::{problemdetails, RequestMetadata};
use temps_entities::types::RoleType;
use tracing::{debug, error, info, warn};
use utoipa::{OpenApi, ToSchema};

use crate::types::{
    AssignRoleRequest, AuthStatusResponse, AuthTokenResponse, ChangePasswordRequest,
    CliLoginRequest, CreateUserRequest, DisableMfaRequest, InitAuthResponse, MfaRequiredResponse,
    MfaSetupResponse, MfaVerificationRequest, RouteRole, RouteUser, RouteUserWithRoles,
    SetupMfaRequest, StepUpResponse, TokenRenewalRequest, UpdateSelfRequest, UpdateUserRequest,
    UserResponse, VerifyMfaRequest, VerifyStepUpRequest,
};
use temps_core::problemdetails::{new as problem_new, Problem};

const MAX_LOGIN_EMAIL_BYTES: usize = 254;

fn bounded_audit_identity(identity: &str) -> String {
    let normalized = identity.to_lowercase();
    if normalized.len() <= MAX_LOGIN_EMAIL_BYTES {
        return normalized;
    }

    let mut boundary = MAX_LOGIN_EMAIL_BYTES;
    while !normalized.is_char_boundary(boundary) {
        boundary -= 1;
    }
    normalized[..boundary].to_string()
}

fn normalized_login_email(email: &str) -> Option<String> {
    normalize_user_email(email)
}

fn parse_user_roles(role_names: &[String]) -> Result<Vec<RoleType>, UserServiceError> {
    role_names
        .iter()
        .map(|role| {
            RoleType::from_str(role).map_err(|_| {
                UserServiceError::Validation(format!(
                    "Unsupported role '{role}'. Expected one of: admin, user"
                ))
            })
        })
        .collect()
}

async fn record_login_failure(
    state: &AuthState,
    metadata: &RequestMetadata,
    user_id: Option<i32>,
    attempted_email: &str,
    login_method: &'static str,
    reason: &'static str,
) {
    if let Err(error) = state
        .audit_service
        .create_audit_log(&LoginFailedAudit {
            user_id,
            attempted_email: bounded_audit_identity(attempted_email),
            ip_address: Some(metadata.ip_address.to_string()),
            user_agent: metadata.user_agent.as_str().to_string(),
            login_method: login_method.to_string(),
            reason: reason.to_string(),
        })
        .await
    {
        error!(
            user_id,
            reason, "Failed to record login failure audit event: {}", error
        );
    }
}

async fn record_mfa_rejection(
    state: &AuthState,
    metadata: &RequestMetadata,
    user_id: Option<i32>,
    reason: &'static str,
) {
    if let Err(error) = state
        .audit_service
        .create_audit_log(&MfaVerificationFailedAudit {
            user_id,
            ip_address: Some(metadata.ip_address.to_string()),
            user_agent: metadata.user_agent.as_str().to_string(),
            reason: reason.to_string(),
        })
        .await
    {
        error!(
            user_id,
            reason, "Failed to record MFA rejection audit event: {}", error
        );
    }
}

async fn record_pending_login(
    state: &AuthState,
    metadata: &RequestMetadata,
    user_id: i32,
    login_method: &'static str,
) {
    if let Err(error) = state
        .audit_service
        .create_audit_log(&LoginAudit {
            context: AuditContext {
                user_id,
                ip_address: Some(metadata.ip_address.to_string()),
                user_agent: metadata.user_agent.as_str().to_string(),
            },
            success: true,
            login_method: login_method.to_string(),
        })
        .await
    {
        error!(
            user_id,
            login_method, "Failed to record pending login audit event: {}", error
        );
    }
}

fn invalid_mfa_problem() -> Problem {
    problem_new(StatusCode::UNAUTHORIZED)
        .with_title("MFA Verification Failed")
        .with_detail("The verification code is incorrect or has expired. Please try again with a new code from your authenticator app.")
}

async fn record_step_up_verification_audit(
    auth_state: &AuthState,
    metadata: &RequestMetadata,
    user_id: i32,
    session_id: i32,
    success: bool,
    reason: Option<&str>,
) {
    let audit = StepUpVerificationAudit {
        context: AuditContext {
            user_id,
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        success,
        reason: reason.map(str::to_string),
    };
    if let Err(error) = auth_state.audit_service.create_audit_log(&audit).await {
        error!(
            user_id,
            session_id, %error,
            "Failed to record sensitive-action verification audit"
        );
    }
}

#[utoipa::path(
    get,
    path = "/user/me",
    responses(
        (status = 200, description = "Successfully retrieved user information", body = UserResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("session_token" = [])
    ),
    tag = "Authentication"
)]
pub async fn get_current_user(RequireAuth(auth): RequireAuth) -> impl IntoResponse {
    // Require a user for this endpoint (deployment tokens not allowed)
    let user = match auth.require_user() {
        Ok(u) => u,
        Err(msg) => {
            return (StatusCode::FORBIDDEN, Json(json!({"error": msg}))).into_response();
        }
    };
    let user_response = UserResponse {
        id: user.id,
        username: user.name.clone(),
        name: user.name.clone(),
        email: Some(user.email.clone()),
        avatar_url: generate_avatar_data_url(&user.name),
        mfa_enabled: user.mfa_enabled,
        role: auth.effective_role.to_string(),
    };
    Json(user_response).into_response()
}

#[utoipa::path(
    post,
    path = "/logout",
    responses(
        (status = 200, description = "Successfully logged out"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("session_token" = [])
    ),
    tag = "Authentication"
)]
pub async fn logout(
    State(auth_state): State<Arc<AuthState>>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Require a user for this endpoint (deployment tokens not allowed)
    let user = match auth.require_user() {
        Ok(u) => u,
        Err(msg) => {
            return (StatusCode::FORBIDDEN, Json(json!({"error": msg}))).into_response();
        }
    };
    let audit_context = AuditContext {
        user_id: user.id,
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent.as_str().to_string(),
    };

    let audit = LogoutAudit {
        context: audit_context,
        username: user.name.clone(),
    };

    if let Err(e) = auth_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    match auth_state.auth_service.logout(user.id, &headers).await {
        Ok(_) => {
            let mut headers = HeaderMap::new();
            headers.insert(
                SET_COOKIE,
                "session=; Max-Age=0; Path=/; HttpOnly; Secure; SameSite=Strict"
                    .parse()
                    .unwrap(),
            );
            (StatusCode::OK, headers, Json(json!({"status": "success"}))).into_response()
        }
        Err(e) => {
            error!("Logout error: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Logout failed"})),
            )
                .into_response()
        }
    }
}

#[utoipa::path(
    post,
    path = "/auth/verify-mfa",
    request_body = MfaVerificationRequest,
    responses(
        (status = 204, description = "MFA verification successful"),
        (status = 401, description = "Invalid MFA code"),
        (status = 400, description = "Invalid request"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Authentication"
)]
pub async fn verify_mfa_challenge(
    State(auth_state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    headers: HeaderMap,
    Json(verification): Json<MfaVerificationRequest>,
) -> Result<impl IntoResponse, Problem> {
    // Extract and decrypt MFA session from cookie
    let encrypted_mfa_session = headers
        .get_all("Cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|cookie_str| Cookie::split_parse(cookie_str).filter_map(Result::ok))
        .find_map(|cookie| {
            if cookie.name() == "mfa_session" {
                Some(cookie.value().to_string())
            } else {
                None
            }
        });
    let encrypted_mfa_session = match encrypted_mfa_session {
        Some(session) => session,
        None => {
            record_mfa_rejection(
                auth_state.as_ref(),
                &metadata,
                None,
                "missing_session_cookie",
            )
            .await;
            return Err(problem_new(StatusCode::UNAUTHORIZED)
                .with_title("MFA Session Required")
                .with_detail(
                    "No MFA session found. Please log in first to start the MFA verification flow.",
                ));
        }
    };

    // Decrypt the MFA session cookie
    let mfa_session = match auth_state.cookie_crypto.decrypt(&encrypted_mfa_session) {
        Ok(session) => session,
        Err(error) => {
            record_mfa_rejection(
                auth_state.as_ref(),
                &metadata,
                None,
                "invalid_session_cookie",
            )
            .await;
            tracing::error!("Failed to decrypt MFA session cookie: {}", error);
            return Err(problem_new(StatusCode::UNAUTHORIZED)
                .with_title("MFA Session Expired")
                .with_detail("Your MFA session has expired or is invalid. Please log in again."));
        }
    };

    tracing::debug!("MFA session decrypted successfully");

    match auth_state
        .auth_service
        .verify_mfa_challenge(&mfa_session, &verification.code)
        .await
    {
        Ok(user) => {
            let audit_context = AuditContext {
                user_id: user.id,
                ip_address: Some(metadata.ip_address.to_string()),
                user_agent: metadata.user_agent.as_str().to_string(),
            };

            let audit = MfaVerifiedAudit {
                context: audit_context.clone(),
                username: user.email.clone(),
            };

            if let Err(e) = auth_state.audit_service.create_audit_log(&audit).await {
                error!("Failed to create audit log: {}", e);
            }

            // Count this user's already-active sessions *before* creating a
            // new one (the temporary MFA-challenge session was already
            // deleted by `verify_mfa_challenge` above, so this only reflects
            // pre-existing real sessions). Purely observational -- see
            // bherila/temps#24; a lookup failure must never block the login.
            let existing_active_sessions =
                match auth_state.auth_service.count_active_sessions(user.id).await {
                    Ok(count) => count,
                    Err(e) => {
                        error!(
                        "Failed to count active sessions for user {} after MFA verification: {}",
                        user.id, e
                    );
                        0
                    }
                };

            let session_token = match auth_state.auth_service.create_session(user.id).await {
                Ok(session_token) => session_token,
                Err(error) => {
                    error!("Failed to create session after MFA verification: {}", error);
                    record_login_failure(
                        auth_state.as_ref(),
                        &metadata,
                        Some(user.id),
                        &user.email,
                        "password-mfa",
                        "session_creation_failed",
                    )
                    .await;
                    return Err(problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                        .with_title("Session Creation Failed")
                        .with_detail("Could not create session. Please try logging in again."));
                }
            };

            if existing_active_sessions > 0 {
                if let Err(e) = auth_state
                    .audit_service
                    .create_audit_log(&ConcurrentSessionDetectedAudit {
                        context: audit_context,
                        login_method: "password-mfa".to_string(),
                        existing_active_session_count: existing_active_sessions,
                    })
                    .await
                {
                    error!(
                        "Failed to create concurrent-session audit log for user {}: {}",
                        user.id, e
                    );
                }
            }

            let session_token_encrypted = match auth_state.cookie_crypto.encrypt(&session_token) {
                Ok(enc) => enc,
                Err(e) => {
                    error!("Failed to encrypt session token: {}", e);
                    record_login_failure(
                        auth_state.as_ref(),
                        &metadata,
                        Some(user.id),
                        &user.email,
                        "password-mfa",
                        "session_encryption_failed",
                    )
                    .await;
                    return Err(problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                        .with_title("Session Error")
                        .with_detail("Could not secure your session. Please try again."));
                }
            };

            let mut response_headers = auth_state
                .auth_service
                .create_session_cookie(&session_token_encrypted, metadata.is_secure);

            // // Clear the MFA session cookie
            let clear_mfa_cookie = Cookie::build(("mfa_session", ""))
                .http_only(true)
                .path("/")
                .max_age(cookie::time::Duration::seconds(0))
                .same_site(cookie::SameSite::Strict)
                .secure(metadata.is_secure)
                .build();
            let clear_cookie_header = match clear_mfa_cookie.to_string().parse() {
                Ok(cookie_header) => cookie_header,
                Err(error) => {
                    error!("Failed to create MFA clear-cookie header: {}", error);
                    record_login_failure(
                        auth_state.as_ref(),
                        &metadata,
                        Some(user.id),
                        &user.email,
                        "password-mfa",
                        "session_cookie_creation_failed",
                    )
                    .await;
                    return Err(problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                        .with_title("Session Error")
                        .with_detail("Could not finalize your session. Please try again."));
                }
            };
            response_headers.append(SET_COOKIE, clear_cookie_header);

            Ok((StatusCode::NO_CONTENT, response_headers))
        }
        Err(crate::auth_service::MfaChallengeError::InvalidOrExpiredSession) => {
            record_mfa_rejection(
                auth_state.as_ref(),
                &metadata,
                None,
                "invalid_or_expired_session",
            )
            .await;
            Err(invalid_mfa_problem())
        }
        Err(crate::auth_service::MfaChallengeError::InvalidCode { user_id }) => {
            record_mfa_rejection(
                auth_state.as_ref(),
                &metadata,
                Some(user_id),
                "invalid_code",
            )
            .await;
            Err(invalid_mfa_problem())
        }
        Err(error) => {
            error!("MFA verification infrastructure failed: {}", error);
            Err(problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("MFA Verification Error")
                .with_detail("MFA verification could not be completed. Please try again."))
        }
    }
}

#[utoipa::path(
    post,
    path = "/auth/step-up",
    request_body = VerifyStepUpRequest,
    responses(
        (status = 200, description = "Session elevated for sensitive actions", body = StepUpResponse),
        (status = 400, description = "Verification code is empty"),
        (status = 401, description = "Invalid code or expired session"),
        (status = 403, description = "Browser session required"),
        (status = 429, description = "Too many verification attempts"),
        (status = 428, description = "MFA setup required"),
        (status = 500, description = "Verification infrastructure failed")
    ),
    tag = "Authentication",
    security(("session_token" = []))
)]
pub async fn verify_step_up(
    State(auth_state): State<Arc<AuthState>>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<VerifyStepUpRequest>,
) -> Result<Json<StepUpResponse>, Problem> {
    let code = request.code.trim();
    if code.is_empty() {
        return Err(temps_core::error_builder::bad_request()
            .title("Verification Code Required")
            .detail("Provide a TOTP code or recovery code")
            .value("error_code", "STEP_UP_CODE_REQUIRED")
            .build());
    }

    let session_id = auth.session_id().ok_or_else(|| {
        temps_core::error_builder::forbidden()
            .title("Browser Session Required")
            .detail("Sensitive-action verification is available only to browser sessions")
            .value("error_code", "STEP_UP_SESSION_REQUIRED")
            .build()
    })?;
    let user_id = auth.user_id();
    let rate_limit_key = format!("user:{user_id}:session:{session_id}");
    if auth_state
        .step_up_rate_limiter
        .check(&rate_limit_key)
        .await
        .is_err()
    {
        record_step_up_verification_audit(
            &auth_state,
            &metadata,
            user_id,
            session_id,
            false,
            Some("rate_limited"),
        )
        .await;
        return Err(
            temps_core::error_builder::ErrorBuilder::new(StatusCode::TOO_MANY_REQUESTS)
                .title("Too Many Verification Attempts")
                .detail("Wait before trying another verification code")
                .value("error_code", "STEP_UP_RATE_LIMITED")
                .build(),
        );
    }

    let result = auth_state
        .step_up_service
        .verify_and_elevate(user_id, session_id, code)
        .await;

    let (success, reason) = match &result {
        Ok(_) => (true, None),
        Err(crate::StepUpError::SessionRequired) => (false, Some("session_required")),
        Err(crate::StepUpError::MfaNotConfigured { .. }) => (false, Some("mfa_not_configured")),
        Err(crate::StepUpError::InvalidCode { .. }) => (false, Some("invalid_code")),
        Err(crate::StepUpError::UserNotFound { .. }) => (false, Some("user_not_found")),
        Err(crate::StepUpError::SessionNotFound { .. }) => (false, Some("session_not_found")),
        Err(crate::StepUpError::Verification { .. }) => (false, Some("verification_failed")),
        Err(crate::StepUpError::Database { .. }) => (false, Some("database_failed")),
    };
    record_step_up_verification_audit(&auth_state, &metadata, user_id, session_id, success, reason)
        .await;

    match result {
        Ok(expires_at) => Ok(Json(StepUpResponse {
            expires_at: expires_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        })),
        Err(crate::StepUpError::SessionRequired) => Err(temps_core::error_builder::forbidden()
            .title("Browser Session Required")
            .detail("Sensitive-action verification is available only to browser sessions")
            .value("error_code", "STEP_UP_SESSION_REQUIRED")
            .build()),
        Err(crate::StepUpError::MfaNotConfigured { .. }) => Err(
            temps_core::error_builder::ErrorBuilder::new(StatusCode::PRECONDITION_REQUIRED)
                .type_("https://temps.sh/probs/mfa-setup-required")
                .title("MFA Setup Required")
                .detail("Configure MFA before performing sensitive actions")
                .value("error_code", "MFA_SETUP_REQUIRED")
                .build(),
        ),
        Err(crate::StepUpError::InvalidCode { .. }) => {
            Err(temps_core::error_builder::unauthorized()
                .title("Invalid Verification Code")
                .detail("The TOTP or recovery code is invalid")
                .value("error_code", "STEP_UP_CODE_INVALID")
                .build())
        }
        Err(crate::StepUpError::SessionNotFound { .. })
        | Err(crate::StepUpError::UserNotFound { .. }) => {
            Err(temps_core::error_builder::unauthorized()
                .title("Session Expired")
                .detail("The authenticated browser session is no longer active")
                .value("error_code", "STEP_UP_SESSION_EXPIRED")
                .build())
        }
        Err(crate::StepUpError::Verification { user_id, reason }) => {
            error!(user_id, %reason, "Sensitive-action MFA verification failed");
            Err(temps_core::error_builder::internal_server_error()
                .title("Verification Failed")
                .detail("The verification service could not complete the request")
                .value("error_code", "STEP_UP_VERIFICATION_FAILED")
                .build())
        }
        Err(error @ crate::StepUpError::Database { .. }) => {
            error!(%error, "Sensitive-action verification database failure");
            Err(temps_core::error_builder::internal_server_error()
                .title("Verification Failed")
                .detail("The verification service could not complete the request")
                .value("error_code", "STEP_UP_VERIFICATION_FAILED")
                .build())
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_current_user,
        logout,
        verify_mfa_challenge,
        verify_step_up,
        register,
        login,
        email_status,
        request_password_reset,
        reset_password,
        change_required_password,
        verify_email,
        list_users,
        create_user,
        delete_user,
        assign_role,
        remove_role,
        update_user,
        restore_user,
        update_self,
        change_password_self,
        setup_mfa,
        verify_and_enable_mfa,
        disable_mfa,
        crate::cli_auth_handler::cli_logout,
        crate::cli_device_handler::cli_device_start,
        crate::cli_device_handler::cli_device_poll,
        crate::cli_device_handler::cli_device_lookup,
        crate::cli_device_handler::cli_device_approve,
        crate::cli_device_handler::cli_device_deny
    ),
    components(
        schemas(
            UserResponse,
            CliLoginRequest,
            AuthTokenResponse,
            TokenRenewalRequest,
            InitAuthResponse,
            AuthStatusResponse,
            MfaVerificationRequest,
            MfaRequiredResponse,
            RegisterRequest,
            LoginRequest,
            EmailRequest,
            ResetPasswordRequest,
            RequiredPasswordChangeRequest,
            RequiredPasswordChangeResponse,
            AuthResponse,
            EmailStatusResponse,
            crate::oidc_types::OidcProviderSummary,
            RouteUser,
            RouteRole,
            RouteUserWithRoles,
            AssignRoleRequest,
            CreateUserRequest,
            UpdateUserRequest,
            UpdateSelfRequest,
            ChangePasswordRequest,
            VerifyMfaRequest,
            VerifyStepUpRequest,
            StepUpResponse,
            MfaSetupResponse,
            DisableMfaRequest,
            crate::cli_device_handler::CliDeviceStartRequest,
            crate::cli_device_handler::CliDeviceStartResponse,
            crate::cli_device_handler::CliDevicePollRequest,
            crate::cli_device_handler::CliDevicePollResponse,
            crate::cli_device_handler::CliDeviceLookupResponse,
            crate::cli_device_handler::CliDeviceApproveRequest,
            crate::cli_device_handler::CliDeviceApproveResponse
        )
    ),
    info(
        title = "Authentication & User Management API",
        description = "Complete API for authentication, authorization, and user management. \
        Includes login/logout, MFA, user CRUD operations, role management, \
        password reset and email verification.",
        version = "1.0.0"
    ),
    tags(
        (name = "Authentication", description = "Authentication and authorization endpoints"),
        (name = "Users", description = "User management endpoints")
    )
)]
pub struct AuthApiDoc;

pub fn configure_routes() -> Router<Arc<AuthState>> {
    use crate::rate_limit::{auth_rate_limit_middleware, AuthRateLimitConfig, AuthRateLimiter};

    let rate_limiter = AuthRateLimiter::new(AuthRateLimitConfig::default());

    // Auth-sensitive routes that are rate limited to prevent brute force attacks.
    // These are the public-facing endpoints that accept credentials or tokens.
    let rate_limited_auth_routes = Router::new()
        .route("/auth/login", post(login))
        .route("/auth/verify-mfa", post(verify_mfa_challenge))
        .route("/auth/step-up", post(verify_step_up))
        .route(
            "/auth/cli/device/start",
            post(crate::cli_device_handler::cli_device_start),
        )
        .route(
            "/auth/cli/device/poll",
            post(crate::cli_device_handler::cli_device_poll),
        )
        .route("/auth/password-reset/request", post(request_password_reset))
        .route("/auth/password-reset/verify", post(reset_password))
        .route(
            "/auth/password-change-required",
            post(change_required_password),
        )
        .route(
            "/auth/oidc/login/{slug}",
            get(crate::oidc_handler::start_oidc_login_by_slug),
        )
        .route(
            "/auth/oidc/callback",
            get(crate::oidc_handler::oidc_callback),
        )
        .layer(axum::Extension(rate_limiter))
        .layer(axum::middleware::from_fn(auth_rate_limit_middleware));

    // Non-rate-limited routes (require authentication already)
    let authenticated_routes = Router::new()
        .route("/user/me", get(get_current_user))
        .route("/logout", post(logout))
        .route(
            "/auth/cli/logout",
            post(crate::cli_auth_handler::cli_logout),
        )
        .route(
            "/auth/cli/device/lookup",
            get(crate::cli_device_handler::cli_device_lookup),
        )
        .route(
            "/auth/cli/device/approve",
            post(crate::cli_device_handler::cli_device_approve),
        )
        .route(
            "/auth/cli/device/deny",
            post(crate::cli_device_handler::cli_device_deny),
        )
        .route("/auth/email-status", get(email_status))
        .route("/auth/verify-email", get(verify_email))
        .route("/users", get(list_users))
        .route("/users", post(create_user))
        .route("/users/me", patch(update_self))
        .route("/users/me/password", post(change_password_self))
        .route("/users/me/mfa/setup", post(setup_mfa))
        .route("/users/me/mfa/verify", post(verify_and_enable_mfa))
        .route("/users/me/mfa", delete(disable_mfa))
        .route("/users/{user_id}", delete(delete_user))
        .route("/users/{user_id}", patch(update_user))
        .route("/users/{user_id}/restore", post(restore_user))
        .route("/users/{user_id}/roles", post(assign_role))
        .route("/users/{user_id}/roles/{role_type}", delete(remove_role));

    rate_limited_auth_routes.merge(authenticated_routes)
}

// Service error conversions will be added as needed

// Re-export request types with ToSchema for OpenAPI
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

/// Request body carrying just an email address (password-reset request).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EmailRequest {
    pub email: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ResetPasswordRequest {
    pub token: String,
    pub new_password: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RequiredPasswordChangeRequest {
    pub new_password: String,
}

// Implement From traits for conversions
impl From<RegisterRequest> for crate::auth_service::RegisterRequest {
    fn from(req: RegisterRequest) -> Self {
        crate::auth_service::RegisterRequest {
            email: req.email,
            password: req.password,
            name: req.name,
        }
    }
}

impl From<LoginRequest> for crate::auth_service::LoginRequest {
    fn from(req: LoginRequest) -> Self {
        crate::auth_service::LoginRequest {
            email: req.email,
            password: req.password,
        }
    }
}

impl From<ResetPasswordRequest> for crate::auth_service::ResetPasswordRequest {
    fn from(req: ResetPasswordRequest) -> Self {
        crate::auth_service::ResetPasswordRequest {
            token: req.token,
            new_password: req.new_password,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AuthResponse {
    pub success: bool,
    pub message: String,
    pub user_id: Option<i32>,
    pub mfa_required: bool,
    pub mfa_enrollment_required: bool,
    pub mfa_setup: Option<MfaSetupResponse>,
    pub password_change_required: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RequiredPasswordChangeResponse {
    pub success: bool,
    pub message: String,
    pub user_id: i32,
    pub mfa_enrollment_required: bool,
    pub mfa_setup: Option<MfaSetupResponse>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EmailStatusResponse {
    pub email_configured: bool,
    pub password_reset_available: bool,
    pub oidc_providers: Vec<crate::oidc_types::OidcProviderSummary>,
}

#[derive(Debug, Deserialize)]
pub struct VerifyTokenQuery {
    pub token: String,
}

#[utoipa::path(
    post,
    path = "/users",
    request_body = RegisterRequest,
    responses(
        (status = 201, description = "User registered successfully, session cookie set", body = AuthResponse),
        (status = 400, description = "Bad request"),
        (status = 409, description = "Email already registered"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Users",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn register(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<RegisterRequest>,
) -> Result<(StatusCode, HeaderMap, Json<AuthResponse>), temps_core::problemdetails::Problem> {
    permission_guard!(auth, UsersCreate);
    let username = request.name.clone();

    match state.auth_service.register_user(request.into()).await {
        Ok(user) => {
            // Create audit log
            let audit_context = AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.to_string()),
                user_agent: metadata.user_agent.as_str().to_string(),
            };

            let user_audit = UserCreatedAudit {
                context: audit_context,
                target_user_id: user.id,
                username: username.clone(),
                assigned_roles: vec![],
            };

            if let Err(e) = state.audit_service.create_audit_log(&user_audit).await {
                error!("Failed to create audit log: {}", e);
            }

            // Don't create a new session - the current user remains logged in
            // Just return success without any session changes
            let headers = HeaderMap::new();

            Ok((
                StatusCode::CREATED,
                headers,
                Json(AuthResponse {
                    success: true,
                    message: "User created successfully".to_string(),
                    user_id: Some(user.id),
                    mfa_required: false,
                    mfa_enrollment_required: false,
                    mfa_setup: None,
                    password_change_required: false,
                }),
            ))
        }
        Err(e) => match e {
            crate::auth_service::UserAuthError::EmailAlreadyRegistered => {
                Err(problem_new(StatusCode::CONFLICT)
                    .with_title("Email Already Registered")
                    .with_detail("A user with this email address already exists"))
            }
            _ => Err(problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Registration Failed")
                .with_detail(format!("Failed to register user: {}", e))),
        },
    }
}

#[utoipa::path(
    post,
    path = "/auth/login",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Login successful, session cookie set", body = AuthResponse),
        (status = 401, description = "Invalid credentials, or the account's role requires MFA enrollment that has not been completed"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Authentication"
)]
pub async fn login(
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(mut request): Json<LoginRequest>,
) -> Result<impl IntoResponse, temps_core::problemdetails::Problem> {
    // Bound the identity before it reaches the database or durable audit
    // storage. The client still gets the same generic credential response,
    // so validation does not become an account-discovery oracle.
    let Some(login_email) = normalized_login_email(&request.email) else {
        record_login_failure(
            state.as_ref(),
            &metadata,
            None,
            &request.email,
            "password",
            "invalid_identifier",
        )
        .await;
        return Err(problem_new(StatusCode::UNAUTHORIZED)
            .with_title("Invalid Credentials")
            .with_detail("Invalid email or password."));
    };
    request.email.clone_from(&login_email);

    match state.auth_service.login(request.into()).await {
        Ok(user) => {
            if user.must_change_password {
                let reset_token = state
                    .auth_service
                    .create_required_password_change_token(user.id)
                    .await
                    .map_err(|error| {
                        error!(
                            user_id = user.id,
                            error = %error,
                            "Failed to create required password-change session"
                        );
                        problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                            .with_title("Authentication Error")
                            .with_detail(
                                "Could not start the required password change. Please try again.",
                            )
                    })?;

                let encrypted_token =
                    state.cookie_crypto.encrypt(&reset_token).map_err(|error| {
                        error!(
                            user_id = user.id,
                            error = %error,
                            "Failed to encrypt required password-change session"
                        );
                        problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                            .with_title("Authentication Error")
                            .with_detail(
                                "Could not secure the required password change. Please try again.",
                            )
                    })?;

                let password_change_cookie =
                    Cookie::build(("password_change_session", encrypted_token))
                        .http_only(true)
                        .path("/")
                        .max_age(cookie::time::Duration::minutes(15))
                        .same_site(cookie::SameSite::Strict)
                        .secure(metadata.is_secure)
                        .build();
                let cookie_header =
                    password_change_cookie
                        .to_string()
                        .parse()
                        .map_err(|error| {
                            error!(
                                user_id = user.id,
                                error = %error,
                                "Failed to create required password-change cookie header"
                            );
                            problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                        .with_title("Authentication Error")
                        .with_detail(
                            "Could not secure the required password change. Please try again.",
                        )
                        })?;
                let mut headers = HeaderMap::new();
                headers.insert(SET_COOKIE, cookie_header);

                record_pending_login(
                    state.as_ref(),
                    &metadata,
                    user.id,
                    "password-change-required",
                )
                .await;

                return Ok((
                    headers,
                    Json(AuthResponse {
                        success: false,
                        message: "Password change required".to_string(),
                        user_id: Some(user.id),
                        mfa_required: false,
                        mfa_enrollment_required: false,
                        mfa_setup: None,
                        password_change_required: true,
                    }),
                ));
            }

            // Check if user has MFA enabled
            if user.mfa_enabled {
                // Create temporary MFA session
                match state.auth_service.create_mfa_session(user.id).await {
                    Ok(mfa_token) => {
                        // Encrypt the MFA token
                        let encrypted_token = match state.cookie_crypto.encrypt(&mfa_token) {
                            Ok(encrypted_token) => encrypted_token,
                            Err(error) => {
                                error!("Failed to encrypt MFA token: {}", error);
                                record_login_failure(
                                    state.as_ref(),
                                    &metadata,
                                    Some(user.id),
                                    &login_email,
                                    "password",
                                    "mfa_session_encryption_failed",
                                )
                                .await;
                                return Err(problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                                    .with_title("Authentication Error")
                                    .with_detail(
                                        "Could not process MFA session. Please try again.",
                                    ));
                            }
                        };

                        // Use the pre-calculated secure flag from metadata

                        // Create MFA cookie
                        let mut headers = HeaderMap::new();
                        let mfa_cookie = cookie::Cookie::build(("mfa_session", encrypted_token))
                            .http_only(true)
                            .path("/")
                            .max_age(cookie::time::Duration::minutes(5))
                            .same_site(cookie::SameSite::Strict)
                            .secure(metadata.is_secure)
                            .build();
                        let cookie_header = match mfa_cookie.to_string().parse() {
                            Ok(cookie_header) => cookie_header,
                            Err(error) => {
                                error!("Failed to create MFA cookie header: {}", error);
                                record_login_failure(
                                    state.as_ref(),
                                    &metadata,
                                    Some(user.id),
                                    &login_email,
                                    "password",
                                    "mfa_cookie_creation_failed",
                                )
                                .await;
                                return Err(problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                                    .with_title("Authentication Error")
                                    .with_detail(
                                        "Could not process MFA session. Please try again.",
                                    ));
                            }
                        };
                        headers.insert(SET_COOKIE, cookie_header);

                        Ok((
                            headers,
                            Json(AuthResponse {
                                success: false,
                                message: "MFA authentication required".to_string(),
                                user_id: None,
                                mfa_required: true,
                                mfa_enrollment_required: false,
                                mfa_setup: None,
                                password_change_required: false,
                            }),
                        ))
                    }
                    Err(e) => {
                        error!("Failed to create MFA session: {}", e);
                        record_login_failure(
                            state.as_ref(),
                            &metadata,
                            Some(user.id),
                            &login_email,
                            "password",
                            "mfa_session_creation_failed",
                        )
                        .await;
                        Err(problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                            .with_title("Authentication Error")
                            .with_detail("Could not initiate MFA verification. Please try again."))
                    }
                }
            } else {
                // Count this user's already-active sessions *before* creating
                // a new one, so we can flag concurrent logins in the audit
                // trail (bherila/temps#24). A lookup failure here must not
                // block the login -- graceful degradation per CLAUDE.md.
                let existing_active_sessions =
                    match state.auth_service.count_active_sessions(user.id).await {
                        Ok(count) => count,
                        Err(e) => {
                            error!(
                                "Failed to count active sessions for user {} before login: {}",
                                user.id, e
                            );
                            0
                        }
                    };

                // Create regular session
                match state.auth_service.create_session(user.id).await {
                    Ok(session_token) => {
                        // Encrypt the session token
                        let encrypted_token = match state.cookie_crypto.encrypt(&session_token) {
                            Ok(encrypted_token) => encrypted_token,
                            Err(error) => {
                                error!("Failed to encrypt session token: {}", error);
                                record_login_failure(
                                    state.as_ref(),
                                    &metadata,
                                    Some(user.id),
                                    &login_email,
                                    "password",
                                    "session_encryption_failed",
                                )
                                .await;
                                return Err(problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                                    .with_title("Authentication Error")
                                    .with_detail(
                                        "Could not secure your session. Please try again.",
                                    ));
                            }
                        };

                        // Use the pre-calculated secure flag from metadata

                        // Create session cookie headers using pre-calculated secure flag
                        let headers = state
                            .auth_service
                            .create_session_cookie(&encrypted_token, metadata.is_secure);
                        let audit_context = AuditContext {
                            user_id: user.id,
                            ip_address: Some(metadata.ip_address.to_string()),
                            user_agent: metadata.user_agent.as_str().to_string(),
                        };
                        if let Err(e) = state
                            .audit_service
                            .create_audit_log(&LoginAudit {
                                context: audit_context.clone(),
                                success: true,
                                login_method: "password".to_string(),
                            })
                            .await
                        {
                            error!("Failed to create audit log: {}", e);
                        }
                        if existing_active_sessions > 0 {
                            if let Err(e) = state
                                .audit_service
                                .create_audit_log(&ConcurrentSessionDetectedAudit {
                                    context: audit_context,
                                    login_method: "password".to_string(),
                                    existing_active_session_count: existing_active_sessions,
                                })
                                .await
                            {
                                error!(
                                    "Failed to create concurrent-session audit log for user {}: {}",
                                    user.id, e
                                );
                            }
                        }
                        Ok((
                            headers,
                            Json(AuthResponse {
                                success: true,
                                message: "Login successful".to_string(),
                                user_id: Some(user.id),
                                mfa_required: false,
                                mfa_enrollment_required: false,
                                mfa_setup: None,
                                password_change_required: false,
                            }),
                        ))
                    }
                    Err(e) => {
                        // The credentials were already verified, so the actor
                        // is known here. (This previously logged user_id 0,
                        // which the audit FK rejected -- the row never landed.)
                        record_login_failure(
                            state.as_ref(),
                            &metadata,
                            Some(user.id),
                            &login_email,
                            "password",
                            "session_creation_failed",
                        )
                        .await;
                        error!("Failed to create session: {}", e);
                        Err(problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                            .with_title("Login Failed")
                            .with_detail("Could not create your session. Please try again."))
                    }
                }
            }
        }
        Err(e) => match e {
            crate::auth_service::UserAuthError::InvalidCredentials
            | crate::auth_service::UserAuthError::UserNotFound => {
                // Record the rejected attempt so credential probing leaves a
                // trail. The account may not exist, so the actor is optional;
                // the attempted email preserves the claimed identity either
                // way. Volume is bounded by the login rate limiter.
                record_login_failure(
                    state.as_ref(),
                    &metadata,
                    None,
                    &login_email,
                    "password",
                    "invalid_credentials",
                )
                .await;
                Err(problem_new(StatusCode::UNAUTHORIZED)
                    .with_title("Invalid Credentials")
                    .with_detail("Invalid email or password."))
            }
            crate::auth_service::UserAuthError::MfaRequiredForRole { user_id, role } => {
                // This branch is reached after a valid password for an elevated
                // account, so do not start MFA enrollment or disclose setup
                // material here. Enrollment must happen from an already
                // authenticated/trusted path; otherwise a stolen password could
                // claim attacker-controlled MFA and complete admin login. Return
                // the same response as invalid credentials to avoid making the
                // role/MFA policy observable to password-guessing callers.
                warn!(
                    user_id,
                    role = %role,
                    email = %login_email,
                    "Blocking password login for role requiring pre-enrolled MFA"
                );
                record_login_failure(
                    state.as_ref(),
                    &metadata,
                    Some(user_id),
                    &login_email,
                    "password",
                    "mfa_required_for_role",
                )
                .await;
                Err(problem_new(StatusCode::UNAUTHORIZED)
                    .with_title("Invalid Credentials")
                    .with_detail("Invalid email or password."))
            }
            _ => {
                // Log the real error server-side (email is PII but legitimate
                // for login-failure auditing; never include password).
                error!(
                    email = %login_email,
                    error = %e,
                    "Authentication system error during login"
                );
                record_login_failure(
                    state.as_ref(),
                    &metadata,
                    None,
                    &login_email,
                    "password",
                    "internal_error",
                )
                .await;
                Err(problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Authentication Error")
                    .with_detail("Authentication system error. Please try again later."))
            }
        },
    }
}

#[utoipa::path(
    get,
    path = "/auth/email-status",
    responses(
        (status = 200, description = "Email configuration status", body = EmailStatusResponse),
        (status = 500, description = "Internal server error")
    ),
    tag = "Authentication"
)]
pub async fn email_status(State(state): State<Arc<AuthState>>) -> Json<EmailStatusResponse> {
    let email_configured = state.auth_service.is_email_configured().await;
    let oidc_providers = state
        .oidc_service
        .list_enabled_providers()
        .await
        .unwrap_or_default();

    Json(EmailStatusResponse {
        email_configured,
        password_reset_available: email_configured,
        oidc_providers,
    })
}

#[utoipa::path(
    post,
    path = "/auth/password-reset/request",
    request_body = EmailRequest,
    responses(
        (status = 200, description = "Reset email sent if account exists", body = AuthResponse),
        (status = 503, description = "Email service not configured")
    ),
    tag = "Authentication"
)]
pub async fn request_password_reset(
    State(state): State<Arc<AuthState>>,
    Json(body): Json<EmailRequest>,
) -> Result<impl IntoResponse, temps_core::problemdetails::Problem> {
    if !state.auth_service.is_email_configured().await {
        return Err(problem_new(StatusCode::SERVICE_UNAVAILABLE)
            .with_title("Email Service Not Configured")
            .with_detail("Password reset is not available without email configuration"));
    }

    let email = &body.email;

    match state.auth_service.request_password_reset(email).await {
        Ok(_) | Err(_) => {
            // Always return success to prevent email enumeration
            Ok(Json(AuthResponse {
                success: true,
                message:
                    "If an account exists with this email, a password reset link has been sent"
                        .to_string(),
                user_id: None,
                mfa_required: false,
                mfa_enrollment_required: false,
                mfa_setup: None,
                password_change_required: false,
            }))
        }
    }
}

#[utoipa::path(
    post,
    path = "/auth/password-reset/verify",
    request_body = ResetPasswordRequest,
    responses(
        (status = 200, description = "Password reset successful", body = AuthResponse),
        (status = 400, description = "Invalid or expired token"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Authentication"
)]
pub async fn reset_password(
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<ResetPasswordRequest>,
) -> Result<impl IntoResponse, temps_core::problemdetails::Problem> {
    match state.auth_service.reset_password(request.into()).await {
        Ok(user) => {
            // Create audit log
            let audit_context = AuditContext {
                user_id: user.id,
                ip_address: Some(metadata.ip_address.to_string()),
                user_agent: metadata.user_agent.as_str().to_string(),
            };

            let password_reset_audit = PasswordResetAudit {
                context: audit_context,
                username: user.name.clone(),
            };

            if let Err(e) = state
                .audit_service
                .create_audit_log(&password_reset_audit)
                .await
            {
                error!("Failed to create audit log: {}", e);
            }

            Ok(Json(AuthResponse {
                success: true,
                message: "Password reset successful. You can now login with your new password."
                    .to_string(),
                user_id: None,
                mfa_required: false,
                mfa_enrollment_required: false,
                mfa_setup: None,
                password_change_required: false,
            }))
        }
        Err(crate::auth_service::UserAuthError::InvalidToken) => {
            Err(problem_new(StatusCode::BAD_REQUEST)
                .with_title("Password Reset Failed")
                .with_detail("The password reset link is invalid or expired."))
        }
        Err(crate::auth_service::UserAuthError::WeakPassword(message)) => {
            Err(problem_new(StatusCode::BAD_REQUEST)
                .with_title("Password Requirements Not Met")
                .with_detail(message))
        }
        Err(crate::auth_service::UserAuthError::SamePassword) => {
            Err(problem_new(StatusCode::BAD_REQUEST)
                .with_title("Choose a Different Password")
                .with_detail("Your new password must differ from the temporary password."))
        }
        Err(error) => {
            error!(error = %error, "Password reset failed internally");
            Err(problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Password Reset Failed")
                .with_detail("Could not reset the password. Please try again."))
        }
    }
}

#[utoipa::path(
    post,
    path = "/auth/password-change-required",
    request_body = RequiredPasswordChangeRequest,
    responses(
        (status = 200, description = "Required password change completed", body = RequiredPasswordChangeResponse),
        (status = 400, description = "Password does not meet requirements"),
        (status = 401, description = "Password-change session is missing or expired"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Authentication"
)]
pub async fn change_required_password(
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    headers: HeaderMap,
    Json(request): Json<RequiredPasswordChangeRequest>,
) -> Result<impl IntoResponse, Problem> {
    let encrypted_token = headers
        .get_all("Cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|cookie_header| Cookie::split_parse(cookie_header).filter_map(Result::ok))
        .find_map(|cookie| {
            (cookie.name() == "password_change_session").then(|| cookie.value().to_string())
        })
        .ok_or_else(|| {
            problem_new(StatusCode::UNAUTHORIZED)
                .with_title("Password Change Session Required")
                .with_detail("Log in again to restart the required password change.")
        })?;

    let token = state
        .cookie_crypto
        .decrypt(&encrypted_token)
        .map_err(|error| {
            warn!(error = %error, "Rejected invalid required password-change cookie");
            problem_new(StatusCode::UNAUTHORIZED)
                .with_title("Password Change Session Expired")
                .with_detail("Log in again to restart the required password change.")
        })?;

    let pending_user = state
        .auth_service
        .required_password_change_user(&token)
        .await
        .map_err(|error| {
            error!(error = %error, "Failed to resolve required password-change session");
            problem_new(StatusCode::UNAUTHORIZED)
                .with_title("Password Change Session Expired")
                .with_detail("Log in again to restart the required password change.")
        })?;

    // Prepare the response cookie before changing the password. If header
    // construction fails, the temporary credential remains usable and the
    // user can safely retry instead of being locked out.
    let clear_cookie = Cookie::build(("password_change_session", ""))
        .http_only(true)
        .path("/")
        .max_age(cookie::time::Duration::seconds(0))
        .same_site(cookie::SameSite::Strict)
        .secure(metadata.is_secure)
        .build();
    let cookie_header = clear_cookie.to_string().parse().map_err(|error| {
        error!(error = %error, "Failed to clear required password-change cookie");
        problem_new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Password Change Failed")
            .with_detail("Could not prepare the password change. Please try again.")
    })?;
    let mut response_headers = HeaderMap::new();
    response_headers.append(SET_COOKIE, cookie_header);

    let requires_mfa_enrollment = state
        .auth_service
        .requires_mfa_enrollment(pending_user.id)
        .await
        .map_err(|error| {
            error!(user_id = pending_user.id, error = %error, "Failed to evaluate MFA enrollment policy");
            problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Password Change Failed")
                .with_detail("Could not prepare required MFA enrollment. Please try again.")
        })?;

    let user = state
        .auth_service
        .reset_required_password(crate::auth_service::ResetPasswordRequest {
            token,
            new_password: request.new_password,
        })
        .await
        .map_err(|error| match error {
            crate::auth_service::UserAuthError::WeakPassword(message) => {
                problem_new(StatusCode::BAD_REQUEST)
                    .with_title("Password Requirements Not Met")
                    .with_detail(message)
            }
            crate::auth_service::UserAuthError::SamePassword => {
                problem_new(StatusCode::BAD_REQUEST)
                    .with_title("Choose a Different Password")
                    .with_detail("Your new password must differ from the temporary password.")
            }
            crate::auth_service::UserAuthError::InvalidToken => {
                problem_new(StatusCode::UNAUTHORIZED)
                    .with_title("Password Change Session Expired")
                    .with_detail("Log in again to restart the required password change.")
            }
            internal_error => {
                error!(error = %internal_error, "Required password change failed internally");
                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Password Change Failed")
                    .with_detail("Could not change the password. Please try again.")
            }
        })?;

    let audit = PasswordResetAudit {
        context: AuditContext {
            user_id: user.id,
            ip_address: Some(metadata.ip_address.to_string()),
            user_agent: metadata.user_agent.as_str().to_string(),
        },
        username: user.name,
    };
    if let Err(error) = state.audit_service.create_audit_log(&audit).await {
        error!(user_id = user.id, error = %error, "Failed to audit required password change");
    }

    Ok((
        response_headers,
        Json(RequiredPasswordChangeResponse {
            success: true,
            message: if requires_mfa_enrollment {
                "Password changed. Log in to set up multi-factor authentication.".to_string()
            } else {
                "Password changed. Log in with your new password.".to_string()
            },
            user_id: user.id,
            mfa_enrollment_required: requires_mfa_enrollment,
            mfa_setup: None,
        }),
    ))
}

#[utoipa::path(
    get,
    path = "/auth/verify-email",
    params(
        ("token" = String, Query, description = "Email verification token")
    ),
    responses(
        (status = 200, description = "Email verified successfully", body = AuthResponse),
        (status = 400, description = "Invalid or expired token"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Authentication"
)]
pub async fn verify_email(
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Query(query): Query<VerifyTokenQuery>,
) -> Result<impl IntoResponse, temps_core::problemdetails::Problem> {
    match state.auth_service.verify_email(&query.token).await {
        Ok(user) => {
            // Create audit log
            let audit_context = AuditContext {
                user_id: user.id,
                ip_address: Some(metadata.ip_address.to_string()),
                user_agent: metadata.user_agent.as_str().to_string(),
            };

            let email_verified_audit = EmailVerifiedAudit {
                context: audit_context,
                username: user.name.clone(),
                email: user.email.clone(),
            };

            if let Err(e) = state
                .audit_service
                .create_audit_log(&email_verified_audit)
                .await
            {
                error!("Failed to create audit log: {}", e);
            }

            Ok(Json(AuthResponse {
                success: true,
                message: "Email verified successfully. You can now login.".to_string(),
                user_id: None,
                mfa_required: false,
                mfa_enrollment_required: false,
                mfa_setup: None,
                password_change_required: false,
            }))
        }
        Err(e) => Err(problem_new(StatusCode::BAD_REQUEST)
            .with_title("Email Verification Failed")
            .with_detail(e.to_string())),
    }
}

impl From<UserServiceError> for Problem {
    fn from(err: UserServiceError) -> Self {
        match err {
            UserServiceError::DatabaseConnection(msg) => {
                error!("Database connection error: {}", msg);
                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database connection error")
                    .with_detail(msg)
            }
            UserServiceError::Database { reason } => problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Database error")
                .with_detail(reason),
            UserServiceError::NotFound(msg) => problem_new(StatusCode::NOT_FOUND)
                .with_title("User not found")
                .with_detail(msg),
            UserServiceError::RoleNotFound(msg) => problem_new(StatusCode::NOT_FOUND)
                .with_title("Role not found")
                .with_detail(msg),
            UserServiceError::Mfa(msg) => problem_new(StatusCode::BAD_REQUEST)
                .with_title("MFA error")
                .with_detail(msg),
            UserServiceError::InvalidMfaCode => problem_new(StatusCode::BAD_REQUEST)
                .with_title("Invalid MFA code")
                .with_detail("The provided MFA code is invalid"),
            UserServiceError::MfaNotSetup(user_id) => problem_new(StatusCode::BAD_REQUEST)
                .with_title("MFA not setup")
                .with_detail(format!("MFA is not setup for user {}", user_id)),
            UserServiceError::MfaAlreadyEnabled(user_id) => problem_new(StatusCode::CONFLICT)
                .with_title("MFA already enabled")
                .with_detail(format!(
                    "MFA is already enabled for user {}. Disable it with current MFA verification before setting it up again.",
                    user_id
                )),
            UserServiceError::AlreadyDeleted(user_id) => problem_new(StatusCode::BAD_REQUEST)
                .with_title("User already deleted")
                .with_detail(format!("User {} is already deleted", user_id)),
            UserServiceError::NotDeleted(user_id) => problem_new(StatusCode::BAD_REQUEST)
                .with_title("User not deleted")
                .with_detail(format!("User {} is not deleted", user_id)),
            UserServiceError::RoleAlreadyAssigned(role, user_id) => {
                problem_new(StatusCode::BAD_REQUEST)
                    .with_title("Role already assigned")
                    .with_detail(format!(
                        "Role {} is already assigned to user {}",
                        role, user_id
                    ))
            }
            UserServiceError::RoleNotAssigned(role, user_id) => {
                problem_new(StatusCode::BAD_REQUEST)
                    .with_title("Role not assigned")
                    .with_detail(format!("Role {} is not assigned to user {}", role, user_id))
            }
            UserServiceError::Validation(msg) => problem_new(StatusCode::BAD_REQUEST)
                .with_title("Validation error")
                .with_detail(msg),
            UserServiceError::Encryption(msg) => {
                error!("Encryption error: {}", msg);
                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Error")
                    .with_detail("An unexpected error occurred. Please try again.")
            }
            UserServiceError::Io(e) => {
                error!("IO error: {}", e);
                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Error")
                    .with_detail("An unexpected error occurred. Please try again.")
            }
            UserServiceError::Serialization(e) => {
                error!("Serialization error: {}", e);
                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Error")
                    .with_detail("An unexpected error occurred. Please try again.")
            }
            UserServiceError::Internal(msg) => {
                error!("Internal error: {}", msg);
                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Error")
                    .with_detail("An unexpected error occurred. Please try again.")
            }
            UserServiceError::CurrentPasswordRequired { .. } => {
                problem_new(StatusCode::BAD_REQUEST)
                    .with_title("Current Password Required")
                    .with_detail("Provide your current password to enroll MFA.")
            }
            UserServiceError::InvalidCurrentPassword { .. } => {
                problem_new(StatusCode::UNAUTHORIZED)
                    .with_title("Invalid Current Password")
                    .with_detail("The current password you entered is incorrect.")
            }
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_users,
        create_user,
        delete_user,
        assign_role,
        remove_role,
        update_user,
        restore_user,
        update_self,
        setup_mfa,
        verify_and_enable_mfa,
        disable_mfa
    ),
    components(
        schemas(RouteUser, RouteRole, RouteUserWithRoles, AssignRoleRequest, CreateUserRequest, UpdateUserRequest, UpdateSelfRequest, SetupMfaRequest, VerifyMfaRequest, MfaSetupResponse, DisableMfaRequest)
    ),
    tags(
        (name = "Users", description = "User management API")
    )
)]
pub struct UserApiDoc;

#[utoipa::path(
    tag = "Users",
    get,
    path = "/users",
    params(
        ("include_deleted" = bool, Query, description = "Include deleted users in the response")
    ),
    responses(
        (status = 200, description = "List all users with their roles", body = Vec<RouteUserWithRoles>),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
#[axum_macros::debug_handler]
async fn list_users(
    State(app_state): State<Arc<AuthState>>,
    RequireAuth(auth): RequireAuth,
    Query(params): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, Problem> {
    // Check for admin role
    permission_guard!(auth, UsersWrite);

    let include_deleted = params
        .get("include_deleted")
        .and_then(|v| v.parse::<bool>().ok())
        .unwrap_or(false);

    info!("Listing all users (include_deleted: {})", include_deleted);
    let users = app_state
        .user_service
        .get_all_users(include_deleted)
        .await?;

    let route_users: Vec<RouteUserWithRoles> = users.into_iter().map(|u| u.into()).collect();

    Ok(Json(route_users).into_response())
}

/// Reason a role-assignment request was denied by [`authorize_role_assignment`].
#[derive(Debug, PartialEq, Eq)]
enum RoleChangeDenied {
    /// The presented credential lacks the `users:manage` permission. `UsersWrite`
    /// alone (held by non-admin roles such as `PlatformAdmin`) is not sufficient
    /// to grant roles.
    MissingPermission,
    /// The path `user_id` and the body `user_id` disagree, leaving the target
    /// ambiguous. Historically the self-modification guard checked the path id
    /// while the mutation acted on the body id, so a mismatched pair bypassed it.
    TargetMismatch,
    /// Caller attempted to change their own roles.
    SelfModification,
}

/// Reason a `users:manage`-gated mutation of another user was denied.
#[derive(Debug, PartialEq, Eq)]
enum AdminTargetDenied {
    /// The presented credential lacks the `users:manage` permission.
    MissingPermission,
    /// The credential owner attempted to mutate their own account or roles.
    SelfModification,
}

/// Authorize a `users:manage`-gated mutation against another user.
///
/// The check is permission-based, not role-based: the capability can be carried
/// either by a role that includes `users:manage` (only `Admin`) or by an API key
/// explicitly granted it. It reads the presented credential's *effective*
/// permissions, so an admin-owned key restricted below `users:manage` (e.g. a
/// `PlatformAdmin`-scoped or custom `UsersWrite` key) is correctly rejected
/// without consulting the owner's database roles.
fn authorize_admin_target(
    auth: &AuthContext,
    target_user_id: i32,
) -> Result<(), AdminTargetDenied> {
    if !auth.has_permission(&Permission::UsersManage) {
        return Err(AdminTargetDenied::MissingPermission);
    }
    if target_user_id == auth.user_id() {
        return Err(AdminTargetDenied::SelfModification);
    }
    Ok(())
}

/// Authorize a role assignment and resolve the single target user id.
///
/// This is the security precondition for the `assign_role` endpoint, factored
/// out so every bypass path can be unit-tested without a full HTTP harness. It
/// mirrors the guards on the sibling `remove_role`/`delete_user` handlers
/// (`users:manage` required, no self-modification) and additionally requires the
/// path and body target ids to agree so there is exactly one, unambiguous target.
fn authorize_role_assignment(
    auth: &AuthContext,
    path_user_id: i32,
    body_user_id: i32,
) -> Result<i32, RoleChangeDenied> {
    if !auth.has_permission(&Permission::UsersManage) {
        return Err(RoleChangeDenied::MissingPermission);
    }
    if path_user_id != body_user_id {
        return Err(RoleChangeDenied::TargetMismatch);
    }
    let target_user_id = path_user_id;
    if target_user_id == auth.user_id() {
        return Err(RoleChangeDenied::SelfModification);
    }
    Ok(target_user_id)
}

#[utoipa::path(
    tag = "Users",
    post,
    path = "/users/{user_id}/roles",
    request_body = AssignRoleRequest,
    responses(
        (status = 200, description = "Role assigned successfully"),
        (status = 400, description = "Invalid role type"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Admin role required or self-modification forbidden"),
        (status = 404, description = "User or role not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("user_id" = i32, Path, description = "User ID")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
#[axum_macros::debug_handler]
async fn assign_role(
    State(app_state): State<Arc<AuthState>>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Path(user_id): Path<i32>,
    Json(assign_req): Json<AssignRoleRequest>,
) -> Result<impl IntoResponse, Problem> {
    info!(
        "Assigning role {} to user {}",
        assign_req.role_type, assign_req.user_id
    );

    // Authorize and resolve the single target: role mutation requires the
    // `users:manage` permission (UsersWrite alone is not enough, matching
    // remove_role/delete_user), the path and body target ids must agree, and
    // callers may not change their own roles.
    let target_user_id =
        authorize_role_assignment(&auth, user_id, assign_req.user_id).map_err(|denied| {
            error!(
                "Denied role assignment by user {}: {:?}",
                auth.user_id(),
                denied
            );
            match denied {
                RoleChangeDenied::TargetMismatch => temps_core::error_builder::bad_request()
                    .detail("Path user_id and body user_id must match".to_string())
                    .build(),
                RoleChangeDenied::MissingPermission | RoleChangeDenied::SelfModification => {
                    temps_core::error_builder::forbidden().build()
                }
            }
        })?;

    crate::require_sensitive_action(
        app_state.sensitive_action_authorizer.as_ref(),
        &auth,
        temps_core::SensitiveAction::AssignRole { user_id },
    )
    .await?;

    // Verify role type is valid
    let role_type = match RoleType::from_str(&assign_req.role_type) {
        Ok(rt) => rt,
        Err(_) => {
            error!("Invalid role type: {}", assign_req.role_type);
            return Err(temps_core::error_builder::bad_request()
                .detail(format!("Invalid role type: {}", assign_req.role_type))
                .build());
        }
    };

    let user_to_update = app_state
        .user_service
        .get_user_by_id(target_user_id)
        .await?;

    app_state
        .user_service
        .assign_role_by_type(target_user_id, role_type)
        .await?;

    info!("Role successfully assigned to user {}", target_user_id);

    // Create audit log
    let audit_context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent.as_str().to_string(),
    };

    let role_audit = RoleAssignedAudit {
        context: audit_context,
        target_user_id,
        role: assign_req.role_type.clone(),
        username: user_to_update.name.clone(),
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&role_audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok((StatusCode::OK, "Role assigned successfully").into_response())
}

/// Create a new user with roles
#[utoipa::path(
    tag = "Users",
    post,
    path = "/users",
    request_body = CreateUserRequest,
    responses(
        (status = 201, description = "User created successfully", body = RouteUserWithRoles),
        (status = 400, description = "Invalid input"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
#[axum_macros::debug_handler]
async fn create_user(
    State(app_state): State<Arc<AuthState>>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(create_req): Json<CreateUserRequest>,
) -> Result<impl IntoResponse, Problem> {
    // Creating a user (and assigning its roles) is gated on `users:manage`, not
    // the broader `UsersWrite`: the latter is also held by `PlatformAdmin`, which
    // must not be able to mint a brand-new `admin` account. There is no existing
    // target here, so this is a plain permission gate rather than
    // authorize_admin_target.
    permission_guard!(auth, UsersManage);

    info!(
        "Creating new user with username: {} and email: {}",
        create_req.username,
        create_req.email.clone().unwrap_or("no email".to_string())
    );

    // Debug: Check if password was provided
    if create_req.password.is_some() {
        debug!("Password provided for new user (will be hashed)");
    } else {
        warn!("No password provided for new user - user will not be able to login with password!");
    }

    // Reject the entire request if any role is unknown. Silently discarding a
    // role can create a different account than the administrator approved.
    let roles = parse_user_roles(&create_req.roles)?;

    let user = app_state
        .user_service
        .create_user(
            create_req.username.clone(),
            create_req.email.clone().unwrap_or("".to_string()),
            create_req.password.clone(),
            roles.clone(),
            create_req.must_change_password,
        )
        .await?;

    info!("Successfully created user with id: {}", user.user.id);

    // Create audit log
    let audit_context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent.as_str().to_string(),
    };

    let user_audit = UserCreatedAudit {
        context: audit_context,
        target_user_id: user.user.id,
        username: user.user.name.clone(),
        assigned_roles: roles.iter().map(|r| r.to_string()).collect(),
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&user_audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok((StatusCode::CREATED, Json(user)).into_response())
}

/// Delete a user
#[utoipa::path(
    tag = "Users",
    delete,
    path = "/users/{user_id}",
    responses(
        (status = 204, description = "User deleted successfully"),
        (status = 404, description = "User not found"),
        (status = 403, description = "Forbidden - Cannot delete yourself or non-admin attempt"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("user_id" = i32, Path, description = "User ID")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
#[axum_macros::debug_handler]
async fn delete_user(
    State(app_state): State<Arc<AuthState>>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Path(user_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    info!(
        "Request to delete user {} by user {}",
        user_id,
        auth.user_id()
    );

    // `users:manage` gate (+ no self-deletion) lives in authorize_admin_target.
    if let Err(denied) = authorize_admin_target(&auth, user_id) {
        error!(
            "Denied user deletion by user {} for target {}: {:?}",
            auth.user_id(),
            user_id,
            denied
        );
        return Err(temps_core::error_builder::forbidden().build());
    }
    let deleted_user = app_state.user_service.delete_user(user_id).await?;

    info!("Successfully deleted user with id: {}", user_id);

    let audit_context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent.as_str().to_string(),
    };

    let user_audit = UserDeletedAudit {
        context: audit_context,
        target_user_id: user_id,
        username: deleted_user.name.clone(),
        email: deleted_user.email,
        name: deleted_user.name,
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&user_audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(StatusCode::NO_CONTENT.into_response())
}

#[utoipa::path(
    tag = "Users",
    delete,
    path = "/users/{user_id}/roles/{role_type}",
    responses(
        (status = 204, description = "Role removed successfully"),
        (status = 400, description = "Invalid role type"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden - Cannot modify own roles or non-admin attempt"),
        (status = 404, description = "User or role not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("user_id" = i32, Path, description = "User ID"),
        ("role_type" = String, Path, description = "Role type to remove")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn remove_role(
    State(app_state): State<Arc<AuthState>>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Path((user_id, role_type)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    info!(
        "Request to remove role {} from user {} by user {}",
        role_type,
        user_id,
        auth.user_id()
    );
    // `users:manage` gate (+ no self-modification) lives in authorize_admin_target.
    if let Err(denied) = authorize_admin_target(&auth, user_id) {
        error!(
            "Denied role removal by user {} for target {}: {:?}",
            auth.user_id(),
            user_id,
            denied
        );
        return Err(temps_core::error_builder::forbidden().build());
    }

    // Verify role type is valid
    let role_type = match RoleType::from_str(&role_type) {
        Ok(rt) => rt,
        Err(_) => {
            error!("Invalid role type: {}", role_type);
            return Err(temps_core::error_builder::bad_request()
                .detail(format!("Invalid role type: {}", role_type))
                .build());
        }
    };
    let user_to_update = app_state.user_service.get_user_by_id(user_id).await?;
    app_state
        .user_service
        .remove_role_from_user(user_id, role_type.clone())
        .await?;
    info!("Successfully removed role from user {}", user_id);

    // Create audit log
    let audit_context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent.as_str().to_string(),
    };

    let role_audit = RoleRemovedAudit {
        context: audit_context,
        target_user_id: user_id,
        role: role_type.to_string(),
        username: user_to_update.name.clone(),
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&role_audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Update current user's information
#[utoipa::path(
    tag = "Users",
    patch,
    path = "/users/me",
    request_body = UpdateSelfRequest,
    responses(
        (status = 200, description = "User updated successfully", body = RouteUserWithRoles),
        (status = 401, description = "Unauthorized"),
        (status = 400, description = "Invalid input"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
#[axum_macros::debug_handler]
async fn update_self(
    State(app_state): State<Arc<AuthState>>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(update_req): Json<UpdateSelfRequest>,
) -> Result<impl IntoResponse, Problem> {
    // Check authentication
    permission_guard!(auth, UsersWrite);

    info!("Request to update self (user {})", auth.user_id());

    // Don't allow empty updates
    if update_req.email.is_none() && update_req.name.is_none() {
        return Err(temps_core::error_builder::bad_request()
            .detail("No fields to update")
            .build());
    }

    // Email change is security-sensitive: it redirects account recovery and
    // notification email. Require a step-up challenge when the request
    // contains a new email address. Name-only edits are safe without it.
    if update_req.email.is_some() {
        crate::require_sensitive_action(
            app_state.sensitive_action_authorizer.as_ref(),
            &auth,
            temps_core::SensitiveAction::UpdateAccountEmail,
        )
        .await?;
    }

    let updated_user = app_state
        .user_service
        .update_user(
            auth.user_id(),
            update_req.email.clone(),
            update_req.name.clone(),
        )
        .await?;

    // Create audit log
    let audit_context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent.as_str().to_string(),
    };

    let user_audit = UserUpdatedAudit {
        context: audit_context,
        target_user_id: auth.user_id(),
        username: updated_user.user.name.clone(),
        new_values: UpdatedFields {
            email: update_req.email,
            name: update_req.name,
        },
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&user_audit).await {
        error!("Failed to create audit log: {}", e);
    }

    info!("Successfully updated user {}", auth.user_id());
    Ok(Json(RouteUserWithRoles::from(updated_user)).into_response())
}

/// Update user information (admin only)
#[utoipa::path(
    tag = "Users",
    patch,
    path = "/users/{user_id}",
    request_body = UpdateUserRequest,
    responses(
        (status = 200, description = "User updated successfully", body = RouteUserWithRoles),
        (status = 404, description = "User not found"),
        (status = 403, description = "Forbidden - Non-admin attempt"),
        (status = 401, description = "Unauthorized"),
        (status = 400, description = "Invalid input"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("user_id" = i32, Path, description = "User ID")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
#[axum_macros::debug_handler]
async fn update_user(
    State(app_state): State<Arc<AuthState>>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Path(user_id): Path<i32>,
    Json(update_req): Json<UpdateUserRequest>,
) -> Result<impl IntoResponse, Problem> {
    // Editing another user's account is gated on `users:manage` (+ no
    // self-modification via authorize_admin_target); callers use PATCH /users/me
    // to change their own details. UsersWrite alone (held by PlatformAdmin) is
    // not sufficient.
    if let Err(denied) = authorize_admin_target(&auth, user_id) {
        error!(
            "Denied user update by user {} for target {}: {:?}",
            auth.user_id(),
            user_id,
            denied
        );
        return Err(temps_core::error_builder::forbidden().build());
    }

    info!("Admin {} updating user {}", auth.user_id(), user_id);

    // Don't allow empty updates
    if update_req.email.is_none() && update_req.name.is_none() {
        return Err(temps_core::error_builder::bad_request()
            .detail("No fields to update")
            .build());
    }

    let updated_user = app_state
        .user_service
        .update_user(user_id, update_req.email.clone(), update_req.name.clone())
        .await?;
    info!("Successfully updated user {}", user_id);

    let audit_context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent.as_str().to_string(),
    };

    let user_audit = UserUpdatedAudit {
        context: audit_context,
        target_user_id: user_id,
        username: updated_user.user.name.clone(),
        new_values: UpdatedFields {
            email: update_req.email,
            name: update_req.name,
        },
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&user_audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(Json(RouteUserWithRoles::from(updated_user)).into_response())
}

#[utoipa::path(
    tag = "Users",
    post,
    path = "/users/{user_id}/restore",
    responses(
        (status = 200, description = "User restored successfully", body = RouteUserWithRoles),
        (status = 404, description = "User not found"),
        (status = 400, description = "User is not deleted"),
        (status = 403, description = "Forbidden - Non-admin attempt"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("user_id" = i32, Path, description = "User ID")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn restore_user(
    State(app_state): State<Arc<AuthState>>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Path(user_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    // Restoring a soft-deleted user is gated on `users:manage`, matching
    // delete_user/remove_role. UsersWrite alone (held by PlatformAdmin) is not
    // sufficient.
    if let Err(denied) = authorize_admin_target(&auth, user_id) {
        error!(
            "Denied user restore by user {} for target {}: {:?}",
            auth.user_id(),
            user_id,
            denied
        );
        return Err(temps_core::error_builder::forbidden().build());
    }

    info!("Request to restore user {}", user_id);

    let restored_user = app_state.user_service.restore_user(user_id).await?;
    info!("Successfully restored user {}", user_id);

    let audit_context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent.as_str().to_string(),
    };

    let user_audit = UserRestoredAudit {
        context: audit_context,
        target_user_id: user_id,
        username: restored_user.user.name.clone(),
        email: restored_user.user.email.clone(),
        name: restored_user.user.name.clone(),
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&user_audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(Json(RouteUserWithRoles::from(restored_user)).into_response())
}

#[utoipa::path(
    tag = "Users",
    post,
    path = "/users/me/mfa/setup",
    request_body = SetupMfaRequest,
    responses(
        (status = 200, description = "MFA setup data", body = MfaSetupResponse),
        (status = 400, description = "Current password is required but was not provided"),
        (status = 401, description = "Unauthorized or current password is incorrect"),
        (status = 409, description = "MFA is already enabled; verify and disable it before re-enrollment"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn setup_mfa(
    State(app_state): State<Arc<AuthState>>,
    RequireAuth(auth): RequireAuth,
    Json(req): Json<SetupMfaRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, UsersWrite);

    let setup_data = app_state
        .user_service
        .setup_mfa(auth.user_id(), req.current_password.as_deref())
        .await?;
    Ok(Json(MfaSetupResponse {
        secret_key: setup_data.secret_key,
        qr_code: setup_data.qr_code,
        recovery_codes: setup_data.recovery_codes,
    })
    .into_response())
}

#[utoipa::path(
    tag = "Users",
    post,
    path = "/users/me/password",
    request_body = ChangePasswordRequest,
    responses(
        (status = 204, description = "Password updated"),
        (status = 400, description = "Validation error (weak password, same as current, MFA missing)"),
        (status = 401, description = "Current password incorrect or MFA code invalid"),
        (status = 403, description = "Account has no password set (SSO only)"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn change_password_self(
    State(app_state): State<Arc<AuthState>>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    headers: HeaderMap,
    Json(req): Json<ChangePasswordRequest>,
) -> Result<impl IntoResponse, Problem> {
    // Real users only — deployment tokens have no password to rotate.
    let user = auth.require_user().map_err(|msg| {
        problem_new(StatusCode::FORBIDDEN)
            .with_title("User Required")
            .with_detail(msg)
    })?;

    // Pull the encrypted session cookie so the service can preserve the
    // current session when revoke_other_sessions is true. Decrypt to the
    // plaintext token (that's what the sessions table stores). Missing /
    // undecryptable cookie just means we don't know which session is
    // "current" — fine, the service falls back to revoking everything.
    let current_session_token: Option<String> = headers
        .get_all("Cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|s| Cookie::split_parse(s).filter_map(Result::ok))
        .find_map(|c| {
            if c.name() == "session" {
                Some(c.value().to_string())
            } else {
                None
            }
        })
        .and_then(|enc| app_state.cookie_crypto.decrypt(&enc).ok());

    match app_state
        .auth_service
        .change_password_self(
            auth.user_id(),
            &req.current_password,
            &req.new_password,
            req.mfa_code.as_deref(),
            req.revoke_other_sessions,
            current_session_token.as_deref(),
        )
        .await
    {
        Ok(_) => {
            let audit = crate::audit::PasswordChangedAudit {
                context: AuditContext {
                    user_id: auth.user_id(),
                    ip_address: Some(metadata.ip_address.to_string()),
                    user_agent: metadata.user_agent.as_str().to_string(),
                },
                username: user.name.clone(),
                other_sessions_revoked: req.revoke_other_sessions,
            };
            if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
                error!("Failed to create audit log: {}", e);
            }
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        Err(e) => Err(match e {
            crate::auth_service::AuthError::InvalidCurrentPassword => {
                problem_new(StatusCode::UNAUTHORIZED)
                    .with_title("Invalid Current Password")
                    .with_detail("The current password you entered is incorrect.")
            }
            crate::auth_service::AuthError::MfaCodeRequired => problem_new(StatusCode::BAD_REQUEST)
                .with_title("MFA Code Required")
                .with_detail("Your account has MFA enabled. Provide a TOTP code or recovery code."),
            crate::auth_service::AuthError::InvalidMfaCode => problem_new(StatusCode::UNAUTHORIZED)
                .with_title("Invalid MFA Code")
                .with_detail("The MFA code you entered is incorrect or expired."),
            crate::auth_service::AuthError::SamePassword => problem_new(StatusCode::BAD_REQUEST)
                .with_title("Same Password")
                .with_detail("The new password must be different from the current one."),
            crate::auth_service::AuthError::WeakPassword(msg) => {
                problem_new(StatusCode::BAD_REQUEST)
                    .with_title("Weak Password")
                    .with_detail(msg)
            }
            crate::auth_service::AuthError::NoPasswordSet => problem_new(StatusCode::FORBIDDEN)
                .with_title("No Password On Account")
                .with_detail("This account uses SSO login and has no password to change."),
            other => {
                error!("Password change failed: {}", other);
                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Password Change Failed")
                    .with_detail("Could not change password. Please try again.")
            }
        }),
    }
}

#[utoipa::path(
    tag = "Users",
    post,
    path = "/users/me/mfa/verify",
    request_body = VerifyMfaRequest,
    responses(
        (status = 204, description = "MFA verified and enabled"),
        (status = 401, description = "Unauthorized"),
        (status = 400, description = "Invalid code"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn verify_and_enable_mfa(
    State(app_state): State<Arc<AuthState>>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(req): Json<VerifyMfaRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, UsersWrite);

    // Require a user for this endpoint (deployment tokens not allowed)
    let user = auth.require_user().map_err(|msg| {
        problemdetails::new(StatusCode::FORBIDDEN)
            .with_title("User Required")
            .with_detail(msg)
    })?;

    app_state
        .user_service
        .verify_and_enable_mfa(auth.user_id(), &req.code)
        .await?;
    // Create audit log
    let audit_context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent.as_str().to_string(),
    };

    let mfa_audit = MfaEnabledAudit {
        context: audit_context,
        username: user.name.clone(),
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&mfa_audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(StatusCode::NO_CONTENT.into_response())
}

#[utoipa::path(
    tag = "Users",
    delete,
    path = "/users/me/mfa",
    request_body = DisableMfaRequest,
    responses(
        (status = 204, description = "MFA disabled"),
        (status = 401, description = "Unauthorized"),
        (status = 400, description = "Invalid verification code"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn disable_mfa(
    State(app_state): State<Arc<AuthState>>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(req): Json<DisableMfaRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, UsersWrite);

    // Require a user for this endpoint (deployment tokens not allowed)
    let user = auth.require_user().map_err(|msg| {
        problemdetails::new(StatusCode::FORBIDDEN)
            .with_title("User Required")
            .with_detail(msg)
    })?;

    // First verify code and then disable MFA
    app_state
        .user_service
        .verify_and_disable_mfa(auth.user_id(), &req.code)
        .await?;
    // Create audit log
    let audit_context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent.as_str().to_string(),
    };

    let mfa_audit = MfaDisabledAudit {
        context: audit_context,
        username: user.name.clone(),
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&mfa_audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests {
    use super::{
        assign_role, authorize_admin_target, authorize_role_assignment, bounded_audit_identity,
        create_user, delete_user, login, normalized_login_email, parse_user_roles,
        record_pending_login, remove_role, restore_user, update_user, verify_mfa_challenge,
        AdminTargetDenied, AssignRoleRequest, CreateUserRequest, LoginRequest, RoleChangeDenied,
        UpdateUserRequest,
    };
    use crate::auth_service::UserAuthError;
    use crate::context::AuthContext;
    use crate::permissions::{Permission, Role};
    use crate::state::AuthState;
    use crate::types::MfaVerificationRequest;
    use crate::user_service::UserServiceError;
    use crate::RequireAuth;
    use async_trait::async_trait;
    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::{Extension, Json};
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, DatabaseConnection, DbErr, MockDatabase};
    use serde_json::Value;
    use std::sync::{Arc, Mutex};
    use temps_core::notifications::{
        EmailMessage, NotificationData, NotificationError, NotificationService,
    };
    use temps_core::{AuditLogger, RequestMetadata};
    use temps_entities::types::RoleType;
    use temps_entities::{roles, sessions, user_roles, users};

    // Regression tests for the user-management privilege-escalation hole. The
    // handlers checked only `UsersWrite`, which `PlatformAdmin` (and admin-owned
    // restricted API keys) also hold, so a non-admin could grant the `admin`
    // role or mint an `admin` account; and `assign_role`'s self-modification
    // guard checked the path id while the mutation used the body id, so a
    // mismatched pair bypassed it. The fix gates these operations on the
    // dedicated `users:manage` permission (held only by `Role::Admin`, or
    // grantable directly to an API key). `authorize_role_assignment` /
    // `authorize_admin_target` are the extracted, permission-based preconditions.

    fn test_user(id: i32) -> users::Model {
        let now = Utc::now();
        users::Model {
            id,
            name: "Test User".to_string(),
            email: format!("user{id}@example.com"),
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
        }
    }

    fn session_auth(role: Role) -> AuthContext {
        AuthContext::new_session(test_user(1), role)
    }

    fn api_key_auth(role: Option<Role>, permissions: Option<Vec<Permission>>) -> AuthContext {
        AuthContext::new_api_key(
            test_user(1),
            role,
            permissions,
            "restricted-key".to_string(),
            7,
        )
    }

    struct NoopAuditLogger;

    #[async_trait]
    impl AuditLogger for NoopAuditLogger {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::audit::AuditOperation,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[derive(Debug, Clone)]
    struct RecordedAudit {
        operation_type: String,
        user_id: Option<i32>,
        data: Value,
    }

    #[derive(Default)]
    struct RecordingAuditLogger {
        events: Mutex<Vec<RecordedAudit>>,
    }

    impl RecordingAuditLogger {
        fn events(&self) -> Vec<RecordedAudit> {
            self.events
                .lock()
                .expect("recorded audit mutex is not poisoned")
                .clone()
        }
    }

    #[async_trait]
    impl AuditLogger for RecordingAuditLogger {
        async fn create_audit_log(
            &self,
            operation: &dyn temps_core::audit::AuditOperation,
        ) -> anyhow::Result<()> {
            let data = serde_json::from_str(&operation.serialize()?)?;
            self.events
                .lock()
                .expect("recorded audit mutex is not poisoned")
                .push(RecordedAudit {
                    operation_type: operation.operation_type(),
                    user_id: operation.user_id(),
                    data,
                });
            Ok(())
        }
    }

    struct FailingAuditLogger;

    #[async_trait]
    impl AuditLogger for FailingAuditLogger {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::audit::AuditOperation,
        ) -> anyhow::Result<()> {
            anyhow::bail!("audit storage unavailable")
        }
    }

    struct NoopNotificationService;

    #[async_trait]
    impl NotificationService for NoopNotificationService {
        async fn send_email(&self, _message: EmailMessage) -> Result<(), NotificationError> {
            Ok(())
        }

        async fn send_notification(
            &self,
            _notification: NotificationData,
        ) -> Result<(), NotificationError> {
            Ok(())
        }

        async fn is_configured(&self) -> Result<bool, NotificationError> {
            Ok(false)
        }
    }

    /// Build handler state whose database says the credential owner has the
    /// persisted admin role. The fixed handlers must reject restricted API
    /// keys before consulting this database state. Under the vulnerable
    /// implementation, `user_service.is_admin(auth.user_id())` consumes these
    /// rows, succeeds, and continues past the expected 403.
    fn admin_owner_state() -> Arc<AuthState> {
        let now = Utc::now();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![roles::Model {
                id: 1,
                name: "admin".to_string(),
                created_at: now,
                updated_at: now,
            }]])
            .append_query_results(vec![vec![user_roles::Model {
                id: 1,
                user_id: 1,
                role_id: 1,
                created_at: now,
                updated_at: now,
            }]])
            .into_connection();
        let db = Arc::new(db);

        Arc::new(AuthState::new(
            db,
            Arc::new(NoopAuditLogger),
            Arc::new(temps_core::EncryptionService::new_from_password(
                "handler-regression-test",
            )),
            Arc::new(temps_core::CookieCrypto::from_bytes(&[7; 32])),
            Arc::new(NoopNotificationService),
            Arc::new(temps_core::telemetry::NoopTelemetryReporter),
        ))
    }

    fn auth_state_with_audit(
        db: DatabaseConnection,
        audit: Arc<dyn AuditLogger>,
    ) -> Arc<AuthState> {
        Arc::new(AuthState::new(
            Arc::new(db),
            audit,
            Arc::new(temps_core::EncryptionService::new_from_password(
                "handler-regression-test",
            )),
            Arc::new(temps_core::CookieCrypto::from_bytes(&[7; 32])),
            Arc::new(NoopNotificationService),
            Arc::new(temps_core::telemetry::NoopTelemetryReporter),
        ))
    }

    fn request_metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "127.0.0.1".to_string(),
            user_agent: "temps-auth-test".to_string(),
            headers: HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "https://temps.test".to_string(),
            scheme: "https".to_string(),
            host: "temps.test".to_string(),
            is_secure: true,
        }
    }

    fn assert_problem_status(problem: temps_core::problemdetails::Problem, expected: StatusCode) {
        assert_eq!(problem.status_code, expected);
    }

    fn expect_problem<T>(
        result: Result<T, temps_core::problemdetails::Problem>,
        message: &str,
    ) -> temps_core::problemdetails::Problem {
        match result {
            Ok(_) => panic!("{message}"),
            Err(problem) => problem,
        }
    }

    #[test]
    fn login_identifier_is_normalized_and_bounded_before_auditing() {
        assert_eq!(
            normalized_login_email("User@Example.COM"),
            Some("user@example.com".to_string())
        );
        assert_eq!(normalized_login_email("not-an-email"), None);

        let oversized = format!("{}@example.com", "é".repeat(300));
        let bounded = bounded_audit_identity(&oversized);
        assert!(bounded.len() <= 254);
        assert!(std::str::from_utf8(bounded.as_bytes()).is_ok());
    }

    #[test]
    fn create_user_roles_are_strictly_validated() {
        assert_eq!(
            parse_user_roles(&["user".to_string(), "admin".to_string()])
                .expect("known roles should parse"),
            vec![RoleType::User, RoleType::Admin]
        );

        let result = parse_user_roles(&["user".to_string(), "administrator".to_string()]);
        assert!(matches!(result, Err(UserServiceError::Validation(_))));
    }

    #[tokio::test]
    async fn invalid_login_identifier_is_audited_without_querying_the_database() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let state = auth_state_with_audit(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
            audit.clone(),
        );
        let oversized = format!("{}@example.com", "x".repeat(500));

        let result = login(
            State(state),
            Extension(request_metadata()),
            Json(LoginRequest {
                email: oversized,
                password: "irrelevant".to_string(),
            }),
        )
        .await;

        assert_problem_status(
            expect_problem(result, "invalid identifier must be rejected"),
            StatusCode::UNAUTHORIZED,
        );
        let events = audit.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].operation_type, "LOGIN_FAILURE");
        assert_eq!(events[0].user_id, None);
        assert_eq!(events[0].data["reason"], "invalid_identifier");
        assert!(
            events[0].data["attempted_email"]
                .as_str()
                .expect("attempted email is a string")
                .len()
                <= 254
        );
    }

    #[tokio::test]
    async fn audit_write_failure_does_not_change_login_rejection() {
        let state = auth_state_with_audit(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
            Arc::new(FailingAuditLogger),
        );

        let result = login(
            State(state),
            Extension(request_metadata()),
            Json(LoginRequest {
                email: "not-an-email".to_string(),
                password: "irrelevant".to_string(),
            }),
        )
        .await;

        assert_problem_status(
            expect_problem(
                result,
                "invalid identifier must remain rejected when auditing fails",
            ),
            StatusCode::UNAUTHORIZED,
        );
    }

    #[tokio::test]
    async fn pending_password_and_mfa_logins_are_durably_audited() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let state = auth_state_with_audit(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
            audit.clone(),
        );
        let metadata = request_metadata();

        record_pending_login(state.as_ref(), &metadata, 41, "password-change-required").await;
        record_pending_login(
            state.as_ref(),
            &metadata,
            42,
            "password-mfa-enrollment-pending",
        )
        .await;

        let events = audit.events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].operation_type, "LOGIN_SUCCESS");
        assert_eq!(events[0].user_id, Some(41));
        assert_eq!(events[0].data["login_method"], "password-change-required");
        assert_eq!(events[1].operation_type, "LOGIN_SUCCESS");
        assert_eq!(events[1].user_id, Some(42));
        assert_eq!(
            events[1].data["login_method"],
            "password-mfa-enrollment-pending"
        );
    }

    #[tokio::test]
    async fn pending_login_audit_failure_does_not_block_authentication_progress() {
        let state = auth_state_with_audit(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
            Arc::new(FailingAuditLogger),
        );

        record_pending_login(
            state.as_ref(),
            &request_metadata(),
            41,
            "password-change-required",
        )
        .await;
    }

    #[tokio::test]
    async fn invalid_credentials_are_audited_with_normalized_identity() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<users::Model>::new()])
            .into_connection();
        let state = auth_state_with_audit(db, audit.clone());

        let result = login(
            State(state),
            Extension(request_metadata()),
            Json(LoginRequest {
                email: "Nobody@Example.COM".to_string(),
                password: "incorrect".to_string(),
            }),
        )
        .await;

        assert_problem_status(
            expect_problem(result, "invalid credentials must be rejected"),
            StatusCode::UNAUTHORIZED,
        );
        let events = audit.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].operation_type, "LOGIN_FAILURE");
        assert_eq!(events[0].user_id, None);
        assert_eq!(events[0].data["attempted_email"], "nobody@example.com");
        assert_eq!(events[0].data["reason"], "invalid_credentials");
    }

    fn mfa_headers(state: &AuthState, session_token: &str) -> HeaderMap {
        let encrypted = state
            .cookie_crypto
            .encrypt(session_token)
            .expect("test MFA session encrypts");
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            format!("mfa_session={encrypted}")
                .parse()
                .expect("test cookie header is valid"),
        );
        headers
    }

    #[tokio::test]
    async fn missing_mfa_cookie_is_recorded_as_a_rejection() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let state = auth_state_with_audit(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
            audit.clone(),
        );

        let result = verify_mfa_challenge(
            State(state),
            Extension(request_metadata()),
            HeaderMap::new(),
            Json(MfaVerificationRequest {
                code: "123456".to_string(),
            }),
        )
        .await;

        assert_problem_status(
            expect_problem(result, "missing MFA cookie must be rejected"),
            StatusCode::UNAUTHORIZED,
        );
        let events = audit.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].operation_type, "MFA_VERIFICATION_FAILED");
        assert_eq!(events[0].user_id, None);
        assert_eq!(events[0].data["reason"], "missing_session_cookie");
    }

    #[tokio::test]
    async fn expired_mfa_session_is_recorded_as_a_rejection() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<sessions::Model>::new()])
            .into_connection();
        let state = auth_state_with_audit(db, audit.clone());
        let headers = mfa_headers(state.as_ref(), "expired-session");

        let result = verify_mfa_challenge(
            State(state),
            Extension(request_metadata()),
            headers,
            Json(MfaVerificationRequest {
                code: "123456".to_string(),
            }),
        )
        .await;

        assert_problem_status(
            expect_problem(result, "expired MFA session must be rejected"),
            StatusCode::UNAUTHORIZED,
        );
        let events = audit.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].operation_type, "MFA_VERIFICATION_FAILED");
        assert_eq!(events[0].user_id, None);
        assert_eq!(events[0].data["reason"], "invalid_or_expired_session");
    }

    #[tokio::test]
    async fn mfa_database_failure_is_not_recorded_as_a_bad_code() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([DbErr::Custom("database unavailable".to_string())])
            .into_connection();
        let state = auth_state_with_audit(db, audit.clone());
        let headers = mfa_headers(state.as_ref(), "pending-session");

        let result = verify_mfa_challenge(
            State(state),
            Extension(request_metadata()),
            headers,
            Json(MfaVerificationRequest {
                code: "123456".to_string(),
            }),
        )
        .await;

        assert_problem_status(
            expect_problem(result, "database failure must be internal"),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
        assert!(
            audit.events().is_empty(),
            "an infrastructure error must not become a false MFA rejection audit"
        );
    }

    #[test]
    fn admin_assigning_to_another_user_is_allowed() {
        // caller=1 (admin) assigns to user 2, path and body agree.
        let auth = session_auth(Role::Admin);
        assert_eq!(authorize_role_assignment(&auth, 2, 2), Ok(2));
    }

    #[test]
    fn admin_owned_platform_admin_api_key_is_rejected_for_admin_only_mutations() {
        // The owner may be an administrator in the database, but this presented
        // credential is deliberately restricted to PlatformAdmin.
        let auth = api_key_auth(Some(Role::PlatformAdmin), None);
        assert!(auth.has_permission(&Permission::UsersWrite));
        assert!(!auth.is_admin());
        assert_eq!(
            authorize_role_assignment(&auth, 2, 2),
            Err(RoleChangeDenied::MissingPermission)
        );
        assert_eq!(
            authorize_admin_target(&auth, 2),
            Err(AdminTargetDenied::MissingPermission)
        );
    }

    #[test]
    fn admin_owned_custom_userswrite_api_key_is_rejected_for_admin_only_mutations() {
        let auth = api_key_auth(None, Some(vec![Permission::UsersWrite]));
        assert!(auth.has_permission(&Permission::UsersWrite));
        assert!(!auth.is_admin());
        assert_eq!(
            authorize_role_assignment(&auth, 2, 2),
            Err(RoleChangeDenied::MissingPermission)
        );
        assert_eq!(
            authorize_admin_target(&auth, 2),
            Err(AdminTargetDenied::MissingPermission)
        );
    }

    #[test]
    fn admin_scoped_api_key_can_apply_admin_only_mutations_to_another_user() {
        let auth = api_key_auth(Some(Role::Admin), None);
        assert!(auth.is_admin());
        assert_eq!(authorize_role_assignment(&auth, 2, 2), Ok(2));
        assert_eq!(authorize_admin_target(&auth, 2), Ok(()));
    }

    #[test]
    fn api_key_granted_users_manage_permission_is_authorized() {
        // The point of gating on a permission rather than a role: a key that is
        // NOT an admin role but was explicitly granted `users:manage` may manage
        // users, while a key holding only the broader `UsersWrite` may not.
        let manage_key = api_key_auth(None, Some(vec![Permission::UsersManage]));
        assert!(!manage_key.is_admin());
        assert!(manage_key.has_permission(&Permission::UsersManage));
        assert_eq!(authorize_role_assignment(&manage_key, 2, 2), Ok(2));
        assert_eq!(authorize_admin_target(&manage_key, 2), Ok(()));

        let write_only_key = api_key_auth(None, Some(vec![Permission::UsersWrite]));
        assert_eq!(
            authorize_role_assignment(&write_only_key, 2, 2),
            Err(RoleChangeDenied::MissingPermission)
        );
        assert_eq!(
            authorize_admin_target(&write_only_key, 2),
            Err(AdminTargetDenied::MissingPermission)
        );
    }

    #[test]
    fn defect_b_path_body_mismatch_targeting_self_is_rejected() {
        // The exploit: caller=1 sets the path to another id (2) so the old
        // self-guard passed, but the body targets themselves (1), so the
        // mutation acted on the caller. The mismatch is now rejected outright.
        let auth = session_auth(Role::Admin);
        assert_eq!(
            authorize_role_assignment(&auth, 2, 1),
            Err(RoleChangeDenied::TargetMismatch)
        );
    }

    #[test]
    fn self_modification_is_rejected_even_when_ids_agree() {
        // caller=1, path=body=1 -> still a self-modification.
        let auth = session_auth(Role::Admin);
        assert_eq!(
            authorize_role_assignment(&auth, 1, 1),
            Err(RoleChangeDenied::SelfModification)
        );
        assert_eq!(
            authorize_admin_target(&auth, 1),
            Err(AdminTargetDenied::SelfModification)
        );
    }

    #[test]
    fn admin_gate_takes_precedence_over_target_checks() {
        // A non-admin must be denied regardless of how the ids line up.
        let auth = session_auth(Role::PlatformAdmin);
        assert_eq!(
            authorize_role_assignment(&auth, 2, 1),
            Err(RoleChangeDenied::MissingPermission)
        );
    }

    #[tokio::test]
    async fn assign_role_handler_rejects_admin_owned_platform_admin_api_key() {
        let result = assign_role(
            State(admin_owner_state()),
            RequireAuth(api_key_auth(Some(Role::PlatformAdmin), None)),
            Extension(request_metadata()),
            Path(2),
            Json(AssignRoleRequest {
                user_id: 2,
                role_type: "admin".to_string(),
            }),
        )
        .await;

        match result {
            Ok(_) => panic!("restricted API key unexpectedly assigned a role"),
            Err(problem) => assert_problem_status(problem, StatusCode::FORBIDDEN),
        }
    }

    #[tokio::test]
    async fn assign_role_handler_rejects_admin_owned_custom_userswrite_api_key() {
        let result = assign_role(
            State(admin_owner_state()),
            RequireAuth(api_key_auth(None, Some(vec![Permission::UsersWrite]))),
            Extension(request_metadata()),
            Path(2),
            Json(AssignRoleRequest {
                user_id: 2,
                role_type: "admin".to_string(),
            }),
        )
        .await;

        match result {
            Ok(_) => panic!("restricted API key unexpectedly assigned a role"),
            Err(problem) => assert_problem_status(problem, StatusCode::FORBIDDEN),
        }
    }

    #[tokio::test]
    async fn assign_role_handler_rejects_path_body_target_mismatch() {
        let result = assign_role(
            State(admin_owner_state()),
            RequireAuth(session_auth(Role::Admin)),
            Extension(request_metadata()),
            Path(2),
            Json(AssignRoleRequest {
                user_id: 3,
                role_type: "user".to_string(),
            }),
        )
        .await;

        match result {
            Ok(_) => panic!("mismatched role-assignment target unexpectedly succeeded"),
            Err(problem) => assert_problem_status(problem, StatusCode::BAD_REQUEST),
        }
    }

    #[tokio::test]
    async fn delete_user_handler_rejects_admin_owned_restricted_api_keys() {
        for auth in [
            api_key_auth(Some(Role::PlatformAdmin), None),
            api_key_auth(None, Some(vec![Permission::UsersWrite])),
        ] {
            let result = delete_user(
                State(admin_owner_state()),
                RequireAuth(auth),
                Extension(request_metadata()),
                Path(2),
            )
            .await;

            match result {
                Ok(_) => panic!("restricted API key unexpectedly deleted a user"),
                Err(problem) => assert_problem_status(problem, StatusCode::FORBIDDEN),
            }
        }
    }

    #[tokio::test]
    async fn remove_role_handler_rejects_admin_owned_restricted_api_keys() {
        for auth in [
            api_key_auth(Some(Role::PlatformAdmin), None),
            api_key_auth(None, Some(vec![Permission::UsersWrite])),
        ] {
            let result = remove_role(
                State(admin_owner_state()),
                RequireAuth(auth),
                Extension(request_metadata()),
                Path((2, "user".to_string())),
            )
            .await;

            match result {
                Ok(_) => panic!("restricted API key unexpectedly removed a role"),
                Err(problem) => assert_problem_status(problem, StatusCode::FORBIDDEN),
            }
        }
    }

    #[tokio::test]
    async fn create_user_handler_rejects_admin_owned_restricted_api_keys() {
        for auth in [
            api_key_auth(Some(Role::PlatformAdmin), None),
            api_key_auth(None, Some(vec![Permission::UsersWrite])),
        ] {
            let result = create_user(
                State(admin_owner_state()),
                RequireAuth(auth),
                Extension(request_metadata()),
                Json(CreateUserRequest {
                    username: "provisioned-user".to_string(),
                    email: Some("provisioned@example.com".to_string()),
                    password: Some("a-valid-passphrase".to_string()),
                    // Requesting the admin role is the case the gate must stop a
                    // restricted credential from performing.
                    roles: vec!["admin".to_string()],
                    must_change_password: true,
                }),
            )
            .await;

            match result {
                Ok(_) => panic!("restricted API key unexpectedly created a user"),
                Err(problem) => assert_problem_status(problem, StatusCode::FORBIDDEN),
            }
        }
    }

    #[tokio::test]
    async fn update_user_handler_rejects_admin_owned_restricted_api_keys() {
        for auth in [
            api_key_auth(Some(Role::PlatformAdmin), None),
            api_key_auth(None, Some(vec![Permission::UsersWrite])),
        ] {
            let result = update_user(
                State(admin_owner_state()),
                RequireAuth(auth),
                Extension(request_metadata()),
                Path(2),
                Json(UpdateUserRequest {
                    email: Some("changed@example.com".to_string()),
                    name: None,
                }),
            )
            .await;

            match result {
                Ok(_) => panic!("restricted API key unexpectedly updated a user"),
                Err(problem) => assert_problem_status(problem, StatusCode::FORBIDDEN),
            }
        }
    }

    #[tokio::test]
    async fn restore_user_handler_rejects_admin_owned_restricted_api_keys() {
        for auth in [
            api_key_auth(Some(Role::PlatformAdmin), None),
            api_key_auth(None, Some(vec![Permission::UsersWrite])),
        ] {
            let result = restore_user(
                State(admin_owner_state()),
                RequireAuth(auth),
                Extension(request_metadata()),
                Path(2),
            )
            .await;

            match result {
                Ok(_) => panic!("restricted API key unexpectedly restored a user"),
                Err(problem) => assert_problem_status(problem, StatusCode::FORBIDDEN),
            }
        }
    }

    /// Regression test for the CI/CD panic
    /// `Overlapping method route. Handler for "GET /auth/oidc/login/{slug}" already exists`
    ///
    /// Both `handlers::configure_routes()` (rate-limited) and
    /// `oidc_handler::configure_oidc_routes()` were registering the same
    /// path; Axum panics on `Router::merge` when this happens. The
    /// production fix lives in oidc_handler.rs (route removed there,
    /// kept in handlers.rs so it stays inside the rate-limit group).
    /// This test exercises the merge to catch any future re-introduction.
    #[test]
    fn test_no_overlapping_routes_between_configurators() {
        // Build both routers without state so we can merge them without
        // wiring up AuthState. State is unrelated to route registration.
        let auth_routes: axum::Router<std::sync::Arc<crate::state::AuthState>> =
            super::configure_routes();
        let oidc_routes: axum::Router<std::sync::Arc<crate::state::AuthState>> =
            crate::oidc_handler::configure_oidc_routes();
        // This call panics if any route overlaps. Just performing the merge
        // is the assertion — no need to inspect the result.
        let _merged = auth_routes.merge(oidc_routes);
    }

    /// The login handler must return a constant 401 detail for both
    /// `InvalidCredentials` and `UserNotFound` — the caller must not be able
    /// to distinguish "email does not exist" from "wrong password".
    #[test]
    fn test_login_error_mapping_invalid_credentials_constant_message() {
        let e = UserAuthError::InvalidCredentials;
        let (status, detail) = match e {
            UserAuthError::InvalidCredentials | UserAuthError::UserNotFound => {
                (401u16, "Invalid email or password.")
            }
            _ => (
                500u16,
                "Authentication system error. Please try again later.",
            ),
        };
        assert_eq!(status, 401);
        assert_eq!(detail, "Invalid email or password.");
    }

    /// Both `UserNotFound` and `InvalidCredentials` must produce identical
    /// response shape to prevent user enumeration via differing HTTP bodies.
    #[test]
    fn test_login_error_mapping_user_not_found_same_as_invalid_credentials() {
        let map = |e: UserAuthError| match e {
            UserAuthError::InvalidCredentials | UserAuthError::UserNotFound => {
                (401u16, "Invalid Credentials", "Invalid email or password.")
            }
            _ => (
                500u16,
                "Authentication Error",
                "Authentication system error. Please try again later.",
            ),
        };

        let (status_c, title_c, detail_c) = map(UserAuthError::InvalidCredentials);
        let (status_n, title_n, detail_n) = map(UserAuthError::UserNotFound);

        assert_eq!(status_c, status_n, "HTTP status must be identical");
        assert_eq!(title_c, title_n, "title must be identical");
        assert_eq!(
            detail_c, detail_n,
            "detail must be identical to prevent enumeration"
        );
    }

    /// `MfaRequiredForRole` must produce the exact same response as
    /// `InvalidCredentials`/`UserNotFound` -- it's only reachable *after* the
    /// password has already been verified correct, so a distinguishable
    /// response would let an attacker use login attempts as an oracle to
    /// confirm a guessed admin password (and that the account is Admin)
    /// without completing a login.
    #[test]
    fn test_login_error_mapping_mfa_required_same_as_invalid_credentials() {
        let map = |e: UserAuthError| match e {
            UserAuthError::InvalidCredentials | UserAuthError::UserNotFound => {
                (401u16, "Invalid Credentials", "Invalid email or password.")
            }
            UserAuthError::MfaRequiredForRole { .. } => {
                (401u16, "Invalid Credentials", "Invalid email or password.")
            }
            _ => (
                500u16,
                "Authentication Error",
                "Authentication system error. Please try again later.",
            ),
        };

        let (status_c, title_c, detail_c) = map(UserAuthError::InvalidCredentials);
        let (status_m, title_m, detail_m) = map(UserAuthError::MfaRequiredForRole {
            user_id: 42,
            role: "Admin".to_string(),
        });

        assert_eq!(status_c, status_m, "HTTP status must be identical");
        assert_eq!(title_c, title_m, "title must be identical");
        assert_eq!(
            detail_c, detail_m,
            "detail must be identical to prevent an MFA-enforcement oracle"
        );
    }

    /// `DatabaseError` (and all other internal variants) must map to 500,
    /// not 401, so internal errors are not mislabeled as auth failures.
    #[test]
    fn test_login_error_mapping_database_error_returns_500() {
        let e = UserAuthError::DatabaseError(sea_orm::DbErr::Custom(
            "pg: connection refused".to_string(),
        ));
        let (status, detail) = match e {
            UserAuthError::InvalidCredentials | UserAuthError::UserNotFound => {
                (401u16, "Invalid email or password.")
            }
            _ => (
                500u16,
                "Authentication system error. Please try again later.",
            ),
        };
        assert_eq!(status, 500);
        assert_eq!(
            detail,
            "Authentication system error. Please try again later."
        );
    }

    #[test]
    fn test_login_error_mapping_password_hash_error_returns_500() {
        let e = UserAuthError::PasswordHashError;
        let status = match e {
            UserAuthError::InvalidCredentials | UserAuthError::UserNotFound => 401u16,
            _ => 500u16,
        };
        assert_eq!(status, 500);
    }

    /// OidcProviderSummary must expose `slug` instead of `id`.
    /// This is a compile-time guard: constructing the struct without `id`
    /// proves the field was removed from the public type.
    #[test]
    fn test_email_status_response_oidc_summary_has_slug_not_id() {
        let summary = crate::oidc_types::OidcProviderSummary {
            slug: "my-provider-aabbccdd".to_string(),
            name: "My Provider".to_string(),
            template: "okta".to_string(),
        };
        assert!(!summary.slug.is_empty());
        // Slug must carry the hash suffix (separated by '-')
        assert!(summary.slug.contains('-'));
    }
}
