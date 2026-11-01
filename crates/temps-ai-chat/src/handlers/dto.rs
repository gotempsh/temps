// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Request and response types for the chat, application and workspace routes,
//! with the validation that belongs to each request.

use super::*;

#[derive(Debug, Serialize, ToSchema)]
pub struct ConversationResponse {
    pub public_id: String,
    /// `ai` (a conversation with an AI harness) or `terminal` (an interactive
    /// shell in the workspace sandbox). Fixed at creation.
    pub kind: String,
    pub project_id: Option<i32>,
    pub context_type: String,
    pub context_id: String,
    pub title: Option<String>,
    pub status: String,
    pub created_at: String,
    pub last_activity_at: String,
    pub ai_provider: String,
    pub ai_model: String,
    pub ai_thinking_level: Option<String>,
    pub ai_permission_mode: String,
    /// Server-authoritative lifecycle for the current/most recent turn.
    pub turn_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub application_id: Option<i64>,
    /// The project an assistant chat was started about, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus_project_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<ChatFailureResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_usage: Option<ContextWindowUsageResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ChatFailureResponse {
    pub code: String,
    pub title: String,
    pub detail: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ContextWindowUsageResponse {
    pub used_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub source: String,
    pub estimated: bool,
    pub updated_at: String,
}

pub(super) fn conversation_failure(
    metadata: Option<&serde_json::Value>,
    turn_status: &str,
) -> Option<ChatFailureResponse> {
    if turn_status != "failed" {
        return None;
    }
    metadata
        .and_then(|metadata| metadata.get("last_failure"))
        .and_then(|failure| serde_json::from_value(failure.clone()).ok())
}

pub(super) fn conversation_context_usage(
    metadata: Option<&serde_json::Value>,
) -> Option<ContextWindowUsageResponse> {
    metadata
        .and_then(|metadata| metadata.get("context_usage"))
        .and_then(|usage| serde_json::from_value(usage.clone()).ok())
}

impl From<ai_conversations::Model> for ConversationResponse {
    fn from(m: ai_conversations::Model) -> Self {
        let failure = conversation_failure(m.metadata.as_ref(), &m.turn_status);
        let context_usage = conversation_context_usage(m.metadata.as_ref());
        let focus_project_id = crate::service::focus_project_id(m.metadata.as_ref());
        Self {
            public_id: m.public_id,
            kind: m.kind,
            project_id: m.project_id,
            context_type: m.context_type,
            context_id: m.context_id,
            title: m.title,
            status: m.status,
            created_at: m.created_at.to_rfc3339(),
            last_activity_at: m.last_activity_at.to_rfc3339(),
            ai_provider: m.ai_provider,
            ai_model: m.ai_model,
            ai_thinking_level: m.ai_thinking_level,
            ai_permission_mode: m.ai_permission_mode,
            turn_status: m.turn_status,
            active_turn_id: m.active_turn_id,
            turn_started_at: m.turn_started_at.map(|value| value.to_rfc3339()),
            application_id: m.application_id,
            focus_project_id,
            failure,
            context_usage,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ApplicationProjectResponse {
    pub id: i32,
    pub name: String,
    pub slug: String,
    pub repository: Option<String>,
    pub main_branch: String,
    pub is_private: bool,
    pub is_primary: bool,
    pub automatic_deploy: bool,
    pub last_deployment_at: Option<String>,
    pub environments: Vec<ApplicationProjectEnvironmentResponse>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ApplicationProjectEnvironmentResponse {
    pub name: String,
    pub slug: String,
    pub sleeping: bool,
    pub deployment_state: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ApplicationResponse {
    pub public_id: String,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub projects: Vec<ApplicationProjectResponse>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<crate::applications::ApplicationWithProjects> for ApplicationResponse {
    fn from(value: crate::applications::ApplicationWithProjects) -> Self {
        let primary_project_id = value.primary_project_id;
        let mut environment_statuses = value.environment_statuses;
        Self {
            public_id: value.application.public_id,
            name: value.application.name,
            description: value.application.description,
            status: value.application.status,
            projects: value
                .projects
                .into_iter()
                .map(|project| ApplicationProjectResponse {
                    id: project.id,
                    name: project.name,
                    slug: project.slug,
                    repository: (!project.repo_owner.is_empty() && !project.repo_name.is_empty())
                        .then(|| format!("{}/{}", project.repo_owner, project.repo_name)),
                    main_branch: project.main_branch,
                    is_private: !project.is_public_repo,
                    is_primary: primary_project_id == Some(project.id),
                    automatic_deploy: project
                        .deployment_config
                        .as_ref()
                        .and_then(|config| config.automatic_deploy)
                        .unwrap_or(false),
                    last_deployment_at: project.last_deployment.map(|date| date.to_rfc3339()),
                    environments: environment_statuses
                        .remove(&project.id)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|environment| ApplicationProjectEnvironmentResponse {
                            name: environment.name,
                            slug: environment.slug,
                            sleeping: environment.sleeping,
                            deployment_state: environment.deployment_state,
                        })
                        .collect(),
                })
                .collect(),
            created_at: value.application.created_at.to_rfc3339(),
            updated_at: value.application.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateApplicationRequest {
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub project_ids: Vec<i32>,
    /// Recommended default: create a deployable, Git-less starter project in
    /// the application's persistent workspace.
    #[serde(default)]
    pub starter_project: Option<CreateApplicationProjectRequest>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct RenameApplicationRequest {
    /// New name shown for the application. Trimmed; 1 to 200 characters.
    pub name: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateApplicationProjectRequest {
    pub name: String,
    /// Deployment preset for the project. Defaults to `autopack`, which
    /// detects the application runtime from the workspace source.
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub exposed_port: Option<i32>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct LinkApplicationProjectRequest {
    pub project_id: i32,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct DeployApplicationProjectRequest {
    /// Target environment. Omit to use production, then the project's oldest
    /// active environment as a fallback.
    #[serde(default)]
    pub environment_id: Option<i32>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ApplicationProjectDeploymentResponse {
    pub id: i32,
    pub project_id: i32,
    pub environment_id: i32,
    pub slug: String,
    pub state: String,
    pub source_type: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateApplicationConversationRequest {
    /// A registered development harness (for example `claude_cli`).
    /// Application threads never inherit an API-gateway provider. Ignored for
    /// terminal threads, which have no harness.
    pub ai_provider: Option<String>,
    /// `ai` (default) or `terminal`.
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateGlobalConversationRequest {
    /// `ai` (default) or `terminal`. A terminal thread has no harness, model
    /// or turns.
    #[serde(default)]
    pub kind: Option<String>,
    /// A registered development harness. Global operator chats run inside a
    /// persistent Temps-managed sandbox rather than on the host filesystem.
    pub ai_provider: Option<String>,
    pub ai_model: Option<String>,
    pub ai_thinking_level: Option<String>,
    pub ai_permission_mode: Option<String>,
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct CreateAssistantConversationRequest {
    /// An AI-gateway provider id (`gateway_key:<id>`). Defaults to the active
    /// OpenRouter key, then to the instance default. Host harnesses are
    /// rejected: the assistant has no sandbox.
    pub ai_provider: Option<String>,
    /// Model id to start with; defaults to the provider's default model.
    pub ai_model: Option<String>,
    /// A project the chat is about. The assistant is told which it is, so it
    /// needs no searching to resolve "my app". It is a default, not a limit,
    /// and grants nothing: the chat still acts with the user's own permissions.
    #[serde(default)]
    pub project_id: Option<i32>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateApplicationPreviewLinkRequest {
    /// The development server port detected or selected in the sandbox.
    pub port: u16,
    /// Optional same-origin path to open after the preview grant is exchanged.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AuthorizeWorkspacePreviewRequest {
    pub sandbox_public_id: String,
    pub port: u16,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ApplicationPreviewLinkResponse {
    /// A short-lived authenticated URL. Its fragment carries the grant and
    /// never reaches the development server or referrer headers.
    pub url: String,
    pub expires_at: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ApplicationWorkspaceFileResponse {
    pub path: String,
    pub status: Option<String>,
    pub staged: bool,
    pub unstaged: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ApplicationWorkspaceChangesResponse {
    pub branch: Option<String>,
    pub head: Option<String>,
    pub clean: bool,
    pub truncated: bool,
    pub files_truncated: bool,
    pub changes_truncated: bool,
    /// Number of file paths discovered within the server-side safety cap.
    pub listed_file_count: usize,
    /// Opaque position for the next bounded page, when another page exists.
    pub next_cursor: Option<usize>,
    pub files: Vec<String>,
    pub changes: Vec<ApplicationWorkspaceFileResponse>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ApplicationWorkspaceChangesQuery {
    /// Opaque position returned by the previous response.
    #[serde(default)]
    pub cursor: Option<usize>,
    /// Page size. Defaults to 100 and is capped at 200.
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct ApplicationWorkspaceDiffQuery {
    pub path: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ApplicationWorkspaceDiffResponse {
    pub path: String,
    pub diff: String,
    pub truncated: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct ApplicationWorkspaceDirectoryQuery {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub cursor: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct ApplicationWorkspaceFileQuery {
    pub path: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ApplicationWorkspaceDirectoryEntryResponse {
    pub name: String,
    pub path: String,
    pub kind: String,
    pub size_bytes: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ApplicationWorkspaceDirectoryResponse {
    pub path: String,
    pub entries: Vec<ApplicationWorkspaceDirectoryEntryResponse>,
    pub next_cursor: Option<usize>,
    pub truncated: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ApplicationWorkspaceFileContentResponse {
    pub path: String,
    pub content: Option<String>,
    /// Base64-encoded raster image bytes when this file has a verified,
    /// browser-safe image signature and is within the configured preview cap.
    pub content_b64: Option<String>,
    pub media_type: Option<String>,
    pub binary: bool,
    pub size_bytes: u64,
    pub truncated: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ApplicationWorkspaceResponse {
    pub state: String,
    pub desired_state: String,
    pub sandbox_public_id: Option<String>,
    pub runtime: String,
    pub image: Option<String>,
    pub runtime_compatible: Option<bool>,
    pub runtime_update_available: bool,
    pub runtime_update_image: Option<String>,
    pub runtime_update_error: Option<String>,
    pub cpu_limit: f64,
    pub memory_limit_mb: i64,
    pub pids_limit: i64,
    pub disk_limit_mb: i64,
    /// Docker bind-mounted workspaces report usage but cannot enforce a
    /// per-directory quota. Firecracker workspaces enforce this value.
    pub disk_limit_enforced: bool,
    pub idle_timeout_secs: i64,
    pub memory_used_bytes: Option<u64>,
    pub pids_used: Option<u64>,
    pub disk_used_bytes: Option<u64>,
    pub cpu_usage_usec: Option<u64>,
    pub open_preview_ports: Vec<u16>,
    pub persistent_volume_healthy: bool,
    pub data_network_service_count: usize,
    pub last_error: Option<String>,
    pub snapshot_id: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateApplicationWorkspaceRequest {
    pub runtime: Option<String>,
    /// Pinned Temps-managed daemon image. Empty string restores the runtime default.
    /// Omit to leave the selected image unchanged. Arbitrary images are rejected.
    pub image: Option<String>,
    pub cpu_limit: Option<f64>,
    pub memory_limit_mb: Option<i64>,
    pub pids_limit: Option<i64>,
    pub disk_limit_mb: Option<i64>,
    pub idle_timeout_secs: Option<i64>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ControlApplicationWorkspaceRequest {
    /// restart, pause, resume, rebuild, snapshot, restore, or update_runtime
    pub action: String,
    /// Must be true for update_runtime because compute is replaced.
    pub confirm: Option<bool>,
    /// Optional built-in flavor to select during update_runtime.
    pub runtime: Option<String>,
    pub snapshot_id: Option<String>,
    pub label: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportApplicationWorkspaceGitRequest {
    pub url: String,
    pub revision: Option<String>,
    pub depth: Option<u32>,
    /// Opaque user-owned connection reference. The credential value remains
    /// server-side and is restricted to the provider's configured origin.
    pub git_connection_id: Option<i32>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ApplicationWorkspaceFileWrite {
    /// Project-relative path. Absolute paths and traversal are rejected.
    pub path: String,
    pub contents_b64: String,
    pub mode: Option<u32>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteApplicationWorkspaceFilesRequest {
    pub files: Vec<ApplicationWorkspaceFileWrite>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WriteApplicationWorkspaceFilesResponse {
    pub written: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ThreadArtifactResponse {
    pub public_id: String,
    pub kind: String,
    pub schema_version: i32,
    pub title: Option<String>,
    pub payload: serde_json::Value,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

impl From<temps_entities::ai_thread_artifacts::Model> for ThreadArtifactResponse {
    fn from(value: temps_entities::ai_thread_artifacts::Model) -> Self {
        Self {
            public_id: value.public_id,
            kind: value.kind,
            schema_version: value.schema_version,
            title: value.title,
            // Artifact writes reject plaintext credentials. Redact again on
            // the response boundary so a legacy row or future validation
            // regression cannot expose credential-like JSON to the browser.
            payload: redact_value(&value.payload),
            status: value.status,
            created_at: value.created_at.to_rfc3339(),
            updated_at: value.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateThreadArtifactRequest {
    pub kind: String,
    pub title: Option<String>,
    pub payload: serde_json::Value,
}

/// A conversation in the unified cross-project switcher: carries the project it
/// belongs to (name/slug) so the UI can show where the chat was started and
/// link back to the source.
#[derive(Debug, Serialize, ToSchema)]
pub struct GlobalConversationResponse {
    pub public_id: String,
    pub project_id: Option<i32>,
    pub project_name: Option<String>,
    pub project_slug: Option<String>,
    pub context_type: String,
    pub context_id: String,
    pub title: Option<String>,
    pub status: String,
    /// `"ai"` for an agent conversation, `"terminal"` for a shell thread.
    pub kind: String,
    pub created_at: String,
    pub last_activity_at: String,
    pub ai_provider: String,
    pub ai_model: String,
    pub ai_thinking_level: Option<String>,
    pub ai_permission_mode: String,
    /// Server-authoritative lifecycle for the current/most recent turn.
    pub turn_status: String,
    /// The project an assistant chat was started about, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus_project_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<ChatFailureResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_usage: Option<ContextWindowUsageResponse>,
    /// The sandbox linked to this chat, when it is still running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<ConversationSandboxResponse>,
    /// The workspace an assistant chat is in (ADR-053).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<ConversationWorkspaceResponse>,
}

/// The workspace a chat is in, as the conversation list shows it.
#[derive(Debug, Serialize, ToSchema)]
pub struct ConversationWorkspaceResponse {
    /// Public id of the workspace (`app_…`).
    pub public_id: String,
    pub name: String,
}

/// The sandbox a chat is linked to, as the conversation list shows it.
#[derive(Debug, Serialize, ToSchema)]
pub struct ConversationSandboxResponse {
    /// Public id of the sandbox (`sbx_…`).
    pub sandbox_id: String,
    pub name: String,
    /// Isolation backend: `docker`, `firecracker` or `microsandbox`. Absent
    /// for sandboxes created before the backend was recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct WorkspaceActivityQuery {
    /// Comma-separated application public IDs (maximum 100). Omit for the global workspace only.
    pub application_public_ids: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WorkspaceActivityResponse {
    pub workspaces: Vec<WorkspaceActivitySummary>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WorkspaceActivitySummary {
    /// Null identifies the user's global workspace.
    pub application_public_id: Option<String>,
    pub total: u64,
    pub harnesses: Vec<WorkspaceHarnessActivity>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WorkspaceHarnessActivity {
    pub ai_provider: String,
    pub total: u64,
    pub pending: u64,
    pub running: u64,
    pub completed: u64,
    pub failed: u64,
    pub cancelled: u64,
    pub idle: u64,
}

impl WorkspaceHarnessActivity {
    pub(super) fn add_status(&mut self, turn_status: &str, count: u64) {
        self.total = self.total.saturating_add(count);
        let bucket = match turn_status {
            "pending" => &mut self.pending,
            "running" => &mut self.running,
            "completed" => &mut self.completed,
            "failed" => &mut self.failed,
            "cancelled" => &mut self.cancelled,
            _ => &mut self.idle,
        };
        *bucket = bucket.saturating_add(count);
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MessageResponse {
    /// Stable opaque cursor for this stored message. Clients must treat this as
    /// an uninterpreted token and send it back through the `before` query.
    pub cursor: String,
    pub role: String,
    pub content: String,
    pub created_at: String,
    /// Tools the assistant ran on this turn (persisted in message metadata), so
    /// the chat replays its tool work after a reload. Absent for plain turns.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolInfo>>,
    /// Ordered render segments (text / tool, in the order they occurred) so a
    /// reloaded chat shows the same interleaving as the live stream. Absent for
    /// older messages persisted before parts were tracked; the client then falls
    /// back to `tools` (rendered first) + `content`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<MessagePart>>,
    /// Files copied into the persistent workspace for this user turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<ChatAttachmentResponse>>,
    /// Next steps already suggested after this answer, shown again on reload
    /// without asking the model a second time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_steps: Option<Vec<String>>,
}

#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct ChatAttachmentResponse {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub sandbox_path: String,
    pub is_image: bool,
    #[serde(skip)]
    #[schema(ignore)]
    pub(crate) contents: Vec<u8>,
}

impl std::fmt::Debug for ChatAttachmentResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChatAttachmentResponse")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("mime_type", &self.mime_type)
            .field("size_bytes", &self.size_bytes)
            .field("sandbox_path", &self.sandbox_path)
            .field("is_image", &self.is_image)
            .field(
                "contents",
                &format_args!("[REDACTED; {} bytes]", self.contents.len()),
            )
            .finish()
    }
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ChatAttachmentReference {
    pub id: String,
    pub name: String,
}

#[derive(Debug, ToSchema)]
pub struct ChatAttachmentUpload {
    #[schema(value_type = String, format = Binary)]
    pub file: String,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ChatAttachmentContentQuery {
    pub name: String,
}

/// One persisted tool invocation + its result, attached to an assistant message.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ToolInfo {
    pub id: String,
    pub name: String,
    pub arguments: String,
    pub result: Option<String>,
}

/// One ordered segment of an assistant turn: a chunk of prose, or a tool
/// invocation. Mirrors the `metadata.parts` persisted by the chat service.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum MessagePart {
    Text { text: String },
    Tool { tool: ToolInfo },
}

impl From<ai_messages::Model> for MessageResponse {
    fn from(m: ai_messages::Model) -> Self {
        let cursor = encode_message_before_cursor(m.id);
        let redact_tool = |mut tool: ToolInfo| {
            tool.arguments = redact_json_string(&tool.arguments);
            tool.result = tool.result.map(|result| redact_json_string(&result));
            tool
        };
        let tools = m
            .metadata
            .as_ref()
            .and_then(|v| v.get("tools"))
            .and_then(|t| serde_json::from_value::<Vec<ToolInfo>>(t.clone()).ok())
            .map(|tools| tools.into_iter().map(&redact_tool).collect::<Vec<_>>())
            .filter(|t| !t.is_empty());
        let parts = m
            .metadata
            .as_ref()
            .and_then(|v| v.get("parts"))
            .and_then(|p| serde_json::from_value::<Vec<MessagePart>>(p.clone()).ok())
            .map(|parts| {
                parts
                    .into_iter()
                    .map(|part| match part {
                        MessagePart::Tool { tool } => MessagePart::Tool {
                            tool: redact_tool(tool),
                        },
                        text => text,
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|p| !p.is_empty());
        let attachments = m
            .metadata
            .as_ref()
            .and_then(|value| value.get("attachments"))
            .and_then(|value| {
                serde_json::from_value::<Vec<ChatAttachmentResponse>>(value.clone()).ok()
            })
            .filter(|attachments| !attachments.is_empty());
        let next_steps = crate::suggestions::saved_next_steps(m.metadata.as_ref())
            .map(|saved| saved.suggestions);
        Self {
            cursor,
            role: m.role,
            content: m.content,
            created_at: m.created_at.to_rfc3339(),
            tools,
            parts,
            attachments,
            next_steps,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ConversationMessagePageResponse {
    pub has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_before: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ConversationDetailResponse {
    #[serde(flatten)]
    pub conversation: ConversationResponse,
    /// Turns oldest-first. The `system` seed message is omitted (internal).
    pub messages: Vec<MessageResponse>,
    pub page: ConversationMessagePageResponse,
    /// A still-unresolved interactive permission request (ADR-038 Phase 2), if
    /// one is pending on this conversation right now. The client renders this
    /// as a live, answerable `PermissionCard` — without it, a question that
    /// arrived while the tab was away shows only as inert history text with no
    /// way to answer it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_permission: Option<PermissionRequest>,
}

pub(super) const DIAGNOSTIC_MAX_MESSAGES: u64 = 25;
pub(super) const DIAGNOSTIC_MAX_TEXT_BYTES: usize = 12 * 1024;
pub(super) const DIAGNOSTIC_MAX_METADATA_BYTES: usize = 8 * 1024;
pub(super) const DIAGNOSTIC_MAX_NATIVE_BYTES: usize = 512 * 1024;

#[derive(Debug, Serialize, ToSchema)]
pub struct ConversationDiagnosticResponse {
    pub schema_version: u8,
    pub conversation: ConversationDiagnosticRuntimeResponse,
    pub source: ConversationDiagnosticSourceResponse,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_session: Option<ConversationNativeDiagnosticResponse>,
    pub messages: Vec<ConversationDiagnosticMessageResponse>,
    pub returned_message_count: usize,
    pub message_limit: u64,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_permission: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ConversationDiagnosticRuntimeResponse {
    pub public_id: String,
    pub context_type: String,
    pub context_id: String,
    pub provider: String,
    pub model: String,
    pub thinking_level: Option<String>,
    pub permission_mode: String,
    pub turn_status: String,
    pub active_turn_id: Option<String>,
    pub last_turn_id: Option<String>,
    pub turn_started_at: Option<String>,
    pub native_session_id: Option<String>,
    pub context_usage: Option<ContextWindowUsageResponse>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ConversationDiagnosticSourceResponse {
    pub normalized_history: bool,
    pub native_session_status: &'static str,
    pub native_session_note: &'static str,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ConversationNativeDiagnosticResponse {
    pub provider: String,
    pub session_id: String,
    pub format: String,
    pub json: serde_json::Value,
    pub truncated: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ConversationDiagnosticMessageResponse {
    pub role: String,
    pub content: String,
    pub metadata: Option<serde_json::Value>,
    pub tokens_in: Option<i32>,
    pub tokens_out: Option<i32>,
    pub cost_microcents: Option<i64>,
    pub created_at: String,
    pub truncated: bool,
}

pub(super) fn bounded_redacted_text(value: &str, max_bytes: usize) -> (String, bool) {
    let redacted = redact_text(value);
    if redacted.len() <= max_bytes {
        return (redacted, false);
    }
    let mut end = max_bytes;
    while !redacted.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    (redacted[..end].to_string(), true)
}

pub(super) fn bounded_redacted_metadata(value: &serde_json::Value) -> (serde_json::Value, bool) {
    let redacted = redact_value(value);
    let encoded = redacted.to_string();
    let (preview, truncated) = bounded_redacted_text(&encoded, DIAGNOSTIC_MAX_METADATA_BYTES);
    if truncated {
        (
            serde_json::json!({ "truncated": true, "preview": preview }),
            true,
        )
    } else {
        (redacted, false)
    }
}

pub(super) fn diagnostic_response_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    headers
}

#[derive(Debug, Default, Deserialize)]
pub struct ConversationMessagesQuery {
    pub before: Option<String>,
    pub limit: Option<u64>,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct ConversationDiagnosticsQuery {
    /// Persisted rows to include, newest window first (default/max 25).
    pub limit: Option<u64>,
}

#[derive(Debug, Default, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConversationListStatus {
    #[default]
    Active,
    Archived,
}

impl ConversationListStatus {
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
        }
    }
}

#[derive(Debug, Default, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConversationListScope {
    #[default]
    All,
    Global,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ConversationListQuery {
    /// Conversation lifecycle state (defaults to active).
    #[serde(default)]
    pub status: ConversationListStatus,
    /// Limit results to the global workspace, or return every readable context.
    #[serde(default)]
    pub scope: ConversationListScope,
    /// Page number (1-indexed).
    #[param(example = 1, minimum = 1)]
    pub page: Option<u64>,
    /// Number of conversations per page (clamped to 1..=100).
    #[param(example = 20, minimum = 1, maximum = 100)]
    pub page_size: Option<u64>,
    /// Keep only chats whose title, or whose project's name or slug, contains
    /// this text (case-insensitive, matched literally). At most 200
    /// characters; blank means no filter.
    #[param(example = "landing page", max_length = 200)]
    pub search: Option<String>,
}

/// Longest `search` accepted by the conversation list.
pub(super) const MAX_CONVERSATION_SEARCH_LEN: usize = 200;

pub(super) fn validate_conversation_search(search: Option<&str>) -> Result<(), Problem> {
    if search.is_some_and(|search| search.chars().count() > MAX_CONVERSATION_SEARCH_LEN) {
        return Err(too_long("search", MAX_CONVERSATION_SEARCH_LEN));
    }
    Ok(())
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LifecycleListQuery {
    /// Resource lifecycle state (defaults to active).
    #[serde(default)]
    pub status: ConversationListStatus,
    /// Page number (1-indexed).
    #[param(example = 1, minimum = 1)]
    pub page: Option<u64>,
    /// Number of applications or conversations per page (clamped to 1..=100).
    #[param(example = 20, minimum = 1, maximum = 100)]
    pub page_size: Option<u64>,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LifecycleStatusQuery {
    /// Resource lifecycle state (defaults to active).
    #[serde(default)]
    pub status: ConversationListStatus,
}

pub(super) fn normalize_list_pagination(page: Option<u64>, page_size: Option<u64>) -> (u64, u64) {
    (
        page.unwrap_or(1).max(1),
        page_size.unwrap_or(20).clamp(1, 100),
    )
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateConversationRequest {
    /// e.g. `"deployment"`.
    pub context_type: String,
    /// The entity id (ints stringified).
    pub context_id: String,
    /// Provider pinned to this conversation. Omitted requests use the current
    /// instance preference.
    pub ai_provider: Option<String>,
    pub ai_model: Option<String>,
    pub ai_thinking_level: Option<String>,
    pub ai_permission_mode: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct FindConversationQuery {
    pub context_type: String,
    pub context_id: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct RenameConversationRequest {
    /// New human-facing title. Trimmed; must be non-empty after trimming.
    pub title: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdatePermissionModeRequest {
    /// Provider permission mode to persist. During a running turn only `Auto`
    /// (`full-access`) can be applied because provider CLI launch flags cannot
    /// be safely reduced after the process has started.
    pub permission_mode: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SendMessageRequest {
    pub content: String,
    /// Opaque references returned by the conversation attachment endpoint.
    #[serde(default)]
    pub attachments: Vec<ChatAttachmentReference>,
    /// Client-generated opaque idempotency key for this turn. Retries with the
    /// same id never create a second user message or harness execution.
    #[serde(default)]
    pub turn_id: Option<String>,
    /// Optional AI gateway provider (`gateway_key:<id>`) for this and later
    /// turns. A chat on an AI gateway provider may move to another one; a
    /// provider harness chat stays on the provider it was created with.
    #[serde(default)]
    pub ai_provider: Option<String>,
    /// Optional next-turn model, one advertised by the conversation's
    /// provider (or by `ai_provider` when that is set).
    #[serde(default)]
    pub ai_model: Option<String>,
    /// Optional next-turn thinking level.
    #[serde(default)]
    pub ai_thinking_level: Option<String>,
    /// Optional next-turn permission mode for the pinned provider harness.
    #[serde(default)]
    pub ai_permission_mode: Option<String>,
    /// Optional, client-supplied description of the page/entity the user is
    /// currently viewing (e.g. a trace in a project). Injected into the model's
    /// view of this turn only — never stored or shown in history. Capped server
    /// side; oversized values are ignored rather than rejected.
    #[serde(default)]
    pub page_context: Option<String>,
}

pub(super) fn validate_send_message_request(req: &SendMessageRequest) -> Result<(), Problem> {
    if req.content.trim().is_empty() && req.attachments.is_empty() {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Empty Message")
            .with_detail("A message must contain text or at least one attachment."));
    }
    if req.content.len() > MAX_MESSAGE_CONTENT_LEN {
        return Err(too_long("content", MAX_MESSAGE_CONTENT_LEN));
    }
    if req
        .turn_id
        .as_ref()
        .is_some_and(|turn_id| turn_id.trim().is_empty() || turn_id.len() > MAX_TURN_ID_LEN)
    {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Turn ID")
            .with_detail("turn_id must be a non-empty opaque id of at most 128 characters."));
    }
    if req.attachments.len() > MAX_CHAT_ATTACHMENTS {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Too Many Attachments")
            .with_detail(format!(
                "A message may include at most {MAX_CHAT_ATTACHMENTS} attachments."
            )));
    }
    Ok(())
}

/// Acknowledgement that a durable, server-owned turn has started. Live token,
/// tool, permission, error, and completion events are delivered exclusively by
/// the conversation WebSocket.
#[derive(Debug, Serialize, ToSchema)]
pub struct SendMessageAcceptedResponse {
    pub turn_id: String,
    pub status: String,
    /// Server timestamp used by every observer to render one continuous
    /// elapsed-time counter across refreshes and reconnects.
    pub turn_started_at: String,
}

/// Payload for the `tool_call` SSE event: the model is about to run a tool.
/// Serialized as compact single-line JSON onto one `data:` line.
#[derive(Debug, Serialize, ToSchema)]
pub struct ToolCallEvent {
    pub id: String,
    pub name: String,
    /// The raw JSON-args string the model emitted.
    pub arguments: String,
}

/// Payload for the `tool_result` SSE event: a tool finished running. Serialized
/// as compact single-line JSON; `content` is JSON-string-escaped so it stays on
/// one `data:` line even when long.
#[derive(Debug, Serialize, ToSchema)]
pub struct ToolResultEvent {
    pub id: String,
    pub name: String,
    pub content: String,
}

/// Payload for the `permission_requested` SSE event (ADR-038 Phase 2).
/// The active provider turn is paused waiting for the user to approve or deny
/// a tool/question/plan. Resolve via
/// `POST .../permissions/{id}/resolve`.
#[derive(Debug, Serialize, ToSchema)]
pub struct PermissionRequestedEvent {
    /// The CLI's `request_id` — also the `{permission_id}` in the resolve URL.
    pub id: String,
    /// What kind of interaction is required: `"tool_approval"`, `"question"`,
    /// or `"plan_approval"`.
    pub kind: PermissionKind,
    /// Tool name from the CLI request (e.g. `"Bash"`, `"AskUserQuestion"`).
    pub tool_name: String,
    /// Raw `input` from the CLI request. Passed through verbatim so each
    /// milestone's card can render tool-specific fields without the service
    /// layer needing to know about their schemas.
    pub input: serde_json::Value,
}

/// Body for the `POST .../permissions/{permission_id}/resolve` endpoint.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ResolvePermissionRequest {
    pub decision: PermissionDecision,
}
