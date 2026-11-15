// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Restores of a MongoDB on a worker node (ADR-099): the provider's own
//! in-place restore, run on the node by its agent: `wal-g backup-fetch` in
//! the container for a WAL-G repository, a `mongorestore --drop` sidecar for
//! a mongodump archive.
//!
//! Admission is the control plane's: the same engine-compatibility and
//! cross-service confirmation as a local restore (in `start_restore`), and
//! the same credential handling: the origin's password is handed to the
//! engine as its fallback login, and the target's stored configuration is
//! patched to the origin's username and password once the restored
//! `admin.system.users` answers to them. Only
//! the transport differs. Restoring into a new service is not available on a
//! worker yet and is refused at admission; MongoDB has no point-in-time
//! recovery anywhere.

use temps_providers::externalsvc::RestoreCapabilities;
use temps_providers::remote_service_client::WorkerRestoreEngine;
use tracing::info;

use super::*;

/// Whether a restore into `target` runs here: a standalone MongoDB on a
/// worker node.
pub(super) fn takes_path(target: &temps_entities::external_services::Model) -> bool {
    crate::engines::workload_job::worker_node(target).is_some()
        && target.topology.as_str() != "cluster"
        && matches!(target.service_type.as_str(), "mongodb" | "mongo")
}

/// What a worker MongoDB can restore: in place, from a WAL-G repository or a
/// mongodump archive, its own backup or another service's. The same as a
/// local MongoDB except a new service, which a worker cannot provision yet.
pub(super) fn capabilities() -> RestoreCapabilities {
    RestoreCapabilities {
        restore_in_place: true,
        restore_to_new_service: false,
        pitr: false,
        earliest_pitr_time: None,
        latest_pitr_time: None,
    }
}

/// Refuse what a worker's MongoDB restore cannot do: anything but in place.
pub(super) fn supported(
    target: &temps_entities::external_services::Model,
    mode: &RestoreRequestMode,
) -> Result<(), RestoreError> {
    if matches!(mode, RestoreRequestMode::InPlace) {
        return Ok(());
    }
    let node = crate::engines::workload_job::worker_node(target).unwrap_or_default();
    Err(RestoreError::Validation {
        message: format!(
            "MongoDB service {} ('{}') runs on worker node {node}, where a backup is restored in \
             place; restore mode '{}' is not available there yet. Restore in place, or create \
             the new service first and restore into it",
            target.id,
            target.name,
            mode.as_str()
        ),
    })
}

/// What a worker's MongoDB restore does, for the plan: the control plane's
/// own steps, run on the node.
pub(super) fn steps(
    node: i32,
    strategy: &str,
    mode: &RestoreRequestMode,
    container: &str,
    location: &str,
) -> (Vec<String>, Vec<String>) {
    let mut steps = vec![format!(
        "Restore on worker node {node}, through its agent, with the control plane's MongoDB \
         restore, into container {container}"
    )];
    let mut destructive = false;
    let mut errors = Vec::new();
    // The warning only concerns a new service, which is refused here.
    let _ = build_mongodb_steps(
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

/// Restore `backup` in place into `target`, a MongoDB on a worker node.
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
    supported(target, mode)?;
    let node_id = workload_job::worker_node(target).ok_or_else(|| RestoreError::Internal {
        reason: format!(
            "restore run {run_id}: MongoDB service {} left its worker node",
            target.id
        ),
    })?;
    if backup.s3_location.trim().is_empty() {
        return Err(RestoreError::Validation {
            message: format!(
                "Backup {} has no location to restore MongoDB service {} from",
                backup.id, target.id
            ),
        });
    }
    // The restore streams into the live mongod, which answers to the
    // target's password until the restored `admin.system.users` lands: the
    // origin's goes to the engine only as its fallback login, and the stored
    // configuration is patched to it afterwards, as on the control plane.
    let origin_credentials =
        capture_origin_credentials(db, enc, mgr, run_id, backup, target).await?;
    let origin_password = origin_credentials.as_ref().and_then(|c| c.password.clone());
    // The node can read this one backup and nothing else (ADR-102): the
    // WAL-G repository a stream backup is fetched from, or the archive's own
    // directory.
    let destination =
        super::worker_read_destination(db, scoper, run_id, backup, target.id, node_id, &[]).await?;

    // The last safe point: once the node starts writing, the run can no
    // longer be cancelled.
    super::super::restore_cancel::enter_phase_unless_cancelled(db.as_ref(), run_id, "restore")
        .await?;
    info!(
        run_id,
        service_id = target.id,
        node_id,
        backup_id = backup.id,
        location = %backup.s3_location,
        "restoring MongoDB service {} in place on its node",
        target.id
    );
    // A node already running its maximum of backups and restores is waited
    // for, visibly and cancellably, instead of failing the run.
    // A try after a wait gets a credential minted after it.
    let answered = crate::node_busy::retry_while_node_busy_with_fresh_credential(
        &crate::node_busy::NodeBusyPolicy::DEFAULT,
        destination,
        || super::worker_read_destination(db, scoper, run_id, backup, target.id, node_id, &[]),
        |destination| {
            mgr.mongodb_restore_worker(
                target.id,
                origin_password.clone(),
                destination,
                backup.s3_location.clone(),
                Some(super::worker_outcome::operation_id(run_id)),
            )
        },
        |wait| super::wait_for_node_slot(db.as_ref(), run_id, "restore", wait),
    )
    .await?;
    super::worker_outcome::WorkerRestoreRun {
        db,
        run_id,
        node_id,
        target_id: target.id,
        engine: WorkerRestoreEngine::Mongodb,
        reconcile: origin_credentials.as_ref().map(|origin| (enc, origin)),
    }
    .finish(mgr, answered)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mongodb(node_id: Option<i32>) -> temps_entities::external_services::Model {
        temps_entities::external_services::Model {
            id: 12,
            name: "orders".to_string(),
            service_type: "mongodb".to_string(),
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
            workload_mode: Default::default(),
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
    fn only_a_worker_mongodb_takes_this_path() {
        assert!(takes_path(&mongodb(Some(2))));
        assert!(!takes_path(&mongodb(None)));
        assert!(
            !takes_path(&mongodb(Some(0))),
            "node 0 is the control plane"
        );
        let mut redis = mongodb(Some(2));
        redis.service_type = "redis".to_string();
        assert!(!takes_path(&redis));
    }

    /// The capabilities are a local MongoDB's (no PITR), restore into a new
    /// service aside.
    #[test]
    fn a_worker_mongodb_restores_in_place_without_pitr() {
        let capabilities = capabilities();
        assert!(capabilities.restore_in_place);
        assert!(!capabilities.pitr);
        assert!(!capabilities.restore_to_new_service);
        assert!(supported(&mongodb(Some(2)), &RestoreRequestMode::InPlace).is_ok());
        let refused = supported(
            &mongodb(Some(2)),
            &RestoreRequestMode::NewService {
                name: "copy".into(),
                parameter_overrides: serde_json::json!({}),
            },
        )
        .expect_err("a new service is refused")
        .to_string();
        assert!(refused.contains("worker node 2"), "{refused}");
        assert!(refused.contains("Restore in place"), "{refused}");
    }

    /// The plan is the control plane's own MongoDB steps, run on the node.
    #[test]
    fn the_plan_is_the_local_steps_on_the_node() {
        let (walg, errors) = steps(
            2,
            "walg_restore",
            &RestoreRequestMode::InPlace,
            "temps-mongodb-orders",
            "s3://b/x/walg",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let described = walg.join("\n");
        for expected in [
            "worker node 2",
            "control plane's MongoDB restore",
            "wal-g backup-fetch LATEST",
            "admin.system.users",
        ] {
            assert!(described.contains(expected), "{expected}: {described}");
        }
        let (archive, _) = steps(
            2,
            "mongodump_restore",
            &RestoreRequestMode::InPlace,
            "temps-mongodb-orders",
            "x/dump.archive",
        );
        assert!(archive
            .iter()
            .any(|s| s.contains("mongorestore --archive --gzip --drop")));
    }
}
