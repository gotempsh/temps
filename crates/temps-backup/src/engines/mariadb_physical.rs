// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `MariadbPhysicalEngine`: WAL-G repository containing a physical
//! (`mariadb-backup`) base backup of an
//! external MariaDB service, implemented against `engine_v2::BackupEngine`.
//!
//! This is the **PITR** engine — the MariaDB analog of `postgres_walg`.
//! MariaDB has no turnkey continuous archiver (WAL-G drives MariaDB but does
//! not support automatic PITR for it), so PITR here is the standard,
//! MariaDB-documented approach:
//!
//!   physical base backup  +  archived binary logs  +  `mariadb-binlog` replay
//!
//! This engine owns the **base backup** half: it streams a `mariadb-backup`
//! physical snapshot to S3 and records the binlog coordinates
//! (`file`/`position`/`gtid`) at backup time into the `metadata.json`
//! companion. Those coordinates are the replay start for restore. The
//! continuous **binary-log archiving** half lives in `temps-providers`
//! (per-service background task) and ships closed binlog segments to the same
//! S3 prefix.
//!
//! ## Flow
//! 1. Load + decrypt the external-service row for the root password.
//! 2. Validate the configured S3 source.
//! 3. `docker exec wal-g backup-push` inside the running container. WAL-G runs
//!    `mariadb-backup --stream=mbstream` and uploads the stream directly to S3.
//!    No database-sized host file is created.
//! 4. Query `SHOW BINLOG STATUS` directly against the live server for the
//!    binlog position at backup time — NOT by parsing `mariadb-backup`'s own
//!    stderr, which WAL-G's `WALG_STREAM_CREATE_COMMAND` never forwards into
//!    its own captured output.
//! 5. Write `metadata.json` with the coordinates and exact backup identity.

use std::sync::Arc;

use async_trait::async_trait;
use aws_sdk_s3::Client as S3Client;
use sea_orm::{DatabaseConnection, EntityTrait};
use serde_json::{json, Value};
use temps_providers::externalsvc::mariadb_node as node_half;
use temps_providers::remote_service_client::{
    MariadbBinlogCoordinate, MariadbPhysicalBackupResult,
};
use tracing::{info, warn};

use super::dispatch::service_container_name;
use super::v2_common;
use crate::execution::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};

pub(crate) const ENGINE_KEY: &str = "mariadb_physical";

pub struct MariadbPhysicalDeps {
    pub db: Arc<DatabaseConnection>,
    pub encryption_service: Arc<temps_core::EncryptionService>,
    /// This host's Docker, for services on this host. `None` on a server
    /// without one, which backs up only services on worker nodes.
    pub docker: Option<bollard::Docker>,
    /// Reaches a worker service's node through its agent.
    pub manager: Arc<temps_providers::ExternalServiceManager>,
    /// Derives the per-job credential a worker's agent receives (ADR-093).
    pub worker_credentials: Arc<temps_providers::worker_s3_scope::WorkerCredentialScoper>,
}

pub struct MariadbPhysicalEngine {
    deps: Arc<MariadbPhysicalDeps>,
}

impl MariadbPhysicalEngine {
    pub fn new(deps: MariadbPhysicalDeps) -> Self {
        Self {
            deps: Arc::new(deps),
        }
    }
}

#[async_trait]
impl BackupEngine for MariadbPhysicalEngine {
    fn engine(&self) -> &'static str {
        ENGINE_KEY
    }

    async fn run(&self, ctx: &BackupContext) -> Result<BackupOutcome, BackupError> {
        let backup_id = ctx.backup_id;
        let deps = Arc::clone(&self.deps);

        let service_id = v2_common::require_i32_param(&ctx.params, "service_id")?;
        let s3_source_id = v2_common::require_i32_param(&ctx.params, "s3_source_id")?;

        let service = temps_entities::external_services::Entity::find_by_id(service_id)
            .one(deps.db.as_ref())
            .await
            .map_err(|e| BackupError::Failed {
                reason: format!("db error loading service {}: {}", service_id, e),
            })?
            .ok_or_else(|| BackupError::PermanentFailure {
                reason: format!("service {} not found", service_id),
            })?;

        temps_providers::continuous_archive::ensure_continuous_archive_source_pin(
            deps.db.as_ref(),
            &service,
            s3_source_id,
            "MariaDB binlog archiving",
        )
        .await
        .map_err(|e| match e {
            temps_providers::continuous_archive::ContinuousArchiveError::Mismatch(reason) => {
                BackupError::PermanentFailure { reason }
            }
            temps_providers::continuous_archive::ContinuousArchiveError::Database {
                context,
                source,
            } => BackupError::Failed {
                reason: format!("{context}: {source}"),
            },
        })?;

        let (s3_source, s3_client) = v2_common::load_and_build_s3_client(
            deps.db.as_ref(),
            &deps.encryption_service,
            s3_source_id,
            "mariadb-physical-engine",
        )
        .await?;
        v2_common::assert_bucket_reachable(&s3_client, &s3_source.bucket_name).await?;

        let backup_uuid = v2_common::load_backup_uuid(deps.db.as_ref(), backup_id).await?;
        let walg_prefix = node_half::walg_prefix(
            &s3_source.bucket_name,
            &s3_source.bucket_path,
            &service.name,
        );
        let list_prefix = node_half::walg_key_prefix(&s3_source.bucket_path, &service.name);

        info!(
            backup_id,
            service_id,
            repository = %walg_prefix,
            "MariadbPhysicalEngine: starting direct WAL-G physical base backup",
        );

        let config_json = decrypt_service_config(
            &deps.encryption_service,
            service_id,
            service.config.as_deref(),
        )?;
        let root_password = root_password_from_config(service_id, &config_json)?;

        let coord = match super::workload_job::worker_node(&service) {
            None => {
                physical_backup_on_this_host(
                    &deps,
                    ctx,
                    &service,
                    &s3_source,
                    root_password,
                    &backup_uuid,
                )
                .await?
            }
            Some(node_id) => {
                physical_backup_on_worker(
                    &deps,
                    ctx,
                    &service,
                    node_id,
                    s3_source_id,
                    &s3_source.bucket_path,
                    &backup_uuid,
                )
                .await?
            }
        };
        let file_size =
            list_total_s3_size(&s3_client, &s3_source.bucket_name, &list_prefix).await?;
        if file_size <= 0 {
            return Err(BackupError::Failed {
                reason: "MariaDB WAL-G repository contains no backup bytes after backup-push"
                    .into(),
            });
        }

        // Binlog coordinates anchor PITR replay. Absence means binary logging
        // is off on the source — the base is still a valid full backup, but
        // PITR will not be possible until binlog archiving is enabled.
        match &coord {
            Some(c) => info!(
                backup_id,
                binlog_file = %c.file,
                binlog_position = %c.position,
                gtid = %c.gtid,
                "MariadbPhysicalEngine: captured binlog coordinates",
            ),
            None => warn!(
                backup_id,
                "MariadbPhysicalEngine: no binlog position on the source \
                 (binary logging disabled?) — PITR will be unavailable for this base",
            ),
        }

        let metadata_key = format!("{list_prefix}{backup_uuid}.metadata.json");
        v2_common::write_metadata_companion(
            &s3_client,
            &s3_source.bucket_name,
            &metadata_key,
            ENGINE_KEY,
            &backup_uuid,
            &walg_prefix,
            file_size,
            s3_source_id,
            "wal-g-native",
            Some(json!({
                "backup_tool": "wal-g+mariadb-backup",
                "stream_format": "mbstream",
                "pitr": coord.is_some(),
                "binlog_file": coord.as_ref().map(|c| c.file.clone()).unwrap_or_default(),
                "binlog_position": coord.as_ref().map(|c| c.position.clone()).unwrap_or_default(),
                "gtid": coord.as_ref().map(|c| c.gtid.clone()).unwrap_or_default(),
                "service": { "id": service_id, "name": service.name },
            })),
        )
        .await?;
        v2_common::record_walg_identity(deps.db.as_ref(), backup_id, &backup_uuid).await?;

        info!(
            backup_id,
            repository = %walg_prefix,
            size_bytes = file_size,
            pitr = coord.is_some(),
            "MariadbPhysicalEngine: backup complete",
        );

        Ok(BackupOutcome {
            location: walg_prefix,
            size_bytes: Some(file_size),
            compression: "wal-g-native".to_string(),
        })
    }
}

fn decrypt_service_config(
    encryption: &temps_core::EncryptionService,
    service_id: i32,
    encrypted_config: Option<&str>,
) -> Result<String, BackupError> {
    let encrypted_config = encrypted_config.ok_or_else(|| BackupError::PermanentFailure {
        reason: format!("MariaDB service {service_id} has no encrypted configuration"),
    })?;
    encryption
        .decrypt_string(encrypted_config)
        .map_err(|error| BackupError::PermanentFailure {
            reason: format!("decrypt configuration for MariaDB service {service_id}: {error}"),
        })
}

fn root_password_from_config(service_id: i32, config_json: &str) -> Result<String, BackupError> {
    let params: Value =
        serde_json::from_str(config_json).map_err(|error| BackupError::PermanentFailure {
            reason: format!(
                "parse decrypted configuration for MariaDB service {service_id}: {error}"
            ),
        })?;
    params
        .get("root_password")
        .and_then(|v| v.as_str())
        .filter(|password| !password.is_empty())
        .map(str::to_string)
        .ok_or_else(|| BackupError::PermanentFailure {
            reason: format!(
                "decrypted configuration for MariaDB service {service_id} has no root_password"
            ),
        })
}

/// The node half, here, against this host's Docker.
async fn physical_backup_on_this_host(
    deps: &MariadbPhysicalDeps,
    ctx: &BackupContext,
    service: &temps_entities::external_services::Model,
    s3_source: &temps_entities::s3_sources::Model,
    root_password: String,
    backup_uuid: &str,
) -> Result<Option<MariadbBinlogCoordinate>, BackupError> {
    let Some(docker) = deps.docker.as_ref() else {
        return Err(BackupError::PermanentFailure {
            reason: format!(
                "MariaDB service {} ('{}') runs on this server, which has no Docker daemon to \
                 run its physical backup in",
                service.id, service.name
            ),
        });
    };
    let decrypt = |field: &str, value: &str| {
        deps.encryption_service
            .decrypt_string(value)
            .map_err(|error| BackupError::PermanentFailure {
                reason: format!("decrypt MariaDB WAL-G {field}: {error}"),
            })
    };
    let s3 = temps_providers::externalsvc::S3Credentials {
        access_key_id: decrypt("access key", &s3_source.access_key_id)?,
        secret_key: decrypt("secret key", &s3_source.secret_key)?,
        session_token: v2_common::decrypt_session_token(s3_source, &deps.encryption_service)?,
        region: s3_source.region.clone(),
        endpoint: s3_source.endpoint.clone(),
        bucket_name: s3_source.bucket_name.clone(),
        bucket_path: s3_source.bucket_path.clone(),
        force_path_style: s3_source.force_path_style.unwrap_or(true),
    };
    node_half::physical_backup(
        docker,
        &node_half::MariadbPhysicalBackup {
            service_id: service.id,
            container_name: service_container_name(service),
            root_password,
            s3,
            backup_uuid: backup_uuid.to_string(),
        },
        &service.name,
        &ctx.cancel,
    )
    .await
    .map_err(node_error)
}

/// The node half, on the worker that runs the service, by its agent.
async fn physical_backup_on_worker(
    deps: &MariadbPhysicalDeps,
    ctx: &BackupContext,
    service: &temps_entities::external_services::Model,
    node_id: i32,
    s3_source_id: i32,
    bucket_path: &str,
    backup_uuid: &str,
) -> Result<Option<MariadbBinlogCoordinate>, BackupError> {
    // A credential for the service's WAL-G repository only (ADR-093), the
    // one prefix `wal-g backup-push` writes.
    let mint = || {
        super::worker_agent::scoped_destination(
            &deps.worker_credentials,
            deps.db.as_ref(),
            s3_source_id,
            temps_providers::worker_s3_scope::WorkerS3Grant::job(
                service.id,
                node_id,
                node_half::walg_key_prefix(bucket_path, &service.name),
                temps_core::ScopedS3Access::Write,
                "backup",
            ),
            "backup",
        )
    };
    let target = mint().await?;
    // A try after a wait for a slot gets a credential minted after it.
    let request = crate::node_busy::retry_while_node_busy_with_fresh_credential(
        &crate::node_busy::NodeBusyPolicy::DEFAULT,
        target,
        mint,
        |target| {
            deps.manager.mariadb_physical_backup_worker(
                service.id,
                target,
                bucket_path.to_string(),
                backup_uuid.to_string(),
            )
        },
        |wait| super::worker_agent::wait_for_node_slot_minting(ctx.backup_id, service.id, wait),
    );
    let result = tokio::select! {
        // Dropping the request ends it on the node too.
        _ = ctx.cancel.cancelled() => return Err(BackupError::Cancelled),
        result = request => result?,
    }
    .map_err(|e| BackupError::Failed {
        reason: format!(
            "MariaDB physical backup of service {} ('{}') on node {node_id}: {e}",
            service.id, service.name
        ),
    })?;
    match result {
        MariadbPhysicalBackupResult::Completed { binlog } => Ok(binlog),
        MariadbPhysicalBackupResult::Refused { reason } => Err(BackupError::PermanentFailure {
            reason: format!(
                "MariaDB physical backup of service {} ('{}') on node {node_id}: {reason}",
                service.id, service.name
            ),
        }),
        MariadbPhysicalBackupResult::Failed { reason } => Err(BackupError::Failed {
            reason: format!(
                "MariaDB physical backup of service {} ('{}') on node {node_id}: {reason}",
                service.id, service.name
            ),
        }),
    }
}

/// The node half's error as the executor's: a refusal is final, a
/// cancellation is a cancellation, the rest may succeed on a retry.
pub(crate) fn node_error(error: node_half::MariadbNodeError) -> BackupError {
    if error.is_cancelled() {
        BackupError::Cancelled
    } else if error.is_refusal() {
        BackupError::PermanentFailure {
            reason: error.to_string(),
        }
    } else {
        BackupError::Failed {
            reason: error.to_string(),
        }
    }
}

async fn list_total_s3_size(
    client: &S3Client,
    bucket: &str,
    prefix: &str,
) -> Result<i64, BackupError> {
    let mut total = 0_i64;
    let mut continuation = None;
    loop {
        let mut request = client.list_objects_v2().bucket(bucket).prefix(prefix);
        if let Some(token) = continuation {
            request = request.continuation_token(token);
        }
        let response = request.send().await.map_err(|error| BackupError::Failed {
            reason: format!("list MariaDB WAL-G repository {prefix}: {error}"),
        })?;
        for object in response.contents() {
            total = total
                .checked_add(object.size().unwrap_or(0))
                .ok_or_else(|| BackupError::Failed {
                    reason: format!("MariaDB WAL-G repository {prefix} size overflowed i64"),
                })?;
        }
        if response.is_truncated().unwrap_or(false) {
            continuation = response.next_continuation_token().map(str::to_owned);
        } else {
            break;
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrong_encryption_key_is_a_contextual_permanent_failure() {
        let original =
            temps_core::EncryptionService::new_from_password("original-mariadb-config-key");
        let wrong =
            temps_core::EncryptionService::new_from_password("different-mariadb-config-key");
        let encrypted = original
            .encrypt_string(r#"{"root_password":"secret"}"#)
            .unwrap();

        let error = decrypt_service_config(&wrong, 42, Some(&encrypted)).unwrap_err();
        match error {
            BackupError::PermanentFailure { reason } => {
                assert!(reason.contains("MariaDB service 42"));
                assert!(reason.contains("decrypt configuration"));
            }
            other => panic!("expected permanent failure, got {other:?}"),
        }
    }

    #[test]
    fn invalid_decrypted_config_is_not_treated_as_an_empty_object() {
        let error = root_password_from_config(73, "not-json").unwrap_err();
        assert!(matches!(error, BackupError::PermanentFailure { .. }));
        assert!(error.to_string().contains("MariaDB service 73"));
    }
}
