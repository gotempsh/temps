use futures::Stream;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, Set,
};
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use temps_entities::{
    deployment_container_logs, deployment_containers, deployment_domains, deployments,
    environments, projects,
};
use thiserror::Error;
use tracing::{debug, error, info, warn};

/// Boxed log stream so local-Docker and remote-agent paths share one return type.
pub type ContainerLogStream =
    Pin<Box<dyn Stream<Item = Result<String, std::io::Error>> + Send + 'static>>;

/// Connection details the CP terminal handler needs to dial a worker's
/// agent WebSocket.
#[derive(Debug, Clone)]
pub struct RemoteTerminalTarget {
    pub ws_url: String,
    pub token: String,
}

use crate::services::types::{
    Deployment, DeploymentDomain, DeploymentEnvironment, DeploymentListResponse,
};
use crate::UpdateDeploymentSettingsRequest;
use temps_core::WorkflowTask;

/// Parameters for container log retrieval
pub struct ContainerLogParams {
    pub start_date: Option<i64>,
    pub end_date: Option<i64>,
    pub tail: Option<String>,
    pub timestamps: bool,
    pub follow: bool,
}

#[derive(Error, Debug)]
pub enum DeploymentError {
    #[error("Database connection error: {0}")]
    DatabaseConnectionError(String),

    #[error("Deployment not found")]
    NotFound(String),

    #[error("Database error: {reason}")]
    DatabaseError { reason: String },

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Invalid deployment state: {0}")]
    InvalidDeploymentState(String),

    #[error("Pipeline error: {0}")]
    PipelineError(String),

    #[error("Deployment error: {0}")]
    DeploymentError(String),

    #[error("Queue error: {0}")]
    QueueError(String),

    /// Bundle path (read from DB and joined to data_dir) resolved outside the
    /// data directory.  `path` is the offending resolved path; `reason`
    /// explains how the check failed.
    #[error("Invalid bundle path '{path}': {reason}")]
    InvalidBundlePath { path: String, reason: String },

    #[error("Other error: {0}")]
    Other(String),
}

impl From<sea_orm::DbErr> for DeploymentError {
    fn from(error: sea_orm::DbErr) -> Self {
        match error {
            sea_orm::DbErr::RecordNotFound(_) => DeploymentError::NotFound(error.to_string()),
            _ => DeploymentError::DatabaseError {
                reason: error.to_string(),
            },
        }
    }
}

#[derive(Clone)]
pub struct DeploymentService {
    db: Arc<temps_database::DbConnection>,
    log_service: Arc<temps_logs::LogService>,
    config_service: Arc<temps_config::ConfigService>,
    queue_service: Arc<dyn temps_core::JobQueue>,
    docker_log_service: Arc<temps_logs::DockerLogService>,
    deployer: Arc<dyn temps_deployer::ContainerDeployer>,
    encryption_service: Arc<temps_core::EncryptionService>,
}

impl DeploymentService {
    /// Resolve CPU/memory limits + requests for a deploy from the environment
    /// config first, then the project config, leaving each field unset when
    /// neither configures it (→ no Docker limit = uncapped). Mirrors the
    /// resolution in `WorkflowExecutionService` so every deploy path (initial,
    /// rollback, promote) treats resource limits as opt-in identically.
    ///
    /// CPU is stored as microcores in the DB (1_000_000 = 1 core), memory as MB;
    /// emitted with the `u`/`Mi` suffixes the deployer's parsers understand.
    fn resolve_resource_usage(
        env_cfg: Option<&temps_entities::deployment_config::DeploymentConfig>,
        proj_cfg: Option<&temps_entities::deployment_config::DeploymentConfig>,
    ) -> crate::jobs::ResourceUsage {
        let resolve = |getter: fn(
            &temps_entities::deployment_config::DeploymentConfig,
        ) -> Option<i32>|
         -> Option<i32> {
            env_cfg
                .and_then(getter)
                .or_else(|| proj_cfg.and_then(getter))
        };
        crate::jobs::ResourceUsage {
            cpu_limit: resolve(|c| c.cpu_limit).map(|u| format!("{}u", u)),
            memory_limit: resolve(|c| c.memory_limit).map(|mb| format!("{}Mi", mb)),
            cpu_request: resolve(|c| c.cpu_request).map(|u| format!("{}u", u)),
            memory_request: resolve(|c| c.memory_request).map(|mb| format!("{}Mi", mb)),
        }
    }

    pub fn new(
        db: Arc<temps_database::DbConnection>,
        log_service: Arc<temps_logs::LogService>,
        config_service: Arc<temps_config::ConfigService>,
        queue_service: Arc<dyn temps_core::JobQueue>,
        docker_log_service: Arc<temps_logs::DockerLogService>,
        deployer: Arc<dyn temps_deployer::ContainerDeployer>,
        encryption_service: Arc<temps_core::EncryptionService>,
    ) -> Self {
        DeploymentService {
            db,
            log_service,
            config_service,
            queue_service,
            docker_log_service,
            deployer,
            encryption_service,
        }
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
        match container.node_id {
            None => self.local_container_log_stream(&container_id, params).await,
            Some(node_id) => {
                self.remote_container_log_stream(node_id, &container_id, params)
                    .await
            }
        }
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

        let deployment_id = environment
            .current_deployment_id
            .ok_or_else(|| DeploymentError::NotFound("No active deployment found".to_string()))?;

        // Verify the container belongs to this deployment and pick up its node placement.
        let container = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_containers::Column::ContainerId.eq(&container_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!(
                    "Container {} not found in deployment",
                    container_id
                ))
            })?;

        match container.node_id {
            None => self.local_container_log_stream(&container_id, params).await,
            Some(node_id) => {
                self.remote_container_log_stream(node_id, &container_id, params)
                    .await
            }
        }
    }

    /// Return the right `ContainerDeployer` for a container based on where it
    /// runs: `None` → the local CP dockerd; `Some(node_id)` → a fresh
    /// `RemoteNodeDeployer` pointing at that worker's agent.
    ///
    /// We construct the remote deployer per call (cheap — it's just a
    /// reqwest Client + URL + token) so we don't have to keep a long-lived
    /// per-node cache that would have to invalidate on token rotation or
    /// node deletion.
    async fn deployer_for_node(
        &self,
        node_id: Option<i32>,
    ) -> Result<Arc<dyn temps_deployer::ContainerDeployer>, DeploymentError> {
        let Some(nid) = node_id else {
            return Ok(self.deployer.clone());
        };
        let remote = self.remote_deployer_for_node(nid).await?;
        Ok(Arc::new(remote))
    }

    /// Build a concrete `RemoteNodeDeployer` for a node — needed for
    /// methods that aren't on the `ContainerDeployer` trait (e.g. exec).
    async fn remote_deployer_for_node(
        &self,
        node_id: i32,
    ) -> Result<temps_deployer::remote::RemoteNodeDeployer, DeploymentError> {
        use temps_entities::nodes;
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

        temps_deployer::remote::RemoteNodeDeployer::new(
            node.address.clone(),
            token,
            node.name.clone(),
        )
        .map_err(|e| {
            DeploymentError::Other(format!(
                "Failed to build remote deployer for node {}: {}",
                node_id, e
            ))
        })
    }

    /// Resolve the WebSocket URL + bearer token for a worker agent's
    /// terminal endpoint. The handler dials this WS and pipes frames
    /// 1:1 between the browser and the agent.
    pub async fn resolve_remote_terminal(
        &self,
        node_id: i32,
        container_id: &str,
    ) -> Result<RemoteTerminalTarget, DeploymentError> {
        let remote = self.remote_deployer_for_node(node_id).await?;
        let base = remote.agent_url().trim_end_matches('/').to_string();
        // Map the agent's HTTP scheme to the WS scheme. The agent uses
        // `http://` on the underlay or `https://` if TLS-fronted, so the
        // ws scheme tracks it directly.
        let ws_base = if let Some(rest) = base.strip_prefix("https://") {
            format!("wss://{}", rest)
        } else if let Some(rest) = base.strip_prefix("http://") {
            format!("ws://{}", rest)
        } else {
            return Err(DeploymentError::Other(format!(
                "Node {} agent URL has an unsupported scheme: {}",
                node_id, base
            )));
        };
        Ok(RemoteTerminalTarget {
            ws_url: format!("{}/agent/containers/{}/terminal", ws_base, container_id),
            token: remote.token().to_string(),
        })
    }

    /// Run a one-shot exec on a remote worker. The container's `node_id`
    /// must be `Some(_)` — local-CP exec stays in the handler so we don't
    /// duplicate bollard plumbing here.
    pub async fn exec_command_remote(
        &self,
        node_id: i32,
        container_id: &str,
        command: Vec<String>,
        timeout_seconds: Option<u64>,
    ) -> Result<temps_deployer::remote::RemoteExecResult, DeploymentError> {
        let remote = self.remote_deployer_for_node(node_id).await?;
        remote
            .exec_command(container_id, command, timeout_seconds)
            .await
            .map_err(|e| {
                DeploymentError::Other(format!("Remote exec on node {} failed: {}", node_id, e))
            })
    }

    /// Stream logs from the locally-running dockerd via `DockerLogService`.
    async fn local_container_log_stream(
        &self,
        container_id: &str,
        params: ContainerLogParams,
    ) -> Result<ContainerLogStream, DeploymentError> {
        let stream_result = self
            .docker_log_service
            .get_container_logs(
                container_id,
                temps_logs::docker_logs::ContainerLogOptions {
                    start_date: params.start_date.map(|ts| {
                        chrono::DateTime::from_timestamp(ts, 0).unwrap_or_else(chrono::Utc::now)
                    }),
                    end_date: params.end_date.map(|ts| {
                        chrono::DateTime::from_timestamp(ts, 0).unwrap_or_else(chrono::Utc::now)
                    }),
                    tail: params.tail,
                    timestamps: params.timestamps,
                    follow: params.follow,
                },
            )
            .await
            .map_err(|e| DeploymentError::Other(e.to_string()))?;

        let mapped = futures_util::stream::StreamExt::map(stream_result, |item| {
            item.map_err(|container_err| std::io::Error::other(container_err.to_string()))
        });
        Ok(Box::pin(mapped))
    }

    /// Stream logs from a remote agent's chunked HTTP endpoint.
    ///
    /// The agent endpoint at `/agent/containers/{id}/logs/stream` emits the
    /// same byte stream the local `docker logs` would have produced, so each
    /// chunk maps 1:1 to a `String` log line for the WebSocket client. Auth
    /// uses the per-node token we issued at `temps join`, decrypted here from
    /// `nodes.token_encrypted`.
    async fn remote_container_log_stream(
        &self,
        node_id: i32,
        container_id: &str,
        params: ContainerLogParams,
    ) -> Result<ContainerLogStream, DeploymentError> {
        use futures_util::StreamExt as _;
        use temps_entities::nodes;

        let node = nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!(
                    "Node {} for container {} not found",
                    node_id, container_id
                ))
            })?;

        let encrypted_token = node.token_encrypted.as_ref().ok_or_else(|| {
            DeploymentError::Other(format!(
                "Node {} has no agent token; cannot stream remote logs",
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

        let mut url = format!(
            "{}/agent/containers/{}/logs/stream",
            node.address.trim_end_matches('/'),
            container_id,
        );
        let mut query: Vec<(&str, String)> = Vec::new();
        if let Some(s) = params.start_date {
            query.push(("start_date", s.to_string()));
        }
        if let Some(s) = params.end_date {
            query.push(("end_date", s.to_string()));
        }
        if let Some(t) = &params.tail {
            query.push(("tail", t.clone()));
        }
        query.push(("timestamps", params.timestamps.to_string()));
        query.push(("follow", params.follow.to_string()));
        if !query.is_empty() {
            let qs = query
                .into_iter()
                .map(|(k, v)| format!("{}={}", k, urlencoding::encode(&v)))
                .collect::<Vec<_>>()
                .join("&");
            url.push('?');
            url.push_str(&qs);
        }

        // Strict TLS by default; opt-in via the same `insecure_tls` toggle
        // that the rest of the CP→agent traffic uses, so dev clusters with
        // self-signed agent certs work without a global escape hatch.
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(temps_core::tls::insecure_tls_enabled())
            // No top-level timeout — log streams are long-lived by design.
            .build()
            .map_err(|e| {
                DeploymentError::Other(format!(
                    "Failed to build HTTP client for node {}: {}",
                    node_id, e
                ))
            })?;

        let resp = client
            .get(&url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| {
                DeploymentError::Other(format!(
                    "Failed to reach agent on node {} at {}: {}",
                    node.name, url, e
                ))
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(DeploymentError::Other(format!(
                "Agent on node {} returned {} for log stream: {}",
                node.name, status, body
            )));
        }

        // The agent interleaves NUL bytes as keepalives to keep the
        // chunked HTTP body alive across idle periods. Drop them here so
        // the WebSocket client only sees real log bytes. The control plane
        // emits its own WebSocket Ping frames upstream of this stream
        // (see `handle_container_logs_socket`) so the browser side stays
        // alive too.
        let bytes_stream = resp
            .bytes_stream()
            .map(|chunk| match chunk {
                Ok(b) => {
                    let filtered: Vec<u8> = b.iter().copied().filter(|&c| c != 0).collect();
                    Ok(filtered)
                }
                Err(e) => Err(std::io::Error::other(format!(
                    "Remote log stream error: {}",
                    e
                ))),
            })
            .filter_map(|res| async move {
                match res {
                    Ok(v) if v.is_empty() => None,
                    Ok(v) => Some(Ok(String::from_utf8_lossy(&v).to_string())),
                    Err(e) => Some(Err(e)),
                }
            });

        Ok(Box::pin(bytes_stream))
    }

    /// List all containers for a specific environment.
    /// Returns container info paired with the optional node_id each container runs on.
    /// Returns (ContainerInfo, node_id, service_name) for each container
    pub async fn list_environment_containers(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<Vec<(temps_deployer::ContainerInfo, Option<i32>, Option<String>)>, DeploymentError>
    {
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
            let service_name = db_container.service_name.clone();

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
                Ok(info) => container_infos.push((info, node_id, service_name)),
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

    /// Purge all cached static assets for a project or a specific environment.
    /// Deletes static_asset_cache DB rows. Orphaned CAS blobs are cleaned up
    /// by the nightly garbage collector.
    /// Returns the number of cache entries deleted.
    pub async fn purge_asset_cache(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<u64, DeploymentError> {
        use sea_orm::ConnectionTrait;

        let mut sql = format!(
            "DELETE FROM static_asset_cache WHERE project_id = {}",
            project_id
        );
        if let Some(env_id) = environment_id {
            sql.push_str(&format!(" AND environment_id = {}", env_id));
        }

        let result = self
            .db
            .as_ref()
            .execute(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                sql,
            ))
            .await?;

        let deleted = result.rows_affected();
        info!(
            "Purged {} asset cache entries for project {} (env: {:?})",
            deleted, project_id, environment_id
        );

        Ok(deleted)
    }

    pub async fn update_deployment_settings(
        &self,
        project_id: i32,
        environment_id: i32,
        settings: UpdateDeploymentSettingsRequest,
    ) -> Result<(), DeploymentError> {
        // Find the current deployment for the environment
        let environment = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Environment not found".to_string()))?;

        // Update the environment with new settings
        let mut active_environment: environments::ActiveModel = environment.clone().into();

        // Update deployment config with new resource settings
        let mut deployment_config = environment.deployment_config.clone().unwrap_or_default();
        deployment_config.cpu_request = settings.cpu_request;
        deployment_config.cpu_limit = settings.cpu_limit;
        deployment_config.memory_request = settings.memory_request;
        deployment_config.memory_limit = settings.memory_limit;

        active_environment.deployment_config = Set(Some(deployment_config));
        active_environment.update(self.db.as_ref()).await?;

        Ok(())
    }

    pub async fn get_project_deployments(
        &self,
        project_id: i32,
        page: Option<i64>,
        per_page: Option<i64>,
        environment_id: Option<i32>,
    ) -> Result<DeploymentListResponse, DeploymentError> {
        let page = page.unwrap_or(1) as u64;
        let per_page = per_page.unwrap_or(10) as u64;

        // Build base query with project_id filter
        let mut query =
            deployments::Entity::find().filter(deployments::Column::ProjectId.eq(project_id));

        let mut total_query =
            deployments::Entity::find().filter(deployments::Column::ProjectId.eq(project_id));

        // Add environment_id filter if provided
        if let Some(env_id) = environment_id {
            query = query.filter(deployments::Column::EnvironmentId.eq(env_id));
            total_query = total_query.filter(deployments::Column::EnvironmentId.eq(env_id));
        }

        let total = total_query
            .count(self.db.as_ref())
            .await
            .map_err(|e| DeploymentError::Other(e.to_string()))?;

        let results = query
            .order_by_desc(deployments::Column::CreatedAt)
            .paginate(self.db.as_ref(), per_page)
            .fetch_page(page - 1)
            .await
            .map_err(|e| DeploymentError::Other(e.to_string()))?;

        if results.is_empty() && page == 1 {
            return Ok(DeploymentListResponse {
                deployments: Vec::new(),
                total: 0,
                page: page as i64,
                per_page: per_page as i64,
            });
        }

        // Collect all unique environment IDs
        let env_ids: Vec<i32> = results
            .iter()
            .map(|d| d.environment_id)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        // Fetch all environments with their domains in a single query
        let environments_with_domains = self.get_environments_with_domains(&env_ids).await?;

        // For each deployment, check if it's the current deployment for any environment
        let mut deployments_with_info = Vec::new();
        for deployment in results {
            let is_current = environments::Entity::find()
                .filter(environments::Column::ProjectId.eq(project_id))
                .filter(environments::Column::CurrentDeploymentId.eq(deployment.id))
                .one(self.db.as_ref())
                .await
                .map_err(|e| DeploymentError::Other(e.to_string()))?
                .is_some();

            let environment = environments_with_domains
                .get(&deployment.environment_id)
                .cloned();

            deployments_with_info.push(
                self.map_db_deployment_to_deployment(deployment, is_current, environment)
                    .await,
            );
        }

        Ok(DeploymentListResponse {
            deployments: deployments_with_info,
            total: total as i64,
            page: page as i64,
            per_page: per_page as i64,
        })
    }

    pub async fn get_last_deployment(
        &self,
        project_id: i32,
    ) -> Result<Deployment, DeploymentError> {
        let deployment_with_pipeline = deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(project_id))
            .order_by_desc(deployments::Column::CreatedAt)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!("project {} not found", project_id))
            })?;

        let deployment = deployment_with_pipeline;

        // Check if this deployment is current for any environment
        let is_current = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::CurrentDeploymentId.eq(deployment.id))
            .one(self.db.as_ref())
            .await?
            .is_some();

        // Fetch environment with domains
        let environments_with_domains = self
            .get_environments_with_domains(&[deployment.environment_id])
            .await?;
        let environment = environments_with_domains
            .get(&deployment.environment_id)
            .cloned();

        Ok(self
            .map_db_deployment_to_deployment(deployment, is_current, environment)
            .await)
    }

    pub async fn get_deployment(
        &self,
        project_id: i32,
        deployment_id: i32,
    ) -> Result<Deployment, DeploymentError> {
        // Get the deployment with its pipeline
        let deployment_with_pipeline = deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(project_id))
            .filter(deployments::Column::Id.eq(deployment_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!(
                    "deployment {} for project {} not found",
                    deployment_id, project_id
                ))
            })?;

        let deployment = deployment_with_pipeline;

        // Check if this deployment is current for any environment
        let is_current = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::CurrentDeploymentId.eq(deployment_id))
            .one(self.db.as_ref())
            .await?
            .is_some();

        // Fetch environment with domains
        let environments_with_domains = self
            .get_environments_with_domains(&[deployment.environment_id])
            .await?;
        let environment = environments_with_domains
            .get(&deployment.environment_id)
            .cloned();

        Ok(self
            .map_db_deployment_to_deployment(deployment, is_current, environment)
            .await)
    }

    /// List the captured (historical) container-log dumps for a deployment.
    ///
    /// These are written just before a superseded deployment's containers are
    /// torn down (see `MarkDeploymentCompleteJob::capture_container_logs`), so
    /// they let a user read the logs of a container that no longer exists
    /// (e.g. "web-2" from a few days ago).
    ///
    /// Scoped to `project_id`: the deployment must belong to the caller's
    /// project or this returns `NotFound`, preventing cross-tenant access.
    pub async fn list_deployment_container_logs(
        &self,
        project_id: i32,
        deployment_id: i32,
    ) -> Result<Vec<deployment_container_logs::Model>, DeploymentError> {
        // Authorize: confirm the deployment is in this project before exposing
        // anything tied to it.
        deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(project_id))
            .filter(deployments::Column::Id.eq(deployment_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!(
                    "deployment {} for project {} not found",
                    deployment_id, project_id
                ))
            })?;

        let logs = deployment_container_logs::Entity::find()
            .filter(deployment_container_logs::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_container_logs::Column::ProjectId.eq(project_id))
            .order_by_desc(deployment_container_logs::Column::CapturedAt)
            .all(self.db.as_ref())
            .await?;

        Ok(logs)
    }

    /// Read the captured text content for a single historical container-log
    /// dump, returning the metadata row alongside the log body.
    ///
    /// Scoped to `project_id` via the `log_id` row's own `project_id` column —
    /// a caller can only read dumps that belong to their project.
    pub async fn get_deployment_container_log_content(
        &self,
        project_id: i32,
        deployment_id: i32,
        log_id: i32,
    ) -> Result<(deployment_container_logs::Model, String), DeploymentError> {
        let row = deployment_container_logs::Entity::find()
            .filter(deployment_container_logs::Column::Id.eq(log_id))
            .filter(deployment_container_logs::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_container_logs::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!(
                    "captured log {} for deployment {} in project {} not found",
                    log_id, deployment_id, project_id
                ))
            })?;

        // `log_path` is a server-generated relative path (never user input), so
        // `get_log_content` resolves it safely under the data dir.
        let content = self
            .log_service
            .get_log_content(&row.log_path)
            .await
            .map_err(|e| {
                DeploymentError::Other(format!(
                    "Failed to read captured log file for log {} (deployment {}): {}",
                    log_id, deployment_id, e
                ))
            })?;

        Ok((row, content))
    }

    pub async fn get_deployment_domains(
        &self,
        deployment_id: i32,
    ) -> Result<Vec<DeploymentDomain>, DeploymentError> {
        let mut domains: Vec<DeploymentDomain> = Vec::new();

        // check if deployment_id is current in environments table
        let is_current = environments::Entity::find()
            .filter(environments::Column::CurrentDeploymentId.eq(Some(deployment_id)))
            .one(self.db.as_ref())
            .await?;

        if let Some(env) = is_current {
            domains.push(DeploymentDomain {
                id: 999999999,
                domain: env.subdomain,
            });
        }

        let db_domains = deployment_domains::Entity::find()
            .filter(deployment_domains::Column::DeploymentId.eq(deployment_id))
            .all(self.db.as_ref())
            .await?;

        let db_domains_mapped: Vec<DeploymentDomain> = db_domains
            .into_iter()
            .map(|d| DeploymentDomain {
                id: d.id,
                domain: d.domain,
            })
            .collect();
        domains.extend(db_domains_mapped);
        Ok(domains)
    }

    pub async fn trigger_pipeline(
        &self,
        project_id: i32,
        environment_id: i32,
        branch: Option<String>,
        tag: Option<String>,
        commit: Option<String>,
    ) -> Result<(), DeploymentError> {
        info!("Triggering pipeline for project_id: {}", project_id);
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| DeploymentError::Other(e.to_string()))?;

        let project = project.ok_or_else(|| {
            DeploymentError::NotFound(format!("project {} not found", project_id))
        })?;
        debug!("Project found: {:?}", project);

        debug!(
            "Before invoking pipeline service project_id: {}, environment_id: {}",
            project_id, environment_id
        );
        // Check if repo_owner and repo_name are present
        let repo_owner = project.repo_owner.clone();
        let repo_name = project.repo_name.clone();

        // Validate that they're not empty
        if repo_owner.is_empty() {
            return Err(DeploymentError::InvalidInput(
                "Project repo_owner is missing".to_string(),
            ));
        }
        if repo_name.is_empty() {
            return Err(DeploymentError::InvalidInput(
                "Project repo_name is missing".to_string(),
            ));
        }
        let git_push_job = temps_core::GitPushEventJob {
            owner: repo_owner,
            repo: repo_name,
            branch: branch.clone(),
            tag: tag.clone(),
            commit: commit.clone().unwrap_or_default(),
            project_id,
            // User-initiated trigger — bypasses environments.automatic_deploy.
            manual_trigger: true,
        };

        tracing::debug!(
            "🔥 Sending GitPushEvent to queue - owner: {}, repo: {}, branch: {:?}, tag: {:?}, commit: {}",
            git_push_job.owner, git_push_job.repo, git_push_job.branch, git_push_job.tag, git_push_job.commit
        );

        self.queue_service
            .send(temps_core::Job::GitPushEvent(git_push_job))
            .await
            .map_err(|e| {
                tracing::error!("Failed to send GitPushEvent to queue: {}", e);
                DeploymentError::QueueError(e.to_string())
            })?;

        tracing::debug!("GitPushEvent successfully sent to queue");
        Ok(())
    }

    /// Redeploy an environment using the context (branch, tag, commit) from its
    /// latest successful deployment.  Used by node drain and failover — these
    /// operations need to reschedule existing workloads, not start a fresh
    /// deployment from scratch.
    ///
    /// Falls back to the environment's configured branch when no prior
    /// deployment exists.
    pub async fn redeploy_environment(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<(), DeploymentError> {
        // Find the latest successful deployment for this environment
        let latest = deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(project_id))
            .filter(deployments::Column::EnvironmentId.eq(environment_id))
            .filter(deployments::Column::State.is_in(vec!["deployed", "completed", "ready"]))
            .order_by_desc(deployments::Column::CreatedAt)
            .one(self.db.as_ref())
            .await
            .map_err(|e| DeploymentError::Other(e.to_string()))?;

        let (branch, tag, commit) = if let Some(ref deploy) = latest {
            (
                deploy.branch_ref.clone(),
                deploy.tag_ref.clone(),
                deploy.commit_sha.clone(),
            )
        } else {
            // No prior deployment — fall back to environment's branch
            let env = temps_entities::environments::Entity::find_by_id(environment_id)
                .one(self.db.as_ref())
                .await
                .map_err(|e| DeploymentError::Other(e.to_string()))?;
            let branch = env.and_then(|e| e.branch.filter(|b| !b.is_empty()));
            (branch, None, None)
        };

        self.trigger_pipeline(project_id, environment_id, branch, tag, commit)
            .await
    }

    pub async fn rollback_to_deployment(
        &self,
        project_id: i32,
        deployment_id: i32,
    ) -> Result<Deployment, DeploymentError> {
        use temps_entities::deployments::DeploymentMetadata;

        // Fetch the target deployment (the one we're rolling back TO)
        let target_deployment = deployments::Entity::find_by_id(deployment_id)
            .filter(deployments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Target deployment not found".to_string()))?;

        // Validate that the deployment is in a valid state for rollback
        let valid_rollback_states = ["deployed", "completed"];
        if !valid_rollback_states.contains(&target_deployment.state.as_str()) {
            return Err(DeploymentError::InvalidDeploymentState(format!(
                "Cannot rollback to deployment in '{}' state. Only deployed or completed deployments can be rolled back to.",
                target_deployment.state
            )));
        }

        // Ensure target deployment has an image to roll back to
        let image_name = target_deployment.image_name.clone().ok_or_else(|| {
            DeploymentError::Other(
                "Target deployment has no image_name - cannot rollback".to_string(),
            )
        })?;

        let environment_id = target_deployment.environment_id;

        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Project not found".to_string()))?;

        let environment = environments::Entity::find_by_id(environment_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Environment not found".to_string()))?;

        info!(
            "Initiating rollback for project_id: {}, to deployment_id: {}, image: {}, environment_id: {}",
            project_id, deployment_id, image_name, environment_id
        );

        let preset = temps_presets::get_preset_by_slug(project.preset.as_str())
            .ok_or_else(|| DeploymentError::NotFound("Preset not found".to_string()))?;

        // --- Create a NEW deployment record for the rollback ---
        // This gives us fresh timestamps, a unique slug, and proper tracking.
        let now = chrono::Utc::now();

        // Get next deployment number
        let deployment_count = deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(project_id))
            .count(self.db.as_ref())
            .await
            .map_err(|e| DeploymentError::Other(format!("Failed to count deployments: {}", e)))?;
        let deployment_number = deployment_count + 1;

        let rollback_slug = format!("{}-{}", project.slug, deployment_number);

        let rollback_metadata = DeploymentMetadata {
            is_rollback: true,
            rolled_back_from_id: Some(deployment_id),
            ..Default::default()
        };

        let new_deployment = deployments::ActiveModel {
            id: sea_orm::NotSet,
            project_id: Set(project_id),
            environment_id: Set(environment_id),
            slug: Set(rollback_slug.clone()),
            state: Set("running".to_string()),
            metadata: Set(Some(rollback_metadata)),
            branch_ref: Set(target_deployment.branch_ref.clone()),
            tag_ref: Set(target_deployment.tag_ref.clone()),
            commit_sha: Set(target_deployment.commit_sha.clone()),
            commit_message: Set(target_deployment.commit_message.clone()),
            commit_author: Set(target_deployment.commit_author.clone()),
            commit_json: Set(target_deployment.commit_json.clone()),
            image_name: Set(Some(image_name.clone())),
            started_at: Set(Some(now)),
            finished_at: Set(None),
            deploying_at: Set(Some(now)),
            ready_at: Set(None),
            static_dir_location: Set(target_deployment.static_dir_location.clone()),
            screenshot_location: Set(None),
            cancelled_reason: Set(None),
            context_vars: Set(Some(serde_json::json!({
                "trigger": "rollback",
                "source_deployment_id": deployment_id,
            }))),
            deployment_config: Set(target_deployment.deployment_config.clone()),
            promoted_from_deployment_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        };

        let rollback_deployment = new_deployment.insert(self.db.as_ref()).await.map_err(|e| {
            DeploymentError::Other(format!("Failed to create rollback deployment: {}", e))
        })?;

        let rollback_deployment_id = rollback_deployment.id;
        info!(
            "Created rollback deployment #{} (rolling back to #{}, image: {})",
            rollback_deployment_id, deployment_id, image_name
        );

        // Check if preset is static - if so, just update environment without deploying
        if preset.project_type() == temps_presets::ProjectType::Static {
            info!("Rollback: Static preset detected - updating environment only");

            let mut active_env: environments::ActiveModel = environment.into();
            active_env.current_deployment_id = Set(Some(rollback_deployment_id));
            active_env.update(self.db.as_ref()).await?;

            // Mark the rollback deployment as completed
            let mut active_dep: deployments::ActiveModel = rollback_deployment.clone().into();
            active_dep.state = Set("completed".to_string());
            active_dep.finished_at = Set(Some(chrono::Utc::now()));
            active_dep.update(self.db.as_ref()).await?;

            info!(
                "Rollback completed - environment {} now points to rollback deployment {}",
                environment_id, rollback_deployment_id
            );
        } else {
            // Pre-flight check: verify the Docker image still exists locally
            match self.deployer.image_exists(&image_name).await {
                Ok(true) => {
                    info!(
                        "Rollback: Image '{}' exists locally, proceeding",
                        image_name
                    );
                }
                Ok(false) => {
                    // Mark the rollback deployment as failed
                    let mut active_dep: deployments::ActiveModel =
                        rollback_deployment.clone().into();
                    active_dep.state = Set("failed".to_string());
                    active_dep.finished_at = Set(Some(chrono::Utc::now()));
                    active_dep.cancelled_reason = Set(Some(format!(
                        "Docker image '{}' no longer exists locally",
                        image_name
                    )));
                    let _ = active_dep.update(self.db.as_ref()).await;

                    return Err(DeploymentError::Other(format!(
                        "Cannot rollback: Docker image '{}' no longer exists locally. \
                         The image may have been removed by Docker pruning. \
                         Consider redeploying from source instead.",
                        image_name
                    )));
                }
                Err(e) => {
                    return Err(DeploymentError::Other(format!(
                        "Cannot rollback: failed to verify Docker image '{}' exists: {}",
                        image_name, e
                    )));
                }
            }

            // --- Create per-job log paths (matching normal deployment pattern) ---
            let deploy_log_id = format!(
                "{}/{}/{}/{:02}/{:02}/{:02}/{:02}/deployment-{}-job-deploy_container.log",
                project.slug,
                environment.slug,
                now.format("%Y"),
                now.format("%m"),
                now.format("%d"),
                now.format("%H"),
                now.format("%M"),
                rollback_deployment_id
            );
            let complete_log_id = format!(
                "{}/{}/{}/{:02}/{:02}/{:02}/{:02}/deployment-{}-job-mark_deployment_complete.log",
                project.slug,
                environment.slug,
                now.format("%Y"),
                now.format("%m"),
                now.format("%d"),
                now.format("%H"),
                now.format("%M"),
                rollback_deployment_id
            );

            self.log_service
                .create_log_path(&deploy_log_id)
                .await
                .map_err(|e| {
                    DeploymentError::Other(format!("Failed to create deploy log path: {}", e))
                })?;
            self.log_service
                .create_log_path(&complete_log_id)
                .await
                .map_err(|e| {
                    DeploymentError::Other(format!("Failed to create complete log path: {}", e))
                })?;

            // --- Create deployment_jobs records so the API can return them ---
            use temps_entities::{deployment_jobs, types::JobStatus};

            let deploy_job_record = deployment_jobs::ActiveModel {
                deployment_id: Set(rollback_deployment_id),
                job_id: Set("deploy_container".to_string()),
                job_type: Set("DeployImageJob".to_string()),
                name: Set("Deploy Container".to_string()),
                description: Set(Some(format!("Rollback: deploy image {}", image_name))),
                status: Set(JobStatus::Running),
                log_id: Set(deploy_log_id.clone()),
                job_config: Set(None),
                dependencies: Set(None),
                execution_order: Set(Some(0)),
                started_at: Set(Some(now)),
                ..Default::default()
            };
            let deploy_job_model =
                deploy_job_record
                    .insert(self.db.as_ref())
                    .await
                    .map_err(|e| {
                        DeploymentError::Other(format!("Failed to create deploy job record: {}", e))
                    })?;

            let complete_job_record = deployment_jobs::ActiveModel {
                deployment_id: Set(rollback_deployment_id),
                job_id: Set("mark_deployment_complete".to_string()),
                job_type: Set("MarkDeploymentCompleteJob".to_string()),
                name: Set("Mark Deployment Complete".to_string()),
                description: Set(Some("Finalize rollback deployment".to_string())),
                status: Set(JobStatus::Pending),
                log_id: Set(complete_log_id.clone()),
                job_config: Set(None),
                dependencies: Set(Some(
                    serde_json::to_value(vec!["deploy_container"]).unwrap_or_default(),
                )),
                execution_order: Set(Some(1)),
                ..Default::default()
            };
            let complete_job_model =
                complete_job_record
                    .insert(self.db.as_ref())
                    .await
                    .map_err(|e| {
                        DeploymentError::Other(format!(
                            "Failed to create complete job record: {}",
                            e
                        ))
                    })?;

            // --- Step 0: Stop current environment containers BEFORE deploying ---
            // This prevents port conflicts where the old container still holds a port.
            info!(
                "Rollback: Stopping current containers for environment {}",
                environment_id
            );
            self.stop_environment_containers(environment_id, rollback_deployment_id)
                .await;

            info!("Rollback: Deploying image: {}", image_name);

            // Step 1: Execute DeployImageJob with external image
            // Use the NEW rollback slug as the container name (not the old deployment's slug)
            let mut deploy_builder = crate::jobs::DeployImageJobBuilder::new()
                .job_id("deploy_container".to_string())
                .build_job_id("external-image".to_string())
                .target(crate::jobs::DeploymentTarget::Docker {
                    registry_url: "local".to_string(),
                    network: Some(temps_core::NETWORK_NAME.to_string()),
                })
                .service_name(rollback_slug.clone())
                .health_check_path(None)
                .replicas(
                    environment
                        .deployment_config
                        .as_ref()
                        .map(|c| c.replicas as u32)
                        .or_else(|| {
                            project
                                .deployment_config
                                .as_ref()
                                .map(|c| c.replicas as u32)
                        })
                        .unwrap_or(1),
                )
                .port(
                    environment
                        .deployment_config
                        .as_ref()
                        .and_then(|c| c.exposed_port)
                        .or_else(|| {
                            project
                                .deployment_config
                                .as_ref()
                                .and_then(|c| c.exposed_port)
                        })
                        .unwrap_or(3000) as u32,
                )
                .log_id(deploy_log_id.clone())
                .log_service(self.log_service.clone());

            // Apply container log rotation settings from config
            if let Ok(settings) = self.config_service.get_settings().await {
                deploy_builder =
                    deploy_builder.container_log_config(temps_deployer::ContainerLogConfig::new(
                        settings.container_logs.max_size.clone(),
                        settings.container_logs.max_file,
                    ));
            }

            // Resolve CPU/memory limits + requests (env → project), matching the
            // normal deploy path (WorkflowExecutionService). Each field resolves
            // independently; when neither side configures a value it stays unset
            // so the deployer applies no Docker limit. Without this, a rollback
            // would inherit `ResourceUsage::default()` (now all-None) and silently
            // drop a configured limit — or, before the default was fixed, cap an
            // unconfigured environment.
            deploy_builder = deploy_builder.resources(Self::resolve_resource_usage(
                environment.deployment_config.as_ref(),
                project.deployment_config.as_ref(),
            ));

            let deploy_job = deploy_builder
                .build(self.deployer.clone())
                .map_err(|e| DeploymentError::Other(format!("Failed to create deploy job: {}", e)))?
                .with_external_image_tag(image_name.clone());

            // Create workflow context for the NEW rollback deployment
            let mock_log_writer = Arc::new(crate::test_utils::MockLogWriter::new(0));
            let mut rollback_context = temps_core::WorkflowContext::new(
                format!("rollback-{}", rollback_deployment_id),
                rollback_deployment_id,
                project_id,
                environment_id,
                mock_log_writer,
            );

            match deploy_job.execute(rollback_context.clone()).await {
                Ok(job_result) => {
                    info!("Rollback: Deploy job completed successfully");
                    rollback_context = job_result.context;

                    // Update deploy job record to Success
                    let mut active_job: deployment_jobs::ActiveModel = deploy_job_model.into();
                    active_job.status = Set(JobStatus::Success);
                    active_job.finished_at = Set(Some(chrono::Utc::now()));
                    let _ = active_job.update(self.db.as_ref()).await;
                }
                Err(e) => {
                    error!("Rollback: Deploy job failed: {}", e);

                    // Update deploy job record to Failure
                    let mut active_job: deployment_jobs::ActiveModel = deploy_job_model.into();
                    active_job.status = Set(JobStatus::Failure);
                    active_job.finished_at = Set(Some(chrono::Utc::now()));
                    active_job.error_message = Set(Some(format!("Deploy failed: {}", e)));
                    let _ = active_job.update(self.db.as_ref()).await;

                    // Cancel the pending complete job
                    let mut active_complete: deployment_jobs::ActiveModel =
                        complete_job_model.into();
                    active_complete.status = Set(JobStatus::Cancelled);
                    active_complete.error_message = Set(Some("Deploy job failed".to_string()));
                    let _ = active_complete.update(self.db.as_ref()).await;

                    // Mark the rollback deployment as failed
                    let mut active_dep: deployments::ActiveModel =
                        rollback_deployment.clone().into();
                    active_dep.state = Set("failed".to_string());
                    active_dep.finished_at = Set(Some(chrono::Utc::now()));
                    active_dep.cancelled_reason = Set(Some(format!("Deploy failed: {}", e)));
                    let _ = active_dep.update(self.db.as_ref()).await;

                    return Err(DeploymentError::Other(format!(
                        "Failed to deploy image during rollback: {}",
                        e
                    )));
                }
            }

            // Step 2: Execute MarkDeploymentCompleteJob on the NEW rollback deployment
            info!(
                "Rollback: Marking deployment {} as complete",
                rollback_deployment_id
            );

            // Update complete job to Running
            let mut active_complete: deployment_jobs::ActiveModel = complete_job_model.into();
            active_complete.status = Set(JobStatus::Running);
            active_complete.started_at = Set(Some(chrono::Utc::now()));
            let complete_job_model =
                active_complete
                    .update(self.db.as_ref())
                    .await
                    .map_err(|e| {
                        DeploymentError::Other(format!(
                            "Failed to update complete job status: {}",
                            e
                        ))
                    })?;

            let mark_complete_job = crate::jobs::MarkDeploymentCompleteJobBuilder::new()
                .job_id("mark_deployment_complete".to_string())
                .deployment_id(rollback_deployment_id)
                .db(self.db.clone())
                .log_id(complete_log_id)
                .log_service(self.log_service.clone())
                .container_deployer(self.deployer.clone())
                .queue(self.queue_service.clone())
                .config_service(self.config_service.clone())
                .encryption_service(self.encryption_service.clone())
                .build()
                .map_err(|e| {
                    DeploymentError::Other(format!("Failed to create mark complete job: {}", e))
                })?;

            match mark_complete_job.execute(rollback_context).await {
                Ok(_) => {
                    info!("Rollback: Mark complete job executed successfully");

                    // Update complete job record to Success
                    let mut active_job: deployment_jobs::ActiveModel = complete_job_model.into();
                    active_job.status = Set(JobStatus::Success);
                    active_job.finished_at = Set(Some(chrono::Utc::now()));
                    let _ = active_job.update(self.db.as_ref()).await;
                }
                Err(e) => {
                    error!("Rollback: Mark complete job failed: {}", e);

                    // Update complete job record to Failure
                    let mut active_job: deployment_jobs::ActiveModel = complete_job_model.into();
                    active_job.status = Set(JobStatus::Failure);
                    active_job.finished_at = Set(Some(chrono::Utc::now()));
                    active_job.error_message = Set(Some(format!("Mark complete failed: {}", e)));
                    let _ = active_job.update(self.db.as_ref()).await;

                    return Err(DeploymentError::Other(format!(
                        "Failed to mark deployment complete during rollback: {}",
                        e
                    )));
                }
            }

            info!(
                "Rollback completed - deployment {} is now active",
                rollback_deployment_id
            );
        }

        // Re-fetch the rollback deployment to get the final state
        let final_deployment = deployments::Entity::find_by_id(rollback_deployment_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::Other("Rollback deployment disappeared".to_string()))?;

        Ok(self
            .map_db_deployment_to_deployment(final_deployment, true, None)
            .await)
    }

    /// Stop all running containers for an environment (used before rollback deploys)
    async fn stop_environment_containers(&self, environment_id: i32, exclude_deployment_id: i32) {
        // Find all active deployments for this environment
        let active_deployments = match deployments::Entity::find()
            .filter(deployments::Column::EnvironmentId.eq(environment_id))
            .filter(deployments::Column::Id.ne(exclude_deployment_id))
            .filter(deployments::Column::State.is_in(vec!["running", "completed", "deployed"]))
            .all(self.db.as_ref())
            .await
        {
            Ok(deps) => deps,
            Err(e) => {
                warn!(
                    "Failed to fetch active deployments for pre-rollback cleanup: {}",
                    e
                );
                return;
            }
        };

        for dep in &active_deployments {
            let containers = match deployment_containers::Entity::find()
                .filter(deployment_containers::Column::DeploymentId.eq(dep.id))
                .filter(deployment_containers::Column::DeletedAt.is_null())
                .all(self.db.as_ref())
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    warn!(
                        "Failed to fetch containers for deployment {}: {}",
                        dep.id, e
                    );
                    continue;
                }
            };

            for container in containers {
                let container_id = container.container_id.clone();
                if let Err(e) = self.deployer.stop_container(&container_id).await {
                    warn!(
                        "Failed to stop container {} during pre-rollback cleanup: {}",
                        container_id, e
                    );
                }
                if let Err(e) = self.deployer.remove_container(&container_id).await {
                    warn!(
                        "Failed to remove container {} during pre-rollback cleanup: {}",
                        container_id, e
                    );
                }

                // Mark container as deleted
                let mut active_container: deployment_containers::ActiveModel = container.into();
                active_container.deleted_at = Set(Some(chrono::Utc::now()));
                active_container.status = Set(Some("removed".to_string()));
                let _ = active_container.update(self.db.as_ref()).await;

                info!(
                    "Pre-rollback: stopped and removed container {}",
                    container_id
                );
            }
        }
    }

    /// Promote a deployment to a different environment.
    /// Creates a new deployment in the target environment using the source
    /// deployment's image. The target environment must belong to the same project.
    pub async fn promote_deployment(
        &self,
        project_id: i32,
        source_deployment_id: i32,
        target_environment_id: i32,
    ) -> Result<Deployment, DeploymentError> {
        use temps_entities::deployments::DeploymentMetadata;

        // Fetch the source deployment
        let source = deployments::Entity::find_by_id(source_deployment_id)
            .filter(deployments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!(
                    "Source deployment {} not found in project {}",
                    source_deployment_id, project_id
                ))
            })?;

        // Validate state — only successful deployments can be promoted
        let valid_states = ["deployed", "completed", "ready"];
        if !valid_states.contains(&source.state.as_str()) {
            return Err(DeploymentError::InvalidDeploymentState(format!(
                "Cannot promote deployment in '{}' state. Only deployed/completed/ready deployments can be promoted.",
                source.state
            )));
        }

        // Must have an image to promote
        let image_name = source.image_name.clone().ok_or_else(|| {
            DeploymentError::Other(format!(
                "Source deployment {} has no image — cannot promote",
                source_deployment_id
            ))
        })?;

        // Fetch target environment and verify it belongs to the same project
        let target_env = environments::Entity::find_by_id(target_environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!(
                    "Target environment {} not found in project {}",
                    target_environment_id, project_id
                ))
            })?;

        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Project not found".to_string()))?;

        info!(
            "Promoting deployment {} to environment '{}' (project {}, image: {})",
            source_deployment_id, target_env.name, project_id, image_name
        );

        let preset = temps_presets::get_preset_by_slug(project.preset.as_str())
            .ok_or_else(|| DeploymentError::NotFound("Preset not found".to_string()))?;

        let now = chrono::Utc::now();

        // Get next deployment number
        let deployment_count = deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(project_id))
            .count(self.db.as_ref())
            .await
            .map_err(|e| DeploymentError::Other(format!("Failed to count deployments: {}", e)))?;
        let deployment_number = deployment_count + 1;

        let promote_slug = format!("{}-{}", project.slug, deployment_number);

        let promote_metadata = DeploymentMetadata {
            // Reuse build info from source
            builder: source.metadata.as_ref().and_then(|m| m.builder.clone()),
            image_size_bytes: source.metadata.as_ref().and_then(|m| m.image_size_bytes),
            ..Default::default()
        };

        // Merge deployment config for the target environment
        let merged_config = if let Some(project_config) = &project.deployment_config {
            if let Some(env_config) = &target_env.deployment_config {
                Some(project_config.merge(env_config))
            } else {
                Some(project_config.clone())
            }
        } else {
            target_env.deployment_config.clone()
        };

        let deployment_config_snapshot = merged_config.map(|config| {
            temps_entities::deployment_config::DeploymentConfigSnapshot::from_config(
                &config,
                std::collections::HashMap::new(),
            )
        });

        let new_deployment = deployments::ActiveModel {
            id: sea_orm::NotSet,
            project_id: Set(project_id),
            environment_id: Set(target_environment_id),
            slug: Set(promote_slug.clone()),
            state: Set("running".to_string()),
            metadata: Set(Some(promote_metadata)),
            branch_ref: Set(source.branch_ref.clone()),
            tag_ref: Set(source.tag_ref.clone()),
            commit_sha: Set(source.commit_sha.clone()),
            commit_message: Set(source.commit_message.clone()),
            commit_author: Set(source.commit_author.clone()),
            commit_json: Set(source.commit_json.clone()),
            image_name: Set(Some(image_name.clone())),
            started_at: Set(Some(now)),
            finished_at: Set(None),
            deploying_at: Set(Some(now)),
            ready_at: Set(None),
            static_dir_location: Set(source.static_dir_location.clone()),
            screenshot_location: Set(None),
            cancelled_reason: Set(None),
            context_vars: Set(Some(serde_json::json!({
                "trigger": "promotion",
                "source_deployment_id": source_deployment_id,
                "source_environment_id": source.environment_id,
            }))),
            deployment_config: Set(deployment_config_snapshot),
            promoted_from_deployment_id: Set(Some(source_deployment_id)),
            created_at: Set(now),
            updated_at: Set(now),
        };

        let promoted_deployment = new_deployment.insert(self.db.as_ref()).await.map_err(|e| {
            DeploymentError::Other(format!("Failed to create promoted deployment: {}", e))
        })?;

        let promoted_id = promoted_deployment.id;
        info!(
            "Created promoted deployment #{} (from #{} to environment '{}')",
            promoted_id, source_deployment_id, target_env.name
        );

        // Same logic as rollback — for static presets, just update env pointer
        if preset.project_type() == temps_presets::ProjectType::Static {
            info!("Promotion: Static preset detected — updating environment only");

            let mut active_env: environments::ActiveModel = target_env.into();
            active_env.current_deployment_id = Set(Some(promoted_id));
            active_env.update(self.db.as_ref()).await?;

            let mut active_dep: deployments::ActiveModel = promoted_deployment.clone().into();
            active_dep.state = Set("completed".to_string());
            active_dep.finished_at = Set(Some(chrono::Utc::now()));
            active_dep.update(self.db.as_ref()).await?;
        } else {
            // Verify the Docker image still exists
            match self.deployer.image_exists(&image_name).await {
                Ok(true) => {
                    info!("Promotion: Image '{}' exists locally", image_name);
                }
                Ok(false) => {
                    let mut active_dep: deployments::ActiveModel =
                        promoted_deployment.clone().into();
                    active_dep.state = Set("failed".to_string());
                    active_dep.finished_at = Set(Some(chrono::Utc::now()));
                    active_dep.cancelled_reason = Set(Some(format!(
                        "Docker image '{}' no longer exists locally",
                        image_name
                    )));
                    let _ = active_dep.update(self.db.as_ref()).await;

                    return Err(DeploymentError::Other(format!(
                        "Cannot promote: Docker image '{}' no longer exists locally. \
                         Consider redeploying from source instead.",
                        image_name
                    )));
                }
                Err(e) => {
                    return Err(DeploymentError::Other(format!(
                        "Cannot promote: failed to verify Docker image '{}': {}",
                        image_name, e
                    )));
                }
            }

            // --- Create per-job log paths (matching rollback/normal deployment pattern) ---
            let deploy_log_id = format!(
                "{}/{}/{}/{:02}/{:02}/{:02}/{:02}/deployment-{}-job-deploy_container.log",
                project.slug,
                target_env.slug,
                now.format("%Y"),
                now.format("%m"),
                now.format("%d"),
                now.format("%H"),
                now.format("%M"),
                promoted_id
            );
            let complete_log_id = format!(
                "{}/{}/{}/{:02}/{:02}/{:02}/{:02}/deployment-{}-job-mark_deployment_complete.log",
                project.slug,
                target_env.slug,
                now.format("%Y"),
                now.format("%m"),
                now.format("%d"),
                now.format("%H"),
                now.format("%M"),
                promoted_id
            );

            self.log_service
                .create_log_path(&deploy_log_id)
                .await
                .map_err(|e| {
                    DeploymentError::Other(format!("Failed to create deploy log path: {}", e))
                })?;
            self.log_service
                .create_log_path(&complete_log_id)
                .await
                .map_err(|e| {
                    DeploymentError::Other(format!("Failed to create complete log path: {}", e))
                })?;

            // --- Create deployment_jobs records ---
            use temps_entities::{deployment_jobs, types::JobStatus};

            let deploy_job_record = deployment_jobs::ActiveModel {
                deployment_id: Set(promoted_id),
                job_id: Set("deploy_container".to_string()),
                job_type: Set("DeployImageJob".to_string()),
                name: Set("Deploy Container".to_string()),
                description: Set(Some(format!("Promote: deploy image {}", image_name))),
                status: Set(JobStatus::Running),
                log_id: Set(deploy_log_id.clone()),
                job_config: Set(None),
                dependencies: Set(None),
                execution_order: Set(Some(0)),
                started_at: Set(Some(now)),
                ..Default::default()
            };
            let deploy_job_model =
                deploy_job_record
                    .insert(self.db.as_ref())
                    .await
                    .map_err(|e| {
                        DeploymentError::Other(format!("Failed to create deploy job record: {}", e))
                    })?;

            let complete_job_record = deployment_jobs::ActiveModel {
                deployment_id: Set(promoted_id),
                job_id: Set("mark_deployment_complete".to_string()),
                job_type: Set("MarkDeploymentCompleteJob".to_string()),
                name: Set("Mark Deployment Complete".to_string()),
                description: Set(Some("Finalize promoted deployment".to_string())),
                status: Set(JobStatus::Pending),
                log_id: Set(complete_log_id.clone()),
                job_config: Set(None),
                dependencies: Set(Some(
                    serde_json::to_value(vec!["deploy_container"]).unwrap_or_default(),
                )),
                execution_order: Set(Some(1)),
                ..Default::default()
            };
            let complete_job_model =
                complete_job_record
                    .insert(self.db.as_ref())
                    .await
                    .map_err(|e| {
                        DeploymentError::Other(format!(
                            "Failed to create complete job record: {}",
                            e
                        ))
                    })?;

            // Stop current environment containers before deploying
            info!(
                "Promotion: Stopping current containers for environment {}",
                target_environment_id
            );
            self.stop_environment_containers(target_environment_id, promoted_id)
                .await;

            info!("Promotion: Deploying image: {}", image_name);

            // Execute DeployImageJob with external image
            let mut deploy_builder = crate::jobs::DeployImageJobBuilder::new()
                .job_id("deploy_container".to_string())
                .build_job_id("external-image".to_string())
                .target(crate::jobs::DeploymentTarget::Docker {
                    registry_url: "local".to_string(),
                    network: Some(temps_core::NETWORK_NAME.to_string()),
                })
                .service_name(promote_slug.clone())
                .health_check_path(None)
                .replicas(
                    target_env
                        .deployment_config
                        .as_ref()
                        .map(|c| c.replicas as u32)
                        .or_else(|| {
                            project
                                .deployment_config
                                .as_ref()
                                .map(|c| c.replicas as u32)
                        })
                        .unwrap_or(1),
                )
                .port(
                    target_env
                        .deployment_config
                        .as_ref()
                        .and_then(|c| c.exposed_port)
                        .or_else(|| {
                            project
                                .deployment_config
                                .as_ref()
                                .and_then(|c| c.exposed_port)
                        })
                        .unwrap_or(3000) as u32,
                )
                .log_id(deploy_log_id.clone())
                .log_service(self.log_service.clone());

            // Apply container log rotation settings from config
            if let Ok(settings) = self.config_service.get_settings().await {
                deploy_builder =
                    deploy_builder.container_log_config(temps_deployer::ContainerLogConfig::new(
                        settings.container_logs.max_size.clone(),
                        settings.container_logs.max_file,
                    ));
            }

            // Resolve CPU/memory limits + requests (target env → project),
            // matching the normal deploy path so a promotion preserves a
            // configured limit and leaves an unconfigured environment uncapped.
            deploy_builder = deploy_builder.resources(Self::resolve_resource_usage(
                target_env.deployment_config.as_ref(),
                project.deployment_config.as_ref(),
            ));

            let deploy_job = deploy_builder
                .build(self.deployer.clone())
                .map_err(|e| DeploymentError::Other(format!("Failed to create deploy job: {}", e)))?
                .with_external_image_tag(image_name.clone());

            // Create workflow context for the promoted deployment
            let mock_log_writer = Arc::new(crate::test_utils::MockLogWriter::new(0));
            let mut promote_context = temps_core::WorkflowContext::new(
                format!("promote-{}", promoted_id),
                promoted_id,
                project_id,
                target_environment_id,
                mock_log_writer,
            );

            match deploy_job.execute(promote_context.clone()).await {
                Ok(job_result) => {
                    info!("Promotion: Deploy job completed successfully");
                    promote_context = job_result.context;

                    let mut active_job: deployment_jobs::ActiveModel = deploy_job_model.into();
                    active_job.status = Set(JobStatus::Success);
                    active_job.finished_at = Set(Some(chrono::Utc::now()));
                    let _ = active_job.update(self.db.as_ref()).await;
                }
                Err(e) => {
                    error!("Promotion: Deploy job failed: {}", e);

                    let mut active_job: deployment_jobs::ActiveModel = deploy_job_model.into();
                    active_job.status = Set(JobStatus::Failure);
                    active_job.finished_at = Set(Some(chrono::Utc::now()));
                    active_job.error_message = Set(Some(format!("Deploy failed: {}", e)));
                    let _ = active_job.update(self.db.as_ref()).await;

                    let mut active_complete: deployment_jobs::ActiveModel =
                        complete_job_model.into();
                    active_complete.status = Set(JobStatus::Cancelled);
                    active_complete.error_message = Set(Some("Deploy job failed".to_string()));
                    let _ = active_complete.update(self.db.as_ref()).await;

                    let mut active_dep: deployments::ActiveModel =
                        promoted_deployment.clone().into();
                    active_dep.state = Set("failed".to_string());
                    active_dep.finished_at = Set(Some(chrono::Utc::now()));
                    active_dep.cancelled_reason = Set(Some(format!("Deploy failed: {}", e)));
                    let _ = active_dep.update(self.db.as_ref()).await;

                    return Err(DeploymentError::Other(format!(
                        "Failed to deploy image during promotion: {}",
                        e
                    )));
                }
            }

            // Execute MarkDeploymentCompleteJob
            info!("Promotion: Marking deployment {} as complete", promoted_id);

            let mut active_complete: deployment_jobs::ActiveModel = complete_job_model.into();
            active_complete.status = Set(JobStatus::Running);
            active_complete.started_at = Set(Some(chrono::Utc::now()));
            let complete_job_model =
                active_complete
                    .update(self.db.as_ref())
                    .await
                    .map_err(|e| {
                        DeploymentError::Other(format!(
                            "Failed to update complete job status: {}",
                            e
                        ))
                    })?;

            let mark_complete_job = crate::jobs::MarkDeploymentCompleteJobBuilder::new()
                .job_id("mark_deployment_complete".to_string())
                .deployment_id(promoted_id)
                .db(self.db.clone())
                .log_id(complete_log_id)
                .log_service(self.log_service.clone())
                .container_deployer(self.deployer.clone())
                .queue(self.queue_service.clone())
                .config_service(self.config_service.clone())
                .encryption_service(self.encryption_service.clone())
                .build()
                .map_err(|e| {
                    DeploymentError::Other(format!("Failed to create mark complete job: {}", e))
                })?;

            match mark_complete_job.execute(promote_context).await {
                Ok(_) => {
                    info!("Promotion: Mark complete job executed successfully");

                    let mut active_job: deployment_jobs::ActiveModel = complete_job_model.into();
                    active_job.status = Set(JobStatus::Success);
                    active_job.finished_at = Set(Some(chrono::Utc::now()));
                    let _ = active_job.update(self.db.as_ref()).await;
                }
                Err(e) => {
                    error!("Promotion: Mark complete job failed: {}", e);

                    let mut active_job: deployment_jobs::ActiveModel = complete_job_model.into();
                    active_job.status = Set(JobStatus::Failure);
                    active_job.finished_at = Set(Some(chrono::Utc::now()));
                    active_job.error_message = Set(Some(format!("Mark complete failed: {}", e)));
                    let _ = active_job.update(self.db.as_ref()).await;

                    return Err(DeploymentError::Other(format!(
                        "Failed to mark deployment complete during promotion: {}",
                        e
                    )));
                }
            }

            info!(
                "Promotion completed - deployment {} is now active",
                promoted_id
            );
        }

        // Re-fetch the promoted deployment to get the final state
        let final_deployment = deployments::Entity::find_by_id(promoted_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::Other("Promoted deployment disappeared".to_string()))?;

        Ok(self
            .map_db_deployment_to_deployment(final_deployment, true, None)
            .await)
    }

    /// Tears down a specific deployment, removing containers and cleaning up resources
    pub async fn teardown_deployment(
        &self,
        project_id: i32,
        deployment_id: i32,
    ) -> Result<(), DeploymentError> {
        use temps_entities::deployment_containers;

        // Find the deployment
        let deployment = deployments::Entity::find_by_id(deployment_id)
            .filter(deployments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Deployment not found".to_string()))?;

        // Stop all containers for this deployment
        let containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        for container in containers {
            self.deployer
                .stop_container(&container.container_id)
                .await
                .map_err(|e| DeploymentError::Other(format!("Failed to stop container: {}", e)))?;

            // Mark container as deleted
            let mut active_container: deployment_containers::ActiveModel = container.into();
            active_container.deleted_at = Set(Some(chrono::Utc::now()));
            active_container.status = Set(Some("stopped".to_string()));
            active_container.update(self.db.as_ref()).await?;
        }

        // Update deployment state to "stopped"
        let mut active_deployment: deployments::ActiveModel = deployment.into();
        active_deployment.state = Set("stopped".to_string());
        active_deployment.update(self.db.as_ref()).await?;

        Ok(())
    }

    /// Tears down an environment and all its active deployments
    pub async fn teardown_environment(
        &self,
        project_id: i32,
        env_id: i32,
    ) -> Result<(), DeploymentError> {
        use temps_entities::deployment_containers;

        // Find all deployments in this environment
        let deployments = deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(project_id))
            .filter(deployments::Column::EnvironmentId.eq(env_id))
            .all(self.db.as_ref())
            .await?;

        // Stop all containers for all deployments
        for deployment in &deployments {
            let containers = deployment_containers::Entity::find()
                .filter(deployment_containers::Column::DeploymentId.eq(deployment.id))
                .filter(deployment_containers::Column::DeletedAt.is_null())
                .all(self.db.as_ref())
                .await?;

            for container in containers {
                // Stop container with timeout - don't fail the whole teardown if one container fails
                match tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    self.deployer.stop_container(&container.container_id),
                )
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        warn!(
                            "Failed to stop container {} during teardown: {} (continuing)",
                            container.container_id, e
                        );
                    }
                    Err(_) => {
                        warn!(
                            "Timed out stopping container {} after 30s during teardown (continuing)",
                            container.container_id
                        );
                    }
                }

                // Mark container as deleted
                let mut active_container: deployment_containers::ActiveModel = container.into();
                active_container.deleted_at = Set(Some(chrono::Utc::now()));
                active_container.status = Set(Some("stopped".to_string()));
                active_container.update(self.db.as_ref()).await?;
            }
        }

        // Update all deployment states to "stopped"
        for deployment in deployments {
            let mut active_deployment: deployments::ActiveModel = deployment.into();
            active_deployment.state = Set("stopped".to_string());
            active_deployment.update(self.db.as_ref()).await?;
        }

        Ok(())
    }

    pub async fn pause_deployment(
        &self,
        project_id: i32,
        deployment_id: i32,
    ) -> Result<(), DeploymentError> {
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::{deployment_containers, deployments};

        // First verify the deployment exists and belongs to the project
        let deployment = deployments::Entity::find_by_id(deployment_id)
            .filter(deployments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Deployment not found".to_string()))?;

        // Stop and remove all containers for this deployment
        let containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        for container in containers {
            // Stop the container first
            if let Err(e) = self.deployer.stop_container(&container.container_id).await {
                warn!(
                    "Failed to stop container {} during deployment pause: {}",
                    container.container_id, e
                );
            }

            // Remove the container
            if let Err(e) = self
                .deployer
                .remove_container(&container.container_id)
                .await
            {
                warn!(
                    "Failed to remove container {} during deployment pause: {}",
                    container.container_id, e
                );
            }

            // Update container status to removed
            let mut active_container: deployment_containers::ActiveModel = container.into();
            active_container.status = Set(Some("removed".to_string()));
            active_container.update(self.db.as_ref()).await?;
        }

        // Update deployment state to "paused"
        let mut active_deployment: deployments::ActiveModel = deployment.into();
        active_deployment.state = Set("paused".to_string());
        active_deployment.update(self.db.as_ref()).await?;

        info!(
            "Successfully paused deployment {}: removed all containers",
            deployment_id
        );
        Ok(())
    }

    pub async fn resume_deployment(
        &self,
        project_id: i32,
        deployment_id: i32,
    ) -> Result<(), DeploymentError> {
        use temps_entities::deployment_containers;

        // First verify the deployment exists and belongs to the project
        let deployment = deployments::Entity::find()
            .filter(deployments::Column::Id.eq(deployment_id))
            .filter(deployments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Deployment not found".to_string()))?;

        // Resume all containers for this deployment
        let containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        for container in containers {
            self.deployer
                .resume_container(&container.container_id)
                .await
                .map_err(|e| {
                    DeploymentError::Other(format!("Failed to resume container: {}", e))
                })?;

            // Update container status
            let mut active_container: deployment_containers::ActiveModel = container.into();
            active_container.status = Set(Some("running".to_string()));
            active_container.update(self.db.as_ref()).await?;
        }

        // Update deployment state to "deployed"
        let mut active_deployment: deployments::ActiveModel = deployment.into();
        active_deployment.state = Set("deployed".to_string());
        active_deployment.update(self.db.as_ref()).await?;

        info!("Successfully resumed deployment: {}", deployment_id);
        Ok(())
    }

    async fn get_environments_with_domains(
        &self,
        environment_ids: &[i32],
    ) -> Result<HashMap<i32, DeploymentEnvironment>, DeploymentError> {
        use temps_entities::{environments, project_custom_domains, projects};

        if environment_ids.is_empty() {
            return Ok(HashMap::new());
        }

        // Fetch all environments with their projects
        let environments = environments::Entity::find()
            .filter(environments::Column::Id.is_in(environment_ids.to_vec()))
            .find_also_related(projects::Entity)
            .all(self.db.as_ref())
            .await?;

        // Fetch all custom domains for these environments
        let custom_domains = project_custom_domains::Entity::find()
            .filter(project_custom_domains::Column::EnvironmentId.is_in(environment_ids.to_vec()))
            .filter(project_custom_domains::Column::Status.eq("active"))
            .all(self.db.as_ref())
            .await?;

        // Group domains by environment_id
        let mut domains_by_env: HashMap<i32, Vec<String>> = HashMap::new();
        for domain in custom_domains {
            domains_by_env
                .entry(domain.environment_id)
                .or_default()
                .push(domain.domain);
        }

        // Build the result map
        let mut result = HashMap::new();
        for (env, _project) in environments {
            let mut domains = domains_by_env.remove(&env.id).unwrap_or_default();

            // Build the environment URL from the env's stored `subdomain`
            // (the canonical hostname source). Reconstructing from project_slug
            // and env_slug would produce stale URLs after a subdomain rename,
            // since `environments.subdomain` can be renamed independently.
            let env_url = self
                .compute_environment_url(&env.subdomain)
                .await
                .unwrap_or_else(|_| format!("http://{}.localhost", env.subdomain));
            domains.insert(0, env_url);

            result.insert(
                env.id,
                DeploymentEnvironment {
                    id: env.id,
                    name: env.name,
                    slug: env.slug,
                    domains,
                },
            );
        }

        Ok(result)
    }

    async fn compute_deployment_url(&self, deployment_slug: &str) -> anyhow::Result<String> {
        let settings = self.config_service.get_settings().await.unwrap_or_default();

        let base_domain = settings.preview_domain;
        let domain = format!("{}.{}", deployment_slug, base_domain);

        // Determine protocol and port from external_url if set, otherwise default to http
        let (protocol, port) = if let Some(ref url) = settings.external_url {
            if let Ok(parsed_url) = url::Url::parse(url) {
                let scheme = match parsed_url.scheme() {
                    "https" => "https",
                    "http" => "http",
                    _ => "http",
                };
                (scheme, parsed_url.port())
            } else {
                // Fallback for malformed URLs - detect protocol from prefix
                let protocol = if url.starts_with("https://") {
                    "https"
                } else {
                    "http"
                };
                (protocol, None)
            }
        } else {
            ("http", None)
        };

        // Construct the URL with port if present
        // Only include port if it's non-standard (not 443 for https, not 80 for http)
        let url = if let Some(port) = port {
            let is_standard_port =
                (protocol == "https" && port == 443) || (protocol == "http" && port == 80);
            if is_standard_port {
                format!("{}://{}", protocol, domain)
            } else {
                format!("{}://{}:{}", protocol, domain, port)
            }
        } else {
            format!("{}://{}", protocol, domain)
        };

        Ok(url)
    }

    async fn compute_environment_url(&self, env_subdomain: &str) -> anyhow::Result<String> {
        let settings = self.config_service.get_settings().await.unwrap_or_default();

        let base_domain = settings.preview_domain;
        let domain = format!("{}.{}", env_subdomain, base_domain);

        // Determine protocol and port from external_url if set, otherwise default to http
        let (protocol, port) = if let Some(ref url) = settings.external_url {
            if let Ok(parsed_url) = url::Url::parse(url) {
                let scheme = match parsed_url.scheme() {
                    "https" => "https",
                    "http" => "http",
                    _ => "http",
                };
                (scheme, parsed_url.port())
            } else {
                // Fallback for malformed URLs - detect protocol from prefix
                let protocol = if url.starts_with("https://") {
                    "https"
                } else {
                    "http"
                };
                (protocol, None)
            }
        } else {
            ("http", None)
        };

        // Construct the URL with port if present
        // Only include port if it's non-standard (not 443 for https, not 80 for http)
        let url = if let Some(port) = port {
            let is_standard_port =
                (protocol == "https" && port == 443) || (protocol == "http" && port == 80);
            if is_standard_port {
                format!("{}://{}", protocol, domain)
            } else {
                format!("{}://{}:{}", protocol, domain, port)
            }
        } else {
            format!("{}://{}", protocol, domain)
        };

        Ok(url)
    }

    async fn map_db_deployment_to_deployment(
        &self,
        db_deployment: deployments::Model,
        is_current: bool,
        environment: Option<DeploymentEnvironment>,
    ) -> Deployment {
        // Use provided environment or create a basic one
        let environment = environment.unwrap_or_else(|| DeploymentEnvironment {
            id: db_deployment.environment_id,
            name: "Environment".to_string(),
            slug: "environment".to_string(),
            domains: vec![],
        });

        // Extract commit information from deployment metadata or fields
        let commit_sha = db_deployment.commit_sha.clone();
        let commit_message = db_deployment.commit_message.clone();
        let branch_ref = db_deployment.branch_ref.clone();
        let tag_ref = db_deployment.tag_ref.clone();

        let repo_commit: Option<octocrab::models::repos::RepoCommit> =
            match &db_deployment.commit_json {
                Some(commit) => serde_json::from_value(commit.clone()).ok(),
                None => None,
            };
        let commit_author = repo_commit
            .clone()
            .and_then(|rc| rc.author.map(|a| a.login))
            .map(|login| login.to_string());
        let commit_date = repo_commit
            .clone()
            .and_then(|rc| rc.commit.committer.and_then(|c| c.date));

        // Compute the actual URL from the stored slug
        let deployment_url = self
            .compute_deployment_url(&db_deployment.slug)
            .await
            .unwrap_or_else(|_| format!("http://{}", db_deployment.slug));

        Deployment {
            id: db_deployment.id,
            project_id: db_deployment.project_id,
            environment_id: db_deployment.environment_id,
            environment,
            status: db_deployment.state,
            url: deployment_url,
            commit_hash: commit_sha,
            commit_message,
            branch: branch_ref,
            tag: tag_ref,
            created_at: db_deployment.created_at,
            started_at: db_deployment.started_at,
            finished_at: db_deployment.finished_at,
            screenshot_location: db_deployment.screenshot_location,
            commit_author,
            commit_date,
            is_current,
            cancelled_reason: db_deployment.cancelled_reason.clone(),
            deployment_config: db_deployment.deployment_config,
            metadata: db_deployment.metadata,
        }
    }

    /// Add a custom domain to a deployment (marks it as not calculated)
    pub async fn add_custom_domain(
        &self,
        deployment_id: i32,
        domain: String,
    ) -> Result<deployment_domains::Model, DeploymentError> {
        // Check if deployment exists
        let _deployment = deployments::Entity::find_by_id(deployment_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!("Deployment {} not found", deployment_id))
            })?;

        // Remove any existing calculated domains for this deployment
        deployment_domains::Entity::delete_many()
            .filter(deployment_domains::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_domains::Column::IsCalculated.eq(true))
            .exec(self.db.as_ref())
            .await?;

        // Add the custom domain
        let new_domain = deployment_domains::ActiveModel {
            deployment_id: Set(deployment_id),
            domain: Set(domain),
            is_calculated: Set(false), // This is a user-set custom domain
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        };

        let domain = new_domain.insert(self.db.as_ref()).await?;

        info!(
            "Added custom domain {} to deployment {}",
            domain.domain, deployment_id
        );
        Ok(domain)
    }

    /// Update deployment to use calculated wildcard domain
    pub async fn use_calculated_domain(
        &self,
        deployment_id: i32,
        project: &projects::Model,
        environment: &environments::Model,
    ) -> Result<deployment_domains::Model, DeploymentError> {
        // Get preview domain from config service
        let settings = self
            .config_service
            .get_settings()
            .await
            .map_err(|e| DeploymentError::Other(format!("Failed to get settings: {}", e)))?;

        let base_domain = settings.preview_domain.trim_start_matches("*.").to_string();

        // Get pipeline id from deployment
        let deployment = deployments::Entity::find_by_id(deployment_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!("Deployment {} not found", deployment_id))
            })?;

        let domain = format!(
            "{}-{}-{}.{}",
            project.slug, environment.slug, deployment.id, base_domain
        );

        // Remove any existing domains for this deployment
        deployment_domains::Entity::delete_many()
            .filter(deployment_domains::Column::DeploymentId.eq(deployment_id))
            .exec(self.db.as_ref())
            .await?;

        // Add the calculated domain
        let new_domain = deployment_domains::ActiveModel {
            deployment_id: Set(deployment_id),
            domain: Set(domain.clone()),
            is_calculated: Set(true), // This is a calculated wildcard domain
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        };

        let domain_model = new_domain.insert(self.db.as_ref()).await?;

        info!(
            "Updated deployment {} to use calculated domain {}",
            deployment_id, domain
        );
        Ok(domain_model)
    }

    /// Get all domains for a deployment with their type information
    pub async fn get_deployment_domains_with_type(
        &self,
        deployment_id: i32,
    ) -> Result<Vec<deployment_domains::Model>, DeploymentError> {
        let domains = deployment_domains::Entity::find()
            .filter(deployment_domains::Column::DeploymentId.eq(deployment_id))
            .all(self.db.as_ref())
            .await?;

        Ok(domains)
    }

    /// Remove a custom domain from a deployment
    pub async fn remove_custom_domain(
        &self,
        deployment_id: i32,
        domain_id: i32,
    ) -> Result<(), DeploymentError> {
        // Only allow removing non-calculated domains
        let domain = deployment_domains::Entity::find_by_id(domain_id)
            .filter(deployment_domains::Column::DeploymentId.eq(deployment_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Domain not found".to_string()))?;

        if domain.is_calculated {
            return Err(DeploymentError::InvalidInput(
                "Cannot remove calculated domains. Use custom domain instead.".to_string(),
            ));
        }

        deployment_domains::Entity::delete_by_id(domain_id)
            .exec(self.db.as_ref())
            .await?;

        info!(
            "Removed custom domain {} from deployment {}",
            domain.domain, deployment_id
        );
        Ok(())
    }

    /// Get all jobs for a deployment
    pub async fn get_deployment_jobs(
        &self,
        deployment_id: i32,
    ) -> Result<Vec<temps_entities::deployment_jobs::Model>, DeploymentError> {
        use temps_entities::deployment_jobs;

        let jobs = deployment_jobs::Entity::find()
            .filter(deployment_jobs::Column::DeploymentId.eq(deployment_id))
            .order_by_asc(deployment_jobs::Column::ExecutionOrder)
            .all(self.db.as_ref())
            .await
            .map_err(|e| DeploymentError::DatabaseError {
                reason: e.to_string(),
            })?;

        Ok(jobs)
    }

    /// Cancel all running deployments with a given reason
    /// This is typically called during server shutdown or startup
    pub async fn cancel_running_deployments(
        &self,
        cancelled_reason: &str,
    ) -> Result<u64, DeploymentError> {
        use sea_orm::sea_query::Expr;
        use temps_entities::deployments;

        debug!(
            "Cancelling all running deployments with reason: {}",
            cancelled_reason
        );

        // Update all running deployments to cancelled status in a single query
        let result = deployments::Entity::update_many()
            .filter(deployments::Column::State.eq("running"))
            .col_expr(deployments::Column::State, Expr::value("cancelled"))
            .col_expr(
                deployments::Column::CancelledReason,
                Expr::value(cancelled_reason),
            )
            .col_expr(
                deployments::Column::FinishedAt,
                Expr::current_timestamp().into(),
            )
            .col_expr(
                deployments::Column::UpdatedAt,
                Expr::current_timestamp().into(),
            )
            .exec(self.db.as_ref())
            .await
            .map_err(|e| DeploymentError::DatabaseError {
                reason: e.to_string(),
            })?;

        let count = result.rows_affected;

        if count > 0 {
            info!("Successfully cancelled {} running deployment(s)", count);
        } else {
            debug!("No running deployments found");
        }

        Ok(count)
    }

    /// Cancel all active deployments for an environment
    ///
    /// Used when deleting an environment to ensure no deployments are left running
    /// This method:
    /// 1. Stops and removes all running containers
    /// 2. Writes cancellation messages to job logs
    /// 3. Updates deployment states to cancelled
    pub async fn cancel_all_environment_deployments(
        &self,
        environment_id: i32,
    ) -> Result<u64, DeploymentError> {
        use temps_entities::{deployment_jobs, types::JobStatus};

        info!(
            "Cancelling all active deployments for environment {}",
            environment_id
        );

        // First, stop and remove all containers for this environment
        info!(
            "Stopping and removing all containers for environment {}",
            environment_id
        );

        let containers = deployment_containers::Entity::find()
            .inner_join(deployments::Entity)
            .filter(deployments::Column::EnvironmentId.eq(environment_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        for container in containers {
            info!(
                "Stopping and removing container {} for environment {}",
                container.container_id, environment_id
            );

            // Stop the container with a 30-second timeout to prevent hanging
            match tokio::time::timeout(
                std::time::Duration::from_secs(30),
                self.deployer.stop_container(&container.container_id),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    warn!(
                        "Failed to stop container {}: {} (continuing anyway)",
                        container.container_id, e
                    );
                }
                Err(_) => {
                    warn!(
                        "Timed out stopping container {} after 30s (continuing anyway)",
                        container.container_id
                    );
                }
            }

            // Remove the container with a 15-second timeout
            match tokio::time::timeout(
                std::time::Duration::from_secs(15),
                self.deployer.remove_container(&container.container_id),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    warn!(
                        "Failed to remove container {}: {} (continuing anyway)",
                        container.container_id, e
                    );
                }
                Err(_) => {
                    warn!(
                        "Timed out removing container {} after 15s (continuing anyway)",
                        container.container_id
                    );
                }
            }

            // Update container status to stopped
            let mut active_container: deployment_containers::ActiveModel = container.into();
            active_container.status = Set(Some("stopped".to_string()));
            active_container.deleted_at = Set(Some(chrono::Utc::now()));
            let _ = active_container.update(self.db.as_ref()).await;
        }

        // Find all active deployments for this environment
        let active_deployments = deployments::Entity::find()
            .filter(deployments::Column::EnvironmentId.eq(environment_id))
            .filter(deployments::Column::State.is_in(vec![
                "pending",
                "running",
                "deploying",
                "ready",
            ]))
            .all(self.db.as_ref())
            .await?;

        let count = active_deployments.len() as u64;

        if count == 0 {
            info!(
                "No active deployments found for environment {}",
                environment_id
            );
            return Ok(0);
        }

        info!(
            "Found {} active deployment(s) for environment {} - cancelling",
            count, environment_id
        );

        for deployment in active_deployments {
            // Find currently running jobs and write cancellation message to their logs
            let running_jobs = deployment_jobs::Entity::find()
                .filter(deployment_jobs::Column::DeploymentId.eq(deployment.id))
                .filter(deployment_jobs::Column::Status.eq(JobStatus::Running))
                .all(self.db.as_ref())
                .await?;

            for job in running_jobs {
                info!(
                    "📝 Writing cancellation message to running job: {} ({})",
                    job.name, job.log_id
                );

                let cancel_msg = format!(
                    "DEPLOYMENT CANCELLED DUE TO ENVIRONMENT DELETION - Job '{}' is being terminated",
                    job.name
                );
                if let Err(e) = self
                    .log_service
                    .append_structured_log(&job.log_id, temps_logs::LogLevel::Error, &cancel_msg)
                    .await
                {
                    warn!(
                        "Failed to write cancellation message to job log {}: {}",
                        job.log_id, e
                    );
                }
            }

            // Update deployment to cancelled state
            let mut active_deployment: deployments::ActiveModel = deployment.into();
            active_deployment.state = Set("cancelled".to_string());
            active_deployment.cancelled_reason = Set(Some("Environment deleted".to_string()));
            active_deployment.finished_at = Set(Some(chrono::Utc::now()));
            active_deployment.updated_at = Set(chrono::Utc::now());
            active_deployment.update(self.db.as_ref()).await?;
        }

        info!(
            "Successfully cancelled {} deployment(s) and cleaned up containers for environment {}",
            count, environment_id
        );

        Ok(count)
    }

    /// Cancel a specific deployment
    pub async fn cancel_deployment(
        &self,
        project_id: i32,
        deployment_id: i32,
    ) -> Result<(), DeploymentError> {
        use temps_entities::{deployment_jobs, types::JobStatus};

        info!(
            "Cancelling deployment {} for project {}",
            deployment_id, project_id
        );

        // Verify the deployment exists and belongs to the project
        let deployment = deployments::Entity::find_by_id(deployment_id)
            .filter(deployments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Deployment not found".to_string()))?;

        info!(
            "Deployment {} current state: '{}' - checking if cancellable",
            deployment_id, deployment.state
        );

        // Only allow cancelling deployments in pending or running state
        if deployment.state != "pending" && deployment.state != "running" {
            info!(
                "Cannot cancel deployment {} - already in '{}' state",
                deployment_id, deployment.state
            );
            return Err(DeploymentError::InvalidInput(format!(
                "Cannot cancel deployment in '{}' state. Only 'pending' or 'running' deployments can be cancelled.",
                deployment.state
            )));
        }

        // Find currently running job and write cancellation message to its logs
        let running_jobs = deployment_jobs::Entity::find()
            .filter(deployment_jobs::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_jobs::Column::Status.eq(JobStatus::Running))
            .all(self.db.as_ref())
            .await?;

        for job in running_jobs {
            info!(
                "📝 Writing cancellation message to running job: {} ({})",
                job.name, job.log_id
            );

            // Write cancellation message to the job's log
            let cancel_msg = format!(
                "DEPLOYMENT CANCELLED BY USER - Job '{}' is being terminated",
                job.name
            );
            if let Err(e) = self
                .log_service
                .append_structured_log(&job.log_id, temps_logs::LogLevel::Error, &cancel_msg)
                .await
            {
                warn!(
                    "Failed to write cancellation message to job log {}: {}",
                    job.log_id, e
                );
            }
        }

        // Snapshot fields we'll need *after* the move into ActiveModel for
        // the queue event below — the active model takes ownership of the row.
        let environment_id = deployment.environment_id;

        // Update deployment to cancelled state
        let mut active_deployment: deployments::ActiveModel = deployment.into();
        active_deployment.state = Set("cancelled".to_string());
        active_deployment.cancelled_reason = Set(Some("Cancelled by user".to_string()));
        active_deployment.finished_at = Set(Some(chrono::Utc::now()));
        active_deployment.updated_at = Set(chrono::Utc::now());
        active_deployment.update(self.db.as_ref()).await?;

        // Publish a DeploymentCancelled event so downstream listeners (PR
        // commenter, notifications, audit consumers) can react. The workflow
        // executor publishes the same event when it transitions to Cancelled
        // mid-pipeline; this site covers user-initiated cancels from the UI /
        // API, which previously left the PR comment stuck on "Deploying preview".
        //
        // Best-effort: a queue failure here must NOT undo the cancellation —
        // log and move on, mirroring how DeploymentFailed/Succeeded handle it
        // elsewhere in this file.
        let environment_name =
            match temps_entities::environments::Entity::find_by_id(environment_id)
                .one(self.db.as_ref())
                .await
            {
                Ok(Some(env)) => env.name,
                _ => String::new(),
            };
        let event = temps_core::Job::DeploymentCancelled(temps_core::DeploymentCancelledJob {
            deployment_id,
            project_id,
            environment_id,
            environment_name,
        });
        if let Err(e) = self.queue_service.send(event).await {
            warn!(
                "Failed to send DeploymentCancelled event for deployment {}: {}",
                deployment_id, e
            );
        }

        info!(
            "Successfully cancelled deployment {} for project {} - workflow will stop at next checkpoint",
            deployment_id, project_id
        );

        Ok(())
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
            domains: vec![], // Could be populated if needed
        };

        Ok((container, env_info))
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

        // Route to the worker that owns this container — calling the local
        // CP dockerd for a remote container would 404 silently, leaving the
        // container running while the UI thinks it stopped.
        let deployer = self.deployer_for_node(container.node_id).await?;
        deployer
            .stop_container(&container.container_id)
            .await
            .map_err(|e| DeploymentError::Other(format!("Failed to stop container: {}", e)))?;

        // Update container status in database
        let mut active_container: deployment_containers::ActiveModel = container.into();
        active_container.status = Set(Some("stopped".to_string()));
        active_container.update(self.db.as_ref()).await?;

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

        let deployer = self.deployer_for_node(container.node_id).await?;
        deployer
            .start_container(&container.container_id)
            .await
            .map_err(|e| DeploymentError::Other(format!("Failed to start container: {}", e)))?;

        // Update container status in database
        let mut active_container: deployment_containers::ActiveModel = container.into();
        active_container.status = Set(Some("running".to_string()));
        active_container.update(self.db.as_ref()).await?;

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

    /// Get deployment activity graph for the last N days
    /// Returns daily counts of unique commits deployed, with intensity levels for GitHub-style contribution graph
    /// Note: Only counts deployments that have a commit SHA, and counts each unique commit once per day
    pub async fn get_activity_graph(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        days: i32,
    ) -> Result<crate::handlers::types::ActivityGraphResponse, DeploymentError> {
        use chrono::{Duration, NaiveDate, Utc};
        use std::collections::HashMap;

        let end_date = Utc::now().date_naive();
        let start_date = end_date - Duration::days(days as i64 - 1);

        // Convert NaiveDate to DateTime for comparison
        let start_datetime = start_date.and_hms_opt(0, 0, 0).unwrap().and_utc();
        let end_datetime = end_date.and_hms_opt(23, 59, 59).unwrap().and_utc();

        // Build query using Sea-ORM
        let mut query = deployments::Entity::find()
            .filter(deployments::Column::CreatedAt.gte(start_datetime))
            .filter(deployments::Column::CreatedAt.lte(end_datetime));

        if let Some(pid) = project_id {
            query = query.filter(deployments::Column::ProjectId.eq(pid));
        }

        if let Some(eid) = environment_id {
            query = query.filter(deployments::Column::EnvironmentId.eq(eid));
        }

        // Fetch all deployments in the date range
        let deployments_list = query.all(self.db.as_ref()).await?;

        // Group deployments by date, counting unique commit SHAs per day
        // We use a HashMap of HashSet to track unique commits per date
        let mut commits_by_date: HashMap<NaiveDate, std::collections::HashSet<String>> =
            HashMap::new();
        for deployment in deployments_list {
            let date = deployment.created_at.date_naive();

            // Only count deployments with a commit SHA
            if let Some(commit_sha) = deployment.commit_sha {
                commits_by_date.entry(date).or_default().insert(commit_sha);
            }
        }

        // Convert unique commits to counts
        let mut activity_map: HashMap<NaiveDate, i64> = HashMap::new();
        for (date, commits) in commits_by_date {
            activity_map.insert(date, commits.len() as i64);
        }

        // Generate all days in the range (including days with zero activity)
        let mut days_vec = Vec::new();
        let mut total_count = 0i64;
        let mut current = start_date;

        while current <= end_date {
            let count = activity_map.get(&current).copied().unwrap_or(0);
            total_count += count;

            // Calculate intensity level for visualization
            // 0: No activity, 1: Low (1-2), 2: Medium (3-5), 3: High (6-10), 4: Very High (11+)
            let level = match count {
                0 => 0,
                1..=2 => 1,
                3..=5 => 2,
                6..=10 => 3,
                _ => 4,
            };

            days_vec.push(crate::handlers::types::ActivityDay {
                date: current.to_string(),
                count,
                level,
            });

            current = current.succ_opt().unwrap_or(current);
        }

        Ok(crate::handlers::types::ActivityGraphResponse {
            days: days_vec,
            total_count,
            start_date: start_date.to_string(),
            end_date: end_date.to_string(),
        })
    }
}

// Implement DeploymentCanceller trait from temps-core
#[async_trait::async_trait]
impl temps_core::DeploymentCanceller for DeploymentService {
    async fn cancel_all_environment_deployments(
        &self,
        environment_id: i32,
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        self.cancel_all_environment_deployments(environment_id)
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::Utc;
    use mockall::mock;
    use sea_orm::{ActiveModelTrait, EntityTrait, Set};

    use std::sync::Arc;
    use temps_core::EncryptionService;
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{
        deployment_config::DeploymentConfig, deployments, env_vars, environments,
        external_services, preset::Preset, project_services, projects,
        upstream_config::UpstreamList,
    };

    // Mock for other services
    mock! {
        LogService {}
    }

    mock! {
        ConfigService {}
    }

    mock! {
        QueueService {}
        #[async_trait::async_trait]
        impl temps_core::JobQueue for QueueService {
            async fn send(&self, job: temps_core::Job) -> Result<(), temps_core::QueueError>;
            fn subscribe(&self) -> Box<dyn temps_core::JobReceiver>;
        }
    }

    mock! {
        DockerLogService {}
    }

    mock! {
        JobReceiver {}
        #[async_trait::async_trait]
        impl temps_core::JobReceiver for JobReceiver {
            async fn recv(&mut self) -> Result<temps_core::Job, temps_core::QueueError>;
        }
    }

    mock! {
        ContainerDeployer {}
        #[async_trait::async_trait]
        impl temps_deployer::ContainerDeployer for ContainerDeployer {
            async fn deploy_container(&self, request: temps_deployer::DeployRequest) -> Result<temps_deployer::DeployResult, temps_deployer::DeployerError>;
            async fn start_container(&self, container_id: &str) -> Result<(), temps_deployer::DeployerError>;
            async fn stop_container(&self, container_id: &str) -> Result<(), temps_deployer::DeployerError>;
            async fn pause_container(&self, container_id: &str) -> Result<(), temps_deployer::DeployerError>;
            async fn resume_container(&self, container_id: &str) -> Result<(), temps_deployer::DeployerError>;
            async fn remove_container(&self, container_id: &str) -> Result<(), temps_deployer::DeployerError>;
            async fn get_container_info(&self, container_id: &str) -> Result<temps_deployer::ContainerInfo, temps_deployer::DeployerError>;
            async fn get_container_stats(&self, container_id: &str) -> Result<temps_deployer::ContainerStats, temps_deployer::DeployerError>;
            async fn list_containers(&self) -> Result<Vec<temps_deployer::ContainerInfo>, temps_deployer::DeployerError>;
            async fn get_container_logs(&self, container_id: &str) -> Result<String, temps_deployer::DeployerError>;
            async fn stream_container_logs(&self, container_id: &str) -> Result<Box<dyn futures::Stream<Item = String> + Unpin + Send>, temps_deployer::DeployerError>;
            async fn image_exists(&self, image_name: &str) -> Result<bool, temps_deployer::DeployerError>;
        }
    }
    fn create_test_external_service_manager(
        db: Arc<temps_database::DbConnection>,
    ) -> Arc<temps_providers::ExternalServiceManager> {
        let encryption_service = create_test_encryption_service();
        let docker = Arc::new(bollard::Docker::connect_with_local_defaults().ok().unwrap());
        let dns_registry = Arc::new(temps_providers::DnsRegistry::new(db.clone()));
        Arc::new(temps_providers::ExternalServiceManager::new(
            db,
            encryption_service,
            docker,
            dns_registry,
        ))
    }

    fn create_test_encryption_service() -> Arc<EncryptionService> {
        Arc::new(
            EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
        )
    }

    async fn setup_test_data(
        db: &Arc<temps_database::DbConnection>,
    ) -> Result<
        (projects::Model, environments::Model, deployments::Model),
        Box<dyn std::error::Error>,
    > {
        // Create test project
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            repo_owner: Set("test-owner".to_string()),
            repo_name: Set("test-repo".to_string()),
            main_branch: Set("main".to_string()),
            git_provider_connection_id: Set(Some(1)),
            preset: Set(Preset::NextJs),
            directory: Set("/".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            deleted_at: Set(None),
            is_deleted: Set(false),
            deployment_config: Set(Some(DeploymentConfig::default())),
            last_deployment: Set(None),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await?;

        // Create test environment
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Test Environment".to_string()),
            slug: Set("test".to_string()),
            host: Set("test.example.com".to_string()), // Add required host field
            upstreams: Set(UpstreamList::default()),   // Add required upstreams field (empty array)
            current_deployment_id: Set(None),
            subdomain: Set("test.example.com".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let environment = environment.insert(db.as_ref()).await?;

        // Create test deployment
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("test-deployment-123".to_string()),
            state: Set("deployed".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            image_name: Set(Some("nginx:latest".to_string())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let deployment = deployment.insert(db.as_ref()).await?;

        Ok((project, environment, deployment))
    }

    async fn setup_test_environment_variables(
        db: &Arc<temps_database::DbConnection>,
        project_id: i32,
        environment_id: i32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Create project-level environment variables
        let project_env = env_vars::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(None),
            key: Set("PROJECT_VAR".to_string()),
            value: Set("project_value".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        project_env.insert(db.as_ref()).await?;

        // Create environment-specific environment variables
        let env_specific = env_vars::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(Some(environment_id)),
            key: Set("ENV_VAR".to_string()),
            value: Set("env_value".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        env_specific.insert(db.as_ref()).await?;

        // Override project var at environment level
        let env_override = env_vars::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(Some(environment_id)),
            key: Set("PROJECT_VAR".to_string()),
            value: Set("overridden_value".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        env_override.insert(db.as_ref()).await?;

        Ok(())
    }

    #[allow(dead_code)]
    async fn setup_test_external_services(
        db: &Arc<temps_database::DbConnection>,
        project_id: i32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Create external service
        let external_service = external_services::ActiveModel {
            name: Set("Redis".to_string()),
            service_type: Set("redis".to_string()),
            version: Set(Some("7.0".to_string())),
            status: Set("active".to_string()),
            slug: Set(Some("redis".to_string())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let external_service = external_service.insert(db.as_ref()).await?;

        // Create project-service relationship
        let project_service = project_services::ActiveModel {
            project_id: Set(project_id),
            service_id: Set(external_service.id),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        project_service.insert(db.as_ref()).await?;

        Ok(())
    }

    fn create_deployment_service_for_test(
        db: Arc<temps_database::DbConnection>,
    ) -> DeploymentService {
        // Create mock log service
        let log_service = Arc::new(temps_logs::LogService::new(std::env::temp_dir()));

        // Create a minimal real config service for testing
        // We need to provide the database URL that the test database is using
        let test_db_url = "postgresql://test_user:test_password@localhost:5432/test_db";
        let server_config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:8080".to_string(),
                test_db_url.to_string(),
                None,
                None,
            )
            .expect("Failed to create test server config"),
        );
        let config_service = Arc::new(temps_config::ConfigService::new(server_config, db.clone()));

        // Use a real broadcast queue so that mark_complete's route-ready
        // wait can be satisfied. We spawn a background task that listens for
        // any job on the queue and automatically responds with a
        // RouteTableUpdated event, simulating what the PG route listener does
        // in production.
        let (queue_service, _keep_alive) =
            temps_queue::BroadcastQueueService::create_job_queue_arc_with_receiver(64);
        {
            let queue_for_auto_responder = queue_service.clone();
            let mut auto_rx = queue_service.subscribe();
            tokio::spawn(async move {
                loop {
                    match auto_rx.recv().await {
                        Ok(temps_core::Job::RouteTableUpdated(_)) => {
                            // Don't echo RouteTableUpdated events (avoid infinite loop)
                        }
                        Ok(_job) => {
                            // Ignore other jobs — the route table update is
                            // triggered by the DB PG trigger, not by queue
                            // events. We just need to keep this receiver alive.
                        }
                        Err(_) => break,
                    }
                }
                drop(queue_for_auto_responder);
            });
        }
        // Spawn a second task that periodically sends RouteTableUpdated for
        // any deployment currently going through mark_complete. Since we don't
        // know the exact IDs, we listen for the `current_deployment_id` DB
        // change. In tests, instead we just send a broadly-matching event
        // after a short delay.
        //
        // In practice, for integration tests the simplest approach is to have
        // `wait_for_route_ready` accept an environment_id of None as a
        // wildcard, but that would weaken production safety. Instead, we
        // directly send the right event from a monitoring task on the DB.
        //
        // For unit tests: we use a simpler approach — we send the
        // RouteTableUpdated from a DB-watching perspective. Since tests use
        // real DB, we poll the environments table for current_deployment_id
        // changes and then send the corresponding RouteTableUpdated.
        {
            let queue_for_watcher = queue_service.clone();
            let db_for_watcher = db.clone();
            tokio::spawn(async move {
                use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
                // Poll every 50ms for environments with a current_deployment_id
                // that doesn't have a matching completed deployment yet.
                // This simulates the PG route listener.
                let mut seen: std::collections::HashSet<(i32, i32)> =
                    std::collections::HashSet::new();
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

                    let envs = match temps_entities::environments::Entity::find()
                        .filter(
                            temps_entities::environments::Column::CurrentDeploymentId.is_not_null(),
                        )
                        .all(db_for_watcher.as_ref())
                        .await
                    {
                        Ok(envs) => envs,
                        Err(_) => continue,
                    };

                    for env in envs {
                        if let Some(dep_id) = env.current_deployment_id {
                            if seen.insert((env.id, dep_id)) {
                                let _ = queue_for_watcher
                                    .send(temps_core::Job::RouteTableUpdated(
                                        temps_core::RouteTableUpdatedJob {
                                            environment_id: Some(env.id),
                                            deployment_id: Some(dep_id),
                                            route_count: 1,
                                        },
                                    ))
                                    .await;
                            }
                        }
                    }
                }
            });
        }

        // Create real docker log service for testing
        // For tests, we'll create a basic Docker connection (may fail but that's OK for tests)
        let docker = Arc::new(bollard::Docker::connect_with_local_defaults().unwrap());
        let docker_log_service = Arc::new(temps_logs::DockerLogService::new(docker));

        // Create mock deployer with all required methods
        let mut deployer = MockContainerDeployer::new();
        deployer.expect_deploy_container().returning(|_| {
            Ok(temps_deployer::DeployResult {
                container_id: "test-container".to_string(),
                container_name: "test-container".to_string(),
                container_port: 3000,
                host_port: 3000,
                status: temps_deployer::ContainerStatus::Running,
            })
        });
        deployer.expect_start_container().returning(|_| Ok(()));
        deployer.expect_stop_container().returning(|_| Ok(()));
        deployer.expect_pause_container().returning(|_| Ok(()));
        deployer.expect_resume_container().returning(|_| Ok(()));
        deployer.expect_remove_container().returning(|_| Ok(()));
        deployer
            .expect_get_container_logs()
            .returning(|_| Ok("test logs".to_string()));
        deployer.expect_get_container_info().returning(|_| {
            use std::collections::HashMap;
            Ok(temps_deployer::ContainerInfo {
                container_id: "test-container".to_string(),
                container_name: "test-container".to_string(),
                image_name: "nginx:latest".to_string(),
                created_at: chrono::Utc::now(),
                ports: vec![],
                environment_vars: HashMap::new(),
                status: temps_deployer::ContainerStatus::Running,
                restart_count: Some(0),
                labels: std::collections::HashMap::new(),
                ..Default::default()
            })
        });
        deployer.expect_list_containers().returning(|| Ok(vec![]));
        deployer.expect_stream_container_logs().returning(|_| {
            use futures::stream;
            let stream = stream::empty();
            Ok(Box::new(stream))
        });
        deployer.expect_image_exists().returning(|_| Ok(true));
        let deployer: Arc<dyn temps_deployer::ContainerDeployer> = Arc::new(deployer);

        // For tests, we'll create a service that directly accepts the trait
        DeploymentService {
            db,
            log_service,
            config_service,
            queue_service,
            docker_log_service,
            deployer,
            encryption_service: create_test_encryption_service(),
        }
    }

    #[tokio::test]
    async fn test_pause_deployment() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Setup test data
        let (_project, _environment, deployment) = setup_test_data(&db).await?;

        let deployment_service = create_deployment_service_for_test(db.clone());

        // Test pause deployment
        deployment_service
            .pause_deployment(deployment.project_id, deployment.id)
            .await?;

        // Verify deployment state was updated
        let updated_deployment = deployments::Entity::find_by_id(deployment.id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(updated_deployment.state, "paused");

        Ok(())
    }

    #[tokio::test]
    async fn test_resume_deployment() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Setup test data
        let (_project, environment, mut deployment) = setup_test_data(&db).await?;
        setup_test_environment_variables(&db, deployment.project_id, environment.id).await?;

        // Set deployment to paused state
        let mut active_deployment: deployments::ActiveModel = deployment.clone().into();
        active_deployment.state = Set("paused".to_string());
        deployment = active_deployment.update(db.as_ref()).await?;

        let deployment_service = create_deployment_service_for_test(db.clone());

        // Test resume deployment
        deployment_service
            .resume_deployment(deployment.project_id, deployment.id)
            .await?;

        // Verify deployment state was updated
        let updated_deployment = deployments::Entity::find_by_id(deployment.id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(updated_deployment.state, "deployed");

        Ok(())
    }

    #[tokio::test]
    async fn test_rollback_to_deployment() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Setup test data
        let (_project, mut environment, target_deployment) = setup_test_data(&db).await?;
        setup_test_environment_variables(&db, target_deployment.project_id, environment.id).await?;

        // Create container for target deployment (required for rollback)
        let now = Utc::now();
        let target_container = deployment_containers::ActiveModel {
            deployment_id: Set(target_deployment.id),
            container_id: Set("container-rollback-target".to_string()),
            container_name: Set("app-rollback-target".to_string()),
            container_port: Set(8080),
            image_name: Set(Some("nginx:target".to_string())),
            status: Set(Some("running".to_string())),
            created_at: Set(now),
            deployed_at: Set(now),
            ..Default::default()
        };
        target_container.insert(db.as_ref()).await?;

        // Create current deployment that will be stopped
        let current_deployment = deployments::ActiveModel {
            project_id: Set(target_deployment.project_id),
            environment_id: Set(environment.id),
            slug: Set("current-deployment-456".to_string()),
            state: Set("deployed".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            image_name: Set(Some("nginx:current".to_string())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let current_deployment = current_deployment.insert(db.as_ref()).await?;

        // Create container for current deployment
        let current_container = deployment_containers::ActiveModel {
            deployment_id: Set(current_deployment.id),
            container_id: Set("container-rollback-current".to_string()),
            container_name: Set("app-rollback-current".to_string()),
            container_port: Set(8080),
            image_name: Set(Some("nginx:current".to_string())),
            status: Set(Some("running".to_string())),
            created_at: Set(now),
            deployed_at: Set(now),
            ..Default::default()
        };
        current_container.insert(db.as_ref()).await?;

        // Update environment to point to current deployment
        let mut active_environment: environments::ActiveModel = environment.into();
        active_environment.current_deployment_id = Set(Some(current_deployment.id));
        environment = active_environment.update(db.as_ref()).await?;

        let deployment_service = create_deployment_service_for_test(db.clone());

        // Test rollback
        let result = deployment_service
            .rollback_to_deployment(target_deployment.project_id, target_deployment.id)
            .await?;

        // Verify result - rollback now creates a NEW deployment record
        // The returned deployment ID should be different from the target (it's the new rollback deployment)
        assert_ne!(result.id, target_deployment.id);
        assert!(result.is_current);

        // Verify the new rollback deployment has the correct metadata
        let rollback_dep = deployments::Entity::find_by_id(result.id)
            .one(db.as_ref())
            .await?
            .unwrap();
        let metadata = rollback_dep.metadata.unwrap();
        assert!(metadata.is_rollback);
        assert_eq!(metadata.rolled_back_from_id, Some(target_deployment.id));

        // Verify environment was updated to point to the NEW rollback deployment
        let updated_environment = environments::Entity::find_by_id(environment.id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(updated_environment.current_deployment_id, Some(result.id));

        Ok(())
    }

    #[tokio::test]
    async fn test_rollback_to_deployment_invalid_state() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Setup test data
        let (_project, _environment, mut target_deployment) = setup_test_data(&db).await?;

        // Update the deployment state to "failed" to make it invalid for rollback
        let mut active_deployment: deployments::ActiveModel = target_deployment.into();
        active_deployment.state = Set("failed".to_string());
        target_deployment = active_deployment.update(db.as_ref()).await?;

        let deployment_service = create_deployment_service_for_test(db.clone());

        // Test rollback to invalid deployment state
        let result = deployment_service
            .rollback_to_deployment(target_deployment.project_id, target_deployment.id)
            .await;

        // Verify error is thrown
        assert!(result.is_err());
        match result.unwrap_err() {
            DeploymentError::InvalidDeploymentState(msg) => {
                assert!(msg.contains("failed"));
                assert!(msg.contains("deployed"));
            }
            e => panic!("Expected InvalidDeploymentState error, got: {:?}", e),
        }

        Ok(())
    }

    /// Creates a DeploymentService where image_exists returns false,
    /// simulating a pruned/missing Docker image.
    fn create_deployment_service_with_missing_image(
        db: Arc<temps_database::DbConnection>,
    ) -> DeploymentService {
        let log_service = Arc::new(temps_logs::LogService::new(std::env::temp_dir()));
        let test_db_url = "postgresql://test_user:test_password@localhost:5432/test_db";
        let server_config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:8080".to_string(),
                test_db_url.to_string(),
                None,
                None,
            )
            .expect("Failed to create test server config"),
        );
        let config_service = Arc::new(temps_config::ConfigService::new(server_config, db.clone()));

        let mut queue_service = MockQueueService::new();
        queue_service.expect_send().returning(|_| Ok(()));
        queue_service
            .expect_subscribe()
            .returning(|| Box::new(MockJobReceiver::new()));
        let queue_service: Arc<dyn temps_core::JobQueue> = Arc::new(queue_service);

        let docker = Arc::new(bollard::Docker::connect_with_local_defaults().unwrap());
        let docker_log_service = Arc::new(temps_logs::DockerLogService::new(docker));

        let mut deployer = MockContainerDeployer::new();
        deployer.expect_deploy_container().returning(|_| {
            Ok(temps_deployer::DeployResult {
                container_id: "test-container".to_string(),
                container_name: "test-container".to_string(),
                container_port: 3000,
                host_port: 3000,
                status: temps_deployer::ContainerStatus::Running,
            })
        });
        deployer.expect_stop_container().returning(|_| Ok(()));
        deployer.expect_remove_container().returning(|_| Ok(()));
        deployer.expect_image_exists().returning(|_| Ok(false));
        let deployer: Arc<dyn temps_deployer::ContainerDeployer> = Arc::new(deployer);

        DeploymentService {
            db,
            log_service,
            config_service,
            queue_service,
            docker_log_service,
            deployer,
            encryption_service: create_test_encryption_service(),
        }
    }

    #[tokio::test]
    async fn test_rollback_fails_when_image_missing() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Setup test data (creates a non-static project with a deployed deployment)
        let (_project, _environment, target_deployment) = setup_test_data(&db).await?;

        // Use a service where image_exists returns false
        let deployment_service = create_deployment_service_with_missing_image(db.clone());

        // Attempt rollback — should fail with a clear error before any containers are touched
        let result = deployment_service
            .rollback_to_deployment(target_deployment.project_id, target_deployment.id)
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            DeploymentError::Other(msg) => {
                assert!(
                    msg.contains("no longer exists locally"),
                    "Expected 'no longer exists locally' in error message, got: {}",
                    msg
                );
                assert!(
                    msg.contains("Docker image"),
                    "Expected 'Docker image' in error message, got: {}",
                    msg
                );
            }
            e => panic!(
                "Expected DeploymentError::Other with image-not-found message, got: {:?}",
                e
            ),
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_teardown_deployment() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Setup test data
        let (_project, _environment, deployment) = setup_test_data(&db).await?;

        let deployment_service = create_deployment_service_for_test(db.clone());

        // Test teardown deployment
        deployment_service
            .teardown_deployment(deployment.project_id, deployment.id)
            .await?;

        // Verify deployment state was updated
        let updated_deployment = deployments::Entity::find_by_id(deployment.id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(updated_deployment.state, "stopped");

        Ok(())
    }

    #[tokio::test]
    async fn test_teardown_environment() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Setup test data with multiple deployments
        let (_project, environment, deployment1) = setup_test_data(&db).await?;

        // Create second deployment in same environment
        let deployment2 = deployments::ActiveModel {
            project_id: Set(deployment1.project_id),
            environment_id: Set(environment.id),
            slug: Set("deployment2-456".to_string()),
            state: Set("deployed".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let deployment2 = deployment2.insert(db.as_ref()).await?;

        let deployment_service = create_deployment_service_for_test(db.clone());

        // Test teardown environment
        deployment_service
            .teardown_environment(deployment1.project_id, environment.id)
            .await?;

        // Verify both deployments were stopped
        let updated_deployment1 = deployments::Entity::find_by_id(deployment1.id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(updated_deployment1.state, "stopped");

        let updated_deployment2 = deployments::Entity::find_by_id(deployment2.id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(updated_deployment2.state, "stopped");

        Ok(())
    }

    #[tokio::test]
    async fn test_deployment_not_found() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        let deployment_service = create_deployment_service_for_test(db);

        // Test with non-existent deployment
        let result = deployment_service.pause_deployment(999, 999).await;
        assert!(result.is_err());

        if let Err(DeploymentError::NotFound(_)) = result {
            // Expected error type
        } else {
            panic!("Expected NotFound error");
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_deployment_without_container() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Setup test data
        let (_project, _environment, deployment) = setup_test_data(&db).await?;

        // Note: container_id field no longer exists after workflow refactoring

        let deployment_service = create_deployment_service_for_test(db.clone());

        // Test pause deployment without container - should succeed but not call stop_containers
        deployment_service
            .pause_deployment(deployment.project_id, deployment.id)
            .await?;

        // Verify deployment state was still updated
        let updated_deployment = deployments::Entity::find_by_id(deployment.id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(updated_deployment.state, "paused");

        Ok(())
    }

    #[tokio::test]
    async fn test_deployment_jobs_creation() -> Result<(), Box<dyn std::error::Error>> {
        use crate::services::workflow_planner::WorkflowPlanner;
        use temps_entities::deployment_jobs;

        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let log_service = Arc::new(temps_logs::LogService::new(std::env::temp_dir()));
        // Setup test data
        let (_project, _environment, deployment) = setup_test_data(&db).await?;
        // Create config service
        let server_config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:3000".to_string(),
                "postgresql://test".to_string(),
                None,
                Some("127.0.0.1:8000".to_string()),
            )
            .unwrap(),
        );
        let config_service = Arc::new(temps_config::ConfigService::new(server_config, db.clone()));
        // Create workflow planner
        let dsn_service = Arc::new(temps_error_tracking::DSNService::new(db.clone()));
        let external_service_manager = create_test_external_service_manager(db.clone());
        let workflow_planner = WorkflowPlanner::new(
            db.clone(),
            log_service.clone(),
            external_service_manager.clone(),
            config_service,
            dsn_service,
            create_test_encryption_service(),
        );

        // Create deployment jobs using workflow planner
        let created_jobs = workflow_planner
            .create_deployment_jobs(deployment.id)
            .await?;

        // Verify jobs were created
        assert!(
            !created_jobs.is_empty(),
            "Should have created at least one job"
        );

        // Verify jobs are in database
        let db_jobs = deployment_jobs::Entity::find()
            .filter(deployment_jobs::Column::DeploymentId.eq(deployment.id))
            .all(db.as_ref())
            .await?;

        assert_eq!(
            db_jobs.len(),
            created_jobs.len(),
            "Number of jobs in DB should match created jobs"
        );

        // Verify job properties
        for job in &db_jobs {
            assert_eq!(job.deployment_id, deployment.id);
            assert!(!job.job_id.is_empty(), "Job ID should not be empty");
            assert!(!job.job_type.is_empty(), "Job type should not be empty");
            assert!(!job.name.is_empty(), "Job name should not be empty");
            assert_eq!(job.status, temps_entities::types::JobStatus::Pending);

            // Verify execution order was set
            assert!(
                job.execution_order.is_some(),
                "Execution order should be set"
            );
        }
        // Verify first job is download_repo (for projects with git info)
        let first_job = db_jobs.first().expect("Should have at least one job");
        assert_eq!(first_job.job_id, "download_repo");
        assert_eq!(first_job.job_type, "DownloadRepoJob");

        // Verify job has no dependencies (should be first)
        assert!(
            first_job.dependencies.is_none()
                || first_job
                    .dependencies
                    .as_ref()
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .is_empty(),
            "First job should have no dependencies"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_deployment_jobs_with_log_ids() -> Result<(), Box<dyn std::error::Error>> {
        use crate::services::workflow_planner::WorkflowPlanner;

        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let log_service = Arc::new(temps_logs::LogService::new(std::env::temp_dir()));
        // Setup test data
        let (_project, _environment, deployment) = setup_test_data(&db).await?;

        // Create config service
        let server_config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:3000".to_string(),
                "postgresql://test".to_string(),
                None,
                Some("127.0.0.1:8000".to_string()),
            )
            .unwrap(),
        );
        let config_service = Arc::new(temps_config::ConfigService::new(server_config, db.clone()));

        // Create workflow planner
        let dsn_service = Arc::new(temps_error_tracking::DSNService::new(db.clone()));
        let external_service_manager = create_test_external_service_manager(db.clone());
        let workflow_planner = WorkflowPlanner::new(
            db.clone(),
            log_service.clone(),
            external_service_manager.clone(),
            config_service,
            dsn_service,
            create_test_encryption_service(),
        );

        // Create deployment jobs
        let created_jobs = workflow_planner
            .create_deployment_jobs(deployment.id)
            .await?;

        // Verify each job can be used to generate a log_id
        for job in &created_jobs {
            let log_id = format!("deployment-{}-job-{}", deployment.id, job.job_id);

            // Log IDs should be unique and well-formed
            assert!(!log_id.is_empty());
            assert!(log_id.starts_with(&format!("deployment-{}", deployment.id)));
            assert!(log_id.contains(&job.job_id));

            println!("Job '{}' has log_id: {}", job.name, log_id);
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_list_environment_containers() -> Result<(), Box<dyn std::error::Error>> {
        use temps_entities::deployment_containers;

        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Setup test data
        let (_project, mut environment, deployment) = setup_test_data(&db).await?;

        // Update environment to have current deployment
        let mut active_environment: environments::ActiveModel = environment.into();
        active_environment.current_deployment_id = Set(Some(deployment.id));
        environment = active_environment.update(db.as_ref()).await?;

        // Create deployment_containers entries
        let now = Utc::now();
        let container1 = deployment_containers::ActiveModel {
            deployment_id: Set(deployment.id),
            container_id: Set("container-123".to_string()),
            container_name: Set("test-container-1".to_string()),
            container_port: Set(8080),
            image_name: Set(Some("nginx:latest".to_string())),
            status: Set(Some("running".to_string())),
            created_at: Set(now),
            deployed_at: Set(now),
            ..Default::default()
        };
        container1.insert(db.as_ref()).await?;

        let container2 = deployment_containers::ActiveModel {
            deployment_id: Set(deployment.id),
            container_id: Set("container-456".to_string()),
            container_name: Set("test-container-2".to_string()),
            container_port: Set(5432),
            image_name: Set(Some("postgres:15".to_string())),
            status: Set(Some("running".to_string())),
            created_at: Set(now),
            deployed_at: Set(now),
            ..Default::default()
        };
        container2.insert(db.as_ref()).await?;

        let deployment_service = create_deployment_service_for_test(db.clone());

        // Test list containers
        let containers = deployment_service
            .list_environment_containers(deployment.project_id, environment.id)
            .await?;

        // Verify we got container info (mocked deployer returns container info)
        assert_eq!(containers.len(), 2, "Should return 2 containers");

        Ok(())
    }

    #[tokio::test]
    async fn test_list_environment_containers_no_deployment(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Setup test data without current deployment
        let (project, environment, _deployment) = setup_test_data(&db).await?;

        let deployment_service = create_deployment_service_for_test(db.clone());

        // Test list containers - should return empty for no active deployment
        let containers = deployment_service
            .list_environment_containers(project.id, environment.id)
            .await?;

        assert_eq!(
            containers.len(),
            0,
            "Should return no containers when no active deployment"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_get_container_logs_by_id_validation() -> Result<(), Box<dyn std::error::Error>> {
        use temps_entities::deployment_containers;

        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Setup test data
        let (_project, mut environment, deployment) = setup_test_data(&db).await?;

        // Update environment to have current deployment
        let mut active_environment: environments::ActiveModel = environment.into();
        active_environment.current_deployment_id = Set(Some(deployment.id));
        environment = active_environment.update(db.as_ref()).await?;

        // Create a container for the deployment
        let now = Utc::now();
        let container = deployment_containers::ActiveModel {
            deployment_id: Set(deployment.id),
            container_id: Set("valid-container-id".to_string()),
            container_name: Set("test-container".to_string()),
            container_port: Set(8080),
            image_name: Set(Some("nginx:latest".to_string())),
            status: Set(Some("running".to_string())),
            created_at: Set(now),
            deployed_at: Set(now),
            ..Default::default()
        };
        container.insert(db.as_ref()).await?;

        let deployment_service = create_deployment_service_for_test(db.clone());

        // Test with invalid container ID - should fail
        let result = deployment_service
            .get_container_logs_by_id(
                deployment.project_id,
                environment.id,
                "invalid-container-id".to_string(),
                ContainerLogParams {
                    start_date: None,
                    end_date: None,
                    tail: None,
                    timestamps: false,
                    follow: false,
                },
            )
            .await;

        assert!(result.is_err(), "Should fail with invalid container ID");
        match result {
            Err(DeploymentError::NotFound(msg)) => {
                assert!(
                    msg.contains("Container"),
                    "Error should mention container not found"
                );
            }
            _ => panic!("Expected NotFound error"),
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_list_containers_not_server_project() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Create a non-server project (static site)
        let project = projects::ActiveModel {
            name: Set("Static Site".to_string()),
            slug: Set("static-site".to_string()),
            repo_name: Set("static-site-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            preset: Set(Preset::Static), // Static preset doesn't require a server
            main_branch: Set("main".to_string()),
            directory: Set("/".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await?;

        // Create environment
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Test".to_string()),
            slug: Set("test".to_string()),
            host: Set("test.example.com".to_string()),
            upstreams: Set(UpstreamList::default()),
            subdomain: Set("test.example.com".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let environment = environment.insert(db.as_ref()).await?;

        let deployment_service = create_deployment_service_for_test(db.clone());

        // Test list containers on non-server project - should fail
        let result = deployment_service
            .list_environment_containers(project.id, environment.id)
            .await;

        assert!(result.is_err(), "Should fail for non-server projects");
        match result {
            Err(DeploymentError::Other(msg)) => {
                assert!(
                    msg.contains("server-type"),
                    "Error should mention server-type projects"
                );
            }
            _ => panic!("Expected Other error"),
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_get_container_detail_success() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Setup: Create project, environment, deployment, and container
        let (project, environment, _deployment, container) = setup_test_deployment(&db).await?;

        let deployment_service = create_deployment_service_for_test(db.clone());

        // Test: Get container detail
        let (result_container, result_env) = deployment_service
            .get_container_detail(project.id, environment.id, container.container_id.clone())
            .await?;

        // Verify container details
        assert_eq!(result_container.id, container.id);
        assert_eq!(result_container.container_id, "container-123");
        assert_eq!(result_container.container_name, "test-container-1");
        assert_eq!(result_container.status, Some("running".to_string()));

        // Verify environment info
        assert_eq!(result_env.id, environment.id);
        assert_eq!(result_env.name, environment.name);

        Ok(())
    }

    #[tokio::test]
    async fn test_get_container_detail_not_found() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Setup: Create project and environment (no container)
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            preset: Set(Preset::NextJs),
            main_branch: Set("main".to_string()),
            directory: Set("/".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await?;

        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Production".to_string()),
            slug: Set("prod".to_string()),
            host: Set("prod.example.com".to_string()),
            upstreams: Set(UpstreamList::default()),
            subdomain: Set("prod.example.com".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let environment = environment.insert(db.as_ref()).await?;

        let deployment_service = create_deployment_service_for_test(db.clone());

        // Test: Try to get non-existent container
        let result = deployment_service
            .get_container_detail(project.id, environment.id, "non-existent".to_string())
            .await;

        assert!(result.is_err(), "Should fail when container not found");
        match result {
            Err(DeploymentError::NotFound(msg)) => {
                assert!(msg.contains("Container"));
            }
            _ => panic!("Expected NotFound error"),
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_stop_container_success() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Setup
        let (project, environment, _deployment, container) = setup_test_deployment(&db).await?;

        let deployment_service = create_deployment_service_for_test(db.clone());

        // Test: Stop container
        deployment_service
            .stop_container(project.id, environment.id, container.container_id.clone())
            .await?;

        // Verify: Check that container status is updated in database
        let updated_container = deployment_containers::Entity::find_by_id(container.id)
            .one(db.as_ref())
            .await?
            .expect("Container should exist");

        assert_eq!(updated_container.status, Some("stopped".to_string()));

        Ok(())
    }

    #[tokio::test]
    async fn test_start_container_success() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Setup
        let (project, environment, _deployment, mut container) = setup_test_deployment(&db).await?;

        // Set container status to stopped
        let mut active_container: deployment_containers::ActiveModel = container.into();
        active_container.status = Set(Some("stopped".to_string()));
        container = active_container.update(db.as_ref()).await?;

        let deployment_service = create_deployment_service_for_test(db.clone());

        // Test: Start container
        deployment_service
            .start_container(project.id, environment.id, container.container_id.clone())
            .await?;

        // Verify: Check that container status is updated to running
        let updated_container = deployment_containers::Entity::find_by_id(container.id)
            .one(db.as_ref())
            .await?
            .expect("Container should exist");

        assert_eq!(updated_container.status, Some("running".to_string()));

        Ok(())
    }

    #[tokio::test]
    async fn test_restart_container_success() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Setup
        let (project, environment, _deployment, container) = setup_test_deployment(&db).await?;

        let deployment_service = create_deployment_service_for_test(db.clone());

        // Test: Restart container (stop + start)
        deployment_service
            .restart_container(project.id, environment.id, container.container_id.clone())
            .await?;

        // Verify: Container should be running after restart
        let updated_container = deployment_containers::Entity::find_by_id(container.id)
            .one(db.as_ref())
            .await?
            .expect("Container should exist");

        assert_eq!(updated_container.status, Some("running".to_string()));

        Ok(())
    }

    #[tokio::test]
    async fn test_get_container_env_variables() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Setup
        let (project, environment, _deployment, container) = setup_test_deployment(&db).await?;

        let deployment_service = create_deployment_service_for_test(db.clone());

        // Test: Get container environment variables
        let env_vars = deployment_service
            .get_container_env_variables(project.id, environment.id, container.container_id.clone())
            .await?;

        // The mock returns empty HashMap, so we should get empty vec
        assert_eq!(env_vars.len(), 0);

        Ok(())
    }

    #[tokio::test]
    async fn test_stop_all_containers_success() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Setup
        let (project, environment, deployment, _container) = setup_test_deployment(&db).await?;

        // Create second container
        let now = Utc::now();
        let container2 = deployment_containers::ActiveModel {
            deployment_id: Set(deployment.id),
            container_id: Set("container-789".to_string()),
            container_name: Set("test-container-3".to_string()),
            container_port: Set(9090),
            image_name: Set(Some("redis:latest".to_string())),
            status: Set(Some("running".to_string())),
            created_at: Set(now),
            deployed_at: Set(now),
            ..Default::default()
        };
        container2.insert(db.as_ref()).await?;

        let deployment_service = create_deployment_service_for_test(db.clone());

        // Test: Stop all containers
        deployment_service
            .stop_all_containers(project.id, environment.id)
            .await?;

        // Verify: Both containers should be stopped
        let containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment.id))
            .all(db.as_ref())
            .await?;

        for container in containers {
            assert_eq!(container.status, Some("stopped".to_string()));
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_start_all_containers_success() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Setup
        let (project, environment, deployment, container1) = setup_test_deployment(&db).await?;

        // Set all containers to stopped
        let mut active_container: deployment_containers::ActiveModel = container1.into();
        active_container.status = Set(Some("stopped".to_string()));
        active_container.update(db.as_ref()).await?;

        let deployment_service = create_deployment_service_for_test(db.clone());

        // Test: Start all containers
        deployment_service
            .start_all_containers(project.id, environment.id)
            .await?;

        // Verify: All containers should be running
        let containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment.id))
            .all(db.as_ref())
            .await?;

        for container in containers {
            assert_eq!(container.status, Some("running".to_string()));
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_restart_all_containers_success() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Setup (this creates 1 container)
        let (project, environment, deployment, _container) = setup_test_deployment(&db).await?;

        // Create multiple additional containers (3 more, total 4)
        let now = Utc::now();
        for i in 1..=3 {
            let container = deployment_containers::ActiveModel {
                deployment_id: Set(deployment.id),
                container_id: Set(format!("container-{}", i * 100)),
                container_name: Set(format!("test-container-{}", i)),
                container_port: Set(8000 + i),
                image_name: Set(Some("nginx:latest".to_string())),
                status: Set(Some("running".to_string())),
                created_at: Set(now),
                deployed_at: Set(now),
                ..Default::default()
            };
            container.insert(db.as_ref()).await?;
        }

        let deployment_service = create_deployment_service_for_test(db.clone());

        // Test: Restart all containers
        deployment_service
            .restart_all_containers(project.id, environment.id)
            .await?;

        // Verify: All containers should still be running (1 from setup + 3 created = 4 total)
        let containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment.id))
            .all(db.as_ref())
            .await?;

        assert_eq!(
            containers.len(),
            4,
            "Should have 4 containers (1 from setup + 3 created)"
        );
        for container in containers {
            assert_eq!(container.status, Some("running".to_string()));
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_container_operations_wrong_environment() -> Result<(), Box<dyn std::error::Error>>
    {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Setup: Create two environments
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            preset: Set(Preset::NextJs),
            main_branch: Set("main".to_string()),
            directory: Set("/".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await?;

        let env1 = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Environment 1".to_string()),
            slug: Set("env1".to_string()),
            host: Set("env1.example.com".to_string()),
            upstreams: Set(UpstreamList::default()),
            subdomain: Set("env1.example.com".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let env1 = env1.insert(db.as_ref()).await?;

        let env2 = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Environment 2".to_string()),
            slug: Set("env2".to_string()),
            host: Set("env2.example.com".to_string()),
            upstreams: Set(UpstreamList::default()),
            subdomain: Set("env2.example.com".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let env2 = env2.insert(db.as_ref()).await?;

        // Create deployment and container in env1
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(env1.id),
            state: Set("deployed".to_string()),
            slug: Set("test-deployment".to_string()),
            metadata: Set(Some(Default::default())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let deployment = deployment.insert(db.as_ref()).await?;

        let now = Utc::now();
        let container = deployment_containers::ActiveModel {
            deployment_id: Set(deployment.id),
            container_id: Set("container-123".to_string()),
            container_name: Set("test-container".to_string()),
            container_port: Set(8080),
            image_name: Set(Some("nginx:latest".to_string())),
            status: Set(Some("running".to_string())),
            created_at: Set(now),
            deployed_at: Set(now),
            ..Default::default()
        };
        container.insert(db.as_ref()).await?;

        let deployment_service = create_deployment_service_for_test(db.clone());

        // Test: Try to operate on container from wrong environment
        let result = deployment_service
            .get_container_detail(project.id, env2.id, "container-123".to_string())
            .await;

        assert!(
            result.is_err(),
            "Should fail when environment doesn't match"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_rollback_to_multiple_deployments_with_deleted_containers(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Setup: Create project and environment
        let project = projects::ActiveModel {
            name: Set("Multi-Deploy Project".to_string()),
            slug: Set("multi-deploy".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            preset: Set(Preset::NextJs),
            main_branch: Set("main".to_string()),
            directory: Set("/".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await?;

        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Production".to_string()),
            slug: Set("prod".to_string()),
            host: Set("prod.example.com".to_string()),
            upstreams: Set(UpstreamList::default()),
            subdomain: Set("prod.example.com".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let environment = environment.insert(db.as_ref()).await?;

        // Create 3 deployments
        let deployment1 = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            state: Set("deployed".to_string()),
            slug: Set("deployment-1".to_string()),
            image_name: Set(Some("app:v1".to_string())),
            metadata: Set(Some(Default::default())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let deployment1 = deployment1.insert(db.as_ref()).await?;

        let deployment2 = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            state: Set("deployed".to_string()),
            slug: Set("deployment-2".to_string()),
            image_name: Set(Some("app:v2".to_string())),
            metadata: Set(Some(Default::default())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let deployment2 = deployment2.insert(db.as_ref()).await?;

        let deployment3 = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            state: Set("deployed".to_string()),
            slug: Set("deployment-3".to_string()),
            image_name: Set(Some("app:v3".to_string())),
            metadata: Set(Some(Default::default())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let deployment3 = deployment3.insert(db.as_ref()).await?;

        // Create containers for each deployment
        let now = Utc::now();

        let container1 = deployment_containers::ActiveModel {
            deployment_id: Set(deployment1.id),
            container_id: Set("container-v1".to_string()),
            container_name: Set("app-container-v1".to_string()),
            container_port: Set(8080),
            image_name: Set(Some("app:v1".to_string())),
            status: Set(Some("running".to_string())),
            created_at: Set(now),
            deployed_at: Set(now),
            ..Default::default()
        };
        container1.insert(db.as_ref()).await?;

        let container2 = deployment_containers::ActiveModel {
            deployment_id: Set(deployment2.id),
            container_id: Set("container-v2".to_string()),
            container_name: Set("app-container-v2".to_string()),
            container_port: Set(8080),
            image_name: Set(Some("app:v2".to_string())),
            status: Set(Some("running".to_string())),
            created_at: Set(now),
            deployed_at: Set(now),
            ..Default::default()
        };
        container2.insert(db.as_ref()).await?;

        let container3 = deployment_containers::ActiveModel {
            deployment_id: Set(deployment3.id),
            container_id: Set("container-v3".to_string()),
            container_name: Set("app-container-v3".to_string()),
            container_port: Set(8080),
            image_name: Set(Some("app:v3".to_string())),
            status: Set(Some("running".to_string())),
            created_at: Set(now),
            deployed_at: Set(now),
            ..Default::default()
        };
        container3.insert(db.as_ref()).await?;

        // Set current deployment to deployment3
        let mut active_environment: environments::ActiveModel = environment.into();
        active_environment.current_deployment_id = Set(Some(deployment3.id));
        let environment = active_environment.update(db.as_ref()).await?;

        let deployment_service = create_deployment_service_for_test(db.clone());

        // Test 1: Rollback to deployment2
        // Rollback now creates a NEW deployment record with is_rollback metadata
        println!("Test 1: Rolling back to deployment 2");
        let rollback1 = deployment_service
            .rollback_to_deployment(project.id, deployment2.id)
            .await?;

        // Verify the new rollback deployment is now current (not the original deployment2)
        let updated_env = environments::Entity::find_by_id(environment.id)
            .one(db.as_ref())
            .await?
            .expect("Environment should exist");
        assert_eq!(updated_env.current_deployment_id, Some(rollback1.id));
        // Verify rollback metadata points to the original deployment
        let rollback1_dep = deployments::Entity::find_by_id(rollback1.id)
            .one(db.as_ref())
            .await?
            .unwrap();
        let meta1 = rollback1_dep.metadata.unwrap();
        assert!(meta1.is_rollback);
        assert_eq!(meta1.rolled_back_from_id, Some(deployment2.id));

        // Test 2: Rollback to deployment1 (containers redeployed)
        println!("Test 2: Rolling back to deployment 1 (containers redeployed)");
        let rollback2 = deployment_service
            .rollback_to_deployment(project.id, deployment1.id)
            .await?;

        // Verify the new rollback deployment is now current
        let updated_env = environments::Entity::find_by_id(environment.id)
            .one(db.as_ref())
            .await?
            .expect("Environment should exist");
        assert_eq!(updated_env.current_deployment_id, Some(rollback2.id));
        let rollback2_dep = deployments::Entity::find_by_id(rollback2.id)
            .one(db.as_ref())
            .await?
            .unwrap();
        let meta2 = rollback2_dep.metadata.unwrap();
        assert!(meta2.is_rollback);
        assert_eq!(meta2.rolled_back_from_id, Some(deployment1.id));

        // Test 3: Verify rollback chain (3 -> 2 -> 1)
        println!("Test 3: Full rollback chain (3 -> 2 -> 1)");

        let rollback3 = deployment_service
            .rollback_to_deployment(project.id, deployment3.id)
            .await?;
        let updated_env = environments::Entity::find_by_id(environment.id)
            .one(db.as_ref())
            .await?
            .expect("Environment should exist");
        assert_eq!(updated_env.current_deployment_id, Some(rollback3.id));

        let rollback4 = deployment_service
            .rollback_to_deployment(project.id, deployment2.id)
            .await?;
        let updated_env = environments::Entity::find_by_id(environment.id)
            .one(db.as_ref())
            .await?
            .expect("Environment should exist");
        assert_eq!(updated_env.current_deployment_id, Some(rollback4.id));

        let rollback5 = deployment_service
            .rollback_to_deployment(project.id, deployment1.id)
            .await?;
        let updated_env = environments::Entity::find_by_id(environment.id)
            .one(db.as_ref())
            .await?
            .expect("Environment should exist");
        assert_eq!(updated_env.current_deployment_id, Some(rollback5.id));

        println!("All rollback tests passed!");
        Ok(())
    }

    // Helper function to setup a test deployment with a container
    async fn setup_test_deployment(
        db: &Arc<temps_database::DbConnection>,
    ) -> Result<
        (
            projects::Model,
            environments::Model,
            deployments::Model,
            deployment_containers::Model,
        ),
        Box<dyn std::error::Error>,
    > {
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            preset: Set(Preset::NextJs),
            main_branch: Set("main".to_string()),
            directory: Set("/".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await?;

        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Production".to_string()),
            slug: Set("prod".to_string()),
            host: Set("prod.example.com".to_string()),
            upstreams: Set(UpstreamList::default()),
            subdomain: Set("prod.example.com".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let environment = environment.insert(db.as_ref()).await?;

        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            state: Set("deployed".to_string()),
            slug: Set("test-deployment".to_string()),
            metadata: Set(Some(Default::default())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let deployment = deployment.insert(db.as_ref()).await?;

        let now = Utc::now();
        let container = deployment_containers::ActiveModel {
            deployment_id: Set(deployment.id),
            container_id: Set("container-123".to_string()),
            container_name: Set("test-container-1".to_string()),
            container_port: Set(8080),
            image_name: Set(Some("nginx:latest".to_string())),
            status: Set(Some("running".to_string())),
            created_at: Set(now),
            deployed_at: Set(now),
            ..Default::default()
        };
        let container = container.insert(db.as_ref()).await?;

        Ok((project, environment, deployment, container))
    }

    /// Insert a captured-log metadata row and write its backing file under the
    /// service's log base path so the service can read it back. Returns the row.
    async fn seed_captured_log(
        db: &Arc<temps_database::DbConnection>,
        log_base: &std::path::Path,
        deployment: &deployments::Model,
        container_name: &str,
        content: &str,
    ) -> Result<deployment_container_logs::Model, Box<dyn std::error::Error>> {
        let row = deployment_container_logs::ActiveModel {
            deployment_id: Set(deployment.id),
            project_id: Set(deployment.project_id),
            environment_id: Set(deployment.environment_id),
            container_id: Set(format!("cid-{}", container_name)),
            container_name: Set(container_name.to_string()),
            service_name: Set(None),
            node_id: Set(None),
            log_path: Set(String::new()), // filled in after we know the row id
            size_bytes: Set(content.len() as i64),
            truncated: Set(false),
            ..Default::default()
        };
        let row = row.insert(db.as_ref()).await?;

        // Mirror the path scheme used by capture_container_logs.
        let log_path = format!("deployment-container-logs/{}/{}.log", deployment.id, row.id);
        let full_path = log_base.join(&log_path);
        if let Some(parent) = full_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&full_path, content.as_bytes()).await?;

        let mut active: deployment_container_logs::ActiveModel = row.into();
        active.log_path = Set(log_path);
        let row = active.update(db.as_ref()).await?;
        Ok(row)
    }

    #[tokio::test]
    async fn test_list_deployment_container_logs_returns_captured_rows(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let (_project, _environment, deployment) = setup_test_data(&db).await?;

        let log_base = std::env::temp_dir();
        seed_captured_log(&db, &log_base, &deployment, "web-1", "old logs").await?;
        seed_captured_log(&db, &log_base, &deployment, "web-2", "newer logs").await?;

        let service = create_deployment_service_for_test(db.clone());
        let logs = service
            .list_deployment_container_logs(deployment.project_id, deployment.id)
            .await?;

        assert_eq!(logs.len(), 2);
        // All captured logs belong to the requested deployment.
        assert!(logs.iter().all(|l| l.deployment_id == deployment.id));
        Ok(())
    }

    #[tokio::test]
    async fn test_list_deployment_container_logs_wrong_project_not_found(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let (_project, _environment, deployment) = setup_test_data(&db).await?;

        let service = create_deployment_service_for_test(db.clone());
        // A different project id must not see this deployment's logs — IDOR guard.
        let result = service
            .list_deployment_container_logs(deployment.project_id + 999, deployment.id)
            .await;

        assert!(matches!(result, Err(DeploymentError::NotFound(_))));
        Ok(())
    }

    #[tokio::test]
    async fn test_get_deployment_container_log_content_reads_file(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let (_project, _environment, deployment) = setup_test_data(&db).await?;

        let log_base = std::env::temp_dir();
        let row =
            seed_captured_log(&db, &log_base, &deployment, "web-2", "hello from web-2").await?;

        let service = create_deployment_service_for_test(db.clone());
        let (got_row, content) = service
            .get_deployment_container_log_content(deployment.project_id, deployment.id, row.id)
            .await?;

        assert_eq!(got_row.id, row.id);
        assert_eq!(content, "hello from web-2");
        Ok(())
    }

    #[tokio::test]
    async fn test_get_deployment_container_log_content_wrong_project_not_found(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let (_project, _environment, deployment) = setup_test_data(&db).await?;

        let log_base = std::env::temp_dir();
        let row = seed_captured_log(&db, &log_base, &deployment, "web-2", "secret").await?;

        let service = create_deployment_service_for_test(db.clone());
        // Reading with a foreign project id must be denied even with the right log id.
        let result = service
            .get_deployment_container_log_content(
                deployment.project_id + 999,
                deployment.id,
                row.id,
            )
            .await;

        assert!(matches!(result, Err(DeploymentError::NotFound(_))));
        Ok(())
    }

    #[test]
    fn resolve_resource_usage_is_opt_in_with_env_over_project() {
        let cfg = |cpu: Option<i32>, mem: Option<i32>| DeploymentConfig {
            cpu_limit: cpu,
            memory_limit: mem,
            ..Default::default()
        };

        // 1. Nothing configured anywhere -> fully uncapped (the default).
        let none = DeploymentService::resolve_resource_usage(None, None);
        assert_eq!(none.cpu_limit, None);
        assert_eq!(none.memory_limit, None);

        // 2. Project sets a limit, env doesn't -> inherit project (microcores → `u`, MB → `Mi`).
        let proj = cfg(Some(2_000_000), Some(512));
        let inherited = DeploymentService::resolve_resource_usage(None, Some(&proj));
        assert_eq!(inherited.cpu_limit.as_deref(), Some("2000000u"));
        assert_eq!(inherited.memory_limit.as_deref(), Some("512Mi"));

        // 3. Env overrides project per-field (env cpu wins, project memory inherited).
        let env = cfg(Some(500_000), None);
        let overridden = DeploymentService::resolve_resource_usage(Some(&env), Some(&proj));
        assert_eq!(overridden.cpu_limit.as_deref(), Some("500000u"));
        assert_eq!(overridden.memory_limit.as_deref(), Some("512Mi"));

        // 4. Env config present but with all-None limits -> still uncapped
        //    (an environment existing must NOT imply a limit).
        let empty_env = cfg(None, None);
        let still_none =
            DeploymentService::resolve_resource_usage(Some(&empty_env), Some(&cfg(None, None)));
        assert_eq!(still_none.cpu_limit, None);
        assert_eq!(still_none.memory_limit, None);
    }
}
