// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `worker_agent`: backups of the MongoDB, Redis, MariaDB and managed S3
//! services that run on worker nodes, taken by the worker's agent.
//!
//! The local engines (`redis`, `mongodb`, `mariadb_*`, `s3_mirror`) drive
//! this host's Docker, where a worker's container is not, and a
//! `--profile control-plane` server has no Docker at all. So for a service on
//! a worker this engine asks the node's agent to run the engine's own dump
//! (or the storage service's bucket mirror) on that node, straight into the
//! S3 source (`temps_providers::externalsvc::worker_data`). The request is
//! sealed to the node: it carries the source's decrypted credentials,
//! session token included. Nothing about the service changes; it is not
//! stopped or restarted.
//!
//! The objects are the local engines' (`.../dump.archive`,
//! `.../dump.rdb.gz`, `.../dump.sql.gz`, or a bucket mirror under
//! `.../external_services/s3/<service>/<uuid>/<bucket>/`), so the providers'
//! restores read them. A restore of one runs on the node too (see
//! `services::restore::run_agent_restore`), in place.
//!
//! PostgreSQL on a worker takes the job path instead (`workload_job`).

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, EntityTrait};
use serde_json::json;
use tracing::info;

use super::v2_common;
use super::workload_job::{is_loopback_endpoint, worker_node, DatabaseJobError};
use crate::execution::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};
use temps_core::ScopedS3Access;
use temps_providers::externalsvc::worker_data::DumpEngine;
use temps_providers::externalsvc::ServiceType;
use temps_providers::remote_service_client::WorkerS3Destination;
use temps_providers::worker_s3_scope::{
    key_directory, WorkerCredentialError, WorkerCredentialScoper, WorkerS3Grant,
};

/// Engine key stored in `backup_jobs.engine` for a backup a worker's agent
/// took.
pub const ENGINE_KEY: &str = "worker_agent";

/// Whether a backup (and an in-place restore) of `service` runs on its
/// worker node through the agent: a standalone MongoDB, Redis, MariaDB or
/// storage service whose node is a worker.
pub fn takes_agent_path(service: &temps_entities::external_services::Model) -> bool {
    worker_node(service).is_some()
        && service.topology.as_str() != "cluster"
        && worker_service_kind(&service.service_type).is_some()
}

/// What a worker backup of `service_type` writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerServiceKind {
    /// One dump object.
    Dump(DumpEngine),
    /// A mirror of every bucket under one prefix.
    Storage,
}

/// The kind of worker backup `service_type` takes, `None` for PostgreSQL
/// (the job path) and unknown types.
pub fn worker_service_kind(service_type: &str) -> Option<WorkerServiceKind> {
    let service_type = match service_type {
        "mongo" => ServiceType::Mongodb,
        other => ServiceType::from_str(other).ok()?,
    };
    #[allow(deprecated, reason = "MinIO is matched so its refusal names it")]
    match service_type {
        ServiceType::Postgres => None,
        ServiceType::S3 | ServiceType::Blob | ServiceType::Rustfs | ServiceType::Minio => {
            Some(WorkerServiceKind::Storage)
        }
        other => DumpEngine::for_service_type(other).map(WorkerServiceKind::Dump),
    }
}

/// Where a worker backup of `service` is written: a dump's object key, or the
/// prefix a storage service's buckets are mirrored under (the `s3_mirror`
/// engine's layout).
pub fn backup_key(
    bucket_path: &str,
    service: &temps_entities::external_services::Model,
    backup_uuid: &str,
    kind: WorkerServiceKind,
) -> String {
    match kind {
        WorkerServiceKind::Dump(engine) => v2_common::build_external_service_s3_key(
            bucket_path,
            dump_engine_label(engine),
            &service.name,
            backup_uuid,
            engine.file_suffix(),
        ),
        WorkerServiceKind::Storage => {
            super::s3_mirror::build_dest_prefix(bucket_path, &service.name, backup_uuid)
        }
    }
}

/// The path segment the local engine of `engine` writes its dumps under.
fn dump_engine_label(engine: DumpEngine) -> &'static str {
    match engine {
        DumpEngine::Mongodb => "mongodb",
        DumpEngine::Redis => "redis",
        DumpEngine::Mariadb => "mariadb",
    }
}

/// The prefix a worker backup at `key` may write: a dump's own directory
/// (`.../<service>/<date>/<uuid>/`), or a mirror's own prefix.
pub fn backup_scope_prefix(key: &str, kind: WorkerServiceKind) -> String {
    match kind {
        WorkerServiceKind::Dump(_) => key_directory(key),
        WorkerServiceKind::Storage => temps_providers::worker_s3_scope::as_prefix(key),
    }
}

/// The S3 destination a worker job receives (ADR-093): a credential the
/// control plane derives for `grant` (the job's prefixes, read or write, a
/// lifetime), never the source's own unless the source allows it and cannot
/// scope. Refuses an endpoint on this host's loopback, which the node cannot
/// reach.
pub async fn scoped_destination(
    scoper: &WorkerCredentialScoper,
    db: &DatabaseConnection,
    s3_source_id: i32,
    grant: WorkerS3Grant,
    operation: &'static str,
) -> Result<WorkerS3Destination, BackupError> {
    let source = v2_common::load_s3_source(db, s3_source_id).await?;
    if let Some(endpoint) = source.endpoint.as_ref().filter(|e| is_loopback_endpoint(e)) {
        return Err(DatabaseJobError::EndpointUnreachable {
            service_id: grant.service_id,
            operation,
            node_id: grant.node_id,
            s3_source_id,
            endpoint: endpoint.clone(),
        }
        .into());
    }
    let node_id = grant.node_id;
    let scoped = scoper
        .destination(&source, grant)
        .await
        .map_err(credential_error)?;
    if let Some(warning) = scoped.warning(&source.name, node_id) {
        tracing::warn!(s3_source_id, node_id, "{warning}");
    }
    Ok(scoped.destination)
}

/// A credential that could not be derived, as the job's failure: retried
/// only when the STS or Cloud may answer next time.
pub fn credential_error(error: WorkerCredentialError) -> BackupError {
    if error.is_retryable() {
        BackupError::Failed {
            reason: error.to_string(),
        }
    } else {
        BackupError::PermanentFailure {
            reason: error.to_string(),
        }
    }
}

pub struct WorkerAgentDeps {
    pub db: Arc<DatabaseConnection>,
    pub encryption_service: Arc<temps_core::EncryptionService>,
    /// Reaches the service's node through its agent.
    pub manager: Arc<temps_providers::ExternalServiceManager>,
    /// Derives the job's S3 credential (ADR-093).
    pub worker_credentials: Arc<WorkerCredentialScoper>,
}

pub struct WorkerAgentEngine {
    deps: Arc<WorkerAgentDeps>,
}

impl WorkerAgentEngine {
    pub fn new(deps: WorkerAgentDeps) -> Self {
        Self {
            deps: Arc::new(deps),
        }
    }
}

/// The wait hook of a backup whose worker node is busy: logged with the
/// backup's IDs (a backup row has no phase), then a sleep the executor's
/// cancel and runtime limit cut short by dropping it.
pub(crate) async fn wait_for_node_slot(
    backup_id: i32,
    service_id: i32,
    wait: crate::node_busy::NodeWait,
) -> Result<(), std::convert::Infallible> {
    tracing::warn!(
        backup_id,
        service_id,
        "backup {backup_id} of service {service_id} waits for a slot: {}",
        wait.describe()
    );
    tokio::time::sleep(wait.delay).await;
    Ok(())
}

#[async_trait]
impl BackupEngine for WorkerAgentEngine {
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
                reason: format!(
                    "db error loading service {service_id} for backup {backup_id}: {e}"
                ),
            })?
            .ok_or_else(|| BackupError::PermanentFailure {
                reason: format!("service {service_id} of backup {backup_id} not found"),
            })?;
        // The service may have moved since the backup was enqueued.
        let (Some(node_id), Some(kind)) = (
            worker_node(&service),
            worker_service_kind(&service.service_type),
        ) else {
            return Err(BackupError::PermanentFailure {
                reason: format!(
                    "backup {backup_id}: service {service_id} ('{}', {}) is not a service on a \
                     worker node that its agent backs up; the next backup uses the engine for \
                     where it is now",
                    service.name, service.service_type
                ),
            });
        };
        #[allow(deprecated, reason = "the legacy type is refused by name")]
        if ServiceType::from_str(&service.service_type).ok() == Some(ServiceType::Minio) {
            return Err(BackupError::PermanentFailure {
                reason: format!(
                    "backup {backup_id}: service {service_id} ('{}') uses the legacy MinIO \
                     backend, which has no bucket mirror on worker nodes; migrate it to the \
                     RustFS backend",
                    service.name
                ),
            });
        }

        let (s3_source, s3_client) = v2_common::load_and_build_s3_client(
            deps.db.as_ref(),
            &deps.encryption_service,
            s3_source_id,
            "worker-agent-engine",
        )
        .await?;
        v2_common::assert_bucket_reachable(&s3_client, &s3_source.bucket_name).await?;
        let backup_uuid = v2_common::load_backup_uuid(deps.db.as_ref(), backup_id).await?;
        let key = backup_key(&s3_source.bucket_path, &service, &backup_uuid, kind);
        // The worker can write this backup's own directory and nothing else.
        let destination = scoped_destination(
            &deps.worker_credentials,
            deps.db.as_ref(),
            s3_source_id,
            WorkerS3Grant::job(
                service_id,
                node_id,
                backup_scope_prefix(&key, kind),
                ScopedS3Access::Write,
                "backup",
            ),
            "backup",
        )
        .await?;
        info!(
            backup_id,
            service_id,
            node_id,
            key = %key,
            bucket = %s3_source.bucket_name,
            "WorkerAgentEngine: backing up the service on its node"
        );

        // A node already running its maximum of backups and restores is
        // waited for (bounded, within this backup's own runtime limit)
        // instead of failing the backup.
        let request = crate::node_busy::retry_while_node_busy(
            &crate::node_busy::NodeBusyPolicy::DEFAULT,
            || {
                deps.manager
                    .backup_worker_service(service_id, destination.clone(), key.clone())
            },
            |wait| wait_for_node_slot(backup_id, service_id, wait),
        );
        let response = tokio::select! {
            // Dropping the request ends it on the node too: the agent removes
            // its helper and aborts the upload.
            _ = ctx.cancel.cancelled() => return Err(BackupError::Cancelled),
            response = request => match response {
                Ok(response) => response,
                Err(never) => match never {},
            },
        }
        .map_err(|e| BackupError::Failed {
            reason: format!(
                "backup {backup_id} of service {service_id} ('{}') on node {node_id}: {e}",
                service.name
            ),
        })?;

        let (size, compression, metadata_key, extra) = match kind {
            WorkerServiceKind::Dump(engine) => (
                i64::try_from(response.size_bytes).unwrap_or(i64::MAX),
                "gzip",
                v2_common::derive_metadata_key(&key),
                json!({
                    "backup_tool": format!("{engine:?}").to_lowercase(),
                    "service": { "id": service_id, "name": service.name },
                    "node_id": node_id,
                }),
            ),
            WorkerServiceKind::Storage => {
                let size = super::s3_mirror::list_total_s3_size(
                    &s3_client,
                    &s3_source.bucket_name,
                    &format!("{}/", key.trim_matches('/')),
                )
                .await
                .unwrap_or_else(|error| {
                    tracing::warn!(backup_id, %error, "worker_agent: could not size the mirror");
                    0
                });
                (
                    size,
                    "none",
                    format!("{}/metadata.json", key.trim_matches('/')),
                    json!({
                        "backup_tool": "rc",
                        "service": { "id": service_id, "name": service.name },
                        "node_id": node_id,
                        "buckets": response.buckets,
                    }),
                )
            }
        };
        v2_common::write_metadata_companion(
            &s3_client,
            &s3_source.bucket_name,
            &metadata_key,
            ENGINE_KEY,
            &backup_uuid,
            &key,
            size,
            s3_source_id,
            compression,
            Some(extra),
        )
        .await?;
        info!(
            backup_id,
            service_id,
            node_id,
            key = %key,
            size_bytes = size,
            "WorkerAgentEngine: backup complete"
        );
        Ok(BackupOutcome {
            location: key,
            size_bytes: Some(size),
            compression: compression.to_string(),
        })
    }
}

/// Whether a backup at `backup_location` is one a worker restore of
/// `target` applies: a dump in the target engine's format, or a bucket
/// mirror (a prefix, not a file) for storage.
pub fn restore_location_matches(
    target: &temps_entities::external_services::Model,
    backup_location: &str,
) -> bool {
    match worker_service_kind(&target.service_type) {
        Some(WorkerServiceKind::Dump(engine)) => backup_location.ends_with(engine.file_suffix()),
        Some(WorkerServiceKind::Storage) => {
            let last = backup_location
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or_default();
            !backup_location.starts_with("s3://") && !last.contains('.')
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(
        service_type: &str,
        node_id: Option<i32>,
    ) -> temps_entities::external_services::Model {
        temps_entities::external_services::Model {
            id: 7,
            name: "orders".into(),
            service_type: service_type.into(),
            version: None,
            status: "running".into(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            slug: None,
            config: None,
            node_id,
            topology: "standalone".into(),
            error_message: None,
            health_status: None,
            last_health_check_at: None,
            last_health_error: None,
            consecutive_health_failures: 0,
            health_metadata: None,
            metrics_enabled: false,
            default_backup_provisioned: false,
            container_name: None,
            created_by_user_id: None,
            continuous_archive_s3_source_id: None,
            continuous_archive_pinned_at: None,
            workload_mode:
                temps_entities::external_service_workload_mode::ServiceWorkloadMode::Legacy,
            log_ingest_lines_per_sec: None,
            log_ingest_kib_per_sec: None,
        }
    }

    #[test]
    fn every_non_postgres_engine_on_a_worker_takes_the_agent_path() {
        for service_type in [
            "mongodb", "mongo", "redis", "kv", "mariadb", "s3", "rustfs", "blob", "minio",
        ] {
            assert!(
                takes_agent_path(&service(service_type, Some(3))),
                "{service_type} on a worker"
            );
            assert!(
                !takes_agent_path(&service(service_type, None)),
                "{service_type} on the control plane keeps its local engine"
            );
            assert!(
                !takes_agent_path(&service(service_type, Some(0))),
                "node 0 is the control plane"
            );
        }
        assert!(
            !takes_agent_path(&service("postgres", Some(3))),
            "the job path"
        );
        let mut cluster = service("redis", Some(3));
        cluster.topology = "cluster".to_string();
        assert!(!takes_agent_path(&cluster));
    }

    #[test]
    fn backups_use_the_local_engines_keys() {
        let mongo = backup_key(
            "tenant",
            &service("mongodb", Some(3)),
            "u1",
            WorkerServiceKind::Dump(DumpEngine::Mongodb),
        );
        assert!(
            mongo.starts_with("tenant/external_services/mongodb/orders/"),
            "{mongo}"
        );
        assert!(mongo.ends_with("/u1/dump.archive"), "{mongo}");
        let redis = backup_key(
            "",
            &service("redis", Some(3)),
            "u1",
            WorkerServiceKind::Dump(DumpEngine::Redis),
        );
        assert!(
            redis.starts_with("external_services/redis/orders/"),
            "{redis}"
        );
        assert!(redis.ends_with("/u1/dump.rdb.gz"));
        let maria = backup_key(
            "",
            &service("mariadb", Some(3)),
            "u1",
            WorkerServiceKind::Dump(DumpEngine::Mariadb),
        );
        assert!(maria.ends_with("/u1/dump.sql.gz"));
        assert_eq!(
            backup_key(
                "tenant/",
                &service("s3", Some(3)),
                "u1",
                WorkerServiceKind::Storage
            ),
            "tenant/external_services/s3/orders/u1"
        );
    }

    #[test]
    fn restores_apply_only_their_own_backup_format() {
        let redis = service("redis", Some(3));
        assert!(restore_location_matches(
            &redis,
            "external_services/redis/orders/2026/01/01/u/dump.rdb.gz"
        ));
        assert!(!restore_location_matches(
            &redis,
            "external_services/mongodb/orders/2026/01/01/u/dump.archive"
        ));
        assert!(!restore_location_matches(
            &redis,
            "s3://backups/redis/base_000000010000000000000002"
        ));
        let mongo = service("mongodb", Some(3));
        assert!(restore_location_matches(&mongo, "x/dump.archive"));
        let maria = service("mariadb", Some(3));
        assert!(restore_location_matches(&maria, "x/dump.sql.gz"));
        assert!(!restore_location_matches(&maria, "x/base.xbstream"));
        let storage = service("rustfs", Some(3));
        assert!(restore_location_matches(
            &storage,
            "external_services/s3/orders/u1"
        ));
        assert!(!restore_location_matches(
            &storage,
            "external_services/s3/orders/u1/metadata.json"
        ));
        assert!(!restore_location_matches(
            &service("postgres", Some(3)),
            "x/dump.sql.gz"
        ));
    }

    #[test]
    fn a_worker_backup_is_scoped_to_its_own_directory() {
        use temps_providers::externalsvc::worker_data::DumpEngine;
        assert_eq!(
            backup_scope_prefix(
                "base/external_services/redis/cache/2026/11/08/u-1/dump.rdb.gz",
                WorkerServiceKind::Dump(DumpEngine::Redis)
            ),
            "base/external_services/redis/cache/2026/11/08/u-1/"
        );
        assert_eq!(
            backup_scope_prefix(
                "base/external_services/s3/files/u-2",
                WorkerServiceKind::Storage
            ),
            "base/external_services/s3/files/u-2/"
        );
    }
}
