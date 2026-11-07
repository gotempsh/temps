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

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .expect("a free local port")
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
    let (status, body) = envelope(
        temps_node_agent::service_data_handlers::postgres_walg_backup(
            State(agent.clone()),
            Json(request.clone()),
        )
        .await
        .into_response(),
    )
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
    let (status, body) = envelope(
        temps_node_agent::service_data_handlers::postgres_walg_restore(
            State(agent.clone()),
            Json(request.clone()),
        )
        .await
        .into_response(),
    )
    .await;
    assert_eq!(status, 200, "the WAL-G restore did not run: {body}");
    serde_json::from_value(body["data"].clone()).expect("a WAL-G restore result")
}

const RUSTFS_IMAGE: &str = "rustfs/rustfs:1.0.0";
const WALG_S3_KEY: &str = "walgproofkey";
const WALG_BUCKET_PATH: &str = "fleet";

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

/// ADR-092, PostgreSQL step 1: a worker's WAL-G PostgreSQL, created by the
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

/// ADR-092, PostgreSQL step 2: a worker's archiving is turned on, then
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
        let (status, body) = envelope(
            temps_node_agent::service_data_handlers::postgres_walg_archive(
                State(fixture.agent.clone()),
                Json(request),
            )
            .await
            .into_response(),
        )
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

/// ADR-092, PostgreSQL step 3: a worker's WAL-G PostgreSQL is restored in
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

/// ADR-092, PostgreSQL step 4: another service's WAL-G backup is restored
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

/// The same WAL-G restore on the control plane (ADR-092: one implementation,
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
    let (status, body) = envelope(
        data_backup(
            State(agent.clone()),
            Json(RemoteServiceBackupRequest {
                service_config: config.clone(),
                destination: destination.clone(),
                key: prefix.to_string(),
            }),
        )
        .await
        .into_response(),
    )
    .await;
    assert_eq!(status, 200, "the backup failed: {body}");

    // After the backup: 5 new objects and one changed.
    for i in 40..45 {
        put(format!("docs/{i:02}.txt"), "after the backup").await;
    }
    put("docs/00.txt".to_string(), "changed after the backup").await;
    assert_eq!(objects().await.len(), 45);

    let (status, body) = envelope(
        data_restore(
            State(agent.clone()),
            Json(RemoteServiceRestoreRequest {
                service_config: config.clone(),
                source: destination,
                location: prefix.to_string(),
            }),
        )
        .await
        .into_response(),
    )
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
