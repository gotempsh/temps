// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Saved Cloudflare Access tokens (ADR 065 D9).
//!
//! - `GET /cloudflare-access/providers`: the saved tokens, masked.
//! - `POST /cloudflare-access/providers/check`: which Access permissions a
//!   pasted token has; nothing is saved.
//! - `POST /cloudflare-access/providers`: check, then save sealed.
//! - `DELETE /cloudflare-access/providers/{provider_id}`: only when no
//!   address uses it.

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use serde::Serialize;
use temps_auth::{permission_guard, AuthContext, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::{AuditContext, AuditLogger, AuditOperation, RequestMetadata};
use tracing::{error, info};
use utoipa::OpenApi;

use super::access::{
    AccessPermission, AccessPermissionCheck, AccessTokenCheckRequest, AccessTokenCheckResult,
};
use super::access_service::{
    AccessProviderError, AccessProviderResponse, AccessProviderService, CreateAccessProviderRequest,
};

pub struct AccessAppState {
    pub service: Arc<AccessProviderService>,
    pub audit_service: Arc<dyn AuditLogger>,
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_access_providers,
        check_access_provider,
        create_access_provider,
        delete_access_provider
    ),
    components(schemas(
        AccessProviderResponse,
        CreateAccessProviderRequest,
        AccessTokenCheckRequest,
        AccessTokenCheckResult,
        AccessPermissionCheck,
        AccessPermission
    )),
    tags((name = "Signed-in addresses", description = "The Temps sign-in in front of signed-in addresses (ADR 065)"))
)]
pub struct AccessApiDoc;

pub fn configure_routes() -> Router<Arc<AccessAppState>> {
    Router::new()
        .route(
            "/cloudflare-access/providers",
            get(list_access_providers).post(create_access_provider),
        )
        .route(
            "/cloudflare-access/providers/check",
            post(check_access_provider),
        )
        .route(
            "/cloudflare-access/providers/{provider_id}",
            delete(delete_access_provider),
        )
}

/// The console's answer for an Access token error. A function rather than
/// a `From` impl, like the gate's.
fn access_problem(error: AccessProviderError) -> Problem {
    let (status, title) = match &error {
        AccessProviderError::NotFound { .. } => {
            (StatusCode::NOT_FOUND, "Cloudflare Access Token Not Found")
        }
        AccessProviderError::Validation { .. } | AccessProviderError::TokenRefused { .. } => {
            (StatusCode::BAD_REQUEST, "Cloudflare Access Token Refused")
        }
        AccessProviderError::InUse { .. } | AccessProviderError::NeededToUndo { .. } => {
            (StatusCode::CONFLICT, "Cloudflare Access Token In Use")
        }
        AccessProviderError::Encryption { .. } | AccessProviderError::Database { .. } => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Cloudflare Access Token Error",
        ),
    };
    problemdetails::new(status)
        .with_title(title)
        .with_detail(error.to_string())
}

/// Never carries the token.
#[derive(Debug, Serialize)]
struct AccessProviderAudit {
    #[serde(skip)]
    action: &'static str,
    context: AuditContext,
    provider_id: i32,
    name: String,
    account_id: String,
    team_domain: String,
}

impl AuditOperation for AccessProviderAudit {
    fn operation_type(&self) -> String {
        self.action.to_string()
    }
    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }
    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }
    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }
    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {e}"))
    }
}

async fn audit(state: &AccessAppState, record: AccessProviderAudit) {
    if let Err(error) = state.audit_service.create_audit_log(&record).await {
        error!(%error, action = record.action, provider_id = record.provider_id,
            "Cloudflare Access token change made but audit record failed");
    }
}

fn audit_context(auth: &AuthContext, metadata: Option<&RequestMetadata>) -> AuditContext {
    AuditContext {
        user_id: auth.user_id(),
        ip_address: metadata.map(|m| m.ip_address.clone()),
        user_agent: metadata.map(|m| m.user_agent.clone()).unwrap_or_default(),
    }
}

/// The saved Cloudflare Access tokens. Tokens are never returned.
#[utoipa::path(
    tag = "Signed-in addresses",
    get,
    path = "/cloudflare-access/providers",
    operation_id = "list_access_providers",
    responses(
        (status = 200, body = Vec<AccessProviderResponse>),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn list_access_providers(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DnsProvidersRead);
    Ok(Json(state.service.list().await.map_err(access_problem)?))
}

/// Check which Cloudflare Access permissions a pasted token has: Access:
/// Apps and Policies Edit (probed with an app body Cloudflare rejects, so
/// nothing is created) and Access: Organizations, Identity Providers, and
/// Groups Read. The token is used for this check only and is not stored.
#[utoipa::path(
    tag = "Signed-in addresses",
    post,
    path = "/cloudflare-access/providers/check",
    operation_id = "check_access_provider",
    request_body = AccessTokenCheckRequest,
    responses(
        (status = 200, body = AccessTokenCheckResult),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn check_access_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Json(request): Json<AccessTokenCheckRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DnsProvidersWrite);
    Ok(Json(
        state
            .service
            .check(request.token.expose(), request.account_id.trim())
            .await,
    ))
}

/// Check a Cloudflare Access token, then save it sealed. Refused, with the
/// check in the detail, unless every permission is there.
#[utoipa::path(
    tag = "Signed-in addresses",
    post,
    path = "/cloudflare-access/providers",
    operation_id = "create_access_provider",
    request_body = CreateAccessProviderRequest,
    responses(
        (status = 201, body = AccessProviderResponse),
        (status = 400, description = "The token is missing a permission", body = temps_core::problemdetails::ProblemDetails),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn create_access_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    metadata: Option<Extension<RequestMetadata>>,
    Json(request): Json<CreateAccessProviderRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DnsProvidersWrite);
    let saved = state
        .service
        .create(&request, Some(auth.user_id()))
        .await
        .map_err(access_problem)?;
    info!(provider_id = saved.id, team_domain = %saved.team_domain, "Cloudflare Access token saved");
    audit(
        &state,
        AccessProviderAudit {
            action: "CLOUDFLARE_ACCESS_TOKEN_CREATED",
            context: audit_context(&auth, metadata.as_deref()),
            provider_id: saved.id,
            name: saved.name.clone(),
            account_id: saved.account_id.clone(),
            team_domain: saved.team_domain.clone(),
        },
    )
    .await;
    Ok((StatusCode::CREATED, Json(saved)))
}

/// Delete a saved Cloudflare Access token. Refused while an address uses it.
#[utoipa::path(
    tag = "Signed-in addresses",
    delete,
    path = "/cloudflare-access/providers/{provider_id}",
    operation_id = "delete_access_provider",
    params(("provider_id" = i32, Path, description = "Saved token")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 409, description = "An address uses it", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn delete_access_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    metadata: Option<Extension<RequestMetadata>>,
    Path(provider_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DnsProvidersWrite);
    let deleted = state
        .service
        .delete(provider_id)
        .await
        .map_err(access_problem)?;
    info!(provider_id, "Cloudflare Access token deleted");
    audit(
        &state,
        AccessProviderAudit {
            action: "CLOUDFLARE_ACCESS_TOKEN_DELETED",
            context: audit_context(&auth, metadata.as_deref()),
            provider_id,
            name: deleted.name,
            account_id: deleted.account_id,
            team_domain: deleted.team_domain,
        },
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_map_to_their_status() {
        let cases = [
            (
                AccessProviderError::NotFound { provider_id: 3 },
                StatusCode::NOT_FOUND,
            ),
            (
                AccessProviderError::InUse {
                    provider_id: 3,
                    addresses: 2,
                },
                StatusCode::CONFLICT,
            ),
            (
                AccessProviderError::NeededToUndo {
                    provider_id: 3,
                    runs: vec![7],
                },
                StatusCode::CONFLICT,
            ),
            (
                AccessProviderError::Validation {
                    message: "x".into(),
                },
                StatusCode::BAD_REQUEST,
            ),
        ];
        for (error, status) in cases {
            let detail = error.to_string();
            let problem = access_problem(error);
            assert_eq!(problem.status_code, status);
            assert!(detail.contains('3') || detail.contains('x'));
        }
    }

    #[test]
    fn the_audit_record_never_carries_a_token() {
        let record = AccessProviderAudit {
            action: "CLOUDFLARE_ACCESS_TOKEN_CREATED",
            context: AuditContext {
                user_id: 1,
                ip_address: None,
                user_agent: String::new(),
            },
            provider_id: 1,
            name: "access".into(),
            account_id: "acc".into(),
            team_domain: "team.cloudflareaccess.com".into(),
        };
        let json = AuditOperation::serialize(&record).unwrap_or_default();
        assert!(!json.contains("token\""));
        assert!(json.contains("team.cloudflareaccess.com"));
    }
}
