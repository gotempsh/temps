// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Workspace file transfer: limits, git import, writes, uploads and downloads.

use super::*;

pub(super) const WORKSPACE_LIST_LIMIT_BYTES: usize = 256 * 1024;
pub(super) const WORKSPACE_DIFF_LIMIT_BYTES: usize = 256 * 1024;
pub(super) const WORKSPACE_MAX_FILES: usize = 1_000;
pub(super) const WORKSPACE_MAX_CHANGES: usize = 200;
pub(super) const WORKSPACE_MAX_DIFF_FILE_BYTES: u64 = 512 * 1024;
pub(super) const WORKSPACE_DEFAULT_PAGE_SIZE: usize = 100;
pub(super) const WORKSPACE_MAX_PAGE_SIZE: usize = 200;
pub(super) const WORKSPACE_COMMAND_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(10);
pub(super) const WORKSPACE_REQUEST_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(30);
pub(super) const MIB: usize = 1024 * 1024;
pub(super) const WORKSPACE_IMPORT_ABSOLUTE_MAX_FILES_PER_REQUEST: usize = 100;
pub(super) const WORKSPACE_IMPORT_ABSOLUTE_MAX_BYTES_PER_REQUEST: usize = 32 * MIB;
const WORKSPACE_ABSOLUTE_MAX_BYTES: u64 = 2_048 * 1024 * 1024;
const WORKSPACE_ABSOLUTE_MAX_ENTRIES: usize = 50_000;
pub(super) const WORKSPACE_ABSOLUTE_MAX_TEXT_PREVIEW_BYTES: usize = MIB;
pub(super) const WORKSPACE_ABSOLUTE_MAX_IMAGE_PREVIEW_BYTES: usize = 16 * MIB;
const WORKSPACE_ABSOLUTE_MAX_DOWNLOAD_BYTES: usize = 32 * MIB;
// The request carries file bytes as base64 plus bounded paths and JSON syntax.
// Keep this route-specific limit above that encoded maximum while retaining a
// small, explicit cap rather than inheriting Axum's 2 MiB JSON default.
pub(super) const WORKSPACE_IMPORT_BODY_LIMIT: usize = 46 * MIB;
type WorkspaceFileWrite = (std::path::PathBuf, Vec<u8>, Option<u32>);

pub(super) async fn workspace_file_limits(
    state: &AppState,
) -> Result<temps_core::AiWorkspaceFileLimitsSettings, Problem> {
    let Some(config) = &state.config_service else {
        return Ok(temps_core::AiWorkspaceFileLimitsSettings::default());
    };
    config
        .get_settings()
        .await
        .map(|settings| effective_workspace_file_limits(settings.ai_workspace_file_limits))
        .map_err(|error| {
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Workspace File Policy Unavailable")
                .with_detail(format!("Could not load the workspace file limits: {error}"))
        })
}

pub(super) fn effective_workspace_file_limits(
    mut limits: temps_core::AiWorkspaceFileLimitsSettings,
) -> temps_core::AiWorkspaceFileLimitsSettings {
    limits.max_files_per_upload = limits
        .max_files_per_upload
        .min(WORKSPACE_IMPORT_ABSOLUTE_MAX_FILES_PER_REQUEST as u32);
    limits.max_file_size_mb = limits.max_file_size_mb.min(32);
    limits.max_upload_size_mb = limits.max_upload_size_mb.min(32);
    limits.max_workspace_size_mb = limits.max_workspace_size_mb.min(2_048);
    limits.max_workspace_entries = limits.max_workspace_entries.min(50_000);
    limits.max_text_preview_kb = limits.max_text_preview_kb.min(1_024);
    limits.max_image_preview_size_mb = limits.max_image_preview_size_mb.min(16);
    limits.max_download_size_mb = limits.max_download_size_mb.min(32);
    limits
}

pub(super) fn require_workspace_file_policy_access(auth: &AuthContext) -> Result<(), Problem> {
    permission_guard!(auth, ProjectsRead);
    permission_guard!(auth, SandboxesRead);
    deny_deployment_token!(auth);
    Ok(())
}

#[utoipa::path(
    get, tag = "AI Chat",
    path = "/ai/workspace/file-limits",
    operation_id = "get_workspace_file_limits",
    summary = "Get effective AI workspace file limits",
    description = "Returns the non-sensitive effective transfer and preview policy for the current workspace user.",
    responses((status = 200, body = temps_core::AiWorkspaceFileLimitsSettings), (status = 401), (status = 403), (status = 500)),
    security(("bearer_auth" = []))
)]
pub async fn get_workspace_file_limits(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Result<Json<temps_core::AiWorkspaceFileLimitsSettings>, Problem> {
    require_workspace_file_policy_access(&auth)?;
    Ok(Json(workspace_file_limits(&state).await?))
}

pub(super) fn mb_bytes(value: u32) -> usize {
    usize::try_from(value)
        .unwrap_or(usize::MAX / MIB)
        .saturating_mul(MIB)
}

pub(super) fn validate_workspace_import_path(path: &str) -> Result<(), Problem> {
    if path.is_empty()
        || path.len() > 512
        || !FsPath::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        || is_sensitive_workspace_path(path)
    {
        return Err(Problem::from(
            temps_sandbox::error::SandboxError::Validation {
                message: format!(
                "workspace import path '{}' must be a safe project-relative, non-sensitive path",
                path
            ),
            },
        ));
    }
    Ok(())
}

pub(super) fn decode_workspace_file_writes(
    request: WriteApplicationWorkspaceFilesRequest,
    limits: &temps_core::AiWorkspaceFileLimitsSettings,
) -> Result<Vec<WorkspaceFileWrite>, Problem> {
    let max_files = usize::try_from(limits.max_files_per_upload)
        .unwrap_or(WORKSPACE_IMPORT_ABSOLUTE_MAX_FILES_PER_REQUEST)
        .min(WORKSPACE_IMPORT_ABSOLUTE_MAX_FILES_PER_REQUEST);
    let max_file_bytes =
        mb_bytes(limits.max_file_size_mb).min(WORKSPACE_IMPORT_ABSOLUTE_MAX_BYTES_PER_REQUEST);
    let max_request_bytes =
        mb_bytes(limits.max_upload_size_mb).min(WORKSPACE_IMPORT_ABSOLUTE_MAX_BYTES_PER_REQUEST);
    if request.files.len() > max_files {
        return Err(Problem::from(
            temps_sandbox::error::SandboxError::Validation {
                message: format!(
                    "workspace import accepts at most {} files per request",
                    max_files
                ),
            },
        ));
    }
    let mut total_bytes = 0usize;
    let mut entries = Vec::with_capacity(request.files.len());
    let mut paths = std::collections::HashSet::with_capacity(request.files.len());
    for file in request.files {
        validate_workspace_import_path(&file.path)?;
        if !paths.insert(file.path.clone()) {
            return Err(Problem::from(
                temps_sandbox::error::SandboxError::Validation {
                    message: format!(
                        "workspace import contains the duplicate path '{}'",
                        file.path
                    ),
                },
            ));
        }
        let contents = B64.decode(file.contents_b64.as_bytes()).map_err(|error| {
            Problem::from(temps_sandbox::error::SandboxError::Validation {
                message: format!(
                    "contents_b64 for '{}' is not valid base64: {}",
                    file.path, error
                ),
            })
        })?;
        if contents.len() > max_file_bytes {
            return Err(Problem::from(
                temps_sandbox::error::SandboxError::Validation {
                    message: format!(
                        "workspace file '{}' exceeds the configured {} byte file limit",
                        file.path, max_file_bytes
                    ),
                },
            ));
        }
        total_bytes = total_bytes.checked_add(contents.len()).ok_or_else(|| {
            Problem::from(temps_sandbox::error::SandboxError::Validation {
                message: "workspace import byte count overflowed".to_string(),
            })
        })?;
        if total_bytes > max_request_bytes {
            return Err(Problem::from(
                temps_sandbox::error::SandboxError::Validation {
                    message: format!(
                        "workspace import accepts at most {} bytes per request",
                        max_request_bytes
                    ),
                },
            ));
        }
        entries.push((FsPath::new(&file.path).to_path_buf(), contents, file.mode));
    }
    for path in &paths {
        let path = FsPath::new(path);
        if path
            .ancestors()
            .skip(1)
            .filter_map(FsPath::to_str)
            .filter(|ancestor| !ancestor.is_empty())
            .any(|ancestor| paths.contains(ancestor))
        {
            return Err(Problem::from(
                temps_sandbox::error::SandboxError::Validation {
                    message: format!(
                        "workspace import path '{}' conflicts with another file in the same batch",
                        path.display()
                    ),
                },
            ));
        }
    }
    Ok(entries)
}

fn application_project(
    application: &crate::applications::ApplicationWithProjects,
    project_id: i32,
) -> Result<&temps_entities::projects::Model, Problem> {
    application
        .projects
        .iter()
        .find(|project| project.id == project_id)
        .ok_or_else(|| {
            problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Application Project Not Found")
                .with_detail(format!(
                    "Project {} is not linked to this application.",
                    project_id
                ))
        })
}

#[utoipa::path(
    post, tag = "AI Applications",
    path = "/ai/applications/{application_public_id}/projects/{project_id}/workspace/source",
    operation_id = "import_application_workspace_git",
    summary = "Import a Git repository into an application project",
    description = "Re-authorizes every linked project, resolves an optional user-owned Git connection server-side, and shallow-clones into the selected project directory.",
    params(
        ("application_public_id" = String, Path,),
        ("project_id" = i32, Path,),
    ),
    request_body = ImportApplicationWorkspaceGitRequest,
    responses((status = 204), (status = 400), (status = 401), (status = 403), (status = 404), (status = 409), (status = 500)),
    security(("bearer_auth" = []))
)]
pub async fn import_application_workspace_git(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((application_public_id, project_id)): Path<(String, i32)>,
    Json(request): Json<ImportApplicationWorkspaceGitRequest>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, ProjectsWrite);
    permission_guard!(auth, SandboxesWrite);
    if request.git_connection_id.is_some() {
        permission_guard!(auth, GitRepositoriesRead);
    }
    deny_deployment_token!(auth);
    let (application, sandbox_public_id) =
        application_workspace_sandbox(&state, &auth, &application_public_id).await?;
    let project = application_project(&application, project_id)?;
    let sandboxes = state.application_sandboxes.as_ref().ok_or_else(|| {
        problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
            .with_title("Application Sandbox Unavailable")
            .with_detail("The instance sandbox service is not configured.")
    })?;
    let used_git_connection = request.git_connection_id.is_some();
    let source = temps_sandbox::services::SandboxSource::Git {
        url: request.url,
        revision: request.revision,
        depth: request.depth.or(Some(1)),
        username: None,
        password: None,
        git_connection_id: request.git_connection_id,
        destination: Some(format!("projects/{}", project.slug)),
        strip_git_metadata: true,
    };
    sandboxes
        .clone_source(&sandbox_public_id, auth.user_id(), &source)
        .await
        .map_err(Problem::from)?;
    state
        .audit(&ApplicationWorkspaceSourceImportedAudit {
            context: audit_context(&auth, &metadata),
            application_id: application_public_id,
            project_id,
            used_git_connection,
        })
        .await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post, tag = "AI Applications",
    path = "/ai/applications/{application_public_id}/projects/{project_id}/workspace/files",
    operation_id = "write_application_workspace_files",
    summary = "Write a bounded batch of local files into an application project",
    description = "Re-authorizes every linked project and writes a configured, bounded batch of project-relative files into the selected persistent workspace directory.",
    params(
        ("application_public_id" = String, Path,),
        ("project_id" = i32, Path,),
    ),
    request_body = WriteApplicationWorkspaceFilesRequest,
    responses((status = 200, body = WriteApplicationWorkspaceFilesResponse), (status = 400), (status = 401), (status = 403), (status = 404), (status = 409)),
    security(("bearer_auth" = []))
)]
pub async fn write_application_workspace_files(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((application_public_id, project_id)): Path<(String, i32)>,
    Json(request): Json<WriteApplicationWorkspaceFilesRequest>,
) -> Result<Json<WriteApplicationWorkspaceFilesResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    permission_guard!(auth, SandboxesWrite);
    deny_deployment_token!(auth);
    let (application, _sandbox_public_id) =
        application_workspace_sandbox(&state, &auth, &application_public_id).await?;
    let project = application_project(&application, project_id)?;
    let limits = workspace_file_limits(&state).await?;
    let entries = decode_workspace_file_writes(request, &limits)?;
    let written = state
        .application_workspaces
        .store_project_files_bounded(
            &application.application.public_id,
            &project.slug,
            entries,
            u64::try_from(mb_bytes(limits.max_workspace_size_mb))
                .unwrap_or(WORKSPACE_ABSOLUTE_MAX_BYTES)
                .min(WORKSPACE_ABSOLUTE_MAX_BYTES),
            usize::try_from(limits.max_workspace_entries)
                .unwrap_or(WORKSPACE_ABSOLUTE_MAX_ENTRIES)
                .min(WORKSPACE_ABSOLUTE_MAX_ENTRIES),
        )
        .await
        .map_err(Problem::from)?;
    state
        .audit(&ApplicationWorkspaceFilesWrittenAudit {
            context: audit_context(&auth, &metadata),
            application_id: application_public_id,
            project_id: Some(project_id),
            file_count: written,
        })
        .await;
    Ok(Json(WriteApplicationWorkspaceFilesResponse { written }))
}

async fn store_workspace_files(
    state: &AppState,
    workspace_id: &str,
    request: WriteApplicationWorkspaceFilesRequest,
) -> Result<usize, Problem> {
    let limits = workspace_file_limits(state).await?;
    let entries = decode_workspace_file_writes(request, &limits)?;
    state
        .application_workspaces
        .store_workspace_files_bounded(
            workspace_id,
            entries,
            u64::try_from(mb_bytes(limits.max_workspace_size_mb))
                .unwrap_or(WORKSPACE_ABSOLUTE_MAX_BYTES)
                .min(WORKSPACE_ABSOLUTE_MAX_BYTES),
            usize::try_from(limits.max_workspace_entries)
                .unwrap_or(WORKSPACE_ABSOLUTE_MAX_ENTRIES)
                .min(WORKSPACE_ABSOLUTE_MAX_ENTRIES),
        )
        .await
        .map_err(Problem::from)
}

#[utoipa::path(
    post, tag = "AI Applications",
    path = "/ai/applications/{application_public_id}/workspace/files",
    operation_id = "upload_application_workspace_files",
    summary = "Upload files into an application workspace",
    description = "Writes a configured, bounded batch of safe workspace-relative files into the persistent workspace without waking compute.",
    params(("application_public_id" = String, Path,)),
    request_body = WriteApplicationWorkspaceFilesRequest,
    responses((status = 200, body = WriteApplicationWorkspaceFilesResponse), (status = 400), (status = 401), (status = 403), (status = 404), (status = 413), (status = 500)),
    security(("bearer_auth" = []))
)]
pub async fn upload_application_workspace_files(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(application_public_id): Path<String>,
    Json(request): Json<WriteApplicationWorkspaceFilesRequest>,
) -> Result<Json<WriteApplicationWorkspaceFilesResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    permission_guard!(auth, SandboxesWrite);
    deny_deployment_token!(auth);
    let application = authorized_application_for_permissions(
        &state,
        &auth,
        &application_public_id,
        Some("active"),
        &[Permission::ProjectsWrite, Permission::SandboxesWrite],
    )
    .await?;
    state
        .application_workspaces
        .ensure(&application.application.public_id, &application.projects)
        .await?;
    let written =
        store_workspace_files(&state, &application.application.public_id, request).await?;
    state
        .audit(&ApplicationWorkspaceFilesWrittenAudit {
            context: audit_context(&auth, &metadata),
            application_id: application_public_id,
            project_id: None,
            file_count: written,
        })
        .await;
    Ok(Json(WriteApplicationWorkspaceFilesResponse { written }))
}

#[utoipa::path(
    post, tag = "AI Chat",
    path = "/ai/workspace/files",
    operation_id = "upload_global_workspace_files",
    summary = "Upload files into the global AI workspace",
    description = "Writes a configured, bounded batch of safe workspace-relative files into the user's persistent global workspace without waking compute.",
    request_body = WriteApplicationWorkspaceFilesRequest,
    responses((status = 200, body = WriteApplicationWorkspaceFilesResponse), (status = 400), (status = 401), (status = 403), (status = 413), (status = 500)),
    security(("bearer_auth" = []))
)]
pub async fn upload_global_workspace_files(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<WriteApplicationWorkspaceFilesRequest>,
) -> Result<Json<WriteApplicationWorkspaceFilesResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    permission_guard!(auth, SandboxesWrite);
    deny_deployment_token!(auth);
    let workspace_id = global_workspace_context_id(auth.user_id());
    state
        .application_workspaces
        .ensure(&workspace_id, &[])
        .await?;
    let written = store_workspace_files(&state, &workspace_id, request).await?;
    state
        .audit(&GlobalWorkspaceFilesWrittenAudit {
            context: audit_context(&auth, &metadata),
            workspace_id,
            file_count: written,
        })
        .await;
    Ok(Json(WriteApplicationWorkspaceFilesResponse { written }))
}

async fn workspace_file_download_response(
    state: &AppState,
    workspace_id: &str,
    path: String,
) -> Result<Response, Problem> {
    let limits = workspace_file_limits(state).await?;
    let max_bytes =
        mb_bytes(limits.max_download_size_mb).min(WORKSPACE_ABSOLUTE_MAX_DOWNLOAD_BYTES);
    let download = state
        .application_workspaces
        .open_file_download(workspace_id, &path, max_bytes)
        .await?;
    let file_name = FsPath::new(&path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace-file");
    let safe_file_name = file_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let disposition = format!("attachment; filename=\"{safe_file_name}\"");
    let stream = ReaderStream::new(download.file.take(max_bytes as u64));
    let mut response = Response::new(Body::from_stream(stream));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    response.headers_mut().insert(
        CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition).map_err(|error| {
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Invalid Workspace File Name")
                .with_detail(format!("Could not encode the download file name: {error}"))
        })?,
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

#[utoipa::path(
    get, tag = "AI Applications",
    path = "/ai/applications/{application_public_id}/workspace/file/download",
    operation_id = "download_application_workspace_file",
    summary = "Download one application workspace file",
    params(("application_public_id" = String, Path,), ("path" = String, Query,)),
    responses((status = 200, body = Vec<u8>, content_type = "application/octet-stream"), (status = 400), (status = 401), (status = 403), (status = 404), (status = 413), (status = 500)),
    security(("bearer_auth" = []))
)]
pub async fn download_application_workspace_file(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(application_public_id): Path<String>,
    Query(query): Query<ApplicationWorkspaceFileQuery>,
) -> Result<Response, Problem> {
    permission_guard!(auth, ProjectsRead);
    permission_guard!(auth, SandboxesRead);
    deny_deployment_token!(auth);
    let application = authorized_application_for_permissions(
        &state,
        &auth,
        &application_public_id,
        Some("active"),
        &[Permission::ProjectsRead, Permission::SandboxesRead],
    )
    .await?;
    state
        .application_workspaces
        .ensure(&application.application.public_id, &application.projects)
        .await?;
    workspace_file_download_response(&state, &application.application.public_id, query.path).await
}

#[utoipa::path(
    get, tag = "AI Chat",
    path = "/ai/workspace/file/download",
    operation_id = "download_global_workspace_file",
    summary = "Download one global AI workspace file",
    params(("path" = String, Query,)),
    responses((status = 200, body = Vec<u8>, content_type = "application/octet-stream"), (status = 400), (status = 401), (status = 403), (status = 404), (status = 413), (status = 500)),
    security(("bearer_auth" = []))
)]
pub async fn download_global_workspace_file(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Query(query): Query<ApplicationWorkspaceFileQuery>,
) -> Result<Response, Problem> {
    permission_guard!(auth, ProjectsRead);
    permission_guard!(auth, SandboxesRead);
    deny_deployment_token!(auth);
    let workspace_id = global_workspace_context_id(auth.user_id());
    state
        .application_workspaces
        .ensure(&workspace_id, &[])
        .await?;
    workspace_file_download_response(&state, &workspace_id, query.path).await
}
