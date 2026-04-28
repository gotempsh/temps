//! Mark Deployment Complete Job
//!
//! A synthetic job that marks the deployment as complete and updates the environment.
//! This job runs after all core deployment jobs (download, build, deploy) succeed.
//! Optional jobs (screenshots, crons) depend on this job, ensuring the deployment
//! is live before they run.

use async_trait::async_trait;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use temps_core::{
    Job, JobQueue, JobReceiver, JobResult, UtcDateTime, WorkflowContext, WorkflowError,
    WorkflowTask,
};
use temps_database::DbConnection;
use temps_entities::{deployment_containers, deployments, environments, nodes, projects};
use temps_logs::{LogLevel, LogService};
use tracing::{debug, info, warn};

/// Process-level locks keyed by environment_id to serialize mark_complete
/// operations for the same environment. This replaces PostgreSQL advisory
/// locks which don't work correctly with connection pools (Sea-ORM's
/// DatabaseConnection is pooled, so lock/unlock may hit different connections).
static ENVIRONMENT_LOCKS: std::sync::LazyLock<
    std::sync::Mutex<HashMap<i32, Arc<tokio::sync::Mutex<()>>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// Output from MarkDeploymentCompleteJob
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkCompleteOutput {
    pub completed_at: UtcDateTime,
    pub environment_id: i32,
}

/// Job that marks a deployment as complete and updates the environment
pub struct MarkDeploymentCompleteJob {
    job_id: String,
    deployment_id: i32,
    db: Arc<DbConnection>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
    container_deployer: Arc<dyn temps_deployer::ContainerDeployer>,
    queue: Arc<dyn JobQueue>,
    config_service: Option<Arc<temps_config::ConfigService>>,
    encryption_service: Arc<temps_core::EncryptionService>,
}

impl std::fmt::Debug for MarkDeploymentCompleteJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MarkDeploymentCompleteJob")
            .field("job_id", &self.job_id)
            .field("deployment_id", &self.deployment_id)
            .finish()
    }
}

impl MarkDeploymentCompleteJob {
    pub fn new(
        job_id: String,
        deployment_id: i32,
        db: Arc<DbConnection>,
        container_deployer: Arc<dyn temps_deployer::ContainerDeployer>,
        queue: Arc<dyn JobQueue>,
        encryption_service: Arc<temps_core::EncryptionService>,
    ) -> Self {
        Self {
            job_id,
            deployment_id,
            db,
            log_id: None,
            log_service: None,
            container_deployer,
            queue,
            config_service: None,
            encryption_service,
        }
    }

    pub fn with_config_service(mut self, config_service: Arc<temps_config::ConfigService>) -> Self {
        self.config_service = Some(config_service);
        self
    }

    pub fn with_log_id(mut self, log_id: String) -> Self {
        self.log_id = Some(log_id);
        self
    }

    pub fn with_log_service(mut self, log_service: Arc<LogService>) -> Self {
        self.log_service = Some(log_service);
        self
    }

    /// Write log message to job-specific log file
    async fn log(&self, message: String) -> Result<(), WorkflowError> {
        // Detect log level from message content/emojis
        let level = Self::detect_log_level(&message);

        if let (Some(ref log_id), Some(ref log_service)) = (&self.log_id, &self.log_service) {
            log_service
                .append_structured_log(log_id, level, message.clone())
                .await
                .map_err(|e| WorkflowError::Other(format!("Failed to write log: {}", e)))?;
        }
        Ok(())
    }

    /// Detect log level from message content
    fn detect_log_level(message: &str) -> LogLevel {
        if message.contains("✅")
            || message.contains("🎉")
            || message.contains("Complete")
            || message.contains("success")
        {
            LogLevel::Success
        } else if message.contains("❌")
            || message.contains("⚠️")
            || message.contains("Failed")
            || message.contains("Error")
            || message.contains("error")
        {
            LogLevel::Error
        } else if message.contains("⏳")
            || message.contains("🔄")
            || message.contains("🛑")
            || message.contains("Waiting")
            || message.contains("warning")
            || message.contains("Checking")
            || message.contains("Cancelling")
        {
            LogLevel::Warning
        } else {
            LogLevel::Info
        }
    }

    /// Get or create a process-level mutex for the given environment.
    /// This serializes concurrent `mark_complete` operations for the same
    /// environment without relying on PostgreSQL advisory locks (which
    /// break with connection pools — lock and unlock can hit different
    /// pooled connections, leaving locks permanently held).
    fn get_environment_mutex(environment_id: i32) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = ENVIRONMENT_LOCKS
            .lock()
            .expect("environment locks poisoned");
        locks
            .entry(environment_id)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// Mark deployment as complete and update environment
    /// Also updates deployment with workflow outputs (image info, container info)
    async fn mark_complete(
        &self,
        context: &WorkflowContext,
    ) -> Result<MarkCompleteOutput, WorkflowError> {
        self.log("Marking deployment as complete...".to_string())
            .await?;

        // Get deployment
        let deployment = deployments::Entity::find_by_id(self.deployment_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to find deployment: {}", e))
            })?
            .ok_or_else(|| {
                WorkflowError::JobExecutionFailed(format!(
                    "Deployment {} not found",
                    self.deployment_id
                ))
            })?;

        let environment_id = deployment.environment_id;

        // ── Serialize per environment ────────────────────────────────────
        //
        // Acquire a process-level mutex keyed on environment_id. This blocks
        // until any other in-flight mark_complete for the SAME environment
        // finishes, preventing two deployments from racing on the same
        // environment's current_deployment_id and tearing down each
        // other's containers. Different environments proceed in parallel.
        let env_mutex = Self::get_environment_mutex(environment_id);
        let _guard = env_mutex.lock().await;
        info!(
            deployment_id = self.deployment_id,
            environment_id, "Acquired environment lock"
        );

        let result = self
            .mark_complete_inner(context, &deployment, environment_id)
            .await;

        // Lock is released when `_guard` is dropped (on all exit paths)
        drop(_guard);
        info!(
            deployment_id = self.deployment_id,
            environment_id, "Released environment lock"
        );

        // ── Tear down previous deployments (outside the lock) ─────────
        // This runs after the lock is released so it doesn't block
        // subsequent deployments. Container teardown can be slow (10s
        // graceful stop per container) and doesn't need serialization —
        // the route table already points to the new deployment.
        if result.is_ok() {
            self.cancel_previous_deployments(environment_id).await;
        }

        result
    }

    /// Inner implementation of mark_complete, called with the advisory lock held.
    async fn mark_complete_inner(
        &self,
        context: &WorkflowContext,
        deployment: &deployments::Model,
        environment_id: i32,
    ) -> Result<MarkCompleteOutput, WorkflowError> {
        // Update deployment with workflow outputs
        let mut active_deployment: deployments::ActiveModel = deployment.clone().into();

        // Extract image info from build job output
        if let Ok(Some(image_tag)) = context.get_output::<String>("build_image", "image_tag") {
            debug!("Setting deployment image_name to: {}", image_tag);
            active_deployment.image_name = Set(Some(image_tag));
        }

        // Extract static_dir_location from deploy_static or deploy_static_bundle job output
        let static_dir = context
            .get_output::<String>("deploy_static", "static_dir_location")
            .ok()
            .flatten()
            .or_else(|| {
                // Also check deploy_static_bundle for remote static deployments
                context
                    .get_output::<String>("deploy_static_bundle", "static_dir_location")
                    .ok()
                    .flatten()
            });

        if let Some(static_dir) = static_dir {
            debug!("Setting deployment static_dir_location to: {}", static_dir);
            self.log(format!("📁 Static files location: {}", static_dir))
                .await?;
            active_deployment.static_dir_location = Set(Some(static_dir));
        }

        // Extract container info from deploy job output and create deployment_container records
        // Try to get container_ids array first (for multi-replica deployments)
        let container_ids = match context.get_output::<Vec<String>>("deploy_container", "container_ids") {
            Ok(Some(ids)) => {
                info!("Got {} container_ids from deploy_container output", ids.len());
                Some(ids)
            }
            Ok(None) => {
                debug!("No container_ids array in deploy_container output, trying single container_id");
                None
            }
            Err(e) => {
                warn!("Failed to deserialize container_ids from deploy_container: {}. Trying single container_id fallback.", e);
                None
            }
        }
        .or_else(|| {
            // Fallback to single container_id for backward compatibility
            match context.get_output::<String>("deploy_container", "container_id") {
                Ok(Some(id)) => {
                    info!("Got single container_id from deploy_container output: {}", id);
                    Some(vec![id])
                }
                Ok(None) => {
                    warn!("No container_id found in deploy_container output either");
                    None
                }
                Err(e) => {
                    warn!("Failed to deserialize container_id from deploy_container: {}", e);
                    None
                }
            }
        });

        let host_ports = context
            .get_output::<Vec<u16>>("deploy_container", "host_ports")
            .ok()
            .flatten()
            .or_else(|| {
                // Fallback to single host_port for backward compatibility
                context
                    .get_output::<u16>("deploy_container", "host_port")
                    .ok()
                    .flatten()
                    .map(|port| vec![port])
            });

        if container_ids.is_none() {
            warn!(
                "No container_ids found in workflow context from deploy_container job. \
                 Container registration will be skipped. This means the Environments page \
                 will show 'No containers'. Check that deploy_container job set its outputs."
            );
            self.log(
                "WARNING: No container IDs found from deploy job — containers won't appear in UI"
                    .to_string(),
            )
            .await
            .ok();
        }

        if let Some(container_ids) = container_ids {
            let now = chrono::Utc::now();
            let container_port = context
                .get_output::<i32>("deploy_container", "container_port")
                .ok()
                .flatten()
                .unwrap_or(8080);

            // Compose-specific: per-container data arrays (set by DeployComposeJob)
            let service_names: Option<Vec<String>> = context
                .get_output("deploy_container", "service_names")
                .ok()
                .flatten();
            let container_names_list: Option<Vec<String>> = context
                .get_output("deploy_container", "container_names")
                .ok()
                .flatten();
            let container_ports_list: Option<Vec<i32>> = context
                .get_output("deploy_container", "container_ports")
                .ok()
                .flatten();
            let image_names_list: Option<Vec<String>> = context
                .get_output("deploy_container", "image_names")
                .ok()
                .flatten();

            // Create a deployment_container record for each container
            for (index, container_id) in container_ids.iter().enumerate() {
                // Use per-container name if available (compose), otherwise generate
                let container_name = container_names_list
                    .as_ref()
                    .and_then(|names| names.get(index).cloned())
                    .unwrap_or_else(|| {
                        if container_ids.len() > 1 {
                            context
                                .get_output::<String>("deploy_container", "container_name")
                                .ok()
                                .flatten()
                                .map(|name| format!("{}-{}", name, index + 1))
                                .unwrap_or_else(|| {
                                    format!("container-{}-{}", self.deployment_id, index + 1)
                                })
                        } else {
                            context
                                .get_output::<String>("deploy_container", "container_name")
                                .ok()
                                .flatten()
                                .unwrap_or_else(|| format!("container-{}", self.deployment_id))
                        }
                    });

                // Use per-container port if available (compose), otherwise use shared port
                let effective_port = container_ports_list
                    .as_ref()
                    .and_then(|ports| ports.get(index).copied())
                    .unwrap_or(container_port);

                let host_port = host_ports
                    .as_ref()
                    .and_then(|ports| ports.get(index).map(|&p| p as i32));

                // Get node_id for this replica (from multi-node scheduler, if set)
                let node_ids = context
                    .get_output::<Vec<Option<i32>>>("deploy_container", "node_ids")
                    .ok()
                    .flatten();
                let node_id = node_ids
                    .as_ref()
                    .and_then(|ids| ids.get(index).cloned())
                    .flatten();

                // Get service_name for compose containers
                let service_name = service_names
                    .as_ref()
                    .and_then(|names| names.get(index).cloned());

                // Get per-container image name (compose) or fall back to deployment image
                let image_name = image_names_list
                    .as_ref()
                    .and_then(|names| names.get(index).cloned())
                    .or_else(|| match &active_deployment.image_name {
                        sea_orm::ActiveValue::Set(v) => v.clone(),
                        sea_orm::ActiveValue::Unchanged(v) => v.clone(),
                        _ => None,
                    });

                // Create deployment_container record
                let deployment_container = deployment_containers::ActiveModel {
                    deployment_id: Set(self.deployment_id),
                    container_id: Set(container_id.clone()),
                    container_name: Set(container_name.clone()),
                    container_port: Set(effective_port),
                    host_port: Set(host_port),
                    image_name: Set(image_name),
                    status: Set(Some("running".to_string())),
                    service_name: Set(service_name),
                    created_at: Set(now),
                    deployed_at: Set(now),
                    ready_at: Set(Some(now)),
                    deleted_at: Set(None),
                    node_id: Set(node_id),
                    ..Default::default()
                };

                deployment_container
                    .insert(self.db.as_ref())
                    .await
                    .map_err(|e| {
                        WorkflowError::JobExecutionFailed(format!(
                            "Failed to create deployment_container: {}",
                            e
                        ))
                    })?;

                info!(
                    "Created deployment_container record for container {} (replica {}/{})",
                    container_id,
                    index + 1,
                    container_ids.len()
                );
                self.log(format!(
                    "Container {} registered (replica {}/{})",
                    container_id,
                    index + 1,
                    container_ids.len()
                ))
                .await?;
            }
        }

        // ── Pre-flight: staleness check ──────────────────────────────────
        //
        // Before doing any work, verify this deployment hasn't been cancelled
        // (e.g. by cancel-on-supersede when a newer push arrived). This is a
        // safety net — the workflow executor also checks cancellation between
        // batches, but there is a window between the last batch completing and
        // mark_complete starting where a cancellation could have been set.
        let current_state = deployments::Entity::find_by_id(self.deployment_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!(
                    "Failed to re-check deployment {} state: {}",
                    self.deployment_id, e
                ))
            })?
            .ok_or_else(|| {
                WorkflowError::JobExecutionFailed(format!(
                    "Deployment {} disappeared during mark_complete",
                    self.deployment_id
                ))
            })?;

        if current_state.state == "cancelled" {
            info!(
                deployment_id = self.deployment_id,
                environment_id,
                reason = ?current_state.cancelled_reason,
                "Deployment was cancelled before mark_complete — aborting"
            );
            return Err(WorkflowError::JobExecutionFailed(format!(
                "Deployment {} was cancelled: {}",
                self.deployment_id,
                current_state
                    .cancelled_reason
                    .as_deref()
                    .unwrap_or("no reason")
            )));
        }

        // ── Phase 1: Switch route table to the new deployment ────────────
        //
        // Subscribe to the queue BEFORE updating current_deployment_id so we
        // cannot miss the RouteTableUpdated notification that the PG trigger +
        // route listener will produce.
        let mut route_receiver = self.queue.subscribe();

        // Load environment (only if not soft-deleted)
        let environment = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to find environment: {}", e))
            })?
            .ok_or_else(|| {
                WorkflowError::JobExecutionFailed(format!(
                    "Environment {} not found or was deleted",
                    environment_id
                ))
            })?;

        // Find the last *successful* deployment for this environment so we can
        // roll back to it if the route-table update fails. We query for
        // "completed" or "deployed" state (both represent a deployment that was
        // fully live at some point), excluding the current deployment.
        let last_successful_deployment_id = Self::find_last_successful_deployment(
            self.db.as_ref(),
            environment_id,
            self.deployment_id,
        )
        .await;

        debug!(
            environment_id,
            current_deployment_id = ?environment.current_deployment_id,
            last_successful_deployment_id = ?last_successful_deployment_id,
            "Rollback target resolved for route-table timeout"
        );

        let mut active_environment: environments::ActiveModel = environment.into();
        active_environment.current_deployment_id = Set(Some(self.deployment_id));

        active_environment
            .clone()
            .update(self.db.as_ref())
            .await
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to update environment: {}", e))
            })?;

        info!(
            "Environment {} current_deployment_id updated to {}",
            environment_id, self.deployment_id
        );
        self.log(format!(
            "Environment {} now points to deployment {} — waiting for route table confirmation...",
            environment_id, self.deployment_id
        ))
        .await?;

        // ── Phase 2: Wait for route table confirmation ───────────────────
        //
        // The PG trigger on environments.current_deployment_id fires a NOTIFY,
        // the route listener calls load_routes(), and on success publishes
        // RouteTableUpdated to the queue with environment_id + deployment_id.
        // We wait here until we see the matching event or timeout.
        const ROUTE_READY_TIMEOUT_SECS: u64 = 60;
        let route_ready = Self::wait_for_route_ready(
            &mut route_receiver,
            self.db.as_ref(),
            environment_id,
            self.deployment_id,
            std::time::Duration::from_secs(ROUTE_READY_TIMEOUT_SECS),
        )
        .await;

        if let Err(ref reason) = route_ready {
            // ── Timeout / failure path ───────────────────────────────────
            //
            // The proxy did not confirm the route update in time. Revert
            // current_deployment_id so the proxy keeps serving the old
            // deployment. Do NOT tear down old containers. Mark this
            // deployment as failed.
            tracing::warn!(
                deployment_id = self.deployment_id,
                environment_id,
                "Route table confirmation timed out after {}s: {}",
                ROUTE_READY_TIMEOUT_SECS,
                reason
            );

            self.log(format!(
                "Route table confirmation timed out after {}s — reverting to last successful deployment ({:?})",
                ROUTE_READY_TIMEOUT_SECS,
                last_successful_deployment_id
            ))
            .await?;

            // Revert current_deployment_id to the last deployment that actually
            // succeeded (has running containers), NOT just whatever was in the
            // column before. This handles edge cases where the previous value
            // points to a failed or torn-down deployment.
            let revert_env = environments::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(environment_id),
                current_deployment_id: Set(last_successful_deployment_id),
                ..Default::default()
            };
            if let Err(e) = revert_env.update(self.db.as_ref()).await {
                tracing::error!(
                    "Failed to revert current_deployment_id for environment {}: {}",
                    environment_id,
                    e
                );
            }

            // Mark deployment as failed
            let failed_deployment = deployments::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(self.deployment_id),
                state: Set("failed".to_string()),
                finished_at: Set(Some(chrono::Utc::now())),
                updated_at: Set(chrono::Utc::now()),
                cancelled_reason: Set(Some(format!(
                    "Route table confirmation timed out after {}s",
                    ROUTE_READY_TIMEOUT_SECS
                ))),
                ..Default::default()
            };
            if let Err(e) = failed_deployment.update(self.db.as_ref()).await {
                tracing::error!(
                    "Failed to mark deployment {} as failed: {}",
                    self.deployment_id,
                    e
                );
            }

            return Err(WorkflowError::JobExecutionFailed(format!(
                "Route table did not confirm new routes within {}s — deployment rolled back",
                ROUTE_READY_TIMEOUT_SECS
            )));
        }

        self.log("Route table confirmed — new deployment is routable".to_string())
            .await?;

        // ── Phase 2.5: Wait for every worker to apply the new generation ──
        //
        // The CP's local route table is updated, but the worker-side
        // proxy + DNS resolver each long-poll their own generation
        // counter from the CP. Until every healthy worker has ACKed
        // the new generation, a curl from a container *might* land on
        // a worker still proxying the previous backend set.
        //
        // We poll `node_route_state.applied_generation` and
        // `node_dns_state.applied_generation` until they catch up to
        // the CP's generations, with a bounded timeout. On timeout
        // we still proceed (don't fail the deploy on the propagation
        // step) — the route table is correct on the CP and the DNS
        // records are written; workers will catch up shortly. This
        // matches the existing tolerance for transient sync drift.
        const WORKER_APPLY_TIMEOUT_SECS: u64 = 10;
        if let Err(reason) = Self::wait_for_worker_apply(
            self.db.as_ref(),
            std::time::Duration::from_secs(WORKER_APPLY_TIMEOUT_SECS),
        )
        .await
        {
            tracing::warn!(
                deployment_id = self.deployment_id,
                "Worker propagation gate exceeded {WORKER_APPLY_TIMEOUT_SECS}s: {reason} \
                 — proceeding anyway, workers will converge in the background"
            );
            self.log(format!(
                "Worker propagation incomplete after {WORKER_APPLY_TIMEOUT_SECS}s ({reason}) — proceeding"
            ))
            .await?;
        } else {
            self.log("All workers acknowledged new route + DNS generations".to_string())
                .await?;
        }

        // ── Phase 3: Mark deployment as completed ────────────────────────
        let now = chrono::Utc::now();
        active_deployment.state = Set("completed".to_string());
        active_deployment.finished_at = Set(Some(now));
        active_deployment.updated_at = Set(now);

        active_deployment
            .update(self.db.as_ref())
            .await
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to update deployment: {}", e))
            })?;

        info!("Deployment {} marked as complete", self.deployment_id);
        self.log(format!(
            "Deployment {} status updated to Completed",
            self.deployment_id
        ))
        .await?;

        // Update project's last_deployment timestamp
        let project = projects::Entity::find_by_id(deployment.project_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to find project: {}", e))
            })?
            .ok_or_else(|| {
                WorkflowError::JobExecutionFailed(format!(
                    "Project {} not found",
                    deployment.project_id
                ))
            })?;

        let mut active_project: projects::ActiveModel = project.into();
        active_project.last_deployment = Set(Some(now));

        active_project.update(self.db.as_ref()).await.map_err(|e| {
            WorkflowError::JobExecutionFailed(format!(
                "Failed to update project last_deployment: {}",
                e
            ))
        })?;

        info!(
            "Project {} last_deployment updated to {}",
            deployment.project_id, now
        );

        // Reset sleeping state if environment was sleeping (on-demand mode).
        // A fresh deployment means containers are now running, so sleeping=false.
        // Use a direct UPDATE to avoid issues with ActiveModel field tracking.
        let sleeping_reset = environments::Entity::update_many()
            .col_expr(
                environments::Column::Sleeping,
                sea_orm::sea_query::Expr::value(false),
            )
            .col_expr(
                environments::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(now),
            )
            .filter(environments::Column::Id.eq(environment_id))
            .filter(environments::Column::Sleeping.eq(true))
            .exec(self.db.as_ref())
            .await
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!(
                    "Failed to reset sleeping state for environment {}: {}",
                    environment_id, e
                ))
            })?;
        if sleeping_reset.rows_affected > 0 {
            info!(
                "Reset sleeping state for environment {} after deployment",
                environment_id
            );
        }

        self.log("Deployment is now LIVE and ready for traffic!".to_string())
            .await?;

        // ── Phase 4: Emit DeploymentSucceeded event ──────────────────────
        // Get deployment URL from environment: prefer custom host, fall back to preview domain
        let url = if !active_environment.host.as_ref().is_empty() {
            Some(format!("https://{}", active_environment.host.as_ref()))
        } else if !active_environment.subdomain.as_ref().is_empty() {
            // No custom host set — construct URL from preview domain
            if let Some(ref config_service) = self.config_service {
                match config_service
                    .get_deployment_url_by_slug(active_environment.subdomain.as_ref())
                    .await
                {
                    Ok(preview_url) => Some(preview_url),
                    Err(e) => {
                        debug!(
                            "Failed to construct preview domain URL for environment {}: {}",
                            environment_id, e
                        );
                        None
                    }
                }
            } else {
                debug!(
                    "No config_service available to construct preview URL for environment {}",
                    environment_id
                );
                None
            }
        } else {
            None
        };

        // Extract health_check_path from any build job output in the workflow context
        let health_check_path = context.outputs.values().find_map(|job_outputs| {
            job_outputs
                .get("health_check_path")
                .and_then(|v| serde_json::from_value::<String>(v.clone()).ok())
        });

        let event = Job::DeploymentSucceeded(temps_core::DeploymentSucceededJob {
            deployment_id: self.deployment_id,
            project_id: deployment.project_id,
            environment_id,
            environment_name: active_environment.name.as_ref().clone(),
            commit_sha: deployment.commit_sha.clone(),
            url,
            health_check_path,
        });

        if let Err(e) = self.queue.send(event).await {
            self.log(format!("Failed to send DeploymentSucceeded event: {}", e))
                .await?;
        } else {
            debug!(
                "Sent DeploymentSucceeded event for deployment {}",
                self.deployment_id
            );
        }

        Ok(MarkCompleteOutput {
            completed_at: now,
            environment_id,
        })
    }

    /// Wait for the route table to reflect the new deployment.
    ///
    /// Uses a two-pronged approach:
    ///   1. Listen for `RouteTableUpdated` broadcast events matching our environment.
    ///      We match on environment_id only (not deployment_id) because with concurrent
    ///      deployments the NOTIFY payload may carry a different deployment_id than ours
    ///      even though load_routes() has already picked up our change.
    ///   2. Periodically send `ForceRouteReload` requests as a fallback in case the
    ///      PG LISTEN notification was lost (e.g. during the 5-second reconnection
    ///      window in ProjectChangeListener).
    ///
    /// IMPORTANT: We do NOT poll the database to confirm the route. The DB row
    /// `environments.current_deployment_id` was already written by us before this
    /// function is called, so checking it would always return true — a tautology
    /// that tells us nothing about whether the proxy's in-memory route table has
    /// actually loaded the new deployment. Only a `RouteTableUpdated` event from
    /// the route listener confirms the proxy is ready to serve traffic.
    ///
    /// After the event fires, we verify against the database that the environment
    /// still points to our deployment_id (it could have been superseded by a
    /// concurrent deployment).
    async fn wait_for_route_ready(
        receiver: &mut Box<dyn JobReceiver>,
        db: &DbConnection,
        environment_id: i32,
        deployment_id: i32,
        timeout: std::time::Duration,
    ) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + timeout;
        // Request a forced route reload every 5 seconds as a fallback for lost
        // NOTIFY messages. Unlike the old DB poll, this triggers an actual
        // load_routes() in the proxy, which will publish RouteTableUpdated if
        // our route is now present.
        const RELOAD_REQUEST_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
        let mut next_reload_request = tokio::time::Instant::now() + RELOAD_REQUEST_INTERVAL;
        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err("Timed out waiting for route table update".to_string());
            }

            // Periodically request a forced route reload as a fallback.
            // This handles the case where the PG NOTIFY was lost (e.g. during
            // listener reconnection). The reload will cause a RouteTableUpdated
            // event to be published, which we'll pick up in the recv() below.
            if tokio::time::Instant::now() >= next_reload_request {
                next_reload_request = tokio::time::Instant::now() + RELOAD_REQUEST_INTERVAL;
                debug!(
                    "Requesting forced route reload as fallback (environment={}, deployment={})",
                    environment_id, deployment_id
                );
                // Send a NOTIFY directly via the DB connection to trigger the
                // route listener, simulating the PG trigger that may have been missed.
                let notify_payload = serde_json::json!({
                    "action": "ENVIRONMENT_UPDATE",
                    "environment_id": environment_id,
                    "project_id": 0,
                    "deployment_id": deployment_id,
                    "timestamp": chrono::Utc::now().to_rfc3339()
                });
                let notify_sql = format!(
                    "SELECT pg_notify('project_route_change', '{}')",
                    notify_payload.to_string().replace('\'', "''")
                );
                if let Err(e) = db
                    .execute(sea_orm::Statement::from_string(
                        sea_orm::DatabaseBackend::Postgres,
                        notify_sql,
                    ))
                    .await
                {
                    warn!(
                        "Failed to send fallback NOTIFY for environment {}: {}",
                        environment_id, e
                    );
                }
            }

            let wait_until = deadline.min(next_reload_request);

            match tokio::time::timeout_at(wait_until, receiver.recv()).await {
                Ok(Ok(Job::RouteTableUpdated(ref update)))
                    if update.environment_id == Some(environment_id) =>
                {
                    debug!(
                        "Route table event for environment {} (event deployment_id={:?}, \
                         our deployment_id={}, {} routes) — verifying DB state",
                        environment_id, update.deployment_id, deployment_id, update.route_count
                    );
                    // Verify the environment actually points to our deployment
                    match Self::verify_environment_deployment(db, environment_id, deployment_id)
                        .await
                    {
                        Ok(true) => {
                            info!(
                                "Route confirmed via RouteTableUpdated event for environment {} deployment {}",
                                environment_id, deployment_id
                            );
                            return Ok(());
                        }
                        Ok(false) => {
                            // Environment was overwritten by a concurrent deployment.
                            return Err(format!(
                                "Environment {} no longer points to deployment {} \
                                 — superseded by another deployment",
                                environment_id, deployment_id
                            ));
                        }
                        Err(e) => {
                            warn!(
                                "DB verification failed after route event: {} — \
                                 treating event as confirmation",
                                e
                            );
                            // If we can't verify, trust the event (better than timing out)
                            return Ok(());
                        }
                    }
                }
                Ok(Ok(Job::RouteTableUpdated(ref update))) if update.environment_id.is_none() => {
                    // Periodic sync or generic route reload — environment_id is None.
                    // Still worth checking if our route is now live.
                    debug!(
                        "Generic RouteTableUpdated event ({} routes) — checking DB for environment {} deployment {}",
                        update.route_count, environment_id, deployment_id
                    );
                    match Self::verify_environment_deployment(db, environment_id, deployment_id)
                        .await
                    {
                        Ok(true) => {
                            info!(
                                "Route confirmed via periodic sync for environment {} deployment {}",
                                environment_id, deployment_id
                            );
                            return Ok(());
                        }
                        Ok(false) => {
                            // Not yet — keep waiting
                            continue;
                        }
                        Err(e) => {
                            warn!("DB verification failed after generic event: {} — continuing to wait", e);
                            continue;
                        }
                    }
                }
                Ok(Ok(_)) => {
                    // Different job type — keep waiting
                    continue;
                }
                Ok(Err(e)) => {
                    // Broadcast receiver lagged — messages were dropped but the channel
                    // is still alive. Log and continue; the periodic reload or fallback
                    // NOTIFY will produce a fresh event we can catch.
                    warn!(
                        "Queue receiver lagged while waiting for route confirmation (environment={}, deployment={}): {}",
                        environment_id, deployment_id, e
                    );
                    continue;
                }
                Err(_) => {
                    // timeout_at expired — loop back to check deadline and request reload
                }
            }
        }
    }

    /// Wait until every active worker node has applied both the
    /// CP's current route-table generation and the current DNS
    /// generation.
    ///
    /// This is the worker-side half of the consistency barrier — the
    /// CP-side half is `wait_for_route_ready` above (which only proves
    /// the CP's *own* route table reflects the change). Without this
    /// barrier, "completed" can fire while one or more workers still
    /// proxy to the previous backend set, producing transient 502s
    /// for clients that read "completed" and immediately curl the URL.
    ///
    /// The check is exact for both layers: we read the CP's durable
    /// `route_generation.current` and `dns_generation.current` and
    /// require every active node's
    /// `node_route_state.applied_generation` and
    /// `node_dns_state.applied_generation` to meet or exceed them.
    ///
    /// Only `nodes.status = 'active'` rows are gated — offline nodes
    /// aren't serving traffic so we don't block on them.
    async fn wait_for_worker_apply(
        db: &DbConnection,
        timeout: std::time::Duration,
    ) -> Result<(), String> {
        use sea_orm::{FromQueryResult, Statement};
        use temps_entities::{node_dns_state, node_route_state, nodes};

        #[derive(FromQueryResult)]
        struct Gen {
            current: Option<i64>,
        }
        let load_singleton = |table: &'static str| async move {
            Gen::find_by_statement(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                format!("SELECT current FROM {table} WHERE id = 1"),
            ))
            .one(db)
            .await
            .ok()
            .flatten()
            .and_then(|g| g.current)
            .unwrap_or(0)
        };
        let route_gen: i64 = load_singleton("route_generation").await;
        let dns_gen: i64 = load_singleton("dns_generation").await;

        let active_nodes: Vec<i32> = nodes::Entity::find()
            .filter(nodes::Column::Status.eq("active"))
            .all(db)
            .await
            .map_err(|e| format!("listing active nodes: {e}"))?
            .into_iter()
            .map(|n| n.id)
            .collect();

        if active_nodes.is_empty() {
            return Ok(());
        }

        let deadline = tokio::time::Instant::now() + timeout;
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));

        loop {
            interval.tick().await;
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "timed out waiting for {} node(s) to ACK \
                     (target route_gen={}, dns_gen={})",
                    active_nodes.len(),
                    route_gen,
                    dns_gen
                ));
            }

            let route_states = node_route_state::Entity::find()
                .filter(node_route_state::Column::NodeId.is_in(active_nodes.clone()))
                .all(db)
                .await
                .map_err(|e| format!("reading node_route_state: {e}"))?;
            let dns_states = node_dns_state::Entity::find()
                .filter(node_dns_state::Column::NodeId.is_in(active_nodes.clone()))
                .all(db)
                .await
                .map_err(|e| format!("reading node_dns_state: {e}"))?;

            let route_ok = route_states.len() == active_nodes.len()
                && route_states
                    .iter()
                    .all(|s| s.applied_generation >= route_gen);
            let dns_ok = dns_states.len() == active_nodes.len()
                && dns_states.iter().all(|s| s.applied_generation >= dns_gen);

            if route_ok && dns_ok {
                return Ok(());
            }
        }
    }

    /// Check whether the environment's current_deployment_id matches what we expect.
    /// Returns true if the environment points to our deployment_id.
    async fn verify_environment_deployment(
        db: &DbConnection,
        environment_id: i32,
        expected_deployment_id: i32,
    ) -> Result<bool, sea_orm::DbErr> {
        let env = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::DeletedAt.is_null())
            .one(db)
            .await?;
        match env {
            Some(env) => Ok(env.current_deployment_id == Some(expected_deployment_id)),
            None => Ok(false),
        }
    }

    /// Find the last deployment for the given environment that reached a
    /// successful state ("completed" or "deployed"), excluding the deployment
    /// we are currently trying to mark complete. Returns `None` if there is no
    /// previous successful deployment (e.g. first-ever deploy for this env).
    async fn find_last_successful_deployment(
        db: &DbConnection,
        environment_id: i32,
        exclude_deployment_id: i32,
    ) -> Option<i32> {
        match deployments::Entity::find()
            .filter(deployments::Column::EnvironmentId.eq(environment_id))
            .filter(deployments::Column::Id.ne(exclude_deployment_id))
            .filter(deployments::Column::State.is_in(vec!["completed", "deployed"]))
            .order_by_desc(deployments::Column::FinishedAt)
            .one(db)
            .await
        {
            Ok(Some(deployment)) => {
                debug!(
                    "Last successful deployment for environment {}: {} (state: {}, finished: {:?})",
                    environment_id, deployment.id, deployment.state, deployment.finished_at
                );
                Some(deployment.id)
            }
            Ok(None) => {
                debug!(
                    "No previous successful deployment found for environment {}",
                    environment_id
                );
                None
            }
            Err(e) => {
                tracing::error!(
                    "Failed to query last successful deployment for environment {}: {}",
                    environment_id,
                    e
                );
                // Fall back to None rather than crashing — this means the
                // environment will have no current_deployment_id, which is safer
                // than pointing at a broken deployment.
                None
            }
        }
    }

    /// Create a `RemoteNodeDeployer` for a given node_id by looking up the
    /// node's address and decrypting its token.
    async fn get_remote_deployer(
        &self,
        node_id: i32,
    ) -> Result<Arc<dyn temps_deployer::ContainerDeployer>, String> {
        let node = nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| format!("DB error looking up node {}: {}", node_id, e))?
            .ok_or_else(|| format!("Node {} not found in database", node_id))?;

        let encrypted_token = node.token_encrypted.ok_or_else(|| {
            format!(
                "Node '{}' (id={}) has no encrypted token",
                node.name, node_id
            )
        })?;

        let decrypted_bytes = self
            .encryption_service
            .decrypt(&encrypted_token)
            .map_err(|e| format!("Failed to decrypt token for node '{}': {}", node.name, e))?;

        let token = String::from_utf8(decrypted_bytes).map_err(|e| {
            format!(
                "Decrypted token for node '{}' is not valid UTF-8: {}",
                node.name, e
            )
        })?;

        let remote = temps_deployer::remote::RemoteNodeDeployer::new(
            node.address.clone(),
            token,
            node.name.clone(),
        )
        .map_err(|e| {
            format!(
                "Failed to create remote deployer for node '{}': {}",
                node.name, e
            )
        })?;

        Ok(Arc::new(remote))
    }

    /// Teardown all running/pending deployments for the same environment
    /// This ensures only one active deployment per environment
    /// Note: Deployment state is NOT changed - the is_current flag indicates which deployment is active
    async fn cancel_previous_deployments(&self, environment_id: i32) {
        use sea_orm::Set;

        self.log("Checking for previous deployments to teardown...".to_string())
            .await
            .ok();

        // Find all running or pending deployments for this environment (excluding the new one)
        // Note: "failed" deployments are intentionally excluded to preserve error history
        let previous_deployments = match deployments::Entity::find()
            .filter(deployments::Column::EnvironmentId.eq(environment_id))
            .filter(deployments::Column::Id.ne(self.deployment_id))
            .filter(deployments::Column::State.is_in(vec![
                "pending",
                "running",
                "built",
                "completed",
            ]))
            .all(self.db.as_ref())
            .await
        {
            Ok(deps) => deps,
            Err(e) => {
                self.log(format!("Failed to fetch previous deployments: {}", e))
                    .await
                    .ok();
                return;
            }
        };

        if previous_deployments.is_empty() {
            self.log("No previous deployments to teardown".to_string())
                .await
                .ok();
            return;
        }

        self.log(format!(
            "Found {} previous deployment(s) to teardown",
            previous_deployments.len()
        ))
        .await
        .ok();

        for deployment in previous_deployments {
            let deployment_id = deployment.id;
            self.log(format!(
                "Tearing down deployment {} (state: {})",
                deployment_id, deployment.state
            ))
            .await
            .ok();

            // Stop all containers for this deployment
            let containers = match deployment_containers::Entity::find()
                .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
                .filter(deployment_containers::Column::DeletedAt.is_null())
                .all(self.db.as_ref())
                .await
            {
                Ok(containers) => containers,
                Err(e) => {
                    self.log(format!(
                        "Failed to fetch containers for deployment {}: {}",
                        deployment_id, e
                    ))
                    .await
                    .ok();
                    continue;
                }
            };

            if containers.is_empty() {
                // Fallback: no deployment_containers records (pre-migration deployments).
                // Try to stop the container by its slug name convention: {slug}
                let slug = &deployment.slug;
                self.log(format!(
                    "No container records for deployment {} — trying slug-based cleanup: {}",
                    deployment_id, slug
                ))
                .await
                .ok();

                // Try stop + remove by container name (slug)
                if let Err(e) = self.container_deployer.stop_container(slug).await {
                    debug!("Could not stop container by slug {}: {}", slug, e);
                }
                if let Err(e) = self.container_deployer.remove_container(slug).await {
                    debug!("Could not remove container by slug {}: {}", slug, e);
                } else {
                    self.log(format!("Removed orphaned container {}", slug))
                        .await
                        .ok();
                }
            }

            for container in containers {
                let container_id = container.container_id.clone();

                // Determine which deployer to use based on node_id
                let deployer: Arc<dyn temps_deployer::ContainerDeployer> = if let Some(node_id) =
                    container.node_id
                {
                    match self.get_remote_deployer(node_id).await {
                        Ok(remote) => remote,
                        Err(e) => {
                            self.log(format!(
                                    "Failed to create remote deployer for container {} on node {}: {} — falling back to local",
                                    container_id, node_id, e
                                ))
                                .await
                                .ok();
                            self.container_deployer.clone()
                        }
                    }
                } else {
                    self.container_deployer.clone()
                };

                // Stop container first
                match deployer.stop_container(&container_id).await {
                    Ok(_) => {
                        self.log(format!("Stopped container {}", container_id))
                            .await
                            .ok();
                    }
                    Err(e) => {
                        self.log(format!("Failed to stop container {}: {}", container_id, e))
                            .await
                            .ok();
                    }
                }

                // Remove container from Docker
                match deployer.remove_container(&container_id).await {
                    Ok(_) => {
                        self.log(format!("Removed container {}", container_id))
                            .await
                            .ok();
                    }
                    Err(e) => {
                        self.log(format!(
                            "Failed to remove container {}: {}",
                            container_id, e
                        ))
                        .await
                        .ok();
                    }
                }

                // Mark container as deleted in database
                let mut active_container: deployment_containers::ActiveModel = container.into();
                active_container.deleted_at = Set(Some(chrono::Utc::now()));
                active_container.status = Set(Some("removed".to_string()));
                if let Err(e) = active_container.update(self.db.as_ref()).await {
                    self.log(format!("Failed to update container status: {}", e))
                        .await
                        .ok();
                }
            }

            self.log(format!(
                "Torn down deployment {} - containers stopped and removed",
                deployment_id
            ))
            .await
            .ok();
        }

        self.log("All previous deployments torn down successfully".to_string())
            .await
            .ok();
    }
}

#[async_trait]
impl WorkflowTask for MarkDeploymentCompleteJob {
    fn job_id(&self) -> &str {
        &self.job_id
    }

    fn name(&self) -> &str {
        "Mark Deployment Complete"
    }

    fn description(&self) -> &str {
        "Marks the deployment as complete and updates environment routing"
    }

    fn depends_on(&self) -> Vec<String> {
        // This job depends on all core deployment jobs being complete
        // Dependencies are set by the workflow planner
        vec![]
    }

    async fn execute(&self, mut context: WorkflowContext) -> Result<JobResult, WorkflowError> {
        self.log(format!(
            "Marking deployment {} as complete",
            self.deployment_id
        ))
        .await?;

        let output = self.mark_complete(&context).await?;

        // Set job outputs
        context.set_output(
            &self.job_id,
            "completed_at",
            output.completed_at.timestamp(),
        )?;
        context.set_output(&self.job_id, "environment_id", output.environment_id)?;
        context.set_output(&self.job_id, "deployment_id", self.deployment_id)?;

        self.log("Deployment marked as complete successfully".to_string())
            .await?;

        Ok(JobResult::success(context))
    }

    async fn validate_prerequisites(
        &self,
        _context: &WorkflowContext,
    ) -> Result<(), WorkflowError> {
        if self.job_id.is_empty() {
            return Err(WorkflowError::JobValidationFailed(
                "job_id cannot be empty".to_string(),
            ));
        }
        if self.deployment_id <= 0 {
            return Err(WorkflowError::JobValidationFailed(
                "deployment_id must be positive".to_string(),
            ));
        }
        Ok(())
    }

    async fn cleanup(&self, _context: &WorkflowContext) -> Result<(), WorkflowError> {
        Ok(())
    }
}

/// Builder for MarkDeploymentCompleteJob
pub struct MarkDeploymentCompleteJobBuilder {
    job_id: Option<String>,
    deployment_id: Option<i32>,
    db: Option<Arc<DbConnection>>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
    container_deployer: Option<Arc<dyn temps_deployer::ContainerDeployer>>,
    queue: Option<Arc<dyn JobQueue>>,
    config_service: Option<Arc<temps_config::ConfigService>>,
    encryption_service: Option<Arc<temps_core::EncryptionService>>,
}

impl MarkDeploymentCompleteJobBuilder {
    pub fn new() -> Self {
        Self {
            job_id: None,
            deployment_id: None,
            db: None,
            log_id: None,
            log_service: None,
            container_deployer: None,
            queue: None,
            config_service: None,
            encryption_service: None,
        }
    }

    pub fn job_id(mut self, job_id: String) -> Self {
        self.job_id = Some(job_id);
        self
    }

    pub fn deployment_id(mut self, deployment_id: i32) -> Self {
        self.deployment_id = Some(deployment_id);
        self
    }

    pub fn db(mut self, db: Arc<DbConnection>) -> Self {
        self.db = Some(db);
        self
    }

    pub fn log_id(mut self, log_id: String) -> Self {
        self.log_id = Some(log_id);
        self
    }

    pub fn log_service(mut self, log_service: Arc<LogService>) -> Self {
        self.log_service = Some(log_service);
        self
    }

    pub fn container_deployer(
        mut self,
        container_deployer: Arc<dyn temps_deployer::ContainerDeployer>,
    ) -> Self {
        self.container_deployer = Some(container_deployer);
        self
    }

    pub fn queue(mut self, queue: Arc<dyn JobQueue>) -> Self {
        self.queue = Some(queue);
        self
    }

    pub fn config_service(mut self, config_service: Arc<temps_config::ConfigService>) -> Self {
        self.config_service = Some(config_service);
        self
    }

    pub fn encryption_service(mut self, service: Arc<temps_core::EncryptionService>) -> Self {
        self.encryption_service = Some(service);
        self
    }

    pub fn build(self) -> Result<MarkDeploymentCompleteJob, WorkflowError> {
        let job_id = self
            .job_id
            .unwrap_or_else(|| "mark_deployment_complete".to_string());
        let deployment_id = self.deployment_id.ok_or_else(|| {
            WorkflowError::JobValidationFailed("deployment_id is required".to_string())
        })?;
        let db = self.db.ok_or_else(|| {
            WorkflowError::JobValidationFailed("db connection is required".to_string())
        })?;
        let container_deployer = self.container_deployer.ok_or_else(|| {
            WorkflowError::JobValidationFailed("container_deployer is required".to_string())
        })?;
        let queue = self
            .queue
            .ok_or_else(|| WorkflowError::JobValidationFailed("queue is required".to_string()))?;
        let encryption_service = self.encryption_service.ok_or_else(|| {
            WorkflowError::JobValidationFailed("encryption_service is required".to_string())
        })?;

        let mut job = MarkDeploymentCompleteJob::new(
            job_id,
            deployment_id,
            db,
            container_deployer,
            queue,
            encryption_service,
        );

        if let Some(log_id) = self.log_id {
            job = job.with_log_id(log_id);
        }
        if let Some(log_service) = self.log_service {
            job = job.with_log_service(log_service);
        }
        if let Some(config_service) = self.config_service {
            job = job.with_config_service(config_service);
        }

        Ok(job)
    }
}

impl Default for MarkDeploymentCompleteJobBuilder {
    fn default() -> Self {
        Self::new()
    }
}
