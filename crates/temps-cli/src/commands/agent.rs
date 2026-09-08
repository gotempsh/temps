// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `temps agent` subcommand — runs the worker agent HTTP server.
//!
//! Loads configuration from `~/.temps/agent.json` (saved by `temps join`).
//! CLI flags and environment variables override the saved config.

use clap::Args;
use std::path::PathBuf;
use std::sync::Arc;

const MAX_AGENT_WORKER_THREADS: usize = 8;

fn agent_worker_threads(available_parallelism: usize) -> usize {
    available_parallelism.clamp(1, MAX_AGENT_WORKER_THREADS)
}

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

    /// Network device the VXLAN overlay should bind to as its underlay
    /// parent (e.g. "enp6s0"). Overrides the saved config. Defaults to
    /// auto-detecting the device carrying this host's IPv4 default route.
    #[arg(long, env = "TEMPS_AGENT_UNDERLAY_DEV")]
    pub underlay_dev: Option<String>,

    /// Optional MTU ceiling for the overlay underlay. Defaults to reading the
    /// selected interface's MTU from the kernel. Set this only when the path
    /// MTU is lower than the interface advertises.
    #[arg(long, env = "TEMPS_AGENT_UNDERLAY_MTU")]
    pub underlay_mtu: Option<u32>,

    /// This node's private/underlay address, as registered with the control
    /// plane during `temps join` (`nodes.private_address`) — the WireGuard
    /// tunnel IP in relay mode, or the user-managed address in direct mode.
    /// Published Docker container ports are bound to this address only,
    /// never to "0.0.0.0", so deployed containers are reachable from the
    /// control-plane proxy over the private network but never on this
    /// node's public interface. Overrides the saved config; must match what
    /// was registered with the control plane, since that's the address the
    /// proxy dials for this node.
    #[arg(long, env = "TEMPS_AGENT_PRIVATE_ADDRESS")]
    pub private_address: Option<String>,
}

impl AgentCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        let available_parallelism = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1);
        let worker_threads = agent_worker_threads(available_parallelism);
        let rt = tokio::runtime::Builder::new_multi_thread()
            // The agent is predominantly network and Docker-socket I/O. Tokio's
            // default of one worker per logical CPU needlessly multiplies
            // thread stacks and allocator arenas on large worker hosts.
            .worker_threads(worker_threads)
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

            // Bind published container ports to this node's private/overlay
            // address (the WireGuard tunnel IP, or the direct-mode address)
            // rather than 0.0.0.0, so deployed app containers are reachable
            // only from the control-plane proxy over the private network —
            // never on the worker's public interface. `nodes.private_address`
            // is what the proxy already dials for cross-node routing (see
            // `resolve_node_private_address` in temps-routes), so this is
            // just narrowing the bind to match, not changing the routing path.
            // `resolve_config` guarantees this is set (and is a valid IP) —
            // legacy `agent.json` files predating this field fail fast at
            // startup instead of silently reproducing the 0.0.0.0 exposure.
            let host_bind_address = config.private_address.clone().ok_or_else(|| {
                anyhow::anyhow!("private_address missing from resolved agent config")
            })?;

            // Registration deliberately allows a direct-mode node's private
            // address to be a public IP (WireGuard-less direct networking) —
            // see `validate_node_private_address` in temps-deployments. That
            // means this bind can still land on a publicly reachable
            // interface; warn so the operator knows to firewall it rather
            // than discovering it via a port scan.
            if let Ok(std::net::IpAddr::V4(v4)) = host_bind_address.parse::<std::net::IpAddr>() {
                if !v4.is_private() {
                    tracing::warn!(
                        address = %host_bind_address,
                        "this node's private_address is not an RFC 1918 private IP; \
                         deployed container ports will be reachable on this address from \
                         any network that can route to it. If this node has no WireGuard \
                         underlay, restrict access with a host firewall."
                    );
                }
            }

            let mut runtime_builder = temps_deployer::docker::DockerRuntime::new(
                Arc::new(docker.clone()),
                true,
                network_name,
            )
            .with_host_bind_address(host_bind_address)
            .with_overlay_dns_slot(overlay_bridge_address.clone());
            if !overlay_network.is_empty() {
                runtime_builder = runtime_builder
                    .with_overlay_network(overlay_network)
                    .with_overlay_peers(overlay_peers.clone());
            }
            let docker_runtime = Arc::new(runtime_builder);

            let deployer: Arc<dyn temps_deployer::ContainerDeployer> = docker_runtime.clone();
            let builder: Arc<dyn temps_deployer::ImageBuilder> = docker_runtime;

            tracing::info!(
                node_id = config.node_id,
                worker_threads,
                available_parallelism,
                "Starting temps agent"
            );

            // Nightly Docker image + build-cache prune. Worker nodes build
            // and pull images locally but never run the console's plugin
            // system (where `DockerCleanupService` normally lives), so
            // without this they accumulate build cache/images forever.
            // See `DockerOnlyCleanupScheduler` for why this is split out
            // from the console's DB-backed cleanup service.
            tokio::spawn({
                let cleanup_scheduler =
                    temps_deployments::services::DockerOnlyCleanupScheduler::new(Arc::new(
                        temps_deployments::services::DefaultDockerClient,
                    ));
                async move {
                    cleanup_scheduler.start_cleanup_scheduler().await;
                }
            });

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
                let proxy_docker = docker.clone();
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
                    if let Err(e) = temps_agent::internal_proxy::spawn(
                        bridge_ip,
                        80,
                        store,
                        proxy_docker,
                        proxy_shutdown,
                    )
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

        // mTLS cert paths written by `temps join` (ADR-020 WS-2.1). Carried
        // through from the saved config; absent for legacy/HTTP-only nodes.
        let tls_cert_path = saved.as_ref().and_then(|c| c.tls_cert_path.clone());
        let tls_key_path = saved.as_ref().and_then(|c| c.tls_key_path.clone());
        let cluster_ca_path = saved.as_ref().and_then(|c| c.cluster_ca_path.clone());
        let require_mtls = saved.as_ref().is_some_and(|c| c.require_mtls);

        let underlay_dev = self
            .underlay_dev
            .clone()
            .or_else(|| saved.as_ref().and_then(|c| c.underlay_dev.clone()));
        let underlay_mtu = self
            .underlay_mtu
            .or_else(|| saved.as_ref().and_then(|c| c.underlay_mtu));

        // Written by `temps join` (both direct and relay mode always set
        // it) or overridden explicitly via --private-address/env for
        // deployments that don't persist agent.json. Required: this is the
        // address published container ports are bound to, so an agent
        // without it would otherwise fall back to something insecure.
        // Legacy `agent.json` files saved before this field existed must be
        // regenerated with `temps join` before the agent will start.
        let private_address = self
            .private_address
            .clone()
            .or_else(|| saved.as_ref().and_then(|c| c.private_address.clone()))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Missing private_address. This agent.json predates the fix that binds \
                     published container ports to this node's private address instead of \
                     0.0.0.0 (all interfaces). Re-run 'temps join' to update it, or pass \
                     --private-address <ip> matching this node's registered \
                     nodes.private_address."
                )
            })?;
        // Reuse the same reserved-range rejection `temps join` registration
        // already enforces server-side (loopback, link-local, unspecified,
        // multicast, broadcast, documentation ranges) — a manually supplied
        // --private-address/TEMPS_AGENT_PRIVATE_ADDRESS override must not be
        // able to bypass it. In particular this rejects "0.0.0.0", which
        // parses as a syntactically valid IP but would silently reproduce
        // the exact all-interface exposure this whole mechanism exists to
        // close.
        temps_deployments::handlers::nodes::validate_node_private_address(&private_address)
            .map_err(|error| {
                anyhow::anyhow!("private_address '{private_address}' is invalid: {error}")
            })?;

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
            tls_cert_path,
            tls_key_path,
            cluster_ca_path,
            require_mtls,
            underlay_dev,
            underlay_mtu,
            private_address: Some(private_address),
        })
    }

    /// Try to load `agent.json` from the configured agent data directory.
    /// Returns None if not found or unparsable.
    fn load_saved_config(&self) -> Option<temps_agent::AgentConfig> {
        let config_path = agent_data_dir().join("agent.json");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_runtime_uses_available_threads_on_small_hosts() {
        assert_eq!(agent_worker_threads(1), 1);
        assert_eq!(agent_worker_threads(4), 4);
    }

    #[test]
    fn agent_runtime_caps_threads_on_large_hosts() {
        assert_eq!(agent_worker_threads(16), MAX_AGENT_WORKER_THREADS);
        assert_eq!(agent_worker_threads(128), MAX_AGENT_WORKER_THREADS);
    }

    #[test]
    fn agent_runtime_never_builds_with_zero_workers() {
        assert_eq!(agent_worker_threads(0), 1);
    }
}
