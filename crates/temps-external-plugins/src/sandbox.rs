// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Running a plugin in a hardened container with storage of its own.
//!
//! A plugin started here sees three things of the host and nothing else: a
//! read-only copy of its own verified executable, its private data directory,
//! and a directory holding the Unix socket Temps reaches it through. The
//! container starts from the same hardened host config every deployment
//! container gets ([`temps_workload_docker::hardened_host_config`]: every
//! capability dropped, `no-new-privileges`, a PID limit, an init process), and
//! adds a read-only root filesystem, a non-root user and memory and CPU caps.
//!
//! The startup protocol does not change. The container's stdin and stdout
//! carry the same staged handshake a child process's pipes do, so a plugin
//! binary that runs as a process runs in a container without being rebuilt.
//!
//! What a container deliberately cannot be given is the control-plane
//! database URL or the instance data directory: a plugin whose manifest asks
//! for either is refused here and has to be run without a container, which is
//! an explicit, audited choice an administrator makes for one plugin.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bollard::models::{ContainerCreateBody, HostConfig, HostConfigLogConfig, RestartPolicy};
use bollard::query_parameters::{
    AttachContainerOptionsBuilder, CreateContainerOptionsBuilder, ListContainersOptionsBuilder,
    RemoveContainerOptionsBuilder, StartContainerOptions,
};
use bollard::Docker;
use futures::StreamExt as _;
use serde::{Deserialize, Serialize};
use temps_core::DockerHandle;
use thiserror::Error;
use tokio::io::{AsyncWrite, AsyncWriteExt as _};
use tracing::{debug, warn};
use utoipa::ToSchema;

/// The image a plugin container starts from: the Debian base the official
/// Temps image is built on (`Dockerfile.release`), pinned by digest. Plugin
/// executables are built for the server's own platform and have always run
/// inside that userland, so nothing a plugin links against changes.
pub const PLUGIN_BASE_IMAGE: &str =
    "debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251";

/// Where the plugin's executable is mounted, read-only.
pub const CONTAINER_BINARY_DIR: &str = "/plugin";
/// The file name of the staged executable inside [`CONTAINER_BINARY_DIR`].
pub const STAGED_BINARY_FILE: &str = "plugin";
/// Where the directory holding the plugin's Unix socket is mounted.
pub const CONTAINER_SOCKET_DIR: &str = "/run/temps-plugin";
/// The socket's file name in that directory, on both sides of the mount.
pub const SOCKET_FILE: &str = "p.sock";
/// Where the plugin's private data directory is mounted.
pub const CONTAINER_DATA_DIR: &str = "/data";
/// Where the host's CA bundle is mounted, when the host has one.
pub const CONTAINER_CA_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";

/// The user a plugin runs as when Temps itself is root. Never root: the
/// conventional unprivileged `nonroot` id.
pub const UNPRIVILEGED_UID: u32 = 65_532;

const LABEL_HOST: &str = "sh.temps.plugin-host";
const LABEL_PLUGIN: &str = "sh.temps.plugin";
const LABEL_INSTANCE: &str = "sh.temps.plugin-instance";
const LABEL_SHA256: &str = "sh.temps.plugin-sha256";

const IMAGE_PULL_TIMEOUTS: temps_docker_util::PullTimeouts = temps_docker_util::PullTimeouts {
    idle: Duration::from_secs(60),
    total: Duration::from_secs(300),
};
const DOCKER_CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Handshake frames are capped at 2 MiB by the manager; the pipe only has to
/// keep the pump from running ahead of the reader.
const STDOUT_PIPE_BYTES: usize = 64 * 1024;

/// How an administrator asked for one plugin to be isolated. Stored on the
/// plugin's actor row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PluginIsolation {
    /// In a hardened container with storage of its own. The default for a
    /// plugin installed as an executable.
    Container,
    /// As a host process, with the access of the account that starts it.
    Process,
    /// As a Temps service: the container of an ordinary deployment. Never
    /// chosen for an executable; a plugin gets it by being added from the
    /// service it runs as.
    Service,
}

impl PluginIsolation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Container => "container",
            Self::Process => "process",
            Self::Service => "service",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "container" => Some(Self::Container),
            "process" => Some(Self::Process),
            "service" => Some(Self::Service),
            _ => None,
        }
    }

    /// Whether this is a way to run an installed executable, which is what
    /// an installation or an isolation change may ask for.
    pub fn runs_an_executable(self) -> bool {
        !matches!(self, Self::Service)
    }
}

/// What actually separates a running plugin from Temps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IsolationMode {
    /// A hardened container.
    Container,
    /// Another operating-system account, through the plugin launcher.
    SeparateUser,
    /// Nothing: the plugin is a process of the Temps account.
    Process,
    /// The hardened container of a Temps deployment, reached over the
    /// network.
    Service,
}

impl IsolationMode {
    /// Whether the plugin is kept away from Temps' own files and processes.
    pub fn is_sandboxed(self) -> bool {
        !matches!(self, Self::Process)
    }
}

/// Resource caps of one plugin container. They are ceilings, not
/// reservations: an idle plugin uses what it uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginLimits {
    /// Memory, and memory plus swap: the container gets no swap of its own.
    pub memory_bytes: i64,
    /// CPU time in units of 1e-9 CPUs.
    pub nano_cpus: i64,
    /// Processes and threads.
    pub pids: i64,
}

impl Default for PluginLimits {
    fn default() -> Self {
        Self {
            memory_bytes: 512 * 1024 * 1024,
            nano_cpus: 1_000_000_000,
            pids: 256,
        }
    }
}

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("Plugin '{plugin}' cannot run in a container on this server: {reason}")]
    Unavailable { plugin: String, reason: String },
    #[error(
        "Plugin '{plugin}' asks for {access}, which a plugin in a container is never given. \
         Only a plugin an administrator chooses to run without a container can receive it"
    )]
    PrivilegedAccess {
        plugin: String,
        access: &'static str,
    },
    #[error(
        "Could not prepare the {what} '{path}' for the container of plugin '{plugin}': {reason}"
    )]
    Storage {
        plugin: String,
        what: &'static str,
        path: String,
        reason: String,
    },
    #[error("Could not pull the plugin base image '{image}' for plugin '{plugin}': {reason}")]
    Image {
        plugin: String,
        image: String,
        reason: String,
    },
    #[error(
        "Docker could not {operation} the container '{container}' of plugin '{plugin}': {reason}"
    )]
    Runtime {
        plugin: String,
        container: String,
        operation: &'static str,
        reason: String,
    },
}

impl SandboxError {
    /// What an administrator is shown. A refusal explains itself in full; a
    /// storage or Docker failure can name local paths, which stay in the
    /// server log.
    pub fn public_detail(&self) -> String {
        match self {
            Self::Unavailable { .. } | Self::PrivilegedAccess { .. } => self.to_string(),
            Self::Storage { plugin, .. } => format!(
                "The storage for the container of plugin '{plugin}' could not be prepared. \
                 The server log has the details"
            ),
            Self::Image { plugin, image, .. } => format!(
                "The plugin base image '{image}' could not be pulled for plugin '{plugin}'. \
                 Check this server's access to the image registry, then reload plugins"
            ),
            Self::Runtime {
                plugin, operation, ..
            } => format!(
                "Docker could not {operation} the container of plugin '{plugin}'. \
                 The server log has the details"
            ),
        }
    }
}

/// What this host is, as far as running a plugin container goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxHost {
    /// Whether this is Linux. A Unix socket does not cross the virtual
    /// machine a desktop Docker runs in.
    pub linux: bool,
    /// Whether this build targets glibc, the libc of [`PLUGIN_BASE_IMAGE`].
    pub glibc: bool,
    /// Whether Temps itself runs inside a container. Its files then live in
    /// a volume the Docker daemon does not see at the same path.
    pub inside_container: bool,
    /// The account Temps runs as.
    pub uid: u32,
    pub gid: u32,
}

impl SandboxHost {
    /// This process, on this build.
    pub fn detect(inside_container: bool) -> Self {
        Self {
            linux: cfg!(target_os = "linux"),
            glibc: cfg!(target_env = "gnu"),
            inside_container,
            uid: crate::launcher::temps_uid(),
            gid: temps_gid(),
        }
    }

    /// The user a plugin container runs as: the Temps account, so the
    /// plugin's owner-only socket is one Temps can connect to, or the
    /// unprivileged id when Temps is root. Never root.
    pub fn container_user(&self) -> (u32, u32) {
        if self.uid == 0 {
            (UNPRIVILEGED_UID, UNPRIVILEGED_UID)
        } else {
            (self.uid, self.gid)
        }
    }

    fn unsupported_reason(&self) -> Option<String> {
        if !self.linux {
            return Some(
                "plugin containers need a Linux server. Temps reaches a plugin through a Unix \
                 socket, which does not cross the virtual machine Docker runs in on this \
                 operating system"
                    .to_string(),
            );
        }
        if !self.glibc {
            return Some(
                "this Temps build targets musl, and the plugin base image provides glibc only"
                    .to_string(),
            );
        }
        if self.inside_container {
            return Some(
                "Temps itself runs inside a container, so the Docker daemon does not see plugin \
                 files at the paths Temps uses. Run Temps directly on the host to sandbox plugins"
                    .to_string(),
            );
        }
        None
    }
}

fn temps_gid() -> u32 {
    #[cfg(unix)]
    {
        // SAFETY: getegid has no preconditions.
        unsafe { libc::getegid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// The container runtime plugins are sandboxed with, or the reason this
/// server has none.
#[derive(Debug, Clone)]
pub struct ContainerSandbox {
    docker: DockerHandle,
    host: SandboxHost,
}

impl ContainerSandbox {
    pub fn new(docker: DockerHandle, host: SandboxHost) -> Self {
        Self { docker, host }
    }

    /// A sandbox for a plugin host that was given no container runtime.
    pub fn not_configured() -> Self {
        Self {
            docker: DockerHandle::disabled(
                "full",
                "the plugin host was started without a Docker client",
            ),
            host: SandboxHost::detect(false),
        }
    }

    pub fn host(&self) -> &SandboxHost {
        &self.host
    }

    /// The Docker client, or why plugins cannot be put in containers here.
    pub fn availability(&self) -> Result<Arc<Docker>, String> {
        if let Some(reason) = self.host.unsupported_reason() {
            return Err(reason);
        }
        self.docker.require().map_err(|error| {
            format!(
                "no Docker daemon is available to this process ({}). Install Docker on this \
                 server, or run plugins on a full-profile instance",
                error.reason
            )
        })
    }

    /// Stable identity of this installation, so two instances on one host
    /// never touch each other's plugin containers.
    pub fn host_id(host_data_dir: &Path) -> String {
        use sha2::{Digest as _, Sha256};
        let digest = Sha256::digest(host_data_dir.to_string_lossy().as_bytes());
        hex::encode(&digest[..6])
    }

    /// Remove plugin containers a previous run of this installation left
    /// behind. Only containers carrying this installation's label are
    /// touched.
    pub async fn remove_stale(&self, host_id: &str) {
        let Ok(docker) = self.availability() else {
            return;
        };
        let filters =
            HashMap::from([("label".to_string(), vec![format!("{LABEL_HOST}={host_id}")])]);
        let options = ListContainersOptionsBuilder::new()
            .all(true)
            .filters(&filters)
            .build();
        let listed =
            tokio::time::timeout(DOCKER_CALL_TIMEOUT, docker.list_containers(Some(options))).await;
        let containers = match listed {
            Ok(Ok(containers)) => containers,
            Ok(Err(error)) => {
                warn!(%error, "Could not list plugin containers left by a previous run");
                return;
            }
            Err(_) => {
                warn!("Timed out listing plugin containers left by a previous run");
                return;
            }
        };
        for container in containers {
            let Some(id) = container.id else { continue };
            debug!(container = %id, "Removing a plugin container left by a previous run");
            force_remove(&docker, &id).await;
        }
    }

    /// Create, attach to and start the container for one plugin launch.
    pub(crate) async fn launch(
        &self,
        request: &ContainerLaunch<'_>,
    ) -> Result<LaunchedContainer, SandboxError> {
        let plugin = request.plugin_name;
        let docker = self
            .availability()
            .map_err(|reason| SandboxError::Unavailable {
                plugin: plugin.to_string(),
                reason,
            })?;
        for (what, path) in [
            ("plugin executable directory", request.binary_dir),
            ("plugin socket directory", request.socket_dir),
            ("plugin data directory", request.data_dir),
        ] {
            bind_source(plugin, what, path)?;
        }
        let user = self.host.container_user();
        prepare_owned_dir(plugin, "plugin socket directory", request.socket_dir, 0o700)?;
        prepare_owned_dir(plugin, "plugin data directory", request.data_dir, 0o700)?;
        if self.host.uid == 0 {
            // The plugin is not root in its container, so it has to own what
            // it writes: its socket directory and whatever data an earlier,
            // unsandboxed run left behind.
            own_tree(request.socket_dir, user).map_err(|error| SandboxError::Storage {
                plugin: plugin.to_string(),
                what: "plugin socket directory",
                path: request.socket_dir.display().to_string(),
                reason: error.to_string(),
            })?;
            own_tree(request.data_dir, user).map_err(|error| SandboxError::Storage {
                plugin: plugin.to_string(),
                what: "plugin data directory",
                path: request.data_dir.display().to_string(),
                reason: error.to_string(),
            })?;
        }

        ensure_image(&docker, plugin).await?;

        let name = container_name(request.host_id, request.instance_name);
        // A container of this name can only be ours (the name carries this
        // installation's id): a previous launch that was not cleaned up.
        force_remove(&docker, &name).await;
        let body = container_create_body(&ContainerSpec {
            plugin_name: plugin,
            instance_name: request.instance_name,
            sha256: request.sha256,
            host_id: request.host_id,
            binary_dir: request.binary_dir,
            socket_dir: request.socket_dir,
            data_dir: request.data_dir,
            host_api_url: request.host_api_url,
            ca_bundle: host_ca_bundle().as_deref(),
            user,
            limits: request.limits,
        });
        let runtime_error = |operation: &'static str, reason: String| SandboxError::Runtime {
            plugin: plugin.to_string(),
            container: name.clone(),
            operation,
            reason,
        };
        let created = tokio::time::timeout(
            DOCKER_CALL_TIMEOUT,
            docker.create_container(
                Some(CreateContainerOptionsBuilder::new().name(&name).build()),
                body,
            ),
        )
        .await
        .map_err(|_| runtime_error("create", "the Docker daemon did not answer in time".into()))?
        .map_err(|error| runtime_error("create", error.to_string()))?;
        for warning in created.warnings {
            debug!(plugin, container = %name, %warning, "Docker warned about the plugin container");
        }
        // From here the container exists: every early return removes it.
        let container = RunningContainer {
            docker: docker.clone(),
            name: name.clone(),
            removed: false,
        };

        // Attach before starting, so the plugin's first handshake frame
        // cannot be written before anything is listening for it.
        let attached = tokio::time::timeout(
            DOCKER_CALL_TIMEOUT,
            docker.attach_container(
                &name,
                Some(
                    AttachContainerOptionsBuilder::new()
                        .stream(true)
                        .stdin(true)
                        .stdout(true)
                        .stderr(true)
                        .build(),
                ),
            ),
        )
        .await
        .map_err(|_| {
            runtime_error(
                "attach to",
                "the Docker daemon did not answer in time".into(),
            )
        })?
        .map_err(|error| runtime_error("attach to", error.to_string()))?;

        tokio::time::timeout(
            DOCKER_CALL_TIMEOUT,
            docker.start_container(&name, None::<StartContainerOptions>),
        )
        .await
        .map_err(|_| runtime_error("start", "the Docker daemon did not answer in time".into()))?
        .map_err(|error| runtime_error("start", error.to_string()))?;

        let (stdout_writer, stdout) = tokio::io::duplex(STDOUT_PIPE_BYTES);
        let stderr_bytes = Arc::new(AtomicUsize::new(0));
        let pump = tokio::spawn(pump_output(
            attached.output,
            stdout_writer,
            stderr_bytes.clone(),
        ));
        Ok(LaunchedContainer {
            container,
            stdin: Box::new(attached.input),
            stdout,
            stderr_bytes,
            pump,
        })
    }
}

/// One plugin launch, as the manager describes it.
pub(crate) struct ContainerLaunch<'a> {
    pub plugin_name: &'a str,
    pub instance_name: &'a str,
    pub sha256: &'a str,
    pub host_id: &'a str,
    /// Host directory holding the staged, verified executable.
    pub binary_dir: &'a Path,
    /// Host directory the plugin creates its socket in.
    pub socket_dir: &'a Path,
    /// The plugin's private data directory on the host.
    pub data_dir: &'a Path,
    pub host_api_url: Option<&'a str>,
    pub limits: PluginLimits,
}

/// A started container and the two ends of its handshake.
pub(crate) struct LaunchedContainer {
    pub container: RunningContainer,
    pub stdin: Box<dyn AsyncWrite + Unpin + Send>,
    pub stdout: tokio::io::DuplexStream,
    /// Bytes the plugin wrote to stderr. Never their content: it may carry
    /// secrets the plugin was sent.
    pub stderr_bytes: Arc<AtomicUsize>,
    pub pump: tokio::task::JoinHandle<()>,
}

/// A plugin container this process created. Dropping it removes the
/// container.
pub(crate) struct RunningContainer {
    docker: Arc<Docker>,
    name: String,
    removed: bool,
}

impl RunningContainer {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Stop and remove the container, waiting for Docker to finish.
    pub(crate) async fn remove(&mut self) {
        if std::mem::replace(&mut self.removed, true) {
            return;
        }
        force_remove(&self.docker, &self.name).await;
    }

    /// Stop and remove the container without waiting. Used where nothing can
    /// be awaited; with no async runtime left, the container is removed by
    /// the next start of this installation instead.
    pub(crate) fn remove_in_background(&mut self) {
        if std::mem::replace(&mut self.removed, true) {
            return;
        }
        let docker = self.docker.clone();
        let name = self.name.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move { force_remove(&docker, &name).await });
            }
            Err(_) => warn!(
                container = %name,
                "No async runtime is left to remove a plugin container; the next start removes it"
            ),
        }
    }
}

impl Drop for RunningContainer {
    fn drop(&mut self) {
        self.remove_in_background();
    }
}

async fn force_remove(docker: &Docker, name: &str) {
    let options = RemoveContainerOptionsBuilder::new()
        .force(true)
        .v(true)
        .build();
    match tokio::time::timeout(
        DOCKER_CALL_TIMEOUT,
        docker.remove_container(name, Some(options)),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        })) => {}
        // A container that is already being removed (auto-remove after its
        // process exited) answers 409.
        Ok(Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 409, ..
        })) => {}
        Ok(Err(error)) => warn!(container = %name, %error, "Could not remove a plugin container"),
        Err(_) => warn!(container = %name, "Timed out removing a plugin container"),
    }
}

async fn ensure_image(docker: &Docker, plugin: &str) -> Result<(), SandboxError> {
    let inspected =
        tokio::time::timeout(DOCKER_CALL_TIMEOUT, docker.inspect_image(PLUGIN_BASE_IMAGE)).await;
    if matches!(inspected, Ok(Ok(_))) {
        return Ok(());
    }
    debug!(image = PLUGIN_BASE_IMAGE, "Pulling the plugin base image");
    temps_docker_util::pull_image(
        docker,
        temps_docker_util::PullSpec::new(PLUGIN_BASE_IMAGE).timeouts(IMAGE_PULL_TIMEOUTS),
    )
    .await
    .map_err(|error| SandboxError::Image {
        plugin: plugin.to_string(),
        image: PLUGIN_BASE_IMAGE.to_string(),
        reason: error.to_string(),
    })
}

/// Forward the container's stdout to the handshake reader and count its
/// stderr. Ends when the reader is dropped (the handshake is over) or the
/// container's output closes, which releases the attach connection.
async fn pump_output(
    mut output: std::pin::Pin<
        Box<
            dyn futures::Stream<
                    Item = Result<bollard::container::LogOutput, bollard::errors::Error>,
                > + Send,
        >,
    >,
    mut stdout: tokio::io::DuplexStream,
    stderr_bytes: Arc<AtomicUsize>,
) {
    use bollard::container::LogOutput;
    while let Some(frame) = output.next().await {
        match frame {
            Ok(LogOutput::StdOut { message } | LogOutput::Console { message }) => {
                if stdout.write_all(&message).await.is_err() {
                    break;
                }
            }
            Ok(LogOutput::StdErr { message }) => {
                let _ = stderr_bytes.try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                    Some(current.saturating_add(message.len()))
                });
            }
            Ok(LogOutput::StdIn { .. }) => {}
            Err(_) => break,
        }
    }
}

/// The name of the container of one plugin instance.
pub fn container_name(host_id: &str, instance_name: &str) -> String {
    format!("temps-plugin-{host_id}-{instance_name}")
}

/// Everything that decides a plugin container's create request.
#[derive(Debug, Clone)]
pub struct ContainerSpec<'a> {
    pub plugin_name: &'a str,
    pub instance_name: &'a str,
    pub sha256: &'a str,
    pub host_id: &'a str,
    pub binary_dir: &'a Path,
    pub socket_dir: &'a Path,
    pub data_dir: &'a Path,
    pub host_api_url: Option<&'a str>,
    pub ca_bundle: Option<&'a Path>,
    pub user: (u32, u32),
    pub limits: PluginLimits,
}

/// The container-create request for one plugin.
///
/// It starts from the hardened host config every deployment container gets
/// and only tightens it. The plugin's host access is exactly the binds listed
/// here; nothing a manifest declares can add one.
pub fn container_create_body(spec: &ContainerSpec<'_>) -> ContainerCreateBody {
    let mut binds = vec![
        format!("{}:{CONTAINER_BINARY_DIR}:ro", spec.binary_dir.display()),
        format!("{}:{CONTAINER_SOCKET_DIR}:rw", spec.socket_dir.display()),
        format!("{}:{CONTAINER_DATA_DIR}:rw", spec.data_dir.display()),
    ];
    let mut env = Vec::new();
    if let Some(bundle) = spec.ca_bundle {
        binds.push(format!("{}:{CONTAINER_CA_BUNDLE}:ro", bundle.display()));
        env.push(format!("SSL_CERT_FILE={CONTAINER_CA_BUNDLE}"));
    }
    let hardened = temps_workload_docker::hardened_host_config(binds);
    let host_config = HostConfig {
        readonly_rootfs: Some(true),
        // Outbound connections only: no port is published and nothing on the
        // host's loopback interface is reachable.
        network_mode: Some("bridge".to_string()),
        publish_all_ports: Some(false),
        memory: Some(spec.limits.memory_bytes),
        memory_swap: Some(spec.limits.memory_bytes),
        nano_cpus: Some(spec.limits.nano_cpus),
        pids_limit: Some(spec.limits.pids),
        // Scratch space counted against the memory cap. Executable, because a
        // compiled JavaScript runtime unpacks native modules here.
        tmpfs: Some(HashMap::from([(
            "/tmp".to_string(),
            "rw,nosuid,nodev,size=64m".to_string(),
        )])),
        // Plugin output may carry what the plugin was sent, so Docker keeps
        // none of it, exactly as the process launcher discards it.
        log_config: Some(HostConfigLogConfig {
            typ: Some("none".to_string()),
            config: None,
        }),
        restart_policy: Some(RestartPolicy {
            name: Some(bollard::models::RestartPolicyNameEnum::NO),
            ..Default::default()
        }),
        auto_remove: Some(true),
        ..hardened
    };

    let mut command = vec![
        "--socket-path".to_string(),
        format!("{CONTAINER_SOCKET_DIR}/{SOCKET_FILE}"),
        "--data-dir".to_string(),
        CONTAINER_DATA_DIR.to_string(),
    ];
    if let Some(url) = spec.host_api_url {
        command.push("--host-api-url".to_string());
        command.push(url.to_string());
    }

    ContainerCreateBody {
        image: Some(PLUGIN_BASE_IMAGE.to_string()),
        entrypoint: Some(vec![format!("{CONTAINER_BINARY_DIR}/{STAGED_BINARY_FILE}")]),
        cmd: Some(command),
        env: Some(env),
        user: Some(format!("{}:{}", spec.user.0, spec.user.1)),
        working_dir: Some(CONTAINER_DATA_DIR.to_string()),
        // The staged handshake runs over the container's stdin and stdout.
        attach_stdin: Some(true),
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        open_stdin: Some(true),
        stdin_once: Some(true),
        tty: Some(false),
        labels: Some(HashMap::from([
            (LABEL_HOST.to_string(), spec.host_id.to_string()),
            (LABEL_PLUGIN.to_string(), spec.plugin_name.to_string()),
            (LABEL_INSTANCE.to_string(), spec.instance_name.to_string()),
            (LABEL_SHA256.to_string(), spec.sha256.to_string()),
        ])),
        host_config: Some(host_config),
        ..Default::default()
    }
}

/// A host path Docker is asked to bind must be absolute and must not contain
/// the separator of the bind syntax.
fn bind_source(plugin: &str, what: &'static str, path: &Path) -> Result<(), SandboxError> {
    let text = path.to_string_lossy();
    if !path.is_absolute() || text.contains(':') || text.contains('\n') {
        return Err(SandboxError::Storage {
            plugin: plugin.to_string(),
            what,
            path: text.into_owned(),
            reason: "the path must be absolute and must not contain ':'".to_string(),
        });
    }
    Ok(())
}

fn prepare_owned_dir(
    plugin: &str,
    what: &'static str,
    path: &Path,
    mode: u32,
) -> Result<(), SandboxError> {
    let storage = |error: std::io::Error| SandboxError::Storage {
        plugin: plugin.to_string(),
        what,
        path: path.display().to_string(),
        reason: error.to_string(),
    };
    std::fs::create_dir_all(path).map_err(storage)?;
    let metadata = std::fs::symlink_metadata(path).map_err(storage)?;
    if !metadata.is_dir() {
        return Err(storage(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "expected a directory, not a link or a file",
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o777 != mode {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
                .map_err(storage)?;
        }
    }
    #[cfg(not(unix))]
    let _ = mode;
    Ok(())
}

const MAX_OWN_DEPTH: usize = 64;

/// Give `owner` everything beneath `path`, never following a symbolic link.
#[cfg(unix)]
fn own_tree(path: &Path, owner: (u32, u32)) -> std::io::Result<()> {
    own_tree_at(path, owner, 0)
}

#[cfg(unix)]
fn own_tree_at(path: &Path, owner: (u32, u32), depth: usize) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    std::os::unix::fs::lchown(path, Some(owner.0), Some(owner.1))?;
    if metadata.is_dir() && depth < MAX_OWN_DEPTH {
        for entry in std::fs::read_dir(path)? {
            own_tree_at(&entry?.path(), owner, depth + 1)?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn own_tree(_path: &Path, _owner: (u32, u32)) -> std::io::Result<()> {
    Ok(())
}

/// The host's CA bundle, so a plugin's outbound TLS trusts what the host
/// trusts. The base image ships none.
fn host_ca_bundle() -> Option<PathBuf> {
    [
        "/etc/ssl/certs/ca-certificates.crt",
        "/etc/pki/tls/certs/ca-bundle.crt",
        "/etc/ssl/ca-bundle.pem",
        "/etc/ssl/cert.pem",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec<'a>(root: &'a Path, limits: PluginLimits) -> ContainerSpec<'a> {
        ContainerSpec {
            plugin_name: "demo",
            instance_name: "demo",
            sha256: "ab12",
            host_id: "0123456789ab",
            binary_dir: root,
            socket_dir: root,
            data_dir: root,
            host_api_url: Some("http://127.0.0.1:3000"),
            ca_bundle: None,
            user: (1000, 1000),
            limits,
        }
    }

    fn linux_host(uid: u32) -> SandboxHost {
        SandboxHost {
            linux: true,
            glibc: true,
            inside_container: false,
            uid,
            gid: uid,
        }
    }

    #[test]
    fn the_container_starts_from_the_deployment_hardening_and_tightens_it() {
        let body = container_create_body(&spec(Path::new("/srv/x"), PluginLimits::default()));
        let host = body.host_config.expect("host config");
        let deployment = temps_workload_docker::hardened_host_config(Vec::new());

        assert_eq!(host.cap_drop, deployment.cap_drop);
        assert_eq!(host.cap_drop, Some(vec!["ALL".to_string()]));
        assert_eq!(host.cap_add, None, "no capability is ever added back");
        assert_eq!(host.security_opt, deployment.security_opt);
        assert_eq!(host.init, Some(true));
        assert_eq!(host.readonly_rootfs, Some(true));
        assert_eq!(host.privileged, None);
        assert_eq!(host.network_mode.as_deref(), Some("bridge"));
        assert_eq!(host.port_bindings, None, "a plugin publishes no port");
        assert_eq!(host.memory, Some(512 * 1024 * 1024));
        assert_eq!(
            host.memory_swap, host.memory,
            "no swap beyond the memory cap"
        );
        assert_eq!(host.nano_cpus, Some(1_000_000_000));
        assert_eq!(host.pids_limit, Some(256));
        assert_eq!(host.auto_remove, Some(true));
        assert_eq!(
            host.log_config.and_then(|config| config.typ).as_deref(),
            Some("none"),
            "plugin output is never kept"
        );
    }

    #[test]
    fn the_only_host_paths_are_the_executable_the_socket_and_the_data_directory() {
        let body = container_create_body(&ContainerSpec {
            binary_dir: Path::new("/srv/run/demo"),
            socket_dir: Path::new("/tmp/tp-1/demo"),
            data_dir: Path::new("/srv/plugin-data/demo"),
            ..spec(Path::new("/unused"), PluginLimits::default())
        });
        let binds = body.host_config.and_then(|host| host.binds).expect("binds");
        assert_eq!(
            binds,
            [
                "/srv/run/demo:/plugin:ro",
                "/tmp/tp-1/demo:/run/temps-plugin:rw",
                "/srv/plugin-data/demo:/data:rw",
            ]
        );
    }

    #[test]
    fn the_host_ca_bundle_is_mounted_read_only_when_there_is_one() {
        let body = container_create_body(&ContainerSpec {
            ca_bundle: Some(Path::new("/etc/ssl/certs/ca-certificates.crt")),
            ..spec(Path::new("/srv/x"), PluginLimits::default())
        });
        let binds = body
            .host_config
            .as_ref()
            .and_then(|host| host.binds.clone())
            .expect("binds");
        assert!(binds.contains(
            &"/etc/ssl/certs/ca-certificates.crt:/etc/ssl/certs/ca-certificates.crt:ro".to_string()
        ));
        assert_eq!(
            body.env,
            Some(vec![
                "SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt".to_string()
            ])
        );
    }

    #[test]
    fn the_plugin_is_started_with_container_paths_and_never_as_root() {
        let body = container_create_body(&spec(Path::new("/srv/x"), PluginLimits::default()));
        assert_eq!(body.image.as_deref(), Some(PLUGIN_BASE_IMAGE));
        assert_eq!(body.entrypoint, Some(vec!["/plugin/plugin".to_string()]));
        assert_eq!(
            body.cmd,
            Some(
                [
                    "--socket-path",
                    "/run/temps-plugin/p.sock",
                    "--data-dir",
                    "/data",
                    "--host-api-url",
                    "http://127.0.0.1:3000",
                ]
                .map(str::to_string)
                .to_vec()
            )
        );
        assert_eq!(body.user.as_deref(), Some("1000:1000"));
        assert_eq!(body.open_stdin, Some(true));
        assert_eq!(body.tty, Some(false));
        let labels = body.labels.expect("labels");
        assert_eq!(labels[LABEL_HOST], "0123456789ab");
        assert_eq!(labels[LABEL_PLUGIN], "demo");

        assert_eq!(linux_host(1000).container_user(), (1000, 1000));
        assert_eq!(
            linux_host(0).container_user(),
            (UNPRIVILEGED_UID, UNPRIVILEGED_UID),
            "a root Temps never runs a plugin as root"
        );
    }

    #[test]
    fn configured_limits_replace_the_defaults() {
        let limits = PluginLimits {
            memory_bytes: 128 * 1024 * 1024,
            nano_cpus: 250_000_000,
            pids: 32,
        };
        let host = container_create_body(&spec(Path::new("/srv/x"), limits))
            .host_config
            .expect("host config");
        assert_eq!(host.memory, Some(128 * 1024 * 1024));
        assert_eq!(host.memory_swap, Some(128 * 1024 * 1024));
        assert_eq!(host.nano_cpus, Some(250_000_000));
        assert_eq!(host.pids_limit, Some(32));
    }

    #[test]
    fn a_host_that_cannot_sandbox_says_why() {
        let docker = || DockerHandle::disabled("control-plane", "no socket is mounted");
        let not_linux = ContainerSandbox::new(
            docker(),
            SandboxHost {
                linux: false,
                ..linux_host(1000)
            },
        );
        assert!(not_linux.availability().unwrap_err().contains("Linux"));

        let musl = ContainerSandbox::new(
            docker(),
            SandboxHost {
                glibc: false,
                ..linux_host(1000)
            },
        );
        assert!(musl.availability().unwrap_err().contains("musl"));

        let nested = ContainerSandbox::new(
            docker(),
            SandboxHost {
                inside_container: true,
                ..linux_host(1000)
            },
        );
        assert!(nested
            .availability()
            .unwrap_err()
            .contains("runs inside a container"));

        let no_docker = ContainerSandbox::new(docker(), linux_host(1000));
        let reason = no_docker.availability().unwrap_err();
        assert!(reason.contains("no socket is mounted"), "{reason}");
        assert!(reason.contains("Install Docker"), "{reason}");
    }

    #[tokio::test]
    async fn a_launch_without_a_runtime_is_refused_before_anything_is_created() {
        let root = tempfile::tempdir().unwrap();
        let sandbox = ContainerSandbox::new(
            DockerHandle::disabled("full", "Docker is not installed"),
            linux_host(1000),
        );
        let error = match sandbox
            .launch(&ContainerLaunch {
                plugin_name: "demo",
                instance_name: "demo",
                sha256: "ab12",
                host_id: "0123456789ab",
                binary_dir: &root.path().join("bin"),
                socket_dir: &root.path().join("socket"),
                data_dir: &root.path().join("data"),
                host_api_url: None,
                limits: PluginLimits::default(),
            })
            .await
        {
            Ok(_) => panic!("a launch without Docker must fail"),
            Err(error) => error,
        };
        assert!(
            matches!(&error, SandboxError::Unavailable { plugin, reason }
                if plugin == "demo" && reason.contains("Docker is not installed")),
            "{error}"
        );
        assert!(!root.path().join("data").exists(), "nothing was prepared");
    }

    #[test]
    fn a_bind_source_cannot_smuggle_a_second_mount() {
        let error =
            bind_source("demo", "plugin data directory", Path::new("/srv/a:/etc")).unwrap_err();
        assert!(matches!(error, SandboxError::Storage { .. }));
        assert!(bind_source("demo", "plugin data directory", Path::new("relative")).is_err());
        assert!(bind_source("demo", "plugin data directory", Path::new("/srv/data")).is_ok());
    }

    #[test]
    fn isolation_round_trips_through_its_stored_form() {
        for isolation in [PluginIsolation::Container, PluginIsolation::Process] {
            assert_eq!(PluginIsolation::parse(isolation.as_str()), Some(isolation));
        }
        assert_eq!(PluginIsolation::parse("vm"), None);
        assert!(IsolationMode::Container.is_sandboxed());
        assert!(IsolationMode::SeparateUser.is_sandboxed());
        assert!(!IsolationMode::Process.is_sandboxed());
    }

    #[test]
    fn container_names_carry_the_installation_and_the_instance() {
        assert_eq!(
            container_name("0123456789ab", "demo"),
            "temps-plugin-0123456789ab-demo"
        );
        let first = ContainerSandbox::host_id(Path::new("/srv/temps-a"));
        let second = ContainerSandbox::host_id(Path::new("/srv/temps-b"));
        assert_eq!(first.len(), 12);
        assert_ne!(first, second);
        assert_eq!(first, ContainerSandbox::host_id(Path::new("/srv/temps-a")));
    }

    #[cfg(unix)]
    #[test]
    fn a_prepared_directory_is_private_and_a_link_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("data");
        prepare_owned_dir("demo", "plugin data directory", &dir, 0o700).unwrap();
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );

        let target = root.path().join("elsewhere");
        std::fs::create_dir(&target).unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(prepare_owned_dir("demo", "plugin data directory", &link, 0o700).is_err());
    }

    /// What a plugin launched by [`ContainerSandbox::launch`] can see and do,
    /// as reported by a script standing in for the plugin. Returns `None`
    /// when no Docker daemon answers.
    #[cfg(unix)]
    async fn confinement_report(host: SandboxHost, root: &Path) -> Option<Vec<String>> {
        use std::os::unix::fs::PermissionsExt as _;
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

        let docker = Docker::connect_with_local_defaults().ok()?;
        if docker.ping().await.is_err() {
            return None;
        }
        let docker = Arc::new(docker);
        let sandbox = ContainerSandbox::new(DockerHandle::available(docker.clone()), host);

        let binary_dir = root.join("staged");
        let socket_dir = root.join("socket");
        let data_dir = root.join("data");
        std::fs::create_dir_all(&binary_dir).expect("staged directory");
        std::fs::set_permissions(&binary_dir, std::fs::Permissions::from_mode(0o755))
            .expect("staged directory mode");
        let script = "#!/bin/sh\n\
            echo \"uid=$(id -u)\"\n\
            IFS= read -r line\n\
            echo \"received=$line\"\n\
            probe() { if ( : > \"$2\" ) 2>/dev/null; then echo \"$1=writable\"; else echo \"$1=read-only\"; fi; }\n\
            probe data /data/kept\n\
            probe socket /run/temps-plugin/probe\n\
            probe tmp /tmp/probe\n\
            probe executable /plugin/probe\n\
            probe rootfs /etc/probe\n\
            if [ -e /var/run/docker.sock ]; then echo docker=visible; else echo docker=hidden; fi\n\
            echo \"args=$*\"\n\
            echo done\n\
            sleep 30\n";
        let binary = binary_dir.join(STAGED_BINARY_FILE);
        std::fs::write(&binary, script).expect("fixture plugin");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o555))
            .expect("fixture plugin mode");

        let instance = format!("test-{}", &uuid::Uuid::new_v4().simple().to_string()[..16]);
        let host_id = ContainerSandbox::host_id(root);
        let mut launched = sandbox
            .launch(&ContainerLaunch {
                plugin_name: "confined",
                instance_name: &instance,
                sha256: "00",
                host_id: &host_id,
                binary_dir: &binary_dir,
                socket_dir: &socket_dir,
                data_dir: &data_dir,
                host_api_url: Some("http://127.0.0.1:3000"),
                limits: PluginLimits::default(),
            })
            .await
            .expect("launch the plugin container");
        let name = launched.container.name().to_string();

        launched
            .stdin
            .write_all(b"launch-configuration\n")
            .await
            .expect("write the launch line");
        launched.stdin.flush().await.expect("flush the launch line");
        let mut lines = tokio::io::BufReader::new(&mut launched.stdout).lines();
        let mut report = Vec::new();
        let read = tokio::time::timeout(std::time::Duration::from_secs(60), async {
            while let Ok(Some(line)) = lines.next_line().await {
                if line == "done" {
                    break;
                }
                report.push(line);
            }
        })
        .await;
        let inspected = docker
            .inspect_container(
                &name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await;
        launched.container.remove().await;
        launched.pump.abort();
        assert!(
            read.is_ok(),
            "the plugin container did not report: {report:?}"
        );

        let inspected = inspected.expect("inspect the running plugin container");
        let config = inspected.host_config.expect("host config");
        assert_eq!(config.readonly_rootfs, Some(true));
        assert_eq!(config.cap_drop, Some(vec!["ALL".to_string()]));
        assert_eq!(config.memory, Some(PluginLimits::default().memory_bytes));
        assert_eq!(config.pids_limit, Some(PluginLimits::default().pids));
        assert!(
            inspected
                .network_settings
                .and_then(|network| network.ports)
                .is_none_or(|ports| ports.is_empty()),
            "a plugin container publishes no port"
        );
        let gone = docker
            .inspect_container(
                &name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await;
        assert!(
            matches!(
                gone,
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 404,
                    ..
                })
            ),
            "removing the plugin must remove its container"
        );
        assert!(
            data_dir.join("kept").exists(),
            "what the plugin writes to its data directory outlives its container"
        );
        Some(report)
    }

    /// A real daemon accepts the hardened create request, and the plugin it
    /// starts can write its own storage and nothing else. Runs where the
    /// sandbox is supported (a Linux host with Docker) and says why it skips
    /// elsewhere.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_launched_plugin_is_confined_to_its_own_storage_by_a_real_daemon() {
        let host = SandboxHost::detect(false);
        if let Some(reason) = host.unsupported_reason() {
            eprintln!("Skipping plugin container launch test: {reason}");
            return;
        }
        let root = tempfile::tempdir().expect("tempdir");
        let Some(report) = confinement_report(host.clone(), root.path()).await else {
            eprintln!("Skipping plugin container launch test: Docker unavailable");
            return;
        };

        let (uid, _) = host.container_user();
        assert_ne!(uid, 0, "a plugin never runs as root in its container");
        for expected in [
            format!("uid={uid}"),
            "received=launch-configuration".to_string(),
            "data=writable".to_string(),
            "socket=writable".to_string(),
            "tmp=writable".to_string(),
            "executable=read-only".to_string(),
            "rootfs=read-only".to_string(),
            "docker=hidden".to_string(),
        ] {
            assert!(
                report.contains(&expected),
                "missing {expected:?} in {report:?}"
            );
        }
        let args = report
            .iter()
            .find(|line| line.starts_with("args="))
            .expect("the plugin reports its arguments");
        assert!(
            args.contains("--socket-path /run/temps-plugin/p.sock"),
            "{args}"
        );
        assert!(args.contains("--data-dir /data"), "{args}");
        assert!(
            args.contains("--host-api-url http://127.0.0.1:3000"),
            "{args}"
        );
    }
}
