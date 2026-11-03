// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One replica's deployment, phase by phase.
//!
//! [`DeployImageJob::deploy_single_replica`] reads top to bottom as the order
//! these run in: prepare the request, create and track the container, verify
//! what the host actually did, wait for it to run, stream its logs, wait for
//! the application to be ready. Everything after the create runs on a tracked
//! container, so any failure from there on is cleaned up with the rest of the
//! deployment.

use super::*;
use temps_core::repo_config::HealthCheckConfig;
use temps_deployer::DeployResult;

use crate::services::NodeAssignment;

/// What the create call needs, decided before anything exists on a host.
struct PreparedReplica {
    request: DeployRequest,
    container_port: Option<u16>,
    health: HealthCheckConfig,
    /// Node name recorded in the ADR 045 audit and in operator messages.
    node_name: String,
}

/// A created, tracked container and what every later phase needs to reach it.
struct CreatedReplica<'a> {
    context: &'a WorkflowContext,
    deployer: &'a Arc<dyn ContainerDeployer>,
    assignment: &'a NodeAssignment,
    result: DeployResult,
    container_port: Option<u16>,
}

/// The readiness budget, shared by the wait-for-running and health phases.
struct StartupClock {
    started: std::time::Instant,
    limit: Duration,
}

impl StartupClock {
    fn start(limit: Duration) -> Self {
        Self {
            started: std::time::Instant::now(),
            limit,
        }
    }

    fn elapsed_secs(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    fn limit_secs(&self) -> u64 {
        self.limit.as_secs()
    }

    fn expired(&self) -> bool {
        self.started.elapsed() > self.limit
    }

    fn remaining(&self) -> Duration {
        self.limit.saturating_sub(self.started.elapsed())
    }

    /// Sleep for `delay`, or for what is left of the budget when that is less.
    async fn pause(&self, delay: Duration) {
        tokio::time::sleep(delay.min(self.remaining())).await;
    }
}

/// The node name and ID a replica is told it runs on. The control plane is
/// node 0.
fn replica_node_identity(assignment: &NodeAssignment) -> (String, String) {
    match assignment {
        NodeAssignment::Remote {
            node_name, node_id, ..
        } => (node_name.clone(), node_id.to_string()),
        NodeAssignment::Local => ("control-plane".to_string(), "0".to_string()),
    }
}

/// Docker labels for the log aggregator to discover this container.
/// The collector inspects these labels to enrich log lines with project/env/service context.
/// `sh.temps.managed` marks this as a Temps-managed container for reconciliation.
fn replica_labels(context: &WorkflowContext, service_name: &str) -> HashMap<String, String> {
    let mut labels = HashMap::new();
    labels.insert("sh.temps.managed".to_string(), "true".to_string());
    labels.insert(
        "sh.temps.project_id".to_string(),
        context.project_id.to_string(),
    );
    labels.insert(
        "sh.temps.environment".to_string(),
        context.environment_id.to_string(),
    );
    labels.insert("sh.temps.service".to_string(), service_name.to_string());
    labels.insert(
        "sh.temps.deploy_id".to_string(),
        context.deployment_id.to_string(),
    );
    labels
}

/// Where the control plane reaches a replica: a remote node through its
/// private IP and host port, since it cannot resolve a Docker network name
/// there, and a local one through the standard container URL resolution.
fn replica_url(
    assignment: &NodeAssignment,
    container_name: &str,
    container_port: u16,
    host_port: u16,
    path: Option<&str>,
) -> String {
    if let Some(private_addr) = assignment.private_address() {
        format!(
            "http://{}:{}{}",
            private_addr,
            host_port,
            path.unwrap_or("")
        )
    } else {
        temps_core::DeploymentMode::build_container_url(
            container_name,
            container_port,
            host_port,
            path,
        )
    }
}

/// Stream container logs into the deployment log for a bounded time and line
/// count. Runs detached, so its write failures are ignored: a lost log line
/// never fails a deployment from here.
async fn stream_replica_startup_logs(
    deployer: Arc<dyn ContainerDeployer>,
    container_id: String,
    context: WorkflowContext,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
) {
    // Helper macro to write logs in the background task
    macro_rules! write_log {
        ($level:expr, $msg:expr) => {
            if let (Some(ref log_id), Some(ref log_service)) = (&log_id, &log_service) {
                let _ = log_service
                    .append_structured_log(log_id, $level, $msg.clone())
                    .await;
            }
            let _ = context.log(&$msg).await;
        };
    }

    write_log!(
        LogLevel::Info,
        "📋 Streaming container logs for 15s...".to_string()
    );

    // Ask the selected deployer for logs. For worker assignments this
    // streams through the worker agent instead of opening the control
    // plane's local Docker socket.
    let mut log_stream = match deployer.stream_container_logs(&container_id).await {
        Ok(stream) => stream,
        Err(e) => {
            write_log!(
                LogLevel::Warning,
                format!("⚠️  Cannot stream logs from the container's node: {}", e)
            );
            return;
        }
    };
    let mut line_count = 0;
    let max_lines = 100;
    let timeout = tokio::time::sleep(std::time::Duration::from_secs(15));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            _ = &mut timeout => {
                write_log!(LogLevel::Info,
                    format!("📋 Log streaming complete ({} lines captured)", line_count));
                break;
            }
            log_result = log_stream.next() => {
                match log_result {
                    Some(log_output) => {
                        let clean_msg = log_output.trim().to_string();
                        if !clean_msg.is_empty() {
                            write_log!(LogLevel::Info,
                                format!("🐳 {}", clean_msg));
                            line_count += 1;

                            if line_count >= max_lines {
                                write_log!(LogLevel::Info,
                                    format!("📋 Log limit reached ({} lines), stopping stream...", max_lines));
                                break;
                            }
                        }
                    }
                    None => {
                        write_log!(LogLevel::Info,
                            format!("📋 Log streaming complete ({} lines captured)", line_count));
                        break;
                    }
                }
            }
        }
    }
}

impl DeployImageJob {
    /// Deploy a single replica of the container
    pub(super) async fn deploy_single_replica(
        &self,
        // Tag resolved for the target node's architecture by the caller — on a
        // multi-arch build this is the per-node tag, not the primary one.
        image_tag: &str,
        context: &WorkflowContext,
        replica_index: u32,
        health_check_override: Option<&str>,
        deployer: &Arc<dyn ContainerDeployer>,
        target: ReplicaTarget<'_>,
    ) -> Result<(String, u16, Option<u16>), WorkflowError> {
        let ReplicaTarget {
            assignment,
            docker_socket_required,
        } = target;

        let PreparedReplica {
            request,
            container_port,
            health,
            node_name,
        } = self
            .prepare_replica(image_tag, context, replica_index, assignment)
            .await?;

        let replica = CreatedReplica {
            context,
            deployer,
            assignment,
            result: self.create_tracked_container(request, deployer).await?,
            container_port,
        };

        self.verify_created_replica(&replica, &node_name, docker_socket_required, image_tag)
            .await?;

        // Wait for deployment to be ready (with timeout)
        self.log(context, "Waiting for container to start...".to_string())
            .await?;
        let clock = StartupClock::start(std::time::Duration::from_secs(
            self.config.health_check_timeout_secs,
        ));

        self.wait_for_replica_running(&replica, &clock).await?;
        self.spawn_replica_log_stream(&replica);
        self.wait_for_replica_ready(&replica, &health, health_check_override, &clock)
            .await?;

        let endpoint_url = if container_port.is_none() {
            "background worker (healthy)".to_string()
        } else {
            replica_url(
                assignment,
                &replica.result.container_name,
                replica.result.container_port,
                replica.result.host_port,
                None,
            )
        };
        self.log(
            context,
            format!("✅ Replica {} ready at {}", replica_index + 1, endpoint_url),
        )
        .await?;

        // Return container ID, host port, and container port
        Ok((
            replica.result.container_id,
            replica.result.host_port,
            container_port,
        ))
    }

    /// Prepare phase: decide everything the create call needs. Nothing exists
    /// on any host yet, so a failure here has nothing to clean up.
    async fn prepare_replica(
        &self,
        image_tag: &str,
        context: &WorkflowContext,
        replica_index: u32,
        assignment: &NodeAssignment,
    ) -> Result<PreparedReplica, WorkflowError> {
        // Prepare deployment request using temps-deployer types
        self.log(context, "Deploying container image...".to_string())
            .await?;

        let log_path = std::env::temp_dir().join(format!("deploy_{}.log", self.job_id));
        let health = self.resolve_replica_health(context)?;
        let container_port = self
            .resolve_replica_container_port(image_tag, context, assignment)
            .await?;
        let port_mappings = self
            .replica_port_mappings(context, assignment, container_port)
            .await?;
        let resource_limits = self.replica_resource_limits();
        let (node_name, node_id) = replica_node_identity(assignment);
        let environment_vars = self
            .replica_environment(
                context,
                assignment,
                (&node_name, node_id),
                replica_index,
                container_port,
            )
            .await?;

        let request = DeployRequest {
            image_name: image_tag.to_string(),
            container_name: self.replica_container_name(replica_index),
            environment_vars,
            secrets: self.config.secrets.clone(),
            port_mappings,
            network_name: None,
            extra_networks: Vec::new(),
            resource_limits,
            restart_policy: RestartPolicy::Always,
            log_path,
            command: self.config.command.clone(),
            entrypoint: self.config.entrypoint.clone(),
            log_config: self.log_config.clone(),
            labels: replica_labels(context, &self.config.service_name),
            // ADR 045: the slug, never an instruction. The executing host
            // answers from its own environment whether this project gets
            // `/var/run/docker.sock` — but it may only answer "yes" when the
            // control plane also declared the project, which is what the
            // second field carries. Both halves must agree.
            project_slug: self.config.project_slug.clone(),
            control_plane_grants_socket: self.config.control_plane_grants_socket,
        };

        Ok(PreparedReplica {
            request,
            container_port,
            health,
            node_name,
        })
    }

    /// The health probe to run: the repository's `.temps.yaml` when the build
    /// recorded one, then the deployment config, then the default.
    fn resolve_replica_health(
        &self,
        context: &WorkflowContext,
    ) -> Result<HealthCheckConfig, WorkflowError> {
        let repository_health =
            context.get_output::<HealthCheckConfig>(&self.build_job_id, "health")?;
        let health = repository_health
            .or_else(|| self.config.health.clone())
            .unwrap_or_default();
        health.validate().map_err(|reason| {
            WorkflowError::JobValidationFailed(format!(
                "Invalid health probe for deployment {}: {reason}",
                context.deployment_id
            ))
        })?;
        Ok(health)
    }

    /// Determine the actual container port to expose
    /// Priority: explicit environment/project override > Image EXPOSE directive > default
    async fn resolve_replica_container_port(
        &self,
        image_tag: &str,
        context: &WorkflowContext,
        assignment: &NodeAssignment,
    ) -> Result<Option<u16>, WorkflowError> {
        if self.config.configured_port.is_some() || assignment.is_local() {
            return Ok(self.resolve_container_port(image_tag, context).await);
        }
        let Some(node_id) = assignment.node_id() else {
            return Ok(self.resolve_container_port(image_tag, context).await);
        };
        let remote = self.remote_deployer_for_node_id(node_id).await?;
        let image = remote.inspect_image(image_tag).await.map_err(|error| {
            WorkflowError::JobExecutionFailed(format!(
                "Cannot determine declared ports for image {image_tag} on node {node_id}: {error}"
            ))
        })?;
        Ok(match image.config {
            Some(config) => config.exposed_ports.into_iter().min(),
            None => Some(self.config.port as u16), // Compatibility with older agents.
        })
    }

    /// Publish the container port. For local deployments, allocate a port on
    /// this host. For remote deployments, set host_port=0 so Docker on the
    /// agent picks an available port, bound to the node's private address.
    async fn replica_port_mappings(
        &self,
        context: &WorkflowContext,
        assignment: &NodeAssignment,
        container_port: Option<u16>,
    ) -> Result<Vec<PortMapping>, WorkflowError> {
        let host_port = if assignment.is_local() && container_port.is_some() {
            Self::find_available_port()?
        } else {
            0
        };
        self.log(
            context,
            format!(
                "🔌 {} host port: {} → container port: {}",
                if assignment.is_local() {
                    "Allocated"
                } else {
                    "Requesting dynamic"
                },
                if host_port == 0 {
                    "auto".to_string()
                } else {
                    host_port.to_string()
                },
                container_port
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "none (background worker)".to_string())
            ),
        )
        .await?;

        let host_ip = assignment
            .private_address()
            .map(private_remote_bind_address)
            .transpose()?;
        Ok(container_port
            .map(|container_port| PortMapping {
                host_port,
                container_port,
                protocol: Protocol::Tcp,
                host_ip,
            })
            .into_iter()
            .collect())
    }

    /// Convert k8s-style strings ("1000m", "512Mi", "2", "1Gi") into the
    /// numeric ResourceLimits the deployer feeds to bollard. The previous
    /// implementation called `parse::<f64>()` directly, which silently
    /// returned None for any value containing a unit suffix — so even the
    /// builder defaults ("1000m" / "512Mi") never made it to the container.
    fn replica_resource_limits(&self) -> ResourceLimits {
        ResourceLimits {
            cpu_limit: self
                .config
                .resources
                .cpu_limit
                .as_ref()
                .and_then(|s| parse_cpu_cores(s)),
            memory_limit_mb: self
                .config
                .resources
                .memory_limit
                .as_ref()
                .and_then(|s| parse_memory_mb(s)),
            disk_limit_mb: None,
        }
    }

    /// Use remote environment variables for remote deployments (linked-service
    /// container names rewritten to their internal `*.temps.local` DNS names),
    /// fall back to local env vars.
    async fn replica_environment(
        &self,
        context: &WorkflowContext,
        assignment: &NodeAssignment,
        (node_name, node_id): (&str, String),
        replica_index: u32,
        container_port: Option<u16>,
    ) -> Result<HashMap<String, String>, WorkflowError> {
        let mut environment_vars = if !assignment.is_local() {
            // Refuse to start a container that has been handed a connection
            // string which cannot possibly connect. Managed service ports
            // bind to 127.0.0.1 on their own host, so when the planner could
            // not produce a resolvable name there is no working address at
            // all — and a container that boots and then fails to reach its
            // database forever is the exact silent failure this guards.
            if let Some(error) =
                cross_node_unreachable_error(node_name, &self.config.cross_node_service_blockers)
            {
                tracing::error!("{}", error);
                self.log(context, format!("❌ {}", error)).await?;
                return Err(error);
            }

            if let Some(ref remote_vars) = self.config.remote_environment_variables {
                tracing::info!(
                    "Using REMOTE environment variables for non-local assignment (has {} remote vars)",
                    remote_vars.len()
                );
                remote_vars.clone()
            } else {
                tracing::warn!(
                    "Non-local assignment but no remote_environment_variables available, using local vars"
                );
                self.config.environment_variables.clone()
            }
        } else {
            tracing::info!("Using LOCAL environment variables for local assignment");
            self.config.environment_variables.clone()
        };

        // Inject this replica's node identity so the workload can report which
        // node (and replica) is serving a given request — useful for debugging
        // multi-node scheduling and load distribution. Set on every node, local
        // or remote.
        environment_vars.insert("TEMPS_NODE_NAME".to_string(), node_name.to_string());
        environment_vars.insert("TEMPS_NODE_ID".to_string(), node_id);
        environment_vars.insert("TEMPS_REPLICA".to_string(), (replica_index + 1).to_string());

        if let Some(container_port) = container_port {
            if let Some(previous) = align_port_env_with_container_port(
                &mut environment_vars,
                self.config.configured_port,
                container_port,
            ) {
                self.log(
                    context,
                    format!(
                        "Setting PORT={} to match the port detected from the image (was {})",
                        container_port, previous
                    ),
                )
                .await?;
            }
        }

        tracing::info!(
            "Deploying container with {} env vars (Postgres host configured: {}, URL configured: {})",
            environment_vars.len(),
            environment_vars.contains_key("POSTGRES_HOST"),
            environment_vars.contains_key("POSTGRES_URL")
        );

        Ok(environment_vars)
    }

    /// Create unique container name for each replica
    fn replica_container_name(&self, replica_index: u32) -> String {
        if self.config.replicas > 1 {
            format!("{}-{}", self.config.service_name, replica_index + 1)
        } else {
            self.config.service_name.clone()
        }
    }

    /// Create phase: the one place a replica's container comes into existence.
    /// Nothing fallible may run between the create and `track_container`.
    async fn create_tracked_container(
        &self,
        request: DeployRequest,
        deployer: &Arc<dyn ContainerDeployer>,
    ) -> Result<DeployResult, WorkflowError> {
        let deploy_result = deployer.deploy_container(request).await.map_err(|e| {
            WorkflowError::JobExecutionFailed(format!("Failed to deploy container: {}", e))
        })?;

        // Store both the ID and its owning deployer before status checks,
        // startup log streaming, health checks, or ANY other fallible work —
        // including the ADR-045 audit and logging below. A container that
        // exists but is not tracked cannot be cleaned up, and the socket-
        // mounted case is the worst one to leak: a transient log-write error
        // would otherwise strand a running, root-equivalent container.
        self.track_container(deploy_result.container_id.clone(), deployer.clone());

        Ok(deploy_result)
    }

    /// Verify phase: compare what the host says it did with what this
    /// placement requires, then make the container a retention candidate.
    async fn verify_created_replica(
        &self,
        replica: &CreatedReplica<'_>,
        node_name: &str,
        docker_socket_required: bool,
        image_tag: &str,
    ) -> Result<(), WorkflowError> {
        self.enforce_replica_docker_socket(replica, node_name, docker_socket_required)
            .await?;
        self.confirm_private_replica_binding(replica).await?;
        {
            let mut candidates =
                lock_deployment_state(&self.failed_candidates, "failed_candidates");
            candidates.push(FailedContainerCandidate {
                container_id: replica.result.container_id.clone(),
                container_name: replica.result.container_name.clone(),
                container_port: replica.container_port,
                host_port: replica.result.host_port,
                image_name: image_tag.to_string(),
                node_id: replica.assignment.node_id(),
            });
        }

        self.log(
            replica.context,
            format!("Deployment created: {}", replica.result.container_id),
        )
        .await
    }

    async fn enforce_replica_docker_socket(
        &self,
        replica: &CreatedReplica<'_>,
        node_name: &str,
        docker_socket_required: bool,
    ) -> Result<(), WorkflowError> {
        let context = replica.context;
        // ADR 045: the executing host reports back whether it actually mounted
        // `/var/run/docker.sock`. Audited here rather than inferred from the
        // control plane's view, because only that host knows its own grant —
        // and this record is the only durable evidence that a given container
        // was root-equivalent on a given machine.
        if replica.result.docker_socket_mounted {
            self.audit_docker_socket_mount(context, node_name).await;
            self.persist_docker_socket_mounted(context).await;
            self.log(
                context,
                format!(
                    "⚠️  Host Docker socket mounted into this container on '{}' — this \
                     project is granted host Docker access there and is root-equivalent on \
                     that machine (ADR 045)",
                    node_name
                ),
            )
            .await?;
        }

        // ADR 045: the scheduler placed this replica here *because* the
        // project requires the socket, and the host started it without one.
        // Either that host's own `TEMPS_DOCKER_SOCKET_PROJECTS` disagrees with
        // the control plane's declaration (a half-applied config change, or an
        // agent that was never restarted) or the gate was bypassed. Accepting
        // it would leave the workload running with none of the access it
        // exists for, reported as healthy.
        //
        // Checked after `track_container` so the container this rejects is
        // torn down with the rest of the failed deployment rather than left
        // running untracked.
        if docker_socket_required && !replica.result.docker_socket_mounted {
            let error = WorkflowError::DockerSocketNotMounted {
                node: node_name.to_string(),
                project_slug: self
                    .config
                    .project_slug
                    .clone()
                    .unwrap_or_else(|| self.config.service_name.clone()),
                env: temps_core::docker_socket_grant::DOCKER_SOCKET_PROJECTS_ENV,
            };
            tracing::error!(
                node = %node_name,
                container_id = %replica.result.container_id,
                project_slug = self.config.project_slug.as_deref().unwrap_or("<unknown>"),
                "Gated deployment started without the host Docker socket"
            );
            // The deploy log is the only place the user sees this.
            self.log(context, format!("❌ {}", error)).await?;
            return Err(error);
        }
        Ok(())
    }

    /// A rolling-upgrade cluster may still have an older agent that ignores
    /// PortMapping.host_ip. Inspect what Docker actually published before
    /// this container becomes eligible for retention; fail closed if the
    /// worker cannot prove a private-only bind.
    async fn confirm_private_replica_binding(
        &self,
        replica: &CreatedReplica<'_>,
    ) -> Result<(), WorkflowError> {
        let Some(expected_host_ip) = replica
            .assignment
            .private_address()
            .filter(|_| replica.container_port.is_some())
        else {
            return Ok(());
        };
        let private_host_ip = private_remote_bind_address(expected_host_ip)?;
        let binding_is_private = replica
            .deployer
            .get_container_info(&replica.result.container_id)
            .await
            .map(|info| {
                confirms_private_port_binding(
                    &info.ports,
                    replica.result.container_port,
                    replica.result.host_port,
                    &private_host_ip,
                )
            })
            .unwrap_or(false);
        if !binding_is_private {
            self.retention_forbidden.store(true, Ordering::Release);
            return Err(WorkflowError::JobExecutionFailed(format!(
                "Worker did not confirm that container {} is bound only to private address {}; refusing to retain or route it. Upgrade the Temps agent on this node.",
                replica.result.container_id, private_host_ip
            )));
        }
        Ok(())
    }

    /// Wait-for-running phase: poll until Docker reports the container running,
    /// failing on an exit, a crash loop, or the startup budget running out.
    async fn wait_for_replica_running(
        &self,
        replica: &CreatedReplica<'_>,
        clock: &StartupClock,
    ) -> Result<(), WorkflowError> {
        let context = replica.context;
        let deployer = replica.deployer;
        let container_id = replica.result.container_id.as_str();
        loop {
            // Try to get container info, but don't fail hard if it can't be found
            // (container might have been removed by Docker or other processes)
            let container_info = match tokio::time::timeout(
                clock.remaining(),
                deployer.get_container_info(container_id),
            )
            .await
            .map_err(|_| {
                WorkflowError::JobExecutionFailed(format!(
                    "Container {} inspection exceeded startup timeout",
                    container_id
                ))
            })? {
                Ok(info) => info,
                Err(e) => {
                    // Container not found - might have been removed, but that's okay
                    // Log a warning but don't fail the deployment
                    tracing::warn!(
                        "Cannot verify container {} during deployment - container may have been removed: {}",
                        container_id,
                        e
                    );
                    self.log(
                        context,
                        format!(
                            "⏳ Container status check failed (may have been removed): {}",
                            e
                        ),
                    )
                    .await?;

                    // Wait a bit and try again, but don't fail if we can't verify
                    if clock.expired() {
                        return Err(WorkflowError::JobExecutionFailed(format!(
                            "Container {} could not be inspected within the startup timeout",
                            container_id
                        )));
                    }
                    clock.pause(Duration::from_secs(5)).await;
                    continue;
                }
            };

            if container_info.restart_count.unwrap_or(0) >= CRASH_LOOP_RESTART_THRESHOLD {
                self.write_startup_log_excerpt(context, deployer.as_ref(), container_id)
                    .await;
                return Err(WorkflowError::JobExecutionFailed(
                    container_crash_loop_message(
                        &container_info,
                        container_info.restart_count.unwrap_or(0),
                    ),
                ));
            }
            match container_info.status {
                DeployerContainerStatus::Running => {
                    self.log(context, "✅ Container is running".to_string())
                        .await?;
                    return Ok(());
                }
                DeployerContainerStatus::Exited | DeployerContainerStatus::Dead => {
                    self.write_startup_log_excerpt(context, deployer.as_ref(), container_id)
                        .await;
                    let message =
                        container_exit_failure_message(&container_info, clock.elapsed_secs());
                    self.log(context, format!("❌ {message}")).await?;
                    return Err(WorkflowError::JobExecutionFailed(message));
                }
                DeployerContainerStatus::Created => {
                    self.keep_waiting_for_replica_start(
                        context,
                        clock,
                        "created",
                        &container_info.status,
                    )
                    .await?;
                }
                _ => {
                    let state = format!("{:?}", container_info.status);
                    self.keep_waiting_for_replica_start(
                        context,
                        clock,
                        &state,
                        &container_info.status,
                    )
                    .await?;
                }
            }
        }
    }

    async fn keep_waiting_for_replica_start(
        &self,
        context: &WorkflowContext,
        clock: &StartupClock,
        state: &str,
        status: &DeployerContainerStatus,
    ) -> Result<(), WorkflowError> {
        if clock.expired() {
            let message = format!(
                "Container took too long to start: still in Docker state '{state}' \
                 after waiting {}s (readiness limit of {}s)",
                clock.elapsed_secs(),
                clock.limit_secs()
            );
            self.log(context, format!("⏱️  {message}")).await?;
            return Err(WorkflowError::JobExecutionFailed(message));
        }
        self.log(
            context,
            format!("Container status: {:?}, waiting...", status),
        )
        .await?;
        clock.pause(Duration::from_secs(5)).await;
        Ok(())
    }

    /// Log phase: stream container logs in background (non-blocking), which
    /// runs concurrently with health checks. The handle is stored for cleanup
    /// on cancellation.
    fn spawn_replica_log_stream(&self, replica: &CreatedReplica<'_>) {
        let log_task = tokio::spawn(stream_replica_startup_logs(
            replica.deployer.clone(),
            replica.result.container_id.clone(),
            replica.context.clone(),
            self.log_id.clone(),
            self.log_service.clone(),
        ));

        let mut task_handle = lock_deployment_state(&self.log_stream_task, "log_stream_task");
        *task_handle = Some(log_task);
    }

    /// Precedence (highest first):
    ///   1. explicit deploy-time override (config.health_check_path_override) —
    ///      set by image/static deploys that can't read .temps.yaml
    ///   2. .temps.yaml health.path (build job output, passed as health_check_override)
    ///   3. default config.health_check_path ("/" unless explicitly cleared)
    fn effective_replica_health_path(&self, health_check_override: Option<&str>) -> Option<String> {
        self.config
            .health_check_path_override
            .clone()
            .or_else(|| health_check_override.map(String::from))
            .or_else(|| self.config.health.as_ref().map(|h| h.path.clone()))
            .or_else(|| self.config.health_check_path.clone())
    }

    /// Readiness phase: wait for the application to be ready.
    /// When no path resolves, skip HTTP health checks: only the container running
    /// status from the previous phase gates the deploy. Rollbacks and promotions
    /// must not opt into this: they run the reused image against the target
    /// environment's current variables, so having passed once proves nothing.
    async fn wait_for_replica_ready(
        &self,
        replica: &CreatedReplica<'_>,
        health: &HealthCheckConfig,
        health_check_override: Option<&str>,
        clock: &StartupClock,
    ) -> Result<(), WorkflowError> {
        let effective_health_path = self.effective_replica_health_path(health_check_override);
        let kind = health.kind(replica.container_port.is_some());
        if matches!(kind, "http" | "tcp") && replica.container_port.is_none() {
            return Err(WorkflowError::JobValidationFailed(format!("health.type: {kind} requires a declared port; for a background worker set health.type: process")));
        }
        if kind != "http" {
            self.probe_replica_without_http(replica, health, clock)
                .await
        } else if let Some(ref health_path) = effective_health_path {
            self.probe_replica_over_http(replica, health_path, clock)
                .await
        } else {
            self.log(
                replica.context,
                "Health check path not configured - skipping HTTP health checks (container is running)".to_string(),
            )
            .await
        }
    }

    async fn probe_replica_without_http(
        &self,
        replica: &CreatedReplica<'_>,
        health: &HealthCheckConfig,
        clock: &StartupClock,
    ) -> Result<(), WorkflowError> {
        let address = replica.container_port.map(|port| {
            let url = replica_url(
                replica.assignment,
                &replica.result.container_name,
                port,
                replica.result.host_port,
                None,
            );
            url.trim_start_matches("http://").to_string()
        });
        if let Err(error) = wait_for_non_http_probe(
            replica.deployer.as_ref(),
            &replica.result.container_id,
            health,
            address.as_deref(),
            clock.remaining(),
            Some((self, replica.context)),
        )
        .await
        {
            self.write_startup_log_excerpt(
                replica.context,
                replica.deployer.as_ref(),
                &replica.result.container_id,
            )
            .await;
            return Err(error);
        }
        Ok(())
    }

    async fn probe_replica_over_http(
        &self,
        replica: &CreatedReplica<'_>,
        health_path: &str,
        clock: &StartupClock,
    ) -> Result<(), WorkflowError> {
        let context = replica.context;
        self.log(
            context,
            "Waiting for application to be ready...".to_string(),
        )
        .await?;
        let health_check_url = replica_url(
            replica.assignment,
            &replica.result.container_name,
            replica.result.container_port,
            replica.result.host_port,
            Some(health_path),
        );
        self.log(context, format!("Health check URL: {}", health_check_url))
            .await?;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to create HTTP client: {}", e))
            })?;

        let mut consecutive_successes = 0;
        let required_successes = 2; // Require 2 consecutive successful connections

        // What the last failed probe saw, so a timeout can say *why* the
        // app never became ready instead of a bare "timed out".
        let mut probe = ReadinessProbeState::default();
        let probe_port = replica.result.container_port;

        loop {
            // Check for overall timeout
            if clock.expired() {
                let message = probe.timeout_message(
                    probe_port,
                    health_path,
                    clock.limit_secs(),
                    clock.elapsed_secs(),
                );
                self.log(context, format!("⏱️  {message}")).await?;
                return Err(WorkflowError::JobExecutionFailed(message));
            }

            self.fail_if_replica_stopped(replica, clock).await?;

            let request_budget = clock.remaining().min(Duration::from_secs(5));
            match client
                .get(&health_check_url)
                .timeout(request_budget)
                .send()
                .await
            {
                Ok(response) => {
                    let status = response.status();

                    // Any HTTP response means the server is running.
                    // 2xx, 3xx, 404, and 405 are all valid — the health check
                    // path may not exist but the server is up and responding.
                    // Only 5xx indicates a real problem.
                    let is_healthy = http_probe_status_is_healthy(status);
                    probe.responded = true;
                    if is_healthy {
                        consecutive_successes += 1;

                        let message = format!(
                            "Health check passed - server healthy with status {} ({}/{})",
                            status, consecutive_successes, required_successes
                        );
                        if let (Some(ref log_id), Some(ref log_service)) =
                            (&self.log_id, &self.log_service)
                        {
                            log_service
                                .append_structured_log(log_id, LogLevel::Success, message.clone())
                                .await
                                .map_err(|e| {
                                    WorkflowError::Other(format!("Failed to write log: {}", e))
                                })?;
                        }
                        context.log(&message).await?;

                        if consecutive_successes >= required_successes {
                            self.log(context, "Application is ready and healthy!".to_string())
                                .await?;
                            return Ok(());
                        }
                        clock.pause(Duration::from_secs(2)).await;
                    } else {
                        // 4xx, 5xx = application error
                        consecutive_successes = 0;
                        probe.last_failure = Some(format!("HTTP {status}"));

                        self.log(
                            context,
                            format!(
                                "Health check failed - server returned error status {} (not healthy), retrying... ({}/{}s)",
                                status, clock.elapsed_secs(), clock.limit_secs()
                            ),
                        )
                        .await?;
                        clock.pause(Duration::from_secs(5)).await;
                    }
                }
                Err(e) => {
                    consecutive_successes = 0; // Reset counter on connection error
                    probe.record_transport_error(&e);
                    self.log(
                        context,
                        format!("Connectivity check failed ({}), retrying...", e),
                    )
                    .await?;
                    clock.pause(Duration::from_secs(5)).await;
                }
            }
        }
    }

    /// Check if container is still running (it may have crashed).
    /// This prevents waiting the full timeout for a container that
    /// already exited — or that Docker keeps restarting, which
    /// `RestartPolicy::Always` reports as running between crashes.
    async fn fail_if_replica_stopped(
        &self,
        replica: &CreatedReplica<'_>,
        clock: &StartupClock,
    ) -> Result<(), WorkflowError> {
        let context = replica.context;
        let container_id = replica.result.container_id.as_str();
        let container_info = tokio::time::timeout(
            clock.remaining(),
            replica.deployer.get_container_info(container_id),
        )
        .await
        .map_err(|_| {
            WorkflowError::JobExecutionFailed(format!(
                "Container {} inspection exceeded startup timeout",
                container_id
            ))
        })?
        .map_err(|error| {
            WorkflowError::JobExecutionFailed(format!(
                "Cannot evaluate HTTP probe for container {}: {error}",
                container_id
            ))
        })?;
        let restarts = container_info.restart_count.unwrap_or(0);
        match container_info.status {
            DeployerContainerStatus::Exited | DeployerContainerStatus::Dead => {
                self.write_startup_log_excerpt(context, replica.deployer.as_ref(), container_id)
                    .await;
                let message = container_exit_failure_message(&container_info, clock.elapsed_secs());
                self.log(context, format!("❌ {message}")).await?;
                Err(WorkflowError::JobExecutionFailed(message))
            }
            _ if restarts >= CRASH_LOOP_RESTART_THRESHOLD => {
                self.write_startup_log_excerpt(context, replica.deployer.as_ref(), container_id)
                    .await;
                let message = container_crash_loop_message(&container_info, restarts);
                self.log(context, format!("❌ {message}")).await?;
                Err(WorkflowError::JobExecutionFailed(message))
            }
            _ => {
                // Container is still running, continue with connectivity checks
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicBool;
    use temps_deployer::{ContainerInfo, ContainerStats, DeployerError, ExecResult};

    const CONTAINER_ID: &str = "replica-container";

    /// Deployer whose create result and inspection sequence each test scripts.
    /// The last scripted inspection repeats. `created` flips on the create
    /// call, so a log writer can start failing at exactly that point.
    #[derive(Default)]
    struct ScriptedDeployer {
        create_error: Option<String>,
        docker_socket_mounted: bool,
        /// Host port the agent reports it picked; the request's otherwise.
        reported_host_port: Option<u16>,
        inspections: Mutex<VecDeque<Result<ContainerInfo, String>>>,
        logs: Option<String>,
        created: Arc<AtomicBool>,
        requests: Mutex<Vec<DeployRequest>>,
        removed: Mutex<Vec<String>>,
    }

    fn state(status: DeployerContainerStatus) -> Result<ContainerInfo, String> {
        Ok(ContainerInfo {
            status,
            restart_count: Some(0),
            ..Default::default()
        })
    }

    impl ScriptedDeployer {
        fn inspecting(
            inspections: impl IntoIterator<Item = Result<ContainerInfo, String>>,
        ) -> Self {
            Self {
                inspections: Mutex::new(inspections.into_iter().collect()),
                ..Default::default()
            }
        }

        fn creations(&self) -> usize {
            self.requests.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl ContainerDeployer for ScriptedDeployer {
        async fn exec_in_container(
            &self,
            container_id: &str,
            _command: Vec<String>,
            _timeout: Duration,
        ) -> Result<ExecResult, DeployerError> {
            Err(DeployerError::ExecFailed {
                container_id: container_id.to_string(),
                reason: "probe command failed".to_string(),
            })
        }

        async fn deploy_container(
            &self,
            request: DeployRequest,
        ) -> Result<DeployResult, DeployerError> {
            self.requests.lock().unwrap().push(request.clone());
            if let Some(reason) = &self.create_error {
                return Err(DeployerError::Other(reason.clone()));
            }
            self.created.store(true, Ordering::SeqCst);
            let mapping = request.port_mappings.first();
            Ok(DeployResult {
                container_id: CONTAINER_ID.to_string(),
                container_name: request.container_name.clone(),
                container_port: mapping.map(|p| p.container_port).unwrap_or(8080),
                host_port: self
                    .reported_host_port
                    .or(mapping.map(|p| p.host_port))
                    .unwrap_or(8080),
                status: DeployerContainerStatus::Running,
                docker_socket_mounted: self.docker_socket_mounted,
            })
        }

        async fn start_container(&self, _: &str) -> Result<(), DeployerError> {
            Ok(())
        }

        async fn stop_container(&self, _: &str) -> Result<(), DeployerError> {
            Ok(())
        }

        async fn pause_container(&self, _: &str) -> Result<(), DeployerError> {
            Ok(())
        }

        async fn resume_container(&self, _: &str) -> Result<(), DeployerError> {
            Ok(())
        }

        async fn remove_container(&self, container_id: &str) -> Result<(), DeployerError> {
            self.removed.lock().unwrap().push(container_id.to_string());
            Ok(())
        }

        async fn get_container_info(&self, _: &str) -> Result<ContainerInfo, DeployerError> {
            let mut inspections = self.inspections.lock().unwrap();
            let next = if inspections.len() > 1 {
                inspections.pop_front()
            } else {
                inspections.front().cloned()
            };
            match next {
                Some(Ok(info)) => Ok(info),
                Some(Err(reason)) => Err(DeployerError::Other(reason)),
                None => state(DeployerContainerStatus::Running).map_err(DeployerError::Other),
            }
        }

        async fn get_container_stats(&self, _: &str) -> Result<ContainerStats, DeployerError> {
            Err(DeployerError::Other("not scripted".to_string()))
        }

        async fn list_containers(&self) -> Result<Vec<ContainerInfo>, DeployerError> {
            Ok(Vec::new())
        }

        async fn get_container_logs(&self, _: &str) -> Result<String, DeployerError> {
            self.logs
                .clone()
                .ok_or_else(|| DeployerError::Other("no logs scripted".to_string()))
        }

        async fn stream_container_logs(
            &self,
            _: &str,
        ) -> Result<Box<dyn futures::Stream<Item = String> + Unpin + Send>, DeployerError> {
            Err(DeployerError::Other("not scripted".to_string()))
        }
    }

    /// Records deploy-log lines, and fails each write while `failing` is set.
    #[derive(Default)]
    struct RecordingLogWriter {
        lines: Mutex<Vec<String>>,
        failing: Arc<AtomicBool>,
    }

    #[async_trait]
    impl crate::workflow::LogWriter for RecordingLogWriter {
        async fn write_log(&self, message: String) -> Result<(), WorkflowError> {
            if self.failing.load(Ordering::SeqCst) {
                return Err(WorkflowError::Other("log store unavailable".to_string()));
            }
            self.lines.lock().unwrap().push(message);
            Ok(())
        }

        fn stage_id(&self) -> i32 {
            1
        }
    }

    fn builder() -> DeployImageJobBuilder {
        DeployImageJobBuilder::new(
            "test-project",
            temps_core::docker_socket_grant::DeployCaller::Platform,
        )
        .job_id("deploy".to_string())
        .build_job_id("build".to_string())
        .target(DeploymentTarget::Docker {
            registry_url: "local".to_string(),
            network: None,
        })
        .service_name("web".to_string())
        .configured_port(Some(3000))
        .health_check_path(None)
        .health_check_timeout_secs(1)
    }

    fn context_writing_to(writer: &Arc<RecordingLogWriter>) -> WorkflowContext {
        WorkflowContext::new("run-7".to_string(), 7, 2, 3, writer.clone())
    }

    fn remote_on(private_address: &str) -> NodeAssignment {
        NodeAssignment::Remote {
            node_id: 9,
            node_name: "worker-9".to_string(),
            address: "http://127.0.0.1:1".to_string(),
            private_address: private_address.to_string(),
            platform: None,
        }
    }

    async fn deploy(
        job: &DeployImageJob,
        context: &WorkflowContext,
        deployer: &Arc<ScriptedDeployer>,
        assignment: &NodeAssignment,
        docker_socket_required: bool,
    ) -> Result<(String, u16, Option<u16>), WorkflowError> {
        let target: Arc<dyn ContainerDeployer> = deployer.clone();
        job.deploy_single_replica(
            "app:1",
            context,
            0,
            None,
            &target,
            ReplicaTarget {
                assignment,
                docker_socket_required,
            },
        )
        .await
    }

    /// Nothing was created, so nothing may be tracked or retained.
    fn assert_nothing_created(job: &DeployImageJob, deployer: &ScriptedDeployer) {
        assert_eq!(deployer.creations(), 0, "the create call must not run");
        assert_nothing_tracked(job);
    }

    fn assert_nothing_tracked(job: &DeployImageJob) {
        assert!(job.container_ids.lock().unwrap().is_empty());
        assert!(job.replica_deployers.lock().unwrap().is_empty());
        assert!(job.failed_candidates.lock().unwrap().is_empty());
    }

    /// The leak rule: a failure after the create must leave the container
    /// tracked with its owning deployer, so cleanup can remove it from the
    /// node it actually runs on.
    async fn assert_tracked_and_removable(
        job: &DeployImageJob,
        context: &WorkflowContext,
        deployer: &ScriptedDeployer,
    ) {
        assert_eq!(
            job.container_ids.lock().unwrap().as_slice(),
            [CONTAINER_ID],
            "a created container must stay tracked"
        );
        assert!(job
            .replica_deployers
            .lock()
            .unwrap()
            .contains_key(CONTAINER_ID));
        job.cleanup_container(context)
            .await
            .expect("cleanup must succeed");
        assert_eq!(
            deployer.removed.lock().unwrap().as_slice(),
            [CONTAINER_ID],
            "cleanup must remove it through the deployer that created it"
        );
    }

    fn execution_message(error: WorkflowError) -> String {
        match error {
            WorkflowError::JobExecutionFailed(message) => message,
            other => panic!("expected JobExecutionFailed, got {other:?}"),
        }
    }

    /// An HTTP server answering every request with `status`.
    async fn spawn_http_server(status: &'static str) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = [0_u8; 1024];
                let _ = stream.read(&mut request).await;
                let response =
                    format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        port
    }

    /// A port nothing is listening on.
    async fn closed_port() -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap().port()
    }

    /// Inspection result proving a private-only bind for a remote replica.
    fn privately_bound(host_port: u16) -> Result<ContainerInfo, String> {
        Ok(ContainerInfo {
            status: DeployerContainerStatus::Running,
            restart_count: Some(0),
            ports: vec![PortMapping {
                host_port,
                container_port: 3000,
                protocol: Protocol::Tcp,
                host_ip: Some("127.0.0.1".to_string()),
            }],
            ..Default::default()
        })
    }

    // Prepare phase: every failure happens before the create call.

    #[tokio::test]
    async fn an_invalid_health_probe_fails_before_anything_is_created() {
        let deployer = Arc::new(ScriptedDeployer::default());
        let job = builder()
            .health(Some(HealthCheckConfig {
                probe_type: Some("gopher".to_string()),
                ..Default::default()
            }))
            .build(deployer.clone())
            .unwrap();
        let context = context_writing_to(&Arc::default());

        let error = deploy(&job, &context, &deployer, &NodeAssignment::Local, false)
            .await
            .unwrap_err();

        match error {
            WorkflowError::JobValidationFailed(message) => {
                assert!(message.contains("deployment 7"), "{message}");
                assert!(message.contains("gopher"), "{message}");
            }
            other => panic!("expected JobValidationFailed, got {other:?}"),
        }
        assert_nothing_created(&job, &deployer);
    }

    #[tokio::test]
    async fn an_unreadable_repository_health_config_fails_before_anything_is_created() {
        let deployer = Arc::new(ScriptedDeployer::default());
        let job = builder().build(deployer.clone()).unwrap();
        let mut context = context_writing_to(&Arc::default());
        context
            .set_output("build", "health", "not a health config")
            .unwrap();

        deploy(&job, &context, &deployer, &NodeAssignment::Local, false)
            .await
            .unwrap_err();

        assert_nothing_created(&job, &deployer);
    }

    #[tokio::test]
    async fn a_remote_port_lookup_failure_fails_before_anything_is_created() {
        let deployer = Arc::new(ScriptedDeployer::default());
        let job = builder()
            .configured_port(None)
            .build(deployer.clone())
            .unwrap();
        let context = context_writing_to(&Arc::default());

        let error = deploy(&job, &context, &deployer, &remote_on("10.0.0.9"), false)
            .await
            .unwrap_err();

        let message = execution_message(error);
        assert!(message.contains("build node 9"), "{message}");
        assert_nothing_created(&job, &deployer);
    }

    #[tokio::test]
    async fn a_public_worker_address_is_refused_before_anything_is_created() {
        let deployer = Arc::new(ScriptedDeployer::default());
        let job = builder().build(deployer.clone()).unwrap();
        let context = context_writing_to(&Arc::default());

        let error = deploy(&job, &context, &deployer, &remote_on("8.8.8.8"), false)
            .await
            .unwrap_err();

        let message = execution_message(error);
        assert!(message.contains("'8.8.8.8' is public"), "{message}");
        assert_nothing_created(&job, &deployer);
    }

    #[tokio::test]
    async fn an_unreachable_linked_service_is_refused_before_anything_is_created() {
        let deployer = Arc::new(ScriptedDeployer::default());
        let job = builder()
            .cross_node_service_blockers(vec![crate::services::CrossNodeServiceBlocker {
                service_id: 3,
                service_name: "db".to_string(),
                fqdn: None,
                detail: "cluster DNS is disabled".to_string(),
                remedy: "enable cluster DNS".to_string(),
                setup_path: "/settings".to_string(),
            }])
            .build(deployer.clone())
            .unwrap();
        let writer = Arc::new(RecordingLogWriter::default());
        let context = context_writing_to(&writer);

        let error = deploy(&job, &context, &deployer, &remote_on("10.0.0.9"), false)
            .await
            .unwrap_err();

        assert!(
            matches!(
                error,
                WorkflowError::CrossNodeServiceUnreachable { ref node_name, .. }
                    if node_name == "worker-9"
            ),
            "{error:?}"
        );
        // The deploy log is where the operator sees this.
        assert!(writer
            .lines
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains("worker-9")));
        assert_nothing_created(&job, &deployer);
    }

    #[tokio::test]
    async fn a_local_replica_ignores_cross_node_blockers_and_its_remote_env() {
        let deployer = Arc::new(ScriptedDeployer::default());
        let job = builder()
            .cross_node_service_blockers(vec![crate::services::CrossNodeServiceBlocker {
                service_id: 3,
                service_name: "db".to_string(),
                fqdn: None,
                detail: "cluster DNS is disabled".to_string(),
                remedy: "enable cluster DNS".to_string(),
                setup_path: "/settings".to_string(),
            }])
            .build(deployer.clone())
            .unwrap();
        let context = context_writing_to(&Arc::default());

        deploy(&job, &context, &deployer, &NodeAssignment::Local, false)
            .await
            .expect("a control-plane replica reaches services by name");

        let requests = deployer.requests.lock().unwrap();
        let env = &requests[0].environment_vars;
        assert_eq!(env["TEMPS_NODE_NAME"], "control-plane");
        assert_eq!(env["TEMPS_NODE_ID"], "0");
        assert_eq!(env["TEMPS_REPLICA"], "1");
    }

    // Create phase.

    #[tokio::test]
    async fn a_failed_create_leaves_nothing_tracked() {
        let deployer = Arc::new(ScriptedDeployer {
            create_error: Some("no space left".to_string()),
            ..Default::default()
        });
        let job = builder().build(deployer.clone()).unwrap();
        let context = context_writing_to(&Arc::default());

        let error = deploy(&job, &context, &deployer, &NodeAssignment::Local, false)
            .await
            .unwrap_err();

        let message = execution_message(error);
        assert!(
            message.starts_with("Failed to deploy container:"),
            "{message}"
        );
        assert!(message.contains("no space left"), "{message}");
        assert_nothing_tracked(&job);
    }

    /// The rule the whole phase order exists for: the first fallible step after
    /// the create is a log write, and it runs on an already tracked container.
    #[tokio::test]
    async fn a_log_write_failure_right_after_create_cannot_leak_the_container() {
        for docker_socket_mounted in [false, true] {
            let deployer = Arc::new(ScriptedDeployer {
                docker_socket_mounted,
                ..Default::default()
            });
            let job = builder().build(deployer.clone()).unwrap();
            let writer = Arc::new(RecordingLogWriter {
                failing: deployer.created.clone(),
                ..Default::default()
            });
            let context = context_writing_to(&writer);

            deploy(&job, &context, &deployer, &NodeAssignment::Local, false)
                .await
                .expect_err("the failing log write must fail the replica");

            assert_eq!(deployer.creations(), 1);
            writer.failing.store(false, Ordering::SeqCst);
            assert_tracked_and_removable(&job, &context, &deployer).await;
        }
    }

    // Verify phase.

    #[tokio::test]
    async fn a_gated_replica_without_the_socket_stays_tracked_but_is_not_a_candidate() {
        let deployer = Arc::new(ScriptedDeployer::default());
        let job = builder().build(deployer.clone()).unwrap();
        let context = context_writing_to(&Arc::default());

        let error = deploy(&job, &context, &deployer, &NodeAssignment::Local, true)
            .await
            .unwrap_err();

        assert!(
            matches!(error, WorkflowError::DockerSocketNotMounted { .. }),
            "{error:?}"
        );
        assert!(job.failed_candidates.lock().unwrap().is_empty());
        assert_tracked_and_removable(&job, &context, &deployer).await;
    }

    /// An agent that ignores `host_ip`, or that cannot be inspected at all,
    /// fails closed: the container is torn down, never retained.
    #[tokio::test]
    async fn an_unconfirmed_private_binding_forbids_retention_and_stays_tracked() {
        let published_on_all_interfaces = Ok(ContainerInfo {
            status: DeployerContainerStatus::Running,
            ports: vec![PortMapping {
                host_port: 49000,
                container_port: 3000,
                protocol: Protocol::Tcp,
                host_ip: Some("0.0.0.0".to_string()),
            }],
            ..Default::default()
        });
        for inspection in [
            published_on_all_interfaces,
            Err("agent unreachable".to_string()),
        ] {
            let deployer = Arc::new(ScriptedDeployer {
                reported_host_port: Some(49000),
                ..ScriptedDeployer::inspecting([inspection])
            });
            let job = builder().build(deployer.clone()).unwrap();
            let context = context_writing_to(&Arc::default());

            let error = deploy(&job, &context, &deployer, &remote_on("10.0.0.9"), false)
                .await
                .unwrap_err();

            let message = execution_message(error);
            assert!(message.contains(CONTAINER_ID), "{message}");
            assert!(message.contains("10.0.0.9"), "{message}");
            assert!(job.retention_forbidden.load(Ordering::Acquire));
            assert!(job.failed_candidates.lock().unwrap().is_empty());
            assert_tracked_and_removable(&job, &context, &deployer).await;
        }
    }

    #[tokio::test]
    async fn a_confirmed_replica_is_recorded_as_a_retention_candidate_on_its_node() {
        let deployer = Arc::new(ScriptedDeployer {
            reported_host_port: Some(49000),
            ..ScriptedDeployer::inspecting([privately_bound(49000)])
        });
        let job = builder().build(deployer.clone()).unwrap();
        let context = context_writing_to(&Arc::default());

        let (container_id, host_port, container_port) =
            deploy(&job, &context, &deployer, &remote_on("127.0.0.1"), false)
                .await
                .expect("a private bind with no health path is ready once running");

        assert_eq!(container_id, CONTAINER_ID);
        assert_eq!(host_port, 49000);
        assert_eq!(container_port, Some(3000));
        assert!(!job.retention_forbidden.load(Ordering::Acquire));
        let candidates = job.failed_candidates.lock().unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].container_id, CONTAINER_ID);
        assert_eq!(candidates[0].image_name, "app:1");
        assert_eq!(candidates[0].node_id, Some(9));
        assert_eq!(candidates[0].host_port, 49000);
    }

    // Wait-for-running phase. Each case is a candidate, so its logs stay
    // reachable after the failure.

    #[tokio::test]
    async fn an_exit_during_startup_reports_why_and_stays_tracked() {
        let deployer = Arc::new(ScriptedDeployer {
            logs: Some("boom: missing module".to_string()),
            ..ScriptedDeployer::inspecting([Ok(ContainerInfo {
                status: DeployerContainerStatus::Exited,
                exit_code: Some(3),
                restart_count: Some(0),
                ..Default::default()
            })])
        });
        let job = builder().build(deployer.clone()).unwrap();
        let writer = Arc::new(RecordingLogWriter::default());
        let context = context_writing_to(&writer);

        let error = deploy(&job, &context, &deployer, &NodeAssignment::Local, false)
            .await
            .unwrap_err();

        let message = execution_message(error);
        assert!(
            message.starts_with("Container exited during startup"),
            "{message}"
        );
        assert!(
            !message.contains("boom"),
            "runtime output stays out of the failure reason: {message}"
        );
        assert!(writer
            .lines
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains("boom: missing module")));
        assert_eq!(job.failed_candidates.lock().unwrap().len(), 1);
        assert_tracked_and_removable(&job, &context, &deployer).await;
    }

    #[tokio::test]
    async fn a_crash_loop_during_startup_fails_and_stays_tracked() {
        let deployer = Arc::new(ScriptedDeployer::inspecting([Ok(ContainerInfo {
            status: DeployerContainerStatus::Running,
            restart_count: Some(CRASH_LOOP_RESTART_THRESHOLD),
            ..Default::default()
        })]));
        let job = builder().build(deployer.clone()).unwrap();
        let context = context_writing_to(&Arc::default());

        let error = deploy(&job, &context, &deployer, &NodeAssignment::Local, false)
            .await
            .unwrap_err();

        let message = execution_message(error);
        assert!(message.contains("keeps restarting"), "{message}");
        assert_tracked_and_removable(&job, &context, &deployer).await;
    }

    #[tokio::test]
    async fn an_uninspectable_container_times_out_and_stays_tracked() {
        let deployer = Arc::new(ScriptedDeployer::inspecting([Err(
            "no such container".to_string()
        )]));
        let job = builder().build(deployer.clone()).unwrap();
        let context = context_writing_to(&Arc::default());

        let error = deploy(&job, &context, &deployer, &NodeAssignment::Local, false)
            .await
            .unwrap_err();

        let message = execution_message(error);
        assert!(
            message.contains("could not be inspected within the startup timeout"),
            "{message}"
        );
        assert_tracked_and_removable(&job, &context, &deployer).await;
    }

    #[tokio::test]
    async fn a_container_that_never_starts_names_its_state_and_stays_tracked() {
        for (status, expected) in [
            (DeployerContainerStatus::Created, "'created'"),
            (DeployerContainerStatus::Paused, "'Paused'"),
        ] {
            let deployer = Arc::new(ScriptedDeployer::inspecting([state(status)]));
            let job = builder().build(deployer.clone()).unwrap();
            let context = context_writing_to(&Arc::default());

            let error = deploy(&job, &context, &deployer, &NodeAssignment::Local, false)
                .await
                .unwrap_err();

            let message = execution_message(error);
            assert!(
                message.starts_with("Container took too long to start"),
                "{message}"
            );
            assert!(message.contains(expected), "{message}");
            assert_tracked_and_removable(&job, &context, &deployer).await;
        }
    }

    // Readiness phase.

    #[tokio::test]
    async fn a_port_probe_on_a_portless_worker_is_refused_and_stays_tracked() {
        let deployer = Arc::new(ScriptedDeployer::default());
        let job = builder()
            .configured_port(None)
            .health(Some(HealthCheckConfig {
                probe_type: Some("tcp".to_string()),
                ..Default::default()
            }))
            .build(deployer.clone())
            .unwrap();
        let context = {
            let mut context = context_writing_to(&Arc::default());
            context
                .set_output(
                    "build",
                    "image_config",
                    crate::jobs::build_image::BuiltImageConfig {
                        exposed_ports: Vec::new(),
                        working_dir: None,
                        user: None,
                        entrypoint: Vec::new(),
                    },
                )
                .unwrap();
            context
        };

        let error = deploy(&job, &context, &deployer, &NodeAssignment::Local, false)
            .await
            .unwrap_err();

        match error {
            WorkflowError::JobValidationFailed(message) => {
                assert!(message.contains("health.type: tcp"), "{message}");
            }
            other => panic!("expected JobValidationFailed, got {other:?}"),
        }
        assert_tracked_and_removable(&job, &context, &deployer).await;
    }

    #[tokio::test]
    async fn a_failed_exec_probe_writes_the_startup_logs_and_stays_tracked() {
        let deployer = Arc::new(ScriptedDeployer {
            logs: Some("worker never came up".to_string()),
            ..Default::default()
        });
        let job = builder()
            .health(Some(HealthCheckConfig {
                probe_type: Some("exec".to_string()),
                command: vec!["check".to_string()],
                ..Default::default()
            }))
            .build(deployer.clone())
            .unwrap();
        let writer = Arc::new(RecordingLogWriter::default());
        let context = context_writing_to(&writer);

        deploy(&job, &context, &deployer, &NodeAssignment::Local, false)
            .await
            .expect_err("a probe that never passes must fail the replica");

        assert!(writer
            .lines
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains("worker never came up")));
        assert_tracked_and_removable(&job, &context, &deployer).await;
    }

    /// Deployer and job for an HTTP-probed replica on `host_port`: the first
    /// inspection proves the private bind, then `inspections` play in order.
    fn http_probed(
        host_port: u16,
        inspections: Vec<Result<ContainerInfo, String>>,
    ) -> (Arc<ScriptedDeployer>, DeployImageJob) {
        let mut script = vec![privately_bound(host_port), privately_bound(host_port)];
        script.extend(inspections);
        let deployer = Arc::new(ScriptedDeployer {
            reported_host_port: Some(host_port),
            ..ScriptedDeployer::inspecting(script)
        });
        let job = builder()
            .health_check_path(Some("/ready".to_string()))
            .build(deployer.clone())
            .unwrap();
        (deployer, job)
    }

    #[tokio::test]
    async fn an_app_that_never_listens_times_out_naming_the_port_and_stays_tracked() {
        let (deployer, job) = http_probed(closed_port().await, Vec::new());
        let context = context_writing_to(&Arc::default());

        let error = deploy(&job, &context, &deployer, &remote_on("127.0.0.1"), false)
            .await
            .unwrap_err();

        let message = execution_message(error);
        assert!(
            message.contains("never accepted connections on port 3000"),
            "{message}"
        );
        assert_tracked_and_removable(&job, &context, &deployer).await;
    }

    #[tokio::test]
    async fn an_app_answering_5xx_times_out_as_unhealthy_and_stays_tracked() {
        let port = spawn_http_server("503 Service Unavailable").await;
        let (deployer, job) = http_probed(port, Vec::new());
        let writer = Arc::new(RecordingLogWriter::default());
        let context = context_writing_to(&writer);

        let error = deploy(&job, &context, &deployer, &remote_on("127.0.0.1"), false)
            .await
            .unwrap_err();

        let message = execution_message(error);
        assert!(message.contains("health checks did not pass"), "{message}");
        assert!(writer
            .lines
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains(&format!("127.0.0.1:{port}/ready"))));
        assert_tracked_and_removable(&job, &context, &deployer).await;
    }

    #[tokio::test]
    async fn an_exit_while_probing_fails_at_once_and_stays_tracked() {
        let port = closed_port().await;
        let (deployer, job) = http_probed(
            port,
            vec![Ok(ContainerInfo {
                status: DeployerContainerStatus::Dead,
                exit_code: Some(137),
                restart_count: Some(0),
                ..Default::default()
            })],
        );
        let context = context_writing_to(&Arc::default());

        let error = deploy(&job, &context, &deployer, &remote_on("127.0.0.1"), false)
            .await
            .unwrap_err();

        let message = execution_message(error);
        assert!(
            message.starts_with("Container exited during startup"),
            "{message}"
        );
        assert_tracked_and_removable(&job, &context, &deployer).await;
    }

    #[tokio::test]
    async fn a_crash_loop_while_probing_fails_at_once_and_stays_tracked() {
        let port = closed_port().await;
        let (deployer, job) = http_probed(
            port,
            vec![Ok(ContainerInfo {
                status: DeployerContainerStatus::Running,
                restart_count: Some(CRASH_LOOP_RESTART_THRESHOLD),
                ..Default::default()
            })],
        );
        let context = context_writing_to(&Arc::default());

        let error = deploy(&job, &context, &deployer, &remote_on("127.0.0.1"), false)
            .await
            .unwrap_err();

        let message = execution_message(error);
        assert!(message.contains("keeps restarting"), "{message}");
        assert_tracked_and_removable(&job, &context, &deployer).await;
    }

    #[tokio::test]
    async fn losing_the_container_while_probing_fails_and_stays_tracked() {
        let port = closed_port().await;
        let (deployer, job) = http_probed(port, vec![Err("agent unreachable".to_string())]);
        let context = context_writing_to(&Arc::default());

        let error = deploy(&job, &context, &deployer, &remote_on("127.0.0.1"), false)
            .await
            .unwrap_err();

        let message = execution_message(error);
        assert!(
            message.contains("Cannot evaluate HTTP probe for container"),
            "{message}"
        );
        assert_tracked_and_removable(&job, &context, &deployer).await;
    }

    #[tokio::test]
    async fn two_consecutive_healthy_probes_make_the_replica_ready() {
        let port = spawn_http_server("204 No Content").await;
        let (deployer, job) = http_probed(port, Vec::new());
        // Two probes with a pause between them must fit in the budget.
        let mut job = job;
        job.config.health_check_timeout_secs = 30;
        let writer = Arc::new(RecordingLogWriter::default());
        let context = context_writing_to(&writer);

        let (container_id, host_port, container_port) =
            deploy(&job, &context, &deployer, &remote_on("127.0.0.1"), false)
                .await
                .expect("a healthy app is ready");

        assert_eq!(container_id, CONTAINER_ID);
        assert_eq!(host_port, port);
        assert_eq!(container_port, Some(3000));
        let lines = writer.lines.lock().unwrap();
        assert!(lines.iter().any(|line| line.contains("(2/2)")));
        assert!(lines
            .iter()
            .any(|line| line.contains(&format!("ready at http://127.0.0.1:{port}"))));
    }

    #[test]
    fn health_path_precedence_is_override_then_repository_then_config_then_default() {
        let deployer = Arc::new(ScriptedDeployer::default());
        let job = |customize: fn(DeployImageJobBuilder) -> DeployImageJobBuilder| {
            customize(builder()).build(deployer.clone()).unwrap()
        };

        let everything = job(|b| {
            b.health_check_path_override(Some("/override".to_string()))
                .health(Some(HealthCheckConfig {
                    path: "/config".to_string(),
                    ..Default::default()
                }))
                .health_check_path(Some("/default".to_string()))
        });
        assert_eq!(
            everything.effective_replica_health_path(Some("/repo")),
            Some("/override".to_string())
        );

        let no_override = job(|b| {
            b.health(Some(HealthCheckConfig {
                path: "/config".to_string(),
                ..Default::default()
            }))
            .health_check_path(Some("/default".to_string()))
        });
        assert_eq!(
            no_override.effective_replica_health_path(Some("/repo")),
            Some("/repo".to_string())
        );
        assert_eq!(
            no_override.effective_replica_health_path(None),
            Some("/config".to_string())
        );

        let default_only = job(|b| b.health_check_path(Some("/default".to_string())));
        assert_eq!(
            default_only.effective_replica_health_path(None),
            Some("/default".to_string())
        );

        let cleared = job(|b| b);
        assert_eq!(cleared.effective_replica_health_path(None), None);
    }

    #[test]
    fn replica_url_uses_the_private_address_for_remote_nodes() {
        assert_eq!(
            replica_url(&remote_on("10.0.0.9"), "web", 3000, 49000, Some("/ready")),
            "http://10.0.0.9:49000/ready"
        );
        assert_eq!(
            replica_url(&remote_on("10.0.0.9"), "web", 3000, 49000, None),
            "http://10.0.0.9:49000"
        );
        assert_eq!(
            replica_url(&NodeAssignment::Local, "web", 3000, 49000, Some("/ready")),
            temps_core::DeploymentMode::build_container_url("web", 3000, 49000, Some("/ready"))
        );
    }

    #[test]
    fn only_multi_replica_deployments_number_their_containers() {
        let deployer = Arc::new(ScriptedDeployer::default());
        let single = builder().build(deployer.clone()).unwrap();
        assert_eq!(single.replica_container_name(0), "web");
        let many = builder().replicas(3).build(deployer).unwrap();
        assert_eq!(many.replica_container_name(0), "web-1");
        assert_eq!(many.replica_container_name(2), "web-3");
    }

    #[test]
    fn labels_identify_the_deployment_for_log_collection_and_reconciliation() {
        let context = context_writing_to(&Arc::default());
        let labels = replica_labels(&context, "web");
        assert_eq!(labels["sh.temps.managed"], "true");
        assert_eq!(labels["sh.temps.service"], "web");
        assert_eq!(labels["sh.temps.deploy_id"], "7");
        assert_eq!(
            labels["sh.temps.project_id"],
            context.project_id.to_string()
        );
        assert_eq!(
            labels["sh.temps.environment"],
            context.environment_id.to_string()
        );
    }
}
