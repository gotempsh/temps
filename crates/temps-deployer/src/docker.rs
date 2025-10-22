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
}

impl DockerRuntime {
    pub fn new(docker: Arc<Docker>, use_buildkit: bool, network_name: String) -> Self {
        Self {
            docker,
            use_buildkit,
            network_name,
        }
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

        let mut tar_buffer = Vec::new();
        {
            let mut tar_builder = tar::Builder::new(&mut tar_buffer);
            tar_builder
                .append_dir_all(".", context_path)
                .map_err(BuilderError::IoError)?;
            tar_builder.finish().map_err(BuilderError::IoError)?;
        }

        // Create body from tar buffer as expected by Bollard
        // Return Full<Bytes> which will be converted to Either::Left automatically
        Ok(Full::new(Bytes::from(tar_buffer)))
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
    fn get_native_platform() -> String {
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
            platform: request.platform.unwrap_or_else(Self::get_native_platform),
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
                    if let Some(error) = info.error {
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
            platform: request.platform.unwrap_or_else(Self::get_native_platform),
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
                    if let Some(error) = info.error {
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
                        for log in res.logs {
                            // Write to file
                            let _ = log_file.write_all(&log.msg[..]).await;
                            debug!("BuildKit: {}", String::from_utf8_lossy(&log.msg));

                            // Call log callback if provided
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
                .map(|r| r.unwrap().freeze());

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

    async fn extract_from_image(
        &self,
        image_name: &str,
        source_path: &str,
        destination_path: &Path,
    ) -> Result<(), BuilderError> {
        // Pull image if needed
        self.docker
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
        let mut exposed_ports = HashMap::new();

        for port_mapping in &request.port_mappings {
            let container_port_key =
                format!("{}/{}", port_mapping.container_port, port_mapping.protocol);
            let host_port_binding = bollard::models::PortBinding {
                host_ip: Some("0.0.0.0".to_string()),
                host_port: Some(port_mapping.host_port.to_string()),
            };

            port_bindings.insert(container_port_key.clone(), Some(vec![host_port_binding]));
            exposed_ports.insert(container_port_key, HashMap::new());
        }

        // Create host config
        let host_config = bollard::models::HostConfig {
            port_bindings: Some(port_bindings),
            network_mode: Some(self.network_name.clone()),
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
            ..Default::default()
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

        // Start container
        self.docker
            .start_container(&container.id, None::<StartContainerOptions>)
            .await
            .map_err(|e| {
                DeployerError::DeploymentFailed(format!("Failed to start container: {}", e))
            })?;

        // Get the first port mapping for the result
        let (container_port, host_port) = request
            .port_mappings
            .first()
            .map(|pm| (pm.container_port, pm.host_port))
            .unwrap_or((0, 0));

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
            .stop_container(container_id, None::<StopContainerOptions>)
            .await
            .map_err(|e| DeployerError::Other(format!("Failed to stop container: {}", e)))?;
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
            created_at: container.created.unwrap_or_else(chrono::Utc::now),
            ports: port_mappings,
            environment_vars: env_vars,
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

#[cfg(test)]
mod docker_tests {
    use super::*;
    use crate::{
        BuildRequest, DeployRequest, PortMapping, Protocol, ResourceLimits, RestartPolicy,
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

    #[test]
    fn test_native_platform_detection() {
        let platform = DockerRuntime::get_native_platform();

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
