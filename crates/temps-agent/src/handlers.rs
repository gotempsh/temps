// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP handlers for the agent API.
//!
//! These wrap the local `ContainerDeployer` and `ImageBuilder` traits,
//! exposing them over HTTP for remote control from the control plane.

use axum::{
    body::Body,
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Extension, Path, Query, State,
    },
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use bollard::exec::StartExecResults;
use bollard::query_parameters::LogsOptions;
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use temps_deployer::{ContainerDeployer, DeployRequest, ImageBuilder};
use tokio::io::AsyncWriteExt;
use utoipa::{OpenApi, ToSchema};

use crate::exec_timeout::{
    completed_exec_exit_code, exec_start_was_rejected, monitor_exec_until_stopped,
    resolve_container_id, resolve_exec_container_id, run_exec_with_deadline,
    ContainerIdentityError, ExecCleanupGuard, ExecCompletionError, ExecDeadlineOutcome,
};
use crate::output_buffer::{
    json_response_with_capture_permit, BoundedTailBuffer, MAX_CAPTURED_STREAM_BYTES,
};
use crate::NodeHealthReport;

pub(crate) const MAX_CONCURRENT_OUTPUT_CAPTURES: usize = 4;
pub(crate) const MAX_CONCURRENT_EXEC_OPERATIONS: usize = 4;
pub(crate) const MAX_CONCURRENT_IMAGE_IMPORTS: usize = 1;
const MAX_IMAGE_IMPORT_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const IMAGE_IMPORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);
const CONTAINER_LOG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

pub(crate) struct AttachedExecPermits {
    pub(crate) operation: tokio::sync::OwnedSemaphorePermit,
    pub(crate) capture: tokio::sync::OwnedSemaphorePermit,
    pub(crate) container: tokio::sync::OwnedSemaphorePermit,
}

pub(crate) struct ExecLifecyclePermits {
    pub(crate) operation: tokio::sync::OwnedSemaphorePermit,
    pub(crate) container: tokio::sync::OwnedSemaphorePermit,
}

#[derive(Debug, PartialEq)]
pub(crate) enum ExecAdmissionError {
    OperationsExhausted,
    CapturesExhausted,
    ContainerOwned,
}

#[derive(Default)]
struct ContainerOperationRegistry {
    slots: parking_lot::Mutex<HashMap<String, Weak<tokio::sync::Semaphore>>>,
}

impl ContainerOperationRegistry {
    fn try_acquire(
        &self,
        container_id: &str,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, ExecAdmissionError> {
        let slot = {
            let mut slots = self.slots.lock();
            slots.retain(|_, slot| slot.strong_count() > 0);
            if let Some(slot) = slots.get(container_id).and_then(Weak::upgrade) {
                slot
            } else {
                let slot = Arc::new(tokio::sync::Semaphore::new(1));
                slots.insert(container_id.to_string(), Arc::downgrade(&slot));
                slot
            }
        };
        slot.try_acquire_owned()
            .map_err(|_| ExecAdmissionError::ContainerOwned)
    }
}

/// Per-agent limits live in router extensions rather than public `AgentState`
/// fields. This keeps synchronization internals out of the crate's public
/// state construction API.
pub struct AgentResourceLimits {
    output_capture_slots: Arc<tokio::sync::Semaphore>,
    exec_operation_slots: Arc<tokio::sync::Semaphore>,
    image_import_slots: Arc<tokio::sync::Semaphore>,
    container_operations: ContainerOperationRegistry,
}

impl AgentResourceLimits {
    pub fn new() -> Self {
        Self {
            output_capture_slots: Arc::new(tokio::sync::Semaphore::new(
                MAX_CONCURRENT_OUTPUT_CAPTURES,
            )),
            exec_operation_slots: Arc::new(tokio::sync::Semaphore::new(
                MAX_CONCURRENT_EXEC_OPERATIONS,
            )),
            image_import_slots: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_IMAGE_IMPORTS)),
            container_operations: ContainerOperationRegistry::default(),
        }
    }

    #[cfg(test)]
    fn with_capacities(operations: usize, captures: usize, imports: usize) -> Self {
        Self {
            output_capture_slots: Arc::new(tokio::sync::Semaphore::new(captures)),
            exec_operation_slots: Arc::new(tokio::sync::Semaphore::new(operations)),
            image_import_slots: Arc::new(tokio::sync::Semaphore::new(imports)),
            container_operations: ContainerOperationRegistry::default(),
        }
    }
}

impl Default for AgentResourceLimits {
    fn default() -> Self {
        Self::new()
    }
}

/// Reserve lifecycle capacity before memory capacity and before any Docker
/// exec allocation. This keeps cleanup outages from consuming log-capture
/// slots or accumulating unstarted Docker exec metadata.
pub(crate) fn try_acquire_attached_exec_permits(
    limits: &AgentResourceLimits,
    container_id: &str,
) -> Result<AttachedExecPermits, ExecAdmissionError> {
    let operation = limits
        .exec_operation_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| ExecAdmissionError::OperationsExhausted)?;
    let container = limits.container_operations.try_acquire(container_id)?;
    let capture = limits
        .output_capture_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| ExecAdmissionError::CapturesExhausted)?;
    Ok(AttachedExecPermits {
        operation,
        capture,
        container,
    })
}

pub(crate) fn try_acquire_exec_lifecycle_permits(
    limits: &AgentResourceLimits,
    container_id: &str,
) -> Result<ExecLifecyclePermits, ExecAdmissionError> {
    let operation = limits
        .exec_operation_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| ExecAdmissionError::OperationsExhausted)?;
    let container = limits.container_operations.try_acquire(container_id)?;
    Ok(ExecLifecyclePermits {
        operation,
        container,
    })
}

#[cfg(test)]
mod exec_admission_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn exhausted_operation_capacity_rejects_before_capture_or_docker_allocation() {
        let limits = AgentResourceLimits::with_capacities(0, 1, 1);
        let docker_allocations = AtomicUsize::new(0);

        let admission = try_acquire_attached_exec_permits(&limits, "container-a");
        if admission.is_ok() {
            docker_allocations.fetch_add(1, Ordering::SeqCst);
        }

        assert!(matches!(
            admission,
            Err(ExecAdmissionError::OperationsExhausted)
        ));
        assert_eq!(limits.output_capture_slots.available_permits(), 1);
        assert_eq!(docker_allocations.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn capture_rejection_returns_the_reserved_operation_slot() {
        let limits = AgentResourceLimits::with_capacities(1, 0, 1);

        let admission = try_acquire_attached_exec_permits(&limits, "container-a");

        assert!(matches!(
            admission,
            Err(ExecAdmissionError::CapturesExhausted)
        ));
        assert_eq!(limits.exec_operation_slots.available_permits(), 1);
        assert!(limits
            .container_operations
            .try_acquire("container-a")
            .is_ok());
    }

    #[test]
    fn same_container_is_single_flight_while_other_containers_can_run() {
        let limits = AgentResourceLimits::with_capacities(3, 3, 1);
        let first = try_acquire_attached_exec_permits(&limits, "container-a")
            .expect("first container operation is admitted");

        assert!(matches!(
            try_acquire_attached_exec_permits(&limits, "container-a"),
            Err(ExecAdmissionError::ContainerOwned)
        ));
        assert!(try_acquire_attached_exec_permits(&limits, "container-b").is_ok());

        drop(first);
        assert!(try_acquire_attached_exec_permits(&limits, "container-a").is_ok());
    }

    #[test]
    fn detached_exec_reserves_operation_and_container_lifecycle_capacity() {
        let limits = AgentResourceLimits::with_capacities(1, 1, 1);
        let detached = try_acquire_exec_lifecycle_permits(&limits, "container-a")
            .expect("first detached operation is admitted");

        assert_eq!(limits.exec_operation_slots.available_permits(), 0);
        assert!(matches!(
            try_acquire_exec_lifecycle_permits(&limits, "container-a"),
            Err(ExecAdmissionError::OperationsExhausted)
        ));
        assert_eq!(limits.output_capture_slots.available_permits(), 1);

        drop(detached);
        assert!(try_acquire_exec_lifecycle_permits(&limits, "container-a").is_ok());
    }
}

/// Shared state for all agent handlers.
pub struct AgentState {
    pub container_deployer: Arc<dyn ContainerDeployer>,
    pub image_builder: Arc<dyn ImageBuilder>,
    /// Direct Docker client for service operations (create/exec/backup).
    /// None if Docker is not available (shouldn't happen on a real agent).
    pub docker: Option<bollard::Docker>,
    /// Bridge gateway IP for the multi-host overlay (`br-temps0`). The
    /// per-node Hickory DNS resolver listens on this address:53; we
    /// inject it as `--dns` into every container we create so they can
    /// resolve `*.temps.local` natively.
    ///
    /// Populated by `network_sync` once the overlay is bootstrapped.
    /// `None` on single-host setups (the overlay never came up); the
    /// container-create path falls back to Docker's default DNS.
    pub overlay_bridge_address: Arc<std::sync::RwLock<Option<std::net::IpAddr>>>,
    /// Latest peer list from the control plane, refreshed by
    /// `network_sync`. Read by overlay-attach handlers to install
    /// per-peer routes inside each new container's netns. Empty until
    /// the first successful network/peers poll.
    pub overlay_peers: crate::network_sync::SharedPeers,
    /// Container platform of the Docker daemon this agent drives, in OCI form
    /// (`linux/amd64`, `linux/arm64`), once discovered from `docker info`
    /// (see [`crate::server::detect_agent_platform`]).
    ///
    /// Shared with the heartbeat loop, which keeps retrying discovery while it
    /// is `None` — the daemon is often not up yet when the agent starts. The
    /// health report exposes it so the control plane can resolve a node whose
    /// stored architecture is still unknown before transferring an image.
    pub platform: crate::server::SharedPlatform,
}

/// Response wrapper for consistent agent API responses.
#[derive(Serialize, ToSchema)]
pub struct AgentResponse<T: Serialize> {
    pub(crate) success: bool,
    #[schema(nullable = true)]
    pub(crate) data: Option<T>,
    #[schema(nullable = true)]
    pub(crate) error: Option<String>,
}

impl<T: Serialize> AgentResponse<T> {
    pub(crate) fn ok(data: T) -> Json<Self> {
        Json(Self {
            success: true,
            data: Some(data),
            error: None,
        })
    }
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

pub(crate) fn captured_ok_response<T: Serialize>(
    data: T,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Response {
    json_response_with_capture_permit(
        StatusCode::OK,
        AgentResponse {
            success: true,
            data: Some(data),
            error: None,
        },
        permit,
    )
}

pub(crate) fn captured_error_response(
    status: StatusCode,
    message: String,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Response {
    json_response_with_capture_permit(
        status,
        AgentResponse::<()> {
            success: false,
            data: None,
            error: Some(message),
        },
        permit,
    )
}

pub(crate) fn container_identity_error_status(error: &ContainerIdentityError) -> StatusCode {
    match error {
        ContainerIdentityError::Inspect {
            source:
                bollard::errors::Error::DockerResponseServerError {
                    status_code: 404, ..
                },
            ..
        } => StatusCode::NOT_FOUND,
        ContainerIdentityError::Inspect { .. }
        | ContainerIdentityError::MissingId { .. }
        | ContainerIdentityError::InvalidId { .. } => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn remove_error_status(error: &temps_deployer::DeployerError) -> StatusCode {
    match error {
        temps_deployer::DeployerError::ContainerNotFound(_) => StatusCode::NOT_FOUND,
        temps_deployer::DeployerError::DeploymentFailed(_)
        | temps_deployer::DeployerError::ImageNotFound(_)
        | temps_deployer::DeployerError::NetworkError(_)
        | temps_deployer::DeployerError::ResourceAllocationFailed(_)
        | temps_deployer::DeployerError::SecretMountFailed { .. }
        | temps_deployer::DeployerError::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(
        deploy_container,
        stop_container,
        start_container,
        remove_container,
        get_container_logs,
        exec_container,
        get_container_info,
        list_containers,
        image_exists,
        import_image,
        health_check,
        crate::service_handlers::create_service,
        crate::service_handlers::stop_service,
        crate::service_handlers::start_service,
        crate::service_handlers::remove_service,
        crate::service_handlers::service_status,
        crate::service_handlers::service_exec,
        crate::service_handlers::runtime_env,
        crate::service_handlers::health_probe,
        crate::service_handlers::list_services,
        crate::service_handlers::backup_service,
        crate::service_handlers::restore_service,
    ),
    components(schemas(
        AgentResponse<temps_deployer::DeployResult>,
        AgentResponse<String>,
        AgentResponse<bool>,
        AgentResponse<temps_deployer::ContainerInfo>,
        AgentResponse<NodeHealthReport>,
        AgentResponse<crate::ServiceCreateResponse>,
        AgentResponse<crate::ServiceExecResponse>,
        AgentResponse<temps_providers::remote_service_client::RemoteRuntimeEnvResponse>,
        AgentResponse<temps_providers::remote_service_client::RemoteHealthProbeResponse>,
        AgentResponse<crate::ServiceStatus>,
        AgentResponse<Vec<crate::ServiceStatus>>,
        AgentResponse<crate::ServiceBackupResponse>,
        AgentResponse<AgentExecResponse>,
        AgentExecRequest,
        AgentExecResponse,
        NodeHealthReport,
        temps_deployer::DeployRequest,
        temps_deployer::DeployResult,
        temps_deployer::ContainerInfo,
        temps_deployer::ContainerStatus,
        temps_deployer::PortMapping,
        temps_deployer::Protocol,
        temps_deployer::ResourceLimits,
        temps_deployer::RestartPolicy,
        temps_deployer::ContainerLogConfig,
        crate::ServiceCreateRequest,
        crate::ServiceCreateResponse,
        crate::ServicePortMapping,
        crate::ServiceExecRequest,
        crate::ServiceExecResponse,
        temps_providers::remote_service_client::RemoteRuntimeEnvRequest,
        temps_providers::remote_service_client::RemoteRuntimeEnvResponse,
        temps_providers::remote_service_client::RemoteHealthProbeRequest,
        temps_providers::remote_service_client::RemoteHealthProbeResponse,
        crate::ServiceBackupRequest,
        crate::ServiceBackupResponse,
        crate::ServiceRestoreRequest,
        crate::S3CredentialsPayload,
        crate::ServiceStatus,
    )),
    info(
        title = "Temps Agent API",
        description = "Worker node agent API for container and service management. All endpoints require Bearer token authentication.",
        version = "1.0.0"
    ),
    security(
        ("bearer_auth" = [])
    ),
    modifiers(&SecurityAddon)
)]
pub struct AgentApiDoc;

struct SecurityAddon;

impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "bearer_auth",
                utoipa::openapi::security::SecurityScheme::Http(
                    utoipa::openapi::security::Http::new(
                        utoipa::openapi::security::HttpAuthScheme::Bearer,
                    ),
                ),
            );
        }
    }
}

/// Deploy a new container on this worker node
#[utoipa::path(
    tag = "Containers",
    post,
    path = "/agent/containers/deploy",
    request_body = DeployRequest,
    responses(
        (status = 200, description = "Container deployed successfully", body = AgentResponse<temps_deployer::DeployResult>),
        (status = 401, description = "Unauthorized — invalid or missing bearer token"),
        (status = 500, description = "Deploy failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn deploy_container(
    State(state): State<Arc<AgentState>>,
    Json(request): Json<DeployRequest>,
) -> impl IntoResponse {
    let container_name = request.container_name.clone();
    let image_name = request.image_name.clone();
    tracing::info!(
        container = %container_name,
        image = %image_name,
        ports = ?request.port_mappings.iter().map(|p| format!("{}:{}", p.host_port, p.container_port)).collect::<Vec<_>>(),
        "Deploying container"
    );
    match state.container_deployer.deploy_container(request).await {
        Ok(result) => {
            tracing::info!(
                container = %container_name,
                container_id = %result.container_id,
                image = %image_name,
                "Container deployed successfully"
            );
            AgentResponse::ok(result).into_response()
        }
        Err(e) => {
            tracing::error!(
                container = %container_name,
                image = %image_name,
                "Deploy failed: {}",
                e
            );
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Deploy failed: {}", e),
            )
            .into_response()
        }
    }
}

/// Stop a running container
#[utoipa::path(
    tag = "Containers",
    post,
    path = "/agent/containers/{id}/stop",
    params(
        ("id" = String, Path, description = "Container ID or name")
    ),
    responses(
        (status = 200, description = "Container stopped", body = AgentResponse<String>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Stop failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn stop_container(
    State(state): State<Arc<AgentState>>,
    Path(container_id): Path<String>,
) -> impl IntoResponse {
    tracing::info!(container_id = %container_id, "Stopping container");
    match state.container_deployer.stop_container(&container_id).await {
        Ok(()) => {
            tracing::info!(container_id = %container_id, "Container stopped");
            AgentResponse::ok("stopped".to_string()).into_response()
        }
        Err(e) => {
            tracing::error!(container_id = %container_id, "Stop failed: {}", e);
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Stop failed for container {}: {}", container_id, e),
            )
            .into_response()
        }
    }
}

/// Start a stopped container.
///
/// Used by the control plane when the user clicks Start on a container
/// running on this worker. Returns the same `AgentResponse<String>`
/// envelope as `stop_container` so `RemoteNodeDeployer` can decode it
/// uniformly.
#[utoipa::path(
    tag = "Containers",
    post,
    path = "/agent/containers/{id}/start",
    params(
        ("id" = String, Path, description = "Container ID or name")
    ),
    responses(
        (status = 200, description = "Container started", body = AgentResponse<String>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Start failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn start_container(
    State(state): State<Arc<AgentState>>,
    Path(container_id): Path<String>,
) -> impl IntoResponse {
    tracing::info!(container_id = %container_id, "Starting container");
    match state
        .container_deployer
        .start_container(&container_id)
        .await
    {
        Ok(()) => {
            tracing::info!(container_id = %container_id, "Container started");
            AgentResponse::ok("started".to_string()).into_response()
        }
        Err(e) => {
            tracing::error!(container_id = %container_id, "Start failed: {}", e);
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Start failed for container {}: {}", container_id, e),
            )
            .into_response()
        }
    }
}

/// Remove a container
#[utoipa::path(
    tag = "Containers",
    delete,
    path = "/agent/containers/{id}",
    params(
        ("id" = String, Path, description = "Container ID or name")
    ),
    responses(
        (status = 200, description = "Container removed", body = AgentResponse<String>),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Container not found"),
        (status = 500, description = "Remove failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn remove_container(
    State(state): State<Arc<AgentState>>,
    Path(container_id): Path<String>,
) -> impl IntoResponse {
    tracing::info!(container_id = %container_id, "Removing container");
    match state
        .container_deployer
        .remove_container(&container_id)
        .await
    {
        Ok(()) => {
            tracing::info!(container_id = %container_id, "Container removed");
            AgentResponse::ok("removed".to_string()).into_response()
        }
        Err(e) => {
            let status = remove_error_status(&e);
            if status == StatusCode::NOT_FOUND {
                tracing::info!(container_id = %container_id, reason = %e, "Container already absent");
            } else {
                tracing::error!(container_id = %container_id, "Remove failed: {}", e);
            }
            error_response(
                status,
                format!("Remove failed for container {}: {}", container_id, e),
            )
            .into_response()
        }
    }
}

#[cfg(test)]
mod remove_tests {
    use super::*;

    #[test]
    fn missing_container_maps_to_http_not_found_for_idempotent_remote_cleanup() {
        let error = temps_deployer::DeployerError::ContainerNotFound(
            "container no longer exists".to_string(),
        );
        assert_eq!(remove_error_status(&error), StatusCode::NOT_FOUND);
    }

    #[test]
    fn operational_remove_failure_maps_to_http_internal_server_error() {
        let error = temps_deployer::DeployerError::NetworkError("dockerd unavailable".to_string());
        assert_eq!(
            remove_error_status(&error),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}

/// Get container logs
#[utoipa::path(
    tag = "Containers",
    get,
    path = "/agent/containers/{id}/logs",
    params(
        ("id" = String, Path, description = "Container ID or name")
    ),
    responses(
        (status = 200, description = "Container logs", body = AgentResponse<String>),
        (status = 401, description = "Unauthorized"),
        (status = 429, description = "Output capture capacity exhausted"),
        (status = 504, description = "Log capture timed out"),
        (status = 500, description = "Failed to get logs")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_container_logs(
    State(state): State<Arc<AgentState>>,
    Extension(limits): Extension<Arc<AgentResourceLimits>>,
    Path(container_id): Path<String>,
) -> impl IntoResponse {
    let capture_permit = match limits.output_capture_slots.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return error_response(
                StatusCode::TOO_MANY_REQUESTS,
                format!(
                    "Cannot capture logs for container {container_id}: all output capture slots are busy"
                ),
            )
            .into_response();
        }
    };

    tracing::debug!(container_id = %container_id, "Fetching container logs");
    match tokio::time::timeout(
        CONTAINER_LOG_TIMEOUT,
        state.container_deployer.get_container_logs(&container_id),
    )
    .await
    {
        Err(_) => error_response(
            StatusCode::GATEWAY_TIMEOUT,
            format!("Timed out capturing logs for container {container_id} after 60 seconds"),
        )
        .into_response(),
        Ok(Ok(logs)) => captured_ok_response(logs, capture_permit),
        Ok(Err(error)) => {
            tracing::error!(container_id = %container_id, reason = %error, "Failed to get container logs");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get logs for container {container_id}: {error}"),
            )
            .into_response()
        }
    }
}

/// One-shot container stats (CPU%, memory, network counters).
///
/// The control plane calls this when the user opens the metrics tab for a
/// container that runs on this node, and on every poll of the SSE stream
/// (the agent itself doesn't stream — the CP polls at its own interval).
///
/// Not registered in the agent OpenAPI doc because `ContainerStats` does
/// not derive `ToSchema` and is only ever read by the control plane via
/// `RemoteNodeDeployer`.
pub async fn get_container_stats(
    State(state): State<Arc<AgentState>>,
    Path(container_id): Path<String>,
) -> impl IntoResponse {
    tracing::debug!(container_id = %container_id, "Fetching container stats");
    match state
        .container_deployer
        .get_container_stats(&container_id)
        .await
    {
        Ok(stats) => AgentResponse::ok(stats).into_response(),
        Err(e) => {
            tracing::error!(container_id = %container_id, "Failed to get stats: {}", e);
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get stats for container {}: {}", container_id, e),
            )
            .into_response()
        }
    }
}

/// One-shot exec request from the control plane.
///
/// Wire-compatible with the CP's existing `ExecRequest` struct so the
/// remote-deployer client can serialize a single shape regardless of
/// where the container runs.
#[derive(Debug, Deserialize, ToSchema)]
pub struct AgentExecRequest {
    pub command: Vec<String>,
    pub timeout_seconds: Option<u64>,
}

/// Result of a one-shot exec. Mirrors the CP's `ExecResponse`.
#[derive(Debug, Serialize, ToSchema)]
pub struct AgentExecResponse {
    pub exit_code: Option<i64>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, thiserror::Error)]
enum ContainerExecCaptureError {
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

/// Run a one-shot command inside a container on this worker.
///
/// Container exec is timeout-bounded (default 30s, max 300s) so a hung
/// process can't pin an agent worker thread forever. Output is captured
/// to memory; the caller gets a single JSON response, not a stream — for
/// interactive sessions use the (separate) terminal WebSocket.
#[utoipa::path(
    tag = "Containers",
    post,
    path = "/agent/containers/{id}/exec",
    params(
        ("id" = String, Path, description = "Container ID or name")
    ),
    request_body = AgentExecRequest,
    responses(
        (status = 200, description = "Exec result", body = AgentResponse<AgentExecResponse>),
        (status = 400, description = "Invalid command"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Container not found"),
        (status = 409, description = "Container identity changed before exec start"),
        (status = 429, description = "Exec or capture capacity exhausted"),
        (status = 503, description = "Docker unavailable"),
        (status = 500, description = "Exec failed"),
        (status = 504, description = "Exec timed out")
    ),
    security(("bearer_auth" = []))
)]
pub async fn exec_container(
    State(state): State<Arc<AgentState>>,
    Extension(limits): Extension<Arc<AgentResourceLimits>>,
    Path(container_id): Path<String>,
    Json(request): Json<AgentExecRequest>,
) -> impl IntoResponse {
    if request.command.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "Command cannot be empty".into())
            .into_response();
    }

    let Some(docker) = state.docker.clone() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Docker is not available on this agent".into(),
        )
        .into_response();
    };

    let cleanup_container_id = match resolve_container_id(&docker, &container_id).await {
        Ok(container_id) => container_id,
        Err(error) => {
            return error_response(
                container_identity_error_status(&error),
                format!("Failed to resolve container '{container_id}' before exec: {error}"),
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
                    "Cannot execute command in container {container_id}: all exec operation slots are busy"
                ),
            )
            .into_response();
        }
        Err(ExecAdmissionError::CapturesExhausted) => {
            return error_response(
                StatusCode::TOO_MANY_REQUESTS,
                format!(
                    "Cannot execute command in container {container_id}: all output capture slots are busy"
                ),
            )
            .into_response();
        }
        Err(ExecAdmissionError::ContainerOwned) => {
            return error_response(
                StatusCode::TOO_MANY_REQUESTS,
                format!(
                    "Cannot execute command in container {container_id}: another operation already owns that container"
                ),
            )
            .into_response();
        }
    };
    let capture_permit = permits.capture;
    let operation_permit = permits.operation;

    let timeout_secs = std::cmp::min(request.timeout_seconds.unwrap_or(30), 300);

    tracing::info!(
        container_id = %container_id,
        timeout_secs,
        cmd_argc = request.command.len(),
        "Executing one-shot command"
    );

    let exec_config = bollard::models::ExecConfig {
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        cmd: Some(request.command.clone()),
        ..Default::default()
    };

    let exec = match docker.create_exec(&cleanup_container_id, exec_config).await {
        Ok(e) => e,
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => {
            return error_response(
                StatusCode::NOT_FOUND,
                format!("Container {} not found", container_id),
            )
            .into_response();
        }
        Err(e) => {
            tracing::error!(container_id = %container_id, "Failed to create exec: {}", e);
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create exec: {}", e),
            )
            .into_response();
        }
    };
    let exec_container_id = match resolve_exec_container_id(&docker, &exec.id).await {
        Ok(container_id) => container_id,
        Err(error) => {
            tracing::error!(container_id = %container_id, exec_id = %exec.id, reason = %error, "Failed to pin exec container identity");
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
                "Container '{container_id}' changed from '{cleanup_container_id}' to '{exec_container_id}' before exec start"
            ),
        )
        .into_response();
    }
    let mut cleanup_guard = ExecCleanupGuard::new(
        docker.clone(),
        exec.id.clone(),
        cleanup_container_id.clone(),
        capture_permit,
        operation_permit,
        permits.container,
    );

    let start_config = bollard::exec::StartExecOptions {
        detach: false,
        ..Default::default()
    };

    let result = run_exec_with_deadline(
        &docker,
        &exec.id,
        &cleanup_container_id,
        std::time::Duration::from_secs(timeout_secs),
        async {
            let output = docker
                .start_exec(&exec.id, Some(start_config))
                .await
                .map_err(|source| ContainerExecCaptureError::Start {
                    exec_id: exec.id.clone(),
                    source,
                })?;
            let mut stdout = BoundedTailBuffer::new(MAX_CAPTURED_STREAM_BYTES);
            let mut stderr = BoundedTailBuffer::new(MAX_CAPTURED_STREAM_BYTES);
            let mut output = match output {
                bollard::exec::StartExecResults::Attached { output, .. } => output,
                bollard::exec::StartExecResults::Detached => {
                    return Err(ContainerExecCaptureError::UnexpectedDetached {
                        exec_id: exec.id.clone(),
                    });
                }
            };
            while let Some(message) = output.next().await {
                match message.map_err(|source| ContainerExecCaptureError::Stream {
                    exec_id: exec.id.clone(),
                    source,
                })? {
                    bollard::container::LogOutput::StdOut { message } => {
                        stdout.push(message);
                    }
                    bollard::container::LogOutput::StdErr { message } => {
                        stderr.push(message);
                    }
                    _ => {}
                }
            }

            let inspect = docker.inspect_exec(&exec.id).await.map_err(|source| {
                ContainerExecCaptureError::Inspect {
                    exec_id: exec.id.clone(),
                    source,
                }
            })?;
            let exit_code = completed_exec_exit_code(&inspect, &exec.id)?;
            Ok::<_, ContainerExecCaptureError>((
                exit_code,
                stdout.into_string(),
                stderr.into_string(),
            ))
        },
    )
    .await;

    match result {
        ExecDeadlineOutcome::Completed(Ok((exit_code, stdout, stderr))) => {
            let Some(capture_permit) = cleanup_guard.disarm_and_take_capture() else {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!(
                        "Exec in container {container_id} completed without response capture ownership"
                    ),
                )
                .into_response();
            };
            tracing::info!(
                container_id = %container_id,
                exit_code,
                "Exec completed"
            );
            captured_ok_response(
                AgentExecResponse {
                    exit_code: Some(exit_code),
                    stdout,
                    stderr,
                },
                capture_permit,
            )
        }
        ExecDeadlineOutcome::Completed(Err(e)) => {
            let cleanup_scheduled = !matches!(
                &e,
                ContainerExecCaptureError::Start { source, .. }
                    if exec_start_was_rejected(source)
            );
            if !cleanup_scheduled {
                cleanup_guard.disarm();
            }
            let cleanup_note = if cleanup_scheduled {
                format!(
                    "container {container_id} restart was scheduled to stop any ambiguous command workload"
                )
            } else {
                "Docker definitively rejected the command before it started; the container was not restarted".to_string()
            };
            tracing::error!(container_id = %container_id, cleanup_scheduled, "Exec error: {}", e);
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Exec error: {e}; {cleanup_note}"),
            )
            .into_response()
        }
        ExecDeadlineOutcome::ContainerRestarted => {
            cleanup_guard.disarm();
            tracing::warn!(container_id = %container_id, timeout_secs, "Exec timed out; containing container was restarted to stop the complete workload");
            error_response(
                StatusCode::GATEWAY_TIMEOUT,
                format!(
                    "Command timed out after {timeout_secs}s; container {container_id} was restarted to stop the complete command workload"
                ),
            )
            .into_response()
        }
        ExecDeadlineOutcome::TerminationFailed(error) => {
            tracing::error!(
                container_id = %container_id,
                timeout_secs,
                reason = %error,
                "Exec timed out and its container could not be restarted"
            );
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Command timed out after {timeout_secs}s, but the worker could not restart its container to stop the complete workload: {error}"
                ),
            )
            .into_response()
        }
    }
}

/// Persistent terminal session via WebSocket on the worker.
///
/// Speaks the same protocol the browser-facing CP terminal speaks:
///   - client binary frames -> container PTY stdin
///   - container PTY output -> server binary frames (xterm.js renders these)
///   - client text frame `{"type":"resize","cols":N,"rows":N}` -> resize PTY
///   - client text frame `{"type":"input","data":"..."}` -> stdin (legacy)
///   - server text frame `{"type":"exit","code":N}` when exec ends
///
/// The control plane's terminal handler proxies bytes 1:1 between the
/// browser WS and this WS, so exactly the same xterm.js client works
/// against a remote container. No protocol translation in the middle.
pub async fn terminal_container(
    State(state): State<Arc<AgentState>>,
    Extension(limits): Extension<Arc<AgentResourceLimits>>,
    Path(container_id): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(docker) = state.docker.clone() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Docker is not available on this agent".into(),
        )
        .into_response();
    };

    let canonical_container_id = match resolve_container_id(&docker, &container_id).await {
        Ok(container_id) => container_id,
        Err(error) => {
            return error_response(
                container_identity_error_status(&error),
                format!("Failed to resolve terminal container '{container_id}': {error}"),
            )
            .into_response();
        }
    };
    let container_permit = match limits
        .container_operations
        .try_acquire(&canonical_container_id)
    {
        Ok(permit) => permit,
        Err(_) => {
            return error_response(
                StatusCode::TOO_MANY_REQUESTS,
                format!(
                    "Cannot open terminal for container {container_id}: another operation already owns that container"
                ),
            )
            .into_response();
        }
    };

    ws.on_upgrade(move |socket| {
        handle_terminal_session(socket, docker, canonical_container_id, container_permit)
    })
    .into_response()
}

#[derive(Deserialize)]
struct TerminalControl {
    r#type: String,
    cols: Option<u16>,
    rows: Option<u16>,
    data: Option<String>,
}

async fn handle_terminal_session(
    socket: WebSocket,
    docker: bollard::Docker,
    container_id: String,
    container_permit: tokio::sync::OwnedSemaphorePermit,
) {
    tracing::debug!(container_id = %container_id, "Agent terminal session started");

    // Try bash, fall back to sh — same shape as the CP-local terminal so
    // remote sessions feel identical.
    let exec_config = bollard::models::ExecConfig {
        attach_stdin: Some(true),
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        tty: Some(true),
        cmd: Some(vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "if command -v bash >/dev/null 2>&1; then exec bash; else exec sh; fi".to_string(),
        ]),
        ..Default::default()
    };

    let exec = match docker.create_exec(&container_id, exec_config).await {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(error = %e, container_id = %container_id, "Failed to create exec for terminal");
            return;
        }
    };
    let exec_id = exec.id.clone();
    let exec_container_id = match resolve_exec_container_id(&docker, &exec_id).await {
        Ok(container_id) => container_id,
        Err(error) => {
            tracing::error!(reason = %error, container_id = %container_id, exec_id = %exec_id, "Failed to pin terminal exec container identity");
            return;
        }
    };
    if exec_container_id != container_id {
        tracing::error!(container_id = %container_id, exec_container_id = %exec_container_id, exec_id = %exec_id, "Terminal container identity changed before exec start");
        return;
    }
    let mut cleanup_guard = ExecCleanupGuard::new_without_capture(
        docker.clone(),
        exec_id.clone(),
        container_id.clone(),
        None,
        container_permit,
    );

    let start_config = bollard::exec::StartExecOptions {
        detach: false,
        tty: true,
        ..Default::default()
    };

    let (mut docker_output, mut docker_input) = match docker
        .start_exec(&exec_id, Some(start_config))
        .await
    {
        Ok(StartExecResults::Attached { output, input }) => (output, input),
        Ok(StartExecResults::Detached) => {
            tracing::error!(container_id = %container_id, exec_id = %exec_id, "Terminal exec started detached unexpectedly; scheduling containing-container restart");
            return;
        }
        Err(error) => {
            let cleanup_scheduled = !exec_start_was_rejected(&error);
            if !cleanup_scheduled {
                cleanup_guard.disarm();
            }
            tracing::error!(reason = %error, container_id = %container_id, exec_id = %exec_id, cleanup_scheduled, "Failed to start exec for terminal");
            return;
        }
    };

    let (mut ws_sender, mut ws_receiver) = socket.split();

    // PTY -> WS
    let exec_id_for_output = exec_id.clone();
    let docker_for_output = docker.clone();
    let output_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = docker_output.next().await {
            let bytes: bytes::Bytes = match msg {
                bollard::container::LogOutput::StdOut { message } => message,
                bollard::container::LogOutput::StdErr { message } => message,
                bollard::container::LogOutput::Console { message } => message,
                _ => continue,
            };
            if ws_sender
                .send(Message::Binary(bytes.to_vec().into()))
                .await
                .is_err()
            {
                break;
            }
        }

        let exit_code = docker_for_output
            .inspect_exec(&exec_id_for_output)
            .await
            .ok()
            .and_then(|i| i.exit_code)
            .unwrap_or(-1);
        let exit_msg = format!(r#"{{"type":"exit","code":{}}}"#, exit_code);
        let _ = ws_sender.send(Message::Text(exit_msg.into())).await;
        let _ = ws_sender.close().await;
    });

    // WS -> PTY
    let idle_timeout = std::time::Duration::from_secs(15 * 60);
    loop {
        let next = tokio::time::timeout(idle_timeout, ws_receiver.next()).await;
        match next {
            Ok(Some(Ok(Message::Binary(data)))) => {
                if docker_input.write_all(&data).await.is_err() {
                    break;
                }
                if docker_input.flush().await.is_err() {
                    break;
                }
            }
            Ok(Some(Ok(Message::Text(text)))) => {
                if let Ok(ctrl) = serde_json::from_str::<TerminalControl>(&text) {
                    match ctrl.r#type.as_str() {
                        "resize" => {
                            if let (Some(cols), Some(rows)) = (ctrl.cols, ctrl.rows) {
                                let resize_opts = bollard::exec::ResizeExecOptions {
                                    width: cols,
                                    height: rows,
                                };
                                if let Err(e) = docker.resize_exec(&exec_id, resize_opts).await {
                                    tracing::warn!(error = %e, "Failed to resize terminal");
                                }
                            }
                        }
                        "input" => {
                            if let Some(data) = ctrl.data {
                                if docker_input.write_all(data.as_bytes()).await.is_err() {
                                    break;
                                }
                                let _ = docker_input.flush().await;
                            }
                        }
                        _ => {}
                    }
                } else if docker_input.write_all(text.as_bytes()).await.is_err() {
                    break;
                }
            }
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) => {
                tracing::debug!(container_id = %container_id, "Agent terminal closed by client");
                break;
            }
            Err(_) => {
                tracing::info!(container_id = %container_id, "Agent terminal idle 15m, closing");
                break;
            }
            _ => {}
        }
    }

    let _ = docker_input.shutdown().await;
    output_task.abort();
    monitor_exec_until_stopped(
        docker,
        exec_id,
        container_id.clone(),
        "terminal exec",
        cleanup_guard,
    );
    tracing::info!(container_id = %container_id, "Agent terminal session ended; lifecycle ownership retained until Docker confirms exec completion");
}

/// Query parameters for the streaming logs endpoint. Mirrors the control
/// plane's `ContainerLogsQuery` so the proxy can pass them through verbatim.
#[derive(Debug, Deserialize)]
pub struct ContainerLogsStreamQuery {
    /// Unix timestamp (seconds). `0` or absent = beginning.
    pub start_date: Option<i64>,
    /// Unix timestamp (seconds). `0` or absent = no upper bound.
    pub end_date: Option<i64>,
    /// `"all"` or a number of trailing lines.
    pub tail: Option<String>,
    /// Prefix every line with the Docker timestamp.
    #[serde(default)]
    pub timestamps: bool,
    /// `true` to stream new lines as they arrive (default), `false` to dump
    /// the existing logs and close.
    #[serde(default = "default_true")]
    pub follow: bool,
}

fn default_true() -> bool {
    true
}

/// Stream container logs over a chunked HTTP body.
///
/// The control plane proxies each chunk to the browser as a WebSocket
/// Text frame, so callers see exactly what they would see if they hit the
/// existing in-process log path on a single-host cluster.
pub async fn stream_container_logs(
    State(state): State<Arc<AgentState>>,
    Path(container_id): Path<String>,
    Query(params): Query<ContainerLogsStreamQuery>,
) -> Response {
    let Some(docker) = state.docker.clone() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Docker is not available on this agent".into(),
        )
        .into_response();
    };

    // Inspect first so we can return a clean 404 instead of a half-open
    // chunked body that errors mid-stream.
    if let Err(e) = docker
        .inspect_container(
            &container_id,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
    {
        return match e {
            bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            } => error_response(
                StatusCode::NOT_FOUND,
                format!("Container {} not found", container_id),
            )
            .into_response(),
            other => error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to inspect container {}: {}", container_id, other),
            )
            .into_response(),
        };
    }

    let log_options = LogsOptions {
        follow: params.follow,
        stdout: true,
        stderr: true,
        timestamps: params.timestamps,
        tail: params.tail.unwrap_or_else(|| "all".into()),
        since: params.start_date.unwrap_or(0) as i32,
        until: params.end_date.unwrap_or(0) as i32,
    };

    tracing::debug!(
        container_id = %container_id,
        follow = params.follow,
        timestamps = params.timestamps,
        "Streaming container logs"
    );

    let logs = docker.logs(&container_id, Some(log_options));
    let log_stream = logs.map(|chunk| match chunk {
        Ok(out) => {
            let bytes: bytes::Bytes = out.into_bytes();
            Ok::<_, std::io::Error>(bytes)
        }
        Err(e) => Err(std::io::Error::other(format!("docker logs error: {}", e))),
    });

    // Interleave a NUL keepalive every 25s when the container is silent so
    // intermediate proxies (Pingora's 60s body read timeout, idle TCP
    // gateways) don't drop the long-lived stream. The control plane filters
    // these out before forwarding to the WebSocket client.
    let keepalive = futures::stream::unfold((), |_| async move {
        tokio::time::sleep(std::time::Duration::from_secs(25)).await;
        Some((
            Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"\0")),
            (),
        ))
    });
    let body_stream = futures::stream::select(log_stream, keepalive);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        // Disable proxy buffering so log lines flush as they arrive.
        .header("X-Accel-Buffering", "no")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| {
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to construct log stream response".into(),
            )
            .into_response()
        })
}

/// Get container info (status, ports, environment)
#[utoipa::path(
    tag = "Containers",
    get,
    path = "/agent/containers/{id}/info",
    params(
        ("id" = String, Path, description = "Container ID or name")
    ),
    responses(
        (status = 200, description = "Container info", body = AgentResponse<temps_deployer::ContainerInfo>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Failed to get info")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_container_info(
    State(state): State<Arc<AgentState>>,
    Path(container_id): Path<String>,
) -> impl IntoResponse {
    tracing::debug!(container_id = %container_id, "Fetching container info");
    match state
        .container_deployer
        .get_container_info(&container_id)
        .await
    {
        Ok(info) => match managed_container_detail(info) {
            Some(info) => AgentResponse::ok(info).into_response(),
            None => {
                error_response(StatusCode::NOT_FOUND, "Container not found".into()).into_response()
            }
        },
        Err(e) => {
            tracing::error!(container_id = %container_id, "Failed to get info: {}", e);
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get info for container {}: {}", container_id, e),
            )
            .into_response()
        }
    }
}

fn is_temps_managed_container(container: &temps_deployer::ContainerInfo) -> bool {
    container
        .labels
        .get("sh.temps.managed")
        .is_some_and(|value| value == "true")
}

fn managed_container_detail(
    mut container: temps_deployer::ContainerInfo,
) -> Option<temps_deployer::ContainerInfo> {
    if !is_temps_managed_container(&container) {
        return None;
    }
    container.environment_vars.clear();
    Some(container)
}

fn managed_container_inventory(
    containers: Vec<temps_deployer::ContainerInfo>,
) -> Vec<temps_deployer::ContainerInfo> {
    containers
        .into_iter()
        .filter(is_temps_managed_container)
        .map(|mut container| {
            container.environment_vars.clear();
            container
        })
        .collect()
}

#[cfg(test)]
mod inventory_tests {
    use super::*;

    fn container(name: &str, managed_label: Option<&str>) -> temps_deployer::ContainerInfo {
        let mut info = temps_deployer::ContainerInfo {
            container_name: name.to_string(),
            ..Default::default()
        };
        if let Some(value) = managed_label {
            info.labels
                .insert("sh.temps.managed".to_string(), value.to_string());
        }
        info.environment_vars
            .insert("DATABASE_URL".to_string(), "postgres://secret".to_string());
        info
    }

    #[test]
    fn inventory_only_returns_managed_containers_without_environment_secrets() {
        let inventory = managed_container_inventory(vec![
            container("unmanaged", None),
            container("false-label", Some("false")),
            container("managed", Some("true")),
        ]);

        assert_eq!(inventory.len(), 1);
        assert_eq!(inventory[0].container_name, "managed");
        assert!(inventory[0].environment_vars.is_empty());
    }

    #[test]
    fn detail_policy_rejects_unmanaged_and_redacts_managed_containers() {
        assert!(managed_container_detail(container("unmanaged", None)).is_none());
        let managed = managed_container_detail(container("managed", Some("true")))
            .expect("managed container remains visible");
        assert!(managed.environment_vars.is_empty());
    }
}

/// List Temps-managed containers on this worker node without environment variables.
#[utoipa::path(
    tag = "Containers",
    get,
    path = "/agent/containers",
    responses(
        (status = 200, description = "List of Temps-managed containers with environment variables redacted", body = AgentResponse<Vec<temps_deployer::ContainerInfo>>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Failed to list containers")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_containers(State(state): State<Arc<AgentState>>) -> impl IntoResponse {
    tracing::debug!("Listing Temps-managed containers");
    match state.container_deployer.list_containers().await {
        Ok(containers) => {
            AgentResponse::ok(managed_container_inventory(containers)).into_response()
        }
        Err(e) => {
            tracing::error!("Failed to list containers: {}", e);
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to list containers: {}", e),
            )
            .into_response()
        }
    }
}

/// Check if a Docker image exists on this node
#[utoipa::path(
    tag = "Images",
    get,
    path = "/agent/images/{name}/exists",
    params(
        ("name" = String, Path, description = "Docker image name (URL-encoded if it contains slashes)")
    ),
    responses(
        (status = 200, description = "Image existence check result", body = AgentResponse<bool>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Failed to check image")
    ),
    security(("bearer_auth" = []))
)]
pub async fn image_exists(
    State(state): State<Arc<AgentState>>,
    Path(image_name): Path<String>,
) -> impl IntoResponse {
    tracing::debug!(image = %image_name, "Checking if image exists");
    match state.container_deployer.image_exists(&image_name).await {
        Ok(exists) => {
            tracing::debug!(image = %image_name, exists = exists, "Image existence check complete");
            AgentResponse::ok(exists).into_response()
        }
        Err(e) => {
            tracing::error!(image = %image_name, "Failed to check image: {}", e);
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to check image {}: {}", image_name, e),
            )
            .into_response()
        }
    }
}

fn image_upload_stream(
    body: Body,
    received_bytes: Arc<AtomicU64>,
    max_bytes: u64,
) -> temps_deployer::ImageImportStream {
    Box::pin(body.into_data_stream().map(move |result| {
        match result {
            Ok(chunk) => {
                let chunk_bytes = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
                let previous = received_bytes.fetch_add(chunk_bytes, Ordering::Relaxed);
                let total = previous.saturating_add(chunk_bytes);
                if total > max_bytes {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::FileTooLarge,
                        format!(
                            "Image upload exceeded the {max_bytes}-byte worker limit after receiving {total} bytes"
                        ),
                    ));
                }
                Ok(chunk)
            }
            Err(error) => Err(std::io::Error::other(format!(
                "Failed to read image upload body: {error}"
            ))),
        }
    }))
}

fn spawn_permit_owned_task<T, F>(
    permit: tokio::sync::OwnedSemaphorePermit,
    future: F,
) -> tokio::task::JoinHandle<T>
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
{
    tokio::spawn(async move {
        let _permit = permit;
        future.await
    })
}

/// Import a Docker image from a tar archive streamed in the request body.
///
/// The control plane calls this to transfer locally-built images to worker nodes.
/// The image tag is passed via the `x-image-tag` header.
#[utoipa::path(
    tag = "Images",
    post,
    path = "/agent/images/import",
    request_body(content = Vec<u8>, content_type = "application/x-tar"),
    responses(
        (status = 200, description = "Image imported successfully", body = AgentResponse<String>),
        (status = 400, description = "Missing x-image-tag header"),
        (status = 401, description = "Unauthorized"),
        (status = 413, description = "Image exceeds the worker import limit"),
        (status = 503, description = "Image import capacity unavailable"),
        (status = 504, description = "Image import deadline exceeded"),
        (status = 500, description = "Import failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn import_image(
    State(state): State<Arc<AgentState>>,
    Extension(limits): Extension<Arc<AgentResourceLimits>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Body,
) -> impl IntoResponse {
    let tag = match headers.get("x-image-tag").and_then(|v| v.to_str().ok()) {
        Some(t) => t.to_string(),
        None => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "Missing required x-image-tag header".to_string(),
            )
            .into_response();
        }
    };

    tracing::info!(image = %tag, "Receiving image tar from control plane");

    if let Some(content_length) = headers.get(header::CONTENT_LENGTH) {
        let content_length: u64 = match content_length
            .to_str()
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
        {
            Some(length) => length,
            None => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "Invalid Content-Length header for image import".to_string(),
                )
                .into_response();
            }
        };
        if content_length > MAX_IMAGE_IMPORT_BYTES {
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "Image import is {content_length} bytes; worker limit is {MAX_IMAGE_IMPORT_BYTES} bytes"
                ),
            )
            .into_response();
        }
    }

    let deadline = tokio::time::Instant::now() + IMAGE_IMPORT_TIMEOUT;
    let import_permit =
        match tokio::time::timeout_at(deadline, limits.image_import_slots.clone().acquire_owned())
            .await
        {
            Ok(Ok(permit)) => permit,
            Ok(Err(error)) => {
                return error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!("Image import capacity is unavailable: {error}"),
                )
                .into_response();
            }
            Err(_) => {
                return error_response(
                    StatusCode::GATEWAY_TIMEOUT,
                    format!("Image import '{tag}' waited 30 minutes for worker capacity"),
                )
                .into_response();
            }
        };

    // Forward request chunks directly to Docker. Backpressure propagates from
    // the daemon to the request body, keeping memory bounded without a full
    // temporary archive whose page cache is charged to this service's cgroup.
    let received_bytes = Arc::new(AtomicU64::new(0));
    let image_stream = image_upload_stream(body, received_bytes.clone(), MAX_IMAGE_IMPORT_BYTES);

    let image_builder = state.image_builder.clone();
    let import_tag = tag.clone();
    let mut import_task = spawn_permit_owned_task(import_permit, async move {
        image_builder
            .import_image_stream(image_stream, &import_tag)
            .await
    });
    let result = match tokio::time::timeout_at(deadline, &mut import_task).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Image import task for '{tag}' failed: {error}"),
            )
            .into_response();
        }
        Err(_) => {
            tracing::warn!(image = %tag, "Image import request exceeded its deadline; Docker import continues to own the worker slot until it ends");
            return error_response(
                StatusCode::GATEWAY_TIMEOUT,
                format!("Image import '{tag}' exceeded the 30-minute worker deadline"),
            )
            .into_response();
        }
    };
    let total_bytes = received_bytes.load(Ordering::Relaxed);

    match result {
        Ok(image_id) => {
            tracing::info!(
                image = %tag,
                image_id = %image_id,
                size_mb = format!("{:.1}", total_bytes as f64 / 1_048_576.0),
                "Streamed image imported successfully"
            );
            AgentResponse::ok(image_id).into_response()
        }
        Err(error) => {
            if total_bytes > MAX_IMAGE_IMPORT_BYTES {
                tracing::warn!(
                    image = %tag,
                    received_bytes = total_bytes,
                    "Rejected oversized streamed image import"
                );
                return error_response(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    format!("Image import exceeded the {MAX_IMAGE_IMPORT_BYTES}-byte worker limit"),
                )
                .into_response();
            }
            tracing::error!(
                image = %tag,
                received_bytes = total_bytes,
                reason = %error,
                "Streamed image import failed"
            );
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Failed to import image '{tag}' after receiving {total_bytes} bytes: {error}"
                ),
            )
            .into_response()
        }
    }
}

/// Health check — returns system metrics for this worker node
#[utoipa::path(
    tag = "Health",
    get,
    path = "/agent/health",
    responses(
        (status = 200, description = "Node health report", body = AgentResponse<NodeHealthReport>),
        (status = 401, description = "Unauthorized")
    ),
    security(("bearer_auth" = []))
)]
pub async fn health_check(State(state): State<Arc<AgentState>>) -> impl IntoResponse {
    let report = collect_system_metrics(&state).await;
    AgentResponse::ok(report)
}

/// Collect real system metrics using sysinfo.
async fn collect_system_metrics(state: &AgentState) -> NodeHealthReport {
    use sysinfo::{Disks, System};

    let mut sys = System::new();
    sys.refresh_cpu_all();
    sys.refresh_memory();
    let disks = Disks::new_with_refreshed_list();

    let cpu_percent = sys.global_cpu_usage() as f64;
    let memory_used_bytes = sys.used_memory();
    let memory_total_bytes = sys.total_memory();

    // Use only the root mount point to avoid double-counting overlapping mounts
    let (disk_used, disk_total) = disks
        .list()
        .iter()
        .find(|d| d.mount_point() == std::path::Path::new("/"))
        .map(|d| (d.total_space() - d.available_space(), d.total_space()))
        .unwrap_or((0, 0));

    // Count running containers via the deployer
    let running_containers = match state.container_deployer.list_containers().await {
        Ok(containers) => containers.len() as u64,
        Err(_) => 0,
    };

    NodeHealthReport {
        cpu_percent,
        memory_used_bytes,
        memory_total_bytes,
        disk_used_bytes: disk_used,
        disk_total_bytes: disk_total,
        running_containers,
        // Empty means "not discovered yet" — the control plane treats a blank
        // platform as unknown rather than as a claim about this node.
        platform: crate::server::read_platform(&state.platform).unwrap_or_default(),
    }
}

#[cfg(test)]
mod image_import_tests {
    use super::*;
    use bytes::Bytes;
    use std::sync::atomic::AtomicUsize;

    #[tokio::test]
    async fn image_upload_stream_is_lazy_and_backpressure_aware() {
        const CHUNK_BYTES: usize = 1024 * 1024;
        const TOTAL_CHUNKS: usize = 100;

        let source_polls = Arc::new(AtomicUsize::new(0));
        let source_counter = source_polls.clone();
        let source = futures::stream::unfold(0usize, move |index| {
            let source_counter = source_counter.clone();
            async move {
                if index == TOTAL_CHUNKS {
                    return None;
                }
                source_counter.fetch_add(1, Ordering::Relaxed);
                Some((
                    Ok::<_, std::io::Error>(Bytes::from(vec![0; CHUNK_BYTES])),
                    index + 1,
                ))
            }
        });

        let received_bytes = Arc::new(AtomicU64::new(0));
        let mut upload = image_upload_stream(
            Body::from_stream(source),
            received_bytes.clone(),
            (CHUNK_BYTES * TOTAL_CHUNKS) as u64,
        );

        assert_eq!(source_polls.load(Ordering::Relaxed), 0);
        let first = upload
            .next()
            .await
            .expect("stream should produce its first chunk")
            .expect("first chunk should be readable");

        assert_eq!(first.len(), CHUNK_BYTES);
        assert_eq!(received_bytes.load(Ordering::Relaxed), CHUNK_BYTES as u64);
        assert!(
            source_polls.load(Ordering::Relaxed) < TOTAL_CHUNKS,
            "reading one chunk must not eagerly materialize the 100 MiB upload"
        );
    }

    #[tokio::test]
    async fn image_upload_stream_rejects_bytes_past_limit() {
        let received_bytes = Arc::new(AtomicU64::new(0));
        let mut upload = image_upload_stream(
            Body::from(Bytes::from_static(b"12345")),
            received_bytes.clone(),
            4,
        );

        let error = upload
            .next()
            .await
            .expect("body should yield one result")
            .expect_err("five bytes must exceed a four-byte limit");
        assert!(error.to_string().contains("4-byte worker limit"));
        assert_eq!(received_bytes.load(Ordering::Relaxed), 5);
    }

    #[tokio::test]
    async fn dropped_import_waiter_does_not_release_import_capacity() {
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = slots
            .clone()
            .acquire_owned()
            .await
            .expect("test semaphore remains open");
        let finish = Arc::new(tokio::sync::Notify::new());
        let task_finish = finish.clone();
        let task = spawn_permit_owned_task(permit, async move {
            task_finish.notified().await;
        });

        drop(task);
        assert!(slots.clone().try_acquire_owned().is_err());

        finish.notify_one();
        let released = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            slots.clone().acquire_owned(),
        )
        .await
        .expect("owned import task releases capacity after completion")
        .expect("test semaphore remains open");
        drop(released);
    }
}

#[cfg(test)]
mod openapi_response_tests {
    use super::*;

    #[test]
    fn resource_limit_and_lifecycle_statuses_are_documented() {
        let spec = serde_json::to_value(AgentApiDoc::openapi())
            .expect("agent OpenAPI document serializes");
        for pointer in [
            "/paths/~1agent~1containers~1{id}~1logs/get/responses/429",
            "/paths/~1agent~1containers~1{id}~1logs/get/responses/504",
            "/paths/~1agent~1containers~1{id}~1exec/post/responses/404",
            "/paths/~1agent~1containers~1{id}~1exec/post/responses/409",
            "/paths/~1agent~1containers~1{id}~1exec/post/responses/429",
            "/paths/~1agent~1containers~1{id}~1exec/post/responses/503",
            "/paths/~1agent~1containers~1{id}~1exec/post/responses/504",
            "/paths/~1agent~1images~1import/post/responses/413",
            "/paths/~1agent~1images~1import/post/responses/503",
            "/paths/~1agent~1images~1import/post/responses/504",
            "/paths/~1agent~1services~1exec/post/responses/404",
            "/paths/~1agent~1services~1exec/post/responses/409",
            "/paths/~1agent~1services~1exec/post/responses/429",
            "/paths/~1agent~1services~1exec/post/responses/504",
            "/paths/~1agent~1services~1backup/post/responses/404",
            "/paths/~1agent~1services~1backup/post/responses/409",
            "/paths/~1agent~1services~1backup/post/responses/429",
            "/paths/~1agent~1services~1backup/post/responses/504",
            "/paths/~1agent~1services~1restore/post/responses/404",
            "/paths/~1agent~1services~1restore/post/responses/409",
            "/paths/~1agent~1services~1restore/post/responses/429",
            "/paths/~1agent~1services~1restore/post/responses/504",
        ] {
            assert!(spec.pointer(pointer).is_some(), "missing OpenAPI {pointer}");
        }
    }
}
