// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Attaching a terminal to an application or global workspace.

use super::*;

/// Query parameters of the workspace terminal.
///
/// Deliberately narrower than the sandbox terminal's own: no `cmd`, and no
/// `tab`. A terminal is a thread of kind `terminal`; the thread owns its PTY
/// tab, derived from the thread id on the server, so reconnecting the same
/// thread always lands in the same shell and a client cannot open arbitrary
/// tabs. The terminal always opens the user's login shell; running a
/// specific program is what the CLI's `sandbox exec` is for.
#[derive(Debug, Default, Deserialize, ToSchema, IntoParams)]
pub struct WorkspaceTerminalQuery {
    /// Public id of the terminal thread to attach to.
    pub thread: String,
    /// Initial terminal width in columns (default 80).
    pub cols: Option<u16>,
    /// Initial terminal height in rows (default 24).
    pub rows: Option<u16>,
}

/// The sandbox terminal's parameters for a workspace terminal request.
pub(super) fn workspace_terminal_params(
    query: &WorkspaceTerminalQuery,
    tab: String,
) -> temps_sandbox::handlers::terminal::TerminalParams {
    temps_sandbox::handlers::terminal::TerminalParams {
        tab: Some(tab),
        cmd: None,
        cols: query.cols,
        rows: query.rows,
    }
}

/// Check that `thread` is a live terminal thread of the workspace being
/// opened and return the PTY tab it owns.
///
/// `application_id` is the application's internal id, or `None` for the
/// user's global workspace. A thread of another workspace is reported as
/// missing rather than as forbidden, so thread ids do not leak across them.
pub(super) fn terminal_thread_attach_tab(
    thread: &ai_conversations::Model,
    application_id: Option<i64>,
) -> Result<String, Problem> {
    let in_workspace = match application_id {
        Some(id) => thread.application_id == Some(id),
        None => thread.application_id.is_none() && thread.context_type == "global",
    };
    if !in_workspace {
        return Err(problemdetails::new(StatusCode::NOT_FOUND)
            .with_title("Terminal Thread Not Found")
            .with_detail(format!(
                "Thread {} does not exist in this workspace.",
                thread.public_id
            )));
    }
    if thread.kind != THREAD_KIND_TERMINAL {
        return Err(problemdetails::new(StatusCode::CONFLICT)
            .with_title("Not a Terminal Thread")
            .with_detail(format!(
                "Thread {} is an AI thread; only terminal threads can attach a terminal.",
                thread.public_id
            )));
    }
    if thread.status != "active" {
        return Err(problemdetails::new(StatusCode::CONFLICT)
            .with_title("Conversation Is Archived")
            .with_detail("Restore this terminal thread before attaching to it."));
    }
    Ok(terminal_thread_tab(&thread.public_id))
}

/// What opening a global workspace terminal needs. Creating a terminal thread
/// is held to the same bar, so no role is handed a thread it can never attach
/// to (an attach needs `projects:write` as well as the sandbox permissions).
pub(super) fn require_global_terminal_permissions(auth: &AuthContext) -> Result<(), Problem> {
    permission_guard!(auth, ProjectsWrite);
    permission_guard!(auth, SandboxesWrite);
    permission_guard!(auth, SandboxesExec);
    Ok(())
}

/// Require the permissions a shell inside the workspace needs, at both the
/// instance and the linked-project level.
///
/// A shell can read and rewrite every file of every linked project and run
/// anything, so it needs everything a workspace mutation needs, plus
/// `sandboxes:exec`. `application_workspace_sandbox` re-checks the first two
/// at project scope; this adds the third.
pub(super) async fn ensure_workspace_terminal_permissions(
    auth: &AuthContext,
    checker: &Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    project_ids: &[i32],
) -> Result<(), Problem> {
    for required in [
        Permission::ProjectsWrite,
        Permission::SandboxesWrite,
        Permission::SandboxesExec,
    ] {
        ensure_application_project_permission(auth, checker, project_ids, &required).await?;
    }
    Ok(())
}

/// Where an authorized workspace terminal request attaches.
struct WorkspaceTerminalTarget {
    /// `application` or `global`, recorded in the audit event.
    scope: &'static str,
    workspace_id: String,
    sandbox_public_id: String,
    /// The PTY tab the terminal thread owns.
    tab: String,
}

/// Hand an authorized workspace terminal request to the sandbox terminal.
///
/// The generic `/v1/sandboxes/{id}/terminal` route cannot be used directly:
/// managed workspaces are hidden from it on purpose, so that every access to
/// one re-checks the linked projects. By this point the caller has done that,
/// so the sandbox's own bridge — the same PTY relay, session caps, heartbeat
/// and `attach_pty` provider call the standalone terminal uses — can run.
async fn attach_workspace_terminal(
    state: &AppState,
    auth: AuthContext,
    metadata: RequestMetadata,
    target: WorkspaceTerminalTarget,
    query: WorkspaceTerminalQuery,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, Problem> {
    let WorkspaceTerminalTarget {
        scope,
        workspace_id,
        sandbox_public_id,
        tab,
    } = target;
    let terminal_state = state.application_terminal.clone().ok_or_else(|| {
        problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
            .with_title("Workspace Terminal Unavailable")
            .with_detail("The instance sandbox service is not configured.")
    })?;
    let thread_id = query.thread.clone();
    let params = workspace_terminal_params(&query, tab.clone());
    let user_id = auth.user_id();
    let response = temps_sandbox::handlers::terminal::terminal(
        RequireAuth(auth),
        State(terminal_state),
        Path(sandbox_public_id.clone()),
        Query(params),
        headers,
        ws,
    )
    .await?;
    state
        .audit(&WorkspaceTerminalAttachedAudit {
            context: AuditContext {
                user_id,
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            scope: scope.to_string(),
            workspace_id,
            sandbox_id: sandbox_public_id,
            thread_id,
            tab,
        })
        .await;
    Ok(response)
}

#[utoipa::path(
    get, tag = "AI Applications",
    path = "/ai/applications/{application_public_id}/workspace/terminal",
    operation_id = "attach_application_workspace_terminal",
    summary = "Attach an interactive terminal to an application workspace",
    description = "Upgrades to a WebSocket carrying a PTY in the application's persistent workspace sandbox. Binary frames are raw terminal bytes in both directions; text frames are JSON control (`{\"type\":\"resize\",\"cols\":N,\"rows\":N}`) and events (`ready`, `exit`, `error`). The `thread` query parameter names a terminal thread, which owns its shell: attaching the same thread again resumes the running program. Requires `sandboxes:exec`.",
    params(("application_public_id" = String, Path,), WorkspaceTerminalQuery),
    responses(
        (status = 101, description = "WebSocket established; PTY attached"),
        (status = 401), (status = 403), (status = 404),
        (status = 409, description = "The workspace sandbox cannot be attached to right now"),
        (status = 429, description = "Too many concurrent terminals"),
        (status = 503)
    ),
    security(("bearer_auth" = []))
)]
pub async fn attach_application_workspace_terminal(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(application_public_id): Path<String>,
    Query(query): Query<WorkspaceTerminalQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, Problem> {
    validate_websocket_origin(&headers)?;
    permission_guard!(auth, ProjectsWrite);
    permission_guard!(auth, SandboxesWrite);
    permission_guard!(auth, SandboxesExec);
    deny_deployment_token!(auth);
    let application = authorized_application(&state, &auth, &application_public_id).await?;
    let project_ids = application
        .projects
        .iter()
        .map(|project| project.id)
        .collect::<Vec<_>>();
    ensure_workspace_terminal_permissions(&auth, &state.project_access_checker, &project_ids)
        .await?;
    let thread = state
        .service
        .get_owned_by_public_id(auth.user_id(), &query.thread)
        .await?;
    let tab = terminal_thread_attach_tab(&thread, Some(application.application.id))?;
    let (_, sandbox_public_id) =
        application_workspace_sandbox(&state, &auth, &application_public_id).await?;
    attach_workspace_terminal(
        &state,
        auth,
        metadata,
        WorkspaceTerminalTarget {
            scope: "application",
            workspace_id: application_public_id,
            sandbox_public_id,
            tab,
        },
        query,
        headers,
        ws,
    )
    .await
}

#[utoipa::path(
    get, tag = "AI Chat",
    path = "/ai/workspace/terminal",
    operation_id = "attach_global_workspace_terminal",
    summary = "Attach an interactive terminal to the user's global workspace",
    description = "Same protocol as the application workspace terminal, for a terminal thread of the signed-in user's private default workspace. Requires `sandboxes:exec`.",
    params(WorkspaceTerminalQuery),
    responses(
        (status = 101, description = "WebSocket established; PTY attached"),
        (status = 401), (status = 403),
        (status = 409, description = "The workspace sandbox cannot be attached to right now"),
        (status = 429, description = "Too many concurrent terminals"),
        (status = 503)
    ),
    security(("bearer_auth" = []))
)]
pub async fn attach_global_workspace_terminal(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Query(query): Query<WorkspaceTerminalQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, Problem> {
    validate_websocket_origin(&headers)?;
    require_global_terminal_permissions(&auth)?;
    deny_deployment_token!(auth);
    let thread = state
        .service
        .get_owned_by_public_id(auth.user_id(), &query.thread)
        .await?;
    let tab = terminal_thread_attach_tab(&thread, None)?;
    let (workspace_id, sandbox_public_id) = global_workspace_sandbox(&state, &auth).await?;
    attach_workspace_terminal(
        &state,
        auth,
        metadata,
        WorkspaceTerminalTarget {
            scope: "global",
            workspace_id,
            sandbox_public_id,
            tab,
        },
        query,
        headers,
        ws,
    )
    .await
}
