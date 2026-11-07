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

/// ADR-091, PostgreSQL step 1: a worker's WAL-G PostgreSQL, created by the
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

    let docker = match Docker::connect_with_defaults() {
        Ok(docker) if docker.ping().await.is_ok() => docker,
        _ => {
            println!("Docker not available (set DOCKER_HOST), skipping the worker WAL-G proof");
            return;
        }
    };
    const RUSTFS_IMAGE: &str = "rustfs/rustfs:1.0.0";
    const S3_KEY: &str = "walgproofkey";
    const BUCKET: &str = "walg-proof";
    const BUCKET_PATH: &str = "fleet";
    let agent = agent_state(docker.clone());
    let suffix = std::process::id();
    let name = format!("wsewalg{suffix}");
    let s3_name = format!("wsewalgs3{suffix}");

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
            println!("image {image} is not present locally, skipping the worker WAL-G proof");
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
                    format!("RUSTFS_ACCESS_KEY={S3_KEY}"),
                    format!("RUSTFS_SECRET_KEY={S3_KEY}"),
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

    let s3 = temps_providers::externalsvc::S3Credentials {
        access_key_id: S3_KEY.to_string(),
        secret_key: S3_KEY.to_string(),
        session_token: None,
        region: "us-east-1".to_string(),
        endpoint: Some(format!("http://127.0.0.1:{s3_port}")),
        bucket_name: BUCKET.to_string(),
        bucket_path: BUCKET_PATH.to_string(),
        force_path_style: true,
    }
    .build_s3_client()
    .await;
    let mut bucket = Err(String::new());
    for _ in 0..60 {
        bucket = s3
            .create_bucket()
            .bucket(BUCKET)
            .send()
            .await
            .map(|_| ())
            .map_err(|e| format!("{e:?}"));
        if bucket.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    bucket.expect("creating the bucket");

    let request = |allow_restart: bool, backup_uuid: &str| PostgresWalgBackupRequest {
        service_id: 8,
        node_id: 2,
        service_config: config.clone(),
        destination: WorkerS3Destination {
            bucket: BUCKET.to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some(format!("http://{s3_name}:9000")),
            force_path_style: true,
            access_key_id: S3_KEY.to_string(),
            secret_access_key: S3_KEY.to_string(),
            session_token: None,
        },
        bucket_path: BUCKET_PATH.to_string(),
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
    let key_prefix = walg_key_prefix(BUCKET_PATH, &name);
    assert_eq!(
        walg_prefix(BUCKET, BUCKET_PATH, &name),
        format!("s3://{BUCKET}/{}", key_prefix.trim_end_matches('/'))
    );
    let sentinels = |keys: &[String]| {
        keys.iter()
            .filter(|k| k.starts_with(&format!("{key_prefix}basebackups_005/")))
            .filter(|k| k.ends_with("_backup_stop_sentinel.json"))
            .count()
    };
    assert_eq!(sentinels(&keys(&s3, BUCKET, &key_prefix).await), 1);

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
        let all = keys(&s3, BUCKET, &key_prefix).await;
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
