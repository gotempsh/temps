// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Running containers: runtime resolution per node, logs, listings, detail,
//! start/stop/restart, environment variables and metrics.

use super::*;

/// Boxed log stream so local-Docker and remote-agent paths share one return type.
pub type ContainerLogStream =
    Pin<Box<dyn Stream<Item = Result<String, std::io::Error>> + Send + 'static>>;

/// Parameters for container log retrieval
#[derive(Debug, Clone)]
pub struct ContainerLogParams {
    pub start_date: Option<i64>,
    pub end_date: Option<i64>,
    pub tail: Option<String>,
    pub timestamps: bool,
    pub follow: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ResolvedContainerResourceLimits {
    pub cpu_request: Option<i32>,
    pub cpu_limit: Option<i32>,
    pub memory_request: Option<i32>,
    pub memory_limit: Option<i32>,
}

/// Runtime inspection paired with the persisted deployment health and port state.
#[derive(Debug)]
pub struct EnvironmentContainer {
    pub info: temps_deployer::ContainerInfo,
    pub row: temps_entities::deployment_containers::Model,
}

#[derive(Debug, Clone)]
pub struct ContainerPresentationContext {
    pub node_names: HashMap<i32, String>,
    pub app_settings: temps_core::AppSettings,
    pub environment_subdomain: String,
    pub public_ports: Vec<temps_entities::preset::ComposePublicPort>,
    pub resource_limits: ResolvedContainerResourceLimits,
}

impl DeploymentService {
    pub async fn container_presentation_context(
        &self,
        project_id: i32,
        environment_id: i32,
        node_ids: &[i32],
    ) -> Result<ContainerPresentationContext, DeploymentError> {
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!(
                    "Project {project_id} not found while resolving container presentation"
                ))
            })?;
        let environment = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!(
                    "Environment {environment_id} not found in project {project_id} while resolving container presentation"
                ))
            })?;
        let app_settings = self.config_service.get_settings().await.map_err(|error| {
            DeploymentError::Other(format!(
                "Failed to load application settings for containers in project {project_id}, environment {environment_id}: {error}"
            ))
        })?;
        let node_names = if node_ids.is_empty() {
            HashMap::new()
        } else {
            nodes::Entity::find()
                .filter(nodes::Column::Id.is_in(node_ids.iter().copied()))
                .all(self.db.as_ref())
                .await?
                .into_iter()
                .map(|node| (node.id, node.name))
                .collect()
        };
        let public_ports = match project.preset_config.as_ref() {
            Some(temps_entities::preset::PresetConfig::DockerCompose(config)) => {
                config.public_ports.clone()
            }
            _ => Vec::new(),
        };
        let environment_config = environment.deployment_config.as_ref();
        let project_config = project.deployment_config.as_ref();
        let resolve =
            |get: fn(&temps_entities::deployment_config::DeploymentConfig) -> Option<i32>| {
                environment_config
                    .and_then(get)
                    .or_else(|| project_config.and_then(get))
            };

        Ok(ContainerPresentationContext {
            node_names,
            app_settings,
            environment_subdomain: environment.subdomain,
            public_ports,
            resource_limits: ResolvedContainerResourceLimits {
                cpu_request: resolve(|config| config.cpu_request),
                cpu_limit: resolve(|config| config.cpu_limit),
                memory_request: resolve(|config| config.memory_request),
                memory_limit: resolve(|config| config.memory_limit),
            },
        })
    }

    pub async fn get_filtered_container_logs(
        &self,
        project_id: i32,
        environment_id: i32,
        container_name: Option<String>,
        params: ContainerLogParams,
    ) -> Result<ContainerLogStream, DeploymentError> {
        use temps_entities::{deployment_containers, projects};
        let project = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Project not found".to_string()))?;

        if project.preset == temps_entities::preset::Preset::Static {
            return Err(DeploymentError::Other(
                "Container logs are only available for server-type projects".to_string(),
            ));
        }

        let environment = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Environment not found".to_string()))?;
        if environment.current_deployment_id.is_none() {
            return Err(DeploymentError::NotFound(
                "Deployment not found".to_string(),
            ));
        }
        let deployment_id = environment
            .current_deployment_id
            .ok_or_else(|| DeploymentError::NotFound("Deployment not found".to_string()))?;

        // Get container from deployment_containers table
        // If container_name is specified, filter by name; otherwise get the first/primary container
        let mut query = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_containers::Column::DeletedAt.is_null());

        if let Some(name) = container_name.as_ref() {
            query = query.filter(deployment_containers::Column::ContainerName.eq(name));
        }

        let container = query.one(self.db.as_ref()).await?.ok_or_else(|| {
            if let Some(name) = container_name {
                DeploymentError::NotFound(format!("Container '{}' not found for deployment", name))
            } else {
                DeploymentError::NotFound("No containers found for deployment".to_string())
            }
        })?;

        let container_id = container.container_id;
        self.container_operations_for_node(container.node_id)
            .await?
            .stream_logs(&container_id, params)
            .await
    }

    /// Get logs for a specific container by container ID.
    ///
    /// Routes by `deployment_containers.node_id`: when `None` (local
    /// container) we hit the in-process `DockerLogService`; when `Some`,
    /// we proxy a chunked HTTP stream from the agent on that node so the
    /// caller never needs to know the container is remote.
    pub async fn get_container_logs_by_id(
        &self,
        project_id: i32,
        environment_id: i32,
        container_id: String,
        params: ContainerLogParams,
    ) -> Result<ContainerLogStream, DeploymentError> {
        use temps_entities::{deployment_containers, projects};

        // Verify project exists and is a server-type project
        let project = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Project not found".to_string()))?;

        if project.preset == temps_entities::preset::Preset::Static {
            return Err(DeploymentError::Other(
                "Container logs are only available for server-type projects".to_string(),
            ));
        }

        // Verify environment exists and belongs to the project
        let environment = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Environment not found".to_string()))?;

        // Resolve any still-live container in this environment, including a
        // failed Compose candidate retained for debugging. Do not constrain
        // this to `current_deployment_id`: failed candidates are deliberately
        // never promoted, but their logs remain an authenticated project
        // debugging surface until retry/delete cleanup.
        let container = deployment_containers::Entity::find()
            .inner_join(deployments::Entity)
            .filter(deployments::Column::ProjectId.eq(project_id))
            .filter(deployments::Column::EnvironmentId.eq(environment.id))
            .filter(deployment_containers::Column::ContainerId.eq(&container_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!(
                    "Container {} not found in environment",
                    container_id
                ))
            })?;

        self.container_operations_for_node(container.node_id)
            .await?
            .stream_logs(&container_id, params)
            .await
    }

    /// Return the right `ContainerDeployer` for a container based on where it
    /// runs: `None` → the local CP dockerd; `Some(node_id)` → a fresh
    /// `RemoteNodeDeployer` pointing at that worker's agent.
    ///
    /// We construct the remote deployer per call (cheap — it's just a
    /// reqwest Client + URL + token) so we don't have to keep a long-lived
    /// per-node cache that would have to invalidate on token rotation or
    /// node deletion.
    pub(super) async fn deployer_for_node(
        &self,
        node_id: Option<i32>,
    ) -> Result<Arc<dyn temps_deployer::ContainerDeployer>, DeploymentError> {
        Ok(self
            .container_operations_for_node(node_id)
            .await?
            .deployer())
    }

    /// Resolve every operation for a container runtime once, based on the
    /// persisted node placement. Callers do not need local/remote branches.
    pub async fn container_operations_for_node(
        &self,
        node_id: Option<i32>,
    ) -> Result<Arc<dyn ContainerOperations>, DeploymentError> {
        let Some(node_id) = node_id else {
            let docker = self.docker_handle.require()?;
            return Ok(Arc::new(
                LocalContainerOperations::new(
                    docker,
                    self.docker_log_service.clone(),
                    self.deployer.clone(),
                )
                .with_plane(self.workload_plane.get().cloned()),
            ));
        };

        let node = nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound(format!("Node {} not found", node_id)))?;

        let encrypted_token = node.token_encrypted.as_ref().ok_or_else(|| {
            DeploymentError::Other(format!(
                "Node {} has no agent token; cannot reach remote agent",
                node_id
            ))
        })?;
        let token_bytes = self
            .encryption_service
            .decrypt(encrypted_token)
            .map_err(|e| {
                DeploymentError::Other(format!(
                    "Failed to decrypt agent token for node {}: {}",
                    node_id, e
                ))
            })?;
        let token = String::from_utf8(token_bytes).map_err(|e| {
            DeploymentError::Other(format!(
                "Decrypted agent token for node {} is not valid utf-8: {}",
                node_id, e
            ))
        })?;

        let endpoint = crate::cluster_ca::node_agent_endpoint(&node);
        let remote = crate::cluster_ca::build_node_deployer(
            &node,
            token,
            self.config_service.as_ref(),
            self.encryption_service.as_ref(),
        )
        .await
        .map_err(|e| {
            DeploymentError::Other(format!(
                "Failed to build remote deployer for node {}: {}",
                node_id, e
            ))
        })?;
        let client = crate::cluster_ca::build_node_http_client(
            &endpoint,
            self.config_service.as_ref(),
            self.encryption_service.as_ref(),
            None,
        )
        .await
        .map_err(|e| {
            DeploymentError::Other(format!(
                "Failed to build HTTP client for node {}: {}",
                node_id, e
            ))
        })?;
        let terminal_connector = if node.address.starts_with("https://") {
            let config = crate::cluster_ca::cp_ws_client_config(
                &endpoint,
                self.config_service.as_ref(),
                self.encryption_service.as_ref(),
            )
            .await
            .map_err(|error| {
                DeploymentError::Other(format!(
                    "Failed to build terminal TLS client for node {}: {}",
                    node_id, error
                ))
            })?;
            Some(tokio_tungstenite::Connector::Rustls(Arc::new(config)))
        } else {
            None
        };

        Ok(Arc::new(RemoteContainerOperations::new(
            node_id,
            node.name,
            Arc::new(remote),
            client,
            terminal_connector,
        )))
    }

    /// List all containers for a specific environment.
    /// Includes persisted port and deploy-time health state for each inspection.
    pub async fn list_environment_containers(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<Vec<EnvironmentContainer>, DeploymentError> {
        use temps_entities::{deployment_containers, projects};

        // Verify project exists and is a server-type project
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Project not found".to_string()))?;

        if project.preset == temps_entities::preset::Preset::Static {
            return Err(DeploymentError::Other(
                "Containers are only available for server-type projects".to_string(),
            ));
        }

        // Verify environment exists and belongs to the project
        let environment = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Environment not found".to_string()))?;

        let deployment_id = match environment.current_deployment_id {
            Some(id) => id,
            None => return Ok(Vec::new()), // No active deployment, no containers
        };

        // Get all containers for this deployment from the database
        let db_containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        if db_containers.is_empty() {
            return Ok(Vec::new());
        }

        // Get container info from the deployer for each container, routing
        // by `node_id`. Containers placed on a worker node need to be
        // inspected via that worker's agent — calling the local dockerd for
        // them would hit a 404 and silently drop the row from the response
        // (which is exactly the bug this routing fixes).
        let mut container_infos = Vec::new();
        for db_container in db_containers {
            let node_id = db_container.node_id;

            let deployer = match self.deployer_for_node(node_id).await {
                Ok(d) => d,
                Err(e) => {
                    warn!(
                        "Failed to resolve deployer for container {} on node {:?}: {}",
                        db_container.container_id, node_id, e
                    );
                    continue;
                }
            };

            match deployer
                .get_container_info(&db_container.container_id)
                .await
            {
                Ok(info) => container_infos.push(EnvironmentContainer {
                    info,
                    row: db_container,
                }),
                Err(e) => {
                    warn!(
                        "Failed to get info for container {} on node {:?}: {}",
                        db_container.container_id, node_id, e
                    );
                    // Continue with other containers
                }
            }
        }

        Ok(container_infos)
    }

    /// Get detailed information about a specific container
    pub async fn get_container_detail(
        &self,
        project_id: i32,
        environment_id: i32,
        container_id: String,
    ) -> Result<(deployment_containers::Model, DeploymentEnvironment), DeploymentError> {
        use temps_entities::environments;

        // Verify environment belongs to project
        let environment = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Environment not found".to_string()))?;

        // Find the container — supports both short (12-char) and full (64-char) IDs.
        // Compose deployments store short IDs from `docker compose ps`, but
        // `docker inspect` returns full IDs which the frontend may pass back.
        // Try exact match first, then prefix match in both directions.
        let container = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::ContainerId.eq(&container_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?;

        let container = match container {
            Some(c) => c,
            None => {
                // Full ID passed but DB has short ID: query starts with DB value
                // Short ID passed but DB has full ID: DB value starts with query
                let short_id = &container_id[..container_id.len().min(12)];
                deployment_containers::Entity::find()
                    .filter(deployment_containers::Column::ContainerId.starts_with(short_id))
                    .filter(deployment_containers::Column::DeletedAt.is_null())
                    .one(self.db.as_ref())
                    .await?
                    .ok_or_else(|| {
                        DeploymentError::NotFound(format!("Container {} not found", container_id))
                    })?
            }
        };

        // Verify container belongs to a deployment in this environment
        let _deployment = deployments::Entity::find_by_id(container.deployment_id)
            .filter(deployments::Column::EnvironmentId.eq(environment_id))
            .filter(deployments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Deployment not found".to_string()))?;

        let env_info = DeploymentEnvironment {
            id: environment.id,
            name: environment.name,
            slug: environment.slug,
            primary_url: None,
            no_address_reason: None,
            reachability_warning: None,
            domains: vec![], // Could be populated if needed
        };

        Ok((container, env_info))
    }

    /// List containers that have run for an environment — current and
    /// replaced by a later redeploy. Unlike `get_container_detail`, this
    /// does NOT filter out rows with `deleted_at` set, since a redeploy soft
    /// deletes the previous container row and we still want its history
    /// available for metrics lookups.
    ///
    /// `deployment_id` narrows the result to one deployment's containers
    /// (must belong to this environment). `limit` caps how many *replaced*
    /// rows are returned on top of the currently-running ones — an
    /// environment can accumulate hundreds of replaced containers over its
    /// lifetime and returning them all would fan out into that many
    /// concurrent metrics-history requests on the frontend. Currently
    /// running containers (`deleted_at IS NULL`) are never subject to this
    /// cap: a limit truncating a live container would silently drop it from
    /// both the "running" count and its metrics, the exact debugging
    /// scenario this endpoint exists for. Returns all current containers
    /// first (newest first), then the newest `limit` replaced containers,
    /// plus the total count across both groups before the cap was applied.
    pub async fn list_environment_container_history(
        &self,
        project_id: i32,
        environment_id: i32,
        deployment_id: Option<i32>,
        limit: Option<u64>,
    ) -> Result<(Vec<deployment_containers::Model>, u64), DeploymentError> {
        // Verify environment exists and belongs to project
        environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Environment not found".to_string()))?;

        let deployment_ids: Vec<i32> = if let Some(deployment_id) = deployment_id {
            deployments::Entity::find_by_id(deployment_id)
                .filter(deployments::Column::EnvironmentId.eq(environment_id))
                .filter(deployments::Column::ProjectId.eq(project_id))
                .one(self.db.as_ref())
                .await?
                .ok_or_else(|| {
                    DeploymentError::NotFound(format!("Deployment {} not found", deployment_id))
                })?;
            vec![deployment_id]
        } else {
            deployments::Entity::find()
                .filter(deployments::Column::EnvironmentId.eq(environment_id))
                .filter(deployments::Column::ProjectId.eq(project_id))
                .all(self.db.as_ref())
                .await?
                .into_iter()
                .map(|d| d.id)
                .collect()
        };

        if deployment_ids.is_empty() {
            return Ok((Vec::new(), 0));
        }

        let limit = limit.unwrap_or(20).min(100);
        let filter = deployment_containers::Column::DeploymentId.is_in(deployment_ids);

        let total_count = deployment_containers::Entity::find()
            .filter(filter.clone())
            .count(self.db.as_ref())
            .await?;

        let mut containers = deployment_containers::Entity::find()
            .filter(filter.clone())
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .order_by_desc(deployment_containers::Column::DeployedAt)
            .all(self.db.as_ref())
            .await?;

        // `limit` bounds only the replaced containers -- it is not a shared
        // budget with the uncapped current ones above, so a small `limit`
        // can never squeeze out an already-included running container.
        let replaced = deployment_containers::Entity::find()
            .filter(filter)
            .filter(deployment_containers::Column::DeletedAt.is_not_null())
            .order_by_desc(deployment_containers::Column::DeployedAt)
            .limit(limit)
            .all(self.db.as_ref())
            .await?;
        containers.extend(replaced);

        Ok((containers, total_count))
    }

    /// Resolve a container row by docker container_id, including containers
    /// replaced by a later redeploy (`deleted_at` set). Duplicates the
    /// lookup logic of `get_container_detail` minus the `DeletedAt.is_null()`
    /// filters — intended ONLY for read-only historical lookups (e.g.
    /// persisted metrics history) where the container no longer needs to be
    /// live-operable. `stop_container`/`start_container` and similar
    /// operational paths must keep using `get_container_detail`, which
    /// correctly excludes deleted containers.
    pub async fn get_container_row_any(
        &self,
        project_id: i32,
        environment_id: i32,
        container_id: String,
    ) -> Result<deployment_containers::Model, DeploymentError> {
        // Verify environment belongs to project
        environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Environment not found".to_string()))?;

        // Find the container — supports both short (12-char) and full (64-char) IDs.
        // Compose deployments store short IDs from `docker compose ps`, but
        // `docker inspect` returns full IDs which the frontend may pass back.
        // Try exact match first, then prefix match in both directions.
        let container = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::ContainerId.eq(&container_id))
            .one(self.db.as_ref())
            .await?;

        let container = match container {
            Some(c) => c,
            None => {
                // Full ID passed but DB has short ID: query starts with DB value
                // Short ID passed but DB has full ID: DB value starts with query
                let short_id = &container_id[..container_id.len().min(12)];
                deployment_containers::Entity::find()
                    .filter(deployment_containers::Column::ContainerId.starts_with(short_id))
                    .one(self.db.as_ref())
                    .await?
                    .ok_or_else(|| {
                        DeploymentError::NotFound(format!("Container {} not found", container_id))
                    })?
            }
        };

        // Verify container belongs to a deployment in this environment
        deployments::Entity::find_by_id(container.deployment_id)
            .filter(deployments::Column::EnvironmentId.eq(environment_id))
            .filter(deployments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Deployment not found".to_string()))?;

        Ok(container)
    }

    /// Check whether container exec/terminal access is enabled for an
    /// environment after applying project-level defaults and environment-level
    /// overrides.
    pub async fn is_container_exec_enabled(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<bool, DeploymentError> {
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Project not found".to_string()))?;

        let environment = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Environment not found".to_string()))?;

        let enabled = match (
            project.deployment_config.as_ref(),
            environment.deployment_config.as_ref(),
        ) {
            (Some(project_config), Some(environment_config)) => {
                project_config
                    .merge(environment_config)
                    .container_exec_enabled
            }
            (Some(project_config), None) => project_config.container_exec_enabled,
            (None, Some(environment_config)) => environment_config.container_exec_enabled,
            (None, None) => false,
        };

        Ok(enabled)
    }

    /// Stop a specific container
    pub async fn stop_container(
        &self,
        project_id: i32,
        environment_id: i32,
        container_id: String,
    ) -> Result<(), DeploymentError> {
        let (container, _) = self
            .get_container_detail(project_id, environment_id, container_id.clone())
            .await?;
        let deployment_id = container.deployment_id;
        crate::services::plane_deployer::refuse_micro_vm_lifecycle(
            self.workload_plane.get().map(Arc::as_ref),
            &container.container_id,
            "stop",
        )
        .await?;

        // Route to the worker that owns this container — calling the local
        // CP dockerd for a remote container would 404 silently, leaving the
        // container running while the UI thinks it stopped.
        let deployer = self.deployer_for_node(container.node_id).await?;
        deployer
            .stop_container(&container.container_id)
            .await
            .map_err(|e| DeploymentError::Other(format!("Failed to stop container: {}", e)))?;

        // Update container status in database. The stop time lets the health
        // monitor tell this stop apart from a later restart and crash.
        let mut active_container: deployment_containers::ActiveModel = container.into();
        active_container.status = Set(Some("stopped".to_string()));
        active_container.finished_at = Set(Some(chrono::Utc::now()));
        active_container.update(self.db.as_ref()).await?;

        // Same reasoning as `pause_deployment`: this status UPDATE fires no
        // DB trigger, so force an in-process route-table reload or the
        // proxy keeps routing to a container we just told the UI is
        // stopped until some unrelated route change happens to reload it.
        if let Err(e) = self
            .queue_service
            .send(temps_core::Job::ForceRouteReload(
                temps_core::ForceRouteReloadJob {
                    environment_id: Some(environment_id),
                    deployment_id: Some(deployment_id),
                },
            ))
            .await
        {
            warn!(
                "Failed to publish in-process ForceRouteReload after stopping container {}: {} \
                 — falling back to the next PG NOTIFY-triggered reload",
                container_id, e
            );
        }

        info!("Successfully stopped container: {}", container_id);
        Ok(())
    }

    /// Start a stopped container
    pub async fn start_container(
        &self,
        project_id: i32,
        environment_id: i32,
        container_id: String,
    ) -> Result<(), DeploymentError> {
        let (container, _) = self
            .get_container_detail(project_id, environment_id, container_id.clone())
            .await?;
        let deployment_id = container.deployment_id;
        crate::services::plane_deployer::refuse_micro_vm_lifecycle(
            self.workload_plane.get().map(Arc::as_ref),
            &container.container_id,
            "start",
        )
        .await?;

        let deployer = self.deployer_for_node(container.node_id).await?;
        deployer
            .start_container(&container.container_id)
            .await
            .map_err(|e| DeploymentError::Other(format!("Failed to start container: {}", e)))?;

        // Update container status in database
        let mut active_container: deployment_containers::ActiveModel = container.into();
        active_container.status = Set(Some("running".to_string()));
        active_container.update(self.db.as_ref()).await?;

        // Same reasoning as `resume_deployment`: this status UPDATE fires no
        // DB trigger, so force an in-process route-table reload or the
        // proxy keeps treating this container as not-routable until some
        // unrelated route change happens to reload it.
        if let Err(e) = self
            .queue_service
            .send(temps_core::Job::ForceRouteReload(
                temps_core::ForceRouteReloadJob {
                    environment_id: Some(environment_id),
                    deployment_id: Some(deployment_id),
                },
            ))
            .await
        {
            warn!(
                "Failed to publish in-process ForceRouteReload after starting container {}: {} \
                 — falling back to the next PG NOTIFY-triggered reload",
                container_id, e
            );
        }

        info!("Successfully started container: {}", container_id);
        Ok(())
    }

    /// Restart a container (stop and then start)
    pub async fn restart_container(
        &self,
        project_id: i32,
        environment_id: i32,
        container_id: String,
    ) -> Result<(), DeploymentError> {
        let (container, _) = self
            .get_container_detail(project_id, environment_id, container_id.clone())
            .await?;
        let deployment_id = container.deployment_id;
        crate::services::plane_deployer::refuse_micro_vm_lifecycle(
            self.workload_plane.get().map(Arc::as_ref),
            &container.container_id,
            "restart",
        )
        .await?;

        let deployer = self.deployer_for_node(container.node_id).await?;
        deployer
            .stop_container(&container.container_id)
            .await
            .map_err(|e| DeploymentError::Other(format!("Failed to stop container: {}", e)))?;

        deployer
            .start_container(&container.container_id)
            .await
            .map_err(|e| DeploymentError::Other(format!("Failed to start container: {}", e)))?;

        // Update container status in database
        let mut active_container: deployment_containers::ActiveModel = container.into();
        active_container.status = Set(Some("running".to_string()));
        active_container.update(self.db.as_ref()).await?;

        // Same reasoning as `pause_deployment`/`resume_deployment`: this
        // status UPDATE fires no DB trigger, so force an in-process
        // route-table reload or the proxy's cached peer table doesn't
        // notice the restart until some unrelated route change reloads it.
        if let Err(e) = self
            .queue_service
            .send(temps_core::Job::ForceRouteReload(
                temps_core::ForceRouteReloadJob {
                    environment_id: Some(environment_id),
                    deployment_id: Some(deployment_id),
                },
            ))
            .await
        {
            warn!(
                "Failed to publish in-process ForceRouteReload after restarting container {}: {} \
                 — falling back to the next PG NOTIFY-triggered reload",
                container_id, e
            );
        }

        info!("Successfully restarted container: {}", container_id);
        Ok(())
    }

    /// Get container environment variables from Docker
    pub async fn get_container_env_variables(
        &self,
        project_id: i32,
        environment_id: i32,
        container_id: String,
    ) -> Result<Vec<(String, String)>, DeploymentError> {
        let (container, _) = self
            .get_container_detail(project_id, environment_id, container_id.clone())
            .await?;

        let deployer = self.deployer_for_node(container.node_id).await?;
        let container_info = deployer
            .get_container_info(&container.container_id)
            .await
            .map_err(|e| DeploymentError::Other(format!("Failed to get container info: {}", e)))?;

        // Convert HashMap to Vec of tuples
        let env_vars: Vec<(String, String)> = container_info.environment_vars.into_iter().collect();
        Ok(env_vars)
    }

    /// The variables a job cron run beside `container_id` gets: read from
    /// the container when it runs on this host, recomputed from the
    /// container's deployment when it runs on a worker, because a worker's
    /// `/agent/*` API never returns a container's variables (an empty
    /// import would start the run without its configuration). Carries
    /// secret values: never log the result.
    pub async fn job_import_env_variables(
        &self,
        project_id: i32,
        environment_id: i32,
        container_id: String,
    ) -> Result<Vec<(String, String)>, DeploymentError> {
        let (container, _) = self
            .get_container_detail(project_id, environment_id, container_id.clone())
            .await?;
        let Some(node_id) = container.node_id else {
            return self
                .get_container_env_variables(project_id, environment_id, container_id)
                .await;
        };
        let planner = self.workflow_planner.get().ok_or_else(|| {
            DeploymentError::Other(format!(
                "cannot recompute the variables of container {container_id} on node {node_id} \
                 (project {project_id}, environment {environment_id}): the workflow planner is \
                 not wired into the deployment service"
            ))
        })?;
        let deployment = deployments::Entity::find_by_id(container.deployment_id)
            .one(self.db.as_ref())
            .await?
            .filter(|d| d.project_id == project_id && d.environment_id == environment_id)
            .ok_or_else(|| {
                DeploymentError::NotFound(format!(
                    "deployment {} of container {container_id} not found in project \
                     {project_id}, environment {environment_id}",
                    container.deployment_id
                ))
            })?;
        let environment = environments::Entity::find_by_id(environment_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!(
                    "environment {environment_id} of container {container_id} not found"
                ))
            })?;
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!(
                    "project {project_id} of container {container_id} not found"
                ))
            })?;
        let vars = planner
            .runtime_environment_variables(&project, &environment, &deployment, true)
            .await
            .map_err(|e| {
                DeploymentError::Other(format!(
                    "recomputing the variables of container {container_id} on node {node_id} \
                     (deployment {}, project {project_id}, environment {environment_id}) \
                     failed: {e}",
                    deployment.id
                ))
            })?;
        Ok(vars.into_iter().collect())
    }

    /// Get the restart count for a container from Docker
    pub async fn get_container_restart_count(&self, container_id: &str) -> Option<i64> {
        self.deployer
            .get_container_info(container_id)
            .await
            .ok()
            .and_then(|info| info.restart_count)
    }

    /// Stop all containers in an environment
    pub async fn stop_all_containers(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<(), DeploymentError> {
        use temps_entities::environments;

        // Verify environment exists and belongs to project
        let _environment = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Environment not found".to_string()))?;

        // Get all active containers in this environment
        let containers = deployment_containers::Entity::find()
            .inner_join(deployments::Entity)
            .filter(deployments::Column::EnvironmentId.eq(environment_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        for container in containers {
            let _ = self.deployer.stop_container(&container.container_id).await;
            let mut active_container: deployment_containers::ActiveModel = container.into();
            active_container.status = Set(Some("stopped".to_string()));
            let _ = active_container.update(self.db.as_ref()).await;
        }

        info!("Stopped containers in environment: {}", environment_id);
        Ok(())
    }

    /// Start all containers in an environment
    pub async fn start_all_containers(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<(), DeploymentError> {
        use temps_entities::environments;

        // Verify environment exists and belongs to project
        let _environment = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Environment not found".to_string()))?;

        // Get all active containers in this environment
        let containers = deployment_containers::Entity::find()
            .inner_join(deployments::Entity)
            .filter(deployments::Column::EnvironmentId.eq(environment_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        for container in containers {
            let _ = self.deployer.start_container(&container.container_id).await;
            let mut active_container: deployment_containers::ActiveModel = container.into();
            active_container.status = Set(Some("running".to_string()));
            let _ = active_container.update(self.db.as_ref()).await;
        }

        info!("Started containers in environment: {}", environment_id);
        Ok(())
    }

    /// Restart all containers in an environment
    pub async fn restart_all_containers(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<(), DeploymentError> {
        use temps_entities::environments;

        // Verify environment exists and belongs to project
        let _environment = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Environment not found".to_string()))?;

        // Get all active containers in this environment
        let containers = deployment_containers::Entity::find()
            .inner_join(deployments::Entity)
            .filter(deployments::Column::EnvironmentId.eq(environment_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        for container in containers {
            let _ = self.deployer.stop_container(&container.container_id).await;
            let _ = self.deployer.start_container(&container.container_id).await;
            let mut active_container: deployment_containers::ActiveModel = container.into();
            active_container.status = Set(Some("running".to_string()));
            let _ = active_container.update(self.db.as_ref()).await;
        }

        info!("Restarted containers in environment: {}", environment_id);
        Ok(())
    }

    /// Get metrics/stats for a specific container
    pub async fn get_container_metrics(
        &self,
        project_id: i32,
        environment_id: i32,
        container_id: String,
    ) -> Result<temps_deployer::ContainerStats, DeploymentError> {
        let (container, _) = self
            .get_container_detail(project_id, environment_id, container_id.clone())
            .await?;

        // Route to the worker that owns this container so remote stats
        // come back via the agent's `/agent/containers/{id}/stats` endpoint
        // instead of hitting the CP's local dockerd.
        let deployer = self.deployer_for_node(container.node_id).await?;
        let stats = deployer
            .get_container_stats(&container.container_id)
            .await
            .map_err(|e| DeploymentError::Other(format!("Failed to get container stats: {}", e)))?;

        debug!("Retrieved metrics for container: {}", container_id);
        Ok(stats)
    }
}

#[async_trait::async_trait]
impl temps_monitoring::ContainerRuntimeResolver for DeploymentService {
    async fn resolve_runtime(
        &self,
        node_id: Option<i32>,
    ) -> Result<
        Arc<dyn temps_deployer::ContainerDeployer>,
        temps_monitoring::ContainerRuntimeResolutionError,
    > {
        self.deployer_for_node(node_id).await.map_err(|error| {
            temps_monitoring::ContainerRuntimeResolutionError {
                node_id,
                reason: error.to_string(),
            }
        })
    }
}
