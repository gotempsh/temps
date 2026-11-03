// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a node drain does with each deployment on the node, decided before anything
//! changes.
//!
//! Container rows can't answer this: a worker that died a second ago still has its
//! rows, and still looks healthy, so retiring this node's replica on their word can
//! retire the last working one. So the other replicas are actually asked, along with
//! every node a replacement could go to.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use futures::StreamExt;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter};
use temps_deployer::ContainerStatus;
use temps_entities::{deployment_containers, environments, projects};
use tracing::warn;

use super::{DeploymentError, DeploymentService};
use crate::services::node_scheduler::{
    is_dedicated_node, NodeScheduler, ReplicaPlacementRequest, CONTROL_PLANE_NODE_ID,
};
use crate::services::node_service::{AffectedDeployment, DrainDecision};
use crate::services::workflow_execution_service::resolve_placement_constraints;

/// How long a node gets to answer. A drain is an operator waiting on a request, and
/// a node this slow isn't one to move work to anyway.
const DRAIN_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Nodes or replicas asked at once. With the timeout above, this bounds how long a
/// drain can spend on nodes that don't answer.
const DRAIN_PROBE_CONCURRENCY: usize = 8;

/// What's known about one of the node's current deployments.
#[derive(Debug, Clone, Copy)]
struct DrainFacts {
    /// Replicas on other nodes that answered as running just now.
    confirmed_elsewhere: usize,
    /// A reachable node other than this one can run it.
    can_place_elsewhere: bool,
    /// One of those can take this replica while honouring anti-affinity.
    has_spare_node: bool,
}

fn decide(facts: DrainFacts, dep: &AffectedDeployment) -> DrainDecision {
    if facts.has_spare_node || (facts.confirmed_elsewhere == 0 && facts.can_place_elsewhere) {
        return DrainDecision::Redeploy;
    }
    if facts.confirmed_elsewhere > 0 {
        return DrainDecision::Retire {
            restore_after_undrain: true,
        };
    }
    DrainDecision::Refuse {
        reason: format!(
            "deployment {} of project {} (environment {}) has no replica confirmed running on \
             another node, and no reachable node it can move to; add a target node or bring one \
             back",
            dep.deployment_id, dep.project_id, dep.environment_id
        ),
    }
}

/// Nodes that answered just now, other than the one draining.
#[derive(Debug, Default)]
struct ReachableNodes {
    ids: HashSet<i32>,
    /// Those that only run environments naming them by ID.
    dedicated: HashSet<i32>,
}

/// Reachable nodes this environment could run on, other than the one draining.
/// `target_nodes` names the control plane as [`CONTROL_PLANE_NODE_ID`]; without
/// targets the control plane is a candidate when it runs workloads at all, and a
/// dedicated node isn't one. The scheduler is asked about these by ID, which would
/// admit a dedicated node the real, unpinned redeploy can't use.
fn candidate_nodes(
    target_nodes: Option<&[i32]>,
    nodes: &ReachableNodes,
    draining_node_id: i32,
    local_workloads_enabled: bool,
) -> Vec<i32> {
    let reachable = &nodes.ids;
    let is_candidate = |id: &i32| *id != draining_node_id && reachable.contains(id);
    let mut candidates: Vec<i32> = match target_nodes.filter(|ids| !ids.is_empty()) {
        Some(targets) => targets
            .iter()
            .copied()
            .filter(|id| {
                is_candidate(id) || *id == CONTROL_PLANE_NODE_ID && local_workloads_enabled
            })
            .collect(),
        None => {
            let mut all: Vec<i32> = reachable
                .iter()
                .copied()
                .filter(|id| is_candidate(id) && !nodes.dedicated.contains(id))
                .collect();
            if local_workloads_enabled {
                all.push(CONTROL_PLANE_NODE_ID);
            }
            all
        }
    };
    candidates.sort_unstable();
    candidates.dedup();
    candidates
}

impl DeploymentService {
    /// Decide what draining `node_id` does with each deployment in `affected`.
    /// Reads only: nothing changes until the caller acts on the decisions.
    /// Deployments an environment no longer serves get no decision, and are retired.
    pub async fn plan_node_drain(
        &self,
        scheduler: &NodeScheduler,
        node_id: i32,
        affected: &mut [AffectedDeployment],
    ) -> Result<(), DeploymentError> {
        let current_ids: Vec<i32> = affected
            .iter()
            .filter(|dep| dep.is_current)
            .map(|dep| dep.deployment_id)
            .collect();
        if current_ids.is_empty() {
            return Ok(());
        }

        let reachable = self.reachable_nodes(scheduler, node_id).await?;

        let replicas = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.is_in(current_ids))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .filter(
                Condition::any()
                    .add(deployment_containers::Column::NodeId.ne(node_id))
                    .add(deployment_containers::Column::NodeId.is_null()),
            )
            .all(self.db.as_ref())
            .await?;
        let mut hosting: HashMap<i32, HashSet<i32>> = HashMap::new();
        for replica in &replicas {
            hosting
                .entry(replica.deployment_id)
                .or_default()
                .insert(replica.node_id.unwrap_or(CONTROL_PLANE_NODE_ID));
        }
        let confirmed = self.confirm_running(replicas, &reachable.ids).await;

        let environment_ids: Vec<i32> = affected
            .iter()
            .filter(|dep| dep.is_current)
            .map(|dep| dep.environment_id)
            .collect();
        let environments = environments::Entity::find()
            .filter(environments::Column::Id.is_in(environment_ids))
            .all(self.db.as_ref())
            .await?;
        let projects: HashMap<i32, projects::Model> = projects::Entity::find()
            .filter(
                projects::Column::Id.is_in(
                    environments
                        .iter()
                        .map(|environment| environment.project_id)
                        .collect::<HashSet<_>>(),
                ),
            )
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(|project| (project.id, project))
            .collect();
        let rules: HashMap<i32, PlacementRules> = environments
            .iter()
            .map(|environment| {
                let project = projects.get(&environment.project_id);
                let slug = project
                    .map(|project| project.slug.as_str())
                    .filter(|slug| scheduler.docker_socket_grant().declares(slug));
                let rules = PlacementRules::resolve(
                    environment.deployment_config.as_ref(),
                    project.and_then(|project| project.deployment_config.as_ref()),
                    slug,
                );
                (environment.id, rules)
            })
            .collect();

        // Environments mostly share their placement rules, and each scheduling
        // pass reads the node list.
        let mut placeable: HashMap<PlacementQuestion, bool> = HashMap::new();
        let local_workloads = scheduler.local_workloads_enabled();
        for dep in affected.iter_mut().filter(|dep| dep.is_current) {
            let config = rules.get(&dep.environment_id).cloned().unwrap_or_default();
            let candidates = candidate_nodes(
                config.target_nodes.as_deref(),
                &reachable,
                node_id,
                local_workloads,
            );
            let spare: Vec<i32> = if config.anti_affinity {
                let hosts = hosting.get(&dep.deployment_id);
                candidates
                    .iter()
                    .copied()
                    .filter(|id| !hosts.is_some_and(|hosts| hosts.contains(id)))
                    .collect()
            } else {
                candidates.clone()
            };
            let facts = DrainFacts {
                confirmed_elsewhere: confirmed.get(&dep.deployment_id).copied().unwrap_or(0),
                can_place_elsewhere: can_place(
                    scheduler,
                    &candidates,
                    config.target_labels.as_ref(),
                    config.host_access_slug.as_deref(),
                    &mut placeable,
                )
                .await,
                has_spare_node: can_place(
                    scheduler,
                    &spare,
                    config.target_labels.as_ref(),
                    config.host_access_slug.as_deref(),
                    &mut placeable,
                )
                .await,
            };
            dep.drain_decision = Some(decide(facts, dep));
        }
        Ok(())
    }

    /// Worker nodes marked active that answer right now, other than `node_id`. The
    /// mark alone isn't enough: a worker that just died still carries it.
    async fn reachable_nodes(
        &self,
        scheduler: &NodeScheduler,
        node_id: i32,
    ) -> Result<ReachableNodes, DeploymentError> {
        let active = scheduler
            .node_service()
            .list_active(scheduler.heartbeat_threshold_secs())
            .await
            .map_err(|error| {
                DeploymentError::Other(format!(
                    "Could not list active nodes to plan the drain of node {node_id}: {error}"
                ))
            })?;
        let dedicated: HashSet<i32> = active
            .iter()
            .filter(|node| is_dedicated_node(node))
            .map(|node| node.id)
            .collect();
        let probes = active
            .into_iter()
            .map(|node| node.id)
            .filter(|id| *id != node_id)
            .map(|id| async move {
                let deployer = self.deployer_for_node(Some(id)).await.ok()?;
                let answer =
                    tokio::time::timeout(DRAIN_PROBE_TIMEOUT, deployer.list_containers()).await;
                matches!(answer, Ok(Ok(_))).then_some(id)
            });
        let ids = futures::stream::iter(probes)
            .buffer_unordered(DRAIN_PROBE_CONCURRENCY)
            .filter_map(|id| async move { id })
            .collect()
            .await;
        Ok(ReachableNodes { ids, dedicated })
    }

    /// How many of `replicas` each deployment has running, asking their nodes.
    /// Whatever doesn't answer isn't confirmed.
    async fn confirm_running(
        &self,
        replicas: Vec<deployment_containers::Model>,
        reachable: &HashSet<i32>,
    ) -> HashMap<i32, usize> {
        let probes = replicas
            .into_iter()
            .filter(|replica| replica.node_id.is_none_or(|id| reachable.contains(&id)))
            .map(|replica| async move {
                let deployer = self.deployer_for_node(replica.node_id).await.ok()?;
                let answer = tokio::time::timeout(
                    DRAIN_PROBE_TIMEOUT,
                    deployer.get_container_info(&replica.container_id),
                )
                .await;
                match answer {
                    Ok(Ok(info)) if info.status == ContainerStatus::Running => {
                        Some(replica.deployment_id)
                    }
                    Ok(Ok(_)) => None,
                    Ok(Err(error)) => {
                        warn!(
                            deployment_id = replica.deployment_id,
                            container_id = %replica.container_id,
                            "Drain: could not confirm a replica is running: {}",
                            error
                        );
                        None
                    }
                    Err(_) => {
                        warn!(
                            deployment_id = replica.deployment_id,
                            container_id = %replica.container_id,
                            "Drain: timed out confirming a replica is running"
                        );
                        None
                    }
                }
            });
        let running: Vec<Option<i32>> = futures::stream::iter(probes)
            .buffer_unordered(DRAIN_PROBE_CONCURRENCY)
            .collect()
            .await;
        let mut counts = HashMap::new();
        for deployment_id in running.into_iter().flatten() {
            *counts.entry(deployment_id).or_insert(0) += 1;
        }
        counts
    }
}

/// Target nodes, labels and the slug of a project needing host Docker access: what
/// makes one scheduling pass differ from another.
type PlacementQuestion = (Vec<i32>, Option<String>, Option<String>);

/// Where a replacement for this environment's replica may go, worked out the way a
/// deployment works it out, so a drain can't approve a node the replacement can't use.
#[derive(Clone, Debug, PartialEq)]
struct PlacementRules {
    target_nodes: Option<Vec<i32>>,
    target_labels: Option<serde_json::Value>,
    anti_affinity: bool,
    /// Only set for a project this control plane declares for host Docker access,
    /// the one case where its identity changes the answer. Leaving it off for the
    /// rest lets environments with the same rules share a scheduling pass.
    host_access_slug: Option<String>,
}

impl Default for PlacementRules {
    fn default() -> Self {
        Self::resolve(None, None, None)
    }
}

impl PlacementRules {
    fn resolve(
        environment: Option<&temps_entities::deployment_config::DeploymentConfig>,
        project: Option<&temps_entities::deployment_config::DeploymentConfig>,
        host_access_slug: Option<&str>,
    ) -> Self {
        let (target_nodes, target_labels) = resolve_placement_constraints(environment, project);
        Self {
            target_nodes,
            target_labels,
            anti_affinity: environment
                .or(project)
                .is_none_or(|config| config.anti_affinity),
            host_access_slug: host_access_slug.map(str::to_string),
        }
    }
}

/// Whether the scheduler would place one replica on these nodes under these labels.
async fn can_place(
    scheduler: &NodeScheduler,
    candidates: &[i32],
    labels: Option<&serde_json::Value>,
    host_access_slug: Option<&str>,
    known: &mut HashMap<PlacementQuestion, bool>,
) -> bool {
    // Empty target ids mean "no constraint" to the scheduler, not "nowhere".
    if candidates.is_empty() {
        return false;
    }
    let key = (
        candidates.to_vec(),
        labels.map(|labels| labels.to_string()),
        host_access_slug.map(str::to_string),
    );
    if let Some(answer) = known.get(&key) {
        return *answer;
    }
    let answer = scheduler
        .schedule_placement(ReplicaPlacementRequest {
            replica_count: 1,
            labels,
            target_node_ids: Some(candidates),
            anti_affinity: false,
            exclude_node_ids: &[],
            image_platforms: &[],
            project_slug: host_access_slug,
            exclude_control_plane: false,
        })
        .await
        .is_ok_and(|outcome| !outcome.assignments.is_empty());
    known.insert(key, answer);
    answer
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_entities::deployment_config::DeploymentConfig;

    /// A project pinned to the node being drained has nowhere else to go, even
    /// though other nodes are up, unless its environment says otherwise.
    #[test]
    fn an_environment_with_no_rules_of_its_own_follows_its_project() {
        let project = DeploymentConfig {
            target_nodes: Some(vec![4]),
            target_labels: Some(serde_json::json!({"gpu": "true"})),
            anti_affinity: false,
            ..Default::default()
        };
        let rules = PlacementRules::resolve(None, Some(&project), None);
        assert_eq!(rules.target_nodes, Some(vec![4]));
        assert_eq!(
            rules.target_labels,
            Some(serde_json::json!({"gpu": "true"}))
        );
        assert!(!rules.anti_affinity);
    }

    #[test]
    fn an_environments_own_rules_win_and_an_empty_one_inherits() {
        let project = DeploymentConfig {
            target_nodes: Some(vec![4]),
            anti_affinity: false,
            ..Default::default()
        };
        let own = DeploymentConfig {
            target_nodes: Some(vec![5]),
            ..Default::default()
        };
        let rules = PlacementRules::resolve(Some(&own), Some(&project), None);
        assert_eq!(rules.target_nodes, Some(vec![5]));
        assert!(rules.anti_affinity, "the environment's setting applies");

        let cleared = DeploymentConfig {
            target_nodes: Some(vec![]),
            ..Default::default()
        };
        let rules = PlacementRules::resolve(Some(&cleared), Some(&project), None);
        assert_eq!(rules.target_nodes, Some(vec![4]));
    }

    #[test]
    fn no_settings_anywhere_spreads_replicas_without_constraints() {
        let rules = PlacementRules::default();
        assert_eq!(rules.target_nodes, None);
        assert_eq!(rules.target_labels, None);
        assert!(rules.anti_affinity);
    }

    fn dep() -> AffectedDeployment {
        AffectedDeployment {
            project_id: 1,
            environment_id: 2,
            deployment_id: 3,
            is_current: true,
            containers_on_node: 1,
            total_active_containers: 2,
            drain_decision: None,
        }
    }

    fn facts(confirmed: usize, place: bool, spare: bool) -> DrainFacts {
        DrainFacts {
            confirmed_elsewhere: confirmed,
            can_place_elsewhere: place,
            has_spare_node: spare,
        }
    }

    /// The reported outage: the row for the other replica is there, but its worker
    /// just died, and nowhere else can run it. Retiring would take the app down.
    #[test]
    fn nothing_confirmed_and_nowhere_to_go_is_refused_not_retired() {
        let decision = decide(facts(0, false, false), &dep());
        let DrainDecision::Refuse { reason } = decision else {
            panic!("expected a refusal, got {decision:?}");
        };
        assert!(reason.contains("deployment 3"), "{reason}");
        assert!(reason.contains("project 1"), "{reason}");
    }

    #[test]
    fn nothing_confirmed_with_somewhere_to_go_starts_a_replacement_first() {
        assert_eq!(
            decide(facts(0, true, false), &dep()),
            DrainDecision::Redeploy
        );
        assert_eq!(
            decide(facts(0, true, true), &dep()),
            DrainDecision::Redeploy
        );
    }

    #[test]
    fn a_confirmed_replica_with_a_spare_node_keeps_the_replica_count() {
        assert_eq!(
            decide(facts(1, true, true), &dep()),
            DrainDecision::Redeploy
        );
    }

    /// Every other eligible node already hosts one, and anti-affinity won't stack
    /// them: run short for now, and restore it on undrain.
    #[test]
    fn a_confirmed_replica_with_no_spare_node_retires_and_restores_on_undrain() {
        assert_eq!(
            decide(facts(1, true, false), &dep()),
            DrainDecision::Retire {
                restore_after_undrain: true
            }
        );
    }

    fn reachable(ids: &[i32]) -> ReachableNodes {
        ReachableNodes {
            ids: ids.iter().copied().collect(),
            dedicated: HashSet::new(),
        }
    }

    /// An unpinned redeploy can't use a dedicated node, so it can't count as
    /// somewhere to go, unless the environment names it.
    #[test]
    fn a_dedicated_node_is_a_candidate_only_when_targeted() {
        let nodes = ReachableNodes {
            ids: [2, 3].into(),
            dedicated: [3].into(),
        };
        assert_eq!(candidate_nodes(None, &nodes, 1, false), vec![2]);
        assert_eq!(candidate_nodes(Some(&[3]), &nodes, 1, false), vec![3]);
    }

    #[test]
    fn candidates_honour_targets_and_leave_out_the_draining_and_unreachable_nodes() {
        let reachable = reachable(&[2, 3]);
        assert_eq!(
            candidate_nodes(Some(&[1, 2, 4]), &reachable, 1, false),
            vec![2]
        );
        assert_eq!(
            candidate_nodes(Some(&[1]), &reachable, 1, false),
            Vec::<i32>::new()
        );
        assert_eq!(candidate_nodes(None, &reachable, 1, false), vec![2, 3]);
    }

    #[test]
    fn the_control_plane_is_a_candidate_only_when_it_runs_workloads() {
        let reachable = reachable(&[2]);
        assert_eq!(
            candidate_nodes(None, &reachable, 1, true),
            vec![CONTROL_PLANE_NODE_ID, 2]
        );
        assert_eq!(
            candidate_nodes(Some(&[CONTROL_PLANE_NODE_ID]), &reachable, 1, true),
            vec![CONTROL_PLANE_NODE_ID]
        );
        assert_eq!(
            candidate_nodes(Some(&[CONTROL_PLANE_NODE_ID]), &reachable, 1, false),
            Vec::<i32>::new()
        );
    }
}
