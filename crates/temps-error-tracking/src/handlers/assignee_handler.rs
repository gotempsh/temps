// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Assigning error groups to users and teams (ADR 074, D1.6).

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    routing::{get, put},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_authz::{
    permission_guard, project_access_guard, project_scope_guard, AuthContext, RequireAuth,
};
use temps_core::problemdetails::Problem;
use temps_core::RequestMetadata;
use utoipa::{IntoParams, OpenApi, ToSchema};

use super::audit::{AuditContext, ErrorGroupAssignedAudit};
use super::types::AppState;
use crate::services::group_workflow::{Assignee, GroupAssignee};
use crate::services::ErrorTrackingError;

#[derive(OpenApi)]
#[openapi(
    paths(set_error_group_assignee, search_error_group_assignees),
    components(schemas(
        SetErrorGroupAssigneeRequest,
        ErrorGroupAssigneeResponse,
        ErrorGroupAssigneeOptionsResponse,
        GroupAssignee,
    )),
    tags((name = "error-tracking", description = "Error group assignment"))
)]
pub struct AssigneeApiDoc;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/error-groups/{group_id}/assignee",
            put(set_error_group_assignee),
        )
        .route(
            "/projects/{project_id}/error-group-assignees",
            get(search_error_group_assignees),
        )
}

/// Who to assign the group to. Set one of the two ids, or neither to
/// unassign.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct SetErrorGroupAssigneeRequest {
    pub user_id: Option<i32>,
    pub team_id: Option<i32>,
}

impl SetErrorGroupAssigneeRequest {
    fn assignee(&self) -> Result<Option<Assignee>, Problem> {
        match (self.user_id, self.team_id) {
            (Some(_), Some(_)) => Err(ErrorTrackingError::Validation(
                "set user_id or team_id, not both".into(),
            )
            .into()),
            (Some(id), None) => Ok(Some(Assignee::User(id))),
            (None, Some(id)) => Ok(Some(Assignee::Team(id))),
            (None, None) => Ok(None),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorGroupAssigneeResponse {
    pub group_id: i32,
    /// The new assignee; `None` when the group was unassigned.
    pub assignee: Option<GroupAssignee>,
    /// Whether the assignee changed. Assigning the current assignee again
    /// changes nothing and notifies nobody.
    pub changed: bool,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct AssigneeSearchQuery {
    /// Text in the user's name or email, or the team's name. Empty lists the
    /// first candidates alphabetically.
    #[serde(default)]
    pub q: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorGroupAssigneeOptionsResponse {
    /// Users who can access the project, at most 20.
    pub users: Vec<GroupAssignee>,
    /// Teams, at most 20.
    pub teams: Vec<GroupAssignee>,
}

/// Fail unless a user assignee can open the project the group belongs to.
async fn check_assignee_access(
    state: &AppState,
    project_id: i32,
    assignee: Option<Assignee>,
) -> Result<(), Problem> {
    let (Some(Assignee::User(user_id)), Some(checker)) =
        (assignee, state.project_access_checker.as_ref())
    else {
        return Ok(());
    };
    let allowed = checker
        .user_can_access_project(user_id, project_id)
        .await
        .map_err(|error| {
            tracing::error!(
                project_id,
                user_id,
                "could not check whether user {user_id} can access project {project_id}: {error}"
            );
            Problem::from(ErrorTrackingError::Validation(format!(
                "could not check whether user {user_id} can access project {project_id}"
            )))
        })?;
    if !allowed {
        return Err(ErrorTrackingError::Validation(format!(
            "user {user_id} cannot access project {project_id}, so the group cannot be assigned to them"
        ))
        .into());
    }
    Ok(())
}

/// Assign `group_id` after checking access, audit-log it, and return the
/// outcome. Shared with the legacy `assigned_to` field of the update route.
pub(crate) async fn assign_and_audit(
    state: &AppState,
    auth: &AuthContext,
    metadata: &RequestMetadata,
    project_id: i32,
    group_id: i32,
    assignee: Option<Assignee>,
) -> Result<ErrorGroupAssigneeResponse, Problem> {
    check_assignee_access(state, project_id, assignee).await?;
    let assigned_by = auth.require_user().ok().map(|user| user.name.clone());
    let outcome = state
        .error_tracking_service
        .assign_error_group(
            project_id,
            group_id,
            assignee,
            assigned_by,
            Some(metadata.base_url.clone()),
        )
        .await?;
    let changed = outcome.previous != assignee;
    let audit = ErrorGroupAssignedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        group_id,
        previous: outcome.previous,
        assignee,
        changed,
    };
    if let Err(error) = state.audit_service.create_audit_log(&audit).await {
        tracing::error!(
            project_id,
            group_id,
            "Failed to create error-group assignment audit log: {}",
            error
        );
    }
    Ok(ErrorGroupAssigneeResponse {
        group_id,
        assignee: outcome.current,
        changed,
    })
}

/// Assign an error group to a user or a team, or unassign it
///
/// The assignee is independent of the status: an assigned group can be
/// resolved or ignored and keeps its assignee. A new assignee is emailed
/// when an email provider is configured, and `status_change` alert rules
/// that list `assigned` fire.
#[utoipa::path(
    put,
    path = "/projects/{project_id}/error-groups/{group_id}/assignee",
    request_body = SetErrorGroupAssigneeRequest,
    responses(
        (status = 200, description = "Assignee set", body = ErrorGroupAssigneeResponse),
        (status = 400, description = "Both ids set, an unknown user or team, or a user without access to the project"),
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
pub async fn set_error_group_assignee(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, group_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<SetErrorGroupAssigneeRequest>,
) -> Result<Json<ErrorGroupAssigneeResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let assignee = request.assignee()?;
    let response =
        assign_and_audit(&state, &auth, &metadata, project_id, group_id, assignee).await?;
    Ok(Json(response))
}

/// Search users and teams to assign an error group to
///
/// Returns at most 20 users who can access the project and 20 teams whose
/// name (or a user's email) contains `q`.
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-group-assignees",
    responses(
        (status = 200, description = "Assignee candidates", body = ErrorGroupAssigneeOptionsResponse),
        (status = 400, description = "Search text too long"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Missing error_tracking:write or project access"),
        (status = 500, description = "Internal server error")
    ),
    params(("project_id" = i32, Path, description = "Project ID"), AssigneeSearchQuery),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn search_error_group_assignees(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(project_id): Path<i32>,
    Query(query): Query<AssigneeSearchQuery>,
) -> Result<Json<ErrorGroupAssigneeOptionsResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let candidates = state
        .error_tracking_service
        .workflow
        .search_assignees(&query.q)
        .await?;
    // At most MAX_ASSIGNEE_OPTIONS checks, one per candidate on the page.
    let mut users = Vec::with_capacity(candidates.users.len());
    for user in candidates.users {
        if check_assignee_access(&state, project_id, Some(Assignee::User(user.id)))
            .await
            .is_ok()
        {
            users.push(user);
        }
    }
    Ok(Json(ErrorGroupAssigneeOptionsResponse {
        users,
        teams: candidates.teams,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::group_workflow_handler::tests::{
        app_state, metadata, seed, test_user, RecordingAuditLogger,
    };
    use temps_authz::Role;
    use temps_test_utils::{is_container_runtime_unavailable, TestDatabase};

    #[tokio::test]
    async fn a_reader_cannot_assign_an_error_group() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let state = app_state(&db, Arc::new(RecordingAuditLogger::default()));
        let problem = set_error_group_assignee(
            State(state),
            RequireAuth(test_user(9, Role::Reader)),
            Path((1, 2)),
            Extension(metadata()),
            Json(SetErrorGroupAssigneeRequest::default()),
        )
        .await
        .expect_err("a viewer must be refused");
        assert_eq!(
            axum::response::IntoResponse::into_response(problem).status(),
            axum::http::StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn unassigning_a_group_is_audited_and_both_ids_are_refused() {
        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                eprintln!("Skipping assignee handler test: {error}");
                return;
            }
            Err(error) => panic!("assignee handler test database setup failed: {error}"),
        };
        let db = test_db.connection_arc();
        let (project_id, group_id, _) = seed(db.as_ref()).await;
        let recorder = Arc::new(RecordingAuditLogger::default());
        let state = app_state(&db, recorder.clone());

        let response = set_error_group_assignee(
            State(state.clone()),
            RequireAuth(test_user(7, Role::Admin)),
            Path((project_id, group_id)),
            Extension(metadata()),
            Json(SetErrorGroupAssigneeRequest::default()),
        )
        .await
        .expect("unassign succeeds")
        .0;
        assert_eq!(response.assignee, None);
        assert!(!response.changed, "it was not assigned");
        {
            let entries = recorder.entries.lock().expect("audit entries");
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].0, "ERROR_GROUP_ASSIGNED");
            assert_eq!(entries[0].1["group_id"], group_id);
        }

        let problem = set_error_group_assignee(
            State(state),
            RequireAuth(test_user(7, Role::Admin)),
            Path((project_id, group_id)),
            Extension(metadata()),
            Json(SetErrorGroupAssigneeRequest {
                user_id: Some(1),
                team_id: Some(1),
            }),
        )
        .await
        .expect_err("both ids are refused");
        assert_eq!(
            axum::response::IntoResponse::into_response(problem).status(),
            axum::http::StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn a_request_names_one_assignee_or_none() {
        let user = SetErrorGroupAssigneeRequest {
            user_id: Some(3),
            team_id: None,
        };
        assert_eq!(user.assignee().ok(), Some(Some(Assignee::User(3))));
        let team = SetErrorGroupAssigneeRequest {
            user_id: None,
            team_id: Some(4),
        };
        assert_eq!(team.assignee().ok(), Some(Some(Assignee::Team(4))));
        assert_eq!(
            SetErrorGroupAssigneeRequest::default().assignee().ok(),
            Some(None)
        );
        assert!(SetErrorGroupAssigneeRequest {
            user_id: Some(3),
            team_id: Some(4),
        }
        .assignee()
        .is_err());
    }
}
