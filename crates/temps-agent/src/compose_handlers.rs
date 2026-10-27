// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Authenticated Docker Compose endpoints: run a project's stack on this
//! node with the same executor the control plane uses for local stacks.
//!
//! See [`temps_deployer::compose_protocol`] for the request flow. State kept
//! here:
//!
//! - `<data dir>/compose-stacks/<project>/<generation>/source` — the
//!   repository checkout a generation runs from. Compose resolves build
//!   contexts and bind mounts that exist in the repository against it, so the
//!   generation that last started is kept until the next one starts.
//! - `<data dir>/compose-stacks/<project>/current` — that generation's name.
//! - Prepared generations, in memory, until they are started or discarded.
//!   An agent restart forgets them; the control plane's deploy then fails
//!   cleanly while the running stack keeps running.
//!
//! Named volumes and the executor's persistent bind directories
//! (`<data dir>/compose/<project>/binds`) are never removed here. They are the
//! stack's data, which is why the control plane pins a Compose environment to
//! the node that holds them.

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Extension, Multipart},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures::stream;
use temps_auth::permission_guard;
use temps_core::problemdetails::{self, Problem};
use temps_deployer::compose::{
    ComposeDeployRequest, ComposeExecutor, ComposeOutputSink, PreparedComposeDeploy,
};
use temps_deployer::compose_protocol::{
    unpack_compose_stack, validate_stack_name, ComposeAck, ComposeGenerationRequest,
    ComposeHostEvent, ComposeHostFailure, ComposePrepareSpec, ComposePruneRequest,
    ComposeRemoveVerifiedRequest, ComposeStackRequest, RemoteComposeError, MAX_COMPOSE_SPEC_BYTES,
    MAX_COMPOSE_STACK_BYTES,
};
use tokio::io::AsyncWriteExt;

use crate::auth::RequireAgentAuth;
use crate::handlers::{AgentResourceLimits, AgentResponse};

/// Longest a prepare (upload, build, pull) may run once admitted.
const PREPARE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60 * 60);
/// Longest a prepare waits for an image-operation slot.
const PREPARE_QUEUE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10 * 60);
/// A prepared generation nobody started is forgotten after this long.
const PREPARED_TTL: std::time::Duration = std::time::Duration::from_secs(2 * 60 * 60);

#[derive(Debug, thiserror::Error)]
enum AgentComposeError {
    #[error("Invalid worker Compose request: {0}")]
    Invalid(String),
    #[error("Worker Compose upload too large: {0}")]
    TooLarge(String),
    #[error("Worker Compose capacity unavailable: {0}")]
    Unavailable(String),
    #[error("Compose generation '{generation}' of project '{project}' is not prepared on this node; it may have been discarded or the agent restarted since it was prepared. Redeploy to prepare it again")]
    NotPrepared { project: String, generation: String },
    #[error(
        "Compose generation '{generation}' of project '{project}' was prepared to publish its \
         ports on {prepared}, but this node now publishes on {current} (its private address \
         changed, for example when its mesh address was reassigned). It was discarded rather \
         than started on the old address; redeploy to prepare it again"
    )]
    BindAddressMoved {
        project: String,
        generation: String,
        prepared: std::net::IpAddr,
        current: std::net::IpAddr,
    },
    #[error(
        "This node publishes container ports on {address}, a public address: it joined the \
         cluster with a public address and is not on its WireGuard mesh yet. A Compose stack \
         publishes its ports only on a private address, as the control plane keeps them on \
         loopback, so project '{project}' was not {operation}. Deploy again once the node is on \
         the mesh (its mesh status is shown in the node list)"
    )]
    PublicBindAddress {
        project: String,
        operation: &'static str,
        address: std::net::IpAddr,
    },
    #[error("Worker Compose storage operation failed: {0}")]
    Storage(String),
    #[error("Compose operation on project '{project}' failed: {message}")]
    Operation { project: String, message: String },
}

impl From<AgentComposeError> for Problem {
    fn from(error: AgentComposeError) -> Self {
        let (status, title) = match &error {
            AgentComposeError::Invalid(_) => (StatusCode::BAD_REQUEST, "Invalid Compose request"),
            AgentComposeError::TooLarge(_) => {
                (StatusCode::PAYLOAD_TOO_LARGE, "Compose upload too large")
            }
            AgentComposeError::Unavailable(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "Compose capacity unavailable",
            ),
            AgentComposeError::NotPrepared { .. } => {
                (StatusCode::CONFLICT, "Compose generation not prepared")
            }
            AgentComposeError::BindAddressMoved { .. } => (
                StatusCode::CONFLICT,
                "Compose generation prepared for another address",
            ),
            AgentComposeError::PublicBindAddress { .. } => (
                StatusCode::SERVICE_UNAVAILABLE,
                "Node not on its private network yet",
            ),
            AgentComposeError::Storage(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Compose storage failed")
            }
            AgentComposeError::Operation { .. } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Compose operation failed",
            ),
        };
        problemdetails::new(status)
            .with_title(title)
            .with_detail(error.to_string())
    }
}

/// A generation that has been built and pulled and is waiting to start.
struct PreparedGeneration {
    executor: Arc<ComposeExecutor>,
    prepared: PreparedComposeDeploy,
    spec: ComposePrepareSpec,
    repo_dir: Option<PathBuf>,
    prepared_at: std::time::Instant,
}

/// Compose state shared by the handlers.
pub struct ComposeHostState {
    docker: Option<Arc<bollard::Docker>>,
    /// The agent data directory; the executor keeps its files below it.
    data_dir: PathBuf,
    host_bind_address: crate::network_sync::SharedBindAddress,
    prepared: tokio::sync::Mutex<HashMap<(String, String), PreparedGeneration>>,
}

impl ComposeHostState {
    pub fn new(
        docker: Option<bollard::Docker>,
        data_dir: PathBuf,
        host_bind_address: crate::network_sync::SharedBindAddress,
    ) -> Self {
        Self {
            docker: docker.map(Arc::new),
            data_dir,
            host_bind_address,
            prepared: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    fn stacks_dir(&self, project: &str) -> PathBuf {
        self.data_dir.join("compose-stacks").join(project)
    }

    fn generation_dir(&self, project: &str, generation: &str) -> PathBuf {
        self.stacks_dir(project).join(generation)
    }

    fn current_marker(&self, project: &str) -> PathBuf {
        self.stacks_dir(project).join("current")
    }

    async fn current_generation(&self, project: &str) -> Option<String> {
        tokio::fs::read_to_string(self.current_marker(project))
            .await
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| validate_stack_name(value, "generation").is_ok())
    }

    /// The checkout a lifecycle command runs from: `generation`'s when it is
    /// still on disk, else the generation that last started, else none (an
    /// inline stack, which the executor keeps under its own project dir).
    async fn source_dir_for(&self, project: &str, generation: Option<&str>) -> Option<PathBuf> {
        let mut candidates = Vec::new();
        if let Some(generation) = generation {
            candidates.push(generation.to_string());
        }
        if let Some(current) = self.current_generation(project).await {
            candidates.push(current);
        }
        for candidate in candidates {
            let source = self.generation_dir(project, &candidate).join("source");
            if tokio::fs::metadata(&source)
                .await
                .is_ok_and(|metadata| metadata.is_dir())
            {
                return Some(source);
            }
        }
        None
    }

    /// An executor for lifecycle commands that need no security policy.
    fn executor(&self) -> Result<ComposeExecutor, AgentComposeError> {
        let docker = self.docker.clone().ok_or_else(|| {
            AgentComposeError::Unavailable(
                "this agent has no Docker connection, so it cannot run Compose stacks".into(),
            )
        })?;
        let executor = ComposeExecutor::new(docker, self.data_dir.clone());
        match self.published_host_ip() {
            Some(address) => executor
                .with_published_host_ip(address)
                .map_err(|error| AgentComposeError::Invalid(error.to_string())),
            None => Ok(executor),
        }
    }

    fn published_host_ip(&self) -> Option<std::net::IpAddr> {
        self.host_bind_address
            .read()
            .ok()
            .and_then(|address| address.parse().ok())
    }

    /// The address an executor built now would publish ports on.
    fn current_published_host_ip(&self) -> std::net::IpAddr {
        self.published_host_ip()
            .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
    }

    /// The address Compose work may publish ports on now, or why not.
    ///
    /// A node that joined with a public address publishes on it until its
    /// WireGuard mesh is up, then moves to its mesh address (see
    /// `network_sync::publish_mesh_bind_address`); a node that joined with a
    /// private address never moves. Refusing a public address therefore
    /// means a stack is never published on a public interface, however a
    /// deployment interleaves with the mesh coming up: the only moves left
    /// are between private addresses, which [`Self::take_prepared`] and the
    /// check after `docker compose up` handle.
    fn require_private_bind_address(
        &self,
        project: &str,
        operation: &'static str,
    ) -> Result<std::net::IpAddr, AgentComposeError> {
        let address = self.current_published_host_ip();
        if is_private_bind_address(address) {
            return Ok(address);
        }
        Err(AgentComposeError::PublicBindAddress {
            project: project.to_string(),
            operation,
            address,
        })
    }

    /// The node's current bind address when it differs from `prepared`, the
    /// one a generation's Compose files publish on.
    fn bind_address_moved_from(&self, prepared: std::net::IpAddr) -> Option<std::net::IpAddr> {
        let current = self.current_published_host_ip();
        (current != prepared).then_some(current)
    }

    /// Take a prepared generation to start it. Prepare writes the node's
    /// bind address into the generation's Compose files, so a generation
    /// prepared before that address moved (the mesh coming up replaces the
    /// public address with the private one) would publish its ports on the
    /// old address. Such a generation is discarded, never started.
    async fn take_prepared(
        &self,
        project: &str,
        generation: &str,
    ) -> Result<PreparedGeneration, AgentComposeError> {
        let entry = self
            .prepared
            .lock()
            .await
            .remove(&(project.to_string(), generation.to_string()))
            .ok_or_else(|| AgentComposeError::NotPrepared {
                project: project.to_string(),
                generation: generation.to_string(),
            })?;
        let prepared = entry.executor.published_host_ip();
        let Some(current) = self.bind_address_moved_from(prepared) else {
            return Ok(entry);
        };
        tracing::warn!(
            project = %project,
            generation = %generation,
            %prepared,
            %current,
            "Discarding a prepared Compose generation whose bind address moved"
        );
        if let Err(error) = entry
            .executor
            .cleanup_secret_generation(project, generation)
            .await
        {
            tracing::warn!(
                project = %project,
                generation = %generation,
                "Could not remove the secrets of a discarded Compose generation: {error}"
            );
        }
        self.remove_unused_generation(project, generation).await;
        Err(AgentComposeError::BindAddressMoved {
            project: project.to_string(),
            generation: generation.to_string(),
            prepared,
            current,
        })
    }

    /// [`Self::take_prepared`], refused first while the node's bind address
    /// is public: the generation then stays prepared, neither started nor
    /// discarded, until its TTL.
    async fn take_prepared_to_start(
        &self,
        project: &str,
        generation: &str,
    ) -> Result<PreparedGeneration, AgentComposeError> {
        self.require_private_bind_address(project, "started")?;
        self.take_prepared(project, generation).await
    }

    /// Forget prepared generations nobody started, and their files.
    async fn evict_stale(&self) {
        let stale: Vec<(String, String)> = {
            let mut prepared = self.prepared.lock().await;
            let keys: Vec<_> = prepared
                .iter()
                .filter(|(_, entry)| entry.prepared_at.elapsed() > PREPARED_TTL)
                .map(|(key, _)| key.clone())
                .collect();
            for key in &keys {
                prepared.remove(key);
            }
            keys
        };
        for (project, generation) in stale {
            tracing::warn!(
                project = %project,
                generation = %generation,
                "Discarding a prepared Compose generation that was never started"
            );
            self.remove_unused_generation(&project, &generation).await;
        }
    }

    /// Remove a generation's checkout unless it is the one running.
    async fn remove_unused_generation(&self, project: &str, generation: &str) {
        if self.current_generation(project).await.as_deref() == Some(generation) {
            return;
        }
        let dir = self.generation_dir(project, generation);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    project = %project,
                    generation = %generation,
                    path = %dir.display(),
                    "Could not remove an unused Compose generation: {error}"
                );
            }
        }
    }

    /// Record `generation` as the one that runs, and remove every other
    /// generation's checkout except those still prepared.
    async fn promote(&self, project: &str, generation: &str) -> Result<(), AgentComposeError> {
        let stacks = self.stacks_dir(project);
        tokio::fs::write(self.current_marker(project), generation)
            .await
            .map_err(|error| {
                AgentComposeError::Storage(format!(
                    "Cannot record generation '{generation}' as current for project '{project}' \
                     at {}: {error}",
                    stacks.display()
                ))
            })?;
        let still_prepared: Vec<String> = self
            .prepared
            .lock()
            .await
            .keys()
            .filter(|(prepared_project, _)| prepared_project == project)
            .map(|(_, generation)| generation.clone())
            .collect();
        let Ok(mut entries) = tokio::fs::read_dir(&stacks).await else {
            return Ok(());
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            let is_generation = entry.file_type().await.is_ok_and(|kind| kind.is_dir());
            if is_generation && name != generation && !still_prepared.contains(&name) {
                if let Err(error) = tokio::fs::remove_dir_all(entry.path()).await {
                    tracing::warn!(
                        project = %project,
                        generation = %name,
                        "Could not remove a superseded Compose generation: {error}"
                    );
                }
            }
        }
        Ok(())
    }
}

/// Loopback (an agent registered without a private address) or a private
/// (RFC 1918 / unique-local) address: one no public interface answers on.
fn is_private_bind_address(address: std::net::IpAddr) -> bool {
    address.is_loopback() || temps_core::node_address::is_private_node_address(&address.to_string())
}

fn event_bytes(event: &ComposeHostEvent) -> bytes::Bytes {
    let mut encoded = serde_json::to_vec(event).unwrap_or_else(|_| {
        b"{\"type\":\"failed\",\"data\":{\"error\":{\"message\":\"Worker could not encode a Compose event\",\"class\":\"internal\"}}}".to_vec()
    });
    encoded.push(b'\n');
    bytes::Bytes::from(encoded)
}

fn ndjson(rx: tokio::sync::mpsc::Receiver<ComposeHostEvent>) -> Response {
    let output = stream::unfold(rx, |mut rx| async move {
        rx.recv()
            .await
            .map(|event| (Ok::<_, Infallible>(event_bytes(&event)), rx))
    });
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/x-ndjson")],
        Body::from_stream(output),
    )
        .into_response()
}

fn output_sink(tx: tokio::sync::mpsc::Sender<ComposeHostEvent>) -> ComposeOutputSink {
    ComposeOutputSink(Arc::new(move |lines| {
        let tx = tx.clone();
        Box::pin(async move {
            let _ = tx.send(ComposeHostEvent::Output(lines)).await;
        })
    }))
}

fn failed(error: &temps_deployer::compose::ComposeError) -> ComposeHostEvent {
    ComposeHostEvent::Failed(ComposeHostFailure {
        error: RemoteComposeError::from(error),
        containers: Vec::new(),
        cleanup_containers: Vec::new(),
        retention_error: None,
    })
}

/// Build and pull one generation of a stack on this node.
#[utoipa::path(
    tag = "Compose",
    post,
    path = "/agent/compose/prepare",
    request_body(content = Vec<u8>, content_type = "multipart/form-data"),
    responses(
        (status = 200, description = "NDJSON Compose output and terminal result", content_type = "application/x-ndjson", body = String),
        (status = 400, description = "Invalid spec or stack archive"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Deployment-create permission required"),
        (status = 413, description = "Stack archive too large"),
        (status = 503, description = "No Docker connection, no image-operation slot became free, or this node still publishes on a public address (not on its WireGuard mesh yet)")
    ),
    security(("bearer_auth" = []))
)]
pub async fn prepare(
    RequireAgentAuth(auth): RequireAgentAuth,
    Extension(state): Extension<Arc<ComposeHostState>>,
    Extension(limits): Extension<Arc<AgentResourceLimits>>,
    mut multipart: Multipart,
) -> Result<Response, Problem> {
    permission_guard!(auth, DeploymentsCreate);
    state.evict_stale().await;

    let mut spec_field = match multipart.next_field().await {
        Ok(Some(field)) if field.name() == Some("spec") => field,
        _ => return Err(AgentComposeError::Invalid("Expected spec field first".into()).into()),
    };
    let mut spec_bytes = Vec::new();
    loop {
        match spec_field.chunk().await {
            Ok(Some(chunk))
                if spec_bytes.len().saturating_add(chunk.len()) <= MAX_COMPOSE_SPEC_BYTES =>
            {
                spec_bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            _ => {
                return Err(AgentComposeError::TooLarge(format!(
                    "Compose spec is unreadable or larger than {MAX_COMPOSE_SPEC_BYTES} bytes"
                ))
                .into())
            }
        }
    }
    // Multer only advances to the next part once this field is dropped.
    drop(spec_field);
    let spec: ComposePrepareSpec = serde_json::from_slice(&spec_bytes).map_err(|error| {
        AgentComposeError::Invalid(format!("Compose spec is not valid: {error}"))
    })?;
    spec.validate().map_err(AgentComposeError::Invalid)?;
    let project = spec.project_name.clone();
    let generation = spec.generation.clone();
    // Prepare writes the bind address into the generation's Compose files.
    state.require_private_bind_address(&project, "prepared")?;

    // A prepare builds and pulls images, so it shares the node's
    // image-operation slots with builds, imports and exports.
    let permit = match tokio::time::timeout(
        PREPARE_QUEUE_TIMEOUT,
        limits.image_import_slots.clone().acquire_owned(),
    )
    .await
    {
        Ok(Ok(permit)) => permit,
        Ok(Err(_)) => {
            return Err(AgentComposeError::Unavailable(
                "worker image operation capacity unavailable".into(),
            )
            .into())
        }
        Err(_) => {
            return Err(AgentComposeError::Unavailable(format!(
                "no image-operation slot became free within {} minutes for Compose project \
                 '{project}'; this node is busy building or pulling. Retry later",
                PREPARE_QUEUE_TIMEOUT.as_secs() / 60
            ))
            .into())
        }
    };

    let generation_dir = state.generation_dir(&project, &generation);
    // A retried attempt reuses no files from an earlier one.
    let _ = tokio::fs::remove_dir_all(&generation_dir).await;
    tokio::fs::create_dir_all(&generation_dir)
        .await
        .map_err(|error| {
            AgentComposeError::Storage(format!(
                "Cannot create {} for project '{project}': {error}",
                generation_dir.display()
            ))
        })?;

    let repo_dir = if spec.has_stack {
        match receive_stack(&mut multipart, &generation_dir).await {
            Ok(source) => Some(source),
            Err(error) => {
                let _ = tokio::fs::remove_dir_all(&generation_dir).await;
                return Err(error.into());
            }
        }
    } else {
        None
    };

    let executor = state.executor()?.with_security_policy(spec.policy.clone());
    let executor = Arc::new(executor);
    let (tx, rx) = tokio::sync::mpsc::channel::<ComposeHostEvent>(64);
    let request = spec.to_request(repo_dir.clone(), Some(output_sink(tx.clone())));
    let task_state = state.clone();
    tokio::spawn(async move {
        let _permit = permit;
        let _lock = executor.acquire_project_lifecycle_lock(&project).await;
        let work = tokio::time::timeout(
            PREPARE_DEADLINE,
            executor.prepare_and_pull_for_generation(&request, &generation),
        );
        let outcome = tokio::select! {
            // The control plane gave up: its job has already failed, so stop
            // building and leave nothing behind.
            _ = tx.closed() => None,
            result = work => Some(result),
        };
        match outcome {
            Some(Ok(Ok(prepared))) => {
                task_state.prepared.lock().await.insert(
                    (project.clone(), generation.clone()),
                    PreparedGeneration {
                        executor: executor.clone(),
                        prepared,
                        spec,
                        repo_dir,
                        prepared_at: std::time::Instant::now(),
                    },
                );
                let _ = tx.send(ComposeHostEvent::Prepared).await;
            }
            Some(Ok(Err(error))) => {
                task_state
                    .remove_unused_generation(&project, &generation)
                    .await;
                let _ = tx.send(failed(&error)).await;
            }
            Some(Err(_)) => {
                let _ = executor
                    .cleanup_secret_generation(&project, &generation)
                    .await;
                task_state
                    .remove_unused_generation(&project, &generation)
                    .await;
                let error = temps_deployer::compose::ComposeError::CommandFailed {
                    project: project.clone(),
                    reason: format!(
                        "preparing generation '{generation}' exceeded the worker's {}-minute \
                         deadline",
                        PREPARE_DEADLINE.as_secs() / 60
                    ),
                };
                let _ = tx.send(failed(&error)).await;
            }
            None => {
                let _ = executor
                    .cleanup_secret_generation(&project, &generation)
                    .await;
                task_state
                    .remove_unused_generation(&project, &generation)
                    .await;
            }
        }
    });
    Ok(ndjson(rx))
}

async fn receive_stack(
    multipart: &mut Multipart,
    generation_dir: &Path,
) -> Result<PathBuf, AgentComposeError> {
    let mut field = match multipart.next_field().await {
        Ok(Some(field)) if field.name() == Some("stack") => field,
        _ => {
            return Err(AgentComposeError::Invalid(
                "Expected stack field after spec".into(),
            ))
        }
    };
    let archive_path = generation_dir.join("stack.tar");
    let mut archive = tokio::fs::File::create(&archive_path)
        .await
        .map_err(|error| {
            AgentComposeError::Storage(format!("Cannot create {}: {error}", archive_path.display()))
        })?;
    let mut received = 0_u64;
    loop {
        match field.chunk().await {
            Ok(Some(chunk)) => {
                received = received.saturating_add(chunk.len() as u64);
                if received > MAX_COMPOSE_STACK_BYTES {
                    return Err(AgentComposeError::TooLarge(format!(
                        "stack archive exceeds {MAX_COMPOSE_STACK_BYTES} bytes"
                    )));
                }
                archive.write_all(&chunk).await.map_err(|error| {
                    AgentComposeError::Storage(format!(
                        "Cannot write {}: {error}",
                        archive_path.display()
                    ))
                })?;
            }
            Ok(None) => break,
            Err(error) => {
                return Err(AgentComposeError::Invalid(format!(
                    "Stack upload failed: {error}"
                )))
            }
        }
    }
    archive.flush().await.map_err(|error| {
        AgentComposeError::Storage(format!("Cannot write {}: {error}", archive_path.display()))
    })?;
    drop(archive);
    drop(field);
    if !matches!(multipart.next_field().await, Ok(None)) {
        return Err(AgentComposeError::Invalid(
            "Unexpected field after stack".into(),
        ));
    }
    let source = generation_dir.join("source");
    let (unpack_archive, unpack_source) = (archive_path.clone(), source.clone());
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&unpack_source)
            .map_err(|error| format!("Cannot create {}: {error}", unpack_source.display()))?;
        unpack_compose_stack(&unpack_archive, &unpack_source)
    })
    .await
    .map_err(|error| AgentComposeError::Storage(format!("Stack extraction task failed: {error}")))?
    .map_err(AgentComposeError::Invalid)?;
    let _ = tokio::fs::remove_file(&archive_path).await;
    Ok(source)
}

/// Start a generation prepared on this node.
#[utoipa::path(
    tag = "Compose",
    post,
    path = "/agent/compose/deploy",
    request_body = ComposeGenerationRequest,
    responses(
        (status = 200, description = "NDJSON Compose output and terminal result", content_type = "application/x-ndjson", body = String),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Deployment-create permission required"),
        (status = 409, description = "The generation is not prepared on this node, or was prepared for a bind address the node has since left"),
        (status = 503, description = "This node still publishes on a public address: it is not on its WireGuard mesh yet")
    ),
    security(("bearer_auth" = []))
)]
pub async fn deploy(
    RequireAgentAuth(auth): RequireAgentAuth,
    Extension(state): Extension<Arc<ComposeHostState>>,
    Json(request): Json<ComposeGenerationRequest>,
) -> Result<Response, Problem> {
    permission_guard!(auth, DeploymentsCreate);
    validate_stack_name(&request.project_name, "project_name")
        .map_err(AgentComposeError::Invalid)?;
    validate_stack_name(&request.generation, "generation").map_err(AgentComposeError::Invalid)?;
    let entry = state
        .take_prepared_to_start(&request.project_name, &request.generation)
        .await?;
    // The control plane tore the previous stack down before asking for this
    // start, so this generation's files are now the ones that matter.
    state
        .promote(&request.project_name, &request.generation)
        .await?;

    let (tx, rx) = tokio::sync::mpsc::channel::<ComposeHostEvent>(64);
    let PreparedGeneration {
        executor,
        prepared,
        spec,
        repo_dir,
        ..
    } = entry;
    let compose_request: ComposeDeployRequest =
        spec.to_request(repo_dir, Some(output_sink(tx.clone())));
    let project = request.project_name;
    let generation = request.generation;
    tokio::spawn(async move {
        let _lock = executor.acquire_project_lifecycle_lock(&project).await;
        // Not cancelled on disconnect: stopping `docker compose up` half way
        // would leave a stack nobody records. The executor bounds it with the
        // project's startup timeout.
        let result = executor.deploy_prepared(prepared, &compose_request).await;
        // Checked on failure too: a stack whose readiness failed still
        // created containers, and those are kept for diagnosis.
        let event = match (
            result,
            state.bind_address_moved_from(executor.published_host_ip()),
        ) {
            (Ok(services), None) => ComposeHostEvent::Deployed(services),
            (Err(failure), None) => {
                ComposeHostEvent::Failed(ComposeHostFailure::from(failure.as_ref()))
            }
            (result, Some(current)) => {
                let start_error = result.err().map(|failure| failure.error.to_string());
                stop_stack_on_moved_bind_address(
                    &executor,
                    &compose_request,
                    &generation,
                    current,
                    start_error,
                )
                .await
            }
        };
        let _ = tx.send(event).await;
    });
    Ok(ndjson(rx))
}

/// The node's bind address moved while `docker compose up` ran (its mesh
/// address was reassigned), so the stack's ports were published on the address it left.
/// Docker cannot rebind a running container, so the stack is stopped
/// (`docker compose down`, volumes kept) rather than left on that address,
/// and the deployment fails with the reason. Moving the address is checked
/// again here because nothing can hold it still for the whole `up`: that
/// would stall joining the mesh behind every deploy in flight.
async fn stop_stack_on_moved_bind_address(
    executor: &ComposeExecutor,
    request: &ComposeDeployRequest,
    generation: &str,
    current: std::net::IpAddr,
    start_error: Option<String>,
) -> ComposeHostEvent {
    let prepared = executor.published_host_ip();
    tracing::warn!(
        project = %request.project_name,
        generation = %generation,
        %prepared,
        %current,
        "This node's bind address moved while a Compose stack started; stopping it"
    );
    let teardown = executor
        .teardown_at(
            &request.project_name,
            request.repo_dir.as_deref(),
            request.compose_path.as_deref(),
            &HashMap::new(),
            true,
        )
        .await;
    let stopped = match teardown {
        Ok(()) => "It was stopped (its volumes are kept)".to_string(),
        Err(error) => format!(
            "Stopping it failed, so it may still publish on {prepared}: {error}. Stop it with a \
             redeploy"
        ),
    };
    failed(&temps_deployer::compose::ComposeError::CommandFailed {
        project: request.project_name.clone(),
        reason: format!(
            "generation '{generation}' started with its ports published on {prepared}, but this \
             node moved its published ports to {current} while it started (its private address \
             changed, for example when its mesh address was reassigned). {stopped}; redeploy to \
             start it on {current}{}",
            start_error
                .map(|error| format!(". Starting it had also failed: {error}"))
                .unwrap_or_default()
        ),
    })
}

fn ack(project_name: String) -> Json<AgentResponse<ComposeAck>> {
    AgentResponse::ok(ComposeAck { project_name })
}

/// `docker compose down` for a project on this node. Volumes are kept.
#[utoipa::path(
    tag = "Compose",
    post,
    path = "/agent/compose/teardown",
    request_body = ComposeStackRequest,
    responses(
        (status = 200, description = "Stack torn down", body = ComposeAck),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Deployment-create permission required"),
        (status = 500, description = "docker compose down failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn teardown(
    RequireAgentAuth(auth): RequireAgentAuth,
    Extension(state): Extension<Arc<ComposeHostState>>,
    Json(request): Json<ComposeStackRequest>,
) -> Result<Json<AgentResponse<ComposeAck>>, Problem> {
    permission_guard!(auth, DeploymentsCreate);
    let source = stack_source(&state, &request).await?;
    let executor = state.executor()?;
    let _lock = executor
        .acquire_project_lifecycle_lock(&request.project_name)
        .await;
    executor
        .teardown_at(
            &request.project_name,
            source.as_deref(),
            request.compose_path.as_deref(),
            &HashMap::new(),
            request.remove_secrets,
        )
        .await
        .map_err(|error| AgentComposeError::Operation {
            project: request.project_name.clone(),
            message: error.to_string(),
        })?;
    Ok(ack(request.project_name))
}

/// `docker compose stop` for a project on this node.
#[utoipa::path(
    tag = "Compose",
    post,
    path = "/agent/compose/stop",
    request_body = ComposeStackRequest,
    responses(
        (status = 200, description = "Stack stopped", body = ComposeAck),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Deployment-create permission required"),
        (status = 500, description = "docker compose stop failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn stop(
    RequireAgentAuth(auth): RequireAgentAuth,
    Extension(state): Extension<Arc<ComposeHostState>>,
    Json(request): Json<ComposeStackRequest>,
) -> Result<Json<AgentResponse<ComposeAck>>, Problem> {
    permission_guard!(auth, DeploymentsCreate);
    let source = stack_source(&state, &request).await?;
    let executor = state.executor()?;
    executor
        .stop_at(
            &request.project_name,
            source.as_deref(),
            request.compose_path.as_deref(),
        )
        .await
        .map_err(|error| AgentComposeError::Operation {
            project: request.project_name.clone(),
            message: error.to_string(),
        })?;
    Ok(ack(request.project_name))
}

async fn stack_source(
    state: &ComposeHostState,
    request: &ComposeStackRequest,
) -> Result<Option<PathBuf>, AgentComposeError> {
    validate_stack_name(&request.project_name, "project_name")
        .map_err(AgentComposeError::Invalid)?;
    if let Some(generation) = request.generation.as_deref() {
        validate_stack_name(generation, "generation").map_err(AgentComposeError::Invalid)?;
    }
    Ok(state
        .source_dir_for(&request.project_name, request.generation.as_deref())
        .await)
}

/// Forget a prepared generation that will not be started.
#[utoipa::path(
    tag = "Compose",
    post,
    path = "/agent/compose/discard",
    request_body = ComposeGenerationRequest,
    responses(
        (status = 200, description = "Generation discarded", body = ComposeAck),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Deployment-create permission required")
    ),
    security(("bearer_auth" = []))
)]
pub async fn discard(
    RequireAgentAuth(auth): RequireAgentAuth,
    Extension(state): Extension<Arc<ComposeHostState>>,
    Json(request): Json<ComposeGenerationRequest>,
) -> Result<Json<AgentResponse<ComposeAck>>, Problem> {
    permission_guard!(auth, DeploymentsCreate);
    validate_stack_name(&request.project_name, "project_name")
        .map_err(AgentComposeError::Invalid)?;
    validate_stack_name(&request.generation, "generation").map_err(AgentComposeError::Invalid)?;
    state
        .prepared
        .lock()
        .await
        .remove(&(request.project_name.clone(), request.generation.clone()));
    let executor = state.executor()?;
    executor
        .cleanup_secret_generation(&request.project_name, &request.generation)
        .await
        .map_err(|error| AgentComposeError::Operation {
            project: request.project_name.clone(),
            message: error.to_string(),
        })?;
    state
        .remove_unused_generation(&request.project_name, &request.generation)
        .await;
    Ok(ack(request.project_name))
}

/// Remove every secret generation except the one named.
#[utoipa::path(
    tag = "Compose",
    post,
    path = "/agent/compose/prune-secrets",
    request_body = ComposePruneRequest,
    responses(
        (status = 200, description = "Secret generations pruned", body = ComposeAck),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Deployment-create permission required")
    ),
    security(("bearer_auth" = []))
)]
pub async fn prune_secrets(
    RequireAgentAuth(auth): RequireAgentAuth,
    Extension(state): Extension<Arc<ComposeHostState>>,
    Json(request): Json<ComposePruneRequest>,
) -> Result<Json<AgentResponse<ComposeAck>>, Problem> {
    permission_guard!(auth, DeploymentsCreate);
    validate_stack_name(&request.project_name, "project_name")
        .map_err(AgentComposeError::Invalid)?;
    let executor = state.executor()?;
    executor
        .prune_secret_generations(&request.project_name, request.keep_generation.as_deref())
        .await
        .map_err(|error| AgentComposeError::Operation {
            project: request.project_name.clone(),
            message: error.to_string(),
        })?;
    Ok(ack(request.project_name))
}

/// Force-remove containers after re-verifying their Temps labels.
#[utoipa::path(
    tag = "Compose",
    post,
    path = "/agent/compose/remove-verified",
    request_body = ComposeRemoveVerifiedRequest,
    responses(
        (status = 200, description = "Containers removed", body = ComposeAck),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Deployment-create permission required"),
        (status = 500, description = "A container could not be removed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn remove_verified(
    RequireAgentAuth(auth): RequireAgentAuth,
    Extension(state): Extension<Arc<ComposeHostState>>,
    Json(request): Json<ComposeRemoveVerifiedRequest>,
) -> Result<Json<AgentResponse<ComposeAck>>, Problem> {
    permission_guard!(auth, DeploymentsCreate);
    let project = request
        .expected_labels
        .get("com.docker.compose.project")
        .cloned()
        .unwrap_or_default();
    let executor = state.executor()?;
    executor
        .remove_verified_containers(&request.containers, &request.expected_labels)
        .await
        .map_err(|error| AgentComposeError::Operation {
            project: project.clone(),
            message: error.to_string(),
        })?;
    Ok(ack(project))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(data_dir: &Path) -> ComposeHostState {
        ComposeHostState::new(
            None,
            data_dir.to_path_buf(),
            Arc::new(std::sync::RwLock::new("10.20.0.5".to_string())),
        )
    }

    /// Lifecycle commands run from a generation's files while it is on disk,
    /// then from the generation that last started; an inline stack has none.
    #[tokio::test]
    async fn lifecycle_commands_find_the_right_checkout() {
        let data = tempfile::tempdir().unwrap();
        let state = state(data.path());
        assert_eq!(state.source_dir_for("temps-1-2", None).await, None);

        let first = state
            .generation_dir("temps-1-2", "deployment-1-a")
            .join("source");
        std::fs::create_dir_all(&first).unwrap();
        state.promote("temps-1-2", "deployment-1-a").await.unwrap();
        assert_eq!(
            state.source_dir_for("temps-1-2", None).await,
            Some(first.clone())
        );

        let second = state
            .generation_dir("temps-1-2", "deployment-2-b")
            .join("source");
        std::fs::create_dir_all(&second).unwrap();
        assert_eq!(
            state
                .source_dir_for("temps-1-2", Some("deployment-2-b"))
                .await,
            Some(second.clone())
        );
        // A generation no longer on disk falls back to the running one.
        assert_eq!(
            state
                .source_dir_for("temps-1-2", Some("deployment-9-z"))
                .await,
            Some(first)
        );
    }

    /// Starting a generation removes the checkouts it superseded, but never
    /// the one starting or another that is still prepared.
    #[tokio::test]
    async fn promoting_a_generation_prunes_superseded_checkouts() {
        let data = tempfile::tempdir().unwrap();
        let state = state(data.path());
        for generation in ["deployment-1-a", "deployment-2-b", "deployment-3-c"] {
            std::fs::create_dir_all(state.generation_dir("temps-1-2", generation).join("source"))
                .unwrap();
        }
        state.promote("temps-1-2", "deployment-2-b").await.unwrap();
        assert!(!state.generation_dir("temps-1-2", "deployment-1-a").exists());
        assert!(state.generation_dir("temps-1-2", "deployment-2-b").exists());
        assert_eq!(
            state.current_generation("temps-1-2").await.as_deref(),
            Some("deployment-2-b")
        );

        // The running generation is never removed as "unused".
        state
            .remove_unused_generation("temps-1-2", "deployment-2-b")
            .await;
        assert!(state.generation_dir("temps-1-2", "deployment-2-b").exists());
        state
            .remove_unused_generation("temps-1-2", "deployment-3-c")
            .await;
        assert!(!state.generation_dir("temps-1-2", "deployment-3-c").exists());
    }

    /// Without Docker the agent refuses with a reason instead of failing
    /// part way through.
    #[test]
    fn an_agent_without_docker_refuses_compose_work() {
        let data = tempfile::tempdir().unwrap();
        let error = state(data.path()).executor().unwrap_err();
        assert!(
            error.to_string().contains("no Docker connection"),
            "{error}"
        );
    }

    /// A prepared generation for `project`, built by an executor that
    /// publishes on `address`.
    fn prepared_generation(project: &str, generation: &str, address: &str) -> PreparedGeneration {
        let executor = ComposeExecutor::new_with_handle(
            Arc::new(temps_core::DockerHandle::disabled(
                "test",
                "no Docker in tests",
            )),
            std::env::temp_dir(),
        )
        .with_published_host_ip(address.parse().unwrap())
        .unwrap();
        let spec: ComposePrepareSpec = serde_json::from_value(serde_json::json!({
            "version": 1,
            "project_name": project,
            "generation": generation,
            "compose_content": "services: {}",
            "env_content": null,
            "compose_path": null,
            "environment_vars": {},
            "secrets": {},
            "secret_compose_services": {},
            "build_args": {},
            "labels": {},
            "compose_override": null,
            "relaxed_capability_services": [],
            "unsandboxed_services": [],
            "ready_timeout_secs": null,
            "routed_services": [],
            "policy": {},
            "has_stack": false,
        }))
        .unwrap();
        PreparedGeneration {
            executor: Arc::new(executor),
            prepared: PreparedComposeDeploy {
                effective_dir: std::env::temp_dir(),
                project_name: project.to_string(),
                compose_file: "docker-compose.yml".to_string(),
                redact_values: Vec::new(),
                secret_generation: generation.to_string(),
                has_materialized_secrets: false,
            },
            spec,
            repo_dir: None,
            prepared_at: std::time::Instant::now(),
        }
    }

    /// A generation prepared before the node's bind address moved (its mesh
    /// address reassigned) would publish its ports on the old address. It is
    /// discarded instead of started; one prepared on the current address
    /// starts.
    #[tokio::test]
    async fn a_generation_prepared_for_an_old_bind_address_is_discarded() {
        let data = tempfile::tempdir().unwrap();
        let state = state(data.path());
        let stale_dir = state.generation_dir("temps-1-2", "deployment-1-a");
        std::fs::create_dir_all(stale_dir.join("source")).unwrap();
        state.prepared.lock().await.insert(
            ("temps-1-2".to_string(), "deployment-1-a".to_string()),
            prepared_generation("temps-1-2", "deployment-1-a", "203.0.113.7"),
        );
        state.prepared.lock().await.insert(
            ("temps-1-2".to_string(), "deployment-2-b".to_string()),
            prepared_generation("temps-1-2", "deployment-2-b", "10.20.0.5"),
        );

        let error = state
            .take_prepared("temps-1-2", "deployment-1-a")
            .await
            .err()
            .unwrap();
        assert!(
            matches!(error, AgentComposeError::BindAddressMoved { ref generation, .. }
                if generation == "deployment-1-a"),
            "{error}"
        );
        let message = error.to_string();
        assert!(message.contains("203.0.113.7"), "{message}");
        assert!(message.contains("10.20.0.5"), "{message}");
        assert_eq!(Problem::from(error).status_code, StatusCode::CONFLICT);
        assert!(!stale_dir.exists());
        // Discarded, so a retry finds nothing to start.
        assert!(matches!(
            state.take_prepared("temps-1-2", "deployment-1-a").await,
            Err(AgentComposeError::NotPrepared { .. })
        ));

        let current = state
            .take_prepared("temps-1-2", "deployment-2-b")
            .await
            .unwrap();
        assert_eq!(
            current.executor.published_host_ip().to_string(),
            "10.20.0.5"
        );
    }

    /// A stack that started on the address its generation was prepared with
    /// is kept; one whose node moved its bind address meanwhile is not.
    #[test]
    fn a_moved_bind_address_is_detected_against_the_prepared_one() {
        let data = tempfile::tempdir().unwrap();
        let state = state(data.path());
        assert_eq!(
            state.bind_address_moved_from("10.20.0.5".parse().unwrap()),
            None
        );
        *state.host_bind_address.write().unwrap() = "10.99.0.2".to_string();
        assert_eq!(
            state.bind_address_moved_from("10.20.0.5".parse().unwrap()),
            Some("10.99.0.2".parse().unwrap())
        );
    }

    /// A node that joined with a public address is refused Compose work
    /// until its bind address is on the mesh, so a stack's ports never land
    /// on a public interface; private and loopback addresses are accepted.
    #[test]
    fn compose_work_is_refused_while_the_bind_address_is_public() {
        let data = tempfile::tempdir().unwrap();
        let state = state(data.path());
        for private in [
            "10.20.0.5",
            "172.16.4.2",
            "192.168.1.9",
            "127.0.0.1",
            "fd00::7",
        ] {
            *state.host_bind_address.write().unwrap() = private.to_string();
            assert_eq!(
                state
                    .require_private_bind_address("temps-1-2", "prepared")
                    .unwrap(),
                private.parse::<std::net::IpAddr>().unwrap(),
            );
        }
        for public in ["203.0.113.10", "2001:db8::10"] {
            *state.host_bind_address.write().unwrap() = public.to_string();
            let error = state
                .require_private_bind_address("temps-1-2", "started")
                .unwrap_err();
            assert!(
                matches!(error, AgentComposeError::PublicBindAddress { ref address, .. }
                    if address.to_string() == public),
                "{error}"
            );
            let message = error.to_string();
            assert!(message.contains("temps-1-2"), "{message}");
            assert!(message.contains("WireGuard mesh"), "{message}");
            assert_eq!(
                Problem::from(error).status_code,
                StatusCode::SERVICE_UNAVAILABLE
            );
        }
    }

    /// Starting a generation is refused on a public bind address before the
    /// prepared generation is taken, so it is neither started nor lost.
    #[tokio::test]
    async fn a_start_on_a_public_bind_address_leaves_the_generation_prepared() {
        let data = tempfile::tempdir().unwrap();
        let state = state(data.path());
        state.prepared.lock().await.insert(
            ("temps-1-2".to_string(), "deployment-1-a".to_string()),
            prepared_generation("temps-1-2", "deployment-1-a", "10.20.0.5"),
        );
        *state.host_bind_address.write().unwrap() = "203.0.113.10".to_string();

        let error = state
            .take_prepared_to_start("temps-1-2", "deployment-1-a")
            .await
            .err()
            .expect("a public bind address is refused");
        assert!(
            matches!(
                error,
                AgentComposeError::PublicBindAddress {
                    operation: "started",
                    ..
                }
            ),
            "{error}"
        );
        assert!(state
            .prepared
            .lock()
            .await
            .contains_key(&("temps-1-2".to_string(), "deployment-1-a".to_string())));
    }

    /// The executor publishes on the node's bind address.
    #[test]
    fn the_bind_address_is_the_published_host_address() {
        let data = tempfile::tempdir().unwrap();
        assert_eq!(
            state(data.path()).published_host_ip(),
            Some("10.20.0.5".parse().unwrap())
        );
    }
}
