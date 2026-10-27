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

/// `ip -d -o link show dev <name>`: the device's full detail on one line.
async fn link_detail(name: &str) -> String {
    let output = Command::new("ip")
        .args(["-d", "-o", "link", "show", "dev", name])
        .output()
        .await
        .expect("ip link show");
    String::from_utf8_lossy(&output.stdout).into_owned()
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
    // Devices a test set up beside Temps' own: a foreign VXLAN and the
    // dummy parent standing in for a previous underlay.
    for netns in [PEER_NETNS, FOREIGN_NETNS] {
        let _ = Command::new("ip")
            .args(["netns", "del", netns])
            .output()
            .await;
    }
    for device in ["temps-it-vx0", "temps-it-d0", PEER_VETH, FOREIGN_VETH] {
        let _ = Command::new("ip")
            .args(["link", "del", device])
            .output()
            .await;
    }
    let _ = Command::new("ip")
        .args(["link", "del", "br-temps0"])
        .output()
        .await;
    let _ = Command::new("ip")
        .args(["link", "del", OLD_UNDERLAY])
        .output()
        .await;
}

/// Dummy parent device for the underlay-migration test.
const OLD_UNDERLAY: &str = "temps-old-ul0";

/// Network namespace standing in for a peer on the underlay network, and the
/// host's end of the veth that reaches it.
const PEER_NETNS: &str = "temps-it-peer";
const PEER_VETH: &str = "temps-it-u0";

/// Network namespace standing in for a host on another network (the public
/// one), and the host's end of the veth that reaches it.
const FOREIGN_NETNS: &str = "temps-it-far";
const FOREIGN_VETH: &str = "temps-it-h0";

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
        !nft_table_contains("inet", "temps_network", &["snat"]).await,
        "remote overlay traffic keeps its source address (its network policy identity); \
         dual-network containers reply over VXLAN through their injected peer routes"
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
    let detail = link_detail("vxlan-temps0").await;
    assert!(
        detail.contains(&format!(" local {} ", env.local_underlay)),
        "vxlan must source its packets from the underlay address: {detail}"
    );
    assert!(
        nft_table_contains(
            "inet",
            "temps_network",
            &[
                "@vxlan_peers",
                "fib saddr . iif oif exists",
                "udp dport 4789 @th,96,24"
            ]
        )
        .await,
        "VXLAN must be admitted only from known peers"
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
async fn bootstrap_recreates_existing_vxlan_with_a_changed_topology() {
    // A VXLAN device under Temps' name built for another parent, VNI or port
    // (the underlay moved onto the WireGuard mesh) is never adopted as is:
    // its parent cannot change in place, so bootstrap replaces it with the
    // requested topology.
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

    let peer = env.peer();
    mgr.bootstrap(env.alloc(), vec![peer])
        .await
        .expect("bootstrap replaces a VXLAN device with another topology");

    let detail = link_detail("vxlan-temps0").await;
    let tokens: Vec<&str> = detail.split_whitespace().collect();
    let has_pair = |key: &str, value: &str| {
        tokens
            .windows(2)
            .any(|pair| pair[0] == key && pair[1] == value)
    };
    assert!(has_pair("id", "42"), "VNI not replaced: {detail}");
    assert!(!has_pair("id", "99"), "old VNI kept: {detail}");
    assert!(has_pair("dev", &env.underlay_dev), "parent: {detail}");
    assert!(has_pair("master", "br-temps0"), "not re-enslaved: {detail}");
    assert!(
        fdb_has_entry("vxlan-temps0", &env.peer_underlay.to_string()).await,
        "the replacement's FDB was not repopulated"
    );
}

#[tokio::test]
async fn a_replacement_that_cannot_be_built_keeps_the_working_vxlan() {
    // The topology changed to an underlay device that does not exist (yet):
    // the replacement cannot be built, so the device carrying the overlay
    // today must survive, still on the bridge, with its FDB.
    let (env, mgr, _cleanup) = fixture().await;
    let alloc = env.alloc();
    let peer = env.peer();
    mgr.bootstrap(alloc.clone(), vec![peer.clone()])
        .await
        .expect("first bootstrap");

    let mut moved = env.config();
    moved.underlay_dev = "temps-it-gone0".into();
    let moved = NetworkManager::new(moved).expect("manager for the moved underlay");
    let error = moved
        .bootstrap(alloc, vec![peer])
        .await
        .expect_err("no replacement on a missing underlay");
    assert!(
        error.to_string().contains("the existing device is kept"),
        "{error}"
    );

    let detail = link_detail("vxlan-temps0").await;
    let tokens: Vec<&str> = detail.split_whitespace().collect();
    let has_pair = |key: &str, value: &str| {
        tokens
            .windows(2)
            .any(|pair| pair[0] == key && pair[1] == value)
    };
    assert!(has_pair("id", "42"), "{detail}");
    assert!(has_pair("dev", &env.underlay_dev), "{detail}");
    assert!(has_pair("master", "br-temps0"), "{detail}");
    assert!(fdb_has_entry("vxlan-temps0", &env.peer_underlay.to_string()).await);
}

#[tokio::test]
async fn bootstrap_sets_the_local_address_of_an_existing_vxlan_in_place() {
    // Every node set up before the local address was set has a device like
    // this one. An upgrade sets the address on it in place: same device, same
    // FDB, so the overlay is not interrupted.
    let (env, mgr, _cleanup) = fixture().await;
    ip(&[
        "link",
        "add",
        "vxlan-temps0",
        "type",
        "vxlan",
        "id",
        "42",
        "dev",
        &env.underlay_dev,
        "dstport",
        "4789",
        "nolearning",
    ])
    .await;
    // An entry bootstrap would not write itself, so only an untouched FDB
    // still has it afterwards.
    let kept = "127.0.0.9";
    let output = Command::new("bridge")
        .args([
            "fdb",
            "append",
            "00:00:00:00:00:00",
            "dev",
            "vxlan-temps0",
            "dst",
            kept,
        ])
        .output()
        .await
        .expect("spawn bridge");
    assert!(
        output.status.success(),
        "bridge fdb append: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!link_detail("vxlan-temps0").await.contains(" local "));
    let index = link_index("vxlan-temps0").await;

    mgr.bootstrap(env.alloc(), vec![env.peer()])
        .await
        .expect("bootstrap sets the local address");

    let detail = link_detail("vxlan-temps0").await;
    assert!(
        detail.contains(&format!(" local {} ", env.local_underlay)),
        "local address not set: {detail}"
    );
    assert_eq!(
        link_index("vxlan-temps0").await,
        index,
        "device was rebuilt"
    );
    assert!(
        fdb_has_entry("vxlan-temps0", kept).await,
        "FDB was not kept"
    );
    assert!(
        detail.contains("nolearning"),
        "learning was turned on: {detail}"
    );
    assert!(
        detail.contains("master br-temps0"),
        "not enslaved: {detail}"
    );
    assert!(fdb_has_entry("vxlan-temps0", &env.peer_underlay.to_string()).await);

    // Now simply compatible.
    mgr.bootstrap(env.alloc(), vec![env.peer()])
        .await
        .expect("second bootstrap");
    assert_eq!(
        link_index("vxlan-temps0").await,
        index,
        "device was rebuilt"
    );
}

/// Packets the VXLAN device has decapsulated so far.
async fn vxlan_rx_packets() -> u64 {
    tokio::fs::read_to_string("/sys/class/net/vxlan-temps0/statistics/rx_packets")
        .await
        .expect("read vxlan rx_packets")
        .trim()
        .parse()
        .expect("rx_packets is a number")
}

/// Packets the rule dropping this overlay's VXLAN has dropped so far.
async fn vxlan_drops() -> u64 {
    let output = Command::new("nft")
        .args(["list", "chain", "inet", "temps_network", "input"])
        .output()
        .await
        .expect("list the input chain");
    let listing = String::from_utf8_lossy(&output.stdout);
    let rule = listing
        .lines()
        .find(|line| {
            // The VNI prints in decimal or hex depending on the nft version.
            line.trim_start().starts_with("udp dport 4789 @th,96,24 ") && line.ends_with("drop")
        })
        .unwrap_or_else(|| panic!("no VXLAN drop rule in:\n{listing}"));
    let tokens: Vec<&str> = rule.split_whitespace().collect();
    tokens
        .windows(2)
        .find(|pair| pair[0] == "packets")
        .and_then(|pair| pair[1].parse().ok())
        .unwrap_or_else(|| panic!("no packet count in: {rule}"))
}

/// What became of one injected VXLAN frame.
#[derive(Debug, PartialEq, Eq)]
struct Injection {
    /// The VXLAN device decapsulated something meanwhile.
    decapsulated: bool,
    /// The overlay's drop rule dropped it.
    dropped: bool,
}

/// Send `frame` from `source` to `destination`:4789 from inside network
/// namespace `netns`. Entering a namespace affects only the calling thread,
/// so the send happens on a thread of its own.
///
/// Senders always live in a namespace the test built, holding addresses the
/// test chose: the harness's own addresses differ between the single- and
/// two-node runs, and the two-node peer is not even on this host.
fn send_udp(netns: &str, source: IpAddr, destination: IpAddr, frame: Vec<u8>) {
    let netns = netns.to_owned();
    std::thread::spawn(move || {
        let namespace = std::fs::File::open(format!("/run/netns/{netns}"))
            .unwrap_or_else(|error| panic!("open netns {netns}: {error}"));
        nix::sched::setns(&namespace, nix::sched::CloneFlags::CLONE_NEWNET)
            .unwrap_or_else(|error| panic!("enter netns {netns}: {error}"));
        let socket = std::net::UdpSocket::bind((source, 0))
            .unwrap_or_else(|error| panic!("bind {source}: {error}"));
        socket
            .send_to(&frame, (destination, 4789))
            .unwrap_or_else(|error| panic!("send to {destination}: {error}"));
    })
    .join()
    .expect("injection thread");
}

/// Send one VXLAN-encapsulated broadcast ARP frame for `vni` from `source` to
/// `destination`:4789, from inside network namespace `netns`.
async fn inject_vxlan(netns: &str, source: IpAddr, destination: IpAddr, vni: u32) -> Injection {
    let (rx, drops) = (vxlan_rx_packets().await, vxlan_drops().await);
    let vni = vni.to_be_bytes();
    let mut frame = vec![0x08, 0, 0, 0, vni[1], vni[2], vni[3], 0];
    frame.extend_from_slice(&[0xff; 6]);
    frame.extend_from_slice(&[0x02, 0, 0, 0, 0, 0x99]);
    frame.extend_from_slice(&[0x08, 0x06, 0, 1, 0x08, 0, 6, 4, 0, 1]);
    frame.extend_from_slice(&[0x02, 0, 0, 0, 0, 0x99, 172, 20, 1, 99]);
    frame.extend_from_slice(&[0; 6]);
    frame.extend_from_slice(&[172, 20, 1, 1]);
    send_udp(netns, source, destination, frame);
    tokio::time::sleep(Duration::from_millis(300)).await;
    Injection {
        decapsulated: vxlan_rx_packets().await > rx,
        dropped: vxlan_drops().await == drops + 1,
    }
}

/// Restores `net.ipv4.conf.all.rp_filter` when dropped.
struct RpFilter(String);
impl Drop for RpFilter {
    fn drop(&mut self) {
        let _ = std::fs::write("/proc/sys/net/ipv4/conf/all/rp_filter", &self.0);
    }
}

/// Turn the kernel's own reverse-path filter off for `device` (the effective
/// value is the larger of `all` and the device's), so that what a test sees
/// dropped is dropped by the overlay's rules and not before them — as on the
/// many hosts that run it loose or off.
fn disable_rp_filter(device: &str) -> RpFilter {
    let all = "/proc/sys/net/ipv4/conf/all/rp_filter";
    let previous = std::fs::read_to_string(all).expect("read rp_filter");
    std::fs::write(all, "0").expect("disable rp_filter");
    std::fs::write(format!("/proc/sys/net/ipv4/conf/{device}/rp_filter"), "0")
        .expect("disable the device's rp_filter");
    RpFilter(previous.trim().to_string())
}

#[tokio::test]
async fn a_peer_address_forged_from_another_network_is_dropped() {
    // A real peer on the underlay network, and a host on another network —
    // the public one. The kernel binds the VXLAN socket to the wildcard
    // address whatever the device's local address, so without the ingress
    // rules it would decapsulate everything below: the peer's address claimed
    // from the other network, VXLAN to the public address, and an unknown
    // sender's VXLAN to the underlay address. Only the peer's own frame may
    // reach the overlay. A frame for another VNI is no business of this
    // overlay's rules, whoever sends it.
    //
    // A rejected frame is recognised by the drop rule's counter (a drop is
    // final: such a frame never reaches the socket); the receive count is the
    // positive control.
    let (env, mgr, _cleanup) = fixture().await;
    let underlay: IpAddr = "10.123.45.1".parse().unwrap();
    let peer: IpAddr = "10.123.45.2".parse().unwrap();
    let public: IpAddr = "198.51.100.1".parse().unwrap();
    let foreign: IpAddr = "198.51.100.2".parse().unwrap();
    for args in [
        vec!["netns", "add", PEER_NETNS],
        vec!["netns", "add", FOREIGN_NETNS],
        vec![
            "link",
            "add",
            PEER_VETH,
            "type",
            "veth",
            "peer",
            "name",
            "temps-it-p0",
            "netns",
            PEER_NETNS,
        ],
        vec!["addr", "add", "10.123.45.1/24", "dev", PEER_VETH],
        vec!["link", "set", PEER_VETH, "up"],
        vec![
            "-n",
            PEER_NETNS,
            "addr",
            "add",
            "10.123.45.2/24",
            "dev",
            "temps-it-p0",
        ],
        vec!["-n", PEER_NETNS, "link", "set", "temps-it-p0", "up"],
        vec![
            "link",
            "add",
            FOREIGN_VETH,
            "type",
            "veth",
            "peer",
            "name",
            "temps-it-a0",
            "netns",
            FOREIGN_NETNS,
        ],
        vec!["addr", "add", "198.51.100.1/24", "dev", FOREIGN_VETH],
        vec!["link", "set", FOREIGN_VETH, "up"],
        vec![
            "-n",
            FOREIGN_NETNS,
            "addr",
            "add",
            "198.51.100.2/24",
            "dev",
            "temps-it-a0",
        ],
        vec!["-n", FOREIGN_NETNS, "link", "set", "temps-it-a0", "up"],
        vec!["-n", FOREIGN_NETNS, "link", "set", "lo", "up"],
        // The forger holds the peer's address and routes the underlay
        // address through this host's public end.
        vec![
            "-n",
            FOREIGN_NETNS,
            "addr",
            "add",
            "10.123.45.2/32",
            "dev",
            "lo",
        ],
        vec![
            "-n",
            FOREIGN_NETNS,
            "route",
            "add",
            "10.123.45.1/32",
            "via",
            "198.51.100.1",
        ],
    ] {
        ip(&args).await;
    }
    let _rp_filter = disable_rp_filter(FOREIGN_VETH);

    let alloc = NodeAlloc {
        underlay_address: underlay,
        ..env.alloc()
    };
    let overlay_peer = Peer {
        underlay_address: peer,
        ..env.peer()
    };
    mgr.bootstrap(alloc, vec![overlay_peer])
        .await
        .expect("bootstrap");

    assert_eq!(
        inject_vxlan(PEER_NETNS, peer, underlay, 42).await,
        Injection {
            decapsulated: true,
            dropped: false
        },
        "the peer's VXLAN must be admitted"
    );
    assert_eq!(
        inject_vxlan(FOREIGN_NETNS, peer, underlay, 42).await,
        Injection {
            decapsulated: false,
            dropped: true
        },
        "the peer's address arriving from another network must be dropped"
    );
    assert_eq!(
        inject_vxlan(FOREIGN_NETNS, foreign, public, 42).await,
        Injection {
            decapsulated: false,
            dropped: true
        },
        "VXLAN to the public address must be dropped"
    );
    assert_eq!(
        inject_vxlan(FOREIGN_NETNS, foreign, underlay, 42).await,
        Injection {
            decapsulated: false,
            dropped: true
        },
        "an unknown sender's VXLAN to the underlay address must be dropped"
    );
    assert!(
        !inject_vxlan(FOREIGN_NETNS, foreign, public, 4242)
            .await
            .dropped,
        "another VNI is left to whoever uses it"
    );
}

/// Run `ip` with `args`, failing the test with its stderr if it fails.
async fn ip(args: &[&str]) {
    let output = Command::new("ip")
        .args(args)
        .output()
        .await
        .expect("run ip");
    assert!(
        output.status.success(),
        "ip {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The interface index of `name`: unchanged as long as the device was never
/// deleted and recreated.
async fn link_index(name: &str) -> String {
    let detail = link_detail(name).await;
    detail
        .split(':')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[tokio::test]
async fn a_replacement_another_vxlan_would_collide_with_is_never_started() {
    // Another VXLAN device already holds the requested VNI and port, so the
    // kernel would refuse the replacement: the working device is not even
    // deleted.
    let (env, _mgr, _cleanup) = fixture().await;
    let mut previous = env.config();
    previous.transport = Transport::Vxlan {
        vni: 99,
        port: 4789,
    };
    let previous = NetworkManager::new(previous).expect("manager for the previous VNI");
    let alloc = env.alloc();
    let peer = env.peer();
    previous
        .bootstrap(alloc.clone(), vec![peer.clone()])
        .await
        .expect("first bootstrap");
    let index = link_index("vxlan-temps0").await;

    ip(&["link", "add", "temps-it-d0", "type", "dummy"]).await;
    ip(&[
        "link",
        "add",
        "temps-it-vx0",
        "type",
        "vxlan",
        "id",
        "42",
        "dev",
        "temps-it-d0",
        "dstport",
        "4789",
        "nolearning",
    ])
    .await;

    let requested = NetworkManager::new(env.config()).expect("manager for the requested VNI");
    let error = requested
        .bootstrap(alloc, vec![peer])
        .await
        .expect_err("VNI 42 is held by another device");
    assert!(
        error.to_string().contains("already holds vni=42"),
        "{error}"
    );

    assert_eq!(
        link_index("vxlan-temps0").await,
        index,
        "the device was deleted"
    );
    let detail = link_detail("vxlan-temps0").await;
    assert!(detail.contains("id 99"), "{detail}");
    assert!(detail.contains("master br-temps0"), "{detail}");
    assert!(fdb_has_entry("vxlan-temps0", &env.peer_underlay.to_string()).await);
    assert!(link_detail("temps-it-vx0").await.contains("id 42"));
}

#[tokio::test]
async fn a_refused_replacement_is_restored_then_migrates_once_the_parent_is_repaired() {
    // The new parent cannot carry the overlay's MTU, which the kernel only
    // reports once the old device is gone. The half-built replacement is
    // removed, the previous device comes back as it was — parent, MTU,
    // bridge, FDB — and the next bootstrap keeps it rather than tearing it
    // down to hit the same refusal — a carrier flap on the parent repairs
    // nothing and does not change that. Once the parent is repaired, the
    // move happens without waiting out the backoff.
    let (env, mgr, _cleanup) = fixture().await;
    let alloc = env.alloc();
    let peer = env.peer();
    mgr.bootstrap(alloc.clone(), vec![peer.clone()])
        .await
        .expect("first bootstrap");
    let mtu_before = link_mtu("vxlan-temps0").await;

    ip(&["link", "add", "temps-it-d0", "mtu", "1000", "type", "dummy"]).await;
    ip(&["link", "set", "temps-it-d0", "up"]).await;
    let mut moved = env.config();
    moved.underlay_dev = "temps-it-d0".into();
    let moved = NetworkManager::new(moved).expect("manager for the moved underlay");

    let error = moved
        .bootstrap(alloc.clone(), vec![peer.clone()])
        .await
        .expect_err("the new parent cannot carry the overlay MTU");
    assert!(
        error
            .to_string()
            .contains("the previous device was restored"),
        "{error}"
    );
    let detail = link_detail("vxlan-temps0").await;
    assert!(
        detail.contains(&format!("dev {} ", env.underlay_dev)),
        "previous parent not restored: {detail}"
    );
    assert!(detail.contains("id 42"), "{detail}");
    assert!(detail.contains("master br-temps0"), "{detail}");
    assert_eq!(link_mtu("vxlan-temps0").await, mtu_before);
    assert!(fdb_has_entry("vxlan-temps0", &env.peer_underlay.to_string()).await);

    let index = link_index("vxlan-temps0").await;
    let error = moved
        .bootstrap(alloc.clone(), vec![peer.clone()])
        .await
        .expect_err("the refused replacement is not retried yet");
    assert!(error.to_string().contains("retried in"), "{error}");
    assert_eq!(
        link_index("vxlan-temps0").await,
        index,
        "a retry tore the restored device down again"
    );

    ip(&["link", "set", "dev", "temps-it-d0", "carrier", "off"]).await;
    ip(&["link", "set", "dev", "temps-it-d0", "carrier", "on"]).await;
    let error = moved
        .bootstrap(alloc.clone(), vec![peer.clone()])
        .await
        .expect_err("a carrier flap is not a repair");
    assert!(error.to_string().contains("retried in"), "{error}");
    assert_eq!(
        link_index("vxlan-temps0").await,
        index,
        "a carrier flap tore the restored device down"
    );

    ip(&["link", "set", "temps-it-d0", "mtu", "1500"]).await;
    // Even a repaired parent waits out the floor between two attempts.
    tokio::time::sleep(Duration::from_secs(16)).await;
    moved
        .bootstrap(alloc, vec![peer])
        .await
        .expect("the repaired parent is used at once");
    let detail = link_detail("vxlan-temps0").await;
    assert!(
        detail.contains("dev temps-it-d0 "),
        "not migrated: {detail}"
    );
    assert!(detail.contains("master br-temps0"), "{detail}");
    assert!(fdb_has_entry("vxlan-temps0", &env.peer_underlay.to_string()).await);
}

#[tokio::test]
async fn bootstrap_rejects_a_non_vxlan_device_under_the_vxlan_name() {
    // Something else holding the name is not Temps' to delete.
    let (env, mgr, _cleanup) = fixture().await;
    let output = Command::new("ip")
        .args(["link", "add", "vxlan-temps0", "type", "dummy"])
        .output()
        .await
        .expect("create dummy device");
    assert!(
        output.status.success(),
        "create dummy device: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let error = mgr
        .bootstrap(env.alloc(), vec![])
        .await
        .expect_err("bootstrap must not take over a device that is not VXLAN");
    assert!(
        error.to_string().contains("existing device is not VXLAN"),
        "{error}"
    );
    assert!(link_exists("vxlan-temps0").await, "the device was deleted");
}

#[tokio::test]
async fn bootstrap_moves_existing_vxlan_to_the_requested_underlay() {
    // An agent upgraded from "default-route device" to "private-address
    // device" finds its overlay on the old parent. It must be recreated on
    // the requested one, at that device's MTU, with peers re-added.
    let (env, mgr, _cleanup) = fixture().await;
    for args in [
        vec!["link", "add", OLD_UNDERLAY, "type", "dummy"],
        vec!["link", "set", OLD_UNDERLAY, "mtu", "9000", "up"],
        vec![
            "link",
            "add",
            "vxlan-temps0",
            "type",
            "vxlan",
            "id",
            "42",
            "dev",
            OLD_UNDERLAY,
            "dstport",
            "4789",
            "nolearning",
        ],
        vec!["link", "set", "vxlan-temps0", "mtu", "8950"],
    ] {
        let output = Command::new("ip")
            .args(&args)
            .output()
            .await
            .expect("spawn ip");
        assert!(
            output.status.success(),
            "ip {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    mgr.bootstrap(env.alloc(), vec![env.peer()])
        .await
        .expect("bootstrap must move the overlay to the requested underlay");

    let detail = Command::new("ip")
        .args(["-d", "-o", "link", "show", "dev", "vxlan-temps0"])
        .output()
        .await
        .expect("inspect vxlan");
    let detail = String::from_utf8_lossy(&detail.stdout);
    assert!(
        detail.contains(&format!("dev {} ", env.underlay_dev)),
        "vxlan must be on {}: {detail}",
        env.underlay_dev
    );
    assert!(
        detail.contains("master br-temps0"),
        "vxlan must be enslaved: {detail}"
    );
    assert_eq!(link_mtu("vxlan-temps0").await, Some(1450));
    assert!(fdb_has_entry("vxlan-temps0", &env.peer_underlay.to_string()).await);

    // And the moved device is now simply compatible.
    mgr.bootstrap(env.alloc(), vec![env.peer()])
        .await
        .expect("second bootstrap is a no-op");
    assert_eq!(link_mtu("vxlan-temps0").await, Some(1450));
}

#[tokio::test]
async fn bootstrap_keeps_existing_vxlan_when_the_requested_underlay_is_missing() {
    // Moving the overlay must not delete the working device before its new
    // parent is known to exist: a typo in the underlay device would
    // otherwise cut every container off the overlay.
    let (env, _mgr, _cleanup) = fixture().await;
    for args in [
        vec!["link", "add", OLD_UNDERLAY, "type", "dummy"],
        vec!["link", "set", OLD_UNDERLAY, "up"],
        vec![
            "link",
            "add",
            "vxlan-temps0",
            "type",
            "vxlan",
            "id",
            "42",
            "dev",
            OLD_UNDERLAY,
            "dstport",
            "4789",
            "nolearning",
        ],
    ] {
        let output = Command::new("ip")
            .args(&args)
            .output()
            .await
            .expect("spawn ip");
        assert!(
            output.status.success(),
            "ip {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let mut config = env.config();
    config.underlay_dev = "temps-none0".into();
    let mgr = NetworkManager::new(config).expect("manager new");
    let error = mgr
        .bootstrap(env.alloc(), vec![env.peer()])
        .await
        .expect_err("bootstrap must fail when the requested underlay does not exist");
    assert!(
        error.to_string().contains("'temps-none0' not found"),
        "unexpected error: {error}"
    );

    let detail = Command::new("ip")
        .args(["-d", "-o", "link", "show", "dev", "vxlan-temps0"])
        .output()
        .await
        .expect("inspect vxlan");
    assert!(detail.status.success(), "the existing vxlan must survive");
    let detail = String::from_utf8_lossy(&detail.stdout);
    assert!(
        detail.contains(&format!("dev {OLD_UNDERLAY} ")),
        "the existing vxlan must stay on {OLD_UNDERLAY}: {detail}"
    );
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
async fn routed_subnet_collision_is_rejected_without_partial_kernel_state() {
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
    let occupied: Ipv4Net = existing_cidr.parse().expect("valid existing CIDR");
    // ADR 056: only routed subnets matter. With no peer on the occupied
    // range, the rest of the pool is irrelevant.
    let own_only = [env.alloc().compute_cidr];
    if !own_only
        .iter()
        .any(|own| own.contains(&occupied.network()) || occupied.contains(&own.network()))
    {
        temps_network::docker::preflight_network_for_subnets(
            &docker,
            &env.config(),
            &env.alloc(),
            &own_only,
        )
        .await
        .expect("a Docker network elsewhere in the pool must not block this node");
    }
    assert!(pool.contains(&occupied.network()) || occupied.contains(&pool.network()));
    // A peer routed over the occupied range is refused, naming the network.
    let error = temps_network::docker::preflight_network_for_subnets(
        &docker,
        &env.config(),
        &env.alloc(),
        &[env.alloc().compute_cidr, occupied],
    )
    .await
    .expect_err("a routed peer subnet over a Docker network must be refused");
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
    temps_network::preflight_compute_routes(&cfg, &[env.alloc().compute_cidr, env.peer_cidr, pool])
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
