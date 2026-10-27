// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-055 Phase 4: managed databases on the workload plane, across both
//! creation paths (the control-plane engine and the worker agent).
//!
//! - `enforce_mode_sends_the_same_create_request_from_both_paths` runs
//!   against the fake Engine API: the control-plane engine and the agent
//!   build the same container-create request from one admitted spec.
//! - `shadow_pass_reports_the_drift_of_each_creation_path` is the phase's
//!   proof. It seeds real containers through the legacy control-plane and
//!   worker paths and through the worker's enforce path, runs the shadow
//!   pass, and checks it computes a spec for each and reports exactly the
//!   expected differences. It needs Docker and skips with a printed reason
//!   without it.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::Json;
use bollard::models::ContainerCreateBody;
use bollard::Docker;
use serde_json::{json, Value};
use temps_agent::handlers::AgentState;
use temps_agent::service_handlers::{create_service, create_service_workload, service_snapshot};
use temps_agent::{ServiceCreateRequest, ServicePortMapping};
use temps_docker_capture::{golden_value, FakeDocker, Method};
use temps_entities::external_service_workload_mode::ServiceWorkloadMode;
use temps_entities::external_services;
use temps_providers::externalsvc::redis::RedisService;
use temps_providers::externalsvc::{ExternalService, ServiceConfig, ServiceType};
use temps_providers::remote_service_client::{
    RemotePortMapping, RemoteServiceCreateParams, ServiceWorkloadCreateRequest,
};
use temps_providers::workload::registry::{self, EnforcedDatabase};
use temps_providers::workload::runtime::{self, ContainerSnapshot};
use temps_providers::workload::shadow::{shadow_pass, SnapshotSource};
use temps_providers::workload::{
    DatabaseEngine, DatabaseIdentity, DatabaseProcess, DatabaseWorkloadPlan, ServiceWorkloadError,
    ShapeField,
};

const REDIS_IMAGE: &str = "gotempsh/redis-walg:8-bookworm";

fn agent_state(docker: Docker, host_ip: &str) -> Arc<AgentState> {
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
        image_builder: remote,
        docker: Some(docker),
        overlay_bridge_address: Default::default(),
        overlay_peers: Default::default(),
        platform: Default::default(),
        host_bind_address: Arc::new(std::sync::RwLock::new(host_ip.into())),
    })
}

fn redis_config(name: &str, port: u16, password: &str) -> ServiceConfig {
    ServiceConfig {
        name: name.to_string(),
        service_type: ServiceType::Redis,
        version: None,
        parameters: json!({ "port": port.to_string(), "password": password }),
    }
}

async fn fake() -> FakeDocker {
    let fake = FakeDocker::start()
        .await
        .expect("start the fake Engine API");
    fake.respond(Method::GET, "/images/", 200, json!({}));
    fake.respond(
        Method::GET,
        "/networks",
        200,
        json!([{ "Name": temps_core::NETWORK_NAME.as_str() }]),
    );
    fake
}

fn bodies(fake: &FakeDocker) -> Vec<Value> {
    fake.creates()
        .iter()
        .map(|c| golden_value(c)["body"].clone())
        .collect()
}

/// The parts of a request that are the database's policy, not its process
/// or its node.
fn policy(body: &Value) -> Value {
    let host = &body["HostConfig"];
    json!({
        "Healthcheck": body["Healthcheck"],
        "RestartPolicy": host["RestartPolicy"],
        "PidsLimit": host["PidsLimit"],
        "SecurityOpt": host["SecurityOpt"],
        "CapDrop": host["CapDrop"],
        "Init": host["Init"],
        "LogConfig": host["LogConfig"],
        "LabelKeys": body["Labels"].as_object().map(|l| l.keys().cloned().collect::<BTreeSet<_>>()),
    })
}

#[tokio::test]
async fn enforce_mode_sends_the_same_create_request_from_both_paths() {
    let service_name = "p4enforce";
    let container = DatabaseEngine::Redis.container_name(service_name);

    // 1. The control-plane engine's legacy request: the process both
    //    paths are handed.
    let legacy = fake().await;
    let engine = RedisService::new(service_name.to_string(), Arc::new(legacy.docker()));
    assert!(engine
        .init(redis_config(service_name, 16391, "seeded-enforce-1"))
        .await
        .is_err());
    let legacy_body = bodies(&legacy).pop().expect("one legacy create");
    assert!(
        legacy_body["Labels"].get("sh.temps.kind").is_none(),
        "legacy mode sends the engine's own request"
    );
    let legacy_request: ContainerCreateBody =
        serde_json::from_value(legacy_body).expect("the request parses as a create body");

    // 2. The same engine with the database in `enforce`.
    let database = EnforcedDatabase {
        service_id: 41,
        service_name: service_name.to_string(),
        engine: DatabaseEngine::Redis,
    };
    registry::set_enforced(&container, database.clone());
    let enforced = fake().await;
    let engine = RedisService::new(service_name.to_string(), Arc::new(enforced.docker()));
    assert!(engine
        .init(redis_config(service_name, 16391, "seeded-enforce-1"))
        .await
        .is_err());
    registry::clear_service(41);
    let control_plane = bodies(&enforced);
    assert_eq!(control_plane.len(), 1, "{:?}", enforced.calls());
    let control_plane = &control_plane[0];

    // 3. The worker agent, handed the plan of the same database.
    let (plan, process, _) =
        registry::enforced_plan(&database, &container, &legacy_request).expect("admitted");
    let worker = fake().await;
    let response = create_service_workload(
        State(agent_state(worker.docker(), "127.0.0.1")),
        Json(ServiceWorkloadCreateRequest {
            service_name: service_name.to_string(),
            plan,
            secrets: process.secret_values(),
            network: Some(temps_core::NETWORK_NAME.to_string()),
            replace_existing: false,
        }),
    )
    .await
    .into_response();
    assert!(
        response.status().is_server_error(),
        "the fake fails the create"
    );
    let from_worker = bodies(&worker);
    assert_eq!(from_worker.len(), 1, "{:?}", worker.calls());

    // Byte for byte, once canonicalised: one request, whichever path.
    assert_eq!(control_plane, &from_worker[0]);
    let host = &control_plane["HostConfig"];
    assert_eq!(host["PidsLimit"], 512);
    assert_eq!(host["SecurityOpt"], json!(["no-new-privileges:true"]));
    assert_eq!(host["RestartPolicy"]["Name"], "always");
    assert_eq!(control_plane["Labels"]["temps.service_type"], "redis");
    assert_eq!(control_plane["Labels"]["sh.temps.service.type"], "redis");
    assert_eq!(
        control_plane["Healthcheck"]["Test"],
        json!(["CMD-SHELL", "redis-cli ping"])
    );

    // A worker database created from the agent's own parameters (another
    // process: no AOF, another volume) still gets the same policy.
    let params = RemoteServiceCreateParams {
        name: container.clone(),
        service_type: "redis".into(),
        image: REDIS_IMAGE.into(),
        environment: HashMap::new(),
        port_mappings: vec![RemotePortMapping {
            host_port: 16392,
            container_port: 6379,
        }],
        volumes: HashMap::from([(format!("{container}_data"), "/data".into())]),
        network: Some(temps_core::NETWORK_NAME.to_string()),
        command: Some(vec![
            "redis-server".into(),
            "--requirepass".into(),
            "seeded-enforce-2".into(),
        ]),
        resource_limits: None,
    };
    let identity = DatabaseIdentity {
        service_id: 41,
        service_name: service_name.to_string(),
        engine: DatabaseEngine::Redis,
        node_id: Some(3),
        container_name: container.clone(),
    };
    let process = DatabaseProcess::from_remote_params(DatabaseEngine::Redis, &params);
    let plan = DatabaseWorkloadPlan::admit(&identity, &process).expect("admitted");
    let worker = fake().await;
    let _ = create_service_workload(
        State(agent_state(worker.docker(), "10.100.0.2")),
        Json(ServiceWorkloadCreateRequest {
            service_name: service_name.to_string(),
            plan,
            secrets: process.secret_values(),
            network: params.network.clone(),
            replace_existing: false,
        }),
    )
    .await;
    let remote = bodies(&worker).pop().expect("one create");
    assert_eq!(policy(control_plane), policy(&remote));
    assert_eq!(
        remote["HostConfig"]["PortBindings"]["6379/tcp"][0]["HostIp"], "10.100.0.2",
        "only where it attaches is the node's"
    );
}

/// Removes what the proof created, even when an assertion fails.
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

/// Containers without a node are read from Docker; worker containers
/// through the agent's snapshot route, decoded as the control plane does.
struct Snapshots {
    docker: Docker,
    agent: Arc<AgentState>,
}

#[async_trait::async_trait]
impl SnapshotSource for Snapshots {
    async fn snapshot(
        &self,
        identity: &DatabaseIdentity,
    ) -> Result<ContainerSnapshot, ServiceWorkloadError> {
        if identity.node_id.is_none() {
            return runtime::snapshot(&self.docker, &identity.container_name).await;
        }
        let response = service_snapshot(
            State(self.agent.clone()),
            Path(identity.container_name.clone()),
        )
        .await
        .into_response();
        let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .map_err(|e| ServiceWorkloadError::Agent {
                node: "test".into(),
                operation: "inspect",
                container: identity.container_name.clone(),
                reason: e.to_string(),
            })?;
        let envelope: Value = serde_json::from_slice(&bytes).expect("an agent response");
        assert_eq!(envelope["success"], true, "{envelope}");
        Ok(serde_json::from_value(envelope["data"].clone()).expect("a snapshot"))
    }
}

fn row(id: i32, name: &str, node_id: Option<i32>) -> external_services::Model {
    external_services::Model {
        id,
        name: name.to_string(),
        service_type: "redis".to_string(),
        version: Some("8".to_string()),
        status: "running".to_string(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        slug: None,
        config: None,
        node_id,
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
        workload_mode: ServiceWorkloadMode::Shadow,
        log_ingest_lines_per_sec: None,
        log_ingest_kib_per_sec: None,
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .unwrap_or(16399)
}

#[tokio::test]
async fn shadow_pass_reports_the_drift_of_each_creation_path() {
    let docker = match Docker::connect_with_defaults() {
        Ok(docker) if docker.ping().await.is_ok() => docker,
        _ => {
            println!("Docker not available (set DOCKER_HOST), skipping the shadow-mode proof");
            return;
        }
    };
    if docker.inspect_image(REDIS_IMAGE).await.is_err() {
        println!("Image {REDIS_IMAGE} is not present locally, skipping the shadow-mode proof");
        return;
    }
    let tag = format!("p4s{}", std::process::id());
    let (cp, worker, matching) = (format!("{tag}cp"), format!("{tag}wk"), format!("{tag}ok"));
    let container = |name: &str| DatabaseEngine::Redis.container_name(name);
    let _cleanup = Cleanup {
        docker: docker.clone(),
        containers: vec![container(&cp), container(&worker), container(&matching)],
        volumes: vec![
            format!("redis_data_{cp}"),
            format!("{}_data", container(&worker)),
            format!("{}_data", container(&matching)),
        ],
    };
    let agent = agent_state(docker.clone(), "127.0.0.1");

    // A control-plane database, created by the engine's legacy request.
    RedisService::new(cp.clone(), Arc::new(docker.clone()))
        .init(redis_config(&cp, free_port(), "seeded-shadow-cp-1"))
        .await
        .expect("the control-plane engine creates its database");

    // A worker database, created by the agent's legacy request.
    let legacy = ServiceCreateRequest {
        name: container(&worker),
        service_type: "redis".into(),
        image: REDIS_IMAGE.into(),
        environment: HashMap::new(),
        port_mappings: vec![ServicePortMapping {
            host_port: 0,
            container_port: 6379,
        }],
        volumes: HashMap::from([(format!("{}_data", container(&worker)), "/data".into())]),
        network: None,
        command: Some(vec![
            "redis-server".into(),
            "--requirepass".into(),
            "seeded-shadow-wk-1".into(),
        ]),
        resource_limits: None,
    };
    let response = create_service(State(agent.clone()), Json(legacy))
        .await
        .into_response();
    assert!(response.status().is_success(), "{}", response.status());

    // A worker database created from its spec (enforce): the matching one.
    let params = RemoteServiceCreateParams {
        name: container(&matching),
        service_type: "redis".into(),
        image: REDIS_IMAGE.into(),
        environment: HashMap::new(),
        port_mappings: vec![RemotePortMapping {
            host_port: 0,
            container_port: 6379,
        }],
        volumes: HashMap::from([(format!("{}_data", container(&matching)), "/data".into())]),
        network: None,
        command: Some(vec![
            "redis-server".into(),
            "--requirepass".into(),
            "seeded-shadow-ok-1".into(),
        ]),
        resource_limits: None,
    };
    let process = DatabaseProcess::from_remote_params(DatabaseEngine::Redis, &params);
    let plan = DatabaseWorkloadPlan::admit(
        &DatabaseIdentity {
            service_id: 3,
            service_name: matching.clone(),
            engine: DatabaseEngine::Redis,
            node_id: Some(2),
            container_name: container(&matching),
        },
        &process,
    )
    .expect("admitted");
    let response = create_service_workload(
        State(agent.clone()),
        Json(ServiceWorkloadCreateRequest {
            service_name: matching.clone(),
            plan,
            secrets: process.secret_values(),
            network: None,
            replace_existing: false,
        }),
    )
    .await
    .into_response();
    assert!(response.status().is_success(), "{}", response.status());

    let rows = vec![
        row(1, &cp, None),
        row(2, &worker, Some(2)),
        row(3, &matching, Some(2)),
    ];
    let source = Snapshots {
        docker: docker.clone(),
        agent,
    };
    let report = shadow_pass(&rows, false, &source).await;

    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert_eq!(
        report.compared.len(),
        3,
        "a spec is computed for every database"
    );
    let fields: HashMap<i32, Vec<ShapeField>> = report
        .compared
        .iter()
        .map(|r| (r.service_id, r.fields()))
        .collect();
    // The control plane already has the Data host config; it lacks only
    // the worker's label set.
    assert_eq!(
        fields[&1],
        vec![ShapeField::Labels],
        "{:?}",
        report.compared[0]
    );
    // The worker's legacy request: defect 1.
    assert_eq!(
        fields[&2],
        vec![
            ShapeField::Labels,
            ShapeField::RestartPolicy,
            ShapeField::PidsLimit,
            ShapeField::NoNewPrivileges,
            ShapeField::Healthcheck,
            ShapeField::LogConfig,
        ],
        "{:?}",
        report.compared[1]
    );
    assert_eq!(fields[&3], vec![], "{:?}", report.compared[2]);

    let worker_result = &report.compared[1];
    let difference = |field| {
        worker_result
            .differences
            .iter()
            .find(|d| d.field == field)
            .expect("listed")
    };
    assert_eq!(
        difference(ShapeField::RestartPolicy).actual,
        "unless-stopped"
    );
    assert_eq!(difference(ShapeField::RestartPolicy).expected, "always");
    assert_eq!(difference(ShapeField::PidsLimit).expected, "512");
    assert_eq!(difference(ShapeField::NoNewPrivileges).actual, "false");
    assert!(difference(ShapeField::Healthcheck)
        .expected
        .contains("redis-cli ping"));
    assert!(difference(ShapeField::Labels)
        .expected
        .contains("temps.service_type=redis"));

    // No seeded secret reaches the report.
    let rendered = format!("{report:?}");
    for secret in [
        "seeded-shadow-cp-1",
        "seeded-shadow-wk-1",
        "seeded-shadow-ok-1",
    ] {
        assert!(!rendered.contains(secret), "{secret} leaked: {rendered}");
    }
    // And the pass changed nothing: the legacy worker is as it was created.
    let inspect = docker
        .inspect_container(
            &container(&worker),
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .expect("the legacy worker container still exists");
    let restart = inspect
        .host_config
        .and_then(|h| h.restart_policy)
        .and_then(|r| r.name)
        .map(|n| n.to_string());
    assert_eq!(restart.as_deref(), Some("unless-stopped"));
}

// ---------------------------------------------------------------------------
// Every database engine, without Docker: the requests each creation path
// sends are captured by the fake Engine API, and the shadow pass compares
// containers shaped exactly like those requests.
// ---------------------------------------------------------------------------

const ENGINES: [DatabaseEngine; 4] = [
    DatabaseEngine::Postgres,
    DatabaseEngine::Redis,
    DatabaseEngine::Mongodb,
    DatabaseEngine::Mariadb,
];

/// The create request the control-plane engine sends for `name`, legacy
/// (or `enforce`, when the registry says so).
async fn control_plane_body(engine: DatabaseEngine, name: &str) -> Value {
    use temps_providers::externalsvc::mariadb::MariaDbService;
    use temps_providers::externalsvc::mongodb::MongodbService;
    use temps_providers::externalsvc::postgres::PostgresService;

    let fake = fake().await;
    let docker = Arc::new(fake.docker());
    let config = |service_type, parameters| ServiceConfig {
        name: name.to_string(),
        service_type,
        version: None,
        parameters,
    };
    match engine {
        DatabaseEngine::Postgres => {
            let service = PostgresService::new(name.to_string(), docker);
            let _ = service
                .init(config(
                    ServiceType::Postgres,
                    json!({ "port": "15433", "password": "seeded-all-pg" }),
                ))
                .await;
        }
        DatabaseEngine::Redis => {
            let service = RedisService::new(name.to_string(), docker);
            let _ = service
                .init(config(
                    ServiceType::Redis,
                    json!({ "port": "16393", "password": "seeded-all-redis" }),
                ))
                .await;
        }
        DatabaseEngine::Mariadb => {
            let service = MariaDbService::new(name.to_string(), docker);
            let _ = service
                .init(config(
                    ServiceType::Mariadb,
                    json!({
                        "port": "13307",
                        "password": "seeded-all-maria",
                        "root_password": "seeded-all-root",
                    }),
                ))
                .await;
        }
        DatabaseEngine::Mongodb => {
            let service = MongodbService::new(name.to_string(), docker);
            service
                .init(config(
                    ServiceType::Mongodb,
                    json!({ "port": "17018", "password": "seeded-all-mongo" }),
                ))
                .await
                .expect("init makes no Docker call");
            let _ = service.start().await;
        }
    }
    let container = engine.container_name(name);
    fake.creates()
        .into_iter()
        .find(|create| create.name.as_deref() == Some(container.as_str()))
        .map(|create| golden_value(&create)["body"].clone())
        .unwrap_or_else(|| panic!("{engine}: no create for {container}: {:?}", fake.calls()))
}

/// The agent request the control plane sends a worker for `name`, as
/// `ExternalServiceManager::build_remote_create_params` builds it.
fn worker_params(engine: DatabaseEngine, name: &str) -> RemoteServiceCreateParams {
    let container = engine.container_name(name);
    let env = |pairs: &[(&str, &str)]| {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<HashMap<_, _>>()
    };
    let (image, port, environment, path, command) = match engine {
        DatabaseEngine::Postgres => (
            "gotempsh/postgres-walg:18-bookworm",
            5432,
            env(&[
                ("POSTGRES_USER", "postgres"),
                ("POSTGRES_PASSWORD", "seeded-all-pg-wk"),
                ("POSTGRES_DB", "postgres"),
                ("POSTGRES_HOST_AUTH_METHOD", "md5"),
            ]),
            "/var/lib/postgresql",
            Some(
                [
                    "postgres",
                    "-c",
                    "max_connections=100",
                    "-c",
                    "wal_level=replica",
                    "-c",
                    "archive_mode=off",
                    "-c",
                    "archive_timeout=60",
                ]
                .map(String::from)
                .to_vec(),
            ),
        ),
        DatabaseEngine::Redis => (
            REDIS_IMAGE,
            6379,
            HashMap::new(),
            "/data",
            Some(
                ["redis-server", "--requirepass", "seeded-all-redis-wk"]
                    .map(String::from)
                    .to_vec(),
            ),
        ),
        DatabaseEngine::Mongodb => (
            "mongo:7",
            27017,
            env(&[
                ("MONGO_INITDB_ROOT_USERNAME", "admin"),
                ("MONGO_INITDB_ROOT_PASSWORD", "seeded-all-mongo-wk"),
                ("MONGO_INITDB_DATABASE", "admin"),
            ]),
            "/data/db",
            None,
        ),
        DatabaseEngine::Mariadb => (
            "mariadb:lts",
            3306,
            env(&[
                ("MARIADB_ROOT_PASSWORD", "seeded-all-root-wk"),
                ("MARIADB_DATABASE", "app"),
                ("MARIADB_USER", "app"),
                ("MARIADB_PASSWORD", "seeded-all-maria-wk"),
                ("MARIADB_AUTO_UPGRADE", "1"),
            ]),
            "/var/lib/mysql",
            Some(
                temps_providers::externalsvc::mariadb::MariaDbSizeProfile::default().server_args(),
            ),
        ),
    };
    RemoteServiceCreateParams {
        name: container.clone(),
        service_type: engine.as_str().to_string(),
        image: image.to_string(),
        environment,
        port_mappings: vec![RemotePortMapping {
            host_port: 0,
            container_port: port,
        }],
        volumes: HashMap::from([(format!("{container}_data"), path.to_string())]),
        network: Some(temps_core::NETWORK_NAME.to_string()),
        command,
        resource_limits: None,
    }
}

/// The create request the agent's legacy `create_service` sends.
async fn worker_legacy_body(params: &RemoteServiceCreateParams) -> Value {
    let fake = fake().await;
    let request = ServiceCreateRequest {
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
    };
    let _ = create_service(
        State(agent_state(fake.docker(), "127.0.0.1")),
        Json(request),
    )
    .await
    .into_response();
    bodies(&fake)
        .pop()
        .unwrap_or_else(|| panic!("{}: no worker create: {:?}", params.name, fake.calls()))
}

/// A running container exactly as a create request describes it.
fn snapshot_of(container: &str, body: &Value) -> ContainerSnapshot {
    let mut config = body.clone();
    if let Some(config) = config.as_object_mut() {
        config.remove("HostConfig");
        config.remove("NetworkingConfig");
    }
    // Docker reports every volume a container uses, whether requested as a
    // `Binds` entry (`name:/path[:opts]`) or a `HostConfig.Mounts` entry, in
    // the inspect's `Mounts`; a source starting with `/` is a host path.
    let host = &body["HostConfig"];
    let mut mounts = Vec::new();
    for bind in host["Binds"].as_array().into_iter().flatten() {
        let mut parts = bind.as_str().unwrap_or_default().split(':');
        if let (Some(source), Some(target)) = (parts.next(), parts.next()) {
            mounts.push(if source.starts_with('/') {
                json!({ "Type": "bind", "Source": source, "Destination": target })
            } else {
                json!({ "Type": "volume", "Name": source, "Destination": target })
            });
        }
    }
    for mount in host["Mounts"].as_array().into_iter().flatten() {
        mounts.push(json!({
            "Type": mount["Type"],
            "Name": mount["Source"],
            "Source": mount["Source"],
            "Destination": mount["Target"],
        }));
    }
    let inspect = json!({
        "Name": format!("/{container}"),
        "Image": body["Image"],
        "Config": config,
        "HostConfig": body["HostConfig"],
        "Mounts": mounts,
        "State": { "Status": "running", "Running": true },
    });
    ContainerSnapshot {
        found: true,
        inspect: Some(serde_json::from_value(inspect).expect("an inspect response")),
        image_env: Vec::new(),
        image_cmd: None,
    }
}

struct Captured(HashMap<String, ContainerSnapshot>);

#[async_trait::async_trait]
impl SnapshotSource for Captured {
    async fn snapshot(
        &self,
        identity: &DatabaseIdentity,
    ) -> Result<ContainerSnapshot, ServiceWorkloadError> {
        Ok(self
            .0
            .get(&identity.container_name)
            .cloned()
            .unwrap_or_default())
    }
}

#[tokio::test]
async fn shadow_pass_computes_a_spec_for_every_database_engine() {
    let mut snapshots = HashMap::new();
    let mut rows = Vec::new();
    let mut id = 100;
    for engine in ENGINES {
        let cp = format!("all{}cp", engine.as_str());
        let wk = format!("all{}wk", engine.as_str());
        snapshots.insert(
            engine.container_name(&cp),
            snapshot_of(
                &engine.container_name(&cp),
                &control_plane_body(engine, &cp).await,
            ),
        );
        let params = worker_params(engine, &wk);
        snapshots.insert(
            params.name.clone(),
            snapshot_of(&params.name, &worker_legacy_body(&params).await),
        );
        for (name, node_id) in [(cp, None), (wk, Some(7))] {
            id += 1;
            let mut row = row(id, &name, node_id);
            row.service_type = engine.as_str().to_string();
            rows.push((engine, node_id, row));
        }
    }

    let models: Vec<_> = rows.iter().map(|(_, _, row)| row.clone()).collect();
    let report = shadow_pass(&models, false, &Captured(snapshots)).await;

    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert_eq!(report.compared.len(), 8, "a spec for every database");
    let rendered = format!("{report:?}");
    assert!(
        !rendered.contains("seeded-all"),
        "a seeded secret reached the report: {rendered}"
    );
    for ((engine, node_id, row), result) in rows.iter().zip(&report.compared) {
        assert_eq!(result.service_id, row.id);
        let expected = match node_id {
            // The control-plane engines already send the Data host config;
            // only the worker's label set is missing.
            None => vec![ShapeField::Labels],
            // The worker's generic request: defect 1, for every engine.
            Some(_) => vec![
                ShapeField::Labels,
                ShapeField::RestartPolicy,
                ShapeField::PidsLimit,
                ShapeField::NoNewPrivileges,
                ShapeField::Healthcheck,
                ShapeField::LogConfig,
            ],
        };
        assert_eq!(result.fields(), expected, "{engine} {result:?}");
    }
}

#[tokio::test]
async fn enforce_mode_sends_the_same_create_request_from_both_paths_for_every_engine() {
    for (offset, engine) in ENGINES.into_iter().enumerate() {
        let name = format!("allenf{}", engine.as_str());
        let container = engine.container_name(&name);
        let service_id = 200 + offset as i32;
        let legacy: ContainerCreateBody =
            serde_json::from_value(control_plane_body(engine, &name).await)
                .expect("the legacy request parses as a create body");

        let database = EnforcedDatabase {
            service_id,
            service_name: name.clone(),
            engine,
        };
        registry::set_enforced(&container, database.clone());
        let control_plane = control_plane_body(engine, &name).await;
        registry::clear_service(service_id);
        assert_eq!(
            control_plane["Labels"]["sh.temps.service.type"],
            engine.as_str(),
            "{engine}: the enforced control-plane request comes from the spec"
        );

        let (plan, process, _) =
            registry::enforced_plan(&database, &container, &legacy).expect("admitted");
        let worker = fake().await;
        let _ = create_service_workload(
            State(agent_state(worker.docker(), "127.0.0.1")),
            Json(ServiceWorkloadCreateRequest {
                service_name: name.clone(),
                plan,
                secrets: process.secret_values(),
                network: Some(temps_core::NETWORK_NAME.to_string()),
                replace_existing: false,
            }),
        )
        .await
        .into_response();
        let from_worker = bodies(&worker).pop().expect("one worker create");
        assert_eq!(
            control_plane, from_worker,
            "{engine}: one request, whichever path"
        );
    }
}
