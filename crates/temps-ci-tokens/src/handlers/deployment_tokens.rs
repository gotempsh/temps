// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deployment Token Handlers
//!
//! REST API endpoints for managing deployment tokens that provide
//! TEMPS_API_URL and TEMPS_API_TOKEN environment variables to deployed applications.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Extension, Json, Router,
};
use temps_auth::{permission_guard, project_access_guard, require_sensitive_action};
use temps_auth::{AuthContext, RequireAuth};
use temps_core::{AuditContext, RequestMetadata, SensitiveAction};
use temps_entities::deployment_tokens::DeploymentTokenPermission;
use tracing::error;
use utoipa::OpenApi;

use super::audit::DeploymentTokenRotatedAudit;
use crate::services::deployment_token_service::{
    CreateDeploymentTokenRequest, CreateDeploymentTokenResponse, DeploymentTokenListResponse,
    DeploymentTokenResponse, DeploymentTokenService, UpdateDeploymentTokenRequest,
};
use temps_core::problemdetails::Problem;

use serde::Deserialize;
use utoipa::ToSchema;

/// App state for deployment token handlers
pub struct DeploymentTokenAppState {
    pub deployment_token_service: Arc<DeploymentTokenService>,
    /// Audit logger for write operations (e.g. token rotation)
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
    /// Optional checker enforcing team-based project access for human sessions.
    ///
    /// `None` when no plugin implementing this check is registered — the
    /// `project_access_guard!` macro is a strict synchronous no-op in that
    /// case. Resolved once in `configure_routes` via
    /// `context.get_service::<dyn temps_core::ProjectAccessChecker>()`.
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    /// Central sensitive-action policy (MFA step-up), used to gate destructive
    /// token operations (rotate, delete). Resolved once in `configure_routes`
    /// via `context.require_service::<dyn temps_core::SensitiveActionAuthorizer>()`.
    pub sensitive_action_authorizer: Arc<dyn temps_core::SensitiveActionAuthorizer>,
}

/// Privilege-escalation ceiling: a deployment token must never carry more than
/// the principal that creates or edits it, the same rule API keys follow.
///
/// Without it, any role holding `DeploymentTokensCreate` (the standard
/// `Role::User` does) could mint a token with permissions its own role lacks,
/// for example `visitors:enrich` (needs `AnalyticsWrite`) or `blob:delete`, and
/// then use the token to do what the role itself is refused.
///
/// Permission names that do not parse are left to the service's validation,
/// which answers 400.
fn enforce_permission_ceiling(auth: &AuthContext, permissions: &[String]) -> Result<(), Problem> {
    for name in permissions {
        let Some(requested) = DeploymentTokenPermission::from_str(name) else {
            continue;
        };
        if let Some(missing) = auth.missing_permission_to_grant(&requested) {
            return Err(
                temps_core::error_builder::ErrorBuilder::new(StatusCode::FORBIDDEN)
                    .type_("https://temps.sh/probs/permission-ceiling-exceeded")
                    .title("Forbidden")
                    .detail(format!(
                        "Cannot grant deployment token permission '{name}': it needs '{missing}', \
                     which exceeds your own permissions"
                    ))
                    .value("error_code", "PERMISSION_CEILING_EXCEEDED")
                    .build(),
            );
        }
    }
    Ok(())
}

/// Ceiling for token creation. Leaving `permissions` out creates a full-access
/// token, so it needs the same ceiling as asking for `*`.
fn enforce_create_ceiling(
    auth: &AuthContext,
    permissions: Option<&[String]>,
) -> Result<(), Problem> {
    let full_access = ["*".to_string()];
    enforce_permission_ceiling(auth, permissions.unwrap_or(&full_access))
}

/// Ceiling for token updates. An update that leaves `permissions` out keeps the
/// stored permissions, so there is nothing new to grant.
fn enforce_update_ceiling(
    auth: &AuthContext,
    permissions: Option<&[String]>,
) -> Result<(), Problem> {
    permissions.map_or(Ok(()), |requested| {
        enforce_permission_ceiling(auth, requested)
    })
}

#[derive(Deserialize, ToSchema)]
pub struct ListDeploymentTokensQuery {
    #[schema(example = 1)]
    pub page: Option<u64>,
    #[schema(example = 20)]
    pub page_size: Option<u64>,
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_deployment_tokens,
        create_deployment_token,
        get_deployment_token,
        update_deployment_token,
        delete_deployment_token,
        rotate_deployment_token,
    ),
    components(schemas(
        DeploymentTokenResponse,
        CreateDeploymentTokenResponse,
        DeploymentTokenListResponse,
        CreateDeploymentTokenRequest,
        UpdateDeploymentTokenRequest,
        ListDeploymentTokensQuery,
    )),
    info(
        title = "Deployment Tokens API",
        description = "API endpoints for managing deployment tokens. \
        Deployment tokens provide API access credentials (TEMPS_API_URL and TEMPS_API_TOKEN) \
        that are automatically injected into deployed applications, allowing them to access \
        Temps platform APIs for visitor enrichment, email sending, and more.",
        version = "1.0.0"
    )
)]
pub struct DeploymentTokensApiDoc;

/// Configure deployment token routes
pub fn configure_routes() -> Router<Arc<DeploymentTokenAppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/deployment-tokens",
            get(list_deployment_tokens).post(create_deployment_token),
        )
        .route(
            "/projects/{project_id}/deployment-tokens/{token_id}",
            get(get_deployment_token)
                .patch(update_deployment_token)
                .delete(delete_deployment_token),
        )
        .route(
            "/projects/{project_id}/deployment-tokens/{token_id}/rotate",
            axum::routing::post(rotate_deployment_token),
        )
}

/// List all deployment tokens for a project
#[utoipa::path(
    tag = "Deployment Tokens",
    get,
    path = "/projects/{project_id}/deployment-tokens",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("page" = Option<u64>, Query, description = "Page number (default: 1)"),
        ("page_size" = Option<u64>, Query, description = "Page size (default: 20, max: 100)")
    ),
    responses(
        (status = 200, description = "List of deployment tokens", body = DeploymentTokenListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn list_deployment_tokens(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DeploymentTokenAppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<ListDeploymentTokensQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentTokensRead);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    let page = query.page.unwrap_or(1);
    let page_size = std::cmp::min(query.page_size.unwrap_or(20), 100);

    let response = app_state
        .deployment_token_service
        .list_tokens(project_id, page, page_size)
        .await
        .map_err(|e| {
            error!("Failed to list deployment tokens: {}", e);
            e.to_problem()
        })?;

    Ok(Json(response))
}

/// Create a new deployment token for a project
#[utoipa::path(
    tag = "Deployment Tokens",
    post,
    path = "/projects/{project_id}/deployment-tokens",
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    request_body = CreateDeploymentTokenRequest,
    responses(
        (status = 201, description = "Deployment token created successfully", body = CreateDeploymentTokenResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 409, description = "Token with this name already exists"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn create_deployment_token(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DeploymentTokenAppState>>,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateDeploymentTokenRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentTokensCreate);
    project_access_guard!(auth, project_id, app_state.project_access_checker);
    enforce_create_ceiling(&auth, request.permissions.as_deref())?;

    let response = app_state
        .deployment_token_service
        .create_token(project_id, Some(auth.user_id()), request)
        .await
        .map_err(|e| {
            error!("Failed to create deployment token: {}", e);
            e.to_problem()
        })?;

    Ok((StatusCode::CREATED, Json(response)))
}

/// Get a specific deployment token
#[utoipa::path(
    tag = "Deployment Tokens",
    get,
    path = "/projects/{project_id}/deployment-tokens/{token_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("token_id" = i32, Path, description = "Deployment token ID")
    ),
    responses(
        (status = 200, description = "Deployment token details", body = DeploymentTokenResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Deployment token not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn get_deployment_token(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DeploymentTokenAppState>>,
    Path((project_id, token_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentTokensRead);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    let response = app_state
        .deployment_token_service
        .get_token(project_id, token_id)
        .await
        .map_err(|e| {
            error!("Failed to get deployment token: {}", e);
            e.to_problem()
        })?;

    Ok(Json(response))
}

/// Update a deployment token
#[utoipa::path(
    tag = "Deployment Tokens",
    patch,
    path = "/projects/{project_id}/deployment-tokens/{token_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("token_id" = i32, Path, description = "Deployment token ID")
    ),
    request_body = UpdateDeploymentTokenRequest,
    responses(
        (status = 200, description = "Deployment token updated successfully", body = DeploymentTokenResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Deployment token not found"),
        (status = 409, description = "Token with this name already exists"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn update_deployment_token(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DeploymentTokenAppState>>,
    Path((project_id, token_id)): Path<(i32, i32)>,
    Json(request): Json<UpdateDeploymentTokenRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentTokensWrite);
    project_access_guard!(auth, project_id, app_state.project_access_checker);
    enforce_update_ceiling(&auth, request.permissions.as_deref())?;

    let response = app_state
        .deployment_token_service
        .update_token(project_id, token_id, request)
        .await
        .map_err(|e| {
            error!("Failed to update deployment token: {}", e);
            e.to_problem()
        })?;

    Ok(Json(response))
}

/// Delete a deployment token
#[utoipa::path(
    tag = "Deployment Tokens",
    delete,
    path = "/projects/{project_id}/deployment-tokens/{token_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("token_id" = i32, Path, description = "Deployment token ID")
    ),
    responses(
        (status = 204, description = "Deployment token deleted successfully"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Deployment token not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn delete_deployment_token(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DeploymentTokenAppState>>,
    Path((project_id, token_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentTokensDelete);
    project_access_guard!(auth, project_id, app_state.project_access_checker);
    require_sensitive_action(
        app_state.sensitive_action_authorizer.as_ref(),
        &auth,
        SensitiveAction::DeleteDeploymentToken {
            project_id,
            token_id,
        },
    )
    .await?;

    app_state
        .deployment_token_service
        .delete_token(project_id, token_id)
        .await
        .map_err(|e| {
            error!("Failed to delete deployment token: {}", e);
            e.to_problem()
        })?;

    Ok(StatusCode::NO_CONTENT)
}

/// Rotate a deployment token, invalidating its old secret and issuing a new one
#[utoipa::path(
    tag = "Deployment Tokens",
    post,
    path = "/projects/{project_id}/deployment-tokens/{token_id}/rotate",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("token_id" = i32, Path, description = "Deployment token ID")
    ),
    responses(
        (status = 200, description = "Deployment token rotated successfully; the response contains the new plaintext token, shown only once", body = CreateDeploymentTokenResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Deployment token not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn rotate_deployment_token(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DeploymentTokenAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, token_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentTokensWrite);
    permission_guard!(auth, DeploymentTokensCreate);
    project_access_guard!(auth, project_id, app_state.project_access_checker);
    require_sensitive_action(
        app_state.sensitive_action_authorizer.as_ref(),
        &auth,
        SensitiveAction::RotateDeploymentToken {
            project_id,
            token_id,
        },
    )
    .await?;

    let rotated = app_state
        .deployment_token_service
        .rotate_token(project_id, token_id)
        .await
        .map_err(|e| {
            error!("Failed to rotate deployment token: {}", e);
            e.to_problem()
        })?;

    let audit = DeploymentTokenRotatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        token_id: rotated.id,
        token_name: rotated.name.clone(),
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for deployment token {} rotation: {}",
            token_id, e
        );
    }

    Ok(Json(rotated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_auth::Role;
    use temps_entities::users;

    fn auth_with(role: Role) -> AuthContext {
        let now = chrono::Utc::now();
        let user = users::Model {
            id: 42,
            name: "Test User".to_string(),
            email: "user42@example.com".to_string(),
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
        };
        AuthContext::new_session(user, role)
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn user_is_refused_permissions_its_role_lacks() {
        let user = auth_with(Role::User);
        for requested in [
            &["visitors:enrich"][..],
            &["blob:delete", "kv:delete"],
            &["analytics:read", "visitors:enrich"],
            &["*"],
        ] {
            let problem = enforce_permission_ceiling(&user, &names(requested))
                .expect_err("a user must not exceed its own permissions");
            assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
        }
    }

    #[test]
    fn user_may_grant_what_it_holds() {
        let user = auth_with(Role::User);
        enforce_permission_ceiling(
            &user,
            &names(&[
                "analytics:read",
                "emails:send",
                "blob:write",
                "kv:write",
                "events:write",
            ]),
        )
        .expect("every one of these is within the user role");
    }

    #[test]
    fn admin_may_grant_full_access() {
        let admin = auth_with(Role::Admin);
        enforce_permission_ceiling(&admin, &names(&["*"])).expect("an admin holds everything");
    }

    #[test]
    fn unknown_names_are_left_to_service_validation() {
        let user = auth_with(Role::User);
        enforce_permission_ceiling(&user, &names(&["not-a-permission"]))
            .expect("the service answers 400 for names it cannot parse");
    }

    #[test]
    fn refusal_matches_the_api_key_ceiling_error() {
        let user = auth_with(Role::User);
        let problem = enforce_permission_ceiling(&user, &names(&["visitors:enrich"]))
            .expect_err("a user lacks analytics:write");

        assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
        assert_eq!(
            problem.body.get("error_code"),
            Some(&serde_json::json!("PERMISSION_CEILING_EXCEEDED"))
        );
        assert_eq!(
            problem.body.get("type"),
            Some(&serde_json::json!(
                "https://temps.sh/probs/permission-ceiling-exceeded"
            ))
        );
        let detail = problem
            .body
            .get("detail")
            .and_then(|detail| detail.as_str())
            .expect("the refusal explains itself");
        assert!(
            detail.contains("visitors:enrich") && detail.contains("analytics:write"),
            "the detail names the requested and the missing permission: {detail}"
        );
    }

    #[test]
    fn refusal_names_the_first_permission_that_exceeds_the_caller() {
        let user = auth_with(Role::User);
        let problem = enforce_permission_ceiling(
            &user,
            &names(&["analytics:read", "blob:delete", "visitors:enrich"]),
        )
        .expect_err("blob:delete is beyond a user");

        let detail = problem.body.get("detail").and_then(|d| d.as_str()).unwrap();
        assert!(detail.contains("blob:delete"), "{detail}");
        assert!(!detail.contains("visitors:enrich"), "{detail}");
    }

    #[test]
    fn create_without_permissions_is_held_to_full_access() {
        // An omitted `permissions` field creates a `*` token, so a user, who
        // cannot hold everything, is refused and an admin is not.
        let problem = enforce_create_ceiling(&auth_with(Role::User), None)
            .expect_err("a user cannot create a token with full access by omission");
        assert_eq!(problem.status_code, StatusCode::FORBIDDEN);

        enforce_create_ceiling(&auth_with(Role::Admin), None)
            .expect("an admin holds everything a default token carries");
    }

    #[test]
    fn create_with_an_explicit_list_is_held_to_that_list() {
        let user = auth_with(Role::User);
        enforce_create_ceiling(&user, Some(&names(&["kv:write"])))
            .expect("kv:write is within the user role");
        enforce_create_ceiling(&user, Some(&names(&["kv:delete"]))).expect_err("kv:delete is not");
    }

    #[test]
    fn update_without_permissions_changes_nothing_and_is_not_refused() {
        // Renaming or deactivating a token must keep working for a role that
        // could not mint the token's permissions itself.
        enforce_update_ceiling(&auth_with(Role::User), None)
            .expect("no permissions in the request means nothing new is granted");
    }

    #[test]
    fn update_that_widens_permissions_is_held_to_the_ceiling() {
        let user = auth_with(Role::User);
        enforce_update_ceiling(&user, Some(&names(&["analytics:read"])))
            .expect("analytics:read is within the user role");
        let problem = enforce_update_ceiling(&user, Some(&names(&["visitors:enrich"])))
            .expect_err("widening to visitors:enrich exceeds a user");
        assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
    }
}
