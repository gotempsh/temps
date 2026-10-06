// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Startup reconciliation for restore runs whose worker died with the
//! previous Temps process.
//!
//! A restore run is driven by a detached in-process task; only that task
//! writes a terminal status. If Temps restarts mid-restore the row stays
//! `running` forever, and for an in-place restore it keeps holding the
//! one-active-restore-per-service index, so every later restore fails.
//!
//! At boot, before the API serves requests, [`active_restore_run_ids`]
//! snapshots the runs that are still `pending`/`running` — by construction
//! none of them has an owner in the new process. For each, reconciliation:
//!
//! 1. **Fences** the restore: stops any restore helper container that is
//!    still writing into the target (see
//!    [`temps_providers::externalsvc::restore_helper`]). If that cannot be
//!    confirmed (Docker unreachable, stop failed) the run is left active and
//!    retried later, because releasing it would let a second restore race
//!    the survivor.
//! 2. Marks it **`interrupted`** — a distinct terminal status, not `failed` —
//!    keeping its last phase, stamping `finished_at`, and explaining that the
//!    target may be partially restored.
//!
//! Nothing is replayed or rolled back: the operator decides what to do with
//! a possibly half-restored database. The update is conditional on the row
//! still being active, so repeated reconciliation (several restarts in a
//! row, or a retry of deferred runs) is idempotent. Only the snapshotted
//! ids are ever touched, so a restore started by this process after boot
//! can never be mistaken for an orphan.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder};
use temps_providers::externalsvc::restore_helper::{
    fence_restore_helpers, RestoreFenceError, RestoreFenceReport,
};
use tracing::{error, info, warn};

use super::restore::{engine_container_name, RestoreError, ACTIVE_RESTORE_STATUSES};

/// Terminal status for a run whose worker died with a previous process.
pub const INTERRUPTED_STATUS: &str = "interrupted";

/// Stops restore helpers that may still be writing into a target container.
#[async_trait]
pub trait RestoreHelperFence: Send + Sync {
    /// `Ok` means no helper for `target_container` can still be running.
    async fn fence(&self, target_container: &str) -> Result<RestoreFenceReport, RestoreFenceError>;
}

/// Production fence backed by this process's Docker handle.
pub struct DockerRestoreFence {
    docker: Arc<temps_core::DockerHandle>,
}

impl DockerRestoreFence {
    pub fn new(docker: Arc<temps_core::DockerHandle>) -> Self {
        Self { docker }
    }
}

#[async_trait]
impl RestoreHelperFence for DockerRestoreFence {
    async fn fence(&self, target_container: &str) -> Result<RestoreFenceReport, RestoreFenceError> {
        match self.docker.get() {
            Some(docker) => fence_restore_helpers(docker, target_container).await,
            // Restores drive local containers, so a process started without
            // a Docker daemon cannot have launched a helper to fence.
            None => Ok(RestoreFenceReport::default()),
        }
    }
}

/// Outcome of one reconciliation pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreReconcileReport {
    /// Runs now marked `interrupted`.
    pub interrupted: Vec<i32>,
    /// Runs left active because their helpers could not be fenced; retry them.
    pub deferred: Vec<i32>,
}

/// Ids of every run still `pending`/`running`. Call once at boot, before
/// the API accepts restores: at that moment none of them has an owner.
/// Bounded by the number of active runs, never by restore history.
pub async fn active_restore_run_ids(db: &DatabaseConnection) -> Result<Vec<i32>, RestoreError> {
    use temps_entities::restore_runs::{Column, Entity};
    Ok(Entity::find()
        .filter(Column::Status.is_in(ACTIVE_RESTORE_STATUSES))
        .order_by_asc(Column::Id)
        .all(db)
        .await?
        .into_iter()
        .map(|run| run.id)
        .collect())
}

/// Fence and mark as interrupted each of `run_ids` that is still active.
pub async fn reconcile_interrupted_restores(
    db: &DatabaseConnection,
    run_ids: &[i32],
    fence: &dyn RestoreHelperFence,
) -> Result<RestoreReconcileReport, RestoreError> {
    use temps_entities::restore_runs::{Column, Entity};

    let mut report = RestoreReconcileReport::default();
    if run_ids.is_empty() {
        return Ok(report);
    }

    let runs = Entity::find()
        .filter(Column::Id.is_in(run_ids.iter().copied()))
        .filter(Column::Status.is_in(ACTIVE_RESTORE_STATUSES))
        .order_by_asc(Column::Id)
        .all(db)
        .await?;
    if runs.is_empty() {
        return Ok(report);
    }

    let service_ids: BTreeSet<i32> = runs.iter().map(|run| run.source_service_id).collect();
    let services: BTreeMap<i32, temps_entities::external_services::Model> =
        temps_entities::external_services::Entity::find()
            .filter(temps_entities::external_services::Column::Id.is_in(service_ids))
            .all(db)
            .await?
            .into_iter()
            .map(|service| (service.id, service))
            .collect();

    for run in runs {
        let service = services.get(&run.source_service_id);
        let targets = fence_targets(&run, service);

        let mut stopped = 0usize;
        let mut fence_failure = None;
        for target in &targets {
            match fence.fence(target).await {
                Ok(fenced) => stopped += fenced.stopped.len(),
                Err(e) => {
                    fence_failure = Some(e);
                    break;
                }
            }
        }
        if let Some(e) = fence_failure {
            warn!(
                restore_run_id = run.id,
                service_id = run.source_service_id,
                phase = %run.phase,
                "Restore run left active: its restore helpers could not be fenced \
                 after a restart, so releasing it could race a surviving helper. \
                 Will retry: {}",
                e
            );
            report.deferred.push(run.id);
            continue;
        }

        let message = interrupted_message(&run, stopped);
        let now = Utc::now();
        let updated = Entity::update_many()
            .col_expr(Column::Status, Expr::value(INTERRUPTED_STATUS))
            .col_expr(Column::ErrorMessage, Expr::value(Some(message)))
            .col_expr(Column::FinishedAt, Expr::value(now))
            .col_expr(Column::UpdatedAt, Expr::value(now))
            .filter(Column::Id.eq(run.id))
            .filter(Column::Status.is_in(ACTIVE_RESTORE_STATUSES))
            .exec(db)
            .await;
        match updated {
            Ok(outcome) if outcome.rows_affected > 0 => {
                info!(
                    restore_run_id = run.id,
                    service_id = run.source_service_id,
                    phase = %run.phase,
                    stopped_helpers = stopped,
                    "Marked restore run interrupted: its worker did not survive a restart"
                );
                report.interrupted.push(run.id);
            }
            Ok(_) => {
                // Settled by someone else since the snapshot; nothing to do.
            }
            Err(e) => {
                error!(
                    restore_run_id = run.id,
                    "Failed to mark restore run interrupted; will retry: {}", e
                );
                report.deferred.push(run.id);
            }
        }
    }

    Ok(report)
}

/// Containers whose restore helpers must be fenced for `run`.
///
/// An in-place restore (or PITR in place) writes into the target service's
/// own container. A restore to a new service writes into the new container
/// only, and the source container is deliberately left alone: a later
/// in-place restore on the source may legitimately be running by the time a
/// deferred run is retried.
fn fence_targets(
    run: &temps_entities::restore_runs::Model,
    service: Option<&temps_entities::external_services::Model>,
) -> Vec<String> {
    let Some(service) = service else {
        return Vec::new();
    };
    let engine = service.service_type.to_lowercase();
    match run.target_service_name.as_deref() {
        Some(new_name) if !new_name.trim().is_empty() => {
            vec![engine_container_name(&engine, new_name)]
        }
        _ => vec![engine_container_name(&engine, &service.name)],
    }
}

/// Plain-language description of where a restore stopped.
fn phase_description(phase: &str) -> &str {
    match phase {
        "prepare" => "preparation",
        "provision" => "provisioning of the new service",
        "restore" => "the data restore",
        "recover" => "point-in-time recovery",
        "verify" => "final verification",
        other => other,
    }
}

/// The message stored on an interrupted run. It names the phase, states what
/// may be left behind, and never claims anything was rolled back.
pub(crate) fn interrupted_message(
    run: &temps_entities::restore_runs::Model,
    stopped_helpers: usize,
) -> String {
    let mut message = format!(
        "This restore was interrupted when Temps restarted during {} (phase '{}').",
        phase_description(&run.phase),
        run.phase
    );
    match run.target_service_name.as_deref() {
        Some(new_name) if !new_name.trim().is_empty() => message.push_str(&format!(
            " The source database was not modified, but the new service '{}' may be \
             partially created and was not registered in Temps. Check for a leftover \
             container or volume with that name before retrying.",
            new_name
        )),
        _ => message.push_str(
            " The database may be partially restored. Check its health and data before retrying.",
        ),
    }
    if stopped_helpers > 0 {
        message.push_str(&format!(
            " Temps stopped {} restore helper container(s) that were still running; \
             nothing was rolled back.",
            stopped_helpers
        ));
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use std::sync::Mutex;

    fn run(
        id: i32,
        mode: &str,
        phase: &str,
        target_name: Option<&str>,
    ) -> temps_entities::restore_runs::Model {
        let now = Utc::now();
        temps_entities::restore_runs::Model {
            id,
            source_backup_id: 1,
            source_service_id: 10,
            target_service_id: None,
            target_service_name: target_name.map(String::from),
            mode: mode.to_string(),
            status: "running".to_string(),
            phase: phase.to_string(),
            recovery_target: None,
            parameter_overrides: serde_json::json!({}),
            resume_token: None,
            log_id: format!("log-{id}"),
            error_message: None,
            attempt: 1,
            started_at: Some(now),
            finished_at: None,
            created_by: 1,
            created_at: now,
            updated_at: now,
        }
    }

    fn service(
        id: i32,
        name: &str,
        service_type: &str,
    ) -> temps_entities::external_services::Model {
        let now = Utc::now();
        temps_entities::external_services::Model {
            id,
            name: name.to_string(),
            service_type: service_type.to_string(),
            version: None,
            status: "running".to_string(),
            created_at: now,
            updated_at: now,
            slug: None,
            config: None,
            node_id: None,
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
            ai_data_access: false,
            created_by_user_id: None,
            continuous_archive_s3_source_id: None,
            continuous_archive_pinned_at: None,
        }
    }

    /// Records every container it was asked to fence; fails on demand.
    #[derive(Default)]
    struct RecordingFence {
        calls: Mutex<Vec<String>>,
        fail: bool,
        stop_count: usize,
    }

    #[async_trait]
    impl RestoreHelperFence for RecordingFence {
        async fn fence(
            &self,
            target_container: &str,
        ) -> Result<RestoreFenceReport, RestoreFenceError> {
            if let Ok(mut calls) = self.calls.lock() {
                calls.push(target_container.to_string());
            }
            if self.fail {
                return Err(RestoreFenceError::List {
                    target_container: target_container.to_string(),
                    reason: "daemon unreachable".to_string(),
                });
            }
            Ok(RestoreFenceReport {
                stopped: (0..self.stop_count)
                    .map(|i| format!("helper-{i}"))
                    .collect(),
                removed: Vec::new(),
            })
        }
    }

    fn update_result(rows_affected: u64) -> MockExecResult {
        MockExecResult {
            last_insert_id: 0,
            rows_affected,
        }
    }

    #[test]
    fn message_names_each_phase_and_never_claims_rollback() {
        for (phase, description) in [
            ("prepare", "preparation"),
            ("provision", "provisioning of the new service"),
            ("restore", "the data restore"),
            ("recover", "point-in-time recovery"),
            ("verify", "final verification"),
        ] {
            let message = interrupted_message(&run(1, "in_place", phase, None), 0);
            assert!(
                message.starts_with(&format!(
                    "This restore was interrupted when Temps restarted during {description} (phase '{phase}')."
                )),
                "{message}"
            );
            assert!(message.contains("may be partially restored"), "{message}");
            assert!(message.contains("Check its health and data before retrying"));
            assert!(!message.to_lowercase().contains("rolled back to"));
        }
    }

    #[test]
    fn message_for_new_service_restore_points_at_the_leftover_service() {
        let message =
            interrupted_message(&run(2, "new_service", "provision", Some("orders-copy")), 1);
        assert!(
            message.contains("source database was not modified"),
            "{message}"
        );
        assert!(message.contains("'orders-copy'"), "{message}");
        assert!(message.contains("stopped 1 restore helper"), "{message}");
        assert!(message.contains("nothing was rolled back"), "{message}");
    }

    #[test]
    fn fence_targets_follow_where_the_restore_writes() {
        let svc = service(10, "orders", "postgres");
        assert_eq!(
            fence_targets(&run(1, "in_place", "restore", None), Some(&svc)),
            vec!["postgres-orders".to_string()]
        );
        // PITR in place has no new-service name and writes to the target.
        assert_eq!(
            fence_targets(&run(2, "pitr", "recover", None), Some(&svc)),
            vec!["postgres-orders".to_string()]
        );
        // A new-service restore never fences the source container.
        assert_eq!(
            fence_targets(
                &run(3, "new_service", "provision", Some("orders-copy")),
                Some(&svc)
            ),
            vec!["postgres-orders-copy".to_string()]
        );
        assert!(fence_targets(&run(4, "in_place", "restore", None), None).is_empty());
    }

    #[tokio::test]
    async fn interrupts_runs_at_several_phases_after_fencing_them() {
        let runs = vec![
            run(1, "in_place", "prepare", None),
            run(2, "in_place", "restore", None),
            run(3, "pitr", "recover", None),
            run(4, "in_place", "verify", None),
        ];
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![runs])
            .append_query_results(vec![vec![service(10, "orders", "postgres")]])
            .append_exec_results((0..4).map(|_| update_result(1)))
            .into_connection();
        let fence = RecordingFence::default();

        let report = reconcile_interrupted_restores(&db, &[1, 2, 3, 4], &fence)
            .await
            .expect("reconcile should succeed");

        assert_eq!(report.interrupted, vec![1, 2, 3, 4]);
        assert!(report.deferred.is_empty());
        assert_eq!(fence.calls.lock().map(|c| c.len()).unwrap_or(0), 4);

        let log = db.into_transaction_log();
        // `Debug` escapes the SQL's quotes; undo that so we can match on it.
        let statements: Vec<String> = log
            .iter()
            .map(|t| format!("{t:?}").replace("\\\"", "\""))
            .collect();
        let updates: Vec<&String> = statements
            .iter()
            .filter(|s| s.contains("UPDATE \"restore_runs\""))
            .collect();
        assert_eq!(updates.len(), 4);
        for update in updates {
            // Conditional on still being active, and never touches `phase`.
            assert!(update.contains("interrupted"), "{update}");
            assert!(update.contains("\"status\" IN"), "{update}");
            assert!(!update.contains("\"phase\" ="), "{update}");
        }
    }

    #[tokio::test]
    async fn fence_failure_keeps_the_run_active() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![run(7, "in_place", "restore", None)]])
            .append_query_results(vec![vec![service(10, "orders", "postgres")]])
            .into_connection();
        let fence = RecordingFence {
            fail: true,
            ..Default::default()
        };

        let report = reconcile_interrupted_restores(&db, &[7], &fence)
            .await
            .expect("a fence failure is reported per run, not as an error");

        assert_eq!(report.deferred, vec![7]);
        assert!(report.interrupted.is_empty());
        let log = db.into_transaction_log();
        assert!(
            !log.iter().any(|t| format!("{t:?}").contains("UPDATE")),
            "the active constraint must not be released before fencing succeeds"
        );
    }

    #[tokio::test]
    async fn repeated_reconciliation_is_a_no_op() {
        // Second pass: the runs are no longer active, so the snapshot query
        // returns nothing and no fence or update happens.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results::<temps_entities::restore_runs::Model, _, _>(vec![vec![]])
            .into_connection();
        let fence = RecordingFence::default();

        let report = reconcile_interrupted_restores(&db, &[1, 2], &fence)
            .await
            .expect("reconcile should succeed");

        assert_eq!(report, RestoreReconcileReport::default());
        assert!(fence.calls.lock().map(|c| c.is_empty()).unwrap_or(false));
    }

    #[tokio::test]
    async fn run_settled_concurrently_is_not_reported_interrupted() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![run(5, "in_place", "restore", None)]])
            .append_query_results(vec![vec![service(10, "orders", "postgres")]])
            .append_exec_results([update_result(0)])
            .into_connection();

        let report = reconcile_interrupted_restores(&db, &[5], &RecordingFence::default())
            .await
            .expect("reconcile should succeed");

        assert!(report.interrupted.is_empty());
        assert!(report.deferred.is_empty());
    }

    #[tokio::test]
    async fn empty_snapshot_skips_the_database() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let report = reconcile_interrupted_restores(&db, &[], &RecordingFence::default())
            .await
            .expect("nothing to do");
        assert_eq!(report, RestoreReconcileReport::default());
        assert!(db.into_transaction_log().is_empty());
    }
}
