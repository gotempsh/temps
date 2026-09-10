// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Command execution for standalone sandboxes.
//!
//! Two flavors:
//! - [`SandboxService::exec`] — foreground, blocks until the command exits,
//!   returns stdout + exit code to the caller. Mirrors `@vercel/sandbox`
//!   `exec()`.
//! - [`SandboxService::exec_detached`] — fires the command into a background
//!   `tokio::task`, streams stdout/stderr into a [`JobTracker`] state, and
//!   returns immediately with a job ID. Mirrors `execDetached()`.
//!
//! Both routes `touch` the sandbox row on entry so the expiry sweeper
//! doesn't reap a sandbox that someone is actively using.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_agents::sandbox::{ExecStream, KillSignal, OnStreamEventCallback, SandboxExecResult};

use crate::error::SandboxError;
use crate::services::job_tracker::{Job, JobLogEvent, JobState, JobStatus, JobTracker};
use crate::services::sandbox_service::SandboxService;

/// Options accepted by both `exec` and `exec_detached`.
#[derive(Debug, Clone, Default)]
pub struct ExecOptions {
    /// Command + arguments. Must be non-empty.
    pub cmd: Vec<String>,
    /// Extra environment variables to merge into the sandbox environment.
    pub env: HashMap<String, String>,
    /// Working directory override. `None` → the sandbox's default work dir.
    /// Applied by the caller via `["sh", "-c", "cd <dir> && <cmd>"]` when
    /// the underlying provider lacks a cwd knob.
    pub cwd: Option<String>,
}

impl ExecOptions {
    fn validate(&self) -> Result<(), SandboxError> {
        if self.cmd.is_empty() {
            return Err(SandboxError::Validation {
                message: "exec cmd must contain at least one argument".into(),
            });
        }
        Ok(())
    }

    /// Materialize the final command line, applying `cwd` if provided.
    fn effective_cmd(&self) -> Vec<String> {
        match &self.cwd {
            Some(dir) if !dir.is_empty() => {
                // `sh -c 'cd <dir> && <shell-escaped-cmd>'`
                let joined = self
                    .cmd
                    .iter()
                    .map(|a| shell_escape(a))
                    .collect::<Vec<_>>()
                    .join(" ");
                vec![
                    "sh".into(),
                    "-c".into(),
                    format!("cd {} && {}", shell_escape(dir), joined),
                ]
            }
            _ => self.cmd.clone(),
        }
    }
}

/// Result of a synchronous `exec`. Matches the `@vercel/sandbox` shape:
/// a non-zero exit code is NOT an error — callers inspect `exit_code`.
#[derive(Debug, Clone)]
pub struct ExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl From<SandboxExecResult> for ExecResult {
    fn from(r: SandboxExecResult) -> Self {
        Self {
            exit_code: r.exit_code,
            stdout: r.stdout,
            stderr: r.stderr,
        }
    }
}

impl SandboxService {
    /// Run a command inside the sandbox and block until it exits.
    pub async fn exec(
        &self,
        public_id: &str,
        user_id: i32,
        options: ExecOptions,
    ) -> Result<ExecResult, SandboxError> {
        options.validate()?;
        let (row, internal_id) = self.resolve_id(public_id, user_id).await?;
        self.touch(internal_id, row.timeout_secs).await;
        let handle = self
            .registry()
            .get(internal_id, public_id)
            .await
            .map_err(|e| Self::provider_err(public_id, e))?;
        let result = self
            .registry()
            .provider()
            .exec(&handle, options.effective_cmd(), options.env, None)
            .await
            .map_err(|e| Self::provider_err(public_id, e))?;
        Ok(ExecResult::from(result))
    }

    /// Spawn a command in the background and return a job ID. The caller
    /// polls `job_status` / aborts via `destroy_sandbox`.
    ///
    /// Output handling: we accumulate stdout lines into the job's
    /// [`JobState`]. The provider's `exec` callback is line-based, so the
    /// streaming granularity matches what the agent executor already uses.
    pub async fn exec_detached(
        &self,
        public_id: &str,
        user_id: i32,
        options: ExecOptions,
    ) -> Result<String, SandboxError> {
        options.validate()?;
        let (row, internal_id) = self.resolve_id(public_id, user_id).await?;
        self.touch(internal_id, row.timeout_secs).await;

        // Verify the sandbox is alive *before* spawning the background task,
        // so callers get a synchronous NotFound/ExecFailed instead of a
        // silently-dead job.
        let handle = self
            .registry()
            .get(internal_id, public_id)
            .await
            .map_err(|e| Self::provider_err(public_id, e))?;

        let state = Arc::new(tokio::sync::Mutex::new(JobState::default()));
        let state_for_task = state.clone();
        let state_for_callback = state.clone();

        let log_tx = JobTracker::new_log_channel();
        let log_tx_for_callback = log_tx.clone();

        // Stream-tagged callback: record each line in the per-stream buffer
        // AND publish it to the broadcast channel so SSE subscribers see it
        // live. `send` ignores the error when no receivers are attached —
        // that's the common case; it only becomes Err when the channel
        // capacity overflows or the channel is closed.
        let on_event: OnStreamEventCallback = Arc::new(move |stream: ExecStream, line: String| {
            let state = state_for_callback.clone();
            let tx = log_tx_for_callback.clone();
            let fut: Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(async move {
                {
                    let mut s = state.lock().await;
                    let buf = match stream {
                        ExecStream::Stdout => &mut s.stdout,
                        ExecStream::Stderr => &mut s.stderr,
                    };
                    buf.push_str(&line);
                    if !line.ends_with('\n') {
                        buf.push('\n');
                    }
                }
                let _ = tx.send(JobLogEvent { stream, line });
            });
            fut
        });

        let provider = self.registry().provider_arc();
        let effective_cmd = options.effective_cmd();
        // Derive a `pkill -f` anchor from the user-supplied command (not
        // the shell-wrapped form). Using the raw cmd[0] keeps the pattern
        // stable whether or not we prepended `sh -c 'cd ... && '`. `pkill
        // -f` matches the full command line, so this catches the child.
        let kill_pattern = options.cmd.first().cloned().unwrap_or_default();
        let env = options.env.clone();
        let public_id_for_log = public_id.to_string();
        let task = tokio::spawn(async move {
            let result = provider
                .exec_streamed(&handle, effective_cmd, env, Some(on_event))
                .await;
            let mut s = state_for_task.lock().await;
            match result {
                Ok(r) => {
                    s.status = JobStatus::Exited {
                        exit_code: r.exit_code,
                    };
                    // Provider accumulates stream output in the result as
                    // well — prefer that as the canonical record if the
                    // streaming callback produced an empty buffer (e.g.
                    // provider didn't call it).
                    if s.stdout.is_empty() && !r.stdout.is_empty() {
                        s.stdout = r.stdout;
                    }
                    if s.stderr.is_empty() && !r.stderr.is_empty() {
                        s.stderr = r.stderr;
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "exec_detached failed in sandbox {}: {}",
                        public_id_for_log,
                        e
                    );
                    s.status = JobStatus::Failed {
                        reason: e.to_string(),
                    };
                }
            }
        });

        let job = Job {
            id: JobTracker::new_job_id(),
            state,
            task,
            log_tx,
            kill_pattern,
            cmd_display: options.cmd.join(" "),
            started_at: chrono::Utc::now(),
        };
        let id = self.jobs().insert(internal_id, job).await;
        Ok(id)
    }

    /// Terminate a detached job. Aborts the tokio task AND sends
    /// SIGTERM/SIGKILL to matching processes inside the sandbox
    /// container — abort() alone would leave the child orphaned.
    ///
    /// When `force` is true, skips SIGTERM and goes straight to
    /// SIGKILL. Otherwise uses SIGTERM (so the process can flush).
    pub async fn kill_job(
        &self,
        public_id: &str,
        user_id: i32,
        job_id: &str,
        force: bool,
    ) -> Result<(), SandboxError> {
        let (_row, internal_id) = self.resolve_id(public_id, user_id).await?;
        let pattern = self
            .jobs()
            .kill_and_remove(public_id, internal_id, job_id)
            .await?;
        if pattern.is_empty() {
            return Ok(());
        }
        let handle = match self.registry().get(internal_id, public_id).await {
            Ok(h) => h,
            Err(_) => {
                // Sandbox is gone — the task is already aborted, there's
                // nothing left to kill. Not an error.
                return Ok(());
            }
        };
        let signal = if force {
            KillSignal::Kill
        } else {
            KillSignal::Term
        };
        if let Err(e) = self
            .registry()
            .provider()
            .kill_processes(&handle, &pattern, signal)
            .await
        {
            // pkill failure is best-effort per the trait contract —
            // surface as a warning, not a hard error. The task is
            // already aborted on our side.
            tracing::warn!(
                "kill_processes(pattern={}) failed in sandbox {}: {}",
                pattern,
                public_id,
                e
            );
        }
        Ok(())
    }

    /// Subscribe to the live log stream of a detached job. Consumed by
    /// the SSE handler. Returns `JobNotFound` when the job is unknown
    /// — historical lines live in `job_status`, subscribers only see
    /// events produced after subscription.
    pub async fn subscribe_job_logs(
        &self,
        public_id: &str,
        user_id: i32,
        job_id: &str,
    ) -> Result<tokio::sync::broadcast::Receiver<JobLogEvent>, SandboxError> {
        // Ownership only, no wake: job state lives in the in-process tracker,
        // and a suspended container's jobs are already gone.
        let (_row, internal_id) = self.resolve_id_no_wake(public_id, user_id).await?;
        self.jobs().subscribe(public_id, internal_id, job_id).await
    }

    /// Snapshot a background job's state.
    pub async fn job_status(
        &self,
        public_id: &str,
        user_id: i32,
        job_id: &str,
    ) -> Result<JobState, SandboxError> {
        let (_row, internal_id) = self.resolve_id_no_wake(public_id, user_id).await?;
        self.jobs().status(public_id, internal_id, job_id).await
    }

    /// List detached jobs registered for a sandbox. Returns an empty vec
    /// when the sandbox has never run one. Does not include stdout/stderr
    /// bodies — fetch those via `job_status` per-id.
    pub async fn list_jobs(
        &self,
        public_id: &str,
        user_id: i32,
    ) -> Result<Vec<crate::services::job_tracker::JobSummary>, SandboxError> {
        let (_row, internal_id) = self.resolve_id_no_wake(public_id, user_id).await?;
        Ok(self.jobs().list(internal_id).await)
    }
}

/// POSIX-style single-quoted escape. Safe against all shell metacharacters
/// including embedded single quotes. Used to splice `cwd` / `cmd` tokens
/// into the `sh -c` wrapper.
fn shell_escape(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@".contains(c))
    {
        // Common case — no escaping needed for plain tokens.
        s.to_string()
    } else {
        let escaped = s.replace('\'', "'\\''");
        format!("'{}'", escaped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_rejects_empty_cmd() {
        let opts = ExecOptions::default();
        let err = opts.validate().unwrap_err();
        assert!(matches!(err, SandboxError::Validation { .. }));
    }

    #[test]
    fn validate_accepts_single_cmd() {
        let opts = ExecOptions {
            cmd: vec!["echo".into()],
            ..Default::default()
        };
        opts.validate().unwrap();
    }

    #[test]
    fn effective_cmd_without_cwd_is_passthrough() {
        let opts = ExecOptions {
            cmd: vec!["echo".into(), "hi".into()],
            ..Default::default()
        };
        assert_eq!(opts.effective_cmd(), vec!["echo", "hi"]);
    }

    #[test]
    fn effective_cmd_with_cwd_wraps_in_sh() {
        let opts = ExecOptions {
            cmd: vec!["ls".into(), "-la".into()],
            cwd: Some("/tmp/data".into()),
            ..Default::default()
        };
        let c = opts.effective_cmd();
        assert_eq!(c[0], "sh");
        assert_eq!(c[1], "-c");
        assert_eq!(c[2], "cd /tmp/data && ls -la");
    }

    #[test]
    fn effective_cmd_shell_escapes_risky_cwd() {
        let opts = ExecOptions {
            cmd: vec!["ls".into()],
            cwd: Some("/tmp/with space".into()),
            ..Default::default()
        };
        let c = opts.effective_cmd();
        assert_eq!(c[2], "cd '/tmp/with space' && ls");
    }

    #[test]
    fn shell_escape_plain_is_unchanged() {
        assert_eq!(shell_escape("abc-123_x"), "abc-123_x");
    }

    #[test]
    fn shell_escape_quotes_spaces() {
        assert_eq!(shell_escape("a b"), "'a b'");
    }

    #[test]
    fn shell_escape_escapes_embedded_quote() {
        assert_eq!(shell_escape("a'b"), "'a'\\''b'");
    }

    #[test]
    fn exec_result_from_provider_preserves_exit_and_stdout() {
        let r = SandboxExecResult {
            exit_code: 42,
            stdout: "hello".into(),
            stderr: "oops".into(),
        };
        let er = ExecResult::from(r);
        assert_eq!(er.exit_code, 42);
        assert_eq!(er.stdout, "hello");
        assert_eq!(er.stderr, "oops");
    }
}
