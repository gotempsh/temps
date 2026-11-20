// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Restores of a MariaDB on a worker node (ADR-099): the control plane's own
//! in-place MariaDB restore (`MariaDbService::restore_in_place_on_this_node`:
//! the staged WAL-G swap with rollback, point-in-time recovery by binary-log
//! replay, a dump applied with the `mariadb` client), run on the node by its
//! agent.
//!
//! Admission is the control plane's: the same engine-compatibility,
//! cross-service confirmation and PITR checks as a local restore (in
//! `start_restore`), and the same credential handling: a physical base of
//! another service brings that service's `mysql` schema, so the restore runs
//! with the origin's login merged in, and the target's stored configuration
//! is patched to it afterwards; a dump carries no accounts and changes
//! neither. Only the transport differs. Restoring into a new service is not
//! available on a worker yet and is refused at admission.

use sea_orm::DatabaseConnection;
use temps_providers::externalsvc::mariadb::MariaDbService;
use temps_providers::externalsvc::RestoreCapabilities;
use temps_providers::remote_service_client::WorkerRestoreEngine;
use tracing::{info, warn};

use super::*;

/// Whether a restore into `target` runs here: a standalone MariaDB on a
/// worker node.
pub(super) fn takes_mariadb_path(target: &temps_entities::external_services::Model) -> bool {
    target.service_type.eq_ignore_ascii_case("mariadb")
        && target.topology.as_str() != "cluster"
        && crate::engines::workload_job::worker_node(target).is_some()
}

/// Refuse what a worker's MariaDB restore cannot do: a new service (in
/// place, or to a point in time in place, only).
pub(super) fn restore_supported(
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
                "MariaDB service {} ('{}') runs on worker node {node}, where a backup is restored \
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

/// The restore modes of a worker MariaDB: those of a MariaDB on the control
/// plane, in place and to a point in time (validated against the chosen
/// base on execute, as there), except into a new service.
pub(super) fn capabilities() -> RestoreCapabilities {
    RestoreCapabilities {
        restore_in_place: true,
        restore_to_new_service: false,
        pitr: true,
        earliest_pitr_time: None,
        latest_pitr_time: None,
    }
}

/// The plan of a worker MariaDB restore: where it runs, then the steps of
/// the same restore on the control plane.
pub(super) fn steps(
    node: i32,
    location: &str,
    mode: &RestoreRequestMode,
    container: &str,
) -> (Vec<String>, Vec<String>) {
    let strategy = if MariaDbService::is_physical_base_backup_location(location) {
        "mariadb_physical_restore"
    } else {
        "mariadb_dump_restore"
    };
    let mut steps = vec![format!(
        "Restore on worker node {node}, through its agent, with the control plane's MariaDB \
         restore, into container {container}"
    )];
    let mut destructive = false;
    let mut errors = Vec::new();
    build_mariadb_steps(
        strategy,
        mode,
        container,
        location,
        &mut steps,
        &mut destructive,
        &mut errors,
    );
    (steps, errors)
}

/// The origin's login a restore of `backup` into `target` runs with, and the
/// credentials the target's stored configuration is patched to afterwards:
/// exactly the local restore's (see `origin_login_merge`).
async fn origin_login(
    db: &DatabaseConnection,
    mgr: &ExternalServiceManager,
    backup: &temps_entities::backups::Model,
    target: &temps_entities::external_services::Model,
) -> (
    Option<serde_json::Map<String, serde_json::Value>>,
    Option<RestoredServiceCredentials>,
) {
    let (preserves, pre_merge) =
        credential_propagation_gates(&target.service_type, &backup.s3_location);
    if !preserves {
        return (None, None);
    }
    let Some(origin_id) = backup_origin_service_id(db, backup).await else {
        return (None, None);
    };
    if origin_id == target.id {
        return (None, None);
    }
    match mgr.get_service_config(origin_id).await {
        Ok(origin) => (
            pre_merge.then(|| origin_login_merge(&origin.parameters)),
            RestoredServiceCredentials::from_origin(&target.service_type, &origin.parameters),
        ),
        Err(e) => {
            warn!(
                "Failed to read origin service {origin_id} config; the restore of backup {} into \
                 service {} keeps the target's credentials, which may not match the restored \
                 data: {e}",
                backup.id, target.id
            );
            (None, None)
        }
    }
}

/// Restore `backup` into `target`, a MariaDB on a worker node, in place or
/// to the point `mode` names.
pub(super) async fn run(
    deps: super::WorkerRestoreDeps<'_>,
    run_id: i32,
    backup: &temps_entities::backups::Model,
    target: &temps_entities::external_services::Model,
    mode: &RestoreRequestMode,
) -> Result<Option<i32>, RestoreError> {
    let super::WorkerRestoreDeps {
        db,
        enc,
        scoper,
        mgr,
    } = deps;
    use crate::engines::workload_job;
    restore_supported(target, mode)?;
    let node_id = workload_job::worker_node(target).ok_or_else(|| RestoreError::Internal {
        reason: format!(
            "restore run {run_id}: MariaDB service {} left its worker node",
            target.id
        ),
    })?;
    let recovery_target = match mode {
        RestoreRequestMode::Pitr { target, .. } => Some(target.clone()),
        _ => None,
    };
    if let Some(recovery_target) = &recovery_target {
        MariaDbService::recovery_target_to_stop_flag(recovery_target).map_err(|e| {
            RestoreError::Validation {
                message: format!("restore run {run_id}: {e}"),
            }
        })?;
        if !MariaDbService::is_physical_base_backup_location(&backup.s3_location) {
            return Err(RestoreError::Validation {
                message: format!(
                    "PITR requires a physical (mariadb-backup) base backup; backup {} at '{}' is \
                     a logical dump and cannot be used for point-in-time recovery",
                    backup.id, backup.s3_location
                ),
            });
        }
    }
    let target_user_data = selected_walg_target_user_data(backup)?;
    let (restored_login, origin_credentials) = origin_login(db, mgr, backup, target).await;
    let s3_source = crate::engines::v2_common::load_s3_source(db.as_ref(), backup.s3_source_id)
        .await
        .map_err(|e| RestoreError::ExternalService {
            reason: format!("restore run {run_id}: {e}"),
        })?;
    if backup.s3_location.starts_with("s3://")
        && !backup
            .s3_location
            .starts_with(&format!("s3://{}/", s3_source.bucket_name))
    {
        return Err(RestoreError::Validation {
            message: format!(
                "Backup {} is at '{}', outside bucket '{}' of S3 source {}",
                backup.id, backup.s3_location, s3_source.bucket_name, backup.s3_source_id
            ),
        });
    }
    let bucket_path = s3_source.bucket_path;
    // A read-only credential for the backup's own location (ADR-102) and,
    // for a point-in-time recovery, the binary logs the replay reads: the
    // same prefix the node derives from the backup's layout, so another
    // service's base replays that service's logs and nothing else.
    let binlogs = match &recovery_target {
        Some(_) => vec![
            temps_providers::externalsvc::mariadb_node::binlog_read_prefix(
                &backup.s3_location,
                &s3_source.bucket_name,
                &bucket_path,
                &target.name,
            ),
        ],
        None => Vec::new(),
    };
    let s3 =
        super::worker_read_destination(db, scoper, run_id, backup, target.id, node_id, &binlogs)
            .await?;

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
        cross_service_login = restored_login.is_some(),
        "restoring MariaDB service {} on its node",
        target.id
    );
    // A node already running its maximum of backups and restores is waited
    // for, visibly and cancellably, instead of failing the run.
    // A try after a wait gets a credential minted after it.
    let answered = crate::node_busy::retry_while_node_busy_with_fresh_credential(
        &crate::node_busy::NodeBusyPolicy::DEFAULT,
        s3,
        || super::worker_read_destination(db, scoper, run_id, backup, target.id, node_id, &binlogs),
        |s3| {
            mgr.mariadb_restore_worker(
                target.id,
                restored_login.as_ref(),
                s3,
                bucket_path.clone(),
                backup.s3_location.clone(),
                recovery_target.clone(),
                target_user_data.clone(),
                Some(super::worker_outcome::operation_id(run_id)),
            )
        },
        |wait| super::wait_for_node_slot(db.as_ref(), run_id, phase, wait),
    )
    .await?;
    super::worker_outcome::WorkerRestoreRun {
        db,
        run_id,
        node_id,
        target_id: target.id,
        engine: WorkerRestoreEngine::Mariadb,
        reconcile: origin_credentials.as_ref().map(|origin| (enc, origin)),
    }
    .finish(mgr, answered)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mariadb(node_id: Option<i32>) -> temps_entities::external_services::Model {
        temps_entities::external_services::Model {
            id: 7,
            name: "shop".to_string(),
            service_type: "mariadb".to_string(),
            version: None,
            status: "running".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
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
            healthcheck_interval_secs: None,
            healthcheck_start_interval_secs: None,
            healthcheck_start_period_secs: None,
            healthcheck_timeout_secs: None,
            healthcheck_retries: None,
        }
    }

    #[test]
    fn a_worker_mariadb_takes_this_path_and_a_local_one_does_not() {
        assert!(takes_mariadb_path(&mariadb(Some(3))));
        assert!(!takes_mariadb_path(&mariadb(None)));
        assert!(!takes_mariadb_path(&mariadb(Some(0))));
        let mut redis = mariadb(Some(3));
        redis.service_type = "redis".to_string();
        assert!(!takes_mariadb_path(&redis));
    }

    /// The capabilities are a local MariaDB's, except restoring into a new
    /// service, which is refused at admission rather than offered.
    #[test]
    fn capabilities_are_the_local_engines_but_a_new_service() {
        let caps = capabilities();
        assert!(caps.restore_in_place && caps.pitr && !caps.restore_to_new_service);
        let target = mariadb(Some(3));
        assert!(restore_supported(&target, &RestoreRequestMode::InPlace).is_ok());
        let pitr = RestoreRequestMode::Pitr {
            target: RecoveryTarget::Time { time: Utc::now() },
            to_new_service: false,
            new_service_name: None,
        };
        assert!(restore_supported(&target, &pitr).is_ok());
        let error = restore_supported(
            &target,
            &RestoreRequestMode::Pitr {
                target: RecoveryTarget::Time { time: Utc::now() },
                to_new_service: true,
                new_service_name: Some("copy".into()),
            },
        )
        .expect_err("new service");
        assert!(error.to_string().contains("worker node 3"), "{error}");
    }

    #[test]
    fn the_plan_is_the_local_restores_on_the_node() {
        let (in_place, errors) = steps(
            3,
            "s3://b/external_services/mariadb/shop/walg",
            &RestoreRequestMode::InPlace,
            "mariadb-shop",
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert!(in_place[0].contains("worker node 3"), "{in_place:?}");
        assert!(
            in_place.iter().any(|step| step.contains("staging")),
            "{in_place:?}"
        );
        let (_, errors) = steps(
            3,
            "external_services/mariadb/shop/2026/11/08/u/dump.sql.gz",
            &RestoreRequestMode::Pitr {
                target: RecoveryTarget::Time { time: Utc::now() },
                to_new_service: false,
                new_service_name: None,
            },
            "mariadb-shop",
        );
        assert!(!errors.is_empty(), "a dump cannot anchor PITR");
    }
}
