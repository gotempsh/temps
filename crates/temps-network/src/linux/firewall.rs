// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! nftables baseline rules.
//!
//! We install one dedicated nftables table named `temps_network` so we can
//! tear our rules down without touching anything else on the host. The
//! table has two chains:
//!
//! * `forward` (priority -100, type filter, hook forward) — records the
//!   nftables baseline for bridge traffic. Docker's later default-DROP chain
//!   is handled separately through a scoped, owned `DOCKER-USER` hook because
//!   an nftables ACCEPT in an earlier base chain does not terminate traversal
//!   of later base chains.
//! * `postrouting` (priority 100, type nat, hook postrouting) — masquerades
//!   compute CIDR traffic that egresses on a non-bridge interface and gives
//!   cross-node traffic a symmetric return path when the destination container
//!   is also attached to another Docker network.
//!
//! We shell out to `nft` because it is the canonical tool, every modern
//! distro ships it, and the rule set we need is small enough that an
//! embedded library (`rustables`) would add more complexity than value.

use crate::config::{NetworkConfig, NodeAlloc, Peer, Transport};
use crate::error::NetworkError;
use crate::mesh::{LockdownState, MeshLockdown, MESH_INTERFACE};
use std::collections::HashSet;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{debug, info, warn};
use uuid::Uuid;

const TABLE: &str = "temps_network";
const DOCKER_USER_CHAIN: &str = "DOCKER-USER";
const OVERLAY_FORWARD_CHAIN: &str = "TEMPS_OVERLAY_FORWARD";
const OWNER_COMMENT: &str = "temps-overlay-forward-owner-v1";
const RULE_COMMENT: &str = "temps-overlay-forward-rule-v1";
const HOOK_COMMENT: &str = "temps-overlay-forward-hook-v1";
const RELAY_COMMENT: &str = "temps-mesh-relay-v1";

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct OverlayForwardRule {
    physical_input: Option<String>,
    output: Option<String>,
    source: String,
    destination: String,
}

impl OverlayForwardRule {
    fn ingress(config: &NetworkConfig, alloc: &NodeAlloc, peer: &Peer) -> Self {
        Self {
            // Once a VXLAN frame is admitted to the Linux bridge, the IPv4
            // FORWARD hook reports the logical bridge as its input device.
            // `-i vxlan-temps0` therefore never matches on production Docker
            // hosts. physdev preserves the actual ingress bridge port and
            // lets us keep this exception restricted to trusted VXLAN input.
            physical_input: Some(config.vxlan_dev_name.clone()),
            output: None,
            source: peer.compute_cidr.to_string(),
            destination: alloc.compute_cidr.to_string(),
        }
    }

    fn args(&self, operation: &str) -> Vec<String> {
        let mut args = vec![operation.to_string(), OVERLAY_FORWARD_CHAIN.to_string()];
        if let Some(input) = &self.physical_input {
            args.extend([
                "-m".to_string(),
                "physdev".to_string(),
                "--physdev-is-bridged".to_string(),
                "--physdev-in".to_string(),
                input.clone(),
            ]);
        }
        args.extend([
            "-s".to_string(),
            self.source.clone(),
            "-d".to_string(),
            self.destination.clone(),
            "-m".to_string(),
            "comment".to_string(),
            "--comment".to_string(),
            RULE_COMMENT.to_string(),
            "-j".to_string(),
            "ACCEPT".to_string(),
        ]);
        args
    }
}

/// Install the baseline rules. Idempotent: the script first deletes the
/// table (ignoring "not found"), then recreates it.
pub async fn install_baseline(
    config: &NetworkConfig,
    alloc: &NodeAlloc,
    peers: &[Peer],
) -> crate::Result<()> {
    let script = render_baseline(config, alloc, peers);
    apply_nft(&script)
        .await
        .map_err(|reason| NetworkError::Nftables {
            op: "install_baseline",
            table: TABLE.into(),
            reason,
        })?;
    install_docker_forwarding(config, alloc, peers).await?;
    info!(table = TABLE, bridge = %config.bridge_name, cidr = %alloc.compute_cidr, "nftables baseline installed");
    Ok(())
}

/// Return whether the owned table contains the marker for the exact desired
/// configuration. This avoids rewriting a live firewall on every peer poll
/// while still repairing `nft flush table inet temps_network` and stale peer
/// allowlists automatically.
pub async fn baseline_is_current(
    config: &NetworkConfig,
    alloc: &NodeAlloc,
    peers: &[Peer],
) -> crate::Result<bool> {
    let marker = baseline_marker(config, alloc, peers);
    let output = Command::new("nft")
        .args(["list", "table", "inet", TABLE])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|error| NetworkError::Nftables {
            op: "inspect_baseline",
            table: TABLE.into(),
            reason: format!("spawn nft: {error}"),
        })?;
    if !output.status.success() {
        return Ok(false);
    }
    if !String::from_utf8_lossy(&output.stdout).contains(&marker) {
        return Ok(false);
    }
    docker_forwarding_is_current(config, alloc, peers).await
}

/// Table owning the WireGuard mesh lockdown. Separate from [`TABLE`] so it
/// can be in place before `temps-wg0` exists, independently of whether the
/// overlay itself has bootstrapped.
const MESH_TABLE: &str = "temps_mesh";

/// Whether the mesh lockdown this host should have is installed.
pub async fn mesh_lockdown_state(lockdown: &MeshLockdown) -> crate::Result<LockdownState> {
    let output = Command::new("nft")
        .args(["list", "table", "inet", MESH_TABLE])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|error| NetworkError::Nftables {
            op: "inspect_mesh_lockdown",
            table: MESH_TABLE.into(),
            reason: format!("spawn nft: {error}"),
        })?;
    Ok(if !output.status.success() {
        LockdownState::Missing
    } else if String::from_utf8_lossy(&output.stdout).contains(&mesh_lockdown_marker(lockdown)) {
        LockdownState::Current
    } else {
        LockdownState::Outdated
    })
}

/// Install the mesh lockdown unless the current one is already in place.
/// Returns whether it (re)installed. Idempotent and atomic: the script
/// replaces the whole table in one nft transaction.
pub async fn ensure_mesh_lockdown(lockdown: &MeshLockdown) -> crate::Result<bool> {
    if mesh_lockdown_state(lockdown).await? == LockdownState::Current {
        return Ok(false);
    }
    apply_nft(&render_mesh_lockdown(lockdown))
        .await
        .map_err(|reason| NetworkError::Nftables {
            op: "install_mesh_lockdown",
            table: MESH_TABLE.into(),
            reason,
        })?;
    info!(table = MESH_TABLE, "WireGuard mesh lockdown installed");
    Ok(true)
}

/// The mesh carries VXLAN between nodes, plus (on workers) the control
/// plane's connections to published container ports. Without this table it
/// would also be a path from every peer to anything the host binds on
/// 0.0.0.0 (the control plane's database, agent APIs) and, since Docker
/// enables forwarding, a route through the node into its own LAN — reachable
/// even where a cloud firewall guards the public addresses. The icmp rule is
/// IPv4-only on purpose: the mesh carries no IPv6.
fn render_mesh_lockdown(lockdown: &MeshLockdown) -> String {
    let wg = MESH_INTERFACE;
    let vxlan_port = lockdown.vxlan_port;
    let mesh = lockdown.mesh;
    let marker = mesh_lockdown_marker(lockdown);
    let node_api = lockdown
        .node_api_port
        .map(|port| {
            format!(
                "add rule inet {MESH_TABLE} input iifname \"{wg}\" ip saddr {mesh} tcp dport {port} accept\n"
            )
        })
        .unwrap_or_default();
    // A hub forwards between members (ADR 048 D4): in from the tunnel and
    // straight back into it, mesh address to mesh address. Each member's own
    // input rules still decide what it accepts.
    let relay = if lockdown.relay {
        format!(
            "add rule inet {MESH_TABLE} forward iifname \"{wg}\" oifname \"{wg}\" ip saddr {mesh} ip daddr {mesh} accept\n"
        )
    } else {
        String::new()
    };
    format!(
        "
add table inet {MESH_TABLE}
delete table inet {MESH_TABLE}
add table inet {MESH_TABLE}

# Ahead of temps_network (-100) and Docker's chains.
add chain inet {MESH_TABLE} input {{ type filter hook input priority -110; policy accept; }}
add rule inet {MESH_TABLE} input counter comment \"{marker}\"
add rule inet {MESH_TABLE} input iifname \"{wg}\" ct state established,related accept
add rule inet {MESH_TABLE} input iifname \"{wg}\" udp dport {vxlan_port} accept
add rule inet {MESH_TABLE} input iifname \"{wg}\" icmp type echo-request accept
{node_api}add rule inet {MESH_TABLE} input iifname \"{wg}\" counter drop

add chain inet {MESH_TABLE} forward {{ type filter hook forward priority -110; policy accept; }}
add rule inet {MESH_TABLE} forward oifname \"{wg}\" ct state established,related accept
{relay}add rule inet {MESH_TABLE} forward iifname \"{wg}\" ip saddr {mesh} ct status dnat accept
add rule inet {MESH_TABLE} forward iifname \"{wg}\" counter drop
add rule inet {MESH_TABLE} forward oifname \"{wg}\" counter drop
"
    )
}

fn mesh_lockdown_marker(lockdown: &MeshLockdown) -> String {
    const MESH_LOCKDOWN_VERSION: &str = "v1";
    let signature = format!("{MESH_LOCKDOWN_VERSION}|{lockdown:?}");
    format!(
        "temps-mesh-{MESH_LOCKDOWN_VERSION}-{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, signature.as_bytes())
    )
}

/// Make this host relay mesh traffic between members (ADR 048 D4), or stop.
/// Idempotent.
///
/// Whether this host relays is decided by the firewall alone: the mesh
/// lockdown's forward chain accepts tunnel-to-tunnel traffic only on the hub
/// (`relay`) and drops it everywhere else. Forwarding on the tunnel
/// interface is on for every member, hub or not, because the kernel gates
/// forwarding on the ingress interface and DNAT'd connections to published
/// container ports arrive on it too (see
/// [`crate::linux::sysctl::enable_interface_forwarding`]).
///
/// On a Docker host the hub also accepts the relayed traffic in
/// `DOCKER-USER`: Docker's `FORWARD` chain drops by default, and an nftables
/// accept does not end traversal of later base chains.
pub async fn ensure_mesh_relay(mesh: ipnet::Ipv4Net, enabled: bool) -> crate::Result<()> {
    // The iptables checks run on every mesh tick of every member; once a
    // state is verified, only recheck it now and then to repair drift (a
    // flushed DOCKER-USER chain).
    let wanted = (mesh, enabled);
    if let Ok(last) = LAST_RELAY.lock() {
        if last.is_some_and(|(state, at)| state == wanted && at.elapsed() < RELAY_RECHECK) {
            return Ok(());
        }
    }
    // Only a fully verified state is cached: one this tick could not finish
    // (forwarding not on yet, DOCKER-USER unreadable) is retried next tick
    // rather than left broken until the recheck interval.
    if ensure_mesh_relay_now(mesh, enabled).await? == RelayCheck::Verified {
        if let Ok(mut last) = LAST_RELAY.lock() {
            *last = Some((wanted, std::time::Instant::now()));
        }
    }
    Ok(())
}

/// Whether [`ensure_mesh_relay_now`] verified everything it set out to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelayCheck {
    Verified,
    /// Something was left undone without failing the tick; retry next tick.
    Retry,
}

/// Forget the relay state [`ensure_mesh_relay`] verified, so the next call
/// checks everything again. For when what it verified may be gone: the mesh
/// interface was recreated (a new interface starts without forwarding) or
/// could not be reconciled.
pub fn forget_mesh_relay() {
    if let Ok(mut last) = LAST_RELAY.lock() {
        *last = None;
    }
}

/// The relay state [`ensure_mesh_relay`] last verified, and when.
static LAST_RELAY: std::sync::Mutex<Option<((ipnet::Ipv4Net, bool), std::time::Instant)>> =
    std::sync::Mutex::new(None);
const RELAY_RECHECK: std::time::Duration = std::time::Duration::from_secs(300);

async fn ensure_mesh_relay_now(mesh: ipnet::Ipv4Net, enabled: bool) -> crate::Result<RelayCheck> {
    let forwarding = settle_mesh_forwarding(
        mesh,
        enabled,
        crate::linux::sysctl::enable_interface_forwarding(MESH_INTERFACE),
    );
    let check = forwarding.check();
    match forwarding {
        MeshForwarding::Ready | MeshForwarding::NoInterface => {}
        MeshForwarding::Degraded(error) => warn!(
            mesh = %mesh,
            interface = MESH_INTERFACE,
            error = %error,
            "could not enable forwarding on the WireGuard mesh interface: published container \
             ports on this host are unreachable over the mesh until it is on"
        ),
        MeshForwarding::Failed(error) => return Err(error),
    }

    let mut probes = Vec::with_capacity(IPTABLES_BACKENDS.len());
    for backend in IPTABLES_BACKENDS {
        probes.push((backend, probe_docker_user(backend).await));
    }
    let backends = match docker_user_decision(&probes) {
        DockerUserDecision::NoDocker => {
            debug!(mesh = %mesh, "no DOCKER-USER chain on any iptables backend; nothing else drops relayed mesh traffic");
            return Ok(check);
        }
        DockerUserDecision::Unknown(reason) if enabled => {
            return Err(NetworkError::Iptables {
                op: "probe_docker_user_for_mesh_relay",
                chain: DOCKER_USER_CHAIN.into(),
                reason: format!(
                    "this host is the mesh hub for {mesh}, but whether Docker's {DOCKER_USER_CHAIN} \
                     chain exists could not be determined, so the relay accept rule was not \
                     installed and Docker's FORWARD chain may drop relayed traffic: {reason}"
                ),
            });
        }
        DockerUserDecision::Unknown(reason) => {
            // Not the hub: a leftover accept rule is harmless, since the
            // mesh lockdown drops tunnel-to-tunnel forwarding before
            // Docker's chains see it. Say why it may linger.
            warn!(
                mesh = %mesh,
                reason = %reason,
                "could not probe Docker's DOCKER-USER chain to remove a stale mesh relay rule"
            );
            return Ok(RelayCheck::Retry);
        }
        DockerUserDecision::Install(backends) => backends,
    };
    let mesh = mesh.to_string();
    for backend in backends {
        let present = relay_rule_present(backend, &relay_args(RelayOp::Check, &mesh)).await?;
        if enabled && !present {
            run_relay_iptables(
                backend,
                "install_mesh_relay",
                &relay_args(RelayOp::Insert, &mesh),
            )
            .await?;
            info!(mesh = %mesh, backend, "this host now relays WireGuard mesh traffic (mesh hub)");
        } else if !enabled && present {
            run_relay_iptables(
                backend,
                "remove_mesh_relay",
                &relay_args(RelayOp::Delete, &mesh),
            )
            .await?;
            info!(mesh = %mesh, backend, "this host no longer relays WireGuard mesh traffic");
        }
    }
    if check == RelayCheck::Retry {
        debug!(mesh = %mesh, "mesh forwarding is not on yet; checking again next tick");
    }
    Ok(check)
}

/// What enabling forwarding on the mesh interface means for this tick.
#[derive(Debug)]
enum MeshForwarding {
    Ready,
    /// No interface yet, and this host is not the hub: nothing to forward.
    NoInterface,
    /// Not the hub, and the write failed: published ports over the mesh
    /// break, but nothing this tick does depends on it, so warn and go on.
    Degraded(NetworkError),
    /// The hub cannot relay without it.
    Failed(NetworkError),
}

impl MeshForwarding {
    /// Whether this outcome may be cached: forwarding that is not on yet
    /// (no interface, or a failed write) is retried on the next tick.
    fn check(&self) -> RelayCheck {
        match self {
            Self::Ready => RelayCheck::Verified,
            Self::NoInterface | Self::Degraded(_) | Self::Failed(_) => RelayCheck::Retry,
        }
    }
}

fn settle_mesh_forwarding(
    mesh: ipnet::Ipv4Net,
    relay: bool,
    outcome: crate::Result<crate::linux::sysctl::InterfaceForwarding>,
) -> MeshForwarding {
    use crate::linux::sysctl::InterfaceForwarding;
    match (outcome, relay) {
        (Ok(InterfaceForwarding::Enabled), _) => MeshForwarding::Ready,
        (Ok(InterfaceForwarding::NoInterface), false) => MeshForwarding::NoInterface,
        (Ok(InterfaceForwarding::NoInterface), true) => MeshForwarding::Failed(NetworkError::Io {
            op: "enable_mesh_forwarding",
            path: format!("/proc/sys/net/ipv4/conf/{MESH_INTERFACE}"),
            reason: format!(
                "this host is the mesh hub for {mesh}, but interface {MESH_INTERFACE} does not \
                 exist, so it cannot relay"
            ),
        }),
        (Err(error), false) => MeshForwarding::Degraded(error),
        (Err(error), true) => MeshForwarding::Failed(error),
    }
}

/// The iptables front ends Docker may have programmed its chains through.
/// `iptables` is whatever the host's alternatives point at; Docker may have
/// used the other backend (its own binary choice, or a host switched
/// between them), and a rule added through the wrong one lands in a table
/// Docker's drop does not consult.
const IPTABLES_BACKENDS: [&str; 3] = ["iptables", "iptables-nft", "iptables-legacy"];

/// What `<backend> -S DOCKER-USER` said.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ChainProbe {
    Present,
    /// The chain, or the whole table, does not exist on this backend.
    Absent,
    /// This backend is not installed.
    NoBinary,
    /// It ran and failed for another reason (permissions, a backend
    /// mismatch, a locked table): whether the chain exists is unknown.
    Failed(String),
}

async fn probe_docker_user(backend: &'static str) -> ChainProbe {
    match Command::new(backend)
        .args(["-w", "5", "-S", DOCKER_USER_CHAIN])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
    {
        Ok(output) => classify_chain_probe(
            output.status.code(),
            &String::from_utf8_lossy(&output.stderr),
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ChainProbe::NoBinary,
        Err(error) => ChainProbe::Failed(format!("spawn {backend}: {error}")),
    }
}

fn classify_chain_probe(code: Option<i32>, stderr: &str) -> ChainProbe {
    if code == Some(0) {
        return ChainProbe::Present;
    }
    let lower = stderr.to_ascii_lowercase();
    // iptables: "No chain/target/match by that name."; iptables-nft:
    // "chain `X' in table `filter' does not exist"; iptables-legacy on an
    // nft-only kernel: "Table does not exist (do you need to insmod?)".
    if lower.contains("no chain/target/match by that name") || lower.contains("does not exist") {
        return ChainProbe::Absent;
    }
    ChainProbe::Failed(format!("exit status {code:?}: {}", stderr.trim()))
}

/// Where (if anywhere) the hub's DOCKER-USER rule belongs.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DockerUserDecision {
    /// Every backend that has the chain: a drop in any of them is final.
    Install(Vec<&'static str>),
    /// No backend has the chain: Docker is not here, or not filtering.
    NoDocker,
    /// No backend has the chain for certain, and at least one could not
    /// tell: the rule may be needed and cannot be placed.
    Unknown(String),
}

fn docker_user_decision(probes: &[(&'static str, ChainProbe)]) -> DockerUserDecision {
    let present: Vec<&'static str> = probes
        .iter()
        .filter(|(_, probe)| *probe == ChainProbe::Present)
        .map(|(backend, _)| *backend)
        .collect();
    if !present.is_empty() {
        return DockerUserDecision::Install(present);
    }
    let failures: Vec<String> = probes
        .iter()
        .filter_map(|(backend, probe)| match probe {
            ChainProbe::Failed(reason) => Some(format!("{backend}: {reason}")),
            ChainProbe::Present | ChainProbe::Absent | ChainProbe::NoBinary => None,
        })
        .collect();
    if failures.is_empty() {
        DockerUserDecision::NoDocker
    } else {
        DockerUserDecision::Unknown(failures.join("; "))
    }
}

async fn relay_iptables_output(
    backend: &'static str,
    op: &'static str,
    args: &[&str],
) -> crate::Result<std::process::Output> {
    Command::new(backend)
        .arg("-w")
        .arg("5")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|error| NetworkError::Iptables {
            op,
            chain: DOCKER_USER_CHAIN.into(),
            reason: format!("spawn {backend}: {error}"),
        })
}

/// `-C` for the relay rule: exit 1 means absent, anything else is an error.
async fn relay_rule_present(backend: &'static str, args: &[&str]) -> crate::Result<bool> {
    let output = relay_iptables_output(backend, "check_mesh_relay", args).await?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        code => Err(NetworkError::Iptables {
            op: "check_mesh_relay",
            chain: DOCKER_USER_CHAIN.into(),
            reason: format!(
                "{backend} exited with status {code:?}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        }),
    }
}

async fn run_relay_iptables(
    backend: &'static str,
    op: &'static str,
    args: &[&str],
) -> crate::Result<()> {
    let output = relay_iptables_output(backend, op, args).await?;
    if output.status.success() {
        return Ok(());
    }
    Err(NetworkError::Iptables {
        op,
        chain: DOCKER_USER_CHAIN.into(),
        reason: format!(
            "{backend} exited with status {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    })
}

#[derive(Debug, Clone, Copy)]
enum RelayOp {
    Check,
    /// First in the chain: Docker's own rules come after DOCKER-USER's.
    Insert,
    Delete,
}

/// The iptables arguments for the hub's DOCKER-USER rule.
fn relay_args(op: RelayOp, mesh: &str) -> Vec<&str> {
    let rule = relay_rule_args(mesh);
    let mut args = Vec::with_capacity(rule.len() + 2);
    match op {
        RelayOp::Check => args.push("-C"),
        RelayOp::Delete => args.push("-D"),
        RelayOp::Insert => args.push("-I"),
    }
    args.push(rule[0]);
    if matches!(op, RelayOp::Insert) {
        args.push("1");
    }
    args.extend_from_slice(&rule[1..]);
    args
}

fn relay_rule_args(mesh: &str) -> [&str; 15] {
    [
        DOCKER_USER_CHAIN,
        "-i",
        MESH_INTERFACE,
        "-o",
        MESH_INTERFACE,
        "-s",
        mesh,
        "-d",
        mesh,
        "-m",
        "comment",
        "--comment",
        RELAY_COMMENT,
        "-j",
        "ACCEPT",
    ]
}

/// Remove the baseline rules. Idempotent.
pub async fn remove_baseline(_config: &NetworkConfig) -> crate::Result<()> {
    // Also clean verified, Temps-owned VXLAN state after a VXLAN -> native
    // transition. Native transport itself never installs forwarding policy.
    remove_docker_forwarding().await?;
    let script = format!("delete table inet {table}\n", table = TABLE);
    match apply_nft(&script).await {
        Ok(()) => Ok(()),
        Err(reason) if reason.contains("No such file") || reason.contains("does not exist") => {
            debug!(table = TABLE, "nftables table already absent");
            Ok(())
        }
        Err(reason) => Err(NetworkError::Nftables {
            op: "remove_baseline",
            table: TABLE.into(),
            reason,
        }),
    }
}

fn desired_overlay_rules(
    config: &NetworkConfig,
    alloc: &NodeAlloc,
    peers: &[Peer],
) -> HashSet<OverlayForwardRule> {
    if !matches!(config.transport, Transport::Vxlan { .. }) {
        return HashSet::new();
    }
    // Docker already owns local bridge egress and established return traffic
    // in FORWARD. The only missing allowance is a new connection arriving
    // from a trusted VXLAN peer for a local overlay container. Do not add an
    // egress exception here: accepting solely by source/destination CIDRs and
    // VXLAN output would let an unrelated local bridge spoof an overlay source
    // and bypass Docker's isolation policy.
    peers
        .iter()
        .map(|peer| OverlayForwardRule::ingress(config, alloc, peer))
        .collect()
}

/// Reconcile the Docker-supported forwarding hook without flushing Docker's
/// own chains. Desired rules are installed before stale rules are removed, so
/// a peer refresh cannot interrupt established overlay connectivity.
async fn install_docker_forwarding(
    config: &NetworkConfig,
    alloc: &NodeAlloc,
    peers: &[Peer],
) -> crate::Result<()> {
    if !matches!(config.transport, Transport::Vxlan { .. }) {
        return remove_docker_forwarding().await;
    }

    ensure_owned_chain().await?;
    reconcile_owned_hook().await?;

    let desired = desired_overlay_rules(config, alloc, peers);
    for rule in &desired {
        let check = rule.args("-C");
        if !iptables_check_owned("check_overlay_rule", &check).await? {
            let append = rule.args("-A");
            run_iptables_owned("append_overlay_rule", &append).await?;
        }
    }

    let existing = list_overlay_rules().await?;
    let mut retained = HashSet::new();
    for (rule, delete_args) in existing {
        let keep = rule.as_ref().is_some_and(|candidate| {
            desired.contains(candidate) && retained.insert(candidate.clone())
        });
        if !keep {
            run_iptables_owned("delete_stale_overlay_rule", &delete_args).await?;
        }
    }
    Ok(())
}

async fn docker_forwarding_is_current(
    config: &NetworkConfig,
    alloc: &NodeAlloc,
    peers: &[Peer],
) -> crate::Result<bool> {
    if !matches!(config.transport, Transport::Vxlan { .. }) {
        return Ok(!owned_chain_exists().await? && list_owned_hooks().await?.is_empty());
    }
    if !owned_chain_exists().await?
        || list_owned_hooks().await?.len() != 1
        || !owned_hook_is_correctly_positioned().await?
    {
        return Ok(false);
    }
    let desired = desired_overlay_rules(config, alloc, peers);
    let existing = list_overlay_rules().await?;
    let actual: HashSet<_> = existing
        .iter()
        .filter_map(|(rule, _)| rule.clone())
        .collect();
    Ok(existing.len() == desired.len() && actual == desired)
}

async fn remove_docker_forwarding() -> crate::Result<()> {
    for hook in list_owned_hooks().await? {
        run_iptables_owned("remove_overlay_hook", &hook).await?;
    }
    if owned_chain_exists().await? {
        run_iptables("flush_overlay_chain", &["-F", OVERLAY_FORWARD_CHAIN]).await?;
        run_iptables("delete_overlay_chain", &["-X", OVERLAY_FORWARD_CHAIN]).await?;
    }
    Ok(())
}

async fn ensure_owned_chain() -> crate::Result<()> {
    if iptables_check(&["-S", OVERLAY_FORWARD_CHAIN]).await? {
        if owned_chain_exists().await? {
            return Ok(());
        }
        return Err(NetworkError::Iptables {
            op: "verify_overlay_chain_owner",
            chain: OVERLAY_FORWARD_CHAIN.into(),
            reason: format!(
                "chain already exists without the required ownership marker '{OWNER_COMMENT}'"
            ),
        });
    }
    run_iptables("create_overlay_chain", &["-N", OVERLAY_FORWARD_CHAIN]).await?;
    run_iptables(
        "mark_overlay_chain_owner",
        &[
            "-A",
            OVERLAY_FORWARD_CHAIN,
            "-m",
            "comment",
            "--comment",
            OWNER_COMMENT,
        ],
    )
    .await
}

async fn owned_chain_exists() -> crate::Result<bool> {
    iptables_check(&[
        "-C",
        OVERLAY_FORWARD_CHAIN,
        "-m",
        "comment",
        "--comment",
        OWNER_COMMENT,
    ])
    .await
}

fn owned_hook_args(operation: &str) -> Vec<String> {
    [
        operation,
        DOCKER_USER_CHAIN,
        "-m",
        "comment",
        "--comment",
        HOOK_COMMENT,
        "-j",
        OVERLAY_FORWARD_CHAIN,
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn owner_marker_args(operation: &str) -> Vec<String> {
    [
        operation,
        OVERLAY_FORWARD_CHAIN,
        "-m",
        "comment",
        "--comment",
        OWNER_COMMENT,
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

async fn list_owned_hooks() -> crate::Result<Vec<Vec<String>>> {
    let output = iptables_output("list_overlay_hooks", &["-S", DOCKER_USER_CHAIN]).await?;
    if !output.status.success() {
        return status_absent_or_error("list_overlay_hooks", output).map(|_| Vec::new());
    }
    let mut hooks = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let tokens: Vec<String> = line.split_ascii_whitespace().map(str::to_string).collect();
        if tokens == owned_hook_args("-A") {
            let mut delete = tokens;
            delete[0] = "-D".to_string();
            hooks.push(delete);
        }
    }
    Ok(hooks)
}

async fn reconcile_owned_hook() -> crate::Result<()> {
    for hook in list_owned_hooks().await? {
        run_iptables_owned("remove_stale_overlay_hook", &hook).await?;
    }

    let output = iptables_output("list_docker_user", &["-S", DOCKER_USER_CHAIN]).await?;
    if !output.status.success() {
        return status_absent_or_error("list_docker_user", output).and_then(|_| {
            Err(NetworkError::Iptables {
                op: "install_overlay_hook",
                chain: DOCKER_USER_CHAIN.into(),
                reason: "Docker's DOCKER-USER chain is unavailable".into(),
            })
        });
    }
    let rendered = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<_> = rendered
        .lines()
        .filter(|line| line.starts_with("-A "))
        .collect();
    let unconditional_return = format!("-A {DOCKER_USER_CHAIN} -j RETURN");
    let position = lines
        .iter()
        .position(|line| *line == unconditional_return)
        .map(|index| index + 1);
    let mut args = if let Some(position) = position {
        vec![
            "-I".to_string(),
            DOCKER_USER_CHAIN.to_string(),
            position.to_string(),
        ]
    } else {
        vec!["-A".to_string(), DOCKER_USER_CHAIN.to_string()]
    };
    args.extend([
        "-m".into(),
        "comment".into(),
        "--comment".into(),
        HOOK_COMMENT.into(),
        "-j".into(),
        OVERLAY_FORWARD_CHAIN.into(),
    ]);
    run_iptables_owned("install_overlay_hook", &args).await
}

async fn owned_hook_is_correctly_positioned() -> crate::Result<bool> {
    let output = iptables_output("inspect_docker_user", &["-S", DOCKER_USER_CHAIN]).await?;
    if !output.status.success() {
        return status_absent_or_error("inspect_docker_user", output);
    }
    let rendered = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<_> = rendered
        .lines()
        .filter(|line| line.starts_with("-A "))
        .collect();
    let hook = owned_hook_args("-A").join(" ");
    let Some(hook_index) = lines.iter().position(|line| *line == hook) else {
        return Ok(false);
    };
    let unconditional_return = format!("-A {DOCKER_USER_CHAIN} -j RETURN");
    let expected = lines
        .iter()
        .position(|line| *line == unconditional_return)
        .unwrap_or(lines.len());
    Ok(hook_index == expected.saturating_sub(1))
}

async fn list_overlay_rules() -> crate::Result<Vec<(Option<OverlayForwardRule>, Vec<String>)>> {
    let output = iptables_output("list_overlay_rules", &["-S", OVERLAY_FORWARD_CHAIN]).await?;
    if !output.status.success() {
        return status_absent_or_error("list_overlay_rules", output).map(|_| Vec::new());
    }
    let mut rules = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let tokens: Vec<String> = line.split_ascii_whitespace().map(str::to_string).collect();
        if tokens.first().map(String::as_str) != Some("-A")
            || tokens.get(1).map(String::as_str) != Some(OVERLAY_FORWARD_CHAIN)
        {
            continue;
        }
        if tokens == owner_marker_args("-A") {
            continue;
        }
        let parsed = parse_overlay_rule(&tokens);
        let legacy_owned = parsed.is_none() && parse_legacy_overlay_rule(&tokens).is_some();
        if parsed.is_none() && !legacy_owned {
            return Err(NetworkError::Iptables {
                op: "inspect_overlay_chain",
                chain: OVERLAY_FORWARD_CHAIN.into(),
                reason: format!("owned chain contains an unexpected rule: {line}"),
            });
        }
        let mut delete_args = tokens;
        delete_args[0] = "-D".to_string();
        rules.push((parsed, delete_args));
    }
    Ok(rules)
}

/// Parse the exact forwarding rule emitted before Temps switched from the
/// logical `-i vxlan-temps0` match to bridge-aware `physdev` matching. These
/// rules carry our ownership comment, but no longer match bridged packets on
/// production Docker hosts. Recognizing only this narrow shape lets the
/// reconciler install the replacement first and then safely delete the stale
/// rule during an in-place upgrade.
fn parse_legacy_overlay_rule(tokens: &[String]) -> Option<OverlayForwardRule> {
    if tokens.first().map(String::as_str) != Some("-A")
        || tokens.get(1).map(String::as_str) != Some(OVERLAY_FORWARD_CHAIN)
    {
        return None;
    }
    let mut input = None;
    let mut source = None;
    let mut destination = None;
    let mut comment_module = false;
    let mut comment = None;
    let mut jump = None;
    let mut index = 2;
    while index < tokens.len() {
        if tokens[index] == "-m"
            && tokens.get(index + 1).map(String::as_str) == Some("comment")
            && !comment_module
        {
            comment_module = true;
            index += 2;
            continue;
        }
        let value = tokens.get(index + 1)?.clone();
        let slot = match tokens[index].as_str() {
            "-i" if input.is_none() => &mut input,
            "-s" if source.is_none() => &mut source,
            "-d" if destination.is_none() => &mut destination,
            "--comment" if comment.is_none() => &mut comment,
            "-j" if jump.is_none() => &mut jump,
            _ => return None,
        };
        *slot = Some(value);
        index += 2;
    }
    if !comment_module
        || comment.as_deref() != Some(RULE_COMMENT)
        || jump.as_deref() != Some("ACCEPT")
    {
        return None;
    }
    Some(OverlayForwardRule {
        physical_input: input,
        output: None,
        source: source?,
        destination: destination?,
    })
}

fn parse_overlay_rule(tokens: &[String]) -> Option<OverlayForwardRule> {
    if tokens.first().map(String::as_str) != Some("-A")
        || tokens.get(1).map(String::as_str) != Some(OVERLAY_FORWARD_CHAIN)
    {
        return None;
    }
    let mut physical_input = None;
    let mut output = None;
    let mut source = None;
    let mut destination = None;
    let mut physdev_module = false;
    let mut physdev_is_bridged = false;
    let mut comment_module = false;
    let mut comment = None;
    let mut jump = None;
    let mut index = 2;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "--physdev-is-bridged" if !physdev_is_bridged => {
                physdev_is_bridged = true;
                index += 1;
                continue;
            }
            "--physdev-in" if physical_input.is_none() => {
                physical_input = Some(tokens.get(index + 1)?.clone());
                index += 2;
                continue;
            }
            "-m" if tokens.get(index + 1).map(String::as_str) == Some("physdev")
                && !physdev_module =>
            {
                physdev_module = true;
                index += 2;
                continue;
            }
            "-m" if tokens.get(index + 1).map(String::as_str) == Some("comment")
                && !comment_module =>
            {
                comment_module = true;
                index += 2;
                continue;
            }
            _ => {}
        }
        let value = tokens.get(index + 1)?.clone();
        let slot = match tokens[index].as_str() {
            "-o" if output.is_none() => &mut output,
            "-s" if source.is_none() => &mut source,
            "-d" if destination.is_none() => &mut destination,
            "--comment" if comment.is_none() => &mut comment,
            "-j" if jump.is_none() => &mut jump,
            // Reject negation, duplicate clauses, comments, and any other
            // extension. The chain is exclusively owned by Temps; retaining
            // a broader rule because it merely resembles a desired rule
            // would turn stale-state cleanup into a firewall bypass.
            _ => return None,
        };
        *slot = Some(value);
        index += 2;
    }
    if !physdev_module
        || !physdev_is_bridged
        || physical_input.is_none()
        || !comment_module
        || comment.as_deref() != Some(RULE_COMMENT)
        || jump.as_deref() != Some("ACCEPT")
    {
        return None;
    }
    Some(OverlayForwardRule {
        physical_input,
        output,
        source: source?,
        destination: destination?,
    })
}

async fn iptables_check(args: &[&str]) -> crate::Result<bool> {
    let output = iptables_output("check", args).await?;
    if output.status.success() {
        Ok(true)
    } else {
        status_absent_or_error("check", output)
    }
}

async fn iptables_check_owned(op: &'static str, args: &[String]) -> crate::Result<bool> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = iptables_output(op, &refs).await?;
    if output.status.success() {
        Ok(true)
    } else {
        status_absent_or_error(op, output)
    }
}

fn status_absent_or_error(op: &'static str, output: std::process::Output) -> crate::Result<bool> {
    if output.status.code() == Some(1) {
        return Ok(false);
    }
    Err(NetworkError::Iptables {
        op,
        chain: OVERLAY_FORWARD_CHAIN.into(),
        reason: format!(
            "iptables exited with status {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    })
}

async fn run_iptables_owned(op: &'static str, args: &[String]) -> crate::Result<()> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_iptables(op, &refs).await
}

async fn run_iptables(op: &'static str, args: &[&str]) -> crate::Result<()> {
    let output = iptables_output(op, args).await?;
    if output.status.success() {
        Ok(())
    } else {
        Err(NetworkError::Iptables {
            op,
            chain: OVERLAY_FORWARD_CHAIN.into(),
            reason: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

async fn iptables_output(op: &'static str, args: &[&str]) -> crate::Result<std::process::Output> {
    Command::new("iptables")
        .arg("-w")
        .arg("5")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|error| NetworkError::Iptables {
            op,
            chain: OVERLAY_FORWARD_CHAIN.into(),
            reason: format!("spawn iptables: {error}"),
        })
}

fn render_baseline(config: &NetworkConfig, alloc: &NodeAlloc, peers: &[Peer]) -> String {
    let bridge = &config.bridge_name;
    let cidr = alloc.compute_cidr;
    let vxlan_ingress = match config.transport {
        Transport::Vxlan { port, .. } => {
            let mut rules = String::new();
            let underlay_device = &config.underlay_dev;
            let local_family = if alloc.underlay_address.is_ipv4() {
                "ip"
            } else {
                "ip6"
            };
            for peer in peers {
                let family = if peer.underlay_address.is_ipv4() {
                    "ip"
                } else {
                    "ip6"
                };
                if family != local_family {
                    continue;
                }
                rules.push_str(&format!(
                    "add rule inet {TABLE} input iifname \"{underlay_device}\" {family} daddr {} {family} saddr {} udp dport {port} accept\n",
                    alloc.underlay_address, peer.underlay_address
                ));
            }
            rules.push_str(&format!(
                "add rule inet {TABLE} input iifname \"{underlay_device}\" {local_family} daddr {} udp dport {port} counter drop\n",
                alloc.underlay_address
            ));
            // The kernel VXLAN socket listens on every interface, and a
            // decapsulated frame lands on the overlay bridge. On the WireGuard
            // mesh, overlay traffic only ever arrives inside the tunnel, so
            // VXLAN on the public NIC is injected, not peer, traffic. (Without
            // the mesh a LAN underlay may legitimately arrive on another
            // device, e.g. a VLAN sub-interface, so no such rule there.)
            if underlay_device == MESH_INTERFACE {
                rules.push_str(&format!(
                    "add rule inet {TABLE} input iifname != \"{underlay_device}\" udp dport {port} counter drop\n"
                ));
            }
            rules
        }
        Transport::Native => String::new(),
    };
    // A service may already be attached to `temps-app-network` before it is
    // attached to the overlay. Linux then keeps that first network as the
    // container's default route, so replies to a remote overlay CIDR leave via
    // the wrong interface and never traverse VXLAN. SNAT remote-node traffic
    // to this node's overlay gateway. Conntrack reverses it on return, while
    // local same-node traffic keeps its original source address.
    let mut cross_node_snat = String::new();
    for peer in peers {
        cross_node_snat.push_str(&format!(
            "add rule inet {TABLE} postrouting ip saddr {} ip daddr {cidr} snat to {}\n",
            peer.compute_cidr, alloc.bridge_address
        ));
    }
    let marker = baseline_marker(config, alloc, peers);
    format!(
        "
# Idempotent install: drop the table if it exists, recreate from scratch.
add table inet {table}
delete table inet {table}
add table inet {table}

add chain inet {table} forward {{ type filter hook forward priority -100; policy accept; }}
# Cloud-metadata endpoints hand out instance credentials to any local
# caller; containers must never reach them. These sit BEFORE the bridge
# accept rules (this chain runs at priority -100, ahead of Docker's own
# chains, so a later iptables rule could not catch this traffic).
# 169.254/16 = AWS/GCP/Azure/Hetzner/DO/Tencent; 100.100.100.200 = Alibaba.
add rule inet {table} forward ip daddr 169.254.0.0/16 counter reject
add rule inet {table} forward ip daddr 100.100.100.200 counter reject
add rule inet {table} forward ip6 daddr fd00:ec2::254 counter reject
add rule inet {table} forward ip6 daddr fd20:ce::254 counter reject
add rule inet {table} forward iifname \"{bridge}\" accept
add rule inet {table} forward oifname \"{bridge}\" accept

add chain inet {table} input {{ type filter hook input priority -100; policy accept; }}
{vxlan_ingress}
# Marker used by the reconciler to detect a flushed or stale owned table.
add rule inet {table} input counter comment \"{marker}\"

add chain inet {table} postrouting {{ type nat hook postrouting priority 100; policy accept; }}
{cross_node_snat}
add rule inet {table} postrouting ip saddr {cidr} oifname != \"{bridge}\" masquerade
",
        table = TABLE,
        bridge = bridge,
        cidr = cidr,
        vxlan_ingress = vxlan_ingress,
        cross_node_snat = cross_node_snat,
        marker = marker,
    )
}

fn baseline_marker(config: &NetworkConfig, alloc: &NodeAlloc, peers: &[Peer]) -> String {
    const BASELINE_SCHEMA_VERSION: &str = "v5";

    let mut peers = peers.to_vec();
    peers.sort_by_key(|peer| (peer.compute_cidr, peer.underlay_address, peer.node_id));
    let signature = format!("{BASELINE_SCHEMA_VERSION}|{config:?}|{alloc:?}|{peers:?}");
    format!(
        "temps-baseline-{BASELINE_SCHEMA_VERSION}-{}",
        Uuid::new_v5(&Uuid::NAMESPACE_OID, signature.as_bytes())
    )
}

async fn apply_nft(script: &str) -> std::result::Result<(), String> {
    let mut child = Command::new("nft")
        .arg("-f")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn nft: {}", e))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(script.as_bytes())
            .await
            .map_err(|e| format!("write nft script: {}", e))?;
        stdin
            .shutdown()
            .await
            .map_err(|e| format!("close nft stdin: {}", e))?;
    }

    let out = child
        .wait_with_output()
        .await
        .map_err(|e| format!("wait nft: {}", e))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ipnet::Ipv4Net;
    use std::net::{IpAddr, Ipv4Addr};
    use std::str::FromStr;
    use uuid::Uuid;

    #[test]
    fn baseline_script_includes_bridge_and_cidr() {
        let cfg = NetworkConfig::default();
        let alloc = NodeAlloc {
            node_id: Uuid::nil(),
            compute_cidr: Ipv4Net::from_str("172.20.5.0/24").unwrap(),
            bridge_address: IpAddr::V4(Ipv4Addr::new(172, 20, 5, 1)),
            underlay_address: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
        };
        let s = render_baseline(&cfg, &alloc, &[]);
        assert!(s.contains("br-temps0"));
        assert!(s.contains("172.20.5.0/24"));
        assert!(s.contains("masquerade"));
        assert!(s.contains("delete table inet temps_network"));
    }

    #[test]
    fn baseline_script_blocks_metadata_before_bridge_accept() {
        let cfg = NetworkConfig::default();
        let alloc = NodeAlloc {
            node_id: Uuid::nil(),
            compute_cidr: Ipv4Net::from_str("172.20.5.0/24").unwrap(),
            bridge_address: IpAddr::V4(Ipv4Addr::new(172, 20, 5, 1)),
            underlay_address: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
        };
        let s = render_baseline(&cfg, &alloc, &[]);
        let aws_block = s
            .find("ip daddr 169.254.0.0/16 counter reject")
            .expect("link-local cloud metadata reject rule present");
        let alibaba_block = s
            .find("ip daddr 100.100.100.200 counter reject")
            .expect("Alibaba metadata reject rule present");
        let aws_ipv6_block = s
            .find("ip6 daddr fd00:ec2::254 counter reject")
            .expect("AWS IPv6 metadata reject rule present");
        let google_ipv6_block = s
            .find("ip6 daddr fd20:ce::254 counter reject")
            .expect("Google IPv6 metadata reject rule present");
        let bridge_accept = s
            .find("forward iifname \"br-temps0\" accept")
            .expect("bridge accept rule present");
        assert!(
            aws_block < bridge_accept
                && alibaba_block < bridge_accept
                && aws_ipv6_block < bridge_accept
                && google_ipv6_block < bridge_accept,
            "metadata rejects must precede the bridge accept rule, \
             or accepted traffic would never reach them"
        );
    }

    #[test]
    fn vxlan_ingress_is_restricted_to_known_peers() {
        let cfg = NetworkConfig::default();
        let alloc = NodeAlloc {
            node_id: Uuid::nil(),
            compute_cidr: Ipv4Net::from_str("172.20.5.0/24").unwrap(),
            bridge_address: IpAddr::V4(Ipv4Addr::new(172, 20, 5, 1)),
            underlay_address: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
        };
        let peer = Peer {
            node_id: Uuid::new_v4(),
            compute_cidr: Ipv4Net::from_str("172.20.6.0/24").unwrap(),
            underlay_address: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)),
        };
        let marker = baseline_marker(&cfg, &alloc, std::slice::from_ref(&peer));
        let script = render_baseline(&cfg, &alloc, &[peer]);
        let allow = script
            .find(
                "input iifname \"eth0\" ip daddr 10.0.0.1 ip saddr 10.0.0.2 udp dport 4789 accept",
            )
            .expect("known peer allow rule");
        let drop = script
            .find("input iifname \"eth0\" ip daddr 10.0.0.1 udp dport 4789 counter drop")
            .expect("unknown peer drop rule");
        assert!(allow < drop);
        assert!(script.contains(&marker));
    }

    #[test]
    fn vxlan_off_the_underlay_device_is_dropped() {
        // With the WireGuard mesh as underlay, VXLAN must only arrive inside
        // the tunnel: the same port on the public NIC is injected traffic.
        let cfg = NetworkConfig {
            underlay_dev: "temps-wg0".into(),
            ..NetworkConfig::default()
        };
        let alloc = NodeAlloc {
            node_id: Uuid::nil(),
            compute_cidr: Ipv4Net::from_str("172.20.5.0/24").unwrap(),
            bridge_address: IpAddr::V4(Ipv4Addr::new(172, 20, 5, 1)),
            underlay_address: IpAddr::V4(Ipv4Addr::new(10, 201, 0, 2)),
        };
        let peer = Peer {
            node_id: Uuid::new_v4(),
            compute_cidr: Ipv4Net::from_str("172.20.6.0/24").unwrap(),
            underlay_address: IpAddr::V4(Ipv4Addr::new(10, 201, 0, 3)),
        };
        let script = render_baseline(&cfg, &alloc, &[peer]);
        let allow = script
            .find("input iifname \"temps-wg0\" ip daddr 10.201.0.2 ip saddr 10.201.0.3 udp dport 4789 accept")
            .expect("peer allow rule inside the tunnel");
        let off_underlay = script
            .find("input iifname != \"temps-wg0\" udp dport 4789 counter drop")
            .expect("drop rule for VXLAN outside the tunnel");
        assert!(allow < off_underlay);
    }

    #[test]
    fn the_mesh_carries_only_vxlan_ping_and_published_ports_from_members() {
        let mesh: ipnet::Ipv4Net = "10.201.0.0/24".parse().unwrap();
        let rules = render_mesh_lockdown(&MeshLockdown {
            vxlan_port: 4789,
            mesh,
            node_api_port: None,
            relay: false,
        });
        assert!(
            !rules.contains("tcp dport"),
            "workers take no TCP from the mesh"
        );
        let control_plane = render_mesh_lockdown(&MeshLockdown {
            vxlan_port: 4789,
            mesh,
            node_api_port: Some(51820),
            relay: false,
        });
        let node_api = control_plane
            .find("input iifname \"temps-wg0\" ip saddr 10.201.0.0/24 tcp dport 51820 accept")
            .expect("the control plane serves the node API to mesh members");
        assert!(
            node_api
                < control_plane
                    .find("input iifname \"temps-wg0\" counter drop")
                    .unwrap()
        );
        let accept_vxlan = rules
            .find("input iifname \"temps-wg0\" udp dport 4789 accept")
            .expect("VXLAN is accepted");
        let lockdown = rules
            .find("input iifname \"temps-wg0\" counter drop")
            .expect("everything else from the mesh is dropped");
        assert!(accept_vxlan < lockdown);
        let published = rules
            .find("forward iifname \"temps-wg0\" ip saddr 10.201.0.0/24 ct status dnat accept")
            .expect("mesh members reach published ports (proxy, ingress nodes)");
        let forward_drop = rules
            .find("forward iifname \"temps-wg0\" counter drop")
            .expect("nothing else is routed in from the mesh");
        assert!(published < forward_drop);
        assert!(rules.contains("forward oifname \"temps-wg0\" counter drop"));
        assert!(
            rules.contains("delete table inet temps_mesh"),
            "atomic replace"
        );
        assert_ne!(
            mesh_lockdown_marker(&MeshLockdown {
                vxlan_port: 4789,
                mesh,
                node_api_port: None,
                relay: false,
            }),
            mesh_lockdown_marker(&MeshLockdown {
                vxlan_port: 4789,
                mesh: "10.202.0.0/24".parse().unwrap(),
                node_api_port: None,
                relay: false,
            }),
            "a new pool reinstalls the rules"
        );
    }

    #[test]
    fn the_hub_rule_accepts_only_mesh_to_mesh_on_the_mesh_interface() {
        let rule = [
            "DOCKER-USER",
            "-i",
            "temps-wg0",
            "-o",
            "temps-wg0",
            "-s",
            "10.201.0.0/24",
            "-d",
            "10.201.0.0/24",
            "-m",
            "comment",
            "--comment",
            "temps-mesh-relay-v1",
            "-j",
            "ACCEPT",
        ];
        let with = |head: &[&'static str], tail: &[&'static str]| -> Vec<&'static str> {
            head.iter().chain(tail).copied().collect()
        };
        assert_eq!(
            relay_args(RelayOp::Check, "10.201.0.0/24"),
            with(&["-C"], &rule)
        );
        assert_eq!(
            relay_args(RelayOp::Delete, "10.201.0.0/24"),
            with(&["-D"], &rule)
        );
        // Inserted first, ahead of Docker's own drops.
        assert_eq!(
            relay_args(RelayOp::Insert, "10.201.0.0/24"),
            with(&["-I", "DOCKER-USER", "1"], &rule[1..])
        );
    }

    #[test]
    fn only_a_hub_forwards_from_the_mesh_back_into_it() {
        let mesh: ipnet::Ipv4Net = "10.201.0.0/24".parse().unwrap();
        let member = MeshLockdown {
            vxlan_port: 4789,
            mesh,
            node_api_port: None,
            relay: false,
        };
        let relay_rule = "forward iifname \"temps-wg0\" oifname \"temps-wg0\" ip saddr 10.201.0.0/24 ip daddr 10.201.0.0/24 accept";
        assert!(!render_mesh_lockdown(&member).contains(relay_rule));

        let hub = MeshLockdown {
            relay: true,
            ..member.clone()
        };
        let rules = render_mesh_lockdown(&hub);
        let relay = rules
            .find(relay_rule)
            .expect("the hub relays between members");
        assert!(
            relay
                < rules
                    .find("forward iifname \"temps-wg0\" counter drop")
                    .unwrap()
        );
        assert_ne!(
            mesh_lockdown_marker(&member),
            mesh_lockdown_marker(&hub),
            "becoming (or ceasing to be) the hub reinstalls the rules"
        );
    }

    #[test]
    fn a_member_that_is_not_the_hub_forwards_published_ports_but_never_relays() {
        // Forwarding is on for the tunnel on every member (the kernel would
        // otherwise drop DNAT'd traffic to published ports arriving on it),
        // so this chain alone keeps a non-hub from relaying.
        let mesh: ipnet::Ipv4Net = "10.201.0.0/24".parse().unwrap();
        let rules = render_mesh_lockdown(&MeshLockdown {
            vxlan_port: 4789,
            mesh,
            node_api_port: None,
            relay: false,
        });
        let from_tunnel: Vec<&str> = rules
            .lines()
            .filter(|line| line.contains(" forward iifname \"temps-wg0\""))
            .collect();
        assert_eq!(
            from_tunnel,
            vec![
                "add rule inet temps_mesh forward iifname \"temps-wg0\" ip saddr 10.201.0.0/24 ct status dnat accept",
                "add rule inet temps_mesh forward iifname \"temps-wg0\" counter drop",
            ],
            "only DNAT'd connections (published ports) are routed in from the mesh"
        );
        assert!(!rules.contains("oifname \"temps-wg0\" ip saddr"));
    }

    #[test]
    fn forgetting_the_relay_check_makes_the_next_one_run() {
        // A recreated or unreconciled interface must not ride on a relay
        // check verified on the old one.
        let mesh: ipnet::Ipv4Net = "10.201.0.0/24".parse().unwrap();
        *LAST_RELAY.lock().unwrap() = Some(((mesh, true), std::time::Instant::now()));
        forget_mesh_relay();
        assert!(LAST_RELAY.lock().unwrap().is_none());
    }

    #[test]
    fn only_forwarding_that_is_on_is_cached() {
        // A non-hub whose forwarding write failed, or whose interface is not
        // up yet, retries on the next tick instead of waiting out
        // RELAY_RECHECK with published ports unreachable over the mesh.
        use crate::linux::sysctl::InterfaceForwarding;
        let mesh: ipnet::Ipv4Net = "10.201.0.0/24".parse().unwrap();
        let failed_write = || {
            Err(NetworkError::Io {
                op: "write",
                path: "/proc/sys/net/ipv4/conf/temps-wg0/forwarding".into(),
                reason: "resource busy".into(),
            })
        };
        assert_eq!(
            settle_mesh_forwarding(mesh, false, Ok(InterfaceForwarding::Enabled)).check(),
            RelayCheck::Verified
        );
        assert_eq!(
            settle_mesh_forwarding(mesh, false, failed_write()).check(),
            RelayCheck::Retry
        );
        assert_eq!(
            settle_mesh_forwarding(mesh, false, Ok(InterfaceForwarding::NoInterface)).check(),
            RelayCheck::Retry
        );
    }

    #[test]
    fn mesh_forwarding_failures_stop_only_the_hub() {
        use crate::linux::sysctl::InterfaceForwarding;
        let mesh: ipnet::Ipv4Net = "10.201.0.0/24".parse().unwrap();
        let io = || NetworkError::Io {
            op: "write",
            path: "/proc/sys/net/ipv4/conf/temps-wg0/forwarding".into(),
            reason: "read-only file system".into(),
        };
        for relay in [false, true] {
            assert!(matches!(
                settle_mesh_forwarding(mesh, relay, Ok(InterfaceForwarding::Enabled)),
                MeshForwarding::Ready
            ));
        }
        assert!(matches!(
            settle_mesh_forwarding(mesh, false, Ok(InterfaceForwarding::NoInterface)),
            MeshForwarding::NoInterface
        ));
        let MeshForwarding::Failed(error) =
            settle_mesh_forwarding(mesh, true, Ok(InterfaceForwarding::NoInterface))
        else {
            panic!("a hub without the interface cannot relay");
        };
        assert!(error.to_string().contains("temps-wg0"));
        assert!(error.to_string().contains("10.201.0.0/24"));
        assert!(matches!(
            settle_mesh_forwarding(mesh, false, Err(io())),
            MeshForwarding::Degraded(_)
        ));
        assert!(matches!(
            settle_mesh_forwarding(mesh, true, Err(io())),
            MeshForwarding::Failed(_)
        ));
    }

    #[test]
    fn docker_user_probes_tell_absent_from_unknown() {
        assert_eq!(classify_chain_probe(Some(0), ""), ChainProbe::Present);
        for absent in [
            "iptables: No chain/target/match by that name.\n",
            "iptables v1.8.9 (nf_tables): chain `DOCKER-USER' in table `filter' does not exist\n",
            "iptables v1.8.7 (legacy): can't initialize iptables table `filter': Table does not exist (do you need to insmod?)\n",
        ] {
            assert_eq!(classify_chain_probe(Some(1), absent), ChainProbe::Absent);
        }
        for failed in [
            (
                Some(1),
                "iptables v1.8.7 (nf_tables): table `filter' is incompatible, use 'nft' tool.\n",
            ),
            (
                Some(4),
                "Another app is currently holding the xtables lock.\n",
            ),
            (
                Some(3),
                "can't initialize iptables table `filter': Permission denied (you must be root)\n",
            ),
            (None, ""),
        ] {
            assert!(
                matches!(
                    classify_chain_probe(failed.0, failed.1),
                    ChainProbe::Failed(_)
                ),
                "{failed:?} must not read as an absent chain"
            );
        }
    }

    #[test]
    fn the_relay_rule_goes_wherever_docker_user_exists_and_unknown_is_not_absent() {
        use ChainProbe::*;
        let failed = || Failed("exit status Some(4): xtables lock".into());

        // Docker programmed the backend `iptables` does not point at.
        assert_eq!(
            docker_user_decision(&[
                ("iptables", Absent),
                ("iptables-nft", Absent),
                ("iptables-legacy", Present),
            ]),
            DockerUserDecision::Install(vec!["iptables-legacy"])
        );
        // A failing backend does not hide one that answered.
        assert_eq!(
            docker_user_decision(&[
                ("iptables", Present),
                ("iptables-nft", Present),
                ("iptables-legacy", failed()),
            ]),
            DockerUserDecision::Install(vec!["iptables", "iptables-nft"])
        );
        // No Docker, or no iptables at all: nothing would drop relayed traffic.
        assert_eq!(
            docker_user_decision(&[
                ("iptables", Absent),
                ("iptables-nft", NoBinary),
                ("iptables-legacy", Absent),
            ]),
            DockerUserDecision::NoDocker
        );
        assert_eq!(
            docker_user_decision(&[
                ("iptables", NoBinary),
                ("iptables-nft", NoBinary),
                ("iptables-legacy", NoBinary),
            ]),
            DockerUserDecision::NoDocker
        );
        // A probe that failed is not proof Docker is absent.
        let DockerUserDecision::Unknown(reason) = docker_user_decision(&[
            ("iptables", failed()),
            ("iptables-nft", Absent),
            ("iptables-legacy", NoBinary),
        ]) else {
            panic!("an unanswered probe must not read as no Docker");
        };
        assert!(reason.contains("iptables: exit status Some(4)"), "{reason}");
    }

    #[test]
    fn the_overlay_baseline_leaves_the_mesh_to_its_own_table() {
        let alloc = NodeAlloc {
            node_id: Uuid::nil(),
            compute_cidr: Ipv4Net::from_str("172.20.5.0/24").unwrap(),
            bridge_address: IpAddr::V4(Ipv4Addr::new(172, 20, 5, 1)),
            underlay_address: IpAddr::V4(Ipv4Addr::new(10, 201, 0, 2)),
        };
        let mesh = NetworkConfig {
            underlay_dev: "temps-wg0".into(),
            ..NetworkConfig::default()
        };
        let script = render_baseline(&mesh, &alloc, &[]);
        assert!(!script.contains("forward iifname \"temps-wg0\""));
        assert!(script.contains("iifname != \"temps-wg0\" udp dport 4789 counter drop"));
        let lan = render_baseline(&NetworkConfig::default(), &alloc, &[]);
        assert!(
            !lan.contains("iifname !="),
            "a LAN underlay may deliver VXLAN on another device"
        );
    }

    #[test]
    fn baseline_marker_is_stable_across_peer_order() {
        let cfg = NetworkConfig::default();
        let alloc = NodeAlloc {
            node_id: Uuid::nil(),
            compute_cidr: Ipv4Net::from_str("172.20.5.0/24").unwrap(),
            bridge_address: IpAddr::V4(Ipv4Addr::new(172, 20, 5, 1)),
            underlay_address: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
        };
        let a = Peer {
            node_id: Uuid::from_u128(1),
            compute_cidr: Ipv4Net::from_str("172.20.6.0/24").unwrap(),
            underlay_address: "10.0.0.2".parse().unwrap(),
        };
        let b = Peer {
            node_id: Uuid::from_u128(2),
            compute_cidr: Ipv4Net::from_str("172.20.7.0/24").unwrap(),
            underlay_address: "10.0.0.3".parse().unwrap(),
        };
        assert_eq!(
            baseline_marker(&cfg, &alloc, &[a.clone(), b.clone()]),
            baseline_marker(&cfg, &alloc, &[b, a])
        );
        assert!(baseline_marker(&cfg, &alloc, &[]).starts_with("temps-baseline-v5-"));
    }

    #[test]
    fn baseline_snat_gives_dual_network_services_a_symmetric_return_path() {
        let cfg = NetworkConfig::default();
        let alloc = NodeAlloc {
            node_id: Uuid::nil(),
            compute_cidr: "172.20.255.0/24".parse().unwrap(),
            bridge_address: "172.20.255.1".parse().unwrap(),
            underlay_address: "10.200.4.1".parse().unwrap(),
        };
        let peer = Peer {
            node_id: Uuid::from_u128(1),
            compute_cidr: "172.20.0.0/24".parse().unwrap(),
            underlay_address: "10.200.4.2".parse().unwrap(),
        };

        let script = render_baseline(&cfg, &alloc, &[peer]);
        assert!(script.contains(
            "postrouting ip saddr 172.20.0.0/24 ip daddr 172.20.255.0/24 snat to 172.20.255.1"
        ));
    }

    #[test]
    fn overlay_forward_rules_are_scoped_to_local_and_peer_cidrs() {
        let cfg = NetworkConfig::default();
        let alloc = NodeAlloc {
            node_id: Uuid::nil(),
            compute_cidr: "172.20.255.0/24".parse().unwrap(),
            bridge_address: "172.20.255.1".parse().unwrap(),
            underlay_address: "10.200.4.1".parse().unwrap(),
        };
        let peer = Peer {
            node_id: Uuid::from_u128(1),
            compute_cidr: "172.20.0.0/24".parse().unwrap(),
            underlay_address: "10.200.4.2".parse().unwrap(),
        };
        let rules = desired_overlay_rules(&cfg, &alloc, &[peer]);
        assert!(rules.contains(&OverlayForwardRule {
            physical_input: Some("vxlan-temps0".into()),
            output: None,
            source: "172.20.0.0/24".into(),
            destination: "172.20.255.0/24".into(),
        }));
        assert_eq!(rules.len(), 1, "local egress remains owned by Docker");
    }

    #[test]
    fn parses_owned_iptables_rule_semantically() {
        let tokens = "-A TEMPS_OVERLAY_FORWARD -m physdev --physdev-is-bridged --physdev-in vxlan-temps0 -s 172.20.0.0/24 -d 172.20.255.0/24 -m comment --comment temps-overlay-forward-rule-v1 -j ACCEPT"
            .split_ascii_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert_eq!(
            parse_overlay_rule(&tokens),
            Some(OverlayForwardRule {
                physical_input: Some("vxlan-temps0".into()),
                output: None,
                source: "172.20.0.0/24".into(),
                destination: "172.20.255.0/24".into(),
            })
        );
    }

    #[test]
    fn recognizes_exact_legacy_overlay_rule_for_safe_migration() {
        let tokens = "-A TEMPS_OVERLAY_FORWARD -s 172.20.0.0/24 -d 172.20.255.0/24 -i vxlan-temps0 -m comment --comment temps-overlay-forward-rule-v1 -j ACCEPT"
            .split_ascii_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert_eq!(
            parse_legacy_overlay_rule(&tokens),
            Some(OverlayForwardRule {
                physical_input: Some("vxlan-temps0".into()),
                output: None,
                source: "172.20.0.0/24".into(),
                destination: "172.20.255.0/24".into(),
            })
        );
    }

    #[test]
    fn rejects_broader_rules_as_legacy_migrations() {
        for rule in [
            "-A TEMPS_OVERLAY_FORWARD ! -i vxlan-temps0 -s 172.20.0.0/24 -d 172.20.255.0/24 -m comment --comment temps-overlay-forward-rule-v1 -j ACCEPT",
            "-A TEMPS_OVERLAY_FORWARD -i vxlan-temps0 -s 172.20.0.0/24 -d 172.20.255.0/24 -m comment --comment broader -j ACCEPT",
            "-A TEMPS_OVERLAY_FORWARD -i vxlan-temps0 -s 172.20.0.0/24 -d 172.20.255.0/24 -p tcp -m comment --comment temps-overlay-forward-rule-v1 -j ACCEPT",
        ] {
            let tokens = rule
                .split_ascii_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>();
            assert_eq!(parse_legacy_overlay_rule(&tokens), None);
        }
    }

    #[test]
    fn rejects_negated_or_extended_owned_rules() {
        for rule in [
            "-A TEMPS_OVERLAY_FORWARD ! -i vxlan-temps0 -o br-temps0 -s 172.20.0.0/24 -d 172.20.255.0/24 -j ACCEPT",
            "-A TEMPS_OVERLAY_FORWARD -i vxlan-temps0 -o br-temps0 ! -s 172.20.0.0/24 -d 172.20.255.0/24 -j ACCEPT",
            "-A TEMPS_OVERLAY_FORWARD -i vxlan-temps0 -o br-temps0 -s 172.20.0.0/24 -d 172.20.255.0/24 -m comment --comment broader -j ACCEPT",
            "-A TEMPS_OVERLAY_FORWARD -i vxlan-temps0 -o br-temps0 -s 172.20.0.0/24 -d 172.20.255.0/24 -m comment --comment temps-overlay-forward-rule-v1 -p tcp -j ACCEPT",
        ] {
            let tokens = rule
                .split_ascii_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>();
            assert_eq!(parse_overlay_rule(&tokens), None);
        }
    }
}
