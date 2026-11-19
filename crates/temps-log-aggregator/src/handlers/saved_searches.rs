// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Saved log search endpoints (ADR 108 D1).
//!
//! Project-scoped CRUD. Reads need `logs:read`, writes `logs:write`, and
//! every call is additionally confined to the project the caller was
//! authorized for, so a saved search cannot be read or written across
//! projects by guessing an id.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, patch};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use temps_authz::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::ProblemDetails;
use temps_core::RequestMetadata;
use utoipa::{IntoParams, ToSchema};

use temps_entities::log_saved_searches::Model;

use crate::handlers::types::LogAggregatorAppState;
use crate::services::{
    decode_saved_search_query, NewSavedSearch, SavedSearchQuery, SavedSearchUpdate,
    SAVED_SEARCH_TIME_RANGES,
};

// ── DTOs ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, IntoParams)]
pub struct ListSavedSearchesParams {
    /// 1-based page number. Default 1.
    pub page: Option<u64>,
    /// Rows per page. Default 20, maximum 100.
    pub page_size: Option<u64>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateSavedSearchRequest {
    /// Unique within the project.
    pub name: String,
    pub description: Option<String>,
    /// The filter set. An empty query is allowed: "everything in this
    /// project" is a legitimate thing to pin.
    #[serde(default)]
    pub query: SavedSearchQuery,
    /// One of `15m`, `1h`, `6h`, `24h`, `7d`, `30d`. Default `1h`.
    pub time_range: Option<String>,
    /// Pinned searches sort first. Default false.
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateSavedSearchRequest {
    /// Absent leaves the name unchanged.
    pub name: Option<String>,
    /// Absent leaves the description unchanged; an empty string clears it.
    pub description: Option<String>,
    /// Replaces the filter set wholesale when present.
    pub query: Option<SavedSearchQuery>,
    pub time_range: Option<String>,
    pub pinned: Option<bool>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SavedSearchResponse {
    pub id: i32,
    pub project_id: i32,
    pub name: String,
    pub description: Option<String>,
    pub query: SavedSearchQuery,
    pub time_range: String,
    pub pinned: bool,
    /// Log alert rules counting this search. A search with alerts cannot be
    /// deleted until they are detached, so the console can say so before
    /// offering the button.
    pub alert_count: u64,
    pub created_by: Option<i32>,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: DateTime<Utc>,
}

impl SavedSearchResponse {
    fn new(model: Model, alert_count: u64) -> Self {
        // A query blob that no longer decodes is reported as the empty
        // filter set *with* its name and id, so the operator can see and
        // fix the broken row instead of losing the whole list to one error.
        let query = decode_saved_search_query(&model).unwrap_or_else(|e| {
            tracing::error!(
                saved_search_id = model.id,
                error = %e,
                "a saved log search has a query that no longer decodes; reporting it empty"
            );
            SavedSearchQuery::default()
        });
        Self {
            id: model.id,
            project_id: model.project_id,
            name: model.name,
            description: model.description,
            query,
            time_range: model.time_range,
            pinned: model.pinned,
            alert_count,
            created_by: model.created_by,
            created_at: model.created_at,
            updated_at: model.updated_at,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SavedSearchesResponse {
    pub data: Vec<SavedSearchResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
    /// Allowed `time_range` values, so a client never has to hard-code them.
    pub time_ranges: Vec<String>,
}

// ── Audit events ────────────────────────────────────────────────────────

macro_rules! saved_search_audit {
    ($name:ident, $operation:literal) => {
        #[derive(Debug, Clone, serde::Serialize)]
        struct $name {
            context: temps_core::AuditContext,
            project_id: i32,
            saved_search_id: i32,
            name: String,
        }

        impl temps_core::AuditOperation for $name {
            fn operation_type(&self) -> String {
                $operation.to_string()
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
    };
}

saved_search_audit!(SavedSearchCreatedAudit, "LOG_SAVED_SEARCH_CREATED");
saved_search_audit!(SavedSearchUpdatedAudit, "LOG_SAVED_SEARCH_UPDATED");
saved_search_audit!(SavedSearchDeletedAudit, "LOG_SAVED_SEARCH_DELETED");

fn audit_context(
    auth: &temps_authz::AuthContext,
    metadata: &RequestMetadata,
) -> temps_core::AuditContext {
    temps_core::AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

// ── Routes ──────────────────────────────────────────────────────────────

pub fn configure_routes() -> Router<Arc<LogAggregatorAppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/logs/saved-searches",
            get(list_saved_searches).post(create_saved_search),
        )
        .route(
            "/projects/{project_id}/logs/saved-searches/{saved_search_id}",
            patch(update_saved_search)
                .get(get_saved_search)
                .delete(delete_saved_search),
        )
        .route(
            "/projects/{project_id}/logs/saved-searches/{saved_search_id}/series",
            get(saved_search_series),
        )
}

// ── Handlers ────────────────────────────────────────────────────────────

/// List a project's saved log searches, pinned first.
#[utoipa::path(
    tag = "Logs",
    get,
    path = "/projects/{project_id}/logs/saved-searches",
    params(("project_id" = i32, Path, description = "Project ID"), ListSavedSearchesParams),
    responses(
        (status = 200, description = "Saved searches", body = SavedSearchesResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_saved_searches(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<LogAggregatorAppState>>,
    Path(project_id): Path<i32>,
    Query(params): Query<ListSavedSearchesParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LogsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let page = params.page.unwrap_or(1).max(1);
    let page_size = params.page_size.unwrap_or(20).clamp(1, 100);
    let (items, total) = state
        .saved_search_service
        .list(project_id, Some(page), Some(page_size))
        .await?;
    // One grouped query for every row's alert count, never one per row.
    let ids: Vec<i32> = items.iter().map(|item| item.id).collect();
    let counts = state.saved_search_service.alert_counts(&ids).await?;
    Ok(Json(SavedSearchesResponse {
        data: items
            .into_iter()
            .map(|item| {
                let alert_count = counts.get(&item.id).copied().unwrap_or(0);
                SavedSearchResponse::new(item, alert_count)
            })
            .collect(),
        total,
        page,
        page_size,
        time_ranges: SAVED_SEARCH_TIME_RANGES
            .iter()
            .map(|range| (*range).to_string())
            .collect(),
    }))
}

/// Save a log search for a project.
#[utoipa::path(
    tag = "Logs",
    post,
    path = "/projects/{project_id}/logs/saved-searches",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = CreateSavedSearchRequest,
    responses(
        (status = 201, description = "Saved search created", body = SavedSearchResponse),
        (status = 400, description = "Validation error", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 409, description = "Name taken or limit reached", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_saved_search(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<LogAggregatorAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateSavedSearchRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LogsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let model = state
        .saved_search_service
        .create(
            project_id,
            NewSavedSearch {
                name: request.name,
                description: request.description,
                query: request.query,
                time_range: request.time_range.unwrap_or_else(|| "1h".to_string()),
                pinned: request.pinned,
                created_by: auth.user_id_opt(),
            },
        )
        .await?;

    let audit = SavedSearchCreatedAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        saved_search_id: model.id,
        name: model.name.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        tracing::error!(error = %e, "Failed to audit a saved log search creation");
    }

    Ok((
        StatusCode::CREATED,
        Json(SavedSearchResponse::new(model, 0)),
    ))
}

/// Read one saved log search.
#[utoipa::path(
    tag = "Logs",
    get,
    path = "/projects/{project_id}/logs/saved-searches/{saved_search_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("saved_search_id" = i32, Path, description = "Saved search ID")
    ),
    responses(
        (status = 200, description = "Saved search", body = SavedSearchResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Saved search not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_saved_search(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<LogAggregatorAppState>>,
    Path((project_id, saved_search_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LogsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let model = state
        .saved_search_service
        .get(project_id, saved_search_id)
        .await?;
    let alert_count = state
        .saved_search_service
        .alert_count(saved_search_id)
        .await?;
    Ok(Json(SavedSearchResponse::new(model, alert_count)))
}

/// Change a saved log search.
#[utoipa::path(
    tag = "Logs",
    patch,
    path = "/projects/{project_id}/logs/saved-searches/{saved_search_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("saved_search_id" = i32, Path, description = "Saved search ID")
    ),
    request_body = UpdateSavedSearchRequest,
    responses(
        (status = 200, description = "Saved search updated", body = SavedSearchResponse),
        (status = 400, description = "Validation error", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Saved search not found", body = ProblemDetails),
        (status = 409, description = "Name taken", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_saved_search(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<LogAggregatorAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, saved_search_id)): Path<(i32, i32)>,
    Json(request): Json<UpdateSavedSearchRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LogsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let model = state
        .saved_search_service
        .update(
            project_id,
            saved_search_id,
            SavedSearchUpdate {
                name: request.name,
                // An empty string clears the description; absent leaves it.
                description: request.description.map(|text| {
                    if text.trim().is_empty() {
                        None
                    } else {
                        Some(text)
                    }
                }),
                query: request.query,
                time_range: request.time_range,
                pinned: request.pinned,
            },
        )
        .await?;

    let audit = SavedSearchUpdatedAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        saved_search_id: model.id,
        name: model.name.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        tracing::error!(error = %e, "Failed to audit a saved log search update");
    }

    let alert_count = state
        .saved_search_service
        .alert_count(saved_search_id)
        .await?;
    Ok(Json(SavedSearchResponse::new(model, alert_count)))
}

/// Delete a saved log search. Refused while a log alert counts it.
#[utoipa::path(
    tag = "Logs",
    delete,
    path = "/projects/{project_id}/logs/saved-searches/{saved_search_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("saved_search_id" = i32, Path, description = "Saved search ID")
    ),
    responses(
        (status = 204, description = "Saved search deleted"),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Saved search not found", body = ProblemDetails),
        (status = 409, description = "Still used by a log alert", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_saved_search(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<LogAggregatorAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, saved_search_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LogsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    // Read the name before deleting it, so the audit entry says what went.
    let existing = state
        .saved_search_service
        .get(project_id, saved_search_id)
        .await?;
    state
        .saved_search_service
        .delete(project_id, saved_search_id)
        .await?;

    let audit = SavedSearchDeletedAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        saved_search_id,
        name: existing.name,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        tracing::error!(error = %e, "Failed to audit a saved log search deletion");
    }

    Ok(StatusCode::NO_CONTENT)
}

// ── Series (dashboard panels) ───────────────────────────────────────────

#[derive(Debug, Deserialize, IntoParams)]
pub struct SavedSearchSeriesParams {
    /// Window start, RFC 3339.
    #[param(value_type = String, format = DateTime)]
    pub start_time: DateTime<Utc>,
    /// Window end, RFC 3339.
    #[param(value_type = String, format = DateTime)]
    pub end_time: DateTime<Utc>,
    /// Bucket width in seconds. Default 60.
    pub bucket_secs: Option<u32>,
    /// `count` (default), or `avg`, `sum`, `max`, `p50`, `p95`, `p99` or
    /// `count_distinct` followed by `:<attribute>`, e.g. `p95:duration_ms`.
    pub metric: Option<String>,
}

/// Chart a saved log search over time: matching lines per bucket, or a
/// numeric attribute aggregated per bucket.
#[utoipa::path(
    tag = "Logs",
    get,
    path = "/projects/{project_id}/logs/saved-searches/{saved_search_id}/series",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("saved_search_id" = i32, Path, description = "Saved search ID"),
        SavedSearchSeriesParams
    ),
    responses(
        (status = 200, description = "The series", body = crate::services::LogSeries),
        (status = 400, description = "Invalid window, bucket or metric", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Saved search not found", body = ProblemDetails),
        (status = 503, description = "A numeric series needs the line index, which is unavailable", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn saved_search_series(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<LogAggregatorAppState>>,
    Path((project_id, saved_search_id)): Path<(i32, i32)>,
    Query(params): Query<SavedSearchSeriesParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LogsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let metric =
        crate::index::analytics::parse_metric(params.metric.as_deref().unwrap_or("count"))?;
    let query = state
        .saved_search_service
        .query_for(project_id, saved_search_id)
        .await?;
    let series = state
        .log_series_service
        .series(
            project_id,
            &query,
            crate::services::SeriesRequest {
                start: params.start_time,
                end: params.end_time,
                bucket_secs: params.bucket_secs.unwrap_or(60),
                metric,
            },
        )
        .await?;
    Ok(Json(series))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axum_test::TestServer;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_authz::{AuthContext, Role};

    use crate::chunk::cache::ChunkCache;
    use crate::index::NoLineIndex;
    use crate::services::{
        ChunkWriterService, IngestBudget, IngestLimits, LogMetadataService, LogSavedSearchService,
        LogSearchService, RetentionService, TailService,
    };
    use crate::storage::FilesystemStorage;
    use crate::store::chunk_store::ChunkStore;
    use crate::store::manifest::ManifestRepo;
    use crate::store::LogLineStore;

    struct NoopAudit;

    #[async_trait]
    impl temps_core::AuditLogger for NoopAudit {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::AuditOperation,
        ) -> temps_core::AuditResult<()> {
            Ok(())
        }
    }

    fn user() -> temps_entities::users::Model {
        temps_entities::users::Model {
            id: 1,
            name: "Test User".to_string(),
            email: "test@example.com".to_string(),
            password_hash: Some("hashed".to_string()),
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// App state over a mock database. Every test here asserts a decision
    /// the guards make *before* any service call, so the services only have
    /// to exist, not to answer.
    async fn state(
        db: sea_orm::DatabaseConnection,
    ) -> (Arc<LogAggregatorAppState>, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let db = Arc::new(db);
        let storage: Arc<dyn crate::storage::LogStorage> =
            Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).expect("filesystem storage"));
        let cache = ChunkCache::open(None, 8 * 1024 * 1024)
            .await
            .expect("cache");
        let chunk_writer = ChunkWriterService::open_deferred_with_index(
            storage.clone(),
            Arc::new(ManifestRepo::new(db.clone())),
            None,
            Some(cache.clone()),
            Arc::new(NoLineIndex::new("test")),
        )
        .await
        .expect("chunk writer");
        let store: Arc<dyn LogLineStore> = Arc::new(ChunkStore::new(
            ManifestRepo::new(db.clone()),
            storage,
            cache,
            chunk_writer.clone(),
        ));
        let metadata_service = Arc::new(LogMetadataService::new(db.clone()));
        let ingest_keys = Arc::new(crate::ingest::LogIngestKeyService::new(db.clone()));
        let chunk_writer_for_ingest = chunk_writer.clone();
        let saved_searches = Arc::new(LogSavedSearchService::new(db.clone()));
        let log_series_service = Arc::new(crate::services::LogSeriesService::new(
            saved_searches.clone(),
            store.clone(),
            Arc::new(NoLineIndex::new("test")),
            db.clone(),
        ));
        let state = Arc::new(LogAggregatorAppState {
            search_service: Arc::new(LogSearchService::new(
                store.clone(),
                metadata_service.clone(),
            )),
            metadata_service: metadata_service.clone(),
            tail_service: Arc::new(TailService::new(tokio::sync::broadcast::channel(16).0)),
            retention_service: Arc::new(RetentionService::new(
                Arc::new(ManifestRepo::new(db.clone())),
                metadata_service,
            )),
            audit_service: Arc::new(NoopAudit),
            store,
            db: db.clone(),
            project_access_checker: None,
            line_index: Arc::new(NoLineIndex::new("test")),
            manifests: Arc::new(ManifestRepo::new(db.clone())),
            chunk_writer,
            ingest_budget: Arc::new(IngestBudget::new(IngestLimits::UNLIMITED)),
            saved_search_service: saved_searches,
            log_series_service,
            ingest_service: Arc::new(crate::ingest::LogIngestService::new(
                ingest_keys.clone(),
                Arc::new(chunk_writer_for_ingest),
                Arc::new(IngestBudget::new(IngestLimits::UNLIMITED)),
            )),
            ingest_key_service: ingest_keys,
        });
        (state, tmp)
    }

    #[tokio::test]
    async fn every_saved_search_endpoint_requires_authentication() {
        let (state, _tmp) =
            state(MockDatabase::new(DatabaseBackend::Postgres).into_connection()).await;
        let server = TestServer::new(configure_routes().with_state(state));

        // No `AuthContext` extension is inserted, so `RequireAuth` rejects.
        let list = server.get("/projects/7/logs/saved-searches").await;
        assert_eq!(list.status_code(), StatusCode::UNAUTHORIZED);
        let create = server
            .post("/projects/7/logs/saved-searches")
            .json(&serde_json::json!({ "name": "errors" }))
            .await;
        assert_eq!(create.status_code(), StatusCode::UNAUTHORIZED);
        let get = server.get("/projects/7/logs/saved-searches/1").await;
        assert_eq!(get.status_code(), StatusCode::UNAUTHORIZED);
        let patch = server
            .patch("/projects/7/logs/saved-searches/1")
            .json(&serde_json::json!({ "pinned": true }))
            .await;
        assert_eq!(patch.status_code(), StatusCode::UNAUTHORIZED);
        let delete = server.delete("/projects/7/logs/saved-searches/1").await;
        assert_eq!(delete.status_code(), StatusCode::UNAUTHORIZED);
        let series = server
            .get("/projects/7/logs/saved-searches/1/series")
            .add_query_param("start_time", "2026-11-19T11:00:00Z")
            .add_query_param("end_time", "2026-11-19T12:00:00Z")
            .await;
        assert_eq!(series.status_code(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_reader_can_list_but_cannot_save_or_delete() {
        let (state, _tmp) = state(
            MockDatabase::new(DatabaseBackend::Postgres)
                // list(): count, then the page
                .append_query_results([vec![std::collections::BTreeMap::from([(
                    "num_items".to_string(),
                    sea_orm::Value::BigInt(Some(0)),
                )])]])
                .append_query_results([Vec::<Model>::new()])
                .into_connection(),
        )
        .await;
        let reader = AuthContext::new_session(user(), Role::Reader);
        let metadata = RequestMetadata {
            ip_address: "127.0.0.1".to_string(),
            user_agent: "test-agent".to_string(),
            headers: axum::http::HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://localhost".to_string(),
            scheme: "http".to_string(),
            host: "localhost".to_string(),
            is_secure: false,
        };

        // `logs:read` is enough to list.
        assert!(list_saved_searches(
            RequireAuth(reader.clone()),
            State(state.clone()),
            Path(7),
            Query(ListSavedSearchesParams {
                page: None,
                page_size: None,
            }),
        )
        .await
        .is_ok());

        // `logs:write` is not held by a reader, so saving is refused before
        // the service is reached.
        let created = create_saved_search(
            RequireAuth(reader.clone()),
            State(state.clone()),
            Extension(metadata.clone()),
            Path(7),
            Json(CreateSavedSearchRequest {
                name: "errors".to_string(),
                description: None,
                query: SavedSearchQuery::default(),
                time_range: None,
                pinned: false,
            }),
        )
        .await;
        assert_eq!(
            created.err().map(|p| p.status_code),
            Some(StatusCode::FORBIDDEN),
            "a reader must not be able to save a search"
        );

        let deleted = delete_saved_search(
            RequireAuth(reader),
            State(state),
            Extension(metadata),
            Path((7, 1)),
        )
        .await;
        assert_eq!(
            deleted.err().map(|p| p.status_code),
            Some(StatusCode::FORBIDDEN),
            "a reader must not be able to delete a search"
        );
    }

    #[tokio::test]
    async fn a_deployment_token_cannot_reach_another_projects_saved_searches() {
        let (state, _tmp) =
            state(MockDatabase::new(DatabaseBackend::Postgres).into_connection()).await;
        // A token bound to project 7, asking about project 8.
        let token = AuthContext::new_deployment_token(
            7,
            None,
            None,
            1,
            "deploy".to_string(),
            vec![temps_entities::deployment_tokens::DeploymentTokenPermission::AnalyticsRead],
        );
        let refused = get_saved_search(RequireAuth(token), State(state), Path((8, 1))).await;
        assert_eq!(
            refused.err().map(|p| p.status_code),
            Some(StatusCode::FORBIDDEN),
            "a deployment token is confined to its own project"
        );
    }

    #[test]
    fn a_response_reports_a_corrupt_query_as_empty_rather_than_losing_the_row() {
        let model = Model {
            id: 3,
            project_id: 7,
            name: "broken".to_string(),
            description: None,
            query: serde_json::json!({ "levels": 5 }),
            time_range: "1h".to_string(),
            pinned: true,
            created_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let response = SavedSearchResponse::new(model, 1);
        assert_eq!(response.query, SavedSearchQuery::default());
        assert_eq!(response.name, "broken");
        assert_eq!(response.alert_count, 1);
    }
}
