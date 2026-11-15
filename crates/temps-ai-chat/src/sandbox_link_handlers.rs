// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Endpoints behind the assistant's sandbox control: see whether a chat can use
//! a sandbox, link one the user owns or create one, and unlink it again.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::permissions::Permission;
use temps_auth::{
    deny_deployment_token, permission_guard, project_access_guard, project_scope_guard, RequireAuth,
};
use temps_core::{
    problemdetails::{self, Problem, ProblemDetails},
    AuditContext, RequestMetadata,
};
use utoipa::{OpenApi, ToSchema};

use temps_entities::ai_conversations;

use crate::audit::{
    AiSandboxLinkedAudit, AiSandboxServicesConnectedAudit, AiSandboxServicesDisconnectedAudit,
    AiSandboxUnlinkedAudit, AiSandboxWokenAudit,
};
use crate::handlers::{ensure_user_conversation_access, AppState};
use crate::sandbox_link::{
    context_supports_sandbox, link_from_metadata, preview_port_from_metadata,
    project_for_new_sandbox, readiness, ConnectedService, SandboxConnections, SandboxLink,
    SandboxLinkError, SandboxLinkService, SandboxReadiness,
};

impl From<SandboxLinkError> for Problem {
    fn from(error: SandboxLinkError) -> Self {
        match error {
            SandboxLinkError::SandboxNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Sandbox Not Found")
                .with_detail(error.to_string()),
            SandboxLinkError::Validation { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Sandbox Link")
                .with_detail(error.to_string()),
            SandboxLinkError::NotLinkable { .. } | SandboxLinkError::NotLinked { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Sandbox Cannot Be Linked")
                    .with_detail(error.to_string())
            }
            SandboxLinkError::Unavailable { .. } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("Sandboxes Unavailable")
                    .with_detail(error.to_string())
            }
            SandboxLinkError::Sandbox { .. } | SandboxLinkError::Database { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

/// What a chat can do with a sandbox, and the one it is linked to.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ChatSandboxStatus {
    /// Whether this chat can use a sandbox at all.
    pub configured: bool,
    /// Why it cannot, when `configured` is false.
    pub reason: Option<String>,
    /// Console path where the user can fix it, when there is one.
    pub setup_path: Option<String>,
    /// The sandbox currently linked to the chat.
    pub linked: Option<SandboxLink>,
    /// True once the assistant has asked the user to link a sandbox.
    pub offered: bool,
    /// Where the linked sandbox's apps are served, for the Preview tab.
    pub preview: Option<ChatPreview>,
    /// Whether the linked sandbox's compute is up. `None` when nothing is
    /// linked or the sandbox could not be read.
    pub runtime: Option<ChatSandboxRuntime>,
    /// Projects whose services the linked sandbox is connected to: its
    /// commands run with credentials for a database of its own on each.
    pub connected_projects: Vec<i32>,
    /// Every service the linked sandbox is connected to, whether or not it was
    /// connected through a project.
    pub connected_services: Vec<ConnectedService>,
}

/// Whether a linked sandbox's compute is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChatSandboxRuntime {
    /// Up: commands and the preview work.
    Running,
    /// Stopped after sitting idle. Its files are intact; waking it starts the
    /// container again, which `POST .../sandbox/wake` does.
    Asleep,
}

impl ChatSandboxRuntime {
    fn of_status(status: &str) -> Self {
        match readiness(status) {
            SandboxReadiness::Ready => Self::Running,
            SandboxReadiness::Asleep => Self::Asleep,
        }
    }
}

/// Connect the chat's sandbox to one service.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ConnectServiceRequest {
    /// The service the sandbox may use.
    pub service_id: i32,
    /// A project the service is linked to. Names the sandbox's database or
    /// bucket after the project; without it the name comes from the sandbox.
    pub project_id: Option<i32>,
}

/// Connect the chat's sandbox to the services of a project.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ConnectProjectRequest {
    /// The project whose services the sandbox may use.
    pub project_id: i32,
}

/// How to reach a page served from the chat's sandbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ChatPreview {
    /// The port the assistant last confirmed serving a page, if it has.
    pub port: Option<u16>,
    /// Address of that page, served by Temps.
    pub url: Option<String>,
    /// Address for any port: `{port}` stands for the port number.
    pub url_template: String,
}

/// The preview of `link`, with the port the assistant last confirmed.
fn preview_of(
    parts: &temps_sandbox::services::PreviewUrlParts,
    link: &SandboxLink,
    metadata: Option<&serde_json::Value>,
) -> ChatPreview {
    let port = preview_port_from_metadata(metadata);
    ChatPreview {
        port,
        url: port.map(|port| parts.url_for(&link.sandbox_id, port)),
        url_template: parts.host_template(&link.sandbox_id),
    }
}

/// Link an existing sandbox (`sandbox_id`) or create a new one (`create`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct LinkSandboxRequest {
    /// Public id of one of the caller's own sandboxes.
    pub sandbox_id: Option<String>,
    /// Create a fresh sandbox for this chat instead.
    #[serde(default)]
    pub create: bool,
}

struct Availability {
    configured: bool,
    reason: Option<String>,
}

fn availability(
    state: &AppState,
    auth: &temps_auth::context::AuthContext,
    context_type: &str,
) -> Availability {
    if !context_supports_sandbox(context_type) {
        return Availability {
            configured: false,
            reason: Some(
                "Sandboxes can be linked to Temps assistant chats and project chats.".to_string(),
            ),
        };
    }
    if state.application_sandboxes.is_none() {
        return Availability {
            configured: false,
            reason: Some("This server has no sandbox backend.".to_string()),
        };
    }
    if !auth.has_permission(&Permission::SandboxesExec) {
        return Availability {
            configured: false,
            reason: Some(format!(
                "Your role cannot run commands in sandboxes ({}).",
                Permission::SandboxesExec
            )),
        };
    }
    Availability {
        configured: true,
        reason: None,
    }
}

fn status_of(
    availability: Availability,
    metadata: Option<&serde_json::Value>,
    connected: &[ConnectedService],
) -> ChatSandboxStatus {
    ChatSandboxStatus {
        configured: availability.configured,
        reason: availability.reason,
        setup_path: None,
        linked: link_from_metadata(metadata),
        offered: crate::sandbox_link::offered_in_metadata(metadata),
        preview: None,
        runtime: None,
        connected_projects: project_ids_of(connected),
        connected_services: connected.to_vec(),
    }
}

fn project_ids_of(connected: &[ConnectedService]) -> Vec<i32> {
    let mut projects: Vec<i32> = connected.iter().filter_map(|c| c.project_id).collect();
    projects.sort_unstable();
    projects.dedup();
    projects
}

pub(crate) fn link_service(
    state: &AppState,
    conversation_id: &str,
) -> Result<SandboxLinkService, Problem> {
    let sandboxes =
        state
            .application_sandboxes
            .clone()
            .ok_or_else(|| SandboxLinkError::Unavailable {
                conversation_id: conversation_id.to_string(),
            })?;
    Ok(SandboxLinkService::new(state.db.clone(), sandboxes))
}

/// The chat's sandbox status, read fresh: what it can use, what is linked, how
/// to reach its apps, and whether its compute is up.
async fn sandbox_status(
    state: &AppState,
    auth: &temps_auth::context::AuthContext,
    conversation: &ai_conversations::Model,
) -> ChatSandboxStatus {
    let linked = link_from_metadata(conversation.metadata.as_ref());
    let connected = match (&linked, &state.application_sandboxes) {
        (Some(link), Some(sandboxes)) => sandboxes
            .linked_services(auth.user_id(), &link.sandbox_id)
            .await
            .map(|links| links.iter().map(ConnectedService::from).collect())
            .unwrap_or_else(|error| {
                tracing::warn!(
                    conversation_id = %conversation.public_id,
                    sandbox_id = %link.sandbox_id,
                    "could not read the sandbox's service links: {error}"
                );
                Vec::new()
            }),
        _ => Vec::new(),
    };
    let mut status = status_of(
        availability(state, auth, &conversation.context_type),
        conversation.metadata.as_ref(),
        &connected,
    );
    if let (Some(link), Some(sandboxes)) = (&status.linked, &state.application_sandboxes) {
        let parts = sandboxes.preview_parts().await;
        status.preview = Some(preview_of(&parts, link, conversation.metadata.as_ref()));
        status.runtime = sandboxes
            .find_by_public_id(&link.sandbox_id, auth.user_id())
            .await
            .ok()
            .map(|row| ChatSandboxRuntime::of_status(&row.status));
    }
    status
}

/// Whether the chat can use a sandbox, and which one it is linked to.
#[utoipa::path(
    get, tag = "AI Chat", path = "/ai/conversations/{public_id}/sandbox",
    params(("public_id" = String, Path, description = "Conversation public id")),
    responses(
        (status = 200, description = "Sandbox status of the conversation", body = ChatSandboxStatus),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_chat_sandbox(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(public_id): Path<String>,
) -> Result<Json<ChatSandboxStatus>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    Ok(Json(sandbox_status(&state, &auth, &conversation).await))
}

/// Link a sandbox to the chat, or create one for it. Replaces any earlier link.
#[utoipa::path(
    post, tag = "AI Chat", path = "/ai/conversations/{public_id}/sandbox",
    params(("public_id" = String, Path, description = "Conversation public id")),
    request_body = LinkSandboxRequest,
    responses(
        (status = 200, description = "Sandbox linked", body = ChatSandboxStatus),
        (status = 400, description = "Neither or both of sandbox_id and create were sent", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation or sandbox not found", body = ProblemDetails),
        (status = 409, description = "The sandbox cannot be linked", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
        (status = 503, description = "This server has no sandbox backend", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn link_chat_sandbox(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(public_id): Path<String>,
    Json(request): Json<LinkSandboxRequest>,
) -> Result<Json<ChatSandboxStatus>, Problem> {
    permission_guard!(auth, ProjectsRead);
    permission_guard!(auth, SandboxesExec);
    deny_deployment_token!(auth);
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    if !context_supports_sandbox(&conversation.context_type) {
        return Err(SandboxLinkError::Validation {
            conversation_id: conversation.public_id.clone(),
            message: format!(
                "sandboxes can be linked to assistant and project chats only, not '{}' chats",
                conversation.context_type
            ),
        }
        .into());
    }
    let service = link_service(&state, &conversation.public_id)?;
    let sandbox_id = request
        .sandbox_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty());
    let link = match (sandbox_id, request.create) {
        (Some(sandbox_id), false) => {
            service
                .link_existing(&conversation, auth.user_id(), sandbox_id)
                .await?
        }
        (None, true) => {
            permission_guard!(auth, SandboxesWrite);
            // A sandbox made for a project chat starts as a clone of the
            // project's repository, which reads its stored Git credential: the
            // same gates as creating a sandbox from a project directly.
            if let Some(project_id) = project_for_new_sandbox(&conversation) {
                permission_guard!(auth, GitRepositoriesRead);
                project_scope_guard!(auth, project_id);
                project_access_guard!(auth, project_id, state.project_access_checker);
            }
            service
                .create_and_link(&conversation, auth.user_id(), None)
                .await?
        }
        _ => {
            return Err(SandboxLinkError::Validation {
                conversation_id: conversation.public_id.clone(),
                message: "send either `sandbox_id` to link one of your sandboxes or `create: true` to make a new one".to_string(),
            }
            .into())
        }
    };
    state
        .audit(&AiSandboxLinkedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: conversation.project_id,
            conversation_id: conversation.public_id.clone(),
            sandbox_id: link.sandbox_id.clone(),
            created: request.create,
        })
        .await;
    Ok(Json(ChatSandboxStatus {
        configured: true,
        reason: None,
        setup_path: None,
        linked: Some(link),
        offered: true,
        preview: None,
        runtime: None,
        connected_projects: Vec::new(),
        connected_services: Vec::new(),
    }))
}

/// Start the chat's sandbox if it went to sleep, so the preview and the
/// assistant's next command find it up. Does nothing when it is already
/// running.
#[utoipa::path(
    post, tag = "AI Chat", path = "/ai/conversations/{public_id}/sandbox/wake",
    params(("public_id" = String, Path, description = "Conversation public id")),
    responses(
        (status = 200, description = "Sandbox running", body = ChatSandboxStatus),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation or sandbox not found", body = ProblemDetails),
        (status = 409, description = "The chat has no linked sandbox", body = ProblemDetails),
        (status = 500, description = "The sandbox could not be started", body = ProblemDetails),
        (status = 503, description = "This server has no sandbox backend", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn wake_chat_sandbox(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(public_id): Path<String>,
) -> Result<Json<ChatSandboxStatus>, Problem> {
    permission_guard!(auth, ProjectsRead);
    permission_guard!(auth, SandboxesExec);
    deny_deployment_token!(auth);
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    let service = link_service(&state, &conversation.public_id)?;
    let woken = service.wake_linked(&conversation, auth.user_id()).await?;
    if woken {
        if let Some(link) = link_from_metadata(conversation.metadata.as_ref()) {
            state
                .audit(&AiSandboxWokenAudit {
                    context: AuditContext {
                        user_id: auth.user_id(),
                        ip_address: Some(metadata.ip_address.clone()),
                        user_agent: metadata.user_agent.clone(),
                    },
                    project_id: conversation.project_id,
                    conversation_id: conversation.public_id.clone(),
                    sandbox_id: link.sandbox_id,
                })
                .await;
        }
    }
    Ok(Json(sandbox_status(&state, &auth, &conversation).await))
}

/// Unlink the chat's sandbox. The sandbox itself is left running.
#[utoipa::path(
    delete, tag = "AI Chat", path = "/ai/conversations/{public_id}/sandbox",
    params(("public_id" = String, Path, description = "Conversation public id")),
    responses(
        (status = 200, description = "Sandbox unlinked", body = ChatSandboxStatus),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
        (status = 503, description = "This server has no sandbox backend", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn unlink_chat_sandbox(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(public_id): Path<String>,
) -> Result<Json<ChatSandboxStatus>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    let previous = link_from_metadata(conversation.metadata.as_ref());
    let service = link_service(&state, &conversation.public_id)?;
    service.unlink(&conversation).await?;
    state
        .audit(&AiSandboxUnlinkedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: conversation.project_id,
            conversation_id: conversation.public_id.clone(),
            sandbox_id: previous.map(|link| link.sandbox_id),
        })
        .await;
    Ok(Json(ChatSandboxStatus {
        configured: true,
        reason: None,
        setup_path: None,
        linked: None,
        offered: true,
        preview: None,
        runtime: None,
        connected_projects: Vec::new(),
        connected_services: Vec::new(),
    }))
}

/// Guards shared by connecting and disconnecting: handing a sandbox database
/// credentials is the same act as issuing a deployment its runtime variables.
macro_rules! connection_guards {
    ($auth:expr, $state:expr, $project_id:expr) => {
        permission_guard!($auth, ProjectsRead);
        permission_guard!($auth, SandboxesExec);
        permission_guard!($auth, ExternalServicesWrite);
        permission_guard!($auth, SecretsRead);
        deny_deployment_token!($auth);
        project_scope_guard!($auth, $project_id);
        project_access_guard!($auth, $project_id, $state.project_access_checker);
    };
}

/// Like `connection_guards!` for a service that may or may not be reached
/// through a project: the project checks apply only when one is named.
macro_rules! service_connection_guards {
    ($auth:expr, $state:expr, $project_id:expr) => {
        permission_guard!($auth, ProjectsRead);
        permission_guard!($auth, SandboxesExec);
        permission_guard!($auth, ExternalServicesWrite);
        permission_guard!($auth, SecretsRead);
        deny_deployment_token!($auth);
        if let Some(project_id) = $project_id {
            project_scope_guard!($auth, project_id);
            project_access_guard!($auth, project_id, $state.project_access_checker);
        }
    };
}

/// Let the chat's sandbox use a project's database.
///
/// The sandbox gets a database of its own on each PostgreSQL service linked to
/// the project, and a role that can use that database and no other. Commands
/// the assistant runs there receive the credentials as the same `POSTGRES_*`
/// variables a deployment gets; the values are never shown to the model.
#[utoipa::path(
    post, tag = "AI Chat", path = "/ai/conversations/{public_id}/sandbox/projects",
    params(("public_id" = String, Path, description = "Conversation public id")),
    request_body = ConnectProjectRequest,
    responses(
        (status = 200, description = "Sandbox connected", body = SandboxConnections),
        (status = 400, description = "The project has no service that can be connected", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation or sandbox not found", body = ProblemDetails),
        (status = 409, description = "The chat has no linked sandbox", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
        (status = 503, description = "This server has no sandbox backend", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn connect_chat_sandbox_project(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(public_id): Path<String>,
    Json(request): Json<ConnectProjectRequest>,
) -> Result<Json<SandboxConnections>, Problem> {
    connection_guards!(auth, state, request.project_id);
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    let service = link_service(&state, &conversation.public_id)?;
    let connections = service
        .connect_project(&conversation, auth.user_id(), request.project_id)
        .await?;
    let sandbox_id = link_from_metadata(conversation.metadata.as_ref())
        .map(|link| link.sandbox_id)
        .unwrap_or_default();
    state
        .audit(&AiSandboxServicesConnectedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: Some(request.project_id),
            conversation_id: conversation.public_id.clone(),
            sandbox_id,
            connected_service_ids: connections.connected_service_ids.clone(),
            variable_names: connections.variable_names.clone(),
        })
        .await;
    Ok(Json(connections))
}

/// Stop the chat's sandbox using a project's database.
#[utoipa::path(
    delete, tag = "AI Chat", path = "/ai/conversations/{public_id}/sandbox/projects/{project_id}",
    params(
        ("public_id" = String, Path, description = "Conversation public id"),
        ("project_id" = i32, Path, description = "Project to disconnect")
    ),
    responses(
        (status = 200, description = "Sandbox disconnected", body = SandboxConnections),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation or sandbox not found", body = ProblemDetails),
        (status = 409, description = "The chat has no linked sandbox", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
        (status = 503, description = "This server has no sandbox backend", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn disconnect_chat_sandbox_project(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((public_id, project_id)): Path<(String, i32)>,
) -> Result<Json<SandboxConnections>, Problem> {
    connection_guards!(auth, state, project_id);
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    let service = link_service(&state, &conversation.public_id)?;
    let connections = service
        .disconnect_project(&conversation, auth.user_id(), project_id)
        .await?;
    let sandbox_id = link_from_metadata(conversation.metadata.as_ref())
        .map(|link| link.sandbox_id)
        .unwrap_or_default();
    state
        .audit(&AiSandboxServicesDisconnectedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: Some(project_id),
            service_id: None,
            conversation_id: conversation.public_id.clone(),
            sandbox_id,
        })
        .await;
    Ok(Json(connections))
}

/// Let the chat's sandbox use one service, whether or not it belongs to a
/// project. The service must be one the sandbox can get credentials of its own
/// on; the variables commands receive are the same a deployment gets.
#[utoipa::path(
    post, tag = "AI Chat", path = "/ai/conversations/{public_id}/sandbox/services",
    params(("public_id" = String, Path, description = "Conversation public id")),
    request_body = ConnectServiceRequest,
    responses(
        (status = 200, description = "Sandbox connected", body = SandboxConnections),
        (status = 400, description = "The service cannot give a sandbox credentials", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation or sandbox not found", body = ProblemDetails),
        (status = 409, description = "The chat has no linked sandbox", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
        (status = 503, description = "This server has no sandbox backend", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn connect_chat_sandbox_service(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(public_id): Path<String>,
    Json(request): Json<ConnectServiceRequest>,
) -> Result<Json<SandboxConnections>, Problem> {
    service_connection_guards!(auth, state, request.project_id);
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    let service = link_service(&state, &conversation.public_id)?;
    let connections = service
        .connect_service(
            &conversation,
            auth.user_id(),
            request.service_id,
            request.project_id,
        )
        .await?;
    let sandbox_id = link_from_metadata(conversation.metadata.as_ref())
        .map(|link| link.sandbox_id)
        .unwrap_or_default();
    state
        .audit(&AiSandboxServicesConnectedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: request.project_id,
            conversation_id: conversation.public_id.clone(),
            sandbox_id,
            connected_service_ids: connections.connected_service_ids.clone(),
            variable_names: connections.variable_names.clone(),
        })
        .await;
    Ok(Json(connections))
}

/// Stop the chat's sandbox using one service. Its data is left as it is.
#[utoipa::path(
    delete, tag = "AI Chat", path = "/ai/conversations/{public_id}/sandbox/services/{service_id}",
    params(
        ("public_id" = String, Path, description = "Conversation public id"),
        ("service_id" = i32, Path, description = "Service to disconnect")
    ),
    responses(
        (status = 200, description = "Sandbox disconnected", body = SandboxConnections),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation or sandbox not found", body = ProblemDetails),
        (status = 409, description = "The chat has no linked sandbox", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
        (status = 503, description = "This server has no sandbox backend", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn disconnect_chat_sandbox_service(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((public_id, service_id)): Path<(String, i32)>,
) -> Result<Json<SandboxConnections>, Problem> {
    service_connection_guards!(auth, state, None::<i32>);
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    let service = link_service(&state, &conversation.public_id)?;
    let connections = service
        .disconnect_service(&conversation, auth.user_id(), service_id)
        .await?;
    let sandbox_id = link_from_metadata(conversation.metadata.as_ref())
        .map(|link| link.sandbox_id)
        .unwrap_or_default();
    state
        .audit(&AiSandboxServicesDisconnectedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: None,
            service_id: Some(service_id),
            conversation_id: conversation.public_id.clone(),
            sandbox_id,
        })
        .await;
    Ok(Json(connections))
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/ai/conversations/{public_id}/sandbox",
            get(get_chat_sandbox)
                .post(link_chat_sandbox)
                .delete(unlink_chat_sandbox),
        )
        .route(
            "/ai/conversations/{public_id}/sandbox/wake",
            axum::routing::post(wake_chat_sandbox),
        )
        .route(
            "/ai/conversations/{public_id}/sandbox/projects",
            axum::routing::post(connect_chat_sandbox_project),
        )
        .route(
            "/ai/conversations/{public_id}/sandbox/projects/{project_id}",
            delete(disconnect_chat_sandbox_project),
        )
        .route(
            "/ai/conversations/{public_id}/sandbox/services",
            axum::routing::post(connect_chat_sandbox_service),
        )
        .route(
            "/ai/conversations/{public_id}/sandbox/services/{service_id}",
            delete(disconnect_chat_sandbox_service),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_chat_sandbox,
        link_chat_sandbox,
        unlink_chat_sandbox,
        wake_chat_sandbox,
        connect_chat_sandbox_project,
        disconnect_chat_sandbox_project,
        connect_chat_sandbox_service,
        disconnect_chat_sandbox_service
    ),
    components(schemas(
        ChatSandboxStatus,
        ChatSandboxRuntime,
        ChatPreview,
        LinkSandboxRequest,
        SandboxLink,
        ConnectProjectRequest,
        ConnectServiceRequest,
        ConnectedService,
        SandboxConnections
    ))
)]
pub struct SandboxLinkApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn link_errors_map_to_the_status_a_client_can_act_on() {
        let status = |error: SandboxLinkError| Problem::from(error).status_code;
        assert_eq!(
            status(SandboxLinkError::SandboxNotFound {
                sandbox_id: "sbx_1".into(),
                user_id: 3
            }),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status(SandboxLinkError::Validation {
                conversation_id: "conv_1".into(),
                message: "bad".into()
            }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status(SandboxLinkError::NotLinkable {
                sandbox_id: "sbx_1".into(),
                conversation_id: "conv_1".into(),
                reason: "it has been destroyed".into()
            }),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(SandboxLinkError::Unavailable {
                conversation_id: "conv_1".into()
            }),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[test]
    fn only_a_stopped_sandbox_reports_as_asleep() {
        assert_eq!(
            ChatSandboxRuntime::of_status("stopped"),
            ChatSandboxRuntime::Asleep
        );
        assert_eq!(
            ChatSandboxRuntime::of_status("running"),
            ChatSandboxRuntime::Running
        );
        assert_eq!(
            serde_json::to_value(ChatSandboxRuntime::Asleep).ok(),
            Some(serde_json::json!("asleep"))
        );
    }

    #[test]
    fn the_preview_names_the_confirmed_port_and_offers_a_template_for_any_other() {
        let parts = temps_sandbox::services::PreviewUrlParts {
            protocol: "http".into(),
            domain: "example.test".into(),
            port: Some(8080),
        };
        let link = SandboxLink {
            sandbox_id: "sbx_deadbeef00001122".into(),
            name: "scratch".into(),
            linked_at: chrono::Utc::now(),
        };
        let confirmed = preview_of(
            &parts,
            &link,
            Some(&json!({ "sandbox_preview_port": 3000 })),
        );
        assert_eq!(confirmed.port, Some(3000));
        assert_eq!(
            confirmed.url.as_deref(),
            Some("http://ws-deadbeef00001122-3000.example.test:8080")
        );
        assert_eq!(
            confirmed.url_template,
            "http://ws-deadbeef00001122-{port}.example.test:8080"
        );

        let none = preview_of(&parts, &link, None);
        assert!(none.port.is_none() && none.url.is_none());
        assert!(none.url_template.contains("{port}"));
    }

    #[test]
    fn status_carries_the_link_and_the_offer_flag_from_metadata() {
        let metadata = json!({
            "sandbox_link": {
                "sandbox_id": "sbx_abc",
                "name": "scratch",
                "linked_at": "2026-10-17T09:30:00Z"
            },
            "sandbox_offered_at": "2026-10-17T09:00:00Z"
        });
        let connected = [
            ConnectedService {
                service_id: 7,
                project_id: Some(12),
            },
            ConnectedService {
                service_id: 8,
                project_id: Some(5),
            },
            ConnectedService {
                service_id: 9,
                project_id: Some(12),
            },
            ConnectedService {
                service_id: 10,
                project_id: None,
            },
        ];
        let status = status_of(
            Availability {
                configured: true,
                reason: None,
            },
            Some(&metadata),
            &connected,
        );
        assert_eq!(status.linked.map(|l| l.sandbox_id), Some("sbx_abc".into()));
        assert!(status.offered);
        assert_eq!(status.connected_projects, vec![5, 12]);
        assert_eq!(status.connected_services.len(), 4);

        let empty = status_of(
            Availability {
                configured: false,
                reason: Some("This server has no sandbox backend.".into()),
            },
            None,
            &[],
        );
        assert!(empty.linked.is_none() && !empty.offered && !empty.configured);
        assert!(empty.connected_projects.is_empty());
        assert!(empty.reason.is_some());
    }
}
