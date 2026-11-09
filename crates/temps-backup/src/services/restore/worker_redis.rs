// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! In-place restores of a Redis on a worker node (ADR-099): the provider's
//! own `restore_from_s3`, the one the control plane runs for a local Redis,
//! run on the node by its agent.
//!
//! Admission is the control plane's: the same engine-compatibility and
//! cross-service confirmation as a local restore (in `start_restore`), and
//! the same handling of another service's backup: the target keeps its own
//! Temps bookkeeping (`_temps:*` keys), on either plane. Only the transport
//! differs. Restoring into a new service is not available on a worker yet
//! and is refused at admission, as for a worker PostgreSQL.

use temps_providers::externalsvc::redis::{
    classify_redis_backup_location, RedisBackupLocationKind,
};
use temps_providers::remote_service_client::RedisRestoreResult;
use tracing::{info, warn};

use super::*;

/// Whether `service_type` is a Redis.
pub(super) fn is_redis(service_type: &str) -> bool {
    matches!(service_type, "redis" | "kv")
}

/// Whether a restore into `target` runs here: a standalone Redis on a
/// worker node.
pub(super) fn takes_redis_path(target: &temps_entities::external_services::Model) -> bool {
    crate::engines::workload_job::worker_node(target).is_some()
        && target.topology.as_str() != "cluster"
        && is_redis(&target.service_type)
}

/// Whether an in-place restore of a backup whose origin is `origin` keeps
/// target `target_id`'s own bookkeeping: anything but the target's own
/// backup, an unknown origin included.
pub(super) fn keeps_target_bookkeeping(origin: Option<i32>, target_id: i32) -> bool {
    origin != Some(target_id)
}

/// Refuse what a worker's Redis restore cannot do: a new service, a point
/// in time, or a backup that is neither a WAL-G repository nor an RDB
/// snapshot.
pub(super) fn restore_supported(
    target: &temps_entities::external_services::Model,
    location: &str,
    mode: &RestoreRequestMode,
) -> Result<(), RestoreError> {
    let node = crate::engines::workload_job::worker_node(target).unwrap_or_default();
    if !matches!(mode, RestoreRequestMode::InPlace) {
        return Err(RestoreError::Validation {
            message: format!(
                "Redis service {} ('{}') runs on worker node {node}, where a backup is restored \
                 in place; restoring it as '{}' is not available there yet. Restore in place, \
                 or create the new service first and restore into it",
                target.id,
                target.name,
                mode.as_str()
            ),
        });
    }
    if classify_redis_backup_location(location) == RedisBackupLocationKind::Unsupported {
        return Err(RestoreError::Validation {
            message: format!(
                "Backup '{location}' is neither a Redis WAL-G repository nor an RDB snapshot \
                 (.rdb.gz), so it cannot be restored into Redis service {} ('{}')",
                target.id, target.name
            ),
        });
    }
    Ok(())
}

/// What a worker's Redis restore does, for the plan: the local restore's
/// steps, run on the node.
pub(super) fn steps(
    node: i32,
    location: &str,
    mode: &RestoreRequestMode,
    container: &str,
    cross_service: bool,
) -> Vec<String> {
    let mut steps = vec![format!(
        "Restore on worker node {node}, through its agent, with the control plane's Redis \
         restore, into container {container}"
    )];
    let mut destructive = false;
    let mut errors = Vec::new();
    build_redis_steps(
        classify_restore_strategy("redis", location),
        mode,
        container,
        location,
        &mut steps,
        &mut destructive,
        &mut errors,
    );
    if cross_service && matches!(mode, RestoreRequestMode::InPlace) {
        steps.push(redis_bookkeeping_step());
    }
    steps
}

/// Restore `backup` in place into `target`, a Redis on a worker node, with
/// the provider's restore run by the node's agent.
pub(super) async fn run(
    db: &Arc<DatabaseConnection>,
    scoper: &temps_providers::worker_s3_scope::WorkerCredentialScoper,
    mgr: &Arc<ExternalServiceManager>,
    run_id: i32,
    backup: &temps_entities::backups::Model,
    target: &temps_entities::external_services::Model,
    mode: &RestoreRequestMode,
) -> Result<Option<i32>, RestoreError> {
    use crate::engines::workload_job;
    restore_supported(target, &backup.s3_location, mode)?;
    let node_id = workload_job::worker_node(target).ok_or_else(|| RestoreError::Internal {
        reason: format!(
            "restore run {run_id}: Redis service {} left its worker node",
            target.id
        ),
    })?;
    // Another service's backup was confirmed at `start_restore`, as on the
    // control plane; the target keeps its own bookkeeping, as there.
    let origin = backup_origin_service_id(db.as_ref(), backup).await;
    let keep_bookkeeping = keeps_target_bookkeeping(origin, target.id);
    // The node can read this one backup and nothing else (ADR-093): the
    // WAL-G repository a stream backup is fetched from, or the snapshot's own
    // directory.
    let source =
        super::worker_read_destination(db, scoper, run_id, backup, target.id, node_id, &[]).await?;

    // The last safe point: once the node stops Redis, the run can no
    // longer be cancelled (the node finishes, or puts the original back).
    super::super::restore_cancel::enter_phase_unless_cancelled(db.as_ref(), run_id, "restore")
        .await?;
    info!(
        run_id,
        service_id = target.id,
        node_id,
        backup_id = backup.id,
        keep_target_bookkeeping = keep_bookkeeping,
        "restoring Redis service {} in place on its node",
        target.id
    );
    // A node already running its maximum of backups and restores is waited
    // for, visibly and cancellably, instead of failing the run.
    let outcome = crate::node_busy::retry_while_node_busy(
        &crate::node_busy::NodeBusyPolicy::DEFAULT,
        || {
            mgr.restore_worker_redis(
                target.id,
                source.clone(),
                backup.s3_location.clone(),
                keep_bookkeeping,
            )
        },
        |wait| super::wait_for_node_slot(db.as_ref(), run_id, "restore", wait),
    )
    .await?
    .map_err(|e| RestoreError::ExternalService {
        reason: format!("restore run {run_id} on node {node_id}: {e}"),
    })?;
    match outcome {
        RedisRestoreResult::Completed => {}
        RedisRestoreResult::Refused { reason } => {
            return Err(RestoreError::Validation {
                message: format!("restore run {run_id} on node {node_id} was refused: {reason}"),
            })
        }
        RedisRestoreResult::Failed { reason } => {
            return Err(RestoreError::ExternalService {
                reason: format!("restore run {run_id} on node {node_id}: {reason}"),
            })
        }
    }
    if let Err(e) = update_phase(db.as_ref(), run_id, "verify").await {
        warn!("Restore run {run_id}: recording the verify phase failed: {e}");
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker_redis() -> temps_entities::external_services::Model {
        temps_entities::external_services::Model {
            id: 9,
            name: "cache".into(),
            service_type: "redis".into(),
            version: None,
            status: "running".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            slug: None,
            config: None,
            node_id: Some(2),
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
    fn only_a_standalone_redis_on_a_worker_takes_this_path() {
        let worker = worker_redis();
        assert!(takes_redis_path(&worker));
        assert!(!crate::engines::worker_agent::takes_agent_path(&worker));
        let mut local = worker.clone();
        local.node_id = None;
        assert!(!takes_redis_path(&local));
        let mut control_plane = worker.clone();
        control_plane.node_id = Some(0);
        assert!(!takes_redis_path(&control_plane));
        let mut mongo = worker.clone();
        mongo.service_type = "mongodb".into();
        assert!(!takes_redis_path(&mongo));
    }

    /// Both backup formats are restored in place; a new service, a point in
    /// time and anything that is not a Redis backup are refused with the
    /// remedy.
    #[test]
    fn a_worker_redis_restore_is_in_place_from_either_format() {
        let worker = worker_redis();
        for location in [
            "s3://backups/external_services/redis/cache/walg",
            "external_services/redis/cache/2026/11/08/u/dump.rdb.gz",
        ] {
            assert!(
                restore_supported(&worker, location, &RestoreRequestMode::InPlace).is_ok(),
                "{location}"
            );
        }
        let message = restore_supported(
            &worker,
            "external_services/mongodb/cache/u/dump.archive",
            &RestoreRequestMode::InPlace,
        )
        .expect_err("not a Redis backup")
        .to_string();
        assert!(message.contains("Redis service 9"), "{message}");
        for refused in [
            RestoreRequestMode::NewService {
                name: "copy".into(),
                parameter_overrides: serde_json::json!({}),
            },
            RestoreRequestMode::Pitr {
                to_new_service: false,
                new_service_name: None,
                target: RecoveryTarget::Time { time: Utc::now() },
            },
        ] {
            let message = restore_supported(
                &worker,
                "s3://backups/external_services/redis/cache/walg",
                &refused,
            )
            .expect_err("only in place")
            .to_string();
            assert!(message.contains("worker node 2"), "{message}");
            assert!(message.contains("Restore in place"), "{message}");
        }
    }

    /// The target's own backup restores its bookkeeping with the data;
    /// another's, or one whose origin is unknown, keeps the target's.
    #[test]
    fn only_the_targets_own_backup_brings_its_bookkeeping() {
        assert!(!keeps_target_bookkeeping(Some(9), 9));
        assert!(keeps_target_bookkeeping(Some(4), 9));
        assert!(keeps_target_bookkeeping(None, 9));
    }

    /// The plan is the local restore's, run on the node, with the
    /// bookkeeping step for another service's backup.
    #[test]
    fn the_plan_is_the_local_restore_run_on_the_node() {
        let walg = steps(
            2,
            "s3://backups/external_services/redis/other/walg",
            &RestoreRequestMode::InPlace,
            "redis-cache",
            true,
        )
        .join("\n");
        for expected in [
            "worker node 2",
            "wal-g backup-fetch LATEST",
            "stage the RDB",
            "original is moved back",
            "_temps:*",
        ] {
            assert!(walg.contains(expected), "{expected}: {walg}");
        }
        let own = steps(
            2,
            "external_services/redis/cache/2026/11/08/u/dump.rdb.gz",
            &RestoreRequestMode::InPlace,
            "redis-cache",
            false,
        );
        assert!(own.iter().any(|s| s.contains("Download and decompress")));
        assert!(!own.iter().any(|s| s.contains("_temps:*")));
    }
}
