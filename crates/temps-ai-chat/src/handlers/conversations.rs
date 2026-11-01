// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Conversation routes: thread artifacts, listing and finding conversations,
//! creating global and assistant chats, the global AI workspace, reading,
//! diagnostics, stopping, archiving, restoring and renaming.

use super::*;

pub(super) fn validate_conversation_messages_query(
    query: &ConversationMessagesQuery,
) -> Result<(Option<i64>, u64), Problem> {
    let limit = query.limit.unwrap_or(DEFAULT_MESSAGE_PAGE_LIMIT);
    if limit == 0 || limit > MAX_MESSAGE_PAGE_LIMIT {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Message Page Limit")
            .with_detail(format!(
                "'limit' must be between 1 and {MAX_MESSAGE_PAGE_LIMIT}; received {limit}."
            )));
    }

    let before_message_id = query
        .before
        .as_deref()
        .map(decode_message_before_cursor)
        .transpose()
        .map_err(|error| {
            problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Message Cursor")
                .with_detail(format!(
                    "The 'before' cursor is invalid ({error}). Use a next_before cursor returned by this conversation endpoint."
                ))
        })?;

    Ok((before_message_id, limit))
}

#[utoipa::path(
    get, tag = "AI Applications", path = "/ai/applications/{application_public_id}/conversations/{conversation_public_id}/artifacts",
    params(("application_public_id" = String, Path,), ("conversation_public_id" = String, Path,)),
    responses((status = 200, body = Vec<ThreadArtifactResponse>), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn list_thread_artifacts(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((application_public_id, conversation_public_id)): Path<(String, String)>,
) -> Result<Json<Vec<ThreadArtifactResponse>>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let application = authorized_application(&state, &auth, &application_public_id).await?;
    let artifacts = state
        .applications
        .artifacts(
            application.application.id,
            &conversation_public_id,
            auth.user_id(),
        )
        .await?;
    Ok(Json(
        artifacts
            .into_iter()
            .map(ThreadArtifactResponse::from)
            .collect(),
    ))
}

#[utoipa::path(
    post, tag = "AI Applications", path = "/ai/applications/{application_public_id}/conversations/{conversation_public_id}/artifacts",
    params(("application_public_id" = String, Path,), ("conversation_public_id" = String, Path,)), request_body = CreateThreadArtifactRequest,
    responses((status = 201, body = ThreadArtifactResponse), (status = 400), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn create_thread_artifact(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((application_public_id, conversation_public_id)): Path<(String, String)>,
    Json(request): Json<CreateThreadArtifactRequest>,
) -> Result<(StatusCode, Json<ThreadArtifactResponse>), Problem> {
    permission_guard!(auth, ProjectsWrite);
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
    let artifact = state
        .applications
        .create_artifact(
            application.application.id,
            &conversation_public_id,
            auth.user_id(),
            &request.kind,
            request.title.as_deref(),
            request.payload,
        )
        .await?;
    state
        .audit(&ThreadArtifactCreatedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            application_id: application_public_id,
            conversation_id: conversation_public_id,
            artifact_id: artifact.public_id.clone(),
            kind: artifact.kind.clone(),
        })
        .await;
    state.service.publish_wire_event(
        artifact.conversation_id,
        "artifacts_changed",
        serde_json::json!({ "artifact_id": artifact.public_id }).to_string(),
    );
    Ok((
        StatusCode::CREATED,
        Json(ThreadArtifactResponse::from(artifact)),
    ))
}

/// Find the current user's existing chat for a context (returns `null` if none
/// yet). Conversations are private even between members of the same project.
#[utoipa::path(
    get, tag = "AI Chat",
    path = "/projects/{project_id}/ai/conversations",
    params(("project_id" = i32, Path,), ("context_type" = String, Query,), ("context_id" = String, Query,)),
    responses((status = 200, body = Option<ConversationResponse>), (status = 401), (status = 403)),
    security(("bearer_auth" = []))
)]
pub async fn find_conversation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(q): Query<FindConversationQuery>,
) -> Result<Json<Option<ConversationResponse>>, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    // Bound the lookup keys, consistent with create_conversation, so oversized
    // query strings can't reach the DB.
    if q.context_type.len() > MAX_CONTEXT_TYPE_LEN {
        return Err(too_long("context_type", MAX_CONTEXT_TYPE_LEN));
    }
    if q.context_id.len() > MAX_CONTEXT_ID_LEN {
        return Err(too_long("context_id", MAX_CONTEXT_ID_LEN));
    }
    ensure_context_read_permission(&auth, &q.context_type)?;
    let found = state
        .service
        .find_by_context(
            Some(project_id),
            auth.user_id(),
            &q.context_type,
            &q.context_id,
        )
        .await?;
    if let Some(conversation) = found.as_ref() {
        ensure_application_conversation_access(&state, &auth, conversation).await?;
    }
    Ok(Json(found.map(ConversationResponse::from)))
}

/// List the current user's active conversations across all projects,
/// most-recently-active first, annotated with project name/slug.
#[utoipa::path(
    get, tag = "AI Chat",
    path = "/ai/conversations",
    params(ConversationListQuery),
    responses((status = 200, body = Vec<GlobalConversationResponse>), (status = 401), (status = 403)),
    security(("bearer_auth" = []))
)]
pub async fn list_all_conversations(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Query(query): Query<ConversationListQuery>,
) -> Result<Json<Vec<GlobalConversationResponse>>, Problem> {
    permission_guard!(auth, ProjectsRead);
    // This global endpoint returns conversations across every project; a
    // project-scoped deployment/project token must not reach another tenant's
    // chats through it. Restrict to human/admin (user/API-key) principals.
    deny_deployment_token!(auth);
    validate_conversation_search(query.search.as_deref())?;
    let (page, page_size) = normalize_list_pagination(query.page, query.page_size);
    let hidden_project_ids =
        hidden_conversation_project_ids(&auth, &state.project_access_checker).await?;
    let visible_application_ids = if query.scope == ConversationListScope::All {
        // Conversation lifecycle is independent from application lifecycle:
        // archived threads may belong to archived applications, while an
        // active thread can outlive an application archive race. Authorize
        // against every application owned by the caller.
        Some(visible_application_ids_for_listing(&state, &auth, None).await?)
    } else {
        None
    };
    let hidden_context_types = hidden_conversation_context_types(&auth);
    let items = state
        .service
        .list_all_conversations_with_visibility(
            auth.user_id(),
            &hidden_project_ids,
            query.status.as_str(),
            query.scope == ConversationListScope::Global,
            page,
            page_size,
            visible_application_ids.as_deref(),
            &hidden_context_types,
            query.search.as_deref(),
        )
        .await?;
    // Workspace names for the assistant chats in a workspace, in one query.
    let mut workspace_ids: Vec<i64> = items
        .iter()
        .filter(|item| {
            item.conversation.context_type == crate::providers::assistant::ASSISTANT_CONTEXT_TYPE
        })
        .filter_map(|item| item.conversation.application_id)
        .collect();
    workspace_ids.sort_unstable();
    workspace_ids.dedup();
    let workspace_names = crate::workspace_chats::workspace_names(&state.db, &workspace_ids)
        .await
        .map_err(|error| {
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail(format!(
                    "Could not read the workspaces of user {}'s chats: {error}",
                    auth.user_id()
                ))
        })?;
    let mut conversations = Vec::with_capacity(items.len());
    for item in items {
        let workspace = (item.conversation.context_type
            == crate::providers::assistant::ASSISTANT_CONTEXT_TYPE)
            .then_some(item.conversation.application_id)
            .flatten()
            .and_then(|id| workspace_names.get(&id))
            .map(|(public_id, name)| ConversationWorkspaceResponse {
                public_id: public_id.clone(),
                name: name.clone(),
            });
        let failure = conversation_failure(
            item.conversation.metadata.as_ref(),
            &item.conversation.turn_status,
        );
        let context_usage = conversation_context_usage(item.conversation.metadata.as_ref());
        let focus_project_id =
            crate::service::focus_project_id(item.conversation.metadata.as_ref());
        conversations.push(GlobalConversationResponse {
            public_id: item.conversation.public_id,
            project_id: item.conversation.project_id,
            project_name: item.project_name,
            project_slug: item.project_slug,
            context_type: item.conversation.context_type,
            context_id: item.conversation.context_id,
            title: item.conversation.title,
            status: item.conversation.status,
            kind: item.conversation.kind,
            created_at: item.conversation.created_at.to_rfc3339(),
            last_activity_at: item.conversation.last_activity_at.to_rfc3339(),
            ai_provider: item.conversation.ai_provider,
            ai_model: item.conversation.ai_model,
            ai_thinking_level: item.conversation.ai_thinking_level,
            ai_permission_mode: item.conversation.ai_permission_mode,
            turn_status: item.conversation.turn_status,
            focus_project_id,
            failure,
            context_usage,
            sandbox: item.sandbox.map(|sandbox| ConversationSandboxResponse {
                sandbox_id: sandbox.sandbox_id,
                name: sandbox.name,
                backend: sandbox.backend,
            }),
            workspace,
        });
    }
    Ok(Json(conversations))
}

pub(super) fn parse_workspace_activity_ids(
    query: &WorkspaceActivityQuery,
) -> Result<Vec<String>, Problem> {
    let Some(raw) = query.application_public_ids.as_deref() else {
        return Ok(Vec::new());
    };
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let ids = raw.split(',').collect::<Vec<_>>();
    if ids.len() > 100
        || ids.iter().any(|id| {
            id.is_empty()
                || id.len() > 200
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
    {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Workspace Activity Scope")
            .with_detail("Provide at most 100 comma-separated application public IDs."));
    }
    let mut seen = std::collections::HashSet::new();
    Ok(ids
        .into_iter()
        .filter(|id| seen.insert(*id))
        .map(str::to_string)
        .collect())
}

/// Aggregate all active threads for the caller's global workspace and up to
/// 100 requested, visible application workspaces. Thread history is not paged.
#[utoipa::path(
    get, tag = "AI Chat",
    path = "/ai/workspace-activity",
    operation_id = "get_workspace_activity",
    params(WorkspaceActivityQuery),
    responses((status = 200, body = WorkspaceActivityResponse), (status = 400), (status = 401), (status = 403)),
    security(("bearer_auth" = []))
)]
pub async fn get_workspace_activity(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Query(query): Query<WorkspaceActivityQuery>,
) -> Result<Json<WorkspaceActivityResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let requested = parse_workspace_activity_ids(&query)?;
    let candidates = state
        .applications
        .project_scopes_for_listing_bounded(auth.user_id(), Some("active"), Some(&requested))
        .await?
        .into_iter()
        .collect::<Vec<_>>();
    let project_ids = candidates
        .iter()
        .flat_map(|(_, scope)| scope.project_ids.iter().copied())
        .collect::<Vec<_>>();
    let access =
        application_project_access_map(&auth, &state.project_access_checker, &project_ids).await?;
    let mut visible_by_public = HashMap::new();
    for (id, scope) in candidates {
        let visible = access.as_ref().is_none_or(|access| {
            scope
                .project_ids
                .iter()
                .all(|project_id| access.get(project_id).copied().unwrap_or(false))
        });
        if visible {
            visible_by_public.insert(scope.public_id, id);
        }
    }
    let visible = requested
        .into_iter()
        .filter_map(|public_id| {
            visible_by_public
                .remove(&public_id)
                .map(|id| (id, public_id))
        })
        .collect::<Vec<_>>();
    let visible_ids = visible.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    let hidden_project_ids =
        hidden_conversation_project_ids(&auth, &state.project_access_checker).await?;
    let counts = state
        .service
        .workspace_activity_counts(auth.user_id(), &hidden_project_ids, &visible_ids)
        .await?;
    let mut grouped: HashMap<Option<i64>, HashMap<String, WorkspaceHarnessActivity>> =
        HashMap::new();
    for row in counts {
        let harness = grouped
            .entry(row.application_id)
            .or_default()
            .entry(row.ai_provider.clone())
            .or_insert_with(|| WorkspaceHarnessActivity {
                ai_provider: row.ai_provider,
                total: 0,
                pending: 0,
                running: 0,
                completed: 0,
                failed: 0,
                cancelled: 0,
                idle: 0,
            });
        harness.add_status(&row.turn_status, row.thread_count.max(0) as u64);
    }
    let mut workspace_keys = vec![(None, None)];
    workspace_keys.extend(
        visible
            .into_iter()
            .map(|(id, public_id)| (Some(id), Some(public_id))),
    );
    let workspaces = workspace_keys
        .into_iter()
        .map(|(id, application_public_id)| {
            let mut harnesses = grouped
                .remove(&id)
                .unwrap_or_default()
                .into_values()
                .collect::<Vec<_>>();
            harnesses.sort_by(|a, b| a.ai_provider.cmp(&b.ai_provider));
            WorkspaceActivitySummary {
                application_public_id,
                total: harnesses
                    .iter()
                    .fold(0_u64, |total, harness| total.saturating_add(harness.total)),
                harnesses,
            }
        })
        .collect();
    Ok(Json(WorkspaceActivityResponse { workspaces }))
}

/// Create a private user-owned operator thread without binding its lifetime or
/// authority to a project. Project access is selected per tool call and checked
/// against the user's current role and memberships.
#[utoipa::path(
    post, tag = "AI Chat",
    path = "/ai/conversations",
    request_body = CreateGlobalConversationRequest,
    responses((status = 201, body = ConversationResponse), (status = 400), (status = 401), (status = 403), (status = 409), (status = 503)),
    security(("bearer_auth" = []))
)]
pub async fn create_global_conversation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateGlobalConversationRequest>,
) -> Result<(StatusCode, Json<ConversationResponse>), Problem> {
    permission_guard!(auth, ProjectsRead);
    permission_guard!(auth, SandboxesWrite);
    deny_deployment_token!(auth);
    let context_id = format!("global_{}", uuid::Uuid::new_v4().simple());
    if requested_thread_kind(request.kind.as_deref())? == ThreadKind::Terminal {
        require_global_terminal_permissions(&auth)?;
        let conversation = state
            .service
            .create_terminal_conversation(
                None,
                "global",
                &context_id,
                auth.user_id(),
                DEFAULT_TERMINAL_THREAD_TITLE,
            )
            .await?;
        state
            .audit(&ConversationCreatedAudit {
                context: audit_context(&auth, &metadata),
                project_id: None,
                conversation_id: conversation.public_id.clone(),
                context_type: "global".to_string(),
            })
            .await;
        return Ok((
            StatusCode::CREATED,
            Json(ConversationResponse::from(conversation)),
        ));
    }
    let harness = require_application_harness(request.ai_provider.as_deref())?;
    // Global conversations are distinct, but their durable execution boundary
    // is user-scoped. Reusing one bounded workspace prevents a user from
    // allocating a new persistent container and volume for every thread.
    let workspace_context_id = global_workspace_context_id(auth.user_id());
    let runtime = state
        .service
        .resolve_get_or_create_runtime(
            None,
            "global",
            &context_id,
            auth.user_id(),
            Some(&harness),
            request.ai_model.as_deref(),
            request.ai_thinking_level.as_deref(),
            request.ai_permission_mode.as_deref(),
        )
        .await?;
    ensure_runtime_permission(
        &auth,
        Some(&runtime.provider),
        Some(&runtime.permission_mode),
    )?;
    if !state
        .service
        .ai_available_for(Some(&runtime.provider))
        .await
    {
        return Err(problemdetails::new(StatusCode::CONFLICT)
            .with_title("AI Harness Not Ready")
            .with_detail(
                "The selected development harness is not authenticated on this Temps instance.",
            ));
    }

    // Provision the managed boundary before persisting the thread so creation
    // cannot succeed with a harness that would later fall back to host access.
    let workspace = state
        .application_workspaces
        .ensure(&workspace_context_id, &[])
        .await?;
    let sandboxes = state.application_sandboxes.as_ref().ok_or_else(|| {
        problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
            .with_title("Global Chat Sandbox Unavailable")
            .with_detail("The instance sandbox service is not configured.")
    })?;
    sandboxes
        .get_or_create_application_workspace(
            auth.user_id(),
            &workspace_context_id,
            None,
            workspace.host_work_dir,
        )
        .await
        .map_err(|error| {
            application_workspace_provider_problem(
                error,
                &workspace_context_id,
                "start global workspace",
            )
        })?;

    let conversation = state
        .service
        .get_or_create(
            None,
            "global",
            &context_id,
            auth.user_id(),
            Some(&runtime.provider),
            Some(&runtime.model),
            runtime.thinking_level.as_deref(),
            Some(&runtime.permission_mode),
        )
        .await?;
    state
        .audit(&ConversationCreatedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: None,
            conversation_id: conversation.public_id.clone(),
            context_type: "global".to_string(),
        })
        .await;
    Ok((
        StatusCode::CREATED,
        Json(ConversationResponse::from(conversation)),
    ))
}

/// The calling client, attached to assistant-issued API calls so the audit trail
/// of what the assistant did names who approved it and from where.
pub(super) fn request_origin(metadata: &RequestMetadata) -> temps_core::ai_tool_call::AiToolOrigin {
    temps_core::ai_tool_call::AiToolOrigin {
        ip_address: metadata.ip_address.clone(),
        user_agent: metadata.user_agent.clone(),
    }
}

/// Console path where an operator adds the AI provider key the assistant needs.
const ASSISTANT_SETUP_PATH: &str = "/ai-gateway?add=openrouter";

/// Start a chat with the Temps assistant.
///
/// The assistant runs on the AI gateway (an OpenRouter key is enough), has no
/// sandbox, and acts with the calling user's own permissions: every tool call
/// is replayed through the real router with their auth, and every change is a
/// proposal the user confirms in the UI. When no provider is configured this
/// answers `409` with `setup_path`, so the client can onboard instead of
/// failing.
#[utoipa::path(
    post, tag = "AI Chat",
    path = "/ai/assistant/conversations",
    request_body = CreateAssistantConversationRequest,
    responses(
        (status = 201, description = "Conversation created", body = ConversationResponse),
        (status = 400, description = "Provider is not an AI-gateway provider"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 409, description = "No AI provider is configured; the problem carries `setup_path`")
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_assistant_conversation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateAssistantConversationRequest>,
) -> Result<(StatusCode, Json<ConversationResponse>), Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    if let Some(project_id) = request.project_id {
        ensure_application_project_permission(
            &auth,
            &state.project_access_checker,
            &[project_id],
            &Permission::ProjectsRead,
        )
        .await?;
    }
    let conversation = state
        .service
        .create_assistant_conversation(
            auth.user_id(),
            request.ai_provider.as_deref(),
            request.ai_model.as_deref(),
            request.project_id,
        )
        .await
        .map_err(|error| match error {
            ChatError::AiUnavailable => problemdetails::new(StatusCode::CONFLICT)
                .with_title("AI Not Configured")
                .with_detail(
                    "No AI provider is configured. Add an OpenRouter API key to start managing Temps by chat.",
                )
                .with_value("setup_path", ASSISTANT_SETUP_PATH),
            ChatError::Ai(message) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Assistant Provider Not Usable")
                .with_detail(message),
            other => Problem::from(other),
        })?;
    state
        .audit(&ConversationCreatedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: request.project_id,
            conversation_id: conversation.public_id.clone(),
            context_type: "assistant".to_string(),
        })
        .await;
    Ok((
        StatusCode::CREATED,
        Json(ConversationResponse::from(conversation)),
    ))
}

pub(super) fn global_workspace_context_id(user_id: i32) -> String {
    format!("global-user-{user_id}")
}

#[utoipa::path(
    get, tag = "AI Chat",
    path = "/ai/workspace",
    operation_id = "get_global_ai_workspace",
    responses((status = 200, body = ApplicationWorkspaceResponse), (status = 401), (status = 403), (status = 503)),
    security(("bearer_auth" = []))
)]
pub async fn get_global_ai_workspace(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Result<Json<ApplicationWorkspaceResponse>, Problem> {
    permission_guard!(auth, SandboxesRead);
    deny_deployment_token!(auth);
    let workspace_id = global_workspace_context_id(auth.user_id());
    let sandboxes = state.application_sandboxes.as_ref().ok_or_else(|| {
        problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
            .with_title("Global Workspace Unavailable")
            .with_detail("The instance sandbox service is not configured.")
    })?;
    let summary = sandboxes
        .application_workspace_summary(auth.user_id(), &workspace_id)
        .await
        .map_err(|error| {
            application_workspace_provider_problem(error, &workspace_id, "inspect global workspace")
        })?;
    let defaults = temps_sandbox::services::ApplicationWorkspaceConfig::default();
    let mut diagnostic = None;
    let usage = if let Some(summary) = summary
        .as_ref()
        .filter(|summary| summary.status == "running")
    {
        match sandboxes
            .application_workspace_usage(auth.user_id(), &summary.public_id)
            .await
        {
            Ok(usage) => usage,
            Err(error) => {
                tracing::warn!(
                    user_id = auth.user_id(),
                    sandbox_id = %summary.public_id,
                    error = %error,
                    "Could not read global workspace resource usage"
                );
                diagnostic =
                    Some("Workspace resource usage is temporarily unavailable.".to_string());
                temps_sandbox::services::ApplicationWorkspaceUsage::default()
            }
        }
    } else {
        temps_sandbox::services::ApplicationWorkspaceUsage::default()
    };
    let state_name = application_workspace_state(
        summary.as_ref().map(|summary| summary.status.as_str()),
        &defaults.desired_state,
        summary.is_some(),
        diagnostic.is_some(),
    );
    let volume_path = state.application_workspaces.root().join(&workspace_id);
    let persistent_volume_healthy = tokio::fs::metadata(volume_path)
        .await
        .is_ok_and(|metadata| metadata.is_dir());

    Ok(Json(ApplicationWorkspaceResponse {
        state: state_name.to_string(),
        desired_state: defaults.desired_state,
        sandbox_public_id: summary.as_ref().map(|summary| summary.public_id.clone()),
        runtime: "node".to_string(),
        image: summary
            .as_ref()
            .and_then(|summary| summary.image.clone())
            .or(defaults.image),
        runtime_compatible: None,
        runtime_update_available: false,
        runtime_update_image: None,
        runtime_update_error: None,
        cpu_limit: defaults.cpu_limit,
        memory_limit_mb: defaults.memory_limit_mb as i64,
        pids_limit: defaults.pids_limit,
        disk_limit_mb: defaults.disk_limit_mb as i64,
        disk_limit_enforced: summary
            .as_ref()
            .and_then(|summary| summary.backend.as_deref())
            == Some("firecracker"),
        idle_timeout_secs: defaults.idle_timeout_secs as i64,
        memory_used_bytes: usage.memory_used_bytes,
        pids_used: usage.pids_used,
        disk_used_bytes: usage.disk_used_bytes,
        cpu_usage_usec: usage.cpu_usage_usec,
        open_preview_ports: usage.open_ports,
        persistent_volume_healthy,
        data_network_service_count: 0,
        last_error: diagnostic,
        snapshot_id: None,
    }))
}

/// Parse the `kind` of a thread-creation request.
pub(super) fn requested_thread_kind(kind: Option<&str>) -> Result<ThreadKind, Problem> {
    ThreadKind::parse(kind).map_err(|error| match error {
        ThreadKindError::Unknown { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Unknown Thread Kind")
            .with_detail(error.to_string()),
    })
}

/// A terminal thread runs a shell, not an AI turn: it never accepts messages.
pub(super) fn ensure_conversation_takes_messages(
    conversation: &ai_conversations::Model,
) -> Result<(), Problem> {
    if conversation.kind == THREAD_KIND_TERMINAL {
        return Err(problemdetails::new(StatusCode::CONFLICT)
            .with_title("Terminal Threads Take No Messages")
            .with_detail(format!(
                "Thread {} is a terminal. Type into the terminal instead of sending a message.",
                conversation.public_id
            )));
    }
    ensure_conversation_is_active(conversation)
}

pub(super) fn ensure_conversation_is_active(
    conversation: &ai_conversations::Model,
) -> Result<(), Problem> {
    if conversation.status == "active" {
        return Ok(());
    }
    Err(problemdetails::new(StatusCode::CONFLICT)
        .with_title("Conversation Is Archived")
        .with_detail("Restore this conversation before sending another message."))
}

/// List the current user's active conversations for a project,
/// most-recently-active first.
#[utoipa::path(
    get, tag = "AI Chat",
    path = "/projects/{project_id}/ai/conversations/list",
    params(("project_id" = i32, Path,)),
    responses((status = 200, body = Vec<ConversationResponse>), (status = 401), (status = 403)),
    security(("bearer_auth" = []))
)]
pub async fn list_conversations(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
) -> Result<Json<Vec<ConversationResponse>>, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let conversations = state
        .service
        .list_conversations(project_id, auth.user_id())
        .await?;
    let application_ids = conversations
        .iter()
        .filter_map(|conversation| conversation.application_id)
        .collect::<Vec<_>>();
    let (application_scopes, application_access) =
        application_list_access(&state, &auth, &application_ids).await?;
    let mut responses = Vec::with_capacity(conversations.len());
    for conversation in conversations {
        if !can_read_context(&auth, &conversation.context_type) {
            continue;
        }
        if !application_conversation_is_visible(
            &state,
            &auth,
            &conversation,
            &application_scopes,
            &application_access,
        )
        .await?
        {
            continue;
        }
        responses.push(ConversationResponse::from(conversation));
    }
    Ok(Json(responses))
}

/// Get-or-create the current user's private chat for a context.
#[utoipa::path(
    post, tag = "AI Chat",
    path = "/projects/{project_id}/ai/conversations",
    params(("project_id" = i32, Path,)),
    request_body = CreateConversationRequest,
    responses((status = 200, body = ConversationResponse), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn create_conversation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(req): Json<CreateConversationRequest>,
) -> Result<Json<ConversationResponse>, Problem> {
    // Creating a conversation mutates state and can drive AI cost → write scope.
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    if req.context_type.len() > MAX_CONTEXT_TYPE_LEN {
        return Err(too_long("context_type", MAX_CONTEXT_TYPE_LEN));
    }
    if req.context_id.len() > MAX_CONTEXT_ID_LEN {
        return Err(too_long("context_id", MAX_CONTEXT_ID_LEN));
    }
    if req.context_type == "application" {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Use the AI Application Thread Endpoint")
            .with_detail(
                "Application threads must be created through /ai/applications/{application_id}/conversations so access can be checked across every linked project.",
            ));
    }
    ensure_context_read_permission(&auth, &req.context_type)?;
    let runtime = state
        .service
        .resolve_get_or_create_runtime(
            Some(project_id),
            &req.context_type,
            &req.context_id,
            auth.user_id(),
            req.ai_provider.as_deref(),
            req.ai_model.as_deref(),
            req.ai_thinking_level.as_deref(),
            req.ai_permission_mode.as_deref(),
        )
        .await?;
    ensure_runtime_permission(
        &auth,
        Some(&runtime.provider),
        Some(&runtime.permission_mode),
    )?;
    ensure_enabled(&state, Some(&runtime.provider)).await?;
    let conv = state
        .service
        .get_or_create(
            Some(project_id),
            &req.context_type,
            &req.context_id,
            auth.user_id(),
            Some(&runtime.provider),
            Some(&runtime.model),
            runtime.thinking_level.as_deref(),
            Some(&runtime.permission_mode),
        )
        .await?;
    state
        .audit(&ConversationCreatedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: Some(project_id),
            conversation_id: conv.public_id.clone(),
            context_type: conv.context_type.clone(),
        })
        .await;
    Ok(Json(ConversationResponse::from(conv)))
}

/// One bounded conversation-history page (excluding internal context rows).
#[utoipa::path(
    get, tag = "AI Chat",
    path = "/projects/{project_id}/ai/conversations/{public_id}",
    params(
        ("project_id" = i32, Path,),
        ("public_id" = String, Path,),
        ("before" = Option<String>, Query, description = "Opaque next_before cursor returned by the previous page"),
        ("limit" = Option<u64>, Query, description = "Messages per page (default 50, maximum 100)"),
    ),
    responses((status = 200, body = ConversationDetailResponse), (status = 400), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn get_conversation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, public_id)): Path<(i32, String)>,
    Query(query): Query<ConversationMessagesQuery>,
) -> Result<Json<ConversationDetailResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let (before_message_id, limit) = validate_conversation_messages_query(&query)?;
    let conv = state
        .service
        .get_by_public_id(project_id, auth.user_id(), &public_id)
        .await?;
    ensure_application_conversation_access(&state, &auth, &conv).await?;
    ensure_conversation_read_permission(&auth, &conv)?;
    let message_page = state
        .service
        .messages_page(conv.id, before_message_id, limit)
        .await?;
    let messages = message_page
        .messages
        .into_iter()
        .map(MessageResponse::from)
        .collect();
    let pending_permission = state.service.pending_permission_for(&conv.public_id);
    Ok(Json(ConversationDetailResponse {
        conversation: ConversationResponse::from(conv),
        messages,
        page: ConversationMessagePageResponse {
            has_more: message_page.has_more,
            next_before: message_page.next_before,
        },
        pending_permission,
    }))
}

/// Load one bounded history page for a private conversation by its owner-facing
/// id. Project/application context is revalidated, but never used as the
/// ownership key.
#[utoipa::path(
    get, tag = "AI Chat", path = "/ai/conversations/{public_id}",
    params(
        ("public_id" = String, Path,),
        ("before" = Option<String>, Query, description = "Opaque next_before cursor returned by the previous page"),
        ("limit" = Option<u64>, Query, description = "Messages per page (default 50, maximum 100)"),
    ),
    responses((status = 200, body = ConversationDetailResponse), (status = 400), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn get_user_conversation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(public_id): Path<String>,
    Query(query): Query<ConversationMessagesQuery>,
) -> Result<Json<ConversationDetailResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let (before_message_id, limit) = validate_conversation_messages_query(&query)?;
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    let message_page = state
        .service
        .messages_page(conversation.id, before_message_id, limit)
        .await?;
    let messages = message_page
        .messages
        .into_iter()
        .map(MessageResponse::from)
        .collect();
    let pending_permission = state
        .service
        .pending_permission_for(&conversation.public_id);
    Ok(Json(ConversationDetailResponse {
        conversation: ConversationResponse::from(conversation),
        messages,
        page: ConversationMessagePageResponse {
            has_more: message_page.has_more,
            next_before: message_page.next_before,
        },
        pending_permission,
    }))
}

/// What this conversation has cost so far.
///
/// Read from the AI gateway's usage log for the conversation's owner only.
/// Figures are estimates: requests to a model with no known price are counted
/// in `unpriced_requests` instead of being silently priced at zero.
#[utoipa::path(
    get,
    operation_id = "getUserConversationUsage",
    tag = "AI Chat",
    path = "/ai/conversations/{public_id}/usage",
    params(("public_id" = String, Path,)),
    responses(
        (status = 200, body = crate::usage::ConversationUsage),
        (status = 401),
        (status = 403),
        (status = 404),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_user_conversation_usage(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(public_id): Path<String>,
) -> Result<Json<crate::usage::ConversationUsage>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    let usage = state
        .service
        .usage_for_owned_conversation(&conversation, auth.user_id())
        .await?;
    Ok(Json(usage))
}

/// Export the bounded, server-redacted diagnostic state Temps currently owns.
#[utoipa::path(
    get,
    operation_id = "getUserConversationDiagnostics",
    tag = "AI Chat",
    path = "/ai/conversations/{public_id}/diagnostics",
    params(
        ("public_id" = String, Path,),
        ConversationDiagnosticsQuery,
    ),
    responses(
        (status = 200, body = ConversationDiagnosticResponse),
        (status = 401),
        (status = 403),
        (status = 404),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_user_conversation_diagnostics(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(public_id): Path<String>,
    Query(query): Query<ConversationDiagnosticsQuery>,
) -> Result<Response, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    let limit = query.limit.unwrap_or(DIAGNOSTIC_MAX_MESSAGES);
    if limit == 0 || limit > DIAGNOSTIC_MAX_MESSAGES {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid diagnostic message limit")
            .with_detail(format!(
                "limit must be between 1 and {DIAGNOSTIC_MAX_MESSAGES}"
            )));
    }
    let snapshot = state
        .service
        .diagnostic_messages(conversation.id, limit)
        .await?;
    let (native_session, native_session_status, native_session_note) = match state
        .service
        .export_native_session(&conversation, auth.user_id())
        .await
    {
        crate::service::NativeSessionDiagnostic::Available(export) => {
            match (export.json.len() <= DIAGNOSTIC_MAX_NATIVE_BYTES)
                .then(|| serde_json::from_str::<serde_json::Value>(&export.json))
                .transpose()
            {
                Ok(Some(json)) => (
                    Some(ConversationNativeDiagnosticResponse {
                        provider: export.provider,
                        session_id: export.session_id,
                        format: export.format,
                        json: redact_value(&json),
                        truncated: export.truncated,
                    }),
                    "available",
                    "The provider-native session is sanitized and bounded. OpenCode removes tool inputs and outputs; terminal-only events absent from normalized history may still appear structurally in this export.",
                ),
                Ok(None) | Err(_) => (
                    None,
                    "invalid_export",
                    "The provider returned a native session export that could not be safely decoded as JSON.",
                ),
            }
        }
        crate::service::NativeSessionDiagnostic::NotAvailable => (
            None,
            "not_available",
            "No active, supported provider-native session export is available. Normalized persisted history is still included.",
        ),
        crate::service::NativeSessionDiagnostic::Busy => (
            None,
            "busy",
            "The provider-native session is busy with another operation. Normalized persisted history is still included; refresh diagnostics after the turn finishes.",
        ),
        crate::service::NativeSessionDiagnostic::Error => (
            None,
            "error",
            "The provider-native session could not be exported safely. Normalized persisted history is still included.",
        ),
    };
    let mut any_truncated = snapshot.truncated;
    let messages: Vec<ConversationDiagnosticMessageResponse> = snapshot
        .messages
        .into_iter()
        .map(|message| {
            let (content, content_truncated) =
                bounded_redacted_text(&message.content, DIAGNOSTIC_MAX_TEXT_BYTES);
            let (metadata, metadata_truncated) = message
                .metadata
                .as_ref()
                .map(bounded_redacted_metadata)
                .map_or((None, false), |(metadata, truncated)| {
                    (Some(metadata), truncated)
                });
            let truncated = content_truncated || metadata_truncated;
            any_truncated |= truncated;
            ConversationDiagnosticMessageResponse {
                role: message.role,
                content,
                metadata,
                tokens_in: message.tokens_in,
                tokens_out: message.tokens_out,
                cost_microcents: message.cost_microcents,
                created_at: message.created_at.to_rfc3339(),
                truncated,
            }
        })
        .collect();
    let pending_permission = state
        .service
        .pending_permission_for(&conversation.public_id)
        .and_then(|permission| serde_json::to_value(permission).ok())
        .map(|value| bounded_redacted_metadata(&value).0);
    let context_id = bounded_redacted_text(&conversation.context_id, 1024).0;
    let context_usage = conversation_context_usage(conversation.metadata.as_ref());
    let response = ConversationDiagnosticResponse {
        schema_version: 1,
        conversation: ConversationDiagnosticRuntimeResponse {
            public_id: conversation.public_id,
            context_type: conversation.context_type,
            context_id,
            provider: conversation.ai_provider,
            model: conversation.ai_model,
            thinking_level: conversation.ai_thinking_level,
            permission_mode: conversation.ai_permission_mode,
            turn_status: conversation.turn_status,
            active_turn_id: conversation.active_turn_id,
            last_turn_id: conversation.last_turn_id,
            turn_started_at: conversation.turn_started_at.map(|value| value.to_rfc3339()),
            native_session_id: conversation.cli_session_id,
            context_usage,
        },
        source: ConversationDiagnosticSourceResponse {
            normalized_history: true,
            native_session_status,
            native_session_note,
        },
        native_session,
        returned_message_count: messages.len(),
        message_limit: limit,
        messages,
        truncated: any_truncated,
        pending_permission,
    };
    Ok((diagnostic_response_headers(), Json(response)).into_response())
}

/// Explicitly cancel the server-owned active turn. Closing or refreshing a
/// browser only detaches its stream; this endpoint is the sole UI cancellation
/// path so execution lifetime is not coupled to connectivity.
#[utoipa::path(
    post, tag = "AI Chat",
    path = "/projects/{project_id}/ai/conversations/{public_id}/stop",
    params(("project_id" = i32, Path,), ("public_id" = String, Path,)),
    responses((status = 204), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn stop_turn(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, public_id)): Path<(i32, String)>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let conversation = state
        .service
        .get_by_public_id(project_id, auth.user_id(), &public_id)
        .await?;
    ensure_application_conversation_access(&state, &auth, &conversation).await?;
    ensure_conversation_read_permission(&auth, &conversation)?;
    state.service.cancel_turn(&conversation).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post, tag = "AI Chat", path = "/ai/conversations/{public_id}/stop",
    params(("public_id" = String, Path,)), responses((status = 204), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn stop_user_turn(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(public_id): Path<String>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    ensure_user_conversation_write_permission(&auth, &conversation)?;
    state.service.cancel_turn(&conversation).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Archive (soft-delete) a conversation.
#[utoipa::path(
    post, tag = "AI Chat",
    path = "/projects/{project_id}/ai/conversations/{public_id}/archive",
    params(("project_id" = i32, Path,), ("public_id" = String, Path,)),
    responses((status = 204), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn archive_conversation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, public_id)): Path<(i32, String)>,
) -> Result<axum::http::StatusCode, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let conv = state
        .service
        .get_by_public_id(project_id, auth.user_id(), &public_id)
        .await?;
    ensure_application_conversation_access(&state, &auth, &conv).await?;
    ensure_conversation_read_permission(&auth, &conv)?;
    state.service.archive(&conv).await?;
    state
        .audit(&ConversationArchivedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: Some(project_id),
            conversation_id: conv.public_id.clone(),
        })
        .await;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post, tag = "AI Chat", path = "/ai/conversations/{public_id}/archive",
    params(("public_id" = String, Path,)), responses((status = 204), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn archive_user_conversation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(public_id): Path<String>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    ensure_user_conversation_write_permission(&auth, &conversation)?;
    state.service.archive(&conversation).await?;
    state
        .audit(&ConversationArchivedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: conversation.project_id,
            conversation_id: conversation.public_id,
        })
        .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Restore a user-owned archived conversation.
#[utoipa::path(
    post, tag = "AI Chat", path = "/ai/conversations/{public_id}/restore",
    params(("public_id" = String, Path,)), responses((status = 204), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn restore_user_conversation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(public_id): Path<String>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    ensure_user_conversation_write_permission(&auth, &conversation)?;
    state.service.restore(&conversation).await?;
    state
        .audit(&ConversationRestoredAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: conversation.project_id,
            conversation_id: conversation.public_id,
        })
        .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Rename a conversation (set its human-facing title).
#[utoipa::path(
    patch, tag = "AI Chat",
    path = "/projects/{project_id}/ai/conversations/{public_id}",
    params(("project_id" = i32, Path,), ("public_id" = String, Path,)),
    request_body = RenameConversationRequest,
    responses((status = 200, body = ConversationResponse), (status = 400), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn rename_conversation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, public_id)): Path<(i32, String)>,
    Json(req): Json<RenameConversationRequest>,
) -> Result<Json<ConversationResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let title = req.title.trim();
    if title.is_empty() {
        return Err(problemdetails::new(axum::http::StatusCode::BAD_REQUEST)
            .with_title("Invalid Title")
            .with_detail("Conversation title cannot be empty."));
    }
    if title.len() > MAX_TITLE_LEN {
        return Err(too_long("title", MAX_TITLE_LEN));
    }

    let conv = state
        .service
        .get_by_public_id(project_id, auth.user_id(), &public_id)
        .await?;
    ensure_application_conversation_access(&state, &auth, &conv).await?;
    ensure_context_read_permission(&auth, &conv.context_type)?;
    let updated = state.service.rename(&conv, title).await?;

    state
        .audit(&ConversationRenamedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: Some(project_id),
            conversation_id: updated.public_id.clone(),
            title: title.to_string(),
        })
        .await;

    Ok(Json(ConversationResponse::from(updated)))
}

#[utoipa::path(
    patch, tag = "AI Chat", path = "/ai/conversations/{public_id}",
    params(("public_id" = String, Path,)), request_body = RenameConversationRequest,
    responses((status = 200, body = ConversationResponse), (status = 400), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn rename_user_conversation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(public_id): Path<String>,
    Json(req): Json<RenameConversationRequest>,
) -> Result<Json<ConversationResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let title = req.title.trim();
    if title.is_empty() {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Title")
            .with_detail("Conversation title cannot be empty."));
    }
    if title.len() > MAX_TITLE_LEN {
        return Err(too_long("title", MAX_TITLE_LEN));
    }
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    ensure_user_conversation_write_permission(&auth, &conversation)?;
    let updated = state.service.rename(&conversation, title).await?;
    state
        .audit(&ConversationRenamedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: updated.project_id,
            conversation_id: updated.public_id.clone(),
            title: title.to_string(),
        })
        .await;
    Ok(Json(ConversationResponse::from(updated)))
}
