//! Docker implementation of ImageBuilder and ContainerDeployer traits

use crate::{
    BuildRequest, BuildResult, BuilderError, ContainerDeployer, ContainerInfo, ContainerRuntime,
    ContainerStatus, DeployRequest, DeployResult, DeployerError, ImageBuilder, PortMapping,
    Protocol, RuntimeInfo,
};
use async_trait::async_trait;
use bollard::{
    query_parameters::{
        BuilderVersion, InspectContainerOptions, ListContainersOptions, LogsOptions,
        RemoveContainerOptions, StartContainerOptions, StopContainerOptions, TagImageOptions,
    },
    Docker,
};
use futures::{Stream, StreamExt, TryStreamExt};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use sysinfo::{System, SystemExt};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tracing::{debug, error, info, warn};

pub struct DockerRuntime {
    docker: Arc<Docker>,
    use_buildkit: bool,
    network_name: String,
    /// Address to bind host ports to (e.g. "127.0.0.1" for local, "0.0.0.0" for remote agents)
    host_bind_address: String,
    /// Optional secondary network for multi-host overlay (e.g. "temps-overlay").
    /// When set, every container is additionally connected to this network
    /// after creation. Skipped silently when the network doesn't exist —
    /// that's the legitimate "overlay not yet bootstrapped on this node"
    /// state, not an error. Set via [`Self::with_overlay_network`].
    overlay_network: Option<String>,
    /// Static resolvers to write into each new container's
    /// `/etc/resolv.conf`. Use [`Self::with_dns_servers`] for tests or
    /// fixed-IP setups; in the live agent we use
    /// [`Self::with_overlay_dns_slot`] instead, which reads the bridge
    /// IP dynamically from the network_sync loop's shared slot (the
    /// bridge IP isn't known until the overlay has bootstrapped).
    dns_servers: Vec<String>,
    /// Optional dynamic DNS slot — populated by the agent's
    /// `network_sync` loop after the overlay bridge is up. Read on
    /// every `deploy_container` so containers booted after the
    /// overlay is up get the per-node Hickory resolver wired in. The
    /// static `dns_servers` field takes precedence when both are set.
    overlay_dns_slot: Option<Arc<std::sync::RwLock<Option<std::net::IpAddr>>>>,
    /// Shared snapshot of overlay peers, refreshed by the agent's
    /// `network_sync` loop. After dual-attaching a container to the
    /// overlay, we install one route per peer inside the container's
    /// netns so traffic for *other* workers' overlay /24s leaves
    /// through the overlay interface (eth1) rather than falling
    /// through the container's default route on the primary network.
    /// Wrapped in `Option` so non-overlay deployers (e.g. tests) can
    /// skip the wiring entirely.
    overlay_peers: Option<Arc<std::sync::RwLock<Vec<temps_network::Peer>>>>,
}

impl DockerRuntime {
    /// Per-container directory that holds plaintext secret files for bind-mounting
    /// into `/run/secrets`. Lives outside `TempDir` because Docker keeps the
    /// directory open for the container lifetime; cleanup happens in
    /// `remove_container`.
    fn secrets_host_dir(&self, container_name: &str) -> PathBuf {
        // Use std::env::temp_dir() so the path matches what the Docker daemon
        // can see — both daemon and us run on the same host in the local
        // (non-remote) deployer path.
        std::env::temp_dir()
            .join("temps-secrets")
            .join(container_name)
    }

    /// Resolves the numeric (uid, gid) that the container will run as,
    /// by inspecting the image's `Config.User`. Used to `chown` the
    /// bind-mounted secrets directory so the app can read its secret
    /// files even when the image runs as a non-root user (e.g. distroless
    /// nonroot = 65532:65532).
    ///
    /// Falls back to `(0, 0)` — matching Docker's default when no USER is
    /// set — on inspect failure or when the USER is a named user we can't
    /// resolve without reading `/etc/passwd` from inside the image. Named
    /// users are logged as a warning so it's obvious why secrets might be
    /// unreadable for images like `node:alpine` (USER=node).
    async fn resolve_image_user(&self, image_name: &str) -> (u32, u32) {
        let inspect = match self.docker.inspect_image(image_name).await {
            Ok(i) => i,
            Err(e) => {
                warn!(
                    "Failed to inspect image '{}' for secrets chown; defaulting to root: {}",
                    image_name, e
                );
                return (0, 0);
            }
        };

        let user_spec = inspect
            .config
            .as_ref()
            .and_then(|c| c.user.as_ref())
            .map(|u| u.as_str())
            .unwrap_or("");

        match parse_numeric_user_spec(user_spec) {
            Some(pair) => pair,
            None => {
                warn!(
                    "Image '{}' declares USER='{}' which is not numeric; \
                     secrets will be owned by root and may be unreadable. \
                     Use numeric UIDs (e.g. USER 1000:1000) for secrets \
                     to work with this image.",
                    image_name, user_spec
                );
                (0, 0)
            }
        }
    }

    pub fn new(docker: Arc<Docker>, use_buildkit: bool, network_name: String) -> Self {
        Self {
            docker,
            use_buildkit,
            network_name,
            host_bind_address: "127.0.0.1".to_string(),
            overlay_network: None,
            dns_servers: Vec::new(),
            overlay_dns_slot: None,
            overlay_peers: None,
        }
    }

    /// Read the per-node Hickory resolver IP from a shared slot
    /// populated by the agent's `network_sync` loop after the overlay
    /// bridge is up. Required for app containers to resolve
    /// `*.temps.local` (cluster member FQDNs, service VIPs).
    pub fn with_overlay_dns_slot(
        mut self,
        slot: Arc<std::sync::RwLock<Option<std::net::IpAddr>>>,
    ) -> Self {
        self.overlay_dns_slot = Some(slot);
        self
    }

    /// Wire the agent's shared peer-list snapshot into the deployer.
    /// Used after `connect_network` to add per-peer routes inside the
    /// container's netns so cross-worker overlay traffic actually
    /// flows. Without this, containers can dial peers in their own /24
    /// but anything in a peer worker's /24 falls through to the
    /// primary network and gets dropped at iptables.
    pub fn with_overlay_peers(
        mut self,
        peers: Arc<std::sync::RwLock<Vec<temps_network::Peer>>>,
    ) -> Self {
        self.overlay_peers = Some(peers);
        self
    }

    /// Configure resolvers (typically the per-node Hickory bridge IP) to
    /// write into every new container's `/etc/resolv.conf`. Without this
    /// containers default to Docker's embedded DNS at 127.0.0.11, which
    /// only knows other containers on the same Docker network — it can't
    /// resolve `*.temps.local` cluster FQDNs. Setting this aligns the
    /// app-deploy path with the service-deploy path on the agent.
    pub fn with_dns_servers(mut self, servers: Vec<String>) -> Self {
        self.dns_servers = servers;
        self
    }

    /// Set the host bind address for container port mappings.
    /// Use "0.0.0.0" on agent nodes so containers are reachable from the private network.
    pub fn with_host_bind_address(mut self, address: String) -> Self {
        self.host_bind_address = address;
        self
    }

    /// Configure a secondary multi-host overlay network. When set, every
    /// new container is additionally connected to this network *after*
    /// creation, so it ends up with two interfaces:
    ///   eth0 → primary `network_name` (existing behavior, unchanged)
    ///   eth1 → `overlay_network` (cross-node traffic via VXLAN)
    ///
    /// If the overlay network does not exist on this host (the agent's
    /// `network_sync` loop has not bootstrapped it yet, or this node has
    /// no `compute_cidr` allocated), the dual-attach is skipped silently
    /// — the container still boots normally on the primary network.
    pub fn with_overlay_network(mut self, name: impl Into<String>) -> Self {
        self.overlay_network = Some(name.into());
        self
    }

    /// Best-effort additional attachment to the overlay network. Logs and
    /// returns `Ok(())` when the overlay isn't configured or doesn't
    /// exist; only true bollard errors propagate.
    async fn maybe_attach_overlay(&self, container_id: &str) -> Result<(), DeployerError> {
        let Some(overlay) = self.overlay_network.as_deref() else {
            return Ok(());
        };

        // Cheap existence probe: list_networks once. If the overlay
        // doesn't exist yet, skip (sync loop hasn't bootstrapped it).
        let networks = self
            .docker
            .list_networks(None::<bollard::query_parameters::ListNetworksOptions>)
            .await
            .map_err(|e| DeployerError::NetworkError(format!("list_networks: {}", e)))?;
        let exists = networks.iter().any(|n| n.name.as_deref() == Some(overlay));
        if !exists {
            tracing::debug!(
                container = %container_id,
                overlay,
                "overlay network not present; skipping dual-attach"
            );
            return Ok(());
        }

        let req = bollard::models::NetworkConnectRequest {
            container: container_id.to_string(),
            ..Default::default()
        };
        match self.docker.connect_network(overlay, req).await {
            Ok(()) => {
                tracing::info!(container = %container_id, overlay, "attached to overlay");
                Ok(())
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 403, ..
            }) => {
                // 403 from /networks/<id>/connect typically means "already
                // connected" — that's a no-op for our purposes.
                tracing::debug!(
                    container = %container_id,
                    overlay,
                    "container already connected to overlay (403)"
                );
                Ok(())
            }
            Err(e) => Err(DeployerError::NetworkError(format!(
                "connect_network({}): {}",
                overlay, e
            ))),
        }
    }

    /// Install per-peer routes inside the container's netns. Must be
    /// called **after** the container is started — `docker inspect`
    /// only reports a non-zero PID for a running container, and we
    /// need that PID to `nsenter` into its network namespace.
    ///
    /// Best-effort: any failure is logged and swallowed so a flaky
    /// route install doesn't break the deploy.
    pub async fn install_overlay_peer_routes(&self, container_id: &str) {
        let Some(overlay) = self.overlay_network.as_deref() else {
            return;
        };
        if let Err(e) = self
            .install_overlay_peer_routes_inner(container_id, overlay)
            .await
        {
            tracing::warn!(
                container = %container_id,
                overlay,
                error = %e,
                "Failed to install overlay peer routes; cross-worker traffic to other CIDRs will fall through the primary network and be dropped"
            );
        }
    }

    async fn install_overlay_peer_routes_inner(
        &self,
        container_id: &str,
        overlay: &str,
    ) -> Result<(), String> {
        let Some(shared) = self.overlay_peers.as_ref() else {
            return Ok(());
        };
        let peers = shared.read().map(|guard| guard.clone()).unwrap_or_default();
        if peers.is_empty() {
            return Ok(());
        }

        let inspect = self
            .docker
            .inspect_container(
                container_id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .map_err(|e| format!("inspect_container: {}", e))?;

        let pid = inspect
            .state
            .as_ref()
            .and_then(|s| s.pid)
            .filter(|p| *p > 0)
            .ok_or_else(|| "container PID not yet available".to_string())? as i32;

        let gateway = inspect
            .network_settings
            .as_ref()
            .and_then(|ns| ns.networks.as_ref())
            .and_then(|nets| nets.get(overlay))
            .and_then(|net| net.gateway.clone())
            .filter(|g| !g.is_empty())
            .ok_or_else(|| format!("no gateway recorded for overlay '{}' on container", overlay))?;

        // Convention: Docker assigns interface names in attach order.
        // Primary network = eth0, overlay attach = eth1.
        temps_network::overlay_routes::install_peer_routes_in_container(
            pid, "eth1", &gateway, &peers,
        )
        .await
        .map_err(|e| e.to_string())
    }

    pub async fn ensure_network_exists(&self) -> Result<(), DeployerError> {
        // Check if network exists
        let networks = self
            .docker
            .list_networks(None::<bollard::query_parameters::ListNetworksOptions>)
            .await
            .map_err(|e| DeployerError::NetworkError(format!("Failed to list networks: {}", e)))?;

        let network_exists = networks
            .iter()
            .any(|network| network.name.as_ref() == Some(&self.network_name));

        if !network_exists {
            info!("Creating network: {}", self.network_name);
            let create_options = bollard::models::NetworkCreateRequest {
                name: self.network_name.clone(),
                driver: Some("bridge".to_string()),
                ..Default::default()
            };

            self.docker
                .create_network(create_options)
                .await
                .map_err(|e| {
                    DeployerError::NetworkError(format!("Failed to create network: {}", e))
                })?;
        }

        Ok(())
    }

    async fn create_tar_context_body(
        &self,
        context_path: PathBuf,
    ) -> Result<http_body_util::Full<bytes::Bytes>, BuilderError> {
        use bytes::Bytes;
        use http_body_util::Full;

        // Write the tar archive to a temporary file to avoid holding the entire
        // build context in memory.  The temp file is cleaned up when `_tmp` drops.
        let tmp = tempfile::NamedTempFile::new().map_err(BuilderError::IoError)?;
        let tmp_path = tmp.path().to_path_buf();

        // Tar creation is synchronous and CPU-bound — run it on a blocking thread.
        let ctx = context_path.clone();
        let out_path = tmp_path.clone();
        tokio::task::spawn_blocking(move || {
            let file = std::fs::File::create(&out_path).map_err(BuilderError::IoError)?;
            let mut tar_builder = tar::Builder::new(file);
            tar_builder
                .append_dir_all(".", ctx)
                .map_err(BuilderError::IoError)?;
            tar_builder.finish().map_err(BuilderError::IoError)?;
            Ok::<(), BuilderError>(())
        })
        .await
        .map_err(|e| BuilderError::Other(format!("Tar task panicked: {}", e)))??;

        // Read the completed tar file back into memory for Bollard.
        let tar_data = tokio::fs::read(&tmp_path)
            .await
            .map_err(BuilderError::IoError)?;

        Ok(Full::new(Bytes::from(tar_data)))
    }

    fn get_resource_limits() -> (usize, u64) {
        let cpu_num = num_cpus::get();
        let mut sys = System::new_all();
        sys.refresh_all();
        let total_memory_gb = sys.total_memory() / 1024 / 1024; // Convert KB to GB

        // Use half of CPUs with minimum of 2
        let cpu_limit = std::cmp::max(2, cpu_num / 2);
        // Use half of memory with minimum of 2GB
        let memory_limit = std::cmp::max(2, total_memory_gb / 2);

        (cpu_limit, memory_limit)
    }

    /// Detect the native platform for Docker builds
    /// Returns the platform string in the format "linux/arch"
    fn detect_native_platform() -> String {
        #[cfg(target_arch = "x86_64")]
        {
            "linux/amd64".to_string()
        }
        #[cfg(target_arch = "aarch64")]
        {
            "linux/arm64".to_string()
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            // Fallback to amd64 for other architectures
            "linux/amd64".to_string()
        }
    }

    async fn concat_byte_stream<S>(s: S) -> Result<Vec<u8>, bollard::errors::Error>
    where
        S: Stream<Item = Result<bytes::Bytes, bollard::errors::Error>>,
    {
        s.try_fold(Vec::new(), |mut acc, chunk| async move {
            acc.extend_from_slice(&chunk[..]);
            Ok(acc)
        })
        .await
    }

    fn map_container_status(status: &str) -> ContainerStatus {
        match status {
            "created" => ContainerStatus::Created,
            "running" => ContainerStatus::Running,
            "paused" => ContainerStatus::Paused,
            "restarting" => ContainerStatus::Running,
            "removing" => ContainerStatus::Stopped,
            "exited" => ContainerStatus::Exited,
            "dead" => ContainerStatus::Dead,
            _ => ContainerStatus::Stopped,
        }
    }

    fn map_restart_policy(policy: &crate::RestartPolicy) -> bollard::models::RestartPolicyNameEnum {
        match policy {
            crate::RestartPolicy::Never => bollard::models::RestartPolicyNameEnum::NO,
            crate::RestartPolicy::Always => bollard::models::RestartPolicyNameEnum::ALWAYS,
            crate::RestartPolicy::OnFailure => bollard::models::RestartPolicyNameEnum::ON_FAILURE,
            crate::RestartPolicy::UnlessStopped => {
                bollard::models::RestartPolicyNameEnum::UNLESS_STOPPED
            }
        }
    }

    /// Find a container by its name
    /// Returns the container ID if found, or None if not found
    async fn find_container_by_name(
        &self,
        container_name: &str,
    ) -> Result<Option<String>, DeployerError> {
        use std::collections::HashMap;

        let mut filters = HashMap::new();
        filters.insert("name".to_string(), vec![container_name.to_string()]);

        let options = Some(ListContainersOptions {
            all: true, // Include stopped containers
            filters: Some(filters),
            ..Default::default()
        });

        let containers = self
            .docker
            .list_containers(options)
            .await
            .map_err(|e| DeployerError::Other(format!("Failed to list containers: {}", e)))?;

        // Docker prefixes container names with "/", so we need to match both formats
        for container in containers {
            if let Some(ref names) = container.names {
                for name in names {
                    // Remove the leading "/" that Docker adds
                    let clean_name = name.trim_start_matches('/');
                    if clean_name == container_name {
                        return Ok(container.id.clone());
                    }
                }
            }
        }

        Ok(None)
    }
}

#[async_trait]
impl ImageBuilder for DockerRuntime {
    async fn build_image(&self, request: BuildRequest) -> Result<BuildResult, BuilderError> {
        // BuildKit is automatically detected and enabled if supported by Docker daemon
        // The standard Docker build API will use BuildKit when available (Docker 18.09+)
        info!(
            "Building image {} (BuildKit: {})",
            request.image_name,
            if self.use_buildkit {
                "enabled"
            } else {
                "disabled"
            }
        );

        let start_time = Instant::now();

        self.ensure_network_exists()
            .await
            .map_err(|e| BuilderError::Other(format!("Network setup failed: {}", e)))?;

        info!(
            "Building image {} from context: {:?}",
            request.image_name, request.context_path
        );

        // Create tar archive body from build context
        let tar_body = self
            .create_tar_context_body(request.context_path.clone())
            .await?;

        // Prepare build options using Bollard
        let mut build_args = HashMap::new();
        for (key, value) in request.build_args.iter().filter(|(_, v)| !v.is_empty()) {
            build_args.insert(key.to_string(), value.to_string());
        }

        let (cpu_limit, memory_limit) = Self::get_resource_limits();

        let mut labels = HashMap::new();
        labels.insert("built-by".to_string(), "temps".to_string());
        let mut build_args = Some(build_args.clone());
        if self.use_buildkit && !request.build_args_buildkit.is_empty() {
            build_args = Some(request.build_args_buildkit.clone());
        }
        let build_options = bollard::query_parameters::BuildImageOptions {
            dockerfile: request
                .dockerfile_path
                .as_ref()
                .and_then(|p| p.strip_prefix(&request.context_path).ok())
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| "Dockerfile".to_string()),
            t: Some(request.image_name.clone()),
            buildargs: build_args,
            labels: Some(labels),
            networkmode: if self.use_buildkit {
                // BuildKit only supports "default", "host", or "none"
                Some("host".to_string())
            } else {
                // Legacy builder supports custom networks
                Some(self.network_name.clone())
            },
            platform: request
                .platform
                .unwrap_or_else(Self::detect_native_platform),
            memory: Some(((memory_limit * 1024 * 1024 * 1024) & 0x7FFFFFFF) as i32), // Convert GB to bytes
            cpuquota: Some((cpu_limit * 100000) as i32), // CPU quota in microseconds (cpu_limit * 100ms)
            cpuperiod: Some(100000),                     // CPU period in microseconds (100ms)
            version: if self.use_buildkit {
                BuilderVersion::BuilderBuildKit
            } else {
                BuilderVersion::BuilderV1
            },
            session: if self.use_buildkit {
                // Generate unique session ID for BuildKit to avoid conflicts
                Some(uuid::Uuid::new_v4().to_string())
            } else {
                None
            },
            ..Default::default()
        };

        // Open log file for streaming
        let mut log_file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&request.log_path)
            .await
            .map_err(BuilderError::IoError)?;

        let mut build_stream = self.docker.build_image(
            build_options,
            None,
            Some(http_body_util::Either::Left(tar_body)),
        );

        // Stream build output and write to log
        while let Some(build_info) = build_stream.next().await {
            match build_info {
                Ok(info) => {
                    if let Some(stream) = info.stream {
                        let _ = log_file.write_all(stream.as_bytes()).await;
                        debug!("Build: {}", stream.trim());
                    }
                    if let Some(error_detail) = info.error_detail {
                        let error = error_detail
                            .message
                            .unwrap_or_else(|| "Unknown build error".to_string());
                        error!("Build error: {}", error);
                        let _ = log_file
                            .write_all(format!("ERROR: {}\n", error).as_bytes())
                            .await;
                        return Err(BuilderError::BuildFailed(error));
                    }
                }
                Err(e) => {
                    let error_msg = format!("Build failed: {}", e);
                    error!("{}", error_msg);
                    let _ = log_file
                        .write_all(format!("ERROR: {}\n", error_msg).as_bytes())
                        .await;
                    return Err(BuilderError::BuildFailed(error_msg));
                }
            }
        }

        let _ = log_file.flush().await;

        let build_duration = start_time.elapsed().as_millis() as u64;

        // Get image info for size
        let images = self
            .docker
            .list_images(Some(bollard::query_parameters::ListImagesOptions {
                filters: {
                    let mut filters = HashMap::new();
                    filters.insert("reference".to_string(), vec![request.image_name.clone()]);
                    Some(filters)
                },
                ..Default::default()
            }))
            .await
            .map_err(|e| BuilderError::Other(format!("Failed to get image info: {}", e)))?;

        let image = images
            .first()
            .ok_or_else(|| BuilderError::Other("Built image not found".to_string()))?;

        Ok(BuildResult {
            image_id: image.id.clone(),
            image_name: request.image_name,
            size_bytes: image.size as u64,
            build_duration_ms: build_duration,
        })
    }

    async fn build_image_with_callback(
        &self,
        request_with_callback: crate::BuildRequestWithCallback,
    ) -> Result<BuildResult, BuilderError> {
        let request = request_with_callback.request;
        let log_callback = request_with_callback.log_callback;

        // BuildKit is automatically detected and enabled if supported by Docker daemon
        // The standard Docker build API will use BuildKit when available (Docker 18.09+)
        info!(
            "Building image {} with callback (BuildKit: {})",
            request.image_name,
            if self.use_buildkit {
                "enabled"
            } else {
                "disabled"
            }
        );

        let start_time = Instant::now();

        self.ensure_network_exists()
            .await
            .map_err(|e| BuilderError::Other(format!("Network setup failed: {}", e)))?;

        info!(
            "Building image {} from context: {:?} with log callback",
            request.image_name, request.context_path
        );

        // Create tar archive body from build context
        let tar_body = self
            .create_tar_context_body(request.context_path.clone())
            .await?;

        // Prepare build options using Bollard
        let mut build_args = HashMap::new();
        for (key, value) in request.build_args.iter().filter(|(_, v)| !v.is_empty()) {
            build_args.insert(key.to_string(), value.to_string());
        }

        let (cpu_limit, memory_limit) = Self::get_resource_limits();

        let mut labels = HashMap::new();
        labels.insert("built-by".to_string(), "temps".to_string());

        let build_options = bollard::query_parameters::BuildImageOptions {
            dockerfile: request
                .dockerfile_path
                .as_ref()
                .and_then(|p| p.strip_prefix(&request.context_path).ok())
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| "Dockerfile".to_string()),
            t: Some(request.image_name.clone()),
            buildargs: Some(build_args),
            labels: Some(labels),
            networkmode: if self.use_buildkit {
                // BuildKit only supports "default", "host", or "none"
                Some("default".to_string())
            } else {
                // Legacy builder supports custom networks
                Some(self.network_name.clone())
            },
            platform: request
                .platform
                .unwrap_or_else(Self::detect_native_platform),
            memory: Some(((memory_limit * 1024 * 1024 * 1024) & 0x7FFFFFFF) as i32), // Convert GB to bytes
            cpuquota: Some((cpu_limit * 100000) as i32), // CPU quota in microseconds (cpu_limit * 100ms)
            cpuperiod: Some(100000),                     // CPU period in microseconds (100ms)
            version: if self.use_buildkit {
                BuilderVersion::BuilderBuildKit
            } else {
                BuilderVersion::BuilderV1
            },
            session: if self.use_buildkit {
                // Generate unique session ID for BuildKit to avoid conflicts
                Some(uuid::Uuid::new_v4().to_string())
            } else {
                None
            },
            ..Default::default()
        };

        // Open log file for streaming
        let mut log_file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&request.log_path)
            .await
            .map_err(BuilderError::IoError)?;

        // Execute build using Bollard
        let mut build_stream = self.docker.build_image(
            build_options,
            None,
            Some(http_body_util::Either::Left(tar_body)),
        );

        // Stream build output and write to log and callback
        while let Some(build_info) = build_stream.next().await {
            match build_info {
                Ok(info) => {
                    if let Some(stream) = info.stream {
                        // Write to file
                        let _ = log_file.write_all(stream.as_bytes()).await;
                        debug!("Build: {}", stream.trim());

                        // Call log callback if provided
                        if let Some(ref callback) = log_callback {
                            callback(stream.clone()).await;
                        }
                    }
                    if let Some(error_detail) = info.error_detail {
                        let error = error_detail
                            .message
                            .unwrap_or_else(|| "Unknown build error".to_string());
                        error!("Build error: {}", error);
                        let error_line = format!("ERROR: {}\n", error);
                        let _ = log_file.write_all(error_line.as_bytes()).await;

                        // Call log callback with error
                        if let Some(ref callback) = log_callback {
                            callback(error_line.clone()).await;
                        }

                        return Err(BuilderError::BuildFailed(error));
                    }
                    if let Some(bollard::models::BuildInfoAux::BuildKit(res)) = info.aux {
                        // Emit vertex names (build step descriptions) when they
                        // start or complete.  This gives visibility into cached
                        // layers and overall build progress even when there is
                        // no command output (logs).
                        for vertex in &res.vertexes {
                            if vertex.name.is_empty() {
                                continue;
                            }
                            let status = if vertex.error.is_empty() {
                                if vertex.completed.is_some() {
                                    if vertex.cached {
                                        "CACHED"
                                    } else {
                                        "DONE"
                                    }
                                } else if vertex.started.is_some() {
                                    "RUNNING"
                                } else {
                                    continue; // Not started yet, skip
                                }
                            } else {
                                "ERROR"
                            };
                            let line = format!("[{}] {}\n", status, vertex.name);
                            let _ = log_file.write_all(line.as_bytes()).await;
                            debug!("BuildKit vertex: {}", line.trim());

                            if let Some(ref callback) = log_callback {
                                callback(line).await;
                            }
                        }

                        // Emit actual command output from build steps
                        for log in res.logs {
                            let _ = log_file.write_all(&log.msg[..]).await;
                            debug!("BuildKit: {}", String::from_utf8_lossy(&log.msg));

                            if let Some(ref callback) = log_callback {
                                callback(String::from_utf8_lossy(&log.msg[..]).to_string()).await;
                            }
                        }
                    }
                }
                Err(e) => {
                    let error_msg = format!("Build failed: {}", e);
                    error!("{}", error_msg);
                    let error_line = format!("ERROR: {}\n", error_msg);
                    let _ = log_file.write_all(error_line.as_bytes()).await;

                    // Call log callback with error
                    if let Some(ref callback) = log_callback {
                        callback(error_line).await;
                    }

                    return Err(BuilderError::BuildFailed(error_msg));
                }
            }
        }

        let _ = log_file.flush().await;

        let build_duration = start_time.elapsed().as_millis() as u64;

        // Get image info
        let images = self
            .docker
            .list_images(Some(bollard::query_parameters::ListImagesOptions {
                filters: {
                    let mut filters = HashMap::new();
                    filters.insert("reference".to_string(), vec![request.image_name.clone()]);
                    Some(filters)
                },
                ..Default::default()
            }))
            .await
            .map_err(|e| BuilderError::Other(format!("Failed to get image info: {}", e)))?;

        let image = images
            .first()
            .ok_or_else(|| BuilderError::Other("Built image not found".to_string()))?;

        Ok(BuildResult {
            image_id: image.id.clone(),
            image_name: request.image_name,
            size_bytes: image.size as u64,
            build_duration_ms: build_duration,
        })
    }

    async fn import_image(&self, image_path: PathBuf, tag: &str) -> Result<String, BuilderError> {
        info!("Importing image from {:?} with tag: {}", image_path, tag);

        let file = tokio::fs::File::open(&image_path)
            .await
            .map_err(BuilderError::IoError)?;

        let byte_stream =
            tokio_util::codec::FramedRead::new(file, tokio_util::codec::BytesCodec::new())
                .map(|r| r.map(|b| b.freeze()));

        let import_stream = self.docker.import_image_stream(
            bollard::query_parameters::ImportImageOptions {
                quiet: false,
                ..Default::default()
            },
            byte_stream,
            None,
        );

        let mut image_id = None;
        let mut stream = std::pin::Pin::new(Box::new(import_stream));

        while let Some(result) = futures::StreamExt::next(&mut stream).await {
            match result {
                Ok(info) => {
                    if let Some(stream_msg) = info.stream {
                        info!("Import progress: {}", stream_msg.trim());
                        if stream_msg.contains("Loaded image:") {
                            image_id = stream_msg
                                .split("Loaded image: ")
                                .nth(1)
                                .map(|s| s.trim().to_string());
                        }
                    }
                }
                Err(e) => {
                    return Err(BuilderError::Other(format!(
                        "Failed to import image: {}",
                        e
                    )));
                }
            }
        }

        let id = image_id.ok_or_else(|| BuilderError::Other("No image ID found".to_string()))?;

        // Tag the image
        self.docker
            .tag_image(
                &id,
                Some(TagImageOptions {
                    repo: Some(tag.split(':').next().unwrap_or(tag).to_string()),
                    tag: Some(tag.split(':').nth(1).unwrap_or("latest").to_string()),
                }),
            )
            .await
            .map_err(|e| BuilderError::Other(format!("Failed to tag image: {}", e)))?;

        Ok(id)
    }

    async fn save_image(&self, image_name: &str, output_path: &Path) -> Result<(), BuilderError> {
        info!("Exporting image '{}' to {:?}", image_name, output_path);

        let stream = self.docker.export_image(image_name);

        let mut file = tokio::fs::File::create(output_path).await.map_err(|e| {
            BuilderError::IoError(std::io::Error::new(
                e.kind(),
                format!("Failed to create tar file {:?}: {}", output_path, e),
            ))
        })?;

        let mut stream = std::pin::Pin::new(Box::new(stream));

        while let Some(chunk) = StreamExt::next(&mut stream).await {
            let bytes = chunk.map_err(|e| {
                BuilderError::Other(format!("Failed to export image '{}': {}", image_name, e))
            })?;
            file.write_all(&bytes).await.map_err(|e| {
                BuilderError::IoError(std::io::Error::new(
                    e.kind(),
                    format!("Failed to write image tar: {}", e),
                ))
            })?;
        }

        file.flush().await.map_err(BuilderError::IoError)?;

        let metadata = tokio::fs::metadata(output_path)
            .await
            .map_err(BuilderError::IoError)?;
        info!(
            "Exported image '{}' ({:.1} MB)",
            image_name,
            metadata.len() as f64 / 1_048_576.0
        );

        Ok(())
    }

    async fn extract_from_image(
        &self,
        image_name: &str,
        source_path: &str,
        destination_path: &Path,
    ) -> Result<(), BuilderError> {
        // Skip pull for local images (temps-* are built locally, not from a registry)
        if !image_name.starts_with("temps-") {
            let _ = self
                .docker
                .create_image(
                    Some(bollard::query_parameters::CreateImageOptions {
                        from_image: Some(image_name.to_string()),
                        ..Default::default()
                    }),
                    None,
                    None,
                )
                .for_each(|_| async {})
                .await;
        }

        // Create container
        let container_config = bollard::models::ContainerCreateBody {
            image: Some(image_name.to_string()),
            cmd: Some(vec!["/bin/sh".to_string()]),
            tty: Some(true),
            ..Default::default()
        };

        let container = self
            .docker
            .create_container(
                Some(bollard::query_parameters::CreateContainerOptionsBuilder::new().build()),
                container_config,
            )
            .await
            .map_err(|e| BuilderError::Other(format!("Failed to create container: {}", e)))?;

        let container_id = container.id.clone();

        // Cleanup function
        let cleanup = || async {
            let _ = self
                .docker
                .remove_container(
                    &container_id,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;
        };

        // Download from container
        let temp_dir = TempDir::new().map_err(BuilderError::IoError)?;
        let temp_path = temp_dir.path();

        let response_stream = self.docker.download_from_container(
            &container_id,
            Some(bollard::query_parameters::DownloadFromContainerOptions {
                path: source_path.to_string(),
            }),
        );

        let bytes = match Self::concat_byte_stream(response_stream).await {
            Ok(bytes) => bytes,
            Err(e) => {
                cleanup().await;
                return Err(BuilderError::Other(format!(
                    "Failed to download from container: {}",
                    e
                )));
            }
        };

        let mut archive_reader = tar::Archive::new(&bytes[..]);
        if let Err(e) = archive_reader.unpack(temp_path) {
            cleanup().await;
            return Err(BuilderError::Other(format!(
                "Failed to extract archive: {}",
                e
            )));
        }

        let last_path_component = std::path::Path::new(source_path)
            .file_name()
            .and_then(|os_str| os_str.to_str())
            .unwrap_or("");

        let extracted_dir = temp_path.join(last_path_component);

        if let Err(e) = std::fs::rename(&extracted_dir, destination_path) {
            cleanup().await;
            return Err(BuilderError::Other(format!(
                "Failed to move extracted files: {}",
                e
            )));
        }

        cleanup().await;
        Ok(())
    }

    async fn list_images(&self) -> Result<Vec<String>, BuilderError> {
        let images = self
            .docker
            .list_images(Some(bollard::query_parameters::ListImagesOptions {
                all: true,
                ..Default::default()
            }))
            .await
            .map_err(|e| BuilderError::Other(format!("Failed to list images: {}", e)))?;

        let mut all_tags = Vec::new();
        for img in images {
            // repo_tags is Vec<String> in this version of bollard
            all_tags.extend(img.repo_tags);
        }
        Ok(all_tags)
    }

    async fn remove_image(&self, image_name: &str) -> Result<(), BuilderError> {
        // Remove image - ignore any errors for now since it returns a stream
        let _stream = self.docker.remove_image(
            image_name,
            Some(bollard::query_parameters::RemoveImageOptions {
                force: true,
                ..Default::default()
            }),
            None,
        );

        Ok(())
    }

    async fn inspect_image(&self, image_name: &str) -> Result<crate::ImageInfo, BuilderError> {
        let inspect = self.docker.inspect_image(image_name).await.map_err(|e| {
            BuilderError::ImageNotFound(format!("Failed to inspect image '{}': {}", image_name, e))
        })?;

        let architecture = inspect
            .architecture
            .unwrap_or_else(|| "unknown".to_string());
        let os = inspect.os.unwrap_or_else(|| "linux".to_string());
        let platform = format!("{}/{}", os, architecture);

        let size_bytes = inspect.size.map(|s| s as u64).unwrap_or(0);

        // Get tags from repo_tags
        let tags = inspect.repo_tags.unwrap_or_default();

        let created = inspect.created.and_then(|dt| {
            chrono::DateTime::from_timestamp(dt.unix_timestamp(), dt.nanosecond())
                .map(|c| c.to_rfc3339())
        });

        // Extract WORKDIR from the image config
        let working_dir = inspect
            .config
            .as_ref()
            .and_then(|c| c.working_dir.clone())
            .filter(|w| !w.is_empty());

        Ok(crate::ImageInfo {
            id: inspect.id.unwrap_or_default(),
            architecture,
            os,
            platform,
            size_bytes,
            tags,
            created,
            working_dir,
        })
    }

    fn get_native_platform(&self) -> String {
        Self::detect_native_platform()
    }
}

#[async_trait]
impl ContainerDeployer for DockerRuntime {
    async fn deploy_container(
        &self,
        request: DeployRequest,
    ) -> Result<DeployResult, DeployerError> {
        info!(
            "Deploying container {} from image {}",
            request.container_name, request.image_name
        );

        self.ensure_network_exists().await?;

        // Check if a container with this name already exists and remove it
        match self.find_container_by_name(&request.container_name).await {
            Ok(Some(existing_container_id)) => {
                info!(
                    "🔄 Container {} already exists ({}), removing it before redeployment",
                    request.container_name, existing_container_id
                );

                // Stop the container if it's running
                if let Err(e) = self.stop_container(&existing_container_id).await {
                    warn!(
                        "⚠️  Failed to stop existing container {}: {}",
                        existing_container_id, e
                    );
                }

                // Remove the container
                if let Err(e) = self.remove_container(&existing_container_id).await {
                    return Err(DeployerError::DeploymentFailed(format!(
                        "Failed to remove existing container {}: {}",
                        existing_container_id, e
                    )));
                }

                info!("✅ Removed existing container {}", existing_container_id);
            }
            Ok(None) => {
                debug!("No existing container with name {}", request.container_name);
            }
            Err(e) => {
                warn!("⚠️  Error checking for existing container: {}", e);
            }
        }

        // Create port bindings
        let mut port_bindings = HashMap::new();
        let mut exposed_ports = Vec::new();

        for port_mapping in &request.port_mappings {
            let container_port_key =
                format!("{}/{}", port_mapping.container_port, port_mapping.protocol);
            let host_port_binding = bollard::models::PortBinding {
                host_ip: Some(self.host_bind_address.clone()),
                // When host_port is 0, let Docker pick an available port
                host_port: if port_mapping.host_port == 0 {
                    None
                } else {
                    Some(port_mapping.host_port.to_string())
                },
            };

            port_bindings.insert(container_port_key.clone(), Some(vec![host_port_binding]));
            exposed_ports.push(container_port_key);
        }

        // Create host config with log rotation to prevent unbounded disk growth
        let log_config = request
            .log_config
            .as_ref()
            .map(|lc| lc.to_bollard_log_config());

        // When secrets are present, materialize them as files in a per-container
        // host directory (mode 0700) and bind-mount that directory into the
        // container at /run/secrets (read-only). This matches how Docker Swarm
        // delivers secrets: directory is created by the mount itself, so it
        // works with any image (no requirement that /run/secrets pre-exist),
        // and files are visible from container start (no race with start).
        //
        // Trade-off vs tmpfs: plaintext lives on the host filesystem under
        // SecretsHostDir until the container is removed. The directory is
        // mode 0700 root-owned; individual files are mode 0400. Cleanup is
        // handled in `remove_container`.
        let secrets_bind = if request.secrets.is_empty() {
            None
        } else {
            let host_dir = self.secrets_host_dir(&request.container_name);
            // Resolve the image's USER so we can chown the secret files to
            // the uid that the container will actually run as — otherwise
            // mode-0400 root-owned files are unreadable by nonroot images.
            let owner = self.resolve_image_user(&request.image_name).await;
            write_secrets_to_host_dir(&host_dir, &request.secrets, Some(owner)).map_err(|e| {
                DeployerError::SecretMountFailed {
                    container_name: request.container_name.clone(),
                    reason: format!("write host dir {}: {}", host_dir.display(), e),
                }
            })?;
            // Docker bind-mount syntax: "<host_path>:<container_path>:<options>"
            Some(format!("{}:/run/secrets:ro", host_dir.display()))
        };

        // Wire the per-node Hickory resolver into the container's resolv.conf
        // when configured. Without this, containers default to Docker's
        // embedded DNS at 127.0.0.11, which can resolve names of other
        // containers on the same Docker network but NOT `*.temps.local`
        // cluster FQDNs that apps need to dial postgres-cluster members.
        //
        // Static `dns_servers` wins when set (test/manual setups);
        // otherwise read the dynamic slot the agent's network_sync
        // loop publishes after the overlay bridge is up.
        let dns_for_container: Option<Vec<String>> = if !self.dns_servers.is_empty() {
            Some(self.dns_servers.clone())
        } else if let Some(slot) = self.overlay_dns_slot.as_ref() {
            slot.read()
                .ok()
                .and_then(|guard| guard.map(|ip| vec![ip.to_string()]))
        } else {
            None
        };

        let host_config = bollard::models::HostConfig {
            port_bindings: Some(port_bindings),
            network_mode: Some(self.network_name.clone()),
            dns: dns_for_container,
            restart_policy: Some(bollard::models::RestartPolicy {
                name: Some(Self::map_restart_policy(&request.restart_policy)),
                ..Default::default()
            }),
            memory: request
                .resource_limits
                .memory_limit_mb
                .map(|mb| mb as i64 * 1024 * 1024),
            nano_cpus: request
                .resource_limits
                .cpu_limit
                .map(|cores| (cores * 1_000_000_000.0) as i64),
            log_config,
            // Security hardening: drop all Linux capabilities by default
            cap_drop: Some(vec!["ALL".to_string()]),
            // Security hardening: prevent privilege escalation via setuid/setgid
            security_opt: Some(vec!["no-new-privileges:true".to_string()]),
            // Security hardening: limit number of processes to prevent fork bombs
            pids_limit: Some(512),
            // Security hardening: use init process for proper signal handling and zombie reaping
            init: Some(true),
            binds: secrets_bind.map(|b| vec![b]),
            ..Default::default()
        };

        // Build container labels (used by log aggregator for container discovery)
        let container_labels = if request.labels.is_empty() {
            None
        } else {
            Some(request.labels.clone())
        };

        // Create container config
        let container_config = bollard::models::ContainerCreateBody {
            image: Some(request.image_name.clone()),
            env: Some(
                request
                    .environment_vars
                    .iter()
                    .map(|(k, v)| format!("{}={}", k, v))
                    .collect(),
            ),
            exposed_ports: Some(exposed_ports),
            host_config: Some(host_config),
            cmd: request.command.clone(),
            labels: container_labels,
            ..Default::default()
        };

        // Create container
        let container = self
            .docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&request.container_name)
                        .build(),
                ),
                container_config,
            )
            .await
            .map_err(|e| {
                DeployerError::DeploymentFailed(format!("Failed to create container: {}", e))
            })?;

        // Multi-host overlay: best-effort additional attach to `temps-overlay`
        // (or whatever the operator configured). Containers always boot with
        // their primary network interface (`temps-app-network`); the overlay
        // attachment is purely additive and silently no-ops when the overlay
        // network isn't present yet on this node.
        self.maybe_attach_overlay(&container.id).await?;

        // Start container
        self.docker
            .start_container(&container.id, None::<StartContainerOptions>)
            .await
            .map_err(|e| {
                DeployerError::DeploymentFailed(format!("Failed to start container: {}", e))
            })?;

        // Install overlay peer routes inside the container's netns.
        // Must run *after* start_container — `docker inspect` only
        // reports a non-zero PID for a running container, and route
        // injection uses `nsenter -t <pid> -n ip route ...`.
        self.install_overlay_peer_routes(&container.id).await;

        // Get the first port mapping for the result
        let (container_port, requested_host_port) = request
            .port_mappings
            .first()
            .map(|pm| (pm.container_port, pm.host_port))
            .unwrap_or((0, 0));

        // When host_port was 0 (Docker picks), inspect the container to get the actual port
        let host_port = if requested_host_port == 0 && container_port > 0 {
            let inspect = self
                .docker
                .inspect_container(&container.id, None::<InspectContainerOptions>)
                .await
                .map_err(|e| {
                    DeployerError::DeploymentFailed(format!(
                        "Failed to inspect container {} for port mapping: {}",
                        container.id, e
                    ))
                })?;

            let port_key = format!("{}/tcp", container_port);
            inspect
                .network_settings
                .and_then(|ns| ns.ports)
                .and_then(|ports| ports.get(&port_key).cloned())
                .flatten()
                .and_then(|bindings| bindings.first().cloned())
                .and_then(|binding| binding.host_port)
                .and_then(|p| p.parse::<u16>().ok())
                .ok_or_else(|| {
                    DeployerError::DeploymentFailed(format!(
                        "Container {} has no host port binding for {}",
                        container.id, port_key
                    ))
                })?
        } else {
            requested_host_port
        };

        Ok(DeployResult {
            container_id: container.id,
            container_name: request.container_name,
            container_port,
            host_port,
            status: ContainerStatus::Running,
        })
    }

    async fn start_container(&self, container_id: &str) -> Result<(), DeployerError> {
        self.docker
            .start_container(container_id, None::<StartContainerOptions>)
            .await
            .map_err(|e| DeployerError::Other(format!("Failed to start container: {}", e)))?;
        Ok(())
    }

    async fn stop_container(&self, container_id: &str) -> Result<(), DeployerError> {
        self.docker
            .stop_container(
                container_id,
                Some(StopContainerOptions {
                    t: Some(10),
                    signal: None,
                }),
            )
            .await
            .map_err(|e| {
                DeployerError::Other(format!("Failed to stop container {}: {}", container_id, e))
            })?;
        Ok(())
    }

    async fn pause_container(&self, container_id: &str) -> Result<(), DeployerError> {
        self.docker
            .pause_container(container_id)
            .await
            .map_err(|e| DeployerError::Other(format!("Failed to pause container: {}", e)))?;
        Ok(())
    }

    async fn resume_container(&self, container_id: &str) -> Result<(), DeployerError> {
        self.docker
            .unpause_container(container_id)
            .await
            .map_err(|e| DeployerError::Other(format!("Failed to resume container: {}", e)))?;
        Ok(())
    }

    async fn remove_container(&self, container_id: &str) -> Result<(), DeployerError> {
        // Look up the container name before removal so we can clean up its
        // per-container secrets host directory (if any). Inspect failures are
        // non-fatal: we still try to remove the container.
        let container_name = self
            .docker
            .inspect_container(container_id, None::<InspectContainerOptions>)
            .await
            .ok()
            .and_then(|c| c.name)
            // Docker prefixes inspect names with a leading '/'.
            .map(|n| n.trim_start_matches('/').to_string());

        self.docker
            .remove_container(
                container_id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| DeployerError::Other(format!("Failed to remove container: {}", e)))?;

        if let Some(name) = container_name {
            let dir = self.secrets_host_dir(&name);
            if dir.exists() {
                if let Err(e) = std::fs::remove_dir_all(&dir) {
                    warn!(
                        "Failed to clean up secrets host dir {}: {}",
                        dir.display(),
                        e
                    );
                }
            }
        }

        Ok(())
    }

    async fn get_container_info(&self, container_id: &str) -> Result<ContainerInfo, DeployerError> {
        let container = self
            .docker
            .inspect_container(container_id, None::<InspectContainerOptions>)
            .await
            .map_err(|e| DeployerError::ContainerNotFound(format!("Container not found: {}", e)))?;

        let state = container.state.unwrap_or_default();
        let config = container.config.unwrap_or_default();
        let container_labels = config.labels.clone().unwrap_or_default();

        // Parse environment variables
        let env_vars = config
            .env
            .unwrap_or_default()
            .into_iter()
            .filter_map(|env_str| {
                let parts: Vec<&str> = env_str.splitn(2, '=').collect();
                if parts.len() == 2 {
                    Some((parts[0].to_string(), parts[1].to_string()))
                } else {
                    None
                }
            })
            .collect();

        // Parse port mappings
        let port_mappings = container
            .network_settings
            .and_then(|ns| ns.ports)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(port_key, bindings)| {
                if let Some(bindings) = bindings {
                    if let Some(binding) = bindings.first() {
                        let parts: Vec<&str> = port_key.split('/').collect();
                        if parts.len() == 2 {
                            let container_port = parts[0].parse().ok()?;
                            let protocol = match parts[1] {
                                "tcp" => Protocol::Tcp,
                                "udp" => Protocol::Udp,
                                _ => Protocol::Tcp,
                            };
                            let host_port = binding.host_port.as_ref()?.parse().ok()?;

                            return Some(PortMapping {
                                host_port,
                                container_port,
                                protocol,
                            });
                        }
                    }
                }
                None
            })
            .collect();

        Ok(ContainerInfo {
            container_id: container.id.unwrap_or_default(),
            container_name: container
                .name
                .unwrap_or_default()
                .trim_start_matches('/')
                .to_string(),
            image_name: config.image.unwrap_or_default(),
            status: Self::map_container_status(
                &state.status.map(|s| s.to_string()).unwrap_or_default(),
            ),
            created_at: container
                .created
                .and_then(|dt| {
                    chrono::DateTime::from_timestamp(dt.unix_timestamp(), dt.nanosecond())
                })
                .unwrap_or_else(chrono::Utc::now),
            ports: port_mappings,
            environment_vars: env_vars,
            restart_count: container.restart_count,
            labels: container_labels,
        })
    }

    async fn get_container_stats(
        &self,
        container_id: &str,
    ) -> Result<crate::ContainerStats, DeployerError> {
        use bollard::query_parameters::StatsOptions;

        // Get container info first to get the name
        let container_info = self.get_container_info(container_id).await?;

        // Get stats from Docker - stream but take only first stat and close
        let mut stats_stream = self.docker.stats(
            container_id,
            Some(StatsOptions {
                stream: false,  // Only get one stat, don't stream
                one_shot: true, // Return immediately after first stat
            }),
        );

        // Take the first stat
        let stats_data = stats_stream
            .try_next()
            .await
            .map_err(|e| DeployerError::Other(format!("Failed to get container stats: {}", e)))?
            .ok_or_else(|| DeployerError::Other("No stats available".to_string()))?;

        // Extract CPU percentage using delta between cpu_stats and precpu_stats
        let cpu_percent = {
            let current_cpu = stats_data
                .cpu_stats
                .as_ref()
                .and_then(|cs| cs.cpu_usage.as_ref())
                .and_then(|cu| cu.total_usage);
            let current_system = stats_data
                .cpu_stats
                .as_ref()
                .and_then(|cs| cs.system_cpu_usage);
            let prev_cpu = stats_data
                .precpu_stats
                .as_ref()
                .and_then(|cs| cs.cpu_usage.as_ref())
                .and_then(|cu| cu.total_usage);
            let prev_system = stats_data
                .precpu_stats
                .as_ref()
                .and_then(|cs| cs.system_cpu_usage);

            match (current_cpu, current_system, prev_cpu, prev_system) {
                (Some(cur_cpu), Some(cur_sys), Some(pre_cpu), Some(pre_sys)) => {
                    let cpu_delta = cur_cpu as f64 - pre_cpu as f64;
                    let system_delta = cur_sys as f64 - pre_sys as f64;
                    if system_delta > 0.0 && cpu_delta >= 0.0 {
                        let num_cpus = stats_data
                            .cpu_stats
                            .as_ref()
                            .and_then(|cs| cs.online_cpus)
                            .unwrap_or(1) as f64;
                        ((cpu_delta / system_delta) * num_cpus * 100.0).clamp(0.0, 100.0)
                    } else {
                        0.0
                    }
                }
                _ => 0.0,
            }
        };

        // Extract memory stats
        let memory_stats = stats_data.memory_stats.as_ref();
        let memory_bytes = memory_stats.and_then(|ms| ms.usage).unwrap_or(0);
        let memory_limit_bytes = memory_stats.and_then(|ms| ms.limit);

        let memory_percent = if let Some(limit) = memory_limit_bytes {
            if limit > 0 {
                Some(((memory_bytes as f64 / limit as f64) * 100.0).clamp(0.0, 100.0))
            } else {
                None
            }
        } else {
            None
        };

        // Extract network stats
        let default_networks = Default::default();
        let networks_stats = stats_data.networks.as_ref().unwrap_or(&default_networks);
        let (network_rx_bytes, network_tx_bytes) =
            if let Some(net_stat) = networks_stats.values().next() {
                (
                    net_stat.rx_bytes.unwrap_or(0),
                    net_stat.tx_bytes.unwrap_or(0),
                )
            } else {
                (0, 0)
            };

        Ok(crate::ContainerStats {
            container_id: container_info.container_id,
            container_name: container_info.container_name,
            cpu_percent,
            memory_bytes,
            memory_limit_bytes,
            memory_percent,
            network_rx_bytes,
            network_tx_bytes,
            timestamp: chrono::Utc::now(),
        })
    }

    async fn list_containers(&self) -> Result<Vec<ContainerInfo>, DeployerError> {
        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                ..Default::default()
            }))
            .await
            .map_err(|e| DeployerError::Other(format!("Failed to list containers: {}", e)))?;

        let mut container_infos = Vec::new();

        for container in containers {
            if let Some(id) = container.id {
                match self.get_container_info(&id).await {
                    Ok(info) => container_infos.push(info),
                    Err(e) => warn!("Failed to get info for container {}: {}", id, e),
                }
            }
        }

        Ok(container_infos)
    }

    async fn get_container_logs(&self, container_id: &str) -> Result<String, DeployerError> {
        let logs_stream = self
            .docker
            .logs(
                container_id,
                Some(LogsOptions {
                    stdout: true,
                    stderr: true,
                    tail: "10000".to_string(),
                    ..Default::default()
                }),
            )
            .map(|chunk| chunk.map(|c| String::from_utf8_lossy(&c.into_bytes()).to_string()))
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| DeployerError::Other(format!("Failed to get logs: {}", e)))?;

        Ok(logs_stream.join(""))
    }

    async fn stream_container_logs(
        &self,
        container_id: &str,
    ) -> Result<Box<dyn futures::Stream<Item = String> + Unpin + Send>, DeployerError> {
        let logs_stream = self
            .docker
            .logs(
                container_id,
                Some(LogsOptions {
                    stdout: true,
                    stderr: true,
                    follow: true,
                    ..Default::default()
                }),
            )
            .map(|chunk| match chunk {
                Ok(c) => String::from_utf8_lossy(&c.into_bytes()).to_string(),
                Err(e) => format!("Error reading logs: {}", e),
            });

        Ok(Box::new(Box::pin(logs_stream)))
    }

    async fn image_exists(&self, image_name: &str) -> Result<bool, DeployerError> {
        match self.docker.inspect_image(image_name).await {
            Ok(_) => Ok(true),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(false),
            Err(e) => Err(DeployerError::Other(format!(
                "Failed to check image '{}': {}",
                image_name, e
            ))),
        }
    }
}

#[async_trait]
impl ContainerRuntime for DockerRuntime {
    async fn get_runtime_info(&self) -> Result<RuntimeInfo, DeployerError> {
        let version =
            self.docker.version().await.map_err(|e| {
                DeployerError::Other(format!("Failed to get Docker version: {}", e))
            })?;

        let mut system = System::new_all();
        system.refresh_all();

        Ok(RuntimeInfo {
            runtime_type: "Docker".to_string(),
            version: version.version.unwrap_or_default(),
            available_cpu_cores: num_cpus::get(),
            available_memory_mb: system.total_memory() / 1024,
            available_disk_mb: 0, // Docker doesn't easily expose this
        })
    }
}

/// Writes secrets as files into a per-container host directory for Docker
/// to bind-mount into `/run/secrets`. The directory is created (or recreated)
/// fresh on each call so stale entries from a previous deployment of the same
/// container name don't leak through. Directory mode 0700, file mode 0400.
///
/// When `owner` is provided, the directory and every file inside are
/// `chown`ed to that (uid, gid). Combined with 0700/0400 this means only
/// that UID inside the container can read the secrets — matching the
/// image's declared `USER` so distroless/nonroot images work without
/// world-readable files.
///
/// Rejects keys that would escape the directory (path separators, `.`, `..`)
/// to defend against a maliciously-crafted secret name.
fn write_secrets_to_host_dir(
    dir: &Path,
    secrets: &HashMap<String, String>,
    owner: Option<(u32, u32)>,
) -> std::io::Result<()> {
    use std::fs;
    use std::io::Write;

    // Recreate fresh — wipes any stale files from a previous container with
    // the same name (rolling deploys, retries after failure, etc.).
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }

    for (key, value) in secrets {
        // Keys must be plain identifiers — defense-in-depth even though
        // SecretService::validate_secret_key already enforces this.
        if key.is_empty()
            || key == "."
            || key == ".."
            || key.contains('/')
            || key.contains('\\')
            || key.contains('\0')
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "invalid secret key '{}': contains path-separator characters",
                    key
                ),
            ));
        }
        let path = dir.join(key);
        let mut f = fs::File::create(&path)?;
        f.write_all(value.as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o400))?;
        }
    }

    // Chown after files are written so we don't have to re-chown on every
    // write. Dir chown happens last so we still have permission to create
    // files in it while writing (when running as non-root, chown-to-self
    // is a no-op; when running as root we own it either way).
    //
    // chown(2) requires root on every sensible OS, or chown-to-self. When
    // Temps itself runs unprivileged (local macOS dev is the common case)
    // and the image runs as a non-root uid like 1000, the chown will fail
    // with EPERM. Rather than breaking the deploy, fall back to
    // world-readable permissions (0755 dir, 0444 files) so the container
    // can read its secrets. The container still has `cap_drop: ALL` and
    // `no-new-privileges`, and the bind mount is read-only — the trust
    // boundary is still the container itself.
    #[cfg(unix)]
    if let Some((uid, gid)) = owner {
        use std::os::unix::fs::{chown, PermissionsExt};

        let chown_result: std::io::Result<()> = (|| {
            for key in secrets.keys() {
                chown(dir.join(key), Some(uid), Some(gid))?;
            }
            chown(dir, Some(uid), Some(gid))?;
            Ok(())
        })();

        if let Err(e) = chown_result {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                warn!(
                    "chown of secrets dir {} to {}:{} denied (Temps is not root); \
                     falling back to world-readable mode so nonroot containers can read. \
                     Run Temps as root for strict per-uid ownership.",
                    dir.display(),
                    uid,
                    gid
                );
                // World-readable fallback. Parent dir already restricts
                // access on the host side (only the Temps user can traverse
                // into /tmp/temps-secrets); these bits are what the
                // container sees.
                fs::set_permissions(dir, fs::Permissions::from_mode(0o755))?;
                for key in secrets.keys() {
                    fs::set_permissions(dir.join(key), fs::Permissions::from_mode(0o444))?;
                }
            } else {
                return Err(e);
            }
        }
    }

    Ok(())
}

/// Parses a Docker image `USER` spec into a numeric `(uid, gid)` pair.
///
/// Handles the forms Docker accepts in a Dockerfile `USER` directive and
/// in `Config.User`: `""` (root), `"0"`, `"1000"`, `"1000:1000"`,
/// `"1000:gname"`, `":1000"`. Named users/groups cannot be resolved
/// without consulting the image's `/etc/passwd` — those return `None`
/// so the caller can decide whether to look them up or fall back.
///
/// When only a uid is given, gid defaults to the same value (matches
/// Docker's behavior: `USER 1000` runs as uid=1000, gid=1000).
fn parse_numeric_user_spec(spec: &str) -> Option<(u32, u32)> {
    let spec = spec.trim();
    if spec.is_empty() || spec == "root" {
        return Some((0, 0));
    }

    let (user_part, group_part) = match spec.split_once(':') {
        Some((u, g)) => (u, Some(g)),
        None => (spec, None),
    };

    let uid = if user_part.is_empty() {
        0
    } else {
        user_part.parse::<u32>().ok()?
    };

    let gid = match group_part {
        Some(g) if !g.is_empty() => g.parse::<u32>().ok()?,
        _ => uid,
    };

    Some((uid, gid))
}

#[cfg(test)]
mod docker_tests {
    use super::*;
    use crate::{
        BuildRequest, ContainerLogConfig, DeployRequest, PortMapping, Protocol, ResourceLimits,
        RestartPolicy,
    };
    use serial_test::serial;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use tokio::fs;
    use tokio::time::{timeout, Duration};

    async fn create_test_docker_runtime() -> Result<DockerRuntime, Box<dyn std::error::Error>> {
        let docker = Docker::connect_with_local_defaults()?;
        Ok(DockerRuntime::new(
            Arc::new(docker),
            false,
            "test-network".to_string(),
        ))
    }

    #[test]
    fn test_write_secrets_to_host_dir_creates_files_with_correct_perms() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("c1");

        let mut secrets = HashMap::new();
        secrets.insert("DB_PASSWORD".to_string(), "s3cret".to_string());
        secrets.insert("API_KEY".to_string(), "abc\ndef".to_string());

        write_secrets_to_host_dir(&dir, &secrets, None).expect("write");

        let db = std::fs::read_to_string(dir.join("DB_PASSWORD")).unwrap();
        assert_eq!(db, "s3cret");
        let api = std::fs::read_to_string(dir.join("API_KEY")).unwrap();
        assert_eq!(api, "abc\ndef");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o700);
            let file_mode = std::fs::metadata(dir.join("DB_PASSWORD"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(file_mode, 0o400);
        }
    }

    #[test]
    fn test_write_secrets_to_host_dir_overwrites_stale_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("c2");

        let mut first = HashMap::new();
        first.insert("OLD".to_string(), "old".to_string());
        write_secrets_to_host_dir(&dir, &first, None).unwrap();
        assert!(dir.join("OLD").exists());

        let mut second = HashMap::new();
        second.insert("NEW".to_string(), "new".to_string());
        write_secrets_to_host_dir(&dir, &second, None).unwrap();
        // Stale file from first call must be gone
        assert!(!dir.join("OLD").exists());
        assert!(dir.join("NEW").exists());
    }

    #[test]
    fn test_write_secrets_to_host_dir_rejects_path_traversal() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("c3");

        for bad in ["..", ".", "../escape", "a/b", "a\\b", "with\0null"] {
            let mut secrets = HashMap::new();
            secrets.insert(bad.to_string(), "v".to_string());
            let err = write_secrets_to_host_dir(&dir, &secrets, None).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "key={}", bad);
        }
    }

    #[test]
    fn test_write_secrets_to_host_dir_empty_creates_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("c4");
        let secrets: HashMap<String, String> = HashMap::new();
        write_secrets_to_host_dir(&dir, &secrets, None).unwrap();
        assert!(dir.exists());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[test]
    fn test_parse_numeric_user_spec() {
        // Empty / root → (0, 0)
        assert_eq!(parse_numeric_user_spec(""), Some((0, 0)));
        assert_eq!(parse_numeric_user_spec("  "), Some((0, 0)));
        assert_eq!(parse_numeric_user_spec("root"), Some((0, 0)));
        assert_eq!(parse_numeric_user_spec("0"), Some((0, 0)));

        // Uid only → gid defaults to uid (Docker's behavior)
        assert_eq!(parse_numeric_user_spec("1000"), Some((1000, 1000)));
        assert_eq!(parse_numeric_user_spec("65532"), Some((65532, 65532)));

        // uid:gid
        assert_eq!(parse_numeric_user_spec("1000:2000"), Some((1000, 2000)));
        assert_eq!(parse_numeric_user_spec("65532:65532"), Some((65532, 65532)));

        // :gid (uid defaults to 0 — matches Docker)
        assert_eq!(parse_numeric_user_spec(":1000"), Some((0, 1000)));

        // uid: (trailing colon → gid falls back to uid)
        assert_eq!(parse_numeric_user_spec("1000:"), Some((1000, 1000)));

        // Named users/groups are not numeric — caller falls back
        assert_eq!(parse_numeric_user_spec("node"), None);
        assert_eq!(parse_numeric_user_spec("node:node"), None);
        assert_eq!(parse_numeric_user_spec("1000:node"), None);
        assert_eq!(parse_numeric_user_spec("node:1000"), None);

        // Malformed
        assert_eq!(parse_numeric_user_spec("abc"), None);
        assert_eq!(parse_numeric_user_spec("-1"), None);
        assert_eq!(parse_numeric_user_spec("1:2:3"), None);
    }

    #[test]
    fn test_write_secrets_to_host_dir_chown_to_self_is_noop() {
        // chown(2) only succeeds when chown-ing to yourself (unless root).
        // We verify the happy path by chown-ing to our own uid/gid: the
        // function must succeed and leave the mode bits intact. This guards
        // against the chown being accidentally reordered before the file
        // writes (which would fail) or changing the permission bits.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            use std::os::unix::fs::PermissionsExt;

            let tmp = TempDir::new().unwrap();
            let dir = tmp.path().join("c5");

            let mut secrets = HashMap::new();
            secrets.insert("DB_PASSWORD".to_string(), "s3cret".to_string());

            let my_uid = std::fs::metadata(tmp.path()).unwrap().uid();
            let my_gid = std::fs::metadata(tmp.path()).unwrap().gid();

            write_secrets_to_host_dir(&dir, &secrets, Some((my_uid, my_gid))).unwrap();

            let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o700);
            let file_mode = std::fs::metadata(dir.join("DB_PASSWORD"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(file_mode, 0o400);
        }
    }

    #[test]
    fn test_write_secrets_to_host_dir_falls_back_to_world_readable_on_eperm() {
        // When Temps runs unprivileged and is asked to chown to a uid that
        // isn't its own, chown returns EPERM. The function must not fail
        // the deploy — it must downgrade permissions so the container can
        // still read its secrets. Only runs as non-root (skipped under sudo).
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            use std::os::unix::fs::PermissionsExt;

            let tmp = TempDir::new().unwrap();
            let my_uid = std::fs::metadata(tmp.path()).unwrap().uid();
            if my_uid == 0 {
                eprintln!("running as root; skipping EPERM fallback test");
                return;
            }

            let dir = tmp.path().join("c6");
            let mut secrets = HashMap::new();
            secrets.insert("DB_PASSWORD".to_string(), "s3cret".to_string());

            // Ask for a uid/gid we definitely don't own — forces EPERM.
            write_secrets_to_host_dir(&dir, &secrets, Some((65532, 65532)))
                .expect("fallback must succeed, not error");

            let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o755, "dir must be world-traversable on fallback");

            let file_mode = std::fs::metadata(dir.join("DB_PASSWORD"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(file_mode, 0o444, "file must be world-readable on fallback");

            // Content still intact.
            assert_eq!(
                std::fs::read_to_string(dir.join("DB_PASSWORD")).unwrap(),
                "s3cret"
            );
        }
    }

    #[tokio::test]
    async fn test_docker_runtime_creation() {
        match create_test_docker_runtime().await {
            Ok(runtime) => {
                assert_eq!(runtime.network_name, "test-network");
                assert!(!runtime.use_buildkit);
                println!("✅ Docker runtime created successfully");
            }
            Err(e) => {
                println!(
                    "🔧 Docker not available (expected in some test environments): {}",
                    e
                );
            }
        }
    }

    #[tokio::test]
    async fn test_overlay_network_default_unset() {
        match create_test_docker_runtime().await {
            Ok(runtime) => {
                assert!(
                    runtime.overlay_network.is_none(),
                    "overlay_network must default to None for backwards compatibility"
                );
            }
            Err(e) => {
                println!("🔧 Docker not available: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_with_overlay_network_sets_field() {
        match create_test_docker_runtime().await {
            Ok(runtime) => {
                let runtime = runtime.with_overlay_network("temps-overlay".to_string());
                assert_eq!(runtime.overlay_network.as_deref(), Some("temps-overlay"));
            }
            Err(e) => {
                println!("🔧 Docker not available: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_maybe_attach_overlay_noop_when_unset() {
        // When `overlay_network` is None, the function returns Ok(()) without
        // ever calling Docker. We can prove that by passing a runtime to a
        // bogus container_id — Docker would 404 if we tried to attach, but
        // we never call it.
        match create_test_docker_runtime().await {
            Ok(runtime) => {
                let r = runtime.maybe_attach_overlay("non-existent-container").await;
                assert!(r.is_ok(), "must be Ok(()) when overlay_network is None");
            }
            Err(e) => {
                println!("🔧 Docker not available: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_maybe_attach_overlay_skips_when_network_missing() {
        // When the overlay network does not exist on the host, the function
        // logs and returns Ok(()) — it does NOT propagate an error. This is
        // the "agent started before sync_loop bootstrapped the overlay"
        // case.
        match create_test_docker_runtime().await {
            Ok(runtime) => {
                let runtime =
                    runtime.with_overlay_network("temps-overlay-does-not-exist-xyz".to_string());
                let r = runtime.maybe_attach_overlay("non-existent-container").await;
                assert!(
                    r.is_ok(),
                    "must skip silently when overlay network missing, got {:?}",
                    r
                );
            }
            Err(e) => {
                println!("🔧 Docker not available: {}", e);
            }
        }
    }

    #[test]
    fn test_native_platform_detection() {
        let platform = DockerRuntime::detect_native_platform();

        // Verify platform format
        assert!(platform.starts_with("linux/"));

        // Verify it matches the current architecture
        #[cfg(target_arch = "x86_64")]
        {
            assert_eq!(platform, "linux/amd64");
            println!("✅ Detected x86_64 platform: {}", platform);
        }

        #[cfg(target_arch = "aarch64")]
        {
            assert_eq!(platform, "linux/arm64");
            println!("✅ Detected ARM64 platform: {}", platform);
        }

        // Verify platform is one of the supported architectures
        assert!(
            platform == "linux/amd64" || platform == "linux/arm64",
            "Platform should be either linux/amd64 or linux/arm64, got: {}",
            platform
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_docker_build_with_dockerfile() {
        let temp_dir = TempDir::new().unwrap();
        let context_path = temp_dir.path().to_path_buf();

        // Create a simple Dockerfile
        let dockerfile = r#"FROM alpine:latest
RUN echo "Hello from Docker test" > /hello.txt
CMD ["cat", "/hello.txt"]
"#;
        fs::write(context_path.join("Dockerfile"), dockerfile)
            .await
            .unwrap();

        match create_test_docker_runtime().await {
            Ok(runtime) => {
                let request = BuildRequest {
                    image_name: "docker-test:latest".to_string(),
                    context_path,
                    dockerfile_path: None,
                    build_args: HashMap::new(),
                    build_args_buildkit: HashMap::new(),
                    platform: None,
                    log_path: temp_dir.path().join("build.log"),
                };

                let result = timeout(Duration::from_secs(60), runtime.build_image(request)).await;

                match result {
                    Ok(Ok(build_result)) => {
                        println!("✅ Docker build succeeded: {}", build_result.image_name);
                        assert_eq!(build_result.image_name, "docker-test:latest");
                        assert!(build_result.build_duration_ms > 0);
                    }
                    Ok(Err(e)) => {
                        println!("🔧 Docker build failed (may be expected): {}", e);
                    }
                    Err(_) => {
                        println!("⏰ Docker build timed out");
                    }
                }
            }
            Err(e) => {
                println!("🔧 Docker not available: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_runtime_info() {
        match create_test_docker_runtime().await {
            Ok(runtime) => match runtime.get_runtime_info().await {
                Ok(info) => {
                    println!("✅ Runtime info retrieved:");
                    println!("  Type: {}", info.runtime_type);
                    println!("  Version: {}", info.version);
                    println!("  CPU cores: {}", info.available_cpu_cores);
                    println!("  Memory: {} MB", info.available_memory_mb);

                    assert_eq!(info.runtime_type, "Docker");
                    assert!(info.available_cpu_cores > 0);
                    assert!(info.available_memory_mb > 0);
                }
                Err(e) => {
                    println!("🔧 Failed to get runtime info: {}", e);
                }
            },
            Err(e) => {
                println!("🔧 Docker not available: {}", e);
            }
        }
    }

    #[tokio::test]
    #[serial]
    async fn test_container_lifecycle() {
        match create_test_docker_runtime().await {
            Ok(runtime) => {
                // First ensure we have an image to work with (alpine is usually available)
                let deploy_request = DeployRequest {
                    image_name: "alpine:latest".to_string(),
                    container_name: "lifecycle-test".to_string(),
                    environment_vars: {
                        let mut env = HashMap::new();
                        env.insert("TEST_VAR".to_string(), "test_value".to_string());
                        env
                    },
                    secrets: HashMap::new(),
                    port_mappings: vec![],
                    network_name: None,
                    resource_limits: ResourceLimits {
                        cpu_limit: Some(0.5),
                        memory_limit_mb: Some(64),
                        disk_limit_mb: Some(256),
                    },
                    restart_policy: RestartPolicy::Never,
                    log_path: PathBuf::from("/tmp/lifecycle-test.log"),
                    command: Some(vec!["sleep".to_string(), "30".to_string()]),
                    log_config: Some(ContainerLogConfig::app_default()),
                    labels: HashMap::new(),
                };

                let deploy_result = runtime.deploy_container(deploy_request).await;

                match deploy_result {
                    Ok(deploy_info) => {
                        println!("✅ Container deployed: {}", deploy_info.container_name);

                        // Test container operations
                        let container_id = &deploy_info.container_id;

                        // Test getting container info
                        if let Ok(info) = runtime.get_container_info(container_id).await {
                            println!("📋 Container info: {:?}", info.status);
                        }

                        // Test pause/resume
                        if let Ok(()) = runtime.pause_container(container_id).await {
                            println!("⏸️  Container paused");
                        }

                        if let Ok(()) = runtime.resume_container(container_id).await {
                            println!("▶️  Container resumed");
                        }

                        // Test stop
                        if let Ok(()) = runtime.stop_container(container_id).await {
                            println!("⏹️  Container stopped");
                        }

                        // Test remove
                        if let Ok(()) = runtime.remove_container(container_id).await {
                            println!("🗑️  Container removed");
                        }

                        println!("✅ Container lifecycle test completed");
                    }
                    Err(e) => {
                        println!("🔧 Container deployment failed (may be expected): {}", e);
                    }
                }
            }
            Err(e) => {
                println!("🔧 Docker not available: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_image_operations() {
        match create_test_docker_runtime().await {
            Ok(runtime) => {
                // Test list images
                match runtime.list_images().await {
                    Ok(images) => {
                        println!("✅ Found {} images", images.len());
                        // Don't assert specific count as it depends on system state
                    }
                    Err(e) => {
                        println!("🔧 Failed to list images: {}", e);
                    }
                }

                // Test remove non-existent image
                match runtime
                    .remove_image("definitely-does-not-exist:latest")
                    .await
                {
                    Ok(()) => println!("⚠️ Unexpectedly succeeded removing non-existent image"),
                    Err(e) => println!("✅ Correctly failed to remove non-existent image: {}", e),
                }
            }
            Err(e) => {
                println!("🔧 Docker not available: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_list_containers() {
        match create_test_docker_runtime().await {
            Ok(runtime) => match runtime.list_containers().await {
                Ok(containers) => {
                    println!("✅ Found {} containers", containers.len());
                    for container in containers.iter().take(3) {
                        println!("  📦 {}: {:?}", container.container_name, container.status);
                    }
                }
                Err(e) => {
                    println!("🔧 Failed to list containers: {}", e);
                }
            },
            Err(e) => {
                println!("🔧 Docker not available: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_network_operations() {
        match create_test_docker_runtime().await {
            Ok(runtime) => {
                // Test network existence (this will try to create if not exists)
                match runtime.ensure_network_exists().await {
                    Ok(()) => {
                        println!("✅ Network operations test passed");
                    }
                    Err(e) => {
                        println!("🔧 Network operations failed: {}", e);
                    }
                }
            }
            Err(e) => {
                println!("🔧 Docker not available: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_resource_limits_validation() {
        let resource_limits = ResourceLimits {
            cpu_limit: Some(2.0),
            memory_limit_mb: Some(512),
            disk_limit_mb: Some(1024),
        };

        // Test that resource limits are properly structured
        assert!(resource_limits.cpu_limit.unwrap() > 0.0);
        assert!(resource_limits.memory_limit_mb.unwrap() > 0);
        assert!(resource_limits.disk_limit_mb.unwrap() > 0);

        println!("✅ Resource limits validation passed");
    }

    #[tokio::test]
    async fn test_port_mapping_validation() {
        let port_mapping = PortMapping {
            host_port: 8080,
            container_port: 80,
            protocol: Protocol::Tcp,
        };

        assert_eq!(port_mapping.host_port, 8080);
        assert_eq!(port_mapping.container_port, 80);
        assert!(matches!(port_mapping.protocol, Protocol::Tcp));

        println!("✅ Port mapping validation passed");
    }

    #[tokio::test]
    async fn test_restart_policy_enum() {
        let policies = [
            RestartPolicy::Never,
            RestartPolicy::Always,
            RestartPolicy::OnFailure,
            RestartPolicy::UnlessStopped,
        ];

        // Just test that all enum variants exist and can be created
        assert_eq!(policies.len(), 4);
        println!("✅ Restart policy enum validation passed");
    }

    #[tokio::test]
    async fn test_error_types() {
        // Test that error types can be created and match properly
        let build_error = BuilderError::BuildFailed("test error".to_string());
        let deploy_error = DeployerError::DeploymentFailed("test deploy error".to_string());

        assert!(matches!(build_error, BuilderError::BuildFailed(_)));
        assert!(matches!(deploy_error, DeployerError::DeploymentFailed(_)));

        println!("✅ Error types validation passed");
    }
}
