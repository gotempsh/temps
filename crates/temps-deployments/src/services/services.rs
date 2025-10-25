use futures::Stream;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, Set,
};
use std::collections::HashMap;
use std::sync::Arc;
use temps_entities::{
    deployment_containers, deployment_domains, deployments, environments, projects,
};
use thiserror::Error;
use tracing::{debug, error, info, warn};

use crate::services::types::{
    Deployment, DeploymentDomain, DeploymentEnvironment, DeploymentListResponse,
};
use crate::UpdateDeploymentSettingsRequest;

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

    #[error("Pipeline error: {0}")]
    PipelineError(String),

    #[error("Deployment error: {0}")]
    DeploymentError(String),

    #[error("Queue error: {0}")]
    QueueError(String),

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
}

impl DeploymentService {
    pub fn new(
        db: Arc<temps_database::DbConnection>,
        log_service: Arc<temps_logs::LogService>,
        config_service: Arc<temps_config::ConfigService>,
        queue_service: Arc<dyn temps_core::JobQueue>,
        docker_log_service: Arc<temps_logs::DockerLogService>,
        deployer: Arc<dyn temps_deployer::ContainerDeployer>,
    ) -> Self {
        DeploymentService {
            db,
            log_service,
            config_service,
            queue_service,
            docker_log_service,
            deployer,
        }
    }
    pub async fn get_filtered_container_logs(
        &self,
        project_id: i32,
        environment_id: i32,
        start_date: Option<i64>,
        end_date: Option<i64>,
        tail: Option<String>,
        container_name: Option<String>,
        timestamps: bool,
    ) -> Result<impl Stream<Item = Result<String, std::io::Error>>, DeploymentError> {
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
        let stream_result = self
            .docker_log_service
            .get_container_logs(
                &container_id,
                temps_logs::docker_logs::ContainerLogOptions {
                    start_date: start_date.map(|ts| {
                        chrono::DateTime::from_timestamp(ts, 0).unwrap_or_else(chrono::Utc::now)
                    }),
                    end_date: end_date.map(|ts| {
                        chrono::DateTime::from_timestamp(ts, 0).unwrap_or_else(chrono::Utc::now)
                    }),
                    tail,
                    timestamps,
                },
            )
            .await
            .map_err(|e| DeploymentError::Other(e.to_string()))?;

        // Map ContainerError to std::io::Error to maintain API compatibility
        let mapped_stream = futures_util::stream::StreamExt::map(stream_result, |item| {
            item.map_err(|container_err| std::io::Error::other(container_err.to_string()))
        });

        Ok(mapped_stream)
    }

    /// Get logs for a specific container by container ID
    pub async fn get_container_logs_by_id(
        &self,
        project_id: i32,
        environment_id: i32,
        container_id: String,
        start_date: Option<i64>,
        end_date: Option<i64>,
        tail: Option<String>,
        timestamps: bool,
    ) -> Result<impl Stream<Item = Result<String, std::io::Error>>, DeploymentError> {
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

        if environment.current_deployment_id.is_none() {
            return Err(DeploymentError::NotFound(
                "No active deployment found".to_string(),
            ));
        }

        let deployment_id = environment.current_deployment_id.unwrap();

        // Verify the container belongs to this deployment
        let _container = deployment_containers::Entity::find()
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

        // Get logs from the Docker log service
        let stream_result = self
            .docker_log_service
            .get_container_logs(
                &container_id,
                temps_logs::docker_logs::ContainerLogOptions {
                    start_date: start_date.map(|ts| {
                        chrono::DateTime::from_timestamp(ts, 0).unwrap_or_else(chrono::Utc::now)
                    }),
                    end_date: end_date.map(|ts| {
                        chrono::DateTime::from_timestamp(ts, 0).unwrap_or_else(chrono::Utc::now)
                    }),
                    tail,
                    timestamps,
                },
            )
            .await
            .map_err(|e| DeploymentError::Other(e.to_string()))?;

        // Map ContainerError to std::io::Error to maintain API compatibility
        let mapped_stream = futures_util::stream::StreamExt::map(stream_result, |item| {
            item.map_err(|container_err| std::io::Error::other(container_err.to_string()))
        });

        Ok(mapped_stream)
    }

    /// List all containers for a specific environment
    pub async fn list_environment_containers(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<Vec<temps_deployer::ContainerInfo>, DeploymentError> {
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

        if environment.current_deployment_id.is_none() {
            return Ok(Vec::new()); // No active deployment, no containers
        }

        let deployment_id = environment.current_deployment_id.unwrap();

        // Get all containers for this deployment from the database
        let db_containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        if db_containers.is_empty() {
            return Ok(Vec::new());
        }

        // Get container info from the deployer for each container
        let mut container_infos = Vec::new();
        for db_container in db_containers {
            match self
                .deployer
                .get_container_info(&db_container.container_id)
                .await
            {
                Ok(info) => container_infos.push(info),
                Err(e) => {
                    warn!(
                        "Failed to get info for container {}: {}",
                        db_container.container_id, e
                    );
                    // Continue with other containers
                }
            }
        }

        Ok(container_infos)
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

        // Fetch environment with domains
        let environments_with_domains = self
            .get_environments_with_domains(&[deployment.environment_id])
            .await?;
        let environment = environments_with_domains
            .get(&deployment.environment_id)
            .cloned();

        Ok(self
            .map_db_deployment_to_deployment(deployment, false, environment)
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

        if project.is_none() {
            return Err(DeploymentError::NotFound(format!(
                "project {} not found",
                project_id
            )));
        }
        info!("Project found: {:?}", project);

        info!(
            "Before invoking pipeline service project_id: {}, environment_id: {}",
            project_id, environment_id
        );
        // Check if repo_owner and repo_name are present
        let repo_owner = project.as_ref().unwrap().repo_owner.clone();
        let repo_name = project.as_ref().unwrap().repo_name.clone();

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

    pub async fn rollback_to_deployment(
        &self,
        project_id: i32,
        deployment_id: i32,
    ) -> Result<Deployment, DeploymentError> {
        // Fetch the target deployment
        let target_deployment = deployments::Entity::find_by_id(deployment_id)
            .filter(deployments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::Other("Target deployment not found".to_string()))?;

        let environment_id = target_deployment.environment_id;

        info!(
            "Initiating container-based rollback for project_id: {}, deployment_id: {}, environment_id: {}",
            project_id, deployment_id, environment_id
        );

        // Find the current active deployment for this environment
        let environment = environments::Entity::find_by_id(environment_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Environment not found".to_string()))?;

        // Stop current deployment's containers if any
        if let Some(current_deployment_id) = environment.current_deployment_id {
            if current_deployment_id != deployment_id {
                let current_containers = deployment_containers::Entity::find()
                    .filter(deployment_containers::Column::DeploymentId.eq(current_deployment_id))
                    .filter(deployment_containers::Column::DeletedAt.is_null())
                    .all(self.db.as_ref())
                    .await?;

                for container in current_containers {
                    self.deployer
                        .stop_container(&container.container_id)
                        .await
                        .map_err(|e| {
                            DeploymentError::Other(format!(
                                "Failed to stop current container: {}",
                                e
                            ))
                        })?;
                }
            }
        }

        // Launch the target deployment containers
        let target_containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        for container in target_containers {
            self.deployer
                .start_container(&container.container_id)
                .await
                .map_err(|e| {
                    DeploymentError::Other(format!("Failed to start target container: {}", e))
                })?;
        }

        // Update the environment to point to the target deployment
        let mut active_env: environments::ActiveModel = environment.into();
        active_env.current_deployment_id = Set(Some(deployment_id));
        active_env.update(self.db.as_ref()).await?;

        info!("Rollback completed successfully");

        Ok(self
            .map_db_deployment_to_deployment(target_deployment, true, None)
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
                self.deployer
                    .stop_container(&container.container_id)
                    .await
                    .map_err(|e| {
                        DeploymentError::Other(format!(
                            "Failed to stop container {}: {}",
                            container.container_id, e
                        ))
                    })?;

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

        // Pause all containers for this deployment
        let containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        for container in containers {
            self.deployer
                .pause_container(&container.container_id)
                .await
                .map_err(|e| DeploymentError::Other(format!("Failed to pause container: {}", e)))?;

            // Update container status
            let mut active_container: deployment_containers::ActiveModel = container.into();
            active_container.status = Set(Some("paused".to_string()));
            active_container.update(self.db.as_ref()).await?;
        }

        // Update deployment state to "paused"
        let mut active_deployment: deployments::ActiveModel = deployment.into();
        active_deployment.state = Set("paused".to_string());
        active_deployment.update(self.db.as_ref()).await?;

        info!("Successfully paused deployment: {}", deployment_id);
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
        for (env, project) in environments {
            let mut domains = domains_by_env.remove(&env.id).unwrap_or_default();

            // Compute the environment URL using project slug and environment slug
            let project_slug = project
                .as_ref()
                .map(|p| p.slug.as_str())
                .unwrap_or("unknown");
            let env_url = self
                .compute_environment_url(project_slug, &env.slug)
                .await
                .unwrap_or_else(|_| format!("http://{}-{}.localhost", project_slug, env.slug));
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
        let protocol = if let Some(ref url) = settings.external_url {
            if let Ok(parsed_url) = url::Url::parse(url) {
                match parsed_url.scheme() {
                    "https" => "https",
                    "http" => "http",
                    _ => "http",
                }
            } else {
                "http"
            }
        } else {
            "http"
        };

        Ok(format!("{}://{}", protocol, domain))
    }

    async fn compute_environment_url(
        &self,
        project_slug: &str,
        environment_slug: &str,
    ) -> anyhow::Result<String> {
        let settings = self.config_service.get_settings().await.unwrap_or_default();

        let base_domain = settings.preview_domain;

        // Determine protocol - use https if external_url is configured, otherwise http
        let protocol = if let Some(ref url) = settings.external_url {
            if let Ok(parsed_url) = url::Url::parse(url) {
                match parsed_url.scheme() {
                    "https" => "https",
                    "http" => "http",
                    _ => "http",
                }
            } else {
                "http"
            }
        } else {
            "http"
        };
        // New format: <protocol>://<project_slug>-<env_slug>.<preview_domain>
        Ok(format!(
            "{}://{}-{}.{}",
            protocol, project_slug, environment_slug, base_domain
        ))
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
            .and_then(|rc| rc.commit.committer.map(|c| c.date))
            .map(|date| date.unwrap());

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

        // Update deployment to cancelled state
        let mut active_deployment: deployments::ActiveModel = deployment.into();
        active_deployment.state = Set("cancelled".to_string());
        active_deployment.cancelled_reason = Set(Some("Cancelled by user".to_string()));
        active_deployment.finished_at = Set(Some(chrono::Utc::now()));
        active_deployment.updated_at = Set(chrono::Utc::now());
        active_deployment.update(self.db.as_ref()).await?;

        info!(
            "Successfully cancelled deployment {} for project {} - workflow will stop at next checkpoint",
            deployment_id, project_id
        );

        Ok(())
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
            async fn list_containers(&self) -> Result<Vec<temps_deployer::ContainerInfo>, temps_deployer::DeployerError>;
            async fn get_container_logs(&self, container_id: &str) -> Result<String, temps_deployer::DeployerError>;
            async fn stream_container_logs(&self, container_id: &str) -> Result<Box<dyn futures::Stream<Item = String> + Unpin + Send>, temps_deployer::DeployerError>;
        }
    }
    fn create_test_external_service_manager(
        db: Arc<temps_database::DbConnection>,
    ) -> Arc<temps_providers::ExternalServiceManager> {
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let docker = Arc::new(bollard::Docker::connect_with_local_defaults().ok().unwrap());
        Arc::new(temps_providers::ExternalServiceManager::new(
            db,
            encryption_service,
            docker,
        ))
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
            metadata: Set(None),
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

        // Create mock queue service
        let mut queue_service = MockQueueService::new();
        queue_service.expect_send().returning(|_| Ok(()));
        queue_service.expect_subscribe().returning(|| {
            // Return a simple mock receiver
            Box::new(MockJobReceiver::new())
        });
        let queue_service: Arc<dyn temps_core::JobQueue> = Arc::new(queue_service);

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
            })
        });
        deployer.expect_list_containers().returning(|| Ok(vec![]));
        deployer.expect_stream_container_logs().returning(|_| {
            use futures::stream;
            let stream = stream::empty();
            Ok(Box::new(stream))
        });
        let deployer: Arc<dyn temps_deployer::ContainerDeployer> = Arc::new(deployer);

        // For tests, we'll create a service that directly accepts the trait
        DeploymentService {
            db,
            log_service,
            config_service,
            queue_service,
            docker_log_service,
            deployer,
        }
    }

    // Note: test_deployment_to_container_launch_spec has been removed
    // The deployment_to_container_launch_spec method no longer exists as deployments
    // are now handled through the workflow system
    #[tokio::test]
    #[ignore] // Temporarily ignored - method no longer exists after workflow refactoring
    async fn test_deployment_to_container_launch_spec_removed(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // This test has been removed because deployment_to_container_launch_spec
        // no longer exists after the workflow system refactoring
        Ok(())
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

        // Create current deployment that will be stopped
        let current_deployment = deployments::ActiveModel {
            project_id: Set(target_deployment.project_id),
            environment_id: Set(environment.id),
            slug: Set("current-deployment-456".to_string()),
            state: Set("deployed".to_string()),
            metadata: Set(None),
            image_name: Set(Some("nginx:current".to_string())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let current_deployment = current_deployment.insert(db.as_ref()).await?;

        // Update environment to point to current deployment
        let mut active_environment: environments::ActiveModel = environment.into();
        active_environment.current_deployment_id = Set(Some(current_deployment.id));
        environment = active_environment.update(db.as_ref()).await?;

        let deployment_service = create_deployment_service_for_test(db.clone());

        // Test rollback
        let result = deployment_service
            .rollback_to_deployment(target_deployment.project_id, target_deployment.id)
            .await?;

        // Verify result
        assert_eq!(result.id, target_deployment.id);
        assert!(result.is_current);

        // Verify environment was updated to point to target deployment
        let updated_environment = environments::Entity::find_by_id(environment.id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(
            updated_environment.current_deployment_id,
            Some(target_deployment.id)
        );

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
            metadata: Set(None),
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
                None,
                None,
                None,
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
            preset: Set(Preset::NextJs),
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
}
