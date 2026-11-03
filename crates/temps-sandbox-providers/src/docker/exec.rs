// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Running commands in a sandbox container: exec configs, the root and
//! user exec paths, bounded output capture and ownership normalization.

use super::*;

pub(super) fn agent_runtime_exec_config() -> bollard::models::ExecConfig {
    bollard::models::ExecConfig {
        attach_stdin: Some(true),
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        tty: Some(false),
        user: Some(format!("{SANDBOX_UID}:{SANDBOX_GID}")),
        cmd: Some(vec![
            "/usr/local/bin/temps-sandbox-runtime".into(),
            "connect".into(),
        ]),
        ..Default::default()
    }
}

pub(super) fn runtime_health_exec_config() -> bollard::models::ExecConfig {
    bollard::models::ExecConfig {
        user: Some(format!("{SANDBOX_UID}:{SANDBOX_GID}")),
        cmd: Some(vec![
            "/usr/local/bin/temps-sandbox-runtime".into(),
            "request".into(),
        ]),
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        ..Default::default()
    }
}

#[derive(Deserialize)]
struct RuntimeHealthResponse {
    #[serde(rename = "type")]
    response_type: String,
    capabilities: Vec<String>,
}

pub(super) fn has_harness_recovery_capability(output: &str) -> bool {
    serde_json::from_str::<RuntimeHealthResponse>(output.trim()).is_ok_and(|health| {
        health.response_type == "health"
            && health
                .capabilities
                .iter()
                .any(|capability| capability == "recover_harness")
    })
}

/// PATH used for every command Temps runs as root inside a sandbox.
///
/// System directories only, and deliberately not the image's own PATH: the
/// sandbox user can write to `~/.local/bin` and `~/.bun/bin`, which the image
/// puts *ahead* of `/usr/bin`. Inheriting that would let sandbox-controlled
/// binaries run as container root during ownership normalisation or recovery.
pub(crate) const ROOT_EXEC_PATH: &str =
    "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// Whether a Docker exec `user` string runs the command as root, and therefore
/// must have [`ROOT_EXEC_PATH`] forced on it.
///
/// Docker accepts `user`, `uid`, `user:group` and `uid:gid`, so the check is on
/// the user half only — `0:0`, `0`, `root` and `root:root` are all root, while
/// `root-ish` names like `rootless` are not. Errs toward *not* claiming root
/// for an unrecognised value: PATH pinning is applied on top of a privilege the
/// caller already asked for, so a false negative leaves behaviour unchanged
/// while a false positive would silently rewrite a non-root exec's environment.
pub(super) fn exec_runs_as_root(user: Option<&str>) -> bool {
    matches!(
        user.map(|u| u.split(':').next().unwrap_or(u).trim()),
        Some("0") | Some("root")
    )
}

// Docker frames are arbitrary byte chunks, not lines or UTF-8 boundaries.
// Keep partial JSONL tool events intact until a newline (or final EOF).
/// One exec stream's output: split into lines for the caller's callback and
/// kept for the [`SandboxExecResult`].
///
/// With a `limit`, memory stays bounded however much the command prints: a
/// line longer than the limit is delivered in pieces instead of being held
/// until its newline, and already-delivered bytes beyond the limit are
/// dropped from the front, so the buffer peaks at about twice the limit. The
/// result then holds the last `limit` bytes with a marker saying how many
/// earlier bytes were dropped.
pub(super) struct ExecOutputBuffer {
    pub(super) bytes: Vec<u8>,
    /// Bytes of `bytes` already handed out as lines.
    pub(super) emitted: usize,
    limit: Option<usize>,
    /// Bytes dropped from the front of `bytes`.
    dropped: usize,
}

impl ExecOutputBuffer {
    pub(super) fn new(limit: Option<usize>) -> Self {
        Self {
            bytes: Vec::new(),
            emitted: 0,
            limit,
            dropped: 0,
        }
    }

    /// Append a chunk of output and return the lines it completed.
    pub(super) fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.bytes.extend_from_slice(chunk);
        let mut lines = completed_exec_lines(&self.bytes, &mut self.emitted, false);
        let Some(limit) = self.limit else {
            return lines;
        };
        if self.bytes.len() - self.emitted > limit {
            // An unterminated line past the limit: deliver what there is so
            // the pending part cannot grow without bound.
            lines.push(String::from_utf8_lossy(&self.bytes[self.emitted..]).into_owned());
            self.emitted = self.bytes.len();
        }
        // Compact only once the buffer is twice the limit, so dropping from
        // the front costs amortised O(1) per byte rather than a copy per chunk.
        if self.bytes.len() > limit.saturating_mul(2) {
            let mut cut = (self.bytes.len() - limit).min(self.emitted);
            // Never start the kept tail inside a UTF-8 sequence.
            while cut < self.emitted && (self.bytes[cut] & 0xC0) == 0x80 {
                cut += 1;
            }
            self.bytes.drain(..cut);
            self.emitted -= cut;
            self.dropped += cut;
        }
        lines
    }

    /// Lines still pending at the end of the stream.
    pub(super) fn finish(&mut self) -> Vec<String> {
        completed_exec_lines(&self.bytes, &mut self.emitted, true)
    }

    /// The kept output: at most `limit` bytes including the truncation
    /// marker, so a caller applying the same limit again changes nothing.
    pub(super) fn into_output(self) -> String {
        let text = String::from_utf8_lossy(&self.bytes).into_owned();
        let Some(limit) = self.limit else {
            return text;
        };
        if self.dropped == 0 && text.len() <= limit {
            return text;
        }
        let marker =
            |dropped: usize| format!("[{dropped} earlier bytes truncated by the worker node]\n");
        // Size the marker for the most bytes it could report, then keep as
        // much of the tail as fits beside it, starting on a char boundary.
        let room = limit.saturating_sub(marker(self.dropped + text.len()).len());
        let mut start = text.len().saturating_sub(room);
        while !text.is_char_boundary(start) {
            start += 1;
        }
        format!("{}{}", marker(self.dropped + start), &text[start..])
    }
}

pub(super) fn completed_exec_lines(bytes: &[u8], emitted: &mut usize, eof: bool) -> Vec<String> {
    let mut lines = Vec::new();
    while let Some(end) = bytes[*emitted..].iter().position(|byte| *byte == b'\n') {
        let end = *emitted + end;
        let line = bytes[*emitted..end]
            .strip_suffix(b"\r")
            .unwrap_or(&bytes[*emitted..end]);
        lines.push(String::from_utf8_lossy(line).into_owned());
        *emitted = end + 1;
    }
    if eof && *emitted < bytes.len() {
        lines.push(String::from_utf8_lossy(&bytes[*emitted..]).into_owned());
        *emitted = bytes.len();
    }
    lines
}

/// Whether an ownership fix may cross into filesystems mounted below its root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OwnershipScope {
    /// Stay on the root's own filesystem. The home volume has the workspace
    /// bind-mounted inside it, which gets its own pass; without this a
    /// home pass walks every workspace file a second time.
    OwnFilesystem,
    /// Follow the tree wherever it is mounted from.
    AcrossMounts,
}

/// Root-run command giving the sandbox user everything under `root` that it
/// does not already own.
///
/// It changes only what is wrong instead of `chown -R` on every file, so a
/// recovered sandbox whose files are already right costs a directory walk
/// rather than a write per file (a JavaScript project's dependencies alone
/// run to tens of thousands of files). `-h` changes a symlink itself, never
/// what it points at: a link in the workspace must not let root hand an
/// arbitrary container file to the sandbox user.
pub(super) fn ownership_fix_command(root: &str, scope: OwnershipScope) -> Vec<String> {
    let mut cmd = vec!["find".to_string(), root.to_string()];
    if scope == OwnershipScope::OwnFilesystem {
        cmd.push("-xdev".to_string());
    }
    cmd.extend(
        [
            "(",
            "!",
            "-uid",
            &SANDBOX_UID.to_string(),
            "-o",
            "!",
            "-gid",
            &SANDBOX_GID.to_string(),
            ")",
            "-exec",
            "chown",
            "-h",
            SANDBOX_CHOWN,
            "{}",
            "+",
        ]
        .iter()
        .map(|arg| arg.to_string()),
    );
    cmd
}

/// Shell script, run as the sandbox user, that proves it can create a file
/// in `dir`. Each run uses its own file name, so two checks of the same
/// sandbox at once (two requests recovering it together) cannot delete each
/// other's file and report a writable workspace as broken.
pub(super) fn write_probe_script(dir: &str) -> String {
    format!(
        "probe=$(mktemp '{}/.temps-write-probe.XXXXXX') && rm -f \"$probe\"",
        dir
    )
}

impl DockerSandboxProvider {
    /// Run `cmd` in the sandbox as `SANDBOX_USER` with its stdio attached,
    /// as a plain binary pipe (no TTY: Docker would mangle the bytes with
    /// ONLCR). Anything on the exec's stderr is the relay failing, and ends
    /// the stream as an error rather than being mixed into the bytes.
    ///
    /// Nothing long-lived runs inside this exec: `socat` exits when its stdin
    /// closes, and the exec dies with it.
    pub(super) async fn exec_byte_pipe(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        operation: &'static str,
        relay: &'static str,
    ) -> Result<PtyAttachment, AgentError> {
        let exec_failed = |reason: String| AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: handle.sandbox_id.clone(),
            reason,
        };

        let exec_config = bollard::models::ExecConfig {
            attach_stdin: Some(true),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            tty: Some(false),
            cmd: Some(cmd),
            user: Some(SANDBOX_USER.to_string()),
            ..Default::default()
        };

        let exec = self
            .docker
            .create_exec(&handle.sandbox_id, exec_config)
            .await
            .map_err(|e| exec_failed(format!("failed to create {} exec: {}", operation, e)))?;

        let started = self
            .docker
            .start_exec(
                &exec.id,
                Some(bollard::exec::StartExecOptions {
                    detach: false,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| exec_failed(format!("failed to start {} exec: {}", operation, e)))?;

        match started {
            StartExecResults::Attached { output, input } => {
                let sandbox_id = handle.sandbox_id.clone();
                let output = output.map(move |chunk| match chunk {
                    Ok(LogOutput::StdOut { message }) => Ok(message),
                    Ok(LogOutput::Console { message }) => Ok(message),
                    Ok(LogOutput::StdErr { message }) => Err(AgentError::SandboxExecFailed {
                        run_id: 0,
                        sandbox_id: sandbox_id.clone(),
                        reason: format!(
                            "{} error: {}",
                            relay,
                            String::from_utf8_lossy(&message).trim()
                        ),
                    }),
                    Ok(LogOutput::StdIn { .. }) => Ok(bytes::Bytes::new()),
                    Err(e) => Err(AgentError::SandboxExecFailed {
                        run_id: 0,
                        sandbox_id: sandbox_id.clone(),
                        reason: format!("{} stream failed: {}", operation, e),
                    }),
                });
                Ok(PtyAttachment {
                    output: Box::pin(output),
                    input,
                })
            }
            StartExecResults::Detached => Err(exec_failed(format!(
                "{} exec returned detached; expected an attached stream",
                operation
            ))),
        }
    }

    /// Run a command as root inside a freshly-started sandbox container,
    /// drain its output, and return its exit code. Used for the post-start
    /// fix-up steps (chown of home + work dir, AI-CLI restore) where we
    /// need to know whether the command actually succeeded — earlier
    /// versions used `let _ = start_exec(...)` and silently masked
    /// failures, leaving sandboxes with root-owned workspaces that broke
    /// `git pull` ("dubious ownership") and writes from the temps user.
    pub(super) async fn run_root_exec(
        &self,
        container_id: &str,
        run_id: i32,
        label: &str,
        cmd: Vec<String>,
    ) -> Result<i64, AgentError> {
        self.run_exec_config(
            container_id,
            run_id,
            label,
            bollard::models::ExecConfig {
                user: Some("0:0".to_string()),
                cmd: Some(cmd),
                // Root maintenance commands (`chown`, `su`, `cp`, `curl`,
                // `sh`) are resolved through PATH. The sandbox image puts
                // user-writable directories ahead of system ones.
                env: Some(vec![format!("PATH={ROOT_EXEC_PATH}")]),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                ..Default::default()
            },
        )
        .await
    }

    pub(super) async fn run_exec_config(
        &self,
        container_id: &str,
        run_id: i32,
        label: &str,
        config: bollard::models::ExecConfig,
    ) -> Result<i64, AgentError> {
        self.run_exec_config_captured(container_id, run_id, label, config)
            .await
            .map(|(code, _)| code)
    }

    pub(super) async fn run_exec_config_captured(
        &self,
        container_id: &str,
        run_id: i32,
        label: &str,
        config: bollard::models::ExecConfig,
    ) -> Result<(i64, String), AgentError> {
        // Drain the output stream so the exec actually runs to completion;
        // bollard tears the exec down when the stream is dropped, which
        // can race with the underlying command on slow hosts. A faulty image
        // can print indefinitely, so only a small diagnostic part is retained.
        let output = temps_docker_util::exec_capture(
            &self.docker,
            container_id,
            config,
            temps_docker_util::ExecCaptureOptions::separate(
                temps_docker_util::OutputLimit::Head(4096),
                temps_docker_util::OutputLimit::Head(4096),
            )
            .stream_errors(temps_docker_util::StreamErrorPolicy::Ignore)
            .exit_code(temps_docker_util::ExitCodePolicy::BestEffort),
        )
        .await
        .map_err(|error| {
            let stage = match &error {
                temps_docker_util::DockerUtilError::ExecCreate { .. } => "create",
                _ => "start",
            };
            AgentError::SandboxCreationFailed {
                run_id,
                provider: "docker".to_string(),
                reason: format!("Failed to {} {} exec: {}", stage, label, error.cause_text()),
            }
        })?;
        let exit_code = output.exit_code.unwrap_or(-1);
        let stderr_tail = output.stderr;
        let stdout = output.stdout;

        if exit_code != 0 {
            let trimmed = stderr_tail.trim();
            tracing::error!(
                "Sandbox {} step '{}' exited {} (stderr: {})",
                container_id,
                label,
                exit_code,
                if trimmed.is_empty() {
                    "<empty>"
                } else {
                    trimmed
                }
            );
        }

        Ok((exit_code, stdout))
    }

    /// Run the per-container ownership-normalization steps: chown -R
    /// /home/temps and /home/temps/workspace to temps:temps.
    ///
    /// Idempotent and safe to call repeatedly. We invoke it on every create
    /// AND on every recover — recovered containers might have been left
    /// root-owned by an earlier server that crashed before chown ran, or by
    /// host-side git operations performed after the first chown.
    ///
    /// chown-work is treated as **fatal**: without it the sandbox user can
    /// never write under /home/temps/workspace, so the entire run will fail
    /// with confusing "Permission denied" errors. Better to abort startup
    /// with a clear message. chown-home is best-effort: it operates on the
    /// home volume which may legitimately contain files we don't need to
    /// touch (e.g. socket files, FIFOs) and whose individual failures don't
    /// break workflows.
    pub(super) async fn normalize_ownership(
        &self,
        container_id: &str,
        run_id: i32,
    ) -> Result<(), AgentError> {
        // Sandbox home: best-effort. Some files in the named volume (sockets,
        // FIFOs, files owned by uids the kernel won't let us touch even with
        // CAP_CHOWN) can make a recursive chown exit non-zero while still
        // doing useful work. Don't fail the whole sandbox over that.
        let home_exit = self
            .run_root_exec(
                container_id,
                run_id,
                "chown-home",
                ownership_fix_command(SANDBOX_HOME, OwnershipScope::OwnFilesystem),
            )
            .await?;
        if home_exit != 0 {
            tracing::warn!(
                "Sandbox {} chown-home returned {} — best-effort, continuing. \
                 Investigate if subsequent home-dir writes fail.",
                container_id,
                home_exit
            );
        }

        // Work dir: bind-mounted from the host where the temps server
        // (often root) ran `git clone`, so files arrive owned by uid 0.
        // Without this chown the sandbox user can't `mkdir reports/`,
        // can't `git commit`, can't open lockfiles. This is the exact
        // root cause of "mkdir: cannot create directory '/home/temps/
        // workspace/reports': Permission denied".
        //
        // We can't hard-fail on non-zero exit: some bind-mount backends
        // (macOS Docker Desktop's virtiofs, userns-remap on Linux) return
        // EPERM for chown even when the operation is logically a no-op.
        // Instead we verify the result with `stat` and only error if the
        // ownership didn't actually take. That way prod Linux failures
        // (real permission problem) abort startup with a clear message,
        // while dev-machine warnings (chown says no but ownership is
        // already correct or doesn't matter) flow through.
        let chown_exit = self
            .run_root_exec(
                container_id,
                run_id,
                "chown-work",
                ownership_fix_command(CONTAINER_WORK_DIR, OwnershipScope::AcrossMounts),
            )
            .await?;

        // Probe: can the sandbox user actually write into the workspace?
        // `su temps -c 'touch ...'` is the truest test — if this fails,
        // every subsequent workflow command will fail too, so refuse to
        // hand back a broken sandbox.
        let probe_exit = self
            .run_root_exec(
                container_id,
                run_id,
                "write-probe",
                vec![
                    "su".to_string(),
                    SANDBOX_USER.to_string(),
                    "-c".to_string(),
                    write_probe_script(CONTAINER_WORK_DIR),
                ],
            )
            .await?;

        if probe_exit != 0 {
            return Err(AgentError::SandboxCreationFailed {
                run_id,
                provider: "docker".to_string(),
                reason: format!(
                    "sandbox user '{}' cannot write to {} (chown-work exit {}, \
                     write-probe exit {}). The bind-mounted workspace is owned by \
                     a uid the container can't normalize — usually because the \
                     host process that cloned the repo ran as root and the \
                     container lacks CAP_CHOWN, or userns-remap is rewriting uids \
                     in a way chown can't follow. Workflows would all fail with \
                     'Permission denied' so we're aborting startup instead.",
                    SANDBOX_USER, CONTAINER_WORK_DIR, chown_exit, probe_exit
                ),
            });
        }

        if chown_exit != 0 {
            tracing::warn!(
                "Sandbox {} chown-work exited {} but write-probe succeeded — \
                 likely a bind-mount backend (macOS Docker / userns-remap) where \
                 chown reports EPERM but the resulting ownership is workable.",
                container_id,
                chown_exit
            );
        }

        Ok(())
    }

    /// Shared exec implementation — one place that owns the bollard
    /// StartExec stream handling and the IDLE_POLL phantom-stream guard.
    /// Both `exec` (legacy stdout-only callback) and `exec_streamed`
    /// (stream-tagged callback) funnel through here so the two paths
    /// can't drift apart.
    ///
    /// When `on_event` is present, every stdout/stderr line is dispatched
    /// through the callback as it arrives. The returned `SandboxExecResult`
    /// has `stdout` containing **stdout only** and `stderr` containing
    /// **stderr only** — the bollard-side aggregation that used to fold
    /// stderr into `stdout` is gone. Callers that still want the combined
    /// view can concatenate at the call site.
    pub(super) async fn exec_inner(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_event: Option<OnStreamEventCallback>,
        user: Option<String>,
    ) -> Result<SandboxExecResult, AgentError> {
        let info = self.inspect_handle_policy(handle).await?;
        let uses_daemon = info
            .config
            .as_ref()
            .and_then(|c| c.labels.as_ref())
            .and_then(|labels| labels.get("sh.temps.runtime.protocol"))
            .map(String::as_str)
            == Some("1");
        let cmd = if uses_daemon && !exec_runs_as_root(user.as_deref()) {
            [
                vec![
                    "/usr/local/bin/temps-sandbox-runtime".to_string(),
                    "exec".to_string(),
                ],
                cmd,
            ]
            .concat()
        } else {
            cmd
        };
        // Pin PATH for every root exec, not just the one in `run_root_exec`.
        //
        // The image's own PATH puts sandbox-user-writable directories
        // (`{home}/.local/bin`, `{home}/.bun/bin`) ahead of the system ones, so
        // a root exec that inherits it will run a binary the sandbox user
        // planted. `exec_as_root` reaches this function with the caller's env
        // map, which never sets PATH — the credential-shred step before a
        // snapshot is one such caller, and there a planted `sh`/`shred`/`rm`
        // would run as container root *before* the credential file is wiped.
        //
        // Enforced here rather than at each call site so a new root-exec caller
        // cannot reintroduce the hole by forgetting, and the caller's own PATH
        // is overridden rather than merged: this is a privilege boundary, not a
        // default.
        let is_root_exec = exec_runs_as_root(user.as_deref());
        let mut env_vars: Vec<String> = env
            .iter()
            .filter(|(k, _)| !(is_root_exec && k.eq_ignore_ascii_case("PATH")))
            .map(|(k, v)| format!("{}={}", k, v))
            .collect();
        if is_root_exec {
            env_vars.push(format!("PATH={ROOT_EXEC_PATH}"));
        }

        let exec_config = bollard::models::ExecConfig {
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            cmd: Some(cmd.clone()),
            working_dir: Some(handle.work_dir.to_string_lossy().to_string()),
            env: if env_vars.is_empty() {
                None
            } else {
                Some(env_vars)
            },
            user,
            ..Default::default()
        };

        let exec = self
            .docker
            .create_exec(&handle.sandbox_id, exec_config)
            .await
            .map_err(|e| AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!("Failed to create exec: {}", e),
            })?;

        let start_config = bollard::exec::StartExecOptions {
            detach: false,
            ..Default::default()
        };

        let output = self
            .docker
            .start_exec(&exec.id, Some(start_config))
            .await
            .map_err(|e| AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!("Failed to start exec: {}", e),
            })?;

        let mut stdout_output = ExecOutputBuffer::new(self.config.exec_output_limit);
        let mut stderr_output = ExecOutputBuffer::new(self.config.exec_output_limit);

        match output {
            StartExecResults::Attached { mut output, .. } => {
                // See comment in prior implementation: bollard's exec stream
                // can park forever on phantom completions. IDLE_POLL + an
                // `inspect_exec` check lets us detect and break out of that.
                const IDLE_POLL: std::time::Duration = std::time::Duration::from_secs(15);
                loop {
                    match tokio::time::timeout(IDLE_POLL, output.next()).await {
                        Ok(Some(Ok(LogOutput::StdOut { message }))) => {
                            for line in stdout_output.push(&message) {
                                if let Some(ref cb) = on_event {
                                    cb(ExecStream::Stdout, line).await;
                                }
                            }
                        }
                        Ok(Some(Ok(LogOutput::StdErr { message }))) => {
                            for line in stderr_output.push(&message) {
                                if let Some(ref cb) = on_event {
                                    cb(ExecStream::Stderr, line).await;
                                }
                            }
                        }
                        Ok(Some(Ok(_))) => {}
                        Ok(Some(Err(e))) => {
                            tracing::warn!(
                                "Sandbox {} exec stream error: {}",
                                handle.sandbox_name,
                                e
                            );
                            break;
                        }
                        Ok(None) => {
                            // Stream closed cleanly — exec is done.
                            break;
                        }
                        Err(_) => match self.docker.inspect_exec(&exec.id).await {
                            Ok(info) if info.running == Some(true) => {
                                tracing::debug!(
                                    "Sandbox {} exec idle (still running), continuing to wait",
                                    handle.sandbox_name
                                );
                                continue;
                            }
                            Ok(_) => {
                                tracing::warn!(
                                        "Sandbox {} exec stream went idle and the exec is no longer running — bailing out to avoid permanent hang",
                                        handle.sandbox_name
                                    );
                                break;
                            }
                            Err(e) => {
                                tracing::warn!(
                                        "Sandbox {} inspect_exec failed during idle check: {} — bailing out",
                                        handle.sandbox_name,
                                        e
                                    );
                                break;
                            }
                        },
                    }
                }
            }
            StartExecResults::Detached => {
                return Err(AgentError::SandboxExecFailed {
                    run_id: 0,
                    sandbox_id: handle.sandbox_id.clone(),
                    reason: "Exec started in detached mode unexpectedly".to_string(),
                });
            }
        }

        for (stream, buffer) in [
            (ExecStream::Stdout, &mut stdout_output),
            (ExecStream::Stderr, &mut stderr_output),
        ] {
            for line in buffer.finish() {
                if let Some(ref cb) = on_event {
                    cb(stream, line).await;
                }
            }
        }
        let exit_code = self
            .docker
            .inspect_exec(&exec.id)
            .await
            .ok()
            .and_then(|i| i.exit_code)
            .unwrap_or(-1) as i32;

        Ok(SandboxExecResult {
            exit_code,
            stdout: stdout_output.into_output(),
            stderr: stderr_output.into_output(),
        })
    }
}
