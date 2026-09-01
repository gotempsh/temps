// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP handlers for external service operations on worker nodes.
//!
//! These endpoints allow the control plane to manage external services
//! (PostgreSQL, Redis, MongoDB, S3) on any node in the cluster.

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use bollard::query_parameters::{
    InspectContainerOptions, ListContainersOptions, RemoveContainerOptions, StartContainerOptions,
    StopContainerOptions,
};
use std::collections::HashMap;
use std::sync::Arc;

use crate::exec_timeout::{
    completed_exec_exit_code, exec_start_was_rejected, monitor_exec_until_stopped,
    resolve_container_id, resolve_exec_container_id, run_exec_with_deadline, ExecCleanupGuard,
    ExecCompletionError, ExecDeadlineOutcome,
};
use crate::handlers::{
    captured_error_response, captured_ok_response, container_identity_error_status,
    try_acquire_attached_exec_permits, try_acquire_exec_lifecycle_permits, AgentResourceLimits,
    AgentResponse, AgentState, ExecAdmissionError,
};
use crate::output_buffer::{BoundedTailBuffer, MAX_CAPTURED_STREAM_BYTES};
use crate::{
    ServiceBackupRequest, ServiceBackupResponse, ServiceCreateRequest, ServiceCreateResponse,
    ServiceExecRequest, ServiceExecResponse, ServiceRestoreRequest, ServiceStatus,
};
use temps_providers::remote_service_client::{
    RemoteHealthProbeRequest, RemoteHealthProbeResponse, RemoteRuntimeEnvRequest,
    RemoteRuntimeEnvResponse,
};

const SERVICE_EXEC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5 * 60);
const SERVICE_DATA_OPERATION_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(2 * 60 * 60);
const DATA_OPERATION_STDERR_CAPTURE_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy)]
struct ExecCaptureLimits {
    stdout: usize,
    stderr: usize,
}

const SERVICE_EXEC_CAPTURE_LIMITS: ExecCaptureLimits = ExecCaptureLimits {
    stdout: MAX_CAPTURED_STREAM_BYTES,
    stderr: MAX_CAPTURED_STREAM_BYTES,
};
const DATA_OPERATION_CAPTURE_LIMITS: ExecCaptureLimits = ExecCaptureLimits {
    stdout: 0,
    stderr: DATA_OPERATION_STDERR_CAPTURE_BYTES,
};
const POSTGRES_DUMP_SCRIPT: &str =
    "pg_dumpall --clean --if-exists --no-acl --no-owner -U postgres | gzip > /tmp/backup.sql.gz && echo 'dump_complete'";

fn postgres_dump_command() -> Vec<String> {
    vec![
        "bash".to_string(),
        "-o".to_string(),
        "pipefail".to_string(),
        "-c".to_string(),
        POSTGRES_DUMP_SCRIPT.to_string(),
    ]
}

struct CapturedExecOutput {
    exit_code: i64,
    stdout: String,
    stderr: String,
    received_stdout_bytes: usize,
    received_stderr_bytes: usize,
}

#[derive(Debug, thiserror::Error)]
enum CaptureExecError {
    #[error("Failed to start Docker exec '{exec_id}': {source}")]
    Start {
        exec_id: String,
        source: bollard::errors::Error,
    },
    #[error("Docker exec '{exec_id}' unexpectedly started detached")]
    UnexpectedDetached { exec_id: String },
    #[error("Failed while reading output from Docker exec '{exec_id}': {source}")]
    Stream {
        exec_id: String,
        source: bollard::errors::Error,
    },
    #[error("Failed to inspect completed Docker exec '{exec_id}': {source}")]
    Inspect {
        exec_id: String,
        source: bollard::errors::Error,
    },
    #[error("Docker exec completion could not be confirmed: {source}")]
    Completion {
        #[from]
        source: ExecCompletionError,
    },
}

async fn capture_exec_output(
    docker: &bollard::Docker,
    exec_id: &str,
    limits: ExecCaptureLimits,
) -> Result<CapturedExecOutput, CaptureExecError> {
    use bollard::exec::{StartExecOptions, StartExecResults};
    use futures::StreamExt;

    let mut output = match docker
        .start_exec(exec_id, None::<StartExecOptions>)
        .await
        .map_err(|source| CaptureExecError::Start {
            exec_id: exec_id.to_string(),
            source,
        })? {
        StartExecResults::Attached { output, .. } => output,
        StartExecResults::Detached => {
            return Err(CaptureExecError::UnexpectedDetached {
                exec_id: exec_id.to_string(),
            });
        }
    };

    let mut stdout = BoundedTailBuffer::new(limits.stdout);
    let mut stderr = BoundedTailBuffer::new(limits.stderr);
    let mut received_stdout_bytes = 0usize;
    let mut received_stderr_bytes = 0usize;
    while let Some(chunk) = output.next().await {
        match chunk.map_err(|source| CaptureExecError::Stream {
            exec_id: exec_id.to_string(),
            source,
        })? {
            bollard::container::LogOutput::StdOut { message } => {
                received_stdout_bytes = received_stdout_bytes.saturating_add(message.len());
                stdout.push(message);
            }
            bollard::container::LogOutput::StdErr { message } => {
                received_stderr_bytes = received_stderr_bytes.saturating_add(message.len());
                stderr.push(message);
            }
            _ => {}
        }
    }

    let inspect =
        docker
            .inspect_exec(exec_id)
            .await
            .map_err(|source| CaptureExecError::Inspect {
                exec_id: exec_id.to_string(),
                source,
            })?;
    let exit_code = completed_exec_exit_code(&inspect, exec_id)?;

    Ok(CapturedExecOutput {
        exit_code,
        stdout: stdout.into_string(),
        stderr: stderr.into_string(),
        received_stdout_bytes,
        received_stderr_bytes,
    })
}

fn exec_failure_message(
    operation: &str,
    container_name: &str,
    output: &CapturedExecOutput,
) -> Option<String> {
    if output.exit_code == 0 {
        return None;
    }

    let stderr = output.stderr.trim();
    let context = if stderr.is_empty() {
        String::new()
    } else {
        format!(": {stderr}")
    };
    Some(format!(
        "{operation} command in '{container_name}' exited with status {}{context}",
        output.exit_code
    ))
}

fn error_response(status: StatusCode, message: String) -> impl IntoResponse {
    (
        status,
        Json(AgentResponse::<()> {
            success: false,
            data: None,
            error: Some(message),
        }),
    )
}

fn ok_response<T: serde::Serialize>(data: T) -> Json<AgentResponse<T>> {
    Json(AgentResponse {
        success: true,
        data: Some(data),
        error: None,
    })
}

fn mounted_volume_names(
    container_name: &str,
    mounts: Vec<bollard::models::MountPoint>,
) -> std::collections::HashSet<String> {
    let mut names = mounts
        .into_iter()
        .filter_map(|mount| mount.name)
        .collect::<std::collections::HashSet<_>>();
    names.insert(format!("{}_data", container_name));
    names
}

/// Create and start an external service container on this node.
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services",
    request_body = ServiceCreateRequest,
    responses(
        (status = 200, description = "Service created", body = AgentResponse<ServiceCreateResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Service creation failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_service(
    State(state): State<Arc<AgentState>>,
    Json(request): Json<ServiceCreateRequest>,
) -> impl IntoResponse {
    tracing::info!(
        service = %request.name,
        service_type = %request.service_type,
        image = %request.image,
        "Creating external service container"
    );

    let docker = match state.docker.as_ref() {
        Some(d) => d,
        None => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Docker client not available on this agent".to_string(),
            )
            .into_response();
        }
    };

    let container_name = request.name.clone();

    // Create volumes
    for volume_name in request.volumes.keys() {
        let create_opts = bollard::models::VolumeCreateRequest {
            name: Some(volume_name.clone()),
            ..Default::default()
        };
        if let Err(e) = docker.create_volume(create_opts).await {
            tracing::warn!(
                volume = %volume_name,
                "Volume creation returned error (may already exist): {}",
                e
            );
        }
    }

    // Build port bindings
    let mut port_bindings: HashMap<String, Option<Vec<bollard::models::PortBinding>>> =
        HashMap::new();
    let mut exposed_ports: Vec<String> = Vec::new();
    let mut first_host_port: u16 = 0;
    let mut has_auto_assign = false;

    for pm in &request.port_mappings {
        let container_port_key = format!("{}/tcp", pm.container_port);
        exposed_ports.push(container_port_key.clone());

        if pm.host_port == 0 {
            // Auto-assign: let Docker pick a free host port
            has_auto_assign = true;
            port_bindings.insert(
                container_port_key,
                Some(vec![bollard::models::PortBinding {
                    host_ip: Some("0.0.0.0".to_string()),
                    host_port: None,
                }]),
            );
        } else {
            port_bindings.insert(
                container_port_key,
                Some(vec![bollard::models::PortBinding {
                    host_ip: Some("0.0.0.0".to_string()),
                    host_port: Some(pm.host_port.to_string()),
                }]),
            );
            if first_host_port == 0 {
                first_host_port = pm.host_port;
            }
        }
    }

    // Build volume binds
    let binds: Vec<String> = request
        .volumes
        .iter()
        .map(|(vol, path)| format!("{}:{}", vol, path))
        .collect();

    // Build environment
    let env: Vec<String> = request
        .environment
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect();

    // Wire the per-node Hickory resolver into the container's resolv.conf
    // so it can resolve `*.temps.local` natively (ADR-011). Falls back to
    // Docker's default DNS when the overlay isn't bootstrapped yet
    // (single-host setups). Read from the agent's shared slot — published
    // by `network_sync` once the bridge gateway is up. `dns_with_fallback`
    // appends public resolvers so a crashed/unreachable Hickory resolver
    // never takes down DNS for every managed-service container on the node.
    let dns_servers: Option<Vec<String>> = state
        .overlay_bridge_address
        .read()
        .ok()
        .and_then(|slot| slot.as_ref().map(|ip| vec![ip.to_string()]))
        .map(temps_deployer::docker::dns_with_fallback);
    if let Some(ref dns) = dns_servers {
        tracing::debug!(
            container = %container_name,
            dns = ?dns,
            "Wiring temps DNS into container resolv.conf"
        );
    }

    let mut host_config = bollard::models::HostConfig {
        binds: Some(binds),
        port_bindings: Some(port_bindings),
        network_mode: request.network.clone(),
        dns: dns_servers,
        restart_policy: Some(bollard::models::RestartPolicy {
            name: Some(bollard::models::RestartPolicyNameEnum::UNLESS_STOPPED),
            maximum_retry_count: None,
        }),
        ..Default::default()
    };
    if let Some(ref limits) = request.resource_limits {
        if let Some(mb) = limits.memory_mb {
            host_config.memory = Some(mb.saturating_mul(1024 * 1024));
        }
        if let Some(mb) = limits.memory_swap_mb {
            host_config.memory_swap = Some(mb.saturating_mul(1024 * 1024));
        }
        if let Some(nc) = limits.nano_cpus {
            host_config.nano_cpus = Some(nc);
        }
        if let Some(cs) = limits.cpu_shares {
            host_config.cpu_shares = Some(cs);
        }
        if let Some(mb) = limits.shm_size_mb {
            host_config.shm_size = Some(mb.saturating_mul(1024 * 1024));
        }
    }

    let container_config = bollard::models::ContainerCreateBody {
        image: Some(request.image.clone()),
        env: Some(env),
        exposed_ports: Some(exposed_ports),
        host_config: Some(host_config),
        cmd: request.command.clone(),
        labels: Some(HashMap::from([
            ("sh.temps.managed".to_string(), "true".to_string()),
            ("sh.temps.service".to_string(), "true".to_string()),
            (
                "sh.temps.service.type".to_string(),
                request.service_type.clone(),
            ),
            ("sh.temps.service.name".to_string(), request.name.clone()),
        ])),
        ..Default::default()
    };

    // Pull the image if not already present locally
    {
        use bollard::query_parameters::CreateImageOptions;
        use futures::StreamExt;

        let image_ref = request.image.as_str();
        tracing::info!(image = %image_ref, "Pulling image (if not present)...");

        let mut pull_stream = docker.create_image(
            Some(CreateImageOptions {
                from_image: Some(image_ref.to_string()),
                ..Default::default()
            }),
            None,
            None,
        );

        while let Some(result) = pull_stream.next().await {
            if let Err(e) = result {
                tracing::error!(image = %image_ref, "Failed to pull image: {}", e);
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to pull image '{}': {}", image_ref, e),
                )
                .into_response();
            }
        }
        tracing::info!(image = %image_ref, "Image ready");
    }

    let create_opts = bollard::query_parameters::CreateContainerOptionsBuilder::new()
        .name(&container_name)
        .build();

    match docker
        .create_container(Some(create_opts), container_config)
        .await
    {
        Ok(response) => {
            // Best-effort dual-attach to the multi-host overlay (ADR-011).
            // The container is already on `request.network` (typically
            // temps-app-network) for legacy single-host routing; this also
            // attaches it to the temps-overlay bridge so the container has
            // a routable cross-node IP and can be reached by name from any
            // worker. Skipped silently if the overlay isn't bootstrapped on
            // this host yet (single-host mode).
            if let Err(e) = attach_to_overlay_if_present(docker, &response.id).await {
                tracing::warn!(
                    container = %container_name,
                    error = %e,
                    "Failed to attach service container to overlay; continuing single-host"
                );
            }

            // Start the container
            if let Err(e) = docker
                .start_container(&container_name, None::<StartContainerOptions>)
                .await
            {
                tracing::error!(
                    container = %container_name,
                    "Failed to start service container: {}",
                    e
                );
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Container created but failed to start: {}", e),
                )
                .into_response();
            }

            // Install per-peer overlay routes inside the container's netns
            // *after* it's running. Without these, traffic destined for
            // other workers' overlay /24s falls through the container's
            // default route on the primary network and gets dropped.
            // Best-effort: failures are logged and don't fail the deploy.
            if let Err(e) = install_overlay_peer_routes_after_start(
                docker,
                &container_name,
                &state.overlay_peers,
            )
            .await
            {
                tracing::warn!(
                    container = %container_name,
                    error = %e,
                    "Failed to install overlay peer routes; cross-worker traffic to other CIDRs will fail"
                );
            }

            // Inspect once to (a) discover an auto-assigned host port if needed,
            // and (b) read the temps-overlay IP for the DNS registry (ADR-011).
            // We always inspect now because the overlay IP is independent of the
            // auto-assign port path.
            let mut compute_ip: Option<String> = None;
            match docker
                .inspect_container(&container_name, None::<InspectContainerOptions>)
                .await
            {
                Ok(info) => {
                    if has_auto_assign && first_host_port == 0 {
                        if let Some(network_settings) = &info.network_settings {
                            if let Some(ports) = &network_settings.ports {
                                'find: for bindings in ports.values().flatten() {
                                    for binding in bindings {
                                        if let Some(hp) = &binding.host_port {
                                            if let Ok(port) = hp.parse::<u16>() {
                                                first_host_port = port;
                                                break 'find;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    compute_ip = extract_overlay_ip(&info);
                }
                Err(e) => {
                    tracing::warn!(
                        container = %container_name,
                        "Failed to inspect container after start: {}",
                        e
                    );
                }
            }

            tracing::info!(
                container = %container_name,
                container_id = %response.id,
                host_port = first_host_port,
                compute_ip = ?compute_ip,
                "Service container created and started"
            );

            ok_response(ServiceCreateResponse {
                container_id: response.id,
                container_name,
                host_port: first_host_port,
                compute_ip,
            })
            .into_response()
        }
        Err(e) => {
            tracing::error!(
                container = %container_name,
                "Failed to create service container: {}",
                e
            );
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Failed to create service container '{}': {}",
                    container_name, e
                ),
            )
            .into_response()
        }
    }
}

/// Stop a service container.
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services/{name}/stop",
    params(("name" = String, Path, description = "Service container name")),
    responses(
        (status = 200, description = "Service stopped", body = AgentResponse<String>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Stop failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn stop_service(
    State(state): State<Arc<AgentState>>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    tracing::info!(service = %name, "Stopping service container");

    let docker = match state.docker.as_ref() {
        Some(d) => d,
        None => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Docker client not available".to_string(),
            )
            .into_response();
        }
    };

    match docker
        .stop_container(&name, None::<StopContainerOptions>)
        .await
    {
        Ok(()) => {
            tracing::info!(service = %name, "Service container stopped");
            ok_response("stopped".to_string()).into_response()
        }
        Err(e) => {
            tracing::error!(service = %name, "Failed to stop service: {}", e);
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to stop service '{}': {}", name, e),
            )
            .into_response()
        }
    }
}

/// Start a stopped service container.
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services/{name}/start",
    params(("name" = String, Path, description = "Service container name")),
    responses(
        (status = 200, description = "Service started", body = AgentResponse<String>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Start failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn start_service(
    State(state): State<Arc<AgentState>>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    tracing::info!(service = %name, "Starting service container");

    let docker = match state.docker.as_ref() {
        Some(d) => d,
        None => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Docker client not available".to_string(),
            )
            .into_response();
        }
    };

    match docker
        .start_container(&name, None::<StartContainerOptions>)
        .await
    {
        Ok(()) => {
            tracing::info!(service = %name, "Service container started");
            ok_response("started".to_string()).into_response()
        }
        Err(e) => {
            tracing::error!(service = %name, "Failed to start service: {}", e);
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to start service '{}': {}", name, e),
            )
            .into_response()
        }
    }
}

/// Remove a service container and optionally its volumes.
#[utoipa::path(
    tag = "Services",
    delete,
    path = "/agent/services/{name}",
    params(("name" = String, Path, description = "Service container name")),
    responses(
        (status = 200, description = "Service removed", body = AgentResponse<String>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Remove failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn remove_service(
    State(state): State<Arc<AgentState>>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    tracing::info!(service = %name, "Removing service container");

    let docker = match state.docker.as_ref() {
        Some(d) => d,
        None => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Docker client not available".to_string(),
            )
            .into_response();
        }
    };

    // Capture the actual named volumes before removing the container. Volume
    // names are not always derived from the final canonical container name
    // (notably MongoDB, managed S3, KV, and Blob).
    let mounts = match docker
        .inspect_container(&name, None::<InspectContainerOptions>)
        .await
    {
        Ok(container) => container.mounts.unwrap_or_default(),
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => Vec::new(),
        Err(error) => {
            tracing::error!(
                service = %name,
                "Refusing service removal because mounted volumes could not be inspected: {}",
                error
            );
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Failed to inspect service '{}' before volume-safe removal: {}",
                    name, error
                ),
            )
            .into_response();
        }
    };
    let volume_names = mounted_volume_names(&name, mounts);

    // Stop first if running
    let _ = docker
        .stop_container(&name, None::<StopContainerOptions>)
        .await;

    match docker
        .remove_container(
            &name,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await
    {
        Ok(()) => {
            tracing::info!(service = %name, "Service container removed");
        }
        Err(e) => {
            tracing::error!(service = %name, "Failed to remove service: {}", e);
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to remove service '{}': {}", name, e),
            )
            .into_response();
        }
    }

    // Also remove the named data volume so that re-adding a service at
    // the same container name doesn't inherit stale state. Without this,
    // a deleted-then-re-added pg_auto_failover member silently picks up
    // the previous member's `pg_autoctl.cfg` and masquerades as the old
    // identity, which deadlocks the monitor's view of the cluster.
    //
    // Best-effort: a "volume in use" failure here usually means another
    // container still mounts it (shouldn't happen, but harmless to log
    // and continue).
    for volume_name in volume_names {
        match docker
            .remove_volume(
                &volume_name,
                None::<bollard::query_parameters::RemoveVolumeOptions>,
            )
            .await
        {
            Ok(()) => {
                tracing::info!(volume = %volume_name, "Service data volume removed");
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {
                tracing::debug!(volume = %volume_name, "Service data volume already absent");
            }
            Err(e) => {
                tracing::warn!(
                    volume = %volume_name,
                    "Failed to remove service data volume; cluster may inherit stale state on re-add: {}",
                    e
                );
            }
        }
    }

    ok_response("removed".to_string()).into_response()
}

/// Get service container status.
#[utoipa::path(
    tag = "Services",
    get,
    path = "/agent/services/{name}/status",
    params(("name" = String, Path, description = "Service container name")),
    responses(
        (status = 200, description = "Service status", body = AgentResponse<ServiceStatus>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Status check failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn service_status(
    State(state): State<Arc<AgentState>>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let docker = match state.docker.as_ref() {
        Some(d) => d,
        None => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Docker client not available".to_string(),
            )
            .into_response();
        }
    };

    match docker
        .inspect_container(&name, None::<InspectContainerOptions>)
        .await
    {
        Ok(info) => {
            let state_info = info.state.as_ref();
            let running = state_info.and_then(|s| s.running).unwrap_or(false);
            let health = state_info
                .and_then(|s| s.health.as_ref())
                .and_then(|h| h.status.as_ref())
                .map(|s| format!("{:?}", s));

            let container_id = info.id.clone();

            ok_response(ServiceStatus {
                container_name: name,
                container_id,
                running,
                health,
            })
            .into_response()
        }
        Err(e) => {
            // Container not found = not running
            if e.to_string().contains("404") || e.to_string().contains("No such container") {
                ok_response(ServiceStatus {
                    container_name: name,
                    container_id: None,
                    running: false,
                    health: None,
                })
                .into_response()
            } else {
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to inspect service '{}': {}", name, e),
                )
                .into_response()
            }
        }
    }
}

/// Execute a command inside a service container.
///
/// Used by the control plane for operations like pg_dump, redis-cli BGSAVE, etc.
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services/exec",
    request_body = ServiceExecRequest,
    responses(
        (status = 200, description = "Command executed", body = AgentResponse<ServiceExecResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Service container not found"),
        (status = 409, description = "Container identity changed before exec start"),
        (status = 429, description = "Exec or capture capacity exhausted"),
        (status = 504, description = "Exec deadline exceeded"),
        (status = 500, description = "Exec failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn service_exec(
    State(state): State<Arc<AgentState>>,
    Extension(limits): Extension<Arc<AgentResourceLimits>>,
    Json(request): Json<ServiceExecRequest>,
) -> impl IntoResponse {
    tracing::info!(
        container = %request.container_name,
        command_argc = request.command.len(),
        detached = request.detach,
        "Executing command in service container"
    );

    let docker = match state.docker.as_ref() {
        Some(d) => d,
        None => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Docker client not available".to_string(),
            )
            .into_response();
        }
    };
    let cleanup_container_id = match resolve_container_id(docker, &request.container_name).await {
        Ok(container_id) => container_id,
        Err(error) => {
            return error_response(
                container_identity_error_status(&error),
                format!(
                    "Failed to resolve service container '{}' before exec: {error}",
                    request.container_name
                ),
            )
            .into_response();
        }
    };

    use bollard::exec::{CreateExecOptions, StartExecOptions};

    let env_strings: Vec<String> = request
        .environment
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect();
    let env_refs: Vec<&str> = env_strings.iter().map(|s| &s[..]).collect();

    let cmd_refs: Vec<&str> = request.command.iter().map(|s| &s[..]).collect();

    let (operation_permit, container_permit, capture_permit) = if request.detach {
        match try_acquire_exec_lifecycle_permits(&limits, &cleanup_container_id) {
            Ok(permits) => (permits.operation, permits.container, None),
            Err(ExecAdmissionError::OperationsExhausted) => {
                return error_response(
                    StatusCode::TOO_MANY_REQUESTS,
                    format!(
                        "Cannot execute detached command in '{}': all exec operation slots are busy",
                        request.container_name
                    ),
                )
                .into_response();
            }
            Err(ExecAdmissionError::ContainerOwned) => {
                return error_response(
                    StatusCode::TOO_MANY_REQUESTS,
                    format!(
                        "Cannot execute detached command in '{}': another operation already owns that container",
                        request.container_name
                    ),
                )
                .into_response();
            }
            Err(ExecAdmissionError::CapturesExhausted) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!(
                        "Detached command in '{}' unexpectedly required output capture capacity",
                        request.container_name
                    ),
                )
                .into_response();
            }
        }
    } else {
        match try_acquire_attached_exec_permits(&limits, &cleanup_container_id) {
            Ok(permits) => (permits.operation, permits.container, Some(permits.capture)),
            Err(ExecAdmissionError::OperationsExhausted) => {
                return error_response(
                    StatusCode::TOO_MANY_REQUESTS,
                    format!(
                        "Cannot execute command in '{}': all exec operation slots are busy",
                        request.container_name
                    ),
                )
                .into_response();
            }
            Err(ExecAdmissionError::CapturesExhausted) => {
                return error_response(
                    StatusCode::TOO_MANY_REQUESTS,
                    format!(
                        "Cannot execute command in '{}': all output capture slots are busy",
                        request.container_name
                    ),
                )
                .into_response();
            }
            Err(ExecAdmissionError::ContainerOwned) => {
                return error_response(
                    StatusCode::TOO_MANY_REQUESTS,
                    format!(
                        "Cannot execute command in '{}': another operation already owns that container",
                        request.container_name
                    ),
                )
                .into_response();
            }
        }
    };

    let exec_config = CreateExecOptions {
        cmd: Some(cmd_refs),
        env: if env_refs.is_empty() {
            None
        } else {
            Some(env_refs)
        },
        attach_stdout: Some(!request.detach),
        attach_stderr: Some(!request.detach),
        user: request.user.as_deref(),
        ..Default::default()
    };

    let exec_create = match docker.create_exec(&cleanup_container_id, exec_config).await {
        Ok(r) => r,
        Err(e) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Failed to create exec in '{}': {}",
                    request.container_name, e
                ),
            )
            .into_response();
        }
    };

    let exec_container_id = match resolve_exec_container_id(docker, &exec_create.id).await {
        Ok(container_id) => container_id,
        Err(error) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to prepare command cleanup: {error}"),
            )
            .into_response();
        }
    };
    if exec_container_id != cleanup_container_id {
        return error_response(
            StatusCode::CONFLICT,
            format!(
                "Service container '{}' changed from '{}' to '{}' before exec start",
                request.container_name, cleanup_container_id, exec_container_id
            ),
        )
        .into_response();
    }

    if request.detach {
        let mut cleanup_guard = ExecCleanupGuard::new_without_capture(
            docker.clone(),
            exec_create.id.clone(),
            cleanup_container_id.clone(),
            Some(operation_permit),
            container_permit,
        );
        let start_result = docker
            .start_exec(
                &exec_create.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await;
        if let Err(error) = start_result {
            let cleanup_scheduled = !exec_start_was_rejected(&error);
            if !cleanup_scheduled {
                cleanup_guard.disarm();
            }
            let cleanup_note = if cleanup_scheduled {
                "its container restart was scheduled because Docker did not definitively reject the workload"
            } else {
                "Docker definitively rejected the workload before start; its container was not restarted"
            };
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to start detached exec: {error}; {cleanup_note}"),
            )
            .into_response();
        }

        monitor_exec_until_stopped(
            docker.clone(),
            exec_create.id.clone(),
            cleanup_container_id,
            "detached service exec",
            cleanup_guard,
        );

        return ok_response(ServiceExecResponse {
            exit_code: 0,
            stdout: String::new(),
            stderr: "detached".to_string(),
        })
        .into_response();
    }

    let capture_permit = match capture_permit {
        Some(permit) => permit,
        None => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Missing output capture permit for attached command in '{}'",
                    request.container_name
                ),
            )
            .into_response();
        }
    };
    let mut cleanup_guard = ExecCleanupGuard::new(
        docker.clone(),
        exec_create.id.clone(),
        cleanup_container_id.clone(),
        capture_permit,
        operation_permit,
        container_permit,
    );

    let (output, response_permit) = match run_exec_with_deadline(
        docker,
        &exec_create.id,
        &cleanup_container_id,
        SERVICE_EXEC_TIMEOUT,
        capture_exec_output(docker, &exec_create.id, SERVICE_EXEC_CAPTURE_LIMITS),
    )
    .await
    {
        ExecDeadlineOutcome::Completed(Ok(output)) => {
            let Some(response_permit) = cleanup_guard.disarm_and_take_capture() else {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!(
                        "Command in '{}' completed without response capture ownership",
                        request.container_name
                    ),
                )
                .into_response();
            };
            (output, response_permit)
        }
        ExecDeadlineOutcome::Completed(Err(error)) => {
            let cleanup_scheduled = !matches!(
                &error,
                CaptureExecError::Start { source, .. } if exec_start_was_rejected(source)
            );
            if !cleanup_scheduled {
                cleanup_guard.disarm();
            }
            let cleanup_note = if cleanup_scheduled {
                "its container restart was scheduled to stop any ambiguous command workload"
            } else {
                "Docker definitively rejected the command before it started; its container was not restarted"
            };
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Failed to execute command in '{}': {error}; {cleanup_note}",
                    request.container_name
                ),
            )
            .into_response();
        }
        ExecDeadlineOutcome::ContainerRestarted => {
            cleanup_guard.disarm();
            return error_response(
                StatusCode::GATEWAY_TIMEOUT,
                format!(
                    "Command in '{}' exceeded the 5-minute worker deadline; its container was restarted to stop the complete command workload",
                    request.container_name
                ),
            )
            .into_response();
        }
        ExecDeadlineOutcome::TerminationFailed(error) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Command in '{}' exceeded the 5-minute worker deadline, but the worker could not restart its container to stop the complete workload: {error}",
                    request.container_name
                ),
            )
            .into_response();
        }
    };

    tracing::info!(
        container = %request.container_name,
        exit_code = output.exit_code,
        received_stdout_bytes = output.received_stdout_bytes,
        received_stderr_bytes = output.received_stderr_bytes,
        "Exec completed"
    );

    captured_ok_response(
        ServiceExecResponse {
            exit_code: output.exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
        },
        response_permit,
    )
}

/// Provision a project's logical database, bucket, or Redis allocation on
/// the worker that owns the external-service container.
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services/runtime-env",
    request_body = RemoteRuntimeEnvRequest,
    responses(
        (status = 200, description = "Runtime resource provisioned", body = AgentResponse<RemoteRuntimeEnvResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Provisioning failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn runtime_env(
    State(state): State<Arc<AgentState>>,
    Json(request): Json<RemoteRuntimeEnvRequest>,
) -> impl IntoResponse {
    let service_name = request.service_config.name.clone();
    let service_type = request.service_config.service_type;
    tracing::info!(
        service = %service_name,
        service_type = %service_type,
        project = %request.project_slug,
        environment = %request.environment_slug,
        "Provisioning external-service runtime environment"
    );

    let docker = match state.docker.as_ref() {
        Some(docker) => Arc::new(docker.clone()),
        None => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Docker client is unavailable while provisioning service '{}'",
                    service_name
                ),
            )
            .into_response();
        }
    };

    match temps_providers::externalsvc::provision_runtime_environment(
        service_name.clone(),
        service_type,
        request.service_config,
        &request.project_slug,
        &request.environment_slug,
        docker,
    )
    .await
    {
        Ok(environment) => ok_response(RemoteRuntimeEnvResponse { environment }).into_response(),
        Err(error) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "Failed to provision runtime resource for service '{}': {}",
                service_name, error
            ),
        )
        .into_response(),
    }
}

/// Run an authenticated, provider-specific health probe on this node.
///
/// This endpoint intentionally does not accept arbitrary hosts, ports,
/// commands, or query text. Provider code derives the probe target and
/// protocol from the service configuration supplied by the control plane.
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services/health-probe",
    request_body = RemoteHealthProbeRequest,
    responses(
        (status = 200, description = "Health probe completed", body = AgentResponse<RemoteHealthProbeResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Health probe failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn health_probe(
    State(state): State<Arc<AgentState>>,
    Json(request): Json<RemoteHealthProbeRequest>,
) -> impl IntoResponse {
    let service_name = request.service_config.name.clone();
    let service_type = request.service_config.service_type;
    tracing::info!(
        service = %service_name,
        service_type = %service_type,
        "Running provider-authenticated external-service health probe"
    );

    let docker = match state.docker.as_ref() {
        Some(docker) => Arc::new(docker.clone()),
        None => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Docker client is unavailable while probing service '{}'",
                    service_name
                ),
            )
            .into_response();
        }
    };

    match temps_providers::externalsvc::probe_service_health(request.service_config, docker).await {
        Ok(result) => ok_response(RemoteHealthProbeResponse { result }).into_response(),
        Err(error) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "Health probe failed for service '{}': {}",
                service_name, error
            ),
        )
        .into_response(),
    }
}

/// List all service containers on this node.
#[utoipa::path(
    tag = "Services",
    get,
    path = "/agent/services",
    responses(
        (status = 200, description = "Service list", body = AgentResponse<Vec<ServiceStatus>>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "List failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_services(State(state): State<Arc<AgentState>>) -> impl IntoResponse {
    let docker = match state.docker.as_ref() {
        Some(d) => d,
        None => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Docker client not available".to_string(),
            )
            .into_response();
        }
    };

    let mut filters = HashMap::new();
    filters.insert(
        "label".to_string(),
        vec!["sh.temps.service=true".to_string()],
    );

    let opts = ListContainersOptions {
        all: true,
        filters: Some(filters),
        ..Default::default()
    };

    match docker.list_containers(Some(opts)).await {
        Ok(containers) => {
            let services: Vec<ServiceStatus> = containers
                .into_iter()
                .map(|c| {
                    let name = c
                        .names
                        .as_ref()
                        .and_then(|n| n.first())
                        .map(|n| n.trim_start_matches('/').to_string())
                        .unwrap_or_default();
                    let running = c
                        .state
                        .as_ref()
                        .map(|s| format!("{:?}", s).to_lowercase().contains("running"))
                        .unwrap_or(false);
                    ServiceStatus {
                        container_name: name,
                        container_id: c.id.clone(),
                        running,
                        health: c.status.clone(),
                    }
                })
                .collect();
            ok_response(services).into_response()
        }
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to list services: {}", e),
        )
        .into_response(),
    }
}

/// Backup a service directly to S3.
///
/// Executes the appropriate backup command inside the service container
/// and streams the output to S3. The control plane distributes S3 credentials
/// to the agent for each backup request.
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services/backup",
    request_body = ServiceBackupRequest,
    responses(
        (status = 200, description = "Backup completed", body = AgentResponse<ServiceBackupResponse>),
        (status = 400, description = "Unsupported service type"),
        (status = 404, description = "Service container not found"),
        (status = 409, description = "Container identity changed before backup start"),
        (status = 429, description = "Exec or capture capacity exhausted"),
        (status = 504, description = "Backup deadline exceeded"),
        (status = 500, description = "Backup failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn backup_service(
    State(state): State<Arc<AgentState>>,
    Extension(limits): Extension<Arc<AgentResourceLimits>>,
    Json(request): Json<ServiceBackupRequest>,
) -> impl IntoResponse {
    tracing::info!(
        container = %request.container_name,
        service_type = %request.service_type,
        s3_path = %request.s3_path,
        "Starting service backup"
    );

    let docker = match state.docker.as_ref() {
        Some(d) => d,
        None => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Docker client not available".to_string(),
            )
            .into_response();
        }
    };
    let cleanup_container_id = match resolve_container_id(docker, &request.container_name).await {
        Ok(container_id) => container_id,
        Err(error) => {
            return error_response(
                container_identity_error_status(&error),
                format!(
                    "Failed to resolve service container '{}' before backup: {error}",
                    request.container_name
                ),
            )
            .into_response();
        }
    };

    // Build the backup command and env vars based on service type
    let method = request
        .method
        .as_deref()
        .unwrap_or(match request.service_type.as_str() {
            "postgres" => "walg",
            "redis" => "rdb_copy",
            "mongodb" => "mongodump",
            _ => "pg_dump",
        });

    let s3_env = build_s3_env(&request);

    let (cmd, user): (Vec<String>, Option<&str>) = match (request.service_type.as_str(), method) {
        ("postgres", "walg") => {
            let cmd = vec![
                "bash".to_string(),
                "-c".to_string(),
                "wal-g backup-push /var/lib/postgresql/data".to_string(),
            ];
            (cmd, Some("postgres"))
        }
        ("postgres", _) => {
            // pg_dumpall dumps the entire cluster (all databases, roles, tablespaces).
            // Output is plain SQL (custom format is not supported by pg_dumpall), so the
            // restore path must use `psql -f` rather than `pg_restore`.
            let cmd = postgres_dump_command();
            (cmd, Some("postgres"))
        }
        ("redis", _) => {
            let cmd = vec![
                "bash".to_string(),
                "-c".to_string(),
                "redis-cli BGSAVE && sleep 2 && cp /data/dump.rdb /tmp/backup.rdb && echo 'dump_complete'"
                    .to_string(),
            ];
            (cmd, None)
        }
        ("mongodb", _) => {
            let cmd = vec![
                "bash".to_string(),
                "-c".to_string(),
                "mongodump --archive=/tmp/backup.archive --gzip && echo 'dump_complete'"
                    .to_string(),
            ];
            (cmd, None)
        }
        _ => {
            return error_response(
                StatusCode::BAD_REQUEST,
                format!(
                    "Unsupported service type for backup: {}",
                    request.service_type
                ),
            )
            .into_response();
        }
    };

    let permits = match try_acquire_attached_exec_permits(&limits, &cleanup_container_id) {
        Ok(permits) => permits,
        Err(ExecAdmissionError::OperationsExhausted) => {
            return error_response(
                StatusCode::TOO_MANY_REQUESTS,
                format!(
                    "Cannot back up '{}': all exec operation slots are busy",
                    request.container_name
                ),
            )
            .into_response();
        }
        Err(ExecAdmissionError::CapturesExhausted) => {
            return error_response(
                StatusCode::TOO_MANY_REQUESTS,
                format!(
                    "Cannot back up '{}': all output capture slots are busy",
                    request.container_name
                ),
            )
            .into_response();
        }
        Err(ExecAdmissionError::ContainerOwned) => {
            return error_response(
                StatusCode::TOO_MANY_REQUESTS,
                format!(
                    "Cannot back up '{}': another operation already owns that container",
                    request.container_name
                ),
            )
            .into_response();
        }
    };
    let capture_permit = permits.capture;
    let operation_permit = permits.operation;
    let container_permit = permits.container;

    // Execute the backup command inside the container.
    use bollard::exec::CreateExecOptions;

    let env_strings: Vec<String> = s3_env.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
    let env_refs: Vec<&str> = env_strings.iter().map(|s| &s[..]).collect();
    let cmd_refs: Vec<&str> = cmd.iter().map(|s| &s[..]).collect();

    let exec_config = CreateExecOptions {
        cmd: Some(cmd_refs),
        env: if env_refs.is_empty() {
            None
        } else {
            Some(env_refs)
        },
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        user,
        ..Default::default()
    };

    let exec_create = match docker.create_exec(&cleanup_container_id, exec_config).await {
        Ok(r) => r,
        Err(e) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create backup exec: {}", e),
            )
            .into_response();
        }
    };
    let exec_container_id = match resolve_exec_container_id(docker, &exec_create.id).await {
        Ok(container_id) => container_id,
        Err(error) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to prepare backup cleanup: {error}"),
            )
            .into_response();
        }
    };
    if exec_container_id != cleanup_container_id {
        return error_response(
            StatusCode::CONFLICT,
            format!(
                "Service container '{}' changed from '{}' to '{}' before backup start",
                request.container_name, cleanup_container_id, exec_container_id
            ),
        )
        .into_response();
    }
    let mut cleanup_guard = ExecCleanupGuard::new(
        docker.clone(),
        exec_create.id.clone(),
        cleanup_container_id.clone(),
        capture_permit,
        operation_permit,
        container_permit,
    );

    let (output, response_permit) = match run_exec_with_deadline(
        docker,
        &exec_create.id,
        &cleanup_container_id,
        SERVICE_DATA_OPERATION_TIMEOUT,
        capture_exec_output(docker, &exec_create.id, DATA_OPERATION_CAPTURE_LIMITS),
    )
    .await
    {
        ExecDeadlineOutcome::Completed(Ok(output)) => {
            let Some(response_permit) = cleanup_guard.disarm_and_take_capture() else {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!(
                        "Backup in '{}' completed without response capture ownership",
                        request.container_name
                    ),
                )
                .into_response();
            };
            (output, response_permit)
        }
        ExecDeadlineOutcome::Completed(Err(error)) => {
            let cleanup_scheduled = !matches!(
                &error,
                CaptureExecError::Start { source, .. } if exec_start_was_rejected(source)
            );
            if !cleanup_scheduled {
                cleanup_guard.disarm();
            }
            let cleanup_note = if cleanup_scheduled {
                "its container restart was scheduled to stop any ambiguous backup workload"
            } else {
                "Docker definitively rejected the backup before it started; its container was not restarted"
            };
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Failed to run backup command in '{}': {error}; {cleanup_note}",
                    request.container_name
                ),
            )
            .into_response();
        }
        ExecDeadlineOutcome::ContainerRestarted => {
            cleanup_guard.disarm();
            return error_response(
                StatusCode::GATEWAY_TIMEOUT,
                format!(
                    "Backup command in '{}' exceeded the 2-hour worker deadline; its container was restarted to stop the complete backup workload",
                    request.container_name
                ),
            )
            .into_response();
        }
        ExecDeadlineOutcome::TerminationFailed(error) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Backup command in '{}' exceeded the 2-hour worker deadline, but the worker could not restart its container to stop the complete workload: {error}",
                    request.container_name
                ),
            )
            .into_response();
        }
    };

    if let Some(message) = exec_failure_message("Backup", &request.container_name, &output) {
        tracing::error!(
            container = %request.container_name,
            exit_code = output.exit_code,
            received_stdout_bytes = output.received_stdout_bytes,
            received_stderr_bytes = output.received_stderr_bytes,
            "Backup command failed"
        );
        return captured_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            message,
            response_permit,
        );
    }

    tracing::info!(
        container = %request.container_name,
        received_stdout_bytes = output.received_stdout_bytes,
        received_stderr_bytes = output.received_stderr_bytes,
        "Backup completed successfully"
    );

    captured_ok_response(
        ServiceBackupResponse {
            s3_location: request.s3_path.clone(),
            size_bytes: 0,
            compression_type: "gzip".to_string(),
            checksum: None,
        },
        response_permit,
    )
}

/// Restore a service from S3.
///
/// Downloads the backup from S3 and restores it into the service container.
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services/restore",
    request_body = ServiceRestoreRequest,
    responses(
        (status = 200, description = "Restore completed"),
        (status = 400, description = "Unsupported service type"),
        (status = 404, description = "Service container not found"),
        (status = 409, description = "Container identity changed before restore start"),
        (status = 429, description = "Exec or capture capacity exhausted"),
        (status = 504, description = "Restore deadline exceeded"),
        (status = 500, description = "Restore failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn restore_service(
    State(state): State<Arc<AgentState>>,
    Extension(limits): Extension<Arc<AgentResourceLimits>>,
    Json(request): Json<ServiceRestoreRequest>,
) -> impl IntoResponse {
    tracing::info!(
        container = %request.container_name,
        service_type = %request.service_type,
        s3_location = %request.s3_location,
        "Starting service restore"
    );

    let docker = match state.docker.as_ref() {
        Some(d) => d,
        None => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Docker client not available".to_string(),
            )
            .into_response();
        }
    };
    let cleanup_container_id = match resolve_container_id(docker, &request.container_name).await {
        Ok(container_id) => container_id,
        Err(error) => {
            return error_response(
                container_identity_error_status(&error),
                format!(
                    "Failed to resolve service container '{}' before restore: {error}",
                    request.container_name
                ),
            )
            .into_response();
        }
    };

    let s3_env = build_s3_restore_env(&request);

    let (cmd, user): (Vec<String>, Option<&str>) = match request.service_type.as_str() {
        "postgres" => {
            let cmd = vec![
                "bash".to_string(),
                "-c".to_string(),
                "wal-g backup-fetch /var/lib/postgresql/data LATEST".to_string(),
            ];
            (cmd, Some("postgres"))
        }
        "redis" => {
            let cmd = vec![
                "bash".to_string(),
                "-c".to_string(),
                "redis-cli SHUTDOWN NOSAVE; cp /tmp/restore.rdb /data/dump.rdb; redis-server"
                    .to_string(),
            ];
            (cmd, None)
        }
        "mongodb" => {
            let cmd = vec![
                "bash".to_string(),
                "-c".to_string(),
                "mongorestore --archive=/tmp/restore.archive --gzip --drop".to_string(),
            ];
            (cmd, None)
        }
        _ => {
            return error_response(
                StatusCode::BAD_REQUEST,
                format!(
                    "Unsupported service type for restore: {}",
                    request.service_type
                ),
            )
            .into_response();
        }
    };

    let permits = match try_acquire_attached_exec_permits(&limits, &cleanup_container_id) {
        Ok(permits) => permits,
        Err(ExecAdmissionError::OperationsExhausted) => {
            return error_response(
                StatusCode::TOO_MANY_REQUESTS,
                format!(
                    "Cannot restore '{}': all exec operation slots are busy",
                    request.container_name
                ),
            )
            .into_response();
        }
        Err(ExecAdmissionError::CapturesExhausted) => {
            return error_response(
                StatusCode::TOO_MANY_REQUESTS,
                format!(
                    "Cannot restore '{}': all output capture slots are busy",
                    request.container_name
                ),
            )
            .into_response();
        }
        Err(ExecAdmissionError::ContainerOwned) => {
            return error_response(
                StatusCode::TOO_MANY_REQUESTS,
                format!(
                    "Cannot restore '{}': another operation already owns that container",
                    request.container_name
                ),
            )
            .into_response();
        }
    };
    let capture_permit = permits.capture;
    let operation_permit = permits.operation;
    let container_permit = permits.container;

    use bollard::exec::CreateExecOptions;

    let env_strings: Vec<String> = s3_env.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
    let env_refs: Vec<&str> = env_strings.iter().map(|s| &s[..]).collect();
    let cmd_refs: Vec<&str> = cmd.iter().map(|s| &s[..]).collect();

    let exec_config = CreateExecOptions {
        cmd: Some(cmd_refs),
        env: if env_refs.is_empty() {
            None
        } else {
            Some(env_refs)
        },
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        user,
        ..Default::default()
    };

    let exec_create = match docker.create_exec(&cleanup_container_id, exec_config).await {
        Ok(r) => r,
        Err(e) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create restore exec: {}", e),
            )
            .into_response();
        }
    };
    let exec_container_id = match resolve_exec_container_id(docker, &exec_create.id).await {
        Ok(container_id) => container_id,
        Err(error) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to prepare restore cleanup: {error}"),
            )
            .into_response();
        }
    };
    if exec_container_id != cleanup_container_id {
        return error_response(
            StatusCode::CONFLICT,
            format!(
                "Service container '{}' changed from '{}' to '{}' before restore start",
                request.container_name, cleanup_container_id, exec_container_id
            ),
        )
        .into_response();
    }
    let mut cleanup_guard = ExecCleanupGuard::new(
        docker.clone(),
        exec_create.id.clone(),
        cleanup_container_id.clone(),
        capture_permit,
        operation_permit,
        container_permit,
    );

    let (output, response_permit) = match run_exec_with_deadline(
        docker,
        &exec_create.id,
        &cleanup_container_id,
        SERVICE_DATA_OPERATION_TIMEOUT,
        capture_exec_output(docker, &exec_create.id, DATA_OPERATION_CAPTURE_LIMITS),
    )
    .await
    {
        ExecDeadlineOutcome::Completed(Ok(output)) => {
            let Some(response_permit) = cleanup_guard.disarm_and_take_capture() else {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!(
                        "Restore in '{}' completed without response capture ownership",
                        request.container_name
                    ),
                )
                .into_response();
            };
            (output, response_permit)
        }
        ExecDeadlineOutcome::Completed(Err(error)) => {
            let cleanup_scheduled = !matches!(
                &error,
                CaptureExecError::Start { source, .. } if exec_start_was_rejected(source)
            );
            if !cleanup_scheduled {
                cleanup_guard.disarm();
            }
            let cleanup_note = if cleanup_scheduled {
                "its container restart was scheduled to stop any ambiguous restore workload"
            } else {
                "Docker definitively rejected the restore before it started; its container was not restarted"
            };
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Failed to run restore command in '{}': {error}; {cleanup_note}",
                    request.container_name
                ),
            )
            .into_response();
        }
        ExecDeadlineOutcome::ContainerRestarted => {
            cleanup_guard.disarm();
            return error_response(
                StatusCode::GATEWAY_TIMEOUT,
                format!(
                    "Restore command in '{}' exceeded the 2-hour worker deadline; its container was restarted to stop the complete restore workload",
                    request.container_name
                ),
            )
            .into_response();
        }
        ExecDeadlineOutcome::TerminationFailed(error) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Restore command in '{}' exceeded the 2-hour worker deadline, but the worker could not restart its container to stop the complete workload: {error}",
                    request.container_name
                ),
            )
            .into_response();
        }
    };

    if let Some(message) = exec_failure_message("Restore", &request.container_name, &output) {
        tracing::error!(
            container = %request.container_name,
            exit_code = output.exit_code,
            received_stdout_bytes = output.received_stdout_bytes,
            received_stderr_bytes = output.received_stderr_bytes,
            "Restore command failed"
        );
        return captured_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            message,
            response_permit,
        );
    }

    tracing::info!(
        container = %request.container_name,
        received_stdout_bytes = output.received_stdout_bytes,
        received_stderr_bytes = output.received_stderr_bytes,
        "Restore completed successfully"
    );

    captured_ok_response(
        serde_json::json!({
            "status": "restored",
            "container_name": request.container_name,
        }),
        response_permit,
    )
}

/// Build S3 environment variables for backup commands (WAL-G, etc.)
fn build_s3_env(request: &ServiceBackupRequest) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert(
        "AWS_ACCESS_KEY_ID".to_string(),
        request.s3.access_key_id.clone(),
    );
    env.insert(
        "AWS_SECRET_ACCESS_KEY".to_string(),
        request.s3.secret_key.clone(),
    );
    env.insert("AWS_REGION".to_string(), request.s3.region.clone());
    env.insert(
        "WALG_S3_PREFIX".to_string(),
        format!("s3://{}/{}", request.s3.bucket_name, request.s3_path),
    );
    if let Some(ref endpoint) = request.s3.endpoint {
        env.insert("AWS_ENDPOINT".to_string(), endpoint.clone());
    }
    if request.s3.force_path_style {
        env.insert("AWS_S3_FORCE_PATH_STYLE".to_string(), "true".to_string());
    }
    env
}

/// Build S3 environment variables for restore commands.
fn build_s3_restore_env(request: &ServiceRestoreRequest) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert(
        "AWS_ACCESS_KEY_ID".to_string(),
        request.s3.access_key_id.clone(),
    );
    env.insert(
        "AWS_SECRET_ACCESS_KEY".to_string(),
        request.s3.secret_key.clone(),
    );
    env.insert("AWS_REGION".to_string(), request.s3.region.clone());
    env.insert("WALG_S3_PREFIX".to_string(), request.s3_location.clone());
    if let Some(ref endpoint) = request.s3.endpoint {
        env.insert("AWS_ENDPOINT".to_string(), endpoint.clone());
    }
    if request.s3.force_path_style {
        env.insert("AWS_S3_FORCE_PATH_STYLE".to_string(), "true".to_string());
    }
    env
}

/// Attach a container to the multi-host overlay network (ADR-011) if the
/// overlay exists on this host. Best-effort: returns `Ok(())` when the
/// overlay isn't bootstrapped yet (single-host mode) or when the
/// container is already attached. Only true bollard errors propagate.
///
/// The agent calls this between `create_container` and `start_container`
/// for service members so they come up dual-attached to both the legacy
/// `temps-app-network` (so existing single-host code paths keep working)
/// AND `temps-overlay` (so cross-node DNS records can be written and
/// apps anywhere on the overlay can reach the container by FQDN VIP).
async fn attach_to_overlay_if_present(
    docker: &bollard::Docker,
    container_id: &str,
) -> std::result::Result<(), bollard::errors::Error> {
    let overlay_name = temps_network::NetworkConfig::default().docker_network_name;

    // Cheap existence probe — if the overlay isn't here, skip silently.
    let networks = docker
        .list_networks(None::<bollard::query_parameters::ListNetworksOptions>)
        .await?;
    let exists = networks
        .iter()
        .any(|n| n.name.as_deref() == Some(overlay_name.as_str()));
    if !exists {
        tracing::debug!(
            container = %container_id,
            overlay = %overlay_name,
            "overlay network not present on this host; skipping attach (single-host mode)"
        );
        return Ok(());
    }

    let req = bollard::models::NetworkConnectRequest {
        container: container_id.to_string(),
        ..Default::default()
    };
    match docker.connect_network(&overlay_name, req).await {
        Ok(()) => {
            tracing::info!(
                container = %container_id,
                overlay = %overlay_name,
                "attached service container to overlay"
            );
            Ok(())
        }
        // 403 from /networks/<id>/connect = "already connected" — no-op.
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 403, ..
        }) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Install per-peer overlay routes inside the container's netns. Must
/// be called **after** start_container — `docker inspect` only reports
/// a non-zero PID for running containers, and `nsenter -t <pid> -n`
/// needs that PID to enter the netns.
///
/// Best-effort: any failure is logged and swallowed.
async fn install_overlay_peer_routes_after_start(
    docker: &bollard::Docker,
    container_id: &str,
    shared_peers: &crate::network_sync::SharedPeers,
) -> std::result::Result<(), String> {
    let peers = shared_peers
        .read()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    if peers.is_empty() {
        // No peers known yet — common on a freshly-started worker before
        // the first network/peers poll completes. The next reconcile
        // tick will re-attach via this same path or the route will be
        // missing until the container is recreated; either way we don't
        // block.
        return Ok(());
    }

    let inspect = docker
        .inspect_container(
            container_id,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .map_err(|e| format!("inspect_container: {}", e))?;

    let pid = inspect
        .state
        .as_ref()
        .and_then(|s| s.pid)
        .filter(|p| *p > 0)
        .ok_or_else(|| "container PID not yet available".to_string())? as i32;

    let overlay_name = temps_network::NetworkConfig::default().docker_network_name;
    let gateway = inspect
        .network_settings
        .as_ref()
        .and_then(|ns| ns.networks.as_ref())
        .and_then(|nets| nets.get(&overlay_name))
        .and_then(|net| net.gateway.clone())
        .filter(|g| !g.is_empty())
        .ok_or_else(|| {
            format!(
                "no gateway recorded for overlay '{}' on container",
                overlay_name
            )
        })?;

    // Convention: Docker assigns interface names in attach order,
    // primary network first. The overlay attach happens last in the
    // service-create path, so the overlay interface is `eth1`.
    temps_network::overlay_routes::install_peer_routes_in_container(pid, "eth1", &gateway, &peers)
        .await
        .map_err(|e| e.to_string())
}

/// Pull the container's IP on the multi-host overlay (`temps-overlay`) out
/// of a `docker inspect` result. Returns `None` when the container isn't
/// attached to the overlay (single-host clusters), or when the inspect
/// payload doesn't carry network settings yet (rare, transient post-start).
///
/// We deliberately don't hard-code the network name — the agent's overlay
/// uses `temps_network::NetworkConfig::default().docker_network_name`, so
/// reading it from there keeps both call sites agreeing on the spelling.
fn extract_overlay_ip(info: &bollard::models::ContainerInspectResponse) -> Option<String> {
    let overlay_name = temps_network::NetworkConfig::default().docker_network_name;
    let networks = info.network_settings.as_ref()?.networks.as_ref()?;
    let entry = networks.get(&overlay_name)?;
    let ip = entry.ip_address.as_deref()?.trim();
    if ip.is_empty() {
        None
    } else {
        Some(ip.to_string())
    }
}

#[cfg(test)]
mod overlay_ip_tests {
    use super::*;
    use bollard::models::{ContainerInspectResponse, EndpointSettings, NetworkSettings};

    fn inspect_with_networks(
        networks: HashMap<String, EndpointSettings>,
    ) -> ContainerInspectResponse {
        ContainerInspectResponse {
            network_settings: Some(NetworkSettings {
                networks: Some(networks),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn returns_none_when_overlay_absent() {
        let mut nets = HashMap::new();
        nets.insert(
            "temps-app-network".to_string(),
            EndpointSettings {
                ip_address: Some("172.18.0.5".into()),
                ..Default::default()
            },
        );
        assert!(extract_overlay_ip(&inspect_with_networks(nets)).is_none());
    }

    #[test]
    fn returns_ip_when_overlay_present() {
        let overlay_name = temps_network::NetworkConfig::default().docker_network_name;
        let mut nets = HashMap::new();
        nets.insert(
            overlay_name,
            EndpointSettings {
                ip_address: Some("172.20.5.42".into()),
                ..Default::default()
            },
        );
        assert_eq!(
            extract_overlay_ip(&inspect_with_networks(nets)).as_deref(),
            Some("172.20.5.42")
        );
    }

    #[test]
    fn returns_none_for_empty_ip_string() {
        let overlay_name = temps_network::NetworkConfig::default().docker_network_name;
        let mut nets = HashMap::new();
        nets.insert(
            overlay_name,
            EndpointSettings {
                ip_address: Some("".into()),
                ..Default::default()
            },
        );
        assert!(extract_overlay_ip(&inspect_with_networks(nets)).is_none());
    }

    #[test]
    fn returns_none_when_network_settings_missing() {
        let info = ContainerInspectResponse::default();
        assert!(extract_overlay_ip(&info).is_none());
    }

    #[test]
    fn removal_uses_actual_named_mounts_and_canonical_fallback() {
        let names = mounted_volume_names(
            "temps-mongodb-orders",
            vec![bollard::models::MountPoint {
                name: Some("mongodb-orders_data".to_string()),
                ..Default::default()
            }],
        );

        assert!(names.contains("mongodb-orders_data"));
        assert!(names.contains("temps-mongodb-orders_data"));
    }

    #[test]
    fn nonzero_exec_exit_includes_bounded_stderr_context() {
        let output = CapturedExecOutput {
            exit_code: 17,
            stdout: String::new(),
            // Model a diagnostic split across Docker frames before capture.
            stderr: ["FA", "TAL: upload failed"].concat(),
            received_stdout_bytes: 0,
            received_stderr_bytes: 20,
        };

        let message = exec_failure_message("Backup", "postgres-orders", &output)
            .expect("a nonzero exit must fail regardless of frame boundaries");
        assert!(message.contains("postgres-orders"));
        assert!(message.contains("status 17"));
        assert!(message.contains("FATAL: upload failed"));
    }

    #[test]
    fn zero_exec_exit_is_success_even_when_stderr_contains_error_word() {
        let output = CapturedExecOutput {
            exit_code: 0,
            stdout: String::new(),
            stderr: "non-fatal error counter: 0".to_string(),
            received_stdout_bytes: 0,
            received_stderr_bytes: 26,
        };

        assert!(exec_failure_message("Restore", "redis-cache", &output).is_none());
    }

    #[test]
    fn data_operations_discard_stdout_and_keep_small_stderr_tail() {
        assert_eq!(DATA_OPERATION_CAPTURE_LIMITS.stdout, 0);
        assert_eq!(
            DATA_OPERATION_CAPTURE_LIMITS.stderr,
            DATA_OPERATION_STDERR_CAPTURE_BYTES
        );
    }

    #[test]
    fn postgres_dump_pipeline_enables_pipefail() {
        let command = postgres_dump_command();

        assert_eq!(&command[..4], ["bash", "-o", "pipefail", "-c"]);
        assert!(command[4].contains("pg_dumpall"));
        assert!(command[4].contains("| gzip"));
        assert!(command[4].contains("&& echo 'dump_complete'"));
    }
}
