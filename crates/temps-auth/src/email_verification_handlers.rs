// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP handlers for email verification: a user asking for a fresh link to
//! their own address, and an administrator vouching for a user's address.
//! What `email_verified` means is documented in [`crate::email_verification`].

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use temps_core::problemdetails::{new as problem_new, Problem};
use temps_core::{AuditContext, RequestMetadata};
use tracing::{error, info};
use utoipa::ToSchema;

use super::AuthState;
use crate::audit::{EmailVerificationRequestedAudit, UserEmailMarkedVerifiedAudit};
use crate::types::RouteUserWithRoles;
use crate::{permission_guard, RequireAuth};

/// Response of `POST /auth/resend-verification`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct VerificationEmailSentResponse {
    /// Address the verification link was sent to.
    pub email: String,
    pub message: String,
}

/// Email a fresh verification link to the signed-in user's address.
///
/// Needs an email provider: without one the request is refused with a 409
/// that tells the user to ask an administrator, who can verify the address
/// from the user's page. `GET /auth/email-status` reports up front whether a
/// provider is configured.
#[utoipa::path(
    post,
    path = "/auth/resend-verification",
    responses(
        (status = 200, description = "Verification email sent", body = VerificationEmailSentResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Only a user account can verify an email address"),
        (status = 409, description = "No email provider is configured, or the address is already verified"),
        (status = 429, description = "A verification email was sent recently, or too many requests"),
        (status = 502, description = "The email provider could not deliver the email"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn resend_email_verification(
    State(state): State<Arc<AuthState>>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<Json<VerificationEmailSentResponse>, Problem> {
    // Self-service on the caller's own account, like changing one's own
    // password: any real user may do it, deployment tokens may not.
    let user = auth.require_user().map_err(|msg| {
        problem_new(StatusCode::FORBIDDEN)
            .with_title("User Required")
            .with_detail(msg)
    })?;
    let user_id = user.id;

    let email = state
        .auth_service
        .resend_email_verification(user_id)
        .await?;

    let audit = EmailVerificationRequestedAudit {
        context: AuditContext {
            user_id,
            ip_address: Some(metadata.ip_address.to_string()),
            user_agent: metadata.user_agent.as_str().to_string(),
        },
        email: email.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(Json(VerificationEmailSentResponse {
        message: format!("A verification link was sent to {email}."),
        email,
    }))
}

/// Mark a user's email address as verified (admin only).
///
/// For instances without an email provider, or a user who cannot receive the
/// link. SSO sign-in is only linked onto an existing account whose address is
/// verified, so this is how an administrator vouches for an address.
#[utoipa::path(
    tag = "Users",
    post,
    path = "/users/{user_id}/verify-email",
    responses(
        (status = 200, description = "Email address marked verified", body = RouteUserWithRoles),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden - requires the users:manage permission"),
        (status = 404, description = "User not found"),
        (status = 409, description = "User is deleted"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("user_id" = i32, Path, description = "User ID")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub(crate) async fn mark_user_email_verified(
    State(app_state): State<Arc<AuthState>>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Path(user_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, UsersManage);

    let updated = app_state.user_service.mark_email_verified(user_id).await?;
    info!(
        admin_id = auth.user_id(),
        user_id, "Administrator marked a user's email address verified"
    );

    let audit = UserEmailMarkedVerifiedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.to_string()),
            user_agent: metadata.user_agent.as_str().to_string(),
        },
        target_user_id: user_id,
        username: updated.name.clone(),
        email: updated.email.clone(),
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    let user = app_state.user_service.get_user_with_roles(user_id).await?;
    Ok(Json(RouteUserWithRoles::from(user)).into_response())
}
