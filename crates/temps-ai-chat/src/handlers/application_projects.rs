// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Projects inside an application: create (including a dropped source
//! archive), deploy, link, unlink and choose the primary project.

use super::*;

pub(super) fn validate_application_project_choice(
    has_starter_project: bool,
    linked_project_count: usize,
) -> Result<(), &'static str> {
    match (has_starter_project, linked_project_count > 0) {
        (true, true) => {
            Err("Create a workspace with either starter_project or project_ids, not both.")
        }
        (false, false) => {
            Err("Create a workspace with a starter_project or at least one existing project.")
        }
        _ => Ok(()),
    }
}

pub(super) async fn create_starter_project(
    project_service: &temps_projects::ProjectService,
    request: &CreateApplicationProjectRequest,
) -> Result<temps_projects::services::types::Project, temps_projects::services::types::ProjectError>
{
    project_service
        .create_project(temps_projects::services::types::CreateProjectRequest {
            cloudflare_enabled: None,
            delivery_provider: None,
            name: request.name.trim().to_string(),
            expected_slug: None,
            repo_name: None,
            repo_owner: None,
            directory: ".".to_string(),
            main_branch: "main".to_string(),
            preset: application_project_preset(request),
            preset_config: None,
            environment_variables: None,
            automatic_deploy: false,
            storage_service_ids: Vec::new(),
            storage_service_claim_ids: Vec::new(),
            storage_service_claim_user_id: None,
            is_public_repo: Some(false),
            git_url: None,
            git_provider_connection_id: None,
            exposed_port: request.exposed_port.or(Some(3000)),
            cpu_request: None,
            cpu_limit: None,
            memory_request: None,
            memory_limit: None,
            source_type: SourceType::UploadedSource,
            template_slug: None,
            hosting: Default::default(),
        })
        .await
}

pub(super) fn application_project_preset(request: &CreateApplicationProjectRequest) -> String {
    request
        .preset
        .clone()
        .unwrap_or_else(|| "autopack".to_string())
}

const MAX_APPLICATION_DROP_FILES: usize = 100_000;
const MAX_APPLICATION_DROP_BYTES: u64 = 500 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub(super) enum ApplicationDropArchiveError {
    #[error("workspace project directory does not exist: {0}")]
    Missing(String),
    #[error("Drop does not follow symbolic links: {0}")]
    Symlink(String),
    #[error("workspace contains more than {MAX_APPLICATION_DROP_FILES} files")]
    TooManyFiles,
    #[error("workspace source exceeds the {MAX_APPLICATION_DROP_BYTES} byte limit")]
    TooLarge,
    #[error("workspace path is not valid UTF-8: {0}")]
    InvalidPath(String),
    #[error("could not prepare workspace source archive: {0}")]
    Io(#[from] std::io::Error),
}

fn drop_ignored_directory(entry: &walkdir::DirEntry) -> bool {
    if entry.depth() == 0 || !entry.file_type().is_dir() {
        return false;
    }
    entry.file_name().to_str().is_some_and(|name| {
        name.eq_ignore_ascii_case("node_modules") || is_sensitive_workspace_path(name)
    })
}

fn drop_ignored_file(path: &FsPath) -> bool {
    path.to_str()
        .map(|value| value.replace(std::path::MAIN_SEPARATOR, "/"))
        .is_some_and(|path| is_sensitive_workspace_path(&path))
}

pub(super) fn canonical_workspace_subdirectory(
    workspace_root: &FsPath,
    relative: &FsPath,
) -> Result<std::path::PathBuf, ApplicationDropArchiveError> {
    if !relative
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(ApplicationDropArchiveError::InvalidPath(
            relative.display().to_string(),
        ));
    }

    #[cfg(unix)]
    {
        use rustix::fs::{open, openat, Mode, OFlags};

        let mut current = open(
            workspace_root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
        for component in relative.components() {
            let Component::Normal(component) = component else {
                unreachable!("relative components were validated above");
            };
            current = openat(
                &current,
                component,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
        }
    }

    let canonical_root = workspace_root.canonicalize()?;
    let canonical_project = workspace_root.join(relative).canonicalize()?;
    if !canonical_project.starts_with(&canonical_root) {
        return Err(ApplicationDropArchiveError::Symlink(
            relative.display().to_string(),
        ));
    }
    Ok(canonical_project)
}

#[cfg(unix)]
fn open_workspace_file(root: &FsPath, relative: &FsPath) -> std::io::Result<std::fs::File> {
    use rustix::fs::{open, openat, Mode, OFlags};
    use std::os::fd::OwnedFd;

    let mut current: OwnedFd = open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
    let components = relative.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(component) = component else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "source path contains a non-normal component",
            ));
        };
        let final_component = index + 1 == components.len();
        let flags = if final_component {
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC
        } else {
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
        };
        current = openat(&current, *component, flags, Mode::empty())
            .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
    }
    Ok(current.into())
}

#[cfg(not(unix))]
fn open_workspace_file(root: &FsPath, relative: &FsPath) -> std::io::Result<std::fs::File> {
    let canonical_root = root.canonicalize()?;
    let canonical_file = root.join(relative).canonicalize()?;
    if !canonical_file.starts_with(&canonical_root) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "source path escapes the application workspace",
        ));
    }
    std::fs::File::open(canonical_file)
}

pub(super) fn prepare_application_drop_archive(
    project_root: &FsPath,
) -> Result<tempfile::NamedTempFile, ApplicationDropArchiveError> {
    if !project_root.is_dir() {
        return Err(ApplicationDropArchiveError::Missing(
            project_root.display().to_string(),
        ));
    }
    let archive = tempfile::NamedTempFile::new()?;
    let mut writer = zip::ZipWriter::new(archive.reopen()?);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    let mut file_count = 0_usize;
    let mut source_bytes = 0_u64;
    let entries = walkdir::WalkDir::new(project_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| !drop_ignored_directory(entry));
    for entry in entries {
        let entry = entry.map_err(|error| {
            ApplicationDropArchiveError::Io(std::io::Error::other(error.to_string()))
        })?;
        let relative = entry.path().strip_prefix(project_root).map_err(|error| {
            ApplicationDropArchiveError::Io(std::io::Error::other(error.to_string()))
        })?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        if entry.file_type().is_symlink() {
            return Err(ApplicationDropArchiveError::Symlink(
                relative.display().to_string(),
            ));
        }
        if !entry.file_type().is_file() || drop_ignored_file(relative) {
            continue;
        }
        file_count += 1;
        if file_count > MAX_APPLICATION_DROP_FILES {
            return Err(ApplicationDropArchiveError::TooManyFiles);
        }
        let source = open_workspace_file(project_root, relative)?;
        let metadata = source.metadata()?;
        if !metadata.is_file() {
            return Err(ApplicationDropArchiveError::Symlink(
                relative.display().to_string(),
            ));
        }
        let archive_name = relative
            .to_str()
            .ok_or_else(|| {
                ApplicationDropArchiveError::InvalidPath(relative.display().to_string())
            })?
            .replace(std::path::MAIN_SEPARATOR, "/");
        writer
            .start_file(archive_name, options)
            .map_err(|error| ApplicationDropArchiveError::Io(std::io::Error::other(error)))?;
        let remaining = MAX_APPLICATION_DROP_BYTES.saturating_sub(source_bytes);
        let copied = std::io::copy(&mut source.take(remaining + 1), &mut writer)?;
        if copied > remaining {
            return Err(ApplicationDropArchiveError::TooLarge);
        }
        source_bytes = source_bytes.saturating_add(copied);
    }
    if file_count == 0 {
        return Err(ApplicationDropArchiveError::Missing(
            "the project directory contains no deployable files".to_string(),
        ));
    }
    writer
        .finish()
        .map_err(|error| ApplicationDropArchiveError::Io(std::io::Error::other(error)))?;
    if archive.as_file().metadata()?.len() > MAX_APPLICATION_DROP_BYTES {
        return Err(ApplicationDropArchiveError::TooLarge);
    }
    Ok(archive)
}

#[utoipa::path(
    post, tag = "AI Applications",
    path = "/ai/applications/{application_public_id}/projects",
    operation_id = "create_application_project",
    summary = "Create and link an application project",
    description = "Creates a Temps project, links it to the user-owned application, creates projects/<slug> in its persistent workspace, and refreshes the application topology in one approval-gated server workflow.",
    params(("application_public_id" = String, Path,)),
    request_body = CreateApplicationProjectRequest,
    responses((status = 201, body = ApplicationResponse), (status = 400), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn create_application_project(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(application_public_id): Path<String>,
    Json(request): Json<CreateApplicationProjectRequest>,
) -> Result<(StatusCode, Json<ApplicationResponse>), Problem> {
    permission_guard!(auth, ProjectsCreate);
    permission_guard!(auth, ProjectsWrite);
    permission_guard!(auth, SandboxesWrite);
    deny_deployment_token!(auth);
    let existing = state
        .applications
        .get(auth.user_id(), &application_public_id)
        .await?;
    let existing_project_ids = existing
        .projects
        .iter()
        .map(|project| project.id)
        .collect::<Vec<_>>();
    ensure_application_project_permission(
        &auth,
        &state.project_access_checker,
        &existing_project_ids,
        &Permission::ProjectsWrite,
    )
    .await?;
    ensure_application_project_permission(
        &auth,
        &state.project_access_checker,
        &existing_project_ids,
        &Permission::SandboxesWrite,
    )
    .await?;
    let _runtime_gate = state
        .service
        .lock_application_runtime_update(&application_public_id)
        .await
        .map_err(Problem::from)?;
    let project = create_starter_project(&state.project_service, &request)
        .await
        .map_err(Problem::from)?;
    let application = match state
        .applications
        .link_project(auth.user_id(), &application_public_id, project.id)
        .await
    {
        Ok(application) => application,
        Err(error) => {
            if let Err(cleanup_error) = state
                .project_service
                .delete_project(project.id, &project.name)
                .await
            {
                tracing::error!(
                    project_id = project.id,
                    error = %cleanup_error,
                    "Failed to clean up project after application link failed"
                );
            }
            return Err(error.into());
        }
    };
    if let Err(workspace_error) = state
        .application_workspaces
        .ensure(&application.application.public_id, &application.projects)
        .await
    {
        match state
            .applications
            .unlink_project(auth.user_id(), &application_public_id, project.id)
            .await
        {
            Ok(_) => {
                if let Err(cleanup_error) = state
                    .project_service
                    .delete_project(project.id, &project.name)
                    .await
                {
                    tracing::error!(
                        project_id = project.id,
                        error = %cleanup_error,
                        "Failed to clean up project after workspace creation failed"
                    );
                }
            }
            Err(cleanup_error) => {
                tracing::error!(
                    project_id = project.id,
                    error = %cleanup_error,
                    "Failed to roll back application project after workspace creation failed"
                );
            }
        }
        return Err(workspace_error.into());
    }
    if let Some(sandboxes) = state.application_sandboxes.as_ref() {
        if let Err(permission_error) = sandboxes
            .normalize_application_project_permissions(
                auth.user_id(),
                &application_public_id,
                &project.slug,
            )
            .await
        {
            match state
                .applications
                .unlink_project(auth.user_id(), &application_public_id, project.id)
                .await
            {
                Ok(_) => {
                    if let Err(cleanup_error) = state
                        .project_service
                        .delete_project(project.id, &project.name)
                        .await
                    {
                        tracing::error!(
                            project_id = project.id,
                            error = %cleanup_error,
                            "Failed to clean up project after workspace permission update failed"
                        );
                    }
                }
                Err(cleanup_error) => {
                    tracing::error!(
                        project_id = project.id,
                        error = %cleanup_error,
                        "Failed to roll back project link after workspace permission update failed"
                    );
                }
            }
            return Err(permission_error.into());
        }
    }
    if let Err(network_error) =
        synchronize_application_network_if_running(&state, &auth, &application).await
    {
        match state
            .applications
            .unlink_project(auth.user_id(), &application_public_id, project.id)
            .await
        {
            Ok(_) => {
                if let Err(cleanup_error) = state
                    .project_service
                    .delete_project(project.id, &project.name)
                    .await
                {
                    tracing::error!(
                        project_id = project.id,
                        error = %cleanup_error,
                        "Failed to clean up project after database network update failed"
                    );
                }
            }
            Err(cleanup_error) => {
                tracing::error!(
                    project_id = project.id,
                    error = %cleanup_error,
                    "Failed to roll back project after database network update failed"
                );
            }
        }
        if let Err(reconcile_error) =
            synchronize_application_network_if_running(&state, &auth, &existing).await
        {
            tracing::error!(
                application_id = application_public_id,
                error = ?reconcile_error,
                "Failed to restore application data network after project-create rollback"
            );
        }
        return Err(network_error);
    }
    state
        .audit(&ApplicationTopologyChangedAudit {
            context: audit_context(&auth, &metadata),
            application_id: application_public_id,
            action: "create_project".to_string(),
            project_id: project.id,
        })
        .await;
    Ok((
        StatusCode::CREATED,
        Json(ApplicationResponse::from(application)),
    ))
}

#[utoipa::path(
    post, tag = "AI Applications",
    path = "/ai/applications/{application_public_id}/projects/{project_id}/deploy",
    operation_id = "deploy_application_workspace_project",
    summary = "Deploy an application workspace project with Drop",
    description = "Packages projects/<slug> from the application's persistent workspace and starts the existing Temps uploaded-source Drop workflow. The operation is exposed to chat through temps_write and therefore follows the active native approval mode.",
    params(
        ("application_public_id" = String, Path,),
        ("project_id" = i32, Path,),
    ),
    request_body = DeployApplicationProjectRequest,
    responses(
        (status = 202, body = ApplicationProjectDeploymentResponse),
        (status = 400), (status = 401), (status = 403), (status = 404),
        (status = 409), (status = 413), (status = 503)
    ),
    security(("bearer_auth" = []))
)]
pub async fn deploy_application_workspace_project(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((application_public_id, project_id)): Path<(String, i32)>,
    Json(request): Json<DeployApplicationProjectRequest>,
) -> Result<(StatusCode, Json<ApplicationProjectDeploymentResponse>), Problem> {
    permission_guard!(auth, ProjectsRead);
    permission_guard!(auth, ProjectsWrite);
    permission_guard!(auth, DeploymentsCreate);
    deny_deployment_token!(auth);
    let application = state
        .applications
        .get(auth.user_id(), &application_public_id)
        .await?;
    ensure_application_project_permission(
        &auth,
        &state.project_access_checker,
        &[project_id],
        &Permission::DeploymentsCreate,
    )
    .await?;
    ensure_application_project_permission(
        &auth,
        &state.project_access_checker,
        &[project_id],
        &Permission::ProjectsWrite,
    )
    .await?;
    let project = application
        .projects
        .iter()
        .find(|project| project.id == project_id)
        .cloned()
        .ok_or_else(|| {
            problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Application Project Not Found")
                .with_detail(format!(
                    "Project {project_id} is not linked to application '{application_public_id}'"
                ))
        })?;

    let workspace = state
        .application_workspaces
        .ensure(&application.application.public_id, &application.projects)
        .await?;
    let project_relative = FsPath::new("projects").join(&project.slug);
    let project_root =
        canonical_workspace_subdirectory(&workspace.host_work_dir, &project_relative).map_err(
            |error| {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Workspace Project Path Rejected")
                    .with_detail(error.to_string())
            },
        )?;
    let archive =
        tokio::task::spawn_blocking(move || prepare_application_drop_archive(&project_root))
            .await
            .map_err(|error| {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Workspace Archive Failed")
                    .with_detail(format!("Workspace archive task failed: {error}"))
            })?
            .map_err(|error| {
                let status = match error {
                    ApplicationDropArchiveError::TooManyFiles
                    | ApplicationDropArchiveError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
                    _ => StatusCode::BAD_REQUEST,
                };
                problemdetails::new(status)
                    .with_title("Workspace Archive Failed")
                    .with_detail(error.to_string())
            })?;
    let deployer = state.source_drop_deployer.as_ref().ok_or_else(|| {
        problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
            .with_title("Drop Deployment Unavailable")
            .with_detail("The deployments plugin is not available on this Temps instance")
    })?;
    let deployed = deployer
        .deploy_source_drop(temps_core::SourceDropRequest {
            project_id,
            environment_id: request.environment_id,
            archive_path: archive.path().to_path_buf(),
            original_filename: format!("{}.zip", project.slug),
            promote_manual_source: true,
        })
        .await
        .map_err(|error| {
            let status = source_drop_error_status(&error);
            problemdetails::new(status)
                .with_title("Workspace Drop Failed")
                .with_detail(error.to_string())
        })?;
    state
        .audit(&ApplicationWorkspaceDeployedAudit {
            context: audit_context(&auth, &metadata),
            application_id: application_public_id,
            project_id,
            environment_id: deployed.environment_id,
            deployment_id: deployed.id,
        })
        .await;
    Ok((
        StatusCode::ACCEPTED,
        Json(ApplicationProjectDeploymentResponse {
            id: deployed.id,
            project_id: deployed.project_id,
            environment_id: deployed.environment_id,
            slug: deployed.slug,
            state: deployed.state,
            source_type: "uploaded_source".to_string(),
        }),
    ))
}

pub(super) fn source_drop_error_status(error: &temps_core::SourceDropError) -> StatusCode {
    match error {
        temps_core::SourceDropError::ProjectNotFound { .. }
        | temps_core::SourceDropError::EnvironmentNotFound { .. }
        | temps_core::SourceDropError::NoEnvironment { .. } => StatusCode::NOT_FOUND,
        temps_core::SourceDropError::SourceNotAllowed { .. }
        | temps_core::SourceDropError::InvalidArchive { .. } => StatusCode::BAD_REQUEST,
        temps_core::SourceDropError::UnsupportedInStateless { .. } => StatusCode::CONFLICT,
        temps_core::SourceDropError::ArchiveTooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
        temps_core::SourceDropError::Storage { .. }
        | temps_core::SourceDropError::Database { .. }
        | temps_core::SourceDropError::Workflow { .. }
        | temps_core::SourceDropError::Queue { .. }
        | temps_core::SourceDropError::Compensation { .. } => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[utoipa::path(
    post, tag = "AI Applications",
    path = "/ai/applications/{application_public_id}/projects/link",
    operation_id = "link_application_project",
    summary = "Link an existing project to an application",
    params(("application_public_id" = String, Path,)), request_body = LinkApplicationProjectRequest,
    responses((status = 200, body = ApplicationResponse), (status = 401), (status = 403), (status = 404), (status = 409)),
    security(("bearer_auth" = []))
)]
pub async fn link_application_project(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(application_public_id): Path<String>,
    Json(request): Json<LinkApplicationProjectRequest>,
) -> Result<Json<ApplicationResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    permission_guard!(auth, SandboxesWrite);
    deny_deployment_token!(auth);
    let previous = authorized_application(&state, &auth, &application_public_id).await?;
    let application = project_linker(&state)
        .link(
            &auth,
            state.project_access_checker.as_ref(),
            &previous,
            request.project_id,
        )
        .await?;
    state
        .audit(&ApplicationTopologyChangedAudit {
            context: audit_context(&auth, &metadata),
            application_id: application_public_id,
            action: "link_project".to_string(),
            project_id: request.project_id,
        })
        .await;
    Ok(Json(ApplicationResponse::from(application)))
}

#[utoipa::path(
    delete, tag = "AI Applications",
    path = "/ai/applications/{application_public_id}/projects/{project_id}",
    operation_id = "unlink_application_project",
    summary = "Unlink an application project",
    description = "Unlinks a project and its data-network access. A workspace may contain no linked projects.",
    params(("application_public_id" = String, Path,), ("project_id" = i32, Path,)),
    responses((status = 200, body = ApplicationResponse), (status = 401), (status = 403), (status = 404), (status = 409)),
    security(("bearer_auth" = []))
)]
pub async fn unlink_application_project(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((application_public_id, project_id)): Path<(String, i32)>,
) -> Result<Json<ApplicationResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    permission_guard!(auth, SandboxesWrite);
    deny_deployment_token!(auth);
    let current = authorized_application(&state, &auth, &application_public_id).await?;
    let current_project_ids = current
        .projects
        .iter()
        .map(|project| project.id)
        .collect::<Vec<_>>();
    ensure_application_project_permission(
        &auth,
        &state.project_access_checker,
        &current_project_ids,
        &Permission::ProjectsWrite,
    )
    .await?;
    ensure_application_project_permission(
        &auth,
        &state.project_access_checker,
        &current_project_ids,
        &Permission::SandboxesWrite,
    )
    .await?;
    let _runtime_gate = state
        .service
        .lock_application_runtime_update(&application_public_id)
        .await
        .map_err(Problem::from)?;
    let project_slug = current
        .projects
        .iter()
        .find(|project| project.id == project_id)
        .map(|project| project.slug.clone())
        .ok_or_else(|| ApplicationError::ProjectNotLinked {
            application_id: application_public_id.clone(),
            project_id,
        })?;
    let remaining_project_ids = current
        .projects
        .iter()
        .filter(|project| project.id != project_id)
        .map(|project| project.id)
        .collect::<Vec<_>>();
    let mut paused_sandbox = None;
    if let Some(sandboxes) = state.application_sandboxes.as_ref() {
        if let Some(summary) = sandboxes
            .application_workspace_summary(auth.user_id(), &application_public_id)
            .await
            .map_err(|error| {
                application_workspace_provider_problem(
                    error,
                    &application_public_id,
                    "inspect before unlink",
                )
            })?
            .filter(|summary| workspace_may_retain_data_plane(&summary.status))
        {
            // Stop compute before moving its source tree. This closes both the
            // filesystem and data-network race while unlinking.
            sandboxes
                .pause_sandbox(&summary.public_id, auth.user_id())
                .await
                .map_err(|error| {
                    application_workspace_provider_problem(
                        error,
                        &application_public_id,
                        "pause before unlink",
                    )
                })?;
            paused_sandbox = Some(summary.public_id);
        }
    }
    let staged_source = match state
        .application_workspaces
        .stage_project_removal(&application_public_id, &project_slug)
        .await
    {
        Ok(staged) => staged,
        Err(error) => {
            if let (Some(sandboxes), Some(sandbox_id)) = (
                state.application_sandboxes.as_ref(),
                paused_sandbox.as_deref(),
            ) {
                if let Err(resume_error) =
                    sandboxes.resume_sandbox(sandbox_id, auth.user_id()).await
                {
                    tracing::error!(
                        application_id = application_public_id,
                        %resume_error,
                        "Failed to resume application workspace after unlink staging failed"
                    );
                }
            }
            return Err(error.into());
        }
    };
    let application = match state
        .applications
        .unlink_project(auth.user_id(), &application_public_id, project_id)
        .await
    {
        Ok(application) => application,
        Err(error) => {
            if let Some(staged) = staged_source.as_ref() {
                if let Err(restore_error) = state
                    .application_workspaces
                    .restore_staged_project(staged)
                    .await
                {
                    tracing::error!(
                        application_id = application_public_id,
                        project_id,
                        %restore_error,
                        "Failed to restore application project source after unlink rollback"
                    );
                }
            }
            if let (Some(sandboxes), Some(sandbox_id)) = (
                state.application_sandboxes.as_ref(),
                paused_sandbox.as_deref(),
            ) {
                if let Err(resume_error) =
                    sandboxes.resume_sandbox(sandbox_id, auth.user_id()).await
                {
                    tracing::error!(
                        application_id = application_public_id,
                        %resume_error,
                        "Failed to resume application workspace after unlink rollback"
                    );
                }
            }
            return Err(error.into());
        }
    };
    if let Some(staged) = staged_source {
        if let Err(error) = state
            .application_workspaces
            .finalize_staged_project(staged)
            .await
        {
            tracing::error!(
                application_id = application_public_id,
                project_id,
                %error,
                "Failed to delete staged source after project unlink; source remains outside the mounted workspace"
            );
        }
    }
    if let (Some(sandboxes), Some(sandbox_id)) = (
        state.application_sandboxes.as_ref(),
        paused_sandbox.as_deref(),
    ) {
        // Narrow data-plane reachability while compute is still stopped. If
        // synchronization fails, leave the sandbox paused rather than briefly
        // reopening access to the unlinked project's services.
        sandboxes
            .synchronize_application_data_network(
                auth.user_id(),
                &application_public_id,
                sandbox_id,
                &remaining_project_ids,
            )
            .await
            .map_err(|error| {
                application_workspace_provider_problem(
                    error,
                    &application_public_id,
                    "synchronize data network after unlink",
                )
            })?;
        sandboxes
            .resume_sandbox(sandbox_id, auth.user_id())
            .await
            .map_err(|error| {
                application_workspace_provider_problem(
                    error,
                    &application_public_id,
                    "resume after unlink",
                )
            })?;
    }
    state
        .audit(&ApplicationTopologyChangedAudit {
            context: audit_context(&auth, &metadata),
            application_id: application_public_id,
            action: "unlink_project".to_string(),
            project_id,
        })
        .await;
    Ok(Json(ApplicationResponse::from(application)))
}

#[utoipa::path(
    post, tag = "AI Applications",
    path = "/ai/applications/{application_public_id}/projects/{project_id}/primary",
    operation_id = "set_application_primary_project",
    summary = "Choose the application's primary project",
    params(("application_public_id" = String, Path,), ("project_id" = i32, Path,)),
    responses((status = 200, body = ApplicationResponse), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn set_application_primary_project(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((application_public_id, project_id)): Path<(String, i32)>,
) -> Result<Json<ApplicationResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    deny_deployment_token!(auth);
    let current = authorized_application(&state, &auth, &application_public_id).await?;
    ensure_application_project_permission(
        &auth,
        &state.project_access_checker,
        &current
            .projects
            .iter()
            .map(|project| project.id)
            .collect::<Vec<_>>(),
        &Permission::ProjectsWrite,
    )
    .await?;
    let application = state
        .applications
        .set_primary_project(auth.user_id(), &application_public_id, project_id)
        .await?;
    state
        .audit(&ApplicationTopologyChangedAudit {
            context: audit_context(&auth, &metadata),
            application_id: application_public_id,
            action: "set_primary_project".to_string(),
            project_id,
        })
        .await;
    Ok(Json(ApplicationResponse::from(application)))
}
