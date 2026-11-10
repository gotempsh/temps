// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! WAL-G restores of a PostgreSQL on a worker node (ADR-099, PostgreSQL
//! step 3): the control plane's own `restore_from_walg`, run on the node by
//! its agent, in place or to a point in time.
//!
//! Admission is the control plane's: the same engine-compatibility,
//! cross-service confirmation and PITR checks as a local restore (in
//! `start_restore`), and the same login reconciliation (the restored data
//! answers to its origin's login, which the target's stored configuration is
//! patched to afterwards). Only the transport differs. Restoring into a new
//! service is not available on a worker yet and is refused at admission.

use std::future::Future;
use std::time::Duration;

use sea_orm::{DatabaseConnection, EntityTrait};
use temps_providers::externalsvc::postgres_walg_backup::walg_key_prefix;
use temps_providers::externalsvc::RestoreCapabilities;
use temps_providers::remote_service_client::{
    PostgresWalgRestoreProgress, PostgresWalgRestoreResult,
};
use temps_providers::services::ExternalServiceError;
use tracing::{info, warn};

use super::*;

/// How long reading a worker archive's PITR window may take before the
/// capabilities report it as unknown.
const WINDOW_DEADLINE: Duration = Duration::from_secs(10);

/// At most this many listing pages (1,000 keys each) are read for the
/// window: a bigger archive reports its latest point as unknown.
const WINDOW_MAX_PAGES: usize = 100;

/// How a restore whose request went unanswered is waited for: its node is
/// asked every `interval` where the restore is. The wait gives up when the
/// node stays unreachable for `unreachable_for`, or after `max_wait` (the
/// node keeps an outcome for a day).
#[derive(Debug, Clone, Copy)]
pub(super) struct OutcomeWait {
    pub(super) interval: Duration,
    pub(super) unreachable_for: Duration,
    pub(super) max_wait: Duration,
}

const OUTCOME_WAIT: OutcomeWait = OutcomeWait {
    interval: Duration::from_secs(15),
    unreachable_for: Duration::from_secs(2 * 3600),
    max_wait: Duration::from_secs(24 * 3600),
};

/// The name a restore run's WAL-G restore is started under on its node, to
/// ask for its outcome by.
fn operation_id(run_id: i32) -> String {
    format!("restore-run-{run_id}")
}

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
    let mint = || async {
        worker_agent::scoped_destination(
            scoper,
            db.as_ref(),
            backup.s3_source_id,
            temps_providers::worker_s3_scope::WorkerS3Grant::job(
                target.id,
                node_id,
                repository.clone(),
                temps_core::ScopedS3Access::Read,
                "restore",
            ),
            "restore",
        )
        .await
        .map_err(|e| RestoreError::ExternalService {
            reason: format!("restore run {run_id}: {e}"),
        })
    };
    let destination = mint().await?;

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
    // A try after a wait gets a credential minted after it.
    let answered = crate::node_busy::retry_while_node_busy_with_fresh_credential(
        &crate::node_busy::NodeBusyPolicy::DEFAULT,
        destination,
        mint,
        |destination| {
            mgr.walg_restore_worker_postgres(
                target.id,
                restored_login.as_ref(),
                destination,
                backup.s3_location.clone(),
                recovery_target.clone(),
                target_user_data.clone(),
                Some(operation_id(run_id)),
            )
        },
        |wait| super::wait_for_node_slot(db.as_ref(), run_id, phase, wait),
    )
    .await?;
    let outcome = match answered {
        Ok(outcome) => outcome,
        // The request timed out or its connection dropped: the node may
        // still be restoring, past its deadline once it began writing. The
        // run stays active (and keeps other restores of the service out)
        // until the node says how it ended.
        Err(ExternalServiceError::AgentUnanswered { detail, .. }) => {
            warn!(
                run_id,
                service_id = target.id,
                node_id,
                %detail,
                "the node did not answer a WAL-G restore; waiting for its outcome there"
            );
            let operation_id = operation_id(run_id);
            await_unanswered_restore(&OUTCOME_WAIT, run_id, node_id, || {
                mgr.walg_restore_worker_postgres_outcome(target.id, &operation_id)
            })
            .await?
        }
        Err(e) => {
            return Err(RestoreError::ExternalService {
                reason: format!("restore run {run_id} on node {node_id}: {e}"),
            })
        }
    };
    settle(
        db,
        enc,
        run_id,
        node_id,
        target.id,
        origin_credentials.as_ref(),
        outcome,
    )
    .await
}

/// What a run makes of its node's restore outcome: a completed restore
/// patches the target's stored login to the restored one and enters
/// verification; a refused or failed one fails the run.
async fn settle(
    db: &Arc<DatabaseConnection>,
    enc: &Arc<temps_core::EncryptionService>,
    run_id: i32,
    node_id: i32,
    target_id: i32,
    origin_credentials: Option<&RestoredServiceCredentials>,
    outcome: PostgresWalgRestoreResult,
) -> Result<Option<i32>, RestoreError> {
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
    reconcile_restored_credentials(db, enc, target_id, origin_credentials).await;
    if let Err(e) = update_phase(db.as_ref(), run_id, "verify").await {
        warn!("Restore run {run_id}: recording the verify phase failed: {e}");
    }
    Ok(None)
}

/// Wait for the outcome of a restore whose request went unanswered, asking
/// its node with `ask` as `wait` says. Nothing is written meanwhile: the run
/// stays `running`, so no other destructive restore of the service starts
/// while the node may still be swapping data.
///
/// A restore that ended before it changed the database fails cleanly; one
/// that ended without an outcome after it began writing, that the node no
/// longer knows, or whose node stays unreachable, fails saying the database
/// must be checked before it is restored again.
pub(super) async fn await_unanswered_restore<Ask, AskFut>(
    wait: &OutcomeWait,
    run_id: i32,
    node_id: i32,
    mut ask: Ask,
) -> Result<PostgresWalgRestoreResult, RestoreError>
where
    Ask: FnMut() -> AskFut,
    AskFut: Future<Output = Result<PostgresWalgRestoreProgress, ExternalServiceError>>,
{
    let started = tokio::time::Instant::now();
    let mut unreachable_since: Option<tokio::time::Instant> = None;
    let mut last_error = String::new();
    loop {
        match ask().await {
            Ok(PostgresWalgRestoreProgress::Finished { outcome }) => {
                info!(
                    run_id,
                    node_id,
                    ?outcome,
                    "the node reported the outcome of an unanswered WAL-G restore"
                );
                return Ok(outcome);
            }
            Ok(PostgresWalgRestoreProgress::Ended {
                writes_began: false,
                reason,
            }) => {
                return Err(RestoreError::ExternalService {
                    reason: format!(
                        "restore run {run_id} on node {node_id} stopped before it changed the \
                         database, which is unchanged: {reason}"
                    ),
                })
            }
            Ok(PostgresWalgRestoreProgress::Ended {
                writes_began: true,
                reason,
            }) => {
                return Err(RestoreError::ExternalService {
                    reason: format!(
                        "restore run {run_id} on node {node_id} stopped after it began writing to \
                         the database, without an outcome: {reason}. The database may be \
                         partially restored; check it before restoring again"
                    ),
                })
            }
            Ok(PostgresWalgRestoreProgress::Unknown) => {
                return Err(RestoreError::ExternalService {
                    reason: format!(
                        "restore run {run_id}: node {node_id} did not answer the restore and has \
                         no record of it (its agent may have restarted). The database may have \
                         been changed; check it before restoring again"
                    ),
                })
            }
            Ok(PostgresWalgRestoreProgress::Running { writes_began }) => {
                unreachable_since = None;
                tracing::debug!(
                    run_id,
                    node_id,
                    writes_began,
                    "an unanswered WAL-G restore is still running"
                );
            }
            Err(error) => {
                let since = *unreachable_since.get_or_insert_with(tokio::time::Instant::now);
                last_error = error.to_string();
                warn!(
                    run_id,
                    node_id,
                    error = %last_error,
                    "could not ask node for the outcome of an unanswered WAL-G restore; retrying"
                );
                if since.elapsed() >= wait.unreachable_for {
                    return Err(RestoreError::ExternalService {
                        reason: format!(
                            "restore run {run_id}: node {node_id} did not answer the restore, and \
                             could not be asked for its outcome for {}s ({last_error}). The \
                             restore may still be running there and the database may be \
                             partially restored; check it before restoring again",
                            wait.unreachable_for.as_secs()
                        ),
                    });
                }
            }
        }
        if started.elapsed() >= wait.max_wait {
            return Err(RestoreError::ExternalService {
                reason: format!(
                    "restore run {run_id}: node {node_id} reported no outcome for the restore \
                     within {}s{}. The restore may still be running there; check the database \
                     before restoring again",
                    wait.max_wait.as_secs(),
                    if last_error.is_empty() {
                        String::new()
                    } else {
                        format!(" (last error: {last_error})")
                    }
                ),
            });
        }
        tokio::time::sleep(wait.interval).await;
    }
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

    // ---- A restore whose request went unanswered

    const FAST: OutcomeWait = OutcomeWait {
        interval: Duration::from_millis(5),
        unreachable_for: Duration::from_millis(60),
        max_wait: Duration::from_secs(30),
    };

    /// The node's answers to the outcome asks, in order; the last repeats.
    fn answers(
        answers: Vec<Result<PostgresWalgRestoreProgress, ExternalServiceError>>,
    ) -> impl FnMut() -> std::future::Ready<Result<PostgresWalgRestoreProgress, ExternalServiceError>>
    {
        let answers = std::sync::Mutex::new(std::collections::VecDeque::from(answers));
        move || {
            let mut answers = answers.lock().unwrap();
            let answer = if answers.len() > 1 {
                answers.pop_front().unwrap()
            } else {
                match answers.front().unwrap() {
                    Ok(progress) => Ok(progress.clone()),
                    Err(_) => Err(unreachable()),
                }
            };
            std::future::ready(answer)
        }
    }

    fn unreachable() -> ExternalServiceError {
        ExternalServiceError::AgentUnanswered {
            node: "worker-2".to_string(),
            route: "/agent/services/postgres/walg-restore/outcome".to_string(),
            detail: "operation timed out".to_string(),
        }
    }

    /// The restore's own request went unanswered before it began writing:
    /// the node's agent stopped it at its deadline, and the run fails at
    /// once, saying the database is unchanged.
    #[tokio::test]
    async fn an_unanswered_restore_that_never_wrote_fails_cleanly() {
        let error = await_unanswered_restore(
            &FAST,
            41,
            2,
            answers(vec![Ok(PostgresWalgRestoreProgress::Ended {
                writes_began: false,
                reason: "WAL-G restore of 'orders' passed its deadline".to_string(),
            })]),
        )
        .await
        .expect_err("ended without an outcome");
        let message = error.to_string();
        assert!(message.contains("restore run 41 on node 2"), "{message}");
        assert!(message.contains("which is unchanged"), "{message}");
        assert!(message.contains("passed its deadline"), "{message}");
    }

    /// Running answers are waited through, an unreachable node in between
    /// is retried, and the node's final outcome is what the run gets.
    #[tokio::test]
    async fn an_unanswered_restore_is_waited_for_until_the_node_reports_it() {
        let outcome = await_unanswered_restore(
            &FAST,
            41,
            2,
            answers(vec![
                Ok(PostgresWalgRestoreProgress::Running { writes_began: true }),
                Err(unreachable()),
                Ok(PostgresWalgRestoreProgress::Running { writes_began: true }),
                Ok(PostgresWalgRestoreProgress::Finished {
                    outcome: PostgresWalgRestoreResult::Failed {
                        reason: "the original database was put back unchanged".to_string(),
                    },
                }),
            ]),
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            PostgresWalgRestoreResult::Failed {
                reason: "the original database was put back unchanged".to_string()
            }
        );
    }

    /// Ended after writing, unknown to the node, or a node unreachable past
    /// the limit: each fails, saying the database must be checked.
    #[tokio::test]
    async fn an_unanswered_restore_without_a_known_outcome_says_to_check_the_database() {
        let cases = [
            (
                vec![Ok(PostgresWalgRestoreProgress::Ended {
                    writes_began: true,
                    reason: "the engine stopped".to_string(),
                })],
                "may be partially restored",
            ),
            (
                vec![Ok(PostgresWalgRestoreProgress::Unknown)],
                "has no record of it",
            ),
            (
                vec![
                    Ok(PostgresWalgRestoreProgress::Running { writes_began: true }),
                    Err(unreachable()),
                ],
                "could not be asked for its outcome",
            ),
        ];
        for (answered, expected) in cases {
            let message = await_unanswered_restore(&FAST, 41, 2, answers(answered))
                .await
                .expect_err("no known outcome")
                .to_string();
            assert!(message.contains(expected), "{expected}: {message}");
            assert!(message.contains("before restoring again"), "{message}");
        }
    }

    /// A node that keeps reporting the restore as running is given up on
    /// after the wait's limit, not waited for forever.
    #[tokio::test]
    async fn an_unanswered_restore_is_not_waited_for_past_the_limit() {
        let wait = OutcomeWait {
            max_wait: Duration::from_millis(40),
            ..FAST
        };
        let message = await_unanswered_restore(
            &wait,
            41,
            2,
            answers(vec![Ok(PostgresWalgRestoreProgress::Running {
                writes_began: true,
            })]),
        )
        .await
        .expect_err("past the limit")
        .to_string();
        assert!(message.contains("reported no outcome"), "{message}");
    }

    struct RunFixture {
        _test_db: temps_test_utils::TestDatabase,
        db: Arc<DatabaseConnection>,
        enc: Arc<temps_core::EncryptionService>,
        user_id: i32,
        service_id: i32,
        backup_id: i32,
    }

    /// A PostgreSQL with its stored login, a backup of another service and
    /// the user who restores it, in a real database (settling a run reads
    /// neither the node nor the backup's data). `None` without one.
    async fn run_fixture() -> Option<RunFixture> {
        use sea_orm::{ActiveModelTrait, ActiveValue::Set, NotSet};
        if bollard::Docker::connect_with_local_defaults().is_err() {
            println!("Docker not available, skipping");
            return None;
        }
        let test_db = match temps_test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("TestDatabase unavailable, skipping: {e}");
                return None;
            }
        };
        let db = test_db.db.clone();
        let enc = Arc::new(temps_core::EncryptionService::new_from_password(
            "synthetic-restore-test-key",
        ));
        let user = temps_entities::users::ActiveModel {
            name: Set("Restore Tester".to_string()),
            email: Set("restore-tester@example.com".to_string()),
            password_hash: Set(Some("hash".to_string())),
            email_verified: Set(true),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert user");
        let source = temps_entities::s3_sources::ActiveModel {
            name: Set("restore-source".to_string()),
            bucket_name: Set("backups".to_string()),
            bucket_path: Set("/".to_string()),
            access_key_id: Set(String::new()),
            secret_key: Set(String::new()),
            region: Set("us-east-1".to_string()),
            force_path_style: Set(Some(true)),
            is_default: Set(true),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert S3 source");
        let config = serde_json::json!({
            "host": "localhost",
            "port": "5432",
            "username": "orders_owner",
            "password": "target-password",
        })
        .to_string();
        let service = temps_entities::external_services::ActiveModel {
            name: Set("orders".to_string()),
            service_type: Set("postgres".to_string()),
            status: Set("running".to_string()),
            topology: Set("standalone".to_string()),
            config: Set(Some(enc.encrypt_string(&config).unwrap())),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert service");
        let backup = temps_entities::backups::ActiveModel {
            id: NotSet,
            name: Set("nightly".to_string()),
            backup_id: Set(uuid::Uuid::new_v4().to_string()),
            schedule_id: Set(None),
            schedule_run_id: Set(None),
            backup_type: Set("full".to_string()),
            state: Set("completed".to_string()),
            started_at: Set(Utc::now()),
            finished_at: Set(Some(Utc::now())),
            s3_source_id: Set(source.id),
            s3_location: Set("s3://backups/external_services/postgres/billing/walg".to_string()),
            compression_type: Set("none".to_string()),
            created_by: Set(user.id),
            tags: Set("[]".to_string()),
            size_bytes: Set(None),
            file_count: Set(None),
            error_message: Set(None),
            expires_at: Set(None),
            checksum: Set(None),
            metadata: Set("{}".to_string()),
        }
        .insert(db.as_ref())
        .await
        .expect("insert backup");
        Some(RunFixture {
            _test_db: test_db,
            db,
            enc,
            user_id: user.id,
            service_id: service.id,
            backup_id: backup.id,
        })
    }

    impl RunFixture {
        async fn start_in_place(
            &self,
        ) -> Result<temps_entities::restore_runs::Model, RestoreError> {
            use sea_orm::{ActiveValue::Set, NotSet};
            insert_restore_run(
                self.db.as_ref(),
                temps_entities::restore_runs::ActiveModel {
                    id: NotSet,
                    source_backup_id: Set(self.backup_id),
                    source_service_id: Set(self.service_id),
                    target_service_id: Set(None),
                    target_service_name: Set(None),
                    mode: Set("in_place".to_string()),
                    status: Set("running".to_string()),
                    phase: Set("restore".to_string()),
                    recovery_target: Set(None),
                    parameter_overrides: Set(serde_json::json!({})),
                    resume_token: Set(None),
                    log_id: Set(uuid::Uuid::new_v4().to_string()),
                    error_message: Set(None),
                    attempt: Set(1),
                    started_at: Set(Some(Utc::now())),
                    finished_at: Set(None),
                    created_by: Set(self.user_id),
                    created_at: NotSet,
                    updated_at: NotSet,
                    cancel_requested_at: Set(None),
                    cancel_requested_by: Set(None),
                },
                Some(self.backup_id),
                self.service_id,
                true,
            )
            .await
        }

        async fn run_status(&self, run_id: i32) -> String {
            temps_entities::restore_runs::Entity::find_by_id(run_id)
                .one(self.db.as_ref())
                .await
                .unwrap()
                .unwrap()
                .status
        }

        async fn stored_login(&self) -> serde_json::Value {
            let service = temps_entities::external_services::Entity::find_by_id(self.service_id)
                .one(self.db.as_ref())
                .await
                .unwrap()
                .unwrap();
            let config = self.enc.decrypt_string(&service.config.unwrap()).unwrap();
            let config: serde_json::Value = serde_json::from_str(&config).unwrap();
            serde_json::json!({
                "username": config["username"],
                "password": config["password"],
            })
        }

        /// Settle `run_id` from the node's answers, as `run` does after an
        /// unanswered request, in its own task like the restore's.
        fn settle_unanswered(
            &self,
            run_id: i32,
            progress: tokio::sync::watch::Receiver<PostgresWalgRestoreProgress>,
        ) -> tokio::task::JoinHandle<Result<Option<i32>, RestoreError>> {
            let db = Arc::clone(&self.db);
            let enc = Arc::clone(&self.enc);
            let service_id = self.service_id;
            tokio::spawn(async move {
                // The backup is another service's: the restored data answers
                // to its origin's login.
                let origin = RestoredServiceCredentials::from_origin(
                    "postgres",
                    &serde_json::json!({"username": "billing_owner", "password": "origin-password"}),
                );
                let outcome = await_unanswered_restore(&FAST, run_id, 2, || {
                    std::future::ready(Ok(progress.borrow().clone()))
                })
                .await?;
                settle(&db, &enc, run_id, 2, service_id, origin.as_ref(), outcome).await
            })
        }
    }

    /// While the node still restores (past its write boundary) after the
    /// request went unanswered, the run stays `running` and a second
    /// destructive restore of the service is refused. The node's later
    /// success completes the run and patches the stored login to the
    /// restored one; the service is free again after.
    #[tokio::test]
    async fn an_unanswered_restore_that_wrote_holds_the_service_until_it_succeeds() {
        let Some(fixture) = run_fixture().await else {
            return;
        };
        let run = fixture.start_in_place().await.expect("first restore");
        let (report, progress) =
            tokio::sync::watch::channel(PostgresWalgRestoreProgress::Running {
                writes_began: true,
            });
        let settling = fixture.settle_unanswered(run.id, progress);

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!settling.is_finished(), "waits for the node's outcome");
        assert_eq!(fixture.run_status(run.id).await, "running");
        match fixture.start_in_place().await {
            Err(RestoreError::RestoreAlreadyActive {
                service_id,
                restore_run_id,
            }) => {
                assert_eq!(service_id, fixture.service_id);
                assert_eq!(restore_run_id, run.id);
            }
            other => panic!("expected RestoreAlreadyActive, got {other:?}"),
        }
        assert_eq!(
            fixture.stored_login().await,
            serde_json::json!({"username": "orders_owner", "password": "target-password"}),
            "the login is not changed while the outcome is unknown"
        );

        report
            .send(PostgresWalgRestoreProgress::Finished {
                outcome: PostgresWalgRestoreResult::Completed,
            })
            .unwrap();
        let result = settling.await.unwrap();
        assert!(result.is_ok(), "{result:?}");
        persist_terminal_state(fixture.db.as_ref(), run.id, &result, TERMINAL_WRITE_RETRY)
            .await
            .unwrap();
        assert_eq!(fixture.run_status(run.id).await, "completed");
        assert_eq!(
            fixture.stored_login().await,
            serde_json::json!({"username": "billing_owner", "password": "origin-password"})
        );
        fixture
            .start_in_place()
            .await
            .expect("the service is free once the run completed");
    }

    /// The node's later failure (the original put back) fails the run and
    /// leaves the stored login as it was.
    #[tokio::test]
    async fn an_unanswered_restore_that_wrote_fails_when_the_node_reports_failure() {
        let Some(fixture) = run_fixture().await else {
            return;
        };
        let run = fixture.start_in_place().await.expect("first restore");
        let (report, progress) =
            tokio::sync::watch::channel(PostgresWalgRestoreProgress::Running {
                writes_began: true,
            });
        let settling = fixture.settle_unanswered(run.id, progress);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!settling.is_finished(), "waits for the node's outcome");
        assert!(matches!(
            fixture.start_in_place().await,
            Err(RestoreError::RestoreAlreadyActive { .. })
        ));

        report
            .send(PostgresWalgRestoreProgress::Finished {
                outcome: PostgresWalgRestoreResult::Failed {
                    reason: "recovery did not finish; the original database was put back"
                        .to_string(),
                },
            })
            .unwrap();
        let result = settling.await.unwrap();
        let message = result.as_ref().expect_err("the node failed").to_string();
        assert!(
            message.contains("the original database was put back"),
            "{message}"
        );
        persist_terminal_state(fixture.db.as_ref(), run.id, &result, TERMINAL_WRITE_RETRY)
            .await
            .unwrap();
        assert_eq!(fixture.run_status(run.id).await, "failed");
        assert_eq!(
            fixture.stored_login().await,
            serde_json::json!({"username": "orders_owner", "password": "target-password"})
        );
    }

    /// Unanswered before it began writing: the node stopped it at its
    /// deadline, the run fails at once with the database unchanged, and the
    /// service is free for another restore.
    #[tokio::test]
    async fn an_unanswered_restore_before_writes_fails_and_frees_the_service() {
        let Some(fixture) = run_fixture().await else {
            return;
        };
        let run = fixture.start_in_place().await.expect("first restore");
        let (_report, progress) = tokio::sync::watch::channel(PostgresWalgRestoreProgress::Ended {
            writes_began: false,
            reason: "WAL-G restore of 'orders' passed its deadline".to_string(),
        });
        let result = fixture.settle_unanswered(run.id, progress).await.unwrap();
        let message = result.as_ref().expect_err("ended early").to_string();
        assert!(message.contains("which is unchanged"), "{message}");
        persist_terminal_state(fixture.db.as_ref(), run.id, &result, TERMINAL_WRITE_RETRY)
            .await
            .unwrap();
        assert_eq!(fixture.run_status(run.id).await, "failed");
        assert_eq!(
            fixture.stored_login().await,
            serde_json::json!({"username": "orders_owner", "password": "target-password"})
        );
        fixture
            .start_in_place()
            .await
            .expect("the service is free once the run failed");
    }
}
