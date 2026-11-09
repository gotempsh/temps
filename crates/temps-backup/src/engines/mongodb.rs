// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `MongodbEngine`: backups of a managed MongoDB, a direct-to-S3 WAL-G stream
//! when its image has WAL-G, else one `mongodump` archive.
//!
//! ## Flow
//!
//! 1. Load the external-service row and the S3 source; check the bucket.
//! 2. The node half (`temps_providers::externalsvc::mongodb_backup`): read
//!    the root credentials from the container, check its image for WAL-G,
//!    then either `wal-g backup-push` in the container (WAL-G streams
//!    `mongodump --archive` into the service's repository) or a pinned
//!    `mongodump --archive --gzip` helper streamed into one S3 object.
//! 3. Size the result and write its `metadata.json` companion; record the
//!    WAL-G identity of a stream backup.
//!
//! The node half is one function. For a service on this host it runs here
//! with this host's Docker; for a service on a worker node, that node's
//! agent runs it with the node's Docker (ADR-099). So the engine choice, the
//! commands, the layout and every artifact are the same for both.
//!
//! A worker never receives the S3 source's own credential (ADR-093). The
//! control plane asks the node whether the image has WAL-G (the check the
//! node half makes locally), then picks the backup itself: a WAL-G stream
//! into the service's repository, or an archive at a key it chooses, date
//! included. The credential it sends is scoped to that one repository or
//! that one archive's directory, and the node takes exactly that backup.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, EntityTrait};
use serde_json::{json, Value};
use temps_providers::externalsvc::mongodb_backup::{
    self as node_half, MongodbBackup, MongodbBackupError, MongodbBackupOutcome,
};
use temps_providers::remote_service_client::{MongodbBackupMode, MongodbBackupResult};
use temps_providers::worker_s3_scope::{key_directory, WorkerCredentialScoper, WorkerS3Grant};
use tracing::{info, warn};

use super::dispatch::service_container_name;
use super::v2_common;
use crate::execution::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};

pub(crate) const ENGINE_KEY: &str = "mongodb";

pub struct MongodbDeps {
    pub db: Arc<DatabaseConnection>,
    pub encryption_service: Arc<temps_core::EncryptionService>,
    /// This host's Docker, for services on this host. `None` on a server
    /// without one (`--profile control-plane`), which backs up only
    /// services on worker nodes.
    pub docker: Option<bollard::Docker>,
    /// Reaches a worker service's node through its agent.
    pub manager: Arc<temps_providers::ExternalServiceManager>,
    /// Derives a worker backup's S3 credential (ADR-093).
    pub worker_credentials: Arc<WorkerCredentialScoper>,
}

pub struct MongodbEngine {
    deps: Arc<MongodbDeps>,
}

impl MongodbEngine {
    pub fn new(deps: MongodbDeps) -> Self {
        Self {
            deps: Arc::new(deps),
        }
    }
}

#[async_trait]
impl BackupEngine for MongodbEngine {
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
            "mongodb-engine",
        )
        .await?;
        v2_common::assert_bucket_reachable(&s3_client, &s3_source.bucket_name).await?;
        let backup_uuid = v2_common::load_backup_uuid(deps.db.as_ref(), backup_id).await?;

        let outcome = match super::workload_job::worker_node(&service) {
            Some(node_id) => {
                backup_on_worker(
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
            None => backup_on_this_host(&deps, ctx, &service, &s3_source, &backup_uuid).await?,
        };
        if ctx.cancel.is_cancelled() {
            return Err(BackupError::Cancelled);
        }
        match outcome {
            MongodbBackupOutcome::WalgStream { .. } => {
                record_walg_stream(&deps, ctx, &service, &s3_source, &s3_client, &backup_uuid).await
            }
            MongodbBackupOutcome::Archive { key, size_bytes } => {
                record_archive(
                    &service,
                    &s3_source,
                    &s3_client,
                    &backup_uuid,
                    backup_id,
                    key,
                    size_bytes,
                )
                .await
            }
        }
    }
}

/// The node half, here, against this host's Docker.
async fn backup_on_this_host(
    deps: &MongodbDeps,
    ctx: &BackupContext,
    service: &temps_entities::external_services::Model,
    s3_source: &temps_entities::s3_sources::Model,
    backup_uuid: &str,
) -> Result<MongodbBackupOutcome, BackupError> {
    let Some(docker) = deps.docker.clone() else {
        return Err(BackupError::PermanentFailure {
            reason: format!(
                "Service {} ('{}') runs on this server, which has no Docker daemon to run its \
                 MongoDB backup in",
                service.id, service.name
            ),
        });
    };
    let decrypt = |field: &str, value: &str| {
        deps.encryption_service
            .decrypt_string(value)
            .map_err(|e| BackupError::PermanentFailure {
                reason: format!("decrypt {field} of S3 source {}: {e}", s3_source.id),
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
    let config_json = deps
        .encryption_service
        .decrypt_string(service.config.as_deref().unwrap_or("{}"))
        .unwrap_or_else(|_| "{}".to_string());
    let service_config = temps_providers::externalsvc::ServiceConfig {
        name: service.name.clone(),
        service_type: temps_providers::externalsvc::ServiceType::Mongodb,
        version: None,
        parameters: serde_json::from_str(&config_json).unwrap_or(Value::Null),
    };
    node_half::mongodb_backup(
        Arc::new(docker),
        MongodbBackup {
            service_id: service.id,
            container_name: service_container_name(service),
            service_config,
            s3,
            backup_uuid: backup_uuid.to_string(),
            mode: None,
        },
        &ctx.cancel,
    )
    .await
    .map_err(backup_error)
}

/// The node half, on the worker that runs the service, by its agent.
async fn backup_on_worker(
    deps: &MongodbDeps,
    ctx: &BackupContext,
    service: &temps_entities::external_services::Model,
    node_id: i32,
    s3_source_id: i32,
    bucket_path: &str,
    backup_uuid: &str,
) -> Result<MongodbBackupOutcome, BackupError> {
    // The plane picks the backup (ADR-099), so the credential covers it and
    // nothing else: the check is the node half's own `which wal-g`.
    let has_walg = super::dispatch::WorkerToolProbe::worker_has_walg(
        deps.manager.as_ref(),
        node_id,
        &service_container_name(service),
    )
    .await;
    let (mode, prefix) = worker_backup_plan(
        has_walg,
        bucket_path,
        &service.name,
        backup_uuid,
        chrono::Utc::now(),
    );
    info!(
        backup_id = ctx.backup_id,
        service_id = service.id,
        node_id,
        mode = ?mode,
        prefix = %prefix,
        "MongoDB backup on a worker node: credential scoped to one backup location"
    );
    // Refuses an endpoint on this host's loopback, which the node cannot
    // reach.
    let destination = super::worker_agent::scoped_destination(
        &deps.worker_credentials,
        deps.db.as_ref(),
        s3_source_id,
        WorkerS3Grant::job(
            service.id,
            node_id,
            prefix,
            temps_core::ScopedS3Access::Write,
            "backup",
        ),
        "backup",
    )
    .await?;
    // A node already running its maximum of backups and restores is waited
    // for (bounded, within this backup's own runtime limit) instead of
    // failing the backup.
    let request = crate::node_busy::retry_while_node_busy(
        &crate::node_busy::NodeBusyPolicy::DEFAULT,
        || {
            deps.manager.mongodb_backup_worker(
                service.id,
                destination.clone(),
                bucket_path.to_string(),
                backup_uuid.to_string(),
                mode.clone(),
            )
        },
        |wait| super::worker_agent::wait_for_node_slot(ctx.backup_id, service.id, wait),
    );
    let result = tokio::select! {
        // Dropping the request ends it on the node too.
        _ = ctx.cancel.cancelled() => return Err(BackupError::Cancelled),
        result = request => match result {
            Ok(result) => result,
            Err(never) => match never {},
        },
    }
    .map_err(|e| BackupError::Failed {
        reason: format!(
            "MongoDB backup of service {} ('{}') on node {node_id}: {e}",
            service.id, service.name
        ),
    })?;
    worker_outcome(service, node_id, &mode, result)
}

/// The backup a worker takes and the key prefix its credential is scoped
/// to: the service's WAL-G repository when the image has WAL-G, else one
/// archive at the key of `now`, whose directory is that backup's alone.
pub(crate) fn worker_backup_plan(
    has_walg: bool,
    bucket_path: &str,
    service_name: &str,
    backup_uuid: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> (MongodbBackupMode, String) {
    if has_walg {
        (
            MongodbBackupMode::WalgStream,
            node_half::walg_key_prefix(bucket_path, service_name),
        )
    } else {
        let key = node_half::archive_key(bucket_path, service_name, backup_uuid, now);
        let prefix = key_directory(&key);
        (MongodbBackupMode::Archive { key }, prefix)
    }
}

/// A worker's answer, as the outcome or error of the same run here. Only
/// the backup the plane granted is accepted: a stream for a stream, the
/// chosen key for an archive.
fn worker_outcome(
    service: &temps_entities::external_services::Model,
    node_id: i32,
    granted: &MongodbBackupMode,
    result: MongodbBackupResult,
) -> Result<MongodbBackupOutcome, BackupError> {
    let on_node = |reason: String| {
        format!(
            "MongoDB backup of service {} ('{}') on node {node_id}: {reason}",
            service.id, service.name
        )
    };
    match result {
        MongodbBackupResult::WalgStream { stdout, stderr } => match granted {
            MongodbBackupMode::WalgStream => Ok(MongodbBackupOutcome::WalgStream { stdout, stderr }),
            MongodbBackupMode::Archive { key } => Err(BackupError::PermanentFailure {
                reason: on_node(format!(
                    "the node reported a WAL-G stream backup, but it was granted an archive at                      '{key}'"
                )),
            }),
        },
        MongodbBackupResult::Archive { key, size_bytes } => match granted {
            MongodbBackupMode::Archive { key: granted_key } if *granted_key == key => {
                Ok(MongodbBackupOutcome::Archive { key, size_bytes })
            }
            MongodbBackupMode::Archive { key: granted_key } => Err(BackupError::PermanentFailure {
                reason: on_node(format!(
                    "the node reported archive '{key}', which is not the key it was granted                      ('{granted_key}')"
                )),
            }),
            MongodbBackupMode::WalgStream => Err(BackupError::PermanentFailure {
                reason: on_node(format!(
                    "the node reported archive '{key}', but it was granted a WAL-G stream backup"
                )),
            }),
        },
        MongodbBackupResult::Refused { reason } => Err(BackupError::PermanentFailure {
            reason: on_node(reason),
        }),
        MongodbBackupResult::Failed { reason } => Err(BackupError::Failed {
            reason: on_node(reason),
        }),
    }
}

/// The node half's error as the executor's: a refusal is final, a
/// cancellation is a cancellation, the rest may succeed on a retry.
fn backup_error(error: MongodbBackupError) -> BackupError {
    match error {
        MongodbBackupError::Cancelled { .. } => BackupError::Cancelled,
        error if error.is_refusal() => BackupError::PermanentFailure {
            reason: error.to_string(),
        },
        error => BackupError::Failed {
            reason: error.to_string(),
        },
    }
}

/// Size, metadata and identity of a WAL-G stream backup.
async fn record_walg_stream(
    deps: &MongodbDeps,
    ctx: &BackupContext,
    service: &temps_entities::external_services::Model,
    s3_source: &temps_entities::s3_sources::Model,
    s3_client: &aws_sdk_s3::Client,
    backup_uuid: &str,
) -> Result<BackupOutcome, BackupError> {
    let walg_prefix = node_half::walg_prefix(
        &s3_source.bucket_name,
        &s3_source.bucket_path,
        &service.name,
    );
    let list_prefix = node_half::walg_key_prefix(&s3_source.bucket_path, &service.name);
    let file_size = list_total_s3_size(s3_client, &s3_source.bucket_name, &list_prefix).await?;
    if file_size <= 0 {
        return Err(BackupError::Failed {
            reason: format!("MongoDB WAL-G repository {walg_prefix} contains no backup bytes"),
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
            "backup_tool": "wal-g+mongodump-stream",
            "service": { "id": service.id, "name": service.name },
        })),
    )
    .await?;
    v2_common::record_walg_identity(deps.db.as_ref(), ctx.backup_id, backup_uuid).await?;
    info!(
        backup_id = ctx.backup_id,
        repository = %walg_prefix,
        size_bytes = file_size,
        "MongodbEngine: WAL-G stream backup complete",
    );
    Ok(BackupOutcome {
        location: walg_prefix,
        size_bytes: Some(file_size),
        compression: "wal-g-native".to_string(),
    })
}

/// Metadata of an archive backup.
async fn record_archive(
    service: &temps_entities::external_services::Model,
    s3_source: &temps_entities::s3_sources::Model,
    s3_client: &aws_sdk_s3::Client,
    backup_uuid: &str,
    backup_id: i32,
    key: String,
    size_bytes: u64,
) -> Result<BackupOutcome, BackupError> {
    let file_size = i64::try_from(size_bytes).map_err(|_| BackupError::Failed {
        reason: format!("mongodump archive for backup {backup_id} is larger than i64::MAX bytes"),
    })?;
    v2_common::write_metadata_companion(
        s3_client,
        &s3_source.bucket_name,
        &v2_common::derive_metadata_key(&key),
        ENGINE_KEY,
        backup_uuid,
        &key,
        file_size,
        s3_source.id,
        "gzip",
        Some(json!({
            "service": { "id": service.id, "name": service.name },
        })),
    )
    .await?;
    warn!(
        backup_id,
        key = %key,
        "MongodbEngine: the image has no WAL-G; took a mongodump archive. It is restorable but \
         not eligible for managed Cloud restore verification",
    );
    info!(
        backup_id,
        bucket = %s3_source.bucket_name,
        key = %key,
        size_bytes = file_size,
        "MongodbEngine: archive backup complete",
    );
    Ok(BackupOutcome {
        location: key,
        size_bytes: Some(file_size),
        compression: "gzip".to_string(),
    })
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
            reason: format!("list MongoDB WAL-G repository {prefix}: {error}"),
        })?;
        for object in response.contents() {
            total = total
                .checked_add(object.size().unwrap_or(0))
                .ok_or_else(|| BackupError::Failed {
                    reason: format!("MongoDB WAL-G repository {prefix} size overflowed i64"),
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

    fn service() -> temps_entities::external_services::Model {
        temps_entities::external_services::Model {
            id: 7,
            name: "orders".into(),
            service_type: "mongodb".into(),
            version: None,
            status: "running".into(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            slug: None,
            config: None,
            node_id: Some(3),
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
            workload_mode: Default::default(),
            log_ingest_lines_per_sec: None,
            log_ingest_kib_per_sec: None,
        }
    }

    /// The layout is the control plane's: a WAL-G repository under the
    /// service, or the archive key the local engines always wrote.
    #[test]
    fn the_archive_layout_is_the_local_engines() {
        let key = node_half::archive_key("tenant", "orders", "u-1", chrono::Utc::now());
        let expected = v2_common::build_external_service_s3_key(
            "tenant",
            ENGINE_KEY,
            "orders",
            "u-1",
            node_half::ARCHIVE_FILE_NAME,
        );
        assert_eq!(key, expected);
        assert_eq!(
            v2_common::derive_metadata_key(&key),
            key.replace("dump.archive", "metadata.json")
        );
    }

    /// The plane scopes a worker's credential to one backup location: the
    /// service's WAL-G repository for a stream, one archive's own directory
    /// (date and backup included) for an archive. Never the service's whole
    /// tree, never the source's path.
    #[test]
    fn a_worker_backup_is_granted_one_location() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-09T10:00:00Z")
            .expect("a timestamp")
            .with_timezone(&chrono::Utc);
        let (mode, prefix) = worker_backup_plan(true, "tenant", "orders", "u-1", now);
        assert_eq!(mode, MongodbBackupMode::WalgStream);
        assert_eq!(prefix, "tenant/external_services/mongodb/orders/walg/");

        let (mode, prefix) = worker_backup_plan(false, "tenant", "orders", "u-1", now);
        assert_eq!(
            mode,
            MongodbBackupMode::Archive {
                key: "tenant/external_services/mongodb/orders/2026/11/09/u-1/dump.archive".into()
            }
        );
        assert_eq!(
            prefix,
            "tenant/external_services/mongodb/orders/2026/11/09/u-1/"
        );
        // No source path: still inside the service's own tree.
        let (_, prefix) = worker_backup_plan(false, "", "orders", "u-1", now);
        assert_eq!(prefix, "external_services/mongodb/orders/2026/11/09/u-1/");
    }

    /// A worker's outcomes are this engine's, and only the backup the node was
    /// granted is accepted: a stream for a stream, the very key for an
    /// archive. A refusal is final, a failure retryable.
    #[test]
    fn worker_outcomes_map_to_the_engines() {
        let service = service();
        let key = node_half::archive_key("base", "orders", "u-1", chrono::Utc::now());
        let archive = MongodbBackupMode::Archive { key: key.clone() };
        let stream = MongodbBackupMode::WalgStream;
        assert_eq!(
            worker_outcome(
                &service,
                3,
                &archive,
                MongodbBackupResult::Archive {
                    key: key.clone(),
                    size_bytes: 9
                }
            )
            .ok(),
            Some(MongodbBackupOutcome::Archive {
                key: key.clone(),
                size_bytes: 9
            })
        );
        assert!(matches!(
            worker_outcome(
                &service,
                3,
                &archive,
                MongodbBackupResult::Archive {
                    key: "elsewhere/dump.archive".into(),
                    size_bytes: 9
                }
            ),
            Err(BackupError::PermanentFailure { reason }) if reason.contains("elsewhere")
        ));
        assert!(matches!(
            worker_outcome(
                &service,
                3,
                &stream,
                MongodbBackupResult::Archive { key, size_bytes: 9 }
            ),
            Err(BackupError::PermanentFailure { reason }) if reason.contains("WAL-G stream")
        ));
        assert!(matches!(
            worker_outcome(
                &service,
                3,
                &archive,
                MongodbBackupResult::WalgStream {
                    stdout: String::new(),
                    stderr: String::new()
                }
            ),
            Err(BackupError::PermanentFailure { reason }) if reason.contains("granted an archive")
        ));
        assert!(matches!(
            worker_outcome(
                &service,
                3,
                &stream,
                MongodbBackupResult::Refused {
                    reason: "no".into()
                }
            ),
            Err(BackupError::PermanentFailure { reason }) if reason.contains("node 3")
        ));
        assert!(matches!(
            worker_outcome(
                &service,
                3,
                &stream,
                MongodbBackupResult::Failed {
                    reason: "exit 1".into()
                }
            ),
            Err(BackupError::Failed { .. })
        ));
        assert!(matches!(
            worker_outcome(
                &service,
                3,
                &stream,
                MongodbBackupResult::WalgStream {
                    stdout: String::new(),
                    stderr: String::new()
                }
            ),
            Ok(MongodbBackupOutcome::WalgStream { .. })
        ));
    }

    #[test]
    fn node_half_errors_keep_their_retry_class() {
        assert!(matches!(
            backup_error(MongodbBackupError::Cancelled {
                container: "c".into()
            }),
            BackupError::Cancelled
        ));
        assert!(matches!(
            backup_error(MongodbBackupError::Target {
                service_id: 1,
                service: "s".into(),
                reason: "r".into()
            }),
            BackupError::PermanentFailure { .. }
        ));
        assert!(matches!(
            backup_error(MongodbBackupError::BackupPush {
                service_id: 1,
                container: "c".into(),
                exit_code: 1,
                stderr: "e".into()
            }),
            BackupError::Failed { .. }
        ));
    }
}
