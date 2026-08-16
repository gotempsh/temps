use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Extension, Json, Router,
};
use serde::Serialize;
use std::sync::Arc;
use temps_auth::{permission_guard, project_access_guard, RequireAuth};
use temps_core::{
    problemdetails::{self, Problem},
    RequestMetadata,
};
use tracing::error;
use utoipa::{OpenApi, ToSchema};

use crate::handlers::audit::{AuditContext, SourceFileUploadedAudit, SourceFilesDeletedAudit};

use crate::services::source_map_service::{
    SourceFileInfo, SourceMapError, SourceMapInfo, SourceMapService,
};

#[derive(OpenApi)]
#[openapi(
    paths(
        upload_source_map,
        list_source_maps,
        list_releases,
        delete_release_source_maps,
        delete_source_map,
        upload_source_file,
        list_source_files,
        delete_release_source_files,
    ),
    components(schemas(
        SourceMapResponse,
        SourceMapListResponse,
        ReleaseListResponse,
        DeleteResponse,
        SourceFileResponse,
        SourceFileListResponse,
    )),
    tags(
        (name = "source-maps", description = "Source map management for error symbolication")
    )
)]
pub struct SourceMapApiDoc;

#[derive(Clone)]
pub struct SourceMapAppState {
    pub source_map_service: Arc<SourceMapService>,
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
    /// Optional checker for team-based project access (human sessions only).
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

pub fn configure_source_map_routes() -> Router<Arc<SourceMapAppState>> {
    // Body-size caps that fire at the transport layer, before the multipart
    // extractor buffers the request into memory. The per-field checks in the
    // handlers are defense-in-depth on top of these.
    const SOURCE_MAP_UPLOAD_LIMIT: usize = 50 * 1024 * 1024;
    const SOURCE_FILE_UPLOAD_LIMIT: usize = 10 * 1024 * 1024;

    Router::new()
        .route(
            "/projects/{project_id}/releases/{release}/source-maps",
            post(upload_source_map)
                .get(list_source_maps)
                .delete(delete_release_source_maps)
                .layer(DefaultBodyLimit::max(SOURCE_MAP_UPLOAD_LIMIT)),
        )
        .route(
            "/projects/{project_id}/source-map-releases",
            get(list_releases),
        )
        .route(
            "/projects/{project_id}/source-maps/{source_map_id}",
            delete(delete_source_map),
        )
        // Raw source files for native (Go/Rust/etc.) symbolication.
        .route(
            "/projects/{project_id}/releases/{release}/source-files",
            post(upload_source_file)
                .get(list_source_files)
                .delete(delete_release_source_files)
                .layer(DefaultBodyLimit::max(SOURCE_FILE_UPLOAD_LIMIT)),
        )
}

#[derive(Serialize, ToSchema)]
pub struct SourceMapResponse {
    pub id: i32,
    pub project_id: i32,
    pub release: String,
    pub file_path: String,
    pub dist: Option<String>,
    pub size_bytes: i64,
    pub checksum: Option<String>,
    #[schema(example = "2025-10-12T12:15:47.609192Z")]
    pub created_at: String,
}

impl From<SourceMapInfo> for SourceMapResponse {
    fn from(info: SourceMapInfo) -> Self {
        Self {
            id: info.id,
            project_id: info.project_id,
            release: info.release,
            file_path: info.file_path,
            dist: info.dist,
            size_bytes: info.size_bytes,
            checksum: info.checksum,
            created_at: info.created_at.to_rfc3339(),
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct SourceMapListResponse {
    pub source_maps: Vec<SourceMapResponse>,
    pub total: usize,
}

#[derive(Serialize, ToSchema)]
pub struct ReleaseListResponse {
    pub releases: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct DeleteResponse {
    pub deleted: u64,
}

impl From<SourceMapError> for Problem {
    fn from(error: SourceMapError) -> Self {
        match error {
            SourceMapError::NotFound { release, file_path } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Source Map Not Found")
                    .with_detail(format!(
                        "No source map found for release '{}' and file '{}'",
                        release, file_path
                    ))
            }
            SourceMapError::ParseError(msg) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Source Map")
                .with_detail(msg),
            SourceMapError::Validation(msg) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Validation Error")
                .with_detail(msg),
            SourceMapError::Database(e) => {
                error!("Source map database error: {}", e);
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Error")
                    .with_detail("An internal error occurred")
            }
        }
    }
}

/// Upload a source map for a release.
///
/// Accepts a multipart form with:
/// - `file`: The .map file (required)
/// - `file_path`: The URL path of the minified file as it appears in stack traces (required).
///   Uses the ~ prefix convention (e.g., "~/assets/main.js").
///   If a full URL is provided, it will be normalized automatically.
/// - `dist`: Optional distribution identifier
///
/// If a source map already exists for the same (project, release, file_path), it is replaced.
#[utoipa::path(
    tag = "source-maps",
    post,
    path = "/projects/{project_id}/releases/{release}/source-maps",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("release" = String, Path, description = "Release version"),
    ),
    responses(
        (status = 201, description = "Source map uploaded", body = SourceMapResponse),
        (status = 400, description = "Invalid source map or missing fields"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 413, description = "Source map too large"),
    ),
    security(("bearer_auth" = []))
)]
async fn upload_source_map(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SourceMapAppState>>,
    Path((project_id, release)): Path<(i32, String)>,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ErrorTrackingCreate);
    project_access_guard!(auth, project_id, state.project_access_checker);

    // Maximum source map size: 50MB
    const MAX_SOURCE_MAP_SIZE: usize = 50 * 1024 * 1024;

    let mut file_data: Option<Vec<u8>> = None;
    let mut file_path: Option<String> = None;
    let mut dist: Option<String> = None;

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Multipart Error")
            .with_detail(e.to_string())
    })? {
        let name = field.name().unwrap_or_default().to_string();

        match name.as_str() {
            "file" => {
                // If file_path wasn't explicitly set, derive from the uploaded filename
                if file_path.is_none() {
                    if let Some(filename) = field.file_name() {
                        // Strip the .map extension to get the minified file path
                        let source_file = filename.strip_suffix(".map").unwrap_or(filename);
                        file_path = Some(source_file.to_string());
                    }
                }

                let data = field.bytes().await.map_err(|e| {
                    problemdetails::new(StatusCode::BAD_REQUEST)
                        .with_title("File Read Error")
                        .with_detail(e.to_string())
                })?;

                if data.len() > MAX_SOURCE_MAP_SIZE {
                    return Err(problemdetails::new(StatusCode::PAYLOAD_TOO_LARGE)
                        .with_title("Source Map Too Large")
                        .with_detail(format!(
                            "Source map size {} bytes exceeds maximum of {} bytes",
                            data.len(),
                            MAX_SOURCE_MAP_SIZE
                        )));
                }

                file_data = Some(data.to_vec());
            }
            "file_path" | "name" => {
                let value = field.text().await.map_err(|e| {
                    problemdetails::new(StatusCode::BAD_REQUEST)
                        .with_title("Field Read Error")
                        .with_detail(e.to_string())
                })?;
                file_path = Some(value);
            }
            "dist" => {
                let value = field.text().await.map_err(|e| {
                    problemdetails::new(StatusCode::BAD_REQUEST)
                        .with_title("Field Read Error")
                        .with_detail(e.to_string())
                })?;
                if !value.is_empty() {
                    dist = Some(value);
                }
            }
            _ => {
                // Ignore unknown fields
            }
        }
    }

    let source_map_data = file_data.ok_or_else(|| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Missing File")
            .with_detail("No source map file was provided in the multipart request")
    })?;

    let file_path = file_path.ok_or_else(|| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Missing File Path")
            .with_detail(
                "No file_path was provided. Set the 'file_path' or 'name' form field, \
                 or upload a file with a .map extension",
            )
    })?;

    let info = state
        .source_map_service
        .upload(project_id, &release, &file_path, source_map_data, dist)
        .await?;

    Ok((StatusCode::CREATED, Json(SourceMapResponse::from(info))))
}

/// List all source maps for a specific release
#[utoipa::path(
    tag = "source-maps",
    get,
    path = "/projects/{project_id}/releases/{release}/source-maps",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("release" = String, Path, description = "Release version"),
    ),
    responses(
        (status = 200, description = "List of source maps", body = SourceMapListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
async fn list_source_maps(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SourceMapAppState>>,
    Path((project_id, release)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let maps = state
        .source_map_service
        .list_for_release(project_id, &release)
        .await?;

    let total = maps.len();
    let source_maps: Vec<SourceMapResponse> = maps.into_iter().map(Into::into).collect();

    Ok(Json(SourceMapListResponse { source_maps, total }))
}

/// List all releases that have source maps for a project
#[utoipa::path(
    tag = "source-maps",
    get,
    path = "/projects/{project_id}/source-map-releases",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
    ),
    responses(
        (status = 200, description = "List of releases", body = ReleaseListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
async fn list_releases(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SourceMapAppState>>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let releases = state.source_map_service.list_releases(project_id).await?;

    Ok(Json(ReleaseListResponse { releases }))
}

/// Delete all source maps for a specific release
#[utoipa::path(
    tag = "source-maps",
    delete,
    path = "/projects/{project_id}/releases/{release}/source-maps",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("release" = String, Path, description = "Release version"),
    ),
    responses(
        (status = 200, description = "Source maps deleted", body = DeleteResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
async fn delete_release_source_maps(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SourceMapAppState>>,
    Path((project_id, release)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let deleted = state
        .source_map_service
        .delete_release(project_id, &release)
        .await?;

    Ok(Json(DeleteResponse { deleted }))
}

/// Delete a specific source map by ID
#[utoipa::path(
    tag = "source-maps",
    delete,
    path = "/projects/{project_id}/source-maps/{source_map_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("source_map_id" = i32, Path, description = "Source map ID"),
    ),
    responses(
        (status = 204, description = "Source map deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Source map not found"),
    ),
    security(("bearer_auth" = []))
)]
async fn delete_source_map(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SourceMapAppState>>,
    Path((project_id, source_map_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_access_guard!(auth, project_id, state.project_access_checker);

    state
        .source_map_service
        .delete_by_id(project_id, source_map_id)
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Source files (native symbolication: Go, Rust, Python, …)
//
// Same shape as source maps, but the artifact is the raw source file rather
// than a `.map`. Gated by the per-project `error_source_context_enabled`
// toggle so application source is only ever stored when explicitly opted in.
// ---------------------------------------------------------------------------

#[derive(Serialize, ToSchema)]
pub struct SourceFileResponse {
    pub id: i32,
    pub project_id: i32,
    pub release: String,
    pub file_path: String,
    pub size_bytes: i64,
    pub checksum: Option<String>,
    #[schema(example = "2025-10-12T12:15:47.609192Z")]
    pub created_at: String,
}

impl From<SourceFileInfo> for SourceFileResponse {
    fn from(info: SourceFileInfo) -> Self {
        Self {
            id: info.id,
            project_id: info.project_id,
            release: info.release,
            file_path: info.file_path,
            size_bytes: info.size_bytes,
            checksum: info.checksum,
            created_at: info.created_at.to_rfc3339(),
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct SourceFileListResponse {
    pub source_files: Vec<SourceFileResponse>,
    pub total: usize,
}

/// Problem returned when the project has not opted into source context.
fn source_context_disabled_problem() -> Problem {
    problemdetails::new(StatusCode::CONFLICT)
        .with_title("Source Context Disabled")
        .with_detail(
            "Native source context is disabled for this project. Enable it in \
             Settings → Error Tracking before uploading source files.",
        )
}

/// Upload a raw source file for a release (native symbolication).
///
/// Accepts a multipart form with:
/// - `file`: the source file bytes (required)
/// - `file_path`: the path of the file as it appears in stack frames (required;
///   derived from the uploaded filename if omitted). Normalized with the `~`
///   prefix convention, matching source-map storage.
///
/// Requires the project's `error_source_context_enabled` toggle to be on.
/// Upserts on (project, release, file_path).
#[utoipa::path(
    tag = "source-maps",
    post,
    path = "/projects/{project_id}/releases/{release}/source-files",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("release" = String, Path, description = "Release version"),
    ),
    responses(
        (status = 201, description = "Source file uploaded", body = SourceFileResponse),
        (status = 400, description = "Missing fields"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 409, description = "Source context disabled for project"),
        (status = 413, description = "Source file too large"),
    ),
    security(("bearer_auth" = []))
)]
async fn upload_source_file(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SourceMapAppState>>,
    Path((project_id, release)): Path<(i32, String)>,
    Extension(metadata): Extension<RequestMetadata>,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ErrorTrackingCreate);
    project_access_guard!(auth, project_id, state.project_access_checker);

    // Opt-in gate: never store application source unless the project enabled it.
    if !state
        .source_map_service
        .source_context_enabled(project_id)
        .await
    {
        return Err(source_context_disabled_problem());
    }

    // Maximum single source file size: 10MB (individual files, not bundles).
    const MAX_SOURCE_FILE_SIZE: usize = 10 * 1024 * 1024;

    let mut file_data: Option<Vec<u8>> = None;
    let mut file_path: Option<String> = None;

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Multipart Error")
            .with_detail(e.to_string())
    })? {
        let name = field.name().unwrap_or_default().to_string();

        match name.as_str() {
            "file" => {
                if file_path.is_none() {
                    if let Some(filename) = field.file_name() {
                        file_path = Some(filename.to_string());
                    }
                }

                let data = field.bytes().await.map_err(|e| {
                    problemdetails::new(StatusCode::BAD_REQUEST)
                        .with_title("File Read Error")
                        .with_detail(e.to_string())
                })?;

                if data.len() > MAX_SOURCE_FILE_SIZE {
                    return Err(problemdetails::new(StatusCode::PAYLOAD_TOO_LARGE)
                        .with_title("Source File Too Large")
                        .with_detail(format!(
                            "Source file size {} bytes exceeds maximum of {} bytes",
                            data.len(),
                            MAX_SOURCE_FILE_SIZE
                        )));
                }

                file_data = Some(data.to_vec());
            }
            "file_path" | "name" => {
                let value = field.text().await.map_err(|e| {
                    problemdetails::new(StatusCode::BAD_REQUEST)
                        .with_title("Field Read Error")
                        .with_detail(e.to_string())
                })?;
                file_path = Some(value);
            }
            _ => {}
        }
    }

    let content = file_data.ok_or_else(|| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Missing File")
            .with_detail("No source file was provided in the multipart request")
    })?;

    let file_path = file_path.ok_or_else(|| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Missing File Path")
            .with_detail(
                "No file_path was provided. Set the 'file_path' or 'name' form field, \
                 or upload a file with a filename",
            )
    })?;

    let info = state
        .source_map_service
        .upload_source_file(project_id, &release, &file_path, content)
        .await?;

    let audit = SourceFileUploadedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        release: release.clone(),
        file_path: info.file_path.clone(),
        size_bytes: info.size_bytes,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create source-file upload audit log: {}", e);
    }

    Ok((StatusCode::CREATED, Json(SourceFileResponse::from(info))))
}

/// List uploaded source files for a release (metadata only).
#[utoipa::path(
    tag = "source-maps",
    get,
    path = "/projects/{project_id}/releases/{release}/source-files",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("release" = String, Path, description = "Release version"),
    ),
    responses(
        (status = 200, description = "List of source files", body = SourceFileListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
async fn list_source_files(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SourceMapAppState>>,
    Path((project_id, release)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let files = state
        .source_map_service
        .list_source_files(project_id, &release)
        .await?;

    let total = files.len();
    let source_files: Vec<SourceFileResponse> = files.into_iter().map(Into::into).collect();

    Ok(Json(SourceFileListResponse {
        source_files,
        total,
    }))
}

/// Delete all uploaded source files for a release.
#[utoipa::path(
    tag = "source-maps",
    delete,
    path = "/projects/{project_id}/releases/{release}/source-files",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("release" = String, Path, description = "Release version"),
    ),
    responses(
        (status = 200, description = "Source files deleted", body = DeleteResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
async fn delete_release_source_files(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SourceMapAppState>>,
    Path((project_id, release)): Path<(i32, String)>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let deleted = state
        .source_map_service
        .delete_source_files_for_release(project_id, &release)
        .await?;

    let audit = SourceFilesDeletedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        release: release.clone(),
        deleted_count: deleted,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create source-file deletion audit log: {}", e);
    }

    Ok(Json(DeleteResponse { deleted }))
}
