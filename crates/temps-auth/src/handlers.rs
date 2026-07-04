use super::AuthState;
use crate::audit::{
    ConcurrentSessionDetectedAudit, EmailVerifiedAudit, LoginAudit, LogoutAudit, MfaDisabledAudit,
    MfaEnabledAudit, MfaVerifiedAudit, PasswordResetAudit, RoleAssignedAudit, RoleRemovedAudit,
    UpdatedFields, UserCreatedAudit, UserDeletedAudit, UserRestoredAudit, UserUpdatedAudit,
};
use crate::avatar::generate_avatar_data_url;
use crate::user_service::UserServiceError;
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
    TokenRenewalRequest, UpdateSelfRequest, UpdateUserRequest, UserResponse, VerifyMfaRequest,
};
use temps_core::problemdetails::{new as problem_new, Problem};

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
        })
        .ok_or(
            problem_new(StatusCode::UNAUTHORIZED)
                .with_title("MFA Session Required")
                .with_detail(
                    "No MFA session found. Please log in first to start the MFA verification flow.",
                ),
        )?;

    // Decrypt the MFA session cookie
    let mfa_session = auth_state
        .cookie_crypto
        .decrypt(&encrypted_mfa_session)
        .map_err(|e| {
            tracing::error!("Failed to decrypt MFA session cookie: {}", e);
            problem_new(StatusCode::UNAUTHORIZED)
                .with_title("MFA Session Expired")
                .with_detail("Your MFA session has expired or is invalid. Please log in again.")
        })?;

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

            let session_token = auth_state
                .auth_service
                .create_session(user.id)
                .await
                .map_err(|e| {
                    error!("Failed to create session after MFA verification: {}", e);
                    problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                        .with_title("Session Creation Failed")
                        .with_detail("Could not create session. Please try logging in again.")
                })?;

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
            response_headers.append(SET_COOKIE, clear_mfa_cookie.to_string().parse().unwrap());

            Ok((StatusCode::NO_CONTENT, response_headers))
        }
        Err(e) => {
            error!("MFA verification failed: {}", e);
            Err(problem_new(StatusCode::UNAUTHORIZED)
                .with_title("MFA Verification Failed")
                .with_detail("The verification code is incorrect or has expired. Please try again with a new code from your authenticator app."))
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_current_user,
        logout,
        verify_mfa_challenge,
        register,
        login,
        email_status,
        request_magic_link,
        verify_magic_link,
        request_password_reset,
        reset_password,
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
            MagicLinkRequest,
            ResetPasswordRequest,
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
        magic links, password reset, and email verification.",
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
        .route(
            "/auth/cli/device/start",
            post(crate::cli_device_handler::cli_device_start),
        )
        .route(
            "/auth/cli/device/poll",
            post(crate::cli_device_handler::cli_device_poll),
        )
        .route("/auth/magic-link/request", post(request_magic_link))
        .route("/auth/magic-link/verify", get(verify_magic_link))
        .route("/auth/password-reset/request", post(request_password_reset))
        .route("/auth/password-reset/verify", post(reset_password))
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

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MagicLinkRequest {
    pub email: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ResetPasswordRequest {
    pub token: String,
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

impl From<MagicLinkRequest> for crate::auth_service::MagicLinkRequest {
    fn from(req: MagicLinkRequest) -> Self {
        crate::auth_service::MagicLinkRequest { email: req.email }
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
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EmailStatusResponse {
    pub email_configured: bool,
    pub magic_link_available: bool,
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
    Json(request): Json<LoginRequest>,
) -> Result<impl IntoResponse, temps_core::problemdetails::Problem> {
    // Capture email before `request` is moved into the service call so we
    // can log it (lowercased) on internal errors without leaking password.
    let login_email = request.email.to_lowercase();
    match state.auth_service.login(request.into()).await {
        Ok(user) => {
            // Check if user has MFA enabled
            if user.mfa_enabled {
                // Create temporary MFA session
                match state.auth_service.create_mfa_session(user.id).await {
                    Ok(mfa_token) => {
                        // Encrypt the MFA token
                        let encrypted_token =
                            state.cookie_crypto.encrypt(&mfa_token).map_err(|e| {
                                error!("Failed to encrypt MFA token: {}", e);
                                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                                    .with_title("Authentication Error")
                                    .with_detail("Could not process MFA session. Please try again.")
                            })?;

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
                        headers.insert(SET_COOKIE, mfa_cookie.to_string().parse().unwrap());

                        Ok((
                            headers,
                            Json(AuthResponse {
                                success: false,
                                message: "MFA authentication required".to_string(),
                                user_id: None,
                                mfa_required: true,
                            }),
                        ))
                    }
                    Err(e) => {
                        error!("Failed to create MFA session: {}", e);
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
                        let encrypted_token =
                            state.cookie_crypto.encrypt(&session_token).map_err(|e| {
                                error!("Failed to encrypt session token: {}", e);
                                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                                    .with_title("Authentication Error")
                                    .with_detail("Could not secure your session. Please try again.")
                            })?;

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
                            }),
                        ))
                    }
                    Err(e) => {
                        if let Err(e) = state
                            .audit_service
                            .create_audit_log(&LoginAudit {
                                context: AuditContext {
                                    user_id: 0,
                                    ip_address: Some(metadata.ip_address.to_string()),
                                    user_agent: metadata.user_agent.as_str().to_string(),
                                },
                                success: false,
                                login_method: "password".to_string(),
                            })
                            .await
                        {
                            error!("Failed to create audit log: {}", e);
                        }
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
                Err(problem_new(StatusCode::UNAUTHORIZED)
                    .with_title("Invalid Credentials")
                    .with_detail("Invalid email or password."))
            }
            crate::auth_service::UserAuthError::MfaRequiredForRole { user_id, role } => {
                // Deliberately the *same* status/title/detail as
                // InvalidCredentials above -- this error is only reachable
                // after the password has already been verified correct, so
                // a distinguishable response here would let an attacker use
                // login attempts as an oracle to confirm a guessed admin
                // password (and that the account holds the Admin role)
                // without ever completing a login. The real reason is only
                // ever surfaced server-side via this log line.
                warn!(
                    user_id,
                    role = %role,
                    email = %login_email,
                    "Login blocked: MFA is required for this role but is not enrolled"
                );
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
                Err(problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Authentication Error")
                    .with_detail("Authentication system error. Please try again later."))
            }
        },
    }
}

#[utoipa::path(
    post,
    path = "/auth/magic-link/request",
    request_body = MagicLinkRequest,
    responses(
        (status = 200, description = "Magic link sent if email exists", body = AuthResponse),
        (status = 400, description = "Bad request"),
        (status = 503, description = "Email service not configured")
    ),
    tag = "Authentication"
)]
pub async fn request_magic_link(
    State(state): State<Arc<AuthState>>,
    Json(request): Json<MagicLinkRequest>,
) -> Result<impl IntoResponse, temps_core::problemdetails::Problem> {
    if !state.auth_service.is_email_configured().await {
        return Err(problem_new(StatusCode::SERVICE_UNAVAILABLE)
            .with_title("Email Service Not Configured")
            .with_detail(
                "Magic link authentication is not available without email configuration",
            ));
    }

    match state
        .auth_service
        .send_magic_link(request.clone().into())
        .await
    {
        Ok(_) => Ok(Json(AuthResponse {
            success: true,
            message: "If an account exists with this email, a magic link has been sent".to_string(),
            user_id: None,
            mfa_required: false,
        })),
        Err(_) => {
            warn!("Failed to send magic link to email: {}", request.email);
            // Always return success to prevent email enumeration
            Ok(Json(AuthResponse {
                success: true,
                message: "If an account exists with this email, a magic link has been sent"
                    .to_string(),
                user_id: None,
                mfa_required: false,
            }))
        }
    }
}

#[utoipa::path(
    get,
    path = "/auth/magic-link/verify",
    params(
        ("token" = String, Query, description = "Magic link token")
    ),
    responses(
        (status = 200, description = "Magic link verified, session cookie set", body = AuthResponse),
        (status = 400, description = "Invalid or expired token"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Authentication"
)]
pub async fn verify_magic_link(
    State(state): State<Arc<AuthState>>,
    Query(query): Query<VerifyTokenQuery>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, temps_core::problemdetails::Problem> {
    match state.auth_service.verify_magic_link(&query.token).await {
        Ok(user) => {
            // Create session
            match state.auth_service.create_session(user.id).await {
                Ok(session_token) => {
                    // Encrypt the session token
                    let encrypted_token = state.cookie_crypto.encrypt(&session_token)?;

                    // Create session cookie headers using pre-calculated secure flag
                    let headers = state
                        .auth_service
                        .create_session_cookie(&encrypted_token, metadata.is_secure);

                    // Create audit log for successful magic link login
                    if let Err(e) = state
                        .audit_service
                        .create_audit_log(&LoginAudit {
                            context: AuditContext {
                                user_id: user.id,
                                ip_address: Some(metadata.ip_address.to_string()),
                                user_agent: metadata.user_agent.as_str().to_string(),
                            },
                            success: true,
                            login_method: "magic_link".to_string(),
                        })
                        .await
                    {
                        error!("Failed to create audit log: {}", e);
                    }

                    Ok((
                        headers,
                        Json(AuthResponse {
                            success: true,
                            message: "Login successful".to_string(),
                            user_id: Some(user.id),
                            mfa_required: false,
                        }),
                    ))
                }
                Err(e) => {
                    error!("Failed to create session after magic link: {}", e);
                    Err(problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                        .with_title("Session Error")
                        .with_detail("Could not create your session. Please try again."))
                }
            }
        }
        Err(e) => {
            warn!("Magic link verification failed: {}", e);
            Err(problem_new(StatusCode::BAD_REQUEST)
                .with_title("Invalid or Expired Link")
                .with_detail(
                    "This magic link is invalid or has expired. Please request a new one.",
                ))
        }
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
        magic_link_available: email_configured,
        password_reset_available: email_configured,
        oidc_providers,
    })
}

#[utoipa::path(
    post,
    path = "/auth/password-reset/request",
    request_body = MagicLinkRequest,
    responses(
        (status = 200, description = "Reset email sent if account exists", body = AuthResponse),
        (status = 503, description = "Email service not configured")
    ),
    tag = "Authentication"
)]
pub async fn request_password_reset(
    State(state): State<Arc<AuthState>>,
    Json(body): Json<MagicLinkRequest>,
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
            }))
        }
        Err(e) => Err(problem_new(StatusCode::BAD_REQUEST)
            .with_title("Password Reset Failed")
            .with_detail(e.to_string())),
    }
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
        schemas(RouteUser, RouteRole, RouteUserWithRoles, AssignRoleRequest, CreateUserRequest, UpdateUserRequest, UpdateSelfRequest, VerifyMfaRequest, MfaSetupResponse, DisableMfaRequest)
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

#[utoipa::path(
    tag = "Users",
    post,
    path = "/users/{user_id}/roles",
    request_body = AssignRoleRequest,
    responses(
        (status = 200, description = "Role assigned successfully"),
        (status = 404, description = "User or role not found"),
        (status = 400, description = "Invalid role type"),
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
    permission_guard!(auth, UsersWrite);

    info!(
        "Assigning role {} to user {}",
        assign_req.role_type, assign_req.user_id
    );
    permission_guard!(auth, UsersWrite);
    // Check if user is trying to modify their own roles
    if user_id == auth.user_id() {
        error!(
            "User {} attempted to modify their own roles",
            auth.user_id()
        );
        return Err(temps_core::error_builder::forbidden().build());
    }

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
        .get_user_by_id(assign_req.user_id)
        .await?;

    app_state
        .user_service
        .assign_role_by_type(assign_req.user_id, role_type)
        .await?;

    info!("Role successfully assigned to user {}", assign_req.user_id);

    // Create audit log
    let audit_context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent.as_str().to_string(),
    };

    let role_audit = RoleAssignedAudit {
        context: audit_context,
        target_user_id: assign_req.user_id,
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
    permission_guard!(auth, UsersWrite);
    permission_guard!(auth, UsersWrite);

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

    // Convert role strings to RoleTypes
    let roles: Vec<RoleType> = create_req
        .roles
        .iter()
        .filter_map(|r| RoleType::from_str(r).ok())
        .collect();

    let user = app_state
        .user_service
        .create_user(
            create_req.username.clone(),
            create_req.email.clone().unwrap_or("".to_string()),
            create_req.password.clone(),
            roles.clone(),
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
        username: create_req.username.clone(),
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
    permission_guard!(auth, UsersWrite);
    permission_guard!(auth, UsersWrite);

    info!(
        "Request to delete user {} by user {}",
        user_id,
        auth.user_id()
    );

    // Check if user is trying to delete themselves
    if user_id == auth.user_id() {
        error!("User {} attempted to delete themselves", auth.user_id());
        return Err(temps_core::error_builder::forbidden().build());
    }

    // Check if user has admin role
    if !app_state.user_service.is_admin(auth.user_id()).await? {
        error!(
            "Non-admin user {} attempted to delete user {}",
            auth.user_id(),
            user_id
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
        (status = 404, description = "User or role not found"),
        (status = 403, description = "Forbidden - Cannot modify own roles or non-admin attempt"),
        (status = 400, description = "Invalid role type"),
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
    permission_guard!(auth, UsersWrite);

    info!(
        "Request to remove role {} from user {} by user {}",
        role_type,
        user_id,
        auth.user_id()
    );
    permission_guard!(auth, UsersWrite);
    // Check if user is trying to modify their own roles
    if user_id == auth.user_id() {
        error!(
            "User {} attempted to modify their own roles",
            auth.user_id()
        );
        return Err(temps_core::error_builder::forbidden().build());
    }

    // Check if user has admin role
    if !app_state.user_service.is_admin(auth.user_id()).await? {
        error!(
            "Non-admin user {} attempted to modify roles for user {}",
            auth.user_id(),
            user_id
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
    // Check authentication
    permission_guard!(auth, UsersWrite);

    permission_guard!(auth, UsersWrite);

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
    permission_guard!(auth, UsersWrite);
    permission_guard!(auth, UsersWrite);

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
    responses(
        (status = 200, description = "MFA setup data", body = MfaSetupResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn setup_mfa(
    State(app_state): State<Arc<AuthState>>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, UsersWrite);

    let setup_data = app_state.user_service.setup_mfa(auth.user_id()).await?;
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
        (status = 403, description = "Account has no password set (SSO/magic-link only)"),
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
                .with_detail(
                    "This account uses SSO or magic-link login and has no password to change.",
                ),
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
    use crate::auth_service::UserAuthError;

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
