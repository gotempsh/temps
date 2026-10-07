// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `GET /operations` — the feed behind the console's operations tray.
//!
//! Read-only: entries are derived from deployments, restore runs, backups and
//! autofix runs by [`OperationsService`]. There is no write path, so no audit
//! logging.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, AuthContext, Permission, RequireAuth};
use temps_core::problemdetails::{self, Problem, ProblemDetails};
use tracing::error;
use utoipa::{IntoParams, ToSchema};

use crate::services::operations::{
    OperationEntry, OperationKind, OperationSourceAccess, OperationStatusFilter, OperationsError,
    OperationsQuery, OperationsScope, OperationsService,
};

/// State for the operations routes. Kept separate from the projects
/// `AppState` so the feed depends only on what it reads.
pub struct OperationsAppState {
    pub operations_service: Arc<OperationsService>,
    /// Optional team-based project access checker; when present, projects the
    /// caller's teams cannot see are excluded from the feed.
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

pub fn configure_operations_routes() -> Router<Arc<OperationsAppState>> {
    Router::new().route("/operations", get(list_operations))
}

/// Query parameters for `GET /operations`.
#[derive(Debug, Clone, Default, Deserialize, ToSchema, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListOperationsQuery {
    /// Page number, 1-based (default 1).
    #[param(minimum = 1, example = 1)]
    pub page: Option<u64>,
    /// Items per page (default 20, max 100).
    #[param(minimum = 1, maximum = 100, example = 20)]
    pub page_size: Option<u64>,
    /// `running` (queued, running, waiting), `finished` (succeeded, failed,
    /// cancelled) or `all` (default).
    pub status: Option<OperationStatusFilter>,
    /// Only operations of this kind.
    pub kind: Option<OperationKind>,
    /// Only operations belonging to this project.
    pub project_id: Option<i32>,
}

/// A page of the operations feed. Sorted by `created_at` descending (fixed).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OperationsListResponse {
    pub operations: Vec<OperationEntry>,
    /// Operations matching every filter.
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
    /// Queued, running and waiting operations matching the scope, `kind` and
    /// `project_id` filters — independent of the `status` filter, so the tray
    /// badge stays correct while browsing finished operations.
    pub running_count: u64,
}

impl From<OperationsError> for Problem {
    fn from(error: OperationsError) -> Self {
        match error {
            OperationsError::Validation { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Operations Query")
                .with_detail(error.to_string()),
            OperationsError::Database { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Operations Unavailable")
                    .with_detail(error.to_string())
            }
        }
    }
}

/// Which rows this caller may see. Instance administrators see everything;
/// a deployment token sees only its own project; everyone else sees every
/// project their teams don't hide.
fn operations_scope(
    auth: &AuthContext,
    hidden_project_ids: Vec<i32>,
) -> Result<OperationsScope, Problem> {
    if auth.is_deployment_token() {
        return match auth.project_id() {
            Some(project_id) => Ok(OperationsScope::SingleProject { project_id }),
            None => Err(problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Project Access Denied")
                .with_detail("Deployment token is not bound to a project")),
        };
    }
    if auth.is_instance_admin() {
        return Ok(OperationsScope::Instance);
    }
    Ok(OperationsScope::Projects { hidden_project_ids })
}

/// Sources the caller holds the read permission for. A source the caller
/// can't read is left out of the union rather than failing the request.
fn source_access(auth: &AuthContext) -> OperationSourceAccess {
    OperationSourceAccess {
        deployments: auth.has_permission(&Permission::DeploymentsRead),
        backups: auth.has_permission(&Permission::BackupsRead),
        autofix: auth.has_permission(&Permission::ProjectsRead),
    }
}

/// List recent and in-flight operations
///
/// Deployments, rollbacks, promotions, restores, backups and autofix runs the
/// caller can see, newest first (`created_at` DESC, fixed). Finished
/// operations older than 7 days are omitted; operations still in flight are
/// always included. Redeploys are reported as `deployment` — no column
/// distinguishes them.
#[utoipa::path(
    tag = "Operations",
    get,
    path = "/operations",
    operation_id = "list_operations",
    params(ListOperationsQuery),
    responses(
        (status = 200, description = "A page of operations", body = OperationsListResponse),
        (status = 400, description = "Invalid query", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_operations(
    State(state): State<Arc<OperationsAppState>>,
    RequireAuth(auth): RequireAuth,
    Query(params): Query<ListOperationsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);

    let query = OperationsQuery::normalize(
        params.page,
        params.page_size,
        params.status,
        params.kind,
        params.project_id,
    );
    let hidden = super::handlers::hidden_project_ids_for_caller(
        state.project_access_checker.as_ref(),
        &auth,
    )
    .await?;
    let scope = operations_scope(&auth, hidden)?;

    let page = state
        .operations_service
        .list_operations(&query, &scope, source_access(&auth))
        .await
        .map_err(|e| {
            error!(
                user_id = auth.user_id(),
                page = query.page,
                error = %e,
                "Failed to list operations"
            );
            Problem::from(e)
        })?;

    Ok(Json(OperationsListResponse {
        operations: page.operations,
        total: page.total,
        page: query.page,
        page_size: query.page_size,
        running_count: page.running_count,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_auth::Role;
    use temps_entities::deployment_tokens::DeploymentTokenPermission;
    use temps_entities::users;
    use tower::ServiceExt;

    fn test_user() -> users::Model {
        let now = Utc::now();
        users::Model {
            id: 7,
            name: "Operator".to_string(),
            email: "operator@example.com".to_string(),
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
        }
    }

    fn app_state() -> Arc<OperationsAppState> {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        Arc::new(OperationsAppState {
            operations_service: Arc::new(OperationsService::new(db)),
            project_access_checker: None,
        })
    }

    async fn rejection_status(auth: AuthContext) -> Option<StatusCode> {
        list_operations(
            State(app_state()),
            RequireAuth(auth),
            Query(ListOperationsQuery::default()),
        )
        .await
        .err()
        .map(|problem| problem.status_code)
    }

    #[tokio::test]
    async fn unauthenticated_request_is_rejected_with_401() {
        let router = configure_operations_routes().with_state(app_state());
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/operations")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn key_without_projects_read_is_rejected_with_403() {
        let auth = AuthContext::new_api_key(
            test_user(),
            None,
            Some(vec![Permission::AnalyticsRead]),
            "analytics-only".to_string(),
            1,
        );
        assert_eq!(rejection_status(auth).await, Some(StatusCode::FORBIDDEN));
    }

    #[tokio::test]
    async fn deployment_token_is_rejected_with_403() {
        let auth = AuthContext::new_deployment_token(
            3,
            None,
            None,
            1,
            "ci".to_string(),
            vec![DeploymentTokenPermission::FullAccess],
        );
        assert_eq!(rejection_status(auth).await, Some(StatusCode::FORBIDDEN));
    }

    #[tokio::test]
    async fn reader_passes_the_guard_and_reaches_the_service() {
        // The empty mock DB makes the service fail with a database error,
        // which proves the request got past authorization.
        let auth = AuthContext::new_session(test_user(), Role::Reader);
        assert_eq!(
            rejection_status(auth).await,
            Some(StatusCode::INTERNAL_SERVER_ERROR)
        );
    }

    #[test]
    fn scope_follows_the_principal() {
        let admin = AuthContext::new_session(test_user(), Role::Admin);
        assert_eq!(
            operations_scope(&admin, vec![4]).ok(),
            Some(OperationsScope::Instance)
        );

        let user = AuthContext::new_session(test_user(), Role::User);
        assert_eq!(
            operations_scope(&user, vec![4, 5]).ok(),
            Some(OperationsScope::Projects {
                hidden_project_ids: vec![4, 5]
            })
        );

        let token = AuthContext::new_deployment_token(
            9,
            None,
            None,
            1,
            "ci".to_string(),
            vec![DeploymentTokenPermission::FullAccess],
        );
        assert_eq!(
            operations_scope(&token, Vec::new()).ok(),
            Some(OperationsScope::SingleProject { project_id: 9 })
        );
    }

    #[test]
    fn source_access_tracks_per_source_read_permissions() {
        let key = AuthContext::new_api_key(
            test_user(),
            None,
            Some(vec![Permission::ProjectsRead, Permission::DeploymentsRead]),
            "deploys".to_string(),
            1,
        );
        assert_eq!(
            source_access(&key),
            OperationSourceAccess {
                deployments: true,
                backups: false,
                autofix: true,
            }
        );
    }

    #[test]
    fn errors_map_to_problem_statuses() {
        let validation: Problem = OperationsError::Validation {
            message: "page 9 is out of range".to_string(),
        }
        .into();
        assert_eq!(validation.status_code, StatusCode::BAD_REQUEST);

        let database: Problem = OperationsError::Database {
            operation: "count operations",
            source: sea_orm::DbErr::Custom("connection reset".to_string()),
        }
        .into();
        assert_eq!(database.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }
}
