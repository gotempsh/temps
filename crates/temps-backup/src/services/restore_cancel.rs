// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cancelling a restore run.
//!
//! A restore can be stopped only while stopping it is safe:
//!
//! - **In place** (and PITR in place): while it is still preparing or
//!   downloading the backup. The worker's move into its write phase is the
//!   last safe point.
//! - **New service** (and PITR to a new service): until the new service is
//!   registered. Everything the run wrote lives in a container and volume the
//!   run itself created, and cancelling tears both down.
//!
//! The database row is the arbiter. A cancellation is recorded with a
//! conditional `UPDATE … WHERE phase IN (<cancellable phases>) AND
//! cancel_requested_at IS NULL`, and every worker move out of a cancellable
//! phase is a conditional `UPDATE … WHERE cancel_requested_at IS NULL`.
//! PostgreSQL serializes the two updates on the row, so exactly one wins:
//! either the cancellation lands first and the worker stops, or the worker
//! moves on first and the API answers `409 restore-not-cancellable`.
//!
//! The in-process [`RestoreCancellations`] registry only makes the worker
//! notice quickly (between download chunks, or by dropping a new-service
//! provision); it never decides the outcome on its own.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use temps_providers::externalsvc::{RestoreCancelled, RestoreGate, ServiceType};
use temps_providers::ExternalServiceManager;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::restore::{RestoreError, ACTIVE_RESTORE_STATUSES};

/// Phases in which a restore that overwrites its target can still be stopped.
const DESTRUCTIVE_CANCELLABLE_PHASES: [&str; 2] = ["prepare", "download"];

/// Phases in which a restore into a service it creates can still be stopped.
const NEW_SERVICE_CANCELLABLE_PHASES: [&str; 4] = ["prepare", "download", "provision", "recover"];

/// Whether a run writes over an existing service's data. Mirrors
/// `RestoreRequestMode::is_destructive`, from the stored columns: PITR in
/// place is the PITR run that names no new service.
pub fn run_is_destructive(mode: &str, target_service_name: Option<&str>) -> bool {
    match mode {
        "in_place" => true,
        "pitr" => target_service_name.is_none_or(|name| name.trim().is_empty()),
        _ => false,
    }
}

/// Phases from which a run with this mode can still be cancelled.
pub fn cancellable_phases(
    mode: &str,
    target_service_name: Option<&str>,
) -> &'static [&'static str] {
    if run_is_destructive(mode, target_service_name) {
        &DESTRUCTIVE_CANCELLABLE_PHASES
    } else {
        &NEW_SERVICE_CANCELLABLE_PHASES
    }
}

/// Why a run cannot be cancelled right now, or `None` when it can.
///
/// The text is shown verbatim in the console's tracking panel and in the
/// `409 restore-not-cancellable` detail, so it says what happened and what
/// the operator can do instead.
pub fn not_cancellable_reason(run: &temps_entities::restore_runs::Model) -> Option<String> {
    if !ACTIVE_RESTORE_STATUSES.contains(&run.status.as_str()) {
        return Some(format!(
            "The restore has already finished with status '{}'.",
            run.status
        ));
    }
    if run.cancel_requested_at.is_some() {
        return Some("Cancellation has already been requested.".to_string());
    }
    if cancellable_phases(&run.mode, run.target_service_name.as_deref())
        .contains(&run.phase.as_str())
    {
        return None;
    }
    Some(
        if run_is_destructive(&run.mode, run.target_service_name.as_deref()) {
            format!(
                "The restore has started writing data to the service (phase '{}'). \
                 Stopping it now would leave the database partially restored, so it \
                 will run to completion.",
                run.phase
            )
        } else {
            format!(
                "The new service is being registered (phase '{}'). Let the restore \
                 finish, then delete the new service if you do not want it.",
                run.phase
            )
        },
    )
}

/// What the run's stored message says after a cancellation.
pub fn cancelled_message(phase: &str, destructive: bool, cleanup: &CancelCleanup) -> String {
    format!(
        "Restore cancelled during phase '{}'. {}",
        phase,
        cleanup_note(destructive, cleanup)
    )
}

/// One sentence saying what a stopped run left behind.
pub fn cleanup_note(destructive: bool, cleanup: &CancelCleanup) -> String {
    match cleanup {
        CancelCleanup::TargetUntouched if destructive => {
            "The service's data was not modified.".to_string()
        }
        CancelCleanup::TargetUntouched => "No new service was created.".to_string(),
        CancelCleanup::NewServiceRemoved { name } => format!(
            "The partially created service '{}' was removed; nothing was left behind.",
            name
        ),
        CancelCleanup::NewServiceLeftBehind { name, reason } => format!(
            "The partially created service '{}' could not be removed ({}). \
             Remove its container and volume before reusing the name.",
            name, reason
        ),
    }
}

/// What a cancellation cleaned up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelCleanup {
    /// Nothing was written that needed removing.
    TargetUntouched,
    /// The half-created new service was torn down.
    NewServiceRemoved { name: String },
    /// Teardown of the half-created new service failed.
    NewServiceLeftBehind { name: String, reason: String },
}

/// Cancellation tokens of the restore workers running in this process.
#[derive(Clone, Default)]
pub struct RestoreCancellations {
    tokens: Arc<Mutex<HashMap<i32, CancellationToken>>>,
}

impl RestoreCancellations {
    /// Register a worker. The returned guard unregisters it on drop, so a
    /// worker that exits on any path never leaves a stale token behind.
    pub fn register(&self, run_id: i32) -> RegisteredRestore {
        let token = CancellationToken::new();
        self.lock().insert(run_id, token.clone());
        RegisteredRestore {
            registry: self.clone(),
            run_id,
            token,
        }
    }

    /// Wake the worker of `run_id`, if it runs in this process. Returns
    /// whether a worker was found.
    pub fn signal(&self, run_id: i32) -> bool {
        match self.lock().get(&run_id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<i32, CancellationToken>> {
        // A poisoned map still holds valid tokens; a panic elsewhere must not
        // disable cancellation for every other restore.
        self.tokens
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// A worker's registration in [`RestoreCancellations`].
pub struct RegisteredRestore {
    registry: RestoreCancellations,
    run_id: i32,
    token: CancellationToken,
}

impl RegisteredRestore {
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }
}

impl Drop for RegisteredRestore {
    fn drop(&mut self) {
        self.registry.lock().remove(&self.run_id);
    }
}

/// Record a cancellation request. `Ok(true)` when this call recorded it;
/// `Ok(false)` when the run was not in a cancellable state (the caller
/// re-reads the row to explain why).
pub async fn record_cancellation<C: ConnectionTrait>(
    conn: &C,
    run: &temps_entities::restore_runs::Model,
    user_id: i32,
) -> Result<bool, RestoreError> {
    use temps_entities::restore_runs::{Column, Entity};
    let now = Utc::now();
    let phases = cancellable_phases(&run.mode, run.target_service_name.as_deref());
    let outcome = Entity::update_many()
        .col_expr(Column::CancelRequestedAt, Expr::value(now))
        .col_expr(Column::CancelRequestedBy, Expr::value(user_id))
        .col_expr(Column::UpdatedAt, Expr::value(now))
        .filter(Column::Id.eq(run.id))
        .filter(Column::Status.is_in(ACTIVE_RESTORE_STATUSES))
        .filter(Column::CancelRequestedAt.is_null())
        .filter(Column::Phase.is_in(phases.iter().copied()))
        .exec(conn)
        .await?;
    Ok(outcome.rows_affected == 1)
}

/// Move a run into `phase` unless a cancellation was recorded first.
///
/// Returns [`RestoreError::RestoreCancelled`] when the cancellation won,
/// carrying the phase the run was in.
pub async fn enter_phase_unless_cancelled<C: ConnectionTrait>(
    conn: &C,
    run_id: i32,
    phase: &str,
) -> Result<(), RestoreError> {
    use temps_entities::restore_runs::{Column, Entity};
    let outcome = Entity::update_many()
        .col_expr(Column::Phase, Expr::value(phase))
        .col_expr(Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(Column::Id.eq(run_id))
        .filter(Column::Status.is_in(ACTIVE_RESTORE_STATUSES))
        .filter(Column::CancelRequestedAt.is_null())
        .exec(conn)
        .await?;
    if outcome.rows_affected == 1 {
        return Ok(());
    }
    let run =
        Entity::find_by_id(run_id)
            .one(conn)
            .await?
            .ok_or(RestoreError::RestoreRunNotFound {
                restore_run_id: run_id,
            })?;
    if run.cancel_requested_at.is_some() {
        return Err(RestoreError::RestoreCancelled {
            restore_run_id: run_id,
            phase: run.phase,
        });
    }
    Err(RestoreError::Internal {
        reason: format!(
            "Restore run {} could not enter phase '{}': it is no longer active (status '{}')",
            run_id, phase, run.status
        ),
    })
}

/// [`enter_phase_unless_cancelled`] that retries transient database errors.
///
/// Used where giving up is expensive: the move into `verify` decides whether
/// a new-service restore registers its service, and a worker that cannot
/// record it must not register (it cannot tell whether a cancellation was
/// accepted meanwhile). A recorded cancellation or a run that is no longer
/// active is returned at once; only database errors are retried.
pub async fn enter_phase_unless_cancelled_retrying<C: ConnectionTrait>(
    conn: &C,
    run_id: i32,
    phase: &str,
    retry: &temps_core::retry::RetryConfig,
) -> Result<(), RestoreError> {
    retry
        .retry(|| async {
            match enter_phase_unless_cancelled(conn, run_id, phase).await {
                Err(RestoreError::Database(e)) => Err(e),
                settled => Ok(settled),
            }
        })
        .await
        .map_err(|e| RestoreError::Internal {
            reason: format!(
                "Restore run {} could not record its move into phase '{}' after retrying: {}",
                run_id, phase, e
            ),
        })?
}

/// The [`RestoreGate`] a worker hands its engine.
///
/// For a destructive restore, `begin_target_writes` performs the guarded move
/// into `write_phase` — the last safe point. For a restore into a new
/// service there is no such point inside the engine (the whole provision is
/// cancellable), so it only reports a pending cancellation.
pub struct WorkerGate<'a, C: ConnectionTrait> {
    conn: &'a C,
    run_id: i32,
    token: CancellationToken,
    destructive: bool,
    write_phase: &'static str,
    target: String,
    writes_started: AtomicBool,
}

impl<'a, C: ConnectionTrait> WorkerGate<'a, C> {
    pub fn new(
        conn: &'a C,
        run_id: i32,
        token: CancellationToken,
        destructive: bool,
        write_phase: &'static str,
        target: String,
    ) -> Self {
        Self {
            conn,
            run_id,
            token,
            destructive,
            write_phase,
            target,
            writes_started: AtomicBool::new(false),
        }
    }

    /// Whether the engine passed the last safe point.
    pub fn writes_started(&self) -> bool {
        self.writes_started.load(Ordering::SeqCst)
    }

    /// Whether this process received a cancellation signal for the run.
    pub fn signalled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// The guarded move into the write phase, shared by engines that honour
    /// the gate and by the orchestrator for engines that do not.
    pub async fn enter_write_phase(&self) -> Result<(), RestoreError> {
        enter_phase_unless_cancelled(self.conn, self.run_id, self.write_phase).await?;
        self.writes_started.store(true, Ordering::SeqCst);
        Ok(())
    }
}

#[async_trait]
impl<C: ConnectionTrait> RestoreGate for WorkerGate<'_, C> {
    fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    async fn begin_target_writes(&self) -> Result<(), RestoreCancelled> {
        if !self.destructive {
            return if self.token.is_cancelled() {
                Err(RestoreCancelled {
                    target: self.target.clone(),
                })
            } else {
                Ok(())
            };
        }
        match self.enter_write_phase().await {
            Ok(()) => Ok(()),
            Err(RestoreError::RestoreCancelled { .. }) => Err(RestoreCancelled {
                target: self.target.clone(),
            }),
            Err(e) => {
                // The safe point could not be recorded, so the write phase
                // must not start: an unrecorded write phase would let a later
                // cancellation be accepted while data is being overwritten.
                warn!(
                    "Restore run {} could not record entering phase '{}'; not writing to {}: {}",
                    self.run_id, self.write_phase, self.target, e
                );
                Err(RestoreCancelled {
                    target: self.target.clone(),
                })
            }
        }
    }
}

/// Removes a new service's container and volume that a cancelled restore
/// created but never registered.
#[async_trait]
pub trait UnpersistedServiceTeardown: Send + Sync {
    async fn remove(&self, name: &str, service_type: ServiceType) -> Result<(), String>;
}

/// Production teardown through the external-service manager, which owns the
/// Docker handle and the engines' container/volume naming.
pub struct ManagerServiceTeardown {
    manager: Arc<ExternalServiceManager>,
}

impl ManagerServiceTeardown {
    pub fn new(manager: Arc<ExternalServiceManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl UnpersistedServiceTeardown for ManagerServiceTeardown {
    async fn remove(&self, name: &str, service_type: ServiceType) -> Result<(), String> {
        let instance = self
            .manager
            .get_service_instance(name.to_string(), service_type)
            .map_err(|e| format!("could not resolve service '{}': {}", name, e))?;
        instance
            .remove()
            .await
            .map_err(|e| format!("removing service '{}' failed: {}", name, e))
    }
}

/// Reports which of the Docker objects a new service would own already exist.
#[async_trait]
pub trait NewServiceResourceProbe: Send + Sync {
    /// One `"container NAME"` / `"volume NAME"` entry per object found. An
    /// error means existence could not be established.
    async fn existing(&self, name: &str, service_type: ServiceType) -> Result<Vec<String>, String>;
}

#[async_trait]
impl NewServiceResourceProbe for ManagerServiceTeardown {
    async fn existing(&self, name: &str, service_type: ServiceType) -> Result<Vec<String>, String> {
        self.manager
            .existing_docker_resources(name, service_type)
            .await
            .map_err(|e| e.to_string())
    }
}

/// Make sure everything a new-service restore will create under `name` is
/// owned by run `run_id` alone, before anything is provisioned.
///
/// Engines derive container and volume names from the service name, and
/// cancelling a new-service restore removes them by that name. That is only
/// safe if none of them existed when the run started and no other active run
/// is creating a service with the same name. Otherwise a cancellation (or a
/// failed provision being cleaned up) would delete data this run never wrote,
/// such as the volume an earlier failed restore left behind. Fails closed:
/// if existence cannot be established, the restore does not start.
pub async fn ensure_new_service_name_unclaimed<C: ConnectionTrait>(
    conn: &C,
    probe: &dyn NewServiceResourceProbe,
    run_id: i32,
    name: &str,
    service_type: ServiceType,
) -> Result<(), RestoreError> {
    use temps_entities::restore_runs::{Column, Entity};
    if let Some(other) = Entity::find()
        .filter(Column::TargetServiceName.eq(name))
        .filter(Column::Status.is_in(ACTIVE_RESTORE_STATUSES))
        .filter(Column::Id.ne(run_id))
        .one(conn)
        .await?
    {
        return Err(RestoreError::Validation {
            message: format!(
                "Restore run {} cannot create service '{}': restore run {} is already creating \
                 a service with that name. Wait for it to finish or choose another name.",
                run_id, name, other.id
            ),
        });
    }
    let existing = probe.existing(name, service_type).await.map_err(|reason| {
        RestoreError::ExternalService {
            reason: format!(
                "Restore run {} cannot confirm that no container or volume already uses the \
                 name '{}', so it did not start: {}",
                run_id, name, reason
            ),
        }
    })?;
    if !existing.is_empty() {
        return Err(RestoreError::Validation {
            message: format!(
                "Restore run {} cannot create service '{}': {} already exist(s), probably left \
                 over from an earlier failed restore. Remove it, or choose another name.",
                run_id,
                name,
                existing.join(", ")
            ),
        });
    }
    Ok(())
}

/// Tear down the half-created service of a cancelled new-service restore.
///
/// Refuses when a registered service already has the name: the engines derive
/// container and volume names from the service name, so removing "the new
/// service" would then remove someone else's data.
pub async fn teardown_cancelled_new_service<C: ConnectionTrait>(
    conn: &C,
    teardown: &dyn UnpersistedServiceTeardown,
    probe: &dyn NewServiceResourceProbe,
    name: &str,
    service_type: ServiceType,
) -> CancelCleanup {
    use temps_entities::external_services::{Column, Entity};
    match Entity::find().filter(Column::Name.eq(name)).one(conn).await {
        Ok(Some(existing)) => {
            return CancelCleanup::NewServiceLeftBehind {
                name: name.to_string(),
                reason: format!(
                    "service {} is registered under the same name, so nothing was removed",
                    existing.id
                ),
            };
        }
        Ok(None) => {}
        Err(e) => {
            return CancelCleanup::NewServiceLeftBehind {
                name: name.to_string(),
                reason: format!(
                    "could not check for a registered service with that name: {}",
                    e
                ),
            };
        }
    }
    match teardown.remove(name, service_type).await {
        // An engine's `remove()` skips objects it did not list and tolerates
        // volume-removal errors, so success is only claimed once Docker
        // confirms nothing under the name is left.
        Ok(()) => match probe.existing(name, service_type).await {
            Ok(left) if left.is_empty() => {
                info!(
                    "Removed partially created service '{}' after a cancelled restore",
                    name
                );
                CancelCleanup::NewServiceRemoved {
                    name: name.to_string(),
                }
            }
            Ok(left) => {
                warn!(
                    "Cancelled restore of '{}' left objects behind after removal: {}",
                    name,
                    left.join(", ")
                );
                CancelCleanup::NewServiceLeftBehind {
                    name: name.to_string(),
                    reason: format!("still present after removal: {}", left.join(", ")),
                }
            }
            Err(reason) => CancelCleanup::NewServiceLeftBehind {
                name: name.to_string(),
                reason: format!("could not confirm it was removed: {}", reason),
            },
        },
        Err(reason) => {
            warn!(
                "Could not remove partially created service '{}' after a cancelled restore: {}",
                name, reason
            );
            CancelCleanup::NewServiceLeftBehind {
                name: name.to_string(),
                reason,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase, MockExecResult};

    fn run(
        mode: &str,
        target_name: Option<&str>,
        status: &str,
        phase: &str,
    ) -> temps_entities::restore_runs::Model {
        let now = Utc::now();
        temps_entities::restore_runs::Model {
            id: 7,
            source_backup_id: 3,
            source_service_id: 11,
            target_service_id: None,
            target_service_name: target_name.map(str::to_string),
            mode: mode.to_string(),
            status: status.to_string(),
            phase: phase.to_string(),
            recovery_target: None,
            parameter_overrides: serde_json::json!({}),
            resume_token: None,
            log_id: "log".to_string(),
            error_message: None,
            attempt: 1,
            started_at: Some(now),
            finished_at: None,
            created_by: 1,
            created_at: now,
            updated_at: now,
            cancel_requested_at: None,
            cancel_requested_by: None,
        }
    }

    fn service(id: i32, name: &str) -> temps_entities::external_services::Model {
        let now = Utc::now();
        temps_entities::external_services::Model {
            id,
            name: name.to_string(),
            service_type: "postgres".to_string(),
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

    #[test]
    fn destructive_runs_are_cancellable_only_before_writing() {
        for (mode, target) in [("in_place", None), ("pitr", None), ("pitr", Some(" "))] {
            assert_eq!(
                cancellable_phases(mode, target),
                &["prepare", "download"],
                "{mode} {target:?}"
            );
            assert!(not_cancellable_reason(&run(mode, target, "running", "download")).is_none());
            let reason = not_cancellable_reason(&run(mode, target, "running", "restore"))
                .expect("writing phase is not cancellable");
            assert!(reason.contains("started writing data"), "{reason}");
        }
    }

    #[test]
    fn new_service_runs_are_cancellable_until_registration() {
        for (mode, target) in [("new_service", Some("copy")), ("pitr", Some("copy"))] {
            for phase in ["prepare", "download", "provision", "recover"] {
                assert!(
                    not_cancellable_reason(&run(mode, target, "running", phase)).is_none(),
                    "{mode} {phase}"
                );
            }
            let reason = not_cancellable_reason(&run(mode, target, "running", "verify"))
                .expect("registration is not cancellable");
            assert!(reason.contains("being registered"), "{reason}");
        }
    }

    #[test]
    fn finished_or_already_cancelled_runs_explain_why() {
        let finished = not_cancellable_reason(&run("in_place", None, "completed", "completed"))
            .expect("finished");
        assert!(finished.contains("'completed'"), "{finished}");

        let mut requested = run("in_place", None, "running", "download");
        requested.cancel_requested_at = Some(Utc::now());
        assert_eq!(
            not_cancellable_reason(&requested).as_deref(),
            Some("Cancellation has already been requested.")
        );
    }

    #[test]
    fn cancelled_message_states_what_was_left_behind() {
        assert!(
            cancelled_message("download", true, &CancelCleanup::TargetUntouched)
                .contains("data was not modified")
        );
        assert!(
            cancelled_message("prepare", false, &CancelCleanup::TargetUntouched)
                .contains("No new service was created")
        );
        assert!(cancelled_message(
            "provision",
            false,
            &CancelCleanup::NewServiceRemoved {
                name: "copy".into()
            }
        )
        .contains("'copy' was removed"));
        let left = cancelled_message(
            "provision",
            false,
            &CancelCleanup::NewServiceLeftBehind {
                name: "copy".into(),
                reason: "docker down".into(),
            },
        );
        assert!(
            left.contains("could not be removed (docker down)"),
            "{left}"
        );
    }

    #[test]
    fn registry_signals_registered_workers_and_forgets_them_on_drop() {
        let registry = RestoreCancellations::default();
        let registration = registry.register(5);
        let token = registration.token();

        assert!(!registry.signal(6), "unknown runs are not signalled");
        assert!(registry.signal(5));
        assert!(token.is_cancelled());

        drop(registration);
        assert!(!registry.signal(5), "a finished worker is unregistered");
    }

    #[tokio::test]
    async fn record_cancellation_reports_whether_the_conditional_update_won() {
        let won = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        assert!(
            record_cancellation(&won, &run("in_place", None, "running", "download"), 1)
                .await
                .expect("update")
        );

        let lost = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();
        assert!(
            !record_cancellation(&lost, &run("in_place", None, "running", "restore"), 1)
                .await
                .expect("update")
        );
    }

    #[tokio::test]
    async fn guarded_phase_move_loses_to_a_recorded_cancellation() {
        let mut cancelled = run("in_place", None, "running", "download");
        cancelled.cancel_requested_at = Some(Utc::now());
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_results([vec![cancelled]])
            .into_connection();

        let err = enter_phase_unless_cancelled(&db, 7, "restore")
            .await
            .expect_err("cancellation recorded first");

        assert!(
            matches!(&err, RestoreError::RestoreCancelled { restore_run_id: 7, phase } if phase == "download"),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn gate_enters_the_write_phase_once_and_reports_it() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let gate = WorkerGate::new(
            &db,
            7,
            CancellationToken::new(),
            true,
            "restore",
            "pg".into(),
        );

        gate.begin_target_writes().await.expect("no cancellation");

        assert!(gate.writes_started());
    }

    #[tokio::test]
    async fn gate_refuses_writes_when_the_cancellation_won() {
        let mut cancelled = run("in_place", None, "running", "download");
        cancelled.cancel_requested_at = Some(Utc::now());
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_results([vec![cancelled]])
            .into_connection();
        let gate = WorkerGate::new(
            &db,
            7,
            CancellationToken::new(),
            true,
            "restore",
            "pg".into(),
        );

        let err = gate.begin_target_writes().await.expect_err("cancelled");

        assert_eq!(err.target, "pg");
        assert!(!gate.writes_started());
    }

    #[tokio::test]
    async fn new_service_gate_reports_signalled_cancellation_without_a_phase_move() {
        // No exec results queued: touching the database would fail the test.
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let token = CancellationToken::new();
        let gate = WorkerGate::new(&db, 7, token.clone(), false, "provision", "copy".into());

        gate.begin_target_writes().await.expect("not cancelled yet");
        token.cancel();
        assert!(gate.is_cancelled());
        assert!(gate.begin_target_writes().await.is_err());
        assert!(!gate.writes_started());
    }

    struct RecordingTeardown {
        removed: Mutex<Vec<String>>,
        fail: bool,
        /// What Docker still reports under the name after removal.
        left_after_removal: Vec<String>,
    }

    #[async_trait]
    impl NewServiceResourceProbe for RecordingTeardown {
        async fn existing(
            &self,
            _name: &str,
            _service_type: ServiceType,
        ) -> Result<Vec<String>, String> {
            Ok(self.left_after_removal.clone())
        }
    }

    #[async_trait]
    impl UnpersistedServiceTeardown for RecordingTeardown {
        async fn remove(&self, name: &str, _service_type: ServiceType) -> Result<(), String> {
            self.removed
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(name.to_string());
            if self.fail {
                Err("daemon unreachable".into())
            } else {
                Ok(())
            }
        }
    }

    fn teardown(fail: bool) -> RecordingTeardown {
        RecordingTeardown {
            removed: Mutex::new(Vec::new()),
            fail,
            left_after_removal: Vec::new(),
        }
    }

    #[tokio::test]
    async fn cancelled_new_service_is_torn_down() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<temps_entities::external_services::Model>::new()])
            .into_connection();
        let recorder = teardown(false);

        let cleanup = teardown_cancelled_new_service(
            &db,
            &recorder,
            &recorder,
            "copy",
            ServiceType::Postgres,
        )
        .await;

        assert_eq!(
            cleanup,
            CancelCleanup::NewServiceRemoved {
                name: "copy".into()
            }
        );
        assert_eq!(
            *recorder.removed.lock().expect("lock"),
            vec!["copy".to_string()]
        );
    }

    #[tokio::test]
    async fn teardown_never_removes_a_registered_service_with_the_same_name() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![service(42, "copy")]])
            .into_connection();
        let recorder = teardown(false);

        let cleanup = teardown_cancelled_new_service(
            &db,
            &recorder,
            &recorder,
            "copy",
            ServiceType::Postgres,
        )
        .await;

        assert!(
            matches!(&cleanup, CancelCleanup::NewServiceLeftBehind { reason, .. } if reason.contains("service 42")),
            "{cleanup:?}"
        );
        assert!(recorder.removed.lock().expect("lock").is_empty());
    }

    fn quick_retry() -> temps_core::retry::RetryConfig {
        temps_core::retry::RetryConfig::new(3)
            .with_base_delay(std::time::Duration::from_millis(1))
            .with_max_delay(std::time::Duration::from_millis(2))
    }

    /// REGRESSION (Greptile on #1295): a temporary database error on the
    /// move into `verify` is retried instead of letting the worker register
    /// the service while a cancellation may already have been accepted.
    #[tokio::test]
    async fn guarded_move_retries_a_transient_database_error() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_errors([sea_orm::DbErr::Custom("connection reset".into())])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        enter_phase_unless_cancelled_retrying(&db, 7, "verify", &quick_retry())
            .await
            .expect("second attempt records the move");
    }

    #[tokio::test]
    async fn guarded_move_gives_up_with_an_error_not_a_silent_success() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_errors([
                sea_orm::DbErr::Custom("down".into()),
                sea_orm::DbErr::Custom("down".into()),
                sea_orm::DbErr::Custom("down".into()),
            ])
            .into_connection();
        let err = enter_phase_unless_cancelled_retrying(&db, 7, "verify", &quick_retry())
            .await
            .expect_err("never recorded");
        assert!(matches!(err, RestoreError::Internal { .. }), "{err}");
        assert!(err.to_string().contains("run 7"), "{err}");
        assert!(err.to_string().contains("'verify'"), "{err}");
    }

    #[tokio::test]
    async fn guarded_move_returns_a_recorded_cancellation_without_retrying() {
        let mut cancelled = run("new_service", Some("copy"), "running", "provision");
        cancelled.cancel_requested_at = Some(Utc::now());
        // One exec and one query only: a retry would hit an empty mock and fail.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_results([vec![cancelled]])
            .into_connection();
        let err = enter_phase_unless_cancelled_retrying(&db, 7, "verify", &quick_retry())
            .await
            .expect_err("cancellation won");
        assert!(
            matches!(err, RestoreError::RestoreCancelled { ref phase, .. } if phase == "provision"),
            "{err}"
        );
    }

    struct FixedProbe(Result<Vec<String>, String>);

    #[async_trait]
    impl NewServiceResourceProbe for FixedProbe {
        async fn existing(
            &self,
            _name: &str,
            _service_type: ServiceType,
        ) -> Result<Vec<String>, String> {
            self.0.clone()
        }
    }

    fn no_other_runs() -> DatabaseConnection {
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<temps_entities::restore_runs::Model>::new()])
            .into_connection()
    }

    #[tokio::test]
    async fn unclaimed_name_lets_the_restore_provision() {
        let db = no_other_runs();
        ensure_new_service_name_unclaimed(
            &db,
            &FixedProbe(Ok(vec![])),
            7,
            "copy",
            ServiceType::Postgres,
        )
        .await
        .expect("nothing exists under the name");
    }

    /// REGRESSION (Greptile on #1295): a volume left by an earlier failed
    /// restore under the same name must stop the run before it provisions,
    /// so a later cancellation can never remove that volume.
    #[tokio::test]
    async fn leftover_volume_under_the_name_stops_the_restore() {
        let db = no_other_runs();
        let err = ensure_new_service_name_unclaimed(
            &db,
            &FixedProbe(Ok(vec!["volume postgres-copy_data".into()])),
            7,
            "copy",
            ServiceType::Postgres,
        )
        .await
        .expect_err("leftover data must block the restore");
        let message = err.to_string();
        assert!(matches!(err, RestoreError::Validation { .. }), "{message}");
        assert!(message.contains("volume postgres-copy_data"), "{message}");
        assert!(message.contains("run 7"), "{message}");
    }

    #[tokio::test]
    async fn another_active_run_creating_the_same_name_stops_the_restore() {
        let mut other = run("new_service", Some("copy"), "running", "provision");
        other.id = 9;
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![other]])
            .into_connection();
        let err = ensure_new_service_name_unclaimed(
            &db,
            &FixedProbe(Ok(vec![])),
            7,
            "copy",
            ServiceType::Postgres,
        )
        .await
        .expect_err("a concurrent run owns the name");
        assert!(err.to_string().contains("restore run 9"), "{err}");
    }

    #[tokio::test]
    async fn unknown_existence_fails_closed() {
        let db = no_other_runs();
        let err = ensure_new_service_name_unclaimed(
            &db,
            &FixedProbe(Err("docker unreachable".into())),
            7,
            "copy",
            ServiceType::Postgres,
        )
        .await
        .expect_err("cannot prove the name is free");
        assert!(matches!(err, RestoreError::ExternalService { .. }), "{err}");
        assert!(err.to_string().contains("docker unreachable"), "{err}");
    }

    /// REGRESSION (Greptile on #1295): an engine's `remove()` can return Ok
    /// while a container or volume survives (it skips objects it did not
    /// list and ignores volume errors). The run must say what is left, never
    /// "nothing was left behind".
    #[tokio::test]
    async fn teardown_reports_objects_still_present_after_removal() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<temps_entities::external_services::Model>::new()])
            .into_connection();
        let mut recorder = teardown(false);
        recorder.left_after_removal = vec!["volume postgres-copy_data".into()];

        let cleanup = teardown_cancelled_new_service(
            &db,
            &recorder,
            &recorder,
            "copy",
            ServiceType::Postgres,
        )
        .await;

        assert_eq!(
            cleanup,
            CancelCleanup::NewServiceLeftBehind {
                name: "copy".into(),
                reason: "still present after removal: volume postgres-copy_data".into(),
            }
        );
        assert!(!cancelled_message("provision", false, &cleanup).contains("nothing was left"));
    }

    #[tokio::test]
    async fn failed_teardown_is_reported_not_hidden() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<temps_entities::external_services::Model>::new()])
            .into_connection();

        let cleanup = teardown_cancelled_new_service(
            &db,
            &teardown(true),
            &teardown(true),
            "copy",
            ServiceType::Postgres,
        )
        .await;

        assert_eq!(
            cleanup,
            CancelCleanup::NewServiceLeftBehind {
                name: "copy".into(),
                reason: "daemon unreachable".into()
            }
        );
    }
}
