// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Container health monitoring loop
//!
//! Periodically inspects all active deployment containers to detect:
//! - Restart count increases (crash loops)
//! - OOM kills
//! - High CPU/memory usage
//! - Containers that exited unexpectedly

use crate::alarm_service::{AlarmService, AlarmSeverity, AlarmType, FireAlarmRequest};
use futures::{stream, StreamExt};
use sea_orm::{
    sea_query::Expr, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseBackend,
    DatabaseConnection, DbErr, EntityTrait, QueryFilter, Statement, TransactionTrait,
};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use temps_deployer::ContainerDeployer;
use temps_entities::{deployment_containers, deployments};
use temps_metrics::store::{MetricKind, MetricPoint, MetricsStore, SourceKind};
use thiserror::Error;
use tracing::{debug, error, info, warn};

/// Resolves the Docker runtime for a worker node.
///
/// Implementations live outside this crate so monitoring does not depend on
/// deployment orchestration or worker credential storage.
#[async_trait::async_trait]
pub trait ContainerRuntimeResolver: Send + Sync {
    async fn resolve_runtime(
        &self,
        node_id: i32,
    ) -> Result<Arc<dyn ContainerDeployer>, ContainerRuntimeResolutionError>;
}

/// Context retained when a worker runtime cannot be constructed.
#[derive(Debug, Clone, Error)]
#[error("Failed to resolve container runtime for worker node {node_id}: {reason}")]
pub struct ContainerRuntimeResolutionError {
    pub node_id: i32,
    pub reason: String,
}

/// Cached state for a container between health checks
#[derive(Debug, Clone)]
struct ContainerState {
    /// Last known restart count from Docker
    restart_count: i64,
    /// Last observed network bytes received (for rate computation)
    last_net_rx_bytes: u64,
    /// Last observed network bytes transmitted (for rate computation)
    last_net_tx_bytes: u64,
}

type RuntimeCache =
    HashMap<i32, Result<Arc<dyn ContainerDeployer>, ContainerRuntimeResolutionError>>;

struct ContainerCheckJob {
    container: deployment_containers::Model,
    deployment: deployments::Model,
    deployer: Arc<dyn ContainerDeployer>,
    is_remote: bool,
}

const MAX_CONCURRENT_CONTAINER_CHECKS: usize = 16;

fn fair_order_jobs(jobs: Vec<ContainerCheckJob>) -> Vec<ContainerCheckJob> {
    let mut buckets: Vec<(Option<i32>, VecDeque<ContainerCheckJob>)> = Vec::new();
    for job in jobs {
        if let Some((_, bucket)) = buckets
            .iter_mut()
            .find(|(node_id, _)| *node_id == job.container.node_id)
        {
            bucket.push_back(job);
        } else {
            buckets.push((job.container.node_id, VecDeque::from([job])));
        }
    }

    let mut ordered = Vec::new();
    loop {
        let mut added = false;
        for (_, bucket) in &mut buckets {
            if let Some(job) = bucket.pop_front() {
                ordered.push(job);
                added = true;
            }
        }
        if !added {
            return ordered;
        }
    }
}

fn published_tcp_host_port(
    container_port: i32,
    ports: &[temps_deployer::PortMapping],
) -> Option<i32> {
    ports
        .iter()
        .filter(|port| {
            i32::from(port.container_port) == container_port
                && matches!(port.protocol, temps_deployer::Protocol::Tcp)
                && port.host_port != 0
        })
        .min_by_key(|port| {
            let host_ip = port.host_ip.as_deref().unwrap_or("");
            let interface_rank = match host_ip {
                "" | "0.0.0.0" | "::" => 0,
                _ => 1,
            };
            (interface_rank, host_ip, port.host_port)
        })
        .map(|port| i32::from(port.host_port))
}

/// Whether a `deployment_containers.status` records that the container was
/// stopped on purpose: by the user (`"stopped"`) or retained after a failed
/// deployment for log inspection (`"retained:*"`).
fn is_intentionally_stopped(status: Option<&str>) -> bool {
    status.is_some_and(|status| status == "stopped" || status.starts_with("retained:"))
}

/// Whether the container's exit is still covered by its intentional-stop
/// marker. A user-stopped (`"stopped"`) container that Docker reports as
/// started after the stop ran again since — by hand or through a restart
/// policy — so a new exit is a real crash even if the restart and the crash
/// both fell between two polls. The stop time is the row's `finished_at`,
/// written by the stop itself; rows stopped before that was recorded fall
/// back to the last start the monitor saw.
fn exit_is_intentional(
    container: &deployment_containers::Model,
    info: &temps_deployer::ContainerInfo,
) -> bool {
    if !is_intentionally_stopped(container.status.as_deref()) {
        return false;
    }
    let stopped_at = container.finished_at.or(container.started_at);
    let restarted_since_stop = container.status.as_deref() == Some("stopped")
        && matches!(
            (stopped_at, info.started_at),
            (Some(stopped_at), Some(started_at)) if started_at > stopped_at
        );
    !restarted_since_stop
}

/// Configuration for resource usage thresholds
#[derive(Debug, Clone)]
pub struct ContainerHealthConfig {
    /// How often to poll containers (seconds)
    pub poll_interval_secs: u64,
    /// CPU usage percent threshold to trigger alarm
    pub cpu_threshold_percent: f64,
    /// Memory usage percent threshold to trigger alarm
    pub memory_threshold_percent: f64,
    /// Number of consecutive checks above threshold before firing alarm
    pub consecutive_threshold_checks: u32,
    /// Maximum time one worker container may occupy a monitoring slot.
    pub worker_check_timeout_ms: u64,
}

impl Default for ContainerHealthConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: 30,
            cpu_threshold_percent: 90.0,
            memory_threshold_percent: 90.0,
            consecutive_threshold_checks: 3,
            worker_check_timeout_ms: 15_000,
        }
    }
}

/// Container health monitoring service.
/// Polls Docker for container state and fires alarms via AlarmService.
pub struct ContainerHealthMonitor {
    db: Arc<DatabaseConnection>,
    deployer: Arc<dyn ContainerDeployer>,
    alarm_service: Arc<AlarmService>,
    config: ContainerHealthConfig,
    /// Optional metrics store. When set, container resource metrics are written
    /// after each poll cycle in addition to the alarm-firing logic.
    metrics_store: Option<Arc<dyn MetricsStore>>,
    /// Node-aware runtime lookup. Remote containers are skipped when this is
    /// absent or resolution fails; they must never fall back to local Docker.
    runtime_resolver: Option<Arc<dyn ContainerRuntimeResolver>>,
    /// Cached restart counts and network stats keyed by deployment_container.id
    container_states: tokio::sync::RwLock<HashMap<i32, ContainerState>>,
    /// Consecutive high-resource checks keyed by (container_db_id, alarm_type_str)
    resource_counters: tokio::sync::RwLock<HashMap<(i32, &'static str), u32>>,
}

impl ContainerHealthMonitor {
    pub fn new(
        db: Arc<DatabaseConnection>,
        deployer: Arc<dyn ContainerDeployer>,
        alarm_service: Arc<AlarmService>,
        config: ContainerHealthConfig,
    ) -> Self {
        Self {
            db,
            deployer,
            alarm_service,
            config,
            metrics_store: None,
            runtime_resolver: None,
            container_states: tokio::sync::RwLock::new(HashMap::new()),
            resource_counters: tokio::sync::RwLock::new(HashMap::new()),
        }
    }

    /// Attach a metrics store. When set, container resource metrics
    /// (CPU, memory, network I/O) are written after each poll cycle.
    /// Monitoring works correctly without a metrics store — this field
    /// is intentionally `Option` so the monitor starts even when metrics
    /// collection is disabled.
    pub fn with_metrics_store(mut self, store: Arc<dyn MetricsStore>) -> Self {
        self.metrics_store = Some(store);
        self
    }

    /// Attach the node-aware runtime resolver used for worker containers.
    pub fn with_runtime_resolver(mut self, resolver: Arc<dyn ContainerRuntimeResolver>) -> Self {
        self.runtime_resolver = Some(resolver);
        self
    }

    async fn runtime_for_node(
        &self,
        node_id: Option<i32>,
        remote_runtimes: &mut RuntimeCache,
    ) -> Result<Arc<dyn ContainerDeployer>, ContainerRuntimeResolutionError> {
        let Some(node_id) = node_id else {
            return Ok(self.deployer.clone());
        };
        if let Some(cached) = remote_runtimes.get(&node_id) {
            return cached.clone();
        }
        let resolved = match &self.runtime_resolver {
            Some(resolver) => resolver.resolve_runtime(node_id).await,
            None => Err(ContainerRuntimeResolutionError {
                node_id,
                reason: "no worker runtime resolver is registered".to_string(),
            }),
        };
        remote_runtimes.insert(node_id, resolved.clone());
        resolved
    }

    /// Start the health monitoring loop. Runs forever.
    pub async fn start(self: Arc<Self>) {
        info!(
            "Starting container health monitor (poll interval: {}s, cpu threshold: {}%, memory threshold: {}%)",
            self.config.poll_interval_secs,
            self.config.cpu_threshold_percent,
            self.config.memory_threshold_percent,
        );

        loop {
            if let Err(e) = self.check_all_containers().await {
                error!("Container health check cycle failed: {}", e);
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(
                self.config.poll_interval_secs,
            ))
            .await;
        }
    }

    /// Run one check cycle over all active containers
    async fn check_all_containers(&self) -> Result<(), String> {
        // Find all non-deleted deployment containers
        let containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await
            .map_err(|e| format!("Failed to query deployment_containers: {}", e))?;

        if containers.is_empty() {
            debug!("No active containers to monitor");
            return Ok(());
        }

        // Prune cached state for containers that no longer exist
        let active_ids: std::collections::HashSet<i32> = containers.iter().map(|c| c.id).collect();
        {
            let mut states = self.container_states.write().await;
            states.retain(|id, _| active_ids.contains(id));
        }
        {
            let mut counters = self.resource_counters.write().await;
            counters.retain(|(id, _), _| active_ids.contains(id));
        }

        debug!("Checking {} active containers", containers.len());

        // Batch-load all deployments referenced by the active container set in a
        // single query to avoid N+1 per-container SELECT.
        let deployment_ids: Vec<i32> = containers
            .iter()
            .map(|c| c.deployment_id)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        let deployments_map: HashMap<i32, deployments::Model> = deployments::Entity::find()
            .filter(deployments::Column::Id.is_in(deployment_ids))
            .all(self.db.as_ref())
            .await
            .map_err(|e| format!("Failed to batch-query deployments: {e}"))?
            .into_iter()
            .map(|d| (d.id, d))
            .collect();

        // Cache each worker runtime only for this cycle. This bounds node and
        // credential lookups while ensuring token/CA rotation is observed on
        // the next poll.
        let mut remote_runtimes = RuntimeCache::new();

        let mut jobs = Vec::with_capacity(containers.len());
        for container in &containers {
            match deployments_map.get(&container.deployment_id) {
                None => {
                    debug!(
                        "Deployment {} not found for container {} ({}), skipping",
                        container.deployment_id, container.id, container.container_name
                    );
                }
                Some(deployment) => {
                    let deployer = match self
                        .runtime_for_node(container.node_id, &mut remote_runtimes)
                        .await
                    {
                        Ok(deployer) => deployer,
                        Err(resolution_error) => {
                            warn!(
                                node_id = container.node_id,
                                container_id = container.id,
                                container_runtime_id = %container.container_id,
                                deployment_id = container.deployment_id,
                                error = %resolution_error,
                                "Skipping worker container health check because its runtime could not be resolved"
                            );
                            continue;
                        }
                    };
                    jobs.push(ContainerCheckJob {
                        container: container.clone(),
                        deployment: deployment.clone(),
                        deployer,
                        is_remote: container.node_id.is_some(),
                    });
                }
            }
        }

        self.check_container_jobs(jobs).await;

        Ok(())
    }

    async fn check_container_jobs(&self, jobs: Vec<ContainerCheckJob>) {
        stream::iter(fair_order_jobs(jobs))
            .for_each_concurrent(MAX_CONCURRENT_CONTAINER_CHECKS, |job| async move {
                let result = if job.is_remote {
                    self.check_remote_container(
                        &job.container,
                        &job.deployment,
                        job.deployer.as_ref(),
                    )
                    .await
                } else {
                    self.check_container(&job.container, &job.deployment, job.deployer.as_ref())
                        .await
                };
                if let Err(error) = result {
                    debug!(
                        container_id = job.container.id,
                        container_name = %job.container.container_name,
                        %error,
                        "Failed to check container"
                    );
                }
            })
            .await;
    }

    /// Check a single container for health issues
    async fn check_container(
        &self,
        container: &deployment_containers::Model,
        deployment: &deployments::Model,
        deployer: &dyn ContainerDeployer,
    ) -> Result<(), String> {
        // Get container info from Docker
        let info = deployer
            .get_container_info(&container.container_id)
            .await
            .map_err(|e| {
                format!(
                    "Failed to get info for container {} ({}): {}",
                    container.container_id, container.container_name, e
                )
            })?;

        self.process_container_info(container, deployment, &info)
            .await;
        self.check_resource_usage(container, deployment, deployer)
            .await;

        Ok(())
    }

    async fn check_remote_container(
        &self,
        container: &deployment_containers::Model,
        deployment: &deployments::Model,
        deployer: &dyn ContainerDeployer,
    ) -> Result<(), String> {
        let timeout = tokio::time::Duration::from_millis(self.config.worker_check_timeout_ms);
        let deadline = tokio::time::Instant::now() + timeout;
        let info = tokio::time::timeout(
            timeout,
            deployer.get_container_info(&container.container_id),
        )
        .await
        .map_err(|_| {
            format!(
                "Worker container info timed out after {}ms for {} ({})",
                self.config.worker_check_timeout_ms,
                container.container_id,
                container.container_name
            )
        })?
        .map_err(|error| {
            format!(
                "Failed to get info for container {} ({}): {error}",
                container.container_id, container.container_name
            )
        })?;

        if matches!(
            info.status,
            temps_deployer::ContainerStatus::Exited | temps_deployer::ContainerStatus::Dead
        ) {
            self.process_container_info(container, deployment, &info)
                .await;
            return Ok(());
        }

        let ((), stats) = tokio::join!(
            self.process_container_info(container, deployment, &info),
            async {
                match tokio::time::timeout_at(
                    deadline,
                    deployer.get_container_stats(&container.container_id),
                )
                .await
                {
                    Ok(Ok(stats)) => Some(stats),
                    Ok(Err(error)) => {
                        debug!(container_id = container.id, %error, "Failed to get worker container stats");
                        None
                    }
                    Err(_) => {
                        warn!(
                            container_id = container.id,
                            node_id = container.node_id,
                            timeout_ms = self.config.worker_check_timeout_ms,
                            "Worker container runtime deadline expired while fetching stats"
                        );
                        None
                    }
                }
            },
        );

        if let Some(stats) = stats {
            self.check_resource_stats(container, deployment, &stats)
                .await;
        }
        Ok(())
    }

    async fn process_container_info(
        &self,
        container: &deployment_containers::Model,
        deployment: &deployments::Model,
        info: &temps_deployer::ContainerInfo,
    ) {
        // Check restart count
        self.check_restart_count(container, deployment, info).await;

        // Persist runtime metadata (started_at, cpu_limit_cores) once they're
        // observed. These don't change while a container is running, so the
        // diff check in persist_runtime_info skips writes after the first hit.
        if let Err(error) = self.persist_runtime_info(container, info).await {
            error!(container_id = container.id, deployment_id = container.deployment_id,
                %error, "Failed to persist container runtime information and refresh routes");
        }

        // Check container status (exited, dead, OOM)
        self.check_container_status(container, deployment, info)
            .await;
    }

    /// Reconcile runtime metadata and published ports after container restarts.
    /// Persist the port and route notification atomically so a failed notification
    /// leaves the old port in place and the next health check retries both.
    async fn persist_runtime_info(
        &self,
        container: &deployment_containers::Model,
        info: &temps_deployer::ContainerInfo,
    ) -> Result<(), DbErr> {
        // A successful runtime inspection is authoritative. If the expected
        // TCP binding is absent, clear the recorded host port so the proxy
        // cannot keep dialing a port that Docker may have reassigned.
        let host_port = published_tcp_host_port(container.container_port, &info.ports);
        let port_changed = host_port != container.host_port;
        if !port_changed
            && container.started_at == info.started_at
            && container.cpu_limit_cores == info.cpu_limit_cores
        {
            return Ok(());
        }
        let txn = self.db.begin().await?;
        let active = deployment_containers::ActiveModel {
            id: Set(container.id),
            host_port: Set(host_port),
            started_at: Set(info.started_at),
            cpu_limit_cores: Set(info.cpu_limit_cores),
            ..Default::default()
        };
        deployment_containers::Entity::update(active)
            .exec(&txn)
            .await?;
        if port_changed {
            txn.execute(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT pg_notify('route_table_changes', '')".to_string(),
            ))
            .await?;
        }
        txn.commit().await?;
        Ok(())
    }

    /// Detect restart count increases and fire alarms
    async fn check_restart_count(
        &self,
        container: &deployment_containers::Model,
        deployment: &deployments::Model,
        info: &temps_deployer::ContainerInfo,
    ) {
        let current_restart_count = info.restart_count.unwrap_or(0);

        let mut states = self.container_states.write().await;
        let previous = states.get(&container.id).cloned();

        // Update cached state (preserve network counters if already present)
        let prev_net = previous
            .as_ref()
            .map(|p| (p.last_net_rx_bytes, p.last_net_tx_bytes))
            .unwrap_or((0, 0));
        states.insert(
            container.id,
            ContainerState {
                restart_count: current_restart_count,
                last_net_rx_bytes: prev_net.0,
                last_net_tx_bytes: prev_net.1,
            },
        );

        drop(states);

        // On first check, just record the baseline
        let Some(prev) = previous else {
            debug!(
                "Container {} ({}) baseline restart_count={}",
                container.id, container.container_name, current_restart_count
            );
            return;
        };

        let delta = current_restart_count - prev.restart_count;
        if delta <= 0 {
            return;
        }

        warn!(
            "Container {} ({}) restarted {} time(s) (total: {})",
            container.id, container.container_name, delta, current_restart_count
        );

        let severity = if current_restart_count >= 10 {
            AlarmSeverity::Critical
        } else if current_restart_count >= 3 {
            AlarmSeverity::Warning
        } else {
            AlarmSeverity::Info
        };

        let request = FireAlarmRequest {
            project_id: Some(deployment.project_id),
            environment_id: Some(deployment.environment_id),
            deployment_id: Some(deployment.id),
            container_id: Some(container.id),
            service_id: None,
            alarm_type: AlarmType::ContainerRestart,
            severity,
            title: format!(
                "Container '{}' restarted {} time(s)",
                container.container_name, delta
            ),
            message: format!(
                "Container '{}' has restarted. Total restart count: {}. \
                 This may indicate a crash loop or OOM kill.",
                container.container_name, current_restart_count
            ),
            metadata: Some(serde_json::json!({
                "container_name": container.container_name,
                "container_id": container.container_id,
                "restart_count": current_restart_count,
                "restart_delta": delta,
                "previous_restart_count": prev.restart_count,
            })),
        };

        if let Err(e) = self.alarm_service.fire_alarm(request).await {
            error!(
                "Failed to fire restart alarm for container {}: {}",
                container.id, e
            );
        }
    }

    /// Detect containers that have exited or died
    async fn check_container_status(
        &self,
        container: &deployment_containers::Model,
        deployment: &deployments::Model,
        info: &temps_deployer::ContainerInfo,
    ) {
        let status_str = info.status.to_string();

        match &info.status {
            temps_deployer::ContainerStatus::Exited | temps_deployer::ContainerStatus::Dead => {
                // Persist the exit metadata first so the UI/API can surface
                // *why* even if the alarm path is skipped (e.g. on-demand
                // sleep).
                let intentional = exit_is_intentional(container, info);
                self.persist_exit_info(container, info, intentional).await;

                // A user-initiated stop (`stop_container`/`stop_all_containers`)
                // and a failed deployment retained for log inspection are both
                // stopped on purpose — an exit there is expected, not a crash.
                if intentional {
                    debug!(
                        "Container {} ({}) is {} but was intentionally stopped (status: {:?}), skipping alarm",
                        container.id, container.container_name, status_str, container.status
                    );
                    return;
                }

                // Only the deployment currently serving the environment is
                // expected to be running. A container row left live under a
                // superseded deployment (e.g. its teardown failed) is an
                // orphan awaiting cleanup by the next deployment's sweep;
                // alarming on it every poll cycle would page forever about a
                // container nobody routes to. It also covers on-demand
                // environments that were intentionally put to sleep.
                if let Some(reason) = self.exit_not_alarmable_reason(deployment).await {
                    debug!(
                        "Container {} ({}) is {} but {}, skipping alarm",
                        container.id, container.container_name, status_str, reason
                    );
                    return;
                }

                // Skip alarm if the deployment was intentionally paused by the
                // user — `pause_deployment` stops containers on purpose, so
                // this exit is expected, not a crash. Re-read the deployment
                // live rather than trusting `deployment.state`: that value is
                // a snapshot batch-loaded once at the start of the current
                // poll cycle (see `check_all_containers`), so a pause that
                // commits mid-cycle — after the snapshot was taken but before
                // this container is reached — would otherwise still look
                // unpaused here.
                if self.is_deployment_paused(deployment.id).await {
                    debug!(
                        "Container {} ({}) is {} but deployment {} is paused, skipping alarm",
                        container.id, container.container_name, status_str, deployment.id
                    );
                    return;
                }

                warn!(
                    "Container {} ({}) is in '{}' state (reason: {})",
                    container.id,
                    container.container_name,
                    status_str,
                    info.exit_reason.as_deref().unwrap_or("unknown")
                );

                // Pick OOM alarm only when Docker actually flagged OOMKilled;
                // a plain non-zero exit is a different signal.
                let alarm_type = if info.oom_killed == Some(true) {
                    AlarmType::ContainerOomKilled
                } else {
                    AlarmType::ContainerCrash
                };
                let severity = AlarmSeverity::Critical;

                let exit_reason_str = info
                    .exit_reason
                    .clone()
                    .unwrap_or_else(|| status_str.clone());

                let request = FireAlarmRequest {
                    project_id: Some(deployment.project_id),
                    environment_id: Some(deployment.environment_id),
                    deployment_id: Some(deployment.id),
                    container_id: Some(container.id),
                    service_id: None,
                    alarm_type,
                    severity,
                    title: format!(
                        "Container '{}' is {}: {}",
                        container.container_name, status_str, exit_reason_str
                    ),
                    message: format!(
                        "Container '{}' has exited or died unexpectedly. Status: {}. Reason: {}",
                        container.container_name, status_str, exit_reason_str
                    ),
                    metadata: Some(serde_json::json!({
                        "container_name": container.container_name,
                        "container_id": container.container_id,
                        "status": status_str,
                        "exit_code": info.exit_code,
                        "exit_reason": info.exit_reason,
                        "oom_killed": info.oom_killed,
                        "error_message": info.error_message,
                        "finished_at": info.finished_at.map(|d| d.to_rfc3339()),
                    })),
                };

                if let Err(e) = self.alarm_service.fire_alarm(request).await {
                    error!(
                        "Failed to fire status alarm for container {}: {}",
                        container.id, e
                    );
                }
            }
            temps_deployer::ContainerStatus::Running
                if container.status.as_deref() == Some("stopped") =>
            {
                // A user-stopped container is running again (started by
                // hand, or by Docker's restart policy). Drop the stale
                // marker, otherwise its next crash would be mistaken for
                // the earlier intentional stop and never alarm.
                self.clear_user_stop_marker(container).await;
            }
            _ => {
                // Container is in a healthy state, nothing to do
            }
        }
    }

    /// Replace a `"stopped"` marker with `"running"`. Conditional on the
    /// marker still being there so a concurrent stop is never overwritten.
    async fn clear_user_stop_marker(&self, container: &deployment_containers::Model) {
        let result = deployment_containers::Entity::update_many()
            .col_expr(
                deployment_containers::Column::Status,
                Expr::value("running"),
            )
            .filter(deployment_containers::Column::Id.eq(container.id))
            .filter(deployment_containers::Column::Status.eq("stopped"))
            .exec(self.db.as_ref())
            .await;
        if let Err(e) = result {
            error!(
                "Failed to clear stopped marker for running container {} ({}): {}",
                container.id, container.container_name, e
            );
        }
    }

    /// Write Docker's exit metadata onto the deployment_containers row so the
    /// API can return it long after the alarm fires. Skips writes when nothing
    /// changed, so this is safe to call every poll cycle.
    async fn persist_exit_info(
        &self,
        container: &deployment_containers::Model,
        info: &temps_deployer::ContainerInfo,
        intentional: bool,
    ) {
        // Keep an intentional-stop marker: overwriting "stopped" or
        // "retained:*" with Docker's "exited" would erase why the container
        // is down and turn the next poll's exit into a crash alarm. A stale
        // marker (the container ran again since) is replaced.
        let new_status = if intentional {
            container.status.clone()
        } else {
            Some(info.status.to_string())
        };
        // While the marker stands, `finished_at` is the stop time the
        // restart check compares against: never erase it with a missing value.
        let finished_at = if intentional {
            info.finished_at.or(container.finished_at)
        } else {
            info.finished_at
        };
        let unchanged = container.status == new_status
            && container.exit_code == info.exit_code
            && container.exit_reason == info.exit_reason
            && container.oom_killed == info.oom_killed
            && container.error_message == info.error_message
            && container.finished_at == finished_at;
        if unchanged {
            return;
        }

        let active = deployment_containers::ActiveModel {
            id: Set(container.id),
            status: Set(new_status),
            exit_code: Set(info.exit_code),
            exit_reason: Set(info.exit_reason.clone()),
            oom_killed: Set(info.oom_killed),
            error_message: Set(info.error_message.clone()),
            finished_at: Set(finished_at),
            ..Default::default()
        };

        if let Err(e) = deployment_containers::Entity::update(active)
            .exec(self.db.as_ref())
            .await
        {
            error!(
                "Failed to persist exit info for container {} ({}): {}",
                container.id, container.container_name, e
            );
        }
    }

    /// Returns why an exited container of `deployment` must not raise an
    /// alarm, based on a live read of its environment: the environment is
    /// on-demand sleeping (scale-to-zero stops containers on idle), was
    /// deleted, or is served by a different deployment. Returns `None` when
    /// the exit is unexpected — including when the environment cannot be
    /// read, since an unreadable state must not silence a real crash.
    async fn exit_not_alarmable_reason(&self, deployment: &deployments::Model) -> Option<String> {
        use temps_entities::environments;

        let env = match environments::Entity::find_by_id(deployment.environment_id)
            .one(self.db.as_ref())
            .await
        {
            Ok(Some(env)) => env,
            Ok(None) => return None,
            Err(error) => {
                warn!(
                    environment_id = deployment.environment_id,
                    %error,
                    "Failed to read environment while evaluating a container exit; alarming"
                );
                return None;
            }
        };
        if env.sleeping {
            return Some(format!(
                "environment {} is on-demand sleeping",
                deployment.environment_id
            ));
        }
        if env.deleted_at.is_some() {
            return Some(format!(
                "environment {} was deleted",
                deployment.environment_id
            ));
        }
        if env.current_deployment_id != Some(deployment.id) {
            return Some(format!(
                "deployment {} is not the environment's current deployment ({:?})",
                deployment.id, env.current_deployment_id
            ));
        }
        None
    }

    /// Check if a deployment is currently paused. Always a live read (never
    /// cached or batch-snapshotted) since this feeds an alarm-suppression
    /// decision that must reflect `pause_deployment`'s state at the moment
    /// the alarm would fire, not at the start of the poll cycle.
    async fn is_deployment_paused(&self, deployment_id: i32) -> bool {
        deployments::Entity::find_by_id(deployment_id)
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten()
            .map(|d| d.state == "paused")
            .unwrap_or(false)
    }

    /// Check CPU and memory usage against thresholds
    async fn check_resource_usage(
        &self,
        container: &deployment_containers::Model,
        deployment: &deployments::Model,
        deployer: &dyn ContainerDeployer,
    ) {
        let stats = match deployer.get_container_stats(&container.container_id).await {
            Ok(s) => s,
            Err(e) => {
                debug!(
                    "Failed to get stats for container {} ({}): {}",
                    container.id, container.container_name, e
                );
                return;
            }
        };

        self.check_resource_stats(container, deployment, &stats)
            .await;
    }

    async fn check_resource_stats(
        &self,
        container: &deployment_containers::Model,
        deployment: &deployments::Model,
        stats: &temps_deployer::ContainerStats,
    ) {
        // Check CPU.
        //
        // `stats.cpu_percent` is the raw Docker number where 100% == one core, so
        // a container *allowed* 2 cores can hit 200% while only being 100%
        // utilised. We must compare the threshold against utilisation relative to
        // the CPU the container is allowed to use — its explicit limit when it
        // has one (otherwise a 2-core container fires at ~95% raw, ≈47% of its
        // limit), and the host's core count when it doesn't (otherwise an
        // uncapped container using 1 of 8 cores fires at "100%" on an idle host).
        // `cpu_used_cores` (= raw% / 100) is surfaced alongside so the alarm is
        // actionable.
        let cpu_utilization = stats.cpu_utilization_percent();
        let cpu_used_cores = stats.cpu_percent / 100.0;
        if cpu_utilization > self.config.cpu_threshold_percent {
            let capacity_label = match (stats.cpu_limit_cores, stats.online_cpus) {
                (Some(cores), _) if cores > 0.0 => format!("its {cores:.2}-core limit"),
                (_, Some(cpus)) if cpus > 0 => {
                    format!("the {cpus} cores on this host (no CPU limit set)")
                }
                _ => "one core (no CPU limit set, host core count unknown)".to_string(),
            };
            self.handle_resource_threshold(
                container,
                deployment,
                AlarmType::HighCpu,
                AlarmSeverity::Warning,
                format!(
                    "Container '{}' CPU at {:.0}% of available capacity",
                    container.container_name, cpu_utilization
                ),
                format!(
                    "Container '{}' is using {:.2} cores — {:.0}% of {}, above the {:.0}% threshold.",
                    container.container_name,
                    cpu_used_cores,
                    cpu_utilization,
                    capacity_label,
                    self.config.cpu_threshold_percent,
                ),
                serde_json::json!({
                    "container_name": container.container_name,
                    // Utilisation relative to the CPU the container may use — what
                    // the threshold is compared against.
                    "cpu_utilization_percent": cpu_utilization,
                    // Raw Docker percentage (100% == one core) and the cores it maps to.
                    "cpu_percent": stats.cpu_percent,
                    "cpu_used_cores": cpu_used_cores,
                    "cpu_limit_cores": stats.cpu_limit_cores,
                    // Host cores — the ceiling used when no limit is configured.
                    "online_cpus": stats.online_cpus,
                    "cpu_ceiling_cores": stats.cpu_ceiling_cores(),
                    "threshold_percent": self.config.cpu_threshold_percent,
                }),
            )
            .await;
        } else {
            self.reset_resource_counter(container.id, AlarmType::HighCpu.as_str())
                .await;
        }

        // Check memory
        if let Some(mem_percent) = stats.memory_percent {
            if mem_percent > self.config.memory_threshold_percent {
                self.handle_resource_threshold(
                    container,
                    deployment,
                    AlarmType::HighMemory,
                    AlarmSeverity::Warning,
                    format!(
                        "Container '{}' memory at {:.1}%",
                        container.container_name, mem_percent
                    ),
                    format!(
                        "Container '{}' memory usage is at {:.1}% ({:.0} MB), above the {:.0}% threshold.",
                        container.container_name,
                        mem_percent,
                        stats.memory_bytes as f64 / 1024.0 / 1024.0,
                        self.config.memory_threshold_percent,
                    ),
                    serde_json::json!({
                        "container_name": container.container_name,
                        "memory_percent": mem_percent,
                        "memory_bytes": stats.memory_bytes,
                        "memory_limit_bytes": stats.memory_limit_bytes,
                        "threshold_percent": self.config.memory_threshold_percent,
                    }),
                )
                .await;
            } else {
                self.reset_resource_counter(container.id, AlarmType::HighMemory.as_str())
                    .await;
            }
        }

        // Write container resource metrics to the metrics store (if configured).
        // This is non-fatal — metric write failures are logged as warnings only.
        if let Some(store) = &self.metrics_store {
            self.write_container_metrics(store, container, deployment, stats)
                .await;
        }
    }

    /// Emit container resource metric points to the metrics store.
    ///
    /// Writes:
    /// - `container.cpu_percent` (Gauge)
    /// - `container.memory_used_bytes` (Gauge)
    /// - `container.memory_percent` (Gauge, when limit is known)
    /// - `container.network_rx_bytes_delta` (Gauge — bytes received since last poll)
    /// - `container.network_tx_bytes_delta` (Gauge — bytes transmitted since last poll)
    ///
    /// Network metrics are emitted as **deltas** (bytes since the previous poll)
    /// rather than raw cumulative counters.  This keeps TimescaleDB rollups
    /// meaningful — `SUM(value)` over a time window gives total bytes, not a
    /// meaningless sum of ever-growing counters.  On the first poll for a
    /// container the previous baseline is 0, so the delta equals the raw value;
    /// this is a known acceptable overcount for the very first data point.
    async fn write_container_metrics(
        &self,
        store: &Arc<dyn MetricsStore>,
        container: &deployment_containers::Model,
        deployment: &deployments::Model,
        stats: &temps_deployer::ContainerStats,
    ) {
        let now = chrono::Utc::now();
        let container_id = container.id;
        let node_id = container.node_id;

        let mut labels = std::collections::HashMap::new();
        labels.insert("project_id".into(), deployment.project_id.to_string());
        labels.insert(
            "environment_id".into(),
            deployment.environment_id.to_string(),
        );
        labels.insert("deployment_id".into(), deployment.id.to_string());
        labels.insert("container_name".into(), container.container_name.clone());
        if let Some(svc) = &container.service_name {
            labels.insert("service_name".into(), svc.clone());
        }

        let make_point = |name: &str, value: f64, kind: MetricKind| MetricPoint {
            time: now,
            source_kind: SourceKind::Container,
            source_id: container_id,
            name: name.to_string(),
            value,
            kind,
            engine: None,
            environment: None,
            node_id,
            labels: labels.clone(),
        };

        let mut points = vec![
            // Raw Docker CPU percentage (100% == one core). Drives the
            // "cores in use" view; NOT directly comparable to a flat threshold.
            make_point(
                "container.cpu_percent",
                stats.cpu_percent,
                MetricKind::Gauge,
            ),
            // CPU usage relative to the CPU the container may use — its limit
            // when set, otherwise every core on the host (100% == that capacity
            // fully saturated). This is the metric alert rules should threshold
            // against — see `container_default_seeds()` in the evaluator.
            make_point(
                "container.cpu_utilization_percent",
                stats.cpu_utilization_percent(),
                MetricKind::Gauge,
            ),
            make_point(
                "container.memory_used_bytes",
                stats.memory_bytes as f64,
                MetricKind::Gauge,
            ),
        ];

        // Surface the configured CPU limit so dashboards can render "used / limit".
        if let Some(limit_cores) = stats.cpu_limit_cores {
            points.push(make_point(
                "container.cpu_limit_cores",
                limit_cores,
                MetricKind::Gauge,
            ));
        }

        if let Some(mem_pct) = stats.memory_percent {
            points.push(make_point(
                "container.memory_percent",
                mem_pct,
                MetricKind::Gauge,
            ));
        }

        // Compute network byte deltas from the cached previous values and update
        // the cache.  Writing raw cumulative counters into TimescaleDB produces
        // meaningless rollup aggregates — delta gauges are what dashboards need.
        let (net_rx_delta, net_tx_delta) = {
            let mut states = self.container_states.write().await;
            if let Some(state) = states.get_mut(&container_id) {
                let rx_delta = if stats.network_rx_bytes >= state.last_net_rx_bytes {
                    stats.network_rx_bytes - state.last_net_rx_bytes
                } else {
                    // Counter reset (container restarted) — use raw value as delta.
                    stats.network_rx_bytes
                };
                let tx_delta = if stats.network_tx_bytes >= state.last_net_tx_bytes {
                    stats.network_tx_bytes - state.last_net_tx_bytes
                } else {
                    stats.network_tx_bytes
                };
                state.last_net_rx_bytes = stats.network_rx_bytes;
                state.last_net_tx_bytes = stats.network_tx_bytes;
                (rx_delta as f64, tx_delta as f64)
            } else {
                // No prior state — baseline is 0, delta = raw value.
                (stats.network_rx_bytes as f64, stats.network_tx_bytes as f64)
            }
        };

        points.push(make_point(
            "container.network_rx_bytes_delta",
            net_rx_delta,
            MetricKind::Gauge,
        ));
        points.push(make_point(
            "container.network_tx_bytes_delta",
            net_tx_delta,
            MetricKind::Gauge,
        ));

        store
            .write_batch(points)
            .await
            .unwrap_or_else(|e| warn!("container metrics write for container {container_id}: {e}"));
    }

    /// Handle a resource threshold breach. Only fires alarm after N consecutive breaches.
    #[allow(clippy::too_many_arguments)]
    async fn handle_resource_threshold(
        &self,
        container: &deployment_containers::Model,
        deployment: &deployments::Model,
        alarm_type: AlarmType,
        severity: AlarmSeverity,
        title: String,
        message: String,
        metadata: serde_json::Value,
    ) {
        let key = (container.id, alarm_type.as_str());

        let count = {
            let mut counters = self.resource_counters.write().await;
            let counter = counters.entry(key).or_insert(0);
            *counter += 1;
            *counter
        };

        if count < self.config.consecutive_threshold_checks {
            debug!(
                "Container {} resource threshold breach {}/{} for {}",
                container.id,
                count,
                self.config.consecutive_threshold_checks,
                alarm_type.as_str()
            );
            return;
        }

        let request = FireAlarmRequest {
            project_id: Some(deployment.project_id),
            environment_id: Some(deployment.environment_id),
            deployment_id: Some(deployment.id),
            container_id: Some(container.id),
            service_id: None,
            alarm_type,
            severity,
            title,
            message,
            metadata: Some(metadata),
        };

        if let Err(e) = self.alarm_service.fire_alarm(request).await {
            error!(
                "Failed to fire resource alarm for container {}: {}",
                container.id, e
            );
        }

        // Reset counter after firing (cooldown in AlarmService prevents spam)
        self.reset_resource_counter(container.id, alarm_type.as_str())
            .await;
    }

    /// Reset the consecutive counter for a resource type
    async fn reset_resource_counter(&self, container_id: i32, alarm_type: &'static str) {
        let mut counters = self.resource_counters.write().await;
        counters.remove(&(container_id, alarm_type));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alarm_service::AlarmService;
    use async_trait::async_trait;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use temps_core::jobs::QueueError;
    use temps_core::notifications::{
        EmailMessage, NotificationData, NotificationError, NotificationService,
    };
    use temps_deployer::{
        ContainerInfo, ContainerStats, ContainerStatus, DeployRequest, DeployResult, DeployerError,
    };

    // ── Mock helpers ──────────────────────────────────────────────────

    struct NoopNotificationService;

    #[async_trait]
    impl NotificationService for NoopNotificationService {
        async fn send_notification(
            &self,
            _notification: NotificationData,
        ) -> Result<(), NotificationError> {
            Ok(())
        }
        async fn send_email(&self, _message: EmailMessage) -> Result<(), NotificationError> {
            Ok(())
        }
        async fn is_configured(&self) -> Result<bool, NotificationError> {
            Ok(false)
        }
    }

    struct NoopJobQueue;

    #[async_trait]
    impl temps_core::JobQueue for NoopJobQueue {
        async fn send(&self, _job: temps_core::Job) -> Result<(), QueueError> {
            Ok(())
        }
        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            unimplemented!()
        }
    }

    /// Mock ContainerDeployer that returns configurable container info and stats
    struct MockDeployer {
        info: tokio::sync::Mutex<ContainerInfo>,
        stats: tokio::sync::Mutex<ContainerStats>,
        info_calls: AtomicUsize,
        stats_calls: AtomicUsize,
        block_info: AtomicBool,
        fail_info: AtomicBool,
    }

    #[allow(dead_code)]
    impl MockDeployer {
        fn new(restart_count: i64, status: ContainerStatus) -> Self {
            Self {
                info: tokio::sync::Mutex::new(ContainerInfo {
                    container_id: "abc123".to_string(),
                    container_name: "test-container".to_string(),
                    image_name: "test-image:latest".to_string(),
                    status,
                    created_at: chrono::Utc::now(),
                    ports: vec![],
                    environment_vars: std::collections::HashMap::new(),
                    restart_count: Some(restart_count),
                    labels: std::collections::HashMap::new(),
                    ..Default::default()
                }),
                stats: tokio::sync::Mutex::new(ContainerStats {
                    container_id: "abc123".to_string(),
                    container_name: "test-container".to_string(),
                    cpu_percent: 10.0,
                    memory_bytes: 100 * 1024 * 1024,
                    memory_limit_bytes: Some(512 * 1024 * 1024),
                    memory_percent: Some(19.5),
                    network_rx_bytes: 0,
                    network_tx_bytes: 0,
                    timestamp: chrono::Utc::now(),
                    ..Default::default()
                }),
                info_calls: AtomicUsize::new(0),
                stats_calls: AtomicUsize::new(0),
                block_info: AtomicBool::new(false),
                fail_info: AtomicBool::new(false),
            }
        }

        async fn set_restart_count(&self, count: i64) {
            self.info.lock().await.restart_count = Some(count);
        }

        async fn set_status(&self, status: ContainerStatus) {
            self.info.lock().await.status = status;
        }

        async fn set_cpu_percent(&self, percent: f64) {
            self.stats.lock().await.cpu_percent = percent;
        }

        /// Set the raw Docker CPU percentage together with the CPU the
        /// container is allowed to use (limit if any, plus the host's cores).
        async fn set_cpu(&self, percent: f64, limit_cores: Option<f64>, online_cpus: u32) {
            let mut stats = self.stats.lock().await;
            stats.cpu_percent = percent;
            stats.cpu_limit_cores = limit_cores;
            stats.online_cpus = Some(online_cpus);
        }

        async fn set_memory_percent(&self, percent: f64) {
            self.stats.lock().await.memory_percent = Some(percent);
        }

        fn block_container_info(&self) {
            self.block_info.store(true, Ordering::Relaxed);
        }

        fn fail_container_info(&self) {
            self.fail_info.store(true, Ordering::Relaxed);
        }
    }

    #[async_trait]
    impl ContainerDeployer for MockDeployer {
        async fn deploy_container(
            &self,
            _request: DeployRequest,
        ) -> Result<DeployResult, DeployerError> {
            unimplemented!()
        }
        async fn start_container(&self, _id: &str) -> Result<(), DeployerError> {
            unimplemented!()
        }
        async fn stop_container(&self, _id: &str) -> Result<(), DeployerError> {
            unimplemented!()
        }
        async fn pause_container(&self, _id: &str) -> Result<(), DeployerError> {
            unimplemented!()
        }
        async fn resume_container(&self, _id: &str) -> Result<(), DeployerError> {
            unimplemented!()
        }
        async fn remove_container(&self, _id: &str) -> Result<(), DeployerError> {
            unimplemented!()
        }
        async fn get_container_info(&self, _id: &str) -> Result<ContainerInfo, DeployerError> {
            self.info_calls.fetch_add(1, Ordering::Relaxed);
            if self.block_info.load(Ordering::Relaxed) {
                std::future::pending::<()>().await;
            }
            if self.fail_info.load(Ordering::Relaxed) {
                return Err(DeployerError::NetworkError(
                    "transient inspection failure".to_string(),
                ));
            }
            Ok(self.info.lock().await.clone())
        }
        async fn get_container_stats(&self, _id: &str) -> Result<ContainerStats, DeployerError> {
            self.stats_calls.fetch_add(1, Ordering::Relaxed);
            Ok(self.stats.lock().await.clone())
        }
        async fn list_containers(&self) -> Result<Vec<ContainerInfo>, DeployerError> {
            Ok(vec![])
        }
        async fn get_container_logs(&self, _id: &str) -> Result<String, DeployerError> {
            Ok(String::new())
        }
        async fn stream_container_logs(
            &self,
            _id: &str,
        ) -> Result<Box<dyn futures::Stream<Item = String> + Unpin + Send>, DeployerError> {
            unimplemented!()
        }
    }

    struct MockRuntimeResolver {
        runtimes: HashMap<i32, Arc<dyn ContainerDeployer>>,
        failed_nodes: std::collections::HashSet<i32>,
        calls: tokio::sync::Mutex<HashMap<i32, usize>>,
    }

    #[async_trait]
    impl ContainerRuntimeResolver for MockRuntimeResolver {
        async fn resolve_runtime(
            &self,
            node_id: i32,
        ) -> Result<Arc<dyn ContainerDeployer>, ContainerRuntimeResolutionError> {
            *self.calls.lock().await.entry(node_id).or_default() += 1;
            if self.failed_nodes.contains(&node_id) {
                return Err(ContainerRuntimeResolutionError {
                    node_id,
                    reason: "worker is unreachable".to_string(),
                });
            }
            self.runtimes
                .get(&node_id)
                .cloned()
                .ok_or_else(|| ContainerRuntimeResolutionError {
                    node_id,
                    reason: "worker is not configured in the test resolver".to_string(),
                })
        }
    }

    fn make_container_model(id: i32) -> deployment_containers::Model {
        deployment_containers::Model {
            id,
            deployment_id: 10,
            container_id: "abc123".to_string(),
            container_name: "test-container".to_string(),
            container_port: 3000,
            host_port: Some(8080),
            image_name: Some("test-image:latest".to_string()),
            status: Some("running".to_string()),
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
        }
    }

    fn make_deployment_model() -> deployments::Model {
        deployments::Model {
            id: 10,
            project_id: 1,
            environment_id: 1,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            slug: "deploy-abc".to_string(),
            state: "ready".to_string(),
            metadata: None,
            deploying_at: None,
            ready_at: None,
            started_at: None,
            finished_at: None,
            context_vars: None,
            branch_ref: None,
            tag_ref: None,
            commit_sha: None,
            commit_message: None,
            commit_author: None,
            commit_json: None,
            cancelled_reason: None,
            static_dir_location: None,
            screenshot_location: None,
            image_name: None,
            deployment_config: None,
            promoted_from_deployment_id: None,
            upload_request_id: None,
            docker_socket_mounted: false,
        }
    }

    fn make_alarm_service(db: Arc<sea_orm::DatabaseConnection>) -> Arc<AlarmService> {
        Arc::new(AlarmService::new(
            db,
            Arc::new(NoopNotificationService),
            Arc::new(NoopJobQueue),
        ))
    }

    #[tokio::test]
    async fn unresponsive_worker_does_not_stall_healthy_local_or_remote_containers() {
        let slow_worker = Arc::new(MockDeployer::new(0, ContainerStatus::Running));
        slow_worker.block_container_info();
        let healthy_local = Arc::new(MockDeployer::new(0, ContainerStatus::Running));
        let healthy_worker = Arc::new(MockDeployer::new(0, ContainerStatus::Running));
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let config = ContainerHealthConfig {
            worker_check_timeout_ms: 25,
            ..ContainerHealthConfig::default()
        };
        let monitor = ContainerHealthMonitor::new(
            db.clone(),
            healthy_local.clone(),
            make_alarm_service(db),
            config,
        );
        let local_container = make_container_model(2);
        let mut healthy_worker_container = make_container_model(3);
        healthy_worker_container.node_id = Some(8);
        let deployment = make_deployment_model();
        let mut jobs = Vec::new();
        for id in 10..27 {
            let mut worker_container = make_container_model(id);
            worker_container.node_id = Some(7);
            jobs.push(ContainerCheckJob {
                container: worker_container,
                deployment: deployment.clone(),
                deployer: slow_worker.clone(),
                is_remote: true,
            });
        }
        jobs.extend([
            ContainerCheckJob {
                container: local_container,
                deployment: deployment.clone(),
                deployer: healthy_local.clone(),
                is_remote: false,
            },
            ContainerCheckJob {
                container: healthy_worker_container,
                deployment: make_deployment_model(),
                deployer: healthy_worker.clone(),
                is_remote: true,
            },
        ]);

        tokio::time::timeout(
            tokio::time::Duration::from_millis(100),
            monitor.check_container_jobs(jobs),
        )
        .await
        .expect("slow worker must be bounded by its own deadline");

        assert_eq!(slow_worker.info_calls.load(Ordering::Relaxed), 17);
        assert_eq!(healthy_local.info_calls.load(Ordering::Relaxed), 1);
        assert_eq!(healthy_local.stats_calls.load(Ordering::Relaxed), 1);
        assert_eq!(healthy_worker.info_calls.load(Ordering::Relaxed), 1);
        assert_eq!(healthy_worker.stats_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn terminal_worker_container_processes_exit_without_requesting_stats() {
        let deployer = Arc::new(MockDeployer::new(0, ContainerStatus::Exited));
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let monitor = ContainerHealthMonitor::new(
            db.clone(),
            deployer.clone(),
            make_alarm_service(db),
            ContainerHealthConfig {
                worker_check_timeout_ms: 25,
                ..ContainerHealthConfig::default()
            },
        );
        let mut container = make_container_model(1);
        container.node_id = Some(7);

        monitor
            .check_remote_container(&container, &make_deployment_model(), deployer.as_ref())
            .await
            .unwrap();

        assert_eq!(deployer.info_calls.load(Ordering::Relaxed), 1);
        assert_eq!(deployer.stats_calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn worker_runtime_is_cached_per_cycle_and_used_for_info_and_stats() {
        let local = Arc::new(MockDeployer::new(0, ContainerStatus::Running));
        let worker = Arc::new(MockDeployer::new(0, ContainerStatus::Running));
        let resolver = Arc::new(MockRuntimeResolver {
            runtimes: HashMap::from([(7, worker.clone() as Arc<dyn ContainerDeployer>)]),
            failed_nodes: std::collections::HashSet::new(),
            calls: tokio::sync::Mutex::new(HashMap::new()),
        });
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let monitor = ContainerHealthMonitor::new(
            db.clone(),
            local.clone(),
            make_alarm_service(db),
            ContainerHealthConfig::default(),
        )
        .with_runtime_resolver(resolver.clone());
        let mut cache = RuntimeCache::new();

        let first = monitor.runtime_for_node(Some(7), &mut cache).await.unwrap();
        let second = monitor.runtime_for_node(Some(7), &mut cache).await.unwrap();
        first.get_container_info("abc123").await.unwrap();
        second.get_container_stats("abc123").await.unwrap();

        assert_eq!(resolver.calls.lock().await.get(&7), Some(&1));
        assert_eq!(worker.info_calls.load(Ordering::Relaxed), 1);
        assert_eq!(worker.stats_calls.load(Ordering::Relaxed), 1);
        assert_eq!(local.info_calls.load(Ordering::Relaxed), 0);
        assert_eq!(local.stats_calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn failed_worker_resolution_is_cached_without_blocking_other_nodes() {
        let local = Arc::new(MockDeployer::new(0, ContainerStatus::Running));
        let healthy_worker = Arc::new(MockDeployer::new(0, ContainerStatus::Running));
        let resolver = Arc::new(MockRuntimeResolver {
            runtimes: HashMap::from([(8, healthy_worker.clone() as Arc<dyn ContainerDeployer>)]),
            failed_nodes: std::collections::HashSet::from([7]),
            calls: tokio::sync::Mutex::new(HashMap::new()),
        });
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let monitor = ContainerHealthMonitor::new(
            db.clone(),
            local,
            make_alarm_service(db),
            ContainerHealthConfig::default(),
        )
        .with_runtime_resolver(resolver.clone());
        let mut cache = RuntimeCache::new();

        assert!(monitor.runtime_for_node(Some(7), &mut cache).await.is_err());
        assert!(monitor.runtime_for_node(Some(7), &mut cache).await.is_err());
        let healthy = monitor.runtime_for_node(Some(8), &mut cache).await.unwrap();
        healthy.get_container_info("abc123").await.unwrap();

        let calls = resolver.calls.lock().await;
        assert_eq!(calls.get(&7), Some(&1));
        assert_eq!(calls.get(&8), Some(&1));
        assert_eq!(healthy_worker.info_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn runtime_port_change_updates_row_and_notifies_routes_atomically() {
        let container = make_container_model(1);
        let mut updated = container.clone();
        updated.host_port = Some(32001);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([[updated]])
                .append_exec_results([sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .into_connection(),
        );
        let deployer = Arc::new(MockDeployer::new(0, ContainerStatus::Running));
        let mut info = deployer.get_container_info("abc123").await.unwrap();
        info.ports = vec![temps_deployer::PortMapping {
            host_port: 32001,
            container_port: 3000,
            protocol: temps_deployer::Protocol::Tcp,
            host_ip: None,
        }];
        let monitor = ContainerHealthMonitor::new(
            db.clone(),
            deployer,
            make_alarm_service(db.clone()),
            ContainerHealthConfig::default(),
        );
        monitor
            .persist_runtime_info(&container, &info)
            .await
            .unwrap();
        drop(monitor);
        let transactions = Arc::try_unwrap(db).unwrap().into_transaction_log();
        let sql = format!("{transactions:?}");
        assert!(
            sql.contains("32001"),
            "new published port must be persisted: {sql}"
        );
        assert!(
            sql.contains("pg_notify"),
            "proxy must reload after port change: {sql}"
        );
        assert_eq!(
            transactions.len(),
            1,
            "update and notification share a transaction"
        );
    }

    #[tokio::test]
    async fn authoritative_missing_mapping_clears_host_port_and_notifies_routes() {
        let invalid_mappings = [
            vec![],
            vec![temps_deployer::PortMapping {
                host_port: 32001,
                container_port: 9000,
                protocol: temps_deployer::Protocol::Tcp,
                host_ip: None,
            }],
            vec![temps_deployer::PortMapping {
                host_port: 32001,
                container_port: 3000,
                protocol: temps_deployer::Protocol::Udp,
                host_ip: None,
            }],
            vec![temps_deployer::PortMapping {
                host_port: 0,
                container_port: 3000,
                protocol: temps_deployer::Protocol::Tcp,
                host_ip: None,
            }],
        ];

        for ports in invalid_mappings {
            let container = make_container_model(1);
            let mut updated = container.clone();
            updated.host_port = None;
            let db = Arc::new(
                MockDatabase::new(DatabaseBackend::Postgres)
                    .append_query_results([[updated]])
                    .append_exec_results([sea_orm::MockExecResult {
                        last_insert_id: 0,
                        rows_affected: 1,
                    }])
                    .into_connection(),
            );
            let deployer = Arc::new(MockDeployer::new(0, ContainerStatus::Running));
            let mut info = deployer.get_container_info("abc123").await.unwrap();
            info.ports = ports;
            let monitor = ContainerHealthMonitor::new(
                db.clone(),
                deployer,
                make_alarm_service(db.clone()),
                ContainerHealthConfig::default(),
            );

            monitor
                .persist_runtime_info(&container, &info)
                .await
                .unwrap();
            drop(monitor);

            let transactions = Arc::try_unwrap(db).unwrap().into_transaction_log();
            let sql = format!("{transactions:?}");
            assert!(
                sql.contains("host_port"),
                "host port must be cleared: {sql}"
            );
            assert!(sql.contains("pg_notify"), "routes must reload: {sql}");
            assert_eq!(transactions.len(), 1, "clear and notify must be atomic");
        }
    }

    #[tokio::test]
    async fn transient_inspection_failure_preserves_recorded_host_port() {
        let container = make_container_model(1);
        let deployment = make_deployment_model();
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let deployer = Arc::new(MockDeployer::new(0, ContainerStatus::Running));
        deployer.fail_container_info();
        let monitor = ContainerHealthMonitor::new(
            db.clone(),
            deployer,
            make_alarm_service(db.clone()),
            ContainerHealthConfig::default(),
        );

        assert!(monitor
            .check_container(&container, &deployment, monitor.deployer.as_ref())
            .await
            .is_err());
        drop(monitor);

        assert!(Arc::try_unwrap(db)
            .unwrap()
            .into_transaction_log()
            .is_empty());
    }

    #[test]
    fn published_port_selection_is_deterministic_across_interface_bindings() {
        let mappings = vec![
            temps_deployer::PortMapping {
                host_port: 32003,
                container_port: 3000,
                protocol: temps_deployer::Protocol::Tcp,
                host_ip: Some("192.0.2.10".to_string()),
            },
            temps_deployer::PortMapping {
                host_port: 32002,
                container_port: 3000,
                protocol: temps_deployer::Protocol::Tcp,
                host_ip: Some("::".to_string()),
            },
            temps_deployer::PortMapping {
                host_port: 32001,
                container_port: 3000,
                protocol: temps_deployer::Protocol::Tcp,
                host_ip: Some("0.0.0.0".to_string()),
            },
        ];

        assert_eq!(published_tcp_host_port(3000, &mappings), Some(32001));
        assert_eq!(
            published_tcp_host_port(3000, &mappings.into_iter().rev().collect::<Vec<_>>()),
            Some(32001),
            "Docker port ordering must not change the route target"
        );
    }

    #[tokio::test]
    async fn missing_mapping_notification_failure_rolls_back_clear_for_retry() {
        let container = make_container_model(1);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([[container.clone()]])
                .append_exec_errors([DbErr::Custom("notification failed".into())])
                .into_connection(),
        );
        let deployer = Arc::new(MockDeployer::new(0, ContainerStatus::Running));
        let info = deployer.get_container_info("abc123").await.unwrap();
        let monitor = ContainerHealthMonitor::new(
            db.clone(),
            deployer,
            make_alarm_service(db.clone()),
            ContainerHealthConfig::default(),
        );
        assert!(monitor
            .persist_runtime_info(&container, &info)
            .await
            .is_err());
        drop(monitor);
        let sql = format!("{:?}", Arc::try_unwrap(db).unwrap().into_transaction_log());
        assert!(
            sql.contains("ROLLBACK"),
            "failed notification must roll back port update: {sql}"
        );
    }

    // ── Config tests ──────────────────────────────────────────────────

    #[test]
    fn test_container_health_config_default() {
        let config = ContainerHealthConfig::default();
        assert_eq!(config.poll_interval_secs, 30);
        assert_eq!(config.cpu_threshold_percent, 90.0);
        assert_eq!(config.memory_threshold_percent, 90.0);
        assert_eq!(config.consecutive_threshold_checks, 3);
    }

    #[test]
    fn test_container_state_clone() {
        let state = ContainerState {
            restart_count: 5,
            last_net_rx_bytes: 100,
            last_net_tx_bytes: 200,
        };
        let cloned = state.clone();
        assert_eq!(cloned.restart_count, 5);
        assert_eq!(cloned.last_net_rx_bytes, 100);
        assert_eq!(cloned.last_net_tx_bytes, 200);
    }

    // ── Restart detection tests ───────────────────────────────────────

    #[tokio::test]
    async fn test_check_restart_count_first_check_sets_baseline() {
        let deployer = Arc::new(MockDeployer::new(0, ContainerStatus::Running));
        let container = make_container_model(1);
        let deployment = make_deployment_model();

        // No DB calls for alarm on first check (just baseline)
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let db = Arc::new(db);
        let alarm_service = make_alarm_service(db.clone());

        let monitor = ContainerHealthMonitor::new(
            db,
            deployer.clone(),
            alarm_service,
            ContainerHealthConfig::default(),
        );

        let info = deployer.get_container_info("abc123").await.unwrap();
        monitor
            .check_restart_count(&container, &deployment, &info)
            .await;

        // Baseline should be recorded
        let states = monitor.container_states.read().await;
        assert_eq!(states.get(&1).unwrap().restart_count, 0);
    }

    #[tokio::test]
    async fn test_check_restart_count_detects_increase() {
        let deployer = Arc::new(MockDeployer::new(0, ContainerStatus::Running));
        let container = make_container_model(1);
        let deployment = make_deployment_model();

        // DB calls: cooldown check (count=0) + insert alarm
        let alarm_model = temps_entities::alarms::Model {
            id: 1,
            project_id: Some(1),
            environment_id: Some(1),
            deployment_id: Some(10),
            container_id: Some(1),
            service_id: None,
            alarm_type: "container_restart".to_string(),
            severity: "info".to_string(),
            status: "firing".to_string(),
            title: "Container restarted".to_string(),
            message: None,
            metadata: None,
            fired_at: chrono::Utc::now(),
            acknowledged_at: None,
            acknowledged_by: None,
            resolved_at: None,
            silenced_until: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[maplit::btreemap! {
                "num_items" => sea_orm::Value::BigInt(Some(0)),
            }]])
            .append_query_results(vec![vec![alarm_model]])
            .into_connection();
        let db = Arc::new(db);
        let alarm_service = make_alarm_service(db.clone());

        let monitor = ContainerHealthMonitor::new(
            db,
            deployer.clone(),
            alarm_service,
            ContainerHealthConfig::default(),
        );

        // First check: set baseline at 0
        let info = deployer.get_container_info("abc123").await.unwrap();
        monitor
            .check_restart_count(&container, &deployment, &info)
            .await;

        // Simulate restart: count goes to 2
        deployer.set_restart_count(2).await;
        let info = deployer.get_container_info("abc123").await.unwrap();
        monitor
            .check_restart_count(&container, &deployment, &info)
            .await;

        // State should be updated to 2
        let states = monitor.container_states.read().await;
        assert_eq!(states.get(&1).unwrap().restart_count, 2);
    }

    #[tokio::test]
    async fn test_check_restart_count_no_alarm_if_no_increase() {
        let deployer = Arc::new(MockDeployer::new(5, ContainerStatus::Running));
        let container = make_container_model(1);
        let deployment = make_deployment_model();

        // No DB calls needed (no alarm fired)
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let db = Arc::new(db);
        let alarm_service = make_alarm_service(db.clone());

        let monitor = ContainerHealthMonitor::new(
            db,
            deployer.clone(),
            alarm_service,
            ContainerHealthConfig::default(),
        );

        // First check: baseline at 5
        let info = deployer.get_container_info("abc123").await.unwrap();
        monitor
            .check_restart_count(&container, &deployment, &info)
            .await;

        // Second check: still 5, no alarm
        let info = deployer.get_container_info("abc123").await.unwrap();
        monitor
            .check_restart_count(&container, &deployment, &info)
            .await;

        let states = monitor.container_states.read().await;
        assert_eq!(states.get(&1).unwrap().restart_count, 5);
    }

    // ── Container status detection tests ──────────────────────────────

    #[tokio::test]
    async fn test_check_container_status_exited_fires_alarm() {
        let deployer = Arc::new(MockDeployer::new(0, ContainerStatus::Exited));
        let container = make_container_model(1);
        let deployment = make_deployment_model();

        let alarm_model = temps_entities::alarms::Model {
            id: 1,
            project_id: Some(1),
            environment_id: Some(1),
            deployment_id: Some(10),
            container_id: Some(1),
            service_id: None,
            alarm_type: "container_oom_killed".to_string(),
            severity: "critical".to_string(),
            status: "firing".to_string(),
            title: "Container exited".to_string(),
            message: None,
            metadata: None,
            fired_at: chrono::Utc::now(),
            acknowledged_at: None,
            acknowledged_by: None,
            resolved_at: None,
            silenced_until: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[maplit::btreemap! {
                "num_items" => sea_orm::Value::BigInt(Some(0)),
            }]])
            .append_query_results(vec![vec![alarm_model]])
            .into_connection();
        let db = Arc::new(db);
        let alarm_service = make_alarm_service(db.clone());

        let monitor = ContainerHealthMonitor::new(
            db,
            deployer.clone(),
            alarm_service,
            ContainerHealthConfig::default(),
        );

        let info = deployer.get_container_info("abc123").await.unwrap();
        // Should fire alarm for exited container
        monitor
            .check_container_status(&container, &deployment, &info)
            .await;
    }

    #[tokio::test]
    async fn test_check_container_status_exited_skips_alarm_when_paused() {
        let deployer = Arc::new(MockDeployer::new(0, ContainerStatus::Exited));
        let container = make_container_model(1);
        let deployment = deployments::Model {
            state: "paused".to_string(),
            ..make_deployment_model()
        };

        // Query order: `is_on_demand_sleeping` reads `environments` first (not
        // found here, so it reports "not sleeping"), then `is_deployment_paused`
        // reads `deployments` live and must see `state == "paused"` — this is
        // the live re-read added after PR #835 review found the deployment
        // model passed into `check_container_status` can be a stale snapshot
        // batch-loaded once per poll cycle. No alarm-related DB calls
        // (cooldown check / insert) should happen after that: if the paused
        // check were skipped, the monitor would try to query for them and
        // this MockDatabase (with no further results queued) would surface it.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<temps_entities::environments::Model>::new()])
            .append_query_results(vec![vec![deployment.clone()]])
            .into_connection();
        let db = Arc::new(db);
        let alarm_service = make_alarm_service(db.clone());

        let monitor = ContainerHealthMonitor::new(
            db,
            deployer.clone(),
            alarm_service,
            ContainerHealthConfig::default(),
        );

        let info = deployer.get_container_info("abc123").await.unwrap();
        monitor
            .check_container_status(&container, &deployment, &info)
            .await;
    }

    /// Regression test: `check_all_containers` batch-loads its deployments
    /// map once per poll cycle, so the `deployment` snapshot passed into
    /// `check_container_status` can be stale by the time a specific
    /// container is reached — e.g. a pause that commits mid-cycle, after the
    /// snapshot was taken. The alarm decision must be driven by a live read,
    /// not by `deployment.state` on the passed-in snapshot: here the
    /// snapshot still says "ready" (not paused) but the live DB row is
    /// "paused", and the alarm must still be skipped.
    #[tokio::test]
    async fn test_check_container_status_uses_live_state_not_stale_snapshot() {
        let deployer = Arc::new(MockDeployer::new(0, ContainerStatus::Exited));
        let container = make_container_model(1);
        let stale_snapshot = deployments::Model {
            state: "ready".to_string(),
            ..make_deployment_model()
        };
        let live_paused_row = deployments::Model {
            state: "paused".to_string(),
            ..make_deployment_model()
        };

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<temps_entities::environments::Model>::new()])
            .append_query_results(vec![vec![live_paused_row]])
            .into_connection();
        let db = Arc::new(db);
        let alarm_service = make_alarm_service(db.clone());

        let monitor = ContainerHealthMonitor::new(
            db,
            deployer.clone(),
            alarm_service,
            ContainerHealthConfig::default(),
        );

        let info = deployer.get_container_info("abc123").await.unwrap();
        // Passing the stale, still-"ready" snapshot: if the implementation
        // regressed to reading `stale_snapshot.state` instead of doing a
        // live lookup, it would try to fire an alarm here and hit
        // unmocked DB calls this MockDatabase has no results queued for.
        monitor
            .check_container_status(&container, &stale_snapshot, &info)
            .await;
    }

    #[tokio::test]
    async fn test_check_container_status_running_no_alarm() {
        let deployer = Arc::new(MockDeployer::new(0, ContainerStatus::Running));
        let container = make_container_model(1);
        let deployment = make_deployment_model();

        // No DB calls needed (no alarm)
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let db = Arc::new(db);
        let alarm_service = make_alarm_service(db.clone());

        let monitor = ContainerHealthMonitor::new(
            db,
            deployer.clone(),
            alarm_service,
            ContainerHealthConfig::default(),
        );

        let info = deployer.get_container_info("abc123").await.unwrap();
        // Should not fire alarm for running container
        monitor
            .check_container_status(&container, &deployment, &info)
            .await;
    }

    // ── Resource threshold tests ──────────────────────────────────────

    /// Build a monitor over a mock deployer, with no metrics store.
    fn make_monitor(deployer: Arc<MockDeployer>) -> ContainerHealthMonitor {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let alarm_service = make_alarm_service(db.clone());
        ContainerHealthMonitor::new(
            db,
            deployer,
            alarm_service,
            ContainerHealthConfig::default(),
        )
    }

    #[tokio::test]
    async fn test_uncapped_container_does_not_breach_at_one_core_of_many() {
        // Regression: with no CPU limit, one saturated core used to normalise to
        // 100% and trip the 90% threshold — on a host with 7 idle cores.
        let deployer = Arc::new(MockDeployer::new(0, ContainerStatus::Running));
        deployer.set_cpu(100.0, None, 8).await;
        let container = make_container_model(1);
        let deployment = make_deployment_model();

        let monitor = make_monitor(deployer.clone());
        monitor
            .check_resource_usage(&container, &deployment, deployer.as_ref())
            .await;

        let counters = monitor.resource_counters.read().await;
        assert!(
            counters.get(&(1, "high_cpu")).is_none(),
            "uncapped container using 1 of 8 cores must not count as a CPU breach"
        );
    }

    #[tokio::test]
    async fn test_uncapped_container_breaches_when_it_saturates_the_host() {
        // 7.8 of 8 cores == 97.5% of what it's allowed: a real breach.
        let deployer = Arc::new(MockDeployer::new(0, ContainerStatus::Running));
        deployer.set_cpu(780.0, None, 8).await;
        let container = make_container_model(1);
        let deployment = make_deployment_model();

        let monitor = make_monitor(deployer.clone());
        monitor
            .check_resource_usage(&container, &deployment, deployer.as_ref())
            .await;

        let counters = monitor.resource_counters.read().await;
        assert_eq!(*counters.get(&(1, "high_cpu")).unwrap(), 1);
    }

    #[tokio::test]
    async fn test_capped_container_breaches_against_its_own_limit() {
        // 0.48 cores against a 0.5-core cap == 96%, even though the 8-core host
        // is almost entirely idle.
        let deployer = Arc::new(MockDeployer::new(0, ContainerStatus::Running));
        deployer.set_cpu(48.0, Some(0.5), 8).await;
        let container = make_container_model(1);
        let deployment = make_deployment_model();

        let monitor = make_monitor(deployer.clone());
        monitor
            .check_resource_usage(&container, &deployment, deployer.as_ref())
            .await;

        let counters = monitor.resource_counters.read().await;
        assert_eq!(*counters.get(&(1, "high_cpu")).unwrap(), 1);
    }

    #[tokio::test]
    async fn test_resource_counter_increments_before_alarm() {
        let deployer = Arc::new(MockDeployer::new(0, ContainerStatus::Running));
        let container = make_container_model(1);
        let deployment = make_deployment_model();

        // No DB calls until we hit consecutive_threshold_checks
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let db = Arc::new(db);
        let alarm_service = make_alarm_service(db.clone());

        let config = ContainerHealthConfig {
            consecutive_threshold_checks: 3,
            ..ContainerHealthConfig::default()
        };

        let monitor = ContainerHealthMonitor::new(db, deployer.clone(), alarm_service, config);

        // Breach 1: should NOT fire alarm
        monitor
            .handle_resource_threshold(
                &container,
                &deployment,
                AlarmType::HighCpu,
                AlarmSeverity::Warning,
                "CPU high".to_string(),
                "CPU at 95%".to_string(),
                serde_json::json!({"cpu_percent": 95.0}),
            )
            .await;

        let counters = monitor.resource_counters.read().await;
        assert_eq!(*counters.get(&(1, "high_cpu")).unwrap(), 1);
        drop(counters);

        // Breach 2: still should NOT fire alarm
        monitor
            .handle_resource_threshold(
                &container,
                &deployment,
                AlarmType::HighCpu,
                AlarmSeverity::Warning,
                "CPU high".to_string(),
                "CPU at 95%".to_string(),
                serde_json::json!({"cpu_percent": 95.0}),
            )
            .await;

        let counters = monitor.resource_counters.read().await;
        assert_eq!(*counters.get(&(1, "high_cpu")).unwrap(), 2);
    }

    #[tokio::test]
    async fn test_resource_counter_resets_below_threshold() {
        let deployer = Arc::new(MockDeployer::new(0, ContainerStatus::Running));
        let _container = make_container_model(1);

        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let db = Arc::new(db);
        let alarm_service = make_alarm_service(db.clone());

        let monitor = ContainerHealthMonitor::new(
            db,
            deployer,
            alarm_service,
            ContainerHealthConfig::default(),
        );

        // Manually set a counter
        {
            let mut counters = monitor.resource_counters.write().await;
            counters.insert((1, "high_cpu"), 2);
        }

        // Reset it
        monitor.reset_resource_counter(1, "high_cpu").await;

        let counters = monitor.resource_counters.read().await;
        assert!(counters.get(&(1, "high_cpu")).is_none());
    }

    // ── State pruning tests ───────────────────────────────────────────

    #[tokio::test]
    async fn test_state_pruning_removes_stale_containers() {
        let deployer = Arc::new(MockDeployer::new(0, ContainerStatus::Running));

        // DB returns only container id=1, but we have cached state for id=1 and id=2
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![make_container_model(1)]])
            // For check_container: deployment lookup
            .append_query_results(vec![vec![make_deployment_model()]])
            .into_connection();
        let db = Arc::new(db);
        let alarm_service = make_alarm_service(db.clone());

        let monitor = ContainerHealthMonitor::new(
            db,
            deployer,
            alarm_service,
            ContainerHealthConfig::default(),
        );

        // Pre-populate state for containers 1 and 2
        {
            let mut states = monitor.container_states.write().await;
            states.insert(
                1,
                ContainerState {
                    restart_count: 0,
                    last_net_rx_bytes: 0,
                    last_net_tx_bytes: 0,
                },
            );
            states.insert(
                2,
                ContainerState {
                    restart_count: 5,
                    last_net_rx_bytes: 0,
                    last_net_tx_bytes: 0,
                },
            );
        }
        {
            let mut counters = monitor.resource_counters.write().await;
            counters.insert((2, "high_cpu"), 2);
        }

        // After check_all_containers, container 2 should be pruned
        // Container 1 remains but its resource counters may be reset by the health check
        let _ = monitor.check_all_containers().await;

        let states = monitor.container_states.read().await;
        assert!(states.contains_key(&1), "Container 1 state should survive");
        assert!(
            !states.contains_key(&2),
            "Container 2 state should be pruned"
        );

        let counters = monitor.resource_counters.read().await;
        assert!(
            !counters.contains_key(&(2, "high_cpu")),
            "Counter for container 2 should be pruned"
        );
    }

    // ── Exit alarms against a real database ───────────────────────────
    //
    // Regression coverage for container-exit alarms that kept firing every
    // poll cycle for a long-superseded deployment's container (exited days
    // earlier, while a much newer deployment served traffic).
    // The monitor treated every non-deleted `deployment_containers` row as
    // "expected to be running", so any row whose teardown was missed paged
    // forever.

    struct ExitFixture {
        _database: temps_database::test_utils::TestDatabase,
        db: Arc<sea_orm::DatabaseConnection>,
        environment_id: i32,
    }

    async fn exit_fixture() -> Option<ExitFixture> {
        use sea_orm::ActiveModelTrait;
        use temps_entities::{
            environments, preset::Preset, projects, upstream_config::UpstreamList,
        };

        let database = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(database) => database,
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                eprintln!("Skipping container exit alarm test: Docker runtime unavailable");
                return None;
            }
            Err(error) => panic!("Could not create isolated test database: {error}"),
        };
        let db = database.connection_arc();
        let now = chrono::Utc::now();
        let project = projects::ActiveModel {
            name: Set("app".to_string()),
            slug: Set("app".to_string()),
            repo_owner: Set("owner".to_string()),
            repo_name: Set("app".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(Preset::NextJs),
            directory: Set("/".to_string()),
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
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            host: Set("app.example.com".to_string()),
            subdomain: Set("app.example.com".to_string()),
            upstreams: Set(UpstreamList::default()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        Some(ExitFixture {
            _database: database,
            db,
            environment_id: environment.id,
        })
    }

    impl ExitFixture {
        async fn deployment(&self, slug: &str, state: &str) -> deployments::Model {
            use sea_orm::ActiveModelTrait;
            let environment = temps_entities::environments::Entity::find_by_id(self.environment_id)
                .one(self.db.as_ref())
                .await
                .unwrap()
                .unwrap();
            deployments::ActiveModel {
                project_id: Set(environment.project_id),
                environment_id: Set(self.environment_id),
                slug: Set(slug.to_string()),
                state: Set(state.to_string()),
                metadata: Set(Some(deployments::DeploymentMetadata::default())),
                created_at: Set(chrono::Utc::now()),
                updated_at: Set(chrono::Utc::now()),
                ..Default::default()
            }
            .insert(self.db.as_ref())
            .await
            .unwrap()
        }

        async fn serve(&self, deployment: &deployments::Model) {
            use sea_orm::ActiveModelTrait;
            temps_entities::environments::ActiveModel {
                id: Set(self.environment_id),
                current_deployment_id: Set(Some(deployment.id)),
                ..Default::default()
            }
            .update(self.db.as_ref())
            .await
            .unwrap();
        }

        async fn container(
            &self,
            deployment: &deployments::Model,
            status: &str,
        ) -> deployment_containers::Model {
            use sea_orm::ActiveModelTrait;
            deployment_containers::ActiveModel {
                deployment_id: Set(deployment.id),
                container_id: Set(format!("{}-container", deployment.slug)),
                container_name: Set(deployment.slug.clone()),
                container_port: Set(3000),
                status: Set(Some(status.to_string())),
                created_at: Set(chrono::Utc::now()),
                deployed_at: Set(chrono::Utc::now()),
                ..Default::default()
            }
            .insert(self.db.as_ref())
            .await
            .unwrap()
        }

        /// Run one monitor poll with every container reported as exited(1).
        async fn poll_with_exited_containers(&self) {
            self.poll_with_status(ContainerStatus::Exited).await;
        }

        /// Run one monitor poll with every container in `status`.
        async fn poll_with_status(&self, status: ContainerStatus) {
            self.poll(status, None).await;
        }

        /// Run one monitor poll with every container in `status`, reporting
        /// Docker's `started_at` as `started_at`.
        async fn poll(
            &self,
            status: ContainerStatus,
            started_at: Option<chrono::DateTime<chrono::Utc>>,
        ) {
            let deployer = Arc::new(MockDeployer::new(0, status));
            {
                deployer.info.lock().await.started_at = started_at;
                let mut info = deployer.info.lock().await;
                info.exit_code = Some(1);
                info.exit_reason = Some("Exit code 1".to_string());
            }
            let monitor = ContainerHealthMonitor::new(
                self.db.clone(),
                deployer,
                make_alarm_service(self.db.clone()),
                ContainerHealthConfig::default(),
            );
            monitor.check_all_containers().await.unwrap();
        }

        async fn alarms(&self) -> Vec<temps_entities::alarms::Model> {
            temps_entities::alarms::Entity::find()
                .all(self.db.as_ref())
                .await
                .unwrap()
        }
    }

    #[tokio::test]
    async fn exited_container_of_superseded_deployment_does_not_alarm() {
        let Some(fixture) = exit_fixture().await else {
            return;
        };
        // app-1 was superseded long ago but its row was never retired;
        // app-8 is what the environment serves now.
        let superseded = fixture.deployment("app-1", "stopped").await;
        let current = fixture.deployment("app-8", "completed").await;
        fixture.serve(&current).await;
        let orphan = fixture.container(&superseded, "exited").await;

        fixture.poll_with_exited_containers().await;
        fixture.poll_with_exited_containers().await;

        let alarms = fixture.alarms().await;
        assert!(
            alarms.is_empty(),
            "a superseded deployment's exited container must not page: {alarms:?}"
        );
        // Exit metadata is still recorded so the UI can show why it is down.
        let row = deployment_containers::Entity::find_by_id(orphan.id)
            .one(fixture.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.exit_code, Some(1));
    }

    #[tokio::test]
    async fn exited_container_of_current_deployment_still_alarms() {
        let Some(fixture) = exit_fixture().await else {
            return;
        };
        let current = fixture.deployment("app-8", "completed").await;
        fixture.serve(&current).await;
        let container = fixture.container(&current, "running").await;

        fixture.poll_with_exited_containers().await;

        let alarms = fixture.alarms().await;
        assert_eq!(alarms.len(), 1, "the serving container crashed: {alarms:?}");
        assert_eq!(alarms[0].alarm_type, "container_crash");
        assert_eq!(alarms[0].container_id, Some(container.id));
        assert_eq!(alarms[0].deployment_id, Some(current.id));
    }

    #[tokio::test]
    async fn user_stopped_container_does_not_alarm_and_keeps_its_status() {
        let Some(fixture) = exit_fixture().await else {
            return;
        };
        let current = fixture.deployment("app-8", "completed").await;
        fixture.serve(&current).await;
        let stopped = fixture.container(&current, "stopped").await;
        let retained = fixture
            .container(
                &fixture.deployment("app-9", "failed").await,
                "retained:stopped-after-failed-readiness",
            )
            .await;

        fixture.poll_with_exited_containers().await;
        fixture.poll_with_exited_containers().await;

        assert!(fixture.alarms().await.is_empty());
        for (id, status) in [
            (stopped.id, "stopped"),
            (retained.id, "retained:stopped-after-failed-readiness"),
        ] {
            let row = deployment_containers::Entity::find_by_id(id)
                .one(fixture.db.as_ref())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                row.status.as_deref(),
                Some(status),
                "the poll must not overwrite an intentional stop with Docker's state"
            );
            assert_eq!(row.exit_code, Some(1));
        }
    }

    #[tokio::test]
    async fn restarted_user_stopped_container_alarms_on_its_next_crash() {
        let Some(fixture) = exit_fixture().await else {
            return;
        };
        let current = fixture.deployment("app-8", "completed").await;
        fixture.serve(&current).await;
        let container = fixture.container(&current, "stopped").await;

        // Started again (by hand or by Docker's restart policy).
        fixture.poll_with_status(ContainerStatus::Running).await;
        let row = deployment_containers::Entity::find_by_id(container.id)
            .one(fixture.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status.as_deref(), Some("running"));

        fixture.poll_with_exited_containers().await;
        let alarms = fixture.alarms().await;
        assert_eq!(
            alarms.len(),
            1,
            "a crash after the restart is a real crash: {alarms:?}"
        );
    }

    #[tokio::test]
    async fn exited_container_of_environment_without_current_deployment_does_not_alarm() {
        let Some(fixture) = exit_fixture().await else {
            return;
        };
        // No `serve`: the environment has no current deployment at all.
        let deployment = fixture.deployment("app-1", "completed").await;
        fixture.container(&deployment, "running").await;

        fixture.poll_with_exited_containers().await;

        assert!(fixture.alarms().await.is_empty());
    }

    #[tokio::test]
    async fn unreadable_environment_does_not_silence_an_exit() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([DbErr::Custom("connection reset".to_string())])
                .into_connection(),
        );
        let monitor = ContainerHealthMonitor::new(
            db.clone(),
            Arc::new(MockDeployer::new(0, ContainerStatus::Exited)),
            make_alarm_service(db),
            ContainerHealthConfig::default(),
        );

        let reason = monitor
            .exit_not_alarmable_reason(&make_deployment_model())
            .await;

        assert_eq!(reason, None, "an unreadable environment must still alarm");
    }

    #[tokio::test]
    async fn crash_after_unobserved_restart_of_user_stopped_container_alarms() {
        use sea_orm::ActiveModelTrait;
        let Some(fixture) = exit_fixture().await else {
            return;
        };
        let current = fixture.deployment("app-8", "completed").await;
        fixture.serve(&current).await;
        let container = fixture.container(&current, "stopped").await;
        let stopped_run_started = chrono::Utc::now() - chrono::Duration::hours(1);
        deployment_containers::ActiveModel {
            id: Set(container.id),
            started_at: Set(Some(stopped_run_started)),
            ..Default::default()
        }
        .update(fixture.db.as_ref())
        .await
        .unwrap();

        // Still the run the user stopped: no alarm.
        fixture
            .poll(ContainerStatus::Exited, Some(stopped_run_started))
            .await;
        assert!(fixture.alarms().await.is_empty());

        // Started again and crashed between two polls: never seen running,
        // but Docker's start time moved on.
        fixture
            .poll(
                ContainerStatus::Exited,
                Some(stopped_run_started + chrono::Duration::minutes(30)),
            )
            .await;

        let alarms = fixture.alarms().await;
        assert_eq!(alarms.len(), 1, "the new run crashed: {alarms:?}");
        let row = deployment_containers::Entity::find_by_id(container.id)
            .one(fixture.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status.as_deref(), Some("exited"));
    }

    #[tokio::test]
    async fn restart_then_user_stop_between_polls_does_not_alarm() {
        use sea_orm::ActiveModelTrait;
        let Some(fixture) = exit_fixture().await else {
            return;
        };
        let current = fixture.deployment("app-8", "completed").await;
        fixture.serve(&current).await;
        let container = fixture.container(&current, "stopped").await;
        let last_seen_start = chrono::Utc::now() - chrono::Duration::hours(1);
        let stopped_at = chrono::Utc::now() - chrono::Duration::minutes(5);
        deployment_containers::ActiveModel {
            id: Set(container.id),
            started_at: Set(Some(last_seen_start)),
            finished_at: Set(Some(stopped_at)),
            ..Default::default()
        }
        .update(fixture.db.as_ref())
        .await
        .unwrap();

        // Restarted after the last poll, then stopped by the user before the
        // next one: Docker's start time is newer than the recorded start but
        // older than the stop.
        fixture
            .poll(
                ContainerStatus::Exited,
                Some(stopped_at - chrono::Duration::minutes(1)),
            )
            .await;

        assert!(fixture.alarms().await.is_empty());
        let row = deployment_containers::Entity::find_by_id(container.id)
            .one(fixture.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status.as_deref(), Some("stopped"));
    }

    #[tokio::test]
    async fn crash_after_restart_following_recorded_stop_alarms() {
        use sea_orm::ActiveModelTrait;
        let Some(fixture) = exit_fixture().await else {
            return;
        };
        let current = fixture.deployment("app-8", "completed").await;
        fixture.serve(&current).await;
        let container = fixture.container(&current, "stopped").await;
        let stopped_at = chrono::Utc::now() - chrono::Duration::minutes(5);
        deployment_containers::ActiveModel {
            id: Set(container.id),
            started_at: Set(Some(stopped_at - chrono::Duration::hours(1))),
            finished_at: Set(Some(stopped_at)),
            ..Default::default()
        }
        .update(fixture.db.as_ref())
        .await
        .unwrap();

        fixture
            .poll(
                ContainerStatus::Exited,
                Some(stopped_at + chrono::Duration::minutes(1)),
            )
            .await;

        assert_eq!(fixture.alarms().await.len(), 1);
    }
}
