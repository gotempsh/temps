// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! WAL-G setup and continuous archiving for managed PostgreSQL.

pub use temps_providers_core::externalsvc::error::PostgresRestartNotConfirmed;
pub use temps_providers_core::externalsvc::naming::shell_escape;

use super::*;
use crate::externalsvc::{ExternalService, ServiceConfig, ServiceResourceLimits};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, info, warn};
use urlencoding;

pub(super) fn shell_export_assignment(line: &str) -> Option<String> {
    let (key, value) = line.split_once('=')?;
    let valid_key = key
        .chars()
        .all(|character| character == '_' || character.is_ascii_alphanumeric())
        && key
            .chars()
            .next()
            .is_some_and(|character| character == '_' || character.is_ascii_alphabetic());
    valid_key.then(|| format!("export {key}={}", shell_escape(value)))
}

/// A shell command that replaces `path` with `lines` atomically.
///
/// `archive_command` sources `walg.env` for every segment, so the live file
/// must never be seen half written, and a failed write (a full disk is
/// likely exactly when WAL has piled up) must leave the old file intact.
/// The lines go to a sibling temporary file created mode 0600, which is
/// renamed over `path` only after the whole write succeeded; on failure the
/// temporary file is removed and the command exits non-zero. Leftovers of
/// an earlier write killed mid-way are removed first.
///
/// Every write holds an `flock` on `<path>.lock` (where `flock` exists), so
/// two writers never interleave. With `expected_checksum` (a `cksum < path`
/// line from [`WalgEnvSnapshot`]) the file is only replaced if it still
/// has that content under the lock; otherwise the command exits
/// [`WALG_ENV_CHANGED_EXIT`] and leaves it alone, so a repair computed from
/// an older snapshot never overwrites what a backup or repoint just wrote.
pub(crate) fn atomic_write_command(
    lines: &[String],
    path: &str,
    expected_checksum: Option<&str>,
) -> String {
    let path = shell_escape(path);
    let unchanged_check = expected_checksum
        .map(|checksum| {
            format!(
                "if [ \"$(cksum < {path} 2>/dev/null)\" != {checksum} ]; then exit {WALG_ENV_CHANGED_EXIT}; fi; ",
                checksum = shell_escape(checksum),
            )
        })
        .unwrap_or_default();
    format!(
        "umask 077; exec 9>>{path}.lock || exit 1; \
         if command -v flock >/dev/null 2>&1; then flock -w 30 9 || exit {WALG_ENV_LOCK_BUSY_EXIT}; fi; \
         {unchanged_check}\
         rm -f {path}.tmp.*; tmp={path}.tmp.$$; \
         if printf '%s\\n' {lines} > \"$tmp\" && chmod 600 \"$tmp\" && mv -f \"$tmp\" {path}; \
         then exit 0; else rm -f \"$tmp\"; exit 1; fi",
        lines = lines
            .iter()
            .map(|line| shell_escape(line))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// Exit code of [`atomic_write_command`] when the file no longer has the
/// expected content.
pub(crate) const WALG_ENV_CHANGED_EXIT: i64 = 3;

/// Exit code of [`atomic_write_command`] when another writer held the lock
/// for 30 seconds.
pub(crate) const WALG_ENV_LOCK_BUSY_EXIT: i64 = 4;

/// What a conditional write of `walg.env` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WalgEnvWrite {
    Written,
    /// The file changed since the snapshot the new contents were built
    /// from; it was left as it is.
    ChangedConcurrently,
}

/// One consistent read of `walg.env`: its checksum, its text, and the
/// `WALG_`/`AWS_` assignments the shell sees when sourcing it, all taken
/// from the same copy of the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WalgEnvSnapshot {
    pub(crate) checksum: String,
    pub(crate) raw: String,
    pub(crate) evaluated: Vec<String>,
}

const SNAPSHOT_NO_FILE: &str = "__TEMPS_NO_WALG_ENV__";
const SNAPSHOT_CKSUM: &str = "__TEMPS_WALG_ENV_CKSUM__";
const SNAPSHOT_RAW: &str = "__TEMPS_WALG_ENV_RAW__";
const SNAPSHOT_EVAL: &str = "__TEMPS_WALG_ENV_EVAL__";

/// Copies `walg.env` once and reports everything from that copy, so the
/// checksum, text and assignments always describe the same file.
///
/// The copy holds credentials, so it is removed on every way out: the
/// `EXIT` trap is set as soon as `mktemp` created it, before the copy that
/// can fail part way (a full `/tmp`, the file vanishing).
const WALG_ENV_SNAPSHOT_SCRIPT: &str = "f=/var/lib/postgresql/walg.env; \
     [ -f \"$f\" ] || { echo __TEMPS_NO_WALG_ENV__; exit 0; }; \
     umask 077; snap=$(mktemp \"${TMPDIR:-/tmp}/walg-env.XXXXXX\") || exit 1; \
     trap 'rm -f \"$snap\"' EXIT; \
     cp \"$f\" \"$snap\" || exit 1; \
     echo __TEMPS_WALG_ENV_CKSUM__; cksum < \"$snap\"; \
     echo __TEMPS_WALG_ENV_RAW__; cat \"$snap\"; echo; \
     echo __TEMPS_WALG_ENV_EVAL__; \
     env -i sh -c '. \"$1\" && env' sh \"$snap\" | grep -E '^(WALG_|AWS_)'; \
     exit 0";

/// Parse what [`WALG_ENV_SNAPSHOT_SCRIPT`] printed: `Ok(None)` when there is
/// no file.
pub(crate) fn parse_walg_env_snapshot(
    output: &str,
) -> std::result::Result<Option<WalgEnvSnapshot>, String> {
    if output.lines().any(|line| line == SNAPSHOT_NO_FILE) {
        return Ok(None);
    }
    let missing = || "the walg.env snapshot is incomplete".to_string();
    let after_cksum = output
        .split_once(&format!("{SNAPSHOT_CKSUM}\n"))
        .ok_or_else(missing)?
        .1;
    let (checksum, after_raw) = after_cksum
        .split_once(&format!("\n{SNAPSHOT_RAW}\n"))
        .ok_or_else(missing)?;
    let (raw, evaluated) = after_raw
        .rsplit_once(&format!("\n{SNAPSHOT_EVAL}\n"))
        .or_else(|| {
            after_raw
                .strip_suffix(&format!("\n{SNAPSHOT_EVAL}"))
                .map(|raw| (raw, ""))
        })
        .ok_or_else(missing)?;
    Ok(Some(WalgEnvSnapshot {
        checksum: checksum.trim().to_string(),
        raw: raw.to_string(),
        evaluated: evaluated
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect(),
    }))
}

/// What enabling WAL archiving did to the running service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalArchivingOutcome {
    /// PostgreSQL was restarted in a recreated container (archive_mode was
    /// off). The data was verified to be the same cluster before this was
    /// reported.
    pub restarted: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("Failed to verify WAL archiving for PostgreSQL service '{service_name}': {reason}. Check this service's logs and retry this operation")]
pub(super) struct PostgresArchiveVerificationError {
    pub(super) service_name: String,
    pub(super) reason: String,
}

impl From<PostgresArchiveVerificationError> for EngineError {
    fn from(error: PostgresArchiveVerificationError) -> Self {
        EngineError::failed(error.service_name.clone(), error.to_string())
    }
}

/// WAL-G could not write to the archive target with the exact settings
/// `walg.env` would hold. Raised before `walg.env` is written, so a broken
/// target is reported when archiving is turned on or re-pointed instead of
/// PostgreSQL silently keeping every WAL segment until the disk fills.
#[derive(Debug, Clone, thiserror::Error)]
#[error(
    "WAL archive target check failed for PostgreSQL service '{service_name}': WAL-G could not \
     write to {endpoint} (scheme {scheme}, path-style addressing {path_style}, {path_style_reason}): \
     {message}. Archiving settings were not changed",
    path_style = if *force_path_style { "on" } else { "off" }
)]
pub struct ArchiveTargetCheckError {
    pub service_name: String,
    /// `AWS_ENDPOINT` as WAL-G received it, or "AWS S3" when unset.
    pub endpoint: String,
    /// `http`, `https`, or `aws` for AWS S3 itself.
    pub scheme: String,
    pub force_path_style: bool,
    pub path_style_reason: String,
    /// WAL-G's own error, credentials redacted.
    pub message: String,
}

/// Boxed where it is returned: it carries six strings, too large to pass
/// by value through every `Result`.
impl From<Box<ArchiveTargetCheckError>> for EngineError {
    fn from(error: Box<ArchiveTargetCheckError>) -> Self {
        EngineError::failed(error.service_name.clone(), error.to_string())
    }
}

/// What a successful archive target check verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveTargetCheck {
    pub endpoint: String,
    pub scheme: String,
    pub force_path_style: bool,
    pub path_style_reason: String,
}

/// Upper bound of one archive target check. WAL-G retries a failing request
/// for minutes by default; `S3_MAX_RETRIES=1` makes it report the first
/// error, and this bounds a target that never answers.
const ARCHIVE_TARGET_CHECK_TIMEOUT_SECS: u64 = 60;

/// Upper bound of removing the check's probe after a successful upload.
/// Best effort: a removal that fails or hangs never fails the check.
const ARCHIVE_PROBE_CLEANUP_TIMEOUT_SECS: u64 = 30;

/// How long the exec running the check may take: the upload and the
/// cleanup run one after the other, each under its own `timeout`, plus a
/// margin for starting the shell. A slow but successful upload followed by
/// a stalled removal must still reach the line that reports the upload's
/// exit code, or a writable target would read as a failed check.
fn archive_target_check_exec_deadline() -> Duration {
    Duration::from_secs(ARCHIVE_TARGET_CHECK_TIMEOUT_SECS + ARCHIVE_PROBE_CLEANUP_TIMEOUT_SECS + 15)
}

/// The settings a check reports: what `walg_env` hands WAL-G.
fn archive_target_settings(
    walg_env: &[String],
    s3_credentials: &crate::externalsvc::S3Credentials,
) -> ArchiveTargetCheck {
    let endpoint = walg_env
        .iter()
        .find_map(|line| line.strip_prefix("AWS_ENDPOINT="))
        .map(str::to_string);
    let force_path_style = walg_env
        .iter()
        .any(|line| line == "AWS_S3_FORCE_PATH_STYLE=true");
    // The reason follows what WAL-G is actually given, so a report never
    // says "off, forced on".
    let ip_endpoint = endpoint
        .as_deref()
        .is_some_and(temps_core::s3_endpoint::endpoint_host_is_ip_literal);
    let reason = if !force_path_style || s3_credentials.force_path_style {
        temps_core::s3_endpoint::PathStyleReason::Configured
    } else if ip_endpoint {
        temps_core::s3_endpoint::PathStyleReason::IpAddressEndpoint
    } else {
        s3_credentials.path_style().reason
    };
    let scheme = match endpoint.as_deref() {
        // What WAL-G does with a scheme-less endpoint, which is the failure
        // this report exists to explain.
        Some(endpoint) if !endpoint.contains("://") => "missing (WAL-G uses https)".to_string(),
        Some(endpoint) => temps_core::s3_endpoint::S3Endpoint::parse(endpoint, None)
            .map(|parsed| parsed.scheme().to_string())
            .unwrap_or_else(|_| "unknown".to_string()),
        None => "aws".to_string(),
    };
    ArchiveTargetCheck {
        endpoint: endpoint.unwrap_or_else(|| format!("AWS S3 (region {})", s3_credentials.region)),
        scheme,
        force_path_style,
        path_style_reason: reason.describe().to_string(),
    }
}

/// The one object the archive target check writes, at the root of the
/// service's WAL-G prefix, outside `basebackups_005/` and `wal_005/` (the
/// only folders WAL-G reads for backups and WAL).
pub(crate) const ARCHIVE_CHECK_PROBE_KEY: &str = "temps-archive-check";

/// The archive target check, run in the service's container with the
/// `WALG_`/`AWS_` settings in its environment.
///
/// It uploads a 5-byte probe to the fixed [`ARCHIVE_CHECK_PROBE_KEY`]
/// (`st put -f` overwrites it), then tries to remove it. A worker node's
/// scoped archive credential may write but never delete, so its removal is
/// refused; because the key is fixed, the next check overwrites the same
/// object instead of leaving another one. (`wal-g st check write` uploads a
/// randomly named object and only warns when it cannot delete it, which
/// left one object per scoped backup.)
///
/// The script always exits 0 and prints the upload's exit code on its last
/// line, so a refusal is read as WAL-G's answer rather than a failed exec.
pub(crate) fn archive_target_check_script() -> String {
    format!(
        "if command -v wal-g >/dev/null 2>&1; then \
         probe=$(mktemp) && printf temps > \"$probe\" && \
         timeout {ARCHIVE_TARGET_CHECK_TIMEOUT_SECS} wal-g st put -f --no-compress --no-encrypt \
           \"$probe\" {ARCHIVE_CHECK_PROBE_KEY} 2>&1; code=$?; \
         rm -f \"$probe\"; \
         if [ \"$code\" -eq 0 ]; then \
           timeout {ARCHIVE_PROBE_CLEANUP_TIMEOUT_SECS} wal-g st rm {ARCHIVE_CHECK_PROBE_KEY} >/dev/null 2>&1 || true; \
         fi; \
         else echo 'wal-g is not installed in this image'; code=127; fi; \
         echo \"{CHECK_EXIT_MARKER}$code\"; exit 0"
    )
}

/// Prefix of the last line the check script prints: WAL-G's exit code.
const CHECK_EXIT_MARKER: &str = "__TEMPS_WALG_CHECK_EXIT__=";

/// Split the check script's output into WAL-G's own output and its exit
/// code.
fn split_check_exit(output: &str) -> (String, Option<i64>) {
    match output.rfind(CHECK_EXIT_MARKER) {
        Some(at) => {
            let code = output[at + CHECK_EXIT_MARKER.len()..]
                .trim()
                .parse::<i64>()
                .ok();
            (output[..at].to_string(), code)
        }
        None => (output.to_string(), None),
    }
}

/// The part of WAL-G's output worth showing: its error lines, or the tail.
fn summarize_walg_output(output: &str) -> String {
    let errors: Vec<&str> = output
        .lines()
        .map(str::trim)
        .filter(|line| {
            line.starts_with("ERROR")
                || line.starts_with("caused by")
                || line.contains("wal-g is not installed")
        })
        .collect();
    let picked = if errors.is_empty() {
        output
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .rev()
            .take(3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
    } else {
        errors
    };
    let joined = picked.join(" | ");
    if joined.is_empty() {
        return "no output".to_string();
    }
    const LIMIT: usize = 800;
    if joined.len() > LIMIT {
        let cut = (0..=LIMIT)
            .rev()
            .find(|i| joined.is_char_boundary(*i))
            .unwrap_or(0);
        format!("{}…", &joined[..cut])
    } else {
        joined
    }
}

pub(super) fn walg_target_user_data(
    backup: &temps_entities::backups::Model,
) -> Result<Option<String>> {
    let metadata: serde_json::Value = serde_json::from_str(&backup.metadata).map_err(|e| {
        EngineError::engine_failure(
            "PostgreSQL",
            format!(
                "Backup {} has invalid metadata JSON: {}",
                backup.backup_id, e
            ),
        )
    })?;
    let Some(version) = metadata.get("walg_identity_version") else {
        return Ok(None);
    };
    if version.as_u64() != Some(1) {
        return Err(EngineError::engine_failure(
            "PostgreSQL",
            format!(
                "Backup {} uses unsupported WAL-G identity version {}",
                backup.backup_id, version
            ),
        ));
    }
    let value = metadata.get("walg_target_user_data").ok_or_else(|| {
        EngineError::engine_failure(
            "PostgreSQL",
            format!(
                "Backup {} is missing its WAL-G target user data",
                backup.backup_id
            ),
        )
    })?;
    if value
        .get("temps_backup_id")
        .and_then(serde_json::Value::as_str)
        != Some(backup.backup_id.as_str())
    {
        return Err(EngineError::engine_failure(
            "PostgreSQL",
            format!(
                "Backup {} has WAL-G target user data for a different backup",
                backup.backup_id
            ),
        ));
    }
    Ok(Some(serde_json::to_string(value).map_err(|e| {
        EngineError::engine_failure(
            "PostgreSQL",
            format!(
                "Failed to serialize WAL-G target user data for backup {}: {}",
                backup.backup_id, e
            ),
        )
    })?))
}

impl PostgresService {
    /// Read a file from inside a container and return its contents as a String.
    /// Used for capturing log output on failure. Returns a fallback message on error.
    pub(super) async fn read_container_file(&self, container_name: &str, path: &str) -> String {
        use bollard::exec::CreateExecOptions;
        use temps_docker_util::{
            exec_capture, DockerUtilError, ExecCaptureOptions, ExitCodePolicy, OtherFrames,
            OutputLimit, StreamErrorPolicy,
        };

        let captured = exec_capture(
            &self.docker,
            container_name,
            CreateExecOptions {
                cmd: Some(vec!["cat", path]),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                user: Some("postgres"),
                ..Default::default()
            },
            ExecCaptureOptions::combined(OutputLimit::Unbounded)
                .other_frames(OtherFrames::ToStdout)
                .stream_errors(StreamErrorPolicy::Stop)
                .exit_code(ExitCodePolicy::NotRead),
        )
        .await;
        match captured {
            Ok(output) => output.stdout,
            Err(DockerUtilError::ExecCreate { .. }) => {
                "(failed to create exec for log capture)".to_string()
            }
            Err(_) => "(failed to read log file)".to_string(),
        }
    }

    /// Check if the WAL-G binary is available inside a container.
    /// Returns true if `wal-g` is found, false otherwise.
    pub(super) async fn container_has_walg(&self, container_name: &str) -> bool {
        use bollard::exec::{CreateExecOptions, StartExecOptions};

        let exec = match self
            .docker
            .create_exec(
                container_name,
                CreateExecOptions {
                    cmd: Some(vec!["which", "wal-g"]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(e) => e,
            Err(_) => return false,
        };

        if self
            .docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await
            .is_err()
        {
            return false;
        }

        // Wait for completion
        loop {
            match self.docker.inspect_exec(&exec.id).await {
                Ok(inspect) => {
                    if inspect.running == Some(false) {
                        return inspect.exit_code == Some(0);
                    }
                }
                Err(_) => return false,
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    /// Write WAL-G credentials to an env file on the shared volume and enable
    /// continuous WAL archiving via `ALTER SYSTEM`.
    ///
    /// PostgreSQL calls `archive_command` once per completed WAL segment (16 MB each).
    /// Each invocation is a fresh shell, so it reads the latest env file automatically.
    /// This means credential rotations take effect without any restart — just overwrite
    /// the file and the next WAL push uses the new credentials.
    ///
    /// Write `/var/lib/postgresql/walg.env` onto the given container.
    ///
    /// This is the credential file `archive_command = wal-g wal-push %p`
    /// relies on when continuous WAL archiving is enabled. Called from
    /// `enable_wal_archiving` before a recoverable WAL-G backup.
    ///
    /// Idempotent — overwrites any existing file. Writes with 0600 perms.
    async fn write_walg_env_file(&self, container_name: &str, walg_env: &[String]) -> Result<()> {
        self.write_walg_env_file_at(container_name, walg_env, "/var/lib/postgresql/walg.env")
            .await
    }

    /// Write a read-only WAL-G credential file used only by
    /// `restore_command`. Kept separate from the write-capable `walg.env`
    /// so a restored cluster can read WAL from the source's prefix without
    /// ever having the credentials at a path that `archive_command` would
    /// find, preventing source-prefix contamination during recovery.
    pub(super) async fn write_walg_restore_env_file(
        &self,
        container_name: &str,
        walg_env: &[String],
    ) -> Result<()> {
        self.write_walg_env_file_at(
            container_name,
            walg_env,
            "/var/lib/postgresql/walg-restore.env",
        )
        .await
    }

    /// Internal: write a wal-g credential file at an arbitrary path.
    /// See `write_walg_env_file` / `write_walg_restore_env_file` for the
    /// two concrete roles.
    async fn write_walg_env_file_at(
        &self,
        container_name: &str,
        walg_env: &[String],
        target_path: &str,
    ) -> Result<()> {
        // Only WAL-G / AWS envs go into the file — PG connection envs (PGHOST,
        // PGUSER, etc.) are not needed by wal-g archive/fetch from inside PG.
        let file_lines: Vec<String> = walg_env
            .iter()
            .filter(|line| line.starts_with("WALG_") || line.starts_with("AWS_"))
            .filter_map(|line| shell_export_assignment(line))
            .collect();
        self.write_file_lines_at(container_name, &file_lines, target_path)
            .await
    }

    /// Replace the file at `target_path` with `lines`, verbatim, owned by
    /// `postgres` and readable only by it (it holds credentials). Atomic:
    /// see [`atomic_write_command`].
    async fn write_file_lines_at(
        &self,
        container_name: &str,
        lines: &[String],
        target_path: &str,
    ) -> Result<()> {
        match self
            .write_file_lines_if_unchanged(container_name, lines, target_path, None)
            .await?
        {
            WalgEnvWrite::Written => Ok(()),
            // Without an expected checksum the file is always replaced.
            WalgEnvWrite::ChangedConcurrently => Err(EngineError::failed(
                &self.name,
                format!("walg.env on container '{container_name}' changed while it was written"),
            )),
        }
    }

    /// [`Self::write_file_lines_at`], but only if the file still has the
    /// content `expected_checksum` describes (see [`atomic_write_command`]).
    async fn write_file_lines_if_unchanged(
        &self,
        container_name: &str,
        lines: &[String],
        target_path: &str,
        expected_checksum: Option<&str>,
    ) -> Result<WalgEnvWrite> {
        use bollard::exec::{CreateExecOptions, StartExecOptions};

        let walg_env_path = target_path;
        let write_cmd = atomic_write_command(lines, walg_env_path, expected_checksum);

        let exec = self
            .docker
            .create_exec(
                container_name,
                CreateExecOptions {
                    cmd: Some(vec!["sh", "-c", &write_cmd]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    user: Some("postgres"),
                    ..Default::default()
                },
            )
            .await?;
        self.docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await?;

        loop {
            let inspect = self.docker.inspect_exec(&exec.id).await?;
            if inspect.running == Some(false) {
                match inspect.exit_code {
                    Some(0) => break,
                    Some(WALG_ENV_CHANGED_EXIT) if expected_checksum.is_some() => {
                        return Ok(WalgEnvWrite::ChangedConcurrently)
                    }
                    Some(WALG_ENV_LOCK_BUSY_EXIT) => {
                        return Err(EngineError::failed(
                            &self.name,
                            format!(
                                "Could not write {walg_env_path} on container '{container_name}': \
                                 another write held its lock for 30 seconds"
                            ),
                        ))
                    }
                    code => {
                        return Err(EngineError::failed(
                            &self.name,
                            format!(
                                "Failed to write {walg_env_path} on container '{container_name}' \
                                 (exit code {code:?}); the previous file is unchanged"
                            ),
                        ))
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        info!(
            "Written WAL-G credentials to {} on container '{}'",
            walg_env_path, container_name
        );
        Ok(WalgEnvWrite::Written)
    }

    /// The env file lives at `/var/lib/postgresql/walg.env` on the shared volume, so it:
    /// - Survives container restarts
    /// - Is accessible via `volumes_from` in helper containers
    /// - Is NOT inside PGDATA (so pg_basebackup/wal-g don't back it up — credentials
    ///   should not be stored inside backups)
    ///
    /// Flow:
    ///   1. Write `walg.env` onto the volume. From this moment, the volume
    ///      records "WAL-G is configured" — `compute_desired_enable_archiving`
    ///      will return true on every subsequent start.
    ///   2. Write `archive_command` via ALTER SYSTEM so an immediate
    ///      `wal-g wal-push` works on the running container (SIGHUP-reloadable).
    ///   3. Recreate the container so `archive_mode=on` lands in CMD args.
    ///      `archive_mode` is postmaster-context — recreate is the only way
    ///      to flip it. Volume is preserved; PGDATA is intact.
    pub(super) async fn enable_wal_archiving(
        &self,
        container_name: &str,
        walg_env: &[String],
        postgres_config: &PostgresConfig,
        allow_restart: bool,
    ) -> Result<WalArchivingOutcome> {
        use bollard::exec::{CreateExecOptions, StartExecOptions};

        if postgres_config.container_name.is_some() {
            let (mode, _) = self.archive_settings(postgres_config, None).await?;
            if mode == "off" {
                return Err(PostgresArchiveVerificationError {
                    service_name: self.name.clone(),
                    reason: "the imported database has archive_mode off. Enable archiving in its external startup configuration and restart it, or back up a new managed service; the imported container and data have not been changed".to_string(),
                }
                .into());
            }
        }

        // A managed service with archive_mode off will need its container
        // recreated (step 3). Refuse before writing anything if the new
        // container would not mount the data the current one uses (#1415):
        // a refusal must leave the service exactly as it was, and a stray
        // walg.env would make every later start try the same recreate.
        if postgres_config.container_name.is_none() {
            let (mode, _) = self.archive_settings(postgres_config, None).await?;
            if mode == "off" {
                if !allow_restart {
                    return Err(PostgresRestartNotConfirmed {
                        service_name: self.name.clone(),
                    }
                    .into());
                }
                if let Some(node) = &self.node {
                    self.node_archive_preflight(node, container_name).await?;
                } else {
                    let replacement = self.replacement_storage(postgres_config)?;
                    crate::externalsvc::recreate_guard::preflight(
                        &self.docker,
                        crate::externalsvc::recreate_guard::ManagedEngine::Postgres,
                        &self.name,
                        &self.get_container_name(),
                        &replacement,
                        "enabling WAL archiving",
                    )
                    .await?;
                }
            }
        }

        // Step 1: write walg.env onto the volume. This is the durable truth
        // source `compute_desired_enable_archiving` reads on every start.
        self.write_walg_env_file(container_name, walg_env).await?;
        let walg_env_path = "/var/lib/postgresql/walg.env";

        // Step 2: set archive_command via ALTER SYSTEM. SIGHUP-reloadable —
        // takes effect immediately. archive_mode comes in step 3 via CMD.
        let archive_command = format!(". {} && wal-g wal-push %p", walg_env_path);
        let alter_command_sql = format!(
            "ALTER SYSTEM SET archive_command = '{}'",
            archive_command.replace('\'', "''")
        );
        let reload_sql = "SELECT pg_reload_conf()";

        let password_env = format!("PGPASSWORD={}", postgres_config.password);
        let exec = self
            .docker
            .create_exec(
                container_name,
                CreateExecOptions {
                    cmd: Some(vec![
                        "psql",
                        "-U",
                        &postgres_config.username,
                        "-d",
                        &postgres_config.database,
                        "-c",
                        &alter_command_sql,
                        "-c",
                        reload_sql,
                    ]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    env: Some(vec![&password_env]),
                    ..Default::default()
                },
            )
            .await?;

        self.docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await?;

        loop {
            let inspect = self.docker.inspect_exec(&exec.id).await?;
            if inspect.running == Some(false) {
                if inspect.exit_code != Some(0) {
                    return Err(EngineError::failed(
                        &self.name,
                        format!(
                            "ALTER SYSTEM SET archive_command failed (exit code {:?})",
                            inspect.exit_code
                        ),
                    ));
                }
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        // Marker presence describes configured credentials, not active
        // PostgreSQL settings. Verify the reload before deciding whether a
        // postmaster restart is required; an ordinary backup must not cause
        // downtime when archiving already works.
        let (mode, command) = self
            .active_archive_settings(postgres_config, &archive_command)
            .await?;
        if matches!(mode.as_str(), "on" | "always") && command != archive_command {
            return Err(PostgresArchiveVerificationError {
                service_name: self.name.clone(),
                reason: "the WAL-G archive command did not apply after reload".to_string(),
            }
            .into());
        }
        if mode == "on" || mode == "always" {
            return Ok(WalArchivingOutcome { restarted: false });
        }
        // An operator can restart an imported workload while the settings
        // reload is in flight. Its physical container must never enter the
        // managed recreation path, regardless of either settings snapshot.
        if postgres_config.container_name.is_some() {
            return Err(PostgresArchiveVerificationError {
                service_name: self.name.clone(),
                reason: "the imported database now has archive_mode off. Enable archiving and restart it using its external lifecycle before retrying; Temps will not recreate this imported container".to_string(),
            }
            .into());
        }
        info!(
            "Wrote walg.env + archive_command in container '{}'. Recreating container so archive_mode=on lands in CMD.",
            container_name
        );

        // archive_mode can have been turned off since the check above (an
        // in-place restore finishing meanwhile): the restart is decided here,
        // so the caller's consent is enforced here too. Undo walg.env, or
        // every later start would try the same recreate.
        if !allow_restart {
            self.remove_walg_env(container_name).await;
            return Err(PostgresRestartNotConfirmed {
                service_name: self.name.clone(),
            }
            .into());
        }

        // Step 3: recreate so archive_mode=on lands in CMD args. A
        // command-line `-c archive_mode=off` outranks ALTER SYSTEM, so a
        // restart alone cannot turn it on for a container created with it.
        // The shared guard refuses unless the new container mounts the same
        // data, keeps the old container until the new one is verified to
        // hold the same cluster, and puts the old one back otherwise.
        warn!(
            service = %self.name,
            container = container_name,
            "Enabling WAL archiving restarts PostgreSQL: recreating its container with archive_mode=on"
        );
        let limits = self.resource_limits.read().await.clone();
        // A backup's provider has no persistence path for a new published
        // port. Preserve this registered endpoint instead of silently choosing
        // a different port on a bind conflict; its data volume remains intact.
        // On a worker the replacement is the running container's own
        // specification with archive_mode on (see `node_recreate`).
        let recreated = match &self.node {
            Some(node) => self.node_archive_recreate(node, container_name).await,
            None => self
                .recreate_container_guarded(
                    &self.docker,
                    postgres_config,
                    &limits,
                    true,
                    "enabling WAL archiving",
                )
                .await
                .map(|_| ()),
        };
        if let Err(e) = recreated {
            // The original container is back (or was never touched). Take
            // walg.env away again, or every later start would see "archiving
            // configured" and try the same recreate.
            self.remove_walg_env(container_name).await;
            return Err(e);
        }

        let (mode, command) = self
            .active_archive_settings(postgres_config, &archive_command)
            .await?;
        if !matches!(mode.as_str(), "on" | "always") || command != archive_command {
            return Err(PostgresArchiveVerificationError {
                service_name: self.name.clone(),
                reason: "the service restarted without active WAL-G archiving".to_string(),
            }
            .into());
        }

        info!(
            "Recreated container '{}' with archive_mode=on. WAL-G archiving active.",
            container_name
        );

        Ok(WalArchivingOutcome { restarted: true })
    }

    /// Remove `walg.env` from the service's volume, best effort.
    async fn remove_walg_env(&self, container_name: &str) {
        if let Err(cleanup) = crate::externalsvc::exec_util::run_exec(
            &self.docker,
            container_name,
            vec![
                "rm".to_string(),
                "-f".to_string(),
                "/var/lib/postgresql/walg.env".to_string(),
            ],
            None,
            Duration::from_secs(30),
        )
        .await
        {
            warn!(
                service = %self.name,
                container = container_name,
                error = %cleanup,
                "Could not remove walg.env after WAL archiving was not enabled"
            );
        }
    }

    /// Read settings from PostgreSQL itself, rather than inferring them
    /// from a credential file. Bounded control-plane work before a backup.
    pub(super) async fn active_archive_settings(
        &self,
        config: &PostgresConfig,
        expected_command: &str,
    ) -> std::result::Result<(String, String), PostgresArchiveVerificationError> {
        self.archive_settings(config, Some(expected_command)).await
    }

    async fn archive_settings(
        &self,
        config: &PostgresConfig,
        expected_command: Option<&str>,
    ) -> std::result::Result<(String, String), PostgresArchiveVerificationError> {
        let failure = |reason: String| PostgresArchiveVerificationError {
            service_name: self.name.clone(),
            reason,
        };
        let connection = format!(
            "postgres://{}:{}@{}:{}/{}?sslmode={}",
            urlencoding::encode(&config.username),
            urlencoding::encode(&config.password),
            config.host,
            config.port,
            urlencoding::encode(&config.database),
            urlencoding::encode(config.ssl_mode.as_deref().unwrap_or("disable")),
        );
        let pool = tokio::time::timeout(
            Duration::from_secs(5),
            sqlx::postgres::PgPoolOptions::new()
                .max_connections(1)
                .acquire_timeout(Duration::from_secs(5))
                .connect(&connection),
        )
        .await
        .map_err(|_| failure("database connection timed out after 5 seconds".to_string()))?
        .map_err(|error| failure(error.to_string()))?;
        // pg_reload_conf signals the postmaster asynchronously. Wait for
        // its reload to become visible before taking the setting snapshot.
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let settings: (String, String) = sqlx::query_as(
                    "SELECT current_setting('archive_mode'), current_setting('archive_command')",
                )
                .fetch_one(&pool)
                .await?;
                // PostgreSQL's archive_command show hook returns "(disabled)"
                // while archive_mode is off, even after ALTER SYSTEM and reload
                // applied the command. Return the disabled policy so the caller
                // can perform the postmaster restart; verify the visible command
                // once archiving is active.
                if settings.0 == "off"
                    || expected_command.is_none_or(|command| settings.1 == command)
                {
                    return Ok::<_, sqlx::Error>(settings);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await;
        pool.close().await;
        result
            .map_err(|_| {
                failure("the expected archive command did not apply within 5 seconds".to_string())
            })?
            .map_err(|error| failure(error.to_string()))
    }

    /// Ask the running database whether startup arguments override its
    /// archiving policy. Entrypoint wrappers can hide PostgreSQL arguments
    /// from Docker's CMD; reject them before fetching or replacing data.
    pub(super) async fn imported_archive_mode_source(
        &self,
        config: &PostgresConfig,
    ) -> std::result::Result<bool, PostgresArchiveVerificationError> {
        let failure = |reason: String| {
            PostgresArchiveVerificationError {
            service_name: self.name.clone(),
            reason: format!(
                "Cannot verify the imported container's startup archive policy before restore: {reason}; the original database has not been changed"
            ),
        }
        };
        let connection = format!(
            "postgres://{}:{}@{}:{}/{}?sslmode={}",
            urlencoding::encode(&config.username),
            urlencoding::encode(&config.password),
            config.host,
            config.port,
            urlencoding::encode(&config.database),
            urlencoding::encode(config.ssl_mode.as_deref().unwrap_or("disable")),
        );
        let pool = tokio::time::timeout(
            Duration::from_secs(5),
            sqlx::postgres::PgPoolOptions::new()
                .max_connections(1)
                .acquire_timeout(Duration::from_secs(5))
                .connect(&connection),
        )
        .await
        .map_err(|_| failure("database connection timed out after 5 seconds".to_string()))?
        .map_err(|error| failure(error.to_string()))?;
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            sqlx::query_as::<_, (String, String)>(
                "SELECT setting, source FROM pg_settings WHERE name = 'archive_mode'",
            )
            .fetch_one(&pool),
        )
        .await;
        pool.close().await;
        let (mode, source) = result
            .map_err(|_| failure("settings query timed out after 5 seconds".to_string()))?
            .map_err(|error| failure(error.to_string()))?;
        Ok(imported_archive_mode_is_command_line(&mode, &source))
    }

    /// Compute the desired `archive_mode` for this service's container CMD.
    ///
    /// Truth source: `/var/lib/postgresql/walg.env` existing on the
    /// service's data volume. WAL-G archiving is enabled iff that credential
    /// file is present — it's written by `enable_wal_archiving()` and lives
    /// on the persistent volume, so it survives container recreates, Temps
    /// restarts, and node failovers.
    ///
    /// On any inspection error, returns `false` (archiving off) — the safer
    /// default. A spurious `false` causes archiving to be disabled until the
    /// operator notices; a spurious `true` would cause WAL bloat, which is
    /// the exact bug we're trying to avoid.
    pub(super) async fn compute_desired_enable_archiving(&self) -> bool {
        let container_name = self.get_container_name();
        self.walg_env_exists_in_container(&container_name).await
    }

    /// Returns true iff `/var/lib/postgresql/walg.env` exists on this
    /// service's data volume. Docker's archive API can read a stopped
    /// container's mounted volume without pulling or executing a mutable
    /// helper image against database data.
    ///
    /// The live container is probed directly when it exists. Otherwise
    /// (fresh create, `force_recreate`, an externally-removed container, or
    /// node failover) there's no container to probe, but the volume can
    /// still hold a `walg.env` from a prior life -- probed via a throwaway
    /// container that only mounts the named volume and is never started
    /// (see `walg_env_exists_on_volume`).
    async fn walg_env_exists_in_container(&self, container_name: &str) -> bool {
        let live = self
            .docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                all: true,
                filters: Some(HashMap::from([(
                    "name".to_string(),
                    vec![crate::externalsvc::exact_container_name_filter(
                        container_name,
                    )],
                )])),
                ..Default::default()
            }))
            .await
            .map(|containers| !containers.is_empty())
            .unwrap_or(false);

        if live {
            return self.download_walg_env(container_name).await;
        }

        let volume_name = format!("{container_name}_data");
        let probe_image = self
            .config
            .read()
            .await
            .as_ref()
            .map(|c| c.docker_image.clone())
            .unwrap_or_else(|| {
                let (image, tag) = self.get_default_docker_image();
                format!("{image}:{tag}")
            });
        self.walg_env_exists_on_volume(&volume_name, &probe_image)
            .await
    }

    /// Reads `/var/lib/postgresql/walg.env` from a live container's
    /// filesystem via Docker's archive API.
    async fn download_walg_env(&self, container_name: &str) -> bool {
        use bollard::query_parameters::DownloadFromContainerOptions;
        use futures::StreamExt;

        let mut archive_stream = self.docker.download_from_container(
            container_name,
            Some(DownloadFromContainerOptions {
                path: "/var/lib/postgresql/walg.env".to_string(),
            }),
        );
        let mut saw_archive_data = false;
        while let Some(chunk) = archive_stream.next().await {
            match chunk {
                Ok(bytes) if !bytes.is_empty() => saw_archive_data = true,
                Ok(_) => {}
                Err(_) => return false,
            }
        }

        saw_archive_data
    }

    /// Probes `volume_name` for `walg.env` by creating a never-started
    /// container that mounts it at `/var/lib/postgresql`, reading it via
    /// the same archive API `download_walg_env` uses, then removing the
    /// probe container. The probe container is never started, so
    /// `probe_image`'s content never executes -- it only needs to exist so
    /// Docker can materialize the container's filesystem for the archive
    /// read. `probe_image` is the service's own already-vetted image
    /// (already local from the container that used to run against this
    /// volume), so this never pulls or trusts anything new.
    async fn walg_env_exists_on_volume(&self, volume_name: &str, probe_image: &str) -> bool {
        let probe_name = format!("{volume_name}-walg-probe");

        let _ = self
            .docker
            .remove_container(
                &probe_name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        let create_result = self
            .docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&probe_name)
                        .build(),
                ),
                bollard::models::ContainerCreateBody {
                    image: Some(probe_image.to_string()),
                    host_config: Some(bollard::models::HostConfig {
                        mounts: Some(vec![bollard::models::Mount {
                            target: Some("/var/lib/postgresql".to_string()),
                            source: Some(volume_name.to_string()),
                            typ: Some(bollard::models::MountType::VOLUME),
                            ..Default::default()
                        }]),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await;

        let exists = match create_result {
            Ok(_) => self.download_walg_env(&probe_name).await,
            Err(_) => false,
        };

        let _ = self
            .docker
            .remove_container(
                &probe_name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        exists
    }
}

pub(super) fn imported_archive_mode_is_command_line(mode: &str, source: &str) -> bool {
    matches!(mode, "on" | "always") && source == "command line"
}

pub(super) fn postgres_cmd_pins_archive_mode_on(command: &[String]) -> bool {
    command
        .iter()
        .flat_map(|part| part.split_whitespace())
        .filter_map(|part| {
            let part = part.trim_start_matches("--").trim_start_matches("-c");
            let (key, value) = part.split_once('=')?;
            (key.replace('-', "_").eq_ignore_ascii_case("archive_mode"))
                .then(|| value.trim_matches(['\'', '"']).to_ascii_lowercase())
        })
        .next_back()
        .is_some_and(|mode| matches!(mode.as_str(), "on" | "always" | "true" | "yes" | "1"))
}

impl PostgresService {
    /// Point continuous WAL-G archiving at `s3_credentials`/`walg_prefix`
    /// explicitly. The normal backup path uses the pinned destination;
    /// repointing accepts a different destination after an audited decision.
    ///
    /// Only the explicit, operator-initiated WAL archive source repoint
    /// (`ExternalServiceManager::repoint_continuous_archive_source`) should call
    /// this. Ordinary backups are guarded by the persisted archive source pin.
    pub async fn force_reenable_continuous_archiving(
        &self,
        service_config: ServiceConfig,
        s3_credentials: &crate::externalsvc::S3Credentials,
        walg_prefix: &str,
    ) -> Result<()> {
        self.write_wal_archiving_config(service_config, s3_credentials, walg_prefix, true)
            .await
            .map(|_| ())
    }

    /// [`ExternalService::enable_continuous_archiving`], also reporting
    /// whether PostgreSQL had to be restarted, so a backup can say so. With
    /// `allow_restart` false it never restarts PostgreSQL: when turning
    /// archiving on would need one it returns [`PostgresRestartNotConfirmed`]
    /// and changes nothing.
    pub async fn enable_continuous_archiving_reporting(
        &self,
        service_config: ServiceConfig,
        s3_credentials: &crate::externalsvc::S3Credentials,
        walg_prefix: &str,
        allow_restart: bool,
    ) -> Result<WalArchivingOutcome> {
        self.write_wal_archiving_config(service_config, s3_credentials, walg_prefix, allow_restart)
            .await
    }

    /// Whether running a WAL-G backup now would restart PostgreSQL: true when
    /// this managed service has archive_mode off, so enabling archiving has
    /// to recreate its container. Bounded: one SQL round trip.
    pub async fn backup_requires_restart(&self, service_config: ServiceConfig) -> Result<bool> {
        let postgres_config = self.get_postgres_config(service_config)?;
        if postgres_config.container_name.is_some() {
            // Imported services are never recreated; enabling refuses instead.
            return Ok(false);
        }
        let (mode, _) = self.archive_settings(&postgres_config, None).await?;
        Ok(mode == "off")
    }

    /// When the restart [`Self::backup_requires_restart`] reports cannot be
    /// done safely (the new container would not see this service's data),
    /// the guard's refusal, so a client can say the backup will be refused
    /// instead of promising a restart. Read-only.
    pub async fn backup_restart_refusal(
        &self,
        service_config: ServiceConfig,
    ) -> Result<Option<String>> {
        let postgres_config = self.get_postgres_config(service_config)?;
        if postgres_config.container_name.is_some() {
            return Ok(None);
        }
        if let Some(node) = &self.node {
            let container_name = self.get_live_container_name(&postgres_config);
            return match self.node_archive_preflight(node, &container_name).await {
                Ok(()) => Ok(None),
                Err(EngineError::RecreateGuard(e)) if e.is_refusal() => Ok(Some(e.to_string())),
                Err(e) => Err(e),
            };
        }
        let replacement = self.replacement_storage(&postgres_config)?;
        match crate::externalsvc::recreate_guard::preflight(
            &self.docker,
            crate::externalsvc::recreate_guard::ManagedEngine::Postgres,
            &self.name,
            &self.get_container_name(),
            &replacement,
            "enabling WAL archiving",
        )
        .await
        {
            Ok(_) => Ok(None),
            Err(e) if e.is_refusal() => Ok(Some(e.to_string())),
            Err(e) => Err(e.into()),
        }
    }

    pub(super) async fn write_wal_archiving_config(
        &self,
        service_config: ServiceConfig,
        s3_credentials: &crate::externalsvc::S3Credentials,
        walg_prefix: &str,
        allow_restart: bool,
    ) -> Result<WalArchivingOutcome> {
        // Backup engines construct a fresh provider. Hydrate its limits from
        // the registered service before an off-policy managed recreation so
        // enabling archiving cannot remove the workload's resource bounds.
        let limits = ServiceResourceLimits::from_parameters(&service_config.parameters);
        limits
            .validate()
            .map_err(|reason| PostgresArchiveVerificationError {
                service_name: self.name.clone(),
                reason: format!("invalid WAL archive service resource limits: {reason}"),
            })?;
        *self.resource_limits.write().await = limits;
        let postgres_config = self.get_postgres_config(service_config)?;
        let container_name = self.get_live_container_name(&postgres_config);
        let walg_env = self
            .archive_walg_env(&container_name, s3_credentials, walg_prefix)
            .await;
        // Prove the target accepts a write with exactly these settings
        // before archiving depends on them. Turning archiving on against a
        // target WAL-G cannot reach does not fail loudly later: PostgreSQL
        // keeps every segment until the disk is full.
        self.check_archive_target_env(&container_name, &walg_env, s3_credentials)
            .await?;
        self.enable_wal_archiving(&container_name, &walg_env, &postgres_config, allow_restart)
            .await
    }

    /// Check that WAL-G can write to the archive target of this service
    /// (see [`archive_target_check_script`]), with the settings `walg.env`
    /// would hold, without writing `walg.env`. Read-only for the database. The outer error is a
    /// service that cannot be checked at all; the inner one is the target's
    /// answer.
    pub async fn check_archive_target(
        &self,
        service_config: ServiceConfig,
        s3_credentials: &crate::externalsvc::S3Credentials,
        walg_prefix: &str,
    ) -> std::result::Result<
        std::result::Result<ArchiveTargetCheck, Box<ArchiveTargetCheckError>>,
        EngineError,
    > {
        let postgres_config = self.get_postgres_config(service_config)?;
        let container_name = self.get_live_container_name(&postgres_config);
        let walg_env = self
            .archive_walg_env(&container_name, s3_credentials, walg_prefix)
            .await;
        Ok(self
            .check_archive_target_env(&container_name, &walg_env, s3_credentials)
            .await)
    }

    async fn check_archive_target_env(
        &self,
        container_name: &str,
        walg_env: &[String],
        s3_credentials: &crate::externalsvc::S3Credentials,
    ) -> std::result::Result<ArchiveTargetCheck, Box<ArchiveTargetCheckError>> {
        let settings = archive_target_settings(walg_env, s3_credentials);
        let failure = |message: String| {
            Box::new(ArchiveTargetCheckError {
                service_name: self.name.clone(),
                endpoint: settings.endpoint.clone(),
                scheme: settings.scheme.clone(),
                force_path_style: settings.force_path_style,
                path_style_reason: settings.path_style_reason.clone(),
                message,
            })
        };
        let mut env: Vec<String> = walg_env
            .iter()
            .filter(|line| line.starts_with("WALG_") || line.starts_with("AWS_"))
            .cloned()
            .collect();
        env.push("S3_MAX_RETRIES=1".to_string());
        let script = archive_target_check_script();
        let redact = crate::externalsvc::SensitiveValues::new().s3_credentials(s3_credentials);
        let result = crate::externalsvc::exec_util::run_exec(
            &self.docker,
            container_name,
            vec!["sh".to_string(), "-c".to_string(), script],
            Some(env),
            archive_target_check_exec_deadline(),
        )
        .await
        .map_err(|error| failure(redact.redact(&format!("could not run the check: {error}"))))?;
        let (output, exit_code) = split_check_exit(&result.output);
        let result = crate::externalsvc::exec_util::ExecResult {
            exit_code: exit_code.unwrap_or(result.exit_code),
            output,
        };
        match result.exit_code {
            0 => {
                info!(
                    service = %self.name,
                    endpoint = %settings.endpoint,
                    scheme = %settings.scheme,
                    force_path_style = settings.force_path_style,
                    "WAL archive target accepted a WAL-G write check"
                );
                Ok(settings)
            }
            124 => Err(failure(format!(
                "no answer within {ARCHIVE_TARGET_CHECK_TIMEOUT_SECS} seconds ({})",
                summarize_walg_output(&redact.redact(&result.output))
            ))),
            code => {
                let message = summarize_walg_output(&redact.redact(&result.output));
                warn!(
                    service = %self.name,
                    endpoint = %settings.endpoint,
                    scheme = %settings.scheme,
                    force_path_style = settings.force_path_style,
                    exit_code = code,
                    error = %message,
                    "WAL archive target refused a WAL-G write check"
                );
                Err(failure(message))
            }
        }
    }

    /// The variables `archive_command` reads from `walg.env`.
    async fn archive_walg_env(
        &self,
        container_name: &str,
        s3_credentials: &crate::externalsvc::S3Credentials,
        walg_prefix: &str,
    ) -> Vec<String> {
        let resolved_endpoint = s3_credentials
            .resolve_endpoint_for_container(&self.docker, container_name)
            .await;
        archive_walg_env_lines(s3_credentials, walg_prefix, resolved_endpoint.as_deref())
    }

    /// Replace the credential in `walg.env` with a fresh one for the same
    /// repository, and change nothing else: no `ALTER SYSTEM`, no reload, no
    /// restart (`archive_command` reads the file on every segment). A worker's
    /// archiving credential is short-lived and the control plane refreshes it
    /// this way (ADR-102).
    ///
    /// Returns `false`, writing nothing, when archiving is not set up here
    /// (no `walg.env` on the volume): a refresh never turns archiving on.
    pub async fn refresh_archive_credentials(
        &self,
        service_config: ServiceConfig,
        s3_credentials: &crate::externalsvc::S3Credentials,
        walg_prefix: &str,
    ) -> Result<bool> {
        let postgres_config = self.get_postgres_config(service_config)?;
        let container_name = self.get_live_container_name(&postgres_config);
        let probe = crate::externalsvc::exec_util::run_exec(
            &self.docker,
            &container_name,
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "if [ -f /var/lib/postgresql/walg.env ]; then echo configured; else echo absent; fi"
                    .to_string(),
            ],
            None,
            std::time::Duration::from_secs(30),
        )
        .await?;
        if !probe.output.contains("configured") {
            return Ok(false);
        }
        let walg_env = self
            .archive_walg_env(&container_name, s3_credentials, walg_prefix)
            .await;
        self.write_walg_env_file(&container_name, &walg_env).await?;
        Ok(true)
    }
}

/// The lines of `walg.env`, the file `archive_command` sources before every
/// `wal-g wal-push`. `resolved_endpoint` is the endpoint as the container
/// reaches it ([`crate::externalsvc::S3Credentials::resolve_endpoint_for_container`]),
/// always with an explicit scheme: WAL-G reads a scheme-less `AWS_ENDPOINT`
/// as HTTPS.
pub(crate) fn archive_walg_env_lines(
    s3_credentials: &crate::externalsvc::S3Credentials,
    walg_prefix: &str,
    resolved_endpoint: Option<&str>,
) -> Vec<String> {
    let mut walg_env: Vec<String> = vec![
        format!("WALG_S3_PREFIX={}", walg_prefix),
        format!("AWS_ACCESS_KEY_ID={}", s3_credentials.access_key_id),
        format!("AWS_SECRET_ACCESS_KEY={}", s3_credentials.secret_key),
        format!("AWS_REGION={}", s3_credentials.region),
    ];
    // Absent unless this source holds a temporary (STS-style) credential, so
    // a long-lived one produces the exact environment it always did.
    walg_env.extend(s3_credentials.session_token_env());
    if let Some(resolved_endpoint) = resolved_endpoint {
        walg_env.push(format!(
            "AWS_ENDPOINT={}",
            temps_core::s3_endpoint::endpoint_url(resolved_endpoint)
        ));
    }
    // Virtual-hosted addressing (`<bucket>.<host>`) cannot resolve for an IP
    // literal: decide on the endpoint the container actually dials as well
    // as the stored one.
    if s3_credentials.uses_path_style()
        || resolved_endpoint.is_some_and(temps_core::s3_endpoint::endpoint_host_is_ip_literal)
    {
        walg_env.push("AWS_S3_FORCE_PATH_STYLE=true".to_string());
    }
    walg_env
}

/// How `live` (a container's `walg.env`) differs from `generated` in what
/// decides where and how WAL-G connects: the endpoint and path style.
/// Credentials are never compared or named.
pub(crate) fn walg_env_drift(live: &[String], generated: &[String]) -> Option<String> {
    fn endpoint(lines: &[String]) -> Option<&str> {
        lines
            .iter()
            .find_map(|line| line.strip_prefix("AWS_ENDPOINT="))
    }
    fn path_style(lines: &[String]) -> bool {
        lines.iter().any(|line| {
            line.strip_prefix("AWS_S3_FORCE_PATH_STYLE=")
                .is_some_and(|value| value.eq_ignore_ascii_case("true"))
        })
    }
    fn on_off(on: bool) -> &'static str {
        if on {
            "on"
        } else {
            "off"
        }
    }
    let mut differences = Vec::new();
    let (live_endpoint, generated_endpoint) = (endpoint(live), endpoint(generated));
    if live_endpoint != generated_endpoint {
        let describe = |endpoint: Option<&str>| match endpoint {
            Some(endpoint) if !endpoint.contains("://") => {
                format!("{endpoint} (no scheme, so WAL-G uses https)")
            }
            Some(endpoint) => endpoint.to_string(),
            None => "AWS S3".to_string(),
        };
        differences.push(format!(
            "sends WAL to {} instead of {}",
            describe(live_endpoint),
            describe(generated_endpoint)
        ));
    }
    let (live_path_style, generated_path_style) = (path_style(live), path_style(generated));
    if live_path_style != generated_path_style {
        differences.push(format!(
            "has path-style addressing {} instead of {}",
            on_off(live_path_style),
            on_off(generated_path_style)
        ));
    }
    if differences.is_empty() {
        return None;
    }
    let fix = if repair_walg_env_lines(live).is_some() {
        "Restart the service: Temps repairs this walg.env when it starts, without changing \
         where WAL is archived"
    } else {
        "Run a backup of the service to rewrite walg.env from its S3 source"
    };
    Some(format!(
        "The running container's walg.env {}, and PostgreSQL archives with that file. {fix}.",
        differences.join(" and ")
    ))
}

/// Signals every process whose command line starts with `wal-g wal-push `
/// (the images ship no `pkill`) and prints `ended <pid>` for each.
const STALE_WAL_PUSH_SCRIPT: &str = "for p in /proc/[0-9]*; do \
     case \"$(tr '\\0' ' ' < \"$p/cmdline\" 2>/dev/null)\" in \
       'wal-g wal-push '*) kill \"${p#/proc/}\" 2>/dev/null && echo \"ended ${p#/proc/}\";; \
     esac; \
   done; exit 0";

/// Bring a `walg.env` written by an older Temps up to the current rule, or
/// `None` when it already follows it.
///
/// `lines` are the file's `KEY=value` assignments as the shell sees them.
/// A scheme-less `AWS_ENDPOINT` gets the `http://` the source was validated
/// with, and an IP-literal endpoint gets `AWS_S3_FORCE_PATH_STYLE=true`.
/// Credentials, prefix and every other assignment are unchanged: this never
/// changes which repository WAL is archived to. The file itself is patched
/// by [`patch_walg_env_file`], which keeps every line it does not repair.
pub(crate) fn repair_walg_env_lines(lines: &[String]) -> Option<Vec<String>> {
    let endpoint = lines
        .iter()
        .find_map(|line| line.strip_prefix("AWS_ENDPOINT="))?;
    let normalized = temps_core::s3_endpoint::endpoint_url(endpoint);
    let needs_scheme = normalized != endpoint;
    let has_path_style = lines.iter().any(|line| {
        line.strip_prefix("AWS_S3_FORCE_PATH_STYLE=")
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    });
    let needs_path_style =
        !has_path_style && temps_core::s3_endpoint::endpoint_host_is_ip_literal(endpoint);
    if !needs_scheme && !needs_path_style {
        return None;
    }
    let mut repaired: Vec<String> = lines
        .iter()
        .filter(|line| !(needs_path_style && line.starts_with("AWS_S3_FORCE_PATH_STYLE=")))
        .map(|line| {
            if line.starts_with("AWS_ENDPOINT=") {
                format!("AWS_ENDPOINT={normalized}")
            } else {
                line.clone()
            }
        })
        .collect();
    if needs_path_style {
        repaired.push("AWS_S3_FORCE_PATH_STYLE=true".to_string());
    }
    Some(repaired)
}

/// The two settings the start-time repair rewrites.
const REPAIRED_WALG_KEYS: [&str; 2] = ["AWS_ENDPOINT", "AWS_S3_FORCE_PATH_STYLE"];

/// Whether a `walg.env` line assigns `key` (`KEY=…` or `export KEY=…`).
fn assigns(line: &str, key: &str) -> bool {
    let line = line.trim_start();
    let line = match line.strip_prefix("export") {
        Some(rest) if rest.starts_with([' ', '\t']) => rest.trim_start(),
        _ => line,
    };
    line.strip_prefix(key)
        .is_some_and(|rest| rest.starts_with('='))
}

/// The lines of `walg.env` after the repair: every line of `raw` kept
/// byte for byte (comments, other variables such as an operator's
/// `S3_MAX_RETRIES`, quoting) except the `AWS_ENDPOINT` and
/// `AWS_S3_FORCE_PATH_STYLE` assignments, which are replaced by those in
/// `repaired` (from [`repair_walg_env_lines`]) at the end of the file, where
/// they also win over any assignment this did not recognise.
pub(crate) fn patch_walg_env_file(raw: &str, repaired: &[String]) -> Vec<String> {
    let mut lines: Vec<String> = raw
        .lines()
        .filter(|line| !REPAIRED_WALG_KEYS.iter().any(|key| assigns(line, key)))
        .map(str::to_string)
        .collect();
    lines.extend(
        repaired
            .iter()
            .filter(|line| {
                REPAIRED_WALG_KEYS.iter().any(|key| {
                    line.strip_prefix(key)
                        .is_some_and(|rest| rest.starts_with('='))
                })
            })
            .filter_map(|line| shell_export_assignment(line)),
    );
    lines
}

impl PostgresService {
    /// End the `wal-g wal-push` processes running in the container, after
    /// `walg.env` was repaired.
    ///
    /// PostgreSQL starts archiving as soon as it is up, before Temps repairs
    /// the file, so the first attempt runs with the old settings, and
    /// WAL-G retrying an endpoint it cannot reach holds that attempt open for
    /// up to a quarter of an hour. Ending it is safe: PostgreSQL counts it as
    /// a failed attempt, keeps the segment and retries it, now with the
    /// repaired file. Only processes whose command line starts with
    /// `wal-g wal-push ` are signalled. Best effort.
    async fn end_stale_wal_pushes(&self, container_name: &str) {
        let result = crate::externalsvc::exec_util::run_exec(
            &self.docker,
            container_name,
            vec![
                "sh".to_string(),
                "-c".to_string(),
                STALE_WAL_PUSH_SCRIPT.to_string(),
            ],
            None,
            Duration::from_secs(15),
        )
        .await;
        match result {
            Ok(result) => {
                let ended = result
                    .output
                    .lines()
                    .filter(|line| line.starts_with("ended "))
                    .count();
                if ended > 0 {
                    info!(
                        service = %self.name,
                        container = container_name,
                        ended,
                        "Ended wal-push attempts started with the old walg.env; PostgreSQL \
                         retries those segments with the repaired file"
                    );
                }
            }
            Err(error) => warn!(
                service = %self.name,
                container = container_name,
                error = %error,
                "Could not end wal-push attempts started with the old walg.env; archiving \
                 resumes once they give up"
            ),
        }
    }

    /// The `WALG_`/`AWS_` assignments of the container's `walg.env` as the
    /// shell sees them, `None` when there is no file (archiving was never
    /// configured on this container).
    async fn read_live_walg_env(
        &self,
        container_name: &str,
    ) -> std::result::Result<Option<Vec<String>>, String> {
        Ok(self
            .read_walg_env_snapshot(container_name)
            .await?
            .map(|snapshot| snapshot.evaluated))
    }

    /// One consistent [`WalgEnvSnapshot`] of the container's `walg.env`, or
    /// `None` when there is no file. `env -i`: only what the file sets, not
    /// the container's own environment.
    pub(crate) async fn read_walg_env_snapshot(
        &self,
        container_name: &str,
    ) -> std::result::Result<Option<WalgEnvSnapshot>, String> {
        let result = crate::externalsvc::exec_util::run_exec(
            &self.docker,
            container_name,
            vec![
                "sh".to_string(),
                "-c".to_string(),
                WALG_ENV_SNAPSHOT_SCRIPT.to_string(),
            ],
            None,
            Duration::from_secs(15),
        )
        .await
        .map_err(|error| error.to_string())?;
        if result.exit_code != 0 {
            return Err(format!("reading walg.env exited with {}", result.exit_code));
        }
        parse_walg_env_snapshot(&result.output)
    }

    /// How the running container's `walg.env` differs from the settings
    /// Temps generates for its archive target, if it does. `None` when they
    /// agree, when there is no file, or when it cannot be read (a check
    /// must not fail because of this comparison).
    pub async fn live_walg_env_drift(
        &self,
        service_config: ServiceConfig,
        s3_credentials: &crate::externalsvc::S3Credentials,
        walg_prefix: &str,
    ) -> Option<String> {
        let postgres_config = self.get_postgres_config(service_config).ok()?;
        let container_name = self.get_live_container_name(&postgres_config);
        let live = match self.read_live_walg_env(&container_name).await {
            Ok(live) => live?,
            Err(error) => {
                debug!(
                    service = %self.name,
                    container = %container_name,
                    error = %error,
                    "Could not read walg.env to compare it with the archive target settings"
                );
                return None;
            }
        };
        let generated = self
            .archive_walg_env(&container_name, s3_credentials, walg_prefix)
            .await;
        walg_env_drift(&live, &generated)
    }

    /// Repair the `walg.env` an existing service carries, on start.
    ///
    /// Installs that predate explicit endpoint schemes have a `walg.env`
    /// whose `AWS_ENDPOINT` is a bare `host:port`, which WAL-G dials over
    /// HTTPS: every `wal-push` to a plain-HTTP store fails and PostgreSQL
    /// keeps every segment. `archive_command` sources the file per segment,
    /// so rewriting it takes effect on the next segment, with no reload and
    /// no restart. Best effort: a failure is logged and the start goes on,
    /// leaving the file as it was; the next backup rewrites it from the S3
    /// source anyway.
    pub(super) async fn repair_walg_env_on_start(&self, container_name: &str) {
        // One snapshot: what is checked, patched and compared before the
        // write all describe the same file.
        let snapshot = match self.read_walg_env_snapshot(container_name).await {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => return,
            Err(error) => {
                warn!(
                    service = %self.name,
                    container = container_name,
                    error = %error,
                    "Could not read walg.env to check its S3 endpoint; leaving it unchanged"
                );
                return;
            }
        };
        let Some(repaired) = repair_walg_env_lines(&snapshot.evaluated) else {
            return;
        };
        // Patched, not regenerated, so nothing else in the file is lost; and
        // written only if nobody rewrote the file since the snapshot.
        let patched = patch_walg_env_file(&snapshot.raw, &repaired);
        match self
            .write_file_lines_if_unchanged(
                container_name,
                &patched,
                "/var/lib/postgresql/walg.env",
                Some(&snapshot.checksum),
            )
            .await
        {
            Ok(WalgEnvWrite::Written) => {
                info!(
                    service = %self.name,
                    container = container_name,
                    "Repaired walg.env: AWS_ENDPOINT now carries an explicit scheme and path \
                     style is forced for an IP endpoint; WAL archiving uses it from the next \
                     attempt"
                );
                self.end_stale_wal_pushes(container_name).await;
            }
            Ok(WalgEnvWrite::ChangedConcurrently) => info!(
                service = %self.name,
                container = container_name,
                "walg.env was rewritten (by a backup or repoint) while it was being repaired; \
                 keeping the newer file, which Temps generated with the current rules"
            ),
            Err(error) => warn!(
                service = %self.name,
                container = container_name,
                error = %error,
                "Could not rewrite walg.env with an explicit S3 endpoint scheme; WAL archiving \
                 may still be failing until the next backup rewrites it"
            ),
        }
    }
}

#[cfg(test)]
mod tests;
