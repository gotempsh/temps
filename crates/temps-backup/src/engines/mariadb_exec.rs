// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared docker-exec helpers for the MariaDB backup engines
//! (`mariadb_physical`, `mariadb_dump`).
//!
//! These are **standalone free functions** living in `temps-backup` — they do
//! NOT call into `temps-providers::MariaDbService`. `temps-backup` already
//! depends on `temps-providers`, so reaching back the other way would be a
//! circular dependency. The engines therefore own their own `docker exec`
//! plumbing, mirroring how `dispatch::container_has_walg` and
//! `postgres_walg::run_walg_exec` keep WAL-G's docker access inside this crate.
//!
//! ## Credential safety (see upstream PR #149)
//!
//! Passing a DB password as a CLI argument leaks it via `ps`/`pgrep -af` /
//! `/proc/<pid>/cmdline`. Every helper here takes credentials through the
//! exec `env` field (`MYSQL_PWD`/`MARIADB_PWD`) and NEVER interpolates them
//! into the `sh -c` command string. `mariadb-backup`, `mariadb-dump`, and the
//! `mariadb` client all read `MYSQL_PWD` from the environment, so `-uroot`
//! with no `-p` flag is sufficient. Tests pin this invariant.

use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, StartExecResults};
use futures::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use temps_backup_core::engine_v2::BackupError;

/// Cap on captured stderr. `mariadb-backup` is chatty; we only need the tail
/// (which carries the binlog-position line) plus enough context for errors.
const STDERR_CAP: usize = 256 * 1024;

/// Result of a streamed exec: the process exit code and its captured stderr.
/// stdout is streamed to a file and is not held in memory.
pub struct StreamedExec {
    pub exit_code: i64,
    pub stderr: String,
}

/// Run `sh -c <cmd>` inside `container_name`, streaming **stdout** to
/// `out_path` (raw bytes, as produced — the caller is responsible for any
/// in-container `| gzip`) and capturing **stderr** into a bounded string.
///
/// `env` entries are passed via the exec environment (where credentials
/// belong). `cmd` must never contain secrets. Bails early with
/// `BackupError::Cancelled` if `cancel` fires mid-stream.
pub async fn exec_stream_stdout_to_file(
    docker: &bollard::Docker,
    container_name: &str,
    cmd: &str,
    env: &[String],
    out_path: &std::path::Path,
    cancel: &CancellationToken,
) -> Result<StreamedExec, BackupError> {
    let env_refs: Vec<&str> = env.iter().map(|s| s.as_str()).collect();
    let exec = docker
        .create_exec(
            container_name,
            CreateExecOptions {
                cmd: Some(vec!["sh", "-c", cmd]),
                env: Some(env_refs),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| BackupError::Failed {
            reason: format!("create exec on {}: {}", container_name, e),
        })?;

    let file = tokio::fs::File::create(out_path)
        .await
        .map_err(|e| BackupError::Failed {
            reason: format!("create backup file {}: {}", out_path.display(), e),
        })?;
    let mut writer = tokio::io::BufWriter::new(file);
    let mut stderr = String::new();

    let stream = docker
        .start_exec(&exec.id, None)
        .await
        .map_err(|e| BackupError::Failed {
            reason: format!("start exec on {}: {}", container_name, e),
        })?;

    if let StartExecResults::Attached { mut output, .. } = stream {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    return Err(BackupError::Cancelled);
                }
                item = output.next() => {
                    match item {
                        Some(Ok(LogOutput::StdOut { message })) => {
                            writer.write_all(&message).await.map_err(|e| BackupError::Failed {
                                reason: format!("write backup stream to {}: {}", out_path.display(), e),
                            })?;
                        }
                        Some(Ok(LogOutput::StdErr { message })) => {
                            if stderr.len() < STDERR_CAP {
                                stderr.push_str(&String::from_utf8_lossy(&message));
                            }
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => {
                            return Err(BackupError::Failed {
                                reason: format!("exec stream error on {}: {}", container_name, e),
                            });
                        }
                        None => break,
                    }
                }
            }
        }
    }

    writer.flush().await.map_err(|e| BackupError::Failed {
        reason: format!("flush backup file {}: {}", out_path.display(), e),
    })?;

    let inspect = docker
        .inspect_exec(&exec.id)
        .await
        .map_err(|e| BackupError::Failed {
            reason: format!("inspect exec on {}: {}", container_name, e),
        })?;

    Ok(StreamedExec {
        exit_code: inspect.exit_code.unwrap_or(-1),
        stderr,
    })
}

/// Binlog coordinates captured at base-backup time. These anchor PITR: replay
/// starts from `(file, position)` (or `gtid`) and runs forward to the
/// recovery target.
///
/// Captured by querying `SHOW BINLOG STATUS` directly against the live
/// server (see `mariadb_physical::query_binlog_status`), not by parsing
/// `mariadb-backup`'s own stderr: WAL-G's `WALG_STREAM_CREATE_COMMAND` runs
/// `mariadb-backup` as a subprocess and does not forward that subprocess's
/// stderr into WAL-G's own captured output, so the position line it prints
/// never reaches the exec result this crate captures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinlogCoord {
    pub file: String,
    pub position: String,
    /// MariaDB GTID (`domain-server-seq`, e.g. `0-1-12`). Empty if the source
    /// has GTID disabled.
    pub gtid: String,
}
