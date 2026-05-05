pub mod git_credential;
pub mod memory;
pub mod sessions;

use std::sync::Arc;

use axum::Router;
use sea_orm::DatabaseConnection;
use temps_core::AuditLogger;
use utoipa::OpenApi;

use crate::services::git_credential_service::GitCredentialService;
use crate::services::memory_service::WorkflowMemoryService;
use crate::services::message_executor::MessageExecutor;
use crate::services::session_manager::WorkspaceSessionManager;
use crate::services::workspace_service::WorkspaceService;

/// Shared state for workspace HTTP handlers.
pub struct WorkspaceAppState {
    pub db: Arc<DatabaseConnection>,
    pub workspace_service: Arc<WorkspaceService>,
    pub session_manager: Arc<WorkspaceSessionManager>,
    pub message_executor: Option<Arc<MessageExecutor>>,
    pub memory_service: Arc<WorkflowMemoryService>,
    pub audit_service: Arc<dyn AuditLogger>,
    /// Platform settings service. Used to build preview URLs
    /// (`ws-<sid>-<port>.<preview_domain>`) for workspace session responses.
    pub platform_config_service: Arc<temps_config::ConfigService>,
    /// Docker client used by the terminal WebSocket handler to open a PTY
    /// exec against the session's sandbox container. Optional: when missing
    /// (e.g. local sandbox provider, non-Docker env), the terminal endpoint
    /// returns 503.
    pub docker: Option<Arc<bollard::Docker>>,
    /// Mints per-operation, single-repo, narrow-permission git
    /// credentials for the in-sandbox credential daemon. Optional: only
    /// present when the git plugin is loaded — otherwise the
    /// `/workspace/git-credential` endpoint returns 503.
    pub git_credential_service: Option<Arc<GitCredentialService>>,
}

/// OpenAPI document for the temps-workspace crate.
///
/// Registered into the unified `/api-docs/openapi.json` via the plugin system's
/// `openapi_schema()` hook. SDK generators should filter by the `Workspace` or
/// `WorkspaceMemory` tag.
///
/// The WebSocket terminal endpoint (`GET /terminal`) is intentionally omitted —
/// utoipa cannot represent WebSocket upgrades and hey-api cannot generate a
/// typed client for them.
#[derive(OpenApi)]
#[openapi(
    paths(
        // Session CRUD
        sessions::workspace_list_sessions,
        sessions::workspace_start_session,
        sessions::workspace_get_session,
        sessions::workspace_update_session,
        sessions::workspace_delete_session,

        // Session lifecycle
        sessions::workspace_close_session,
        sessions::workspace_reopen_session,
        sessions::workspace_cancel_run,

        // Messages
        sessions::workspace_send_message,
        sessions::workspace_stream_messages,

        // Preview password
        sessions::workspace_regenerate_preview_password,

        // Sandbox lifecycle
        sessions::workspace_stop_sandbox,
        sessions::workspace_start_sandbox,
        sessions::workspace_restart_sandbox,
        sessions::workspace_refresh_sandbox,
        sessions::workspace_sandbox_stats,

        // Terminal
        sessions::workspace_list_terminal_tabs,
        sessions::workspace_delete_terminal_tab,
        sessions::workspace_terminal_paste_image,

        // Memory (legacy and v1 share the same handlers)
        memory::list_memory,
        memory::search_memory,
        memory::write_memory,
        memory::supersede_memory,
        memory::drop_memory,

        // Git credential (in-sandbox daemon mint endpoint)
        git_credential::mint_git_credential,
    ),
    components(schemas(
        // Session DTOs
        sessions::WorkspaceSessionResponse,
        sessions::WorkspaceMessageResponse,
        sessions::WorkspaceSessionWithMessagesResponse,
        sessions::WorkspaceSessionListResponse,
        sessions::StartSessionRequest,
        sessions::UpdateSessionBody,
        sessions::SendMessageBody,
        sessions::WorkspacePaginationParams,
        sessions::PreviewPortUrl,

        // Sandbox / terminal DTOs
        sessions::WorkspaceSandboxStatsResponse,
        sessions::WorkspaceTerminalTab,
        sessions::WorkspaceTerminalTabsResponse,
        sessions::WorkspacePasteImageRequest,
        sessions::WorkspacePasteImageResponse,

        // Memory DTOs
        memory::MemoryFactResponse,
        memory::MemoryListResponse,
        memory::WriteMemoryBody,
        memory::SupersedeBody,

        // Git credential DTOs
        git_credential::MintGitCredentialRequest,
        git_credential::MintGitCredentialResponse,
        git_credential::MintOperation,
    )),
    tags(
        (name = "Workspace", description = "Interactive AI workspace sessions with sandbox containers, message streaming, and terminal access."),
        (name = "WorkspaceMemory", description = "Workflow agent memory — persistent fact store read/written by agents across runs."),
    )
)]
pub struct WorkspaceApiDoc;

/// Configure all workspace routes.
pub fn configure_routes() -> Router<Arc<WorkspaceAppState>> {
    sessions::routes()
        .merge(memory::routes())
        .merge(git_credential::routes())
}
