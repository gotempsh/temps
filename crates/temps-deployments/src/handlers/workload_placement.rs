// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Admin API for deployments on the workload plane (ADR-055 Phase 5):
//! an environment's `workload_mode` and `workload_isolation` (Phase 10),
//! each node's resource policy and
//! system reserve (node 0 is the control plane), the scheduler's read-only
//! explanation for an environment, and the pinning recommendation.
//!
//! Every route is instance-admin: reads need `SettingsRead`, writes
//! `SettingsWrite` (held by admins only), and every write is audited with
//! the previous and new value.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::{
    problemdetails::{self, Problem, ProblemDetails},
    AuditContext, AuditLogger, RequestMetadata,
};
use temps_entities::environment_workload_isolation::EnvironmentWorkloadIsolation;
use temps_entities::environment_workload_mode::EnvironmentWorkloadMode;
use temps_entities::node_resource_settings::{NodeResourcePolicy, SystemReserved};
use tracing::error;
use utoipa::{IntoParams, OpenApi, ToSchema};

use crate::handlers::audit::{
    EnvironmentWorkloadIsolationChangedAudit, EnvironmentWorkloadModeChangedAudit,
    NodeResourcesChangedAudit,
};
use crate::services::workload_placement::{
    NodeResourceSettings, NodeResourceUpdate, WorkloadPlacementError, WorkloadPlacementService,
    DEFAULT_RECOMMENDATION_WINDOW_DAYS,
};

pub struct WorkloadPlacementAppState {
    pub service: Arc<WorkloadPlacementService>,
    pub audit_service: Arc<dyn AuditLogger>,
}

impl From<WorkloadPlacementError> for Problem {
    fn from(error: WorkloadPlacementError) -> Self {
        let (status, title) = match &error {
            WorkloadPlacementError::EnvironmentNotFound { .. } => {
                (StatusCode::NOT_FOUND, "Environment Not Found")
            }
            WorkloadPlacementError::ProjectNotFound { .. } => {
                (StatusCode::NOT_FOUND, "Project Not Found")
            }
            WorkloadPlacementError::NodeNotFound { .. } => {
                (StatusCode::NOT_FOUND, "Node Not Found")
            }
            WorkloadPlacementError::Validation { .. } => {
                (StatusCode::BAD_REQUEST, "Invalid Placement Settings")
            }
            WorkloadPlacementError::Database { .. } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Placement Storage Failed",
            ),
            WorkloadPlacementError::Node { .. } => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Reading Nodes Failed")
            }
            WorkloadPlacementError::Settings { .. } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Control Plane Settings Failed",
            ),
        };
        problemdetails::new(status)
            .with_title(title)
            .with_detail(error.to_string())
    }
}

// ── DTOs ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkloadModeResponse {
    pub environment_id: i32,
    pub workload_mode: EnvironmentWorkloadMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetWorkloadModeRequest {
    /// `legacy` (today's deploy path), `shadow` (legacy, plus a logged
    /// unified-scheduler comparison) or `enforce` (control-plane replicas run
    /// through the workload plane). `legacy` is the rollback.
    pub workload_mode: EnvironmentWorkloadMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkloadIsolationResponse {
    pub environment_id: i32,
    pub workload_isolation: EnvironmentWorkloadIsolation,
    /// Whether the setting takes effect: only `enforce` deployments run
    /// through the plane, so `legacy` and `shadow` keep a container.
    pub applies: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetWorkloadIsolationRequest {
    /// `process` (a container, the default and the rollback) or `micro_vm`
    /// (a Firecracker microVM; placed only on a node whose runtime provides
    /// it, refused with the scheduler's reason otherwise). Honoured only
    /// when the environment's `workload_mode` is `enforce`.
    pub workload_isolation: EnvironmentWorkloadIsolation,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NodeResourcesResponse {
    /// `0` is the control plane.
    pub node_id: i32,
    pub resource_policy: NodeResourcePolicy,
    pub system_reserved: SystemReserved,
    /// Whether `system_reserved` is the default for the node's role. The
    /// defaults are ADR-055 §5 estimates, not measurements.
    pub system_reserved_is_default: bool,
}

impl From<NodeResourceSettings> for NodeResourcesResponse {
    fn from(settings: NodeResourceSettings) -> Self {
        Self {
            node_id: settings.node_id,
            resource_policy: settings.resource_policy,
            system_reserved: settings.system_reserved,
            system_reserved_is_default: settings.system_reserved_is_default,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct SetNodeResourcesRequest {
    /// `best_effort` (default: reservations are not checked) or `enforced`
    /// (a placement that does not fit the schedulable capacity is refused).
    #[serde(default)]
    pub resource_policy: Option<NodeResourcePolicy>,
    /// What to keep for the OS and Temps itself; deducted before the load
    /// threshold and, on an enforced node, before reservations.
    #[serde(default)]
    pub system_reserved: Option<SystemReserved>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct NodeRecommendationDto {
    pub node_id: i32,
    pub node_name: String,
    pub score: f64,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct NodeExclusionDto {
    pub node_id: i32,
    pub node_name: String,
    /// The `Scheduled` condition reason, e.g. `NodeDraining`.
    pub reason: String,
    pub message: String,
    /// `false` when the node was kept with a warning.
    pub excluded: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PlacementExplanationResponse {
    pub environment_id: i32,
    /// Eligible nodes, best first.
    pub recommended: Vec<NodeRecommendationDto>,
    pub excluded: Vec<NodeExclusionDto>,
    /// Where the unified scheduler would put each replica now (`0` = the
    /// control plane).
    pub placement: Option<Vec<i32>>,
    pub unschedulable: Option<String>,
    pub unschedulable_reason: Option<String>,
    pub warnings: Vec<String>,
    /// Why the environment is pinned by its volumes, e.g. "pinned to node 0
    /// by volume `data`" (ADR-055 §12). `null` without attached volumes.
    pub pinned_by: Option<String>,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct RecommendationQuery {
    /// History window in days (1-90, default 7).
    pub window_days: Option<u32>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PinningRecommendationResponse {
    pub environment_id: i32,
    pub window_days: u32,
    /// Distinct nodes the environment ran on in the window.
    pub nodes_used: Vec<i32>,
    /// Unpinned and moved between nodes: show the recommendation.
    pub moved_between_nodes: bool,
    /// The recommended `target_nodes`. Recommended, never applied.
    pub recommended_nodes: Vec<i32>,
    pub reasons: Vec<String>,
    pub current_nodes: Vec<i32>,
    pub changes_placement: bool,
}

fn audit_context(auth: &temps_auth::AuthContext, metadata: &RequestMetadata) -> AuditContext {
    AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

// ── Handlers ────────────────────────────────────────────────────────────────

#[utoipa::path(
    tag = "Workload Placement", get,
    path = "/internal/environments/{environment_id}/workload-mode",
    operation_id = "admin_get_environment_workload_mode",
    params(("environment_id" = i32, Path, description = "Environment ID")),
    responses(
        (status = 200, body = WorkloadModeResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Environment not found", body = ProblemDetails),
        (status = 500, description = "Storage failed", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn admin_get_environment_workload_mode(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkloadPlacementAppState>>,
    Path(environment_id): Path<i32>,
) -> Result<Json<WorkloadModeResponse>, Problem> {
    permission_guard!(auth, SettingsRead);
    let workload_mode = state
        .service
        .environment_workload_mode(environment_id)
        .await?;
    Ok(Json(WorkloadModeResponse {
        environment_id,
        workload_mode,
    }))
}

#[utoipa::path(
    tag = "Workload Placement", put,
    path = "/internal/environments/{environment_id}/workload-mode",
    operation_id = "admin_set_environment_workload_mode",
    params(("environment_id" = i32, Path, description = "Environment ID")),
    request_body = SetWorkloadModeRequest,
    responses(
        (status = 200, body = WorkloadModeResponse),
        (status = 400, description = "Invalid mode", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Instance admin required", body = ProblemDetails),
        (status = 404, description = "Environment not found", body = ProblemDetails),
        (status = 500, description = "Storage failed", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn admin_set_environment_workload_mode(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkloadPlacementAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(environment_id): Path<i32>,
    Json(request): Json<SetWorkloadModeRequest>,
) -> Result<Json<WorkloadModeResponse>, Problem> {
    permission_guard!(auth, SettingsWrite);
    let previous = state
        .service
        .set_environment_workload_mode(environment_id, request.workload_mode)
        .await?;
    let audit = EnvironmentWorkloadModeChangedAudit {
        context: audit_context(&auth, &metadata),
        environment_id,
        previous_mode: previous.as_str().to_string(),
        workload_mode: request.workload_mode.as_str().to_string(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(environment_id, error = %e, "workload mode changed but the audit record failed");
    }
    Ok(Json(WorkloadModeResponse {
        environment_id,
        workload_mode: request.workload_mode,
    }))
}

async fn isolation_response(
    state: &WorkloadPlacementAppState,
    environment_id: i32,
    workload_isolation: EnvironmentWorkloadIsolation,
) -> Result<WorkloadIsolationResponse, Problem> {
    let mode = state
        .service
        .environment_workload_mode(environment_id)
        .await?;
    Ok(WorkloadIsolationResponse {
        environment_id,
        workload_isolation,
        applies: mode == EnvironmentWorkloadMode::Enforce,
    })
}

#[utoipa::path(
    tag = "Workload Placement", get,
    path = "/internal/environments/{environment_id}/workload-isolation",
    operation_id = "admin_get_environment_workload_isolation",
    params(("environment_id" = i32, Path, description = "Environment ID")),
    responses(
        (status = 200, body = WorkloadIsolationResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Environment not found", body = ProblemDetails),
        (status = 500, description = "Storage failed", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn admin_get_environment_workload_isolation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkloadPlacementAppState>>,
    Path(environment_id): Path<i32>,
) -> Result<Json<WorkloadIsolationResponse>, Problem> {
    permission_guard!(auth, SettingsRead);
    let isolation = state
        .service
        .environment_workload_isolation(environment_id)
        .await?;
    Ok(Json(
        isolation_response(&state, environment_id, isolation).await?,
    ))
}

#[utoipa::path(
    tag = "Workload Placement", put,
    path = "/internal/environments/{environment_id}/workload-isolation",
    operation_id = "admin_set_environment_workload_isolation",
    params(("environment_id" = i32, Path, description = "Environment ID")),
    request_body = SetWorkloadIsolationRequest,
    responses(
        (status = 200, body = WorkloadIsolationResponse),
        (status = 400, description = "Invalid isolation", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Instance admin required", body = ProblemDetails),
        (status = 404, description = "Environment not found", body = ProblemDetails),
        (status = 500, description = "Storage failed", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn admin_set_environment_workload_isolation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkloadPlacementAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(environment_id): Path<i32>,
    Json(request): Json<SetWorkloadIsolationRequest>,
) -> Result<Json<WorkloadIsolationResponse>, Problem> {
    permission_guard!(auth, SettingsWrite);
    let previous = state
        .service
        .set_environment_workload_isolation(environment_id, request.workload_isolation)
        .await?;
    let audit = EnvironmentWorkloadIsolationChangedAudit {
        context: audit_context(&auth, &metadata),
        environment_id,
        previous_isolation: previous.as_str().to_string(),
        workload_isolation: request.workload_isolation.as_str().to_string(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(environment_id, error = %e, "workload isolation changed but the audit record failed");
    }
    Ok(Json(
        isolation_response(&state, environment_id, request.workload_isolation).await?,
    ))
}

#[utoipa::path(
    tag = "Workload Placement", get,
    path = "/internal/nodes/{node_id}/resources",
    operation_id = "admin_get_node_resources",
    params(("node_id" = i32, Path, description = "Node ID; 0 is the control plane")),
    responses(
        (status = 200, body = NodeResourcesResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Node not found", body = ProblemDetails),
        (status = 500, description = "Storage failed", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn admin_get_node_resources(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkloadPlacementAppState>>,
    Path(node_id): Path<i32>,
) -> Result<Json<NodeResourcesResponse>, Problem> {
    permission_guard!(auth, SettingsRead);
    Ok(Json(state.service.node_resources(node_id).await?.into()))
}

#[utoipa::path(
    tag = "Workload Placement", put,
    path = "/internal/nodes/{node_id}/resources",
    operation_id = "admin_set_node_resources",
    params(("node_id" = i32, Path, description = "Node ID; 0 is the control plane")),
    request_body = SetNodeResourcesRequest,
    responses(
        (status = 200, body = NodeResourcesResponse),
        (status = 400, description = "Invalid reserve", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Instance admin required", body = ProblemDetails),
        (status = 404, description = "Node not found", body = ProblemDetails),
        (status = 500, description = "Storage failed", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn admin_set_node_resources(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkloadPlacementAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(node_id): Path<i32>,
    Json(request): Json<SetNodeResourcesRequest>,
) -> Result<Json<NodeResourcesResponse>, Problem> {
    permission_guard!(auth, SettingsWrite);
    if request.resource_policy.is_none() && request.system_reserved.is_none() {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Placement Settings")
            .with_detail(format!(
                "Nothing to change for node {node_id}: set resource_policy and/or system_reserved"
            )));
    }
    let (previous, current) = state
        .service
        .set_node_resources(
            node_id,
            NodeResourceUpdate {
                resource_policy: request.resource_policy,
                system_reserved: request.system_reserved,
            },
        )
        .await?;
    let audit = NodeResourcesChangedAudit {
        context: audit_context(&auth, &metadata),
        node_id,
        previous_resource_policy: previous.resource_policy.as_str().to_string(),
        resource_policy: current.resource_policy.as_str().to_string(),
        previous_system_reserved: previous.system_reserved,
        system_reserved: current.system_reserved,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(node_id, error = %e, "node resources changed but the audit record failed");
    }
    Ok(Json(current.into()))
}

#[utoipa::path(
    tag = "Workload Placement", get,
    path = "/internal/environments/{environment_id}/placement/explain",
    operation_id = "admin_explain_environment_placement",
    params(("environment_id" = i32, Path, description = "Environment ID")),
    responses(
        (status = 200, body = PlacementExplanationResponse),
        (status = 400, description = "Invalid placement settings", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Environment not found", body = ProblemDetails),
        (status = 500, description = "Storage failed", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn admin_explain_environment_placement(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkloadPlacementAppState>>,
    Path(environment_id): Path<i32>,
) -> Result<Json<PlacementExplanationResponse>, Problem> {
    permission_guard!(auth, SettingsRead);
    let (explanation, _) = state.service.explain_environment(environment_id).await?;
    let pinned_by = state
        .service
        .volume_pin(environment_id)
        .await?
        .map(|pin| pin.to_string());
    Ok(Json(PlacementExplanationResponse {
        pinned_by,
        environment_id,
        recommended: explanation
            .recommended
            .into_iter()
            .map(|node| NodeRecommendationDto {
                node_id: node.node_id,
                node_name: node.node_name,
                score: node.score,
                reasons: node.reasons,
            })
            .collect(),
        excluded: explanation
            .excluded
            .into_iter()
            .map(|exclusion| NodeExclusionDto {
                reason: exclusion.reason.code().to_string(),
                message: exclusion.to_string(),
                node_id: exclusion.node_id,
                node_name: exclusion.node_name,
                excluded: exclusion.excluded,
            })
            .collect(),
        placement: explanation.placement,
        unschedulable: explanation.unschedulable,
        unschedulable_reason: explanation.unschedulable_reason,
        warnings: explanation.warnings,
    }))
}

#[utoipa::path(
    tag = "Workload Placement", get,
    path = "/internal/environments/{environment_id}/placement/recommendation",
    operation_id = "admin_environment_pinning_recommendation",
    params(("environment_id" = i32, Path, description = "Environment ID"), RecommendationQuery),
    responses(
        (status = 200, body = PinningRecommendationResponse),
        (status = 400, description = "Invalid window", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Environment not found", body = ProblemDetails),
        (status = 500, description = "Storage failed", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn admin_environment_pinning_recommendation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkloadPlacementAppState>>,
    Path(environment_id): Path<i32>,
    Query(query): Query<RecommendationQuery>,
) -> Result<Json<PinningRecommendationResponse>, Problem> {
    permission_guard!(auth, SettingsRead);
    let result = state
        .service
        .pinning_recommendation(
            environment_id,
            query
                .window_days
                .unwrap_or(DEFAULT_RECOMMENDATION_WINDOW_DAYS),
        )
        .await?;
    Ok(Json(PinningRecommendationResponse {
        environment_id: result.environment_id,
        window_days: result.window_days,
        nodes_used: result.nodes_used,
        moved_between_nodes: result.moved_between_nodes,
        recommended_nodes: result.recommendation.nodes,
        reasons: result.recommendation.reasons,
        current_nodes: result.recommendation.current,
        changes_placement: result.recommendation.changes_placement,
    }))
}

pub fn configure_routes() -> Router<Arc<WorkloadPlacementAppState>> {
    Router::new()
        .route(
            "/internal/environments/{environment_id}/workload-mode",
            get(admin_get_environment_workload_mode).put(admin_set_environment_workload_mode),
        )
        .route(
            "/internal/environments/{environment_id}/workload-isolation",
            get(admin_get_environment_workload_isolation)
                .put(admin_set_environment_workload_isolation),
        )
        .route(
            "/internal/nodes/{node_id}/resources",
            get(admin_get_node_resources).put(admin_set_node_resources),
        )
        .route(
            "/internal/environments/{environment_id}/placement/explain",
            get(admin_explain_environment_placement),
        )
        .route(
            "/internal/environments/{environment_id}/placement/recommendation",
            get(admin_environment_pinning_recommendation),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(
        admin_get_environment_workload_mode,
        admin_set_environment_workload_mode,
        admin_get_environment_workload_isolation,
        admin_set_environment_workload_isolation,
        admin_get_node_resources,
        admin_set_node_resources,
        admin_explain_environment_placement,
        admin_environment_pinning_recommendation,
    ),
    components(schemas(
        WorkloadModeResponse,
        SetWorkloadModeRequest,
        WorkloadIsolationResponse,
        SetWorkloadIsolationRequest,
        EnvironmentWorkloadIsolation,
        NodeResourcesResponse,
        SetNodeResourcesRequest,
        NodeRecommendationDto,
        NodeExclusionDto,
        PlacementExplanationResponse,
        PinningRecommendationResponse,
        EnvironmentWorkloadMode,
        NodeResourcePolicy,
        SystemReserved,
    )),
    tags((name = "Workload Placement", description = "ADR-055 deployments on the workload plane: modes, node reserves, scheduler explanations"))
)]
pub struct WorkloadPlacementApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::wireguard_mesh::admin_test_support::*;
    use crate::services::{NodeScheduler, NodeService};
    use sea_orm::DatabaseConnection;

    fn user_session() -> temps_auth::AuthContext {
        let mut auth = admin();
        auth.effective_role = temps_auth::Role::User;
        auth
    }

    fn app(db: DatabaseConnection, audit: Arc<RecordingAuditLogger>) -> axum::Router {
        let db = Arc::new(db);
        let node_service = Arc::new(NodeService::new(db.clone()));
        let scheduler = NodeScheduler::new(node_service.clone());
        let service = Arc::new(WorkloadPlacementService::new(
            db,
            node_service,
            config_service(temps_core::AppSettings::default()),
            &scheduler,
        ));
        configure_routes().with_state(Arc::new(WorkloadPlacementAppState {
            service,
            audit_service: audit,
        }))
    }

    fn environment(id: i32, mode: EnvironmentWorkloadMode) -> temps_entities::environments::Model {
        serde_json::from_value(serde_json::json!({
            "id": id, "project_id": 7, "name": "production", "slug": "production",
            "subdomain": "shop-production", "host": "shop.example.test", "upstreams": [],
            "created_at": chrono::Utc::now(), "updated_at": chrono::Utc::now(),
            "is_preview": false, "protected": false, "sleeping": false,
            "workload_mode": mode.as_str(),
        }))
        .expect("environment fixture")
    }

    #[tokio::test]
    async fn every_route_requires_authentication() {
        for (method, uri, body) in [
            ("GET", "/internal/environments/3/workload-mode", None),
            (
                "PUT",
                "/internal/environments/3/workload-mode",
                Some(serde_json::json!({"workload_mode": "shadow"})),
            ),
            ("GET", "/internal/environments/3/workload-isolation", None),
            (
                "PUT",
                "/internal/environments/3/workload-isolation",
                Some(serde_json::json!({"workload_isolation": "micro_vm"})),
            ),
            ("GET", "/internal/nodes/0/resources", None),
            ("GET", "/internal/environments/3/placement/explain", None),
            (
                "GET",
                "/internal/environments/3/placement/recommendation",
                None,
            ),
        ] {
            let audit = Arc::new(RecordingAuditLogger::default());
            let (status, _, _) = send(
                app(mock_db().into_connection(), audit),
                method,
                uri,
                body,
                None,
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn changing_a_mode_or_a_reserve_is_admin_only() {
        for auth in [settings_reader(), user_session()] {
            for (uri, body) in [
                (
                    "/internal/environments/3/workload-mode",
                    serde_json::json!({"workload_mode": "enforce"}),
                ),
                (
                    "/internal/nodes/4/resources",
                    serde_json::json!({"resource_policy": "enforced"}),
                ),
                (
                    "/internal/environments/3/workload-isolation",
                    serde_json::json!({"workload_isolation": "micro_vm"}),
                ),
            ] {
                let audit = Arc::new(RecordingAuditLogger::default());
                let (status, _, _) = send(
                    app(mock_db().into_connection(), audit.clone()),
                    "PUT",
                    uri,
                    Some(body),
                    Some(auth.clone()),
                )
                .await;
                assert_eq!(status, StatusCode::FORBIDDEN, "PUT {uri}");
                assert!(audit.records().is_empty(), "a refused write is not audited");
            }
        }
        let audit = Arc::new(RecordingAuditLogger::default());
        let (status, _, _) = send(
            app(mock_db().into_connection(), audit),
            "GET",
            "/internal/environments/3/placement/explain",
            None,
            Some(outsider()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "explain needs SettingsRead");
    }

    #[tokio::test]
    async fn a_mode_change_is_audited_with_the_previous_mode() {
        let db = mock_db()
            .append_query_results(vec![vec![environment(3, EnvironmentWorkloadMode::Legacy)]])
            .append_query_results(vec![vec![environment(3, EnvironmentWorkloadMode::Shadow)]])
            .into_connection();
        let audit = Arc::new(RecordingAuditLogger::default());
        let (status, _, body) = send(
            app(db, audit.clone()),
            "PUT",
            "/internal/environments/3/workload-mode",
            Some(serde_json::json!({"workload_mode": "shadow"})),
            Some(admin()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["workload_mode"], "shadow");
        let records = audit.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].operation, "ENVIRONMENT_WORKLOAD_MODE_CHANGED");
        assert_eq!(records[0].ip_address.as_deref(), Some(CLIENT_IP));
        assert!(
            records[0].body.contains("\"previous_mode\":\"legacy\""),
            "{}",
            records[0].body
        );
        assert!(records[0].body.contains("\"workload_mode\":\"shadow\""));
    }

    #[tokio::test]
    async fn an_unknown_environment_is_a_404_naming_it() {
        let db = mock_db()
            .append_query_results(vec![Vec::<temps_entities::environments::Model>::new()])
            .into_connection();
        let audit = Arc::new(RecordingAuditLogger::default());
        let (status, _, body) = send(
            app(db, audit.clone()),
            "PUT",
            "/internal/environments/99/workload-mode",
            Some(serde_json::json!({"workload_mode": "enforce"})),
            Some(admin()),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["detail"].as_str().unwrap_or_default().contains("99"));
        assert!(audit.records().is_empty());
    }

    #[tokio::test]
    async fn the_control_plane_reserve_reads_the_estimate_and_rejects_absurd_values() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let (status, _, body) = send(
            app(mock_db().into_connection(), audit.clone()),
            "GET",
            "/internal/nodes/0/resources",
            None,
            Some(settings_reader()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["resource_policy"], "best_effort");
        assert_eq!(
            body["system_reserved"]["memory_mb"],
            temps_entities::node_resource_settings::SystemReserved::CONTROL_PLANE_LOCAL_DATABASE
                .memory_mb
        );
        assert_eq!(body["system_reserved_is_default"], true);

        let (status, _, body) = send(
            app(mock_db().into_connection(), audit.clone()),
            "PUT",
            "/internal/nodes/0/resources",
            Some(
                serde_json::json!({"system_reserved": {"cpu_millis": 999_999_999, "memory_mb": 1}}),
            ),
            Some(admin()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(audit.records().is_empty());

        let (status, _, _) = send(
            app(mock_db().into_connection(), audit.clone()),
            "PUT",
            "/internal/nodes/0/resources",
            Some(serde_json::json!({})),
            Some(admin()),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "an empty change is refused"
        );
    }

    fn isolation_row(
        id: i32,
        isolation: EnvironmentWorkloadIsolation,
    ) -> temps_entities::environment_workload_isolation::Model {
        temps_entities::environment_workload_isolation::Model {
            id,
            project_id: 7,
            workload_mode: EnvironmentWorkloadMode::Enforce,
            workload_isolation: isolation,
            deleted_at: None,
            updated_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn an_isolation_change_is_audited_with_the_previous_isolation() {
        let db = mock_db()
            .append_query_results(vec![vec![isolation_row(
                3,
                EnvironmentWorkloadIsolation::Process,
            )]])
            .append_query_results(vec![vec![isolation_row(
                3,
                EnvironmentWorkloadIsolation::MicroVm,
            )]])
            .append_query_results(vec![vec![environment(3, EnvironmentWorkloadMode::Shadow)]])
            .into_connection();
        let audit = Arc::new(RecordingAuditLogger::default());
        let (status, _, body) = send(
            app(db, audit.clone()),
            "PUT",
            "/internal/environments/3/workload-isolation",
            Some(serde_json::json!({"workload_isolation": "micro_vm"})),
            Some(admin()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["workload_isolation"], "micro_vm");
        // Shadow keeps the legacy container: the response says so.
        assert_eq!(body["applies"], false);
        let records = audit.records();
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].operation,
            "ENVIRONMENT_WORKLOAD_ISOLATION_CHANGED"
        );
        assert!(
            records[0]
                .body
                .contains("\"previous_isolation\":\"process\""),
            "{}",
            records[0].body
        );
        assert!(records[0]
            .body
            .contains("\"workload_isolation\":\"micro_vm\""));
    }

    #[tokio::test]
    async fn an_unknown_isolation_value_or_environment_is_refused_unaudited() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let (status, _, _) = send(
            app(mock_db().into_connection(), audit.clone()),
            "PUT",
            "/internal/environments/3/workload-isolation",
            Some(serde_json::json!({"workload_isolation": "wasm"})),
            Some(admin()),
        )
        .await;
        assert!(status.is_client_error(), "{status}");
        let db = mock_db()
            .append_query_results(vec![Vec::<
                temps_entities::environment_workload_isolation::Model,
            >::new()])
            .into_connection();
        let (status, _, body) = send(
            app(db, audit.clone()),
            "PUT",
            "/internal/environments/98/workload-isolation",
            Some(serde_json::json!({"workload_isolation": "micro_vm"})),
            Some(admin()),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["detail"].as_str().unwrap_or_default().contains("98"));
        assert!(audit.records().is_empty());
    }
}
