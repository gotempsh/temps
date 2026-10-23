// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP handlers for functions.
//!
//! Functions reuse the deployment permissions for now (a function is a
//! deployable workload): `deployments:read` to see them,
//! `deployments:create` to upload one, `deployments:write` to wake or sleep
//! one, `deployments:delete` to remove one.

use std::sync::Arc;

use anyhow::Result as AnyResult;
use axum::{
    extract::{DefaultBodyLimit, Extension, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::{
    AuditContext, AuditLogger, AuditOperation, ProjectAccessChecker, RequestMetadata,
};
use temps_workload_activator::ActivationError;
use tracing::error;
use utoipa::{IntoParams, OpenApi, ToSchema};

use crate::error::FunctionError;
use crate::service::{
    normalize_pagination, CreateFunction, FunctionArtifact, FunctionService, FunctionView,
    FunctionsCapability, RuntimeAvailability, MAX_WASM_BYTES,
};

pub struct FunctionsAppState {
    pub service: Arc<FunctionService>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub project_access_checker: Option<Arc<dyn ProjectAccessChecker>>,
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_capability,
        list_functions,
        create_function,
        get_function,
        delete_function,
        wake_function,
        sleep_function,
    ),
    components(schemas(
        CreateFunctionRequest,
        FunctionResponse,
        FunctionListResponse,
        FunctionsCapabilityResponse,
        RuntimeAvailabilityResponse,
        SleepFunctionResponse,
    )),
    tags((
        name = "Functions",
        description = "Upload a WASM module or an OCI image and serve it on its own hostname; \
                       it runs only while requests arrive"
    ))
)]
pub struct FunctionsApiDoc;

/// Largest create body: a base64 module of [`MAX_WASM_BYTES`] plus the
/// other fields.
const MAX_CREATE_BODY_BYTES: usize = MAX_WASM_BYTES / 3 * 4 + 64 * 1024;

pub fn configure_routes() -> Router<Arc<FunctionsAppState>> {
    Router::new()
        .route("/functions/capability", get(get_capability))
        .route("/projects/{project_id}/functions", get(list_functions))
        .route(
            "/projects/{project_id}/functions",
            post(create_function).layer(DefaultBodyLimit::max(MAX_CREATE_BODY_BYTES)),
        )
        .route(
            "/projects/{project_id}/functions/{function_id}",
            get(get_function).delete(delete_function),
        )
        .route(
            "/projects/{project_id}/functions/{function_id}/wake",
            post(wake_function),
        )
        .route(
            "/projects/{project_id}/functions/{function_id}/sleep",
            post(sleep_function),
        )
}

// =============================================================================
// DTOs
// =============================================================================

/// Exactly one of `wasm_base64` and `image`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateFunctionRequest {
    /// Lowercase letters, digits and single inner hyphens; unique in the
    /// project. Part of the hostname.
    #[schema(example = "resize-image")]
    pub name: String,
    /// A WASI preview 1 core module (`wasm32-wasip1`), base64. It serves
    /// HTTP CGI-style: the request body on stdin, the response on stdout.
    pub wasm_base64: Option<String>,
    /// An OCI image reference that serves HTTP on `port`.
    #[schema(example = "registry.example.com/team/fn:1")]
    pub image: Option<String>,
    /// Port the function serves on. Default 8080.
    pub port: Option<u16>,
    /// Seconds without a request before it is stopped. 10..=86400, default 300.
    pub idle_timeout_seconds: Option<i32>,
    /// Seconds the first request waits for it to start. 1..=300, default 30.
    pub wake_timeout_seconds: Option<i32>,
    /// Memory limit in MiB. 1..=4096.
    pub memory_limit_mb: Option<i32>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FunctionResponse {
    pub id: i64,
    pub project_id: i32,
    pub name: String,
    /// `wasm` or `oci`.
    pub artifact_kind: String,
    pub image_reference: Option<String>,
    pub wasm_digest: Option<String>,
    pub wasm_size_bytes: Option<i64>,
    pub port: i32,
    pub idle_timeout_seconds: i32,
    pub wake_timeout_seconds: i32,
    pub memory_limit_mb: Option<i32>,
    /// Requests to this hostname reach the function.
    pub hostname: String,
    pub workload_id: Option<i64>,
    /// `asleep`, `waking`, `awake`, `sleeping`, or `unregistered`.
    pub state: String,
    /// Why it is not serving (the plane's `Ready` reason), on the detail view.
    pub ready_reason: Option<String>,
    pub ready_message: Option<String>,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: DateTime<Utc>,
}

impl From<FunctionView> for FunctionResponse {
    fn from(view: FunctionView) -> Self {
        let function = view.function;
        Self {
            id: function.id,
            project_id: function.project_id,
            name: function.name,
            artifact_kind: function.artifact_kind,
            image_reference: function.image_reference,
            wasm_digest: function.wasm_digest,
            wasm_size_bytes: function.wasm_size_bytes,
            port: function.port,
            idle_timeout_seconds: function.idle_timeout_seconds,
            wake_timeout_seconds: function.wake_timeout_seconds,
            memory_limit_mb: function.memory_limit_mb,
            hostname: function.hostname,
            workload_id: function.workload_id,
            state: view.state.to_string(),
            ready_reason: view.ready_reason,
            ready_message: view.ready_message,
            created_at: function.created_at,
            updated_at: function.updated_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FunctionListResponse {
    pub functions: Vec<FunctionResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
    pub total_pages: u64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RuntimeAvailabilityResponse {
    pub available: bool,
    pub reason: Option<String>,
}

impl From<RuntimeAvailability> for RuntimeAvailabilityResponse {
    fn from(value: RuntimeAvailability) -> Self {
        Self {
            available: value.available,
            reason: value.reason,
        }
    }
}

/// Whether functions can be created here, and what is missing if not.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FunctionsCapabilityResponse {
    pub configured: bool,
    pub wasm: RuntimeAvailabilityResponse,
    pub oci: RuntimeAvailabilityResponse,
    pub reason: Option<String>,
    pub setup_path: Option<String>,
}

impl From<FunctionsCapability> for FunctionsCapabilityResponse {
    fn from(value: FunctionsCapability) -> Self {
        Self {
            configured: value.configured,
            wasm: value.wasm.into(),
            oci: value.oci.into(),
            reason: value.reason,
            setup_path: value.setup_path,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SleepFunctionResponse {
    pub function: FunctionResponse,
    /// Whether it was running and is now stopped.
    pub slept: bool,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ListFunctionsQuery {
    /// 1-based page. Default 1.
    pub page: Option<u64>,
    /// Default 20, max 100.
    pub page_size: Option<u64>,
}

// =============================================================================
// Errors
// =============================================================================

impl From<FunctionError> for Problem {
    fn from(error: FunctionError) -> Self {
        match &error {
            FunctionError::NotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Function Not Found")
                .with_detail(error.to_string()),
            FunctionError::ProjectNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Project Not Found")
                .with_detail(error.to_string()),
            FunctionError::Validation { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Validation Error")
                .with_detail(error.to_string()),
            FunctionError::DuplicateName { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Function Already Exists")
                .with_detail(error.to_string()),
            FunctionError::PlaneUnavailable { .. } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("Functions Unavailable")
                    .with_detail(error.to_string())
            }
            FunctionError::NoRuntime { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("No Runtime For This Function")
                .with_detail(error.to_string()),
            FunctionError::Plane { source, .. } => {
                let status = if matches!(**source, temps_workload_plane::PlaneError::Admission(_)) {
                    StatusCode::BAD_REQUEST
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                };
                problemdetails::new(status)
                    .with_title("Workload Plane Error")
                    .with_detail(error.to_string())
            }
            FunctionError::Activation { source, .. } => {
                let status = match source {
                    ActivationError::Saturated { .. }
                    | ActivationError::WakeTimeout { .. }
                    | ActivationError::WakeFailed { .. } => StatusCode::SERVICE_UNAVAILABLE,
                    ActivationError::NotRegistered { .. }
                    | ActivationError::NoBackend { .. }
                    | ActivationError::SleepFailed { .. } => StatusCode::INTERNAL_SERVER_ERROR,
                };
                problemdetails::new(status)
                    .with_title("Function Activation Failed")
                    .with_detail(error.to_string())
            }
            FunctionError::Storage { .. } => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Function Storage Error")
                .with_detail(error.to_string()),
            FunctionError::Database { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

// =============================================================================
// Audit
// =============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct FunctionAudit {
    #[serde(skip)]
    operation: &'static str,
    pub context: AuditContext,
    pub project_id: i32,
    pub function_id: i64,
    pub name: String,
    pub artifact_kind: String,
    pub wasm_digest: Option<String>,
    pub image_reference: Option<String>,
    pub hostname: String,
}

impl AuditOperation for FunctionAudit {
    fn operation_type(&self) -> String {
        self.operation.to_string()
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

    fn serialize(&self) -> AnyResult<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize function audit: {}", e))
    }
}

async fn audit(
    state: &FunctionsAppState,
    operation: &'static str,
    auth: &temps_auth::AuthContext,
    metadata: &RequestMetadata,
    function: &FunctionResponse,
) {
    let entry = FunctionAudit {
        operation,
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id: function.project_id,
        function_id: function.id,
        name: function.name.clone(),
        artifact_kind: function.artifact_kind.clone(),
        wasm_digest: function.wasm_digest.clone(),
        image_reference: function.image_reference.clone(),
        hostname: function.hostname.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&entry).await {
        error!(
            function_id = function.id,
            operation, "Failed to create audit log for a function: {}", e
        );
    }
}

// =============================================================================
// Handlers
// =============================================================================

#[utoipa::path(
    tag = "Functions",
    get,
    path = "/functions/capability",
    responses(
        (status = 200, description = "Whether functions can run here", body = FunctionsCapabilityResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_capability(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FunctionsAppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);
    Ok(Json(FunctionsCapabilityResponse::from(
        state.service.capability(),
    )))
}

#[utoipa::path(
    tag = "Functions",
    get,
    path = "/projects/{project_id}/functions",
    params(("project_id" = i32, Path, description = "Project ID"), ListFunctionsQuery),
    responses(
        (status = 200, description = "Functions, newest first", body = FunctionListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_functions(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FunctionsAppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<ListFunctionsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, &state.project_access_checker);
    let (views, total) = state
        .service
        .list(project_id, query.page, query.page_size)
        .await?;
    let (page, page_size) = normalize_pagination(query.page, query.page_size);
    Ok(Json(FunctionListResponse {
        functions: views.into_iter().map(FunctionResponse::from).collect(),
        total,
        page,
        page_size,
        total_pages: total.div_ceil(page_size),
    }))
}

#[utoipa::path(
    tag = "Functions",
    post,
    path = "/projects/{project_id}/functions",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = CreateFunctionRequest,
    responses(
        (status = 201, description = "Function created; it starts on its first request", body = FunctionResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Project not found"),
        (status = 409, description = "Name taken, or no runtime runs this artifact"),
        (status = 503, description = "The workload plane is not running"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_function(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FunctionsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateFunctionRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsCreate);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, &state.project_access_checker);

    let artifact = match (request.wasm_base64.as_deref(), request.image.as_deref()) {
        (Some(encoded), None) => {
            let module = base64::engine::general_purpose::STANDARD
                .decode(encoded.trim())
                .map_err(|e| FunctionError::Validation {
                    message: format!("wasm_base64 is not valid base64: {e}"),
                })?;
            FunctionArtifact::Wasm { module }
        }
        (None, Some(image)) => FunctionArtifact::Oci {
            image: image.to_string(),
        },
        _ => {
            return Err(FunctionError::Validation {
                message: "set exactly one of wasm_base64 (a WASM module) and image (an OCI \
                          image reference)"
                    .to_string(),
            }
            .into())
        }
    };
    let function = state
        .service
        .create(
            project_id,
            CreateFunction {
                name: request.name,
                artifact,
                port: request.port,
                idle_timeout_seconds: request.idle_timeout_seconds,
                wake_timeout_seconds: request.wake_timeout_seconds,
                memory_limit_mb: request.memory_limit_mb,
            },
        )
        .await?;
    let view = state.service.get(project_id, function.id).await?;
    let response = FunctionResponse::from(view);
    audit(&state, "FUNCTION_CREATED", &auth, &metadata, &response).await;
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    tag = "Functions",
    get,
    path = "/projects/{project_id}/functions/{function_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("function_id" = i64, Path, description = "Function ID")
    ),
    responses(
        (status = 200, description = "The function and its state", body = FunctionResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Function not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_function(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FunctionsAppState>>,
    Path((project_id, function_id)): Path<(i32, i64)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, &state.project_access_checker);
    let view = state.service.get(project_id, function_id).await?;
    Ok(Json(FunctionResponse::from(view)))
}

#[utoipa::path(
    tag = "Functions",
    delete,
    path = "/projects/{project_id}/functions/{function_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("function_id" = i64, Path, description = "Function ID")
    ),
    responses(
        (status = 204, description = "Function deleted; its instance is removed"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Function not found"),
        (status = 503, description = "The workload plane is not running"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_function(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FunctionsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, function_id)): Path<(i32, i64)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsDelete);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, &state.project_access_checker);
    let function = state.service.delete(project_id, function_id).await?;
    let response = FunctionResponse::from(FunctionView {
        function,
        state: "deleted",
        ready_reason: None,
        ready_message: None,
    });
    audit(&state, "FUNCTION_DELETED", &auth, &metadata, &response).await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    tag = "Functions",
    post,
    path = "/projects/{project_id}/functions/{function_id}/wake",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("function_id" = i64, Path, description = "Function ID")
    ),
    responses(
        (status = 200, description = "The function is running", body = FunctionResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Function not found"),
        (status = 503, description = "It could not be started; the detail says why"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn wake_function(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FunctionsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, function_id)): Path<(i32, i64)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, &state.project_access_checker);
    let view = state.service.wake(project_id, function_id).await?;
    let response = FunctionResponse::from(view);
    audit(&state, "FUNCTION_WOKEN", &auth, &metadata, &response).await;
    Ok(Json(response))
}

#[utoipa::path(
    tag = "Functions",
    post,
    path = "/projects/{project_id}/functions/{function_id}/sleep",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("function_id" = i64, Path, description = "Function ID")
    ),
    responses(
        (status = 200, description = "The function is stopped", body = SleepFunctionResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Function not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn sleep_function(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FunctionsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, function_id)): Path<(i32, i64)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, &state.project_access_checker);
    let (view, slept) = state.service.sleep(project_id, function_id).await?;
    let response = FunctionResponse::from(view);
    audit(&state, "FUNCTION_SLEPT", &auth, &metadata, &response).await;
    Ok(Json(SleepFunctionResponse {
        function: response,
        slept,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_map_to_their_status_codes() {
        let cases: Vec<(FunctionError, StatusCode)> = vec![
            (
                FunctionError::NotFound {
                    project_id: 1,
                    function_id: 2,
                },
                StatusCode::NOT_FOUND,
            ),
            (
                FunctionError::Validation {
                    message: "bad".into(),
                },
                StatusCode::BAD_REQUEST,
            ),
            (
                FunctionError::DuplicateName {
                    project_id: 1,
                    name: "a".into(),
                },
                StatusCode::CONFLICT,
            ),
            (
                FunctionError::PlaneUnavailable {
                    reason: "off".into(),
                },
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                FunctionError::Activation {
                    function_id: 2,
                    name: "a".into(),
                    source: ActivationError::Saturated {
                        key: crate::service::activation_key(2),
                        limit: 1,
                    },
                },
                StatusCode::SERVICE_UNAVAILABLE,
            ),
        ];
        for (error, status) in cases {
            let detail = error.to_string();
            let problem = Problem::from(error);
            assert_eq!(problem.status_code, status, "{detail}");
        }
    }

    #[test]
    fn the_create_body_limit_fits_the_largest_module_in_base64() {
        let encoded = base64::engine::general_purpose::STANDARD.encode(vec![0u8; MAX_WASM_BYTES]);
        assert!(encoded.len() < MAX_CREATE_BODY_BYTES);
    }
}
