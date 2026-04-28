//! HTTP handlers for external service operations on worker nodes.
//!
//! These endpoints allow the control plane to manage external services
//! (PostgreSQL, Redis, MongoDB, S3) on any node in the cluster.

use axum::{
    extract::{Path, State},
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

use crate::handlers::{AgentResponse, AgentState};
use crate::{
    ServiceBackupRequest, ServiceBackupResponse, ServiceCreateRequest, ServiceCreateResponse,
    ServiceExecRequest, ServiceExecResponse, ServiceRestoreRequest, ServiceStatus,
};

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
    // by `network_sync` once the bridge gateway is up.
    let dns_servers: Option<Vec<String>> = state
        .overlay_bridge_address
        .read()
        .ok()
        .and_then(|slot| slot.as_ref().map(|ip| vec![ip.to_string()]));
    if let Some(ref dns) = dns_servers {
        tracing::debug!(
            container = %container_name,
            dns = ?dns,
            "Wiring temps DNS into container resolv.conf"
        );
    }

    let host_config = bollard::models::HostConfig {
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
    let volume_name = format!("{}_data", name);
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
        (status = 500, description = "Exec failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn service_exec(
    State(state): State<Arc<AgentState>>,
    Json(request): Json<ServiceExecRequest>,
) -> impl IntoResponse {
    tracing::info!(
        container = %request.container_name,
        command = ?request.command,
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

    use bollard::exec::{CreateExecOptions, StartExecOptions};

    let env_strings: Vec<String> = request
        .environment
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect();
    let env_refs: Vec<&str> = env_strings.iter().map(|s| &s[..]).collect();

    let cmd_refs: Vec<&str> = request.command.iter().map(|s| &s[..]).collect();

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

    let exec_create = match docker
        .create_exec(&request.container_name, exec_config)
        .await
    {
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

    if request.detach {
        // Start detached — don't wait for output
        if let Err(e) = docker
            .start_exec(
                &exec_create.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await
        {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to start exec (detached): {}", e),
            )
            .into_response();
        }

        return ok_response(ServiceExecResponse {
            exit_code: 0,
            stdout: String::new(),
            stderr: "detached".to_string(),
        })
        .into_response();
    }

    // Start attached — collect output
    let output = match docker
        .start_exec(&exec_create.id, None::<StartExecOptions>)
        .await
    {
        Ok(bollard::exec::StartExecResults::Attached { mut output, .. }) => {
            use futures::StreamExt;
            let mut stdout = String::new();
            let mut stderr = String::new();
            while let Some(chunk) = output.next().await {
                match chunk {
                    Ok(bollard::container::LogOutput::StdOut { message }) => {
                        stdout.push_str(&String::from_utf8_lossy(&message));
                    }
                    Ok(bollard::container::LogOutput::StdErr { message }) => {
                        stderr.push_str(&String::from_utf8_lossy(&message));
                    }
                    Ok(_) => {}
                    Err(e) => {
                        stderr.push_str(&format!("Stream error: {}\n", e));
                    }
                }
            }
            (stdout, stderr)
        }
        Ok(bollard::exec::StartExecResults::Detached) => (String::new(), String::new()),
        Err(e) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to start exec: {}", e),
            )
            .into_response();
        }
    };

    // Get exit code
    let exit_code = match docker.inspect_exec(&exec_create.id).await {
        Ok(info) => info.exit_code.unwrap_or(-1),
        Err(_) => -1,
    };

    tracing::info!(
        container = %request.container_name,
        exit_code = exit_code,
        "Exec completed"
    );

    ok_response(ServiceExecResponse {
        exit_code,
        stdout: output.0,
        stderr: output.1,
    })
    .into_response()
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
        (status = 500, description = "Backup failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn backup_service(
    State(state): State<Arc<AgentState>>,
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
            let cmd = vec![
                "bash".to_string(),
                "-c".to_string(),
                "pg_dumpall --clean --if-exists --no-acl --no-owner -U postgres | gzip > /tmp/backup.sql.gz && echo 'dump_complete'"
                    .to_string(),
            ];
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

    // Execute the backup command inside the container
    use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};
    use futures::StreamExt;

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

    let exec_create = match docker
        .create_exec(&request.container_name, exec_config)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create backup exec: {}", e),
            )
            .into_response();
        }
    };

    let start_opts = StartExecOptions {
        ..Default::default()
    };

    match docker.start_exec(&exec_create.id, Some(start_opts)).await {
        Ok(StartExecResults::Attached { mut output, .. }) => {
            let mut stdout = String::new();
            let mut stderr = String::new();

            while let Some(chunk) = output.next().await {
                match chunk {
                    Ok(bollard::container::LogOutput::StdOut { message }) => {
                        stdout.push_str(&String::from_utf8_lossy(&message));
                    }
                    Ok(bollard::container::LogOutput::StdErr { message }) => {
                        stderr.push_str(&String::from_utf8_lossy(&message));
                    }
                    Ok(_) => {}
                    Err(e) => {
                        return error_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("Error reading backup output: {}", e),
                        )
                        .into_response();
                    }
                }
            }

            if stderr.contains("error") || stderr.contains("FATAL") {
                tracing::error!(
                    container = %request.container_name,
                    "Backup failed: {}", stderr
                );
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Backup failed: {}", stderr),
                )
                .into_response();
            }

            tracing::info!(
                container = %request.container_name,
                stdout = %stdout,
                "Backup completed successfully"
            );

            ok_response(ServiceBackupResponse {
                s3_location: request.s3_path.clone(),
                size_bytes: 0,
                compression_type: "gzip".to_string(),
                checksum: None,
            })
            .into_response()
        }
        Ok(StartExecResults::Detached) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Backup exec unexpectedly detached".to_string(),
        )
        .into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to start backup exec: {}", e),
        )
        .into_response(),
    }
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
        (status = 500, description = "Restore failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn restore_service(
    State(state): State<Arc<AgentState>>,
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

    use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};
    use futures::StreamExt;

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

    let exec_create = match docker
        .create_exec(&request.container_name, exec_config)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create restore exec: {}", e),
            )
            .into_response();
        }
    };

    let start_opts = StartExecOptions {
        ..Default::default()
    };

    match docker.start_exec(&exec_create.id, Some(start_opts)).await {
        Ok(StartExecResults::Attached { mut output, .. }) => {
            let mut stderr = String::new();

            while let Some(chunk) = output.next().await {
                match chunk {
                    Ok(bollard::container::LogOutput::StdErr { message }) => {
                        stderr.push_str(&String::from_utf8_lossy(&message));
                    }
                    Ok(_) => {}
                    Err(e) => {
                        return error_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("Error reading restore output: {}", e),
                        )
                        .into_response();
                    }
                }
            }

            if stderr.contains("error") || stderr.contains("FATAL") {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Restore failed: {}", stderr),
                )
                .into_response();
            }

            tracing::info!(
                container = %request.container_name,
                "Restore completed successfully"
            );

            ok_response(serde_json::json!({
                "status": "restored",
                "container_name": request.container_name,
            }))
            .into_response()
        }
        Ok(StartExecResults::Detached) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Restore exec unexpectedly detached".to_string(),
        )
        .into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to start restore exec: {}", e),
        )
        .into_response(),
    }
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
}
