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
            let overlay_network = std::env::var("TEMPS_OVERLAY_NETWORK")
                .unwrap_or_else(|_| "temps-overlay".to_string());
            let mut runtime_builder = temps_deployer::docker::DockerRuntime::new(
                Arc::new(docker.clone()),
                true,
                network_name,
            )
            .with_host_bind_address("0.0.0.0".to_string());
            if !overlay_network.is_empty() {
                runtime_builder = runtime_builder.with_overlay_network(overlay_network);
            }
            let docker_runtime = Arc::new(runtime_builder);

            let deployer: Arc<dyn temps_deployer::ContainerDeployer> = docker_runtime.clone();
            let builder: Arc<dyn temps_deployer::ImageBuilder> = docker_runtime;

            tracing::info!("Starting temps agent (node_id={})...", config.node_id);

            temps_agent::server::start_agent_server(deployer, builder, Some(docker), config)
                .await
                .map_err(|e| anyhow::anyhow!("Agent server error: {}", e))?;

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
