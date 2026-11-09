// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The control plane's MariaDB backup and restore engines, run on this node
//! for a MariaDB it hosts (ADR-099): physical (WAL-G) base backups, logical
//! dumps, the staged in-place restore and point-in-time recovery, and binary
//! log shipping.
//!
//! Each route takes a sealed body that names a service and its S3 source,
//! never a command, image, path or attachment; unknown fields are refused.
//! The operations share the node's [`MAX_DATA_OPERATIONS`] slots and the
//! deadline with every other backup and restore, and run in their own task,
//! so a control plane that stops waiting never leaves a restore half done.
//! Outcomes come back as 200 with `completed`, `refused` (nothing was
//! changed) or `failed`.
//!
//! [`MAX_DATA_OPERATIONS`]: crate::service_data_handlers::MAX_DATA_OPERATIONS

use std::future::Future;
use std::net::IpAddr;
use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use bollard::Docker;
use temps_service_protocol::{
    MariadbBinlogArchiveRequest, MariadbBinlogArchiveResult, MariadbDumpBackupRequest,
    MariadbDumpBackupResult, MariadbPhysicalBackupRequest, MariadbPhysicalBackupResult,
    MariadbRestoreRequest, MariadbRestoreResult, NodeServiceError, NodeServiceRuntime,
};

use crate::handlers::{error_response, AgentResponse, AgentState};
use crate::service_data_handlers::{
    admit, busy, deadline_exceeded, docker_missing, run_detached, Detached, DATA_OPERATIONS,
};

/// Run `engine` with this node's runtime and Docker in a data-operation
/// slot, detached, or the response that says why it could not run (boxed:
/// a response is large, and is only built on the way out).
async fn run_on_node<T, F, Fut>(
    state: &AgentState,
    operation: &'static str,
    service: &str,
    engine: F,
) -> Result<T, Box<axum::response::Response>>
where
    T: Send + 'static,
    F: FnOnce(Arc<dyn NodeServiceRuntime>, Arc<Docker>, Option<IpAddr>) -> Fut,
    Fut: Future<Output = Result<T, NodeServiceError>> + Send + 'static,
{
    let Some(permit) = admit(&DATA_OPERATIONS) else {
        return Err(Box::new(busy(operation, service)));
    };
    let Some(docker) = state.docker.as_ref() else {
        return Err(Box::new(docker_missing(operation, service)));
    };
    let published_address =
        crate::service_handlers::published_probe_address(&state.host_bind_address());
    let future = engine(
        state.service_runtime.clone(),
        Arc::new(docker.clone()),
        published_address,
    );
    match run_detached(permit, future).await {
        Detached::Done(Ok(outcome)) => Ok(outcome),
        Detached::Done(Err(error)) => {
            tracing::error!(service, operation, %error, "MariaDB operation could not run");
            Err(Box::new(
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("MariaDB {operation} of service '{service}' could not run: {error}"),
                )
                .into_response(),
            ))
        }
        Detached::Panicked(error) => {
            tracing::error!(service, operation, %error, "MariaDB operation panicked");
            Err(Box::new(
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!(
                        "MariaDB {operation} of service '{service}' stopped unexpectedly: {error}"
                    ),
                )
                .into_response(),
            ))
        }
        Detached::DeadlineExceeded => Err(Box::new(deadline_exceeded(operation, service))),
    }
}

/// A restore stops and starts the container, which then has none of the
/// routes to the other workers' overlay ranges.
async fn reroute_restarted_mariadb(state: &AgentState, service: &str) {
    let Some(docker) = state.docker.as_ref() else {
        return;
    };
    let container = format!("mariadb-{service}");
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
            "Failed to install overlay peer routes after a MariaDB restore restarted it"
        );
    }
}

/// `POST /agent/services/mariadb/physical-backup`: a WAL-G physical base
/// backup with the control plane's `mariadb_physical` engine.
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services/mariadb/physical-backup",
    request_body = MariadbPhysicalBackupRequest,
    responses(
        (status = 200, description = "The engine ran; its outcome", body = AgentResponse<MariadbPhysicalBackupResult>),
        (status = 401, description = "Unauthorized"),
        (status = 429, description = "This node already runs its maximum of backups and restores"),
        (status = 500, description = "The engine could not run"),
        (status = 503, description = "Docker is not available on this agent"),
        (status = 504, description = "Backup deadline exceeded")
    ),
    security(("bearer_auth" = []))
)]
pub async fn mariadb_physical_backup(
    State(state): State<Arc<AgentState>>,
    Json(request): Json<MariadbPhysicalBackupRequest>,
) -> axum::response::Response {
    let service = request.service_config.name.clone();
    tracing::info!(
        service = %service,
        service_id = request.service_id,
        bucket = %request.destination.bucket,
        "Taking a MariaDB physical backup on this node"
    );
    match run_on_node(
        &state,
        "physical backup",
        &service,
        move |runtime, docker, published| async move {
            runtime
                .mariadb_physical_backup(request, docker, published)
                .await
        },
    )
    .await
    {
        Ok(outcome) => {
            if let MariadbPhysicalBackupResult::Failed { reason }
            | MariadbPhysicalBackupResult::Refused { reason } = &outcome
            {
                tracing::warn!(service = %service, %reason, "MariaDB physical backup did not complete");
            }
            AgentResponse::ok(outcome).into_response()
        }
        Err(response) => *response,
    }
}

/// `POST /agent/services/mariadb/dump-backup`: a logical dump with the
/// control plane's `mariadb_dump` engine, streamed to S3.
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services/mariadb/dump-backup",
    request_body = MariadbDumpBackupRequest,
    responses(
        (status = 200, description = "The engine ran; its outcome", body = AgentResponse<MariadbDumpBackupResult>),
        (status = 401, description = "Unauthorized"),
        (status = 429, description = "This node already runs its maximum of backups and restores"),
        (status = 500, description = "The engine could not run"),
        (status = 503, description = "Docker is not available on this agent"),
        (status = 504, description = "Backup deadline exceeded")
    ),
    security(("bearer_auth" = []))
)]
pub async fn mariadb_dump_backup(
    State(state): State<Arc<AgentState>>,
    Json(request): Json<MariadbDumpBackupRequest>,
) -> axum::response::Response {
    let service = request.service_config.name.clone();
    tracing::info!(
        service = %service,
        service_id = request.service_id,
        bucket = %request.destination.bucket,
        "Dumping a MariaDB on this node"
    );
    match run_on_node(
        &state,
        "dump",
        &service,
        move |runtime, docker, published| async move {
            runtime
                .mariadb_dump_backup(request, docker, published)
                .await
        },
    )
    .await
    {
        Ok(outcome) => {
            if let MariadbDumpBackupResult::Failed { reason }
            | MariadbDumpBackupResult::Refused { reason } = &outcome
            {
                tracing::warn!(service = %service, %reason, "MariaDB dump did not complete");
            }
            AgentResponse::ok(outcome).into_response()
        }
        Err(response) => *response,
    }
}

/// `POST /agent/services/mariadb/restore`: the control plane's in-place
/// restore or point-in-time recovery: the staged WAL-G swap, the original
/// put back when the restored server does not come up.
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services/mariadb/restore",
    request_body = MariadbRestoreRequest,
    responses(
        (status = 200, description = "The engine ran; its outcome", body = AgentResponse<MariadbRestoreResult>),
        (status = 401, description = "Unauthorized"),
        (status = 429, description = "This node already runs its maximum of backups and restores"),
        (status = 500, description = "The engine could not run"),
        (status = 503, description = "Docker is not available on this agent"),
        (status = 504, description = "Restore deadline exceeded")
    ),
    security(("bearer_auth" = []))
)]
pub async fn mariadb_restore(
    State(state): State<Arc<AgentState>>,
    Json(request): Json<MariadbRestoreRequest>,
) -> axum::response::Response {
    let service = request.service_config.name.clone();
    tracing::info!(
        service = %service,
        service_id = request.service_id,
        bucket = %request.source.bucket,
        pitr = request.recovery_target.is_some(),
        "Restoring a MariaDB on this node"
    );
    let outcome = match run_on_node(
        &state,
        "restore",
        &service,
        move |runtime, docker, published| async move {
            runtime.mariadb_restore(request, docker, published).await
        },
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(response) => return *response,
    };
    match &outcome {
        MariadbRestoreResult::Completed => {
            reroute_restarted_mariadb(&state, &service).await;
            tracing::info!(service = %service, "MariaDB restore completed");
        }
        MariadbRestoreResult::Refused { reason } => {
            tracing::warn!(service = %service, %reason, "MariaDB restore refused");
        }
        MariadbRestoreResult::Failed { reason } => {
            // The original was started again, or the reason says otherwise.
            reroute_restarted_mariadb(&state, &service).await;
            tracing::error!(service = %service, %reason, "MariaDB restore failed");
        }
    }
    AgentResponse::ok(outcome).into_response()
}

/// `POST /agent/services/mariadb/binlog-archive`: ship the closed binary
/// logs with the control plane's archiver.
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services/mariadb/binlog-archive",
    request_body = MariadbBinlogArchiveRequest,
    responses(
        (status = 200, description = "The archiver ran; its outcome", body = AgentResponse<MariadbBinlogArchiveResult>),
        (status = 401, description = "Unauthorized"),
        (status = 429, description = "This node already runs its maximum of backups and restores"),
        (status = 500, description = "The archiver could not run"),
        (status = 503, description = "Docker is not available on this agent"),
        (status = 504, description = "Deadline exceeded")
    ),
    security(("bearer_auth" = []))
)]
pub async fn mariadb_binlog_archive(
    State(state): State<Arc<AgentState>>,
    Json(request): Json<MariadbBinlogArchiveRequest>,
) -> axum::response::Response {
    let service = request.service_config.name.clone();
    match run_on_node(
        &state,
        "binary-log archiving",
        &service,
        move |runtime, docker, published| async move {
            runtime
                .mariadb_binlog_archive(request, docker, published)
                .await
        },
    )
    .await
    {
        Ok(outcome) => AgentResponse::ok(outcome).into_response(),
        Err(response) => *response,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_engines::{agent_state, plugin_builder, StubRuntime};
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::post;
    use axum::Router;
    use tower::ServiceExt;

    const ROUTES: [&str; 4] = [
        "/agent/services/mariadb/physical-backup",
        "/agent/services/mariadb/dump-backup",
        "/agent/services/mariadb/restore",
        "/agent/services/mariadb/binlog-archive",
    ];

    fn app(state: Arc<AgentState>) -> Router {
        Router::new()
            .route(ROUTES[0], post(mariadb_physical_backup))
            .route(ROUTES[1], post(mariadb_dump_backup))
            .route(ROUTES[2], post(mariadb_restore))
            .route(ROUTES[3], post(mariadb_binlog_archive))
            .with_state(state)
    }

    fn destination() -> serde_json::Value {
        serde_json::json!({
            "bucket": "backups",
            "region": "us-east-1",
            "force_path_style": true,
            "access_key_id": "a",
            "secret_access_key": "s"
        })
    }

    fn body(route: &str) -> serde_json::Value {
        let mut body = serde_json::json!({
            "service_id": 8,
            "node_id": 2,
            "service_config": {
                "name": "shop",
                "service_type": "mariadb",
                "version": null,
                "parameters": {"root_password": "r"}
            },
            "bucket_path": "base"
        });
        match route {
            "/agent/services/mariadb/physical-backup" => {
                body["destination"] = destination();
                body["backup_uuid"] = serde_json::json!("u-1");
            }
            "/agent/services/mariadb/dump-backup" => {
                body["destination"] = destination();
                body["key"] =
                    serde_json::json!("base/external_services/mariadb/shop/u/dump.sql.gz");
            }
            "/agent/services/mariadb/restore" => {
                body["source"] = destination();
                body["location"] =
                    serde_json::json!("s3://backups/base/external_services/mariadb/shop/walg");
            }
            _ => body["destination"] = destination(),
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

    /// No route takes a command, image, path or container of the caller's.
    #[tokio::test]
    async fn no_mariadb_route_takes_a_command_of_its_own() {
        for route in ROUTES {
            for field in ["command", "image", "walg_prefix", "container", "datadir"] {
                let runtime = Arc::new(StubRuntime::default());
                let mut request = body(route);
                request[field] = serde_json::json!("x");
                let (status, _) = post_json(
                    app(agent_state(runtime.clone(), plugin_builder())),
                    route,
                    request,
                )
                .await;
                assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{route} {field}");
                assert!(
                    runtime
                        .mariadb_operations
                        .lock()
                        .expect("operations")
                        .is_empty(),
                    "{route} {field}: nothing ran"
                );
            }
        }
    }

    /// Each route runs its own engine operation on this node, and a failure
    /// to run names the operation and the service.
    #[tokio::test]
    async fn each_route_runs_its_engine_and_a_failure_names_the_service() {
        for (route, operation) in ROUTES.iter().zip([
            "physical-backup",
            "dump-backup",
            "restore",
            "binlog-archive",
        ]) {
            let runtime = Arc::new(StubRuntime::default());
            let (status, json) = post_json(
                app(agent_state(runtime.clone(), plugin_builder())),
                route,
                body(route),
            )
            .await;
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{route}");
            let error = json["error"].as_str().unwrap_or_default();
            assert!(error.contains("service 'shop'"), "{route}: {error}");
            assert!(
                error.contains("test engine"),
                "{route}: the stub ran: {error}"
            );
            assert_eq!(
                *runtime.mariadb_operations.lock().expect("operations"),
                vec![(operation, 8)],
                "{route}"
            );
        }
    }
}
