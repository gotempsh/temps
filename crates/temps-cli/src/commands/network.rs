// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `temps network` — operator visibility into the multi-host overlay.
//!
//! `temps join` remains the worker enrollment path. These subcommands let an
//! operator configure/repair the control-plane side and inspect the resulting
//! state.
//!
//! Subcommands:
//!   - `temps network setup-multi-node` — idempotently join the control plane
//!     to the overlay and republish managed-service DNS without a restart
//!   - `temps network status` — local kernel data plane (bridge, vxlan,
//!     route table, FDB count, nftables table)
//!   - `temps network peers`  — peer list as fetched from the control
//!     plane via the same endpoint the agent's sync loop uses
//!   - `temps network diag`   — ICMP/UDP reachability check against each
//!     peer's bridge_address

use std::path::PathBuf;
use std::process::Command as ProcCommand;
use std::sync::Arc;

use clap::{Args, Subcommand};
use colored::Colorize;
use sea_orm::EntityTrait;
use serde::Deserialize;

/// Inspect the multi-host overlay on this node and across the cluster.
#[derive(Args)]
pub struct NetworkCommand {
    #[command(subcommand)]
    pub command: NetworkSubcommand,
}

#[derive(Subcommand)]
pub enum NetworkSubcommand {
    /// Configure or repair the control-plane side of multi-node networking.
    /// Existing services are attached and their internal DNS records are
    /// republished without restarting `temps serve`.
    #[command(alias = "setup-control-plane-network")]
    SetupMultiNode(SetupMultiNodeCommand),
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
pub struct SetupMultiNodeCommand {
    /// Database URL. Environment-only so credentials do not leak through the
    /// process list.
    #[arg(skip)]
    pub database_url: Option<String>,

    /// Private address of this control plane on the multi-node underlay.
    /// When omitted, the value saved by `temps serve --private-address` is
    /// used.
    #[arg(long, env = "TEMPS_PRIVATE_ADDRESS")]
    pub private_address: Option<String>,

    /// Interface carrying the private address (for example enp6s0.4000 or
    /// wg0). Normally detected from the address.
    #[arg(long, env = "TEMPS_UNDERLAY_DEV")]
    pub underlay_dev: Option<String>,

    /// Cluster-wide private pool carved into one Docker overlay subnet per
    /// node. It may only be changed before any node has an allocation.
    #[arg(long, env = "TEMPS_COMPUTE_POOL_CIDR")]
    pub compute_pool_cidr: Option<String>,

    /// Prefix allocated to each node (for example 24 gives 254 container
    /// addresses per node). It may only be changed with an unused pool.
    #[arg(long, env = "TEMPS_COMPUTE_SUBNET_PREFIX_LEN")]
    pub node_prefix_len: Option<u8>,

    /// Temps data directory containing the existing encryption_key.
    #[arg(long, env = "TEMPS_DATA_DIR")]
    pub data_dir: Option<PathBuf>,
}

#[derive(Args)]
pub struct NetworkStatusCommand {
    /// Docker overlay network name (default: temps0)
    #[arg(long, default_value = "temps0")]
    pub docker_network: String,
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
    network: Option<WireNetworkPool>,
    #[serde(default)]
    alloc: Option<WireAlloc>,
    #[serde(default)]
    peers: Vec<WirePeer>,
}

#[derive(Debug, Clone, Deserialize)]
struct WireNetworkPool {
    compute_pool_cidr: String,
    subnet_prefix_len: u8,
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
                NetworkSubcommand::SetupMultiNode(c) => execute_setup_multi_node(c).await,
                NetworkSubcommand::Status(c) => execute_status(c),
                NetworkSubcommand::Peers(c) => execute_peers(c).await,
                NetworkSubcommand::Diag(c) => execute_diag(c).await,
            }
        })
    }
}

async fn execute_setup_multi_node(cmd: SetupMultiNodeCommand) -> anyhow::Result<()> {
    let database_url = cmd
        .database_url
        .or_else(|| std::env::var("TEMPS_DATABASE_URL").ok())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("TEMPS_DATABASE_URL must be set in the protected process environment")
        })?;
    let db = temps_database::establish_connection(&database_url)
        .await
        .map_err(|error| anyhow::anyhow!("could not connect to the Temps database: {error}"))?;

    let allocator = temps_network::allocator::PostgresAllocator::new(db.clone());
    let current_network = allocator
        .cluster_config()
        .await
        .map_err(|error| anyhow::anyhow!("could not load cluster network config: {error}"))?;
    let requested_pool = cmd
        .compute_pool_cidr
        .as_deref()
        .map(str::parse)
        .transpose()
        .map_err(|error| anyhow::anyhow!("invalid --compute-pool-cidr: {error}"))?
        .unwrap_or(current_network.compute_pool_cidr);
    let requested_prefix = cmd
        .node_prefix_len
        .unwrap_or(current_network.subnet_prefix_len);
    let cluster_network = allocator
        .configure_pool(requested_pool, requested_prefix)
        .await
        .map_err(|error| anyhow::anyhow!("cluster network configuration refused: {error}"))?;

    let private_address = match cmd.private_address {
        Some(value) if !value.trim().is_empty() => value,
        _ => {
            let settings = temps_entities::settings::Entity::find_by_id(1)
                .one(db.as_ref())
                .await
                .map_err(|error| anyhow::anyhow!("could not load multi-node settings: {error}"))?
                .map(|model| temps_core::AppSettings::from_json(model.data))
                .unwrap_or_default();
            settings.multi_node.private_address.ok_or_else(|| {
                anyhow::anyhow!(
                    "the control-plane private address is not configured; pass \
                     --private-address <IP> or start once with \
                     `temps serve --private-address <IP>`"
                )
            })?
        }
    };

    let docker = Arc::new(
        bollard::Docker::connect_with_defaults()
            .map_err(|error| anyhow::anyhow!("could not connect to Docker: {error}"))?,
    );
    let overlay = temps_network::control_plane::setup(
        db.clone(),
        docker.as_ref(),
        private_address.trim(),
        cmd.underlay_dev.as_deref(),
    )
    .await
    .map_err(|error| anyhow::anyhow!("multi-node control-plane setup failed: {error}"))?;

    let data_dir = cmd
        .data_dir
        .or_else(|| std::env::var_os("TEMPS_DATA_DIR").map(PathBuf::from))
        .or_else(|| dirs::home_dir().map(|home| home.join(".temps")))
        .ok_or_else(|| anyhow::anyhow!("could not determine the Temps data directory"))?;
    let key_path = data_dir.join("encryption_key");
    let encryption_key = std::fs::read_to_string(&key_path).map_err(|error| {
        anyhow::anyhow!(
            "could not read the existing encryption key at {}: {error}; pass the same \
             --data-dir used by `temps serve`",
            key_path.display()
        )
    })?;
    let encryption = Arc::new(
        temps_core::EncryptionService::new(encryption_key.trim())
            .map_err(|error| anyhow::anyhow!("invalid Temps encryption key: {error}"))?,
    );
    let dns_registry = Arc::new(temps_dns::DnsRegistry::new(db.clone()));
    let services =
        temps_providers::ExternalServiceManager::new(db.clone(), encryption, docker, dns_registry);
    let existing = services
        .list_services()
        .await
        .map_err(|error| anyhow::anyhow!("could not list managed services: {error}"))?;

    let mut published = 0usize;
    let mut skipped = 0usize;
    for service in existing {
        match services.register_standalone_service_dns(service.id).await {
            Ok(Some(fqdn)) => {
                published += 1;
                println!("  {} {} -> {}", "DNS".bright_green(), service.name, fqdn);
            }
            Ok(None) => skipped += 1,
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "overlay is ready, but DNS reconciliation failed for service {} (id {}): {}",
                    service.name,
                    service.id,
                    error
                ));
            }
        }
    }

    println!();
    println!(
        "  {} Multi-node control-plane networking is ready",
        "PASS".bright_green().bold()
    );
    println!(
        "  Cluster pool: {} (one /{} Docker subnet per node)",
        cluster_network.compute_pool_cidr, cluster_network.subnet_prefix_len
    );
    println!("  Overlay CIDR: {}", overlay.alloc.compute_cidr);
    println!("  Bridge: {}", overlay.alloc.bridge_address);
    println!(
        "  Underlay: {} via {}",
        overlay.alloc.underlay_address, overlay.config.underlay_dev
    );
    println!("  Managed-service DNS: {published} published, {skipped} skipped");
    println!("  No `temps serve` restart is required.");
    Ok(())
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

    print_section("Docker overlay allocation:");
    print_docker_network(&cmd.docker_network);

    print_section("Routes:");
    // VXLAN peer CIDRs are deliberately routed through the bridge so
    // containers attached to the Docker bridge can use the routes directly.
    // Showing routes on the VXLAN device therefore reports a false empty
    // state even when reconciliation succeeded.
    print_routes(&cmd.bridge);

    print_section("FDB entries:");
    print_fdb(&cmd.vxlan);

    print_section("nftables table:");
    print_nft_table(&cmd.nft_table);

    println!();
    Ok(())
}

fn print_docker_network(network: &str) {
    let output = ProcCommand::new("docker")
        .args([
            "network",
            "inspect",
            network,
            "--format",
            "{{range .IPAM.Config}}{{.Subnet}} gateway={{.Gateway}}{{end}}",
        ])
        .output();
    match output {
        Ok(result) if result.status.success() => {
            let allocation = String::from_utf8_lossy(&result.stdout);
            println!("    {}: {}", network, allocation.trim().bright_green());
        }
        Ok(result) => println!(
            "    {}",
            format!(
                "{} not found or unreadable: {}",
                network,
                String::from_utf8_lossy(&result.stderr).trim()
            )
            .bright_black()
        ),
        Err(error) => println!("    could not run docker network inspect: {error}"),
    }
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
    if let Some(network) = &resp.network {
        println!(
            "  {}",
            "Authoritative cluster network".bright_white().bold()
        );
        println!(
            "    {} {}",
            "compute_pool:".bright_white(),
            network.compute_pool_cidr.bright_green()
        );
        println!(
            "    {} /{} per node",
            "allocation:".bright_white(),
            network.subnet_prefix_len
        );
        println!();
    }
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
    use clap::Parser;

    #[test]
    fn setup_multi_node_command_is_explicitly_scoped() {
        let cli = crate::Cli::try_parse_from([
            "temps",
            "network",
            "setup-multi-node",
            "--private-address",
            "10.200.4.1",
            "--compute-pool-cidr",
            "10.240.0.0/16",
            "--node-prefix-len",
            "24",
        ])
        .expect("multi-node setup arguments should parse");
        assert!(matches!(
            cli.command,
            crate::Commands::Network(NetworkCommand {
                command: NetworkSubcommand::SetupMultiNode(SetupMultiNodeCommand {
                    private_address: Some(ref address),
                    compute_pool_cidr: Some(ref pool),
                    node_prefix_len: Some(24),
                    ..
                }),
            }) if address == "10.200.4.1" && pool == "10.240.0.0/16"
        ));
    }

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
