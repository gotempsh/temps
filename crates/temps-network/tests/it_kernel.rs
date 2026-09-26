// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Kernel-touching integration tests for `temps-network`.
//!
//! These tests are gated behind two conditions:
//!   - `--features integration_kernel` (so they don't run during normal
//!     `cargo test`)
//!   - `target_os = "linux"` (the data-plane primitives only exist on Linux)
//!   - the `TEMPS_RUN_DIND_TESTS=1` env var (so accidental local invocation
//!     never tries to mutate the developer's host network)
//!
//! Each test asserts a real-kernel outcome:
//!   - bridges, vxlan devices, addresses, routes, nftables tables exist /
//!     don't exist after the relevant calls
//!   - cross-host scenarios (driven by the surrounding DinD harness) are
//!     covered by separate "node-a" / "node-b" tests that the harness
//!     orchestrates.
//!
//! Run from the DinD harness:
//!   crates/temps-network/tests/dind/run.sh

#![cfg(all(feature = "integration_kernel", target_os = "linux"))]

use ipnet::Ipv4Net;
use std::net::{IpAddr, Ipv4Addr};
use std::str::FromStr;
use std::time::Duration;
use tokio::process::Command;
use uuid::Uuid;

use temps_network::{NetworkConfig, NetworkManager, NodeAlloc, Peer, Transport};

// ---------------------------------------------------------------------------
// Test environment
// ---------------------------------------------------------------------------

struct Env {
    local_cidr: Ipv4Net,
    local_bridge_ip: IpAddr,
    local_underlay: IpAddr,
    peer_cidr: Ipv4Net,
    peer_underlay: IpAddr,
    underlay_dev: String,
}

impl Env {
    fn from_env() -> Self {
        Self {
            local_cidr: parse_env("TEMPS_IT_LOCAL_CIDR"),
            local_bridge_ip: parse_env("TEMPS_IT_LOCAL_BRIDGE_IP"),
            local_underlay: parse_env("TEMPS_IT_LOCAL_UNDERLAY"),
            peer_cidr: parse_env("TEMPS_IT_PEER_CIDR"),
            peer_underlay: parse_env("TEMPS_IT_PEER_UNDERLAY"),
            // Inside the DinD container, eth0 is the underlay-facing device.
            underlay_dev: std::env::var("TEMPS_IT_UNDERLAY_DEV").unwrap_or_else(|_| "eth0".into()),
        }
    }

    fn config(&self) -> NetworkConfig {
        NetworkConfig {
            bridge_name: "br-temps0".into(),
            docker_network_name: "temps-overlay".into(),
            transport: Transport::Vxlan {
                vni: 42,
                port: 4789,
            },
            // Run with the host's actual MTU - 50; alpine's `ip link show eth0`
            // reports 1500 inside docker bridges, so 1450 is correct.
            underlay_mtu: 1500,
            underlay_dev: self.underlay_dev.clone(),
            vxlan_dev_name: "vxlan-temps0".into(),
        }
    }

    fn alloc(&self) -> NodeAlloc {
        NodeAlloc {
            node_id: Uuid::new_v4(),
            compute_cidr: self.local_cidr,
            bridge_address: self.local_bridge_ip,
            underlay_address: self.local_underlay,
        }
    }

    fn peer(&self) -> Peer {
        Peer {
            node_id: Uuid::new_v4(),
            compute_cidr: self.peer_cidr,
            underlay_address: self.peer_underlay,
        }
    }
}

fn parse_env<T: FromStr>(key: &str) -> T
where
    <T as FromStr>::Err: std::fmt::Debug,
{
    let raw = std::env::var(key).unwrap_or_else(|_| panic!("missing env: {}", key));
    raw.parse()
        .unwrap_or_else(|e| panic!("bad value for {}: {:?} ({})", key, e, raw))
}

// ---------------------------------------------------------------------------
// Kernel-state helpers
// ---------------------------------------------------------------------------

async fn link_exists(name: &str) -> bool {
    Command::new("ip")
        .args(["link", "show", name])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

async fn link_mtu(name: &str) -> Option<u32> {
    let out = Command::new("ip")
        .args(["-d", "link", "show", name])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    // `... mtu 1450 qdisc ...`
    s.split_whitespace()
        .skip_while(|t| *t != "mtu")
        .nth(1)
        .and_then(|m| m.parse().ok())
}

async fn ip4_addr_present(link: &str, addr: &str) -> bool {
    let out = Command::new("ip")
        .args(["-4", "addr", "show", "dev", link])
        .output()
        .await
        .expect("ip addr show");
    String::from_utf8_lossy(&out.stdout).contains(addr)
}

async fn route_exists(cidr: &str) -> bool {
    let out = Command::new("ip")
        .args(["-4", "route", "show", cidr])
        .output()
        .await
        .expect("ip route show");
    !out.stdout.is_empty()
}

async fn nft_table_exists(family: &str, name: &str) -> bool {
    Command::new("nft")
        .args(["list", "table", family, name])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

async fn nft_table_contains(family: &str, name: &str, fragments: &[&str]) -> bool {
    let output = Command::new("nft")
        .args(["list", "table", family, name])
        .output()
        .await;
    matches!(output, Ok(output) if output.status.success() && {
        let rules = String::from_utf8_lossy(&output.stdout);
        fragments.iter().all(|fragment| rules.contains(fragment))
    })
}

async fn iptables_chain_contains(chain: &str, fragments: &[&str]) -> bool {
    let output = Command::new("iptables").args(["-S", chain]).output().await;
    matches!(output, Ok(output) if output.status.success() && {
        let rules = String::from_utf8_lossy(&output.stdout);
        fragments.iter().all(|fragment| rules.contains(fragment))
    })
}

async fn fdb_has_entry(dev: &str, dst: &str) -> bool {
    let out = Command::new("bridge")
        .args(["fdb", "show", "dev", dev])
        .output()
        .await
        .expect("bridge fdb show");
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines().any(|l| l.contains(&format!("dst {}", dst)))
}

async fn ip_forward_enabled() -> bool {
    let s = tokio::fs::read_to_string("/proc/sys/net/ipv4/ip_forward")
        .await
        .unwrap_or_default();
    s.trim() == "1"
}

async fn docker_network_exists(name: &str) -> bool {
    Command::new("docker")
        .args(["network", "inspect", name])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

async fn docker_container_running(name: &str) -> bool {
    let output = Command::new("docker")
        .args(["inspect", "-f", "{{.State.Running}}", name])
        .output()
        .await;
    matches!(
        output,
        Ok(output) if output.status.success()
            && String::from_utf8_lossy(&output.stdout).trim() == "true"
    )
}

// ---------------------------------------------------------------------------
// Per-test fixture: ensure clean state before AND after each test
// ---------------------------------------------------------------------------

async fn cleanup_all() {
    // Best-effort tear-down of anything a previous test may have left.
    while Command::new("iptables")
        .args([
            "-C",
            "DOCKER-USER",
            "-m",
            "comment",
            "--comment",
            "temps-overlay-forward-hook-v1",
            "-j",
            "TEMPS_OVERLAY_FORWARD",
        ])
        .output()
        .await
        .is_ok_and(|output| output.status.success())
    {
        let _ = Command::new("iptables")
            .args([
                "-D",
                "DOCKER-USER",
                "-m",
                "comment",
                "--comment",
                "temps-overlay-forward-hook-v1",
                "-j",
                "TEMPS_OVERLAY_FORWARD",
            ])
            .output()
            .await;
    }
    let _ = Command::new("iptables")
        .args(["-F", "TEMPS_OVERLAY_FORWARD"])
        .output()
        .await;
    let _ = Command::new("iptables")
        .args(["-X", "TEMPS_OVERLAY_FORWARD"])
        .output()
        .await;
    let _ = Command::new("docker")
        .args(["network", "rm", "temps-overlay"])
        .output()
        .await;
    let _ = Command::new("nft")
        .args(["delete", "table", "inet", "temps_network"])
        .output()
        .await;
    let _ = Command::new("ip")
        .args(["link", "del", "vxlan-temps0"])
        .output()
        .await;
    let _ = Command::new("ip")
        .args(["link", "del", "br-temps0"])
        .output()
        .await;
}

struct Cleanup;
impl Drop for Cleanup {
    fn drop(&mut self) {
        // Run cleanup synchronously on a dedicated thread so it always
        // executes even if the test panics.
        let h = std::thread::spawn(|| {
            if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                rt.block_on(cleanup_all());
            }
        });
        let _ = h.join();
    }
}

async fn fixture() -> (Env, NetworkManager, Cleanup) {
    cleanup_all().await;
    let env = Env::from_env();
    let mgr = NetworkManager::new(env.config()).expect("manager new");
    (env, mgr, Cleanup)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn detect_underlay_device_matches_default_route() {
    // Regression test for the bug where the underlay device was hardcoded
    // to "eth0" everywhere, breaking any host whose default-route
    // interface has a different name (e.g. cloud "predictable naming"
    // like enp6s0/ens5). Inside the DinD harness the default route goes
    // out `TEMPS_IT_UNDERLAY_DEV` (or "eth0" if unset), so auto-detection
    // must land on the same device the rest of the suite already assumes.
    cleanup_all().await;
    let env = Env::from_env();

    let detected = temps_network::detect_underlay_device()
        .await
        .expect("detect_underlay_device");

    assert_eq!(detected, env.underlay_dev);
}

#[tokio::test]
async fn detect_underlay_mtu_matches_kernel_link() {
    cleanup_all().await;
    let env = Env::from_env();
    let expected = link_mtu(&env.underlay_dev)
        .await
        .expect("underlay link must report an MTU");

    let detected = temps_network::detect_underlay_mtu(&env.underlay_dev)
        .await
        .expect("detect_underlay_mtu");

    assert_eq!(detected, expected);
}

#[tokio::test]
async fn bootstrap_creates_all_kernel_state() {
    let (env, mgr, _cleanup) = fixture().await;
    let alloc = env.alloc();
    let peer = env.peer();

    mgr.bootstrap(alloc.clone(), vec![peer.clone()])
        .await
        .expect("bootstrap");

    assert!(ip_forward_enabled().await, "net.ipv4.ip_forward must be 1");
    assert!(link_exists("br-temps0").await, "bridge must exist");
    assert!(link_exists("vxlan-temps0").await, "vxlan must exist");

    // MTU check: vxlan transport => bridge_mtu = underlay_mtu - 50 = 1450
    assert_eq!(link_mtu("br-temps0").await, Some(1450));
    assert_eq!(link_mtu("vxlan-temps0").await, Some(1450));

    assert!(
        ip4_addr_present("br-temps0", &env.local_bridge_ip.to_string()).await,
        "bridge must have its address"
    );

    assert!(
        route_exists(&env.peer_cidr.to_string()).await,
        "route to peer cidr must exist"
    );

    assert!(
        fdb_has_entry("vxlan-temps0", &env.peer_underlay.to_string()).await,
        "fdb entry for peer underlay must exist"
    );

    assert!(
        nft_table_exists("inet", "temps_network").await,
        "nftables table must exist"
    );
    assert!(
        nft_table_contains(
            "inet",
            "temps_network",
            &[
                &env.peer_cidr.to_string(),
                &env.local_cidr.to_string(),
                &env.local_bridge_ip.to_string(),
                "snat",
            ]
        )
        .await,
        "remote overlay traffic must be SNATed to the local bridge so a dual-network service replies over VXLAN"
    );
    assert!(
        iptables_chain_contains(
            "TEMPS_OVERLAY_FORWARD",
            &[
                "-m physdev",
                "--physdev-is-bridged",
                "--physdev-in vxlan-temps0",
                &env.peer_cidr.to_string(),
                &env.local_cidr.to_string(),
            ]
        )
        .await,
        "Docker's default-DROP FORWARD chain must permit scoped VXLAN ingress"
    );
    assert!(
        !iptables_chain_contains("TEMPS_OVERLAY_FORWARD", &["-o vxlan-temps0"]).await,
        "Temps must not bypass Docker isolation for locally spoofed VXLAN egress"
    );
}

#[tokio::test]
async fn bootstrap_is_idempotent() {
    let (env, mgr, _cleanup) = fixture().await;
    // Reuse the same alloc + peer across both calls — env.alloc() and
    // env.peer() each mint a fresh Uuid::new_v4(), and the manager treats
    // a different node_id as a different peer.
    let alloc = env.alloc();
    let peer = env.peer();
    mgr.bootstrap(alloc.clone(), vec![peer.clone()])
        .await
        .expect("first bootstrap");
    // Second call must not error and must leave kernel state intact.
    mgr.bootstrap(alloc, vec![peer])
        .await
        .expect("second bootstrap");
    assert!(link_exists("br-temps0").await);
    assert!(fdb_has_entry("vxlan-temps0", &env.peer_underlay.to_string()).await);
}

#[tokio::test]
async fn bootstrap_rejects_existing_vxlan_with_incompatible_topology() {
    let (env, mgr, _cleanup) = fixture().await;
    let output = Command::new("ip")
        .args([
            "link",
            "add",
            "vxlan-temps0",
            "type",
            "vxlan",
            "id",
            "99",
            "dev",
            &env.underlay_dev,
            "dstport",
            "4789",
            "nolearning",
        ])
        .output()
        .await
        .expect("create incompatible VXLAN device");
    assert!(
        output.status.success(),
        "create incompatible VXLAN device: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let error = mgr
        .bootstrap(env.alloc(), vec![])
        .await
        .expect_err("bootstrap must not adopt an incompatible VXLAN device");
    let message = error.to_string();
    assert!(message.contains("existing VXLAN topology does not match"));
    assert!(message.contains("vni=42"));
    assert!(message.contains("id 99"));
}

#[tokio::test]
async fn reconcile_peers_adds_new_peer() {
    let (env, mgr, _cleanup) = fixture().await;
    // Original peer must be the SAME object across bootstrap + reconcile so
    // the diff sees only the `extra` addition, not a remove+add of the
    // original.
    let original = env.peer();
    mgr.bootstrap(env.alloc(), vec![original.clone()])
        .await
        .expect("bootstrap");

    let extra = Peer {
        node_id: Uuid::new_v4(),
        compute_cidr: Ipv4Net::from_str("172.20.99.0/24").unwrap(),
        underlay_address: IpAddr::V4(Ipv4Addr::new(10, 123, 0, 99)),
    };
    let changed = mgr
        .reconcile_peers(vec![original, extra.clone()])
        .await
        .expect("reconcile add");
    assert!(changed);
    assert!(fdb_has_entry("vxlan-temps0", &extra.underlay_address.to_string()).await);
    assert!(route_exists(&extra.compute_cidr.to_string()).await);

    // Original peer untouched.
    assert!(fdb_has_entry("vxlan-temps0", &env.peer_underlay.to_string()).await);
    assert!(route_exists(&env.peer_cidr.to_string()).await);
}

#[tokio::test]
async fn reconcile_peers_removes_peer() {
    let (env, mgr, _cleanup) = fixture().await;
    let peer1 = env.peer();
    let peer2 = Peer {
        node_id: Uuid::new_v4(),
        compute_cidr: Ipv4Net::from_str("172.20.99.0/24").unwrap(),
        underlay_address: IpAddr::V4(Ipv4Addr::new(10, 123, 0, 99)),
    };
    mgr.bootstrap(env.alloc(), vec![peer1.clone(), peer2.clone()])
        .await
        .expect("bootstrap with 2 peers");

    let changed = mgr
        .reconcile_peers(vec![peer1.clone()])
        .await
        .expect("reconcile remove");
    assert!(changed);

    assert!(
        !fdb_has_entry("vxlan-temps0", &peer2.underlay_address.to_string()).await,
        "peer2 fdb entry should be gone"
    );
    assert!(
        !route_exists(&peer2.compute_cidr.to_string()).await,
        "peer2 route should be gone"
    );

    // Surviving peer untouched.
    assert!(fdb_has_entry("vxlan-temps0", &peer1.underlay_address.to_string()).await);
}

#[tokio::test]
async fn reconcile_peers_noop_on_unchanged() {
    let (env, mgr, _cleanup) = fixture().await;
    // Build the peer ONCE — env.peer() generates a fresh Uuid::new_v4() each
    // call, so calling it twice would feed reconcile two peers with
    // different node_ids and look like "remove + add", not a no-op.
    let p = env.peer();
    mgr.bootstrap(env.alloc(), vec![p.clone()])
        .await
        .expect("bootstrap");

    let changed = mgr.reconcile_peers(vec![p]).await.expect("reconcile noop");
    assert!(
        !changed,
        "reconcile with identical peer list must be a no-op"
    );
}

#[tokio::test]
async fn bootstrap_migrates_the_exact_legacy_forwarding_rule() {
    let (env, mgr, _cleanup) = fixture().await;
    let alloc = env.alloc();
    let peer = env.peer();
    mgr.bootstrap(alloc.clone(), vec![peer.clone()])
        .await
        .expect("initial bootstrap");

    let peer_cidr = peer.compute_cidr.to_string();
    let local_cidr = alloc.compute_cidr.to_string();
    let current_rule = [
        "-D",
        "TEMPS_OVERLAY_FORWARD",
        "-m",
        "physdev",
        "--physdev-is-bridged",
        "--physdev-in",
        "vxlan-temps0",
        "-s",
        &peer_cidr,
        "-d",
        &local_cidr,
        "-m",
        "comment",
        "--comment",
        "temps-overlay-forward-rule-v1",
        "-j",
        "ACCEPT",
    ];
    let deleted = Command::new("iptables")
        .args(current_rule)
        .output()
        .await
        .expect("delete current forwarding rule");
    assert!(deleted.status.success());

    let legacy_rule = [
        "-A",
        "TEMPS_OVERLAY_FORWARD",
        "-i",
        "vxlan-temps0",
        "-s",
        &peer_cidr,
        "-d",
        &local_cidr,
        "-m",
        "comment",
        "--comment",
        "temps-overlay-forward-rule-v1",
        "-j",
        "ACCEPT",
    ];
    let appended = Command::new("iptables")
        .args(legacy_rule)
        .output()
        .await
        .expect("append legacy forwarding rule");
    assert!(appended.status.success());

    mgr.bootstrap(alloc, vec![peer])
        .await
        .expect("upgrade bootstrap must migrate the owned legacy rule");

    let output = Command::new("iptables")
        .args(["-S", "TEMPS_OVERLAY_FORWARD"])
        .output()
        .await
        .expect("inspect migrated forwarding rules");
    assert!(output.status.success());
    let rules = String::from_utf8_lossy(&output.stdout);
    assert!(rules.contains("--physdev-in vxlan-temps0"));
    assert!(!rules.contains(" -i vxlan-temps0"));
}

#[tokio::test]
async fn teardown_removes_everything_and_is_idempotent() {
    let (env, mgr, _cleanup) = fixture().await;
    mgr.bootstrap(env.alloc(), vec![env.peer()])
        .await
        .expect("bootstrap");

    mgr.teardown().await.expect("first teardown");

    assert!(!link_exists("br-temps0").await, "bridge must be gone");
    assert!(!link_exists("vxlan-temps0").await, "vxlan must be gone");
    assert!(
        !nft_table_exists("inet", "temps_network").await,
        "nftables table must be gone"
    );
    assert!(
        !iptables_chain_contains("TEMPS_OVERLAY_FORWARD", &[]).await,
        "owned Docker forwarding chain must be gone"
    );

    // Second teardown must succeed silently.
    mgr.teardown().await.expect("second teardown");
}

#[tokio::test]
async fn bootstrap_creates_docker_network() {
    let (env, _mgr, _cleanup) = fixture().await;
    // Build a Docker client and call ensure_network directly so we test that
    // surface without depending on the manager fully wiring docker yet.
    let docker = bollard::Docker::connect_with_local_defaults().expect("docker connect");
    let alloc = env.alloc();
    let cfg = env.config();
    // We need the bridge to exist before docker can pin a network to it.
    // Bootstrap the kernel side first via the manager.
    let mgr = NetworkManager::new(cfg.clone()).unwrap();
    mgr.bootstrap(alloc.clone(), vec![])
        .await
        .expect("bootstrap");

    let id = temps_network::docker::ensure_network(&docker, &cfg, &alloc)
        .await
        .expect("ensure docker network");
    assert!(!id.is_empty());
    assert!(docker_network_exists("temps-overlay").await);

    // Idempotent: a second call with the same args returns the same id.
    let id2 = temps_network::docker::ensure_network(&docker, &cfg, &alloc)
        .await
        .expect("ensure docker network 2");
    assert_eq!(id, id2);

    // Cleanup the docker network specifically (cleanup_all also handles this).
    temps_network::docker::remove_network(&docker, &cfg)
        .await
        .expect("remove docker network");
}

#[tokio::test]
async fn docker_cidr_collision_is_detected() {
    let (env, _mgr, _cleanup) = fixture().await;
    let docker = bollard::Docker::connect_with_local_defaults().expect("docker connect");

    // Pre-create a Docker network on the same CIDR but with a different name —
    // that simulates "operator already has a network using this CIDR".
    let existing = "temps-it-collider";
    let _ = Command::new("docker")
        .args(["network", "rm", existing])
        .output()
        .await;
    let mut cmd = Command::new("docker");
    cmd.args([
        "network",
        "create",
        "--driver",
        "bridge",
        "--subnet",
        &env.local_cidr.to_string(),
        existing,
    ]);
    let out = cmd.output().await.expect("create collider network");
    assert!(
        out.status.success(),
        "should be able to create collider network: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let cfg = env.config();
    let alloc = env.alloc();
    let err = temps_network::docker::ensure_network(&docker, &cfg, &alloc)
        .await
        .expect_err("expected DockerCidrCollision");

    let msg = err.to_string();
    assert!(
        msg.contains(existing),
        "error must name the colliding network: {}",
        msg
    );
    assert!(msg.contains(&env.local_cidr.to_string()));

    let _ = Command::new("docker")
        .args(["network", "rm", existing])
        .output()
        .await;
}

#[tokio::test]
async fn invalid_config_rejected_before_kernel_calls() {
    let env = Env::from_env();
    let mut cfg = env.config();
    // Bridge name longer than IFNAMSIZ.
    cfg.bridge_name = "this-is-far-too-long-for-linux".into();
    let err = NetworkManager::new(cfg).expect_err("validation should fail");
    assert!(matches!(
        err,
        temps_network::NetworkError::InvalidConfig { .. }
    ));
}

#[tokio::test]
async fn bridge_address_outside_cidr_rejected() {
    let (env, mgr, _cleanup) = fixture().await;
    let mut alloc = env.alloc();
    alloc.bridge_address = IpAddr::V4(Ipv4Addr::new(10, 99, 99, 99));
    let err = mgr.bootstrap(alloc, vec![]).await.expect_err("bad alloc");
    assert!(matches!(
        err,
        temps_network::NetworkError::InvalidConfig { .. }
    ));
}

#[tokio::test]
async fn full_pool_collision_is_rejected_without_partial_kernel_state() {
    if std::env::var("TEMPS_IT_PHASE_TESTS").as_deref() != Ok("1") {
        return;
    }
    cleanup_all().await;
    let env = Env::from_env();
    let pool: Ipv4Net = parse_env("TEMPS_IT_CLUSTER_POOL");
    let existing_cidr =
        std::env::var("TEMPS_IT_EXISTING_CIDR").unwrap_or_else(|_| "10.240.99.0/24".into());
    let existing = "temps-it-existing-cidr";
    let _ = Command::new("docker")
        .args(["network", "rm", existing])
        .output()
        .await;
    let output = Command::new("docker")
        .args([
            "network",
            "create",
            "--driver",
            "bridge",
            "--subnet",
            &existing_cidr,
            existing,
        ])
        .output()
        .await
        .expect("create pre-existing Docker CIDR");
    assert!(
        output.status.success(),
        "create pre-existing CIDR: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let docker = bollard::Docker::connect_with_local_defaults().expect("docker connect");
    let error = temps_network::docker::preflight_network_for_pool(
        &docker,
        &env.config(),
        &env.alloc(),
        pool,
    )
    .await
    .expect_err("the full cluster pool must reject an occupied future peer subnet");
    assert!(error.to_string().contains(existing));
    assert!(!link_exists("br-temps0").await);
    assert!(!link_exists("vxlan-temps0").await);
    assert!(!docker_network_exists("temps-overlay").await);
    assert!(!nft_table_exists("inet", "temps_network").await);
    assert!(!route_exists(&env.peer_cidr.to_string()).await);

    let _ = Command::new("docker")
        .args(["network", "rm", existing])
        .output()
        .await;
}

// ---------------------------------------------------------------------------
// Cross-host scenario: a "bootstrap_only" test the DinD runner triggers
// once per node so each side ends up bootstrapped with its peer pointing
// to the other.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bootstrap_only() {
    // Don't auto-cleanup — the runner script will tear the container down at
    // the end and the second call to `docker exec` does inter-node ping
    // testing using the bootstrapped state.
    let env = Env::from_env();
    cleanup_all().await;
    let cfg = env.config();
    let alloc = env.alloc();
    let mgr = NetworkManager::new(cfg.clone()).expect("manager new");
    mgr.bootstrap(alloc.clone(), vec![env.peer()])
        .await
        .expect("bootstrap_only");
    // Give the kernel a moment to settle FDB / route additions.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Mirror what the production caller (temps-agent's network_sync.rs)
    // does right after bootstrap: create the Docker bridge network pinned
    // to the kernel bridge we just brought up. `NetworkManager::bootstrap`
    // deliberately stays pure of bollard (see `bootstrap_creates_docker_network`
    // above), so callers that need containers on the overlay — including the
    // DinD harness's cross-host container-ping step in run.sh — must create
    // it themselves.
    let docker = bollard::Docker::connect_with_local_defaults().expect("docker connect");
    temps_network::docker::ensure_network(&docker, &cfg, &alloc)
        .await
        .expect("ensure docker network");
}

/// Production-shaped lifecycle regression: the control plane starts alone,
/// remains alive, and reconciles a worker which physically starts later.
///
/// The DinD runner coordinates the two nodes with marker files in the shared
/// workspace. Keeping this test process alive is intentional: constructing a
/// second manager after the worker joins would only prove restart recovery,
/// not live late-worker reconciliation.
#[tokio::test]
#[cfg(feature = "control_plane")]
async fn control_plane_stays_running_and_reconciles_worker_later() {
    if std::env::var("TEMPS_IT_PHASE_TESTS").as_deref() != Ok("1") {
        return;
    }
    let env = Env::from_env();
    let pool: Ipv4Net = parse_env("TEMPS_IT_CLUSTER_POOL");
    let ready_file = parse_env::<String>("TEMPS_IT_CONTROL_PLANE_READY_FILE");
    let worker_file = parse_env::<String>("TEMPS_IT_WORKER_READY_FILE");
    let existing_app_network = parse_env::<String>("TEMPS_IT_EXISTING_APP_NETWORK");
    let existing_app_cidr = parse_env::<String>("TEMPS_IT_EXISTING_APP_CIDR");
    let existing_app_container = parse_env::<String>("TEMPS_IT_EXISTING_APP_CONTAINER");
    let existing_custom_route = parse_env::<String>("TEMPS_IT_EXISTING_CUSTOM_ROUTE");
    cleanup_all().await;
    let _ = tokio::fs::remove_file(&ready_file).await;
    let _ = tokio::fs::remove_file(&worker_file).await;
    let cfg = env.config();
    assert!(
        docker_network_exists(&existing_app_network).await,
        "the pre-existing control-plane app network must exist before setup"
    );
    assert!(route_exists(&existing_app_cidr).await);
    assert!(route_exists(&existing_custom_route).await);
    assert!(docker_container_running(&existing_app_container).await);
    temps_network::preflight_compute_pool_routes(&cfg, pool)
        .await
        .expect("safe cluster pool must not overlap host routes");
    let alloc = env.alloc();
    let mgr = NetworkManager::new(cfg.clone()).expect("manager new");
    mgr.bootstrap(alloc.clone(), vec![])
        .await
        .expect("bootstrap control plane without workers");
    let docker = bollard::Docker::connect_with_local_defaults().expect("docker connect");
    temps_network::docker::ensure_network(&docker, &cfg, &alloc)
        .await
        .expect("ensure control-plane Docker overlay");
    assert!(!route_exists(&env.peer_cidr.to_string()).await);
    assert!(docker_network_exists(&existing_app_network).await);
    assert!(route_exists(&existing_app_cidr).await);
    assert!(route_exists(&existing_custom_route).await);
    assert!(docker_container_running(&existing_app_container).await);
    tokio::fs::write(&ready_file, b"ready\n")
        .await
        .expect("publish control-plane ready marker");

    // Node B is created only after this marker. A cold CI runner may need to
    // install and compile Rust in that new container, which is not a network
    // convergence failure and must not make this lifecycle test flaky.
    let worker_ready_timeout = std::env::var("TEMPS_IT_WORKER_READY_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(900);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(worker_ready_timeout);
    while tokio::fs::metadata(&worker_file).await.is_err() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "worker did not become ready before the lifecycle-test deadline"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let peer = env.peer();
    let changed = temps_network::control_plane::reconcile_peer_snapshot(
        &mgr,
        &docker,
        &cfg,
        &alloc,
        pool,
        vec![peer.clone()],
    )
    .await
    .expect("reconcile late worker");
    assert!(changed);
    assert!(route_exists(&peer.compute_cidr.to_string()).await);
    assert!(fdb_has_entry("vxlan-temps0", &peer.underlay_address.to_string()).await);
    assert!(docker_network_exists(&existing_app_network).await);
    assert!(route_exists(&existing_app_cidr).await);
    assert!(route_exists(&existing_custom_route).await);
    assert!(docker_container_running(&existing_app_container).await);
}
