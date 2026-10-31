// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The `ContainerDeployer` impl of [`DockerRuntime`] and the container
//! inspection and stats helpers it relies on.

use super::guards::ProbeExecCleanup;
use super::log_tail::{DockerLogTail, MAX_CONTAINER_LOG_BYTES};
use super::{docker_socket_bind_for, DockerRuntime};
use crate::{
    ContainerDeployer, ContainerInfo, ContainerStatus, DeployRequest, DeployResult, DeployerError,
    PortMapping, Protocol,
};
use async_trait::async_trait;
use bollard::{
    query_parameters::{
        InspectContainerOptions, ListContainersOptions, LogsOptions, RemoveContainerOptions,
        StartContainerOptions, StopContainerOptions,
    },
    Docker,
};
use futures::{StreamExt, TryStreamExt};
use std::collections::HashMap;
use std::path::PathBuf;
use temps_workload_docker::secrets::{
    image_owner, validate_secret_dir_name, write_secrets_to_host_dir,
};
use tracing::{debug, info, warn};

/// A failed inspect as a deployer error. Only Docker's 404 means the
/// container is gone: callers record a container as removed on
/// [`DeployerError::ContainerNotFound`], so a daemon error, timeout or
/// unreadable response must never be reported that way.
fn inspect_error(container_id: &str, error: bollard::errors::Error) -> DeployerError {
    match error {
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            message,
        } => DeployerError::ContainerNotFound(format!(
            "Container {container_id} not found: {message}"
        )),
        other => DeployerError::Other(format!(
            "Failed to inspect container {container_id}: {other}"
        )),
    }
}

fn parse_inspected_port_mappings(
    ports: HashMap<String, Option<Vec<bollard::models::PortBinding>>>,
) -> Vec<PortMapping> {
    let mut mappings = Vec::new();
    for (port_key, bindings) in ports {
        let Some((container_port, protocol)) = port_key.split_once('/') else {
            continue;
        };
        let Ok(container_port) = container_port.parse() else {
            continue;
        };
        let protocol = match protocol {
            "tcp" => Protocol::Tcp,
            "udp" => Protocol::Udp,
            _ => continue,
        };
        for binding in bindings.unwrap_or_default() {
            let Some(host_port) = binding
                .host_port
                .as_deref()
                .and_then(|port| port.parse().ok())
            else {
                continue;
            };
            mappings.push(PortMapping {
                host_port,
                container_port,
                protocol: protocol.clone(),
                host_ip: binding.host_ip,
            });
        }
    }
    mappings
}

/// Map a POSIX exit code (>= 128 means "killed by signal N - 128") to a human
/// signal name. Returns None for normal-range exit codes.
fn signal_name_from_exit_code(code: i64) -> Option<&'static str> {
    if !(128..=192).contains(&code) {
        return None;
    }
    match (code - 128) as i32 {
        1 => Some("SIGHUP"),
        2 => Some("SIGINT"),
        3 => Some("SIGQUIT"),
        4 => Some("SIGILL"),
        6 => Some("SIGABRT"),
        8 => Some("SIGFPE"),
        9 => Some("SIGKILL"),
        11 => Some("SIGSEGV"),
        13 => Some("SIGPIPE"),
        14 => Some("SIGALRM"),
        15 => Some("SIGTERM"),
        _ => None,
    }
}

/// Build a short human-readable explanation of why a container is in its
/// current state. Returns None for containers that are still running, have
/// never been started, or exited cleanly with status 0 and no Docker error.
pub(crate) fn build_exit_reason(
    status: &ContainerStatus,
    oom_killed: Option<bool>,
    exit_code: Option<i64>,
    error: Option<&str>,
) -> Option<String> {
    let is_terminal = matches!(
        status,
        ContainerStatus::Exited | ContainerStatus::Dead | ContainerStatus::Stopped
    );
    if !is_terminal {
        return None;
    }
    if oom_killed == Some(true) {
        return Some(match exit_code {
            Some(code) => format!("OOMKilled (exit code {})", code),
            None => "OOMKilled".to_string(),
        });
    }
    if let Some(err) = error.filter(|e| !e.is_empty()) {
        return Some(match exit_code {
            Some(code) => format!("Error (exit code {}): {}", code, err),
            None => format!("Error: {}", err),
        });
    }
    match exit_code {
        Some(0) => None,
        Some(code) => Some(match signal_name_from_exit_code(code) {
            Some(sig) => format!("Killed by {} (exit code {})", sig, code),
            None => format!("Exit code {}", code),
        }),
        None => None,
    }
}

/// Sample container stats twice ~1s apart so the CPU delta formula has a real
/// window to work with. Docker's `one_shot: true` returns immediately but
/// leaves `precpu_stats` empty, which makes the single-sample CPU math
/// collapse to ~0% for any container that's been running long enough that
/// (cumulative_cpu_time / system_time_since_boot) rounds to zero. The 1s
/// interval matches the Docker CLI default.
/// Bytes read and written, summed over every device in a container's
/// `blkio_stats.io_service_bytes_recursive` (cgroup v1 reports `Read` and
/// `Write`, v2 reports `read` and `write`; the other operations are ignored).
fn block_io_bytes(entries: &[bollard::models::ContainerBlkioStatEntry]) -> (u64, u64) {
    let mut read = 0u64;
    let mut written = 0u64;
    for entry in entries {
        let value = entry.value.unwrap_or(0);
        match entry.op.as_deref().map(str::to_ascii_lowercase).as_deref() {
            Some("read") => read = read.saturating_add(value),
            Some("write") => written = written.saturating_add(value),
            _ => {}
        }
    }
    (read, written)
}

async fn sample_container_stats_twice(
    docker: &Docker,
    container_id: &str,
) -> Result<
    (
        bollard::models::ContainerStatsResponse,
        bollard::models::ContainerStatsResponse,
    ),
    bollard::errors::Error,
> {
    use bollard::query_parameters::StatsOptions;

    let opts = StatsOptions {
        stream: false,
        one_shot: true,
    };

    let mut first_stream = docker.stats(container_id, Some(opts.clone()));
    let first = first_stream
        .try_next()
        .await?
        .ok_or_else(|| bollard::errors::Error::IOError {
            err: std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "first stats sample produced no frames",
            ),
        })?;
    drop(first_stream);

    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    let mut second_stream = docker.stats(container_id, Some(opts));
    let second =
        second_stream
            .try_next()
            .await?
            .ok_or_else(|| bollard::errors::Error::IOError {
                err: std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "second stats sample produced no frames",
                ),
            })?;

    Ok((first, second))
}

/// Compute the Docker-CLI-equivalent CPU percent from two consecutive stats
/// samples.
///
/// Formula (matches `docker stats`):
/// ```text
/// cpu_delta    = current.total_usage     - previous.total_usage
/// system_delta = current.system_cpu_usage - previous.system_cpu_usage
/// percent      = (cpu_delta / system_delta) * online_cpus * 100
/// ```
///
/// Returns `None` when counters are missing or the delta is zero/negative
/// (container just started, stopped, or clock skew). A fully pinned 4-core
/// container correctly reads as 400% — we deliberately do NOT clamp to 100,
/// because the UI shows "CPU x.x% / N cores" and capping the numerator hides
/// real over-saturation.
fn cpu_percent_from_samples(
    current: &bollard::models::ContainerStatsResponse,
    previous: &bollard::models::ContainerStatsResponse,
) -> Option<f64> {
    let cur_cpu = current.cpu_stats.as_ref()?;
    let prev_cpu = previous.cpu_stats.as_ref()?;

    let cur_total = cur_cpu.cpu_usage.as_ref()?.total_usage? as i128;
    let prev_total = prev_cpu.cpu_usage.as_ref()?.total_usage? as i128;
    let cur_system = cur_cpu.system_cpu_usage? as i128;
    let prev_system = prev_cpu.system_cpu_usage? as i128;

    let cpu_delta = cur_total - prev_total;
    let system_delta = cur_system - prev_system;

    if cpu_delta <= 0 || system_delta <= 0 {
        return None;
    }

    let cpus = cur_cpu.online_cpus.unwrap_or(1).max(1) as f64;
    let percent = (cpu_delta as f64 / system_delta as f64) * cpus * 100.0;
    if percent.is_finite() && percent >= 0.0 {
        Some(percent)
    } else {
        None
    }
}

/// Subtract reclaimable page cache from raw memory usage so the number
/// matches `docker stats`'s "MEM USAGE" column.
///
/// Docker reports `usage` straight from cgroups, which includes the page
/// cache. A Postgres container with an 8 GB working set + 8 GB of file
/// cache reads back as `usage == limit` on a 16 GB cap even though only
/// half of that is real RSS. The Docker CLI compensates by subtracting:
/// - cgroup v2: `stats.inactive_file`
/// - cgroup v1: `stats.cache`
///
/// We prefer `inactive_file` (cgroup v2 is the modern default) and fall
/// back to `cache`. If neither key is present, return the raw usage —
/// better to slightly over-report than to crash on a missing field.
fn memory_usage_excluding_cache(mem: &bollard::models::ContainerMemoryStats) -> Option<u64> {
    let raw_usage = mem.usage?;
    let cache = mem
        .stats
        .as_ref()
        .and_then(|s| s.get("inactive_file").or_else(|| s.get("cache")).copied());
    match cache {
        Some(c) if c <= raw_usage => Some(raw_usage - c),
        _ => Some(raw_usage),
    }
}

impl DockerRuntime {
    /// Per-container directory that holds plaintext secret files for bind-mounting
    /// into `/run/secrets`. Lives outside `TempDir` because Docker keeps the
    /// directory open for the container lifetime; cleanup happens in
    /// `remove_container`.
    ///
    /// Lives under `secrets_root` (defaults to `$TEMPS_DATA_DIR/secrets`)
    /// rather than `/tmp` so the bind mount survives a host reboot —
    /// `/tmp` is tmpfs on most Linux distros and gets wiped, leaving
    /// containers with empty `/run/secrets` until the next redeploy.
    fn secrets_host_dir(&self, container_name: &str) -> std::io::Result<PathBuf> {
        validate_secret_dir_name(container_name)?;
        Ok(self.secrets_root.join(container_name))
    }

    /// Resolves the numeric (uid, gid) that the container will run as,
    /// by inspecting the image's `Config.User`. Used to `chown` the
    /// bind-mounted secrets directory so the app can read its secret
    /// files even when the image runs as a non-root user (e.g. distroless
    /// nonroot = 65532:65532).
    ///
    /// Falls back to `(0, 0)` — matching Docker's default when no USER is
    /// set — on inspect failure or when the USER is a named user we can't
    /// resolve without reading `/etc/passwd` from inside the image. Named
    /// users are logged as a warning so it's obvious why secrets might be
    /// unreadable for images like `node:alpine` (USER=node).
    async fn resolve_image_user(&self, image_name: &str) -> (u32, u32) {
        let Some(docker) = self.docker.cloned() else {
            // No local daemon — control-plane profile. Secret ownership
            // defaults to root; the actual deploy runs on a worker node.
            return (0, 0);
        };
        image_owner(&docker, image_name).await
    }

    async fn cleanup_created_container_after_error(
        &self,
        container_id: &str,
        error: DeployerError,
    ) -> DeployerError {
        if let Err(cleanup_error) = self.remove_container(container_id).await {
            tracing::warn!(
                container = %container_id,
                error = %cleanup_error,
                "failed to remove container after deploy error"
            );
            // Self-hosted operators have no support channel: a stale container
            // squatting the name will break every retry, so say so in the
            // error they actually see instead of only in the server log.
            return DeployerError::DeploymentFailed(format!(
                "{} (cleanup also failed: container {} could not be removed and \
                 may need `docker rm -f {}` before retrying)",
                error, container_id, container_id
            ));
        }
        error
    }

    fn map_container_status(status: &str) -> ContainerStatus {
        match status {
            "created" => ContainerStatus::Created,
            "running" => ContainerStatus::Running,
            "paused" => ContainerStatus::Paused,
            "restarting" => ContainerStatus::Running,
            "removing" => ContainerStatus::Stopped,
            "exited" => ContainerStatus::Exited,
            "dead" => ContainerStatus::Dead,
            _ => ContainerStatus::Stopped,
        }
    }

    fn workload_restart_policy(policy: &crate::RestartPolicy) -> temps_workload::RestartPolicy {
        match policy {
            crate::RestartPolicy::Never => temps_workload::RestartPolicy::Never,
            crate::RestartPolicy::Always => temps_workload::RestartPolicy::Always,
            crate::RestartPolicy::OnFailure => temps_workload::RestartPolicy::OnFailure,
            crate::RestartPolicy::UnlessStopped => temps_workload::RestartPolicy::UnlessStopped,
        }
    }

    /// Find a container by its name
    /// Returns the container ID if found, or None if not found
    pub(super) async fn find_container_by_name(
        &self,
        container_name: &str,
    ) -> Result<Option<String>, DeployerError> {
        use std::collections::HashMap;

        let mut filters = HashMap::new();
        filters.insert("name".to_string(), vec![container_name.to_string()]);

        let options = Some(ListContainersOptions {
            all: true, // Include stopped containers
            filters: Some(filters),
            ..Default::default()
        });

        let containers = self
            .require_docker()?
            .list_containers(options)
            .await
            .map_err(|e| DeployerError::Other(format!("Failed to list containers: {}", e)))?;

        // Docker prefixes container names with "/", so we need to match both formats
        for container in containers {
            if let Some(ref names) = container.names {
                for name in names {
                    // Remove the leading "/" that Docker adds
                    let clean_name = name.trim_start_matches('/');
                    if clean_name == container_name {
                        return Ok(container.id.clone());
                    }
                }
            }
        }

        Ok(None)
    }
}

#[async_trait]
impl ContainerDeployer for DockerRuntime {
    async fn exec_in_container(
        &self,
        container_id: &str,
        command: Vec<String>,
        timeout: std::time::Duration,
    ) -> Result<crate::ExecResult, DeployerError> {
        let docker = self.require_docker()?;
        let failure = |reason: String| DeployerError::ExecFailed {
            container_id: container_id.to_string(),
            reason,
        };
        if command.is_empty() || command[0].is_empty() || timeout.is_zero() {
            return Err(failure(
                "command and positive timeout are required".to_string(),
            ));
        }
        let operation = async {
            let exec = docker
                .create_exec(
                    container_id,
                    bollard::models::ExecConfig {
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        cmd: Some(command),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| failure(e.to_string()))?;
            // Creating an exec does not start it. Pin Docker's canonical container
            // identity before the first start await, so cleanup cannot stop a
            // replacement that later acquires the same operator-facing name.
            let prepared = docker
                .inspect_exec(&exec.id)
                .await
                .map_err(|error| failure(error.to_string()))?;
            let pinned_id = prepared
                .container_id
                .filter(|id| {
                    id.len() == 64
                        && id
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                })
                .ok_or_else(|| {
                    failure("probe exec did not report a canonical container ID".into())
                })?;
            let mut cleanup = ProbeExecCleanup::new(docker.clone(), pinned_id);
            let bollard::exec::StartExecResults::Attached { mut output, .. } = docker
                .start_exec(&exec.id, None::<bollard::exec::StartExecOptions>)
                .await
                .map_err(|e| failure(e.to_string()))?
            else {
                return Err(failure("probe exec unexpectedly detached".to_string()));
            };
            let mut captured = Vec::with_capacity(crate::MAX_EXEC_OUTPUT_BYTES);
            let mut truncated = false;
            while let Some(frame) = output.next().await {
                let frame = frame.map_err(|e| failure(e.to_string()))?;
                truncated |= crate::append_exec_output(&mut captured, frame.as_ref());
            }
            let inspect = docker
                .inspect_exec(&exec.id)
                .await
                .map_err(|e| failure(e.to_string()))?;
            if inspect.running != Some(false) {
                return Err(failure(
                    "command completion could not be confirmed".to_string(),
                ));
            }
            // Positive stopped state is the only proof that no command remains.
            cleanup.disarm();
            let exit_code = inspect
                .exit_code
                .ok_or_else(|| failure("completed command has no exit code".to_string()))?;
            Ok(crate::ExecResult {
                exit_code,
                output: crate::exec_output_string(&captured),
                truncated,
            })
        };
        match tokio::time::timeout(timeout, operation).await {
            Ok(result) => result,
            Err(_) => {
                // Dropping the armed guard also handles an outer deadline, task
                // cancellation, a broken output stream or ambiguous inspection.
                Err(DeployerError::ExecTimeout {
                    container_id: container_id.to_string(),
                    timeout_secs: timeout.as_secs(),
                })
            }
        }
    }

    async fn deploy_container(
        &self,
        request: DeployRequest,
    ) -> Result<DeployResult, DeployerError> {
        info!(
            "Deploying container {} from image {}",
            request.container_name, request.image_name
        );

        self.ensure_network_exists().await?;

        // Check if a container with this name already exists and remove it
        match self.find_container_by_name(&request.container_name).await {
            Ok(Some(existing_container_id)) => {
                info!(
                    "🔄 Container {} already exists ({}), removing it before redeployment",
                    request.container_name, existing_container_id
                );

                // Stop the container if it's running
                if let Err(e) = self.stop_container(&existing_container_id).await {
                    warn!(
                        "⚠️  Failed to stop existing container {}: {}",
                        existing_container_id, e
                    );
                }

                // Remove the container
                if let Err(e) = self.remove_container(&existing_container_id).await {
                    return Err(DeployerError::DeploymentFailed(format!(
                        "Failed to remove existing container {}: {}",
                        existing_container_id, e
                    )));
                }

                info!("✅ Removed existing container {}", existing_container_id);
            }
            Ok(None) => {
                debug!("No existing container with name {}", request.container_name);
            }
            Err(e) => {
                warn!("⚠️  Error checking for existing container: {}", e);
            }
        }

        // ADR-055 phase 1: the request is built by the Docker driver from an
        // instance spec. The golden captures in
        // `tests/golden_container_create.rs` pin it to the request this
        // function built by hand before.
        let ports: Vec<temps_workload::PortBinding> = request
            .port_mappings
            .iter()
            .map(|port_mapping| {
                let binding = self.host_port_binding(port_mapping);
                temps_workload::PortBinding {
                    container_port: port_mapping.container_port,
                    protocol: match port_mapping.protocol {
                        crate::Protocol::Tcp => temps_workload::PortProtocol::Tcp,
                        crate::Protocol::Udp => temps_workload::PortProtocol::Udp,
                    },
                    host_ip: binding.host_ip.unwrap_or_default(),
                    host_port: binding.host_port.and_then(|p| p.parse().ok()),
                }
            })
            .collect();

        // When secrets are present, materialize them as files in a per-container
        // host directory (mode 0700) and bind-mount that directory into the
        // container at /run/secrets (read-only). This matches how Docker Swarm
        // delivers secrets: directory is created by the mount itself, so it
        // works with any image (no requirement that /run/secrets pre-exist),
        // and files are visible from container start (no race with start).
        //
        // Trade-off vs tmpfs: plaintext lives on the host filesystem under
        // `secrets_root` ($TEMPS_DATA_DIR/secrets by default) until the
        // container is removed. We deliberately use a persistent path
        // rather than `std::env::temp_dir()` so the bind mount survives a
        // host reboot — `/tmp` is tmpfs on most Linux distros, and the
        // mount would otherwise point at an empty directory after a
        // restart, forcing a redeploy. The directory is mode 0700
        // root-owned; individual files are mode 0400. Cleanup is handled
        // in `remove_container`.
        let secrets_bind = if request.secrets.is_empty() {
            None
        } else {
            let host_dir = self
                .secrets_host_dir(&request.container_name)
                .map_err(|e| DeployerError::SecretMountFailed {
                    container_name: request.container_name.clone(),
                    reason: format!("derive host dir: {}", e),
                })?;
            // Resolve the image's USER so we can chown the secret files to
            // the uid that the container will actually run as — otherwise
            // mode-0400 root-owned files are unreadable by nonroot images.
            let owner = self.resolve_image_user(&request.image_name).await;
            write_secrets_to_host_dir(&host_dir, &request.secrets, Some(owner)).map_err(|e| {
                DeployerError::SecretMountFailed {
                    container_name: request.container_name.clone(),
                    reason: format!("write host dir {}: {}", host_dir.display(), e),
                }
            })?;
            // Docker bind-mount syntax: "<host_path>:<container_path>:<options>"
            Some(format!("{}:/run/secrets:ro", host_dir.display()))
        };

        let dns_for_container = self.dns_for_container();

        // ADR 045: this host's own grant decides, not the caller. The control
        // plane only says "this is project X"; the answer comes from the
        // environment of the process creating the container.
        let docker_socket_bind = docker_socket_bind_for(
            &self.docker_socket_grant,
            request.project_slug.as_deref(),
            request.control_plane_grants_socket,
        );
        if docker_socket_bind.is_some() {
            // Carries a stable `event` field so host-side log shipping can
            // select these lines without pattern-matching prose. The control
            // plane's audit record for the same mount is written from this
            // host's *self-report* and is therefore not tamper-evident against
            // a compromise of this host; this line is the independent,
            // host-local record of the same fact (ADR 045).
            warn!(
                event = "docker_socket_mounted",
                container_name = %request.container_name,
                project_slug = request.project_slug.as_deref().unwrap_or("<unknown>"),
                "Mounting the host Docker socket into this container: the project is named in \
                 {}. It is root-equivalent on this host.",
                temps_core::docker_socket_grant::DOCKER_SOCKET_PROJECTS_ENV
            );
        }

        // A limit of 0 is the explicit "uncapped" sentinel: leave Docker's
        // memory cap unset so the container runs unlimited. Swap is capped at
        // the memory limit so the advertised hard cap is real (Docker would
        // otherwise let a "512 MB" app reach ~1 GiB of memory+swap).
        let memory_bytes = request
            .resource_limits
            .memory_limit_mb
            .filter(|&mb| mb > 0)
            .map(|mb| mb as i64 * 1024 * 1024);
        let spec = temps_workload::InstanceSpec {
            owner: temps_workload::WorkloadRef::new(temps_workload::WorkloadKind::Deployment, 0),
            name: request.container_name.clone(),
            image: request.image_name.clone(),
            platform: None,
            env: request
                .environment_vars
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            // Delivered through `secrets_bind`, prepared above.
            secret_files: Vec::new(),
            command: request.command.clone(),
            entrypoint: request.entrypoint.clone(),
            workdir: None,
            user: None,
            ports,
            restart: Self::workload_restart_policy(&request.restart_policy),
            limits: temps_workload::InstanceLimits {
                nano_cpus: request
                    .resource_limits
                    .cpu_limit
                    .map(|cores| (cores * 1_000_000_000.0) as i64),
                memory_bytes,
                memory_swap_bytes: memory_bytes,
                pids: None,
                disk_mb: None,
                ..temps_workload::InstanceLimits::default()
            },
            log_rotation: request
                .log_config
                .as_ref()
                .map(|lc| temps_workload::LogRotation {
                    max_size: lc.max_size.clone(),
                    max_file: lc.max_file,
                }),
            // Used by the log aggregator for container discovery.
            labels: request
                .labels
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            security: if docker_socket_bind.is_some() {
                temps_workload::SecurityProfile::HostPrivileged
            } else {
                temps_workload::SecurityProfile::Trusted
            },
        };
        let attachments = temps_workload::Attachments {
            network: temps_workload::NetworkAttachment {
                primary: Some(self.network_name.clone()),
                dns: dns_for_container,
                ..Default::default()
            },
            volumes: Vec::new(),
        };
        // Secrets first, then the socket: binds are collected, never
        // assigned, so neither can drop the other.
        let binds: Vec<String> = secrets_bind
            .into_iter()
            .chain(docker_socket_bind.map(str::to_string))
            .collect();

        // Create container — require the daemon here (as late as possible).
        // If Docker is unavailable, ensure_network_exists() above already
        // returned DockerUnavailable; this call is the guard for paths that
        // skip network creation (e.g. when it already exists).
        let docker = self.require_docker()?;
        let container = temps_workload_docker::DockerDriver::new(docker.as_ref().clone())
            .create_with_binds(&spec, &attachments, binds)
            .await
            .map_err(|e| {
                let reason = match e {
                    temps_workload::DriverError::Runtime { reason, .. } => reason,
                    other => other.to_string(),
                };
                DeployerError::DeploymentFailed(format!("Failed to create container: {}", reason))
            })?;

        // Write the host-owned receipt before the candidate can be assigned a
        // runtime probe. Failure must not leave an untracked application.
        if let Err(error) = crate::runtime_health::record_creation(
            &docker,
            &crate::runtime_health::data_root(),
            &container.id,
            &request,
        )
        .await
        {
            return Err(self
                .cleanup_created_container_after_error(
                    &container.id,
                    DeployerError::DeploymentFailed(format!(
                        "Cannot persist runtime health ownership receipt: {error}"
                    )),
                )
                .await);
        }

        // Multi-host overlay: best-effort additional attach to `temps-overlay`
        // (or whatever the operator configured). Containers always boot with
        // their primary network interface (`temps-app-network`); the overlay
        // attachment is purely additive and silently no-ops when the overlay
        // network isn't present yet on this node.
        if let Err(e) = self.maybe_attach_overlay(&container.id).await {
            return Err(self
                .cleanup_created_container_after_error(&container.id, e)
                .await);
        }

        if let Err(e) = self
            .attach_required_networks(&container.id, &request.extra_networks)
            .await
        {
            return Err(self
                .cleanup_created_container_after_error(&container.id, e)
                .await);
        }

        // Start container
        if let Err(e) = docker
            .start_container(&container.id, None::<StartContainerOptions>)
            .await
            .map_err(|e| {
                DeployerError::DeploymentFailed(format!("Failed to start container: {}", e))
            })
        {
            return Err(self
                .cleanup_created_container_after_error(&container.id, e)
                .await);
        }

        // Install overlay peer routes inside the container's netns.
        // Must run *after* start_container — `docker inspect` only
        // reports a non-zero PID for a running container, and route
        // injection uses `nsenter -t <pid> -n ip route ...`.
        self.install_overlay_peer_routes(&container.id).await;

        // Get the first port mapping for the result
        let (container_port, requested_host_port) = request
            .port_mappings
            .first()
            .map(|pm| (pm.container_port, pm.host_port))
            .unwrap_or((0, 0));

        // When host_port was 0 (Docker picks), inspect the container to get the actual port
        let host_port = if requested_host_port == 0 && container_port > 0 {
            let inspect = match docker
                .inspect_container(&container.id, None::<InspectContainerOptions>)
                .await
                .map_err(|e| {
                    DeployerError::DeploymentFailed(format!(
                        "Failed to inspect container {} for port mapping: {}",
                        container.id, e
                    ))
                }) {
                Ok(inspect) => inspect,
                Err(e) => {
                    return Err(self
                        .cleanup_created_container_after_error(&container.id, e)
                        .await);
                }
            };

            let port_key = format!("{}/tcp", container_port);
            match inspect
                .network_settings
                .and_then(|ns| ns.ports)
                .and_then(|ports| ports.get(&port_key).cloned())
                .flatten()
                .and_then(|bindings| bindings.first().cloned())
                .and_then(|binding| binding.host_port)
                .and_then(|p| p.parse::<u16>().ok())
                .ok_or_else(|| {
                    DeployerError::DeploymentFailed(format!(
                        "Container {} has no host port binding for {}",
                        container.id, port_key
                    ))
                }) {
                Ok(host_port) => host_port,
                Err(e) => {
                    return Err(self
                        .cleanup_created_container_after_error(&container.id, e)
                        .await);
                }
            }
        } else {
            requested_host_port
        };

        Ok(DeployResult {
            container_id: container.id,
            container_name: request.container_name,
            container_port,
            host_port,
            status: ContainerStatus::Running,
            docker_socket_mounted: docker_socket_bind.is_some(),
        })
    }

    async fn start_container(&self, container_id: &str) -> Result<(), DeployerError> {
        self.require_docker()?
            .start_container(container_id, None::<StartContainerOptions>)
            .await
            .map_err(|e| DeployerError::Other(format!("Failed to start container: {}", e)))?;
        Ok(())
    }

    async fn stop_container(&self, container_id: &str) -> Result<(), DeployerError> {
        self.require_docker()?
            .stop_container(
                container_id,
                Some(StopContainerOptions {
                    t: Some(10),
                    signal: None,
                }),
            )
            .await
            .map_err(|e| {
                DeployerError::Other(format!("Failed to stop container {}: {}", container_id, e))
            })?;
        Ok(())
    }

    async fn pause_container(&self, container_id: &str) -> Result<(), DeployerError> {
        self.require_docker()?
            .pause_container(container_id)
            .await
            .map_err(|e| DeployerError::Other(format!("Failed to pause container: {}", e)))?;
        Ok(())
    }

    async fn resume_container(&self, container_id: &str) -> Result<(), DeployerError> {
        self.require_docker()?
            .unpause_container(container_id)
            .await
            .map_err(|e| DeployerError::Other(format!("Failed to resume container: {}", e)))?;
        Ok(())
    }

    async fn remove_container(&self, container_id: &str) -> Result<(), DeployerError> {
        let docker = self.require_docker()?;

        // Look up the container name before removal so we can clean up its
        // per-container secrets host directory (if any). Inspect failures are
        // non-fatal: we still try to remove the container.
        let inspected = docker
            .inspect_container(container_id, None::<InspectContainerOptions>)
            .await
            .ok();
        let immutable_id = inspected
            .as_ref()
            .and_then(|container| container.id.clone())
            .unwrap_or_else(|| container_id.to_string());
        let container_name = inspected
            .and_then(|container| container.name)
            .map(|name| name.trim_start_matches('/').to_string());

        let removal_result = docker
            .remove_container(
                container_id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|error| match error {
                bollard::errors::Error::DockerResponseServerError {
                    status_code: 404,
                    message,
                } => DeployerError::ContainerNotFound(message),
                other => DeployerError::Other(format!("Failed to remove container: {other}")),
            });

        let should_cleanup_secrets = matches!(
            &removal_result,
            Ok(()) | Err(DeployerError::ContainerNotFound(_))
        );
        if should_cleanup_secrets {
            crate::runtime_health::forget_creation(
                &crate::runtime_health::data_root(),
                &immutable_id,
            );
            if let Some(name) = container_name {
                match self.secrets_host_dir(&name) {
                    Ok(dir) if dir.exists() => {
                        if let Err(e) = std::fs::remove_dir_all(&dir) {
                            warn!(
                                "Failed to clean up secrets host dir {}: {}",
                                dir.display(),
                                e
                            );
                        }
                    }
                    Ok(_) => {}
                    Err(error) => warn!(
                        container_name = %name,
                        %error,
                        "Skipping secrets host dir cleanup for invalid container name"
                    ),
                }
            }
        }

        removal_result?;

        Ok(())
    }

    async fn get_container_info(&self, container_id: &str) -> Result<ContainerInfo, DeployerError> {
        let container = self
            .require_docker()?
            .inspect_container(container_id, None::<InspectContainerOptions>)
            .await
            .map_err(|error| inspect_error(container_id, error))?;

        // Pull host_config out before state/config so we can read the CPU
        // limit (nano_cpus) without re-borrowing the moved container value.
        let host_config = container.host_config.clone();
        let state = container.state.unwrap_or_default();
        let config = container.config.unwrap_or_default();
        let container_labels = config.labels.clone().unwrap_or_default();

        // Parse environment variables
        let env_vars = config
            .env
            .unwrap_or_default()
            .into_iter()
            .filter_map(|env_str| {
                let parts: Vec<&str> = env_str.splitn(2, '=').collect();
                if parts.len() == 2 {
                    Some((parts[0].to_string(), parts[1].to_string()))
                } else {
                    None
                }
            })
            .collect();

        // Parse port mappings
        let port_mappings = container
            .network_settings
            .and_then(|ns| ns.ports)
            .map(parse_inspected_port_mappings)
            .unwrap_or_default();

        let status =
            Self::map_container_status(&state.status.map(|s| s.to_string()).unwrap_or_default());

        // Capture exit metadata so the API/UI can show *why* a container is
        // gone instead of a bare "Exited". Docker only populates these once
        // the container has actually exited; for running containers they
        // remain None.
        let oom_killed = state.oom_killed;
        let exit_code_i64 = state.exit_code;
        let exit_code: Option<i32> = exit_code_i64.and_then(|c| i32::try_from(c).ok());
        // Trim Docker's empty-string "no error" sentinel; only surface a real message.
        let error_message = state.error.and_then(|e| {
            let trimmed = e.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });
        // FinishedAt comes back as RFC3339; "0001-01-01T00:00:00Z" means
        // never-finished, which we treat as None.
        let parse_docker_ts = |s: &str| -> Option<chrono::DateTime<chrono::Utc>> {
            if s.is_empty() || s.starts_with("0001-01-01") {
                None
            } else {
                chrono::DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|dt| dt.with_timezone(&chrono::Utc))
            }
        };
        let finished_at = state.finished_at.as_deref().and_then(parse_docker_ts);
        let started_at = state.started_at.as_deref().and_then(parse_docker_ts);

        // Translate Docker's nano_cpus to whole-core units so the UI can
        // render "0.5 / 1.0 cores" without doing the divide. None when no
        // limit was set on the container (host_config absent or nano_cpus 0).
        let cpu_limit_cores = host_config
            .as_ref()
            .and_then(|hc| hc.nano_cpus)
            .filter(|nc| *nc > 0)
            .map(|nc| nc as f64 / 1_000_000_000.0);

        let exit_reason =
            build_exit_reason(&status, oom_killed, exit_code_i64, error_message.as_deref());

        Ok(ContainerInfo {
            container_id: container.id.unwrap_or_default(),
            container_name: container
                .name
                .unwrap_or_default()
                .trim_start_matches('/')
                .to_string(),
            image_name: config.image.unwrap_or_default(),
            status,
            created_at: container
                .created
                .and_then(|dt| {
                    chrono::DateTime::from_timestamp(dt.unix_timestamp(), dt.nanosecond())
                })
                .unwrap_or_else(chrono::Utc::now),
            ports: port_mappings,
            environment_vars: env_vars,
            restart_count: container.restart_count,
            labels: container_labels,
            exit_code,
            exit_reason,
            oom_killed,
            error_message,
            finished_at,
            started_at,
            cpu_limit_cores,
        })
    }

    async fn get_container_stats(
        &self,
        container_id: &str,
    ) -> Result<crate::ContainerStats, DeployerError> {
        // Get container info first to get the name and cpu_limit
        let container_info = self.get_container_info(container_id).await?;

        // Sample twice ~1s apart so CPU% has a real delta window — Docker's
        // `one_shot: true` doesn't populate `precpu_stats`, so the
        // single-sample math collapses to (cumulative cpu time / system time
        // since boot) which is ~0% for any long-running container. This is
        // the same pattern `docker stats` uses internally.
        let docker = self.require_docker()?;
        let (first, second) = sample_container_stats_twice(&docker, container_id)
            .await
            .map_err(|e| DeployerError::Other(format!("Failed to get container stats: {}", e)))?;

        let cpu_percent = cpu_percent_from_samples(&second, &first).unwrap_or(0.0);

        // Host core count at sample time. For a container with no CPU limit
        // this is its real ceiling — `cpu_utilization_percent()` needs it to
        // avoid treating one saturated core on a multi-core host as 100%.
        let online_cpus = second
            .cpu_stats
            .as_ref()
            .and_then(|cpu| cpu.online_cpus)
            .filter(|cpus| *cpus > 0);

        // Memory: subtract page cache (matches `docker stats` MEM USAGE).
        // cgroup v2 → `inactive_file`, cgroup v1 → `cache`. Both are
        // reclaimable file pages that the kernel counts as `usage` but
        // shouldn't show up as the container's working set.
        let memory_stats = second.memory_stats.as_ref();
        let memory_bytes = memory_stats
            .and_then(memory_usage_excluding_cache)
            .unwrap_or(0);
        let memory_limit_bytes = memory_stats.and_then(|ms| ms.limit);

        let memory_percent = match memory_limit_bytes {
            Some(limit) if limit > 0 => {
                Some(((memory_bytes as f64 / limit as f64) * 100.0).clamp(0.0, 100.0))
            }
            _ => None,
        };

        // Extract network stats from the latest sample.
        let default_networks = Default::default();
        let networks_stats = second.networks.as_ref().unwrap_or(&default_networks);
        let (network_rx_bytes, network_tx_bytes) =
            if let Some(net_stat) = networks_stats.values().next() {
                (
                    net_stat.rx_bytes.unwrap_or(0),
                    net_stat.tx_bytes.unwrap_or(0),
                )
            } else {
                (0, 0)
            };

        let (block_read_bytes, block_write_bytes) = second
            .blkio_stats
            .as_ref()
            .and_then(|blkio| blkio.io_service_bytes_recursive.as_deref())
            .map(block_io_bytes)
            .unzip();

        Ok(crate::ContainerStats {
            container_id: container_info.container_id,
            container_name: container_info.container_name,
            cpu_percent,
            cpu_limit_cores: container_info.cpu_limit_cores,
            online_cpus,
            memory_bytes,
            memory_limit_bytes,
            memory_percent,
            network_rx_bytes,
            network_tx_bytes,
            block_read_bytes,
            block_write_bytes,
            restart_count: container_info.restart_count,
            started_at: container_info.started_at,
            timestamp: chrono::Utc::now(),
        })
    }

    async fn list_containers(&self) -> Result<Vec<ContainerInfo>, DeployerError> {
        let containers = self
            .require_docker()?
            .list_containers(Some(ListContainersOptions {
                all: true,
                ..Default::default()
            }))
            .await
            .map_err(|e| DeployerError::Other(format!("Failed to list containers: {}", e)))?;

        let mut container_infos = Vec::new();

        for container in containers {
            if let Some(id) = container.id {
                match self.get_container_info(&id).await {
                    Ok(info) => container_infos.push(info),
                    Err(e) => warn!("Failed to get info for container {}: {}", id, e),
                }
            }
        }

        Ok(container_infos)
    }

    async fn get_container_logs(&self, container_id: &str) -> Result<String, DeployerError> {
        let docker = self.require_docker()?;
        let mut logs_stream = docker.logs(
            container_id,
            Some(LogsOptions {
                stdout: true,
                stderr: true,
                tail: "10000".to_string(),
                ..Default::default()
            }),
        );
        let mut logs = DockerLogTail::new(MAX_CONTAINER_LOG_BYTES);

        while let Some(chunk) = logs_stream.try_next().await.map_err(|error| {
            DeployerError::Other(format!(
                "Failed to read logs for container '{container_id}': {error}"
            ))
        })? {
            logs.push(chunk.into_bytes());
        }

        Ok(logs.into_string())
    }

    async fn stream_container_logs(
        &self,
        container_id: &str,
    ) -> Result<Box<dyn futures::Stream<Item = String> + Unpin + Send>, DeployerError> {
        let docker = self.require_docker()?;
        let logs_stream = docker
            .logs(
                container_id,
                Some(LogsOptions {
                    stdout: true,
                    stderr: true,
                    follow: true,
                    ..Default::default()
                }),
            )
            .map(|chunk| match chunk {
                Ok(c) => String::from_utf8_lossy(&c.into_bytes()).to_string(),
                Err(e) => format!("Error reading logs: {}", e),
            });

        Ok(Box::new(Box::pin(logs_stream)))
    }

    async fn image_exists(&self, image_name: &str) -> Result<bool, DeployerError> {
        match self.require_docker()?.inspect_image(image_name).await {
            Ok(_) => Ok(true),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(false),
            Err(e) => Err(DeployerError::Other(format!(
                "Failed to check image '{}': {}",
                image_name, e
            ))),
        }
    }
}

#[cfg(test)]
mod tests;
