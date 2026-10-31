// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::externalsvc::postgres_cluster::{
    ClusterAuthSecrets, PostgresClusterConfig, PostgresClusterService, DEFAULT_CLUSTER_IMAGE,
};
use crate::externalsvc::ClusterMemberSpec;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::{ContainerCreateBody, HostConfig, NetworkCreateRequest};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, RemoveContainerOptions, StartContainerOptions,
};
use futures::StreamExt;
use std::time::{Duration, Instant};

struct Harness {
    docker: Arc<Docker>,
    manager: ExternalServiceManager,
    /// Bridge whose gateway stands in for the control plane's private
    /// address.
    network: String,
    address: std::net::IpAddr,
    containers: Vec<String>,
    volumes: Vec<String>,
}

impl Harness {
    async fn new() -> Option<Self> {
        let docker = match Docker::connect_with_local_defaults() {
            Ok(docker) => Arc::new(docker),
            Err(e) => {
                eprintln!("Docker not available, skipping: {e}");
                return None;
            }
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker daemon not responding, skipping");
            return None;
        }
        let network = format!(
            "temps-xhost-it-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        );
        docker
            .create_network(NetworkCreateRequest {
                name: network.clone(),
                driver: Some("bridge".to_string()),
                ..Default::default()
            })
            .await
            .ok()?;
        let address = docker
            .inspect_network(
                &network,
                None::<bollard::query_parameters::InspectNetworkOptions>,
            )
            .await
            .ok()
            .and_then(|info| info.ipam)
            .and_then(|ipam| ipam.config)
            .and_then(|configs| configs.into_iter().find_map(|c| c.gateway))
            .and_then(|gateway| gateway.parse().ok());
        let Some(address) = address else {
            // The guard does not exist yet; remove the bridge here.
            eprintln!("Test bridge {network} has no IPv4 gateway, skipping");
            let _ = docker.remove_network(&network).await;
            return None;
        };

        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let manager = ExternalServiceManager::new(
            db.clone(),
            Arc::new(EncryptionService::new_from_password(
                "cross-host-cluster-test",
            )),
            docker.clone(),
            Arc::new(temps_dns::DnsRegistry::new(db)),
        );
        Some(Self {
            docker,
            manager,
            network,
            address,
            containers: Vec::new(),
            volumes: Vec::new(),
        })
    }

    /// Run a data member the way a worker would: outside the control
    /// plane's Docker networks, reaching the monitor only by address.
    async fn run_worker_member(
        &mut self,
        name: &str,
        env: &HashMap<String, String>,
        cmd: Vec<String>,
    ) {
        let body = ContainerCreateBody {
            image: Some(DEFAULT_CLUSTER_IMAGE.to_string()),
            cmd: Some(cmd),
            env: Some(env.iter().map(|(k, v)| format!("{k}={v}")).collect()),
            host_config: Some(HostConfig {
                network_mode: Some("host".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        self.containers.push(name.to_string());
        self.docker
            .create_container(
                Some(CreateContainerOptionsBuilder::new().name(name).build()),
                body,
            )
            .await
            .unwrap_or_else(|e| panic!("create worker member {name}: {e}"));
        self.docker
            .start_container(name, None::<StartContainerOptions>)
            .await
            .unwrap_or_else(|e| panic!("start worker member {name}: {e}"));
    }

    /// Run `script` in `container`; `(-1, reason)` when the container
    /// cannot run it (e.g. it is restarting), never a panic, so callers
    /// can poll and still clean up.
    async fn sh(&self, container: &str, script: &str) -> (i64, String) {
        let exec = match self
            .docker
            .create_exec(
                container,
                CreateExecOptions {
                    cmd: Some(vec![
                        "bash".to_string(),
                        "-c".to_string(),
                        script.to_string(),
                    ]),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(exec) => exec,
            Err(e) => return (-1, format!("create exec in {container}: {e}")),
        };
        let mut out = String::new();
        if let Ok(StartExecResults::Attached { mut output, .. }) =
            self.docker.start_exec(&exec.id, None).await
        {
            while let Some(Ok(chunk)) = output.next().await {
                out.push_str(&chunk.to_string());
            }
        }
        let code = self
            .docker
            .inspect_exec(&exec.id)
            .await
            .ok()
            .and_then(|i| i.exit_code)
            .unwrap_or(-1);
        (code, out)
    }

    /// What the health probe reads: the data nodes registered with the
    /// monitor.
    async fn registered_nodes(&self, monitor: &str, port: u16) -> String {
        self.sh(
            monitor,
            &format!(
                "gosu postgres psql -X -At -p {port} -d pg_auto_failover \
                     -c 'SELECT nodename, reportedstate FROM pgautofailover.node'"
            ),
        )
        .await
        .1
    }

    async fn wait_until_monitor_accepts(&self, monitor: &str, port: u16) {
        let start = Instant::now();
        loop {
            let (code, _) = self
                .sh(monitor, &format!("gosu postgres pg_isready -q -p {port}"))
                .await;
            if code == 0 {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(120),
                "monitor {monitor} never accepted connections on {port}"
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
}

/// Removes everything the test created on every exit path — including a
/// panicking assertion, a timed-out wait or a failed create/start — so a
/// failing test never leaves containers, volumes or its bridge behind.
/// Cleanup is async, so it runs on a dedicated thread with its own
/// runtime: `drop` may be called from inside the test's runtime.
impl Drop for Harness {
    fn drop(&mut self) {
        let containers = std::mem::take(&mut self.containers);
        let volumes = std::mem::take(&mut self.volumes);
        let network = std::mem::take(&mut self.network);
        let docker = self.docker.clone();
        let cleanup = std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                eprintln!("could not build a runtime to clean up {containers:?}");
                return;
            };
            runtime.block_on(async move {
                for name in &containers {
                    let _ = docker
                        .remove_container(
                            name,
                            Some(RemoveContainerOptions {
                                force: true,
                                v: true,
                                ..Default::default()
                            }),
                        )
                        .await;
                }
                for volume in &volumes {
                    let _ = docker
                        .remove_volume(
                            volume,
                            None::<bollard::query_parameters::RemoveVolumeOptions>,
                        )
                        .await;
                }
                if !network.is_empty() {
                    let _ = docker.remove_network(&network).await;
                }
            });
        });
        let _ = cleanup.join();
    }
}

/// A port block nothing else on the Docker host is likely to hold; the
/// worker member binds directly in the host's network namespace.
fn test_base_port() -> u16 {
    let entropy = uuid::Uuid::new_v4().as_u128();
    30000 + u16::try_from(entropy % 2000).unwrap_or(0) * 10
}

fn cluster_parts(
    address: std::net::IpAddr,
) -> (
    PostgresClusterService,
    PostgresClusterConfig,
    ClusterAuthSecrets,
    ClusterMemberSpec,
    ClusterMemberSpec,
) {
    let name = format!("ha-pg-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
    let config: PostgresClusterConfig = serde_json::from_value(serde_json::json!({
        "password": "cross-host-test-password"
    }))
    .unwrap();
    let monitor = ClusterMemberSpec {
        role: "monitor".to_string(),
        node_id: None,
        ordinal: 0,
        hostname: Some(address.to_string()),
    };
    let worker_node = ClusterMemberSpec {
        role: "replica".to_string(),
        node_id: Some(1),
        ordinal: 1,
        hostname: Some(address.to_string()),
    };
    (
        PostgresClusterService::new(name, docker),
        config,
        ClusterAuthSecrets::generate(),
        monitor,
        worker_node,
    )
}

#[tokio::test]
async fn worker_data_node_registers_with_control_plane_monitor() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let address = h.address;
    let base_port = test_base_port();
    let (cluster, config, auth, monitor_spec, node_spec) = cluster_parts(address);
    let address_str = address.to_string();

    let monitor_params = cluster.build_member_params(
        &monitor_spec,
        &config,
        &address_str,
        base_port,
        base_port,
        Default::default(),
        &auth,
    );
    let monitor = monitor_params.container_name.clone();
    h.containers.push(monitor.clone());
    h.volumes.push(format!("{monitor}_data"));
    // The control-plane member, created exactly as `initialize_cluster`
    // now creates it for a cluster that also has worker members.
    let created = h
        .manager
        .create_local_cluster_member(&monitor, &monitor_params, Some(address))
        .await;
    if let Err(e) = created {
        panic!("creating the control-plane monitor failed: {e}");
    }
    h.wait_until_monitor_accepts(&monitor, base_port).await;

    let node_params = cluster.build_member_params(
        &node_spec,
        &config,
        &address_str,
        base_port,
        base_port + 1,
        Default::default(),
        &auth,
    );
    let node = node_params.container_name.clone();
    h.run_worker_member(
        &node,
        &node_params.environment,
        node_params.command.clone().unwrap_or_default(),
    )
    .await;

    let start = Instant::now();
    let registered = loop {
        let nodes = h.registered_nodes(&monitor, base_port).await;
        if nodes.lines().any(|line| line.starts_with(&node)) {
            break Ok(nodes);
        }
        if start.elapsed() > Duration::from_secs(180) {
            break Err(nodes);
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    };
    match registered {
        Ok(nodes) => eprintln!("monitor sees: {nodes}"),
        Err(nodes) => panic!(
            "the worker data member never registered with the control-plane monitor at \
                 {address}:{base_port}; monitor reports: {nodes:?}"
        ),
    }
}

/// Regression: after an unclean stop (host reboot, Docker daemon restart)
/// the member's stale pidfile survived in `/tmp`, the restarted PID 1 saw
/// "already running with PID 1" and the member crash-looped forever.
#[tokio::test]
async fn monitor_comes_back_after_an_unclean_restart() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let address = h.address;
    let base_port = test_base_port();
    let (cluster, config, auth, monitor_spec, _) = cluster_parts(address);
    let monitor_params = cluster.build_member_params(
        &monitor_spec,
        &config,
        &address.to_string(),
        base_port,
        base_port,
        Default::default(),
        &auth,
    );
    let monitor = monitor_params.container_name.clone();
    h.containers.push(monitor.clone());
    h.volumes.push(format!("{monitor}_data"));
    if let Err(e) = h
        .manager
        .create_local_cluster_member(&monitor, &monitor_params, None)
        .await
    {
        panic!("creating the control-plane monitor failed: {e}");
    }
    h.wait_until_monitor_accepts(&monitor, base_port).await;
    // Postgres already accepts connections while `pg_autoctl create
    // monitor` runs as a child; its pidfile then names a PID that is gone
    // after a restart, which pg_autoctl rightly treats as stale. The
    // failure needs the steady state: `pg_autoctl run` as PID 1, whose
    // pidfile says `1` — the restarted process's own PID.
    let start = Instant::now();
    loop {
        let (code, _) = h
            .sh(
                &monitor,
                "tr '\\0' ' ' < /proc/1/cmdline | grep -q 'pg_autoctl run' && \
                     grep -qx 1 /tmp/pg_autoctl/var/lib/postgresql/monitor/pg_autoctl.pid",
            )
            .await;
        if code == 0 {
            break;
        }
        if start.elapsed() > Duration::from_secs(120) {
            panic!("{monitor} never reached `pg_autoctl run` as PID 1");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // SIGKILL leaves pg_autoctl's pidfile behind, exactly as a crash does.
    let killed = h
        .docker
        .kill_container(
            &monitor,
            None::<bollard::query_parameters::KillContainerOptions>,
        )
        .await;
    let started = h
        .docker
        .start_container(&monitor, None::<StartContainerOptions>)
        .await;
    if killed.is_err() || started.is_err() {
        panic!("could not kill/start {monitor}: {killed:?} {started:?}");
    }

    let start = Instant::now();
    let recovered = loop {
        let running = h
            .docker
            .inspect_container(&monitor, None)
            .await
            .ok()
            .and_then(|i| i.state)
            .and_then(|s| s.running)
            .unwrap_or(false);
        let ready =
            h.sh(
                &monitor,
                &format!("gosu postgres pg_isready -q -p {base_port}"),
            )
            .await
            .0 == 0;
        if running && ready {
            break true;
        }
        if start.elapsed() > Duration::from_secs(90) {
            break false;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    };
    assert!(
        recovered,
        "the monitor must accept connections again after an unclean restart"
    );
}

/// The pre-fix publishing (loopback only) is what made the reported
/// cluster unrecoverable: the monitor is up, but nothing outside the
/// control plane's loopback can connect to it.
#[tokio::test]
async fn loopback_only_monitor_is_unreachable_from_a_worker() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let address = h.address;
    let base_port = test_base_port();
    let (cluster, config, auth, monitor_spec, _) = cluster_parts(address);
    let monitor_params = cluster.build_member_params(
        &monitor_spec,
        &config,
        &address.to_string(),
        base_port,
        base_port,
        Default::default(),
        &auth,
    );
    let monitor = monitor_params.container_name.clone();
    h.containers.push(monitor.clone());
    h.volumes.push(format!("{monitor}_data"));
    if let Err(e) = h
        .manager
        .create_local_cluster_member(&monitor, &monitor_params, None)
        .await
    {
        panic!("creating the control-plane monitor failed: {e}");
    }
    h.wait_until_monitor_accepts(&monitor, base_port).await;

    let probe = format!("{monitor}-probe");
    h.run_worker_member(
        &probe,
        &HashMap::new(),
        vec!["sleep".to_string(), "300".to_string()],
    )
    .await;
    let connect = |host: String| format!("timeout 5 bash -c 'exec 3<>/dev/tcp/{host}/{base_port}'");
    let (via_loopback, _) = h.sh(&probe, &connect("127.0.0.1".to_string())).await;
    let (via_address, _) = h.sh(&probe, &connect(address.to_string())).await;

    assert_eq!(via_loopback, 0, "the monitor itself must be up on loopback");
    assert_ne!(
        via_address, 0,
        "a loopback-only monitor must not be reachable on {address}:{base_port}"
    );
}
