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
    BuildRequest, BuildRequestWithCallback, BuilderError,
};
use tokio::io::AsyncWriteExt;

use crate::handlers::{error_response, AgentResourceLimits, AgentState};

const BUILD_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30 * 60);

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
        (status = 503, description = "Worker build capacity unavailable"),
        (status = 504, description = "Build admission or upload deadline exceeded")
    ),
    security(("bearer_auth" = []))
)]
pub async fn build_image(
    State(state): State<Arc<AgentState>>,
    Extension(limits): Extension<Arc<AgentResourceLimits>>,
    mut multipart: Multipart,
) -> Response {
    let mut spec_field = match multipart.next_field().await {
        Ok(Some(field)) if field.name() == Some("spec") => field,
        _ => {
            return error_response(StatusCode::BAD_REQUEST, "Expected spec field first".into())
                .into_response()
        }
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
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "Build spec is invalid or too large".into(),
                )
                .into_response()
            }
        }
    }
    // Multer only advances to the next part after this Field is dropped,
    // even when chunk() has returned None. Keep this before next_field().
    drop(spec_field);
    let spec: BuildSpec = match serde_json::from_slice(&spec_bytes) {
        Ok(spec) => spec,
        Err(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "Build spec is not valid JSON".into(),
            )
            .into_response()
        }
    };
    if let Err(message) = spec.validate() {
        return error_response(StatusCode::BAD_REQUEST, message).into_response();
    }

    let deadline = tokio::time::Instant::now() + BUILD_DEADLINE;
    let permit =
        match tokio::time::timeout_at(deadline, limits.image_import_slots.clone().acquire_owned())
            .await
        {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => {
                return error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Worker image operation capacity unavailable".into(),
                )
                .into_response()
            }
            Err(_) => {
                return error_response(
                    StatusCode::GATEWAY_TIMEOUT,
                    "Worker build waited 30 minutes for capacity".into(),
                )
                .into_response()
            }
        };
    let scratch = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(error) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Cannot create worker build scratch directory: {error}"),
            )
            .into_response()
        }
    };
    let archive_path = scratch.path().join("context.tar");
    let mut archive_file = match tokio::fs::File::create(&archive_path).await {
        Ok(file) => file,
        Err(error) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Cannot create worker build archive: {error}"),
            )
            .into_response()
        }
    };
    let mut context = match multipart.next_field().await {
        Ok(Some(field)) if field.name() == Some("context") => field,
        _ => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "Expected context field after spec".into(),
            )
            .into_response()
        }
    };
    let mut received = 0_u64;
    loop {
        let chunk = match tokio::time::timeout_at(deadline, context.chunk()).await {
            Ok(Ok(Some(chunk))) => chunk,
            Ok(Ok(None)) => break,
            Ok(Err(error)) => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    format!("Build context upload failed: {error}"),
                )
                .into_response()
            }
            Err(_) => {
                return error_response(
                    StatusCode::GATEWAY_TIMEOUT,
                    "Build context upload exceeded 30 minutes".into(),
                )
                .into_response()
            }
        };
        received = received.saturating_add(chunk.len() as u64);
        if received > MAX_BUILD_CONTEXT_BYTES {
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("Build context exceeds the {MAX_BUILD_CONTEXT_BYTES}-byte limit"),
            )
            .into_response();
        }
        if let Err(error) = archive_file.write_all(&chunk).await {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Cannot write worker build archive: {error}"),
            )
            .into_response();
        }
    }
    drop(archive_file);
    drop(context);
    match multipart.next_field().await {
        Ok(None) => {}
        _ => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "Unexpected field after build context".into(),
            )
            .into_response()
        }
    }
    let context_dir = scratch.path().join("source");
    if let Err(error) = std::fs::create_dir(&context_dir) {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Cannot create worker source directory: {error}"),
        )
        .into_response();
    }
    let archive_for_extract = archive_path.clone();
    let context_for_extract = context_dir.clone();
    match tokio::task::spawn_blocking(move || {
        safe_extract_context(&archive_for_extract, &context_for_extract)
    })
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(message)) => {
            return error_response(StatusCode::BAD_REQUEST, message).into_response()
        }
        Err(error) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Worker build extraction task failed: {error}"),
            )
            .into_response()
        }
    }
    let dockerfile = context_dir.join(&spec.dockerfile);
    if !dockerfile.is_file() {
        return error_response(
            StatusCode::BAD_REQUEST,
            format!(
                "Dockerfile '{}' is absent from the uploaded context",
                spec.dockerfile
            ),
        )
        .into_response();
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
    let mut task = tokio::spawn(async move {
        let _permit = permit;
        let _scratch = scratch;
        builder
            .build_image_with_callback(BuildRequestWithCallback {
                request,
                log_callback: Some(callback),
            })
            .await
    });
    tokio::spawn(async move {
        let event = match tokio::time::timeout_at(deadline, &mut task).await {
            Ok(Ok(Ok(result))) => BuildEvent::Result(result),
            Ok(Ok(Err(error))) => BuildEvent::Failure(BuildFailure {
                kind: if matches!(
                    error,
                    BuilderError::BuildFailed(_) | BuilderError::BuildOutOfMemory { .. }
                ) {
                    BuildFailureKind::Build
                } else {
                    BuildFailureKind::Worker
                },
                message: error.to_string(),
            }),
            Ok(Err(error)) => BuildEvent::Failure(BuildFailure {
                kind: BuildFailureKind::Worker,
                message: format!("Worker build task failed: {error}"),
            }),
            Err(_) => BuildEvent::Failure(BuildFailure {
                kind: BuildFailureKind::Timeout,
                message: "Worker build exceeded its 30-minute deadline".into(),
            }),
        };
        let _ = tx.send(event).await;
    });
    let output = stream::unfold(rx, |mut rx| async move {
        rx.recv()
            .await
            .map(|event| (Ok::<_, Infallible>(event_bytes(event)), rx))
    });
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/x-ndjson")],
        Body::from_stream(output),
    )
        .into_response()
}

/// Query for `GET /agent/images/export`. The reference travels as a query
/// parameter because image names contain `/` and `:`.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ExportImageQuery {
    /// Image reference to export, e.g. `temps-app:3f2a`.
    pub image: String,
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
        (status = 503, description = "Image operation capacity unavailable"),
        (status = 504, description = "Timed out waiting for image operation capacity")
    ),
    security(("bearer_auth" = []))
)]
pub async fn export_image(
    State(state): State<Arc<AgentState>>,
    Extension(limits): Extension<Arc<AgentResourceLimits>>,
    Query(query): Query<ExportImageQuery>,
) -> Response {
    let image = query.image;
    if image.is_empty() || image.len() > 256 || image.chars().any(char::is_whitespace) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "Image reference must be 1–256 characters without whitespace".into(),
        )
        .into_response();
    }
    let deadline = tokio::time::Instant::now() + BUILD_DEADLINE;
    let permit =
        match tokio::time::timeout_at(deadline, limits.image_import_slots.clone().acquire_owned())
            .await
        {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => {
                return error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Worker image operation capacity unavailable".into(),
                )
                .into_response()
            }
            Err(_) => {
                return error_response(
                    StatusCode::GATEWAY_TIMEOUT,
                    format!("Export of '{image}' waited 30 minutes for capacity"),
                )
                .into_response()
            }
        };
    let exported = match state.image_builder.export_image_stream(&image).await {
        Ok(stream) => stream,
        Err(BuilderError::ImageNotFound(_)) => {
            return error_response(
                StatusCode::NOT_FOUND,
                format!("Image '{image}' is not present on this node"),
            )
            .into_response()
        }
        Err(error) => {
            tracing::error!(image = %image, "Image export failed to start: {error}");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Cannot export image '{image}': {error}"),
            )
            .into_response();
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
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/x-tar")],
        Body::from_stream(body),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_extracts_regular_files_and_rejects_links() {
        let root = tempfile::tempdir().expect("scratch");
        let valid = root.path().join("valid.tar");
        let file = std::fs::File::create(&valid).expect("create tar");
        let mut tar = tar::Builder::new(file);
        let contents = b"FROM scratch\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
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
