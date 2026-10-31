// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The `ImageBuilder` impl of [`DockerRuntime`] and the build-context packing
//! it relies on.

use super::build_failure::build_stream_error_text;
use super::build_limits::describe_build_memory_request;
use super::guards::{combine_extraction_and_cleanup, DockerContainerCleanupGuard};
use super::static_archive::{
    static_archive_root, unpack_static_archive_stream, MAX_STATIC_ARCHIVE_STREAM_BYTES,
};
use super::DockerRuntime;
use crate::build_timings::BuildStepTimer;
use crate::{BuildRequest, BuildResult, BuilderError, ImageBuilder, ImageImportStream};
use async_trait::async_trait;
use bollard::{
    query_parameters::{BuilderVersion, TagImageOptions},
    Docker,
};
use futures::StreamExt;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tracing::{debug, error, info};

/// Write the build context under `root` into `archive` the way the Docker
/// CLI sends it: paths the Dockerfile's ignore rules exclude are left out,
/// symlinks are archived as links, and every entry is owned by root.
///
/// The CLI rewrites every context entry to uid/gid 0 before uploading it.
/// Without that, the archive carries whatever user owns the checkout on this
/// host, and BuildKit keeps those ids when it unpacks a tarball context, so a
/// `COPY . .` lands files owned by an unrelated uid in the build stage. Git
/// then refuses to touch the copied `.git` ("detected dubious ownership"):
/// `go build` fails with `error obtaining VCS status: exit status 128`, and a
/// `git describe` / `git rev-parse` in a Makefile fails the same way.
///
/// `.git` follows the ignore rules like any other path, as it does with the
/// CLI: builds that read VCS metadata need it, and a `.dockerignore` entry
/// keeps it out of builds that don't. File modes and modification times are
/// kept, and entries are written in name order so the archive does not
/// depend on directory iteration order.
fn append_local_build_context<W: Write>(
    archive: &mut tar::Builder<W>,
    root: &Path,
    relative: &Path,
    filter: &crate::remote::ContextFilter,
) -> Result<(), BuilderError> {
    let mut entries = std::fs::read_dir(root.join(relative))
        .map_err(BuilderError::IoError)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(BuilderError::IoError)?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = relative.join(entry.file_name());
        let file_type = entry.file_type().map_err(BuilderError::IoError)?;
        let included = filter.includes(&path);
        if file_type.is_dir() {
            if included {
                append_root_owned_entry(archive, &entry.path(), &path)?;
            }
            if included || filter.must_descend(&path) {
                append_local_build_context(archive, root, &path, filter)?;
            }
        } else if included {
            if !file_type.is_file() && !file_type.is_symlink() {
                return Err(BuilderError::InvalidContext(format!(
                    "Unsupported build context entry '{}'",
                    path.display()
                )));
            }
            append_root_owned_entry(archive, &entry.path(), &path)?;
        }
    }
    Ok(())
}

/// Append one context entry as `name`, owned by uid/gid 0. A symlink is
/// archived as a link, so nothing outside the checkout is read through it.
fn append_root_owned_entry<W: Write>(
    archive: &mut tar::Builder<W>,
    source: &Path,
    name: &Path,
) -> Result<(), BuilderError> {
    let metadata = std::fs::symlink_metadata(source).map_err(BuilderError::IoError)?;
    let mut header = tar::Header::new_gnu();
    header.set_metadata_in_mode(&metadata, tar::HeaderMode::Complete);
    header.set_uid(0);
    header.set_gid(0);
    if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(source).map_err(BuilderError::IoError)?;
        header.set_size(0);
        archive
            .append_link(&mut header, name, target)
            .map_err(BuilderError::IoError)
    } else if metadata.is_dir() {
        archive
            .append_data(&mut header, name, std::io::empty())
            .map_err(BuilderError::IoError)
    } else {
        let file = std::fs::File::open(source).map_err(BuilderError::IoError)?;
        archive
            .append_data(&mut header, name, file)
            .map_err(BuilderError::IoError)
    }
}

fn docker_build_args(request: &BuildRequest, use_buildkit: bool) -> HashMap<String, String> {
    let mut args = request.build_args.clone();
    if use_buildkit {
        args.extend(request.build_args_buildkit.clone());
        // Make images usable as cache_from seeds on another builder. Inline
        // metadata travels with the image; cache mounts remain local.
        args.entry("BUILDKIT_INLINE_CACHE".to_string())
            .or_insert_with(|| "1".to_string());
    }
    args.retain(|_, value| !value.is_empty());
    args
}

fn split_repository_and_tag(image: &str) -> (&str, &str) {
    match image.rsplit_once(':') {
        Some((repository, image_tag)) if !image_tag.contains('/') => (repository, image_tag),
        _ => (image, "latest"),
    }
}

async fn import_stream_into_docker(
    docker: &Docker,
    image_stream: ImageImportStream,
    tag: &str,
) -> Result<String, BuilderError> {
    let import_stream = docker.import_image_stream(
        bollard::query_parameters::ImportImageOptions {
            quiet: false,
            ..Default::default()
        },
        image_stream,
        None,
    );

    let mut image_id = None;
    let mut stream = std::pin::Pin::new(Box::new(import_stream));

    while let Some(result) = futures::StreamExt::next(&mut stream).await {
        match result {
            Ok(info) => {
                if let Some(stream_msg) = info.stream {
                    info!(message = %stream_msg.trim(), "Docker image import progress");
                    if stream_msg.contains("Loaded image:") {
                        image_id = stream_msg
                            .split("Loaded image: ")
                            .nth(1)
                            .map(|value| value.trim().to_string());
                    }
                }
            }
            Err(error) => {
                return Err(BuilderError::Other(format!(
                    "Failed to import image '{tag}' into Docker: {error}"
                )));
            }
        }
    }

    let image_id = image_id.ok_or_else(|| {
        BuilderError::Other(format!(
            "Docker completed the import for image '{tag}' without reporting an image ID"
        ))
    })?;

    let (repository, image_tag) = split_repository_and_tag(tag);

    docker
        .tag_image(
            &image_id,
            Some(TagImageOptions {
                repo: Some(repository.to_string()),
                tag: Some(image_tag.to_string()),
            }),
        )
        .await
        .map_err(|error| {
            BuilderError::Other(format!(
                "Failed to tag imported Docker image '{tag}' from ID '{image_id}': {error}"
            ))
        })?;

    Ok(image_id)
}

/// Stream `file` in fixed-size chunks, keeping `dir` (which contains it)
/// alive until the stream ends or is dropped, so the staged archive is
/// deleted exactly when the transfer finishes.
fn stream_file_owning_dir(file: tokio::fs::File, dir: TempDir) -> ImageImportStream {
    use tokio::io::AsyncReadExt;
    const CHUNK_BYTES: usize = 64 * 1024;
    Box::pin(futures::stream::unfold(
        Some((file, dir)),
        |state| async move {
            let (mut file, dir) = state?;
            let mut buffer = vec![0_u8; CHUNK_BYTES];
            match file.read(&mut buffer).await {
                Ok(0) => None,
                Ok(read) => {
                    buffer.truncate(read);
                    Some((Ok(bytes::Bytes::from(buffer)), Some((file, dir))))
                }
                Err(error) => Some((Err(error), None)),
            }
        },
    ))
}

impl DockerRuntime {
    async fn create_tar_context_body(
        &self,
        context_path: PathBuf,
        dockerfile_path: Option<PathBuf>,
    ) -> Result<ImageImportStream, BuilderError> {
        let tmp = tempfile::NamedTempFile::new().map_err(BuilderError::IoError)?;
        // Keep ownership in the blocking task; cancellation must not leave
        // an orphan archive. Read back in bounded chunks rather than a Vec.
        let tmp = tokio::task::spawn_blocking(move || {
            let dockerfile = dockerfile_path
                .as_deref()
                .and_then(|path| path.strip_prefix(&context_path).ok())
                .unwrap_or_else(|| Path::new("Dockerfile"));
            let filter = crate::remote::load_context_filter(&context_path, dockerfile)?;
            let file = tmp.reopen().map_err(BuilderError::IoError)?;
            let mut archive = tar::Builder::new(file);
            append_local_build_context(&mut archive, &context_path, Path::new(""), &filter)?;
            archive.finish().map_err(BuilderError::IoError)?;
            Ok::<_, BuilderError>(tmp)
        })
        .await
        .map_err(|error| {
            BuilderError::Other(format!("Build context tar task failed: {error}"))
        })??;
        let file = tokio::fs::File::from_std(tmp.reopen().map_err(BuilderError::IoError)?);
        Ok(Box::pin(
            tokio_util::codec::FramedRead::new(file, tokio_util::codec::BytesCodec::new()).map(
                move |result| {
                    let _owner = &tmp;
                    result.map(|chunk| chunk.freeze())
                },
            ),
        ))
    }

    /// Detect the native platform for Docker builds.
    ///
    /// Prefers the daemon platform learned by [`Self::refresh_daemon_platform`]
    /// and falls back to this binary's architecture until that succeeds. The
    /// fallback is never cached, so a daemon that comes up late (or recovers)
    /// is picked up by the next refresh — see the build paths, which refresh
    /// before using this.
    fn detect_native_platform(&self) -> String {
        self.daemon_platform
            .get()
            .cloned()
            .unwrap_or_else(crate::platform::native_platform)
    }

    /// Create (never start) a container from `image_name` so its filesystem
    /// can be read through the container-archive API. The returned guard
    /// removes it, including when the caller is cancelled.
    async fn create_extraction_container(
        docker: &Arc<Docker>,
        image_name: &str,
        source_path: &str,
    ) -> Result<DockerContainerCleanupGuard, BuilderError> {
        let container_config = bollard::models::ContainerCreateBody {
            image: Some(image_name.to_string()),
            cmd: Some(vec!["/bin/sh".to_string()]),
            tty: Some(true),
            ..Default::default()
        };
        let container = docker
            .create_container(
                Some(bollard::query_parameters::CreateContainerOptionsBuilder::new().build()),
                container_config,
            )
            .await
            .map_err(|error| {
                BuilderError::Other(format!(
                    "Failed to create container from image '{image_name}' to read path '{source_path}': {error}"
                ))
            })?;
        Ok(DockerContainerCleanupGuard::new(
            docker.clone(),
            container.id,
            image_name,
            source_path,
        ))
    }
}

#[async_trait]
impl ImageBuilder for DockerRuntime {
    async fn fetch_registry_image(
        &self,
        reference: &str,
        tag: &str,
        credentials: Option<crate::remote::RemotePullCredentials>,
    ) -> Result<(), BuilderError> {
        let docker = self.docker.require()?;
        let auth = credentials.map(|c| bollard::auth::DockerCredentials {
            username: c.username,
            password: c.password,
            identitytoken: c.identity_token,
            serveraddress: c.server_address,
            ..Default::default()
        });
        let mut stream = docker.create_image(
            Some(bollard::query_parameters::CreateImageOptions {
                from_image: Some(reference.into()),
                ..Default::default()
            }),
            None,
            auth,
        );
        while let Some(event) = stream.next().await {
            let event = event.map_err(|error| {
                BuilderError::Other(format!("Pull of '{reference}' failed: {error}"))
            })?;
            if event.error_detail.is_some() {
                return Err(BuilderError::Other(format!(
                    "Registry refused pull of '{reference}'"
                )));
            }
        }
        let (repo, image_tag) = split_repository_and_tag(tag);
        docker
            .tag_image(
                reference,
                Some(TagImageOptions {
                    repo: Some(repo.into()),
                    tag: Some(image_tag.into()),
                }),
            )
            .await
            .map_err(|error| {
                BuilderError::Other(format!(
                    "Cannot tag registry image '{reference}' as '{tag}': {error}"
                ))
            })?;
        Ok(())
    }

    async fn build_image(&self, request: BuildRequest) -> Result<BuildResult, BuilderError> {
        // Cheap when already known (a `OnceLock` read); one `docker info` when
        // discovery failed earlier. Doing it here means a daemon that wasn't up
        // at boot — or a `DOCKER_HOST` pointing at another architecture — is
        // reflected in the platform this build defaults to.
        let _ = self.refresh_daemon_platform().await;

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

        // Gate on the build concurrency semaphore when one is configured.
        // The permit is held for the entire build duration — dropped at end
        // of function via `_build_permit` so the next queued build can
        // start as soon as this one finishes (success OR failure). When no
        // semaphore is set (worker nodes, tests), builds run unbounded as
        // before.
        let _build_permit = if let Some(gate) = self.build_limits.gate() {
            if gate.is_full() {
                info!(
                    "Build for {} queued: all build slots in use, waiting for a slot",
                    request.image_name
                );
            }
            Some(gate.acquire().await)
        } else {
            None
        };

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
            .create_tar_context_body(
                request.context_path.clone(),
                request.dockerfile_path.clone(),
            )
            .await?;

        // Prepare build options using Bollard
        let build_args = docker_build_args(&request, self.use_buildkit);

        // Resolve effective build caps from settings (or fall back to the
        // legacy 50%-of-host heuristic when no override is set). The memory
        // value is reduced to what the API accepts, with a warning, in
        // `effective_build_memory`.
        let configured_caps = self.build_limits.resource_caps();
        let (memory_bytes, cpu_quota_us, cpu_period_us) =
            Self::resolve_build_resource_caps_for(configured_caps);
        let memory_i32 = self.effective_build_memory(
            memory_bytes,
            configured_caps.is_some(),
            &request.image_name,
        );
        info!(
            "{}",
            describe_build_memory_request(&request.image_name, memory_i32, self.use_buildkit)
        );

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
            cachefrom: Some(request.cache_from.clone()),
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
                .clone()
                .unwrap_or_else(|| self.detect_native_platform()),
            memory: Some(memory_i32),
            cpuquota: Some(cpu_quota_us),
            cpuperiod: Some(cpu_period_us),
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

        let docker = self.require_docker_for_build()?;
        let build_start = self.sample_build_start();
        let mut build_stream = docker.build_image(
            build_options,
            None,
            Some(bollard::body_try_stream(tar_body)),
        );

        // Stream build output and write to log
        let mut step_timer = BuildStepTimer::new();
        let mut build_error = None;
        while let Some(build_info) = build_stream.next().await {
            match build_info {
                Ok(info) => {
                    if let Some(stream) = info.stream {
                        let _ = log_file.write_all(stream.as_bytes()).await;
                        debug!("Build: {}", stream.trim());
                    }
                    if let Some(bollard::models::BuildInfoAux::BuildKit(ref res)) = info.aux {
                        step_timer.observe_vertices(&res.vertexes);
                    }
                    if let Some(error_detail) = info.error_detail {
                        let error = error_detail
                            .message
                            .unwrap_or_else(|| "Unknown build error".to_string());
                        error!("Build error: {}", error);
                        let _ = log_file
                            .write_all(format!("ERROR: {}\n", error).as_bytes())
                            .await;
                        let (err, memory_line) =
                            self.classify_build_failure(error, &build_start, i64::from(memory_i32));
                        if let Some(line) = memory_line {
                            let _ = log_file.write_all(line.as_bytes()).await;
                        }
                        build_error = Some(err);
                        break;
                    }
                }
                Err(e) => {
                    // `BuilderError::BuildFailed` adds the "Build failed:"
                    // prefix itself; passing it in too doubled it.
                    let error_text = build_stream_error_text(&e);
                    error!("Build failed: {}", error_text);
                    let _ = log_file
                        .write_all(format!("ERROR: Build failed: {}\n", error_text).as_bytes())
                        .await;
                    let (err, memory_line) = self.classify_build_failure(
                        error_text,
                        &build_start,
                        i64::from(memory_i32),
                    );
                    if let Some(line) = memory_line {
                        let _ = log_file.write_all(line.as_bytes()).await;
                    }
                    build_error = Some(err);
                    break;
                }
            }
        }

        if let Some(summary) = step_timer.summary(start_time.elapsed()) {
            info!(image = %request.image_name, "Build step timings\n{}", summary.trim_end());
            let _ = log_file.write_all(summary.as_bytes()).await;
        }
        let _ = log_file.flush().await;

        if let Some(error) = build_error {
            return Err(error);
        }

        let build_duration = start_time.elapsed().as_millis() as u64;

        // Get image info for size
        let images = docker
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
        // See `build_image`: refresh before defaulting to a platform.
        let _ = self.refresh_daemon_platform().await;

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

        // Gate on the build concurrency semaphore (see build_image above for
        // the rationale). This is the path the workflow executor actually
        // uses, so the semaphore would be ineffective without this branch.
        let _build_permit = if let Some(gate) = self.build_limits.gate() {
            if gate.is_full() {
                info!(
                    "Build for {} queued: all build slots in use, waiting for a slot",
                    request.image_name
                );
                if let Some(ref cb) = log_callback {
                    cb("[BUILD QUEUED] Waiting for an available build slot...".to_string()).await;
                }
            }
            Some(gate.acquire().await)
        } else {
            None
        };

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
            .create_tar_context_body(
                request.context_path.clone(),
                request.dockerfile_path.clone(),
            )
            .await?;

        // Prepare build options using Bollard
        let build_args = docker_build_args(&request, self.use_buildkit);

        let configured_caps = self.build_limits.resource_caps();
        let (memory_bytes, cpu_quota_us, cpu_period_us) =
            Self::resolve_build_resource_caps_for(configured_caps);
        let memory_i32 = self.effective_build_memory(
            memory_bytes,
            configured_caps.is_some(),
            &request.image_name,
        );
        info!(
            "{}",
            describe_build_memory_request(&request.image_name, memory_i32, self.use_buildkit)
        );

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
            cachefrom: Some(request.cache_from.clone()),
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
                .clone()
                .unwrap_or_else(|| self.detect_native_platform()),
            memory: Some(memory_i32),
            cpuquota: Some(cpu_quota_us),
            cpuperiod: Some(cpu_period_us),
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
        let docker = self.require_docker_for_build()?;
        let build_start = self.sample_build_start();
        let mut build_stream = docker.build_image(
            build_options,
            None,
            Some(bollard::body_try_stream(tar_body)),
        );

        // Stream build output and write to log and callback
        let mut step_timer = BuildStepTimer::new();
        let mut build_error = None;
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
                    if let Some(bollard::models::BuildInfoAux::BuildKit(ref res)) = info.aux {
                        step_timer.observe_vertices(&res.vertexes);
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

                        let (err, memory_line) =
                            self.classify_build_failure(error, &build_start, i64::from(memory_i32));
                        if let Some(line) = memory_line {
                            let _ = log_file.write_all(line.as_bytes()).await;
                            if let Some(ref callback) = log_callback {
                                callback(line).await;
                            }
                        }
                        build_error = Some(err);
                        break;
                    }
                    if let Some(bollard::models::BuildInfoAux::BuildKit(res)) = info.aux {
                        step_timer.observe_vertices(&res.vertexes);
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
                    // `BuilderError::BuildFailed` adds the "Build failed:"
                    // prefix itself; passing it in too doubled it.
                    let error_text = build_stream_error_text(&e);
                    error!("Build failed: {}", error_text);
                    let error_line = format!("ERROR: Build failed: {}\n", error_text);
                    let _ = log_file.write_all(error_line.as_bytes()).await;

                    // Call log callback with error
                    if let Some(ref callback) = log_callback {
                        callback(error_line).await;
                    }

                    let (err, memory_line) = self.classify_build_failure(
                        error_text,
                        &build_start,
                        i64::from(memory_i32),
                    );
                    if let Some(line) = memory_line {
                        let _ = log_file.write_all(line.as_bytes()).await;
                        if let Some(ref callback) = log_callback {
                            callback(line).await;
                        }
                    }
                    build_error = Some(err);
                    break;
                }
            }
        }

        if let Some(summary) = step_timer.summary(start_time.elapsed()) {
            info!(image = %request.image_name, "Build step timings\n{}", summary.trim_end());
            let _ = log_file.write_all(summary.as_bytes()).await;
            if let Some(ref callback) = log_callback {
                // One deployment log entry per line, like the build output.
                for line in summary.lines() {
                    callback(format!("{line}\n")).await;
                }
            }
        }
        let _ = log_file.flush().await;

        if let Some(error) = build_error {
            return Err(error);
        }

        let build_duration = start_time.elapsed().as_millis() as u64;

        // Get image info
        let images = docker
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

        let byte_stream: ImageImportStream = Box::pin(
            tokio_util::codec::FramedRead::new(file, tokio_util::codec::BytesCodec::new())
                .map(|result| result.map(|bytes| bytes.freeze())),
        );

        let docker = self.require_docker_for_build()?;
        import_stream_into_docker(&docker, byte_stream, tag).await
    }

    async fn import_image_stream(
        &self,
        image_stream: ImageImportStream,
        tag: &str,
    ) -> Result<String, BuilderError> {
        info!(image = %tag, "Importing streamed image into Docker");
        let docker = self.require_docker_for_build()?;
        import_stream_into_docker(&docker, image_stream, tag).await
    }

    async fn export_image_stream(
        &self,
        image_name: &str,
    ) -> Result<crate::ImageImportStream, BuilderError> {
        info!(image = %image_name, "Streaming image export from Docker");
        let docker = self.require_docker_for_build()?;
        // Fail with a typed not-found before a 200 is sent: `export_image`
        // only reports a missing image once the stream is polled.
        match docker.inspect_image(image_name).await {
            Ok(_) => {}
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => return Err(BuilderError::ImageNotFound(image_name.to_string())),
            Err(error) => {
                return Err(BuilderError::Other(format!(
                    "Failed to inspect image '{image_name}' before export: {error}"
                )))
            }
        }
        let name = image_name.to_string();
        Ok(Box::pin(docker.export_image(image_name).map(
            move |chunk| {
                chunk.map_err(|error| {
                    std::io::Error::other(format!("Failed to export image '{name}': {error}"))
                })
            },
        )))
    }

    async fn save_image(&self, image_name: &str, output_path: &Path) -> Result<(), BuilderError> {
        info!("Exporting image '{}' to {:?}", image_name, output_path);

        let docker = self.require_docker_for_build()?;
        let stream = docker.export_image(image_name);

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
        let docker = self.require_docker_for_build()?;
        // Reject a path with no final component before touching the daemon.
        static_archive_root(source_path, image_name)?;

        // Skip pull for local images (temps-* are built locally, not from a registry)
        if !image_name.starts_with("temps-") {
            let _ = docker
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

        let mut cleanup_guard =
            Self::create_extraction_container(&docker, image_name, source_path).await?;
        let container_id = cleanup_guard.container_id.clone();

        let response_stream = docker.download_from_container(
            &container_id,
            Some(bollard::query_parameters::DownloadFromContainerOptions {
                path: source_path.to_string(),
            }),
        );
        let operation = unpack_static_archive_stream(
            response_stream,
            image_name,
            source_path,
            destination_path,
        )
        .await;
        let cleanup = cleanup_guard.cleanup().await;
        combine_extraction_and_cleanup(operation, cleanup, &container_id, image_name, source_path)
    }

    /// Stream `source_path` out of a locally present image as the raw
    /// container-archive tar, for a control plane that unpacks it itself.
    ///
    /// Never pulls: the agent serves this for images it built, and a request
    /// naming anything else is answered with `ImageNotFound` rather than
    /// fetching an arbitrary reference onto the node. The archive is staged in
    /// an owner-only temp directory under the same byte bound as local
    /// extraction, so the temporary container is removed before the first
    /// byte is sent and a slow reader cannot keep it alive.
    async fn export_path_stream(
        &self,
        image_name: &str,
        source_path: &str,
    ) -> Result<ImageImportStream, BuilderError> {
        let docker = self.require_docker_for_build()?;
        static_archive_root(source_path, image_name)?;
        match docker.inspect_image(image_name).await {
            Ok(_) => {}
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {
                return Err(BuilderError::ImageNotFound(format!(
                    "'{image_name}' is not present on this node"
                )))
            }
            Err(error) => {
                return Err(BuilderError::Other(format!(
                    "Cannot inspect image '{image_name}' before exporting path '{source_path}': {error}"
                )))
            }
        }

        let temp_dir = TempDir::new().map_err(|error| {
            BuilderError::IoError(std::io::Error::new(
                error.kind(),
                format!(
                    "Failed to create temporary directory to export path '{source_path}' from image '{image_name}': {error}"
                ),
            ))
        })?;
        let archive_path = temp_dir.path().join("path-export.tar");

        let mut cleanup_guard =
            Self::create_extraction_container(&docker, image_name, source_path).await?;
        let container_id = cleanup_guard.container_id.clone();
        let download = docker.download_from_container(
            &container_id,
            Some(bollard::query_parameters::DownloadFromContainerOptions {
                path: source_path.to_string(),
            }),
        );
        let operation = Self::write_bounded_byte_stream(
            download,
            &archive_path,
            MAX_STATIC_ARCHIVE_STREAM_BYTES,
            image_name,
            source_path,
        )
        .await
        .map(|_| ());
        let cleanup = cleanup_guard.cleanup().await;
        combine_extraction_and_cleanup(operation, cleanup, &container_id, image_name, source_path)?;

        let file = tokio::fs::File::open(&archive_path).await.map_err(|error| {
            BuilderError::IoError(std::io::Error::new(
                error.kind(),
                format!(
                    "Failed to reopen exported archive {} for image '{image_name}' path '{source_path}': {error}",
                    archive_path.display()
                ),
            ))
        })?;
        Ok(stream_file_owning_dir(file, temp_dir))
    }

    async fn list_images(&self) -> Result<Vec<String>, BuilderError> {
        let images = self
            .require_docker_for_build()?
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
        // The returned future used to be bound to `_stream` and dropped
        // without ever being awaited, so nothing was sent to the daemon and
        // every caller got a silent `Ok(())` while the image stayed put.
        let deleted = self
            .require_docker_for_build()?
            .remove_image(
                image_name,
                Some(bollard::query_parameters::RemoveImageOptions {
                    force: true,
                    ..Default::default()
                }),
                None,
            )
            .await
            .map_err(|e| {
                BuilderError::Other(format!("Failed to remove image '{}': {}", image_name, e))
            })?;

        debug!(
            "Removed image '{}' ({} layer(s) affected)",
            image_name,
            deleted.len()
        );

        Ok(())
    }

    async fn image_identity(
        &self,
        image_name: &str,
    ) -> Result<crate::LocalImageIdentity, BuilderError> {
        let docker = self.require_docker_for_build()?;
        let inspect = docker.inspect_image(image_name).await.map_err(|e| {
            BuilderError::ImageNotFound(format!("Failed to inspect image '{}': {}", image_name, e))
        })?;
        Ok(crate::LocalImageIdentity {
            id: inspect.id.unwrap_or_default(),
            repo_digests: inspect.repo_digests.unwrap_or_default(),
        })
    }

    async fn inspect_image(&self, image_name: &str) -> Result<crate::ImageInfo, BuilderError> {
        let docker = self.require_docker_for_build()?;
        let inspect = docker.inspect_image(image_name).await.map_err(|e| {
            BuilderError::ImageNotFound(format!("Failed to inspect image '{}': {}", image_name, e))
        })?;

        let architecture = inspect
            .architecture
            .unwrap_or_else(|| "unknown".to_string());
        let os = inspect.os.unwrap_or_else(|| "linux".to_string());
        // ARM images carry the variant in a separate field: an ARMv6 image is
        // `architecture = "arm"`, `variant = "v6"`. Dropping the variant turns
        // it into a bare `arm`, which normalizes to v7 — so a correctly built
        // ARMv6 image would look like a mismatch and be rejected.
        let platform = match inspect.variant.as_deref().map(str::trim) {
            Some(variant) if !variant.is_empty() => {
                crate::platform::normalize_platform(&os, &format!("{}/{}", architecture, variant))
            }
            _ => crate::platform::normalize_platform(&os, &architecture),
        };

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

        let config = inspect.config.as_ref().map(|c| {
            let exposed = c.exposed_ports.clone().unwrap_or_default();
            crate::ImageConfig::from_raw(
                &exposed,
                c.user.as_deref(),
                c.env.as_deref().unwrap_or_default(),
                c.entrypoint.as_deref().unwrap_or_default(),
            )
        });

        Ok(crate::ImageInfo {
            id: inspect.id.unwrap_or_default(),
            architecture,
            os,
            platform,
            size_bytes,
            tags,
            created,
            working_dir,
            config,
        })
    }

    fn get_native_platform(&self) -> String {
        self.detect_native_platform()
    }

    fn discovered_platform(&self) -> Option<String> {
        self.daemon_platform.get().cloned()
    }

    async fn ensure_platform_discovered(&self) -> Option<String> {
        self.refresh_daemon_platform().await
    }
}

#[cfg(test)]
mod tests;
