// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Control-plane client for the worker's Compose endpoints
//! (see [`crate::compose_protocol`]).

use std::path::Path;
use std::time::Duration;

use futures::{StreamExt, TryStreamExt};
use serde::Serialize;

use super::{agent_error_detail, AgentResponse, RemoteNodeDeployer};
use crate::compose::{ComposeDeployFailure, ComposeError, ComposeOutputSink, ComposeServiceResult};
use crate::compose_protocol::{
    pack_compose_stack, ComposeAck, ComposeGenerationRequest, ComposeHostEvent, ComposePrepareSpec,
    ComposePruneRequest, ComposeRemoveVerifiedRequest, ComposeStackRequest,
    COMPOSE_PREPARE_DEADLINE, COMPOSE_PREPARE_QUEUE_TIMEOUT, MAX_COMPOSE_EVENT_BYTES,
    MAX_COMPOSE_SPEC_BYTES,
};
use temps_presets::compose_check::ComposeCheckErrorClass;

/// Prepare waits for an image-operation slot on the worker and then builds
/// and pulls there, each bounded by the worker. This outlasts both together,
/// with a margin for the upload and for waiting on the project's lifecycle
/// lock, so the worker's own timeout answer arrives before this one fires.
const COMPOSE_PREPARE_REQUEST_TIMEOUT: Duration = Duration::from_secs(
    COMPOSE_PREPARE_QUEUE_TIMEOUT.as_secs() + COMPOSE_PREPARE_DEADLINE.as_secs() + 15 * 60,
);
/// Starting a stack waits for its services to become ready, bounded by the
/// project's startup timeout; this outlasts any value the setting allows.
const COMPOSE_DEPLOY_REQUEST_TIMEOUT: Duration = Duration::from_secs(61 * 60);
/// `docker compose down`/`stop` give each container 30 seconds to exit.
const COMPOSE_LIFECYCLE_REQUEST_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// What a terminal event resolved to for the operation that asked.
enum Terminal {
    Prepared,
    Deployed(Vec<ComposeServiceResult>),
    Failed(Box<ComposeDeployFailure>),
}

impl RemoteNodeDeployer {
    fn compose_transport_error(&self, operation: &str, reason: String) -> ComposeError {
        ComposeError::Remote {
            node_name: self.node_name.clone(),
            message: format!("Compose {operation} on worker failed: {reason}"),
            class: ComposeCheckErrorClass::Internal,
        }
    }

    /// Send a generation's files to the worker and build/pull it there. The
    /// stack that is running keeps running.
    pub async fn compose_prepare(
        &self,
        spec: &ComposePrepareSpec,
        stack_dir: Option<&Path>,
        sink: Option<&ComposeOutputSink>,
    ) -> Result<(), ComposeError> {
        let spec_json = serde_json::to_string(spec)
            .map_err(|error| self.compose_transport_error("prepare", error.to_string()))?;
        if spec_json.len() > MAX_COMPOSE_SPEC_BYTES {
            return Err(ComposeError::Remote {
                node_name: self.node_name.clone(),
                message: format!(
                    "The Compose deployment for '{}' is {} bytes once its variables and secrets \
                     are included, more than the {MAX_COMPOSE_SPEC_BYTES} bytes a worker \
                     accepts; reduce the size of its variables or secrets",
                    spec.project_name,
                    spec_json.len()
                ),
                class: ComposeCheckErrorClass::Internal,
            });
        }
        let mut form = reqwest::multipart::Form::new().text("spec", spec_json);
        // Held until the upload has finished.
        let mut archive_guard = None;
        if let Some(stack_dir) = stack_dir {
            let stack_dir = stack_dir.to_path_buf();
            let archive = tokio::task::spawn_blocking(move || pack_compose_stack(&stack_dir))
                .await
                .map_err(|error| self.compose_transport_error("prepare", error.to_string()))??;
            let size = archive
                .as_file()
                .metadata()
                .map_err(ComposeError::Io)?
                .len();
            let file = tokio::fs::File::from_std(archive.reopen().map_err(ComposeError::Io)?);
            let stream =
                tokio_util::codec::FramedRead::new(file, tokio_util::codec::BytesCodec::new());
            let body = reqwest::Body::wrap_stream(stream.map_ok(|bytes| bytes.freeze()));
            let part = reqwest::multipart::Part::stream_with_length(body, size)
                .file_name("stack.tar")
                .mime_str("application/x-tar")
                .map_err(|error| self.compose_transport_error("prepare", error.to_string()))?;
            form = form.part("stack", part);
            archive_guard = Some(archive);
        }
        let url = format!("{}/agent/compose/prepare", self.agent_url);
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .timeout(COMPOSE_PREPARE_REQUEST_TIMEOUT)
            .multipart(form)
            .send()
            .await
            .map_err(|error| {
                self.compose_transport_error("prepare", format!("cannot reach {url}: {error}"))
            })?;
        drop(archive_guard);
        match self.read_compose_events(response, "prepare", sink).await? {
            Terminal::Prepared => Ok(()),
            Terminal::Deployed(_) => Err(self.compose_transport_error(
                "prepare",
                "the worker answered a prepare with a deploy result".to_string(),
            )),
            Terminal::Failed(failure) => Err(failure.error),
        }
    }

    /// Start a generation the worker prepared.
    pub async fn compose_deploy(
        &self,
        project_name: &str,
        generation: &str,
        sink: Option<&ComposeOutputSink>,
    ) -> Result<Vec<ComposeServiceResult>, Box<ComposeDeployFailure>> {
        let wrap = |error: ComposeError| {
            Box::new(ComposeDeployFailure {
                error,
                containers: Vec::new(),
                cleanup_containers: Vec::new(),
                retention_error: None,
            })
        };
        let url = format!("{}/agent/compose/deploy", self.agent_url);
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .timeout(COMPOSE_DEPLOY_REQUEST_TIMEOUT)
            .json(&ComposeGenerationRequest {
                project_name: project_name.to_string(),
                generation: generation.to_string(),
            })
            .send()
            .await
            .map_err(|error| {
                wrap(self.compose_transport_error("deploy", format!("cannot reach {url}: {error}")))
            })?;
        match self
            .read_compose_events(response, "deploy", sink)
            .await
            .map_err(wrap)?
        {
            Terminal::Deployed(services) => Ok(services),
            Terminal::Prepared => Err(wrap(self.compose_transport_error(
                "deploy",
                "the worker answered a deploy with a prepare result".to_string(),
            ))),
            Terminal::Failed(failure) => Err(failure),
        }
    }

    /// `docker compose down` on the worker. Volumes are always kept.
    pub async fn compose_teardown(
        &self,
        request: &ComposeStackRequest,
    ) -> Result<(), ComposeError> {
        self.compose_post("/agent/compose/teardown", "teardown", request)
            .await
    }

    /// `docker compose stop` on the worker.
    pub async fn compose_stop(&self, request: &ComposeStackRequest) -> Result<(), ComposeError> {
        self.compose_post("/agent/compose/stop", "stop", request)
            .await
    }

    /// Forget a prepared generation that will not be started, removing its
    /// files and secrets from the worker.
    pub async fn compose_discard_generation(
        &self,
        project_name: &str,
        generation: &str,
    ) -> Result<(), ComposeError> {
        self.compose_post(
            "/agent/compose/discard",
            "discard",
            &ComposeGenerationRequest {
                project_name: project_name.to_string(),
                generation: generation.to_string(),
            },
        )
        .await
    }

    /// Remove every secret generation of a project except `keep_generation`.
    pub async fn compose_prune_secret_generations(
        &self,
        project_name: &str,
        keep_generation: Option<&str>,
    ) -> Result<(), ComposeError> {
        self.compose_post(
            "/agent/compose/prune-secrets",
            "secret cleanup",
            &ComposePruneRequest {
                project_name: project_name.to_string(),
                keep_generation: keep_generation.map(str::to_string),
            },
        )
        .await
    }

    /// Force-remove containers on the worker after it re-verifies their
    /// Temps labels.
    pub async fn compose_remove_verified_containers(
        &self,
        request: &ComposeRemoveVerifiedRequest,
    ) -> Result<(), ComposeError> {
        self.compose_post(
            "/agent/compose/remove-verified",
            "container removal",
            request,
        )
        .await
    }

    async fn compose_post<B: Serialize>(
        &self,
        path: &str,
        operation: &str,
        body: &B,
    ) -> Result<(), ComposeError> {
        let url = format!("{}{}", self.agent_url, path);
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .timeout(COMPOSE_LIFECYCLE_REQUEST_TIMEOUT)
            .json(body)
            .send()
            .await
            .map_err(|error| {
                self.compose_transport_error(operation, format!("cannot reach {url}: {error}"))
            })?;
        if !response.status().is_success() {
            let status = response.status();
            let detail = agent_error_detail(response).await;
            return Err(self.compose_transport_error(
                operation,
                format!(
                    "{url} answered HTTP {status}: {detail}{}",
                    upgrade_hint(status)
                ),
            ));
        }
        let body: AgentResponse<ComposeAck> = response.json().await.map_err(|error| {
            self.compose_transport_error(operation, format!("invalid response from {url}: {error}"))
        })?;
        if body.success {
            Ok(())
        } else {
            Err(self.compose_transport_error(operation, body.error.unwrap_or_default()))
        }
    }

    async fn read_compose_events(
        &self,
        response: reqwest::Response,
        operation: &str,
        sink: Option<&ComposeOutputSink>,
    ) -> Result<Terminal, ComposeError> {
        if !response.status().is_success() {
            let status = response.status();
            let detail = agent_error_detail(response).await;
            return Err(self.compose_transport_error(
                operation,
                format!(
                    "the worker refused it (HTTP {status}): {detail}{}",
                    upgrade_hint(status)
                ),
            ));
        }
        let mut chunks = response.bytes_stream();
        let mut pending = Vec::new();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|error| {
                self.compose_transport_error(operation, format!("lost the event stream: {error}"))
            })?;
            for fragment in chunk.split_inclusive(|byte| *byte == b'\n') {
                let complete = fragment.last() == Some(&b'\n');
                let data = if complete {
                    &fragment[..fragment.len() - 1]
                } else {
                    fragment
                };
                if pending.len().saturating_add(data.len()) > MAX_COMPOSE_EVENT_BYTES {
                    return Err(self.compose_transport_error(
                        operation,
                        "the worker sent an oversized event".to_string(),
                    ));
                }
                pending.extend_from_slice(data);
                if !complete {
                    continue;
                }
                if !pending.is_empty() {
                    let event: ComposeHostEvent =
                        serde_json::from_slice(&pending).map_err(|error| {
                            self.compose_transport_error(
                                operation,
                                format!("the worker sent an invalid event: {error}"),
                            )
                        })?;
                    match event {
                        ComposeHostEvent::Output(lines) => {
                            if let Some(sink) = sink {
                                (sink.0)(lines).await;
                            }
                        }
                        ComposeHostEvent::Prepared => return Ok(Terminal::Prepared),
                        ComposeHostEvent::Deployed(services) => {
                            return Ok(Terminal::Deployed(services))
                        }
                        ComposeHostEvent::Failed(failure) => {
                            return Ok(Terminal::Failed(Box::new(
                                failure.into_deploy_failure(&self.node_name),
                            )))
                        }
                    }
                }
                pending.clear();
            }
        }
        Err(self.compose_transport_error(
            operation,
            "the event stream ended without a result; the worker may have restarted".to_string(),
        ))
    }
}

/// An agent that predates Compose support answers its routes with 404.
fn upgrade_hint(status: reqwest::StatusCode) -> &'static str {
    if status == reqwest::StatusCode::NOT_FOUND {
        ". This worker runs a Temps agent without Docker Compose support; upgrade Temps on the \
         worker"
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The control plane must not give up on a prepare the worker may still
    /// legitimately be running: a slow queue followed by a long build would
    /// otherwise fail as a transport timeout instead of the worker's answer.
    #[test]
    fn a_prepare_request_outlasts_the_worker_queue_and_deadline() {
        assert!(
            COMPOSE_PREPARE_REQUEST_TIMEOUT
                > COMPOSE_PREPARE_QUEUE_TIMEOUT + COMPOSE_PREPARE_DEADLINE
        );
    }
}
