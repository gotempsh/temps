// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which AI prerequisites a project satisfies.

use super::*;

/// What still has to be true before an AI chat can run a turn in this project.
#[derive(Debug, Serialize, ToSchema)]
pub struct ChatReadinessResponse {
    /// An AI provider is configured on this instance. Fixed in
    /// Settings → AI Providers; instance-wide, not per project.
    pub ai_configured: bool,
}

/// Report which AI prerequisites this project satisfies.
///
/// Read-only and cheap, so the UI can decide up front whether to show a working
/// entry point, an onboarding path, or nothing — instead of letting the user
/// click something that fails with a 409 they can't act on.
#[utoipa::path(
    get, tag = "AI Chat",
    path = "/projects/{project_id}/ai/readiness",
    params(("project_id" = i32, Path,)),
    responses(
        (status = 200, description = "Which AI prerequisites are met", body = ChatReadinessResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Project not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_chat_readiness(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
) -> Result<Json<ChatReadinessResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let readiness = state.service.chat_readiness(project_id).await?;

    Ok(Json(ChatReadinessResponse {
        ai_configured: readiness.ai_configured,
    }))
}
