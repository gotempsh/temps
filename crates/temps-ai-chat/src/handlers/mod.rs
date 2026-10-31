// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP surface for AI debugging conversations (ADR-023).
//!
//! `GET/POST /projects/{project_id}/ai/conversations` (find / get-or-create),
//! `GET .../{public_id}` (history), `POST .../{public_id}/messages` (durable
//! turn submission), `GET .../{public_id}/stream` (WebSocket live events),
//! `POST .../{public_id}/archive`. All scoped to the current user and their
//! permissions; starting a turn additionally requires a configured provider.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Component, Path as FsPath};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{
    header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE},
    HeaderMap, HeaderValue, StatusCode,
};
use axum::response::{IntoResponse, Response};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        DefaultBodyLimit, Multipart, Path, Query, State,
    },
    routing::{delete, get, post},
    Extension, Json, Router,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};
use temps_core::websocket_origin::validate_websocket_origin;
use tokio::io::AsyncReadExt;
use tokio_util::io::ReaderStream;
use tracing::{error, info};
use utoipa::{IntoParams, OpenApi, ToSchema};

use crate::application_link::{
    check_project_permission, synchronize_application_network, workspace_may_retain_data_plane,
    ApplicationProjectLinker, LinkProjectError, NetworkSyncError, ProjectPermissionError,
};
use temps_auth::permissions::Permission;
use temps_auth::{
    context::{AuthContext, AuthSource},
    deny_deployment_token, permission_guard, project_access_guard, project_scope_guard,
    RequireAuth,
};
use temps_core::problemdetails::{self, Problem};
use temps_core::{AuditContext, AuditLogger, RequestMetadata};
use temps_entities::source_type::SourceType;
use temps_entities::{ai_conversations, ai_messages, ai_pending_actions};

use temps_ai::streaming::{PermissionDecision, PermissionKind, PermissionRequest};

use crate::applications::{is_sensitive_workspace_path, WorkspaceEntryKind};
use crate::audit::{
    AiActionConfirmedAudit, AiActionRejectedAudit, ApplicationArchivedAudit,
    ApplicationCreatedAudit, ApplicationPreviewLinkCreatedAudit, ApplicationRenamedAudit,
    ApplicationRestoredAudit, ApplicationTopologyChangedAudit, ApplicationWorkspaceChangedAudit,
    ApplicationWorkspaceDeployedAudit, ApplicationWorkspaceFilesWrittenAudit,
    ApplicationWorkspaceSourceImportedAudit, ChatMessageSentAudit, ConversationArchivedAudit,
    ConversationAttachmentUploadedAudit, ConversationCreatedAudit,
    ConversationPermissionModeChangedAudit, ConversationRenamedAudit, ConversationRestoredAudit,
    GlobalWorkspaceFilesWrittenAudit, GlobalWorkspacePreviewLinkCreatedAudit,
    PermissionResolvedAudit, ThreadArtifactCreatedAudit, WorkspaceTerminalAttachedAudit,
};
use crate::pending_actions::{PendingActionError, PendingActionService};
use crate::sensitive::{
    display_value, redact_json_string, redact_text, redact_value, redact_workspace_diff,
};
use crate::service::{
    decode_message_before_cursor, encode_message_before_cursor, PendingPermissionOrigin,
    PermissionResolution,
};
use crate::thread_kind::{
    terminal_thread_tab, ThreadKind, ThreadKindError, DEFAULT_TERMINAL_THREAD_TITLE,
    THREAD_KIND_TERMINAL,
};
use crate::{
    ApplicationError, ApplicationService, ApplicationWorkspaceService, ChatError,
    ConversationService, HarnessMcpError,
};

mod access;
mod application_projects;
mod application_workspace;
mod applications;
mod attachments;
mod conversations;
mod dto;
mod errors;
mod limits;
mod messages;
mod preview;
mod workspace_browse;
mod workspace_files;
mod workspace_sandbox;
mod workspace_terminal;

pub(crate) use access::*;
pub use application_projects::*;
pub use application_workspace::*;
pub use applications::*;
pub use attachments::*;
pub use conversations::*;
pub use dto::*;
pub(crate) use limits::*;
pub use messages::*;
pub use preview::*;
pub use workspace_browse::*;
pub use workspace_files::*;
use workspace_sandbox::*;
pub use workspace_terminal::*;

/// Shared state for the chat routes.
pub struct AppState {
    pub service: Arc<ConversationService>,
    pub db: Arc<DatabaseConnection>,
    /// Audit logger for write operations (best-effort; never fails a request).
    pub audit_service: Arc<dyn AuditLogger>,
    /// Pending-action service (confirm/reject write proposals).
    pub pending_actions: Arc<PendingActionService>,
    pub applications: Arc<ApplicationService>,
    /// Goals the assistant is watching for each chat.
    pub goals: Arc<crate::goals::GoalService>,
    /// Credentials the user provided to the assistant through a sealed form.
    pub secret_slots: Arc<crate::secret_slots::SecretSlotService>,
    /// Files and images attached to assistant chats, which have no workspace.
    pub assistant_attachments: Arc<crate::attachments::AssistantAttachmentService>,
    /// Files being uploaded in chunks, until they are complete and stored.
    pub attachment_uploads: Arc<crate::attachment_uploads::UploadSessions>,
    pub project_service: Arc<temps_projects::ProjectService>,
    /// Derives the durable, Temps-owned directory mounted into an application
    /// harness sandbox. This never accepts a browser-supplied host path.
    pub application_workspaces: Arc<ApplicationWorkspaceService>,
    /// Runtime workspace transfer/preview policy. Optional only for minimal
    /// test/plugin wirings; production always supplies ConfigService.
    pub config_service: Option<Arc<temps_config::ConfigService>>,
    /// Optional because minimal/test server wiring may omit the standalone
    /// sandbox plugin. Application preview links fail closed when absent.
    pub application_sandboxes: Option<Arc<temps_sandbox::SandboxService>>,
    pub sandbox_snapshots: Option<Arc<temps_sandbox::services::SnapshotService>>,
    /// Existing Drop deployment pipeline, shared with the CLI upload flow.
    /// Optional only for minimal test servers that omit the deployments plugin.
    pub source_drop_deployer: Option<Arc<dyn temps_core::SourceDropDeployer>>,
    /// Optional checker for team-based project access (human sessions only).
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    /// State handed to the sandbox terminal bridge. Resolved when routes are
    /// assembled, because it needs the instance's sensitive-action policy,
    /// which is not registered yet during service registration. `None` in
    /// minimal wirings without the standalone sandbox plugin: the workspace
    /// terminal then answers 503 instead of disappearing.
    pub application_terminal: Option<Arc<temps_sandbox::handlers::SandboxAppState>>,
}

impl AppState {
    /// Emit an audit entry, best-effort: a logging failure must never fail the
    /// underlying operation (it already succeeded).
    pub(crate) async fn audit(&self, op: &dyn temps_core::AuditOperation) {
        if let Err(e) = self.audit_service.create_audit_log(op).await {
            error!("Failed to write AI-chat audit log: {e}");
        }
    }
}

/// Recursively scrub object keys that may carry sensitive values.
///
/// Any key whose name (case-insensitive) is or contains one of:
/// `value`, `secret`, `password`, `token`, `key`
/// has its value replaced with `"***"`. Objects nested below neutral wrapper
/// keys such as `parameters` and objects inside arrays are traversed too.
/// Structural fields (`operation`, `method`, `summary`, etc.) are left intact.
fn redact_params(v: &serde_json::Value) -> serde_json::Value {
    redact_value(v)
}

// --- Pending-action DTO ------------------------------------------------------

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

fn audit_context(auth: &AuthContext, metadata: &RequestMetadata) -> AuditContext {
    AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

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
fn permission_decision_matches_kind(kind: &PermissionKind, decision: &PermissionDecision) -> bool {
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

fn ensure_permission_mode_can_change_during_turn(
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

// --- Pending-action handlers -------------------------------------------------

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

/// MCP transport used only by an active application-harness turn. It is
/// intentionally outside `RequireAuth`: the sandbox does not receive the
/// user's session or an API key. Instead it presents a random, one-turn
/// capability whose in-process executor already captures that user's scoped
/// `AuthContext`.
async fn sandbox_tools_mcp(
    State(state): State<Arc<AppState>>,
    Path(bridge_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<temps_ai::mcp::McpRequest>,
) -> Response {
    let bearer = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default();
    match state
        .service
        .handle_harness_mcp_request(&bridge_id, bearer, request)
        .await
    {
        Ok(Some(response)) => (StatusCode::OK, Json(response)).into_response(),
        Ok(None) => StatusCode::ACCEPTED.into_response(),
        Err(HarnessMcpError::NotFound | HarnessMcpError::Unauthorized) => {
            // Do not reveal whether a random bridge id exists.
            (
                StatusCode::UNAUTHORIZED,
                Json(temps_ai::mcp::McpTransportError {
                    error: "sandbox tool capability is not authorized",
                }),
            )
                .into_response()
        }
        Err(HarnessMcpError::Expired) => (
            StatusCode::UNAUTHORIZED,
            Json(temps_ai::mcp::McpTransportError {
                error: "sandbox tool capability has expired",
            }),
        )
            .into_response(),
    }
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/ai/preview/authorize", post(authorize_workspace_preview))
        .route("/ai/workspace-activity", get(get_workspace_activity))
        .route("/ai/workspace", get(get_global_ai_workspace))
        .route("/ai/workspace/file-limits", get(get_workspace_file_limits))
        .route(
            "/ai/workspace/preview-link",
            post(create_global_workspace_preview_link),
        )
        .route(
            "/ai/workspace/changes",
            get(get_global_workspace_changes),
        )
        .route(
            "/ai/workspace/directory",
            get(get_global_workspace_directory),
        )
        .route("/ai/workspace/file", get(get_global_workspace_file))
        .route(
            "/ai/workspace/file/download",
            get(download_global_workspace_file),
        )
        .route(
            "/ai/workspace/files",
            post(upload_global_workspace_files)
                .layer(DefaultBodyLimit::max(WORKSPACE_IMPORT_BODY_LIMIT)),
        )
        .route("/ai/workspace/diff", get(get_global_workspace_diff))
        .route(
            "/ai/workspace/terminal",
            get(attach_global_workspace_terminal),
        )
        .route(
            "/ai/applications",
            get(list_applications).post(create_application),
        )
        .route(
            "/ai/applications/{application_public_id}",
            get(get_application)
                .patch(rename_application)
                .delete(archive_application),
        )
        .route(
            "/ai/applications/{application_public_id}/restore",
            post(restore_application),
        )
        .route(
            "/ai/applications/{application_public_id}/projects",
            post(create_application_project),
        )
        .route(
            "/ai/applications/{application_public_id}/projects/link",
            post(link_application_project),
        )
        .route(
            "/ai/applications/{application_public_id}/projects/{project_id}",
            delete(unlink_application_project),
        )
        .route(
            "/ai/applications/{application_public_id}/projects/{project_id}/primary",
            post(set_application_primary_project),
        )
        .route(
            "/ai/applications/{application_public_id}/projects/{project_id}/deploy",
            post(deploy_application_workspace_project),
        )
        .route(
            "/ai/applications/{application_public_id}/workspace",
            get(get_application_workspace).patch(update_application_workspace),
        )
        .route(
            "/ai/applications/{application_public_id}/workspace/actions",
            post(control_application_workspace),
        )
        .route(
            "/ai/applications/{application_public_id}/workspace/terminal",
            get(attach_application_workspace_terminal),
        )
        .route(
            "/ai/applications/{application_public_id}/projects/{project_id}/workspace/source",
            post(import_application_workspace_git),
        )
        .route(
            "/ai/applications/{application_public_id}/projects/{project_id}/workspace/files",
            post(write_application_workspace_files)
                .layer(DefaultBodyLimit::max(WORKSPACE_IMPORT_BODY_LIMIT)),
        )
        .route(
            "/ai/applications/{application_public_id}/workspace/files",
            post(upload_application_workspace_files)
                .layer(DefaultBodyLimit::max(WORKSPACE_IMPORT_BODY_LIMIT)),
        )
        .route(
            "/ai/applications/{application_public_id}/workspace/changes",
            get(get_application_workspace_changes),
        )
        .route(
            "/ai/applications/{application_public_id}/workspace/directory",
            get(get_application_workspace_directory),
        )
        .route(
            "/ai/applications/{application_public_id}/workspace/file",
            get(get_application_workspace_file),
        )
        .route(
            "/ai/applications/{application_public_id}/workspace/file/download",
            get(download_application_workspace_file),
        )
        .route(
            "/ai/applications/{application_public_id}/workspace/diff",
            get(get_application_workspace_diff),
        )
        .route(
            "/ai/applications/{application_public_id}/conversations",
            get(list_application_conversations).post(create_application_conversation),
        )
        .route(
            "/ai/applications/{application_public_id}/preview-link",
            post(create_application_preview_link),
        )
        .route(
            "/ai/applications/{application_public_id}/conversations/{conversation_public_id}/artifacts",
            get(list_thread_artifacts).post(create_thread_artifact),
        )
        // Readiness for the chat, so the UI can onboard instead of guessing.
        .route(
            "/projects/{project_id}/ai/readiness",
            get(get_chat_readiness),
        )
        // Unified cross-project switcher.
        .route(
            "/ai/conversations",
            get(list_all_conversations).post(create_global_conversation),
        )
        .route(
            "/ai/assistant/conversations",
            post(create_assistant_conversation),
        )
        .route(
            "/ai/conversations/{public_id}",
            get(get_user_conversation).patch(rename_user_conversation),
        )
        .route(
            "/ai/conversations/{public_id}/messages",
            post(send_user_message),
        )
        .route(
            "/ai/conversations/{public_id}/diagnostics",
            get(get_user_conversation_diagnostics),
        )
        .route(
            "/ai/conversations/{public_id}/usage",
            get(get_user_conversation_usage),
        )
        .route(
            "/ai/conversations/{public_id}/attachments",
            post(upload_user_conversation_attachment)
                .layer(DefaultBodyLimit::max(MAX_CHAT_ATTACHMENT_UPLOAD_BYTES)),
        )
        .route(
            "/ai/conversations/{public_id}/attachments/{attachment_id}",
            get(get_user_conversation_attachment),
        )
        .route(
            "/ai/conversations/{public_id}/stream",
            get(user_conversation_stream),
        )
        .route(
            "/ai/conversations/{public_id}/stop",
            post(stop_user_turn),
        )
        .route(
            "/ai/conversations/{public_id}/permission-mode",
            post(update_user_permission_mode),
        )
        .route(
            "/ai/conversations/{public_id}/archive",
            post(archive_user_conversation),
        )
        .route(
            "/ai/conversations/{public_id}/restore",
            post(restore_user_conversation),
        )
        .route(
            "/ai/conversations/{public_id}/permissions/{permission_id}/resolve",
            post(resolve_user_permission)
                .layer(DefaultBodyLimit::max(RESOLVE_PERMISSION_BODY_LIMIT)),
        )
        .route(
            "/ai/conversations/{public_id}/pending-actions",
            get(list_user_pending_actions),
        )
        .route(
            "/ai/pending-actions/{action_public_id}",
            get(get_user_pending_action),
        )
        .route(
            "/ai/pending-actions/{action_public_id}/confirm",
            post(confirm_user_pending_action),
        )
        .route(
            "/ai/pending-actions/{action_public_id}/reject",
            post(reject_user_pending_action),
        )
        .route(
            "/ai/sandbox-tools/{bridge_id}/mcp",
            post(sandbox_tools_mcp).layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route(
            "/projects/{project_id}/ai/conversations",
            get(find_conversation).post(create_conversation),
        )
        // Static `/list` registered before the `{public_id}` param route; matchit
        // prioritizes the literal segment so it can't be shadowed.
        .route(
            "/projects/{project_id}/ai/conversations/list",
            get(list_conversations),
        )
        .route(
            "/projects/{project_id}/ai/conversations/{public_id}",
            get(get_conversation).patch(rename_conversation),
        )
        .route(
            "/projects/{project_id}/ai/conversations/{public_id}/messages",
            post(send_message),
        )
        .route(
            "/projects/{project_id}/ai/conversations/{public_id}/stop",
            post(stop_turn),
        )
        .route(
            "/projects/{project_id}/ai/conversations/{public_id}/permission-mode",
            post(update_permission_mode),
        )
        // Live wire for cross-tab sync (ADR-038 follow-up). Read-only.
        .route(
            "/projects/{project_id}/ai/conversations/{public_id}/stream",
            get(conversation_stream),
        )
        .route(
            "/projects/{project_id}/ai/conversations/{public_id}/archive",
            post(archive_conversation),
        )
        // Permission-bridge resolve endpoint (ADR-038 Phase 2).
        // The body-limit layer caps inbound JSON before deserialization so an
        // oversized payload is rejected with 413 rather than OOM-ing the server.
        .route(
            "/projects/{project_id}/ai/conversations/{public_id}/permissions/{permission_id}/resolve",
            post(resolve_permission)
                .layer(DefaultBodyLimit::max(RESOLVE_PERMISSION_BODY_LIMIT)),
        )
        // Pending-action routes (propose-then-confirm write actions).
        .route(
            "/projects/{project_id}/ai/conversations/{public_id}/pending-actions",
            get(list_pending_actions),
        )
        .route(
            "/projects/{project_id}/ai/pending-actions/{action_public_id}",
            get(get_pending_action),
        )
        .route(
            "/projects/{project_id}/ai/pending-actions/{action_public_id}/confirm",
            post(confirm_pending_action),
        )
        .route(
            "/projects/{project_id}/ai/pending-actions/{action_public_id}/reject",
            post(reject_pending_action),
        )
}

// `user_conversation_stream` and `sandbox_tools_mcp` are intentionally absent
// from the public OpenAPI document. They are capability-bound streaming
// transports (SSE and MCP respectively), not ordinary bearer-authenticated
// JSON APIs, and the generated REST clients cannot model either lifecycle.
#[derive(OpenApi)]
#[openapi(
    paths(
        list_applications,
        create_application,
        archive_application,
        rename_application,
        restore_application,
        create_application_project,
        deploy_application_workspace_project,
        link_application_project,
        unlink_application_project,
        set_application_primary_project,
        get_application,
        create_application_preview_link,
        create_global_workspace_preview_link,
        authorize_workspace_preview,
        get_workspace_file_limits,
        get_application_workspace_changes,
        get_global_workspace_changes,
        get_application_workspace_directory,
        get_global_workspace_directory,
        get_application_workspace_file,
        get_global_workspace_file,
        attach_application_workspace_terminal,
        attach_global_workspace_terminal,
        download_application_workspace_file,
        download_global_workspace_file,
        get_application_workspace_diff,
        get_global_workspace_diff,
        get_application_workspace,
        update_application_workspace,
        control_application_workspace,
        import_application_workspace_git,
        write_application_workspace_files,
        upload_application_workspace_files,
        upload_global_workspace_files,
        list_application_conversations,
        create_application_conversation,
        list_thread_artifacts,
        create_thread_artifact,
        get_chat_readiness,
        find_conversation,
        list_conversations,
        list_all_conversations,
        get_workspace_activity,
        create_global_conversation,
        create_assistant_conversation,
        get_global_ai_workspace,
        get_user_conversation,
        get_user_conversation_diagnostics,
        get_user_conversation_usage,
        send_user_message,
        upload_user_conversation_attachment,
        get_user_conversation_attachment,
        stop_user_turn,
        update_user_permission_mode,
        archive_user_conversation,
        restore_user_conversation,
        rename_user_conversation,
        resolve_user_permission,
        list_user_pending_actions,
        get_user_pending_action,
        confirm_user_pending_action,
        reject_user_pending_action,
        create_conversation,
        get_conversation,
        send_message,
        stop_turn,
        update_permission_mode,
        archive_conversation,
        rename_conversation,
        resolve_permission,
        list_pending_actions,
        get_pending_action,
        confirm_pending_action,
        reject_pending_action,
    ),
    components(schemas(
        crate::usage::ConversationUsage,
        crate::usage::ModelSpend,
        ApplicationProjectResponse,
        ApplicationProjectEnvironmentResponse,
        ApplicationResponse,
        CreateApplicationPreviewLinkRequest,
        AuthorizeWorkspacePreviewRequest,
        ApplicationPreviewLinkResponse,
        ApplicationWorkspaceFileResponse,
        ApplicationWorkspaceChangesResponse,
        ApplicationWorkspaceDiffResponse,
        ApplicationWorkspaceResponse,
        UpdateApplicationWorkspaceRequest,
        ControlApplicationWorkspaceRequest,
        ImportApplicationWorkspaceGitRequest,
        ApplicationWorkspaceFileWrite,
        WriteApplicationWorkspaceFilesRequest,
        WriteApplicationWorkspaceFilesResponse,
        CreateApplicationRequest,
        RenameApplicationRequest,
        CreateApplicationProjectRequest,
        DeployApplicationProjectRequest,
        ApplicationProjectDeploymentResponse,
        LinkApplicationProjectRequest,
        CreateApplicationConversationRequest,
        ThreadArtifactResponse,
        CreateThreadArtifactRequest,
        ConversationResponse,
        GlobalConversationResponse,
        ConversationSandboxResponse,
        ConversationWorkspaceResponse,
        MessageResponse,
        ChatAttachmentResponse,
        ChatAttachmentReference,
        ChatAttachmentUpload,
        ToolInfo,
        MessagePart,
        ConversationMessagePageResponse,
        ConversationDetailResponse,
        ConversationListStatus,
        ConversationListScope,
        WorkspaceActivityResponse,
        WorkspaceActivitySummary,
        WorkspaceHarnessActivity,
        CreateConversationRequest,
        CreateGlobalConversationRequest,
        CreateAssistantConversationRequest,
        RenameConversationRequest,
        UpdatePermissionModeRequest,
        SendMessageRequest,
        ToolCallEvent,
        ToolResultEvent,
        PermissionRequestedEvent,
        PermissionKind,
        PermissionDecision,
        ResolvePermissionRequest,
        PendingActionResponse,
        ChatReadinessResponse,
    ))
)]
pub struct AiChatApiDoc;

#[cfg(test)]
mod tests;

/// The `/ai/conversations/{id}` and `/ai/pending-actions/{id}` mutations,
/// driven through the real handlers. The state's database is a mock that
/// records every statement, so each test can show not only the status a
/// caller gets but that a refused call changed nothing.
#[cfg(test)]
mod user_route_authorization_tests;

#[cfg(test)]
mod websocket_origin_tests {
    /// A cross-origin page must not be able to open this socket with the
    /// user's cookie (browsers apply no CORS to WebSocket handshakes), so the
    /// Origin check has to run before any lookup or upgrade work.
    #[test]
    fn websocket_handlers_validate_origin_before_any_work() {
        let source = concat!(
            include_str!("workspace_terminal.rs"),
            include_str!("messages.rs")
        );
        for name in [
            "attach_application_workspace_terminal",
            "attach_global_workspace_terminal",
            "conversation_stream",
            "user_conversation_stream",
        ] {
            let start = source
                .find(&format!("pub async fn {name}("))
                .unwrap_or_else(|| panic!("{name} handler not found in source"));
            let body_start = start + source[start..].find(") -> ").unwrap_or(0);
            let body = &source[body_start..];
            let check = body
                .find("validate_websocket_origin(&headers)?")
                .unwrap_or_else(|| panic!("{name} must call validate_websocket_origin"));
            let first_await = body.find(".await").unwrap_or(usize::MAX);
            assert!(
                check < first_await,
                "{name} must validate the Origin before its first await"
            );
        }
    }
}
