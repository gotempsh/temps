// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `POST /agent/plugins/build`: compile an external plugin for a control plane
//! that has no Docker of its own (a hosted control plane).
//!
//! The control plane streams a tar archive of the plugin source; this node
//! compiles it in the same pinned, network-cut, resource-bounded Bun
//! container a control plane with Docker would use, and streams the binary
//! back. The archive is never extracted on this node's filesystem: it is
//! written to a private temporary file and unpacked inside the container.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use serde::Deserialize;
use temps_authz::permission_guard;
use temps_core::problemdetails::Problem;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::auth::RequireAgentAuth;
use crate::handlers::{error_response, AgentState};

/// Why a plugin did not compile on this node.
#[derive(Debug, thiserror::Error)]
pub enum PluginBuildError {
    /// There is neither `docker` nor `podman` here. Nothing was compiled.
    #[error("this node has no Docker or Podman to build plugins with")]
    RuntimeMissing,
    /// The container engine ran and the build failed.
    #[error("plugin build failed: {reason}")]
    Failed { reason: String },
}

/// Compiles a plugin in the pinned, resource-bounded build container. The
/// binary that starts the agent provides it (`temps-external-plugins`).
#[async_trait]
pub trait PluginContainerBuilder: Send + Sync {
    /// Compile the source tar at `archive` for `target`, writing the binary to
    /// `output`. The archive is unpacked inside the container, never here.
    async fn build_from_archive(
        &self,
        archive: &Path,
        target: &str,
        output: &Path,
    ) -> Result<(), PluginBuildError>;
}

/// Same cap the control plane applies before it sends the archive.
const MAX_SOURCE_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;

/// One plugin build at a time: each takes a CPU and 1.5 GiB for several
/// minutes, and a worker also runs the customer's applications.
static BUILD_SLOT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

/// The Linux targets a control plane runs on. A build for anything else is
/// refused rather than handed to Bun.
const ALLOWED_TARGETS: &[&str] = &[
    "bun-linux-x64-musl",
    "bun-linux-arm64-musl",
    "bun-linux-x64-baseline",
    "bun-linux-arm64",
];

#[derive(Debug, Deserialize)]
pub struct PluginBuildQuery {
    target: String,
}

/// Compile a plugin from a streamed source archive.
#[utoipa::path(
    tag = "Plugins",
    post,
    path = "/agent/plugins/build",
    params(("target" = String, Query, description = "Bun compile target of the machine the plugin will run on")),
    request_body(content = Vec<u8>, content_type = "application/x-tar"),
    responses(
        (status = 200, description = "The compiled plugin binary", content_type = "application/octet-stream", body = Vec<u8>),
        (status = 400, description = "Unsupported target or unreadable archive"),
        (status = 413, description = "Source archive too large"),
        (status = 500, description = "The build failed"),
        (status = 501, description = "This node has no Docker or Podman to build with"),
        (status = 503, description = "A plugin build is already running on this node")
    ),
    security(("bearer_auth" = []))
)]
pub async fn build_plugin(
    RequireAgentAuth(auth): RequireAgentAuth,
    State(state): State<Arc<AgentState>>,
    Query(query): Query<PluginBuildQuery>,
    body: Body,
) -> Result<Response, Problem> {
    permission_guard!(auth, DeploymentsCreate);
    if !ALLOWED_TARGETS.contains(&query.target.as_str()) {
        return Ok(error_response(
            StatusCode::BAD_REQUEST,
            format!("unsupported plugin compile target '{}'", query.target),
        )
        .into_response());
    }
    let Ok(_slot) = BUILD_SLOT.try_acquire() else {
        return Ok(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "a plugin build is already running on this node".to_string(),
        )
        .into_response());
    };
    let work = match tempfile::tempdir() {
        Ok(work) => work,
        Err(error) => {
            return Ok(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not create a build directory: {error}"),
            )
            .into_response())
        }
    };
    let archive = work.path().join("source.tar");
    let output = work.path().join("plugin");

    // Stage the archive, bounded.
    let mut file = match tokio::fs::File::create(&archive).await {
        Ok(file) => file,
        Err(error) => {
            return Ok(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not stage the source archive: {error}"),
            )
            .into_response())
        }
    };
    let mut received: u64 = 0;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                return Ok(error_response(
                    StatusCode::BAD_REQUEST,
                    format!("the source upload was interrupted: {error}"),
                )
                .into_response())
            }
        };
        received += chunk.len() as u64;
        if received > MAX_SOURCE_ARCHIVE_BYTES {
            return Ok(error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("source archive exceeds {MAX_SOURCE_ARCHIVE_BYTES} bytes"),
            )
            .into_response());
        }
        if let Err(error) = file.write_all(&chunk).await {
            return Ok(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not stage the source archive: {error}"),
            )
            .into_response());
        }
    }
    if let Err(error) = file.flush().await {
        return Ok(error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not stage the source archive: {error}"),
        )
        .into_response());
    }
    drop(file);

    match state
        .plugin_builder
        .build_from_archive(&archive, &query.target, &output)
        .await
    {
        Ok(()) => {}
        Err(PluginBuildError::RuntimeMissing) => {
            return Ok(error_response(
                StatusCode::NOT_IMPLEMENTED,
                "this node has no Docker or Podman to build plugins with".to_string(),
            )
            .into_response())
        }
        Err(PluginBuildError::Failed { reason }) => {
            tracing::warn!(%reason, "plugin build failed on this node");
            return Ok(error_response(StatusCode::INTERNAL_SERVER_ERROR, reason).into_response());
        }
    }

    let length = match tokio::fs::metadata(&output).await {
        Ok(metadata) => metadata.len(),
        Err(error) => {
            return Ok(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("the build produced no binary: {error}"),
            )
            .into_response())
        }
    };
    let compiled = match tokio::fs::File::open(&output).await {
        Ok(compiled) => compiled,
        Err(error) => {
            return Ok(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not read the compiled binary: {error}"),
            )
            .into_response())
        }
    };
    // Keep the directory and the slot alive until the last byte has been read.
    let stream = futures::stream::unfold(
        (compiled, work, Vec::<u8>::new()),
        |(mut compiled, work, mut buffer)| async move {
            buffer.clear();
            buffer.resize(64 * 1024, 0);
            match compiled.read(&mut buffer).await {
                Ok(0) => None,
                Ok(read) => {
                    let chunk = bytes::Bytes::copy_from_slice(&buffer[..read]);
                    Some((Ok::<_, std::io::Error>(chunk), (compiled, work, buffer)))
                }
                Err(error) => Some((Err(error), (compiled, work, buffer))),
            }
        },
    );
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (header::CONTENT_LENGTH, length.to_string()),
        ],
        Body::from_stream(stream),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_engines::{agent_state, service_runtime, PluginBuild, StubPluginBuilder};
    use axum::body::to_bytes;
    use axum::routing::post;
    use axum::{Extension, Router};
    use tower::ServiceExt as _;

    fn router(outcome: PluginBuild) -> Router {
        Router::new()
            .route("/build", post(build_plugin))
            .layer(Extension(Arc::new(crate::auth::AgentAuth::new(
                "node-token",
            ))))
            .with_state(agent_state(
                service_runtime(),
                Arc::new(StubPluginBuilder { outcome }),
            ))
    }

    fn request(token: Option<&str>, target: &str) -> axum::http::Request<Body> {
        let mut request = axum::http::Request::builder()
            .method("POST")
            .uri(format!("/build?target={target}"));
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        request.body(Body::from("archive")).expect("build request")
    }

    #[tokio::test]
    async fn a_build_needs_the_agent_token() {
        for token in [None, Some("wrong-token")] {
            let response = router(PluginBuild::WritesBinary)
                .oneshot(request(token, "bun-linux-arm64"))
                .await
                .expect("respond");
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn a_target_outside_the_list_never_reaches_the_builder() {
        let response = router(PluginBuild::WritesBinary)
            .oneshot(request(Some("node-token"), "bun-windows-x64"))
            .await
            .expect("respond");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// One test for the three builder outcomes: the handler admits one build
    /// at a time, so parallel tests would see each other's slot.
    #[tokio::test]
    async fn each_builder_outcome_has_its_status() {
        let response = router(PluginBuild::RuntimeMissing)
            .oneshot(request(Some("node-token"), "bun-linux-arm64"))
            .await
            .expect("respond");
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);

        let response = router(PluginBuild::Fails)
            .oneshot(request(Some("node-token"), "bun-linux-arm64"))
            .await
            .expect("respond");
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read the response");
        assert!(String::from_utf8_lossy(&body).contains("bun exited 1"));

        let response = router(PluginBuild::WritesBinary)
            .oneshot(request(Some("node-token"), "bun-linux-arm64"))
            .await
            .expect("respond");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read the response");
        assert_eq!(&body[..], b"compiled");
    }
}
