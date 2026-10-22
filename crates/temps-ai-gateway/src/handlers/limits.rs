// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Admin API for the AI gateway's per-project rate limit and monthly budget.
//!
//! Limits apply to deployment-token callers (the `TEMPS_API_TOKEN` every
//! deployed container receives). A project's own limits override the
//! instance-wide defaults, which override the built-in default. In every
//! request/response a `null` limit inherits and `0` means unlimited.

use anyhow::Result as AnyhowResult;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::RequireAuth;
use temps_auth::{permission_guard, project_access_guard, project_scope_guard};
use temps_core::problemdetails::{Problem, ProblemDetails};
use temps_core::{AuditContext, AuditOperation, RequestMetadata};
use tracing::error;
use utoipa::{OpenApi, ToSchema};

use crate::handlers::types::AiGatewayAppState;
use crate::services::limits_service::{
    EffectiveLimits, LimitScope, LimitSettings, DEFAULT_MAX_REQUESTS_PER_MINUTE,
};

// ============================================================================
// OpenAPI schema
// ============================================================================

#[derive(OpenApi)]
#[openapi(
    paths(
        get_limits_overview,
        update_instance_limits,
        get_project_limits,
        update_project_limits,
        reset_project_limits,
    ),
    components(schemas(
        LimitSettingsBody,
        LimitsOverviewResponse,
        ProjectLimitOverride,
        ProjectLimitsResponse,
        EffectiveLimits,
        crate::services::limits_service::LimitSource,
    )),
    info(
        title = "AI Gateway Limits API",
        description = "Per-project rate limits and monthly budgets for deployment-token callers",
        version = "1.0.0"
    ),
    tags(
        (name = "AI Gateway Limits", description = "Rate limits and spend caps for AI gateway callers")
    )
)]
pub struct AiGatewayLimitsApiDoc;

pub fn configure_limits_routes() -> Router<Arc<AiGatewayAppState>> {
    Router::new()
        .route("/ai/limits", get(get_limits_overview))
        .route(
            "/ai/limits/instance",
            axum::routing::put(update_instance_limits),
        )
        .route(
            "/ai/limits/projects/{project_id}",
            get(get_project_limits)
                .put(update_project_limits)
                .delete(reset_project_limits),
        )
}

// ============================================================================
// DTOs
// ============================================================================

/// Limits as stored for one scope. `null` inherits from the next level
/// (project → instance → built-in); `0` means unlimited.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
pub struct LimitSettingsBody {
    /// Requests per rolling minute
    pub max_requests_per_minute: Option<i64>,
    /// Operator-funded spend per UTC calendar month, in microcents
    /// (1/10000 of a cent; 1 USD = 1,000,000 microcents)
    pub max_cost_per_month_microcents: Option<i64>,
}

impl From<LimitSettings> for LimitSettingsBody {
    fn from(settings: LimitSettings) -> Self {
        Self {
            max_requests_per_minute: settings.max_requests_per_minute,
            max_cost_per_month_microcents: settings.max_cost_per_month_microcents,
        }
    }
}

impl From<LimitSettingsBody> for LimitSettings {
    fn from(body: LimitSettingsBody) -> Self {
        Self {
            max_requests_per_minute: body.max_requests_per_minute,
            max_cost_per_month_microcents: body.max_cost_per_month_microcents,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProjectLimitOverride {
    pub project_id: i32,
    pub limits: LimitSettingsBody,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LimitsOverviewResponse {
    /// Instance-wide limits as stored (what the admin set)
    pub instance: LimitSettingsBody,
    /// Requests per minute applied to a project when nothing is configured
    pub built_in_max_requests_per_minute: u32,
    /// Projects that override the instance defaults
    pub project_overrides: Vec<ProjectLimitOverride>,
    /// Total number of projects with overrides, across all pages
    pub total_project_overrides: u64,
    pub page: u64,
    pub page_size: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProjectLimitsResponse {
    pub project_id: i32,
    /// This project's own limits as stored; `null` fields inherit
    pub stored: LimitSettingsBody,
    /// Limits actually enforced after inheritance
    pub effective: EffectiveLimits,
    /// Operator-funded spend so far this UTC month, in microcents
    pub month_spend_microcents: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct OverviewQuery {
    /// Page of project overrides (default 1)
    pub page: Option<u64>,
    /// Page size (default 20, max 100)
    pub page_size: Option<u64>,
}

// ============================================================================
// Audit
// ============================================================================

#[derive(Debug, Clone, Serialize)]
struct AiGatewayLimitsUpdatedAudit {
    context: AuditContext,
    scope: String,
    max_requests_per_minute: Option<i64>,
    max_cost_per_month_microcents: Option<i64>,
}

impl AuditOperation for AiGatewayLimitsUpdatedAudit {
    fn operation_type(&self) -> String {
        "ai_gateway.limits.updated".to_string()
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

    fn serialize(&self) -> AnyhowResult<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {e}"))
    }
}

async fn audit_limits_change(
    app_state: &AiGatewayAppState,
    auth: &temps_auth::AuthContext,
    metadata: &RequestMetadata,
    scope: String,
    settings: LimitSettings,
) {
    let audit = AiGatewayLimitsUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        scope: scope.clone(),
        max_requests_per_minute: settings.max_requests_per_minute,
        max_cost_per_month_microcents: settings.max_cost_per_month_microcents,
    };
    if let Err(audit_error) = app_state.audit_service.create_audit_log(&audit).await {
        error!(scope, error = %audit_error, "Failed to create audit log for AI gateway limits change");
    }
}

// ============================================================================
// Handlers
// ============================================================================

/// Limits and spend are an operator view over every project. `AiGatewayRead`
/// is held by ordinary users and readers, and would expose other projects'
/// limits and month-to-date spend, so reads require an administrator.
fn require_limits_reader(auth: &temps_auth::AuthContext) -> Result<(), Problem> {
    permission_guard!(auth, SystemAdmin);
    Ok(())
}

/// Limit writers need the same operator standing as readers. `AiGatewayWrite`
/// alone is grantable to a custom role or API key that is meant to be confined
/// to one project, and a limit write on any project (or the instance default
/// every project inherits) would let it raise its own spend cap or zero
/// another project's.
fn require_limits_writer(auth: &temps_auth::AuthContext) -> Result<(), Problem> {
    permission_guard!(auth, AiGatewayWrite);
    permission_guard!(auth, SystemAdmin);
    Ok(())
}

#[utoipa::path(
    tag = "AI Gateway Limits",
    get,
    path = "/ai/limits",
    params(
        ("page" = Option<u64>, Query, description = "Page of project overrides (default 1)"),
        ("page_size" = Option<u64>, Query, description = "Page size (default 20, max 100)"),
    ),
    responses(
        (status = 200, description = "Instance defaults and per-project overrides", body = LimitsOverviewResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn get_limits_overview(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Query(query): Query<OverviewQuery>,
) -> Result<impl IntoResponse, Problem> {
    require_limits_reader(&auth)?;

    let page = query.page.unwrap_or(1).max(1);
    let page_size = query.page_size.unwrap_or(20).clamp(1, 100);
    let instance = app_state
        .limits_service
        .get_settings(LimitScope::Instance)
        .await?
        .unwrap_or_default();
    let (overrides, total) = app_state
        .limits_service
        .list_project_overrides(page, page_size)
        .await?;

    Ok(Json(LimitsOverviewResponse {
        instance: instance.into(),
        built_in_max_requests_per_minute: DEFAULT_MAX_REQUESTS_PER_MINUTE,
        project_overrides: overrides
            .into_iter()
            .map(|(project_id, settings)| ProjectLimitOverride {
                project_id,
                limits: settings.into(),
            })
            .collect(),
        total_project_overrides: total,
        page,
        page_size,
    }))
}

#[utoipa::path(
    tag = "AI Gateway Limits",
    put,
    path = "/ai/limits/instance",
    request_body = LimitSettingsBody,
    responses(
        (status = 200, description = "Instance-wide defaults replaced", body = LimitSettingsBody),
        (status = 400, description = "Invalid limit", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn update_instance_limits(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    axum::extract::Extension(metadata): axum::extract::Extension<RequestMetadata>,
    Json(body): Json<LimitSettingsBody>,
) -> Result<impl IntoResponse, Problem> {
    require_limits_writer(&auth)?;

    let saved = app_state
        .limits_service
        .set_settings(LimitScope::Instance, body.into())
        .await?;
    audit_limits_change(&app_state, &auth, &metadata, "instance".to_string(), saved).await;
    Ok(Json(LimitSettingsBody::from(saved)))
}

async fn project_limits_response(
    app_state: &AiGatewayAppState,
    project_id: i32,
) -> Result<ProjectLimitsResponse, Problem> {
    let stored = app_state
        .limits_service
        .get_settings(LimitScope::Project(project_id))
        .await?
        .unwrap_or_default();
    let effective = app_state
        .limits_service
        .effective_limits(project_id)
        .await?;
    let month_spend_microcents = app_state.limits_service.month_spend(project_id).await?;
    Ok(ProjectLimitsResponse {
        project_id,
        stored: stored.into(),
        effective,
        month_spend_microcents,
    })
}

#[utoipa::path(
    tag = "AI Gateway Limits",
    get,
    path = "/ai/limits/projects/{project_id}",
    params(("project_id" = i32, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Stored and effective limits plus this month's spend", body = ProjectLimitsResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn get_project_limits(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    require_limits_reader(&auth)?;

    Ok(Json(project_limits_response(&app_state, project_id).await?))
}

#[utoipa::path(
    tag = "AI Gateway Limits",
    put,
    path = "/ai/limits/projects/{project_id}",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = LimitSettingsBody,
    responses(
        (status = 200, description = "Project limits replaced, as stored", body = LimitSettingsBody),
        (status = 400, description = "Invalid limit", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project not found", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn update_project_limits(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    axum::extract::Extension(metadata): axum::extract::Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(body): Json<LimitSettingsBody>,
) -> Result<impl IntoResponse, Problem> {
    require_limits_writer(&auth)?;
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    let saved = app_state
        .limits_service
        .set_settings(LimitScope::Project(project_id), body.into())
        .await?;
    audit_limits_change(
        &app_state,
        &auth,
        &metadata,
        format!("project:{project_id}"),
        saved,
    )
    .await;
    // Only the saved settings: the month-to-date spend lives in
    // `ProjectLimitsResponse`, served by the read endpoint.
    Ok(Json(LimitSettingsBody::from(saved)))
}

#[utoipa::path(
    tag = "AI Gateway Limits",
    delete,
    path = "/ai/limits/projects/{project_id}",
    params(("project_id" = i32, Path, description = "Project ID")),
    responses(
        (status = 204, description = "Project limits cleared; the project inherits the instance defaults"),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project not found", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn reset_project_limits(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    axum::extract::Extension(metadata): axum::extract::Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    require_limits_writer(&auth)?;
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    let cleared = app_state
        .limits_service
        .set_settings(LimitScope::Project(project_id), LimitSettings::default())
        .await?;
    audit_limits_change(
        &app_state,
        &auth,
        &metadata,
        format!("project:{project_id}"),
        cleared,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use temps_auth::permissions::Role;
    use temps_auth::AuthContext;
    use temps_entities::users;

    fn test_auth(role: Role) -> AuthContext {
        let now = chrono::Utc::now();
        AuthContext::new_session(
            users::Model {
                id: 7,
                name: "Limits Tester".to_string(),
                email: "limits-tester@example.com".to_string(),
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
            },
            role,
        )
    }

    #[test]
    fn administrators_may_read_limits() {
        assert!(require_limits_reader(&test_auth(Role::Admin)).is_ok());
    }

    #[test]
    fn administrators_may_write_limits() {
        assert!(require_limits_writer(&test_auth(Role::Admin)).is_ok());
    }

    #[test]
    fn non_administrators_cannot_write_limits() {
        for role in [Role::User, Role::Reader] {
            let err = require_limits_writer(&test_auth(role))
                .expect_err("limit writes span every project");
            assert_eq!(err.status_code, StatusCode::FORBIDDEN);
        }
    }

    /// F8: a custom grant of `ai_gateway:write` without `system:admin` (the
    /// shape of a project-confined custom role or API key) must not be able to
    /// rewrite any project's limits.
    #[test]
    fn custom_ai_gateway_write_without_system_admin_cannot_write_limits() {
        let mut auth = test_auth(Role::Reader);
        auth.custom_permissions = Some(vec![temps_auth::permissions::Permission::AiGatewayWrite]);
        assert!(auth.has_permission(&temps_auth::permissions::Permission::AiGatewayWrite));
        let err = require_limits_writer(&auth)
            .expect_err("ai_gateway:write alone is not an operator grant");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    #[test]
    fn ordinary_users_and_readers_cannot_read_other_projects_limits() {
        for role in [Role::User, Role::Reader] {
            let err = require_limits_reader(&test_auth(role))
                .expect_err("limits and spend span every project");
            assert_eq!(err.status_code, StatusCode::FORBIDDEN);
        }
    }
}
