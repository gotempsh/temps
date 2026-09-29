// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sandbox placement API (ADR-048): which nodes may run sandboxes.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use temps_auth::{permissions::Permission, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::{AuditContext, AuditOperation, RequestMetadata};

use crate::error::SandboxError;
use crate::handlers::sandboxes::{SandboxInner, SandboxResponse};
use crate::handlers::SandboxAppState;
use crate::services::placement::PlacementNode;

/// Sandbox placement state: the operator allow-list and every node.
#[derive(Debug, Serialize, ToSchema)]
pub struct SandboxPlacementResponse {
    /// Node ids allowed to run sandboxes. `null` = every node (the default);
    /// `0` is the control plane.
    pub allowed_node_ids: Option<Vec<i32>>,
    /// Every node (control plane first) with its placement state.
    pub nodes: Vec<PlacementNode>,
}

/// A live sandbox on a node, with its owner. For sandboxes the caller does
/// not own, owner-only details (preview password hint, source repository
/// URL) are omitted.
#[derive(Debug, Serialize, ToSchema)]
pub struct NodeSandboxEntry {
    pub sandbox: SandboxInner,
    pub owner_user_id: Option<i32>,
    /// Owner's email, when the owner is a user that still exists.
    pub owner_email: Option<String>,
}

/// `GET /v1/sandboxes/placement/nodes/{node}` — every live sandbox on one
/// node, whoever owns it.
#[derive(Debug, Serialize, ToSchema)]
pub struct NodeSandboxesResponse {
    pub node: PlacementNode,
    /// One page, newest first.
    pub sandboxes: Vec<NodeSandboxEntry>,
    /// Every live sandbox on the node, across all pages.
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

/// Paging for `GET /v1/sandboxes/placement/nodes/{node}`.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct NodeSandboxesQuery {
    /// 1-based page (default 1).
    pub page: Option<u64>,
    /// Items per page (default 20, max 100).
    pub page_size: Option<u64>,
}

/// Replace the sandbox placement allow-list.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateSandboxPlacementBody {
    /// Node ids allowed to run new sandboxes; `0` is the control plane.
    /// `null` allows every node. `[]` stops new sandboxes from being created
    /// anywhere. Existing sandboxes keep running wherever they are.
    #[schema(example = json!([0, 3]))]
    pub allowed_node_ids: Option<Vec<i32>>,
}

/// `POST /v1/sandboxes/placement/nodes/{node}/evict`
#[derive(Debug, Serialize, ToSchema)]
pub struct NodeEvictionResponse {
    pub node: PlacementNode,
    /// Public ids of the sandboxes destroyed (their rows are gone, and they
    /// no longer block removing the node).
    pub destroyed: Vec<String>,
    /// Destroyed sandboxes whose container the node did not confirm
    /// removing: it may still be running there. Check the node, or remove
    /// it if it is gone for good.
    pub containers_unconfirmed: Vec<EvictionUnconfirmedContainer>,
}

/// A sandbox destroyed without the node confirming its container is gone.
#[derive(Debug, Serialize, ToSchema)]
pub struct EvictionUnconfirmedContainer {
    pub sandbox_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
struct SandboxNodeEvictedAudit {
    context: AuditContext,
    node_id: i32,
    node_name: String,
    destroyed: Vec<String>,
    /// Destroyed sandboxes whose container the node did not confirm removing.
    containers_unconfirmed: Vec<String>,
    /// Sandbox ids that could not be destroyed.
    failed: Vec<String>,
}

impl AuditOperation for SandboxNodeEvictedAudit {
    fn operation_type(&self) -> String {
        "SANDBOX_NODE_EVICTED".to_string()
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
            .map_err(|e| anyhow::anyhow!("Failed to serialize SandboxNodeEvictedAudit: {}", e))
    }
}

#[derive(Debug, Clone, Serialize)]
struct SandboxPlacementUpdatedAudit {
    context: AuditContext,
    previous_allowed_node_ids: Option<Vec<i32>>,
    allowed_node_ids: Option<Vec<i32>>,
}

impl AuditOperation for SandboxPlacementUpdatedAudit {
    fn operation_type(&self) -> String {
        "SANDBOX_PLACEMENT_UPDATED".to_string()
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
            .map_err(|e| anyhow::anyhow!("Failed to serialize SandboxPlacementUpdatedAudit: {}", e))
    }
}

/// List the nodes sandboxes can be placed on and the operator allow-list.
#[utoipa::path(
    tag = "Sandboxes",
    get,
    path = "/v1/sandboxes/placement",
    responses(
        (status = 200, description = "Sandbox placement state", body = SandboxPlacementResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_sandbox_placement(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
) -> Result<impl IntoResponse, Problem> {
    super::sandboxes::sandbox_permission_guard(
        &auth,
        Permission::SandboxesRead,
        Permission::ProjectsRead,
    )?;
    let allowed_node_ids = state.sandbox_service.allowed_node_ids().await?;
    let nodes = state.sandbox_service.placement_nodes().await?;
    Ok(Json(SandboxPlacementResponse {
        allowed_node_ids,
        nodes,
    }))
}

/// Set which nodes may run new sandboxes. Admin only.
#[utoipa::path(
    tag = "Sandboxes",
    put,
    path = "/v1/sandboxes/placement",
    request_body = UpdateSandboxPlacementBody,
    responses(
        (status = 200, description = "Updated placement state", body = SandboxPlacementResponse),
        (status = 400, description = "Unknown or duplicate node id"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Administrator role required")
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_sandbox_placement(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(body): Json<UpdateSandboxPlacementBody>,
) -> Result<impl IntoResponse, Problem> {
    super::sandboxes::require_sandbox_admin(&auth)?;
    let previous = state.sandbox_service.allowed_node_ids().await?;
    let allowed_node_ids = state
        .sandbox_service
        .set_allowed_node_ids(body.allowed_node_ids)
        .await?;

    let user_id = auth.user_id();
    tracing::info!(
        user_id = %user_id,
        allowed_node_ids = ?allowed_node_ids,
        "sandbox placement: allow-list updated"
    );
    if let Some(ref audit) = state.audit_service {
        let event = SandboxPlacementUpdatedAudit {
            context: AuditContext {
                user_id,
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            previous_allowed_node_ids: previous,
            allowed_node_ids: allowed_node_ids.clone(),
        };
        if let Err(e) = audit.create_audit_log(&event).await {
            tracing::error!(
                user_id = %user_id,
                "sandbox placement: failed to write audit log: {}",
                e
            );
        }
    }

    let nodes = state.sandbox_service.placement_nodes().await?;
    Ok(Json(SandboxPlacementResponse {
        allowed_node_ids,
        nodes,
    }))
}

/// List every live sandbox on one node, from all owners. Admin only: it
/// exposes other users' sandboxes.
#[utoipa::path(
    tag = "Sandboxes",
    get,
    path = "/v1/sandboxes/placement/nodes/{node}",
    params(
        ("node" = String, Path, description = "Worker name or id, or `control-plane` / `0`"),
        NodeSandboxesQuery
    ),
    responses(
        (status = 200, description = "Sandboxes on the node", body = NodeSandboxesResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Administrator role required"),
        (status = 404, description = "No such node")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_node_sandboxes(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(node): Path<String>,
    Query(q): Query<NodeSandboxesQuery>,
) -> Result<impl IntoResponse, Problem> {
    super::sandboxes::require_sandbox_admin(&auth)?;
    // The service clamps both; the response echoes what it served.
    let result = match state
        .sandbox_service
        .node_sandboxes(&node, q.page, q.page_size)
        .await
    {
        Ok(result) => result,
        // Unlike `create --node`, the node is the resource being read here.
        Err(e @ SandboxError::NodeNotFound { .. }) => {
            return Err(problemdetails::new(StatusCode::NOT_FOUND)
                .with_type("https://temps.sh/probs/sandbox-node-not-found")
                .with_title("Sandbox Node Not Found")
                .with_detail(e.to_string()));
        }
        Err(e) => return Err(e.into()),
    };
    let caller = auth.user_id();
    let parts = state.sandbox_service.preview_parts().await;
    let sandboxes = result
        .sandboxes
        .into_iter()
        .map(|entry| {
            let template = parts.host_template(&entry.summary.public_id);
            let mut sandbox =
                SandboxResponse::with_template(entry.summary, template, &parts).sandbox;
            if entry.owner_user_id != Some(caller) {
                sandbox.preview_password_hint = None;
                sandbox.source_repo_url = None;
            }
            NodeSandboxEntry {
                sandbox,
                owner_user_id: entry.owner_user_id,
                owner_email: entry.owner_email,
            }
        })
        .collect();
    Ok(Json(NodeSandboxesResponse {
        node: result.node,
        sandboxes,
        total: result.total,
        page: result.page,
        page_size: result.page_size,
    }))
}

/// Destroy every live sandbox on a worker node, from all owners, so the node
/// can be removed. Works on a node that is offline for good: its sandboxes
/// are marked destroyed even when the node cannot be reached. Admin only.
#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/v1/sandboxes/placement/nodes/{node}/evict",
    params(
        ("node" = String, Path, description = "Worker name or id")
    ),
    responses(
        (status = 200, description = "Sandboxes destroyed", body = NodeEvictionResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Administrator role required"),
        (status = 428, description = "Recent MFA verification required (browser sessions)"),
        (status = 400, description = "The control plane cannot be evicted"),
        (status = 404, description = "No such worker node"),
        (status = 503, description = "Some sandboxes could not be destroyed; the detail lists them and retrying picks them up")
    ),
    security(("bearer_auth" = []))
)]
pub async fn evict_node_sandboxes(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(node): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    super::sandboxes::require_sandbox_admin(&auth)?;
    // Destroys every owner's sandboxes and files: the same step-up as
    // draining a node.
    temps_auth::require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        temps_core::SensitiveAction::EvictNodeSandboxes { node: node.clone() },
    )
    .await?;
    let user_id = auth.user_id();

    // Detached, so the eviction and its audit record finish even if the
    // client disconnects part-way: sandboxes it already destroyed are gone
    // either way, and must not go unaudited.
    let task_state = state.clone();
    let eviction = tokio::spawn(async move {
        let eviction = task_state.sandbox_service.evict_node(&node).await?;
        tracing::info!(
            user_id = %user_id,
            node = %eviction.node.name,
            destroyed = eviction.destroyed.len(),
            failed = eviction.failed.len(),
            "sandbox placement: evicted node"
        );
        // Audited even when some destroys failed: the others are gone.
        if let Some(ref audit) = task_state.audit_service {
            let event = SandboxNodeEvictedAudit {
                context: AuditContext {
                    user_id,
                    ip_address: Some(metadata.ip_address.clone()),
                    user_agent: metadata.user_agent.clone(),
                },
                node_id: eviction.node.id,
                node_name: eviction.node.name.clone(),
                destroyed: eviction.destroyed.clone(),
                containers_unconfirmed: eviction
                    .containers_unconfirmed
                    .iter()
                    .map(|(id, _)| id.clone())
                    .collect(),
                failed: eviction.failed.iter().map(|(id, _)| id.clone()).collect(),
            };
            if let Err(e) = audit.create_audit_log(&event).await {
                tracing::error!(
                    user_id = %user_id,
                    "sandbox placement: failed to write eviction audit log: {}",
                    e
                );
            }
        }
        Ok::<_, SandboxError>(eviction)
    })
    .await
    .map_err(|e| {
        problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Sandbox Node Eviction Failed")
            .with_detail(format!(
                "The eviction stopped unexpectedly ({e}). Check the node's Sandboxes tab \
                 and run the eviction again."
            ))
    })?;
    let eviction = match eviction {
        Ok(eviction) => eviction,
        Err(e @ SandboxError::NodeNotFound { .. }) => {
            return Err(problemdetails::new(StatusCode::NOT_FOUND)
                .with_type("https://temps.sh/probs/sandbox-node-not-found")
                .with_title("Sandbox Node Not Found")
                .with_detail(e.to_string()));
        }
        Err(e) => return Err(e.into()),
    };

    if !eviction.failed.is_empty() {
        let reasons = eviction
            .failed
            .iter()
            .map(|(id, reason)| format!("{id}: {reason}"))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
            .with_type("https://temps.sh/probs/sandbox-node-eviction-incomplete")
            .with_title("Sandbox Node Eviction Incomplete")
            .with_detail(format!(
                "Destroyed {} sandbox(es) on node '{}', but {} could not be destroyed ({}). \
                 Run the eviction again to retry them.{}",
                eviction.destroyed.len(),
                eviction.node.name,
                eviction.failed.len(),
                reasons,
                if eviction.containers_unconfirmed.is_empty() {
                    String::new()
                } else {
                    format!(
                        " The node did not confirm removing the containers of {} destroyed \
                         sandbox(es): {}.",
                        eviction.containers_unconfirmed.len(),
                        eviction
                            .containers_unconfirmed
                            .iter()
                            .map(|(id, _)| id.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            )));
    }
    Ok(Json(NodeEvictionResponse {
        node: eviction.node,
        destroyed: eviction.destroyed,
        containers_unconfirmed: eviction
            .containers_unconfirmed
            .into_iter()
            .map(|(sandbox_id, reason)| EvictionUnconfirmedContainer { sandbox_id, reason })
            .collect(),
    }))
}
