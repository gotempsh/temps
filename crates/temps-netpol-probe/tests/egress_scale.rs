// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The 50-sandbox egress measurement (ADR-055 §9): what it costs to put
//! 50 restricted sandboxes behind an egress proxy, three ways.
//!
//! 1. **Sidecars** (today): one Node.js proxy per sandbox, each on that
//!    sandbox's internal network, with today's limits (128 MiB, 0.25 CPU).
//! 2. **Joined**: one Rust proxy ([`temps_workload_netpol::EgressProxy`])
//!    that joins every sandbox's internal network.
//! 3. **Shared**: one Rust proxy and every sandbox on a single internal
//!    network, with member-to-member traffic blocked by the node enforcer
//!    (each sandbox's egress is restricted to the proxy).
//!
//! It prints idle memory for each, the time to wire the proxy up, and for
//! the shared layout the proxy's CPU and memory after 50 sandboxes make 20
//! requests each through it at once. It asserts what the decision rests
//! on: the shared layout blocks member-to-member traffic, every proxied
//! request is answered, nothing is refused for saturation, and one Rust
//! proxy uses less memory than the sidecars it replaces.
//!
//! Skips, saying why, without Docker, `alpine:3.21`, the nft helper image,
//! a sandbox image with `node`, or the cross-built probe binary.

use std::collections::HashMap;
use std::net::IpAddr;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use bollard::exec::{StartExecOptions, StartExecResults};
use bollard::models::{
    ContainerCreateBody, EndpointIpamConfig, EndpointSettings, ExecConfig, HostConfig, Ipam,
    IpamConfig, NetworkConnectRequest, NetworkCreateRequest, NetworkingConfig,
};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, LogsOptions, RemoveContainerOptions, StatsOptions,
};
use bollard::Docker;
use futures::{FutureExt, StreamExt};
use temps_netpol_probe::ServeConfig;
use temps_workload::{labels, WorkloadKind, WorkloadRef};
use temps_workload_netpol::enforcer::{EnforcerError, ObservedContainer};
use temps_workload_netpol::executor::HELPER_IMAGE;
use temps_workload_netpol::{
    choose_executor, choose_port_resolver, Action, DnsPolicy, DockerInstanceSource, EgressPolicy,
    EgressRule, EgressTable, EgressTarget, HostPattern, InstanceSource, L4Protocol, NodeEnforcer,
    NodeMode, PassOutcome, PolicySnapshot, PortRange, Profile, StaticPolicySource, SystemPeer,
    TableName, WorkloadPolicy,
};

const SANDBOXES: usize = 50;
const REQUESTS_EACH: usize = 20;
const IMAGE: &str = "alpine:3.21";
const SIDECAR_IMAGES: [&str; 2] = [
    "ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.4-beta",
    "ghcr.io/gotempsh/temps-sandbox-node:latest",
];
const TEST_LABEL: &str = "sh.temps.test";
const TEST_VALUE: &str = "adr055-p7";
const SCOPE_LABEL: &str = "sh.temps.test.scope";
const SCOPE: &str = "egress-scale";
const SHARED_SUBNET: &str = "10.237.150.0/24";
const PROXY_IP: &str = "10.237.150.2";
const TARGET_IP: &str = "10.237.150.3";
const MIB: f64 = 1024.0 * 1024.0;

/// Today's sidecar, verbatim from the sandbox provider.
fn sidecar_script() -> Option<&'static str> {
    let source = include_str!("../../temps-agents/src/sandbox/docker.rs");
    let start = source.find("const SANDBOX_EGRESS_PROXY_SCRIPT: &str = r#\"")?;
    let body = &source[start..];
    let open = body.find("r#\"")? + 3;
    let close = body[open..].find("\"#;")?;
    Some(&body[open..open + close])
}

/// This run's id. It scopes every label, container, network, nft table and
/// rule comment the run creates, so concurrent runs (two checkouts, a CI
/// retry) never observe or remove each other's objects, and cleanup touches
/// only what this run recorded.
fn run_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{}{:04x}", std::process::id(), nanos & 0xffff)
}

fn bin_path() -> PathBuf {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/musl")
        .join(format!("{}-unknown-linux-musl", std::env::consts::ARCH));
    let release = base.join("release/temps-netpol-probe");
    if release.exists() {
        release
    } else {
        base.join("debug/temps-netpol-probe")
    }
}

fn sandbox(i: usize) -> WorkloadRef {
    WorkloadRef::new(WorkloadKind::Sandbox, 1000 + i as i64)
}

fn shared_ip(i: usize) -> String {
    format!("10.237.150.{}", 10 + i)
}

#[derive(Debug, Clone, Copy, Default)]
struct Usage {
    memory: u64,
    cpu_ns: u64,
}

struct Scale {
    docker: Docker,
    run: String,
    sidecar_image: String,
    config_dir: PathBuf,
    containers: Vec<String>,
    networks: Vec<String>,
    tables: Vec<TableName>,
}

impl Scale {
    async fn start() -> Option<Scale> {
        let docker = Docker::connect_with_defaults().ok()?;
        if docker.ping().await.is_err() {
            println!("Docker not available, skipping");
            return None;
        }
        for image in [IMAGE, HELPER_IMAGE] {
            if docker.inspect_image(image).await.is_err() {
                println!("{image} not present, skipping");
                return None;
            }
        }
        let mut sidecar_image = None;
        for image in SIDECAR_IMAGES {
            if docker.inspect_image(image).await.is_ok() {
                sidecar_image = Some(image.to_string());
                break;
            }
        }
        let Some(sidecar_image) = sidecar_image else {
            println!("no sandbox image with node present ({SIDECAR_IMAGES:?}), skipping");
            return None;
        };
        if sidecar_script().is_none() {
            println!("the sidecar script was not found in the sandbox provider, skipping");
            return None;
        }
        if !bin_path().exists() {
            println!(
                "{} is missing, skipping (cross-build the probe)",
                bin_path().display()
            );
            return None;
        }
        let run = run_id();
        let config_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../target/musl/egress-scale-config-{run}"));
        if std::fs::create_dir_all(&config_dir).is_err() {
            return None;
        }
        Some(Scale {
            docker,
            run,
            sidecar_image,
            config_dir,
            containers: Vec::new(),
            networks: Vec::new(),
            tables: Vec::new(),
        })
    }

    /// This run's scope label value: only its containers are enforced on.
    fn scope(&self) -> String {
        format!("{SCOPE}-{}", self.run)
    }

    /// The prefix of every container and network name in this run.
    fn prefix(&self) -> String {
        format!("temps-p7-scale-{}", self.run)
    }

    fn labels(&self, workload: Option<WorkloadRef>) -> HashMap<String, String> {
        let mut map = HashMap::from([
            (TEST_LABEL.to_string(), TEST_VALUE.to_string()),
            (SCOPE_LABEL.to_string(), self.scope()),
        ]);
        if let Some(w) = workload {
            map.insert(labels::KIND.to_string(), w.kind.as_str().to_string());
            map.insert(labels::OWNER_ID.to_string(), w.id.to_string());
        }
        map
    }

    async fn remove_container(&self, name: &str) {
        let _ = self
            .docker
            .remove_container(
                name,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
    }

    async fn network(&mut self, name: &str, subnet: &str) {
        self.docker
            .create_network(NetworkCreateRequest {
                name: name.to_string(),
                driver: Some("bridge".to_string()),
                internal: Some(true),
                ipam: Some(Ipam {
                    config: Some(vec![IpamConfig {
                        subnet: Some(subnet.to_string()),
                        ..Default::default()
                    }]),
                    ..Default::default()
                }),
                labels: Some(self.labels(None)),
                ..Default::default()
            })
            .await
            .unwrap_or_else(|e| {
                panic!(
                    "create network {name} ({subnet}): {e}. The subnets are fixed, so this \
                     fails while another run, or one that was killed, holds them; list those \
                     with `docker network ls --filter label={TEST_LABEL}={TEST_VALUE}`"
                )
            });
        self.networks.push(name.to_string());
    }

    #[allow(clippy::too_many_arguments)]
    async fn container(
        &mut self,
        name: &str,
        image: &str,
        workload: Option<WorkloadRef>,
        network: &str,
        address: Option<String>,
        entrypoint: Option<Vec<String>>,
        cmd: Vec<String>,
        env: Vec<String>,
        limits: bool,
    ) {
        let body = ContainerCreateBody {
            image: Some(image.to_string()),
            entrypoint,
            cmd: Some(cmd),
            env: Some(env),
            labels: Some(self.labels(workload)),
            host_config: Some(HostConfig {
                network_mode: Some(network.to_string()),
                binds: Some(vec![
                    format!("{}:/probe:ro", bin_path().display()),
                    format!("{}:/config:ro", self.config_dir.display()),
                ]),
                cap_drop: Some(vec!["ALL".to_string()]),
                security_opt: Some(vec!["no-new-privileges:true".to_string()]),
                memory: limits.then_some(128 * 1024 * 1024),
                nano_cpus: limits.then_some(250_000_000),
                ..Default::default()
            }),
            networking_config: address.map(|address| NetworkingConfig {
                endpoints_config: Some(HashMap::from([(
                    network.to_string(),
                    EndpointSettings {
                        ipam_config: Some(EndpointIpamConfig {
                            ipv4_address: Some(address),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                )])),
            }),
            ..Default::default()
        };
        self.docker
            .create_container(
                Some(CreateContainerOptionsBuilder::new().name(name).build()),
                body,
            )
            .await
            .unwrap_or_else(|e| panic!("create container {name}: {e}"));
        self.containers.push(name.to_string());
        self.docker
            .start_container(name, None)
            .await
            .unwrap_or_else(|e| panic!("start container {name}: {e}"));
    }

    async fn usage(&self, name: &str) -> Usage {
        let mut stream = self.docker.stats(
            name,
            Some(StatsOptions {
                stream: false,
                one_shot: true,
            }),
        );
        let Some(Ok(stats)) = stream.next().await else {
            return Usage::default();
        };
        let memory = stats
            .memory_stats
            .map(|m| {
                let inactive = m
                    .stats
                    .as_ref()
                    .and_then(|s| s.get("inactive_file").copied())
                    .unwrap_or(0);
                m.usage.unwrap_or(0).saturating_sub(inactive)
            })
            .unwrap_or(0);
        let cpu_ns = stats
            .cpu_stats
            .and_then(|c| c.cpu_usage)
            .and_then(|u| u.total_usage)
            .unwrap_or(0);
        Usage { memory, cpu_ns }
    }

    async fn exec(&self, name: &str, script: &str) -> String {
        let Ok(exec) = self
            .docker
            .create_exec(
                name,
                ExecConfig {
                    cmd: Some(vec!["sh".into(), "-c".into(), script.into()]),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
        else {
            return String::new();
        };
        let mut text = String::new();
        if let Ok(StartExecResults::Attached { mut output, .. }) = self
            .docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: false,
                    ..Default::default()
                }),
            )
            .await
        {
            while let Some(Ok(chunk)) = output.next().await {
                text.push_str(&String::from_utf8_lossy(&chunk.into_bytes()));
            }
        }
        text
    }

    async fn logs(&self, name: &str) -> String {
        let mut stream = self.docker.logs(
            name,
            Some(LogsOptions {
                stdout: true,
                stderr: true,
                ..Default::default()
            }),
        );
        let mut text = String::new();
        while let Some(Ok(chunk)) = stream.next().await {
            text.push_str(&String::from_utf8_lossy(&chunk.into_bytes()));
        }
        text
    }

    /// Removes every container and network created so far.
    async fn teardown(&mut self) {
        for name in std::mem::take(&mut self.containers) {
            self.remove_container(&name).await;
        }
        for name in std::mem::take(&mut self.networks) {
            let _ = self.docker.remove_network(&name).await;
        }
    }

    async fn cleanup(&mut self) {
        self.teardown().await;
        let daemon_is_local = std::env::var("DOCKER_HOST")
            .map(|h| h.starts_with("unix://"))
            .unwrap_or(true);
        let executor = choose_executor(self.docker.clone(), daemon_is_local);
        for table in std::mem::take(&mut self.tables) {
            let _ = executor
                .apply(&temps_workload_netpol::node_removal_ruleset(&table))
                .await;
        }
        let _ = std::fs::remove_dir_all(&self.config_dir);
    }
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / MIB
}

fn proxy_config(identities: Vec<(WorkloadRef, IpAddr)>) -> ServeConfig {
    let egress = identities
        .iter()
        .map(|(w, _)| EgressPolicy {
            subject: *w,
            rules: vec![
                EgressRule {
                    id: 1,
                    target: EgressTarget::Host(
                        HostPattern::parse("*").unwrap_or_else(|e| panic!("{e}")),
                    ),
                    ports: None,
                    action: Action::Allow,
                },
                EgressRule {
                    id: 2,
                    target: EgressTarget::Cidr(
                        format!("{TARGET_IP}/32")
                            .parse()
                            .unwrap_or_else(|e| panic!("{e:?}")),
                    ),
                    ports: Some(PortRange::single(8080)),
                    action: Action::Allow,
                },
            ],
        })
        .collect();
    ServeConfig {
        proxy_listen: Some("0.0.0.0:3128".parse().unwrap_or_else(|e| panic!("{e}"))),
        identities,
        egress,
        stats_every_secs: Some(1),
        ..Default::default()
    }
}

/// The shared layout's node: its sandboxes' egress is restricted to the
/// proxy, which is what blocks member-to-member traffic.
/// Observes only this run's containers (the `String` is its scope).
struct ScaleInstances(DockerInstanceSource, String);

#[async_trait]
impl InstanceSource for ScaleInstances {
    async fn observe(&self) -> Result<Vec<ObservedContainer>, EnforcerError> {
        Ok(self
            .0
            .observe()
            .await?
            .into_iter()
            .filter(|c| c.labels.get(SCOPE_LABEL) == Some(&self.1))
            .collect())
    }
    async fn pid(&self, id: &str) -> Result<Option<i64>, EnforcerError> {
        self.0.pid(id).await
    }
}

async fn measure(scale: &mut Scale) {
    let mut report: Vec<String> = Vec::new();
    let script = sidecar_script().unwrap_or_default().to_string();
    let prefix = scale.prefix();
    let shared_net = format!("{prefix}-shared");
    let idle = vec!["sleep".to_string(), "900".to_string()];

    // ---- 1. Sidecars ----
    let started = Instant::now();
    for i in 0..SANDBOXES {
        let net = format!("{prefix}-net-{i}");
        scale
            .network(&net, &format!("10.237.{}.0/24", 100 + i))
            .await;
        scale
            .container(
                &format!("{prefix}-sb-{i}"),
                IMAGE,
                Some(sandbox(i)),
                &net,
                None,
                None,
                idle.clone(),
                vec![],
                false,
            )
            .await;
        let image = scale.sidecar_image.clone();
        scale
            .container(
                &format!("{prefix}-sidecar-{i}"),
                &image,
                None,
                &net,
                None,
                Some(vec!["node".into()]),
                vec!["-e".into(), script.clone()],
                vec!["TEMPS_CONTROL_PLANE_URL=http://127.0.0.1:9".into()],
                true,
            )
            .await;
    }
    let wired = started.elapsed();
    tokio::time::sleep(Duration::from_secs(8)).await;
    let mut sidecar_memory = 0u64;
    let mut sidecar_cpu = 0u64;
    let mut sidecars_up = 0;
    for i in 0..SANDBOXES {
        let usage = scale.usage(&format!("{prefix}-sidecar-{i}")).await;
        if usage.memory > 0 {
            sidecars_up += 1;
        }
        sidecar_memory += usage.memory;
        sidecar_cpu += usage.cpu_ns;
    }
    assert_eq!(sidecars_up, SANDBOXES, "every sidecar is running");
    report.push(format!(
        "sidecars: {SANDBOXES} Node.js proxies, {:.1} MiB total ({:.1} MiB each), {:.2} CPU-s to start, {:.1}s to create {SANDBOXES} networks + sidecars",
        mib(sidecar_memory),
        mib(sidecar_memory) / SANDBOXES as f64,
        sidecar_cpu as f64 / 1e9,
        wired.as_secs_f64()
    ));
    scale.teardown().await;

    // ---- 2. One Rust proxy joining every network ----
    let identities: Vec<(WorkloadRef, IpAddr)> = (0..SANDBOXES)
        .map(|i| {
            let addr: IpAddr = format!("10.237.{}.10", 100 + i)
                .parse()
                .unwrap_or_else(|e| panic!("{e}"));
            (sandbox(i), addr)
        })
        .collect();
    let config = serde_json::to_vec(&proxy_config(identities)).unwrap_or_default();
    std::fs::write(scale.config_dir.join("joined.json"), config)
        .unwrap_or_else(|e| panic!("write the joined config: {e}"));
    for i in 0..SANDBOXES {
        let net = format!("{prefix}-net-{i}");
        scale
            .network(&net, &format!("10.237.{}.0/24", 100 + i))
            .await;
        scale
            .container(
                &format!("{prefix}-sb-{i}"),
                IMAGE,
                Some(sandbox(i)),
                &net,
                Some(format!("10.237.{}.10", 100 + i)),
                None,
                idle.clone(),
                vec![],
                false,
            )
            .await;
    }
    let started = Instant::now();
    let joined = format!("{prefix}-rust-joined");
    scale
        .container(
            &joined,
            IMAGE,
            None,
            &format!("{prefix}-net-0"),
            Some("10.237.100.2".to_string()),
            None,
            vec![
                "/probe".into(),
                "serve".into(),
                "/config/joined.json".into(),
            ],
            vec![],
            true,
        )
        .await;
    for i in 1..SANDBOXES {
        scale
            .docker
            .connect_network(
                &format!("{prefix}-net-{i}"),
                NetworkConnectRequest {
                    container: joined.clone(),
                    endpoint_config: Some(EndpointSettings {
                        ipam_config: Some(EndpointIpamConfig {
                            ipv4_address: Some(format!("10.237.{}.2", 100 + i)),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                },
            )
            .await
            .unwrap_or_else(|e| panic!("connect the proxy to network {i}: {e}"));
    }
    let join_time = started.elapsed();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let joined_usage = scale.usage(&joined).await;
    let last = SANDBOXES - 1;
    let probe = scale
        .exec(
            &format!("{prefix}-sb-{last}"),
            &format!("nc -z -w 2 10.237.{}.2 3128; echo rc=$?", 100 + last),
        )
        .await;
    assert!(
        probe.contains("rc=0"),
        "the last sandbox reaches the joined proxy: {probe}"
    );
    report.push(format!(
        "joined: 1 Rust proxy on {SANDBOXES} networks, {:.1} MiB idle, {:.1}s to start it and connect {} more networks",
        mib(joined_usage.memory),
        join_time.as_secs_f64(),
        SANDBOXES - 1
    ));
    scale.teardown().await;

    // ---- 3. One shared network ----
    let identities: Vec<(WorkloadRef, IpAddr)> = (0..SANDBOXES)
        .map(|i| {
            (
                sandbox(i),
                shared_ip(i).parse().unwrap_or_else(|e| panic!("{e}")),
            )
        })
        .collect();
    let config = serde_json::to_vec(&proxy_config(identities)).unwrap_or_default();
    std::fs::write(scale.config_dir.join("shared.json"), config)
        .unwrap_or_else(|e| panic!("write the shared config: {e}"));
    let started = Instant::now();
    scale.network(&shared_net, SHARED_SUBNET).await;
    let shared = format!("{prefix}-rust-shared");
    scale
        .container(
            &shared,
            IMAGE,
            None,
            &shared_net,
            Some(PROXY_IP.into()),
            None,
            vec![
                "/probe".into(),
                "serve".into(),
                "/config/shared.json".into(),
            ],
            vec![],
            true,
        )
        .await;
    let shared_wired = started.elapsed();
    scale
        .container(
            &format!("{prefix}-target"),
            IMAGE,
            None,
            &shared_net,
            Some(TARGET_IP.into()),
            None,
            vec!["/probe".into(), "sink".into(), "-".into(), "8080".into()],
            vec![],
            false,
        )
        .await;
    for i in 0..SANDBOXES {
        scale
            .container(
                &format!("{prefix}-sb-{i}"),
                IMAGE,
                Some(sandbox(i)),
                &shared_net,
                Some(shared_ip(i)),
                None,
                vec!["/probe".into(), "sink".into(), "-".into(), "7000".into()],
                vec![format!("http_proxy=http://{PROXY_IP}:3128")],
                false,
            )
            .await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    let member = |_from: usize, to: usize| format!("nc -z -w 2 {} 7000; echo rc=$?", shared_ip(to));
    let control = scale.exec(&format!("{prefix}-sb-0"), &member(0, 1)).await;
    assert!(
        control.contains("rc=0"),
        "control: members reach each other before enforcement: {control}"
    );

    // The node enforcer restricts every sandbox to the proxy.
    let daemon_is_local = std::env::var("DOCKER_HOST")
        .map(|h| h.starts_with("unix://"))
        .unwrap_or(true);
    let table =
        TableName::new(format!("temps_p7_scale_{}", scale.run)).unwrap_or_else(|e| panic!("{e}"));
    scale.tables.push(table.clone());
    let source: Arc<StaticPolicySource> = Arc::new(StaticPolicySource::new(
        NodeMode::Enforce,
        PolicySnapshot {
            policies: (0..SANDBOXES)
                .map(|i| WorkloadPolicy {
                    subject: sandbox(i),
                    egress: Profile::Restricted,
                    ingress: Profile::Open,
                    rules: vec![],
                })
                .collect(),
            system_peers: vec![SystemPeer {
                cidr: format!("{PROXY_IP}/32")
                    .parse()
                    .unwrap_or_else(|e| panic!("{e:?}")),
                ports: Some(PortRange::single(3128)),
                protocol: L4Protocol::Tcp,
            }],
            ..Default::default()
        },
    ));
    let mut enforcer = NodeEnforcer::new(
        "scale",
        table,
        Arc::from(choose_executor(scale.docker.clone(), daemon_is_local)),
        Arc::from(choose_port_resolver(scale.docker.clone(), daemon_is_local)),
        Arc::new(ScaleInstances(
            DockerInstanceSource::new("scale", scale.docker.clone()),
            scale.scope(),
        )),
        source.clone(),
        Arc::new(ArcSwap::from_pointee(DnsPolicy::open())),
        Arc::new(ArcSwap::from_pointee(EgressTable::default())),
    );
    let enforce_started = Instant::now();
    let outcome = enforcer
        .reconcile()
        .await
        .unwrap_or_else(|e| panic!("enforce: {e}"));
    let enforce_time = enforce_started.elapsed();
    let PassOutcome::Applied(summary) = outcome else {
        panic!("the scale node did not apply: {outcome:?}");
    };
    let blocked = scale.exec(&format!("{prefix}-sb-0"), &member(0, 1)).await;
    assert!(
        blocked.contains("rc=1"),
        "members cannot reach each other on the shared network: {blocked}"
    );
    let direct = scale
        .exec(
            &format!("{prefix}-sb-0"),
            &format!("nc -z -w 2 {TARGET_IP} 8080; echo rc=$?"),
        )
        .await;
    assert!(direct.contains("rc=1"), "nor the target directly: {direct}");

    let idle_shared = scale.usage(&shared).await;
    let before = scale.usage(&shared).await;
    let load_started = Instant::now();
    let load = format!(
        "ok=0; for i in $(seq {REQUESTS_EACH}); do wget -q -O - -Y on http://{TARGET_IP}:8080/ 2>/dev/null | grep -q ok && ok=$((ok+1)); done; echo ok=$ok"
    );
    let runs = (0..SANDBOXES).map(|i| {
        let name = format!("{prefix}-sb-{i}");
        let load = load.clone();
        let scale = &*scale;
        async move { scale.exec(&name, &load).await }
    });
    let outputs = futures::future::join_all(runs).await;
    let load_time = load_started.elapsed();
    let after = scale.usage(&shared).await;
    let answered: usize = outputs
        .iter()
        .filter_map(|o| {
            o.trim()
                .strip_prefix("ok=")
                .and_then(|n| n.trim().parse::<usize>().ok())
        })
        .sum();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let logs = scale.logs(&shared).await;
    let stats_line = logs
        .lines()
        .rev()
        .find(|l| l.starts_with("stats "))
        .unwrap_or_default();
    let stats: serde_json::Value =
        serde_json::from_str(stats_line.trim_start_matches("stats ")).unwrap_or_default();
    let total = SANDBOXES * REQUESTS_EACH;
    assert_eq!(
        answered, total,
        "every proxied request is answered: {outputs:?}"
    );
    assert_eq!(
        stats["refused_saturated"].as_u64(),
        Some(0),
        "nothing refused at saturation: {stats_line}"
    );
    report.push(format!(
        "shared: 1 Rust proxy + {SANDBOXES} sandboxes on 1 network, {:.1} MiB idle, {:.2}s to create the network and proxy, enforcer pass {:.2}s ({} bindings, {} restricted)",
        mib(idle_shared.memory),
        shared_wired.as_secs_f64(),
        enforce_time.as_secs_f64(),
        summary.bindings,
        summary.enforced_policies
    ));
    report.push(format!(
        "shared under load: {total} requests ({SANDBOXES} sandboxes x {REQUESTS_EACH}, concurrent) in {:.2}s, proxy CPU {:.3} CPU-s ({:.0} us/request), memory after {:.1} MiB; proxy stats {stats_line}",
        load_time.as_secs_f64(),
        (after.cpu_ns.saturating_sub(before.cpu_ns)) as f64 / 1e9,
        (after.cpu_ns.saturating_sub(before.cpu_ns)) as f64 / 1e3 / total as f64,
        mib(after.memory)
    ));
    source.set_mode(NodeMode::Legacy);
    let rollback = enforcer
        .reconcile()
        .await
        .unwrap_or_else(|e| panic!("rollback: {e}"));
    assert_eq!(
        rollback,
        PassOutcome::Removed {
            mode: NodeMode::Legacy
        }
    );
    let restored = scale.exec(&format!("{prefix}-sb-0"), &member(0, 1)).await;
    assert!(
        restored.contains("rc=0"),
        "legacy restores member-to-member traffic: {restored}"
    );

    println!("\n==== 50-sandbox egress measurement ====");
    for line in &report {
        println!("{line}");
    }
    println!("=======================================\n");
    assert!(
        after.memory < sidecar_memory,
        "one Rust proxy under load ({:.1} MiB) uses less than {SANDBOXES} idle sidecars ({:.1} MiB)",
        mib(after.memory),
        mib(sidecar_memory)
    );
}

#[tokio::test]
async fn fifty_sandboxes_behind_one_egress_proxy() {
    let Some(mut scale) = Scale::start().await else {
        return;
    };
    let outcome = AssertUnwindSafe(measure(&mut scale)).catch_unwind().await;
    scale.cleanup().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
