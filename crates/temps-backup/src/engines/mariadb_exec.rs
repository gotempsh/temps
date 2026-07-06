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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinlogCoord {
    pub file: String,
    pub position: String,
    /// MariaDB GTID (`domain-server-seq`, e.g. `0-1-12`). Empty if the source
    /// has GTID disabled.
    pub gtid: String,
}

/// Parse the binlog-position line `mariadb-backup` prints to stderr at the end
/// of a successful `--backup`. Format (MariaDB 10.x–12.x):
///
/// ```text
/// mariabackup: MySQL binlog position: filename 'mysql-bin.000003', position '342', GTID of the last change '0-1-12'
/// ```
///
/// Older builds omit the GTID clause. Returns `None` if no position line is
/// present (e.g. binary logging disabled on the source).
pub fn parse_binlog_position(stderr: &str) -> Option<BinlogCoord> {
    // Anchor on "binlog position:" so we don't mis-match the bare word
    // "position" elsewhere in the log.
    let anchor = stderr.find("binlog position:")?;
    let tail = &stderr[anchor..];
    let file = extract_quoted_after(tail, "filename", 0)?;
    // Search for "position '" strictly after the filename match so the
    // "binlog position:" header (which has no quote) is skipped.
    let pos_key = tail.find("position '")?;
    let position = extract_quoted_after(tail, "position", pos_key)?;
    let gtid = tail
        .find("GTID of the last change")
        .and_then(|idx| extract_quoted_after(tail, "GTID of the last change", idx))
        .unwrap_or_default();
    Some(BinlogCoord {
        file,
        position,
        gtid,
    })
}

/// Find `key` at/after `from`, then return the next single-quoted value.
fn extract_quoted_after(hay: &str, key: &str, from: usize) -> Option<String> {
    let key_idx = hay.get(from..)?.find(key)? + from;
    let after = &hay[key_idx + key.len()..];
    let open = after.find('\'')?;
    let rest = &after[open + 1..];
    let close = rest.find('\'')?;
    Some(rest[..close].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_binlog_position_with_gtid() {
        let stderr = "[00] 2024-06-23 mariabackup: MySQL binlog position: \
             filename 'mysql-bin.000003', position '342', GTID of the last change '0-1-12'\n";
        let c = parse_binlog_position(stderr).expect("should parse");
        assert_eq!(c.file, "mysql-bin.000003");
        assert_eq!(c.position, "342");
        assert_eq!(c.gtid, "0-1-12");
    }

    #[test]
    fn parses_binlog_position_without_gtid() {
        let stderr =
            "mariabackup: MySQL binlog position: filename 'mariadb-bin.000007', position '15201'\n";
        let c = parse_binlog_position(stderr).expect("should parse");
        assert_eq!(c.file, "mariadb-bin.000007");
        assert_eq!(c.position, "15201");
        assert_eq!(c.gtid, "");
    }

    #[test]
    fn returns_none_when_binlog_disabled() {
        let stderr = "mariabackup: completed OK!\n";
        assert!(parse_binlog_position(stderr).is_none());
    }
}
