// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The invokable replicas of an environment's current deployment (ADR-103).
//!
//! Implements [`ExecutorEndpoints`] for the durable workflows engine: which
//! containers of the current deployment can take an invocation, and the
//! internal address each one is reached on. The address rules are the
//! proxy's (see `build_backend_entry` in `temps-routes`): a worker-node
//! replica on the node's data address and its published host port, a local
//! replica through the process [`RuntimeContext`] (the container name on the
//! Docker network, or `127.0.0.1` and the host port when Temps runs on the
//! host). Unhealthy replicas are left out; replicas whose health is not
//! known yet stay in, as the proxy routes to them too.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{ColumnTrait, Condition, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder};
use temps_core::{
    ExecutionEnvironment, ExecutorEndpoint, ExecutorEndpointSet, ExecutorEndpoints,
    ExecutorEndpointsError, RuntimeContext, ServiceEndpointScheme,
};
use temps_entities::{deployment_containers, deployments, environments, nodes};

/// `deployment_containers.health_state` of a replica the monitor saw fail.
const UNHEALTHY: &str = "unhealthy";

pub struct DeploymentExecutorEndpoints {
    db: Arc<DatabaseConnection>,
    runtime_context: Arc<RuntimeContext>,
}

impl DeploymentExecutorEndpoints {
    pub fn new(db: Arc<DatabaseConnection>, runtime_context: Arc<RuntimeContext>) -> Self {
        Self {
            db,
            runtime_context,
        }
    }

    fn lookup_error(
        project_id: i32,
        environment_id: i32,
        what: &str,
        error: sea_orm::DbErr,
    ) -> ExecutorEndpointsError {
        ExecutorEndpointsError::Lookup {
            project_id,
            environment_id,
            reason: format!("loading {what} failed: {error}"),
        }
    }
}

/// Why replicas of the current deployment were left out, for the reason
/// shown when none is left.
#[derive(Debug, Default, PartialEq, Eq)]
struct Skipped {
    unhealthy: usize,
    no_port: usize,
    unreachable: usize,
}

/// The replicas that can be invoked, in container id order, and why the rest
/// were skipped. Pure: `node_addresses` maps a node id to its data address.
fn select_endpoints(
    containers: &[deployment_containers::Model],
    node_addresses: &HashMap<i32, String>,
    runtime_context: &RuntimeContext,
) -> (Vec<ExecutorEndpoint>, Skipped) {
    let mut endpoints = Vec::new();
    let mut skipped = Skipped::default();
    for container in containers {
        if container.health_state == UNHEALTHY {
            skipped.unhealthy += 1;
            continue;
        }
        let Some(container_port) = container.container_port else {
            skipped.no_port += 1;
            continue;
        };
        match replica_authority(container, container_port, node_addresses, runtime_context) {
            Some(authority) => endpoints.push(ExecutorEndpoint {
                base_url: format!("http://{authority}"),
                container_id: container.id,
                node_id: container.node_id,
            }),
            None => skipped.unreachable += 1,
        }
    }
    (endpoints, skipped)
}

/// `host:port` of one replica, or `None` when it cannot be reached: a
/// worker-node replica whose node is gone, or a replica that must be reached
/// on a published host port that Docker has not reported (dialing the
/// container port instead could reach an unrelated process).
fn replica_authority(
    container: &deployment_containers::Model,
    container_port: i32,
    node_addresses: &HashMap<i32, String>,
    runtime_context: &RuntimeContext,
) -> Option<String> {
    let port = |value: i32| u16::try_from(value).ok();
    match container.node_id {
        Some(node_id) => {
            let address = node_addresses.get(&node_id)?;
            let host_port = port(container.host_port?)?;
            Some(match address.parse::<std::net::IpAddr>() {
                // Brackets an IPv6 address.
                Ok(ip) => std::net::SocketAddr::new(ip, host_port).to_string(),
                Err(_) => format!("{address}:{host_port}"),
            })
        }
        None => {
            if runtime_context.execution_environment() == ExecutionEnvironment::Host
                && container.host_port.is_none()
            {
                return None;
            }
            let target = port(container_port)?;
            let published = port(container.host_port.unwrap_or(container_port))?;
            Some(
                runtime_context
                    .resolve_service_endpoint(
                        &container.container_name,
                        ServiceEndpointScheme::Http,
                        target,
                        published,
                    )
                    .authority(),
            )
        }
    }
}

fn unavailable_reason(deployment_id: i32, total: usize, skipped: &Skipped) -> String {
    if total == 0 {
        return format!("Deployment {deployment_id} has no running replica");
    }
    let mut parts = Vec::new();
    if skipped.unhealthy > 0 {
        parts.push(format!("{} unhealthy", skipped.unhealthy));
    }
    if skipped.no_port > 0 {
        parts.push(format!("{} exposing no HTTP port", skipped.no_port));
    }
    if skipped.unreachable > 0 {
        parts.push(format!(
            "{} without a reachable address yet",
            skipped.unreachable
        ));
    }
    format!(
        "No replica of deployment {deployment_id} can take an invocation ({})",
        parts.join(", ")
    )
}

#[async_trait]
impl ExecutorEndpoints for DeploymentExecutorEndpoints {
    async fn endpoints(
        &self,
        project_id: i32,
        environment_id: i32,
        service: Option<&str>,
    ) -> Result<ExecutorEndpointSet, ExecutorEndpointsError> {
        let db = self.db.as_ref();
        let environment = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::DeletedAt.is_null())
            .one(db)
            .await
            .map_err(|e| Self::lookup_error(project_id, environment_id, "the environment", e))?
            .ok_or(ExecutorEndpointsError::EnvironmentNotFound {
                project_id,
                environment_id,
            })?;
        let Some(deployment_id) = environment.current_deployment_id else {
            return Ok(ExecutorEndpointSet::unavailable(
                None,
                "No deployment is live in this environment yet",
            ));
        };
        let deployment = deployments::Entity::find_by_id(deployment_id)
            .one(db)
            .await
            .map_err(|e| Self::lookup_error(project_id, environment_id, "the deployment", e))?;
        match deployment {
            None => {
                return Ok(ExecutorEndpointSet::unavailable(
                    Some(deployment_id),
                    format!("The current deployment {deployment_id} no longer exists"),
                ))
            }
            Some(deployment) if deployment.state == "paused" => {
                return Ok(ExecutorEndpointSet::unavailable(
                    Some(deployment_id),
                    format!(
                        "The current deployment {deployment_id} is paused; resume it to run workflows"
                    ),
                ))
            }
            Some(_) => {}
        }

        let mut query = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .filter(
                Condition::any()
                    .add(deployment_containers::Column::Status.is_null())
                    .add(deployment_containers::Column::Status.eq("running")),
            );
        query = match service {
            Some(name) => query.filter(deployment_containers::Column::ServiceName.eq(name)),
            None => query.filter(deployment_containers::Column::ServiceName.is_null()),
        };
        let containers = query
            .order_by_asc(deployment_containers::Column::Id)
            .all(db)
            .await
            .map_err(|e| Self::lookup_error(project_id, environment_id, "the replicas", e))?;

        if containers.is_empty() && service.is_none() {
            // A Compose deployment names every service: say which to pick.
            let services: Vec<String> = deployment_containers::Entity::find()
                .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
                .filter(deployment_containers::Column::DeletedAt.is_null())
                .filter(deployment_containers::Column::ServiceName.is_not_null())
                .order_by_asc(deployment_containers::Column::ServiceName)
                .all(db)
                .await
                .map_err(|e| Self::lookup_error(project_id, environment_id, "the services", e))?
                .into_iter()
                .filter_map(|container| container.service_name)
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            if !services.is_empty() {
                return Ok(ExecutorEndpointSet::unavailable(
                    Some(deployment_id),
                    format!(
                        "Deployment {deployment_id} runs Compose services ({}); choose the one serving workflows in the workflow settings",
                        services.join(", ")
                    ),
                ));
            }
        }

        let node_ids: HashSet<i32> = containers.iter().filter_map(|c| c.node_id).collect();
        let node_addresses: HashMap<i32, String> = if node_ids.is_empty() {
            HashMap::new()
        } else {
            nodes::Entity::find()
                .filter(nodes::Column::Id.is_in(node_ids))
                .all(db)
                .await
                .map_err(|e| Self::lookup_error(project_id, environment_id, "the nodes", e))?
                .into_iter()
                .map(|node| (node.id, node.data_address().to_string()))
                .collect()
        };

        let (endpoints, skipped) =
            select_endpoints(&containers, &node_addresses, &self.runtime_context);
        if endpoints.is_empty() {
            return Ok(ExecutorEndpointSet::unavailable(
                Some(deployment_id),
                unavailable_reason(deployment_id, containers.len(), &skipped),
            ));
        }
        Ok(ExecutorEndpointSet {
            deployment_id: Some(deployment_id),
            endpoints,
            unavailable_reason: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, Set};
    use temps_entities::preset::Preset;
    use temps_entities::projects;
    use temps_entities::upstream_config::UpstreamList;
    use temps_test_utils::TestDatabase;

    fn container(id: i32) -> deployment_containers::Model {
        deployment_containers::Model {
            id,
            deployment_id: 1,
            container_id: format!("c{id}"),
            container_name: format!("app-{id}"),
            container_port: Some(3000),
            health_state: "healthy".into(),
            health_checked_at: None,
            health_detail: None,
            host_port: Some(40000 + id),
            image_name: None,
            status: Some("running".into()),
            service_name: None,
            created_at: chrono::Utc::now(),
            deployed_at: chrono::Utc::now(),
            ready_at: None,
            deleted_at: None,
            node_id: None,
            exit_code: None,
            exit_reason: None,
            oom_killed: None,
            error_message: None,
            finished_at: None,
            started_at: None,
            cpu_limit_cores: None,
            port_bindings: None,
        }
    }

    #[test]
    fn docker_replicas_use_the_container_name_and_port() {
        let (endpoints, skipped) = select_endpoints(
            &[container(1), container(2)],
            &HashMap::new(),
            &RuntimeContext::docker(),
        );
        assert_eq!(skipped, Skipped::default());
        let urls: Vec<_> = endpoints.iter().map(|e| e.base_url.as_str()).collect();
        assert_eq!(urls, ["http://app-1:3000", "http://app-2:3000"]);
        assert_eq!(endpoints[0].container_id, 1);
        assert_eq!(endpoints[0].node_id, None);
    }

    #[test]
    fn host_replicas_use_loopback_and_the_published_port_or_are_skipped() {
        let mut unpublished = container(2);
        unpublished.host_port = None;
        let (endpoints, skipped) = select_endpoints(
            &[container(1), unpublished],
            &HashMap::new(),
            &RuntimeContext::host(),
        );
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].base_url, "http://127.0.0.1:40001");
        assert_eq!(skipped.unreachable, 1);
    }

    #[test]
    fn worker_replicas_use_the_node_data_address_and_host_port() {
        let mut v4 = container(1);
        v4.node_id = Some(7);
        let mut v6 = container(2);
        v6.node_id = Some(8);
        let mut gone = container(3);
        gone.node_id = Some(9);
        let mut unpublished = container(4);
        unpublished.node_id = Some(7);
        unpublished.host_port = None;
        let nodes = HashMap::from([(7, "10.201.0.2".to_string()), (8, "fc00::1".to_string())]);
        let (endpoints, skipped) = select_endpoints(
            &[v4, v6, gone, unpublished],
            &nodes,
            &RuntimeContext::docker(),
        );
        let urls: Vec<_> = endpoints.iter().map(|e| e.base_url.as_str()).collect();
        assert_eq!(urls, ["http://10.201.0.2:40001", "http://[fc00::1]:40002"]);
        assert_eq!(endpoints[0].node_id, Some(7));
        assert_eq!(skipped.unreachable, 2);
    }

    #[test]
    fn unhealthy_and_portless_replicas_are_skipped_but_unknown_health_is_kept() {
        let mut unhealthy = container(1);
        unhealthy.health_state = UNHEALTHY.into();
        let mut portless = container(2);
        portless.container_port = None;
        let mut unknown = container(3);
        unknown.health_state = "unknown".into();
        let (endpoints, skipped) = select_endpoints(
            &[unhealthy, portless, unknown],
            &HashMap::new(),
            &RuntimeContext::docker(),
        );
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].container_id, 3);
        assert_eq!(
            skipped,
            Skipped {
                unhealthy: 1,
                no_port: 1,
                unreachable: 0
            }
        );
        assert_eq!(
            unavailable_reason(5, 3, &skipped),
            "No replica of deployment 5 can take an invocation (1 unhealthy, 1 exposing no HTTP port)"
        );
        assert_eq!(
            unavailable_reason(5, 0, &Skipped::default()),
            "Deployment 5 has no running replica"
        );
    }

    struct Seeded {
        project_id: i32,
        environment_id: i32,
    }

    async fn seed(db: &Arc<DatabaseConnection>) -> Seeded {
        let now = chrono::Utc::now();
        let project = projects::ActiveModel {
            name: Set("Executor Endpoints".into()),
            slug: Set("executor-endpoints".into()),
            repo_owner: Set("owner".into()),
            repo_name: Set("repo".into()),
            main_branch: Set("main".into()),
            preset: Set(Preset::NextJs),
            directory: Set("/".into()),
            created_at: Set(now),
            updated_at: Set(now),
            is_deleted: Set(false),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("prod".into()),
            slug: Set("prod".into()),
            host: Set("app.example.test".into()),
            subdomain: Set("app.example.test".into()),
            upstreams: Set(UpstreamList::default()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        Seeded {
            project_id: project.id,
            environment_id: environment.id,
        }
    }

    async fn deploy(db: &Arc<DatabaseConnection>, seeded: &Seeded, state: &str) -> i32 {
        let now = chrono::Utc::now();
        let deployment = deployments::ActiveModel {
            project_id: Set(seeded.project_id),
            environment_id: Set(seeded.environment_id),
            slug: Set(format!("d-{state}")),
            state: Set(state.into()),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        let mut environment: environments::ActiveModel =
            environments::Entity::find_by_id(seeded.environment_id)
                .one(db.as_ref())
                .await
                .unwrap()
                .unwrap()
                .into();
        environment.current_deployment_id = Set(Some(deployment.id));
        environment.update(db.as_ref()).await.unwrap();
        deployment.id
    }

    async fn add_container(
        db: &Arc<DatabaseConnection>,
        deployment_id: i32,
        name: &str,
        service: Option<&str>,
        status: &str,
    ) -> i32 {
        let now = chrono::Utc::now();
        deployment_containers::ActiveModel {
            deployment_id: Set(deployment_id),
            container_id: Set(name.into()),
            container_name: Set(name.into()),
            container_port: Set(Some(3000)),
            host_port: Set(Some(41000)),
            health_state: Set("healthy".into()),
            status: Set(Some(status.into())),
            service_name: Set(service.map(str::to_string)),
            created_at: Set(now),
            deployed_at: Set(now),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap()
        .id
    }

    #[tokio::test]
    async fn resolves_the_current_deployments_running_replicas() {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Postgres not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let seeded = seed(&db).await;
        let resolver =
            DeploymentExecutorEndpoints::new(db.clone(), Arc::new(RuntimeContext::docker()));

        let none = resolver
            .endpoints(seeded.project_id, seeded.environment_id, None)
            .await
            .unwrap();
        assert_eq!(none.deployment_id, None);
        assert_eq!(
            none.unavailable_reason.as_deref(),
            Some("No deployment is live in this environment yet")
        );

        // Another project cannot read this environment's replicas.
        let foreign = resolver
            .endpoints(seeded.project_id + 1000, seeded.environment_id, None)
            .await
            .unwrap_err();
        assert!(matches!(
            foreign,
            ExecutorEndpointsError::EnvironmentNotFound { environment_id, .. }
                if environment_id == seeded.environment_id
        ));

        let deployment_id = deploy(&db, &seeded, "completed").await;
        let running = add_container(&db, deployment_id, "web-a", None, "running").await;
        add_container(&db, deployment_id, "web-b", None, "exited").await;
        let set = resolver
            .endpoints(seeded.project_id, seeded.environment_id, None)
            .await
            .unwrap();
        assert_eq!(set.deployment_id, Some(deployment_id));
        assert_eq!(set.unavailable_reason, None);
        assert_eq!(
            set.endpoints,
            vec![ExecutorEndpoint {
                base_url: "http://web-a:3000".into(),
                container_id: running,
                node_id: None,
            }]
        );
    }

    #[tokio::test]
    async fn compose_services_are_selected_by_name_and_paused_deployments_explain_themselves() {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Postgres not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let seeded = seed(&db).await;
        let resolver =
            DeploymentExecutorEndpoints::new(db.clone(), Arc::new(RuntimeContext::docker()));

        let deployment_id = deploy(&db, &seeded, "completed").await;
        add_container(&db, deployment_id, "stack-web", Some("web"), "running").await;
        let worker = add_container(
            &db,
            deployment_id,
            "stack-worker",
            Some("worker"),
            "running",
        )
        .await;

        let unnamed = resolver
            .endpoints(seeded.project_id, seeded.environment_id, None)
            .await
            .unwrap();
        assert_eq!(
            unnamed.unavailable_reason.as_deref(),
            Some(
                format!(
                    "Deployment {deployment_id} runs Compose services (web, worker); choose the one serving workflows in the workflow settings"
                )
                .as_str()
            )
        );
        let named = resolver
            .endpoints(seeded.project_id, seeded.environment_id, Some("worker"))
            .await
            .unwrap();
        assert_eq!(named.endpoints.len(), 1);
        assert_eq!(named.endpoints[0].container_id, worker);
        assert_eq!(named.endpoints[0].base_url, "http://stack-worker:3000");

        let paused = deploy(&db, &seeded, "paused").await;
        let set = resolver
            .endpoints(seeded.project_id, seeded.environment_id, None)
            .await
            .unwrap();
        assert_eq!(set.deployment_id, Some(paused));
        assert!(set.endpoints.is_empty());
        assert!(set
            .unavailable_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("is paused")));
    }
}
