// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Remote node deployer — implements `ContainerDeployer` and `ImageBuilder`
//! by calling the agent's HTTP API on a remote worker node.
//!
//! From `WorkflowExecutionService`'s perspective, deploying to a remote node
//! is identical to deploying locally.

use async_trait::async_trait;
use futures::{StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::build_protocol::{
    validate_archive_path, BuildEvent, BuildFailureKind, BuildSpec, DockerIgnore,
    BUILD_PROTOCOL_VERSION, MAX_BUILD_CONTEXT_BYTES, MAX_BUILD_CONTEXT_ENTRIES,
    MAX_BUILD_EVENT_BYTES,
};

use crate::{
    BuildRequest, BuildRequestWithCallback, BuildResult, BuilderError, ContainerDeployer,
    ContainerInfo, ContainerStats, DeployRequest, DeployResult, DeployerError, ImageBuilder,
    ImageImportStream, ImageInfo,
};

/// Slightly exceeds the worker's 30-minute queue-and-import deadline so the
/// control plane receives the worker's contextual timeout response.
const IMAGE_IMPORT_REQUEST_TIMEOUT: Duration = Duration::from_secs(31 * 60);

/// Slightly exceeds the worker's 30-minute pull-and-inspect deadline for the
/// same reason as [`IMAGE_IMPORT_REQUEST_TIMEOUT`] — the control plane should
/// see the worker's own timeout response rather than a client-side cutoff.
const IMAGE_PULL_REQUEST_TIMEOUT: Duration = Duration::from_secs(31 * 60);
const IMAGE_BUILD_REQUEST_TIMEOUT: Duration = Duration::from_secs(31 * 60);
/// Slightly exceeds the worker's 30-minute export deadline.
const IMAGE_EXPORT_REQUEST_TIMEOUT: Duration = Duration::from_secs(31 * 60);
/// Longest agent error body quoted back into a deployment log.
const MAX_AGENT_ERROR_BYTES: usize = 4 * 1024;

/// Read an agent's refusal body for an error message, bounded so a
/// misbehaving agent cannot inflate a deployment log.
async fn agent_error_detail(response: reqwest::Response) -> String {
    let body = match response.bytes().await {
        Ok(body) => body,
        Err(error) => return format!("unreadable response body: {error}"),
    };
    let body = &body[..body.len().min(MAX_AGENT_ERROR_BYTES)];
    match serde_json::from_slice::<AgentResponse<serde_json::Value>>(body) {
        Ok(AgentResponse {
            error: Some(error), ..
        }) => error,
        _ => String::from_utf8_lossy(body).trim().to_string(),
    }
}

/// What to archive from a build context, decided before anything is read.
struct ContextFilter {
    ignore: DockerIgnore,
    /// Paths the Docker CLI always sends even when ignored: the Dockerfile
    /// and the ignore file itself.
    always_include: Vec<PathBuf>,
}

impl ContextFilter {
    fn includes(&self, path: &Path) -> bool {
        self.always_include.iter().any(|kept| kept == path) || !self.ignore.is_excluded(path)
    }

    /// Whether an excluded directory may still contain something to send.
    fn must_descend(&self, directory: &Path) -> bool {
        self.ignore.has_exceptions()
            || self
                .always_include
                .iter()
                .any(|kept| kept.starts_with(directory))
    }
}

fn append_build_context(
    archive: &mut tar::Builder<std::fs::File>,
    root: &Path,
    relative: &Path,
    filter: &ContextFilter,
    total: &mut u64,
    entries: &mut usize,
) -> Result<(), BuilderError> {
    let directory = root.join(relative);
    for item in std::fs::read_dir(&directory).map_err(BuilderError::IoError)? {
        let item = item.map_err(BuilderError::IoError)?;
        let path = relative.join(item.file_name());
        validate_archive_path(&path).map_err(BuilderError::InvalidContext)?;
        if path.components().any(|part| part.as_os_str() == ".git") {
            continue;
        }
        let file_type = item.file_type().map_err(BuilderError::IoError)?;
        let included = filter.includes(&path);
        if !included && !(file_type.is_dir() && filter.must_descend(&path)) {
            continue;
        }
        if file_type.is_symlink() {
            return Err(BuilderError::InvalidContext(format!(
                "Worker build context contains a symlink: '{}'; add it to .dockerignore \
                 or replace it with a regular file",
                path.display()
            )));
        }
        let metadata = item.metadata().map_err(BuilderError::IoError)?;
        if metadata.is_dir() {
            if included {
                *entries += 1;
                if *entries > MAX_BUILD_CONTEXT_ENTRIES {
                    return Err(BuilderError::ResourceLimitExceeded(format!(
                        "Worker build context exceeds {MAX_BUILD_CONTEXT_ENTRIES} entries"
                    )));
                }
                archive
                    .append_dir(&path, item.path())
                    .map_err(BuilderError::IoError)?;
            }
            append_build_context(archive, root, &path, filter, total, entries)?;
        } else if metadata.is_file() {
            *entries += 1;
            if *entries > MAX_BUILD_CONTEXT_ENTRIES {
                return Err(BuilderError::ResourceLimitExceeded(format!(
                    "Worker build context exceeds {MAX_BUILD_CONTEXT_ENTRIES} entries"
                )));
            }
            *total = total.checked_add(metadata.len()).ok_or_else(|| {
                BuilderError::ResourceLimitExceeded("Worker build context size overflowed".into())
            })?;
            if *total > MAX_BUILD_CONTEXT_BYTES {
                return Err(BuilderError::ResourceLimitExceeded(format!(
                    "Worker build context exceeds {MAX_BUILD_CONTEXT_BYTES} bytes"
                )));
            }
            let mut file = std::fs::File::open(item.path()).map_err(BuilderError::IoError)?;
            archive
                .append_file(&path, &mut file)
                .map_err(BuilderError::IoError)?;
        } else {
            return Err(BuilderError::InvalidContext(format!(
                "Worker build context contains a non-file entry: '{}'",
                path.display()
            )));
        }
    }
    Ok(())
}

/// Load the ignore rules Docker would apply for `dockerfile`: a
/// `<Dockerfile>.dockerignore` beside it takes precedence over the context
/// root's `.dockerignore`, as with BuildKit.
fn load_context_filter(root: &Path, dockerfile: &Path) -> Result<ContextFilter, BuilderError> {
    let mut specific = dockerfile.as_os_str().to_owned();
    specific.push(".dockerignore");
    let candidates = [PathBuf::from(specific), PathBuf::from(".dockerignore")];
    let mut always_include = vec![dockerfile.to_path_buf()];
    for candidate in candidates {
        let full = root.join(&candidate);
        let metadata = match std::fs::symlink_metadata(&full) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(BuilderError::IoError(error)),
        };
        if !metadata.is_file() {
            return Err(BuilderError::InvalidContext(format!(
                "'{}' must be a regular file",
                candidate.display()
            )));
        }
        let contents = std::fs::read_to_string(&full).map_err(|error| {
            BuilderError::InvalidContext(format!("Cannot read '{}': {error}", candidate.display()))
        })?;
        let ignore = DockerIgnore::parse(&contents, &candidate.to_string_lossy())
            .map_err(BuilderError::InvalidContext)?;
        always_include.push(candidate);
        return Ok(ContextFilter {
            ignore,
            always_include,
        });
    }
    Ok(ContextFilter {
        ignore: DockerIgnore::empty(),
        always_include,
    })
}

fn prepare_build_context(
    request: &BuildRequest,
) -> Result<(tempfile::NamedTempFile, BuildSpec), BuilderError> {
    let root = request
        .context_path
        .canonicalize()
        .map_err(BuilderError::IoError)?;
    if !root.is_dir() {
        return Err(BuilderError::InvalidContext(format!(
            "Worker build context '{}' is not a directory",
            root.display()
        )));
    }
    let dockerfile = request
        .dockerfile_path
        .as_deref()
        .unwrap_or_else(|| Path::new("Dockerfile"));
    let dockerfile = if dockerfile.is_absolute() {
        dockerfile
            .strip_prefix(&root)
            .map_err(|_| {
                BuilderError::InvalidContext(
                    "Dockerfile is outside the worker build context".into(),
                )
            })?
            .to_path_buf()
    } else {
        dockerfile.to_path_buf()
    };
    validate_archive_path(&dockerfile).map_err(BuilderError::InvalidContext)?;
    let dockerfile_meta =
        std::fs::symlink_metadata(root.join(&dockerfile)).map_err(BuilderError::IoError)?;
    if !dockerfile_meta.is_file() || dockerfile_meta.file_type().is_symlink() {
        return Err(BuilderError::InvalidContext(
            "Worker build Dockerfile must be a regular file".into(),
        ));
    }
    // The planner includes every resolved environment value in build_args,
    // including credentials, even when a Dockerfile does not use them. Do not
    // transfer any of those values to a worker. A Dockerfile declaring ARG
    // needs an explicit credential-handling design before remote builds can
    // preserve the local builder's semantics safely.
    if !request.build_args.is_empty() || !request.build_args_buildkit.is_empty() {
        let contents =
            std::fs::read_to_string(root.join(&dockerfile)).map_err(BuilderError::IoError)?;
        if dockerfile_declares_build_arg(&contents) {
            return Err(BuilderError::InvalidContext(
                "Worker builds cannot use Dockerfile ARG instructions until build-argument credential handling is reviewed".into(),
            ));
        }
    }
    let spec = BuildSpec {
        version: BUILD_PROTOCOL_VERSION,
        image_name: request.image_name.clone(),
        dockerfile: dockerfile.to_string_lossy().into_owned(),
        platform: request.platform.clone(),
    };
    spec.validate().map_err(BuilderError::InvalidContext)?;
    let archive_file = tempfile::NamedTempFile::new().map_err(BuilderError::IoError)?;
    let mut builder = tar::Builder::new(archive_file.reopen().map_err(BuilderError::IoError)?);
    let filter = load_context_filter(&root, &dockerfile)?;
    let mut total = 0;
    let mut entries = 0;
    append_build_context(
        &mut builder,
        &root,
        Path::new(""),
        &filter,
        &mut total,
        &mut entries,
    )?;
    builder.finish().map_err(BuilderError::IoError)?;
    drop(builder);
    if archive_file
        .as_file()
        .metadata()
        .map_err(BuilderError::IoError)?
        .len()
        > MAX_BUILD_CONTEXT_BYTES
    {
        return Err(BuilderError::ResourceLimitExceeded(format!(
            "Worker build archive exceeds {MAX_BUILD_CONTEXT_BYTES} bytes"
        )));
    }
    Ok((archive_file, spec))
}

/// Conservative scan: false positives only refuse a build, while a missed
/// ARG could change its result after we intentionally discard all arguments.
fn dockerfile_declares_build_arg(contents: &str) -> bool {
    contents
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .any(|word| word.eq_ignore_ascii_case("ARG"))
}

async fn dispatch_build_event(
    line: &[u8],
    callback: Option<&crate::LogCallback>,
) -> Result<Option<BuildResult>, BuilderError> {
    let event: BuildEvent = serde_json::from_slice(line).map_err(|error| {
        BuilderError::Other(format!("Worker returned an invalid build event: {error}"))
    })?;
    match event {
        BuildEvent::Log(message) => {
            if let Some(callback) = callback {
                callback(message).await;
            }
            Ok(None)
        }
        BuildEvent::Result(result) => Ok(Some(result)),
        BuildEvent::Failure(failure) => Err(match failure.kind {
            BuildFailureKind::Build => BuilderError::BuildFailed(failure.message),
            BuildFailureKind::Timeout | BuildFailureKind::Worker => {
                BuilderError::Other(format!("Worker build failed: {}", failure.message))
            }
        }),
    }
}

/// Registry credentials to forward to the worker's `POST /agent/images/pull`
/// call. Mirrors the wire shape of `temps_agent::RegistryCredentials` field
/// for field; `temps-deployer` does not depend on `temps-agent` (the same
/// reason `import_image` below builds its own request rather than sharing a
/// DTO type), so this struct only needs to serialize to the JSON body the
/// agent handler already deserializes.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RemotePullCredentials {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// **Never logged or included in error messages.**
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// **Never logged or included in error messages.**
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_address: Option<String>,
}

/// Wire body for `POST /agent/images/pull`, mirroring
/// `temps_agent::PullImageRequest`.
#[derive(Serialize)]
struct RemotePullImageRequest {
    image: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    credentials: Option<RemotePullCredentials>,
}

/// Wire response from `POST /agent/images/pull`, mirroring
/// `temps_agent::PullImageResponse`.
#[derive(Deserialize)]
struct RemotePullImageResponse {
    image_id: String,
    #[allow(dead_code)]
    digest: Option<String>,
}

/// Response envelope from the agent API.
#[derive(Deserialize)]
struct AgentResponse<T> {
    success: bool,
    data: Option<T>,
    error: Option<String>,
}

/// Deploys containers to a remote node by calling its agent HTTP API.
pub struct RemoteNodeDeployer {
    /// Base URL of the agent, e.g. "https://10.100.0.2:3100"
    agent_url: String,
    /// Bearer token for authentication
    token: String,
    /// Node name (for error messages)
    node_name: String,
    /// HTTP client with timeouts
    client: reqwest::Client,
    /// Container platform of the remote node's Docker daemon
    /// (`linux/amd64`, `linux/arm64`), as recorded on the `nodes` row.
    ///
    /// This is what makes `get_native_platform` truthful for a remote node.
    /// Set via [`Self::with_platform`] by the caller that already loaded the
    /// node row, so the common path costs no extra round-trip; when it is
    /// `None` (node never reported, e.g. a pre-multi-arch agent), it can be
    /// filled in from the agent's health endpoint with
    /// [`Self::refresh_platform`].
    platform: std::sync::Arc<std::sync::OnceLock<String>>,
}

impl RemoteNodeDeployer {
    pub fn new(agent_url: String, token: String, node_name: String) -> Result<Self, DeployerError> {
        // Strict TLS by default; operators with self-signed agent certs
        // on a trusted internal network can opt in via the
        // `insecure_tls` toggle in the application settings UI.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .danger_accept_invalid_certs(temps_core::tls::insecure_tls_enabled())
            .build()
            .map_err(|e| {
                DeployerError::NetworkError(format!(
                    "Failed to create HTTP client for node {}: {}",
                    node_name, e
                ))
            })?;

        Ok(Self {
            agent_url,
            token,
            node_name,
            client,
            platform: std::sync::Arc::new(std::sync::OnceLock::new()),
        })
    }

    /// Construct a deployer that talks to the agent over **mutual TLS**
    /// (ADR-020 WS-2.1): the control plane presents `client_identity_pem`
    /// (its leaf cert + key, signed by the cluster CA) and pins the agent's
    /// server cert to the cluster CA (`ca_cert_pem`). Built-in roots are
    /// disabled so ONLY the cluster CA is trusted. Used for nodes whose
    /// `agent_url` is `https://`.
    pub fn new_mtls(
        agent_url: String,
        token: String,
        node_name: String,
        client_identity_pem: &str,
        ca_cert_pem: &str,
    ) -> Result<Self, DeployerError> {
        let identity =
            reqwest::Identity::from_pem(client_identity_pem.as_bytes()).map_err(|e| {
                DeployerError::NetworkError(format!(
                    "Invalid control-plane client identity for node {}: {}",
                    node_name, e
                ))
            })?;
        let ca = reqwest::Certificate::from_pem(ca_cert_pem.as_bytes()).map_err(|e| {
            DeployerError::NetworkError(format!(
                "Invalid cluster CA certificate for node {}: {}",
                node_name, e
            ))
        })?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .use_rustls_tls()
            .identity(identity)
            .add_root_certificate(ca)
            .tls_built_in_root_certs(false)
            .build()
            .map_err(|e| {
                DeployerError::NetworkError(format!(
                    "Failed to create mTLS client for node {}: {}",
                    node_name, e
                ))
            })?;

        Ok(Self {
            agent_url,
            token,
            node_name,
            client,
            platform: std::sync::Arc::new(std::sync::OnceLock::new()),
        })
    }

    /// Record the node's container platform, as stored on its `nodes` row.
    ///
    /// A `None` or blank value leaves the platform unknown — the caller can
    /// then fall back to [`Self::refresh_platform`], which asks the agent.
    pub fn with_platform(self, platform: Option<String>) -> Self {
        if let Some(platform) = platform {
            let platform = platform.trim();
            if !platform.is_empty() {
                let _ = self
                    .platform
                    .set(crate::platform::canonicalize_platform(platform));
            }
        }
        self
    }

    /// Ask the agent for its platform and cache it.
    ///
    /// Only needed when the `nodes` row has no architecture yet (agent older
    /// than multi-arch support, or upgraded but not yet heartbeated). Returns
    /// `None` when the agent is unreachable or reports nothing usable — the
    /// caller must then decide whether to proceed, not silently assume amd64.
    pub async fn refresh_platform(&self) -> Option<String> {
        if let Some(cached) = self.platform.get() {
            return Some(cached.clone());
        }

        #[derive(Deserialize)]
        struct HealthPlatform {
            #[serde(default)]
            platform: String,
        }

        let health: HealthPlatform = match self.agent_get("/agent/health").await {
            Ok(health) => health,
            Err(e) => {
                tracing::warn!(
                    node = %self.node_name,
                    "Could not read platform from agent health endpoint: {}",
                    e
                );
                return None;
            }
        };

        let reported = health.platform.trim();
        if reported.is_empty() {
            return None;
        }

        let platform = crate::platform::canonicalize_platform(reported);
        let _ = self.platform.set(platform.clone());
        Some(platform)
    }

    /// The node's platform if known, without contacting the agent.
    pub fn platform(&self) -> Option<String> {
        self.platform.get().cloned()
    }

    /// Helper to make authenticated GET requests to the agent.
    async fn agent_get<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
    ) -> Result<T, DeployerError> {
        self.agent_get_inner(path, None).await
    }

    /// GET a container-scoped agent resource while preserving a missing
    /// runtime container as the typed, idempotent not-found outcome.
    async fn agent_get_container<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        container_id: &str,
    ) -> Result<T, DeployerError> {
        self.agent_get_inner(path, Some(container_id)).await
    }

    async fn agent_get_inner<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        missing_container_id: Option<&str>,
    ) -> Result<T, DeployerError> {
        let url = format!("{}{}", self.agent_url, path);
        let response = self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| {
                DeployerError::NetworkError(format!(
                    "Failed to reach agent on node {} at {}: {}",
                    self.node_name, url, e
                ))
            })?;

        let status = response.status();
        let body: AgentResponse<T> = response.json().await.map_err(|e| {
            DeployerError::NetworkError(format!(
                "Invalid response from node {} at {}: {}",
                self.node_name, url, e
            ))
        })?;

        if status == reqwest::StatusCode::NOT_FOUND {
            if let Some(container_id) = missing_container_id {
                return Err(DeployerError::ContainerNotFound(format!(
                    "container {} was not found on node {} at {}: {}",
                    container_id,
                    self.node_name,
                    url,
                    body.error.unwrap_or_else(|| "not found".to_string())
                )));
            }
        }

        if !body.success {
            return Err(DeployerError::DeploymentFailed(format!(
                "Agent on node {} returned error ({}): {}",
                self.node_name,
                status,
                body.error.unwrap_or_default()
            )));
        }

        body.data.ok_or_else(|| {
            DeployerError::DeploymentFailed(format!(
                "Agent on node {} returned success but no data at {}",
                self.node_name, url
            ))
        })
    }

    /// Helper to make authenticated POST requests to the agent.
    async fn agent_post<B: Serialize, T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, DeployerError> {
        let url = format!("{}{}", self.agent_url, path);
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await
            .map_err(|e| {
                DeployerError::NetworkError(format!(
                    "Failed to reach agent on node {} at {}: {}",
                    self.node_name, url, e
                ))
            })?;

        let status = response.status();
        let body: AgentResponse<T> = response.json().await.map_err(|e| {
            DeployerError::NetworkError(format!(
                "Invalid response from node {} at {}: {}",
                self.node_name, url, e
            ))
        })?;

        if !body.success {
            return Err(DeployerError::DeploymentFailed(format!(
                "Agent on node {} returned error ({}): {}",
                self.node_name,
                status,
                body.error.unwrap_or_default()
            )));
        }

        body.data.ok_or_else(|| {
            DeployerError::DeploymentFailed(format!(
                "Agent on node {} returned success but no data at {}",
                self.node_name, url
            ))
        })
    }

    /// Helper to make authenticated DELETE requests to the agent.
    async fn agent_delete(&self, path: &str) -> Result<(), DeployerError> {
        let url = format!("{}{}", self.agent_url, path);
        let response = self
            .client
            .delete(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| {
                DeployerError::NetworkError(format!(
                    "Failed to reach agent on node {} at {}: {}",
                    self.node_name, url, e
                ))
            })?;
        let status = response.status();

        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(DeployerError::ContainerNotFound(format!(
                "container at {} was not found on node {}",
                url, self.node_name
            )));
        }

        let body: AgentResponse<String> = response.json().await.map_err(|e| {
            DeployerError::NetworkError(format!(
                "Invalid response from node {} at {}: {}",
                self.node_name, url, e
            ))
        })?;

        if !body.success {
            return Err(DeployerError::DeploymentFailed(format!(
                "Agent on node {} returned error: {}",
                self.node_name,
                body.error.unwrap_or_default()
            )));
        }

        Ok(())
    }

    /// Get the node name this deployer targets.
    pub fn node_name(&self) -> &str {
        &self.node_name
    }

    /// Get the agent URL.
    pub fn agent_url(&self) -> &str {
        &self.agent_url
    }

    /// The bearer token used to authenticate with the agent. Exposed so
    /// the control plane can build a `Sec-WebSocket` upgrade request that
    /// matches the rest of the agent API auth.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Run a one-shot command in a container on the remote node and
    /// collect stdout/stderr + exit code. Mirrors the CP's local
    /// `exec_command` handler so the CP can pick the right path by
    /// `node_id` without a behavior change for callers.
    pub async fn exec_command(
        &self,
        container_id: &str,
        command: Vec<String>,
        timeout_seconds: Option<u64>,
    ) -> Result<RemoteExecResult, DeployerError> {
        let body = serde_json::json!({
            "command": command,
            "timeout_seconds": timeout_seconds,
        });
        self.agent_post(&format!("/agent/containers/{}/exec", container_id), &body)
            .await
    }

    /// Stream `image` out of this node's Docker daemon as a `docker save`
    /// tar, via `GET /agent/images/export`.
    ///
    /// Used to hand a worker-built image to the node that will run it. The
    /// bytes flow through the control plane's process straight into the
    /// target's import request; nothing is written to its disk and no Docker
    /// daemon is involved on the control plane.
    pub async fn export_image_stream(
        &self,
        image: &str,
    ) -> Result<ImageImportStream, BuilderError> {
        let url = format!("{}/agent/images/export", self.agent_url);
        let response = self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .timeout(IMAGE_EXPORT_REQUEST_TIMEOUT)
            .query(&[("image", image)])
            .send()
            .await
            .map_err(|error| {
                BuilderError::Other(format!(
                    "Cannot export image '{image}' from node '{}': {error}",
                    self.node_name
                ))
            })?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(BuilderError::ImageNotFound(format!(
                "'{image}' is not present on node '{}'",
                self.node_name
            )));
        }
        if !status.is_success() {
            let detail = agent_error_detail(response).await;
            return Err(BuilderError::Other(format!(
                "Node '{}' refused to export image '{image}' (HTTP {status}): {detail}",
                self.node_name
            )));
        }
        let node_name = self.node_name.clone();
        Ok(Box::pin(response.bytes_stream().map_err(move |error| {
            std::io::Error::other(format!(
                "Image export stream from node '{node_name}' failed: {error}"
            ))
        })))
    }

    /// Ask this worker to pull `image` directly from its registry via
    /// `POST /agent/images/pull`, instead of the control plane `docker save`-ing
    /// the image and streaming a tar to [`ImageBuilder::import_image`].
    ///
    /// This is the path used for registry-sourced deploys so a control-plane
    /// process with no Docker client of its own (the control-plane serve
    /// profile) can still get an image onto a worker node — the worker does
    /// all the Docker work itself. Returns the resolved image ID reported by
    /// the worker's Docker daemon.
    ///
    /// Uses a request-scoped timeout matching the worker's own 30-minute pull
    /// deadline, mirroring [`Self::import_image`]'s handling of its long-running
    /// transfer.
    pub async fn pull_image_from_registry(
        &self,
        image: &str,
        credentials: Option<RemotePullCredentials>,
    ) -> Result<String, DeployerError> {
        tracing::info!(
            node = %self.node_name,
            image = %image,
            has_credentials = credentials.is_some(),
            "Requesting remote pull from registry"
        );

        let request = RemotePullImageRequest {
            image: image.to_string(),
            credentials,
        };

        let url = format!("{}/agent/images/pull", self.agent_url);
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .timeout(IMAGE_PULL_REQUEST_TIMEOUT)
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                DeployerError::NetworkError(format!(
                    "Failed to reach agent on node {} at {}: {}",
                    self.node_name, url, e
                ))
            })?;

        let status = response.status();
        let body: AgentResponse<RemotePullImageResponse> = response.json().await.map_err(|e| {
            DeployerError::NetworkError(format!(
                "Invalid response from node {} during image pull at {}: {}",
                self.node_name, url, e
            ))
        })?;

        if !body.success {
            return Err(DeployerError::DeploymentFailed(format!(
                "Image pull for '{}' failed on node {} ({}): {}",
                image,
                self.node_name,
                status,
                body.error.unwrap_or_default()
            )));
        }

        let data = body.data.ok_or_else(|| {
            DeployerError::DeploymentFailed(format!(
                "Agent on node {} returned success but no data pulling '{}' at {}",
                self.node_name, image, url
            ))
        })?;

        tracing::info!(
            node = %self.node_name,
            image = %image,
            image_id = %data.image_id,
            "Image pulled successfully on remote node"
        );

        Ok(data.image_id)
    }
}

/// Wire-compatible mirror of the agent's `AgentExecResponse`. Lives in the
/// deployer crate so the CP service layer can depend on it without pulling
/// in `temps-agent`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteExecResult {
    pub exit_code: Option<i64>,
    pub stdout: String,
    pub stderr: String,
}

#[async_trait]
impl ContainerDeployer for RemoteNodeDeployer {
    async fn deploy_container(
        &self,
        request: DeployRequest,
    ) -> Result<DeployResult, DeployerError> {
        self.agent_post("/agent/containers/deploy", &request).await
    }

    async fn start_container(&self, container_id: &str) -> Result<(), DeployerError> {
        let _: String = self
            .agent_post(
                &format!("/agent/containers/{}/start", container_id),
                &serde_json::json!({}),
            )
            .await?;
        Ok(())
    }

    async fn stop_container(&self, container_id: &str) -> Result<(), DeployerError> {
        let _: String = self
            .agent_post(
                &format!("/agent/containers/{}/stop", container_id),
                &serde_json::json!({}),
            )
            .await?;
        Ok(())
    }

    async fn pause_container(&self, _container_id: &str) -> Result<(), DeployerError> {
        Err(DeployerError::Other(
            "Pause not supported on remote nodes".into(),
        ))
    }

    async fn resume_container(&self, _container_id: &str) -> Result<(), DeployerError> {
        Err(DeployerError::Other(
            "Resume not supported on remote nodes".into(),
        ))
    }

    async fn remove_container(&self, container_id: &str) -> Result<(), DeployerError> {
        self.agent_delete(&format!("/agent/containers/{}", container_id))
            .await
    }

    async fn get_container_info(&self, container_id: &str) -> Result<ContainerInfo, DeployerError> {
        self.agent_get_container(
            &format!("/agent/containers/{}/info", container_id),
            container_id,
        )
        .await
    }

    async fn get_container_stats(
        &self,
        container_id: &str,
    ) -> Result<ContainerStats, DeployerError> {
        self.agent_get_container(
            &format!("/agent/containers/{}/stats", container_id),
            container_id,
        )
        .await
    }

    async fn list_containers(&self) -> Result<Vec<ContainerInfo>, DeployerError> {
        self.agent_get("/agent/containers").await
    }

    async fn get_container_logs(&self, container_id: &str) -> Result<String, DeployerError> {
        self.agent_get_container(
            &format!("/agent/containers/{}/logs", container_id),
            container_id,
        )
        .await
    }

    async fn stream_container_logs(
        &self,
        container_id: &str,
    ) -> Result<Box<dyn futures::Stream<Item = String> + Unpin + Send>, DeployerError> {
        let url = format!(
            "{}/agent/containers/{}/logs/stream?follow=true&tail=all",
            self.agent_url,
            urlencoding::encode(container_id)
        );
        let response = self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| {
                DeployerError::NetworkError(format!(
                    "Failed to start log stream for container {} on node {} at {}: {}",
                    container_id, self.node_name, url, e
                ))
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|e| format!("response body could not be read: {e}"));
            return Err(DeployerError::DeploymentFailed(format!(
                "Agent on node {} rejected log stream for container {} ({}): {}",
                self.node_name, container_id, status, body
            )));
        }

        let node_name = self.node_name.clone();
        let container_id = container_id.to_string();
        let stream = response.bytes_stream().map(move |chunk| match chunk {
            Ok(bytes) => String::from_utf8_lossy(&bytes).replace('\0', ""),
            Err(e) => format!(
                "Log stream transport error for container {} on node {}: {}",
                container_id, node_name, e
            ),
        });

        Ok(Box::new(Box::pin(stream)))
    }

    async fn image_exists(&self, image_name: &str) -> Result<bool, DeployerError> {
        self.agent_get(&format!(
            "/agent/images/{}/exists",
            urlencoding::encode(image_name)
        ))
        .await
    }
}

#[async_trait]
impl ImageBuilder for RemoteNodeDeployer {
    async fn build_image(&self, request: BuildRequest) -> Result<BuildResult, BuilderError> {
        self.build_image_with_callback(BuildRequestWithCallback {
            request,
            log_callback: None,
        })
        .await
    }

    async fn build_image_with_callback(
        &self,
        request: BuildRequestWithCallback,
    ) -> Result<BuildResult, BuilderError> {
        let BuildRequestWithCallback {
            request,
            log_callback,
        } = request;
        let node_name = self.node_name.clone();
        let (archive, spec) = tokio::task::spawn_blocking(move || prepare_build_context(&request))
            .await
            .map_err(|error| {
                BuilderError::Other(format!(
                    "Build context archive task for node '{node_name}' failed: {error}"
                ))
            })??;
        let size = archive
            .as_file()
            .metadata()
            .map_err(BuilderError::IoError)?
            .len();
        let file = tokio::fs::File::from_std(archive.reopen().map_err(BuilderError::IoError)?);
        let stream = tokio_util::codec::FramedRead::new(file, tokio_util::codec::BytesCodec::new());
        let body = reqwest::Body::wrap_stream(stream.map_ok(|bytes| bytes.freeze()));
        let context = reqwest::multipart::Part::stream_with_length(body, size)
            .file_name("context.tar")
            .mime_str("application/x-tar")
            .map_err(|error| {
                BuilderError::Other(format!("Cannot encode worker build context: {error}"))
            })?;
        let spec_json = serde_json::to_string(&spec).map_err(|error| {
            BuilderError::Other(format!("Cannot encode worker build spec: {error}"))
        })?;
        let form = reqwest::multipart::Form::new()
            .text("spec", spec_json)
            .part("context", context);
        let url = format!("{}/agent/images/build", self.agent_url);
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .timeout(IMAGE_BUILD_REQUEST_TIMEOUT)
            .multipart(form)
            .send()
            .await
            .map_err(|error| {
                BuilderError::Other(format!(
                    "Cannot start build on node '{}': {error}",
                    self.node_name
                ))
            })?;
        // The archive must remain alive until reqwest has uploaded its file.
        drop(archive);
        if !response.status().is_success() {
            let status = response.status();
            let detail = agent_error_detail(response).await;
            return Err(BuilderError::Other(format!(
                "Worker '{}' refused image build (HTTP {status}): {detail}",
                self.node_name
            )));
        }
        let mut chunks = response.bytes_stream();
        let mut pending = Vec::new();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|error| {
                BuilderError::Other(format!(
                    "Lost build stream from node '{}': {error}",
                    self.node_name
                ))
            })?;
            pending.extend_from_slice(&chunk);
            while let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
                if end > MAX_BUILD_EVENT_BYTES {
                    return Err(BuilderError::Other(format!(
                        "Node '{}' sent an oversized build event",
                        self.node_name
                    )));
                }
                let line: Vec<u8> = pending.drain(..=end).collect();
                if let Some(result) =
                    dispatch_build_event(&line[..end], log_callback.as_ref()).await?
                {
                    return Ok(result);
                }
            }
            if pending.len() > MAX_BUILD_EVENT_BYTES {
                return Err(BuilderError::Other(format!(
                    "Node '{}' sent an oversized build event",
                    self.node_name
                )));
            }
        }
        Err(BuilderError::Other(format!(
            "Build stream from node '{}' ended without a terminal result",
            self.node_name
        )))
    }

    async fn import_image(&self, image_path: PathBuf, tag: &str) -> Result<String, BuilderError> {
        tracing::info!(
            node = %self.node_name,
            image = %tag,
            "Transferring image tar to remote node"
        );

        let file = tokio::fs::File::open(&image_path).await.map_err(|e| {
            BuilderError::IoError(std::io::Error::new(
                e.kind(),
                format!("Failed to open image tar {:?}: {}", image_path, e),
            ))
        })?;

        let file_size = file
            .metadata()
            .await
            .map_err(|source| {
                BuilderError::IoError(std::io::Error::new(
                    source.kind(),
                    format!(
                        "Failed to read image archive metadata for {:?}: {source}",
                        image_path
                    ),
                ))
            })?
            .len();

        let stream = tokio_util::codec::FramedRead::new(file, tokio_util::codec::BytesCodec::new());
        let body = reqwest::Body::wrap_stream(stream.map_ok(|b| b.freeze()));

        let url = format!("{}/agent/images/import", self.agent_url);
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .timeout(IMAGE_IMPORT_REQUEST_TIMEOUT)
            .header("content-type", "application/x-tar")
            .header(reqwest::header::CONTENT_LENGTH, file_size)
            .header("x-image-tag", tag)
            .body(body)
            .send()
            .await
            .map_err(|e| {
                BuilderError::Other(format!(
                    "Failed to transfer image to node {} at {}: {}",
                    self.node_name, url, e
                ))
            })?;

        let status = response.status();
        let resp_body: AgentResponse<String> = response.json().await.map_err(|e| {
            BuilderError::Other(format!(
                "Invalid response from node {} during image import: {}",
                self.node_name, e
            ))
        })?;

        if !resp_body.success {
            return Err(BuilderError::Other(format!(
                "Image import failed on node {} ({}): {}",
                self.node_name,
                status,
                resp_body.error.unwrap_or_default()
            )));
        }

        tracing::info!(
            node = %self.node_name,
            image = %tag,
            size_mb = format!("{:.1}", file_size as f64 / 1_048_576.0),
            "Image transferred successfully"
        );

        Ok(resp_body.data.unwrap_or_else(|| tag.to_string()))
    }

    async fn export_image_stream(&self, image: &str) -> Result<ImageImportStream, BuilderError> {
        RemoteNodeDeployer::export_image_stream(self, image).await
    }

    /// Forward a byte stream to `POST /agent/images/import` without staging
    /// it on local disk. The agent enforces its own size limit on the stream.
    async fn import_image_stream(
        &self,
        stream: ImageImportStream,
        tag: &str,
    ) -> Result<String, BuilderError> {
        let url = format!("{}/agent/images/import", self.agent_url);
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .timeout(IMAGE_IMPORT_REQUEST_TIMEOUT)
            .header("content-type", "application/x-tar")
            .header("x-image-tag", tag)
            .body(reqwest::Body::wrap_stream(stream))
            .send()
            .await
            .map_err(|error| {
                BuilderError::Other(format!(
                    "Failed to stream image '{tag}' to node {} at {url}: {error}",
                    self.node_name
                ))
            })?;
        let status = response.status();
        if !status.is_success() {
            let detail = agent_error_detail(response).await;
            return Err(BuilderError::Other(format!(
                "Image import failed on node {} ({status}): {detail}",
                self.node_name
            )));
        }
        let body: AgentResponse<String> = response.json().await.map_err(|error| {
            BuilderError::Other(format!(
                "Invalid response from node {} during image import: {error}",
                self.node_name
            ))
        })?;
        if !body.success {
            return Err(BuilderError::Other(format!(
                "Image import failed on node {} ({status}): {}",
                self.node_name,
                body.error.unwrap_or_default()
            )));
        }
        Ok(body.data.unwrap_or_else(|| tag.to_string()))
    }

    async fn save_image(&self, _image_name: &str, _output_path: &Path) -> Result<(), BuilderError> {
        Err(BuilderError::Other(
            "Save image not supported on remote nodes — images are saved on the control plane"
                .into(),
        ))
    }

    async fn extract_from_image(
        &self,
        _image_name: &str,
        _source_path: &str,
        _destination_path: &Path,
    ) -> Result<(), BuilderError> {
        Err(BuilderError::Other(
            "Extract from image not supported on remote nodes".into(),
        ))
    }

    async fn list_images(&self) -> Result<Vec<String>, BuilderError> {
        Err(BuilderError::Other(
            "List images not supported on remote nodes".into(),
        ))
    }

    async fn remove_image(&self, _image_name: &str) -> Result<(), BuilderError> {
        Err(BuilderError::Other(
            "Remove image not supported on remote nodes".into(),
        ))
    }

    async fn inspect_image(&self, _image_name: &str) -> Result<ImageInfo, BuilderError> {
        Err(BuilderError::Other(
            "Inspect image not supported on remote nodes".into(),
        ))
    }

    fn get_native_platform(&self) -> String {
        // Known from the node row (or a health-endpoint refresh). Falling back
        // to the control plane's own platform when it isn't known keeps the
        // historical behaviour for pre-multi-arch agents: assume compatible
        // rather than block the deploy. Callers that need certainty check
        // `platform()` for `None` and warn.
        self.platform
            .get()
            .cloned()
            .unwrap_or_else(crate::platform::native_platform)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_context_archive_excludes_git_metadata() {
        let source = tempfile::tempdir().expect("source");
        std::fs::write(source.path().join("Dockerfile"), "FROM scratch\n").expect("Dockerfile");
        std::fs::create_dir(source.path().join(".git")).expect("git metadata");
        std::fs::write(
            source.path().join(".git/config"),
            "credential=must-not-transfer",
        )
        .expect("git config");
        let request = BuildRequest {
            image_name: "app:latest".to_string(),
            context_path: source.path().to_path_buf(),
            dockerfile_path: None,
            build_args: Default::default(),
            build_args_buildkit: Default::default(),
            platform: Some("linux/amd64".to_string()),
            log_path: source.path().join("build.log"),
        };
        let (archive, spec) = prepare_build_context(&request).expect("archive context");
        assert_eq!(spec.dockerfile, "Dockerfile");
        let mut tar = tar::Archive::new(archive.reopen().expect("reopen archive"));
        let paths: Vec<_> = tar
            .entries()
            .expect("entries")
            .map(|entry| entry.expect("entry").path().expect("path").into_owned())
            .collect();
        assert!(paths.iter().any(|path| path == Path::new("Dockerfile")));
        assert!(!paths
            .iter()
            .any(|path| path.to_string_lossy().contains(".git")));
    }

    #[test]
    fn worker_build_discards_unconsumed_sensitive_arguments() {
        let source = tempfile::tempdir().expect("source");
        std::fs::write(
            source.path().join("Dockerfile"),
            "FROM scratch\nCOPY app /app\n",
        )
        .expect("Dockerfile");
        std::fs::write(source.path().join("app"), "hello").expect("app");
        let mut request = context_request(source.path(), None);
        request
            .build_args
            .insert("TEMPS_API_TOKEN".into(), "must-not-transfer".into());
        let (archive, spec) = prepare_build_context(&request).expect("archive context");
        let spec_json = serde_json::to_string(&spec).expect("serialize spec");
        assert!(!spec_json.contains("must-not-transfer"));
        let mut tar = tar::Archive::new(archive.reopen().expect("reopen archive"));
        for entry in tar.entries().expect("entries") {
            let mut entry = entry.expect("entry");
            let mut body = String::new();
            use std::io::Read;
            entry.read_to_string(&mut body).expect("entry body");
            assert!(!body.contains("must-not-transfer"));
        }
    }

    #[test]
    fn worker_build_refuses_dockerfile_arg_with_sensitive_arguments() {
        let source = tempfile::tempdir().expect("source");
        std::fs::write(
            source.path().join("Dockerfile"),
            "FROM scratch\nARG TOKEN\n",
        )
        .expect("Dockerfile");
        let mut request = context_request(source.path(), None);
        request
            .build_args
            .insert("TOKEN".into(), "must-not-transfer".into());
        match prepare_build_context(&request) {
            Err(BuilderError::InvalidContext(message)) => {
                assert!(message.contains("ARG instructions"));
                assert!(!message.contains("must-not-transfer"));
            }
            other => panic!("expected InvalidContext, got {:?}", other.map(|_| ())),
        }
        assert!(dockerfile_declares_build_arg(
            "FROM base\nONBUILD ARG TOKEN\n"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn worker_context_archive_rejects_symlinks() {
        let source = tempfile::tempdir().expect("source");
        std::fs::write(source.path().join("Dockerfile"), "FROM scratch\n").expect("Dockerfile");
        std::os::unix::fs::symlink("/etc/passwd", source.path().join("linked")).expect("symlink");
        let request = BuildRequest {
            image_name: "app:latest".to_string(),
            context_path: source.path().to_path_buf(),
            dockerfile_path: None,
            build_args: Default::default(),
            build_args_buildkit: Default::default(),
            platform: None,
            log_path: source.path().join("build.log"),
        };
        assert!(matches!(
            prepare_build_context(&request),
            Err(BuilderError::InvalidContext(_))
        ));
    }

    fn archive_paths(request: &BuildRequest) -> Vec<String> {
        let (archive, _spec) = prepare_build_context(request).expect("archive context");
        let mut tar = tar::Archive::new(archive.reopen().expect("reopen archive"));
        tar.entries()
            .expect("entries")
            .map(|entry| {
                entry
                    .expect("entry")
                    .path()
                    .expect("path")
                    .to_string_lossy()
                    .trim_end_matches('/')
                    .to_string()
            })
            .collect()
    }

    fn context_request(root: &Path, dockerfile: Option<PathBuf>) -> BuildRequest {
        BuildRequest {
            image_name: "app:latest".to_string(),
            context_path: root.to_path_buf(),
            dockerfile_path: dockerfile,
            build_args: Default::default(),
            build_args_buildkit: Default::default(),
            platform: None,
            log_path: root.join("build.log"),
        }
    }

    /// Files the project keeps out of its image must not leave the control
    /// plane at all — `.env` files are the canonical case.
    #[cfg(unix)]
    #[test]
    fn worker_context_archive_applies_dockerignore() {
        let source = tempfile::tempdir().expect("source");
        let root = source.path();
        std::fs::write(root.join("Dockerfile"), "FROM scratch\n").expect("Dockerfile");
        std::fs::write(
            root.join(".dockerignore"),
            ".env\nnode_modules\nDockerfile\nconfig\n!config/public.json\n",
        )
        .expect("dockerignore");
        std::fs::write(root.join(".env"), "SECRET=must-not-transfer").expect(".env");
        std::fs::write(root.join("main.js"), "console.log(1)").expect("source");
        std::fs::create_dir(root.join("node_modules")).expect("node_modules");
        // An ignored symlink is skipped rather than failing the build.
        std::os::unix::fs::symlink("/etc/passwd", root.join("node_modules/linked"))
            .expect("symlink");
        std::fs::create_dir(root.join("config")).expect("config");
        std::fs::write(root.join("config/private.json"), "{}").expect("private");
        std::fs::write(root.join("config/public.json"), "{}").expect("public");

        let paths = archive_paths(&context_request(root, None));

        for kept in [
            "Dockerfile",
            ".dockerignore",
            "main.js",
            "config/public.json",
        ] {
            assert!(
                paths.iter().any(|path| path == kept),
                "{kept} missing: {paths:?}"
            );
        }
        for dropped in [".env", "node_modules", "config/private.json"] {
            assert!(
                !paths.iter().any(|path| path.starts_with(dropped)),
                "{dropped} transferred: {paths:?}"
            );
        }
    }

    /// BuildKit's `<Dockerfile>.dockerignore` wins over the root file.
    #[test]
    fn worker_context_archive_prefers_dockerfile_specific_ignore() {
        let source = tempfile::tempdir().expect("source");
        let root = source.path();
        std::fs::create_dir(root.join("deploy")).expect("deploy dir");
        std::fs::write(root.join("deploy/app.Dockerfile"), "FROM scratch\n").expect("Dockerfile");
        std::fs::write(
            root.join("deploy/app.Dockerfile.dockerignore"),
            "fixtures\n",
        )
        .expect("specific ignore");
        std::fs::write(root.join(".dockerignore"), "src\n").expect("root ignore");
        std::fs::create_dir(root.join("src")).expect("src");
        std::fs::write(root.join("src/main.rs"), "fn main() {}").expect("main");
        std::fs::create_dir(root.join("fixtures")).expect("fixtures");
        std::fs::write(root.join("fixtures/dump.sql"), "--").expect("fixture");

        let paths = archive_paths(&context_request(
            root,
            Some(PathBuf::from("deploy/app.Dockerfile")),
        ));

        assert!(paths.iter().any(|path| path == "src/main.rs"), "{paths:?}");
        assert!(
            paths.iter().any(|path| path == "deploy/app.Dockerfile"),
            "{paths:?}"
        );
        assert!(
            !paths.iter().any(|path| path.starts_with("fixtures")),
            "{paths:?}"
        );
    }

    #[test]
    fn worker_context_archive_rejects_invalid_dockerignore() {
        let source = tempfile::tempdir().expect("source");
        std::fs::write(source.path().join("Dockerfile"), "FROM scratch\n").expect("Dockerfile");
        std::fs::write(source.path().join(".dockerignore"), "[broken\n").expect("ignore");
        match prepare_build_context(&context_request(source.path(), None)) {
            Err(BuilderError::InvalidContext(message)) => {
                assert!(message.contains(".dockerignore line 1"), "{message}")
            }
            other => panic!("expected InvalidContext, got {:?}", other.map(|_| ())),
        }
    }

    /// Serve one raw HTTP response and return the request head it received.
    async fn spawn_one_shot_agent(
        status_line: &'static str,
        content_type: &'static str,
        body: &'static [u8],
    ) -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock agent");
        let address = listener.local_addr().expect("mock agent address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            // Drain the whole request (a build uploads a multipart body) so
            // the client is not reset mid-upload before it reads the reply.
            let mut request = Vec::new();
            let mut buf = vec![0_u8; 4096];
            while let Ok(Ok(n)) =
                tokio::time::timeout(std::time::Duration::from_millis(200), stream.read(&mut buf))
                    .await
            {
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
            }
            let head = format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).await.expect("write head");
            stream.write_all(body).await.expect("write body");
            let _ = stream.shutdown().await;
            String::from_utf8_lossy(&request).to_string()
        });
        (format!("http://{address}"), server)
    }

    #[tokio::test]
    async fn export_image_stream_yields_the_agent_tar_bytes() {
        let (url, server) = spawn_one_shot_agent("200 OK", "application/x-tar", b"tar-bytes").await;
        let deployer =
            RemoteNodeDeployer::new(url, "token".into(), "builder-1".into()).expect("deployer");

        let stream = deployer
            .export_image_stream("temps-app:abc")
            .await
            .expect("export starts");
        let bytes: Vec<u8> = stream
            .map_ok(|chunk| chunk.to_vec())
            .try_concat()
            .await
            .expect("stream completes");

        assert_eq!(bytes, b"tar-bytes");
        let request = server.await.expect("server task");
        assert!(
            request.starts_with("GET /agent/images/export?image=temps-app%3Aabc "),
            "{request}"
        );
        assert!(request
            .to_lowercase()
            .contains("authorization: bearer token"));
    }

    #[tokio::test]
    async fn export_image_stream_maps_missing_image_to_image_not_found() {
        let (url, _server) = spawn_one_shot_agent(
            "404 Not Found",
            "application/json",
            br#"{"success":false,"data":null,"error":"No such image"}"#,
        )
        .await;
        let deployer =
            RemoteNodeDeployer::new(url, "token".into(), "builder-1".into()).expect("deployer");

        match deployer.export_image_stream("temps-app:abc").await {
            Err(BuilderError::ImageNotFound(message)) => {
                assert!(message.contains("builder-1"), "{message}")
            }
            Err(other) => panic!("expected ImageNotFound, got {other}"),
            Ok(_) => panic!("expected ImageNotFound, got a stream"),
        }
    }

    #[tokio::test]
    async fn worker_build_refusal_quotes_the_agent_reason() {
        let (url, _server) = spawn_one_shot_agent(
            "400 Bad Request",
            "application/json",
            br#"{"success":false,"data":null,"error":"Dockerfile 'Dockerfile' is absent from the uploaded context"}"#,
        )
        .await;
        let deployer =
            RemoteNodeDeployer::new(url, "token".into(), "builder-1".into()).expect("deployer");
        let source = tempfile::tempdir().expect("source");
        std::fs::write(source.path().join("Dockerfile"), "FROM scratch\n").expect("Dockerfile");

        let error = deployer
            .build_image(context_request(source.path(), None))
            .await
            .expect_err("agent refused");

        let message = error.to_string();
        assert!(message.contains("HTTP 400"), "{message}");
        assert!(
            message.contains("absent from the uploaded context"),
            "{message}"
        );
    }

    #[test]
    fn test_remote_node_deployer_creation() {
        let deployer = RemoteNodeDeployer::new(
            "https://10.100.0.2:3100".to_string(),
            "test-token".to_string(),
            "worker-1".to_string(),
        );
        assert!(deployer.is_ok());
        let deployer = deployer.unwrap();
        assert_eq!(deployer.node_name(), "worker-1");
        assert_eq!(deployer.agent_url(), "https://10.100.0.2:3100");
    }

    #[test]
    fn test_remote_node_deployer_accessors() {
        let deployer = RemoteNodeDeployer::new(
            "https://worker-3.internal:3100".to_string(),
            "secret-token".to_string(),
            "worker-3".to_string(),
        )
        .unwrap();
        assert_eq!(deployer.node_name(), "worker-3");
        assert_eq!(deployer.agent_url(), "https://worker-3.internal:3100");
    }

    #[tokio::test]
    async fn remove_container_maps_agent_not_found_to_typed_error() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock agent");
        let address = listener.local_addr().expect("mock agent address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).await.expect("read request");
            let body = r#"{"success":false,"data":null,"error":"container missing"}"#;
            let response = format!(
                "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        });

        let deployer = RemoteNodeDeployer::new(
            format!("http://{address}"),
            "test-token".to_string(),
            "worker-test".to_string(),
        )
        .expect("create remote deployer");
        let result = deployer.remove_container("already-gone").await;
        assert!(matches!(result, Err(DeployerError::ContainerNotFound(_))));
        server.await.expect("mock agent task");
    }

    #[tokio::test]
    async fn get_container_info_maps_agent_not_found_to_typed_error() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock agent");
        let address = listener.local_addr().expect("mock agent address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).await.expect("read request");
            let body = r#"{"success":false,"data":null,"error":"container missing"}"#;
            let response = format!(
                "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        });

        let deployer = RemoteNodeDeployer::new(
            format!("http://{address}"),
            "test-token".to_string(),
            "worker-test".to_string(),
        )
        .expect("create remote deployer");
        let result = deployer.get_container_info("already-gone").await;
        assert!(matches!(result, Err(DeployerError::ContainerNotFound(_))));
        server.await.expect("mock agent task");
    }

    /// Live **REAL DEPLOYMENT** over mTLS: drives the production
    /// `RemoteNodeDeployer::deploy_container` to actually create + start a
    /// container on the worker's Docker through the mutual-TLS channel — the
    /// genuine end-to-end deploy path, not just a control-plane round-trip.
    /// Gated on `TEMPS_MTLS_DEPLOY_IMAGE` (the image, which MUST already be
    /// present in the worker's Docker, e.g. pre-pulled) plus the same
    /// `TEMPS_MTLS_*` connection env. Run inside a cluster container.
    #[tokio::test]
    async fn test_mtls_real_deploy_live() {
        let image = match std::env::var("TEMPS_MTLS_DEPLOY_IMAGE") {
            Ok(i) => i,
            Err(_) => {
                eprintln!("TEMPS_MTLS_DEPLOY_IMAGE not set — skipping real mTLS deploy test");
                return;
            }
        };
        let (url, token, cert, key, ca) = match (
            std::env::var("TEMPS_MTLS_AGENT_URL"),
            std::env::var("TEMPS_MTLS_TOKEN"),
            std::env::var("TEMPS_MTLS_CERT"),
            std::env::var("TEMPS_MTLS_KEY"),
            std::env::var("TEMPS_MTLS_CA"),
        ) {
            (Ok(u), Ok(t), Ok(c), Ok(k), Ok(a)) => (u, t, c, k, a),
            _ => {
                eprintln!("TEMPS_MTLS_* not set — skipping real mTLS deploy test");
                return;
            }
        };
        let cert_pem = std::fs::read_to_string(&cert).expect("read client cert PEM");
        let key_pem = std::fs::read_to_string(&key).expect("read client key PEM");
        let ca_pem = std::fs::read_to_string(&ca).expect("read cluster CA PEM");
        let identity = format!("{}\n{}", cert_pem.trim(), key_pem.trim());

        let deployer = RemoteNodeDeployer::new_mtls(
            url.clone(),
            token,
            "mtls-deploy-probe".to_string(),
            &identity,
            &ca_pem,
        )
        .expect("build mTLS deployer");

        // Deployed containers run with cap_drop:ALL + no-new-privileges, so the
        // probe image must be unprivileged-friendly (no startup chown, high
        // port). Port + command are env-configurable to fit such an image.
        let container_port: u16 = std::env::var("TEMPS_MTLS_DEPLOY_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(80);
        let command = std::env::var("TEMPS_MTLS_DEPLOY_CMD")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|s| s.split(' ').map(|x| x.to_string()).collect::<Vec<_>>());

        let name = "mtls-deploy-probe".to_string();
        let req = DeployRequest {
            image_name: image.clone(),
            container_name: name.clone(),
            environment_vars: std::collections::HashMap::new(),
            secrets: std::collections::HashMap::new(),
            port_mappings: vec![crate::PortMapping {
                host_port: 18080,
                container_port,
                protocol: crate::Protocol::Tcp,
                host_ip: None,
            }],
            network_name: None,
            extra_networks: Vec::new(),
            resource_limits: crate::ResourceLimits::default(),
            restart_policy: crate::RestartPolicy::Never,
            log_path: std::path::PathBuf::from("/tmp/mtls-deploy-probe.log"),
            command,
            log_config: None,
            labels: std::collections::HashMap::new(),
            project_slug: None,
            control_plane_grants_socket: false,
        };

        let result = deployer
            .deploy_container(req)
            .await
            .expect("deploy_container over mTLS must succeed");
        eprintln!(
            "✓ REAL deploy over mTLS: image={} container_id={} status={:?} host_port={}",
            image, result.container_id, result.status, result.host_port
        );
        assert!(
            !result.container_id.is_empty(),
            "expected a real container id"
        );
    }

    /// Live end-to-end check of the control-plane reqwest+rustls **mutual TLS**
    /// client against a real agent serving mTLS (ADR-020 WS-2.1). Unlike the
    /// curl-based `verify-mtls.sh` harness (which proves the *agent* side), this
    /// drives the exact production client path — `new_mtls` loading a CA-signed
    /// client identity, pinning the cluster CA, completing the rustls handshake,
    /// and round-tripping an authenticated request. Skips gracefully unless the
    /// `TEMPS_MTLS_*` env is set (so normal `cargo test` runs are unaffected);
    /// run it inside a cluster container that can reach the agent. A client cert
    /// SAN is irrelevant here (client certs aren't hostname-checked), so any
    /// CA-signed leaf faithfully stands in for the control plane's identity.
    #[tokio::test]
    async fn test_mtls_deploy_channel_live() {
        let (url, token, cert, key, ca) = match (
            std::env::var("TEMPS_MTLS_AGENT_URL"),
            std::env::var("TEMPS_MTLS_TOKEN"),
            std::env::var("TEMPS_MTLS_CERT"),
            std::env::var("TEMPS_MTLS_KEY"),
            std::env::var("TEMPS_MTLS_CA"),
        ) {
            (Ok(u), Ok(t), Ok(c), Ok(k), Ok(a)) => (u, t, c, k, a),
            _ => {
                eprintln!("TEMPS_MTLS_* not set — skipping live mTLS deploy-channel test");
                return;
            }
        };

        let cert_pem = std::fs::read_to_string(&cert).expect("read client cert PEM");
        let key_pem = std::fs::read_to_string(&key).expect("read client key PEM");
        let ca_pem = std::fs::read_to_string(&ca).expect("read cluster CA PEM");
        // reqwest's PEM Identity wants the cert chain followed by the key — the
        // same layout `cluster_ca::cp_client_identity` produces in production.
        let identity = format!("{}\n{}", cert_pem.trim(), key_pem.trim());

        let deployer = RemoteNodeDeployer::new_mtls(
            url.clone(),
            token,
            "mtls-live-test".to_string(),
            &identity,
            &ca_pem,
        )
        .expect("build mTLS deployer");

        // A lightweight authenticated read that fully round-trips the mutual-TLS
        // channel: TLS handshake (server cert validated against the pinned CA +
        // our client cert presented), bearer auth, JSON response.
        let containers = deployer
            .list_containers()
            .await
            .expect("list_containers over mTLS must succeed");
        eprintln!(
            "✓ mTLS deploy channel live: agent {} returned {} container(s)",
            url,
            containers.len()
        );
    }

    #[tokio::test]
    async fn test_pause_container_not_supported() {
        let deployer = RemoteNodeDeployer::new(
            "https://10.100.0.2:3100".to_string(),
            "token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap();
        let result = deployer.pause_container("test-container").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DeployerError::Other(_)));
    }

    #[tokio::test]
    async fn test_resume_container_not_supported() {
        let deployer = RemoteNodeDeployer::new(
            "https://10.100.0.2:3100".to_string(),
            "token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap();
        let result = deployer.resume_container("test-container").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DeployerError::Other(_)));
    }

    #[tokio::test]
    async fn test_get_container_stats_returns_network_error_for_unreachable_agent() {
        let deployer = RemoteNodeDeployer::new(
            "https://10.100.0.2:3100".to_string(),
            "token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap();
        let result = deployer.get_container_stats("test-container").await;
        // Stats now hit the real `/agent/containers/{id}/stats` endpoint.
        // With an unreachable address the call must surface a network error
        // (used to be a hard-coded "not supported" before the endpoint
        // existed).
        assert!(matches!(
            result.unwrap_err(),
            DeployerError::NetworkError(_)
        ));
    }

    #[tokio::test]
    async fn test_list_containers_not_supported() {
        let deployer = RemoteNodeDeployer::new(
            "https://10.100.0.2:3100".to_string(),
            "token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap();
        let result = deployer.list_containers().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_worker_build_rejects_unreviewed_build_arguments() {
        let deployer = RemoteNodeDeployer::new(
            "https://10.100.0.2:3100".to_string(),
            "token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap();
        let source = tempfile::tempdir().unwrap();
        std::fs::write(
            source.path().join("Dockerfile"),
            "FROM scratch\nARG TOKEN\n",
        )
        .unwrap();
        let result = deployer
            .build_image(BuildRequest {
                image_name: "test:latest".to_string(),
                context_path: source.path().to_path_buf(),
                dockerfile_path: None,
                build_args: std::collections::HashMap::from([("TOKEN".into(), "secret".into())]),
                build_args_buildkit: std::collections::HashMap::new(),
                platform: None,
                log_path: source.path().join("build.log"),
            })
            .await;
        assert!(matches!(result, Err(BuilderError::InvalidContext(_))));
    }

    #[tokio::test]
    async fn test_import_image_missing_file_returns_error() {
        let deployer = RemoteNodeDeployer::new(
            "https://10.100.0.2:3100".to_string(),
            "token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap();
        let result = deployer
            .import_image(PathBuf::from("/tmp/nonexistent-image.tar"), "test:latest")
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_save_image_not_supported() {
        let deployer = RemoteNodeDeployer::new(
            "https://10.100.0.2:3100".to_string(),
            "token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap();
        let result = deployer
            .save_image("test:latest", Path::new("/tmp/out.tar"))
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn test_get_native_platform() {
        // No platform known yet: fall back to the control plane's own, which
        // is what the historical hardcoded value effectively assumed.
        let deployer = RemoteNodeDeployer::new(
            "https://10.100.0.2:3100".to_string(),
            "token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap();
        assert_eq!(
            deployer.get_native_platform(),
            crate::platform::native_platform()
        );
        assert_eq!(deployer.platform(), None);
    }

    /// The whole point of the change: a node's platform must be reported
    /// truthfully, not assumed to be the control plane's.
    #[test]
    fn test_with_platform_reports_the_nodes_architecture() {
        let deployer = RemoteNodeDeployer::new(
            "https://10.100.0.2:3100".to_string(),
            "token".to_string(),
            "worker-arm".to_string(),
        )
        .unwrap()
        .with_platform(Some("linux/arm64".to_string()));

        assert_eq!(deployer.get_native_platform(), "linux/arm64");
        assert_eq!(deployer.platform().as_deref(), Some("linux/arm64"));
    }

    #[test]
    fn test_with_platform_canonicalizes_and_ignores_blanks() {
        let make = |platform: Option<&str>| {
            RemoteNodeDeployer::new(
                "https://10.100.0.2:3100".to_string(),
                "token".to_string(),
                "worker-1".to_string(),
            )
            .unwrap()
            .with_platform(platform.map(|p| p.to_string()))
        };

        // Docker's kernel spelling is normalized to the OCI one.
        assert_eq!(
            make(Some("linux/aarch64")).platform().as_deref(),
            Some("linux/arm64")
        );
        // Blank/whitespace means "unknown", not a platform named "".
        assert_eq!(make(Some("   ")).platform(), None);
        assert_eq!(make(None).platform(), None);
    }

    /// Spawn a throwaway HTTP server that answers `GET /agent/health` like a
    /// real agent would. Returns its base URL.
    ///
    /// Hand-rolled rather than pulled from a framework: this crate has no HTTP
    /// server dependency and one canned response doesn't justify adding one.
    async fn spawn_fake_agent(body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let body = body.to_string();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    // Read (and discard) the request head; we only ever serve
                    // one route.
                    let mut buf = [0u8; 1024];
                    let _ = socket.read(&mut buf).await;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });

        format!("http://{}", addr)
    }

    #[tokio::test]
    async fn test_refresh_platform_reads_agent_health() {
        // A node that never reported its architecture at registration time —
        // the pre-multi-arch agent case — can still be identified by asking it.
        let url = spawn_fake_agent(
            r#"{"success":true,"data":{"cpu_percent":1.0,"memory_used_bytes":1,"memory_total_bytes":2,"disk_used_bytes":1,"disk_total_bytes":2,"running_containers":0,"platform":"linux/aarch64"},"error":null}"#,
        )
        .await;

        let deployer =
            RemoteNodeDeployer::new(url, "token".to_string(), "worker-arm".to_string()).unwrap();

        assert_eq!(
            deployer.refresh_platform().await.as_deref(),
            Some("linux/arm64")
        );
        // Cached afterwards, so the deploy path pays at most one round-trip.
        assert_eq!(deployer.platform().as_deref(), Some("linux/arm64"));
        assert_eq!(deployer.get_native_platform(), "linux/arm64");
    }

    #[tokio::test]
    async fn test_refresh_platform_returns_none_when_agent_omits_it() {
        // An agent too old to report a platform must leave us with `None` —
        // "unknown" — never a guess that would silently pass a compatibility
        // check.
        let url = spawn_fake_agent(
            r#"{"success":true,"data":{"cpu_percent":1.0,"memory_used_bytes":1,"memory_total_bytes":2,"disk_used_bytes":1,"disk_total_bytes":2,"running_containers":0,"platform":""},"error":null}"#,
        )
        .await;

        let deployer =
            RemoteNodeDeployer::new(url, "token".to_string(), "legacy-worker".to_string()).unwrap();

        assert_eq!(deployer.refresh_platform().await, None);
        assert_eq!(deployer.platform(), None);
    }

    #[tokio::test]
    async fn test_refresh_platform_returns_none_when_agent_unreachable() {
        let deployer = RemoteNodeDeployer::new(
            "http://127.0.0.1:1".to_string(),
            "token".to_string(),
            "dead-worker".to_string(),
        )
        .unwrap();

        assert_eq!(deployer.refresh_platform().await, None);
    }

    #[tokio::test]
    async fn test_with_platform_wins_over_agent_query() {
        // When the node row already carries the architecture we must not spend
        // a round-trip; point the deployer at a server that would answer with
        // a different value and verify it is never consulted.
        let url = spawn_fake_agent(
            r#"{"success":true,"data":{"cpu_percent":1.0,"memory_used_bytes":1,"memory_total_bytes":2,"disk_used_bytes":1,"disk_total_bytes":2,"running_containers":0,"platform":"linux/amd64"},"error":null}"#,
        )
        .await;

        let deployer = RemoteNodeDeployer::new(url, "token".to_string(), "worker-arm".to_string())
            .unwrap()
            .with_platform(Some("linux/arm64".to_string()));

        assert_eq!(
            deployer.refresh_platform().await.as_deref(),
            Some("linux/arm64")
        );
    }

    #[tokio::test]
    async fn test_deploy_container_unreachable_returns_network_error() {
        let deployer = RemoteNodeDeployer::new(
            "https://192.0.2.1:3100".to_string(), // Non-routable address
            "token".to_string(),
            "test-node".to_string(),
        )
        .unwrap();

        let request = DeployRequest {
            image_name: "nginx:latest".to_string(),
            container_name: "test-container".to_string(),
            environment_vars: std::collections::HashMap::new(),
            secrets: std::collections::HashMap::new(),
            port_mappings: vec![],
            network_name: None,
            extra_networks: Vec::new(),
            resource_limits: crate::ResourceLimits::default(),
            restart_policy: crate::RestartPolicy::default(),
            log_path: PathBuf::from("/tmp/deploy.log"),
            command: None,
            log_config: None,
            labels: std::collections::HashMap::new(),
            project_slug: None,
            control_plane_grants_socket: false,
        };

        let result = deployer.deploy_container(request).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            DeployerError::NetworkError(msg) => {
                assert!(
                    msg.contains("test-node"),
                    "Error should mention node name: {}",
                    msg
                );
            }
            other => panic!("Expected NetworkError, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn pull_image_from_registry_returns_image_id_on_success() {
        let url = spawn_fake_agent(
            r#"{"success":true,"data":{"image_id":"sha256:abc123","digest":"sha256:def456"},"error":null}"#,
        )
        .await;

        let deployer =
            RemoteNodeDeployer::new(url, "token".to_string(), "worker-1".to_string()).unwrap();

        let image_id = deployer
            .pull_image_from_registry("ghcr.io/acme/app:v1", None)
            .await
            .expect("pull should succeed against a mock agent reporting success");

        assert_eq!(image_id, "sha256:abc123");
    }

    #[tokio::test]
    async fn pull_image_from_registry_sends_image_and_credentials_in_body() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock agent");
        let address = listener.local_addr().expect("mock agent address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let mut buf = vec![0_u8; 4096];
            let n = stream.read(&mut buf).await.expect("read request");
            let request_text = String::from_utf8_lossy(&buf[..n]).to_string();

            let body = r#"{"success":true,"data":{"image_id":"sha256:private","digest":null},"error":null}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response");
            request_text
        });

        let deployer = RemoteNodeDeployer::new(
            format!("http://{address}"),
            "test-token".to_string(),
            "worker-private".to_string(),
        )
        .expect("create remote deployer");

        let credentials = RemotePullCredentials {
            username: Some("deployer".to_string()),
            password: Some("hunter2".to_string()),
            identity_token: None,
            server_address: Some("registry.internal".to_string()),
        };

        let image_id = deployer
            .pull_image_from_registry("registry.internal/app:v2", Some(credentials))
            .await
            .expect("pull should succeed");
        assert_eq!(image_id, "sha256:private");

        let request_text = server.await.expect("mock agent task");
        assert!(
            request_text.contains("POST /agent/images/pull"),
            "expected the pull endpoint to be hit, got: {request_text}"
        );
        assert!(request_text.contains("registry.internal/app:v2"));
        assert!(request_text.contains("\"username\":\"deployer\""));
        assert!(request_text.contains("\"password\":\"hunter2\""));
        assert!(request_text.contains("registry.internal"));
    }

    #[tokio::test]
    async fn pull_image_from_registry_maps_agent_error_to_deployment_failed() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock agent");
        let address = listener.local_addr().expect("mock agent address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let mut buf = [0_u8; 2048];
            let _ = stream.read(&mut buf).await.expect("read request");
            let body = r#"{"success":false,"data":null,"error":"registry authentication failed"}"#;
            let response = format!(
                "HTTP/1.1 422 Unprocessable Entity\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        });

        let deployer = RemoteNodeDeployer::new(
            format!("http://{address}"),
            "test-token".to_string(),
            "worker-1".to_string(),
        )
        .expect("create remote deployer");

        let result = deployer
            .pull_image_from_registry("ghcr.io/private/app:latest", None)
            .await;

        match result {
            Err(DeployerError::DeploymentFailed(msg)) => {
                assert!(
                    msg.contains("registry authentication failed"),
                    "error should surface the agent's message: {msg}"
                );
            }
            other => panic!("Expected DeploymentFailed, got {:?}", other),
        }
        server.await.expect("mock agent task");
    }

    #[tokio::test]
    async fn pull_image_from_registry_unreachable_returns_network_error() {
        let deployer = RemoteNodeDeployer::new(
            "https://192.0.2.1:3100".to_string(), // Non-routable (TEST-NET-1) address
            "token".to_string(),
            "test-node".to_string(),
        )
        .unwrap();

        let result = deployer
            .pull_image_from_registry("ghcr.io/acme/app:v1", None)
            .await;

        match result {
            Err(DeployerError::NetworkError(msg)) => {
                assert!(
                    msg.contains("test-node"),
                    "Error should mention node name: {}",
                    msg
                );
            }
            other => panic!("Expected NetworkError, got {:?}", other),
        }
    }
}
