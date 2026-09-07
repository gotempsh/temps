// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! External plugin process manager.
//!
//! Handles discovering, spawning, handshaking, health-checking, and
//! shutting down external plugin binaries.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use temps_core::external_plugin::{
    HandshakeMessage, PluginLaunchConfig, PluginManifest, EXTERNAL_PLUGIN_PROTOCOL_VERSION,
};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use sea_orm::DatabaseConnection;
use utoipa::openapi::OpenApi;

use crate::channel::PluginChannel;
use crate::install::open_verified_executable;
use crate::proxy::PluginProxy;

/// State of a single external plugin process.
pub struct ExternalPluginProcess {
    /// The parsed manifest from the handshake
    pub manifest: PluginManifest,
    /// Path to the plugin binary
    pub binary_path: PathBuf,
    /// Signed digest verified against the descriptor used for execution.
    pub sha256: String,
    /// Unix socket path for communication
    pub socket_path: PathBuf,
    /// Per-process secret used to bind internal requests to this staged
    /// plugin launch. This is protocol integrity, not shared-UID isolation.
    auth_secret: String,
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

/// A candidate that completed the complete protocol handshake but has not yet
/// replaced the healthy active process.
pub(crate) struct PendingPlugin {
    expected_name: String,
    process: ExternalPluginProcess,
}

#[derive(Debug, Clone)]
pub struct PluginLoadFailure {
    pub plugin: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Default)]
pub struct PluginReloadResult {
    pub manifests: Vec<PluginManifest>,
    pub failures: Vec<PluginLoadFailure>,
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
    /// The instance's own data root — the parent of all the directories
    /// above. Disclosed in the typed launch configuration only when a trusted
    /// plugin declares that privileged requirement.
    pub host_data_dir: PathBuf,
    /// Base URL at which this instance's API answers, passed to plugins as
    /// `--host-api-url`. `None` when the caller did not supply one, in which
    /// case plugins are told nothing rather than guessing.
    pub host_api_url: Option<String>,
    /// Directory to extract plugin UI assets into
    pub ui_assets_dir: PathBuf,
    /// Directory for PID files (one per running plugin process)
    pub pids_dir: PathBuf,
    /// Database URL disclosed in typed launch configuration to plugins that
    /// declare direct database access.
    pub database_url: String,
    /// Timeout for plugin handshake (default: 30s)
    pub handshake_timeout: Duration,
    /// Timeout for health check (default: 5s)
    pub health_check_timeout: Duration,
    /// Signed registry trust and network policy used for installed receipts.
    pub registry: crate::catalog::RegistryConfig,
}

/// Maximum length of a Unix socket path on this platform.
/// macOS: 104 bytes, Linux: 108 bytes.
#[cfg(target_os = "macos")]
const SUN_PATH_MAX: usize = 104;
#[cfg(not(target_os = "macos"))]
const SUN_PATH_MAX: usize = 108;
const MAX_HANDSHAKE_FRAME_BYTES: usize = 2 * 1024 * 1024;

fn generate_plugin_auth_secret() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn legacy_startup_eof_error(binary_name: &str) -> String {
    format!(
        "Plugin {binary_name} closed stdout before sending the staged protocol v{EXTERNAL_PLUGIN_PROTOCOL_VERSION} hello. The binary is incompatible or uses a legacy temps-plugin-sdk; rebuild it with protocol v{EXTERNAL_PLUGIN_PROTOCOL_VERSION}"
    )
}

fn scrub_plugin_environment(command: &mut Command) {
    command.env_clear();
}

fn plugin_stderr_context(observed_bytes: usize) -> String {
    if observed_bytes == 0 {
        String::new()
    } else {
        format!(
            "\nPlugin emitted diagnostic output ({observed_bytes} bytes withheld to prevent secret disclosure)"
        )
    }
}

async fn read_handshake_frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    binary_name: &str,
    phase: &str,
) -> Result<Option<String>, String> {
    let mut bytes = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|error| format!("Failed to read {phase} from {binary_name}: {error}"))?;
        if available.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            break;
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        if bytes.len().saturating_add(consumed) > MAX_HANDSHAKE_FRAME_BYTES {
            return Err(format!(
                "Plugin {binary_name} {phase} exceeded the {MAX_HANDSHAKE_FRAME_BYTES}-byte limit"
            ));
        }
        bytes.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if newline.is_some() {
            break;
        }
    }
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes.pop();
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|error| format!("Plugin {binary_name} sent non-UTF-8 {phase}: {error}"))
}

#[cfg(unix)]
fn secure_socket_directory(path: &Path) -> Result<(), std::io::Error> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::DirBuilderExt;

    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }

    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "plugin socket directory contains a NUL byte",
        )
    })?;
    // SAFETY: `path` is a valid C string. O_NOFOLLOW rejects a malicious
    // symlink at the deterministic /tmp path, and OwnedFd closes the result.
    let raw_fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
        )
    };
    if raw_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `raw_fd` was returned by open above and ownership is transferred
    // exactly once to OwnedFd.
    let directory = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    // SAFETY: fchmod operates on the live directory descriptor and does not
    // dereference the path again, closing the symlink-swap race.
    if unsafe { libc::fchmod(directory.as_raw_fd(), 0o700) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

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
            host_data_dir: data_dir,
            host_api_url: None,
            database_url,
            handshake_timeout: Duration::from_secs(30),
            health_check_timeout: Duration::from_secs(5),
            registry: crate::catalog::RegistryConfig::default(),
        }
    }

    /// Inject an authenticated registry configuration. The default has no
    /// trust anchors and therefore refuses registry installs and startup.
    pub fn with_registry(mut self, registry: crate::catalog::RegistryConfig) -> Self {
        self.registry = registry;
        self
    }

    /// Record where the proxy listens, so plugins can be told their own
    /// externally-reachable base URL.
    ///
    /// A bind address is not a URL: `0.0.0.0` means "every interface" to a
    /// listener but nothing to a client, so it is rewritten to loopback.
    /// Callers that terminate TLS or sit behind another proxy should set the
    /// instance's public URL in settings instead of relying on this.
    pub fn with_proxy_address(mut self, address: &str) -> Self {
        let address = address.trim();
        if address.is_empty() {
            return self;
        }
        let host_port = match address.rsplit_once(':') {
            Some((host, port)) if host == "0.0.0.0" || host == "[::]" || host.is_empty() => {
                format!("127.0.0.1:{port}")
            }
            _ => address.to_string(),
        };
        self.host_api_url = Some(format!("http://{host_port}"));
        self
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
    /// Database connection for serving channel requests
    db: Arc<DatabaseConnection>,
    /// Late-bound bridge into the platform's own HTTP router, shared with
    /// every channel. Empty until the console finishes assembling the
    /// router — see [`crate::channel::HostApiSlot`].
    host_api: Arc<crate::channel::HostApiSlot>,
    /// Instance key material for minting per-caller actor tokens. Set by the
    /// console; `None` leaves plugins without one, so their API calls fail
    /// closed rather than being attributed to nobody.
    actor_crypto: crate::proxy::ActorCryptoSlot,
}

impl ExternalPluginManager {
    pub fn new(config: ExternalPluginConfig, db: Arc<DatabaseConnection>) -> Self {
        Self {
            config,
            plugins: Arc::new(RwLock::new(HashMap::new())),
            db,
            host_api: Arc::new(crate::channel::HostApiSlot::new(None)),
            actor_crypto: crate::proxy::ActorCryptoSlot::default(),
        }
    }

    /// Install the bridge plugins use to call the platform's own HTTP API.
    ///
    /// Called by the console once its router is assembled. Until then the
    /// slot is empty and an `ApiCall` is answered with an error naming the
    /// gap, rather than being silently dropped.
    pub async fn set_host_api(&self, bridge: Arc<dyn crate::channel::HostApiBridge>) {
        *self.host_api.write().await = Some(bridge);
    }

    /// Supply the key material used to mint per-caller actor tokens.
    pub async fn set_actor_crypto(&self, crypto: Arc<temps_core::CookieCrypto>) {
        // Every proxy already holds a clone of this slot, so mounted routes
        // start minting immediately — no rebuild, no ordering requirement.
        self.actor_crypto.set(crypto);
    }

    /// Discover and start all plugins in the plugins directory.
    ///
    /// Returns the list of successfully started plugin manifests.
    pub async fn discover_and_start(&self) -> Vec<PluginManifest> {
        self.discover_and_start_report().await.manifests
    }

    async fn discover_and_start_report(&self) -> PluginReloadResult {
        #[cfg(unix)]
        if let Err(error) = secure_socket_directory(&self.config.sockets_dir) {
            error!(
                directory = %self.config.sockets_dir.display(),
                "Failed to create a secure 0700 plugin socket directory: {error}"
            );
            return PluginReloadResult {
                manifests: Vec::new(),
                failures: vec![PluginLoadFailure {
                    plugin: None,
                    reason: "Plugin socket directory could not be prepared".to_string(),
                }],
            };
        }
        #[cfg(not(unix))]
        if let Err(error) = tokio::fs::create_dir_all(&self.config.sockets_dir).await {
            error!(
                directory = %self.config.sockets_dir.display(),
                "Failed to create plugin socket directory: {error}"
            );
            return PluginReloadResult {
                manifests: Vec::new(),
                failures: vec![PluginLoadFailure {
                    plugin: None,
                    reason: "Plugin socket directory could not be prepared".to_string(),
                }],
            };
        }

        for dir in [
            &self.config.plugins_dir,
            &self.config.data_dir,
            &self.config.ui_assets_dir,
            &self.config.pids_dir,
        ] {
            if let Err(e) = tokio::fs::create_dir_all(dir).await {
                error!("Failed to create directory {}: {}", dir.display(), e);
                return PluginReloadResult {
                    manifests: Vec::new(),
                    failures: vec![PluginLoadFailure {
                        plugin: None,
                        reason: "A required plugin runtime directory could not be prepared"
                            .to_string(),
                    }],
                };
            }
        }
        // Kill any stale plugin processes left over from a previous run
        // (e.g. if the server was killed without graceful shutdown).
        self.kill_stale_processes().await;

        let (binaries, mut failures) = self.scan_plugins_dir().await;

        if binaries.is_empty() {
            debug!(
                "No external plugins found in {}",
                self.config.plugins_dir.display()
            );
            return PluginReloadResult {
                manifests: Vec::new(),
                failures,
            };
        }

        info!(
            "Found {} external plugin binary(ies) in {}",
            binaries.len(),
            self.config.plugins_dir.display()
        );

        let mut manifests = Vec::new();

        for installation in binaries {
            match self
                .start_plugin(
                    &installation.name,
                    &installation.version,
                    &installation.sha256,
                    &installation.binary_path,
                )
                .await
            {
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
                        binary = %installation.binary_path.display(),
                        "Failed to start external plugin: {}", e
                    );
                    failures.push(PluginLoadFailure {
                        plugin: Some(installation.name),
                        reason: "Plugin failed startup verification".to_string(),
                    });
                }
            }
        }

        PluginReloadResult {
            manifests,
            failures,
        }
    }

    /// Discover only activated binaries whose local receipt still verifies
    /// against a trusted signed registry document. Flat executable files are
    /// intentionally ignored: dropping a file into this directory must never
    /// turn it into code executed by the Temps server.
    async fn scan_plugins_dir(
        &self,
    ) -> (
        Vec<crate::install::ActiveInstallation>,
        Vec<PluginLoadFailure>,
    ) {
        let mut binaries = Vec::new();
        let mut failures = Vec::new();
        for result in
            crate::install::discover_active(&self.config.plugins_dir, &self.config.registry).await
        {
            match result {
                Ok(installation) => binaries.push(installation),
                Err(error) => {
                    warn!(error = %error, "Ignoring unverified external plugin install");
                    failures.push(PluginLoadFailure {
                        plugin: None,
                        reason: "Activated plugin installation failed verification".to_string(),
                    });
                }
            }
        }
        binaries.sort_by(|left, right| left.name.cmp(&right.name));
        (binaries, failures)
    }

    /// Remove stale plugin bookkeeping left over from a previous run.
    ///
    /// A persisted numeric PID is not process identity: it may have been
    /// reused by the operating system. Therefore startup never signals a
    /// process based on this file alone; it only removes the stale record and
    /// socket pathname.
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

            let plugin_name = filename.trim_end_matches(".pid");

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

    /// Spawn a single plugin binary and complete the handshake without adding
    /// it to the active process table.
    async fn spawn_plugin(
        &self,
        binary_path: &Path,
        expected_sha256: &str,
        expected_name: &str,
        expected_version: &str,
        instance_name: &str,
        data_name: &str,
    ) -> Result<ExternalPluginProcess, String> {
        let binary_name = binary_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");

        let socket_path = self
            .config
            .sockets_dir
            .join(format!("{}.sock", instance_name));
        let plugin_data_dir = self.config.data_dir.join(data_name);
        // This authenticates traffic as belonging to the staged child process.
        // Installed plugins are trusted host code under the current shared-UID
        // architecture; this is protocol integrity, not a sandbox boundary.
        let auth_secret = generate_plugin_auth_secret();

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

        let pid_file_path = self.config.pids_dir.join(format!("{}.pid", instance_name));

        let executable = open_verified_executable(
            binary_name,
            &self.config.plugins_dir,
            binary_path,
            expected_sha256,
        )
        .await
        .map_err(|error| error.to_string())?;
        let command_path = executable
            .command_path(binary_name)
            .map_err(|error| error.to_string())?;
        let mut command = Command::new(command_path);
        scrub_plugin_environment(&mut command);
        command
            // Registry plugins are host code, but they do not need the Temps
            // process's secrets. Required non-secret paths and URLs are
            // passed explicitly below.
            .kill_on_drop(true)
            .arg("--socket-path")
            .arg(socket_path.to_str().unwrap_or_default())
            .arg("--data-dir")
            .arg(plugin_data_dir.to_str().unwrap_or_default())
            .args(
                self.config
                    .host_api_url
                    .iter()
                    .flat_map(|url| ["--host-api-url", url.as_str()]),
            );
        let mut child = command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn {}: {}", binary_name, e))?;
        // Keep the verified executable descriptor alive until spawn has
        // completed and the child has inherited it.
        drop(executable);

        let mut child_stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("Failed to open the launch-config pipe for {binary_name}"))?;

        // Write PID file so we can clean up stale processes on restart
        if let Some(pid) = child.id() {
            let pid_write = async {
                let mut file = tokio::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&pid_file_path)
                    .await?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    file.set_permissions(std::fs::Permissions::from_mode(0o600))
                        .await?;
                }
                file.write_all(pid.to_string().as_bytes()).await?;
                file.sync_all().await
            }
            .await;
            if let Err(e) = pid_write {
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

        // Drain stderr so a noisy child cannot block, but never place
        // plugin-controlled output in logs or HTTP errors. The process may
        // receive database credentials after its identity handshake, so even
        // apparently harmless diagnostics must be treated as secret-bearing.
        let stderr_bytes = Arc::new(AtomicUsize::new(0));
        let stderr_task = if let Some(mut stderr) = child.stderr.take() {
            let observed = stderr_bytes.clone();
            Some(tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                loop {
                    match stderr.read(&mut buffer).await {
                        Ok(0) | Err(_) => break,
                        Ok(read) => {
                            let _ = observed.fetch_update(
                                Ordering::Relaxed,
                                Ordering::Relaxed,
                                |current| Some(current.saturating_add(read)),
                            );
                        }
                    }
                }
            }))
        } else {
            None
        };

        let mut reader = BufReader::new(stdout);

        // Stage 1: the already-running child identifies its protocol and
        // declares which host values it needs. Nothing sensitive or privileged
        // has been passed to it in argv.
        let manifest = match tokio::time::timeout(self.config.handshake_timeout, async {
            let line = read_handshake_frame(&mut reader, binary_name, "startup hello")
                .await?
                .ok_or_else(|| legacy_startup_eof_error(binary_name))?;

            let msg: HandshakeMessage = serde_json::from_str(&line)
                .map_err(|e| format!("Invalid manifest JSON from {}: {}", binary_name, e))?;

            match msg {
                HandshakeMessage::Hello(hello)
                    if hello.protocol_version == EXTERNAL_PLUGIN_PROTOCOL_VERSION =>
                {
                    Ok(*hello.manifest)
                }
                HandshakeMessage::Hello(hello) => Err(format!(
                    "Plugin {binary_name} uses external-plugin protocol {}, but this Temps build requires {}; rebuild the plugin with the current temps-plugin-sdk",
                    hello.protocol_version, EXTERNAL_PLUGIN_PROTOCOL_VERSION
                )),
                HandshakeMessage::Manifest(_) => Err(format!(
                    "Plugin {binary_name} uses the legacy startup handshake; rebuild it with temps-plugin-sdk protocol {EXTERNAL_PLUGIN_PROTOCOL_VERSION}"
                )),
                _ => Err(format!(
                    "Plugin {binary_name} sent an unexpected startup message; rebuild it with temps-plugin-sdk protocol {EXTERNAL_PLUGIN_PROTOCOL_VERSION}"
                )),
            }
        })
        .await
        {
            Ok(Ok(m)) => m,
            Ok(Err(e)) => {
                // Give stderr a moment to flush
                tokio::time::sleep(Duration::from_millis(100)).await;
                let stderr_context = plugin_stderr_context(stderr_bytes.load(Ordering::Relaxed));
                if let Some(task) = stderr_task {
                    task.abort();
                }
                return Err(format!("{}{}", e, stderr_context));
            }
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let stderr_context = plugin_stderr_context(stderr_bytes.load(Ordering::Relaxed));
                if let Some(task) = stderr_task {
                    task.abort();
                }
                return Err(format!(
                    "Plugin {binary_name} did not emit the staged startup hello; rebuild it with temps-plugin-sdk protocol {EXTERNAL_PLUGIN_PROTOCOL_VERSION}{stderr_context}"
                ));
            }
        };

        // Verify signed identity before disclosing the database URL or host
        // data directory through the second handshake frame.
        if manifest.name != expected_name || manifest.version != expected_version {
            let declared_name = manifest.name.clone();
            let declared_version = manifest.version.clone();
            let _ = child.start_kill();
            if let Some(task) = stderr_task {
                task.abort();
            }
            return Err(format!(
                "Plugin binary {} declares {declared_name} v{declared_version}, but signed installation expects {expected_name} v{expected_version}",
                binary_path.display()
            ));
        }

        debug!(plugin = %manifest.name, "Received manifest from plugin");

        // Stage 2: send one typed line to this same child. The manifest flags
        // control disclosure/routing for trusted installed code; they do not
        // turn a shared-UID plugin process into a security sandbox.
        if manifest.requires_host_data_access {
            warn!(
                plugin = %manifest.name,
                "Plugin declares privileged access to the platform host data root"
            );
        }
        let launch_config = PluginLaunchConfig {
            protocol_version: EXTERNAL_PLUGIN_PROTOCOL_VERSION,
            auth_secret: auth_secret.clone(),
            database_url: manifest
                .requires_db
                .then(|| self.config.database_url.clone()),
            host_data_dir: manifest
                .requires_host_data_access
                .then(|| self.config.host_data_dir.to_string_lossy().into_owned()),
        };
        let launch_json = serde_json::to_string(&launch_config).map_err(|error| {
            format!("Failed to encode launch configuration for {binary_name}: {error}")
        })?;
        if let Err(error) = child_stdin
            .write_all(format!("{launch_json}\n").as_bytes())
            .await
        {
            let _ = child.start_kill();
            return Err(format!(
                "Failed to deliver typed launch configuration to {binary_name}: {error}"
            ));
        }
        if let Err(error) = child_stdin.shutdown().await {
            let _ = child.start_kill();
            return Err(format!(
                "Failed to close the launch-config pipe for {binary_name}: {error}"
            ));
        }

        // Read ready signal (handshake phase 2)
        let (has_ui, openapi_schema) = match tokio::time::timeout(self.config.handshake_timeout, async {
            let line = read_handshake_frame(&mut reader, binary_name, "ready signal")
                .await?
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
                    if r.protocol_version != EXTERNAL_PLUGIN_PROTOCOL_VERSION {
                        return Err(format!(
                            "Plugin {binary_name} uses external-plugin protocol {}, but this Temps build requires {}; rebuild the plugin with the current temps-plugin-sdk",
                            r.protocol_version, EXTERNAL_PLUGIN_PROTOCOL_VERSION
                        ));
                    }
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
                let stderr_context = plugin_stderr_context(stderr_bytes.load(Ordering::Relaxed));
                if let Some(task) = stderr_task {
                    task.abort();
                }
                return Err(format!("{}{}", e, stderr_context));
            }
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let stderr_context = plugin_stderr_context(stderr_bytes.load(Ordering::Relaxed));
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

        // Open the platform channel (WebSocket to plugin for queries + events).
        // This is non-fatal: older plugins that don't serve /_temps/channel
        // will simply not get a channel (they can still use POST /_events).
        let channel = PluginChannel::connect(
            &socket_path,
            manifest.name.clone(),
            self.db.clone(),
            self.host_api.clone(),
            manifest.capabilities.clone(),
            &auth_secret,
        )
        .await;
        let channel = match channel {
            Some(channel) => channel,
            None => {
                let _ = child.start_kill();
                return Err(format!(
                    "Plugin {binary_name} did not establish an authenticated platform channel; rebuild it with temps-plugin-sdk protocol {EXTERNAL_PLUGIN_PROTOCOL_VERSION}"
                ));
            }
        };

        Ok(ExternalPluginProcess {
            manifest,
            binary_path: binary_path.to_path_buf(),
            sha256: expected_sha256.to_string(),
            socket_path,
            auth_secret,
            pid_file_path,
            child,
            has_ui,
            channel: Some(channel),
            openapi_schema,
        })
    }

    /// Start a verified active install and register it under its signed name.
    async fn start_plugin(
        &self,
        expected_name: &str,
        expected_version: &str,
        expected_sha256: &str,
        binary_path: &Path,
    ) -> Result<PluginManifest, String> {
        let process = self
            .spawn_plugin(
                binary_path,
                expected_sha256,
                expected_name,
                expected_version,
                expected_name,
                expected_name,
            )
            .await?;
        let manifest = process.manifest.clone();
        if let Some(mut replaced) = self
            .plugins
            .write()
            .await
            .insert(expected_name.to_string(), process)
        {
            replaced.shutdown().await;
        }
        Ok(manifest)
    }

    /// Run a candidate through the full startup protocol while leaving the
    /// current active process untouched.
    pub(crate) async fn prepare_candidate(
        &self,
        expected_name: &str,
        expected_version: &str,
        expected_sha256: &str,
        binary_path: &Path,
    ) -> Result<PendingPlugin, String> {
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let instance_name = format!("candidate-{}", &suffix[..16]);
        let process = self
            .spawn_plugin(
                binary_path,
                expected_sha256,
                expected_name,
                expected_version,
                &instance_name,
                expected_name,
            )
            .await?;
        Ok(PendingPlugin {
            expected_name: expected_name.to_string(),
            process,
        })
    }

    /// Atomically swap the process table entry, then stop the old process.
    pub(crate) async fn promote_candidate(&self, pending: PendingPlugin) -> PluginManifest {
        let manifest = pending.process.manifest.clone();
        let old = self
            .plugins
            .write()
            .await
            .insert(pending.expected_name, pending.process);
        if let Some(mut old) = old {
            old.shutdown().await;
        }
        manifest
    }

    pub(crate) async fn discard_candidate(&self, mut pending: PendingPlugin) {
        pending.process.shutdown().await;
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
                process.auth_secret.clone(),
            )
            .with_public_paths(process.manifest.public_paths.clone())
            .with_actor_minting(
                self.actor_crypto.clone(),
                !process.manifest.capabilities.is_empty(),
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

    /// Per-process assertion secret for internal event delivery.
    pub(crate) async fn auth_secret_for(&self, plugin_name: &str) -> Option<String> {
        self.plugins
            .read()
            .await
            .get(plugin_name)
            .map(|process| process.auth_secret.clone())
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
    pub async fn reload_all(&self) -> PluginReloadResult {
        info!("Reloading all external plugins");

        // Phase 1: Shut down all running plugins
        self.shutdown_all().await;

        // Phase 2: Re-discover and start
        self.discover_and_start_report().await
    }

    /// Reload a single plugin by name: shut it down (if running), then
    /// re-start its binary.
    ///
    /// Returns the new manifest on success, or an error string on failure.
    pub async fn reload_plugin(&self, plugin_name: &str) -> Result<PluginManifest, String> {
        // Find the binary path before shutting down
        let (binary_path, version, sha256) = {
            let plugins = self.plugins.read().await;
            match plugins.get(plugin_name) {
                Some(process) => (
                    process.binary_path.clone(),
                    process.manifest.version.clone(),
                    process.sha256.clone(),
                ),
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
        self.start_plugin(plugin_name, &version, &sha256, &binary_path)
            .await
    }

    /// Get the config.
    pub fn config(&self) -> &ExternalPluginConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest as _, Sha256};

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
    }

    #[test]
    fn plugin_processes_receive_distinct_auth_secrets() {
        let first = generate_plugin_auth_secret();
        let second = generate_plugin_auth_secret();

        assert_ne!(first, second);
        assert!(!first.is_empty());
        assert!(!second.is_empty());
    }

    #[tokio::test]
    async fn test_read_handshake_frame_partial_eof_returns_complete_frame() {
        // Arrange
        let mut reader = BufReader::new(&b"{\"type\":\"hello\"}"[..]);

        // Act
        let frame = read_handshake_frame(&mut reader, "fixture", "startup hello")
            .await
            .expect("partial final frame should be readable");

        // Assert
        assert_eq!(frame.as_deref(), Some("{\"type\":\"hello\"}"));
    }

    #[tokio::test]
    async fn test_read_handshake_frame_crlf_strips_line_terminator() {
        // Arrange
        let mut reader = BufReader::new(&b"{\"type\":\"ready\"}\r\nsecond\n"[..]);

        // Act
        let first = read_handshake_frame(&mut reader, "fixture", "ready signal")
            .await
            .expect("CRLF frame should be readable");
        let second = read_handshake_frame(&mut reader, "fixture", "next frame")
            .await
            .expect("reader must retain bytes after the first newline");

        // Assert
        assert_eq!(first.as_deref(), Some("{\"type\":\"ready\"}"));
        assert_eq!(second.as_deref(), Some("second"));
    }

    #[tokio::test]
    async fn test_read_handshake_frame_over_limit_returns_bounded_error() {
        // Arrange: the newline itself counts toward the protocol frame limit.
        let mut bytes = vec![b'a'; MAX_HANDSHAKE_FRAME_BYTES];
        bytes.push(b'\n');
        let mut reader = BufReader::new(bytes.as_slice());

        // Act
        let error = read_handshake_frame(&mut reader, "oversized-plugin", "startup hello")
            .await
            .expect_err("an oversized frame must be rejected before allocation grows further");

        // Assert
        assert!(error.contains("oversized-plugin"), "{error}");
        assert!(error.contains("startup hello"), "{error}");
        assert!(error.contains("byte limit"), "{error}");
    }

    #[test]
    fn legacy_startup_eof_is_actionable_without_exposing_stderr() {
        let secret = "postgres://admin:secret@example.test/temps";
        let error = format!(
            "{}{}",
            legacy_startup_eof_error("plugin-v0.0.8"),
            plugin_stderr_context(secret.len())
        );

        assert!(error.contains("incompatible or uses a legacy temps-plugin-sdk"));
        assert!(error.contains("rebuild it with protocol v2"));
        assert!(error.contains("diagnostic output"));
        assert!(error.contains("withheld to prevent secret disclosure"));
        assert!(!error.contains(secret));
    }

    #[tokio::test]
    async fn plugin_command_environment_is_scrubbed() {
        let mut command = Command::new("sh");
        command.env("TEMPS_TEST_SECRET", "must-not-leak");
        scrub_plugin_environment(&mut command);
        let status = command
            .arg("-c")
            .arg("test -z \"$TEMPS_TEST_SECRET\"")
            .status()
            .await
            .expect("run environment probe");
        assert!(status.success());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_prepare_candidate_wrong_identity_receives_no_launch_configuration() {
        use std::os::unix::fs::PermissionsExt as _;

        // Arrange: the child identifies itself, then records stdin only if the
        // host sends the second (secret-bearing) handshake frame.
        let temp = tempfile::tempdir().expect("tempdir");
        let mut config = ExternalPluginConfig::new(
            temp.path().to_path_buf(),
            "postgres://admin:must-not-leak@example.test/temps".to_string(),
        );
        config.handshake_timeout = Duration::from_secs(2);
        std::fs::create_dir_all(&config.plugins_dir).expect("plugins directory");
        let marker = temp.path().join("launch-config-received");
        let manifest = PluginManifest::builder("wrong-plugin", "1.0.0")
            .requires_db(true)
            .requires_host_data_access(true)
            .build();
        let hello = serde_json::to_string(&HandshakeMessage::Hello(
            temps_core::external_plugin::PluginHello {
                protocol_version: EXTERNAL_PLUGIN_PROTOCOL_VERSION,
                manifest: Box::new(manifest),
            },
        ))
        .expect("serialize hello");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' '{}'\nif IFS= read -r launch; then printf '%s' \"$launch\" > '{}'; fi\n",
            hello.replace('\'', "'\\''"),
            marker.display()
        );
        let binary = config.plugins_dir.join("wrong-identity-fixture");
        std::fs::write(&binary, script.as_bytes()).expect("fixture script");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o500))
            .expect("fixture permissions");
        let digest = hex::encode(Sha256::digest(script.as_bytes()));
        let manager = ExternalPluginManager::new(config, mock_db());

        // Act
        let error = match manager
            .prepare_candidate("expected-plugin", "1.0.0", &digest, &binary)
            .await
        {
            Ok(_) => panic!("signed and declared identities must match"),
            Err(error) => error,
        };
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Assert
        assert!(error.contains("wrong-plugin"), "{error}");
        assert!(error.contains("expected-plugin"), "{error}");
        assert!(!error.contains("must-not-leak"), "{error}");
        assert!(
            !marker.exists(),
            "identity rejection must happen before the launch secret is written"
        );
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
        let result = manager.reload_all().await;
        assert!(result.manifests.is_empty());
        assert!(result.failures.is_empty());
        assert!(manager.manifests().await.is_empty());
    }

    #[tokio::test]
    async fn test_reload_all_invalid_active_install_reports_typed_failure() {
        // Arrange
        let temp = tempfile::tempdir().expect("tempdir");
        let config = ExternalPluginConfig::new(
            temp.path().to_path_buf(),
            "postgres://localhost/test".to_string(),
        );
        std::fs::create_dir_all(config.plugins_dir.join("broken-plugin"))
            .expect("broken plugin directory");
        let manager = ExternalPluginManager::new(config, mock_db());

        // Act
        let result = manager.reload_all().await;

        // Assert
        assert!(result.manifests.is_empty());
        assert_eq!(result.failures.len(), 1);
        assert_eq!(
            result.failures[0].reason,
            "Activated plugin installation failed verification"
        );
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

    #[test]
    #[cfg(unix)]
    fn secure_socket_directory_uses_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let socket_dir = tmp.path().join("sockets");
        secure_socket_directory(&socket_dir).expect("secure socket directory");

        let mode = std::fs::metadata(socket_dir)
            .expect("socket directory metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn stale_pid_cleanup_never_kills_a_reused_process_id() {
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

        // Clean stale bookkeeping. A numeric PID alone is deliberately not
        // sufficient authority to signal a process because the OS may have
        // reused it.
        manager.kill_stale_processes().await;

        let still_alive = unsafe { libc::kill(pid as i32, 0) } == 0;
        assert!(still_alive, "PID-file cleanup must not kill a process");

        // PID file should be cleaned up
        assert!(!pid_file.exists(), "PID file should be removed");

        child.start_kill().expect("terminate test process");
        child.wait().await.expect("reap test process");
    }
}
