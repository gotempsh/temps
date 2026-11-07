// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Read-only observation of the managed services a worker runs: container
//! runtime state, resource usage samples and the Postgres WAL probe.
//!
//! The control plane has no Docker handle on a worker (and none at all on a
//! `--profile control-plane` server), so the service page's runtime, stats
//! and WAL panels ask the owning node's agent. These routes sit behind the
//! same bearer authentication as every other `/agent/services/*` route, read
//! only, and accept no command, host or query from the caller.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use bollard::query_parameters::InspectContainerOptions;
use serde::Deserialize;
use temps_service_protocol::{
    sample_container_stats, RemoteWalHealthRequest, RemoteWalHealthResponse,
    ServiceContainerRuntime, ServiceType, MAX_SERVICE_STATS_SAMPLES,
};

use crate::handlers::{error_response, AgentResponse, AgentState};

/// The longest container name accepted here. Docker's own limit is lower in
/// practice; this only bounds what is echoed back in errors.
const MAX_CONTAINER_NAME_LEN: usize = 255;

/// Whether `name` is a container name Docker could have created: a letter or
/// digit, then letters, digits, `_`, `.` or `-`. The name goes into a Docker
/// API path, so anything else (a decoded `/`, `?`, `..`) is refused before it
/// reaches the daemon.
fn is_container_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    name.len() <= MAX_CONTAINER_NAME_LEN
        && first.is_ascii_alphanumeric()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

fn refuse_name(name: &str) -> axum::response::Response {
    let shown: String = name.chars().take(64).collect();
    error_response(
        StatusCode::BAD_REQUEST,
        format!("'{shown}' is not a service container name"),
    )
    .into_response()
}

fn docker_missing(operation: &str, name: &str) -> axum::response::Response {
    error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        format!("Docker is not available on this agent to {operation} service '{name}'"),
    )
    .into_response()
}

fn is_not_found(error: &bollard::errors::Error) -> bool {
    matches!(
        error,
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}

/// Lifecycle state of a service container: status, restarts, OOM kill, exit
/// code and the limits applied. Never its environment or command, which hold
/// the service's credentials. A missing container is `found: false`.
#[utoipa::path(
    tag = "Services",
    get,
    path = "/agent/services/{name}/runtime",
    params(("name" = String, Path, description = "Service container name")),
    responses(
        (status = 200, description = "Container runtime state", body = AgentResponse<ServiceContainerRuntime>),
        (status = 400, description = "Not a container name"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Inspection failed"),
        (status = 503, description = "Docker is not available on this agent")
    ),
    security(("bearer_auth" = []))
)]
pub async fn service_runtime(
    State(state): State<Arc<AgentState>>,
    Path(name): Path<String>,
) -> axum::response::Response {
    if !is_container_name(&name) {
        return refuse_name(&name);
    }
    let Some(docker) = state.docker.as_ref() else {
        return docker_missing("inspect", &name);
    };
    match docker
        .inspect_container(&name, None::<InspectContainerOptions>)
        .await
    {
        Ok(info) => AgentResponse::ok(ServiceContainerRuntime::from_inspect(&info)).into_response(),
        Err(error) if is_not_found(&error) => {
            AgentResponse::ok(ServiceContainerRuntime::default()).into_response()
        }
        Err(error) => {
            tracing::warn!(service = %name, error = %error, "Failed to inspect service container");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to inspect service container '{name}': {error}"),
            )
            .into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ServiceStatsQuery {
    /// How many one-shot samples to take, a second apart: 1 or 2. Two give
    /// a CPU percentage on their own; one suits a poller that keeps the
    /// previous sample. Defaults to 2.
    #[serde(default)]
    samples: Option<usize>,
}

/// Raw one-shot `docker stats` samples of a service container, oldest
/// first, a second apart. The control plane computes CPU and memory from
/// them with the same formula it uses for its own containers.
///
/// Not in the agent's OpenAPI document: the samples are bollard's
/// `ContainerStatsResponse`, which has no schema (as with
/// `/agent/containers/{id}/stats`).
pub async fn service_stats(
    State(state): State<Arc<AgentState>>,
    Path(name): Path<String>,
    Query(query): Query<ServiceStatsQuery>,
) -> axum::response::Response {
    if !is_container_name(&name) {
        return refuse_name(&name);
    }
    let Some(docker) = state.docker.as_ref() else {
        return docker_missing("sample", &name);
    };
    let samples = query
        .samples
        .unwrap_or(MAX_SERVICE_STATS_SAMPLES)
        .clamp(1, MAX_SERVICE_STATS_SAMPLES);
    match sample_container_stats(docker, &name, samples).await {
        Ok(stats) => AgentResponse::ok(stats).into_response(),
        Err(error) => {
            tracing::warn!(service = %name, error = %error, "Failed to sample service container stats");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to sample stats of service container '{name}': {error}"),
            )
            .into_response()
        }
    }
}

/// Run the Postgres WAL and archive probe against a database this node runs.
///
/// Takes the stored service configuration (sealed to the node: it holds the
/// password) and nothing else. The host must be loopback and is pointed at
/// this node's own published address, the port must be the one the service's
/// container publishes, and the queries are fixed in the engine.
#[utoipa::path(
    tag = "Services",
    post,
    path = "/agent/services/wal-health",
    request_body = RemoteWalHealthRequest,
    responses(
        (status = 200, description = "Probe ran; a snapshot, or why there is none", body = AgentResponse<RemoteWalHealthResponse>),
        (status = 400, description = "Not a Postgres service"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Probe refused or failed"),
        (status = 503, description = "Docker is not available on this agent")
    ),
    security(("bearer_auth" = []))
)]
pub async fn wal_health(
    State(state): State<Arc<AgentState>>,
    Json(request): Json<RemoteWalHealthRequest>,
) -> axum::response::Response {
    let service_name = request.service_config.name.clone();
    if request.service_config.service_type != ServiceType::Postgres {
        return error_response(
            StatusCode::BAD_REQUEST,
            format!(
                "The WAL probe only runs for Postgres services; '{}' is {}",
                service_name, request.service_config.service_type
            ),
        )
        .into_response();
    }
    let Some(docker) = state.docker.as_ref() else {
        return docker_missing("probe the WAL of", &service_name);
    };
    // Where this node publishes service ports, from its own configuration.
    let published_address =
        crate::service_handlers::published_probe_address(&state.host_bind_address());
    match state
        .service_runtime
        .probe_postgres_wal(
            request.service_config,
            Arc::new(docker.clone()),
            published_address,
        )
        .await
    {
        Ok(response) => AgentResponse::ok(response).into_response(),
        Err(error) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("WAL probe failed for service '{service_name}': {error}"),
        )
        .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_engines::{
        agent_state, plugin_builder, service_runtime as stub_service_runtime,
    };
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::{get, post};
    use axum::Router;
    use tower::ServiceExt;

    fn app() -> Router {
        Router::new()
            .route("/agent/services/{name}/runtime", get(service_runtime))
            .route("/agent/services/{name}/stats", get(service_stats))
            .route("/agent/services/wal-health", post(wal_health))
            .with_state(agent_state(stub_service_runtime(), plugin_builder()))
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("read the body");
        serde_json::from_slice(&bytes).expect("a JSON body")
    }

    #[test]
    fn only_docker_container_names_are_accepted() {
        for name in ["postgres-orders", "temps-mongodb-a.b_C-1", "a"] {
            assert!(is_container_name(name), "{name}");
        }
        for name in [
            "",
            "../images/json",
            "x/../../info",
            "name?all=1",
            "-leading-dash",
            ".hidden",
            "spa ce",
        ] {
            assert!(!is_container_name(name), "{name}");
        }
        assert!(!is_container_name(&"a".repeat(MAX_CONTAINER_NAME_LEN + 1)));
    }

    /// A name that decodes to a path is refused before it can be spliced
    /// into a Docker API request.
    #[tokio::test]
    async fn an_encoded_path_is_not_sent_to_docker() {
        for route in ["runtime", "stats"] {
            let response = app()
                .oneshot(
                    Request::get(format!("/agent/services/..%2F..%2Finfo/{route}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{route}");
        }
    }

    /// The probe is for Postgres only, refused before any engine runs.
    #[tokio::test]
    async fn the_wal_probe_refuses_other_engines() {
        let body = serde_json::json!({
            "service_config": {
                "name": "cache",
                "service_type": "redis",
                "version": null,
                "parameters": {"host": "localhost", "port": "6379"}
            }
        });
        let response = app()
            .oneshot(
                Request::post("/agent/services/wal-health")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let json = body_json(response).await;
        assert!(
            json["error"].as_str().unwrap_or_default().contains("cache"),
            "{json}"
        );
    }

    /// The request names a service and nothing else: no host, port or query
    /// field of its own is accepted.
    #[tokio::test]
    async fn the_wal_probe_takes_no_target_of_its_own() {
        let body = serde_json::json!({
            "service_config": {
                "name": "orders",
                "service_type": "postgres",
                "version": null,
                "parameters": {}
            },
            "query": "SELECT 1"
        });
        let response = app()
            .oneshot(
                Request::post("/agent/services/wal-health")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    /// The engine's refusal (here the stub's) comes back as the agent's
    /// error, naming the service.
    #[tokio::test]
    async fn an_engine_failure_names_the_service() {
        let body = serde_json::json!({
            "service_config": {
                "name": "orders",
                "service_type": "postgres",
                "version": null,
                "parameters": {"host": "localhost", "port": "5561"}
            }
        });
        let response = app()
            .oneshot(
                Request::post("/agent/services/wal-health")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let json = body_json(response).await;
        assert!(
            json["error"]
                .as_str()
                .unwrap_or_default()
                .contains("orders"),
            "{json}"
        );
    }

    /// Inspection of an existing container keeps status fields only.
    #[test]
    fn runtime_never_carries_the_container_environment() {
        let info: bollard::models::ContainerInspectResponse =
            serde_json::from_value(serde_json::json!({
                "Id": "abc123",
                "RestartCount": 3,
                "Image": "sha256:feed",
                "State": {"Status": "running", "OOMKilled": true, "ExitCode": 0},
                "HostConfig": {"Memory": 536870912, "NanoCpus": 0},
                "Config": {"Env": ["POSTGRES_PASSWORD=hunter2"], "Cmd": ["postgres"]}
            }))
            .expect("an inspect response");
        let runtime = ServiceContainerRuntime::from_inspect(&info);
        assert!(runtime.found);
        assert_eq!(runtime.status.as_deref(), Some("running"));
        assert_eq!(runtime.restart_count, Some(3));
        assert_eq!(runtime.oom_killed, Some(true));
        assert_eq!(runtime.memory_bytes, Some(536870912));
        assert_eq!(runtime.nano_cpus, None, "0 means unlimited");
        let wire = serde_json::to_string(&runtime).expect("serialize");
        assert!(!wire.contains("hunter2"), "{wire}");
    }
}
