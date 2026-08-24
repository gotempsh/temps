use crate::avatar::generate_avatar_data_url;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UserResponse {
    pub id: i32,
    pub username: String,
    pub name: String,
    pub email: Option<String>,
    pub avatar_url: String,
    pub mfa_enabled: bool,
    /// User's role (e.g., "admin", "user", "demo")
    pub role: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CliLoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AuthTokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct TokenRenewalRequest {
    pub refresh_token: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct InitAuthResponse {
    pub auth_url: String,
    pub session_token: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AuthStatusResponse {
    pub status: String,
    pub cli_token: Option<String>,
}
impl From<crate::auth_service::AuthStatusResponse> for AuthStatusResponse {
    fn from(status: crate::auth_service::AuthStatusResponse) -> Self {
        AuthStatusResponse {
            status: status.status,
            cli_token: status.cli_token.clone(),
        }
    }
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct MfaVerificationRequest {
    pub code: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct MfaRequiredResponse {
    pub requires_mfa: bool,
    pub session_token: String,
}

// Add OpenAPI types
#[derive(Serialize, utoipa::ToSchema)]
pub struct RouteUser {
    pub id: i32,
    pub name: String,
    pub username: String,
    pub email: String,
    pub image: String,
    pub mfa_enabled: bool,
    pub email_verified: bool,
    pub must_change_password: bool,
    #[schema(format = "int64", example = "1683900000000")]
    pub created_at: i64,
    #[schema(format = "int64", example = "1683900000000")]
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct RouteRole {
    pub id: i32,
    pub name: String,
    #[schema(format = "int64", example = "1683900000000")]
    pub created_at: i64,
    #[schema(format = "int64", example = "1683900000000")]
    pub updated_at: i64,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct RouteUserWithRoles {
    pub user: RouteUser,
    pub roles: Vec<RouteRole>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct AssignRoleRequest {
    pub user_id: i32,
    pub role_type: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct CreateUserRequest {
    pub username: String,
    pub email: Option<String>,
    pub password: Option<String>,
    pub roles: Vec<String>,
    #[serde(default)]
    pub must_change_password: bool,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct UpdateUserRequest {
    #[schema(example = "john.doe@example.com")]
    pub email: Option<String>,
    #[schema(example = "John Doe")]
    pub name: Option<String>,
}

// Add a new route for self-modification
#[derive(Deserialize, utoipa::ToSchema)]
pub struct UpdateSelfRequest {
    #[schema(example = "john.doe@example.com")]
    pub email: Option<String>,
    #[schema(example = "John Doe")]
    pub name: Option<String>,
}

// In-app password change for the authenticated user. Distinct from the
// out-of-band password-reset flow because it requires the current
// password as a re-auth gate and runs while the user is logged in. When
// the user has MFA enabled, `mfa_code` is required and validated against
// either a TOTP value or a recovery code (same gate as login).
#[derive(Deserialize, utoipa::ToSchema)]
pub struct ChangePasswordRequest {
    #[schema(example = "current_password_value")]
    pub current_password: String,
    #[schema(example = "new_password_value")]
    pub new_password: String,
    /// TOTP code (or recovery code). Required iff the user has MFA enabled.
    #[schema(example = "123456")]
    pub mfa_code: Option<String>,
    /// When true, every session OTHER than the one making this request is
    /// revoked. Defaults to false; the UI surfaces this as a checkbox.
    #[serde(default)]
    pub revoke_other_sessions: bool,
}

// Add new request/response types
#[derive(Deserialize, utoipa::ToSchema)]
pub struct SetupMfaRequest {
    /// Required to enroll MFA on an account that has a password set.
    /// Omit (or leave empty) for SSO-only accounts with no local password.
    #[schema(example = "current_password_value")]
    pub current_password: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct VerifyMfaRequest {
    pub code: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct VerifyStepUpRequest {
    /// Current TOTP value or an unused recovery code.
    pub code: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct StepUpResponse {
    /// ISO 8601 timestamp after which sensitive actions require verification
    /// again.
    #[schema(value_type = String, format = DateTime)]
    pub expires_at: String,
}

#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct MfaSetupResponse {
    pub secret_key: String,
    pub qr_code: String,
    pub recovery_codes: Vec<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct DisableMfaRequest {
    pub code: String,
}

// Add mapping functions
impl From<temps_entities::users::Model> for RouteUser {
    fn from(db_user: temps_entities::users::Model) -> Self {
        Self {
            id: db_user.id,
            name: db_user.name.clone(),
            username: db_user.name.clone(),
            email: db_user.email.clone(),
            image: generate_avatar_data_url(&db_user.name),
            mfa_enabled: db_user.mfa_enabled,
            email_verified: db_user.email_verified,
            must_change_password: db_user.must_change_password,
            created_at: db_user.created_at.timestamp_millis(),
            updated_at: db_user.updated_at.timestamp_millis(),
            deleted_at: db_user.deleted_at.map(|d| d.timestamp_millis()),
        }
    }
}

impl From<temps_entities::roles::Model> for RouteRole {
    fn from(db_role: temps_entities::roles::Model) -> Self {
        Self {
            id: db_role.id,
            name: db_role.name,
            created_at: db_role.created_at.timestamp_millis(),
            updated_at: db_role.updated_at.timestamp_millis(),
        }
    }
}

impl From<crate::user_service::ServiceUser> for RouteUser {
    fn from(service_user: crate::user_service::ServiceUser) -> Self {
        Self {
            id: service_user.id,
            name: service_user.name.clone(),
            username: service_user.name.clone(),
            email: service_user.email,
            image: service_user.image,
            mfa_enabled: service_user.mfa_enabled,
            email_verified: service_user.email_verified,
            must_change_password: service_user.must_change_password,
            created_at: service_user.created_at.timestamp_millis(),
            updated_at: service_user.updated_at.timestamp_millis(),
            deleted_at: service_user.deleted_at.map(|d| d.timestamp_millis()),
        }
    }
}

impl From<crate::user_service::ServiceRole> for RouteRole {
    fn from(service_role: crate::user_service::ServiceRole) -> Self {
        Self {
            id: service_role.id,
            name: service_role.name,
            created_at: service_role.created_at.timestamp_millis(),
            updated_at: service_role.updated_at.timestamp_millis(),
        }
    }
}

impl From<crate::user_service::UserWithRoles> for RouteUserWithRoles {
    fn from(service_user: crate::user_service::UserWithRoles) -> Self {
        Self {
            user: service_user.user.into(),
            roles: service_user.roles.into_iter().map(|r| r.into()).collect(),
        }
    }
}
