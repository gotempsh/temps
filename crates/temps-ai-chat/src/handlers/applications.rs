// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Application routes: list, create, get, archive, rename and restore an
//! application, and its conversations.

use super::*;

#[utoipa::path(
    get, tag = "AI Applications", path = "/ai/applications",
    params(LifecycleListQuery),
    responses((status = 200, body = Vec<ApplicationResponse>), (status = 401), (status = 403)),
    security(("bearer_auth" = []))
)]
pub async fn list_applications(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Query(query): Query<LifecycleListQuery>,
) -> Result<Json<Vec<ApplicationResponse>>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let (page, page_size) = normalize_list_pagination(query.page, query.page_size);
    let visible_application_ids =
        visible_application_ids_for_listing(&state, &auth, Some(query.status.as_str())).await?;
    let applications = state
        .applications
        .list_with_status_and_visibility(
            auth.user_id(),
            page,
            page_size,
            query.status.as_str(),
            Some(&visible_application_ids),
        )
        .await?;
    Ok(Json(
        applications
            .into_iter()
            .map(ApplicationResponse::from)
            .collect(),
    ))
}

#[utoipa::path(
    post, tag = "AI Applications", path = "/ai/applications",
    request_body = CreateApplicationRequest,
    responses((status = 201, body = ApplicationResponse), (status = 400), (status = 401), (status = 403)),
    security(("bearer_auth" = []))
)]
pub async fn create_application(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateApplicationRequest>,
) -> Result<(StatusCode, Json<ApplicationResponse>), Problem> {
    permission_guard!(auth, SandboxesWrite);
    deny_deployment_token!(auth);
    if let Err(detail) = validate_application_project_choice(
        request.starter_project.is_some(),
        request.project_ids.len(),
    ) {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Choose an Application Project")
            .with_detail(detail));
    }
    if request.starter_project.is_some() || !request.project_ids.is_empty() {
        permission_guard!(auth, ProjectsCreate);
        permission_guard!(auth, ProjectsWrite);
    }
    ensure_application_creation_project_permission(
        &auth,
        &state.project_access_checker,
        &request.project_ids,
        &Permission::ProjectsWrite,
    )
    .await?;
    ensure_application_creation_project_permission(
        &auth,
        &state.project_access_checker,
        &request.project_ids,
        &Permission::SandboxesWrite,
    )
    .await?;
    let mut project_ids = request.project_ids.clone();
    let mut created_starter = None;
    if let Some(starter) = request.starter_project.as_ref() {
        let project = create_starter_project(&state.project_service, starter)
            .await
            .map_err(Problem::from)?;
        project_ids.push(project.id);
        created_starter = Some((project.id, project.name));
    }
    let application = match state
        .applications
        .create(
            auth.user_id(),
            &request.name,
            request.description.as_deref(),
            &project_ids,
        )
        .await
    {
        Ok(application) => application,
        Err(error) => {
            if let Some((project_id, project_name)) = created_starter {
                if let Err(cleanup_error) = state
                    .project_service
                    .delete_project(project_id, &project_name)
                    .await
                {
                    tracing::error!(
                        project_id,
                        error = %cleanup_error,
                        "Failed to clean up starter project after application creation failed"
                    );
                }
            }
            return Err(error.into());
        }
    };
    if let Err(workspace_error) = state
        .application_workspaces
        .ensure(&application.application.public_id, &application.projects)
        .await
    {
        if let Err(rollback_error) = state
            .applications
            .rollback_failed_create(auth.user_id(), &application.application.public_id)
            .await
        {
            tracing::error!(
                application_id = application.application.public_id,
                error = %rollback_error,
                "Failed to roll back application after workspace preparation failed"
            );
        }
        if let Some((project_id, project_name)) = created_starter {
            if let Err(cleanup_error) = state
                .project_service
                .delete_project(project_id, &project_name)
                .await
            {
                tracing::error!(
                    project_id,
                    error = %cleanup_error,
                    "Failed to clean up starter project after workspace preparation failed"
                );
            }
        }
        return Err(workspace_error.into());
    }
    state
        .audit(&ApplicationCreatedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            application_id: application.application.public_id.clone(),
            project_ids,
        })
        .await;
    Ok((
        StatusCode::CREATED,
        Json(ApplicationResponse::from(application)),
    ))
}

#[utoipa::path(
    get, tag = "AI Applications", path = "/ai/applications/{application_public_id}",
    params(
        ("application_public_id" = String, Path,),
        LifecycleStatusQuery,
    ),
    responses((status = 200, body = ApplicationResponse), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn get_application(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(application_public_id): Path<String>,
    Query(query): Query<LifecycleStatusQuery>,
) -> Result<Json<ApplicationResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let application = authorized_application_with_status(
        &state,
        &auth,
        &application_public_id,
        query.status.as_str(),
    )
    .await?;
    Ok(Json(ApplicationResponse::from(application)))
}

#[utoipa::path(
    delete, tag = "AI Applications", path = "/ai/applications/{application_public_id}",
    operation_id = "archive_application",
    summary = "Archive an AI application",
    description = "Archives the application and pauses its workspace compute while retaining projects, conversations, and persistent files.",
    params(("application_public_id" = String, Path,)),
    responses((status = 204), (status = 401), (status = 403), (status = 404), (status = 503)),
    security(("bearer_auth" = []))
)]
pub async fn archive_application(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(application_public_id): Path<String>,
) -> Result<StatusCode, Problem> {
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

    let _runtime_gate = state
        .service
        .lock_application_runtime_update(&application_public_id)
        .await
        .map_err(Problem::from)?;

    let mut paused_sandbox = None;
    if let Some(sandboxes) = state.application_sandboxes.as_ref() {
        if let Some(summary) = sandboxes
            .application_workspace_summary(auth.user_id(), &application_public_id)
            .await
            .map_err(|error| {
                application_workspace_provider_problem(
                    error,
                    &application_public_id,
                    "inspect before archive",
                )
            })?
        {
            sandboxes
                .pause_sandbox(&summary.public_id, auth.user_id())
                .await
                .map_err(|error| {
                    application_workspace_provider_problem(
                        error,
                        &application_public_id,
                        "pause before archive",
                    )
                })?;
            paused_sandbox = Some(summary.public_id);
        }
    }

    let project_ids = match state
        .applications
        .archive(auth.user_id(), &application_public_id)
        .await
    {
        Ok(project_ids) => project_ids,
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
                        "Failed to resume application workspace after archive rollback"
                    );
                }
            }
            return Err(error.into());
        }
    };
    state
        .audit(&ApplicationArchivedAudit {
            context: audit_context(&auth, &metadata),
            application_id: application_public_id,
            project_ids,
        })
        .await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    patch, tag = "AI Applications", path = "/ai/applications/{application_public_id}",
    operation_id = "rename_application",
    summary = "Rename an AI application",
    description = "Changes the application's name. Its id, chats, workspace and projects are unchanged.",
    params(("application_public_id" = String, Path,)),
    request_body = RenameApplicationRequest,
    responses(
        (status = 200, body = ApplicationResponse),
        (status = 400, description = "The name is empty or longer than 200 characters"),
        (status = 401), (status = 403), (status = 404)
    ),
    security(("bearer_auth" = []))
)]
pub async fn rename_application(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(application_public_id): Path<String>,
    Json(request): Json<RenameApplicationRequest>,
) -> Result<Json<ApplicationResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    deny_deployment_token!(auth);
    let current = authorized_application(&state, &auth, &application_public_id).await?;
    let project_ids = current
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
    let renamed = state
        .applications
        .rename(auth.user_id(), &application_public_id, &request.name)
        .await?;
    if renamed.application.name != current.application.name {
        state
            .audit(&ApplicationRenamedAudit {
                context: audit_context(&auth, &metadata),
                application_id: application_public_id,
                previous_name: current.application.name,
                name: renamed.application.name.clone(),
            })
            .await;
    }
    Ok(Json(ApplicationResponse::from(renamed)))
}

#[utoipa::path(
    post, tag = "AI Applications", path = "/ai/applications/{application_public_id}/restore",
    operation_id = "restore_application",
    summary = "Restore an archived AI application",
    description = "Restores an archived application and requests that its retained persistent workspace resume on next access.",
    params(("application_public_id" = String, Path,)),
    responses((status = 200, body = ApplicationResponse), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn restore_application(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(application_public_id): Path<String>,
) -> Result<Json<ApplicationResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    permission_guard!(auth, SandboxesWrite);
    deny_deployment_token!(auth);
    let archived =
        authorized_application_with_status(&state, &auth, &application_public_id, "archived")
            .await?;
    let project_ids = archived
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
    let _runtime_gate = state
        .service
        .lock_application_runtime_update(&application_public_id)
        .await
        .map_err(Problem::from)?;
    let restored = state
        .applications
        .restore(auth.user_id(), &application_public_id)
        .await?;
    state
        .audit(&ApplicationRestoredAudit {
            context: audit_context(&auth, &metadata),
            application_id: application_public_id,
            project_ids,
        })
        .await;
    Ok(Json(ApplicationResponse::from(restored)))
}

#[utoipa::path(
    get, tag = "AI Applications", path = "/ai/applications/{application_public_id}/conversations",
    params(
        ("application_public_id" = String, Path,),
        LifecycleListQuery,
    ),
    responses((status = 200, body = Vec<ConversationResponse>), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn list_application_conversations(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(application_public_id): Path<String>,
    Query(query): Query<LifecycleListQuery>,
) -> Result<Json<Vec<ConversationResponse>>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    // Application and conversation lifecycles are independent. Read-only
    // history remains available after its parent application is archived;
    // mutating endpoints continue to use active-only authorization.
    let application = authorized_application_for_permissions(
        &state,
        &auth,
        &application_public_id,
        None,
        &[Permission::ProjectsRead],
    )
    .await?;
    let (page, page_size) = normalize_list_pagination(query.page, query.page_size);
    let conversations = state
        .applications
        .conversations_with_status(
            application.application.id,
            auth.user_id(),
            query.status.as_str(),
            page,
            page_size,
        )
        .await?;
    Ok(Json(
        conversations
            .into_iter()
            .map(ConversationResponse::from)
            .collect(),
    ))
}

#[utoipa::path(
    post, tag = "AI Applications", path = "/ai/applications/{application_public_id}/conversations",
    params(("application_public_id" = String, Path,)), request_body = CreateApplicationConversationRequest,
    responses((status = 201, body = ConversationResponse), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn create_application_conversation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(application_public_id): Path<String>,
    Json(request): Json<CreateApplicationConversationRequest>,
) -> Result<(StatusCode, Json<ConversationResponse>), Problem> {
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
    let harness_workspace = state
        .application_workspaces
        .ensure(&application.application.public_id, &application.projects)
        .await?;
    // Application ownership/context is represented by `application_id`.
    // Never manufacture a project authorization scope from the first link.
    let conversation_project_id = None;
    let context_id = format!(
        "{}:{}",
        application_public_id,
        uuid::Uuid::new_v4().simple()
    );
    if requested_thread_kind(request.kind.as_deref())? == ThreadKind::Terminal {
        // A terminal thread is a shell in the workspace sandbox: it needs what
        // opening one needs, so a role that could never attach is refused here
        // instead of being handed a thread that cannot connect.
        permission_guard!(auth, SandboxesExec);
        ensure_workspace_terminal_permissions(&auth, &state.project_access_checker, &project_ids)
            .await?;
        let conversation = state
            .service
            .create_terminal_conversation(
                Some(application.application.id),
                "application",
                &context_id,
                auth.user_id(),
                DEFAULT_TERMINAL_THREAD_TITLE,
            )
            .await?;
        state
            .audit(&ConversationCreatedAudit {
                context: audit_context(&auth, &metadata),
                project_id: conversation_project_id,
                conversation_id: conversation.public_id.clone(),
                context_type: "application".to_string(),
            })
            .await;
        return Ok((
            StatusCode::CREATED,
            Json(ConversationResponse::from(conversation)),
        ));
    }
    let harness = require_application_harness(request.ai_provider.as_deref())?;
    let runtime = state
        .service
        .resolve_get_or_create_runtime(
            conversation_project_id,
            "application",
            &context_id,
            auth.user_id(),
            Some(&harness),
            None,
            None,
            None,
        )
        .await?;
    ensure_runtime_permission(
        &auth,
        Some(&runtime.provider),
        Some(&runtime.permission_mode),
    )?;
    ensure_enabled(&state, Some(&runtime.provider)).await?;
    let conversation = state
        .service
        .get_or_create(
            conversation_project_id,
            "application",
            &context_id,
            auth.user_id(),
            Some(&runtime.provider),
            Some(&runtime.model),
            runtime.thinking_level.as_deref(),
            Some(&runtime.permission_mode),
        )
        .await?;
    // The workspace is derived and re-created above before the conversation
    // exists. Its opaque label is sufficient for sandbox recovery; the host
    // path remains private to the server-side workspace service.
    tracing::debug!(
        application_id = %application.application.public_id,
        sandbox_label = %harness_workspace.sandbox_label,
        "prepared managed application harness workspace"
    );
    state
        .audit(&ConversationCreatedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: conversation_project_id,
            conversation_id: conversation.public_id.clone(),
            context_type: "application".to_string(),
        })
        .await;
    Ok((
        StatusCode::CREATED,
        Json(ConversationResponse::from(conversation)),
    ))
}

/// Revoke data-plane access after any application authorization failure. A
/// network detach is the preferred path; if Docker cannot confirm it, stop
/// the compute so untrusted application code cannot keep using stale database
/// connections while the control plane reports access as denied.
pub(super) async fn quarantine_application_workspace(
    state: &AppState,
    user_id: i32,
    application_id: &str,
) {
    let application_internal_id = match state.applications.get(user_id, application_id).await {
        Ok(application) => Some(application.application.id),
        Err(error) => {
            tracing::error!(
                application_id,
                error = %error,
                "Failed to resolve application while quarantining its workspace"
            );
            None
        }
    };
    if let Some(application_internal_id) = application_internal_id {
        if let Err(error) = state
            .applications
            .update_workspace_runtime_state(
                application_internal_id,
                Some("quarantined"),
                Some("Workspace access was revoked because linked-project authorization could not be verified.".to_string()),
            )
            .await
        {
            tracing::error!(
                application_id,
                error = %error,
                "Failed to record fail-closed application workspace state; compute quarantine will still be attempted"
            );
        }
    }
    let Some(sandboxes) = state.application_sandboxes.as_ref() else {
        return;
    };
    let summary = match sandboxes
        .application_workspace_summary(user_id, application_id)
        .await
    {
        Ok(Some(summary)) => summary,
        Ok(None) => return,
        Err(error) => {
            tracing::error!(
                application_id,
                error = %error,
                "Failed to locate application workspace during authorization quarantine"
            );
            return;
        }
    };
    if let Err(error) = sandboxes
        .synchronize_application_data_network(user_id, application_id, &summary.public_id, &[])
        .await
    {
        tracing::error!(
            application_id,
            sandbox_id = %summary.public_id,
            error = %error,
            "Failed to detach application data network; stopping workspace compute"
        );
    }
    // Stop compute even after a successful detach: an already-open database
    // socket can otherwise outlive Docker network membership changes.
    if workspace_may_retain_data_plane(&summary.status) {
        if let Err(stop_error) = sandboxes.pause_sandbox(&summary.public_id, user_id).await {
            tracing::error!(
                application_id,
                sandbox_id = %summary.public_id,
                error = %stop_error,
                "Failed to stop quarantined application workspace"
            );
        }
    }
}

/// The linker the HTTP handlers share with the assistant's blueprint tool.
pub(super) fn project_linker(state: &AppState) -> ApplicationProjectLinker {
    ApplicationProjectLinker::new(
        state.db.clone(),
        state.applications.clone(),
        state.application_workspaces.clone(),
        state.application_sandboxes.clone(),
    )
}

pub(super) async fn synchronize_application_network_if_running(
    state: &AppState,
    auth: &AuthContext,
    application: &crate::applications::ApplicationWithProjects,
) -> Result<(), Problem> {
    synchronize_application_network(
        state.application_sandboxes.as_ref(),
        auth.user_id(),
        application,
    )
    .await
    .map_err(Problem::from)
}
