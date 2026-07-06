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

// ---------------------------------------------------------------------------
// Per-test fixture: ensure clean state before AND after each test
// ---------------------------------------------------------------------------

async fn cleanup_all() {
    // Best-effort tear-down of anything a previous test may have left.
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
