//! External plugin process manager.
//!
//! Handles discovering, spawning, handshaking, health-checking, and
//! shutting down external plugin binaries.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use temps_core::external_plugin::{HandshakeMessage, PluginManifest};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::proxy::PluginProxy;

/// State of a single external plugin process.
#[derive(Debug)]
pub struct ExternalPluginProcess {
    /// The parsed manifest from the handshake
    pub manifest: PluginManifest,
    /// Path to the plugin binary
    pub binary_path: PathBuf,
    /// Unix socket path for communication
    pub socket_path: PathBuf,
    /// The child process handle
    child: Child,
    /// Whether the plugin has UI assets
    pub has_ui: bool,
}

impl ExternalPluginProcess {
    /// Kill the plugin process.
    pub fn shutdown(&mut self) {
        if let Some(id) = self.child.id() {
            debug!(plugin = %self.manifest.name, pid = id, "Killing plugin process");
            if let Err(e) = self.child.start_kill() {
                warn!(
                    plugin = %self.manifest.name,
                    "Failed to kill plugin process: {}", e
                );
            }
        }
    }
}

impl Drop for ExternalPluginProcess {
    fn drop(&mut self) {
        self.shutdown();
        // Cleanup socket file
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// Configuration for the external plugin manager.
#[derive(Debug, Clone)]
pub struct ExternalPluginConfig {
    /// Directory to scan for plugin binaries
    pub plugins_dir: PathBuf,
    /// Directory for plugin Unix sockets
    pub sockets_dir: PathBuf,
    /// Directory for plugin data files
    pub data_dir: PathBuf,
    /// Directory to extract plugin UI assets into
    pub ui_assets_dir: PathBuf,
    /// Database URL to pass to plugins
    pub database_url: String,
    /// Timeout for plugin handshake (default: 30s)
    pub handshake_timeout: Duration,
    /// Timeout for health check (default: 5s)
    pub health_check_timeout: Duration,
}

impl ExternalPluginConfig {
    /// Create a config with default settings for a given data directory.
    pub fn new(data_dir: PathBuf, database_url: String) -> Self {
        Self {
            plugins_dir: data_dir.join("plugins"),
            sockets_dir: data_dir.join("run").join("plugins"),
            data_dir: data_dir.join("plugin-data"),
            ui_assets_dir: data_dir.join("plugin-ui"),
            database_url,
            handshake_timeout: Duration::from_secs(30),
            health_check_timeout: Duration::from_secs(5),
        }
    }
}

/// Manages the lifecycle of external plugin processes.
///
/// This runs inside the main Temps process and handles:
/// - Scanning the plugins directory for binaries
/// - Spawning each binary as a child process
/// - Reading the handshake manifest from stdout
/// - Verifying health checks
/// - Providing proxy targets for the axum router
/// - Graceful shutdown
pub struct ExternalPluginManager {
    config: ExternalPluginConfig,
    /// Running plugin processes, keyed by plugin name
    plugins: Arc<RwLock<HashMap<String, ExternalPluginProcess>>>,
    /// HMAC auth secret for signing proxied requests
    auth_secret: String,
}

impl ExternalPluginManager {
    pub fn new(config: ExternalPluginConfig) -> Self {
        let auth_secret = uuid::Uuid::new_v4().to_string();

        Self {
            config,
            plugins: Arc::new(RwLock::new(HashMap::new())),
            auth_secret,
        }
    }

    /// Discover and start all plugins in the plugins directory.
    ///
    /// Returns the list of successfully started plugin manifests.
    pub async fn discover_and_start(&self) -> Vec<PluginManifest> {
        for dir in [
            &self.config.plugins_dir,
            &self.config.sockets_dir,
            &self.config.data_dir,
            &self.config.ui_assets_dir,
        ] {
            if let Err(e) = tokio::fs::create_dir_all(dir).await {
                error!("Failed to create directory {}: {}", dir.display(), e);
                return Vec::new();
            }
        }

        let binaries = match self.scan_plugins_dir().await {
            Ok(bins) => bins,
            Err(e) => {
                error!("Failed to scan plugins directory: {}", e);
                return Vec::new();
            }
        };

        if binaries.is_empty() {
            debug!(
                "No external plugins found in {}",
                self.config.plugins_dir.display()
            );
            return Vec::new();
        }

        info!(
            "Found {} external plugin binary(ies) in {}",
            binaries.len(),
            self.config.plugins_dir.display()
        );

        let mut manifests = Vec::new();

        for binary_path in binaries {
            match self.start_plugin(&binary_path).await {
                Ok(manifest) => {
                    info!(
                        plugin = %manifest.name,
                        version = %manifest.version,
                        "External plugin started successfully"
                    );
                    manifests.push(manifest);
                }
                Err(e) => {
                    error!(
                        binary = %binary_path.display(),
                        "Failed to start external plugin: {}", e
                    );
                }
            }
        }

        manifests
    }

    /// Scan the plugins directory for executable binaries.
    async fn scan_plugins_dir(&self) -> Result<Vec<PathBuf>, std::io::Error> {
        let mut binaries = Vec::new();
        let mut entries = tokio::fs::read_dir(&self.config.plugins_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();

            if path.is_dir() {
                continue;
            }
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'))
            {
                continue;
            }

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(metadata) = path.metadata() {
                    if metadata.permissions().mode() & 0o111 != 0 {
                        binaries.push(path);
                    }
                }
            }

            #[cfg(not(unix))]
            {
                binaries.push(path);
            }
        }

        binaries.sort();
        Ok(binaries)
    }

    /// Start a single plugin binary and complete the handshake.
    async fn start_plugin(&self, binary_path: &Path) -> Result<PluginManifest, String> {
        let binary_name = binary_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");

        let socket_path = self
            .config
            .sockets_dir
            .join(format!("{}.sock", binary_name));
        let plugin_data_dir = self.config.data_dir.join(binary_name);

        // Remove stale socket
        let _ = tokio::fs::remove_file(&socket_path).await;

        debug!(binary = %binary_name, "Spawning external plugin");

        let mut child = Command::new(binary_path)
            .arg("--socket-path")
            .arg(socket_path.to_str().unwrap_or_default())
            .arg("--database-url")
            .arg(&self.config.database_url)
            .arg("--auth-secret")
            .arg(&self.auth_secret)
            .arg("--data-dir")
            .arg(plugin_data_dir.to_str().unwrap_or_default())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn {}: {}", binary_name, e))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("No stdout from {}", binary_name))?;

        let mut reader = BufReader::new(stdout).lines();

        // Read manifest (handshake phase 1)
        let manifest = tokio::time::timeout(self.config.handshake_timeout, async {
            let line = reader
                .next_line()
                .await
                .map_err(|e| format!("Failed to read manifest from {}: {}", binary_name, e))?
                .ok_or_else(|| {
                    format!(
                        "Plugin {} closed stdout before sending manifest",
                        binary_name
                    )
                })?;

            let msg: HandshakeMessage = serde_json::from_str(&line)
                .map_err(|e| format!("Invalid manifest JSON from {}: {}", binary_name, e))?;

            match msg {
                HandshakeMessage::Manifest(m) => Ok(*m),
                _ => Err(format!(
                    "Expected manifest message from {}, got something else",
                    binary_name
                )),
            }
        })
        .await
        .map_err(|_| format!("Handshake timeout for {}", binary_name))??;

        debug!(plugin = %manifest.name, "Received manifest from plugin");

        // Read ready signal (handshake phase 2)
        let has_ui = tokio::time::timeout(self.config.handshake_timeout, async {
            let line = reader
                .next_line()
                .await
                .map_err(|e| format!("Failed to read ready signal from {}: {}", binary_name, e))?
                .ok_or_else(|| {
                    format!(
                        "Plugin {} closed stdout before sending ready signal",
                        binary_name
                    )
                })?;

            let msg: HandshakeMessage = serde_json::from_str(&line)
                .map_err(|e| format!("Invalid ready JSON from {}: {}", binary_name, e))?;

            match msg {
                HandshakeMessage::Ready(r) => {
                    if r.ready {
                        Ok(r.has_ui)
                    } else {
                        Err(format!("Plugin {} reported not ready", binary_name))
                    }
                }
                _ => Err(format!(
                    "Expected ready message from {}, got something else",
                    binary_name
                )),
            }
        })
        .await
        .map_err(|_| format!("Ready signal timeout for {}", binary_name))??;

        debug!(plugin = %manifest.name, has_ui = has_ui, "Plugin is ready");

        // Spawn a task to forward plugin stderr to our logs
        if let Some(stderr) = child.stderr.take() {
            let plugin_name = manifest.name.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    debug!(plugin = %plugin_name, "[plugin stderr] {}", line);
                }
            });
        }

        if has_ui {
            let ui_dir = self.config.ui_assets_dir.join(&manifest.name);
            debug!(
                plugin = %manifest.name,
                dir = %ui_dir.display(),
                "Plugin has UI assets (will be served via /x/<plugin>/ui/* route)"
            );
        }

        let result_manifest = manifest.clone();

        let process = ExternalPluginProcess {
            manifest,
            binary_path: binary_path.to_path_buf(),
            socket_path,
            child,
            has_ui,
        };

        self.plugins
            .write()
            .await
            .insert(result_manifest.name.clone(), process);

        Ok(result_manifest)
    }

    /// Get all running plugin manifests.
    pub async fn manifests(&self) -> Vec<PluginManifest> {
        self.plugins
            .read()
            .await
            .values()
            .map(|p| p.manifest.clone())
            .collect()
    }

    /// Create a PluginProxy for a given plugin name.
    pub async fn proxy_for(&self, plugin_name: &str) -> Option<PluginProxy> {
        let plugins = self.plugins.read().await;
        plugins.get(plugin_name).map(|process| {
            PluginProxy::new(
                process.socket_path.clone(),
                process.manifest.name.clone(),
                self.auth_secret.clone(),
            )
        })
    }

    /// Get the socket path for a plugin.
    pub async fn socket_path_for(&self, plugin_name: &str) -> Option<PathBuf> {
        self.plugins
            .read()
            .await
            .get(plugin_name)
            .map(|p| p.socket_path.clone())
    }

    /// Check if a plugin is running.
    pub async fn is_running(&self, plugin_name: &str) -> bool {
        self.plugins.read().await.contains_key(plugin_name)
    }

    /// Shut down all plugins gracefully.
    pub async fn shutdown_all(&self) {
        let mut plugins = self.plugins.write().await;
        for (name, mut process) in plugins.drain() {
            info!(plugin = %name, "Shutting down external plugin");
            process.shutdown();
        }
    }

    /// Shut down a specific plugin.
    pub async fn shutdown_plugin(&self, plugin_name: &str) {
        let mut plugins = self.plugins.write().await;
        if let Some(mut process) = plugins.remove(plugin_name) {
            info!(plugin = %plugin_name, "Shutting down external plugin");
            process.shutdown();
        } else {
            warn!(plugin = %plugin_name, "Plugin not found for shutdown");
        }
    }

    /// Get the auth secret (for creating proxies externally).
    pub fn auth_secret(&self) -> &str {
        &self.auth_secret
    }

    /// Get the config.
    pub fn config(&self) -> &ExternalPluginConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_manager_creation() {
        let config = ExternalPluginConfig::new(
            PathBuf::from("/tmp/temps-test"),
            "postgres://localhost/test".to_string(),
        );
        let manager = ExternalPluginManager::new(config);

        assert!(manager.manifests().await.is_empty());
        assert!(!manager.auth_secret().is_empty());
    }

    #[tokio::test]
    async fn test_empty_plugins_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let config = ExternalPluginConfig::new(
            tmp.path().to_path_buf(),
            "postgres://localhost/test".to_string(),
        );
        let manager = ExternalPluginManager::new(config);

        let manifests = manager.discover_and_start().await;
        assert!(manifests.is_empty());
    }
}
