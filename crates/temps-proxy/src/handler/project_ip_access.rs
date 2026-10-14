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
            | ProjectIpAccessError::InvalidIpAddress { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid IP Access Policy")
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
