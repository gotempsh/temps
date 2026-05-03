//! CLI authentication handlers.
//!
//! Endpoints for credential-based login from the Temps CLI. The CLI exchanges
//! email + password (+ optional MFA) for a freshly minted API key that the CLI
//! stores locally (keyring) and uses for subsequent requests.
//!
//! Two-stage MFA flow:
//!   1. CLI POSTs `{email, password}` to `/auth/cli/login`.
//!   2. If MFA is enabled, server returns `{mfa_required: true, mfa_session_token}`.
//!   3. CLI prompts for the code and POSTs `{email, password, mfa_code, mfa_session_token}`
//!      to the same endpoint, which returns the api key.
//!
//! Logout revokes the presented bearer key server-side.

use std::sync::Arc;

use axum::{
    extract::{Extension, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use temps_core::problemdetails::{new as problem_new, Problem};
use temps_core::{AuditContext, RequestMetadata};
use thiserror::Error;
use tracing::{debug, error, warn};
use utoipa::ToSchema;

use crate::apikey_service::{ApiKeyServiceError, CreateApiKeyRequest};
use crate::audit::{LoginAudit, LogoutAudit};
use crate::auth_service::{AuthError, LoginRequest as ServiceLoginRequest, UserAuthError};
use crate::permissions::Role;
use crate::state::AuthState;
use crate::RequireAuth;

/// Default lifetime for a CLI-minted API key.
const CLI_KEY_TTL_DAYS: i64 = 90;
/// Audit `login_method` value for CLI password logins.
const CLI_LOGIN_METHOD: &str = "cli_password";

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct CliLoginPasswordRequest {
    /// User email.
    #[schema(example = "dviejo@kfs.es")]
    pub email: String,
    /// User password.
    pub password: String,
    /// Six-digit TOTP code. Required only when the user has MFA enabled.
    #[schema(example = "123456")]
    pub mfa_code: Option<String>,
    /// Opaque session token returned by a prior `/auth/cli/login` call when
    /// MFA was required. Must be passed back together with `mfa_code`.
    pub mfa_session_token: Option<String>,
    /// Friendly device name shown to the user when they audit API keys.
    /// Defaults to `cli` when omitted.
    #[schema(example = "laptop-david")]
    pub device_name: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(untagged)]
pub enum CliLoginResponse {
    /// MFA is required; CLI must re-call with `mfa_session_token` + `mfa_code`.
    MfaRequired {
        mfa_required: bool,
        mfa_session_token: String,
    },
    /// Login succeeded; returns a freshly minted API key (plaintext, shown once).
    Success {
        user_id: i32,
        email: String,
        role: String,
        api_key: String,
        key_prefix: String,
        #[schema(value_type = Option<String>, format = "date-time")]
        expires_at: Option<temps_core::UtcDateTime>,
    },
}

#[derive(Debug, Error)]
pub enum CliAuthError {
    #[error("Invalid email or password")]
    InvalidCredentials,
    #[error("Invalid or expired MFA code")]
    InvalidMfaCode,
    #[error("MFA session expired or invalid; restart the login flow")]
    MfaSessionInvalid,
    #[error("User {user_id} has no role assigned; cannot mint a CLI API key")]
    NoRoleAssigned { user_id: i32 },
    #[error("Failed to create MFA session for user {user_id}: {reason}")]
    MfaSessionFailed { user_id: i32, reason: String },
    #[error("Failed to mint CLI API key for user {user_id}: {reason}")]
    ApiKeyMintFailed { user_id: i32, reason: String },
    #[error("Failed to revoke CLI API key {key_id}: {reason}")]
    RevokeFailed { key_id: i32, reason: String },
    #[error("This endpoint requires API key authentication; sessions and deployment tokens are not allowed")]
    NotApiKeyAuth,
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
}

impl From<CliAuthError> for Problem {
    fn from(err: CliAuthError) -> Self {
        match &err {
            CliAuthError::InvalidCredentials => problem_new(StatusCode::UNAUTHORIZED)
                .with_title("Invalid Credentials")
                .with_detail(err.to_string()),
            CliAuthError::InvalidMfaCode => problem_new(StatusCode::UNAUTHORIZED)
                .with_title("Invalid MFA Code")
                .with_detail(err.to_string()),
            CliAuthError::MfaSessionInvalid => problem_new(StatusCode::UNAUTHORIZED)
                .with_title("MFA Session Invalid")
                .with_detail(err.to_string()),
            CliAuthError::NoRoleAssigned { .. } => problem_new(StatusCode::FORBIDDEN)
                .with_title("No Role Assigned")
                .with_detail(err.to_string()),
            CliAuthError::NotApiKeyAuth => problem_new(StatusCode::FORBIDDEN)
                .with_title("API Key Required")
                .with_detail(err.to_string()),
            CliAuthError::MfaSessionFailed { .. }
            | CliAuthError::ApiKeyMintFailed { .. }
            | CliAuthError::RevokeFailed { .. }
            | CliAuthError::Database(_) => problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail(err.to_string()),
        }
    }
}

#[utoipa::path(
    post,
    path = "/auth/cli/login",
    request_body = CliLoginPasswordRequest,
    responses(
        (status = 200, description = "Login successful or MFA challenge issued", body = CliLoginResponse),
        (status = 401, description = "Invalid credentials or MFA code"),
        (status = 403, description = "User has no role assigned"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Authentication"
)]
pub async fn cli_login(
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CliLoginPasswordRequest>,
) -> Result<Json<CliLoginResponse>, Problem> {
    // 1. Verify email + password.
    let user = match state
        .auth_service
        .login(ServiceLoginRequest {
            email: request.email.clone(),
            password: request.password.clone(),
        })
        .await
    {
        Ok(u) => u,
        Err(e) => {
            warn!(
                "CLI login: invalid credentials for {}: {}",
                request.email, e
            );
            audit_login(&state, 0, &metadata, false).await;
            return match e {
                UserAuthError::InvalidCredentials => Err(CliAuthError::InvalidCredentials.into()),
                UserAuthError::DatabaseError(db) => Err(CliAuthError::Database(db).into()),
                _ => Err(CliAuthError::InvalidCredentials.into()),
            };
        }
    };

    // 2. Handle MFA — either issue a challenge or verify the supplied code.
    if user.mfa_enabled {
        match (
            request.mfa_session_token.as_deref(),
            request.mfa_code.as_deref(),
        ) {
            (Some(session_token), Some(code)) => {
                match state
                    .auth_service
                    .verify_mfa_challenge(session_token, code)
                    .await
                {
                    Ok(verified_user) => {
                        debug!("CLI login: MFA verified for user {}", verified_user.id);
                        // Fall through to API key minting.
                        return mint_key_and_respond(
                            &state,
                            verified_user,
                            request.device_name.as_deref(),
                            &metadata,
                        )
                        .await;
                    }
                    Err(AuthError::NotFound(_)) => {
                        return Err(CliAuthError::MfaSessionInvalid.into());
                    }
                    Err(e) => {
                        warn!("CLI login: MFA verify failed for user {}: {}", user.id, e);
                        return Err(CliAuthError::InvalidMfaCode.into());
                    }
                }
            }
            _ => {
                // Issue MFA challenge.
                let session_token = state
                    .auth_service
                    .create_mfa_session(user.id)
                    .await
                    .map_err(|e| CliAuthError::MfaSessionFailed {
                        user_id: user.id,
                        reason: e.to_string(),
                    })?;
                return Ok(Json(CliLoginResponse::MfaRequired {
                    mfa_required: true,
                    mfa_session_token: session_token,
                }));
            }
        }
    }

    // 3. No MFA — mint key directly.
    mint_key_and_respond(&state, user, request.device_name.as_deref(), &metadata).await
}

#[utoipa::path(
    post,
    path = "/auth/cli/logout",
    responses(
        (status = 204, description = "API key revoked"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Endpoint requires API key authentication"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn cli_logout(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<StatusCode, Problem> {
    let (key_name, key_id) = auth.api_key_info().ok_or(CliAuthError::NotApiKeyAuth)?;
    let user_id = auth.user_id();
    let username = auth
        .user
        .as_ref()
        .map(|u| u.email.clone())
        .unwrap_or_default();

    state
        .api_key_service
        .delete_api_key(user_id, key_id)
        .await
        .map_err(|e| match e {
            ApiKeyServiceError::NotFound(_) => Problem::from(CliAuthError::RevokeFailed {
                key_id,
                reason: "API key not found".into(),
            }),
            other => Problem::from(CliAuthError::RevokeFailed {
                key_id,
                reason: other.to_string(),
            }),
        })?;

    let audit = LogoutAudit {
        context: AuditContext {
            user_id,
            ip_address: Some(metadata.ip_address.to_string()),
            user_agent: metadata.user_agent.as_str().to_string(),
        },
        username,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create CLI logout audit log: {}", e);
    }

    debug!(
        "CLI logout: user {} revoked api key {} ({})",
        user_id, key_id, key_name
    );
    Ok(StatusCode::NO_CONTENT)
}

async fn mint_key_and_respond(
    state: &Arc<AuthState>,
    user: temps_entities::users::Model,
    device_name: Option<&str>,
    metadata: &RequestMetadata,
) -> Result<Json<CliLoginResponse>, Problem> {
    // Determine the user's primary role to scope the new key.
    let user_with_roles = state
        .user_service
        .get_user_with_roles(user.id)
        .await
        .map_err(|e| CliAuthError::ApiKeyMintFailed {
            user_id: user.id,
            reason: format!("Failed to load user roles: {}", e),
        })?;

    let role_name = pick_primary_role(&user_with_roles)
        .ok_or(CliAuthError::NoRoleAssigned { user_id: user.id })?;

    // Build the api-key request: name = "cli:<device>-<unix_ts>", role inherited.
    //
    // `device_name` is supplied by the client and surfaced verbatim in the
    // user's API-key management UI. Sanitize it so a hostile/typo client
    // can't store HTML/JS or control characters in someone else's key list,
    // and bound length so the persisted name stays printable.
    let device = sanitize_device_name(device_name.unwrap_or("cli"));
    let timestamp = chrono::Utc::now().timestamp();
    let key_name = format!("cli:{device}-{timestamp}");
    let expires_at = Some(chrono::Utc::now() + chrono::Duration::days(CLI_KEY_TTL_DAYS));

    let created = state
        .api_key_service
        .create_api_key(
            user.id,
            CreateApiKeyRequest {
                name: key_name,
                role_type: role_name.clone(),
                permissions: None,
                expires_at,
            },
        )
        .await
        .map_err(|e| CliAuthError::ApiKeyMintFailed {
            user_id: user.id,
            reason: e.to_string(),
        })?;

    audit_login(state, user.id, metadata, true).await;

    Ok(Json(CliLoginResponse::Success {
        user_id: user.id,
        email: user.email.clone(),
        role: role_name,
        api_key: created.api_key,
        key_prefix: created.key_prefix,
        expires_at: created.expires_at,
    }))
}

/// Sanitize a CLI `device_name` before splicing it into the API-key
/// `name` field. Keeps `[A-Za-z0-9._-]`, replaces every other byte with
/// `_`, and clamps length to 64 chars. Falls back to `"cli"` when the
/// result would be empty.
///
/// Rationale: the key name is shown verbatim in the user's API-key
/// management UI. Without sanitization, a malformed (or hostile) client
/// could plant HTML, control characters, or extremely long strings into
/// that surface. Hostnames from `hostname(1)` already fall in the safe
/// alphabet, so legitimate use is unaffected.
fn sanitize_device_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(64));
    for c in raw.chars().take(64) {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches(|c: char| c == '_' || c == '.' || c == '-');
    if trimmed.is_empty() {
        "cli".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Pick the user's primary role for CLI-key minting. Prefers admin > user >
/// reader > anything else. Returns `None` only when the user has no roles.
fn pick_primary_role(user_with_roles: &crate::user_service::UserWithRoles) -> Option<String> {
    if user_with_roles.roles.is_empty() {
        return None;
    }
    let priorities = [
        Role::Admin.to_string(),
        Role::User.to_string(),
        Role::Reader.to_string(),
    ];
    for preferred in &priorities {
        if user_with_roles
            .roles
            .iter()
            .any(|r| r.name.eq_ignore_ascii_case(preferred))
        {
            return Some(preferred.clone());
        }
    }
    user_with_roles.roles.first().map(|r| r.name.clone())
}

async fn audit_login(
    state: &Arc<AuthState>,
    user_id: i32,
    metadata: &RequestMetadata,
    success: bool,
) {
    let audit = LoginAudit {
        context: AuditContext {
            user_id,
            ip_address: Some(metadata.ip_address.to_string()),
            user_agent: metadata.user_agent.as_str().to_string(),
        },
        success,
        login_method: CLI_LOGIN_METHOD.to_string(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create CLI login audit log: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_user() -> crate::user_service::ServiceUser {
        let now = chrono::Utc::now();
        crate::user_service::ServiceUser {
            id: 1,
            name: "x".into(),
            email: "x@x".into(),
            image: String::new(),
            mfa_enabled: false,
            email_verified: true,
            created_at: now,
            updated_at: now,
            deleted_at: None,
        }
    }

    fn make_role(id: i32, name: &str) -> crate::user_service::ServiceRole {
        let now = chrono::Utc::now();
        crate::user_service::ServiceRole {
            id,
            name: name.into(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn pick_primary_role_picks_admin_first() {
        use crate::user_service::UserWithRoles;
        let uwr = UserWithRoles {
            user: make_user(),
            roles: vec![make_role(2, "reader"), make_role(1, "admin")],
        };
        assert_eq!(pick_primary_role(&uwr).as_deref(), Some("admin"));
    }

    #[test]
    fn pick_primary_role_falls_back_to_first_when_no_priority_match() {
        use crate::user_service::UserWithRoles;
        let uwr = UserWithRoles {
            user: make_user(),
            roles: vec![make_role(1, "mcp")],
        };
        assert_eq!(pick_primary_role(&uwr).as_deref(), Some("mcp"));
    }

    #[test]
    fn pick_primary_role_returns_none_for_no_roles() {
        use crate::user_service::UserWithRoles;
        let uwr = UserWithRoles {
            user: make_user(),
            roles: vec![],
        };
        assert!(pick_primary_role(&uwr).is_none());
    }

    #[test]
    fn sanitize_device_name_keeps_safe_chars() {
        assert_eq!(sanitize_device_name("dviejo-mac.local"), "dviejo-mac.local");
        assert_eq!(sanitize_device_name("MyHost_42"), "MyHost_42");
    }

    #[test]
    fn sanitize_device_name_replaces_unsafe_chars() {
        // Leading/trailing separators are trimmed after substitution.
        assert_eq!(
            sanitize_device_name("<script>alert(1)</script>"),
            "script_alert_1___script"
        );
        assert_eq!(
            sanitize_device_name("name\nwith\nnewlines"),
            "name_with_newlines"
        );
    }

    #[test]
    fn sanitize_device_name_clamps_length() {
        let long = "x".repeat(200);
        let out = sanitize_device_name(&long);
        assert!(out.len() <= 64, "got {} chars", out.len());
    }

    #[test]
    fn sanitize_device_name_falls_back_for_empty_or_only_separators() {
        assert_eq!(sanitize_device_name(""), "cli");
        assert_eq!(sanitize_device_name("---___..."), "cli");
        assert_eq!(sanitize_device_name("@@@!!!"), "cli");
    }

    #[test]
    fn cli_auth_error_status_codes() {
        let p: Problem = CliAuthError::InvalidCredentials.into();
        assert_eq!(p.status_code, StatusCode::UNAUTHORIZED);
        let p: Problem = CliAuthError::InvalidMfaCode.into();
        assert_eq!(p.status_code, StatusCode::UNAUTHORIZED);
        let p: Problem = CliAuthError::NoRoleAssigned { user_id: 1 }.into();
        assert_eq!(p.status_code, StatusCode::FORBIDDEN);
        let p: Problem = CliAuthError::NotApiKeyAuth.into();
        assert_eq!(p.status_code, StatusCode::FORBIDDEN);
    }
}
