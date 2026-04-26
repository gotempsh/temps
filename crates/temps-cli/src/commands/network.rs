//! `temps network` — operator visibility into the multi-host overlay.
//!
//! This command is purely additive. `temps join` is unchanged; the only
//! way the overlay gets enabled on a node is when the control plane
//! allocates a `compute_cidr` for it. These subcommands let an operator
//! inspect the resulting state.
//!
//! Subcommands:
//!   - `temps network status` — local kernel data plane (bridge, vxlan,
//!     route table, FDB count, nftables table)
//!   - `temps network peers`  — peer list as fetched from the control
//!     plane via the same endpoint the agent's sync loop uses
//!   - `temps network diag`   — ICMP/UDP reachability check against each
//!     peer's bridge_address

use std::process::Command as ProcCommand;

use clap::{Args, Subcommand};
use colored::Colorize;
use serde::Deserialize;

/// Inspect the multi-host overlay on this node and across the cluster.
#[derive(Args)]
pub struct NetworkCommand {
    #[command(subcommand)]
    pub command: NetworkSubcommand,
}

#[derive(Subcommand)]
pub enum NetworkSubcommand {
    /// Show the local overlay state: bridge, vxlan device, routes, fdb,
    /// nftables baseline. Run on a worker node.
    Status(NetworkStatusCommand),
    /// Show this node's compute_cidr and the peer list as known to the
    /// control plane.
    Peers(NetworkPeersCommand),
    /// Diagnose connectivity to each peer (ICMP echo to peer bridge IP).
    Diag(NetworkDiagCommand),
}

#[derive(Args)]
pub struct NetworkStatusCommand {
    /// Bridge name (default: br-temps0)
    #[arg(long, default_value = "br-temps0")]
    pub bridge: String,
    /// VXLAN device name (default: vxlan-temps0)
    #[arg(long, default_value = "vxlan-temps0")]
    pub vxlan: String,
    /// nftables table name (default: temps_network)
    #[arg(long, default_value = "temps_network")]
    pub nft_table: String,
}

#[derive(Args)]
pub struct NetworkPeersCommand {
    /// Control plane URL (defaults to TEMPS_CONTROL_PLANE_URL or saved
    /// agent.json).
    #[arg(long, env = "TEMPS_CONTROL_PLANE_URL")]
    pub control_plane_url: Option<String>,
    /// Node id (defaults to TEMPS_NODE_ID or saved agent.json).
    #[arg(long, env = "TEMPS_NODE_ID")]
    pub node_id: Option<i32>,
    /// Bearer token (defaults to TEMPS_AGENT_TOKEN or saved agent.json).
    #[arg(long, env = "TEMPS_AGENT_TOKEN", hide = true)]
    pub token: Option<String>,
}

#[derive(Args)]
pub struct NetworkDiagCommand {
    /// Same source-of-truth as `peers`.
    #[arg(long, env = "TEMPS_CONTROL_PLANE_URL")]
    pub control_plane_url: Option<String>,
    #[arg(long, env = "TEMPS_NODE_ID")]
    pub node_id: Option<i32>,
    #[arg(long, env = "TEMPS_AGENT_TOKEN", hide = true)]
    pub token: Option<String>,
    /// ICMP ping count per peer (default: 3).
    #[arg(long, default_value = "3")]
    pub count: u32,
}

// ---------------------------------------------------------------------------
// Wire types — copied from `temps-deployments::handlers::network` deliberately
// so the CLI doesn't pull in the (very heavy) deployments crate.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct WirePeerListResponse {
    #[serde(default)]
    alloc: Option<WireAlloc>,
    #[serde(default)]
    peers: Vec<WirePeer>,
}

#[derive(Debug, Clone, Deserialize)]
struct WireAlloc {
    node_id: String,
    compute_cidr: String,
    bridge_address: String,
    underlay_address: String,
}

#[derive(Debug, Clone, Deserialize)]
struct WirePeer {
    node_id: String,
    compute_cidr: String,
    underlay_address: String,
}

// ---------------------------------------------------------------------------
// Saved agent config — same shape used by `temps agent`. Letting the
// network CLI reuse `~/.temps/agent.json` means operators can run
// `temps network peers` with no flags on a worker node.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct SavedAgentConfig {
    token: String,
    control_plane_url: String,
    node_id: i32,
}

fn load_saved_agent_config() -> Option<SavedAgentConfig> {
    let home = dirs::home_dir()?;
    let path = home.join(".temps").join("agent.json");
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

struct ResolvedAuth {
    control_plane_url: String,
    node_id: i32,
    token: String,
}

fn resolve_auth(
    cli_url: Option<String>,
    cli_node_id: Option<i32>,
    cli_token: Option<String>,
) -> anyhow::Result<ResolvedAuth> {
    let saved = load_saved_agent_config();
    let control_plane_url = cli_url
        .or_else(|| saved.as_ref().map(|s| s.control_plane_url.clone()))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "missing --control-plane-url (or TEMPS_CONTROL_PLANE_URL). \
                 Run `temps join` first or pass the flag."
            )
        })?;
    let node_id = cli_node_id
        .or_else(|| saved.as_ref().map(|s| s.node_id))
        .ok_or_else(|| anyhow::anyhow!("missing --node-id"))?;
    let token = cli_token
        .or_else(|| saved.as_ref().map(|s| s.token.clone()))
        .ok_or_else(|| anyhow::anyhow!("missing --token"))?;
    Ok(ResolvedAuth {
        control_plane_url,
        node_id,
        token,
    })
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

impl NetworkCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async {
            match self.command {
                NetworkSubcommand::Status(c) => execute_status(c),
                NetworkSubcommand::Peers(c) => execute_peers(c).await,
                NetworkSubcommand::Diag(c) => execute_diag(c).await,
            }
        })
    }
}

// ---------------------------------------------------------------------------
// status — local kernel state
// ---------------------------------------------------------------------------

fn execute_status(cmd: NetworkStatusCommand) -> anyhow::Result<()> {
    println!();
    println!(
        "  {}",
        "Multi-host overlay status (this node)"
            .bright_white()
            .bold()
    );
    println!("  {}", "─".repeat(60).bright_black());

    print_link("bridge", &cmd.bridge);
    print_link("vxlan", &cmd.vxlan);

    print_section("Routes:");
    print_routes(&cmd.vxlan);

    print_section("FDB entries:");
    print_fdb(&cmd.vxlan);

    print_section("nftables table:");
    print_nft_table(&cmd.nft_table);

    println!();
    Ok(())
}

fn print_link(label: &str, name: &str) {
    let out = ProcCommand::new("ip")
        .args(["-d", "link", "show", name])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            println!(
                "  {} {}",
                format!("{}:", label).bright_white(),
                name.bright_green()
            );
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                println!("    {}", line.dimmed());
            }
        }
        Ok(_) => {
            println!(
                "  {} {} {}",
                format!("{}:", label).bright_white(),
                name.bright_red(),
                "(not found)".bright_black()
            );
        }
        Err(e) => {
            println!(
                "  {} {} ({})",
                format!("{}:", label).bright_white(),
                name.bright_red(),
                e
            );
        }
    }
}

fn print_section(label: &str) {
    println!();
    println!("  {}", label.bright_white());
}

fn print_routes(vxlan: &str) {
    let out = ProcCommand::new("ip")
        .args(["-4", "route", "show", "dev", vxlan])
        .output();
    match out {
        Ok(o) if o.status.success() && !o.stdout.is_empty() => {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                println!("    {}", line);
            }
        }
        Ok(_) => println!("    {}", "(no routes)".bright_black()),
        Err(e) => println!("    error: {}", e),
    }
}

fn print_fdb(vxlan: &str) {
    let out = ProcCommand::new("bridge")
        .args(["fdb", "show", "dev", vxlan])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let count = String::from_utf8_lossy(&o.stdout).lines().count();
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                println!("    {}", line);
            }
            println!("    {} entries", count.to_string().bright_white());
        }
        Ok(_) => println!("    {}", "(none)".bright_black()),
        Err(e) => println!("    error: {}", e),
    }
}

fn print_nft_table(table: &str) {
    let out = ProcCommand::new("nft")
        .args(["list", "table", "inet", table])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                println!("    {}", line.dimmed());
            }
        }
        Ok(_) => println!("    {}", "(table not present)".bright_black()),
        Err(e) => println!("    error: {}", e),
    }
}

// ---------------------------------------------------------------------------
// peers — control-plane view
// ---------------------------------------------------------------------------

async fn execute_peers(cmd: NetworkPeersCommand) -> anyhow::Result<()> {
    let auth = resolve_auth(cmd.control_plane_url, cmd.node_id, cmd.token)?;
    let resp = fetch_peers(&auth).await?;

    println!();
    if let Some(a) = &resp.alloc {
        println!("  {}", "Local allocation".bright_white().bold());
        println!("    {} {}", "node_id:".bright_white(), a.node_id);
        println!(
            "    {} {}",
            "compute_cidr:".bright_white(),
            a.compute_cidr.bright_green()
        );
        println!(
            "    {} {}",
            "bridge_address:".bright_white(),
            a.bridge_address
        );
        println!(
            "    {} {}",
            "underlay_address:".bright_white(),
            a.underlay_address
        );
    } else {
        println!(
            "  {}",
            "Multi-host networking is not enabled on this node.".bright_yellow()
        );
        println!(
            "  {}",
            "(compute_cidr has not been allocated by the control plane)".bright_black()
        );
    }

    println!();
    if resp.peers.is_empty() {
        println!("  {}", "No peers.".bright_black());
        println!();
        return Ok(());
    }

    println!("  {} ({})", "Peers".bright_white().bold(), resp.peers.len());
    println!("  {}", "─".repeat(72).bright_black());
    println!(
        "  {:<38} {:<18} {}",
        "NODE_ID".bright_white().bold(),
        "COMPUTE_CIDR".bright_white().bold(),
        "UNDERLAY".bright_white().bold(),
    );
    for p in &resp.peers {
        println!(
            "  {:<38} {:<18} {}",
            p.node_id,
            p.compute_cidr.bright_green(),
            p.underlay_address
        );
    }
    println!();
    Ok(())
}

// ---------------------------------------------------------------------------
// diag — ICMP reachability per peer
// ---------------------------------------------------------------------------

async fn execute_diag(cmd: NetworkDiagCommand) -> anyhow::Result<()> {
    let auth = resolve_auth(cmd.control_plane_url, cmd.node_id, cmd.token)?;
    let resp = fetch_peers(&auth).await?;

    let Some(_alloc) = resp.alloc else {
        println!(
            "  {}",
            "Multi-host networking is not enabled on this node — nothing to diagnose."
                .bright_yellow()
        );
        return Ok(());
    };

    if resp.peers.is_empty() {
        println!("  {}", "No peers to diagnose.".bright_black());
        return Ok(());
    }

    println!();
    println!("  {}", "Diagnosing peer reachability".bright_white().bold());
    println!("  {}", "─".repeat(60).bright_black());

    let mut failures = 0;
    for peer in &resp.peers {
        // We ping the *first usable host* of the peer's compute_cidr,
        // which is the peer's bridge_address by convention.
        let target = first_usable_host(&peer.compute_cidr).unwrap_or(peer.underlay_address.clone());
        let result = ping(&target, cmd.count);
        let status = if result {
            "✓ ok".bright_green()
        } else {
            failures += 1;
            "✗ FAIL".bright_red()
        };
        println!(
            "  {} {} → {} ({} via overlay)",
            status, peer.node_id, peer.compute_cidr, target
        );
    }
    println!();
    if failures > 0 {
        println!(
            "  {} {} peer(s) unreachable.",
            "WARN:".bright_yellow().bold(),
            failures
        );
        println!("  Run `temps network status` to inspect local kernel state.");
        std::process::exit(2);
    }
    Ok(())
}

/// "172.20.5.0/24" → "172.20.5.1" (first usable host = network + 1)
fn first_usable_host(cidr: &str) -> Option<String> {
    let (net, _) = cidr.split_once('/')?;
    let mut octets: Vec<u8> = net.split('.').filter_map(|p| p.parse().ok()).collect();
    if octets.len() != 4 {
        return None;
    }
    // Bump the last octet by 1; works for /24 and most reasonable smaller
    // prefixes. For /31 / /32 the result wouldn't be useful anyway.
    let last = octets.last_mut()?;
    *last = last.checked_add(1)?;
    Some(format!(
        "{}.{}.{}.{}",
        octets[0], octets[1], octets[2], octets[3]
    ))
}

fn ping(host: &str, count: u32) -> bool {
    let out = ProcCommand::new("ping")
        .args(["-c", &count.to_string(), "-W", "2", host])
        .output();
    matches!(out, Ok(o) if o.status.success())
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

async fn fetch_peers(auth: &ResolvedAuth) -> anyhow::Result<WirePeerListResponse> {
    let url = format!(
        "{}/api/internal/nodes/{}/network/peers",
        auth.control_plane_url.trim_end_matches('/'),
        auth.node_id
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let resp = client.get(&url).bearer_auth(&auth.token).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!(
            "control plane returned {}: {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        );
    }
    Ok(resp.json().await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_usable_host_basic() {
        assert_eq!(
            first_usable_host("172.20.5.0/24").as_deref(),
            Some("172.20.5.1")
        );
        assert_eq!(
            first_usable_host("10.50.0.0/16").as_deref(),
            Some("10.50.0.1")
        );
    }

    #[test]
    fn first_usable_host_handles_bad_input() {
        assert!(first_usable_host("not-a-cidr").is_none());
        assert!(first_usable_host("172.20.5.0").is_none());
        assert!(first_usable_host("172.20.5.0.0/24").is_none());
    }

    #[test]
    fn first_usable_host_no_overflow_panic() {
        // 255 +1 = None, not panic
        assert!(first_usable_host("172.20.5.255/24").is_none());
    }

    #[test]
    fn deserialize_wire_response_with_alloc() {
        let json = r#"{
            "alloc": {
                "node_id": "abc",
                "compute_cidr": "172.20.5.0/24",
                "bridge_address": "172.20.5.1",
                "underlay_address": "10.0.0.5"
            },
            "peers": [
                {
                    "node_id": "def",
                    "compute_cidr": "172.20.6.0/24",
                    "underlay_address": "10.0.0.6"
                }
            ]
        }"#;
        let r: WirePeerListResponse = serde_json::from_str(json).unwrap();
        assert!(r.alloc.is_some());
        assert_eq!(r.peers.len(), 1);
    }

    #[test]
    fn deserialize_wire_response_without_alloc() {
        let r: WirePeerListResponse = serde_json::from_str(r#"{"peers": []}"#).unwrap();
        assert!(r.alloc.is_none());
        assert!(r.peers.is_empty());
    }

    #[test]
    fn resolve_auth_requires_at_least_one_source() {
        let r = resolve_auth(None, None, None);
        // Without a saved config or flags, we expect the missing-url error.
        // Tests run without ~/.temps/agent.json on CI runners.
        if dirs::home_dir()
            .map(|h| h.join(".temps/agent.json").exists())
            .unwrap_or(false)
        {
            // dev box: skip — saved config exists.
            return;
        }
        assert!(r.is_err());
    }
}
