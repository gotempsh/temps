// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Agent HTTP server setup and routing.

use axum::{
    middleware,
    routing::{delete, get, post},
    Extension, Router,
};
use std::sync::Arc;
use std::time::Duration;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::auth::{require_agent_auth, AgentAuth};
use crate::handlers::{self, AgentApiDoc, AgentState};
use crate::sandbox_handlers::{self, SandboxHost, SandboxHostState, SANDBOX_UPLOAD_BODY_LIMIT};
use crate::service_handlers;
use crate::AgentConfig;
use temps_deployer::docker_socket_grant::DockerSocketGrant;
use temps_deployer::{ContainerDeployer, ImageBuilder};

/// The node's container platform once discovered, shared between the HTTP
/// handlers and the heartbeat loop.
///
/// `None` means "not discovered yet" — the heartbeat loop keeps retrying and
/// fills it in. A plain `Mutex` is right here: it's read once per request and
/// written at most once, so there is no contention to design around.
pub type SharedPlatform = Arc<std::sync::Mutex<Option<String>>>;

/// Read the discovered platform, treating a poisoned lock as "unknown" rather
/// than panicking a request handler.
pub fn read_platform(platform: &SharedPlatform) -> Option<String> {
    platform.lock().ok().and_then(|p| p.clone())
}

/// Record a discovered platform. A poisoned lock is ignored — the next
/// heartbeat retries anyway.
fn store_platform(platform: &SharedPlatform, value: String) {
    if let Ok(mut slot) = platform.lock() {
        *slot = Some(value);
    }
}

/// Network state the sync loop keeps current and request handlers read.
#[derive(Clone)]
pub struct SharedNetwork {
    pub overlay_bridge_address: Arc<std::sync::RwLock<Option<std::net::IpAddr>>>,
    pub overlay_peers: crate::network_sync::SharedPeers,
    pub host_bind_address: crate::network_sync::SharedBindAddress,
}

/// Build the agent Axum router with authentication middleware.
pub fn build_router(
    container_deployer: Arc<dyn ContainerDeployer>,
    image_builder: Arc<dyn ImageBuilder>,
    docker: Option<bollard::Docker>,
    config: &AgentConfig,
    network: SharedNetwork,
    platform: SharedPlatform,
) -> Router {
    // ADR-048: the worker hosts sandboxes with the same Docker provider the
    // control plane uses for local ones.
    let sandbox_host = docker.clone().map(|docker| {
        let docker = Arc::new(docker);
        let provider = Arc::new(temps_agents::sandbox::docker::DockerSandboxProvider::new(
            docker.clone(),
            temps_agents::sandbox::docker::DockerSandboxConfig {
                control_plane_url: config.control_plane_url.clone(),
                // Bound exec output while Docker streams it, not after: the
                // exec response is cut to this size anyway.
                exec_output_limit: Some(temps_agents::sandbox::remote::WORKER_EXEC_OUTPUT_LIMIT),
                ..Default::default()
            },
        ));
        spawn_sandbox_quarantine(provider.clone(), config.node_name.clone());
        SandboxHost {
            provider,
            containers: docker,
        }
    });
    let sandbox_state = Arc::new(SandboxHostState::new(
        sandbox_host,
        config.sandbox_work_root(),
    ));
    let runtime_docker = docker.clone();
    let state = Arc::new(AgentState {
        container_deployer,
        image_builder,
        docker,
        overlay_bridge_address: network.overlay_bridge_address,
        overlay_peers: network.overlay_peers,
        platform,
        host_bind_address: network.host_bind_address,
    });
    let resource_limits = Arc::new(handlers::AgentResourceLimits::new());

    let auth = Arc::new(AgentAuth::new(&config.token));

    let runtime_health_routes = crate::runtime_health::router(runtime_docker, config, auth.clone());
    let worker_metrics_routes = crate::worker_metrics::router(config, auth.clone());

    // API routes — all protected by bearer token auth
    let api_routes = Router::new()
        // Container management routes
        .route("/agent/containers/deploy", post(handlers::deploy_container))
        .route(
            "/agent/containers/{id}/stop",
            post(handlers::stop_container),
        )
        .route(
            "/agent/containers/{id}/start",
            post(handlers::start_container),
        )
        .route(
            "/agent/containers/{id}/exec",
            post(handlers::exec_container),
        )
        .route(
            "/agent/containers/{id}/health-exec",
            post(handlers::exec_health_probe),
        )
        .route(
            "/agent/containers/{id}/terminal",
            get(handlers::terminal_container),
        )
        .route("/agent/containers/{id}", delete(handlers::remove_container))
        .route(
            "/agent/containers/{id}/logs",
            get(handlers::get_container_logs),
        )
        .route(
            "/agent/containers/{id}/logs/stream",
            get(handlers::stream_container_logs),
        )
        .route(
            "/agent/containers/{id}/stats",
            get(handlers::get_container_stats),
        )
        .route(
            "/agent/containers/{id}/info",
            get(handlers::get_container_info),
        )
        .route("/agent/containers", get(handlers::list_containers))
        .route("/agent/images/import", post(handlers::import_image))
        .route("/agent/images/pull", post(handlers::pull_image))
        .route(
            "/agent/images/inspect",
            get(crate::build_handler::inspect_image),
        )
        .route(
            "/agent/images/build",
            post(crate::build_handler::build_image)
                .layer(axum::extract::DefaultBodyLimit::disable()),
        )
        .route(
            "/agent/plugins/build",
            post(crate::plugin_build_handler::build_plugin)
                .layer(axum::extract::DefaultBodyLimit::disable()),
        )
        .route(
            "/agent/images/export",
            get(crate::build_handler::export_image),
        )
        .route(
            "/agent/images/extract",
            get(crate::build_handler::extract_image_path),
        )
        .route(
            "/agent/images/fetch",
            post(crate::build_handler::fetch_registry_image),
        )
        .route("/agent/images/scan", post(crate::build_handler::scan_image))
        .route("/agent/images/{name}/exists", get(handlers::image_exists))
        .route("/agent/health", get(handlers::health_check))
        // Service management routes
        .route("/agent/services", post(service_handlers::create_service))
        .route("/agent/services", get(service_handlers::list_services))
        .route(
            "/agent/services/{name}/stop",
            post(service_handlers::stop_service),
        )
        .route(
            "/agent/services/{name}/start",
            post(service_handlers::start_service),
        )
        .route(
            "/agent/services/{name}",
            delete(service_handlers::remove_service),
        )
        .route(
            "/agent/services/{name}/status",
            get(service_handlers::service_status),
        )
        .route("/agent/services/exec", post(service_handlers::service_exec))
        // ADR-055 Phase 4: databases created from their workload spec.
        .route(
            "/agent/services/workload",
            post(service_handlers::create_service_workload),
        )
        .route(
            "/agent/services/{name}/snapshot",
            get(service_handlers::service_snapshot),
        )
        .route(
            "/agent/services/runtime-env",
            post(service_handlers::runtime_env),
        )
        .route(
            "/agent/services/revoke-s3-access",
            post(service_handlers::revoke_s3_access),
        )
        .route(
            "/agent/services/health-probe",
            post(service_handlers::health_probe),
        )
        .route(
            "/agent/services/backup",
            post(service_handlers::backup_service),
        )
        .route(
            "/agent/services/restore",
            post(service_handlers::restore_service),
        )
        .layer(middleware::from_fn(require_agent_auth))
        .layer(Extension(auth.clone()))
        .layer(Extension(resource_limits))
        .with_state(state);

    let sandbox_routes = sandbox_router(sandbox_state, auth);

    // Swagger UI — no auth required so it's accessible for documentation
    let mut api = AgentApiDoc::openapi();
    api.merge(crate::runtime_health::RuntimeHealthApiDoc::openapi());
    api.merge(crate::worker_metrics::WorkerMetricsApiDoc::openapi());
    let swagger_ui = SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", api);

    api_routes
        .merge(sandbox_routes)
        .merge(runtime_health_routes)
        .merge(worker_metrics_routes)
        .merge(swagger_ui)
}

/// ADR-048 sandbox host API — same auth as every other agent route. File
/// and directory uploads travel base64-encoded in JSON, so only those two
/// routes raise the body limit (to [`SANDBOX_UPLOAD_BODY_LIMIT`]) and only
/// they take an upload slot ([`sandbox_handlers::limit_uploads`]); every
/// other route keeps axum's 2 MiB default.
pub(crate) fn sandbox_router(sandbox_state: Arc<SandboxHostState>, auth: Arc<AgentAuth>) -> Router {
    let upload_slot =
        || middleware::from_fn_with_state(sandbox_state.clone(), sandbox_handlers::limit_uploads);
    Router::new()
        .route("/agent/sandboxes", post(sandbox_handlers::create_sandbox))
        .route(
            "/agent/sandboxes/exec",
            post(sandbox_handlers::exec_sandbox),
        )
        .route(
            "/agent/sandboxes/exec-stream",
            post(sandbox_handlers::exec_sandbox_stream),
        )
        .route(
            "/agent/sandboxes/alive",
            post(sandbox_handlers::sandbox_alive),
        )
        .route(
            "/agent/sandboxes/read-file",
            post(sandbox_handlers::read_sandbox_file),
        )
        .route(
            "/agent/sandboxes/write-file",
            post(sandbox_handlers::write_sandbox_file)
                .layer(axum::extract::DefaultBodyLimit::max(
                    SANDBOX_UPLOAD_BODY_LIMIT,
                ))
                .layer(upload_slot()),
        )
        .route(
            "/agent/sandboxes/write-directory",
            post(sandbox_handlers::write_sandbox_directory)
                .layer(axum::extract::DefaultBodyLimit::max(
                    SANDBOX_UPLOAD_BODY_LIMIT,
                ))
                .layer(upload_slot()),
        )
        .route(
            "/agent/sandboxes/kill-processes",
            post(sandbox_handlers::kill_sandbox_processes),
        )
        .route(
            "/agent/sandboxes/destroy",
            post(sandbox_handlers::destroy_sandbox),
        )
        .route(
            "/agent/sandboxes/stop",
            post(sandbox_handlers::stop_sandbox),
        )
        .route(
            "/agent/sandboxes/start",
            post(sandbox_handlers::start_sandbox),
        )
        .route(
            "/agent/sandboxes/recover",
            post(sandbox_handlers::recover_sandbox),
        )
        .route(
            "/agent/sandboxes/release-orphaned",
            post(sandbox_handlers::release_orphaned_sandbox_resources),
        )
        .route(
            "/agent/sandboxes/status",
            post(sandbox_handlers::sandbox_status),
        )
        // ADR-055 phase 6: driver operations the control plane forwards.
        .route(
            "/agent/sandboxes/resize-disk",
            post(sandbox_handlers::resize_sandbox_disk),
        )
        .route(
            "/agent/sandboxes/egress-policy",
            post(sandbox_handlers::set_sandbox_egress_policy),
        )
        .route(
            "/agent/sandboxes/image-identity",
            post(sandbox_handlers::sandbox_image_identity),
        )
        .route(
            "/agent/sandboxes/agent-runtime-check",
            post(sandbox_handlers::check_sandbox_agent_runtime),
        )
        .route(
            "/agent/sandboxes/recover-agent-harness",
            post(sandbox_handlers::recover_sandbox_agent_harness),
        )
        .route(
            "/agent/sandboxes/byte-channel",
            get(sandbox_handlers::open_sandbox_byte_channel),
        )
        .layer(middleware::from_fn_with_state(
            sandbox_state.clone(),
            sandbox_handlers::redact_host_paths,
        ))
        .layer(middleware::from_fn(require_agent_auth))
        .layer(Extension(auth))
        .with_state(sandbox_state)
}

/// Stop sandboxes on this node whose network isolation predates the current
/// policy, as the control plane does for its own sandboxes at startup. They
/// are recreated (keeping their volumes) the next time they are used.
/// Off the critical path: the agent serves while this runs, and a failure
/// is logged, not fatal — the node still refuses to recover a stale sandbox
/// when it is next accessed.
fn spawn_sandbox_quarantine(
    provider: Arc<temps_agents::sandbox::docker::DockerSandboxProvider>,
    node_name: String,
) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        tracing::warn!(
            node = %node_name,
            "No async runtime while building the agent router; skipping stale sandbox quarantine"
        );
        return;
    };
    runtime.spawn(async move {
        match provider.quarantine_stale_sandboxes().await {
            Ok(()) => tracing::info!(
                node = %node_name,
                "Validated the isolation policy of existing sandboxes on this node"
            ),
            Err(error) => tracing::warn!(
                node = %node_name,
                error = %error,
                "Could not validate the isolation policy of existing sandboxes on this node; \
                 stale sandboxes are still refused when they are next recovered"
            ),
        }
    });
}

/// Maximum number of consecutive heartbeat failures before escalating to error-level logging.
const HEARTBEAT_MAX_RETRIES: u32 = 3;
/// The shortest gap between a heartbeat and an early one sent because an
/// instance report changed (ADR-055 §7).
const EARLY_BEAT_MIN_GAP: Duration = Duration::from_secs(2);

/// Base delay for exponential backoff between heartbeat retries (doubled each attempt).
const HEARTBEAT_RETRY_BASE_DELAY: Duration = Duration::from_secs(2);

/// Maximum backoff delay between heartbeat retries.
const HEARTBEAT_RETRY_MAX_DELAY: Duration = Duration::from_secs(15);

/// The part of the control plane's heartbeat response the agent acts on.
#[derive(serde::Deserialize)]
struct HeartbeatAck {
    /// Absent from servers that predate it: read as "not recorded", so a
    /// report is resent rather than silently dropped.
    #[serde(default)]
    instances_accepted: bool,
}

/// Whether a successful heartbeat's response says the instance reports it
/// carried were recorded. Anything unreadable counts as not recorded.
fn instance_reports_acknowledged(body: &[u8]) -> bool {
    serde_json::from_slice::<HeartbeatAck>(body)
        .map(|ack| ack.instances_accepted)
        .unwrap_or(false)
}

/// Heartbeats between re-checks of an already-known container platform.
///
/// At the 30s heartbeat interval this is ~10 minutes. The daemon behind
/// `DOCKER_HOST` can be repointed or replaced while the agent runs, and a node
/// advertising a stale architecture gets images it cannot execute.
const PLATFORM_RECHECK_BEATS: u32 = 20;

/// Spawn a background task that sends heartbeats to the control plane every 30 seconds.
///
/// On transient failures, retries up to `HEARTBEAT_MAX_RETRIES` times with exponential
/// backoff before giving up for this interval. This prevents a brief network blip from
/// causing the control plane to mark the node as offline (90s stale threshold).
///
/// The first successful heartbeat includes a full container inventory so the control
/// plane can reconcile stale DB records against actual Docker state (e.g., after a crash).
fn spawn_heartbeat_loop(
    config: &AgentConfig,
    container_deployer: Arc<dyn temps_deployer::ContainerDeployer>,
    platform: SharedPlatform,
    docker: Option<bollard::Docker>,
    health: crate::network_sync::HealthSlots,
    docker_socket_grant: DockerSocketGrant,
    wakers: HeartbeatWakers,
) {
    let HeartbeatWakers {
        mesh_listener,
        node_runtime,
    } = wakers;
    let control_plane_url = config.control_plane_url.clone();
    let heartbeat_client = crate::control_plane_client_builder(config);
    let node_id = config.node_id;
    let token = config.token.clone();
    let labels = config.labels.clone();
    // ADR 045: advertise this host's own grant so the control plane can gate
    // placement up front instead of discovering mid-deploy that the node would
    // have deployed the project without the socket. Snapshotted once at
    // startup, like the grant itself — the list cannot change without a
    // restart, so recomputing it per beat would only invite drift.
    let docker_socket_projects = docker_socket_grant.to_vec();

    tokio::spawn(async move {
        // Strict TLS — the worker→control-plane heartbeat carries the
        // node's auth token. A MitM with a self-signed cert here would
        // capture the token and impersonate this worker. There is no
        // opt-in: `AppSettings.insecure_tls` is server-side only.
        let client = match heartbeat_client.timeout(Duration::from_secs(10)).build() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("Failed to build heartbeat HTTP client: {}", e);
                return;
            }
        };

        let heartbeat_url = format!(
            "{}/api/internal/nodes/{}/heartbeat",
            control_plane_url, node_id
        );

        let mut interval = tokio::time::interval(Duration::from_secs(30));
        let mut consecutive_failures: u32 = 0;
        let mut inventory_sent = false;
        let mut beats_since_platform_check: u32 = 0;
        // Occupied ranges the control plane acknowledged last (ADR 056):
        // they are sent only when they differ.
        let mut occupied_sent: Option<Vec<temps_network::occupied::OccupiedRange>> = None;
        let mut capacity_sampler = CapacitySampler::new();

        let mut last_beat: Option<tokio::time::Instant> = None;

        loop {
            tokio::select! {
                _ = interval.tick() => {}
                // Report a new mesh listener at once: a control plane that
                // reaches this node only through the mesh dials the
                // registered address until it hears of it.
                _ = mesh_listener.changed() => {
                    tracing::info!(
                        node_id,
                        mesh_listener = ?mesh_listener.get(),
                        "the agent API's mesh listener changed; sending a heartbeat now"
                    );
                }
                // ADR-055 §7: a changed instance report is sent early rather
                // than up to 30 s late, but never more often than once per
                // `EARLY_BEAT_MIN_GAP`.
                _ = instance_reports_changed(node_runtime.as_deref()) => {
                    if let Some(last) = last_beat {
                        let gap = last.elapsed();
                        if gap < EARLY_BEAT_MIN_GAP {
                            tokio::time::sleep(EARLY_BEAT_MIN_GAP - gap).await;
                        }
                    }
                }
            }
            last_beat = Some(tokio::time::Instant::now());

            let mut capacity = capacity_sampler.sample().await;
            // Where the agent API is served on the WireGuard mesh: what lets
            // the control plane call this node through the mesh (an agent
            // that never reports it keeps being called at its registered
            // address).
            if let (Some(value), Some(fields)) = (
                mesh_listener.get().heartbeat_value(),
                capacity.as_object_mut(),
            ) {
                fields.insert(
                    temps_core::node_address::AGENT_MESH_LISTENER_KEY.to_string(),
                    value,
                );
            }
            // The node runtime's reports: only those that changed since the
            // last accepted beat.
            let instance_reports = match &node_runtime {
                Some(runtime) => {
                    capacity["node_capabilities"] =
                        serde_json::json!([crate::node_runtime::STATE_STREAM_CAPABILITY]);
                    runtime.pending_reports().await
                }
                None => Vec::new(),
            };

            // Platform discovery can fail at startup — the agent often boots
            // alongside the Docker daemon — so retry here until it answers.
            // Reporting the agent binary's architecture instead would be a
            // confident wrong answer whenever the daemon differs, and the
            // control plane would schedule on it.
            //
            // A known platform is also re-checked periodically: `DOCKER_HOST`
            // can be repointed, or the daemon replaced, under a running agent.
            // Without this the node would keep advertising its old
            // architecture indefinitely and receive images it can no longer
            // run. One `docker info` every ~10 minutes is negligible next to
            // that failure mode.
            let known_platform = read_platform(&platform);
            let due_for_recheck = beats_since_platform_check >= PLATFORM_RECHECK_BEATS;
            let reported_platform = if known_platform.is_none() || due_for_recheck {
                beats_since_platform_check = 0;
                match detect_agent_platform(docker.as_ref()).await {
                    Some(discovered) => {
                        match known_platform.as_deref() {
                            None => tracing::info!(
                                node_id,
                                platform = %discovered,
                                "Container platform resolved on retry"
                            ),
                            Some(previous) if previous != discovered => tracing::warn!(
                                node_id,
                                previous,
                                platform = %discovered,
                                "Docker daemon architecture changed under a running agent; \
                                 reporting the new platform"
                            ),
                            Some(_) => {}
                        }
                        store_platform(&platform, discovered.clone());
                        Some(discovered)
                    }
                    // The daemon stopped answering. Keep reporting the last
                    // confirmed value rather than dropping to unknown: it was
                    // true as of the last successful check, and a node whose
                    // daemon is down fails its health checks anyway.
                    None => known_platform,
                }
            } else {
                beats_since_platform_check += 1;
                known_platform
            };

            let mut body = serde_json::json!({
                "capacity": capacity,
                "labels": labels,
                // Sent on EVERY beat, including as an empty array: "this node
                // grants nothing" is a fact the scheduler must be able to act
                // on, and an operator who *removes* a grant and restarts the
                // agent needs the control plane to stop placing that project
                // here on the next beat rather than at the next re-join.
                "docker_socket_projects": docker_socket_projects,
            });
            // `architecture` goes out on EVERY beat once known, not just at
            // registration: it's how a node upgraded from a pre-multi-arch
            // agent (which left the column NULL) becomes schedulable with
            // confidence, and how a re-pointed DOCKER_HOST is picked up
            // without re-joining. While unknown the field is omitted, and the
            // control plane leaves the stored value untouched.
            if let Some(platform) = reported_platform {
                body["architecture"] = serde_json::json!(platform);
            }
            if let Some(ingress) = crate::public_ingress::health() {
                body["public_ingress"] = serde_json::to_value(ingress).unwrap_or_default();
            }
            if !instance_reports.is_empty() {
                match serde_json::to_value(&instance_reports) {
                    Ok(v) => body["instances"] = v,
                    Err(e) => tracing::warn!(
                        node_id = node_id,
                        error = %e,
                        "Failed to serialize instance reports for heartbeat"
                    ),
                }
            }

            // DNS resolver health (ADR-024), published by the network-sync
            // loop on every tick. `None` until that loop has ticked at
            // least once for this node (startup, or a single-host node
            // that never gets a compute_cidr allocation and so never
            // touches cluster DNS at all) — omitted from the body in that
            // case, same treatment as `architecture` above.
            let dns_snapshot = health.dns.read().ok().and_then(|guard| guard.clone());
            if let Some(health) = dns_snapshot {
                match serde_json::to_value(&health) {
                    Ok(v) => body["dns_resolver"] = v,
                    Err(e) => tracing::warn!(
                        node_id = node_id,
                        error = %e,
                        "Failed to serialize DNS resolver health for heartbeat"
                    ),
                }
            }

            // Cross-node overlay state and this host's occupied ranges
            // (ADR 056), published by the network-sync loop. Both omitted
            // until it ran; the ranges also when unchanged since last sent.
            let overlay_snapshot = health
                .overlay
                .read()
                .map(|guard| guard.clone())
                .unwrap_or_default();
            if let Some(overlay) = &overlay_snapshot.overlay {
                body["overlay"] = serde_json::json!(overlay);
            }
            let occupied_to_send = overlay_snapshot
                .occupied_ranges
                .filter(|ranges| occupied_sent.as_ref() != Some(ranges));
            if let Some(ranges) = &occupied_to_send {
                body["occupied_ranges"] = serde_json::json!(ranges);
            }

            // On the first heartbeat (agent startup/reconnect), include a full
            // container inventory so the control plane can reconcile stale state.
            if !inventory_sent {
                match container_deployer.list_containers().await {
                    Ok(containers) => {
                        // Only include temps-managed containers
                        let managed: Vec<_> = containers
                            .into_iter()
                            .filter(|c| {
                                c.labels
                                    .get("sh.temps.managed")
                                    .map(|v| v == "true")
                                    .unwrap_or(false)
                            })
                            .map(|c| {
                                serde_json::json!({
                                    "container_id": c.container_id,
                                    "container_name": c.container_name,
                                })
                            })
                            .collect();
                        body["containers"] = serde_json::json!(managed);
                        tracing::info!(
                            node_id = node_id,
                            count = managed.len(),
                            "Including container inventory in heartbeat for reconciliation"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            node_id = node_id,
                            "Failed to list containers for inventory: {}",
                            e
                        );
                    }
                }
            }

            let mut attempt = 0;
            let mut succeeded = false;

            loop {
                match client
                    .post(&heartbeat_url)
                    .bearer_auth(&token)
                    .json(&body)
                    .send()
                    .await
                {
                    Ok(response) if response.status().is_success() => {
                        if consecutive_failures > 0 {
                            tracing::info!(
                                node_id = node_id,
                                previous_failures = consecutive_failures,
                                "Heartbeat recovered after {} consecutive failure(s)",
                                consecutive_failures
                            );
                        }
                        consecutive_failures = 0;
                        succeeded = true;
                        if body.get("containers").is_some() {
                            inventory_sent = true;
                        }
                        if let (Some(runtime), Some(_)) = (&node_runtime, body.get("instances")) {
                            // The runtime suppresses a report it believes
                            // delivered until it changes, so only a server
                            // that says it recorded them makes them sent;
                            // otherwise they ride the next beat again.
                            let accepted = match response.bytes().await {
                                Ok(bytes) => instance_reports_acknowledged(&bytes),
                                Err(e) => {
                                    tracing::warn!(
                                        node_id = node_id,
                                        error = %e,
                                        "Failed to read the heartbeat response; instance reports will be resent"
                                    );
                                    false
                                }
                            };
                            if accepted {
                                runtime.mark_sent(&instance_reports);
                            } else {
                                tracing::debug!(
                                    node_id = node_id,
                                    count = instance_reports.len(),
                                    "Control plane did not acknowledge the instance reports; resending them next heartbeat"
                                );
                            }
                        }
                        if let Some(ranges) = occupied_to_send.clone() {
                            occupied_sent = Some(ranges);
                        }
                        tracing::debug!(node_id = node_id, "Heartbeat sent to control plane");
                        break;
                    }
                    Ok(response)
                        if response.status().is_server_error()
                            && attempt < HEARTBEAT_MAX_RETRIES =>
                    {
                        // Server errors are retryable
                        attempt += 1;
                        let delay = std::cmp::min(
                            HEARTBEAT_RETRY_BASE_DELAY * 2u32.saturating_pow(attempt - 1),
                            HEARTBEAT_RETRY_MAX_DELAY,
                        );
                        tracing::warn!(
                            node_id = node_id,
                            attempt = attempt,
                            status = %response.status(),
                            retry_in_ms = delay.as_millis() as u64,
                            "Heartbeat failed with server error, retrying"
                        );
                        tokio::time::sleep(delay).await;
                    }
                    Ok(response) => {
                        // Client error (4xx) or exhausted retries — don't retry
                        tracing::warn!(
                            node_id = node_id,
                            status = %response.status(),
                            "Heartbeat failed with status {}",
                            response.status()
                        );
                        break;
                    }
                    Err(e) if attempt < HEARTBEAT_MAX_RETRIES => {
                        // Network errors are retryable
                        attempt += 1;
                        let delay = std::cmp::min(
                            HEARTBEAT_RETRY_BASE_DELAY * 2u32.saturating_pow(attempt - 1),
                            HEARTBEAT_RETRY_MAX_DELAY,
                        );
                        tracing::warn!(
                            node_id = node_id,
                            attempt = attempt,
                            retry_in_ms = delay.as_millis() as u64,
                            "Heartbeat network error, retrying: {}",
                            e
                        );
                        tokio::time::sleep(delay).await;
                    }
                    Err(e) => {
                        tracing::warn!(
                            node_id = node_id,
                            attempts = attempt + 1,
                            "Heartbeat failed after {} attempt(s): {}",
                            attempt + 1,
                            e
                        );
                        break;
                    }
                }
            }

            if !succeeded {
                consecutive_failures += 1;
                if consecutive_failures >= 3 {
                    tracing::error!(
                        node_id = node_id,
                        consecutive_failures = consecutive_failures,
                        "Heartbeat has failed {} consecutive times — node may be marked offline",
                        consecutive_failures
                    );
                }
            }
        }
    });
}

/// Resolve the container platform this agent can actually run images for.
///
/// The source of truth is the Docker **daemon** (`docker info`), not this
/// process: an agent may drive a daemon over `DOCKER_HOST`, or a
/// QEMU-emulated `docker:dind`, whose architecture differs from the binary's.
/// Placing an image is decided by the daemon, so that is what we report.
///
/// Returns `None` when the daemon can't be reached or doesn't report an
/// architecture. That is deliberate: falling back to the agent binary's
/// architecture would be a *confident wrong answer* whenever the two differ
/// (`DOCKER_HOST`, an emulated daemon), and the control plane trusts a
/// reported platform — it would schedule an incompatible image and both
/// compatibility checks would pass against the bogus value. "Unknown" is
/// handled safely upstream (assume compatible, log it as unverified), and the
/// heartbeat loop keeps retrying until the daemon answers.
pub async fn detect_agent_platform(docker: Option<&bollard::Docker>) -> Option<String> {
    let Some(docker) = docker else {
        tracing::warn!(
            "No Docker client available; this node will not report a container platform \
             and the control plane cannot verify image compatibility for it"
        );
        return None;
    };

    match docker.info().await {
        Ok(info) => {
            let os = info.os_type.unwrap_or_else(|| "linux".to_string());
            match info.architecture {
                Some(arch) => Some(temps_deployer::platform::normalize_platform(&os, &arch)),
                None => {
                    tracing::warn!("Docker daemon reported no architecture; will retry");
                    None
                }
            }
        }
        Err(e) => {
            tracing::warn!("Could not read Docker daemon info ({}); will retry", e);
            None
        }
    }
}

/// Shortest gap between two CPU refreshes that sysinfo reports meaningfully.
/// Refreshing sooner returns the previous reading (or ~0).
const CPU_MIN_REFRESH_INTERVAL: Duration = Duration::from_millis(250);

/// Samples this host's CPU/memory/disk for the heartbeat `capacity` payload.
///
/// What can send a heartbeat ahead of the 30-second interval.
struct HeartbeatWakers {
    /// The agent API's mesh listener; a change is reported at once.
    mesh_listener: crate::mesh_listener::SharedMeshListener,
    /// The node runtime (ADR-055 §7), when the state stream is enabled; a
    /// changed instance report is sent early.
    node_runtime: Option<Arc<temps_workload_node::NodeRuntime>>,
}

/// Resolves when the node runtime has a changed instance report to send;
/// never, when this node has no runtime.
async fn instance_reports_changed(runtime: Option<&temps_workload_node::NodeRuntime>) {
    match runtime {
        Some(runtime) => runtime.reports_changed.notified().await,
        None => std::future::pending().await,
    }
}

/// CPU usage is a delta between two refreshes, so the `System` must outlive a
/// single reading: the previous implementation built a fresh `System` per beat
/// and refreshed once, which always reported ~0% and made any CPU alert
/// useless. Holding it across beats makes `cpu_percent` the average load over
/// the heartbeat interval, which is also the right signal to alert on (a
/// 30-second mean rather than an instantaneous spike).
struct CapacitySampler {
    sys: sysinfo::System,
    last_cpu_refresh: std::time::Instant,
}

impl CapacitySampler {
    fn new() -> Self {
        let mut sys = sysinfo::System::new();
        sys.refresh_cpu_all();
        Self {
            sys,
            last_cpu_refresh: std::time::Instant::now(),
        }
    }

    /// Collect one reading. Waits out the remainder of
    /// [`CPU_MIN_REFRESH_INTERVAL`] first, which only ever matters for the
    /// very first beat (the heartbeat interval is far longer).
    async fn sample(&mut self) -> serde_json::Value {
        let since_last = self.last_cpu_refresh.elapsed();
        if since_last < CPU_MIN_REFRESH_INTERVAL {
            tokio::time::sleep(CPU_MIN_REFRESH_INTERVAL - since_last).await;
        }
        self.sys.refresh_cpu_usage();
        self.last_cpu_refresh = std::time::Instant::now();
        self.sys.refresh_memory();

        let disks = sysinfo::Disks::new_with_refreshed_list();
        // Use only the root mount point to avoid double-counting overlapping mounts
        let (disk_used, disk_total) = disks
            .list()
            .iter()
            .find(|d| d.mount_point() == std::path::Path::new("/"))
            .map(|d| (d.total_space() - d.available_space(), d.total_space()))
            .unwrap_or((0, 0));

        serde_json::json!({
            "cpu_percent": self.sys.global_cpu_usage(),
            "memory_used_bytes": self.sys.used_memory(),
            "memory_total_bytes": self.sys.total_memory(),
            "disk_used_bytes": disk_used,
            "disk_total_bytes": disk_total,
            // ADR-055 §5: the scheduler deducts `system_reserved`
            // (millicores) from the node's CPU capacity, and an enforcing
            // node counts reservations against it. Only the core count makes
            // that possible.
            "cpu_cores": self.sys.cpus().len(),
        })
    }
}

fn validate_agent_transport(config: &AgentConfig) -> Result<(), crate::AgentError> {
    if !config.require_mtls {
        return Ok(());
    }
    if !config.control_plane_url.starts_with("https://") {
        return Err(crate::AgentError::TlsConfig {
            context: "validate control-plane transport".to_string(),
            reason: format!(
                "mTLS is required but control_plane_url '{}' is not HTTPS",
                config.control_plane_url
            ),
        });
    }
    if config.tls_cert_path.is_none()
        || config.tls_key_path.is_none()
        || config.cluster_ca_path.is_none()
    {
        return Err(crate::AgentError::TlsConfig {
            context: "validate required agent identity".to_string(),
            reason: "mTLS is required but tls_cert_path, tls_key_path, and cluster_ca_path are not all configured".to_string(),
        });
    }
    Ok(())
}

/// Options of [`start_agent_server_with`] that older callers do not set.
#[derive(Debug, Clone)]
pub struct AgentServerOptions {
    /// Run the node runtime and advertise the `state_stream` capability
    /// (ADR-055 §7). Off, the agent is driven through `/agent/*` only, as
    /// before; `temps agent --disable-state-stream` is the rollback.
    pub state_stream: bool,
}

impl Default for AgentServerOptions {
    fn default() -> Self {
        Self { state_stream: true }
    }
}

/// Start the agent server. This blocks until the server shuts down.
pub async fn start_agent_server(
    container_deployer: Arc<dyn ContainerDeployer>,
    image_builder: Arc<dyn ImageBuilder>,
    docker: Option<bollard::Docker>,
    config: AgentConfig,
    network: SharedNetwork,
    docker_socket_grant: DockerSocketGrant,
) -> Result<(), crate::AgentError> {
    start_agent_server_with(
        container_deployer,
        image_builder,
        docker,
        config,
        network,
        docker_socket_grant,
        AgentServerOptions::default(),
    )
    .await
}

/// [`start_agent_server`] with [`AgentServerOptions`].
pub async fn start_agent_server_with(
    container_deployer: Arc<dyn ContainerDeployer>,
    image_builder: Arc<dyn ImageBuilder>,
    docker: Option<bollard::Docker>,
    config: AgentConfig,
    network: SharedNetwork,
    docker_socket_grant: DockerSocketGrant,
    options: AgentServerOptions,
) -> Result<(), crate::AgentError> {
    validate_agent_transport(&config)?;

    // Resolve the daemon's platform up front when possible. A failure here is
    // not fatal and not cached as a wrong answer: the heartbeat loop retries
    // until the daemon responds.
    let platform: SharedPlatform = Arc::new(std::sync::Mutex::new(
        detect_agent_platform(docker.as_ref()).await,
    ));
    match read_platform(&platform) {
        Some(known) => tracing::info!(
            node = %config.node_name,
            platform = %known,
            "Agent container platform detected"
        ),
        None => tracing::warn!(
            node = %config.node_name,
            "Container platform not detected yet; will retry on each heartbeat. \
             Until then the control plane cannot verify image compatibility for this node."
        ),
    }

    // `network.host_bind_address` is the same slot `container_deployer`
    // publishes app-container ports on (never "0.0.0.0" — see
    // AgentConfig::private_address); the network-sync loop moves it to the
    // mesh address for a node that joined with a public one, and both app
    // deploys and agent-API services follow it.
    let router = build_router(
        container_deployer.clone(),
        image_builder,
        docker.clone(),
        &config,
        network.clone(),
        platform.clone(),
    );

    // Shared DNS resolver health (ADR-024) and overlay report (ADR 056)
    // slots. Written by the network-sync loop below on every tick, read by
    // the heartbeat loop so the control plane learns resolver and overlay
    // health without an operator SSHing in to read logs. Live for the
    // agent's process lifetime, same as `platform`.
    let health = crate::network_sync::HealthSlots::default();

    // This node's mesh address (published by the network-sync loop) and the
    // address the agent API is served on over the mesh (reported by the
    // heartbeat). Only an mTLS agent ever serves there; see `mesh_listener`.
    let (mesh_address_tx, mesh_address_rx) =
        tokio::sync::watch::channel(crate::mesh_listener::MeshAddress::Unknown);
    let mesh_listener: crate::mesh_listener::SharedMeshListener = Arc::default();

    // The node runtime (ADR-055 §7). Without it, or when it cannot start,
    // the capability is not advertised and the control plane keeps using
    // `/agent/*` for this node.
    let node_runtime = match (&docker, options.state_stream) {
        (Some(docker), true) => match crate::node_runtime::spawn(
            &config,
            docker.clone(),
            network.host_bind_address.clone(),
            network.overlay_bridge_address.clone(),
            network.overlay_peers.clone(),
        ) {
            Ok(runtime) => Some(runtime),
            Err(e) => {
                tracing::warn!(
                    node_id = config.node_id,
                    "The node runtime did not start; this node is driven through /agent/* \
                     only: {e}"
                );
                None
            }
        },
        (None, true) => {
            tracing::info!(
                node_id = config.node_id,
                "No Docker daemon; the node runtime is not started"
            );
            None
        }
        (_, false) => {
            tracing::info!(
                node_id = config.node_id,
                "The state stream is disabled; this node is driven through /agent/* only"
            );
            None
        }
    };

    // Start heartbeat background loop (with deployer for container inventory on first beat)
    spawn_heartbeat_loop(
        &config,
        container_deployer.clone(),
        platform,
        docker,
        health.clone(),
        docker_socket_grant,
        HeartbeatWakers {
            mesh_listener: mesh_listener.clone(),
            node_runtime,
        },
    );

    // Start the multi-host network sync loop. Failures here NEVER stop the
    // agent — when this node has no compute_cidr allocated (single-host
    // cluster, or simply not yet allocated), the loop is a no-op. When a
    // compute_cidr is allocated, the loop bootstraps the overlay and keeps
    // peers reconciled. `temps join` semantics are unchanged either way.
    crate::network_sync::spawn(
        &config,
        network.overlay_bridge_address,
        network.overlay_peers,
        health,
        network.host_bind_address,
        container_deployer,
        mesh_address_tx,
    );

    let listener = tokio::net::TcpListener::bind(&config.listen_address)
        .await
        .map_err(|e| {
            crate::AgentError::ServerError(format!(
                "Failed to bind to {}: {}",
                config.listen_address, e
            ))
        })?;

    tracing::info!(
        address = %config.listen_address,
        node = %config.node_name,
        node_id = config.node_id,
        swagger_ui = format!(
            "{}://{}/swagger-ui/",
            if config.require_mtls { "https" } else { "http" },
            config.listen_address
        ),
        "Temps agent server started"
    );

    // Serve mutual TLS when the node has been provisioned with certs
    // (ADR-020 WS-2.1). A newly enrolled node is fail-closed: it must never
    // silently downgrade to plaintext because a path was lost or a config was
    // only partially written. Explicit legacy configs may still use HTTP until
    // they are re-enrolled.
    match (
        config.tls_cert_path.as_ref(),
        config.tls_key_path.as_ref(),
        config.cluster_ca_path.as_ref(),
    ) {
        (Some(cert), Some(key), Some(ca)) => {
            tracing::info!(
                cert = %cert.display(),
                "Agent serving with mutual TLS (control-plane client cert verified against cluster CA)"
            );
            let server_config = std::sync::Arc::new(build_tls_server_config(cert, key, ca)?);
            // The same router, TLS configuration and auth on the mesh
            // address, for a control plane that reaches this node only
            // through the mesh. Never for the plaintext branch below.
            crate::mesh_listener::spawn(
                router.clone(),
                server_config.clone(),
                config.listen_address.clone(),
                mesh_address_rx,
                mesh_listener,
            );
            serve_mtls(listener, router, server_config).await?;
        }
        (None, None, None) if !config.require_mtls => {
            tracing::warn!(
                "Agent serving legacy plaintext HTTP; re-enroll this node to enable mTLS"
            );
            axum::serve(listener, router).await.map_err(|e| {
                crate::AgentError::ServerError(format!("Agent server error: {}", e))
            })?;
        }
        _ => {
            return Err(crate::AgentError::TlsConfig {
                context: "validate required agent identity".to_string(),
                reason: "mTLS is required but tls_cert_path, tls_key_path, and cluster_ca_path are not all configured".to_string(),
            });
        }
    }

    Ok(())
}

/// Build a rustls `ServerConfig` that serves the node's leaf cert and requires
/// the client (the control plane) to present a certificate chaining to the
/// cluster CA (ADR-020 WS-2.1).
pub(crate) fn build_tls_server_config(
    cert_path: &std::path::Path,
    key_path: &std::path::Path,
    ca_path: &std::path::Path,
) -> Result<rustls::ServerConfig, crate::AgentError> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use std::io::BufReader;

    let tls_err = |context: &str, reason: String| crate::AgentError::TlsConfig {
        context: context.to_string(),
        reason,
    };

    let cert_bytes =
        std::fs::read(cert_path).map_err(|e| tls_err("read leaf cert", e.to_string()))?;
    let cert_chain: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut BufReader::new(&cert_bytes[..]))
            .collect::<Result<_, _>>()
            .map_err(|e| tls_err("parse leaf cert", e.to_string()))?;
    if cert_chain.is_empty() {
        return Err(tls_err("parse leaf cert", "no certificates found".into()));
    }

    let key_bytes = std::fs::read(key_path).map_err(|e| tls_err("read node key", e.to_string()))?;
    let key: PrivateKeyDer<'static> =
        rustls_pemfile::private_key(&mut BufReader::new(&key_bytes[..]))
            .map_err(|e| tls_err("parse node key", e.to_string()))?
            .ok_or_else(|| tls_err("parse node key", "no private key found".into()))?;

    let ca_bytes = std::fs::read(ca_path).map_err(|e| tls_err("read cluster CA", e.to_string()))?;
    let ca_certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut BufReader::new(&ca_bytes[..]))
            .collect::<Result<_, _>>()
            .map_err(|e| tls_err("parse cluster CA", e.to_string()))?;
    let mut roots = rustls::RootCertStore::empty();
    for c in ca_certs {
        roots
            .add(c)
            .map_err(|e| tls_err("add cluster CA root", e.to_string()))?;
    }
    // A non-empty root store is required; an empty/garbage CA fails here rather
    // than silently allowing any client.
    let verifier = rustls::server::WebPkiClientVerifier::builder(std::sync::Arc::new(roots))
        .build()
        .map_err(|e| tls_err("build client-cert verifier", e.to_string()))?;

    rustls::ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(cert_chain, key)
        .map_err(|e| tls_err("build server config", e.to_string()))
}

/// Hand-rolled accept loop that TLS-terminates each connection and drives the
/// axum router over hyper with WebSocket-upgrade support (the agent exposes a
/// terminal WS route, so `serve_connection_with_upgrades` is required). Mirrors
/// the pattern in `temps-plugin-sdk/src/runtime.rs`.
pub(crate) async fn serve_mtls(
    listener: tokio::net::TcpListener,
    router: Router,
    server_config: std::sync::Arc<rustls::ServerConfig>,
) -> Result<(), crate::AgentError> {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use tower::Service;

    let acceptor = tokio_rustls::TlsAcceptor::from(server_config);

    loop {
        let (stream, _peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::error!("agent: failed to accept connection: {}", e);
                continue;
            }
        };
        let acceptor = acceptor.clone();
        let router = router.clone();
        tokio::spawn(async move {
            let tls_stream = match acceptor.accept(stream).await {
                Ok(s) => s,
                Err(e) => {
                    // Rejected client (missing/invalid cert) or handshake error.
                    tracing::warn!("agent: TLS handshake rejected: {}", e);
                    return;
                }
            };
            let socket = TokioIo::new(tls_stream);
            let hyper_service =
                hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                    let mut router = router.clone();
                    async move { router.call(req).await }
                });
            if let Err(err) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(socket, hyper_service)
                .await
            {
                let msg = err.to_string();
                if !msg.contains("shutting down") {
                    tracing::warn!("agent: connection error: {}", msg);
                }
            }
        });
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn instance_reports_count_as_sent_only_when_the_server_recorded_them() {
        assert!(instance_reports_acknowledged(
            br#"{"status":"ok","message":"Heartbeat received","instances_accepted":true}"#
        ));
        // The server could not record them: resend.
        assert!(!instance_reports_acknowledged(
            br#"{"status":"ok","message":"Heartbeat received","instances_accepted":false}"#
        ));
        // A server that predates the acknowledgement: resend.
        assert!(!instance_reports_acknowledged(
            br#"{"status":"ok","message":"Heartbeat received"}"#
        ));
        // An unreadable body: resend.
        assert!(!instance_reports_acknowledged(b"<html>bad gateway</html>"));
        assert!(!instance_reports_acknowledged(b""));
    }

    pub(crate) fn test_agent_config(require_mtls: bool) -> AgentConfig {
        AgentConfig {
            listen_address: "127.0.0.1:3100".to_string(),
            token: "secret".to_string(),
            node_name: "worker-1".to_string(),
            control_plane_url: "https://control.example.com".to_string(),
            node_id: 1,
            labels: serde_json::json!({}),
            dns_data_dir: std::path::PathBuf::from("/tmp/temps-dns"),
            tls_cert_path: None,
            tls_key_path: None,
            cluster_ca_path: None,
            require_mtls,
            underlay_dev: None,
            underlay_mtu: None,
            private_address: None,
            public_ingress_address: None,
            public_ingress_http_port: 80,
            public_ingress_https_port: 443,
            public_ingress_private_key: None,
            ingress: None,
            mesh_key_dir: std::path::PathBuf::from("/tmp/temps-wireguard"),
            wg_endpoint: None,
            control_plane_trust: Some(crate::ControlPlaneTrust::PublicRoots),
            control_plane_legacy_cert_path: None,
            join_url: None,
            overlay_network_name: None,
        }
    }

    #[test]
    fn required_mtls_rejects_missing_identity() {
        let error = validate_agent_transport(&test_agent_config(true)).unwrap_err();
        assert!(matches!(
            error,
            crate::AgentError::TlsConfig { ref context, .. }
                if context == "validate required agent identity"
        ));
    }

    #[test]
    fn required_mtls_rejects_plaintext_control_plane_url() {
        let mut config = test_agent_config(true);
        config.control_plane_url = "http://10.0.0.1:3000".to_string();
        config.tls_cert_path = Some("node.pem".into());
        config.tls_key_path = Some("node.key".into());
        config.cluster_ca_path = Some("ca.pem".into());
        let error = validate_agent_transport(&config).unwrap_err();
        assert!(matches!(
            error,
            crate::AgentError::TlsConfig { ref context, .. }
                if context == "validate control-plane transport"
        ));
    }

    #[test]
    fn explicit_legacy_transport_remains_available() {
        let mut config = test_agent_config(false);
        config.control_plane_url = "http://10.0.0.1:3000".to_string();
        assert!(validate_agent_transport(&config).is_ok());
    }

    /// The heartbeat feeds CPU alerts, so every field must be present and
    /// numeric, and CPU must be a valid percentage on every beat -- including
    /// the first, which previously read a cold sample.
    #[tokio::test]
    async fn capacity_sampler_reports_numeric_fields_with_a_valid_cpu_percent() {
        let mut sampler = CapacitySampler::new();

        for _ in 0..2 {
            let capacity = sampler.sample().await;
            for key in [
                "cpu_percent",
                "memory_used_bytes",
                "memory_total_bytes",
                "disk_used_bytes",
                "disk_total_bytes",
            ] {
                assert!(
                    capacity.get(key).is_some_and(serde_json::Value::is_number),
                    "capacity is missing numeric field '{key}': {capacity}"
                );
            }
            let cpu = capacity["cpu_percent"].as_f64().unwrap_or(-1.0);
            assert!(
                (0.0..=100.0).contains(&cpu),
                "cpu_percent out of range: {cpu}"
            );
            assert!(capacity["memory_total_bytes"].as_u64().unwrap_or(0) > 0);
        }
    }

    /// With no Docker client we cannot know the daemon's architecture, and
    /// guessing the agent binary's would be a confident wrong answer whenever
    /// the two differ (`DOCKER_HOST`, an emulated daemon). The control plane
    /// trusts a reported platform, so "unknown" must stay unknown.
    #[tokio::test]
    async fn test_detect_agent_platform_is_unknown_without_docker() {
        assert_eq!(detect_agent_platform(None).await, None);
    }

    /// The reported platform must come from the **daemon**: an agent can drive
    /// a daemon over `DOCKER_HOST` (or an emulated one) whose architecture
    /// differs from the binary's, and it is the daemon that decides whether an
    /// image can run.
    #[tokio::test]
    async fn test_detect_agent_platform_reads_the_daemon() {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            println!("Docker not available, skipping");
            return;
        };
        if docker.ping().await.is_err() {
            println!("Docker daemon not responding, skipping");
            return;
        }

        let reported = detect_agent_platform(Some(&docker))
            .await
            .expect("a reachable daemon must report a platform");

        let info = docker.info().await.expect("docker info");
        let expected = temps_deployer::platform::normalize_platform(
            &info.os_type.unwrap_or_else(|| "linux".to_string()),
            &info.architecture.expect("daemon architecture"),
        );

        assert_eq!(
            reported, expected,
            "the agent must report the daemon's platform"
        );
        // Whatever we report has to survive the canonicalization the control
        // plane applies before storing it, or the node would be recorded with
        // a spelling that never matches an image platform.
        assert_eq!(
            reported,
            temps_deployer::platform::canonicalize_platform(&reported)
        );
    }

    /// A known platform is re-checked periodically. `DOCKER_HOST` can be
    /// repointed, or the daemon replaced, under a running agent — a node that
    /// keeps advertising its old architecture would be sent images it can no
    /// longer execute.
    #[test]
    fn test_platform_recheck_cadence_is_bounded() {
        // ~10 minutes at the 30s heartbeat interval: frequent enough that a
        // swapped daemon is noticed, rare enough to be free.
        assert_eq!(PLATFORM_RECHECK_BEATS, 20);

        // Mirrors the loop's decision, which re-detects when the platform is
        // unknown OR the counter is due.
        let due = |beats: u32, known: bool| !known || beats >= PLATFORM_RECHECK_BEATS;

        assert!(
            due(0, false),
            "unknown platform must be retried immediately"
        );
        assert!(
            !due(1, true),
            "a known platform is not re-checked every beat"
        );
        assert!(!due(PLATFORM_RECHECK_BEATS - 1, true));
        assert!(due(PLATFORM_RECHECK_BEATS, true), "re-check must come due");
    }

    /// A daemon that stops answering must not drop the node back to "unknown":
    /// the last confirmed value was true as of the last check, and a node whose
    /// daemon is down fails its health checks anyway.
    #[test]
    fn test_a_failed_recheck_keeps_the_last_known_platform() {
        let slot: SharedPlatform = Arc::new(std::sync::Mutex::new(Some("linux/arm64".to_string())));
        let known = read_platform(&slot);

        // What the loop does when re-detection returns None.
        let reported = match None::<String> {
            Some(discovered) => Some(discovered),
            None => known,
        };

        assert_eq!(reported.as_deref(), Some("linux/arm64"));
        assert_eq!(read_platform(&slot).as_deref(), Some("linux/arm64"));
    }

    /// An undiscovered platform must not be reported as a fact. The heartbeat
    /// omits the field entirely so the control plane leaves whatever it has
    /// alone, instead of overwriting a known architecture with a guess.
    #[test]
    fn test_unknown_platform_is_omitted_from_the_heartbeat_body() {
        let slot: SharedPlatform = Arc::new(std::sync::Mutex::new(None));
        assert_eq!(read_platform(&slot), None);

        let mut body = serde_json::json!({ "capacity": {}, "labels": {} });
        if let Some(platform) = read_platform(&slot) {
            body["architecture"] = serde_json::json!(platform);
        }
        assert!(
            body.get("architecture").is_none(),
            "an unknown platform must not be sent at all: {body}"
        );

        // Once discovered it is reported, and stays reported.
        store_platform(&slot, "linux/arm64".to_string());
        assert_eq!(read_platform(&slot).as_deref(), Some("linux/arm64"));
        if let Some(platform) = read_platform(&slot) {
            body["architecture"] = serde_json::json!(platform);
        }
        assert_eq!(body["architecture"], serde_json::json!("linux/arm64"));
    }

    /// Every sandbox host route, as the control plane's client calls it.
    const SANDBOX_ROUTES: [&str; 19] = [
        "/agent/sandboxes",
        "/agent/sandboxes/exec",
        "/agent/sandboxes/exec-stream",
        "/agent/sandboxes/alive",
        "/agent/sandboxes/read-file",
        "/agent/sandboxes/write-file",
        "/agent/sandboxes/write-directory",
        "/agent/sandboxes/kill-processes",
        "/agent/sandboxes/destroy",
        "/agent/sandboxes/stop",
        "/agent/sandboxes/start",
        "/agent/sandboxes/recover",
        "/agent/sandboxes/release-orphaned",
        "/agent/sandboxes/status",
        "/agent/sandboxes/resize-disk",
        "/agent/sandboxes/egress-policy",
        "/agent/sandboxes/image-identity",
        "/agent/sandboxes/agent-runtime-check",
        "/agent/sandboxes/recover-agent-harness",
    ];
    const UPLOAD_ROUTES: [&str; 2] = [
        "/agent/sandboxes/write-file",
        "/agent/sandboxes/write-directory",
    ];

    fn sandbox_app() -> (Router, tempfile::TempDir) {
        let root = tempfile::tempdir().unwrap();
        let state = Arc::new(SandboxHostState::new(None, root.path().to_path_buf()));
        (
            sandbox_router(state, Arc::new(AgentAuth::new("secret"))),
            root,
        )
    }

    fn post_json(
        route: &str,
        token: Option<&str>,
        body: Vec<u8>,
    ) -> axum::http::Request<axum::body::Body> {
        let mut request = axum::http::Request::post(route)
            .header(axum::http::header::CONTENT_TYPE, "application/json");
        if let Some(token) = token {
            request = request.header(axum::http::header::AUTHORIZATION, format!("Bearer {token}"));
        }
        request.body(axum::body::Body::from(body)).unwrap()
    }

    #[tokio::test]
    async fn every_sandbox_route_is_registered_behind_agent_auth() {
        use tower::ServiceExt;
        let (app, _root) = sandbox_app();
        for route in SANDBOX_ROUTES {
            for token in [None, Some("wrong")] {
                let response = app
                    .clone()
                    .oneshot(post_json(route, token, b"{}".to_vec()))
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    axum::http::StatusCode::UNAUTHORIZED,
                    "{route} with token {token:?}"
                );
            }
            let response = app
                .clone()
                .oneshot(post_json(route, Some("secret"), b"{}".to_vec()))
                .await
                .unwrap();
            assert!(
                !matches!(
                    response.status(),
                    axum::http::StatusCode::NOT_FOUND
                        | axum::http::StatusCode::METHOD_NOT_ALLOWED
                        | axum::http::StatusCode::UNAUTHORIZED
                ),
                "{route} answered {}",
                response.status()
            );
        }
    }

    #[tokio::test]
    async fn the_byte_channel_route_is_registered_behind_agent_auth() {
        use tower::ServiceExt;
        let (app, _root) = sandbox_app();
        let get = |token: Option<&str>| {
            let mut request = axum::http::Request::get(
                "/agent/sandboxes/byte-channel?sandbox=temps-sandbox-abc&channel=terminal",
            );
            if let Some(token) = token {
                request =
                    request.header(axum::http::header::AUTHORIZATION, format!("Bearer {token}"));
            }
            request.body(axum::body::Body::empty()).unwrap()
        };
        for token in [None, Some("wrong")] {
            let response = app.clone().oneshot(get(token)).await.unwrap();
            assert_eq!(
                response.status(),
                axum::http::StatusCode::UNAUTHORIZED,
                "byte channel with token {token:?}"
            );
        }
        let response = app.clone().oneshot(get(Some("secret"))).await.unwrap();
        assert!(
            !matches!(
                response.status(),
                axum::http::StatusCode::NOT_FOUND
                    | axum::http::StatusCode::METHOD_NOT_ALLOWED
                    | axum::http::StatusCode::UNAUTHORIZED
            ),
            "byte channel answered {}",
            response.status()
        );
    }

    #[tokio::test]
    async fn only_upload_routes_accept_bodies_over_two_mib() {
        use tower::ServiceExt;
        let (app, _root) = sandbox_app();
        // Valid JSON just over axum's 2 MiB default.
        let mut big = b"{\"padding\":\"".to_vec();
        big.extend(std::iter::repeat_n(b'a', 3 * 1024 * 1024));
        big.extend_from_slice(b"\"}");
        for route in SANDBOX_ROUTES {
            // `status` reads no body at all.
            if route == "/agent/sandboxes/status" {
                continue;
            }
            let response = app
                .clone()
                .oneshot(post_json(route, Some("secret"), big.clone()))
                .await
                .unwrap();
            if UPLOAD_ROUTES.contains(&route) {
                assert_ne!(
                    response.status(),
                    axum::http::StatusCode::PAYLOAD_TOO_LARGE,
                    "{route} must accept large uploads"
                );
            } else {
                assert_eq!(
                    response.status(),
                    axum::http::StatusCode::PAYLOAD_TOO_LARGE,
                    "{route} must keep the 2 MiB default"
                );
            }
        }
    }
}
