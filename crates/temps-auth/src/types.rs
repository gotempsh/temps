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

// Add new request/response types
#[derive(Deserialize, utoipa::ToSchema)]
pub struct VerifyMfaRequest {
    pub code: String,
}

#[derive(Serialize, utoipa::ToSchema)]
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
            image: format!(
                "https://ui-avatars.com/api/?name={}&background=random",
                urlencoding::encode(&db_user.name)
            ),
            mfa_enabled: db_user.mfa_enabled,
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
