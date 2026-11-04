// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Run a command in a container and capture its output under an explicit limit.
//!
//! The output stream has to be drained for the command to finish, and a
//! container can print without end, so how much of it stays in memory is a
//! decision every caller makes: [`ExecCaptureOptions`] has no default limit.
//!
//! What this does not do: it never kills the command. A timeout or a
//! cancellation stops the wait; the command may still be running in the
//! container, and a caller that must stop it has to do so itself.

use std::time::Duration;

use bollard::container::LogOutput;
use bollard::errors::Error as DockerError;
use bollard::exec::{StartExecOptions, StartExecResults};
use bollard::models::ExecConfig;
use bollard::Docker;
use futures::{Stream, StreamExt};
use tokio_util::sync::CancellationToken;

use crate::error::DockerUtilError;

/// How much of an output stream stays in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputLimit {
    /// Keep everything. Only for a command whose output is known to be small,
    /// or a caller that must parse all of it.
    Unbounded,
    /// Keep the first `n` bytes; the rest is read and dropped. `Head(0)` keeps
    /// nothing. A multi-byte character at the cut comes out as `U+FFFD`.
    Head(usize),
    /// Keep the last `n` bytes, which is where a failure's diagnostics are. A
    /// multi-byte character at the cut comes out as `U+FFFD`.
    Tail(usize),
}

/// What to do when the output stream reports an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamErrorPolicy {
    /// Fail with [`DockerUtilError::ExecStream`], losing the output so far.
    Fail,
    /// Stop reading and return what was captured, with the error in
    /// [`ExecOutput::stream_error`].
    Stop,
    /// Keep reading. The error is dropped.
    Ignore,
}

/// What to do about the exec's exit code once its output has ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCodePolicy {
    /// Inspect the exec; failing to is [`DockerUtilError::ExecInspect`].
    Required,
    /// Inspect the exec; failing to leaves [`ExecOutput::exit_code`] empty and
    /// the error in [`ExecOutput::inspect_error`], so the output is not lost.
    BestEffort,
    /// Do not inspect the exec. For a caller that judges by the output alone.
    NotRead,
}

/// Frames that are neither stdout nor stderr. Docker sends them when it does
/// not multiplex the stream, as for an exec with a TTY.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtherFrames {
    /// Read and drop them.
    Drop,
    /// Keep them with stdout.
    ToStdout,
}

#[derive(Debug, Clone, Copy)]
enum Capture {
    Separate {
        stdout: OutputLimit,
        stderr: OutputLimit,
    },
    Combined(OutputLimit),
}

/// How [`exec_capture`] reads an exec. Built from the output limits, which have
/// no default. The other settings start at the strict choice: frames that are
/// neither stdout nor stderr are dropped, a stream or inspect error is a
/// failure, and the wait is not bounded.
#[derive(Debug, Clone)]
pub struct ExecCaptureOptions {
    capture: Capture,
    other_frames: OtherFrames,
    stream_errors: StreamErrorPolicy,
    exit_code: ExitCodePolicy,
    timeout: Option<Duration>,
    cancel: Option<CancellationToken>,
}

impl ExecCaptureOptions {
    fn new(capture: Capture) -> Self {
        Self {
            capture,
            other_frames: OtherFrames::Drop,
            stream_errors: StreamErrorPolicy::Fail,
            exit_code: ExitCodePolicy::Required,
            timeout: None,
            cancel: None,
        }
    }

    /// Keep stdout and stderr apart, each under its own limit.
    pub fn separate(stdout: OutputLimit, stderr: OutputLimit) -> Self {
        Self::new(Capture::Separate { stdout, stderr })
    }

    /// Keep stdout and stderr interleaved as they arrived, in
    /// [`ExecOutput::stdout`], under one limit. [`ExecOutput::stderr`] stays
    /// empty.
    pub fn combined(limit: OutputLimit) -> Self {
        Self::new(Capture::Combined(limit))
    }

    #[must_use]
    pub fn other_frames(mut self, other_frames: OtherFrames) -> Self {
        self.other_frames = other_frames;
        self
    }

    #[must_use]
    pub fn stream_errors(mut self, stream_errors: StreamErrorPolicy) -> Self {
        self.stream_errors = stream_errors;
        self
    }

    #[must_use]
    pub fn exit_code(mut self, exit_code: ExitCodePolicy) -> Self {
        self.exit_code = exit_code;
        self
    }

    /// Bound starting the exec and reading its output. Creating and inspecting
    /// it are not covered: a caller that needs a bound on the whole operation
    /// wraps the call. Expiry is [`DockerUtilError::ExecTimedOut`] and does not
    /// stop the command.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Stop reading when `token` fires, with
    /// [`DockerUtilError::ExecCancelled`]. Does not stop the command.
    #[must_use]
    pub fn cancel(mut self, token: &CancellationToken) -> Self {
        self.cancel = Some(token.clone());
        self
    }
}

/// What an exec produced.
#[derive(Debug)]
pub struct ExecOutput {
    /// The daemon's exit code. `None` if it reports none, or if the exec was
    /// not inspected or could not be (see [`ExitCodePolicy`]).
    pub exit_code: Option<i64>,
    pub stdout: String,
    pub stderr: String,
    /// Whether the limit dropped any of stdout.
    pub stdout_truncated: bool,
    /// Whether the limit dropped any of stderr.
    pub stderr_truncated: bool,
    /// The error that ended the output, under [`StreamErrorPolicy::Stop`].
    pub stream_error: Option<DockerError>,
    /// Why the exec could not be inspected, under
    /// [`ExitCodePolicy::BestEffort`].
    pub inspect_error: Option<DockerError>,
}

/// Run an exec and capture its output as `options` says.
///
/// `config` is passed to the daemon as it is, so to capture anything it has to
/// set `attach_stdout`/`attach_stderr`. Its `cmd` and `env` never appear in an
/// error, since they can hold credentials.
pub async fn exec_capture(
    docker: &Docker,
    container: &str,
    config: impl Into<ExecConfig>,
    options: ExecCaptureOptions,
) -> Result<ExecOutput, DockerUtilError> {
    let exec = docker
        .create_exec(container, config)
        .await
        .map_err(|source| DockerUtilError::ExecCreate {
            container: container.to_string(),
            source,
        })?;

    let read = start_and_drain(docker, container, &exec.id, &options);
    let drained = match options.timeout {
        Some(timeout) => tokio::time::timeout(timeout, read).await.map_err(|_| {
            DockerUtilError::ExecTimedOut {
                container: container.to_string(),
                exec_id: exec.id.clone(),
                timeout_secs: timeout.as_secs(),
            }
        })??,
        None => read.await?,
    };

    let (exit_code, inspect_error) = match options.exit_code {
        ExitCodePolicy::NotRead => (None, None),
        ExitCodePolicy::Required => {
            let inspected = docker.inspect_exec(&exec.id).await.map_err(|source| {
                DockerUtilError::ExecInspect {
                    container: container.to_string(),
                    exec_id: exec.id.clone(),
                    source,
                }
            })?;
            (inspected.exit_code, None)
        }
        ExitCodePolicy::BestEffort => match docker.inspect_exec(&exec.id).await {
            Ok(inspected) => (inspected.exit_code, None),
            Err(error) => (None, Some(error)),
        },
    };

    Ok(ExecOutput {
        exit_code,
        stdout_truncated: drained.stdout.truncated(),
        stderr_truncated: drained
            .stderr
            .as_ref()
            .is_some_and(CaptureBuffer::truncated),
        stdout: drained.stdout.into_string(),
        stderr: drained
            .stderr
            .map(CaptureBuffer::into_string)
            .unwrap_or_default(),
        stream_error: drained.stream_error,
        inspect_error,
    })
}

async fn start_and_drain(
    docker: &Docker,
    container: &str,
    exec_id: &str,
    options: &ExecCaptureOptions,
) -> Result<Drained, DockerUtilError> {
    let started = docker
        .start_exec(exec_id, None::<StartExecOptions>)
        .await
        .map_err(|source| DockerUtilError::ExecStart {
            container: container.to_string(),
            exec_id: exec_id.to_string(),
            source,
        })?;
    // Not asking for detach means this is always `Attached`; there is nothing
    // to read otherwise.
    let StartExecResults::Attached { output, .. } = started else {
        return Ok(Drained::empty(options));
    };
    drain_output(output, options)
        .await
        .map_err(|interrupted| match interrupted {
            DrainInterrupted::Stream(source) => DockerUtilError::ExecStream {
                container: container.to_string(),
                exec_id: exec_id.to_string(),
                source,
            },
            DrainInterrupted::Cancelled => DockerUtilError::ExecCancelled {
                container: container.to_string(),
                exec_id: exec_id.to_string(),
            },
        })
}

#[derive(Debug)]
enum DrainInterrupted {
    Stream(DockerError),
    Cancelled,
}

#[derive(Debug)]
struct Drained {
    /// Stdout, or everything when the capture is combined.
    stdout: CaptureBuffer,
    /// `None` when the capture is combined.
    stderr: Option<CaptureBuffer>,
    stream_error: Option<DockerError>,
}

impl Drained {
    fn empty(options: &ExecCaptureOptions) -> Self {
        let (stdout, stderr) = match options.capture {
            Capture::Separate { stdout, stderr } => {
                (CaptureBuffer::new(stdout), Some(CaptureBuffer::new(stderr)))
            }
            Capture::Combined(limit) => (CaptureBuffer::new(limit), None),
        };
        Self {
            stdout,
            stderr,
            stream_error: None,
        }
    }
}

async fn drain_output<S>(
    mut output: S,
    options: &ExecCaptureOptions,
) -> Result<Drained, DrainInterrupted>
where
    S: Stream<Item = Result<LogOutput, DockerError>> + Unpin,
{
    let mut drained = Drained::empty(options);
    loop {
        let item = match &options.cancel {
            Some(token) => tokio::select! {
                _ = token.cancelled() => return Err(DrainInterrupted::Cancelled),
                item = output.next() => item,
            },
            None => output.next().await,
        };
        match item {
            None => return Ok(drained),
            Some(Ok(LogOutput::StdOut { message })) => drained.stdout.push(&message),
            Some(Ok(LogOutput::StdErr { message })) => match drained.stderr.as_mut() {
                Some(stderr) => stderr.push(&message),
                None => drained.stdout.push(&message),
            },
            Some(Ok(other)) => {
                if options.other_frames == OtherFrames::ToStdout {
                    drained.stdout.push(other.as_ref());
                }
            }
            Some(Err(error)) => match options.stream_errors {
                StreamErrorPolicy::Fail => return Err(DrainInterrupted::Stream(error)),
                StreamErrorPolicy::Stop => {
                    drained.stream_error = Some(error);
                    return Ok(drained);
                }
                StreamErrorPolicy::Ignore => {}
            },
        }
    }
}

/// One stream under one [`OutputLimit`].
#[derive(Debug)]
enum CaptureBuffer {
    /// `Unbounded` and `Head`: text from the start of the stream, decoded frame
    /// by frame.
    Text {
        text: String,
        limit: Option<usize>,
        truncated: bool,
    },
    /// `Tail`: raw bytes, since the cut can fall anywhere, decoded at the end.
    Tail { ring: RingBuffer, received: usize },
}

impl CaptureBuffer {
    fn new(limit: OutputLimit) -> Self {
        match limit {
            OutputLimit::Unbounded => Self::Text {
                text: String::new(),
                limit: None,
                truncated: false,
            },
            OutputLimit::Head(bytes) => Self::Text {
                text: String::new(),
                limit: Some(bytes),
                truncated: false,
            },
            OutputLimit::Tail(bytes) => Self::Tail {
                ring: RingBuffer::with_capacity(bytes),
                received: 0,
            },
        }
    }

    fn push(&mut self, message: &[u8]) {
        match self {
            Self::Text {
                text,
                limit,
                truncated,
            } => {
                let kept = match limit {
                    Some(limit) => {
                        let available = limit.saturating_sub(text.len());
                        *truncated |= message.len() > available;
                        &message[..message.len().min(available)]
                    }
                    None => message,
                };
                text.push_str(&String::from_utf8_lossy(kept));
            }
            Self::Tail { ring, received } => {
                *received = received.saturating_add(message.len());
                ring.append(message);
            }
        }
    }

    fn truncated(&self) -> bool {
        match self {
            Self::Text { truncated, .. } => *truncated,
            Self::Tail { ring, received } => *received > ring.len(),
        }
    }

    fn into_string(self) -> String {
        match self {
            Self::Text { text, .. } => text,
            Self::Tail { ring, .. } => ring.into_string_lossy(),
        }
    }
}

/// Bounded ring buffer that keeps the tail of an appended byte stream.
///
/// The buffer capacity is set at construction and never changes. When
/// `append` would cause the total size to exceed `capacity`, bytes are
/// discarded from the front (oldest data) to make room for the new chunk.
///
/// # Examples
///
/// ```rust
/// use temps_docker_util::RingBuffer;
///
/// let mut buf = RingBuffer::with_capacity(16);
/// buf.append(b"hello, ");
/// buf.append(b"world");
/// assert_eq!(buf.into_string_lossy(), "hello, world");
/// ```
#[derive(Debug)]
pub struct RingBuffer {
    capacity: usize,
    buf: Vec<u8>,
}

impl RingBuffer {
    /// Create a new `RingBuffer` that keeps at most `capacity` bytes.
    ///
    /// If `capacity` is 0, every `append` is a no-op and `into_string_lossy`
    /// always returns an empty string.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            buf: Vec::with_capacity(capacity.min(64 * 1024)),
        }
    }

    /// Append `chunk` to the buffer.
    ///
    /// If appending `chunk` would push the total length over `capacity`,
    /// bytes are dropped from the front (oldest) until the buffer fits
    /// within `capacity`. If `chunk` itself is larger than `capacity`,
    /// only the trailing `capacity` bytes of `chunk` are kept.
    pub fn append(&mut self, chunk: &[u8]) {
        if self.capacity == 0 {
            return;
        }

        // If the incoming chunk alone exceeds capacity, keep only its tail.
        let chunk = if chunk.len() >= self.capacity {
            &chunk[chunk.len() - self.capacity..]
        } else {
            chunk
        };

        // How many existing bytes need to be evicted to fit `chunk`?
        let combined = self.buf.len() + chunk.len();
        if combined > self.capacity {
            let drop_count = combined - self.capacity;
            self.buf.drain(..drop_count);
        }

        self.buf.extend_from_slice(chunk);
    }

    /// Return the buffered data as a `String`, replacing invalid UTF-8 sequences
    /// with the Unicode replacement character (`U+FFFD`).
    ///
    /// Consumes the `RingBuffer`.
    pub fn into_string_lossy(self) -> String {
        String::from_utf8_lossy(&self.buf).into_owned()
    }

    /// Return the current number of bytes in the buffer.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Return `true` if the buffer contains no bytes.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{docker_or_skip, remove_container, start_sleeping_container};
    use bollard::exec::CreateExecOptions;
    use bytes::Bytes;

    fn stdout(text: &str) -> Result<LogOutput, DockerError> {
        Ok(LogOutput::StdOut {
            message: Bytes::copy_from_slice(text.as_bytes()),
        })
    }

    fn stderr(text: &str) -> Result<LogOutput, DockerError> {
        Ok(LogOutput::StdErr {
            message: Bytes::copy_from_slice(text.as_bytes()),
        })
    }

    fn console(text: &str) -> Result<LogOutput, DockerError> {
        Ok(LogOutput::Console {
            message: Bytes::copy_from_slice(text.as_bytes()),
        })
    }

    fn stream_failure() -> Result<LogOutput, DockerError> {
        Err(DockerError::DockerResponseServerError {
            status_code: 500,
            message: "stream broke".to_string(),
        })
    }

    async fn drain(
        frames: Vec<Result<LogOutput, DockerError>>,
        options: ExecCaptureOptions,
    ) -> Result<Drained, DrainInterrupted> {
        drain_output(futures::stream::iter(frames), &options).await
    }

    fn unbounded() -> ExecCaptureOptions {
        ExecCaptureOptions::separate(OutputLimit::Unbounded, OutputLimit::Unbounded)
    }

    #[tokio::test]
    async fn separate_capture_keeps_the_streams_apart() {
        let drained = drain(vec![stdout("a"), stderr("b"), stdout("c")], unbounded())
            .await
            .expect("drain");
        assert_eq!(drained.stdout.into_string(), "ac");
        assert_eq!(drained.stderr.expect("stderr").into_string(), "b");
    }

    #[tokio::test]
    async fn combined_capture_keeps_arrival_order() {
        let drained = drain(
            vec![stdout("a"), stderr("b"), stdout("c")],
            ExecCaptureOptions::combined(OutputLimit::Unbounded),
        )
        .await
        .expect("drain");
        assert_eq!(drained.stdout.into_string(), "abc");
        assert!(drained.stderr.is_none());
    }

    #[tokio::test]
    async fn head_keeps_the_start_and_reports_the_cut() {
        let drained = drain(
            vec![stdout("01234"), stdout("56789"), stdout("abc")],
            ExecCaptureOptions::separate(OutputLimit::Head(7), OutputLimit::Unbounded),
        )
        .await
        .expect("drain");
        assert!(drained.stdout.truncated());
        assert_eq!(drained.stdout.into_string(), "0123456");
    }

    #[tokio::test]
    async fn head_at_exactly_the_limit_is_not_truncated() {
        let drained = drain(
            vec![stdout("0123"), stdout("456")],
            ExecCaptureOptions::separate(OutputLimit::Head(7), OutputLimit::Unbounded),
        )
        .await
        .expect("drain");
        assert!(!drained.stdout.truncated());
        assert_eq!(drained.stdout.into_string(), "0123456");
    }

    #[tokio::test]
    async fn head_zero_keeps_nothing() {
        let drained = drain(
            vec![stdout("dropped")],
            ExecCaptureOptions::separate(OutputLimit::Head(0), OutputLimit::Unbounded),
        )
        .await
        .expect("drain");
        assert!(drained.stdout.truncated());
        assert_eq!(drained.stdout.into_string(), "");
    }

    #[tokio::test]
    async fn tail_keeps_the_end_and_reports_the_cut() {
        let drained = drain(
            vec![stderr("01234"), stderr("56789"), stderr("abc")],
            ExecCaptureOptions::separate(OutputLimit::Unbounded, OutputLimit::Tail(5)),
        )
        .await
        .expect("drain");
        let stderr = drained.stderr.expect("stderr");
        assert!(stderr.truncated());
        assert_eq!(stderr.into_string(), "89abc");
    }

    #[tokio::test]
    async fn tail_within_the_limit_is_not_truncated() {
        let drained = drain(
            vec![stderr("abc")],
            ExecCaptureOptions::separate(OutputLimit::Unbounded, OutputLimit::Tail(5)),
        )
        .await
        .expect("drain");
        let stderr = drained.stderr.expect("stderr");
        assert!(!stderr.truncated());
        assert_eq!(stderr.into_string(), "abc");
    }

    /// `Tail` decodes once at the end, so a character split across two frames
    /// survives. `Unbounded` and `Head` decode frame by frame, so it does not.
    #[tokio::test]
    async fn a_character_split_across_frames_depends_on_the_limit() {
        let frames = || {
            vec![
                Ok(LogOutput::StdOut {
                    message: Bytes::from_static(&[0xC3]),
                }),
                Ok(LogOutput::StdOut {
                    message: Bytes::from_static(&[0xA9]),
                }),
            ]
        };
        let tail = drain(
            frames(),
            ExecCaptureOptions::separate(OutputLimit::Tail(8), OutputLimit::Unbounded),
        )
        .await
        .expect("drain");
        assert_eq!(tail.stdout.into_string(), "é");

        let text = drain(frames(), unbounded()).await.expect("drain");
        assert_eq!(text.stdout.into_string(), "\u{FFFD}\u{FFFD}");
    }

    #[tokio::test]
    async fn other_frames_are_dropped_unless_asked_for() {
        let drained = drain(vec![stdout("a"), console("b")], unbounded())
            .await
            .expect("drain");
        assert_eq!(drained.stdout.into_string(), "a");

        let drained = drain(
            vec![stdout("a"), console("b")],
            unbounded().other_frames(OtherFrames::ToStdout),
        )
        .await
        .expect("drain");
        assert_eq!(drained.stdout.into_string(), "ab");
    }

    #[tokio::test]
    async fn a_stream_error_fails_by_default() {
        let result = drain(vec![stdout("a"), stream_failure()], unbounded()).await;
        assert!(matches!(result, Err(DrainInterrupted::Stream(_))));
    }

    #[tokio::test]
    async fn stop_returns_what_was_captured_and_the_error() {
        let drained = drain(
            vec![stdout("a"), stream_failure(), stdout("b")],
            unbounded().stream_errors(StreamErrorPolicy::Stop),
        )
        .await
        .expect("drain");
        assert!(drained.stream_error.is_some());
        assert_eq!(drained.stdout.into_string(), "a");
    }

    #[tokio::test]
    async fn ignore_keeps_reading_past_the_error() {
        let drained = drain(
            vec![stdout("a"), stream_failure(), stdout("b")],
            unbounded().stream_errors(StreamErrorPolicy::Ignore),
        )
        .await
        .expect("drain");
        assert!(drained.stream_error.is_none());
        assert_eq!(drained.stdout.into_string(), "ab");
    }

    #[tokio::test]
    async fn cancelling_stops_a_stream_that_never_ends() {
        let token = CancellationToken::new();
        let stream = futures::stream::iter(vec![stdout("a")]).chain(futures::stream::pending());
        let options = unbounded().cancel(&token);
        let canceller = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            canceller.cancel();
        });

        let result = tokio::time::timeout(
            Duration::from_secs(10),
            drain_output(Box::pin(stream), &options),
        )
        .await
        .expect("cancellation must end the wait");
        assert!(matches!(result, Err(DrainInterrupted::Cancelled)));
    }

    fn sh(script: &str) -> CreateExecOptions<String> {
        CreateExecOptions {
            cmd: Some(vec!["sh".to_string(), "-c".to_string(), script.to_string()]),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            ..Default::default()
        }
    }

    /// Output far above both caps: memory stays at the cap, the command still
    /// runs to completion and its exit code is read.
    #[tokio::test]
    async fn output_above_the_cap_is_cut_and_the_command_still_finishes() {
        let Some(docker) = docker_or_skip("output_above_the_cap").await else {
            return;
        };
        let name = start_sleeping_container(&docker, "exec-cap").await;

        let script = "i=0; while [ $i -lt 20000 ]; do echo line-$i; echo err-$i >&2; \
                      i=$((i+1)); done; exit 3";
        let result = exec_capture(
            &docker,
            &name,
            sh(script),
            ExecCaptureOptions::separate(OutputLimit::Head(100), OutputLimit::Tail(100)),
        )
        .await;
        remove_container(&docker, &name).await;

        let output = result.expect("exec");
        assert_eq!(output.exit_code, Some(3));
        assert!(output.stdout_truncated && output.stderr_truncated);
        assert_eq!(output.stdout.len(), 100);
        assert!(output.stdout.starts_with("line-0\n"), "{}", output.stdout);
        assert_eq!(output.stderr.len(), 100);
        assert!(output.stderr.ends_with("err-19999\n"), "{}", output.stderr);
    }

    #[tokio::test]
    async fn stdout_stderr_and_exit_code_come_back() {
        let Some(docker) = docker_or_skip("stdout_stderr_and_exit_code").await else {
            return;
        };
        let name = start_sleeping_container(&docker, "exec-basic").await;

        let result = exec_capture(&docker, &name, sh("echo out; echo err >&2"), unbounded()).await;
        remove_container(&docker, &name).await;

        let output = result.expect("exec");
        assert_eq!(output.exit_code, Some(0));
        assert_eq!(output.stdout, "out\n");
        assert_eq!(output.stderr, "err\n");
        assert!(!output.stdout_truncated && !output.stderr_truncated);
    }

    /// `NotRead` leaves the exit code empty even when the command failed.
    #[tokio::test]
    async fn not_read_leaves_the_exit_code_empty() {
        let Some(docker) = docker_or_skip("not_read_leaves_the_exit_code").await else {
            return;
        };
        let name = start_sleeping_container(&docker, "exec-not-read").await;

        let result = exec_capture(
            &docker,
            &name,
            sh("exit 9"),
            unbounded().exit_code(ExitCodePolicy::NotRead),
        )
        .await;
        remove_container(&docker, &name).await;

        assert_eq!(result.expect("exec").exit_code, None);
    }

    /// A timeout stops the wait and names the container. The command's secret
    /// environment and arguments stay out of the message.
    #[tokio::test]
    async fn a_timeout_is_typed_and_keeps_the_command_out_of_the_message() {
        let Some(docker) = docker_or_skip("a_timeout_is_typed").await else {
            return;
        };
        let name = start_sleeping_container(&docker, "exec-timeout").await;

        let mut config = sh("sleep 30 # s3cret-arg");
        config.env = Some(vec!["TOKEN=s3cret-env".to_string()]);
        let result = exec_capture(
            &docker,
            &name,
            config,
            unbounded().timeout(Duration::from_millis(200)),
        )
        .await;
        remove_container(&docker, &name).await;

        let error = result.expect_err("the command outlasts the timeout");
        assert!(
            matches!(&error, DockerUtilError::ExecTimedOut { container, .. } if container == &name),
            "{error:?}"
        );
        let message = error.to_string();
        assert!(!message.contains("s3cret"), "{message}");
    }

    #[tokio::test]
    async fn a_missing_container_is_a_typed_error_naming_it() {
        let Some(docker) = docker_or_skip("a_missing_container").await else {
            return;
        };
        let name = crate::test_support::unique_name("exec-missing");
        let error = exec_capture(&docker, &name, sh("true"), unbounded())
            .await
            .expect_err("the container does not exist");
        assert!(
            matches!(&error, DockerUtilError::ExecCreate { container, .. } if container == &name),
            "{error:?}"
        );
    }

    /// Empty buffer returns empty string.
    #[test]
    fn test_empty_buffer_returns_empty_string() {
        let buf = RingBuffer::with_capacity(64);
        assert_eq!(buf.into_string_lossy(), "");
    }

    /// A single small chunk is preserved exactly.
    #[test]
    fn test_small_chunk_preserved() {
        let mut buf = RingBuffer::with_capacity(64);
        buf.append(b"hello");
        assert_eq!(buf.into_string_lossy(), "hello");
    }

    /// A chunk larger than capacity keeps only the tail.
    #[test]
    fn test_chunk_larger_than_capacity_keeps_tail() {
        let mut buf = RingBuffer::with_capacity(8);
        buf.append(b"0123456789"); // 10 bytes > capacity 8
        assert_eq!(buf.into_string_lossy(), "23456789"); // last 8 bytes
    }

    /// Multiple appends staying within capacity are all preserved.
    #[test]
    fn test_multiple_appends_within_capacity() {
        let mut buf = RingBuffer::with_capacity(16);
        buf.append(b"hello, ");
        buf.append(b"world");
        assert_eq!(buf.into_string_lossy(), "hello, world");
    }

    /// Multiple appends overflowing capacity keep only the tail.
    #[test]
    fn test_multiple_appends_overflowing_capacity_keeps_tail() {
        let mut buf = RingBuffer::with_capacity(8);
        buf.append(b"abcd"); // 4 bytes — fits
        buf.append(b"efgh"); // 4 bytes — exactly fills capacity
        buf.append(b"ijkl"); // 4 bytes — overflows; oldest 4 must be dropped
                             // After overflow: buf had "abcdefgh" (8), then "ijkl" appended → drop 4
                             // → keeps "efghijkl" (last 8 bytes of all appended data)
        assert_eq!(buf.into_string_lossy(), "efghijkl");
    }

    /// Zero-capacity buffer is always empty.
    #[test]
    fn test_zero_capacity_always_empty() {
        let mut buf = RingBuffer::with_capacity(0);
        buf.append(b"any data");
        // Check is_empty() before consuming the buffer.
        assert!(buf.is_empty());
        assert_eq!(buf.into_string_lossy(), "");
    }

    /// `len()` reports current buffer length correctly.
    #[test]
    fn test_len_tracks_content() {
        let mut buf = RingBuffer::with_capacity(16);
        assert_eq!(buf.len(), 0);
        buf.append(b"abc");
        assert_eq!(buf.len(), 3);
        buf.append(b"def");
        assert_eq!(buf.len(), 6);
    }

    /// `is_empty()` returns true initially and false after data.
    #[test]
    fn test_is_empty() {
        let mut buf = RingBuffer::with_capacity(8);
        assert!(buf.is_empty());
        buf.append(b"x");
        assert!(!buf.is_empty());
    }

    /// Invalid UTF-8 bytes are replaced with the replacement character.
    #[test]
    fn test_invalid_utf8_is_replaced() {
        let mut buf = RingBuffer::with_capacity(16);
        buf.append(&[0xFF, 0xFE]); // invalid UTF-8
        let s = buf.into_string_lossy();
        assert!(s.contains('\u{FFFD}'));
    }
}
