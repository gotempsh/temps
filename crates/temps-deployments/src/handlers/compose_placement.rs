// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where a Docker Compose environment's stack lives, and moving it.
//!
//! A stack is pinned to the node its first deployment ran on, because its
//! volumes are there (see `services::compose_placement`). These endpoints
//! show that home and move it, which is the only way a stack changes nodes.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};
use temps_auth::{project_permission_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem, ProblemDetails};
use temps_core::{AuditContext, RequestMetadata};
use tracing::error;
use utoipa::{OpenApi, ToSchema};

use crate::services::compose_placement::{
    ComposePlacementError, ComposePlacementResponse, ComposePlacementService,
};

pub struct ComposePlacementAppState {
    pub placement_service: Arc<ComposePlacementService>,
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

impl From<ComposePlacementError> for Problem {
    fn from(error: ComposePlacementError) -> Self {
        match error {
            ComposePlacementError::EnvironmentNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Environment Not Found")
                    .with_detail(error.to_string())
            }
            ComposePlacementError::NodeNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Node Not Found")
                    .with_detail(error.to_string())
            }
            ComposePlacementError::NotComposeProject { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Not a Docker Compose Project")
                    .with_detail(error.to_string())
            }
            ComposePlacementError::DataLossNotAcknowledged { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Data Loss Not Acknowledged")
                    .with_detail(error.to_string())
            }
            ComposePlacementError::InvalidMove { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Invalid Compose Stack Move")
                .with_detail(error.to_string()),
            ComposePlacementError::HomeNodeUnavailable { .. }
            | ComposePlacementError::NoPlacement { .. }
            | ComposePlacementError::PinnedConcurrently { .. }
            | ComposePlacementError::MovedConcurrently { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Compose Stack Placement Conflict")
                    .with_detail(error.to_string())
            }
            ComposePlacementError::Database { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

/// Move a Compose stack to another node.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct MoveComposeStackRequest {
    /// The node to move the stack to; `0` is the control plane.
    pub node_id: i32,
    /// The stack starts on the new node with empty volumes; the data on its
    /// current home is kept there but not copied. Must be `true` to move a
    /// stack that already has a home.
    #[serde(default)]
    pub acknowledge_data_loss: bool,
}

#[derive(Debug, Clone, Serialize)]
struct ComposeStackMovedAudit {
    context: AuditContext,
    project_id: i32,
    environment_id: i32,
    /// The home it leaves (`0` is the control plane); `None` when it had none.
    from_node_id: Option<i32>,
    to_node_id: i32,
}

impl temps_core::AuditOperation for ComposeStackMovedAudit {
    fn operation_type(&self) -> String {
        "COMPOSE_STACK_MOVED".to_string()
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

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

/// Where this environment's Compose stack lives: its home node, a pending
/// move, and whether the environment's Target Nodes / Label Selectors still
/// describe that home.
#[utoipa::path(
    tag = "Compose",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/compose-placement",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    responses(
        (status = 200, description = "The stack's placement", body = ComposePlacementResponse),
        (status = 400, description = "Not a Docker Compose project", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Environment not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_compose_placement(
    State(state): State<Arc<ComposePlacementAppState>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        EnvironmentsRead,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);
    let placement = state
        .placement_service
        .status(project_id, environment_id)
        .await?;
    Ok(Json(placement))
}

/// Move this environment's Compose stack to another node. Nothing runs yet:
/// the next deployment stops the stack on its current home (`docker compose
/// down`, volumes kept there) and starts it on the new node with empty
/// volumes. Redeploy to complete the move.
#[utoipa::path(
    tag = "Compose",
    post,
    path = "/projects/{project_id}/environments/{environment_id}/compose-placement/move",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    request_body = MoveComposeStackRequest,
    responses(
        (status = 200, description = "The new placement; redeploy to complete the move", body = ComposePlacementResponse),
        (status = 400, description = "Not a Docker Compose project, or data loss not acknowledged", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Environment or node not found", body = ProblemDetails),
        (status = 409, description = "The node cannot take the stack, or it already lives there", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn move_compose_stack(
    State(state): State<Arc<ComposePlacementAppState>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<MoveComposeStackRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        EnvironmentsWrite,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);
    let (placement, from_node_id) = state
        .placement_service
        .move_stack(
            project_id,
            environment_id,
            request.node_id,
            request.acknowledge_data_loss,
        )
        .await?;

    let audit = ComposeStackMovedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        environment_id,
        from_node_id,
        to_node_id: request.node_id,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for moving the Compose stack of environment {}: {}",
            environment_id, e
        );
    }
    Ok(Json(placement))
}

pub fn configure_routes() -> Router<Arc<ComposePlacementAppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/environments/{environment_id}/compose-placement",
            get(get_compose_placement),
        )
        .route(
            "/projects/{project_id}/environments/{environment_id}/compose-placement/move",
            post(move_compose_stack),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(get_compose_placement, move_compose_stack),
    // The "Compose" tag is declared once, by `ComposeCheckApiDoc`; declaring
    // it here too would list it twice in the merged document.
    components(schemas(ComposePlacementResponse, MoveComposeStackRequest))
)]
pub struct ComposePlacementApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_errors_map_to_problem_statuses() {
        let status = |error: ComposePlacementError| Problem::from(error).status_code;
        assert_eq!(
            status(ComposePlacementError::EnvironmentNotFound {
                project_id: 1,
                environment_id: 2
            }),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status(ComposePlacementError::DataLossNotAcknowledged {
                environment_id: 2,
                home: "the control plane".to_string()
            }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status(ComposePlacementError::InvalidMove {
                environment_id: 2,
                reason: "the stack already lives on the control plane".to_string()
            }),
            StatusCode::CONFLICT
        );
    }

    #[test]
    fn a_move_request_defaults_to_not_acknowledging_data_loss() {
        let request: MoveComposeStackRequest = serde_json::from_str(r#"{ "node_id": 3 }"#).unwrap();
        assert_eq!(request.node_id, 3);
        assert!(!request.acknowledge_data_loss);
    }
}
