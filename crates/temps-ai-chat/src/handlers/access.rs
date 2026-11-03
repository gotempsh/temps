// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Authorization checks shared by the handlers: runtime permissions, context and
//! conversation visibility, and application and project access.

use super::*;

/// Host CLIs execute as the Temps host user, so project write access alone is
/// not sufficient. The stronger check is repeated on every send because the
/// creator's provider administration permission may have been revoked.
pub(super) fn ensure_runtime_permission(
    auth: &AuthContext,
    provider: Option<&str>,
    permission_mode: Option<&str>,
) -> Result<(), Problem> {
    if let Some(provider) = provider.and_then(temps_ai_cli::find_provider) {
        let has_host_access = match provider.host_access_requirement {
            temps_ai_cli::HostAccessRequirement::AiGatewayWrite => {
                auth.has_permission(&Permission::AiGatewayWrite)
            }
            temps_ai_cli::HostAccessRequirement::SystemAdmin => {
                auth.has_permission(&Permission::SystemAdmin)
            }
        };
        if !has_host_access {
            return Err(problemdetails::new(axum::http::StatusCode::FORBIDDEN)
                .with_title("Host AI Provider Access Denied")
                .with_detail(format!(
                    "Your role cannot run the host-authenticated {} provider.",
                    provider.name
                )));
        }
        let requires_system_admin = permission_mode
            .and_then(|mode| {
                provider
                    .permission_modes
                    .iter()
                    .find(|option| option.id == mode)
            })
            .is_some_and(|mode| mode.requires_system_admin);
        if requires_system_admin && !auth.has_permission(&Permission::SystemAdmin) {
            return Err(problemdetails::new(axum::http::StatusCode::FORBIDDEN)
                .with_title("Full AI Access Denied")
                .with_detail("This AI permission mode requires system administrator permission."));
        }
    }
    Ok(())
}

/// Application threads are agent-runtime workspaces, not generic inference
/// sessions. Requiring a catalog harness here keeps gateway API keys from
/// gaining filesystem, MCP, or sandbox execution reachability through this
/// endpoint. The catalog lookup is deliberately general: adding a future
/// harness needs no handler allowlist change.
pub(super) fn require_application_harness(provider: Option<&str>) -> Result<String, Problem> {
    let Some(provider) = provider.filter(|provider| !provider.trim().is_empty()) else {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Development Harness Required")
            .with_detail(
                "Choose an authenticated development harness before creating an application thread.",
            ));
    };
    let registration = temps_ai_cli::find_provider(provider).ok_or_else(|| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Development Harness")
            .with_detail(format!(
                "'{provider}' is not a registered development harness. Choose a harness from Agent Sandbox settings.",
            ))
    })?;
    if !registration.workspace_chat_supported {
        return Err(problemdetails::new(StatusCode::CONFLICT)
            .with_title("Workspace Harness Not Supported")
            .with_detail(format!(
                "{} can run host workflows, but its secure persistent-workspace relay is not implemented yet. Choose a workspace-ready harness.",
                registration.name
            )));
    }
    Ok(registration.id.to_string())
}

/// Gate for create/send: the selected AI provider must be ready to run a turn.
pub(super) async fn ensure_enabled(
    state: &AppState,
    provider: Option<&str>,
) -> Result<(), Problem> {
    if !state.service.ai_available_for(provider).await {
        // Prefer the adapter's precise reason (e.g. a self-hosted key that
        // does not declare streaming) over the generic setup advice.
        let detail = state
            .service
            .ai_unavailable_reason(provider)
            .await
            .unwrap_or_else(|| {
                "The selected AI provider is not ready for chat. Configure a gateway key or \
                 authenticate the selected host harness, then try again."
                    .to_string()
            });
        return Err(problemdetails::new(axum::http::StatusCode::CONFLICT)
            .with_title("AI Not Configured")
            .with_detail(detail));
    }
    Ok(())
}

/// Contexts can expose domain data beyond the generic project-chat surface.
/// Enforce the same domain permission as the canonical API before creating,
/// reading, or running one of those conversations.
pub(super) fn ensure_context_read_permission(
    auth: &AuthContext,
    context_type: &str,
) -> Result<(), Problem> {
    if !can_read_context(auth, context_type) {
        return Err(problemdetails::new(axum::http::StatusCode::FORBIDDEN)
            .with_title("Insufficient Permissions")
            .with_detail(format!(
                "The {} permission is required for this AI conversation context.",
                required_context_permission(context_type)
                    .map(|permission| permission.to_string())
                    .unwrap_or_else(|| "corresponding read".to_string())
            )));
    }
    Ok(())
}

pub(super) fn can_read_context(auth: &AuthContext, context_type: &str) -> bool {
    required_context_permission(context_type)
        .is_none_or(|permission| auth.has_permission(&permission))
}

fn required_context_permission(context_type: &str) -> Option<Permission> {
    match context_type {
        "alert" | "alert_suggest" => Some(Permission::OtelRead),
        "deployment" => Some(Permission::DeploymentsRead),
        _ => None,
    }
}

pub(super) fn ensure_conversation_read_permission(
    auth: &AuthContext,
    conversation: &ai_conversations::Model,
) -> Result<(), Problem> {
    // Persisted history is project/context data. Host-provider administration
    // is an execution boundary and must not make stored chat content disappear
    // when that separate permission is later revoked.
    ensure_context_read_permission(auth, &conversation.context_type)
}

pub(super) async fn hidden_conversation_project_ids(
    auth: &AuthContext,
    checker: &Option<Arc<dyn temps_core::ProjectAccessChecker>>,
) -> Result<Vec<i32>, Problem> {
    if auth.is_admin() || auth.has_role(&temps_auth::permissions::Role::PlatformAdmin) {
        return Ok(Vec::new());
    }
    let Some(checker) = checker else {
        return Ok(Vec::new());
    };
    let Some(user_id) = auth.user_id_opt() else {
        return Err(problemdetails::new(StatusCode::FORBIDDEN)
            .with_title("Project Access Denied")
            .with_detail("A user identity is required to list AI conversations."));
    };
    checker
        .hidden_project_ids(user_id)
        .await
        .map(|ids| ids.unwrap_or_default())
        .map_err(|error| {
            tracing::error!(user_id, error = %error, "failed to filter global AI conversations");
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Project Access Check Failed")
                .with_detail("Could not verify project access; please try again")
        })
}

pub(super) async fn ensure_application_project_access(
    auth: &AuthContext,
    checker: &Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    project_ids: &[i32],
) -> Result<(), Problem> {
    if has_application_project_access(auth, checker, project_ids).await? {
        return Ok(());
    }
    Err(problemdetails::new(StatusCode::FORBIDDEN)
        .with_title("Project Access Denied")
        .with_detail("Your team membership does not include every project in this application."))
}

/// Resolve whether a principal may see a full application topology. Application
/// threads deliberately require access to *every* linked project: an anchor
/// project must never become a route around a revoked project membership.
pub(super) async fn has_application_project_access(
    auth: &AuthContext,
    checker: &Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    project_ids: &[i32],
) -> Result<bool, Problem> {
    let access = application_project_access_map(auth, checker, project_ids).await?;
    Ok(access.is_none_or(|access| {
        project_ids
            .iter()
            .all(|project_id| access.get(project_id).copied().unwrap_or(false))
    }))
}

/// Return a ProjectsRead visibility map when a team checker is configured.
/// Project roles may narrow instance permissions. A checker's explicit
/// permission set is authoritative; `None` preserves compatibility with
/// checkers that only implement coarse membership. `None` for the entire map
/// represents the OSS/admin path where every requested project is visible.
pub(crate) async fn application_project_access_map(
    auth: &AuthContext,
    checker: &Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    project_ids: &[i32],
) -> Result<Option<std::collections::BTreeMap<i32, bool>>, Problem> {
    if !auth.has_permission(&Permission::ProjectsRead) {
        return Ok(Some(
            project_ids
                .iter()
                .copied()
                .map(|project_id| (project_id, false))
                .collect(),
        ));
    }
    if auth.is_admin() || auth.has_role(&temps_auth::permissions::Role::PlatformAdmin) {
        return Ok(None);
    }
    let Some(checker) = checker else {
        return Ok(None);
    };
    let user_id = auth.user_id_opt().ok_or_else(|| {
        problemdetails::new(StatusCode::FORBIDDEN)
            .with_title("Project Access Denied")
            .with_detail("A user identity is required to access an AI application.")
    })?;
    let permissions = checker
        .effective_project_permissions_batch(user_id, project_ids)
        .await
        .map_err(|error| {
            error!(user_id, error = %error, "failed to check AI application project permissions");
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Project Permission Check Failed")
                .with_detail("Could not verify project permissions; please try again")
        })?;
    let membership = checker
        .user_can_access_projects(user_id, project_ids)
        .await
        .map_err(|error| {
            error!(user_id, error = %error, "failed to check AI application project access");
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Project Access Check Failed")
                .with_detail("Could not verify project access; please try again")
        })?;
    let required = Permission::ProjectsRead.to_string();
    let access = project_ids
        .iter()
        .copied()
        .map(|project_id| {
            let visible = match permissions.get(&project_id) {
                Some(Some(granted)) => granted.iter().any(|permission| permission == &required),
                Some(None) => membership.get(&project_id).copied().unwrap_or(false),
                None => false,
            };
            (project_id, visible)
        })
        .collect();
    Ok(Some(access))
}

/// Enforce one permission across every project linked to an application.
/// Instance permissions are the ceiling; a project role may narrow them but
/// can never grant a permission that the instance role does not hold.
pub(crate) async fn ensure_application_project_permission(
    auth: &AuthContext,
    checker: &Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    project_ids: &[i32],
    required: &Permission,
) -> Result<(), Problem> {
    check_project_permission(auth, checker.as_ref(), project_ids, required)
        .await
        .map_err(Problem::from)
}

/// Existing projects may only be bundled when the installation can prove the
/// caller's project membership. OSS has no project ownership column to fall
/// back to, so a non-admin request must fail closed when no checker is
/// registered. Starter projects created by this request are safe because they
/// are not supplied by the caller and therefore bypass this preflight.
pub(super) async fn ensure_application_creation_project_permission(
    auth: &AuthContext,
    checker: &Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    project_ids: &[i32],
    required: &Permission,
) -> Result<(), Problem> {
    if project_ids.is_empty()
        || auth.is_admin()
        || auth.has_role(&temps_auth::permissions::Role::PlatformAdmin)
    {
        return ensure_application_project_permission(auth, checker, project_ids, required).await;
    }
    if checker.is_none() {
        return Err(problemdetails::new(StatusCode::FORBIDDEN)
            .with_title("Project Ownership Cannot Be Verified")
            .with_detail(
                "This installation cannot verify access to existing projects. Ask an administrator to create the workspace, or create it with a new starter project.",
            ));
    }
    ensure_application_project_permission(auth, checker, project_ids, required).await
}

pub(super) async fn ensure_application_conversation_permission(
    state: &AppState,
    auth: &AuthContext,
    conversation: &ai_conversations::Model,
    required: &Permission,
) -> Result<(), Problem> {
    if conversation.context_type != "application" {
        return Ok(());
    }
    let application_id = conversation
        .application_id
        .ok_or_else(|| ApplicationError::ConversationNotFound(conversation.public_id.clone()))?;
    let mut scopes = state
        .applications
        .project_scopes(auth.user_id(), &[application_id])
        .await?;
    let scope = scopes
        .remove(&application_id)
        .ok_or_else(|| ApplicationError::ConversationNotFound(conversation.public_id.clone()))?;
    let permission = ensure_application_project_permission(
        auth,
        &state.project_access_checker,
        &scope.project_ids,
        required,
    )
    .await;
    if permission.is_err() {
        quarantine_application_workspace(state, auth.user_id(), &scope.public_id).await;
    }
    permission
}

pub(super) async fn ensure_application_conversation_access(
    state: &AppState,
    auth: &AuthContext,
    conversation: &ai_conversations::Model,
) -> Result<(), Problem> {
    if conversation.context_type != "application" {
        return Ok(());
    }
    let application_id = conversation
        .application_id
        .ok_or_else(|| ApplicationError::ConversationNotFound(conversation.public_id.clone()))?;
    let mut scopes = state
        .applications
        .project_scopes(auth.user_id(), &[application_id])
        .await?;
    let scope = scopes
        .remove(&application_id)
        .ok_or_else(|| ApplicationError::ConversationNotFound(conversation.public_id.clone()))?;
    let access = ensure_application_project_permission(
        auth,
        &state.project_access_checker,
        &scope.project_ids,
        &Permission::ProjectsRead,
    )
    .await;
    if access.is_err() {
        quarantine_application_workspace(state, auth.user_id(), &scope.public_id).await;
    }
    access
}

pub(super) async fn application_list_access(
    state: &AppState,
    auth: &AuthContext,
    application_ids: &[i64],
) -> Result<
    (
        HashMap<i64, crate::applications::ApplicationProjectScope>,
        Option<std::collections::BTreeMap<i32, bool>>,
    ),
    Problem,
> {
    let scopes = state
        .applications
        .project_scopes(auth.user_id(), application_ids)
        .await?;
    let project_ids = scopes
        .values()
        .flat_map(|scope| scope.project_ids.iter().copied())
        .collect::<Vec<_>>();
    let access =
        application_project_access_map(auth, &state.project_access_checker, &project_ids).await?;
    Ok((scopes, access))
}

/// Resolve every application visible in the requested candidate set before a
/// list is paginated. This prevents inaccessible rows from consuming a page.
/// Active applications are quota-bounded; archived applications require the
/// full candidate set because page-number offsets are defined over visible
/// rows.
pub(super) async fn visible_application_ids_for_listing(
    state: &AppState,
    auth: &AuthContext,
    status: Option<&str>,
) -> Result<Vec<i64>, Problem> {
    let scopes = state
        .applications
        .project_scopes_for_listing(auth.user_id(), status)
        .await?;
    let project_ids = scopes
        .values()
        .flat_map(|scope| scope.project_ids.iter().copied())
        .collect::<Vec<_>>();
    let access =
        application_project_access_map(auth, &state.project_access_checker, &project_ids).await?;
    let mut visible = Vec::with_capacity(scopes.len());
    for (application_id, scope) in scopes {
        let application_visible = access.as_ref().is_none_or(|access| {
            scope
                .project_ids
                .iter()
                .all(|project_id| access.get(project_id).copied().unwrap_or(false))
        });
        if application_visible {
            visible.push(application_id);
        } else {
            quarantine_application_workspace(state, auth.user_id(), &scope.public_id).await;
        }
    }
    visible.sort_unstable();
    Ok(visible)
}

pub(super) fn hidden_conversation_context_types(auth: &AuthContext) -> Vec<&'static str> {
    let mut hidden = Vec::with_capacity(3);
    if !auth.has_permission(&Permission::OtelRead) {
        hidden.extend(["alert", "alert_suggest"]);
    }
    if !auth.has_permission(&Permission::DeploymentsRead) {
        hidden.push("deployment");
    }
    hidden
}

/// Resolve visibility from a topology/access snapshot loaded once per list.
pub(super) async fn application_conversation_is_visible(
    state: &AppState,
    auth: &AuthContext,
    conversation: &ai_conversations::Model,
    scopes: &HashMap<i64, crate::applications::ApplicationProjectScope>,
    access: &Option<std::collections::BTreeMap<i32, bool>>,
) -> Result<bool, Problem> {
    if conversation.context_type != "application" {
        return Ok(true);
    }
    let Some(application_id) = conversation.application_id else {
        return Ok(false);
    };
    let Some(scope) = scopes.get(&application_id) else {
        return Ok(false);
    };
    let visible = access.as_ref().is_none_or(|access| {
        scope
            .project_ids
            .iter()
            .all(|project_id| access.get(project_id).copied().unwrap_or(false))
    });
    if !visible {
        quarantine_application_workspace(state, auth.user_id(), &scope.public_id).await;
    }
    Ok(visible)
}

pub(super) async fn authorized_application(
    state: &AppState,
    auth: &AuthContext,
    application_public_id: &str,
) -> Result<crate::applications::ApplicationWithProjects, Problem> {
    authorized_application_with_status(state, auth, application_public_id, "active").await
}

pub(super) async fn authorized_application_with_status(
    state: &AppState,
    auth: &AuthContext,
    application_public_id: &str,
    status: &str,
) -> Result<crate::applications::ApplicationWithProjects, Problem> {
    authorized_application_for_permissions(
        state,
        auth,
        application_public_id,
        Some(status),
        &[Permission::ProjectsRead],
    )
    .await
}

pub(crate) async fn authorized_application_for_permissions(
    state: &AppState,
    auth: &AuthContext,
    application_public_id: &str,
    status: Option<&str>,
    required_permissions: &[Permission],
) -> Result<crate::applications::ApplicationWithProjects, Problem> {
    let application = match status {
        Some(status) => {
            state
                .applications
                .get_with_status(auth.user_id(), application_public_id, status)
                .await?
        }
        None => {
            state
                .applications
                .get_any_status(auth.user_id(), application_public_id)
                .await?
        }
    };
    let project_ids = application
        .projects
        .iter()
        .map(|project| project.id)
        .collect::<Vec<_>>();
    for required in required_permissions {
        if let Err(problem) = ensure_application_project_permission(
            auth,
            &state.project_access_checker,
            &project_ids,
            required,
        )
        .await
        {
            quarantine_application_workspace(state, auth.user_id(), application_public_id).await;
            return Err(problem);
        }
    }
    Ok(application)
}

pub(crate) async fn ensure_user_conversation_access(
    state: &AppState,
    auth: &AuthContext,
    conversation: &ai_conversations::Model,
) -> Result<(), Problem> {
    // The owner lookup already proved the chat belongs to this user. Context
    // is relevance, not authority: do not make the chat unreadable because a
    // linked project's membership changed. The application's own ownership is
    // still checked, while each platform tool/endpoint independently enforces
    // its current resource permission and scope.
    if conversation.context_type == "application" {
        let application_id = conversation.application_id.ok_or_else(|| {
            ApplicationError::ConversationNotFound(conversation.public_id.clone())
        })?;
        let scopes = state
            .applications
            .project_scopes(auth.user_id(), &[application_id])
            .await?;
        if !scopes.contains_key(&application_id) {
            return Err(
                ApplicationError::ConversationNotFound(conversation.public_id.clone()).into(),
            );
        }
    }
    ensure_conversation_read_permission(auth, conversation)
}

/// A user-owned chat remains readable after project membership changes, but a
/// mutation that controls an active provider must still honor current project
/// access for legacy project-scoped conversations.
pub(super) async fn ensure_user_conversation_mutation_access(
    state: &AppState,
    auth: &AuthContext,
    conversation: &ai_conversations::Model,
) -> Result<(), Problem> {
    ensure_conversation_read_permission(auth, conversation)?;
    if conversation.context_type == "application" {
        ensure_application_conversation_access(state, auth, conversation).await?;
    } else {
        ensure_legacy_conversation_mutation_access(
            auth,
            &state.project_access_checker,
            conversation.project_id,
        )
        .await?;
    }
    Ok(())
}

/// Renaming, archiving, restoring or stopping a user-owned conversation
/// changes its state, so read access to it is not enough. Each context takes
/// the permission its project-scoped routes have always asked for,
/// `projects:write`. Assistant chats are the exception: any role holding
/// `projects:read` may start one and send it messages (it has no sandbox, and
/// every tool call it makes is re-checked against the caller's own
/// permissions), so the same role may manage it.
pub(super) fn ensure_user_conversation_write_permission(
    auth: &AuthContext,
    conversation: &ai_conversations::Model,
) -> Result<(), Problem> {
    if conversation.context_type == crate::providers::assistant::ASSISTANT_CONTEXT_TYPE {
        return Ok(());
    }
    permission_guard!(auth, ProjectsWrite);
    Ok(())
}

pub(super) async fn ensure_legacy_conversation_mutation_access(
    auth: &AuthContext,
    project_access_checker: &Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    project_id: Option<i32>,
) -> Result<(), Problem> {
    if let Some(project_id) = project_id {
        ensure_application_project_permission(
            auth,
            project_access_checker,
            &[project_id],
            &Permission::ProjectsWrite,
        )
        .await?;
    }
    Ok(())
}

pub(super) fn conversation_context_requires_sandbox_write(context_type: &str) -> bool {
    matches!(context_type, "application" | "global")
}
