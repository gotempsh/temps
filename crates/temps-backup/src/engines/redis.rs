// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `RedisEngine`: direct-to-S3 WAL-G stream backups for managed Redis
//! images, with a `redis-cli --rdb` snapshot for images without WAL-G,
//! wherever the service runs (ADR-099).
//!
//! ## Flow
//!
//! 1. Load the external-service row and validate the S3 source.
//! 2. Check for WAL-G in the service's container: on this host's Docker, or,
//!    for a Redis on a worker, the same check through the node's agent.
//! 3. With WAL-G, `wal-g backup-push` runs in the container, streaming
//!    `redis-cli --rdb -` to S3 with no database-sized file. This is the
//!    Cloud-verifiable path. Then the repository's size is listed, the
//!    `metadata.json` companion written and the WAL-G identity recorded.
//! 4. Without WAL-G, a pinned `redis-cli` helper takes an RDB snapshot over
//!    the service network and its gzipped output is streamed into one
//!    object, a part at a time. Then the object is tagged and the companion
//!    written. Usable in OSS, not eligible for managed Cloud restore
//!    verification.
//!
//! The container steps (3's push, 4's helper) are the node half,
//! `temps_providers::externalsvc::redis_node`: run here with this host's
//! Docker, or on the worker by its agent. Everything else, the engine
//! choice included, is this plane half, the same for both. Redis is never
//! stopped by a backup.
//!
//! A worker never receives the S3 source's own credential (ADR-093): the
//! plane makes the engine choice, so it scopes the credential it sends to
//! that backup alone, the service's WAL-G repository for a stream, or the
//! one directory of the RDB snapshot's key, which the plane chose.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, EntityTrait};
use serde_json::{json, Value};
use temps_providers::externalsvc::redis_node::{
    self, RedisNodeError, RedisRdbBackup, RedisWalgBackup, RDB_DUMP_FILE,
};
use temps_providers::remote_service_client::{
    RedisRdbBackupResult, RedisWalgBackupResult, WorkerS3Destination,
};
use tracing::{info, warn};

use super::dispatch::{container_has_walg, service_container_name, WorkerToolProbe};
use super::v2_common;
use super::workload_job::worker_node;
use crate::execution::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};

pub(crate) const ENGINE_KEY: &str = "redis";

pub struct RedisDeps {
    pub db: Arc<DatabaseConnection>,
    pub encryption_service: Arc<temps_core::EncryptionService>,
    /// This host's Docker, for a Redis on this host. `None` on a server
    /// without one (`--profile control-plane`), which backs up only Redis
    /// services on worker nodes.
    pub docker: Option<bollard::Docker>,
    /// Reaches a worker Redis's node through its agent.
    pub manager: Arc<temps_providers::ExternalServiceManager>,
    /// Derives a worker backup's S3 credential (ADR-093).
    pub worker_credentials: Arc<temps_providers::worker_s3_scope::WorkerCredentialScoper>,
}

pub struct RedisEngine {
    deps: Arc<RedisDeps>,
}

impl RedisEngine {
    pub fn new(deps: RedisDeps) -> Self {
        Self {
            deps: Arc::new(deps),
        }
    }
}

/// Where the node half of a backup runs.
enum Node {
    /// This host, with its Docker.
    Here(bollard::Docker),
    /// Worker `node_id`, by its agent.
    Worker(i32),
}

#[async_trait]
impl BackupEngine for RedisEngine {
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
            "redis-engine",
        )
        .await?;
        v2_common::assert_bucket_reachable(&s3_client, &s3_source.bucket_name).await?;

        let backup_uuid = v2_common::load_backup_uuid(deps.db.as_ref(), backup_id).await?;
        let target_container = service_container_name(&service);
        let node = match worker_node(&service) {
            Some(node_id) => Node::Worker(node_id),
            None => match deps.docker.clone() {
                Some(docker) => Node::Here(docker),
                None => {
                    return Err(BackupError::PermanentFailure {
                        reason: format!(
                            "Redis service {} ('{}') runs on this server, which has no Docker \
                             daemon to back it up with",
                            service.id, service.name
                        ),
                    })
                }
            },
        };
        let has_walg = match &node {
            Node::Here(docker) => container_has_walg(docker, &target_container).await,
            Node::Worker(node_id) => {
                deps.manager
                    .worker_has_walg(*node_id, &target_container)
                    .await
            }
        };

        if has_walg {
            return run_walg_backup(
                &deps,
                ctx,
                &node,
                &service,
                &s3_source,
                &s3_client,
                &target_container,
                &backup_uuid,
            )
            .await;
        }
        warn!(
            backup_id,
            container = %target_container,
            "RedisEngine: WAL-G is unavailable; using logical redis-cli fallback. This backup remains usable in OSS but is not eligible for managed Cloud restore verification",
        );

        let s3_key = v2_common::build_external_service_s3_key(
            &s3_source.bucket_path,
            "redis",
            &service.name,
            &backup_uuid,
            RDB_DUMP_FILE,
        );
        let file_size = match &node {
            Node::Here(docker) => {
                let backup = RedisRdbBackup {
                    service_id: service.id,
                    service: service.name.clone(),
                    container_name: target_container.clone(),
                    password: service_password(&deps, &service),
                    destination: local_destination(&deps, &s3_source)?,
                    key: s3_key.clone(),
                };
                tokio::select! {
                    // Dropping the upload removes its helper container.
                    _ = ctx.cancel.cancelled() => return Err(BackupError::Cancelled),
                    result = redis_node::redis_rdb_backup(Arc::new(docker.clone()), backup) => {
                        result.map_err(node_error)?
                    }
                }
            }
            Node::Worker(node_id) => {
                // Write access to this snapshot's own directory, nothing else.
                let mint = || {
                    worker_destination(
                        &deps,
                        s3_source_id,
                        &service,
                        *node_id,
                        temps_providers::worker_s3_scope::key_directory(&s3_key),
                    )
                };
                let destination = mint().await?;
                let result = on_worker(
                    ctx,
                    &service,
                    *node_id,
                    "RDB backup",
                    destination,
                    mint,
                    |destination| {
                        deps.manager.rdb_backup_worker_redis(
                            service.id,
                            destination,
                            s3_key.clone(),
                        )
                    },
                )
                .await?;
                match result {
                    RedisRdbBackupResult::Completed { size_bytes } => size_bytes,
                    RedisRdbBackupResult::Refused { reason } => {
                        return Err(worker_refused(&service, *node_id, "RDB backup", &reason))
                    }
                    RedisRdbBackupResult::Failed { reason } => {
                        return Err(worker_failed(&service, *node_id, "RDB backup", &reason))
                    }
                }
            }
        };
        let file_size = i64::try_from(file_size).map_err(|_| BackupError::Failed {
            reason: format!("Redis dump for backup {backup_id} is larger than i64::MAX bytes"),
        })?;

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
                "backup_tool": "redis-cli-rdb",
                "service": { "id": service_id, "name": service.name },
            })),
        )
        .await?;

        info!(
            backup_id,
            bucket = %s3_source.bucket_name,
            key = %s3_key,
            size_bytes = file_size,
            "RedisEngine: backup complete",
        );

        Ok(BackupOutcome {
            location: s3_key,
            size_bytes: Some(file_size),
            compression: "gzip".to_string(),
        })
    }
}

/// The plane half of a WAL-G stream backup around its node half.
#[allow(
    clippy::too_many_arguments,
    reason = "the backup's inputs, loaded once by the caller"
)]
async fn run_walg_backup(
    deps: &RedisDeps,
    ctx: &BackupContext,
    node: &Node,
    service: &temps_entities::external_services::Model,
    s3_source: &temps_entities::s3_sources::Model,
    s3_client: &aws_sdk_s3::Client,
    container_name: &str,
    backup_uuid: &str,
) -> Result<BackupOutcome, BackupError> {
    let walg_prefix = redis_node::walg_prefix(
        &s3_source.bucket_name,
        &s3_source.bucket_path,
        &service.name,
    );
    let list_prefix = redis_node::walg_key_prefix(&s3_source.bucket_path, &service.name);

    info!(
        backup_id = ctx.backup_id,
        repository = %walg_prefix,
        "RedisEngine: starting direct WAL-G stream backup",
    );
    match node {
        Node::Here(docker) => {
            let backup = RedisWalgBackup {
                service_id: service.id,
                service: service.name.clone(),
                container_name: container_name.to_string(),
                password: service_password(deps, service),
                s3: local_credentials(deps, s3_source)?,
                backup_uuid: backup_uuid.to_string(),
            };
            redis_node::redis_walg_backup(docker, backup, &ctx.cancel)
                .await
                .map_err(node_error)?;
        }
        Node::Worker(node_id) => {
            // Write access to the service's own WAL-G repository, nothing
            // else: `backup-push` writes and reads its sentinels there.
            let mint =
                || worker_destination(deps, s3_source.id, service, *node_id, list_prefix.clone());
            let destination = mint().await?;
            let result = on_worker(
                ctx,
                service,
                *node_id,
                "WAL-G backup",
                destination,
                mint,
                |destination| {
                    deps.manager.walg_backup_worker_redis(
                        service.id,
                        destination,
                        s3_source.bucket_path.clone(),
                        backup_uuid.to_string(),
                    )
                },
            )
            .await?;
            match result {
                RedisWalgBackupResult::Completed => {}
                RedisWalgBackupResult::Refused { reason } => {
                    return Err(worker_refused(service, *node_id, "WAL-G backup", &reason))
                }
                RedisWalgBackupResult::Failed { reason } => {
                    return Err(worker_failed(service, *node_id, "WAL-G backup", &reason))
                }
            }
        }
    }

    let file_size = list_total_s3_size(s3_client, &s3_source.bucket_name, &list_prefix).await?;
    if file_size <= 0 {
        return Err(BackupError::Failed {
            reason: format!("Redis WAL-G repository {walg_prefix} contains no backup bytes"),
        });
    }
    let metadata_key = format!("{list_prefix}{backup_uuid}.metadata.json");
    v2_common::write_metadata_companion(
        s3_client,
        &s3_source.bucket_name,
        &metadata_key,
        ENGINE_KEY,
        backup_uuid,
        &walg_prefix,
        file_size,
        s3_source.id,
        "wal-g-native",
        Some(json!({
            "backup_tool": "wal-g+redis-rdb-stream",
            "service": { "id": service.id, "name": service.name },
        })),
    )
    .await?;
    v2_common::record_walg_identity(deps.db.as_ref(), ctx.backup_id, backup_uuid).await?;

    info!(
        backup_id = ctx.backup_id,
        repository = %walg_prefix,
        size_bytes = file_size,
        "RedisEngine: WAL-G stream backup complete",
    );
    Ok(BackupOutcome {
        location: walg_prefix,
        size_bytes: Some(file_size),
        compression: "wal-g-native".to_string(),
    })
}

/// The password of a Redis on this host, from its stored configuration
/// (empty for one without auth). A worker's travels sealed with its config.
fn service_password(
    deps: &RedisDeps,
    service: &temps_entities::external_services::Model,
) -> String {
    let config_json = deps
        .encryption_service
        .decrypt_string(service.config.as_deref().unwrap_or("{}"))
        .unwrap_or_else(|_| "{}".to_string());
    let cfg: Value = serde_json::from_str(&config_json).unwrap_or_else(|_| json!({}));
    cfg.get("password")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The decrypted credentials of `s3_source`, for a node half on this host.
fn local_credentials(
    deps: &RedisDeps,
    s3_source: &temps_entities::s3_sources::Model,
) -> Result<temps_providers::externalsvc::S3Credentials, BackupError> {
    let decrypt = |field: &str, value: &str| {
        deps.encryption_service
            .decrypt_string(value)
            .map_err(|e| BackupError::PermanentFailure {
                reason: format!("decrypt {field} of S3 source {}: {e}", s3_source.id),
            })
    };
    Ok(temps_providers::externalsvc::S3Credentials {
        access_key_id: decrypt("access key", &s3_source.access_key_id)?,
        secret_key: decrypt("secret key", &s3_source.secret_key)?,
        session_token: v2_common::decrypt_session_token(s3_source, &deps.encryption_service)?,
        region: s3_source.region.clone(),
        endpoint: s3_source.endpoint.clone(),
        bucket_name: s3_source.bucket_name.clone(),
        bucket_path: s3_source.bucket_path.clone(),
        force_path_style: s3_source.force_path_style.unwrap_or(true),
    })
}

/// [`local_credentials`] as the destination an RDB backup streams to.
fn local_destination(
    deps: &RedisDeps,
    s3_source: &temps_entities::s3_sources::Model,
) -> Result<WorkerS3Destination, BackupError> {
    let s3 = local_credentials(deps, s3_source)?;
    Ok(WorkerS3Destination {
        bucket: s3.bucket_name,
        region: s3.region,
        endpoint: s3.endpoint,
        force_path_style: s3.force_path_style,
        access_key_id: s3.access_key_id,
        secret_access_key: s3.secret_key,
        session_token: s3.session_token,
    })
}

/// The S3 source as worker `node_id` receives it for one backup: a
/// credential that can write `prefix` and nothing else (ADR-093). Refuses an
/// endpoint on this host's loopback, which the node cannot reach.
async fn worker_destination(
    deps: &RedisDeps,
    s3_source_id: i32,
    service: &temps_entities::external_services::Model,
    node_id: i32,
    prefix: String,
) -> Result<WorkerS3Destination, BackupError> {
    super::worker_agent::scoped_destination(
        &deps.worker_credentials,
        deps.db.as_ref(),
        s3_source_id,
        temps_providers::worker_s3_scope::WorkerS3Grant::job(
            service.id,
            node_id,
            prefix,
            temps_core::ScopedS3Access::Write,
            "backup",
        ),
        "backup",
    )
    .await
}

/// Ask worker `node_id`'s agent to run a node half: a node already running
/// its maximum of backups and restores is waited for (bounded) instead of
/// failing the backup, and a cancelled backup stops waiting. The first try
/// is sent `first`; a try after a wait, a credential `mint` derives after it.
async fn on_worker<C, T, Mint, MintFut, Call, CallFut>(
    ctx: &BackupContext,
    service: &temps_entities::external_services::Model,
    node_id: i32,
    operation: &str,
    first: C,
    mint: Mint,
    call: Call,
) -> Result<T, BackupError>
where
    Mint: FnMut() -> MintFut,
    MintFut: std::future::Future<Output = Result<C, BackupError>>,
    Call: FnMut(C) -> CallFut,
    CallFut: std::future::Future<Output = Result<T, temps_providers::ExternalServiceError>>,
{
    let request = crate::node_busy::retry_while_node_busy_with_fresh_credential(
        &crate::node_busy::NodeBusyPolicy::DEFAULT,
        first,
        mint,
        call,
        |wait| super::worker_agent::wait_for_node_slot_minting(ctx.backup_id, service.id, wait),
    );
    tokio::select! {
        _ = ctx.cancel.cancelled() => Err(BackupError::Cancelled),
        result = request => result?.map_err(|e| BackupError::Failed {
            reason: format!(
                "Redis {operation} of service {} ('{}') on node {node_id}: {e}",
                service.id, service.name
            ),
        }),
    }
}

fn worker_refused(
    service: &temps_entities::external_services::Model,
    node_id: i32,
    operation: &str,
    reason: &str,
) -> BackupError {
    BackupError::PermanentFailure {
        reason: format!(
            "Redis {operation} of service {} ('{}') on node {node_id}: {reason}",
            service.id, service.name
        ),
    }
}

fn worker_failed(
    service: &temps_entities::external_services::Model,
    node_id: i32,
    operation: &str,
    reason: &str,
) -> BackupError {
    BackupError::Failed {
        reason: format!(
            "Redis {operation} of service {} ('{}') on node {node_id}: {reason}",
            service.id, service.name
        ),
    }
}

/// The node half's error as the executor's: a refusal is final, a
/// cancellation is a cancellation, the rest may succeed on a retry.
fn node_error(error: RedisNodeError) -> BackupError {
    match error {
        RedisNodeError::Cancelled { .. } => BackupError::Cancelled,
        error if error.is_refusal() => BackupError::PermanentFailure {
            reason: error.to_string(),
        },
        error => BackupError::Failed {
            reason: error.to_string(),
        },
    }
}

async fn list_total_s3_size(
    client: &aws_sdk_s3::Client,
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
            reason: format!("list Redis WAL-G repository {prefix}: {error}"),
        })?;
        for object in response.contents() {
            total = total
                .checked_add(object.size().unwrap_or(0))
                .ok_or_else(|| BackupError::Failed {
                    reason: format!("Redis WAL-G repository {prefix} size overflowed i64"),
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

    fn service(node_id: Option<i32>) -> temps_entities::external_services::Model {
        let now = chrono::Utc::now();
        temps_entities::external_services::Model {
            id: 7,
            name: "cache".to_string(),
            service_type: "redis".to_string(),
            version: None,
            status: "running".to_string(),
            created_at: now,
            updated_at: now,
            slug: None,
            config: None,
            node_id,
            topology: "standalone".to_string(),
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

    /// Both planes write a Redis's backups where the other restores them
    /// from: the WAL-G repository and the RDB object keys are the ones the
    /// local engine always wrote.
    #[test]
    fn the_layout_is_the_local_engines() {
        assert_eq!(
            redis_node::walg_prefix("backups", "tenant/", "cache"),
            "s3://backups/tenant/external_services/redis/cache/walg"
        );
        let key = v2_common::build_external_service_s3_key(
            "tenant",
            "redis",
            "cache",
            "u-1",
            RDB_DUMP_FILE,
        );
        assert!(
            key.starts_with("tenant/external_services/redis/cache/"),
            "{key}"
        );
        assert!(key.ends_with("/u-1/dump.rdb.gz"), "{key}");
    }

    /// A worker's answer is the same error the run here would give: a
    /// refusal is final, a failure may be retried, both name the service and
    /// the node.
    #[test]
    fn a_worker_answer_is_the_error_of_the_same_run() {
        let service = service(Some(3));
        let refused = worker_refused(&service, 3, "WAL-G backup", "imported container");
        assert!(
            matches!(&refused, BackupError::PermanentFailure { reason }
                if reason.contains("service 7 ('cache') on node 3") && reason.contains("imported container")),
            "{refused:?}"
        );
        let failed = worker_failed(&service, 3, "RDB backup", "exit 1");
        assert!(
            matches!(&failed, BackupError::Failed { reason } if reason.contains("RDB backup")),
            "{failed:?}"
        );
    }

    #[test]
    fn only_a_refusal_of_the_node_half_is_final() {
        assert!(matches!(
            node_error(RedisNodeError::Cancelled {
                container: "redis-cache".to_string()
            }),
            BackupError::Cancelled
        ));
        assert!(matches!(
            node_error(RedisNodeError::Refused {
                service_id: 7,
                service: "cache".to_string(),
                operation: "backed up",
                reason: "not running".to_string(),
            }),
            BackupError::PermanentFailure { .. }
        ));
        assert!(matches!(
            node_error(RedisNodeError::BackupPush {
                service_id: 7,
                container: "redis-cache".to_string(),
                exit_code: 1,
                stderr: "boom".to_string(),
            }),
            BackupError::Failed { .. }
        ));
    }
}
