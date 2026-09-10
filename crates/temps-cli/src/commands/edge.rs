// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `temps edge` subcommand — starts a lightweight edge CDN proxy node.
//!
//! Connects to a Temps control plane, caches static assets locally, and
//! proxies dynamic requests to the origin. No database required.

use clap::Args;
use std::path::PathBuf;

/// Start an edge CDN proxy node
#[derive(Args)]
pub struct EdgeCommand {
    /// Origin Temps instance URL (e.g., "https://mytemps.com")
    #[arg(long, env = "TEMPS_ORIGIN_URL")]
    pub url: String,

    /// Proxy origin URL for forwarding traffic (defaults to --url).
    /// Only needed when the API and proxy are on different ports (e.g., local dev).
    #[arg(long, env = "TEMPS_EDGE_PROXY_URL")]
    pub proxy_url: Option<String>,

    /// Authentication token for origin API
    #[arg(long, env = "TEMPS_EDGE_TOKEN")]
    pub token: String,

    /// HTTP listen address
    #[arg(long, default_value = "0.0.0.0:80", env = "TEMPS_EDGE_ADDRESS")]
    pub address: String,

    /// TLS listen address (optional)
    #[arg(long, env = "TEMPS_EDGE_TLS_ADDRESS")]
    pub tls_address: Option<String>,

    /// Local cache directory
    #[arg(long, env = "TEMPS_EDGE_CACHE_DIR")]
    pub cache_dir: Option<PathBuf>,

    /// Maximum cache size in MB
    #[arg(long, default_value = "1024", env = "TEMPS_EDGE_MAX_CACHE_MB")]
    pub max_cache_mb: u64,

    /// Route sync interval in seconds
    #[arg(long, default_value = "15", env = "TEMPS_EDGE_SYNC_INTERVAL")]
    pub sync_interval: u64,

    /// Analytics API listen address (queried by origin for edge analytics)
    #[arg(long, default_value = "0.0.0.0:3200", env = "TEMPS_EDGE_API_ADDRESS")]
    pub api_address: String,

    /// Node name (defaults to hostname)
    #[arg(long)]
    pub name: Option<String>,

    /// Region label (e.g., "us-east", "ap-southeast")
    #[arg(long, env = "TEMPS_EDGE_REGION")]
    pub region: Option<String>,
}

impl EdgeCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        let mut config = self.build_config();

        // Registration is async — use a temporary runtime that we drop
        // before start_edge(), which creates its own runtime (Pingora).
        if config.node_id.is_none() {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            println!("Registering edge node with origin...");
            rt.block_on(temps_edge::server::register_with_origin(&mut config))?;
            config.save()?;
            println!(
                "Registered as edge node #{} — config saved to {:?}",
                config.node_id.unwrap(),
                temps_edge::EdgeConfig::config_path()
            );
            drop(rt);
        }

        println!(
            "Starting edge CDN proxy — origin: {}, listen: {}, region: {}",
            config.origin_url,
            config.listen_address,
            config.region.as_deref().unwrap_or("default"),
        );

        // start_edge creates its own tokio runtime + Pingora event loop — must
        // NOT be called from inside another runtime.
        temps_edge::server::start_edge(config)?;

        Ok(())
    }

    fn build_config(&self) -> temps_edge::EdgeConfig {
        // If a saved config exists and matches the origin URL, use it (preserves node_id)
        if let Ok(saved) = temps_edge::EdgeConfig::load() {
            if saved.origin_url == self.url {
                // Merge CLI overrides with saved config
                return temps_edge::EdgeConfig {
                    origin_url: self.url.clone(),
                    proxy_origin_url: self.proxy_url.clone().or(saved.proxy_origin_url),
                    token: self.token.clone(),
                    listen_address: self.address.clone(),
                    tls_listen_address: self.tls_address.clone(),
                    api_address: self.api_address.clone(),
                    cache_dir: self.resolve_cache_dir(),
                    max_cache_size_mb: self.max_cache_mb,
                    route_sync_interval_secs: self.sync_interval,
                    node_id: saved.node_id, // preserve from saved
                    node_name: self.resolve_node_name(),
                    region: self.region.clone().or(saved.region),
                    edge_private_key: saved.edge_private_key, // preserve from saved
                };
            }
        }

        temps_edge::EdgeConfig {
            origin_url: self.url.clone(),
            proxy_origin_url: self.proxy_url.clone(),
            token: self.token.clone(),
            listen_address: self.address.clone(),
            tls_listen_address: self.tls_address.clone(),
            api_address: self.api_address.clone(),
            cache_dir: self.resolve_cache_dir(),
            max_cache_size_mb: self.max_cache_mb,
            route_sync_interval_secs: self.sync_interval,
            node_id: None,
            node_name: self.resolve_node_name(),
            region: self.region.clone(),
            edge_private_key: None,
        }
    }

    fn resolve_cache_dir(&self) -> PathBuf {
        self.cache_dir.clone().unwrap_or_else(|| {
            let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
            home.join(".temps").join("edge-cache")
        })
    }

    fn resolve_node_name(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| gethostname().unwrap_or_else(|| "edge".to_string()))
    }
}

fn gethostname() -> Option<String> {
    let name = gethostname::gethostname();
    name.to_str().map(|s| s.to_string())
}
