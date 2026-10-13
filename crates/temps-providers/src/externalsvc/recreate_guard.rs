// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The one way a managed service's container may be replaced.
//!
//! Several operations can only be applied by creating a new container:
//! enabling WAL archiving (a postmaster-only setting baked into `CMD`), the
//! `start()` drift reconcile, an image swap, a new `/dev/shm` size. Each of
//! them used to `remove_container` the old container and create a new one
//! from the service's *current* specification. Nothing checked that the new
//! specification pointed at the storage the old container actually used, so
//! when the two disagreed the new container initialised an empty data
//! directory, the service reported success, and the following backup captured
//! the empty database (#1415).
//!
//! [`recreate_preserving_data`] makes that impossible regardless of why the
//! two disagree:
//!
//! 1. **Before**: inspect the existing container, work out where its data
//!    directory really is (from its own env/command, not from what Temps
//!    would configure today) and which mount holds it. Refuse with a typed
//!    [`RecreateGuardError`] unless the replacement mounts the same named
//!    volume with the data directory at the same place inside it.
//! 2. **Keep the old container**: disable its restart policy, stop it and
//!    rename it aside. It is never removed until the replacement is verified,
//!    and it is never removed with `v: true`.
//! 3. **After**: compare an engine fingerprint taken before and after (the
//!    PostgreSQL `pg_control` system identifier, databases and roles; the
//!    database list for MariaDB and MongoDB; the keyspace for Redis; bucket
//!    names for object stores). A fresh `initdb` changes the system
//!    identifier, so it can never pass.
//! 4. **On any failure** remove only the replacement, rename the original
//!    back, restore its restart policy and start it again.
//!
//! [`check_storage`] is pure so every container shape older releases
//! produced can be unit tested without Docker.

// A `RecreateGuardError` is built at most once per container recreate, a
// rare control-plane operation, and carries every identifier an operator
// needs to act on it (service, container, data directory, storage).
// Boxing it would only add indirection to a cold path.
#![allow(clippy::result_large_err)]

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::time::Duration;

use bollard::models::{
    ContainerInspectResponse, ContainerUpdateBody, MountPointTypeEnum, RestartPolicy,
    RestartPolicyNameEnum,
};
use bollard::query_parameters::{
    DownloadFromContainerOptions, InspectContainerOptions, ListContainersOptions, LogsOptions,
    RemoveContainerOptions, RenameContainerOptions, StartContainerOptions, StopContainerOptions,
};
use bollard::Docker;
use futures::StreamExt;
use tracing::{error, info, warn};

/// Seconds a stopping database gets to shut down cleanly before Docker
/// kills it. A killed PostgreSQL recovers from WAL, but a clean stop keeps
/// the replacement's startup short.
const STOP_TIMEOUT_SECS: i32 = 60;

/// How long a stop requested by an abandoned recreate can still be running
/// in the Docker daemon after its caller went away.
const ABANDONED_STOP_GRACE: Duration = Duration::from_secs(STOP_TIMEOUT_SECS as u64 + 5);

/// Attempts at reading the replacement's fingerprint once it reports healthy.
/// A healthy container can still refuse its first connections for a moment.
const FINGERPRINT_ATTEMPTS: u32 = 10;
const FINGERPRINT_RETRY_DELAY: Duration = Duration::from_secs(2);

/// Upper bound for a small file read through the archive API (`pg_control`
/// is 8 KiB; its tar framing adds a few blocks).
const SMALL_FILE_LIMIT: usize = 1024 * 1024;

/// Bound on the startup log scanned for a fresh-initialisation marker.
const LOG_TAIL_LINES: &str = "2000";

/// Prefix the fingerprint queries put on every line they emit, so client
/// warnings mixed into the exec output are never mistaken for object names.
const FINGERPRINT_LINE_PREFIX: &str = "TEMPSFP:";
/// Printed last by every fingerprint query once its listing is complete.
const FINGERPRINT_DONE: &str = "TEMPSFP_DONE";

/// Engines whose containers Temps manages and may need to replace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedEngine {
    Postgres,
    Redis,
    MariaDb,
    MongoDb,
    Rustfs,
    Minio,
}

impl fmt::Display for ManagedEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ManagedEngine::Postgres => "PostgreSQL",
            ManagedEngine::Redis => "Redis",
            ManagedEngine::MariaDb => "MariaDB",
            ManagedEngine::MongoDb => "MongoDB",
            ManagedEngine::Rustfs => "RustFS",
            ManagedEngine::Minio => "MinIO",
        })
    }
}

impl ManagedEngine {
    /// Where this engine keeps its data inside a container with the given
    /// environment and command. Resolved from the container's own settings,
    /// never from what Temps would configure today: a container created by an
    /// older release, imported, or swapped to another image may differ.
    pub fn data_dir(&self, env: &[String], cmd: &[String]) -> String {
        let dir = match self {
            // Docker merges the image's ENV into the container's, so PGDATA
            // is present whenever either set it. The fallback is the
            // historical default of every official image before PostgreSQL 18.
            ManagedEngine::Postgres => pg_cmd_data_dir(cmd)
                .or_else(|| env_value(env, "PGDATA"))
                .unwrap_or_else(|| "/var/lib/postgresql/data".into()),
            ManagedEngine::Redis => flag_value(cmd, "--dir").unwrap_or_else(|| "/data".to_string()),
            ManagedEngine::MariaDb => {
                flag_value(cmd, "--datadir").unwrap_or_else(|| "/var/lib/mysql".to_string())
            }
            ManagedEngine::MongoDb => {
                flag_value(cmd, "--dbpath").unwrap_or_else(|| "/data/db".to_string())
            }
            ManagedEngine::Rustfs => env_value(env, "RUSTFS_VOLUMES")
                .filter(|v| v.starts_with('/') && !v.contains(['{', ' ', ',']))
                .unwrap_or_else(|| "/data".to_string()),
            ManagedEngine::Minio => cmd
                .iter()
                .position(|arg| arg == "server")
                .and_then(|i| cmd.get(i + 1))
                .filter(|arg| arg.starts_with('/'))
                .cloned()
                .unwrap_or_else(|| "/data".to_string()),
        };
        normalize_path(&dir)
    }

    /// Log lines only a first-boot initialisation prints. Their presence in
    /// the replacement's log means it did not find the existing data.
    fn fresh_init_markers(&self) -> &'static [&'static str] {
        match self {
            ManagedEngine::Postgres => &[
                "running bootstrap script",
                "PostgreSQL init process complete",
            ],
            ManagedEngine::MariaDb => &["Initializing database files"],
            ManagedEngine::MongoDb => &["MongoDB init process complete"],
            ManagedEngine::Redis | ManagedEngine::Rustfs | ManagedEngine::Minio => &[],
        }
    }

    /// A small file inside the data directory that exists once the engine
    /// has initialised it, read through the archive API (so it works on a
    /// stopped container), and how to turn it into an identity.
    fn marker_file(&self) -> (&'static str, MarkerKind) {
        match self {
            ManagedEngine::Postgres => ("global/pg_control", MarkerKind::PgSystemIdentifier),
            ManagedEngine::MariaDb => ("aria_log_control", MarkerKind::Exists),
            ManagedEngine::MongoDb => ("WiredTiger", MarkerKind::Exists),
            ManagedEngine::Redis => ("appendonlydir/appendonly.aof.manifest", MarkerKind::Exists),
            ManagedEngine::Rustfs => (".rustfs.sys/format.json", MarkerKind::DeploymentId),
            ManagedEngine::Minio => (".minio.sys/format.json", MarkerKind::DeploymentId),
        }
    }

    /// Shell script printing one `TEMPSFP:<kind>:<name>` line per object that
    /// must survive a recreate, then `TEMPSFP_DONE` once the listing is
    /// known to be complete (absent = unreadable, never "empty"). Credentials come from the container's own
    /// environment, which `docker exec` inherits, so nothing secret is put on
    /// a command line or into a log.
    fn objects_script(&self, data_dir: &str) -> String {
        match self {
            ManagedEngine::Postgres => concat!(
                "export PGPASSWORD=\"${POSTGRES_PASSWORD:-}\"; ",
                "u=\"${POSTGRES_USER:-postgres}\"; ",
                "for d in \"${POSTGRES_DB:-$u}\" postgres template1; do ",
                "psql -X -q -A -t -v ON_ERROR_STOP=1 -U \"$u\" -d \"$d\" -c ",
                "\"SELECT 'TEMPSFP:db:' || datname FROM pg_database WHERE NOT datistemplate ",
                "UNION ALL SELECT 'TEMPSFP:role:' || rolname FROM pg_roles WHERE rolname !~ '^pg_' ",
                "UNION ALL SELECT 'TEMPSFP:tablespace:' || pg_tablespace_location(oid) FROM pg_tablespace ",
                "WHERE pg_tablespace_location(oid) <> ''\" ",
                "&& echo TEMPSFP_DONE && exit 0; done; exit 1"
            )
            .to_string(),
            ManagedEngine::MariaDb => concat!(
                "export MYSQL_PWD=\"${MARIADB_ROOT_PASSWORD:-${MYSQL_ROOT_PASSWORD:-}}\"; ",
                "c=$(command -v mariadb || command -v mysql) || exit 1; ",
                "out=$(\"$c\" -uroot -N -B -e 'SHOW DATABASES') || exit 1; ",
                "printf '%s\\n' \"$out\" | sed 's/^/TEMPSFP:db:/'; echo TEMPSFP_DONE"
            )
            .to_string(),
            ManagedEngine::MongoDb => concat!(
                "s=$(command -v mongosh || command -v mongo) || exit 1; ",
                "\"$s\" --quiet --norc --eval '",
                "if (process.env.MONGO_INITDB_ROOT_USERNAME) { db.getSiblingDB(\"admin\").auth(",
                "process.env.MONGO_INITDB_ROOT_USERNAME, process.env.MONGO_INITDB_ROOT_PASSWORD); } ",
                "db.adminCommand({listDatabases: 1, nameOnly: true}).databases.forEach(",
                "function (d) { print(\"TEMPSFP:db:\" + d.name); }); print(\"TEMPSFP_DONE\")'"
            )
            .to_string(),
            // Only databases holding keys without a TTL: keys that expire
            // during the restart are not lost data. `loading:0` makes sure
            // the dataset is fully loaded before it is listed.
            ManagedEngine::Redis => concat!(
                "if [ -n \"${REDIS_PASSWORD:-}\" ]; then export REDISCLI_AUTH=\"$REDIS_PASSWORD\"; fi; ",
                "out=$(redis-cli INFO) || exit 1; ",
                "printf '%s\\n' \"$out\" | tr -d '\\r' | grep -qx 'loading:0' || exit 1; ",
                "printf '%s\\n' \"$out\" | tr -d '\\r' | sed -n 's/^\\(db[0-9]*\\):keys=\\([0-9]*\\),expires=\\([0-9]*\\).*/\\1 \\2 \\3/p' ",
                "| while read db keys expires; do [ \"$keys\" -gt \"$expires\" ] && echo \"TEMPSFP:keyspace:$db\"; done; ",
                "echo TEMPSFP_DONE"
            )
            .to_string(),
            // Buckets are the visible top-level directories of the data
            // directory on both object stores.
            ManagedEngine::Rustfs | ManagedEngine::Minio => format!(
                "cd {} || exit 1; for b in *; do [ -d \"$b\" ] && echo \"TEMPSFP:bucket:$b\"; done; echo TEMPSFP_DONE",
                shell_quote(data_dir)
            ),
        }
    }
}

/// What a [`ManagedEngine::marker_file`] says about the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkerKind {
    /// Its first 8 bytes: the PostgreSQL cluster's system identifier.
    PgSystemIdentifier,
    /// The `id` of an object store's `format.json`: its deployment ID.
    DeploymentId,
    /// Only whether it exists.
    Exists,
}

/// How an existing container stores one of its mount points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageKind {
    /// A Docker volume with a chosen name.
    NamedVolume,
    /// A Docker volume Docker named itself (an image `VOLUME` or `-v /path`).
    /// It is tied to the container that created it, and a replacement never
    /// mounts it.
    AnonymousVolume,
    /// A host directory.
    Bind,
    /// Memory-backed; nothing survives the container.
    Tmpfs,
    /// Any other mount type Docker reports.
    Other(String),
}

/// One mount of an existing container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedMount {
    pub kind: StorageKind,
    /// Volume name, or host path for a bind mount.
    pub source: String,
    pub destination: String,
}

impl ObservedMount {
    fn describe(&self) -> String {
        match &self.kind {
            StorageKind::NamedVolume => format!("volume '{}'", self.source),
            StorageKind::AnonymousVolume => format!(
                "anonymous volume '{}' (created automatically for this container only)",
                self.source
            ),
            StorageKind::Bind => format!("host directory '{}' (bind mount)", self.source),
            StorageKind::Tmpfs => "a tmpfs mount (memory only)".to_string(),
            StorageKind::Other(kind) => format!("a '{}' mount '{}'", kind, self.source),
        }
    }
}

/// What a replacement container will mount: always named volumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedMount {
    pub volume: String,
    pub target: String,
}

/// The storage layout a replacement container will be created with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplacementStorage {
    /// The data directory the replacement's engine will use.
    pub data_dir: String,
    pub mounts: Vec<PlannedMount>,
}

/// The parts of an existing container the guard needs.
#[derive(Debug, Clone)]
pub struct ContainerSnapshot {
    pub id: String,
    pub name: String,
    /// The image reference the container was created from.
    pub image: String,
    pub env: Vec<String>,
    pub cmd: Vec<String>,
    pub mounts: Vec<ObservedMount>,
    pub running: bool,
    pub restart_policy: Option<RestartPolicy>,
}

impl ContainerSnapshot {
    pub fn from_inspect(name: &str, inspect: &ContainerInspectResponse) -> Self {
        let config = inspect.config.as_ref();
        // A volume mounted with a subpath exposes only part of the volume;
        // the mount point list does not say so, the requested mounts do.
        let subpath_destinations: Vec<String> = inspect
            .host_config
            .as_ref()
            .and_then(|h| h.mounts.as_ref())
            .into_iter()
            .flatten()
            .filter(|m| {
                m.volume_options
                    .as_ref()
                    .and_then(|o| o.subpath.as_deref())
                    .is_some_and(|p| !p.is_empty())
            })
            .filter_map(|m| m.target.as_deref().map(normalize_path))
            .collect();
        let mounts = inspect
            .mounts
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(|m| {
                let destination = normalize_path(m.destination.as_deref()?);
                let kind = match m.typ {
                    Some(MountPointTypeEnum::VOLUME)
                        if subpath_destinations.contains(&destination) =>
                    {
                        StorageKind::Other("volume subpath".to_string())
                    }
                    Some(MountPointTypeEnum::VOLUME) => {
                        if m.name.as_deref().is_some_and(is_anonymous_volume_name) {
                            StorageKind::AnonymousVolume
                        } else {
                            StorageKind::NamedVolume
                        }
                    }
                    Some(MountPointTypeEnum::BIND) => StorageKind::Bind,
                    Some(MountPointTypeEnum::TMPFS) => StorageKind::Tmpfs,
                    Some(other) => StorageKind::Other(other.to_string()),
                    None => StorageKind::Other("unknown".to_string()),
                };
                let source = match &kind {
                    StorageKind::NamedVolume | StorageKind::AnonymousVolume => {
                        m.name.clone().unwrap_or_default()
                    }
                    StorageKind::Other(k) if k == "volume subpath" => {
                        m.name.clone().unwrap_or_default()
                    }
                    _ => m.source.clone().unwrap_or_default(),
                };
                Some(ObservedMount {
                    kind,
                    source,
                    destination,
                })
            })
            .collect();
        Self {
            id: inspect.id.clone().unwrap_or_default(),
            name: name.to_string(),
            image: config.and_then(|c| c.image.clone()).unwrap_or_default(),
            env: config.and_then(|c| c.env.clone()).unwrap_or_default(),
            cmd: config.and_then(|c| c.cmd.clone()).unwrap_or_default(),
            mounts,
            running: inspect
                .state
                .as_ref()
                .and_then(|s| s.running)
                .unwrap_or(false),
            restart_policy: inspect
                .host_config
                .as_ref()
                .and_then(|h| h.restart_policy.clone()),
        }
    }
}

/// Where the data is and why the replacement will see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageMatch {
    pub data_dir: String,
    pub volume: String,
    /// Where the original mounts that volume.
    pub mount_destination: String,
    /// The data directory's path inside the volume (empty = volume root).
    pub path_in_volume: String,
}

/// Why a container replacement was refused or failed. Every message names
/// the service, the container, the data directory and, for a refusal, the
/// safe manual steps; every refusal states that nothing was changed.
#[derive(Debug, thiserror::Error)]
pub enum RecreateGuardError {
    #[error(
        "Refusing to recreate container '{container}' of {engine} service '{service}' ({operation}): \
         its data directory {data_dir} is not on any mount, so it lives in the container's own \
         filesystem and would be deleted with it. Nothing was changed. To keep the data: take a \
         logical dump (or `docker cp {q_source} <dir>`) before anything removes this \
         container, then restore it into the service's volume or into a new service",
        q_source = shell_quote(&format!("{}:{}", .container, .data_dir))
    )]
    DataInContainerLayer {
        engine: ManagedEngine,
        service: String,
        container: String,
        operation: String,
        data_dir: String,
    },

    #[error(
        "Refusing to recreate container '{container}' of {engine} service '{service}' ({operation}): \
         its data directory {data_dir} is on {storage}, which a replacement container would not \
         mount. Recreating would start {engine} on empty storage. Nothing was changed. To fix: stop \
         the service, copy {data_dir} from that storage into volume '{expected_volume}' so it \
         appears at '{expected_path}' inside it (for example with a throwaway container that \
         mounts both, `docker run --rm --volumes-from {q_container} -v {q_volume} alpine \
         cp -a {q_from} {q_to}`), check it, then retry; or dump the database and restore it \
         into a new service",
        q_container = shell_quote(.container),
        q_volume = shell_quote(&format!("{}:/to", .expected_volume)),
        q_from = shell_quote(&format!("{}/.", .data_dir)),
        q_to = shell_quote(&format!("/to/{}", .expected_path))
    )]
    StorageNotCarriedOver {
        engine: ManagedEngine,
        service: String,
        container: String,
        operation: String,
        data_dir: String,
        storage: String,
        expected_volume: String,
        expected_path: String,
    },

    #[error(
        "Refusing to recreate container '{container}' of {engine} service '{service}' ({operation}): \
         its data is at '{old_path}' inside volume '{volume}' but the replacement would use \
         '{new_path}' in the same volume (data directory {old_data_dir} -> {new_data_dir}), so it \
         would start {engine} on an empty directory. This happens when the image or major version \
         changed. Nothing was changed. Keep the service on its current image, or move the data \
         with the major-version upgrade, or dump it and restore it into a new service"
    )]
    DataPathChanged {
        engine: ManagedEngine,
        service: String,
        container: String,
        operation: String,
        volume: String,
        old_path: String,
        new_path: String,
        old_data_dir: String,
        new_data_dir: String,
    },

    #[error(
        "Refusing to recreate container '{container}' of {engine} service '{service}' ({operation}): \
         {destination} (inside the data directory {data_dir}) is mounted from {storage}, which the \
         replacement would not mount at the same place. Nothing was changed. Recreate this \
         container by hand with the same mounts, or move that data into volume '{volume}' first"
    )]
    NestedMountNotCarriedOver {
        engine: ManagedEngine,
        service: String,
        container: String,
        operation: String,
        data_dir: String,
        destination: String,
        storage: String,
        volume: String,
    },

    #[error(
        "Refusing to recreate container '{container}' of {engine} service '{service}' ({operation}): \
         the replacement specification mounts nothing over its data directory {data_dir}. This is \
         a bug in Temps; nothing was changed"
    )]
    ReplacementWithoutDataMount {
        engine: ManagedEngine,
        service: String,
        container: String,
        operation: String,
        data_dir: String,
    },

    #[error(
        "Refusing to recreate container '{container}' of {engine} service '{service}' ({operation}): \
         tablespace location {location} is outside volume '{volume}' (mounted at {mount}), so a \
         replacement would not see it. Nothing was changed. Move the tablespace into the data \
         volume, or dump and restore the database into a new service"
    )]
    TablespaceNotCarriedOver {
        engine: ManagedEngine,
        service: String,
        container: String,
        operation: String,
        location: String,
        volume: String,
        mount: String,
    },

    #[error("Failed to inspect container '{container}' of service '{service}' before {operation}: {reason}. Nothing was changed")]
    Inspect {
        service: String,
        container: String,
        operation: String,
        reason: String,
    },

    #[error(
        "Failed to set aside container '{container}' of service '{service}' before {operation}: \
         {reason}. The original container was left in place{restored}"
    )]
    SetAside {
        service: String,
        container: String,
        operation: String,
        reason: String,
        restored: String,
        /// The original is back under its name, as it was (verified).
        original_in_place: bool,
    },

    #[error(
        "Recreating container '{container}' of {engine} service '{service}' ({operation}) failed: \
         {reason}. {rollback}"
    )]
    ReplacementFailed {
        engine: ManagedEngine,
        service: String,
        container: String,
        operation: String,
        reason: String,
        rollback: String,
        /// The rollback completed: the original is back under its name, as
        /// it was, with nothing else running on its volume (verified).
        original_in_place: bool,
    },

    #[error(
        "The recreated container '{container}' of {engine} service '{service}' ({operation}) did \
         not come up with the existing data: {reason}. {rollback}"
    )]
    DataNotPreserved {
        engine: ManagedEngine,
        service: String,
        container: String,
        operation: String,
        reason: String,
        rollback: String,
        /// The rollback completed: the original is back under its name, as
        /// it was, with nothing else running on its volume (verified).
        original_in_place: bool,
    },
}

impl RecreateGuardError {
    /// A refusal decided from the container's layout, before anything was
    /// changed. (A Docker API failure is not one: it says nothing about
    /// where the data is.)
    pub fn is_refusal(&self) -> bool {
        matches!(
            self,
            RecreateGuardError::DataInContainerLayer { .. }
                | RecreateGuardError::StorageNotCarriedOver { .. }
                | RecreateGuardError::DataPathChanged { .. }
                | RecreateGuardError::NestedMountNotCarriedOver { .. }
                | RecreateGuardError::ReplacementWithoutDataMount { .. }
                | RecreateGuardError::TablespaceNotCarriedOver { .. }
        )
    }

    /// Whether the original container is in place exactly as it was: the
    /// guard refused before changing anything, or a rollback completed and
    /// was verified. Only then may a caller fall back to using the original
    /// (for example, starting it unchanged). Otherwise the state needs an
    /// operator: the original may still be set aside, or a replacement that
    /// failed verification may still hold its name.
    pub fn original_in_place(&self) -> bool {
        match self {
            RecreateGuardError::DataInContainerLayer { .. }
            | RecreateGuardError::StorageNotCarriedOver { .. }
            | RecreateGuardError::DataPathChanged { .. }
            | RecreateGuardError::NestedMountNotCarriedOver { .. }
            | RecreateGuardError::ReplacementWithoutDataMount { .. }
            | RecreateGuardError::TablespaceNotCarriedOver { .. }
            | RecreateGuardError::Inspect { .. } => true,
            RecreateGuardError::SetAside {
                original_in_place, ..
            }
            | RecreateGuardError::ReplacementFailed {
                original_in_place, ..
            }
            | RecreateGuardError::DataNotPreserved {
                original_in_place, ..
            } => *original_in_place,
        }
    }
}

/// Decide whether a replacement with `replacement` storage would see the
/// data of `old`. Pure; see the module docs.
pub fn check_storage(
    engine: ManagedEngine,
    service: &str,
    operation: &str,
    old: &ContainerSnapshot,
    replacement: &ReplacementStorage,
) -> Result<StorageMatch, RecreateGuardError> {
    let old_data_dir = engine.data_dir(&old.env, &old.cmd);
    let new_data_dir = normalize_path(&replacement.data_dir);

    let planned = replacement
        .mounts
        .iter()
        .filter(|m| path_within(&new_data_dir, &normalize_path(&m.target)).is_some())
        .max_by_key(|m| normalize_path(&m.target).len())
        .ok_or_else(|| RecreateGuardError::ReplacementWithoutDataMount {
            engine,
            service: service.to_string(),
            container: old.name.clone(),
            operation: operation.to_string(),
            data_dir: new_data_dir.clone(),
        })?;
    let planned_target = normalize_path(&planned.target);
    let new_path = path_within(&new_data_dir, &planned_target).unwrap_or_default();

    let Some(covering) = old
        .mounts
        .iter()
        .filter(|m| path_within(&old_data_dir, &m.destination).is_some())
        .max_by_key(|m| m.destination.len())
    else {
        return Err(RecreateGuardError::DataInContainerLayer {
            engine,
            service: service.to_string(),
            container: old.name.clone(),
            operation: operation.to_string(),
            data_dir: old_data_dir,
        });
    };

    if covering.kind != StorageKind::NamedVolume || covering.source != planned.volume {
        return Err(RecreateGuardError::StorageNotCarriedOver {
            engine,
            service: service.to_string(),
            container: old.name.clone(),
            operation: operation.to_string(),
            data_dir: old_data_dir,
            storage: covering.describe(),
            expected_volume: planned.volume.clone(),
            expected_path: new_path,
        });
    }

    let old_path = path_within(&old_data_dir, &covering.destination).unwrap_or_default();
    if old_path != new_path {
        return Err(RecreateGuardError::DataPathChanged {
            engine,
            service: service.to_string(),
            container: old.name.clone(),
            operation: operation.to_string(),
            volume: planned.volume.clone(),
            old_path,
            new_path,
            old_data_dir,
            new_data_dir,
        });
    }

    // Data split across mounts (a separate WAL or tablespace volume inside
    // the data directory) must arrive at the same place from the same source.
    for nested in old.mounts.iter().filter(|m| {
        m.destination != covering.destination
            && path_within(&m.destination, &old_data_dir).is_some_and(|rel| !rel.is_empty())
    }) {
        let rel = path_within(&nested.destination, &old_data_dir).unwrap_or_default();
        let expected_destination = join_path(&new_data_dir, &rel);
        let carried = nested.kind == StorageKind::NamedVolume
            && replacement.mounts.iter().any(|m| {
                m.volume == nested.source && normalize_path(&m.target) == expected_destination
            });
        if !carried {
            return Err(RecreateGuardError::NestedMountNotCarriedOver {
                engine,
                service: service.to_string(),
                container: old.name.clone(),
                operation: operation.to_string(),
                data_dir: old_data_dir,
                destination: nested.destination.clone(),
                storage: nested.describe(),
                volume: planned.volume.clone(),
            });
        }
    }

    Ok(StorageMatch {
        data_dir: old_data_dir,
        volume: planned.volume.clone(),
        mount_destination: covering.destination.clone(),
        path_in_volume: old_path,
    })
}

/// What must look the same before and after a recreate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataFingerprint {
    /// Identity of the data directory itself, readable even from a stopped
    /// container: the PostgreSQL `pg_control` system identifier, or an
    /// object store's deployment ID. `None` when the engine has none or the
    /// directory holds no initialised data.
    pub identity: Option<String>,
    /// The engine's marker file exists in the data directory (see
    /// `ManagedEngine::marker_file`): it has been initialised.
    pub marker_present: bool,
    /// Objects the data must still contain. `None` when they could not be
    /// read (the container was stopped, or the listing was incomplete).
    pub objects: Option<BTreeSet<String>>,
    /// The container's own log says it initialised a new data directory.
    pub fresh_init: bool,
}

impl DataFingerprint {
    /// Whether there is data worth protecting.
    pub fn has_data(&self) -> bool {
        self.identity.is_some()
            || self.marker_present
            || self.objects.as_ref().is_some_and(|o| !o.is_empty())
    }

    /// What a comparison against this fingerprint can check, for logs and
    /// reports.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(identity) = &self.identity {
            parts.push(format!("identity {identity}"));
        } else if self.marker_present {
            parts.push("initialised data directory".to_string());
        }
        match &self.objects {
            Some(o) => parts.push(format!("{} object(s)", o.len())),
            None => parts.push("objects not readable".to_string()),
        }
        parts.join(", ")
    }
}

/// Compare a fingerprint taken from the original container with the
/// replacement's. `Err` names what differs. Only called when an original
/// existed, so a replacement that initialised a new data directory is
/// always a failure.
pub fn compare_fingerprints(
    before: &DataFingerprint,
    after: &DataFingerprint,
) -> Result<(), String> {
    if let Some(identity) = &before.identity {
        match &after.identity {
            Some(now) if now == identity => {}
            Some(now) => {
                return Err(format!(
                    "the data directory's identity changed from {identity} to {now}, so the \
                     replacement is running newly initialised data"
                ))
            }
            None => {
                return Err(format!(
                    "the replacement has no initialised data in its data directory (the \
                     original's identity was {identity})"
                ))
            }
        }
    }
    if before.marker_present && !after.marker_present {
        return Err(
            "the replacement's data directory is not initialised, but the original's was"
                .to_string(),
        );
    }
    if after.fresh_init {
        return Err(
            "the replacement's log shows it initialised a new, empty data directory".to_string(),
        );
    }
    if let Some(expected) = before.objects.as_ref().filter(|o| !o.is_empty()) {
        let Some(found) = &after.objects else {
            return Err(format!(
                "the replacement's contents could not be read to confirm that {} object(s) \
                 survived",
                expected.len()
            ));
        };
        let missing: Vec<&str> = expected.difference(found).map(String::as_str).collect();
        if !missing.is_empty() {
            return Err(format!(
                "these existed before and are missing now: {}",
                missing.join(", ")
            ));
        }
    }
    Ok(())
}

/// The successful outcome of [`recreate_preserving_data`].
#[derive(Debug, Clone)]
pub struct RecreateReport {
    /// A container existed and was replaced (false = there was none, so the
    /// container was simply created).
    pub replaced: bool,
    pub storage: Option<StorageMatch>,
    pub fingerprint: DataFingerprint,
    /// What was compared before declaring the data intact.
    pub verification: String,
}

/// Everything [`recreate_preserving_data`] needs besides the create step.
pub struct RecreateRequest<'a> {
    pub docker: &'a Docker,
    pub engine: ManagedEngine,
    pub service_name: &'a str,
    pub container_name: &'a str,
    pub replacement: ReplacementStorage,
    /// What the recreate is for, e.g. "enabling WAL archiving". Appears in
    /// every log line and error.
    pub operation: &'a str,
}

/// Inspect `container_name` and check that a replacement with `replacement`
/// storage would see its data, without changing anything. `Ok(None)` when
/// there is no such container.
pub async fn preflight(
    docker: &Docker,
    engine: ManagedEngine,
    service_name: &str,
    container_name: &str,
    replacement: &ReplacementStorage,
    operation: &str,
) -> Result<Option<(ContainerSnapshot, StorageMatch)>, RecreateGuardError> {
    let Some(snapshot) = inspect_snapshot(docker, service_name, container_name, operation).await?
    else {
        return Ok(None);
    };
    let storage = check_storage(engine, service_name, operation, &snapshot, replacement)?;
    if engine == ManagedEngine::Postgres {
        let locations = pg_tablespace_locations(docker, container_name, &storage.data_dir)
            .await
            .map_err(|reason| RecreateGuardError::Inspect {
                service: service_name.to_string(),
                container: container_name.to_string(),
                operation: operation.to_string(),
                reason: format!("reading its tablespaces failed: {reason}"),
            })?;
        check_tablespaces(
            service_name,
            container_name,
            operation,
            &storage,
            replacement,
            &locations,
        )?;
    }
    Ok(Some((snapshot, storage)))
}

/// Tablespaces are absolute symlinks out of the data directory
/// (`$PGDATA/pg_tblspc/<oid>`): a replacement only sees them when they are on
/// the data volume and that volume is mounted at the same place. Pure.
pub fn check_tablespaces(
    service_name: &str,
    container_name: &str,
    operation: &str,
    storage: &StorageMatch,
    replacement: &ReplacementStorage,
    locations: &[String],
) -> Result<(), RecreateGuardError> {
    let carried_at_same_place = replacement.mounts.iter().any(|m| {
        m.volume == storage.volume && normalize_path(&m.target) == storage.mount_destination
    });
    for location in locations {
        let inside_data_dir = path_within(location, &storage.data_dir).is_some();
        let on_volume = path_within(location, &storage.mount_destination).is_some();
        if !(inside_data_dir || (carried_at_same_place && on_volume)) {
            return Err(RecreateGuardError::TablespaceNotCarriedOver {
                engine: ManagedEngine::Postgres,
                service: service_name.to_string(),
                container: container_name.to_string(),
                operation: operation.to_string(),
                location: location.clone(),
                volume: storage.volume.clone(),
                mount: storage.mount_destination.clone(),
            });
        }
    }
    Ok(())
}

/// Where each tablespace of the cluster in `data_dir` lives, read from the
/// `pg_tblspc` links through the archive API, so it works on a stopped
/// container too. Relative links (in-place tablespaces) are inside the data
/// directory and resolved against it.
async fn pg_tablespace_locations(
    docker: &Docker,
    container: &str,
    data_dir: &str,
) -> Result<Vec<String>, String> {
    let mut stream = docker.download_from_container(
        container,
        Some(DownloadFromContainerOptions {
            path: format!("{data_dir}/pg_tblspc"),
        }),
    );
    let mut archive = Vec::new();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => {
                if archive.len() + bytes.len() > SMALL_FILE_LIMIT {
                    return Err("pg_tblspc is unexpectedly large".to_string());
                }
                archive.extend_from_slice(&bytes);
            }
            // No cluster yet: nothing to carry over.
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        }
    }
    tablespace_links(&archive, data_dir)
}

/// The link targets in a tar of `pg_tblspc`, as absolute normalised paths.
pub fn tablespace_links(archive: &[u8], data_dir: &str) -> Result<Vec<String>, String> {
    let mut tar = tar::Archive::new(archive);
    let entries = tar.entries().map_err(|e| e.to_string())?;
    let mut locations = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.header().entry_type().is_symlink() {
            continue;
        }
        let Some(target) = entry.link_name().map_err(|e| e.to_string())? else {
            continue;
        };
        let target = target.to_string_lossy().to_string();
        locations.push(if target.starts_with('/') {
            normalize_path(&target)
        } else {
            normalize_path(&format!("{data_dir}/pg_tblspc/{target}"))
        });
    }
    Ok(locations)
}

/// Replace a managed container without ever losing its data; see the module
/// docs. `create` must create, start and wait for a container named
/// `container_name` from the service's current specification. It runs after
/// the original has been set aside under another name, so it always takes
/// its "create new" branch.
///
/// Recreates of one container are serialised in this process: two of them
/// interleaving (a scheduled and a manual backup, a backup and a start-up
/// reconcile) could otherwise run two servers on one data directory. A
/// recreate whose create step recreates the same container again (a
/// drift reconcile inside a re-initialisation) runs inside the outer one's
/// turn instead of waiting for itself.
pub async fn recreate_preserving_data<F, Fut, E>(
    request: RecreateRequest<'_>,
    create: F,
) -> Result<RecreateReport, RecreateGuardError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<(), E>>,
    E: fmt::Display,
{
    let name = request.container_name.to_string();
    let reentrant = HELD_RECREATES
        .try_with(|held| held.borrow().contains(&name))
        .unwrap_or(false);
    if reentrant {
        return recreate_locked(request, create, None).await;
    }
    let turn = container_lock(&name).lock_owned().await;
    let mut held = HELD_RECREATES
        .try_with(|held| held.borrow().clone())
        .unwrap_or_default();
    held.insert(name);
    HELD_RECREATES
        .scope(
            std::cell::RefCell::new(held),
            recreate_locked(request, create, Some(turn)),
        )
        .await
}

/// Finishes a recreate that its caller abandoned. Armed once the original
/// has been set aside: if the future driving the recreate is dropped before
/// it verified the replacement or rolled back (a cancelled backup, a client
/// that disconnected), dropping this puts the original back on a task of its
/// own, still holding the container's turn so no other recreate starts on
/// the half-finished state.
struct PendingRollback {
    docker: Docker,
    old: ContainerSnapshot,
    aside_name: String,
    volume: String,
    turn: Option<tokio::sync::OwnedMutexGuard<()>>,
    /// Set by [`set_aside`] just before it asks Docker to stop the original.
    /// That request keeps running in the daemon when this future is dropped.
    stop_requested: std::sync::Arc<std::sync::atomic::AtomicBool>,
    armed: bool,
}

impl PendingRollback {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PendingRollback {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let docker = self.docker.clone();
        let old = self.old.clone();
        let aside_name = self.aside_name.clone();
        let volume = self.volume.clone();
        let turn = self.turn.take();
        let stop_requested = self
            .stop_requested
            .load(std::sync::atomic::Ordering::SeqCst);
        error!(
            container = %old.name,
            set_aside_as = %aside_name,
            stop_requested,
            "Recreate abandoned before it finished; restoring the original container"
        );
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    // Held until the original is back.
                    let _turn = turn;
                    // A stop the abandoned future asked for is still running
                    // in the daemon and would stop the original right after
                    // a restart. Restarts are disabled on it, so wait until
                    // that stop has landed (or cannot still be in flight),
                    // then restore once. Nothing is replayed later: an
                    // operator's own stop after this is theirs to keep.
                    if stop_requested && old.running {
                        wait_until_stopped(&docker, &old.id).await;
                    }
                    let outcome = roll_back(&docker, &old, &aside_name, &volume).await;
                    if outcome.restored {
                        warn!(
                            container = %old.name,
                            outcome = %outcome.message,
                            "Abandoned recreate rolled back"
                        );
                    } else {
                        error!(
                            container = %old.name,
                            outcome = %outcome.message,
                            "Abandoned recreate could not be fully rolled back"
                        );
                    }
                });
            }
            Err(_) => error!(
                container = %old.name,
                set_aside_as = %aside_name,
                "No runtime to restore the original on: it is kept stopped with restarts disabled \
                 under its set-aside name; rename it back and start it"
            ),
        }
    }
}

/// Whether one inspect of the original shows that the stop an abandoned
/// recreate asked for has landed: it reports `running: false`, or the
/// container is gone. A failed or unclear inspect says nothing: the stop
/// may still be in flight, and restarting under it would leave the original
/// stopped when it lands.
fn abandoned_stop_has_landed(
    inspected: &Result<ContainerInspectResponse, bollard::errors::Error>,
) -> bool {
    match inspected {
        Ok(container) => {
            matches!(
                container.state.as_ref().and_then(|s| s.running),
                Some(false)
            )
        }
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => true,
        Err(_) => false,
    }
}

/// Wait until the stop of container `id` that an abandoned recreate asked
/// for has landed, at most [`ABANDONED_STOP_GRACE`]: the length of a stop
/// request, after which Docker has killed the container. An inspect that
/// fails is retried until then rather than taken as "stopped".
async fn wait_until_stopped(docker: &Docker, id: &str) {
    let deadline = std::time::Instant::now() + ABANDONED_STOP_GRACE;
    while std::time::Instant::now() < deadline {
        let inspected = docker
            .inspect_container(id, None::<InspectContainerOptions>)
            .await;
        if abandoned_stop_has_landed(&inspected) {
            return;
        }
        if let Err(e) = &inspected {
            warn!(
                container_id = id,
                error = %e,
                "Could not inspect the original while waiting for an abandoned stop; retrying"
            );
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

tokio::task_local! {
    /// Containers whose recreate this task is currently inside.
    static HELD_RECREATES: std::cell::RefCell<BTreeSet<String>>;
}

/// One lock per container name, for the life of the process. Bounded by the
/// number of managed services.
fn container_lock(container_name: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    static LOCKS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>,
    > = std::sync::OnceLock::new();
    let locks = LOCKS.get_or_init(Default::default);
    let mut map = match locks.lock() {
        Ok(map) => map,
        // A panic elsewhere while holding this map cannot leave it
        // inconsistent: entries are only ever inserted.
        Err(poisoned) => poisoned.into_inner(),
    };
    map.entry(container_name.to_string())
        .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

async fn recreate_locked<F, Fut, E>(
    request: RecreateRequest<'_>,
    create: F,
    turn: Option<tokio::sync::OwnedMutexGuard<()>>,
) -> Result<RecreateReport, RecreateGuardError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<(), E>>,
    E: fmt::Display,
{
    let RecreateRequest {
        docker,
        engine,
        service_name,
        container_name,
        replacement,
        operation,
    } = request;

    let Some((old, storage)) = preflight(
        docker,
        engine,
        service_name,
        container_name,
        &replacement,
        operation,
    )
    .await?
    else {
        // Nothing to replace: a create can only add a container.
        create()
            .await
            .map_err(|e| RecreateGuardError::ReplacementFailed {
                engine,
                service: service_name.to_string(),
                container: container_name.to_string(),
                operation: operation.to_string(),
                reason: e.to_string(),
                rollback: "There was no previous container to restore".to_string(),
                // Nothing to fall back to.
                original_in_place: false,
            })?;
        return Ok(RecreateReport {
            replaced: false,
            storage: None,
            fingerprint: DataFingerprint::default(),
            verification: "no previous container".to_string(),
        });
    };

    info!(
        service = service_name,
        container = container_name,
        engine = %engine,
        operation,
        data_dir = %storage.data_dir,
        volume = %storage.volume,
        "Recreating container: data directory is on the volume the replacement mounts"
    );

    let before = fingerprint(
        docker,
        engine,
        &old.name,
        &storage.data_dir,
        old.running,
        false,
    )
    .await;
    info!(
        service = service_name,
        container = container_name,
        fingerprint = %before.describe(),
        "Captured data fingerprint before recreating"
    );

    let aside_name = set_aside_name(container_name);
    // Armed before the first change and kept armed until a rollback has
    // finished: dropping this future at any await from here on (a cancelled
    // backup, a client that went away) restores the original on its own
    // task. A rollback is idempotent, so a second one is harmless.
    let mut pending = PendingRollback {
        docker: docker.clone(),
        old: old.clone(),
        aside_name: aside_name.clone(),
        volume: storage.volume.clone(),
        turn,
        stop_requested: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        armed: true,
    };
    let stop_requested = pending.stop_requested.clone();
    if let Err(reason) = set_aside(docker, &old, &aside_name, &stop_requested).await {
        let restore = restore_original(docker, &old, &aside_name, &storage.volume).await;
        pending.disarm();
        let (restored, original_in_place) = match restore {
            Ok(()) => (" and restored".to_string(), true),
            Err(e) => (format!("; restoring it also failed: {e}"), false),
        };
        return Err(RecreateGuardError::SetAside {
            service: service_name.to_string(),
            container: container_name.to_string(),
            operation: operation.to_string(),
            reason,
            restored,
            original_in_place,
        });
    }

    if let Err(e) = create().await {
        let rollback = roll_back(docker, &old, &aside_name, &storage.volume).await;
        pending.disarm();
        return Err(RecreateGuardError::ReplacementFailed {
            engine,
            service: service_name.to_string(),
            container: container_name.to_string(),
            operation: operation.to_string(),
            reason: e.to_string(),
            rollback: rollback.message,
            original_in_place: rollback.restored,
        });
    }

    let new_data_dir = normalize_path(&replacement.data_dir);
    let after =
        fingerprint_with_retry(docker, engine, container_name, &new_data_dir, &before).await;
    if let Err(reason) = compare_fingerprints(&before, &after) {
        error!(
            service = service_name,
            container = container_name,
            operation,
            %reason,
            "Recreated container does not hold the original data; rolling back"
        );
        let rollback = roll_back(docker, &old, &aside_name, &storage.volume).await;
        pending.disarm();
        return Err(RecreateGuardError::DataNotPreserved {
            engine,
            service: service_name.to_string(),
            container: container_name.to_string(),
            operation: operation.to_string(),
            reason,
            rollback: rollback.message,
            original_in_place: rollback.restored,
        });
    }

    // Verified: only now is the original removed, and never its volumes.
    pending.disarm();
    if let Err(e) = docker
        .remove_container(
            &aside_name,
            Some(RemoveContainerOptions {
                force: true,
                v: false,
                ..Default::default()
            }),
        )
        .await
    {
        warn!(
            service = service_name,
            container = %aside_name,
            error = %e,
            "Replacement verified, but the stopped original container could not be removed; it \
             is kept stopped with restart disabled and can be removed by hand"
        );
    }

    info!(
        service = service_name,
        container = container_name,
        operation,
        verified = %before.describe(),
        "Recreated container verified: the existing data is intact"
    );
    Ok(RecreateReport {
        replaced: true,
        storage: Some(storage),
        fingerprint: after,
        verification: before.describe(),
    })
}

/// Inspect a container; `Ok(None)` when it does not exist.
pub async fn inspect_snapshot(
    docker: &Docker,
    service_name: &str,
    container_name: &str,
    operation: &str,
) -> Result<Option<ContainerSnapshot>, RecreateGuardError> {
    match docker
        .inspect_container(container_name, None::<InspectContainerOptions>)
        .await
    {
        Ok(inspect) => Ok(Some(ContainerSnapshot::from_inspect(
            container_name,
            &inspect,
        ))),
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => Ok(None),
        Err(e) => Err(RecreateGuardError::Inspect {
            service: service_name.to_string(),
            container: container_name.to_string(),
            operation: operation.to_string(),
            reason: e.to_string(),
        }),
    }
}

/// The image reference a container was created from (`Config.Image`). The
/// list API's `Image` turns into an image ID once the tag has been re-pulled
/// to a newer digest, which must not look like an image change.
pub async fn created_image(docker: &Docker, container_name: &str) -> Option<String> {
    docker
        .inspect_container(container_name, None::<InspectContainerOptions>)
        .await
        .ok()
        .and_then(|c| c.config.and_then(|c| c.image))
}

/// Whether `container_name` already runs exactly the image `image_ref`
/// resolves to now: created from that reference *and* on the same image ID.
/// `None` when there is no such container (or either cannot be inspected).
///
/// An explicit upgrade uses this, not [`created_image`]: a tag re-pulled to
/// a new digest keeps the same reference string, but an operator who asks
/// for that tag again wants the new build. Start-up deliberately compares
/// only the reference, so a moved tag never restarts a service by itself.
pub async fn runs_current_image(
    docker: &Docker,
    container_name: &str,
    image_ref: &str,
) -> Option<bool> {
    let container = docker
        .inspect_container(container_name, None::<InspectContainerOptions>)
        .await
        .ok()?;
    let image = docker.inspect_image(image_ref).await.ok()?;
    let same_reference =
        container.config.as_ref().and_then(|c| c.image.as_deref()) == Some(image_ref);
    let same_build = container.image.is_some() && container.image == image.id;
    Some(same_reference && same_build)
}

/// Remove originals that an interrupted recreate of `container_name` kept
/// aside. Only for deleting the service and its data: anywhere else they may
/// be the only copy that runs. Best effort; failures are logged.
///
/// A container is only taken for one when it is exactly what
/// [`set_aside`] leaves behind: named [`set_aside_name`] for this container
/// (the suffix is a fixed-width timestamp, so another service whose name
/// merely starts the same way never matches), stopped, and with restarts
/// disabled.
pub async fn remove_set_aside_originals(docker: &Docker, container_name: &str, service_name: &str) {
    let containers = match docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            filters: Some(std::collections::HashMap::from([(
                "name".to_string(),
                vec![format!("{container_name}-pre-recreate-")],
            )])),
            ..Default::default()
        }))
        .await
    {
        Ok(c) => c,
        Err(e) => {
            warn!(container = container_name, error = %e, "Could not list set-aside containers");
            return;
        }
    };
    for c in containers {
        let Some(name) = c
            .names
            .unwrap_or_default()
            .into_iter()
            .map(|n| n.trim_start_matches('/').to_string())
            .find(|n| is_set_aside_name(container_name, n))
        else {
            continue;
        };
        let inspect = match docker
            .inspect_container(&name, None::<InspectContainerOptions>)
            .await
        {
            Ok(i) => i,
            Err(e) => {
                warn!(container = %name, error = %e, "Could not inspect a set-aside candidate; leaving it");
                continue;
            }
        };
        let running = inspect
            .state
            .as_ref()
            .and_then(|s| s.running)
            .unwrap_or(true);
        let restart_disabled = inspect
            .host_config
            .as_ref()
            .and_then(|h| h.restart_policy.as_ref())
            .and_then(|p| p.name)
            .is_some_and(|n| matches!(n, RestartPolicyNameEnum::NO | RestartPolicyNameEnum::EMPTY));
        // Temps labels every container it creates with its service's name
        // (MongoDB under `temps.name`); an original set aside for this
        // service carries this one.
        let owner_labels = [
            format!("{}service_name", temps_core::DOCKER_LABEL_PREFIX),
            "temps.name".to_string(),
        ];
        let owned = inspect
            .config
            .as_ref()
            .and_then(|c| c.labels.as_ref())
            .is_some_and(|labels| {
                owner_labels
                    .iter()
                    .any(|key| labels.get(key).is_some_and(|owner| owner == service_name))
            });
        if !owned {
            warn!(
                container = %name,
                service = service_name,
                "Not removing a container named like a set-aside original: it is not labelled as this service's"
            );
            continue;
        }
        if running || !restart_disabled {
            warn!(
                container = %name,
                running,
                restart_disabled,
                "Not removing a container named like a set-aside original: it is not in the stopped, restart-disabled state the recreate guard leaves"
            );
            continue;
        }
        match docker
            .remove_container(
                &name,
                Some(RemoveContainerOptions {
                    force: false,
                    v: false,
                    ..Default::default()
                }),
            )
            .await
        {
            Ok(()) => info!(container = %name, "Removed set-aside original of a deleted service"),
            Err(e) => warn!(container = %name, error = %e, "Could not remove set-aside original"),
        }
    }
}

/// Whether `name` is exactly what [`set_aside_name`] produces for
/// `container_name`: `<container>-pre-recreate-` followed by the 23-digit
/// timestamp and nothing else.
pub fn is_set_aside_name(container_name: &str, name: &str) -> bool {
    name.strip_prefix(container_name)
        .and_then(|rest| rest.strip_prefix("-pre-recreate-"))
        .is_some_and(|stamp| stamp.len() == 23 && stamp.bytes().all(|b| b.is_ascii_digit()))
}

/// Unique even for two recreates of one container within the same second
/// (a recreate whose create step itself reconciles drift).
fn set_aside_name(container_name: &str) -> String {
    let now = chrono::Utc::now();
    format!(
        "{}-pre-recreate-{}{:09}",
        container_name,
        now.format("%Y%m%d%H%M%S"),
        now.timestamp_subsec_nanos()
    )
}

fn no_restart() -> RestartPolicy {
    RestartPolicy {
        name: Some(RestartPolicyNameEnum::NO),
        maximum_retry_count: None,
    }
}

/// Disable restarts (so a daemon restart can never run two servers on one
/// data directory), stop, and rename the original out of the way.
async fn set_aside(
    docker: &Docker,
    old: &ContainerSnapshot,
    aside_name: &str,
    stop_requested: &std::sync::atomic::AtomicBool,
) -> Result<(), String> {
    docker
        .update_container(
            &old.id,
            ContainerUpdateBody {
                restart_policy: Some(no_restart()),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("disabling the restart policy failed: {e}"))?;
    if old.running {
        stop_requested.store(true, std::sync::atomic::Ordering::SeqCst);
        match docker
            .stop_container(
                &old.id,
                Some(StopContainerOptions {
                    t: Some(STOP_TIMEOUT_SECS),
                    ..Default::default()
                }),
            )
            .await
        {
            Ok(())
            | Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304, ..
            }) => {}
            Err(e) => return Err(format!("stopping it failed: {e}")),
        }
    }
    docker
        .rename_container(
            &old.id,
            RenameContainerOptions {
                name: aside_name.to_string(),
            },
        )
        .await
        .map_err(|e| format!("renaming it to '{aside_name}' failed: {e}"))?;
    info!(
        container = %old.name,
        set_aside_as = aside_name,
        "Original container stopped and kept until the replacement is verified"
    );
    Ok(())
}

/// Put the original back exactly as it was: its name, restart policy and
/// running state.
async fn restore_original(
    docker: &Docker,
    old: &ContainerSnapshot,
    aside_name: &str,
    volume: &str,
) -> Result<(), String> {
    // Addressed by ID throughout: the name may be either.
    let current_name = docker
        .inspect_container(&old.id, None::<InspectContainerOptions>)
        .await
        .map_err(|e| format!("inspecting the original ({}) failed: {e}", old.id))?
        .name
        .unwrap_or_default();
    if current_name.trim_start_matches('/') == aside_name {
        docker
            .rename_container(
                &old.id,
                RenameContainerOptions {
                    name: old.name.clone(),
                },
            )
            .await
            .map_err(|e| format!("renaming '{aside_name}' back to '{}' failed: {e}", old.name))?;
    }
    // Never two servers on one data directory: while anything else runs on
    // this volume, the original stays stopped *and* with restarts disabled,
    // or a daemon restart would start it next to the other one.
    let others: Vec<String> = docker
        .list_containers(Some(ListContainersOptions {
            all: false,
            filters: Some(std::collections::HashMap::from([(
                "volume".to_string(),
                vec![volume.to_string()],
            )])),
            ..Default::default()
        }))
        .await
        .map_err(|e| format!("checking who else uses volume '{volume}' failed: {e}"))?
        .into_iter()
        .filter(|c| c.id.as_deref() != Some(old.id.as_str()))
        .flat_map(|c| c.names.unwrap_or_default())
        .collect();
    if !others.is_empty() {
        return Err(format!(
            "left the original stopped with restarts disabled because running container(s) {} \
             also mount volume '{volume}'; stop them, then restore '{}' (restart policy and \
             start)",
            others.join(", "),
            old.name
        ));
    }
    if let Some(policy) = old.restart_policy.clone() {
        docker
            .update_container(
                &old.id,
                ContainerUpdateBody {
                    restart_policy: Some(policy),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| format!("restoring the restart policy failed: {e}"))?;
    }
    if old.running {
        match docker
            .start_container(&old.id, None::<StartContainerOptions>)
            .await
        {
            Ok(()) => {}
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304, ..
            }) => {}
            Err(e) => return Err(format!("starting the original again failed: {e}")),
        }
    }
    Ok(())
}

/// How a rollback ended.
struct Rollback {
    /// The original is back under its name, as it was, with nothing else
    /// running on its volume.
    restored: bool,
    /// A sentence for the error message.
    message: String,
}

/// Remove the replacement (never the original, never with volumes) and
/// restore the original.
async fn roll_back(
    docker: &Docker,
    old: &ContainerSnapshot,
    aside_name: &str,
    volume: &str,
) -> Rollback {
    let mut problems = Vec::new();
    match docker
        .inspect_container(&old.name, None::<InspectContainerOptions>)
        .await
    {
        Ok(replacement) if replacement.id.as_deref() != Some(old.id.as_str()) => {
            if let Err(e) = docker
                .remove_container(
                    &old.name,
                    Some(RemoveContainerOptions {
                        force: true,
                        v: false,
                        ..Default::default()
                    }),
                )
                .await
            {
                problems.push(format!("removing the replacement failed: {e}"));
            }
        }
        Ok(_) => {}
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => {}
        Err(e) => problems.push(format!("inspecting the replacement failed: {e}")),
    }
    if problems.is_empty() {
        if let Err(e) = restore_original(docker, old, aside_name, volume).await {
            problems.push(e);
        }
    }
    if problems.is_empty() {
        Rollback {
            restored: true,
            message: format!(
                "The replacement was removed and the original container '{}' was restored{}; no \
                 data was deleted",
                old.name,
                if old.running { " and restarted" } else { "" }
            ),
        }
    } else {
        error!(
            container = %old.name,
            set_aside_as = aside_name,
            problems = %problems.join("; "),
            "Rolling back a failed container recreate did not complete"
        );
        Rollback {
            restored: false,
            message: format!(
                "Rolling back did not complete ({}). No data was deleted: the original \
                 container is kept stopped with restarts disabled (named '{}' or '{}') with its \
                 volumes; once nothing else runs on volume '{}', restore its name, restart \
                 policy and start it",
                problems.join("; "),
                aside_name,
                old.name,
                volume
            ),
        }
    }
}

async fn fingerprint_with_retry(
    docker: &Docker,
    engine: ManagedEngine,
    container: &str,
    data_dir: &str,
    before: &DataFingerprint,
) -> DataFingerprint {
    let mut latest = DataFingerprint::default();
    for attempt in 1..=FINGERPRINT_ATTEMPTS {
        latest = fingerprint(docker, engine, container, data_dir, true, true).await;
        if compare_fingerprints(before, &latest).is_ok() {
            return latest;
        }
        // An identity mismatch or a fresh-init log is final; only a missing
        // object list can still be a server that is not accepting queries yet.
        let only_objects_unreadable = before.identity == latest.identity
            && !latest.fresh_init
            && (latest.marker_present || !before.marker_present)
            && latest.objects.is_none();
        if !only_objects_unreadable || attempt == FINGERPRINT_ATTEMPTS {
            return latest;
        }
        tokio::time::sleep(FINGERPRINT_RETRY_DELAY).await;
    }
    latest
}

/// Read what [`DataFingerprint`] describes from a container. Never fails:
/// what cannot be read is left `None`, and [`compare_fingerprints`] decides
/// whether that matters.
pub async fn fingerprint(
    docker: &Docker,
    engine: ManagedEngine,
    container: &str,
    data_dir: &str,
    running: bool,
    scan_log: bool,
) -> DataFingerprint {
    let (marker, kind) = engine.marker_file();
    let marker_bytes = read_small_file(docker, container, &format!("{data_dir}/{marker}")).await;
    let marker_present = marker_bytes.is_some();
    let identity = marker_bytes.and_then(|bytes| match kind {
        MarkerKind::PgSystemIdentifier => pg_system_identifier(&bytes),
        MarkerKind::DeploymentId => deployment_id(&bytes),
        MarkerKind::Exists => None,
    });
    let objects = if running {
        read_objects(docker, engine, container, data_dir).await
    } else {
        None
    };
    let fresh_init = scan_log && log_shows_fresh_init(docker, engine, container).await;
    DataFingerprint {
        identity,
        marker_present,
        objects,
        fresh_init,
    }
}

/// The deployment ID in an object store's `format.json`
/// (`{"format":"xl-single","id":"<uuid>",...}`), stable for the lifetime of
/// its data.
pub fn deployment_id(format_json: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(format_json).ok()?;
    value
        .get("id")
        .and_then(|id| id.as_str())
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// The `pg_control` system identifier: the first 8 bytes of the file, a
/// `uint64` in the server's byte order. Kept as hex of the raw bytes so no
/// byte order has to be assumed to compare two of them.
pub fn pg_system_identifier(pg_control: &[u8]) -> Option<String> {
    let bytes = pg_control.get(..8)?;
    if bytes.iter().all(|b| *b == 0) {
        return None;
    }
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

async fn read_objects(
    docker: &Docker,
    engine: ManagedEngine,
    container: &str,
    data_dir: &str,
) -> Option<BTreeSet<String>> {
    let script = engine.objects_script(data_dir);
    let result = super::exec_util::run_exec(
        docker,
        container,
        vec!["sh".into(), "-c".into(), script],
        None,
        Duration::from_secs(30),
    )
    .await;
    match result {
        Ok(out) => parse_objects(&out.output),
        Err(e) => {
            warn!(
                container,
                engine = %engine,
                error = %e,
                "Could not list the data's objects for a recreate fingerprint"
            );
            None
        }
    }
}

/// Collect the `TEMPSFP:` lines of a fingerprint query. `None` unless the
/// query also printed `TEMPSFP_DONE`: a listing cut short (an auth error, a
/// server still loading) must never read as "there is nothing".
pub fn parse_objects(output: &str) -> Option<BTreeSet<String>> {
    let lines: Vec<&str> = output.lines().map(str::trim).collect();
    if !lines.contains(&FINGERPRINT_DONE) {
        return None;
    }
    Some(
        lines
            .iter()
            .filter_map(|line| line.strip_prefix(FINGERPRINT_LINE_PREFIX))
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

async fn log_shows_fresh_init(docker: &Docker, engine: ManagedEngine, container: &str) -> bool {
    let markers = engine.fresh_init_markers();
    if markers.is_empty() {
        return false;
    }
    let mut stream = docker.logs(
        container,
        Some(LogsOptions {
            stdout: true,
            stderr: true,
            tail: LOG_TAIL_LINES.to_string(),
            ..Default::default()
        }),
    );
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else { return false };
        let text = chunk.to_string();
        if markers.iter().any(|m| text.contains(m)) {
            return true;
        }
    }
    false
}

/// Read one small file out of a container (running or stopped) through the
/// archive API. `None` when it does not exist or is unreadable.
async fn read_small_file(docker: &Docker, container: &str, path: &str) -> Option<Vec<u8>> {
    let mut stream = docker.download_from_container(
        container,
        Some(DownloadFromContainerOptions {
            path: path.to_string(),
        }),
    );
    let mut archive = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.ok()?;
        if archive.len() + chunk.len() > SMALL_FILE_LIMIT {
            return None;
        }
        archive.extend_from_slice(&chunk);
    }
    let mut tar = tar::Archive::new(archive.as_slice());
    let mut entry = tar.entries().ok()?.next()?.ok()?;
    let mut contents = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut contents).ok()?;
    Some(contents)
}

/// Docker names anonymous volumes with 64 lowercase hex characters.
fn is_anonymous_volume_name(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn env_value(env: &[String], key: &str) -> Option<String> {
    env.iter()
        .rev()
        .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('=').map(str::to_string))
        .filter(|v| !v.is_empty())
}

/// A data directory given on a PostgreSQL command line, which outranks
/// `PGDATA`: `-D dir`, `-Ddir`, `--pgdata=dir`, `-c data_directory=dir`.
fn pg_cmd_data_dir(cmd: &[String]) -> Option<String> {
    let mut found = None;
    let mut iter = cmd.iter().peekable();
    while let Some(arg) = iter.next() {
        let value = if arg == "-D" || arg == "--pgdata" {
            iter.next().cloned()
        } else if let Some(v) = arg.strip_prefix("--pgdata=") {
            Some(v.to_string())
        } else if let Some(v) = arg.strip_prefix("-D").filter(|v| !v.is_empty()) {
            Some(v.to_string())
        } else if arg == "-c" {
            iter.peek()
                .and_then(|next| next.strip_prefix("data_directory="))
                .map(|v| v.trim_matches(['\'', '"']).to_string())
        } else {
            arg.strip_prefix("--data_directory=")
                .or_else(|| arg.strip_prefix("--data-directory="))
                .map(|v| v.to_string())
        };
        if let Some(v) = value.filter(|v| v.starts_with('/')) {
            // The last occurrence wins, as it does for PostgreSQL.
            found = Some(v);
        }
    }
    found
}

/// `--flag value` or `--flag=value`.
fn flag_value(cmd: &[String], flag: &str) -> Option<String> {
    let mut iter = cmd.iter();
    while let Some(arg) = iter.next() {
        if arg == flag {
            return iter.next().cloned();
        }
        if let Some(value) = arg.strip_prefix(flag).and_then(|r| r.strip_prefix('=')) {
            return Some(value.to_string());
        }
    }
    None
}

/// Collapse repeated and trailing slashes and `.` components. Docker mount
/// destinations are clean absolute paths; data directories from env or
/// command lines may not be.
pub fn normalize_path(path: &str) -> String {
    let parts: Vec<&str> = path
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    format!("/{}", parts.join("/"))
}

/// `path` relative to `base` when `base` is `path` or one of its ancestors.
fn path_within(path: &str, base: &str) -> Option<String> {
    let path = normalize_path(path);
    let base = normalize_path(base);
    if base == "/" {
        return Some(path.trim_start_matches('/').to_string());
    }
    if path == base {
        return Some(String::new());
    }
    path.strip_prefix(&base)
        .and_then(|rest| rest.strip_prefix('/'))
        .map(str::to_string)
}

fn join_path(base: &str, rel: &str) -> String {
    if rel.is_empty() {
        normalize_path(base)
    } else {
        normalize_path(&format!("{base}/{rel}"))
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(env: &[&str], cmd: &[&str], mounts: Vec<ObservedMount>) -> ContainerSnapshot {
        ContainerSnapshot {
            id: "abc".into(),
            name: "postgres-app".into(),
            image: "gotempsh/postgres-walg:18-bookworm".into(),
            env: env.iter().map(|s| s.to_string()).collect(),
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
            mounts,
            running: true,
            restart_policy: None,
        }
    }

    fn volume(name: &str, destination: &str) -> ObservedMount {
        ObservedMount {
            kind: if is_anonymous_volume_name(name) {
                StorageKind::AnonymousVolume
            } else {
                StorageKind::NamedVolume
            },
            source: name.into(),
            destination: destination.into(),
        }
    }

    fn bind(source: &str, destination: &str) -> ObservedMount {
        ObservedMount {
            kind: StorageKind::Bind,
            source: source.into(),
            destination: destination.into(),
        }
    }

    const ANON: &str = "3f1c9e0d2b7a4c6e8f0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f6a";

    /// What Temps creates today for a PostgreSQL 18 service.
    fn pg18_replacement() -> ReplacementStorage {
        ReplacementStorage {
            data_dir: "/var/lib/postgresql/18/docker".into(),
            mounts: vec![PlannedMount {
                volume: "postgres-app_data".into(),
                target: "/var/lib/postgresql".into(),
            }],
        }
    }

    fn check(
        old: &ContainerSnapshot,
        new: &ReplacementStorage,
    ) -> Result<StorageMatch, RecreateGuardError> {
        check_storage(
            ManagedEngine::Postgres,
            "app",
            "enabling WAL archiving",
            old,
            new,
        )
    }

    #[test]
    fn current_layout_is_carried_over() {
        // Nov 2025 onward: volume at /var/lib/postgresql, PGDATA=<major>/docker.
        let old = snapshot(
            &["PGDATA=/var/lib/postgresql/18/docker"],
            &[],
            vec![volume("postgres-app_data", "/var/lib/postgresql")],
        );
        let m = check(&old, &pg18_replacement()).unwrap();
        assert_eq!(m.volume, "postgres-app_data");
        assert_eq!(m.path_in_volume, "18/docker");
        assert_eq!(m.data_dir, "/var/lib/postgresql/18/docker");
    }

    #[test]
    fn layout_from_releases_before_nov_2025_is_refused() {
        // v0.0.1-beta*: volume at /var/lib/postgresql/data, PGDATA from the
        // image (postgres:17-alpine) = /var/lib/postgresql/data. The data is
        // at the volume root; today's spec would look in <volume>/17/docker.
        let old = snapshot(
            &["PGDATA=/var/lib/postgresql/data"],
            &[],
            vec![volume("postgres-app_data", "/var/lib/postgresql/data")],
        );
        let new = ReplacementStorage {
            data_dir: "/var/lib/postgresql/17/docker".into(),
            ..pg18_replacement()
        };
        let err = check(&old, &new).unwrap_err();
        assert!(
            matches!(err, RecreateGuardError::DataPathChanged { ref old_path, ref new_path, .. }
            if old_path.is_empty() && new_path == "17/docker"),
            "{err}"
        );
    }

    #[test]
    fn missing_pgdata_falls_back_to_the_historical_default() {
        let old = snapshot(
            &[],
            &[],
            vec![volume("postgres-app_data", "/var/lib/postgresql/data")],
        );
        assert_eq!(
            ManagedEngine::Postgres.data_dir(&old.env, &old.cmd),
            "/var/lib/postgresql/data"
        );
        assert!(check(&old, &pg18_replacement()).is_err());
    }

    #[test]
    fn data_on_an_image_volume_with_the_named_volume_elsewhere_is_refused() {
        // Named volume mounted somewhere that does not contain PGDATA: the
        // data sits on the image's anonymous VOLUME.
        let old = snapshot(
            &["PGDATA=/var/lib/postgresql/18/docker"],
            &[],
            vec![
                volume(ANON, "/var/lib/postgresql"),
                volume("postgres-app_data", "/var/lib/postgresql/data"),
            ],
        );
        let err = check(&old, &pg18_replacement()).unwrap_err();
        assert!(
            matches!(err, RecreateGuardError::StorageNotCarriedOver { ref storage, .. } if storage.contains("anonymous")),
            "{err}"
        );
        assert!(err.is_refusal());
        assert!(err.to_string().contains("Nothing was changed"));
    }

    #[test]
    fn data_in_the_container_layer_is_refused() {
        let old = snapshot(
            &["PGDATA=/pgdata"],
            &[],
            vec![volume("postgres-app_data", "/var/lib/postgresql")],
        );
        let err = check(&old, &pg18_replacement()).unwrap_err();
        assert!(
            matches!(err, RecreateGuardError::DataInContainerLayer { .. }),
            "{err}"
        );
    }

    #[test]
    fn a_bind_mount_is_refused() {
        let old = snapshot(
            &["PGDATA=/var/lib/postgresql/18/docker"],
            &[],
            vec![bind("/srv/pg", "/var/lib/postgresql")],
        );
        let err = check(&old, &pg18_replacement()).unwrap_err();
        assert!(
            matches!(err, RecreateGuardError::StorageNotCarriedOver { ref storage, .. } if storage.contains("/srv/pg")),
            "{err}"
        );
    }

    #[test]
    fn a_differently_named_volume_is_refused() {
        // Renamed service, adopted legacy container, imported container.
        for name in ["postgres-old-name_data", "app_data", "pgdata"] {
            let old = snapshot(
                &["PGDATA=/var/lib/postgresql/18/docker"],
                &[],
                vec![volume(name, "/var/lib/postgresql")],
            );
            let err = check(&old, &pg18_replacement()).unwrap_err();
            assert!(
                matches!(err, RecreateGuardError::StorageNotCarriedOver { .. }),
                "{name}: {err}"
            );
            assert!(err.to_string().contains(name));
        }
    }

    #[test]
    fn a_changed_major_version_is_refused() {
        let old = snapshot(
            &["PGDATA=/var/lib/postgresql/17/docker"],
            &[],
            vec![volume("postgres-app_data", "/var/lib/postgresql")],
        );
        let err = check(&old, &pg18_replacement()).unwrap_err();
        assert!(
            matches!(err, RecreateGuardError::DataPathChanged { .. }),
            "{err}"
        );
    }

    #[test]
    fn an_image_with_another_pgdata_is_refused() {
        // timescale/timescaledb-ha keeps its data under /home/postgres.
        let old = snapshot(
            &["PGDATA=/home/postgres/pgdata/data"],
            &[],
            vec![volume("postgres-app_data", "/home/postgres/pgdata")],
        );
        let err = check(&old, &pg18_replacement()).unwrap_err();
        assert!(
            matches!(err, RecreateGuardError::DataPathChanged { .. }),
            "{err}"
        );
    }

    #[test]
    fn imported_compose_container_is_refused() {
        let old = snapshot(
            &["PGDATA=/var/lib/postgresql/data"],
            &[],
            vec![
                volume("myproject_db-data", "/var/lib/postgresql/data"),
                bind("/srv/init", "/docker-entrypoint-initdb.d"),
            ],
        );
        assert!(matches!(
            check(&old, &pg18_replacement()).unwrap_err(),
            RecreateGuardError::StorageNotCarriedOver { .. }
        ));
    }

    #[test]
    fn a_wal_volume_inside_the_data_directory_must_be_carried_over() {
        let old = snapshot(
            &["PGDATA=/var/lib/postgresql/18/docker"],
            &[],
            vec![
                volume("postgres-app_data", "/var/lib/postgresql"),
                volume("postgres-app_wal", "/var/lib/postgresql/18/docker/pg_wal"),
            ],
        );
        let err = check(&old, &pg18_replacement()).unwrap_err();
        assert!(
            matches!(err, RecreateGuardError::NestedMountNotCarriedOver { .. }),
            "{err}"
        );

        let mut with_wal = pg18_replacement();
        with_wal.mounts.push(PlannedMount {
            volume: "postgres-app_wal".into(),
            target: "/var/lib/postgresql/18/docker/pg_wal".into(),
        });
        assert!(check(&old, &with_wal).is_ok());
    }

    #[test]
    fn mounts_outside_the_data_directory_do_not_matter() {
        let old = snapshot(
            &["PGDATA=/var/lib/postgresql/18/docker"],
            &[],
            vec![
                volume("postgres-app_data", "/var/lib/postgresql"),
                bind("/etc/localtime", "/etc/localtime"),
            ],
        );
        assert!(check(&old, &pg18_replacement()).is_ok());
    }

    #[test]
    fn same_data_through_a_different_mount_point_is_accepted() {
        // Same volume and same place inside it: the replacement sees the
        // same files even though it mounts the volume elsewhere.
        let old = snapshot(
            &["PGDATA=/pg/18/docker"],
            &[],
            vec![volume("postgres-app_data", "/pg")],
        );
        assert!(check(&old, &pg18_replacement()).is_ok());
    }

    #[test]
    fn a_replacement_without_a_data_mount_is_refused() {
        let old = snapshot(
            &["PGDATA=/var/lib/postgresql/18/docker"],
            &[],
            vec![volume("postgres-app_data", "/var/lib/postgresql")],
        );
        let new = ReplacementStorage {
            data_dir: "/var/lib/postgresql/18/docker".into(),
            mounts: vec![PlannedMount {
                volume: "postgres-app_data".into(),
                target: "/somewhere/else".into(),
            }],
        };
        assert!(matches!(
            check(&old, &new).unwrap_err(),
            RecreateGuardError::ReplacementWithoutDataMount { .. }
        ));
    }

    #[test]
    fn other_engines_resolve_their_data_directories() {
        let e = |engine: ManagedEngine, env: &[&str], cmd: &[&str]| {
            engine.data_dir(
                &env.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                &cmd.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            )
        };
        assert_eq!(
            e(
                ManagedEngine::Redis,
                &[],
                &["redis-server", "--appendonly", "yes"]
            ),
            "/data"
        );
        assert_eq!(
            e(
                ManagedEngine::Redis,
                &[],
                &["redis-server", "--dir", "/srv/r/"]
            ),
            "/srv/r"
        );
        assert_eq!(e(ManagedEngine::MariaDb, &[], &[]), "/var/lib/mysql");
        assert_eq!(e(ManagedEngine::MariaDb, &[], &["--datadir=/db"]), "/db");
        assert_eq!(e(ManagedEngine::MongoDb, &[], &["--auth"]), "/data/db");
        assert_eq!(e(ManagedEngine::MongoDb, &[], &["--dbpath", "/m"]), "/m");
        assert_eq!(
            e(
                ManagedEngine::Minio,
                &[],
                &["server", "/data", "--console-address", ":9001"]
            ),
            "/data"
        );
        assert_eq!(
            e(ManagedEngine::Rustfs, &["RUSTFS_VOLUMES=/data"], &[]),
            "/data"
        );
        assert_eq!(e(ManagedEngine::Rustfs, &[], &[]), "/data");
    }

    #[test]
    fn redis_on_a_different_volume_is_refused() {
        let old = ContainerSnapshot {
            name: "redis-cache".into(),
            ..snapshot(&[], &["redis-server"], vec![volume("cache_redis", "/data")])
        };
        let new = ReplacementStorage {
            data_dir: "/data".into(),
            mounts: vec![PlannedMount {
                volume: "redis_data_cache".into(),
                target: "/data".into(),
            }],
        };
        let err = check_storage(
            ManagedEngine::Redis,
            "cache",
            "changing /dev/shm",
            &old,
            &new,
        )
        .unwrap_err();
        assert!(err.to_string().contains("Redis service 'cache'"), "{err}");
    }

    #[test]
    fn only_exact_set_aside_names_count_as_set_aside_originals() {
        let made = set_aside_name("postgres-app");
        assert!(is_set_aside_name("postgres-app", &made), "{made}");
        // Another service whose name merely starts the same way.
        assert!(!is_set_aside_name(
            "postgres-app",
            "postgres-app-pre-recreate-reporting"
        ));
        assert!(!is_set_aside_name(
            "postgres-app",
            "postgres-app-pre-recreate-20261012114525200595000-x"
        ));
        assert!(!is_set_aside_name(
            "postgres-app",
            "postgres-app-pre-recreate-2026"
        ));
        assert!(!is_set_aside_name("postgres-ap", &made));
        assert!(!is_set_aside_name("postgres-app", "postgres-app"));
    }

    fn tblspc_tar(links: &[(&str, &str)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let mut dir = tar::Header::new_gnu();
        dir.set_entry_type(tar::EntryType::Directory);
        dir.set_size(0);
        builder
            .append_data(&mut dir, "pg_tblspc/", std::io::empty())
            .unwrap();
        for (name, target) in links {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(tar::EntryType::Symlink);
            h.set_size(0);
            builder
                .append_link(&mut h, format!("pg_tblspc/{name}"), target)
                .unwrap();
        }
        builder.into_inner().unwrap()
    }

    #[test]
    fn tablespace_links_are_read_from_pg_tblspc() {
        let archive = tblspc_tar(&[("16390", "/extra/ts"), ("16391", "../pg_tblspc_inplace/x")]);
        let links = tablespace_links(&archive, "/var/lib/postgresql/18/docker").unwrap();
        assert_eq!(
            links,
            vec![
                "/extra/ts".to_string(),
                "/var/lib/postgresql/18/docker/pg_tblspc/../pg_tblspc_inplace/x".to_string()
            ]
        );
        assert!(tablespace_links(&tblspc_tar(&[]), "/d").unwrap().is_empty());
    }

    #[test]
    fn tablespaces_must_live_on_the_carried_volume() {
        let storage = StorageMatch {
            data_dir: "/var/lib/postgresql/18/docker".into(),
            volume: "postgres-app_data".into(),
            mount_destination: "/var/lib/postgresql".into(),
            path_in_volume: "18/docker".into(),
        };
        let check = |locations: &[&str]| {
            check_tablespaces(
                "app",
                "postgres-app",
                "a test",
                &storage,
                &pg18_replacement(),
                &locations.iter().map(|l| l.to_string()).collect::<Vec<_>>(),
            )
        };
        assert!(check(&[]).is_ok());
        assert!(check(&["/var/lib/postgresql/tablespaces/ts1"]).is_ok());
        let err = check(&["/extra/ts"]).unwrap_err();
        assert!(
            matches!(err, RecreateGuardError::TablespaceNotCarriedOver { .. }),
            "{err}"
        );
        assert!(err.is_refusal());
        // On the volume, but the replacement mounts it elsewhere: the
        // absolute link would dangle.
        let moved = ReplacementStorage {
            data_dir: "/pg/18/docker".into(),
            mounts: vec![PlannedMount {
                volume: "postgres-app_data".into(),
                target: "/pg".into(),
            }],
        };
        assert!(check_tablespaces(
            "app",
            "postgres-app",
            "a test",
            &storage,
            &moved,
            &["/var/lib/postgresql/tablespaces/ts1".to_string()],
        )
        .is_err());
    }

    #[test]
    fn an_abandoned_stop_has_landed_only_when_the_inspect_says_so() {
        let inspect = |running: Option<bool>| -> Result<ContainerInspectResponse, _> {
            Ok(ContainerInspectResponse {
                state: Some(bollard::models::ContainerState {
                    running,
                    ..Default::default()
                }),
                ..Default::default()
            })
        };
        assert!(abandoned_stop_has_landed(&inspect(Some(false))));
        assert!(!abandoned_stop_has_landed(&inspect(Some(true))));
        // No state to read: not evidence of anything.
        assert!(!abandoned_stop_has_landed(&inspect(None)));
        assert!(!abandoned_stop_has_landed(&Ok(
            ContainerInspectResponse::default()
        )));
        // Gone for good: nothing is in flight.
        assert!(abandoned_stop_has_landed(&Err(
            bollard::errors::Error::DockerResponseServerError {
                status_code: 404,
                message: "no such container".into(),
            }
        )));
        // A daemon hiccup must not end the wait.
        assert!(!abandoned_stop_has_landed(&Err(
            bollard::errors::Error::DockerResponseServerError {
                status_code: 500,
                message: "daemon busy".into(),
            }
        )));
    }

    #[test]
    fn only_refusals_and_verified_rollbacks_leave_the_original_usable() {
        let refused = check(
            &snapshot(
                &["PGDATA=/pgdata"],
                &[],
                vec![volume("postgres-app_data", "/var/lib/postgresql")],
            ),
            &pg18_replacement(),
        )
        .unwrap_err();
        assert!(refused.original_in_place());
        let failed = |restored: bool| RecreateGuardError::DataNotPreserved {
            engine: ManagedEngine::Postgres,
            service: "app".into(),
            container: "postgres-app".into(),
            operation: "a test".into(),
            reason: "identity changed".into(),
            rollback: "…".into(),
            original_in_place: restored,
        };
        assert!(failed(true).original_in_place());
        // A rollback that did not complete: never fall back to "start it".
        assert!(!failed(false).original_in_place());
        assert!(!failed(false).is_refusal());
    }

    #[test]
    fn anonymous_volume_names_are_recognised() {
        assert!(is_anonymous_volume_name(ANON));
        assert!(!is_anonymous_volume_name("postgres-app_data"));
        assert!(!is_anonymous_volume_name(&ANON.to_uppercase()));
    }

    #[test]
    fn system_identifier_comes_from_the_first_eight_bytes() {
        let mut control = vec![0u8; 8192];
        control[..8].copy_from_slice(&0x7123_4567_89ab_cdefu64.to_le_bytes());
        assert_eq!(
            pg_system_identifier(&control).as_deref(),
            Some("efcdab8967452371")
        );
        assert_eq!(pg_system_identifier(&[0u8; 16]), None);
        assert_eq!(pg_system_identifier(&[1, 2]), None);
    }

    fn fp(identity: Option<&str>, objects: Option<&[&str]>, fresh: bool) -> DataFingerprint {
        DataFingerprint {
            identity: identity.map(str::to_string),
            marker_present: identity.is_some(),
            objects: objects.map(|o| o.iter().map(|s| s.to_string()).collect()),
            fresh_init: fresh,
        }
    }

    #[test]
    fn a_fresh_initdb_never_passes_verification() {
        let before = fp(Some("aa"), Some(&["db:app", "role:app"]), false);
        // New system identifier: initdb ran.
        assert!(compare_fingerprints(
            &before,
            &fp(Some("bb"), Some(&["db:app", "role:app"]), false)
        )
        .is_err());
        // No cluster at all.
        assert!(compare_fingerprints(&before, &fp(None, Some(&["db:app"]), false)).is_err());
        // Log says it bootstrapped.
        assert!(compare_fingerprints(
            &before,
            &fp(Some("aa"), Some(&["db:app", "role:app"]), true)
        )
        .is_err());
        // A database disappeared.
        let err =
            compare_fingerprints(&before, &fp(Some("aa"), Some(&["role:app"]), false)).unwrap_err();
        assert!(err.contains("db:app"), "{err}");
        // Unreadable afterwards is not proof.
        assert!(compare_fingerprints(&before, &fp(Some("aa"), None, false)).is_err());
        // Identical, or with additions, passes.
        assert!(compare_fingerprints(
            &before,
            &fp(Some("aa"), Some(&["db:app", "role:app", "db:x"]), false)
        )
        .is_ok());
    }

    #[test]
    fn nothing_to_protect_passes() {
        assert!(compare_fingerprints(
            &DataFingerprint::default(),
            &fp(Some("bb"), Some(&[]), false)
        )
        .is_ok());
        // An original existed: a replacement that initialised a new data
        // directory fails even when nothing could be read beforehand (a
        // stopped Redis, MariaDB or MongoDB has no identity).
        assert!(compare_fingerprints(&DataFingerprint::default(), &fp(None, None, true)).is_err());
        // Stopped original (objects unknown) still checks the identity.
        assert!(
            compare_fingerprints(&fp(Some("aa"), None, false), &fp(Some("aa"), None, false))
                .is_ok()
        );
        assert!(
            compare_fingerprints(&fp(Some("aa"), None, false), &fp(Some("cc"), None, false))
                .is_err()
        );
    }

    #[test]
    fn fingerprint_lines_are_parsed_and_noise_ignored() {
        let out = "psql: warning: something\nTEMPSFP:db:app\n TEMPSFP:role:app \nTEMPSFP:\nrandom\nTEMPSFP_DONE";
        let parsed = parse_objects(out).unwrap();
        assert_eq!(parsed.len(), 2);
        assert!(parsed.contains("db:app") && parsed.contains("role:app"));
    }

    #[test]
    fn a_listing_without_its_sentinel_is_unreadable_not_empty() {
        assert_eq!(parse_objects("NOAUTH Authentication required."), None);
        assert_eq!(parse_objects("TEMPSFP:db:app"), None);
        assert_eq!(parse_objects("TEMPSFP_DONE"), Some(BTreeSet::new()));
    }

    #[test]
    fn a_stopped_original_is_still_checked_through_its_marker_file() {
        let before = DataFingerprint {
            marker_present: true,
            ..Default::default()
        };
        let empty = DataFingerprint::default();
        assert!(compare_fingerprints(&before, &empty).is_err());
        let same = DataFingerprint {
            marker_present: true,
            objects: Some(BTreeSet::new()),
            ..Default::default()
        };
        assert!(compare_fingerprints(&before, &same).is_ok());
    }

    #[test]
    fn object_store_identity_is_its_deployment_id() {
        let json = br#"{"version":"1","format":"xl-single","id":"b340a955-3a2c-4a60-94ff-2fbf535d3b22","xl":{}}"#;
        assert_eq!(
            deployment_id(json).as_deref(),
            Some("b340a955-3a2c-4a60-94ff-2fbf535d3b22")
        );
        assert_eq!(deployment_id(b"{}"), None);
        assert_eq!(deployment_id(b"not json"), None);
    }

    #[test]
    fn a_postgres_command_line_data_dir_outranks_pgdata() {
        let env = ["PGDATA=/var/lib/postgresql/18/docker".to_string()];
        let dir = |cmd: &[&str]| {
            ManagedEngine::Postgres
                .data_dir(&env, &cmd.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        };
        assert_eq!(
            dir(&["postgres", "-c", "max_connections=50"]),
            "/var/lib/postgresql/18/docker"
        );
        assert_eq!(dir(&["postgres", "-D", "/srv/pg"]), "/srv/pg");
        assert_eq!(dir(&["postgres", "-D/srv/pg2"]), "/srv/pg2");
        assert_eq!(dir(&["postgres", "--pgdata=/srv/pg3"]), "/srv/pg3");
        assert_eq!(
            dir(&["postgres", "-c", "data_directory=/srv/pg4"]),
            "/srv/pg4"
        );
        assert_eq!(
            dir(&["postgres", "-c", "data_directory='/srv/pg5'"]),
            "/srv/pg5"
        );
    }

    #[test]
    fn a_volume_subpath_mount_is_refused() {
        let inspect: ContainerInspectResponse = serde_json::from_value(serde_json::json!({
            "Id": "abc",
            "Config": { "Image": "postgres:18", "Env": ["PGDATA=/var/lib/postgresql/18/docker"] },
            "HostConfig": { "Mounts": [{
                "Type": "volume", "Source": "postgres-app_data", "Target": "/var/lib/postgresql",
                "VolumeOptions": { "Subpath": "tenant-a" }
            }]},
            "Mounts": [{ "Type": "volume", "Name": "postgres-app_data",
                         "Destination": "/var/lib/postgresql" }]
        }))
        .unwrap();
        let old = ContainerSnapshot::from_inspect("postgres-app", &inspect);
        let err = check(&old, &pg18_replacement()).unwrap_err();
        assert!(
            matches!(err, RecreateGuardError::StorageNotCarriedOver { ref storage, .. } if storage.contains("subpath")),
            "{err}"
        );
    }

    #[test]
    fn suggested_commands_quote_container_controlled_values() {
        let old = snapshot(
            &["PGDATA=/x; rm -rf /"],
            &[],
            vec![volume(ANON, "/x; rm -rf /")],
        );
        let message = check(&old, &pg18_replacement()).unwrap_err().to_string();
        assert!(
            message.contains("cp -a '/x; rm -rf /.' '/to/18/docker'"),
            "{message}"
        );
    }

    #[test]
    fn paths_are_compared_by_component() {
        assert_eq!(
            path_within("/var/lib/postgresql/18/docker", "/var/lib/postgresql").as_deref(),
            Some("18/docker")
        );
        assert_eq!(
            path_within("/var/lib/postgresql", "/var/lib/postgresql/").as_deref(),
            Some("")
        );
        assert_eq!(
            path_within("/var/lib/postgresql-x", "/var/lib/postgresql"),
            None
        );
        assert_eq!(path_within("/data", "/").as_deref(), Some("data"));
        assert_eq!(normalize_path("//var//lib/./pg/"), "/var/lib/pg");
    }

    #[test]
    fn env_and_flags_are_read_like_docker_and_engines_do() {
        let env = vec![
            "PGDATA=/a".to_string(),
            "PGDATA=/b".to_string(),
            "PGDATAX=/c".to_string(),
        ];
        assert_eq!(env_value(&env, "PGDATA").as_deref(), Some("/b"));
        assert_eq!(env_value(&["PGDATA=".to_string()], "PGDATA"), None);
        let cmd: Vec<String> = ["mongod", "--dbpath=/x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(flag_value(&cmd, "--dbpath").as_deref(), Some("/x"));
        assert_eq!(flag_value(&cmd, "--dir"), None);
    }
}

/// Docker fixtures for the guard's integration tests and the engines' own.
/// Every container and volume is named from [`test_support::RunScope`], so a
/// test only ever creates, touches and removes its own objects.
#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::HashMap;
    use std::time::Duration;

    use bollard::models::{ContainerCreateBody, HostConfig, Mount, MountTypeEnum, PortBinding};
    use bollard::query_parameters::{
        CreateContainerOptionsBuilder, ListContainersOptions, RemoveContainerOptions,
        StartContainerOptions,
    };
    use bollard::Docker;

    /// Docker from the environment, or `None` (the test prints why and
    /// returns) when it is unreachable. Never `#[ignore]`.
    pub async fn docker_or_skip(test: &str) -> Option<Docker> {
        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => d,
            Err(e) => {
                println!("Docker not available, skipping {test}: {e}");
                return None;
            }
        };
        if docker.ping().await.is_err() {
            println!("Docker not responding, skipping {test}");
            return None;
        }
        Some(docker)
    }

    /// A unique prefix for one test's Docker objects, removed on drop.
    pub struct RunScope {
        pub docker: Docker,
        pub prefix: String,
        volumes: std::sync::Mutex<Vec<String>>,
        binds: std::sync::Mutex<Vec<String>>,
        cleaned: std::sync::atomic::AtomicBool,
    }

    /// A test that panics before its `cleanup().await` still removes what it
    /// created: on a dedicated thread with its own runtime, since `Drop`
    /// cannot await and may run inside the test's runtime.
    impl Drop for RunScope {
        fn drop(&mut self) {
            if self.cleaned.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            let scope = RunScope {
                docker: self.docker.clone(),
                prefix: self.prefix.clone(),
                volumes: std::sync::Mutex::new(
                    self.volumes.lock().map(|v| v.clone()).unwrap_or_default(),
                ),
                binds: std::sync::Mutex::new(
                    self.binds.lock().map(|v| v.clone()).unwrap_or_default(),
                ),
                cleaned: std::sync::atomic::AtomicBool::new(false),
            };
            let cleanup = std::thread::spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt.block_on(scope.cleanup()),
                    // Never retry from this copy's own Drop.
                    Err(_) => scope
                        .cleaned
                        .store(true, std::sync::atomic::Ordering::SeqCst),
                }
            });
            let _ = cleanup.join();
        }
    }

    impl RunScope {
        pub fn new(docker: &Docker, test: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            Self {
                docker: docker.clone(),
                prefix: format!(
                    "rg{}{:x}",
                    test.chars().take(6).collect::<String>(),
                    nanos % 0xffff_ffff
                ),
                volumes: std::sync::Mutex::new(Vec::new()),
                binds: std::sync::Mutex::new(Vec::new()),
                cleaned: std::sync::atomic::AtomicBool::new(false),
            }
        }

        pub fn name(&self, suffix: &str) -> String {
            format!("{}-{}", self.prefix, suffix)
        }

        /// A host directory for a bind mount, removed at the end.
        pub fn bind_dir(&self, suffix: &str) -> String {
            let dir = format!("/tmp/{}", self.name(suffix));
            if let Ok(mut b) = self.binds.lock() {
                b.push(dir.clone());
            }
            dir
        }

        /// Register a volume this test created (or caused to be created) for
        /// removal at the end.
        pub fn track_volume(&self, name: impl Into<String>) {
            if let Ok(mut v) = self.volumes.lock() {
                v.push(name.into());
            }
        }

        /// Remove every container whose name starts with this scope's prefix
        /// (including set-aside originals) and every tracked volume.
        pub async fn cleanup(&self) {
            // Once only: also marks the scope done for `Drop`.
            if self.cleaned.swap(true, std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            let containers = self
                .docker
                .list_containers(Some(ListContainersOptions {
                    all: true,
                    filters: Some(HashMap::from([(
                        "name".to_string(),
                        vec![self.prefix.clone()],
                    )])),
                    ..Default::default()
                }))
                .await
                .unwrap_or_default();
            for c in containers {
                let Some(id) = c.id else { continue };
                // Test-owned containers only: their anonymous volumes go too.
                let _ = self
                    .docker
                    .remove_container(
                        &id,
                        Some(RemoveContainerOptions {
                            force: true,
                            v: true,
                            ..Default::default()
                        }),
                    )
                    .await;
            }
            let binds = self.binds.lock().map(|v| v.clone()).unwrap_or_default();
            if !binds.is_empty() {
                // Bind sources live on the Docker host (a VM on macOS), so
                // remove them from a container that sees that filesystem.
                let script = binds
                    .iter()
                    .map(|b| format!("rm -rf {}", super::shell_quote(b)))
                    .collect::<Vec<_>>()
                    .join("; ");
                let cleaner = self.name("bind-cleanup");
                let mounts = binds
                    .iter()
                    .filter_map(|b| std::path::Path::new(b).parent())
                    .map(|p| p.to_string_lossy().to_string())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .map(|p| bind_mount(&p, &p))
                    .collect();
                ensure_image(&self.docker, "alpine:3.22").await;
                if let Ok(c) = self
                    .docker
                    .create_container(
                        Some(CreateContainerOptionsBuilder::new().name(&cleaner).build()),
                        ContainerCreateBody {
                            image: Some("alpine:3.22".to_string()),
                            cmd: Some(vec!["sh".into(), "-c".into(), script]),
                            host_config: Some(HostConfig {
                                mounts: Some(mounts),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    )
                    .await
                {
                    let _ = self
                        .docker
                        .start_container(&c.id, None::<StartContainerOptions>)
                        .await;
                    let mut wait = self.docker.wait_container(
                        &c.id,
                        None::<bollard::query_parameters::WaitContainerOptions>,
                    );
                    let _ = futures::StreamExt::next(&mut wait).await;
                    let _ = self
                        .docker
                        .remove_container(
                            &c.id,
                            Some(RemoveContainerOptions {
                                force: true,
                                ..Default::default()
                            }),
                        )
                        .await;
                }
            }
            let volumes = self.volumes.lock().map(|v| v.clone()).unwrap_or_default();
            for v in volumes {
                let _ = self
                    .docker
                    .remove_volume(&v, None::<bollard::query_parameters::RemoveVolumeOptions>)
                    .await;
            }
        }
    }

    pub fn volume_mount(source: &str, target: &str) -> Mount {
        Mount {
            source: Some(source.to_string()),
            target: Some(target.to_string()),
            typ: Some(MountTypeEnum::VOLUME),
            ..Default::default()
        }
    }

    pub fn bind_mount(source: &str, target: &str) -> Mount {
        Mount {
            source: Some(source.to_string()),
            target: Some(target.to_string()),
            typ: Some(MountTypeEnum::BIND),
            bind_options: Some(bollard::models::MountBindOptions {
                create_mountpoint: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// Pull `image` unless it is already present.
    pub async fn ensure_image(docker: &Docker, image: &str) {
        if docker.inspect_image(image).await.is_ok() {
            return;
        }
        crate::utils::pull_image_with_retry(docker, image, None)
            .await
            .unwrap_or_else(|e| panic!("pull {image}: {e}"));
    }

    /// Create and start a container the way a user, an older release or
    /// another tool might have, with `restart=always` like Temps' own.
    pub async fn run_container(
        docker: &Docker,
        name: &str,
        image: &str,
        env: &[String],
        cmd: Option<Vec<String>>,
        mounts: Vec<Mount>,
        port: Option<(&str, u16)>,
    ) -> String {
        ensure_image(docker, image).await;
        let port_bindings = port.map(|(container_port, host)| {
            HashMap::from([(
                container_port.to_string(),
                Some(vec![PortBinding {
                    host_ip: Some("127.0.0.1".to_string()),
                    host_port: Some(host.to_string()),
                }]),
            )])
        });
        let created = docker
            .create_container(
                Some(CreateContainerOptionsBuilder::new().name(name).build()),
                ContainerCreateBody {
                    image: Some(image.to_string()),
                    env: Some(env.to_vec()),
                    cmd,
                    exposed_ports: port.map(|(p, _)| vec![p.to_string()]),
                    host_config: Some(HostConfig {
                        mounts: Some(mounts),
                        port_bindings,
                        restart_policy: Some(bollard::models::RestartPolicy {
                            name: Some(bollard::models::RestartPolicyNameEnum::ALWAYS),
                            maximum_retry_count: None,
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap_or_else(|e| panic!("create {name}: {e}"));
        docker
            .start_container(&created.id, None::<StartContainerOptions>)
            .await
            .unwrap_or_else(|e| panic!("start {name}: {e}"));
        created.id
    }

    /// Retry `script` until it exits 0 (a server still starting).
    pub async fn sh_eventually(docker: &Docker, container: &str, script: &str) -> String {
        let mut last = String::new();
        for _ in 0..90 {
            match super::super::exec_util::run_exec(
                docker,
                container,
                vec!["sh".into(), "-c".into(), script.to_string()],
                None,
                Duration::from_secs(30),
            )
            .await
            {
                Ok(out) => return out.output,
                Err(e) => last = e.to_string(),
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        panic!("{container}: `{script}` never succeeded: {last}");
    }

    /// ID of the container currently named `name`, if any.
    pub async fn container_id(docker: &Docker, name: &str) -> Option<String> {
        docker
            .inspect_container(
                name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .ok()
            .and_then(|c| c.id)
    }

    pub async fn is_running(docker: &Docker, name: &str) -> bool {
        docker
            .inspect_container(
                name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .ok()
            .and_then(|c| c.state.and_then(|s| s.running))
            .unwrap_or(false)
    }

    /// Names of containers left over as set-aside originals.
    pub async fn set_aside_containers(docker: &Docker, container: &str) -> Vec<String> {
        docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                filters: Some(HashMap::from([(
                    "name".to_string(),
                    vec![format!("{container}-pre-recreate-")],
                )])),
                ..Default::default()
            }))
            .await
            .unwrap_or_default()
            .into_iter()
            .flat_map(|c| c.names.unwrap_or_default())
            .collect()
    }

    /// PostgreSQL: SQL as the container's own superuser, rows as text.
    pub async fn psql(docker: &Docker, container: &str, sql: &str) -> String {
        let script = format!(
            "PGPASSWORD=\"$POSTGRES_PASSWORD\" psql -X -q -A -t -v ON_ERROR_STOP=1 -U \"${{POSTGRES_USER:-postgres}}\" -d \"${{POSTGRES_DB:-postgres}}\" -c {}",
            super::shell_quote(sql)
        );
        sh_eventually(docker, container, &script)
            .await
            .trim()
            .to_string()
    }
}

#[cfg(test)]
mod docker_tests {
    use super::test_support::*;
    use super::*;

    const PG_IMAGE: &str = "postgres:18";

    fn pg_env() -> Vec<String> {
        vec![
            "POSTGRES_USER=app".to_string(),
            "POSTGRES_PASSWORD=apppass123".to_string(),
            "POSTGRES_DB=app".to_string(),
        ]
    }

    async fn seed(docker: &Docker, container: &str) {
        psql(docker, container, "CREATE TABLE accounts(id int); INSERT INTO accounts SELECT generate_series(1,7); CREATE ROLE reporting LOGIN").await;
    }

    async fn rows(docker: &Docker, container: &str) -> String {
        psql(docker, container, "SELECT count(*) FROM accounts").await
    }

    /// A replacement that claims to mount the original's volume but does not:
    /// the shape of #1415. It must be detected after the fact, removed, and the
    /// original put back, running, with its data.
    #[tokio::test]
    async fn a_replacement_on_other_storage_is_rolled_back() {
        let Some(docker) = docker_or_skip("a_replacement_on_other_storage_is_rolled_back").await
        else {
            return;
        };
        let scope = RunScope::new(&docker, "rollbk");
        let name = scope.name("pg");
        let volume = scope.name("data");
        let decoy = scope.name("decoy");
        scope.track_volume(&volume);
        scope.track_volume(&decoy);

        let original_id = run_container(
            &docker,
            &name,
            PG_IMAGE,
            &pg_env(),
            None,
            vec![volume_mount(&volume, "/var/lib/postgresql")],
            None,
        )
        .await;
        seed(&docker, &name).await;
        assert_eq!(rows(&docker, &name).await, "7");

        let replacement = ReplacementStorage {
            data_dir: "/var/lib/postgresql/18/docker".into(),
            mounts: vec![PlannedMount {
                volume: volume.clone(),
                target: "/var/lib/postgresql".into(),
            }],
        };
        let lie = {
            let docker = docker.clone();
            let name = name.clone();
            let decoy = decoy.clone();
            move || async move {
                run_container(
                    &docker,
                    &name,
                    PG_IMAGE,
                    &pg_env(),
                    None,
                    vec![volume_mount(&decoy, "/var/lib/postgresql")],
                    None,
                )
                .await;
                psql(&docker, &name, "SELECT 1").await;
                Ok::<(), String>(())
            }
        };
        let err = recreate_preserving_data(
            RecreateRequest {
                docker: &docker,
                engine: ManagedEngine::Postgres,
                service_name: "app",
                container_name: &name,
                replacement,
                operation: "a test",
            },
            lie,
        )
        .await
        .expect_err("a fresh cluster must never pass verification");

        assert!(
            matches!(err, RecreateGuardError::DataNotPreserved { .. }),
            "{err}"
        );
        assert!(err.to_string().contains("restored"), "{err}");
        assert_eq!(
            container_id(&docker, &name).await.as_deref(),
            Some(original_id.as_str())
        );
        assert!(is_running(&docker, &name).await);
        assert_eq!(rows(&docker, &name).await, "7");
        assert!(set_aside_containers(&docker, &name).await.is_empty());
        let policy = docker
            .inspect_container(&name, None::<InspectContainerOptions>)
            .await
            .unwrap()
            .host_config
            .and_then(|h| h.restart_policy)
            .and_then(|p| p.name);
        assert_eq!(policy, Some(RestartPolicyNameEnum::ALWAYS));
        scope.cleanup().await;
    }

    /// A replacement that fails to start leaves the original running.
    #[tokio::test]
    async fn a_failed_create_restores_the_original() {
        let Some(docker) = docker_or_skip("a_failed_create_restores_the_original").await else {
            return;
        };
        let scope = RunScope::new(&docker, "failcr");
        let name = scope.name("pg");
        let volume = scope.name("data");
        scope.track_volume(&volume);
        let original_id = run_container(
            &docker,
            &name,
            PG_IMAGE,
            &pg_env(),
            None,
            vec![volume_mount(&volume, "/var/lib/postgresql")],
            None,
        )
        .await;
        seed(&docker, &name).await;

        let err = recreate_preserving_data(
            RecreateRequest {
                docker: &docker,
                engine: ManagedEngine::Postgres,
                service_name: "app",
                container_name: &name,
                replacement: ReplacementStorage {
                    data_dir: "/var/lib/postgresql/18/docker".into(),
                    mounts: vec![PlannedMount {
                        volume: volume.clone(),
                        target: "/var/lib/postgresql".into(),
                    }],
                },
                operation: "a test",
            },
            || async { Err::<(), _>("port is already allocated") },
        )
        .await
        .expect_err("create failed");
        assert!(
            matches!(err, RecreateGuardError::ReplacementFailed { .. }),
            "{err}"
        );
        assert_eq!(
            container_id(&docker, &name).await.as_deref(),
            Some(original_id.as_str())
        );
        assert!(is_running(&docker, &name).await);
        assert_eq!(rows(&docker, &name).await, "7");
        scope.cleanup().await;
    }

    /// Two recreates of one container at once (a scheduled and a manual
    /// backup) take turns: both succeed, one server at a time, same cluster.
    #[tokio::test]
    async fn concurrent_recreates_of_one_container_take_turns() {
        let Some(docker) = docker_or_skip("concurrent_recreates_of_one_container_take_turns").await
        else {
            return;
        };
        let scope = RunScope::new(&docker, "concur");
        let name = scope.name("pg");
        let volume = scope.name("data");
        scope.track_volume(&volume);
        run_container(
            &docker,
            &name,
            PG_IMAGE,
            &pg_env(),
            None,
            vec![volume_mount(&volume, "/var/lib/postgresql")],
            None,
        )
        .await;
        seed(&docker, &name).await;
        let identity = fingerprint(
            &docker,
            ManagedEngine::Postgres,
            &name,
            "/var/lib/postgresql/18/docker",
            true,
            false,
        )
        .await
        .identity;

        let attempt = |tag: &'static str| {
            let (docker, name, volume) = (docker.clone(), name.clone(), volume.clone());
            async move {
                let create = {
                    let (docker, name, volume) = (docker.clone(), name.clone(), volume.clone());
                    move || async move {
                        // Exactly one PostgreSQL may run on the volume.
                        let running = docker
                            .list_containers(Some(
                                bollard::query_parameters::ListContainersOptions {
                                    filters: Some(std::collections::HashMap::from([(
                                        "volume".to_string(),
                                        vec![volume.clone()],
                                    )])),
                                    ..Default::default()
                                },
                            ))
                            .await
                            .map_err(|e| e.to_string())?;
                        if !running.is_empty() {
                            return Err(format!(
                                "{tag}: {} container(s) still on the volume",
                                running.len()
                            ));
                        }
                        run_container(
                            &docker,
                            &name,
                            PG_IMAGE,
                            &pg_env(),
                            None,
                            vec![volume_mount(&volume, "/var/lib/postgresql")],
                            None,
                        )
                        .await;
                        Ok::<(), String>(())
                    }
                };
                recreate_preserving_data(
                    RecreateRequest {
                        docker: &docker,
                        engine: ManagedEngine::Postgres,
                        service_name: "app",
                        container_name: &name,
                        replacement: ReplacementStorage {
                            data_dir: "/var/lib/postgresql/18/docker".into(),
                            mounts: vec![PlannedMount {
                                volume: volume.clone(),
                                target: "/var/lib/postgresql".into(),
                            }],
                        },
                        operation: tag,
                    },
                    create,
                )
                .await
            }
        };
        let (a, b) = tokio::join!(attempt("first"), attempt("second"));
        a.expect("first recreate");
        b.expect("second recreate");
        assert_eq!(rows(&docker, &name).await, "7");
        let after = fingerprint(
            &docker,
            ManagedEngine::Postgres,
            &name,
            "/var/lib/postgresql/18/docker",
            true,
            false,
        )
        .await
        .identity;
        assert_eq!(after, identity);
        assert!(set_aside_containers(&docker, &name).await.is_empty());
        scope.cleanup().await;
    }

    /// Deleting service `app` removes only its own set-aside original: a
    /// running service whose name merely starts with `app-pre-recreate-`,
    /// and a running container with a set-aside name, are left alone.
    #[tokio::test]
    async fn delete_cleanup_takes_only_real_set_aside_originals() {
        let Some(docker) =
            docker_or_skip("delete_cleanup_takes_only_real_set_aside_originals").await
        else {
            return;
        };
        let scope = RunScope::new(&docker, "sweep");
        let base = scope.name("pg");
        let owner = "app";
        let label = format!("{}service_name", temps_core::DOCKER_LABEL_PREFIX);
        // Every container here sleeps; `labelled` decides whether it carries
        // this service's Temps label, `parked` puts it in the state the guard
        // leaves a set-aside original in (stopped, restarts disabled).
        let make = |name: String, labelled: bool, parked: bool| {
            let docker = docker.clone();
            let label = label.clone();
            async move {
                ensure_image(&docker, "alpine:3.22").await;
                let labels =
                    labelled.then(|| std::collections::HashMap::from([(label, owner.to_string())]));
                docker
                    .create_container(
                        Some(
                            bollard::query_parameters::CreateContainerOptionsBuilder::new()
                                .name(&name)
                                .build(),
                        ),
                        bollard::models::ContainerCreateBody {
                            image: Some("alpine:3.22".to_string()),
                            cmd: Some(vec!["sleep".into(), "600".into()]),
                            labels,
                            host_config: Some(bollard::models::HostConfig {
                                restart_policy: Some(if parked {
                                    no_restart()
                                } else {
                                    RestartPolicy {
                                        name: Some(RestartPolicyNameEnum::ALWAYS),
                                        maximum_retry_count: None,
                                    }
                                }),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
                if !parked {
                    docker
                        .start_container(&name, None::<StartContainerOptions>)
                        .await
                        .unwrap();
                }
            }
        };
        // A real set-aside original of this service.
        let real = set_aside_name(&base);
        make(real.clone(), true, true).await;
        // The same name shape and state, but not labelled as this service's.
        let impostor = format!("{base}-pre-recreate-{}", "2".repeat(23));
        make(impostor.clone(), false, true).await;
        // Another service, named so it shares the prefix.
        let other_service = format!("{base}-pre-recreate-reporting");
        make(other_service.clone(), true, false).await;
        // A set-aside name that is running (not left by the guard).
        let running = format!("{base}-pre-recreate-{}", "1".repeat(23));
        make(running.clone(), true, false).await;

        remove_set_aside_originals(&docker, &base, owner).await;

        assert!(
            container_id(&docker, &real).await.is_none(),
            "the stopped original is removed"
        );
        assert!(
            container_id(&docker, &impostor).await.is_some(),
            "an unlabelled container is untouched"
        );
        assert!(
            is_running(&docker, &other_service).await,
            "another service is untouched"
        );
        assert!(
            is_running(&docker, &running).await,
            "a running container is untouched"
        );
        scope.cleanup().await;
    }

    /// The data directory is on the volume, but a tablespace lives in the
    /// container's own filesystem. Stopped, the container can only be read
    /// through the archive API, and the guard must still refuse.
    #[tokio::test]
    async fn a_stopped_cluster_with_a_tablespace_off_the_volume_is_refused() {
        let Some(docker) =
            docker_or_skip("a_stopped_cluster_with_a_tablespace_off_the_volume_is_refused").await
        else {
            return;
        };
        let scope = RunScope::new(&docker, "tblspc");
        let name = scope.name("pg");
        let volume = scope.name("data");
        scope.track_volume(&volume);
        run_container(
            &docker,
            &name,
            PG_IMAGE,
            &pg_env(),
            None,
            vec![volume_mount(&volume, "/var/lib/postgresql")],
            None,
        )
        .await;
        seed(&docker, &name).await;
        sh_eventually(
            &docker,
            &name,
            "mkdir -p /extra/ts && chown postgres:postgres /extra/ts",
        )
        .await;
        psql(
            &docker,
            &name,
            "CREATE TABLESPACE extra LOCATION '/extra/ts'",
        )
        .await;
        psql(
            &docker,
            &name,
            "CREATE TABLE in_extra(x int) TABLESPACE extra; INSERT INTO in_extra VALUES (1)",
        )
        .await;
        docker
            .stop_container(
                &name,
                Some(StopContainerOptions {
                    t: Some(30),
                    ..Default::default()
                }),
            )
            .await
            .unwrap();

        let err = preflight(
            &docker,
            ManagedEngine::Postgres,
            "app",
            &name,
            &ReplacementStorage {
                data_dir: "/var/lib/postgresql/18/docker".into(),
                mounts: vec![PlannedMount {
                    volume: volume.clone(),
                    target: "/var/lib/postgresql".into(),
                }],
            },
            "a test",
        )
        .await
        .expect_err("the tablespace would not be carried over");
        assert!(
            matches!(err, RecreateGuardError::TablespaceNotCarriedOver { ref location, .. } if location == "/extra/ts"),
            "{err}"
        );
        scope.cleanup().await;
    }

    /// A recreate whose caller gives up half-way (a cancelled backup) still
    /// ends with the original back, running, with its data.
    #[tokio::test]
    async fn an_abandoned_recreate_restores_the_original() {
        let Some(docker) = docker_or_skip("an_abandoned_recreate_restores_the_original").await
        else {
            return;
        };
        let scope = RunScope::new(&docker, "cancel");
        let name = scope.name("pg");
        let volume = scope.name("data");
        scope.track_volume(&volume);
        let original_id = run_container(
            &docker,
            &name,
            PG_IMAGE,
            &pg_env(),
            None,
            vec![volume_mount(&volume, "/var/lib/postgresql")],
            None,
        )
        .await;
        seed(&docker, &name).await;

        let abandoned = tokio::time::timeout(
            Duration::from_secs(20),
            recreate_preserving_data(
                RecreateRequest {
                    docker: &docker,
                    engine: ManagedEngine::Postgres,
                    service_name: "app",
                    container_name: &name,
                    replacement: ReplacementStorage {
                        data_dir: "/var/lib/postgresql/18/docker".into(),
                        mounts: vec![PlannedMount {
                            volume: volume.clone(),
                            target: "/var/lib/postgresql".into(),
                        }],
                    },
                    operation: "a test",
                },
                // Never finishes: the caller gives up first.
                || async {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                    Ok::<(), String>(())
                },
            ),
        )
        .await;
        assert!(abandoned.is_err(), "the recreate was abandoned");

        // The rollback runs on its own task; give it time.
        let mut back = false;
        for _ in 0..60 {
            if container_id(&docker, &name).await.as_deref() == Some(original_id.as_str())
                && is_running(&docker, &name).await
            {
                back = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        assert!(back, "the original is back under its name and running");
        assert_eq!(rows(&docker, &name).await, "7");
        assert!(set_aside_containers(&docker, &name).await.is_empty());
        let policy = docker
            .inspect_container(&name, None::<InspectContainerOptions>)
            .await
            .unwrap()
            .host_config
            .and_then(|h| h.restart_policy)
            .and_then(|p| p.name);
        assert_eq!(policy, Some(RestartPolicyNameEnum::ALWAYS));
        scope.cleanup().await;
    }

    /// Cancelled while the original is still being stopped (before it was
    /// renamed): the original ends up back under its name, running, with
    /// its restart policy, even though the daemon finished that stop later.
    #[tokio::test]
    async fn a_recreate_abandoned_while_stopping_restores_the_original() {
        let Some(docker) =
            docker_or_skip("a_recreate_abandoned_while_stopping_restores_the_original").await
        else {
            return;
        };
        let scope = RunScope::new(&docker, "midstop");
        let name = scope.name("svc");
        let volume = scope.name("data");
        scope.track_volume(&volume);
        // `sleep` as PID 1 ignores SIGTERM: a stop takes the full timeout.
        let original_id = run_container(
            &docker,
            &name,
            "alpine:3.22",
            &[],
            Some(vec!["sleep".into(), "3600".into()]),
            vec![volume_mount(&volume, "/data")],
            None,
        )
        .await;

        let abandoned = tokio::time::timeout(
            Duration::from_secs(3),
            recreate_preserving_data(
                RecreateRequest {
                    docker: &docker,
                    engine: ManagedEngine::Redis,
                    service_name: "app",
                    container_name: &name,
                    replacement: ReplacementStorage {
                        data_dir: "/data".into(),
                        mounts: vec![PlannedMount {
                            volume: volume.clone(),
                            target: "/data".into(),
                        }],
                    },
                    operation: "a test",
                },
                || async { Ok::<(), String>(()) },
            ),
        )
        .await;
        assert!(abandoned.is_err(), "abandoned while the stop was in flight");

        let deadline = std::time::Instant::now() + ABANDONED_STOP_GRACE + Duration::from_secs(30);
        let mut back = false;
        while std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let policy = docker
                .inspect_container(&name, None::<InspectContainerOptions>)
                .await
                .ok()
                .and_then(|c| {
                    c.host_config
                        .and_then(|h| h.restart_policy)
                        .and_then(|p| p.name)
                });
            if container_id(&docker, &name).await.as_deref() == Some(original_id.as_str())
                && is_running(&docker, &name).await
                && policy == Some(RestartPolicyNameEnum::ALWAYS)
                && std::time::Instant::now() > deadline - Duration::from_secs(25)
            {
                back = true;
                break;
            }
        }
        assert!(
            back,
            "the original is back, running, with its restart policy, after the late stop"
        );
        assert!(set_aside_containers(&docker, &name).await.is_empty());
        scope.cleanup().await;
    }

    /// The recovery of an abandoned recreate happens once. A stop the
    /// operator issues after the original is back is theirs to keep: nothing
    /// replays the restore from the old snapshot afterwards.
    #[tokio::test]
    async fn a_stop_after_an_abandoned_recreate_is_not_undone() {
        let Some(docker) = docker_or_skip("a_stop_after_an_abandoned_recreate_is_not_undone").await
        else {
            return;
        };
        let scope = RunScope::new(&docker, "laterstop");
        let name = scope.name("svc");
        let volume = scope.name("data");
        scope.track_volume(&volume);
        let original_id = run_container(
            &docker,
            &name,
            "alpine:3.22",
            &[],
            Some(vec!["sleep".into(), "3600".into()]),
            vec![volume_mount(&volume, "/data")],
            None,
        )
        .await;

        let abandoned = tokio::time::timeout(
            Duration::from_secs(3),
            recreate_preserving_data(
                RecreateRequest {
                    docker: &docker,
                    engine: ManagedEngine::Redis,
                    service_name: "app",
                    container_name: &name,
                    replacement: ReplacementStorage {
                        data_dir: "/data".into(),
                        mounts: vec![PlannedMount {
                            volume: volume.clone(),
                            target: "/data".into(),
                        }],
                    },
                    operation: "a test",
                },
                || async { Ok::<(), String>(()) },
            ),
        )
        .await;
        assert!(abandoned.is_err(), "abandoned while the stop was in flight");

        // Wait for the one recovery to finish. The original still answers to
        // its own name while the abandoned stop is in flight, so "running"
        // alone proves nothing: its restart policy was disabled by the set
        // aside and only the recovery puts it back.
        let deadline = std::time::Instant::now() + ABANDONED_STOP_GRACE + Duration::from_secs(30);
        let mut back = false;
        while std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let policy = docker
                .inspect_container(&name, None::<InspectContainerOptions>)
                .await
                .ok()
                .and_then(|c| {
                    c.host_config
                        .and_then(|h| h.restart_policy)
                        .and_then(|p| p.name)
                });
            if container_id(&docker, &name).await.as_deref() == Some(original_id.as_str())
                && is_running(&docker, &name).await
                && policy == Some(RestartPolicyNameEnum::ALWAYS)
            {
                back = true;
                break;
            }
        }
        assert!(back, "the original was put back and is running");

        // The operator stops the service. A recovery that replayed the old
        // "running" snapshot would start it again within the grace period.
        docker
            .stop_container(
                &name,
                Some(StopContainerOptions {
                    t: Some(1),
                    signal: None,
                }),
            )
            .await
            .unwrap();
        tokio::time::sleep(ABANDONED_STOP_GRACE + Duration::from_secs(10)).await;
        assert!(
            !is_running(&docker, &name).await,
            "the operator's stop was undone by a delayed recovery"
        );
        scope.cleanup().await;
    }

    /// Restoring an original while another container runs on its volume
    /// leaves it stopped *with restarts disabled*, so a daemon restart can
    /// never start two servers on one data directory.
    #[tokio::test]
    async fn restoring_beside_another_server_keeps_restarts_disabled() {
        let Some(docker) =
            docker_or_skip("restoring_beside_another_server_keeps_restarts_disabled").await
        else {
            return;
        };
        let scope = RunScope::new(&docker, "shared");
        let name = scope.name("orig");
        let volume = scope.name("data");
        scope.track_volume(&volume);
        let sleep = Some(vec!["sleep".to_string(), "600".to_string()]);
        run_container(
            &docker,
            &name,
            "alpine:3.22",
            &[],
            sleep.clone(),
            vec![volume_mount(&volume, "/data")],
            None,
        )
        .await;
        let snapshot = inspect_snapshot(&docker, "app", &name, "a test")
            .await
            .unwrap()
            .unwrap();
        let aside = set_aside_name(&name);
        set_aside(
            &docker,
            &snapshot,
            &aside,
            &std::sync::atomic::AtomicBool::new(false),
        )
        .await
        .unwrap();
        let other = scope.name("other");
        run_container(
            &docker,
            &other,
            "alpine:3.22",
            &[],
            sleep,
            vec![volume_mount(&volume, "/data")],
            None,
        )
        .await;

        let err = restore_original(&docker, &snapshot, &aside, &volume)
            .await
            .unwrap_err();
        assert!(err.contains(&other), "{err}");
        assert!(!is_running(&docker, &name).await);
        let policy = docker
            .inspect_container(&name, None::<InspectContainerOptions>)
            .await
            .unwrap()
            .host_config
            .and_then(|h| h.restart_policy)
            .and_then(|p| p.name);
        assert_eq!(policy, Some(RestartPolicyNameEnum::NO));
        scope.cleanup().await;
    }

    /// An explicit upgrade to the same tag sees a re-published build.
    #[tokio::test]
    async fn runs_current_image_follows_the_tag_not_only_its_name() {
        let Some(docker) =
            docker_or_skip("runs_current_image_follows_the_tag_not_only_its_name").await
        else {
            return;
        };
        let scope = RunScope::new(&docker, "imgid");
        let name = scope.name("svc");
        let tag = format!("{}:v1", scope.name("img"));
        let (repo, version) = tag.split_once(':').unwrap();
        ensure_image(&docker, "alpine:3.22").await;
        ensure_image(&docker, "redis:7-alpine").await;
        let retag = |source: &'static str| {
            let docker = docker.clone();
            let (repo, version) = (repo.to_string(), version.to_string());
            async move {
                docker
                    .tag_image(
                        source,
                        Some(bollard::query_parameters::TagImageOptions {
                            repo: Some(repo),
                            tag: Some(version),
                        }),
                    )
                    .await
                    .unwrap();
            }
        };
        retag("alpine:3.22").await;
        run_container(
            &docker,
            &name,
            &tag,
            &[],
            Some(vec!["sleep".into(), "600".into()]),
            vec![],
            None,
        )
        .await;
        assert_eq!(runs_current_image(&docker, &name, &tag).await, Some(true));
        assert_eq!(
            created_image(&docker, &name).await.as_deref(),
            Some(tag.as_str())
        );

        // The tag moves to another build; the reference string does not.
        retag("redis:7-alpine").await;
        assert_eq!(
            created_image(&docker, &name).await.as_deref(),
            Some(tag.as_str())
        );
        assert_eq!(runs_current_image(&docker, &name, &tag).await, Some(false));
        assert_eq!(
            runs_current_image(&docker, &scope.name("missing"), &tag).await,
            None
        );

        scope.cleanup().await;
        let _ = docker
            .remove_image(
                &tag,
                None::<bollard::query_parameters::RemoveImageOptions>,
                None,
            )
            .await;
    }

    /// The happy path: same volume, same PGDATA. The original is removed only
    /// after the replacement shows the same system identifier and objects.
    #[tokio::test]
    async fn a_recreate_on_the_same_volume_keeps_the_cluster() {
        let Some(docker) = docker_or_skip("a_recreate_on_the_same_volume_keeps_the_cluster").await
        else {
            return;
        };
        let scope = RunScope::new(&docker, "keeps");
        let name = scope.name("pg");
        let volume = scope.name("data");
        scope.track_volume(&volume);
        let original_id = run_container(
            &docker,
            &name,
            PG_IMAGE,
            &pg_env(),
            None,
            vec![volume_mount(&volume, "/var/lib/postgresql")],
            None,
        )
        .await;
        seed(&docker, &name).await;
        let before = fingerprint(
            &docker,
            ManagedEngine::Postgres,
            &name,
            "/var/lib/postgresql/18/docker",
            true,
            false,
        )
        .await;
        assert!(before.identity.is_some());
        assert!(before.objects.as_ref().unwrap().contains("role:reporting"));

        let create = {
            let docker = docker.clone();
            let name = name.clone();
            let volume = volume.clone();
            move || async move {
                run_container(
                    &docker,
                    &name,
                    PG_IMAGE,
                    &pg_env(),
                    Some(vec![
                        "postgres".into(),
                        "-c".into(),
                        "archive_mode=on".into(),
                    ]),
                    vec![volume_mount(&volume, "/var/lib/postgresql")],
                    None,
                )
                .await;
                Ok::<(), String>(())
            }
        };
        let report = recreate_preserving_data(
            RecreateRequest {
                docker: &docker,
                engine: ManagedEngine::Postgres,
                service_name: "app",
                container_name: &name,
                replacement: ReplacementStorage {
                    data_dir: "/var/lib/postgresql/18/docker".into(),
                    mounts: vec![PlannedMount {
                        volume: volume.clone(),
                        target: "/var/lib/postgresql".into(),
                    }],
                },
                operation: "a test",
            },
            create,
        )
        .await
        .expect("same storage must recreate");
        assert!(report.replaced);
        assert_eq!(report.fingerprint.identity, before.identity);
        assert_ne!(
            container_id(&docker, &name).await.as_deref(),
            Some(original_id.as_str())
        );
        assert_eq!(rows(&docker, &name).await, "7");
        assert_eq!(psql(&docker, &name, "SHOW archive_mode").await, "on");
        assert!(
            set_aside_containers(&docker, &name).await.is_empty(),
            "original removed after verification"
        );
        scope.cleanup().await;
    }
}

/// Every engine through the guard with real containers: a replacement on the
/// same volume keeps the data (and the engine's fingerprint query sees it),
/// and a container whose data is anywhere else is refused untouched.
#[cfg(test)]
mod engine_docker_tests {
    use super::test_support::*;
    use super::*;

    struct Case {
        engine: ManagedEngine,
        image: &'static str,
        env: &'static [&'static str],
        cmd: Option<&'static [&'static str]>,
        data_dir: &'static str,
        /// Writes recognisable data; retried until the server accepts it.
        seed: &'static str,
        /// Prints `ok` when the seeded data is present.
        check: &'static str,
        /// The fingerprint object the seed must produce.
        object: &'static str,
    }

    const REDIS: Case = Case {
        engine: ManagedEngine::Redis,
        image: "redis:7-alpine",
        env: &["REDIS_PASSWORD=redispass123"],
        cmd: Some(&[
            "redis-server",
            "--appendonly",
            "yes",
            "--requirepass",
            "redispass123",
        ]),
        data_dir: "/data",
        seed: "REDISCLI_AUTH=$REDIS_PASSWORD redis-cli SET seeded yes | grep -q OK",
        check: "[ \"$(REDISCLI_AUTH=$REDIS_PASSWORD redis-cli GET seeded)\" = yes ] && echo ok",
        object: "keyspace:db0",
    };

    const MARIADB: Case = Case {
        engine: ManagedEngine::MariaDb,
        image: "mariadb:11.4",
        env: &["MARIADB_ROOT_PASSWORD=rootpass123"],
        cmd: None,
        data_dir: "/var/lib/mysql",
        seed: "mariadb -uroot -p\"$MARIADB_ROOT_PASSWORD\" -e 'CREATE DATABASE IF NOT EXISTS appdb; CREATE TABLE IF NOT EXISTS appdb.t(x int); INSERT INTO appdb.t VALUES (7)'",
        check: "[ \"$(mariadb -uroot -p\"$MARIADB_ROOT_PASSWORD\" -N -e 'SELECT x FROM appdb.t')\" = 7 ] && echo ok",
        object: "db:appdb",
    };

    const MONGODB: Case = Case {
        engine: ManagedEngine::MongoDb,
        image: "mongo:7.0",
        env: &["MONGO_INITDB_ROOT_USERNAME=root", "MONGO_INITDB_ROOT_PASSWORD=rootpass123"],
        cmd: None,
        data_dir: "/data/db",
        seed: "mongosh --quiet -u root -p rootpass123 --authenticationDatabase admin --eval 'db.getSiblingDB(\"appdb\").c.insertOne({x: 7})' | grep -q acknowledged",
        check: "mongosh --quiet -u root -p rootpass123 --authenticationDatabase admin --eval 'db.getSiblingDB(\"appdb\").c.countDocuments({x: 7})' | grep -q '^1$' && echo ok",
        object: "db:appdb",
    };

    const RUSTFS: Case = Case {
        engine: ManagedEngine::Rustfs,
        image: "rustfs/rustfs:1.0.0",
        env: &[],
        cmd: None,
        data_dir: "/data",
        seed: "mkdir -p /data/appbucket && echo payload > /data/appbucket/object.txt",
        check: "grep -q payload /data/appbucket/object.txt && echo ok",
        object: "bucket:appbucket",
    };

    fn env(case: &Case) -> Vec<String> {
        case.env.iter().map(|s| String::from(*s)).collect()
    }

    fn cmd(case: &Case) -> Option<Vec<String>> {
        case.cmd
            .map(|c| c.iter().map(|s| String::from(*s)).collect())
    }

    async fn seeded(docker: &Docker, name: &str, case: &Case) {
        sh_eventually(docker, name, case.seed).await;
        assert_eq!(sh_eventually(docker, name, case.check).await.trim(), "ok");
    }

    /// Same volume: replaced, verified, data intact, original removed.
    async fn keeps_data(case: Case, tag: &str) {
        let Some(docker) = docker_or_skip(tag).await else {
            return;
        };
        let scope = RunScope::new(&docker, tag);
        let name = scope.name("svc");
        let volume = scope.name("data");
        scope.track_volume(&volume);
        let original = run_container(
            &docker,
            &name,
            case.image,
            &env(&case),
            cmd(&case),
            vec![volume_mount(&volume, case.data_dir)],
            None,
        )
        .await;
        seeded(&docker, &name, &case).await;

        let before = fingerprint(&docker, case.engine, &name, case.data_dir, true, false).await;
        let objects = before.objects.clone().unwrap_or_default();
        assert!(
            objects.contains(case.object),
            "{} fingerprint saw {objects:?}",
            case.engine
        );

        let create = {
            let (docker, name, volume) = (docker.clone(), name.clone(), volume.clone());
            let (image, e, c, dir) = (case.image, env(&case), cmd(&case), case.data_dir);
            move || async move {
                run_container(
                    &docker,
                    &name,
                    image,
                    &e,
                    c,
                    vec![volume_mount(&volume, dir)],
                    None,
                )
                .await;
                Ok::<(), String>(())
            }
        };
        let report = recreate_preserving_data(
            RecreateRequest {
                docker: &docker,
                engine: case.engine,
                service_name: "svc",
                container_name: &name,
                replacement: ReplacementStorage {
                    data_dir: case.data_dir.into(),
                    mounts: vec![PlannedMount {
                        volume: volume.clone(),
                        target: case.data_dir.into(),
                    }],
                },
                operation: "a test",
            },
            create,
        )
        .await
        .unwrap_or_else(|e| panic!("{}: same-volume recreate must succeed: {e}", case.engine));
        assert!(report.replaced);
        assert_ne!(
            container_id(&docker, &name).await.as_deref(),
            Some(original.as_str())
        );
        assert_eq!(sh_eventually(&docker, &name, case.check).await.trim(), "ok");
        assert!(set_aside_containers(&docker, &name).await.is_empty());
        scope.cleanup().await;
    }

    /// Data on the image's own anonymous volume (no Temps volume mounted):
    /// refused, nothing touched, data still there.
    async fn refuses_anonymous(case: Case, tag: &str) {
        let Some(docker) = docker_or_skip(tag).await else {
            return;
        };
        let scope = RunScope::new(&docker, tag);
        let name = scope.name("svc");
        let volume = scope.name("data");
        let original = run_container(
            &docker,
            &name,
            case.image,
            &env(&case),
            cmd(&case),
            vec![],
            None,
        )
        .await;
        seeded(&docker, &name, &case).await;

        let mut created = false;
        let err = recreate_preserving_data(
            RecreateRequest {
                docker: &docker,
                engine: case.engine,
                service_name: "svc",
                container_name: &name,
                replacement: ReplacementStorage {
                    data_dir: case.data_dir.into(),
                    mounts: vec![PlannedMount {
                        volume: volume.clone(),
                        target: case.data_dir.into(),
                    }],
                },
                operation: "a test",
            },
            || {
                created = true;
                async { Ok::<(), String>(()) }
            },
        )
        .await
        .expect_err("data off the planned volume must be refused");
        assert!(err.is_refusal(), "{err}");
        assert!(!created, "nothing may be created after a refusal");
        assert_eq!(
            container_id(&docker, &name).await.as_deref(),
            Some(original.as_str())
        );
        assert!(is_running(&docker, &name).await);
        assert_eq!(sh_eventually(&docker, &name, case.check).await.trim(), "ok");
        scope.cleanup().await;
    }

    #[tokio::test]
    async fn redis_same_volume_keeps_data() {
        keeps_data(REDIS, "rdkeep").await
    }
    #[tokio::test]
    async fn redis_other_storage_is_refused() {
        refuses_anonymous(REDIS, "rdanon").await
    }
    #[tokio::test]
    async fn mariadb_same_volume_keeps_data() {
        keeps_data(MARIADB, "mdkeep").await
    }
    #[tokio::test]
    async fn mariadb_other_storage_is_refused() {
        refuses_anonymous(MARIADB, "mdanon").await
    }
    #[tokio::test]
    async fn mongodb_same_volume_keeps_data() {
        keeps_data(MONGODB, "mgkeep").await
    }
    #[tokio::test]
    async fn mongodb_other_storage_is_refused() {
        refuses_anonymous(MONGODB, "mganon").await
    }
    #[tokio::test]
    async fn rustfs_same_volume_keeps_data() {
        keeps_data(RUSTFS, "rfkeep").await
    }
    #[tokio::test]
    async fn rustfs_other_storage_is_refused() {
        refuses_anonymous(RUSTFS, "rfanon").await
    }
}
