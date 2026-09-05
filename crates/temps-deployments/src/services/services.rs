// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use futures::Stream;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DatabaseBackend, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set, Statement, TransactionTrait,
};
use std::collections::HashMap;
use std::collections::HashSet;
use std::pin::Pin;
use std::sync::Arc;
use temps_entities::{
    deployment_container_logs, deployment_containers, deployment_domains, deployments,
    environments, nodes, projects,
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
    LatestDeploymentMedia,
};
use crate::UpdateDeploymentSettingsRequest;
use temps_core::PublicHostnameStrategy;
use temps_core::WorkflowTask;

/// Parameters for container log retrieval
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

#[derive(Debug, Clone)]
pub struct ContainerPresentationContext {
    pub node_names: HashMap<i32, String>,
    pub app_settings: temps_core::AppSettings,
    pub environment_subdomain: String,
    pub public_ports: Vec<temps_entities::preset::ComposePublicPort>,
    pub resource_limits: ResolvedContainerResourceLimits,
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

    #[error(
        "Cannot resolve the build artifact for deployment {deployment_id} in project {project_id}: source deployment {source_deployment_id} was not found"
    )]
    AssetOriginNotFound {
        project_id: i32,
        deployment_id: i32,
        source_deployment_id: i32,
    },

    #[error(
        "Cannot resolve the build artifact for deployment {deployment_id} in project {project_id}: deployment reuse metadata contains a cycle at deployment {source_deployment_id}"
    )]
    AssetOriginCycle {
        project_id: i32,
        deployment_id: i32,
        source_deployment_id: i32,
    },

    #[error(transparent)]
    EnvironmentResolution(#[from] super::env_resolver::DeploymentEnvResolutionError),

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

fn confined_archive_path(
    data_dir: &std::path::Path,
    stored_path: &str,
) -> Result<std::path::PathBuf, DeploymentError> {
    let relative = std::path::Path::new(stored_path);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(DeploymentError::InvalidBundlePath {
            path: stored_path.to_string(),
            reason: "stored archive path escapes the Temps data directory".to_string(),
        });
    }
    Ok(data_dir.join(relative))
}

/// A public git repository + branch, returned only for projects whose repo
/// is actually public -- see [`DeploymentService::get_public_repo_reference`].
#[derive(Clone, Debug)]
pub struct RepoReference {
    pub owner: String,
    pub repo: String,
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeploymentAssetOrigin {
    deployment_id: i32,
    environment_id: i32,
    slug: String,
}

/// Resolve the immutable build artifact behind a deployment. Promotion and
/// rollback rows may already reference an earlier source, so propagating the
/// immediate row would lose assets after the second hop.
fn complete_deployment_asset_origin(
    deployment_id: i32,
    environment_id: i32,
    slug: &str,
    context: Option<&serde_json::Value>,
) -> Option<DeploymentAssetOrigin> {
    let source_deployment_id = context
        .and_then(|value| value.get("source_deployment_id"))
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i32::try_from(value).ok());
    let source_environment_id = context
        .and_then(|value| value.get("source_environment_id"))
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i32::try_from(value).ok());
    let source_slug = context
        .and_then(|value| value.get("source_deployment_slug"))
        .and_then(serde_json::Value::as_str);

    match (source_deployment_id, source_environment_id, source_slug) {
        (Some(deployment_id), Some(environment_id), Some(slug)) => Some(DeploymentAssetOrigin {
            deployment_id,
            environment_id,
            slug: slug.to_string(),
        }),
        (None, None, None) => Some(DeploymentAssetOrigin {
            deployment_id,
            environment_id,
            slug: slug.to_string(),
        }),
        _ => None,
    }
}

fn source_deployment_id(context: Option<&serde_json::Value>) -> Option<i32> {
    context
        .and_then(|value| value.get("source_deployment_id"))
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
}

/// Resolve both the current complete reuse metadata and the partial metadata
/// written by Temps before source slugs were persisted. Legacy rows are walked
/// back to the immutable build deployment so a second promotion/rollback does
/// not perpetuate an intermediate, asset-less deployment.
async fn deployment_asset_origin(
    db: &temps_database::DbConnection,
    deployment: &deployments::Model,
) -> Result<DeploymentAssetOrigin, DeploymentError> {
    let root_deployment_id = deployment.id;
    let project_id = deployment.project_id;
    let mut current = deployment.clone();
    let mut visited = HashSet::from([current.id]);

    loop {
        if let Some(origin) = complete_deployment_asset_origin(
            current.id,
            current.environment_id,
            &current.slug,
            current.context_vars.as_ref(),
        ) {
            return Ok(origin);
        }

        let Some(source_id) = source_deployment_id(current.context_vars.as_ref()) else {
            return Ok(DeploymentAssetOrigin {
                deployment_id: current.id,
                environment_id: current.environment_id,
                slug: current.slug,
            });
        };

        if !visited.insert(source_id) {
            return Err(DeploymentError::AssetOriginCycle {
                project_id,
                deployment_id: root_deployment_id,
                source_deployment_id: source_id,
            });
        }

        current = deployments::Entity::find_by_id(source_id)
            .filter(deployments::Column::ProjectId.eq(project_id))
            .one(db)
            .await?
            .ok_or(DeploymentError::AssetOriginNotFound {
                project_id,
                deployment_id: root_deployment_id,
                source_deployment_id: source_id,
            })?;
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
    /// Anonymous product telemetry reporter (late-bound, optional). Set via
    /// [`Self::set_telemetry`]; defaults to a no-op when unset.
    telemetry: std::sync::OnceLock<Arc<dyn temps_core::telemetry::TelemetryReporter>>,
    /// Resolves a container's environment variables from the selected
    /// environment (late-bound; the resolver's six service deps only exist later
    /// in plugin init). Set via [`Self::set_env_resolver`]. Used by the inline
    /// promote/rollback deploy paths so a promoted/rolled-back container gets the
    /// SAME resolved env (user vars, external-service vars, Sentry/OTel, API
    /// token) as a normal deploy — see [`crate::services::env_resolver`].
    env_resolver: std::sync::OnceLock<Arc<crate::services::env_resolver::DeploymentEnvResolver>>,
    /// Late-bound Compose executor (the `Arc<bollard::Docker>` client it needs
    /// is only constructed later in plugin init, after `DeploymentService`
    /// itself). Set via [`Self::set_compose_executor`]. Used by
    /// `cleanup_containers` to sweep Compose-managed volumes/networks -- which
    /// individual `deployer.remove_container` calls never touch -- when a
    /// project/environment that deployed via Docker Compose is deleted.
    compose_executor: std::sync::OnceLock<Arc<temps_deployer::compose::ComposeExecutor>>,
}

fn deployment_url_from_settings(
    settings: &temps_core::AppSettings,
    proxy_port: u16,
    deployment_slug: &str,
) -> String {
    let domain = PublicHostnameStrategy::Standard
        .deployment_hostname(&settings.preview_domain, deployment_slug);
    let (protocol, port) = if let Some(external_url) = settings.external_url.as_deref() {
        if let Ok(parsed_url) = url::Url::parse(external_url) {
            let protocol = match parsed_url.scheme() {
                "https" => "https",
                _ => "http",
            };
            (protocol, parsed_url.port())
        } else {
            let protocol = if external_url.starts_with("https://") {
                "https"
            } else {
                "http"
            };
            (protocol, None)
        }
    } else {
        ("http", Some(proxy_port))
    };

    match port {
        Some(port)
            if !((protocol == "https" && port == 443) || (protocol == "http" && port == 80)) =>
        {
            format!("{protocol}://{domain}:{port}")
        }
        _ => format!("{protocol}://{domain}"),
    }
}

impl DeploymentService {
    /// Return the currently served deployment media for each requested project.
    ///
    /// Media rows are selected in one ranked `DISTINCT ON` query. A
    /// non-preview production current deployment wins, followed by another
    /// current deployment, then the newest historical deployment with a
    /// screenshot. Historical fallbacks omit their URL because they are not
    /// guaranteed to be routable. The newest attempt status is selected
    /// independently because a failed attempt does not replace the older
    /// deployment that remains live.
    pub async fn get_latest_deployment_media(
        &self,
        project_ids: &[i32],
    ) -> Result<Vec<LatestDeploymentMedia>, DeploymentError> {
        const MAX_PROJECT_IDS: usize = 100;

        if project_ids.is_empty() {
            return Ok(Vec::new());
        }
        if project_ids.len() > MAX_PROJECT_IDS {
            return Err(DeploymentError::InvalidInput(format!(
                "latest deployment media accepts at most {MAX_PROJECT_IDS} project IDs; received {}",
                project_ids.len()
            )));
        }

        let placeholders = (1..=project_ids.len())
            .map(|index| format!("${index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let statement = sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            format!(
                "WITH latest_attempts AS ( \
                    SELECT DISTINCT ON (d.project_id) \
                        d.project_id, d.state AS latest_attempt_status \
                    FROM deployments d \
                    WHERE d.project_id IN ({placeholders}) \
                    ORDER BY d.project_id, d.created_at DESC, d.id DESC \
                 ), candidates AS ( \
                    SELECT d.project_id, d.slug, d.screenshot_location, TRUE AS is_current, \
                        CASE WHEN e.slug = 'production' AND NOT e.is_preview THEN 0 ELSE 1 END AS priority, \
                        e.updated_at AS candidate_updated_at, d.created_at, d.id \
                    FROM environments e \
                    JOIN deployments d \
                      ON d.id = e.current_deployment_id \
                     AND d.project_id = e.project_id \
                    WHERE e.project_id IN ({placeholders}) \
                      AND e.deleted_at IS NULL \
                    UNION ALL \
                    SELECT d.project_id, d.slug, d.screenshot_location, FALSE AS is_current, \
                        2 AS priority, d.created_at AS candidate_updated_at, d.created_at, d.id \
                    FROM deployments d \
                    WHERE d.project_id IN ({placeholders}) \
                      AND d.screenshot_location IS NOT NULL \
                 ), selected_media AS ( \
                    SELECT DISTINCT ON (project_id) \
                        project_id, slug, screenshot_location, is_current \
                    FROM candidates \
                    ORDER BY project_id, priority, candidate_updated_at DESC, created_at DESC, id DESC \
                 ) \
                 SELECT latest_attempts.project_id, latest_attempts.latest_attempt_status, \
                    selected_media.slug, selected_media.screenshot_location, \
                    COALESCE(selected_media.is_current, FALSE) AS is_current \
                 FROM latest_attempts \
                 LEFT JOIN selected_media USING (project_id)"
            ),
            project_ids.iter().copied().map(Into::into),
        );
        let rows = self.db.query_all(statement).await.map_err(|error| {
            DeploymentError::DatabaseError {
                reason: format!(
                    "Failed to query latest deployment media for project IDs {project_ids:?}: {error}"
                ),
            }
        })?;

        let settings = self.config_service.get_settings().await.map_err(|error| {
            DeploymentError::DeploymentError(format!(
                "Failed to load application settings for latest deployment media for project IDs {project_ids:?}: {error}"
            ))
        })?;
        let proxy_port = self.config_service.proxy_port();
        let mut media_by_project = HashMap::with_capacity(rows.len());
        for row in rows {
            let project_id = row.try_get("", "project_id").map_err(|error| {
                DeploymentError::DatabaseError {
                    reason: format!(
                        "Failed to decode project_id in latest deployment media query for project IDs {project_ids:?}: {error}"
                    ),
                }
            })?;
            let slug: Option<String> =
                row.try_get("", "slug")
                    .map_err(|error| DeploymentError::DatabaseError {
                        reason: format!(
                            "Failed to decode deployment slug for project {project_id}: {error}"
                        ),
                    })?;
            let latest_attempt_status: String =
                row.try_get("", "latest_attempt_status")
                    .map_err(|error| DeploymentError::DatabaseError {
                        reason: format!(
                            "Failed to decode latest deployment attempt status for project {project_id}: {error}"
                        ),
                    })?;
            let screenshot_location = row.try_get("", "screenshot_location").map_err(|error| {
                DeploymentError::DatabaseError {
                    reason: format!(
                        "Failed to decode screenshot location for project {project_id}: {error}"
                    ),
                }
            })?;
            let is_current: bool = row.try_get("", "is_current").map_err(|error| {
                DeploymentError::DatabaseError {
                    reason: format!(
                        "Failed to decode current-deployment state for project {project_id}: {error}"
                    ),
                }
            })?;
            let url = if is_current {
                let slug = slug.ok_or_else(|| DeploymentError::DatabaseError {
                    reason: format!(
                        "Current deployment media for project {project_id} is missing its deployment slug"
                    ),
                })?;
                Some(deployment_url_from_settings(&settings, proxy_port, &slug))
            } else {
                None
            };
            media_by_project.insert(
                project_id,
                LatestDeploymentMedia {
                    project_id,
                    latest_attempt_status,
                    url,
                    screenshot_location,
                },
            );
        }

        Ok(project_ids
            .iter()
            .filter_map(|project_id| media_by_project.remove(project_id))
            .collect())
    }

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

    async fn cleanup_containers(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<u64, temps_core::ContainerCleanupError> {
        let mut query = deployment_containers::Entity::find()
            .inner_join(deployments::Entity)
            .filter(deployments::Column::ProjectId.eq(project_id));
        if let Some(environment_id) = environment_id {
            query = query.filter(deployments::Column::EnvironmentId.eq(environment_id));
        }

        let containers = query.all(self.db.as_ref()).await.map_err(|error| {
            temps_core::ContainerCleanupError::Discovery {
                project_id,
                environment_id,
                reason: error.to_string(),
            }
        })?;

        let recorded_container_ids: std::collections::HashSet<String> = containers
            .iter()
            .map(|container| container.container_id.clone())
            .collect();

        let deployment_ids: Vec<i32> = containers
            .iter()
            .map(|container| container.deployment_id)
            .collect();
        let deployment_environment_ids: HashMap<i32, i32> = deployments::Entity::find()
            .filter(deployments::Column::Id.is_in(deployment_ids))
            .all(self.db.as_ref())
            .await
            .map_err(|error| temps_core::ContainerCleanupError::Discovery {
                project_id,
                environment_id,
                reason: format!("failed to resolve container environments: {error}"),
            })?
            .into_iter()
            .map(|deployment| (deployment.id, deployment.environment_id))
            .collect();

        let mut removed = 0_u64;
        for original in containers {
            let container_id = original.container_id.clone();
            let node_id = original.node_id;
            let container_environment_id = deployment_environment_ids
                .get(&original.deployment_id)
                .copied()
                .ok_or_else(|| temps_core::ContainerCleanupError::Discovery {
                    project_id,
                    environment_id,
                    reason: format!(
                        "deployment {} disappeared while preparing container '{}' for cleanup",
                        original.deployment_id, container_id
                    ),
                })?;

            // Hide intentional teardown from routing and health monitoring before
            // Docker observes the removal. The distinct `removing` state makes a
            // process crash retryable instead of turning the row into a false
            // completed cleanup.
            let already_prepared = original.status.as_deref() == Some("removing");
            let prepared = if already_prepared {
                original.clone()
            } else {
                let mut active: deployment_containers::ActiveModel = original.clone().into();
                active.deleted_at = Set(Some(chrono::Utc::now()));
                active.status = Set(Some("removing".to_string()));
                active.update(self.db.as_ref()).await.map_err(|error| {
                    temps_core::ContainerCleanupError::Prepare {
                        project_id,
                        environment_id: container_environment_id,
                        container_id: container_id.clone(),
                        node_id,
                        reason: error.to_string(),
                    }
                })?
            };

            let deployer = match self.deployer_for_node(node_id).await {
                Ok(deployer) => deployer,
                Err(error) => {
                    let reason = self
                        .restore_cleanup_marker(&original, already_prepared)
                        .await
                        .map_or_else(
                            |restore_error| {
                                format!(
                                    "{error}; additionally failed to restore the container record: {restore_error}"
                                )
                            },
                            |()| error.to_string(),
                        );
                    return Err(temps_core::ContainerCleanupError::Removal {
                        project_id,
                        environment_id: container_environment_id,
                        container_id,
                        node_id,
                        reason,
                    });
                }
            };

            match deployer.get_container_info(&container_id).await {
                Ok(info) => {
                    let expected_project = project_id.to_string();
                    let expected_environment = container_environment_id.to_string();
                    if info.labels.get("sh.temps.managed").map(String::as_str) != Some("true")
                        || info.labels.get("sh.temps.project_id") != Some(&expected_project)
                        || info.labels.get("sh.temps.environment") != Some(&expected_environment)
                    {
                        let reason = "runtime container labels do not match the project and environment being deleted".to_string();
                        let _ = self
                            .restore_cleanup_marker(&original, already_prepared)
                            .await;
                        return Err(temps_core::ContainerCleanupError::Removal {
                            project_id,
                            environment_id: container_environment_id,
                            container_id,
                            node_id,
                            reason,
                        });
                    }
                }
                Err(temps_deployer::DeployerError::ContainerNotFound(_)) => {}
                Err(error) => {
                    let _ = self
                        .restore_cleanup_marker(&original, already_prepared)
                        .await;
                    return Err(temps_core::ContainerCleanupError::Removal {
                        project_id,
                        environment_id: container_environment_id,
                        container_id,
                        node_id,
                        reason: format!("failed to verify runtime container ownership: {error}"),
                    });
                }
            }

            let removal_error = match tokio::time::timeout(
                std::time::Duration::from_secs(30),
                deployer.remove_container(&container_id),
            )
            .await
            {
                Ok(Ok(())) | Ok(Err(temps_deployer::DeployerError::ContainerNotFound(_))) => None,
                Ok(Err(error)) => Some(error.to_string()),
                Err(_) => Some("container removal timed out after 30 seconds".to_string()),
            };
            if let Some(reason) = removal_error {
                // Once a request may have reached Docker/the agent, its result is
                // ambiguous. Keep the durable `removing` marker so routing stays
                // fenced and a retry converges idempotently.
                return Err(temps_core::ContainerCleanupError::Removal {
                    project_id,
                    environment_id: container_environment_id,
                    container_id,
                    node_id,
                    reason,
                });
            }

            let mut active: deployment_containers::ActiveModel = prepared.into();
            active.status = Set(Some("removed".to_string()));
            active.update(self.db.as_ref()).await.map_err(|error| {
                temps_core::ContainerCleanupError::Finalize {
                    project_id,
                    environment_id: container_environment_id,
                    container_id: container_id.clone(),
                    node_id,
                    reason: error.to_string(),
                }
            })?;

            removed += 1;
            info!(
                project_id,
                environment_id = container_environment_id,
                container_id,
                ?node_id,
                "Removed application container before owner deletion"
            );
        }

        // Compose can create labeled containers before the deployment rows are
        // registered. Discover those runtime-owned containers as well so a
        // concurrent cancellation/deletion cannot orphan an unrecorded stack.
        let runtime_containers = self.deployer.list_containers().await.map_err(|error| {
            temps_core::ContainerCleanupError::Discovery {
                project_id,
                environment_id,
                reason: format!("failed to discover labeled runtime containers: {error}"),
            }
        })?;
        let expected_project = project_id.to_string();
        let expected_environment = environment_id.map(|id| id.to_string());
        for container in runtime_containers {
            if recorded_container_ids.contains(&container.container_id)
                || container.labels.get("sh.temps.managed").map(String::as_str) != Some("true")
                || container.labels.get("sh.temps.project_id") != Some(&expected_project)
                || expected_environment.as_ref().is_some_and(|expected| {
                    container.labels.get("sh.temps.environment") != Some(expected)
                })
            {
                continue;
            }
            let container_environment_id = container
                .labels
                .get("sh.temps.environment")
                .and_then(|value| value.parse::<i32>().ok())
                .ok_or_else(|| temps_core::ContainerCleanupError::Discovery {
                    project_id,
                    environment_id,
                    reason: format!(
                        "managed container '{}' has an invalid environment label",
                        container.container_id
                    ),
                })?;
            match tokio::time::timeout(
                std::time::Duration::from_secs(30),
                self.deployer.remove_container(&container.container_id),
            )
            .await
            {
                Ok(Ok(())) | Ok(Err(temps_deployer::DeployerError::ContainerNotFound(_))) => {
                    removed += 1;
                }
                Ok(Err(error)) => {
                    return Err(temps_core::ContainerCleanupError::Removal {
                        project_id,
                        environment_id: container_environment_id,
                        container_id: container.container_id,
                        node_id: None,
                        reason: error.to_string(),
                    });
                }
                Err(_) => {
                    return Err(temps_core::ContainerCleanupError::Removal {
                        project_id,
                        environment_id: container_environment_id,
                        container_id: container.container_id,
                        node_id: None,
                        reason: "runtime container removal timed out after 30 seconds".to_string(),
                    });
                }
            }
        }

        // Individual `deployer.remove_container` calls above remove Compose
        // containers themselves, but never the volumes/networks `docker
        // compose up` also creates for the stack -- those only carry the
        // `com.docker.compose.project` label. Sweep them per environment
        // (compose project names are `temps-{project_id}-{environment_id}`,
        // see `DeployComposeJob`). Best-effort: a stuck volume/network must
        // not block the deletion the caller is otherwise done with.
        if let Some(compose_executor) = self.compose_executor.get() {
            let compose_environment_ids: Vec<i32> = match environment_id {
                Some(id) => vec![id],
                None => environments::Entity::find()
                    .filter(environments::Column::ProjectId.eq(project_id))
                    .select_only()
                    .column(environments::Column::Id)
                    .into_tuple()
                    .all(self.db.as_ref())
                    .await
                    .map_err(|error| temps_core::ContainerCleanupError::Discovery {
                        project_id,
                        environment_id,
                        reason: format!(
                            "failed to enumerate environments for Compose resource cleanup: {error}"
                        ),
                    })?,
            };
            for env_id in compose_environment_ids {
                let compose_project_name = format!("temps-{project_id}-{env_id}");
                if let Err(error) = compose_executor.destroy(&compose_project_name).await {
                    warn!(
                        project_id,
                        environment_id = env_id,
                        compose_project = %compose_project_name,
                        %error,
                        "Failed to clean up Compose-managed volumes/networks (best-effort)"
                    );
                }
            }
        }

        Ok(removed)
    }

    async fn restore_cleanup_marker(
        &self,
        original: &deployment_containers::Model,
        already_prepared: bool,
    ) -> Result<(), sea_orm::DbErr> {
        if already_prepared {
            return Ok(());
        }
        let mut active: deployment_containers::ActiveModel = original.clone().into();
        active.status = Set(original.status.clone());
        active.deleted_at = Set(original.deleted_at);
        active.update(self.db.as_ref()).await.map(|_| ())
    }

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

    fn resource_usage_from_snapshot(
        snapshot: &temps_entities::deployment_config::DeploymentConfigSnapshot,
    ) -> crate::jobs::ResourceUsage {
        crate::jobs::ResourceUsage {
            cpu_limit: snapshot.cpu_limit.map(|value| format!("{value}u")),
            memory_limit: snapshot.memory_limit.map(|value| format!("{value}Mi")),
            cpu_request: snapshot.cpu_request.map(|value| format!("{value}u")),
            memory_request: snapshot.memory_request.map(|value| format!("{value}Mi")),
        }
    }

    fn rollback_snapshot_port_and_replicas(
        snapshot: &temps_entities::deployment_config::DeploymentConfigSnapshot,
        deployment_id: i32,
    ) -> Result<(Option<u16>, u32), DeploymentError> {
        let port = snapshot
            .exposed_port
            .map(u16::try_from)
            .transpose()
            .map_err(|error| {
                DeploymentError::InvalidDeploymentState(format!(
                    "Target deployment {deployment_id} has invalid exposed port {:?}: {error}",
                    snapshot.exposed_port
                ))
            })?;
        let replicas = u32::try_from(snapshot.replicas)
            .ok()
            .filter(|replicas| *replicas > 0)
            .ok_or_else(|| {
                DeploymentError::InvalidDeploymentState(format!(
                    "Target deployment {deployment_id} has invalid replica count {}",
                    snapshot.replicas
                ))
            })?;
        Ok((port, replicas))
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
            telemetry: std::sync::OnceLock::new(),
            env_resolver: std::sync::OnceLock::new(),
            compose_executor: std::sync::OnceLock::new(),
        }
    }

    /// Late-bind the environment-variable resolver (see the field docs). Called
    /// once during plugin init after the resolver's service deps exist.
    pub fn set_env_resolver(
        &self,
        resolver: Arc<crate::services::env_resolver::DeploymentEnvResolver>,
    ) {
        let _ = self.env_resolver.set(resolver);
    }

    /// Late-bind the Compose executor (see the field docs). Called once
    /// during plugin init after the `Arc<bollard::Docker>` client exists.
    pub fn set_compose_executor(&self, executor: Arc<temps_deployer::compose::ComposeExecutor>) {
        let _ = self.compose_executor.set(executor);
    }

    /// Set the anonymous telemetry reporter used to emit deploy-funnel events
    /// (currently `rollback_triggered`).
    pub fn set_telemetry(&self, reporter: Arc<dyn temps_core::telemetry::TelemetryReporter>) {
        let _ = self.telemetry.set(reporter);
    }

    /// The telemetry reporter, or a no-op when none has been wired.
    fn telemetry(&self) -> Arc<dyn temps_core::telemetry::TelemetryReporter> {
        self.telemetry
            .get()
            .cloned()
            .unwrap_or_else(|| Arc::new(temps_core::telemetry::NoopTelemetryReporter))
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

        crate::cluster_ca::build_node_deployer(
            &node.address,
            token,
            node.name.clone(),
            self.config_service.as_ref(),
            self.encryption_service.as_ref(),
        )
        .await
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

        // Mutual TLS for https:// nodes (ADR-020 WS-2.1), plain HTTP otherwise
        // — the shared factory presents the CP's cluster-CA-signed identity so
        // the stream isn't rejected once `require_mtls` is on. No top-level
        // timeout: log streams are long-lived by design.
        let client = crate::cluster_ca::build_node_http_client(
            &node.address,
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
        // Clamp before the `as u64` cast: an out-of-range or negative i64 here
        // wraps to a huge u64 on cast, and Sea-ORM's OFFSET (page_size * page)
        // then overflows the i64 bind sea-query-binder sends to Postgres,
        // panicking with `TryFromIntError(PosOverflow)` and taking down the
        // whole HTTP listener task -- not just this request. Every caller
        // (REST and MCP) reaches this cast, so it must be enforced here, not
        // only at a caller's argument-parsing boundary.
        let page = page.unwrap_or(1).clamp(1, i64::from(i32::MAX)) as u64;
        let per_page = per_page.unwrap_or(10).clamp(1, 100) as u64;

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
        self.trigger_pipeline_inner(project_id, environment_id, branch, tag, commit, None)
            .await
    }

    /// Internal pipeline trigger that also carries an optional rollback marker.
    /// `rollback_from_deployment_id` is `Some(id)` only for rebuild-from-source
    /// rollbacks, which tags the resulting deployment as a rollback of `id`.
    async fn trigger_pipeline_inner(
        &self,
        project_id: i32,
        environment_id: i32,
        branch: Option<String>,
        tag: Option<String>,
        commit: Option<String>,
        rollback_from_deployment_id: Option<i32>,
    ) -> Result<(), DeploymentError> {
        info!("Triggering pipeline for project_id: {}", project_id);
        let project = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .one(self.db.as_ref())
            .await
            .map_err(|e| DeploymentError::Other(e.to_string()))?;

        let project = project.ok_or_else(|| {
            DeploymentError::NotFound(format!("project {} not found", project_id))
        })?;
        debug!(
            "Project found id={} slug={} preset={}",
            project.id, project.slug, project.preset
        );

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
            rollback_from_deployment_id,
            // This trigger names a concrete environment (redeploy, rollback,
            // node-drain reschedule) — deploy to it directly instead of
            // re-inferring the target from the branch.
            target_environment_id: Some(environment_id),
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

    /// Trigger a real deployment of a pre-built Docker image, with no build
    /// step. This is the DockerImage-source counterpart to `trigger_pipeline`
    /// (which is Git-only and requires `repo_owner`/`repo_name`) — used to
    /// deploy projects that have no git repository at all, e.g. imports from
    /// Portainer, Kubernetes, or Kamal.
    ///
    /// Reuses the `DeployImageRequested` job already driven by the template
    /// one-click-deploy flow (see `job_processor::process_deploy_image_requested_job`),
    /// which creates the deployment row itself (with `external_image_ref` in
    /// its metadata) and plans a pull+run pipeline for the project's
    /// non-preview environment(s).
    pub async fn trigger_image_deployment(
        &self,
        project_id: i32,
        target_environment_id: Option<i32>,
        image_ref: String,
        health_check_path: Option<String>,
        command: Option<Vec<String>>,
    ) -> Result<(), DeploymentError> {
        if image_ref.is_empty() {
            return Err(DeploymentError::InvalidInput(
                "Image reference is missing".to_string(),
            ));
        }

        info!(
            "Triggering image deployment for project_id: {} (image: {})",
            project_id, image_ref
        );

        self.queue_service
            .send(temps_core::Job::DeployImageRequested(
                temps_core::DeployImageRequestedJob {
                    project_id,
                    target_environment_id,
                    image_ref,
                    health_check_path,
                    command,
                },
            ))
            .await
            .map_err(|e| {
                tracing::error!("Failed to send DeployImageRequested to queue: {}", e);
                DeploymentError::QueueError(e.to_string())
            })?;

        tracing::debug!("DeployImageRequested successfully sent to queue");
        Ok(())
    }

    /// Redeploy the exact workload affected by node drain or failover.
    pub async fn redeploy_environment(
        &self,
        project_id: i32,
        environment_id: i32,
        deployment_id: i32,
    ) -> Result<(), DeploymentError> {
        // Use the deployment that owns the affected containers. Selecting the
        // newest row can race a concurrent failed/cancelled deploy and restore
        // the wrong workload during failover.
        let deploy = deployments::Entity::find_by_id(deployment_id)
            .filter(deployments::Column::ProjectId.eq(project_id))
            .filter(deployments::Column::EnvironmentId.eq(environment_id))
            .one(self.db.as_ref())
            .await
            .map_err(|e| DeploymentError::Other(format!(
                "Failed to load deployment {deployment_id} for project {project_id}, environment {environment_id}: {e}"
            )))?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!(
                    "Deployment {deployment_id} was not found in project {project_id}, environment {environment_id}"
                ))
            })?;

        // Git-less deployments (docker_image source, e.g. imports or
        // `deployFromImage`) have no branch/tag/commit to rebuild from —
        // `trigger_pipeline` requires `repo_owner`/`repo_name` and fails with
        // "Project repo_owner is missing" for these. Redeploy them from the
        // same image instead, mirroring `trigger_image_deployment`.
        if let Some(image_ref) = deploy
            .metadata
            .as_ref()
            .and_then(|m| m.external_image_ref.clone())
        {
            return self
                .trigger_image_deployment(
                    project_id,
                    Some(environment_id),
                    image_ref,
                    deploy
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.health_check_path.clone()),
                    deploy
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.command.clone()),
                )
                .await;
        }

        let (branch, tag, commit) = (
            deploy.branch_ref.clone(),
            deploy.tag_ref.clone(),
            deploy.commit_sha.clone(),
        );

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

        // Validate that the deployment is in a valid state for rollback.
        //
        // "stopped" belongs here alongside "completed": once a LATER
        // deployment supersedes this one, `cancel_previous_deployments`
        // stops its containers and flips its state to "stopped" (see that
        // function and `teardown_deployment`) -- that is the terminal state
        // every successful-but-no-longer-current deployment actually ends up
        // in. Rollback's whole purpose is reverting to an older deployment,
        // so its target is virtually always going to be "stopped" in
        // practice; excluding it made rollback reject its own primary use
        // case ("Cannot rollback to deployment in 'stopped' state") for any
        // deployment that had already been superseded -- which is every
        // deployment a real user would ever actually want to roll back to.
        // "deployed" is kept for the one other live path that sets it
        // (`resume_deployment`); "failed"/"cancelled"/"paused" stay excluded
        // -- a failed/cancelled deployment has no reliable image to reuse,
        // and rolling back TO a paused deployment is a distinct, not yet
        // supported, operation.
        let valid_rollback_states = ["deployed", "completed", "stopped"];
        if !valid_rollback_states.contains(&target_deployment.state.as_str()) {
            return Err(DeploymentError::InvalidDeploymentState(format!(
                "Cannot rollback to deployment in '{}' state. Only deployed, completed, or stopped (superseded) deployments can be rolled back to.",
                target_deployment.state
            )));
        }

        let environment_id = target_deployment.environment_id;

        let project = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Project not found".to_string()))?;

        let preset =
            temps_presets::get_preset_for_storage(project.preset, project.preset_config.as_ref())
                .map_err(|error| DeploymentError::InvalidInput(error.to_string()))?
                .ok_or_else(|| DeploymentError::NotFound("Preset not found".to_string()))?;

        // --- Git projects: rebuild from source when the image isn't reusable ---
        //
        // The image-reuse path below is fast — it redeploys the target
        // deployment's stored Docker image as-is — but it only works when that
        // image is still present locally. The nightly cleanup prunes images
        // after ~7 days, so reusing an older one fails with "image no longer
        // exists locally", and static deployments have no runnable server image
        // to reuse at all.
        //
        // So for git-sourced projects we PREFER image reuse when the image is
        // still in the local Docker cache (the common case — rolling back a
        // recent deploy): it's near-instant and byte-identical to what we're
        // rolling back to, with no dependency on the git remote or registry.
        // We only fall back to a full rebuild-from-source at the target
        // deployment's commit when the image is gone (pruned) or the preset is
        // static (no reusable server image). The rebuild path always works (no
        // dependency on a surviving image), goes through the same health checks
        // as a normal deploy, and reconstructs static bundles correctly.
        //
        // Non-git projects (docker_image / static_files / manual without a git
        // ref) have no source to rebuild, so they always use image reuse.
        let has_git_ref = target_deployment
            .commit_sha
            .as_ref()
            .is_some_and(|c| !c.is_empty())
            || target_deployment
                .branch_ref
                .as_ref()
                .is_some_and(|b| !b.is_empty());

        // Is the target's image still in the local cache? A static preset has no
        // reusable server image, so treat it as "not present" to force a rebuild.
        // Any error probing Docker is treated as "not present" — rebuilding from
        // source is always safe, whereas trusting a possibly-stale image is not.
        let is_static = preset.project_type() == temps_presets::ProjectType::Static;
        let image_present = if is_static {
            false
        } else {
            match target_deployment.image_name.as_deref() {
                Some(img) if !img.is_empty() => {
                    self.deployer.image_exists(img).await.unwrap_or(false)
                }
                _ => false,
            }
        };

        if project.source_type == temps_entities::source_type::SourceType::Git
            && has_git_ref
            && !image_present
        {
            info!(
                "Rollback: project {} is git-sourced and the target image is unavailable ({}) — rebuilding from source at commit {:?} (rolling back to #{})",
                project_id,
                if is_static { "static preset" } else { "image not in local cache" },
                target_deployment.commit_sha,
                deployment_id
            );

            // Snapshot the latest deployment id BEFORE triggering, so we can
            // identify the one the pipeline creates and return it.
            let prev_max_id = deployments::Entity::find()
                .filter(deployments::Column::ProjectId.eq(project_id))
                .filter(deployments::Column::EnvironmentId.eq(environment_id))
                .order_by_desc(deployments::Column::Id)
                .one(self.db.as_ref())
                .await?
                .map(|d| d.id)
                .unwrap_or(0);

            self.trigger_pipeline_inner(
                project_id,
                environment_id,
                target_deployment.branch_ref.clone(),
                target_deployment.tag_ref.clone(),
                target_deployment.commit_sha.clone(),
                Some(deployment_id),
            )
            .await?;

            // Anonymous telemetry: a rollback was initiated. No identifying props.
            self.telemetry()
                .report(temps_core::telemetry::TelemetryEvent::new(
                    temps_core::telemetry::TelemetryEventKind::RollbackTriggered,
                ));

            // The pipeline created a new deployment row; return it so the API
            // response carries the rollback deployment's id/status. It's the
            // newest row for this environment above the prior max.
            let created = deployments::Entity::find()
                .filter(deployments::Column::ProjectId.eq(project_id))
                .filter(deployments::Column::EnvironmentId.eq(environment_id))
                .filter(deployments::Column::Id.gt(prev_max_id))
                .order_by_desc(deployments::Column::Id)
                .one(self.db.as_ref())
                .await?;

            let model = match created {
                Some(dep) => dep,
                // The job is queued; the row may not be visible yet. Surface the
                // target as a stand-in rather than failing — the rollback is
                // already in flight.
                None => target_deployment,
            };
            return Ok(self
                .map_db_deployment_to_deployment(model, false, None)
                .await);
        }

        // Ensure target deployment has an image to roll back to
        let image_name = target_deployment.image_name.clone().ok_or_else(|| {
            DeploymentError::Other(
                "Target deployment has no image_name - cannot rollback".to_string(),
            )
        })?;

        let environment = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Environment not found".to_string()))?;

        info!(
            "Initiating rollback for project_id: {}, to deployment_id: {}, image: {}, environment_id: {}",
            project_id, deployment_id, image_name, environment_id
        );

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

        let rollback_asset_origin =
            deployment_asset_origin(self.db.as_ref(), &target_deployment).await?;
        let rollback_health_check_path = target_deployment
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.health_check_path.clone());
        let rollback_command = target_deployment
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.command.clone());
        let rollback_metadata = DeploymentMetadata {
            is_rollback: true,
            rolled_back_from_id: Some(deployment_id),
            health_check_path: rollback_health_check_path.clone(),
            command: rollback_command.clone(),
            ..Default::default()
        };

        let new_deployment = deployments::ActiveModel {
            id: sea_orm::NotSet,
            project_id: Set(project_id),
            environment_id: Set(environment_id),
            slug: Set(rollback_slug.clone()),
            state: Set("pending".to_string()),
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
                "source_deployment_id": rollback_asset_origin.deployment_id,
                "source_deployment_slug": rollback_asset_origin.slug.clone(),
                "source_environment_id": rollback_asset_origin.environment_id,
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

        if !super::job_processor::JobProcessorService::try_admit_deployment(
            self.db.as_ref(),
            rollback_deployment_id,
        )
        .await
        .map_err(|error| DeploymentError::DatabaseError {
            reason: format!(
                "Failed to admit rollback deployment {}: {}",
                rollback_deployment_id, error
            ),
        })? {
            return Err(DeploymentError::InvalidDeploymentState(format!(
                "Rollback deployment {} was not admitted because its owner is being deleted",
                rollback_deployment_id
            )));
        }

        // Anonymous telemetry: a rollback was initiated. No identifying props.
        self.telemetry()
            .report(temps_core::telemetry::TelemetryEvent::new(
                temps_core::telemetry::TelemetryEventKind::RollbackTriggered,
            ));

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
            let (configured_port, rollback_replicas, rollback_resources) =
                if let Some(snapshot) = target_deployment.deployment_config.as_ref() {
                    let (port, replicas) =
                        Self::rollback_snapshot_port_and_replicas(snapshot, deployment_id)?;
                    (port, replicas, Self::resource_usage_from_snapshot(snapshot))
                } else {
                    (
                        super::port_resolver::configured_port_override(&environment, &project),
                        environment
                            .deployment_config
                            .as_ref()
                            .map(|config| config.replicas as u32)
                            .or_else(|| {
                                project
                                    .deployment_config
                                    .as_ref()
                                    .map(|config| config.replicas as u32)
                            })
                            .unwrap_or(1),
                        Self::resolve_resource_usage(
                            environment.deployment_config.as_ref(),
                            project.deployment_config.as_ref(),
                        ),
                    )
                };
            let exposed_port = configured_port.map(u32::from).unwrap_or(3000);
            let mut deploy_builder = crate::jobs::DeployImageJobBuilder::new()
                .job_id("deploy_container".to_string())
                .build_job_id("external-image".to_string())
                .target(crate::jobs::DeploymentTarget::Docker {
                    registry_url: "local".to_string(),
                    network: Some(temps_core::NETWORK_NAME.to_string()),
                })
                .service_name(rollback_slug.clone())
                .health_check_path(None)
                .health_check_path_override(rollback_health_check_path)
                .command(rollback_command)
                .replicas(rollback_replicas)
                .port(exposed_port)
                .configured_port(configured_port)
                .log_id(deploy_log_id.clone())
                .log_service(self.log_service.clone())
                .failed_container_retention(self.db.clone(), rollback_deployment_id);

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
            deploy_builder = deploy_builder.resources(rollback_resources);

            // Resolve the environment's env vars exactly as a normal deploy does,
            // so the rolled-back container boots with the full set (user vars,
            // external-service connection strings, SENTRY_DSN, TEMPS_API_TOKEN,
            // CRON_SECRET, OTEL_*) instead of nothing. Without this, a rollback
            // reuses the image but starts it unconfigured.
            let resolved_env = if let Some(resolver) = self.env_resolver.get() {
                let mut resolved = resolver
                    .resolve(&project, &environment, &rollback_deployment)
                    .await?;
                crate::services::env_resolver::apply_deployment_owned_variables(
                    &mut resolved,
                    project.preset,
                    &rollback_asset_origin.slug,
                    (project.preset != temps_entities::preset::Preset::DockerCompose)
                        .then_some(exposed_port),
                );
                resolved
            } else {
                tracing::warn!(
                    "Rollback: env resolver not wired — rolled-back container starts with no resolved env vars"
                );
                std::collections::HashMap::new()
            };
            deploy_builder = deploy_builder.environment_variables(resolved_env);

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

            let cancellation_provider =
                super::workflow_execution_service::DatabaseCancellationProvider::new(
                    self.db.clone(),
                    rollback_deployment_id,
                );
            match deploy_job
                .execute_with_cancellation(rollback_context.clone(), &cancellation_provider)
                .await
            {
                Ok(job_result) => {
                    info!("Rollback: Deploy job completed successfully");
                    rollback_context = job_result.context;

                    // Update deploy job record to Success
                    let mut active_job: deployment_jobs::ActiveModel = deploy_job_model.into();
                    active_job.status = Set(temps_entities::types::JobStatus::Success);
                    active_job.finished_at = Set(Some(chrono::Utc::now()));
                    let _ = active_job.update(self.db.as_ref()).await;
                }
                Err(e) => {
                    error!("Rollback: Deploy job failed: {}", e);
                    let failure_message = match deploy_job.cleanup(&rollback_context).await {
                        Ok(()) => format!("Deploy failed: {e}"),
                        Err(cleanup_error) => format!(
                            "Deploy failed: {e}; rollback container cleanup also failed: {cleanup_error}"
                        ),
                    };

                    // Update deploy job record to Failure
                    let mut active_job: deployment_jobs::ActiveModel = deploy_job_model.into();
                    active_job.status = Set(temps_entities::types::JobStatus::Failure);
                    active_job.finished_at = Set(Some(chrono::Utc::now()));
                    active_job.error_message = Set(Some(failure_message.clone()));
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
                    active_dep.cancelled_reason = Set(Some(failure_message.clone()));
                    let _ = active_dep.update(self.db.as_ref()).await;

                    return Err(DeploymentError::Other(format!(
                        "Failed to deploy image during rollback: {failure_message}"
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
                    active_job.status = Set(temps_entities::types::JobStatus::Success);
                    active_job.finished_at = Set(Some(chrono::Utc::now()));
                    let _ = active_job.update(self.db.as_ref()).await;
                }
                Err(e) => {
                    error!("Rollback: Mark complete job failed: {}", e);

                    // Update complete job record to Failure
                    let mut active_job: deployment_jobs::ActiveModel = complete_job_model.into();
                    active_job.status = Set(temps_entities::types::JobStatus::Failure);
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

                // Mark the row deleted *before* stopping the container in Docker
                // (see the identical race explained in
                // WorkflowExecutionService::teardown_previous_deployment):
                // ContainerHealthMonitor polls on its own schedule and would
                // otherwise observe this container mid-exit with no signal that
                // the exit is an intentional pre-rollback cleanup, firing a
                // false ContainerCrash alarm.
                let mut active_container: deployment_containers::ActiveModel = container.into();
                active_container.deleted_at = Set(Some(chrono::Utc::now()));
                active_container.status = Set(Some("removed".to_string()));
                if let Err(e) = active_container.update(self.db.as_ref()).await {
                    warn!(
                        "Failed to mark container {} deleted before pre-rollback stop: {}",
                        container_id, e
                    );
                }

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

                info!(
                    "Pre-rollback: stopped and removed container {}",
                    container_id
                );
            }

            // If this deployment is currently in-flight (state = "running"),
            // its MarkDeploymentCompleteJob may still be executing — in
            // particular, it may be waiting inside Phase 2.75 (public
            // readiness check). We have just killed all of its containers, so
            // Phase 2.75 will fail, causing reject_unusable_deployment to mark
            // this deployment "failed" even though it was intentionally
            // superseded by the incoming rollback. Atomically flip it to
            // "stopped" here (CAS: only transitions from "running") so that
            // the staleness check added to mark_complete_inner can detect the
            // supersession and abort cleanly without calling
            // reject_unusable_deployment, preserving "stopped" for
            // promote/rollback reuse.
            if dep.state == "running" {
                use sea_orm::sea_query::Expr;
                match deployments::Entity::update_many()
                    .col_expr(deployments::Column::State, Expr::value("stopped"))
                    .col_expr(
                        deployments::Column::UpdatedAt,
                        Expr::value(chrono::Utc::now()),
                    )
                    .filter(deployments::Column::Id.eq(dep.id))
                    .filter(deployments::Column::State.eq("running"))
                    .exec(self.db.as_ref())
                    .await
                {
                    Ok(res) if res.rows_affected > 0 => {
                        info!(
                            "Pre-rollback: marked in-flight deployment {} as stopped \
                             to prevent spurious 'failed' state from concurrent Phase 2.75",
                            dep.id
                        );
                    }
                    Ok(_) => {
                        // 0 rows affected: deployment already left "running"
                        // (e.g., completed between the container kill and this
                        // update) — nothing to do.
                    }
                    Err(e) => {
                        warn!(
                            "Pre-rollback: failed to mark in-flight deployment {} as stopped: {}",
                            dep.id, e
                        );
                    }
                }
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

        // Validate state — only successful deployments can be promoted.
        // "stopped" is included for the same reason `rollback_to_deployment`
        // includes it (see the comment there): a source deployment that has
        // since been superseded by a newer one in ITS OWN environment is
        // "stopped", not "completed" -- and promoting an older, already-
        // superseded deployment's image into a different environment is
        // exactly the kind of thing a real user does (e.g. re-promote a
        // known-good build after a bad one shipped on top of it).
        let valid_states = ["deployed", "completed", "ready", "stopped"];
        if !valid_states.contains(&source.state.as_str()) {
            return Err(DeploymentError::InvalidDeploymentState(format!(
                "Cannot promote deployment in '{}' state. Only deployed/completed/ready/stopped deployments can be promoted.",
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
            .filter(projects::Column::IsDeleted.eq(false))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Project not found".to_string()))?;

        info!(
            "Promoting deployment {} to environment '{}' (project {}, image: {})",
            source_deployment_id, target_env.name, project_id, image_name
        );

        let preset =
            temps_presets::get_preset_for_storage(project.preset, project.preset_config.as_ref())
                .map_err(|error| DeploymentError::InvalidInput(error.to_string()))?
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
            health_check_path: source
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.health_check_path.clone()),
            command: source
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.command.clone()),
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

        let promotion_asset_origin = deployment_asset_origin(self.db.as_ref(), &source).await?;
        let new_deployment = deployments::ActiveModel {
            id: sea_orm::NotSet,
            project_id: Set(project_id),
            environment_id: Set(target_environment_id),
            slug: Set(promote_slug.clone()),
            state: Set("pending".to_string()),
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
                "source_deployment_id": promotion_asset_origin.deployment_id,
                "source_deployment_slug": promotion_asset_origin.slug.clone(),
                "source_environment_id": promotion_asset_origin.environment_id,
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

        if !super::job_processor::JobProcessorService::try_admit_deployment(
            self.db.as_ref(),
            promoted_id,
        )
        .await
        .map_err(|error| DeploymentError::DatabaseError {
            reason: format!(
                "Failed to admit promoted deployment {}: {}",
                promoted_id, error
            ),
        })? {
            return Err(DeploymentError::InvalidDeploymentState(format!(
                "Promoted deployment {} was not admitted because its owner is being deleted",
                promoted_id
            )));
        }

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
            let configured_port =
                super::port_resolver::configured_port_override(&target_env, &project);
            let exposed_port = configured_port.map(u32::from).unwrap_or(3000);
            let mut deploy_builder = crate::jobs::DeployImageJobBuilder::new()
                .job_id("deploy_container".to_string())
                .build_job_id("external-image".to_string())
                .target(crate::jobs::DeploymentTarget::Docker {
                    registry_url: "local".to_string(),
                    network: Some(temps_core::NETWORK_NAME.to_string()),
                })
                .service_name(promote_slug.clone())
                .health_check_path(None)
                .health_check_path_override(
                    promoted_deployment
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.health_check_path.clone()),
                )
                .command(
                    promoted_deployment
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.command.clone()),
                )
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
                .port(exposed_port)
                .configured_port(configured_port)
                .log_id(deploy_log_id.clone())
                .log_service(self.log_service.clone())
                .failed_container_retention(self.db.clone(), promoted_id);

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

            // Resolve the TARGET environment's env vars exactly as a normal
            // deploy does, so the promoted container boots with the full set
            // (user vars, external-service connection strings, SENTRY_DSN,
            // TEMPS_API_TOKEN/URL, CRON_SECRET, OTEL_*) instead of nothing.
            // Without this, promotion reuses the image but starts it unconfigured.
            let resolved_env = if let Some(resolver) = self.env_resolver.get() {
                let mut resolved = resolver
                    .resolve(&project, &target_env, &promoted_deployment)
                    .await?;
                crate::services::env_resolver::apply_deployment_owned_variables(
                    &mut resolved,
                    project.preset,
                    &promotion_asset_origin.slug,
                    (project.preset != temps_entities::preset::Preset::DockerCompose)
                        .then_some(exposed_port),
                );
                resolved
            } else {
                tracing::warn!(
                    "Promotion: env resolver not wired — promoted container starts with no resolved env vars"
                );
                std::collections::HashMap::new()
            };
            deploy_builder = deploy_builder.environment_variables(resolved_env);

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

            let cancellation_provider =
                super::workflow_execution_service::DatabaseCancellationProvider::new(
                    self.db.clone(),
                    promoted_id,
                );
            match deploy_job
                .execute_with_cancellation(promote_context.clone(), &cancellation_provider)
                .await
            {
                Ok(job_result) => {
                    info!("Promotion: Deploy job completed successfully");
                    promote_context = job_result.context;

                    let mut active_job: deployment_jobs::ActiveModel = deploy_job_model.into();
                    active_job.status = Set(temps_entities::types::JobStatus::Success);
                    active_job.finished_at = Set(Some(chrono::Utc::now()));
                    let _ = active_job.update(self.db.as_ref()).await;
                }
                Err(e) => {
                    error!("Promotion: Deploy job failed: {}", e);
                    let failure_message = match deploy_job.cleanup(&promote_context).await {
                        Ok(()) => format!("Deploy failed: {e}"),
                        Err(cleanup_error) => format!(
                            "Deploy failed: {e}; promoted container cleanup also failed: {cleanup_error}"
                        ),
                    };

                    let mut active_job: deployment_jobs::ActiveModel = deploy_job_model.into();
                    active_job.status = Set(temps_entities::types::JobStatus::Failure);
                    active_job.finished_at = Set(Some(chrono::Utc::now()));
                    active_job.error_message = Set(Some(failure_message.clone()));
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
                    active_dep.cancelled_reason = Set(Some(failure_message.clone()));
                    let _ = active_dep.update(self.db.as_ref()).await;

                    return Err(DeploymentError::Other(format!(
                        "Failed to deploy image during promotion: {failure_message}"
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
                    active_job.status = Set(temps_entities::types::JobStatus::Success);
                    active_job.finished_at = Set(Some(chrono::Utc::now()));
                    let _ = active_job.update(self.db.as_ref()).await;
                }
                Err(e) => {
                    error!("Promotion: Mark complete job failed: {}", e);

                    let mut active_job: deployment_jobs::ActiveModel = complete_job_model.into();
                    active_job.status = Set(temps_entities::types::JobStatus::Failure);
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
        use temps_entities::{deployment_containers, deployments, status_incidents};

        // First verify the deployment exists and belongs to the project
        let deployment = deployments::Entity::find_by_id(deployment_id)
            .filter(deployments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Deployment not found".to_string()))?;

        let environment_id = deployment.environment_id;

        // Persist "paused" BEFORE touching any container. Monitoring (the
        // container-health poller and the uptime health checker) treats
        // `state == "paused"` as "stopped on purpose, don't alert" — if we
        // stopped containers first and updated this row last, a poll that
        // lands in between would see an exited container against a
        // not-yet-paused deployment and fire a false crash/downtime alert.
        // Flipping the state first closes that window: any concurrent read
        // of this deployment either sees the old state with all containers
        // still running (nothing exited yet to alert on) or sees "paused"
        // once anything might be mid-stop.
        //
        // That alone still leaves a plain check-then-write race against a
        // concurrent outage check in flight: it can read "not paused" a
        // moment before this write commits, then create an incident for a
        // deployment that's paused by the time the incident actually lands.
        // Closing that requires more than moving this write earlier — the
        // check and the write need to serialize. Take the same Postgres
        // advisory lock, keyed on the environment, that
        // `OutageDetectionService::handle_outage_event` takes around its
        // final live pause re-check + incident insert: whichever side gets
        // the lock first commits (or observes "paused" and bails) before the
        // other proceeds, so no unpaused-read can ever precede this write
        // without the write also being visible to it.
        let txn = self.db.begin().await?;
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT pg_advisory_xact_lock($1)",
            [sea_orm::Value::BigInt(Some(environment_id as i64))],
        ))
        .await?;
        let mut active_deployment: deployments::ActiveModel = deployment.into();
        active_deployment.state = Set("paused".to_string());
        active_deployment.update(&txn).await?;

        // The lock above only serializes the incident *insert* against this
        // write — `OutageDetectionService` still sends the notification,
        // fires the alarm, and dispatches the workflow AFTER releasing the
        // lock, since those are external I/O (webhook/email delivery, job
        // queue send) that can't reasonably sit inside a DB transaction. A
        // pause landing in that specific gap would otherwise still produce
        // an alert for an incident that's already stale by the time it goes
        // out. Rather than trying to also lock out external I/O (which a DB
        // lock structurally can't do), make this side of the race
        // proactive: while still holding the lock, resolve any incident for
        // this environment that's still open. `handle_outage_event`
        // re-reads the incident's own status immediately before each side
        // effect (see its comment), so a resolve that lands here — even a
        // moment after that incident was created — is what that re-read is
        // watching for.
        status_incidents::Entity::update_many()
            .col_expr(
                status_incidents::Column::Status,
                sea_orm::sea_query::Expr::value("resolved"),
            )
            .col_expr(
                status_incidents::Column::ResolvedAt,
                sea_orm::sea_query::Expr::value(chrono::Utc::now()),
            )
            .filter(status_incidents::Column::EnvironmentId.eq(environment_id))
            .filter(status_incidents::Column::Status.ne("resolved"))
            .exec(&txn)
            .await?;

        txn.commit().await?;

        // Stop and remove all containers for this deployment
        let containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        // Stop (but do not remove) each container. Keeping the Docker
        // container object around — rather than force-removing it, as this
        // used to do — is what makes `resume_deployment` able to bring the
        // exact same containers back with a plain `docker start` instead of
        // trying to "unpause" containers that no longer exist (see the fix
        // note on `resume_deployment` below). The route table additionally
        // stops sending live traffic to any container whose `status` isn't
        // "running" (see `route_table::load_routes`), so a stopped-but-not-
        // removed container is just as inert from the outside as a removed
        // one, without sacrificing resumability.
        // Best-effort like the `stop_container` call above: the deployment
        // row is already committed as "paused", so aborting this loop on a
        // single container's DB write failure would strand the *remaining*
        // containers untouched (still "running" in the DB, never even
        // asked to stop) and skip the route-table reload below, while the
        // deployment stays paused indefinitely. Warn and keep going so one
        // failure can't silently drop the rest of the pause.
        for container in containers {
            let container_id = container.container_id.clone();
            if let Err(e) = self.deployer.stop_container(&container_id).await {
                warn!(
                    "Failed to stop container {} during deployment pause: {}",
                    container_id, e
                );
            }

            // Retry the DB write: a container whose status never makes it to
            // "stopped" keeps being treated as routable by
            // `route_table::load_routes` even though we just told Docker to
            // stop it — retrying absorbs the transient connection blips that
            // are the realistic cause of a single UPDATE failing right after
            // the read and the deployment-state write above it both
            // succeeded, so routes don't go stale on something recoverable.
            let retry = temps_core::retry::RetryConfig::new(3)
                .with_base_delay(std::time::Duration::from_millis(100))
                .with_max_delay(std::time::Duration::from_secs(2));
            let update_result = retry
                .retry(|| async {
                    let active_container = deployment_containers::ActiveModel {
                        status: Set(Some("stopped".to_string())),
                        ..deployment_containers::ActiveModel::from(container.clone())
                    };
                    active_container.update(self.db.as_ref()).await
                })
                .await;
            if let Err(e) = update_result {
                warn!(
                    "Failed to persist stopped status for container {} during deployment pause \
                     after retrying: {} — the route table may still treat it as routable until \
                     the next successful status update",
                    container_id, e
                );
            }
        }

        // Force an in-process route-table reload (same mechanism
        // `mark_deployment_complete.rs` uses after a normal deploy — see its
        // comment for why this is needed in addition to PG NOTIFY). Nothing
        // else about a pause touches `environments` or `projects`, which are
        // the only tables with a NOTIFY trigger wired up (see
        // `m20251209_000001_add_environments_route_trigger.rs` /
        // `m20250205_000003_add_projects_route_trigger.rs`) — a bare
        // `deployment_containers` status UPDATE fires no trigger at all. So
        // without this, the proxy's cached peer table keeps the container's
        // OLD (still "valid-looking") address indefinitely and only
        // discovers the pause when the next unrelated route change happens
        // to reload it, in the meantime returning "upstream connection
        // refused" instead of the intended "not currently serving" state.
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
                "Failed to publish in-process ForceRouteReload after pausing deployment {}: {} \
                 — falling back to the next PG NOTIFY-triggered reload",
                deployment_id, e
            );
        }

        info!(
            "Successfully paused deployment {}: stopped all containers",
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

        // `pause_deployment` stops (not removes) containers, so bring them
        // back with a plain `docker start` on the same container id/name —
        // not `resume_container` (Docker's `unpause`/cgroup-freeze reverse).
        // `unpause` only undoes a genuine `docker pause`, which nothing in
        // this codebase's real pause path ever calls; using it here against
        // a merely-stopped container always failed ("container is not
        // paused"), so resume could never actually succeed after a real
        // pause.
        for container in containers {
            self.deployer
                .start_container(&container.container_id)
                .await
                .map_err(|e| {
                    DeploymentError::Other(format!("Failed to resume container: {}", e))
                })?;

            // Update container status
            let mut active_container: deployment_containers::ActiveModel = container.into();
            active_container.status = Set(Some("running".to_string()));
            active_container.update(self.db.as_ref()).await?;
        }

        let environment_id = deployment.environment_id;

        // Update deployment state to "deployed"
        let mut active_deployment: deployments::ActiveModel = deployment.into();
        active_deployment.state = Set("deployed".to_string());
        active_deployment.update(self.db.as_ref()).await?;

        // See the matching comment in `pause_deployment`: a container-status
        // UPDATE fires no DB trigger, so force an in-process reload rather
        // than leaving the proxy's cached peer table to notice the resume
        // only whenever some unrelated route change happens to trigger one.
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
                "Failed to publish in-process ForceRouteReload after resuming deployment {}: {} \
                 — falling back to the next PG NOTIFY-triggered reload",
                deployment_id, e
            );
        }

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
        Ok(deployment_url_from_settings(
            &settings,
            self.config_service.proxy_port(),
            deployment_slug,
        ))
    }

    pub async fn compute_environment_url(&self, env_subdomain: &str) -> anyhow::Result<String> {
        let settings = self.config_service.get_settings().await.unwrap_or_default();

        let domain = PublicHostnameStrategy::Standard
            .environment_hostname(&settings.preview_domain, env_subdomain);

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
            // No external_url: the public port IS the proxy listener port from
            // the Rust server config (e.g. :8080 on a local instance). Without
            // this the URL drops to :80 and is unreachable on a non-standard
            // port. `proxy_port()` is the single source of truth.
            ("http", Some(self.config_service.proxy_port()))
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

        // Get pipeline id from deployment
        let deployment = deployments::Entity::find_by_id(deployment_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!("Deployment {} not found", deployment_id))
            })?;

        let deployment_label = deployment.id.to_string();
        let domain = PublicHostnameStrategy::Standard.project_deployment_hostname(
            &settings.preview_domain,
            &project.slug,
            &environment.slug,
            &deployment_label,
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

    /// Get all jobs for a deployment owned by the requested project.
    ///
    /// The project constraint is part of this service method rather than only
    /// an HTTP guard because deployment IDs are globally enumerable and this
    /// result includes sensitive workflow metadata.
    pub async fn get_deployment_jobs(
        &self,
        project_id: i32,
        deployment_id: i32,
    ) -> Result<Vec<temps_entities::deployment_jobs::Model>, DeploymentError> {
        use temps_entities::deployment_jobs;

        let deployment_exists = deployments::Entity::find_by_id(deployment_id)
            .filter(deployments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await
            .map_err(|e| DeploymentError::DatabaseError {
                reason: e.to_string(),
            })?
            .is_some();

        if !deployment_exists {
            return Err(DeploymentError::NotFound(format!(
                "deployment {} for project {} not found",
                deployment_id, project_id
            )));
        }

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

    /// The project's git host reference (owner/repo + branch) for a
    /// deployment, but ONLY when the repo is public -- returns `None` for a
    /// private repo or a project with no git connection at all (e.g. a
    /// manual/CLI upload). Callers that surface this externally (e.g. in a
    /// GitHub issue template on a public repo) must never see a private
    /// repo's URL, so the `is_public_repo` check happens here rather than
    /// being left to each caller to remember.
    pub async fn get_public_repo_reference(
        &self,
        project_id: i32,
        deployment_id: i32,
    ) -> Result<Option<RepoReference>, DeploymentError> {
        use temps_entities::projects;

        let deployment = deployments::Entity::find_by_id(deployment_id)
            .filter(deployments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await
            .map_err(|e| DeploymentError::DatabaseError {
                reason: e.to_string(),
            })?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!(
                    "deployment {} for project {} not found",
                    deployment_id, project_id
                ))
            })?;

        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| DeploymentError::DatabaseError {
                reason: e.to_string(),
            })?
            .ok_or_else(|| DeploymentError::NotFound(format!("project {project_id} not found")))?;

        if !project.is_public_repo {
            return Ok(None);
        }

        Ok(Some(RepoReference {
            owner: project.repo_owner,
            repo: project.repo_name,
            branch: deployment.branch_ref.unwrap_or(project.main_branch),
        }))
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

    /// Cancel all active deployments for an environment before its containers
    /// are removed by `DeploymentContainerCleaner`.
    pub async fn cancel_all_environment_deployments(
        &self,
        environment_id: i32,
    ) -> Result<u64, DeploymentError> {
        self.cancel_deployments_for_deletion(None, Some(environment_id), "Environment deleted")
            .await
    }

    pub async fn cancel_all_project_deployments(
        &self,
        project_id: i32,
    ) -> Result<u64, DeploymentError> {
        self.cancel_deployments_for_deletion(Some(project_id), None, "Project deleted")
            .await
    }

    async fn cancel_deployments_for_deletion(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        reason: &str,
    ) -> Result<u64, DeploymentError> {
        use temps_entities::{deployment_jobs, types::JobStatus};

        let mut query = deployments::Entity::find().filter(
            Condition::all()
                .add(deployments::Column::State.ne("cancelled"))
                .add(deployments::Column::State.ne("completed"))
                .add(deployments::Column::State.ne("deployed"))
                .add(deployments::Column::State.ne("failed"))
                .add(deployments::Column::State.ne("paused"))
                .add(deployments::Column::State.ne("stopped")),
        );
        if let Some(project_id) = project_id {
            query = query.filter(deployments::Column::ProjectId.eq(project_id));
        }
        if let Some(environment_id) = environment_id {
            query = query.filter(deployments::Column::EnvironmentId.eq(environment_id));
        }

        let active_deployments = query.all(self.db.as_ref()).await?;
        let count = active_deployments.len() as u64;

        for deployment in active_deployments {
            let running_jobs = deployment_jobs::Entity::find()
                .filter(deployment_jobs::Column::DeploymentId.eq(deployment.id))
                .filter(deployment_jobs::Column::Status.eq(JobStatus::Running))
                .all(self.db.as_ref())
                .await?;

            for job in running_jobs {
                let message = format!(
                    "DEPLOYMENT CANCELLED: {reason} - Job '{}' is being terminated",
                    job.name
                );
                if let Err(error) = self
                    .log_service
                    .append_structured_log(&job.log_id, temps_logs::LogLevel::Error, &message)
                    .await
                {
                    warn!(
                        deployment_id = deployment.id,
                        job_log_id = %job.log_id,
                        %error,
                        "Failed to append deletion cancellation to deployment job log"
                    );
                }
            }

            let deployment_id = deployment.id;
            let mut active_deployment: deployments::ActiveModel = deployment.into();
            active_deployment.state = Set("cancelled".to_string());
            active_deployment.cancelled_reason = Set(Some(reason.to_string()));
            active_deployment.finished_at = Set(Some(chrono::Utc::now()));
            active_deployment.updated_at = Set(chrono::Utc::now());
            active_deployment
                .update(self.db.as_ref())
                .await
                .map_err(|error| DeploymentError::DatabaseError {
                    reason: format!(
                        "Failed to cancel deployment {deployment_id} before owner deletion: {error}"
                    ),
                })?;
        }

        info!(
            ?project_id,
            ?environment_id,
            count,
            "Cancelled deployments before owner deletion"
        );
        Ok(count)
    }

    /// Remove every uploaded archive recorded for a project before deletion.
    /// Runtime containers are handled by `DeploymentContainerCleaner`.
    pub async fn cleanup_project_archives(&self, project_id: i32) -> Result<u64, DeploymentError> {
        let data_dir = self.config_service.data_dir();
        let source_archives = temps_entities::source_bundles::Entity::find()
            .filter(temps_entities::source_bundles::Column::ProjectId.eq(project_id))
            .all(self.db.as_ref())
            .await?;
        let static_archives = temps_entities::static_bundles::Entity::find()
            .filter(temps_entities::static_bundles::Column::ProjectId.eq(project_id))
            .all(self.db.as_ref())
            .await?;
        let archive_paths: Vec<String> = source_archives
            .into_iter()
            .map(|bundle| bundle.archive_path)
            .chain(static_archives.into_iter().map(|bundle| bundle.blob_path))
            .collect();
        let removed = archive_paths.len() as u64;
        for relative_path in archive_paths {
            let archive_path =
                confined_archive_path(&data_dir, &relative_path).map_err(|error| {
                    DeploymentError::InvalidBundlePath {
                        path: relative_path.clone(),
                        reason: format!("stored path for project {project_id} is invalid: {error}"),
                    }
                })?;
            match tokio::fs::remove_file(&archive_path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(DeploymentError::Other(format!(
                        "Failed to remove archive '{}' for project {}: {}",
                        archive_path.display(),
                        project_id,
                        error
                    )));
                }
            }
        }

        info!(
            "Removed {} uploaded archive(s) before deleting project {}",
            removed, project_id
        );
        Ok(removed)
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

        // Anonymous telemetry: this is the explicit "user clicked Cancel"
        // path (the other emission site, in WorkflowExecutionService, only
        // fires as a fallback when the executor detects a "cancelled" error
        // without this method having already set the state — see that
        // file's comment). Deliberately NOT emitted from the supersede
        // (cancel_in_flight_deployments) or bulk-shutdown
        // (cancel_running_deployments) paths, since those fire automatically
        // on every push / restart and would swamp the funnel signal with
        // non-user-initiated noise.
        let template_provenance = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten()
            .and_then(|project| project.template_slug);
        self.telemetry().report(
            temps_core::telemetry::TelemetryEvent::new(
                temps_core::telemetry::TelemetryEventKind::DeployCancelled,
            )
            .with("trigger", "user")
            .with_template_provenance(template_provenance.as_deref()),
        );

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
    async fn cancel_all_project_deployments(
        &self,
        project_id: i32,
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        self.cancel_all_project_deployments(project_id)
            .await
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)
    }

    async fn cancel_all_environment_deployments(
        &self,
        environment_id: i32,
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        self.cancel_all_environment_deployments(environment_id)
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    }
}

#[async_trait::async_trait]
impl temps_core::ProjectArchiveCleaner for DeploymentService {
    async fn cleanup_project_archives(
        &self,
        project_id: i32,
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        self.cleanup_project_archives(project_id)
            .await
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)
    }
}

#[async_trait::async_trait]
impl temps_core::DeploymentContainerCleaner for DeploymentService {
    async fn cleanup_project_containers(
        &self,
        project_id: i32,
    ) -> Result<u64, temps_core::ContainerCleanupError> {
        self.cleanup_containers(project_id, None).await
    }

    async fn cleanup_environment_containers(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<u64, temps_core::ContainerCleanupError> {
        self.cleanup_containers(project_id, Some(environment_id))
            .await
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

    #[test]
    fn archive_cleanup_paths_are_lexically_confined() {
        let root = std::path::Path::new("/var/lib/temps");
        assert_eq!(
            confined_archive_path(root, "source-bundles/archive.zip").unwrap(),
            root.join("source-bundles/archive.zip")
        );
        for path in [
            "../archive.zip",
            "source-bundles/../../archive.zip",
            "/tmp/archive.zip",
        ] {
            assert!(matches!(
                confined_archive_path(root, path),
                Err(DeploymentError::InvalidBundlePath { .. })
            ));
        }
    }

    #[test]
    fn complete_asset_origin_stays_canonical_across_reuse_hops() {
        let first_reuse_context = serde_json::json!({
            "source_deployment_id": 10,
            "source_environment_id": 20,
            "source_deployment_slug": "original-build",
        });

        let origin =
            complete_deployment_asset_origin(30, 40, "first-promotion", Some(&first_reuse_context))
                .expect("complete reuse metadata should resolve without a database lookup");
        assert_eq!(
            origin,
            DeploymentAssetOrigin {
                deployment_id: 10,
                environment_id: 20,
                slug: "original-build".to_string(),
            }
        );

        let second_reuse_context = serde_json::json!({
            "source_deployment_id": origin.deployment_id,
            "source_environment_id": origin.environment_id,
            "source_deployment_slug": origin.slug.clone(),
        });
        assert_eq!(
            complete_deployment_asset_origin(
                50,
                60,
                "second-promotion",
                Some(&second_reuse_context),
            ),
            Some(origin)
        );
    }
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

    #[tokio::test]
    async fn legacy_asset_origin_walks_partial_reuse_metadata_to_original_build() {
        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                println!("Test database not available, skipping: {error}");
                return;
            }
        };
        let db = test_db.connection_arc().clone();
        let (project, environment, original) = setup_test_data(&db)
            .await
            .expect("create deployment fixtures");

        let first_reuse = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("legacy-promotion".to_string()),
            state: Set("deployed".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            context_vars: Set(Some(serde_json::json!({
                "trigger": "promotion",
                "source_deployment_id": original.id,
                "source_environment_id": original.environment_id,
            }))),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert legacy promotion");

        let second_reuse = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("legacy-rollback".to_string()),
            state: Set("deployed".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            context_vars: Set(Some(serde_json::json!({
                "trigger": "rollback",
                "source_deployment_id": first_reuse.id,
            }))),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert legacy rollback");

        let origin = deployment_asset_origin(db.as_ref(), &second_reuse)
            .await
            .expect("legacy reuse metadata should resolve");

        assert_eq!(origin.deployment_id, original.id);
        assert_eq!(origin.environment_id, original.environment_id);
        assert_eq!(origin.slug, original.slug);
    }

    #[tokio::test]
    async fn latest_deployment_media_keeps_current_media_and_reports_latest_attempt() {
        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                println!("Test database not available, skipping: {error}");
                return;
            }
        };
        let db = test_db.connection_arc().clone();
        let (project, environment, old_deployment) = setup_test_data(&db)
            .await
            .expect("create deployment fixtures");
        let service = create_deployment_service_for_test(db.clone());
        let status_without_media = service
            .get_latest_deployment_media(&[project.id])
            .await
            .expect("query a latest attempt without current or historical media");
        assert_eq!(status_without_media.len(), 1);
        assert_eq!(status_without_media[0].latest_attempt_status, "deployed");
        assert_eq!(status_without_media[0].url, None);
        assert_eq!(status_without_media[0].screenshot_location, None);

        let old_screenshot = "screenshots/old.webp".to_string();
        let mut old: deployments::ActiveModel = old_deployment.into();
        old.screenshot_location = Set(Some(old_screenshot));
        old.created_at = Set(Utc::now() - chrono::Duration::hours(1));
        old.update(db.as_ref())
            .await
            .expect("update old deployment");

        let newest_slug = "newest-deployment".to_string();
        let newest_screenshot = "screenshots/newest.webp".to_string();
        let newest = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set(newest_slug.clone()),
            state: Set("completed".to_string()),
            screenshot_location: Set(Some(newest_screenshot.clone())),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert newest deployment");
        let mut environment: environments::ActiveModel = environment.into();
        environment.current_deployment_id = Set(Some(newest.id));
        let environment = environment
            .update(db.as_ref())
            .await
            .expect("set current deployment");

        let media = service
            .get_latest_deployment_media(&[project.id, i32::MAX])
            .await
            .expect("query latest deployment media");

        assert_eq!(media.len(), 1);
        assert_eq!(media[0].project_id, project.id);
        assert_eq!(media[0].latest_attempt_status, "completed");
        assert_eq!(
            media[0].screenshot_location.as_deref(),
            Some(newest_screenshot.as_str())
        );
        assert!(media[0]
            .url
            .as_deref()
            .is_some_and(|url| url.contains(&newest_slug)));

        let failed_attempt = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("failed-attempt".to_string()),
            state: Set("failed".to_string()),
            screenshot_location: Set(None),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            created_at: Set(Utc::now() + chrono::Duration::seconds(1)),
            updated_at: Set(Utc::now() + chrono::Duration::seconds(1)),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert failed latest attempt");
        let media_after_failure = service
            .get_latest_deployment_media(&[project.id])
            .await
            .expect("query current media after a failed attempt");
        assert_eq!(media_after_failure[0].latest_attempt_status, "failed");
        assert_eq!(
            media_after_failure[0].screenshot_location.as_deref(),
            Some(newest_screenshot.as_str())
        );
        assert!(media_after_failure[0]
            .url
            .as_deref()
            .is_some_and(|url| url.contains(&newest_slug)));
        assert_ne!(failed_attempt.id, newest.id);

        let mut environment: environments::ActiveModel = environment.into();
        environment.current_deployment_id = Set(None);
        environment
            .update(db.as_ref())
            .await
            .expect("clear current deployment");
        let historical_media = service
            .get_latest_deployment_media(&[project.id])
            .await
            .expect("query historical screenshot fallback");
        assert_eq!(historical_media.len(), 1);
        assert_eq!(historical_media[0].latest_attempt_status, "failed");
        assert_eq!(historical_media[0].url, None);
        assert_eq!(
            historical_media[0].screenshot_location.as_deref(),
            Some(newest_screenshot.as_str())
        );
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

    fn spawn_test_readiness_proxy() -> String {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind rollback readiness proxy");
        listener
            .set_nonblocking(true)
            .expect("make rollback readiness proxy nonblocking");
        let address = listener.local_addr().expect("read readiness proxy address");
        let listener = tokio::net::TcpListener::from_std(listener)
            .expect("create async rollback readiness proxy");
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut request = [0_u8; 1024];
                    let _ = stream.read(&mut request).await;
                    let _ = stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .await;
                });
            }
        });
        address.to_string()
    }

    fn create_deployment_service_for_test(
        db: Arc<temps_database::DbConnection>,
    ) -> DeploymentService {
        // Create mock log service
        let log_service = Arc::new(temps_logs::LogService::new(std::env::temp_dir()));

        // Create a minimal real config service for testing
        // We need to provide the database URL that the test database is using
        let test_db_url = "postgresql://test_user:test_password@localhost:5432/test_db";
        let proxy_address = spawn_test_readiness_proxy();
        let readiness_host_port = proxy_address
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse::<u16>().ok())
            .expect("read test readiness proxy port");
        let server_config = Arc::new(
            temps_config::ServerConfig::new(
                proxy_address,
                test_db_url.to_string(),
                None,
                Some("127.0.0.1:3001".to_string()),
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
        deployer.expect_deploy_container().returning(move |_| {
            Ok(temps_deployer::DeployResult {
                container_id: "test-container".to_string(),
                container_name: "test-container".to_string(),
                container_port: 3000,
                host_port: readiness_host_port,
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
            telemetry: std::sync::OnceLock::new(),
            env_resolver: std::sync::OnceLock::new(),
            compose_executor: std::sync::OnceLock::new(),
        }
    }

    async fn configure_test_service_for_http_readiness(
        service: &DeploymentService,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut settings = service.config_service.get_settings().await?;
        settings.external_url = Some("http://temps-test.local".to_string());
        service.config_service.update_settings(settings).await?;
        Ok(())
    }

    /// Stub `get_container_info` so the runtime ownership check in
    /// `cleanup_project_containers` sees a container whose labels match the
    /// project/environment being deleted.
    ///
    /// Cleanup refuses to remove a container that is not provably ours, so
    /// every cleanup test has to present the labels a real managed container
    /// would carry — otherwise the test fails on the guard rather than on the
    /// behaviour it is actually asserting.
    fn expect_owned_container_info(
        deployer: &mut MockContainerDeployer,
        project_id: i32,
        environment_id: i32,
    ) {
        deployer.expect_get_container_info().returning(move |id| {
            Ok(temps_deployer::ContainerInfo {
                container_id: id.to_string(),
                container_name: id.to_string(),
                image_name: "nginx:latest".to_string(),
                created_at: Utc::now(),
                ports: vec![],
                environment_vars: std::collections::HashMap::new(),
                status: temps_deployer::ContainerStatus::Running,
                restart_count: Some(0),
                labels: std::collections::HashMap::from([
                    ("sh.temps.managed".to_string(), "true".to_string()),
                    ("sh.temps.project_id".to_string(), project_id.to_string()),
                    (
                        "sh.temps.environment".to_string(),
                        environment_id.to_string(),
                    ),
                ]),
                ..Default::default()
            })
        });
    }

    fn create_cleanup_service_for_test(
        db: Arc<temps_database::DbConnection>,
        deployer: Arc<dyn temps_deployer::ContainerDeployer>,
    ) -> DeploymentService {
        let server_config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:8080".to_string(),
                "postgresql://test_user:test_password@localhost:5432/test_db".to_string(),
                None,
                None,
            )
            .expect("create test server config"),
        );
        let config_service = Arc::new(temps_config::ConfigService::new(server_config, db.clone()));
        let (queue_service, _receiver) =
            temps_queue::BroadcastQueueService::create_job_queue_arc_with_receiver(8);
        let docker = Arc::new(
            bollard::Docker::connect_with_local_defaults().expect("create Docker client config"),
        );

        DeploymentService {
            db,
            log_service: Arc::new(temps_logs::LogService::new(std::env::temp_dir())),
            config_service,
            queue_service,
            docker_log_service: Arc::new(temps_logs::DockerLogService::new(docker)),
            deployer,
            encryption_service: create_test_encryption_service(),
            telemetry: std::sync::OnceLock::new(),
            env_resolver: std::sync::OnceLock::new(),
            compose_executor: std::sync::OnceLock::new(),
        }
    }

    async fn database_integration_tests_available() -> bool {
        std::env::var_os("TEMPS_TEST_DATABASE_URL").is_some()
            || tokio::process::Command::new("docker")
                .arg("info")
                .output()
                .await
                .map(|output| output.status.success())
                .unwrap_or(false)
    }

    #[tokio::test]
    async fn cleanup_project_containers_removes_container_before_database_cascade(
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !database_integration_tests_available().await {
            eprintln!("Docker unavailable; skipping project container cleanup integration test");
            return Ok(());
        }

        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let (project, environment, _deployment, container) = setup_test_deployment(&db).await?;

        let expected_container_id = container.container_id.clone();
        let mut deployer = MockContainerDeployer::new();
        deployer.expect_list_containers().returning(|| Ok(vec![]));
        expect_owned_container_info(&mut deployer, project.id, environment.id);
        deployer
            .expect_remove_container()
            .withf(move |container_id| container_id == expected_container_id)
            .times(1)
            .returning(|_| Ok(()));
        let service = create_cleanup_service_for_test(db.clone(), Arc::new(deployer));

        let removed = temps_core::DeploymentContainerCleaner::cleanup_project_containers(
            &service, project.id,
        )
        .await?;
        assert_eq!(removed, 1);

        let cleaned = deployment_containers::Entity::find_by_id(container.id)
            .one(db.as_ref())
            .await?
            .expect("container cleanup record remains until project cascade");
        assert_eq!(cleaned.status.as_deref(), Some("removed"));
        assert!(cleaned.deleted_at.is_some());

        projects::Entity::delete_by_id(project.id)
            .exec(db.as_ref())
            .await?;
        assert!(
            deployment_containers::Entity::find_by_id(container.id)
                .one(db.as_ref())
                .await?
                .is_none(),
            "the database cascade must happen only after external cleanup succeeds"
        );

        Ok(())
    }

    #[tokio::test]
    async fn ambiguous_cleanup_failure_keeps_container_fenced_for_retry(
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !database_integration_tests_available().await {
            eprintln!("Docker unavailable; skipping cleanup failure integration test");
            return Ok(());
        }

        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let (project, environment, _deployment, container) = setup_test_deployment(&db).await?;

        let mut deployer = MockContainerDeployer::new();
        deployer.expect_list_containers().returning(|| Ok(vec![]));
        expect_owned_container_info(&mut deployer, project.id, environment.id);
        deployer.expect_remove_container().times(1).returning(|_| {
            Err(temps_deployer::DeployerError::NetworkError(
                "worker unavailable".to_string(),
            ))
        });
        let service = create_cleanup_service_for_test(db.clone(), Arc::new(deployer));

        let result = temps_core::DeploymentContainerCleaner::cleanup_project_containers(
            &service, project.id,
        )
        .await;
        assert!(matches!(
            result,
            Err(temps_core::ContainerCleanupError::Removal { .. })
        ));

        let fenced = deployment_containers::Entity::find_by_id(container.id)
            .one(db.as_ref())
            .await?
            .expect("failed cleanup must preserve the container record");
        assert_eq!(fenced.status.as_deref(), Some("removing"));
        assert!(fenced.deleted_at.is_some());
        assert!(
            projects::Entity::find_by_id(project.id)
                .one(db.as_ref())
                .await?
                .is_some(),
            "the project must remain recoverable after container cleanup fails"
        );

        Ok(())
    }

    /// The runtime ownership guard is the last line of defence against
    /// deleting a container that merely *recorded* a matching id: if the
    /// container Docker actually has under that id belongs to someone else,
    /// cleanup must refuse to touch it rather than remove another tenant's
    /// workload.
    #[tokio::test]
    async fn cleanup_refuses_to_remove_a_container_owned_by_another_project(
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !database_integration_tests_available().await {
            eprintln!("Docker unavailable; skipping cleanup ownership integration test");
            return Ok(());
        }

        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let (project, environment, _deployment, container) = setup_test_deployment(&db).await?;

        let mut deployer = MockContainerDeployer::new();
        deployer.expect_list_containers().returning(|| Ok(vec![]));
        // Same container id, but the running container claims a different project.
        expect_owned_container_info(&mut deployer, project.id + 1, environment.id);
        // The whole point: removal must never be attempted.
        deployer.expect_remove_container().never();
        let service = create_cleanup_service_for_test(db.clone(), Arc::new(deployer));

        let result = temps_core::DeploymentContainerCleaner::cleanup_project_containers(
            &service, project.id,
        )
        .await;
        assert!(
            matches!(
                result,
                Err(temps_core::ContainerCleanupError::Removal { .. })
            ),
            "cleanup must fail closed when runtime labels do not match, got {result:?}"
        );

        let preserved = deployment_containers::Entity::find_by_id(container.id)
            .one(db.as_ref())
            .await?
            .expect("a refused cleanup must preserve the container record");
        assert!(
            preserved.deleted_at.is_none(),
            "a container we refused to touch must not be marked deleted"
        );

        Ok(())
    }

    #[tokio::test]
    async fn interrupted_cleanup_is_idempotent_when_container_is_already_absent(
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !database_integration_tests_available().await {
            eprintln!("Docker unavailable; skipping interrupted cleanup integration test");
            return Ok(());
        }

        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let (project, environment, _deployment, container) = setup_test_deployment(&db).await?;

        let mut interrupted: deployment_containers::ActiveModel = container.clone().into();
        interrupted.status = Set(Some("removing".to_string()));
        interrupted.deleted_at = Set(Some(Utc::now()));
        interrupted.update(db.as_ref()).await?;

        let mut deployer = MockContainerDeployer::new();
        deployer.expect_list_containers().returning(|| Ok(vec![]));
        expect_owned_container_info(&mut deployer, project.id, environment.id);
        deployer.expect_remove_container().times(1).returning(|_| {
            Err(temps_deployer::DeployerError::ContainerNotFound(
                "already removed".to_string(),
            ))
        });
        let service = create_cleanup_service_for_test(db.clone(), Arc::new(deployer));

        let removed = temps_core::DeploymentContainerCleaner::cleanup_project_containers(
            &service, project.id,
        )
        .await?;
        assert_eq!(removed, 1);

        let finalized = deployment_containers::Entity::find_by_id(container.id)
            .one(db.as_ref())
            .await?
            .expect("cleanup record remains until project cascade");
        assert_eq!(finalized.status.as_deref(), Some("removed"));
        assert!(finalized.deleted_at.is_some());

        Ok(())
    }

    #[tokio::test]
    async fn cancel_all_project_deployments_is_project_scoped(
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !database_integration_tests_available().await {
            eprintln!("Docker unavailable; skipping project cancellation integration test");
            return Ok(());
        }

        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let (project, _environment, deployment, _container) = setup_test_deployment(&db).await?;
        let mut active: deployments::ActiveModel = deployment.clone().into();
        active.state = Set("running".to_string());
        active.update(db.as_ref()).await?;
        let stopped_deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(deployment.environment_id),
            state: Set("stopped".to_string()),
            slug: Set("stopped-deployment".to_string()),
            metadata: Set(Some(Default::default())),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        let other_project = projects::ActiveModel {
            name: Set("Other Project".to_string()),
            slug: Set("other-project".to_string()),
            repo_name: Set("other-repo".to_string()),
            repo_owner: Set("other-owner".to_string()),
            preset: Set(Preset::NextJs),
            main_branch: Set("main".to_string()),
            directory: Set("/".to_string()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
        let other_environment = environments::ActiveModel {
            project_id: Set(other_project.id),
            name: Set("Production".to_string()),
            slug: Set("other-prod".to_string()),
            host: Set("other.example.com".to_string()),
            upstreams: Set(UpstreamList::default()),
            subdomain: Set("other.example.com".to_string()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
        let other_deployment = deployments::ActiveModel {
            project_id: Set(other_project.id),
            environment_id: Set(other_environment.id),
            state: Set("running".to_string()),
            slug: Set("other-deployment".to_string()),
            metadata: Set(Some(Default::default())),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        let service = create_deployment_service_for_test(db.clone());
        let cancelled = service.cancel_all_project_deployments(project.id).await?;
        assert_eq!(cancelled, 1);

        let cancelled_deployment = deployments::Entity::find_by_id(deployment.id)
            .one(db.as_ref())
            .await?
            .expect("target deployment remains for history");
        assert_eq!(cancelled_deployment.state, "cancelled");
        assert_eq!(
            cancelled_deployment.cancelled_reason.as_deref(),
            Some("Project deleted")
        );

        let untouched = deployments::Entity::find_by_id(other_deployment.id)
            .one(db.as_ref())
            .await?
            .expect("other project deployment remains");
        assert_eq!(untouched.state, "running");
        let stopped = deployments::Entity::find_by_id(stopped_deployment.id)
            .one(db.as_ref())
            .await?
            .expect("stopped deployment remains for history");
        assert_eq!(stopped.state, "stopped");
        assert_ne!(project.id, other_project.id);

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

    // Regression coverage for the pause/resume Docker-op mismatch bug: the
    // two tests above use `setup_test_data`, which never inserts a
    // `deployment_containers` row, so the container loop inside
    // `pause_deployment`/`resume_deployment` never actually executes and
    // neither test can observe which Docker operation gets called. These
    // two variants use `setup_test_deployment` (which inserts one real
    // container) and pin down the *exact* deployer method invoked via a
    // narrowly-scoped mock, so a regression back to `pause_container`/
    // `resume_container` (the old, broken ops) — or simply forgetting to
    // touch the container at all — fails the test instead of passing it
    // silently.
    #[tokio::test]
    async fn test_pause_deployment_stops_real_container() -> Result<(), Box<dyn std::error::Error>>
    {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        let (_project, _environment, deployment, container) = setup_test_deployment(&db).await?;
        assert_eq!(container.status.as_deref(), Some("running"));

        // Only `stop_container` is wired up on this mock. If pause
        // regressed to calling `pause_container`/`resume_container`
        // instead, the mock has no expectation for that method and panics.
        let expected_container_id = container.container_id.clone();
        let mut deployer = MockContainerDeployer::new();
        deployer
            .expect_stop_container()
            .withf(move |id| id == expected_container_id)
            .times(1)
            .returning(|_| Ok(()));
        let deployer: Arc<dyn temps_deployer::ContainerDeployer> = Arc::new(deployer);

        let deployment_service = create_cleanup_service_for_test(db.clone(), deployer);

        deployment_service
            .pause_deployment(deployment.project_id, deployment.id)
            .await?;

        let updated_deployment = deployments::Entity::find_by_id(deployment.id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(updated_deployment.state, "paused");

        let updated_container = deployment_containers::Entity::find_by_id(container.id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(updated_container.status.as_deref(), Some("stopped"));

        Ok(())
    }

    #[tokio::test]
    async fn test_resume_deployment_starts_real_container() -> Result<(), Box<dyn std::error::Error>>
    {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        let (_project, _environment, deployment, container) = setup_test_deployment(&db).await?;

        // Simulate the post-pause state: deployment "paused", container
        // "stopped" but not removed (pause_deployment keeps the Docker
        // container object around rather than force-removing it).
        let mut active_deployment: deployments::ActiveModel = deployment.clone().into();
        active_deployment.state = Set("paused".to_string());
        let deployment = active_deployment.update(db.as_ref()).await?;

        let mut active_container: deployment_containers::ActiveModel = container.clone().into();
        active_container.status = Set(Some("stopped".to_string()));
        let container = active_container.update(db.as_ref()).await?;

        // Only `start_container` (a plain `docker start`) is wired up. If
        // resume regressed to calling `resume_container` (Docker's
        // unpause/cgroup-freeze reverse, which always fails against a
        // container that was merely stopped, not paused), the mock has no
        // expectation for that method and panics.
        let expected_container_id = container.container_id.clone();
        let mut deployer = MockContainerDeployer::new();
        deployer
            .expect_start_container()
            .withf(move |id| id == expected_container_id)
            .times(1)
            .returning(|_| Ok(()));
        let deployer: Arc<dyn temps_deployer::ContainerDeployer> = Arc::new(deployer);

        let deployment_service = create_cleanup_service_for_test(db.clone(), deployer);

        deployment_service
            .resume_deployment(deployment.project_id, deployment.id)
            .await?;

        let updated_deployment = deployments::Entity::find_by_id(deployment.id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(updated_deployment.state, "deployed");

        let updated_container = deployment_containers::Entity::find_by_id(container.id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(updated_container.status.as_deref(), Some("running"));

        Ok(())
    }

    #[tokio::test]
    async fn test_rollback_to_deployment() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Setup test data
        let (_project, mut environment, target_deployment) = setup_test_data(&db).await?;
        let expected_command = vec!["start".to_string(), "--optimized".to_string()];
        let expected_health_check_path = "/realms/master".to_string();
        let mut active_target: deployments::ActiveModel = target_deployment.into();
        active_target.metadata = Set(Some(temps_entities::deployments::DeploymentMetadata {
            command: Some(expected_command.clone()),
            health_check_path: Some(expected_health_check_path.clone()),
            ..Default::default()
        }));
        let target_deployment = active_target.update(db.as_ref()).await?;
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
        configure_test_service_for_http_readiness(&deployment_service).await?;

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
        assert_eq!(metadata.command, Some(expected_command));
        assert_eq!(
            metadata.health_check_path.as_deref(),
            Some(expected_health_check_path.as_str())
        );

        // Verify environment was updated to point to the NEW rollback deployment
        let updated_environment = environments::Entity::find_by_id(environment.id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(updated_environment.current_deployment_id, Some(result.id));

        Ok(())
    }

    #[tokio::test]
    async fn test_promote_deployment_preserves_image_runtime(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        let (project, _source_environment, source) = setup_test_data(&db).await?;
        let expected_command = vec!["start".to_string(), "--optimized".to_string()];
        let expected_health_check_path = "/realms/master".to_string();
        let mut active_source: deployments::ActiveModel = source.into();
        active_source.metadata = Set(Some(temps_entities::deployments::DeploymentMetadata {
            command: Some(expected_command.clone()),
            health_check_path: Some(expected_health_check_path.clone()),
            ..Default::default()
        }));
        let source = active_source.update(db.as_ref()).await?;

        let now = Utc::now();
        let target_environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Promotion Target".to_string()),
            slug: Set("promotion-target".to_string()),
            host: Set("promotion-target.example.com".to_string()),
            upstreams: Set(UpstreamList::default()),
            current_deployment_id: Set(None),
            subdomain: Set("promotion-target.example.com".to_string()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        let deployment_service = create_deployment_service_for_test(db.clone());
        configure_test_service_for_http_readiness(&deployment_service).await?;

        let promoted = deployment_service
            .promote_deployment(project.id, source.id, target_environment.id)
            .await?;

        let promoted_model = deployments::Entity::find_by_id(promoted.id)
            .one(db.as_ref())
            .await?
            .ok_or("promoted deployment was not persisted")?;
        let metadata = promoted_model
            .metadata
            .ok_or("promoted deployment metadata was not persisted")?;
        assert_eq!(metadata.command, Some(expected_command));
        assert_eq!(
            metadata.health_check_path.as_deref(),
            Some(expected_health_check_path.as_str())
        );

        Ok(())
    }

    /// When the target deployment carries a git commit on a git-sourced
    /// project AND the stored image is gone (pruned), rollback should rebuild
    /// from source (enqueue a GitPushEvent) rather than fail. We assert it does
    /// NOT take the image-reuse path: that path synchronously inserts a
    /// brand-new deployment row (different id) and flips the environment
    /// pointer. The rebuild path enqueues an async job, so within the test the
    /// only deployments present are the originals — no extra image-reuse row.
    #[tokio::test]
    async fn test_rollback_rebuilds_from_source_when_image_missing(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // setup_test_data creates a Git-source project (SourceType default).
        let (_project, _environment, target_deployment) = setup_test_data(&db).await?;

        // Give the target a real git commit so it's rebuildable from source.
        let mut active: deployments::ActiveModel = target_deployment.clone().into();
        active.commit_sha = Set(Some("abc1234deadbeef".to_string()));
        active.branch_ref = Set(Some("main".to_string()));
        let target_deployment = active.update(db.as_ref()).await?;

        let count_before = deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(target_deployment.project_id))
            .count(db.as_ref())
            .await?;

        // image_exists -> false simulates the nightly prune having removed the
        // target's image, so rollback must rebuild from source.
        let deployment_service = create_deployment_service_with_missing_image(db.clone());

        let result = deployment_service
            .rollback_to_deployment(target_deployment.project_id, target_deployment.id)
            .await?;

        // The image-reuse path would have inserted a new deployment row and
        // returned its (different) id. The rebuild path enqueues a job instead,
        // so no synchronous row is added and we get the target back as a
        // stand-in (the queued pipeline row isn't visible in-test).
        let count_after = deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(target_deployment.project_id))
            .count(db.as_ref())
            .await?;
        assert_eq!(
            count_before, count_after,
            "rebuild-from-source must not synchronously create an image-reuse deployment"
        );
        assert_eq!(
            result.id, target_deployment.id,
            "rebuild path returns the target as a stand-in while the job is queued"
        );

        Ok(())
    }

    /// When the target deployment carries a git commit on a git-sourced project
    /// AND the stored image is still in the local Docker cache (the common case
    /// — rolling back a recent deploy), rollback should REUSE that image rather
    /// than pay for a full rebuild from source. Reuse is near-instant and
    /// byte-identical to the deployment we're rolling back to. We assert it
    /// takes the image-reuse path: that path synchronously inserts a brand-new
    /// rollback deployment row (a different id from the target) and returns it.
    #[tokio::test]
    async fn test_rollback_reuses_local_image_for_git_projects(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // setup_test_data creates a Git-source project (SourceType default)
        // with a non-static preset (NextJs) and image_name "nginx:latest".
        let (_project, _environment, target_deployment) = setup_test_data(&db).await?;

        // Give the target a real git commit — without the fix this alone would
        // force a rebuild even though the image is sitting right here.
        let mut active: deployments::ActiveModel = target_deployment.clone().into();
        active.commit_sha = Set(Some("abc1234deadbeef".to_string()));
        active.branch_ref = Set(Some("main".to_string()));
        let target_deployment = active.update(db.as_ref()).await?;

        let count_before = deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(target_deployment.project_id))
            .count(db.as_ref())
            .await?;

        // Default test service: image_exists -> true (image present locally).
        let deployment_service = create_deployment_service_for_test(db.clone());
        configure_test_service_for_http_readiness(&deployment_service).await?;

        let result = deployment_service
            .rollback_to_deployment(target_deployment.project_id, target_deployment.id)
            .await?;

        // The image-reuse path synchronously inserts a fresh rollback row, so
        // the count grows and the returned id differs from the target's. (The
        // rebuild path would have left the count unchanged and returned the
        // target as a stand-in.)
        let count_after = deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(target_deployment.project_id))
            .count(db.as_ref())
            .await?;
        assert_eq!(
            count_before + 1,
            count_after,
            "image reuse must synchronously create a new rollback deployment"
        );
        assert_ne!(
            result.id, target_deployment.id,
            "image reuse returns the freshly-created rollback deployment, not the target"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_rollback_to_deployment_invalid_state() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Setup test data
        let (_project, environment, mut target_deployment) = setup_test_data(&db).await?;

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

        // A second, distinct invalid state must still be rejected too --
        // this isn't just re-asserting "failed" from above. Reuse the same
        // project/environment (rather than calling `setup_test_data` again,
        // which would collide on its hard-coded project slug) with a fresh
        // deployment row.
        let other_target = deployments::ActiveModel {
            project_id: Set(target_deployment.project_id),
            environment_id: Set(environment.id),
            slug: Set("test-deployment-creating".to_string()),
            state: Set("creating".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let other_target = other_target.insert(db.as_ref()).await?;

        let result = deployment_service
            .rollback_to_deployment(other_target.project_id, other_target.id)
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            DeploymentError::InvalidDeploymentState(msg) => {
                assert!(msg.contains("creating"));
            }
            e => panic!("Expected InvalidDeploymentState error, got: {:?}", e),
        }

        Ok(())
    }

    // Regression coverage for `valid_rollback_states` gaining "stopped":
    // the only pre-existing invalid-state test above only ever asserted on
    // "failed" being rejected, so a regression that dropped "stopped" from
    // the allow-list (reintroducing "Cannot rollback to deployment in
    // 'stopped' state" for the primary real-world rollback target -- a
    // superseded, previously-successful deployment) would pass every
    // existing test in this file.
    #[tokio::test]
    async fn test_rollback_to_deployment_accepts_stopped_state(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Setup test data
        let (_project, mut environment, target_deployment) = setup_test_data(&db).await?;
        setup_test_environment_variables(&db, target_deployment.project_id, environment.id).await?;

        // Create container for target deployment (required for rollback)
        let now = Utc::now();
        let target_container = deployment_containers::ActiveModel {
            deployment_id: Set(target_deployment.id),
            container_id: Set("container-rollback-stopped-target".to_string()),
            container_name: Set("app-rollback-stopped-target".to_string()),
            container_port: Set(8080),
            image_name: Set(Some("nginx:target".to_string())),
            status: Set(Some("stopped".to_string())),
            created_at: Set(now),
            deployed_at: Set(now),
            ..Default::default()
        };
        target_container.insert(db.as_ref()).await?;

        // This is the state `cancel_previous_deployments`/`teardown_deployment`
        // actually leave a superseded-but-successful deployment in -- the
        // primary real-world rollback target.
        let mut active_target: deployments::ActiveModel = target_deployment.into();
        active_target.state = Set("stopped".to_string());
        let target_deployment = active_target.update(db.as_ref()).await?;

        // Create current deployment that will be stopped by the rollback
        let current_deployment = deployments::ActiveModel {
            project_id: Set(target_deployment.project_id),
            environment_id: Set(environment.id),
            slug: Set("current-deployment-for-stopped-rollback".to_string()),
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

        let current_container = deployment_containers::ActiveModel {
            deployment_id: Set(current_deployment.id),
            container_id: Set("container-rollback-stopped-current".to_string()),
            container_name: Set("app-rollback-stopped-current".to_string()),
            container_port: Set(8080),
            image_name: Set(Some("nginx:current".to_string())),
            status: Set(Some("running".to_string())),
            created_at: Set(now),
            deployed_at: Set(now),
            ..Default::default()
        };
        current_container.insert(db.as_ref()).await?;

        let mut active_environment: environments::ActiveModel = environment.into();
        active_environment.current_deployment_id = Set(Some(current_deployment.id));
        environment = active_environment.update(db.as_ref()).await?;

        let deployment_service = create_deployment_service_for_test(db.clone());
        // Phase 2.75 probes the public URL over HTTP via the test proxy.
        // Without this the URL scheme defaults to "https", which causes the
        // TLS handshake to fail against the plain-HTTP test proxy and the
        // readiness gate times out rather than succeeding.
        configure_test_service_for_http_readiness(&deployment_service).await?;

        // Rollback to a "stopped" target must succeed, not bounce off the
        // InvalidDeploymentState guard.
        let result = deployment_service
            .rollback_to_deployment(target_deployment.project_id, target_deployment.id)
            .await?;

        assert_ne!(result.id, target_deployment.id);
        assert!(result.is_current);

        let updated_environment = environments::Entity::find_by_id(environment.id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(updated_environment.current_deployment_id, Some(result.id));

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
            telemetry: std::sync::OnceLock::new(),
            env_resolver: std::sync::OnceLock::new(),
            compose_executor: std::sync::OnceLock::new(),
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
    async fn retained_failed_compose_container_passes_log_ownership_lookup(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use temps_entities::deployment_containers;

        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let (project, environment, current_deployment) = setup_test_data(&db).await?;

        let mut active_environment: environments::ActiveModel = environment.clone().into();
        active_environment.current_deployment_id = Set(Some(current_deployment.id));
        active_environment.update(db.as_ref()).await?;

        let failed_deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("failed-compose-candidate".to_string()),
            state: Set("failed".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
        deployment_containers::ActiveModel {
            deployment_id: Set(failed_deployment.id),
            container_id: Set("retained-failed-container".to_string()),
            container_name: Set("temps-retained-app-1".to_string()),
            container_port: Set(80),
            image_name: Set(Some("nginx:alpine".to_string())),
            status: Set(Some("retained:running".to_string())),
            service_name: Set(Some("app".to_string())),
            created_at: Set(Utc::now()),
            deployed_at: Set(Utc::now()),
            ready_at: Set(None),
            deleted_at: Set(None),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        let deployment_service = create_deployment_service_for_test(db.clone());
        let result = deployment_service
            .get_container_logs_by_id(
                project.id,
                environment.id,
                "retained-failed-container".to_string(),
                ContainerLogParams {
                    start_date: None,
                    end_date: None,
                    tail: Some("10".to_string()),
                    timestamps: false,
                    follow: false,
                },
            )
            .await;

        assert!(
            !matches!(result, Err(DeploymentError::NotFound(_))),
            "a live retained container must pass project/environment ownership lookup even when another deployment is current"
        );

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
    async fn test_list_environment_container_history_filters_by_deployment_and_limits(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // First deployment/container comes from the shared fixture.
        let (project, environment, deployment_one, container_one) =
            setup_test_deployment(&db).await?;

        // A second deployment on the SAME environment, with three containers
        // that have all since been superseded by a later redeploy (deleted_at
        // set) — simulates an environment with a lot of replaced history.
        let deployment_two = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            state: Set("deployed".to_string()),
            slug: Set("test-deployment-two".to_string()),
            metadata: Set(Some(Default::default())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let deployment_two = deployment_two.insert(db.as_ref()).await?;

        let now = Utc::now();
        for (idx, container_id) in ["container-456", "container-789", "container-999"]
            .iter()
            .enumerate()
        {
            let container = deployment_containers::ActiveModel {
                deployment_id: Set(deployment_two.id),
                container_id: Set(container_id.to_string()),
                container_name: Set(format!("test-container-{}", idx + 2)),
                container_port: Set(8080),
                image_name: Set(Some("nginx:latest".to_string())),
                status: Set(Some("stopped".to_string())),
                created_at: Set(now + chrono::Duration::seconds(idx as i64 + 1)),
                deployed_at: Set(now + chrono::Duration::seconds(idx as i64 + 1)),
                deleted_at: Set(Some(now + chrono::Duration::seconds(idx as i64 + 10))),
                ..Default::default()
            };
            container.insert(db.as_ref()).await?;
        }

        let deployment_service = create_deployment_service_for_test(db.clone());

        // No filter, no limit override: sees every container across both
        // deployments (1 current + 3 replaced), and total_count matches.
        let (all, all_total) = deployment_service
            .list_environment_container_history(project.id, environment.id, None, None)
            .await?;
        assert_eq!(all.len(), 4);
        assert_eq!(all_total, 4);

        // Filtered to deployment_two: only its three (replaced) containers
        // come back, deployment_one's current container is excluded.
        let (filtered, filtered_total) = deployment_service
            .list_environment_container_history(
                project.id,
                environment.id,
                Some(deployment_two.id),
                None,
            )
            .await?;
        assert_eq!(filtered_total, 3);
        assert!(filtered
            .iter()
            .all(|c| c.deployment_id == deployment_two.id));
        assert!(!filtered.iter().any(|c| c.id == container_one.id));

        // limit=1 across all deployments: the single currently-running
        // container (container_one) is NEVER subject to the cap -- only
        // replaced containers are capped, so exactly 1 (of 3) replaced rows
        // joins it. total_count still reports the unfiltered total (4).
        let (limited, limited_total) = deployment_service
            .list_environment_container_history(project.id, environment.id, None, Some(1))
            .await?;
        assert_eq!(limited.len(), 2);
        assert_eq!(limited_total, 4);
        assert!(
            limited.iter().any(|c| c.id == container_one.id),
            "the running container must never be dropped by `limit`"
        );
        assert_eq!(
            limited.iter().filter(|c| c.deleted_at.is_some()).count(),
            1,
            "limit=1 should cap replaced containers to exactly 1"
        );

        // Filtering by a deployment ID from a different environment 404s
        // rather than silently returning nothing.
        let other_project = projects::ActiveModel {
            name: Set("Other Project".to_string()),
            slug: Set("other-project".to_string()),
            repo_name: Set("other-repo".to_string()),
            repo_owner: Set("other-owner".to_string()),
            preset: Set(Preset::NextJs),
            main_branch: Set("main".to_string()),
            directory: Set("/".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let other_project = other_project.insert(db.as_ref()).await?;
        let other_environment = environments::ActiveModel {
            project_id: Set(other_project.id),
            name: Set("Other".to_string()),
            slug: Set("other".to_string()),
            host: Set("other.example.com".to_string()),
            upstreams: Set(UpstreamList::default()),
            subdomain: Set("other.example.com".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let other_environment = other_environment.insert(db.as_ref()).await?;
        let result = deployment_service
            .list_environment_container_history(
                other_project.id,
                other_environment.id,
                Some(deployment_one.id),
                None,
            )
            .await;
        assert!(matches!(result, Err(DeploymentError::NotFound(_))));

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
        configure_test_service_for_http_readiness(&deployment_service).await?;

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
    async fn test_get_deployment_jobs_enforces_project_ownership(
    ) -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var_os("TEMPS_TEST_DATABASE_URL").is_none()
            && !tokio::process::Command::new("docker")
                .arg("info")
                .output()
                .await
                .map(|output| output.status.success())
                .unwrap_or(false)
        {
            eprintln!("Docker unavailable; skipping deployment ownership test");
            return Ok(());
        }
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let (_project, _environment, deployment) = setup_test_data(&db).await?;

        let job = temps_entities::deployment_jobs::ActiveModel {
            deployment_id: Set(deployment.id),
            job_id: Set("sensitive-build".to_string()),
            job_type: Set("BuildImageJob".to_string()),
            name: Set("Sensitive Build".to_string()),
            log_id: Set("sensitive-build-log".to_string()),
            status: Set(temps_entities::types::JobStatus::Success),
            job_config: Set(Some(serde_json::json!({
                "build_args": {"DATABASE_PASSWORD": "must-not-cross-projects"}
            }))),
            ..Default::default()
        };
        job.insert(db.as_ref()).await?;

        let service = create_deployment_service_for_test(db.clone());
        let own_jobs = service
            .get_deployment_jobs(deployment.project_id, deployment.id)
            .await?;
        assert_eq!(own_jobs.len(), 1);

        let foreign_result = service
            .get_deployment_jobs(deployment.project_id + 999, deployment.id)
            .await;
        assert!(matches!(foreign_result, Err(DeploymentError::NotFound(_))));

        Ok(())
    }

    #[tokio::test]
    async fn test_failure_report_preview_stops_at_failed_job_and_redacts_secrets(
    ) -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var_os("TEMPS_TEST_DATABASE_URL").is_none()
            && !tokio::process::Command::new("docker")
                .arg("info")
                .output()
                .await
                .map(|output| output.status.success())
                .unwrap_or(false)
        {
            eprintln!("Docker unavailable; skipping failure-report preview test");
            return Ok(());
        }
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let (_project, _environment, deployment) = setup_test_data(&db).await?;

        let enc = create_test_encryption_service();
        let log_base = std::env::temp_dir();
        let log_service = Arc::new(temps_logs::LogService::new(log_base.clone()));

        let unique = uuid::Uuid::new_v4().simple().to_string();
        let mut secrets_config = serde_json::Map::new();
        let mut secret_map = std::collections::HashMap::new();
        secret_map.insert("DB_PASSWORD".to_string(), "super-secret-value".to_string());
        crate::services::sensitive_envelope::write_sealed(
            &mut secrets_config,
            enc.as_ref(),
            "secrets",
            &secret_map,
        )?;

        let jobs = [
            (
                "download_repo",
                "Download Repo",
                0,
                temps_entities::types::JobStatus::Success,
                None,
                None,
            ),
            (
                "build_image",
                "Build Image",
                1,
                temps_entities::types::JobStatus::Failure,
                Some("build failed: exit code 1".to_string()),
                Some(serde_json::Value::Object(secrets_config)),
            ),
            (
                "deploy_container",
                "Deploy Container",
                2,
                temps_entities::types::JobStatus::Skipped,
                None,
                None,
            ),
        ];

        for (job_id, name, order, status, error_message, job_config) in jobs {
            let log_id = format!("{unique}-{job_id}");
            // `download_repo` deliberately never writes its log file, to
            // exercise a job that finished too fast to log anything (e.g.
            // PrepareSourceBundleJob) -- the preview must still succeed.
            if job_id != "download_repo" {
                tokio::fs::write(
                    log_base.join(format!("{log_id}.log")),
                    format!("log output for {job_id}, secret is super-secret-value\n"),
                )
                .await?;
            }

            temps_entities::deployment_jobs::ActiveModel {
                deployment_id: Set(deployment.id),
                job_id: Set(job_id.to_string()),
                job_type: Set(format!("{job_id}Job")),
                name: Set(name.to_string()),
                log_id: Set(log_id),
                status: Set(status),
                error_message: Set(error_message),
                job_config: Set(job_config),
                execution_order: Set(Some(order)),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await?;
        }

        let deployment_service = Arc::new(create_deployment_service_for_test(db.clone()));
        let failure_service =
            crate::services::FailureReportService::new(deployment_service, log_service, enc)?;

        let preview = failure_service
            .build_preview(deployment.project_id, deployment.id, "build_image")
            .await?;

        assert_eq!(
            preview.error_message.as_deref(),
            Some("build failed: exit code 1")
        );
        assert!(preview.redacted_log.contains("download_repo"));
        assert!(
            preview
                .redacted_log
                .contains("no log output for this stage"),
            "a job with no log file must degrade to a placeholder, not fail the whole preview"
        );
        assert!(preview.redacted_log.contains("build_image"));
        assert!(
            !preview.redacted_log.contains("deploy_container"),
            "trace must stop at the failed job, not include later jobs"
        );
        assert!(
            !preview.redacted_log.contains("super-secret-value"),
            "known secret value must be redacted from the trace"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_get_public_repo_reference_none_for_private_none_for_public(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        // setup_test_data's project doesn't set is_public_repo, so it defaults
        // to private -- must never leak repo_owner/repo_name into a public
        // GitHub issue template.
        let (project, _environment, deployment) = setup_test_data(&db).await?;
        let service = create_deployment_service_for_test(db.clone());

        let private_result = service
            .get_public_repo_reference(project.id, deployment.id)
            .await?;
        assert!(
            private_result.is_none(),
            "a private (or unlinked) repo must never be surfaced"
        );

        let mut project_update: projects::ActiveModel = project.clone().into();
        project_update.is_public_repo = Set(true);
        project_update.update(db.as_ref()).await?;

        let mut deployment_update: deployments::ActiveModel = deployment.clone().into();
        deployment_update.branch_ref = Set(Some("feat/some-branch".to_string()));
        deployment_update.update(db.as_ref()).await?;

        let public_result = service
            .get_public_repo_reference(project.id, deployment.id)
            .await?
            .expect("public repo must return a reference");
        assert_eq!(public_result.owner, "test-owner");
        assert_eq!(public_result.repo, "test-repo");
        assert_eq!(public_result.branch, "feat/some-branch");

        Ok(())
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

    #[test]
    fn rollback_runtime_uses_target_deployment_snapshot() {
        let snapshot = temps_entities::deployment_config::DeploymentConfigSnapshot {
            exposed_port: Some(8080),
            replicas: 3,
            cpu_request: Some(500_000),
            cpu_limit: Some(1_000_000),
            memory_request: Some(512),
            memory_limit: Some(1_536),
            ..Default::default()
        };

        let (port, replicas) =
            DeploymentService::rollback_snapshot_port_and_replicas(&snapshot, 42)
                .expect("valid deployment snapshot");
        let resources = DeploymentService::resource_usage_from_snapshot(&snapshot);

        assert_eq!(port, Some(8080));
        assert_eq!(replicas, 3);
        assert_eq!(resources.cpu_request.as_deref(), Some("500000u"));
        assert_eq!(resources.cpu_limit.as_deref(), Some("1000000u"));
        assert_eq!(resources.memory_request.as_deref(), Some("512Mi"));
        assert_eq!(resources.memory_limit.as_deref(), Some("1536Mi"));
    }

    #[test]
    fn rollback_rejects_invalid_target_snapshot_runtime() {
        let invalid_port = temps_entities::deployment_config::DeploymentConfigSnapshot {
            exposed_port: Some(70_000),
            ..Default::default()
        };
        assert!(matches!(
            DeploymentService::rollback_snapshot_port_and_replicas(&invalid_port, 42),
            Err(DeploymentError::InvalidDeploymentState(_))
        ));

        let invalid_replicas = temps_entities::deployment_config::DeploymentConfigSnapshot {
            replicas: 0,
            ..Default::default()
        };
        assert!(matches!(
            DeploymentService::rollback_snapshot_port_and_replicas(&invalid_replicas, 42),
            Err(DeploymentError::InvalidDeploymentState(_))
        ));
    }

    /// `stop_environment_containers` (pre-rollback cleanup) has the same
    /// deleted-before-stopped ordering requirement as
    /// `WorkflowExecutionService::teardown_previous_deployment`. Uses
    /// `block_in_place` to run a real DB check from inside the mock's
    /// `stop_container` expectation, which requires the multi-thread runtime.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_stop_environment_containers_marks_deleted_before_stopping_container(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        let (_project, environment, old_deployment) = setup_test_data(&db).await?;

        let container = deployment_containers::ActiveModel {
            deployment_id: Set(old_deployment.id),
            container_id: Set("old-env-container-1".to_string()),
            container_name: Set("old-env-container-1".to_string()),
            container_port: Set(3000),
            deployed_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        // Deployment state must be one of the active states
        // `stop_environment_containers` scans for; a nonexistent id is enough
        // for the "exclude current deployment" filter.
        let exclude_deployment_id = old_deployment.id + 1_000_000;

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

        let db_for_check = db.clone();
        let mut deployer = MockContainerDeployer::new();
        deployer
            .expect_stop_container()
            .returning(move |container_id| {
                let db_for_check = db_for_check.clone();
                let container_id = container_id.to_string();
                let refreshed = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        deployment_containers::Entity::find()
                            .filter(deployment_containers::Column::ContainerId.eq(container_id))
                            .one(db_for_check.as_ref())
                            .await
                    })
                })
                .expect("query deployment_containers row")
                .expect("deployment_containers row exists");
                assert!(
                    refreshed.deleted_at.is_some(),
                    "container must be marked deleted before stop_container() is called \
                     during pre-rollback cleanup — otherwise ContainerHealthMonitor's \
                     concurrent poll can observe it mid-exit with no signal the exit is \
                     intentional, and fires a false ContainerCrash alarm"
                );
                Ok(())
            });
        deployer.expect_remove_container().returning(|_| Ok(()));
        let deployer: Arc<dyn temps_deployer::ContainerDeployer> = Arc::new(deployer);

        let service = DeploymentService {
            db: db.clone(),
            log_service,
            config_service,
            queue_service,
            docker_log_service,
            deployer,
            encryption_service: create_test_encryption_service(),
            telemetry: std::sync::OnceLock::new(),
            env_resolver: std::sync::OnceLock::new(),
            compose_executor: std::sync::OnceLock::new(),
        };

        service
            .stop_environment_containers(environment.id, exclude_deployment_id)
            .await;

        let refreshed = deployment_containers::Entity::find_by_id(container.id)
            .one(db.as_ref())
            .await?
            .expect("container row still exists");
        assert!(refreshed.deleted_at.is_some());
        assert_eq!(refreshed.status.as_deref(), Some("removed"));

        Ok(())
    }

    #[tokio::test]
    async fn test_trigger_image_deployment_rejects_empty_image_ref(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let deployment_service = create_deployment_service_for_test(db);

        let result = deployment_service
            .trigger_image_deployment(1, None, String::new(), None, None)
            .await;

        assert!(matches!(result, Err(DeploymentError::InvalidInput(_))));
        Ok(())
    }

    #[tokio::test]
    async fn test_trigger_image_deployment_sends_deploy_image_requested_job(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        let deployment_service = create_deployment_service_for_test(db.clone());
        let mut receiver = deployment_service.queue_service.subscribe();

        deployment_service
            .trigger_image_deployment(
                42,
                Some(7),
                "ghcr.io/org/app:latest".to_string(),
                Some("/healthz".to_string()),
                Some(vec!["serve".to_string()]),
            )
            .await?;

        // The auto-responder spawned in create_deployment_service_for_test also
        // listens on this queue (to keep RouteTableUpdated flowing) — drain past
        // whatever else it produces until our own job shows up.
        let job = loop {
            match receiver.recv().await {
                Ok(temps_core::Job::DeployImageRequested(job)) => break job,
                Ok(_) => continue,
                Err(e) => panic!("queue closed before DeployImageRequested arrived: {}", e),
            }
        };

        assert_eq!(job.project_id, 42);
        assert_eq!(job.target_environment_id, Some(7));
        assert_eq!(job.image_ref, "ghcr.io/org/app:latest");
        assert_eq!(job.health_check_path.as_deref(), Some("/healthz"));
        assert_eq!(job.command, Some(vec!["serve".to_string()]));
        Ok(())
    }

    #[tokio::test]
    async fn redeploy_environment_uses_affected_deployment_and_target_environment(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let (project, environment, affected, _) = setup_test_deployment(&db).await?;

        let mut affected_active: deployments::ActiveModel = affected.clone().into();
        affected_active.metadata = Set(Some(temps_entities::deployments::DeploymentMetadata {
            external_image_ref: Some("registry.example/app:known-good".to_string()),
            ..Default::default()
        }));
        affected_active.update(db.as_ref()).await?;

        // A newer failed deployment must never supersede the workload whose
        // container is actually being migrated.
        deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            state: Set("failed".to_string()),
            slug: Set("failed-newer-deployment".to_string()),
            metadata: Set(Some(temps_entities::deployments::DeploymentMetadata {
                external_image_ref: Some("registry.example/app:failed".to_string()),
                ..Default::default()
            })),
            created_at: Set(Utc::now() + chrono::Duration::seconds(1)),
            updated_at: Set(Utc::now() + chrono::Duration::seconds(1)),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        let service = create_deployment_service_for_test(db);
        let mut receiver = service.queue_service.subscribe();
        service
            .redeploy_environment(project.id, environment.id, affected.id)
            .await?;

        let job = loop {
            match receiver.recv().await {
                Ok(temps_core::Job::DeployImageRequested(job)) => break job,
                Ok(_) => continue,
                Err(e) => panic!("queue closed before DeployImageRequested arrived: {}", e),
            }
        };
        assert_eq!(job.project_id, project.id);
        assert_eq!(job.target_environment_id, Some(environment.id));
        assert_eq!(job.image_ref, "registry.example/app:known-good");
        Ok(())
    }

    #[tokio::test]
    async fn redeploy_environment_rejects_deployment_from_another_environment(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let (project, environment, deployment, _) = setup_test_deployment(&db).await?;
        let service = create_deployment_service_for_test(db);

        let result = service
            .redeploy_environment(project.id, environment.id + 1, deployment.id)
            .await;

        assert!(matches!(result, Err(DeploymentError::NotFound(_))));
        Ok(())
    }
}
