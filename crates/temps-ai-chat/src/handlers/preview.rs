// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Preview links for application and global workspaces, and the preview
//! authorization check.

use super::*;

/// Mint a short-lived URL for a port running in the application harness
/// sandbox. This intentionally does not return the bare `ws-…` hostname:
/// application sandboxes always have a private preview password and only the
/// gateway can exchange this grant for the preview cookie.
#[utoipa::path(
    post, tag = "AI Applications", path = "/ai/applications/{application_public_id}/preview-link",
    params(("application_public_id" = String, Path,)), request_body = CreateApplicationPreviewLinkRequest,
    responses((status = 200, body = ApplicationPreviewLinkResponse), (status = 400), (status = 401), (status = 403), (status = 404), (status = 503)),
    security(("bearer_auth" = []))
)]
pub async fn create_application_preview_link(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(application_public_id): Path<String>,
    Json(request): Json<CreateApplicationPreviewLinkRequest>,
) -> Result<Json<ApplicationPreviewLinkResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    permission_guard!(auth, SandboxesWrite);
    deny_deployment_token!(auth);
    let (_, sandbox_public_id) =
        application_workspace_sandbox(&state, &auth, &application_public_id).await?;
    let sandboxes = state.application_sandboxes.as_ref().ok_or_else(|| {
        problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
            .with_title("Application Sandbox Unavailable")
            .with_detail("The instance sandbox service is not configured.")
    })?;
    let (url, expires_at) = sandboxes
        .preview_share_link(
            &sandbox_public_id,
            auth.user_id(),
            request.port,
            request.path.as_deref().unwrap_or("/"),
            std::time::Duration::from_secs(60 * 60),
        )
        .await
        .map_err(Problem::from)?;
    state
        .audit(&ApplicationPreviewLinkCreatedAudit {
            context: audit_context(&auth, &metadata),
            application_id: application_public_id,
            sandbox_id: sandbox_public_id,
            port: request.port,
        })
        .await;
    Ok(Json(ApplicationPreviewLinkResponse { url, expires_at }))
}

/// Mint a short-lived preview URL for a port in the user's shared global AI
/// workspace. The gateway grant is identical to application previews; the
/// only difference is how the owning sandbox is resolved.
#[utoipa::path(
    post, tag = "AI Chat", path = "/ai/workspace/preview-link",
    operation_id = "create_global_workspace_preview_link",
    request_body = CreateApplicationPreviewLinkRequest,
    responses((status = 200, body = ApplicationPreviewLinkResponse), (status = 400), (status = 401), (status = 403), (status = 503)),
    security(("bearer_auth" = []))
)]
pub async fn create_global_workspace_preview_link(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateApplicationPreviewLinkRequest>,
) -> Result<Json<ApplicationPreviewLinkResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    permission_guard!(auth, SandboxesWrite);
    deny_deployment_token!(auth);
    let (workspace_id, sandbox_public_id) = global_workspace_sandbox(&state, &auth).await?;
    let sandboxes = state.application_sandboxes.as_ref().ok_or_else(|| {
        problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
            .with_title("Global Workspace Unavailable")
            .with_detail("The instance sandbox service is not configured.")
    })?;
    let (url, expires_at) = sandboxes
        .preview_share_link(
            &sandbox_public_id,
            auth.user_id(),
            request.port,
            request.path.as_deref().unwrap_or("/"),
            std::time::Duration::from_secs(60 * 60),
        )
        .await
        .map_err(Problem::from)?;
    state
        .audit(&GlobalWorkspacePreviewLinkCreatedAudit {
            context: audit_context(&auth, &metadata),
            workspace_id,
            sandbox_id: sandbox_public_id,
            port: request.port,
        })
        .await;
    Ok(Json(ApplicationPreviewLinkResponse { url, expires_at }))
}

pub(super) fn require_console_preview_session(auth: &AuthContext) -> Result<(), Problem> {
    if matches!(
        &auth.source,
        AuthSource::Session {
            session_id: Some(_),
            ..
        }
    ) {
        Ok(())
    } else {
        Err(problemdetails::new(StatusCode::FORBIDDEN)
            .with_title("Console Session Required")
            .with_detail("Preview renewal requires an active console session."))
    }
}

/// Renew a managed preview only for an already-bound, running workspace. The
/// requested sandbox ID is an object selector, never an authorization grant.
#[utoipa::path(
    post, tag = "AI Chat", path = "/ai/preview/authorize",
    operation_id = "authorize_workspace_preview",
    request_body = AuthorizeWorkspacePreviewRequest,
    responses((status = 200, body = ApplicationPreviewLinkResponse), (status = 400), (status = 401), (status = 403), (status = 404), (status = 503)),
    security(("bearer_auth" = []))
)]
pub async fn authorize_workspace_preview(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<AuthorizeWorkspacePreviewRequest>,
) -> Result<Json<ApplicationPreviewLinkResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    permission_guard!(auth, SandboxesWrite);
    deny_deployment_token!(auth);
    require_console_preview_session(&auth)?;
    if request.port == 0 {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Preview Port")
            .with_detail("The preview port must be between 1 and 65535."));
    }
    let sandboxes = state.application_sandboxes.as_ref().ok_or_else(|| {
        problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
            .with_title("Workspace Preview Unavailable")
            .with_detail("The instance sandbox service is not configured.")
    })?;
    let sandbox = sandboxes
        .find_by_public_id(&request.sandbox_public_id, auth.user_id())
        .await
        .map_err(Problem::from)?;
    let global_workspace_id = global_workspace_context_id(auth.user_id());
    if sandbox.name == format!("ai-application:{global_workspace_id}") {
        // Global workspaces are bound by their deterministic user-scoped name.
    } else {
        let application_public_id = state
            .applications
            .preview_application_for_sandbox(auth.user_id(), &request.sandbox_public_id)
            .await?
            .ok_or_else(|| {
                problemdetails::new(StatusCode::NOT_FOUND).with_title("Workspace Preview Not Found")
            })?;
        if sandbox.name != format!("ai-application:{application_public_id}") {
            return Err(problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Workspace Preview Not Found"));
        }
        let application = state
            .applications
            .get(auth.user_id(), &application_public_id)
            .await?;
        let project_ids = application
            .projects
            .iter()
            .map(|project| project.id)
            .collect::<Vec<_>>();
        ensure_application_project_permission(
            &auth,
            &state.project_access_checker,
            &project_ids,
            &Permission::SandboxesWrite,
        )
        .await?;
        ensure_application_project_permission(
            &auth,
            &state.project_access_checker,
            &project_ids,
            &Permission::ProjectsWrite,
        )
        .await?;
        let workspace = state
            .applications
            .workspace(application.application.id)
            .await?;
        if workspace.desired_state == "quarantined" {
            return Err(problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Application Workspace Quarantined")
                .with_detail(
                    "Workspace preview access is blocked until linked project access is restored.",
                ));
        }
    }
    sandboxes.preview_ready_sandbox(&request.sandbox_public_id, auth.user_id()).await.map_err(|error| {
        error!(%error, sandbox_id = %request.sandbox_public_id, "managed preview runtime unavailable");
        problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
            .with_title("Workspace Preview Unavailable")
            .with_detail("The workspace sandbox is not running. Resume the workspace and try again.")
    })?;
    let (url, expires_at) = sandboxes
        .preview_share_link(
            &request.sandbox_public_id,
            auth.user_id(),
            request.port,
            request.path.as_deref().unwrap_or("/"),
            std::time::Duration::from_secs(60),
        )
        .await
        .map_err(Problem::from)?;
    if sandbox.name == format!("ai-application:{global_workspace_id}") {
        state
            .audit(&GlobalWorkspacePreviewLinkCreatedAudit {
                context: audit_context(&auth, &metadata),
                workspace_id: global_workspace_id,
                sandbox_id: request.sandbox_public_id,
                port: request.port,
            })
            .await;
    } else {
        state
            .audit(&ApplicationPreviewLinkCreatedAudit {
                context: audit_context(&auth, &metadata),
                application_id: sandbox
                    .name
                    .trim_start_matches("ai-application:")
                    .to_string(),
                sandbox_id: request.sandbox_public_id,
                port: request.port,
            })
            .await;
    }
    Ok(Json(ApplicationPreviewLinkResponse { url, expires_at }))
}
