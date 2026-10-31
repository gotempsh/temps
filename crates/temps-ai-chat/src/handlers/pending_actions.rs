// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pending write actions the assistant proposed: list, get, confirm and reject.

use super::*;

/// Recursively scrub object keys that may carry sensitive values.
///
/// Any key whose name (case-insensitive) is or contains one of:
/// `value`, `secret`, `password`, `token`, `key`
/// has its value replaced with `"***"`. Objects nested below neutral wrapper
/// keys such as `parameters` and objects inside arrays are traversed too.
/// Structural fields (`operation`, `method`, `summary`, etc.) are left intact.
pub(super) fn redact_params(v: &serde_json::Value) -> serde_json::Value {
    redact_value(v)
}

/// A proposed AI write action awaiting human confirmation.
#[derive(Debug, Serialize, ToSchema)]
pub struct PendingActionResponse {
    pub public_id: String,
    pub operation_id: String,
    pub method: String,
    pub summary: String,
    pub status: String,
    /// Set when this action is one step of a multi-step plan (chained actions);
    /// all steps of the plan share this id. Absent for standalone single actions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_public_id: Option<String>,
    /// 0-based order of this step within its plan (0 for standalone actions).
    pub step_index: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_permission: Option<String>,
    /// The flat params to be replayed at execute time (shown pre-execution for review).
    pub params: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executed_at: Option<String>,
}

impl From<ai_pending_actions::Model> for PendingActionResponse {
    fn from(m: ai_pending_actions::Model) -> Self {
        Self {
            public_id: m.public_id,
            operation_id: m.operation_id,
            method: m.method,
            summary: m.summary,
            status: m.status,
            plan_public_id: m.plan_public_id,
            step_index: m.step_index,
            required_permission: m.required_permission,
            // Scrub sensitive values (e.g. env-var values) before returning to
            // clients who may only hold a broad read permission.
            params: display_value(&m.params),
            result: m.result.as_ref().map(redact_params),
            error: m.error.map(|error| redact_text(&error)),
            created_at: m.created_at.to_rfc3339(),
            confirmed_at: m.confirmed_at.map(|t| t.to_rfc3339()),
            executed_at: m.executed_at.map(|t| t.to_rfc3339()),
        }
    }
}

/// List all pending actions for a conversation (most-recently-proposed first).
#[utoipa::path(
    get, tag = "AI Chat",
    path = "/projects/{project_id}/ai/conversations/{public_id}/pending-actions",
    params(
        ("project_id" = i32, Path,),
        ("public_id" = String, Path, description = "Conversation public id"),
    ),
    responses(
        (status = 200, body = Vec<PendingActionResponse>),
        (status = 401), (status = 403), (status = 404)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_pending_actions(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, conv_public_id)): Path<(i32, String)>,
) -> Result<Json<Vec<PendingActionResponse>>, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    // Verify conversation exists + is scoped to this project.
    let conv = state
        .service
        .get_by_public_id(project_id, auth.user_id(), &conv_public_id)
        .await?;
    ensure_application_conversation_access(&state, &auth, &conv).await?;
    ensure_context_read_permission(&auth, &conv.context_type)?;
    let rows = state
        .pending_actions
        .list_for_conversation(Some(project_id), auth.user_id(), conv.id)
        .await
        .map_err(Problem::from)?;
    Ok(Json(
        rows.into_iter().map(PendingActionResponse::from).collect(),
    ))
}

/// Get a single pending action by its public id (scoped to the project).
#[utoipa::path(
    get, tag = "AI Chat",
    path = "/projects/{project_id}/ai/pending-actions/{action_public_id}",
    params(
        ("project_id" = i32, Path,),
        ("action_public_id" = String, Path,),
    ),
    responses(
        (status = 200, body = PendingActionResponse),
        (status = 401), (status = 403), (status = 404)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_pending_action(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, action_public_id)): Path<(i32, String)>,
) -> Result<Json<PendingActionResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let action = state
        .pending_actions
        .get(Some(project_id), auth.user_id(), &action_public_id)
        .await
        .map_err(Problem::from)?;
    let conv = state
        .service
        .get_by_id(project_id, auth.user_id(), action.conversation_id)
        .await?;
    ensure_application_conversation_access(&state, &auth, &conv).await?;
    ensure_context_read_permission(&auth, &conv.context_type)?;
    Ok(Json(PendingActionResponse::from(action)))
}

/// Confirm a proposed AI action: validate permission, atomically claim, execute,
/// persist outcome. The execution uses the CONFIRMING user's auth — never the model's.
#[utoipa::path(
    post, tag = "AI Chat",
    path = "/projects/{project_id}/ai/pending-actions/{action_public_id}/confirm",
    params(
        ("project_id" = i32, Path,),
        ("action_public_id" = String, Path,),
    ),
    responses(
        (status = 200, body = PendingActionResponse),
        (status = 401), (status = 403), (status = 404), (status = 409), (status = 503)
    ),
    security(("bearer_auth" = []))
)]
pub async fn confirm_pending_action(
    RequireAuth(auth): RequireAuth,
    ai_call: Option<Extension<temps_core::ai_tool_call::AiToolCall>>,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, action_public_id)): Path<(i32, String)>,
) -> Result<Json<PendingActionResponse>, Problem> {
    temps_core::ai_tool_call::require_human_caller(
        ai_call.as_ref().map(|e| &e.0),
        "confirm_pending_action",
    )?;
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let action = state
        .pending_actions
        .get(Some(project_id), auth.user_id(), &action_public_id)
        .await
        .map_err(Problem::from)?;
    let conv = state
        .service
        .get_by_id(project_id, auth.user_id(), action.conversation_id)
        .await?;
    ensure_application_conversation_access(&state, &auth, &conv).await?;
    ensure_context_read_permission(&auth, &conv.context_type)?;
    let confirmed_by = Some(auth.user_id());
    let updated = state
        .pending_actions
        .confirm(
            Some(project_id),
            &action_public_id,
            &auth,
            confirmed_by,
            Some(request_origin(&metadata)),
        )
        .await
        .map_err(Problem::from)?;

    // Audit is also emitted inside the service, but we emit here with full
    // metadata (ip_address, user_agent) for the HTTP-layer record.
    let audit = AiActionConfirmedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id: Some(project_id),
        action_id: updated.public_id.clone(),
        operation_id: updated.operation_id.clone(),
        status: updated.status.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to write ai.pending_action.confirmed audit log: {e}");
    }

    Ok(Json(PendingActionResponse::from(updated)))
}

/// Reject a proposed AI action (no execution). Status transitions to "rejected".
#[utoipa::path(
    post, tag = "AI Chat",
    path = "/projects/{project_id}/ai/pending-actions/{action_public_id}/reject",
    params(
        ("project_id" = i32, Path,),
        ("action_public_id" = String, Path,),
    ),
    responses(
        (status = 200, body = PendingActionResponse),
        (status = 401), (status = 403), (status = 404), (status = 409)
    ),
    security(("bearer_auth" = []))
)]
pub async fn reject_pending_action(
    RequireAuth(auth): RequireAuth,
    ai_call: Option<Extension<temps_core::ai_tool_call::AiToolCall>>,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, action_public_id)): Path<(i32, String)>,
) -> Result<Json<PendingActionResponse>, Problem> {
    temps_core::ai_tool_call::require_human_caller(
        ai_call.as_ref().map(|e| &e.0),
        "reject_pending_action",
    )?;
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let action = state
        .pending_actions
        .get(Some(project_id), auth.user_id(), &action_public_id)
        .await
        .map_err(Problem::from)?;
    let conv = state
        .service
        .get_by_id(project_id, auth.user_id(), action.conversation_id)
        .await?;
    ensure_application_conversation_access(&state, &auth, &conv).await?;
    ensure_context_read_permission(&auth, &conv.context_type)?;
    let rejected_by = Some(auth.user_id());
    let updated = state
        .pending_actions
        .reject(Some(project_id), &action_public_id, &auth, rejected_by)
        .await
        .map_err(Problem::from)?;

    let audit = AiActionRejectedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id: Some(project_id),
        action_id: updated.public_id.clone(),
        operation_id: updated.operation_id.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to write ai.pending_action.rejected audit log: {e}");
    }

    Ok(Json(PendingActionResponse::from(updated)))
}

#[utoipa::path(
    get, tag = "AI Chat", path = "/ai/conversations/{public_id}/pending-actions",
    params(("public_id" = String, Path,)), responses((status = 200, body = Vec<PendingActionResponse>), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn list_user_pending_actions(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(public_id): Path<String>,
) -> Result<Json<Vec<PendingActionResponse>>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    let actions = state
        .pending_actions
        .list_owned_for_conversation(auth.user_id(), conversation.id)
        .await
        .map_err(Problem::from)?;
    Ok(Json(
        actions
            .into_iter()
            .map(PendingActionResponse::from)
            .collect(),
    ))
}

#[utoipa::path(
    get, tag = "AI Chat", path = "/ai/pending-actions/{action_public_id}",
    params(("action_public_id" = String, Path,)), responses((status = 200, body = PendingActionResponse), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn get_user_pending_action(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(action_public_id): Path<String>,
) -> Result<Json<PendingActionResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let action = state
        .pending_actions
        .get_owned(auth.user_id(), &action_public_id)
        .await
        .map_err(Problem::from)?;
    let conversation = state
        .service
        .get_owned_by_id(auth.user_id(), action.conversation_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    Ok(Json(PendingActionResponse::from(action)))
}

#[utoipa::path(
    post, tag = "AI Chat", path = "/ai/pending-actions/{action_public_id}/confirm",
    params(("action_public_id" = String, Path,)), responses((status = 200, body = PendingActionResponse), (status = 401), (status = 403), (status = 404), (status = 409)),
    security(("bearer_auth" = []))
)]
pub async fn confirm_user_pending_action(
    RequireAuth(auth): RequireAuth,
    ai_call: Option<Extension<temps_core::ai_tool_call::AiToolCall>>,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(action_public_id): Path<String>,
) -> Result<Json<PendingActionResponse>, Problem> {
    temps_core::ai_tool_call::require_human_caller(
        ai_call.as_ref().map(|e| &e.0),
        "confirm_user_pending_action",
    )?;
    // The pending-action service re-checks the operation's exact permission
    // against this current auth context. A blanket project-write gate would
    // incorrectly block global actions whose authority is non-project-specific.
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let action = state
        .pending_actions
        .get_owned(auth.user_id(), &action_public_id)
        .await
        .map_err(Problem::from)?;
    let conversation = state
        .service
        .get_owned_by_id(auth.user_id(), action.conversation_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    let updated = state
        .pending_actions
        .confirm(
            action.project_id,
            &action_public_id,
            &auth,
            Some(auth.user_id()),
            Some(request_origin(&metadata)),
        )
        .await
        .map_err(Problem::from)?;
    state
        .audit(&AiActionConfirmedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: updated.project_id,
            action_id: updated.public_id.clone(),
            operation_id: updated.operation_id.clone(),
            status: updated.status.clone(),
        })
        .await;
    Ok(Json(PendingActionResponse::from(updated)))
}

#[utoipa::path(
    post, tag = "AI Chat", path = "/ai/pending-actions/{action_public_id}/reject",
    params(("action_public_id" = String, Path,)), responses((status = 200, body = PendingActionResponse), (status = 401), (status = 403), (status = 404), (status = 409)),
    security(("bearer_auth" = []))
)]
pub async fn reject_user_pending_action(
    RequireAuth(auth): RequireAuth,
    ai_call: Option<Extension<temps_core::ai_tool_call::AiToolCall>>,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(action_public_id): Path<String>,
) -> Result<Json<PendingActionResponse>, Problem> {
    temps_core::ai_tool_call::require_human_caller(
        ai_call.as_ref().map(|e| &e.0),
        "reject_user_pending_action",
    )?;
    // Like confirm, the pending-action service checks the operation's own
    // permission before rejecting: deciding a proposal either way takes the
    // authority to approve it, which a read-only key never holds.
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let action = state
        .pending_actions
        .get_owned(auth.user_id(), &action_public_id)
        .await
        .map_err(Problem::from)?;
    let conversation = state
        .service
        .get_owned_by_id(auth.user_id(), action.conversation_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    let updated = state
        .pending_actions
        .reject(
            action.project_id,
            &action_public_id,
            &auth,
            Some(auth.user_id()),
        )
        .await
        .map_err(Problem::from)?;
    state
        .audit(&AiActionRejectedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: updated.project_id,
            action_id: updated.public_id.clone(),
            operation_id: updated.operation_id.clone(),
        })
        .await;
    Ok(Json(PendingActionResponse::from(updated)))
}
