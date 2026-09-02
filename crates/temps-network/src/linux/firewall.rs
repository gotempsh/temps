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
use std::collections::HashSet;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{debug, info};
use uuid::Uuid;

const TABLE: &str = "temps_network";
const DOCKER_USER_CHAIN: &str = "DOCKER-USER";
const OVERLAY_FORWARD_CHAIN: &str = "TEMPS_OVERLAY_FORWARD";
const OWNER_COMMENT: &str = "temps-overlay-forward-owner-v1";
const RULE_COMMENT: &str = "temps-overlay-forward-rule-v1";
const HOOK_COMMENT: &str = "temps-overlay-forward-hook-v1";

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
        if let Some(output) = &self.output {
            args.extend(["-o".to_string(), output.clone()]);
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
    const BASELINE_SCHEMA_VERSION: &str = "v2";

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
        assert!(baseline_marker(&cfg, &alloc, &[]).starts_with("temps-baseline-v2-"));
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
