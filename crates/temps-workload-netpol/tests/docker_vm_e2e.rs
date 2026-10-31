// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! End-to-end enforcement against a real Docker engine (ADR-055 §17).
//!
//! Real containers on a real bridge, rules applied with the executor this
//! platform would use in production: the Docker VM on macOS (colima,
//! Docker Desktop), the host on Linux. Every assertion is preceded by the
//! same probe without the policy, so a pass proves the rules did the
//! blocking and not a missing route.
//!
//! Each test owns a network on a random subnet and an nftables table with
//! a random name, so the tests can run in parallel and never touch the
//! table the plane uses. Skips when Docker or `alpine:3.20` is missing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration-test helpers: panicking is how a test fails"
)]
#![allow(
    clippy::disallowed_methods,
    reason = "integration test: reads fixture configuration from the environment"
)]
use std::net::IpAddr;
use std::panic::AssertUnwindSafe;

use bollard::exec::{StartExecOptions, StartExecResults};
use bollard::models::{
    ContainerCreateBody, ExecConfig, HostConfig, Ipam, IpamConfig, NetworkCreateRequest,
};
use bollard::query_parameters::{CreateContainerOptionsBuilder, RemoveContainerOptions};
use bollard::Docker;
use futures::{FutureExt, StreamExt};
use rand::RngExt;
use temps_workload::{WorkloadKind, WorkloadRef};
use temps_workload_netpol::{
    choose_executor, compile, Action, Direction, IdentityMap, L4Protocol, NftError, NftExecutor,
    Origin, Peer, PortRange, Profile, Rule, TableName, WorkloadPolicy,
};

const IMAGE: &str = "alpine:3.20";

/// Listens on 5432 and 6379, answering `ok-<port>` to every connection.
const SERVER: &str =
    "for p in 5432 6379; do (while true; do echo ok-$p | nc -l -p $p; done) & done; sleep 600";

struct Lab {
    docker: Docker,
    network: String,
    containers: Vec<String>,
    table: TableName,
    executor: Box<dyn NftExecutor>,
}

impl Lab {
    async fn start() -> Option<Lab> {
        let docker = Docker::connect_with_defaults().ok()?;
        if docker.ping().await.is_err() {
            println!("Docker not available, skipping");
            return None;
        }
        if docker.inspect_image(IMAGE).await.is_err() {
            println!("{IMAGE} not present, skipping");
            return None;
        }
        let tag: u32 = rand::rng().random_range(0..u32::MAX);
        let network = format!("temps-netpol-e2e-{tag:08x}");
        let subnet = format!(
            "10.{}.{}.0/24",
            rand::rng().random_range(200..=250u8),
            rand::rng().random_range(0..=255u8)
        );
        docker
            .create_network(NetworkCreateRequest {
                name: network.clone(),
                driver: Some("bridge".to_string()),
                ipam: Some(Ipam {
                    config: Some(vec![IpamConfig {
                        subnet: Some(subnet),
                        ..Default::default()
                    }]),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await
            .expect("create the test network");
        let table = TableName::new(format!("temps_netpol_e2e_{tag:08x}")).expect("table name");
        let daemon_is_local = std::env::var("DOCKER_HOST")
            .map(|h| h.starts_with("unix://"))
            .unwrap_or(true);
        let executor = choose_executor(docker.clone(), daemon_is_local);
        Some(Lab {
            docker,
            network,
            containers: Vec::new(),
            table,
            executor,
        })
    }

    /// Starts a container running `script` and returns its address.
    async fn container(&mut self, role: &str, script: &str, host_network: bool) -> Option<IpAddr> {
        let name = format!("{}-{role}", self.network);
        self.docker
            .create_container(
                Some(CreateContainerOptionsBuilder::new().name(&name).build()),
                ContainerCreateBody {
                    image: Some(IMAGE.to_string()),
                    cmd: Some(vec!["sh".into(), "-c".into(), script.into()]),
                    host_config: Some(HostConfig {
                        network_mode: Some(if host_network {
                            "host".to_string()
                        } else {
                            self.network.clone()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .expect("create a test container");
        self.containers.push(name.clone());
        self.docker
            .start_container(&name, None)
            .await
            .expect("start a test container");
        if host_network {
            return None;
        }
        let inspect = self
            .docker
            .inspect_container(&name, None)
            .await
            .expect("inspect");
        let address = inspect
            .network_settings
            .and_then(|s| s.networks)
            .and_then(|n| n.get(&self.network).cloned())
            .and_then(|e| e.ip_address)
            .expect("the container has an address on the test network");
        Some(address.parse().expect("an IP address"))
    }

    async fn exec(&self, role: &str, script: &str) -> String {
        let name = format!("{}-{role}", self.network);
        let exec = self
            .docker
            .create_exec(
                &name,
                ExecConfig {
                    cmd: Some(vec!["sh".into(), "-c".into(), script.into()]),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .expect("create exec");
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
            .expect("start exec");
        let mut text = String::new();
        if let StartExecResults::Attached { mut output, .. } = started {
            while let Some(Ok(chunk)) = output.next().await {
                text.push_str(&String::from_utf8_lossy(&chunk.into_bytes()));
            }
        }
        text
    }

    /// Whether `role` gets an answer from `addr:port`.
    async fn reaches(&self, role: &str, addr: IpAddr, port: u16) -> bool {
        // Wait for the server's listener on first use.
        let script = format!("echo | nc -w 2 {addr} {port}");
        let out = self.exec(role, &script).await;
        out.contains(&format!("ok-{port}"))
    }

    async fn reaches_internet(&self, role: &str) -> bool {
        // A TCP connect, not a request: busybox nc exits on stdin EOF
        // before a reply would arrive.
        let out = self.exec(role, "nc -v -w 3 1.1.1.1 443 </dev/null").await;
        out.contains("open")
    }

    async fn apply(&self, policies: &[WorkloadPolicy], ids: &IdentityMap) -> Result<(), NftError> {
        let compiled = compile(&self.table, policies, ids).expect("the policy compiles");
        self.executor.apply(&compiled.ruleset).await
    }

    async fn cleanup(&self) {
        let _ = self.executor.remove(&self.table).await;
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
        let _ = self.docker.remove_network(&self.network).await;
    }
}

/// Runs `body` and always cleans up, then re-raises a failure.
async fn with_lab<F>(body: F)
where
    F: for<'a> FnOnce(&'a mut Lab) -> futures::future::BoxFuture<'a, ()>,
{
    let Some(mut lab) = Lab::start().await else {
        return;
    };
    let outcome = AssertUnwindSafe(body(&mut lab)).catch_unwind().await;
    lab.cleanup().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// The bridge needs a moment before the first connection succeeds.
async fn wait_until_reachable(lab: &Lab, role: &str, addr: IpAddr, port: u16) {
    for _ in 0..20 {
        if lab.reaches(role, addr, port).await {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    panic!("{role} never reached {addr}:{port} before any policy was applied");
}

fn sandbox(id: i64) -> WorkloadRef {
    WorkloadRef::new(WorkloadKind::Sandbox, id)
}

fn service(id: i64) -> WorkloadRef {
    WorkloadRef::new(WorkloadKind::Service, id)
}

fn allow(id: i64, direction: Direction, peer: WorkloadRef, port: u16) -> Rule {
    Rule {
        id,
        direction,
        peer: Peer::Workload(peer),
        ports: Some(PortRange::single(port)),
        protocol: L4Protocol::Tcp,
        action: Action::Allow,
        origin: Origin::Link,
    }
}

#[tokio::test]
async fn restricted_egress_reaches_only_the_linked_port() {
    with_lab(|lab| {
        async move {
            let db = lab.container("db", SERVER, false).await.unwrap();
            let other = lab.container("other", SERVER, false).await.unwrap();
            let client = lab.container("client", "sleep 600", false).await.unwrap();
            wait_until_reachable(lab, "client", db, 5432).await;
            wait_until_reachable(lab, "client", other, 5432).await;

            // Before: everything is reachable.
            assert!(lab.reaches("client", db, 6379).await);
            let internet = lab.reaches_internet("client").await;
            if !internet {
                println!(
                    "no internet from the Docker network: the internet assertions are skipped"
                );
            }

            let ids = IdentityMap::new()
                .with(sandbox(1), client)
                .with(service(9), db)
                .with(service(10), other);
            let policy = WorkloadPolicy {
                subject: sandbox(1),
                egress: Profile::Restricted,
                ingress: Profile::Open,
                rules: vec![allow(1, Direction::Egress, service(9), 5432)],
            };
            lab.apply(&[policy], &ids).await.expect("apply the policy");

            assert!(
                lab.reaches("client", db, 5432).await,
                "the linked port must stay reachable"
            );
            assert!(
                !lab.reaches("client", db, 6379).await,
                "another port of the peer must be refused"
            );
            assert!(
                !lab.reaches("client", other, 5432).await,
                "an unlinked peer must be refused"
            );
            if internet {
                assert!(
                    !lab.reaches_internet("client").await,
                    "the internet must be refused"
                );
            }
            // The peers themselves are not restricted.
            assert!(lab.reaches("other", db, 6379).await);

            // Removing the policy restores everything.
            lab.executor
                .remove(&lab.table)
                .await
                .expect("remove the table");
            assert!(lab.reaches("client", db, 6379).await);
            assert!(lab.reaches("client", other, 5432).await);
            if internet {
                assert!(lab.reaches_internet("client").await);
            }
        }
        .boxed()
    })
    .await;
}

#[tokio::test]
async fn ingress_isolation_admits_linked_peers_and_the_host() {
    with_lab(|lab| {
        async move {
            let db = lab.container("db", SERVER, false).await.unwrap();
            let linked = lab.container("linked", "sleep 600", false).await.unwrap();
            let stranger = lab.container("stranger", "sleep 600", false).await.unwrap();
            lab.container("host", "sleep 600", true).await;
            wait_until_reachable(lab, "linked", db, 5432).await;
            assert!(lab.reaches("stranger", db, 5432).await);
            assert!(
                lab.reaches("host", db, 5432).await,
                "the host reaches the bridge before any policy"
            );

            let ids = IdentityMap::new()
                .with(service(9), db)
                .with(sandbox(1), linked)
                .with(sandbox(2), stranger);
            let policy = WorkloadPolicy {
                subject: service(9),
                egress: Profile::Open,
                ingress: Profile::Restricted,
                rules: vec![allow(1, Direction::Ingress, sandbox(1), 5432)],
            };
            lab.apply(&[policy], &ids).await.expect("apply the policy");

            assert!(
                lab.reaches("linked", db, 5432).await,
                "the linked peer must get in"
            );
            assert!(
                !lab.reaches("linked", db, 6379).await,
                "only on the linked port"
            );
            assert!(
                !lab.reaches("stranger", db, 5432).await,
                "an unlinked peer must be refused"
            );
            assert!(
                lab.reaches("host", db, 5432).await,
                "the host (proxy, health checks) must still get in"
            );
            assert!(lab.reaches("host", db, 6379).await);
        }
        .boxed()
    })
    .await;
}

#[tokio::test]
async fn both_ends_must_agree_and_a_deny_beats_an_allow() {
    with_lab(|lab| {
        async move {
            let db = lab.container("db", SERVER, false).await.unwrap();
            let client = lab.container("client", "sleep 600", false).await.unwrap();
            wait_until_reachable(lab, "client", db, 5432).await;
            let ids = IdentityMap::new()
                .with(service(9), db)
                .with(sandbox(1), client);

            // The client may send, the server does not admit it.
            let sender = WorkloadPolicy {
                subject: sandbox(1),
                egress: Profile::Restricted,
                ingress: Profile::Open,
                rules: vec![allow(1, Direction::Egress, service(9), 5432)],
            };
            let receiver = WorkloadPolicy {
                subject: service(9),
                egress: Profile::Open,
                ingress: Profile::Restricted,
                rules: vec![],
            };
            lab.apply(&[sender.clone(), receiver.clone()], &ids)
                .await
                .unwrap();
            assert!(
                !lab.reaches("client", db, 5432).await,
                "the receiver's ingress must also allow it"
            );

            let mut receiver = receiver;
            receiver
                .rules
                .push(allow(2, Direction::Ingress, sandbox(1), 5432));
            lab.apply(&[sender.clone(), receiver.clone()], &ids)
                .await
                .unwrap();
            assert!(
                lab.reaches("client", db, 5432).await,
                "both ends allow it now"
            );

            // A deny with a higher id still wins over the allow.
            let mut sender = sender;
            sender.rules.push(Rule {
                id: 99,
                direction: Direction::Egress,
                peer: Peer::Cidr(temps_workload_netpol::Cidr::host(db)),
                ports: None,
                protocol: L4Protocol::Any,
                action: Action::Deny,
                origin: Origin::Operator,
            });
            lab.apply(&[sender, receiver], &ids).await.unwrap();
            assert!(!lab.reaches("client", db, 5432).await, "the deny must win");
        }
        .boxed()
    })
    .await;
}

#[tokio::test]
async fn a_rejected_ruleset_leaves_the_previous_policy_in_force() {
    with_lab(|lab| {
        async move {
            let db = lab.container("db", SERVER, false).await.unwrap();
            let client = lab.container("client", "sleep 600", false).await.unwrap();
            wait_until_reachable(lab, "client", db, 5432).await;
            let ids = IdentityMap::new().with(service(9), db).with(sandbox(1), client);
            let policy = WorkloadPolicy {
                subject: sandbox(1),
                egress: Profile::Restricted,
                ingress: Profile::Open,
                rules: vec![allow(1, Direction::Egress, service(9), 5432)],
            };
            lab.apply(&[policy], &ids).await.unwrap();
            let before = lab.executor.list(&lab.table).await.unwrap().expect("the table exists");

            // The replacement fails half way: nothing of it may land.
            let broken = format!(
                "table inet {t}\ndelete table inet {t}\ntable inet {t} {{\n\tchain forward {{\n\t\ttype filter hook forward priority -190; policy accept;\n\t\tthis is not nft\n\t}}\n}}\n",
                t = lab.table
            );
            let error = lab.executor.apply(&broken).await.expect_err("nft must reject it");
            assert!(matches!(error, NftError::Rejected { .. }), "{error}");

            let after = lab.executor.list(&lab.table).await.unwrap().expect("the table survived");
            assert_eq!(before, after, "the previous ruleset must be untouched");
            assert!(lab.reaches("client", db, 5432).await);
            assert!(!lab.reaches("client", db, 6379).await, "the previous policy still applies");
        }
        .boxed()
    })
    .await;
}

#[tokio::test]
async fn rules_follow_the_identity_when_the_address_changes() {
    with_lab(|lab| {
        async move {
            let db = lab.container("db", SERVER, false).await.unwrap();
            let db2 = lab.container("db2", SERVER, false).await.unwrap();
            let client = lab.container("client", "sleep 600", false).await.unwrap();
            wait_until_reachable(lab, "client", db2, 5432).await;
            let policy = WorkloadPolicy {
                subject: sandbox(1),
                egress: Profile::Restricted,
                ingress: Profile::Open,
                rules: vec![allow(1, Direction::Egress, service(9), 5432)],
            };

            // service/9 is at `db`.
            let ids = IdentityMap::new()
                .with(service(9), db)
                .with(sandbox(1), client);
            lab.apply(std::slice::from_ref(&policy), &ids)
                .await
                .unwrap();
            assert!(lab.reaches("client", db, 5432).await);
            assert!(!lab.reaches("client", db2, 5432).await);

            // It moved to `db2`: same rule, new address.
            let ids = IdentityMap::new()
                .with(service(9), db2)
                .with(sandbox(1), client);
            lab.apply(&[policy], &ids).await.unwrap();
            assert!(lab.reaches("client", db2, 5432).await);
            assert!(!lab.reaches("client", db, 5432).await);
        }
        .boxed()
    })
    .await;
}
