// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use aws_sdk_s3::{Client as S3Client, Config};
use sea_orm::{
    sea_query::OnConflict, ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend,
    DatabaseConnection, EntityTrait, FromQueryResult, QueryFilter, QuerySelect, Set, Statement,
    TransactionTrait,
};
use sha2::{Digest, Sha256};
use temps_cloud_client::CloudLink;
use temps_cloud_protocol::{
    BackupCompression, BackupEngine, BackupFormat, NativeSnapshotIdentity,
    NativeSnapshotObjectDeclaration, NativeSnapshotObjectKind, NativeSnapshotRequest,
    WalGObjectCompleted, WalGObjectDeclaration, WalGObjectKind, WalGObjectTargetRequest,
    WalGSnapshotCompleted, WalGSnapshotRequest,
};
use temps_core::EncryptionService;
use temps_entities::{
    backups, cloud_backup_mirror_cursors, cloud_backup_mirror_states, external_service_backups,
    external_services, s3_sources,
};
use tokio::{io::AsyncReadExt, sync::watch};
use tracing::{info, warn};
use uuid::Uuid;

/// Healthy cadence for discovering completed local backups.
const BASE_SWEEP_INTERVAL: Duration = Duration::from_secs(30);
const S3_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const S3_CONTROL_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const S3_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Outage ceiling. Cloud can be unavailable indefinitely without making the
/// self-hosted instance hammer it or affecting local backup completion.
const MAX_SWEEP_INTERVAL: Duration = Duration::from_secs(15 * 60);
const SWEEP_LIMIT: u64 = 50;
/// A corrupt or hostile repository must not turn discovery into an unbounded
/// allocation. This still permits years of WAL segments for ordinary fleets.
const MAX_REPOSITORY_OBJECTS: usize = 100_000;
const MAX_REPOSITORY_KEY_BYTES: usize = 16 * 1024 * 1024;
/// WAL-G sentinels and Temps metadata are small JSON control objects. A larger
/// object is malformed for this protocol and is rejected before allocation.
const MAX_JSON_OBJECT_BYTES: usize = 1024 * 1024;
const MIRROR_STATE_VERSION: u32 = 1;
const DUE_BACKUPS_SQL: &str = r#"
SELECT b.*
FROM cloud_backup_mirror_states AS mirror
JOIN backups AS b ON b.id = mirror.backup_id
WHERE mirror.tenant_id = $1
  AND mirror.outcome <> 'complete'
  AND mirror.retry_after <= $3
  AND b.state = 'completed'
ORDER BY mirror.retry_after ASC, mirror.backup_id ASC
LIMIT $2
"#;
const DISCOVER_BACKUPS_SQL: &str = r#"
SELECT b.*
FROM backups AS b
WHERE b.state = 'completed'
  AND (COALESCE(b.finished_at, b.started_at), b.id) > ($1, $2)
  AND NOT EXISTS (
    SELECT 1
    FROM cloud_backup_mirror_states AS mirror
    WHERE mirror.backup_id = b.id
      AND mirror.tenant_id = $3
  )
ORDER BY COALESCE(b.finished_at, b.started_at) ASC, b.id ASC
LIMIT $4
"#;

enum StageError {
    Unsupported(String),
    Retry(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SweepOutcome {
    NotLinked,
    Idle,
    Progress,
    Retry,
}

fn next_sweep_interval(current: Duration, outcome: SweepOutcome) -> Duration {
    match outcome {
        SweepOutcome::NotLinked => MAX_SWEEP_INTERVAL,
        SweepOutcome::Idle | SweepOutcome::Progress => BASE_SWEEP_INTERVAL,
        SweepOutcome::Retry if current.is_zero() => BASE_SWEEP_INTERVAL,
        SweepOutcome::Retry => (current * 2).min(MAX_SWEEP_INTERVAL),
    }
}

pub async fn run(
    link: Arc<CloudLink>,
    db: Arc<DatabaseConnection>,
    encryption: Arc<EncryptionService>,
    mut cancel: watch::Receiver<bool>,
) {
    info!("Cloud backup mirror started");
    // The first discovery pass is immediate. Subsequent failures back off,
    // while any successful progress resets the healthy cadence.
    let mut retry_in = Duration::ZERO;
    loop {
        tokio::select! {
            changed = cancel.changed() => {
                if changed.is_err() {
                    warn!("Cloud backup mirror stopped because its owner was dropped");
                    return;
                }
                if *cancel.borrow() {
                    info!("Cloud backup mirror stopped after shutdown request");
                    return;
                }
            }
            _ = tokio::time::sleep(retry_in) => {
                let outcome = match sweep(&link, &db, &encryption).await {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        warn!(error = %error, "Cloud backup mirror sweep failed; local backups remain authoritative");
                        SweepOutcome::Retry
                    }
                };
                retry_in = next_sweep_interval(retry_in, outcome);
                if outcome == SweepOutcome::Retry {
                    warn!(
                        retry_in_secs = retry_in.as_secs(),
                        "Cloud backup mirror retained local backup; retrying with exponential backoff"
                    );
                }
            }
        }
    }
}

async fn sweep(
    link: &Arc<CloudLink>,
    db: &Arc<DatabaseConnection>,
    encryption: &Arc<EncryptionService>,
) -> Result<SweepOutcome, sea_orm::DbErr> {
    if !link.is_linked() {
        return Ok(SweepOutcome::NotLinked);
    }
    if !link.backups_enabled() {
        return Ok(SweepOutcome::NotLinked);
    }
    let (Some(tenant_id), Some(instance_id)) = (link.tenant_id(), link.instance_id()) else {
        return Ok(SweepOutcome::NotLinked);
    };
    let selection = select_due_backups(db, tenant_id, SWEEP_LIMIT).await?;
    let candidates = selection.backups;

    tracing::debug!(
        candidate_count = candidates.len(),
        %tenant_id,
        %instance_id,
        "Cloud backup mirror sweep selected local backups"
    );

    if candidates.is_empty() {
        if let Some(watermark) = selection.watermark {
            advance_discovery_cursor(db, tenant_id, watermark).await?;
        }
        return Ok(SweepOutcome::Idle);
    }

    let mut resources = SweepResources::load(db, encryption, &candidates).await?;
    let mut made_progress = false;
    let mut retry_required = false;

    for backup in candidates {
        // Inspection results are useful only while constructing one backup's
        // manifest. Never accumulate object-derived metadata across a sweep.
        resources.object_inspections.clear();
        // Before the indexed state table existed, mirror state lived in a
        // free-form metadata string. Materialize terminal or not-yet-due
        // legacy states in bounded batches. Malformed metadata deliberately
        // falls through and is treated as new work.
        if let Some(legacy) = deferred_legacy_state(&backup.metadata, tenant_id) {
            persist_state(
                db,
                backup.id,
                tenant_id,
                legacy.outcome,
                legacy.classification,
                legacy.reason.as_deref(),
            )
            .await?;
            made_progress = true;
            continue;
        }
        info!(
            local_backup_id = %backup.backup_id,
            "Cloud backup mirror staging local backup"
        );
        match mirror_backup(link, &mut resources, &backup, instance_id).await {
            Ok(()) => {
                info!(local_backup_id = %backup.backup_id, "WAL-G repository mirrored to Cloud");
                persist_state(db, backup.id, tenant_id, "complete", "mirrored", None).await?;
                made_progress = true;
            }
            Err(StageError::Unsupported(reason)) => {
                persist_state(
                    db,
                    backup.id,
                    tenant_id,
                    "retry",
                    "unsupported",
                    Some(&reason),
                )
                .await?;
                retry_required = true;
            }
            Err(StageError::Retry(error)) => {
                warn!(backup_id = %backup.backup_id, error = %error, "Cloud backup mirror unavailable; will retry without affecting the local backup");
                persist_state(db, backup.id, tenant_id, "retry", "transient", Some(&error)).await?;
                retry_required = true;
            }
        }
    }
    if let Some(watermark) = selection.watermark {
        advance_discovery_cursor(db, tenant_id, watermark).await?;
    }
    Ok(if retry_required {
        SweepOutcome::Retry
    } else if made_progress {
        SweepOutcome::Progress
    } else {
        SweepOutcome::Idle
    })
}

/// Select a bounded page of work using the durable mirror-state index.
///
/// Rows without a state are included so pre-migration backups are lazily
/// classified. The query never casts `backups.metadata`; malformed legacy
/// strings therefore cannot poison discovery.
#[derive(Clone, Copy)]
struct DiscoveryWatermark {
    finished_at: chrono::DateTime<chrono::Utc>,
    backup_id: i32,
}

struct SweepSelection {
    backups: Vec<backups::Model>,
    watermark: Option<DiscoveryWatermark>,
}

async fn select_due_backups(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> Result<SweepSelection, sea_orm::DbErr> {
    let retry_limit = (limit / 2).max(1);
    let retry_limit = i64::try_from(retry_limit).unwrap_or(i64::MAX);
    let due = backups::Model::find_by_statement(Statement::from_sql_and_values(
        db.get_database_backend(),
        DUE_BACKUPS_SQL,
        [
            tenant_id.into(),
            retry_limit.into(),
            chrono::Utc::now().into(),
        ],
    ))
    .all(db)
    .await?;

    let cursor = cloud_backup_mirror_cursors::Entity::find_by_id(tenant_id)
        .one(db)
        .await?;
    let cursor_finished_at = cursor
        .as_ref()
        .map(|cursor| cursor.last_finished_at)
        .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH);
    let cursor_backup_id = cursor.as_ref().map_or(0, |cursor| cursor.last_backup_id);
    let remaining = limit.saturating_sub(due.len() as u64);
    let newly_completed = if remaining == 0 {
        Vec::new()
    } else {
        backups::Model::find_by_statement(Statement::from_sql_and_values(
            db.get_database_backend(),
            DISCOVER_BACKUPS_SQL,
            [
                cursor_finished_at.into(),
                cursor_backup_id.into(),
                tenant_id.into(),
                i64::try_from(remaining).unwrap_or(i64::MAX).into(),
            ],
        ))
        .all(db)
        .await?
    };
    let watermark = newly_completed.last().map(|backup| DiscoveryWatermark {
        finished_at: backup.finished_at.unwrap_or(backup.started_at),
        backup_id: backup.id,
    });
    let mut seen = due.iter().map(|backup| backup.id).collect::<HashSet<_>>();
    let mut candidates = due;
    candidates.extend(
        newly_completed
            .into_iter()
            .filter(|backup| seen.insert(backup.id)),
    );
    Ok(SweepSelection {
        backups: candidates,
        watermark,
    })
}

async fn advance_discovery_cursor(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    watermark: DiscoveryWatermark,
) -> Result<(), sea_orm::DbErr> {
    db.execute(Statement::from_sql_and_values(
        db.get_database_backend(),
        r#"
INSERT INTO cloud_backup_mirror_cursors
    (tenant_id, last_finished_at, last_backup_id, updated_at)
VALUES ($1, $2, $3, $4)
ON CONFLICT (tenant_id) DO UPDATE SET
    last_finished_at = EXCLUDED.last_finished_at,
    last_backup_id = EXCLUDED.last_backup_id,
    updated_at = EXCLUDED.updated_at
WHERE (
    cloud_backup_mirror_cursors.last_finished_at,
    cloud_backup_mirror_cursors.last_backup_id
) < (EXCLUDED.last_finished_at, EXCLUDED.last_backup_id)
"#,
        [
            tenant_id.into(),
            watermark.finished_at.into(),
            watermark.backup_id.into(),
            chrono::Utc::now().into(),
        ],
    ))
    .await?;
    Ok(())
}

struct SweepResources<'a> {
    db: &'a DatabaseConnection,
    encryption: &'a EncryptionService,
    external_by_backup: HashMap<i32, external_service_backups::Model>,
    services: HashMap<i32, external_services::Model>,
    sources: HashMap<i32, s3_sources::Model>,
    clients: HashMap<i32, S3Client>,
    object_inspections: HashMap<(i32, String, String), (u64, String)>,
    control_plane_postgres_major: Option<u16>,
}

impl<'a> SweepResources<'a> {
    async fn load(
        db: &'a DatabaseConnection,
        encryption: &'a EncryptionService,
        candidates: &[backups::Model],
    ) -> Result<Self, sea_orm::DbErr> {
        let backup_ids = candidates
            .iter()
            .map(|backup| backup.id)
            .collect::<Vec<_>>();
        let source_ids = candidates
            .iter()
            .map(|backup| backup.s3_source_id)
            .collect::<HashSet<_>>();

        let external_rows = if backup_ids.is_empty() {
            Vec::new()
        } else {
            external_service_backups::Entity::find()
                .filter(external_service_backups::Column::BackupId.is_in(backup_ids))
                .all(db)
                .await?
        };
        let service_ids = external_rows
            .iter()
            .map(|external| external.service_id)
            .collect::<HashSet<_>>();
        let services = if service_ids.is_empty() {
            Vec::new()
        } else {
            external_services::Entity::find()
                .filter(external_services::Column::Id.is_in(service_ids))
                .all(db)
                .await?
        };
        let sources = if source_ids.is_empty() {
            Vec::new()
        } else {
            s3_sources::Entity::find()
                .filter(s3_sources::Column::Id.is_in(source_ids))
                .all(db)
                .await?
        };

        Ok(Self {
            db,
            encryption,
            external_by_backup: external_rows
                .into_iter()
                .map(|external| (external.backup_id, external))
                .collect(),
            services: services
                .into_iter()
                .map(|service| (service.id, service))
                .collect(),
            sources: sources
                .into_iter()
                .map(|source| (source.id, source))
                .collect(),
            clients: HashMap::new(),
            object_inspections: HashMap::new(),
            control_plane_postgres_major: None,
        })
    }

    fn external(&self, backup_id: i32) -> Option<external_service_backups::Model> {
        self.external_by_backup.get(&backup_id).cloned()
    }

    fn service(&self, service_id: i32) -> Result<external_services::Model, StageError> {
        self.services
            .get(&service_id)
            .cloned()
            .ok_or_else(|| StageError::Retry(format!("external service {service_id} is missing")))
    }

    fn source(&self, source_id: i32) -> Result<s3_sources::Model, StageError> {
        self.sources
            .get(&source_id)
            .cloned()
            .ok_or_else(|| StageError::Retry(format!("S3 source {source_id} is missing")))
    }

    fn client(&mut self, source_id: i32) -> Result<S3Client, StageError> {
        if let Some(client) = self.clients.get(&source_id) {
            return Ok(client.clone());
        }
        let source = self.source(source_id)?;
        let client = s3_client(self.encryption, &source)?;
        self.clients.insert(source_id, client.clone());
        Ok(client)
    }

    async fn list_repository(
        &mut self,
        source_id: i32,
        bucket: &str,
        root: &str,
    ) -> Result<Vec<SourceObject>, StageError> {
        let client = self.client(source_id)?;
        // Deliberately do not retain repository listings across the sweep.
        // A sweep may cover 50 backups; caching every key for all of them would
        // multiply the bounded per-repository allocation into process-wide
        // memory pressure.
        list_repository_objects(&client, bucket, root).await
    }

    async fn read_json(
        &mut self,
        source_id: i32,
        bucket: &str,
        key: &str,
    ) -> Result<serde_json::Value, StageError> {
        let client = self.client(source_id)?;
        // Sentinel candidates are normally read once. Retaining parsed values
        // lets storage-controlled JSON allocation accumulate across a sweep,
        // so bounded bodies are parsed and immediately consumed instead.
        read_json_object(&client, bucket, key).await
    }

    async fn inspect_object(
        &mut self,
        source_id: i32,
        bucket: &str,
        key: &str,
    ) -> Result<(u64, String), StageError> {
        let cache_key = (source_id, bucket.to_owned(), key.to_owned());
        if let Some(inspection) = self.object_inspections.get(&cache_key) {
            return Ok(inspection.clone());
        }
        let client = self.client(source_id)?;
        let inspection = inspect_source_object(&client, bucket, key).await?;
        self.object_inspections
            .insert(cache_key, inspection.clone());
        Ok(inspection)
    }

    async fn find_snapshot_sentinel(
        &mut self,
        source_id: i32,
        bucket: &str,
        objects: &[SourceObject],
        backup_uuid: &str,
    ) -> Result<(String, serde_json::Value), StageError> {
        for object in objects
            .iter()
            .filter(|object| object.key.ends_with("_backup_stop_sentinel.json"))
        {
            let value = self.read_json(source_id, bucket, &object.key).await?;
            if contains_backup_identity(&value, backup_uuid) {
                return Ok((object.key.clone(), value));
            }
        }
        Err(StageError::Unsupported(format!(
            "WAL-G repository has no sentinel carrying temps_backup_id={backup_uuid}; rerun the backup with the current Temps image"
        )))
    }
}

async fn mirror_backup(
    link: &CloudLink,
    resources: &mut SweepResources<'_>,
    backup: &backups::Model,
    instance_id: Uuid,
) -> Result<(), StageError> {
    let external = resources.external(backup.id);
    let Some(external) = external else {
        return mirror_walg_backup(link, resources, backup, None, instance_id).await;
    };
    let service = resources.service(external.service_id)?;
    match service.service_type.to_ascii_lowercase().as_str() {
        "postgres" | "postgresql" | "timescale" | "timescaledb" => {
            mirror_walg_backup(link, resources, backup, Some(external), instance_id).await
        }
        engine if supports_native_mirror(engine) => {
            mirror_native_backup(link, resources, backup, &external, &service, instance_id).await
        }
        engine => Err(StageError::Unsupported(format!(
            "Cloud backup mirroring does not support engine {engine}"
        ))),
    }
}

fn supports_native_mirror(service_type: &str) -> bool {
    matches!(
        service_type,
        "mongodb" | "mongo" | "redis" | "mariadb" | "rustfs" | "s3" | "minio" | "blob"
    )
}

async fn mirror_native_backup(
    link: &CloudLink,
    resources: &mut SweepResources<'_>,
    backup: &backups::Model,
    external: &external_service_backups::Model,
    service: &external_services::Model,
    instance_id: Uuid,
) -> Result<(), StageError> {
    let source_config = resources.source(backup.s3_source_id)?;
    let client = resources.client(backup.s3_source_id)?;
    let location = if external.s3_location.trim().is_empty() {
        backup.s3_location.as_str()
    } else {
        external.s3_location.as_str()
    };
    let location_key = s3_key(&source_config.bucket_name, location)?;
    let service_type = service.service_type.to_ascii_lowercase();
    let version = service_engine_version(resources.encryption, service);

    let (root, selected, engine, format, compression, identity) = match service_type.as_str() {
        "mongodb" | "mongo" | "redis" => {
            let root = location_key.trim_end_matches('/').to_string();
            if !root.ends_with("/walg") {
                return Err(StageError::Unsupported(format!(
                    "{service_type} Cloud backups require the WAL-G stream path; got {location}"
                )));
            }
            let all = resources
                .list_repository(backup.s3_source_id, &source_config.bucket_name, &root)
                .await?;
            let (sentinel_key, _) = resources
                .find_snapshot_sentinel(
                    backup.s3_source_id,
                    &source_config.bucket_name,
                    &all,
                    &backup.backup_id,
                )
                .await?;
            let backup_name = sentinel_key
                .rsplit('/')
                .next()
                .and_then(|name| name.strip_suffix("_backup_stop_sentinel.json"))
                .ok_or_else(|| {
                    StageError::Retry(format!("invalid WAL-G stream sentinel {sentinel_key}"))
                })?
                .to_string();
            let selected = all
                .iter()
                .filter(|object| object.key == sentinel_key || object.key.contains(&backup_name))
                .cloned()
                .collect::<Vec<_>>();
            if selected.len() < 2 {
                return Err(StageError::Retry(format!(
                    "WAL-G stream snapshot {backup_name} is incomplete"
                )));
            }
            if service_type == "redis" {
                (
                    root,
                    selected,
                    BackupEngine::Redis,
                    BackupFormat::RedisRdb,
                    BackupCompression::WalGNative,
                    NativeSnapshotIdentity::RedisRdbStream {
                        engine_version: version,
                        backup_name,
                    },
                )
            } else {
                (
                    root,
                    selected,
                    BackupEngine::MongoDb,
                    BackupFormat::MongoDumpArchive,
                    BackupCompression::WalGNative,
                    NativeSnapshotIdentity::MongoDbStream {
                        engine_version: version,
                        backup_name,
                    },
                )
            }
        }
        "mariadb" => {
            let root = location_key.trim_end_matches('/').to_string();
            if !root.ends_with("/walg") {
                return Err(StageError::Unsupported(format!(
                    "MariaDB Cloud backups require the WAL-G repository path; got {location}"
                )));
            }
            let all = resources
                .list_repository(backup.s3_source_id, &source_config.bucket_name, &root)
                .await?;
            let (sentinel_key, _) = resources
                .find_snapshot_sentinel(
                    backup.s3_source_id,
                    &source_config.bucket_name,
                    &all,
                    &backup.backup_id,
                )
                .await?;
            let backup_name = sentinel_key
                .rsplit('/')
                .next()
                .and_then(|name| name.strip_suffix("_backup_stop_sentinel.json"))
                .ok_or_else(|| {
                    StageError::Retry(format!("invalid MariaDB WAL-G sentinel {sentinel_key}"))
                })?
                .to_string();
            let metadata_key = format!("{root}/{}.metadata.json", backup.backup_id);
            let selected = all
                .iter()
                .filter(|object| {
                    object.key == sentinel_key
                        || object.key == metadata_key
                        || object.key.contains(&backup_name)
                })
                .cloned()
                .collect::<Vec<_>>();
            if selected.len() < 3 || !selected.iter().any(|object| object.key == metadata_key) {
                return Err(StageError::Retry(format!(
                    "MariaDB WAL-G snapshot {backup_name} is incomplete or lacks {metadata_key}"
                )));
            }
            let metadata = resources
                .read_json(
                    backup.s3_source_id,
                    &source_config.bucket_name,
                    &metadata_key,
                )
                .await?;
            let binlog_file = metadata
                .pointer("/extra/binlog_file")
                .or_else(|| metadata.get("binlog_file"))
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            let binlog_position = metadata
                .pointer("/extra/binlog_position")
                .or_else(|| metadata.get("binlog_position"))
                .and_then(|value| {
                    value
                        .as_u64()
                        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
                });
            (
                root,
                selected,
                BackupEngine::MariaDb,
                BackupFormat::WalGRepository,
                BackupCompression::WalGNative,
                NativeSnapshotIdentity::MariaDbPhysical {
                    engine_version: version,
                    backup_name,
                    binlog_file,
                    binlog_position,
                },
            )
        }
        "rustfs" | "s3" | "minio" | "blob" => {
            let root = location_key.trim_end_matches('/').to_string();
            let selected = resources
                .list_repository(backup.s3_source_id, &source_config.bucket_name, &root)
                .await?
                .iter()
                .filter(|object| !object.key.ends_with("/metadata.json"))
                .cloned()
                .collect::<Vec<_>>();
            if selected.is_empty() {
                return Err(StageError::Retry(format!(
                    "RustFS snapshot {root} has no objects"
                )));
            }
            (
                root,
                selected,
                BackupEngine::RustFs,
                BackupFormat::ObjectSet,
                BackupCompression::None,
                NativeSnapshotIdentity::ObjectSet {
                    snapshot_name: backup.backup_id.clone(),
                },
            )
        }
        engine => {
            return Err(StageError::Unsupported(format!(
                "native mirror does not support {engine}"
            )))
        }
    };

    let mut declarations = Vec::with_capacity(selected.len());
    for object in &selected {
        let (bytes, checksum_sha256) = resources
            .inspect_object(backup.s3_source_id, &source_config.bucket_name, &object.key)
            .await?;
        if bytes != object.bytes {
            return Err(StageError::Retry(format!(
                "native snapshot object {} changed while its manifest was built",
                object.key
            )));
        }
        let relative_key = object
            .key
            .strip_prefix(&format!("{root}/"))
            .unwrap_or_else(|| object.key.rsplit('/').next().unwrap_or(&object.key))
            .to_string();
        let kind = if object.key.ends_with("_backup_stop_sentinel.json")
            || object.key.ends_with("metadata.json")
        {
            NativeSnapshotObjectKind::Metadata
        } else if engine == BackupEngine::MariaDb {
            NativeSnapshotObjectKind::BaseBackup
        } else if engine == BackupEngine::RustFs {
            NativeSnapshotObjectKind::Object
        } else {
            NativeSnapshotObjectKind::Data
        };
        declarations.push(NativeSnapshotObjectDeclaration {
            relative_key,
            kind,
            bytes,
            checksum_sha256,
        });
    }
    let cloud_backup_id = Uuid::new_v5(
        &link
            .tenant_id()
            .ok_or_else(|| StageError::Retry("Cloud link lost its tenant identity".into()))?,
        format!("{instance_id}:{}", backup.backup_id).as_bytes(),
    );
    let request = NativeSnapshotRequest {
        backup_id: cloud_backup_id,
        instance_id,
        source: format!("{}/{}", service.service_type, service.name),
        engine,
        format,
        compression,
        identity,
        objects: declarations.clone(),
    };
    let snapshot = link
        .declare_native_snapshot(&request)
        .await
        .map_err(|error| StageError::Retry(error.to_string()))?;
    if snapshot.upload_required {
        for declaration in declarations {
            upload_native_object(
                link,
                &client,
                &source_config.bucket_name,
                &root,
                instance_id,
                cloud_backup_id,
                declaration,
            )
            .await?;
        }
    }
    link.complete_native_snapshot(&WalGSnapshotCompleted {
        backup_id: cloud_backup_id,
    })
    .await
    .map_err(|error| StageError::Retry(error.to_string()))
}

async fn mirror_walg_backup(
    link: &CloudLink,
    resources: &mut SweepResources<'_>,
    backup: &backups::Model,
    external: Option<external_service_backups::Model>,
    instance_id: Uuid,
) -> Result<(), StageError> {
    let root = walg_root_key(&backup.s3_location).ok_or_else(|| {
        StageError::Unsupported(format!(
            "{} is not a WAL-G repository; PostgreSQL Cloud backups require WAL-G",
            backup.s3_location
        ))
    })?;
    let (source, engine, postgres_major) = load_postgres_identity(resources, external).await?;
    let source_config = resources.source(backup.s3_source_id)?;
    let client = resources.client(backup.s3_source_id)?;
    let objects = resources
        .list_repository(backup.s3_source_id, &source_config.bucket_name, &root)
        .await?;
    let (sentinel_key, sentinel) = resources
        .find_snapshot_sentinel(
            backup.s3_source_id,
            &source_config.bucket_name,
            &objects,
            &backup.backup_id,
        )
        .await?;
    let backup_name = sentinel_key
        .rsplit('/')
        .next()
        .and_then(|name| name.strip_suffix("_backup_stop_sentinel.json"))
        .ok_or_else(|| StageError::Retry(format!("invalid WAL-G sentinel key {sentinel_key}")))?
        .to_string();
    let timeline = sentinel_u32(&sentinel, &["Timeline", "timeline"])
        .or_else(|| timeline_from_backup_name(&backup_name))
        .unwrap_or(1);
    let start_lsn = sentinel_lsn(&sentinel, &["StartLsn", "StartLSN", "start_lsn", "LSN"])
        .ok_or_else(|| {
            StageError::Unsupported("WAL-G sentinel does not report start LSN".into())
        })?;
    let finish_lsn = sentinel_lsn(&sentinel, &["FinishLsn", "FinishLSN", "finish_lsn", "LSN"])
        .ok_or_else(|| {
            StageError::Unsupported("WAL-G sentinel does not report finish LSN".into())
        })?;
    let first_wal = wal_segment_name(&start_lsn, timeline)?;
    let last_wal = wal_segment_name(&finish_lsn, timeline)?;
    let base_prefix = format!("{root}/basebackups_005/{backup_name}/");
    let wal_prefix = format!("{root}/wal_005/");
    let selected = objects
        .iter()
        .filter(|object| {
            object.key == sentinel_key
                || object.key.starts_with(&base_prefix)
                || object.key.strip_prefix(&wal_prefix).is_some_and(|name| {
                    let segment = name.get(..24).unwrap_or(name);
                    segment >= first_wal.as_str() && segment <= last_wal.as_str()
                })
        })
        .cloned()
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return Err(StageError::Retry(format!(
            "WAL-G snapshot {backup_name} has no repository objects"
        )));
    }

    let mut declarations = Vec::with_capacity(selected.len());
    for object in &selected {
        let (bytes, checksum_sha256) = resources
            .inspect_object(backup.s3_source_id, &source_config.bucket_name, &object.key)
            .await?;
        if bytes != object.bytes {
            return Err(StageError::Retry(format!(
                "WAL-G object {} changed while its manifest was being built",
                object.key
            )));
        }
        declarations.push(WalGObjectDeclaration {
            relative_key: object
                .key
                .strip_prefix(&format!("{root}/"))
                .ok_or_else(|| {
                    StageError::Retry(format!("object {} escaped repository", object.key))
                })?
                .to_string(),
            kind: if object.key == sentinel_key {
                WalGObjectKind::Sentinel
            } else if object.key.starts_with(&base_prefix) {
                WalGObjectKind::BaseBackup
            } else {
                WalGObjectKind::Wal
            },
            bytes,
            checksum_sha256,
        });
    }
    let cloud_backup_id = Uuid::new_v5(
        &link
            .tenant_id()
            .ok_or_else(|| StageError::Retry("Cloud link lost its tenant identity".into()))?,
        format!("{instance_id}:{}", backup.backup_id).as_bytes(),
    );
    let request = WalGSnapshotRequest {
        backup_id: cloud_backup_id,
        instance_id,
        source,
        engine,
        postgres_major,
        postgres_system_identifier: sentinel_string(
            &sentinel,
            &["SystemIdentifier", "system_identifier"],
        )
        .ok_or_else(|| {
            StageError::Unsupported(
                "WAL-G sentinel does not report PostgreSQL system identifier".into(),
            )
        })?,
        backup_name,
        timeline,
        start_lsn,
        finish_lsn,
        objects: declarations.clone(),
    };
    let snapshot = link
        .declare_walg_snapshot(&request)
        .await
        .map_err(|error| StageError::Retry(error.to_string()))?;
    if snapshot.upload_required {
        for declaration in declarations {
            upload_repository_object(
                link,
                &client,
                &source_config.bucket_name,
                &root,
                instance_id,
                cloud_backup_id,
                declaration,
            )
            .await?;
        }
    }
    link.complete_walg_snapshot(&WalGSnapshotCompleted {
        backup_id: cloud_backup_id,
    })
    .await
    .map_err(|error| StageError::Retry(error.to_string()))
}

#[derive(Clone, Debug)]
struct SourceObject {
    key: String,
    bytes: u64,
}

async fn list_repository_objects(
    client: &S3Client,
    bucket: &str,
    root: &str,
) -> Result<Vec<SourceObject>, StageError> {
    let mut objects = Vec::new();
    let mut key_bytes = 0usize;
    let mut continuation = None;
    loop {
        let mut request = client
            .list_objects_v2()
            .bucket(bucket)
            .prefix(format!("{root}/"));
        if let Some(token) = continuation.take() {
            request = request.continuation_token(token);
        }
        let response = tokio::time::timeout(S3_CONTROL_REQUEST_TIMEOUT, request.send())
            .await
            .map_err(|_| StageError::Retry("listing the backup repository timed out".into()))?
            .map_err(|error| {
                StageError::Retry(format!("could not list WAL-G repository: {error}"))
            })?;
        for object in response.contents() {
            if let (Some(key), Some(bytes)) = (
                object.key(),
                object.size().and_then(|value| u64::try_from(value).ok()),
            ) {
                append_source_object(
                    &mut objects,
                    &mut key_bytes,
                    key,
                    bytes,
                    MAX_REPOSITORY_OBJECTS,
                    MAX_REPOSITORY_KEY_BYTES,
                )?;
            }
        }
        if response.is_truncated().unwrap_or(false) {
            continuation = response.next_continuation_token().map(str::to_string);
            if continuation.is_none() {
                break;
            }
        } else {
            break;
        }
    }
    Ok(objects)
}

fn append_source_object(
    objects: &mut Vec<SourceObject>,
    key_bytes: &mut usize,
    key: &str,
    bytes: u64,
    max_objects: usize,
    max_key_bytes: usize,
) -> Result<(), StageError> {
    if objects.len() >= max_objects {
        return Err(StageError::Unsupported(format!(
            "backup repository exceeds the {max_objects} object safety limit"
        )));
    }
    let next_key_bytes = key_bytes.checked_add(key.len()).ok_or_else(|| {
        StageError::Unsupported("backup repository key metadata size overflowed".into())
    })?;
    if next_key_bytes > max_key_bytes {
        return Err(StageError::Unsupported(format!(
            "backup repository key metadata exceeds the {max_key_bytes} byte safety limit"
        )));
    }
    *key_bytes = next_key_bytes;
    objects.push(SourceObject {
        key: key.to_owned(),
        bytes,
    });
    Ok(())
}

fn contains_backup_identity(value: &serde_json::Value, backup_uuid: &str) -> bool {
    match value {
        serde_json::Value::Object(values) => values.iter().any(|(key, value)| {
            (key == "temps_backup_id" && value.as_str() == Some(backup_uuid))
                || contains_backup_identity(value, backup_uuid)
        }),
        serde_json::Value::Array(values) => values
            .iter()
            .any(|value| contains_backup_identity(value, backup_uuid)),
        _ => false,
    }
}

fn sentinel_string(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    match value {
        serde_json::Value::Object(values) => {
            for key in keys {
                if let Some(value) = values.get(*key) {
                    if let Some(string) = value.as_str() {
                        return Some(string.to_string());
                    }
                    if let Some(number) = value.as_u64() {
                        return Some(number.to_string());
                    }
                }
            }
            values
                .values()
                .find_map(|value| sentinel_string(value, keys))
        }
        serde_json::Value::Array(values) => {
            values.iter().find_map(|value| sentinel_string(value, keys))
        }
        _ => None,
    }
}

fn sentinel_lsn(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    match value {
        serde_json::Value::Object(values) => {
            for key in keys {
                if let Some(value) = values.get(*key) {
                    if let Some(string) = value.as_str() {
                        return Some(string.to_string());
                    }
                    if let Some(number) = value.as_u64() {
                        return Some(format!("{:X}/{:X}", number >> 32, number & 0xffff_ffff));
                    }
                }
            }
            values.values().find_map(|value| sentinel_lsn(value, keys))
        }
        serde_json::Value::Array(values) => {
            values.iter().find_map(|value| sentinel_lsn(value, keys))
        }
        _ => None,
    }
}

fn timeline_from_backup_name(name: &str) -> Option<u32> {
    u32::from_str_radix(name.strip_prefix("base_")?.get(..8)?, 16).ok()
}

fn sentinel_u32(value: &serde_json::Value, keys: &[&str]) -> Option<u32> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
    })
}

/// Convert an LSN into the sortable 24-character WAL segment filename used by
/// Temps-managed PostgreSQL images (default 16 MiB WAL segment size).
fn wal_segment_name(lsn: &str, timeline: u32) -> Result<String, StageError> {
    const SEGMENT_BYTES: u64 = 16 * 1024 * 1024;
    const SEGMENTS_PER_LOG: u64 = 0x1_0000_0000 / SEGMENT_BYTES;
    let (high, low) = lsn.split_once('/').ok_or_else(|| {
        StageError::Unsupported(format!("WAL-G sentinel contains invalid LSN {lsn:?}"))
    })?;
    let high = u64::from_str_radix(high, 16).map_err(|error| {
        StageError::Unsupported(format!(
            "WAL-G sentinel contains invalid LSN {lsn:?}: {error}"
        ))
    })?;
    let low = u64::from_str_radix(low, 16).map_err(|error| {
        StageError::Unsupported(format!(
            "WAL-G sentinel contains invalid LSN {lsn:?}: {error}"
        ))
    })?;
    let segment_number = ((high << 32) | low) / SEGMENT_BYTES;
    Ok(format!(
        "{timeline:08X}{:08X}{:08X}",
        segment_number / SEGMENTS_PER_LOG,
        segment_number % SEGMENTS_PER_LOG
    ))
}

async fn inspect_source_object(
    client: &S3Client,
    bucket: &str,
    key: &str,
) -> Result<(u64, String), StageError> {
    let response = client
        .get_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .map_err(|error| {
            StageError::Retry(format!("could not read WAL-G object {key}: {error}"))
        })?;
    let mut reader = response.body.into_async_read();
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut bytes = 0_u64;
    let mut digest = Sha256::new();
    loop {
        let read = tokio::time::timeout(S3_STREAM_IDLE_TIMEOUT, reader.read(&mut buffer))
            .await
            .map_err(|_| {
                StageError::Retry(format!(
                    "source object {key} made no progress for {} seconds",
                    S3_STREAM_IDLE_TIMEOUT.as_secs()
                ))
            })?
            .map_err(|error| {
                StageError::Retry(format!("could not stream WAL-G object {key}: {error}"))
            })?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| StageError::Retry(format!("WAL-G object {key} is too large")))?;
        digest.update(&buffer[..read]);
    }
    let checksum = digest
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            use std::fmt::Write;
            let _ = write!(output, "{byte:02x}");
            output
        });
    Ok((bytes, checksum))
}

async fn upload_repository_object(
    link: &CloudLink,
    source_client: &S3Client,
    bucket: &str,
    root: &str,
    instance_id: Uuid,
    backup_id: Uuid,
    declaration: WalGObjectDeclaration,
) -> Result<(), StageError> {
    let target = link
        .walg_object_target(&WalGObjectTargetRequest {
            backup_id,
            instance_id,
            relative_key: declaration.relative_key.clone(),
        })
        .await
        .map_err(|error| StageError::Retry(error.to_string()))?;
    if target.upload_required {
        let source_key = format!("{root}/{}", declaration.relative_key);
        let mut last_failure = None;
        for attempt in 0..3 {
            // Reopen the S3 source on every attempt. The stream is not
            // rewindable, but retrying never allocates a local staging file.
            let response = match tokio::time::timeout(
                S3_CONTROL_REQUEST_TIMEOUT,
                source_client
                    .get_object()
                    .bucket(bucket)
                    .key(&source_key)
                    .send(),
            )
            .await
            {
                Ok(Ok(response)) => response,
                Ok(Err(error)) => {
                    last_failure = Some(format!(
                        "could not reopen WAL-G object {source_key}: {error}"
                    ));
                    if attempt < 2 {
                        tokio::time::sleep(Duration::from_millis(250 * (1 << attempt))).await;
                    }
                    continue;
                }
                Err(_) => {
                    last_failure = Some(format!("opening WAL-G object {source_key} timed out"));
                    if attempt < 2 {
                        tokio::time::sleep(Duration::from_millis(250 * (1 << attempt))).await;
                    }
                    continue;
                }
            };
            match link
                .upload_backup_object_reader(
                    &target,
                    response.body.into_async_read(),
                    declaration.bytes,
                )
                .await
            {
                Ok(response) if response.status().is_success() => {
                    last_failure = None;
                    break;
                }
                Ok(response)
                    if matches!(response.status().as_u16(), 408 | 425 | 429 | 500..=599) =>
                {
                    last_failure = Some(format!("object storage returned {}", response.status()));
                }
                Ok(response) if matches!(response.status().as_u16(), 409 | 412) => {
                    // The previous PUT may have committed before its response
                    // was lost. Completion verifies the immutable object's
                    // declared size and checksum and is the safe arbiter.
                    last_failure = None;
                    break;
                }
                Ok(response) => {
                    return Err(StageError::Unsupported(format!(
                        "Cloud object storage rejected {} with {}",
                        declaration.relative_key,
                        response.status()
                    )));
                }
                Err(error) if error.is_retryable() => {
                    last_failure = Some(error.to_string());
                }
                Err(error) => {
                    return Err(StageError::Unsupported(format!(
                        "Cloud returned an invalid upload target for {}: {error}",
                        declaration.relative_key
                    )));
                }
            }
            if attempt < 2 {
                tokio::time::sleep(Duration::from_millis(250 * (1 << attempt))).await;
            }
        }
        if let Some(reason) = last_failure {
            return Err(StageError::Retry(format!(
                "WAL-G object {} did not upload after bounded retries: {reason}",
                declaration.relative_key
            )));
        }
    }
    link.complete_walg_object(&WalGObjectCompleted {
        backup_id,
        relative_key: declaration.relative_key,
        bytes: declaration.bytes,
        checksum_sha256: declaration.checksum_sha256,
    })
    .await
    .map_err(|error| StageError::Retry(error.to_string()))
}

async fn upload_native_object(
    link: &CloudLink,
    source_client: &S3Client,
    bucket: &str,
    root: &str,
    instance_id: Uuid,
    backup_id: Uuid,
    declaration: NativeSnapshotObjectDeclaration,
) -> Result<(), StageError> {
    let target = link
        .native_object_target(&WalGObjectTargetRequest {
            backup_id,
            instance_id,
            relative_key: declaration.relative_key.clone(),
        })
        .await
        .map_err(|error| StageError::Retry(error.to_string()))?;
    if target.upload_required {
        let source_key = format!("{root}/{}", declaration.relative_key);
        let mut last_failure = None;
        for attempt in 0..3 {
            let response = match tokio::time::timeout(
                S3_CONTROL_REQUEST_TIMEOUT,
                source_client
                    .get_object()
                    .bucket(bucket)
                    .key(&source_key)
                    .send(),
            )
            .await
            {
                Ok(Ok(response)) => response,
                Ok(Err(error)) => {
                    last_failure = Some(format!(
                        "could not reopen native snapshot object {source_key}: {error}"
                    ));
                    if attempt < 2 {
                        tokio::time::sleep(Duration::from_millis(250 * (1 << attempt))).await;
                    }
                    continue;
                }
                Err(_) => {
                    last_failure = Some(format!(
                        "opening native snapshot object {source_key} timed out"
                    ));
                    if attempt < 2 {
                        tokio::time::sleep(Duration::from_millis(250 * (1 << attempt))).await;
                    }
                    continue;
                }
            };
            match link
                .upload_backup_object_reader(
                    &target,
                    response.body.into_async_read(),
                    declaration.bytes,
                )
                .await
            {
                Ok(response) if response.status().is_success() => {
                    last_failure = None;
                    break;
                }
                Ok(response)
                    if matches!(response.status().as_u16(), 408 | 425 | 429 | 500..=599) =>
                {
                    last_failure = Some(format!("object storage returned {}", response.status()));
                }
                Ok(response) if matches!(response.status().as_u16(), 409 | 412) => {
                    last_failure = None;
                    break;
                }
                Ok(response) => {
                    return Err(StageError::Unsupported(format!(
                        "Cloud object storage rejected {} with {}",
                        declaration.relative_key,
                        response.status()
                    )));
                }
                Err(error) if error.is_retryable() => {
                    last_failure = Some(error.to_string());
                }
                Err(error) => {
                    return Err(StageError::Unsupported(format!(
                        "Cloud returned an invalid upload target for {}: {error}",
                        declaration.relative_key
                    )));
                }
            }
            if attempt < 2 {
                tokio::time::sleep(Duration::from_millis(250 * (1 << attempt))).await;
            }
        }
        if let Some(reason) = last_failure {
            return Err(StageError::Retry(format!(
                "native snapshot object {} did not upload after bounded retries: {reason}",
                declaration.relative_key
            )));
        }
    }
    link.complete_native_object(&WalGObjectCompleted {
        backup_id,
        relative_key: declaration.relative_key,
        bytes: declaration.bytes,
        checksum_sha256: declaration.checksum_sha256,
    })
    .await
    .map_err(|error| StageError::Retry(error.to_string()))
}

fn s3_key(expected_bucket: &str, location: &str) -> Result<String, StageError> {
    if let Some(without_scheme) = location.strip_prefix("s3://") {
        let (bucket, key) = without_scheme.split_once('/').ok_or_else(|| {
            StageError::Unsupported(format!("S3 location {location:?} has no object key"))
        })?;
        if bucket != expected_bucket {
            return Err(StageError::Unsupported(format!(
                "backup location bucket {bucket} does not match configured source bucket {expected_bucket}"
            )));
        }
        return Ok(key.trim_matches('/').to_string());
    }
    let key = location.trim_matches('/');
    if key.is_empty() {
        return Err(StageError::Unsupported(
            "backup location has no object key".into(),
        ));
    }
    Ok(key.to_string())
}

async fn read_json_object(
    client: &S3Client,
    bucket: &str,
    key: &str,
) -> Result<serde_json::Value, StageError> {
    let response = tokio::time::timeout(
        S3_CONTROL_REQUEST_TIMEOUT,
        client.get_object().bucket(bucket).key(key).send(),
    )
    .await
    .map_err(|_| StageError::Retry(format!("reading {key} timed out")))?
    .map_err(|error| StageError::Retry(format!("could not read {key}: {error}")))?;
    if let Some(length) = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
    {
        ensure_json_object_size(key, length, MAX_JSON_OBJECT_BYTES)?;
    }
    let mut bytes = Vec::with_capacity(
        response
            .content_length()
            .and_then(|length| usize::try_from(length).ok())
            .unwrap_or(0)
            .min(MAX_JSON_OBJECT_BYTES),
    );
    let mut body = response
        .body
        .into_async_read()
        .take((MAX_JSON_OBJECT_BYTES + 1) as u64);
    tokio::time::timeout(S3_CONTROL_REQUEST_TIMEOUT, body.read_to_end(&mut bytes))
        .await
        .map_err(|_| StageError::Retry(format!("reading the body of {key} timed out")))?
        .map_err(|error| StageError::Retry(format!("could not stream {key}: {error}")))?;
    ensure_json_object_size(key, bytes.len(), MAX_JSON_OBJECT_BYTES)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| StageError::Retry(format!("object {key} is invalid JSON: {error}")))
}

fn ensure_json_object_size(key: &str, bytes: usize, limit: usize) -> Result<(), StageError> {
    if bytes > limit {
        return Err(StageError::Unsupported(format!(
            "JSON control object {key} exceeds the {limit} byte safety limit"
        )));
    }
    Ok(())
}

fn walg_root_key(location: &str) -> Option<String> {
    let without_scheme = location.strip_prefix("s3://")?;
    let (_, key) = without_scheme.split_once('/')?;
    let key = key.trim_end_matches('/');
    (key.ends_with("/walg") || key.contains("/walg/")).then(|| key.to_string())
}

async fn load_postgres_identity(
    resources: &mut SweepResources<'_>,
    external: Option<external_service_backups::Model>,
) -> Result<(String, BackupEngine, u16), StageError> {
    if let Some(external) = external {
        let service = resources.service(external.service_id)?;
        let engine = match service.service_type.to_ascii_lowercase().as_str() {
            "postgres" | "postgresql" => BackupEngine::Postgres,
            "timescale" | "timescaledb" => BackupEngine::TimescaleDb,
            other => {
                return Err(StageError::Unsupported(format!(
                    "engine {other} is not WAL-G compatible"
                )))
            }
        };
        let major = parse_postgres_major(service.version.as_deref()).ok_or_else(|| {
            StageError::Unsupported(format!(
                "service {} has no PostgreSQL major version",
                service.name
            ))
        })?;
        Ok((
            format!("{}/{}", service.service_type, service.name),
            engine,
            major,
        ))
    } else {
        let major = if let Some(major) = resources.control_plane_postgres_major {
            major
        } else {
            let row = resources
                .db
                .query_one(Statement::from_string(
                    resources.db.get_database_backend(),
                    "SELECT current_setting('server_version') AS server_version".to_string(),
                ))
                .await
                .map_err(|error| StageError::Retry(error.to_string()))?
                .ok_or_else(|| {
                    StageError::Retry("PostgreSQL did not return server_version".into())
                })?;
            let version: String = row
                .try_get("", "server_version")
                .map_err(|error| StageError::Retry(error.to_string()))?;
            let major = parse_postgres_major(Some(&version)).ok_or_else(|| {
                StageError::Unsupported(format!("unsupported PostgreSQL version {version}"))
            })?;
            resources.control_plane_postgres_major = Some(major);
            major
        };
        Ok((
            "postgres/control-plane".into(),
            BackupEngine::Postgres,
            major,
        ))
    }
}

fn s3_client(
    encryption: &EncryptionService,
    source: &s3_sources::Model,
) -> Result<S3Client, StageError> {
    let access_key = encryption
        .decrypt_string(&source.access_key_id)
        .map_err(|error| StageError::Retry(format!("could not decrypt S3 access key: {error}")))?;
    let secret_key = encryption
        .decrypt_string(&source.secret_key)
        .map_err(|error| StageError::Retry(format!("could not decrypt S3 secret key: {error}")))?;
    let credentials = aws_sdk_s3::config::Credentials::new(
        access_key,
        secret_key,
        None,
        None,
        "cloud-backup-mirror",
    );
    let mut builder = Config::builder()
        .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
        .region(aws_sdk_s3::config::Region::new(source.region.clone()))
        .force_path_style(source.force_path_style.unwrap_or(true))
        .credentials_provider(credentials)
        .retry_config(aws_sdk_s3::config::retry::RetryConfig::standard().with_max_attempts(3))
        // Connect attempts are bounded, but no whole-operation timeout is set:
        // a valid multi-gigabyte streaming GET must not be killed by its size.
        .timeout_config(
            aws_sdk_s3::config::timeout::TimeoutConfig::builder()
                .connect_timeout(S3_CONNECT_TIMEOUT)
                .build(),
        );
    if let Some(endpoint) = &source.endpoint {
        builder = builder.endpoint_url(if endpoint.starts_with("http") {
            endpoint.clone()
        } else {
            format!("http://{endpoint}")
        });
    }
    Ok(S3Client::from_conf(builder.build()))
}

fn parse_postgres_major(version: Option<&str>) -> Option<u16> {
    version?
        .trim_start_matches(|character: char| !character.is_ascii_digit())
        .split(|character: char| !character.is_ascii_digit())
        .next()
        .and_then(|major| major.parse().ok())
        .filter(|major| (10..=99).contains(major))
}

fn service_engine_version(
    encryption: &EncryptionService,
    service: &external_services::Model,
) -> String {
    if let Some(version) = service
        .version
        .as_deref()
        .map(str::trim)
        .filter(|version| !version.is_empty())
    {
        return version.to_owned();
    }

    service
        .config
        .as_deref()
        .and_then(|config| encryption.decrypt_string(config).ok())
        .and_then(|config| serde_json::from_str::<serde_json::Value>(&config).ok())
        .and_then(|config| {
            config
                .get("docker_image")
                .and_then(serde_json::Value::as_str)
                .and_then(image_tag_version)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn image_tag_version(image: &str) -> Option<&str> {
    let image = image.split('@').next()?.trim();
    let (repository, tag) = image.rsplit_once(':')?;
    (!tag.is_empty() && !tag.contains('/') && !repository.ends_with('/')).then_some(tag)
}

async fn persist_state(
    db: &DatabaseConnection,
    backup_id: i32,
    tenant_id: Uuid,
    outcome: &str,
    classification: &str,
    reason: Option<&str>,
) -> Result<(), sea_orm::DbErr> {
    // Re-read under a row lock. The sweep model may be minutes old by the time
    // S3 work completes; writing it back would erase metadata concurrently
    // added by the local backup workflow.
    let transaction = db.begin().await?;
    let query = backups::Entity::find_by_id(backup_id);
    let backup = if db.get_database_backend() == DatabaseBackend::Postgres {
        query.lock_exclusive().one(&transaction).await?
    } else {
        // SQLite serializes the write transaction itself and does not support
        // SELECT ... FOR UPDATE.
        query.one(&transaction).await?
    }
    .ok_or_else(|| {
        sea_orm::DbErr::RecordNotFound(format!(
            "backup {backup_id} disappeared while persisting Cloud mirror state"
        ))
    })?;
    let previous_attempt_count =
        cloud_backup_mirror_states::Entity::find_by_id((backup_id, tenant_id))
            .one(&transaction)
            .await?
            .map_or(0, |state| state.attempt_count);
    let attempt_count = if outcome == "retry" {
        previous_attempt_count.saturating_add(1)
    } else {
        0
    };
    let retry_after = (outcome == "retry")
        .then(|| chrono::Utc::now() + mirror_retry_delay(classification, attempt_count));
    let metadata = merge_mirror_state(
        &backup.metadata,
        tenant_id,
        outcome,
        classification,
        reason,
        retry_after,
    );
    if let Some(metadata) = metadata {
        let mut active: backups::ActiveModel = backup.into();
        active.metadata = Set(metadata.to_string());
        active.update(&transaction).await?;
    }
    cloud_backup_mirror_states::Entity::insert(cloud_backup_mirror_states::ActiveModel {
        backup_id: Set(backup_id),
        tenant_id: Set(tenant_id),
        schema_version: Set(i32::try_from(MIRROR_STATE_VERSION).unwrap_or(i32::MAX)),
        outcome: Set(outcome.to_owned()),
        classification: Set(classification.to_owned()),
        reason: Set(reason.map(str::to_owned)),
        attempt_count: Set(attempt_count),
        retry_after: Set(retry_after),
        updated_at: Set(chrono::Utc::now()),
    })
    .on_conflict(
        OnConflict::columns([
            cloud_backup_mirror_states::Column::BackupId,
            cloud_backup_mirror_states::Column::TenantId,
        ])
        .update_columns([
            cloud_backup_mirror_states::Column::SchemaVersion,
            cloud_backup_mirror_states::Column::Outcome,
            cloud_backup_mirror_states::Column::Classification,
            cloud_backup_mirror_states::Column::Reason,
            cloud_backup_mirror_states::Column::AttemptCount,
            cloud_backup_mirror_states::Column::RetryAfter,
            cloud_backup_mirror_states::Column::UpdatedAt,
        ])
        .to_owned(),
    )
    .exec(&transaction)
    .await?;
    transaction.commit().await?;
    Ok(())
}

fn mirror_retry_delay(classification: &str, attempt_count: i32) -> chrono::Duration {
    if classification == "unsupported" {
        return chrono::Duration::from_std(MAX_SWEEP_INTERVAL)
            .unwrap_or_else(|_| chrono::Duration::minutes(15));
    }
    let exponent = u32::try_from(attempt_count.saturating_sub(1))
        .unwrap_or_default()
        .min(5);
    let seconds = 30_u64.saturating_mul(1_u64 << exponent);
    chrono::Duration::seconds(
        i64::try_from(seconds.min(MAX_SWEEP_INTERVAL.as_secs())).unwrap_or(900),
    )
}

fn merge_mirror_state(
    current_metadata: &str,
    tenant_id: Uuid,
    outcome: &str,
    classification: &str,
    reason: Option<&str>,
    retry_after: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<serde_json::Value> {
    let mut metadata = serde_json::from_str::<serde_json::Value>(current_metadata).ok()?;
    if !metadata.is_object() {
        return None;
    }
    let root = metadata.as_object_mut()?;
    let cloud = root
        .entry("cloud_mirror")
        .or_insert_with(|| serde_json::json!({}));
    if !cloud.is_object() {
        *cloud = serde_json::json!({});
    }
    let cloud = cloud.as_object_mut()?;
    cloud.insert(
        tenant_id.to_string(),
        serde_json::json!({
            "schema_version": MIRROR_STATE_VERSION,
            "outcome": outcome,
            "classification": classification,
            "reason": reason,
            "retry_after": retry_after.map(|value| value.to_rfc3339()),
            "updated_at": chrono::Utc::now().to_rfc3339(),
        }),
    );
    Some(metadata)
}

struct LegacyMirrorState {
    outcome: &'static str,
    classification: &'static str,
    reason: Option<String>,
}

fn deferred_legacy_state(metadata: &str, tenant_id: Uuid) -> Option<LegacyMirrorState> {
    let metadata = serde_json::from_str::<serde_json::Value>(metadata).ok()?;
    let state = metadata.get("cloud_mirror")?.get(tenant_id.to_string())?;
    if state.get("outcome").and_then(serde_json::Value::as_str) == Some("complete")
        || state.get("state").and_then(serde_json::Value::as_str) == Some("complete")
    {
        return Some(LegacyMirrorState {
            outcome: "complete",
            classification: "legacy",
            reason: state
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        });
    }
    if state
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        != Some(u64::from(MIRROR_STATE_VERSION))
    {
        return None;
    }
    let retry_after = state
        .get("retry_after")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())?;
    (retry_after > chrono::Utc::now()).then(|| LegacyMirrorState {
        outcome: "retry",
        classification: "legacy",
        reason: state
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        append_source_object, contains_backup_identity, deferred_legacy_state,
        ensure_json_object_size, image_tag_version, merge_mirror_state, next_sweep_interval,
        parse_postgres_major, s3_key, select_due_backups, sentinel_lsn, supports_native_mirror,
        timeline_from_backup_name, upload_native_object, wal_segment_name, walg_root_key,
        SourceObject, StageError, SweepOutcome, BASE_SWEEP_INTERVAL, DISCOVER_BACKUPS_SQL,
        DUE_BACKUPS_SQL, MAX_SWEEP_INTERVAL, MIRROR_STATE_VERSION,
    };
    use std::{
        collections::HashMap,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc, Mutex,
        },
        time::Duration,
    };

    use aws_sdk_s3::{Client as S3Client, Config};
    use axum::{
        body::{to_bytes, Body},
        extract::{Path, State},
        http::{header, HeaderMap, StatusCode},
        routing::{get, post, put},
        Json, Router,
    };
    use sea_orm::{
        ActiveModelTrait, ColumnTrait, ConnectionTrait, Database, DatabaseBackend, EntityTrait,
        IntoActiveModel, MockDatabase, QueryFilter, QueryOrder, Schema, Set, Statement,
        TransactionTrait,
    };
    use sha2::{Digest, Sha256};
    use temps_cloud_client::{BackendUrl, CloudFeatureSwitches, CloudLink};
    use temps_cloud_protocol::{
        NativeSnapshotObjectDeclaration, NativeSnapshotObjectKind, WalGObjectCompleted,
        WalGObjectTargetRequest,
    };
    use uuid::Uuid;

    #[test]
    fn repository_discovery_rejects_excess_object_count() {
        let mut objects = Vec::<SourceObject>::new();
        let mut key_bytes = 0;
        assert!(
            append_source_object(&mut objects, &mut key_bytes, "first", 1, 1, 100).is_ok(),
            "first object is within the limit"
        );

        let error = append_source_object(&mut objects, &mut key_bytes, "second", 1, 1, 100)
            .expect_err("second object must exceed the count limit");
        assert!(
            matches!(error, StageError::Unsupported(reason) if reason.contains("object safety limit"))
        );
    }

    #[test]
    fn repository_discovery_rejects_excess_key_metadata() {
        let mut objects = Vec::<SourceObject>::new();
        let mut key_bytes = 0;
        let error = append_source_object(&mut objects, &mut key_bytes, "too-long", 1, 10, 3)
            .expect_err("key metadata must be bounded");

        assert!(
            matches!(error, StageError::Unsupported(reason) if reason.contains("key metadata"))
        );
        assert!(objects.is_empty());
    }

    #[test]
    fn json_control_objects_are_bounded_before_parsing() {
        assert!(
            ensure_json_object_size("sentinel.json", 1024, 1024).is_ok(),
            "object at the exact limit is accepted"
        );
        let error = ensure_json_object_size("sentinel.json", 1025, 1024)
            .expect_err("oversized sentinel must be rejected");
        assert!(
            matches!(error, StageError::Unsupported(reason) if reason.contains("sentinel.json") && reason.contains("1024"))
        );
    }

    #[tokio::test]
    async fn sqlite_select_and_persist_support_retry_state_without_for_update() {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("in-memory SQLite connects");
        let backend = db.get_database_backend();
        let schema = Schema::new(backend);
        for statement in [
            schema.create_table_from_entity(temps_entities::backups::Entity),
            schema.create_table_from_entity(temps_entities::cloud_backup_mirror_states::Entity),
            schema.create_table_from_entity(temps_entities::cloud_backup_mirror_cursors::Entity),
        ] {
            db.execute(backend.build(&statement))
                .await
                .expect("SQLite mirror table creates");
        }
        db.execute_unprepared("PRAGMA foreign_keys = OFF")
            .await
            .expect("SQLite fixture disables unrelated backup foreign keys");
        let now = chrono::Utc::now();
        temps_entities::backups::ActiveModel {
            id: Set(1),
            name: Set("sqlite-backup".to_owned()),
            backup_id: Set(Uuid::new_v4().to_string()),
            schedule_id: Set(None),
            backup_type: Set("full".to_owned()),
            state: Set("completed".to_owned()),
            started_at: Set(now),
            finished_at: Set(Some(now)),
            size_bytes: Set(Some(1)),
            file_count: Set(Some(1)),
            s3_source_id: Set(1),
            s3_location: Set("s3://sqlite/backup".to_owned()),
            error_message: Set(None),
            metadata: Set("malformed legacy metadata".to_owned()),
            checksum: Set(None),
            compression_type: Set("none".to_owned()),
            created_by: Set(1),
            expires_at: Set(None),
            tags: Set("[]".to_owned()),
            schedule_run_id: Set(None),
        }
        .insert(&db)
        .await
        .expect("SQLite backup inserts");
        super::persist_state(
            &db,
            1,
            Uuid::nil(),
            "retry",
            "transient",
            Some("temporary outage"),
        )
        .await
        .expect("SQLite mirror state persists without row locking");
        let first_retry =
            temps_entities::cloud_backup_mirror_states::Entity::find_by_id((1, Uuid::nil()))
                .one(&db)
                .await
                .expect("first SQLite retry reads")
                .expect("first SQLite retry exists");
        assert_eq!(first_retry.attempt_count, 1);
        let first_delay =
            first_retry.retry_after.expect("first retry is scheduled") - first_retry.updated_at;
        assert!(first_delay >= chrono::Duration::seconds(29));
        assert!(first_delay <= chrono::Duration::seconds(31));
        super::persist_state(
            &db,
            1,
            Uuid::nil(),
            "retry",
            "transient",
            Some("still unavailable"),
        )
        .await
        .expect("SQLite retry attempt survives and increments");
        let second_retry =
            temps_entities::cloud_backup_mirror_states::Entity::find_by_id((1, Uuid::nil()))
                .one(&db)
                .await
                .expect("second SQLite retry reads")
                .expect("second SQLite retry exists");
        assert_eq!(second_retry.attempt_count, 2);
        let second_delay =
            second_retry.retry_after.expect("second retry is scheduled") - second_retry.updated_at;
        assert!(second_delay >= chrono::Duration::seconds(59));
        assert!(second_delay <= chrono::Duration::seconds(61));
        let stored = temps_entities::backups::Entity::find_by_id(1)
            .one(&db)
            .await
            .expect("SQLite backup reads")
            .expect("SQLite backup exists");
        assert_eq!(stored.metadata, "malformed legacy metadata");
        let deferred = select_due_backups(&db, Uuid::nil(), 50)
            .await
            .expect("SQLite due query runs");
        assert!(deferred.backups.is_empty());

        let state =
            temps_entities::cloud_backup_mirror_states::Entity::find_by_id((1, Uuid::nil()))
                .one(&db)
                .await
                .expect("SQLite state reads")
                .expect("SQLite state exists");
        let mut due = state.into_active_model();
        due.retry_after = Set(Some(now - chrono::Duration::seconds(1)));
        due.update(&db).await.expect("SQLite retry becomes due");
        let selected = select_due_backups(&db, Uuid::nil(), 50)
            .await
            .expect("SQLite due retry selects");
        assert_eq!(selected.backups.len(), 1);
        assert_eq!(selected.backups[0].id, 1);

        super::persist_state(&db, 1, Uuid::nil(), "complete", "mirrored", None)
            .await
            .expect("successful mirror resets retry state");
        let completed =
            temps_entities::cloud_backup_mirror_states::Entity::find_by_id((1, Uuid::nil()))
                .one(&db)
                .await
                .expect("completed SQLite state reads")
                .expect("completed SQLite state exists");
        assert_eq!(completed.attempt_count, 0);
        assert!(completed.retry_after.is_none());
    }

    #[tokio::test]
    async fn indexed_state_selects_only_bounded_due_work_after_thousands_of_rows() {
        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(database) => database,
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                eprintln!("Docker unavailable; skipping indexed mirror-state query test: {error}");
                return;
            }
            Err(error) => panic!("failed to create mirror-state test database: {error}"),
        };
        let db = test_db.connection();
        let now = chrono::Utc::now();
        let user = temps_entities::users::ActiveModel {
            name: Set("Mirror Test".to_owned()),
            email: Set(format!("mirror-{}@example.invalid", Uuid::new_v4())),
            email_verified: Set(true),
            must_change_password: Set(false),
            mfa_enabled: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("test user inserts");
        let source = temps_entities::s3_sources::ActiveModel {
            name: Set("mirror-source".to_owned()),
            bucket_name: Set("mirror-test".to_owned()),
            region: Set("test-1".to_owned()),
            endpoint: Set(Some("http://127.0.0.1:1".to_owned())),
            bucket_path: Set(String::new()),
            access_key_id: Set("encrypted-test-key".to_owned()),
            secret_key: Set("encrypted-test-secret".to_owned()),
            force_path_style: Set(Some(true)),
            is_default: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("test S3 source inserts");

        let backup_rows = (0..2_005)
            .map(|index| temps_entities::backups::ActiveModel {
                name: Set(format!("backup-{index}")),
                backup_id: Set(Uuid::new_v4().to_string()),
                schedule_id: Set(None),
                backup_type: Set("full".to_owned()),
                state: Set("completed".to_owned()),
                started_at: Set(now),
                finished_at: Set(Some(now + chrono::Duration::seconds(i64::from(index)))),
                size_bytes: Set(Some(1)),
                file_count: Set(Some(1)),
                s3_source_id: Set(source.id),
                s3_location: Set(format!("s3://mirror-test/backup-{index}")),
                error_message: Set(None),
                metadata: Set(if index == 2_002 {
                    "malformed legacy metadata".to_owned()
                } else {
                    "{}".to_owned()
                }),
                checksum: Set(None),
                compression_type: Set("none".to_owned()),
                created_by: Set(user.id),
                expires_at: Set(None),
                tags: Set("[]".to_owned()),
                schedule_run_id: Set(None),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        temps_entities::backups::Entity::insert_many(backup_rows)
            .exec(db)
            .await
            .expect("bulk backups insert");

        let stored = temps_entities::backups::Entity::find()
            .order_by_asc(temps_entities::backups::Column::Id)
            .all(db)
            .await
            .expect("backups list");
        let tenant_id = Uuid::new_v4();
        let terminal = stored
            .iter()
            .take(2_000)
            .map(
                |backup| temps_entities::cloud_backup_mirror_states::ActiveModel {
                    backup_id: Set(backup.id),
                    tenant_id: Set(tenant_id),
                    schema_version: Set(i32::try_from(MIRROR_STATE_VERSION).unwrap_or(i32::MAX)),
                    outcome: Set("complete".to_owned()),
                    classification: Set("mirrored".to_owned()),
                    reason: Set(None),
                    attempt_count: Set(0),
                    retry_after: Set(None),
                    updated_at: Set(now),
                },
            )
            .collect::<Vec<_>>();
        temps_entities::cloud_backup_mirror_states::Entity::insert_many(terminal)
            .exec(db)
            .await
            .expect("terminal mirror states insert");

        temps_entities::cloud_backup_mirror_states::Entity::insert_many([
            temps_entities::cloud_backup_mirror_states::ActiveModel {
                backup_id: Set(stored[2_000].id),
                tenant_id: Set(tenant_id),
                schema_version: Set(i32::try_from(MIRROR_STATE_VERSION).unwrap_or(i32::MAX)),
                outcome: Set("retry".to_owned()),
                classification: Set("network".to_owned()),
                reason: Set(Some("temporary outage".to_owned())),
                attempt_count: Set(2),
                retry_after: Set(Some(now + chrono::Duration::hours(1))),
                updated_at: Set(now),
            },
            temps_entities::cloud_backup_mirror_states::ActiveModel {
                backup_id: Set(stored[2_001].id),
                tenant_id: Set(tenant_id),
                schema_version: Set(i32::try_from(MIRROR_STATE_VERSION).unwrap_or(i32::MAX)),
                outcome: Set("retry".to_owned()),
                classification: Set("network".to_owned()),
                reason: Set(Some("outage recovered".to_owned())),
                attempt_count: Set(2),
                retry_after: Set(Some(now - chrono::Duration::minutes(1))),
                updated_at: Set(now),
            },
            temps_entities::cloud_backup_mirror_states::ActiveModel {
                backup_id: Set(stored[2_003].id),
                tenant_id: Set(tenant_id),
                schema_version: Set(i32::try_from(MIRROR_STATE_VERSION).unwrap_or(i32::MAX)),
                outcome: Set("complete".to_owned()),
                classification: Set("mirrored".to_owned()),
                reason: Set(None),
                attempt_count: Set(0),
                retry_after: Set(None),
                updated_at: Set(now),
            },
        ])
        .exec(db)
        .await
        .expect("deferred and stale states insert");
        temps_entities::cloud_backup_mirror_cursors::ActiveModel {
            tenant_id: Set(tenant_id),
            last_finished_at: Set(stored[2_000]
                .finished_at
                .unwrap_or(stored[2_000].started_at)),
            last_backup_id: Set(stored[2_000].id),
            updated_at: Set(now),
        }
        .insert(db)
        .await
        .expect("discovery cursor inserts");

        let first_page = select_due_backups(db, tenant_id, 3)
            .await
            .expect("bounded due query");
        assert_eq!(first_page.backups.len(), 3);
        assert_eq!(first_page.backups[0].id, stored[2_001].id);
        let all_due = select_due_backups(db, tenant_id, 50)
            .await
            .expect("all due query");
        assert_eq!(all_due.backups.len(), 3);
        assert!(all_due
            .backups
            .iter()
            .any(|backup| backup.metadata == "malformed legacy metadata"));

        // A backup may be created long before it completes. Its lower ID must
        // not fall behind a newer cursor; completion time is the leading key.
        temps_entities::cloud_backup_mirror_states::Entity::delete_many()
            .filter(temps_entities::cloud_backup_mirror_states::Column::BackupId.eq(stored[100].id))
            .filter(temps_entities::cloud_backup_mirror_states::Column::TenantId.eq(tenant_id))
            .exec(db)
            .await
            .expect("old terminal state deletes for late-completion simulation");
        let mut late_completion = stored[100].clone().into_active_model();
        late_completion.finished_at = Set(Some(now + chrono::Duration::hours(2)));
        late_completion
            .update(db)
            .await
            .expect("old backup finishes after cursor advancement");
        let after_late_completion = select_due_backups(db, tenant_id, 50)
            .await
            .expect("late completion query");
        assert!(after_late_completion
            .backups
            .iter()
            .any(|backup| backup.id == stored[100].id));

        let transaction = db.begin().await.expect("query-plan transaction starts");
        transaction
            .execute(Statement::from_string(
                DatabaseBackend::Postgres,
                "SET LOCAL enable_seqscan = off".to_owned(),
            ))
            .await
            .expect("sequential scans disabled for index-shape assertion");
        let due_plan = explain_query_plan(
            &transaction,
            DUE_BACKUPS_SQL,
            [tenant_id.into(), 25_i64.into(), chrono::Utc::now().into()],
        )
        .await;
        assert!(
            due_plan.contains("idx_cloud_backup_mirror_states_due"),
            "due work must start from the partial retry index: {due_plan}"
        );
        assert!(!due_plan.contains("\"Node Type\":\"Seq Scan\""));
        let discovery_plan = explain_query_plan(
            &transaction,
            DISCOVER_BACKUPS_SQL,
            [
                stored[2_000]
                    .finished_at
                    .unwrap_or(stored[2_000].started_at)
                    .into(),
                stored[2_000].id.into(),
                tenant_id.into(),
                25_i64.into(),
            ],
        )
        .await;
        assert!(
            discovery_plan.contains("idx_backups_cloud_mirror_discovery"),
            "discovery must seek from the completion watermark: {discovery_plan}"
        );
        assert!(!discovery_plan.contains("\"Node Type\":\"Seq Scan\""));
        transaction
            .rollback()
            .await
            .expect("query-plan transaction rolls back");
    }

    async fn explain_query_plan<I>(
        connection: &impl ConnectionTrait,
        sql: &str,
        values: I,
    ) -> String
    where
        I: IntoIterator<Item = sea_orm::Value>,
    {
        let row = connection
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {sql}"),
                values,
            ))
            .await
            .expect("EXPLAIN query succeeds")
            .expect("EXPLAIN returns a plan");
        let plan: serde_json::Value = row
            .try_get("", "QUERY PLAN")
            .expect("PostgreSQL JSON plan decodes");
        serde_json::to_string(&plan).expect("query plan serializes")
    }

    #[tokio::test]
    async fn sweep_resources_batch_relational_lookups_for_many_candidates() {
        let now = chrono::Utc::now();
        let candidates = (1..=50)
            .map(|id| temps_entities::backups::Model {
                id,
                name: format!("backup-{id}"),
                backup_id: Uuid::new_v4().to_string(),
                schedule_id: None,
                backup_type: "full".to_owned(),
                state: "completed".to_owned(),
                started_at: now,
                finished_at: Some(now),
                size_bytes: Some(1),
                file_count: Some(1),
                s3_source_id: 7,
                s3_location: format!("s3://mirror/backup-{id}"),
                error_message: None,
                metadata: "{}".to_owned(),
                checksum: None,
                compression_type: "none".to_owned(),
                created_by: 1,
                expires_at: None,
                tags: "[]".to_owned(),
                schedule_run_id: None,
            })
            .collect::<Vec<_>>();
        let external_rows = candidates
            .iter()
            .map(|backup| temps_entities::external_service_backups::Model {
                id: backup.id,
                service_id: 9,
                backup_id: backup.id,
                backup_type: "full".to_owned(),
                state: "completed".to_owned(),
                started_at: now,
                finished_at: Some(now),
                size_bytes: Some(1),
                s3_location: backup.s3_location.clone(),
                error_message: None,
                metadata: serde_json::json!({}),
                checksum: None,
                compression_type: "none".to_owned(),
                created_by: 1,
                expires_at: None,
            })
            .collect::<Vec<_>>();
        let service = temps_entities::external_services::Model {
            id: 9,
            name: "postgres".to_owned(),
            service_type: "postgres".to_owned(),
            version: Some("17".to_owned()),
            status: "running".to_owned(),
            created_at: now,
            updated_at: now,
            slug: None,
            config: None,
            node_id: None,
            topology: "standalone".to_owned(),
            error_message: None,
            health_status: None,
            last_health_check_at: None,
            last_health_error: None,
            consecutive_health_failures: 0,
            health_metadata: None,
            metrics_enabled: false,
            default_backup_provisioned: false,
            container_name: None,
            ai_data_access: false,
        };
        let source = temps_entities::s3_sources::Model {
            id: 7,
            name: "mirror".to_owned(),
            bucket_name: "mirror".to_owned(),
            region: "test-1".to_owned(),
            endpoint: None,
            bucket_path: String::new(),
            access_key_id: "encrypted".to_owned(),
            secret_key: "encrypted".to_owned(),
            force_path_style: Some(true),
            is_default: false,
            managed_by_cloud: false,
            created_at: now,
            updated_at: now,
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([external_rows])
            .append_query_results([[service]])
            .append_query_results([[source]])
            .into_connection();
        let encryption = temps_core::EncryptionService::new_from_password("mirror-test");

        let resources = super::SweepResources::load(&db, &encryption, &candidates)
            .await
            .expect("batched resources load");
        assert_eq!(resources.external_by_backup.len(), 50);
        assert_eq!(resources.services.len(), 1);
        assert_eq!(resources.sources.len(), 1);
        drop(resources);
        let log = db.into_transaction_log();
        assert_eq!(
            log.len(),
            3,
            "candidate count must not increase relational query count"
        );
    }

    #[test]
    fn cloud_outages_use_bounded_exponential_backoff() {
        let first = next_sweep_interval(Duration::ZERO, SweepOutcome::Retry);
        let second = next_sweep_interval(first, SweepOutcome::Retry);
        let third = next_sweep_interval(second, SweepOutcome::Retry);

        assert_eq!(first, BASE_SWEEP_INTERVAL);
        assert_eq!(second, BASE_SWEEP_INTERVAL * 2);
        assert_eq!(third, BASE_SWEEP_INTERVAL * 4);

        let mut interval = third;
        for _ in 0..20 {
            interval = next_sweep_interval(interval, SweepOutcome::Retry);
        }
        assert_eq!(interval, MAX_SWEEP_INTERVAL);

        assert_eq!(
            super::mirror_retry_delay("transient", 1),
            chrono::Duration::seconds(30)
        );
        assert_eq!(
            super::mirror_retry_delay("transient", 2),
            chrono::Duration::seconds(60)
        );
        assert_eq!(
            super::mirror_retry_delay("transient", 6),
            chrono::Duration::minutes(15)
        );
        assert_eq!(
            super::mirror_retry_delay("transient", 100),
            chrono::Duration::minutes(15)
        );
        assert_eq!(
            super::mirror_retry_delay("unsupported", 1),
            chrono::Duration::minutes(15)
        );
    }

    #[test]
    fn mirror_progress_resets_backoff_immediately() {
        assert_eq!(
            next_sweep_interval(MAX_SWEEP_INTERVAL, SweepOutcome::Progress),
            BASE_SWEEP_INTERVAL
        );
        assert_eq!(
            next_sweep_interval(MAX_SWEEP_INTERVAL, SweepOutcome::Idle),
            BASE_SWEEP_INTERVAL
        );
    }

    #[test]
    fn parses_supported_postgres_version_shapes() {
        assert_eq!(parse_postgres_major(Some("17.6")), Some(17));
        assert_eq!(parse_postgres_major(Some("pg16")), Some(16));
        assert_eq!(parse_postgres_major(None), None);
    }

    #[test]
    fn derives_engine_version_from_managed_image_tags() {
        assert_eq!(
            image_tag_version("ghcr.io/gotempsh/mariadb-walg:11.4"),
            Some("11.4")
        );
        assert_eq!(
            image_tag_version("registry.example.com:5000/mongodb-walg:8.0"),
            Some("8.0")
        );
        assert_eq!(image_tag_version("rustfs/rustfs@sha256:abc"), None);
        assert_eq!(image_tag_version("mariadb"), None);
    }

    #[test]
    fn identifies_only_walg_repository_locations() {
        assert_eq!(
            walg_root_key("s3://bucket/team/postgres/walg"),
            Some("team/postgres/walg".into())
        );
        assert_eq!(walg_root_key("team/postgres/walg"), None);
        assert_eq!(walg_root_key("s3://bucket/backup.sql.gz"), None);
    }

    #[test]
    fn native_locations_are_bound_to_the_configured_bucket() {
        assert_eq!(
            s3_key("backups", "s3://backups/tenant/rustfs/snapshot").ok(),
            Some("tenant/rustfs/snapshot".into())
        );
        assert_eq!(
            s3_key("backups", "/tenant/mariadb/base.mbstream.gz").ok(),
            Some("tenant/mariadb/base.mbstream.gz".into())
        );
        assert!(s3_key("backups", "s3://other/tenant/backup").is_err());
        assert!(s3_key("backups", "").is_err());
    }

    #[test]
    fn object_store_service_aliases_use_native_snapshot_mirroring() {
        for service_type in ["rustfs", "s3", "minio", "blob"] {
            assert!(
                supports_native_mirror(service_type),
                "{service_type} must reach the object-set mirror contract"
            );
        }
    }

    #[test]
    fn sentinel_identity_is_found_inside_user_data() {
        let sentinel = serde_json::json!({
            "UserData": { "temps_backup_id": "backup-42" },
            "LSN": "0/2000000"
        });
        assert!(contains_backup_identity(&sentinel, "backup-42"));
        assert!(!contains_backup_identity(&sentinel, "backup-43"));
    }

    #[test]
    fn lsn_bounds_map_to_sortable_wal_segments() {
        assert_eq!(
            wal_segment_name("0/2000000", 1).ok().as_deref(),
            Some("000000010000000000000002")
        );
        assert_eq!(
            wal_segment_name("1/0", 2).ok().as_deref(),
            Some("000000020000000100000000")
        );
        assert!(wal_segment_name("not-an-lsn", 1).is_err());
    }

    #[test]
    fn real_walg_numeric_lsn_and_backup_timeline_are_supported() {
        let sentinel = serde_json::json!({
            "LSN": 33_554_432_u64,
            "FinishLsn": 50_331_648_u64,
            "SystemIdentifier": 7_420_000_000_000_000_000_u64,
        });
        assert_eq!(
            sentinel_lsn(&sentinel, &["LSN"]).as_deref(),
            Some("0/2000000")
        );
        assert_eq!(
            timeline_from_backup_name("base_000000030000000000000002"),
            Some(3)
        );
    }

    #[test]
    fn mirror_state_merge_preserves_concurrently_added_metadata() {
        let tenant_id = uuid::Uuid::new_v4();
        let current = serde_json::json!({
            "cloud_mirror": {"another-tenant": {"outcome": "complete"}},
            "local_completion": {"checksum": "newer-value"},
            "note": tenant_id.to_string()
        });
        let merged = merge_mirror_state(
            &current.to_string(),
            tenant_id,
            "retry",
            "unsupported",
            Some("future engine"),
            Some(chrono::Utc::now() + chrono::Duration::minutes(15)),
        )
        .expect("valid object metadata merges");

        assert_eq!(merged["local_completion"]["checksum"], "newer-value");
        assert_eq!(merged["note"], tenant_id.to_string());
        assert_eq!(
            merged["cloud_mirror"][tenant_id.to_string()]["schema_version"],
            MIRROR_STATE_VERSION
        );
        assert_eq!(
            merged["cloud_mirror"][tenant_id.to_string()]["outcome"],
            "retry"
        );
        assert!(merge_mirror_state(
            "malformed legacy metadata",
            tenant_id,
            "complete",
            "mirrored",
            None,
            None,
        )
        .is_none());
    }

    #[test]
    fn exact_mirror_state_handles_invalid_json_and_uuid_text_without_false_exclusion() {
        let tenant_id = uuid::Uuid::new_v4();
        assert!(deferred_legacy_state("not-json", tenant_id).is_none());
        assert!(deferred_legacy_state(
            &serde_json::json!({"note": tenant_id.to_string()}).to_string(),
            tenant_id
        )
        .is_none());
        assert!(deferred_legacy_state(
            &serde_json::json!({"cloud_mirror": {tenant_id.to_string(): {"outcome": "complete"}}})
                .to_string(),
            tenant_id
        )
        .is_some());
    }

    #[test]
    fn unsupported_retry_is_versioned_and_bounded() {
        let tenant_id = uuid::Uuid::new_v4();
        let delayed = merge_mirror_state(
            "{}",
            tenant_id,
            "retry",
            "unsupported",
            Some("future engine"),
            Some(chrono::Utc::now() + chrono::Duration::minutes(15)),
        )
        .expect("valid object metadata merges");
        assert!(deferred_legacy_state(&delayed.to_string(), tenant_id).is_some());
        let mut old = delayed;
        old["cloud_mirror"][tenant_id.to_string()]["schema_version"] = serde_json::json!(0);
        assert!(deferred_legacy_state(&old.to_string(), tenant_id).is_none());
    }

    struct MirrorFlowStub {
        origin: String,
        source_body: Vec<u8>,
        source_gets: AtomicUsize,
        source_failures_remaining: AtomicUsize,
        upload_attempts: AtomicUsize,
        simulate_commit_response_loss: bool,
        upload_committed: AtomicBool,
        completion_calls: AtomicUsize,
        completed: AtomicBool,
        expected_backup_id: Mutex<Option<Uuid>>,
        expected_instance_id: Mutex<Option<Uuid>>,
        expected_checksum: String,
    }

    async fn enroll_stub(
        State(state): State<Arc<MirrorFlowStub>>,
        Json(request): Json<serde_json::Value>,
    ) -> Json<serde_json::Value> {
        let instance_id = request["instance_id"]
            .as_str()
            .and_then(|value| Uuid::parse_str(value).ok())
            .expect("enrollment carries instance id");
        *state.expected_instance_id.lock().expect("instance id lock") = Some(instance_id);
        Json(serde_json::json!({
            "tenant_id": Uuid::new_v4(),
            "instance_token": "instance-test-token",
            "account_email": "backup-owner@example.invalid"
        }))
    }

    async fn source_object_stub(
        State(state): State<Arc<MirrorFlowStub>>,
        Path((bucket, key)): Path<(String, String)>,
    ) -> (StatusCode, HeaderMap, Body) {
        assert_eq!(bucket, "source-bucket");
        assert_eq!(key, "snapshots/object-0001.bin");
        state.source_gets.fetch_add(1, Ordering::SeqCst);
        if state
            .source_failures_remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                HeaderMap::new(),
                Body::empty(),
            );
        }
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_LENGTH,
            state.source_body.len().to_string().parse().expect("length"),
        );
        (
            StatusCode::OK,
            headers,
            Body::from(state.source_body.clone()),
        )
    }

    async fn native_target_stub(
        State(state): State<Arc<MirrorFlowStub>>,
        Json(request): Json<WalGObjectTargetRequest>,
    ) -> Json<serde_json::Value> {
        assert_eq!(
            Some(request.instance_id),
            *state.expected_instance_id.lock().expect("instance id lock")
        );
        let mut backup_id = state.expected_backup_id.lock().expect("backup id lock");
        if let Some(expected) = *backup_id {
            assert_eq!(
                request.backup_id, expected,
                "resume changed backup identity"
            );
        } else {
            *backup_id = Some(request.backup_id);
        }
        let upload_required = !state.completed.load(Ordering::SeqCst);
        Json(serde_json::json!({
            "backup_id": request.backup_id,
            "relative_key": request.relative_key,
            "upload_required": upload_required,
            "upload_url": format!("{}/object-upload", state.origin),
            "expires_at_millis": chrono::Utc::now().timestamp_millis() + 60_000,
            "headers": if upload_required {
                serde_json::json!({"content-length": state.source_body.len().to_string()})
            } else {
                serde_json::json!({})
            }
        }))
    }

    async fn object_upload_stub(
        State(state): State<Arc<MirrorFlowStub>>,
        body: Body,
    ) -> StatusCode {
        let attempt = state.upload_attempts.fetch_add(1, Ordering::SeqCst);
        if state.simulate_commit_response_loss {
            if state.upload_committed.load(Ordering::SeqCst) {
                return StatusCode::PRECONDITION_FAILED;
            }
            let body = match to_bytes(body, state.source_body.len() + 1).await {
                Ok(body) => body,
                Err(_) => return StatusCode::BAD_REQUEST,
            };
            if body.as_ref() != state.source_body.as_slice() {
                return StatusCode::UNPROCESSABLE_ENTITY;
            }
            state.upload_committed.store(true, Ordering::SeqCst);
            // The object is durable, but the client observes an uncertain
            // response and retries with its original immutable target.
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
        if attempt == 0 {
            // Fail before buffering the body. The production mirror must reopen
            // the S3 object for its retry; it cannot rewind or stage this stream.
            return StatusCode::SERVICE_UNAVAILABLE;
        }
        let body = match to_bytes(body, state.source_body.len() + 1).await {
            Ok(body) => body,
            Err(_) => return StatusCode::BAD_REQUEST,
        };
        if body.as_ref() == state.source_body.as_slice() {
            StatusCode::OK
        } else {
            StatusCode::UNPROCESSABLE_ENTITY
        }
    }

    async fn native_complete_stub(
        State(state): State<Arc<MirrorFlowStub>>,
        Json(completion): Json<WalGObjectCompleted>,
    ) -> (StatusCode, Json<serde_json::Value>) {
        assert_eq!(
            Some(completion.backup_id),
            *state.expected_backup_id.lock().expect("backup id lock")
        );
        assert_eq!(completion.relative_key, "object-0001.bin");
        assert_eq!(completion.bytes, state.source_body.len() as u64);
        assert_eq!(completion.checksum_sha256, state.expected_checksum);
        state.completion_calls.fetch_add(1, Ordering::SeqCst);
        state.completed.store(true, Ordering::SeqCst);
        (
            StatusCode::OK,
            Json(serde_json::json!({"state": "complete"})),
        )
    }

    fn source_client(origin: &str) -> S3Client {
        let credentials = aws_sdk_s3::config::Credentials::new(
            "test-access-key",
            "test-secret-key",
            None,
            None,
            "mirror-flow-test",
        );
        S3Client::from_conf(
            Config::builder()
                .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
                .region(aws_sdk_s3::config::Region::new("test-region-1"))
                .force_path_style(true)
                .credentials_provider(credentials)
                .retry_config(aws_sdk_s3::config::retry::RetryConfig::disabled())
                .endpoint_url(origin)
                .build(),
        )
    }

    fn expect_stage_ok<T>(result: Result<T, StageError>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(StageError::Unsupported(reason)) => {
                panic!("{context}: unexpectedly unsupported: {reason}")
            }
            Err(StageError::Retry(reason)) => panic!("{context}: retryable failure: {reason}"),
        }
    }

    #[tokio::test]
    async fn test_native_backup_mirror_streams_s3_source_and_resumes_at_object_boundary() {
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("sandbox denied TCP bind; skipping production mirror flow test");
                return;
            }
            Err(error) => panic!("bind mirror flow stub: {error}"),
        };
        let address = listener.local_addr().expect("mirror flow stub address");
        // Larger than inspect_source_object's 1 MiB buffer. The checksum pass
        // therefore exercises bounded reads, while upload retries reopen the
        // source instead of retaining or rewinding an in-memory body.
        let source_body = vec![0x5a; 3 * 1024 * 1024 + 17];
        let expected_checksum = Sha256::digest(&source_body).iter().fold(
            String::with_capacity(64),
            |mut output, byte| {
                use std::fmt::Write;
                write!(output, "{byte:02x}").expect("write checksum");
                output
            },
        );
        let state = Arc::new(MirrorFlowStub {
            origin: format!("http://{address}"),
            source_body,
            source_gets: AtomicUsize::new(0),
            source_failures_remaining: AtomicUsize::new(0),
            upload_attempts: AtomicUsize::new(0),
            simulate_commit_response_loss: false,
            upload_committed: AtomicBool::new(false),
            completion_calls: AtomicUsize::new(0),
            completed: AtomicBool::new(false),
            expected_backup_id: Mutex::new(None),
            expected_instance_id: Mutex::new(None),
            expected_checksum,
        });
        let app = Router::new()
            .route("/v1/enroll", post(enroll_stub))
            .route(
                "/v1/backups/native/objects/target",
                post(native_target_stub),
            )
            .route(
                "/v1/backups/native/objects/complete",
                post(native_complete_stub),
            )
            .route("/object-upload", put(object_upload_stub))
            .route("/{bucket}/{*key}", get(source_object_stub))
            .with_state(state.clone());
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve mirror flow stub");
        });

        let temp = tempfile::tempdir().expect("cloud-link state dir");
        let link =
            CloudLink::load_for_loopback_development(temp.path().to_path_buf(), "test-agent");
        link.configure(
            BackendUrl::loopback_development(&state.origin).expect("loopback Cloud URL"),
        )
        .expect("configure Cloud link");
        link.set_feature_switches(CloudFeatureSwitches {
            telemetry: false,
            backups: true,
            notifications: false,
        })
        .expect("enable backup mirroring");
        link.enroll("TEST-CODE").await.expect("enroll Cloud link");
        let instance_id = link.instance_id().expect("linked instance id");
        let backup_id = Uuid::new_v4();
        let s3 = source_client(&state.origin);
        let source_key = "snapshots/object-0001.bin";
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("checksum-cache SQLite connects");
        let encryption = temps_core::EncryptionService::new_from_password("mirror-cache-test");
        let mut clients = HashMap::new();
        clients.insert(7, s3.clone());
        let mut resources = super::SweepResources {
            db: &db,
            encryption: &encryption,
            external_by_backup: HashMap::new(),
            services: HashMap::new(),
            sources: HashMap::new(),
            clients,
            object_inspections: HashMap::new(),
            control_plane_postgres_major: None,
        };
        let (bytes, checksum_sha256) = expect_stage_ok(
            resources
                .inspect_object(7, "source-bucket", source_key)
                .await,
            "stream source checksum",
        );
        let cached_inspection = expect_stage_ok(
            resources
                .inspect_object(7, "source-bucket", source_key)
                .await,
            "reuse source checksum",
        );
        assert_eq!(cached_inspection, (bytes, checksum_sha256.clone()));
        assert_eq!(state.source_gets.load(Ordering::SeqCst), 1);
        let declaration = NativeSnapshotObjectDeclaration {
            relative_key: "object-0001.bin".into(),
            kind: NativeSnapshotObjectKind::Object,
            bytes,
            checksum_sha256,
        };

        expect_stage_ok(
            upload_native_object(
                &link,
                &s3,
                "source-bucket",
                "snapshots",
                instance_id,
                backup_id,
                declaration.clone(),
            )
            .await,
            "mirror recovers from interrupted object upload",
        );
        expect_stage_ok(
            upload_native_object(
                &link,
                &s3,
                "source-bucket",
                "snapshots",
                instance_id,
                backup_id,
                declaration,
            )
            .await,
            "completed object resumes idempotently without another upload",
        );

        assert_eq!(bytes, state.source_body.len() as u64);
        assert_eq!(state.upload_attempts.load(Ordering::SeqCst), 2);
        assert_eq!(
            state.source_gets.load(Ordering::SeqCst),
            3,
            "one bounded checksum pass plus two upload streams; resumed object is not reopened"
        );
        assert_eq!(
            state.completion_calls.load(Ordering::SeqCst),
            2,
            "completion is idempotently replayed after Cloud reports the object already present"
        );
    }

    #[tokio::test]
    async fn native_upload_retries_source_get_and_accepts_precondition_after_lost_response() {
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("sandbox denied TCP bind; skipping upload idempotency test");
                return;
            }
            Err(error) => panic!("bind mirror idempotency stub: {error}"),
        };
        let address = listener.local_addr().expect("mirror stub address");
        let source_body = b"immutable backup object".to_vec();
        let expected_checksum = Sha256::digest(&source_body).iter().fold(
            String::with_capacity(64),
            |mut output, byte| {
                use std::fmt::Write;
                write!(output, "{byte:02x}").expect("write checksum");
                output
            },
        );
        let state = Arc::new(MirrorFlowStub {
            origin: format!("http://{address}"),
            source_body: source_body.clone(),
            source_gets: AtomicUsize::new(0),
            source_failures_remaining: AtomicUsize::new(0),
            upload_attempts: AtomicUsize::new(0),
            simulate_commit_response_loss: true,
            upload_committed: AtomicBool::new(false),
            completion_calls: AtomicUsize::new(0),
            completed: AtomicBool::new(false),
            expected_backup_id: Mutex::new(None),
            expected_instance_id: Mutex::new(None),
            expected_checksum: expected_checksum.clone(),
        });
        let app = Router::new()
            .route("/v1/enroll", post(enroll_stub))
            .route(
                "/v1/backups/native/objects/target",
                post(native_target_stub),
            )
            .route(
                "/v1/backups/native/objects/complete",
                post(native_complete_stub),
            )
            .route("/object-upload", put(object_upload_stub))
            .route("/{bucket}/{*key}", get(source_object_stub))
            .with_state(state.clone());
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve mirror idempotency stub");
        });

        let temp = tempfile::tempdir().expect("cloud-link state dir");
        let link =
            CloudLink::load_for_loopback_development(temp.path().to_path_buf(), "test-agent");
        link.configure(
            BackendUrl::loopback_development(&state.origin).expect("loopback Cloud URL"),
        )
        .expect("configure Cloud link");
        link.set_feature_switches(CloudFeatureSwitches {
            telemetry: false,
            backups: true,
            notifications: false,
        })
        .expect("enable backup mirroring");
        link.enroll("TEST-CODE").await.expect("enroll Cloud link");
        let instance_id = link.instance_id().expect("linked instance id");
        let backup_id = Uuid::new_v4();
        let s3 = source_client(&state.origin);

        // Exercise the outer source-GET retry independently of SDK retries.
        state.source_failures_remaining.store(1, Ordering::SeqCst);
        expect_stage_ok(
            upload_native_object(
                &link,
                &s3,
                "source-bucket",
                "snapshots",
                instance_id,
                backup_id,
                NativeSnapshotObjectDeclaration {
                    relative_key: "object-0001.bin".into(),
                    kind: NativeSnapshotObjectKind::Object,
                    bytes: source_body.len() as u64,
                    checksum_sha256: expected_checksum,
                },
            )
            .await,
            "lost upload response resolves through completion verification",
        );

        assert_eq!(state.source_gets.load(Ordering::SeqCst), 3);
        assert_eq!(state.upload_attempts.load(Ordering::SeqCst), 2);
        assert!(state.upload_committed.load(Ordering::SeqCst));
        assert_eq!(state.completion_calls.load(Ordering::SeqCst), 1);
    }
}
