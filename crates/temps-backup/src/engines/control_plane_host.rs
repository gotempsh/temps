// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The control-plane dump without Docker: `pg_dumpall --globals-only` and
//! `pg_dump` run as host processes.
//!
//! ## When this runs
//!
//! Only when the Docker daemon does not answer. A reachable daemon keeps the
//! one-shot sidecar path of [`super::control_plane`] unchanged, so an install
//! that has Docker never runs a host binary. Without Docker (a hosted plane
//! that ships the PostgreSQL client tools in its image) the dump uses the
//! `pg_dump` and `pg_dumpall` on `PATH`, provided their major version is at
//! least the server's; otherwise the backup fails with an error naming the
//! missing tool and the major it needs ([`HostDumpError`]).
//!
//! ## What it produces
//!
//! The same file the sidecar produces: globals, then the single-database
//! plain-SQL dump with the high-volume tables schema-only, gzip-compressed.
//! Restore therefore cannot tell which path wrote a backup.
//!
//! ## Credentials
//!
//! The password reaches the child processes only through `PGPASSWORD` in
//! their environment, never through their arguments, and nothing here logs
//! the connection URL.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::control_plane::ControlPlaneTarget;
use super::dump_capture::WorkdirGuard;
use super::pg_client_tools::{self, HostTool, HostToolProbe};
use crate::execution::engine_v2::BackupError;

pub(crate) const PG_DUMP: &str = "pg_dump";
pub(crate) const PG_DUMPALL: &str = "pg_dumpall";

/// Bound on deciding whether the Docker daemon is reachable. Generous, so a
/// busy but working daemon still takes the sidecar path.
const DOCKER_PING_TIMEOUT: Duration = Duration::from_secs(10);
/// Upper bound on the stderr carried in an error message.
const MAX_STDERR_BYTES: u64 = 4_000;
/// Read/compress chunk size; memory stays constant whatever the dump size.
const GZIP_CHUNK_BYTES: usize = 256 * 1024;

/// Why the control-plane dump could not run (or finish) without Docker.
#[derive(Debug, thiserror::Error)]
pub enum HostDumpError {
    #[error(
        "Control-plane backup cannot run: the Docker daemon is not reachable ({docker}) and \
         there is no `{tool}` on this host's PATH. Install the PostgreSQL {required_major} client \
         tools ({tool} {required_major} or newer) on the Temps host, or make the Docker daemon \
         reachable so a postgres:{required_major} container can run the dump."
    )]
    ToolMissing {
        tool: &'static str,
        required_major: u32,
        docker: String,
    },

    #[error(
        "Control-plane backup cannot run: the Docker daemon is not reachable ({docker}) and \
         `{tool}` at {binary} is PostgreSQL {found_major}, older than the control-plane server \
         (PostgreSQL {required_major}). Install the PostgreSQL {required_major} client tools \
         ({tool} {required_major} or newer) on the Temps host and put them first on PATH, or \
         make the Docker daemon reachable so a postgres:{required_major} container can run the \
         dump."
    )]
    ToolTooOld {
        tool: &'static str,
        binary: PathBuf,
        found_major: u32,
        required_major: u32,
        docker: String,
    },

    #[error(
        "Control-plane backup cannot run: the Docker daemon is not reachable ({docker}) and \
         `{tool}` at {binary} is not usable: {reason}. Install the PostgreSQL {required_major} \
         client tools ({tool} {required_major} or newer) on the Temps host, or make the Docker \
         daemon reachable so a postgres:{required_major} container can run the dump."
    )]
    ToolUnusable {
        tool: &'static str,
        binary: PathBuf,
        reason: String,
        required_major: u32,
        docker: String,
    },

    #[error("Could not {operation} '{path}' for the control-plane host dump: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Could not start `{tool}` at {binary} for the control-plane backup: {source}")]
    Launch {
        tool: &'static str,
        binary: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "`{tool}` at {binary} exited with code {exit_code} while dumping the control-plane \
         database: {stderr}"
    )]
    Exit {
        tool: &'static str,
        binary: PathBuf,
        exit_code: i32,
        stderr: String,
    },

    #[error(
        "`pg_dumpall` and `pg_dump` exited successfully but wrote nothing to '{path}'; \
         refusing to upload an empty control-plane dump"
    )]
    EmptyDump { path: PathBuf },

    #[error("Control-plane host dump cancelled")]
    Cancelled,
}

impl From<HostDumpError> for BackupError {
    fn from(error: HostDumpError) -> Self {
        match error {
            HostDumpError::Cancelled => BackupError::Cancelled,
            // Retryable on purpose, tool errors included: on an install that
            // has Docker they only appear while the daemon is down, and a
            // daemon that comes back must get the retries it always had.
            HostDumpError::ToolMissing { .. }
            | HostDumpError::ToolTooOld { .. }
            | HostDumpError::ToolUnusable { .. }
            | HostDumpError::Io { .. }
            | HostDumpError::Launch { .. }
            | HostDumpError::Exit { .. }
            | HostDumpError::EmptyDump { .. } => BackupError::Failed {
                reason: error.to_string(),
            },
        }
    }
}

// ── Selection ────────────────────────────────────────────────────────────────

/// How the control-plane dump runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DumpRunner {
    /// The one-shot `postgres:<major>` sidecar (the default).
    Docker,
    /// Host client tools, used only when Docker is unreachable.
    Host {
        pg_dump: HostTool,
        pg_dumpall: HostTool,
    },
}

/// What [`select_dump_runner`] needs to know about this host, behind a trait
/// so the selection is tested without a Docker daemon or installed tools.
#[async_trait::async_trait]
pub(crate) trait DumpToolProbe: Send + Sync {
    /// `Ok(())` when the Docker daemon answers, otherwise why not.
    async fn docker(&self) -> Result<(), String>;
    /// Look up one PostgreSQL client tool on the host.
    async fn host_tool(&self, program: &'static str) -> HostToolProbe;
}

/// The local Docker daemon and this process's `PATH`.
pub(crate) struct LocalDumpToolProbe;

#[async_trait::async_trait]
impl DumpToolProbe for LocalDumpToolProbe {
    async fn docker(&self) -> Result<(), String> {
        pg_client_tools::probe_docker(DOCKER_PING_TIMEOUT).await
    }

    async fn host_tool(&self, program: &'static str) -> HostToolProbe {
        pg_client_tools::probe_host_tool(program).await
    }
}

/// Docker when its daemon answers; otherwise host `pg_dump` and
/// `pg_dumpall` of at least `server_major`; otherwise an error naming the
/// tool that is missing or too old. The host is not probed at all while
/// Docker answers.
pub(crate) async fn select_dump_runner(
    probe: &dyn DumpToolProbe,
    server_major: u32,
) -> Result<DumpRunner, HostDumpError> {
    let docker = match probe.docker().await {
        Ok(()) => return Ok(DumpRunner::Docker),
        Err(reason) => reason,
    };
    let pg_dump = require_host_tool(probe, PG_DUMP, server_major, &docker).await?;
    let pg_dumpall = require_host_tool(probe, PG_DUMPALL, server_major, &docker).await?;
    Ok(DumpRunner::Host {
        pg_dump,
        pg_dumpall,
    })
}

async fn require_host_tool(
    probe: &dyn DumpToolProbe,
    tool: &'static str,
    required_major: u32,
    docker: &str,
) -> Result<HostTool, HostDumpError> {
    match probe.host_tool(tool).await {
        HostToolProbe::Found(found) if found.major >= required_major => Ok(found),
        HostToolProbe::Found(HostTool { binary, major }) => Err(HostDumpError::ToolTooOld {
            tool,
            binary,
            found_major: major,
            required_major,
            docker: docker.to_string(),
        }),
        HostToolProbe::Unusable { binary, reason } => Err(HostDumpError::ToolUnusable {
            tool,
            binary,
            reason,
            required_major,
            docker: docker.to_string(),
        }),
        HostToolProbe::Missing => Err(HostDumpError::ToolMissing {
            tool,
            required_major,
            docker: docker.to_string(),
        }),
    }
}

// ── Running ──────────────────────────────────────────────────────────────────

/// One host dump: the tools, what to dump, and where to write.
pub(crate) struct HostDumpRequest<'a> {
    pub pg_dump: &'a HostTool,
    pub pg_dumpall: &'a HostTool,
    pub target: &'a ControlPlaneTarget,
    pub exclude_patterns: &'a [String],
    /// Uncompressed SQL, removed once compressed. Must not exist yet.
    pub sql_path: PathBuf,
    /// The gzip-compressed dump. Must not exist yet.
    pub gz_path: PathBuf,
    /// Both tools' stderr, read back into the error when one fails.
    pub stderr_path: PathBuf,
    /// Keeps the attempt directory holding these paths alive until the
    /// blocking compression is done with it (see [`gzip_file`]).
    pub workdir: WorkdirGuard,
}

/// `pg_dumpall --globals-only` arguments, the same as the sidecar's.
pub(crate) fn pg_dumpall_args(target: &ControlPlaneTarget) -> Vec<String> {
    vec![
        "--globals-only".to_string(),
        "--clean".to_string(),
        "--if-exists".to_string(),
        "--no-password".to_string(),
        format!("--host={}", target.host),
        format!("--port={}", target.port),
        format!("--username={}", target.username),
        format!("--database={}", target.database),
    ]
}

/// `pg_dump` arguments, the same as the sidecar's.
pub(crate) fn pg_dump_args(
    target: &ControlPlaneTarget,
    exclude_patterns: &[String],
) -> Vec<String> {
    let mut args = vec![
        "--clean".to_string(),
        "--if-exists".to_string(),
        "--no-password".to_string(),
        format!("--host={}", target.host),
        format!("--port={}", target.port),
        format!("--username={}", target.username),
    ];
    args.extend(
        exclude_patterns
            .iter()
            .map(|pattern| format!("--exclude-table-data={pattern}")),
    );
    args.push(format!("--dbname={}", target.database));
    args
}

/// Run `pg_dumpall --globals-only` then `pg_dump` into `sql_path`, gzip it
/// into `gz_path` and return the compressed size.
///
/// Both children are killed, and the compression stops, when `cancel` fires
/// or this future is dropped (the executor's wall-clock timeout drops it
/// without firing `cancel`).
pub(crate) async fn run_host_dump(
    request: HostDumpRequest<'_>,
    cancel: &CancellationToken,
) -> Result<u64, HostDumpError> {
    let outputs = ToolOutputs {
        sql: create_private_file(&request.sql_path)?,
        sql_path: &request.sql_path,
        stderr: create_private_file(&request.stderr_path)?,
        stderr_path: &request.stderr_path,
    };
    let password = request.target.password.as_str();
    run_tool(
        PG_DUMPALL,
        request.pg_dumpall,
        pg_dumpall_args(request.target),
        password,
        &outputs,
        cancel,
    )
    .await?;
    run_tool(
        PG_DUMP,
        request.pg_dump,
        pg_dump_args(request.target, request.exclude_patterns),
        password,
        &outputs,
        cancel,
    )
    .await?;
    drop(outputs);

    // Checked on the SQL, not the gzip file: gzip writes a header and a
    // trailer even for empty input, so a compressed empty dump is not empty.
    let sql_len = tokio::fs::metadata(&request.sql_path)
        .await
        .map_err(|source| HostDumpError::Io {
            operation: "stat",
            path: request.sql_path.clone(),
            source,
        })?
        .len();
    if sql_len == 0 {
        return Err(HostDumpError::EmptyDump {
            path: request.sql_path,
        });
    }

    let size = gzip_file(&request.sql_path, &request.gz_path, cancel, request.workdir).await?;
    if let Err(error) = tokio::fs::remove_file(&request.sql_path).await {
        // The attempt directory is deleted when the attempt drops.
        debug!(
            path = %request.sql_path.display(),
            "Could not remove the uncompressed control-plane dump early: {error}"
        );
    }
    Ok(size)
}

/// Create `path` for writing, owner-only (the dump holds password hashes and
/// encrypted secrets), appending so the second tool continues the file.
fn create_private_file(path: &Path) -> Result<File, HostDumpError> {
    let mut options = std::fs::OpenOptions::new();
    options.append(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(|source| HostDumpError::Io {
        operation: "create",
        path: path.to_path_buf(),
        source,
    })
}

/// The files both tools write into, shared so the second appends.
struct ToolOutputs<'a> {
    sql: File,
    sql_path: &'a Path,
    stderr: File,
    stderr_path: &'a Path,
}

impl ToolOutputs<'_> {
    fn stdio(file: &File, path: &Path) -> Result<Stdio, HostDumpError> {
        file.try_clone()
            .map(Stdio::from)
            .map_err(|source| HostDumpError::Io {
                operation: "hand to the dump process",
                path: path.to_path_buf(),
                source,
            })
    }
}

async fn run_tool(
    tool: &'static str,
    host_tool: &HostTool,
    args: Vec<String>,
    password: &str,
    outputs: &ToolOutputs<'_>,
    cancel: &CancellationToken,
) -> Result<(), HostDumpError> {
    let mut command = tokio::process::Command::new(&host_tool.binary);
    command
        .args(args)
        // The sidecar's environment holds only this; libpq treats an empty
        // PGPASSWORD as unset, like the sidecar.
        .env("PGPASSWORD", password)
        .stdin(Stdio::null())
        .stdout(ToolOutputs::stdio(&outputs.sql, outputs.sql_path)?)
        .stderr(ToolOutputs::stdio(&outputs.stderr, outputs.stderr_path)?)
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|source| HostDumpError::Launch {
        tool,
        binary: host_tool.binary.clone(),
        source,
    })?;

    let status = tokio::select! {
        status = child.wait() => status.map_err(|source| HostDumpError::Launch {
            tool,
            binary: host_tool.binary.clone(),
            source,
        })?,
        () = cancel.cancelled() => {
            if let Err(error) = child.kill().await {
                debug!(tool, "Could not kill the cancelled control-plane dump process: {error}");
            }
            return Err(HostDumpError::Cancelled);
        }
    };
    if status.success() {
        return Ok(());
    }
    Err(HostDumpError::Exit {
        tool,
        binary: host_tool.binary.clone(),
        exit_code: status.code().unwrap_or(-1),
        stderr: read_tail(outputs.stderr_path).await,
    })
}

/// The last [`MAX_STDERR_BYTES`] of a diagnostics file.
async fn read_tail(path: &Path) -> String {
    let path = path.to_path_buf();
    let read = tokio::task::spawn_blocking(move || -> std::io::Result<String> {
        use std::io::{Seek, SeekFrom};
        let mut file = File::open(&path)?;
        let len = file.metadata()?.len();
        let start = len.saturating_sub(MAX_STDERR_BYTES);
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let text = String::from_utf8_lossy(&bytes).trim().to_string();
        Ok(if start > 0 {
            format!("...{text}")
        } else {
            text
        })
    })
    .await;
    match read {
        Ok(Ok(text)) if !text.is_empty() => text,
        Ok(Ok(_)) => "(no stderr output)".to_string(),
        Ok(Err(error)) => format!("(stderr unreadable: {error})"),
        Err(error) => format!("(stderr unreadable: {error})"),
    }
}

/// gzip `source` into a new owner-only `target` in a blocking task, in
/// constant memory.
///
/// Dropping a `spawn_blocking` handle does not stop the task, so the task
/// checks an attempt-local token between chunks that fires when `cancel`
/// does *or* when this future is dropped (a wall-clock timeout drops the
/// backup future without firing `cancel`). It stops within one chunk
/// either way. The task owns `workdir` until it returns, so the attempt
/// directory is never deleted underneath its open files.
async fn gzip_file(
    source: &Path,
    target: &Path,
    cancel: &CancellationToken,
    workdir: WorkdirGuard,
) -> Result<u64, HostDumpError> {
    let source = source.to_path_buf();
    let target = target.to_path_buf();
    let task_cancel = cancel.child_token();
    let _stop_task_on_drop = task_cancel.clone().drop_guard();
    let join_target = target.clone();
    tokio::task::spawn_blocking(move || {
        let _workdir = workdir;
        let cancel = task_cancel;
        let io = |operation: &'static str, path: &Path| {
            let path = path.to_path_buf();
            move |source| HostDumpError::Io {
                operation,
                path,
                source,
            }
        };
        let mut input = File::open(&source).map_err(io("read", &source))?;
        let output = create_private_file(&target)?;
        let mut encoder = flate2::write::GzEncoder::new(output, flate2::Compression::default());
        let mut buffer = vec![0u8; GZIP_CHUNK_BYTES];
        loop {
            if cancel.is_cancelled() {
                return Err(HostDumpError::Cancelled);
            }
            let read = input.read(&mut buffer).map_err(io("read", &source))?;
            if read == 0 {
                break;
            }
            encoder
                .write_all(&buffer[..read])
                .map_err(io("compress into", &target))?;
        }
        let output = encoder.finish().map_err(io("compress into", &target))?;
        output.sync_all().map_err(io("flush", &target))?;
        let size = output.metadata().map_err(io("stat", &target))?.len();
        Ok(size)
    })
    .await
    .map_err(|error| HostDumpError::Io {
        operation: "compress into",
        path: join_target,
        source: std::io::Error::other(error),
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::dump_capture::DumpAttempt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ── Selection ────────────────────────────────────────────────────────

    struct FakeProbe {
        docker: Result<(), String>,
        pg_dump: HostToolProbe,
        pg_dumpall: HostToolProbe,
        host_calls: AtomicUsize,
    }

    impl FakeProbe {
        fn new(
            docker: Result<(), String>,
            pg_dump: HostToolProbe,
            pg_dumpall: HostToolProbe,
        ) -> Self {
            Self {
                docker,
                pg_dump,
                pg_dumpall,
                host_calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl DumpToolProbe for FakeProbe {
        async fn docker(&self) -> Result<(), String> {
            self.docker.clone()
        }

        async fn host_tool(&self, program: &'static str) -> HostToolProbe {
            self.host_calls.fetch_add(1, Ordering::SeqCst);
            match program {
                PG_DUMP => self.pg_dump.clone(),
                PG_DUMPALL => self.pg_dumpall.clone(),
                other => panic!("unexpected tool {other}"),
            }
        }
    }

    fn found(program: &str, major: u32) -> HostToolProbe {
        HostToolProbe::Found(tool(program, major))
    }

    fn tool(program: &str, major: u32) -> HostTool {
        HostTool {
            binary: PathBuf::from(format!("/usr/lib/postgresql/{major}/bin/{program}")),
            major,
        }
    }

    const NO_DOCKER: &str = "cannot connect to the Docker daemon: socket missing";

    #[tokio::test]
    async fn a_reachable_docker_keeps_the_sidecar_and_never_probes_the_host() {
        let probe = FakeProbe::new(Ok(()), found(PG_DUMP, 18), found(PG_DUMPALL, 18));
        assert_eq!(
            select_dump_runner(&probe, 18).await.expect("docker"),
            DumpRunner::Docker
        );
        assert_eq!(probe.host_calls.load(Ordering::SeqCst), 0);

        // Even with no host tools at all.
        let bare = FakeProbe::new(Ok(()), HostToolProbe::Missing, HostToolProbe::Missing);
        assert_eq!(
            select_dump_runner(&bare, 18).await.expect("docker"),
            DumpRunner::Docker
        );
    }

    #[tokio::test]
    async fn without_docker_suitable_host_tools_are_used() {
        let probe = FakeProbe::new(
            Err(NO_DOCKER.to_string()),
            found(PG_DUMP, 18),
            found(PG_DUMPALL, 19),
        );
        assert_eq!(
            select_dump_runner(&probe, 18).await.expect("host tools"),
            DumpRunner::Host {
                pg_dump: tool(PG_DUMP, 18),
                pg_dumpall: tool(PG_DUMPALL, 19),
            },
            "an equal or newer major can dump the server"
        );
    }

    #[tokio::test]
    async fn without_docker_a_missing_pg_dump_names_the_tool_and_major() {
        let probe = FakeProbe::new(
            Err(NO_DOCKER.to_string()),
            HostToolProbe::Missing,
            found(PG_DUMPALL, 18),
        );
        let error = select_dump_runner(&probe, 18).await.expect_err("missing");
        assert!(
            matches!(
                &error,
                HostDumpError::ToolMissing { tool: PG_DUMP, required_major: 18, docker }
                    if docker == NO_DOCKER
            ),
            "{error:?}"
        );
        let message = error.to_string();
        assert!(
            message.contains("no `pg_dump` on this host's PATH"),
            "{message}"
        );
        assert!(message.contains("pg_dump 18 or newer"), "{message}");
        assert!(message.contains(NO_DOCKER), "{message}");
    }

    #[tokio::test]
    async fn without_docker_a_missing_pg_dumpall_names_the_tool() {
        let probe = FakeProbe::new(
            Err(NO_DOCKER.to_string()),
            found(PG_DUMP, 18),
            HostToolProbe::Missing,
        );
        let error = select_dump_runner(&probe, 17).await.expect_err("missing");
        assert!(
            matches!(
                &error,
                HostDumpError::ToolMissing {
                    tool: PG_DUMPALL,
                    required_major: 17,
                    ..
                }
            ),
            "{error:?}"
        );
        assert!(error.to_string().contains("pg_dumpall 17 or newer"));
    }

    #[tokio::test]
    async fn without_docker_a_too_old_tool_is_refused_with_both_majors() {
        let probe = FakeProbe::new(
            Err(NO_DOCKER.to_string()),
            found(PG_DUMP, 16),
            found(PG_DUMPALL, 18),
        );
        let error = select_dump_runner(&probe, 18).await.expect_err("too old");
        assert!(
            matches!(
                &error,
                HostDumpError::ToolTooOld {
                    tool: PG_DUMP,
                    found_major: 16,
                    required_major: 18,
                    ..
                }
            ),
            "{error:?}"
        );
        let message = error.to_string();
        assert!(
            message.contains("/usr/lib/postgresql/16/bin/pg_dump is PostgreSQL 16"),
            "{message}"
        );
        assert!(message.contains("pg_dump 18 or newer"), "{message}");
    }

    #[tokio::test]
    async fn without_docker_an_unusable_tool_is_reported_with_its_reason() {
        let probe = FakeProbe::new(
            Err(NO_DOCKER.to_string()),
            HostToolProbe::Unusable {
                binary: PathBuf::from("/opt/bin/pg_dump"),
                reason: "--version exited with 127".to_string(),
            },
            found(PG_DUMPALL, 18),
        );
        let error = select_dump_runner(&probe, 18).await.expect_err("unusable");
        let message = error.to_string();
        assert!(matches!(error, HostDumpError::ToolUnusable { .. }));
        assert!(message.contains("/opt/bin/pg_dump"), "{message}");
        assert!(message.contains("--version exited with 127"), "{message}");
    }

    #[test]
    fn host_dump_errors_stay_retryable_except_cancellation() {
        // A self-hosted install whose Docker daemon is briefly down must keep
        // the retries it had before the host fallback existed.
        let missing = HostDumpError::ToolMissing {
            tool: PG_DUMP,
            required_major: 18,
            docker: NO_DOCKER.to_string(),
        };
        assert!(matches!(
            BackupError::from(missing),
            BackupError::Failed { reason } if reason.contains("pg_dump 18 or newer")
        ));
        let exit = HostDumpError::Exit {
            tool: PG_DUMP,
            binary: PathBuf::from("/usr/bin/pg_dump"),
            exit_code: 1,
            stderr: "connection refused".to_string(),
        };
        assert!(matches!(
            BackupError::from(exit),
            BackupError::Failed { .. }
        ));
        assert!(matches!(
            BackupError::from(HostDumpError::Cancelled),
            BackupError::Cancelled
        ));
    }

    // ── Arguments ────────────────────────────────────────────────────────

    fn target(password: &str) -> ControlPlaneTarget {
        ControlPlaneTarget {
            host: "127.0.0.1".to_string(),
            port: 5432,
            database: "temps".to_string(),
            username: "postgres".to_string(),
            password: password.to_string(),
        }
    }

    #[test]
    fn host_arguments_match_the_sidecar_and_never_carry_the_password() {
        let target = target("hunter2-secret");
        let dumpall = pg_dumpall_args(&target);
        assert_eq!(
            dumpall,
            vec![
                "--globals-only",
                "--clean",
                "--if-exists",
                "--no-password",
                "--host=127.0.0.1",
                "--port=5432",
                "--username=postgres",
                "--database=temps",
            ]
        );
        let dump = pg_dump_args(
            &target,
            &[
                "public.proxy_logs".to_string(),
                "_timescaledb_internal._hyper_3_*".to_string(),
            ],
        );
        assert_eq!(
            dump,
            vec![
                "--clean",
                "--if-exists",
                "--no-password",
                "--host=127.0.0.1",
                "--port=5432",
                "--username=postgres",
                "--exclude-table-data=public.proxy_logs",
                "--exclude-table-data=_timescaledb_internal._hyper_3_*",
                "--dbname=temps",
            ]
        );
        assert!(dumpall
            .iter()
            .chain(dump.iter())
            .all(|arg| !arg.contains("hunter2")));
    }

    // ── Compression ──────────────────────────────────────────────────────

    fn attempt(uuid: &str) -> (tempfile::TempDir, DumpAttempt) {
        let parent = tempfile::tempdir().expect("attempt parent");
        let attempt = DumpAttempt::new_in(parent.path(), "Control-plane PostgreSQL", "test", uuid)
            .expect("attempt");
        (parent, attempt)
    }

    /// `len` bytes that do not compress, so gzip has to work for them.
    fn write_incompressible(path: &Path, len: usize) {
        let mut file = std::io::BufWriter::new(File::create(path).expect("create source"));
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut chunk = vec![0u8; 1024 * 1024];
        let mut written = 0;
        while written < len {
            for word in chunk.as_chunks_mut::<8>().0 {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *word = state.to_le_bytes();
            }
            let take = chunk.len().min(len - written);
            file.write_all(&chunk[..take]).expect("write source");
            written += take;
        }
        file.flush().expect("flush source");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_timeout_during_compression_stops_the_blocking_task() {
        const SOURCE_BYTES: usize = 96 * 1024 * 1024;
        let (_parent, attempt) = attempt("gzip-timeout");
        let source = attempt.host_path("backup.sql");
        let target = attempt.host_path("backup.sql.gz");
        write_incompressible(&source, SOURCE_BYTES);
        let cancel = CancellationToken::new();

        // What the executor's wall-clock timeout does: drop the future
        // without firing the backup's token.
        let outcome = tokio::time::timeout(
            Duration::from_millis(50),
            gzip_file(&source, &target, &cancel, attempt.workdir_guard()),
        )
        .await;
        assert!(outcome.is_err(), "compressing 96 MiB must outlast 50ms");
        assert!(!cancel.is_cancelled());

        // The blocking task notices within one chunk and lets go of the
        // directory; it does not compress the remaining ~96 MiB first.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while attempt.workdir_shared() {
            assert!(
                std::time::Instant::now() < deadline,
                "the compression kept running after its future was dropped"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let written = std::fs::metadata(&target).expect("partial gz").len();
        assert!(
            written < (SOURCE_BYTES / 2) as u64,
            "compression must stop early, but wrote {written} of ~{SOURCE_BYTES} bytes"
        );
    }

    // ── Running, against stand-in client tools ───────────────────────────

    #[cfg(unix)]
    mod fake_tools {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        const PASSWORD: &str = "s3cr3t-pw";

        /// A shell script standing in for a client tool. It fails unless the
        /// password arrived in `PGPASSWORD` and nowhere in its arguments,
        /// then prints `output` and its arguments.
        fn script(dir: &Path, name: &str, output: &str) -> HostTool {
            let body = format!(
                "#!/bin/sh\n\
                 [ \"$PGPASSWORD\" = '{PASSWORD}' ] || {{ echo 'PGPASSWORD not set' >&2; exit 2; }}\n\
                 case \"$*\" in *'{PASSWORD}'*) echo 'password on argv' >&2; exit 3;; esac\n\
                 echo '{output}'\n\
                 echo \"-- args: $*\"\n"
            );
            write_tool(dir, name, &body)
        }

        fn write_tool(dir: &Path, name: &str, body: &str) -> HostTool {
            let binary = dir.join(name);
            std::fs::write(&binary, body).expect("write tool");
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))
                .expect("chmod tool");
            HostTool { binary, major: 18 }
        }

        fn request<'a>(
            work: &DumpAttempt,
            pg_dump: &'a HostTool,
            pg_dumpall: &'a HostTool,
            target: &'a ControlPlaneTarget,
            excludes: &'a [String],
        ) -> HostDumpRequest<'a> {
            HostDumpRequest {
                pg_dump,
                pg_dumpall,
                target,
                exclude_patterns: excludes,
                sql_path: work.host_path("backup.sql"),
                gz_path: work.host_path("backup.sql.gz"),
                stderr_path: work.host_path("pg_dump.stderr"),
                workdir: work.workdir_guard(),
            }
        }

        fn gunzip(path: &Path) -> String {
            let mut sql = String::new();
            flate2::read::GzDecoder::new(File::open(path).expect("open gz"))
                .read_to_string(&mut sql)
                .expect("gunzip");
            sql
        }

        #[tokio::test]
        async fn globals_then_database_are_gzipped_with_the_password_only_in_the_env() {
            let bin = tempfile::tempdir().expect("bin");
            let (_parent, work) = attempt("fake");
            let pg_dumpall = script(bin.path(), PG_DUMPALL, "-- GLOBALS");
            let pg_dump = script(bin.path(), PG_DUMP, "-- DATABASE");
            let target = target(PASSWORD);
            let excludes = vec!["public.proxy_logs".to_string()];
            let req = request(&work, &pg_dump, &pg_dumpall, &target, &excludes);
            let gz_path = req.gz_path.clone();
            let sql_path = req.sql_path.clone();

            let size = run_host_dump(req, &CancellationToken::new())
                .await
                .expect("host dump");

            assert_eq!(std::fs::metadata(&gz_path).expect("gz").len(), size);
            assert!(!sql_path.exists(), "the uncompressed SQL is removed");
            let mode = std::fs::metadata(&gz_path)
                .expect("gz")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "the dump is owner-only");

            let sql = gunzip(&gz_path);
            let globals = sql.find("-- GLOBALS").expect("globals");
            let database = sql.find("-- DATABASE").expect("database");
            assert!(globals < database, "globals come first: {sql}");
            assert!(sql.contains("--globals-only"), "{sql}");
            assert!(
                sql.contains("--exclude-table-data=public.proxy_logs --dbname=temps"),
                "{sql}"
            );
            assert!(!sql.contains(PASSWORD), "{sql}");
        }

        #[tokio::test]
        async fn a_failing_tool_reports_its_exit_code_and_stderr() {
            let bin = tempfile::tempdir().expect("bin");
            let (_parent, work) = attempt("fake");
            let pg_dumpall = script(bin.path(), PG_DUMPALL, "-- GLOBALS");
            let pg_dump = write_tool(
                bin.path(),
                PG_DUMP,
                "#!/bin/sh\necho 'pg_dump: error: connection to server failed' >&2\nexit 1\n",
            );
            let target = target(PASSWORD);
            let req = request(&work, &pg_dump, &pg_dumpall, &target, &[]);
            let gz_path = req.gz_path.clone();

            let error = run_host_dump(req, &CancellationToken::new())
                .await
                .expect_err("pg_dump fails");

            match &error {
                HostDumpError::Exit {
                    tool,
                    binary,
                    exit_code,
                    stderr,
                } => {
                    assert_eq!(*tool, PG_DUMP);
                    assert_eq!(binary, &pg_dump.binary);
                    assert_eq!(*exit_code, 1);
                    assert!(stderr.contains("connection to server failed"), "{stderr}");
                }
                other => panic!("expected an exit failure, got {other:?}"),
            }
            assert!(!gz_path.exists(), "no dump after a failure");
        }

        #[tokio::test]
        async fn a_tool_that_cannot_start_is_a_launch_error() {
            let (_parent, work) = attempt("fake");
            let gone = HostTool {
                binary: work.host_path("no-such-pg_dumpall"),
                major: 18,
            };
            let target = target(PASSWORD);
            let error = run_host_dump(
                request(&work, &gone, &gone, &target, &[]),
                &CancellationToken::new(),
            )
            .await
            .expect_err("missing binary");
            assert!(
                matches!(
                    &error,
                    HostDumpError::Launch {
                        tool: PG_DUMPALL,
                        ..
                    }
                ),
                "{error:?}"
            );
            assert!(error.to_string().contains("no-such-pg_dumpall"), "{error}");
        }

        #[tokio::test]
        async fn tools_that_print_nothing_fail_as_an_empty_dump_before_compressing() {
            let bin = tempfile::tempdir().expect("bin");
            let (_parent, work) = attempt("fake");
            let silent_dumpall = write_tool(bin.path(), PG_DUMPALL, "#!/bin/sh\nexit 0\n");
            let silent_dump = write_tool(bin.path(), PG_DUMP, "#!/bin/sh\nexit 0\n");
            let target = target(PASSWORD);
            let req = request(&work, &silent_dump, &silent_dumpall, &target, &[]);
            let (sql_path, gz_path) = (req.sql_path.clone(), req.gz_path.clone());

            let error = run_host_dump(req, &CancellationToken::new())
                .await
                .expect_err("an empty dump must not pass");

            assert!(
                matches!(&error, HostDumpError::EmptyDump { path } if *path == sql_path),
                "{error:?}"
            );
            assert!(error.to_string().contains("wrote nothing"), "{error}");
            assert!(
                !gz_path.exists(),
                "a gzip of empty input has a header and trailer, so it must never be written"
            );
        }

        #[tokio::test]
        async fn cancellation_kills_a_running_tool() {
            let bin = tempfile::tempdir().expect("bin");
            let (_parent, work) = attempt("fake");
            let hang = write_tool(bin.path(), PG_DUMPALL, "#!/bin/sh\nexec sleep 30\n");
            let target = target(PASSWORD);
            let cancel = CancellationToken::new();
            let trigger = cancel.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(200)).await;
                trigger.cancel();
            });

            let started = std::time::Instant::now();
            let error = run_host_dump(request(&work, &hang, &hang, &target, &[]), &cancel)
                .await
                .expect_err("cancelled");
            assert!(matches!(error, HostDumpError::Cancelled), "{error:?}");
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "cancellation must not wait for the tool to finish"
            );
        }
    }
}
