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
use temps_service_protocol::{
    NodeAttachment, NodeServiceError, RemoteHealthProbeRequest, RemoteHealthProbeResponse,
    RemoteRuntimeEnvRequest, RemoteRuntimeEnvResponse, WorkloadCreateBody,
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

/// Make sure the Docker network a managed-service container is asked to join
/// exists before the container is created.
///
/// The deployer creates the Temps app network on a worker's first
/// deployment, so a worker that had never run one had no such network, and
/// the first managed service (or HA cluster member) placed on it failed with
/// "network ... not found". Only `app_network` is created here; any other
/// name is left to Docker, which reports it as before. Returns the app
/// network's name when the container joins it, so the caller can apply the
/// cloud-metadata egress block the deployer applies to it.
async fn ensure_service_network<'a>(
    docker: &bollard::Docker,
    requested: Option<&'a str>,
    app_network: &str,
) -> Result<Option<&'a str>, String> {
    let Some(name) = requested.filter(|name| *name == app_network) else {
        return Ok(None);
    };
    let outcome = temps_docker_util::ensure_bridge_network(docker, name)
        .await
        .map_err(|e| {
            format!(
                "Failed to create Docker network '{name}' for the service container: {}",
                e.cause_text()
            )
        })?;
    if outcome != temps_docker_util::NetworkEnsured::Existing {
        tracing::info!(network = name, outcome = ?outcome, "Created the Temps app network for a managed service");
    }
    Ok(Some(name))
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

/// Build a Docker port binding for a service container, always bound to
/// this agent's resolved private/overlay address — never `"0.0.0.0"`, which
/// would make the service reachable on the node's public interface too.
/// `host_port: None` means auto-assign: let Docker pick a free host port.
fn service_port_binding(host_ip: &str, host_port: Option<u16>) -> bollard::models::PortBinding {
    bollard::models::PortBinding {
        host_ip: Some(host_ip.to_string()),
        host_port: host_port.map(|port| port.to_string()),
    }
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
        (status = 409, description = "A create for this service name is still running on this node"),
        (status = 500, description = "Service creation failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_service(
    State(state): State<Arc<AgentState>>,
    Json(request): Json<ServiceCreateRequest>,
) -> impl IntoResponse {
    let service_name = request.name.clone();
    // Run the create as its own task. When the caller gives up (its request
    // timeout during a long image pull, a dropped connection), the server
    // drops this handler future; a create running inside it would stop at
    // its next await and never remove what it had made. The task instead
    // sees `caller_waiting` dropped with the handler, stops, and undoes the
    // attempt: the caller has already counted it as failed (a new service's
    // record is rolled back), so a container left running would be one no
    // record points at, holding the name against a retry.
    let (caller_waiting, caller) = tokio::sync::watch::channel(());
    let outcome = tokio::spawn(run_service_create(state, request, CallerWatch(caller))).await;
    drop(caller_waiting);
    match outcome {
        Ok(response) => response,
        Err(e) => {
            tracing::error!(service = %service_name, error = %e, "Service create task ended abnormally");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Creating service '{}' ended before it finished: {}",
                    service_name, e
                ),
            )
            .into_response()
        }
    }
}

/// Create, start and report one service container, removing what this
/// attempt made on the node when a step fails. Runs detached from the request
/// (see [`create_service`]).
async fn run_service_create(
    state: Arc<AgentState>,
    request: ServiceCreateRequest,
    mut caller: CallerWatch,
) -> axum::response::Response {
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

    // One create per name at a time: a retry that arrives while an abandoned
    // attempt is still undoing itself would otherwise race it for the name
    // and the volumes.
    let Some(_in_progress) = CreateInProgress::claim(&container_name) else {
        tracing::warn!(service = %container_name, "Refusing a service create: another create for this name is still running");
        return error_response(
            StatusCode::CONFLICT,
            format!(
                "A create for service '{}' is still running on this node (an earlier attempt \
                 may be undoing itself); retry in a moment",
                container_name
            ),
        )
        .into_response();
    };
    // Marks the container this attempt creates, so an ambiguous create error
    // removes it only when it is this attempt's.
    let attempt = uuid::Uuid::new_v4().simple().to_string();

    // The volumes this request creates, so a failed create removes those and
    // never a volume an earlier service left holding its data.
    let created_volumes = create_service_volumes(docker, request.volumes.keys()).await;

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
                Some(vec![service_port_binding(&state.host_bind_address(), None)]),
            );
        } else {
            port_bindings.insert(
                container_port_key,
                Some(vec![service_port_binding(
                    &state.host_bind_address(),
                    Some(pm.host_port),
                )]),
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

    match ensure_service_network(
        docker,
        request.network.as_deref(),
        temps_core::NETWORK_NAME.as_str(),
    )
    .await
    {
        Ok(Some(app_network)) => {
            // Same guard the deployer applies to the app network on every
            // deploy; best-effort, never fails the service.
            if let Err(error) =
                temps_deployer::metadata_egress::apply_metadata_egress_block(docker, app_network)
                    .await
            {
                tracing::warn!(
                    network = app_network,
                    error = %error,
                    "Cloud-metadata egress block is incomplete; ensure nftables is installed and \
                     the agent has CAP_NET_ADMIN"
                );
            }
        }
        Ok(None) => {}
        Err(message) => {
            tracing::error!(service = %container_name, "{}", message);
            let message = with_cleanup_note(
                message,
                remove_failed_service_create(docker, None, &created_volumes).await,
            );
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, message).into_response();
        }
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
            (CREATE_ATTEMPT_LABEL.to_string(), attempt.clone()),
        ])),
        ..Default::default()
    };

    // Pull the image if not already present locally
    {
        let image_ref = request.image.as_str();
        tracing::info!(image = %image_ref, "Pulling image (if not present)...");

        // The pull is the long step; it stops as soon as the caller does.
        let pulled = tokio::select! {
            pulled = temps_docker_util::pull_image(docker, temps_docker_util::PullSpec::new(image_ref)) => pulled,
            () = caller.gone() => {
                return abandon_create(docker, &container_name, None, &created_volumes).await;
            }
        };
        if let Err(e) = pulled {
            let cause = e.cause_text();
            tracing::error!(image = %image_ref, "Failed to pull image: {}", cause);
            let message = with_cleanup_note(
                format!("Failed to pull image '{}': {}", image_ref, cause),
                remove_failed_service_create(docker, None, &created_volumes).await,
            );
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, message).into_response();
        }
        tracing::info!(image = %image_ref, "Image ready");
    }
    if caller.has_gone() {
        return abandon_create(docker, &container_name, None, &created_volumes).await;
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
                // Docker refuses a create under a name that is taken, so the
                // container is this request's: remove it, and the volumes it
                // created, so a retry finds the name free and no stray data.
                let message = with_cleanup_note(
                    format!("Container created but failed to start: {}", e),
                    remove_failed_service_create(docker, Some(&container_name), &created_volumes)
                        .await,
                );
                return error_response(StatusCode::INTERNAL_SERVER_ERROR, message).into_response();
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

            if caller.has_gone() {
                return abandon_create(
                    docker,
                    &container_name,
                    Some(&container_name),
                    &created_volumes,
                )
                .await;
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
            // A name conflict means the container is someone else's, and it
            // stays. Any other error can arrive after Docker made the
            // container (the reply lost on the way back), so remove one under
            // the name only when it carries this attempt's label.
            let ours = match &e {
                bollard::errors::Error::DockerResponseServerError {
                    status_code: 409, ..
                } => false,
                _ => made_by_attempt(docker, &container_name, &attempt).await,
            };
            let message = with_cleanup_note(
                format!(
                    "Failed to create service container '{}': {}",
                    container_name, e
                ),
                remove_failed_service_create(
                    docker,
                    ours.then_some(container_name.as_str()),
                    &created_volumes,
                )
                .await,
            );
            error_response(StatusCode::INTERNAL_SERVER_ERROR, message).into_response()
        }
    }
}

/// Label holding the create attempt that made a service container.
const CREATE_ATTEMPT_LABEL: &str = "sh.temps.service.create-attempt";

/// Whether the caller of a service create is still waiting for its answer.
/// The handler holds the sender; the server drops the handler, and with it
/// the sender, when the caller hangs up. Nothing is ever sent.
struct CallerWatch(tokio::sync::watch::Receiver<()>);

impl CallerWatch {
    fn has_gone(&self) -> bool {
        self.0.has_changed().is_err()
    }

    /// Resolves once the caller has hung up.
    async fn gone(&mut self) {
        while self.0.changed().await.is_ok() {}
    }
}

/// Names of the services with a create running on this agent. Entries live
/// only while their create runs, so the set holds at most one per create in
/// flight.
static CREATES_IN_PROGRESS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<String>>,
> = std::sync::LazyLock::new(Default::default);

/// This agent's claim on creating one service name; released on drop, on
/// every path out of the create.
struct CreateInProgress(String);

impl CreateInProgress {
    /// The claim on `name`, or `None` while another create holds it.
    fn claim(name: &str) -> Option<Self> {
        let mut names = CREATES_IN_PROGRESS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        names
            .insert(name.to_string())
            .then(|| Self(name.to_string()))
    }
}

impl Drop for CreateInProgress {
    fn drop(&mut self) {
        CREATES_IN_PROGRESS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.0);
    }
}

/// Whether the container named `name` was made by create attempt `attempt`.
async fn made_by_attempt(docker: &bollard::Docker, name: &str, attempt: &str) -> bool {
    match docker
        .inspect_container(name, None::<InspectContainerOptions>)
        .await
    {
        Ok(info) => {
            info.config
                .and_then(|config| config.labels)
                .and_then(|labels| labels.get(CREATE_ATTEMPT_LABEL).cloned())
                .as_deref()
                == Some(attempt)
        }
        Err(_) => false,
    }
}

/// Undo a create whose caller hung up: remove what it made and answer no
/// one. The caller already counted the create as failed.
async fn abandon_create(
    docker: &bollard::Docker,
    service: &str,
    container: Option<&str>,
    created_volumes: &[String],
) -> axum::response::Response {
    let left_behind = remove_failed_service_create(docker, container, created_volumes).await;
    tracing::warn!(
        service,
        left_behind = ?left_behind,
        "The caller stopped waiting for this service create; undid it on the node"
    );
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        with_cleanup_note(
            format!(
                "The caller stopped waiting for service '{}', so this create was undone",
                service
            ),
            left_behind,
        ),
    )
    .into_response()
}

/// Create the named volumes a service mounts, returning the ones this call
/// created. A volume that already exists belongs to an earlier service (a
/// re-create keeps its data on it), so it is never this request's to remove.
async fn create_service_volumes<'a>(
    docker: &bollard::Docker,
    names: impl IntoIterator<Item = &'a String>,
) -> Vec<String> {
    let mut created = Vec::new();
    for name in names {
        match docker.inspect_volume(name).await {
            Ok(_) => continue,
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {}
            Err(e) => {
                // Unknown: treat it as existing, so it is never removed.
                tracing::warn!(volume = %name, error = %e, "Could not inspect service volume; leaving it to Docker");
                continue;
            }
        }
        let create_opts = bollard::models::VolumeCreateRequest {
            name: Some(name.clone()),
            ..Default::default()
        };
        match docker.create_volume(create_opts).await {
            Ok(_) => created.push(name.clone()),
            Err(e) => {
                tracing::warn!(volume = %name, error = %e, "Volume creation returned an error");
            }
        }
    }
    created
}

/// Undo a failed `create_service` request: remove the container it created,
/// if it got that far, then the volumes it created. Nothing that existed
/// before the request is touched. Returns what could not be removed, for the
/// error the control plane reports; cleanup never replaces the original error.
async fn remove_failed_service_create(
    docker: &bollard::Docker,
    container: Option<&str>,
    created_volumes: &[String],
) -> Option<String> {
    let mut left_behind = Vec::new();
    if let Some(container) = container {
        // `v` drops the container's anonymous volumes, which only it used;
        // named volumes are never removed by it.
        let options = bollard::query_parameters::RemoveContainerOptionsBuilder::new()
            .force(true)
            .v(true)
            .build();
        match docker.remove_container(container, Some(options)).await {
            Ok(()) => tracing::info!(container, "Removed service container after a failed create"),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {}
            Err(e) => {
                tracing::warn!(container, error = %e, "Could not remove service container after a failed create");
                left_behind.push(format!("container '{container}' ({e})"));
            }
        }
    }
    for volume in created_volumes {
        match docker
            .remove_volume(
                volume,
                None::<bollard::query_parameters::RemoveVolumeOptions>,
            )
            .await
        {
            Ok(()) => {
                tracing::info!(volume = %volume, "Removed service volume after a failed create")
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {}
            Err(e) => {
                tracing::warn!(volume = %volume, error = %e, "Could not remove service volume after a failed create");
                left_behind.push(format!("volume '{volume}' ({e})"));
            }
        }
    }
    (!left_behind.is_empty()).then(|| left_behind.join(", "))
}

/// `message`, plus what a failed create could not clean up, if anything.
fn with_cleanup_note(message: String, left_behind: Option<String>) -> String {
    match left_behind {
        Some(left_behind) => format!(
            "{message}. Could not remove what this attempt created on the node: {left_behind}"
        ),
        None => message,
    }
}

/// Pull `image` unless it is present. The same pull `create_service` does.
async fn pull_service_image(docker: &bollard::Docker, image: &str) -> Result<(), String> {
    tracing::info!(image = %image, "Pulling image (if not present)...");
    temps_docker_util::pull_image(docker, temps_docker_util::PullSpec::new(image))
        .await
        .map_err(|e| format!("Failed to pull image '{}': {}", image, e.cause_text()))?;
    tracing::info!(image = %image, "Image ready");
    Ok(())
}

/// Where a database container this agent creates or recreates is attached:
/// ports on this node's bind address, the overlay's resolver when it is up.
pub(crate) fn node_attachment(state: &AgentState, network: Option<String>) -> NodeAttachment {
    NodeAttachment {
        host_ip: state.host_bind_address(),
        network,
        dns: state
            .overlay_bridge_address
            .read()
            .ok()
            .and_then(|slot| slot.as_ref().map(|ip| vec![ip.to_string()]))
            .map(temps_deployer::docker::dns_with_fallback),
    }
}

/// Create (or recreate) a database container from its admitted workload
/// spec (ADR-055 Phase 4, `enforce` mode).
///
/// Unlike `create_service`, the agent builds no request of its own: the
/// spec is admitted again here, and the container-create request is the
/// Docker driver's for the `Data` profile -- the same host config, restart
/// policy, healthcheck, log rotation and labels the control plane gives a
/// database. What stays node-local is only where the container attaches:
/// this agent's private address for the published port, the app network,
/// the overlay and this node's resolvers. With `replace_existing` the
/// running container is replaced through the recreate guard, which keeps
/// the original until the replacement is healthy on the same data.
///
/// Not part of the agent's OpenAPI document: the request carries the plane's
/// `WorkloadSpec`, which is an internal contract between the control plane
/// and its agents of the same release.
pub async fn create_service_workload(
    State(state): State<Arc<AgentState>>,
    Json(request): Json<WorkloadCreateBody>,
) -> impl IntoResponse {
    let Some(docker) = state.docker.as_ref() else {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Docker client not available on this agent".to_string(),
        )
        .into_response();
    };
    // Admission runs on both sides: a request is never trusted to have
    // been admitted by its sender.
    let plan = match state.service_runtime.admit_workload(request.plan) {
        Ok(plan) => plan,
        Err(e @ NodeServiceError::MalformedPlan { .. }) => {
            tracing::warn!(service = %request.service_name, error = %e, "Refused a malformed database workload plan");
            return error_response(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response();
        }
        Err(e) => {
            let container = match &e {
                NodeServiceError::Refused { container, .. } => container.as_str(),
                _ => "unknown",
            };
            tracing::warn!(container = %container, error = %e, "Refused a database workload spec");
            return error_response(StatusCode::BAD_REQUEST, e.to_string()).into_response();
        }
    };
    let container_name = plan.container_name().to_string();
    tracing::info!(
        service = %request.service_name,
        container = %container_name,
        engine = %plan.engine(),
        replace_existing = request.replace_existing,
        workload_mode = "enforce",
        "Creating database container from its workload spec"
    );

    for volume in plan.volumes() {
        let create_opts = bollard::models::VolumeCreateRequest {
            name: Some(volume.clone()),
            ..Default::default()
        };
        if let Err(e) = docker.create_volume(create_opts).await {
            tracing::warn!(
                volume = %volume,
                "Volume creation returned error (may already exist): {}",
                e
            );
        }
    }

    match ensure_service_network(
        docker,
        request.network.as_deref(),
        temps_core::NETWORK_NAME.as_str(),
    )
    .await
    {
        Ok(Some(app_network)) => {
            if let Err(error) =
                temps_deployer::metadata_egress::apply_metadata_egress_block(docker, app_network)
                    .await
            {
                tracing::warn!(
                    network = app_network,
                    error = %error,
                    "Cloud-metadata egress block is incomplete; ensure nftables is installed and \
                     the agent has CAP_NET_ADMIN"
                );
            }
        }
        Ok(None) => {}
        Err(message) => {
            tracing::error!(service = %container_name, "{}", message);
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, message).into_response();
        }
    }

    // Admission refuses anything but an OCI image.
    let image = plan.image().unwrap_or_default().to_string();
    if let Err(message) = pull_service_image(docker, &image).await {
        tracing::error!(container = %container_name, "{}", message);
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, message).into_response();
    }

    let node = node_attachment(&state, request.network.clone());
    let attach_overlay: temps_service_protocol::AfterCreate<'_> = Box::new(|id: String| {
        Box::pin(async move {
            if let Err(e) = attach_to_overlay_if_present(docker, &id).await {
                tracing::warn!(
                    container = %id,
                    error = %e,
                    "Failed to attach service container to overlay; continuing single-host"
                );
            }
        })
    });

    let created = if request.replace_existing {
        plan.replace(
            docker,
            &request.service_name,
            &request.secrets,
            &node,
            attach_overlay,
        )
        .await
    } else {
        plan.create(docker, &request.secrets, &node, attach_overlay)
            .await
    };
    if let Err(e) = created {
        tracing::error!(container = %container_name, error = %e, "Failed to create database container from its workload spec");
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    if let Err(e) =
        install_overlay_peer_routes_after_start(docker, &container_name, &state.overlay_peers).await
    {
        tracing::warn!(
            container = %container_name,
            error = %e,
            "Failed to install overlay peer routes; cross-worker traffic to other CIDRs will fail"
        );
    }

    let mut host_port = plan.host_port().unwrap_or(0);
    let mut compute_ip = None;
    let mut container_id = String::new();
    match docker
        .inspect_container(&container_name, None::<InspectContainerOptions>)
        .await
    {
        Ok(info) => {
            container_id = info.id.clone().unwrap_or_default();
            if host_port == 0 {
                host_port = info
                    .network_settings
                    .as_ref()
                    .and_then(|n| n.ports.as_ref())
                    .and_then(|ports| {
                        ports
                            .values()
                            .flatten()
                            .flatten()
                            .find_map(|b| b.host_port.as_deref()?.parse::<u16>().ok())
                    })
                    .unwrap_or(0);
            }
            compute_ip = extract_overlay_ip(&info);
        }
        Err(e) => tracing::warn!(
            container = %container_name,
            "Failed to inspect container after start: {}",
            e
        ),
    }
    tracing::info!(
        container = %container_name,
        container_id = %container_id,
        host_port,
        compute_ip = ?compute_ip,
        workload_mode = "enforce",
        "Database container created from its workload spec and started"
    );
    ok_response(ServiceCreateResponse {
        container_id,
        container_name,
        host_port,
        compute_ip,
    })
    .into_response()
}

/// Inspect a database container and its image (ADR-055 Phase 4): what the
/// control plane's shadow pass and pending-changes listing compare with the
/// database's spec. Read-only. A missing container is `found: false`, not
/// an error, so the caller can tell it from an agent without this route.
pub async fn service_snapshot(
    State(state): State<Arc<AgentState>>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let Some(docker) = state.docker.as_ref() else {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Docker client not available on this agent".to_string(),
        )
        .into_response();
    };
    match state
        .service_runtime
        .container_snapshot(docker, &name)
        .await
    {
        Ok(snapshot) => ok_response(snapshot).into_response(),
        Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
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
        ExecDeadlineOutcome::ContainerTerminated => {
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

    // Service ports are published on this node's bind address only, so the
    // admin connection goes there, as the health probe's does.
    let published_address = published_probe_address(&state.host_bind_address());

    match state
        .service_runtime
        .provision_runtime_environment(
            service_name.clone(),
            service_type,
            request.service_config,
            &request.project_slug,
            &request.environment_slug,
            docker,
            published_address,
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

/// Preview a project's runtime environment of a service this node runs, as
/// [`runtime_env`] would provision it, without creating, renaming or
/// provisioning anything. The console's per-environment preview of a worker
/// service comes here, so the control plane never builds an engine (or a
/// container) of its own for it.
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services/runtime-env-preview",
    request_body = RemoteRuntimeEnvRequest,
    responses(
        (status = 200, description = "Runtime environment previewed", body = AgentResponse<RemoteRuntimeEnvResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Preview failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn runtime_env_preview(
    State(state): State<Arc<AgentState>>,
    Json(request): Json<RemoteRuntimeEnvRequest>,
) -> impl IntoResponse {
    let service_name = request.service_config.name.clone();
    let service_type = request.service_config.service_type;
    tracing::debug!(
        service = %service_name,
        service_type = %service_type,
        project = %request.project_slug,
        environment = %request.environment_slug,
        "Previewing external-service runtime environment"
    );
    let Some(docker) = state.docker.as_ref().map(|docker| Arc::new(docker.clone())) else {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "Docker client is unavailable while previewing service '{}'",
                service_name
            ),
        )
        .into_response();
    };
    let published_address = published_probe_address(&state.host_bind_address());
    match state
        .service_runtime
        .preview_runtime_environment(
            service_name.clone(),
            service_type,
            request.service_config,
            &request.project_slug,
            &request.environment_slug,
            docker,
            published_address,
        )
        .await
    {
        Ok(environment) => ok_response(RemoteRuntimeEnvResponse { environment }).into_response(),
        Err(error) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "Failed to preview the runtime environment of service '{}': {}",
                service_name, error
            ),
        )
        .into_response(),
    }
}

/// Revoke a project environment's managed S3 credentials on the worker that
/// owns the storage engine. Mirrors `runtime_env`: the control plane sends the
/// stored service configuration and the tenant identity, nothing else.
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services/revoke-s3-access",
    request_body = RemoteRuntimeEnvRequest,
    responses(
        (status = 200, description = "Credentials revoked", body = AgentResponse<String>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Revocation failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn revoke_s3_access(
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
        "Revoking managed S3 credentials"
    );

    let docker = match state.docker.as_ref() {
        Some(docker) => Arc::new(docker.clone()),
        None => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Docker client is unavailable while revoking credentials for service '{}'",
                    service_name
                ),
            )
            .into_response();
        }
    };

    match state
        .service_runtime
        .revoke_runtime_environment(
            service_name.clone(),
            service_type,
            request.service_config,
            &request.project_slug,
            &request.environment_slug,
            docker,
        )
        .await
    {
        Ok(()) => ok_response("revoked".to_string()).into_response(),
        Err(error) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "Failed to revoke managed S3 credentials for service '{}': {}",
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

    // This node publishes managed-service ports on its bind address only
    // (never on all interfaces), so the probe has to connect there rather than
    // to the loopback host the service was created with.
    let published_address = published_probe_address(&state.host_bind_address());

    match state
        .service_runtime
        .probe_service_health(request.service_config, docker, published_address)
        .await
    {
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

/// The address this node publishes service ports on, when it is an IP.
/// The provider validates the caller-supplied host before substituting it.
pub(crate) fn published_probe_address(bind_address: &str) -> Option<std::net::IpAddr> {
    bind_address.trim().parse().ok()
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
        ExecDeadlineOutcome::ContainerTerminated => {
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
        ExecDeadlineOutcome::ContainerTerminated => {
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
pub(crate) async fn attach_to_overlay_if_present(
    docker: &bollard::Docker,
    container_id: &str,
) -> std::result::Result<(), bollard::errors::Error> {
    let overlay_name = temps_network::overlay_name::current().docker_network;

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
pub(crate) async fn install_overlay_peer_routes_after_start(
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

    let overlay_name = temps_network::overlay_name::current().docker_network;
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
/// is whatever `temps_network::overlay_name::current()` names (`temps0`
/// unless the control plane chose another, ADR 059).
fn extract_overlay_ip(info: &bollard::models::ContainerInspectResponse) -> Option<String> {
    let overlay_name = temps_network::overlay_name::current().docker_network;
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
        let overlay_name = temps_network::overlay_name::current().docker_network;
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
        let overlay_name = temps_network::overlay_name::current().docker_network;
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

#[cfg(test)]
mod service_port_binding_tests {
    use super::service_port_binding;

    // Regression guard for the exact bug this replaced: both port mappings
    // used a hardcoded "0.0.0.0" PortBinding, exposing service containers
    // (managed databases, Redis, etc.) on every interface of the worker
    // node instead of only the private/overlay one.
    #[test]
    fn never_binds_to_all_interfaces() {
        let fixed = service_port_binding("10.100.0.2", Some(5432));
        assert_eq!(fixed.host_ip.as_deref(), Some("10.100.0.2"));
        assert_ne!(fixed.host_ip.as_deref(), Some("0.0.0.0"));
        assert_eq!(fixed.host_port.as_deref(), Some("5432"));

        let auto_assign = service_port_binding("10.100.0.2", None);
        assert_eq!(auto_assign.host_ip.as_deref(), Some("10.100.0.2"));
        assert_ne!(auto_assign.host_ip.as_deref(), Some("0.0.0.0"));
        assert_eq!(auto_assign.host_port, None);
    }
}

#[cfg(test)]
mod service_network_tests {
    use super::ensure_service_network;

    async fn docker() -> Option<bollard::Docker> {
        let docker = match bollard::Docker::connect_with_local_defaults() {
            Ok(docker) => docker,
            Err(e) => {
                println!("Docker not available, skipping: {e}");
                return None;
            }
        };
        if docker.ping().await.is_err() {
            println!("Docker daemon not responding, skipping");
            return None;
        }
        Some(docker)
    }

    async fn network_exists(docker: &bollard::Docker, name: &str) -> bool {
        docker
            .inspect_network(
                name,
                None::<bollard::query_parameters::InspectNetworkOptions>,
            )
            .await
            .is_ok()
    }

    fn unique(prefix: &str) -> String {
        format!(
            "{prefix}-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        )
    }

    /// Regression: an HA cluster member placed on a worker that had never
    /// run a deployment failed with "network temps-app-network not found",
    /// because only the deployer ever created the app network.
    #[tokio::test]
    async fn creates_the_app_network_on_a_fresh_worker() {
        let Some(docker) = docker().await else {
            return;
        };
        let app_network = unique("temps-agent-net-it");
        assert!(!network_exists(&docker, &app_network).await);

        let first = ensure_service_network(&docker, Some(&app_network), &app_network).await;
        let created = network_exists(&docker, &app_network).await;
        // Idempotent: a second service on the same worker finds it in place.
        let second = ensure_service_network(&docker, Some(&app_network), &app_network).await;
        let _ = docker.remove_network(&app_network).await;

        assert_eq!(first, Ok(Some(app_network.as_str())));
        assert!(
            created,
            "the app network must exist after the first service"
        );
        assert_eq!(second, Ok(Some(app_network.as_str())));
    }

    /// Only the Temps app network is ever created; any other requested name
    /// is left to Docker, and no network at all is a no-op.
    #[tokio::test]
    async fn never_creates_other_networks() {
        let Some(docker) = docker().await else {
            return;
        };
        let app_network = unique("temps-agent-net-it");
        let other = unique("temps-agent-other-it");

        let result = ensure_service_network(&docker, Some(&other), &app_network).await;
        let other_created = network_exists(&docker, &other).await;
        let none = ensure_service_network(&docker, None, &app_network).await;
        let app_created = network_exists(&docker, &app_network).await;
        if other_created {
            let _ = docker.remove_network(&other).await;
        }
        if app_created {
            let _ = docker.remove_network(&app_network).await;
        }

        assert_eq!(result, Ok(None));
        assert!(
            !other_created,
            "a network other than the app network must not be created"
        );
        assert_eq!(none, Ok(None));
        assert!(!app_created, "no requested network must create nothing");
    }
}

#[cfg(test)]
mod health_probe_target_tests {
    use super::published_probe_address;
    use temps_providers::externalsvc::{
        probe_service_health, ServiceConfig, ServiceHealthProbeError, ServiceType,
    };

    #[test]
    fn health_probe_uses_the_bind_address_only_when_it_is_an_ip() {
        assert_eq!(
            published_probe_address(" 10.42.0.22 "),
            Some("10.42.0.22".parse().unwrap())
        );
        assert_eq!(published_probe_address("not-an-ip"), None);
        assert_eq!(published_probe_address(""), None);
    }

    /// Issue #1772: on a worker that joined with a private address, the probe
    /// was retargeted at that address and then rejected for not being
    /// loopback, so every service on the node read as down. Run the handler's
    /// real path against an unreachable Docker daemon: the probe must get past
    /// target validation and fail only at the container lookup.
    #[tokio::test]
    async fn health_probe_on_a_private_address_worker_passes_target_validation() {
        let config = ServiceConfig {
            name: "orders-cache".to_string(),
            service_type: ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({"host": "localhost", "port": "6379"}),
        };
        let docker = std::sync::Arc::new(
            bollard::Docker::connect_with_http(
                "http://127.0.0.1:1",
                1,
                bollard::API_DEFAULT_VERSION,
            )
            .expect("an HTTP Docker client is built without connecting"),
        );

        let result =
            probe_service_health(config, docker, published_probe_address("10.42.0.22")).await;

        assert!(
            matches!(result, Err(ServiceHealthProbeError::Configuration(_))),
            "expected the unreachable daemon to fail the container lookup, got {result:?}"
        );
    }
}

#[cfg(test)]
mod injected_engine_tests {
    use super::*;
    use crate::test_engines::{agent_state, plugin_builder, Admission, StubRuntime};
    use axum::body::to_bytes;

    fn workload_body() -> WorkloadCreateBody {
        WorkloadCreateBody {
            service_name: "orders".to_string(),
            plan: serde_json::json!({ "container_name": "temps-db" }),
            secrets: Default::default(),
            network: None,
            replace_existing: false,
        }
    }

    async fn body_text(response: axum::response::Response) -> String {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read the response");
        String::from_utf8_lossy(&bytes).to_string()
    }

    /// The plan reaches admission before anything touches Docker, and a
    /// refusal is the client's error, with the engine's reason.
    #[tokio::test]
    async fn a_refused_plan_is_a_400_with_the_reason() {
        let runtime = Arc::new(StubRuntime::default());
        let response = create_service_workload(
            State(agent_state(runtime.clone(), plugin_builder())),
            Json(workload_body()),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(body_text(response).await.contains("refused: privileged"));
        assert_eq!(
            runtime.admitted.lock().expect("admitted").as_slice(),
            [serde_json::json!({ "container_name": "temps-db" })]
        );
    }

    #[tokio::test]
    async fn a_malformed_plan_is_a_422() {
        let runtime = Arc::new(StubRuntime {
            admission: Admission::Malformed,
            ..StubRuntime::default()
        });
        let response = create_service_workload(
            State(agent_state(runtime, plugin_builder())),
            Json(workload_body()),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    /// Engine failures keep their text: the control plane shows it.
    #[tokio::test]
    async fn an_engine_failure_on_a_snapshot_is_a_500_with_the_engine_text() {
        let response = service_snapshot(
            State(agent_state(
                Arc::new(StubRuntime::default()),
                plugin_builder(),
            )),
            Path("temps-db".to_string()),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body_text(response)
            .await
            .contains("snapshot failed in the test engine"));
    }

    /// The live failure: a deployment linked to a worker's Postgres never
    /// started, because provisioning its database connected to
    /// `localhost:<port>` while the node publishes the port on its bind
    /// address only. Provisioning now gets that address, as the health and
    /// WAL probes do.
    #[tokio::test]
    async fn runtime_provisioning_connects_through_the_node_bind_address() {
        let runtime = Arc::new(StubRuntime::default());
        let state = agent_state(runtime.clone(), plugin_builder());
        *state.host_bind_address.write().expect("bind address") = "10.42.0.22".to_string();
        let request = temps_service_protocol::RemoteRuntimeEnvRequest {
            service_config: temps_service_protocol::ServiceConfig {
                name: "orders".to_string(),
                service_type: temps_service_protocol::ServiceType::Postgres,
                version: None,
                parameters: serde_json::json!({"host": "localhost", "port": "5561"}),
            },
            project_slug: "shop".to_string(),
            environment_slug: "production".to_string(),
        };

        let response = runtime_env(State(state), Json(request))
            .await
            .into_response();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            runtime
                .provisioned_through
                .lock()
                .expect("provisioned_through")
                .as_slice(),
            [Some(
                "10.42.0.22"
                    .parse::<std::net::IpAddr>()
                    .expect("an address")
            )]
        );
    }

    /// A preview reaches the engine's preview (here the stub), through the
    /// node's bind address like provisioning, and its failure names the
    /// service and says it was a preview.
    #[tokio::test]
    async fn a_runtime_preview_runs_on_this_node_through_its_bind_address() {
        let runtime = Arc::new(StubRuntime::default());
        let state = agent_state(runtime.clone(), plugin_builder());
        *state.host_bind_address.write().expect("bind address") = "10.42.0.22".to_string();
        let request = temps_service_protocol::RemoteRuntimeEnvRequest {
            service_config: temps_service_protocol::ServiceConfig {
                name: "orders".to_string(),
                service_type: temps_service_protocol::ServiceType::Postgres,
                version: None,
                parameters: serde_json::json!({"host": "localhost", "port": "5561"}),
            },
            project_slug: "shop".to_string(),
            environment_slug: "production".to_string(),
        };

        let response = runtime_env_preview(State(state), Json(request))
            .await
            .into_response();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let text = body_text(response).await;
        assert!(
            text.contains("Failed to preview the runtime environment of service 'orders'"),
            "{text}"
        );
        assert!(text.contains("preview"), "the stub's preview ran: {text}");
        assert_eq!(
            runtime
                .provisioned_through
                .lock()
                .expect("provisioned_through")
                .as_slice(),
            [Some(
                "10.42.0.22"
                    .parse::<std::net::IpAddr>()
                    .expect("an address")
            )]
        );
    }
}

#[cfg(test)]
mod failed_create_cleanup_tests {
    use super::{create_service_volumes, remove_failed_service_create, with_cleanup_note};
    use std::collections::HashMap;

    async fn docker() -> Option<bollard::Docker> {
        let docker = match bollard::Docker::connect_with_local_defaults() {
            Ok(docker) => docker,
            Err(e) => {
                println!("Docker not available, skipping: {e}");
                return None;
            }
        };
        if docker.ping().await.is_err() {
            println!("Docker daemon not responding, skipping");
            return None;
        }
        Some(docker)
    }

    fn unique(prefix: &str) -> String {
        format!(
            "{prefix}-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        )
    }

    async fn volume_exists(docker: &bollard::Docker, name: &str) -> bool {
        docker.inspect_volume(name).await.is_ok()
    }

    async fn container_exists(docker: &bollard::Docker, name: &str) -> bool {
        docker
            .inspect_container(
                name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .is_ok()
    }

    /// Regression: a worker that created a service container but could not
    /// start it (its port bound to an address the host does not have) left
    /// the container and its new volume behind after the control plane
    /// rolled the service back. Cleanup removes exactly what that attempt
    /// created, and never a volume that was there before it.
    #[tokio::test]
    async fn a_failed_start_removes_only_what_the_attempt_created() {
        let Some(docker) = docker().await else {
            return;
        };
        let existing = unique("temps-agent-create-it-existing");
        let fresh = unique("temps-agent-create-it-fresh");
        let container = unique("temps-agent-create-it");
        docker
            .create_volume(bollard::models::VolumeCreateRequest {
                name: Some(existing.clone()),
                ..Default::default()
            })
            .await
            .expect("create the pre-existing volume");

        let created = create_service_volumes(&docker, [&existing, &fresh]).await;

        let image = "busybox:1.36";
        temps_docker_util::pull_image(&docker, temps_docker_util::PullSpec::new(image))
            .await
            .expect("pull the test image");
        let host_config = bollard::models::HostConfig {
            binds: Some(vec![
                format!("{existing}:/existing"),
                format!("{fresh}:/fresh"),
            ]),
            // TEST-NET-3: no host has it, so the start fails after the
            // container exists, as on a worker whose private address is not
            // on the Docker host.
            port_bindings: Some(HashMap::from([(
                "8080/tcp".to_string(),
                Some(vec![bollard::models::PortBinding {
                    host_ip: Some("203.0.113.7".to_string()),
                    host_port: None,
                }]),
            )])),
            ..Default::default()
        };
        docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&container)
                        .build(),
                ),
                bollard::models::ContainerCreateBody {
                    image: Some(image.to_string()),
                    cmd: Some(vec!["sleep".to_string(), "60".to_string()]),
                    exposed_ports: Some(vec!["8080/tcp".to_string()]),
                    host_config: Some(host_config),
                    ..Default::default()
                },
            )
            .await
            .expect("create the test container");
        let started = docker
            .start_container(
                &container,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await;

        let left_behind = remove_failed_service_create(&docker, Some(&container), &created).await;
        let container_left = container_exists(&docker, &container).await;
        let fresh_left = volume_exists(&docker, &fresh).await;
        let existing_kept = volume_exists(&docker, &existing).await;
        // Clean up whatever the assertions below would otherwise leak.
        let _ = docker
            .remove_container(
                &container,
                Some(
                    bollard::query_parameters::RemoveContainerOptionsBuilder::new()
                        .force(true)
                        .build(),
                ),
            )
            .await;
        let _ = docker
            .remove_volume(
                &fresh,
                None::<bollard::query_parameters::RemoveVolumeOptions>,
            )
            .await;
        let _ = docker
            .remove_volume(
                &existing,
                None::<bollard::query_parameters::RemoveVolumeOptions>,
            )
            .await;

        assert_eq!(
            created,
            vec![fresh.clone()],
            "only the new volume is this attempt's"
        );
        assert!(
            started.is_err(),
            "the start must fail on an unassignable address"
        );
        assert_eq!(left_behind, None);
        assert!(!container_left, "the failed container must be removed");
        assert!(
            !fresh_left,
            "the volume this attempt created must be removed"
        );
        assert!(existing_kept, "a volume that existed before must be kept");
    }

    /// Cleaning up after an attempt that never created its container, or
    /// whose volume is already gone, is not an error.
    #[tokio::test]
    async fn cleanup_tolerates_what_is_already_gone() {
        let Some(docker) = docker().await else {
            return;
        };
        let left_behind = remove_failed_service_create(
            &docker,
            Some(&unique("temps-agent-create-it-missing")),
            &[unique("temps-agent-create-it-missing-volume")],
        )
        .await;
        assert_eq!(left_behind, None);
    }

    #[test]
    fn the_original_error_leads_and_leftovers_are_named() {
        assert_eq!(
            with_cleanup_note("Container created but failed to start: x".to_string(), None),
            "Container created but failed to start: x"
        );
        assert_eq!(
            with_cleanup_note(
                "Container created but failed to start: x".to_string(),
                Some("volume 'postgres-orders_data' (in use)".to_string())
            ),
            "Container created but failed to start: x. Could not remove what this attempt \
             created on the node: volume 'postgres-orders_data' (in use)"
        );
    }

    /// How the stub daemon answers the image pull.
    #[derive(Clone, Copy)]
    enum Pull {
        /// Never answers, as a long pull does.
        Hang,
        Succeed,
    }

    /// How the stub daemon answers creating the container.
    #[derive(Clone, Copy)]
    enum ContainerCreate {
        Created,
        /// Makes the container, then fails the reply: the create's own
        /// container, under an error.
        FailAfterCreating,
        /// Fails, and a container under the name carries another attempt's
        /// label.
        FailWithAnotherAttemptsContainer,
        /// The name is taken (409).
        NameTaken,
    }

    /// A stub Docker daemon for the create path. Every volume starts
    /// missing, and every request is recorded in `events`.
    struct StubDaemon {
        pull: Pull,
        container_create: ContainerCreate,
        /// Hold the container start until `release_start`.
        hang_start: bool,
        release_start: tokio::sync::Notify,
        events: std::sync::Mutex<Vec<String>>,
        /// Labels of the container under the requested name, once there is one.
        container_labels: std::sync::Mutex<Option<serde_json::Value>>,
    }

    impl StubDaemon {
        fn new(pull: Pull, container_create: ContainerCreate, hang_start: bool) -> Self {
            Self {
                pull,
                container_create,
                hang_start,
                release_start: tokio::sync::Notify::new(),
                events: Default::default(),
                container_labels: Default::default(),
            }
        }

        fn record(&self, event: String) {
            self.events.lock().expect("events").push(event);
        }

        fn events(&self) -> Vec<String> {
            self.events.lock().expect("events").clone()
        }

        fn count(&self, prefix: &str) -> usize {
            self.events()
                .iter()
                .filter(|event| event.starts_with(prefix))
                .count()
        }

        /// Wait until `count` requests starting with `prefix` have arrived.
        async fn wait_for(&self, prefix: &str, count: usize) {
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                while self.count(prefix) < count {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("no '{prefix}' request #{count}; saw {:?}", self.events()));
        }
    }

    async fn stub_daemon(
        axum::extract::State(daemon): axum::extract::State<std::sync::Arc<StubDaemon>>,
        method: axum::http::Method,
        uri: axum::http::Uri,
        body: axum::body::Bytes,
    ) -> axum::response::Response {
        use axum::http::{Method, StatusCode};
        use axum::response::IntoResponse;
        // A versioned client prefixes the path with the API version (`v1.47/`).
        let path = uri.path().trim_start_matches('/');
        let path = match path.split_once('/') {
            Some((version, rest))
                if version.starts_with('v')
                    && version[1..].chars().all(|c| c.is_ascii_digit() || c == '.') =>
            {
                rest
            }
            _ => path,
        };
        let message = |text: &str| axum::Json(serde_json::json!({ "message": text }));
        match (method, path) {
            (Method::POST, "volumes/create") => {
                daemon.record("create volume".to_string());
                (
                    StatusCode::CREATED,
                    axum::Json(serde_json::json!({
                        "Name": "created", "Driver": "local", "Mountpoint": "/stub",
                        "Labels": {}, "Scope": "local", "Options": {}
                    })),
                )
                    .into_response()
            }
            (Method::GET, p) if p.starts_with("volumes/") => {
                daemon.record(format!("inspect volume {}", &p["volumes/".len()..]));
                (StatusCode::NOT_FOUND, message("no such volume")).into_response()
            }
            (Method::DELETE, p) if p.starts_with("volumes/") => {
                daemon.record(format!("remove volume {}", &p["volumes/".len()..]));
                StatusCode::NO_CONTENT.into_response()
            }
            (Method::POST, "images/create") => {
                daemon.record("pull".to_string());
                match daemon.pull {
                    Pull::Hang => std::future::pending().await,
                    Pull::Succeed => {
                        (StatusCode::OK, "{\"status\":\"Downloaded\"}\n").into_response()
                    }
                }
            }
            (Method::GET, "networks") => {
                (StatusCode::OK, axum::Json(serde_json::json!([]))).into_response()
            }
            (Method::POST, "containers/create") => {
                daemon.record("create container".to_string());
                let labels = serde_json::from_slice::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|body| body.get("Labels").cloned());
                match daemon.container_create {
                    ContainerCreate::Created => {
                        *daemon.container_labels.lock().expect("labels") = labels;
                        (
                            StatusCode::CREATED,
                            axum::Json(
                                serde_json::json!({ "Id": "stub-container", "Warnings": [] }),
                            ),
                        )
                            .into_response()
                    }
                    ContainerCreate::FailAfterCreating => {
                        *daemon.container_labels.lock().expect("labels") = labels;
                        (StatusCode::INTERNAL_SERVER_ERROR, message("reply lost")).into_response()
                    }
                    ContainerCreate::FailWithAnotherAttemptsContainer => {
                        *daemon.container_labels.lock().expect("labels") = Some(
                            serde_json::json!({ super::CREATE_ATTEMPT_LABEL: "another-attempt" }),
                        );
                        (StatusCode::INTERNAL_SERVER_ERROR, message("reply lost")).into_response()
                    }
                    ContainerCreate::NameTaken => (
                        StatusCode::CONFLICT,
                        message("container name already in use"),
                    )
                        .into_response(),
                }
            }
            (Method::POST, p) if p.starts_with("containers/") && p.ends_with("/start") => {
                daemon.record("start container".to_string());
                if daemon.hang_start {
                    daemon.release_start.notified().await;
                }
                StatusCode::NO_CONTENT.into_response()
            }
            (Method::GET, p) if p.starts_with("containers/") && p.ends_with("/json") => {
                daemon.record("inspect container".to_string());
                match daemon.container_labels.lock().expect("labels").clone() {
                    Some(labels) => (
                        StatusCode::OK,
                        axum::Json(serde_json::json!({
                            "Id": "stub-container", "Config": { "Labels": labels }
                        })),
                    )
                        .into_response(),
                    None => (StatusCode::NOT_FOUND, message("no such container")).into_response(),
                }
            }
            (Method::DELETE, p) if p.starts_with("containers/") => {
                daemon.record(format!("remove container {}", &p["containers/".len()..]));
                StatusCode::NO_CONTENT.into_response()
            }
            _ => (StatusCode::NOT_FOUND, message("not stubbed")).into_response(),
        }
    }

    /// Serve `daemon` and return agent state whose Docker client talks to it.
    async fn agent_on(
        daemon: &std::sync::Arc<StubDaemon>,
    ) -> (
        std::sync::Arc<crate::handlers::AgentState>,
        tokio::task::JoinHandle<std::io::Result<()>>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the stub daemon");
        let address = listener.local_addr().expect("stub daemon address");
        let app = axum::Router::new()
            .fallback(stub_daemon)
            .with_state(daemon.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let docker = bollard::Docker::connect_with_http(
            &format!("http://{address}"),
            30,
            bollard::API_DEFAULT_VERSION,
        )
        .expect("client for the stub daemon");
        let state = crate::test_engines::agent_state_with_docker(
            crate::test_engines::service_runtime(),
            crate::test_engines::plugin_builder(),
            docker,
        );
        (state, server)
    }

    fn create_request(name: &str, volume: &str) -> crate::ServiceCreateRequest {
        crate::ServiceCreateRequest {
            name: name.to_string(),
            service_type: "postgres".to_string(),
            image: "stub/postgres:1".to_string(),
            environment: HashMap::new(),
            port_mappings: Vec::new(),
            volumes: HashMap::from([(volume.to_string(), "/var/lib/postgresql/data".to_string())]),
            network: None,
            command: None,
            resource_limits: None,
        }
    }

    /// Start the create handler as the server would, on its own task, so the
    /// test can drop it the way the server does when the caller hangs up.
    fn start_create(
        state: std::sync::Arc<crate::handlers::AgentState>,
        request: crate::ServiceCreateRequest,
    ) -> tokio::task::JoinHandle<axum::response::Response> {
        tokio::spawn(async move {
            use axum::response::IntoResponse;
            super::create_service(axum::extract::State(state), axum::Json(request))
                .await
                .into_response()
        })
    }

    /// Regression: the control plane gives up on a create after its request
    /// timeout, which a long image pull outlasts, and rolls back the
    /// service's record. The create must stop there and remove the volume it
    /// made, not finish later into a container no record points at, and not
    /// leave a volume a retry would keep as an earlier service's.
    #[tokio::test]
    async fn a_create_whose_caller_hangs_up_mid_pull_is_undone() {
        let daemon =
            std::sync::Arc::new(StubDaemon::new(Pull::Hang, ContainerCreate::Created, false));
        let (state, server) = agent_on(&daemon).await;
        let volume = unique("postgres-abandoned_data");

        let handler = start_create(
            state,
            create_request(&unique("postgres-abandoned"), &volume),
        );
        daemon.wait_for("pull", 1).await;
        // What the server does when the caller hangs up: drop the handler.
        handler.abort();
        let _ = handler.await;

        daemon.wait_for(&format!("remove volume {volume}"), 1).await;
        assert_eq!(daemon.count("create container"), 0, "{:?}", daemon.events());
        server.abort();
    }

    /// Regression: a caller that hangs up while the container is starting
    /// leaves no running container behind; the create removes it and the
    /// volume it made.
    #[tokio::test]
    async fn a_create_whose_caller_hangs_up_after_the_container_exists_removes_it() {
        let daemon = std::sync::Arc::new(StubDaemon::new(
            Pull::Succeed,
            ContainerCreate::Created,
            true,
        ));
        let (state, server) = agent_on(&daemon).await;
        let name = unique("postgres-started");
        let volume = unique("postgres-started_data");

        let handler = start_create(state, create_request(&name, &volume));
        daemon.wait_for("start container", 1).await;
        handler.abort();
        let _ = handler.await;
        daemon.release_start.notify_one();

        daemon
            .wait_for(&format!("remove container {name}"), 1)
            .await;
        daemon.wait_for(&format!("remove volume {volume}"), 1).await;
        server.abort();
    }

    /// Regression: a retry that arrives while an earlier create for the same
    /// name is still running is refused, rather than racing it for the name
    /// and the volumes; once the earlier one ends, a create goes ahead.
    #[tokio::test]
    async fn a_second_create_for_a_name_is_refused_while_the_first_runs() {
        let daemon =
            std::sync::Arc::new(StubDaemon::new(Pull::Hang, ContainerCreate::Created, false));
        let (state, server) = agent_on(&daemon).await;
        let name = unique("postgres-retried");
        let volume = unique("postgres-retried_data");

        let first = start_create(state.clone(), create_request(&name, &volume));
        daemon.wait_for("pull", 1).await;

        // Refused at once; without the claim it would join the hung pull.
        let refused = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            start_create(state.clone(), create_request(&name, &volume)),
        )
        .await
        .expect("the second create is refused, not run")
        .expect("second create answers");
        assert_eq!(refused.status(), axum::http::StatusCode::CONFLICT);
        assert_eq!(daemon.count("inspect volume"), 1, "{:?}", daemon.events());

        first.abort();
        let _ = first.await;
        daemon.wait_for(&format!("remove volume {volume}"), 1).await;
        // The claim is released with the undo, so the retry now proceeds.
        let retry = start_create(state, create_request(&name, &volume));
        daemon.wait_for("pull", 2).await;
        retry.abort();
        server.abort();
    }

    /// Regression: a create error can arrive after Docker made the container
    /// (the reply lost on the way back). The container this attempt made is
    /// removed; one that is another attempt's, or that held the name before
    /// (a 409), stays.
    #[tokio::test]
    async fn an_ambiguous_create_error_removes_only_this_attempts_container() {
        for (outcome, removed) in [
            (ContainerCreate::FailAfterCreating, true),
            (ContainerCreate::FailWithAnotherAttemptsContainer, false),
            (ContainerCreate::NameTaken, false),
        ] {
            let daemon = std::sync::Arc::new(StubDaemon::new(Pull::Succeed, outcome, false));
            let (state, server) = agent_on(&daemon).await;
            let name = unique("postgres-ambiguous");
            let volume = unique("postgres-ambiguous_data");

            let response = start_create(state, create_request(&name, &volume))
                .await
                .expect("create answers");
            assert_eq!(
                response.status(),
                axum::http::StatusCode::INTERNAL_SERVER_ERROR
            );
            assert_eq!(
                daemon.count(&format!("remove container {name}")),
                usize::from(removed),
                "{:?}",
                daemon.events()
            );
            assert_eq!(daemon.count(&format!("remove volume {volume}")), 1);
            server.abort();
        }
    }
}
