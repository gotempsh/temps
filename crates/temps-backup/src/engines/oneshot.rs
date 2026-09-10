// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One-shot Docker container helper for backup engines.
//!
//! Replaces the old "create a sleeping sidecar then `docker exec` against
//! it" pattern with a single `docker run` whose **entrypoint IS the
//! backup command**. The container's exit code becomes the backup result.
//!
//! ## Why this design
//!
//! The old pattern created a long-lived container (`sleep 86400`) and
//! issued one or more `docker exec` calls against it. Every exit path
//! (success, error, panic, lost lease, cancelled await) had to remember
//! to remove the container. Miss one and the container survives for
//! 24 hours — the prod leak we just hunted.
//!
//! Replacing it with `docker run` + `auto_remove=true` + the real
//! command as entrypoint means:
//!
//! - The container's lifetime equals the backup command's lifetime.
//! - When the command exits (success, failure, OOM), Docker reaps the
//!   container automatically via `auto_remove`.
//! - When the host process dies, an `auto_remove` container exits when
//!   the daemon notices and is reaped. No `sleep 86400` to time out.
//! - When the caller cancels, we send the container a SIGTERM via
//!   `docker stop`. The container exits, `wait_container` returns,
//!   `auto_remove` reaps.
//!
//! On every path the container is gone within seconds of the work
//! ending. No RAII guard, no janitor, no label-based reaper — the
//! Docker primitives already give us the guarantee.
//!
//! ## Output capture
//!
//! Most backup commands write their primary output (the dump file) to a
//! bind-mounted host directory and write progress/errors to stdout +
//! stderr. We attach to the container's stdout/stderr stream and keep
//! a bounded ring buffer of the last 4 KiB of stderr for the failure
//! message. We do NOT stream the full output — that's the engine's
//! job (e.g. by piping the dump file from the bind mount up to S3
//! after the container exits).

use std::collections::HashMap;
use std::time::Duration;

use bollard::container::LogOutput;
use bollard::exec::StartExecResults;
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, RemoveContainerOptionsBuilder, StopContainerOptionsBuilder,
    WaitContainerOptionsBuilder,
};
use bollard::Docker;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::ring_buffer::RingBuffer;

/// Spec for a one-shot backup container. Engines build this and pass it
/// to [`run_one_shot`]. Everything the helper needs to declare and run
/// the container is in this struct; engines do not call bollard
/// directly.
#[derive(Debug, Clone)]
pub struct OneShotSpec {
    /// `image:tag`. Caller is responsible for ensuring the image is
    /// pulled (use `image_pull::ensure_image_pulled` first).
    pub image: String,
    /// Container name. Should be unique per run; engines typically use
    /// `format!("temps-backup-{engine}-{uuid}", …)`.
    pub name: String,
    /// Engine key (`"control_plane"`, `"postgres_pgdump"`, …) — used
    /// only to stamp the `sh.temps.engine` label.
    pub engine: &'static str,
    /// `backups.id` this container is fulfilling. Stamped as
    /// `sh.temps.backup_id` for `docker ps` filtering and the boot-time
    /// orphan reaper.
    pub backup_id: i32,
    /// Entrypoint override. Pass `vec!["sh", "-c"]` for a shell-form
    /// command, or `vec!["wal-g"]` to call a binary directly.
    pub entrypoint: Vec<String>,
    /// Argv. For `entrypoint=["sh","-c"]` this is a single-element
    /// vector containing the shell string.
    pub cmd: Vec<String>,
    /// `KEY=VALUE` env entries. Don't log these — they often carry
    /// credentials.
    pub env: Vec<String>,
    /// Bind mounts in `"/host:/container[:opts]"` form. Used to surface
    /// dump files from the container to a host directory the engine
    /// then uploads from.
    pub binds: Vec<String>,
    /// `Some("host")` for host networking (control-plane case, talks to
    /// 127.0.0.1:5432), `Some("temps-app-network")` for the user-defined
    /// bridge (external services), `None` for default bridge.
    pub network_mode: Option<String>,
    /// Run-as-user. `Some("root")` is typical for sidecars that write
    /// to a host-owned bind mount.
    pub user: Option<String>,
}

/// Outcome of [`run_one_shot`].
#[derive(Debug)]
pub struct OneShotResult {
    /// Exit code reported by the container. `0` means the command
    /// succeeded. Anything non-zero means failure.
    pub exit_code: i64,
    /// Up to 4 KiB of the most recent stderr the container produced.
    /// Empty when the command wrote nothing to stderr.
    pub stderr_tail: String,
    /// Up to 4 KiB of the most recent stdout. Most backup commands
    /// write the dump to a bind mount, but some (e.g. `mongodump
    /// --archive=-`) write to stdout — those engines should set
    /// `binds=[]` and use this field instead.
    pub stdout_tail: String,
}

/// Failure mode that prevented the container from reaching an exit
/// code. Distinct from "container ran and returned non-zero", which is
/// a successful invocation of `run_one_shot` (the caller looks at
/// `exit_code`).
#[derive(Debug, thiserror::Error)]
pub enum OneShotError {
    #[error("Cancelled before container could finish")]
    Cancelled,

    #[error("Docker daemon refused to create container '{name}': {source}")]
    CreateFailed {
        name: String,
        #[source]
        source: bollard::errors::Error,
    },

    #[error("Docker daemon refused to start container '{name}': {source}")]
    StartFailed {
        name: String,
        #[source]
        source: bollard::errors::Error,
    },

    #[error("Docker `wait` failed for '{name}': {source}")]
    WaitFailed {
        name: String,
        #[source]
        source: bollard::errors::Error,
    },

    #[error("Container '{name}' produced no exit code")]
    NoExitCode { name: String },
}

/// Run a one-shot container start-to-finish. Returns when the container
/// exits, the cancel token fires, or the daemon returns an error.
///
/// On cancel, the container is `docker stop`ped (SIGTERM + 10s grace +
/// SIGKILL); the helper then returns `Err(Cancelled)`.
///
/// On exit (zero or non-zero), the helper returns `Ok(OneShotResult)`
/// with the exit code and captured log tails. Callers decide whether
/// non-zero is a failure (always, in practice).
///
/// The container is created with `auto_remove=true` so Docker reaps it
/// after exit. On error paths inside this function, we still issue an
/// explicit `remove_container --force` so a created-but-never-started
/// container doesn't linger.
pub async fn run_one_shot(
    docker: &Docker,
    spec: OneShotSpec,
    cancel: &CancellationToken,
) -> Result<OneShotResult, OneShotError> {
    let mut labels: HashMap<String, String> = HashMap::new();
    labels.insert("sh.temps.kind".to_string(), "backup".to_string());
    labels.insert("sh.temps.engine".to_string(), spec.engine.to_string());
    labels.insert("sh.temps.backup_id".to_string(), spec.backup_id.to_string());
    labels.insert(
        "sh.temps.born".to_string(),
        chrono::Utc::now().timestamp().to_string(),
    );

    let host_config = bollard::models::HostConfig {
        auto_remove: Some(true),
        oom_score_adj: Some(-500),
        network_mode: spec.network_mode.clone(),
        binds: if spec.binds.is_empty() {
            None
        } else {
            Some(spec.binds.clone())
        },
        ..Default::default()
    };

    let create_body = bollard::models::ContainerCreateBody {
        image: Some(spec.image.clone()),
        entrypoint: Some(spec.entrypoint.clone()),
        cmd: Some(spec.cmd.clone()),
        env: if spec.env.is_empty() {
            None
        } else {
            Some(spec.env.clone())
        },
        user: spec.user.clone(),
        labels: Some(labels),
        host_config: Some(host_config),
        // We want stdout/stderr so we can capture the log tail.
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        tty: Some(false),
        ..Default::default()
    };

    // ── Create + start ───────────────────────────────────────────────────
    docker
        .create_container(
            Some(
                CreateContainerOptionsBuilder::new()
                    .name(&spec.name)
                    .build(),
            ),
            create_body,
        )
        .await
        .map_err(|source| OneShotError::CreateFailed {
            name: spec.name.clone(),
            source,
        })?;

    info!(
        backup_id = spec.backup_id,
        engine = spec.engine,
        container = %spec.name,
        image = %spec.image,
        "one_shot: container created"
    );

    // Begin attaching to logs BEFORE starting so we don't miss early output.
    let attach = docker
        .attach_container(
            &spec.name,
            Some(
                bollard::query_parameters::AttachContainerOptionsBuilder::new()
                    .stream(true)
                    .stdout(true)
                    .stderr(true)
                    .build(),
            ),
        )
        .await;

    if let Err(e) = docker
        .start_container(
            &spec.name,
            None::<bollard::query_parameters::StartContainerOptions>,
        )
        .await
    {
        // Created but couldn't start — explicitly remove so we don't
        // depend on `auto_remove` (which only fires after start).
        let _ = docker
            .remove_container(
                &spec.name,
                Some(RemoveContainerOptionsBuilder::new().force(true).build()),
            )
            .await;
        return Err(OneShotError::StartFailed {
            name: spec.name.clone(),
            source: e,
        });
    }

    let mut stdout_tail = RingBuffer::with_capacity(4 * 1024);
    let mut stderr_tail = RingBuffer::with_capacity(4 * 1024);

    // ── Log collector (background task) ──────────────────────────────────
    //
    // We must drain the attach stream concurrently with `wait_container`,
    // otherwise the container can block on a full pipe buffer. The
    // collector exits naturally when the container does (stream ends).
    let log_handle = match attach {
        Ok(attach_results) => {
            let stream = attach_results.output;
            Some(tokio::spawn(async move {
                let mut stream = stream;
                let mut stdout = RingBuffer::with_capacity(4 * 1024);
                let mut stderr = RingBuffer::with_capacity(4 * 1024);
                while let Some(chunk) = stream.next().await {
                    match chunk {
                        Ok(LogOutput::StdOut { message }) => stdout.append(&message),
                        Ok(LogOutput::StdErr { message }) => stderr.append(&message),
                        Ok(_) => {}
                        Err(e) => {
                            debug!(error = %e, "one_shot: log stream error (non-fatal)");
                            break;
                        }
                    }
                }
                (stdout, stderr)
            }))
        }
        Err(e) => {
            warn!(error = %e, "one_shot: attach failed; will run without log capture");
            None
        }
    };

    // ── Wait for exit, racing the cancel token ───────────────────────────
    let mut wait_stream =
        docker.wait_container(&spec.name, Some(WaitContainerOptionsBuilder::new().build()));

    let exit_code = tokio::select! {
        _ = cancel.cancelled() => {
            warn!(
                backup_id = spec.backup_id,
                engine = spec.engine,
                container = %spec.name,
                "one_shot: cancellation received, stopping container",
            );
            // SIGTERM + 10s grace, then SIGKILL via the daemon.
            let _ = docker
                .stop_container(
                    &spec.name,
                    Some(StopContainerOptionsBuilder::new().t(10).build()),
                )
                .await;
            // `auto_remove` will reap on exit. Don't wait for it — the
            // caller's cancel path may have a tight deadline.
            if let Some(h) = log_handle {
                h.abort();
            }
            return Err(OneShotError::Cancelled);
        }
        result = wait_stream.next() => {
            match result {
                Some(Ok(resp)) => resp.status_code,
                Some(Err(e)) => {
                    if let Some(h) = log_handle {
                        h.abort();
                    }
                    return Err(OneShotError::WaitFailed { name: spec.name.clone(), source: e });
                }
                None => {
                    if let Some(h) = log_handle {
                        h.abort();
                    }
                    return Err(OneShotError::NoExitCode { name: spec.name.clone() });
                }
            }
        }
    };

    // Give the log collector up to 2 seconds to drain anything still in
    // the pipe before we hand back to the caller.
    if let Some(handle) = log_handle {
        match tokio::time::timeout(Duration::from_secs(2), handle).await {
            Ok(Ok((s_out, s_err))) => {
                stdout_tail = s_out;
                stderr_tail = s_err;
            }
            Ok(Err(_)) => {
                // Join error (task panicked or was aborted). Captured
                // tails stay empty.
            }
            Err(_) => {
                // Timeout draining; not worth blocking the caller.
                debug!("one_shot: log drain timed out after 2s");
            }
        }
    }

    info!(
        backup_id = spec.backup_id,
        engine = spec.engine,
        container = %spec.name,
        exit_code,
        "one_shot: container exited",
    );

    Ok(OneShotResult {
        exit_code,
        stdout_tail: stdout_tail.into_string_lossy(),
        stderr_tail: stderr_tail.into_string_lossy(),
    })
}

/// Suppress unused-import warning when the helper is compiled but only
/// the `StartExecResults` re-export is needed elsewhere. Tests in the
/// engines crate consume `attach_container` directly which forces the
/// linker to keep these.
#[allow(dead_code)]
fn _force_link_referenced_types() {
    let _ = std::mem::size_of::<StartExecResults>();
}
