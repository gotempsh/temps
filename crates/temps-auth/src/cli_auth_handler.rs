//! CLI logout endpoint.
//!
//! The CLI no longer accepts passwords in the terminal — interactive login
//! goes through the OAuth-2.0-style device flow in [`cli_device_handler`],
//! and headless / CI authentication uses a pre-minted API key from the web
//! dashboard's **Settings → API Keys** page.
//!
//! What's left in this module is the logout half of the lifecycle:
//! `POST /auth/cli/logout` revokes the bearer key the CLI is currently
//! presenting (whether it was minted by the device flow or pasted from the
//! dashboard) and writes an audit record.

use std::sync::Arc;

use axum::{
    extract::{Extension, State},
    http::StatusCode,
};
use temps_core::problemdetails::{new as problem_new, Problem};
use temps_core::{AuditContext, RequestMetadata};
use thiserror::Error;
use tracing::{debug, error};

use crate::apikey_service::ApiKeyServiceError;
use crate::audit::LogoutAudit;
use crate::state::AuthState;
use crate::RequireAuth;

#[derive(Debug, Error)]
pub enum CliAuthError {
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
            CliAuthError::NotApiKeyAuth => problem_new(StatusCode::FORBIDDEN)
                .with_title("API Key Required")
                .with_detail(err.to_string()),
            CliAuthError::RevokeFailed { .. } | CliAuthError::Database(_) => {
                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(err.to_string())
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_auth_error_status_codes() {
        let p: Problem = CliAuthError::NotApiKeyAuth.into();
        assert_eq!(p.status_code, StatusCode::FORBIDDEN);
        let p: Problem = CliAuthError::RevokeFailed {
            key_id: 1,
            reason: "x".into(),
        }
        .into();
        assert_eq!(p.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }
}
