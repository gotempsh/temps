// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deadline handling for Docker exec processes.
//!
//! Docker has no API for cancelling an exec. Dropping Bollard's attached
//! output stream leaves the command running, and killing only the exec leader
//! can orphan shell pipeline children. On timeout, the agent therefore asks
//! the Docker daemon to kill the containing container. This is deliberately
//! disruptive, but it is the only daemon-mediated operation that reliably
//! stops the complete exec workload for both local and remote Docker hosts.
//! The container is restarted rather than left manually stopped so production
//! services recover even when they use an `unless-stopped` restart policy.

use bollard::query_parameters::RestartContainerOptionsBuilder;
use bollard::Docker;
use std::future::Future;
use std::time::Duration;
use tokio::sync::OwnedSemaphorePermit;

const ORPHAN_INITIAL_RETRY: Duration = Duration::from_secs(5);
const ORPHAN_MAX_RETRY: Duration = Duration::from_secs(5 * 60);
const RESTART_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub(crate) enum ExecDeadlineOutcome<T> {
    Completed(T),
    ContainerRestarted,
    TerminationFailed(ExecTerminationError),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ExecTerminationError {
    #[error("Failed to inspect timed-out Docker exec '{exec_id}': {source}")]
    Inspect {
        exec_id: String,
        source: bollard::errors::Error,
    },
    #[error("Timed-out Docker exec '{exec_id}' did not report its container ID")]
    MissingContainerId { exec_id: String },
    #[error("Timed-out Docker exec '{exec_id}' reported malformed container ID '{container_id}'")]
    InvalidContainerId {
        exec_id: String,
        container_id: String,
    },
    #[error(
        "Failed to restart container '{container_id}' for timed-out Docker exec '{exec_id}': {source}"
    )]
    RestartContainer {
        exec_id: String,
        container_id: String,
        source: bollard::errors::Error,
    },
    #[error(
        "Timed out after {timeout_seconds}s restarting container '{container_id}' for Docker exec '{exec_id}'"
    )]
    RestartTimedOut {
        exec_id: String,
        container_id: String,
        timeout_seconds: u64,
    },
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ContainerIdentityError {
    #[error("Failed to inspect Docker container reference '{container_ref}': {source}")]
    Inspect {
        container_ref: String,
        source: bollard::errors::Error,
    },
    #[error("Docker container reference '{container_ref}' did not report its canonical ID")]
    MissingId { container_ref: String },
    #[error(
        "Docker container reference '{container_ref}' reported malformed canonical ID '{container_id}'"
    )]
    InvalidId {
        container_ref: String,
        container_id: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ExecCompletionError {
    #[error(
        "Docker exec '{exec_id}' output ended without a positively stopped state (running={running:?})"
    )]
    NotStopped {
        exec_id: String,
        running: Option<bool>,
    },
    #[error("Stopped Docker exec '{exec_id}' did not report an exit code")]
    MissingExitCode { exec_id: String },
}

/// Accept completion only when Docker positively reports a stopped exec.
/// ExitCode defaults to zero in the Docker API and is not sufficient evidence
/// by itself that an attached command actually ended.
pub(crate) fn completed_exec_exit_code(
    inspect: &bollard::models::ExecInspectResponse,
    exec_id: &str,
) -> Result<i64, ExecCompletionError> {
    if inspect.running != Some(false) {
        return Err(ExecCompletionError::NotStopped {
            exec_id: exec_id.to_string(),
            running: inspect.running,
        });
    }
    inspect
        .exit_code
        .ok_or_else(|| ExecCompletionError::MissingExitCode {
            exec_id: exec_id.to_string(),
        })
}

/// A concrete Docker rejection proves the exec did not start, so restarting a
/// healthy container would be destructive and unnecessary. Transport errors,
/// server errors, and request timeouts remain ambiguous and keep cleanup armed.
pub(crate) fn exec_start_was_rejected(error: &bollard::errors::Error) -> bool {
    matches!(
        error,
        bollard::errors::Error::DockerResponseServerError {
            status_code: 400 | 404 | 409,
            ..
        }
    )
}

/// Cancellation-safe ownership of a running attached exec.
///
/// Axum may drop a handler before its internal deadline (for example when an
/// upstream request times out). Keeping this guard armed across every await
/// ensures that cancellation and ambiguous output/inspection failures retain
/// operation and per-container ownership while starting the same pinned-
/// container restart workflow. Capture capacity is released as soon as its
/// buffers disappear. Only a positively completed exec or a successful
/// synchronous restart may disarm the lifecycle guard.
pub(crate) struct ExecCleanupGuard {
    docker: Docker,
    exec_id: String,
    container_id: String,
    capture_permit: Option<OwnedSemaphorePermit>,
    operation_permit: Option<OwnedSemaphorePermit>,
    container_permit: Option<OwnedSemaphorePermit>,
    armed: bool,
}

impl ExecCleanupGuard {
    pub(crate) fn new(
        docker: Docker,
        exec_id: String,
        container_id: String,
        capture_permit: OwnedSemaphorePermit,
        operation_permit: OwnedSemaphorePermit,
        container_permit: OwnedSemaphorePermit,
    ) -> Self {
        Self {
            docker,
            exec_id,
            container_id,
            capture_permit: Some(capture_permit),
            operation_permit: Some(operation_permit),
            container_permit: Some(container_permit),
            armed: true,
        }
    }

    pub(crate) fn new_without_capture(
        docker: Docker,
        exec_id: String,
        container_id: String,
        operation_permit: Option<OwnedSemaphorePermit>,
        container_permit: OwnedSemaphorePermit,
    ) -> Self {
        Self {
            docker,
            exec_id,
            container_id,
            capture_permit: None,
            operation_permit,
            container_permit: Some(container_permit),
            armed: true,
        }
    }

    pub(crate) fn disarm(&mut self) {
        self.armed = false;
        self.capture_permit.take();
        self.operation_permit.take();
        self.container_permit.take();
    }

    /// Mark a positively completed exec safe while transferring capture
    /// accounting to the HTTP response that now owns its buffered output.
    pub(crate) fn disarm_and_take_capture(&mut self) -> Option<OwnedSemaphorePermit> {
        self.armed = false;
        self.operation_permit.take();
        self.container_permit.take();
        self.capture_permit.take()
    }
}

impl Drop for ExecCleanupGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // The output future and its bounded buffers are gone when the handler
        // drops this guard, so memory-capture capacity can be returned at once.
        // The separate operation permit remains charged until cleanup succeeds
        // and prevents overlapping exec/backup/restore work.
        self.capture_permit.take();
        let Some(operation_permit) = self.operation_permit.take() else {
            let Some(container_permit) = self.container_permit.take() else {
                return;
            };
            hold_permits_until_exec_stops(
                self.docker.clone(),
                self.exec_id.clone(),
                self.container_id.clone(),
                None,
                container_permit,
            );
            return;
        };
        let Some(container_permit) = self.container_permit.take() else {
            return;
        };
        hold_permits_until_exec_stops(
            self.docker.clone(),
            self.exec_id.clone(),
            self.container_id.clone(),
            Some(operation_permit),
            container_permit,
        );
    }
}

/// Resolve an operator-facing container name/ID to Docker's immutable,
/// canonical ID before allocating an exec or reserving per-container work.
pub(crate) async fn resolve_container_id(
    docker: &Docker,
    container_ref: &str,
) -> Result<String, ContainerIdentityError> {
    let inspect = docker
        .inspect_container(
            container_ref,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .map_err(|source| ContainerIdentityError::Inspect {
            container_ref: container_ref.to_string(),
            source,
        })?;
    let container_id = inspect
        .id
        .ok_or_else(|| ContainerIdentityError::MissingId {
            container_ref: container_ref.to_string(),
        })?;
    if !is_canonical_container_id(&container_id) {
        return Err(ContainerIdentityError::InvalidId {
            container_ref: container_ref.to_string(),
            container_id,
        });
    }
    Ok(container_id)
}

/// Resolve and validate the immutable container identity before an exec starts.
///
/// Keeping this value independently of Docker's in-memory exec store means a
/// daemon restart cannot turn a later exec-inspect 404 into false proof that a
/// live-restored workload stopped.
pub(crate) async fn resolve_exec_container_id(
    docker: &Docker,
    exec_id: &str,
) -> Result<String, ExecTerminationError> {
    let inspect =
        docker
            .inspect_exec(exec_id)
            .await
            .map_err(|source| ExecTerminationError::Inspect {
                exec_id: exec_id.to_string(),
                source,
            })?;
    exec_container_id(&inspect, exec_id)
}

pub(crate) async fn run_exec_with_deadline<T, F>(
    docker: &Docker,
    exec_id: &str,
    container_id: &str,
    deadline: Duration,
    operation: F,
) -> ExecDeadlineOutcome<T>
where
    F: Future<Output = T>,
{
    run_with_timeout_cleanup(deadline, operation, || {
        restart_exec_container(docker, exec_id, container_id)
    })
    .await
}

async fn run_with_timeout_cleanup<T, F, C, CleanupFuture>(
    deadline: Duration,
    operation: F,
    cleanup: C,
) -> ExecDeadlineOutcome<T>
where
    F: Future<Output = T>,
    C: FnOnce() -> CleanupFuture,
    CleanupFuture: Future<Output = Result<(), ExecTerminationError>>,
{
    match tokio::time::timeout(deadline, operation).await {
        Ok(output) => ExecDeadlineOutcome::Completed(output),
        Err(_) => match cleanup().await {
            Ok(()) => ExecDeadlineOutcome::ContainerRestarted,
            Err(error) => ExecDeadlineOutcome::TerminationFailed(error),
        },
    }
}

async fn restart_exec_container(
    docker: &Docker,
    exec_id: &str,
    container_id: &str,
) -> Result<(), ExecTerminationError> {
    restart_exec_container_with_timeout(docker, exec_id, container_id, RESTART_REQUEST_TIMEOUT)
        .await
}

async fn restart_exec_container_with_timeout(
    docker: &Docker,
    exec_id: &str,
    container_id: &str,
    request_timeout: Duration,
) -> Result<(), ExecTerminationError> {
    let options = RestartContainerOptionsBuilder::default()
        .signal("SIGKILL")
        .t(0)
        .build();
    let restart = tokio::time::timeout(
        request_timeout,
        docker.restart_container(container_id, Some(options)),
    )
    .await
    .map_err(|_| ExecTerminationError::RestartTimedOut {
        exec_id: exec_id.to_string(),
        container_id: container_id.to_string(),
        timeout_seconds: request_timeout.as_secs(),
    })?;
    match restart {
        Ok(()) => Ok(()),
        // A missing container has no remaining workload to overlap a retry.
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => Ok(()),
        Err(source) => Err(ExecTerminationError::RestartContainer {
            exec_id: exec_id.to_string(),
            container_id: container_id.to_string(),
            source,
        }),
    }
}

fn exec_container_id(
    inspect: &bollard::models::ExecInspectResponse,
    exec_id: &str,
) -> Result<String, ExecTerminationError> {
    let container_id = inspect.container_id.as_deref().ok_or_else(|| {
        ExecTerminationError::MissingContainerId {
            exec_id: exec_id.to_string(),
        }
    })?;
    if !is_canonical_container_id(container_id) {
        return Err(ExecTerminationError::InvalidContainerId {
            exec_id: exec_id.to_string(),
            container_id: container_id.to_string(),
        });
    }
    Ok(container_id.to_string())
}

fn is_canonical_container_id(container_id: &str) -> bool {
    container_id.len() == 64
        && container_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Keep an operation slot charged to an exec whose container could not
/// initially be restarted. The memory-capture slot is released separately
/// when the request ends; this permit prevents overlapping mutating work until
/// Docker confirms the restart (or that the pinned container no longer
/// exists). Retry traffic and journal volume remain bounded during outages.
pub(crate) fn hold_permits_until_exec_stops(
    docker: Docker,
    exec_id: String,
    container_id: String,
    operation_permit: Option<OwnedSemaphorePermit>,
    container_permit: OwnedSemaphorePermit,
) {
    tokio::spawn(async move {
        let mut retry_delay = ORPHAN_INITIAL_RETRY;
        loop {
            match restart_exec_container(&docker, &exec_id, &container_id).await {
                Ok(()) => break,
                Err(error) => {
                    let sleep_for = retry_delay;
                    tracing::warn!(
                        exec_id = %exec_id,
                        container_id = %container_id,
                        retry_seconds = sleep_for.as_secs(),
                        reason = %error,
                        "Timed-out Docker exec restart still pending; retaining lifecycle slots"
                    );
                    retry_delay = retry_delay.saturating_mul(2).min(ORPHAN_MAX_RETRY);
                    tokio::time::sleep(sleep_for).await;
                }
            }
        }
        drop(operation_permit);
        drop(container_permit);
        tracing::info!(exec_id = %exec_id, container_id = %container_id, "Timed-out Docker exec container restarted; released lifecycle slots");
    });
}

/// Retain lifecycle ownership for a successfully started exec that outlives
/// its initiating HTTP/WebSocket request. A positive stopped state releases
/// the guard without disruption; an ambiguous inspect failure drops the armed
/// guard and schedules the pinned-container restart workflow.
pub(crate) fn monitor_exec_until_stopped(
    docker: Docker,
    exec_id: String,
    container_id: String,
    operation: &'static str,
    mut cleanup_guard: ExecCleanupGuard,
) {
    tokio::spawn(async move {
        loop {
            match docker.inspect_exec(&exec_id).await {
                Ok(inspect) if inspect.running == Some(false) => {
                    cleanup_guard.disarm();
                    tracing::info!(exec_id = %exec_id, container_id = %container_id, operation, "Docker exec completed; released lifecycle capacity");
                    break;
                }
                Ok(_) => {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Err(error) => {
                    tracing::error!(exec_id = %exec_id, container_id = %container_id, operation, reason = %error, "Docker exec state became ambiguous; scheduling containing-container restart");
                    break;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Request, State};
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use axum::{Json, Router};
    use std::future::IntoFuture;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    const CONTAINER_ID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    struct MockDockerState {
        restart_calls: AtomicUsize,
        stall_restart: AtomicBool,
        exec_running: AtomicBool,
    }

    async fn mock_docker_api(
        State(state): State<Arc<MockDockerState>>,
        request: Request,
    ) -> Response {
        let path = request.uri().path();
        if path.ends_with("/exec/exec-1/json") {
            return Json(serde_json::json!({
                "Running": state.exec_running.load(Ordering::SeqCst),
                "ContainerID": CONTAINER_ID,
            }))
            .into_response();
        }
        if path.ends_with("/containers/service-a/json") {
            return Json(serde_json::json!({
                "Id": CONTAINER_ID,
            }))
            .into_response();
        }
        if path.ends_with(&format!("/containers/{CONTAINER_ID}/restart")) {
            state.restart_calls.fetch_add(1, Ordering::SeqCst);
            if state.stall_restart.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            return StatusCode::NO_CONTENT.into_response();
        }
        StatusCode::NOT_FOUND.into_response()
    }

    async fn mock_docker(
        stall_restart: bool,
    ) -> Result<
        (
            Docker,
            Arc<MockDockerState>,
            tokio::task::JoinHandle<Result<(), std::io::Error>>,
        ),
        Box<dyn std::error::Error>,
    > {
        let state = Arc::new(MockDockerState {
            restart_calls: AtomicUsize::new(0),
            stall_restart: AtomicBool::new(stall_restart),
            exec_running: AtomicBool::new(true),
        });
        let router = Router::new()
            .fallback(mock_docker_api)
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(axum::serve(listener, router).into_future());
        let docker = Docker::connect_with_http(
            &format!("http://{address}"),
            5,
            bollard::API_DEFAULT_VERSION,
        )?;
        Ok((docker, state, server))
    }

    #[tokio::test]
    async fn timeout_waits_for_cleanup_before_returning() {
        let cleaned_up = Arc::new(AtomicBool::new(false));
        let cleanup_flag = cleaned_up.clone();

        let outcome = run_with_timeout_cleanup(
            Duration::ZERO,
            std::future::pending::<()>(),
            move || async move {
                cleanup_flag.store(true, Ordering::SeqCst);
                Ok(())
            },
        )
        .await;

        assert!(matches!(outcome, ExecDeadlineOutcome::ContainerRestarted));
        assert!(cleaned_up.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn completed_operation_does_not_run_cleanup() {
        let cleaned_up = Arc::new(AtomicBool::new(false));
        let cleanup_flag = cleaned_up.clone();

        let outcome =
            run_with_timeout_cleanup(Duration::from_secs(1), async { 42 }, move || async move {
                cleanup_flag.store(true, Ordering::SeqCst);
                Ok(())
            })
            .await;

        assert!(matches!(outcome, ExecDeadlineOutcome::Completed(42)));
        assert!(!cleaned_up.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn timeout_reports_cleanup_failure() {
        let outcome =
            run_with_timeout_cleanup(Duration::ZERO, std::future::pending::<()>(), || async {
                Err(ExecTerminationError::MissingContainerId {
                    exec_id: "exec-1".to_string(),
                })
            })
            .await;

        assert!(matches!(
            outcome,
            ExecDeadlineOutcome::TerminationFailed(ExecTerminationError::MissingContainerId { .. })
        ));
    }

    #[test]
    fn running_exec_requires_a_canonical_container_id() {
        let missing = bollard::models::ExecInspectResponse {
            running: Some(true),
            ..Default::default()
        };
        assert!(matches!(
            exec_container_id(&missing, "exec-1"),
            Err(ExecTerminationError::MissingContainerId { .. })
        ));

        for malformed in ["", "/", "abc123", &CONTAINER_ID.to_uppercase()] {
            let inspect = bollard::models::ExecInspectResponse {
                running: Some(true),
                container_id: Some(malformed.to_string()),
                ..Default::default()
            };
            assert!(matches!(
                exec_container_id(&inspect, "exec-1"),
                Err(ExecTerminationError::InvalidContainerId { .. })
            ));
        }
    }

    #[test]
    fn canonical_container_id_is_accepted() {
        let inspect = bollard::models::ExecInspectResponse {
            running: Some(true),
            container_id: Some(CONTAINER_ID.to_string()),
            ..Default::default()
        };
        assert!(matches!(
            exec_container_id(&inspect, "exec-1"),
            Ok(container_id) if container_id == CONTAINER_ID
        ));
    }

    #[test]
    fn exit_code_does_not_prove_a_running_exec_completed() {
        for running in [Some(true), None] {
            let inspect = bollard::models::ExecInspectResponse {
                running,
                exit_code: Some(0),
                ..Default::default()
            };
            assert!(matches!(
                completed_exec_exit_code(&inspect, "exec-1"),
                Err(ExecCompletionError::NotStopped { .. })
            ));
        }
    }

    #[test]
    fn stopped_exec_requires_and_returns_its_exit_code() {
        let missing = bollard::models::ExecInspectResponse {
            running: Some(false),
            ..Default::default()
        };
        assert!(matches!(
            completed_exec_exit_code(&missing, "exec-1"),
            Err(ExecCompletionError::MissingExitCode { .. })
        ));

        let completed = bollard::models::ExecInspectResponse {
            running: Some(false),
            exit_code: Some(17),
            ..Default::default()
        };
        assert!(matches!(
            completed_exec_exit_code(&completed, "exec-1"),
            Ok(17)
        ));
    }

    #[test]
    fn only_definitive_docker_start_rejections_skip_cleanup() {
        for status_code in [400, 404, 409] {
            let error = bollard::errors::Error::DockerResponseServerError {
                status_code,
                message: "rejected".to_string(),
            };
            assert!(exec_start_was_rejected(&error));
        }
        let ambiguous = bollard::errors::Error::DockerResponseServerError {
            status_code: 500,
            message: "unknown outcome".to_string(),
        };
        assert!(!exec_start_was_rejected(&ambiguous));
    }

    #[tokio::test]
    async fn exec_container_id_is_pinned_before_start() -> Result<(), Box<dyn std::error::Error>> {
        let (docker, _state, server) = mock_docker(false).await?;

        let result = resolve_exec_container_id(&docker, "exec-1").await;

        server.abort();
        assert!(matches!(result, Ok(container_id) if container_id == CONTAINER_ID));
        Ok(())
    }

    #[tokio::test]
    async fn container_reference_resolves_to_canonical_id_before_admission(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (docker, _state, server) = mock_docker(false).await?;

        let result = resolve_container_id(&docker, "service-a").await;

        server.abort();
        assert!(matches!(result, Ok(container_id) if container_id == CONTAINER_ID));
        Ok(())
    }

    #[tokio::test]
    async fn request_ended_exec_retains_container_ownership_until_stopped(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (docker, state, server) = mock_docker(false).await?;
        let container_slots = Arc::new(tokio::sync::Semaphore::new(1));
        let container_permit = container_slots.clone().acquire_owned().await?;
        let guard = ExecCleanupGuard::new_without_capture(
            docker.clone(),
            "exec-1".to_string(),
            CONTAINER_ID.to_string(),
            None,
            container_permit,
        );

        monitor_exec_until_stopped(
            docker,
            "exec-1".to_string(),
            CONTAINER_ID.to_string(),
            "test terminal",
            guard,
        );
        tokio::task::yield_now().await;
        assert_eq!(container_slots.available_permits(), 0);

        state.exec_running.store(false, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(2), async {
            while container_slots.available_permits() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await?;

        server.abort();
        assert_eq!(container_slots.available_permits(), 1);
        assert_eq!(state.restart_calls.load(Ordering::SeqCst), 0);
        Ok(())
    }

    #[tokio::test]
    async fn monitor_inspect_failure_restarts_before_releasing_container_ownership(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (docker, state, server) = mock_docker(false).await?;
        let container_slots = Arc::new(tokio::sync::Semaphore::new(1));
        let container_permit = container_slots.clone().acquire_owned().await?;
        let guard = ExecCleanupGuard::new_without_capture(
            docker.clone(),
            "missing-exec".to_string(),
            CONTAINER_ID.to_string(),
            None,
            container_permit,
        );

        monitor_exec_until_stopped(
            docker,
            "missing-exec".to_string(),
            CONTAINER_ID.to_string(),
            "test terminal",
            guard,
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            while container_slots.available_permits() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await?;

        server.abort();
        assert_eq!(container_slots.available_permits(), 1);
        assert_eq!(state.restart_calls.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn timeout_cleanup_restarts_the_containing_container(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (docker, state, server) = mock_docker(false).await?;

        let result = restart_exec_container(&docker, "exec-1", CONTAINER_ID).await;

        server.abort();
        assert!(result.is_ok());
        assert_eq!(state.restart_calls.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn successful_restart_releases_held_operation_permit(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (docker, state, server) = mock_docker(false).await?;
        let operation_slots = Arc::new(tokio::sync::Semaphore::new(1));
        let container_slots = Arc::new(tokio::sync::Semaphore::new(1));
        let operation_permit = operation_slots.clone().acquire_owned().await?;
        let container_permit = container_slots.clone().acquire_owned().await?;
        assert_eq!(operation_slots.available_permits(), 0);

        hold_permits_until_exec_stops(
            docker,
            "exec-1".to_string(),
            CONTAINER_ID.to_string(),
            Some(operation_permit),
            container_permit,
        );
        tokio::time::timeout(Duration::from_secs(1), async {
            while operation_slots.available_permits() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await?;

        server.abort();
        assert_eq!(operation_slots.available_permits(), 1);
        assert_eq!(container_slots.available_permits(), 1);
        assert_eq!(state.restart_calls.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn definitively_rejected_start_does_not_restart_container(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (docker, state, server) = mock_docker(false).await?;
        let capture_slots = Arc::new(tokio::sync::Semaphore::new(1));
        let operation_slots = Arc::new(tokio::sync::Semaphore::new(1));
        let container_slots = Arc::new(tokio::sync::Semaphore::new(1));
        let capture_permit = capture_slots.clone().acquire_owned().await?;
        let operation_permit = operation_slots.clone().acquire_owned().await?;
        let container_permit = container_slots.clone().acquire_owned().await?;
        let mut guard = ExecCleanupGuard::new(
            docker,
            "exec-1".to_string(),
            CONTAINER_ID.to_string(),
            capture_permit,
            operation_permit,
            container_permit,
        );
        let rejection = bollard::errors::Error::DockerResponseServerError {
            status_code: 409,
            message: "container is paused".to_string(),
        };

        if exec_start_was_rejected(&rejection) {
            guard.disarm();
        }
        drop(guard);
        tokio::task::yield_now().await;

        server.abort();
        assert_eq!(state.restart_calls.load(Ordering::SeqCst), 0);
        assert_eq!(capture_slots.available_permits(), 1);
        assert_eq!(operation_slots.available_permits(), 1);
        assert_eq!(container_slots.available_permits(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn canceled_request_restarts_container_before_releasing_permit(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (docker, state, server) = mock_docker(false).await?;
        let capture_slots = Arc::new(tokio::sync::Semaphore::new(1));
        let operation_slots = Arc::new(tokio::sync::Semaphore::new(1));
        let container_slots = Arc::new(tokio::sync::Semaphore::new(1));
        let capture_permit = capture_slots.clone().acquire_owned().await?;
        let operation_permit = operation_slots.clone().acquire_owned().await?;
        let container_permit = container_slots.clone().acquire_owned().await?;
        let guard = ExecCleanupGuard::new(
            docker,
            "exec-1".to_string(),
            CONTAINER_ID.to_string(),
            capture_permit,
            operation_permit,
            container_permit,
        );

        let request = tokio::spawn(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        });
        request.abort();
        let _cancellation = request.await;

        tokio::time::timeout(Duration::from_secs(1), async {
            while capture_slots.available_permits() == 0 || operation_slots.available_permits() == 0
            {
                tokio::task::yield_now().await;
            }
        })
        .await?;

        server.abort();
        assert_eq!(state.restart_calls.load(Ordering::SeqCst), 1);
        assert_eq!(capture_slots.available_permits(), 1);
        assert_eq!(operation_slots.available_permits(), 1);
        assert_eq!(container_slots.available_permits(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn pending_cleanup_releases_capture_but_retains_operation_capacity(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (docker, state, server) = mock_docker(true).await?;
        let capture_slots = Arc::new(tokio::sync::Semaphore::new(1));
        let operation_slots = Arc::new(tokio::sync::Semaphore::new(1));
        let container_slots = Arc::new(tokio::sync::Semaphore::new(1));
        let capture_permit = capture_slots.clone().acquire_owned().await?;
        let operation_permit = operation_slots.clone().acquire_owned().await?;
        let container_permit = container_slots.clone().acquire_owned().await?;
        let guard = ExecCleanupGuard::new(
            docker,
            "exec-1".to_string(),
            CONTAINER_ID.to_string(),
            capture_permit,
            operation_permit,
            container_permit,
        );

        drop(guard);
        tokio::time::timeout(Duration::from_secs(1), async {
            while state.restart_calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await?;

        assert_eq!(capture_slots.available_permits(), 1);
        assert_eq!(operation_slots.available_permits(), 0);
        assert_eq!(container_slots.available_permits(), 0);
        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn stalled_restart_request_is_bounded() -> Result<(), Box<dyn std::error::Error>> {
        let (docker, state, server) = mock_docker(true).await?;

        let result = restart_exec_container_with_timeout(
            &docker,
            "exec-1",
            CONTAINER_ID,
            Duration::from_millis(10),
        )
        .await;

        server.abort();
        assert!(matches!(
            result,
            Err(ExecTerminationError::RestartTimedOut { .. })
        ));
        assert_eq!(state.restart_calls.load(Ordering::SeqCst), 1);
        Ok(())
    }
}
