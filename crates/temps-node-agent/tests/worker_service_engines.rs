// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Every managed engine on a worker node, end to end through the agent's own
//! handlers, with the requests a control-plane-profile server sends:
//!
//! 1. the control plane fills the parameters with the engine's strategy and
//!    builds the create request with `remote_create_params` (no Docker);
//! 2. the agent creates the container (`create_service`);
//! 3. the health probe reaches it with the stored, loopback host;
//! 4. runtime and stats are read through the agent;
//! 5. a linked app's environment is provisioned (`runtime_env`): the
//!    per-environment database, user or bucket key is made, and the
//!    container the agent created is kept, not replaced by the engine;
//! 6. the console's per-environment preview (`runtime_env_preview`) shows
//!    what provisioning gave out;
//! 7. stop, start and remove through the agent, after which a preview is
//!    refused and creates nothing.
//!
//! The agent binds service ports on 127.0.0.1 here, so the probes and the
//! provisioning connect to the stored host as they are; the rewrite to a
//! non-loopback bind address is covered by the unit tests of
//! `resolve_runtime_target` and `published_probe_address`.
//!
//! Needs Docker and skips with a printed reason without it, or when an
//! engine's image is not present locally.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration-test helpers: panicking is how a test fails"
)]
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::Json;
use bollard::Docker;
use serde_json::{json, Value};
use temps_node_agent::handlers::AgentState;
use temps_node_agent::service_handlers::{
    create_service, health_probe, remove_service, runtime_env, runtime_env_preview, start_service,
    stop_service,
};
use temps_node_agent::service_observe_handlers::{service_runtime, service_stats};
use temps_node_agent::{ServiceCreateRequest, ServicePortMapping};
use temps_providers::externalsvc::{ServiceConfig, ServiceType};
use temps_providers::services::ExternalServiceManager;
use temps_service_protocol::{RemoteHealthProbeRequest, RemoteRuntimeEnvRequest};

fn agent_state(docker: Docker) -> Arc<AgentState> {
    let remote = Arc::new(
        temps_deployer::remote::RemoteNodeDeployer::new(
            "http://127.0.0.1:9".into(),
            "unused".into(),
            "no-io".into(),
        )
        .expect("build an unused remote deployer"),
    );
    Arc::new(AgentState {
        container_deployer: remote.clone(),
        service_runtime: Arc::new(temps_providers::node_runtime::ProvidersNodeRuntime),
        plugin_builder: Arc::new(NoPluginBuilds),
        image_builder: remote,
        docker: Some(docker),
        overlay_bridge_address: Default::default(),
        overlay_peers: Default::default(),
        platform: Default::default(),
        host_bind_address: Arc::new(std::sync::RwLock::new("127.0.0.1".into())),
    })
}

/// These tests never compile a plugin.
struct NoPluginBuilds;

#[async_trait::async_trait]
impl temps_node_agent::plugin_build_handler::PluginContainerBuilder for NoPluginBuilds {
    async fn build_from_archive(
        &self,
        _archive: &std::path::Path,
        _target: &str,
        _output: &std::path::Path,
    ) -> Result<(), temps_node_agent::plugin_build_handler::PluginBuildError> {
        Err(temps_node_agent::plugin_build_handler::PluginBuildError::RuntimeMissing)
    }
}

/// Removes what a test created, on every path.
struct Cleanup {
    docker: Docker,
    containers: Vec<String>,
    volumes: Vec<String>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let docker = self.docker.clone();
        let containers = std::mem::take(&mut self.containers);
        let volumes = std::mem::take(&mut self.volumes);
        let _ = std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            rt.block_on(async move {
                for name in containers {
                    let _ = docker
                        .remove_container(
                            &name,
                            Some(bollard::query_parameters::RemoveContainerOptions {
                                force: true,
                                ..Default::default()
                            }),
                        )
                        .await;
                }
                for volume in volumes {
                    let _ = docker
                        .remove_volume(
                            &volume,
                            None::<bollard::query_parameters::RemoveVolumeOptions>,
                        )
                        .await;
                }
            });
        })
        .join();
    }
}

/// The `{success, data, error}` envelope of an agent response.
async fn envelope(response: axum::response::Response) -> (u16, Value) {
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read the agent response");
    (
        status,
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({ "raw": String::from_utf8_lossy(&bytes).to_string() })),
    )
}

/// The envelope of an agent backup or restore, asked again while the node
/// answers 429. A node runs two data operations at a time; the control
/// plane waits for a slot and asks again, and so does this, since every
/// proof in this process shares the node's slots.
async fn admitted<F, Fut>(call: F) -> (u16, Value)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = axum::response::Response>,
{
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        let (status, body) = envelope(call().await).await;
        if status == 429 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        return (status, body);
    }
}

async fn container_id(docker: &Docker, name: &str) -> Option<String> {
    docker
        .inspect_container(
            name,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .ok()
        .and_then(|info| info.id)
}

async fn is_running(docker: &Docker, name: &str) -> bool {
    docker
        .inspect_container(
            name,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .ok()
        .and_then(|info| info.state)
        .and_then(|state| state.running)
        .unwrap_or(false)
}

struct Engine {
    label: &'static str,
    service_type: ServiceType,
    /// The strategy key the control plane fills the parameters with.
    strategy: &'static str,
    parameters: Value,
    /// Variables a linked app must receive.
    expected_env: &'static [&'static str],
}

fn engines() -> Vec<Engine> {
    vec![
        Engine {
            label: "postgres",
            service_type: ServiceType::Postgres,
            strategy: "postgres",
            parameters: json!({ "database": "postgres", "username": "postgres" }),
            expected_env: &["POSTGRES_URL", "POSTGRES_DATABASE"],
        },
        Engine {
            label: "mongodb",
            service_type: ServiceType::Mongodb,
            strategy: "mongodb",
            parameters: json!({
                "docker_image": "mongo:7.0",
                "database": "admin",
                "username": "admin"
            }),
            expected_env: &["MONGODB_URL", "MONGODB_DATABASE"],
        },
        Engine {
            label: "redis",
            service_type: ServiceType::Redis,
            strategy: "redis",
            parameters: json!({}),
            expected_env: &["REDIS_URL", "REDIS_DATABASE"],
        },
        Engine {
            label: "mariadb",
            service_type: ServiceType::Mariadb,
            strategy: "mariadb",
            parameters: json!({}),
            expected_env: &["MARIADB_URL", "MARIADB_DATABASE"],
        },
        Engine {
            label: "rustfs",
            service_type: ServiceType::S3,
            strategy: "s3",
            parameters: json!({}),
            expected_env: &[
                "S3_ENDPOINT",
                "S3_BUCKET",
                "AWS_ACCESS_KEY_ID",
                "AWS_SECRET_ACCESS_KEY",
            ],
        },
    ]
}

/// The parameters the control plane stores for a new service: the request,
/// completed by the engine's strategy.
fn control_plane_parameters(engine: &Engine) -> HashMap<String, Value> {
    let strategy = temps_providers::parameter_strategies::get_strategy(engine.strategy)
        .unwrap_or_else(|| panic!("{}: no strategy", engine.label));
    let mut parameters: HashMap<String, Value> =
        serde_json::from_value(engine.parameters.clone()).expect("parameters are an object");
    strategy
        .validate_for_creation(&parameters)
        .unwrap_or_else(|e| panic!("{}: {e}", engine.label));
    strategy
        .auto_generate_missing(&mut parameters)
        .unwrap_or_else(|e| panic!("{}: {e}", engine.label));
    parameters
}

/// A local port nothing listens on, for a container to publish.
///
/// Not one the kernel picks for port 0: that comes from the ephemeral range
/// (32768+ on Linux, 49152+ on macOS), which is also where the outgoing
/// connections the other proofs in this process open get their source
/// ports, so one of them can take it before Docker binds it. These come
/// from 20000-31999, start at an offset of this process's ID (so parallel
/// test processes start apart), and are never handed out twice in a
/// process.
fn free_port() -> u16 {
    use std::sync::atomic::{AtomicU32, Ordering};
    const FIRST: u32 = 20_000;
    const SPAN: u32 = 12_000;
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let start = std::process::id().wrapping_mul(97) % SPAN;
    for _ in 0..SPAN {
        let offset = (start + NEXT.fetch_add(1, Ordering::Relaxed)) % SPAN;
        let port = u16::try_from(FIRST + offset).expect("below 32000");
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
    panic!("no free local port in {FIRST}-{}", FIRST + SPAN - 1);
}

fn as_strings(parameters: &HashMap<String, Value>) -> HashMap<String, String> {
    parameters
        .iter()
        .map(|(key, value)| {
            let value = match value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            (key.clone(), value)
        })
        .collect()
}

async fn wait_healthy(agent: &Arc<AgentState>, config: &ServiceConfig, label: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let (status, body) = envelope(
            health_probe(
                State(agent.clone()),
                Json(RemoteHealthProbeRequest {
                    service_config: config.clone(),
                }),
            )
            .await
            .into_response(),
        )
        .await;
        let healthy = status == 200 && body["data"]["result"]["status"] == "operational";
        if healthy {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "{label}: never became healthy through the agent's probe: {status} {body}"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn preview(agent: &Arc<AgentState>, config: &ServiceConfig) -> (u16, Value) {
    envelope(
        runtime_env_preview(
            State(agent.clone()),
            Json(RemoteRuntimeEnvRequest {
                service_config: config.clone(),
                project_slug: "shop".into(),
                environment_slug: "production".into(),
            }),
        )
        .await
        .into_response(),
    )
    .await
}

async fn provision(agent: &Arc<AgentState>, config: &ServiceConfig, label: &str) -> (u16, Value) {
    // A fresh container may answer its health probe before it accepts the
    // admin connection; retry for a bounded time.
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let (status, body) = envelope(
            runtime_env(
                State(agent.clone()),
                Json(RemoteRuntimeEnvRequest {
                    service_config: config.clone(),
                    project_slug: "shop".into(),
                    environment_slug: "production".into(),
                }),
            )
            .await
            .into_response(),
        )
        .await;
        if status == 200 || Instant::now() >= deadline {
            return (status, body);
        }
        println!("{label}: provisioning not ready yet ({status}): {body}");
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_engine_on_a_worker_is_created_probed_provisioned_and_controlled_by_its_agent() {
    let docker = match Docker::connect_with_defaults() {
        Ok(docker) if docker.ping().await.is_ok() => docker,
        _ => {
            println!("Docker not available (set DOCKER_HOST), skipping the worker engine proof");
            return;
        }
    };
    let agent = agent_state(docker.clone());
    let suffix = std::process::id();
    let mut failures = Vec::new();

    for engine in engines() {
        let name = format!("wse{}{}", engine.label, suffix);
        let parameters = control_plane_parameters(&engine);
        // The control plane sends a concrete host port, as the strategy
        // chose it; take a free one so parallel runs cannot collide.
        let port = free_port();
        let mut parameters = parameters;
        parameters.insert("port".into(), json!(port.to_string()));
        // What `initialize_service_remote` generates and stores before the
        // create (a Redis password).
        for (key, value) in ExternalServiceManager::worker_generated_secrets(
            &engine.service_type,
            &as_strings(&parameters),
        ) {
            parameters.insert(key.into(), json!(value));
        }
        let create_params = as_strings(&parameters);
        let params = ExternalServiceManager::remote_create_params(
            7,
            &name,
            &engine.service_type,
            &create_params,
            false,
        )
        .unwrap_or_else(|e| panic!("{}: building the create request failed: {e}", engine.label));
        if docker.inspect_image(&params.image).await.is_err() {
            println!(
                "{}: image {} is not present locally, skipping this engine",
                engine.label, params.image
            );
            continue;
        }
        let _cleanup = Cleanup {
            docker: docker.clone(),
            containers: vec![params.name.clone()],
            volumes: params.volumes.keys().cloned().collect(),
        };

        // 1-2. Create through the agent.
        let (status, created) = envelope(
            create_service(
                State(agent.clone()),
                Json(ServiceCreateRequest {
                    name: params.name.clone(),
                    service_type: params.service_type.clone(),
                    image: params.image.clone(),
                    environment: params.environment.clone(),
                    port_mappings: params
                        .port_mappings
                        .iter()
                        .map(|p| ServicePortMapping {
                            host_port: p.host_port,
                            container_port: p.container_port,
                        })
                        .collect(),
                    volumes: params.volumes.clone(),
                    network: params.network.clone(),
                    command: params.command.clone(),
                    resource_limits: None,
                }),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(status, 200, "{}: create failed: {created}", engine.label);
        let container = created["data"]["container_name"]
            .as_str()
            .expect("the created container's name")
            .to_string();
        let host_port = created["data"]["host_port"]
            .as_u64()
            .expect("the published host port");

        // What `initialize_service_remote` stores after the create.
        let mut stored = parameters.clone();
        stored.insert("port".into(), json!(host_port.to_string()));
        stored.insert("container_name".into(), json!(container.clone()));
        let config = ServiceConfig {
            name: name.clone(),
            service_type: engine.service_type,
            version: None,
            parameters: serde_json::to_value(&stored).expect("stored parameters"),
        };

        // 3. Health, through the agent, at the stored loopback host.
        wait_healthy(&agent, &config, engine.label).await;

        // 4. Runtime and stats.
        let (status, runtime) = envelope(
            service_runtime(State(agent.clone()), Path(container.clone()))
                .await
                .into_response(),
        )
        .await;
        assert_eq!(status, 200, "{}: runtime: {runtime}", engine.label);
        let (status, stats) = envelope(
            service_stats(
                State(agent.clone()),
                Path(container.clone()),
                Query(serde_json::from_value(json!({ "samples": 1 })).expect("a stats query")),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(status, 200, "{}: stats: {stats}", engine.label);

        // 5. A linked app's environment, provisioned on the node; the
        //    engine must keep the agent's container.
        let before = container_id(&docker, &container).await;
        let (status, provisioned) = provision(&agent, &config, engine.label).await;
        let after = container_id(&docker, &container).await;
        if status != 200 {
            failures.push(format!(
                "{}: runtime provisioning failed ({status}): {provisioned}",
                engine.label
            ));
        } else {
            let environment = provisioned["data"]["environment"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            for key in engine.expected_env {
                if !environment.contains_key(*key) {
                    failures.push(format!(
                        "{}: the provisioned environment lacks {key}: {:?}",
                        engine.label,
                        environment.keys().collect::<Vec<_>>()
                    ));
                }
            }
        }
        if before != after {
            failures.push(format!(
                "{}: provisioning replaced the agent's container ({before:?} -> {after:?})",
                engine.label
            ));
        }

        // 6. The per-environment preview gives what provisioning gave.
        let (status, previewed) = preview(&agent, &config).await;
        if status != 200 {
            failures.push(format!(
                "{}: the preview failed ({status}): {previewed}",
                engine.label
            ));
        } else if previewed["data"]["environment"] != provisioned["data"]["environment"] {
            failures.push(format!(
                "{}: the preview differs from what provisioning gave out: {} vs {}",
                engine.label, previewed["data"]["environment"], provisioned["data"]["environment"]
            ));
        }
        if container_id(&docker, &container).await != after {
            failures.push(format!(
                "{}: the preview touched the container",
                engine.label
            ));
        }
        if !is_running(&docker, &container).await {
            failures.push(format!(
                "{}: the container is not running after provisioning",
                engine.label
            ));
        }

        // 7. Stop, start and remove through the agent.
        let (status, body) = envelope(
            stop_service(State(agent.clone()), Path(container.clone()))
                .await
                .into_response(),
        )
        .await;
        assert_eq!(status, 200, "{}: stop: {body}", engine.label);
        assert!(!is_running(&docker, &container).await, "{}", engine.label);
        let (status, body) = envelope(
            start_service(State(agent.clone()), Path(container.clone()))
                .await
                .into_response(),
        )
        .await;
        assert_eq!(status, 200, "{}: start: {body}", engine.label);
        assert!(is_running(&docker, &container).await, "{}", engine.label);
        wait_healthy(&agent, &config, engine.label).await;
        let (status, body) = envelope(
            remove_service(State(agent.clone()), Path(container.clone()))
                .await
                .into_response(),
        )
        .await;
        assert_eq!(status, 200, "{}: remove: {body}", engine.label);
        assert!(
            container_id(&docker, &container).await.is_none(),
            "{}: the container survived its removal",
            engine.label
        );
        // A preview of a service with no container here is refused, and
        // the engine's `init` never gets to create one.
        let (status, refused) = preview(&agent, &config).await;
        if status == 200 || !refused.to_string().contains("has no container") {
            failures.push(format!(
                "{}: a preview without a container was not refused ({status}): {refused}",
                engine.label
            ));
        }
        if container_id(&docker, &container).await.is_some() {
            // The cleanup guard removes it under the same name.
            failures.push(format!("{}: a preview created a container", engine.label));
        }
        println!(
            "{}: create, health, runtime, stats, provisioning, preview, stop/start, remove OK",
            engine.label
        );
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// `sh -c`-free exec in `container`: exit code and stdout.
async fn exec(docker: &Docker, container: &str, cmd: &[&str]) -> (i64, String) {
    let (code, stdout, _stderr) = exec_output(docker, container, cmd).await;
    (code, stdout)
}

/// [`exec`], with what the command wrote to stderr.
async fn exec_output(docker: &Docker, container: &str, cmd: &[&str]) -> (i64, String, String) {
    let output = temps_docker_util::exec_capture(
        docker,
        container,
        bollard::exec::CreateExecOptions {
            cmd: Some(cmd.to_vec()),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            ..Default::default()
        },
        temps_docker_util::ExecCaptureOptions::separate(
            temps_docker_util::OutputLimit::Head(64 * 1024),
            temps_docker_util::OutputLimit::Head(64 * 1024),
        )
        .exit_code(temps_docker_util::ExitCodePolicy::BestEffort),
    )
    .await
    .unwrap_or_else(|e| panic!("exec {cmd:?} in {container}: {e}"));
    (
        output.exit_code.unwrap_or(-1),
        output.stdout.trim().to_string(),
        output.stderr.trim().to_string(),
    )
}

async fn psql(docker: &Docker, container: &str, sql: &str) -> (i64, String) {
    exec(
        docker,
        container,
        &["psql", "-U", "postgres", "-d", "postgres", "-tAc", sql],
    )
    .await
}

/// The keys under `prefix` in `bucket`.
async fn keys(s3: &aws_sdk_s3::Client, bucket: &str, prefix: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut token = None;
    loop {
        let page = s3
            .list_objects_v2()
            .bucket(bucket)
            .prefix(prefix)
            .set_continuation_token(token)
            .send()
            .await
            .expect("list the bucket");
        keys.extend(
            page.contents()
                .iter()
                .filter_map(|o| o.key().map(str::to_string)),
        );
        match page.next_continuation_token() {
            Some(next) => token = Some(next.to_string()),
            None => return keys,
        }
    }
}

async fn walg_backup(
    agent: &Arc<AgentState>,
    request: &temps_service_protocol::PostgresWalgBackupRequest,
) -> temps_service_protocol::PostgresWalgBackupResult {
    let (status, body) = admitted(|| {
        let (agent, request) = (agent.clone(), request.clone());
        async move {
            temps_node_agent::service_data_handlers::postgres_walg_backup(
                State(agent),
                Json(request),
            )
            .await
            .into_response()
        }
    })
    .await;
    assert_eq!(status, 200, "the WAL-G backup did not run: {body}");
    serde_json::from_value(body["data"].clone()).expect("a WAL-G backup result")
}

/// The restart policy a container was created with.
fn restart_policy(
    inspect: &bollard::models::ContainerInspectResponse,
) -> Option<bollard::models::RestartPolicyNameEnum> {
    inspect
        .host_config
        .as_ref()
        .and_then(|h| h.restart_policy.as_ref())
        .and_then(|p| p.name)
}

async fn walg_restore(
    agent: &Arc<AgentState>,
    request: &temps_service_protocol::PostgresWalgRestoreRequest,
) -> temps_service_protocol::PostgresWalgRestoreResult {
    let (status, body) = admitted(|| {
        let (agent, request) = (agent.clone(), request.clone());
        async move {
            temps_node_agent::service_data_handlers::postgres_walg_restore(
                State(agent),
                Json(request),
            )
            .await
            .into_response()
        }
    })
    .await;
    assert_eq!(status, 200, "the WAL-G restore did not run: {body}");
    serde_json::from_value(body["data"].clone()).expect("a WAL-G restore result")
}

const RUSTFS_IMAGE: &str = "rustfs/rustfs:1.0.0";
const WALG_S3_KEY: &str = "walgproofkey";
const WALG_BUCKET_PATH: &str = "fleet";

/// What the control plane sends the agent for a job on `prefixes` of
/// `full`'s bucket (ADR-093): a credential the bucket's own STS (RustFS's
/// `AssumeRole`) mints for those prefixes alone, through the control plane's
/// own `WorkerCredentialScoper`. A proof that passes with it shows the
/// engine's grant is enough; one with the full key would not.
async fn scoped_destination(
    full: &temps_service_protocol::WorkerS3Destination,
    bucket_path: &str,
    prefixes: &[String],
    access: temps_core::ScopedS3Access,
) -> temps_service_protocol::WorkerS3Destination {
    use temps_providers::worker_s3_scope::{
        WorkerCredentialScope, WorkerCredentialScoper, WorkerS3Grant,
    };
    let encryption = Arc::new(
        temps_core::EncryptionService::new(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .expect("an encryption key"),
    );
    let now = chrono::Utc::now();
    let source = temps_entities::s3_sources::Model {
        id: 1,
        name: "proof".to_string(),
        bucket_name: full.bucket.clone(),
        region: full.region.clone(),
        endpoint: full.endpoint.clone(),
        bucket_path: bucket_path.to_string(),
        access_key_id: encryption
            .encrypt_string(&full.access_key_id)
            .expect("encrypt"),
        secret_key: encryption
            .encrypt_string(&full.secret_access_key)
            .expect("encrypt"),
        session_token: None,
        credentials_expire_at: None,
        force_path_style: Some(full.force_path_style),
        is_default: false,
        managed_by_cloud: false,
        lifecycle_reconcile_failed_at: None,
        lifecycle_reconcile_generation: 0,
        worker_credentials: temps_entities::s3_sources::WORKER_CREDENTIALS_SCOPED.to_string(),
        created_at: now,
        updated_at: now,
        backing_service_id: None,
    };
    let mut grant = WorkerS3Grant::job(9, 2, prefixes[0].clone(), access, "proof");
    grant.prefixes = prefixes.to_vec();
    let scoped = WorkerCredentialScoper::new(encryption)
        .destination(&source, grant)
        .await
        .unwrap_or_else(|e| panic!("RustFS did not mint a credential for {prefixes:?}: {e}"));
    assert!(
        matches!(scoped.scope, WorkerCredentialScope::Scoped { .. }),
        "{:?}",
        scoped.scope
    );
    assert_ne!(scoped.destination.access_key_id, full.access_key_id);
    scoped.destination
}

/// A worker's PostgreSQL created by the agent as a control-plane-profile
/// server asks, healthy, with an S3 bucket on its network.
struct WalgFixture {
    docker: Docker,
    agent: Arc<AgentState>,
    name: String,
    s3_name: String,
    network: String,
    container: String,
    config: ServiceConfig,
    s3: aws_sdk_s3::Client,
    bucket: String,
    _cleanup: Cleanup,
}

impl WalgFixture {
    /// `None` (and a message) when Docker or an image is missing.
    async fn start(tag: &str, bucket: &str) -> Option<Self> {
        let docker = match Docker::connect_with_defaults() {
            Ok(docker) if docker.ping().await.is_ok() => docker,
            _ => {
                println!(
                    "Docker not available (set DOCKER_HOST), skipping the worker WAL-G {tag} proof"
                );
                return None;
            }
        };
        let agent = agent_state(docker.clone());
        let suffix = std::process::id();
        let name = format!("{tag}{suffix}");
        let s3_name = format!("{tag}s3{suffix}");

        let mut parameters = control_plane_parameters(&Engine {
            label: "postgres",
            service_type: ServiceType::Postgres,
            strategy: "postgres",
            parameters: json!({ "database": "postgres", "username": "postgres" }),
            expected_env: &[],
        });
        parameters.insert("port".into(), json!(free_port().to_string()));
        let params = ExternalServiceManager::remote_create_params(
            8,
            &name,
            &ServiceType::Postgres,
            &as_strings(&parameters),
            false,
        )
        .expect("the create request");
        for image in [params.image.as_str(), RUSTFS_IMAGE] {
            if docker.inspect_image(image).await.is_err() {
                println!(
                    "image {image} is not present locally, skipping the worker WAL-G {tag} proof"
                );
                return None;
            }
        }
        let network = params
            .network
            .clone()
            .expect("worker services join a network");
        let cleanup = Cleanup {
            docker: docker.clone(),
            containers: vec![params.name.clone(), s3_name.clone()],
            volumes: params.volumes.keys().cloned().collect(),
        };

        // The bucket, on the service's network as a remote endpoint would be
        // reachable from it; this test reaches it through a published port.
        let s3_port = free_port();
        docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::default()
                        .name(&s3_name)
                        .build(),
                ),
                bollard::models::ContainerCreateBody {
                    image: Some(RUSTFS_IMAGE.to_string()),
                    env: Some(vec![
                        format!("RUSTFS_ACCESS_KEY={WALG_S3_KEY}"),
                        format!("RUSTFS_SECRET_KEY={WALG_S3_KEY}"),
                    ]),
                    host_config: Some(bollard::models::HostConfig {
                        network_mode: Some(network.clone()),
                        port_bindings: Some(HashMap::from([(
                            "9000/tcp".to_string(),
                            Some(vec![bollard::models::PortBinding {
                                host_ip: Some("127.0.0.1".to_string()),
                                host_port: Some(s3_port.to_string()),
                            }]),
                        )])),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .expect("the bucket container");

        // The agent creates the service; creating it also creates the network
        // on a fresh node, so the bucket starts after it.
        let (status, created) = envelope(
            create_service(
                State(agent.clone()),
                Json(ServiceCreateRequest {
                    name: params.name.clone(),
                    service_type: params.service_type.clone(),
                    image: params.image.clone(),
                    environment: params.environment.clone(),
                    port_mappings: params
                        .port_mappings
                        .iter()
                        .map(|p| ServicePortMapping {
                            host_port: p.host_port,
                            container_port: p.container_port,
                        })
                        .collect(),
                    volumes: params.volumes.clone(),
                    network: params.network.clone(),
                    command: params.command.clone(),
                    resource_limits: None,
                }),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(status, 200, "create failed: {created}");
        let container = created["data"]["container_name"]
            .as_str()
            .expect("the created container's name")
            .to_string();
        let host_port = created["data"]["host_port"]
            .as_u64()
            .expect("the published host port");
        docker
            .start_container(
                &s3_name,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .expect("start the bucket");

        let mut stored = parameters.clone();
        stored.insert("port".into(), json!(host_port.to_string()));
        stored.insert("container_name".into(), json!(container.clone()));
        let config = ServiceConfig {
            name: name.clone(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: serde_json::to_value(&stored).expect("stored parameters"),
        };
        wait_healthy(&agent, &config, "postgres").await;

        let s3 = temps_providers::externalsvc::S3Credentials {
            access_key_id: WALG_S3_KEY.to_string(),
            secret_key: WALG_S3_KEY.to_string(),
            session_token: None,
            region: "us-east-1".to_string(),
            endpoint: Some(format!("http://127.0.0.1:{s3_port}")),
            bucket_name: bucket.to_string(),
            bucket_path: WALG_BUCKET_PATH.to_string(),
            force_path_style: true,
        }
        .build_s3_client()
        .await;
        let mut made = Err(String::new());
        for _ in 0..60 {
            made = s3
                .create_bucket()
                .bucket(bucket)
                .send()
                .await
                .map(|_| ())
                .map_err(|e| format!("{e:?}"));
            if made.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        made.expect("creating the bucket");
        Some(Self {
            docker,
            agent,
            name,
            s3_name,
            network,
            container,
            config,
            s3,
            bucket: bucket.to_string(),
            _cleanup: cleanup,
        })
    }

    /// The bucket as the control plane sends it to the agent.
    fn destination(&self) -> temps_service_protocol::WorkerS3Destination {
        temps_service_protocol::WorkerS3Destination {
            bucket: self.bucket.clone(),
            region: "us-east-1".to_string(),
            endpoint: Some(format!("http://{}:9000", self.s3_name)),
            force_path_style: true,
            access_key_id: WALG_S3_KEY.to_string(),
            secret_access_key: WALG_S3_KEY.to_string(),
            session_token: None,
        }
    }
}

/// ADR-099, PostgreSQL step 1: a worker's WAL-G PostgreSQL, created by the
/// agent as a control-plane-profile server asks, is backed up by the control
/// plane's own WAL-G engine through the agent's sealed operation.
///
/// 1. Without a confirmed restart, a service whose `archive_mode` is off is
///    refused and left exactly as it was: same container, no `walg.env`,
///    archiving still off.
/// 2. Confirmed, archiving is turned on through the recreate guard: the
///    replacement is the agent's container (its labels, port binding and
///    network) with only `archive_mode` changed, on the same data, and the
///    base backup lands under the WAL-G prefix the control plane uses.
/// 3. A second backup restarts nothing, refreshes `walg.env`, and WAL is
///    archived continuously to the same prefix.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_worker_postgres_is_backed_up_by_the_control_plane_walg_engine_through_its_agent() {
    use temps_providers::externalsvc::postgres_walg_backup::{walg_key_prefix, walg_prefix};
    use temps_service_protocol::{
        PostgresWalgBackupRequest, PostgresWalgBackupResult, WorkerS3Destination,
    };

    let Some(fixture) = WalgFixture::start("wsewalg", "walg-proof").await else {
        return;
    };
    let WalgFixture {
        docker,
        agent,
        name,
        s3_name,
        network,
        container,
        config,
        s3,
        bucket,
        _cleanup,
    } = fixture;
    assert_eq!(
        psql(
            &docker,
            &container,
            "create table proof(x int); insert into proof values (42)"
        )
        .await
        .0,
        0
    );

    let request = |allow_restart: bool, backup_uuid: &str| PostgresWalgBackupRequest {
        service_id: 8,
        node_id: 2,
        service_config: config.clone(),
        destination: WorkerS3Destination {
            bucket: bucket.to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some(format!("http://{s3_name}:9000")),
            force_path_style: true,
            access_key_id: WALG_S3_KEY.to_string(),
            secret_access_key: WALG_S3_KEY.to_string(),
            session_token: None,
        },
        bucket_path: WALG_BUCKET_PATH.to_string(),
        backup_uuid: backup_uuid.to_string(),
        allow_restart,
    };
    let inspect = |docker: Docker, container: String| async move {
        docker
            .inspect_container(
                &container,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .expect("inspect the service container")
    };
    let walg_env_mode = || {
        exec(
            &docker,
            &container,
            &["stat", "-c", "%a", "/var/lib/postgresql/walg.env"],
        )
    };
    let original = inspect(docker.clone(), container.clone()).await;

    // 1. Unconfirmed: refused, nothing changed.
    let refused = walg_backup(&agent, &request(false, "proof-0")).await;
    assert!(
        matches!(&refused, PostgresWalgBackupResult::Refused { reason } if reason.contains("not confirmed")),
        "{refused:?}"
    );
    assert_eq!(
        inspect(docker.clone(), container.clone()).await.id,
        original.id,
        "an unconfirmed backup replaced the container"
    );
    assert_ne!(
        walg_env_mode().await.0,
        0,
        "an unconfirmed backup wrote walg.env"
    );
    assert_eq!(
        psql(&docker, &container, "show archive_mode").await.1,
        "off"
    );

    // 2. Confirmed: archiving on through the guarded recreate, then the
    //    base backup.
    let first = walg_backup(&agent, &request(true, "proof-1")).await;
    let PostgresWalgBackupResult::Completed { restarted, lsn, .. } = &first else {
        panic!("the confirmed backup failed: {first:?}");
    };
    assert!(
        *restarted,
        "archive_mode was off: the container must have been recreated"
    );
    assert!(
        lsn.as_deref().is_some_and(|lsn| lsn.contains('/')),
        "{lsn:?}"
    );
    let replaced = inspect(docker.clone(), container.clone()).await;
    assert_ne!(
        replaced.id, original.id,
        "archiving was turned on without a recreate"
    );
    let labels = |i: &bollard::models::ContainerInspectResponse| {
        i.config
            .as_ref()
            .and_then(|c| c.labels.clone())
            .unwrap_or_default()
    };
    for label in [
        "sh.temps.service.name",
        "sh.temps.service.type",
        "sh.temps.managed",
    ] {
        assert_eq!(
            labels(&replaced).get(label),
            labels(&original).get(label),
            "the recreate changed the agent's label {label}"
        );
    }
    let binding = |i: &bollard::models::ContainerInspectResponse| {
        i.host_config
            .as_ref()
            .and_then(|h| h.port_bindings.clone())
            .and_then(|b| b.get("5432/tcp").cloned().flatten())
    };
    assert_eq!(
        binding(&replaced),
        binding(&original),
        "the recreate moved the service's port"
    );
    assert_eq!(
        restart_policy(&replaced),
        Some(bollard::models::RestartPolicyNameEnum::UNLESS_STOPPED),
        "the recreate must keep the agent's restart policy"
    );
    assert_eq!(restart_policy(&replaced), restart_policy(&original));
    let networks = |i: &bollard::models::ContainerInspectResponse| {
        i.network_settings
            .as_ref()
            .and_then(|n| n.networks.as_ref())
            .map(|n| n.keys().cloned().collect::<std::collections::BTreeSet<_>>())
            .unwrap_or_default()
    };
    assert!(
        networks(&replaced).contains(&network),
        "{:?}",
        networks(&replaced)
    );
    let cmd = replaced
        .config
        .as_ref()
        .and_then(|c| c.cmd.clone())
        .unwrap_or_default();
    assert!(cmd.contains(&"archive_mode=on".to_string()), "{cmd:?}");
    assert!(!cmd.contains(&"archive_mode=off".to_string()), "{cmd:?}");
    assert_eq!(psql(&docker, &container, "show archive_mode").await.1, "on");
    assert_eq!(
        psql(&docker, &container, "select x from proof").await.1,
        "42"
    );
    assert_eq!(walg_env_mode().await, (0, "600".to_string()));
    assert_eq!(
        psql(&docker, &container, "show archive_command").await.1,
        ". /var/lib/postgresql/walg.env && wal-g wal-push %p"
    );

    // The control plane's layout, derived the same way on both sides.
    let key_prefix = walg_key_prefix(WALG_BUCKET_PATH, &name);
    assert_eq!(
        walg_prefix(&bucket, WALG_BUCKET_PATH, &name),
        format!("s3://{bucket}/{}", key_prefix.trim_end_matches('/'))
    );
    let sentinels = |keys: &[String]| {
        keys.iter()
            .filter(|k| k.starts_with(&format!("{key_prefix}basebackups_005/")))
            .filter(|k| k.ends_with("_backup_stop_sentinel.json"))
            .count()
    };
    assert_eq!(sentinels(&keys(&s3, &bucket, &key_prefix).await), 1);

    // 3. Again: nothing restarts, WAL keeps arriving.
    let second = walg_backup(&agent, &request(false, "proof-2")).await;
    assert!(
        matches!(
            &second,
            PostgresWalgBackupResult::Completed {
                restarted: false,
                ..
            }
        ),
        "{second:?}"
    );
    assert_eq!(
        inspect(docker.clone(), container.clone()).await.id,
        replaced.id,
        "a backup with archiving on restarted PostgreSQL"
    );
    assert_eq!(
        psql(&docker, &container, "select pg_switch_wal()").await.0,
        0
    );
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let all = keys(&s3, &bucket, &key_prefix).await;
        let wal = all
            .iter()
            .filter(|k| k.starts_with(&format!("{key_prefix}wal_005/")))
            .count();
        if sentinels(&all) == 2 && wal > 0 {
            println!("worker WAL-G proof: 2 base backups, {wal} archived WAL objects");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "expected 2 base backups and archived WAL under {key_prefix}: {all:?}"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let (archived, failed) = {
        let row = psql(
            &docker,
            &container,
            "select archived_count, failed_count from pg_stat_archiver",
        )
        .await
        .1;
        let mut parts = row.split('|').map(|p| p.parse::<i64>().unwrap_or(-1));
        (parts.next().unwrap_or(-1), parts.next().unwrap_or(-1))
    };
    assert!(
        archived > 0 && failed == 0,
        "archiver: {archived} archived, {failed} failed"
    );
}

/// ADR-099, PostgreSQL step 2: a worker's archiving is turned on, then
/// pointed at another S3 source, without a base backup, by the control
/// plane's own `enable_continuous_archiving` through the agent.
///
/// 1. Archiving off: the running container is recreated with archiving on
///    (its labels, port, network and restart policy kept), and WAL lands
///    under the first source's prefix. No base backup is taken.
/// 2. Re-pointed: nothing restarts, `walg.env` names the second prefix, and
///    the next WAL lands there.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_worker_postgres_archive_is_turned_on_and_repointed_through_its_agent() {
    use temps_providers::externalsvc::postgres_walg_backup::{walg_key_prefix, walg_prefix};
    use temps_service_protocol::{PostgresWalgArchiveRequest, PostgresWalgArchiveResult};

    let Some(fixture) = WalgFixture::start("wsearch", "walg-archive").await else {
        return;
    };
    let fixture = &fixture;
    let docker = &fixture.docker;
    let container = fixture.container.as_str();
    let archive = |bucket_path: &str| PostgresWalgArchiveRequest {
        service_id: 8,
        node_id: 2,
        service_config: fixture.config.clone(),
        destination: fixture.destination(),
        bucket_path: bucket_path.to_string(),
    };
    let repoint = |request: PostgresWalgArchiveRequest| async move {
        let (status, body) = admitted(|| {
            let (agent, request) = (fixture.agent.clone(), request.clone());
            async move {
                temps_node_agent::service_data_handlers::postgres_walg_archive(
                    State(agent),
                    Json(request),
                )
                .await
                .into_response()
            }
        })
        .await;
        assert_eq!(status, 200, "the re-point did not run: {body}");
        serde_json::from_value::<PostgresWalgArchiveResult>(body["data"].clone())
            .expect("an archive result")
    };
    let inspect = || async {
        docker
            .inspect_container(
                container,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .expect("inspect the service container")
    };
    // WAL under `bucket_path`'s prefix once a segment is switched out.
    let wal_lands_under = |bucket_path: &'static str| async move {
        let key_prefix = format!("{}wal_005/", walg_key_prefix(bucket_path, &fixture.name));
        let before = keys(&fixture.s3, &fixture.bucket, &key_prefix).await.len();
        let (code, out) = psql(
            docker,
            container,
            "create table if not exists t(x int); insert into t values (1); select pg_switch_wal()",
        )
        .await;
        assert_eq!(code, 0, "{out}");
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            let now = keys(&fixture.s3, &fixture.bucket, &key_prefix).await.len();
            if now > before {
                return now;
            }
            assert!(
                Instant::now() < deadline,
                "no WAL arrived under {key_prefix}"
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    };
    let created = inspect().await;
    assert_eq!(psql(docker, container, "show archive_mode").await.1, "off");

    // 1. On, to the first source.
    let first = repoint(archive("fleet-a")).await;
    assert_eq!(
        first,
        PostgresWalgArchiveResult::Completed { restarted: true }
    );
    let recreated = inspect().await;
    assert_ne!(recreated.id, created.id, "archive_mode was off: a recreate");
    assert_eq!(
        restart_policy(&recreated),
        Some(bollard::models::RestartPolicyNameEnum::UNLESS_STOPPED)
    );
    let label = |i: &bollard::models::ContainerInspectResponse| {
        i.config
            .as_ref()
            .and_then(|c| c.labels.clone())
            .and_then(|l| l.get("sh.temps.service.name").cloned())
    };
    assert_eq!(label(&recreated), label(&created));
    assert_eq!(psql(docker, container, "show archive_mode").await.1, "on");
    let walg_env = || async {
        exec(docker, container, &["cat", "/var/lib/postgresql/walg.env"])
            .await
            .1
    };
    assert!(
        walg_env()
            .await
            .contains(&walg_prefix(&fixture.bucket, "fleet-a", &fixture.name)),
        "walg.env names the first prefix"
    );
    wal_lands_under("fleet-a").await;
    let base_backups = keys(
        &fixture.s3,
        &fixture.bucket,
        &format!(
            "{}basebackups_005/",
            walg_key_prefix("fleet-a", &fixture.name)
        ),
    )
    .await;
    assert!(
        base_backups.is_empty(),
        "a re-point takes no base backup: {base_backups:?}"
    );

    // 2. Re-pointed to the second source: no restart, WAL moves.
    let second = repoint(archive("fleet-b")).await;
    assert_eq!(
        second,
        PostgresWalgArchiveResult::Completed { restarted: false }
    );
    assert_eq!(
        inspect().await.id,
        recreated.id,
        "a re-point with archiving on restarted PostgreSQL"
    );
    let env = walg_env().await;
    assert!(
        env.contains(&walg_prefix(&fixture.bucket, "fleet-b", &fixture.name)),
        "walg.env names the second prefix"
    );
    let landed = wal_lands_under("fleet-b").await;
    println!("worker WAL-G archive proof: on with a recreate, re-pointed, {landed} WAL objects under the second prefix");
}

/// ADR-099, PostgreSQL step 3: a worker's WAL-G PostgreSQL is restored in
/// place, and to a point in time, by the control plane's own WAL-G restore
/// through the agent's sealed operation, and a restore that fails leaves the
/// database it found.
///
/// Rows A, base backup 1, rows B, time T, rows C, base backup 2. Then:
/// 1. PITR from backup 1 to T has A and B, not C.
/// 2. An in-place restore of the latest backup has A, B and C.
/// 3. A PITR to a point the archive never reached fails: the original data
///    (A, B, C) is put back and running, as it was.
///
/// Every restore keeps the agent's container (labels, port, network, restart
/// policy), leaves archiving suspended, and deletes the original it kept.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_worker_postgres_is_restored_and_recovered_to_a_point_in_time_through_its_agent() {
    use temps_providers::externalsvc::postgres_walg_backup::walg_prefix;
    use temps_service_protocol::{
        PostgresWalgBackupRequest, PostgresWalgBackupResult, PostgresWalgRestoreRequest,
        PostgresWalgRestoreResult, RecoveryTarget,
    };

    let Some(fixture) = WalgFixture::start("wsepitr", "walg-pitr").await else {
        return;
    };
    let docker = &fixture.docker;
    let container = fixture.container.as_str();
    let sql = |statement: &'static str| async move {
        let (code, out) = psql(docker, container, statement).await;
        assert_eq!(code, 0, "{statement}: {out}");
        out
    };
    let rows = || async {
        psql(
            docker,
            container,
            "select string_agg(tag, ',' order by tag) from proof",
        )
        .await
        .1
    };
    let backup = |uuid: &str| PostgresWalgBackupRequest {
        service_id: 8,
        node_id: 2,
        service_config: fixture.config.clone(),
        destination: fixture.destination(),
        bucket_path: WALG_BUCKET_PATH.to_string(),
        backup_uuid: uuid.to_string(),
        allow_restart: true,
    };
    let restore = |target: Option<RecoveryTarget>, uuid: Option<&str>| PostgresWalgRestoreRequest {
        service_id: 8,
        node_id: 2,
        service_config: fixture.config.clone(),
        live_service_config: fixture.config.clone(),
        source: fixture.destination(),
        walg_prefix: walg_prefix(&fixture.bucket, WALG_BUCKET_PATH, &fixture.name),
        recovery_target: target,
        target_user_data: uuid
            .map(|uuid| serde_json::json!({ "temps_backup_id": uuid }).to_string()),
        operation_id: None,
    };
    let inspect = || async {
        docker
            .inspect_container(
                container,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .expect("inspect the service container")
    };
    let labels = |i: &bollard::models::ContainerInspectResponse| {
        i.config
            .as_ref()
            .and_then(|c| c.labels.clone())
            .unwrap_or_default()
    };
    let binding = |i: &bollard::models::ContainerInspectResponse| {
        i.host_config
            .as_ref()
            .and_then(|h| h.port_bindings.clone())
            .and_then(|b| b.get("5432/tcp").cloned().flatten())
    };
    let created = inspect().await;
    // The agent's own container, as every restore must leave it.
    let assert_agents_container = |i: &bollard::models::ContainerInspectResponse, step: &str| {
        for label in [
            "sh.temps.service.name",
            "sh.temps.service.type",
            "sh.temps.managed",
        ] {
            assert_eq!(
                labels(i).get(label),
                labels(&created).get(label),
                "{step}: label {label}"
            );
        }
        assert_eq!(binding(i), binding(&created), "{step}: port");
        assert_eq!(
            restart_policy(i),
            Some(bollard::models::RestartPolicyNameEnum::UNLESS_STOPPED),
            "{step}: restart policy"
        );
        let on_network = i
            .network_settings
            .as_ref()
            .and_then(|n| n.networks.as_ref())
            .is_some_and(|n| n.contains_key(&fixture.network));
        assert!(on_network, "{step}: network");
    };
    let kept_original = || async {
        exec(
            docker,
            container,
            &["test", "-e", "/var/lib/postgresql/pre_restore"],
        )
        .await
        .0 == 0
    };

    sql("create table proof(tag text)").await;
    sql("insert into proof values ('a')").await;
    let first = walg_backup(&fixture.agent, &backup("pitr-1")).await;
    assert!(
        matches!(first, PostgresWalgBackupResult::Completed { .. }),
        "{first:?}"
    );
    sql("insert into proof values ('b')").await;
    let t = sql(
        "select to_char(clock_timestamp() at time zone 'utc', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')",
    )
    .await;
    let t: chrono::DateTime<chrono::Utc> = t.parse().unwrap_or_else(|e| panic!("T '{t}': {e}"));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    sql("insert into proof values ('c')").await;
    let second = walg_backup(&fixture.agent, &backup("pitr-2")).await;
    assert!(
        matches!(second, PostgresWalgBackupResult::Completed { .. }),
        "{second:?}"
    );
    // The segment holding C's commit is archived: recovery to T needs a
    // record after T to stop at.
    sql("select pg_switch_wal()").await;
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let pending = psql(
            docker,
            container,
            "select count(*) from pg_ls_dir('pg_wal/archive_status') f where f like '%.ready'",
        )
        .await
        .1;
        if pending == "0" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "WAL was not archived: {pending} ready"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(rows().await, "a,b,c");

    // 1. PITR from backup 1 to T.
    let pitr = walg_restore(
        &fixture.agent,
        &restore(Some(RecoveryTarget::Time { time: t }), Some("pitr-1")),
    )
    .await;
    assert_eq!(pitr, PostgresWalgRestoreResult::Completed, "{pitr:?}");
    assert_eq!(rows().await, "a,b", "PITR to T has A and B, not C");
    assert_eq!(sql("select pg_is_in_recovery()").await, "f");
    assert_eq!(sql("show archive_mode").await, "off");
    assert!(
        !kept_original().await,
        "the PITR kept the original after success"
    );
    assert_agents_container(&inspect().await, "PITR");

    // 2. In place, the latest backup.
    let latest = walg_restore(&fixture.agent, &restore(None, None)).await;
    assert_eq!(latest, PostgresWalgRestoreResult::Completed, "{latest:?}");
    assert_eq!(rows().await, "a,b,c", "the latest backup has everything");
    assert_eq!(sql("show archive_mode").await, "off");
    assert!(!kept_original().await);
    let before_failure = inspect().await;
    assert_agents_container(&before_failure, "in-place restore");

    // 3. A target the archive never reached: recovery cannot promote, and
    //    the original is put back.
    let unreachable = walg_restore(
        &fixture.agent,
        &restore(
            Some(RecoveryTarget::Time {
                time: chrono::Utc::now() + chrono::Duration::days(1),
            }),
            Some("pitr-1"),
        ),
    )
    .await;
    match &unreachable {
        PostgresWalgRestoreResult::Failed { reason } => assert!(
            reason.contains("original database was put back unchanged"),
            "{reason}"
        ),
        other => panic!("a restore to an unreachable point must fail: {other:?}"),
    }
    assert_eq!(rows().await, "a,b,c", "the failed restore changed the data");
    assert_eq!(sql("select pg_is_in_recovery()").await, "f");
    assert!(
        !kept_original().await,
        "the rollback left the kept original"
    );
    assert_agents_container(&inspect().await, "rollback");
    println!("worker WAL-G restore proof: PITR to {t}, in place, and a rolled-back failure");
}

/// ADR-099, PostgreSQL step 4: another service's WAL-G backup is restored
/// into a worker's PostgreSQL through its agent, as a cross-service restore
/// on the control plane: the restored data answers to the origin's login,
/// which the control plane sends as the restored login, while the target's
/// own login is the running server's until the swap.
///
/// Origin X: rows A, base backup, rows B, time T, rows C. Target Y, with its
/// own data and password: a PITR from X's backup to T leaves Y with A and B
/// (not C, not Y's own rows), on Y's container, answering to X's password
/// and no longer to Y's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_services_walg_backup_is_restored_into_a_worker_postgres_through_its_agent() {
    use temps_providers::externalsvc::postgres_walg_backup::walg_prefix;
    use temps_service_protocol::{
        PostgresWalgBackupRequest, PostgresWalgBackupResult, PostgresWalgRestoreRequest,
        PostgresWalgRestoreResult, RecoveryTarget,
    };

    let Some(origin) = WalgFixture::start("wsexo", "walg-cross").await else {
        return;
    };
    let Some(target) = WalgFixture::start("wsexy", "walg-cross-y").await else {
        return;
    };
    let docker = &origin.docker;
    let password = |fixture: &WalgFixture| {
        fixture.config.parameters["password"]
            .as_str()
            .expect("a generated password")
            .to_string()
    };
    let (origin_password, target_password) = (password(&origin), password(&target));
    assert_ne!(origin_password, target_password);
    let sql = |container: &str, statement: &str| {
        let container = container.to_string();
        let statement = statement.to_string();
        async move {
            let (code, out) = psql(docker, &container, &statement).await;
            assert_eq!(code, 0, "{statement}: {out}");
            out
        }
    };
    // A login over the container's network address, as an application
    // connects: the image trusts loopback, so only this checks the password.
    let login = |container: &str, password: &str| {
        let container = container.to_string();
        let env = format!("PGPASSWORD={password}");
        async move {
            let output = temps_docker_util::exec_capture(
                docker,
                &container,
                bollard::exec::CreateExecOptions {
                    cmd: Some(vec![
                        "sh",
                        "-c",
                        "psql -h \"$(hostname -i | cut -d' ' -f1)\" -U postgres -d postgres \
                         -tAc 'select 1'",
                    ]),
                    env: Some(vec![env.as_str()]),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
                temps_docker_util::ExecCaptureOptions::separate(
                    temps_docker_util::OutputLimit::Head(4096),
                    temps_docker_util::OutputLimit::Head(4096),
                )
                .exit_code(temps_docker_util::ExitCodePolicy::BestEffort),
            )
            .await
            .expect("exec psql over TCP");
            output.exit_code == Some(0)
        }
    };
    assert!(login(&target.container, &target_password).await);
    assert!(!login(&target.container, &origin_password).await);

    let x = origin.container.as_str();
    let y = target.container.as_str();
    sql(y, "create table mine(x int); insert into mine values (7)").await;
    sql(
        x,
        "create table proof(tag text); insert into proof values ('a')",
    )
    .await;
    let backup = walg_backup(
        &origin.agent,
        &PostgresWalgBackupRequest {
            service_id: 8,
            node_id: 2,
            service_config: origin.config.clone(),
            destination: origin.destination(),
            bucket_path: WALG_BUCKET_PATH.to_string(),
            backup_uuid: "cross-1".to_string(),
            allow_restart: true,
        },
    )
    .await;
    assert!(
        matches!(backup, PostgresWalgBackupResult::Completed { .. }),
        "{backup:?}"
    );
    sql(x, "insert into proof values ('b')").await;
    let t = sql(
        x,
        "select to_char(clock_timestamp() at time zone 'utc', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')",
    )
    .await;
    let t: chrono::DateTime<chrono::Utc> = t.parse().unwrap_or_else(|e| panic!("T '{t}': {e}"));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    sql(x, "insert into proof values ('c')").await;
    sql(x, "select pg_switch_wal()").await;
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let pending = psql(
            docker,
            x,
            "select count(*) from pg_ls_dir('pg_wal/archive_status') f where f like '%.ready'",
        )
        .await
        .1;
        if pending == "0" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "WAL was not archived: {pending} ready"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    // What the control plane sends for a cross-service restore: the
    // target's configuration with the origin's login merged in, and the
    // target's own as the running server's.
    let mut restored_login = target.config.clone();
    for key in ["password", "username", "database"] {
        restored_login.parameters[key] = origin.config.parameters[key].clone();
    }
    let created = docker
        .inspect_container(
            y,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .expect("inspect the target");
    // The origin's bucket, reached from the target's container on the
    // shared network.
    let restored = walg_restore(
        &target.agent,
        &PostgresWalgRestoreRequest {
            service_id: 8,
            node_id: 2,
            service_config: restored_login,
            live_service_config: target.config.clone(),
            source: origin.destination(),
            walg_prefix: walg_prefix(&origin.bucket, WALG_BUCKET_PATH, &origin.name),
            recovery_target: Some(RecoveryTarget::Time { time: t }),
            target_user_data: Some(serde_json::json!({ "temps_backup_id": "cross-1" }).to_string()),
            operation_id: None,
        },
    )
    .await;
    assert_eq!(
        restored,
        PostgresWalgRestoreResult::Completed,
        "{restored:?}"
    );
    assert_eq!(
        psql(
            docker,
            y,
            "select string_agg(tag, ',' order by tag) from proof"
        )
        .await
        .1,
        "a,b",
        "the target holds the origin's data to T"
    );
    assert_ne!(
        psql(docker, y, "select count(*) from mine").await.0,
        0,
        "the target's own table is gone"
    );
    assert!(
        login(y, &origin_password).await,
        "the restored target answers to the origin's login"
    );
    assert!(
        !login(y, &target_password).await,
        "the target's previous password no longer works"
    );
    let replaced = docker
        .inspect_container(
            y,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .expect("inspect the target");
    let service_label = |i: &bollard::models::ContainerInspectResponse| {
        i.config
            .as_ref()
            .and_then(|c| c.labels.clone())
            .and_then(|l| l.get("sh.temps.service.name").cloned())
    };
    assert_eq!(service_label(&replaced), service_label(&created));
    assert_eq!(
        restart_policy(&replaced),
        Some(bollard::models::RestartPolicyNameEnum::UNLESS_STOPPED)
    );
    // The origin is untouched.
    assert_eq!(
        psql(
            docker,
            x,
            "select string_agg(tag, ',' order by tag) from proof"
        )
        .await
        .1,
        "a,b,c"
    );
    println!("cross-service WAL-G restore proof: origin's backup recovered into the target to {t}");
}

/// The same WAL-G restore on the control plane (ADR-099: one implementation,
/// the transport alone differs): a PostgreSQL the provider created and backs
/// up is recovered to a point in time, restored in place, and left as it was
/// by a restore that cannot promote, with the provider's own container
/// specification (restart `always`) each time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_control_plane_postgres_is_restored_and_recovered_by_the_same_walg_restore() {
    use temps_providers::externalsvc::postgres::PostgresService;
    use temps_providers::externalsvc::postgres_walg_backup::{
        walg_base_backup, walg_prefix, ArchiveRecreate, WalgBaseBackup,
    };
    use temps_providers::externalsvc::{ExternalService, RecoveryTarget, S3Credentials};

    let docker = match Docker::connect_with_defaults() {
        Ok(docker) if docker.ping().await.is_ok() => docker,
        _ => {
            println!(
                "Docker not available (set DOCKER_HOST), skipping the local WAL-G restore proof"
            );
            return;
        }
    };
    const IMAGE: &str = "gotempsh/postgres-walg:18-bookworm";
    for image in [IMAGE, RUSTFS_IMAGE] {
        if docker.inspect_image(image).await.is_err() {
            println!(
                "image {image} is not present locally, skipping the local WAL-G restore proof"
            );
            return;
        }
    }
    let suffix = std::process::id();
    let name = format!("cplpitr{suffix}");
    let container = format!("postgres-{name}");
    let s3_name = format!("cplpitrs3{suffix}");
    let bucket = "walg-local-pitr";
    let mut cleanup = Cleanup {
        docker: docker.clone(),
        containers: vec![container.clone(), s3_name.clone()],
        volumes: Vec::new(),
    };
    let shared = Arc::new(docker.clone());
    let config = ServiceConfig {
        name: name.clone(),
        service_type: ServiceType::Postgres,
        version: None,
        parameters: json!({
            "host": "localhost",
            "port": free_port().to_string(),
            "database": "postgres",
            "username": "postgres",
            "password": "local-proof-password",
            "docker_image": IMAGE,
        }),
    };
    let postgres = PostgresService::new(name.clone(), shared.clone());
    let runtime = postgres
        .init(config.clone())
        .await
        .expect("the provider creates the service");
    let config = ServiceConfig {
        parameters: serde_json::to_value(&runtime).expect("runtime parameters"),
        ..config
    };
    let created = docker
        .inspect_container(
            &container,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .expect("the provider's container");
    cleanup.volumes = created
        .mounts
        .iter()
        .flatten()
        .filter_map(|m| m.name.clone())
        .collect();

    // The bucket, on the provider's network.
    let s3_port = free_port();
    docker
        .create_container(
            Some(
                bollard::query_parameters::CreateContainerOptionsBuilder::default()
                    .name(&s3_name)
                    .build(),
            ),
            bollard::models::ContainerCreateBody {
                image: Some(RUSTFS_IMAGE.to_string()),
                env: Some(vec![
                    format!("RUSTFS_ACCESS_KEY={WALG_S3_KEY}"),
                    format!("RUSTFS_SECRET_KEY={WALG_S3_KEY}"),
                ]),
                host_config: Some(bollard::models::HostConfig {
                    network_mode: Some(temps_core::NETWORK_NAME.to_string()),
                    port_bindings: Some(HashMap::from([(
                        "9000/tcp".to_string(),
                        Some(vec![bollard::models::PortBinding {
                            host_ip: Some("127.0.0.1".to_string()),
                            host_port: Some(s3_port.to_string()),
                        }]),
                    )])),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .expect("the bucket container");
    docker
        .start_container(
            &s3_name,
            None::<bollard::query_parameters::StartContainerOptions>,
        )
        .await
        .expect("start the bucket");
    let credentials = |endpoint: String| S3Credentials {
        access_key_id: WALG_S3_KEY.to_string(),
        secret_key: WALG_S3_KEY.to_string(),
        session_token: None,
        region: "us-east-1".to_string(),
        endpoint: Some(endpoint),
        bucket_name: bucket.to_string(),
        bucket_path: WALG_BUCKET_PATH.to_string(),
        force_path_style: true,
    };
    let s3 = credentials(format!("http://127.0.0.1:{s3_port}"))
        .build_s3_client()
        .await;
    let mut made = Err(String::new());
    for _ in 0..60 {
        made = s3
            .create_bucket()
            .bucket(bucket)
            .send()
            .await
            .map(|_| ())
            .map_err(|e| format!("{e:?}"));
        if made.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    made.expect("creating the bucket");
    let in_network = credentials(format!("http://{s3_name}:9000"));

    let docker = &docker;
    let container = container.as_str();
    let deadline = Instant::now() + Duration::from_secs(90);
    while psql(docker, container, "select 1").await.0 != 0 {
        assert!(
            Instant::now() < deadline,
            "the provider's PostgreSQL never accepted queries"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let sql = |statement: &'static str| async move {
        let (code, out) = psql(docker, container, statement).await;
        assert_eq!(code, 0, "{statement}: {out}");
        out
    };
    let rows = || async {
        psql(
            docker,
            container,
            "select string_agg(tag, ',' order by tag) from proof",
        )
        .await
        .1
    };
    let backup = |uuid: &'static str| {
        let shared = shared.clone();
        let config = config.clone();
        let s3 = in_network.clone();
        async move {
            walg_base_backup(
                shared,
                WalgBaseBackup {
                    service_id: 1,
                    container_name: container.to_string(),
                    service_config: config,
                    s3,
                    backup_uuid: uuid.to_string(),
                    allow_restart: true,
                },
                ArchiveRecreate::Provider,
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap_or_else(|e| panic!("backup {uuid}: {e}"))
        }
    };
    let prefix = walg_prefix(bucket, WALG_BUCKET_PATH, &name);
    let restore = |target: Option<RecoveryTarget>, uuid: Option<&'static str>| {
        let config = config.clone();
        let s3 = in_network.clone();
        let prefix = prefix.clone();
        let postgres = PostgresService::new(name.clone(), shared.clone());
        async move {
            let user_data =
                uuid.map(|uuid| serde_json::json!({ "temps_backup_id": uuid }).to_string());
            postgres
                .restore_walg_in_place(
                    &s3,
                    &prefix,
                    config.clone(),
                    &config,
                    target.as_ref(),
                    user_data.as_deref(),
                    &temps_providers::externalsvc::NoopRestoreGate,
                )
                .await
        }
    };
    let restart = || async {
        restart_policy(
            &docker
                .inspect_container(
                    container,
                    None::<bollard::query_parameters::InspectContainerOptions>,
                )
                .await
                .expect("inspect"),
        )
    };
    let kept_original = || async {
        exec(
            docker,
            container,
            &["test", "-e", "/var/lib/postgresql/pre_restore"],
        )
        .await
        .0 == 0
    };

    sql("create table proof(tag text)").await;
    sql("insert into proof values ('a')").await;
    backup("local-1").await;
    sql("insert into proof values ('b')").await;
    let t = sql(
        "select to_char(clock_timestamp() at time zone 'utc', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')",
    )
    .await;
    let t: chrono::DateTime<chrono::Utc> = t.parse().unwrap_or_else(|e| panic!("T '{t}': {e}"));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    sql("insert into proof values ('c')").await;
    backup("local-2").await;
    sql("select pg_switch_wal()").await;
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let pending = psql(
            docker,
            container,
            "select count(*) from pg_ls_dir('pg_wal/archive_status') f where f like '%.ready'",
        )
        .await
        .1;
        if pending == "0" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "WAL was not archived: {pending} ready"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    restore(Some(RecoveryTarget::Time { time: t }), Some("local-1"))
        .await
        .expect("PITR to T");
    assert_eq!(rows().await, "a,b", "PITR to T has A and B, not C");
    assert_eq!(sql("show archive_mode").await, "off");
    assert!(!kept_original().await);
    assert_eq!(
        restart().await,
        Some(bollard::models::RestartPolicyNameEnum::ALWAYS)
    );

    restore(None, None)
        .await
        .expect("in-place restore of the latest backup");
    assert_eq!(rows().await, "a,b,c");
    assert!(!kept_original().await);

    let error = restore(
        Some(RecoveryTarget::Time {
            time: chrono::Utc::now() + chrono::Duration::days(1),
        }),
        Some("local-1"),
    )
    .await
    .expect_err("a restore to an unreachable point fails");
    assert!(
        error
            .to_string()
            .contains("original database was put back unchanged"),
        "{error}"
    );
    assert_eq!(rows().await, "a,b,c", "the failed restore changed the data");
    assert_eq!(sql("select pg_is_in_recovery()").await, "f");
    assert!(!kept_original().await);
    assert_eq!(
        restart().await,
        Some(bollard::models::RestartPolicyNameEnum::ALWAYS)
    );
    drop(cleanup);
}

/// Garage reads a `garage.toml` the agent's create request cannot carry, so
/// a worker Garage is refused when the request is built, before any node is
/// contacted, with the remedy for a control-plane-profile server.
#[test]
fn a_garage_service_on_a_worker_is_refused_before_the_agent_is_called() {
    let parameters = HashMap::from([("backend".to_string(), "garage".to_string())]);
    let error = ExternalServiceManager::remote_create_params(
        7,
        "assets",
        &ServiceType::S3,
        &parameters,
        false,
    )
    .expect_err("a worker cannot create Garage yet");
    let message = error.to_string();
    assert!(message.contains("garage.toml"), "{message}");
    assert!(message.contains("rustfs"), "{message}");
}

/// Managed S3 storage on a worker is restored in place exactly as on the
/// control plane: the agent's `data-restore` mirrors each backed-up bucket
/// with `--overwrite --remove`, so objects written after the backup are
/// deleted and changed ones put back, and the bucket holds what the backup
/// holds, no more.
///
/// 40 objects are backed up through the agent's `data-backup`; then 5 are
/// added and one is overwritten; the restore leaves exactly the 40, with
/// the overwritten one back to its backed-up content.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_worker_s3_restore_in_place_leaves_exactly_the_backed_up_objects() {
    use temps_node_agent::service_data_handlers::{data_backup, data_restore};
    use temps_service_protocol::{
        RemoteServiceBackupRequest, RemoteServiceRestoreRequest, WorkerS3Destination,
    };

    let docker = match Docker::connect_with_defaults() {
        Ok(docker) if docker.ping().await.is_ok() => docker,
        _ => {
            println!(
                "Docker not available (set DOCKER_HOST), skipping the worker S3 restore proof"
            );
            return;
        }
    };
    let agent = agent_state(docker.clone());
    let suffix = std::process::id();
    let name = format!("wses3r{suffix}");
    let store_name = format!("wses3rbk{suffix}");
    let engine = engines()
        .into_iter()
        .find(|engine| engine.label == "rustfs")
        .expect("the RustFS engine");
    let mut parameters = control_plane_parameters(&engine);
    parameters.insert("port".into(), json!(free_port().to_string()));
    let params = ExternalServiceManager::remote_create_params(
        9,
        &name,
        &engine.service_type,
        &as_strings(&parameters),
        false,
    )
    .expect("the create request");
    for image in [params.image.as_str(), RUSTFS_IMAGE] {
        if docker.inspect_image(image).await.is_err() {
            println!("image {image} is not present locally, skipping the worker S3 restore proof");
            return;
        }
    }
    let network = params
        .network
        .clone()
        .expect("worker services join a network");
    let _cleanup = Cleanup {
        docker: docker.clone(),
        containers: vec![params.name.clone(), store_name.clone()],
        volumes: params.volumes.keys().cloned().collect(),
    };
    let (status, created) = envelope(
        create_service(
            State(agent.clone()),
            Json(ServiceCreateRequest {
                name: params.name.clone(),
                service_type: params.service_type.clone(),
                image: params.image.clone(),
                environment: params.environment.clone(),
                port_mappings: params
                    .port_mappings
                    .iter()
                    .map(|p| ServicePortMapping {
                        host_port: p.host_port,
                        container_port: p.container_port,
                    })
                    .collect(),
                volumes: params.volumes.clone(),
                network: params.network.clone(),
                command: params.command.clone(),
                resource_limits: None,
            }),
        )
        .await
        .into_response(),
    )
    .await;
    assert_eq!(status, 200, "create failed: {created}");
    let container = created["data"]["container_name"]
        .as_str()
        .expect("the created container's name")
        .to_string();
    let host_port = created["data"]["host_port"]
        .as_u64()
        .expect("the published host port");
    let mut stored = parameters.clone();
    stored.insert("port".into(), json!(host_port.to_string()));
    stored.insert("container_name".into(), json!(container.clone()));
    let config = ServiceConfig {
        name: name.clone(),
        service_type: engine.service_type,
        version: None,
        parameters: serde_json::to_value(&stored).expect("stored parameters"),
    };
    wait_healthy(&agent, &config, "rustfs").await;

    // The backup store: another RustFS on the service's network.
    let store_port = free_port();
    docker
        .create_container(
            Some(
                bollard::query_parameters::CreateContainerOptionsBuilder::default()
                    .name(&store_name)
                    .build(),
            ),
            bollard::models::ContainerCreateBody {
                image: Some(RUSTFS_IMAGE.to_string()),
                env: Some(vec![
                    format!("RUSTFS_ACCESS_KEY={WALG_S3_KEY}"),
                    format!("RUSTFS_SECRET_KEY={WALG_S3_KEY}"),
                ]),
                host_config: Some(bollard::models::HostConfig {
                    network_mode: Some(network.clone()),
                    port_bindings: Some(HashMap::from([(
                        "9000/tcp".to_string(),
                        Some(vec![bollard::models::PortBinding {
                            host_ip: Some("127.0.0.1".to_string()),
                            host_port: Some(store_port.to_string()),
                        }]),
                    )])),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .expect("the backup store container");
    docker
        .start_container(
            &store_name,
            None::<bollard::query_parameters::StartContainerOptions>,
        )
        .await
        .expect("start the backup store");

    let client = |endpoint: String, key: &str, secret: &str| {
        let credentials = temps_providers::externalsvc::S3Credentials {
            access_key_id: key.to_string(),
            secret_key: secret.to_string(),
            session_token: None,
            region: "us-east-1".to_string(),
            endpoint: Some(endpoint),
            bucket_name: String::new(),
            bucket_path: String::new(),
            force_path_style: true,
        };
        async move { credentials.build_s3_client().await }
    };
    let text = |key: &str| stored[key].as_str().expect("a generated key").to_string();
    let service = client(
        format!("http://127.0.0.1:{host_port}"),
        &text("access_key"),
        &text("secret_key"),
    )
    .await;
    let store = client(
        format!("http://127.0.0.1:{store_port}"),
        WALG_S3_KEY,
        WALG_S3_KEY,
    )
    .await;
    async fn make_bucket(s3: &aws_sdk_s3::Client, bucket: &str) {
        let mut made = Err(String::new());
        for _ in 0..60 {
            made = s3
                .create_bucket()
                .bucket(bucket)
                .send()
                .await
                .map(|_| ())
                .map_err(|e| format!("{e:?}"));
            if made.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        made.unwrap_or_else(|e| panic!("creating bucket {bucket}: {e}"));
    }
    make_bucket(&service, "files").await;
    make_bucket(&store, "backups").await;
    let put = |key: String, body: &'static str| {
        let service = service.clone();
        async move {
            service
                .put_object()
                .bucket("files")
                .key(&key)
                .body(aws_sdk_s3::primitives::ByteStream::from_static(
                    body.as_bytes(),
                ))
                .send()
                .await
                .unwrap_or_else(|e| panic!("put {key}: {e:?}"));
        }
    };
    for i in 0..40 {
        put(format!("docs/{i:02}.txt"), "backed up").await;
    }
    let objects = || {
        let service = service.clone();
        async move {
            let mut keys = Vec::new();
            let mut token = None;
            loop {
                let page = service
                    .list_objects_v2()
                    .bucket("files")
                    .set_continuation_token(token)
                    .send()
                    .await
                    .expect("list the bucket");
                keys.extend(
                    page.contents()
                        .iter()
                        .filter_map(|o| o.key().map(str::to_string)),
                );
                token = page.next_continuation_token().map(str::to_string);
                if token.is_none() {
                    break;
                }
            }
            keys.sort();
            keys
        }
    };
    let backed_up = objects().await;
    assert_eq!(backed_up.len(), 40);

    let destination = WorkerS3Destination {
        bucket: "backups".to_string(),
        region: "us-east-1".to_string(),
        endpoint: Some(format!("http://{store_name}:9000")),
        force_path_style: true,
        access_key_id: WALG_S3_KEY.to_string(),
        secret_access_key: WALG_S3_KEY.to_string(),
        session_token: None,
    };
    let prefix = "external_services/s3/proof/backup-1";
    let (status, body) = admitted(|| {
        let request = RemoteServiceBackupRequest {
            service_config: config.clone(),
            destination: destination.clone(),
            key: prefix.to_string(),
        };
        let agent = agent.clone();
        async move {
            data_backup(State(agent), Json(request))
                .await
                .into_response()
        }
    })
    .await;
    assert_eq!(status, 200, "the backup failed: {body}");

    // After the backup: 5 new objects and one changed.
    for i in 40..45 {
        put(format!("docs/{i:02}.txt"), "after the backup").await;
    }
    put("docs/00.txt".to_string(), "changed after the backup").await;
    assert_eq!(objects().await.len(), 45);

    let (status, body) = admitted(|| {
        let request = RemoteServiceRestoreRequest {
            service_config: config.clone(),
            source: destination.clone(),
            location: prefix.to_string(),
        };
        let agent = agent.clone();
        async move {
            data_restore(State(agent), Json(request))
                .await
                .into_response()
        }
    })
    .await;
    assert_eq!(status, 200, "the restore failed: {body}");
    let restored = objects().await;
    assert_eq!(
        restored.len(),
        40,
        "objects written after the backup must be removed: {restored:?}"
    );
    assert_eq!(restored, backed_up);
    let first = service
        .get_object()
        .bucket("files")
        .key("docs/00.txt")
        .send()
        .await
        .expect("read the restored object")
        .body
        .collect()
        .await
        .expect("the restored object's body")
        .into_bytes();
    assert_eq!(&first[..], b"backed up");
    println!(
        "worker S3 restore proof: 45 objects before, {} after",
        restored.len()
    );
}

const MONGO_WALG_IMAGE: &str = "gotempsh/mongodb-walg:8.0";
const MONGO_PLAIN_IMAGE: &str = "mongo:7.0";
const MONGO_BUCKET_PATH: &str = "fleet";

/// A worker's MongoDB created by the agent as a control-plane-profile server
/// asks, healthy, with an S3 bucket on its network that the agent process
/// reaches through a published port (as the control plane's archive upload
/// and download do) and WAL-G inside the container reaches by its network.
struct MongoFixture {
    docker: Docker,
    agent: Arc<AgentState>,
    name: String,
    container: String,
    config: ServiceConfig,
    s3: aws_sdk_s3::Client,
    s3_port: u16,
    bucket: String,
    _cleanup: Cleanup,
}

impl MongoFixture {
    /// `None` (and a message) when Docker or an image is missing.
    async fn start(tag: &str, image: &str, bucket: &str) -> Option<Self> {
        let docker = match Docker::connect_with_defaults() {
            Ok(docker) if docker.ping().await.is_ok() => docker,
            _ => {
                println!(
                    "Docker not available (set DOCKER_HOST), skipping the worker MongoDB {tag} proof"
                );
                return None;
            }
        };
        let agent = agent_state(docker.clone());
        let suffix = std::process::id();
        let name = format!("{tag}{suffix}");
        let s3_name = format!("{tag}s3{suffix}");

        let mut parameters = control_plane_parameters(&Engine {
            label: "mongodb",
            service_type: ServiceType::Mongodb,
            strategy: "mongodb",
            parameters: json!({ "docker_image": image, "database": "admin", "username": "admin" }),
            expected_env: &[],
        });
        // The generated password may start with `-`, which a tool given it
        // as a separate argument takes for an option; make every proof's
        // password do so, so that case is always exercised.
        let generated = parameters
            .get("password")
            .and_then(Value::as_str)
            .expect("a generated password")
            .to_string();
        parameters.insert("password".into(), json!(format!("-{generated}")));
        parameters.insert("port".into(), json!(free_port().to_string()));
        let params = ExternalServiceManager::remote_create_params(
            9,
            &name,
            &ServiceType::Mongodb,
            &as_strings(&parameters),
            false,
        )
        .expect("the create request");
        assert_eq!(
            params.image, image,
            "the create request runs the asked image"
        );
        for needed in [
            params.image.as_str(),
            RUSTFS_IMAGE,
            temps_providers::externalsvc::mongodb_backup::MONGODUMP_HELPER_IMAGE,
        ] {
            if docker.inspect_image(needed).await.is_err() {
                println!(
                    "image {needed} is not present locally, skipping the worker MongoDB {tag} proof"
                );
                return None;
            }
        }
        let network = params
            .network
            .clone()
            .expect("worker services join a network");
        let cleanup = Cleanup {
            docker: docker.clone(),
            containers: vec![params.name.clone(), s3_name.clone()],
            volumes: params.volumes.keys().cloned().collect(),
        };

        let s3_port = free_port();
        docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::default()
                        .name(&s3_name)
                        .build(),
                ),
                bollard::models::ContainerCreateBody {
                    image: Some(RUSTFS_IMAGE.to_string()),
                    env: Some(vec![
                        format!("RUSTFS_ACCESS_KEY={WALG_S3_KEY}"),
                        format!("RUSTFS_SECRET_KEY={WALG_S3_KEY}"),
                    ]),
                    host_config: Some(bollard::models::HostConfig {
                        network_mode: Some(network.clone()),
                        port_bindings: Some(HashMap::from([(
                            "9000/tcp".to_string(),
                            Some(vec![bollard::models::PortBinding {
                                host_ip: Some("127.0.0.1".to_string()),
                                host_port: Some(s3_port.to_string()),
                            }]),
                        )])),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .expect("the bucket container");

        let (status, created) = envelope(
            create_service(
                State(agent.clone()),
                Json(ServiceCreateRequest {
                    name: params.name.clone(),
                    service_type: params.service_type.clone(),
                    image: params.image.clone(),
                    environment: params.environment.clone(),
                    port_mappings: params
                        .port_mappings
                        .iter()
                        .map(|p| ServicePortMapping {
                            host_port: p.host_port,
                            container_port: p.container_port,
                        })
                        .collect(),
                    volumes: params.volumes.clone(),
                    network: params.network.clone(),
                    command: params.command.clone(),
                    resource_limits: None,
                }),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(status, 200, "create failed: {created}");
        let container = created["data"]["container_name"]
            .as_str()
            .expect("the created container's name")
            .to_string();
        let host_port = created["data"]["host_port"]
            .as_u64()
            .expect("the published host port");
        docker
            .start_container(
                &s3_name,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .expect("start the bucket");

        let mut stored = parameters.clone();
        stored.insert("port".into(), json!(host_port.to_string()));
        stored.insert("container_name".into(), json!(container.clone()));
        let config = ServiceConfig {
            name: name.clone(),
            service_type: ServiceType::Mongodb,
            version: None,
            parameters: serde_json::to_value(&stored).expect("stored parameters"),
        };
        wait_healthy(&agent, &config, "mongodb").await;

        let s3 = temps_providers::externalsvc::S3Credentials {
            access_key_id: WALG_S3_KEY.to_string(),
            secret_key: WALG_S3_KEY.to_string(),
            session_token: None,
            region: "us-east-1".to_string(),
            endpoint: Some(format!("http://127.0.0.1:{s3_port}")),
            bucket_name: bucket.to_string(),
            bucket_path: MONGO_BUCKET_PATH.to_string(),
            force_path_style: true,
        }
        .build_s3_client()
        .await;
        let mut made = Err(String::new());
        for _ in 0..60 {
            made = s3
                .create_bucket()
                .bucket(bucket)
                .send()
                .await
                .map(|_| ())
                .map_err(|e| format!("{e:?}"));
            if made.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        made.expect("creating the bucket");
        let fixture = Self {
            docker,
            agent,
            name,
            container,
            config,
            s3,
            s3_port,
            bucket: bucket.to_string(),
            _cleanup: cleanup,
        };
        // A fresh mongod may answer the health probe before it accepts the
        // root login; wait until it does, for a bounded time.
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let (code, out, err) = fixture
                .mongosh_output(&fixture.password(), "print('ready')")
                .await;
            if code == 0 && out.ends_with("ready") {
                break;
            }
            if Instant::now() >= deadline {
                let state = fixture
                    .docker
                    .inspect_container(
                        &fixture.container,
                        None::<bollard::query_parameters::InspectContainerOptions>,
                    )
                    .await
                    .map(|inspect| {
                        format!("{:?}, restarts {:?}", inspect.state, inspect.restart_count)
                    })
                    .unwrap_or_else(|e| format!("not inspectable: {e}"));
                let err: String = err.chars().take(2000).collect();
                panic!(
                    "mongod never accepted the root login within 120 s: mongosh exited {code}, \
                     stdout {out:?}, stderr {err:?}; container {state}"
                );
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Some(fixture)
    }

    fn password(&self) -> String {
        self.config.parameters["password"]
            .as_str()
            .expect("a generated password")
            .to_string()
    }

    /// `script` against database `shop`, logged in as the root user with
    /// `password`: exit code and output.
    async fn mongosh(&self, password: &str, script: &str) -> (i64, String) {
        let (code, stdout, _stderr) = self.mongosh_output(password, script).await;
        (code, stdout)
    }

    /// [`Self::mongosh`], with what it wrote to stderr, the password
    /// masked.
    ///
    /// The password is passed attached (`--password=...`): a generated one
    /// may start with `-`, and given as the next argument mongosh would take
    /// it for an unknown option, never log in, and echo it in its error.
    async fn mongosh_output(&self, password: &str, script: &str) -> (i64, String, String) {
        let password_arg = format!("--password={password}");
        let (code, stdout, stderr) = exec_output(
            &self.docker,
            &self.container,
            &[
                "mongosh",
                "--quiet",
                "--username",
                "admin",
                &password_arg,
                "--authenticationDatabase",
                "admin",
                "shop",
                "--eval",
                script,
            ],
        )
        .await;
        let stderr = if password.is_empty() {
            stderr
        } else {
            stderr.replace(password, "***")
        };
        (code, stdout, stderr)
    }

    /// The `x` of every document in `shop.proof`, in `_id` order.
    async fn proof(&self) -> String {
        let (code, out) = self
            .mongosh(
                &self.password(),
                "print(db.proof.find().sort({_id: 1}).toArray().map(d => d.x).join(','))",
            )
            .await;
        assert_eq!(code, 0, "reading shop.proof failed: {out}");
        out.lines().last().unwrap_or_default().to_string()
    }

    async fn write(&self, script: &str) {
        let (code, out) = self.mongosh(&self.password(), script).await;
        assert_eq!(code, 0, "{script}: {out}");
    }

    /// The bucket as the control plane sends it to the agent.
    fn destination(&self) -> temps_service_protocol::WorkerS3Destination {
        temps_service_protocol::WorkerS3Destination {
            bucket: self.bucket.clone(),
            region: "us-east-1".to_string(),
            endpoint: Some(format!("http://127.0.0.1:{}", self.s3_port)),
            force_path_style: true,
            access_key_id: WALG_S3_KEY.to_string(),
            secret_access_key: WALG_S3_KEY.to_string(),
            session_token: None,
        }
    }

    /// The bucket as the control plane's own engine holds it.
    fn credentials(&self) -> temps_providers::externalsvc::S3Credentials {
        temps_providers::externalsvc::S3Credentials {
            access_key_id: WALG_S3_KEY.to_string(),
            secret_key: WALG_S3_KEY.to_string(),
            session_token: None,
            region: "us-east-1".to_string(),
            endpoint: Some(format!("http://127.0.0.1:{}", self.s3_port)),
            bucket_name: self.bucket.clone(),
            bucket_path: MONGO_BUCKET_PATH.to_string(),
            force_path_style: true,
        }
    }

    /// A location this service's backups never use: an empty WAL-G
    /// repository, or an archive key nothing was written to.
    fn missing_location(&self, walg: bool) -> String {
        use temps_providers::externalsvc::mongodb_backup::{archive_key, walg_prefix};
        if walg {
            walg_prefix(&self.bucket, MONGO_BUCKET_PATH, "nobackuphere")
        } else {
            archive_key(MONGO_BUCKET_PATH, &self.name, "missing", chrono::Utc::now())
        }
    }
}

/// How a proof reaches the engine: through the worker's agent, or as the
/// control plane runs it for a service on its own host.
#[derive(Clone, Copy, Debug)]
enum MongoPath {
    Agent,
    ControlPlane,
}

/// What a backup wrote, with where a restore reads it.
#[derive(Debug)]
struct MongoBackup {
    walg: bool,
    location: String,
}

async fn mongo_backup(fixture: &MongoFixture, path: MongoPath, uuid: &str) -> MongoBackup {
    use temps_providers::externalsvc::mongodb_backup::{
        mongodb_backup, walg_prefix, MongodbBackup, MongodbBackupOutcome,
    };
    use temps_service_protocol::{MongodbBackupRequest, MongodbBackupResult};

    let walg = |_: ()| MongoBackup {
        walg: true,
        location: walg_prefix(&fixture.bucket, MONGO_BUCKET_PATH, &fixture.name),
    };
    match path {
        MongoPath::Agent => {
            // As the control plane: check the image through the agent, pick
            // the backup, scope the credential to its one location.
            use temps_providers::externalsvc::mongodb_backup::{archive_key, walg_key_prefix};
            use temps_service_protocol::MongodbBackupMode;
            let has_walg = temps_providers::externalsvc::mongodb::MongodbService::new(
                fixture.name.clone(),
                Arc::new(fixture.docker.clone()),
            )
            .container_has_walg(&fixture.container)
            .await;
            let (mode, prefix) = if has_walg {
                (
                    MongodbBackupMode::WalgStream,
                    walg_key_prefix(MONGO_BUCKET_PATH, &fixture.name),
                )
            } else {
                let key = archive_key(MONGO_BUCKET_PATH, &fixture.name, uuid, chrono::Utc::now());
                let prefix = temps_providers::worker_s3_scope::key_directory(&key);
                (MongodbBackupMode::Archive { key }, prefix)
            };
            let destination = scoped_destination(
                &fixture.destination(),
                MONGO_BUCKET_PATH,
                &[prefix],
                temps_core::ScopedS3Access::Write,
            )
            .await;
            let (status, body) = admitted(|| {
                let request = MongodbBackupRequest {
                    service_id: 9,
                    node_id: 2,
                    service_config: fixture.config.clone(),
                    destination: destination.clone(),
                    bucket_path: MONGO_BUCKET_PATH.to_string(),
                    backup_uuid: uuid.to_string(),
                    mode: mode.clone(),
                };
                let agent = fixture.agent.clone();
                async move {
                    temps_node_agent::service_data_handlers::mongodb_backup(
                        State(agent),
                        Json(request),
                    )
                    .await
                    .into_response()
                }
            })
            .await;
            assert_eq!(status, 200, "the agent did not run the backup: {body}");
            let result: MongodbBackupResult =
                serde_json::from_value(body["data"].clone()).expect("a MongoDB backup result");
            match result {
                MongodbBackupResult::WalgStream { .. } => walg(()),
                MongodbBackupResult::Archive { key, size_bytes } => {
                    assert!(size_bytes > 0, "an empty archive at {key}");
                    MongoBackup {
                        walg: false,
                        location: key,
                    }
                }
                other => panic!("the backup through the agent failed: {other:?}"),
            }
        }
        MongoPath::ControlPlane => {
            let outcome = mongodb_backup(
                Arc::new(fixture.docker.clone()),
                MongodbBackup {
                    service_id: 9,
                    container_name: fixture.container.clone(),
                    service_config: fixture.config.clone(),
                    s3: fixture.credentials(),
                    backup_uuid: uuid.to_string(),
                    mode: None,
                },
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap_or_else(|e| panic!("the control plane's backup failed: {e}"));
            match outcome {
                MongodbBackupOutcome::WalgStream { .. } => walg(()),
                MongodbBackupOutcome::Archive { key, size_bytes } => {
                    assert!(size_bytes > 0, "an empty archive at {key}");
                    MongoBackup {
                        walg: false,
                        location: key,
                    }
                }
            }
        }
    }
}

/// Restore `location`, in the bucket of `held_by`, in place into `fixture`;
/// `Err` is the engine's failure. `origin_password` is a cross-service
/// restore's origin login.
async fn mongo_restore(
    fixture: &MongoFixture,
    held_by: &MongoFixture,
    path: MongoPath,
    location: &str,
    origin_password: Option<String>,
) -> Result<(), String> {
    use temps_providers::externalsvc::mongodb::MongodbService;
    use temps_providers::externalsvc::{ExternalService, NoopRestoreGate, RestoreContext};
    use temps_service_protocol::{MongodbRestoreRequest, MongodbRestoreResult};

    let request = MongodbRestoreRequest {
        service_id: 9,
        node_id: 2,
        service_config: fixture.config.clone(),
        origin_password,
        source: held_by.destination(),
        location: location.to_string(),
        operation_id: Some("restore-run-9".to_string()),
    };
    match path {
        MongoPath::Agent => {
            // As the control plane: read access to this one location.
            let read_prefix = match location.strip_prefix("s3://") {
                Some(_) => {
                    temps_providers::worker_s3_scope::s3_url_prefix(location, &held_by.bucket)
                        .expect("a repository inside the bucket")
                }
                None => temps_providers::worker_s3_scope::key_directory(location),
            };
            let request = MongodbRestoreRequest {
                source: scoped_destination(
                    &held_by.destination(),
                    MONGO_BUCKET_PATH,
                    &[read_prefix],
                    temps_core::ScopedS3Access::Read,
                )
                .await,
                ..request
            };
            let (status, body) = admitted(|| {
                let (agent, request) = (fixture.agent.clone(), request.clone());
                async move {
                    temps_node_agent::service_data_handlers::mongodb_restore(
                        State(agent),
                        Json(request),
                    )
                    .await
                    .into_response()
                }
            })
            .await;
            assert_eq!(status, 200, "the agent did not run the restore: {body}");
            let outcome: MongodbRestoreResult =
                serde_json::from_value(body["data"].clone()).expect("a MongoDB restore result");
            // The node kept the outcome, for a control plane whose request
            // went unanswered.
            assert_eq!(
                recorded_restore_outcome(
                    temps_service_protocol::WorkerRestoreEngine::Mongodb,
                    &request.service_config.name,
                    "restore-run-9",
                )
                .await,
                temps_service_protocol::WorkerRestoreProgress::Finished {
                    outcome: outcome.clone().into()
                }
            );
            match outcome {
                MongodbRestoreResult::Completed => Ok(()),
                MongodbRestoreResult::Failed { reason } => Err(reason),
                MongodbRestoreResult::Refused { reason } => {
                    panic!("the agent refused a restore of its own container: {reason}")
                }
            }
        }
        MongoPath::ControlPlane => {
            // What `RestoreService` hands the provider for a local service.
            let s3 = held_by.credentials();
            let client = s3.build_s3_client().await;
            let now = chrono::Utc::now();
            let source = temps_entities::s3_sources::Model {
                id: 1,
                name: "proof".to_string(),
                bucket_name: held_by.bucket.clone(),
                region: "us-east-1".to_string(),
                endpoint: s3.endpoint.clone(),
                bucket_path: MONGO_BUCKET_PATH.to_string(),
                access_key_id: WALG_S3_KEY.to_string(),
                secret_key: WALG_S3_KEY.to_string(),
                session_token: None,
                credentials_expire_at: None,
                force_path_style: Some(true),
                is_default: false,
                managed_by_cloud: false,
                lifecycle_reconcile_failed_at: None,
                lifecycle_reconcile_generation: 0,
                worker_credentials: temps_entities::s3_sources::WORKER_CREDENTIALS_SCOPED
                    .to_string(),
                created_at: now,
                updated_at: now,
                backing_service_id: None,
            };
            let backup = temps_entities::backups::Model {
                id: 1,
                name: "proof".to_string(),
                backup_id: "proof".to_string(),
                schedule_id: None,
                schedule_run_id: None,
                backup_type: "full".to_string(),
                state: "completed".to_string(),
                started_at: now,
                finished_at: Some(now),
                size_bytes: None,
                file_count: None,
                s3_source_id: 1,
                s3_location: location.to_string(),
                error_message: None,
                metadata: "{}".to_string(),
                checksum: None,
                compression_type: "gzip".to_string(),
                created_by: 0,
                expires_at: None,
                tags: "[]".to_string(),
            };
            let target = temps_entities::external_services::Model {
                id: 9,
                name: fixture.name.clone(),
                service_type: "mongodb".to_string(),
                version: None,
                status: "running".to_string(),
                created_at: now,
                updated_at: now,
                slug: None,
                config: None,
                node_id: None,
                topology: "standalone".to_string(),
                error_message: None,
                health_status: None,
                last_health_check_at: None,
                last_health_error: None,
                consecutive_health_failures: 0,
                health_metadata: None,
                metrics_enabled: false,
                default_backup_provisioned: false,
                container_name: None,
                created_by_user_id: None,
                continuous_archive_s3_source_id: None,
                continuous_archive_pinned_at: None,
                workload_mode: Default::default(),
                log_ingest_lines_per_sec: None,
                log_ingest_kib_per_sec: None,
            };
            // The MongoDB restore reads no rows: a disconnected pool.
            let pool = Default::default();
            MongodbService::new(fixture.name.clone(), Arc::new(fixture.docker.clone()))
                .restore_in_place(RestoreContext {
                    s3_client: &client,
                    s3_credentials: &s3,
                    s3_source: &source,
                    backup: &backup,
                    backup_location: location,
                    source_service: &target,
                    source_config: request.engine_config(),
                    live_target_config: fixture.config.clone(),
                    pool: &pool,
                    gate: &NoopRestoreGate,
                })
                .await
                .map_err(|e| e.to_string())
        }
    }
}

/// Back up, change the data, fail a restore, restore: the sequence every
/// MongoDB proof runs, through `path`.
async fn mongo_backup_and_restore(fixture: &MongoFixture, path: MongoPath, expect_walg: bool) {
    fixture
        .write("db.proof.insertMany([{_id: 1, x: 42}, {_id: 2, x: 43}])")
        .await;
    assert_eq!(fixture.proof().await, "42,43");

    let backup = mongo_backup(fixture, path, "proof-1").await;
    assert_eq!(
        backup.walg, expect_walg,
        "{path:?}: the engine chose the wrong backup for this image: {backup:?}"
    );
    if backup.walg {
        let key_prefix = temps_providers::externalsvc::mongodb_backup::walg_key_prefix(
            MONGO_BUCKET_PATH,
            &fixture.name,
        );
        let written = keys(&fixture.s3, &fixture.bucket, &key_prefix).await;
        assert!(
            written
                .iter()
                .any(|k| k.starts_with(&format!("{key_prefix}basebackups_005/"))),
            "{path:?}: no WAL-G backup under {key_prefix}: {written:?}"
        );
    } else {
        assert!(
            temps_providers::externalsvc::mongodb_backup::is_archive_key_of(
                &backup.location,
                MONGO_BUCKET_PATH,
                &fixture.name,
                "proof-1",
            ),
            "{path:?}: the archive is not where the control plane records it: {}",
            backup.location
        );
        assert_eq!(
            keys(&fixture.s3, &fixture.bucket, &backup.location).await,
            vec![backup.location.clone()]
        );
    }

    // Changed after the backup.
    fixture
        .write("db.proof.deleteOne({_id: 1}); db.proof.insertOne({_id: 3, x: 7})")
        .await;
    assert_eq!(fixture.proof().await, "43,7");

    // A restore that fails leaves the data as it was: a location with no
    // backup, and (for an archive) one that is not a mongodump archive.
    let missing = fixture.missing_location(backup.walg);
    let failed = mongo_restore(fixture, fixture, path, &missing, None).await;
    assert!(
        failed.is_err(),
        "{path:?}: restoring from {missing} succeeded"
    );
    assert_eq!(
        fixture.proof().await,
        "43,7",
        "{path:?}: a failed restore from {missing} changed the data: {failed:?}"
    );
    if !backup.walg {
        let corrupt = format!("{MONGO_BUCKET_PATH}/corrupt-{}/dump.archive", fixture.name);
        fixture
            .s3
            .put_object()
            .bucket(&fixture.bucket)
            .key(&corrupt)
            .body(aws_sdk_s3::primitives::ByteStream::from_static(
                b"not a mongodump archive",
            ))
            .send()
            .await
            .expect("upload the corrupt archive");
        let failed = mongo_restore(fixture, fixture, path, &corrupt, None).await;
        assert!(failed.is_err(), "{path:?}: restoring garbage succeeded");
        assert_eq!(
            fixture.proof().await,
            "43,7",
            "{path:?}: a failed restore of a corrupt archive changed the data: {failed:?}"
        );
    }

    // The backup, in place.
    mongo_restore(fixture, fixture, path, &backup.location, None)
        .await
        .unwrap_or_else(|e| panic!("{path:?}: restoring {} failed: {e}", backup.location));
    assert_eq!(
        fixture.proof().await,
        "42,43",
        "{path:?}: the restore did not bring back the backed-up data"
    );
    println!(
        "MongoDB proof ({path:?}, {}): backed up to {}, a failed restore changed nothing, the \
         restore brought the data back",
        if backup.walg { "WAL-G" } else { "archive" },
        backup.location
    );
}

/// ADR-099, MongoDB: a worker's MongoDB, created by the agent as a
/// control-plane-profile server asks, is backed up and restored in place by
/// the control plane's own MongoDB engine through the agent's sealed routes.
/// A WAL-G image streams into its WAL-G repository; an image without WAL-G
/// gets a mongodump archive at the control plane's key. A restore that fails
/// (no backup at the location, a corrupt archive) leaves the data as it was.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_worker_mongodb_is_backed_up_and_restored_by_the_control_plane_engine_through_its_agent()
{
    for (tag, image, walg) in [
        ("wsemgw", MONGO_WALG_IMAGE, true),
        ("wsemga", MONGO_PLAIN_IMAGE, false),
    ] {
        let Some(fixture) = MongoFixture::start(tag, image, &format!("{tag}-bucket")).await else {
            continue;
        };
        mongo_backup_and_restore(&fixture, MongoPath::Agent, walg).await;
    }
}

/// The same sequence as the control plane runs it for a MongoDB on its own
/// host: the same backup function with its own Docker, the provider's same
/// in-place restore, with no agent in between.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_control_plane_mongodb_is_backed_up_and_restored_by_the_same_engine() {
    for (tag, image, walg) in [
        ("cplmgw", MONGO_WALG_IMAGE, true),
        ("cplmga", MONGO_PLAIN_IMAGE, false),
    ] {
        let Some(fixture) = MongoFixture::start(tag, image, &format!("{tag}-bucket")).await else {
            continue;
        };
        mongo_backup_and_restore(&fixture, MongoPath::ControlPlane, walg).await;
    }
}

/// A cross-service restore through the agent: another MongoDB's WAL-G backup
/// restored in place into a worker's MongoDB brings the origin's
/// `admin.system.users`, so the target answers to the origin's password
/// afterwards, which the control plane then stores. The origin's password
/// reaches the engine only as its fallback login.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_services_mongodb_backup_is_restored_into_a_worker_mongodb_through_its_agent() {
    let Some(origin) = MongoFixture::start("wsemxo", MONGO_WALG_IMAGE, "wsemx-origin").await else {
        return;
    };
    let Some(target) = MongoFixture::start("wsemxt", MONGO_WALG_IMAGE, "wsemx-target").await else {
        return;
    };
    let (origin_password, target_password) = (origin.password(), target.password());
    assert_ne!(origin_password, target_password);
    origin.write("db.proof.insertOne({_id: 1, x: 99})").await;
    target.write("db.proof.insertOne({_id: 1, x: 1})").await;

    let backup = mongo_backup(&origin, MongoPath::Agent, "cross-1").await;
    assert!(backup.walg, "{backup:?}");
    mongo_restore(
        &target,
        &origin,
        MongoPath::Agent,
        &backup.location,
        Some(origin_password.clone()),
    )
    .await
    .unwrap_or_else(|e| panic!("the cross-service restore failed: {e}"));

    let (code, out) = target
        .mongosh(
            &origin_password,
            "print(db.proof.find().toArray().map(d => d.x).join(','))",
        )
        .await;
    assert_eq!(
        code, 0,
        "the target does not answer to the origin's login: {out}"
    );
    assert_eq!(out.lines().last().unwrap_or_default(), "99");

    // Restored again, the target now answers only to the origin's
    // password: the engine falls back to it, as the control plane does on a
    // retried cross-service restore.
    mongo_restore(
        &target,
        &origin,
        MongoPath::Agent,
        &backup.location,
        Some(origin_password.clone()),
    )
    .await
    .unwrap_or_else(|e| panic!("the retried cross-service restore failed: {e}"));
}

const REDIS_WALG_BUCKET: &str = "redis-parity";

/// `redis-cli` in `container`, authenticated: its trimmed stdout.
async fn redis_cli(docker: &Docker, container: &str, password: &str, args: &[&str]) -> String {
    let mut cmd = vec!["redis-cli", "--no-auth-warning", "-a", password];
    cmd.extend_from_slice(args);
    let (code, out) = exec(docker, container, &cmd).await;
    assert_eq!(code, 0, "redis-cli {args:?} in {container}: {out}");
    out
}

/// Wait until `container` answers `PING` (after a restore restarts it).
async fn wait_redis(docker: &Docker, container: &str, password: &str) {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let (code, out) = exec(
            docker,
            container,
            &["redis-cli", "--no-auth-warning", "-a", password, "PING"],
        )
        .await;
        if code == 0 && out == "PONG" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "Redis in {container} never answered: {code} {out}"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// The files a restore may leave on the data volume besides Redis's own.
async fn leftovers(docker: &Docker, container: &str) -> String {
    exec(
        docker,
        container,
        &["sh", "-c", "ls -a /data | grep '^\\.temps-' || true"],
    )
    .await
    .1
}

/// A RustFS bucket on `network`, published on a local port: the client
/// for this test, the port, and the bucket's name on the network.
async fn redis_bucket(docker: &Docker, s3_name: &str, network: &str) -> (aws_sdk_s3::Client, u16) {
    let s3_port = free_port();
    docker
        .create_container(
            Some(
                bollard::query_parameters::CreateContainerOptionsBuilder::default()
                    .name(s3_name)
                    .build(),
            ),
            bollard::models::ContainerCreateBody {
                image: Some(RUSTFS_IMAGE.to_string()),
                env: Some(vec![
                    format!("RUSTFS_ACCESS_KEY={WALG_S3_KEY}"),
                    format!("RUSTFS_SECRET_KEY={WALG_S3_KEY}"),
                ]),
                host_config: Some(bollard::models::HostConfig {
                    network_mode: Some(network.to_string()),
                    port_bindings: Some(HashMap::from([(
                        "9000/tcp".to_string(),
                        Some(vec![bollard::models::PortBinding {
                            host_ip: Some("127.0.0.1".to_string()),
                            host_port: Some(s3_port.to_string()),
                        }]),
                    )])),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .expect("the bucket container");
    docker
        .start_container(
            s3_name,
            None::<bollard::query_parameters::StartContainerOptions>,
        )
        .await
        .expect("start the bucket");
    let s3 = temps_providers::externalsvc::S3Credentials {
        access_key_id: WALG_S3_KEY.to_string(),
        secret_key: WALG_S3_KEY.to_string(),
        session_token: None,
        region: "us-east-1".to_string(),
        endpoint: Some(format!("http://127.0.0.1:{s3_port}")),
        bucket_name: REDIS_WALG_BUCKET.to_string(),
        bucket_path: WALG_BUCKET_PATH.to_string(),
        force_path_style: true,
    }
    .build_s3_client()
    .await;
    let mut made = Err(String::new());
    for _ in 0..60 {
        made = s3
            .create_bucket()
            .bucket(REDIS_WALG_BUCKET)
            .send()
            .await
            .map(|_| ())
            .map_err(|e| format!("{e:?}"));
        if made.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    made.expect("creating the bucket");
    (s3, s3_port)
}

/// The bucket as a node reaches it: on the network (for WAL-G, which runs
/// in the container) or on the published port (for the RDB upload and
/// download, which the node's process does).
fn redis_destination(endpoint: String) -> temps_service_protocol::WorkerS3Destination {
    temps_service_protocol::WorkerS3Destination {
        bucket: REDIS_WALG_BUCKET.to_string(),
        region: "us-east-1".to_string(),
        endpoint: Some(endpoint),
        force_path_style: true,
        access_key_id: WALG_S3_KEY.to_string(),
        secret_access_key: WALG_S3_KEY.to_string(),
        session_token: None,
    }
}

/// What the control plane sends for one Redis job (ADR-093): a credential
/// RustFS mints for `prefix` alone, through the port this process reaches,
/// handed to the node with the endpoint it reaches the bucket at.
async fn redis_scoped(
    reached_at: &temps_service_protocol::WorkerS3Destination,
    minted_through: &temps_service_protocol::WorkerS3Destination,
    prefix: String,
    access: temps_core::ScopedS3Access,
) -> temps_service_protocol::WorkerS3Destination {
    temps_service_protocol::WorkerS3Destination {
        endpoint: reached_at.endpoint.clone(),
        ..scoped_destination(minted_through, WALG_BUCKET_PATH, &[prefix], access).await
    }
}

/// The read grant of a Redis restore of `location`, as the control plane
/// derives it: a WAL-G repository as its prefix, a snapshot as its
/// directory.
fn redis_read_prefix(location: &str) -> String {
    match location.strip_prefix("s3://") {
        Some(_) => temps_providers::worker_s3_scope::s3_url_prefix(location, REDIS_WALG_BUCKET)
            .expect("a repository in the bucket"),
        None => temps_providers::worker_s3_scope::key_directory(location),
    }
}

/// One Redis agent route, called as the agent serves it: the engine's
/// outcome, from a 200.
async fn redis_route<Request, Outcome, Handler, Fut>(
    agent: &Arc<AgentState>,
    handler: Handler,
    request: Request,
) -> Outcome
where
    Request: Clone,
    Handler: Fn(State<Arc<AgentState>>, Json<Request>) -> Fut,
    Fut: std::future::Future<Output = axum::response::Response>,
    Outcome: serde::de::DeserializeOwned,
{
    let (status, body) = admitted(|| handler(State(agent.clone()), Json(request.clone()))).await;
    assert_eq!(status, 200, "the Redis operation did not run: {body}");
    serde_json::from_value(body["data"].clone())
        .unwrap_or_else(|e| panic!("a Redis outcome ({e}): {body}"))
}

/// ADR-099 for Redis: a worker's Redis, created by the agent as a
/// control-plane-profile server asks, is backed up and restored by the
/// control plane's own Redis engine through the agent's sealed routes.
///
/// 1. A WAL-G stream backup lands in the repository the control plane uses,
///    and an RDB backup (the fallback for an image without WAL-G) in the
///    local engine's `dump.rdb.gz` key.
/// 2. Each is restored in place after the data changed: the data is the
///    backup's again, in the same container with its own restart policy,
///    and no staged or kept file is left on the volume.
/// 3. A restore that cannot fetch its backup leaves the running Redis and
///    its data exactly as they were.
/// 4. Restoring another service's backup keeps the target's own Temps
///    bookkeeping (`_temps:*`); the target's own backup brings its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_worker_redis_is_backed_up_and_restored_by_the_control_plane_engine_through_its_agent() {
    use temps_node_agent::service_data_handlers::{
        redis_rdb_backup, redis_restore, redis_walg_backup,
    };
    use temps_providers::externalsvc::redis_node::{walg_key_prefix, walg_prefix};
    use temps_service_protocol::{
        RedisRdbBackupRequest, RedisRdbBackupResult, RedisRestoreRequest, RedisRestoreResult,
        RedisWalgBackupRequest, RedisWalgBackupResult,
    };

    let docker = match Docker::connect_with_defaults() {
        Ok(docker) if docker.ping().await.is_ok() => docker,
        _ => {
            println!("Docker not available (set DOCKER_HOST), skipping the worker Redis proof");
            return;
        }
    };
    let agent = agent_state(docker.clone());
    let suffix = std::process::id();
    let name = format!("wredis{suffix}");
    let s3_name = format!("wredis3{suffix}");
    let engine = engines()
        .into_iter()
        .find(|engine| engine.label == "redis")
        .expect("the Redis engine");
    let mut parameters = control_plane_parameters(&engine);
    parameters.insert("port".into(), json!(free_port().to_string()));
    // `requirepass`, so every step authenticates as the engine does.
    parameters.insert("password".into(), json!("worker-redis-proof"));
    let params = ExternalServiceManager::remote_create_params(
        9,
        &name,
        &ServiceType::Redis,
        &as_strings(&parameters),
        false,
    )
    .expect("the create request");
    for image in [
        params.image.as_str(),
        RUSTFS_IMAGE,
        temps_providers::externalsvc::redis_node::RDB_HELPER_IMAGE,
    ] {
        if docker.inspect_image(image).await.is_err() {
            println!("image {image} is not present locally, skipping the worker Redis proof");
            return;
        }
    }
    let network = params
        .network
        .clone()
        .expect("worker services join a network");
    let _cleanup = Cleanup {
        docker: docker.clone(),
        containers: vec![params.name.clone(), s3_name.clone()],
        volumes: params.volumes.keys().cloned().collect(),
    };
    let (status, created) = envelope(
        create_service(
            State(agent.clone()),
            Json(ServiceCreateRequest {
                name: params.name.clone(),
                service_type: params.service_type.clone(),
                image: params.image.clone(),
                environment: params.environment.clone(),
                port_mappings: params
                    .port_mappings
                    .iter()
                    .map(|p| ServicePortMapping {
                        host_port: p.host_port,
                        container_port: p.container_port,
                    })
                    .collect(),
                volumes: params.volumes.clone(),
                network: params.network.clone(),
                command: params.command.clone(),
                resource_limits: None,
            }),
        )
        .await
        .into_response(),
    )
    .await;
    assert_eq!(status, 200, "create failed: {created}");
    let container = created["data"]["container_name"]
        .as_str()
        .expect("the created container's name")
        .to_string();
    let host_port = created["data"]["host_port"]
        .as_u64()
        .expect("the published host port");
    let mut stored = parameters.clone();
    stored.insert("port".into(), json!(host_port.to_string()));
    stored.insert("container_name".into(), json!(container.clone()));
    let config = ServiceConfig {
        name: name.clone(),
        service_type: ServiceType::Redis,
        version: None,
        parameters: serde_json::to_value(&stored).expect("stored parameters"),
    };
    wait_healthy(&agent, &config, "redis").await;
    let password = stored["password"].as_str().expect("a password").to_string();
    let (s3, s3_port) = redis_bucket(&docker, &s3_name, &network).await;
    let in_network = redis_destination(format!("http://{s3_name}:9000"));
    let published = redis_destination(format!("http://127.0.0.1:{s3_port}"));
    let policy_before = restart_policy(
        &docker
            .inspect_container(
                &container,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .expect("the agent's container"),
    );
    let cli = |args: &'static [&'static str]| {
        let docker = docker.clone();
        let container = container.clone();
        let password = password.clone();
        async move { redis_cli(&docker, &container, &password, args).await }
    };

    // ── 1. Back up, both ways ────────────────────────────────────────────
    // Every request carries what the control plane sends: a credential
    // scoped to the job's own prefix (ADR-093), never the bucket's key.
    use temps_core::ScopedS3Access::{Read, Write};
    cli(&["SET", "proof", "before-backup"]).await;
    cli(&["SET", "_temps:redis_db_mapping:shop", "1"]).await;
    let walg_grant = redis_scoped(
        &in_network,
        &published,
        walg_key_prefix(WALG_BUCKET_PATH, &name),
        Write,
    )
    .await;
    let outcome: RedisWalgBackupResult = redis_route(
        &agent,
        |s, j| async move { redis_walg_backup(s, j).await },
        RedisWalgBackupRequest {
            service_id: 9,
            service_config: config.clone(),
            destination: walg_grant,
            bucket_path: WALG_BUCKET_PATH.to_string(),
            backup_uuid: format!("walg-{suffix}"),
        },
    )
    .await;
    assert_eq!(outcome, RedisWalgBackupResult::Completed);
    let repository = walg_prefix(REDIS_WALG_BUCKET, WALG_BUCKET_PATH, &name);
    let pushed = keys(
        &s3,
        REDIS_WALG_BUCKET,
        &walg_key_prefix(WALG_BUCKET_PATH, &name),
    )
    .await;
    assert!(
        pushed.iter().any(|k| k.contains("basebackups_005/")),
        "the stream backup is in the control plane's repository: {pushed:?}"
    );

    let rdb_key = format!(
        "{WALG_BUCKET_PATH}/external_services/redis/{name}/2026/11/08/rdb-{suffix}/dump.rdb.gz"
    );
    let rdb_grant = redis_scoped(
        &published,
        &published,
        temps_providers::worker_s3_scope::key_directory(&rdb_key),
        Write,
    )
    .await;
    let outcome: RedisRdbBackupResult = redis_route(
        &agent,
        |s, j| async move { redis_rdb_backup(s, j).await },
        RedisRdbBackupRequest {
            service_id: 9,
            service_config: config.clone(),
            destination: rdb_grant.clone(),
            key: rdb_key.clone(),
        },
    )
    .await;
    let RedisRdbBackupResult::Completed { size_bytes } = outcome else {
        panic!("the RDB backup failed: {outcome:?}");
    };
    assert!(size_bytes > 0);
    assert!(keys(&s3, REDIS_WALG_BUCKET, &rdb_key)
        .await
        .contains(&rdb_key));
    // A key outside the service's own dumps is refused before anything runs.
    let outcome: RedisRdbBackupResult = redis_route(
        &agent,
        |s, j| async move { redis_rdb_backup(s, j).await },
        RedisRdbBackupRequest {
            service_id: 9,
            service_config: config.clone(),
            destination: rdb_grant,
            key: "external_services/redis/another/x/dump.rdb.gz".to_string(),
        },
    )
    .await;
    assert!(
        matches!(outcome, RedisRdbBackupResult::Refused { .. }),
        "{outcome:?}"
    );

    // ── 2. Restore each in place ─────────────────────────────────────────
    let restore = |location: String, source, keep: bool| RedisRestoreRequest {
        service_id: 9,
        service_config: config.clone(),
        source,
        location,
        keep_target_bookkeeping: keep,
        operation_id: Some("restore-run-9".to_string()),
    };
    for (location, reached_at) in [
        (repository.clone(), in_network.clone()),
        (rdb_key.clone(), published.clone()),
    ] {
        let source =
            redis_scoped(&reached_at, &published, redis_read_prefix(&location), Read).await;
        cli(&["SET", "proof", "after-backup"]).await;
        cli(&["SET", "added-after", "x"]).await;
        let outcome: RedisRestoreResult = redis_route(
            &agent,
            |s, j| async move { redis_restore(s, j).await },
            restore(location.clone(), source, false),
        )
        .await;
        assert_eq!(outcome, RedisRestoreResult::Completed, "{location}");
        // The node kept the outcome, for a control plane whose request went
        // unanswered.
        assert_eq!(
            recorded_restore_outcome(
                temps_service_protocol::WorkerRestoreEngine::Redis,
                &config.name,
                "restore-run-9",
            )
            .await,
            temps_service_protocol::WorkerRestoreProgress::Finished {
                outcome: temps_service_protocol::WorkerRestoreResult::Completed
            },
            "{location}"
        );
        wait_redis(&docker, &container, &password).await;
        assert_eq!(cli(&["GET", "proof"]).await, "before-backup", "{location}");
        assert_eq!(cli(&["EXISTS", "added-after"]).await, "0", "{location}");
        let after = docker
            .inspect_container(
                &container,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .expect("the same container");
        assert_eq!(restart_policy(&after), policy_before, "{location}");
        assert_eq!(leftovers(&docker, &container).await, "", "{location}");
    }

    // ── 3. A restore that cannot fetch its backup changes nothing ────────
    cli(&["SET", "proof", "kept-on-failure"]).await;
    let missing = walg_prefix(REDIS_WALG_BUCKET, WALG_BUCKET_PATH, "no-such-service");
    let outcome: RedisRestoreResult = redis_route(
        &agent,
        |s, j| async move { redis_restore(s, j).await },
        restore(
            missing.clone(),
            redis_scoped(&in_network, &published, redis_read_prefix(&missing), Read).await,
            false,
        ),
    )
    .await;
    assert!(
        matches!(&outcome, RedisRestoreResult::Failed { reason } if reason.contains(&name)),
        "{outcome:?}"
    );
    assert!(!format!("{outcome:?}").contains(WALG_S3_KEY));
    wait_redis(&docker, &container, &password).await;
    assert!(is_running(&docker, &container).await);
    assert_eq!(cli(&["GET", "proof"]).await, "kept-on-failure");
    assert_eq!(leftovers(&docker, &container).await, "");

    // ── 4. Another service's backup keeps the target's bookkeeping ───────
    cli(&["SET", "_temps:redis_db_mapping:shop", "2"]).await;
    cli(&["SET", "_temps:redis_db_owner:2", "target"]).await;
    let outcome: RedisRestoreResult = redis_route(
        &agent,
        |s, j| async move { redis_restore(s, j).await },
        restore(
            rdb_key.clone(),
            redis_scoped(&published, &published, redis_read_prefix(&rdb_key), Read).await,
            true,
        ),
    )
    .await;
    assert_eq!(outcome, RedisRestoreResult::Completed);
    wait_redis(&docker, &container, &password).await;
    assert_eq!(cli(&["GET", "proof"]).await, "before-backup");
    assert_eq!(cli(&["GET", "_temps:redis_db_mapping:shop"]).await, "2");
    assert_eq!(cli(&["GET", "_temps:redis_db_owner:2"]).await, "target");
    // Its own backup brings its own bookkeeping back with the data.
    let outcome: RedisRestoreResult = redis_route(
        &agent,
        |s, j| async move { redis_restore(s, j).await },
        restore(
            rdb_key.clone(),
            redis_scoped(&published, &published, redis_read_prefix(&rdb_key), Read).await,
            false,
        ),
    )
    .await;
    assert_eq!(outcome, RedisRestoreResult::Completed);
    wait_redis(&docker, &container, &password).await;
    assert_eq!(cli(&["GET", "_temps:redis_db_mapping:shop"]).await, "1");
    assert_eq!(cli(&["EXISTS", "_temps:redis_db_owner:2"]).await, "0");
}

/// The same node half on the control plane (ADR-099: one implementation,
/// the transport alone differs): a Redis the provider created is backed up
/// with `redis_walg_backup`, as the backup engine calls it for a Redis on
/// this host, restored in place by the provider's `restore_from_s3`, as a
/// local restore calls it, and left as it was by a restore that fails, its
/// container keeping the provider's restart policy (`always`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_control_plane_redis_is_backed_up_and_restored_by_the_same_node_half() {
    use bollard::models::RestartPolicyNameEnum;
    use temps_providers::externalsvc::redis::RedisService;
    use temps_providers::externalsvc::redis_node::{
        redis_rdb_backup, redis_walg_backup, walg_prefix, RedisRdbBackup, RedisWalgBackup,
    };
    use temps_providers::externalsvc::{ExternalService, S3Credentials};

    let docker = match Docker::connect_with_defaults() {
        Ok(docker) if docker.ping().await.is_ok() => docker,
        _ => {
            println!("Docker not available (set DOCKER_HOST), skipping the local Redis proof");
            return;
        }
    };
    const IMAGE: &str = "gotempsh/redis-walg:8-bookworm";
    for image in [
        IMAGE,
        RUSTFS_IMAGE,
        temps_providers::externalsvc::redis_node::RDB_HELPER_IMAGE,
    ] {
        if docker.inspect_image(image).await.is_err() {
            println!("image {image} is not present locally, skipping the local Redis proof");
            return;
        }
    }
    let suffix = std::process::id();
    let name = format!("cplredis{suffix}");
    let container = format!("redis-{name}");
    let s3_name = format!("cplredis3{suffix}");
    let password = "local-redis-proof";
    let mut cleanup = Cleanup {
        docker: docker.clone(),
        containers: vec![container.clone(), s3_name.clone()],
        volumes: Vec::new(),
    };
    let shared = Arc::new(docker.clone());
    let config = ServiceConfig {
        name: name.clone(),
        service_type: ServiceType::Redis,
        version: None,
        parameters: json!({
            "host": "localhost",
            "port": free_port().to_string(),
            "password": password,
            "docker_image": IMAGE,
        }),
    };
    let redis = RedisService::new(name.clone(), shared.clone());
    let runtime = redis
        .init(config.clone())
        .await
        .expect("the provider creates the service");
    let config = ServiceConfig {
        parameters: serde_json::to_value(&runtime).expect("runtime parameters"),
        ..config
    };
    let created = docker
        .inspect_container(
            &container,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .expect("the provider's container");
    cleanup.volumes = created
        .mounts
        .iter()
        .flatten()
        .filter_map(|m| m.name.clone())
        .collect();
    let (s3, s3_port) = redis_bucket(&docker, &s3_name, temps_core::NETWORK_NAME.as_str()).await;
    let credentials = |endpoint: String| S3Credentials {
        access_key_id: WALG_S3_KEY.to_string(),
        secret_key: WALG_S3_KEY.to_string(),
        session_token: None,
        region: "us-east-1".to_string(),
        endpoint: Some(endpoint),
        bucket_name: REDIS_WALG_BUCKET.to_string(),
        bucket_path: WALG_BUCKET_PATH.to_string(),
        force_path_style: true,
    };
    let in_network = credentials(format!("http://{s3_name}:9000"));
    let published = credentials(format!("http://127.0.0.1:{s3_port}"));
    let s3_source = |c: &S3Credentials| temps_entities::s3_sources::Model {
        id: 1,
        name: "proof".to_string(),
        bucket_name: c.bucket_name.clone(),
        region: c.region.clone(),
        endpoint: c.endpoint.clone(),
        bucket_path: c.bucket_path.clone(),
        access_key_id: c.access_key_id.clone(),
        secret_key: c.secret_key.clone(),
        session_token: None,
        credentials_expire_at: None,
        force_path_style: Some(true),
        is_default: false,
        managed_by_cloud: false,
        lifecycle_reconcile_failed_at: None,
        lifecycle_reconcile_generation: 0,
        worker_credentials: temps_entities::s3_sources::WORKER_CREDENTIALS_SCOPED.to_string(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        backing_service_id: None,
    };
    wait_redis(&docker, &container, password).await;
    let cli = |args: &'static [&'static str]| {
        let docker = docker.clone();
        let container = container.clone();
        async move { redis_cli(&docker, &container, password, args).await }
    };

    cli(&["SET", "proof", "before-backup"]).await;
    let cancel = tokio_util::sync::CancellationToken::new();
    redis_walg_backup(
        &docker,
        RedisWalgBackup {
            service_id: 1,
            service: name.clone(),
            container_name: container.clone(),
            password: password.to_string(),
            s3: in_network.clone(),
            backup_uuid: format!("local-{suffix}"),
        },
        &cancel,
    )
    .await
    .expect("the WAL-G stream backup on this host");
    let rdb_key = format!(
        "{WALG_BUCKET_PATH}/external_services/redis/{name}/2026/11/08/local-{suffix}/dump.rdb.gz"
    );
    let size = redis_rdb_backup(
        shared.clone(),
        RedisRdbBackup {
            service_id: 1,
            service: name.clone(),
            container_name: container.clone(),
            password: password.to_string(),
            destination: temps_service_protocol::WorkerS3Destination {
                bucket: REDIS_WALG_BUCKET.to_string(),
                region: "us-east-1".to_string(),
                endpoint: published.endpoint.clone(),
                force_path_style: true,
                access_key_id: WALG_S3_KEY.to_string(),
                secret_access_key: WALG_S3_KEY.to_string(),
                session_token: None,
            },
            key: rdb_key.clone(),
        },
    )
    .await
    .expect("the RDB backup on this host");
    assert!(size > 0);

    let repository = walg_prefix(REDIS_WALG_BUCKET, WALG_BUCKET_PATH, &name);
    for (location, source) in [(repository, &in_network), (rdb_key, &published)] {
        cli(&["SET", "proof", "after-backup"]).await;
        let client = source.build_s3_client().await;
        redis
            .restore_from_s3(
                &client,
                source,
                &location,
                &s3_source(source),
                config.clone(),
            )
            .await
            .unwrap_or_else(|e| panic!("restore {location}: {e}"));
        wait_redis(&docker, &container, password).await;
        assert_eq!(cli(&["GET", "proof"]).await, "before-backup", "{location}");
        let after = docker
            .inspect_container(
                &container,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .expect("the same container");
        assert_eq!(restart_policy(&after), Some(RestartPolicyNameEnum::ALWAYS));
        assert_eq!(leftovers(&docker, &container).await, "");
    }

    cli(&["SET", "proof", "kept-on-failure"]).await;
    let missing = walg_prefix(REDIS_WALG_BUCKET, WALG_BUCKET_PATH, "no-such-service");
    let client = in_network.build_s3_client().await;
    let error = redis
        .restore_from_s3(
            &client,
            &in_network,
            &missing,
            &s3_source(&in_network),
            config,
        )
        .await
        .expect_err("nothing to fetch");
    assert!(!error.to_string().contains(WALG_S3_KEY), "{error}");
    wait_redis(&docker, &container, password).await;
    assert_eq!(cli(&["GET", "proof"]).await, "kept-on-failure");
    let after = docker
        .inspect_container(
            &container,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .expect("the same container");
    assert_eq!(restart_policy(&after), Some(RestartPolicyNameEnum::ALWAYS));
    assert_eq!(leftovers(&docker, &container).await, "");
    drop(s3);
}

// ── MariaDB (ADR-099): the control plane's engines, run by the agent ────────

/// `mariadb` as root in `container`, batch output without a header.
async fn mariadb_sql(docker: &Docker, container: &str, root: &str, sql: &str) -> (i64, String) {
    let password = format!("-p{root}");
    exec(
        docker,
        container,
        &[
            "mariadb",
            "-uroot",
            &password,
            "-h127.0.0.1",
            "-N",
            "-B",
            "-e",
            sql,
        ],
    )
    .await
}

/// The rows of the proof table, in order, comma-separated.
async fn mariadb_rows(docker: &Docker, container: &str, root: &str) -> String {
    let (code, rows) = mariadb_sql(
        docker,
        container,
        root,
        "SELECT IFNULL(GROUP_CONCAT(tag ORDER BY tag), '') FROM app.proof",
    )
    .await;
    assert_eq!(code, 0, "reading the proof rows: {rows}");
    rows
}

/// The server's clock, as a recovery target: binary-log timestamps are the
/// server's, at second precision.
async fn mariadb_now(
    docker: &Docker,
    container: &str,
    root: &str,
) -> chrono::DateTime<chrono::Utc> {
    let (code, now) = mariadb_sql(docker, container, root, "SELECT UTC_TIMESTAMP()").await;
    assert_eq!(code, 0, "reading the server's clock: {now}");
    chrono::NaiveDateTime::parse_from_str(now.trim(), "%Y-%m-%d %H:%M:%S")
        .unwrap_or_else(|e| panic!("the server's clock '{now}': {e}"))
        .and_utc()
}

/// The `data` of a MariaDB agent operation that ran (200). A node already
/// running its two data operations answers 429; the control plane waits for
/// a slot and asks again, and so does this, since the other proofs in this
/// process share the node's slots.
/// What the agent's outcome route reports for the `engine` restore of
/// `service` started as `operation_id`.
async fn recorded_restore_outcome(
    engine: temps_service_protocol::WorkerRestoreEngine,
    service: &str,
    operation_id: &str,
) -> temps_service_protocol::WorkerRestoreProgress {
    let response = temps_node_agent::service_data_handlers::worker_restore_outcome(Json(
        temps_service_protocol::WorkerRestoreOutcomeRequest {
            engine,
            service: service.to_string(),
            operation_id: operation_id.to_string(),
        },
    ))
    .await;
    let body = axum::body::to_bytes(response.into_body(), 1 << 16)
        .await
        .expect("the outcome body");
    let body: Value = serde_json::from_slice(&body).expect("a JSON outcome");
    serde_json::from_value(body["data"].clone()).expect("a restore progress")
}

async fn mariadb_outcome<T, F, Fut>(call: F, what: &str) -> T
where
    T: serde::de::DeserializeOwned,
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = axum::response::Response>,
{
    let (status, body) = admitted(call).await;
    assert_eq!(status, 200, "the MariaDB {what} did not run: {body}");
    serde_json::from_value(body["data"].clone())
        .unwrap_or_else(|e| panic!("a MariaDB {what} result: {e}: {body}"))
}

/// Whether the data directory still holds the original kept aside by a
/// restore.
async fn kept_original(docker: &Docker, container: &str) -> bool {
    exec(
        docker,
        container,
        &["test", "-e", "/var/lib/mysql/.temps-restore-original"],
    )
    .await
    .0 == 0
}

/// A worker's MariaDB created by the agent as a control-plane-profile
/// server asks (the managed WAL-G image by default), healthy, seeded, with
/// an S3 bucket on its network.
struct MariadbFixture {
    docker: Docker,
    agent: Arc<AgentState>,
    name: String,
    container: String,
    root: String,
    config: ServiceConfig,
    s3: aws_sdk_s3::Client,
    s3_port: u16,
    bucket: String,
    _cleanup: Cleanup,
}

impl MariadbFixture {
    async fn start(tag: &str, bucket: &str) -> Option<Self> {
        let docker = match Docker::connect_with_defaults() {
            Ok(docker) if docker.ping().await.is_ok() => docker,
            _ => {
                println!("Docker not available (set DOCKER_HOST), skipping the worker MariaDB {tag} proof");
                return None;
            }
        };
        let agent = agent_state(docker.clone());
        let suffix = std::process::id();
        let name = format!("{tag}{suffix}");
        let s3_name = format!("{tag}s3{suffix}");
        let mut parameters = control_plane_parameters(&Engine {
            label: "mariadb",
            service_type: ServiceType::Mariadb,
            strategy: "mariadb",
            parameters: json!({}),
            expected_env: &[],
        });
        parameters.insert("port".into(), json!(free_port().to_string()));
        let params = ExternalServiceManager::remote_create_params(
            8,
            &name,
            &ServiceType::Mariadb,
            &as_strings(&parameters),
            false,
        )
        .expect("the create request");
        for image in [params.image.as_str(), RUSTFS_IMAGE] {
            if docker.inspect_image(image).await.is_err() {
                println!(
                    "image {image} is not present locally, skipping the worker MariaDB {tag} proof"
                );
                return None;
            }
        }
        let network = params
            .network
            .clone()
            .expect("worker services join a network");
        let cleanup = Cleanup {
            docker: docker.clone(),
            containers: vec![params.name.clone(), s3_name.clone()],
            volumes: params.volumes.keys().cloned().collect(),
        };
        let s3_port = free_port();
        docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::default()
                        .name(&s3_name)
                        .build(),
                ),
                bollard::models::ContainerCreateBody {
                    image: Some(RUSTFS_IMAGE.to_string()),
                    env: Some(vec![
                        format!("RUSTFS_ACCESS_KEY={WALG_S3_KEY}"),
                        format!("RUSTFS_SECRET_KEY={WALG_S3_KEY}"),
                    ]),
                    host_config: Some(bollard::models::HostConfig {
                        network_mode: Some(network.clone()),
                        port_bindings: Some(HashMap::from([(
                            "9000/tcp".to_string(),
                            Some(vec![bollard::models::PortBinding {
                                host_ip: Some("127.0.0.1".to_string()),
                                host_port: Some(s3_port.to_string()),
                            }]),
                        )])),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .expect("the bucket container");
        let (status, created) = envelope(
            create_service(
                State(agent.clone()),
                Json(ServiceCreateRequest {
                    name: params.name.clone(),
                    service_type: params.service_type.clone(),
                    image: params.image.clone(),
                    environment: params.environment.clone(),
                    port_mappings: params
                        .port_mappings
                        .iter()
                        .map(|p| ServicePortMapping {
                            host_port: p.host_port,
                            container_port: p.container_port,
                        })
                        .collect(),
                    volumes: params.volumes.clone(),
                    network: params.network.clone(),
                    command: params.command.clone(),
                    resource_limits: None,
                }),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(status, 200, "create failed: {created}");
        let container = created["data"]["container_name"]
            .as_str()
            .expect("the created container's name")
            .to_string();
        let host_port = created["data"]["host_port"]
            .as_u64()
            .expect("the published host port");
        docker
            .start_container(
                &s3_name,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .expect("start the bucket");
        let mut stored = parameters.clone();
        stored.insert("port".into(), json!(host_port.to_string()));
        stored.insert("container_name".into(), json!(container.clone()));
        let root = stored["root_password"]
            .as_str()
            .expect("a generated root password")
            .to_string();
        let config = ServiceConfig {
            name: name.clone(),
            service_type: ServiceType::Mariadb,
            version: None,
            parameters: serde_json::to_value(&stored).expect("stored parameters"),
        };
        wait_healthy(&agent, &config, "mariadb").await;
        // The health probe answers before the entrypoint's init has
        // created the application's user; wait for the real server.
        let deadline = Instant::now() + Duration::from_secs(120);
        while mariadb_sql(&docker, &container, &root, "SELECT 1").await.0 != 0 {
            assert!(
                Instant::now() < deadline,
                "the worker MariaDB never answered as root"
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        let s3 = temps_providers::externalsvc::S3Credentials {
            access_key_id: WALG_S3_KEY.to_string(),
            secret_key: WALG_S3_KEY.to_string(),
            session_token: None,
            region: "us-east-1".to_string(),
            endpoint: Some(format!("http://127.0.0.1:{s3_port}")),
            bucket_name: bucket.to_string(),
            bucket_path: WALG_BUCKET_PATH.to_string(),
            force_path_style: true,
        }
        .build_s3_client()
        .await;
        let mut made = Err(String::new());
        for _ in 0..60 {
            made = s3
                .create_bucket()
                .bucket(bucket)
                .send()
                .await
                .map(|_| ())
                .map_err(|e| format!("{e:?}"));
            if made.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        made.expect("creating the bucket");
        Some(Self {
            docker,
            agent,
            name,
            container,
            root,
            config,
            s3,
            s3_port,
            bucket: bucket.to_string(),
            _cleanup: cleanup,
        })
    }

    /// The bucket as the control plane sends it: an endpoint the node
    /// reaches, which the engine resolves for the containers on its network.
    fn destination(&self) -> temps_service_protocol::WorkerS3Destination {
        temps_service_protocol::WorkerS3Destination {
            bucket: self.bucket.clone(),
            region: "us-east-1".to_string(),
            endpoint: Some(format!("http://127.0.0.1:{}", self.s3_port)),
            force_path_style: true,
            access_key_id: WALG_S3_KEY.to_string(),
            secret_access_key: WALG_S3_KEY.to_string(),
            session_token: None,
        }
    }

    /// What the control plane sends for one MariaDB job (ADR-093): a
    /// credential RustFS mints for `prefixes` alone.
    async fn scoped(
        &self,
        prefixes: &[String],
        access: temps_core::ScopedS3Access,
    ) -> temps_service_protocol::WorkerS3Destination {
        scoped_destination(&self.destination(), WALG_BUCKET_PATH, prefixes, access).await
    }

    /// The read grant of a restore of `location`, as the control plane
    /// derives it: the backup's own location (a WAL-G repository, or a
    /// dump's directory) and, for a point-in-time recovery, the binary logs
    /// the replay reads.
    async fn restore_source(
        &self,
        location: &str,
        pitr: bool,
    ) -> temps_service_protocol::WorkerS3Destination {
        use temps_providers::externalsvc::mariadb_node::binlog_read_prefix;
        use temps_providers::worker_s3_scope::{key_directory, s3_url_prefix};
        let mut prefixes = vec![match location.strip_prefix("s3://") {
            Some(_) => s3_url_prefix(location, &self.bucket).expect("a repository in the bucket"),
            None => key_directory(location),
        }];
        if pitr {
            prefixes.push(binlog_read_prefix(
                location,
                &self.bucket,
                WALG_BUCKET_PATH,
                &self.name,
            ));
        }
        self.scoped(&prefixes, temps_core::ScopedS3Access::Read)
            .await
    }
}

/// ADR-099 for MariaDB: a worker's MariaDB, created by the agent as a
/// control-plane-profile server asks, is backed up, shipped, restored and
/// recovered by the control plane's own MariaDB engines through the agent's
/// sealed operations.
///
/// 1. The agent's MariaDB runs with binary logging, as the control plane's.
/// 2. A WAL-G physical base lands under the control plane's repository and
///    reports its binary-log position.
/// 3. Rows written after it are shipped as binary logs by the archiver.
/// 4. A restore in place brings back the base exactly, keeps the restart
///    policy, deletes the kept original, and the service's own login still
///    reads its database (the base carries the `mysql` schema, grants
///    included).
/// 5. A point-in-time recovery replays up to the target and no further.
/// 6. A restore that fails before the swap (no such base) and one that fails
///    after it (an archived segment is gone) both leave the original data
///    running, unchanged.
/// 7. A logical dump is streamed to S3 and restored in place.
///
/// Every request carries what the control plane now sends (ADR-093): a
/// credential RustFS mints for that job's prefixes alone, write for the
/// base, the binary logs and the dump, read for a restore.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_worker_mariadb_is_backed_up_restored_and_recovered_by_the_control_plane_engines() {
    use temps_core::ScopedS3Access::Write;
    use temps_node_agent::service_mariadb_handlers as handlers;
    use temps_providers::externalsvc::mariadb_node::{
        binlog_key_prefix, service_root, walg_key_prefix, walg_prefix,
    };
    use temps_service_protocol::{
        MariadbBinlogArchiveRequest, MariadbBinlogArchiveResult, MariadbDumpBackupRequest,
        MariadbDumpBackupResult, MariadbPhysicalBackupRequest, MariadbPhysicalBackupResult,
        MariadbRestoreRequest, MariadbRestoreResult, RecoveryTarget,
    };

    let Some(fixture) = MariadbFixture::start("wsemaria", "maria-proof").await else {
        return;
    };
    let docker = &fixture.docker;
    let container = fixture.container.as_str();
    let root = fixture.root.as_str();
    let sql = |statement: &'static str| async move {
        let (code, out) = mariadb_sql(docker, container, root, statement).await;
        assert_eq!(code, 0, "{statement}: {out}");
        out
    };
    assert_eq!(container, format!("mariadb-{}", fixture.name));

    // 1. Binary logging, from creation.
    let created = docker
        .inspect_container(
            container,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .expect("the agent's container");
    let command = created
        .config
        .as_ref()
        .and_then(|c| c.cmd.clone())
        .unwrap_or_default();
    assert!(
        command.iter().any(|arg| arg == "--log-bin=mysql-bin"),
        "{command:?}"
    );
    assert!(
        created
            .config
            .as_ref()
            .and_then(|c| c.env.as_ref())
            .is_some_and(|env| env.iter().any(|e| e == "TZ=UTC")),
        "the server runs in UTC, as on the control plane"
    );
    let policy_before = restart_policy(&created);
    assert_eq!(sql("SELECT @@log_bin").await, "1");

    sql("CREATE TABLE app.proof (tag VARCHAR(32) PRIMARY KEY); INSERT INTO app.proof VALUES ('base')").await;

    // 2. The physical base.
    let request = MariadbPhysicalBackupRequest {
        service_id: 8,
        node_id: 2,
        service_config: fixture.config.clone(),
        destination: fixture
            .scoped(&[walg_key_prefix(WALG_BUCKET_PATH, &fixture.name)], Write)
            .await,
        bucket_path: WALG_BUCKET_PATH.to_string(),
        backup_uuid: "maria-base-1".to_string(),
    };
    let backup: MariadbPhysicalBackupResult = mariadb_outcome(
        || handlers::mariadb_physical_backup(State(fixture.agent.clone()), Json(request.clone())),
        "physical backup",
    )
    .await;
    let MariadbPhysicalBackupResult::Completed { binlog } = backup else {
        panic!("the physical backup did not complete: {backup:?}");
    };
    let binlog = binlog.expect("binary logging is on, so the base has a position");
    assert!(binlog.file.starts_with("mysql-bin."), "{binlog:?}");
    let repository = walg_prefix(&fixture.bucket, WALG_BUCKET_PATH, &fixture.name);
    let base_keys = keys(
        &fixture.s3,
        &fixture.bucket,
        &walg_key_prefix(WALG_BUCKET_PATH, &fixture.name),
    )
    .await;
    assert!(
        base_keys.iter().any(|k| k.contains("basebackups_005/")),
        "the base is in the control plane's repository: {base_keys:?}"
    );

    // 3. Writes after the base, around a recovery point, shipped.
    sql("INSERT INTO app.proof VALUES ('before-target')").await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let target_time = mariadb_now(docker, container, root).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    sql("INSERT INTO app.proof VALUES ('after-target')").await;
    let request = MariadbBinlogArchiveRequest {
        service_id: 8,
        node_id: 2,
        service_config: fixture.config.clone(),
        destination: fixture
            .scoped(&[binlog_key_prefix(WALG_BUCKET_PATH, &fixture.name)], Write)
            .await,
        bucket_path: WALG_BUCKET_PATH.to_string(),
    };
    let archived: MariadbBinlogArchiveResult = mariadb_outcome(
        || handlers::mariadb_binlog_archive(State(fixture.agent.clone()), Json(request.clone())),
        "binlog archive",
    )
    .await;
    assert!(
        matches!(archived, MariadbBinlogArchiveResult::Completed { shipped } if shipped >= 1),
        "{archived:?}"
    );
    let binlog_prefix = format!("{}/binlog/", service_root(WALG_BUCKET_PATH, &fixture.name));
    let shipped = keys(&fixture.s3, &fixture.bucket, &binlog_prefix).await;
    assert!(
        shipped.iter().any(|k| k.ends_with("manifest.json")),
        "{shipped:?}"
    );

    let restore = |location: &str, recovery_target: Option<RecoveryTarget>| {
        let (fixture, location) = (&fixture, location.to_string());
        async move {
            MariadbRestoreRequest {
                service_id: 8,
                node_id: 2,
                service_config: fixture.config.clone(),
                source: fixture
                    .restore_source(&location, recovery_target.is_some())
                    .await,
                bucket_path: WALG_BUCKET_PATH.to_string(),
                location,
                recovery_target,
                target_user_data: None,
                operation_id: Some("restore-run-8".to_string()),
            }
        }
    };
    let run_restore = |request: MariadbRestoreRequest| {
        let agent = fixture.agent.clone();
        async move {
            mariadb_outcome::<MariadbRestoreResult, _, _>(
                || handlers::mariadb_restore(State(agent.clone()), Json(request.clone())),
                "restore",
            )
            .await
        }
    };

    // 4. In place: the base, exactly.
    assert_eq!(
        run_restore(restore(&repository, None).await).await,
        MariadbRestoreResult::Completed
    );
    // The node kept the outcome, for a control plane whose request went
    // unanswered.
    assert_eq!(
        recorded_restore_outcome(
            temps_service_protocol::WorkerRestoreEngine::Mariadb,
            &fixture.config.name,
            "restore-run-8",
        )
        .await,
        temps_service_protocol::WorkerRestoreProgress::Finished {
            outcome: temps_service_protocol::WorkerRestoreResult::Completed
        }
    );
    assert_eq!(mariadb_rows(docker, container, root).await, "base");
    assert!(
        !kept_original(docker, container).await,
        "the kept original is deleted"
    );
    let restored = docker
        .inspect_container(
            container,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .expect("the restored container");
    assert_eq!(
        restart_policy(&restored),
        policy_before,
        "the restart policy is kept"
    );
    assert_eq!(
        restored.id, created.id,
        "the agent's container is restored, not replaced"
    );
    let login = |user: &str, password: &str| {
        let (user, password) = (format!("-u{user}"), format!("-p{password}"));
        async move {
            exec(
                docker,
                container,
                &[
                    "mariadb",
                    &user,
                    &password,
                    "-h127.0.0.1",
                    "-N",
                    "-B",
                    "-e",
                    "SELECT COUNT(*) FROM app.proof",
                ],
            )
            .await
        }
    };
    let parameters = &fixture.config.parameters;
    let (code, out) = login(
        parameters["username"].as_str().expect("username"),
        parameters["password"].as_str().expect("password"),
    )
    .await;
    assert_eq!(
        (code, out.as_str()),
        (0, "1"),
        "the service's own login reads its database after the restore (grants restored)"
    );

    // 5. To a point in time: up to the target, not beyond.
    assert_eq!(
        run_restore(
            restore(
                &repository,
                Some(RecoveryTarget::Time { time: target_time })
            )
            .await
        )
        .await,
        MariadbRestoreResult::Completed
    );
    assert_eq!(
        mariadb_rows(docker, container, root).await,
        "base,before-target"
    );
    assert!(!kept_original(docker, container).await);

    // 6a. Fails before the swap: no base under that repository.
    sql("INSERT INTO app.proof VALUES ('live')").await;
    let missing = walg_prefix(&fixture.bucket, WALG_BUCKET_PATH, "no-such-service");
    let failed = run_restore(restore(&missing, None).await).await;
    assert!(
        matches!(&failed, MariadbRestoreResult::Failed { reason } if reason.contains("left in place")),
        "{failed:?}"
    );
    assert!(
        is_running(docker, container).await,
        "the original is running again"
    );
    assert_eq!(
        mariadb_rows(docker, container, root).await,
        "base,before-target,live"
    );

    // 6b. Fails after the swap: the archived segments the replay needs are
    // gone, though the manifest still lists them.
    let segments: Vec<&String> = shipped
        .iter()
        .filter(|k| !k.ends_with("manifest.json"))
        .collect();
    assert!(!segments.is_empty(), "a shipped segment: {shipped:?}");
    for segment in segments {
        fixture
            .s3
            .delete_object()
            .bucket(&fixture.bucket)
            .key(segment)
            .send()
            .await
            .expect("delete a segment");
    }
    let failed = run_restore(
        restore(
            &repository,
            Some(RecoveryTarget::Time { time: target_time }),
        )
        .await,
    )
    .await;
    assert!(
        matches!(&failed, MariadbRestoreResult::Failed { reason } if reason.contains("put back")),
        "{failed:?}"
    );
    assert!(
        is_running(docker, container).await,
        "the original is running again"
    );
    assert_eq!(
        mariadb_rows(docker, container, root).await,
        "base,before-target,live",
        "the original data, unchanged"
    );
    assert!(
        !kept_original(docker, container).await,
        "the original is back in place"
    );
    let after = docker
        .inspect_container(
            container,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .expect("the container");
    assert_eq!(restart_policy(&after), policy_before);

    // 7. A logical dump, streamed to S3 and applied in place.
    let dump_key = format!(
        "{}/2026/11/08/maria-dump-1/dump.sql.gz",
        service_root(WALG_BUCKET_PATH, &fixture.name)
    );
    let request = MariadbDumpBackupRequest {
        service_id: 8,
        node_id: 2,
        service_config: fixture.config.clone(),
        destination: fixture
            .scoped(
                &[temps_providers::worker_s3_scope::key_directory(&dump_key)],
                Write,
            )
            .await,
        bucket_path: WALG_BUCKET_PATH.to_string(),
        key: dump_key.clone(),
    };
    let dumped: MariadbDumpBackupResult = mariadb_outcome(
        || handlers::mariadb_dump_backup(State(fixture.agent.clone()), Json(request.clone())),
        "dump",
    )
    .await;
    assert!(
        matches!(dumped, MariadbDumpBackupResult::Completed { size_bytes } if size_bytes > 0),
        "{dumped:?}"
    );
    sql("DELETE FROM app.proof WHERE tag = 'live'").await;
    assert_eq!(
        run_restore(restore(&dump_key, None).await).await,
        MariadbRestoreResult::Completed
    );
    assert_eq!(
        mariadb_rows(docker, container, root).await,
        "base,before-target,live"
    );
    // A dump cannot anchor a point-in-time recovery, refused before
    // anything is touched.
    let refused =
        run_restore(restore(&dump_key, Some(RecoveryTarget::Time { time: target_time })).await)
            .await;
    assert!(
        matches!(refused, MariadbRestoreResult::Refused { .. }),
        "{refused:?}"
    );
}

/// The same engines on the control plane: a MariaDB the provider creates on
/// this host is backed up by the very function the agent ran above, and
/// restored and recovered by the very restore, with this host's Docker.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_control_plane_mariadb_is_backed_up_and_restored_by_the_same_engines() {
    use temps_providers::externalsvc::mariadb::{
        MariaDbInPlaceRestore, MariaDbService, MARIADB_DEFAULT_IMAGE,
    };
    use temps_providers::externalsvc::mariadb_node::{
        physical_backup, walg_prefix, MariadbPhysicalBackup,
    };
    use temps_providers::externalsvc::{ExternalService, RecoveryTarget, S3Credentials};

    let docker = match Docker::connect_with_defaults() {
        Ok(docker) if docker.ping().await.is_ok() => docker,
        _ => {
            println!("Docker not available (set DOCKER_HOST), skipping the local MariaDB proof");
            return;
        }
    };
    for image in [MARIADB_DEFAULT_IMAGE, RUSTFS_IMAGE] {
        if docker.inspect_image(image).await.is_err() {
            println!("image {image} is not present locally, skipping the local MariaDB proof");
            return;
        }
    }
    let suffix = std::process::id();
    let name = format!("cplmaria{suffix}");
    let container = format!("mariadb-{name}");
    let s3_name = format!("cplmarias3{suffix}");
    let bucket = "maria-local-proof";
    let mut cleanup = Cleanup {
        docker: docker.clone(),
        containers: vec![container.clone(), s3_name.clone()],
        volumes: Vec::new(),
    };
    let shared = Arc::new(docker.clone());
    let root = "local-root-password";
    let config = ServiceConfig {
        name: name.clone(),
        service_type: ServiceType::Mariadb,
        version: None,
        parameters: json!({
            "host": "localhost",
            "port": free_port().to_string(),
            "database": "app",
            "username": "app",
            "password": "local-app-password",
            "root_password": root,
            "docker_image": MARIADB_DEFAULT_IMAGE,
        }),
    };
    let mariadb = MariaDbService::new(name.clone(), shared.clone());
    let runtime = mariadb
        .init(config.clone())
        .await
        .expect("the provider creates the service");
    let created = docker
        .inspect_container(
            &container,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .expect("the provider's container");
    cleanup.volumes = created
        .mounts
        .iter()
        .flatten()
        .filter_map(|m| m.name.clone())
        .collect();
    let config = ServiceConfig {
        parameters: serde_json::to_value(&runtime).expect("runtime parameters"),
        ..config
    };
    let deadline = Instant::now() + Duration::from_secs(120);
    while mariadb_sql(&docker, &container, root, "SELECT 1").await.0 != 0 {
        assert!(
            Instant::now() < deadline,
            "the local MariaDB never answered"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let s3_port = free_port();
    docker
        .create_container(
            Some(
                bollard::query_parameters::CreateContainerOptionsBuilder::default()
                    .name(&s3_name)
                    .build(),
            ),
            bollard::models::ContainerCreateBody {
                image: Some(RUSTFS_IMAGE.to_string()),
                env: Some(vec![
                    format!("RUSTFS_ACCESS_KEY={WALG_S3_KEY}"),
                    format!("RUSTFS_SECRET_KEY={WALG_S3_KEY}"),
                ]),
                host_config: Some(bollard::models::HostConfig {
                    network_mode: Some(temps_core::NETWORK_NAME.to_string()),
                    port_bindings: Some(HashMap::from([(
                        "9000/tcp".to_string(),
                        Some(vec![bollard::models::PortBinding {
                            host_ip: Some("127.0.0.1".to_string()),
                            host_port: Some(s3_port.to_string()),
                        }]),
                    )])),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .expect("the bucket container");
    docker
        .start_container(
            &s3_name,
            None::<bollard::query_parameters::StartContainerOptions>,
        )
        .await
        .expect("start the bucket");
    let credentials = S3Credentials {
        access_key_id: WALG_S3_KEY.to_string(),
        secret_key: WALG_S3_KEY.to_string(),
        session_token: None,
        region: "us-east-1".to_string(),
        endpoint: Some(format!("http://127.0.0.1:{s3_port}")),
        bucket_name: bucket.to_string(),
        bucket_path: WALG_BUCKET_PATH.to_string(),
        force_path_style: true,
    };
    let s3 = credentials.build_s3_client().await;
    let mut made = Err(String::new());
    for _ in 0..60 {
        made = s3
            .create_bucket()
            .bucket(bucket)
            .send()
            .await
            .map(|_| ())
            .map_err(|e| format!("{e:?}"));
        if made.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    made.expect("creating the bucket");

    let sql = |statement: &'static str| {
        let docker = docker.clone();
        let container = container.clone();
        async move {
            let (code, out) = mariadb_sql(&docker, &container, root, statement).await;
            assert_eq!(code, 0, "{statement}: {out}");
            out
        }
    };
    sql("CREATE TABLE app.proof (tag VARCHAR(32) PRIMARY KEY); INSERT INTO app.proof VALUES ('base')").await;
    let cancel = tokio_util::sync::CancellationToken::new();
    let binlog = physical_backup(
        &docker,
        &MariadbPhysicalBackup {
            service_id: 9,
            container_name: container.clone(),
            root_password: root.to_string(),
            s3: credentials.clone(),
            backup_uuid: "local-base-1".to_string(),
        },
        &name,
        &cancel,
    )
    .await
    .expect("the local physical backup")
    .expect("a binary-log position");
    assert!(binlog.file.starts_with("mysql-bin."), "{binlog:?}");
    sql("INSERT INTO app.proof VALUES ('before-target')").await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let target_time = mariadb_now(&docker, &container, root).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    sql("INSERT INTO app.proof VALUES ('after-target')").await;

    let mariadb_config = MariaDbService::parse_config(&name, &config).expect("the config");
    let source = temps_entities::s3_sources::Model {
        id: 1,
        name: "local".to_string(),
        bucket_name: bucket.to_string(),
        region: "us-east-1".to_string(),
        endpoint: credentials.endpoint.clone(),
        bucket_path: WALG_BUCKET_PATH.to_string(),
        access_key_id: WALG_S3_KEY.to_string(),
        secret_key: WALG_S3_KEY.to_string(),
        session_token: None,
        credentials_expire_at: None,
        force_path_style: Some(true),
        is_default: false,
        managed_by_cloud: false,
        lifecycle_reconcile_failed_at: None,
        lifecycle_reconcile_generation: 0,
        worker_credentials: temps_entities::s3_sources::WORKER_CREDENTIALS_SCOPED.to_string(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        backing_service_id: None,
    };
    let shipped = mariadb
        .archive_binlogs(&s3, &source, &mariadb_config)
        .await
        .expect("the local archiver");
    assert!(shipped >= 1);

    let repository = walg_prefix(bucket, WALG_BUCKET_PATH, &name);
    fn restore<'a>(
        s3_client: &'a aws_sdk_s3::Client,
        s3_credentials: &'a S3Credentials,
        location: &'a str,
        recovery_target: Option<&'a RecoveryTarget>,
    ) -> MariaDbInPlaceRestore<'a> {
        MariaDbInPlaceRestore {
            s3_client,
            s3_credentials,
            location,
            target_user_data: None,
            recovery_target,
            gate: &temps_providers::externalsvc::NoopRestoreGate,
        }
    }
    mariadb
        .restore_in_place_on_this_node(
            &mariadb_config,
            restore(&s3, &credentials, &repository, None),
        )
        .await
        .expect("the local in-place restore");
    assert_eq!(mariadb_rows(&docker, &container, root).await, "base");
    assert!(!kept_original(&docker, &container).await);
    let target = RecoveryTarget::Time { time: target_time };
    mariadb
        .restore_in_place_on_this_node(
            &mariadb_config,
            restore(&s3, &credentials, &repository, Some(&target)),
        )
        .await
        .expect("the local point-in-time recovery");
    assert_eq!(
        mariadb_rows(&docker, &container, root).await,
        "base,before-target"
    );
    let restored = docker
        .inspect_container(
            &container,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .expect("the restored container");
    assert_eq!(
        restart_policy(&restored),
        restart_policy(&created),
        "the provider's restart policy is kept"
    );

    // A failed restore leaves the original running, unchanged.
    let missing = walg_prefix(bucket, WALG_BUCKET_PATH, "no-such-service");
    let error = mariadb
        .restore_in_place_on_this_node(&mariadb_config, restore(&s3, &credentials, &missing, None))
        .await
        .expect_err("no such base");
    assert!(error.to_string().contains("left in place"), "{error}");
    assert!(is_running(&docker, &container).await);
    assert_eq!(
        mariadb_rows(&docker, &container, root).await,
        "base,before-target"
    );
}
