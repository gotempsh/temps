// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The application workspace: its settings, state and lifecycle control.

use super::*;

async fn application_workspace_response(
    state: &AppState,
    auth: &AuthContext,
    application: &crate::applications::ApplicationWithProjects,
    snapshot_id: Option<String>,
) -> Result<ApplicationWorkspaceResponse, Problem> {
    let desired = state
        .applications
        .workspace(application.application.id)
        .await?;
    let sandboxes = state.application_sandboxes.as_ref().ok_or_else(|| {
        problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
            .with_title("Application Sandbox Unavailable")
            .with_detail("The instance sandbox service is not configured.")
    })?;
    let summary = sandboxes
        .application_workspace_summary(auth.user_id(), &application.application.public_id)
        .await
        .map_err(|error| {
            application_workspace_provider_problem(
                error,
                &application.application.public_id,
                "inspect workspace",
            )
        })?;
    let mut diagnostic = desired.last_error.clone();
    let usage = if let Some(summary) = summary
        .as_ref()
        .filter(|summary| summary.status == "running" && desired.desired_state == "running")
    {
        match sandboxes
            .application_workspace_usage(auth.user_id(), &summary.public_id)
            .await
        {
            Ok(usage) => usage,
            Err(error) => {
                tracing::warn!(
                    application_id = %application.application.public_id,
                    sandbox_id = %summary.public_id,
                    error = %error,
                    "Could not read application workspace resource usage"
                );
                diagnostic =
                    Some("Workspace resource usage is temporarily unavailable.".to_string());
                temps_sandbox::services::ApplicationWorkspaceUsage::default()
            }
        }
    } else {
        temps_sandbox::services::ApplicationWorkspaceUsage::default()
    };
    let project_ids = application
        .projects
        .iter()
        .map(|project| project.id)
        .collect::<Vec<_>>();
    let data_network_service_count =
        match sandboxes.application_data_service_count(&project_ids).await {
            Ok(count) => count,
            Err(error) => {
                tracing::warn!(
                    application_id = %application.application.public_id,
                    error = %error,
                    "Could not inspect application database topology"
                );
                diagnostic =
                    Some("The workspace database topology is temporarily unavailable.".to_string());
                0
            }
        };
    let state_name = application_workspace_state(
        summary.as_ref().map(|summary| summary.status.as_str()),
        &desired.desired_state,
        desired.sandbox_public_id.is_some(),
        diagnostic.is_some(),
    );
    let volume_path = state
        .application_workspaces
        .root()
        .join(&application.application.public_id)
        .join("projects");
    let persistent_volume_healthy = tokio::fs::metadata(volume_path)
        .await
        .is_ok_and(|metadata| metadata.is_dir());
    let target_image =
        temps_sandbox::services::managed_application_workspace_image(&desired.runtime)
            .map(str::to_string);
    let (runtime_compatible, runtime_update_error) = if let Some(summary) = summary
        .as_ref()
        .filter(|summary| summary.status == "running")
    {
        match sandboxes
            .application_workspace_runtime_compatibility(auth.user_id(), &summary.public_id)
            .await
        {
            Ok(temps_ai_agents::sandbox::RuntimeCompatibility::Compatible) => (Some(true), None),
            Ok(temps_ai_agents::sandbox::RuntimeCompatibility::Incompatible { reason }) => {
                tracing::info!(application_id = %application.application.public_id, %reason, "Workspace runtime is incompatible");
                (Some(false), Some("This workspace uses an older runtime protocol. Update the runtime to continue chatting.".to_string()))
            }
            Ok(temps_ai_agents::sandbox::RuntimeCompatibility::Unavailable { reason }) => {
                tracing::warn!(application_id = %application.application.public_id, %reason, "Workspace runtime compatibility unavailable");
                (
                    None,
                    Some("Runtime compatibility is temporarily unavailable.".to_string()),
                )
            }
            Err(error) => {
                tracing::warn!(application_id = %application.application.public_id, %error, "Runtime compatibility check failed");
                (
                    None,
                    Some("Runtime compatibility is temporarily unavailable.".to_string()),
                )
            }
        }
    } else {
        (None, None)
    };
    let runtime_update_available = summary.as_ref().is_some_and(|summary| {
        summary.status == "running"
            && summary.backend.as_deref() == Some("docker")
            && target_image.is_some()
    });
    Ok(ApplicationWorkspaceResponse {
        state: state_name.to_string(),
        desired_state: desired.desired_state,
        sandbox_public_id: summary.as_ref().map(|summary| summary.public_id.clone()),
        runtime: desired.runtime,
        image: summary
            .as_ref()
            .and_then(|summary| summary.image.clone())
            .or(desired.image),
        runtime_compatible,
        runtime_update_available,
        runtime_update_image: runtime_update_available.then_some(target_image).flatten(),
        runtime_update_error,
        cpu_limit: desired.cpu_limit,
        memory_limit_mb: desired.memory_limit_mb,
        pids_limit: desired.pids_limit,
        disk_limit_mb: desired.disk_limit_mb,
        disk_limit_enforced: summary
            .as_ref()
            .and_then(|summary| summary.backend.as_deref())
            == Some("firecracker"),
        idle_timeout_secs: desired.idle_timeout_secs,
        memory_used_bytes: usage.memory_used_bytes,
        pids_used: usage.pids_used,
        disk_used_bytes: usage.disk_used_bytes,
        cpu_usage_usec: usage.cpu_usage_usec,
        open_preview_ports: usage.open_ports,
        persistent_volume_healthy,
        data_network_service_count,
        last_error: diagnostic,
        snapshot_id,
    })
}

pub(super) fn application_workspace_state<'a>(
    sandbox_status: Option<&str>,
    desired_state: &'a str,
    has_sandbox_identity: bool,
    has_diagnostic: bool,
) -> &'a str {
    match sandbox_status {
        _ if desired_state == "quarantined" => "failed",
        _ if has_diagnostic => "failed",
        Some("running") => "running",
        Some("recovering") => "recovering",
        Some("stopped") if desired_state == "running" => "recovering",
        Some("stopped") => "sleeping",
        None if has_sandbox_identity && desired_state == "running" => "recovering",
        _ => "sleeping",
    }
}

#[utoipa::path(
    get, tag = "AI Applications",
    path = "/ai/applications/{application_public_id}/workspace",
    params(("application_public_id" = String, Path,)),
    responses((status = 200, body = ApplicationWorkspaceResponse), (status = 401), (status = 403), (status = 404), (status = 503)),
    security(("bearer_auth" = []))
)]
pub async fn get_application_workspace(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(application_public_id): Path<String>,
) -> Result<Json<ApplicationWorkspaceResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let application = authorized_application(&state, &auth, &application_public_id).await?;
    Ok(Json(
        application_workspace_response(&state, &auth, &application, None).await?,
    ))
}

#[utoipa::path(
    patch, tag = "AI Applications",
    path = "/ai/applications/{application_public_id}/workspace",
    operation_id = "update_application_workspace",
    summary = "Update desired application workspace resources",
    description = "Persists desired runtime and resource settings server-side, then replaces compute while retaining the application files.",
    params(("application_public_id" = String, Path,)), request_body = UpdateApplicationWorkspaceRequest,
    responses((status = 200, body = ApplicationWorkspaceResponse), (status = 400), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn update_application_workspace(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(application_public_id): Path<String>,
    Json(request): Json<UpdateApplicationWorkspaceRequest>,
) -> Result<Json<ApplicationWorkspaceResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    permission_guard!(auth, SandboxesWrite);
    deny_deployment_token!(auth);
    if request.image.is_some() {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Use Explicit Runtime Update")
            .with_detail("Image changes require the confirmed update_runtime control action; arbitrary image input is not accepted."));
    }
    let application = authorized_application(&state, &auth, &application_public_id).await?;
    let project_ids = application
        .projects
        .iter()
        .map(|project| project.id)
        .collect::<Vec<_>>();
    ensure_application_project_permission(
        &auth,
        &state.project_access_checker,
        &project_ids,
        &Permission::ProjectsWrite,
    )
    .await?;
    ensure_application_project_permission(
        &auth,
        &state.project_access_checker,
        &project_ids,
        &Permission::SandboxesWrite,
    )
    .await?;
    let _turn_gate = state
        .service
        .lock_application_runtime_update(&application_public_id)
        .await
        .map_err(Problem::from)?;
    if let Some(runtime) = request.runtime.as_deref() {
        let current = state
            .applications
            .workspace(application.application.id)
            .await?;
        if runtime != current.runtime {
            let has_compute = if let Some(sandboxes) = state.application_sandboxes.as_ref() {
                sandboxes
                    .application_workspace_summary(auth.user_id(), &application_public_id)
                    .await
                    .map_err(|error| {
                        application_workspace_provider_problem(
                            error,
                            &application_public_id,
                            "inspect before runtime setting",
                        )
                    })?
                    .is_some()
            } else {
                false
            };
            if has_compute {
                return Err(problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Confirm Runtime Change")
                    .with_detail("Use the update_runtime control action with confirm: true and the selected built-in runtime to safely replace existing compute."));
            }
        }
    }
    let desired = state
        .applications
        .update_workspace(
            auth.user_id(),
            application.application.id,
            crate::applications::WorkspaceSettingsUpdate {
                runtime: request.runtime,
                image: request
                    .image
                    .map(|image| (!image.is_empty()).then_some(image)),
                cpu_limit: request.cpu_limit,
                memory_limit_mb: request.memory_limit_mb,
                pids_limit: request.pids_limit,
                disk_limit_mb: request.disk_limit_mb,
                idle_timeout_secs: request.idle_timeout_secs,
            },
        )
        .await?;
    let workspace = state
        .application_workspaces
        .ensure(&application.application.public_id, &application.projects)
        .await?;
    if let Some(sandboxes) = state.application_sandboxes.as_ref() {
        if let Some(summary) = sandboxes
            .application_workspace_summary(auth.user_id(), &application_public_id)
            .await
            .map_err(|error| {
                application_workspace_provider_problem(
                    error,
                    &application_public_id,
                    "inspect before resource update",
                )
            })?
        {
            if let Err(error) = sandboxes
                .rebuild_application_workspace(
                    auth.user_id(),
                    &summary.public_id,
                    workspace.host_work_dir,
                    (&desired).into(),
                )
                .await
            {
                state
                    .applications
                    .update_workspace_runtime_state(
                        application.application.id,
                        None,
                        Some(
                            "Workspace rebuild failed while applying the saved resource settings."
                                .to_string(),
                        ),
                    )
                    .await?;
                return Err(application_workspace_provider_problem(
                    error,
                    &application_public_id,
                    "rebuild after resource update",
                ));
            }
            state
                .applications
                .update_workspace_runtime_state(application.application.id, None, None)
                .await?;
        }
    }
    let response = application_workspace_response(&state, &auth, &application, None).await?;
    state
        .audit(&ApplicationWorkspaceChangedAudit {
            context: audit_context(&auth, &metadata),
            application_id: application_public_id,
            action: "update_resources".to_string(),
            sandbox_id: response.sandbox_public_id.clone(),
            runtime: Some(desired.runtime),
            cpu_limit: Some(desired.cpu_limit),
            memory_limit_mb: Some(desired.memory_limit_mb),
            pids_limit: Some(desired.pids_limit),
            disk_limit_mb: Some(desired.disk_limit_mb),
        })
        .await;
    Ok(Json(response))
}

#[utoipa::path(
    post, tag = "AI Applications",
    path = "/ai/applications/{application_public_id}/workspace/actions",
    operation_id = "control_application_workspace",
    summary = "Control an application workspace",
    description = "Restart, pause, resume, rebuild, snapshot, or restore the application's persistent workspace. Files outlive suspended or replaced compute.",
    params(("application_public_id" = String, Path,)), request_body = ControlApplicationWorkspaceRequest,
    responses((status = 200, body = ApplicationWorkspaceResponse), (status = 400), (status = 401), (status = 403), (status = 404), (status = 409)),
    security(("bearer_auth" = []))
)]
pub async fn control_application_workspace(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(application_public_id): Path<String>,
    Json(request): Json<ControlApplicationWorkspaceRequest>,
) -> Result<Json<ApplicationWorkspaceResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    permission_guard!(auth, SandboxesWrite);
    deny_deployment_token!(auth);
    let application = authorized_application(&state, &auth, &application_public_id).await?;
    let project_ids = application
        .projects
        .iter()
        .map(|project| project.id)
        .collect::<Vec<_>>();
    ensure_application_project_permission(
        &auth,
        &state.project_access_checker,
        &project_ids,
        &Permission::ProjectsWrite,
    )
    .await?;
    ensure_application_project_permission(
        &auth,
        &state.project_access_checker,
        &project_ids,
        &Permission::SandboxesWrite,
    )
    .await?;
    let sandboxes = state.application_sandboxes.as_ref().ok_or_else(|| {
        problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
            .with_title("Application Sandbox Unavailable")
            .with_detail("The instance sandbox service is not configured.")
    })?;
    let action = request.action.clone();
    let _other_action_gate = if action == "update_runtime" {
        None
    } else {
        Some(
            state
                .service
                .lock_application_runtime_update(&application_public_id)
                .await
                .map_err(Problem::from)?,
        )
    };
    let mut snapshot_id = None;
    match request.action.as_str() {
        "update_runtime" => {
            if request.confirm != Some(true) {
                return Err(problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Runtime Update Confirmation Required")
                    .with_detail("Set confirm: true to replace workspace compute after an isolated runtime preflight."));
            }
            let _turn_gate = state
                .service
                .lock_application_runtime_update(&application_public_id)
                .await
                .map_err(Problem::from)?;
            let desired = state
                .applications
                .workspace(application.application.id)
                .await?;
            let next_runtime = request.runtime.as_deref().unwrap_or(&desired.runtime);
            let pinned_image = temps_sandbox::services::managed_application_workspace_image(next_runtime)
                .ok_or_else(|| problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Unsupported Workspace Runtime")
                    .with_detail(format!("Application {application_public_id} has unsupported runtime {next_runtime}")))?
                .to_string();
            let summary = sandboxes
                .application_workspace_summary(auth.user_id(), &application_public_id)
                .await
                .map_err(|error| {
                    application_workspace_provider_problem(
                        error,
                        &application_public_id,
                        "inspect before runtime update",
                    )
                })?
                .ok_or_else(|| {
                    ApplicationError::NotFound(format!("workspace:{application_public_id}"))
                })?;
            if summary.status != "running" {
                return Err(problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Workspace Must Be Running")
                    .with_detail(format!("Workspace {application_public_id} is {}; resume it before updating the runtime.", summary.status)));
            }
            if summary.backend.as_deref() != Some("docker") {
                return Err(problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Runtime Update Unsupported")
                    .with_detail(format!("Workspace {application_public_id} does not run on the Docker backend required for safe image rollback.")));
            }
            let workspace = state
                .application_workspaces
                .ensure(&application_public_id, &application.projects)
                .await?;
            let mut config: temps_sandbox::services::ApplicationWorkspaceConfig = (&desired).into();
            config.image = Some(pinned_image.clone());
            let sandboxes = Arc::clone(sandboxes);
            let app_id = application.application.id;
            let user_id = auth.user_id();
            let application_id = application_public_id.clone();
            let sandbox_id = summary.public_id;
            let selected_runtime = next_runtime.to_string();
            let authorized_project_ids = project_ids.clone();
            let audit_service = Arc::clone(&state.audit_service);
            let audit_context = audit_context(&auth, &metadata);
            // Keep the write gate and rollback operation alive if the HTTP
            // client disconnects midway through replacement.
            tokio::spawn(async move {
                let _turn_gate = _turn_gate;
                sandboxes.update_application_workspace_runtime(user_id, app_id, &application_id, &sandbox_id, workspace.host_work_dir, &selected_runtime, &authorized_project_ids, config).await
                    .map_err(|error| application_workspace_provider_problem(error, &application_id, "update runtime"))?;
                if let Err(error) = audit_service.create_audit_log(&ApplicationWorkspaceChangedAudit {
                    context: audit_context,
                    application_id: application_id.clone(),
                    action: "update_runtime".to_string(),
                    sandbox_id: Some(sandbox_id),
                    runtime: Some(selected_runtime),
                    cpu_limit: None,
                    memory_limit_mb: None,
                    pids_limit: None,
                    disk_limit_mb: None,
                }).await {
                    tracing::error!(application_id = %application_id, %error, "Failed to audit runtime update");
                }
                Ok::<(), Problem>(())
            }).await.map_err(|error| problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Runtime Update Task Failed")
                .with_detail(format!("Runtime update task for {application_public_id} failed: {error}")))??;
        }
        "pause" => {
            let summary = sandboxes
                .application_workspace_summary(auth.user_id(), &application_public_id)
                .await
                .map_err(|error| {
                    application_workspace_provider_problem(
                        error,
                        &application_public_id,
                        "inspect before pause",
                    )
                })?
                .ok_or_else(|| {
                    ApplicationError::NotFound(format!("workspace:{application_public_id}"))
                })?;
            sandboxes
                .pause_sandbox(&summary.public_id, auth.user_id())
                .await
                .map_err(|error| {
                    application_workspace_provider_problem(error, &application_public_id, "pause")
                })?;
            state
                .applications
                .record_workspace_sandbox(
                    application.application.id,
                    Some(summary.public_id),
                    Some("paused"),
                    None,
                )
                .await?;
        }
        "resume" => {
            state
                .applications
                .update_workspace_runtime_state(application.application.id, Some("running"), None)
                .await?;
            let (_, sandbox_id) =
                application_workspace_sandbox(&state, &auth, &application_public_id).await?;
            if let Some(summary) = sandboxes
                .application_workspace_summary(auth.user_id(), &application_public_id)
                .await
                .map_err(|error| {
                    application_workspace_provider_problem(
                        error,
                        &application_public_id,
                        "inspect before resume",
                    )
                })?
            {
                if summary.status == "stopped" {
                    sandboxes
                        .resume_sandbox(&sandbox_id, auth.user_id())
                        .await
                        .map_err(|error| {
                            application_workspace_provider_problem(
                                error,
                                &application_public_id,
                                "resume",
                            )
                        })?;
                }
            }
            state
                .applications
                .record_workspace_sandbox(
                    application.application.id,
                    Some(sandbox_id),
                    Some("running"),
                    None,
                )
                .await?;
        }
        "restart" => {
            let (_, sandbox_id) =
                application_workspace_sandbox(&state, &auth, &application_public_id).await?;
            sandboxes
                .restart_sandbox(&sandbox_id, auth.user_id())
                .await
                .map_err(|error| {
                    application_workspace_provider_problem(error, &application_public_id, "restart")
                })?;
            state
                .applications
                .update_workspace_runtime_state(application.application.id, None, None)
                .await?;
        }
        "rebuild" => {
            let (_, sandbox_id) =
                application_workspace_sandbox(&state, &auth, &application_public_id).await?;
            let workspace = state
                .application_workspaces
                .ensure(&application.application.public_id, &application.projects)
                .await?;
            let desired = state
                .applications
                .workspace(application.application.id)
                .await?;
            sandboxes
                .rebuild_application_workspace(
                    auth.user_id(),
                    &sandbox_id,
                    workspace.host_work_dir,
                    (&desired).into(),
                )
                .await
                .map_err(|error| {
                    application_workspace_provider_problem(error, &application_public_id, "rebuild")
                })?;
            state
                .applications
                .update_workspace_runtime_state(application.application.id, None, None)
                .await?;
        }
        "snapshot" => {
            let snapshots = state.sandbox_snapshots.as_ref().ok_or_else(|| {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("Workspace Snapshots Unavailable")
                    .with_detail("The sandbox snapshot service is not configured.")
            })?;
            let (_, sandbox_id) =
                application_workspace_sandbox(&state, &auth, &application_public_id).await?;
            let row = sandboxes
                .find_by_public_id(&sandbox_id, auth.user_id())
                .await
                .map_err(|error| {
                    application_workspace_provider_problem(
                        error,
                        &application_public_id,
                        "inspect before snapshot",
                    )
                })?;
            let snapshot = snapshots
                .create_snapshot(
                    row.id,
                    &row.public_id,
                    auth.user_id(),
                    application.primary_project_id,
                    request.label,
                )
                .await
                .map_err(|error| {
                    application_workspace_snapshot_problem(
                        error,
                        &application_public_id,
                        "create snapshot",
                    )
                })?;
            snapshot_id = Some(snapshot.public_id);
        }
        "restore" => {
            let requested_snapshot = request.snapshot_id.as_deref().ok_or_else(|| {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Snapshot Required")
                    .with_detail("restore requires snapshot_id")
            })?;
            let snapshots = state.sandbox_snapshots.as_ref().ok_or_else(|| {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("Workspace Snapshots Unavailable")
                    .with_detail("The sandbox snapshot service is not configured.")
            })?;
            let (_, sandbox_id) =
                application_workspace_sandbox(&state, &auth, &application_public_id).await?;
            let sandbox_row = sandboxes
                .find_by_public_id(&sandbox_id, auth.user_id())
                .await
                .map_err(|error| {
                    application_workspace_provider_problem(
                        error,
                        &application_public_id,
                        "inspect before restore",
                    )
                })?;
            let snapshot_row = snapshots
                .get_snapshot(auth.user_id(), requested_snapshot)
                .await
                .map_err(|error| {
                    application_workspace_snapshot_problem(
                        error,
                        &application_public_id,
                        "inspect snapshot before restore",
                    )
                })?;
            if snapshot_row.source_sandbox_id != Some(sandbox_row.id) {
                return Err(problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Snapshot Does Not Belong to Application")
                    .with_detail(format!(
                        "Snapshot '{requested_snapshot}' was not created from this application's workspace."
                    )));
            }
            let _artifact_guard = snapshots.acquire_artifact_lifecycle().await;
            let artifact = snapshots
                .resolve_for_restore(auth.user_id(), requested_snapshot, Some("docker"))
                .await
                .map_err(|error| {
                    application_workspace_snapshot_problem(
                        error,
                        &application_public_id,
                        "resolve snapshot before restore",
                    )
                })?;
            let workspace = state
                .application_workspaces
                .ensure(&application.application.public_id, &application.projects)
                .await?;
            let desired = state
                .applications
                .workspace(application.application.id)
                .await?;
            sandboxes
                .restore_application_workspace(
                    auth.user_id(),
                    &sandbox_id,
                    workspace.host_work_dir,
                    (&desired).into(),
                    &artifact,
                )
                .await
                .map_err(|error| {
                    application_workspace_provider_problem(error, &application_public_id, "restore")
                })?;
            state
                .applications
                .update_workspace_runtime_state(application.application.id, None, None)
                .await?;
            snapshot_id = Some(requested_snapshot.to_string());
        }
        _ => {
            return Err(problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Workspace Action")
                .with_detail("Expected restart, pause, resume, rebuild, snapshot, restore, or update_runtime."));
        }
    }
    let response = application_workspace_response(&state, &auth, &application, snapshot_id).await?;
    if action == "update_runtime" {
        return Ok(Json(response));
    }
    state
        .audit(&ApplicationWorkspaceChangedAudit {
            context: audit_context(&auth, &metadata),
            application_id: application_public_id,
            action,
            sandbox_id: response.sandbox_public_id.clone(),
            runtime: None,
            cpu_limit: None,
            memory_limit_mb: None,
            pids_limit: None,
            disk_limit_mb: None,
        })
        .await;
    Ok(Json(response))
}
