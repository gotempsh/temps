// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP surface for AI debugging conversations (ADR-023).
//!
//! `GET/POST /projects/{project_id}/ai/conversations` (find / get-or-create),
//! `GET .../{public_id}` (history), `POST .../{public_id}/messages` (durable
//! turn submission), `GET .../{public_id}/stream` (WebSocket live events),
//! `POST .../{public_id}/archive`. All scoped to the current user and their
//! permissions; starting a turn additionally requires a configured provider.
//!
//! This module holds the shared [`AppState`], the router and the OpenAPI
//! document; the handlers are split by concern:
//!
//! - `dto`: request and response types, with the validation of each request.
//! - `errors`: conversions from the domain errors into Problem Details.
//! - `limits`: request size limits shared by the routes.
//! - `access`: authorization checks shared by the handlers.
//! - `applications`, `application_projects`, `application_workspace` and
//!   `preview`: applications, their projects, their workspace and preview links.
//! - `workspace_sandbox`, `workspace_files`, `workspace_browse` and
//!   `workspace_terminal`: the sandbox behind a workspace, file transfer,
//!   browsing and the terminal.
//! - `conversations`, `messages` and `attachments`: conversations, sending and
//!   streaming messages, and chat attachments.
//! - `permissions` and `pending_actions`: permission requests, the permission
//!   mode and proposed write actions.
//! - `readiness` and `sandbox_mcp`: chat readiness and the harness MCP
//!   transport.

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
mod pending_actions;
mod permissions;
mod preview;
mod readiness;
mod sandbox_mcp;
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
pub use pending_actions::*;
pub use permissions::*;
pub use preview::*;
pub use readiness::*;
use sandbox_mcp::*;
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

fn audit_context(auth: &AuthContext, metadata: &RequestMetadata) -> AuditContext {
    AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
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
