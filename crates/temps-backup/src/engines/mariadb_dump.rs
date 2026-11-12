// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `MariadbDumpEngine`: logical (`mariadb-dump`) backup of an external MariaDB
//! service, implemented against `engine_v2::BackupEngine`.
//!
//! This is the **fallback** engine (no PITR), the MariaDB analog of
//! `postgres_pgdump`. The preferred PITR path is `mariadb_physical`
//! (physical `mariadb-backup` base + binary-log archiving). Dispatch
//! (`dispatch::resolve_engine_key`) selects this engine only when the
//! physical-backup prerequisites are absent.
//!
//! ## Flow
//! 1. Load + decrypt the external-service row for the root password.
//! 2. Validate the configured S3 source.
//! 3. The node half (`temps_providers::externalsvc::mariadb_node::dump_backup`):
//!    `mariadb-dump --databases ... --single-transaction | gzip` in the
//!    running container, its stdout streamed to S3 by multipart upload with
//!    no host file. Credentials travel via `MYSQL_PWD` env, never argv
//!    (PR #149). Here for a service on this host; by the node's agent for
//!    one on a worker.
//! 4. Tag the object and write the `metadata.json` companion.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, EntityTrait};
use serde_json::{json, Value};
use tracing::info;

use temps_providers::externalsvc::mariadb_node as node_half;
use temps_providers::remote_service_client::MariadbDumpBackupResult;

use super::mariadb_physical::node_error;
use super::v2_common;
use crate::execution::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};

pub(crate) const ENGINE_KEY: &str = "mariadb_dump";
pub struct MariadbDumpDeps {
    pub db: Arc<DatabaseConnection>,
    pub encryption_service: Arc<temps_core::EncryptionService>,
    /// This host's Docker, for services on this host. `None` on a server
    /// without one, which backs up only services on worker nodes.
    pub docker: Option<bollard::Docker>,
    /// Reaches a worker service's node through its agent.
    pub manager: Arc<temps_providers::ExternalServiceManager>,
    /// Derives the per-job credential a worker's agent receives (ADR-102).
    pub worker_credentials: Arc<temps_providers::worker_s3_scope::WorkerCredentialScoper>,
}

pub struct MariadbDumpEngine {
    deps: Arc<MariadbDumpDeps>,
}

impl MariadbDumpEngine {
    pub fn new(deps: MariadbDumpDeps) -> Self {
        Self {
            deps: Arc::new(deps),
        }
    }
}

#[async_trait]
impl BackupEngine for MariadbDumpEngine {
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

        let (s3_source, s3_client) = v2_common::load_and_build_s3_client(
            deps.db.as_ref(),
            &deps.encryption_service,
            s3_source_id,
            "mariadb-dump-engine",
        )
        .await?;
        v2_common::assert_bucket_reachable(&s3_client, &s3_source.bucket_name).await?;

        let backup_uuid = v2_common::load_backup_uuid(deps.db.as_ref(), backup_id).await?;
        let s3_key = v2_common::build_external_service_s3_key(
            &s3_source.bucket_path,
            "mariadb",
            &service.name,
            &backup_uuid,
            node_half::DUMP_FILE_SUFFIX,
        );

        info!(
            backup_id,
            service_id,
            s3_key = %s3_key,
            "MariadbDumpEngine: starting logical dump",
        );

        let file_size = match super::workload_job::worker_node(&service) {
            None => {
                let Some(docker) = deps.docker.as_ref() else {
                    return Err(BackupError::PermanentFailure {
                        reason: format!(
                            "MariaDB service {service_id} ('{}') runs on this server, which has \
                             no Docker daemon to dump it in",
                            service.name
                        ),
                    });
                };
                let config_json = deps
                    .encryption_service
                    .decrypt_string(service.config.as_deref().unwrap_or("{}"))
                    .unwrap_or_else(|_| "{}".to_string());
                node_half::dump_backup(
                    docker,
                    s3_client.clone(),
                    &node_half::MariadbDump {
                        service_id,
                        service_name: service.name.clone(),
                        container_name: super::dispatch::service_container_name(&service),
                        root_password: root_password_from_config(&config_json),
                        bucket: s3_source.bucket_name.clone(),
                        key: s3_key.clone(),
                    },
                    &ctx.cancel,
                )
                .await
                .map_err(node_error)?
            }
            Some(node_id) => {
                dump_on_worker(
                    &deps,
                    ctx,
                    &service,
                    node_id,
                    s3_source_id,
                    &s3_source.bucket_path,
                    &s3_key,
                )
                .await?
            }
        };
        let file_size = i64::try_from(file_size).map_err(|_| BackupError::Failed {
            reason: format!(
                "mariadb-dump output for backup {backup_id} is larger than i64::MAX bytes"
            ),
        })?;
        if ctx.cancel.is_cancelled() {
            return Err(BackupError::Cancelled);
        }
        let tags = v2_common::BackupTags::load_for_backup(&ctx.db, ctx.backup_id).await;
        v2_common::apply_object_tags(&s3_client, &s3_source.bucket_name, &s3_key, &tags).await?;

        let metadata_key = v2_common::derive_metadata_key(&s3_key);
        v2_common::write_metadata_companion(
            &s3_client,
            &s3_source.bucket_name,
            &metadata_key,
            ENGINE_KEY,
            &backup_uuid,
            &s3_key,
            file_size,
            s3_source_id,
            "gzip",
            Some(json!({
                "backup_tool": "mariadb-dump",
                "pitr": false,
                "service": { "id": service_id, "name": service.name },
            })),
        )
        .await?;

        info!(
            backup_id,
            key = %s3_key,
            size_bytes = file_size,
            "MariadbDumpEngine: backup complete",
        );

        Ok(BackupOutcome {
            location: s3_key,
            size_bytes: Some(file_size),
            compression: "gzip".to_string(),
        })
    }
}

/// The node half, on the worker that runs the service, by its agent.
async fn dump_on_worker(
    deps: &MariadbDumpDeps,
    ctx: &BackupContext,
    service: &temps_entities::external_services::Model,
    node_id: i32,
    s3_source_id: i32,
    bucket_path: &str,
    key: &str,
) -> Result<u64, BackupError> {
    // A credential for the dump's own directory only (ADR-102): the node
    // writes the one key the control plane chose, which it also validates.
    let mint = || {
        super::worker_agent::scoped_destination(
            &deps.worker_credentials,
            deps.db.as_ref(),
            s3_source_id,
            temps_providers::worker_s3_scope::WorkerS3Grant::job(
                service.id,
                node_id,
                temps_providers::worker_s3_scope::key_directory(key),
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
            deps.manager.mariadb_dump_backup_worker(
                service.id,
                target,
                bucket_path.to_string(),
                key.to_string(),
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
            "MariaDB dump of service {} ('{}') on node {node_id}: {e}",
            service.id, service.name
        ),
    })?;
    match result {
        MariadbDumpBackupResult::Completed { size_bytes } => Ok(size_bytes),
        MariadbDumpBackupResult::Refused { reason } => Err(BackupError::PermanentFailure {
            reason: format!(
                "MariaDB dump of service {} ('{}') on node {node_id}: {reason}",
                service.id, service.name
            ),
        }),
        MariadbDumpBackupResult::Failed { reason } => Err(BackupError::Failed {
            reason: format!(
                "MariaDB dump of service {} ('{}') on node {node_id}: {reason}",
                service.id, service.name
            ),
        }),
    }
}

/// Extract the root password from the decrypted service config JSON.
fn root_password_from_config(config_json: &str) -> String {
    let params: Value = serde_json::from_str(config_json).unwrap_or_else(|_| json!({}));
    params
        .get("root_password")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_password_parsed_from_config() {
        assert_eq!(
            root_password_from_config(r#"{"root_password":"s3cr3t"}"#),
            "s3cr3t"
        );
        assert_eq!(root_password_from_config("{}"), "");
        assert_eq!(root_password_from_config("not json"), "");
    }
}
