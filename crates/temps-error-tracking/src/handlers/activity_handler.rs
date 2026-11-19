// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Comments and the activity log of an error group (ADR 074, D1.7).

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, patch, post},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_authz::{
    permission_guard, project_access_guard, project_scope_guard, AuthContext, RequireAuth,
};
use temps_core::problemdetails::Problem;
use temps_core::RequestMetadata;
use utoipa::{IntoParams, OpenApi, ToSchema};

use super::audit::{AuditContext, ErrorGroupCommentAudit};
use super::types::AppState;
use crate::services::group_workflow::{
    ActivityDetails, ActivityEntry, ActivityKind, Assignee, MAX_COMMENT_CHARS,
};

#[derive(OpenApi)]
#[openapi(
    paths(
        list_error_group_activity,
        create_error_group_comment,
        update_error_group_comment,
        delete_error_group_comment,
    ),
    components(schemas(
        ActivityKind,
        ActivityDetails,
        Assignee,
        ErrorGroupActivityResponse,
        ErrorGroupActivityPageResponse,
        ErrorGroupCommentRequest,
    )),
    tags((name = "error-tracking", description = "Error group comments and activity"))
)]
pub struct ActivityApiDoc;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/error-groups/{group_id}/activity",
            get(list_error_group_activity),
        )
        .route(
            "/projects/{project_id}/error-groups/{group_id}/comments",
            post(create_error_group_comment),
        )
        .route(
            "/projects/{project_id}/error-groups/{group_id}/comments/{comment_id}",
            patch(update_error_group_comment).delete(delete_error_group_comment),
        )
}

/// One entry of a group's activity log.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorGroupActivityResponse {
    pub id: i64,
    pub group_id: i32,
    pub kind: ActivityKind,
    /// Who did it; `None` for an event that reopened the group, or a user
    /// who has since been deleted.
    pub actor_user_id: Option<i32>,
    pub actor_name: Option<String>,
    /// The comment text, for `comment` entries.
    pub body: Option<String>,
    pub details: ActivityDetails,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub edited_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<ActivityEntry> for ErrorGroupActivityResponse {
    fn from(entry: ActivityEntry) -> Self {
        Self {
            id: entry.id,
            group_id: entry.group_id,
            kind: entry.kind,
            actor_user_id: entry.actor_user_id,
            actor_name: entry.actor_name,
            body: entry.body,
            details: entry.details,
            created_at: entry.created_at,
            edited_at: entry.edited_at,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorGroupActivityPageResponse {
    /// Newest first.
    pub data: Vec<ErrorGroupActivityResponse>,
    pub page: u64,
    pub page_size: u64,
    pub total_count: u64,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ActivityQuery {
    /// Page number, from 1.
    #[serde(default = "default_page")]
    pub page: u64,
    /// Entries per page, 1 to 100.
    #[serde(default = "default_page_size")]
    pub page_size: u64,
}

fn default_page() -> u64 {
    1
}
fn default_page_size() -> u64 {
    20
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ErrorGroupCommentRequest {
    /// The comment, at most 10,000 characters; Markdown is shown as text.
    #[schema(max_length = 10000)]
    pub body: String,
}

/// Comments carry their author, so they need a user (a session or a user's
/// API key), not a deployment token.
fn comment_author(auth: &AuthContext) -> Result<i32, Problem> {
    auth.user_id_opt().ok_or_else(|| {
        temps_core::error_builder::forbidden()
            .detail("Commenting on an error group needs a user session or a user's API key")
            .build()
    })
}

fn audit(
    auth: &AuthContext,
    metadata: &RequestMetadata,
    project_id: i32,
    group_id: i32,
    comment_id: i64,
    action: &'static str,
) -> ErrorGroupCommentAudit {
    ErrorGroupCommentAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        group_id,
        comment_id,
        action,
    }
}

async fn write_audit(state: &AppState, entry: &ErrorGroupCommentAudit) {
    if let Err(error) = state.audit_service.create_audit_log(entry).await {
        tracing::error!(
            project_id = entry.project_id,
            group_id = entry.group_id,
            "Failed to create error-group comment audit log: {}",
            error
        );
    }
}

/// Record an activity entry after a change that already succeeded. A failed
/// write is logged and never fails the request, as with audit logs.
pub(crate) async fn record_activity(
    state: &AppState,
    project_id: i32,
    group_ids: &[i32],
    kind: ActivityKind,
    actor_user_id: Option<i32>,
    details: ActivityDetails,
) {
    if let Err(error) = state
        .error_tracking_service
        .workflow
        .record_activity(project_id, group_ids, kind, actor_user_id, details)
        .await
    {
        tracing::error!(
            project_id,
            groups = group_ids.len(),
            "Failed to record error-group activity {:?}: {}",
            kind,
            error
        );
    }
}

/// List a group's comments and activity, newest first
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-groups/{group_id}/activity",
    responses(
        (status = 200, description = "A page of the activity log", body = ErrorGroupActivityPageResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Missing error_tracking:read or project access"),
        (status = 404, description = "Error group not found in the project"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("group_id" = i32, Path, description = "Error group ID"),
        ActivityQuery
    ),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn list_error_group_activity(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, group_id)): Path<(i32, i32)>,
    Query(query): Query<ActivityQuery>,
) -> Result<Json<ErrorGroupActivityPageResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let page = query.page.max(1);
    let page_size = query.page_size.clamp(1, 100);
    let result = state
        .error_tracking_service
        .workflow
        .list_activity(project_id, group_id, page, page_size)
        .await?;
    Ok(Json(ErrorGroupActivityPageResponse {
        data: result.entries.into_iter().map(Into::into).collect(),
        page,
        page_size,
        total_count: result.total,
    }))
}

/// Comment on an error group
#[utoipa::path(
    post,
    path = "/projects/{project_id}/error-groups/{group_id}/comments",
    request_body = ErrorGroupCommentRequest,
    responses(
        (status = 201, description = "Comment added", body = ErrorGroupActivityResponse),
        (status = 400, description = "Empty or longer than 10,000 characters"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Missing error_tracking:write or project access"),
        (status = 404, description = "Error group not found in the project"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("group_id" = i32, Path, description = "Error group ID")
    ),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn create_error_group_comment(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, group_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<ErrorGroupCommentRequest>,
) -> Result<(StatusCode, Json<ErrorGroupActivityResponse>), Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let entry = state
        .error_tracking_service
        .workflow
        .add_comment(project_id, group_id, comment_author(&auth)?, &request.body)
        .await?;
    write_audit(
        &state,
        &audit(&auth, &metadata, project_id, group_id, entry.id, "created"),
    )
    .await;
    Ok((StatusCode::CREATED, Json(entry.into())))
}

/// Edit your comment on an error group
#[utoipa::path(
    patch,
    path = "/projects/{project_id}/error-groups/{group_id}/comments/{comment_id}",
    request_body = ErrorGroupCommentRequest,
    responses(
        (status = 200, description = "Comment changed", body = ErrorGroupActivityResponse),
        (status = 400, description = "Empty or longer than 10,000 characters"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Not the comment's author, or missing error_tracking:write"),
        (status = 404, description = "Comment not found on the group"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("group_id" = i32, Path, description = "Error group ID"),
        ("comment_id" = i64, Path, description = "Comment ID")
    ),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn update_error_group_comment(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, group_id, comment_id)): Path<(i32, i32, i64)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<ErrorGroupCommentRequest>,
) -> Result<Json<ErrorGroupActivityResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let entry = state
        .error_tracking_service
        .workflow
        .edit_comment(
            project_id,
            group_id,
            comment_id,
            comment_author(&auth)?,
            &request.body,
        )
        .await?;
    write_audit(
        &state,
        &audit(&auth, &metadata, project_id, group_id, comment_id, "edited"),
    )
    .await;
    Ok(Json(entry.into()))
}

/// Delete a comment on an error group
///
/// Its author may delete it, and so may an administrator.
#[utoipa::path(
    delete,
    path = "/projects/{project_id}/error-groups/{group_id}/comments/{comment_id}",
    responses(
        (status = 204, description = "Comment deleted"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Neither the author nor an administrator"),
        (status = 404, description = "Comment not found on the group"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("group_id" = i32, Path, description = "Error group ID"),
        ("comment_id" = i64, Path, description = "Comment ID")
    ),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn delete_error_group_comment(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, group_id, comment_id)): Path<(i32, i32, i64)>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    state
        .error_tracking_service
        .workflow
        .delete_comment(
            project_id,
            group_id,
            comment_id,
            comment_author(&auth)?,
            auth.is_admin(),
        )
        .await?;
    write_audit(
        &state,
        &audit(
            &auth, &metadata, project_id, group_id, comment_id, "deleted",
        ),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

const _: () = assert!(
    MAX_COMMENT_CHARS == 10_000,
    "keep the schema max_length in step"
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::group_workflow_handler::tests::{
        app_state, create_user, metadata, seed, test_user, RecordingAuditLogger,
    };
    use temps_authz::Role;
    use temps_test_utils::{is_container_runtime_unavailable, TestDatabase};

    #[tokio::test]
    async fn a_reader_cannot_comment() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let state = app_state(&db, Arc::new(RecordingAuditLogger::default()));
        let problem = create_error_group_comment(
            State(state),
            RequireAuth(test_user(9, Role::Reader)),
            Path((1, 2)),
            Extension(metadata()),
            Json(ErrorGroupCommentRequest {
                body: "seen it".into(),
            }),
        )
        .await
        .expect_err("a viewer must be refused");
        assert_eq!(
            axum::response::IntoResponse::into_response(problem).status(),
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn comments_are_audited_and_only_their_author_edits_them() {
        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                eprintln!("Skipping comment handler test: {error}");
                return;
            }
            Err(error) => panic!("comment handler test database setup failed: {error}"),
        };
        let db = test_db.connection_arc();
        let (project_id, group_id, _) = seed(db.as_ref()).await;
        let author = create_user(db.as_ref()).await;
        let recorder = Arc::new(RecordingAuditLogger::default());
        let state = app_state(&db, recorder.clone());

        let (status, created) = create_error_group_comment(
            State(state.clone()),
            RequireAuth(test_user(author, Role::User)),
            Path((project_id, group_id)),
            Extension(metadata()),
            Json(ErrorGroupCommentRequest {
                body: "  started after the cache change ".into(),
            }),
        )
        .await
        .expect("comment");
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(
            created.body.as_deref(),
            Some("started after the cache change")
        );
        assert_eq!(created.actor_name.as_deref(), Some("Ana"));

        let problem = update_error_group_comment(
            State(state.clone()),
            RequireAuth(test_user(author + 1000, Role::Admin)),
            Path((project_id, group_id, created.id)),
            Extension(metadata()),
            Json(ErrorGroupCommentRequest {
                body: "rewritten".into(),
            }),
        )
        .await
        .expect_err("someone else's comment");
        assert_eq!(
            axum::response::IntoResponse::into_response(problem).status(),
            StatusCode::FORBIDDEN
        );

        let page = list_error_group_activity(
            State(state.clone()),
            RequireAuth(test_user(author, Role::Reader)),
            Path((project_id, group_id)),
            Query(ActivityQuery {
                page: 1,
                page_size: 20,
            }),
        )
        .await
        .expect("list")
        .0;
        assert_eq!(page.total_count, 1);
        assert_eq!(page.data[0].kind, ActivityKind::Comment);

        let deleted = delete_error_group_comment(
            State(state.clone()),
            RequireAuth(test_user(author + 1000, Role::Admin)),
            Path((project_id, group_id, created.id)),
            Extension(metadata()),
        )
        .await
        .expect("an administrator may delete it");
        assert_eq!(deleted, StatusCode::NO_CONTENT);

        let entries = recorder.entries.lock().expect("audit entries").clone();
        let actions: Vec<&str> = entries
            .iter()
            .map(|(_, payload)| payload["action"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            actions,
            vec!["created", "deleted"],
            "the refused edit is not audited"
        );
    }
}
