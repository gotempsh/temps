// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The control-plane dump without Docker: `pg_dumpall --globals-only` and
//! `pg_dump` from the PostgreSQL client installed on this server, streamed
//! into one gzip file.
//!
//! A control plane with no Docker daemon (a microVM plane, a
//! `--profile control-plane` server) cannot run the `postgres:<major>` dump
//! image [`super::control_plane`] uses elsewhere. The Temps image installs
//! the PostgreSQL client next to the binary, so the same two commands run
//! here directly, with the same arguments, and produce the same
//! `backup.sql.gz` (globals first, then the database).
//!
//! `pg_dump` refuses to dump a server with a newer major version than its
//! own, so the client is checked against the database before anything runs
//! and a missing or older client fails the backup at once, saying which
//! version to install, instead of after the S3 checks with pg_dump's own
//! "server version mismatch".
//!
//! Memory is bounded: the dump is read in 64 KiB chunks through a channel of
//! [`CHANNEL_CHUNKS`] chunks into a blocking gzip writer, so at most about
//! 512 KiB of dump is in flight whatever the database size. stderr is kept
//! as a bounded tail for the failure message.

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use super::dump_capture::bounded_tail;
use crate::execution::engine_v2::BackupError;

const PG_DUMP: &str = "pg_dump";
const PG_DUMPALL: &str = "pg_dumpall";
/// How long `--version` may take before the tool counts as unusable.
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// Seconds libpq waits for the database before giving up on a connection.
const CONNECT_TIMEOUT_SECS: &str = "30";
/// Size of one read from a tool's stdout.
const CHUNK_BYTES: usize = 64 * 1024;
/// Chunks buffered between the tools and the gzip writer.
const CHANNEL_CHUNKS: usize = 8;
/// stderr bytes kept for a failure message.
const STDERR_TAIL_BYTES: usize = 4096;

/// The PostgreSQL client tools on this server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalPgClient {
    pg_dump: OsString,
    pg_dumpall: OsString,
    /// The major version both tools report.
    pub(crate) major: u32,
}

/// Why this server's PostgreSQL client cannot take the dump.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum LocalPgClientProblem {
    #[error("`{tool}` could not be run ({reason})")]
    NotRunnable { tool: &'static str, reason: String },
    #[error("`{tool} --version` printed no PostgreSQL version ({output:?})")]
    NoVersion { tool: &'static str, output: String },
    #[error("pg_dump is PostgreSQL {pg_dump} but pg_dumpall is PostgreSQL {pg_dumpall}")]
    Mismatched { pg_dump: u32, pg_dumpall: u32 },
}

/// Find `pg_dump` and `pg_dumpall` on this server's `PATH`.
pub(crate) async fn probe() -> Result<LocalPgClient, LocalPgClientProblem> {
    probe_with(OsString::from(PG_DUMP), OsString::from(PG_DUMPALL)).await
}

async fn probe_with(
    pg_dump: OsString,
    pg_dumpall: OsString,
) -> Result<LocalPgClient, LocalPgClientProblem> {
    let dump_major = tool_major(&pg_dump, PG_DUMP).await?;
    let dumpall_major = tool_major(&pg_dumpall, PG_DUMPALL).await?;
    if dump_major != dumpall_major {
        return Err(LocalPgClientProblem::Mismatched {
            pg_dump: dump_major,
            pg_dumpall: dumpall_major,
        });
    }
    Ok(LocalPgClient {
        pg_dump,
        pg_dumpall,
        major: dump_major,
    })
}

async fn tool_major(program: &OsStr, tool: &'static str) -> Result<u32, LocalPgClientProblem> {
    let run = Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = match tokio::time::timeout(VERSION_PROBE_TIMEOUT, run).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            return Err(LocalPgClientProblem::NotRunnable {
                tool,
                reason: error.to_string(),
            })
        }
        Err(_) => {
            return Err(LocalPgClientProblem::NotRunnable {
                tool,
                reason: format!(
                    "`--version` did not answer within {}s",
                    VERSION_PROBE_TIMEOUT.as_secs()
                ),
            })
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        return Err(LocalPgClientProblem::NotRunnable {
            tool,
            reason: format!("`--version` exited with {}", output.status),
        });
    }
    parse_major(&stdout).ok_or_else(|| LocalPgClientProblem::NoVersion {
        tool,
        output: bounded_tail(&stdout, 200),
    })
}

/// The major version in `pg_dump --version` output: `pg_dump (PostgreSQL)
/// 17.5`, `pg_dump (PostgreSQL) 16.4 (Debian 16.4-1.pgdg120+2)`,
/// `pg_dump (PostgreSQL) 18beta1`.
fn parse_major(version_output: &str) -> Option<u32> {
    version_output
        .split_whitespace()
        .find(|token| token.starts_with(|c: char| c.is_ascii_digit()))
        .and_then(|token| {
            let digits: String = token.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
}

/// The client to dump with, or the reason this server cannot take the
/// dump: no Docker for the dump image, and no usable client of at least the
/// database's major version.
pub(crate) fn require_client(
    probed: Result<LocalPgClient, LocalPgClientProblem>,
    server_major: u32,
    backup_id: i32,
) -> Result<LocalPgClient, BackupError> {
    let problem = match probed {
        Ok(client) if client.major >= server_major => return Ok(client),
        Ok(client) => format!(
            "this server's pg_dump is PostgreSQL {}, older than the control-plane database's \
             PostgreSQL {server_major}, and pg_dump refuses to dump a newer server",
            client.major
        ),
        Err(problem) => problem.to_string(),
    };
    Err(BackupError::PermanentFailure {
        reason: format!(
            "Control-plane backup {backup_id} cannot run on this server: it has no Docker \
             daemon to run the postgres:{server_major} dump image, and {problem}. Install the \
             PostgreSQL {server_major} client tools (pg_dump and pg_dumpall, version \
             {server_major} or newer) on this server, or run the control plane on a host with \
             Docker"
        ),
    })
}

/// What the startup log says about control-plane backups on a server with
/// no Docker, given what [`probe`] found.
pub(crate) fn no_docker_summary(probed: &Result<LocalPgClient, LocalPgClientProblem>) -> String {
    match probed {
        Ok(client) => format!(
            "control_plane backups run with this server's PostgreSQL {} client (pg_dump and \
             pg_dumpall), which must be at least the database's major version",
            client.major
        ),
        Err(problem) => format!(
            "control_plane backups will fail: there is no Docker to run the dump image and \
             {problem}; install the PostgreSQL client tools of the database's major version \
             on this server"
        ),
    }
}

/// Where the dump connects and what it leaves schema-only.
pub(crate) struct LocalDumpArgs<'a> {
    pub host: &'a str,
    pub port: u16,
    pub username: &'a str,
    pub database: &'a str,
    pub password: &'a str,
    pub exclude_patterns: &'a [String],
}

impl LocalDumpArgs<'_> {
    fn connection(&self) -> [String; 3] {
        [
            format!("--host={}", self.host),
            format!("--port={}", self.port),
            format!("--username={}", self.username),
        ]
    }

    /// The sidecar's `pg_dumpall --globals-only` arguments.
    fn globals(&self) -> Vec<String> {
        let mut args: Vec<String> = ["--globals-only", "--clean", "--if-exists", "--no-password"]
            .map(String::from)
            .into();
        args.extend(self.connection());
        args.push(format!("--database={}", self.database));
        args
    }

    /// The sidecar's `pg_dump` arguments.
    fn database_dump(&self) -> Vec<String> {
        let mut args: Vec<String> = ["--clean", "--if-exists", "--no-password"]
            .map(String::from)
            .into();
        args.extend(self.connection());
        args.extend(
            self.exclude_patterns
                .iter()
                .map(|pattern| format!("--exclude-table-data={pattern}")),
        );
        args.push(format!("--dbname={}", self.database));
        args
    }
}

/// Dump the globals and then the database into `out` as one gzip stream.
/// Returns the size of `out`. A partial file is left for the caller's
/// attempt directory to remove.
pub(crate) async fn dump_to_gzip(
    client: &LocalPgClient,
    args: &LocalDumpArgs<'_>,
    out: &Path,
    backup_id: i32,
    cancel: &CancellationToken,
) -> Result<u64, BackupError> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(CHANNEL_CHUNKS);
    let out_path = out.to_path_buf();
    let writer = tokio::task::spawn_blocking(move || write_gzip(&out_path, rx));

    let dumped = async {
        stream_tool(
            &client.pg_dumpall,
            PG_DUMPALL,
            &args.globals(),
            args.password,
            &tx,
            backup_id,
            cancel,
        )
        .await?;
        stream_tool(
            &client.pg_dump,
            PG_DUMP,
            &args.database_dump(),
            args.password,
            &tx,
            backup_id,
            cancel,
        )
        .await
    }
    .await;
    // Closing the channel lets the writer finish the gzip stream.
    drop(tx);
    let written = writer.await.map_err(|error| BackupError::Failed {
        reason: format!("gzip writer for control-plane backup {backup_id} panicked: {error}"),
    })?;
    // A writer error (disk full, say) is why a tool saw the channel close,
    // so it is the one to report.
    let size = written.map_err(|error| BackupError::Failed {
        reason: format!(
            "could not write control-plane backup {backup_id} to {}: {error}",
            out.display()
        ),
    })?;
    dumped?;
    Ok(size)
}

fn write_gzip(path: &Path, mut rx: tokio::sync::mpsc::Receiver<Vec<u8>>) -> std::io::Result<u64> {
    let file = std::fs::File::create(path)?;
    let mut gz = flate2::write::GzEncoder::new(
        std::io::BufWriter::new(file),
        flate2::Compression::default(),
    );
    while let Some(chunk) = rx.blocking_recv() {
        gz.write_all(&chunk)?;
    }
    let file = gz.finish()?.into_inner().map_err(|e| e.into_error())?;
    file.sync_all()?;
    Ok(file.metadata()?.len())
}

/// Run one tool and send its stdout down `tx`; fail with its stderr tail
/// when it exits non-zero. The password travels in `PGPASSWORD`, never on
/// the command line.
async fn stream_tool(
    program: &OsStr,
    tool: &'static str,
    args: &[String],
    password: &str,
    tx: &tokio::sync::mpsc::Sender<Vec<u8>>,
    backup_id: i32,
    cancel: &CancellationToken,
) -> Result<(), BackupError> {
    let mut child = Command::new(program)
        .args(args)
        .env("PGPASSWORD", password)
        .env("PGCONNECT_TIMEOUT", CONNECT_TIMEOUT_SECS)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A cancelled or failed backup must not leave the dump running.
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| BackupError::Failed {
            reason: format!("could not start {tool} for control-plane backup {backup_id}: {error}"),
        })?;
    let (Some(mut stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(BackupError::Failed {
            reason: format!("{tool} for control-plane backup {backup_id} has no output pipes"),
        });
    };
    // Drained alongside stdout so a chatty stderr cannot fill its pipe and
    // stall the dump.
    let stderr_tail = tokio::spawn(read_tail(stderr, STDERR_TAIL_BYTES));

    loop {
        let mut chunk = Vec::with_capacity(CHUNK_BYTES);
        let read = tokio::select! {
            read = stdout.read_buf(&mut chunk) => read,
            () = cancel.cancelled() => return Err(BackupError::Cancelled),
        };
        let read = read.map_err(|error| BackupError::Failed {
            reason: format!(
                "could not read {tool} output for control-plane backup {backup_id}: {error}"
            ),
        })?;
        if read == 0 {
            break;
        }
        let sent = tokio::select! {
            sent = tx.send(chunk) => sent,
            () = cancel.cancelled() => return Err(BackupError::Cancelled),
        };
        if sent.is_err() {
            return Err(BackupError::Failed {
                reason: format!(
                    "the gzip writer for control-plane backup {backup_id} stopped while {tool} \
                     was running"
                ),
            });
        }
    }
    let status = tokio::select! {
        status = child.wait() => status,
        () = cancel.cancelled() => return Err(BackupError::Cancelled),
    }
    .map_err(|error| BackupError::Failed {
        reason: format!("could not wait for {tool} in control-plane backup {backup_id}: {error}"),
    })?;
    let stderr = stderr_tail.await.unwrap_or_default();
    if !status.success() {
        return Err(BackupError::Failed {
            reason: format!(
                "{tool} exited with {status} in control-plane backup {backup_id}: {}",
                bounded_tail(&stderr, STDERR_TAIL_BYTES)
            ),
        });
    }
    Ok(())
}

/// Read `reader` to the end, keeping its last `max_bytes` bytes.
async fn read_tail(mut reader: impl AsyncRead + Unpin, max_bytes: usize) -> String {
    let mut tail: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                tail.extend_from_slice(&chunk[..read]);
                if tail.len() > 2 * max_bytes {
                    tail.drain(..tail.len() - max_bytes);
                }
            }
        }
    }
    String::from_utf8_lossy(&tail).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_major_version_is_read_from_every_pg_dump_banner() {
        assert_eq!(parse_major("pg_dump (PostgreSQL) 17.5\n"), Some(17));
        assert_eq!(
            parse_major("pg_dump (PostgreSQL) 16.4 (Debian 16.4-1.pgdg120+2)"),
            Some(16)
        );
        assert_eq!(parse_major("pg_dumpall (PostgreSQL) 18beta1"), Some(18));
        assert_eq!(parse_major("pg_dump (PostgreSQL)"), None);
        assert_eq!(parse_major(""), None);
    }

    fn client(major: u32) -> LocalPgClient {
        LocalPgClient {
            pg_dump: PG_DUMP.into(),
            pg_dumpall: PG_DUMPALL.into(),
            major,
        }
    }

    #[test]
    fn a_client_at_least_as_new_as_the_database_is_used() {
        assert_eq!(
            require_client(Ok(client(18)), 18, 46).ok(),
            Some(client(18))
        );
        assert_eq!(
            require_client(Ok(client(19)), 18, 46).ok(),
            Some(client(19))
        );
    }

    // A microVM control plane has no Docker: without a usable client the
    // backup fails at once, permanently, naming the version to install.
    #[test]
    fn no_usable_client_fails_the_backup_saying_what_to_install() {
        let older = require_client(Ok(client(17)), 18, 46).unwrap_err();
        let missing = require_client(
            Err(LocalPgClientProblem::NotRunnable {
                tool: PG_DUMP,
                reason: "No such file or directory (os error 2)".into(),
            }),
            18,
            46,
        )
        .unwrap_err();
        for (error, cause) in [
            (
                &older,
                "PostgreSQL 17, older than the control-plane database's PostgreSQL 18",
            ),
            (
                &missing,
                "`pg_dump` could not be run (No such file or directory",
            ),
        ] {
            assert!(error.is_permanent(), "{error}");
            let message = error.to_string();
            assert!(message.contains("Control-plane backup 46"), "{message}");
            assert!(message.contains("no Docker daemon"), "{message}");
            assert!(message.contains(cause), "{message}");
            assert!(
                message.contains("Install the PostgreSQL 18 client tools"),
                "{message}"
            );
        }
    }

    // The startup line used to say control_plane needs no local Docker; it
    // says what the backup will actually do.
    #[test]
    fn the_startup_summary_says_whether_control_plane_backups_can_run() {
        let ready = no_docker_summary(&Ok(client(17)));
        assert!(ready.contains("PostgreSQL 17 client"), "{ready}");
        let failing = no_docker_summary(&Err(LocalPgClientProblem::NotRunnable {
            tool: PG_DUMP,
            reason: "not found".into(),
        }));
        assert!(
            failing.starts_with("control_plane backups will fail"),
            "{failing}"
        );
        assert!(
            failing.contains("install the PostgreSQL client"),
            "{failing}"
        );
    }

    #[cfg(unix)]
    mod with_fake_tools {
        use super::*;
        use std::io::Read;
        use std::os::unix::fs::PermissionsExt;

        /// A stand-in for a PostgreSQL tool: answers `--version`, and
        /// otherwise prints its arguments and whether it saw the password.
        fn fake_tool(dir: &Path, name: &str, version: &str, exit: i32) -> OsString {
            let path = dir.join(name);
            std::fs::write(
                &path,
                format!(
                    "#!/bin/sh\n\
                     if [ \"$1\" = --version ]; then echo '{name} (PostgreSQL) {version}'; exit 0; fi\n\
                     echo \"-- {name} $*\"\n\
                     echo \"-- password=$PGPASSWORD\"\n\
                     echo \"{name} complains\" >&2\n\
                     exit {exit}\n"
                ),
            )
            .unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path.into_os_string()
        }

        fn args(patterns: &[String]) -> LocalDumpArgs<'_> {
            LocalDumpArgs {
                host: "127.0.0.1",
                port: 5432,
                username: "temps",
                database: "temps",
                password: "s3cret",
                exclude_patterns: patterns,
            }
        }

        #[tokio::test]
        async fn probing_reads_both_versions_and_refuses_a_mismatch() {
            let dir = tempfile::tempdir().unwrap();
            let pg_dump = fake_tool(dir.path(), "pg_dump", "18.1", 0);
            let pg_dumpall = fake_tool(dir.path(), "pg_dumpall", "18.1", 0);
            let probed = probe_with(pg_dump.clone(), pg_dumpall).await.unwrap();
            assert_eq!(probed.major, 18);

            let old_dumpall = fake_tool(dir.path(), "old_dumpall", "17.2", 0);
            assert_eq!(
                probe_with(pg_dump, old_dumpall).await,
                Err(LocalPgClientProblem::Mismatched {
                    pg_dump: 18,
                    pg_dumpall: 17
                })
            );
            let missing = probe_with(dir.path().join("absent").into(), "x".into()).await;
            assert!(
                matches!(
                    missing,
                    Err(LocalPgClientProblem::NotRunnable { tool: PG_DUMP, .. })
                ),
                "{missing:?}"
            );
        }

        #[tokio::test]
        async fn globals_then_the_database_land_in_one_gzip_file() {
            let dir = tempfile::tempdir().unwrap();
            let client = LocalPgClient {
                pg_dump: fake_tool(dir.path(), "pg_dump", "18.1", 0),
                pg_dumpall: fake_tool(dir.path(), "pg_dumpall", "18.1", 0),
                major: 18,
            };
            let patterns = vec!["public.proxy_logs".to_string()];
            let out = dir.path().join("backup.sql.gz");
            let size = dump_to_gzip(
                &client,
                &args(&patterns),
                &out,
                46,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
            assert_eq!(size, std::fs::metadata(&out).unwrap().len());

            let mut sql = String::new();
            flate2::read::GzDecoder::new(std::fs::File::open(&out).unwrap())
                .read_to_string(&mut sql)
                .unwrap();
            let globals = sql.find("-- pg_dumpall --globals-only").expect(&sql);
            let database = sql.find("-- pg_dump --clean").expect(&sql);
            assert!(globals < database, "{sql}");
            assert!(
                sql.contains("--exclude-table-data=public.proxy_logs"),
                "{sql}"
            );
            assert!(sql.contains("--dbname=temps"), "{sql}");
            // The password reaches the tool through the environment only.
            assert!(sql.contains("-- password=s3cret"), "{sql}");
            assert!(!sql.contains("--password"), "{sql}");
        }

        #[tokio::test]
        async fn a_failing_tool_fails_the_dump_with_its_stderr() {
            let dir = tempfile::tempdir().unwrap();
            let client = LocalPgClient {
                pg_dump: fake_tool(dir.path(), "pg_dump", "18.1", 1),
                pg_dumpall: fake_tool(dir.path(), "pg_dumpall", "18.1", 0),
                major: 18,
            };
            let out = dir.path().join("backup.sql.gz");
            let error = dump_to_gzip(&client, &args(&[]), &out, 46, &CancellationToken::new())
                .await
                .unwrap_err();
            let message = error.to_string();
            assert!(!error.is_permanent(), "{message}");
            assert!(message.contains("pg_dump exited with"), "{message}");
            assert!(message.contains("backup 46"), "{message}");
            assert!(message.contains("pg_dump complains"), "{message}");
        }

        #[tokio::test]
        async fn a_cancelled_dump_stops() {
            let dir = tempfile::tempdir().unwrap();
            let slow = dir.path().join("slow_dumpall");
            std::fs::write(&slow, "#!/bin/sh\nexec sleep 30\n").unwrap();
            std::fs::set_permissions(&slow, std::fs::Permissions::from_mode(0o755)).unwrap();
            let client = LocalPgClient {
                pg_dump: fake_tool(dir.path(), "pg_dump", "18.1", 0),
                pg_dumpall: slow.into_os_string(),
                major: 18,
            };
            let cancel = CancellationToken::new();
            let trigger = cancel.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(200)).await;
                trigger.cancel();
            });
            let started = std::time::Instant::now();
            let error = dump_to_gzip(
                &client,
                &args(&[]),
                &dir.path().join("backup.sql.gz"),
                46,
                &cancel,
            )
            .await
            .unwrap_err();
            assert!(matches!(error, BackupError::Cancelled), "{error}");
            assert!(started.elapsed() < Duration::from_secs(10));
        }
    }
}
