// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The ADR-055 Phase 7 proof: two nodes, each running the real
//! [`NodeEnforcer`] over the real Docker daemon (instances observed with
//! [`DockerInstanceSource`], bridge ports resolved in the VM, rules applied
//! with the production executor), show that a restricted workload
//!
//! - (a) cannot reach a service it is not linked to (nor an unlinked port
//!   of the one it is linked to),
//! - (b) cannot spoof another workload's source address, and
//! - (c) cannot resolve external names — neither through the node
//!   resolver (policy answers) nor through any other resolver (egress).
//!
//! The two nodes are simulated on one Docker engine: one bridge per node
//! (`10.237.11.0/24`, `10.237.12.0/24`), no masquerading, and an accept
//! between the two in `DOCKER-USER`, which is what the overlay gives two
//! real nodes. Each node has its own enforcer and its own nftables tables;
//! an enforcer only knows the other node's workloads by address
//! ([`PolicySnapshot::remote`]).
//!
//! Every negative assertion is preceded by the same probe succeeding with
//! nothing enforced, so a pass proves the policy did the blocking.
//!
//! Needs the probe binary cross-built for Linux:
//! `cargo build -p temps-netpol-probe --target <arch>-unknown-linux-musl`
//! (see `bin_path`). Skips, saying why, without Docker, `alpine:3.21`, the
//! nft helper image or that binary.

#![allow(
    clippy::disallowed_methods,
    reason = "integration test: reads fixture configuration from the environment"
)]
use std::collections::HashMap;
use std::net::IpAddr;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use bollard::exec::{StartExecOptions, StartExecResults};
use bollard::models::{
    ContainerCreateBody, EndpointIpamConfig, EndpointSettings, ExecConfig, HostConfig, Ipam,
    IpamConfig, NetworkCreateRequest, NetworkingConfig,
};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, LogsOptions, RemoveContainerOptions, WaitContainerOptions,
};
use bollard::Docker;
use futures::{FutureExt, StreamExt};
use temps_netpol_probe::{ServeConfig, PUBLIC_DNS_ANSWER};
use temps_node_dns::ZoneRecord;
use temps_workload::{labels, WorkloadKind, WorkloadRef};
use temps_workload_netpol::dns::{group_short_name, Alias, NamedWorkload};
use temps_workload_netpol::enforcer::{EnforcerError, ObservedContainer};
use temps_workload_netpol::executor::HELPER_IMAGE;
use temps_workload_netpol::{
    choose_executor, choose_port_resolver, Action, Direction, DnsPolicy, DockerInstanceSource,
    EgressTable, IdentityMap, InstanceSource, L4Protocol, NodeEnforcer, NodeMode, Origin,
    PassOutcome, Peer, PolicySnapshot, PortRange, Profile, Rule, StaticPolicySource, SystemPeer,
    TableName, WorkloadPolicy,
};

const IMAGE: &str = "alpine:3.21";
const TEST_LABEL: &str = "sh.temps.test";
const TEST_VALUE: &str = "adr055-p7";
const NODE_LABEL: &str = "sh.temps.test.node";
const RUN_LABEL: &str = "sh.temps.test.run";

const SUBNET_A: &str = "10.237.11.0/24";
const SUBNET_B: &str = "10.237.12.0/24";

// Node A
const R_IP: &str = "10.237.11.10"; // restricted sandbox
const V_IP: &str = "10.237.11.11"; // open deployment, the spoofing victim
const X_IP: &str = "10.237.11.12"; // open deployment with NET_ADMIN, the attacker
const RESOLVER_IP: &str = "10.237.11.53"; // node A's resolver
                                          // Node B
const S1_IP: &str = "10.237.12.10"; // service linked to R (5432)
const S2_IP: &str = "10.237.12.11"; // service not linked to R; ingress only from V
const PUBLIC_IP: &str = "10.237.12.54"; // stands in for a public resolver

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
    let arch = std::env::consts::ARCH;
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/musl")
        .join(format!("{arch}-unknown-linux-musl"))
        .join("debug/temps-netpol-probe")
}

fn sandbox() -> WorkloadRef {
    WorkloadRef::new(WorkloadKind::Sandbox, 1)
}
fn victim() -> WorkloadRef {
    WorkloadRef::new(WorkloadKind::Deployment, 2)
}
fn attacker() -> WorkloadRef {
    WorkloadRef::new(WorkloadKind::Deployment, 3)
}
fn linked() -> WorkloadRef {
    WorkloadRef::new(WorkloadKind::Service, 10)
}
fn unlinked() -> WorkloadRef {
    WorkloadRef::new(WorkloadKind::Service, 11)
}

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap_or(IpAddr::from([0, 0, 0, 0]))
}

/// The control plane's view: R may reach S1:5432 (a link); S2 only
/// accepts UDP 9999 from V (an operator rule).
fn policies() -> Vec<WorkloadPolicy> {
    vec![
        WorkloadPolicy {
            subject: sandbox(),
            egress: Profile::Restricted,
            ingress: Profile::Open,
            rules: vec![Rule {
                id: 1,
                direction: Direction::Egress,
                peer: Peer::Workload(linked()),
                ports: Some(PortRange::single(5432)),
                protocol: L4Protocol::Tcp,
                action: Action::Allow,
                origin: Origin::Link,
            }],
        },
        WorkloadPolicy {
            subject: unlinked(),
            egress: Profile::Open,
            ingress: Profile::Restricted,
            rules: vec![Rule {
                id: 2,
                direction: Direction::Ingress,
                peer: Peer::Workload(victim()),
                ports: Some(PortRange::single(9999)),
                protocol: L4Protocol::Udp,
                action: Action::Allow,
                origin: Origin::Operator,
            }],
        },
    ]
}

fn names() -> Vec<NamedWorkload> {
    vec![
        NamedWorkload {
            fqdn: "db.shop.temps.local".into(),
            owner: linked(),
        },
        NamedWorkload {
            fqdn: "cache.shop.temps.local".into(),
            owner: unlinked(),
        },
    ]
}

fn aliases() -> Vec<Alias> {
    vec![
        Alias {
            alias: group_short_name("db", "shop"),
            canonical: "db.shop.temps.local".into(),
        },
        Alias {
            alias: group_short_name("cache", "shop"),
            canonical: "cache.shop.temps.local".into(),
        },
    ]
}

fn all_identities() -> Vec<(WorkloadRef, IpAddr)> {
    vec![
        (sandbox(), ip(R_IP)),
        (victim(), ip(V_IP)),
        (attacker(), ip(X_IP)),
        (linked(), ip(S1_IP)),
        (unlinked(), ip(S2_IP)),
    ]
}

/// What node `node` knows about the other node's workloads.
fn remote_for(node: &str) -> IdentityMap {
    let mut map = IdentityMap::new();
    for (workload, addr) in all_identities() {
        let on_a = addr.to_string().starts_with("10.237.11.");
        if (node == "a") != on_a {
            map.insert(workload, addr);
        }
    }
    map
}

fn snapshot_for(node: &str) -> PolicySnapshot {
    PolicySnapshot {
        policies: policies(),
        remote: remote_for(node),
        names: names(),
        aliases: aliases(),
        system_peers: vec![SystemPeer {
            cidr: format!("{RESOLVER_IP}/32")
                .parse()
                .unwrap_or_else(|_| "0.0.0.0/32".parse().unwrap_or_else(|_| unreachable!())),
            ports: Some(PortRange::single(53)),
            protocol: L4Protocol::Any,
        }],
        ..Default::default()
    }
}

/// Node A's resolver, built from the same snapshot inputs.
fn resolver_config() -> ServeConfig {
    let record = |id: i64, fqdn: &str, addr: &str| ZoneRecord {
        id,
        fqdn: fqdn.into(),
        record_type: "A".into(),
        target_ip: Some(addr.into()),
        target_port: None,
        ttl: 5,
        owner_kind: "service".into(),
        owner_id: id,
        node_id: None,
        generation: 1,
    };
    ServeConfig {
        dns_listen: Some(
            format!("{RESOLVER_IP}:53")
                .parse()
                .unwrap_or_else(|_| unreachable!()),
        ),
        upstream: vec![format!("{PUBLIC_IP}:53")
            .parse()
            .unwrap_or_else(|_| unreachable!())],
        records: vec![
            record(10, "db.shop.temps.local", S1_IP),
            record(11, "cache.shop.temps.local", S2_IP),
        ],
        policies: policies(),
        identities: all_identities(),
        names: names(),
        aliases: aliases(),
        ..Default::default()
    }
}

/// The containers of one simulated node.
struct NodeInstances {
    inner: DockerInstanceSource,
    node: &'static str,
    run: String,
}

#[async_trait]
impl InstanceSource for NodeInstances {
    async fn observe(&self) -> Result<Vec<ObservedContainer>, EnforcerError> {
        Ok(self
            .inner
            .observe()
            .await?
            .into_iter()
            .filter(|c| {
                c.labels.get(TEST_LABEL).map(String::as_str) == Some(TEST_VALUE)
                    && c.labels.get(RUN_LABEL) == Some(&self.run)
                    && c.labels.get(NODE_LABEL).map(String::as_str) == Some(self.node)
            })
            .collect())
    }
    async fn pid(&self, container_id: &str) -> Result<Option<i64>, EnforcerError> {
        self.inner.pid(container_id).await
    }
}

struct Lab {
    docker: Docker,
    run: String,
    containers: Vec<String>,
    networks: Vec<String>,
    config_dir: PathBuf,
    enforcers: Vec<(NodeEnforcer, Arc<StaticPolicySource>)>,
    routed: bool,
}

impl Lab {
    async fn start() -> Option<Lab> {
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
        if !bin_path().exists() {
            println!(
                "{} is missing, skipping: cross-build it with \
                 `cargo build -p temps-netpol-probe --target {}-unknown-linux-musl` \
                 into target/musl",
                bin_path().display(),
                std::env::consts::ARCH
            );
            return None;
        }
        let run = run_id();
        let config_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../target/musl/two-node-config-{run}"));
        if std::fs::create_dir_all(&config_dir).is_err() {
            println!("cannot create {}, skipping", config_dir.display());
            return None;
        }
        Some(Lab {
            docker,
            run,
            containers: Vec::new(),
            networks: Vec::new(),
            config_dir,
            enforcers: Vec::new(),
            routed: false,
        })
    }

    /// A container's full name in this run.
    fn full(&self, name: &str) -> String {
        format!("temps-p7-{}-{name}", self.run)
    }

    /// A simulated node's network in this run.
    fn net(&self, node: &str) -> String {
        format!("temps-p7-{}-node-{node}", self.run)
    }

    /// The comment on this run's DOCKER-USER route rules.
    fn rule_comment(&self) -> String {
        format!("adr055-p7-two-node-{}", self.run)
    }

    fn run_labels(&self) -> HashMap<String, String> {
        HashMap::from([
            (TEST_LABEL.to_string(), TEST_VALUE.to_string()),
            (RUN_LABEL.to_string(), self.run.clone()),
        ])
    }

    async fn network(&mut self, name: &str, subnet: &str) {
        self.docker
            .create_network(NetworkCreateRequest {
                name: name.to_string(),
                driver: Some("bridge".to_string()),
                ipam: Some(Ipam {
                    config: Some(vec![IpamConfig {
                        subnet: Some(subnet.to_string()),
                        ..Default::default()
                    }]),
                    ..Default::default()
                }),
                options: Some(HashMap::from([
                    (
                        "com.docker.network.bridge.enable_ip_masquerade".to_string(),
                        "false".to_string(),
                    ),
                    // Container addresses reachable from the other bridge,
                    // as over an overlay.
                    (
                        "com.docker.network.bridge.gateway_mode_ipv4".to_string(),
                        "nat-unprotected".to_string(),
                    ),
                ])),
                labels: Some(self.run_labels()),
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
        node: &str,
        workload: Option<WorkloadRef>,
        address: &str,
        cmd: Vec<String>,
        cap_add: Vec<String>,
        dns: Option<&str>,
    ) {
        let network = self.net(node);
        let mut container_labels = self.run_labels();
        container_labels.insert(NODE_LABEL.to_string(), node.to_string());
        if let Some(workload) = workload {
            container_labels.insert(labels::KIND.to_string(), workload.kind.as_str().to_string());
            container_labels.insert(labels::OWNER_ID.to_string(), workload.id.to_string());
        }
        let full = self.full(name);
        self.docker
            .create_container(
                Some(CreateContainerOptionsBuilder::new().name(&full).build()),
                ContainerCreateBody {
                    image: Some(IMAGE.to_string()),
                    cmd: Some(cmd),
                    labels: Some(container_labels),
                    host_config: Some(HostConfig {
                        network_mode: Some(network.clone()),
                        binds: Some(vec![
                            format!("{}:/probe:ro", bin_path().display()),
                            format!("{}:/config:ro", self.config_dir.display()),
                        ]),
                        cap_drop: Some(vec!["ALL".to_string()]),
                        cap_add: Some(
                            cap_add
                                .into_iter()
                                .chain(["NET_BIND_SERVICE".to_string()])
                                .collect(),
                        ),
                        dns: dns.map(|d| vec![d.to_string()]),
                        ..Default::default()
                    }),
                    networking_config: Some(NetworkingConfig {
                        endpoints_config: Some(HashMap::from([(
                            network,
                            EndpointSettings {
                                ipam_config: Some(EndpointIpamConfig {
                                    ipv4_address: Some(address.to_string()),
                                    ..Default::default()
                                }),
                                ..Default::default()
                            },
                        )])),
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap_or_else(|e| panic!("create container {full}: {e}"));
        self.containers.push(full.clone());
        self.docker
            .start_container(&full, None)
            .await
            .unwrap_or_else(|e| panic!("start container {full}: {e}"));
    }

    async fn exec(&self, name: &str, script: &str) -> String {
        let full = self.full(name);
        let exec = self
            .docker
            .create_exec(
                &full,
                ExecConfig {
                    cmd: Some(vec!["sh".into(), "-c".into(), script.into()]),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap_or_else(|e| panic!("create exec in {full}: {e}"));
        let started = self
            .docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: false,
                    ..Default::default()
                }),
            )
            .await
            .unwrap_or_else(|e| panic!("start exec in {full}: {e}"));
        let mut text = String::new();
        if let StartExecResults::Attached { mut output, .. } = started {
            while let Some(Ok(chunk)) = output.next().await {
                text.push_str(&String::from_utf8_lossy(&chunk.into_bytes()));
            }
        }
        text
    }

    async fn logs(&self, name: &str) -> String {
        let full = self.full(name);
        let mut stream = self.docker.logs(
            &full,
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

    /// Runs `script` on the Docker VM's network namespace with
    /// `NET_ADMIN` (the nft helper image).
    async fn vm_shell(&self, script: &str) -> Result<String, String> {
        let name = self.full("vm-shell");
        let _ = self
            .docker
            .remove_container(
                &name,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
        self.docker
            .create_container(
                Some(CreateContainerOptionsBuilder::new().name(&name).build()),
                ContainerCreateBody {
                    image: Some(HELPER_IMAGE.to_string()),
                    entrypoint: Some(vec!["sh".into(), "-c".into()]),
                    cmd: Some(vec![script.to_string()]),
                    labels: Some(self.run_labels()),
                    host_config: Some(HostConfig {
                        network_mode: Some("host".to_string()),
                        cap_drop: Some(vec!["ALL".to_string()]),
                        cap_add: Some(vec!["NET_ADMIN".to_string()]),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        self.docker
            .start_container(&name, None)
            .await
            .map_err(|e| e.to_string())?;
        let mut wait = self
            .docker
            .wait_container(&name, None::<WaitContainerOptions>);
        while wait.next().await.is_some() {}
        let mut stream = self.docker.logs(
            &name,
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
        let _ = self
            .docker
            .remove_container(
                &name,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
        Ok(text)
    }

    async fn enforcer(&mut self, node: &'static str) {
        let table = TableName::new(format!("temps_p7_{}_{node}", self.run))
            .unwrap_or_else(|e| panic!("{e}"));
        let daemon_is_local = std::env::var("DOCKER_HOST")
            .map(|h| h.starts_with("unix://"))
            .unwrap_or(true);
        let executor: Arc<dyn temps_workload_netpol::NftExecutor> =
            Arc::from(choose_executor(self.docker.clone(), daemon_is_local));
        let ports: Arc<dyn temps_workload_netpol::PortResolver> =
            Arc::from(choose_port_resolver(self.docker.clone(), daemon_is_local));
        let instances = Arc::new(NodeInstances {
            inner: DockerInstanceSource::new(format!("node-{node}"), self.docker.clone()),
            node,
            run: self.run.clone(),
        });
        let source = Arc::new(StaticPolicySource::new(
            NodeMode::Legacy,
            snapshot_for(node),
        ));
        let enforcer = NodeEnforcer::new(
            format!("node-{node}"),
            table,
            executor,
            ports,
            instances,
            source.clone(),
            Arc::new(ArcSwap::from_pointee(DnsPolicy::open())),
            Arc::new(ArcSwap::from_pointee(EgressTable::default())),
        );
        self.enforcers.push((enforcer, source));
    }

    async fn set_mode(&mut self, index: usize, mode: NodeMode) -> PassOutcome {
        let (enforcer, source) = &mut self.enforcers[index];
        source.set_mode(mode);
        enforcer
            .reconcile()
            .await
            .unwrap_or_else(|e| panic!("reconcile node {index} in {mode}: {e}"))
    }

    async fn tcp(&self, from: &str, addr: &str, port: u16) -> bool {
        let out = self
            .exec(from, &format!("nc -z -w 2 {addr} {port}; echo rc=$?"))
            .await;
        out.contains("rc=0")
    }

    async fn lookup(&self, from: &str, name: &str, server: &str) -> String {
        self.exec(
            from,
            &format!("timeout 6 nslookup -type=a {name} {server} 2>&1; true"),
        )
        .await
    }

    async fn cleanup(&mut self) {
        let mut tables = Vec::new();
        for (enforcer, source) in &mut self.enforcers {
            source.set_mode(NodeMode::Legacy);
            // From `enforce` this removes the tables; otherwise it is a
            // no-op, so remove explicitly as well.
            let _ = enforcer.reconcile().await;
            tables.push(enforcer.table().clone());
        }
        for table in tables {
            let _ = self
                .vm_shell(&format!(
                    "nft delete table inet {table} 2>/dev/null; nft delete table bridge {table} 2>/dev/null; true"
                ))
                .await;
        }
        if self.routed {
            let _ = self
                .vm_shell(&remove_route_script(&self.rule_comment()))
                .await;
        }
        for name in &self.containers {
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
        for name in &self.networks {
            let _ = self.docker.remove_network(name).await;
        }
        let _ = std::fs::remove_dir_all(&self.config_dir);
    }
}

fn add_route_script(comment: &str) -> String {
    format!(
        "nft insert rule ip filter DOCKER-USER ip saddr {SUBNET_A} ip daddr {SUBNET_B} accept comment \\\"{comment}\\\" && \
         nft insert rule ip filter DOCKER-USER ip saddr {SUBNET_B} ip daddr {SUBNET_A} accept comment \\\"{comment}\\\" && echo routed"
    )
}

fn remove_route_script(comment: &str) -> String {
    format!(
        "for h in $(nft -a list chain ip filter DOCKER-USER 2>/dev/null | grep '{comment}' | sed 's/.*# handle //'); do \
           nft delete rule ip filter DOCKER-USER handle $h; done; true"
    )
}

fn sh(script: &str) -> Vec<String> {
    vec!["sh".into(), "-c".into(), script.into()]
}

async fn wait_for<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..40 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("{what} never happened with nothing enforced: the lab is broken, not the policy");
}

async fn udp_seen(lab: &Lab, payload: &str) -> Option<String> {
    tokio::time::sleep(Duration::from_millis(700)).await;
    lab.logs("s2")
        .await
        .lines()
        .find(|l| l.starts_with("udp 9999 ") && l.ends_with(payload))
        .map(str::to_string)
}

async fn run(lab: &mut Lab) {
    let (net_a, net_b) = (lab.net("a"), lab.net("b"));
    lab.network(&net_a, SUBNET_A).await;
    lab.network(&net_b, SUBNET_B).await;
    let routed = lab
        .vm_shell(&add_route_script(&lab.rule_comment()))
        .await
        .unwrap_or_default();
    assert!(
        routed.contains("routed"),
        "DOCKER-USER route between the nodes: {routed}"
    );
    lab.routed = true;

    let config = serde_json::to_vec_pretty(&resolver_config()).unwrap_or_default();
    std::fs::write(lab.config_dir.join("resolver.json"), config)
        .unwrap_or_else(|e| panic!("write the resolver config: {e}"));

    let idle = sh("sleep 900");
    lab.container(
        "r",
        "a",
        Some(sandbox()),
        R_IP,
        idle.clone(),
        vec![],
        Some(RESOLVER_IP),
    )
    .await;
    lab.container(
        "v",
        "a",
        Some(victim()),
        V_IP,
        idle.clone(),
        vec![],
        Some(RESOLVER_IP),
    )
    .await;
    lab.container(
        "x",
        "a",
        Some(attacker()),
        X_IP,
        idle.clone(),
        vec!["NET_ADMIN".into()],
        None,
    )
    .await;
    lab.container(
        "resolver",
        "a",
        None,
        RESOLVER_IP,
        vec![
            "/probe".into(),
            "serve".into(),
            "/config/resolver.json".into(),
        ],
        vec![],
        None,
    )
    .await;
    lab.container(
        "s1",
        "b",
        Some(linked()),
        S1_IP,
        vec![
            "/probe".into(),
            "sink".into(),
            "-".into(),
            "5432,6379".into(),
        ],
        vec![],
        None,
    )
    .await;
    lab.container(
        "s2",
        "b",
        Some(unlinked()),
        S2_IP,
        vec!["/probe".into(), "sink".into(), "9999".into(), "5432".into()],
        vec![],
        None,
    )
    .await;
    lab.container(
        "public",
        "b",
        None,
        PUBLIC_IP,
        vec!["/probe".into(), "public-dns".into(), "53".into()],
        vec![],
        None,
    )
    .await;

    lab.enforcer("a").await;
    lab.enforcer("b").await;
    assert_eq!(lab.set_mode(0, NodeMode::Legacy).await, PassOutcome::Legacy);
    assert_eq!(lab.set_mode(1, NodeMode::Legacy).await, PassOutcome::Legacy);

    // ---- Controls: nothing enforced, everything reachable. ----
    wait_for("R reaching S1:5432", || lab.tcp("r", S1_IP, 5432)).await;
    assert!(
        lab.tcp("r", S1_IP, 6379).await,
        "control: R reaches S1:6379"
    );
    assert!(
        lab.tcp("r", S2_IP, 5432).await,
        "control: R reaches S2:5432"
    );
    let direct = lab.lookup("r", "registry.example.test", PUBLIC_IP).await;
    assert!(
        direct.contains(&PUBLIC_DNS_ANSWER.to_string()),
        "control: R resolves an external name through another resolver: {direct}"
    );
    wait_for("V resolving through node A's resolver", || async {
        lab.lookup("v", "registry.example.test", RESOLVER_IP)
            .await
            .contains(&PUBLIC_DNS_ANSWER.to_string())
    })
    .await;

    // ---- Shadow: computed, nothing applied. ----
    let PassOutcome::Shadowed(shadow) = lab.set_mode(0, NodeMode::Shadow).await else {
        panic!("node A did not shadow");
    };
    assert_eq!(
        shadow.local_workloads, 3,
        "R, V and X are node A's workloads"
    );
    assert_eq!(shadow.bindings, 3, "one bridge port each");
    assert!(lab.tcp("r", S2_IP, 5432).await, "shadow applies nothing");

    // ---- Node A enforces (node B still legacy, so (a) is R's egress alone). ----
    let PassOutcome::Applied(applied) = lab.set_mode(0, NodeMode::Enforce).await else {
        panic!("node A did not apply");
    };
    assert_eq!(applied.bindings, 3);
    assert!(
        matches!(
            lab.set_mode(0, NodeMode::Enforce).await,
            PassOutcome::Unchanged { .. }
        ),
        "a second pass with nothing changed applies nothing"
    );

    // (a) the linked port only.
    assert!(
        lab.tcp("r", S1_IP, 5432).await,
        "(a) R still reaches its linked S1:5432"
    );
    assert!(
        !lab.tcp("r", S1_IP, 6379).await,
        "(a) R cannot reach S1:6379, not linked"
    );
    assert!(
        !lab.tcp("r", S2_IP, 5432).await,
        "(a) R cannot reach S2, not linked"
    );
    assert!(
        lab.tcp("v", S2_IP, 5432).await,
        "V is open and still reaches S2"
    );

    // (c) names.
    let external = lab.lookup("r", "registry.example.test", RESOLVER_IP).await;
    assert!(
        external.contains("NXDOMAIN") && !external.contains(&PUBLIC_DNS_ANSWER.to_string()),
        "(c) the node resolver does not resolve external names for R: {external}"
    );
    let open = lab.lookup("v", "registry.example.test", RESOLVER_IP).await;
    assert!(
        open.contains(&PUBLIC_DNS_ANSWER.to_string()),
        "the same resolver still forwards for V: {open}"
    );
    let around = lab.lookup("r", "registry.example.test", PUBLIC_IP).await;
    assert!(
        !around.contains(&PUBLIC_DNS_ANSWER.to_string()),
        "(c) R cannot go around the node resolver: {around}"
    );
    let db = lab.lookup("r", "db.shop.temps.local", RESOLVER_IP).await;
    assert!(db.contains(S1_IP), "R resolves its linked service: {db}");
    let short = lab
        .lookup("r", "db.shop.grp.temps.local", RESOLVER_IP)
        .await;
    assert!(short.contains(S1_IP), "and its group short name: {short}");
    let cache = lab.lookup("r", "cache.shop.temps.local", RESOLVER_IP).await;
    assert!(
        cache.contains("NXDOMAIN") && !cache.contains(S2_IP),
        "R does not learn the unlinked service's address: {cache}"
    );

    // ---- Rollback: legacy removes node A's tables. ----
    assert_eq!(
        lab.set_mode(0, NodeMode::Legacy).await,
        PassOutcome::Removed {
            mode: NodeMode::Legacy
        }
    );
    assert!(
        lab.tcp("r", S2_IP, 5432).await,
        "legacy is today's behaviour again"
    );

    // ---- (b) Node B enforces: S2 accepts UDP 9999 from V only. ----
    assert!(matches!(
        lab.set_mode(1, NodeMode::Enforce).await,
        PassOutcome::Applied(_)
    ));
    lab.exec("v", &format!("echo legit-before | nc -u -w1 {S2_IP} 9999"))
        .await;
    assert!(
        udp_seen(lab, "legit-before").await.is_some(),
        "V reaches S2 by policy"
    );
    lab.exec("x", &format!("echo own-address | nc -u -w1 {S2_IP} 9999"))
        .await;
    assert!(
        udp_seen(lab, "own-address").await.is_none(),
        "X itself is not allowed in"
    );
    let spoof = |payload: &str| {
        format!(
            "ip addr add {V_IP}/32 dev eth0 2>&1; echo {payload} | nc -u -w1 -s {V_IP} {S2_IP} 9999; ip addr del {V_IP}/32 dev eth0"
        )
    };
    lab.exec("x", &spoof("spoofed-before")).await;
    let fooled = udp_seen(lab, "spoofed-before").await;
    assert_eq!(
        fooled.as_deref(),
        Some(format!("udp 9999 {V_IP} spoofed-before").as_str()),
        "control: without address binding on node A, X passes as V"
    );

    // Node A enforces again: its ports are bound to their addresses.
    assert!(matches!(
        lab.set_mode(0, NodeMode::Enforce).await,
        PassOutcome::Applied(_)
    ));
    lab.exec("x", &spoof("spoofed-after")).await;
    assert!(
        udp_seen(lab, "spoofed-after").await.is_none(),
        "(b) X cannot send as V once its port is bound to its own address"
    );
    lab.exec("v", &format!("echo legit-after | nc -u -w1 {S2_IP} 9999"))
        .await;
    assert!(
        udp_seen(lab, "legit-after").await.is_some(),
        "(b) V itself still gets in"
    );

    println!(
        "two-node proof passed: (a) unlinked service and port refused, (b) spoofed source \
         dropped, (c) external names refused; rollback restored today's behaviour"
    );
}

#[tokio::test]
async fn two_nodes_enforce_links_bindings_and_restricted_dns() {
    let Some(mut lab) = Lab::start().await else {
        return;
    };
    let outcome = AssertUnwindSafe(run(&mut lab)).catch_unwind().await;
    if outcome.is_err() {
        for name in ["resolver", "s2"] {
            println!("--- {name} logs ---\n{}", lab.logs(name).await);
        }
    }
    lab.cleanup().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
