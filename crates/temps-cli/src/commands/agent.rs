//! `temps agent` subcommand — runs the worker agent HTTP server.
//!
//! Loads configuration from `~/.temps/agent.json` (saved by `temps join`).
//! CLI flags and environment variables override the saved config.

use clap::Args;
use std::path::PathBuf;
use std::sync::Arc;

/// Resolve the agent data directory (`TEMPS_DATA_DIR` env var, or
/// `~/.temps`, or `./` as a last resort). Used for the saved agent
/// config and the per-node DNS resolver snapshot (`<dir>/dns/zone.json`).
pub fn agent_data_dir() -> PathBuf {
    if let Ok(p) = std::env::var("TEMPS_DATA_DIR") {
        return PathBuf::from(p);
    }
    if let Some(home) = dirs::home_dir() {
        return home.join(".temps");
    }
    PathBuf::from(".temps")
}

/// Run the worker node agent server
#[derive(Args)]
pub struct AgentCommand {
    /// Listen address for the agent API
    #[arg(long, env = "TEMPS_AGENT_ADDRESS")]
    pub listen_address: Option<String>,

    /// Bearer token for authenticating control plane requests (env var only to avoid process list exposure)
    #[arg(long, env = "TEMPS_AGENT_TOKEN", hide = true)]
    pub token: Option<String>,

    /// Node name (must match what was registered with the control plane)
    #[arg(long, env = "TEMPS_NODE_NAME")]
    pub node_name: Option<String>,

    /// Control plane URL for registration and heartbeats
    #[arg(long, env = "TEMPS_CONTROL_PLANE_URL")]
    pub control_plane_url: Option<String>,

    /// Node ID assigned by the control plane
    #[arg(long, env = "TEMPS_NODE_ID")]
    pub node_id: Option<i32>,

    /// Node labels for scheduling (comma-separated key=value pairs, e.g., "region=us-east,gpu=true").
    /// Overrides labels from saved config. Sent in every heartbeat.
    #[arg(long, env = "TEMPS_NODE_LABELS", value_delimiter = ',')]
    pub labels: Vec<String>,
}

impl AgentCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;

        rt.block_on(async move {
            let config = self.resolve_config()?;

            let docker = bollard::Docker::connect_with_defaults()
                .map_err(|e| anyhow::anyhow!("Failed to connect to Docker: {}", e))?;

            let network_name = temps_core::NETWORK_NAME.clone();
            // Overlay network is always opted into on agents — the runtime
            // silently skips the dual-attach when the overlay isn't yet
            // bootstrapped (single-host clusters, or before the network
            // sync loop has run for the first time). Operators who really
            // need to disable can override via TEMPS_OVERLAY_NETWORK="".
            //
            // The default must match the network name `temps-network`
            // actually creates (`NetworkConfig::default().docker_network_name`),
            // which is `temps0`. The previous default `temps-overlay`
            // never matched any real network on the worker so app
            // containers were silently single-host attached.
            let overlay_network = std::env::var("TEMPS_OVERLAY_NETWORK")
                .unwrap_or_else(|_| temps_network::NetworkConfig::default().docker_network_name);
            // Shared peer-list slot. The agent's network_sync loop
            // refreshes it on every poll; both the deployer (for app
            // containers) and the agent's service handlers (for
            // user-managed services) read it to install per-peer
            // overlay routes inside each new container's netns.
            let overlay_peers: temps_agent::network_sync::SharedPeers =
                Arc::new(std::sync::RwLock::new(Vec::new()));
            // Shared bridge-IP slot, populated by network_sync once
            // the overlay bridge is up. The deployer reads it to set
            // each container's /etc/resolv.conf to the per-node
            // Hickory resolver, so `*.temps.local` FQDNs resolve.
            let overlay_bridge_address: Arc<std::sync::RwLock<Option<std::net::IpAddr>>> =
                Arc::new(std::sync::RwLock::new(None));

            let mut runtime_builder = temps_deployer::docker::DockerRuntime::new(
                Arc::new(docker.clone()),
                true,
                network_name,
            )
            .with_host_bind_address("0.0.0.0".to_string())
            .with_overlay_dns_slot(overlay_bridge_address.clone());
            if !overlay_network.is_empty() {
                runtime_builder = runtime_builder
                    .with_overlay_network(overlay_network)
                    .with_overlay_peers(overlay_peers.clone());
            }
            let docker_runtime = Arc::new(runtime_builder);

            let deployer: Arc<dyn temps_deployer::ContainerDeployer> = docker_runtime.clone();
            let builder: Arc<dyn temps_deployer::ImageBuilder> = docker_runtime;

            tracing::info!("Starting temps agent (node_id={})...", config.node_id);

            // Internal-zone route store (Option 1 sync). Hydrated from
            // disk so the agent serves correctly across restarts even
            // when the CP is briefly unreachable. The sync client below
            // long-polls the CP and applies snapshots into this store;
            // the internal edge proxy reads from it on every request.
            let route_snapshot_path = agent_data_dir().join("routes").join("snapshot.json");
            let route_store = Arc::new(temps_agent::route_store::RouteStore::new(
                route_snapshot_path,
            ));
            route_store.load_from_disk();

            // Spawn the long-poll sync client. Shutdown is wired to the
            // global notifier passed below; if the agent server exits,
            // the client stops on the next round.
            let route_sync_shutdown = Arc::new(tokio::sync::Notify::new());
            match temps_agent::route_sync_client::RouteSyncClient::new(
                config.control_plane_url.clone(),
                config.node_id,
                config.token.clone(),
                route_store.clone(),
                route_sync_shutdown.clone(),
            ) {
                Ok(client) => {
                    tokio::spawn(async move {
                        client.run().await;
                    });
                    tracing::info!("route sync client started");
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "failed to start route sync client; internal proxy will return 503"
                    );
                }
            }

            // Internal edge proxy bound to the overlay bridge gateway.
            // Watches two sources for the bridge IP, taking whichever
            // appears first:
            //   1. `overlay_bridge_address` — populated by network_sync
            //      after the CP returns this node's compute_cidr.
            //   2. The kernel directly (`br-temps0` interface) — works
            //      across CP-outage cold starts, because the bridge is
            //      preserved by the kernel even when the agent restarts
            //      with CP unreachable.
            // Without (2) the proxy would refuse to bind whenever the
            // agent reboots while the CP is down, which is exactly when
            // serving stale-but-correct routes from disk matters most.
            {
                let bridge_slot = overlay_bridge_address.clone();
                let store = route_store.clone();
                let proxy_shutdown = route_sync_shutdown.clone();
                tokio::spawn(async move {
                    let bridge_ip = loop {
                        if let Some(ip) = *bridge_slot.read().expect("bridge slot poisoned") {
                            break ip;
                        }
                        // Fallback path: the bridge IP is also visible
                        // in the kernel as the `br-temps0` interface
                        // address. Read it directly so the proxy can
                        // bind without waiting for CP to confirm the
                        // allocation.
                        if let Some(ip) = read_bridge_ip_from_kernel("br-temps0").await {
                            tracing::info!(
                                bridge = %ip,
                                "using kernel-derived bridge IP (CP slot not yet populated)"
                            );
                            break ip;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    };
                    if let Err(e) =
                        temps_agent::internal_proxy::spawn(bridge_ip, 80, store, proxy_shutdown)
                            .await
                    {
                        tracing::warn!(
                            error = %e,
                            "internal edge proxy failed to bind; internal HTTP routing disabled"
                        );
                    }
                });
            }

            temps_agent::server::start_agent_server(
                deployer,
                builder,
                Some(docker),
                config,
                overlay_peers,
                overlay_bridge_address,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Agent server error: {}", e))?;

            // Best-effort shutdown of the sync client on agent exit.
            route_sync_shutdown.notify_waiters();
            Ok(())
        })
    }

    /// Load config from `~/.temps/agent.json`, then overlay any CLI flags on top.
    fn resolve_config(&self) -> anyhow::Result<temps_agent::AgentConfig> {
        let saved = self.load_saved_config();

        let listen_address = self
            .listen_address
            .clone()
            .or_else(|| saved.as_ref().map(|c| c.listen_address.clone()))
            .unwrap_or_else(|| "127.0.0.1:3100".to_string());

        let token = self
            .token
            .clone()
            .or_else(|| saved.as_ref().map(|c| c.token.clone()))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Missing --token. Run 'temps join' first, or provide --token, --node-name, --control-plane-url, --node-id"
                )
            })?;

        let node_name = self
            .node_name
            .clone()
            .or_else(|| saved.as_ref().map(|c| c.node_name.clone()))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Missing --node-name. Run 'temps join' first, or provide --token, --node-name, --control-plane-url, --node-id"
                )
            })?;

        let control_plane_url = self
            .control_plane_url
            .clone()
            .or_else(|| saved.as_ref().map(|c| c.control_plane_url.clone()))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Missing --control-plane-url. Run 'temps join' first, or provide --token, --node-name, --control-plane-url, --node-id"
                )
            })?;

        let node_id = self
            .node_id
            .or_else(|| saved.as_ref().map(|c| c.node_id))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Missing --node-id. Run 'temps join' first, or provide --token, --node-name, --control-plane-url, --node-id"
                )
            })?;

        // Parse labels from CLI (key=value pairs) or fall back to saved config
        let labels = if !self.labels.is_empty() {
            let mut map = serde_json::Map::new();
            for label in &self.labels {
                if let Some((key, value)) = label.split_once('=') {
                    map.insert(
                        key.trim().to_string(),
                        serde_json::Value::String(value.trim().to_string()),
                    );
                }
            }
            serde_json::Value::Object(map)
        } else {
            saved
                .as_ref()
                .map(|c| c.labels.clone())
                .unwrap_or(serde_json::json!({}))
        };

        Ok(temps_agent::AgentConfig {
            listen_address,
            token,
            node_name,
            control_plane_url,
            node_id,
            labels,
            // Use the same data dir hierarchy the rest of the CLI uses
            // (`~/.temps` by default, overridable by TEMPS_DATA_DIR), so
            // the resolver snapshot lives next to other agent state.
            dns_data_dir: agent_data_dir().join("dns"),
        })
    }

    /// Try to load `~/.temps/agent.json`. Returns None if not found or unparsable.
    fn load_saved_config(&self) -> Option<temps_agent::AgentConfig> {
        let home = dirs::home_dir()?;
        let config_path = home.join(".temps").join("agent.json");
        let data = std::fs::read_to_string(&config_path).ok()?;
        match serde_json::from_str::<temps_agent::AgentConfig>(&data) {
            Ok(config) => {
                tracing::info!("Loaded agent config from {}", config_path.display());
                Some(config)
            }
            Err(e) => {
                tracing::warn!("Failed to parse {}: {}", config_path.display(), e);
                None
            }
        }
    }
}

/// Read the IPv4 address of an interface directly from the kernel.
/// Used as a fallback for cold-start: when the agent restarts while
/// the CP is unreachable, network_sync can't fetch the alloc, but the
/// kernel still has the bridge from the previous run. Returns None if
/// the interface doesn't exist, has no v4 address, or `ip` isn't on
/// PATH (single-host dev mode).
async fn read_bridge_ip_from_kernel(iface: &str) -> Option<std::net::IpAddr> {
    use tokio::process::Command;
    let out = Command::new("ip")
        .args(["-4", "-o", "addr", "show", "dev", iface])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // `ip -4 -o addr show dev br-temps0` produces:
    //   12: br-temps0    inet 172.20.0.1/24 brd ... scope global br-temps0\
    // We want the first `inet X.Y.Z.W/...` token's address part.
    for token in text.split_whitespace() {
        if let Some(addr) = token.strip_suffix(|c: char| c == '/' || c == ' ') {
            if let Ok(ip) = addr.parse::<std::net::Ipv4Addr>() {
                return Some(std::net::IpAddr::V4(ip));
            }
        }
        // Fall back: handle the "172.20.0.1/24" form directly.
        if let Some((addr_part, _)) = token.split_once('/') {
            if let Ok(ip) = addr_part.parse::<std::net::Ipv4Addr>() {
                return Some(std::net::IpAddr::V4(ip));
            }
        }
    }
    None
}
