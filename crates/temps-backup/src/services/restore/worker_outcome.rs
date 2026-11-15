// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! How a run finishes from the outcome of a restore that writes on a worker
//! node rather than in this process: the node's agent running an engine
//! (PostgreSQL WAL-G, MongoDB, Redis, MariaDB, or its generic restore of a
//! storage service or another engine's dump), or a PostgreSQL dump applied
//! by a job through the workload plane. [`restore_placement`] is the one
//! classification of where a restore writes; admission, the live dispatch
//! and the startup reconciliation all use it, so they cannot drift.
//!
//! A worker restore that began writing to its target runs on its node past
//! the request's deadline, until it has started the restored data or put
//! the original back. The control plane's request can time out, or its
//! connection drop, before then. The run must not be finished meanwhile:
//! that would release the service to another destructive restore while the
//! node still swaps its data, and skip the credential reconcile a later
//! success needs. So every agent restore is started under its run's
//! [`operation_id`], and every job carries the run's id; when its request
//! goes unanswered the run stays `running` and the node (or the plane) is
//! asked for the outcome until it reports one. Only then is the run
//! settled: a completed restore patches the target's stored login to the
//! restored one, as an answered one does.
//!
//! The same holds across a restart of this process: the node keeps
//! restoring, so a worker restore run left active is not marked
//! interrupted at startup (that would release the service and skip the
//! reconcile). [`ManagerWorkerRestoreResume`] asks for the outcome recorded
//! under the run's operation id, or the run's job, and settles the run as
//! a live one would.
//!
//! When the outcome cannot be established while the restore may still be
//! writing (its node stays unreachable, or has no record of a restore it
//! may have received), the run is never failed: that would release the
//! service under a running restore. It stays active in phase
//! [`OUTCOME_UNKNOWN_PHASE`], holding the service, with a message saying
//! what to check, and keeps asking. It settles if the node answers; the
//! operator's explicit release (cancelling the run) ends it otherwise.

use std::future::Future;
use std::time::Duration;

use sea_orm::DatabaseConnection;
use temps_providers::remote_service_client::{
    WorkerRestoreEngine, WorkerRestoreProgress, WorkerRestoreResult,
};
use temps_providers::services::ExternalServiceError;
use tracing::{info, warn};

use super::*;
use crate::engines::workload_job::{self, RestoreJobProgress};
use crate::services::restore_cancel::OUTCOME_UNKNOWN_PHASE;

/// How a restore whose request went unanswered is waited for: its node (or
/// the plane) is asked every `interval` where the restore is. Once it has
/// been unreachable for `unreachable_for`, the run is held with what to
/// check (see [`OutcomeHold`]); asking goes on.
#[derive(Debug, Clone, Copy)]
pub(super) struct OutcomeWait {
    pub(super) interval: Duration,
    pub(super) unreachable_for: Duration,
}

const OUTCOME_WAIT: OutcomeWait = OutcomeWait {
    interval: Duration::from_secs(15),
    unreachable_for: Duration::from_secs(10 * 60),
};

/// The name a restore run's worker restore is started under on its node, to
/// ask for its outcome by.
pub(super) fn operation_id(run_id: i32) -> String {
    format!("restore-run-{run_id}")
}

// ---- Where a restore writes

/// Where a restore into a service writes, which says what records its
/// progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RestorePlacement {
    /// The node's agent runs `engine`, and records its outcome under the
    /// run's [`operation_id`].
    Node(WorkerRestoreEngine),
    /// A job on the node, through the workload plane, labelled with the
    /// run's id ([`workload_job::RESTORE_RUN_LABEL`]).
    Job,
    /// This process, with its own Docker helpers: they are fenced, and the
    /// run marked interrupted, when it restarts.
    ControlPlane,
}

/// Where a restore of the backup at `location` into `target` writes: the
/// one classification admission, `run_restore_inner` and the startup
/// reconciliation use. Only an in-place restore writes on a worker:
/// restoring a worker's service into a new service, or to a point in time
/// elsewhere, is refused at admission, so those runs always ran here.
pub(super) fn restore_placement(
    target: &temps_entities::external_services::Model,
    location: &str,
) -> RestorePlacement {
    if super::worker_walg::takes_walg_path(target, location) {
        RestorePlacement::Node(WorkerRestoreEngine::PostgresWalg)
    } else if super::worker_mariadb::takes_mariadb_path(target) {
        RestorePlacement::Node(WorkerRestoreEngine::Mariadb)
    } else if workload_job::takes_job_path(target) {
        RestorePlacement::Job
    } else if super::worker_mongodb::takes_path(target) {
        RestorePlacement::Node(WorkerRestoreEngine::Mongodb)
    } else if super::worker_redis::takes_redis_path(target) {
        RestorePlacement::Node(WorkerRestoreEngine::Redis)
    } else if crate::engines::worker_agent::takes_agent_path(target) {
        RestorePlacement::Node(WorkerRestoreEngine::Data)
    } else {
        RestorePlacement::ControlPlane
    }
}

// ---- Who reports the outcome

/// What keeps the record of a worker restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Keeper {
    /// The node's agent, under the run's operation id. With no record of
    /// a restore it may have received, the restore may still be writing.
    Agent,
    /// The workload plane, which has the run's job until its time-to-live.
    /// With no job, nothing runs.
    Plane,
}

/// What a worker restore's outcome is asked of, for messages.
#[derive(Debug, Clone, Copy)]
pub(super) struct OutcomeSource {
    label: &'static str,
    node_id: i32,
    keeper: Keeper,
}

impl OutcomeSource {
    /// The agent of node `node_id`, running `engine`.
    pub(super) fn node(engine: WorkerRestoreEngine, node_id: i32) -> Self {
        Self {
            label: engine.label(),
            node_id,
            keeper: Keeper::Agent,
        }
    }

    /// The run's job on node `node_id`, through the workload plane.
    pub(super) fn job(node_id: i32) -> Self {
        Self {
            label: "PostgreSQL restore job",
            node_id,
            keeper: Keeper::Plane,
        }
    }
}

/// What a resumed job restore's progress is, in the terms an agent reports.
fn job_outcome(progress: RestoreJobProgress) -> WorkerRestoreProgress {
    match progress {
        RestoreJobProgress::NoJob => WorkerRestoreProgress::Unknown,
        RestoreJobProgress::Running => WorkerRestoreProgress::Running { writes_began: true },
        RestoreJobProgress::Succeeded { .. } => WorkerRestoreProgress::Finished {
            outcome: WorkerRestoreResult::Completed,
        },
        RestoreJobProgress::Failed(error) => WorkerRestoreProgress::Finished {
            outcome: WorkerRestoreResult::Failed {
                reason: error.to_string(),
            },
        },
    }
}

/// Where restore run `run_id`'s job on database `service_id` stands, as the
/// plane bound to `plane` has it.
async fn job_progress(
    plane: &temps_workload_plane::PlaneSlot,
    service_id: i32,
    node_id: i32,
    run_id: i32,
) -> Result<WorkerRestoreProgress, String> {
    let Some(plane) = plane.get() else {
        return Err("the workload plane is not running in this process yet".to_string());
    };
    workload_job::restore_job_progress(plane.as_ref(), service_id, node_id, run_id)
        .await
        .map(job_outcome)
        .map_err(|error| error.to_string())
}

// ---- Holding a run whose outcome is unknown

/// What the operator is told to do about a held run.
const HOLD_ADVICE: &str = "Temps cannot tell whether the restore is still writing, so the \
     service stays held and no other restore of it can start. The run settles on its own once \
     the outcome is known. If it cannot be (the node is gone for good), check the database, \
     then release the service by cancelling this restore.";

/// Keeps a run whose outcome cannot be established active, holding its
/// service, until the outcome is known or an operator releases it.
#[async_trait::async_trait]
pub(super) trait OutcomeHold: Send + Sync {
    /// The outcome cannot be established now (`reason`): show it on the
    /// run, which stays active and can be released by cancelling it.
    async fn hold(&self, reason: &str);
    /// The restore is reported running again: back to its phase.
    async fn unhold(&self);
    /// Whether an operator released the held run.
    async fn released(&self) -> bool;
}

/// [`OutcomeHold`] on the run's row: phase [`OUTCOME_UNKNOWN_PHASE`] (a
/// cancellable one) and the reason as its message.
pub(super) struct RunHold<'a> {
    db: &'a DatabaseConnection,
    run_id: i32,
    resume_phase: String,
}

impl<'a> RunHold<'a> {
    /// Hold run `run_id`, which is in `phase`, when it must be.
    pub(super) fn new(db: &'a DatabaseConnection, run_id: i32, phase: &str) -> Self {
        let resume_phase = if phase == OUTCOME_UNKNOWN_PHASE {
            "restore"
        } else {
            phase
        };
        Self {
            db,
            run_id,
            resume_phase: resume_phase.to_string(),
        }
    }
}

#[async_trait::async_trait]
impl OutcomeHold for RunHold<'_> {
    async fn hold(&self, reason: &str) {
        use sea_orm::sea_query::Expr;
        use temps_entities::restore_runs::{Column, Entity};
        let message = format!("{reason} {HOLD_ADVICE}");
        let held = Entity::update_many()
            .col_expr(Column::Phase, Expr::value(OUTCOME_UNKNOWN_PHASE))
            .col_expr(Column::ErrorMessage, Expr::value(Some(message)))
            .col_expr(Column::UpdatedAt, Expr::value(Utc::now()))
            .filter(Column::Id.eq(self.run_id))
            .filter(Column::Status.is_in(ACTIVE_RESTORE_STATUSES))
            .filter(Column::CancelRequestedAt.is_null())
            .exec(self.db)
            .await;
        match held {
            Ok(_) => warn!(
                restore_run_id = self.run_id,
                "Restore run held: its outcome is unknown and it may still be writing. {reason}"
            ),
            // The run stays active either way; only the console's message
            // is missing.
            Err(e) => error!(
                restore_run_id = self.run_id,
                "Restore run held, but recording why failed: {e}. {reason}"
            ),
        }
    }

    async fn unhold(&self) {
        use sea_orm::sea_query::Expr;
        use temps_entities::restore_runs::{Column, Entity};
        let resumed = Entity::update_many()
            .col_expr(Column::Phase, Expr::value(self.resume_phase.as_str()))
            .col_expr(Column::ErrorMessage, Expr::value(Option::<String>::None))
            .col_expr(Column::UpdatedAt, Expr::value(Utc::now()))
            .filter(Column::Id.eq(self.run_id))
            .filter(Column::Status.is_in(ACTIVE_RESTORE_STATUSES))
            .filter(Column::Phase.eq(OUTCOME_UNKNOWN_PHASE))
            .filter(Column::CancelRequestedAt.is_null())
            .exec(self.db)
            .await;
        if let Err(e) = resumed {
            warn!(
                restore_run_id = self.run_id,
                "Restore run is reported running again, but recording it failed: {e}"
            );
        }
    }

    async fn released(&self) -> bool {
        match temps_entities::restore_runs::Entity::find_by_id(self.run_id)
            .one(self.db)
            .await
        {
            Ok(run) => run.is_some_and(|run| run.cancel_requested_at.is_some()),
            Err(e) => {
                warn!(
                    restore_run_id = self.run_id,
                    "Could not read whether held restore run was released; asking again: {e}"
                );
                false
            }
        }
    }
}

/// Why `run` is held, when the previous process held it: its message,
/// without the advice the hold adds.
fn held_reason(run: &temps_entities::restore_runs::Model) -> Option<String> {
    if run.phase != OUTCOME_UNKNOWN_PHASE {
        return None;
    }
    let message = run.error_message.as_deref().unwrap_or_default();
    let reason = message
        .strip_suffix(HOLD_ADVICE)
        .unwrap_or(message)
        .trim()
        .to_string();
    Some(if reason.is_empty() {
        format!(
            "restore run {}: its outcome was unknown when Temps restarted.",
            run.id
        )
    } else {
        reason
    })
}

/// The run's terminal outcome once an operator released it while its
/// outcome was unknown for `reason`.
fn released(run_id: i32, reason: &str) -> RestoreError {
    RestoreError::Cancelled {
        restore_run_id: run_id,
        phase: OUTCOME_UNKNOWN_PHASE.to_string(),
        message: format!(
            "Released by an operator while the restore's outcome was unknown. {reason} The \
             database may be partially restored; check it before using it or restoring again."
        ),
    }
}

/// Hold run `run_id` for `reason` until an operator releases it: for a run
/// whose outcome cannot be asked for at all.
async fn hold_until_released(
    wait: &OutcomeWait,
    hold: &dyn OutcomeHold,
    run_id: i32,
    reason: &str,
) -> RestoreError {
    hold.hold(reason).await;
    loop {
        if hold.released().await {
            return released(run_id, reason);
        }
        tokio::time::sleep(wait.interval).await;
    }
}

// ---- Waiting for the outcome

/// The run a worker restore by a node's agent belongs to, which its outcome
/// finishes.
pub(super) struct WorkerRestoreRun<'a> {
    pub(super) db: &'a Arc<DatabaseConnection>,
    pub(super) run_id: i32,
    pub(super) node_id: i32,
    pub(super) target_id: i32,
    pub(super) engine: WorkerRestoreEngine,
    /// The login the restored data answers to, which the target's stored
    /// configuration (encrypted with the key given) is patched to once the
    /// restore completed; `None` when the restore keeps the target's.
    pub(super) reconcile: Option<(
        &'a Arc<temps_core::EncryptionService>,
        &'a RestoredServiceCredentials,
    )>,
}

impl WorkerRestoreRun<'_> {
    /// Finish the run from what its node answered to the restore request
    /// (started under [`operation_id`]). An unanswered request is followed
    /// by asking the node until it reports the restore's outcome.
    pub(super) async fn finish<T: Into<WorkerRestoreResult>>(
        &self,
        mgr: &Arc<ExternalServiceManager>,
        answered: Result<T, ExternalServiceError>,
    ) -> Result<Option<i32>, RestoreError> {
        let operation_id = operation_id(self.run_id);
        let hold = RunHold::new(self.db.as_ref(), self.run_id, "restore");
        self.finish_with(&OUTCOME_WAIT, &hold, answered, || {
            mgr.worker_restore_outcome(self.target_id, self.engine, &operation_id)
        })
        .await
    }

    /// [`Self::finish`], asking the node with `ask` as `wait` says, and
    /// holding the run with `hold` while the outcome is unknown.
    pub(super) async fn finish_with<T, Ask, AskFut>(
        &self,
        wait: &OutcomeWait,
        hold: &dyn OutcomeHold,
        answered: Result<T, ExternalServiceError>,
        ask: Ask,
    ) -> Result<Option<i32>, RestoreError>
    where
        T: Into<WorkerRestoreResult>,
        Ask: FnMut() -> AskFut,
        AskFut: Future<Output = Result<WorkerRestoreProgress, ExternalServiceError>>,
    {
        let (run_id, node_id) = (self.run_id, self.node_id);
        let outcome = match answered {
            Ok(outcome) => outcome.into(),
            // The request timed out or its connection dropped: the node may
            // still be restoring, past its deadline once it began writing.
            // The run stays active (and keeps other restores of the service
            // out) until the node says how it ended.
            Err(ExternalServiceError::AgentUnanswered { detail, .. }) => {
                warn!(
                    run_id,
                    service_id = self.target_id,
                    node_id,
                    engine = self.engine.label(),
                    %detail,
                    "the node did not answer a restore; waiting for its outcome there"
                );
                await_outcome(
                    wait,
                    Asking::Unanswered,
                    OutcomeSource::node(self.engine, node_id),
                    run_id,
                    hold,
                    None,
                    ask,
                )
                .await?
            }
            Err(e) => {
                return Err(RestoreError::ExternalService {
                    reason: format!("restore run {run_id} on node {node_id}: {e}"),
                })
            }
        };
        settle_outcome(
            self.db,
            run_id,
            node_id,
            self.target_id,
            self.reconcile,
            outcome,
        )
        .await
    }
}

/// What run `run_id` makes of its worker restore's outcome: a completed
/// restore patches the target's stored login to the restored one
/// (`reconcile`) and enters verification; a refused or failed one fails
/// the run.
async fn settle_outcome(
    db: &Arc<DatabaseConnection>,
    run_id: i32,
    node_id: i32,
    target_id: i32,
    reconcile: Option<(
        &Arc<temps_core::EncryptionService>,
        &RestoredServiceCredentials,
    )>,
    outcome: WorkerRestoreResult,
) -> Result<Option<i32>, RestoreError> {
    match outcome {
        WorkerRestoreResult::Completed => {}
        WorkerRestoreResult::Refused { reason } => {
            return Err(RestoreError::Validation {
                message: format!("restore run {run_id} on node {node_id} was refused: {reason}"),
            })
        }
        WorkerRestoreResult::Failed { reason } => {
            return Err(RestoreError::ExternalService {
                reason: format!("restore run {run_id} on node {node_id}: {reason}"),
            })
        }
    }
    if let Some((enc, credentials)) = reconcile {
        reconcile_restored_credentials(db, enc, target_id, Some(credentials)).await;
    }
    if let Err(e) = update_phase(db.as_ref(), run_id, "verify").await {
        warn!("Restore run {run_id}: recording the verify phase failed: {e}");
    }
    Ok(None)
}

/// Why a restore's outcome is asked for, which says what a node with no
/// record of it means.
#[derive(Debug, Clone, Copy)]
enum Asking<'a> {
    /// The run's own request went unanswered.
    Unanswered,
    /// This process restarted while the run was in `phase`.
    AfterRestart { phase: &'a str },
}

/// The phases a worker run enters just before it sends its restore to the
/// node (or starts its job), and after: the node may have received it.
/// Before them (preparing, or waiting for a busy node's slot) no request is
/// in flight.
fn request_may_have_been_sent(phase: &str) -> bool {
    matches!(phase, "restore" | "recover" | "verify") || phase == OUTCOME_UNKNOWN_PHASE
}

/// What a source with no record of the restore means: `Ok(reason)` to hold
/// the run (the restore may still be writing), `Err(reason)` to fail it
/// (nothing runs).
fn no_record(asking: Asking<'_>, source: OutcomeSource, run_id: i32) -> Result<String, String> {
    let OutcomeSource {
        label,
        node_id,
        keeper,
    } = source;
    match (asking, keeper) {
        (Asking::AfterRestart { phase }, _) if !request_may_have_been_sent(phase) => Err(format!(
            "restore run {run_id}: Temps restarted in phase '{phase}', before the {label} was \
             started on node {node_id}, which has no record of it. The database was not changed; \
             start the restore again"
        )),
        (asking, Keeper::Plane) => Err(format!(
            "restore run {run_id}: {}the workload plane has no {label} for it on node {node_id}: \
             it never started, or it finished over an hour before and was removed. Nothing is \
             restoring now, but the database may have been changed; check it before restoring \
             again",
            match asking {
                Asking::AfterRestart { phase } =>
                    format!("Temps restarted during the {label} (phase '{phase}'), and "),
                Asking::Unanswered => String::new(),
            }
        )),
        (Asking::Unanswered, Keeper::Agent) => Ok(format!(
            "restore run {run_id}: node {node_id} did not answer the {label} and has no record \
             of it (its agent may have restarted). The database may have been changed."
        )),
        (Asking::AfterRestart { phase }, Keeper::Agent) => Ok(format!(
            "restore run {run_id}: Temps restarted during the {label} (phase '{phase}'), and \
             node {node_id} has no record of it: it never received the request, or its agent \
             restarted too. The database may have been changed."
        )),
    }
}

/// Wait for the outcome of a worker restore, asking `source` with `ask` as
/// `wait` says. Nothing is written meanwhile: the run stays `running`, so no
/// other destructive restore of the service starts while the node may still
/// be swapping data.
///
/// A restore that ended before it changed the service fails cleanly; one
/// that ended without an outcome after it began writing fails saying the
/// service must be checked. One whose outcome cannot be established while
/// it may still be writing (a source that stays unreachable, an agent with
/// no record of a restore it may have received) is held with `hold`, and
/// asked for until it answers or an operator releases the run.
///
/// `held` is why the run is already held, when it is: a run the previous
/// process held (phase [`OUTCOME_UNKNOWN_PHASE`]) starts held, so a report
/// that the restore runs again lets go of that hold too. Whatever the
/// source reports, every pass checks for the operator's release: one the
/// API accepted is honoured, never left to wait for the restore's end.
async fn await_outcome<Ask, AskFut, E>(
    wait: &OutcomeWait,
    asking: Asking<'_>,
    source: OutcomeSource,
    run_id: i32,
    hold: &dyn OutcomeHold,
    held: Option<String>,
    mut ask: Ask,
) -> Result<WorkerRestoreResult, RestoreError>
where
    Ask: FnMut() -> AskFut,
    AskFut: Future<Output = Result<WorkerRestoreProgress, E>>,
    E: std::fmt::Display,
{
    let OutcomeSource { label, node_id, .. } = source;
    let mut unreachable_since: Option<tokio::time::Instant> = None;
    // Why the run is held, while it is; and why it was last held, which a
    // release accepted before the hold was let go of explains.
    let mut held = held;
    let mut last_held = held.clone();
    loop {
        match ask().await {
            Ok(WorkerRestoreProgress::Finished { outcome }) => {
                info!(
                    run_id,
                    node_id,
                    engine = label,
                    ?outcome,
                    "the outcome of an unanswered restore is known"
                );
                return Ok(outcome);
            }
            Ok(WorkerRestoreProgress::Ended {
                writes_began: false,
                reason,
            }) => {
                return Err(RestoreError::ExternalService {
                    reason: format!(
                        "restore run {run_id} on node {node_id} ({label}) stopped before it \
                         changed the database, which is unchanged: {reason}"
                    ),
                })
            }
            Ok(WorkerRestoreProgress::Ended {
                writes_began: true,
                reason,
            }) => {
                return Err(RestoreError::ExternalService {
                    reason: format!(
                        "restore run {run_id} on node {node_id} ({label}) stopped after it began \
                         writing to the database, without an outcome: {reason}. The database \
                         may be partially restored; check it before restoring again"
                    ),
                })
            }
            Ok(WorkerRestoreProgress::Unknown) => {
                unreachable_since = None;
                match no_record(asking, source, run_id) {
                    Err(reason) => return Err(RestoreError::ExternalService { reason }),
                    Ok(reason) => {
                        if held.is_none() {
                            hold.hold(&reason).await;
                            last_held = Some(reason.clone());
                            held = Some(reason);
                        }
                    }
                }
            }
            Ok(WorkerRestoreProgress::Running { writes_began }) => {
                unreachable_since = None;
                if held.take().is_some() {
                    info!(
                        run_id,
                        node_id,
                        engine = label,
                        "a held restore is reported running again"
                    );
                    hold.unhold().await;
                }
                tracing::debug!(
                    run_id,
                    node_id,
                    engine = label,
                    writes_began,
                    "an unanswered restore is still running"
                );
            }
            Err(error) => {
                let since = *unreachable_since.get_or_insert_with(tokio::time::Instant::now);
                warn!(
                    run_id,
                    node_id,
                    engine = label,
                    error = %error,
                    "could not ask for the outcome of an unanswered restore; retrying"
                );
                if held.is_none() && since.elapsed() >= wait.unreachable_for {
                    let what = match asking {
                        Asking::Unanswered => {
                            format!("node {node_id} did not answer the {label}, and")
                        }
                        Asking::AfterRestart { phase } => format!(
                            "Temps restarted during the {label} (phase '{phase}'), and node \
                             {node_id}"
                        ),
                    };
                    let reason = format!(
                        "restore run {run_id}: {what} could not be asked for its outcome for \
                         {}s ({error}). Check that the node is online. The restore may still be \
                         running there and the database may be partially restored.",
                        wait.unreachable_for.as_secs()
                    );
                    hold.hold(&reason).await;
                    last_held = Some(reason.clone());
                    held = Some(reason);
                }
            }
        }
        // Every pass, whatever was reported: the API accepts a release only
        // while the run is held, so one on record is the operator's.
        if hold.released().await {
            warn!(
                run_id,
                node_id,
                engine = label,
                "a held restore was released by an operator while its outcome was unknown"
            );
            let reason = last_held.unwrap_or_else(|| {
                format!(
                    "restore run {run_id}: the outcome of the {label} on node {node_id} was not \
                     known when the release was accepted."
                )
            });
            return Err(released(run_id, &reason));
        }
        tokio::time::sleep(wait.interval).await;
    }
}

// ---- After a restart

/// How a worker restore run left active by the previous process is
/// settled.
pub(super) enum Resume {
    /// From the outcome its node's agent records for `engine`.
    Node {
        engine: WorkerRestoreEngine,
        backup: temps_entities::backups::Model,
    },
    /// From its job, through the workload plane.
    Job {
        backup: temps_entities::backups::Model,
    },
    /// Its target is on a worker node, but which restore runs there cannot
    /// be told (`reason`): held until an operator releases it.
    Unclassified { reason: String },
}

/// A worker restore run left active by the previous process.
pub(super) struct ResumedWorkerRestore {
    run: temps_entities::restore_runs::Model,
    target: temps_entities::external_services::Model,
    node_id: i32,
    how: Resume,
}

impl ResumedWorkerRestore {
    /// `run` as a worker restore to resume, or `None` when it wrote in this
    /// process (its helpers are fenced and it is marked interrupted).
    ///
    /// A restore into a new service always ran here: admission refuses one
    /// from a service on a worker node, and the new service is created on
    /// the source's node. An in-place restore into a service on a worker
    /// takes [`restore_placement`], as the live run did.
    pub(super) async fn of(
        db: &DatabaseConnection,
        run: &temps_entities::restore_runs::Model,
        service: Option<&temps_entities::external_services::Model>,
    ) -> Option<Self> {
        if run
            .target_service_name
            .as_deref()
            .is_some_and(|name| !name.trim().is_empty())
        {
            return None;
        }
        let target = service?.clone();
        let node_id = workload_job::worker_node(&target)?;
        let how = match run_backup_model(db, run).await {
            Ok(backup) => match restore_placement(&target, &backup.s3_location) {
                RestorePlacement::Node(engine) => Resume::Node { engine, backup },
                RestorePlacement::Job => Resume::Job { backup },
                RestorePlacement::ControlPlane => return None,
            },
            Err(e) => {
                warn!(
                    restore_run_id = run.id,
                    service_id = target.id,
                    "could not read the backup of restore run {} to tell which restore its node \
                     runs: {e}",
                    run.id
                );
                Resume::Unclassified {
                    reason: format!(
                        "restore run {}: Temps restarted during this restore of service {} on \
                         node {node_id} (phase '{}'), and could not tell which restore the node \
                         runs, so it cannot ask for its outcome: {e}.",
                        run.id, target.id, run.phase
                    ),
                }
            }
        };
        Some(Self {
            run: run.clone(),
            target,
            node_id,
            how,
        })
    }

    /// Settle the run from its outcome as the live run would have, asking
    /// for it with `ask` as `wait` says and holding the run with `hold`
    /// while it is unknown: `origin` is the login the restored data answers
    /// to.
    pub(super) async fn settle_with<Ask, AskFut, E>(
        &self,
        db: &Arc<DatabaseConnection>,
        enc: &Arc<temps_core::EncryptionService>,
        origin: Option<&RestoredServiceCredentials>,
        wait: &OutcomeWait,
        hold: &dyn OutcomeHold,
        ask: Ask,
    ) -> Result<Option<i32>, RestoreError>
    where
        Ask: FnMut() -> AskFut,
        AskFut: Future<Output = Result<WorkerRestoreProgress, E>>,
        E: std::fmt::Display,
    {
        let run_id = self.run.id;
        let source = match &self.how {
            Resume::Node { engine, .. } => OutcomeSource::node(*engine, self.node_id),
            Resume::Job { .. } => OutcomeSource::job(self.node_id),
            Resume::Unclassified { reason } => {
                return Err(hold_until_released(wait, hold, run_id, reason).await)
            }
        };
        info!(
            run_id,
            service_id = self.target.id,
            node_id = self.node_id,
            engine = source.label,
            phase = %self.run.phase,
            "resuming a worker restore after a restart; asking for its outcome"
        );
        let outcome = await_outcome(
            wait,
            Asking::AfterRestart {
                phase: &self.run.phase,
            },
            source,
            run_id,
            hold,
            held_reason(&self.run),
            ask,
        )
        .await?;
        settle_outcome(
            db,
            run_id,
            self.node_id,
            self.target.id,
            origin.map(|origin| (enc, origin)),
            outcome,
        )
        .await
    }
}

/// Resumes worker restore runs left active by the previous process.
pub struct ManagerWorkerRestoreResume {
    db: Arc<DatabaseConnection>,
    enc: Arc<temps_core::EncryptionService>,
    mgr: Arc<ExternalServiceManager>,
    plane: Arc<temps_workload_plane::PlaneSlot>,
}

impl ManagerWorkerRestoreResume {
    pub fn new(
        db: Arc<DatabaseConnection>,
        enc: Arc<temps_core::EncryptionService>,
        mgr: Arc<ExternalServiceManager>,
        plane: Arc<temps_workload_plane::PlaneSlot>,
    ) -> Self {
        Self {
            db,
            enc,
            mgr,
            plane,
        }
    }
}

#[async_trait::async_trait]
impl super::super::restore_reconcile::WorkerRestoreResume for ManagerWorkerRestoreResume {
    async fn resume(
        &self,
        run: &temps_entities::restore_runs::Model,
        service: Option<&temps_entities::external_services::Model>,
    ) -> bool {
        let Some(resumed) = ResumedWorkerRestore::of(self.db.as_ref(), run, service).await else {
            return false;
        };
        let (db, enc, mgr, plane) = (
            Arc::clone(&self.db),
            Arc::clone(&self.enc),
            Arc::clone(&self.mgr),
            Arc::clone(&self.plane),
        );
        // The run stays active, holding its service, until this settles it.
        tokio::spawn(async move {
            let run_id = resumed.run.id;
            let (target_id, node_id) = (resumed.target.id, resumed.node_id);
            let hold = RunHold::new(db.as_ref(), run_id, &resumed.run.phase);
            let result = match &resumed.how {
                Resume::Node { engine, backup } => {
                    let origin = resumed_origin_credentials(
                        enc.as_ref(),
                        &resumed.run,
                        origin_credentials_to_reconcile(
                            db.as_ref(),
                            mgr.as_ref(),
                            backup,
                            &resumed.target,
                        ),
                    )
                    .await;
                    let operation_id = operation_id(run_id);
                    resumed
                        .settle_with(&db, &enc, origin.as_ref(), &OUTCOME_WAIT, &hold, || {
                            mgr.worker_restore_outcome(target_id, *engine, &operation_id)
                        })
                        .await
                }
                Resume::Job { backup } => {
                    // As `run_job_restore`: the job keeps the target's login,
                    // only the database the data lives in is reconciled.
                    let origin = resumed_origin_credentials(
                        enc.as_ref(),
                        &resumed.run,
                        origin_credentials_to_reconcile(
                            db.as_ref(),
                            mgr.as_ref(),
                            backup,
                            &resumed.target,
                        ),
                    )
                    .await
                    .and_then(|origin| origin.into_database_only());
                    resumed
                        .settle_with(&db, &enc, origin.as_ref(), &OUTCOME_WAIT, &hold, || {
                            job_progress(&plane, target_id, node_id, run_id)
                        })
                        .await
                }
                Resume::Unclassified { reason } => {
                    Err(hold_until_released(&OUTCOME_WAIT, &hold, run_id, reason).await)
                }
            };
            match persist_terminal_state(db.as_ref(), run_id, &result, TERMINAL_WRITE_RETRY).await {
                Ok(()) => match &result {
                    Ok(_) => info!("Restore run {run_id} resumed after a restart and completed"),
                    Err(RestoreError::Cancelled { message, .. }) => {
                        info!(
                            "Restore run {run_id} resumed after a restart and released: {message}"
                        )
                    }
                    Err(e) => {
                        error!("Restore run {run_id} resumed after a restart and failed: {e}")
                    }
                },
                Err(e) => error!(
                    "Restore run {run_id} resumed after a restart and settled, but its terminal \
                     state could not be recorded: {e}. It is resumed again at the next restart"
                ),
            }
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PG: WorkerRestoreEngine = WorkerRestoreEngine::PostgresWalg;

    const FAST: OutcomeWait = OutcomeWait {
        interval: Duration::from_millis(5),
        unreachable_for: Duration::from_millis(60),
    };

    /// A hold that records what it is told, and reports the run released
    /// once it was held when `release` says so, or at once when a release
    /// was `accepted`.
    #[derive(Default)]
    struct RecordedHold {
        release: bool,
        accepted: bool,
        events: std::sync::Mutex<Vec<String>>,
    }

    impl RecordedHold {
        fn releasing() -> Self {
            Self {
                release: true,
                ..Default::default()
            }
        }

        fn events(&self) -> Vec<String> {
            self.events.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl OutcomeHold for RecordedHold {
        async fn hold(&self, reason: &str) {
            self.events.lock().unwrap().push(format!("hold: {reason}"));
        }

        async fn unhold(&self) {
            self.events.lock().unwrap().push("unhold".to_string());
        }

        async fn released(&self) -> bool {
            self.accepted
                || self.release
                    && self
                        .events
                        .lock()
                        .unwrap()
                        .last()
                        .is_some_and(|event| event.starts_with("hold"))
        }
    }

    /// Wait for the outcome of a PostgreSQL WAL-G restore of run 41 on node
    /// 2 whose request went unanswered.
    async fn await_unanswered(
        hold: &RecordedHold,
        answers: impl FnMut() -> std::future::Ready<Result<WorkerRestoreProgress, ExternalServiceError>>,
    ) -> Result<WorkerRestoreResult, RestoreError> {
        await_outcome(
            &FAST,
            Asking::Unanswered,
            OutcomeSource::node(PG, 2),
            41,
            hold,
            None,
            answers,
        )
        .await
    }

    /// A release the API accepted is honoured on the next pass even while
    /// the node reports the restore running: it is never left waiting for
    /// the restore to end. A run held before the restart starts held, so the
    /// running report lets go of that hold, and the release keeps its
    /// reason.
    #[tokio::test]
    async fn a_release_accepted_while_the_node_reports_running_is_never_ignored() {
        let running = || {
            answers(vec![Ok(WorkerRestoreProgress::Running {
                writes_began: true,
            })])
        };

        let hold = RecordedHold {
            accepted: true,
            ..Default::default()
        };
        let error = await_unanswered(&hold, running())
            .await
            .expect_err("released");
        assert!(
            matches!(&error, RestoreError::Cancelled { phase, .. } if phase == OUTCOME_UNKNOWN_PHASE),
            "{error:?}"
        );
        assert!(error
            .to_string()
            .contains("not known when the release was accepted"));
        assert!(hold.events().is_empty(), "{:?}", hold.events());

        let hold = RecordedHold {
            accepted: true,
            ..Default::default()
        };
        let error = await_outcome(
            &FAST,
            Asking::AfterRestart {
                phase: OUTCOME_UNKNOWN_PHASE,
            },
            OutcomeSource::node(PG, 2),
            41,
            &hold,
            Some("restore run 41: node 2 was unreachable.".to_string()),
            running(),
        )
        .await
        .expect_err("released");
        assert_eq!(hold.events(), vec!["unhold".to_string()]);
        assert!(
            error.to_string().contains("node 2 was unreachable"),
            "{error}"
        );
    }

    /// The node's answers to the outcome asks, in order; the last repeats.
    fn answers(
        answers: Vec<Result<WorkerRestoreProgress, ExternalServiceError>>,
    ) -> impl FnMut() -> std::future::Ready<Result<WorkerRestoreProgress, ExternalServiceError>>
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
            route: "/agent/services/restore/outcome".to_string(),
            detail: "operation timed out".to_string(),
        }
    }

    /// The restore's own request went unanswered before it began writing:
    /// the node's agent stopped it at its deadline, and the run fails at
    /// once, saying the database is unchanged.
    #[tokio::test]
    async fn an_unanswered_restore_that_never_wrote_fails_cleanly() {
        let error = await_unanswered(
            &RecordedHold::default(),
            answers(vec![Ok(WorkerRestoreProgress::Ended {
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
        let outcome = await_unanswered(
            &RecordedHold::default(),
            answers(vec![
                Ok(WorkerRestoreProgress::Running { writes_began: true }),
                Err(unreachable()),
                Ok(WorkerRestoreProgress::Running { writes_began: true }),
                Ok(WorkerRestoreProgress::Finished {
                    outcome: WorkerRestoreResult::Failed {
                        reason: "the original database was put back unchanged".to_string(),
                    },
                }),
            ]),
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            WorkerRestoreResult::Failed {
                reason: "the original database was put back unchanged".to_string()
            }
        );
    }

    /// Ended after writing: the restore stopped, and the run fails saying
    /// the database must be checked.
    #[tokio::test]
    async fn an_unanswered_restore_that_stopped_after_writing_says_to_check_the_database() {
        let hold = RecordedHold::default();
        let message = await_unanswered(
            &hold,
            answers(vec![Ok(WorkerRestoreProgress::Ended {
                writes_began: true,
                reason: "the engine stopped".to_string(),
            })]),
        )
        .await
        .expect_err("no outcome")
        .to_string();
        assert!(message.contains("may be partially restored"), "{message}");
        assert!(message.contains("before restoring again"), "{message}");
        assert!(hold.events().is_empty(), "nothing runs: not held");
    }

    /// Unknown to the node it was sent to, or a node unreachable past the
    /// limit: the restore may still be writing, so the run is held (never
    /// failed), saying why, and ends only when an operator releases it.
    #[tokio::test]
    async fn an_unanswered_restore_with_no_known_outcome_is_held_until_released() {
        let cases = [
            (
                vec![Ok(WorkerRestoreProgress::Unknown)],
                "has no record of it",
            ),
            (
                vec![
                    Ok(WorkerRestoreProgress::Running { writes_began: true }),
                    Err(unreachable()),
                ],
                "could not be asked for its outcome",
            ),
        ];
        for (answered, expected) in cases {
            let hold = RecordedHold::releasing();
            let error = await_unanswered(&hold, answers(answered))
                .await
                .expect_err("released");
            let events = hold.events();
            assert_eq!(events.len(), 1, "held once: {events:?}");
            assert!(events[0].contains(expected), "{expected}: {events:?}");
            let RestoreError::Cancelled { phase, message, .. } = error else {
                panic!("{expected}: a released run ends cancelled, got {error:?}");
            };
            assert_eq!(phase, OUTCOME_UNKNOWN_PHASE);
            assert!(message.starts_with("Released by an operator"), "{message}");
            assert!(message.contains(expected), "{message}");
            assert!(message.contains("may be partially restored"), "{message}");
        }
    }

    /// A held run is not released while nobody releases it: it keeps asking,
    /// is let go of the hold when the node reports the restore running
    /// again, and settles on the node's outcome. There is no time limit
    /// that would free the service under a running restore.
    #[tokio::test]
    async fn a_held_restore_settles_when_its_node_answers_again() {
        let hold = RecordedHold::default();
        let mut script = vec![Ok(WorkerRestoreProgress::Running { writes_began: true })];
        // Unreachable well past the limit, then back.
        script.extend((0..40).map(|_| Err(unreachable())));
        script.push(Ok(WorkerRestoreProgress::Running { writes_began: true }));
        script.push(Ok(WorkerRestoreProgress::Finished {
            outcome: WorkerRestoreResult::Completed,
        }));
        let outcome = await_unanswered(&hold, answers(script)).await.unwrap();
        assert_eq!(outcome, WorkerRestoreResult::Completed);
        let events = hold.events();
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(events[0].starts_with("hold: "), "{events:?}");
        assert!(
            events[0].contains("Check that the node is online"),
            "{events:?}"
        );
        assert_eq!(events[1], "unhold");
    }

    struct RunFixture {
        _test_db: temps_test_utils::TestDatabase,
        db: Arc<DatabaseConnection>,
        enc: Arc<temps_core::EncryptionService>,
        user_id: i32,
        service_id: i32,
        backup_id: i32,
    }

    /// A `service_type` service with its stored login, a backup of another
    /// service and the user who restores it, in a real database (settling a
    /// run reads neither the node nor the backup's data). `None` without one.
    async fn run_fixture(service_type: &str) -> Option<RunFixture> {
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
            service_type: Set(service_type.to_string()),
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
            s3_location: Set(format!(
                "s3://backups/external_services/{service_type}/billing/walg"
            )),
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
                    restored_login_encrypted: Set(None),
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

        /// Finish `run_id` as an `engine` restore whose request went
        /// unanswered, from the node's answers, in its own task like the
        /// restore's. `origin` is the login of the backup's service, which
        /// the restored data answers to (`None`: the engine keeps the
        /// target's).
        fn finish_unanswered(
            &self,
            engine: WorkerRestoreEngine,
            run_id: i32,
            origin: Option<RestoredServiceCredentials>,
            progress: tokio::sync::watch::Receiver<WorkerRestoreProgress>,
        ) -> tokio::task::JoinHandle<Result<Option<i32>, RestoreError>> {
            let db = Arc::clone(&self.db);
            let enc = Arc::clone(&self.enc);
            let target_id = self.service_id;
            tokio::spawn(async move {
                let run = WorkerRestoreRun {
                    db: &db,
                    run_id,
                    node_id: 2,
                    target_id,
                    engine,
                    reconcile: origin.as_ref().map(|origin| (&enc, origin)),
                };
                let hold = RunHold::new(db.as_ref(), run_id, "restore");
                run.finish_with(
                    &FAST,
                    &hold,
                    Err::<WorkerRestoreResult, _>(ExternalServiceError::AgentUnanswered {
                        node: "worker-2".to_string(),
                        route: "/agent/services/mongodb/restore".to_string(),
                        detail: "operation timed out".to_string(),
                    }),
                    || std::future::ready(Ok(progress.borrow().clone())),
                )
                .await
            })
        }
    }

    /// The backup's service's login, as the restored data of `service_type`
    /// carries it.
    fn origin(service_type: &str) -> Option<RestoredServiceCredentials> {
        RestoredServiceCredentials::from_origin(
            service_type,
            &serde_json::json!({"username": "billing_owner", "password": "origin-password"}),
        )
    }

    /// For every worker engine: while the node still restores (past its
    /// write boundary) after the request went unanswered, the run stays
    /// `running`, a second destructive restore of the service is refused,
    /// and the stored login is untouched. The node's later success
    /// completes the run and runs the credential reconcile (the stored login
    /// becomes the restored one where the engine restores it); the service
    /// is free again after.
    #[tokio::test]
    async fn an_unanswered_restore_that_wrote_holds_the_service_until_it_succeeds() {
        let target_login =
            serde_json::json!({"username": "orders_owner", "password": "target-password"});
        let origin_login =
            serde_json::json!({"username": "billing_owner", "password": "origin-password"});
        // MariaDB's restored catalog carries the password, not the login name.
        let mariadb_login =
            serde_json::json!({"username": "orders_owner", "password": "origin-password"});
        let cases = [
            (PG, "postgres", origin("postgres"), origin_login.clone()),
            (
                WorkerRestoreEngine::Mongodb,
                "mongodb",
                origin("mongodb"),
                origin_login,
            ),
            (
                WorkerRestoreEngine::Mariadb,
                "mariadb",
                origin("mariadb"),
                mariadb_login,
            ),
            // Redis data has no login to reconcile.
            (
                WorkerRestoreEngine::Redis,
                "redis",
                None,
                target_login.clone(),
            ),
        ];
        for (engine, service_type, origin, restored_login) in cases {
            let Some(fixture) = run_fixture(service_type).await else {
                return;
            };
            let run = fixture.start_in_place().await.expect("first restore");
            let (report, progress) =
                tokio::sync::watch::channel(WorkerRestoreProgress::Running { writes_began: true });
            let finishing = fixture.finish_unanswered(engine, run.id, origin, progress);

            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(
                !finishing.is_finished(),
                "{engine:?}: waits for the node's outcome"
            );
            assert_eq!(fixture.run_status(run.id).await, "running", "{engine:?}");
            match fixture.start_in_place().await {
                Err(RestoreError::RestoreAlreadyActive {
                    service_id,
                    restore_run_id,
                }) => {
                    assert_eq!(service_id, fixture.service_id, "{engine:?}");
                    assert_eq!(restore_run_id, run.id, "{engine:?}");
                }
                other => panic!("{engine:?}: expected RestoreAlreadyActive, got {other:?}"),
            }
            assert_eq!(
                fixture.stored_login().await,
                target_login,
                "{engine:?}: the login is not changed while the outcome is unknown"
            );

            report
                .send(WorkerRestoreProgress::Finished {
                    outcome: WorkerRestoreResult::Completed,
                })
                .unwrap();
            let result = finishing.await.unwrap();
            assert!(result.is_ok(), "{engine:?}: {result:?}");
            persist_terminal_state(fixture.db.as_ref(), run.id, &result, TERMINAL_WRITE_RETRY)
                .await
                .unwrap();
            assert_eq!(fixture.run_status(run.id).await, "completed", "{engine:?}");
            assert_eq!(fixture.stored_login().await, restored_login, "{engine:?}");
            fixture
                .start_in_place()
                .await
                .expect("the service is free once the run completed");
        }
    }

    /// The node's later failure (the original put back) fails the run and
    /// leaves the stored login as it was.
    #[tokio::test]
    async fn an_unanswered_restore_that_wrote_fails_when_the_node_reports_failure() {
        let Some(fixture) = run_fixture("mongodb").await else {
            return;
        };
        let run = fixture.start_in_place().await.expect("first restore");
        let (report, progress) =
            tokio::sync::watch::channel(WorkerRestoreProgress::Running { writes_began: true });
        let finishing = fixture.finish_unanswered(
            WorkerRestoreEngine::Mongodb,
            run.id,
            origin("mongodb"),
            progress,
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!finishing.is_finished(), "waits for the node's outcome");
        assert!(matches!(
            fixture.start_in_place().await,
            Err(RestoreError::RestoreAlreadyActive { .. })
        ));

        report
            .send(WorkerRestoreProgress::Finished {
                outcome: WorkerRestoreResult::Failed {
                    reason: "mongorestore failed; the original data was put back".to_string(),
                },
            })
            .unwrap();
        let result = finishing.await.unwrap();
        let message = result.as_ref().expect_err("the node failed").to_string();
        assert!(
            message.contains("the original data was put back"),
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
    /// deadline, the run fails at once with the service unchanged, and the
    /// service is free for another restore.
    #[tokio::test]
    async fn an_unanswered_restore_before_writes_fails_and_frees_the_service() {
        let Some(fixture) = run_fixture("mariadb").await else {
            return;
        };
        let run = fixture.start_in_place().await.expect("first restore");
        let (_report, progress) = tokio::sync::watch::channel(WorkerRestoreProgress::Ended {
            writes_began: false,
            reason: "MariaDB restore of 'orders' passed its deadline".to_string(),
        });
        let result = fixture
            .finish_unanswered(
                WorkerRestoreEngine::Mariadb,
                run.id,
                origin("mariadb"),
                progress,
            )
            .await
            .unwrap();
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

    // ---- A restart while a worker restore runs

    fn service(
        service_type: &str,
        node_id: Option<i32>,
    ) -> temps_entities::external_services::Model {
        let now = Utc::now();
        temps_entities::external_services::Model {
            id: 11,
            name: "orders".to_string(),
            service_type: service_type.to_string(),
            version: None,
            status: "running".to_string(),
            created_at: now,
            updated_at: now,
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
                temps_entities::external_service_workload_mode::ServiceWorkloadMode::Enforce,
            log_ingest_lines_per_sec: None,
            log_ingest_kib_per_sec: None,
            healthcheck_interval_secs: None,
            healthcheck_start_interval_secs: None,
            healthcheck_start_period_secs: None,
            healthcheck_timeout_secs: None,
            healthcheck_retries: None,
        }
    }

    /// Every restore into a service on a worker node writes there, and is
    /// placed there by the one classification admission, the live dispatch
    /// and the startup reconciliation share: an agent engine, the agent's
    /// generic restore, or a job. Only control-plane services (and
    /// clusters, which restore here) write in this process.
    #[test]
    fn every_restore_into_a_worker_service_is_placed_on_its_node() {
        use RestorePlacement::{ControlPlane, Job, Node};
        let walg = "s3://backups/external_services/postgres/orders/walg";
        let dump = "s3://backups/external_services/postgres/orders/2026/dump.sql.gz";
        let cases = [
            ("postgres", walg, Node(PG)),
            ("postgres", dump, Job),
            ("mariadb", "k", Node(WorkerRestoreEngine::Mariadb)),
            ("mongodb", "k", Node(WorkerRestoreEngine::Mongodb)),
            ("redis", "k", Node(WorkerRestoreEngine::Redis)),
            ("s3", "k", Node(WorkerRestoreEngine::Data)),
        ];
        for (service_type, location, expected) in cases {
            assert_eq!(
                restore_placement(&service(service_type, Some(2)), location),
                expected,
                "{service_type} at {location}"
            );
            assert_eq!(
                restore_placement(&service(service_type, None), location),
                ControlPlane,
                "{service_type} on the control plane"
            );
        }
        let mut cluster = service("mongodb", Some(2));
        cluster.topology = "cluster".to_string();
        assert_eq!(restore_placement(&cluster, "k"), ControlPlane);
    }

    /// A restore run as the previous process left it, without a database.
    fn run_model(
        source_backup_id: i32,
        phase: &str,
        new_service: Option<&str>,
    ) -> temps_entities::restore_runs::Model {
        let now = Utc::now();
        temps_entities::restore_runs::Model {
            id: 77,
            source_backup_id,
            source_service_id: 11,
            target_service_id: None,
            target_service_name: new_service.map(str::to_string),
            mode: if new_service.is_some() {
                "new_service"
            } else {
                "in_place"
            }
            .to_string(),
            status: "running".to_string(),
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
            restored_login_encrypted: None,
        }
    }

    /// A restore into a new service never writes on a worker node: every
    /// worker path refuses it at admission (the new service would be
    /// created on the source's node), so a new-service run left active by a
    /// restart ran in this process. It is fenced and marked interrupted
    /// like any local restore, never resumed from a node.
    #[tokio::test]
    async fn a_new_service_restore_never_writes_on_a_worker_so_it_is_not_resumed_from_one() {
        let walg = "s3://backups/external_services/postgres/orders/walg";
        let dump = "s3://backups/external_services/postgres/orders/2026/dump.sql.gz";
        let to_new_service = RestoreRequestMode::NewService {
            name: "orders-copy".to_string(),
            parameter_overrides: serde_json::json!({}),
        };
        for (service_type, location) in [
            ("postgres", walg),
            ("postgres", dump),
            ("mariadb", "k"),
            ("mongodb", "k"),
            ("redis", "k"),
            ("s3", "k"),
        ] {
            let target = service(service_type, Some(2));
            assert_ne!(
                restore_placement(&target, location),
                RestorePlacement::ControlPlane
            );
            let refused = worker_restore_admitted(&target, location, &to_new_service)
                .expect_err("a worker path refuses a new service");
            assert!(
                refused.to_string().contains("worker node"),
                "{service_type}: {refused}"
            );
        }
        let run = run_model(5, "provision", Some("orders-copy"));
        assert!(
            ResumedWorkerRestore::of(
                &DatabaseConnection::Disconnected,
                &run,
                Some(&service("mongodb", Some(2)))
            )
            .await
            .is_none(),
            "a new-service run is reconciled as a local one"
        );
    }

    /// An in-place run into a worker's service whose backup cannot be read
    /// cannot be told which restore its node runs. It is held, never marked
    /// interrupted (its node may still be writing), and ends only when an
    /// operator releases it.
    #[tokio::test]
    async fn a_worker_restore_that_cannot_be_classified_is_held_until_released() {
        // An orphan-location run whose resume token lost its S3 source.
        let run = run_model(0, "restore", None);
        let resumed = ResumedWorkerRestore::of(
            &DatabaseConnection::Disconnected,
            &run,
            Some(&service("mongodb", Some(2))),
        )
        .await
        .expect("held, not reconciled as local");
        assert!(matches!(resumed.how, Resume::Unclassified { .. }));
        let hold = RecordedHold::releasing();
        let db = Arc::new(DatabaseConnection::Disconnected);
        let enc = Arc::new(temps_core::EncryptionService::new_from_password(
            "synthetic-restore-test-key",
        ));
        let error = resumed
            .settle_with(&db, &enc, None, &FAST, &hold, || {
                std::future::ready(Err::<WorkerRestoreProgress, _>("never asked"))
            })
            .await
            .expect_err("released");
        let events = hold.events();
        assert_eq!(events.len(), 1, "{events:?}");
        assert!(
            events[0].contains("could not tell which restore the node runs"),
            "{events:?}"
        );
        assert!(matches!(
            error,
            RestoreError::Cancelled { ref phase, .. } if phase == OUTCOME_UNKNOWN_PHASE
        ));
    }

    impl RunFixture {
        /// Put the target service on a worker node, which exists.
        async fn on_a_worker_node(&self) -> i32 {
            use sea_orm::{ActiveModelTrait, ActiveValue::Set};
            let node = temps_entities::nodes::ActiveModel {
                name: Set("worker-2".to_string()),
                token_hash: Set("unused".to_string()),
                token_encrypted: Set(None),
                address: Set("https://127.0.0.1:1".to_string()),
                private_address: Set("127.0.0.1".to_string()),
                role: Set("worker".to_string()),
                status: Set("online".to_string()),
                labels: Set(serde_json::json!({})),
                capacity: Set(serde_json::json!({})),
                dns_resolver_consecutive_failures: Set(0),
                build_limits: Set(None),
                public_ingress_enabled: Set(false),
                public_ingress_unsupported_reasons: Set(serde_json::json!([])),
                created_at: Set(Utc::now()),
                updated_at: Set(Utc::now()),
                ..Default::default()
            }
            .insert(self.db.as_ref())
            .await
            .expect("insert node");
            self.place_on(node.id).await;
            node.id
        }

        async fn place_on(&self, node_id: i32) {
            temps_entities::external_services::Entity::update_many()
                .col_expr(
                    temps_entities::external_services::Column::NodeId,
                    sea_orm::sea_query::Expr::value(node_id),
                )
                .filter(temps_entities::external_services::Column::Id.eq(self.service_id))
                .exec(self.db.as_ref())
                .await
                .expect("place the service on a node");
        }

        /// Move the fixture's backup to `location`.
        async fn backup_at(&self, location: &str) {
            temps_entities::backups::Entity::update_many()
                .col_expr(
                    temps_entities::backups::Column::S3Location,
                    sea_orm::sea_query::Expr::value(location),
                )
                .filter(temps_entities::backups::Column::Id.eq(self.backup_id))
                .exec(self.db.as_ref())
                .await
                .expect("move the backup");
        }

        async fn target(&self) -> temps_entities::external_services::Model {
            temps_entities::external_services::Entity::find_by_id(self.service_id)
                .one(self.db.as_ref())
                .await
                .unwrap()
                .unwrap()
        }

        async fn run(&self, run_id: i32) -> temps_entities::restore_runs::Model {
            temps_entities::restore_runs::Entity::find_by_id(run_id)
                .one(self.db.as_ref())
                .await
                .unwrap()
                .unwrap()
        }

        async fn set_phase(&self, run_id: i32, phase: &str) {
            temps_entities::restore_runs::Entity::update_many()
                .col_expr(
                    temps_entities::restore_runs::Column::Phase,
                    sea_orm::sea_query::Expr::value(phase),
                )
                .filter(temps_entities::restore_runs::Column::Id.eq(run_id))
                .exec(self.db.as_ref())
                .await
                .unwrap();
        }

        /// The run as the new process finds it at startup.
        async fn found_at_startup(&self, run_id: i32) -> ResumedWorkerRestore {
            let run = self.run(run_id).await;
            let target = self.target().await;
            ResumedWorkerRestore::of(self.db.as_ref(), &run, Some(&target))
                .await
                .expect("a worker restore is resumed")
        }

        /// Settle `resumed` as the startup task does, asking with `ask`, in
        /// its own task, holding the run on its row.
        fn settle_resumed<Ask, AskFut, E>(
            &self,
            resumed: ResumedWorkerRestore,
            origin: Option<RestoredServiceCredentials>,
            ask: Ask,
        ) -> tokio::task::JoinHandle<Result<Option<i32>, RestoreError>>
        where
            Ask: FnMut() -> AskFut + Send + 'static,
            AskFut: Future<Output = Result<WorkerRestoreProgress, E>> + Send,
            E: std::fmt::Display + Send,
        {
            let db = Arc::clone(&self.db);
            let enc = Arc::clone(&self.enc);
            tokio::spawn(async move {
                let hold = RunHold::new(db.as_ref(), resumed.run.id, &resumed.run.phase);
                resumed
                    .settle_with(&db, &enc, origin.as_ref(), &FAST, &hold, ask)
                    .await
            })
        }

        /// The run, once it is held; panics if it is not held in time.
        async fn held(&self, run_id: i32) -> temps_entities::restore_runs::Model {
            for _ in 0..200 {
                let run = self.run(run_id).await;
                if run.phase == OUTCOME_UNKNOWN_PHASE {
                    return run;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("restore run {run_id} was not held");
        }

        /// While a run is held or waiting: still `running`, another restore
        /// of the service is refused, and the stored login is untouched.
        async fn assert_holds_the_service(&self, run_id: i32, what: &str) {
            assert_eq!(self.run_status(run_id).await, "running", "{what}");
            assert!(
                matches!(
                    self.start_in_place().await,
                    Err(RestoreError::RestoreAlreadyActive { restore_run_id, .. })
                        if restore_run_id == run_id
                ),
                "{what}: the service stays held"
            );
            assert_eq!(
                self.stored_login().await,
                serde_json::json!({"username": "orders_owner", "password": "target-password"}),
                "{what}: the login is untouched"
            );
        }

        /// Record `result` as the run's terminal state; the service is free
        /// after.
        async fn settle(&self, run_id: i32, result: &Result<Option<i32>, RestoreError>) {
            persist_terminal_state(self.db.as_ref(), run_id, result, TERMINAL_WRITE_RETRY)
                .await
                .unwrap();
        }
    }

    /// The asks of a watched progress, as a node's answers.
    fn watched(
        progress: tokio::sync::watch::Receiver<WorkerRestoreProgress>,
    ) -> impl FnMut() -> std::future::Ready<Result<WorkerRestoreProgress, ExternalServiceError>>
           + Send
           + 'static {
        move || std::future::ready(Ok(progress.borrow().clone()))
    }

    /// Temps restarts while a node's engine restores: the run is resumed,
    /// stays `running` (a second restore is refused, the login untouched)
    /// while the node still works, and the node's later success completes it
    /// with the credential reconcile and the verify phase, as the live run
    /// would.
    #[tokio::test]
    async fn a_restart_mid_restore_resumes_and_settles_on_the_nodes_success() {
        for (engine, service_type) in [(PG, "postgres"), (WorkerRestoreEngine::Mongodb, "mongodb")]
        {
            let Some(fixture) = run_fixture(service_type).await else {
                return;
            };
            fixture.on_a_worker_node().await;
            let run = fixture.start_in_place().await.expect("first restore");
            let resumed = fixture.found_at_startup(run.id).await;
            assert!(
                matches!(resumed.how, Resume::Node { engine: found, .. } if found == engine),
                "{engine:?}"
            );

            let (report, progress) =
                tokio::sync::watch::channel(WorkerRestoreProgress::Running { writes_began: true });
            let settling = fixture.settle_resumed(resumed, origin(service_type), watched(progress));
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(!settling.is_finished(), "{engine:?}: waits for the node");
            fixture
                .assert_holds_the_service(run.id, &format!("{engine:?}"))
                .await;

            report
                .send(WorkerRestoreProgress::Finished {
                    outcome: WorkerRestoreResult::Completed,
                })
                .unwrap();
            let result = settling.await.unwrap();
            assert!(result.is_ok(), "{engine:?}: {result:?}");
            assert_eq!(fixture.run(run.id).await.phase, "verify", "{engine:?}");
            fixture.settle(run.id, &result).await;
            assert_eq!(fixture.run_status(run.id).await, "completed", "{engine:?}");
            assert_eq!(
                fixture.stored_login().await,
                serde_json::json!({"username": "billing_owner", "password": "origin-password"}),
                "{engine:?}: the credential reconcile ran"
            );
            fixture
                .start_in_place()
                .await
                .expect("the service is free once the run completed");
        }
    }

    /// The origin's login after the restore started: another restore into
    /// the origin, or a password reset, changed it.
    fn origin_changed_since() -> Option<RestoredServiceCredentials> {
        RestoredServiceCredentials::from_origin(
            "postgres",
            &serde_json::json!({"username": "billing_owner", "password": "changed-since"}),
        )
    }

    /// The live failure: after a restart, a cross-service restore on a
    /// worker patched its target to the origin's login as it was at the
    /// restart, not the one the restored data answers to. The login is now
    /// recorded, encrypted, on the run before the restore is sent, and the
    /// resumed run settles with it.
    #[tokio::test]
    async fn a_resumed_restore_patches_the_login_recorded_when_it_started() {
        let Some(fixture) = run_fixture("postgres").await else {
            return;
        };
        fixture.on_a_worker_node().await;
        let run = fixture.start_in_place().await.expect("first restore");
        let started_with = origin("postgres");
        record_restored_login(
            fixture.db.as_ref(),
            &fixture.enc,
            run.id,
            started_with.as_ref(),
        )
        .await
        .expect("record the login");
        let sealed = fixture
            .run(run.id)
            .await
            .restored_login_encrypted
            .expect("recorded");
        assert!(
            !sealed.contains("origin-password"),
            "the recorded login is encrypted"
        );

        let resumed_run = fixture.run(run.id).await;
        let origin = resumed_origin_credentials(&fixture.enc, &resumed_run, async {
            origin_changed_since()
        })
        .await;
        assert_eq!(origin, started_with, "the login recorded at the start wins");

        let resumed = fixture.found_at_startup(run.id).await;
        let settling = fixture.settle_resumed(resumed, origin, || {
            std::future::ready(Ok::<_, ExternalServiceError>(
                WorkerRestoreProgress::Finished {
                    outcome: WorkerRestoreResult::Completed,
                },
            ))
        });
        let result = settling.await.unwrap();
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            fixture.stored_login().await,
            serde_json::json!({"username": "billing_owner", "password": "origin-password"}),
            "the target answers to the login its restored data uses"
        );
    }

    /// A run that recorded no login to reconcile settles with none, whatever
    /// the origin's login is now; only a run from before the login was
    /// recorded falls back to the origin's current one.
    #[tokio::test]
    async fn a_resumed_restore_reads_only_its_own_record_of_the_login() {
        let Some(fixture) = run_fixture("postgres").await else {
            return;
        };
        let run = fixture.start_in_place().await.expect("first restore");
        record_restored_login(fixture.db.as_ref(), &fixture.enc, run.id, None)
            .await
            .expect("record none");
        let none_recorded = fixture.run(run.id).await;
        assert!(none_recorded.restored_login_encrypted.is_some());
        assert_eq!(
            resumed_origin_credentials(&fixture.enc, &none_recorded, async {
                origin_changed_since()
            })
            .await,
            None,
            "none to reconcile stays none"
        );

        let before_upgrade = temps_entities::restore_runs::Model {
            restored_login_encrypted: None,
            ..none_recorded.clone()
        };
        assert_eq!(
            resumed_origin_credentials(&fixture.enc, &before_upgrade, async {
                origin_changed_since()
            })
            .await,
            origin_changed_since(),
            "a run from before the record falls back to the origin's login"
        );

        let unreadable = temps_entities::restore_runs::Model {
            restored_login_encrypted: Some("not-a-sealed-value".to_string()),
            ..none_recorded
        };
        assert_eq!(
            resumed_origin_credentials(&fixture.enc, &unreadable, async { origin_changed_since() })
                .await,
            origin_changed_since(),
            "an unreadable record falls back too"
        );

        assert!(matches!(
            record_restored_login(fixture.db.as_ref(), &fixture.enc, 999_999, None).await,
            Err(RestoreError::RestoreRunNotFound {
                restore_run_id: 999_999
            })
        ));
    }

    /// A storage service (or another engine's dump) on a worker is restored
    /// by the agent's generic restore, which now records its outcome too: a
    /// restart mid-restore resumes from it instead of interrupting it.
    #[tokio::test]
    async fn a_restart_mid_generic_agent_restore_resumes_from_its_node() {
        let Some(fixture) = run_fixture("s3").await else {
            return;
        };
        fixture.on_a_worker_node().await;
        let run = fixture.start_in_place().await.expect("restore");
        let resumed = fixture.found_at_startup(run.id).await;
        assert!(matches!(
            resumed.how,
            Resume::Node {
                engine: WorkerRestoreEngine::Data,
                ..
            }
        ));
        let (report, progress) =
            tokio::sync::watch::channel(WorkerRestoreProgress::Running { writes_began: true });
        let settling = fixture.settle_resumed(resumed, None, watched(progress));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!settling.is_finished(), "waits for the node");
        fixture
            .assert_holds_the_service(run.id, "storage mirror")
            .await;
        report
            .send(WorkerRestoreProgress::Finished {
                outcome: WorkerRestoreResult::Completed,
            })
            .unwrap();
        let result = settling.await.unwrap();
        assert!(result.is_ok(), "{result:?}");
        fixture.settle(run.id, &result).await;
        assert_eq!(fixture.run_status(run.id).await, "completed");
    }

    /// The plane's records of a restore run's job, as a test changes them.
    struct WatchedJobs(tokio::sync::watch::Receiver<Vec<workload_job::RestoreJobRecord>>);

    #[async_trait::async_trait]
    impl workload_job::RestoreJobLookup for WatchedJobs {
        async fn restore_jobs(
            &self,
            _service_id: i32,
            _restore_run_id: i32,
        ) -> Result<Vec<workload_job::RestoreJobRecord>, temps_workload_plane::PlaneError> {
            Ok(self.0.borrow().clone())
        }
    }

    fn job(
        workload_id: i64,
        phase: temps_workload::WorkloadPhase,
    ) -> workload_job::RestoreJobRecord {
        workload_job::RestoreJobRecord {
            workload_id,
            status: temps_workload::WorkloadStatus {
                phase,
                exit_code: (phase == temps_workload::WorkloadPhase::Failed)
                    .then_some(workload_job::exit::APPLY_FAILED),
                ..Default::default()
            },
        }
    }

    /// Asks the plane's records of run `run_id`'s job, as the startup task
    /// does through the plane.
    fn job_asks(
        jobs: tokio::sync::watch::Receiver<Vec<workload_job::RestoreJobRecord>>,
        service_id: i32,
        node_id: i32,
        run_id: i32,
    ) -> impl FnMut() -> std::pin::Pin<
        Box<
            dyn Future<Output = Result<WorkerRestoreProgress, temps_workload_plane::PlaneError>>
                + Send,
        >,
    > + Send
           + 'static {
        let jobs = Arc::new(WatchedJobs(jobs));
        move || {
            let jobs = Arc::clone(&jobs);
            Box::pin(async move {
                workload_job::restore_job_progress(jobs.as_ref(), service_id, node_id, run_id)
                    .await
                    .map(job_outcome)
            })
        }
    }

    /// Temps restarts while a PostgreSQL dump is applied by a job on the
    /// database's node: the run is resumed from the job, through the
    /// workload plane. It holds the service while the job runs and
    /// completes on the job's success; a failed job fails it with the job's
    /// reason.
    #[tokio::test]
    async fn a_restart_mid_job_restore_resumes_from_the_plane_and_settles_on_the_job() {
        let Some(fixture) = run_fixture("postgres").await else {
            return;
        };
        let node_id = fixture.on_a_worker_node().await;
        fixture
            .backup_at("s3://backups/external_services/postgres/billing/2026/dump.sql.gz")
            .await;
        use temps_workload::WorkloadPhase;

        // Succeeds.
        let run = fixture.start_in_place().await.expect("restore");
        let resumed = fixture.found_at_startup(run.id).await;
        assert!(matches!(resumed.how, Resume::Job { .. }));
        let (report, jobs) = tokio::sync::watch::channel(vec![job(5, WorkloadPhase::Running)]);
        let settling = fixture.settle_resumed(
            resumed,
            None,
            job_asks(jobs, fixture.service_id, node_id, run.id),
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!settling.is_finished(), "waits for the job");
        fixture.assert_holds_the_service(run.id, "job").await;
        report.send(vec![job(5, WorkloadPhase::Succeeded)]).unwrap();
        let result = settling.await.unwrap();
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(fixture.run(run.id).await.phase, "verify");
        fixture.settle(run.id, &result).await;
        assert_eq!(fixture.run_status(run.id).await, "completed");

        // Fails.
        let run = fixture.start_in_place().await.expect("the service is free");
        let resumed = fixture.found_at_startup(run.id).await;
        let (_report, jobs) = tokio::sync::watch::channel(vec![job(6, WorkloadPhase::Failed)]);
        let result = fixture
            .settle_resumed(
                resumed,
                None,
                job_asks(jobs, fixture.service_id, node_id, run.id),
            )
            .await
            .unwrap();
        let message = result.as_ref().expect_err("the job failed").to_string();
        assert!(
            message.contains("psql could not apply the dump"),
            "{message}"
        );
        fixture.settle(run.id, &result).await;
        assert_eq!(fixture.run_status(run.id).await, "failed");
    }

    /// The plane has no job for the run: nothing restores, so the run fails
    /// rather than waits, saying whether the database may have changed (the
    /// job may have run and been removed) or not (it was never started).
    #[tokio::test]
    async fn a_restart_with_no_job_left_fails_the_run_and_frees_the_service() {
        let Some(fixture) = run_fixture("postgres").await else {
            return;
        };
        let node_id = fixture.on_a_worker_node().await;
        fixture
            .backup_at("s3://backups/external_services/postgres/billing/2026/dump.sql.gz")
            .await;
        for (phase, expected) in [
            (
                "restore",
                "Nothing is restoring now, but the database may have been changed",
            ),
            (
                "prepare",
                "The database was not changed; start the restore again",
            ),
        ] {
            let run = fixture.start_in_place().await.expect("restore");
            fixture.set_phase(run.id, phase).await;
            let resumed = fixture.found_at_startup(run.id).await;
            let (_report, jobs) = tokio::sync::watch::channel(Vec::new());
            let result = fixture
                .settle_resumed(
                    resumed,
                    None,
                    job_asks(jobs, fixture.service_id, node_id, run.id),
                )
                .await
                .unwrap();
            let message = result.as_ref().expect_err("no job").to_string();
            assert!(message.contains(expected), "{phase}: {message}");
            fixture.settle(run.id, &result).await;
            assert_eq!(fixture.run_status(run.id).await, "failed", "{phase}");
        }
        fixture
            .start_in_place()
            .await
            .expect("the service is free once the run failed");
    }

    /// Release a held run as the console does: `POST /restore-runs/{id}/cancel`.
    async fn release(fixture: &RunFixture, run: &temps_entities::restore_runs::Model) {
        assert_eq!(
            crate::services::restore_cancel::not_cancellable_reason(run),
            None,
            "a held run can be released"
        );
        assert!(
            crate::services::restore_cancel::record_cancellation(
                fixture.db.as_ref(),
                run,
                fixture.user_id
            )
            .await
            .unwrap(),
            "the release is recorded"
        );
    }

    /// The node has no record of a restore it may have received (its agent
    /// restarted): the restore may still be writing, so the run is held,
    /// saying why and what to check, until an operator releases it. A run
    /// that never sent its restore fails at once with the database
    /// unchanged.
    #[tokio::test]
    async fn a_node_with_no_record_of_a_sent_restore_holds_it_until_released() {
        let Some(fixture) = run_fixture("mariadb").await else {
            return;
        };
        fixture.on_a_worker_node().await;

        let run = fixture.start_in_place().await.expect("restore");
        let resumed = fixture.found_at_startup(run.id).await;
        let (_report, progress) = tokio::sync::watch::channel(WorkerRestoreProgress::Unknown);
        let settling = fixture.settle_resumed(resumed, origin("mariadb"), watched(progress));
        let held = fixture.held(run.id).await;
        let message = held.error_message.clone().unwrap_or_default();
        assert!(message.contains("has no record of it"), "{message}");
        assert!(message.contains("phase 'restore'"), "{message}");
        assert!(
            message.contains("release the service by cancelling this restore"),
            "{message}"
        );
        fixture.assert_holds_the_service(run.id, "held").await;
        release(&fixture, &held).await;
        let result = settling.await.unwrap();
        assert!(
            matches!(&result, Err(RestoreError::Cancelled { .. })),
            "{result:?}"
        );
        fixture.settle(run.id, &result).await;
        let released = fixture.run(run.id).await;
        assert_eq!(released.status, "cancelled");
        let message = released.error_message.unwrap_or_default();
        assert!(message.starts_with("Released by an operator"), "{message}");
        assert!(message.contains("may be partially restored"), "{message}");

        let run = fixture.start_in_place().await.expect("the service is free");
        fixture.set_phase(run.id, "prepare").await;
        let resumed = fixture.found_at_startup(run.id).await;
        let (_report, progress) = tokio::sync::watch::channel(WorkerRestoreProgress::Unknown);
        let result = fixture
            .settle_resumed(resumed, origin("mariadb"), watched(progress))
            .await
            .unwrap();
        let message = result.as_ref().expect_err("never sent").to_string();
        assert!(
            message.contains("was not changed; start the restore again"),
            "{message}"
        );
        fixture.settle(run.id, &result).await;
        assert_eq!(fixture.run_status(run.id).await, "failed");
        fixture
            .start_in_place()
            .await
            .expect("the service is free once the run failed");
    }

    /// The node does not come back after the restart: the run is never
    /// failed (which would free the service under a restore that may still
    /// run). Past the limit it is held, naming the node and what to check,
    /// and the service stays held until an operator releases it.
    #[tokio::test]
    async fn a_node_unreachable_after_a_restart_holds_the_service_until_released() {
        let Some(fixture) = run_fixture("redis").await else {
            return;
        };
        let node_id = fixture.on_a_worker_node().await;
        let run = fixture.start_in_place().await.expect("restore");
        let resumed = fixture.found_at_startup(run.id).await;
        assert!(matches!(
            resumed.how,
            Resume::Node {
                engine: WorkerRestoreEngine::Redis,
                ..
            }
        ));
        let settling = fixture.settle_resumed(resumed, None, || {
            std::future::ready(Err::<WorkerRestoreProgress, _>(unreachable()))
        });
        let held = fixture.held(run.id).await;
        let message = held.error_message.clone().unwrap_or_default();
        assert!(
            message.contains(&format!(
                "Temps restarted during the Redis restore (phase 'restore'), and node {node_id} \
                 could not be asked for its outcome"
            )),
            "{message}"
        );
        assert!(
            message.contains("Check that the node is online"),
            "{message}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!settling.is_finished(), "never given up on");
        fixture.assert_holds_the_service(run.id, "held").await;

        release(&fixture, &held).await;
        let result = settling.await.unwrap();
        fixture.settle(run.id, &result).await;
        assert_eq!(fixture.run_status(run.id).await, "cancelled");
        fixture
            .start_in_place()
            .await
            .expect("the service is free once released");
    }

    /// Temps restarts while a run is held. The API accepted a release (the
    /// run's stored phase is cancellable); the node then reports the restore
    /// running. The release is honoured on the first pass, and the service
    /// is free; it is not left blocked until the restore ends.
    #[tokio::test]
    async fn a_release_accepted_before_a_restart_is_honoured_while_the_node_runs() {
        let Some(fixture) = run_fixture("mongodb").await else {
            return;
        };
        fixture.on_a_worker_node().await;
        let run = fixture.start_in_place().await.expect("restore");
        RunHold::new(fixture.db.as_ref(), run.id, "restore")
            .hold("restore run: node 2 could not be asked for its outcome.")
            .await;
        let held = fixture.held(run.id).await;
        release(&fixture, &held).await;

        let resumed = fixture.found_at_startup(run.id).await;
        let (_report, progress) =
            tokio::sync::watch::channel(WorkerRestoreProgress::Running { writes_began: true });
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            fixture.settle_resumed(resumed, origin("mongodb"), watched(progress)),
        )
        .await
        .expect("the release is not ignored")
        .unwrap();
        let message = result.as_ref().expect_err("released").to_string();
        assert!(message.starts_with("Released by an operator"), "{message}");
        assert!(
            message.contains("node 2 could not be asked for its outcome"),
            "{message}"
        );
        fixture.settle(run.id, &result).await;
        assert_eq!(fixture.run_status(run.id).await, "cancelled");
        fixture
            .start_in_place()
            .await
            .expect("the service is free once released");
    }

    /// Temps restarts while a run is held, and the node reports the restore
    /// running: the run is let go of its hold (back to `restore`, its
    /// message cleared, no longer releasable) and settles on the node's
    /// outcome.
    #[tokio::test]
    async fn a_run_held_before_a_restart_is_let_go_when_its_node_runs_again() {
        let Some(fixture) = run_fixture("mongodb").await else {
            return;
        };
        fixture.on_a_worker_node().await;
        let run = fixture.start_in_place().await.expect("restore");
        RunHold::new(fixture.db.as_ref(), run.id, "restore")
            .hold("restore run: node 2 could not be asked for its outcome.")
            .await;
        fixture.held(run.id).await;

        let resumed = fixture.found_at_startup(run.id).await;
        let (report, progress) =
            tokio::sync::watch::channel(WorkerRestoreProgress::Running { writes_began: true });
        let settling = fixture.settle_resumed(resumed, origin("mongodb"), watched(progress));
        let mut unheld = None;
        for _ in 0..200 {
            let current = fixture.run(run.id).await;
            if current.phase == "restore" {
                unheld = Some(current);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let unheld = unheld.expect("the hold is let go");
        assert_eq!(unheld.error_message, None);
        assert!(
            crate::services::restore_cancel::not_cancellable_reason(&unheld).is_some(),
            "a running restore cannot be released"
        );
        fixture.assert_holds_the_service(run.id, "running").await;

        report
            .send(WorkerRestoreProgress::Finished {
                outcome: WorkerRestoreResult::Completed,
            })
            .unwrap();
        let result = settling.await.unwrap();
        assert!(result.is_ok(), "{result:?}");
        fixture.settle(run.id, &result).await;
        assert_eq!(fixture.run_status(run.id).await, "completed");
    }

    /// A held live run whose node answers again is let go of its hold: back
    /// to its phase, without the message. Both worker phases are ones the
    /// schema accepts.
    #[tokio::test]
    async fn a_held_run_returns_to_its_phase_when_its_node_answers() {
        let Some(fixture) = run_fixture("mongodb").await else {
            return;
        };
        let run = fixture.start_in_place().await.expect("restore");
        let hold = RunHold::new(fixture.db.as_ref(), run.id, "restore");
        hold.hold("restore run: node 2 is unreachable.").await;
        let held = fixture.run(run.id).await;
        assert_eq!(held.phase, OUTCOME_UNKNOWN_PHASE);
        assert!(!hold.released().await);
        hold.unhold().await;
        let resumed = fixture.run(run.id).await;
        assert_eq!(resumed.phase, "restore");
        assert_eq!(resumed.error_message, None);
        assert_eq!(resumed.status, "running");

        // A busy node's wait is a phase the schema accepts too.
        crate::services::restore_cancel::enter_phase_unless_cancelled(
            fixture.db.as_ref(),
            run.id,
            crate::services::restore_cancel::WAITING_FOR_NODE_PHASE,
        )
        .await
        .expect("waiting_for_node is recorded");
        assert_eq!(
            fixture.run(run.id).await.phase,
            crate::services::restore_cancel::WAITING_FOR_NODE_PHASE
        );
    }
}
