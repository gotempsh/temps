// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP surface for per-project / per-environment IP allow/deny lists.
//!
//! The same four operations exist at project scope
//! (`/projects/{project_id}/ip-access`) and environment scope
//! (`/projects/{project_id}/environments/{environment_id}/ip-access`).
//! Handlers only authorise, call the service and audit; validation,
//! canonicalisation and the lock-out check live in the service.

use crate::ip_access::IpAccessMode;
use crate::service::project_ip_access_service::{
    PolicySource, ProjectIpAccessError, ProjectIpAccessResponse, ProjectIpAccessService,
    SetProjectIpAccessRequest, TestProjectIpAccessRequest, TestProjectIpAccessResponse,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use serde::Serialize;
use std::net::IpAddr;
use std::sync::Arc;
use temps_auth::{project_permission_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem, ProblemDetails};
use temps_core::{AuditLogger, AuditOperation, RequestMetadata};
use temps_entities::project_ip_access_rules::IpAccessCidrEntry;
use tracing::error;

pub struct ProjectIpAccessState {
    pub service: Arc<ProjectIpAccessService>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

impl From<ProjectIpAccessError> for Problem {
    fn from(error: ProjectIpAccessError) -> Self {
        match error {
            ProjectIpAccessError::ProjectNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Project Not Found")
                    .with_detail(error.to_string())
            }
            ProjectIpAccessError::EnvironmentNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Environment Not Found")
                    .with_detail(error.to_string())
            }
            ProjectIpAccessError::PolicyNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("IP Access Policy Not Found")
                    .with_detail(error.to_string())
            }
            ProjectIpAccessError::InvalidCidr { .. }
            | ProjectIpAccessError::DuplicateCidr { .. }
            | ProjectIpAccessError::TooManyCidrs { .. }
            | ProjectIpAccessError::DescriptionTooLong { .. }
            | ProjectIpAccessError::EmptyAllowList { .. }
            | ProjectIpAccessError::InvalidIpAddress { .. }
            | ProjectIpAccessError::IncompleteDraft { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid IP Access Policy")
                    .with_detail(error.to_string())
            }
            ProjectIpAccessError::ConcurrentUpdate { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("IP Access Policy Changed")
                    .with_detail(error.to_string())
            }
            ProjectIpAccessError::WouldLockOutCaller { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Policy Would Lock You Out")
                    .with_detail(error.to_string())
            }
            ProjectIpAccessError::Database(_) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

/// A stored policy plus the address the console saw for the caller, so the
/// UI can offer "add my IP" and warn before a lock-out.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ProjectIpAccessDetail {
    #[serde(flatten)]
    pub policy: ProjectIpAccessResponse,
    /// The address this request arrived from, as the console resolved it.
    /// It is the address the project will see only if the console and the
    /// project are reached over the same network path.
    #[schema(example = "203.0.113.9")]
    pub your_ip: Option<String>,
}

fn caller_ip(metadata: &RequestMetadata) -> Option<IpAddr> {
    metadata.ip_address.trim().parse::<IpAddr>().ok()
}

fn detail(policy: ProjectIpAccessResponse, metadata: &RequestMetadata) -> ProjectIpAccessDetail {
    ProjectIpAccessDetail {
        policy,
        your_ip: caller_ip(metadata)
            .map(crate::service::project_ip_access_service::normalize_ip)
            .map(|ip| ip.to_string()),
    }
}

// ---- audit -------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct IpAccessAuditContext {
    user_id: Option<i32>,
    ip_address: Option<String>,
    user_agent: String,
}

#[derive(Debug, Clone, Serialize)]
struct ProjectIpAccessUpdatedAudit {
    context: IpAccessAuditContext,
    project_id: i32,
    environment_id: Option<i32>,
    mode: IpAccessMode,
    cidr_count: usize,
    cidrs: Vec<IpAccessCidrEntry>,
    acknowledged_lockout: bool,
}

#[derive(Debug, Clone, Serialize)]
struct ProjectIpAccessDeletedAudit {
    context: IpAccessAuditContext,
    project_id: i32,
    environment_id: Option<i32>,
}

macro_rules! impl_audit_operation {
    ($ty:ty, $name:literal) => {
        impl AuditOperation for $ty {
            fn operation_type(&self) -> String {
                $name.to_string()
            }

            fn user_id(&self) -> Option<i32> {
                self.context.user_id
            }

            fn ip_address(&self) -> Option<String> {
                self.context.ip_address.clone()
            }

            fn user_agent(&self) -> &str {
                &self.context.user_agent
            }

            fn serialize(&self) -> anyhow::Result<String> {
                serde_json::to_string(self).map_err(|e| {
                    anyhow::anyhow!("Failed to serialize audit operation {}: {}", $name, e)
                })
            }
        }
    };
}

impl_audit_operation!(ProjectIpAccessUpdatedAudit, "PROJECT_IP_ACCESS_UPDATED");
impl_audit_operation!(ProjectIpAccessDeletedAudit, "PROJECT_IP_ACCESS_DELETED");

fn audit_context(user_id: Option<i32>, metadata: &RequestMetadata) -> IpAccessAuditContext {
    IpAccessAuditContext {
        user_id,
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

async fn write_audit(state: &ProjectIpAccessState, audit: &dyn AuditOperation) {
    // The write already succeeded; a failed audit entry is reported, not fatal.
    if let Err(e) = state.audit_service.create_audit_log(audit).await {
        error!(
            "Failed to create audit log for {}: {}",
            audit.operation_type(),
            e
        );
    }
}

// ---- shared bodies -----------------------------------------------------

async fn get_policy(
    state: &ProjectIpAccessState,
    auth: &temps_auth::AuthContext,
    metadata: &RequestMetadata,
    project_id: i32,
    environment_id: Option<i32>,
) -> Result<ProjectIpAccessDetail, Problem> {
    project_permission_guard!(auth, ProjectsRead, project_id, state.project_access_checker);
    project_scope_guard!(auth, project_id);

    let policy = state.service.get(project_id, environment_id).await?;
    Ok(detail(policy, metadata))
}

async fn put_policy(
    state: &ProjectIpAccessState,
    auth: &temps_auth::AuthContext,
    metadata: &RequestMetadata,
    project_id: i32,
    environment_id: Option<i32>,
    request: SetProjectIpAccessRequest,
) -> Result<ProjectIpAccessDetail, Problem> {
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);

    let acknowledged_lockout = request.acknowledge_lockout;
    let user_id = auth.user_id_opt();
    let policy = state
        .service
        .set(
            project_id,
            environment_id,
            request,
            caller_ip(metadata),
            user_id,
        )
        .await?;

    write_audit(
        state,
        &ProjectIpAccessUpdatedAudit {
            context: audit_context(user_id, metadata),
            project_id,
            environment_id,
            mode: policy.mode,
            cidr_count: policy.cidrs.len(),
            cidrs: policy.cidrs.clone(),
            acknowledged_lockout,
        },
    )
    .await;

    Ok(detail(policy, metadata))
}

async fn delete_policy(
    state: &ProjectIpAccessState,
    auth: &temps_auth::AuthContext,
    metadata: &RequestMetadata,
    project_id: i32,
    environment_id: Option<i32>,
) -> Result<(), Problem> {
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);

    state.service.delete(project_id, environment_id).await?;

    write_audit(
        state,
        &ProjectIpAccessDeletedAudit {
            context: audit_context(auth.user_id_opt(), metadata),
            project_id,
            environment_id,
        },
    )
    .await;
    Ok(())
}

async fn test_policy(
    state: &ProjectIpAccessState,
    auth: &temps_auth::AuthContext,
    project_id: i32,
    environment_id: Option<i32>,
    request: TestProjectIpAccessRequest,
) -> Result<TestProjectIpAccessResponse, Problem> {
    project_permission_guard!(auth, ProjectsRead, project_id, state.project_access_checker);
    project_scope_guard!(auth, project_id);

    Ok(state
        .service
        .test(project_id, environment_id, request)
        .await?)
}

// ---- project scope -----------------------------------------------------

/// Get a project's IP access policy
///
/// Returns `configured: false` and mode `disabled` when nothing is stored.
#[utoipa::path(
    tag = "Project IP Access",
    get,
    path = "/projects/{project_id}/ip-access",
    params(("project_id" = i32, Path, description = "Project ID")),
    responses(
        (status = 200, description = "The project-wide IP access policy", body = ProjectIpAccessDetail),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_project_ip_access(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectIpAccessState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    Ok(Json(
        get_policy(&state, &auth, &metadata, project_id, None).await?,
    ))
}

/// Set a project's IP access policy
///
/// Replaces the whole project-wide policy. An `allow` list must name at least
/// one CIDR. A save that would lock out the caller's own address is refused
/// with `409` unless `acknowledge_lockout` is `true`.
#[utoipa::path(
    tag = "Project IP Access",
    put,
    path = "/projects/{project_id}/ip-access",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = SetProjectIpAccessRequest,
    responses(
        (status = 200, description = "Policy saved", body = ProjectIpAccessDetail),
        (status = 400, description = "Invalid policy", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project not found", body = ProblemDetails),
        (status = 409, description = "Policy would lock out the caller", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn put_project_ip_access(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectIpAccessState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<SetProjectIpAccessRequest>,
) -> Result<impl IntoResponse, Problem> {
    Ok(Json(
        put_policy(&state, &auth, &metadata, project_id, None, request).await?,
    ))
}

/// Remove a project's IP access policy
///
/// The project becomes reachable from any address again (environments with
/// their own policy keep it).
#[utoipa::path(
    tag = "Project IP Access",
    delete,
    path = "/projects/{project_id}/ip-access",
    params(("project_id" = i32, Path, description = "Project ID")),
    responses(
        (status = 204, description = "Policy removed"),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project or policy not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_project_ip_access(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectIpAccessState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    delete_policy(&state, &auth, &metadata, project_id, None).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Try an address against a project's IP access policy
///
/// Evaluates the stored policy, or an unsaved draft when `mode` and `cidrs`
/// are supplied, using the same matcher the proxy uses.
#[utoipa::path(
    tag = "Project IP Access",
    post,
    path = "/projects/{project_id}/ip-access/test",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = TestProjectIpAccessRequest,
    responses(
        (status = 200, description = "Evaluation result", body = TestProjectIpAccessResponse),
        (status = 400, description = "Invalid address or draft", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn test_project_ip_access(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectIpAccessState>>,
    Path(project_id): Path<i32>,
    Json(request): Json<TestProjectIpAccessRequest>,
) -> Result<impl IntoResponse, Problem> {
    Ok(Json(
        test_policy(&state, &auth, project_id, None, request).await?,
    ))
}

// ---- environment scope -------------------------------------------------

/// Get an environment's IP access policy
///
/// `configured: false` means the environment inherits the project-wide policy.
#[utoipa::path(
    tag = "Project IP Access",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/ip-access",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    responses(
        (status = 200, description = "The environment's own IP access policy", body = ProjectIpAccessDetail),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project or environment not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_environment_ip_access(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectIpAccessState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    Ok(Json(
        get_policy(&state, &auth, &metadata, project_id, Some(environment_id)).await?,
    ))
}

/// Set an environment's IP access policy
///
/// Replaces (does not extend) the project-wide policy for this environment.
/// Mode `disabled` exempts the environment from a project-wide list.
#[utoipa::path(
    tag = "Project IP Access",
    put,
    path = "/projects/{project_id}/environments/{environment_id}/ip-access",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    request_body = SetProjectIpAccessRequest,
    responses(
        (status = 200, description = "Policy saved", body = ProjectIpAccessDetail),
        (status = 400, description = "Invalid policy", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project or environment not found", body = ProblemDetails),
        (status = 409, description = "Policy would lock out the caller", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn put_environment_ip_access(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectIpAccessState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    Json(request): Json<SetProjectIpAccessRequest>,
) -> Result<impl IntoResponse, Problem> {
    Ok(Json(
        put_policy(
            &state,
            &auth,
            &metadata,
            project_id,
            Some(environment_id),
            request,
        )
        .await?,
    ))
}

/// Remove an environment's IP access policy
///
/// The environment goes back to inheriting the project-wide policy.
#[utoipa::path(
    tag = "Project IP Access",
    delete,
    path = "/projects/{project_id}/environments/{environment_id}/ip-access",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    responses(
        (status = 204, description = "Policy removed"),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project, environment or policy not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_environment_ip_access(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectIpAccessState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    delete_policy(&state, &auth, &metadata, project_id, Some(environment_id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Try an address against an environment's IP access policy
///
/// Evaluates the policy that governs the environment (its own, else the
/// project's), or an unsaved draft when `mode` and `cidrs` are supplied.
#[utoipa::path(
    tag = "Project IP Access",
    post,
    path = "/projects/{project_id}/environments/{environment_id}/ip-access/test",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    request_body = TestProjectIpAccessRequest,
    responses(
        (status = 200, description = "Evaluation result", body = TestProjectIpAccessResponse),
        (status = 400, description = "Invalid address or draft", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project or environment not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn test_environment_ip_access(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectIpAccessState>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    Json(request): Json<TestProjectIpAccessRequest>,
) -> Result<impl IntoResponse, Problem> {
    Ok(Json(
        test_policy(&state, &auth, project_id, Some(environment_id), request).await?,
    ))
}

pub fn create_routes() -> axum::Router<Arc<ProjectIpAccessState>> {
    use axum::routing::{get, post};

    axum::Router::new()
        .route(
            "/projects/{project_id}/ip-access",
            get(get_project_ip_access)
                .put(put_project_ip_access)
                .delete(delete_project_ip_access),
        )
        .route(
            "/projects/{project_id}/ip-access/test",
            post(test_project_ip_access),
        )
        .route(
            "/projects/{project_id}/environments/{environment_id}/ip-access",
            get(get_environment_ip_access)
                .put(put_environment_ip_access)
                .delete(delete_environment_ip_access),
        )
        .route(
            "/projects/{project_id}/environments/{environment_id}/ip-access/test",
            post(test_environment_ip_access),
        )
}

pub fn openapi() -> utoipa::openapi::OpenApi {
    use utoipa::OpenApi;

    #[derive(OpenApi)]
    #[openapi(
        paths(
            get_project_ip_access,
            put_project_ip_access,
            delete_project_ip_access,
            test_project_ip_access,
            get_environment_ip_access,
            put_environment_ip_access,
            delete_environment_ip_access,
            test_environment_ip_access,
        ),
        components(schemas(
            ProjectIpAccessDetail,
            ProjectIpAccessResponse,
            SetProjectIpAccessRequest,
            TestProjectIpAccessRequest,
            TestProjectIpAccessResponse,
            IpAccessCidrEntry,
            IpAccessMode,
            PolicySource,
        )),
        tags(
            (name = "Project IP Access", description = "Per-project and per-environment client IP allow/deny lists")
        )
    )]
    struct ProjectIpAccessApiDoc;

    ProjectIpAccessApiDoc::openapi()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn problem_status(error: ProjectIpAccessError) -> StatusCode {
        Problem::from(error).status_code
    }

    #[test]
    fn errors_map_to_distinct_statuses_with_ids_in_detail() {
        assert_eq!(
            problem_status(ProjectIpAccessError::ProjectNotFound { project_id: 3 }),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            problem_status(ProjectIpAccessError::EnvironmentNotFound {
                project_id: 3,
                environment_id: 4
            }),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            problem_status(ProjectIpAccessError::EmptyAllowList {
                scope: "project 3".into()
            }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            problem_status(ProjectIpAccessError::WouldLockOutCaller {
                scope: "project 3".into(),
                caller_ip: "203.0.113.9".into(),
                effect: "exclude"
            }),
            StatusCode::CONFLICT
        );
        assert_eq!(
            problem_status(ProjectIpAccessError::ConcurrentUpdate {
                scope: "project 3".into()
            }),
            StatusCode::CONFLICT
        );
        assert_eq!(
            problem_status(ProjectIpAccessError::Database(sea_orm::DbErr::Custom(
                "boom".into()
            ))),
            StatusCode::INTERNAL_SERVER_ERROR
        );

        let problem = Problem::from(ProjectIpAccessError::EnvironmentNotFound {
            project_id: 3,
            environment_id: 4,
        });
        let body = serde_json::to_string(&problem.body).unwrap_or_default();
        assert!(body.contains('3') && body.contains('4'), "body: {body}");
    }

    // ---- routes -------------------------------------------------------
    //
    // These drive the real router: extractors, permission guards, the
    // service on a mock database, audit writes and the RFC 7807 bodies.

    use axum::body::Body;
    use axum::http::{header, Method, Request};
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use std::collections::BTreeMap;
    use temps_auth::{AuthContext, Permission, Role};
    use temps_entities::project_ip_access_rules::IpAccessCidrs;
    use tower::ServiceExt;

    const CALLER_IP: &str = "203.0.113.9";

    #[derive(Default)]
    struct RecordingAudit {
        entries: std::sync::Mutex<Vec<(String, String)>>,
    }

    impl RecordingAudit {
        fn recorded(&self) -> Vec<(String, String)> {
            self.entries
                .lock()
                .map(|entries| entries.clone())
                .unwrap_or_default()
        }
    }

    #[async_trait::async_trait]
    impl AuditLogger for RecordingAudit {
        async fn create_audit_log(&self, operation: &dyn AuditOperation) -> anyhow::Result<()> {
            let payload = operation.serialize()?;
            if let Ok(mut entries) = self.entries.lock() {
                entries.push((operation.operation_type(), payload));
            }
            Ok(())
        }
    }

    struct DenyEveryProject;

    #[async_trait::async_trait]
    impl temps_core::ProjectAccessChecker for DenyEveryProject {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            _project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(false)
        }
    }

    fn user() -> temps_entities::users::Model {
        let now = chrono::Utc::now();
        temps_entities::users::Model {
            id: 1,
            name: "Test User".to_string(),
            email: "user1@example.test".to_string(),
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

    fn session(role: Role) -> AuthContext {
        AuthContext::new_session(user(), role)
    }

    fn metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: CALLER_IP.to_string(),
            user_agent: "route-test".to_string(),
            headers: axum::http::HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://console.test".to_string(),
            scheme: "http".to_string(),
            host: "console.test".to_string(),
            is_secure: false,
        }
    }

    fn app(
        db: sea_orm::DatabaseConnection,
        auth: Option<AuthContext>,
        checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
        audit: Arc<RecordingAudit>,
    ) -> axum::Router {
        let state = Arc::new(ProjectIpAccessState {
            service: Arc::new(ProjectIpAccessService::new(Arc::new(db))),
            audit_service: audit,
            project_access_checker: checker,
        });
        create_routes()
            .layer(axum::middleware::from_fn(
                move |mut request: axum::extract::Request, next: axum::middleware::Next| {
                    let auth = auth.clone();
                    async move {
                        request.extensions_mut().insert(metadata());
                        if let Some(auth) = auth {
                            request.extensions_mut().insert(auth);
                        }
                        next.run(request).await
                    }
                },
            ))
            .with_state(state)
    }

    fn json_request(method: Method, uri: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("request builds")
    }

    fn empty_request(method: Method, uri: &str) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .body(Body::empty())
            .expect("request builds")
    }

    async fn send(
        app: axum::Router,
        request: Request<Body>,
    ) -> (StatusCode, Option<String>, serde_json::Value) {
        let response = app.oneshot(request).await.expect("router answers");
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body reads");
        let body = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
        };
        (status, content_type, body)
    }

    fn id_row(id: i32) -> BTreeMap<&'static str, sea_orm::Value> {
        BTreeMap::from([("id", sea_orm::Value::Int(Some(id)))])
    }

    fn no_ids() -> Vec<BTreeMap<&'static str, sea_orm::Value>> {
        Vec::new()
    }

    fn no_rules() -> Vec<temps_entities::project_ip_access_rules::Model> {
        Vec::new()
    }

    fn rule(
        project_id: i32,
        environment_id: Option<i32>,
        mode: &str,
        cidrs: &[&str],
    ) -> temps_entities::project_ip_access_rules::Model {
        let now = chrono::Utc::now();
        temps_entities::project_ip_access_rules::Model {
            id: 11,
            project_id,
            environment_id,
            mode: mode.to_string(),
            cidrs: IpAccessCidrs(
                cidrs
                    .iter()
                    .map(|cidr| IpAccessCidrEntry {
                        cidr: cidr.to_string(),
                        description: None,
                    })
                    .collect(),
            ),
            updated_by: Some(1),
            revision: 3,
            created_at: now,
            updated_at: now,
        }
    }

    fn mock_db() -> MockDatabase {
        MockDatabase::new(DatabaseBackend::Postgres)
    }

    fn assert_problem(
        outcome: &(StatusCode, Option<String>, serde_json::Value),
        status: StatusCode,
        title: &str,
    ) {
        assert_eq!(outcome.0, status, "body: {}", outcome.2);
        assert!(
            outcome
                .1
                .as_deref()
                .is_some_and(|ct| ct.starts_with("application/problem+json")),
            "content type: {:?}",
            outcome.1
        );
        assert_eq!(outcome.2["title"], title, "body: {}", outcome.2);
        assert!(
            outcome.2["detail"].as_str().is_some_and(|d| !d.is_empty()),
            "problem must explain itself: {}",
            outcome.2
        );
    }

    const ALL_ROUTES: [(&str, &str); 8] = [
        ("GET", "/projects/5/ip-access"),
        ("PUT", "/projects/5/ip-access"),
        ("DELETE", "/projects/5/ip-access"),
        ("POST", "/projects/5/ip-access/test"),
        ("GET", "/projects/5/environments/9/ip-access"),
        ("PUT", "/projects/5/environments/9/ip-access"),
        ("DELETE", "/projects/5/environments/9/ip-access"),
        ("POST", "/projects/5/environments/9/ip-access/test"),
    ];

    fn request_for(method: &str, uri: &str) -> Request<Body> {
        let method = Method::from_bytes(method.as_bytes()).expect("known method");
        match method {
            Method::PUT => json_request(
                method,
                uri,
                serde_json::json!({ "mode": "deny", "cidrs": [{ "cidr": "198.51.100.0/24" }] }),
            ),
            Method::POST => json_request(method, uri, serde_json::json!({ "ip": "198.51.100.7" })),
            other => empty_request(other, uri),
        }
    }

    #[tokio::test]
    async fn every_route_requires_authentication() {
        for (method, uri) in ALL_ROUTES {
            // Nothing is queued: an unauthenticated request must not reach
            // the database.
            let audit = Arc::new(RecordingAudit::default());
            let outcome = send(
                app(mock_db().into_connection(), None, None, audit.clone()),
                request_for(method, uri),
            )
            .await;
            assert_problem(
                &outcome,
                StatusCode::UNAUTHORIZED,
                "Authentication Required",
            );
            assert!(audit.recorded().is_empty(), "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn writes_require_the_projects_write_permission() {
        for (method, uri) in ALL_ROUTES {
            if !matches!(method, "PUT" | "DELETE") {
                continue;
            }
            let audit = Arc::new(RecordingAudit::default());
            let outcome = send(
                app(
                    mock_db().into_connection(),
                    Some(session(Role::Reader)),
                    None,
                    audit.clone(),
                ),
                request_for(method, uri),
            )
            .await;
            assert_problem(&outcome, StatusCode::FORBIDDEN, "Insufficient Permissions");
            assert!(audit.recorded().is_empty(), "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn reads_require_the_projects_read_permission() {
        let no_read = AuthContext::new_api_key(
            user(),
            None,
            Some(vec![Permission::DeploymentsRead]),
            "scoped-key".to_string(),
            7,
        );
        for (method, uri) in ALL_ROUTES {
            if !matches!(method, "GET" | "POST") {
                continue;
            }
            let outcome = send(
                app(
                    mock_db().into_connection(),
                    Some(no_read.clone()),
                    None,
                    Arc::new(RecordingAudit::default()),
                ),
                request_for(method, uri),
            )
            .await;
            assert_problem(&outcome, StatusCode::FORBIDDEN, "Insufficient Permissions");
        }
    }

    #[tokio::test]
    async fn a_user_outside_the_project_team_is_refused() {
        let checker: Arc<dyn temps_core::ProjectAccessChecker> = Arc::new(DenyEveryProject);
        let audit = Arc::new(RecordingAudit::default());
        let outcome = send(
            app(
                mock_db().into_connection(),
                Some(session(Role::User)),
                Some(checker),
                audit.clone(),
            ),
            request_for("PUT", "/projects/5/ip-access"),
        )
        .await;
        assert_problem(&outcome, StatusCode::FORBIDDEN, "Project Access Denied");
        assert!(audit.recorded().is_empty());
    }

    #[tokio::test]
    async fn get_project_policy_returns_the_stored_list_and_the_callers_address() {
        let db = mock_db()
            .append_query_results(vec![vec![id_row(5)]])
            .append_query_results(vec![vec![rule(5, None, "allow", &["100.64.0.0/10"])]])
            .into_connection();
        let outcome = send(
            app(
                db,
                Some(session(Role::Admin)),
                None,
                Arc::new(RecordingAudit::default()),
            ),
            empty_request(Method::GET, "/projects/5/ip-access"),
        )
        .await;
        assert_eq!(outcome.0, StatusCode::OK, "body: {}", outcome.2);
        assert_eq!(outcome.2["project_id"], 5);
        assert_eq!(outcome.2["environment_id"], serde_json::Value::Null);
        assert_eq!(outcome.2["configured"], true);
        assert_eq!(outcome.2["mode"], "allow");
        assert_eq!(outcome.2["cidrs"][0]["cidr"], "100.64.0.0/10");
        assert_eq!(outcome.2["your_ip"], CALLER_IP);
        let updated_at = outcome.2["updated_at"].as_str().unwrap_or_default();
        assert!(updated_at.ends_with('Z'), "ISO 8601 with Z: {updated_at}");
    }

    #[tokio::test]
    async fn get_environment_policy_reports_that_it_inherits_when_nothing_is_stored() {
        let db = mock_db()
            .append_query_results(vec![vec![id_row(5)]])
            .append_query_results(vec![vec![id_row(9)]])
            .append_query_results(vec![no_rules()])
            .into_connection();
        let outcome = send(
            app(
                db,
                Some(session(Role::Admin)),
                None,
                Arc::new(RecordingAudit::default()),
            ),
            empty_request(Method::GET, "/projects/5/environments/9/ip-access"),
        )
        .await;
        assert_eq!(outcome.0, StatusCode::OK, "body: {}", outcome.2);
        assert_eq!(outcome.2["environment_id"], 9);
        assert_eq!(outcome.2["configured"], false);
        assert_eq!(outcome.2["mode"], "disabled");
    }

    #[tokio::test]
    async fn an_environment_of_another_project_is_not_found() {
        // The project exists; the environment id is not one of its own.
        let db = mock_db()
            .append_query_results(vec![vec![id_row(5)]])
            .append_query_results(vec![no_ids()])
            .into_connection();
        let outcome = send(
            app(
                db,
                Some(session(Role::Admin)),
                None,
                Arc::new(RecordingAudit::default()),
            ),
            empty_request(Method::GET, "/projects/5/environments/77/ip-access"),
        )
        .await;
        assert_problem(&outcome, StatusCode::NOT_FOUND, "Environment Not Found");
        let detail = outcome.2["detail"].as_str().unwrap_or_default();
        assert!(detail.contains('5') && detail.contains("77"), "{detail}");
    }

    #[tokio::test]
    async fn an_unknown_project_is_not_found_for_every_scope() {
        for (method, uri) in ALL_ROUTES {
            let db = mock_db()
                .append_query_results(vec![no_ids()])
                .into_connection();
            let request = match method {
                "PUT" => json_request(
                    Method::PUT,
                    uri,
                    serde_json::json!({
                        "mode": "deny",
                        "cidrs": [{ "cidr": "198.51.100.0/24" }],
                    }),
                ),
                other => request_for(other, uri),
            };
            let outcome = send(
                app(
                    db,
                    Some(session(Role::Admin)),
                    None,
                    Arc::new(RecordingAudit::default()),
                ),
                request,
            )
            .await;
            assert_problem(&outcome, StatusCode::NOT_FOUND, "Project Not Found");
        }
    }

    #[tokio::test]
    async fn put_project_policy_saves_and_audits_it() {
        let saved = rule(5, None, "allow", &["203.0.113.0/24"]);
        let db = mock_db()
            .append_query_results(vec![vec![id_row(5)]])
            .append_query_results(vec![no_rules()])
            .append_query_results(vec![vec![saved]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 11,
                rows_affected: 1,
            }])
            .into_connection();
        let audit = Arc::new(RecordingAudit::default());
        let outcome = send(
            app(db, Some(session(Role::Admin)), None, audit.clone()),
            json_request(
                Method::PUT,
                "/projects/5/ip-access",
                serde_json::json!({
                    "mode": "allow",
                    "cidrs": [{ "cidr": "203.0.113.0/24", "description": "Office" }],
                }),
            ),
        )
        .await;
        assert_eq!(outcome.0, StatusCode::OK, "body: {}", outcome.2);
        assert_eq!(outcome.2["mode"], "allow");
        assert_eq!(outcome.2["cidrs"][0]["cidr"], "203.0.113.0/24");

        let recorded = audit.recorded();
        assert_eq!(recorded.len(), 1, "exactly one audit entry");
        assert_eq!(recorded[0].0, "PROJECT_IP_ACCESS_UPDATED");
        assert!(recorded[0].1.contains("203.0.113.0/24"));
        assert!(recorded[0].1.contains("\"project_id\":5"));
    }

    #[tokio::test]
    async fn put_environment_policy_saves_and_audits_the_environment() {
        let saved = rule(5, Some(9), "deny", &["198.51.100.0/24"]);
        let db = mock_db()
            .append_query_results(vec![vec![id_row(5)]])
            .append_query_results(vec![vec![id_row(9)]])
            .append_query_results(vec![no_rules()])
            .append_query_results(vec![vec![saved]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 11,
                rows_affected: 1,
            }])
            .into_connection();
        let audit = Arc::new(RecordingAudit::default());
        let outcome = send(
            app(db, Some(session(Role::Admin)), None, audit.clone()),
            json_request(
                Method::PUT,
                "/projects/5/environments/9/ip-access",
                serde_json::json!({
                    "mode": "deny",
                    "cidrs": [{ "cidr": "198.51.100.0/24" }],
                }),
            ),
        )
        .await;
        assert_eq!(outcome.0, StatusCode::OK, "body: {}", outcome.2);
        assert_eq!(outcome.2["environment_id"], 9);
        let recorded = audit.recorded();
        assert_eq!(recorded.len(), 1);
        assert!(recorded[0].1.contains("\"environment_id\":9"));
    }

    #[tokio::test]
    async fn a_policy_that_locks_the_caller_out_is_a_conflict_until_acknowledged() {
        let audit = Arc::new(RecordingAudit::default());
        // No query results are queued: the refusal happens before any read.
        let outcome = send(
            app(
                mock_db().into_connection(),
                Some(session(Role::Admin)),
                None,
                audit.clone(),
            ),
            json_request(
                Method::PUT,
                "/projects/5/ip-access",
                serde_json::json!({
                    "mode": "allow",
                    "cidrs": [{ "cidr": "100.64.0.0/10" }],
                }),
            ),
        )
        .await;
        assert_problem(&outcome, StatusCode::CONFLICT, "Policy Would Lock You Out");
        assert!(outcome.2["detail"]
            .as_str()
            .unwrap_or_default()
            .contains(CALLER_IP));
        assert!(audit.recorded().is_empty(), "a refused save is not audited");

        // The same body with the acknowledgement is accepted and the
        // acknowledgement is part of the audit record.
        let saved = rule(5, None, "allow", &["100.64.0.0/10"]);
        let db = mock_db()
            .append_query_results(vec![vec![id_row(5)]])
            .append_query_results(vec![no_rules()])
            .append_query_results(vec![vec![saved]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 11,
                rows_affected: 1,
            }])
            .into_connection();
        let outcome = send(
            app(db, Some(session(Role::Admin)), None, audit.clone()),
            json_request(
                Method::PUT,
                "/projects/5/ip-access",
                serde_json::json!({
                    "mode": "allow",
                    "cidrs": [{ "cidr": "100.64.0.0/10" }],
                    "acknowledge_lockout": true,
                }),
            ),
        )
        .await;
        assert_eq!(outcome.0, StatusCode::OK, "body: {}", outcome.2);
        let recorded = audit.recorded();
        assert_eq!(recorded.len(), 1);
        assert!(recorded[0].1.contains("\"acknowledged_lockout\":true"));
    }

    #[tokio::test]
    async fn an_invalid_policy_is_a_bad_request_with_the_offending_value() {
        let audit = Arc::new(RecordingAudit::default());
        let outcome = send(
            app(
                mock_db().into_connection(),
                Some(session(Role::Admin)),
                None,
                audit.clone(),
            ),
            json_request(
                Method::PUT,
                "/projects/5/ip-access",
                serde_json::json!({
                    "mode": "deny",
                    "cidrs": [{ "cidr": "not-a-network" }],
                }),
            ),
        )
        .await;
        assert_problem(
            &outcome,
            StatusCode::BAD_REQUEST,
            "Invalid IP Access Policy",
        );
        assert!(outcome.2["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("not-a-network"));
        assert!(audit.recorded().is_empty());
    }

    #[tokio::test]
    async fn delete_removes_the_policy_audits_it_and_answers_no_content() {
        let db = mock_db()
            .append_query_results(vec![vec![id_row(5)]])
            .append_query_results(vec![vec![rule(5, None, "deny", &["198.51.100.0/24"])]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let audit = Arc::new(RecordingAudit::default());
        let outcome = send(
            app(db, Some(session(Role::Admin)), None, audit.clone()),
            empty_request(Method::DELETE, "/projects/5/ip-access"),
        )
        .await;
        assert_eq!(outcome.0, StatusCode::NO_CONTENT);
        let recorded = audit.recorded();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "PROJECT_IP_ACCESS_DELETED");
    }

    #[tokio::test]
    async fn delete_without_a_stored_policy_is_not_found_and_not_audited() {
        let db = mock_db()
            .append_query_results(vec![vec![id_row(5)]])
            .append_query_results(vec![vec![id_row(9)]])
            .append_query_results(vec![no_rules()])
            .into_connection();
        let audit = Arc::new(RecordingAudit::default());
        let outcome = send(
            app(db, Some(session(Role::Admin)), None, audit.clone()),
            empty_request(Method::DELETE, "/projects/5/environments/9/ip-access"),
        )
        .await;
        assert_problem(
            &outcome,
            StatusCode::NOT_FOUND,
            "IP Access Policy Not Found",
        );
        assert!(audit.recorded().is_empty());
    }

    #[tokio::test]
    async fn test_endpoint_evaluates_the_inherited_project_list_for_an_environment() {
        let db = mock_db()
            .append_query_results(vec![vec![id_row(5)]])
            .append_query_results(vec![vec![id_row(9)]])
            // the environment has no list of its own...
            .append_query_results(vec![no_rules()])
            // ...so the project's list decides
            .append_query_results(vec![vec![rule(5, None, "allow", &["100.64.0.0/10"])]])
            .into_connection();
        let outcome = send(
            app(
                db,
                Some(session(Role::Reader)),
                None,
                Arc::new(RecordingAudit::default()),
            ),
            json_request(
                Method::POST,
                "/projects/5/environments/9/ip-access/test",
                serde_json::json!({ "ip": "100.100.1.1" }),
            ),
        )
        .await;
        assert_eq!(outcome.0, StatusCode::OK, "body: {}", outcome.2);
        assert_eq!(outcome.2["allowed"], true);
        assert_eq!(outcome.2["source"], "project");
        assert_eq!(outcome.2["matched_cidr"], "100.64.0.0/10");
    }

    #[tokio::test]
    async fn test_endpoint_evaluates_an_unsaved_draft_and_rejects_a_bad_address() {
        let db = mock_db()
            .append_query_results(vec![vec![id_row(5)]])
            .into_connection();
        let outcome = send(
            app(
                db,
                Some(session(Role::Reader)),
                None,
                Arc::new(RecordingAudit::default()),
            ),
            json_request(
                Method::POST,
                "/projects/5/ip-access/test",
                serde_json::json!({
                    "ip": "198.51.100.7",
                    "mode": "deny",
                    "cidrs": [{ "cidr": "198.51.100.0/24" }],
                }),
            ),
        )
        .await;
        assert_eq!(outcome.0, StatusCode::OK, "body: {}", outcome.2);
        assert_eq!(outcome.2["allowed"], false);
        assert_eq!(outcome.2["source"], "draft");

        let outcome = send(
            app(
                mock_db().into_connection(),
                Some(session(Role::Reader)),
                None,
                Arc::new(RecordingAudit::default()),
            ),
            json_request(
                Method::POST,
                "/projects/5/ip-access/test",
                serde_json::json!({ "ip": "999.1.1.1" }),
            ),
        )
        .await;
        assert_problem(
            &outcome,
            StatusCode::BAD_REQUEST,
            "Invalid IP Access Policy",
        );
    }

    #[test]
    fn audit_operations_serialise_without_secrets() {
        let audit = ProjectIpAccessUpdatedAudit {
            context: IpAccessAuditContext {
                user_id: Some(1),
                ip_address: Some("203.0.113.9".into()),
                user_agent: "test".into(),
            },
            project_id: 3,
            environment_id: Some(4),
            mode: IpAccessMode::Allow,
            cidr_count: 1,
            cidrs: vec![IpAccessCidrEntry {
                cidr: "100.64.0.0/10".into(),
                description: None,
            }],
            acknowledged_lockout: false,
        };
        assert_eq!(audit.operation_type(), "PROJECT_IP_ACCESS_UPDATED");
        assert_eq!(audit.user_id(), Some(1));
        let json = temps_core::AuditOperation::serialize(&audit).expect("serialises");
        assert!(json.contains("100.64.0.0/10"));
        assert!(json.contains("\"mode\":\"allow\""));
    }
}
