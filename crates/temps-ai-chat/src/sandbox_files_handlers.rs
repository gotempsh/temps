// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Endpoints behind the Files and Changes working tabs: browse the chat's
//! sandbox, view a file, list the Git changes under its work directory, show
//! one file's diff, and put a folder under Git so its changes can be shown.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Extension, Json, Router,
};
use serde::Deserialize;
use temps_auth::{deny_deployment_token, permission_guard, RequireAuth};
use temps_core::{
    problemdetails::{self, Problem, ProblemDetails},
    AuditContext, RequestMetadata,
};
use utoipa::{IntoParams, OpenApi, ToSchema};

use crate::audit::AiSandboxChangesTrackedAudit;
use crate::handlers::{ensure_user_conversation_access, AppState};
use crate::sandbox_files::{
    GitChangeKind, GitChangedFile, GitRepoChanges, SandboxChanges, SandboxDirectory, SandboxEntry,
    SandboxEntryKind, SandboxFileDiff, SandboxFileView, SandboxFilesError, SandboxFilesService,
};
use crate::sandbox_link_handlers::link_service;

impl From<SandboxFilesError> for Problem {
    fn from(error: SandboxFilesError) -> Self {
        match error {
            SandboxFilesError::Link(link) => Problem::from(link),
            SandboxFilesError::InvalidPath { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Sandbox Path")
                .with_detail(error.to_string()),
            SandboxFilesError::NotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Path Not Found")
                .with_detail(error.to_string()),
            SandboxFilesError::AlreadyTracked { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Already Tracked")
                .with_detail(error.to_string()),
            SandboxFilesError::NotReady { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Sandbox Not Ready")
                .with_detail(error.to_string()),
            SandboxFilesError::Timeout { .. } => problemdetails::new(StatusCode::GATEWAY_TIMEOUT)
                .with_title("Sandbox Command Timed Out")
                .with_detail(error.to_string()),
            SandboxFilesError::Command { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Sandbox Command Failed")
                    .with_detail(error.to_string())
            }
        }
    }
}

/// A directory to list; the work directory when omitted.
#[derive(Debug, Deserialize, IntoParams)]
pub struct SandboxDirectoryQuery {
    /// Absolute, or relative to the sandbox work directory.
    pub path: Option<String>,
}

/// A file to view.
#[derive(Debug, Deserialize, IntoParams)]
pub struct SandboxFileQuery {
    /// Absolute, or relative to the sandbox work directory.
    pub path: String,
}

/// One changed file of one repository.
#[derive(Debug, Deserialize, IntoParams)]
pub struct SandboxDiffQuery {
    /// The repository root, absolute or relative to the work directory.
    pub repo: String,
    /// The file, relative to the repository root.
    pub path: String,
    /// The path before a rename, relative to the repository root.
    pub old_path: Option<String>,
}

/// A folder to put under Git.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct TrackSandboxChangesRequest {
    /// Absolute, or relative to the work directory; the work directory when
    /// omitted.
    #[serde(default)]
    pub path: Option<String>,
}

async fn files_service(
    state: &AppState,
    auth: &temps_auth::context::AuthContext,
    public_id: &str,
) -> Result<(SandboxFilesService, temps_entities::ai_conversations::Model), Problem> {
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), public_id)
        .await?;
    ensure_user_conversation_access(state, auth, &conversation).await?;
    let link = link_service(state, &conversation.public_id)?;
    Ok((SandboxFilesService::new(link), conversation))
}

/// List one directory of the chat's sandbox: folders first, then files.
#[utoipa::path(
    get, tag = "AI Chat", path = "/ai/conversations/{public_id}/sandbox/files",
    params(("public_id" = String, Path, description = "Conversation public id"), SandboxDirectoryQuery),
    responses(
        (status = 200, description = "Directory listing", body = SandboxDirectory),
        (status = 400, description = "Invalid path", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation, sandbox or directory not found", body = ProblemDetails),
        (status = 409, description = "No sandbox is linked, or it could not be started", body = ProblemDetails),
        (status = 500, description = "The listing command failed", body = ProblemDetails),
        (status = 503, description = "This server has no sandbox backend", body = ProblemDetails),
        (status = 504, description = "The sandbox did not answer in time", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_chat_sandbox_files(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(public_id): Path<String>,
    Query(query): Query<SandboxDirectoryQuery>,
) -> Result<Json<SandboxDirectory>, Problem> {
    permission_guard!(auth, ProjectsRead);
    permission_guard!(auth, SandboxesRead);
    deny_deployment_token!(auth);
    let (service, conversation) = files_service(&state, &auth, &public_id).await?;
    let listing = service
        .list(&conversation, auth.user_id(), query.path.as_deref())
        .await?;
    Ok(Json(listing))
}

/// One file of the chat's sandbox, for the viewer. Text is cut at 1 MiB;
/// binary files come back without content.
#[utoipa::path(
    get, tag = "AI Chat", path = "/ai/conversations/{public_id}/sandbox/file",
    params(("public_id" = String, Path, description = "Conversation public id"), SandboxFileQuery),
    responses(
        (status = 200, description = "File content", body = SandboxFileView),
        (status = 400, description = "Invalid path", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation, sandbox or file not found", body = ProblemDetails),
        (status = 409, description = "No sandbox is linked, or it could not be started", body = ProblemDetails),
        (status = 500, description = "The read command failed", body = ProblemDetails),
        (status = 503, description = "This server has no sandbox backend", body = ProblemDetails),
        (status = 504, description = "The sandbox did not answer in time", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn read_chat_sandbox_file(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(public_id): Path<String>,
    Query(query): Query<SandboxFileQuery>,
) -> Result<Json<SandboxFileView>, Problem> {
    permission_guard!(auth, ProjectsRead);
    permission_guard!(auth, SandboxesRead);
    deny_deployment_token!(auth);
    let (service, conversation) = files_service(&state, &auth, &public_id).await?;
    let file = service
        .read(&conversation, auth.user_id(), &query.path)
        .await?;
    Ok(Json(file))
}

/// The uncommitted Git changes of every repository under the sandbox work
/// directory.
#[utoipa::path(
    get, tag = "AI Chat", path = "/ai/conversations/{public_id}/sandbox/changes",
    params(("public_id" = String, Path, description = "Conversation public id")),
    responses(
        (status = 200, description = "Changes per repository", body = SandboxChanges),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation or sandbox not found", body = ProblemDetails),
        (status = 409, description = "No sandbox is linked, or it could not be started", body = ProblemDetails),
        (status = 500, description = "The Git command failed", body = ProblemDetails),
        (status = 503, description = "This server has no sandbox backend", body = ProblemDetails),
        (status = 504, description = "The sandbox did not answer in time", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_chat_sandbox_changes(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(public_id): Path<String>,
) -> Result<Json<SandboxChanges>, Problem> {
    permission_guard!(auth, ProjectsRead);
    permission_guard!(auth, SandboxesRead);
    deny_deployment_token!(auth);
    let (service, conversation) = files_service(&state, &auth, &public_id).await?;
    let changes = service.changes(&conversation, auth.user_id()).await?;
    Ok(Json(changes))
}

/// Both sides of one changed file: as last committed, and as it is now.
#[utoipa::path(
    get, tag = "AI Chat", path = "/ai/conversations/{public_id}/sandbox/changes/diff",
    params(("public_id" = String, Path, description = "Conversation public id"), SandboxDiffQuery),
    responses(
        (status = 200, description = "Both sides of the diff", body = SandboxFileDiff),
        (status = 400, description = "Invalid repository or path", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation or sandbox not found", body = ProblemDetails),
        (status = 409, description = "No sandbox is linked, or it could not be started", body = ProblemDetails),
        (status = 500, description = "The Git command failed", body = ProblemDetails),
        (status = 503, description = "This server has no sandbox backend", body = ProblemDetails),
        (status = 504, description = "The sandbox did not answer in time", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_chat_sandbox_diff(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(public_id): Path<String>,
    Query(query): Query<SandboxDiffQuery>,
) -> Result<Json<SandboxFileDiff>, Problem> {
    permission_guard!(auth, ProjectsRead);
    permission_guard!(auth, SandboxesRead);
    deny_deployment_token!(auth);
    let (service, conversation) = files_service(&state, &auth, &public_id).await?;
    let diff = service
        .diff(
            &conversation,
            auth.user_id(),
            &query.repo,
            &query.path,
            query.old_path.as_deref(),
        )
        .await?;
    Ok(Json(diff))
}

/// Put a folder of the sandbox under Git, so its changes show from now on:
/// a `.gitignore` for dependencies, builds and `.env` files when it has none,
/// then a first commit of what is there. A folder already inside a
/// repository is refused with `409`.
#[utoipa::path(
    post, tag = "AI Chat", path = "/ai/conversations/{public_id}/sandbox/changes/track",
    params(("public_id" = String, Path, description = "Conversation public id")),
    request_body = TrackSandboxChangesRequest,
    responses(
        (status = 200, description = "Tracking started; the changes as they are now", body = SandboxChanges),
        (status = 400, description = "Invalid path", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation, sandbox or folder not found", body = ProblemDetails),
        (status = 409, description = "The folder is already tracked, no sandbox is linked, or it could not be started", body = ProblemDetails),
        (status = 500, description = "The Git command failed", body = ProblemDetails),
        (status = 503, description = "This server has no sandbox backend", body = ProblemDetails),
        (status = 504, description = "The sandbox did not answer in time", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn track_chat_sandbox_changes(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(public_id): Path<String>,
    Json(request): Json<TrackSandboxChangesRequest>,
) -> Result<Json<SandboxChanges>, Problem> {
    permission_guard!(auth, ProjectsRead);
    permission_guard!(auth, SandboxesExec);
    deny_deployment_token!(auth);
    let (service, conversation) = files_service(&state, &auth, &public_id).await?;
    let (sandbox_id, path, changes) = service
        .start_tracking(&conversation, auth.user_id(), request.path.as_deref())
        .await?;
    state
        .audit(&AiSandboxChangesTrackedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: conversation.project_id,
            conversation_id: conversation.public_id.clone(),
            sandbox_id,
            path,
        })
        .await;
    Ok(Json(changes))
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/ai/conversations/{public_id}/sandbox/files",
            get(list_chat_sandbox_files),
        )
        .route(
            "/ai/conversations/{public_id}/sandbox/file",
            get(read_chat_sandbox_file),
        )
        .route(
            "/ai/conversations/{public_id}/sandbox/changes",
            get(get_chat_sandbox_changes),
        )
        .route(
            "/ai/conversations/{public_id}/sandbox/changes/diff",
            get(get_chat_sandbox_diff),
        )
        .route(
            "/ai/conversations/{public_id}/sandbox/changes/track",
            post(track_chat_sandbox_changes),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_chat_sandbox_files,
        read_chat_sandbox_file,
        get_chat_sandbox_changes,
        get_chat_sandbox_diff,
        track_chat_sandbox_changes
    ),
    components(schemas(
        SandboxDirectory,
        SandboxEntry,
        SandboxEntryKind,
        SandboxFileView,
        SandboxChanges,
        GitRepoChanges,
        GitChangedFile,
        GitChangeKind,
        SandboxFileDiff,
        TrackSandboxChangesRequest
    ))
)]
pub struct SandboxFilesApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox_link::SandboxLinkError;

    #[test]
    fn file_errors_map_to_the_status_a_client_can_act_on() {
        let status = |error: SandboxFilesError| Problem::from(error).status_code;
        let conversation_id = || "conv_1".to_string();
        assert_eq!(
            status(SandboxFilesError::InvalidPath {
                conversation_id: conversation_id(),
                path: "a\\nb".into(),
                reason: "line break".into(),
            }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status(SandboxFilesError::NotFound {
                conversation_id: conversation_id(),
                path: "/w/x".into(),
            }),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status(SandboxFilesError::AlreadyTracked {
                conversation_id: conversation_id(),
                path: "/w".into(),
            }),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(SandboxFilesError::NotReady {
                conversation_id: conversation_id(),
                reason: "asleep".into(),
            }),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(SandboxFilesError::Timeout {
                conversation_id: conversation_id(),
                operation: "list /w".into(),
                secs: 30,
            }),
            StatusCode::GATEWAY_TIMEOUT
        );
        assert_eq!(
            status(SandboxFilesError::Command {
                conversation_id: conversation_id(),
                operation: "list /w".into(),
                reason: "boom".into(),
            }),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            status(SandboxFilesError::Link(SandboxLinkError::NotLinked {
                conversation_id: conversation_id(),
            })),
            StatusCode::CONFLICT
        );
    }

    #[test]
    fn a_tracking_request_may_name_no_folder() {
        let request: TrackSandboxChangesRequest = serde_json::from_str("{}").unwrap();
        assert!(request.path.is_none());
    }
}
