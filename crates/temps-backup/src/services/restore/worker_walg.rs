// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! WAL-G restores of a PostgreSQL on a worker node (ADR-092, PostgreSQL
//! step 3): the control plane's own `restore_from_walg`, run on the node by
//! its agent, in place or to a point in time.
//!
//! Admission is the control plane's: the same engine-compatibility,
//! cross-service confirmation and PITR checks as a local restore (in
//! `start_restore`), and the same login reconciliation (the restored data
//! answers to its origin's login, which the target's stored configuration is
//! patched to afterwards). Only the transport differs. Restoring into a new
//! service is not available on a worker yet and is refused at admission.

use std::time::Duration;

use sea_orm::{DatabaseConnection, EntityTrait};
use temps_providers::externalsvc::postgres_walg_backup::walg_key_prefix;
use temps_providers::externalsvc::RestoreCapabilities;
use temps_providers::remote_service_client::PostgresWalgRestoreResult;
use tracing::{info, warn};

use super::*;

/// How long reading a worker archive's PITR window may take before the
/// capabilities report it as unknown.
const WINDOW_DEADLINE: Duration = Duration::from_secs(10);

/// At most this many listing pages (1,000 keys each) are read for the
/// window: a bigger archive reports its latest point as unknown.
const WINDOW_MAX_PAGES: usize = 100;

/// Whether `location` is a WAL-G repository, which a WAL-G restore fetches,
/// rather than a `pg_dumpall` dump, which a job applies.
pub(super) fn is_walg_backup(location: &str) -> bool {
    location.starts_with("s3://") && !location.ends_with(".sql.gz")
}

/// Whether a restore of `location` onto `target` runs here: a WAL-G
/// backup into a PostgreSQL on a worker node.
pub(super) fn takes_walg_path(
    target: &temps_entities::external_services::Model,
    location: &str,
) -> bool {
    crate::engines::workload_job::takes_job_path(target) && is_walg_backup(location)
}

/// Refuse what a worker's WAL-G restore cannot do: a new service (in place,
/// or to a point in time in place, only).
pub(super) fn walg_restore_supported(
    target: &temps_entities::external_services::Model,
    mode: &RestoreRequestMode,
) -> Result<(), RestoreError> {
    let node = crate::engines::workload_job::worker_node(target).unwrap_or_default();
    match mode {
        RestoreRequestMode::InPlace
        | RestoreRequestMode::Pitr {
            to_new_service: false,
            ..
        } => Ok(()),
        _ => Err(RestoreError::Validation {
            message: format!(
                "Database {} ('{}') runs on worker node {node}, where a WAL-G backup is restored \
                 in place or to a point in time in place; restoring it into a new service ('{}') \
                 is not available there yet. Restore in place, or create the new service first \
                 and restore into it",
                target.id,
                target.name,
                mode.as_str()
            ),
        }),
    }
}

/// What a worker's WAL-G restore does, for the plan.
pub(super) fn steps(
    node: i32,
    location: &str,
    mode: &RestoreRequestMode,
    container: &str,
    cross_service: bool,
) -> Vec<String> {
    let target = match mode {
        RestoreRequestMode::Pitr { target, .. } => match target {
            RecoveryTarget::Time { time } => format!("the point in time {}", time.to_rfc3339()),
            RecoveryTarget::Xid { xid } => format!("transaction {xid}"),
            RecoveryTarget::Lsn { lsn } => format!("WAL location {lsn}"),
            RecoveryTarget::Name { name } => format!("restore point '{name}'"),
        },
        _ => "the base backup's consistency point".to_string(),
    };
    let mut steps = vec![
        format!(
            "Restore on worker node {node}, through its agent, with the control plane's WAL-G \
             restore, into container {container}"
        ),
        format!(
            "wal-g backup-fetch from {location} into a staging directory on the data volume \
             while PostgreSQL keeps running"
        ),
        format!(
            "Configure recovery: restore_command fetches WAL from the backup's archive \
             (read-only credentials), recovery target {target}, then promote; archiving is \
             suspended"
        ),
        format!(
            "Stop {container} and swap the staged data in, keeping the original data directory \
             aside on the volume"
        ),
        "Start PostgreSQL on the restored data and wait for recovery to finish and promote"
            .to_string(),
        "On success delete the kept original; on failure put the original back and start it as \
         it was"
            .to_string(),
    ];
    if cross_service {
        steps.push(
            "Update the service's stored credentials to the restored login (the backup origin's)"
                .to_string(),
        );
    }
    steps
}

/// The capabilities of a worker PostgreSQL: in place and to a point in
/// time, with the window its own archive covers when it can be read.
pub(super) async fn capabilities(
    db: &DatabaseConnection,
    enc: &temps_core::EncryptionService,
    service: &temps_entities::external_services::Model,
    read_window: bool,
) -> RestoreCapabilities {
    let (earliest_pitr_time, latest_pitr_time) = if read_window {
        match tokio::time::timeout(WINDOW_DEADLINE, archive_window(db, enc, service)).await {
            Ok(Ok(window)) => window,
            Ok(Err(reason)) => {
                warn!(
                    service_id = service.id,
                    %reason,
                    "Could not read the PITR window of a worker PostgreSQL's archive"
                );
                (None, None)
            }
            Err(_) => {
                warn!(
                    service_id = service.id,
                    "Reading the PITR window of a worker PostgreSQL's archive timed out"
                );
                (None, None)
            }
        }
    } else {
        (None, None)
    };
    RestoreCapabilities {
        restore_in_place: true,
        restore_to_new_service: false,
        pitr: true,
        earliest_pitr_time,
        latest_pitr_time,
    }
}

type Window = (Option<chrono::DateTime<Utc>>, Option<chrono::DateTime<Utc>>);

/// The PITR window of `service`'s WAL-G archive, read from the bucket it
/// archives to: from its oldest completed base backup (its sentinel's
/// upload) to its newest archived WAL object. `(None, None)` while it has no
/// archive.
async fn archive_window(
    db: &DatabaseConnection,
    enc: &temps_core::EncryptionService,
    service: &temps_entities::external_services::Model,
) -> Result<Window, String> {
    let Some(source_id) = service.continuous_archive_s3_source_id else {
        return Ok((None, None));
    };
    let source = temps_entities::s3_sources::Entity::find_by_id(source_id)
        .one(db)
        .await
        .map_err(|e| format!("loading S3 source {source_id}: {e}"))?
        .ok_or_else(|| format!("S3 source {source_id} does not exist"))?;
    let decrypt = |field: &str, value: &str| {
        enc.decrypt_string(value)
            .map_err(|e| format!("decrypting the {field} of S3 source {source_id}: {e}"))
    };
    let credentials = S3Credentials {
        access_key_id: decrypt("access key", &source.access_key_id)?,
        secret_key: decrypt("secret key", &source.secret_key)?,
        session_token: temps_entities::s3_sources::decrypt_session_token(enc, &source)
            .map_err(|e| format!("decrypting the session token of S3 source {source_id}: {e}"))?,
        region: source.region.clone(),
        endpoint: source.endpoint.clone(),
        bucket_name: source.bucket_name.clone(),
        bucket_path: source.bucket_path.clone(),
        force_path_style: source.force_path_style.unwrap_or(true),
    };
    let client = build_s3_client(&credentials);
    let prefix = walg_key_prefix(&source.bucket_path, &service.name);
    let earliest = newest_or_oldest(
        &client,
        &source.bucket_name,
        &format!("{prefix}basebackups_005/"),
        |key| key.ends_with("_backup_stop_sentinel.json"),
        Pick::Oldest,
    )
    .await?;
    if earliest.is_none() {
        return Ok((None, None));
    }
    let latest = newest_or_oldest(
        &client,
        &source.bucket_name,
        &format!("{prefix}wal_005/"),
        |_| true,
        Pick::Newest,
    )
    .await?;
    Ok((earliest, latest))
}

#[derive(Clone, Copy)]
enum Pick {
    Oldest,
    Newest,
}

/// The oldest or newest upload time among the keys under `prefix` that
/// `keep` selects. `Err` past [`WINDOW_MAX_PAGES`].
async fn newest_or_oldest(
    client: &S3Client,
    bucket: &str,
    prefix: &str,
    keep: impl Fn(&str) -> bool,
    pick: Pick,
) -> Result<Option<chrono::DateTime<Utc>>, String> {
    let mut found: Option<chrono::DateTime<Utc>> = None;
    let mut token = None;
    for _ in 0..WINDOW_MAX_PAGES {
        let page = client
            .list_objects_v2()
            .bucket(bucket)
            .prefix(prefix)
            .set_continuation_token(token)
            .send()
            .await
            .map_err(|e| format!("listing s3://{bucket}/{prefix}: {e}"))?;
        for object in page.contents() {
            if !object.key().is_some_and(&keep) {
                continue;
            }
            let Some(modified) = object
                .last_modified()
                .and_then(|t| chrono::DateTime::<Utc>::from_timestamp(t.secs(), t.subsec_nanos()))
            else {
                continue;
            };
            found = Some(match (found, pick) {
                (None, _) => modified,
                (Some(seen), Pick::Oldest) => seen.min(modified),
                (Some(seen), Pick::Newest) => seen.max(modified),
            });
        }
        match page.next_continuation_token() {
            Some(next) => token = Some(next.to_string()),
            None => return Ok(found),
        }
    }
    Err(format!(
        "s3://{bucket}/{prefix} holds more than {} objects",
        WINDOW_MAX_PAGES * 1000
    ))
}

/// Restore `backup`, a WAL-G backup, into `target`, a PostgreSQL on a worker
/// node, in place or to the point `mode` names.
#[allow(
    clippy::too_many_arguments,
    reason = "the restore task's dependencies and the run's inputs"
)]
pub(super) async fn run(
    db: &Arc<DatabaseConnection>,
    enc: &Arc<temps_core::EncryptionService>,
    scoper: &temps_providers::worker_s3_scope::WorkerCredentialScoper,
    mgr: &Arc<ExternalServiceManager>,
    run_id: i32,
    backup: &temps_entities::backups::Model,
    target: &temps_entities::external_services::Model,
    mode: &RestoreRequestMode,
) -> Result<Option<i32>, RestoreError> {
    use crate::engines::{worker_agent, workload_job};
    walg_restore_supported(target, mode)?;
    let node_id = workload_job::worker_node(target).ok_or_else(|| RestoreError::Internal {
        reason: format!(
            "restore run {run_id}: database {} left its worker node",
            target.id
        ),
    })?;
    let recovery_target = match mode {
        RestoreRequestMode::Pitr { target, .. } => Some(target.clone()),
        _ => None,
    };
    if let Some(recovery_target) = &recovery_target {
        temps_providers::externalsvc::postgres_walg_restore::validate_recovery_target(
            recovery_target,
        )
        .map_err(|message| RestoreError::Validation { message })?;
    }
    let target_user_data = selected_walg_target_user_data(backup)?;
    // The base backup carries its origin's `pg_authid`: the restored server
    // answers to the origin's login, which recovery and the readiness check
    // use, and which the target's stored config is patched to afterwards.
    let origin_credentials = origin_credentials_to_reconcile(db, mgr, backup, target).await;
    let restored_login = origin_credentials.as_ref().map(|origin| {
        let mut login = serde_json::Map::new();
        origin.apply(&mut login);
        login
    });
    let s3_source = crate::engines::v2_common::load_s3_source(db.as_ref(), backup.s3_source_id)
        .await
        .map_err(|e| RestoreError::ExternalService {
            reason: format!("restore run {run_id}: {e}"),
        })?;
    // The worker can read the backup's WAL-G repository (its base backups
    // and the WAL recovery replays) and nothing else (ADR-093).
    let Some(repository) = temps_providers::worker_s3_scope::s3_url_prefix(
        &backup.s3_location,
        &s3_source.bucket_name,
    )
    .filter(|prefix| !prefix.is_empty()) else {
        return Err(RestoreError::Validation {
            message: format!(
                "Backup {} is at '{}', outside bucket '{}' of S3 source {}",
                backup.id, backup.s3_location, s3_source.bucket_name, backup.s3_source_id
            ),
        });
    };
    let destination = worker_agent::scoped_destination(
        scoper,
        db.as_ref(),
        backup.s3_source_id,
        temps_providers::worker_s3_scope::WorkerS3Grant::job(
            target.id,
            node_id,
            repository,
            temps_core::ScopedS3Access::Read,
            "restore",
        ),
        "restore",
    )
    .await
    .map_err(|e| RestoreError::ExternalService {
        reason: format!("restore run {run_id}: {e}"),
    })?;

    // The last safe point: once the node stops the server, the run can no
    // longer be cancelled.
    let phase = if recovery_target.is_some() {
        "recover"
    } else {
        "restore"
    };
    super::super::restore_cancel::enter_phase_unless_cancelled(db.as_ref(), run_id, phase).await?;
    info!(
        run_id,
        service_id = target.id,
        node_id,
        backup_id = backup.id,
        pitr = recovery_target.is_some(),
        "restoring database {} from WAL-G on its node",
        target.id
    );
    // A node already running its maximum of backups and restores is waited
    // for, visibly and cancellably, instead of failing the run.
    let outcome = crate::node_busy::retry_while_node_busy(
        &crate::node_busy::NodeBusyPolicy::DEFAULT,
        || {
            mgr.walg_restore_worker_postgres(
                target.id,
                restored_login.as_ref(),
                destination.clone(),
                backup.s3_location.clone(),
                recovery_target.clone(),
                target_user_data.clone(),
            )
        },
        |wait| super::wait_for_node_slot(db.as_ref(), run_id, phase, wait),
    )
    .await?
    .map_err(|e| RestoreError::ExternalService {
        reason: format!("restore run {run_id} on node {node_id}: {e}"),
    })?;
    match outcome {
        PostgresWalgRestoreResult::Completed => {}
        PostgresWalgRestoreResult::Refused { reason } => {
            return Err(RestoreError::Validation {
                message: format!("restore run {run_id} on node {node_id} was refused: {reason}"),
            })
        }
        PostgresWalgRestoreResult::Failed { reason } => {
            return Err(RestoreError::ExternalService {
                reason: format!("restore run {run_id} on node {node_id}: {reason}"),
            })
        }
    }
    reconcile_restored_credentials(db, enc, target.id, origin_credentials.as_ref()).await;
    if let Err(e) = update_phase(db.as_ref(), run_id, "verify").await {
        warn!("Restore run {run_id}: recording the verify phase failed: {e}");
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker_postgres() -> temps_entities::external_services::Model {
        temps_entities::external_services::Model {
            id: 11,
            name: "orders".to_string(),
            service_type: "postgres".to_string(),
            version: None,
            status: "running".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            slug: None,
            config: None,
            node_id: Some(2),
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
                temps_entities::external_service_workload_mode::ServiceWorkloadMode::Enforce,
            log_ingest_lines_per_sec: None,
            log_ingest_kib_per_sec: None,
        }
    }

    #[test]
    fn only_a_walg_repository_on_a_worker_takes_the_walg_path() {
        let worker = worker_postgres();
        let walg = "s3://backups/external_services/postgres/orders/walg";
        assert!(takes_walg_path(&worker, walg));
        assert!(!takes_walg_path(
            &worker,
            "s3://backups/external_services/postgres/orders/2026/dump.sql.gz"
        ));
        assert!(!takes_walg_path(
            &worker,
            "external_services/postgres/orders/dump.sql.gz"
        ));
        let mut local = worker.clone();
        local.node_id = None;
        assert!(!takes_walg_path(&local, walg));
    }

    /// In place and PITR in place are admitted; a new service is refused
    /// with the remedy.
    #[test]
    fn a_worker_walg_restore_is_in_place() {
        let worker = worker_postgres();
        let time = RecoveryTarget::Time { time: Utc::now() };
        assert!(walg_restore_supported(&worker, &RestoreRequestMode::InPlace).is_ok());
        assert!(walg_restore_supported(
            &worker,
            &RestoreRequestMode::Pitr {
                to_new_service: false,
                new_service_name: None,
                target: time.clone(),
            }
        )
        .is_ok());
        for refused in [
            RestoreRequestMode::Pitr {
                to_new_service: true,
                new_service_name: Some("copy".into()),
                target: time,
            },
            RestoreRequestMode::NewService {
                name: "copy".into(),
                parameter_overrides: serde_json::json!({}),
            },
        ] {
            let message = walg_restore_supported(&worker, &refused)
                .expect_err("a new service is refused")
                .to_string();
            assert!(message.contains("worker node 2"), "{message}");
            assert!(message.contains("Restore in place"), "{message}");
        }
    }

    #[test]
    fn the_plan_describes_the_staged_swap_and_the_target() {
        let pitr = RestoreRequestMode::Pitr {
            to_new_service: false,
            new_service_name: None,
            target: RecoveryTarget::Lsn {
                lsn: "0/3000060".into(),
            },
        };
        let described = steps(2, "s3://b/x/walg", &pitr, "postgres-orders", true).join("\n");
        for expected in [
            "worker node 2",
            "wal-g backup-fetch from s3://b/x/walg",
            "WAL location 0/3000060",
            "keeping the original data directory aside",
            "put the original back",
            "restored login",
        ] {
            assert!(described.contains(expected), "{expected}: {described}");
        }
        let in_place = steps(2, "s3://b/x/walg", &RestoreRequestMode::InPlace, "c", false);
        assert!(in_place.iter().any(|s| s.contains("consistency point")));
        assert!(!in_place.iter().any(|s| s.contains("restored login")));
    }

    /// The capabilities say PITR, and a service that has no archive yet
    /// reports no window rather than an error.
    #[tokio::test]
    async fn a_worker_postgres_restores_to_a_point_in_time() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection();
        let enc = temps_core::EncryptionService::new_from_password("synthetic-restore-test-key");
        let capabilities = capabilities(&db, &enc, &worker_postgres(), true).await;
        assert!(capabilities.pitr);
        assert!(capabilities.restore_in_place);
        assert!(!capabilities.restore_to_new_service);
        assert_eq!(capabilities.earliest_pitr_time, None);
        assert_eq!(capabilities.latest_pitr_time, None);
    }
}
