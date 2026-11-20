// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Routes for workspace chats and the architecture blueprint (ADR-053).
//!
//! A chat is put in a workspace, moved or taken out under
//! `/ai/conversations/{public_id}/workspace`. A workspace's blueprint is read,
//! decided, edited and bound under `/ai/applications/{public_id}/blueprint`.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post, put},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{deny_deployment_token, permission_guard, RequireAuth};
use temps_auth::{permissions::Permission, AuthContext};
use temps_core::problemdetails::{self, Problem, ProblemDetails};
use temps_core::{AuditContext, AuditOperation, RequestMetadata};
use utoipa::{IntoParams, OpenApi, ToSchema};

use crate::audit::{
    AiBlueprintBindingChangedAudit, AiBlueprintVersionCreatedAudit, AiBlueprintVersionDecidedAudit,
    AiChatWorkspaceJoinedAudit, AiChatWorkspaceLeftAudit,
};
use crate::blueprint::{
    AppRole, AppRuntime, BlueprintDocument, BlueprintError, BlueprintVersionResponse,
    ClientSurface, Component, ComponentBinding, ComponentKind, ComponentLive, DataEngine, Decision,
    Flow, FlowKind, LiveStatus, VersionStatus,
};
use crate::handlers::{
    authorized_application_for_permissions, ensure_user_conversation_access, AppState,
};
use crate::workspace_chats::{
    JoinTarget, KeepSandbox, WorkspaceChatError, WorkspaceChatService, WorkspaceChatSummary,
};

impl From<WorkspaceChatError> for Problem {
    fn from(error: WorkspaceChatError) -> Self {
        match error {
            WorkspaceChatError::NotAssistantChat { .. } | WorkspaceChatError::Validation { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Workspace Request")
                    .with_detail(error.to_string())
            }
            WorkspaceChatError::WorkspaceNotFound { .. }
            | WorkspaceChatError::NoWorkspace { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Workspace Not Found")
                .with_detail(error.to_string()),
            WorkspaceChatError::Application { source, .. } => Problem::from(source),
            WorkspaceChatError::Blueprint(source) => Problem::from(source),
            WorkspaceChatError::Database { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

impl From<BlueprintError> for Problem {
    fn from(error: BlueprintError) -> Self {
        match error {
            BlueprintError::Invalid { ref problems, .. } => {
                let problems = problems.clone();
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Blueprint")
                    .with_detail(error.to_string())
                    .with_value("problems", problems)
            }
            BlueprintError::VersionNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Blueprint Version Not Found")
                .with_detail(error.to_string()),
            BlueprintError::UnknownComponent { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Blueprint Part Not Found")
                .with_detail(error.to_string()),
            BlueprintError::NotOpen { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Blueprint Version Already Decided")
                .with_detail(error.to_string()),
            BlueprintError::Corrupt { .. } | BlueprintError::Database { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

fn workspaces(state: &AppState) -> Result<&WorkspaceChatService, Problem> {
    state.service.workspace_chats().ok_or_else(|| {
        problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
            .with_title("Workspaces Unavailable")
            .with_detail("Workspaces are not enabled on this server")
    })
}

fn audit_context(auth: &AuthContext, metadata: &RequestMetadata) -> AuditContext {
    AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

async fn audit(state: &AppState, entry: &dyn AuditOperation) {
    if let Err(error) = state.audit_service.create_audit_log(entry).await {
        tracing::error!(
            "Failed to create audit log for {}: {error}",
            entry.operation_type()
        );
    }
}

/// The workspace a chat is in.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ChatWorkspace {
    /// Public id of the workspace (`app_…`).
    pub public_id: String,
    pub name: String,
    pub description: Option<String>,
}

/// A chat's workspace, and the chats in it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ChatWorkspaceResponse {
    /// `None` when the chat is not in a workspace.
    pub workspace: Option<ChatWorkspace>,
    /// The workspace's chats, most recently active first, at most 20.
    pub chats: Vec<WorkspaceChatSummary>,
    /// Whether this chat can be put in a workspace. Chats about one project
    /// cannot: the project is their scope.
    pub joinable: bool,
    /// Why the chat cannot be in a workspace, when `joinable` is false.
    pub reason: Option<String>,
}

/// A new workspace to create for the chat.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct NewChatWorkspace {
    pub name: String,
    pub description: Option<String>,
}

/// Put a chat in a workspace: an existing one (`application_id`) or a new
/// one (`create`). Exactly one of the two.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SetChatWorkspaceRequest {
    pub application_id: Option<String>,
    pub create: Option<NewChatWorkspace>,
    /// Which sandbox the chats keep when both the chat and the workspace
    /// have one. Default `workspace`.
    #[serde(default)]
    pub keep_sandbox: KeepSandbox,
}

async fn workspace_response(
    service: &WorkspaceChatService,
    conversation: &temps_entities::ai_conversations::Model,
) -> Result<ChatWorkspaceResponse, Problem> {
    let joinable = WorkspaceChatService::is_joinable(conversation);
    let reason = (!joinable).then(|| {
        "This chat is about one project, so it works on that project alone. \
         Start a chat from the assistant to design an application with several parts."
            .to_string()
    });
    Ok(match service.summary(conversation).await? {
        Some(summary) => ChatWorkspaceResponse {
            workspace: Some(ChatWorkspace {
                public_id: summary.application.public_id,
                name: summary.application.name,
                description: summary.application.description,
            }),
            chats: summary.chats,
            joinable,
            reason,
        },
        None => ChatWorkspaceResponse {
            workspace: None,
            chats: Vec::new(),
            joinable,
            reason,
        },
    })
}

async fn owned_conversation(
    state: &AppState,
    auth: &AuthContext,
    public_id: &str,
) -> Result<temps_entities::ai_conversations::Model, Problem> {
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), public_id)
        .await?;
    ensure_user_conversation_access(state, auth, &conversation).await?;
    Ok(conversation)
}

/// The workspace this chat is in, with its other chats.
#[utoipa::path(
    get, tag = "AI Chat", path = "/ai/conversations/{public_id}/workspace",
    params(("public_id" = String, Path, description = "Conversation public id")),
    responses(
        (status = 200, description = "The chat's workspace", body = ChatWorkspaceResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_chat_workspace(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(public_id): Path<String>,
) -> Result<Json<ChatWorkspaceResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let conversation = owned_conversation(&state, &auth, &public_id).await?;
    let service = workspaces(&state)?;
    Ok(Json(workspace_response(service, &conversation).await?))
}

/// Put this chat in a workspace, creating it when asked. The chats of a
/// workspace share its sandbox, projects and architecture blueprint.
#[utoipa::path(
    put, tag = "AI Chat", path = "/ai/conversations/{public_id}/workspace",
    params(("public_id" = String, Path, description = "Conversation public id")),
    request_body = SetChatWorkspaceRequest,
    responses(
        (status = 200, description = "The chat's workspace", body = ChatWorkspaceResponse),
        (status = 400, description = "Invalid request", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation or workspace not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn set_chat_workspace(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(public_id): Path<String>,
    Json(request): Json<SetChatWorkspaceRequest>,
) -> Result<Json<ChatWorkspaceResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    deny_deployment_token!(auth);
    let conversation = owned_conversation(&state, &auth, &public_id).await?;
    let service = workspaces(&state)?;
    let target = match (request.application_id, request.create) {
        (Some(application_id), None) => {
            // Joining reads the workspace's projects and blueprint.
            authorized_application_for_permissions(
                &state,
                &auth,
                &application_id,
                Some("active"),
                &[Permission::ProjectsRead],
            )
            .await?;
            JoinTarget::Existing(application_id)
        }
        (None, Some(create)) => {
            permission_guard!(auth, SandboxesWrite);
            JoinTarget::Create {
                name: create.name,
                description: create.description,
            }
        }
        _ => {
            return Err(problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Workspace Request")
                .with_detail(format!(
                    "Pass exactly one of `application_id` or `create` to put chat {public_id} in a workspace"
                )))
        }
    };
    let (application, created) = service
        .join(&conversation, auth.user_id(), target, request.keep_sandbox)
        .await?;
    audit(
        &state,
        &AiChatWorkspaceJoinedAudit {
            context: audit_context(&auth, &metadata),
            conversation_id: conversation.public_id.clone(),
            application_id: application.public_id.clone(),
            created,
        },
    )
    .await;
    let conversation = owned_conversation(&state, &auth, &public_id).await?;
    Ok(Json(workspace_response(service, &conversation).await?))
}

/// Take this chat out of its workspace. It stops using the workspace's
/// sandbox; the workspace and its other chats are unchanged.
#[utoipa::path(
    delete, tag = "AI Chat", path = "/ai/conversations/{public_id}/workspace",
    params(("public_id" = String, Path, description = "Conversation public id")),
    responses(
        (status = 200, description = "The chat is no longer in a workspace", body = ChatWorkspaceResponse),
        (status = 400, description = "Not an assistant chat", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation not found or not in a workspace", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn leave_chat_workspace(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(public_id): Path<String>,
) -> Result<Json<ChatWorkspaceResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    deny_deployment_token!(auth);
    let conversation = owned_conversation(&state, &auth, &public_id).await?;
    let service = workspaces(&state)?;
    let application = service.leave(&conversation).await?;
    audit(
        &state,
        &AiChatWorkspaceLeftAudit {
            context: audit_context(&auth, &metadata),
            conversation_id: conversation.public_id.clone(),
            application_id: application.public_id.clone(),
        },
    )
    .await;
    Ok(Json(ChatWorkspaceResponse {
        workspace: None,
        chats: Vec::new(),
        joinable: true,
        reason: None,
    }))
}

/// A workspace's architecture: what was accepted, what waits for a
/// decision, and how each bound part is doing.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BlueprintResponse {
    pub application_id: String,
    pub workspace_name: String,
    pub accepted: Option<BlueprintVersionResponse>,
    pub proposal: Option<BlueprintVersionResponse>,
    /// Which project or service realises each part, by part id.
    pub bindings: BTreeMap<String, ComponentBinding>,
    /// The live state of each bound part, by part id.
    pub live: BTreeMap<String, ComponentLive>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BlueprintVersionsResponse {
    pub versions: Vec<BlueprintVersionResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct BlueprintVersionsQuery {
    /// 1-based page. Default 1.
    pub page: Option<u64>,
    /// Default 20, at most 100.
    pub page_size: Option<u64>,
}

/// A version the user wrote; it is accepted as it is saved.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SaveBlueprintVersionRequest {
    pub document: BlueprintDocument,
    pub change_summary: String,
}

/// The project (for an app part) or service (for a data part) to bind.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct BindBlueprintComponentRequest {
    pub project_id: Option<i32>,
    pub service_id: Option<i32>,
}

async fn blueprint_response(
    service: &WorkspaceChatService,
    application: &temps_entities::ai_applications::Model,
) -> Result<BlueprintResponse, Problem> {
    let blueprints = service.blueprints();
    let state = blueprints.state(application.id).await?;
    let live = blueprints.live(application.id, &state.bindings).await?;
    Ok(BlueprintResponse {
        application_id: application.public_id.clone(),
        workspace_name: application.name.clone(),
        accepted: state.accepted.map(Into::into),
        proposal: state.proposal.map(Into::into),
        bindings: state.bindings,
        live,
    })
}

/// The workspace's architecture blueprint with the live state of its parts.
#[utoipa::path(
    get, tag = "AI Chat", path = "/ai/applications/{public_id}/blueprint",
    params(("public_id" = String, Path, description = "Workspace (application) public id")),
    responses(
        (status = 200, description = "The workspace's blueprint", body = BlueprintResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Workspace not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_blueprint(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(public_id): Path<String>,
) -> Result<Json<BlueprintResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let application = authorized_application_for_permissions(
        &state,
        &auth,
        &public_id,
        Some("active"),
        &[Permission::ProjectsRead],
    )
    .await?
    .application;
    let service = workspaces(&state)?;
    Ok(Json(blueprint_response(service, &application).await?))
}

/// Every version of the workspace's blueprint, newest first.
#[utoipa::path(
    get, tag = "AI Chat", path = "/ai/applications/{public_id}/blueprint/versions",
    params(
        ("public_id" = String, Path, description = "Workspace (application) public id"),
        BlueprintVersionsQuery
    ),
    responses(
        (status = 200, description = "Blueprint versions", body = BlueprintVersionsResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Workspace not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_blueprint_versions(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(public_id): Path<String>,
    Query(query): Query<BlueprintVersionsQuery>,
) -> Result<Json<BlueprintVersionsResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let application = authorized_application_for_permissions(
        &state,
        &auth,
        &public_id,
        Some("active"),
        &[Permission::ProjectsRead],
    )
    .await?
    .application;
    let page = query.page.unwrap_or(1).max(1);
    let page_size = query.page_size.unwrap_or(20).clamp(1, 100);
    let (versions, total) = workspaces(&state)?
        .blueprints()
        .versions(application.id, page, page_size)
        .await?;
    Ok(Json(BlueprintVersionsResponse {
        versions: versions.into_iter().map(Into::into).collect(),
        total,
        page,
        page_size,
    }))
}

/// Save a version the user wrote. It replaces the accepted version (and any
/// open proposal) as it is saved.
#[utoipa::path(
    post, tag = "AI Chat", path = "/ai/applications/{public_id}/blueprint/versions",
    params(("public_id" = String, Path, description = "Workspace (application) public id")),
    request_body = SaveBlueprintVersionRequest,
    responses(
        (status = 201, description = "The saved, accepted version", body = BlueprintResponse),
        (status = 400, description = "The document is not valid", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Workspace not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn save_blueprint_version(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(public_id): Path<String>,
    Json(request): Json<SaveBlueprintVersionRequest>,
) -> Result<(StatusCode, Json<BlueprintResponse>), Problem> {
    permission_guard!(auth, ProjectsWrite);
    deny_deployment_token!(auth);
    let application = authorized_application_for_permissions(
        &state,
        &auth,
        &public_id,
        Some("active"),
        &[Permission::ProjectsWrite],
    )
    .await?
    .application;
    let service = workspaces(&state)?;
    let version = service
        .blueprints()
        .save_user_version(
            application.id,
            auth.user_id(),
            request.document,
            &request.change_summary,
        )
        .await?;
    audit(
        &state,
        &AiBlueprintVersionCreatedAudit {
            context: audit_context(&auth, &metadata),
            application_id: application.public_id.clone(),
            version: version.version,
            status: "accepted".to_string(),
            conversation_id: None,
            change_summary: version.change_summary.clone(),
        },
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(blueprint_response(service, &application).await?),
    ))
}

async fn decide(
    state: &AppState,
    auth: &AuthContext,
    metadata: &RequestMetadata,
    public_id: &str,
    version: i32,
    decision: Decision,
) -> Result<Json<BlueprintResponse>, Problem> {
    let application = authorized_application_for_permissions(
        state,
        auth,
        public_id,
        Some("active"),
        &[Permission::ProjectsWrite],
    )
    .await?
    .application;
    let service = workspaces(state)?;
    service
        .blueprints()
        .decide(application.id, version, auth.user_id(), decision)
        .await?;
    audit(
        state,
        &AiBlueprintVersionDecidedAudit {
            context: audit_context(auth, metadata),
            application_id: application.public_id.clone(),
            version,
            decision: match decision {
                Decision::Accept => "accepted",
                Decision::Reject => "rejected",
            }
            .to_string(),
        },
    )
    .await;
    Ok(Json(blueprint_response(service, &application).await?))
}

/// Accept a proposed version: every chat of the workspace builds against it.
#[utoipa::path(
    post, tag = "AI Chat", path = "/ai/applications/{public_id}/blueprint/versions/{version}/accept",
    params(
        ("public_id" = String, Path, description = "Workspace (application) public id"),
        ("version" = i32, Path, description = "The proposed version")
    ),
    responses(
        (status = 200, description = "The blueprint after the decision", body = BlueprintResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Workspace or version not found", body = ProblemDetails),
        (status = 409, description = "The version was already decided or replaced", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn accept_blueprint_version(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((public_id, version)): Path<(String, i32)>,
) -> Result<Json<BlueprintResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    deny_deployment_token!(auth);
    decide(
        &state,
        &auth,
        &metadata,
        &public_id,
        version,
        Decision::Accept,
    )
    .await
}

/// Reject a proposed version. The accepted version, if any, stays.
#[utoipa::path(
    post, tag = "AI Chat", path = "/ai/applications/{public_id}/blueprint/versions/{version}/reject",
    params(
        ("public_id" = String, Path, description = "Workspace (application) public id"),
        ("version" = i32, Path, description = "The proposed version")
    ),
    responses(
        (status = 200, description = "The blueprint after the decision", body = BlueprintResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Workspace or version not found", body = ProblemDetails),
        (status = 409, description = "The version was already decided or replaced", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn reject_blueprint_version(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((public_id, version)): Path<(String, i32)>,
) -> Result<Json<BlueprintResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    deny_deployment_token!(auth);
    decide(
        &state,
        &auth,
        &metadata,
        &public_id,
        version,
        Decision::Reject,
    )
    .await
}

async fn change_binding(
    state: &AppState,
    auth: &AuthContext,
    metadata: &RequestMetadata,
    public_id: &str,
    component_id: &str,
    binding: Option<ComponentBinding>,
) -> Result<Json<BlueprintResponse>, Problem> {
    let application = authorized_application_for_permissions(
        state,
        auth,
        public_id,
        Some("active"),
        &[Permission::ProjectsWrite],
    )
    .await?;
    if let Some(binding) = &binding {
        if let Some(project_id) = binding.project_id {
            crate::handlers::ensure_application_project_permission(
                auth,
                &state.project_access_checker,
                &[project_id],
                &Permission::ProjectsWrite,
            )
            .await?;
        }
        if binding.service_id.is_some() {
            permission_guard!(auth, ExternalServicesRead);
        }
    }
    let application = application.application;
    let service = workspaces(state)?;
    service
        .blueprints()
        .bind(application.id, component_id, binding.clone())
        .await?;
    if let Some(project_id) = binding.as_ref().and_then(|binding| binding.project_id) {
        match state
            .applications
            .link_project(auth.user_id(), &application.public_id, project_id)
            .await
        {
            Ok(_) | Err(crate::ApplicationError::ProjectAlreadyLinked { .. }) => {}
            Err(error) => tracing::warn!(
                application_id = %application.public_id,
                project_id,
                "bound a blueprint part to a project but could not add it to the workspace: {error}"
            ),
        }
    }
    audit(
        state,
        &AiBlueprintBindingChangedAudit {
            context: audit_context(auth, metadata),
            application_id: application.public_id.clone(),
            component_id: component_id.to_string(),
            project_id: binding.as_ref().and_then(|binding| binding.project_id),
            service_id: binding.as_ref().and_then(|binding| binding.service_id),
            actor: "user".to_string(),
        },
    )
    .await;
    Ok(Json(blueprint_response(service, &application).await?))
}

/// Bind a part of the accepted blueprint to the project or service that
/// realises it.
#[utoipa::path(
    put, tag = "AI Chat", path = "/ai/applications/{public_id}/blueprint/bindings/{component_id}",
    params(
        ("public_id" = String, Path, description = "Workspace (application) public id"),
        ("component_id" = String, Path, description = "The part's id in the accepted blueprint")
    ),
    request_body = BindBlueprintComponentRequest,
    responses(
        (status = 200, description = "The blueprint with the new binding", body = BlueprintResponse),
        (status = 400, description = "The binding does not fit the part", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Workspace or part not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn bind_blueprint_component(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((public_id, component_id)): Path<(String, String)>,
    Json(request): Json<BindBlueprintComponentRequest>,
) -> Result<Json<BlueprintResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    deny_deployment_token!(auth);
    let binding = ComponentBinding {
        project_id: request.project_id,
        service_id: request.service_id,
    };
    change_binding(
        &state,
        &auth,
        &metadata,
        &public_id,
        &component_id,
        Some(binding),
    )
    .await
}

/// Remove a part's binding. The project or service itself is untouched.
#[utoipa::path(
    delete, tag = "AI Chat", path = "/ai/applications/{public_id}/blueprint/bindings/{component_id}",
    params(
        ("public_id" = String, Path, description = "Workspace (application) public id"),
        ("component_id" = String, Path, description = "The part's id in the accepted blueprint")
    ),
    responses(
        (status = 200, description = "The blueprint without the binding", body = BlueprintResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Workspace or part not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn unbind_blueprint_component(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((public_id, component_id)): Path<(String, String)>,
) -> Result<Json<BlueprintResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    deny_deployment_token!(auth);
    change_binding(&state, &auth, &metadata, &public_id, &component_id, None).await
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/ai/conversations/{public_id}/workspace",
            get(get_chat_workspace)
                .put(set_chat_workspace)
                .delete(leave_chat_workspace),
        )
        .route("/ai/applications/{public_id}/blueprint", get(get_blueprint))
        .route(
            "/ai/applications/{public_id}/blueprint/versions",
            get(list_blueprint_versions).post(save_blueprint_version),
        )
        .route(
            "/ai/applications/{public_id}/blueprint/versions/{version}/accept",
            post(accept_blueprint_version),
        )
        .route(
            "/ai/applications/{public_id}/blueprint/versions/{version}/reject",
            post(reject_blueprint_version),
        )
        .route(
            "/ai/applications/{public_id}/blueprint/bindings/{component_id}",
            put(bind_blueprint_component).delete(unbind_blueprint_component),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_chat_workspace,
        set_chat_workspace,
        leave_chat_workspace,
        get_blueprint,
        list_blueprint_versions,
        save_blueprint_version,
        accept_blueprint_version,
        reject_blueprint_version,
        bind_blueprint_component,
        unbind_blueprint_component
    ),
    components(schemas(
        ChatWorkspace,
        ChatWorkspaceResponse,
        NewChatWorkspace,
        SetChatWorkspaceRequest,
        KeepSandbox,
        WorkspaceChatSummary,
        BlueprintResponse,
        BlueprintVersionsResponse,
        SaveBlueprintVersionRequest,
        BindBlueprintComponentRequest,
        BlueprintVersionResponse,
        BlueprintDocument,
        Component,
        Flow,
        ComponentKind,
        ClientSurface,
        AppRole,
        AppRuntime,
        DataEngine,
        FlowKind,
        VersionStatus,
        ComponentBinding,
        ComponentLive,
        LiveStatus
    ))
)]
pub struct BlueprintApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    fn detail(problem: &Problem) -> String {
        problem
            .body
            .get("detail")
            .and_then(|detail| detail.as_str())
            .unwrap_or_default()
            .to_string()
    }

    #[test]
    fn an_invalid_document_is_a_bad_request_listing_every_problem() {
        let problem = Problem::from(BlueprintError::Invalid {
            application_id: 4,
            problems: vec![
                "the summary is empty".into(),
                "the design has no parts".into(),
            ],
        });
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
        assert!(detail(&problem).contains("workspace 4"));
        assert_eq!(
            problem
                .body
                .get("problems")
                .and_then(|p| p.as_array())
                .map(Vec::len),
            Some(2)
        );
    }

    #[test]
    fn deciding_twice_is_a_conflict_naming_the_version() {
        let problem = Problem::from(BlueprintError::NotOpen {
            application_id: 4,
            version: 2,
            status: "accepted".into(),
        });
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        assert!(detail(&problem).contains("Version 2"));
    }

    #[test]
    fn a_missing_version_or_part_is_not_found() {
        let missing = Problem::from(BlueprintError::VersionNotFound {
            application_id: 4,
            version: 9,
        });
        assert_eq!(missing.status_code, StatusCode::NOT_FOUND);
        let part = Problem::from(BlueprintError::UnknownComponent {
            application_id: 4,
            component_id: "api".into(),
        });
        assert_eq!(part.status_code, StatusCode::NOT_FOUND);
        assert!(detail(&part).contains("'api'"));
    }

    #[test]
    fn a_project_chat_cannot_join_a_workspace() {
        let problem = Problem::from(WorkspaceChatError::NotAssistantChat {
            conversation_id: "conv_1".into(),
            context_type: "project".into(),
        });
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
        assert!(detail(&problem).contains("conv_1"));
        let none = Problem::from(WorkspaceChatError::NoWorkspace {
            conversation_id: "conv_1".into(),
        });
        assert_eq!(none.status_code, StatusCode::NOT_FOUND);
    }
}
