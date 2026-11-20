// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Inbound filter routes (ADR 111, D1.4): per-project rules that drop noisy
//! Sentry error events before they are stored, and how many each dropped.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, patch},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_authz::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::{AuditOperation, RequestMetadata};
use tracing::error;
use utoipa::{OpenApi, ToSchema};

use super::audit::AuditContext;
use super::types::AppState;
use crate::services::inbound_filters::{
    InboundFilter, InboundFilterKind, MAX_FILTERS_PER_PROJECT, MAX_PATTERN_CHARS,
};

#[derive(OpenApi)]
#[openapi(
    paths(
        list_inbound_filters,
        create_inbound_filter,
        update_inbound_filter,
        delete_inbound_filter
    ),
    components(schemas(
        InboundFilterKind,
        InboundFilterResponse,
        InboundFiltersResponse,
        CreateInboundFilterRequest,
        UpdateInboundFilterRequest,
    )),
    tags((name = "error-tracking", description = "Error tracking inbound filters"))
)]
pub struct InboundFilterApiDoc;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/error-inbound-filters",
            get(list_inbound_filters).post(create_inbound_filter),
        )
        .route(
            "/projects/{project_id}/error-inbound-filters/{filter_id}",
            patch(update_inbound_filter).delete(delete_inbound_filter),
        )
}

/// One inbound filter rule.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct InboundFilterResponse {
    pub id: i32,
    pub project_id: i32,
    pub kind: InboundFilterKind,
    /// Glob for `error_message` (matched against `Type: message`) and
    /// `release`; `*` matches any run of characters, `?` one. Case-insensitive.
    pub pattern: Option<String>,
    pub enabled: bool,
    /// Events this rule dropped since it was created.
    pub dropped_count: i64,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_dropped_at: Option<chrono::DateTime<chrono::Utc>>,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<InboundFilter> for InboundFilterResponse {
    fn from(filter: InboundFilter) -> Self {
        Self {
            id: filter.id,
            project_id: filter.project_id,
            kind: filter.kind,
            pattern: filter.pattern,
            enabled: filter.enabled,
            dropped_count: filter.dropped_count,
            last_dropped_at: filter.last_dropped_at,
            created_at: filter.created_at,
            updated_at: filter.updated_at,
        }
    }
}

/// Every inbound filter of a project. Bounded by `limit`, so not paginated.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct InboundFiltersResponse {
    pub filters: Vec<InboundFilterResponse>,
    /// Most rules a project may hold.
    pub limit: u64,
    /// Longest pattern a rule may hold, in characters.
    pub max_pattern_chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateInboundFilterRequest {
    pub kind: InboundFilterKind,
    /// Required for `error_message` and `release`, refused for the others.
    pub pattern: Option<String>,
    /// Defaults to true.
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UpdateInboundFilterRequest {
    pub pattern: Option<String>,
    pub enabled: Option<bool>,
}

/// Audit event for a change to an inbound filter rule.
#[derive(Debug, Clone, Serialize)]
pub struct InboundFilterAudit {
    #[serde(skip)]
    operation: &'static str,
    pub context: AuditContext,
    pub project_id: i32,
    pub filter_id: i32,
    pub kind: InboundFilterKind,
    pub pattern: Option<String>,
    pub enabled: bool,
}

impl AuditOperation for InboundFilterAudit {
    fn operation_type(&self) -> String {
        self.operation.to_string()
    }
    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }
    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }
    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }
    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

async fn audit(
    state: &AppState,
    operation: &'static str,
    auth: &temps_authz::AuthContext,
    metadata: &RequestMetadata,
    filter: &InboundFilter,
) {
    let entry = InboundFilterAudit {
        operation,
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id: filter.project_id,
        filter_id: filter.id,
        kind: filter.kind,
        pattern: filter.pattern.clone(),
        enabled: filter.enabled,
    };
    if let Err(e) = state.audit_service.create_audit_log(&entry).await {
        error!(
            project_id = filter.project_id,
            filter_id = filter.id,
            "Failed to create audit log for {operation}: {e}"
        );
    }
}

/// List a project's inbound filters and how many events each dropped.
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-inbound-filters",
    tag = "error-tracking",
    operation_id = "listErrorInboundFilters",
    params(("project_id" = i32, Path, description = "Project ID")),
    responses(
        (status = 200, description = "The project's inbound filters", body = InboundFiltersResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_inbound_filters(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(project_id): Path<i32>,
) -> Result<Json<InboundFiltersResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let filters = state
        .error_tracking_service
        .inbound_filters
        .list(project_id)
        .await?;
    Ok(Json(InboundFiltersResponse {
        filters: filters.into_iter().map(Into::into).collect(),
        limit: MAX_FILTERS_PER_PROJECT,
        max_pattern_chars: MAX_PATTERN_CHARS,
    }))
}

/// Add an inbound filter. It applies to the next event.
#[utoipa::path(
    post,
    path = "/projects/{project_id}/error-inbound-filters",
    tag = "error-tracking",
    operation_id = "createErrorInboundFilter",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = CreateInboundFilterRequest,
    responses(
        (status = 201, description = "Filter created", body = InboundFilterResponse),
        (status = 400, description = "Invalid kind or pattern, or the project's limit is reached"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 409, description = "The project already has this built-in filter"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_inbound_filter(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(project_id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateInboundFilterRequest>,
) -> Result<(StatusCode, Json<InboundFilterResponse>), Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let filter = state
        .error_tracking_service
        .inbound_filters
        .create(
            project_id,
            request.kind,
            request.pattern,
            request.enabled.unwrap_or(true),
        )
        .await?;
    audit(
        &state,
        "ERROR_INBOUND_FILTER_CREATED",
        &auth,
        &metadata,
        &filter,
    )
    .await;
    Ok((StatusCode::CREATED, Json(filter.into())))
}

/// Change an inbound filter's pattern or switch it on or off.
#[utoipa::path(
    patch,
    path = "/projects/{project_id}/error-inbound-filters/{filter_id}",
    tag = "error-tracking",
    operation_id = "updateErrorInboundFilter",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("filter_id" = i32, Path, description = "Inbound filter ID")
    ),
    request_body = UpdateInboundFilterRequest,
    responses(
        (status = 200, description = "Filter updated", body = InboundFilterResponse),
        (status = 400, description = "Invalid pattern"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "No such filter in this project"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_inbound_filter(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, filter_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<UpdateInboundFilterRequest>,
) -> Result<Json<InboundFilterResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let filter = state
        .error_tracking_service
        .inbound_filters
        .update(project_id, filter_id, request.pattern, request.enabled)
        .await?;
    audit(
        &state,
        "ERROR_INBOUND_FILTER_UPDATED",
        &auth,
        &metadata,
        &filter,
    )
    .await;
    Ok(Json(filter.into()))
}

/// Delete an inbound filter. Matching events are stored again from the next
/// one.
#[utoipa::path(
    delete,
    path = "/projects/{project_id}/error-inbound-filters/{filter_id}",
    tag = "error-tracking",
    operation_id = "deleteErrorInboundFilter",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("filter_id" = i32, Path, description = "Inbound filter ID")
    ),
    responses(
        (status = 204, description = "Filter deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "No such filter in this project"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_inbound_filter(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, filter_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let filter = state
        .error_tracking_service
        .inbound_filters
        .delete(project_id, filter_id)
        .await?;
    audit(
        &state,
        "ERROR_INBOUND_FILTER_DELETED",
        &auth,
        &metadata,
        &filter,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::group_workflow_handler::tests::{
        app_state, metadata, seed, test_user, RecordingAuditLogger,
    };
    use axum::response::IntoResponse;
    use temps_authz::Role;
    use temps_test_utils::{is_container_runtime_unavailable, TestDatabase};

    #[tokio::test]
    async fn a_reader_can_list_but_not_change_inbound_filters() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let state = app_state(&db, Arc::new(RecordingAuditLogger::default()));
        let problem = create_inbound_filter(
            State(state.clone()),
            RequireAuth(test_user(9, Role::Reader)),
            Path(1),
            Extension(metadata()),
            Json(CreateInboundFilterRequest {
                kind: InboundFilterKind::Localhost,
                pattern: None,
                enabled: None,
            }),
        )
        .await
        .expect_err("a reader must be refused");
        assert_eq!(problem.into_response().status(), StatusCode::FORBIDDEN);
        let problem = delete_inbound_filter(
            State(state),
            RequireAuth(test_user(9, Role::Reader)),
            Path((1, 1)),
            Extension(metadata()),
        )
        .await
        .expect_err("a reader must be refused");
        assert_eq!(problem.into_response().status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn rules_are_created_audited_and_refused_with_problem_details() {
        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                eprintln!("Skipping inbound filter handler test: {error}");
                return;
            }
            Err(error) => panic!("inbound filter handler test database setup failed: {error}"),
        };
        let db = test_db.connection_arc();
        let (project_id, _, _) = seed(db.as_ref()).await;
        let recorder = Arc::new(RecordingAuditLogger::default());
        let state = app_state(&db, recorder.clone());
        let create = |kind, pattern: Option<&str>| {
            create_inbound_filter(
                State(state.clone()),
                RequireAuth(test_user(7, Role::Admin)),
                Path(project_id),
                Extension(metadata()),
                Json(CreateInboundFilterRequest {
                    kind,
                    pattern: pattern.map(str::to_string),
                    enabled: None,
                }),
            )
        };

        let (status, created) = create(InboundFilterKind::WebCrawlers, None)
            .await
            .expect("create succeeds");
        assert_eq!(status, StatusCode::CREATED);
        assert!(created.enabled, "a new rule is on by default");
        assert_eq!(created.dropped_count, 0);

        let duplicate = create(InboundFilterKind::WebCrawlers, None)
            .await
            .expect_err("a built-in kind exists once")
            .into_response();
        assert_eq!(duplicate.status(), StatusCode::CONFLICT);

        let invalid = create(InboundFilterKind::Release, None)
            .await
            .expect_err("a release rule needs a pattern")
            .into_response();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

        let missing = delete_inbound_filter(
            State(state.clone()),
            RequireAuth(test_user(7, Role::Admin)),
            Path((project_id, created.id + 1000)),
            Extension(metadata()),
        )
        .await
        .expect_err("unknown rule")
        .into_response();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        let listed = list_inbound_filters(
            State(state.clone()),
            RequireAuth(test_user(9, Role::Reader)),
            Path(project_id),
        )
        .await
        .expect("a reader can list")
        .0;
        assert_eq!(listed.filters.len(), 1);
        assert_eq!(listed.limit, MAX_FILTERS_PER_PROJECT);

        let entries = recorder.entries.lock().expect("lock");
        assert_eq!(entries.len(), 1, "only the successful write is audited");
        assert_eq!(entries[0].0, "ERROR_INBOUND_FILTER_CREATED");
        assert_eq!(entries[0].1["kind"], "web_crawlers");
        assert_eq!(entries[0].1["project_id"], project_id);
    }
}
