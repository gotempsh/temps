// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Backups and in-place restores of the managed services a worker runs.
//!
//! The control plane cannot reach a worker's containers, so a backup of a
//! MongoDB, Redis, MariaDB or managed S3 service on a worker runs here: the
//! engine's own tool against the container on this node, streamed to the S3
//! destination the request names (see
//! `temps_providers::externalsvc::worker_data`). The routes sit behind the
//! node token like every `/agent/services/*` route, and their bodies arrive
//! sealed to this node (they carry the destination's and the service's
//! credentials). The caller chooses no command, image or host: the engine
//! follows from the service type, the container from the service's name.
//!
//! At most [`MAX_DATA_OPERATIONS`] run at once on a node, each bounded by
//! [`DATA_OPERATION_DEADLINE`]. A backup never stops or restarts the service.

use std::sync::Arc;
use std::time::Duration;

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use temps_service_protocol::{
    MongodbBackupRequest, MongodbBackupResult, MongodbRestoreRequest, MongodbRestoreResult,
    PostgresWalgArchiveRequest, PostgresWalgArchiveResult, PostgresWalgBackupRequest,
    PostgresWalgBackupResult, PostgresWalgCredentialsRequest, PostgresWalgCredentialsResult,
    PostgresWalgRestoreRequest, PostgresWalgRestoreResult, RecreateHook, RedisRdbBackupRequest,
    RedisRdbBackupResult, RedisRestoreRequest, RedisRestoreResult, RedisWalgBackupRequest,
    RedisWalgBackupResult, RemoteServiceBackupRequest, RemoteServiceBackupResponse,
    RemoteServiceRestoreRequest, RemoteServiceRestoreResponse,
};
use tokio::sync::Semaphore;

use crate::handlers::{error_response, AgentResponse, AgentState};

/// Backups and restores running at once on this node. Each holds one upload
/// part (16 MiB) in memory and a helper container.
pub const MAX_DATA_OPERATIONS: usize = 2;
/// Upper bound of one backup or restore; the control plane waits as long.
pub const DATA_OPERATION_DEADLINE: Duration = Duration::from_secs(6 * 3600);

static DATA_OPERATIONS: Semaphore = Semaphore::const_new(MAX_DATA_OPERATIONS);

/// A slot among the node's [`MAX_DATA_OPERATIONS`], or `None` when every
/// slot is taken and the request is refused as [`busy`].
fn admit(slots: &Semaphore) -> Option<tokio::sync::SemaphorePermit<'_>> {
    slots.try_acquire().ok()
}

fn busy(operation: &str, service: &str) -> axum::response::Response {
    error_response(
        StatusCode::TOO_MANY_REQUESTS,
        format!(
            "Cannot {operation} service '{service}': this node already runs \
             {MAX_DATA_OPERATIONS} backups or restores; retry when one finishes"
        ),
    )
    .into_response()
}

fn docker_missing(operation: &str, service: &str) -> axum::response::Response {
    error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        format!("Docker is not available on this agent to {operation} service '{service}'"),
    )
    .into_response()
}

fn deadline_exceeded(operation: &str, service: &str) -> axum::response::Response {
    error_response(
        StatusCode::GATEWAY_TIMEOUT,
        format!(
            "The {operation} of service '{service}' exceeded the {}-hour deadline on this \
             node and was stopped; the service itself was left running",
            DATA_OPERATION_DEADLINE.as_secs() / 3600
        ),
    )
    .into_response()
}

/// Back up a service this node runs into the S3 destination the request
/// names, with the engine's own tool, without stopping the service.
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services/data-backup",
    request_body = RemoteServiceBackupRequest,
    responses(
        (status = 200, description = "Backup written", body = AgentResponse<RemoteServiceBackupResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 429, description = "This node already runs its maximum of backups and restores"),
        (status = 500, description = "Backup failed"),
        (status = 503, description = "Docker is not available on this agent"),
        (status = 504, description = "Backup deadline exceeded")
    ),
    security(("bearer_auth" = []))
)]
pub async fn data_backup(
    State(state): State<Arc<AgentState>>,
    Json(request): Json<RemoteServiceBackupRequest>,
) -> axum::response::Response {
    let service = request.service_config.name.clone();
    let Some(_permit) = admit(&DATA_OPERATIONS) else {
        return busy("back up", &service);
    };
    let Some(docker) = state.docker.as_ref() else {
        return docker_missing("back up", &service);
    };
    tracing::info!(
        service = %service,
        service_type = %request.service_config.service_type,
        bucket = %request.destination.bucket,
        key = %request.key,
        "Backing up a service on this node"
    );
    let published_address =
        crate::service_handlers::published_probe_address(&state.host_bind_address());
    match tokio::time::timeout(
        DATA_OPERATION_DEADLINE,
        state
            .service_runtime
            .backup_service(request, Arc::new(docker.clone()), published_address),
    )
    .await
    {
        Ok(Ok(response)) => AgentResponse::ok(response).into_response(),
        Ok(Err(error)) => {
            tracing::error!(service = %service, %error, "Service backup failed");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Backup of service '{service}' failed: {error}"),
            )
            .into_response()
        }
        Err(_) => deadline_exceeded("backup", &service),
    }
}

/// Restore a service this node runs, in place, from a backup
/// [`data_backup`] wrote. User-initiated: the engine's own restore may stop
/// and start the service (Redis does).
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services/data-restore",
    request_body = RemoteServiceRestoreRequest,
    responses(
        (status = 200, description = "Restore applied", body = AgentResponse<RemoteServiceRestoreResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 429, description = "This node already runs its maximum of backups and restores"),
        (status = 500, description = "Restore failed"),
        (status = 503, description = "Docker is not available on this agent"),
        (status = 504, description = "Restore deadline exceeded")
    ),
    security(("bearer_auth" = []))
)]
pub async fn data_restore(
    State(state): State<Arc<AgentState>>,
    Json(request): Json<RemoteServiceRestoreRequest>,
) -> axum::response::Response {
    let service = request.service_config.name.clone();
    let Some(_permit) = admit(&DATA_OPERATIONS) else {
        return busy("restore", &service);
    };
    let Some(docker) = state.docker.as_ref() else {
        return docker_missing("restore", &service);
    };
    tracing::info!(
        service = %service,
        service_type = %request.service_config.service_type,
        location = %request.location,
        "Restoring a service on this node in place"
    );
    let published_address =
        crate::service_handlers::published_probe_address(&state.host_bind_address());
    match tokio::time::timeout(
        DATA_OPERATION_DEADLINE,
        state
            .service_runtime
            .restore_service(request, Arc::new(docker.clone()), published_address),
    )
    .await
    {
        Ok(Ok(response)) => AgentResponse::ok(response).into_response(),
        Ok(Err(error)) => {
            tracing::error!(service = %service, %error, "Service restore failed");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Restore of service '{service}' failed: {error}"),
            )
            .into_response()
        }
        Err(_) => deadline_exceeded("restore", &service),
    }
}

/// Take a WAL-G base backup of a PostgreSQL service this node runs, with the
/// control plane's own engine (ADR-099): continuous archiving is turned on
/// or refreshed first, then `wal-g backup-push` runs in the container.
///
/// Turning archiving on while `archive_mode` is off restarts PostgreSQL in a
/// recreated container, only when the request says the restart was
/// confirmed. The replacement is the running container's own specification
/// with `archive_mode` on, attached to this node as every container this
/// agent creates, under the recreate guard. Engine outcomes come back as
/// 200 with `completed`, `refused` (retrying cannot help) or `failed`.
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services/postgres/walg-backup",
    request_body = PostgresWalgBackupRequest,
    responses(
        (status = 200, description = "The engine ran; its outcome", body = AgentResponse<PostgresWalgBackupResult>),
        (status = 401, description = "Unauthorized"),
        (status = 429, description = "This node already runs its maximum of backups and restores"),
        (status = 500, description = "The engine could not run"),
        (status = 503, description = "Docker is not available on this agent"),
        (status = 504, description = "Backup deadline exceeded")
    ),
    security(("bearer_auth" = []))
)]
pub async fn postgres_walg_backup(
    State(state): State<Arc<AgentState>>,
    Json(request): Json<PostgresWalgBackupRequest>,
) -> axum::response::Response {
    let service = request.service_config.name.clone();
    let Some(permit) = admit(&DATA_OPERATIONS) else {
        return busy("back up", &service);
    };
    let Some(docker) = state.docker.as_ref() else {
        return docker_missing("back up", &service);
    };
    tracing::info!(
        service = %service,
        service_id = request.service_id,
        bucket = %request.destination.bucket,
        allow_restart = request.allow_restart,
        "Taking a WAL-G base backup of a service on this node"
    );
    let node = crate::service_handlers::node_attachment(
        &state,
        Some(temps_core::NETWORK_NAME.to_string()),
    );
    let after_create = overlay_hook(docker);
    let published_address =
        crate::service_handlers::published_probe_address(&state.host_bind_address());
    let runtime = state.service_runtime.clone();
    let engine_docker = Arc::new(docker.clone());
    let outcome = match run_detached(permit, async move {
        runtime
            .postgres_walg_backup(
                request,
                engine_docker,
                node,
                after_create,
                published_address,
            )
            .await
    })
    .await
    {
        Detached::Done(Ok(outcome)) => outcome,
        Detached::Done(Err(error)) => {
            tracing::error!(service = %service, %error, "WAL-G base backup could not run");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("WAL-G base backup of service '{service}' could not run: {error}"),
            )
            .into_response();
        }
        Detached::Panicked(error) => {
            tracing::error!(service = %service, %error, "WAL-G base backup panicked");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("WAL-G base backup of service '{service}' stopped unexpectedly: {error}"),
            )
            .into_response();
        }
        Detached::DeadlineExceeded => return deadline_exceeded("WAL-G base backup", &service),
    };
    match &outcome {
        PostgresWalgBackupResult::Completed { restarted, .. } => {
            if *restarted {
                reroute_recreated_postgres(&state, docker, &service).await;
            }
            tracing::info!(service = %service, restarted, "WAL-G base backup completed");
        }
        PostgresWalgBackupResult::Refused { reason } => {
            tracing::warn!(service = %service, %reason, "WAL-G base backup refused");
        }
        PostgresWalgBackupResult::Failed { reason } => {
            tracing::error!(service = %service, %reason, "WAL-G base backup failed");
        }
    }
    AgentResponse::ok(outcome).into_response()
}

/// What [`run_detached`] returns: the engine's outcome, or why it has none.
enum Detached<T> {
    Done(Result<T, temps_service_protocol::NodeServiceError>),
    DeadlineExceeded,
    Panicked(String),
}

/// Run `engine` in its own task, holding `permit` until it ends, bounded by
/// [`DATA_OPERATION_DEADLINE`]. A control plane that stops waiting drops
/// this request, and with it any future it awaits: a WAL-G engine stopped
/// between moving the data directory aside and starting the server again
/// would leave the service down. Detached, it finishes, putting the
/// original back if it must.
async fn run_detached<T, F>(permit: tokio::sync::SemaphorePermit<'static>, engine: F) -> Detached<T>
where
    T: Send + 'static,
    F: std::future::Future<Output = Result<T, temps_service_protocol::NodeServiceError>>
        + Send
        + 'static,
{
    let task = tokio::spawn(async move {
        let _permit = permit;
        tokio::time::timeout(DATA_OPERATION_DEADLINE, engine).await
    });
    match task.await {
        Ok(Ok(outcome)) => Detached::Done(outcome),
        Ok(Err(_)) => Detached::DeadlineExceeded,
        Err(error) => Detached::Panicked(error.to_string()),
    }
}

/// Joins a container a PostgreSQL engine recreated to the overlay, between
/// create and start, as every container this agent creates.
fn overlay_hook(docker: &bollard::Docker) -> RecreateHook {
    let overlay_docker = docker.clone();
    Arc::new(move |id: String| {
        let docker = overlay_docker.clone();
        Box::pin(async move {
            if let Err(error) =
                crate::service_handlers::attach_to_overlay_if_present(&docker, &id).await
            {
                tracing::warn!(
                    container = %id,
                    %error,
                    "Failed to attach a recreated service container to the overlay; continuing \
                     single-host"
                );
            }
        })
    })
}

/// A recreated container starts without the routes to the other workers'
/// overlay ranges. Its name is the managed `postgres-<service>`, the only one
/// the WAL-G engines run on here.
async fn reroute_recreated_postgres(state: &AgentState, docker: &bollard::Docker, service: &str) {
    let container = format!("postgres-{service}");
    if let Err(error) = crate::service_handlers::install_overlay_peer_routes_after_start(
        docker,
        &container,
        &state.overlay_peers,
    )
    .await
    {
        tracing::warn!(
            container = %container,
            %error,
            "Failed to install overlay peer routes after recreating a service"
        );
    }
}

/// `POST /agent/services/postgres/walg-archive`: point a PostgreSQL
/// service's continuous WAL-G archiving at the request's S3 source, without
/// a base backup: the control plane's own `enable_continuous_archiving`, run
/// on this node. Sent for an operator's explicit re-point of the archive
/// source, which accepts the restart turning `archive_mode` on needs (the
/// running container's own specification recreated with archiving on,
/// under the recreate guard). Outcomes come back as 200 with `completed`,
/// `refused` (nothing was changed) or `failed`.
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services/postgres/walg-archive",
    request_body = PostgresWalgArchiveRequest,
    responses(
        (status = 200, description = "The engine ran; its outcome", body = AgentResponse<PostgresWalgArchiveResult>),
        (status = 401, description = "Unauthorized"),
        (status = 429, description = "This node already runs its maximum of backups and restores"),
        (status = 500, description = "The engine could not run"),
        (status = 503, description = "Docker is not available on this agent"),
        (status = 504, description = "Deadline exceeded")
    ),
    security(("bearer_auth" = []))
)]
pub async fn postgres_walg_archive(
    State(state): State<Arc<AgentState>>,
    Json(request): Json<PostgresWalgArchiveRequest>,
) -> axum::response::Response {
    let service = request.service_config.name.clone();
    let Some(permit) = admit(&DATA_OPERATIONS) else {
        return busy("re-point the archiving of", &service);
    };
    let Some(docker) = state.docker.as_ref() else {
        return docker_missing("re-point the archiving of", &service);
    };
    tracing::info!(
        service = %service,
        service_id = request.service_id,
        bucket = %request.destination.bucket,
        "Pointing WAL-G archiving of a service on this node at an S3 source"
    );
    let node = crate::service_handlers::node_attachment(
        &state,
        Some(temps_core::NETWORK_NAME.to_string()),
    );
    let after_create = overlay_hook(docker);
    let published_address =
        crate::service_handlers::published_probe_address(&state.host_bind_address());
    let runtime = state.service_runtime.clone();
    let engine_docker = Arc::new(docker.clone());
    let outcome = match run_detached(permit, async move {
        runtime
            .postgres_walg_archive(
                request,
                engine_docker,
                node,
                after_create,
                published_address,
            )
            .await
    })
    .await
    {
        Detached::Done(Ok(outcome)) => outcome,
        Detached::Done(Err(error)) => {
            tracing::error!(service = %service, %error, "WAL-G archiving could not be re-pointed");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("WAL-G archiving of service '{service}' could not be re-pointed: {error}"),
            )
            .into_response();
        }
        Detached::Panicked(error) => {
            tracing::error!(service = %service, %error, "WAL-G archiving re-point panicked");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "WAL-G archiving of service '{service}' stopped unexpectedly while being \
                     re-pointed: {error}"
                ),
            )
            .into_response();
        }
        Detached::DeadlineExceeded => return deadline_exceeded("WAL-G archive re-point", &service),
    };
    match &outcome {
        PostgresWalgArchiveResult::Completed { restarted } => {
            if *restarted {
                reroute_recreated_postgres(&state, docker, &service).await;
            }
            tracing::info!(service = %service, restarted, "WAL-G archiving re-pointed");
        }
        PostgresWalgArchiveResult::Refused { reason } => {
            tracing::warn!(service = %service, %reason, "WAL-G archive re-point refused");
        }
        PostgresWalgArchiveResult::Failed { reason } => {
            tracing::error!(service = %service, %reason, "WAL-G archive re-point failed");
        }
    }
    AgentResponse::ok(outcome).into_response()
}

/// `POST /agent/services/postgres/walg-credentials`: replace the credential
/// a PostgreSQL service's continuous archiving reads (`walg.env`) with the
/// request's, scoped by the control plane to the service's own WAL-G
/// repository (ADR-093). Nothing else changes: no restart, no setting.
/// Outcomes come back as 200 with `refreshed`, `refused` (archiving is not
/// set up here; nothing was written) or `failed`.
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services/postgres/walg-credentials",
    request_body = PostgresWalgCredentialsRequest,
    responses(
        (status = 200, description = "The engine ran; its outcome", body = AgentResponse<PostgresWalgCredentialsResult>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "The engine could not run"),
        (status = 503, description = "Docker is not available on this agent")
    ),
    security(("bearer_auth" = []))
)]
pub async fn postgres_walg_credentials(
    State(state): State<Arc<AgentState>>,
    Json(request): Json<PostgresWalgCredentialsRequest>,
) -> axum::response::Response {
    let service = request.service_config.name.clone();
    let Some(docker) = state.docker.as_ref() else {
        return docker_missing("refresh the archiving credential of", &service);
    };
    let published_address =
        crate::service_handlers::published_probe_address(&state.host_bind_address());
    // A file write, not a data operation: it takes no slot, so a node busy
    // with backups still keeps its archiving credential fresh.
    match state
        .service_runtime
        .postgres_walg_credentials(request, Arc::new(docker.clone()), published_address)
        .await
    {
        Ok(outcome) => {
            match &outcome {
                PostgresWalgCredentialsResult::Refreshed => {
                    tracing::info!(service = %service, "WAL-G archiving credential refreshed");
                }
                PostgresWalgCredentialsResult::Refused { reason } => {
                    tracing::info!(service = %service, %reason, "WAL-G archiving credential not refreshed");
                }
                PostgresWalgCredentialsResult::Failed { reason } => {
                    tracing::error!(service = %service, %reason, "WAL-G archiving credential refresh failed");
                }
            }
            AgentResponse::ok(outcome).into_response()
        }
        Err(error) => {
            tracing::error!(service = %service, %error, "WAL-G archiving credential could not be refreshed");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "the WAL-G archiving credential of service '{service}' could not be \
                     refreshed: {error}"
                ),
            )
            .into_response()
        }
    }
}

/// `POST /agent/services/postgres/walg-restore`: the control plane's WAL-G
/// restore (in place, or to a point in time) of a PostgreSQL service on
/// this node, run by the same engine as on the control plane.
///
/// The base backup is fetched into a staging directory on the data volume
/// while the server runs; the original data directory is kept aside until
/// the restored server has finished recovery and promoted, and is put back,
/// its container started as it was, when it does not. The recreated
/// containers are the running container's own specification (restart policy
/// included), attached to this node as every container this agent creates.
/// Engine outcomes come back as 200 with `completed`, `refused` (nothing was
/// changed) or `failed`.
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services/postgres/walg-restore",
    request_body = PostgresWalgRestoreRequest,
    responses(
        (status = 200, description = "The engine ran; its outcome", body = AgentResponse<PostgresWalgRestoreResult>),
        (status = 401, description = "Unauthorized"),
        (status = 429, description = "This node already runs its maximum of backups and restores"),
        (status = 500, description = "The engine could not run"),
        (status = 503, description = "Docker is not available on this agent"),
        (status = 504, description = "Restore deadline exceeded")
    ),
    security(("bearer_auth" = []))
)]
pub async fn postgres_walg_restore(
    State(state): State<Arc<AgentState>>,
    Json(request): Json<PostgresWalgRestoreRequest>,
) -> axum::response::Response {
    let service = request.live_service_config.name.clone();
    let Some(permit) = admit(&DATA_OPERATIONS) else {
        return busy("restore", &service);
    };
    let Some(docker) = state.docker.as_ref() else {
        return docker_missing("restore", &service);
    };
    tracing::info!(
        service = %service,
        service_id = request.service_id,
        bucket = %request.source.bucket,
        pitr = request.recovery_target.is_some(),
        "Restoring a service on this node from WAL-G"
    );
    let node = crate::service_handlers::node_attachment(
        &state,
        Some(temps_core::NETWORK_NAME.to_string()),
    );
    let after_create = overlay_hook(docker);
    let published_address =
        crate::service_handlers::published_probe_address(&state.host_bind_address());
    let runtime = state.service_runtime.clone();
    let engine_docker = Arc::new(docker.clone());
    let outcome = match run_detached(permit, async move {
        runtime
            .postgres_walg_restore(
                request,
                engine_docker,
                node,
                after_create,
                published_address,
            )
            .await
    })
    .await
    {
        Detached::Done(Ok(outcome)) => outcome,
        Detached::Done(Err(error)) => {
            tracing::error!(service = %service, %error, "WAL-G restore could not run");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("WAL-G restore of service '{service}' could not run: {error}"),
            )
            .into_response();
        }
        Detached::Panicked(error) => {
            tracing::error!(service = %service, %error, "WAL-G restore panicked");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("WAL-G restore of service '{service}' stopped unexpectedly: {error}"),
            )
            .into_response();
        }
        Detached::DeadlineExceeded => return deadline_exceeded("WAL-G restore", &service),
    };
    match &outcome {
        PostgresWalgRestoreResult::Completed => {
            reroute_recreated_postgres(&state, docker, &service).await;
            tracing::info!(service = %service, "WAL-G restore completed");
        }
        PostgresWalgRestoreResult::Refused { reason } => {
            tracing::warn!(service = %service, %reason, "WAL-G restore refused");
        }
        PostgresWalgRestoreResult::Failed { reason } => {
            // The original was started again in a recreated container.
            reroute_recreated_postgres(&state, docker, &service).await;
            tracing::error!(service = %service, %reason, "WAL-G restore failed");
        }
    }
    AgentResponse::ok(outcome).into_response()
}

/// `POST /agent/services/mongodb/backup`: back up a MongoDB service this
/// node runs with the control plane's own engine (ADR-099). The engine checks
/// the image for WAL-G as on the control plane: with it, `wal-g backup-push`
/// streams `mongodump --archive` into the service's WAL-G repository; without
/// it, a pinned `mongodump --archive --gzip` helper uploads one archive. The
/// service is never stopped. Engine outcomes come back as 200 with
/// `walg_stream`, `archive`, `refused` (retrying cannot help) or `failed`.
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services/mongodb/backup",
    request_body = MongodbBackupRequest,
    responses(
        (status = 200, description = "The engine ran; its outcome", body = AgentResponse<MongodbBackupResult>),
        (status = 401, description = "Unauthorized"),
        (status = 429, description = "This node already runs its maximum of backups and restores"),
        (status = 500, description = "The engine could not run"),
        (status = 503, description = "Docker is not available on this agent"),
        (status = 504, description = "Backup deadline exceeded")
    ),
    security(("bearer_auth" = []))
)]
pub async fn mongodb_backup(
    State(state): State<Arc<AgentState>>,
    Json(request): Json<MongodbBackupRequest>,
) -> axum::response::Response {
    let service = request.service_config.name.clone();
    let Some(permit) = admit(&DATA_OPERATIONS) else {
        return busy("back up", &service);
    };
    let Some(docker) = state.docker.as_ref() else {
        return docker_missing("back up", &service);
    };
    tracing::info!(
        service = %service,
        service_id = request.service_id,
        bucket = %request.destination.bucket,
        "Backing up a MongoDB service on this node with the control plane's engine"
    );
    let published_address =
        crate::service_handlers::published_probe_address(&state.host_bind_address());
    let runtime = state.service_runtime.clone();
    let engine_docker = Arc::new(docker.clone());
    let outcome = match run_detached(permit, async move {
        runtime
            .mongodb_backup(request, engine_docker, published_address)
            .await
    })
    .await
    {
        Detached::Done(Ok(outcome)) => outcome,
        Detached::Done(Err(error)) => {
            tracing::error!(service = %service, %error, "MongoDB backup could not run");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("MongoDB backup of service '{service}' could not run: {error}"),
            )
            .into_response();
        }
        Detached::Panicked(error) => {
            tracing::error!(service = %service, %error, "MongoDB backup panicked");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("MongoDB backup of service '{service}' stopped unexpectedly: {error}"),
            )
            .into_response();
        }
        Detached::DeadlineExceeded => return deadline_exceeded("MongoDB backup", &service),
    };
    match &outcome {
        MongodbBackupResult::WalgStream { .. } => {
            tracing::info!(service = %service, "MongoDB WAL-G stream backup completed");
        }
        MongodbBackupResult::Archive { key, size_bytes } => {
            tracing::info!(service = %service, %key, size_bytes, "MongoDB archive backup completed");
        }
        MongodbBackupResult::Refused { reason } => {
            tracing::warn!(service = %service, %reason, "MongoDB backup refused");
        }
        MongodbBackupResult::Failed { reason } => {
            tracing::error!(service = %service, %reason, "MongoDB backup failed");
        }
    }
    AgentResponse::ok(outcome).into_response()
}

/// `POST /agent/services/mongodb/restore`: restore a MongoDB service this
/// node runs, in place, with the control plane's own restore (the provider's
/// `restore_in_place`): `wal-g backup-fetch` in the container for a WAL-G
/// repository, a `mongorestore --drop` sidecar for an archive. Runs in a task
/// of its own, so a control plane that stops waiting cannot interrupt it.
/// Engine outcomes come back as 200 with `completed`, `refused` (nothing was
/// changed) or `failed`.
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services/mongodb/restore",
    request_body = MongodbRestoreRequest,
    responses(
        (status = 200, description = "The engine ran; its outcome", body = AgentResponse<MongodbRestoreResult>),
        (status = 401, description = "Unauthorized"),
        (status = 429, description = "This node already runs its maximum of backups and restores"),
        (status = 500, description = "The engine could not run"),
        (status = 503, description = "Docker is not available on this agent"),
        (status = 504, description = "Restore deadline exceeded")
    ),
    security(("bearer_auth" = []))
)]
pub async fn mongodb_restore(
    State(state): State<Arc<AgentState>>,
    Json(request): Json<MongodbRestoreRequest>,
) -> axum::response::Response {
    let service = request.service_config.name.clone();
    let Some(permit) = admit(&DATA_OPERATIONS) else {
        return busy("restore", &service);
    };
    let Some(docker) = state.docker.as_ref() else {
        return docker_missing("restore", &service);
    };
    tracing::info!(
        service = %service,
        service_id = request.service_id,
        bucket = %request.source.bucket,
        location = %request.location,
        "Restoring a MongoDB service on this node with the control plane's restore"
    );
    let published_address =
        crate::service_handlers::published_probe_address(&state.host_bind_address());
    let runtime = state.service_runtime.clone();
    let engine_docker = Arc::new(docker.clone());
    let outcome = match run_detached(permit, async move {
        runtime
            .mongodb_restore(request, engine_docker, published_address)
            .await
    })
    .await
    {
        Detached::Done(Ok(outcome)) => outcome,
        Detached::Done(Err(error)) => {
            tracing::error!(service = %service, %error, "MongoDB restore could not run");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("MongoDB restore of service '{service}' could not run: {error}"),
            )
            .into_response();
        }
        Detached::Panicked(error) => {
            tracing::error!(service = %service, %error, "MongoDB restore panicked");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("MongoDB restore of service '{service}' stopped unexpectedly: {error}"),
            )
            .into_response();
        }
        Detached::DeadlineExceeded => return deadline_exceeded("MongoDB restore", &service),
    };
    match &outcome {
        MongodbRestoreResult::Completed => {
            tracing::info!(service = %service, "MongoDB restore completed");
        }
        MongodbRestoreResult::Refused { reason } => {
            tracing::warn!(service = %service, %reason, "MongoDB restore refused");
        }
        MongodbRestoreResult::Failed { reason } => {
            tracing::error!(service = %service, %reason, "MongoDB restore failed");
        }
    }
    AgentResponse::ok(outcome).into_response()
}

/// Admit one Redis data operation and run it detached on this node's
/// Docker, with the same slot, deadline and error shapes as every data
/// operation. `Err` is the response to return as it is.
async fn run_redis_operation<T, F, Fut>(
    state: &AgentState,
    service: &str,
    verb: &str,
    label: &str,
    engine: F,
) -> Result<T, Box<axum::response::Response>>
where
    T: Send + 'static,
    F: FnOnce(Arc<dyn temps_service_protocol::NodeServiceRuntime>, Arc<bollard::Docker>) -> Fut,
    Fut: std::future::Future<Output = Result<T, temps_service_protocol::NodeServiceError>>
        + Send
        + 'static,
{
    let Some(permit) = admit(&DATA_OPERATIONS) else {
        return Err(Box::new(busy(verb, service)));
    };
    let Some(docker) = state.docker.as_ref() else {
        return Err(Box::new(docker_missing(verb, service)));
    };
    let future = engine(state.service_runtime.clone(), Arc::new(docker.clone()));
    match run_detached(permit, future).await {
        Detached::Done(Ok(outcome)) => Ok(outcome),
        Detached::Done(Err(error)) => {
            tracing::error!(service = %service, %error, "{label} could not run");
            Err(Box::new(
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("{label} of service '{service}' could not run: {error}"),
                )
                .into_response(),
            ))
        }
        Detached::Panicked(error) => {
            tracing::error!(service = %service, %error, "{label} panicked");
            Err(Box::new(
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("{label} of service '{service}' stopped unexpectedly: {error}"),
                )
                .into_response(),
            ))
        }
        Detached::DeadlineExceeded => Err(Box::new(deadline_exceeded(label, service))),
    }
}

/// Log a Redis engine outcome: `Some(reason)` with whether it was refused.
fn log_redis_outcome(service: &str, label: &str, failure: Option<(bool, &str)>) {
    match failure {
        None => tracing::info!(service = %service, "{label} completed"),
        Some((true, reason)) => tracing::warn!(service = %service, %reason, "{label} refused"),
        Some((false, reason)) => tracing::error!(service = %service, %reason, "{label} failed"),
    }
}

/// `POST /agent/services/redis/walg-backup`: the control plane's Redis
/// WAL-G stream backup of a Redis service on this node (ADR-099):
/// `wal-g backup-push` in the service's container, streaming
/// `redis-cli --rdb -` to the S3 source. Redis keeps running. The caller
/// names the service and the source; the container, repository and
/// command follow from them. Engine outcomes come back as 200 with
/// `completed`, `refused` (retrying cannot help) or `failed`.
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services/redis/walg-backup",
    request_body = RedisWalgBackupRequest,
    responses(
        (status = 200, description = "The engine ran; its outcome", body = AgentResponse<RedisWalgBackupResult>),
        (status = 401, description = "Unauthorized"),
        (status = 429, description = "This node already runs its maximum of backups and restores"),
        (status = 500, description = "The engine could not run"),
        (status = 503, description = "Docker is not available on this agent"),
        (status = 504, description = "Backup deadline exceeded")
    ),
    security(("bearer_auth" = []))
)]
pub async fn redis_walg_backup(
    State(state): State<Arc<AgentState>>,
    Json(request): Json<RedisWalgBackupRequest>,
) -> axum::response::Response {
    let service = request.service_config.name.clone();
    const LABEL: &str = "Redis WAL-G backup";
    tracing::info!(
        service = %service,
        service_id = request.service_id,
        bucket = %request.destination.bucket,
        "Backing up a Redis service on this node with WAL-G"
    );
    let outcome = match run_redis_operation(
        &state,
        &service,
        "back up",
        LABEL,
        |runtime, docker| async move { runtime.redis_walg_backup(request, docker).await },
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(response) => return *response,
    };
    log_redis_outcome(
        &service,
        LABEL,
        match &outcome {
            RedisWalgBackupResult::Completed => None,
            RedisWalgBackupResult::Refused { reason } => Some((true, reason)),
            RedisWalgBackupResult::Failed { reason } => Some((false, reason)),
        },
    );
    AgentResponse::ok(outcome).into_response()
}

/// `POST /agent/services/redis/rdb-backup`: the control plane's Redis RDB
/// backup (an image without WAL-G) of a Redis service on this node: the
/// pinned `redis-cli` helper takes a snapshot over the service network and
/// its gzipped output is streamed to the one key the request names, which
/// must be the service's own `dump.rdb.gz`. Redis keeps running.
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services/redis/rdb-backup",
    request_body = RedisRdbBackupRequest,
    responses(
        (status = 200, description = "The engine ran; its outcome", body = AgentResponse<RedisRdbBackupResult>),
        (status = 401, description = "Unauthorized"),
        (status = 429, description = "This node already runs its maximum of backups and restores"),
        (status = 500, description = "The engine could not run"),
        (status = 503, description = "Docker is not available on this agent"),
        (status = 504, description = "Backup deadline exceeded")
    ),
    security(("bearer_auth" = []))
)]
pub async fn redis_rdb_backup(
    State(state): State<Arc<AgentState>>,
    Json(request): Json<RedisRdbBackupRequest>,
) -> axum::response::Response {
    let service = request.service_config.name.clone();
    const LABEL: &str = "Redis RDB backup";
    tracing::info!(
        service = %service,
        service_id = request.service_id,
        bucket = %request.destination.bucket,
        key = %request.key,
        "Backing up a Redis service on this node with an RDB snapshot"
    );
    let outcome = match run_redis_operation(
        &state,
        &service,
        "back up",
        LABEL,
        |runtime, docker| async move { runtime.redis_rdb_backup(request, docker).await },
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(response) => return *response,
    };
    log_redis_outcome(
        &service,
        LABEL,
        match &outcome {
            RedisRdbBackupResult::Completed { .. } => None,
            RedisRdbBackupResult::Refused { reason } => Some((true, reason)),
            RedisRdbBackupResult::Failed { reason } => Some((false, reason)),
        },
    );
    AgentResponse::ok(outcome).into_response()
}

/// `POST /agent/services/redis/restore`: the control plane's in-place
/// restore of a Redis service on this node, from a WAL-G repository or an
/// RDB snapshot. The snapshot is staged on the data volume first; Redis is
/// stopped only to swap it in, the original data kept aside until the
/// restored server answers, and put back, the container started as it was,
/// when it does not. Detached from the request, so a control plane that
/// stops waiting cannot stop it between the swap and the start.
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services/redis/restore",
    request_body = RedisRestoreRequest,
    responses(
        (status = 200, description = "The engine ran; its outcome", body = AgentResponse<RedisRestoreResult>),
        (status = 401, description = "Unauthorized"),
        (status = 429, description = "This node already runs its maximum of backups and restores"),
        (status = 500, description = "The engine could not run"),
        (status = 503, description = "Docker is not available on this agent"),
        (status = 504, description = "Restore deadline exceeded")
    ),
    security(("bearer_auth" = []))
)]
pub async fn redis_restore(
    State(state): State<Arc<AgentState>>,
    Json(request): Json<RedisRestoreRequest>,
) -> axum::response::Response {
    let service = request.service_config.name.clone();
    const LABEL: &str = "Redis restore";
    tracing::info!(
        service = %service,
        service_id = request.service_id,
        bucket = %request.source.bucket,
        keep_target_bookkeeping = request.keep_target_bookkeeping,
        "Restoring a Redis service on this node in place"
    );
    let outcome = match run_redis_operation(
        &state,
        &service,
        "restore",
        LABEL,
        |runtime, docker| async move { runtime.redis_restore(request, docker).await },
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(response) => return *response,
    };
    log_redis_outcome(
        &service,
        LABEL,
        match &outcome {
            RedisRestoreResult::Completed => None,
            RedisRestoreResult::Refused { reason } => Some((true, reason)),
            RedisRestoreResult::Failed { reason } => Some((false, reason)),
        },
    );
    AgentResponse::ok(outcome).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_engines::{agent_state, plugin_builder, service_runtime};
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::post;
    use axum::Router;
    use tower::ServiceExt;

    fn app(state: Arc<AgentState>) -> Router {
        Router::new()
            .route("/agent/services/data-backup", post(data_backup))
            .route("/agent/services/data-restore", post(data_restore))
            .route(
                "/agent/services/postgres/walg-backup",
                post(postgres_walg_backup),
            )
            .route(
                "/agent/services/postgres/walg-restore",
                post(postgres_walg_restore),
            )
            .route(
                "/agent/services/postgres/walg-archive",
                post(postgres_walg_archive),
            )
            .route(
                "/agent/services/postgres/walg-credentials",
                post(postgres_walg_credentials),
            )
            .route("/agent/services/mongodb/backup", post(mongodb_backup))
            .route("/agent/services/mongodb/restore", post(mongodb_restore))
            .route("/agent/services/redis/walg-backup", post(redis_walg_backup))
            .route("/agent/services/redis/rdb-backup", post(redis_rdb_backup))
            .route("/agent/services/redis/restore", post(redis_restore))
            .with_state(state)
    }

    fn backup_body(extra: Option<(&str, serde_json::Value)>) -> serde_json::Value {
        let mut body = serde_json::json!({
            "service_config": {
                "name": "cache",
                "service_type": "redis",
                "version": null,
                "parameters": {"host": "localhost", "port": "6379"}
            },
            "destination": {
                "bucket": "backups",
                "region": "us-east-1",
                "force_path_style": true,
                "access_key_id": "a",
                "secret_access_key": "s"
            },
            "key": "redis/cache/u/dump.rdb.gz"
        });
        if let Some((field, value)) = extra {
            body[field] = value;
        }
        body
    }

    async fn post_json(
        router: Router,
        path: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let response = router
            .oneshot(
                Request::post(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    /// The caller cannot hand the agent a command, image or host of its own.
    #[tokio::test]
    async fn a_backup_takes_no_command_of_its_own() {
        for (field, value) in [
            ("command", serde_json::json!(["sh", "-c", "id"])),
            ("image", serde_json::json!("alpine")),
        ] {
            let (status, _) = post_json(
                app(agent_state(service_runtime(), plugin_builder())),
                "/agent/services/data-backup",
                backup_body(Some((field, value))),
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{field}");
        }
    }

    /// The engine runs (here the stub) and its failure comes back naming the
    /// service; restores take the same path.
    #[tokio::test]
    async fn the_engine_runs_on_this_node_and_its_failure_names_the_service() {
        let state = agent_state(service_runtime(), plugin_builder());
        let (status, json) = post_json(
            app(state.clone()),
            "/agent/services/data-backup",
            backup_body(None),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        let error = json["error"].as_str().unwrap_or_default();
        assert!(error.contains("Backup of service 'cache'"), "{error}");
        assert!(error.contains("backup_service"), "the stub ran: {error}");

        let (status, json) = post_json(
            app(state),
            "/agent/services/data-restore",
            serde_json::json!({
                "service_config": backup_body(None)["service_config"].clone(),
                "source": backup_body(None)["destination"].clone(),
                "location": "redis/cache/u/dump.rdb.gz"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(json["error"]
            .as_str()
            .unwrap_or_default()
            .contains("Restore of service 'cache'"));
    }

    fn walg_body() -> serde_json::Value {
        serde_json::json!({
            "service_id": 8,
            "node_id": 2,
            "service_config": {
                "name": "orders",
                "service_type": "postgres",
                "version": null,
                "parameters": {"host": "localhost", "port": "5432"}
            },
            "destination": backup_body(None)["destination"].clone(),
            "bucket_path": "base",
            "backup_uuid": "u-1",
            "allow_restart": false
        })
    }

    /// A WAL-G base backup names no command, image, path or attachment: the
    /// engine runs on this node with the attachment every container this
    /// agent creates gets, and a failure to run names the service.
    #[tokio::test]
    async fn a_walg_backup_runs_the_engine_with_this_nodes_attachment() {
        for field in ["command", "image", "walg_prefix", "host_ip"] {
            let mut body = walg_body();
            body[field] = serde_json::json!("x");
            let (status, _) = post_json(
                app(agent_state(service_runtime(), plugin_builder())),
                "/agent/services/postgres/walg-backup",
                body,
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{field}");
        }

        let runtime = Arc::new(crate::test_engines::StubRuntime::default());
        let state = agent_state(runtime.clone(), plugin_builder());
        let (status, json) = post_json(
            app(state.clone()),
            "/agent/services/postgres/walg-backup",
            walg_body(),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        let error = json["error"].as_str().unwrap_or_default();
        assert!(
            error.contains("WAL-G base backup of service 'orders' could not run"),
            "{error}"
        );
        let asked = runtime.walg_backups.lock().expect("walg_backups");
        let [(request, node, _)] = asked.as_slice() else {
            panic!("the engine runs once: {}", asked.len());
        };
        assert_eq!(request.service_id, 8);
        assert!(!request.allow_restart);
        assert_eq!(node.host_ip, state.host_bind_address());
        assert_eq!(
            node.network.as_deref(),
            Some(temps_core::NETWORK_NAME.as_str())
        );
    }

    /// Re-pointing archiving takes a service and a destination only, and
    /// runs the engine on this node with this node's attachment.
    #[tokio::test]
    async fn a_walg_archive_repoint_runs_the_engine_with_this_nodes_attachment() {
        let body = || {
            let mut body = walg_body();
            let object = body.as_object_mut().expect("an object");
            object.remove("backup_uuid");
            object.remove("allow_restart");
            body
        };
        for field in ["command", "walg_prefix", "allow_restart", "host_ip"] {
            let mut body = body();
            body[field] = serde_json::json!("x");
            let (status, _) = post_json(
                app(agent_state(service_runtime(), plugin_builder())),
                "/agent/services/postgres/walg-archive",
                body,
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{field}");
        }
        let runtime = Arc::new(crate::test_engines::StubRuntime::default());
        let state = agent_state(runtime.clone(), plugin_builder());
        let (status, json) = post_json(
            app(state.clone()),
            "/agent/services/postgres/walg-archive",
            body(),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        let error = json["error"].as_str().unwrap_or_default();
        assert!(
            error.contains("WAL-G archiving of service 'orders' could not be re-pointed"),
            "{error}"
        );
        let asked = runtime.walg_archives.lock().expect("walg_archives");
        let [(request, node)] = asked.as_slice() else {
            panic!("the engine runs once: {}", asked.len());
        };
        assert_eq!(request.service_id, 8);
        assert_eq!(request.bucket_path, "base");
        assert_eq!(node.host_ip, state.host_bind_address());
    }

    #[tokio::test]
    async fn an_archive_credential_refresh_runs_only_the_credential_engine() {
        let body = || {
            let mut body = walg_body();
            let object = body.as_object_mut().expect("an object");
            object.remove("backup_uuid");
            object.remove("allow_restart");
            object.remove("node_id");
            body
        };
        // Nothing a caller could steer the node with is accepted: no
        // command, prefix, restart or node.
        for field in ["command", "walg_prefix", "allow_restart", "node_id"] {
            let mut body = body();
            body[field] = serde_json::json!("x");
            let (status, _) = post_json(
                app(agent_state(service_runtime(), plugin_builder())),
                "/agent/services/postgres/walg-credentials",
                body,
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{field}");
        }
        let runtime = Arc::new(crate::test_engines::StubRuntime::default());
        let state = agent_state(runtime.clone(), plugin_builder());
        let (status, json) = post_json(
            app(state),
            "/agent/services/postgres/walg-credentials",
            body(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["data"]["status"], "refreshed", "{json}");
        let asked = runtime.walg_credentials.lock().expect("walg_credentials");
        let [request] = asked.as_slice() else {
            panic!("the engine runs once: {}", asked.len());
        };
        assert_eq!(request.service_id, 8);
        assert_eq!(request.bucket_path, "base");
        // A refresh never recreates: it asked for no archive re-point.
        assert!(runtime
            .walg_archives
            .lock()
            .expect("walg_archives")
            .is_empty());
    }

    fn walg_restore_body() -> serde_json::Value {
        let config = serde_json::json!({
            "name": "orders",
            "service_type": "postgres",
            "version": null,
            "parameters": {"host": "localhost", "port": "5432"}
        });
        serde_json::json!({
            "service_id": 8,
            "node_id": 2,
            "service_config": config.clone(),
            "live_service_config": config,
            "source": backup_body(None)["destination"].clone(),
            "walg_prefix": "s3://backups/external_services/postgres/orders/walg",
            "recovery_target": {"kind": "time", "time": "2026-11-07T10:00:00.123456Z"},
            "target_user_data": null
        })
    }

    /// A WAL-G restore names no command, image, path or attachment either:
    /// the engine runs on this node with this node's attachment, the typed
    /// recovery target arrives as sent, and a failure to run names the
    /// service.
    #[tokio::test]
    async fn a_walg_restore_runs_the_engine_with_this_nodes_attachment() {
        for field in ["command", "image", "pgdata", "host_ip"] {
            let mut body = walg_restore_body();
            body[field] = serde_json::json!("x");
            let (status, _) = post_json(
                app(agent_state(service_runtime(), plugin_builder())),
                "/agent/services/postgres/walg-restore",
                body,
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{field}");
        }

        let runtime = Arc::new(crate::test_engines::StubRuntime::default());
        let state = agent_state(runtime.clone(), plugin_builder());
        let (status, json) = post_json(
            app(state.clone()),
            "/agent/services/postgres/walg-restore",
            walg_restore_body(),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        let error = json["error"].as_str().unwrap_or_default();
        assert!(
            error.contains("WAL-G restore of service 'orders' could not run"),
            "{error}"
        );
        let asked = runtime.walg_restores.lock().expect("walg_restores");
        let [(request, node, _)] = asked.as_slice() else {
            panic!("the engine runs once: {}", asked.len());
        };
        assert_eq!(request.service_id, 8);
        assert!(matches!(
            &request.recovery_target,
            Some(temps_service_protocol::RecoveryTarget::Time { time })
                if time.to_rfc3339() == "2026-11-07T10:00:00.123456+00:00"
        ));
        assert_eq!(node.host_ip, state.host_bind_address());
        assert_eq!(
            node.network.as_deref(),
            Some(temps_core::NETWORK_NAME.as_str())
        );
    }

    fn mongodb_backup_body() -> serde_json::Value {
        serde_json::json!({
            "service_id": 8,
            "node_id": 2,
            "service_config": {
                "name": "orders",
                "service_type": "mongodb",
                "version": null,
                "parameters": {"host": "localhost", "port": "27017"}
            },
            "destination": backup_body(None)["destination"].clone(),
            "bucket_path": "base",
            "backup_uuid": "u-1",
            "mode": {
                "kind": "archive",
                "key": "base/external_services/mongodb/orders/2026/11/09/u-1/dump.archive"
            }
        })
    }

    fn mongodb_restore_body() -> serde_json::Value {
        serde_json::json!({
            "service_id": 8,
            "node_id": 2,
            "service_config": mongodb_backup_body()["service_config"].clone(),
            "origin_password": "origin",
            "source": backup_body(None)["destination"].clone(),
            "location": "s3://backups/external_services/mongodb/orders/walg"
        })
    }

    /// A MongoDB backup names no command, image, path, key or attachment:
    /// the engine runs on this node with the published address the node's
    /// own probes use, and a failure to run names the service.
    #[tokio::test]
    async fn a_mongodb_backup_runs_the_engine_and_takes_nothing_to_run() {
        for field in [
            "command",
            "image",
            "key",
            "walg_prefix",
            "host_ip",
            "container",
        ] {
            let mut body = mongodb_backup_body();
            body[field] = serde_json::json!("x");
            let (status, _) = post_json(
                app(agent_state(service_runtime(), plugin_builder())),
                "/agent/services/mongodb/backup",
                body,
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{field}");
        }
        // The control plane names the backup its credential was scoped to
        // (ADR-099, ADR-093): a request without one, or with a kind the node
        // does not know, is refused before anything runs.
        for mode in [
            serde_json::Value::Null,
            serde_json::json!({"kind": "anything"}),
            serde_json::json!({"kind": "archive"}),
        ] {
            let mut body = mongodb_backup_body();
            if mode.is_null() {
                body.as_object_mut().expect("an object").remove("mode");
            } else {
                body["mode"] = mode.clone();
            }
            let (status, _) = post_json(
                app(agent_state(service_runtime(), plugin_builder())),
                "/agent/services/mongodb/backup",
                body,
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{mode}");
        }

        let runtime = Arc::new(crate::test_engines::StubRuntime::default());
        let state = agent_state(runtime.clone(), plugin_builder());
        let (status, json) = post_json(
            app(state),
            "/agent/services/mongodb/backup",
            mongodb_backup_body(),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        let error = json["error"].as_str().unwrap_or_default();
        assert!(
            error.contains("MongoDB backup of service 'orders' could not run"),
            "{error}"
        );
        let asked = runtime.mongodb_backups.lock().expect("mongodb_backups");
        let [(request, _)] = asked.as_slice() else {
            panic!("the engine runs once: {}", asked.len());
        };
        assert_eq!(request.service_id, 8);
        assert_eq!(request.bucket_path, "base");
        assert_eq!(request.backup_uuid, "u-1");
        assert_eq!(
            request.mode,
            temps_service_protocol::MongodbBackupMode::Archive {
                key: "base/external_services/mongodb/orders/2026/11/09/u-1/dump.archive".into()
            }
        );
    }

    /// A MongoDB restore takes a service, a source and a location only; the
    /// origin's password arrives as sent, and a failure to run names the
    /// service.
    #[tokio::test]
    async fn a_mongodb_restore_runs_the_engine_and_takes_nothing_to_run() {
        for field in ["command", "image", "container", "host_ip", "sidecar_image"] {
            let mut body = mongodb_restore_body();
            body[field] = serde_json::json!("x");
            let (status, _) = post_json(
                app(agent_state(service_runtime(), plugin_builder())),
                "/agent/services/mongodb/restore",
                body,
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{field}");
        }

        let runtime = Arc::new(crate::test_engines::StubRuntime::default());
        let state = agent_state(runtime.clone(), plugin_builder());
        let (status, json) = post_json(
            app(state),
            "/agent/services/mongodb/restore",
            mongodb_restore_body(),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        let error = json["error"].as_str().unwrap_or_default();
        assert!(
            error.contains("MongoDB restore of service 'orders' could not run"),
            "{error}"
        );
        let asked = runtime.mongodb_restores.lock().expect("mongodb_restores");
        let [(request, _)] = asked.as_slice() else {
            panic!("the engine runs once: {}", asked.len());
        };
        assert_eq!(request.origin_password.as_deref(), Some("origin"));
        assert_eq!(
            request.location,
            "s3://backups/external_services/mongodb/orders/walg"
        );
    }

    /// An engine keeps running, and keeps its slot, when the request that
    /// started it is dropped: a WAL-G restore stopped mid-swap would leave
    /// the service down.
    #[tokio::test]
    async fn a_detached_engine_finishes_after_its_request_is_dropped() {
        static SLOTS: Semaphore = Semaphore::const_new(1);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let permit = admit(&SLOTS).expect("a free slot");
        let request = tokio::spawn(run_detached(permit, async move {
            let _ = started_tx.send(());
            let _ = release_rx.await;
            let _ = done_tx.send(());
            Ok::<_, temps_service_protocol::NodeServiceError>(())
        }));
        started_rx.await.expect("the engine started");
        request.abort();
        let _ = request.await;
        assert!(admit(&SLOTS).is_none(), "the engine still holds its slot");
        release_tx.send(()).expect("the engine is still running");
        done_rx
            .await
            .expect("the engine finished after the request was dropped");
        for _ in 0..50 {
            if admit(&SLOTS).is_some() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("the engine's slot was not released when it finished");
    }

    /// A node runs a bounded number of backups and restores at once; one
    /// more is refused as busy, naming the service, and admitted again once
    /// a slot frees.
    #[test]
    fn backups_beyond_the_node_limit_are_refused_as_busy() {
        let slots = Semaphore::new(MAX_DATA_OPERATIONS);
        let held: Vec<_> = (0..MAX_DATA_OPERATIONS)
            .map(|_| admit(&slots).expect("a free slot"))
            .collect();
        assert!(
            admit(&slots).is_none(),
            "the node limit must refuse one more"
        );
        let refusal = busy("back up", "cache");
        assert_eq!(refusal.status(), StatusCode::TOO_MANY_REQUESTS);
        drop(held);
        assert!(admit(&slots).is_some());
    }

    fn redis_config() -> serde_json::Value {
        serde_json::json!({
            "name": "cache",
            "service_type": "redis",
            "version": null,
            "parameters": {"host": "localhost", "port": "6379", "password": "pw"}
        })
    }

    /// Each Redis route's body, by path.
    fn redis_bodies() -> [(&'static str, serde_json::Value); 3] {
        let destination = backup_body(None)["destination"].clone();
        [
            (
                "/agent/services/redis/walg-backup",
                serde_json::json!({
                    "service_id": 9,
                    "service_config": redis_config(),
                    "destination": destination.clone(),
                    "bucket_path": "fleet",
                    "backup_uuid": "u-9"
                }),
            ),
            (
                "/agent/services/redis/rdb-backup",
                serde_json::json!({
                    "service_id": 9,
                    "service_config": redis_config(),
                    "destination": destination.clone(),
                    "key": "external_services/redis/cache/2026/11/08/u-9/dump.rdb.gz"
                }),
            ),
            (
                "/agent/services/redis/restore",
                serde_json::json!({
                    "service_id": 9,
                    "service_config": redis_config(),
                    "source": destination,
                    "location": "s3://backups/external_services/redis/cache/walg",
                    "keep_target_bookkeeping": true
                }),
            ),
        ]
    }

    /// The Redis routes take a service and a source only: a command, image,
    /// container, prefix or host of the caller's own is rejected before
    /// anything runs.
    #[tokio::test]
    async fn the_redis_routes_take_no_command_image_or_container() {
        for (path, body) in redis_bodies() {
            for field in [
                "command",
                "image",
                "container_name",
                "walg_prefix",
                "host_ip",
            ] {
                let mut body = body.clone();
                body[field] = serde_json::json!("x");
                let runtime = Arc::new(crate::test_engines::StubRuntime::default());
                let (status, _) = post_json(
                    app(agent_state(runtime.clone(), plugin_builder())),
                    path,
                    body,
                )
                .await;
                assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{path} {field}");
                assert!(runtime.redis_walg_backups.lock().expect("walg").is_empty());
                assert!(runtime.redis_rdb_backups.lock().expect("rdb").is_empty());
                assert!(runtime.redis_restores.lock().expect("restore").is_empty());
            }
        }
    }

    /// Each Redis route runs the control plane's engine once on this node,
    /// with the request as sent, and a failure to run names the service.
    #[tokio::test]
    async fn each_redis_route_runs_the_engine_once_and_names_the_service() {
        for (path, body) in redis_bodies() {
            let runtime = Arc::new(crate::test_engines::StubRuntime::default());
            let (status, json) = post_json(
                app(agent_state(runtime.clone(), plugin_builder())),
                path,
                body,
            )
            .await;
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{path}");
            let error = json["error"].as_str().unwrap_or_default();
            assert!(
                error.contains("of service 'cache' could not run"),
                "{error}"
            );
            assert!(
                error.contains("in the test engine"),
                "the stub ran: {error}"
            );
            let ran = (
                runtime.redis_walg_backups.lock().expect("walg").len(),
                runtime.redis_rdb_backups.lock().expect("rdb").len(),
                runtime.redis_restores.lock().expect("restore").len(),
            );
            match path {
                "/agent/services/redis/walg-backup" => {
                    assert_eq!(ran, (1, 0, 0));
                    let asked = runtime.redis_walg_backups.lock().expect("walg");
                    assert_eq!(asked[0].bucket_path, "fleet");
                    assert_eq!(asked[0].backup_uuid, "u-9");
                }
                "/agent/services/redis/rdb-backup" => assert_eq!(ran, (0, 1, 0)),
                _ => {
                    assert_eq!(ran, (0, 0, 1));
                    assert!(
                        runtime.redis_restores.lock().expect("restore")[0].keep_target_bookkeeping
                    );
                }
            }
        }
    }
}
