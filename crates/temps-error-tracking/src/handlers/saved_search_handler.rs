// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Saved searches over error groups (ADR 074, D1.8).

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_authz::{
    permission_guard, project_access_guard, project_scope_guard, AuthContext, RequireAuth,
};
use temps_core::problemdetails::Problem;
use temps_core::RequestMetadata;
use utoipa::{IntoParams, OpenApi, ToSchema};

use super::audit::{AuditContext, ErrorSavedSearchAudit};
use super::types::AppState;
use crate::services::group_workflow::{SavedSearch, SavedSearchChange, SavedSearchFilters};

#[derive(OpenApi)]
#[openapi(
    paths(
        list_error_saved_searches,
        create_error_saved_search,
        get_error_saved_search,
        update_error_saved_search,
        delete_error_saved_search,
    ),
    components(schemas(
        SavedSearchFilters,
        ErrorSavedSearchResponse,
        ErrorSavedSearchPageResponse,
        CreateErrorSavedSearchRequest,
        UpdateErrorSavedSearchRequest,
    )),
    tags((name = "error-tracking", description = "Saved searches over error groups"))
)]
pub struct SavedSearchApiDoc;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/error-saved-searches",
            get(list_error_saved_searches).post(create_error_saved_search),
        )
        .route(
            "/projects/{project_id}/error-saved-searches/{saved_search_id}",
            get(get_error_saved_search)
                .patch(update_error_saved_search)
                .delete(delete_error_saved_search),
        )
}

/// A saved search: a name and the list filters it applies.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorSavedSearchResponse {
    pub id: i32,
    pub project_id: i32,
    pub name: String,
    /// Visible to everyone who can open the project, not only its owner.
    pub shared: bool,
    pub owner_user_id: i32,
    pub owner_name: Option<String>,
    /// Whether the caller owns it, and so may change it.
    pub is_owner: bool,
    pub filters: SavedSearchFilters,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl ErrorSavedSearchResponse {
    fn new(search: SavedSearch, viewer: Option<i32>) -> Self {
        Self {
            is_owner: Some(search.owner_user_id) == viewer,
            id: search.id,
            project_id: search.project_id,
            name: search.name,
            shared: search.shared,
            owner_user_id: search.owner_user_id,
            owner_name: search.owner_name,
            filters: search.filters,
            created_at: search.created_at,
            updated_at: search.updated_at,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorSavedSearchPageResponse {
    /// Yours and the shared ones, newest first.
    pub data: Vec<ErrorSavedSearchResponse>,
    pub page: u64,
    pub page_size: u64,
    pub total_count: u64,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct SavedSearchListQuery {
    /// Page number, from 1.
    #[serde(default = "default_page")]
    pub page: u64,
    /// Searches per page, 1 to 100.
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
pub struct CreateErrorSavedSearchRequest {
    /// At most 100 characters, unique among your saved searches in the project.
    pub name: String,
    /// Share with everyone who can open the project.
    #[serde(default)]
    pub shared: bool,
    #[serde(default)]
    pub filters: SavedSearchFilters,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UpdateErrorSavedSearchRequest {
    pub name: Option<String>,
    pub shared: Option<bool>,
    /// Replaces every filter when present.
    pub filters: Option<SavedSearchFilters>,
}

/// Saved searches are owned, so they need a user (a session or a user's API
/// key), not a deployment token.
fn owner(auth: &AuthContext) -> Result<i32, Problem> {
    auth.user_id_opt().ok_or_else(|| {
        temps_core::error_builder::forbidden()
            .detail("Saved searches need a user session or a user's API key")
            .build()
    })
}

async fn write_audit(
    state: &AppState,
    auth: &AuthContext,
    metadata: &RequestMetadata,
    search: (i32, i32, &str, bool),
    action: &'static str,
) {
    let (project_id, saved_search_id, name, shared) = search;
    let entry = ErrorSavedSearchAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        saved_search_id,
        name: name.to_string(),
        shared,
        action,
    };
    if let Err(error) = state.audit_service.create_audit_log(&entry).await {
        tracing::error!(
            project_id,
            saved_search_id,
            "Failed to create saved-search audit log: {}",
            error
        );
    }
}

/// List your saved searches and the shared ones, newest first
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-saved-searches",
    responses(
        (status = 200, description = "A page of saved searches", body = ErrorSavedSearchPageResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Missing error_tracking:read or project access"),
        (status = 500, description = "Internal server error")
    ),
    params(("project_id" = i32, Path, description = "Project ID"), SavedSearchListQuery),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn list_error_saved_searches(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(project_id): Path<i32>,
    Query(query): Query<SavedSearchListQuery>,
) -> Result<Json<ErrorSavedSearchPageResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let page = query.page.max(1);
    let page_size = query.page_size.clamp(1, 100);
    let viewer = auth.user_id_opt();
    let result = state
        .error_tracking_service
        .workflow
        .list_saved_searches(project_id, viewer, page, page_size)
        .await?;
    Ok(Json(ErrorSavedSearchPageResponse {
        data: result
            .searches
            .into_iter()
            .map(|search| ErrorSavedSearchResponse::new(search, viewer))
            .collect(),
        page,
        page_size,
        total_count: result.total,
    }))
}

/// Get one saved search: yours, or a shared one
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-saved-searches/{saved_search_id}",
    responses(
        (status = 200, description = "The saved search", body = ErrorSavedSearchResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Missing error_tracking:read or project access"),
        (status = 404, description = "No such saved search you can see in the project"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("saved_search_id" = i32, Path, description = "Saved search ID")
    ),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn get_error_saved_search(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, saved_search_id)): Path<(i32, i32)>,
) -> Result<Json<ErrorSavedSearchResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let viewer = auth.user_id_opt();
    let search = state
        .error_tracking_service
        .workflow
        .get_saved_search(project_id, saved_search_id, viewer)
        .await?;
    Ok(Json(ErrorSavedSearchResponse::new(search, viewer)))
}

/// Save a search over the project's error groups
#[utoipa::path(
    post,
    path = "/projects/{project_id}/error-saved-searches",
    request_body = CreateErrorSavedSearchRequest,
    responses(
        (status = 201, description = "Search saved", body = ErrorSavedSearchResponse),
        (status = 400, description = "An invalid name or filter, or 50 saved searches already"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Missing error_tracking:write or project access"),
        (status = 409, description = "You already have a saved search with that name"),
        (status = 500, description = "Internal server error")
    ),
    params(("project_id" = i32, Path, description = "Project ID")),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn create_error_saved_search(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(project_id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateErrorSavedSearchRequest>,
) -> Result<(StatusCode, Json<ErrorSavedSearchResponse>), Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let owner = owner(&auth)?;
    let search = state
        .error_tracking_service
        .workflow
        .create_saved_search(
            project_id,
            owner,
            &request.name,
            request.shared,
            request.filters,
        )
        .await?;
    write_audit(
        &state,
        &auth,
        &metadata,
        (project_id, search.id, &search.name, search.shared),
        "created",
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(ErrorSavedSearchResponse::new(search, Some(owner))),
    ))
}

/// Rename, share or change the filters of your saved search
#[utoipa::path(
    patch,
    path = "/projects/{project_id}/error-saved-searches/{saved_search_id}",
    request_body = UpdateErrorSavedSearchRequest,
    responses(
        (status = 200, description = "Saved search changed", body = ErrorSavedSearchResponse),
        (status = 400, description = "An invalid name or filter"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Not its owner, or missing error_tracking:write"),
        (status = 404, description = "No such saved search you can see in the project"),
        (status = 409, description = "You already have a saved search with that name"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("saved_search_id" = i32, Path, description = "Saved search ID")
    ),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn update_error_saved_search(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, saved_search_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<UpdateErrorSavedSearchRequest>,
) -> Result<Json<ErrorSavedSearchResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let owner = owner(&auth)?;
    let search = state
        .error_tracking_service
        .workflow
        .update_saved_search(
            project_id,
            saved_search_id,
            owner,
            SavedSearchChange {
                name: request.name,
                shared: request.shared,
                filters: request.filters,
            },
        )
        .await?;
    write_audit(
        &state,
        &auth,
        &metadata,
        (project_id, search.id, &search.name, search.shared),
        "updated",
    )
    .await;
    Ok(Json(ErrorSavedSearchResponse::new(search, Some(owner))))
}

/// Delete a saved search
///
/// Its owner may, and an administrator may delete a shared one.
#[utoipa::path(
    delete,
    path = "/projects/{project_id}/error-saved-searches/{saved_search_id}",
    responses(
        (status = 204, description = "Saved search deleted"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Neither its owner nor an administrator"),
        (status = 404, description = "No such saved search you can see in the project"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("saved_search_id" = i32, Path, description = "Saved search ID")
    ),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn delete_error_saved_search(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, saved_search_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let viewer = Some(owner(&auth)?);
    let workflow = &state.error_tracking_service.workflow;
    let search = workflow
        .get_saved_search(project_id, saved_search_id, viewer)
        .await?;
    workflow
        .delete_saved_search(project_id, saved_search_id, viewer, auth.is_admin())
        .await?;
    write_audit(
        &state,
        &auth,
        &metadata,
        (project_id, saved_search_id, &search.name, search.shared),
        "deleted",
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::group_workflow_handler::tests::{
        app_state, create_user, metadata, seed, test_user, RecordingAuditLogger,
    };
    use temps_authz::Role;
    use temps_test_utils::{is_container_runtime_unavailable, TestDatabase};

    fn status_of(problem: Problem) -> StatusCode {
        axum::response::IntoResponse::into_response(problem).status()
    }

    #[tokio::test]
    async fn a_reader_cannot_save_a_search() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let state = app_state(&db, Arc::new(RecordingAuditLogger::default()));
        let problem = create_error_saved_search(
            State(state),
            RequireAuth(test_user(9, Role::Reader)),
            Path(1),
            Extension(metadata()),
            Json(CreateErrorSavedSearchRequest {
                name: "Mine".into(),
                shared: false,
                filters: SavedSearchFilters::default(),
            }),
        )
        .await
        .expect_err("a viewer must be refused");
        assert_eq!(status_of(problem), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn saved_searches_are_audited_private_by_default_and_owner_only() {
        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                eprintln!("Skipping saved search handler test: {error}");
                return;
            }
            Err(error) => panic!("saved search handler test database setup failed: {error}"),
        };
        let db = test_db.connection_arc();
        let (project_id, _, _) = seed(db.as_ref()).await;
        let owner_id = create_user(db.as_ref()).await;
        let other_id = create_user(db.as_ref()).await;
        let recorder = Arc::new(RecordingAuditLogger::default());
        let state = app_state(&db, recorder.clone());

        let (status, created) = create_error_saved_search(
            State(state.clone()),
            RequireAuth(test_user(owner_id, Role::User)),
            Path(project_id),
            Extension(metadata()),
            Json(CreateErrorSavedSearchRequest {
                name: "Mine, open".into(),
                shared: false,
                filters: SavedSearchFilters {
                    status: Some("unresolved".into()),
                    assignee: Some("me".into()),
                    ..Default::default()
                },
            }),
        )
        .await
        .expect("create");
        assert_eq!(status, StatusCode::CREATED);
        assert!(created.is_owner);

        let hidden = get_error_saved_search(
            State(state.clone()),
            RequireAuth(test_user(other_id, Role::User)),
            Path((project_id, created.id)),
        )
        .await
        .expect_err("private to its owner");
        assert_eq!(status_of(hidden), StatusCode::NOT_FOUND);

        let shared = update_error_saved_search(
            State(state.clone()),
            RequireAuth(test_user(owner_id, Role::User)),
            Path((project_id, created.id)),
            Extension(metadata()),
            Json(UpdateErrorSavedSearchRequest {
                shared: Some(true),
                ..Default::default()
            }),
        )
        .await
        .expect("share")
        .0;
        assert!(shared.shared);

        let seen = list_error_saved_searches(
            State(state.clone()),
            RequireAuth(test_user(other_id, Role::Reader)),
            Path(project_id),
            Query(SavedSearchListQuery {
                page: 1,
                page_size: 20,
            }),
        )
        .await
        .expect("list")
        .0;
        assert_eq!(seen.total_count, 1);
        assert!(!seen.data[0].is_owner);
        assert_eq!(seen.data[0].filters.assignee.as_deref(), Some("me"));

        let refused = update_error_saved_search(
            State(state.clone()),
            RequireAuth(test_user(other_id, Role::Admin)),
            Path((project_id, created.id)),
            Extension(metadata()),
            Json(UpdateErrorSavedSearchRequest {
                name: Some("Taken over".into()),
                ..Default::default()
            }),
        )
        .await
        .expect_err("only the owner changes it");
        assert_eq!(status_of(refused), StatusCode::FORBIDDEN);

        let deleted = delete_error_saved_search(
            State(state.clone()),
            RequireAuth(test_user(other_id, Role::Admin)),
            Path((project_id, created.id)),
            Extension(metadata()),
        )
        .await
        .expect("an administrator deletes a shared search");
        assert_eq!(deleted, StatusCode::NO_CONTENT);

        let entries = recorder.entries.lock().expect("audit entries").clone();
        let operations: Vec<&str> = entries.iter().map(|(op, _)| op.as_str()).collect();
        assert_eq!(
            operations,
            vec![
                "ERROR_SAVED_SEARCH_CREATED",
                "ERROR_SAVED_SEARCH_UPDATED",
                "ERROR_SAVED_SEARCH_DELETED"
            ]
        );
        assert_eq!(entries[1].1["shared"], true);
    }
}
