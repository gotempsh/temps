//! Resilient `docker exec` helpers for backup/restore engines.
//!
//! Why this exists: backup engines (WAL-G, pg_dump sidecar, mongodump,
//! rustfs migrate, etc.) all run as `docker exec` invocations. We hit three
//! independent bugs that all caused backup rows to stick on `state="running"`
//! forever:
//!
//! 1. `start_exec` returns a stream of stdout/stderr. If we don't drain it,
//!    the exec stalls on stdout backpressure and `inspect_exec.exit_code`
//!    never advances. The fix is "always drain, even if you don't care about
//!    the bytes."
//!
//! 2. `inspect_exec.running` can transiently return `None` (Docker drops the
//!    record from its cache for long-running execs). The naive loop
//!    `if let Some(running) = inspect.running { if !running { break; } }`
//!    spins forever in that case. The fix is to treat repeated `None`s as a
//!    finished exec and trust the captured stream + exit code.
//!
//! 3. `detach: true` was passed to keep the exec running after the HTTP
//!    request completed. That throws away stdout/stderr, so when WAL-G
//!    fails we can't tell why. Always attach.
//!
//! All shared backup execs go through `run_exec` so a single fix benefits
//! every engine.

use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::Docker;
use futures::StreamExt;

/// Result of a successful exec invocation.
#[derive(Debug, Clone)]
pub struct ExecResult {
    pub exit_code: i64,
    /// Combined stdout+stderr (mux'd by Docker — we don't try to separate).
    pub output: String,
}

/// Run `cmd` inside `container` via `docker exec`, capturing combined
/// stdout+stderr, and return the exit code + output.
///
/// `timeout` bounds the total exec duration. On timeout we return an error
/// containing whatever output we've captured so far. The caller is
/// responsible for marking any external state (DB rows, etc.) as failed.
///
/// Behavior contract:
/// - Always attaches stdout+stderr (no `detach: true`).
/// - Drains the output stream concurrently with polling, so backpressure
///   can't stall the exec.
/// - Returns `Err` if the exec returns a non-zero exit code, with the
///   output included in the error message.
/// - If `inspect_exec.running` is `None` for ≥ 3 consecutive polls AND
///   the output stream is drained, we assume the exec finished and trust
///   `exit_code` (or "no exit code" if Docker didn't record one).
pub async fn run_exec(
    docker: &Docker,
    container: &str,
    cmd: Vec<String>,
    env: Option<Vec<String>>,
    timeout: Duration,
) -> Result<ExecResult> {
    let exec = docker
        .create_exec(
            container,
            CreateExecOptions {
                cmd: Some(cmd.iter().map(|s| s.as_str()).collect()),
                env: env.as_ref().map(|e| e.iter().map(|s| s.as_str()).collect()),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| {
            anyhow!(
                "docker create_exec failed in container {}: {}",
                container,
                e
            )
        })?;

    let stream = docker
        .start_exec(&exec.id, None)
        .await
        .map_err(|e| anyhow!("docker start_exec failed in container {}: {}", container, e))?;

    // Drain output concurrently with polling. We collect into a String;
    // backups don't produce huge logs (kilobytes at most for WAL-G, larger
    // for pg_dump but still bounded by gzip's stderr).
    let mut captured = String::new();
    let mut output_done = false;

    if let StartExecResults::Attached { mut output, .. } = stream {
        let deadline = Instant::now() + timeout;
        let mut consecutive_none = 0u8;

        loop {
            // Drain whatever's ready, but don't block forever — alternate
            // with polling inspect_exec.
            tokio::select! {
                biased;

                chunk = output.next() => {
                    match chunk {
                        Some(Ok(msg)) => {
                            captured.push_str(&msg.to_string());
                        }
                        Some(Err(e)) => {
                            // Stream errors are usually benign (container
                            // closed the pipe at exec end). Note and move on.
                            tracing::debug!(
                                "exec output stream error in container {}: {}",
                                container,
                                e
                            );
                            output_done = true;
                        }
                        None => {
                            output_done = true;
                        }
                    }
                }

                _ = tokio::time::sleep(Duration::from_millis(500)) => {
                    if Instant::now() > deadline {
                        return Err(anyhow!(
                            "docker exec timed out after {:?} in container {}. \
                             cmd: {:?}. output captured so far ({} bytes):\n{}",
                            timeout,
                            container,
                            cmd.iter().take(3).collect::<Vec<_>>(),
                            captured.len(),
                            tail(&captured, 4096),
                        ));
                    }

                    match docker.inspect_exec(&exec.id).await {
                        Ok(info) => {
                            match info.running {
                                Some(false) => {
                                    // Drain remaining buffered chunks.
                                    while let Some(Ok(msg)) = output.next().await {
                                        captured.push_str(&msg.to_string());
                                    }
                                    let exit_code = info.exit_code.unwrap_or(-1);
                                    if exit_code == 0 {
                                        return Ok(ExecResult {
                                            exit_code,
                                            output: captured,
                                        });
                                    }
                                    return Err(anyhow!(
                                        "docker exec exited with code {} in container {}. \
                                         cmd: {:?}. output ({} bytes):\n{}",
                                        exit_code,
                                        container,
                                        cmd.iter().take(3).collect::<Vec<_>>(),
                                        captured.len(),
                                        tail(&captured, 4096),
                                    ));
                                }
                                Some(true) => {
                                    consecutive_none = 0;
                                }
                                None => {
                                    consecutive_none = consecutive_none.saturating_add(1);
                                    if consecutive_none >= 3 && output_done {
                                        // Docker dropped the exec record but
                                        // our stream is drained — trust the
                                        // captured output. Fail closed since
                                        // we have no exit code.
                                        return Err(anyhow!(
                                            "docker exec finished but Docker reports no \
                                             running state and no exit code in container {}. \
                                             cmd: {:?}. output ({} bytes):\n{}",
                                            container,
                                            cmd.iter().take(3).collect::<Vec<_>>(),
                                            captured.len(),
                                            tail(&captured, 4096),
                                        ));
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            return Err(anyhow!(
                                "docker inspect_exec failed for {} in container {}: {}. \
                                 output captured ({} bytes):\n{}",
                                exec.id,
                                container,
                                e,
                                captured.len(),
                                tail(&captured, 4096),
                            ));
                        }
                    }
                }
            }
        }
    }

    // Detached or non-attached result — shouldn't happen because we asked
    // for attach_stdout/attach_stderr. Fail closed.
    Err(anyhow!(
        "docker start_exec returned a detached result for container {} (this is a bug)",
        container,
    ))
}

/// Trim a long string to its trailing N characters, with an indicator if
/// truncated. Used to keep error messages from blowing up logs.
fn tail(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let start = s.len() - n;
    // Snap to a char boundary.
    let start = (start..s.len())
        .find(|i| s.is_char_boundary(*i))
        .unwrap_or(start);
    format!("...[{} earlier bytes elided]...\n{}", start, &s[start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_short_returns_input() {
        assert_eq!(tail("hello", 100), "hello");
    }

    #[test]
    fn tail_long_truncates_with_indicator() {
        let big = "x".repeat(10_000);
        let result = tail(&big, 100);
        assert!(result.contains("earlier bytes elided"));
        assert!(result.ends_with(&"x".repeat(100)));
    }
}
