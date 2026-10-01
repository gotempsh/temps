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
use crate::services::sandbox_service::{node_cleanup_command, NodeEviction};

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
    /// Required: `null` allows every node, `[]` stops new sandboxes from
    /// being created anywhere. A body without this member is rejected, so
    /// an empty `{}` can never silently mean "every node". Existing
    /// sandboxes keep running wherever they are.
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<Vec<i32>>, required = true, nullable = true, example = json!([0, 3]))]
    pub allowed_node_ids: Option<Option<Vec<i32>>>,
}

/// Distinguishes a member sent as `null` (`Some(None)`) from one left out
/// (`None`, through `#[serde(default)]`).
fn present<'de, D>(deserializer: D) -> Result<Option<Option<Vec<i32>>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<Vec<i32>>::deserialize(deserializer).map(Some)
}

/// RFC 7807 body of a `503` from `POST /v1/sandboxes/placement/nodes/{node}/evict`
/// when some sandboxes could not be destroyed. Carries the same per-sandbox
/// detail as a successful eviction, as Problem extension members.
#[derive(Debug, Serialize, ToSchema)]
pub struct NodeEvictionIncompleteProblem {
    /// `https://temps.sh/probs/sandbox-node-eviction-incomplete`
    #[serde(rename = "type")]
    pub type_: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
    /// Public ids of the sandboxes destroyed by this eviction.
    pub destroyed: Vec<String>,
    /// Destroyed sandboxes whose container the node did not confirm
    /// removing, with the command to remove it on the node.
    pub containers_unconfirmed: Vec<EvictionUnconfirmedContainer>,
    /// Sandboxes that could not be destroyed. Running the eviction again
    /// retries them.
    pub failed: Vec<EvictionFailedSandbox>,
}

/// A sandbox an eviction could not destroy.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EvictionFailedSandbox {
    pub sandbox_id: String,
    pub reason: String,
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
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EvictionUnconfirmedContainer {
    pub sandbox_id: String,
    pub reason: String,
    /// Run this on the node, if it comes back, to remove the sandbox's
    /// leftover containers. Nothing else will: the sandbox is destroyed, and
    /// its containers are not listed anywhere in Temps.
    pub cleanup_command: String,
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
    /// Owners (user ids) of the destroyed sandboxes.
    owner_user_ids: Vec<i32>,
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
        (status = 400, description = "Unknown or duplicate node id, or `allowed_node_ids` missing (send `null` to allow every node)"),
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
    let requested = required_allowed_node_ids(body)?;
    let previous = state.sandbox_service.allowed_node_ids().await?;
    let allowed_node_ids = state
        .sandbox_service
        .set_allowed_node_ids(requested)
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
        (status = 409, description = "An eviction of this node is already running"),
        (status = 503, description = "Some sandboxes could not be destroyed; `destroyed`, `containers_unconfirmed` and `failed` list them, and retrying picks the failed ones up", body = NodeEvictionIncompleteProblem, content_type = "application/problem+json")
    ),
    security(("bearer_auth" = []))
)]
pub async fn evict_node_sandboxes(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(node): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    authorize_eviction(state.sensitive_action_authorizer.as_ref(), &auth, &node).await?;
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
            containers_unconfirmed = eviction.containers_unconfirmed.len(),
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
                owner_user_ids: eviction.owner_user_ids.clone(),
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
        return Err(eviction_incomplete_problem(&eviction));
    }
    Ok(Json(NodeEvictionResponse {
        containers_unconfirmed: unconfirmed_containers(&eviction),
        node: eviction.node,
        destroyed: eviction.destroyed,
    }))
}

/// The containers an eviction could not confirm removed, with the command
/// that removes each on the node.
fn unconfirmed_containers(eviction: &NodeEviction) -> Vec<EvictionUnconfirmedContainer> {
    eviction
        .containers_unconfirmed
        .iter()
        .map(|(sandbox_id, reason)| EvictionUnconfirmedContainer {
            cleanup_command: node_cleanup_command(sandbox_id),
            sandbox_id: sandbox_id.clone(),
            reason: reason.clone(),
        })
        .collect()
}

const EVICTION_INCOMPLETE_TYPE: &str = "https://temps.sh/probs/sandbox-node-eviction-incomplete";
const EVICTION_INCOMPLETE_TITLE: &str = "Sandbox Node Eviction Incomplete";

/// The `503` for an eviction that left some sandboxes in place. The detail
/// explains it in prose; the `destroyed`, `containers_unconfirmed` and
/// `failed` members carry the same data in structured form, so a client
/// can render it (e.g. copy buttons for the cleanup commands).
fn eviction_incomplete_problem(eviction: &NodeEviction) -> Problem {
    let body = NodeEvictionIncompleteProblem {
        type_: EVICTION_INCOMPLETE_TYPE.to_string(),
        title: EVICTION_INCOMPLETE_TITLE.to_string(),
        status: StatusCode::SERVICE_UNAVAILABLE.as_u16(),
        detail: eviction_incomplete_detail(eviction),
        destroyed: eviction.destroyed.clone(),
        containers_unconfirmed: unconfirmed_containers(eviction),
        failed: eviction
            .failed
            .iter()
            .map(|(sandbox_id, reason)| EvictionFailedSandbox {
                sandbox_id: sandbox_id.clone(),
                reason: reason.clone(),
            })
            .collect(),
    };
    let mut problem = problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
        .with_type(body.type_.clone())
        .with_title(body.title.clone())
        .with_detail(body.detail.clone())
        .with_value("status", body.status);
    for (member, value) in [
        ("destroyed", serde_json::to_value(&body.destroyed)),
        (
            "containers_unconfirmed",
            serde_json::to_value(&body.containers_unconfirmed),
        ),
        ("failed", serde_json::to_value(&body.failed)),
    ] {
        match value {
            Ok(value) => problem = problem.with_value(member, value),
            // Plain strings and string-only structs always serialize; if
            // that ever changes, the prose detail still carries everything.
            Err(e) => tracing::error!(
                node = %eviction.node.name,
                member,
                error = %e,
                "sandbox placement: could not serialize an eviction problem member"
            ),
        }
    }
    problem
}

/// The allow-list from a placement update. The member is required (`null`
/// = every node), so a body that omits it is a `400` rather than silently
/// allowing every node.
fn required_allowed_node_ids(
    body: UpdateSandboxPlacementBody,
) -> Result<Option<Vec<i32>>, Problem> {
    body.allowed_node_ids.ok_or_else(|| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Validation Error")
            .with_detail(
                "`allowed_node_ids` is required: send `null` to allow every node, or a list \
                 of node ids (0 is the control plane; `[]` allows none)",
            )
    })
}

/// Evicting destroys every owner's sandboxes and files: administrators only,
/// with the same step-up as draining a node. Checked before any side effect.
async fn authorize_eviction(
    authorizer: &dyn temps_core::SensitiveActionAuthorizer,
    auth: &temps_auth::context::AuthContext,
    node: &str,
) -> Result<(), Problem> {
    super::sandboxes::require_sandbox_admin(auth)?;
    temps_auth::require_sensitive_action(
        authorizer,
        auth,
        temps_core::SensitiveAction::EvictNodeSandboxes {
            node: node.to_string(),
        },
    )
    .await
}

/// Problem detail for an eviction that left some sandboxes in place: what
/// happened, how to retry, and how to clean up containers the node never
/// confirmed removing.
fn eviction_incomplete_detail(eviction: &NodeEviction) -> String {
    let reasons = eviction
        .failed
        .iter()
        .map(|(id, reason)| format!("{id}: {reason}"))
        .collect::<Vec<_>>()
        .join("; ");
    let mut detail = format!(
        "Destroyed {} sandbox(es) on node '{}', but {} could not be destroyed ({}). \
         Run the eviction again to retry them.",
        eviction.destroyed.len(),
        eviction.node.name,
        eviction.failed.len(),
        reasons,
    );
    if !eviction.containers_unconfirmed.is_empty() {
        let commands = eviction
            .containers_unconfirmed
            .iter()
            .map(|(id, _)| format!("{id}: `{}`", node_cleanup_command(id)))
            .collect::<Vec<_>>()
            .join("; ");
        detail.push_str(&format!(
            " The node did not confirm removing the containers of {} destroyed sandbox(es). \
             If the node comes back, remove them by running on it: {commands}.",
            eviction.containers_unconfirmed.len(),
        ));
    }
    detail
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use utoipa::OpenApi;

    use chrono::Utc;
    use temps_auth::context::AuthContext;
    use temps_auth::permissions::Role;
    use temps_core::{
        SensitiveAction, SensitiveActionAuthorizationError, SensitiveActionAuthorizer,
        SensitiveActionDecision, SensitiveActionPrincipal,
    };
    use temps_entities::users;

    fn user() -> users::Model {
        let now = Utc::now();
        users::Model {
            id: 1,
            name: "Operator".to_string(),
            email: "operator@example.com".to_string(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: true,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Answers every request with one decision and records what it was asked.
    struct Fixed {
        decision: SensitiveActionDecision,
        calls: AtomicUsize,
        actions: std::sync::Mutex<Vec<String>>,
    }

    impl Fixed {
        fn new(decision: SensitiveActionDecision) -> Self {
            Self {
                decision,
                calls: AtomicUsize::new(0),
                actions: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl SensitiveActionAuthorizer for Fixed {
        async fn authorize(
            &self,
            action: &SensitiveAction,
            _principal: &SensitiveActionPrincipal,
        ) -> Result<SensitiveActionDecision, SensitiveActionAuthorizationError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.actions
                .lock()
                .expect("actions mutex")
                .push(format!("{action:?}"));
            Ok(self.decision.clone())
        }
    }

    #[tokio::test]
    async fn eviction_requires_an_admin_before_step_up() {
        let authorizer = Fixed::new(SensitiveActionDecision::Allow);
        let auth = AuthContext::new_persisted_session(user(), Role::User, 9);

        let problem = authorize_eviction(&authorizer, &auth, "worker-1")
            .await
            .expect_err("non-admins cannot evict");

        assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
        assert_eq!(authorizer.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn eviction_asks_for_step_up_on_the_evicted_node() {
        let authorizer = Fixed::new(SensitiveActionDecision::RequireVerification {
            mfa_setup_required: false,
        });
        let auth = AuthContext::new_persisted_session(user(), Role::Admin, 9);

        let problem = authorize_eviction(&authorizer, &auth, "worker-1")
            .await
            .expect_err("an admin without a recent step-up is asked for one");

        assert_eq!(problem.status_code, StatusCode::PRECONDITION_REQUIRED);
        let actions = authorizer.actions.lock().expect("actions mutex").clone();
        assert_eq!(actions.len(), 1);
        assert!(actions[0].contains("EvictNodeSandboxes"), "{actions:?}");
        assert!(actions[0].contains("worker-1"), "{actions:?}");
    }

    #[tokio::test]
    async fn eviction_proceeds_for_a_verified_admin() {
        let authorizer = Fixed::new(SensitiveActionDecision::Allow);
        let auth = AuthContext::new_persisted_session(user(), Role::Admin, 9);

        authorize_eviction(&authorizer, &auth, "worker-1")
            .await
            .expect("allowed");
        assert_eq!(authorizer.calls.load(Ordering::SeqCst), 1);
    }

    fn node() -> PlacementNode {
        PlacementNode {
            id: 3,
            name: "worker-3".into(),
            is_control_plane: false,
            status: "active".into(),
            allowed: true,
            eligible: true,
            reason: None,
            live_sandboxes: 1,
        }
    }

    fn partial_eviction() -> NodeEviction {
        NodeEviction {
            node: node(),
            destroyed: vec!["sbx_aaaa".into(), "sbx_bbbb".into()],
            containers_unconfirmed: vec![("sbx_bbbb".into(), "did not answer".into())],
            failed: vec![("sbx_cccc".into(), "database unavailable".into())],
            owner_user_ids: vec![4, 9],
        }
    }

    /// The console reads `destroyed`, `containers_unconfirmed` and `failed`
    /// from the 503 body to render the outcome (with copy buttons for the
    /// cleanup commands); the prose detail stays for other clients.
    #[tokio::test]
    async fn incomplete_eviction_problem_carries_structured_members() {
        let response = eviction_incomplete_problem(&partial_eviction()).into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("json");

        assert_eq!(body["type"], EVICTION_INCOMPLETE_TYPE);
        assert_eq!(body["status"], 503);
        assert_eq!(
            body["destroyed"],
            serde_json::json!(["sbx_aaaa", "sbx_bbbb"])
        );
        assert_eq!(
            body["containers_unconfirmed"],
            serde_json::json!([{
                "sandbox_id": "sbx_bbbb",
                "reason": "did not answer",
                "cleanup_command": "docker ps -aq --filter name=temps-sandbox-bbbb | xargs -r docker rm -f",
            }])
        );
        assert_eq!(
            body["failed"],
            serde_json::json!([{ "sandbox_id": "sbx_cccc", "reason": "database unavailable" }])
        );
        assert!(body["detail"]
            .as_str()
            .is_some_and(|d| d.contains("Run the eviction again")));
    }

    #[test]
    fn eviction_audit_names_the_owners() {
        let eviction = partial_eviction();
        let audit = SandboxNodeEvictedAudit {
            context: AuditContext {
                user_id: 1,
                ip_address: None,
                user_agent: "test".into(),
            },
            node_id: eviction.node.id,
            node_name: eviction.node.name.clone(),
            destroyed: eviction.destroyed.clone(),
            containers_unconfirmed: vec!["sbx_bbbb".into()],
            failed: vec!["sbx_cccc".into()],
            owner_user_ids: eviction.owner_user_ids.clone(),
        };
        let json: serde_json::Value =
            serde_json::from_str(&AuditOperation::serialize(&audit).expect("serialize"))
                .expect("json");
        assert_eq!(json["owner_user_ids"], serde_json::json!([4, 9]));
        assert_eq!(audit.operation_type(), "SANDBOX_NODE_EVICTED");
    }

    #[test]
    fn placement_update_requires_the_allow_list_member() {
        let parse = |raw: &str| serde_json::from_str::<UpdateSandboxPlacementBody>(raw);

        let missing = parse("{}").expect("parses");
        let problem = required_allowed_node_ids(missing).expect_err("missing member");
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);

        let all_nodes = parse(r#"{"allowed_node_ids": null}"#).expect("parses");
        assert_eq!(required_allowed_node_ids(all_nodes).expect("null"), None);

        let some = parse(r#"{"allowed_node_ids": [0, 3]}"#).expect("parses");
        assert_eq!(
            required_allowed_node_ids(some).expect("list"),
            Some(vec![0, 3])
        );

        let none = parse(r#"{"allowed_node_ids": []}"#).expect("parses");
        assert_eq!(
            required_allowed_node_ids(none).expect("empty"),
            Some(vec![])
        );

        assert!(parse(r#"{"allowed_node_ids": null, "extra": 1}"#).is_err());
    }

    #[test]
    fn placement_update_schema_marks_the_member_required_and_nullable() {
        let api = crate::handlers::SandboxApiDoc::openapi();
        let schema = serde_json::to_value(
            api.components
                .as_ref()
                .and_then(|c| c.schemas.get("UpdateSandboxPlacementBody"))
                .expect("schema registered"),
        )
        .expect("schema json");
        assert_eq!(schema["required"], serde_json::json!(["allowed_node_ids"]));
        let member =
            serde_json::to_string(&schema["properties"]["allowed_node_ids"]).expect("member json");
        assert!(member.contains("null"), "nullable: {member}");
        assert!(member.contains("integer"), "list of ids: {member}");
    }

    // ── Routed handler tests ────────────────────────────────────────────

    fn metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "127.0.0.1".into(),
            user_agent: "test".into(),
            headers: axum::http::HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://localhost".into(),
            scheme: "http".into(),
            host: "localhost".into(),
            is_secure: false,
        }
    }

    /// The sandbox routes over a database that answers nothing: every
    /// request here must be decided before the service touches it.
    fn app(role: Role, authorizer: Arc<Fixed>) -> axum::Router {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let service =
            Arc::new(crate::services::sandbox_service::tests::preview_test_service_with_db(db));
        let state = Arc::new(SandboxAppState {
            sandbox_service: service,
            snapshot_service: None,
            project_access_checker: None,
            audit_service: None,
            sensitive_action_authorizer: authorizer,
        });
        crate::handlers::configure_routes()
            .with_state(state)
            .layer(Extension(AuthContext::new_persisted_session(
                user(),
                role,
                9,
            )))
            .layer(Extension(metadata()))
    }

    async fn send(app: axum::Router, method: &str, uri: &str, body: &str) -> StatusCode {
        use tower::ServiceExt;
        let request = axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .expect("request");
        app.oneshot(request).await.expect("response").status()
    }

    #[tokio::test]
    async fn non_admins_cannot_use_the_operator_placement_routes() {
        for (method, uri, body) in [
            (
                "PUT",
                "/v1/sandboxes/placement",
                r#"{"allowed_node_ids": null}"#,
            ),
            ("GET", "/v1/sandboxes/placement/nodes/worker-1", ""),
            ("POST", "/v1/sandboxes/placement/nodes/worker-1/evict", ""),
        ] {
            let authorizer = Arc::new(Fixed::new(SensitiveActionDecision::Allow));
            let status = send(app(Role::User, authorizer.clone()), method, uri, body).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
            assert_eq!(
                authorizer.calls.load(Ordering::SeqCst),
                0,
                "{method} {uri}: refused before any step-up"
            );
        }
    }

    #[tokio::test]
    async fn an_empty_placement_update_is_rejected_not_read_as_every_node() {
        let authorizer = Arc::new(Fixed::new(SensitiveActionDecision::Allow));
        let status = send(
            app(Role::Admin, authorizer),
            "PUT",
            "/v1/sandboxes/placement",
            "{}",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn incomplete_eviction_names_the_failures_and_how_to_clean_up() {
        let eviction = partial_eviction();

        let detail = eviction_incomplete_detail(&eviction);

        assert!(
            detail.contains("Destroyed 2 sandbox(es) on node 'worker-3'"),
            "{detail}"
        );
        assert!(
            detail.contains("sbx_cccc: database unavailable"),
            "{detail}"
        );
        assert!(detail.contains("Run the eviction again"), "{detail}");
        assert!(
            detail
                .contains("docker ps -aq --filter name=temps-sandbox-bbbb | xargs -r docker rm -f"),
            "{detail}"
        );
    }

    #[test]
    fn incomplete_eviction_without_unconfirmed_containers_skips_cleanup() {
        let eviction = NodeEviction {
            node: node(),
            destroyed: vec![],
            containers_unconfirmed: vec![],
            failed: vec![("sbx_cccc".into(), "database unavailable".into())],
            owner_user_ids: vec![],
        };

        assert!(!eviction_incomplete_detail(&eviction).contains("docker rm"));
    }
}
