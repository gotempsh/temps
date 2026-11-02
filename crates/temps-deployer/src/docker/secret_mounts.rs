// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Builds that serve BuildKit secret mounts, for `RUN --mount=type=secret`.

use super::image_builder::docker_build_args;
use super::DockerRuntime;
use crate::build_timings::BuildStepTimer;
use crate::{BuildRequest, BuildResult, BuilderError};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tracing::{debug, error, info, warn};

/// Largest gzipped build context a secret-mount build accepts. bollard holds
/// about three copies of the context while uploading it (the session upload
/// takes owned bytes and copies them twice), so this bounds the peak at about
/// 768 MiB on a control plane that may have 4 GiB.
const MAX_SECRET_MOUNT_CONTEXT_GZIP_BYTES: usize = 256 * 1024 * 1024;

/// bollard refuses a secret file larger than this.
const MAX_BUILD_SECRET_BYTES: usize = 500 * 1024;

/// How long to keep reading build progress once the solve has returned.
const STATUS_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// How long to wait for the daemon to accept the status connection.
const STATUS_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// BuildKit answers a status request for a build it has not seen yet with an
/// error, and the solve starts only after its session is up, so a status
/// request is retried this many times, this far apart.
const STATUS_ATTEMPTS: u32 = 40;
const STATUS_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(250);

type BuildKitStatus = bollard::grpc::build::moby::buildkit::v1::StatusResponse;

fn oversized_secret_mount_context(image_name: &str) -> BuilderError {
    BuilderError::Other(format!(
        "Build context for image '{image_name}' is larger than {} MiB once compressed, the limit \
         for builds that use RUN --mount=type=secret. Exclude what the build does not need with \
         .dockerignore.",
        MAX_SECRET_MOUNT_CONTEXT_GZIP_BYTES / (1024 * 1024)
    ))
}

/// Write each secret to its own file for bollard to read when BuildKit asks.
///
/// Its alternative, `SecretSource::Env`, reads this process's environment,
/// which every child process would inherit. The directory is private to this
/// user, the files are named at random rather than after the secret, and
/// dropping the returned directory removes them, including on cancellation.
fn write_build_secret_files(
    secrets: &crate::BuildSecrets,
) -> Result<(TempDir, Vec<(String, PathBuf)>), BuilderError> {
    let dir = tempfile::Builder::new()
        .prefix("temps-build-secrets-")
        .tempdir()
        .map_err(BuilderError::IoError)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .map_err(BuilderError::IoError)?;
    }
    let mut sources = Vec::new();
    for (id, value) in secrets.iter() {
        if value.len() > MAX_BUILD_SECRET_BYTES {
            return Err(BuilderError::Other(format!(
                "Build secret '{id}' is {} bytes, more than the {MAX_BUILD_SECRET_BYTES} bytes \
                 BuildKit secret mounts accept.",
                value.len()
            )));
        }
        let (mut file, path) = tempfile::NamedTempFile::new_in(dir.path())
            .map_err(BuilderError::IoError)?
            .keep()
            .map_err(|error| BuilderError::IoError(error.error))?;
        file.write_all(value.as_bytes())
            .map_err(BuilderError::IoError)?;
        sources.push((id.clone(), path));
    }
    Ok((dir, sources))
}

/// What a `/build` call would send, for the gRPC solve. The memory and CPU
/// options have no equivalent, and BuildKit ignores them on `/build` too. The
/// network stays the default one: host networking needs an entitlement that
/// bollard's solve does not request.
fn secret_mount_frontend_options(
    request: &BuildRequest,
    default_platform: &str,
    secrets: &[(String, PathBuf)],
) -> bollard::grpc::build::ImageBuildFrontendOptions {
    use bollard::grpc::build::{
        moby::buildkit::v1::CacheOptionsEntry, ImageBuildFrontendOptions, ImageBuildPlatform,
        SecretSource,
    };
    let dockerfile = request
        .dockerfile_path
        .as_ref()
        .and_then(|path| path.strip_prefix(&request.context_path).ok())
        .unwrap_or_else(|| Path::new("Dockerfile"));
    let mut options = ImageBuildFrontendOptions::builder()
        .dockerfile(dockerfile)
        .label("built-by", "temps");
    for (name, value) in docker_build_args(request, true) {
        options = options.buildarg(&name, &value);
    }
    for image in &request.cache_from {
        options = options.cachefrom(&CacheOptionsEntry {
            r#type: "registry".to_string(),
            attrs: HashMap::from([("ref".to_string(), image.clone())]),
        });
    }
    let platform = request.platform.as_deref().unwrap_or(default_platform);
    let mut parts = platform.split('/');
    if let (Some(os), Some(architecture)) = (parts.next(), parts.next()) {
        options = options.platforms(&ImageBuildPlatform {
            os: os.to_string(),
            architecture: architecture.to_string(),
            variant: parts.next().map(str::to_string),
        });
    }
    for (id, path) in secrets {
        options = options.set_secret(id, &SecretSource::File(path.clone()));
    }
    options.build()
}

/// The daemon's message, without the stack trace and metadata the gRPC status
/// carries alongside it.
fn grpc_build_error_text(error: &bollard::grpc::error::GrpcError) -> String {
    match error {
        bollard::grpc::error::GrpcError::TonicStatus { err } => err.message().to_string(),
        other => other.to_string(),
    }
}

/// Write one BuildKit progress update to the build log: step names as they
/// start and finish, which shows cached layers and progress even when a step
/// prints nothing, then the steps' own output.
pub(super) async fn emit_buildkit_status(
    status: &BuildKitStatus,
    log_file: &mut tokio::fs::File,
    log_callback: &Option<crate::LogCallback>,
) {
    for vertex in &status.vertexes {
        if vertex.name.is_empty() {
            continue;
        }
        let state = if vertex.error.is_empty() {
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
        let line = format!("[{}] {}\n", state, vertex.name);
        let _ = log_file.write_all(line.as_bytes()).await;
        debug!("BuildKit vertex: {}", line.trim());
        if let Some(callback) = log_callback {
            callback(line).await;
        }
    }
    for log in &status.logs {
        let _ = log_file.write_all(&log.msg[..]).await;
        debug!("BuildKit: {}", String::from_utf8_lossy(&log.msg));
        if let Some(callback) = log_callback {
            callback(String::from_utf8_lossy(&log.msg[..]).to_string()).await;
        }
    }
}

/// What [`follow_build_status`] reports.
enum BuildStatusEvent {
    Status(Box<BuildKitStatus>),
    /// Progress cannot be shown. The build itself does not depend on it.
    Unavailable(String),
}

async fn record_build_status_event(
    event: BuildStatusEvent,
    step_timer: &mut BuildStepTimer,
    log_file: &mut tokio::fs::File,
    log_callback: &Option<crate::LogCallback>,
) {
    match event {
        BuildStatusEvent::Status(status) => {
            step_timer.observe_vertices(&status.vertexes);
            emit_buildkit_status(&status, log_file, log_callback).await;
        }
        BuildStatusEvent::Unavailable(reason) => {
            warn!("Live build output is unavailable: {}", reason);
            let line = format!(
                "Live build output is unavailable ({reason}). The build continues, and its result \
                 and any error are still reported.\n"
            );
            let _ = log_file.write_all(line.as_bytes()).await;
            if let Some(callback) = log_callback {
                callback(line).await;
            }
        }
    }
}

/// The Unix socket bollard's default connection uses, or `None` when the
/// daemon is reached some other way.
#[allow(
    clippy::disallowed_methods,
    reason = "bootstrap: the same `DOCKER_HOST` bollard's default connection reads, so this opens \
              a second connection to the daemon that connection reached; not Temps configuration"
)]
fn docker_unix_socket() -> Option<PathBuf> {
    docker_unix_socket_from(std::env::var("DOCKER_HOST").ok().as_deref())
}

fn docker_unix_socket_from(docker_host: Option<&str>) -> Option<PathBuf> {
    match docker_host {
        None | Some("") => Some(PathBuf::from("/var/run/docker.sock")),
        Some(host) => host.strip_prefix("unix://").map(PathBuf::from),
    }
}

/// Open a connection to the daemon's BuildKit control API: `POST /grpc`,
/// upgraded to HTTP/2. bollard opens the same one for the solve but does not
/// expose it, and with it the only way to read a solve's progress.
async fn connect_buildkit_control(
    socket: PathBuf,
) -> Result<tonic::transport::Channel, tonic::transport::Error> {
    tonic::transport::Endpoint::from_static("http://[::]:50051")
        .connect_with_connector(tower::service_fn(move |_: tonic::transport::Uri| {
            let socket = socket.clone();
            async move {
                use tokio::io::AsyncReadExt;
                let mut stream = tokio::net::UnixStream::connect(socket).await?;
                stream
                    .write_all(
                        b"POST /grpc HTTP/1.1\r\nHost: docker\r\nConnection: Upgrade\r\n\
                          Upgrade: h2c\r\n\r\n",
                    )
                    .await?;
                // One byte at a time, so nothing past the headers is consumed.
                let mut head = Vec::new();
                while !head.ends_with(b"\r\n\r\n") {
                    if head.len() > 8 * 1024 {
                        return Err(std::io::Error::other("oversized /grpc upgrade response"));
                    }
                    head.push(stream.read_u8().await?);
                }
                if !head.starts_with(b"HTTP/1.1 101") {
                    return Err(std::io::Error::other(format!(
                        "/grpc upgrade refused: {}",
                        String::from_utf8_lossy(&head).lines().next().unwrap_or("")
                    )));
                }
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        }))
        .await
}

/// Report a solve's progress on `events` until it ends. Never fails the build:
/// a problem here is reported once as [`BuildStatusEvent::Unavailable`].
async fn follow_build_status(
    socket: Option<PathBuf>,
    build_ref: String,
    events: tokio::sync::mpsc::Sender<BuildStatusEvent>,
) {
    use bollard::grpc::build::moby::buildkit::v1::{control_client::ControlClient, StatusRequest};
    let unavailable = |reason: String| async {
        let _ = events.send(BuildStatusEvent::Unavailable(reason)).await;
    };
    let Some(socket) = socket else {
        unavailable("the Docker daemon is not reached through a Unix socket".to_string()).await;
        return;
    };
    let channel = match tokio::time::timeout(
        STATUS_CONNECT_TIMEOUT,
        connect_buildkit_control(socket),
    )
    .await
    {
        Ok(Ok(channel)) => channel,
        Ok(Err(error)) => return unavailable(error.to_string()).await,
        Err(_) => return unavailable("the status connection timed out".to_string()).await,
    };
    let mut control = ControlClient::new(channel);
    let mut last_error = String::new();
    for _ in 0..STATUS_ATTEMPTS {
        let request = StatusRequest {
            r#ref: build_ref.clone(),
        };
        match control.status(request).await {
            Ok(response) => {
                let mut stream = response.into_inner();
                loop {
                    match stream.message().await {
                        Ok(Some(status)) => {
                            let event = BuildStatusEvent::Status(Box::new(status));
                            if events.send(event).await.is_err() {
                                return;
                            }
                        }
                        Ok(None) => return,
                        Err(error) => return unavailable(error.message().to_string()).await,
                    }
                }
            }
            Err(error) => {
                last_error = error.message().to_string();
                tokio::time::sleep(STATUS_RETRY_DELAY).await;
            }
        }
    }
    unavailable(last_error).await;
}

impl DockerRuntime {
    /// The build context as a gzipped tar in memory, which is how bollard's
    /// session upload takes it. Refuses a context past
    /// [`MAX_SECRET_MOUNT_CONTEXT_GZIP_BYTES`] rather than exhausting memory.
    async fn create_gzipped_context(
        context_path: PathBuf,
        dockerfile_path: Option<PathBuf>,
        image_name: String,
    ) -> Result<bytes::Bytes, BuilderError> {
        let tmp = Self::write_tar_context_file(context_path, dockerfile_path).await?;
        tokio::task::spawn_blocking(move || {
            let mut source = tmp.reopen().map_err(BuilderError::IoError)?;
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            let mut chunk = vec![0u8; 64 * 1024];
            loop {
                let read =
                    std::io::Read::read(&mut source, &mut chunk).map_err(BuilderError::IoError)?;
                if read == 0 {
                    break;
                }
                encoder
                    .write_all(&chunk[..read])
                    .map_err(BuilderError::IoError)?;
                if encoder.get_ref().len() > MAX_SECRET_MOUNT_CONTEXT_GZIP_BYTES {
                    return Err(oversized_secret_mount_context(&image_name));
                }
            }
            let gzipped = encoder.finish().map_err(BuilderError::IoError)?;
            if gzipped.len() > MAX_SECRET_MOUNT_CONTEXT_GZIP_BYTES {
                return Err(oversized_secret_mount_context(&image_name));
            }
            Ok(bytes::Bytes::from(gzipped))
        })
        .await
        .map_err(|error| {
            BuilderError::Other(format!("Build context compression task failed: {error}"))
        })?
    }

    /// Build through a BuildKit session that serves the request's secrets.
    ///
    /// bollard offers secrets only on its gRPC solve, not on the `/build` call
    /// every other build uses, so only a build whose Dockerfile mounts a known
    /// secret comes through here. The solve reports nothing but a result, so
    /// its progress is followed over a separate connection (see
    /// [`follow_build_status`]).
    pub(super) async fn build_image_with_secret_mounts(
        &self,
        request: BuildRequest,
        log_callback: Option<crate::LogCallback>,
        start_time: Instant,
    ) -> Result<BuildResult, BuilderError> {
        if !self.use_buildkit {
            return Err(BuilderError::Other(format!(
                "Cannot build image '{}': its Dockerfile uses RUN --mount=type=secret, which \
                 needs BuildKit, and this server builds with the legacy builder.",
                request.image_name
            )));
        }
        let (secret_dir, secret_sources) = write_build_secret_files(&request.build_secrets)?;
        let default_platform = self.detect_native_platform();
        let options = secret_mount_frontend_options(&request, &default_platform, &secret_sources);
        let context = Self::create_gzipped_context(
            request.context_path.clone(),
            request.dockerfile_path.clone(),
            request.image_name.clone(),
        )
        .await?;

        let configured_caps = self.build_limits.resource_caps();
        let (memory_bytes, _, _) = Self::resolve_build_resource_caps_for(configured_caps);
        let memory_i32 = self.effective_build_memory(
            memory_bytes,
            configured_caps.is_some(),
            &request.image_name,
        );
        let mut log_file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&request.log_path)
            .await
            .map_err(BuilderError::IoError)?;

        let docker = self.require_docker_for_build()?;
        let build_start = self.sample_build_start();
        let build_ref = bollard::grpc::BuildRef::random();
        let (status_sender, mut status_receiver) = tokio::sync::mpsc::channel(256);
        let follower = tokio::spawn(follow_build_status(
            docker_unix_socket(),
            build_ref.as_ref().to_string(),
            status_sender,
        ));

        // The solve future is not `Send`, so it runs on its own blocking thread.
        // Dropping `_cancel_on_drop` (this future being dropped) ends the select,
        // which closes the session and with it the build.
        let (_cancel_on_drop, cancelled) = tokio::sync::oneshot::channel::<()>();
        let image_name = request.image_name.clone();
        let runtime = tokio::runtime::Handle::current();
        let solve_docker = docker.clone();
        let solve = tokio::task::spawn_blocking(move || {
            runtime.block_on(async move {
                let build = bollard::grpc::driver::Build::docker_build(
                    bollard::grpc::driver::moby::Moby::new(&solve_docker),
                    &image_name,
                    options,
                    bollard::grpc::build::ImageBuildLoadInput::Upload(context),
                    None,
                    Some(build_ref),
                );
                tokio::select! {
                    result = build => result.map_err(|error| grpc_build_error_text(&error)),
                    _ = cancelled => Err("the build was cancelled".to_string()),
                }
            })
        });
        tokio::pin!(solve);

        let mut step_timer = BuildStepTimer::new();
        let outcome = loop {
            tokio::select! {
                Some(event) = status_receiver.recv() => {
                    record_build_status_event(event, &mut step_timer, &mut log_file, &log_callback)
                        .await;
                }
                outcome = &mut solve => break outcome,
            }
        };
        // The solve can end just before its last events are delivered.
        let drain = async {
            while let Some(event) = status_receiver.recv().await {
                record_build_status_event(event, &mut step_timer, &mut log_file, &log_callback)
                    .await;
            }
        };
        let _ = tokio::time::timeout(STATUS_DRAIN_TIMEOUT, drain).await;
        follower.abort();
        drop(secret_dir);

        let outcome = outcome
            .map_err(|error| format!("the build task stopped: {error}"))
            .and_then(|outcome| outcome);
        if let Some(summary) = step_timer.summary(start_time.elapsed()) {
            info!(image = %request.image_name, "Build step timings\n{}", summary.trim_end());
            let _ = log_file.write_all(summary.as_bytes()).await;
            if let Some(ref callback) = log_callback {
                for line in summary.lines() {
                    callback(format!("{line}\n")).await;
                }
            }
        }
        if let Err(error_text) = outcome {
            error!("Build failed: {}", error_text);
            let error_line = format!("ERROR: Build failed: {}\n", error_text);
            let _ = log_file.write_all(error_line.as_bytes()).await;
            if let Some(ref callback) = log_callback {
                callback(error_line).await;
            }
            let (error, memory_line) =
                self.classify_build_failure(error_text, &build_start, i64::from(memory_i32));
            if let Some(line) = memory_line {
                let _ = log_file.write_all(line.as_bytes()).await;
                if let Some(ref callback) = log_callback {
                    callback(line).await;
                }
            }
            let _ = log_file.flush().await;
            return Err(error);
        }
        let _ = log_file.flush().await;

        let images = docker
            .list_images(Some(bollard::query_parameters::ListImagesOptions {
                filters: Some(HashMap::from([(
                    "reference".to_string(),
                    vec![request.image_name.clone()],
                )])),
                ..Default::default()
            }))
            .await
            .map_err(|e| BuilderError::Other(format!("Failed to get image info: {}", e)))?;
        let image = images.first().ok_or_else(|| {
            BuilderError::Other(format!("Built image '{}' not found", request.image_name))
        })?;
        Ok(BuildResult {
            image_id: image.id.clone(),
            image_name: request.image_name,
            size_bytes: image.size as u64,
            build_duration_ms: start_time.elapsed().as_millis() as u64,
        })
    }
}

#[cfg(test)]
mod tests;
