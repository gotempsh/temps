// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Running `docker compose` commands: an isolated `docker` binary, input
//! arguments, and bounded, redacted, streamed output.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::AsyncReadExt;

use super::diagnostics::{
    drop_leading_fragment, redact_byte_ranges, sanitize_compose_diagnostic, truncated_line_head,
};
use super::files::RESERVED_GENERATED_COMPOSE_FILES;
use super::readiness::ComposePsEntry;
use super::{
    ComposeError, ComposeExecutor, COMPOSE_BUILD_TIMEOUT, COMPOSE_CONFIG_TIMEOUT,
    COMPOSE_PULL_TIMEOUT, COMPOSE_UP_TIMEOUT, MAX_COMPOSE_COMMAND_OUTPUT_BYTES,
};
use tracing::{debug, info};

const SAFE_DOCKER_PATH: &str = "/usr/local/bin:/usr/bin:/bin:/opt/homebrew/bin";
const DOCKER_BINARY_CANDIDATES: &[&str] = &[
    "/usr/local/bin/docker",
    "/usr/bin/docker",
    "/opt/homebrew/bin/docker",
];

/// Create an isolated Docker CLI process.
///
/// Compose interpolates values from its process environment before reading
/// `.env`/`--env-file`. Inheriting the Temps server environment would let an
/// untrusted repository reference `${TEMPS_ENCRYPTION_KEY}` (or any other
/// control-plane secret) and copy it into a container, label, command, or
/// image build. Resolve Docker only from administrator-owned system paths,
/// clear the inherited environment, and restore the small set of non-secret
/// Docker connection/configuration paths needed by legitimate installations.
pub(super) fn isolated_docker_command() -> tokio::process::Command {
    isolated_docker_command_with_candidates(DOCKER_BINARY_CANDIDATES)
}

pub(super) fn isolated_docker_command_with_candidates(
    candidates: &[&str],
) -> tokio::process::Command {
    let docker_binary = candidates
        .iter()
        .find(|candidate| std::path::Path::new(candidate).is_file())
        .copied()
        .unwrap_or("/usr/bin/docker");
    let mut command = tokio::process::Command::new(docker_binary);
    command.env_clear().env("PATH", SAFE_DOCKER_PATH);
    #[allow(
        clippy::disallowed_methods,
        reason = "subprocess passthrough: forwards the operator's Docker client settings (HOME, DOCKER_*) to the docker child process"
    )]
    for key in [
        "HOME",
        "DOCKER_CONFIG",
        "DOCKER_HOST",
        "DOCKER_CONTEXT",
        "DOCKER_TLS_VERIFY",
        "DOCKER_CERT_PATH",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
}

/// Callback receiving one batch of redacted output lines, in order.
pub type ComposeOutputCallback = Arc<
    dyn Fn(Vec<String>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// Receiver for live `docker compose` output, typically the deployment's job
/// log. Lines arrive in batches from a single forwarder, so the callback is
/// never invoked concurrently and a log writer can number entries safely.
#[derive(Clone)]
pub struct ComposeOutputSink(pub ComposeOutputCallback);

impl std::fmt::Debug for ComposeOutputSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ComposeOutputSink(..)")
    }
}

/// Where a command's output lines go, if anywhere, and the values neither the
/// streamed lines nor the captured diagnostic may reveal.
#[derive(Clone, Copy)]
struct OutputStream<'a> {
    sink: Option<&'a ComposeOutputSink>,
    redact_values: &'a [String],
}

/// Which end of a command's output survives when it exceeds
/// [`MAX_COMPOSE_COMMAND_OUTPUT_BYTES`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CapturedEnd {
    Head,
    Tail,
}

/// Longest single line forwarded to the sink. A longer line (a minified error
/// blob) is truncated rather than buffered unbounded, and never forwarded in
/// pieces: a piece could carry a fragment of a sensitive value that no
/// redaction rule recognises on its own.
pub(super) const MAX_STREAMED_LINE_BYTES: usize = 16 * 1024;
/// Lines waiting between the readers and the forwarder. A full channel makes
/// the readers wait, which in turn backpressures the Compose CLI.
const OUTPUT_CHANNEL_LINES: usize = 1024;
/// A batch is flushed once it holds this many lines or bytes, or once
/// [`OUTPUT_BATCH_WINDOW`] has passed since its first line. Batching keeps a
/// verbose build from issuing one log write per line, and the byte bound
/// stays well under the durable log's per-entry limit.
const MAX_OUTPUT_BATCH_LINES: usize = 200;
const MAX_OUTPUT_BATCH_BYTES: usize = 256 * 1024;
const OUTPUT_BATCH_WINDOW: std::time::Duration = std::time::Duration::from_millis(250);

impl ComposeExecutor {
    /// Run docker compose build for services with build: directives
    pub(super) async fn compose_build(
        &self,
        project_dir: &Path,
        project_name: &str,
        compose_file: &str,
        redact_values: &[String],
        build_args: &HashMap<String, String>,
        output_sink: Option<&ComposeOutputSink>,
    ) -> Result<(), ComposeError> {
        let cmd = Self::compose_build_command(project_dir, project_name, compose_file, build_args);

        debug!(project = %project_name, "Running docker compose build");

        let output = Self::bounded_command_output_streamed(
            cmd,
            COMPOSE_BUILD_TIMEOUT,
            project_name,
            "docker compose build",
            OutputStream {
                sink: output_sink,
                redact_values,
            },
        )
        .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr = sanitize_compose_diagnostic(&stderr, redact_values);
            return Err(ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!("docker compose build failed: {}", stderr),
            });
        }

        info!(project = %project_name, "docker compose build completed");
        Ok(())
    }

    fn compose_build_command(
        project_dir: &Path,
        project_name: &str,
        compose_file: &str,
        build_args: &HashMap<String, String>,
    ) -> tokio::process::Command {
        let mut cmd = isolated_docker_command();
        cmd.args(["compose", "-p", project_name]);
        Self::append_compose_input_args(&mut cmd, project_dir, compose_file);
        cmd.args(["build", "--pull"])
            .current_dir(project_dir)
            .env("PWD", project_dir.to_string_lossy().to_string());

        // Sort for deterministic command construction and test output. Build
        // arguments are appended explicitly, so they override any tenant
        // Compose `args:` value without placing tenant-controlled values in the
        // Docker CLI process environment.
        let mut sorted_build_args: Vec<_> = build_args.iter().collect();
        sorted_build_args.sort_by_key(|(key, _)| *key);
        for (key, value) in sorted_build_args {
            cmd.args(["--build-arg", &format!("{key}={value}")]);
        }

        cmd
    }

    pub(super) async fn bounded_command_output(
        command: tokio::process::Command,
        timeout: std::time::Duration,
        project_name: &str,
        operation: &str,
    ) -> Result<std::process::Output, ComposeError> {
        Self::bounded_command_output_streamed(
            command,
            timeout,
            project_name,
            operation,
            OutputStream {
                sink: None,
                redact_values: &[],
            },
        )
        .await
    }

    /// [`Self::bounded_command_output`], additionally forwarding every output
    /// line to `stream` as it is produced.
    async fn bounded_command_output_streamed(
        mut command: tokio::process::Command,
        timeout: std::time::Duration,
        project_name: &str,
        operation: &str,
        stream: OutputStream<'_>,
    ) -> Result<std::process::Output, ComposeError> {
        command
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!("{operation} did not expose stdout"),
            })?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!("{operation} did not expose stderr"),
            })?;
        // Both readers feed one forwarder, so the sink sees a single ordered
        // writer instead of two concurrent ones.
        let (stdout_lines, stderr_lines, forwarder) = match stream.sink {
            Some(sink) => {
                let (tx, rx) = tokio::sync::mpsc::channel(OUTPUT_CHANNEL_LINES);
                (
                    Some(tx.clone()),
                    Some(tx),
                    Some(Self::forward_output(rx, sink)),
                )
            }
            None => (None, None, None),
        };
        let run = async {
            let forward = async {
                if let Some(forwarder) = forwarder {
                    forwarder.await;
                }
                Ok::<_, std::io::Error>(())
            };
            // stdout is parsed as data by some callers, where a truncated
            // tail could parse as a different document; stderr is only ever
            // a diagnostic, whose error is at the end.
            let (stdout, stderr, status, ()) = tokio::try_join!(
                Self::read_bounded_stream(
                    stdout,
                    CapturedEnd::Head,
                    stream.redact_values,
                    stdout_lines
                ),
                Self::read_bounded_stream(
                    stderr,
                    CapturedEnd::Tail,
                    stream.redact_values,
                    stderr_lines
                ),
                child.wait(),
                forward
            )?;
            Ok::<_, std::io::Error>(std::process::Output {
                status,
                stdout,
                stderr,
            })
        };
        tokio::time::timeout(timeout, run)
            .await
            .map_err(|_| ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!("{operation} timed out after {} seconds", timeout.as_secs()),
            })?
            .map_err(ComposeError::Io)
    }

    /// Read `reader` to EOF, keeping at most [`MAX_COMPOSE_COMMAND_OUTPUT_BYTES`]
    /// from the `keep` end and sending each redacted line to `lines`.
    pub(super) async fn read_bounded_stream<R>(
        mut reader: R,
        keep: CapturedEnd,
        redact_values: &[String],
        lines: Option<tokio::sync::mpsc::Sender<String>>,
    ) -> Result<Vec<u8>, std::io::Error>
    where
        R: tokio::io::AsyncRead + Unpin,
    {
        let mut captured = Vec::new();
        let mut trimmed = false;
        let mut line = Vec::new();
        // Set once the current line passes MAX_STREAMED_LINE_BYTES: its head
        // has been forwarded and the rest is skipped until the next newline.
        let mut skipping = false;
        let mut buffer = [0_u8; 8192];
        loop {
            let read = reader.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            let chunk = &buffer[..read];
            match keep {
                CapturedEnd::Head => {
                    let remaining = MAX_COMPOSE_COMMAND_OUTPUT_BYTES.saturating_sub(captured.len());
                    captured.extend_from_slice(&chunk[..read.min(remaining)]);
                }
                CapturedEnd::Tail => {
                    captured.extend_from_slice(chunk);
                    // Trim only once the buffer doubles, so a long build does
                    // not shift a megabyte on every read.
                    if captured.len() > 2 * MAX_COMPOSE_COMMAND_OUTPUT_BYTES {
                        captured.drain(..captured.len() - MAX_COMPOSE_COMMAND_OUTPUT_BYTES);
                        trimmed = true;
                    }
                }
            }
            if let Some(lines) = lines.as_ref() {
                for &byte in chunk {
                    if byte == b'\n' || byte == b'\r' {
                        if !skipping {
                            Self::send_line(lines, redact_values, &line).await;
                        }
                        line.clear();
                        skipping = false;
                    } else if !skipping {
                        line.push(byte);
                        if line.len() >= MAX_STREAMED_LINE_BYTES {
                            let _ = lines.send(truncated_line_head(&line, redact_values)).await;
                            line.clear();
                            skipping = true;
                        }
                    }
                }
            }
        }
        if let Some(lines) = lines.as_ref() {
            if !skipping {
                Self::send_line(lines, redact_values, &line).await;
            }
        }
        if captured.len() > MAX_COMPOSE_COMMAND_OUTPUT_BYTES {
            captured.drain(..captured.len() - MAX_COMPOSE_COMMAND_OUTPUT_BYTES);
            trimmed = true;
        }
        if trimmed {
            drop_leading_fragment(&mut captured, redact_values);
        }
        Ok(captured)
    }

    /// Redact one complete line and hand it to the forwarder. Blank lines
    /// (including the empty half of a `\r\n`) are dropped. A line that
    /// exceeded [`MAX_STREAMED_LINE_BYTES`] goes through
    /// [`truncated_line_head`] instead.
    async fn send_line(
        lines: &tokio::sync::mpsc::Sender<String>,
        redact_values: &[String],
        line: &[u8],
    ) {
        let Some(last) = line.iter().rposition(|byte| !byte.is_ascii_whitespace()) else {
            return;
        };
        let text = redact_byte_ranges(&line[..=last], redact_values, None);
        // The forwarder outlives both readers; a closed channel only means
        // the command is being abandoned (timeout), so the line is moot.
        let _ = lines.send(text).await;
    }

    /// Drain redacted lines into batches and deliver each batch to `sink`.
    /// Runs until both readers have finished and the channel is empty.
    async fn forward_output(mut rx: tokio::sync::mpsc::Receiver<String>, sink: &ComposeOutputSink) {
        while let Some(first) = rx.recv().await {
            let deadline = tokio::time::Instant::now() + OUTPUT_BATCH_WINDOW;
            let mut batch_bytes = first.len();
            let mut batch = vec![first];
            while batch.len() < MAX_OUTPUT_BATCH_LINES && batch_bytes < MAX_OUTPUT_BATCH_BYTES {
                match tokio::time::timeout_at(deadline, rx.recv()).await {
                    Ok(Some(line)) => {
                        batch_bytes += line.len();
                        batch.push(line);
                    }
                    // Channel closed or window elapsed: deliver what we have.
                    Ok(None) | Err(_) => break,
                }
            }
            (sink.0)(batch).await;
        }
    }

    /// The `-f` and `--env-file` arguments shared by every `docker compose`
    /// command against a deployed stack (`build`, `pull`, `up`, `ps`, `stop`,
    /// `down`), so each one sees exactly the model `up` deployed.
    ///
    /// Generated overrides follow the user's compose file; the secrets
    /// override is last so a repository cannot redirect where secrets land.
    /// Env files are passed repository `.env` first and Temps' `.env.temps`
    /// second: Compose lets a later `--env-file` override an earlier one, and
    /// project variables configured in Temps must win over repository
    /// defaults — the same order configuration resolution uses.
    pub(crate) fn compose_input_args(project_dir: &Path, compose_file: &str) -> Vec<String> {
        let mut args = vec!["-f".to_string(), compose_file.to_string()];
        for generated in RESERVED_GENERATED_COMPOSE_FILES {
            if project_dir.join(generated).exists() {
                args.push("-f".to_string());
                args.push((*generated).to_string());
            }
        }
        for env_file in [".env", ".env.temps"] {
            if project_dir.join(env_file).exists() {
                args.push("--env-file".to_string());
                args.push(env_file.to_string());
            }
        }
        args
    }

    pub(super) fn append_compose_input_args(
        cmd: &mut tokio::process::Command,
        project_dir: &Path,
        compose_file: &str,
    ) {
        cmd.args(Self::compose_input_args(project_dir, compose_file));
    }

    /// Idempotently create the shared Docker network every Temps-managed
    /// external service and single-container app deployment joins
    /// (`temps_core::NETWORK_NAME`). Same implementation as the other
    /// deployers ([`temps_core::docker_network::ensure_bridge_network`]), so a
    /// concurrent deploy that creates it first is not a failure here either.
    pub(super) async fn ensure_temps_network_exists(&self) -> Result<(), ComposeError> {
        let docker = self.require_docker()?;
        let network_name = temps_core::NETWORK_NAME.as_str();
        temps_core::docker_network::ensure_bridge_network(&docker, network_name)
            .await
            .map(|_| ())
            .map_err(|e| {
                ComposeError::Docker(format!("Failed to create network {network_name}: {e}"))
            })
    }

    /// Pull images for every `image:`-based service in the compose stack.
    ///
    /// This is intentionally a separate step from [`compose_up`] so that
    /// image-fetch latency — which can range from seconds to minutes depending
    /// on registry speed and image size — occurs while the *old* containers are
    /// still running and serving traffic, not during the downtime window between
    /// old-container-stop and new-container-healthy. By the time `compose_up`
    /// tears down and recreates containers the images are already local, which
    /// keeps the actual downtime window as short as possible.
    ///
    /// `--ignore-buildable` skips services that only declare a `build:` stanza
    /// and have no pullable `image:` tag; those services are handled by
    /// `compose_build`. This makes the call safe regardless of whether the
    /// project mixes built and pulled services.
    pub(super) async fn compose_pull(
        &self,
        project_dir: &Path,
        project_name: &str,
        compose_file: &str,
        redact_values: &[String],
        output_sink: Option<&ComposeOutputSink>,
    ) -> Result<(), ComposeError> {
        let mut cmd = isolated_docker_command();
        cmd.args(["compose", "-p", project_name]);
        Self::append_compose_input_args(&mut cmd, project_dir, compose_file);

        cmd.args(["pull", "--ignore-buildable"])
            .current_dir(project_dir)
            .env("PWD", project_dir.to_string_lossy().to_string());

        // Cancellation drops the command future; terminate the Compose CLI so
        // compensating `compose down` cannot race a still-running `compose pull`.
        cmd.kill_on_drop(true);

        debug!(project = %project_name, "Running docker compose pull");

        let output = Self::bounded_command_output_streamed(
            cmd,
            COMPOSE_PULL_TIMEOUT,
            project_name,
            "docker compose pull",
            OutputStream {
                sink: output_sink,
                redact_values,
            },
        )
        .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr = sanitize_compose_diagnostic(&stderr, redact_values);
            return Err(ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!("docker compose pull failed: {stderr}"),
            });
        }

        info!(project = %project_name, "docker compose pull completed");
        Ok(())
    }

    pub(super) async fn compose_up(
        &self,
        project_dir: &Path,
        project_name: &str,
        compose_file: &str,
        redact_values: &[String],
        output_sink: Option<&ComposeOutputSink>,
    ) -> Result<(), ComposeError> {
        let mut cmd = isolated_docker_command();
        cmd.args(["compose", "-p", project_name]);
        Self::append_compose_input_args(&mut cmd, project_dir, compose_file);

        cmd.args(["up", "-d", "--remove-orphans", "--force-recreate"])
            .current_dir(project_dir);

        // Set PWD so compose files using ${PWD} resolve correctly
        cmd.env("PWD", project_dir.to_string_lossy().to_string());

        // Cancellation drops the command future; terminate the Compose CLI so
        // compensating `compose down` cannot race a still-running `compose up`.
        cmd.kill_on_drop(true);

        debug!(project = %project_name, "Running docker compose up");

        let output = Self::bounded_command_output_streamed(
            cmd,
            COMPOSE_UP_TIMEOUT,
            project_name,
            "docker compose up",
            OutputStream {
                sink: output_sink,
                redact_values,
            },
        )
        .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // `up` blocks on `depends_on: condition: service_healthy`, so a
            // dependency that never becomes healthy fails `up` itself — before
            // this fix, that surfaced as just "container X is unhealthy" with
            // no indication of *why* X was unhealthy.
            let container_logs = self
                .describe_unhealthy_containers(
                    project_dir,
                    project_name,
                    compose_file,
                    redact_values,
                )
                .await;
            let diagnostic = sanitize_compose_diagnostic(
                &format!("{}{}", stderr, container_logs),
                redact_values,
            );
            return Err(ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!("docker compose up failed: {diagnostic}"),
            });
        }

        let stdout = redact_byte_ranges(&output.stdout, redact_values, None);
        debug!(project = %project_name, stdout = %stdout, "docker compose up completed");

        Ok(())
    }

    /// Run `docker compose ps --format json --all` and parse its output.
    /// Shared by `discover_containers` (final result assembly) and
    /// `wait_for_services_ready` (readiness polling).
    pub(super) async fn compose_ps(
        &self,
        project_dir: &Path,
        project_name: &str,
        compose_file: &str,
    ) -> Result<Vec<ComposePsEntry>, ComposeError> {
        // Same `-f`/`--env-file` inputs as `up`: a `${VAR:?}` that only the
        // Temps variables satisfy would otherwise fail discovery after a
        // successful `up`, and generated overrides change the model.
        let mut command = isolated_docker_command();
        command.args(["compose", "-p", project_name]);
        Self::append_compose_input_args(&mut command, project_dir, compose_file);
        command
            .args(["ps", "--format", "json", "--all"])
            .current_dir(project_dir);
        let output = Self::bounded_command_output(
            command,
            COMPOSE_CONFIG_TIMEOUT,
            project_name,
            "docker compose ps",
        )
        .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ComposeError::DiscoveryFailed {
                project: project_name.to_string(),
                reason: format!("docker compose ps failed: {}", stderr),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut entries = Vec::new();

        // docker compose ps --format json outputs one JSON object per line
        for line in stdout.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let entry: ComposePsEntry =
                serde_json::from_str(line).map_err(|e| ComposeError::DiscoveryFailed {
                    project: project_name.to_string(),
                    reason: format!("Failed to parse compose ps output: {} (line: {})", e, line),
                })?;
            entries.push(entry);
        }

        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::diagnostics::sanitize_compose_diagnostic;
    use crate::compose::files::render_env_file;
    use crate::compose::test_support::test_executor;
    use crate::compose::{
        ComposeExecutor, MAX_COMPOSE_COMMAND_OUTPUT_BYTES, TEMPS_SECRETS_OVERRIDE,
    };

    #[test]
    fn compose_build_forces_platform_build_args_over_tenant_values() {
        let project_dir = Path::new("/tmp/temps-compose-command-test");
        let build_args = HashMap::from([(
            "BUILDKIT_CACHE_MOUNT_NS".to_string(),
            "platform-derived".to_string(),
        )]);

        let command = ComposeExecutor::compose_build_command(
            project_dir,
            "temps-42-7",
            "docker-compose.yml",
            &build_args,
        );
        let command = command.as_std();
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert!(
            args.windows(2).any(|pair| {
                pair == [
                    "--build-arg".to_string(),
                    "BUILDKIT_CACHE_MOUNT_NS=platform-derived".to_string(),
                ]
            }),
            "platform namespace must be an explicit Compose build argument: {args:?}",
        );
        assert!(!args.iter().any(|arg| arg.contains("tenant-controlled")));
        assert!(!args.iter().any(|arg| arg.contains("RUNTIME_ONLY")));

        let namespace_env = command
            .get_envs()
            .find(|(key, _)| *key == std::ffi::OsStr::new("BUILDKIT_CACHE_MOUNT_NS"))
            .and_then(|(_, value)| value)
            .map(|value| value.to_string_lossy().into_owned());
        assert_eq!(namespace_env, None);
        let allowed = [
            "PATH",
            "PWD",
            "HOME",
            "DOCKER_CONFIG",
            "DOCKER_HOST",
            "DOCKER_CONTEXT",
            "DOCKER_TLS_VERIFY",
            "DOCKER_CERT_PATH",
        ];
        assert!(command.get_envs().all(|(key, _)| {
            allowed
                .iter()
                .any(|allowed_key| key == std::ffi::OsStr::new(allowed_key))
        }));
        assert!(command
            .get_envs()
            .all(|(key, _)| key != std::ffi::OsStr::new("TEMPS_ENCRYPTION_KEY")));
    }

    /// What a [`recording_sink`] saw: every batch, in delivery order, and
    /// whether two deliveries ever overlapped.
    struct SinkRecording {
        batches: Arc<std::sync::Mutex<Vec<Vec<String>>>>,
        overlapped: Arc<std::sync::atomic::AtomicBool>,
    }

    fn recording_sink() -> (ComposeOutputSink, SinkRecording) {
        let recording = SinkRecording {
            batches: Arc::new(std::sync::Mutex::new(Vec::new())),
            overlapped: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let in_flight = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (received, overlap) = (recording.batches.clone(), recording.overlapped.clone());
        let sink = ComposeOutputSink(Arc::new(move |batch: Vec<String>| {
            let (received, overlap, busy) = (received.clone(), overlap.clone(), in_flight.clone());
            Box::pin(async move {
                if busy.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    overlap.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                // Yield so an overlapping delivery would actually interleave.
                tokio::task::yield_now().await;
                if let Ok(mut batches) = received.lock() {
                    batches.push(batch);
                }
                busy.store(false, std::sync::atomic::Ordering::SeqCst);
            })
        }));
        (sink, recording)
    }

    #[tokio::test]
    async fn streamed_lines_are_redacted_and_the_last_unterminated_line_is_sent() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let redact = vec!["s3cr3t-value".to_string()];
        let output: &[u8] =
            b"#1 [internal] load build definition\r\n\n#2 ARG TOKEN=s3cr3t-value\n#3 ERROR: boom";

        let captured =
            ComposeExecutor::read_bounded_stream(output, CapturedEnd::Tail, &redact, Some(tx))
                .await
                .unwrap();

        assert_eq!(captured, output);
        let mut received = Vec::new();
        while let Some(line) = rx.recv().await {
            received.push(line);
        }
        assert_eq!(
            received,
            vec![
                "#1 [internal] load build definition".to_string(),
                "#2 ARG TOKEN=<redacted>".to_string(),
                "#3 ERROR: boom".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn an_overlong_line_never_forwards_a_fragment_of_a_sensitive_value() {
        let secret = "Zq7Xw2Lp9Vn4Rb8T".to_string();
        let redact = vec![secret.clone()];
        // The secret starts just before the cut, so the head carries only
        // its first bytes, and the rest of it arrives after the cut.
        let mut output = "x".repeat(MAX_STREAMED_LINE_BYTES - 5).into_bytes();
        output.push(b' ');
        output.extend_from_slice(secret.as_bytes());
        output.extend_from_slice(b" trailing\n#9 next line\n");
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);

        ComposeExecutor::read_bounded_stream(
            output.as_slice(),
            CapturedEnd::Tail,
            &redact,
            Some(tx),
        )
        .await
        .unwrap();

        let mut received = Vec::new();
        while let Some(line) = rx.recv().await {
            received.push(line);
        }
        assert_eq!(received.len(), 2, "{received:?}");
        assert!(received[0].ends_with("[line truncated after 16384 bytes]"));
        for fragment in [&secret[..4], &secret[..3]] {
            assert!(
                !received[0].contains(fragment),
                "truncated line leaked {fragment:?}"
            );
        }
        // The remainder of the overlong line is skipped, not forwarded.
        assert!(!received.iter().any(|line| line.contains("trailing")));
        assert_eq!(received[1], "#9 next line");
    }

    #[tokio::test]
    async fn stdout_and_stderr_reach_the_sink_in_batches_from_a_single_writer() {
        let (sink, recording) = recording_sink();
        let redact: Vec<String> = Vec::new();
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args([
            "-c",
            "i=0; while [ $i -lt 300 ]; do echo out-$i; echo err-$i >&2; i=$((i+1)); done",
        ]);

        let output = ComposeExecutor::bounded_command_output_streamed(
            command,
            std::time::Duration::from_secs(30),
            "temps-stream-test",
            "stream test",
            OutputStream {
                sink: Some(&sink),
                redact_values: &redact,
            },
        )
        .await
        .unwrap();

        assert!(output.status.success());
        assert!(!recording
            .overlapped
            .load(std::sync::atomic::Ordering::SeqCst));
        let batches = recording.batches.lock().unwrap().clone();
        assert!(
            batches.len() < 600,
            "600 lines must be batched, got {} deliveries",
            batches.len()
        );
        assert!(batches
            .iter()
            .all(|batch| !batch.is_empty() && batch.len() <= MAX_OUTPUT_BATCH_LINES));
        let lines: Vec<&String> = batches.iter().flatten().collect();
        assert_eq!(lines.len(), 600);
        // Each stream's own order is preserved.
        let stdout_order: Vec<&&String> = lines
            .iter()
            .filter(|line| line.starts_with("out-"))
            .collect();
        assert_eq!(
            stdout_order.first().map(|line| line.as_str()),
            Some("out-0")
        );
        assert_eq!(
            stdout_order.last().map(|line| line.as_str()),
            Some("out-299")
        );
    }

    #[tokio::test]
    async fn stderr_capture_keeps_the_last_bytes_and_stdout_the_first() {
        let mut output = vec![b'a'; MAX_COMPOSE_COMMAND_OUTPUT_BYTES * 3];
        output.extend_from_slice(b"the error");

        let tail =
            ComposeExecutor::read_bounded_stream(output.as_slice(), CapturedEnd::Tail, &[], None)
                .await
                .unwrap();
        let head =
            ComposeExecutor::read_bounded_stream(output.as_slice(), CapturedEnd::Head, &[], None)
                .await
                .unwrap();

        // No whole line follows the cut, so the line's ending is kept and
        // only its cut-through first token is dropped.
        assert!(tail.len() <= MAX_COMPOSE_COMMAND_OUTPUT_BYTES);
        assert!(tail.starts_with("… ".as_bytes()));
        assert!(tail.ends_with(b"error"));
        assert_eq!(head.len(), MAX_COMPOSE_COMMAND_OUTPUT_BYTES);
        assert!(head.iter().all(|byte| *byte == b'a'));
    }

    #[tokio::test]
    async fn trimmed_stderr_keeps_a_single_overlong_final_line() {
        // One error line longer than the capture, ending in a newline: there
        // is no whole line after the cut, so the tail must survive.
        let mut output = vec![b'a'; MAX_COMPOSE_COMMAND_OUTPUT_BYTES * 2];
        output.extend_from_slice(b" the actual error\n");

        let tail =
            ComposeExecutor::read_bounded_stream(output.as_slice(), CapturedEnd::Tail, &[], None)
                .await
                .unwrap();

        assert!(tail.len() <= MAX_COMPOSE_COMMAND_OUTPUT_BYTES);
        assert!(tail.ends_with(b"the actual error\n"));
        let diagnostic = sanitize_compose_diagnostic(&String::from_utf8_lossy(&tail), &[]);
        assert!(diagnostic.contains("the actual error"));
    }

    #[tokio::test]
    async fn trimmed_line_of_large_secrets_keeps_the_error_after_masking() {
        // 16 KiB sensitive values, repeated past the capture limit, then a
        // 1 KiB error. Masking shrinks the kept text below one value's
        // length, so the amount removed must not be measured after masking.
        let secret: String = (0..16 * 1024)
            .map(|index| (b'a' + ((index * 31 + index / 97) % 26) as u8) as char)
            .collect();
        let error = format!("ERROR: {}", "e".repeat(1024));
        let mut output = format!("{secret} ").repeat(70).into_bytes();
        output.extend_from_slice(error.as_bytes());
        output.push(b'\n');
        let offset_in_copy = (output.len() - MAX_COMPOSE_COMMAND_OUTPUT_BYTES) % (secret.len() + 1);
        assert!((1..secret.len()).contains(&offset_in_copy));
        let redact = vec![secret.clone()];

        let tail = ComposeExecutor::read_bounded_stream(
            output.as_slice(),
            CapturedEnd::Tail,
            &redact,
            None,
        )
        .await
        .unwrap();

        let diagnostic = sanitize_compose_diagnostic(&String::from_utf8_lossy(&tail), &redact);
        assert!(diagnostic.contains(&error), "{diagnostic:.300}");
        for window in secret.as_bytes().windows(16).step_by(97) {
            let fragment = std::str::from_utf8(window).unwrap();
            assert!(
                !diagnostic.contains(fragment),
                "diagnostic leaked secret fragment {fragment:?}"
            );
        }
    }

    #[tokio::test]
    async fn trimmed_overlong_line_never_starts_with_a_secret_fragment() {
        // One line over the capture limit made of a 1 KiB sensitive value
        // repeated, ending in the error. The trim lands inside one copy, so
        // the capture would open with the rest of it; redacting the other
        // copies then shrinks the text so far that no later length cut
        // would remove that fragment.
        let secret: String = (0..1024)
            .map(|index| {
                let alphabet = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
                alphabet[(index * 7 + index / 13) % alphabet.len()] as char
            })
            .collect();
        let mut output = format!("{secret} ").repeat(1100).into_bytes();
        output.extend_from_slice(b"final error\n");
        let offset_in_copy = (output.len() - MAX_COMPOSE_COMMAND_OUTPUT_BYTES) % (secret.len() + 1);
        assert!(
            (1..secret.len()).contains(&offset_in_copy),
            "the cut must land inside a copy of the secret, got offset {offset_in_copy}"
        );
        let redact = vec![secret.clone()];

        let tail = ComposeExecutor::read_bounded_stream(
            output.as_slice(),
            CapturedEnd::Tail,
            &redact,
            None,
        )
        .await
        .unwrap();

        let diagnostic = sanitize_compose_diagnostic(&String::from_utf8_lossy(&tail), &redact);
        assert!(diagnostic.ends_with("final error\n"), "{diagnostic:.200}");
        for window in secret.as_bytes().windows(8) {
            let fragment = std::str::from_utf8(window).unwrap();
            assert!(
                !diagnostic.contains(fragment),
                "diagnostic leaked secret fragment {fragment:?}"
            );
        }
    }

    #[tokio::test]
    async fn trimmed_stderr_starts_at_a_whole_line() {
        // A sensitive value split by the trim must not open the capture.
        let mut output =
            b"prefix TOKEN-VALUE-SPLIT-HERE\n".repeat(MAX_COMPOSE_COMMAND_OUTPUT_BYTES / 10);
        output.extend_from_slice(b"final error\n");

        let tail =
            ComposeExecutor::read_bounded_stream(output.as_slice(), CapturedEnd::Tail, &[], None)
                .await
                .unwrap();

        assert!(tail.len() <= MAX_COMPOSE_COMMAND_OUTPUT_BYTES);
        assert!(tail.starts_with(b"prefix TOKEN-VALUE-SPLIT-HERE\n"));
        assert!(tail.ends_with(b"final error\n"));
    }

    /// A real failing `docker compose build`: project variables reach the
    /// Dockerfile as build args, every line streams to the sink with secrets
    /// redacted, and the error keeps the failing step instead of the start
    /// of the build log.
    #[tokio::test]
    async fn failing_compose_build_streams_output_and_reports_the_failing_step() {
        let Some(executor) = test_executor() else {
            return;
        };
        let compose_available = isolated_docker_command()
            .args(["compose", "version"])
            .output()
            .await
            .is_ok_and(|output| output.status.success());
        let alpine_available = match executor.docker.require() {
            Ok(docker) => docker.inspect_image("alpine:latest").await.is_ok(),
            Err(_) => false,
        };
        if !compose_available || !alpine_available {
            println!("Docker Compose or alpine:latest is unavailable; skipping runtime test");
            return;
        }

        let project_dir = tempfile::tempdir().unwrap();
        let compose = "services:\n  app:\n    build: .\n";
        tokio::fs::write(project_dir.path().join("compose.yml"), compose)
            .await
            .unwrap();
        tokio::fs::write(
            project_dir.path().join("Dockerfile"),
            "FROM alpine:latest\n\
             ARG GREETING\n\
             ARG API_TOKEN\n\
             RUN echo \"greeting=$GREETING token=$API_TOKEN\"\n\
             RUN echo \"about to fail\" && exit 3\n",
        )
        .await
        .unwrap();
        // Unique per run: a cached layer would not print its output again.
        let greeting = format!(
            "hello-from-project-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        );
        let project_vars = HashMap::from([
            ("GREETING".to_string(), greeting.clone()),
            ("API_TOKEN".to_string(), "tok-7f3a9c1e5b".to_string()),
        ]);
        let env_path = project_dir.path().join(".env.temps");
        tokio::fs::write(&env_path, render_env_file(&project_vars).unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            project_dir.path().join("docker-compose.temps-env.yml"),
            executor.generate_env_override(compose, &env_path.to_string_lossy(), &project_vars),
        )
        .await
        .unwrap();

        let (sink, recording) = recording_sink();
        // Only the token is sensitive here, so the greeting stays readable.
        let redact = vec!["tok-7f3a9c1e5b".to_string()];
        let project_name = format!("temps-build-stream-{}", std::process::id());

        let error = executor
            .compose_build(
                project_dir.path(),
                &project_name,
                "compose.yml",
                &redact,
                &HashMap::new(),
                Some(&sink),
            )
            .await
            .expect_err("the Dockerfile exits 3, so the build must fail");

        let streamed = recording.batches.lock().unwrap().concat().join("\n");
        assert!(
            streamed.contains(&format!("greeting={greeting}")),
            "project variables must reach the Dockerfile as build args:\n{streamed}"
        );
        assert!(streamed.contains("about to fail"), "{streamed}");
        assert!(!streamed.contains("tok-7f3a9c1e5b"), "{streamed}");
        assert!(streamed.contains("token=<redacted>"), "{streamed}");

        // BuildKit and the classic builder word the failing step differently.
        let message = error.to_string();
        assert!(
            message.contains("exit code: 3") || message.contains("non-zero code: 3"),
            "{message}"
        );
        assert!(!message.contains("tok-7f3a9c1e5b"), "{message}");
    }

    #[test]
    fn every_compose_command_shares_files_and_env_files_in_override_order() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            ComposeExecutor::compose_input_args(dir.path(), "deploy/compose.yaml"),
            vec!["-f", "deploy/compose.yaml"]
        );
        for file in [
            "docker-compose.temps-env.yml",
            "docker-compose.temps-labels.yml",
            TEMPS_SECRETS_OVERRIDE,
            ".env",
            ".env.temps",
        ] {
            std::fs::write(dir.path().join(file), "").unwrap();
        }
        assert_eq!(
            ComposeExecutor::compose_input_args(dir.path(), "deploy/compose.yaml"),
            vec![
                "-f",
                "deploy/compose.yaml",
                "-f",
                "docker-compose.temps-env.yml",
                "-f",
                "docker-compose.temps-labels.yml",
                "-f",
                TEMPS_SECRETS_OVERRIDE,
                // Later --env-file wins: Temps variables override the repo .env.
                "--env-file",
                ".env",
                "--env-file",
                ".env.temps",
            ]
        );
    }
}
