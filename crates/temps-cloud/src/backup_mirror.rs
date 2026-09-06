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
/// Both queries below are scoped to `s3_sources.managed_by_cloud`. A backup
/// already written to an operator-configured, non-managed S3 source (their
/// own bucket, their own credentials) is offsite by the operator's own
/// choice; mirroring it into Temps Cloud storage too would silently double
/// the upload/storage cost and, if the managed backend is degraded, retry
/// forever against a destination the operator never asked this backup to
/// use. Only backups written through the Cloud-provisioned source
/// (`managed_by_cloud = true`) are mirrored.
const DUE_BACKUPS_SQL: &str = r#"
SELECT b.*
FROM cloud_backup_mirror_states AS mirror
JOIN backups AS b ON b.id = mirror.backup_id
JOIN s3_sources AS s ON s.id = b.s3_source_id
WHERE mirror.tenant_id = $1
  AND mirror.outcome <> 'complete'
  AND mirror.retry_after <= $3
  AND b.state = 'completed'
  AND s.managed_by_cloud
ORDER BY mirror.retry_after ASC, mirror.backup_id ASC
LIMIT $2
"#;
const DISCOVER_BACKUPS_SQL: &str = r#"
SELECT b.*
FROM backups AS b
JOIN s3_sources AS s ON s.id = b.s3_source_id
WHERE b.state = 'completed'
  AND s.managed_by_cloud
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
        SweepOutcome::Idle | SweepOutcome::Progress => BASE_SWEEP_INTERVAL,
        // `NotLinked` backs off the same way an outage does instead of jumping
        // straight to the ceiling. The mirror starts with the server, and being
        // unlinked is the normal state for the first seconds of every boot and
        // for the whole of the enrollment flow — an operator who links the
        // instance a minute after it started must not then wait fifteen minutes
        // for their first backup to appear in Cloud, with nothing logged in
        // between to say why. The `NotLinked` path does no network or database
        // work at all (three atomic loads), so ticking sooner costs nothing, and
        // an instance that is never linked still settles at the same ceiling.
        SweepOutcome::NotLinked | SweepOutcome::Retry if current.is_zero() => BASE_SWEEP_INTERVAL,
        SweepOutcome::NotLinked | SweepOutcome::Retry => (current * 2).min(MAX_SWEEP_INTERVAL),
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
                // Every tick is logged, including the ones that decide there is
                // nothing to do. A sweep that short-circuits silently is
                // indistinguishable from a task that stopped ticking, and
                // telling those two apart from the outside costs hours.
                tracing::debug!(
                    slept_secs = retry_in.as_secs(),
                    "Cloud backup mirror sweep tick starting"
                );
                let outcome = match sweep(&link, &db, &encryption).await {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        warn!(error = %error, "Cloud backup mirror sweep failed; local backups remain authoritative");
                        SweepOutcome::Retry
                    }
                };
                retry_in = next_sweep_interval(retry_in, outcome);
                tracing::debug!(
                    outcome = ?outcome,
                    next_tick_secs = retry_in.as_secs(),
                    "Cloud backup mirror sweep tick finished"
                );
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
        tracing::debug!(
            reason = "this instance is not linked to Cloud",
            "Cloud backup mirror sweep has nothing to do"
        );
        return Ok(SweepOutcome::NotLinked);
    }
    // Deliberately not `backups_enabled()` on its own. Enrollment provisions
    // the Cloud-managed `s3_sources` row without asking that switch, a
    // background loop keeps its credential fresh, and on an instance with no
    // other S3 source that row becomes the *default* backup destination — so
    // backup bytes reach Cloud's bucket whether or not export consent is on.
    // Gating declaration on the switch alone therefore blocks no data transfer
    // at all; it only strands those objects in Cloud storage with no Cloud
    // record pointing at them, invisible in the dashboard and unusable for
    // restore. `backup_registration_permitted()` also lets the mirror stay idle
    // (and cheap) on the common case: linked, export off, nothing managed.
    if !link.backup_registration_permitted() {
        tracing::debug!(
            reason = "Cloud backup export is off and this instance has no Cloud-managed backup destination",
            "Cloud backup mirror sweep has nothing to do"
        );
        return Ok(SweepOutcome::NotLinked);
    }
    let (Some(tenant_id), Some(instance_id)) = (link.tenant_id(), link.instance_id()) else {
        tracing::debug!(
            reason = "the Cloud link has no tenant or instance identity yet",
            "Cloud backup mirror sweep has nothing to do"
        );
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

    if !link.backups_enabled() {
        // Say this out loud rather than mirroring quietly: the operator's
        // settings page shows Cloud backup export as off while their backups
        // are being written to, and now recorded in, Temps Cloud storage. They
        // cannot act on a state nobody reports.
        warn!(
            candidate_count = candidates.len(),
            "Cloud backup export is switched off, but this instance has a Cloud-managed backup \
             destination and completed backups already written to it. Registering them with Cloud \
             so they stay listed and restorable — turn the export switch on in Cloud settings to \
             make this intentional, or disconnect from Cloud to stop using the managed destination."
        );
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
                // Deliberately does NOT set `retry_required`. `Unsupported`
                // means this specific backup already has its own long
                // `retry_after` window from `mirror_retry_delay`, so it is
                // excluded from `DUE_BACKUPS_SQL` on its own — it needs no
                // help from the outer loop. Treating it the same as
                // `StageError::Retry` here doubled the backoff: one
                // permanently-unsupported backup (e.g. an empty RustFS
                // mirror, or an old WAL-G repository missing the
                // `temps_backup_id` tag) would ratchet the *whole* sweep's
                // cadence up to `MAX_SWEEP_INTERVAL` and hold it there
                // indefinitely, delaying discovery of every other backup on
                // the instance — including brand new, healthy ones — by up
                // to 15 minutes each tick, with no way to recover short of
                // clearing the offending backup.
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
        // Postgres/Timescale falls back from `postgres_walg` to `postgres_pgdump`
        // whenever the container doesn't have wal-g in it (`container_has_walg`
        // in temps-backup's engine dispatch, e.g. a custom image supplied via
        // `TEMPS_ALLOWED_POSTGRES_DOCKER_IMAGES`). A pg_dump-produced backup has
        // no WAL-G sentinel and never will, so routing it into
        // `mirror_walg_backup` doesn't just fail once — it retries forever
        // against a gate it can structurally never satisfy. `backups.metadata`
        // records which engine actually ran (set at row-creation time in
        // temps-backup's `services/backup.rs`), so check that before committing
        // to a mirror path.
        "postgres" | "postgresql" | "timescale" | "timescaledb"
            if backup_engine_key(&backup.metadata).as_deref() == Some("postgres_pgdump") =>
        {
            mirror_native_backup(link, resources, backup, &external, &service, instance_id).await
        }
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

/// The engine key `temps-backup` recorded on `backups.metadata` when it
/// created this row (`{"engine": "postgres_walg" | "postgres_pgdump" | ...}`).
/// Malformed or pre-dispatch-key legacy metadata yields `None`, which callers
/// treat as "assume WAL-G" — the only engine that existed before Postgres
/// backups could fall back to pg_dump.
fn backup_engine_key(metadata: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(metadata)
        .ok()?
        .get("engine")?
        .as_str()
        .map(str::to_owned)
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
        "postgres" | "postgresql" | "timescale" | "timescaledb" => {
            // pg_dump writes exactly two objects, siblings under one date/uuid
            // directory: the dump itself (`location_key`, already the exact
            // object key `postgres_pgdump.rs` uploaded via
            // `build_external_service_s3_key`) and a `metadata.json` sidecar
            // next to it (`v2_common::derive_metadata_key`: same parent,
            // filename replaced). Unlike the RustFS arm below, the sidecar is
            // included and declared with `Metadata` kind (auto-detected by the
            // `.../metadata.json` suffix check further down) — pg_dump's
            // restore needs it, the same way MariaDB's physical snapshot needs
            // its own metadata sidecar for binlog position.
            let root = location_key
                .rsplit_once('/')
                .map(|(parent, _)| parent.to_string())
                .ok_or_else(|| {
                    StageError::Unsupported(format!(
                        "pg_dump snapshot location {location_key} has no parent directory"
                    ))
                })?;
            let metadata_key = format!("{root}/metadata.json");
            let all = resources
                .list_repository(backup.s3_source_id, &source_config.bucket_name, &root)
                .await?;
            let selected = all
                .iter()
                .filter(|object| object.key == location_key || object.key == metadata_key)
                .cloned()
                .collect::<Vec<_>>();
            if selected.len() < 2 || !selected.iter().any(|object| object.key == metadata_key) {
                return Err(StageError::Retry(format!(
                    "pg_dump snapshot {location_key} is incomplete or lacks {metadata_key}"
                )));
            }
            let engine = match service_type.as_str() {
                "postgres" | "postgresql" => BackupEngine::Postgres,
                _ => BackupEngine::TimescaleDb,
            };
            (
                root,
                selected,
                engine,
                BackupFormat::PgDumpPlain,
                BackupCompression::Gzip,
                NativeSnapshotIdentity::ObjectSet {
                    snapshot_name: backup.backup_id.clone(),
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
                // Not transient: `root` is this specific backup's frozen,
                // backup_id-keyed destination prefix — the `mc mirror` run
                // that wrote (or didn't write) to it already finished, and
                // nothing will ever appear under this exact prefix later.
                // A source that was empty when the backup ran produces a
                // permanently empty manifest, not a manifest that will
                // eventually show up. `StageError::Retry` here meant this
                // condition — which can never resolve — retried forever at
                // the sweep's maximum interval, never declaring anything to
                // Cloud and never freeing the backup from the retry queue.
                return Err(StageError::Unsupported(format!(
                    "RustFS snapshot {root} has no objects; the source was empty when this backup ran, so it can never be mirrored to Cloud"
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
                || wal_segment_of(&object.key, &wal_prefix).is_some_and(|segment| {
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
    // `backups.state = 'completed'` only means `wal-g backup-push` exited 0,
    // which confirms the base tar files and the sentinel. WAL archiving is a
    // separate, asynchronous process — PostgreSQL's `archive_command` ships
    // segments on its own schedule — so at the moment this sweep runs, the
    // segment covering the backup's own finish LSN may not have reached the
    // repository yet. Declaring the manifest anyway would mirror a base backup
    // that cannot replay far enough to reach a consistent recovery point, then
    // mark it mirrored: a backup that only fails when someone tries to restore
    // it. Refuse until the range the sentinel names is actually there. The
    // reason is recorded on the mirror state, so an operator sees "waiting for
    // WAL archiving" rather than nothing, and it clears itself on a later
    // sweep the moment the segment lands.
    let archived = selected
        .iter()
        .filter_map(|object| wal_segment_of(&object.key, &wal_prefix))
        .collect::<std::collections::BTreeSet<_>>();
    for (segment, bound) in [(&first_wal, "start"), (&last_wal, "finish")] {
        if !archived.contains(segment.as_str()) {
            return Err(StageError::Retry(format!(
                "WAL-G snapshot {backup_name} is not ready to mirror: WAL segment {segment}, \
                 which covers its {bound} LSN, has not been archived to {wal_prefix} yet. \
                 Mirroring it now would store a base backup that cannot replay to a consistent \
                 point. Retrying; if this persists, check that PostgreSQL's archive_command is \
                 succeeding for this service."
            )));
        }
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
    // The declared manifest is what Cloud binds the snapshot to, and a retry
    // that computes a different one is rejected. Log the identity and the shape
    // of every attempt so a mismatch between attempts is visible here rather
    // than only inferable from Cloud's rejection message.
    tracing::debug!(
        local_backup_id = %backup.backup_id,
        cloud_backup_id = %cloud_backup_id,
        backup_name = %request.backup_name,
        timeline = request.timeline,
        object_count = declarations.len(),
        total_bytes = declarations.iter().map(|object| object.bytes).sum::<u64>(),
        // Count and size cannot distinguish two different manifests; this can.
        manifest_digest = %manifest_digest(&declarations),
        "Cloud backup mirror declaring WAL-G snapshot"
    );
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
/// The 24-character WAL segment name a `wal_005/` object carries, or `None`
/// when the object is not a segment.
///
/// The compression suffix is deliberately dropped: WAL-G names segments
/// `<timeline><logid><segment>` followed by whatever it compressed with
/// (`.lz4`, `.br`, `.zst`), and the same segment can legitimately reappear
/// under a different suffix. Requiring 24 hexadecimal characters is what keeps
/// the repository's other bookkeeping out of a range comparison it has no
/// business being in — a `.history` file is 8 characters and would otherwise be
/// compared as a whole key against segment names.
fn wal_segment_of<'a>(key: &'a str, wal_prefix: &str) -> Option<&'a str> {
    let segment = key.strip_prefix(wal_prefix)?.get(..24)?;
    segment
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit())
        .then_some(segment)
}

/// A stable fingerprint of the exact manifest being declared.
///
/// Cloud binds a `backup_id` to its manifest, so the question during any
/// mirroring incident is "did two attempts send the same objects?". Object
/// count and total size cannot answer it — two different manifests can share
/// both — and logging every key would flood the log for a large repository.
/// This digest answers it in one field: equal digests mean byte-identical
/// manifests.
fn manifest_digest(declarations: &[WalGObjectDeclaration]) -> String {
    let mut ordered = declarations.iter().collect::<Vec<_>>();
    ordered.sort_unstable_by(|left, right| left.relative_key.cmp(&right.relative_key));
    let mut hasher = Sha256::new();
    for object in ordered {
        hasher.update(object.relative_key.as_bytes());
        hasher.update([0]);
        hasher.update(object.bytes.to_le_bytes());
        hasher.update(object.checksum_sha256.as_bytes());
        hasher.update([0]);
    }
    hasher
        .finalize()
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

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
        // `external_services.version` is optional and is left empty by the
        // provisioning paths that pin a version through the image tag instead
        // (`gotempsh/postgres-walg:18-...`). Reading the column raw made every
        // such service permanently unmirrorable, even though the native-snapshot
        // path in this same file has always derived the version from the image.
        // Use the one resolver for both.
        let version = service_engine_version(resources.encryption, &service);
        let major = parse_postgres_major(Some(&version)).ok_or_else(|| {
            StageError::Unsupported(format!(
                "service {} reports PostgreSQL version {version}, which is not a supported major version",
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
        append_source_object, backup_engine_key, contains_backup_identity, deferred_legacy_state,
        ensure_json_object_size, image_tag_version, manifest_digest, merge_mirror_state,
        mirror_backup, next_sweep_interval, parse_postgres_major, run, s3_key, select_due_backups,
        sentinel_lsn, supports_native_mirror, sweep, timeline_from_backup_name,
        upload_native_object, wal_segment_name, wal_segment_of, walg_root_key, SourceObject,
        StageError, SweepOutcome, BASE_SWEEP_INTERVAL, DISCOVER_BACKUPS_SQL, DUE_BACKUPS_SQL,
        MAX_SWEEP_INTERVAL, MIRROR_STATE_VERSION,
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
            schema.create_table_from_entity(temps_entities::s3_sources::Entity),
        ] {
            db.execute(backend.build(&statement))
                .await
                .expect("SQLite mirror table creates");
        }
        db.execute_unprepared("PRAGMA foreign_keys = OFF")
            .await
            .expect("SQLite fixture disables unrelated backup foreign keys");
        let now = chrono::Utc::now();
        temps_entities::s3_sources::ActiveModel {
            id: Set(1),
            name: Set("sqlite-source".to_owned()),
            bucket_name: Set("sqlite-mirror".to_owned()),
            region: Set("test-1".to_owned()),
            endpoint: Set(None),
            bucket_path: Set(String::new()),
            access_key_id: Set("encrypted-test-key".to_owned()),
            secret_key: Set("encrypted-test-secret".to_owned()),
            session_token: Set(None),
            credentials_expire_at: Set(None),
            force_path_style: Set(Some(true)),
            is_default: Set(false),
            // Only Cloud-managed sources are mirror candidates.
            managed_by_cloud: Set(true),
            lifecycle_reconcile_failed_at: Set(None),
            lifecycle_reconcile_generation: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&db)
        .await
        .expect("SQLite S3 source inserts");
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
    async fn discovery_never_selects_backups_from_an_operator_owned_s3_source() {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("in-memory SQLite connects");
        let backend = db.get_database_backend();
        let schema = Schema::new(backend);
        for statement in [
            schema.create_table_from_entity(temps_entities::backups::Entity),
            schema.create_table_from_entity(temps_entities::cloud_backup_mirror_states::Entity),
            schema.create_table_from_entity(temps_entities::cloud_backup_mirror_cursors::Entity),
            schema.create_table_from_entity(temps_entities::s3_sources::Entity),
        ] {
            db.execute(backend.build(&statement))
                .await
                .expect("SQLite mirror table creates");
        }
        db.execute_unprepared("PRAGMA foreign_keys = OFF")
            .await
            .expect("SQLite fixture disables unrelated backup foreign keys");
        let now = chrono::Utc::now();

        let make_source =
            |id: i32, managed_by_cloud: bool| temps_entities::s3_sources::ActiveModel {
                id: Set(id),
                name: Set(format!("source-{id}")),
                bucket_name: Set(format!("bucket-{id}")),
                region: Set("test-1".to_owned()),
                endpoint: Set(None),
                bucket_path: Set(String::new()),
                access_key_id: Set("encrypted".to_owned()),
                secret_key: Set("encrypted".to_owned()),
                session_token: Set(None),
                credentials_expire_at: Set(None),
                force_path_style: Set(Some(true)),
                is_default: Set(false),
                managed_by_cloud: Set(managed_by_cloud),
                lifecycle_reconcile_failed_at: Set(None),
                lifecycle_reconcile_generation: Set(0),
                created_at: Set(now),
                updated_at: Set(now),
            };
        // Source 1: the operator's own bucket, their own credentials -- never
        // a mirror candidate. Source 2: provisioned by the Cloud link.
        make_source(1, false)
            .insert(&db)
            .await
            .expect("operator-owned source inserts");
        make_source(2, true)
            .insert(&db)
            .await
            .expect("Cloud-managed source inserts");

        let make_backup = |id: i32, s3_source_id: i32| temps_entities::backups::ActiveModel {
            id: Set(id),
            name: Set(format!("backup-{id}")),
            backup_id: Set(Uuid::new_v4().to_string()),
            schedule_id: Set(None),
            backup_type: Set("full".to_owned()),
            state: Set("completed".to_owned()),
            started_at: Set(now),
            finished_at: Set(Some(now + chrono::Duration::seconds(i64::from(id)))),
            size_bytes: Set(Some(1)),
            file_count: Set(Some(1)),
            s3_source_id: Set(s3_source_id),
            s3_location: Set(format!("s3://bucket-{s3_source_id}/backup-{id}")),
            error_message: Set(None),
            metadata: Set("{}".to_owned()),
            checksum: Set(None),
            compression_type: Set("none".to_owned()),
            created_by: Set(1),
            expires_at: Set(None),
            tags: Set("[]".to_owned()),
            schedule_run_id: Set(None),
        };
        // Backup 1 went to the operator's own bucket; backup 2 went to the
        // Cloud-managed one. Only backup 2 may ever become a mirror candidate.
        make_backup(1, 1)
            .insert(&db)
            .await
            .expect("operator-bucket backup inserts");
        make_backup(2, 2)
            .insert(&db)
            .await
            .expect("Cloud-managed-bucket backup inserts");

        let selection = select_due_backups(&db, Uuid::nil(), 50)
            .await
            .expect("SQLite discovery query runs");
        assert_eq!(
            selection.backups.len(),
            1,
            "only the backup on the Cloud-managed source may be discovered: {:?}",
            selection.backups
        );
        assert_eq!(selection.backups[0].id, 2);
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
            // Only Cloud-managed sources are mirror candidates; this test
            // exercises the mirror discovery/due queries themselves, so its
            // fixture backups must actually qualify.
            managed_by_cloud: Set(true),
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
            created_by_user_id: None,
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
            session_token: None,
            credentials_expire_at: None,
            force_path_style: Some(true),
            is_default: false,
            managed_by_cloud: false,
            lifecycle_reconcile_failed_at: None,
            lifecycle_reconcile_generation: 0,
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
    fn an_unlinked_instance_rechecks_soon_instead_of_sleeping_out_the_ceiling() {
        // The mirror starts with the server, so its first tick almost always
        // lands before enrollment finishes. Jumping straight to the ceiling
        // there meant an operator who linked their instance a minute after boot
        // saw nothing happen — and nothing logged — for a quarter of an hour.
        let first = next_sweep_interval(Duration::ZERO, SweepOutcome::NotLinked);
        assert_eq!(first, BASE_SWEEP_INTERVAL);
        assert_eq!(
            next_sweep_interval(first, SweepOutcome::NotLinked),
            BASE_SWEEP_INTERVAL * 2
        );

        // An instance that is never linked still settles at the outage ceiling
        // rather than polling forever at the healthy cadence.
        let mut interval = first;
        for _ in 0..20 {
            interval = next_sweep_interval(interval, SweepOutcome::NotLinked);
        }
        assert_eq!(interval, MAX_SWEEP_INTERVAL);
    }

    /// A linked [`CloudLink`] built without a single network round-trip, by
    /// writing the state file `CloudLink::load` reads on startup. Enrolling
    /// against an HTTP stub would drag real socket I/O into tests that need a
    /// deterministic clock.
    ///
    /// `base_url` is a closed loopback port on purpose: nothing in these tests
    /// may reach Cloud, and anything that tries fails immediately instead of
    /// waiting out a connect timeout.
    fn linked_link_fixture(temp: &tempfile::TempDir) -> Arc<CloudLink> {
        let state_dir = temp.path().join("cloud-link");
        std::fs::create_dir_all(&state_dir).expect("cloud-link state dir");
        std::fs::write(
            state_dir.join("state.json"),
            serde_json::json!({
                "instance_id": Uuid::new_v4(),
                "base_url": "http://127.0.0.1:1",
                "allow_loopback_development": true,
                "token": "test-instance-token",
                "tenant_id": Uuid::new_v4(),
                "account_email": "backup-owner@example.invalid",
            })
            .to_string(),
        )
        .expect("write Cloud link state");
        Arc::new(CloudLink::load_for_loopback_development(
            temp.path().to_path_buf(),
            "test-agent",
        ))
    }

    /// One completed backup sitting in a Cloud-managed bucket — the shape an
    /// instance is left in after enrollment provisions `managed_by_cloud` and a
    /// scheduled backup runs against it.
    ///
    /// `s3_location` is deliberately not a WAL-G repository, so `mirror_backup`
    /// reaches a terminal decision without any S3 or Cloud call. The assertion
    /// under test is that the backup was *processed at all*, not how it ended.
    async fn managed_backup_fixture_db() -> sea_orm::DatabaseConnection {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("in-memory SQLite connects");
        let backend = db.get_database_backend();
        let schema = Schema::new(backend);
        for statement in [
            schema.create_table_from_entity(temps_entities::backups::Entity),
            schema.create_table_from_entity(temps_entities::cloud_backup_mirror_states::Entity),
            schema.create_table_from_entity(temps_entities::cloud_backup_mirror_cursors::Entity),
            schema.create_table_from_entity(temps_entities::s3_sources::Entity),
            schema.create_table_from_entity(temps_entities::external_service_backups::Entity),
            schema.create_table_from_entity(temps_entities::external_services::Entity),
        ] {
            db.execute(backend.build(&statement))
                .await
                .expect("SQLite mirror table creates");
        }
        db.execute_unprepared("PRAGMA foreign_keys = OFF")
            .await
            .expect("SQLite fixture disables unrelated backup foreign keys");
        let now = chrono::Utc::now();
        temps_entities::s3_sources::ActiveModel {
            id: Set(1),
            name: Set("Temps Cloud managed backups".to_owned()),
            bucket_name: Set("managed-bucket".to_owned()),
            region: Set("test-1".to_owned()),
            endpoint: Set(None),
            bucket_path: Set(String::new()),
            access_key_id: Set("encrypted".to_owned()),
            secret_key: Set("encrypted".to_owned()),
            session_token: Set(None),
            credentials_expire_at: Set(None),
            force_path_style: Set(Some(false)),
            // Enrollment makes the managed source the default on an instance
            // that has no other one, which is how backup bytes end up in
            // Cloud's bucket without the operator choosing a destination.
            is_default: Set(true),
            managed_by_cloud: Set(true),
            lifecycle_reconcile_failed_at: Set(None),
            lifecycle_reconcile_generation: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&db)
        .await
        .expect("Cloud-managed S3 source inserts");
        temps_entities::backups::ActiveModel {
            id: Set(1),
            name: Set("managed-backup".to_owned()),
            backup_id: Set(Uuid::new_v4().to_string()),
            schedule_id: Set(None),
            backup_type: Set("full".to_owned()),
            state: Set("completed".to_owned()),
            started_at: Set(now),
            finished_at: Set(Some(now)),
            size_bytes: Set(Some(1)),
            file_count: Set(Some(1)),
            s3_source_id: Set(1),
            s3_location: Set("s3://managed-bucket/backups/manual".to_owned()),
            error_message: Set(None),
            metadata: Set("{}".to_owned()),
            checksum: Set(None),
            compression_type: Set("none".to_owned()),
            created_by: Set(1),
            expires_at: Set(None),
            tags: Set("[]".to_owned()),
            schedule_run_id: Set(None),
        }
        .insert(&db)
        .await
        .expect("completed managed backup inserts");
        db
    }

    async fn mirror_states(
        db: &sea_orm::DatabaseConnection,
    ) -> Vec<temps_entities::cloud_backup_mirror_states::Model> {
        temps_entities::cloud_backup_mirror_states::Entity::find()
            .order_by_asc(temps_entities::cloud_backup_mirror_states::Column::BackupId)
            .all(db)
            .await
            .expect("mirror states read")
    }

    #[tokio::test]
    async fn managed_backups_are_registered_even_when_cloud_export_consent_is_off() {
        let temp = tempfile::tempdir().expect("cloud-link state dir");
        let link = linked_link_fixture(&temp);
        // Exactly the state a real enrollment leaves behind: Cloud provisioned
        // the managed destination and the backup engine started writing to it,
        // while `settings.cloud.backups_enabled` was never switched on.
        link.set_feature_switches(CloudFeatureSwitches::default())
            .expect("consent switches default to off");
        assert!(!link.backups_enabled());
        let tenant_id = link.tenant_id().expect("fixture link carries a tenant");

        let db = Arc::new(managed_backup_fixture_db().await);
        let encryption = Arc::new(temps_core::EncryptionService::new_from_password(
            "mirror-gate-test",
        ));

        // Control: nothing of this instance's is in Cloud storage, and export
        // consent is off, so there is genuinely nothing to register.
        link.set_managed_backup_destination(false);
        assert_eq!(
            sweep(&link, &db, &encryption)
                .await
                .expect("sweep runs against the fixture"),
            SweepOutcome::NotLinked
        );
        assert!(mirror_states(&db).await.is_empty());

        // The reported bug: a Cloud-managed destination exists, so completed
        // backups have already been written into a bucket Cloud owns. Skipping
        // them here does not keep one byte out of Cloud — it only leaves those
        // objects with no Cloud record, invisible and unrestorable.
        link.set_managed_backup_destination(true);
        let outcome = sweep(&link, &db, &encryption)
            .await
            .expect("sweep runs against the fixture");
        assert_ne!(
            outcome,
            SweepOutcome::NotLinked,
            "a backup already written to Cloud-managed storage must be processed, not skipped"
        );
        let states = mirror_states(&db).await;
        assert_eq!(states.len(), 1, "the managed backup was never processed");
        assert_eq!(states[0].backup_id, 1);
        assert_eq!(states[0].tenant_id, tenant_id);
    }

    /// The spawned loop — not `sweep` in isolation — must actually reach the
    /// database and register a managed backup, then stop when asked.
    ///
    /// This runs on the real clock. A virtual clock cannot be used here: tokio
    /// auto-advances paused time whenever the runtime is idle, which includes
    /// every moment a query is out at the SQLite worker thread, so the pool's
    /// own acquire timeout fast-forwards and trips while the connection is
    /// legitimately in use. The cadence half of the loop's behaviour — that an
    /// unlinked tick must not push the next one out to the outage ceiling — is
    /// therefore asserted separately and deterministically in
    /// `an_unlinked_instance_rechecks_soon_instead_of_sleeping_out_the_ceiling`.
    /// The two together cover what a `sweep`-only unit test cannot: that the
    /// loop runs the work, and that it comes back soon enough to matter.
    #[tokio::test]
    async fn the_mirror_loop_registers_managed_backups_and_stops_on_request() {
        let temp = tempfile::tempdir().expect("cloud-link state dir");
        let link = linked_link_fixture(&temp);
        // The state a real enrollment leaves behind: a Cloud-managed backup
        // destination in use, with export consent never switched on.
        link.set_feature_switches(CloudFeatureSwitches::default())
            .expect("consent switches default to off");
        link.set_managed_backup_destination(true);
        let db = Arc::new(managed_backup_fixture_db().await);
        let encryption = Arc::new(temps_core::EncryptionService::new_from_password(
            "mirror-loop-test",
        ));
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

        // `run` starts with a zero-length first sleep, so its first sweep is
        // immediate and needs no clock manipulation to observe.
        let loop_handle =
            tokio::spawn(run(link.clone(), db.clone(), encryption.clone(), cancel_rx));

        let registered = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let states = mirror_states(&db).await;
                if !states.is_empty() {
                    return states;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the spawned mirror loop must run its first sweep, not sit idle");
        assert_eq!(registered.len(), 1);
        assert_eq!(registered[0].backup_id, 1);

        cancel_tx.send(true).expect("send shutdown signal");
        tokio::time::timeout(Duration::from_secs(5), loop_handle)
            .await
            .expect("mirror loop stops promptly on cancellation, not at the next tick")
            .expect("mirror loop does not panic");
    }

    #[test]
    fn backup_engine_key_reads_the_dispatch_resolved_engine() {
        assert_eq!(
            backup_engine_key(r#"{"engine":"postgres_pgdump","other":1}"#).as_deref(),
            Some("postgres_pgdump")
        );
        assert_eq!(
            backup_engine_key(r#"{"engine":"postgres_walg"}"#).as_deref(),
            Some("postgres_walg")
        );
        assert_eq!(backup_engine_key("{}"), None, "no engine key recorded");
        assert_eq!(backup_engine_key("not json"), None, "malformed metadata");
        assert_eq!(
            backup_engine_key(r#"{"engine":42}"#),
            None,
            "non-string engine value"
        );
    }

    /// A Postgres backup whose `backups.metadata.engine` reads
    /// `"postgres_pgdump"` — the container had no wal-g in it, so
    /// `temps-backup`'s dispatch fell back from `postgres_walg` at backup
    /// time (`crates/temps-backup/src/engines/dispatch.rs`). Its S3 location
    /// is therefore never a WAL-G repository and it never carries a
    /// `_backup_stop_sentinel.json`.
    ///
    /// `mirror_backup` must recognize the recorded engine and route into
    /// `mirror_native_backup` instead of `mirror_walg_backup`. Routed
    /// correctly, dispatch reaches `mirror_native_backup`'s own S3 listing
    /// call, which fails against the closed loopback source with a
    /// *transient* `StageError::Retry` (connection refused). Routed
    /// incorrectly — the bug this regression test guards against —
    /// `mirror_walg_backup` rejects the location synchronously, with no I/O
    /// at all, as a *permanent* `StageError::Unsupported("... is not a WAL-G
    /// repository ...")`. The two failure shapes are distinguishable without
    /// a working S3 backend, so this test needs no stub server.
    #[tokio::test]
    async fn postgres_backups_recorded_as_pgdump_route_to_native_mirror_not_walg() {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("in-memory SQLite connects");
        let backend = db.get_database_backend();
        let schema = Schema::new(backend);
        for statement in [
            schema.create_table_from_entity(temps_entities::backups::Entity),
            schema.create_table_from_entity(temps_entities::s3_sources::Entity),
            schema.create_table_from_entity(temps_entities::external_service_backups::Entity),
            schema.create_table_from_entity(temps_entities::external_services::Entity),
        ] {
            db.execute(backend.build(&statement))
                .await
                .expect("SQLite fixture table creates");
        }
        db.execute_unprepared("PRAGMA foreign_keys = OFF")
            .await
            .expect("SQLite fixture disables unrelated foreign keys");

        let encryption = temps_core::EncryptionService::new_from_password("pgdump-dispatch-test");
        let now = chrono::Utc::now();
        temps_entities::s3_sources::ActiveModel {
            id: Set(1),
            name: Set("Temps Cloud managed backups".to_owned()),
            bucket_name: Set("managed-bucket".to_owned()),
            region: Set("test-1".to_owned()),
            // A closed loopback port: any S3 call this test reaches fails
            // fast with connection-refused instead of hanging or reaching
            // real AWS, per the convention documented on `linked_link_fixture`.
            endpoint: Set(Some("http://127.0.0.1:1".to_owned())),
            bucket_path: Set(String::new()),
            access_key_id: Set(encryption
                .encrypt_string("test-access-key")
                .expect("encrypt fixture access key")),
            secret_key: Set(encryption
                .encrypt_string("test-secret-key")
                .expect("encrypt fixture secret key")),
            session_token: Set(None),
            credentials_expire_at: Set(None),
            force_path_style: Set(Some(true)),
            is_default: Set(true),
            managed_by_cloud: Set(true),
            lifecycle_reconcile_failed_at: Set(None),
            lifecycle_reconcile_generation: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&db)
        .await
        .expect("S3 source inserts");
        temps_entities::external_services::Model {
            id: 1,
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
            created_by_user_id: None,
        }
        .into_active_model()
        .insert(&db)
        .await
        .expect("external service inserts");
        let backup_uuid = Uuid::new_v4().to_string();
        let s3_location = format!(
            "s3://managed-bucket/external_services/postgres/postgres/2026/09/04/{backup_uuid}/dump.sql.gz"
        );
        temps_entities::external_service_backups::Model {
            id: 1,
            service_id: 1,
            backup_id: 1,
            backup_type: "full".to_owned(),
            state: "completed".to_owned(),
            started_at: now,
            finished_at: Some(now),
            size_bytes: Some(1),
            s3_location: s3_location.clone(),
            error_message: None,
            metadata: serde_json::json!({}),
            checksum: None,
            compression_type: "gzip".to_owned(),
            created_by: 1,
            expires_at: None,
        }
        .into_active_model()
        .insert(&db)
        .await
        .expect("external service backup inserts");
        let backup = temps_entities::backups::ActiveModel {
            id: Set(1),
            name: Set("pgdump-fallback-backup".to_owned()),
            backup_id: Set(backup_uuid),
            schedule_id: Set(None),
            backup_type: Set("full".to_owned()),
            state: Set("completed".to_owned()),
            started_at: Set(now),
            finished_at: Set(Some(now)),
            size_bytes: Set(Some(1)),
            file_count: Set(Some(1)),
            s3_source_id: Set(1),
            s3_location: Set(s3_location),
            error_message: Set(None),
            metadata: Set(serde_json::json!({"engine": "postgres_pgdump"}).to_string()),
            checksum: Set(None),
            compression_type: Set("gzip".to_owned()),
            created_by: Set(1),
            expires_at: Set(None),
            tags: Set("[]".to_owned()),
            schedule_run_id: Set(None),
        }
        .insert(&db)
        .await
        .expect("pg_dump-fallback backup inserts");

        let temp = tempfile::tempdir().expect("cloud-link state dir");
        let link = linked_link_fixture(&temp);
        let instance_id = link.instance_id().expect("linked instance id");
        let mut resources =
            super::SweepResources::load(&db, &encryption, std::slice::from_ref(&backup))
                .await
                .expect("resources load");

        let error = mirror_backup(&link, &mut resources, &backup, instance_id)
            .await
            .expect_err("closed loopback source can never actually mirror in this test");
        match error {
            // `mirror_native_backup` reached its own repository-listing S3
            // call and failed there (connection refused). The shared listing
            // helper's error text says "WAL-G repository" regardless of which
            // engine called it (RustFS and MariaDB hit the same string) — it
            // is not evidence of taking the WAL-G *path*, only of the
            // listing helper's generic naming, so this is exactly the
            // "correctly routed, failed on unreachable S3" outcome under test.
            StageError::Retry(_) => {}
            StageError::Unsupported(reason) => panic!(
                "pg_dump-tagged backup was rejected instead of routed to native mirroring \
                 (this is the exact bug under test — dispatch fell through to WAL-G's synchronous \
                 \"not a WAL-G repository\" check, which runs before any I/O and can only produce \
                 Unsupported, never Retry): {reason}"
            ),
        }
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
    fn a_postgres_service_pinned_only_by_image_tag_still_resolves_a_major_version() {
        // `external_services.version` is optional and the PostgreSQL provider
        // leaves it empty, pinning the version through the image tag instead.
        // Reading the column raw made every such service permanently
        // unmirrorable with "has no PostgreSQL major version", which is the
        // second thing that kept real backups out of Cloud.
        let encryption = temps_core::EncryptionService::new_from_password("engine-version-test");
        let config = encryption
            .encrypt_string(
                &serde_json::json!({"docker_image": "gotempsh/postgres-walg:18-bookworm"})
                    .to_string(),
            )
            .expect("service config encrypts");
        let now = chrono::Utc::now();
        let mut service = temps_entities::external_services::Model {
            id: 1,
            name: "managed-postgres".to_owned(),
            service_type: "postgres".to_owned(),
            version: None,
            status: "running".to_owned(),
            created_at: now,
            updated_at: now,
            slug: None,
            config: Some(config),
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
            created_by_user_id: None,
        };

        assert_eq!(
            parse_postgres_major(Some(&super::service_engine_version(&encryption, &service))),
            Some(18),
            "a version pinned only by the image tag must still be usable"
        );

        // An explicit column value still wins over the image tag.
        service.version = Some("17.6".to_owned());
        assert_eq!(
            parse_postgres_major(Some(&super::service_engine_version(&encryption, &service))),
            Some(17)
        );

        // A service with neither is reported as unsupported rather than
        // silently mirrored against a guessed engine version.
        service.version = None;
        service.config = None;
        assert_eq!(
            parse_postgres_major(Some(&super::service_engine_version(&encryption, &service))),
            None
        );
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
    fn only_real_wal_segments_are_range_compared() {
        let prefix = "repo/wal_005/";
        // Every compression WAL-G may have used names the same segment.
        for suffix in [".lz4", ".br", ".zst", ""] {
            assert_eq!(
                wal_segment_of(&format!("{prefix}000000010000000000000007{suffix}"), prefix),
                Some("000000010000000000000007"),
                "the compression suffix must not change the segment identity"
            );
        }
        // Timeline history and partial/short bookkeeping objects are not
        // segments and must never be compared against segment bounds.
        assert_eq!(
            wal_segment_of(&format!("{prefix}00000002.history"), prefix),
            None
        );
        assert_eq!(wal_segment_of(&format!("{prefix}short"), prefix), None);
        assert_eq!(
            wal_segment_of(&format!("{prefix}00000001000000000000000g.lz4"), prefix),
            None,
            "a non-hexadecimal name is not a segment"
        );
        // Objects outside the WAL prefix are never segments.
        assert_eq!(
            wal_segment_of("repo/basebackups_005/base_7/metadata.json", prefix),
            None
        );
    }

    /// Two attempts that send the same objects must produce the same digest,
    /// and any change to any field must change it — that is the whole point of
    /// logging it, since object count and total size can both stay equal while
    /// the manifest changes.
    #[test]
    fn the_logged_manifest_digest_identifies_the_exact_manifest() {
        use temps_cloud_protocol::{WalGObjectDeclaration, WalGObjectKind};
        let object = |key: &str, bytes: u64, checksum: &str| WalGObjectDeclaration {
            relative_key: key.to_owned(),
            kind: WalGObjectKind::Wal,
            bytes,
            checksum_sha256: checksum.to_owned(),
        };
        let checksum = "a".repeat(64);
        let manifest = vec![
            object("wal_005/000000010000000000000006.lz4", 16, &checksum),
            object("wal_005/000000010000000000000007.lz4", 32, &checksum),
        ];
        let digest = manifest_digest(&manifest);

        // Listing order must not matter: the manifest is the set of objects.
        let mut reordered = manifest.clone();
        reordered.reverse();
        assert_eq!(manifest_digest(&reordered), digest);

        // A size moved between two objects keeps both the count and the total
        // identical. Only the digest catches it.
        let mut rebalanced = manifest.clone();
        rebalanced[0].bytes = 32;
        rebalanced[1].bytes = 16;
        assert_ne!(manifest_digest(&rebalanced), digest);

        let mut rekeyed = manifest.clone();
        rekeyed[1].relative_key = "wal_005/000000010000000000000008.lz4".to_owned();
        assert_ne!(manifest_digest(&rekeyed), digest);

        let mut rechecksummed = manifest;
        rechecksummed[1].checksum_sha256 = "b".repeat(64);
        assert_ne!(manifest_digest(&rechecksummed), digest);
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

    /// Empty `ListBucketResult` — the response a real S3-compatible provider
    /// returns for a prefix with no objects under it, e.g. an `mc mirror` run
    /// whose source bucket was empty at backup time.
    async fn empty_list_objects_stub() -> impl axum::response::IntoResponse {
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/xml")],
            r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
    <Name>source-bucket</Name>
    <Prefix></Prefix>
    <KeyCount>0</KeyCount>
    <MaxKeys>1000</MaxKeys>
    <IsTruncated>false</IsTruncated>
</ListBucketResult>"#,
        )
    }

    /// Regression covering both halves of the empty-manifest fix:
    ///
    /// 1. A native-mirror snapshot that lists zero objects (the source was
    ///    empty when `mc mirror`/the engine ran) is classified `unsupported`,
    ///    not `transient` — this condition is permanent for this specific,
    ///    already-finished backup, so it must never be reported as something
    ///    that might resolve on a later attempt.
    /// 2. `sweep` must not fold that permanent classification into the
    ///    sweep-wide retry signal. Before the fix, one such backup pushed
    ///    every future sweep's cadence to `MAX_SWEEP_INTERVAL` forever (see
    ///    `next_sweep_interval`'s `Retry` arm) — starving discovery of every
    ///    *other*, healthy backup on the instance of prompt reporting to
    ///    Cloud, not just this one. `sweep` returning anything other than
    ///    `SweepOutcome::Retry` here is the regression assertion for that.
    #[tokio::test]
    async fn empty_native_mirror_snapshot_is_permanent_not_transient_and_does_not_stall_the_sweep()
    {
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("sandbox denied TCP bind; skipping empty-manifest sweep test");
                return;
            }
            Err(error) => panic!("bind empty-manifest stub: {error}"),
        };
        let address = listener.local_addr().expect("empty-manifest stub address");
        let app = Router::new()
            .route("/{bucket}", get(empty_list_objects_stub))
            .route("/{bucket}/", get(empty_list_objects_stub));
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve empty-manifest stub");
        });

        let temp = tempfile::tempdir().expect("cloud-link state dir");
        let link = linked_link_fixture(&temp);
        link.set_feature_switches(CloudFeatureSwitches::default())
            .expect("consent switches default to off");
        link.set_managed_backup_destination(true);
        let tenant_id = link.tenant_id().expect("fixture link carries a tenant");

        let db = Database::connect("sqlite::memory:")
            .await
            .expect("in-memory SQLite connects");
        let backend = db.get_database_backend();
        let schema = Schema::new(backend);
        for statement in [
            schema.create_table_from_entity(temps_entities::backups::Entity),
            schema.create_table_from_entity(temps_entities::cloud_backup_mirror_states::Entity),
            schema.create_table_from_entity(temps_entities::cloud_backup_mirror_cursors::Entity),
            schema.create_table_from_entity(temps_entities::s3_sources::Entity),
            schema.create_table_from_entity(temps_entities::external_service_backups::Entity),
            schema.create_table_from_entity(temps_entities::external_services::Entity),
        ] {
            db.execute(backend.build(&statement))
                .await
                .expect("SQLite fixture table creates");
        }
        db.execute_unprepared("PRAGMA foreign_keys = OFF")
            .await
            .expect("SQLite fixture disables unrelated foreign keys");

        let encryption =
            temps_core::EncryptionService::new_from_password("empty-manifest-sweep-test");
        let now = chrono::Utc::now();
        temps_entities::s3_sources::ActiveModel {
            id: Set(1),
            name: Set("Temps Cloud managed backups".to_owned()),
            bucket_name: Set("source-bucket".to_owned()),
            region: Set("test-1".to_owned()),
            endpoint: Set(Some(format!("http://{address}"))),
            bucket_path: Set(String::new()),
            access_key_id: Set(encryption
                .encrypt_string("test-access-key")
                .expect("encrypt fixture access key")),
            secret_key: Set(encryption
                .encrypt_string("test-secret-key")
                .expect("encrypt fixture secret key")),
            session_token: Set(None),
            credentials_expire_at: Set(None),
            force_path_style: Set(Some(true)),
            is_default: Set(true),
            managed_by_cloud: Set(true),
            lifecycle_reconcile_failed_at: Set(None),
            lifecycle_reconcile_generation: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&db)
        .await
        .expect("S3 source inserts");
        temps_entities::external_services::Model {
            id: 1,
            name: "empty-mirror".to_owned(),
            service_type: "s3".to_owned(),
            version: None,
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
            created_by_user_id: None,
        }
        .into_active_model()
        .insert(&db)
        .await
        .expect("external service inserts");
        let backup_uuid = Uuid::new_v4().to_string();
        let location = format!("external_services/s3/empty-mirror/{backup_uuid}");
        temps_entities::backups::ActiveModel {
            id: Set(1),
            name: Set("s3 backup (empty-mirror)".to_owned()),
            backup_id: Set(backup_uuid.clone()),
            schedule_id: Set(None),
            backup_type: Set("scheduled".to_owned()),
            state: Set("completed".to_owned()),
            started_at: Set(now),
            finished_at: Set(Some(now)),
            size_bytes: Set(Some(0)),
            file_count: Set(Some(0)),
            s3_source_id: Set(1),
            s3_location: Set(location.clone()),
            error_message: Set(None),
            metadata: Set("{}".to_owned()),
            checksum: Set(None),
            compression_type: Set("none".to_owned()),
            created_by: Set(1),
            expires_at: Set(None),
            tags: Set("[]".to_owned()),
            schedule_run_id: Set(None),
        }
        .insert(&db)
        .await
        .expect("empty s3-mirror backup inserts");
        temps_entities::external_service_backups::Model {
            id: 1,
            service_id: 1,
            backup_id: 1,
            backup_type: "scheduled".to_owned(),
            state: "completed".to_owned(),
            started_at: now,
            finished_at: Some(now),
            size_bytes: Some(0),
            s3_location: location,
            error_message: None,
            metadata: serde_json::json!({}),
            checksum: None,
            compression_type: "none".to_owned(),
            created_by: 1,
            expires_at: None,
        }
        .into_active_model()
        .insert(&db)
        .await
        .expect("external service backup link inserts");

        let db = Arc::new(db);
        let encryption = Arc::new(encryption);
        let outcome = sweep(&link, &db, &encryption)
            .await
            .expect("sweep runs against the empty-manifest fixture");
        let states = mirror_states(&db).await;
        assert_ne!(
            outcome,
            SweepOutcome::Retry,
            "a permanently-empty native-mirror snapshot must not degrade the shared sweep \
             cadence — every other backup on the instance would wait behind it"
        );
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].tenant_id, tenant_id);
        assert_eq!(states[0].classification, "unsupported");
        assert!(
            states[0]
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("has no objects")),
            "unexpected reason: {:?}",
            states[0].reason
        );
    }
}
