// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Automatic control-plane database backup taken right before an upgrade
//! applies schema migrations.
//!
//! Migrations are forward-only: once a newer release has migrated the
//! database, the only safe way back to the previous release is to restore the
//! database as it was before the upgrade. This module takes that snapshot.
//!
//! ## When it runs
//!
//! Only when the startup schema check reports pending migrations on a
//! database that already has applied ones (an upgrade). A fresh install and a
//! plain restart with nothing pending never reach this code, so a normal
//! restart pays nothing.
//!
//! ## What it writes
//!
//! A `pg_dump --format=custom` archive (compressed, restorable with
//! `pg_restore`) plus a JSON manifest, in `<data dir>/backups/pre-migration/`:
//!
//! ```text
//! temps-pre-migration-20261004T101500Z-v0.1.0-beta.57.dump
//! temps-pre-migration-20261004T101500Z-v0.1.0-beta.57.json
//! ```
//!
//! The dump has the same scope as the control-plane backup engine
//! ([`crate::engines::control_plane`]): the schema of every table, and the
//! data of every table except the high-volume telemetry tables (proxy logs,
//! traces, analytics events, ...), which are dumped schema-only. That keeps an
//! upgrade of a large installation from waiting on gigabytes of telemetry. The
//! exclusion set is resolved by the same
//! [`resolve_excluded_data`](crate::engines::control_plane::resolve_excluded_data)
//! so the dump stays restorable.
//!
//! Only the newest [`PRE_MIGRATION_BACKUPS_KEPT`] backups are kept.
//!
//! ## How it dumps
//!
//! 1. A `pg_dump` on `PATH` whose major version is at least the server's.
//! 2. Otherwise a one-shot `postgres:<server major>` container on the host
//!    network, like the control-plane engine. The archive is copied out
//!    through the Docker API, so this also works when Temps itself runs in a
//!    container.
//!
//! The password is passed through `PGPASSWORD`, never on a command line.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, Statement};
use serde::Serialize;
use tracing::{debug, info, warn};

use crate::engines::control_plane::resolve_excluded_data;

/// Directory, relative to the data dir, holding pre-migration backups.
pub const PRE_MIGRATION_BACKUP_DIR: &str = "backups/pre-migration";

/// How many pre-migration backups are kept (the newest ones).
pub const PRE_MIGRATION_BACKUPS_KEPT: usize = 3;

/// Upper bound on one dump. Telemetry data is excluded, so this is only
/// reached by a very large configuration database or a stuck connection.
const DUMP_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Bound on probing the Docker daemon before choosing the container path.
const DOCKER_PING_TIMEOUT: Duration = Duration::from_secs(5);

const FILE_PREFIX: &str = "temps-pre-migration-";
const DUMP_EXTENSION: &str = "dump";
const MANIFEST_EXTENSION: &str = "json";
const PARTIAL_SUFFIX: &str = ".partial";
const ENGINE_LABEL: &str = "pre_migration";
const MAX_STDERR_BYTES: usize = 4096;
/// Where `pg_dump` writes inside the one-shot container.
const CONTAINER_DUMP_PATH: &str = "/tmp/temps-pre-migration.dump";

/// libpq URI query parameters forwarded to `pg_dump`. Anything else in the
/// Temps database URL (driver-specific options) is not a libpq parameter and
/// would make `pg_dump` reject the URI.
const LIBPQ_QUERY_PARAMS: &[&str] = &[
    "sslmode",
    "sslrootcert",
    "sslcert",
    "sslkey",
    "sslcrl",
    "sslnegotiation",
    "options",
    "application_name",
    "connect_timeout",
    "target_session_attrs",
    "channel_binding",
    "gssencmode",
    "keepalives",
    "keepalives_idle",
];

/// Why a pre-migration backup could not be taken.
#[derive(Debug, thiserror::Error)]
pub enum PreMigrationBackupError {
    #[error("The database URL cannot be used for the pre-migration backup: {reason}")]
    InvalidDatabaseUrl { reason: String },

    #[error("Could not prepare the pre-migration backup directory {path}: {source}")]
    Directory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Could not read the PostgreSQL server version for the pre-migration backup: {source}")]
    ServerVersion {
        #[source]
        source: DbErr,
    },

    #[error(
        "Could not resolve which telemetry tables the pre-migration backup dumps \
         schema-only: {reason}"
    )]
    ExcludedTables { reason: String },

    #[error(
        "No pg_dump able to back up this PostgreSQL {server_major} server is available. \
         Host: {host}. Docker: {docker}. Install the PostgreSQL {server_major} client tools \
         (pg_dump {server_major} or newer) on this machine, or make the Docker daemon \
         reachable so a postgres:{server_major} container can run the dump."
    )]
    NoDumpTool {
        server_major: u32,
        host: String,
        docker: String,
    },

    #[error("Could not run {method} to write {path}: {reason}")]
    DumpLaunch {
        method: String,
        path: PathBuf,
        reason: String,
    },

    #[error("{method} exited with code {exit_code} while writing {path}: {stderr}")]
    DumpFailed {
        method: String,
        path: PathBuf,
        exit_code: i64,
        stderr: String,
    },

    #[error("{method} did not finish within {timeout_secs}s while writing {path}")]
    Timeout {
        method: String,
        path: PathBuf,
        timeout_secs: u64,
    },

    #[error("{method} reported success but produced no data at {path}")]
    EmptyDump { method: String, path: PathBuf },

    #[error("Could not finalize pre-migration backup {path}: {source}")]
    Finalize {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// What the caller knows about the upgrade being backed up.
pub struct PreMigrationBackupRequest<'a> {
    /// Connection used to read the server version and resolve exclusions.
    pub db: &'a DatabaseConnection,
    /// Database URL `pg_dump` connects with.
    pub database_url: &'a str,
    /// Temps data directory; the backup goes to [`PRE_MIGRATION_BACKUP_DIR`] in it.
    pub data_dir: &'a Path,
    /// Version of the binary about to migrate (recorded in the file name).
    pub binary_version: &'a str,
    /// Version of the last successful start before this upgrade, when known:
    /// the release to roll back to with this backup.
    pub previous_version: Option<&'a str>,
    /// Number of migrations already applied.
    pub applied_migrations: usize,
    /// Migrations about to be applied, in order.
    pub pending_migrations: &'a [String],
}

/// How the dump was produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DumpMethod {
    /// A `pg_dump` found on `PATH`.
    HostPgDump { binary: PathBuf, major: u32 },
    /// A one-shot container of this image.
    Docker { image: String },
}

impl std::fmt::Display for DumpMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DumpMethod::HostPgDump { binary, major } => {
                write!(f, "pg_dump {major} ({})", binary.display())
            }
            DumpMethod::Docker { image } => write!(f, "pg_dump in a {image} container"),
        }
    }
}

/// A completed pre-migration backup.
#[derive(Debug, Clone)]
pub struct PreMigrationBackup {
    /// The `pg_restore`-able archive.
    pub dump_path: PathBuf,
    /// Its JSON manifest.
    pub manifest_path: PathBuf,
    pub size_bytes: u64,
    pub method: DumpMethod,
    pub elapsed: Duration,
    /// Older backups removed by retention.
    pub pruned: Vec<PathBuf>,
}

/// Written next to every dump so an operator can tell which upgrade it
/// precedes without opening it.
#[derive(Debug, Serialize)]
struct PreMigrationManifest<'a> {
    format_version: u32,
    /// ISO 8601, `Z` suffix.
    created_at: String,
    binary_version: &'a str,
    /// The release that last ran against this database, when known.
    previous_version: Option<&'a str>,
    server_version_num: i32,
    applied_migrations: usize,
    pending_migrations: &'a [String],
    dump_file: String,
    dump_format: &'static str,
    size_bytes: u64,
    method: String,
    /// Tables whose rows are NOT in the dump (schema only).
    excluded_table_data: &'a [String],
}

/// The directory pre-migration backups are written to.
pub fn pre_migration_backup_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(PRE_MIGRATION_BACKUP_DIR)
}

/// Take the pre-migration backup, then apply retention.
pub async fn create_pre_migration_backup(
    request: PreMigrationBackupRequest<'_>,
) -> Result<PreMigrationBackup, PreMigrationBackupError> {
    let started = Instant::now();
    let connection = DumpConnection::from_database_url(request.database_url)?;
    let dir = pre_migration_backup_dir(request.data_dir);
    prepare_directory(&dir)?;
    // The OS releases this lock even if an attempt is killed. Hold it across
    // cleanup, writing and retention so another process cannot unlink our dump.
    let _backup_lock = acquire_backup_lock(&dir)?;
    remove_incomplete_backups(&dir);

    let server_version_num = read_server_version_num(request.db).await?;
    let server_major = server_major(server_version_num);

    let excluded = resolve_excluded_data(request.db).await.map_err(|error| {
        PreMigrationBackupError::ExcludedTables {
            reason: error.to_string(),
        }
    })?;

    let created_at = Utc::now();
    // Rapid retries must never overwrite the last restorable backup.
    let stem = format!(
        "{}-{}",
        backup_file_stem(created_at, request.binary_version),
        uuid::Uuid::new_v4()
    );
    let dump_name = format!("{stem}.{DUMP_EXTENSION}");
    let partial_name = format!("{dump_name}{PARTIAL_SUFFIX}");
    let dump_path = dir.join(&dump_name);
    let partial_path = dir.join(&partial_name);

    let method = choose_dump_method(server_major).await?;
    info!(
        path = %dump_path.display(),
        method = %method,
        pending_migrations = request.pending_migrations.len(),
        schema_only_tables = excluded.tables.len(),
        "Taking pre-migration database backup"
    );

    let dump_result = match &method {
        DumpMethod::HostPgDump { binary, .. } => {
            let args = pg_dump_args(
                &connection.uri,
                &partial_path.display().to_string(),
                &excluded.patterns,
            );
            run_host_pg_dump(
                binary,
                args,
                connection.password.as_deref(),
                &method,
                &dump_path,
            )
            .await
        }
        DumpMethod::Docker { image } => {
            let args = pg_dump_args(&connection.uri, CONTAINER_DUMP_PATH, &excluded.patterns);
            run_docker_pg_dump(
                image,
                args,
                connection.password.as_deref(),
                &method,
                &partial_path,
                &dump_path,
            )
            .await
        }
    };
    if let Err(error) = dump_result {
        remove_quietly(&partial_path).await;
        return Err(error);
    }

    let size_bytes = match tokio::fs::metadata(&partial_path).await {
        Ok(meta) if meta.len() > 0 => meta.len(),
        Ok(_) | Err(_) => {
            remove_quietly(&partial_path).await;
            return Err(PreMigrationBackupError::EmptyDump {
                method: method.to_string(),
                path: dump_path,
            });
        }
    };
    tokio::fs::rename(&partial_path, &dump_path)
        .await
        .map_err(|source| PreMigrationBackupError::Finalize {
            path: dump_path.clone(),
            source,
        })?;

    let manifest_path = dir.join(format!("{stem}.{MANIFEST_EXTENSION}"));
    let manifest = PreMigrationManifest {
        format_version: 1,
        created_at: created_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        binary_version: request.binary_version,
        previous_version: request.previous_version,
        server_version_num,
        applied_migrations: request.applied_migrations,
        pending_migrations: request.pending_migrations,
        dump_file: dump_name,
        dump_format: "pg_dump custom (pg_restore)",
        size_bytes,
        method: method.to_string(),
        excluded_table_data: &excluded.tables,
    };
    let manifest_json = serde_json::to_vec_pretty(&manifest).map_err(|error| {
        PreMigrationBackupError::Finalize {
            path: manifest_path.clone(),
            source: std::io::Error::other(error),
        }
    })?;
    tokio::fs::write(&manifest_path, manifest_json)
        .await
        .map_err(|source| PreMigrationBackupError::Finalize {
            path: manifest_path.clone(),
            source,
        })?;
    restrict_file_permissions(&dump_path);
    restrict_file_permissions(&manifest_path);

    let pruned =
        match prune_old_backups_preserving(&dir, PRE_MIGRATION_BACKUPS_KEPT, Some(&dump_path)) {
            Ok(pruned) => pruned,
            Err(error) => {
                // Retention is housekeeping: the new backup exists, so a failed
                // cleanup must not block the upgrade it protects.
                warn!(
                    dir = %dir.display(),
                    "Could not prune old pre-migration backups: {error}"
                );
                Vec::new()
            }
        };

    Ok(PreMigrationBackup {
        dump_path,
        manifest_path,
        size_bytes,
        method,
        elapsed: started.elapsed(),
        pruned,
    })
}

/// Connection details for `pg_dump`, with the password split out so it is
/// passed through the environment instead of the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DumpConnection {
    /// `postgresql://user@host:port/db?<libpq params>`, no password.
    uri: String,
    password: Option<String>,
}

impl DumpConnection {
    fn from_database_url(database_url: &str) -> Result<Self, PreMigrationBackupError> {
        let mut url = url::Url::parse(database_url).map_err(|error| {
            PreMigrationBackupError::InvalidDatabaseUrl {
                reason: format!("not a valid URL: {error}"),
            }
        })?;
        if !matches!(url.scheme(), "postgres" | "postgresql") {
            return Err(PreMigrationBackupError::InvalidDatabaseUrl {
                reason: format!(
                    "expected a postgres:// or postgresql:// URL, got scheme '{}'",
                    url.scheme()
                ),
            });
        }
        if url.path().trim_start_matches('/').is_empty() {
            return Err(PreMigrationBackupError::InvalidDatabaseUrl {
                reason: "the URL names no database".to_string(),
            });
        }
        let password = match url.password() {
            Some(encoded) => Some(
                urlencoding::decode(encoded)
                    .map_err(|error| PreMigrationBackupError::InvalidDatabaseUrl {
                        reason: format!("the password is not valid percent-encoded UTF-8: {error}"),
                    })?
                    .into_owned(),
            ),
            None => None,
        };
        url.set_password(None)
            .map_err(|()| PreMigrationBackupError::InvalidDatabaseUrl {
                reason: "the URL cannot carry credentials".to_string(),
            })?;

        let kept: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(key, _)| LIBPQ_QUERY_PARAMS.contains(&key.as_ref()))
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        if kept.is_empty() {
            url.set_query(None);
        } else {
            url.query_pairs_mut().clear().extend_pairs(kept);
        }
        Ok(Self {
            uri: url.to_string(),
            password,
        })
    }
}

fn prepare_directory(dir: &Path) -> Result<(), PreMigrationBackupError> {
    std::fs::create_dir_all(dir).map_err(|source| PreMigrationBackupError::Directory {
        path: dir.to_path_buf(),
        source,
    })?;
    // The dump contains password hashes and encrypted secrets.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(
            |source| PreMigrationBackupError::Directory {
                path: dir.to_path_buf(),
                source,
            },
        )?;
    }
    Ok(())
}

fn acquire_backup_lock(dir: &Path) -> Result<std::fs::File, PreMigrationBackupError> {
    let path = dir.join(".backup.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|source| PreMigrationBackupError::Directory {
            path: path.clone(),
            source,
        })?;
    file.try_lock()
        .map_err(|error| PreMigrationBackupError::Directory {
            path,
            source: std::io::Error::other(format!("another backup may be running: {error}")),
        })?;
    Ok(file)
}

fn remove_incomplete_backups(dir: &Path) {
    // Only called while holding the shared backup lock.
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(FILE_PREFIX)
                && (name.ends_with(PARTIAL_SUFFIX) || name.ends_with(".tar"))
            {
                if let Err(error) = std::fs::remove_file(entry.path()) {
                    debug!(
                        path = %entry.path().display(),
                        "Could not remove an incomplete pre-migration backup: {error}"
                    );
                }
            }
        }
    }
}

fn restrict_file_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // A container-written dump is owned by root; the 0700 directory
        // already keeps it private, so a failure here is not fatal.
        if let Err(error) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
            debug!(path = %path.display(), "Could not restrict backup file mode: {error}");
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

async fn remove_quietly(path: &Path) {
    if let Err(error) = tokio::fs::remove_file(path).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            debug!(path = %path.display(), "Could not remove {}: {error}", path.display());
        }
    }
}

async fn read_server_version_num(db: &DatabaseConnection) -> Result<i32, PreMigrationBackupError> {
    let row = db
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT current_setting('server_version_num')::int AS version_num".to_owned(),
        ))
        .await
        .map_err(|source| PreMigrationBackupError::ServerVersion { source })?
        .ok_or_else(|| PreMigrationBackupError::ServerVersion {
            source: DbErr::RecordNotFound("server_version_num returned no row".to_string()),
        })?;
    row.try_get::<i32>("", "version_num")
        .map_err(|source| PreMigrationBackupError::ServerVersion { source })
}

/// `180006` -> `18`.
fn server_major(version_num: i32) -> u32 {
    (version_num.max(0) / 10_000) as u32
}

/// `pg_dump (PostgreSQL) 18.6 (Ubuntu 18.6-1)` -> `18`.
fn parse_pg_dump_major(version_output: &str) -> Option<u32> {
    version_output
        .split_whitespace()
        .find_map(|token| {
            let major = token.split('.').next()?;
            if token.contains('.') || token.chars().all(|c| c.is_ascii_digit()) {
                major.parse::<u32>().ok()
            } else {
                None
            }
        })
        .filter(|major| *major >= 9)
}

/// `temps-pre-migration-20261004T101500Z-v0.1.0-beta.57`.
fn backup_file_stem(created_at: DateTime<Utc>, binary_version: &str) -> String {
    let version: String = binary_version
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+') {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    let version = if version.is_empty() {
        "unknown".to_string()
    } else {
        version
    };
    format!(
        "{FILE_PREFIX}{}-{version}",
        created_at.format("%Y%m%dT%H%M%SZ")
    )
}

/// `pg_dump` arguments: custom format, no password prompt, telemetry tables
/// schema-only.
fn pg_dump_args(
    connection_uri: &str,
    output_path: &str,
    exclude_patterns: &[String],
) -> Vec<String> {
    let mut args = vec![
        "--format=custom".to_string(),
        "--no-password".to_string(),
        format!("--file={output_path}"),
        format!("--dbname={connection_uri}"),
    ];
    args.extend(
        exclude_patterns
            .iter()
            .map(|pattern| format!("--exclude-table-data={pattern}")),
    );
    args
}

/// A host `pg_dump` if one with a new-enough major is on `PATH`, otherwise
/// Docker, otherwise an error explaining both.
async fn choose_dump_method(server_major: u32) -> Result<DumpMethod, PreMigrationBackupError> {
    let host = match probe_host_pg_dump().await {
        Some((binary, major)) if major >= server_major => {
            return Ok(DumpMethod::HostPgDump { binary, major });
        }
        Some((binary, major)) => format!(
            "{} is pg_dump {major}, older than the server",
            binary.display()
        ),
        None => "no pg_dump on PATH".to_string(),
    };
    match probe_docker().await {
        Ok(()) => Ok(DumpMethod::Docker {
            image: format!("postgres:{server_major}"),
        }),
        Err(docker) => Err(PreMigrationBackupError::NoDumpTool {
            server_major,
            host,
            docker,
        }),
    }
}

async fn probe_host_pg_dump() -> Option<(PathBuf, u32)> {
    let binary = find_on_path("pg_dump")?;
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(&binary)
            .arg("--version")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    let major = parse_pg_dump_major(&String::from_utf8_lossy(&output.stdout))?;
    Some((binary, major))
}

fn find_on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

async fn probe_docker() -> Result<(), String> {
    let docker = bollard::Docker::connect_with_local_defaults()
        .map_err(|error| format!("cannot connect to the Docker daemon: {error}"))?;
    match tokio::time::timeout(DOCKER_PING_TIMEOUT, docker.ping()).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(format!("the Docker daemon did not answer a ping: {error}")),
        Err(_) => Err(format!(
            "the Docker daemon did not answer a ping within {}s",
            DOCKER_PING_TIMEOUT.as_secs()
        )),
    }
}

fn tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.len() <= MAX_STDERR_BYTES {
        return trimmed.to_string();
    }
    let mut start = trimmed.len() - MAX_STDERR_BYTES;
    while !trimmed.is_char_boundary(start) {
        start += 1;
    }
    format!("...{}", &trimmed[start..])
}

async fn run_host_pg_dump(
    binary: &Path,
    args: Vec<String>,
    password: Option<&str>,
    method: &DumpMethod,
    dump_path: &Path,
) -> Result<(), PreMigrationBackupError> {
    let mut command = tokio::process::Command::new(binary);
    command.args(&args).kill_on_drop(true);
    if let Some(password) = password {
        command.env("PGPASSWORD", password);
    }
    let output = match tokio::time::timeout(DUMP_TIMEOUT, command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            return Err(PreMigrationBackupError::DumpLaunch {
                method: method.to_string(),
                path: dump_path.to_path_buf(),
                reason: error.to_string(),
            })
        }
        Err(_) => {
            return Err(PreMigrationBackupError::Timeout {
                method: method.to_string(),
                path: dump_path.to_path_buf(),
                timeout_secs: DUMP_TIMEOUT.as_secs(),
            })
        }
    };
    if output.status.success() {
        return Ok(());
    }
    Err(PreMigrationBackupError::DumpFailed {
        method: method.to_string(),
        path: dump_path.to_path_buf(),
        exit_code: output.status.code().map(i64::from).unwrap_or(-1),
        stderr: tail(&output.stderr),
    })
}

/// Run `pg_dump` in a one-shot container and copy the archive out of it.
///
/// The archive is written inside the container and fetched through the
/// Docker API rather than through a bind mount: a bind source is resolved on
/// the Docker host, which is not this filesystem when Temps itself runs in a
/// container (Compose) or behind a VM-backed daemon. The container is always
/// removed, whatever happens.
async fn run_docker_pg_dump(
    image: &str,
    args: Vec<String>,
    password: Option<&str>,
    method: &DumpMethod,
    partial_path: &Path,
    dump_path: &Path,
) -> Result<(), PreMigrationBackupError> {
    let launch_error = |reason: String| PreMigrationBackupError::DumpLaunch {
        method: method.to_string(),
        path: dump_path.to_path_buf(),
        reason,
    };
    tokio::time::timeout(
        DUMP_TIMEOUT,
        crate::engines::image_pull::ensure_image_pulled_v2(image, ENGINE_LABEL),
    )
    .await
    .map_err(|_| PreMigrationBackupError::Timeout {
        method: format!("pulling {image}"),
        path: dump_path.to_path_buf(),
        timeout_secs: DUMP_TIMEOUT.as_secs(),
    })?
    .map_err(|error| launch_error(format!("could not pull {image}: {error}")))?;
    let docker = bollard::Docker::connect_with_local_defaults()
        .map_err(|error| launch_error(format!("cannot connect to the Docker daemon: {error}")))?;

    let name = format!("temps-pre-migration-backup-{}", uuid::Uuid::new_v4());
    let mut labels = std::collections::HashMap::new();
    labels.insert(
        "sh.temps.kind".to_string(),
        "pre-migration-backup".to_string(),
    );
    let body = bollard::models::ContainerCreateBody {
        image: Some(image.to_string()),
        entrypoint: Some(vec!["pg_dump".to_string()]),
        cmd: Some(args),
        env: password.map(|password| vec![format!("PGPASSWORD={password}")]),
        labels: Some(labels),
        host_config: Some(bollard::models::HostConfig {
            // Same as the control-plane engine: reach a database published
            // on the host's loopback.
            network_mode: Some("host".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    docker
        .create_container(
            Some(
                bollard::query_parameters::CreateContainerOptionsBuilder::new()
                    .name(&name)
                    .build(),
            ),
            body,
        )
        .await
        .map_err(|error| launch_error(format!("could not create container {name}: {error}")))?;

    let outcome = tokio::time::timeout(
        DUMP_TIMEOUT,
        dump_in_container(&docker, &name, method, partial_path, dump_path),
    )
    .await;

    if let Err(error) = docker
        .remove_container(
            &name,
            Some(
                bollard::query_parameters::RemoveContainerOptionsBuilder::new()
                    .force(true)
                    .build(),
            ),
        )
        .await
    {
        warn!(container = %name, "Could not remove the pre-migration backup container: {error}");
    }

    match outcome {
        Ok(result) => result,
        Err(_) => Err(PreMigrationBackupError::Timeout {
            method: method.to_string(),
            path: dump_path.to_path_buf(),
            timeout_secs: DUMP_TIMEOUT.as_secs(),
        }),
    }
}

async fn dump_in_container(
    docker: &bollard::Docker,
    name: &str,
    method: &DumpMethod,
    partial_path: &Path,
    dump_path: &Path,
) -> Result<(), PreMigrationBackupError> {
    use futures::StreamExt;

    let launch_error = |reason: String| PreMigrationBackupError::DumpLaunch {
        method: method.to_string(),
        path: dump_path.to_path_buf(),
        reason,
    };
    docker
        .start_container(
            name,
            None::<bollard::query_parameters::StartContainerOptions>,
        )
        .await
        .map_err(|error| launch_error(format!("could not start container {name}: {error}")))?;

    let mut wait = docker.wait_container(
        name,
        Some(bollard::query_parameters::WaitContainerOptionsBuilder::new().build()),
    );
    let exit_code = match wait.next().await {
        Some(Ok(response)) => response.status_code,
        // bollard reports a non-zero exit as this error variant.
        Some(Err(bollard::errors::Error::DockerContainerWaitError { code, .. })) => code,
        Some(Err(error)) => {
            return Err(launch_error(format!(
                "waiting for container {name} failed: {error}"
            )))
        }
        None => {
            return Err(launch_error(format!(
                "container {name} produced no exit status"
            )))
        }
    };
    if exit_code != 0 {
        let mut logs = docker.logs(
            name,
            Some(
                bollard::query_parameters::LogsOptionsBuilder::new()
                    .stderr(true)
                    .stdout(true)
                    .tail("50")
                    .build(),
            ),
        );
        let mut output = Vec::new();
        while let Some(Ok(chunk)) = logs.next().await {
            output.extend_from_slice(&chunk.into_bytes());
        }
        return Err(PreMigrationBackupError::DumpFailed {
            method: method.to_string(),
            path: dump_path.to_path_buf(),
            exit_code,
            stderr: tail(&output),
        });
    }

    // Stream the tar the daemon produces to disk (constant memory), then
    // extract the single archive member next to it.
    let tar_path = partial_path.with_extension("tar");
    let copy_result = async {
        let mut tar_file = tokio::fs::File::create(&tar_path).await.map_err(|error| {
            launch_error(format!("could not create {}: {error}", tar_path.display()))
        })?;
        let mut stream = docker.download_from_container(
            name,
            Some(
                bollard::query_parameters::DownloadFromContainerOptionsBuilder::new()
                    .path(CONTAINER_DUMP_PATH)
                    .build(),
            ),
        );
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| {
                launch_error(format!(
                    "could not copy {CONTAINER_DUMP_PATH} out of container {name}: {error}"
                ))
            })?;
            tokio::io::AsyncWriteExt::write_all(&mut tar_file, &chunk)
                .await
                .map_err(|error| {
                    launch_error(format!("could not write {}: {error}", tar_path.display()))
                })?;
        }
        tokio::io::AsyncWriteExt::flush(&mut tar_file)
            .await
            .map_err(|error| {
                launch_error(format!("could not write {}: {error}", tar_path.display()))
            })?;
        let tar_for_extract = tar_path.clone();
        let target = partial_path.to_path_buf();
        tokio::task::spawn_blocking(move || extract_single_file(&tar_for_extract, &target))
            .await
            .map_err(|error| launch_error(format!("archive extraction task failed: {error}")))?
            .map_err(|error| {
                launch_error(format!(
                    "could not extract the dump from {}: {error}",
                    tar_path.display()
                ))
            })
    }
    .await;
    remove_quietly(&tar_path).await;
    copy_result
}

/// Extract the first regular file of the tar at `tar_path` into `target`.
fn extract_single_file(tar_path: &Path, target: &Path) -> std::io::Result<()> {
    let file = std::fs::File::open(tar_path)?;
    let mut archive = tar::Archive::new(std::io::BufReader::new(file));
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.header().entry_type().is_file() {
            let mut out = std::fs::File::create(target)?;
            std::io::copy(&mut entry, &mut out)?;
            out.sync_all()?;
            return Ok(());
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "the archive contains no file",
    ))
}

/// Keep the newest `keep` dumps (by file modification time) and delete
/// the rest together with their manifests. Returns the deleted dump paths.
pub fn prune_old_backups(dir: &Path, keep: usize) -> std::io::Result<Vec<PathBuf>> {
    prune_old_backups_preserving(dir, keep, None)
}

fn prune_old_backups_preserving(
    dir: &Path,
    keep: usize,
    protected: Option<&Path>,
) -> std::io::Result<Vec<PathBuf>> {
    let dump_suffix = format!(".{DUMP_EXTENSION}");
    let mut stems: Vec<String> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let stem = name.strip_suffix(&dump_suffix)?;
            stem.starts_with(FILE_PREFIX).then(|| stem.to_string())
        })
        .collect();
    // A UUID does not order retries made in the same second. Preserve the
    // just-completed dump unconditionally, then compare completion mtimes.
    stems.sort_unstable_by_key(|stem| {
        let path = dir.join(format!("{stem}.{DUMP_EXTENSION}"));
        let modified = std::fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .ok();
        std::cmp::Reverse((protected == Some(path.as_path()), modified, stem.clone()))
    });

    let mut pruned = Vec::new();
    for stem in stems.into_iter().skip(keep) {
        let dump = dir.join(format!("{stem}.{DUMP_EXTENSION}"));
        std::fs::remove_file(&dump)?;
        let manifest = dir.join(format!("{stem}.{MANIFEST_EXTENSION}"));
        match std::fs::remove_file(&manifest) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        pruned.push(dump);
    }
    Ok(pruned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn connection_moves_the_password_out_of_the_uri() {
        let connection = DumpConnection::from_database_url(
            "postgresql://temps:s%40cr%3At@db.internal:6543/temps",
        )
        .expect("valid url");
        assert_eq!(connection.uri, "postgresql://temps@db.internal:6543/temps");
        assert_eq!(connection.password.as_deref(), Some("s@cr:t"));
    }

    #[test]
    fn connection_keeps_libpq_parameters_and_drops_driver_options() {
        let connection = DumpConnection::from_database_url(
            "postgres://temps:pw@localhost/temps?sslmode=require&statement-cache-capacity=0",
        )
        .expect("valid url");
        assert_eq!(
            connection.uri,
            "postgres://temps@localhost/temps?sslmode=require"
        );

        let without_params =
            DumpConnection::from_database_url("postgres://temps@localhost/temps?foo=bar")
                .expect("valid url");
        assert_eq!(without_params.uri, "postgres://temps@localhost/temps");
        assert_eq!(without_params.password, None);
    }

    #[test]
    fn connection_rejects_non_postgres_and_database_less_urls() {
        assert!(matches!(
            DumpConnection::from_database_url("mysql://root@localhost/temps"),
            Err(PreMigrationBackupError::InvalidDatabaseUrl { ref reason }) if reason.contains("mysql")
        ));
        assert!(matches!(
            DumpConnection::from_database_url("postgres://temps@localhost"),
            Err(PreMigrationBackupError::InvalidDatabaseUrl { ref reason }) if reason.contains("no database")
        ));
        assert!(matches!(
            DumpConnection::from_database_url("not a url"),
            Err(PreMigrationBackupError::InvalidDatabaseUrl { .. })
        ));
    }

    #[test]
    fn pg_dump_args_never_carry_the_password() {
        let connection =
            DumpConnection::from_database_url("postgresql://temps:hunter2@localhost:5432/temps")
                .expect("valid url");
        let args = pg_dump_args(
            &connection.uri,
            "/backup/x.dump.partial",
            &["public.proxy_logs".to_string()],
        );
        assert_eq!(
            args,
            vec![
                "--format=custom",
                "--no-password",
                "--file=/backup/x.dump.partial",
                "--dbname=postgresql://temps@localhost:5432/temps",
                "--exclude-table-data=public.proxy_logs",
            ]
        );
        assert!(args.iter().all(|arg| !arg.contains("hunter2")));
    }

    #[test]
    fn versions_are_parsed() {
        assert_eq!(server_major(180_006), 18);
        assert_eq!(server_major(160_004), 16);
        assert_eq!(server_major(-1), 0);
        assert_eq!(
            parse_pg_dump_major("pg_dump (PostgreSQL) 18.6 (Ubuntu 18.6-1.pgdg22.04+2)\n"),
            Some(18)
        );
        assert_eq!(parse_pg_dump_major("pg_dump (PostgreSQL) 16.4"), Some(16));
        assert_eq!(parse_pg_dump_major("pg_dump (PostgreSQL) 17"), Some(17));
        assert_eq!(parse_pg_dump_major("command not found"), None);
        assert_eq!(parse_pg_dump_major(""), None);
    }

    #[test]
    fn file_stem_is_sortable_and_sanitized() {
        let at = Utc
            .with_ymd_and_hms(2026, 10, 4, 10, 15, 0)
            .single()
            .expect("valid timestamp");
        assert_eq!(
            backup_file_stem(at, "v0.1.0-beta.57"),
            "temps-pre-migration-20261004T101500Z-v0.1.0-beta.57"
        );
        assert_eq!(
            backup_file_stem(at, "../../etc v1"),
            "temps-pre-migration-20261004T101500Z-.._.._etc_v1"
        );
        assert_eq!(
            backup_file_stem(at, ""),
            "temps-pre-migration-20261004T101500Z-unknown"
        );
    }

    #[test]
    fn retention_keeps_the_newest_backups_and_their_manifests() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let stems = [
            "temps-pre-migration-20260101T000000Z-v1",
            "temps-pre-migration-20260301T000000Z-v3",
            "temps-pre-migration-20260201T000000Z-v2",
            "temps-pre-migration-20260401T000000Z-v4",
        ];
        for stem in stems {
            let dump = dir.path().join(format!("{stem}.dump"));
            std::fs::write(&dump, b"x")?;
            let month = stem[24..26].parse::<u64>().expect("fixture month");
            std::fs::File::open(&dump)?
                .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(month))?;
            std::fs::write(dir.path().join(format!("{stem}.json")), b"{}")?;
        }
        // Files that are not ours are never touched.
        std::fs::write(dir.path().join("operator-notes.dump"), b"keep")?;

        let pruned = prune_old_backups(dir.path(), 2)?;
        let mut pruned_names: Vec<String> = pruned
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        pruned_names.sort();
        assert_eq!(
            pruned_names,
            vec![
                "temps-pre-migration-20260101T000000Z-v1.dump",
                "temps-pre-migration-20260201T000000Z-v2.dump",
            ]
        );
        let mut remaining: Vec<String> = std::fs::read_dir(dir.path())?
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        remaining.sort();
        assert_eq!(
            remaining,
            vec![
                "operator-notes.dump",
                "temps-pre-migration-20260301T000000Z-v3.dump",
                "temps-pre-migration-20260301T000000Z-v3.json",
                "temps-pre-migration-20260401T000000Z-v4.dump",
                "temps-pre-migration-20260401T000000Z-v4.json",
            ]
        );

        // Nothing to prune is not an error.
        assert!(prune_old_backups(dir.path(), 5)?.is_empty());
        Ok(())
    }

    #[test]
    fn concurrent_backup_cannot_clean_up_an_active_partial() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let lock = acquire_backup_lock(dir.path()).expect("first lock");
        let partial = dir.path().join("temps-pre-migration-active.dump.partial");
        std::fs::write(&partial, b"active")?;
        assert!(acquire_backup_lock(dir.path()).is_err());
        assert!(partial.exists());
        drop(lock);
        assert!(acquire_backup_lock(dir.path()).is_ok());
        Ok(())
    }

    #[test]
    fn retention_preserves_the_just_completed_dump_despite_filename_order() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let newest = dir.path().join("temps-pre-migration-same-second-aaa.dump");
        std::fs::write(&newest, b"newest")?;
        for suffix in ["bbb", "ccc", "ddd", "eee"] {
            std::fs::write(
                dir.path()
                    .join(format!("temps-pre-migration-same-second-{suffix}.dump")),
                b"old",
            )?;
        }
        assert_eq!(
            prune_old_backups_preserving(dir.path(), 3, Some(&newest))?.len(),
            2
        );
        assert!(newest.exists());
        Ok(())
    }

    #[test]
    fn locked_cleanup_removes_partial_leftovers_only() -> std::io::Result<()> {
        let data_dir = tempfile::tempdir()?;
        let dir = pre_migration_backup_dir(data_dir.path());
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("temps-pre-migration-x.dump.partial"), b"half")?;
        std::fs::write(dir.join("temps-pre-migration-x.dump.tar"), b"copy")?;
        std::fs::write(dir.join("temps-pre-migration-y.dump"), b"whole")?;
        prepare_directory(&dir).expect("directory prepared");
        let _lock = acquire_backup_lock(&dir).expect("lock acquired");
        remove_incomplete_backups(&dir);
        assert!(!dir.join("temps-pre-migration-x.dump.partial").exists());
        assert!(!dir.join("temps-pre-migration-x.dump.tar").exists());
        assert!(dir.join("temps-pre-migration-y.dump").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir)?.permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
        Ok(())
    }

    #[test]
    fn the_dump_is_extracted_from_the_container_archive() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let tar_path = dir.path().join("copy.tar");
        {
            let mut builder = tar::Builder::new(std::fs::File::create(&tar_path)?);
            let payload = b"PGDMP-archive-bytes";
            let mut header = tar::Header::new_gnu();
            header.set_size(payload.len() as u64);
            header.set_mode(0o600);
            header.set_cksum();
            builder.append_data(&mut header, "temps-pre-migration.dump", &payload[..])?;
            builder.finish()?;
        }
        let target = dir.path().join("out.dump.partial");
        extract_single_file(&tar_path, &target)?;
        assert_eq!(std::fs::read(&target)?, b"PGDMP-archive-bytes");

        let empty_tar = dir.path().join("empty.tar");
        tar::Builder::new(std::fs::File::create(&empty_tar)?).finish()?;
        let error = extract_single_file(&empty_tar, &dir.path().join("none"))
            .expect_err("an empty archive has no dump");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        Ok(())
    }

    #[test]
    fn long_stderr_is_truncated_to_its_tail() {
        let long = "a".repeat(MAX_STDERR_BYTES * 2) + "END";
        let tailed = tail(long.as_bytes());
        assert!(tailed.starts_with("..."));
        assert!(tailed.ends_with("END"));
        assert!(tailed.len() <= MAX_STDERR_BYTES + 3);
        assert_eq!(tail(b"  short \n"), "short");
    }

    #[test]
    fn no_dump_tool_error_explains_both_routes() {
        let message = PreMigrationBackupError::NoDumpTool {
            server_major: 18,
            host: "no pg_dump on PATH".to_string(),
            docker: "cannot connect to the Docker daemon: socket missing".to_string(),
        }
        .to_string();
        assert!(message.contains("PostgreSQL 18"), "{message}");
        assert!(message.contains("no pg_dump on PATH"), "{message}");
        assert!(message.contains("postgres:18"), "{message}");
    }
}
