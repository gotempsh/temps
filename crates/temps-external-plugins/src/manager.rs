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

use sea_orm::DatabaseConnection;
use utoipa::openapi::OpenApi;

use crate::channel::PluginChannel;
use crate::proxy::PluginProxy;

/// State of a single external plugin process.
pub struct ExternalPluginProcess {
    /// The parsed manifest from the handshake
    pub manifest: PluginManifest,
    /// Path to the plugin binary
    pub binary_path: PathBuf,
    /// Unix socket path for communication
    pub socket_path: PathBuf,
    /// Path to the PID file for this process
    pid_file_path: PathBuf,
    /// The child process handle
    child: Child,
    /// Whether the plugin has UI assets
    pub has_ui: bool,
    /// Bidirectional channel for platform queries and event delivery
    pub channel: Option<PluginChannel>,
    /// OpenAPI schema for the plugin's API endpoints (if provided during handshake)
    pub openapi_schema: Option<OpenApi>,
}

impl ExternalPluginProcess {
    /// Send SIGKILL to the plugin process (non-blocking).
    ///
    /// Does NOT wait for exit — call `wait_for_exit()` for that.
    /// This is used by the `Drop` impl where we can't await.
    pub fn kill(&mut self) {
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

    /// Kill the process and wait (async) for it to exit.
    ///
    /// Used during reload/shutdown from async context.
    pub async fn shutdown(&mut self) {
        self.kill();
        // Wait up to 5 seconds for the process to actually exit.
        // This prevents zombie processes and ensures the socket is released.
        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
    }

    /// Clean up socket and PID files on disk.
    fn cleanup_files(&self) {
        let _ = std::fs::remove_file(&self.socket_path);
        let _ = std::fs::remove_file(&self.pid_file_path);
    }
}

impl Drop for ExternalPluginProcess {
    fn drop(&mut self) {
        self.kill();
        self.cleanup_files();
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
    /// Directory for PID files (one per running plugin process)
    pub pids_dir: PathBuf,
    /// Database URL to pass to plugins
    pub database_url: String,
    /// Timeout for plugin handshake (default: 30s)
    pub handshake_timeout: Duration,
    /// Timeout for health check (default: 5s)
    pub health_check_timeout: Duration,
}

/// Maximum length of a Unix socket path on this platform.
/// macOS: 104 bytes, Linux: 108 bytes.
#[cfg(target_os = "macos")]
const SUN_PATH_MAX: usize = 104;
#[cfg(not(target_os = "macos"))]
const SUN_PATH_MAX: usize = 108;

impl ExternalPluginConfig {
    /// Create a config with default settings for a given data directory.
    ///
    /// Socket paths are placed under `/tmp/tp-<hash>/` to avoid exceeding
    /// the Unix `SUN_LEN` limit (104 bytes on macOS, 108 on Linux). We use
    /// `/tmp` directly instead of `std::env::temp_dir()` because macOS
    /// returns a long per-user path like `/var/folders/.../T/` that eats
    /// most of the budget. The directory name includes a hash of `data_dir`
    /// so multiple Temps instances use separate namespaces.
    pub fn new(data_dir: PathBuf, database_url: String) -> Self {
        // Build a short, deterministic socket directory.
        // Format: /tmp/tp-<8-char-hash>/  (15 bytes for the dir component)
        // Leaves ~85 bytes for the socket filename on macOS.
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        data_dir.hash(&mut hasher);
        let hash = format!("{:x}", hasher.finish());
        let short_hash = &hash[..8.min(hash.len())];
        let sockets_dir = PathBuf::from(format!("/tmp/tp-{}", short_hash));

        Self {
            plugins_dir: data_dir.join("plugins"),
            sockets_dir,
            pids_dir: data_dir.join("run").join("plugin-pids"),
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
    /// Database connection for serving channel requests
    db: Arc<DatabaseConnection>,
}

impl ExternalPluginManager {
    pub fn new(config: ExternalPluginConfig, db: Arc<DatabaseConnection>) -> Self {
        let auth_secret = uuid::Uuid::new_v4().to_string();

        Self {
            config,
            plugins: Arc::new(RwLock::new(HashMap::new())),
            auth_secret,
            db,
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
            &self.config.pids_dir,
        ] {
            if let Err(e) = tokio::fs::create_dir_all(dir).await {
                error!("Failed to create directory {}: {}", dir.display(), e);
                return Vec::new();
            }
        }

        // Kill any stale plugin processes left over from a previous run
        // (e.g. if the server was killed without graceful shutdown).
        self.kill_stale_processes().await;

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

    /// Kill stale plugin processes left over from a previous run.
    ///
    /// Reads PID files from the pids directory, checks whether each process
    /// is still alive, and kills it if so. All PID files are removed
    /// regardless of whether the process was still running.
    async fn kill_stale_processes(&self) {
        let mut entries = match tokio::fs::read_dir(&self.config.pids_dir).await {
            Ok(entries) => entries,
            Err(e) => {
                // Directory might not exist yet on first run — that's fine.
                debug!(
                    "Cannot read PID directory {}: {}",
                    self.config.pids_dir.display(),
                    e
                );
                return;
            }
        };

        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let filename = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) if n.ends_with(".pid") => n.to_string(),
                _ => continue,
            };

            let pid_str = match tokio::fs::read_to_string(&path).await {
                Ok(s) => s.trim().to_string(),
                Err(e) => {
                    warn!("Failed to read PID file {}: {}", path.display(), e);
                    let _ = tokio::fs::remove_file(&path).await;
                    continue;
                }
            };

            let pid: u32 = match pid_str.parse() {
                Ok(p) => p,
                Err(_) => {
                    warn!("Invalid PID in file {}: {:?}", path.display(), pid_str);
                    let _ = tokio::fs::remove_file(&path).await;
                    continue;
                }
            };

            let plugin_name = filename.trim_end_matches(".pid");

            // Check if the process is still alive and kill it
            #[cfg(unix)]
            {
                // SAFETY: libc::kill with signal 0 is a standard POSIX
                // existence check that does not affect the target process.
                let exists = unsafe { libc::kill(pid as i32, 0) } == 0;

                if exists {
                    info!(
                        plugin = %plugin_name,
                        pid = pid,
                        "Killing stale plugin process from previous run"
                    );
                    // SAFETY: SIGKILL is always safe to send to a known PID.
                    let ret = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
                    if ret != 0 {
                        let err = std::io::Error::last_os_error();
                        warn!(
                            plugin = %plugin_name,
                            pid = pid,
                            "Failed to kill stale process: {}", err
                        );
                    }
                } else {
                    debug!(
                        plugin = %plugin_name,
                        pid = pid,
                        "Stale PID file for already-exited process"
                    );
                }
            }

            // Remove stale PID file
            let _ = tokio::fs::remove_file(&path).await;

            // Also remove the corresponding stale socket
            let socket_path = self
                .config
                .sockets_dir
                .join(format!("{}.sock", plugin_name));
            if socket_path.exists() {
                debug!(
                    plugin = %plugin_name,
                    "Removing stale socket file {}",
                    socket_path.display()
                );
                let _ = tokio::fs::remove_file(&socket_path).await;
            }
        }
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

        // Validate that the socket path fits within the OS limit.
        let socket_path_len = socket_path.as_os_str().len();
        if socket_path_len >= SUN_PATH_MAX {
            return Err(format!(
                "Socket path for {} is {} bytes, exceeds OS limit of {} bytes: {}. \
                 Move your data directory to a shorter path.",
                binary_name,
                socket_path_len,
                SUN_PATH_MAX,
                socket_path.display(),
            ));
        }

        // Remove stale socket
        let _ = tokio::fs::remove_file(&socket_path).await;

        debug!(binary = %binary_name, "Spawning external plugin");

        let pid_file_path = self.config.pids_dir.join(format!("{}.pid", binary_name));

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

        // Write PID file so we can clean up stale processes on restart
        if let Some(pid) = child.id() {
            if let Err(e) = tokio::fs::write(&pid_file_path, pid.to_string()).await {
                warn!(
                    binary = %binary_name,
                    "Failed to write PID file {}: {}",
                    pid_file_path.display(),
                    e
                );
            }
        }

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("No stdout from {}", binary_name))?;

        // Capture stderr in a background task so we can surface plugin errors
        // even when the handshake fails (the plugin logs to stderr in JSON).
        let stderr_lines: Arc<tokio::sync::Mutex<Vec<String>>> =
            Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let stderr_task = if let Some(stderr) = child.stderr.take() {
            let lines_ref = stderr_lines.clone();
            let name = binary_name.to_string();
            Some(tokio::spawn(async move {
                let mut reader = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    debug!(plugin = %name, "[plugin stderr] {}", line);
                    let mut buf = lines_ref.lock().await;
                    // Keep last 20 lines to avoid unbounded growth
                    if buf.len() >= 20 {
                        buf.remove(0);
                    }
                    buf.push(line);
                }
            }))
        } else {
            None
        };

        let mut reader = BufReader::new(stdout).lines();

        // Helper: collect recent stderr lines into a single string for error context.
        let collect_stderr = |lines: &Arc<tokio::sync::Mutex<Vec<String>>>| {
            let lines = lines.clone();
            async move {
                let buf = lines.lock().await;
                if buf.is_empty() {
                    String::new()
                } else {
                    format!("\nPlugin stderr:\n  {}", buf.join("\n  "))
                }
            }
        };

        // Read manifest (handshake phase 1)
        let manifest = match tokio::time::timeout(self.config.handshake_timeout, async {
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
        {
            Ok(Ok(m)) => m,
            Ok(Err(e)) => {
                // Give stderr a moment to flush
                tokio::time::sleep(Duration::from_millis(100)).await;
                let stderr_context = collect_stderr(&stderr_lines).await;
                if let Some(task) = stderr_task {
                    task.abort();
                }
                return Err(format!("{}{}", e, stderr_context));
            }
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let stderr_context = collect_stderr(&stderr_lines).await;
                if let Some(task) = stderr_task {
                    task.abort();
                }
                return Err(format!(
                    "Handshake timeout for {}{}",
                    binary_name, stderr_context
                ));
            }
        };

        debug!(plugin = %manifest.name, "Received manifest from plugin");

        // Read ready signal (handshake phase 2)
        let (has_ui, openapi_schema) = match tokio::time::timeout(self.config.handshake_timeout, async {
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
                        // Parse the OpenAPI schema if provided
                        let openapi = match r.openapi {
                            Some(json) => match serde_json::from_value::<OpenApi>(json) {
                                Ok(schema) => {
                                    debug!(plugin = %manifest.name, "Received OpenAPI schema from plugin");
                                    Some(schema)
                                }
                                Err(e) => {
                                    warn!(plugin = %manifest.name, "Failed to parse OpenAPI schema: {}", e);
                                    None
                                }
                            },
                            None => None,
                        };
                        Ok((r.has_ui, openapi))
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
        {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let stderr_context = collect_stderr(&stderr_lines).await;
                if let Some(task) = stderr_task {
                    task.abort();
                }
                return Err(format!("{}{}", e, stderr_context));
            }
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let stderr_context = collect_stderr(&stderr_lines).await;
                if let Some(task) = stderr_task {
                    task.abort();
                }
                return Err(format!(
                    "Ready signal timeout for {}{}",
                    binary_name, stderr_context
                ));
            }
        };

        debug!(plugin = %manifest.name, has_ui = has_ui, "Plugin is ready");

        if has_ui {
            let ui_dir = self.config.ui_assets_dir.join(&manifest.name);
            debug!(
                plugin = %manifest.name,
                dir = %ui_dir.display(),
                "Plugin has UI assets (will be served via /x/<plugin>/ui/* route)"
            );
        }

        let result_manifest = manifest.clone();

        // Open the platform channel (WebSocket to plugin for queries + events).
        // This is non-fatal: older plugins that don't serve /_temps/channel
        // will simply not get a channel (they can still use POST /_events).
        let channel =
            PluginChannel::connect(&socket_path, manifest.name.clone(), self.db.clone()).await;

        let process = ExternalPluginProcess {
            manifest,
            binary_path: binary_path.to_path_buf(),
            socket_path,
            pid_file_path,
            child,
            has_ui,
            channel,
            openapi_schema,
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

    /// Get OpenAPI schemas from all running plugins.
    ///
    /// Returns a map of plugin name -> OpenAPI schema.
    pub async fn openapi_schemas(&self) -> Vec<(String, OpenApi)> {
        self.plugins
            .read()
            .await
            .iter()
            .filter_map(|(name, process)| {
                process
                    .openapi_schema
                    .as_ref()
                    .map(|schema| (name.clone(), schema.clone()))
            })
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

    /// Send an event to a specific plugin over its channel.
    ///
    /// Returns `true` if the event was delivered, `false` if the plugin
    /// has no channel or the channel is dead.
    pub async fn send_event_via_channel(
        &self,
        plugin_name: &str,
        event: &temps_core::external_plugin::PluginEvent,
    ) -> bool {
        let plugins = self.plugins.read().await;
        if let Some(process) = plugins.get(plugin_name) {
            if let Some(ref channel) = process.channel {
                if channel.is_alive() {
                    return channel.send_event(event.clone()).is_ok();
                }
            }
        }
        false
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
            process.shutdown().await;
            process.cleanup_files();
        }
    }

    /// Shut down a specific plugin.
    pub async fn shutdown_plugin(&self, plugin_name: &str) {
        let mut plugins = self.plugins.write().await;
        if let Some(mut process) = plugins.remove(plugin_name) {
            info!(plugin = %plugin_name, "Shutting down external plugin");
            process.shutdown().await;
            process.cleanup_files();
        } else {
            warn!(plugin = %plugin_name, "Plugin not found for shutdown");
        }
    }

    /// Reload all plugins: shut down running ones, re-scan the directory,
    /// and start everything fresh.
    ///
    /// Returns the manifests of all successfully started plugins.
    pub async fn reload_all(&self) -> Vec<PluginManifest> {
        info!("Reloading all external plugins");

        // Phase 1: Shut down all running plugins
        self.shutdown_all().await;

        // Phase 2: Re-discover and start
        self.discover_and_start().await
    }

    /// Reload a single plugin by name: shut it down (if running), then
    /// re-start its binary.
    ///
    /// Returns the new manifest on success, or an error string on failure.
    pub async fn reload_plugin(&self, plugin_name: &str) -> Result<PluginManifest, String> {
        // Find the binary path before shutting down
        let binary_path = {
            let plugins = self.plugins.read().await;
            match plugins.get(plugin_name) {
                Some(process) => process.binary_path.clone(),
                None => {
                    return Err(format!(
                        "Plugin '{}' is not running; cannot reload",
                        plugin_name
                    ))
                }
            }
        };

        info!(plugin = %plugin_name, "Reloading external plugin");

        // Phase 1: Shut down
        self.shutdown_plugin(plugin_name).await;

        // Phase 2: Re-start
        self.start_plugin(&binary_path).await
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

    /// Create a mock database connection for tests.
    fn mock_db() -> Arc<DatabaseConnection> {
        Arc::new(sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection())
    }

    #[tokio::test]
    async fn test_manager_creation() {
        let config = ExternalPluginConfig::new(
            PathBuf::from("/tmp/temps-test"),
            "postgres://localhost/test".to_string(),
        );
        let manager = ExternalPluginManager::new(config, mock_db());

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
        let manager = ExternalPluginManager::new(config, mock_db());

        let manifests = manager.discover_and_start().await;
        assert!(manifests.is_empty());
    }

    #[tokio::test]
    async fn test_reload_all_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let config = ExternalPluginConfig::new(
            tmp.path().to_path_buf(),
            "postgres://localhost/test".to_string(),
        );
        let manager = ExternalPluginManager::new(config, mock_db());

        // Initial discover
        let manifests = manager.discover_and_start().await;
        assert!(manifests.is_empty());

        // Reload — should also be empty with no plugins
        let manifests = manager.reload_all().await;
        assert!(manifests.is_empty());
        assert!(manager.manifests().await.is_empty());
    }

    #[tokio::test]
    async fn test_reload_plugin_not_running() {
        let tmp = tempfile::tempdir().unwrap();
        let config = ExternalPluginConfig::new(
            tmp.path().to_path_buf(),
            "postgres://localhost/test".to_string(),
        );
        let manager = ExternalPluginManager::new(config, mock_db());

        let result = manager.reload_plugin("nonexistent").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not running"));
    }

    #[tokio::test]
    async fn test_kill_stale_processes_removes_pid_files_for_dead_process() {
        let tmp = tempfile::tempdir().unwrap();
        let config = ExternalPluginConfig::new(
            tmp.path().to_path_buf(),
            "postgres://localhost/test".to_string(),
        );
        let manager = ExternalPluginManager::new(config.clone(), mock_db());

        // Create PID directory and a PID file for a non-existent process
        tokio::fs::create_dir_all(&config.pids_dir).await.unwrap();
        let pid_file = config.pids_dir.join("some-plugin.pid");
        // PID 99999999 almost certainly doesn't exist
        tokio::fs::write(&pid_file, "99999999").await.unwrap();

        // Also create a stale socket for the same plugin
        tokio::fs::create_dir_all(&config.sockets_dir)
            .await
            .unwrap();
        let socket_file = config.sockets_dir.join("some-plugin.sock");
        tokio::fs::write(&socket_file, "").await.unwrap();

        manager.kill_stale_processes().await;

        // PID file should be removed
        assert!(
            !pid_file.exists(),
            "PID file should be removed after cleanup"
        );
        // Stale socket should also be removed
        assert!(
            !socket_file.exists(),
            "Stale socket file should be removed after cleanup"
        );
    }

    #[tokio::test]
    async fn test_kill_stale_processes_handles_invalid_pid_file() {
        let tmp = tempfile::tempdir().unwrap();
        let config = ExternalPluginConfig::new(
            tmp.path().to_path_buf(),
            "postgres://localhost/test".to_string(),
        );
        let manager = ExternalPluginManager::new(config.clone(), mock_db());

        // Create PID directory and a PID file with invalid content
        tokio::fs::create_dir_all(&config.pids_dir).await.unwrap();
        let pid_file = config.pids_dir.join("bad-plugin.pid");
        tokio::fs::write(&pid_file, "not-a-number").await.unwrap();

        // Should not panic
        manager.kill_stale_processes().await;

        // PID file should be removed even with invalid content
        assert!(
            !pid_file.exists(),
            "Invalid PID file should be removed after cleanup"
        );
    }

    #[tokio::test]
    async fn test_kill_stale_processes_empty_pids_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let config = ExternalPluginConfig::new(
            tmp.path().to_path_buf(),
            "postgres://localhost/test".to_string(),
        );
        let manager = ExternalPluginManager::new(config.clone(), mock_db());

        // Create empty PID directory
        tokio::fs::create_dir_all(&config.pids_dir).await.unwrap();

        // Should not panic
        manager.kill_stale_processes().await;
    }

    #[tokio::test]
    async fn test_kill_stale_processes_no_pids_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let config = ExternalPluginConfig::new(
            tmp.path().to_path_buf(),
            "postgres://localhost/test".to_string(),
        );
        let manager = ExternalPluginManager::new(config, mock_db());

        // PID directory doesn't exist yet — should handle gracefully
        manager.kill_stale_processes().await;
    }

    #[tokio::test]
    async fn test_kill_stale_processes_skips_non_pid_files() {
        let tmp = tempfile::tempdir().unwrap();
        let config = ExternalPluginConfig::new(
            tmp.path().to_path_buf(),
            "postgres://localhost/test".to_string(),
        );
        let manager = ExternalPluginManager::new(config.clone(), mock_db());

        // Create PID directory with a non-.pid file
        tokio::fs::create_dir_all(&config.pids_dir).await.unwrap();
        let other_file = config.pids_dir.join("readme.txt");
        tokio::fs::write(&other_file, "not a pid file")
            .await
            .unwrap();

        manager.kill_stale_processes().await;

        // Non-.pid file should NOT be removed
        assert!(other_file.exists(), "Non-.pid files should not be removed");
    }

    #[tokio::test]
    async fn test_config_has_pids_dir() {
        let config = ExternalPluginConfig::new(
            PathBuf::from("/data"),
            "postgres://localhost/test".to_string(),
        );
        assert_eq!(config.pids_dir, PathBuf::from("/data/run/plugin-pids"));
        assert_eq!(config.plugins_dir, PathBuf::from("/data/plugins"));
        assert_eq!(config.data_dir, PathBuf::from("/data/plugin-data"));
        // Sockets dir is under /tmp, not under data_dir,
        // to keep Unix socket paths short (macOS has a 104-byte limit).
        let sockets_dir_str = config.sockets_dir.to_string_lossy();
        assert!(
            sockets_dir_str.starts_with("/tmp/tp-"),
            "Sockets dir should be under /tmp/tp-*: {}",
            sockets_dir_str
        );
    }

    #[test]
    fn test_socket_path_fits_sun_len() {
        // Verify that even long plugin names produce socket paths under the limit.
        let config = ExternalPluginConfig::new(
            PathBuf::from("/some/very/deeply/nested/data/directory"),
            "postgres://localhost/test".to_string(),
        );
        let socket = config
            .sockets_dir
            .join("temps-very-long-plugin-name-that-would-break-things.sock");
        assert!(
            socket.as_os_str().len() < SUN_PATH_MAX,
            "Socket path {} ({} bytes) exceeds SUN_PATH_MAX ({})",
            socket.display(),
            socket.as_os_str().len(),
            SUN_PATH_MAX,
        );
    }

    #[test]
    fn test_different_data_dirs_get_different_socket_dirs() {
        let config_a = ExternalPluginConfig::new(
            PathBuf::from("/data/a"),
            "postgres://localhost/test".to_string(),
        );
        let config_b = ExternalPluginConfig::new(
            PathBuf::from("/data/b"),
            "postgres://localhost/test".to_string(),
        );
        assert_ne!(
            config_a.sockets_dir, config_b.sockets_dir,
            "Different data dirs must produce different socket dirs"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_kill_stale_processes_kills_real_process() {
        use tokio::process::Command;

        let tmp = tempfile::tempdir().unwrap();
        let config = ExternalPluginConfig::new(
            tmp.path().to_path_buf(),
            "postgres://localhost/test".to_string(),
        );
        let manager = ExternalPluginManager::new(config.clone(), mock_db());

        // Spawn a real process (sleep) that we can kill.
        // We keep the Child handle so we can waitpid on it later —
        // without waitpid the kernel won't reap the zombie and
        // kill(pid, 0) will keep returning success.
        let mut child = Command::new("sleep")
            .arg("300")
            .spawn()
            .expect("Failed to spawn sleep process");
        let pid = child.id().expect("No PID for spawned process");

        // Create PID directory and write the real PID
        tokio::fs::create_dir_all(&config.pids_dir).await.unwrap();
        tokio::fs::create_dir_all(&config.sockets_dir)
            .await
            .unwrap();
        let pid_file = config.pids_dir.join("sleeper.pid");
        tokio::fs::write(&pid_file, pid.to_string()).await.unwrap();

        // Verify the process is alive
        let alive = unsafe { libc::kill(pid as i32, 0) } == 0;
        assert!(alive, "Spawned sleep process should be alive");

        // Kill stale processes
        manager.kill_stale_processes().await;

        // Reap the zombie child so the kernel removes the process entry.
        // Without this, kill(pid, 0) returns success for zombies.
        let exit = child.wait().await;
        assert!(exit.is_ok(), "Should be able to wait on killed child");

        // Verify the process is dead
        let still_alive = unsafe { libc::kill(pid as i32, 0) } == 0;
        assert!(!still_alive, "Stale process should have been killed");

        // PID file should be cleaned up
        assert!(!pid_file.exists(), "PID file should be removed");
    }
}
