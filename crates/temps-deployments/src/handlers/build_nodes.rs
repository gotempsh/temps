// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::services::build_node_policy::{
    BuildNodePolicyError, BuildNodePolicyResponse, BuildNodePolicyService, BuildNodePolicySource,
    SetBuildNodesRequest,
};
use axum::{
    extract::{Path, State},
    routing::get,
    Extension, Json, Router,
};
use serde::Serialize;
use std::sync::Arc;
use temps_auth::{
    deny_deployment_token, permission_guard, project_permission_guard, project_scope_guard,
    RequireAuth,
};
use temps_core::problemdetails::{self, Problem, ProblemDetails};
use temps_core::{AuditContext, AuditLogger, AuditOperation, RequestMetadata};

pub struct BuildNodeState {
    pub service: Arc<BuildNodePolicyService>,
    pub audit: Arc<dyn AuditLogger>,
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

impl From<BuildNodePolicyError> for Problem {
    fn from(error: BuildNodePolicyError) -> Self {
        use axum::http::StatusCode;
        match &error {
            BuildNodePolicyError::ProjectNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Project not found")
                    .with_detail(error.to_string())
            }
            BuildNodePolicyError::Validation { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid builder nodes")
                .with_detail(error.to_string()),
            BuildNodePolicyError::Database { .. } => {
                tracing::error!(%error, "Builder-node policy database operation failed");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Builder-node policy unavailable")
                    .with_detail("Could not read or save builder-node configuration. No fallback was applied.")
            }
        }
    }
}

#[derive(Debug, Serialize)]
struct BuildNodesUpdatedAudit {
    context: AuditContext,
    project_id: Option<i32>,
    node_ids: Option<Vec<i32>>,
}
impl AuditOperation for BuildNodesUpdatedAudit {
    fn operation_type(&self) -> String {
        "BUILD_NODES_UPDATED".into()
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
        Ok(serde_json::to_string(self)?)
    }
}

pub fn configure_routes() -> Router<Arc<BuildNodeState>> {
    Router::new()
        .route(
            "/settings/build-nodes",
            get(get_global_build_nodes).put(set_global_build_nodes),
        )
        .route(
            "/projects/{project_id}/build-nodes",
            get(get_project_build_nodes).put(set_project_build_nodes),
        )
}

/// Read the global default for source-image builds. A null selection preserves
/// automatic placement: local builds in the full profile, workers otherwise.
#[utoipa::path(
    get, tag = "Build Nodes", path = "/settings/build-nodes",
    responses(
        (status = 200, description = "Configured and effective builder selection", body = BuildNodePolicyResponse),
        (status = 400, description = "Invalid builder selection", body = ProblemDetails),
        (status = 401, description = "Authentication required", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project not found", body = ProblemDetails),
        (status = 500, description = "Policy storage unavailable", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_global_build_nodes(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<BuildNodeState>>,
) -> Result<Json<BuildNodePolicyResponse>, Problem> {
    permission_guard!(auth, SettingsRead);
    deny_deployment_token!(auth);

    Ok(Json(state.service.get(None).await?))
}

/// Set an ordered, exclusive default worker pool for future source-image builds.
/// Explicit pools override local building even in the full profile. The first
/// active compatible worker is selected; there is no fallback outside the pool.
/// null restores automatic selection. Does not change worker labels, restart
/// agents, move application replicas, or affect already-running builds.
#[utoipa::path(
    put, tag = "Build Nodes", path = "/settings/build-nodes",
    request_body = SetBuildNodesRequest,
    responses(
        (status = 200, description = "Configured and effective builder selection", body = BuildNodePolicyResponse),
        (status = 400, description = "Invalid builder selection", body = ProblemDetails),
        (status = 401, description = "Authentication required", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project not found", body = ProblemDetails),
        (status = 500, description = "Policy storage unavailable", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn set_global_build_nodes(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<BuildNodeState>>,

    Extension(metadata): Extension<RequestMetadata>,
    request: Result<Json<SetBuildNodesRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<BuildNodePolicyResponse>, Problem> {
    permission_guard!(auth, SettingsWrite);
    deny_deployment_token!(auth);
    let Json(request) = request.map_err(invalid_request)?;
    let result = state.service.set(None, request.node_ids.clone()).await?;
    let audit = BuildNodesUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.to_string()),
            user_agent: metadata.user_agent,
        },
        project_id: None,
        node_ids: request.node_ids,
    };
    if let Err(error) = state.audit.create_audit_log(&audit).await {
        tracing::error!(%error, "Failed to audit builder-node policy update");
    }
    Ok(Json(result))
}

/// Read this project's override and its effective selection after inheritance.
#[utoipa::path(
    get, tag = "Build Nodes", path = "/projects/{project_id}/build-nodes",
    params(("project_id" = i32, Path, description = "Project ID")),

    responses(
        (status = 200, description = "Configured and effective builder selection", body = BuildNodePolicyResponse),
        (status = 400, description = "Invalid builder selection", body = ProblemDetails),
        (status = 401, description = "Authentication required", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project not found", body = ProblemDetails),
        (status = 500, description = "Policy storage unavailable", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_project_build_nodes(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<BuildNodeState>>,
    Path(project_id): Path<i32>,
) -> Result<Json<BuildNodePolicyResponse>, Problem> {
    project_permission_guard!(auth, ProjectsRead, project_id, state.project_access_checker);
    project_scope_guard!(auth, project_id);
    Ok(Json(state.service.get(Some(project_id)).await?))
}

/// Override the global default for this project's future source-image builds.
/// One ID pins builds to that worker; multiple IDs are tried in configured
/// priority order for availability and architecture, not retried after a build
/// starts. null clears the override and inherits the global default. Ordinary
/// workers and build-only workers are accepted; application placement is unchanged.
#[utoipa::path(
    put, tag = "Build Nodes", path = "/projects/{project_id}/build-nodes",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = SetBuildNodesRequest,
    responses(
        (status = 200, description = "Configured and effective builder selection", body = BuildNodePolicyResponse),
        (status = 400, description = "Invalid builder selection", body = ProblemDetails),
        (status = 401, description = "Authentication required", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project not found", body = ProblemDetails),
        (status = 500, description = "Policy storage unavailable", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn set_project_build_nodes(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<BuildNodeState>>,
    Path(project_id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    request: Result<Json<SetBuildNodesRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<BuildNodePolicyResponse>, Problem> {
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);
    let Json(request) = request.map_err(invalid_request)?;
    let result = state
        .service
        .set(Some(project_id), request.node_ids.clone())
        .await?;
    let audit = BuildNodesUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.to_string()),
            user_agent: metadata.user_agent,
        },
        project_id: Some(project_id),
        node_ids: request.node_ids,
    };
    if let Err(error) = state.audit.create_audit_log(&audit).await {
        tracing::error!(%error, "Failed to audit builder-node policy update");
    }
    Ok(Json(result))
}

#[derive(utoipa::OpenApi)]
#[openapi(
    paths(
        get_global_build_nodes,
        set_global_build_nodes,
        get_project_build_nodes,
        set_project_build_nodes
    ),
    components(schemas(SetBuildNodesRequest, BuildNodePolicyResponse, BuildNodePolicySource))
)]
pub struct BuildNodesApiDoc;

fn invalid_request(error: axum::extract::rejection::JsonRejection) -> Problem {
    problemdetails::new(axum::http::StatusCode::BAD_REQUEST)
        .with_title("Invalid builder-node request")
        .with_detail(error.body_text())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use temps_auth::{AuthContext, Role};
    use tower::ServiceExt;

    #[derive(Default)]
    struct Audit(std::sync::Mutex<Vec<String>>);
    #[async_trait::async_trait]
    impl AuditLogger for Audit {
        async fn create_audit_log(&self, operation: &dyn AuditOperation) -> anyhow::Result<()> {
            self.0.lock().unwrap().push(operation.serialize()?);
            Ok(())
        }
    }

    fn auth() -> AuthContext {
        let now = chrono::Utc::now();
        let user = serde_json::from_value(serde_json::json!({
            "id": 1, "name": "Test Operator", "email": "operator@example.test",
            "email_verified": true, "must_change_password": false, "mfa_enabled": false,
            "created_at": now, "updated_at": now
        }))
        .unwrap();
        AuthContext::new_session(user, Role::Admin)
    }

    fn app(db: sea_orm::DatabaseConnection, audit: Arc<Audit>) -> Router {
        app_with_checker(db, audit, None)
    }

    fn app_with_checker(
        db: sea_orm::DatabaseConnection,
        audit: Arc<Audit>,
        checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    ) -> Router {
        configure_routes()
            .with_state(Arc::new(BuildNodeState {
                service: Arc::new(BuildNodePolicyService::new(Arc::new(db))),
                audit,
                project_access_checker: checker,
            }))
            .layer(Extension(RequestMetadata {
                ip_address: "127.0.0.1".into(),
                user_agent: "test".into(),
                headers: Default::default(),
                visitor_id_cookie: None,
                session_id_cookie: None,
                base_url: "http://example.test".into(),
                scheme: "http".into(),
                host: "example.test".into(),
                is_secure: false,
            }))
    }

    #[tokio::test]
    async fn all_policy_handlers_require_auth_and_permissions() {
        for path in ["/settings/build-nodes", "/projects/7/build-nodes"] {
            for method in ["GET", "PUT"] {
                for authenticated in [false, true] {
                    let mut router = app(
                        MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
                        Arc::new(Audit::default()),
                    );
                    if authenticated {
                        let mut denied = auth();
                        denied.custom_permissions = Some(vec![]);
                        router = router.layer(Extension(denied));
                    }
                    let response = router
                        .oneshot(
                            Request::builder()
                                .method(method)
                                .uri(path)
                                .header("content-type", "application/json")
                                .body(Body::from(r#"{"node_ids":null}"#))
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    assert_eq!(
                        response.status(),
                        if authenticated {
                            StatusCode::FORBIDDEN
                        } else {
                            StatusCode::UNAUTHORIZED
                        }
                    );
                    assert_eq!(
                        response.headers()["content-type"],
                        "application/problem+json"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn global_get_returns_effective_selection_and_put_is_audited() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results(vec![
                Vec::<temps_entities::build_node_policies::Model>::new(),
            ])
            .into_connection();
        let audit = Arc::new(Audit::default());
        let response = app(db, audit.clone())
            .layer(Extension(auth()))
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/settings/build-nodes")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"node_ids":null}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body: BuildNodePolicyResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body.source, BuildNodePolicySource::Automatic);
        assert_eq!(audit.0.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn invalid_selection_is_typed_and_does_not_audit_a_write() {
        let audit = Arc::new(Audit::default());
        let response = app(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
            audit.clone(),
        )
        .layer(Extension(auth()))
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/settings/build-nodes")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"node_ids":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response.headers()["content-type"],
            "application/problem+json"
        );
        assert!(audit.0.lock().unwrap().is_empty());
    }

    struct ReadOnlyProject;
    #[async_trait::async_trait]
    impl temps_core::ProjectAccessChecker for ReadOnlyProject {
        async fn user_can_access_project(
            &self,
            _: i32,
            _: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(true)
        }

        async fn effective_project_permissions(
            &self,
            _: i32,
            _: i32,
        ) -> Result<Option<Vec<String>>, Box<dyn std::error::Error + Send + Sync>> {
            Ok(Some(vec!["projects:read".into()]))
        }
    }

    #[tokio::test]
    async fn project_viewer_cannot_change_builders_despite_instance_write_permission() {
        let audit = Arc::new(Audit::default());
        let mut member = auth();
        member.effective_role = Role::Custom;
        member.custom_permissions = Some(vec![temps_auth::Permission::ProjectsWrite]);
        let router = app_with_checker(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
            audit.clone(),
            Some(Arc::new(ReadOnlyProject)),
        )
        .layer(Extension(member));
        let response = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/projects/7/build-nodes")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"node_ids":null}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(audit.0.lock().unwrap().is_empty());
        // The empty mock has no query results: any policy access would panic.
    }

    struct DenyProject;
    #[async_trait::async_trait]
    impl temps_core::ProjectAccessChecker for DenyProject {
        async fn user_can_access_project(
            &self,
            _: i32,
            _: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(false)
        }
    }

    #[tokio::test]
    async fn project_membership_denial_happens_before_policy_access() {
        let state = Arc::new(BuildNodeState {
            service: Arc::new(BuildNodePolicyService::new(Arc::new(
                MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
            ))),
            audit: Arc::new(Audit::default()),
            project_access_checker: Some(Arc::new(DenyProject)),
        });
        let mut member = auth();
        member.effective_role = Role::Custom;
        member.custom_permissions = Some(vec![temps_auth::Permission::ProjectsRead]);
        let response = configure_routes()
            .with_state(state)
            .layer(Extension(member))
            .oneshot(
                Request::builder()
                    .uri("/projects/7/build-nodes")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn missing_node_ids_is_a_typed_bad_request_not_an_implicit_reset() {
        let response = app(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
            Arc::new(Audit::default()),
        )
        .layer(Extension(auth()))
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/settings/build-nodes")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response.headers()["content-type"],
            "application/problem+json"
        );
    }
}
