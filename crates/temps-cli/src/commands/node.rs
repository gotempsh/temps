// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `temps node` subcommand — manage cluster worker nodes via the HTTP API.
//!
//! Provides CLI commands for listing, draining, and removing worker nodes.
//! Join is a separate top-level command (`temps join`).

use clap::{Args, Subcommand};
use colored::Colorize;
use serde::Deserialize;

/// Worker node management commands
#[derive(Args)]
pub struct NodeCommand {
    #[command(subcommand)]
    pub command: NodeSubcommand,
}

#[derive(Subcommand)]
pub enum NodeSubcommand {
    /// List all registered worker nodes
    #[command(alias = "ls")]
    List(NodeListCommand),
    /// Show details for a specific node
    Show(NodeShowCommand),
    /// Drain a node: stop scheduling new containers and redeploy existing ones
    Drain(NodeDrainCommand),
    /// Undrain a node: reactivate it so it can accept new deployments again
    Undrain(NodeUndrainCommand),
    /// Remove a node from the cluster (must be drained first)
    #[command(alias = "rm")]
    Remove(NodeRemoveCommand),
}

#[derive(Args)]
pub struct NodeListCommand {
    /// API base URL (e.g., "http://localhost:3001")
    #[arg(long, env = "TEMPS_API_URL")]
    pub api_url: String,
    /// API authentication token
    #[arg(long, env = "TEMPS_API_TOKEN")]
    pub api_token: String,
}

#[derive(Args)]
pub struct NodeShowCommand {
    /// Node ID
    pub node_id: i32,
    /// API base URL
    #[arg(long, env = "TEMPS_API_URL")]
    pub api_url: String,
    /// API authentication token
    #[arg(long, env = "TEMPS_API_TOKEN")]
    pub api_token: String,
}

#[derive(Args)]
pub struct NodeDrainCommand {
    /// Node ID to drain
    pub node_id: i32,
    /// API base URL
    #[arg(long, env = "TEMPS_API_URL")]
    pub api_url: String,
    /// API authentication token
    #[arg(long, env = "TEMPS_API_TOKEN")]
    pub api_token: String,
    /// Wait for drain to complete (all containers migrated off the node)
    #[arg(long)]
    pub wait: bool,
    /// Timeout in seconds when using --wait (default: 600)
    #[arg(long, default_value = "600")]
    pub timeout: u64,
}

#[derive(Args)]
pub struct NodeUndrainCommand {
    /// Node ID to undrain
    pub node_id: i32,
    /// API base URL
    #[arg(long, env = "TEMPS_API_URL")]
    pub api_url: String,
    /// API authentication token
    #[arg(long, env = "TEMPS_API_TOKEN")]
    pub api_token: String,
}

#[derive(Args)]
pub struct NodeRemoveCommand {
    /// Node ID to remove
    pub node_id: i32,
    /// API base URL
    #[arg(long, env = "TEMPS_API_URL")]
    pub api_url: String,
    /// API authentication token
    #[arg(long, env = "TEMPS_API_TOKEN")]
    pub api_token: String,
    /// Skip confirmation prompt
    #[arg(long)]
    pub yes: bool,
}

// ── API response types ──

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct NodeInfoResponse {
    id: i32,
    name: String,
    address: String,
    private_address: String,
    role: String,
    status: String,
    labels: serde_json::Value,
    capacity: serde_json::Value,
    last_heartbeat: Option<String>,
    created_at: String,
}

#[derive(Debug, Deserialize)]
struct NodeListResponse {
    nodes: Vec<NodeInfoResponse>,
    total: usize,
}

#[derive(Debug, Deserialize)]
struct DrainNodeResponse {
    name: String,
    status: String,
    affected_environments: usize,
    message: String,
}

#[derive(Debug, Deserialize)]
struct RemoveNodeResponse {
    message: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct DrainStatusApiResponse {
    node_id: i32,
    node_name: String,
    status: String,
    remaining_containers: usize,
    drain_complete: bool,
    can_remove: bool,
    message: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct NodeContainerListResponse {
    containers: Vec<serde_json::Value>,
    total: usize,
}

#[derive(Debug, Deserialize)]
struct ProblemDetail {
    title: Option<String>,
    detail: Option<String>,
}

// ── Helpers ──

fn api_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    format!("{}/api/internal{}", base, path)
}

fn make_client() -> reqwest::Client {
    // Strict TLS — CLI talks to the control plane over the public
    // internet. Skipping verification here would let a MitM steal the
    // user's session token. The server-side opt-in (AppSettings.insecure_tls)
    // does NOT apply to CLI binaries.
    reqwest::Client::builder()
        .build()
        .expect("Failed to build HTTP client")
}

async fn handle_api_error(response: reqwest::Response) -> anyhow::Error {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if let Ok(problem) = serde_json::from_str::<ProblemDetail>(&body) {
        anyhow::anyhow!(
            "API error ({}): {} - {}",
            status,
            problem.title.unwrap_or_default(),
            problem.detail.unwrap_or_default()
        )
    } else {
        anyhow::anyhow!("API error ({}): {}", status, body)
    }
}

fn format_relative_time(date_str: &str) -> String {
    let Ok(date) = chrono::DateTime::parse_from_rfc3339(date_str) else {
        return date_str.to_string();
    };
    let diff = chrono::Utc::now().signed_duration_since(date);
    let secs = diff.num_seconds();
    if secs < 60 {
        return format!("{}s ago", secs);
    }
    let mins = diff.num_minutes();
    if mins < 60 {
        return format!("{}m ago", mins);
    }
    let hours = diff.num_hours();
    if hours < 24 {
        return format!("{}h ago", hours);
    }
    format!("{}d ago", diff.num_days())
}

// ── Command execution ──

impl NodeCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async {
            match self.command {
                NodeSubcommand::List(cmd) => execute_list(cmd).await,
                NodeSubcommand::Show(cmd) => execute_show(cmd).await,
                NodeSubcommand::Drain(cmd) => execute_drain(cmd).await,
                NodeSubcommand::Undrain(cmd) => execute_undrain(cmd).await,
                NodeSubcommand::Remove(cmd) => execute_remove(cmd).await,
            }
        })
    }
}

async fn execute_list(cmd: NodeListCommand) -> anyhow::Result<()> {
    let client = make_client();
    let url = api_url(&cmd.api_url, "/nodes");

    let response = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", cmd.api_token))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to API: {}", e))?;

    if !response.status().is_success() {
        return Err(handle_api_error(response).await);
    }

    let data: NodeListResponse = response.json().await?;

    if data.nodes.is_empty() {
        println!("No worker nodes registered.");
        println!("Run {} to add a worker node.", "temps join".bright_cyan());
        return Ok(());
    }

    println!();
    println!(
        "  {:<5} {:<20} {:<10} {:<10} {:<18} {}",
        "ID".bright_white().bold(),
        "NAME".bright_white().bold(),
        "STATUS".bright_white().bold(),
        "ROLE".bright_white().bold(),
        "ADDRESS".bright_white().bold(),
        "HEARTBEAT".bright_white().bold(),
    );
    println!("  {}", "─".repeat(85).bright_black());

    for node in &data.nodes {
        let status_colored = match node.status.as_str() {
            "active" => node.status.bright_green(),
            "draining" => node.status.bright_yellow(),
            "offline" => node.status.bright_red(),
            _ => node.status.bright_white(),
        };

        let heartbeat = node
            .last_heartbeat
            .as_deref()
            .map(format_relative_time)
            .unwrap_or_else(|| "Never".to_string());

        println!(
            "  {:<5} {:<20} {:<10} {:<10} {:<18} {}",
            node.id.to_string().bright_white(),
            node.name.bright_cyan(),
            status_colored,
            node.role,
            node.private_address,
            heartbeat,
        );
    }

    println!();
    println!("  {} node(s) total", data.total);
    println!();
    Ok(())
}

async fn execute_show(cmd: NodeShowCommand) -> anyhow::Result<()> {
    let client = make_client();

    let node_url = api_url(&cmd.api_url, &format!("/nodes/{}", cmd.node_id));
    let containers_url = api_url(&cmd.api_url, &format!("/nodes/{}/containers", cmd.node_id));

    let (node_resp, containers_resp) = tokio::join!(
        client
            .get(&node_url)
            .header("Authorization", format!("Bearer {}", cmd.api_token))
            .send(),
        client
            .get(&containers_url)
            .header("Authorization", format!("Bearer {}", cmd.api_token))
            .send(),
    );

    let node_resp = node_resp.map_err(|e| anyhow::anyhow!("Failed to connect: {}", e))?;
    if !node_resp.status().is_success() {
        return Err(handle_api_error(node_resp).await);
    }
    let node: NodeInfoResponse = node_resp.json().await?;

    let status_colored = match node.status.as_str() {
        "active" => node.status.bright_green(),
        "draining" => node.status.bright_yellow(),
        "offline" => node.status.bright_red(),
        _ => node.status.bright_white(),
    };

    println!();
    println!(
        "  {} {}",
        "Node:".bright_white().bold(),
        node.name.bright_cyan()
    );
    println!("  {} {}", "ID:".bright_white(), node.id);
    println!("  {} {}", "Status:".bright_white(), status_colored);
    println!("  {} {}", "Role:".bright_white(), node.role);
    println!("  {} {}", "Address:".bright_white(), node.address);
    println!(
        "  {} {}",
        "Private IP:".bright_white(),
        node.private_address
    );
    println!(
        "  {} {}",
        "Heartbeat:".bright_white(),
        node.last_heartbeat
            .as_deref()
            .map(format_relative_time)
            .unwrap_or_else(|| "Never".to_string())
    );
    println!("  {} {}", "Created:".bright_white(), node.created_at);

    // Labels
    if let Some(labels) = node.labels.as_object() {
        if !labels.is_empty() {
            let label_str: Vec<String> = labels
                .iter()
                .map(|(k, v)| format!("{}={}", k, v.as_str().unwrap_or(&v.to_string())))
                .collect();
            println!("  {} {}", "Labels:".bright_white(), label_str.join(", "));
        }
    }

    // Containers
    if let Ok(resp) = containers_resp {
        if resp.status().is_success() {
            if let Ok(data) = resp.json::<NodeContainerListResponse>().await {
                println!();
                println!(
                    "  {} {} container(s)",
                    "Containers:".bright_white().bold(),
                    data.total
                );
                if data.total == 0 {
                    println!("  (none)");
                }
            }
        }
    }

    println!();
    Ok(())
}

async fn execute_drain(cmd: NodeDrainCommand) -> anyhow::Result<()> {
    let client = make_client();
    let url = api_url(&cmd.api_url, &format!("/nodes/{}/drain", cmd.node_id));

    println!(
        "  {} Draining node {}...",
        "⏳".bright_yellow(),
        cmd.node_id
    );

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", cmd.api_token))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to API: {}", e))?;

    if !response.status().is_success() {
        return Err(handle_api_error(response).await);
    }

    let data: DrainNodeResponse = response.json().await?;

    println!(
        "  {} Node '{}' is now {}",
        "✓".bright_green(),
        data.name.bright_cyan(),
        data.status.bright_yellow()
    );
    println!("  {}", data.message);

    if cmd.wait && data.affected_environments > 0 {
        println!();
        println!(
            "  {} Waiting for containers to migrate off node...",
            "⏳".bright_yellow()
        );

        let drain_status_url = api_url(&cmd.api_url, &format!("/nodes/{}/drain", cmd.node_id));
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(cmd.timeout);

        loop {
            if start.elapsed() > timeout {
                println!(
                    "\n  {} Timeout after {}s. Node still has containers.",
                    "⚠".bright_yellow(),
                    cmd.timeout
                );
                println!(
                    "  Check status with: {} {}",
                    "temps node show".bright_cyan(),
                    cmd.node_id
                );
                return Ok(());
            }

            tokio::time::sleep(std::time::Duration::from_secs(10)).await;

            let resp = client
                .get(&drain_status_url)
                .header("Authorization", format!("Bearer {}", cmd.api_token))
                .send()
                .await;

            if let Ok(resp) = resp {
                if resp.status().is_success() {
                    if let Ok(status) = resp.json::<DrainStatusApiResponse>().await {
                        if status.drain_complete {
                            println!(
                                "\n  {} Drain complete! All containers migrated off node.",
                                "✓".bright_green()
                            );
                            if status.can_remove {
                                println!(
                                    "  Node can be safely removed with: {} {}",
                                    "temps node remove".bright_cyan(),
                                    cmd.node_id
                                );
                            }
                            return Ok(());
                        }
                        print!(
                            "\r  {} {} container(s) remaining...    ",
                            "⏳".bright_yellow(),
                            status.remaining_containers
                        );
                    }
                }
            }
        }
    }

    Ok(())
}

#[derive(Debug, Deserialize)]
struct UndrainNodeResponse {
    name: String,
    status: String,
    message: String,
}

async fn execute_undrain(cmd: NodeUndrainCommand) -> anyhow::Result<()> {
    let client = make_client();
    let url = api_url(&cmd.api_url, &format!("/nodes/{}/drain", cmd.node_id));

    println!(
        "  {} Undraining node {}...",
        "⏳".bright_yellow(),
        cmd.node_id
    );

    let response = client
        .delete(&url)
        .header("Authorization", format!("Bearer {}", cmd.api_token))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to API: {}", e))?;

    if !response.status().is_success() {
        return Err(handle_api_error(response).await);
    }

    let data: UndrainNodeResponse = response.json().await?;

    println!(
        "  {} Node '{}' is now {}",
        "✓".bright_green(),
        data.name.bright_cyan(),
        data.status.bright_green()
    );
    println!("  {}", data.message);

    Ok(())
}

async fn execute_remove(cmd: NodeRemoveCommand) -> anyhow::Result<()> {
    if !cmd.yes {
        // Show node info first
        let client = make_client();
        let show_url = api_url(&cmd.api_url, &format!("/nodes/{}", cmd.node_id));
        let resp = client
            .get(&show_url)
            .header("Authorization", format!("Bearer {}", cmd.api_token))
            .send()
            .await;

        if let Ok(resp) = resp {
            if let Ok(node) = resp.json::<NodeInfoResponse>().await {
                println!(
                    "  {} Are you sure you want to remove node '{}' (id={})? Use {} to confirm.",
                    "⚠".bright_yellow(),
                    node.name.bright_cyan(),
                    node.id,
                    "--yes".bright_white()
                );
                return Ok(());
            }
        }

        println!(
            "  {} Are you sure you want to remove node {}? Use {} to confirm.",
            "⚠".bright_yellow(),
            cmd.node_id,
            "--yes".bright_white()
        );
        return Ok(());
    }

    let client = make_client();
    let url = api_url(&cmd.api_url, &format!("/nodes/{}", cmd.node_id));

    let response = client
        .delete(&url)
        .header("Authorization", format!("Bearer {}", cmd.api_token))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to API: {}", e))?;

    if !response.status().is_success() {
        return Err(handle_api_error(response).await);
    }

    let data: RemoveNodeResponse = response.json().await?;
    println!("  {} {}", "✓".bright_green(), data.message);

    Ok(())
}
