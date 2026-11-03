// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The node operations that need the deployment pipeline: draining a node
//! redeploys what ran on it, and the Docker disk usage report reads the host
//! daemon through the housekeeping service. The rest of the node API lives in
//! `temps-nodes`.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, require_sensitive_action, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::SensitiveAction;
use temps_core::{AuditContext, AuditOperation, RequestMetadata};
use temps_nodes::handlers::audit::NodeDrainAudit;
use temps_nodes::handlers::nodes::create_remote_deployer;
use temps_nodes::services::node_service::{remove_owned_container, MAX_DESCRIBED_UNREMOVED};
use tracing::{error, info, warn};
use utoipa::{OpenApi, ToSchema};

use crate::handlers::types::AppState;
use crate::services::MAX_REDEPLOY_PLAN_BATCH;
use crate::services::{DockerDiskUsage, DockerDiskUsageCategory, DockerDiskUsageError};

#[derive(Serialize, Deserialize, ToSchema)]
pub struct DrainNodeResponse {
    pub id: i32,
    pub name: String,
    pub status: String,
    pub affected_environments: usize,
    /// Replicas retired with no other node to run them, so their deployments run
    /// short of their configured count until this node is undrained.
    #[serde(default)]
    pub replica_shortfall: usize,
    pub message: String,
}

#[derive(OpenApi)]
#[openapi(
    paths(admin_drain_node, node_docker_disk_usage),
    components(schemas(DrainNodeResponse, DockerDiskUsage, DockerDiskUsageCategory))
)]
pub struct NodeOperationsApiDoc;

/// `POST .../drain` shares its path with the status and undrain routes in
/// `temps-nodes`; the merged router serves all three methods.
pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/internal/nodes/{node_id}/drain", post(admin_drain_node))
        .route(
            "/nodes/{node_id}/docker-disk-usage",
            get(node_docker_disk_usage),
        )
}

/// Docker disk usage (`docker system df`) for the control-plane host.
///
/// On-demand rather than sampled: walking every layer, rootfs and volume can
/// take seconds, so the server monitoring page fetches it when opened or when
/// the operator presses refresh. Node `0` only for now — a worker's daemon is
/// only reachable through its agent, which does not relay this yet, and the
/// error says exactly that instead of 404ing.
#[utoipa::path(
    tag = "Nodes",
    get,
    path = "/nodes/{node_id}/docker-disk-usage",
    operation_id = "NodeDockerDiskUsageGet",
    params(
        ("node_id" = i32, Path, description = "Node ID (0 = control plane)")
    ),
    responses(
        (status = 200, description = "Docker disk usage by category", body = DockerDiskUsage),
        (status = 400, description = "Node is not the control plane"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 502, description = "Docker daemon answered with an unexpected response"),
        (status = 503, description = "Docker daemon unreachable"),
        (status = 504, description = "Docker daemon timed out"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn node_docker_disk_usage(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(node_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);

    let usage = app_state
        .docker_disk_usage
        .fetch(node_id)
        .await
        .map_err(Problem::from)?;

    Ok(Json(usage))
}

impl From<DockerDiskUsageError> for Problem {
    fn from(error: DockerDiskUsageError) -> Self {
        match error {
            DockerDiskUsageError::UnsupportedNode { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Docker Disk Usage Not Available For Node")
                    .with_detail(error.to_string())
            }
            DockerDiskUsageError::Unavailable { .. } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("Docker Daemon Unreachable")
                    .with_detail(error.to_string())
            }
            DockerDiskUsageError::Timeout { .. } => {
                problemdetails::new(StatusCode::GATEWAY_TIMEOUT)
                    .with_title("Docker Daemon Timed Out")
                    .with_detail(error.to_string())
            }
            DockerDiskUsageError::UnexpectedStatus { .. } | DockerDiskUsageError::Parse { .. } => {
                problemdetails::new(StatusCode::BAD_GATEWAY)
                    .with_title("Unexpected Docker Daemon Response")
                    .with_detail(error.to_string())
            }
        }
    }
}

/// 409 for a drain refused before it changed anything, because at least one
/// workload that only runs on this node has nothing to be rebuilt from.
fn drain_refused_problem(node_name: &str, unmovable: &BoundedReport) -> Problem {
    let lines = unmovable.report();
    problemdetails::new(StatusCode::CONFLICT)
        .with_title("Node Cannot Be Drained")
        .with_detail(format!(
            "Node '{node_name}' was not drained and is unchanged: {} workload(s) running only on \
             it cannot be redeployed to another node: {}. Move or delete these workloads, then \
             drain again.",
            unmovable.count(),
            lines.join("; ")
        ))
        .with_value("node_name", node_name.to_string())
        .with_value("unmovable_count", unmovable.count())
        .with_value("unmovable_workloads", lines)
}

/// Failure descriptions with a fixed memory bound: every failure is counted,
/// the first [`MAX_DESCRIBED_UNREMOVED`] are described, and the report ends
/// with a line counting the rest.
#[derive(Debug, Default)]
struct BoundedReport {
    count: usize,
    lines: Vec<String>,
}

impl BoundedReport {
    fn push(&mut self, line: String) {
        self.count += 1;
        if self.lines.len() < MAX_DESCRIBED_UNREMOVED {
            self.lines.push(line);
        }
    }

    fn count(&self) -> usize {
        self.count
    }

    fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn report(&self) -> Vec<String> {
        let mut lines = self.lines.clone();
        let undescribed = self.count.saturating_sub(lines.len());
        if undescribed > 0 {
            lines.push(format!("and {undescribed} more not listed here"));
        }
        lines
    }
}

/// Write an audit record for a node operation. A failure is logged and never
/// fails the operation, which has already happened.
async fn record_audit(app_state: &AppState, audit: &dyn AuditOperation, node_id: i32) {
    if let Err(error) = app_state.audit_service.create_audit_log(audit).await {
        error!(node_id, %error, "node operation succeeded but its audit record failed");
    }
}

/// What to tell the operator when a drain leaves deployments short of replicas.
fn drain_shortfall_note(replica_shortfall: usize) -> String {
    if replica_shortfall == 0 {
        return String::new();
    }
    format!(
        " {replica_shortfall} replica(s) had no other node to run on: those deployments stay \
         short of their configured count until this node is undrained."
    )
}

/// Remove a deployment's containers on a draining node and take them out of
/// routing, returning how many were retired. Historical deployments (including
/// failed ones whose container still exists) are never redeployed, and a current
/// deployment reaches this only when another node has a confirmed running replica.
/// Only a removal the agent confirmed is recorded as `removed`; anything else stays
/// `retired` so cleanup still knows it may exist on this node.
async fn retire_on_drain(
    app_state: &AppState,
    node: &temps_entities::nodes::Model,
    dep: &crate::services::node_service::AffectedDeployment,
) -> usize {
    let node_id = node.id;
    let containers = app_state
        .node_service
        .list_containers_for_node_deployment(node_id, dep.deployment_id)
        .await
        .unwrap_or_default();

    let mut confirmed_removed = HashSet::new();
    if let Some(remote_deployer) = create_remote_deployer(
        node,
        &app_state.config_service,
        &app_state.encryption_service,
    )
    .await
    {
        for container in &containers {
            match remove_owned_container(
                remote_deployer.as_ref(),
                &container.container_id,
                dep.project_id,
            )
            .await
            {
                Ok(()) => {
                    confirmed_removed.insert(container.container_id.clone());
                }
                Err(error) => warn!(
                    node_id,
                    container_id = %container.container_id,
                    "Drain: could not remove container on agent (retiring it; cleanup retries): {}",
                    error
                ),
            }
        }
    }

    // Then soft-delete in DB so the proxy stops routing to them
    match app_state
        .node_service
        .retire_containers_on_node(
            node_id,
            dep.deployment_id,
            &confirmed_removed,
            dep.retire_reason(),
        )
        .await
    {
        Ok(count) => {
            info!(
                node_id,
                deployment_id = dep.deployment_id,
                project_id = dep.project_id,
                environment_id = dep.environment_id,
                is_current = dep.is_current,
                retired = count,
                remaining = dep
                    .total_active_containers
                    .saturating_sub(dep.containers_on_node),
                "Drain: retired containers without starting another deployment"
            );
            count
        }
        Err(e) => {
            error!(
                node_id,
                deployment_id = dep.deployment_id,
                "Drain: failed to retire containers: {}",
                e
            );
            0
        }
    }
}

/// 500 for a drain whose redeploys could not all be queued after the
/// pre-check passed. States exactly what already happened, so the operator
/// does not have to guess whether to retry.
fn drain_incomplete_problem(
    node_id: i32,
    node_name: &str,
    failed_redeploys: &BoundedReport,
    retired_count: usize,
    redeployed_count: usize,
    reactivated: bool,
) -> Problem {
    let node_state = if reactivated {
        "The node was set back to active; retry the drain once the cause is fixed.".to_string()
    } else {
        format!(
            "The node remains draining; undrain it (DELETE /internal/nodes/{node_id}/drain) and \
             retry once the cause is fixed."
        )
    };
    problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
        .with_title("Node Drain Incomplete")
        .with_detail(format!(
            "Drain of node '{node_name}' did not complete: {} redeploy(s) could not be queued, so \
             those workloads are still on the node ({}). {redeployed_count} environment(s) were \
             redeployed and {retired_count} container(s) retired before the failure. {node_state}",
            failed_redeploys.count(),
            failed_redeploys.report().join("; ")
        ))
        .with_value("node_name", node_name.to_string())
        .with_value("failed_redeploys", failed_redeploys.report())
}

/// Drain a node: mark it as "draining" so no new replicas are scheduled on it,
/// and trigger redeployment of all affected environments so their containers
/// are rescheduled to healthy nodes.
#[utoipa::path(
    tag = "Nodes",
    post,
    path = "/internal/nodes/{node_id}/drain",
    params(
        ("node_id" = i32, Path, description = "Node ID")
    ),
    responses(
        (status = 200, description = "Node drain initiated", body = DrainNodeResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Node not found"),
        (status = 409, description = "Node already draining, or a workload only on this node cannot be redeployed elsewhere; the node is unchanged"),
        (status = 500, description = "Internal server error, or a redeploy could not be queued (the response says what already happened)")
    ),
    security(("bearer_auth" = []))
)]
async fn admin_drain_node(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    axum::Extension(metadata): axum::Extension<RequestMetadata>,
    Path(node_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    require_sensitive_action(
        app_state.sensitive_action_authorizer.as_ref(),
        &auth,
        SensitiveAction::DrainNode { node_id },
    )
    .await?;

    let node = app_state
        .node_service
        .get_by_id(node_id)
        .await
        .map_err(Problem::from)?;

    if node.status == "draining" {
        return Err(problemdetails::new(StatusCode::CONFLICT)
            .with_title("Node Already Draining")
            .with_detail(format!("Node '{}' is already in draining state", node.name)));
    }

    // Get detailed info about each deployment on this node
    let mut affected = app_state
        .node_service
        .affected_deployments(node_id)
        .await
        .map_err(Problem::from)?;

    // What each deployment needs, asking the nodes rather than trusting rows: a
    // worker that just died still has its replica's row. Refused before anything
    // changes when nothing else is serving and nowhere else can.
    app_state
        .deployment_service
        .plan_node_drain(&app_state.node_scheduler, node_id, &mut affected)
        .await
        .map_err(Problem::from)?;

    // Every workload that has to move must be rebuildable somewhere else.
    // Check them all before changing anything: a drain that cannot move a
    // workload would otherwise stay "draining" forever with the container
    // still on the node, after reporting success. Planned a batch at a time,
    // keeping only what failed, so memory does not grow with the workloads.
    let mut unmovable = BoundedReport::default();
    for dep in &affected {
        if let Some(crate::services::node_service::DrainDecision::Refuse { reason }) =
            &dep.drain_decision
        {
            warn!(
                node_id,
                deployment_id = dep.deployment_id,
                "Drain refused: {}",
                reason
            );
            unmovable.push(reason.clone());
        }
    }
    for batch in affected.chunks(MAX_REDEPLOY_PLAN_BATCH) {
        let plans = app_state
            .deployment_service
            .plan_drain_redeploys(batch)
            .await
            .map_err(Problem::from)?;
        for (deployment_id, plan) in plans {
            if let Err(e) = plan {
                warn!(
                    node_id,
                    deployment_id, "Drain refused: workload cannot be redeployed elsewhere: {}", e
                );
                unmovable.push(e.to_string());
            }
        }
    }
    if !unmovable.is_empty() {
        return Err(drain_refused_problem(&node.name, &unmovable));
    }

    // Mark the node as draining — scheduler will exclude it from new assignments
    app_state
        .node_service
        .mark_draining(node_id)
        .await
        .map_err(Problem::from)?;

    let mut retired_count = 0usize;
    let mut replica_shortfall = 0usize;
    let mut redeployed_count = 0usize;
    let mut redeployed_environments = HashSet::new();
    let mut failed_redeploys = BoundedReport::default();

    for batch in affected.chunks(MAX_REDEPLOY_PLAN_BATCH) {
        // Planned again for this batch: the redeploys use these plans, and
        // holding every plan from the check above would grow with the node.
        // The node is already draining, so a failure here must not return
        // early: this batch's moves are reported as failed below, which sets
        // an active node back to active and says what already happened.
        let mut planning_error = None;
        let mut sources = match app_state
            .deployment_service
            .plan_drain_redeploys(batch)
            .await
        {
            Ok(sources) => sources,
            Err(e) => {
                error!(node_id, "Drain: could not plan a batch of redeploys: {}", e);
                planning_error = Some(e.to_string());
                HashMap::new()
            }
        };
        for dep in batch {
            if dep.is_current && dep.moves_on_drain() {
                if !redeployed_environments.insert((dep.project_id, dep.environment_id)) {
                    info!(
                        node_id,
                        project_id = dep.project_id,
                        environment_id = dep.environment_id,
                        deployment_id = dep.deployment_id,
                        "Drain: redeploy already queued for environment in this drain pass"
                    );
                    continue;
                }

                // Nothing else is confirmed serving, or a spare node keeps the
                // replica count: start the replacement before retiring anything.
                let source = match sources.remove(&dep.deployment_id) {
                    Some(Ok(source)) => source,
                    // Changed since the check above (e.g. its archive was
                    // deleted in between).
                    Some(Err(e)) => {
                        redeployed_environments.remove(&(dep.project_id, dep.environment_id));
                        failed_redeploys.push(e.to_string());
                        continue;
                    }
                    None => {
                        redeployed_environments.remove(&(dep.project_id, dep.environment_id));
                        failed_redeploys.push(match &planning_error {
                            Some(reason) => format!(
                                "deployment {} of project {} could not be planned: {reason}",
                                dep.deployment_id, dep.project_id
                            ),
                            None => format!(
                                "deployment {} of project {} was not planned",
                                dep.deployment_id, dep.project_id
                            ),
                        });
                        continue;
                    }
                };
                match app_state
                    .deployment_service
                    .redeploy_environment_from(
                        dep.project_id,
                        dep.environment_id,
                        dep.deployment_id,
                        source,
                    )
                    .await
                {
                    Ok(_) => {
                        redeployed_count += 1;
                        info!(
                            node_id,
                            project_id = dep.project_id,
                            environment_id = dep.environment_id,
                            deployment_id = dep.deployment_id,
                            "Drain: triggered a redeploy to replace this node's replicas"
                        );
                    }
                    Err(e) => {
                        redeployed_environments.remove(&(dep.project_id, dep.environment_id));
                        error!(
                            node_id,
                            project_id = dep.project_id,
                            environment_id = dep.environment_id,
                            deployment_id = dep.deployment_id,
                            "Drain: failed to trigger redeploy: {}",
                            e
                        );
                        failed_redeploys.push(e.to_string());
                    }
                }
            } else {
                let retired = retire_on_drain(&app_state, &node, dep).await;
                retired_count += retired;
                if dep.retire_reason()
                    == crate::services::node_service::RetireReason::DrainShortfall
                {
                    replica_shortfall += retired;
                }
            }
        }
    }

    let audit_drain = |outcome: &str| NodeDrainAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        node_id,
        node_name: node.name.clone(),
        outcome: outcome.to_string(),
        redeployed_environments: redeployed_count,
        retired_containers: retired_count,
        failed_redeploys: failed_redeploys.count(),
    };
    if !failed_redeploys.is_empty() {
        record_audit(&app_state, &audit_drain("incomplete"), node_id).await;
        // The pre-check passed, so this is a transient failure (queue,
        // database). Put an active node back so the drain can simply be
        // retried; leaving it "draining" would strand the workloads that never
        // moved. Any other prior status (e.g. offline) is not restored to
        // "active" here — the operator undrains it explicitly.
        let reactivated = node.status == "active"
            && match app_state.node_service.mark_active(node_id).await {
                Ok(()) => true,
                Err(e) => {
                    error!(
                        node_id,
                        "Drain: failed to reactivate node after a failed redeploy: {}", e
                    );
                    false
                }
            };
        return Err(drain_incomplete_problem(
            node_id,
            &node.name,
            &failed_redeploys,
            retired_count,
            redeployed_count,
            reactivated,
        ));
    }

    record_audit(&app_state, &audit_drain("draining"), node_id).await;
    info!(
        node_id,
        node_name = %node.name,
        affected_deployments = affected.len(),
        retired_count,
        redeployed_count,
        "Node drain initiated"
    );

    let affected_count = affected.len();

    Ok(Json(DrainNodeResponse {
        id: node_id,
        name: node.name,
        status: "draining".to_string(),
        affected_environments: affected_count,
        replica_shortfall,
        message: format!(
            "Node drain initiated. {} deployment(s) affected: {} container(s) retired, {} environment(s) redeployed.",
            affected_count, retired_count, redeployed_count
        ) + &drain_shortfall_note(replica_shortfall),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::handlers::deployments::tests::create_test_app_state_for_http;
    use temps_nodes::handlers::wireguard_mesh::admin_test_support::{
        node_admin_state, outsider, send, settings_reader,
    };
    use temps_test_utils::TestDatabase;

    /// The node API as the plugin mounts it: the `temps-nodes` routes with
    /// their state, merged with the routes of this module. `None` when no test
    /// database is available. Keep the database alive as long as the router.
    async fn node_api() -> Option<(Router, TestDatabase)> {
        let test_db = match TestDatabase::with_migrations().await {
            Ok(test_db) => test_db,
            Err(error) => {
                println!("Test database not available, skipping: {error}");
                return None;
            }
        };
        let db = test_db.connection_arc();
        let router = temps_nodes::handlers::nodes::configure_admin_routes()
            .with_state(node_admin_state(db.clone()))
            .merge(configure_routes())
            .with_state(create_test_app_state_for_http(db, std::env::temp_dir()).await);
        Some((router, test_db))
    }

    #[tokio::test]
    async fn the_merged_drain_path_serves_status_drain_and_undrain() {
        const DRAIN: &str = "/internal/nodes/4/drain";
        let Some((app, _test_db)) = node_api().await else {
            return;
        };
        for method in ["GET", "POST", "DELETE"] {
            let (status, _, body) = send(app.clone(), method, DRAIN, None, None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method}: {body}");
        }
        let (status, _, _) = send(app, "PUT", DRAIN, None, None).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn draining_a_node_needs_settings_write() {
        let Some((app, _test_db)) = node_api().await else {
            return;
        };
        let (status, _, body) = send(
            app,
            "POST",
            "/internal/nodes/4/drain",
            None,
            Some(settings_reader()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }

    #[tokio::test]
    async fn docker_disk_usage_needs_settings_read() {
        const USAGE: &str = "/nodes/0/docker-disk-usage";
        let Some((app, _test_db)) = node_api().await else {
            return;
        };
        let (status, _, _) = send(app.clone(), "GET", USAGE, None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let (status, _, body) = send(app, "GET", USAGE, None, Some(outsider())).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }

    #[test]
    fn an_unsupported_node_is_a_400_naming_the_problem() {
        let problem = Problem::from(DockerDiskUsageError::UnsupportedNode { node_id: 4 });
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
        let body = serde_json::to_value(&problem.body).unwrap();
        assert_eq!(body["title"], "Docker Disk Usage Not Available For Node");
    }

    // ── Drain refusals for workloads that cannot move ───────────────────

    #[test]
    fn a_drain_with_an_unmovable_workload_is_refused_with_its_details() {
        let reason = "Invalid deployment state: Deployment 12 of project 3, environment 4 was \
                      built from uploaded source 'source-bundles/a.zip', which is no longer \
                      retained on this server. Upload the source again so it is built on an \
                      eligible node";
        let mut unmovable = BoundedReport::default();
        unmovable.push(reason.to_string());
        let problem = drain_refused_problem("worker-a", &unmovable);
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        let body = serde_json::to_string(&problem.body).unwrap();
        assert!(body.contains("was not drained and is unchanged"), "{body}");
        assert!(
            body.contains("Deployment 12 of project 3, environment 4"),
            "{body}"
        );
        // The reason and the advice are separate sentences.
        assert!(body.contains("eligible node. Move or delete"), "{body}");
        assert!(body.contains("drain again"), "{body}");
        assert_eq!(
            problem.body.get("unmovable_workloads"),
            Some(&serde_json::json!([reason]))
        );
    }

    /// However many workloads cannot move, the refusal counts them all but
    /// describes a bounded number.
    #[test]
    fn a_drain_refusal_stays_bounded() {
        let mut unmovable = BoundedReport::default();
        for id in 0..MAX_DESCRIBED_UNREMOVED + 25 {
            unmovable.push(format!("Deployment {id} cannot be rebuilt"));
        }
        let problem = drain_refused_problem("worker-a", &unmovable);
        assert_eq!(
            problem.body.get("unmovable_count"),
            Some(&serde_json::json!(MAX_DESCRIBED_UNREMOVED + 25))
        );
        let listed = problem
            .body
            .get("unmovable_workloads")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(listed.len(), MAX_DESCRIBED_UNREMOVED + 1);
        assert_eq!(
            listed.last(),
            Some(&serde_json::json!("and 25 more not listed here"))
        );
        let body = serde_json::to_string(&problem.body).unwrap();
        assert!(
            body.contains(&format!("{} workload(s)", MAX_DESCRIBED_UNREMOVED + 25)),
            "{body}"
        );
    }

    #[test]
    fn an_incomplete_drain_says_what_happened_and_how_to_retry() {
        let mut failed = BoundedReport::default();
        failed.push("Queue error: queue closed".to_string());
        let reactivated = drain_incomplete_problem(7, "worker-a", &failed, 2, 1, true);
        assert_eq!(reactivated.status_code, StatusCode::INTERNAL_SERVER_ERROR);
        let body = serde_json::to_string(&reactivated.body).unwrap();
        assert!(body.contains("1 redeploy(s) could not be queued"), "{body}");
        assert!(
            body.contains("1 environment(s) were redeployed and 2 container(s) retired"),
            "{body}"
        );
        assert!(body.contains("set back to active"), "{body}");
        assert!(!body.contains("Node drain initiated"), "{body}");

        let still_draining = drain_incomplete_problem(7, "worker-a", &failed, 0, 0, false);
        let body = serde_json::to_string(&still_draining.body).unwrap();
        assert!(body.contains("remains draining"), "{body}");
        // The undrain path names this node, not a placeholder.
        assert!(body.contains("DELETE /internal/nodes/7/drain"), "{body}");
        assert!(!body.contains("{node_id}"), "{body}");
    }
}
