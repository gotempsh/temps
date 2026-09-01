// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Temps Edge — lightweight, database-free CDN proxy node.
//!
//! Connects to a Temps control plane via HTTPS + bearer token, caches static
//! assets locally using `FsFileStore`, and proxies dynamic requests to the
//! origin. No database required on the edge node.
//!
//! # Usage
//!
//! ```bash
//! temps edge --url https://mytemps.com --token <edge-token>
//! ```

pub mod analytics;
pub mod api;
pub mod cache;
pub mod entities;
pub mod migrations;
pub mod proxy;
pub mod route_table;
pub mod server;
pub mod tls;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum EdgeError {
    #[error("Origin connection failed for {origin_url}: {reason}")]
    OriginConnection { origin_url: String, reason: String },

    #[error("Route sync failed: {0}")]
    RouteSync(String),

    #[error("Cache error for path '{path}': {reason}")]
    Cache { path: String, reason: String },

    #[error("Authentication failed: {0}")]
    AuthenticationFailed(String),

    #[error("Registration failed: {0}")]
    RegistrationFailed(String),

    #[error("Server error: {0}")]
    ServerError(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Configuration for the edge CDN node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeConfig {
    /// Origin Temps API URL for registration and route sync (e.g., "https://mytemps.com")
    pub origin_url: String,
    /// Origin proxy URL for forwarding traffic (defaults to origin_url).
    /// Only needed when the API and proxy listen on different ports (e.g., local dev).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_origin_url: Option<String>,
    /// Bearer token for authenticating with the origin
    pub token: String,
    /// HTTP listen address (e.g., "0.0.0.0:80")
    pub listen_address: String,
    /// Optional TLS listen address
    pub tls_listen_address: Option<String>,
    /// API listen address for analytics queries (e.g., "0.0.0.0:3200")
    pub api_address: String,
    /// Local cache directory
    pub cache_dir: PathBuf,
    /// Maximum cache size in megabytes
    pub max_cache_size_mb: u64,
    /// How often to sync routes from origin (seconds)
    pub route_sync_interval_secs: u64,
    /// Node ID assigned by the control plane after registration
    pub node_id: Option<i32>,
    /// Human-readable node name
    pub node_name: String,
    /// Optional region label (e.g., "us-east", "eu-west")
    pub region: Option<String>,
    /// X25519 private key for ECIES certificate decryption (base64-encoded).
    /// Generated once during registration and saved to config. Never transmitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge_private_key: Option<String>,
}

impl EdgeConfig {
    /// Path to the saved edge config file.
    pub fn config_path() -> PathBuf {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        home.join(".temps").join("edge.json")
    }

    /// Load config from `~/.temps/edge.json`.
    pub fn load() -> Result<Self, EdgeError> {
        let path = Self::config_path();
        let data = std::fs::read_to_string(&path).map_err(EdgeError::Io)?;
        serde_json::from_str(&data)
            .map_err(|e| EdgeError::ServerError(format!("Failed to parse edge config: {}", e)))
    }

    /// Save config to `~/.temps/edge.json` with restrictive permissions (0600).
    pub fn save(&self) -> Result<(), EdgeError> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).ok();
            }
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| {
            EdgeError::ServerError(format!("Failed to serialize edge config: {}", e))
        })?;
        std::fs::write(&path, &json)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).ok();
        }
        Ok(())
    }
}

/// Cache statistics reported in heartbeats.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheStats {
    pub hit_count: u64,
    pub miss_count: u64,
    pub hit_rate: f64,
    pub disk_usage_bytes: u64,
    pub entry_count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edge_config_serialization() {
        let config = EdgeConfig {
            origin_url: "https://mytemps.com".to_string(),
            proxy_origin_url: None,
            token: "test-token".to_string(),
            listen_address: "0.0.0.0:80".to_string(),
            tls_listen_address: None,
            api_address: "0.0.0.0:3200".to_string(),
            cache_dir: PathBuf::from("/tmp/edge-cache"),
            max_cache_size_mb: 1024,
            route_sync_interval_secs: 15,
            node_id: Some(42),
            node_name: "edge-sgp".to_string(),
            region: Some("ap-southeast".to_string()),
            edge_private_key: None,
        };

        let json = serde_json::to_string(&config).unwrap();
        let parsed: EdgeConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.origin_url, "https://mytemps.com");
        assert_eq!(parsed.node_id, Some(42));
        assert_eq!(parsed.region.as_deref(), Some("ap-southeast"));
    }

    #[test]
    fn test_cache_stats_serialization() {
        let stats = CacheStats {
            hit_count: 1000,
            miss_count: 50,
            hit_rate: 0.952,
            disk_usage_bytes: 524_288_000,
            entry_count: 2500,
        };

        let json = serde_json::to_string(&stats).unwrap();
        let parsed: CacheStats = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.hit_count, 1000);
        assert_eq!(parsed.entry_count, 2500);
    }
}
