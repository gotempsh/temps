// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Resolving tool permission requests and changing a conversation's
//! permission mode.

use super::*;

/// Resolve a pending interactive permission request (ADR-038 Phase 2).
///
/// A provider adapter emits a normalized interaction request when user input
/// is required. The common runtime registers it and emits a
/// `permission_requested` WebSocket event. This endpoint sends the decision back to
/// the waiting turn without knowing the provider protocol.
///
/// The claim is atomic (remove-to-claim): a concurrent request for the same
/// `permission_id` receives 409 Conflict.  If the subprocess already exited
/// (and the registry was drained with a synthetic deny), the entry is gone and
/// the caller receives 404.
pub(super) fn permission_decision_matches_kind(
    kind: &PermissionKind,
    decision: &PermissionDecision,
) -> bool {
    matches!(
        (kind, decision),
        (PermissionKind::ToolApproval, PermissionDecision::AllowTool)
            | (
                PermissionKind::ToolApproval,
                PermissionDecision::DenyTool { .. }
            )
            | (
                PermissionKind::Question,
                PermissionDecision::AnswerQuestion { .. }
            )
            | (
                PermissionKind::PlanApproval,
                PermissionDecision::ApprovePlan
            )
            | (
                PermissionKind::PlanApproval,
                PermissionDecision::RejectPlan { .. }
            )
    )
}

fn permission_kind_name(kind: &PermissionKind) -> &'static str {
    match kind {
        PermissionKind::ToolApproval => "tool_approval",
        PermissionKind::Question => "question",
        PermissionKind::PlanApproval => "plan_approval",
    }
}

/// Re-check the resolving user's current authority before consuming a waiter.
/// Platform writes carry the exact operation permission and target project in
/// their server-generated, redacted input. Native harness requests retain the
/// broader project-write gate because they may mutate the application files.
async fn ensure_pending_permission_authorized(
    state: &AppState,
    auth: &AuthContext,
    conversation_public_id: &str,
    permission_id: &str,
) -> Result<uuid::Uuid, Problem> {
    let request = {
        let registry = state.service.pending_permissions.lock().map_err(|_| {
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail("Permission registry lock poisoned")
        })?;
        registry.get(permission_id).and_then(|entry| {
            (entry.conv_public_id == conversation_public_id)
                .then(|| (entry.origin, entry.input.clone(), entry.generation))
        })
    };
    let Some((origin, input, generation)) = request else {
        return Err(problemdetails::new(StatusCode::NOT_FOUND)
            .with_title("Permission Not Found")
            .with_detail(format!(
                "Permission request '{permission_id}' is not pending. It may have timed out, been auto-denied, or already been resolved."
            )));
    };

    if origin == PendingPermissionOrigin::Provider {
        if !auth.has_permission(&Permission::ProjectsWrite) {
            return Err(problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Insufficient Permissions")
                .with_detail(
                    "The projects:write permission is required to approve this harness tool.",
                ));
        }
        return Ok(generation);
    }

    let mut required_permissions = Vec::new();
    if let Some(permission) = input
        .get("required_permission")
        .and_then(serde_json::Value::as_str)
    {
        required_permissions.push(permission.to_string());
    }
    if let Some(steps) = input.get("steps").and_then(serde_json::Value::as_array) {
        required_permissions.extend(steps.iter().filter_map(|step| {
            step.get("required_permission")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        }));
    }
    for permission_name in required_permissions {
        let Some(permission) = Permission::from_str(&permission_name) else {
            return Err(problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Unknown Required Permission")
                .with_detail("This platform action declares an unknown permission and cannot be approved safely."));
        };
        if !auth.has_permission(&permission) {
            return Err(problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Insufficient Permissions")
                .with_detail(format!(
                    "The {permission_name} permission is required to execute this platform action."
                )));
        }
    }

    if let Some(project_id) = input
        .get("project_id")
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
    {
        ensure_application_project_access(auth, &state.project_access_checker, &[project_id])
            .await?;
    }
    Ok(generation)
}

#[utoipa::path(
    post, tag = "AI Chat",
    path = "/projects/{project_id}/ai/conversations/{public_id}/permissions/{permission_id}/resolve",
    params(
        ("project_id" = i32, Path,),
        ("public_id" = String, Path, description = "Conversation public id"),
        ("permission_id" = String, Path, description = "The CLI's request_id from the SSE event"),
    ),
    request_body = ResolvePermissionRequest,
    responses(
        (status = 204, description = "Decision accepted; subprocess will continue"),
        (status = 400, description = "Invalid decision payload"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Unknown permission_id (may have timed out or been auto-denied)"),
        (status = 409, description = "Permission already resolved (concurrent resolve race)"),
        (status = 410, description = "Turn already ended (subprocess exited before decision arrived)"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn resolve_permission(
    RequireAuth(auth): RequireAuth,
    ai_call: Option<Extension<temps_core::ai_tool_call::AiToolCall>>,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, public_id, permission_id)): Path<(i32, String, String)>,
    Json(req): Json<ResolvePermissionRequest>,
) -> Result<axum::http::StatusCode, Problem> {
    temps_core::ai_tool_call::require_human_caller(
        ai_call.as_ref().map(|e| &e.0),
        "resolve_permission",
    )?;
    // Resolving a permission unblocks an AI subprocess (mutates state) → write scope.
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    // Verify the conversation exists and is scoped to this project (auth gate).
    // Runtime authorization is rechecked before the pending entry can be
    // consumed, so revoking provider access also revokes tool approval.
    let conv = state
        .service
        .get_by_public_id(project_id, auth.user_id(), &public_id)
        .await?;
    ensure_application_conversation_access(&state, &auth, &conv).await?;
    ensure_runtime_permission(
        &auth,
        Some(&conv.ai_provider),
        Some(&conv.ai_permission_mode),
    )?;
    ensure_context_read_permission(&auth, &conv.context_type)?;
    let authorized_generation =
        ensure_pending_permission_authorized(&state, &auth, &public_id, &permission_id).await?;

    // ── Finding 3: payload size validation ────────────────────────────────────
    // Reject oversized free-text fields before touching the registry.  The body
    // limit (`DefaultBodyLimit`) on this route's registration provides the outer
    // cap; these per-field checks provide typed 400 errors rather than a 413.
    match &req.decision {
        PermissionDecision::DenyTool {
            reason: Some(reason),
        } if reason.len() > MAX_DECISION_STRING_LEN => {
            return Err(too_long("reason", MAX_DECISION_STRING_LEN));
        }
        PermissionDecision::RejectPlan {
            feedback: Some(feedback),
        } if feedback.len() > MAX_DECISION_STRING_LEN => {
            return Err(too_long("feedback", MAX_DECISION_STRING_LEN));
        }
        PermissionDecision::AnswerQuestion { answers } => {
            let serialized_len = serde_json::to_string(answers)
                .map(|s| s.len())
                .unwrap_or(usize::MAX);
            if serialized_len > MAX_DECISION_STRING_LEN {
                return Err(too_long("answers", MAX_DECISION_STRING_LEN));
            }
        }
        _ => {}
    }

    // Classify the decision kind for the audit log (before we consume the value).
    let decision_kind = match &req.decision {
        PermissionDecision::AllowTool => "allow_tool",
        PermissionDecision::DenyTool { .. } => "deny_tool",
        PermissionDecision::AnswerQuestion { .. } => "answer_question",
        PermissionDecision::ApprovePlan => "approve_plan",
        PermissionDecision::RejectPlan { .. } => "reject_plan",
    }
    .to_string();

    // ── Findings 1 + 2: IDOR guard + kind validation ──────────────────────────
    // Look up the registry entry under the lock.  If present, verify that it
    // belongs to THIS conversation before claiming it (removing it).  A URL
    // `{public_id}` that doesn't match the stored `conv_public_id` gets a 404
    // — the same response as a missing entry — so the existence of the id in
    // another session is not confirmed to the caller.
    //
    // All three operations (lookup, ownership check, remove) happen under the
    // same lock acquisition, so there is no window for a concurrent resolve to
    // claim the entry between our check and our remove.
    let tx = {
        let mut registry = state.service.pending_permissions.lock().map_err(|_| {
            problemdetails::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail("Permission registry lock poisoned")
        })?;

        let stored_entry = registry.get(&permission_id);

        match stored_entry {
            // Entry absent: timed-out, auto-denied, or already resolved.
            None => {
                return Err(problemdetails::new(axum::http::StatusCode::NOT_FOUND)
                    .with_title("Permission Not Found")
                    .with_detail(format!(
                        "Permission request '{permission_id}' is not pending. \
                         It may have timed out, been auto-denied, or already been resolved."
                    )));
            }
            // Entry exists but belongs to a different conversation.
            // Return 404 (not 403) to avoid confirming the id's existence
            // in another session to the caller.
            Some(entry) if entry.conv_public_id != public_id => {
                return Err(problemdetails::new(axum::http::StatusCode::NOT_FOUND)
                    .with_title("Permission Not Found")
                    .with_detail(format!(
                        "Permission request '{permission_id}' is not pending. \
                         It may have timed out, been auto-denied, or already been resolved."
                    )));
            }
            Some(entry) if entry.generation != authorized_generation => {
                return Err(problemdetails::new(axum::http::StatusCode::CONFLICT)
                    .with_title("Permission Request Changed")
                    .with_detail(
                        "The pending permission changed while authorization was being checked. Reload the conversation and review the current request.",
                    ));
            }
            // Entry belongs to this conversation — claim it atomically.
            Some(entry) => {
                if !permission_decision_matches_kind(&entry.kind, &req.decision) {
                    let expected_kind = permission_kind_name(&entry.kind);
                    return Err(ChatError::PermissionKindMismatch {
                        expected_kind: expected_kind.to_string(),
                        received: decision_kind.clone(),
                    }
                    .into());
                }
                let Some(entry) = registry.remove(&permission_id) else {
                    return Err(problemdetails::new(axum::http::StatusCode::CONFLICT)
                        .with_title("Permission Already Resolved")
                        .with_detail("The permission request was resolved concurrently."));
                };
                entry.sender
            }
        }
    };

    // Deliver the structured decision directly to the waiting provider/tool
    // callback. Approval is control-plane state, not conversational user text;
    // the execution result returned by that callback is what the model sees.
    // A `SendError` means the receiver was dropped — i.e.
    // `run_interactive`'s stream task exited (subprocess died) while we were
    // looking up the sender. The turn has ended; the decision can't be used.
    tx.send(PermissionResolution {
        decision: req.decision,
        auth: auth.clone(),
        metadata: metadata.clone(),
    })
    .map_err(|_| {
        problemdetails::new(axum::http::StatusCode::GONE)
            .with_title("Turn Already Ended")
            .with_detail(
                "The AI turn associated with this permission request has already ended. \
                 The subprocess exited before the decision could be delivered.",
            )
    })?;

    // Audit (best-effort: never fail the request on log failure).
    state
        .audit(&PermissionResolvedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: Some(project_id),
            conversation_id: conv.public_id.clone(),
            permission_id: permission_id.clone(),
            tool_name: None,
            decision_kind,
        })
        .await;

    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post, tag = "AI Chat", path = "/ai/conversations/{public_id}/permissions/{permission_id}/resolve",
    params(("public_id" = String, Path,), ("permission_id" = String, Path,)), request_body = ResolvePermissionRequest,
    responses((status = 204), (status = 400), (status = 401), (status = 403), (status = 404), (status = 409), (status = 410)),
    security(("bearer_auth" = []))
)]
pub async fn resolve_user_permission(
    RequireAuth(auth): RequireAuth,
    ai_call: Option<Extension<temps_core::ai_tool_call::AiToolCall>>,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((public_id, permission_id)): Path<(String, String)>,
    Json(req): Json<ResolvePermissionRequest>,
) -> Result<StatusCode, Problem> {
    temps_core::ai_tool_call::require_human_caller(
        ai_call.as_ref().map(|e| &e.0),
        "resolve_user_permission",
    )?;
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_mutation_access(&state, &auth, &conversation).await?;
    if conversation_context_requires_sandbox_write(&conversation.context_type) {
        // Approval can release a sandbox mutation, so it needs the same
        // current capability as starting the turn that requested it.
        permission_guard!(auth, SandboxesWrite);
        ensure_application_conversation_permission(
            &state,
            &auth,
            &conversation,
            &Permission::SandboxesWrite,
        )
        .await?;
        ensure_application_conversation_permission(
            &state,
            &auth,
            &conversation,
            &Permission::ProjectsWrite,
        )
        .await?;
    }
    ensure_runtime_permission(
        &auth,
        Some(&conversation.ai_provider),
        Some(&conversation.ai_permission_mode),
    )?;
    let authorized_generation =
        ensure_pending_permission_authorized(&state, &auth, &public_id, &permission_id).await?;

    match &req.decision {
        PermissionDecision::DenyTool {
            reason: Some(reason),
        } if reason.len() > MAX_DECISION_STRING_LEN => {
            return Err(too_long("reason", MAX_DECISION_STRING_LEN));
        }
        PermissionDecision::RejectPlan {
            feedback: Some(feedback),
        } if feedback.len() > MAX_DECISION_STRING_LEN => {
            return Err(too_long("feedback", MAX_DECISION_STRING_LEN));
        }
        PermissionDecision::AnswerQuestion { answers }
            if serde_json::to_string(answers)
                .map(|value| value.len())
                .unwrap_or(usize::MAX)
                > MAX_DECISION_STRING_LEN =>
        {
            return Err(too_long("answers", MAX_DECISION_STRING_LEN));
        }
        _ => {}
    }
    let decision_kind = match &req.decision {
        PermissionDecision::AllowTool => "allow_tool",
        PermissionDecision::DenyTool { .. } => "deny_tool",
        PermissionDecision::AnswerQuestion { .. } => "answer_question",
        PermissionDecision::ApprovePlan => "approve_plan",
        PermissionDecision::RejectPlan { .. } => "reject_plan",
    }
    .to_string();

    let sender = {
        let mut registry = state.service.pending_permissions.lock().map_err(|_| {
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail("Permission registry lock poisoned")
        })?;
        match registry.get(&permission_id) {
            None => {
                return Err(problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Permission Not Found")
                    .with_detail(format!(
                        "Permission request '{permission_id}' is not pending. It may have timed out, been auto-denied, or already been resolved."
                    )));
            }
            Some(entry) if entry.conv_public_id != public_id => {
                return Err(problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Permission Not Found")
                    .with_detail(format!(
                        "Permission request '{permission_id}' is not pending. It may have timed out, been auto-denied, or already been resolved."
                    )));
            }
            Some(entry) if entry.generation != authorized_generation => {
                return Err(problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Permission Request Changed")
                    .with_detail(
                        "The pending permission changed while authorization was being checked. Reload the conversation and review the current request.",
                    ));
            }
            Some(entry) => {
                if !permission_decision_matches_kind(&entry.kind, &req.decision) {
                    return Err(ChatError::PermissionKindMismatch {
                        expected_kind: permission_kind_name(&entry.kind).to_string(),
                        received: decision_kind.clone(),
                    }
                    .into());
                }
                registry
                    .remove(&permission_id)
                    .ok_or_else(|| {
                        problemdetails::new(StatusCode::CONFLICT)
                            .with_title("Permission Already Resolved")
                            .with_detail("The permission request was resolved concurrently.")
                    })?
                    .sender
            }
        }
    };

    sender
        .send(PermissionResolution {
            decision: req.decision,
            auth: auth.clone(),
            metadata: metadata.clone(),
        })
        .map_err(|_| {
        problemdetails::new(StatusCode::GONE)
            .with_title("Turn Already Ended")
            .with_detail(
                "The AI turn associated with this permission request has already ended. The subprocess exited before the decision could be delivered.",
            )
        })?;
    state
        .audit(&PermissionResolvedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: conversation.project_id,
            conversation_id: conversation.public_id,
            permission_id,
            tool_name: None,
            decision_kind,
        })
        .await;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) fn ensure_permission_mode_can_change_during_turn(
    conversation: &ai_conversations::Model,
    permission_mode: &str,
) -> Result<(), Problem> {
    if conversation.turn_status == "running" && permission_mode != "full-access" {
        return Err(problemdetails::new(StatusCode::CONFLICT)
            .with_title("Permission Mode Cannot Be Reduced Mid-Turn")
            .with_detail(
                "A running provider process can only be elevated to Auto. Stop the turn before switching it back to an approval-based mode.",
            ));
    }
    Ok(())
}

async fn persist_and_apply_permission_mode(
    state: &AppState,
    auth: &AuthContext,
    metadata: &RequestMetadata,
    conversation: &ai_conversations::Model,
    permission_mode: &str,
) -> Result<ai_conversations::Model, Problem> {
    let permission_mode = permission_mode.trim();
    if permission_mode.is_empty() {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Permission Mode")
            .with_detail("Permission mode cannot be empty."));
    }
    ensure_permission_mode_can_change_during_turn(conversation, permission_mode)?;
    ensure_runtime_permission(auth, Some(&conversation.ai_provider), Some(permission_mode))?;
    let updated = state
        .service
        .update_runtime_options(conversation, None, None, Some(permission_mode))
        .await?;
    let active_update = state.service.apply_active_permission_mode(
        updated.id,
        &updated.public_id,
        &updated.ai_permission_mode,
        auth,
        metadata,
    );
    let auto_approved_permission_ids = active_update
        .auto_approved
        .iter()
        .map(|permission| permission.id.clone())
        .collect::<Vec<_>>();
    state.service.publish_wire_event(
        updated.id,
        "runtime_options_updated",
        serde_json::json!({
            "permission_mode": updated.ai_permission_mode,
            "auto_approved_permission_ids": auto_approved_permission_ids,
        })
        .to_string(),
    );
    for permission in &active_update.auto_approved {
        state
            .audit(&PermissionResolvedAudit {
                context: AuditContext {
                    user_id: auth.user_id(),
                    ip_address: Some(metadata.ip_address.clone()),
                    user_agent: metadata.user_agent.clone(),
                },
                project_id: updated.project_id,
                conversation_id: updated.public_id.clone(),
                permission_id: permission.id.clone(),
                tool_name: Some(permission.tool_name.clone()),
                decision_kind: if permission.delivered {
                    "allow_tool_auto"
                } else {
                    "allow_tool_auto_delivery_failed"
                }
                .to_string(),
            })
            .await;
    }
    state
        .audit(&ConversationPermissionModeChangedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: updated.project_id,
            conversation_id: updated.public_id.clone(),
            permission_mode: updated.ai_permission_mode.clone(),
            applied_to_active_turn: active_update.applied_to_active_turn,
        })
        .await;
    Ok(updated)
}

#[utoipa::path(
    post, tag = "AI Chat",
    path = "/projects/{project_id}/ai/conversations/{public_id}/permission-mode",
    params(("project_id" = i32, Path,), ("public_id" = String, Path,)),
    request_body = UpdatePermissionModeRequest,
    responses((status = 200, body = ConversationResponse), (status = 400), (status = 401), (status = 403), (status = 404), (status = 409)),
    security(("bearer_auth" = []))
)]
pub async fn update_permission_mode(
    RequireAuth(auth): RequireAuth,
    ai_call: Option<Extension<temps_core::ai_tool_call::AiToolCall>>,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, public_id)): Path<(i32, String)>,
    Json(request): Json<UpdatePermissionModeRequest>,
) -> Result<Json<ConversationResponse>, Problem> {
    temps_core::ai_tool_call::require_human_caller(
        ai_call.as_ref().map(|e| &e.0),
        "update_permission_mode",
    )?;
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let conversation = state
        .service
        .get_by_public_id(project_id, auth.user_id(), &public_id)
        .await?;
    ensure_application_conversation_access(&state, &auth, &conversation).await?;
    ensure_context_read_permission(&auth, &conversation.context_type)?;
    let updated = persist_and_apply_permission_mode(
        &state,
        &auth,
        &metadata,
        &conversation,
        &request.permission_mode,
    )
    .await?;
    Ok(Json(ConversationResponse::from(updated)))
}

#[utoipa::path(
    post, tag = "AI Chat", path = "/ai/conversations/{public_id}/permission-mode",
    params(("public_id" = String, Path,)), request_body = UpdatePermissionModeRequest,
    responses((status = 200, body = ConversationResponse), (status = 400), (status = 401), (status = 403), (status = 404), (status = 409)),
    security(("bearer_auth" = []))
)]
pub async fn update_user_permission_mode(
    RequireAuth(auth): RequireAuth,
    ai_call: Option<Extension<temps_core::ai_tool_call::AiToolCall>>,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(public_id): Path<String>,
    Json(request): Json<UpdatePermissionModeRequest>,
) -> Result<Json<ConversationResponse>, Problem> {
    temps_core::ai_tool_call::require_human_caller(
        ai_call.as_ref().map(|e| &e.0),
        "update_user_permission_mode",
    )?;
    permission_guard!(auth, ProjectsWrite);
    deny_deployment_token!(auth);
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_mutation_access(&state, &auth, &conversation).await?;
    if conversation.context_type == "application" {
        ensure_application_conversation_permission(
            &state,
            &auth,
            &conversation,
            &Permission::SandboxesWrite,
        )
        .await?;
        ensure_application_conversation_permission(
            &state,
            &auth,
            &conversation,
            &Permission::ProjectsWrite,
        )
        .await?;
    }
    let updated = persist_and_apply_permission_mode(
        &state,
        &auth,
        &metadata,
        &conversation,
        &request.permission_mode,
    )
    .await?;
    Ok(Json(ConversationResponse::from(updated)))
}
