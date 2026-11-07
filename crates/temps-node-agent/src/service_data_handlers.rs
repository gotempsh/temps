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
    RemoteServiceBackupRequest, RemoteServiceBackupResponse, RemoteServiceRestoreRequest,
    RemoteServiceRestoreResponse,
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
}
