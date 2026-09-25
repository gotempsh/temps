// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP handlers for log aggregator endpoints

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt;
use utoipa::{OpenApi, ToSchema};

use axum::Extension;
use temps_auth::{permission_guard, project_access_guard, RequireAuth, Role};
use temps_core::problemdetails;
use temps_core::problemdetails::{Problem, ProblemDetails};
use temps_core::RequestMetadata;

use super::global::{
    facet_global_logs, global_log_aggregate, global_log_attribute_keys, global_log_capabilities,
    global_log_facets_attrs, global_log_histogram, search_global_logs, AggregateResponse,
    AttributeKeysResponse, FacetsAttrsResponse, HistogramResponse,
};
use crate::error::LogAggregatorError;
use crate::handlers::types::LogAggregatorAppState;
use crate::index::analytics::{
    AggregateRow, AttrOp, AttrPredicate, GroupKey, HistogramBucket, Metric,
};
use crate::services::global_search::{
    GlobalLogFacetsRequest, GlobalLogFacetsResponse, GlobalLogLine, GlobalLogSearchRequest,
    GlobalLogSearchResponse,
};
use crate::store::{
    resolve_log_access_scope, FacetField, FacetValue, LogAccessScope, LogSourceKind,
};
use crate::types::*;

// ── Error conversion ────────────────────────────────────────────────────

impl From<LogAggregatorError> for Problem {
    fn from(error: LogAggregatorError) -> Self {
        match error {
            LogAggregatorError::WalRecoveryIncomplete { .. } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("Log Recovery Incomplete")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::OperationTimedOut { .. } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("Log Operation Timed Out")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::ChunkNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Chunk Not Found")
                .with_detail(error.to_string()),
            LogAggregatorError::ContainerNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Container Not Found")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::SearchMissingRequiredParams => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Missing Required Parameters")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::SearchTimeRangeExceeded { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Time Range Exceeded")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::InvalidCursor { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Cursor")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::LineNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Log Line Not Found")
                .with_detail(error.to_string()),
            // Authorization could not be resolved. Refuse — never fall back to
            // an unfiltered query, and never to a silent empty result.
            LogAggregatorError::AccessResolutionFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Could not resolve log access")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::Validation { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Validation Error")
                .with_detail(error.to_string()),
            LogAggregatorError::Database(_) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Error")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::ChunkWriteFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Storage Write Failed")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::ChunkReadFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Storage Read Failed")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::ChunkDeleteFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Storage Delete Failed")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::ChunkListFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Storage List Failed")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::CompressionFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Compression Error")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::DecompressionFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Decompression Error")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::DockerStreamFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Docker Stream Error")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::ContainerContextLookupFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Container Lookup Failed")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::StorageConfiguration { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Storage Configuration Error")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::Io(_) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("IO Error")
                .with_detail(error.to_string()),
            LogAggregatorError::WalRecoveryReadFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("WAL Recovery Read Failed")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::Serialization(_) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Serialization Error")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::S3 { .. } => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("S3 Error")
                .with_detail(error.to_string()),
            // Not a 503: retrying never helps, because this process will never
            // grow a Docker daemon. It is a 409 with the same error code and
            // remedy every other endpoint returns for the condition — the
            // single mapping lives in `temps_core`.
            LogAggregatorError::DockerUnavailable(ref e) => Problem::from(e),
            LogAggregatorError::ChunkFormat { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Chunk Format Error")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::ManifestConflictUnresolved { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Manifest Conflict")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::LineIndex { .. } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("Log Index Unavailable")
                    .with_detail(error.to_string())
            }
        }
    }
}

// ── External-service access guard ───────────────────────────────────────

/// Guard log access for an external service by checking the caller's membership
/// in at least one of the projects that own the service (via `project_services`).
///
/// This is the external-service analogue of `project_access_guard!`.  It cannot
/// reuse that macro because a service may be linked to multiple projects, and the
/// correct semantic is "allow when the caller has access to **any** linked project"
/// (minimum bar) rather than the macro's single-project check.
///
/// Instance admins and creators of standalone services retain access. Deployment
/// tokens require a link to their bound project. With no `ProjectAccessChecker`,
/// session/API-key access follows the unrestricted OSS database access policy.
async fn guard_external_service_access(
    auth: &temps_auth::AuthContext,
    external_service_id: i32,
    project_ids: &[i32],
    created_by_user_id: Option<i32>,
    checker: &Option<Arc<dyn temps_core::ProjectAccessChecker>>,
) -> Result<(), Problem> {
    // A deployment token may only read services linked to its bound project.
    if auth.is_deployment_token() {
        return if auth
            .project_id()
            .is_some_and(|id| project_ids.contains(&id))
        {
            Ok(())
        } else {
            Err(problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Service Access Denied")
                .with_detail(format!(
                    "External service {external_service_id} is not linked to the token's project"
                )))
        };
    }
    // Instance administrators are never restricted by team membership.
    if auth.is_admin() || auth.has_role(&Role::PlatformAdmin) {
        return Ok(());
    }
    // Match database access: creators retain access to their standalone services.
    if project_ids.is_empty()
        && created_by_user_id.is_some()
        && auth.user_id_opt() == created_by_user_id
    {
        return Ok(());
    }
    // No checker registered → no-op (matches project_access_guard! behaviour).
    let Some(ref checker) = checker else {
        return Ok(());
    };
    // user_id_opt() is always Some for non-deployment-token auth, but be safe.
    let Some(user_id) = auth.user_id_opt() else {
        return Ok(());
    };
    // Allow when the user has access to any one of the linked projects.
    for &project_id in project_ids {
        match checker.user_can_access_project(user_id, project_id).await {
            Ok(true) => return Ok(()),
            Ok(false) => {} // try next project
            Err(e) => {
                tracing::error!(
                    external_service_id,
                    project_id,
                    user_id,
                    error = %e,
                    "ProjectAccessChecker infrastructure failure — denying access to \
                     external service logs"
                );
                return Err(temps_core::error_builder::ErrorBuilder::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                )
                .type_("https://temps.sh/probs/project-access-check-failed")
                .title("Project Access Check Failed")
                .detail("Could not verify project access; please try again")
                .build());
            }
        }
    }
    // The checker denied all linked projects.
    Err(
        temps_core::error_builder::ErrorBuilder::new(StatusCode::FORBIDDEN)
            .type_("https://temps.sh/probs/project-access-denied")
            .title("Project Access Denied")
            .detail("Your team membership does not include access to this resource")
            .build(),
    )
}

// ── Audit types ─────────────────────────────────────────────────────────

/// Audit event for log purge operations
#[derive(Debug, Clone, serde::Serialize)]
struct LogsPurgedAudit {
    pub context: temps_core::AuditContext,
    pub project_id: i32,
    pub before_timestamp: String,
    pub chunks_deleted: u64,
}

impl temps_core::AuditOperation for LogsPurgedAudit {
    fn operation_type(&self) -> String {
        "LOGS_PURGED".to_string()
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

    fn serialize(&self) -> temps_core::anyhow::Result<String> {
        Ok(serde_json::to_string(self)?)
    }
}

// ── Request/Response types ──────────────────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub struct SearchLogsRequest {
    /// Project ID (integer, as used by the rest of the platform)
    pub project_id: i32,
    /// When set, search an imported/managed external service's logs instead
    /// of a project's. `project_id` is ignored in this mode.
    #[serde(default)]
    pub external_service_id: Option<i32>,
    /// Start of time range (ISO 8601). Defaults to 1 hour ago.
    pub start_time: Option<String>,
    /// End of time range (ISO 8601). Defaults to now.
    pub end_time: Option<String>,
    /// Filter by log levels
    #[serde(default)]
    pub levels: Vec<String>,
    /// Filter by services
    #[serde(default)]
    pub services: Vec<String>,
    /// Filter by environments
    #[serde(default)]
    pub envs: Vec<String>,
    /// Filter to specific containers (Docker container IDs). Empty = all
    /// containers. Drives "filter by container / show all" in a project's
    /// history, which spans multiple deployments and containers.
    #[serde(default)]
    pub container_ids: Vec<String>,
    /// Filter to specific worker nodes (node_id). Empty = all nodes, including
    /// control-plane-local logs.
    #[serde(default)]
    pub node_ids: Vec<i32>,
    /// Filter by deployment ID (deployments.id)
    pub deploy_id: Option<i32>,
    /// Full text search query
    pub text: Option<String>,
    /// Pagination cursor
    pub cursor: Option<String>,
    /// Page size (default: 100, max: 500)
    pub page_size: Option<u32>,
    /// grep -C: number of raw context lines to include before and after each
    /// match (0 = none, default). Clamped to 50 server-side. The surrounding
    /// lines ignore the level/text filters — they are the actual adjacent log
    /// lines, merged across overlapping matches.
    pub context_lines: Option<u32>,
}

#[derive(Serialize, ToSchema)]
pub struct SearchLogsResponse {
    pub lines: Vec<LogSearchLine>,
    /// Opaque keyset cursor for the next (older) page. Stays populated on a
    /// partial page — that's the whole point: the user can press Next to keep
    /// searching.
    pub next_cursor: Option<String>,
    /// `true` when the store's time/byte budget ran out before this page
    /// could be proven complete.
    #[serde(default)]
    pub partial: bool,
    /// Set when `partial` is `true`: every chunk ending after this timestamp
    /// has been searched, nothing older has yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    pub scanned_back_to: Option<chrono::DateTime<Utc>>,
    /// Distinct containers/nodes/services available in the queried scope, for
    /// the filter dropdowns. Populated on the first page (no cursor).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available_sources: Vec<LogSource>,
}

#[derive(Deserialize, ToSchema)]
pub struct ContextLogsRequest {
    #[schema(value_type = String)]
    pub timestamp: chrono::DateTime<Utc>,
    pub container_id: String,
    /// Decimal string form of the target line's `line_id`.
    pub line_id: String,
    /// Number of context lines before and after (default: 25, max 50)
    pub lines: Option<u32>,
}

#[derive(Serialize, ToSchema)]
pub struct ContextLogsResponse {
    pub lines: Vec<ContextLine>,
    pub target_index: usize,
}

#[derive(Deserialize, ToSchema)]
pub struct TailLogsRequest {
    /// Project ID (integer, as used by the rest of the platform)
    pub project_id: i32,
    /// When set, tail an imported/managed external service's logs instead of
    /// a project's (`project_id` is ignored in this mode).
    #[serde(default)]
    pub external_service_id: Option<i32>,
    pub service: String,
    pub env: String,
    #[serde(default)]
    pub levels: Vec<String>,
    pub text: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct PurgeLogsRequest {
    /// Delete all logs before this timestamp (ISO 8601)
    pub before: String,
}

// ── OpenAPI Doc ─────────────────────────────────────────────────────────

#[derive(OpenApi)]
#[openapi(
    paths(
        super::global::search_global_logs,
        super::global::facet_global_logs,
        super::global::global_log_capabilities,
        super::global::global_log_attribute_keys,
        super::global::global_log_facets_attrs,
        super::global::global_log_histogram,
        super::global::global_log_aggregate,
        search_logs,
        get_log_context,
        tail_logs,
        purge_project_logs
    ),
    components(
        schemas(
            GlobalLogSearchRequest,
            GlobalLogSearchResponse,
            GlobalLogFacetsRequest,
            GlobalLogFacetsResponse,
            super::global::GlobalLogCapabilities,
            super::global::AnalyticsCapability,
            super::global::LogCollectionCapability,
            super::global::LogCollectionState,
            super::global::DeferredWalGeneration,
            AttributeKeysResponse,
            FacetsAttrsResponse,
            HistogramResponse,
            AggregateResponse,
            AttrPredicate,
            AttrOp,
            GroupKey,
            Metric,
            HistogramBucket,
            AggregateRow,
            GlobalLogLine,
            LogSourceKind,
            FacetField,
            FacetValue,
            SearchLogsRequest,
            SearchLogsResponse,
            ContextLogsRequest,
            ContextLogsResponse,
            TailLogsRequest,
            PurgeLogsRequest,
            LogSearchLine,
            LogSource,
            ContextLine,
            LineContext,
            LogLevel,
            LogStream,
        )
    ),
    info(
        title = "Log Aggregator API",
        description = "API endpoints for searching, streaming, and managing application logs.",
        version = "1.0.0"
    ),
    tags(
        (name = "Logs", description = "Log search, context, live tail, and retention management")
    )
)]
pub struct LogAggregatorApiDoc;

// ── Routes ──────────────────────────────────────────────────────────────

pub fn configure_routes() -> Router<Arc<LogAggregatorAppState>> {
    Router::new()
        .route("/logs/search", post(search_logs))
        .route("/logs/global/search", post(search_global_logs))
        .route("/logs/global/facets", post(facet_global_logs))
        .route("/logs/global/capabilities", get(global_log_capabilities))
        .route("/logs/global/attributes", get(global_log_attribute_keys))
        .route("/logs/global/facets/attrs", get(global_log_facets_attrs))
        .route("/logs/global/histogram", get(global_log_histogram))
        .route("/logs/global/aggregate", get(global_log_aggregate))
        .route("/logs/context", get(get_log_context))
        .route("/logs/tail", get(tail_logs))
        .route("/projects/{project_id}/logs", delete(purge_project_logs))
}

// ── Handlers ────────────────────────────────────────────────────────────

/// Search logs with structured filters and full text search
#[utoipa::path(
    tag = "Logs",
    post,
    path = "/logs/search",
    request_body = SearchLogsRequest,
    responses(
        (status = 200, description = "Search results", body = SearchLogsResponse),
        (status = 400, description = "Invalid search parameters", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn search_logs(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<LogAggregatorAppState>>,
    Json(request): Json<SearchLogsRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LogsRead);
    // When `external_service_id` is set, `project_id` is ignored by the search
    // engine; the resource being accessed is the external service itself.
    // Resolve its owning project(s) and check team-based access against those.
    // When only `project_id` is set, guard on it directly as usual.
    let scope = match request.external_service_id {
        None => {
            project_access_guard!(auth, request.project_id, app_state.project_access_checker);
            // The guard above has already decided the caller may read this
            // project. Hand the store that decision as an explicit
            // one-element allow-list, so every read path shares one
            // authorization shape.
            LogAccessScope::Allowed {
                project_ids: vec![request.project_id],
                external_service_ids: vec![],
            }
        }
        Some(service_id) => {
            let service_scope = app_state
                .metadata_service
                .find_external_service_scope(service_id)
                .await?
                .ok_or_else(|| {
                    problemdetails::new(StatusCode::NOT_FOUND)
                        .with_title("External Service Not Found")
                        .with_detail(format!("External service {service_id} does not exist"))
                })?;
            guard_external_service_access(
                &auth,
                service_id,
                &service_scope.project_ids,
                service_scope.created_by_user_id,
                &app_state.project_access_checker,
            )
            .await?;
            LogAccessScope::Allowed {
                project_ids: vec![],
                external_service_ids: vec![service_id],
            }
        }
    };

    let now = Utc::now();
    let start_time = request
        .start_time
        .as_ref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or(now - Duration::hours(1));
    let end_time = request
        .end_time
        .as_ref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or(now);

    let levels: Vec<LogLevel> = request
        .levels
        .iter()
        .filter_map(|l| LogLevel::parse(l))
        .collect();

    let filter = LogSearchFilter {
        project_id: request.project_id,
        external_service_id: request.external_service_id,
        start_time,
        end_time,
        levels,
        services: request.services,
        envs: request.envs,
        container_ids: request.container_ids,
        node_ids: request.node_ids,
        deploy_id: request.deploy_id,
        text: request.text,
        cursor: request.cursor,
        page_size: request.page_size.unwrap_or(crate::store::DEFAULT_PAGE_SIZE),
        context_lines: request.context_lines.unwrap_or(0),
    };

    let result = app_state.search_service.search(&filter, &scope).await?;

    Ok(Json(SearchLogsResponse {
        lines: result.lines,
        next_cursor: result.next_cursor,
        partial: result.partial,
        scanned_back_to: result.scanned_back_to,
        available_sources: result.available_sources,
    }))
}

/// Get context lines surrounding a specific log line
#[utoipa::path(
    tag = "Logs",
    get,
    path = "/logs/context",
    params(
        ("timestamp" = String, Query, description = "Target line timestamp (RFC 3339)"),
        ("container_id" = String, Query, description = "Target line container ID"),
        ("line_id" = String, Query, description = "Target line_id, as a decimal string"),
        ("lines" = Option<u32>, Query, description = "Context lines before and after (default: 25, max 50)")
    ),
    responses(
        (status = 200, description = "Context lines", body = ContextLogsResponse),
        (status = 400, description = "Invalid parameters", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 404, description = "Line not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn get_log_context(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<LogAggregatorAppState>>,
    Query(request): Query<ContextLogsRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LogsRead);
    // Context reaches raw neighbouring lines, so it is guarded by the
    // caller's full allow-list: a line you could not have found through
    // search is a line you cannot reach through its neighbours either.
    let scope = resolve_log_access_scope(
        app_state.db.as_ref(),
        &auth,
        app_state.project_access_checker.as_ref(),
    )
    .await
    .map_err(|error| LogAggregatorError::AccessResolutionFailed {
        reason: error.to_string(),
    })?;

    let context_req = ContextRequest {
        timestamp: request.timestamp,
        container_id: request.container_id,
        line_id: request.line_id,
        lines: request.lines.unwrap_or(25),
    };

    let result = app_state
        .search_service
        .get_context(&context_req, &scope)
        .await?;

    Ok(Json(ContextLogsResponse {
        lines: result.lines,
        target_index: result.target_index,
    }))
}

/// Live tail logs via Server-Sent Events
#[utoipa::path(
    tag = "Logs",
    get,
    path = "/logs/tail",
    params(
        ("project_id" = String, Query, description = "Project ID"),
        ("service" = String, Query, description = "Service name"),
        ("env" = String, Query, description = "Environment"),
        ("levels" = Vec<String>, Query, description = "Optional level filters"),
        ("text" = Option<String>, Query, description = "Optional text filter")
    ),
    responses(
        (status = 200, description = "SSE stream of log lines"),
        (status = 401, description = "Unauthorized", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn tail_logs(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<LogAggregatorAppState>>,
    Query(request): Query<TailLogsRequest>,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>, Problem> {
    permission_guard!(auth, LogsRead);
    // When `external_service_id` is set, `project_id` is ignored by the tail
    // service; the resource being accessed is the external service itself.
    // Resolve its owning project(s) and check team-based access against those.
    // When only `project_id` is set, guard on it directly as usual.
    match request.external_service_id {
        None => {
            project_access_guard!(auth, request.project_id, app_state.project_access_checker);
        }
        Some(service_id) => {
            let scope = app_state
                .metadata_service
                .find_external_service_scope(service_id)
                .await?
                .ok_or_else(|| {
                    problemdetails::new(StatusCode::NOT_FOUND)
                        .with_title("External Service Not Found")
                        .with_detail(format!("External service {service_id} does not exist"))
                })?;
            guard_external_service_access(
                &auth,
                service_id,
                &scope.project_ids,
                scope.created_by_user_id,
                &app_state.project_access_checker,
            )
            .await?;
        }
    }

    let levels: Vec<LogLevel> = request
        .levels
        .iter()
        .filter_map(|l| LogLevel::parse(l))
        .collect();

    let filter = TailFilter {
        project_id: request.project_id,
        external_service_id: request.external_service_id,
        service: request.service,
        env: request.env,
        levels,
        text: request.text,
    };

    let stream = app_state.tail_service.subscribe(filter);

    // Auto-close after 30 minutes of inactivity
    let stream = tokio_stream::StreamExt::timeout(stream, std::time::Duration::from_secs(1800));

    let event_stream = stream.map(|result| {
        match result {
            Ok(line) => {
                let data = serde_json::to_string(&line).unwrap_or_default();
                Ok(Event::default().data(data))
            }
            Err(_timeout) => {
                // Stream closed due to inactivity timeout
                Ok(Event::default().comment("timeout"))
            }
        }
    });

    Ok(Sse::new(event_stream).keep_alive(KeepAlive::default()))
}

/// Purge all logs for a project before a given timestamp
#[utoipa::path(
    tag = "Logs",
    delete,
    path = "/projects/{project_id}/logs",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
    ),
    request_body = PurgeLogsRequest,
    responses(
        (status = 200, description = "Purge completed"),
        (status = 400, description = "Invalid parameters", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn purge_project_logs(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<LogAggregatorAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<PurgeLogsRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LogsDelete);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    let before = chrono::DateTime::parse_from_rfc3339(&request.before)
        .map_err(|_| LogAggregatorError::Validation {
            message: format!("Invalid timestamp: {}", request.before),
        })?
        .with_timezone(&Utc);

    // `RetentionService::manual_purge` and `LogLineStore::purge_project` both
    // tombstone the same `log_chunks` manifest rows (ADR-046 merged the old
    // chunk-archive table and the line-search manifest into one). Calling
    // both would make the second call a no-op that silently under-reports —
    // so the line count is read from the same candidate set `manual_purge`
    // is about to tombstone, not from a second, redundant tombstone call.
    let expired = app_state
        .metadata_service
        .find_expired_chunks(project_id, before)
        .await?;
    let lines_deleted: u64 = expired.iter().map(|c| c.line_count as u64).sum();

    let result = app_state
        .retention_service
        .manual_purge(project_id, before)
        .await?;

    // Audit logging for the destructive purge operation
    let audit = LogsPurgedAudit {
        context: temps_core::AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        before_timestamp: request.before,
        chunks_deleted: result.chunks_deleted,
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        tracing::error!("Failed to create audit log for log purge: {}", e);
    }

    Ok(Json(serde_json::json!({
        "lines_deleted": lines_deleted,
        "chunks_deleted": result.chunks_deleted,
        "chunks_failed": result.chunks_failed,
        "bytes_reclaimed": result.bytes_reclaimed
    })))
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn background_recovery_incomplete_returns_contextual_unavailable_problem() {
        use axum::response::IntoResponse;
        let problem: Problem = LogAggregatorError::WalRecoveryIncomplete {
            path: "logs/wal/example.b.recovery-wal".to_owned(),
        }
        .into();
        let response = problem.into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(body["detail"]
            .as_str()
            .unwrap()
            .contains("example.b.recovery-wal"));
    }

    #[tokio::test]
    async fn operation_timeout_returns_service_unavailable_problem() {
        use axum::response::IntoResponse;
        let problem: Problem = LogAggregatorError::OperationTimedOut {
            operation: "prepare purge",
            target: "project 7".to_owned(),
        }
        .into();
        let response = problem.into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response.headers()["content-type"],
            "application/problem+json"
        );
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let detail: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(detail["detail"].as_str().unwrap().contains("project 7"));
    }

    use super::*;
    use async_trait::async_trait;
    use axum::extract::Request;
    use axum::middleware;
    use axum_test::TestServer;
    use chrono::{Duration, Utc};
    use sea_orm::ConnectionTrait;
    use std::sync::Arc;
    use temps_database::test_utils::TestDatabase;
    use uuid::Uuid;

    use std::sync::atomic::{AtomicI32, Ordering};

    use crate::chunk::cache::ChunkCache;
    use crate::services::{
        ChunkWriterService, LogMetadataService, LogSearchService, RetentionService, TailService,
    };
    use crate::storage::FilesystemStorage;
    use crate::store::chunk_store::ChunkStore;
    use crate::store::manifest::ManifestRepo;
    use crate::store::LogLineStore;
    use crate::types::{LogLevel, LogLine, LogStream};

    /// Atomic counter for unique test project IDs (avoids cross-test collision)
    static TEST_PROJECT_COUNTER: AtomicI32 = AtomicI32::new(10_000);

    fn next_test_project_id() -> i32 {
        TEST_PROJECT_COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    // ── Mock audit logger ───────────────────────────────────────────────

    #[derive(Clone)]
    struct MockAuditLogger;

    #[async_trait]
    impl temps_core::AuditLogger for MockAuditLogger {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::AuditOperation,
        ) -> Result<(), temps_core::anyhow::Error> {
            Ok(())
        }
    }

    // ── Test context ────────────────────────────────────────────────────

    struct TestContext {
        app_state: Arc<LogAggregatorAppState>,
        chunk_writer: Arc<ChunkWriterService>,
        tail_tx: tokio::sync::broadcast::Sender<LogLine>,
        _db: TestDatabase,
        _tmp_dir: tempfile::TempDir,
    }

    /// Create a full test context with real DB, real filesystem storage, and all services wired up.
    async fn create_test_context() -> TestContext {
        let db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database (is Docker running?)");

        let tmp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");
        let storage: Arc<dyn crate::storage::LogStorage> = Arc::new(
            FilesystemStorage::new(tmp_dir.path().to_path_buf())
                .expect("Failed to create filesystem storage"),
        );

        let metadata_service = Arc::new(LogMetadataService::new(db.connection_arc()));
        let cache = ChunkCache::open(None, 64 * 1024 * 1024)
            .await
            .expect("Failed to open chunk cache");
        let chunk_writer = ChunkWriterService::open(
            storage.clone(),
            Arc::new(ManifestRepo::new(db.connection_arc())),
            None,
            None,
        )
        .await
        .expect("Failed to open chunk writer");
        let store: Arc<dyn LogLineStore> = Arc::new(ChunkStore::new(
            ManifestRepo::new(db.connection_arc()),
            storage.clone(),
            cache,
            chunk_writer.clone(),
        ));
        let search_service = Arc::new(LogSearchService::new(
            store.clone(),
            metadata_service.clone(),
        ));
        let (tail_tx, _) = tokio::sync::broadcast::channel::<LogLine>(1024);
        let tail_service = Arc::new(TailService::new(tail_tx.clone()));
        let retention_service = Arc::new(
            RetentionService::new(
                Arc::new(ManifestRepo::new(db.connection_arc())),
                metadata_service.clone(),
            )
            .with_chunk_writer(chunk_writer.clone()),
        );
        let audit_service = Arc::new(MockAuditLogger) as Arc<dyn temps_core::AuditLogger>;

        let app_state = Arc::new(LogAggregatorAppState {
            search_service,
            metadata_service: metadata_service.clone(),
            tail_service,
            retention_service,
            audit_service,
            store,
            db: db.connection_arc(),
            project_access_checker: None,
            line_index: Arc::new(crate::index::NoLineIndex::new("test")),
            manifests: Arc::new(crate::store::manifest::ManifestRepo::new(
                db.connection_arc(),
            )),
            chunk_writer: chunk_writer.clone(),
        });

        TestContext {
            app_state,
            chunk_writer,
            tail_tx,
            _db: db,
            _tmp_dir: tmp_dir,
        }
    }

    /// Helper to create a mock AuthContext for testing
    fn create_test_auth_context() -> temps_auth::AuthContext {
        let user = temps_entities::users::Model {
            id: 1,
            name: "Test User".to_string(),
            email: "test@example.com".to_string(),
            password_hash: Some("hashed_password".to_string()),
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
        };
        temps_auth::AuthContext::new_session(user, temps_auth::Role::Admin)
    }

    /// Build a TestServer with auth middleware that injects AuthContext and RequestMetadata.
    fn build_test_server(app_state: Arc<LogAggregatorAppState>) -> TestServer {
        build_test_server_with_role(app_state, temps_auth::Role::Admin)
    }

    /// Build a TestServer with a specific role for permission testing.
    fn build_test_server_with_role(
        app_state: Arc<LogAggregatorAppState>,
        role: temps_auth::Role,
    ) -> TestServer {
        let auth_middleware =
            middleware::from_fn(move |mut req: Request, next: axum::middleware::Next| {
                let role = role.clone();
                async move {
                    let user = temps_entities::users::Model {
                        id: 1,
                        name: "Test User".to_string(),
                        email: "test@example.com".to_string(),
                        password_hash: Some("hashed_password".to_string()),
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
                    };
                    let auth_context = temps_auth::AuthContext::new_session(user, role);
                    req.extensions_mut().insert(auth_context);
                    req.extensions_mut().insert(temps_core::RequestMetadata {
                        ip_address: "127.0.0.1".to_string(),
                        user_agent: "test-agent".to_string(),
                        headers: axum::http::HeaderMap::new(),
                        visitor_id_cookie: None,
                        session_id_cookie: None,
                        base_url: "http://localhost".to_string(),
                        scheme: "http".to_string(),
                        host: "localhost".to_string(),
                        is_secure: false,
                    });
                    next.run(req).await
                }
            });

        let app = configure_routes()
            .layer(auth_middleware)
            .with_state(app_state);

        TestServer::new(app)
    }

    /// Create a test log line with the given parameters.
    fn make_log_line(
        project_id: i32,
        service: &str,
        env: &str,
        level: LogLevel,
        msg: &str,
        ts: chrono::DateTime<Utc>,
        container_id: &str,
    ) -> LogLine {
        LogLine {
            ts,
            stream: LogStream::Stdout,
            level,
            msg: msg.to_string(),
            fields: None,
            container_id: container_id.to_string(),
            service: service.to_string(),
            env: env.to_string(),
            project_id,
            external_service_id: None,
            deploy_id: None,
            node_id: None,
            node_name: None,
        }
    }

    /// Seed log lines into storage and the manifest via the chunk writer.
    ///
    /// The writer owns the whole seal pipeline itself (object write +
    /// manifest insert), so writing the lines and then removing the
    /// container (which seals its head buffer) is enough to make them
    /// visible to the store.
    async fn seed_logs(ctx: &TestContext, lines: Vec<LogLine>) {
        if lines.is_empty() {
            return;
        }

        let container_id = lines[0].container_id.clone();

        for line in &lines {
            ctx.chunk_writer
                .write_line(line.clone())
                .await
                .expect("Failed to write log line");
        }

        ctx.chunk_writer
            .remove_container(&container_id)
            .await
            .expect("Failed to seal container buffer");
    }

    // ── Tests ───────────────────────────────────────────────────────────

    #[tokio::test]
    #[serial_test::serial]
    async fn test_search_logs_returns_results() {
        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let now = Utc::now();

        // Seed some log lines
        let lines = vec![
            make_log_line(
                project_id,
                "web",
                "prod",
                LogLevel::Error,
                "Database connection failed",
                now - Duration::minutes(5),
                "container-1",
            ),
            make_log_line(
                project_id,
                "web",
                "prod",
                LogLevel::Warn,
                "High memory usage detected",
                now - Duration::minutes(4),
                "container-1",
            ),
            make_log_line(
                project_id,
                "web",
                "prod",
                LogLevel::Info,
                "Request processed successfully",
                now - Duration::minutes(3),
                "container-1",
            ),
        ];
        // Leave these lines in the live head buffer. Search exposes heads with
        // synthetic line IDs, which is the production path that previously
        // survived a successful purge until the normal age-based seal.
        for line in lines {
            ctx.chunk_writer
                .write_line(line)
                .await
                .expect("write live head line");
        }

        let server = build_test_server(ctx.app_state.clone());

        let before_purge = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(3)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
            }))
            .await;
        let before_body: serde_json::Value = before_purge.json();
        // All 3 lines are unsealed in the live head buffer, within the search
        // window, and no level filter is set on the request (an empty
        // `levels` means "all levels" — see `level_mask_for`), so all 3 are
        // expected back, not a subset.
        assert_eq!(before_body["lines"].as_array().map(Vec::len), Some(3));
        assert!(before_body["lines"]
            .as_array()
            .expect("lines")
            .iter()
            .all(|line| line["line_id"]
                .as_str()
                .and_then(|value| value.parse::<i64>().ok())
                .is_some_and(|line_id| line_id >= crate::store::HEAD_LINE_ID_BASE)));

        let response = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let body: serde_json::Value = response.json();
        let lines_arr = body["lines"].as_array().expect("lines should be an array");
        assert!(
            !lines_arr.is_empty(),
            "Expected at least one log line in search results"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_search_logs_empty_project() {
        let ctx = create_test_context().await;
        let project_id = next_test_project_id(); // No logs seeded

        let server = build_test_server(ctx.app_state.clone());
        let now = Utc::now();

        let response = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let body: serde_json::Value = response.json();
        let lines_arr = body["lines"].as_array().expect("lines should be an array");
        assert!(
            lines_arr.is_empty(),
            "Expected no log lines for empty project, got {}",
            lines_arr.len()
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_search_logs_filters_by_level() {
        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let now = Utc::now();

        let lines = vec![
            make_log_line(
                project_id,
                "api",
                "prod",
                LogLevel::Error,
                "Fatal error occurred",
                now - Duration::minutes(5),
                "container-2",
            ),
            make_log_line(
                project_id,
                "api",
                "prod",
                LogLevel::Info,
                "Health check passed",
                now - Duration::minutes(4),
                "container-2",
            ),
            make_log_line(
                project_id,
                "api",
                "prod",
                LogLevel::Warn,
                "Slow query detected",
                now - Duration::minutes(3),
                "container-2",
            ),
        ];
        seed_logs(&ctx, lines).await;

        let server = build_test_server(ctx.app_state.clone());

        let response = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
                "levels": ["ERROR"],
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let body: serde_json::Value = response.json();
        let lines_arr = body["lines"].as_array().expect("lines should be an array");

        // All returned lines should be ERROR level
        for line in lines_arr {
            assert_eq!(
                line["level"].as_str().unwrap(),
                "ERROR",
                "Expected only ERROR level lines when filtering by ERROR"
            );
        }
        assert!(
            !lines_arr.is_empty(),
            "Expected at least one ERROR line in results"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_search_logs_filters_by_service() {
        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let now = Utc::now();

        // Seed logs for two different services in separate containers
        let web_lines = vec![make_log_line(
            project_id,
            "web",
            "prod",
            LogLevel::Error,
            "Web error",
            now - Duration::minutes(5),
            "container-web",
        )];
        let worker_lines = vec![make_log_line(
            project_id,
            "worker",
            "prod",
            LogLevel::Error,
            "Worker error",
            now - Duration::minutes(4),
            "container-worker",
        )];
        seed_logs(&ctx, web_lines).await;
        seed_logs(&ctx, worker_lines).await;

        let server = build_test_server(ctx.app_state.clone());

        let response = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
                "services": ["web"],
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let body: serde_json::Value = response.json();
        let lines_arr = body["lines"].as_array().expect("lines should be an array");

        for line in lines_arr {
            assert_eq!(
                line["service"].as_str().unwrap(),
                "web",
                "Expected only 'web' service lines when filtering by service"
            );
        }
        assert!(
            !lines_arr.is_empty(),
            "Expected at least one line for 'web' service"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_search_logs_fulltext_search() {
        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let now = Utc::now();

        let lines = vec![
            make_log_line(
                project_id,
                "api",
                "prod",
                LogLevel::Error,
                "Connection refused to database at port 5432",
                now - Duration::minutes(5),
                "container-ft",
            ),
            make_log_line(
                project_id,
                "api",
                "prod",
                LogLevel::Error,
                "Timeout waiting for response",
                now - Duration::minutes(4),
                "container-ft",
            ),
        ];
        seed_logs(&ctx, lines).await;

        let server = build_test_server(ctx.app_state.clone());

        let response = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
                "text": "Connection refused",
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let body: serde_json::Value = response.json();
        let lines_arr = body["lines"].as_array().expect("lines should be an array");
        assert!(
            !lines_arr.is_empty(),
            "Expected at least one line matching 'Connection refused'"
        );
        assert!(
            lines_arr[0]["message"]
                .as_str()
                .unwrap()
                .contains("Connection refused"),
            "Matched line should contain the search text"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_get_log_context() {
        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let now = Utc::now();

        // Seed enough lines to have meaningful context
        let mut lines = Vec::new();
        for i in 0..20 {
            lines.push(make_log_line(
                project_id,
                "api",
                "prod",
                if i == 10 {
                    LogLevel::Error
                } else {
                    LogLevel::Info
                },
                &format!("Log line number {}", i),
                now - Duration::seconds(20 - i),
                "container-ctx",
            ));
        }
        seed_logs(&ctx, lines).await;

        // First, search to get a line identity to ask for context around.
        let server = build_test_server(ctx.app_state.clone());

        let search_response = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
            }))
            .await;

        assert_eq!(search_response.status_code(), StatusCode::OK);

        let search_body: serde_json::Value = search_response.json();
        let search_lines = search_body["lines"].as_array().unwrap();
        assert!(
            !search_lines.is_empty(),
            "Need search results to get a line identity"
        );

        let target = &search_lines[search_lines.len() / 2];
        let timestamp = target["timestamp"].as_str().unwrap().to_string();
        let container_id = target["container_id"].as_str().unwrap().to_string();
        let line_id = target["line_id"].as_str().unwrap().to_string();

        // Now request context around that line, keyed on its position.
        let context_response = server
            .get(&format!(
                "/logs/context?timestamp={}&container_id={}&line_id={}&lines=5",
                urlencoding(&timestamp),
                container_id,
                line_id
            ))
            .await;

        assert_eq!(context_response.status_code(), StatusCode::OK);

        let context_body: serde_json::Value = context_response.json();
        let context_lines = context_body["lines"].as_array().unwrap();
        assert!(
            !context_lines.is_empty(),
            "Expected context lines around the target"
        );
        let target_index = context_body["target_index"].as_u64().unwrap() as usize;
        assert_eq!(
            context_lines[target_index]["line_id"].as_str().unwrap(),
            line_id,
            "target_index must point at the line that was asked for"
        );
        assert!(
            context_lines[target_index]["is_match"].as_bool().unwrap(),
            "the target line must be flagged as the match"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_get_log_context_missing_line_is_not_found() {
        let ctx = create_test_context().await;
        let server = build_test_server(ctx.app_state.clone());

        // A well-formed key that points at nothing must be a 404, NOT an
        // empty 200 — "that line is gone" and "here is its context" are
        // different answers the caller must be able to tell apart.
        let response = server
            .get(&format!(
                "/logs/context?timestamp={}&container_id=container-missing&line_id=1&lines=5",
                urlencoding(&Utc::now().to_rfc3339())
            ))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::NOT_FOUND,
            "Expected 404 for a line that does not exist, got {}",
            response.status_code()
        );
    }

    /// Minimal percent-encoding for the RFC 3339 timestamps these tests put
    /// in a query string (`+` would otherwise decode as a space).
    fn urlencoding(value: &str) -> String {
        value
            .chars()
            .map(|c| match c {
                '+' => "%2B".to_string(),
                ':' => "%3A".to_string(),
                other => other.to_string(),
            })
            .collect()
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_purge_project_logs() {
        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let now = Utc::now();

        // Seed logs that we'll purge
        let lines = vec![
            make_log_line(
                project_id,
                "api",
                "prod",
                LogLevel::Error,
                "Old error log",
                now - Duration::hours(2),
                "container-purge",
            ),
            make_log_line(
                project_id,
                "api",
                "prod",
                LogLevel::Info,
                "Old info log",
                now - Duration::hours(2) + Duration::seconds(1),
                "container-purge",
            ),
        ];
        seed_logs(&ctx, lines).await;

        let server = build_test_server(ctx.app_state.clone());

        // Purge all logs before "now" (should delete the seeded logs)
        let purge_response = server
            .delete(&format!("/projects/{}/logs", project_id))
            .json(&serde_json::json!({
                "before": now.to_rfc3339(),
            }))
            .await;

        assert_eq!(purge_response.status_code(), StatusCode::OK);

        let purge_body: serde_json::Value = purge_response.json();
        assert!(
            purge_body["chunks_deleted"].as_u64().unwrap() >= 1,
            "Expected at least 1 chunk deleted"
        );

        // Verify logs are gone by searching again
        let search_response = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(3)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
            }))
            .await;

        assert_eq!(search_response.status_code(), StatusCode::OK);
        let search_body: serde_json::Value = search_response.json();
        let remaining_lines = search_body["lines"].as_array().unwrap();
        assert!(
            remaining_lines.is_empty(),
            "Expected no logs after purge, found {}",
            remaining_lines.len()
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn failed_tombstone_keeps_cutoff_open_for_delayed_ingest() {
        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let now = Utc::now();
        ctx.chunk_writer
            .write_line(make_log_line(
                project_id,
                "api",
                "prod",
                LogLevel::Error,
                "original sensitive line",
                now - Duration::hours(2),
                "container-failed-purge",
            ))
            .await
            .expect("write original head");

        ctx._db
            .db
            .execute_unprepared(
                "CREATE FUNCTION reject_log_chunk_tombstone() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected tombstone failure'; END $$; \
                 CREATE TRIGGER reject_log_chunk_tombstone BEFORE UPDATE OF deleted_at ON log_chunks FOR EACH ROW EXECUTE FUNCTION reject_log_chunk_tombstone();",
            )
            .await
            .expect("install tombstone failure trigger");

        let server = build_test_server(ctx.app_state.clone());
        let response = server
            .delete(&format!("/projects/{project_id}/logs"))
            .json(&serde_json::json!({ "before": now.to_rfc3339() }))
            .await;
        assert_eq!(response.status_code(), StatusCode::OK);
        let body: serde_json::Value = response.json();
        assert_eq!(body["chunks_deleted"].as_u64(), Some(0));
        assert_eq!(body["chunks_failed"].as_u64(), Some(1));

        // Docker may deliver a buffered line after the failed purge returns.
        // It must remain accepted because the destructive boundary did not
        // commit every selected manifest.
        ctx.chunk_writer
            .write_line(make_log_line(
                project_id,
                "api",
                "prod",
                LogLevel::Info,
                "delayed line after failed purge",
                now - Duration::hours(1),
                "container-delayed-after-failure",
            ))
            .await
            .expect("failed purge must not suppress delayed ingest");

        let search = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(3)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
            }))
            .await;
        assert_eq!(search.status_code(), StatusCode::OK);
        let search_body: serde_json::Value = search.json();
        let messages: Vec<&str> = search_body["lines"]
            .as_array()
            .expect("search lines")
            .iter()
            .filter_map(|line| line["message"].as_str())
            .collect();
        assert!(messages.contains(&"original sensitive line"));
        assert!(messages.contains(&"delayed line after failed purge"));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_purge_invalid_timestamp() {
        let ctx = create_test_context().await;
        let server = build_test_server(ctx.app_state.clone());

        let response = server
            .delete(&format!("/projects/{}/logs", next_test_project_id()))
            .json(&serde_json::json!({
                "before": "not-a-valid-timestamp",
            }))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::BAD_REQUEST,
            "Expected 400 for invalid timestamp, got {}",
            response.status_code()
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_tail_logs_sse() {
        use tokio::net::TcpListener;

        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let tail_tx = ctx.tail_tx.clone();

        // SSE endpoints stream indefinitely so we can't use axum-test's .await
        // (it waits for the full response body). Instead, bind to a real port,
        // connect with a raw HTTP client, send a log line via broadcast, and
        // read the first SSE event with a timeout.
        let auth_middleware = middleware::from_fn(
            |mut req: Request, next: axum::middleware::Next| async move {
                let auth_context = create_test_auth_context();
                req.extensions_mut().insert(auth_context);
                req.extensions_mut().insert(temps_core::RequestMetadata {
                    ip_address: "127.0.0.1".to_string(),
                    user_agent: "test-agent".to_string(),
                    headers: axum::http::HeaderMap::new(),
                    visitor_id_cookie: None,
                    session_id_cookie: None,
                    base_url: "http://localhost".to_string(),
                    scheme: "http".to_string(),
                    host: "localhost".to_string(),
                    is_secure: false,
                });
                next.run(req).await
            },
        );

        let app = configure_routes()
            .layer(auth_middleware)
            .with_state(ctx.app_state.clone());

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind");
        let addr = listener.local_addr().unwrap();

        let server_handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // Give the server a moment to start
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Connect with a raw TCP stream and send an HTTP GET
        let url = format!(
            "http://127.0.0.1:{}/logs/tail?project_id={}&service=web&env=prod",
            addr.port(),
            project_id,
        );

        let client = reqwest::Client::new();
        let response = client
            .get(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .expect("Failed to connect to SSE endpoint");

        assert_eq!(response.status(), 200);

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            content_type.contains("text/event-stream"),
            "Expected text/event-stream content type, got: {}",
            content_type
        );

        // Send a log line through the broadcast channel
        let log_line = make_log_line(
            project_id,
            "web",
            "prod",
            LogLevel::Info,
            "Tail test message",
            Utc::now(),
            "container-tail",
        );
        let send_result = tail_tx.send(log_line);
        assert!(
            send_result.is_ok(),
            "Failed to send log line to broadcast channel"
        );

        // Read the first chunk of the SSE response body with a timeout
        let body = tokio::time::timeout(std::time::Duration::from_secs(3), response.text()).await;

        // We either get the SSE data or timeout — both are acceptable.
        // The key assertions are the 200 status and content-type above.
        if let Ok(Ok(text)) = body {
            assert!(
                text.contains("Tail test message"),
                "Expected SSE event to contain 'Tail test message', got: {}",
                &text[..std::cmp::min(200, text.len())]
            );
        }

        server_handle.abort();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_search_logs_with_pagination() {
        // Regression test: pagination via next_cursor must walk into older
        // logs without overlap. Previously the server emitted next_cursor
        // but never honored filter.cursor on the way back in, so every
        // "next page" click returned page 1 again.
        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let now = Utc::now();

        let mut lines = Vec::new();
        for i in 0..15 {
            lines.push(make_log_line(
                project_id,
                "api",
                "prod",
                LogLevel::Error,
                &format!("Error event {}", i),
                now - Duration::seconds(15 - i),
                "container-page",
            ));
        }
        seed_logs(&ctx, lines).await;

        let server = build_test_server(ctx.app_state.clone());

        let page1 = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
                "page_size": 5,
            }))
            .await;
        assert_eq!(page1.status_code(), StatusCode::OK);
        let page1_body: serde_json::Value = page1.json();
        let page1_lines = page1_body["lines"].as_array().unwrap();
        assert_eq!(page1_lines.len(), 5);
        let page1_cursor = page1_body["next_cursor"]
            .as_str()
            .expect("page 1 must emit a cursor when more matches exist (15 total, 5 per page)");

        let page2 = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
                "page_size": 5,
                "cursor": page1_cursor,
            }))
            .await;
        assert_eq!(page2.status_code(), StatusCode::OK);
        let page2_body: serde_json::Value = page2.json();
        let page2_lines = page2_body["lines"].as_array().unwrap();
        assert_eq!(page2_lines.len(), 5, "page 2 should have 5 more lines");

        // No overlap: every page-2 message is distinct from every page-1 message.
        let p1_msgs: std::collections::HashSet<&str> = page1_lines
            .iter()
            .map(|l| l["message"].as_str().unwrap())
            .collect();
        for l in page2_lines {
            let m = l["message"].as_str().unwrap();
            assert!(
                !p1_msgs.contains(m),
                "page 2 leaked a page-1 line: {} (cursor was ignored)",
                m,
            );
        }

        // Page 2 lines are strictly older than every page-1 line.
        let p1_oldest = page1_lines
            .iter()
            .map(|l| l["timestamp"].as_str().unwrap())
            .min()
            .unwrap();
        for l in page2_lines {
            let ts = l["timestamp"].as_str().unwrap();
            assert!(
                ts < p1_oldest,
                "page 2 line {} is not older than page-1 oldest {}",
                ts,
                p1_oldest,
            );
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_search_logs_returns_chronological_order() {
        // The page is bounded server-side to the *newest* page_size matches in
        // the time window (heap-kept), but the wire format is ASC so the UI
        // can render terminal-style with newest at the bottom. "Chronological"
        // here means oldest first inside the page; "load older" pages pre-
        // pend at the top of the rendered rope.
        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let now = Utc::now();

        let mut lines = Vec::new();
        for i in 0..10 {
            lines.push(make_log_line(
                project_id,
                "api",
                "prod",
                LogLevel::Info,
                &format!("event {}", i),
                now - Duration::seconds(10 - i),
                "container-order",
            ));
        }
        seed_logs(&ctx, lines).await;

        let server = build_test_server(ctx.app_state.clone());
        let response = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
                "page_size": 100,
            }))
            .await;

        let body: serde_json::Value = response.json();
        let arr = body["lines"].as_array().unwrap();
        assert_eq!(arr.len(), 10);

        // Each subsequent line must be strictly newer (ASC ordering).
        for w in arr.windows(2) {
            let a = w[0]["timestamp"].as_str().unwrap();
            let b = w[1]["timestamp"].as_str().unwrap();
            assert!(
                a <= b,
                "results not in ASC order: {} appeared before {}",
                a,
                b,
            );
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_search_logs_wider_window_returns_more_recent_logs() {
        // Regression test: the early-exit `break` after page_size matches
        // ran chunks oldest-first, so a "Last 6 hours" query and a "Last 1
        // hour" query returned essentially the same lines from the start
        // of the window. The bigger window expansion was wasted because
        // the scanner stopped reading once it had enough, and "enough"
        // came from the oldest chunks.
        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let now = Utc::now();

        // Seed 10 lines spread across 4 hours: half at -3h30m, half at -30m.
        let mut lines = Vec::new();
        for i in 0..10 {
            let offset = if i < 5 {
                Duration::minutes(-210)
            } else {
                Duration::minutes(-30)
            };
            lines.push(make_log_line(
                project_id,
                "api",
                "prod",
                LogLevel::Info,
                &format!("evt-{}", i),
                now + offset + Duration::seconds(i),
                "container-window",
            ));
        }
        seed_logs(&ctx, lines).await;

        let server = build_test_server(ctx.app_state.clone());

        // Last 1 hour: only the -30m batch is in window (5 lines).
        let r1h = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::minutes(1)).to_rfc3339(),
                "page_size": 100,
            }))
            .await;
        let body_1h: serde_json::Value = r1h.json();
        let len_1h = body_1h["lines"].as_array().unwrap().len();
        assert_eq!(
            len_1h, 5,
            "Last-1h window should hold exactly the -30m batch"
        );

        // Last 4 hours: both batches are in window (10 lines). If the early-exit
        // bug were still present, this would also return ~5.
        let r4h = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(4)).to_rfc3339(),
                "end_time": (now + Duration::minutes(1)).to_rfc3339(),
                "page_size": 100,
            }))
            .await;
        let body_4h: serde_json::Value = r4h.json();
        let len_4h = body_4h["lines"].as_array().unwrap().len();
        assert_eq!(
            len_4h, 10,
            "Last-4h window should expand to include both batches",
        );
    }

    #[tokio::test]
    async fn test_unauthenticated_request_fails() {
        // Build server WITHOUT auth middleware — RequireAuth should fail
        let ctx = create_test_context().await;

        let app = configure_routes().with_state(ctx.app_state.clone());
        let server = TestServer::new(app);
        let now = Utc::now();

        let response = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": 99999,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
            }))
            .await;

        // Without auth context in extensions, RequireAuth should reject with 401
        assert_eq!(
            response.status_code(),
            StatusCode::UNAUTHORIZED,
            "Expected 401 without auth, got {}",
            response.status_code()
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_reader_cannot_purge_logs() {
        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let now = Utc::now();

        // Seed some logs so purge has something to target
        let lines = vec![make_log_line(
            project_id,
            "api",
            "prod",
            LogLevel::Error,
            "Error to purge",
            now - Duration::hours(1),
            "container-perm",
        )];
        seed_logs(&ctx, lines).await;

        // Build server with Reader role (has LogsRead but NOT LogsDelete)
        let server = build_test_server_with_role(ctx.app_state.clone(), temps_auth::Role::Reader);

        let response = server
            .delete(&format!("/projects/{}/logs", project_id))
            .json(&serde_json::json!({
                "before": now.to_rfc3339(),
            }))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::FORBIDDEN,
            "Reader should get 403 on purge, got {}",
            response.status_code()
        );
    }

    // ── External-service IDOR regression tests ─────────────────────────

    /// Mock `ProjectAccessChecker` that allows access only for project IDs in
    /// the `allowed` list.  Used to simulate team-based project access control
    /// without needing a real database-backed implementation.
    #[derive(Clone)]
    struct MockProjectAccessChecker {
        allowed: Vec<i32>,
    }

    #[async_trait]
    impl temps_core::ProjectAccessChecker for MockProjectAccessChecker {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(self.allowed.contains(&project_id))
        }
    }

    /// Seed the full FK chain required for a `project_services` row:
    ///
    /// 1. Insert an `external_services` row (the service whose logs we guard).
    /// 2. Insert a `projects` row (the owning project).
    /// 3. Insert a `project_services` row linking them.
    ///
    /// Returns `(service_id, project_id)` using the DB-assigned auto-increment
    /// IDs so there are no FK violations.
    async fn seed_external_service_with_project(db: &sea_orm::DatabaseConnection) -> (i32, i32) {
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::preset::Preset;

        let svc = temps_entities::external_services::ActiveModel {
            name: Set(format!("test-ext-svc-{}", Uuid::new_v4())),
            service_type: Set("postgres".to_string()),
            status: Set("pending".to_string()),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("Failed to seed external_services row");

        let proj = temps_entities::projects::ActiveModel {
            name: Set("Test Project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            slug: Set(format!("test-proj-{}", Uuid::new_v4())),
            preset: Set(Preset::NextJs),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("Failed to seed projects row");

        temps_entities::project_services::ActiveModel {
            project_id: Set(proj.id),
            service_id: Set(svc.id),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("Failed to seed project_services row");

        (svc.id, proj.id)
    }

    /// Build a server where the authenticated user has `Role::User` (has
    /// `LogsRead`) AND a `ProjectAccessChecker` is active.  Tests that
    /// exercise the external-service IDOR guard need both conditions: a
    /// non-admin user (so the admin bypass does not fire) and a registered
    /// checker (so the guard actually calls `user_can_access_project`).
    fn build_server_with_user_and_checker(
        base_state: &Arc<LogAggregatorAppState>,
        checker: Arc<dyn temps_core::ProjectAccessChecker>,
    ) -> TestServer {
        let app_state = Arc::new(LogAggregatorAppState {
            search_service: base_state.search_service.clone(),
            metadata_service: base_state.metadata_service.clone(),
            tail_service: base_state.tail_service.clone(),
            retention_service: base_state.retention_service.clone(),
            audit_service: base_state.audit_service.clone(),
            store: base_state.store.clone(),
            db: base_state.db.clone(),
            project_access_checker: Some(checker),
            line_index: base_state.line_index.clone(),
            manifests: base_state.manifests.clone(),
            chunk_writer: base_state.chunk_writer.clone(),
        });
        build_test_server_with_role(app_state, temps_auth::Role::User)
    }

    #[tokio::test]
    async fn test_external_service_access_standalone_policy() {
        let admin = create_test_auth_context();
        let user =
            temps_auth::AuthContext::new_session(admin.require_user().unwrap().clone(), Role::User);
        let checker: Option<Arc<dyn temps_core::ProjectAccessChecker>> =
            Some(Arc::new(MockProjectAccessChecker { allowed: vec![7] }));
        // Administrators can aggregate logs from services without a creator/link.
        assert!(
            guard_external_service_access(&admin, 42, &[], None, &checker)
                .await
                .is_ok()
        );
        // A creator can read their standalone database with team checks active.
        assert!(
            guard_external_service_access(&user, 42, &[], Some(1), &checker)
                .await
                .is_ok()
        );
        assert!(
            guard_external_service_access(&user, 42, &[], Some(2), &checker)
                .await
                .is_err()
        );
        assert!(
            guard_external_service_access(&user, 42, &[], None, &checker)
                .await
                .is_err()
        );
        // OSS follows the same unrestricted policy as database access.
        assert!(guard_external_service_access(&user, 42, &[], None, &None)
            .await
            .is_ok());
        // Creator ownership does not bypass linked-project access.
        assert!(
            guard_external_service_access(&user, 42, &[8], Some(1), &checker)
                .await
                .is_err()
        );
        assert!(
            guard_external_service_access(&user, 42, &[8, 7], None, &checker)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn test_external_service_access_deployment_token_requires_link() {
        let token = temps_auth::AuthContext::new_deployment_token(
            7,
            None,
            None,
            1,
            "test-token".into(),
            vec![],
        );
        assert!(guard_external_service_access(&token, 42, &[], None, &None)
            .await
            .is_err());
        assert!(guard_external_service_access(&token, 42, &[8], None, &None)
            .await
            .is_err());
        assert!(guard_external_service_access(&token, 42, &[7], None, &None)
            .await
            .is_ok());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_search_external_service_standalone_returns_logs() {
        use sea_orm::{ActiveModelTrait, Set};
        let ctx = create_test_context().await;
        let service = temps_entities::external_services::ActiveModel {
            name: Set(format!("standalone-svc-{}", Uuid::new_v4())),
            service_type: Set("postgres".into()),
            status: Set("running".into()),
            ..Default::default()
        }
        .insert(ctx._db.db.as_ref())
        .await
        .unwrap();
        let now = Utc::now();
        let mut line = make_log_line(
            0,
            "postgres",
            "",
            LogLevel::Info,
            "database system is ready to accept connections",
            now,
            "standalone-db",
        );
        line.external_service_id = Some(service.id);
        seed_logs(&ctx, vec![line]).await;
        let server = build_test_server(ctx.app_state.clone());
        let response = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": 0,
                "external_service_id": service.id,
                "start_time": (now - Duration::minutes(1)).to_rfc3339(),
                "end_time": (now + Duration::minutes(1)).to_rfc3339(),
            }))
            .await;
        response.assert_status_ok();
        let body: serde_json::Value = response.json();
        assert_eq!(body["lines"].as_array().unwrap().len(), 1);
        assert_eq!(
            body["lines"][0]["message"],
            "database system is ready to accept connections"
        );
        // A missing service is still distinguished from a valid standalone one.
        let missing = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": 0, "external_service_id": -1,
            }))
            .await;
        missing.assert_status_not_found();
    }

    /// (a) A user WITH access to the external service's owning project can
    /// search its logs (200).
    #[tokio::test]
    #[serial_test::serial]
    async fn test_search_external_service_allowed_project_access() {
        let ctx = create_test_context().await;
        let (service_id, owning_project) =
            seed_external_service_with_project(ctx._db.db.as_ref()).await;

        let checker = Arc::new(MockProjectAccessChecker {
            allowed: vec![owning_project],
        });
        let server = build_server_with_user_and_checker(&ctx.app_state, checker);

        let now = Utc::now();
        let response = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": 0,          // ignored when external_service_id is set
                "external_service_id": service_id,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
            }))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::OK,
            "user with access to the owning project must be allowed; body: {}",
            response.text()
        );
    }

    /// (b) A user WITHOUT access to the owning project is denied (403) even
    /// though they hold `LogsRead`.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_search_external_service_denied_project_access() {
        let ctx = create_test_context().await;
        let (service_id, owning_project) =
            seed_external_service_with_project(ctx._db.db.as_ref()).await;

        // Checker allows a completely different project — NOT the one that
        // owns this service — so access must be denied.
        let different_project = owning_project + 1_000_000;
        let checker = Arc::new(MockProjectAccessChecker {
            allowed: vec![different_project],
        });
        let server = build_server_with_user_and_checker(&ctx.app_state, checker);

        let now = Utc::now();
        let response = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": 0,
                "external_service_id": service_id,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
            }))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::FORBIDDEN,
            "user without access to the owning project must be denied 403; body: {}",
            response.text()
        );
    }

    /// A standalone service remains inaccessible to unrelated users when team
    /// access is configured, even if their checker allows other projects.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_search_external_service_standalone_denied_unrelated_user() {
        let ctx = create_test_context().await;
        // Seed an external_services row but deliberately do NOT create a
        // project_services row linking it to any project.
        use sea_orm::{ActiveModelTrait, Set};
        let orphaned_svc = temps_entities::external_services::ActiveModel {
            name: Set(format!("orphaned-svc-{}", Uuid::new_v4())),
            service_type: Set("postgres".to_string()),
            status: Set("pending".to_string()),
            ..Default::default()
        }
        .insert(ctx._db.db.as_ref())
        .await
        .expect("Failed to seed orphaned external_services row");

        // Access to unrelated projects does not grant standalone service access.
        let checker = Arc::new(MockProjectAccessChecker {
            allowed: vec![1, 2, 3, 9999],
        });
        let server = build_server_with_user_and_checker(&ctx.app_state, checker);

        let now = Utc::now();
        let response = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": 0,
                "external_service_id": orphaned_svc.id,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
            }))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::FORBIDDEN,
            "standalone service must deny unrelated users; \
             body: {}",
            response.text()
        );
    }

    /// Verify the guard is also wired into `tail_logs`.  We only test the
    /// access-denied path here (not SSE streaming); a 403 returned before the
    /// stream is created is a plain HTTP response that axum-test handles correctly.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_tail_external_service_denied_project_access() {
        let ctx = create_test_context().await;
        let (service_id, _owning_project) =
            seed_external_service_with_project(ctx._db.db.as_ref()).await;

        // Checker denies all projects — proves the guard fires on tail_logs too.
        let checker = Arc::new(MockProjectAccessChecker { allowed: vec![] });
        let server = build_server_with_user_and_checker(&ctx.app_state, checker);

        let response = server
            .get(&format!(
                "/logs/tail?project_id=0&external_service_id={}&service=web&env=prod",
                service_id
            ))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::FORBIDDEN,
            "tail_logs must also enforce project access for external_service_id; body: {}",
            response.text()
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_reader_can_search_logs() {
        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let now = Utc::now();

        let lines = vec![make_log_line(
            project_id,
            "api",
            "prod",
            LogLevel::Info,
            "Readable log",
            now,
            "container-read",
        )];
        seed_logs(&ctx, lines).await;

        // Build server with Reader role (has LogsRead)
        let server = build_test_server_with_role(ctx.app_state.clone(), temps_auth::Role::Reader);

        let response = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
            }))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::OK,
            "Reader should be able to search logs, got {}",
            response.status_code()
        );
    }

    // ── ADR-047 §5 read-side analytics handlers ────────────────────────

    #[tokio::test]
    #[serial_test::serial]
    async fn test_attribute_keys_reports_503_without_a_line_index() {
        // This harness wires `NoLineIndex::new("test")`, matching every
        // instance that has not configured ClickHouse: the read-side
        // analytics endpoints must fail loudly with the reason, never
        // silently return an empty answer.
        let ctx = create_test_context().await;
        let server = build_test_server(ctx.app_state.clone());
        let now = Utc::now();

        let response = server
            .get(&format!(
                "/logs/global/attributes?start_time={}&end_time={}",
                urlencoding(&(now - Duration::hours(1)).to_rfc3339()),
                urlencoding(&now.to_rfc3339()),
            ))
            .await;

        assert_eq!(response.status_code(), StatusCode::SERVICE_UNAVAILABLE);
        let body: serde_json::Value = response.json();
        assert!(
            body["detail"].as_str().unwrap_or_default().contains("test"),
            "expected the NoLineIndex reason in the problem detail: {body}"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_facets_attrs_reports_503_without_a_line_index() {
        let ctx = create_test_context().await;
        let server = build_test_server(ctx.app_state.clone());
        let now = Utc::now();

        let response = server
            .get(&format!(
                "/logs/global/facets/attrs?start_time={}&end_time={}&keys=env,attr:worker",
                urlencoding(&(now - Duration::hours(1)).to_rfc3339()),
                urlencoding(&now.to_rfc3339()),
            ))
            .await;

        assert_eq!(response.status_code(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_facets_attrs_rejects_malformed_attr_predicate() {
        let ctx = create_test_context().await;
        let server = build_test_server(ctx.app_state.clone());
        let now = Utc::now();

        let response = server
            .get(&format!(
                "/logs/global/facets/attrs?start_time={}&end_time={}&keys=env&attr=not-a-predicate",
                urlencoding(&(now - Duration::hours(1)).to_rfc3339()),
                urlencoding(&now.to_rfc3339()),
            ))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::BAD_REQUEST,
            "a malformed attr predicate must be rejected before it ever reaches the index, \
             got: {}",
            response.text()
        );
    }

    /// Repeated query keys (`levels=…&levels=…`, `attr=…&attr=…`) are how
    /// list filters arrive on the GET analytics endpoints. axum's own
    /// `Query` rejects them ("expected a sequence"); the handlers use
    /// `axum_extra`'s, so the request must get past deserialisation — here
    /// all the way to the 503 the unconfigured index answers with.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_analytics_endpoints_accept_repeated_list_query_params() {
        let ctx = create_test_context().await;
        let server = build_test_server(ctx.app_state.clone());
        let now = Utc::now();
        let window = format!(
            "start_time={}&end_time={}",
            urlencoding(&(now - Duration::hours(1)).to_rfc3339()),
            urlencoding(&now.to_rfc3339()),
        );

        for path in [
            format!("/logs/global/histogram?{window}&levels=ERROR&levels=WARN&services=api&services=web&attr=worker%3D3&attr=cache%3F"),
            format!("/logs/global/aggregate?{window}&group_by=service&metric=count&attr=worker%3D3&attr=upstream%3F"),
            format!("/logs/global/facets/attrs?{window}&keys=env&envs=prod&envs=staging&attr=worker%21%3D3"),
        ] {
            let response = server.get(&path).await;
            assert_eq!(
                response.status_code(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{path}: repeated list params must deserialise and reach the index, got: {}",
                response.text()
            );
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_facets_attrs_rejects_unknown_group_key() {
        let ctx = create_test_context().await;
        let server = build_test_server(ctx.app_state.clone());
        let now = Utc::now();

        let response = server
            .get(&format!(
                "/logs/global/facets/attrs?start_time={}&end_time={}&keys=not-a-real-field",
                urlencoding(&(now - Duration::hours(1)).to_rfc3339()),
                urlencoding(&now.to_rfc3339()),
            ))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_histogram_reports_503_without_a_line_index() {
        let ctx = create_test_context().await;
        let server = build_test_server(ctx.app_state.clone());
        let now = Utc::now();

        let response = server
            .get(&format!(
                "/logs/global/histogram?start_time={}&end_time={}&bucket_secs=60&group_by=service",
                urlencoding(&(now - Duration::hours(1)).to_rfc3339()),
                urlencoding(&now.to_rfc3339()),
            ))
            .await;

        assert_eq!(response.status_code(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_aggregate_reports_503_without_a_line_index() {
        let ctx = create_test_context().await;
        let server = build_test_server(ctx.app_state.clone());
        let now = Utc::now();

        let response = server
            .get(&format!(
                "/logs/global/aggregate?start_time={}&end_time={}&group_by=service&metric=count",
                urlencoding(&(now - Duration::hours(1)).to_rfc3339()),
                urlencoding(&now.to_rfc3339()),
            ))
            .await;

        assert_eq!(response.status_code(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_aggregate_rejects_malformed_metric() {
        let ctx = create_test_context().await;
        let server = build_test_server(ctx.app_state.clone());
        let now = Utc::now();

        let response = server
            .get(&format!(
                "/logs/global/aggregate?start_time={}&end_time={}&group_by=service&metric=bogus",
                urlencoding(&(now - Duration::hours(1)).to_rfc3339()),
                urlencoding(&now.to_rfc3339()),
            ))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_global_search_rejects_malformed_attr() {
        // The extended global search endpoint parses `attrs` before it ever
        // touches the store or the line index, same as the read-only
        // analytics endpoints.
        let ctx = create_test_context().await;
        let server = build_test_server(ctx.app_state.clone());
        let now = Utc::now();

        let response = server
            .post("/logs/global/search")
            .json(&serde_json::json!({
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
                "attrs": ["no-operator-here"],
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_global_search_with_attrs_reports_503_without_a_line_index() {
        let ctx = create_test_context().await;
        let server = build_test_server(ctx.app_state.clone());
        let now = Utc::now();

        let response = server
            .post("/logs/global/search")
            .json(&serde_json::json!({
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
                "attrs": ["status_code>499"],
            }))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::SERVICE_UNAVAILABLE,
            "an attr-filtered search must go through the line index and fail the same way \
             the analytics endpoints do when it is unavailable; got: {}",
            response.text()
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_global_search_without_attrs_is_unaffected() {
        // The plain path (no `attrs`) must keep working exactly as before —
        // it never touches the line index, so it must succeed even when
        // that index is unavailable.
        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let now = Utc::now();

        seed_logs(
            &ctx,
            vec![make_log_line(
                project_id,
                "web",
                "prod",
                LogLevel::Info,
                "Plain global search still works",
                now,
                "container-plain-global",
            )],
        )
        .await;

        let server = build_test_server(ctx.app_state.clone());

        let response = server
            .post("/logs/global/search")
            .json(&serde_json::json!({
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
            }))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::OK,
            "{}",
            response.text()
        );
    }
}
