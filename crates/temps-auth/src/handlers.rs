use super::AuthState;
use crate::audit::{
    EmailVerifiedAudit, LoginAudit, LogoutAudit, MfaDisabledAudit, MfaEnabledAudit,
    MfaVerifiedAudit, PasswordResetAudit, RoleAssignedAudit, RoleRemovedAudit, UpdatedFields,
    UserCreatedAudit, UserDeletedAudit, UserRestoredAudit, UserUpdatedAudit,
};
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
use std::sync::Arc;
pub use temps_core::AuditContext;
use temps_core::RequestMetadata;
use temps_entities::types::RoleType;
use tracing::{error, info, warn};
use utoipa::{OpenApi, ToSchema};

use crate::types::{
    AssignRoleRequest, AuthStatusResponse, AuthTokenResponse, CliLoginRequest, CreateUserRequest,
    DisableMfaRequest, InitAuthResponse, MfaRequiredResponse, MfaSetupResponse,
    MfaVerificationRequest, RouteRole, RouteUser, RouteUserWithRoles, TokenRenewalRequest,
    UpdateSelfRequest, UpdateUserRequest, UserResponse, VerifyMfaRequest,
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
    let user = auth.user;
    let user_response = UserResponse {
        id: user.id,
        username: user.name.clone(),
        name: user.name.clone(),
        email: Some(user.email.clone()),
        avatar_url: format!(
            "https://ui-avatars.com/api/?name={}&background=random",
            urlencoding::encode(&user.name)
        ),
        mfa_enabled: user.mfa_enabled,
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
    let user = auth.user;
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
                Json(json!({"error": e.to_string()})),
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
                .with_title("Failed to decrypt MFA session cookie")
                .with_detail("MFA session cookie not found"),
        )?;

    // Decrypt the MFA session cookie
    let mfa_session = auth_state
        .cookie_crypto
        .decrypt(&encrypted_mfa_session)
        .map_err(|e| {
            tracing::error!("Failed to decrypt MFA session cookie: {}", e);
            problem_new(StatusCode::UNAUTHORIZED)
                .with_title("Failed to decrypt MFA session cookie")
                .with_detail(e.to_string())
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
                context: audit_context,
                username: user.email.clone(),
            };

            if let Err(e) = auth_state.audit_service.create_audit_log(&audit).await {
                error!("Failed to create audit log: {}", e);
            }

            let session_token = auth_state
                .auth_service
                .create_session(user.id)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

            let session_token_encrypted = match auth_state.cookie_crypto.encrypt(&session_token) {
                Ok(enc) => enc,
                Err(e) => {
                    return Err(problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                        .with_title("Encryption Error")
                        .with_detail(e.to_string()))
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
            response_headers.insert(SET_COOKIE, clear_mfa_cookie.to_string().parse().unwrap());

            Ok((StatusCode::NO_CONTENT, response_headers))
        }
        Err(e) => {
            error!("MFA verification failed: {}", e);
            Err(problem_new(StatusCode::UNAUTHORIZED)
                .with_title("MFA verification failed")
                .with_detail(e.to_string()))
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
        setup_mfa,
        verify_and_enable_mfa,
        disable_mfa
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
            RouteUser,
            RouteRole,
            RouteUserWithRoles,
            AssignRoleRequest,
            CreateUserRequest,
            UpdateUserRequest,
            UpdateSelfRequest,
            VerifyMfaRequest,
            MfaSetupResponse,
            DisableMfaRequest
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
    Router::new()
        .route("/auth/verify-mfa", post(verify_mfa_challenge))
        .route("/user/me", get(get_current_user))
        .route("/logout", post(logout))
        .route("/auth/login", post(login))
        .route("/auth/email-status", get(email_status))
        .route("/auth/magic-link/request", post(request_magic_link))
        .route("/auth/magic-link/verify", get(verify_magic_link))
        .route("/auth/password-reset/request", post(request_password_reset))
        .route("/auth/password-reset/verify", post(reset_password))
        .route("/auth/verify-email", get(verify_email))
        .route("/users", get(list_users))
        .route("/users", post(create_user))
        .route("/users/me", patch(update_self))
        .route("/users/me/mfa/setup", post(setup_mfa))
        .route("/users/me/mfa/verify", post(verify_and_enable_mfa))
        .route("/users/me/mfa", delete(disable_mfa))
        .route("/users/{user_id}", delete(delete_user))
        .route("/users/{user_id}", patch(update_user))
        .route("/users/{user_id}/restore", post(restore_user))
        .route("/users/{user_id}/roles", post(assign_role))
        .route("/users/{user_id}/roles/{role_type}", delete(remove_role))
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
        (status = 401, description = "Invalid credentials"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Authentication"
)]
pub async fn login(
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<LoginRequest>,
) -> Result<impl IntoResponse, temps_core::problemdetails::Problem> {
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
                                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                                    .with_title("Encryption Error")
                                    .with_detail(e.to_string())
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
                    Err(e) => Err(problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                        .with_title("Failed to create MFA session")
                        .with_detail(e.to_string())),
                }
            } else {
                // Create regular session
                match state.auth_service.create_session(user.id).await {
                    Ok(session_token) => {
                        // Encrypt the session token
                        let encrypted_token =
                            state.cookie_crypto.encrypt(&session_token).map_err(|e| {
                                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                                    .with_title("Encryption Error")
                                    .with_detail(e.to_string())
                            })?;

                        // Use the pre-calculated secure flag from metadata

                        // Create session cookie headers using pre-calculated secure flag
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
                                login_method: "password".to_string(),
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
                        Err(problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                            .with_title("Failed to create session")
                            .with_detail(e.to_string()))
                    }
                }
            }
        }
        Err(e) => Err(problem_new(StatusCode::UNAUTHORIZED)
            .with_title("Invalid Credentials")
            .with_detail(e.to_string())),
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
    if !state.auth_service.is_email_configured() {
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
                Err(e) => Err(problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Failed to create session")
                    .with_detail(e.to_string())),
            }
        }
        Err(e) => Err(problem_new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Token")
            .with_detail(e.to_string())),
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
    let email_configured = state.auth_service.is_email_configured();

    Json(EmailStatusResponse {
        email_configured,
        magic_link_available: email_configured,
        password_reset_available: email_configured,
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
    if !state.auth_service.is_email_configured() {
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
                    .with_title("Encryption error")
                    .with_detail(msg)
            }
            UserServiceError::Io(e) => {
                error!("IO error: {}", e);
                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("IO error")
                    .with_detail(e.to_string())
            }
            UserServiceError::Serialization(e) => {
                error!("Serialization error: {}", e);
                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Serialization error")
                    .with_detail(e.to_string())
            }
            UserServiceError::Internal(msg) => {
                error!("Internal error: {}", msg);
                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal error")
                    .with_detail(msg)
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
        Some(rt) => rt,
        None => {
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

    // Convert role strings to RoleTypes
    let roles: Vec<RoleType> = create_req
        .roles
        .iter()
        .filter_map(|r| RoleType::from_str(r))
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
        Some(rt) => rt,
        None => {
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
        .update_user(auth.user_id(), update_req.email.clone(), update_req.name.clone())
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
        username: auth.user.name.clone(),
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
        username: auth.user.name.clone(),
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&mfa_audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(StatusCode::NO_CONTENT.into_response())
}
