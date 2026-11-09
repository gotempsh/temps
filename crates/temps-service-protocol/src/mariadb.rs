// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The MariaDB node operations (ADR-099): the control plane's own MariaDB
//! backup and restore engines, run by a worker's agent against the container
//! it hosts.
//!
//! Every request names a service and its S3 source, never a command, image,
//! host or path: the container follows from the service's name, the WAL-G
//! repository and binlog prefix from the source's bucket, `bucket_path` and
//! the service's name, and the commands are fixed in the engine. Unknown
//! fields are refused.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{RecoveryTarget, ServiceConfig, WorkerS3Destination};

/// The binary-log position a physical base backup was taken at: where a
/// point-in-time recovery starts replaying.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MariadbBinlogCoordinate {
    pub file: String,
    pub position: String,
    /// The GTID position (`domain-server-seq`); empty when GTIDs are unused.
    #[serde(default)]
    pub gtid: String,
}

/// `POST /agent/services/mariadb/physical-backup`: a WAL-G physical
/// (`mariadb-backup`) base backup of a MariaDB service on the node that runs
/// it, by the control plane's `mariadb_physical` engine.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MariadbPhysicalBackupRequest {
    pub service_id: i32,
    pub node_id: i32,
    pub service_config: ServiceConfig,
    pub destination: WorkerS3Destination,
    /// The S3 source's base path inside its bucket; empty for none.
    #[serde(default)]
    pub bucket_path: String,
    /// The backup's id, recorded in WAL-G's sentinel to find it again.
    pub backup_uuid: String,
}

/// What a [`MariadbPhysicalBackupRequest`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MariadbPhysicalBackupResult {
    /// The base backup was pushed.
    Completed {
        /// The binary-log position at backup time; `None` when binary
        /// logging is off (the base is valid, PITR from it is not).
        binlog: Option<MariadbBinlogCoordinate>,
    },
    /// Nothing ran and retrying cannot change that.
    Refused { reason: String },
    /// The backup failed; a retry may succeed.
    Failed { reason: String },
}

/// `POST /agent/services/mariadb/dump-backup`: a logical `mariadb-dump`
/// backup of a MariaDB service on the node that runs it, by the control
/// plane's `mariadb_dump` engine, streamed to `key` in the destination.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MariadbDumpBackupRequest {
    pub service_id: i32,
    pub node_id: i32,
    pub service_config: ServiceConfig,
    pub destination: WorkerS3Destination,
    /// The S3 source's base path inside its bucket; empty for none.
    #[serde(default)]
    pub bucket_path: String,
    /// The dump's object key: under the service's own prefix in
    /// `bucket_path`, ending in `dump.sql.gz`. Anything else is refused.
    pub key: String,
}

/// What a [`MariadbDumpBackupRequest`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MariadbDumpBackupResult {
    /// The dump was uploaded.
    Completed { size_bytes: u64 },
    /// Nothing ran and retrying cannot change that.
    Refused { reason: String },
    /// The dump failed; a retry may succeed.
    Failed { reason: String },
}

/// `POST /agent/services/mariadb/restore`: the control plane's in-place
/// MariaDB restore (and point-in-time recovery) of a service on the node
/// that runs it.
///
/// A WAL-G base is fetched and prepared into a staging directory, swapped
/// in with the original data kept aside on the same volume, and the
/// original is deleted only once the restored server answers (and, for
/// point-in-time recovery, the binary logs replayed). Otherwise the original
/// is put back and started as it was.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MariadbRestoreRequest {
    pub service_id: i32,
    pub node_id: i32,
    /// The login the restored data answers to: the target's configuration,
    /// with the backup origin's login when a physical backup of another
    /// service brings that service's `mysql` schema.
    pub service_config: ServiceConfig,
    /// The S3 source holding the backup.
    pub source: WorkerS3Destination,
    /// The S3 source's base path inside its bucket; empty for none.
    #[serde(default)]
    pub bucket_path: String,
    /// The backup's location in `source`: a WAL-G repository
    /// (`s3://<bucket>/…/walg`), or the key of a dump or legacy base.
    pub location: String,
    /// The point to recover to; `None` restores the backup as it was.
    #[serde(default)]
    pub recovery_target: Option<RecoveryTarget>,
    /// The sentinel user data that selects the WAL-G base; `None` selects
    /// the latest.
    #[serde(default)]
    pub target_user_data: Option<String>,
}

/// What a [`MariadbRestoreRequest`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MariadbRestoreResult {
    /// The restored server answers; the kept original was deleted.
    Completed,
    /// Nothing was changed: the request cannot run on this service.
    Refused { reason: String },
    /// The restore failed. The original is running again unless the reason
    /// says otherwise.
    Failed { reason: String },
}

/// `POST /agent/services/mariadb/binlog-archive`: ship the closed binary
/// logs of a MariaDB service on the node that runs it to its S3 source, by
/// the control plane's archiver (`FLUSH BINARY LOGS`, then every closed
/// segment the manifest does not list, then the manifest).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MariadbBinlogArchiveRequest {
    pub service_id: i32,
    pub node_id: i32,
    pub service_config: ServiceConfig,
    pub destination: WorkerS3Destination,
    /// The S3 source's base path inside its bucket; empty for none.
    #[serde(default)]
    pub bucket_path: String,
}

/// What a [`MariadbBinlogArchiveRequest`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MariadbBinlogArchiveResult {
    /// `shipped` new segments were uploaded (none when up to date).
    Completed { shipped: usize },
    /// Nothing ran and retrying cannot change that.
    Refused { reason: String },
    /// Shipping failed; the next run retries from the manifest.
    Failed { reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn destination() -> serde_json::Value {
        serde_json::json!({
            "bucket": "backups",
            "region": "us-east-1",
            "force_path_style": true,
            "access_key_id": "AKIAEXAMPLE",
            "secret_access_key": "very-secret"
        })
    }

    fn config() -> serde_json::Value {
        serde_json::json!({
            "name": "shop",
            "service_type": "mariadb",
            "version": null,
            "parameters": {"root_password": "root-secret"}
        })
    }

    /// No request takes a command, image, path or prefix of the caller's.
    #[test]
    fn mariadb_requests_take_nothing_the_node_could_run() {
        let backup = serde_json::json!({
            "service_id": 1, "node_id": 2, "service_config": config(),
            "destination": destination(), "bucket_path": "", "backup_uuid": "u"
        });
        assert!(serde_json::from_value::<MariadbPhysicalBackupRequest>(backup.clone()).is_ok());
        let dump = serde_json::json!({
            "service_id": 1, "node_id": 2, "service_config": config(),
            "destination": destination(), "key": "external_services/mariadb/shop/x/dump.sql.gz"
        });
        assert!(serde_json::from_value::<MariadbDumpBackupRequest>(dump.clone()).is_ok());
        let restore = serde_json::json!({
            "service_id": 1, "node_id": 2, "service_config": config(),
            "source": destination(), "location": "s3://backups/external_services/mariadb/shop/walg",
            "recovery_target": {"kind": "time", "time": "2026-11-07T10:00:00Z"}
        });
        assert!(serde_json::from_value::<MariadbRestoreRequest>(restore.clone()).is_ok());
        let archive = serde_json::json!({
            "service_id": 1, "node_id": 2, "service_config": config(),
            "destination": destination()
        });
        assert!(serde_json::from_value::<MariadbBinlogArchiveRequest>(archive.clone()).is_ok());

        for field in ["command", "image", "walg_prefix", "container", "datadir"] {
            let with = |mut body: serde_json::Value| {
                body[field] = serde_json::json!("x");
                body
            };
            assert!(
                serde_json::from_value::<MariadbPhysicalBackupRequest>(with(backup.clone()))
                    .is_err()
            );
            assert!(
                serde_json::from_value::<MariadbDumpBackupRequest>(with(dump.clone())).is_err()
            );
            assert!(
                serde_json::from_value::<MariadbRestoreRequest>(with(restore.clone())).is_err()
            );
            assert!(
                serde_json::from_value::<MariadbBinlogArchiveRequest>(with(archive.clone()))
                    .is_err()
            );
        }
    }

    /// The destination's secrets never reach a log line of a request.
    #[test]
    fn mariadb_requests_hide_the_destinations_secret() {
        let request: MariadbBinlogArchiveRequest = serde_json::from_value(serde_json::json!({
            "service_id": 1, "node_id": 2, "service_config": config(),
            "destination": destination()
        }))
        .expect("parse");
        let logged = format!("{request:?}");
        assert!(!logged.contains("very-secret"), "{logged}");
    }

    #[test]
    fn outcomes_are_tagged_by_status() {
        let completed: MariadbPhysicalBackupResult = serde_json::from_value(serde_json::json!({
            "status": "completed",
            "binlog": {"file": "mysql-bin.000003", "position": "342", "gtid": "0-1-7"}
        }))
        .expect("parse");
        assert_eq!(
            completed,
            MariadbPhysicalBackupResult::Completed {
                binlog: Some(MariadbBinlogCoordinate {
                    file: "mysql-bin.000003".into(),
                    position: "342".into(),
                    gtid: "0-1-7".into(),
                })
            }
        );
        let refused: MariadbRestoreResult =
            serde_json::from_value(serde_json::json!({"status": "refused", "reason": "r"}))
                .expect("parse");
        assert_eq!(
            refused,
            MariadbRestoreResult::Refused { reason: "r".into() }
        );
    }
}
