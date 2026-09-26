// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Authenticated image build and export endpoints.
//!
//! Together they let a control plane with no Docker daemon turn source into a
//! running container: `POST /agent/images/build` builds on this node, and
//! `GET /agent/images/export` hands the result to whichever node runs it.
//! Neither depends on this node hosting replicas, so a build-only node serves
//! both unchanged.

use axum::{
    body::Body,
    extract::{Extension, Multipart, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use futures::{stream, StreamExt};
use serde::Deserialize;
use std::{convert::Infallible, io::Write, path::Path, sync::Arc};
use temps_deployer::{
    build_protocol::{
        validate_archive_path, BuildEvent, BuildFailure, BuildFailureKind, BuildSpec,
        MAX_BUILD_CONTEXT_BYTES, MAX_BUILD_CONTEXT_ENTRIES, MAX_BUILD_SPEC_BYTES,
    },
    BuildRequest, BuildRequestWithCallback, BuildResult, BuilderError,
};
use tokio::io::AsyncWriteExt;

use crate::auth::RequireAgentAuth;
use crate::handlers::{AgentResourceLimits, AgentState};
use temps_auth::permission_guard;
use temps_core::problemdetails::{self, Problem};

#[derive(Debug, thiserror::Error)]
enum AgentImageError {
    #[error("Invalid worker image request: {0}")]
    Invalid(String),
    #[error("Worker image resource limit exceeded: {0}")]
    TooLarge(String),
    #[error("Worker image capacity unavailable: {0}")]
    Unavailable(String),
    #[error("Worker image deadline exceeded: {0}")]
    Deadline(String),
    #[error("Worker image not found: {0}")]
    NotFound(String),
    #[error("Worker image storage operation failed: {0}")]
    Storage(String),
}

impl From<AgentImageError> for Problem {
    fn from(error: AgentImageError) -> Self {
        let (status, title) = match &error {
            AgentImageError::Invalid(_) => (StatusCode::BAD_REQUEST, "Invalid image request"),
            AgentImageError::TooLarge(_) => (StatusCode::PAYLOAD_TOO_LARGE, "Image resource limit"),
            AgentImageError::Unavailable(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "Image capacity unavailable",
            ),
            AgentImageError::Deadline(_) => {
                (StatusCode::GATEWAY_TIMEOUT, "Image operation timed out")
            }
            AgentImageError::NotFound(_) => (StatusCode::NOT_FOUND, "Image not found"),
            AgentImageError::Storage(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Image operation failed")
            }
        };
        problemdetails::new(status)
            .with_title(title)
            .with_detail(error.to_string())
    }
}

const BUILD_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Own (rather than detach) the build future. Dropping it cancels the Docker
/// request and releases its scratch directory and admission permit before the
/// terminal event is sent. A disconnected/slow consumer cannot retain a slot.
async fn supervise_build(
    build: impl std::future::Future<Output = Result<BuildResult, BuilderError>>,
    tx: tokio::sync::mpsc::Sender<BuildEvent>,
    deadline: tokio::time::Instant,
) {
    let event = tokio::select! {
        _ = tx.closed() => return,
        result = tokio::time::timeout_at(deadline, build) => match result {
            Ok(Ok(result)) => BuildEvent::Result(result),
            Ok(Err(error)) => BuildEvent::Failure(BuildFailure {
                kind: if matches!(error, BuilderError::BuildFailed(_) | BuilderError::BuildOutOfMemory { .. }) {
                    BuildFailureKind::Build
                } else {
                    BuildFailureKind::Worker
                },
                message: bounded_log_line(error.to_string()),
            }),
            Err(_) => BuildEvent::Failure(BuildFailure {
                kind: BuildFailureKind::Timeout,
                message: "Worker build exceeded its 30-minute deadline".into(),
            }),
        }
    };
    // Do not leave even this small task waiting forever on a full log queue.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), tx.send(event)).await;
}

fn safe_extract_context(archive: &Path, destination: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive)
        .map_err(|error| format!("Cannot open uploaded build context: {error}"))?;
    let mut archive = tar::Archive::new(file);
    let entries = archive
        .entries()
        .map_err(|error| format!("Cannot read build context archive: {error}"))?;
    let mut total_size = 0_u64;
    let mut count = 0_usize;
    for entry in entries {
        count += 1;
        if count > MAX_BUILD_CONTEXT_ENTRIES {
            return Err(format!(
                "Build context exceeds the {MAX_BUILD_CONTEXT_ENTRIES}-entry limit"
            ));
        }
        let mut entry = entry.map_err(|error| format!("Invalid build context entry: {error}"))?;
        let path = entry
            .path()
            .map_err(|error| format!("Invalid build context path: {error}"))?
            .into_owned();
        validate_archive_path(&path)?;
        if path.components().any(|part| part.as_os_str() == ".git") {
            return Err("Build context must not contain a .git directory".to_string());
        }
        let output = destination.join(&path);
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            std::fs::create_dir_all(&output).map_err(|error| {
                format!(
                    "Cannot create build context directory '{}': {error}",
                    path.display()
                )
            })?;
        } else if kind.is_file() {
            let size = entry
                .header()
                .size()
                .map_err(|error| format!("Invalid build context entry size: {error}"))?;
            total_size = total_size
                .checked_add(size)
                .ok_or_else(|| "Build context size overflowed".to_string())?;
            if total_size > MAX_BUILD_CONTEXT_BYTES {
                return Err(format!(
                    "Extracted build context exceeds the {MAX_BUILD_CONTEXT_BYTES}-byte limit"
                ));
            }
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    format!(
                        "Cannot create build context parent '{}': {error}",
                        path.display()
                    )
                })?;
            }
            let mut target = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&output)
                .map_err(|error| {
                    format!(
                        "Cannot create build context file '{}': {error}",
                        path.display()
                    )
                })?;
            std::io::copy(&mut entry, &mut target).map_err(|error| {
                format!(
                    "Cannot extract build context file '{}': {error}",
                    path.display()
                )
            })?;
            target.flush().map_err(|error| {
                format!(
                    "Cannot flush build context file '{}': {error}",
                    path.display()
                )
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = entry
                    .header()
                    .mode()
                    .map_err(|error| format!("Invalid mode for '{}': {error}", path.display()))?;
                // Preserve executable source scripts, but never setuid/setgid.
                target
                    .set_permissions(std::fs::Permissions::from_mode(mode & 0o777))
                    .map_err(|error| {
                        format!(
                            "Cannot set source permissions for '{}': {error}",
                            path.display()
                        )
                    })?;
            }
        } else {
            return Err(format!(
                "Build context entry '{}' is not a regular file or directory",
                path.display()
            ));
        }
    }
    Ok(())
}

fn event_bytes(event: BuildEvent) -> bytes::Bytes {
    let mut encoded = serde_json::to_vec(&event).unwrap_or_else(|_| {
        b"{\"type\":\"failure\",\"data\":{\"kind\":\"worker\",\"message\":\"Worker could not encode build event\"}}".to_vec()
    });
    encoded.push(b'\n');
    bytes::Bytes::from(encoded)
}

fn bounded_log_line(mut line: String) -> String {
    const MAX_LINE_BYTES: usize = 8 * 1024;
    if line.len() > MAX_LINE_BYTES {
        let mut end = MAX_LINE_BYTES;
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        line.truncate(end);
        line.push_str(" [truncated]");
    }
    line
}

/// Build an image on this node from a streamed source tree.
#[utoipa::path(
    tag = "Images",
    post,
    path = "/agent/images/build",
    request_body(content = Vec<u8>, content_type = "multipart/form-data"),
    responses(
        (status = 200, description = "NDJSON build log and terminal result", content_type = "application/x-ndjson", body = String),
        (status = 400, description = "Invalid build specification or archive"),
        (status = 401, description = "Unauthorized"),
        (status = 413, description = "Build context too large"),
        (status = 403, description = "Deployment-create permission required"),
        (status = 500, description = "Worker build storage operation failed"),
        (status = 503, description = "Worker build capacity unavailable"),
        (status = 504, description = "Build admission or upload deadline exceeded")
    ),
    security(("bearer_auth" = []))
)]
pub async fn build_image(
    RequireAgentAuth(auth): RequireAgentAuth,
    State(state): State<Arc<AgentState>>,
    Extension(limits): Extension<Arc<AgentResourceLimits>>,
    multipart: Multipart,
) -> Result<Response, Problem> {
    permission_guard!(auth, DeploymentsCreate);
    let deadline = tokio::time::Instant::now() + BUILD_DEADLINE;
    tokio::time::timeout_at(deadline, receive_build(state, limits, multipart, deadline))
        .await
        .map_err(|_| {
            Problem::from(AgentImageError::Deadline(
                "Build admission/upload exceeded 30 minutes".into(),
            ))
        })?
}

async fn receive_build(
    state: Arc<AgentState>,
    limits: Arc<AgentResourceLimits>,
    mut multipart: Multipart,
    deadline: tokio::time::Instant,
) -> Result<Response, Problem> {
    let mut spec_field = match multipart.next_field().await {
        Ok(Some(field)) if field.name() == Some("spec") => field,
        _ => return Err(AgentImageError::Invalid("Expected spec field first".into()).into()),
    };
    let mut spec_bytes = Vec::new();
    loop {
        match spec_field.chunk().await {
            Ok(Some(chunk))
                if spec_bytes.len().saturating_add(chunk.len()) <= MAX_BUILD_SPEC_BYTES =>
            {
                spec_bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            _ => {
                return Err(
                    AgentImageError::Invalid("Build spec is invalid or too large".into()).into(),
                )
            }
        }
    }
    // Multer only advances to the next part after this Field is dropped,
    // even when chunk() has returned None. Keep this before next_field().
    drop(spec_field);
    let spec: BuildSpec = match serde_json::from_slice(&spec_bytes) {
        Ok(spec) => spec,
        Err(_) => {
            return Err(AgentImageError::Invalid("Build spec is not valid JSON".into()).into())
        }
    };
    if let Err(message) = spec.validate() {
        return Err(AgentImageError::Invalid(message).into());
    }

    let permit =
        match tokio::time::timeout_at(deadline, limits.image_import_slots.clone().acquire_owned())
            .await
        {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => {
                return Err(AgentImageError::Unavailable(
                    "Worker image operation capacity unavailable".into(),
                )
                .into())
            }
            Err(_) => {
                return Err(AgentImageError::Deadline(
                    "Worker build waited 30 minutes for capacity".into(),
                )
                .into())
            }
        };
    let scratch = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(error) => {
            return Err(AgentImageError::Storage(format!(
                "Cannot create worker build scratch directory: {error}"
            ))
            .into())
        }
    };
    let archive_path = scratch.path().join("context.tar");
    let mut archive_file = match tokio::fs::File::create(&archive_path).await {
        Ok(file) => file,
        Err(error) => {
            return Err(AgentImageError::Storage(format!(
                "Cannot create worker build archive: {error}"
            ))
            .into())
        }
    };
    let mut context = match multipart.next_field().await {
        Ok(Some(field)) if field.name() == Some("context") => field,
        _ => {
            return Err(AgentImageError::Invalid("Expected context field after spec".into()).into())
        }
    };
    let mut received = 0_u64;
    loop {
        let chunk = match tokio::time::timeout_at(deadline, context.chunk()).await {
            Ok(Ok(Some(chunk))) => chunk,
            Ok(Ok(None)) => break,
            Ok(Err(error)) => {
                return Err(AgentImageError::Invalid(format!(
                    "Build context upload failed: {error}"
                ))
                .into())
            }
            Err(_) => {
                return Err(AgentImageError::Deadline(
                    "Build context upload exceeded 30 minutes".into(),
                )
                .into())
            }
        };
        received = received.saturating_add(chunk.len() as u64);
        if received > MAX_BUILD_CONTEXT_BYTES {
            return Err(AgentImageError::TooLarge(format!(
                "Build context exceeds the {MAX_BUILD_CONTEXT_BYTES}-byte limit"
            ))
            .into());
        }
        if let Err(error) = archive_file.write_all(&chunk).await {
            return Err(AgentImageError::Storage(format!(
                "Cannot write worker build archive: {error}"
            ))
            .into());
        }
    }
    drop(archive_file);
    drop(context);
    match multipart.next_field().await {
        Ok(None) => {}
        _ => {
            return Err(
                AgentImageError::Invalid("Unexpected field after build context".into()).into(),
            )
        }
    }
    let context_dir = scratch.path().join("source");
    if let Err(error) = std::fs::create_dir(&context_dir) {
        return Err(AgentImageError::Storage(format!(
            "Cannot create worker source directory: {error}"
        ))
        .into());
    }
    let archive_for_extract = archive_path.clone();
    let context_for_extract = context_dir.clone();
    let (scratch, permit) = match tokio::task::spawn_blocking(move || {
        // A blocking extraction cannot be aborted. Keep its resources owned
        // here if the HTTP handler disconnects or expires while it finishes.
        safe_extract_context(&archive_for_extract, &context_for_extract).map(|()| (scratch, permit))
    })
    .await
    {
        Ok(Ok(resources)) => resources,
        Ok(Err(message)) => return Err(AgentImageError::Invalid(message).into()),
        Err(error) => {
            return Err(AgentImageError::Storage(format!(
                "Worker build extraction task failed: {error}"
            ))
            .into())
        }
    };
    let dockerfile = context_dir.join(&spec.dockerfile);
    if !dockerfile.is_file() {
        return Err(AgentImageError::Invalid(format!(
            "Dockerfile '{}' is absent from the uploaded context",
            spec.dockerfile
        ))
        .into());
    }
    let (tx, rx) = tokio::sync::mpsc::channel::<BuildEvent>(64);
    let log_tx = tx.clone();
    let callback: temps_deployer::LogCallback = Arc::new(move |line| {
        let log_tx = log_tx.clone();
        Box::pin(async move {
            let _ = log_tx.send(BuildEvent::Log(bounded_log_line(line))).await;
        })
    });
    let request = BuildRequest {
        image_name: spec.image_name,
        context_path: context_dir,
        dockerfile_path: Some(dockerfile),
        build_args: Default::default(),
        build_args_buildkit: Default::default(),
        platform: spec.platform,
        log_path: scratch.path().join("build.log"),
    };
    let builder = state.image_builder.clone();
    let build = async move {
        let _permit = permit;
        let _scratch = scratch;
        let requested_platform = request.platform.clone();
        let result = builder
            .build_image_with_callback(BuildRequestWithCallback {
                request,
                log_callback: Some(callback),
            })
            .await?;
        if let Some(expected) = requested_platform {
            let info = builder.inspect_image(&result.image_name).await?;
            if !temps_deployer::platform::platforms_match(&info.platform, &expected) {
                return Err(BuilderError::BuildFailed(format!(
                    "Worker built '{}' for {} instead of requested {}",
                    result.image_name, info.platform, expected
                )));
            }
        }
        Ok(result)
    };
    tokio::spawn(supervise_build(build, tx, deadline));
    let output = stream::unfold(rx, |mut rx| async move {
        rx.recv()
            .await
            .map(|event| (Ok::<_, Infallible>(event_bytes(event)), rx))
    });
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/x-ndjson")],
        Body::from_stream(output),
    )
        .into_response())
}

/// Query for `GET /agent/images/export`. The reference travels as a query
/// parameter because image names contain `/` and `:`.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ExportImageQuery {
    /// Image reference to export, e.g. `temps-app:3f2a`.
    pub image: String,
}

/// Inspect on the daemon that actually owns the image, never on the control plane.
#[utoipa::path(
    tag = "Images", get, path = "/agent/images/inspect", params(ExportImageQuery),
    responses(
        (status = 200, description = "Image metadata", body = temps_deployer::ImageInfo),
        (status = 400, description = "Invalid image reference"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Image not found"),
        (status = 403, description = "Deployment-read permission required"),
        (status = 500, description = "Image inspection failed"),
        (status = 504, description = "Inspection deadline exceeded")
    ), security(("bearer_auth" = []))
)]
pub async fn inspect_image(
    RequireAgentAuth(auth): RequireAgentAuth,
    State(state): State<Arc<AgentState>>,
    Query(query): Query<ExportImageQuery>,
) -> Result<axum::Json<temps_deployer::ImageInfo>, Problem> {
    permission_guard!(auth, DeploymentsRead);
    validate_image_reference(&query.image)?;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        state.image_builder.inspect_image(&query.image),
    )
    .await
    .map_err(|_| {
        AgentImageError::Deadline(format!("Inspect '{}' exceeded 30 seconds", query.image))
    })?;
    result.map(axum::Json).map_err(|error| match error {
        BuilderError::ImageNotFound(_) => AgentImageError::NotFound(query.image).into(),
        error => {
            AgentImageError::Storage(format!("Cannot inspect '{}': {error}", query.image)).into()
        }
    })
}

fn validate_image_reference(image: &str) -> Result<(), AgentImageError> {
    if image.is_empty() || image.len() > 256 || image.chars().any(char::is_whitespace) {
        return Err(AgentImageError::Invalid(
            "Image reference must be 1–256 characters without whitespace".into(),
        ));
    }
    Ok(())
}

/// Stream an image on this node out as a `docker save` tar.
///
/// Holds one of the node's image-operation slots for the whole transfer, so
/// exports, imports, pulls and builds share one concurrency bound.
#[utoipa::path(
    tag = "Images",
    get,
    path = "/agent/images/export",
    params(ExportImageQuery),
    responses(
        (status = 200, description = "Image tar stream", content_type = "application/x-tar", body = Vec<u8>),
        (status = 400, description = "Invalid image reference"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Image not present on this node"),
        (status = 403, description = "Deployment-read permission required"),
        (status = 500, description = "Image export failed"),
        (status = 503, description = "Image operation capacity unavailable"),
        (status = 504, description = "Timed out waiting for image operation capacity")
    ),
    security(("bearer_auth" = []))
)]
pub async fn export_image(
    RequireAgentAuth(auth): RequireAgentAuth,
    State(state): State<Arc<AgentState>>,
    Extension(limits): Extension<Arc<AgentResourceLimits>>,
    Query(query): Query<ExportImageQuery>,
) -> Result<Response, Problem> {
    permission_guard!(auth, DeploymentsRead);
    let image = query.image;
    validate_image_reference(&image)?;
    let deadline = tokio::time::Instant::now() + BUILD_DEADLINE;
    let permit =
        match tokio::time::timeout_at(deadline, limits.image_import_slots.clone().acquire_owned())
            .await
        {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => {
                return Err(AgentImageError::Unavailable(
                    "Worker image operation capacity unavailable".into(),
                )
                .into())
            }
            Err(_) => {
                return Err(AgentImageError::Deadline(format!(
                    "Export of '{image}' waited 30 minutes for capacity"
                ))
                .into())
            }
        };
    let export_result =
        tokio::time::timeout_at(deadline, state.image_builder.export_image_stream(&image))
            .await
            .map_err(|_| {
                AgentImageError::Deadline(format!(
                    "Export of '{image}' exceeded 30 minutes before streaming"
                ))
            })?;
    let exported = match export_result {
        Ok(stream) => stream,
        Err(BuilderError::ImageNotFound(_)) => {
            return Err(AgentImageError::NotFound(format!(
                "Image '{image}' is not present on this node"
            ))
            .into())
        }
        Err(error) => {
            tracing::error!(image = %image, "Image export failed to start: {error}");
            return Err(AgentImageError::Storage(format!(
                "Cannot export image '{image}': {error}"
            ))
            .into());
        }
    };
    tracing::info!(image = %image, "Streaming image export");
    // The permit lives in the stream state, so the slot is released exactly
    // when the transfer ends — completed, failed, or dropped by the client.
    let body = stream::unfold(
        (exported, permit, false),
        move |(mut exported, permit, finished)| async move {
            if finished {
                return None;
            }
            match tokio::time::timeout_at(deadline, exported.next()).await {
                Ok(Some(Ok(chunk))) => Some((Ok(chunk), (exported, permit, false))),
                Ok(Some(Err(error))) => Some((Err(error), (exported, permit, true))),
                Ok(None) => None,
                Err(_) => Some((
                    Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "Image export exceeded its 30-minute deadline",
                    )),
                    (exported, permit, true),
                )),
            }
        },
    );
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/x-tar")],
        Body::from_stream(body),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn image_handlers_authenticate_even_without_router_middleware() {
        use axum::{
            routing::{get, post},
            Router,
        };
        use tower::ServiceExt;
        let remote = Arc::new(
            temps_deployer::remote::RemoteNodeDeployer::new(
                "http://127.0.0.1:9".into(),
                "unused".into(),
                "no-io".into(),
            )
            .unwrap(),
        );
        let state = Arc::new(AgentState {
            container_deployer: remote.clone(),
            image_builder: remote,
            docker: None,
            overlay_bridge_address: Default::default(),
            overlay_peers: Default::default(),
            platform: Default::default(),
            host_bind_address: "127.0.0.1".into(),
        });
        // Deliberately omit require_agent_auth middleware. Each real handler
        // must still reject credentials before touching Docker or source data.
        let router = Router::new()
            .route("/build", post(build_image))
            .route("/inspect", get(inspect_image))
            .route("/export", get(export_image))
            .layer(Extension(Arc::new(crate::auth::AgentAuth::new(
                "node-token",
            ))))
            .layer(Extension(Arc::new(AgentResourceLimits::new())))
            .with_state(state);
        for (method, path) in [
            ("POST", "/build"),
            ("GET", "/inspect?image="),
            ("GET", "/export?image="),
        ] {
            for token in [None, Some("Bearer wrong-token"), Some("Bearer node-token")] {
                let mut request = axum::http::Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "multipart/form-data; boundary=test");
                if let Some(token) = token {
                    request = request.header("authorization", token);
                }
                let response = router
                    .clone()
                    .oneshot(request.body(Body::from("--test--\r\n")).unwrap())
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    if token == Some("Bearer node-token") {
                        StatusCode::BAD_REQUEST
                    } else {
                        StatusCode::UNAUTHORIZED
                    },
                    "{method} {path} {token:?}"
                );
                assert_eq!(
                    response.headers()[header::CONTENT_TYPE],
                    "application/problem+json"
                );
            }
        }
    }

    #[tokio::test]
    async fn worker_build_timeout_releases_capacity_and_scratch_before_terminal_event() {
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = slots.clone().acquire_owned().await.unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().to_owned();
        let build = async move {
            let _permit = permit;
            let _scratch = scratch;
            std::future::pending::<Result<BuildResult, BuilderError>>().await
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        supervise_build(
            build,
            tx,
            tokio::time::Instant::now() + std::time::Duration::from_millis(10),
        )
        .await;
        assert_eq!(slots.available_permits(), 1);
        assert!(!path.exists());
        assert!(matches!(
            rx.recv().await,
            Some(BuildEvent::Failure(BuildFailure {
                kind: BuildFailureKind::Timeout,
                ..
            }))
        ));
    }

    #[tokio::test]
    async fn worker_build_disconnect_releases_capacity_and_scratch() {
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = slots.clone().acquire_owned().await.unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().to_owned();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let build = async move {
            let _permit = permit;
            let _scratch = scratch;
            let _ = started_tx.send(());
            std::future::pending::<Result<BuildResult, BuilderError>>().await
        };
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let task = tokio::spawn(supervise_build(
            build,
            tx,
            tokio::time::Instant::now() + BUILD_DEADLINE,
        ));
        started_rx.await.unwrap();
        drop(rx);
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(slots.available_permits(), 1);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn worker_image_errors_are_problem_details() {
        for (error, status) in [
            (
                AgentImageError::Invalid("bad spec".into()),
                StatusCode::BAD_REQUEST,
            ),
            (
                AgentImageError::TooLarge("context limit".into()),
                StatusCode::PAYLOAD_TOO_LARGE,
            ),
            (
                AgentImageError::Unavailable("closed capacity".into()),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                AgentImageError::Deadline("build deadline".into()),
                StatusCode::GATEWAY_TIMEOUT,
            ),
            (
                AgentImageError::NotFound("app:1".into()),
                StatusCode::NOT_FOUND,
            ),
            (
                AgentImageError::Storage("scratch creation".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ] {
            let response = Problem::from(error).into_response();
            assert_eq!(response.status(), status);
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "application/problem+json"
            );
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            let detail: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            // Problem's status is conveyed by HTTP; the JSON status member is optional.
            assert!(detail["title"].is_string());
            assert!(
                detail["detail"].as_str().unwrap().contains("Worker")
                    || detail["detail"].as_str().unwrap().contains("worker")
            );
        }
    }

    #[test]
    fn archive_extracts_regular_files_and_rejects_links() {
        let root = tempfile::tempdir().expect("scratch");
        let valid = root.path().join("valid.tar");
        let file = std::fs::File::create(&valid).expect("create tar");
        let mut tar = tar::Builder::new(file);
        let contents = b"FROM scratch\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o4755);
        header.set_cksum();
        tar.append_data(&mut header, "Dockerfile", &contents[..])
            .expect("append file");
        tar.finish().expect("finish tar");
        let source = root.path().join("source");
        std::fs::create_dir(&source).expect("create source");
        safe_extract_context(&valid, &source).expect("extract safe archive");
        assert_eq!(
            std::fs::read(source.join("Dockerfile")).expect("read source"),
            contents
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(source.join("Dockerfile"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o7777,
                0o755
            );
        }

        let linked = root.path().join("linked.tar");
        let file = std::fs::File::create(&linked).expect("create tar");
        let mut tar = tar::Builder::new(file);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_link_name("/etc/passwd").expect("link target");
        header.set_cksum();
        tar.append_data(&mut header, "linked", &[][..])
            .expect("append link");
        tar.finish().expect("finish tar");
        let other_source = root.path().join("other-source");
        std::fs::create_dir(&other_source).expect("create source");
        assert!(safe_extract_context(&linked, &other_source).is_err());
    }

    #[test]
    fn build_logs_are_bounded_without_breaking_utf8() {
        let line = "é".repeat(10_000);
        let bounded = bounded_log_line(line);
        assert!(bounded.len() < 9_000);
        assert!(bounded.ends_with("[truncated]"));
    }
}
