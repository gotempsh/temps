// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The reconciler (ADR-055 §2): level-triggered and idempotent.
//!
//! A pass reads the workloads that need work, compares what each should be
//! with what the driver reports, and fixes the difference. Nothing it does
//! depends on remembering the previous pass, so a process that died
//! mid-run is healed by the next process's first pass: an instance that
//! exited meanwhile is recorded, one past its deadline is stopped and
//! removed, a container no row tracks is removed.
//!
//! Phase 2 drives `RunToCompletion` workloads on the control plane with
//! Kubernetes Job semantics:
//!
//! - **maximum run time**: `timeout_secs` after the workload was created,
//!   its instance is stopped and removed and the run fails with
//!   `DeadlineExceeded` (`activeDeadlineSeconds`);
//! - **retries**: a failed attempt is retried up to `retry_limit` more
//!   times, after a back-off of 10 s doubling to at most 6 min
//!   (`backoffLimit`);
//! - **time-to-live**: `ttl_secs_after_finished` after it finished, a run
//!   is deleted: its container removed, its finalizers run, its row gone
//!   (`ttlSecondsAfterFinished`);
//! - **orphans**: a container carrying this plane's labels that no instance
//!   row tracks is removed on the slow resync.
//!
//! Passes are triggered by an in-process notify (apply, delete, an
//! instance's exit reported by the driver), at startup, at the earliest
//! deadline, expiry or back-off, and on the slow resync. Each pass reads
//! only the rows that need work, through partial indexes, at most
//! [`PASS_LIMIT`] at a time, and writes status only when it changed.

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use futures::StreamExt;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    Set,
};
use temps_entities::{workload_instances, workloads};
use temps_workload::{
    labels as workload_labels, ConditionKind, ConditionStatus, DriverError, InstanceHandle,
    InstanceLimits, InstanceState, Lifetime, LimitSupport, LogQuery, Resources, RuntimeDriver,
    WorkloadPhase, WorkloadStatus,
};

use crate::admission::DEFAULT_JOB_TTL_SECS;
use crate::env;
use crate::error::PlaneError;
use crate::events;
use crate::labels;
use crate::node::Placed;
use crate::plane::{
    decode_status, encode_status, view_from, WorkloadPlane, WorkloadView, NO_CONTROL_PLANE_DRIVER,
};

/// Most workloads one pass reads. A pass that hits it asks for another.
pub const PASS_LIMIT: u64 = 256;
/// Most bytes of output kept for a finished run.
pub const OUTPUT_TAIL_BYTES: usize = 4_096;
const BACKOFF_BASE_SECS: i64 = 10;
const BACKOFF_MAX_SECS: i64 = 360;
const LOGS_TIMEOUT: StdDuration = StdDuration::from_secs(5);

pub(crate) const PHASE_PENDING: &str = "pending";
pub(crate) const PHASE_RUNNING: &str = "running";
pub(crate) const PHASE_SUCCEEDED: &str = "succeeded";
pub(crate) const PHASE_FAILED: &str = "failed";

/// Where a pass's batch ended in the ring of workloads that need work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Batch {
    /// Full, and rows with higher ids remain.
    Continues,
    /// Full after wrapping to the lowest ids; rows remain after it.
    Wrapped,
    /// Every row that needs work was read.
    Complete,
}

/// What a pass did, and when the next one is due.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PassOutcome {
    pub reconciled: usize,
    pub failed: usize,
    pub orphans_removed: usize,
    pub events_pruned: u64,
    /// The earliest deadline, expiry or back-off among the workloads read.
    pub requeue_at: Option<DateTime<Utc>>,
}

fn earliest(a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn backoff(attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(16);
    Duration::seconds((BACKOFF_BASE_SECS << exponent).min(BACKOFF_MAX_SECS))
}

pub(crate) fn instance_name(scope: &str, kind: &str, workload_id: i64, attempt: u32) -> String {
    format!("{scope}-{kind}-{workload_id}-a{attempt}")
}

pub(crate) fn limits_from(resources: &Resources) -> InstanceLimits {
    let Some(limits) = resources.limits else {
        return InstanceLimits::default();
    };
    let memory_bytes = limits.memory_mb.map(|mb| i64::from(mb) * 1024 * 1024);
    InstanceLimits {
        nano_cpus: limits.cpu_millis.map(|m| i64::from(m) * 1_000_000),
        memory_bytes,
        memory_swap_bytes: memory_bytes,
        pids: limits.pids.map(i64::from),
        disk_mb: limits.disk_mb,
        disk_io_bytes_per_sec: limits
            .disk_io_mbps
            .map(|mbps| i64::from(mbps) * 1024 * 1024),
        network_bytes_per_sec: limits
            .network_mbps
            .map(|mbps| i64::from(mbps) * 1024 * 1024),
    }
}

/// Mark the run failed: `Completed` is `False` with why.
fn fail(status: &mut WorkloadStatus, reason: &str, message: &str, now: DateTime<Utc>) {
    status.set_condition(
        ConditionKind::Completed,
        ConditionStatus::False,
        reason,
        message,
        now,
    );
    status.ready_instances = 0;
    status.phase = WorkloadPhase::Failed;
}

pub(crate) fn is_finished(instance: &workload_instances::Model) -> bool {
    instance.phase == PHASE_SUCCEEDED || instance.phase == PHASE_FAILED
}

pub(crate) fn handle_of(
    instance: &workload_instances::Model,
) -> Result<InstanceHandle, PlaneError> {
    serde_json::from_value(instance.handle.clone()).map_err(|e| PlaneError::Corrupt {
        workload_id: instance.workload_id,
        field: "instance handle",
        reason: format!("instance {}: {e}", instance.id),
    })
}

pub(crate) fn driver_error(
    workload_id: i64,
    operation: &'static str,
) -> impl FnOnce(DriverError) -> PlaneError {
    move |source| PlaneError::Driver {
        workload_id,
        operation,
        source,
    }
}

/// Keeps the last `max` bytes, cut at a line (or char) boundary.
fn tail_of(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let cut = &text[start..];
    match cut.find('\n') {
        Some(newline) if newline + 1 < cut.len() => cut[newline + 1..].to_string(),
        _ => cut.to_string(),
    }
}

/// The reconciler loop. Runs until aborted.
pub(crate) async fn run(plane: Arc<WorkloadPlane>) {
    tracing::info!(scope = %plane.config.scope, "workload reconciler started");
    let mut next_full = tokio::time::Instant::now();
    loop {
        let full = tokio::time::Instant::now() >= next_full;
        let outcome = plane.reconcile_at(Utc::now(), full).await;
        if full {
            next_full = tokio::time::Instant::now() + plane.config.resync_interval;
        }
        let mut sleep_for = next_full.saturating_duration_since(tokio::time::Instant::now());
        if let Some(at) = outcome.requeue_at {
            let until = (at - Utc::now()).to_std().unwrap_or(StdDuration::ZERO);
            sleep_for = sleep_for.min(until);
        }
        // A floor, so a workload whose requeue time has passed cannot spin
        // the loop.
        let sleep_for = sleep_for.max(StdDuration::from_millis(100));
        tokio::select! {
            _ = plane.notify.notified() => {}
            _ = tokio::time::sleep(sleep_for) => {}
        }
    }
}

impl WorkloadPlane {
    /// One pass at `now`. `full` adds orphan cleanup and event pruning (the
    /// startup pass and the slow resync). Errors are logged per workload and
    /// never stop the pass.
    pub async fn reconcile_at(&self, now: DateTime<Utc>, full: bool) -> PassOutcome {
        let poll = Duration::from_std(self.config.poll_interval).unwrap_or(Duration::seconds(5));
        let mut outcome = PassOutcome::default();
        let (rows, batch) = match self.rows_needing_work(now).await {
            Ok(read) => read,
            Err(error) => {
                tracing::error!(scope = %self.config.scope, "reconcile pass could not read workloads: {error}");
                outcome.failed += 1;
                outcome.requeue_at = Some(now + poll);
                return outcome;
            }
        };
        match batch {
            // More rows after this batch: read them now.
            Batch::Continues => outcome.requeue_at = Some(now),
            // The batch wrapped past the highest id and more remain: the
            // next batch is due within a poll interval, not immediately, so
            // a plane with more stable workloads than one batch does not
            // re-read them in a hot loop.
            Batch::Wrapped => outcome.requeue_at = Some(now + poll),
            Batch::Complete => {}
        }
        let mut instances = match self.instances_of(&rows).await {
            Ok(instances) => instances,
            Err(error) => {
                tracing::error!(scope = %self.config.scope, "reconcile pass could not read instances: {error}");
                outcome.failed += 1;
                outcome.requeue_at = Some(now + poll);
                return outcome;
            }
        };
        for row in rows {
            let id = row.id;
            let owned = instances.remove(&id).unwrap_or_default();
            match self.reconcile_one(row, owned, now).await {
                Ok(requeue) => {
                    outcome.reconciled += 1;
                    outcome.requeue_at = earliest(outcome.requeue_at, requeue);
                }
                Err(error) => {
                    outcome.failed += 1;
                    tracing::error!(
                        workload_id = id,
                        "reconciling workload {id} failed: {error}"
                    );
                    outcome.requeue_at = earliest(outcome.requeue_at, Some(now + poll));
                }
            }
        }
        match self.next_expiry(now).await {
            Ok(next) => outcome.requeue_at = earliest(outcome.requeue_at, next),
            Err(error) => tracing::error!("reading the next workload expiry failed: {error}"),
        }
        if full {
            for driver in self.control_plane_drivers() {
                outcome.orphans_removed += self.sweep_orphans(&driver).await;
            }
            match events::prune(self.db.as_ref(), now).await {
                Ok(pruned) => outcome.events_pruned = pruned,
                Err(error) => tracing::error!("pruning workload events failed: {error}"),
            }
        }
        outcome
    }

    /// Live unfinished workloads, workloads holding finalizers, expired
    /// ones and deleted ones: each through its partial index. Reads at most
    /// [`crate::PlaneConfig::pass_limit`] rows, starting after the pass
    /// cursor and wrapping to the lowest ids, so every row is reached even
    /// when more than one batch needs work.
    async fn rows_needing_work(
        &self,
        now: DateTime<Utc>,
    ) -> Result<(Vec<workloads::Model>, Batch), PlaneError> {
        let limit = self.config.pass_limit.max(1);
        let after = self.pass_cursor.load(Ordering::Relaxed);
        let mut rows = self
            .rows_in(now, workloads::Column::Id.gt(after), limit)
            .await?;
        if rows.len() as u64 >= limit {
            self.pass_cursor
                .store(rows.last().map_or(0, |row| row.id), Ordering::Relaxed);
            return Ok((rows, Batch::Continues));
        }
        if after == 0 {
            return Ok((rows, Batch::Complete));
        }
        // The tail of the ring was short: top the batch up from the start.
        let room = limit - rows.len() as u64;
        let head = self
            .rows_in(now, workloads::Column::Id.lte(after), room)
            .await?;
        let filled = head.len() as u64 >= room;
        let head_last = head.last().map(|row| row.id);
        rows.extend(head);
        match (filled, head_last) {
            (true, Some(last)) if last < after => {
                self.pass_cursor.store(last, Ordering::Relaxed);
                Ok((rows, Batch::Wrapped))
            }
            _ => {
                self.pass_cursor.store(0, Ordering::Relaxed);
                Ok((rows, Batch::Complete))
            }
        }
    }

    async fn rows_in(
        &self,
        now: DateTime<Utc>,
        range: sea_orm::sea_query::SimpleExpr,
        limit: u64,
    ) -> Result<Vec<workloads::Model>, PlaneError> {
        workloads::Entity::find()
            .filter(range)
            .filter(
                Condition::any()
                    .add(
                        Condition::all()
                            .add(workloads::Column::DeletedAt.is_null())
                            .add(
                                workloads::Column::Phase.is_not_in([PHASE_SUCCEEDED, PHASE_FAILED]),
                            ),
                    )
                    .add(Expr::cust("cardinality(finalizers) > 0"))
                    .add(
                        Condition::all()
                            .add(workloads::Column::DeletedAt.is_null())
                            .add(workloads::Column::ExpiresAt.lte(now)),
                    )
                    .add(workloads::Column::DeletedAt.is_not_null()),
            )
            .order_by_asc(workloads::Column::Id)
            .limit(limit)
            .all(self.db.as_ref())
            .await
            .map_err(PlaneError::db("reading the workloads that need work", "*"))
    }

    /// Every instance of `rows`, in one query, by workload, oldest first.
    async fn instances_of(
        &self,
        rows: &[workloads::Model],
    ) -> Result<BTreeMap<i64, Vec<workload_instances::Model>>, PlaneError> {
        let mut by_workload: BTreeMap<i64, Vec<workload_instances::Model>> = BTreeMap::new();
        if rows.is_empty() {
            return Ok(by_workload);
        }
        let ids: Vec<i64> = rows.iter().map(|row| row.id).collect();
        let instances = workload_instances::Entity::find()
            .filter(workload_instances::Column::WorkloadId.is_in(ids))
            .order_by_asc(workload_instances::Column::WorkloadId)
            .order_by_asc(workload_instances::Column::Attempt)
            .all(self.db.as_ref())
            .await
            .map_err(PlaneError::db("reading instances", "*"))?;
        for instance in instances {
            by_workload
                .entry(instance.workload_id)
                .or_default()
                .push(instance);
        }
        Ok(by_workload)
    }

    async fn next_expiry(&self, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>, PlaneError> {
        let next = workloads::Entity::find()
            .filter(workloads::Column::DeletedAt.is_null())
            .filter(workloads::Column::ExpiresAt.gt(now))
            .order_by_asc(workloads::Column::ExpiresAt)
            .limit(1)
            .one(self.db.as_ref())
            .await
            .map_err(PlaneError::db("reading the next expiry", "*"))?;
        Ok(next.and_then(|row| row.expires_at))
    }

    async fn reconcile_one(
        &self,
        row: workloads::Model,
        instances: Vec<workload_instances::Model>,
        now: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>, PlaneError> {
        let view = view_from(row.clone(), instances.last())?;
        if row.deleted_at.is_some() {
            return self.reconcile_deleted(row, view, instances, now).await;
        }
        if view.status.phase.is_terminal() {
            return self
                .reconcile_finished(row, view, instances, now, None)
                .await;
        }
        self.drive(row, view, instances, now).await
    }

    /// A live, unfinished workload: start, observe, retry or fail it.
    async fn drive(
        &self,
        row: workloads::Model,
        view: WorkloadView,
        instances: Vec<workload_instances::Model>,
        now: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>, PlaneError> {
        let mut status = view.status.clone();
        let (timeout_secs, retry_limit, ttl_secs_after_finished) = match view.spec.lifetime {
            Lifetime::RunToCompletion {
                timeout_secs,
                retry_limit,
                ttl_secs_after_finished,
            } => (timeout_secs, retry_limit, ttl_secs_after_finished),
            Lifetime::LongRunning { .. } => {
                return self.drive_long_running(row, view, instances, now).await;
            }
            Lifetime::ScaleToZero { .. } => {
                return self.drive_scale_to_zero(row, view, instances, now).await;
            }
        };
        let ttl = Duration::seconds(i64::from(
            ttl_secs_after_finished.unwrap_or(DEFAULT_JOB_TTL_SECS),
        ));
        let deadline = row.created_at + Duration::seconds(i64::from(timeout_secs));
        if let Some(latest) = instances.last() {
            set_scheduled(&mut status, latest.node_id, now);
        }

        // A pass that runs late (after an outage, or behind a slow pull) may
        // find an attempt that already exited inside its deadline: that run
        // is observed below with its real result, not failed for the delay.
        let exited_in_time = match instances.last() {
            Some(latest) if now >= deadline => self.exited_by(row.id, latest, deadline).await,
            _ => false,
        };
        if now >= deadline && !exited_in_time {
            let mut tail = None;
            let attempt = instances.last().map(|i| i.attempt).unwrap_or(0);
            for instance in instances.iter().filter(|i| !is_finished(i)) {
                let handle = handle_of(instance)?;
                match self.driver_of(instance).await {
                    Ok(driver) => {
                        if tail.is_none() && !row.finalizers.is_empty() {
                            tail = self.output_tail(&driver, &view, &handle).await;
                        }
                        let stopped = driver
                            .stop(&handle)
                            .await
                            .map_err(driver_error(row.id, "stop the instance past its deadline"));
                        let removed = match stopped {
                            Ok(()) => driver.remove(&handle, true).await.map_err(driver_error(
                                row.id,
                                "remove the instance past its deadline",
                            )),
                            Err(error) => Err(error),
                        };
                        if let Err(error) = removed {
                            // A worker that reads its stream stops the
                            // instance when it next delivers the failed
                            // row; any other runtime keeps the run past
                            // its deadline until the removal succeeds.
                            self.removal_left_to_node(instance, Some(&driver), error)
                                .await?;
                        }
                    }
                    Err(reason) => {
                        let error = PlaneError::NodeUnavailable {
                            workload_id: row.id,
                            node_id: instance.node_id.unwrap_or(0),
                            reason: format!(
                                "the instance past its deadline could not be stopped: {reason}"
                            ),
                        };
                        if instance.node_id.is_some() {
                            self.removal_left_to_node(instance, None, error).await?;
                        } else {
                            // Node 0 without a runtime has nothing to stop.
                            tracing::warn!(
                                workload_id = row.id,
                                instance_id = instance.id,
                                "{error}"
                            );
                        }
                    }
                }
                self.finish_instance(instance.id, PHASE_FAILED, None, now)
                    .await?;
            }
            let message = format!(
                "the run exceeded its maximum run time of {timeout_secs}s (attempt {attempt}); \
                 its instance was stopped and removed"
            );
            status.set_condition(
                ConditionKind::Running,
                ConditionStatus::False,
                "DeadlineExceeded",
                &message,
                now,
            );
            fail(&mut status, "DeadlineExceeded", &message, now);
            events::record(
                self.db.as_ref(),
                row.id,
                None,
                "DeadlineExceeded",
                &message,
                now,
            )
            .await?;
            tracing::warn!(workload_id = row.id, name = %row.name, "{message}");
            return self
                .conclude(row, view, instances, status, now, now + ttl, tail)
                .await;
        }

        let Some(latest) = instances.last().cloned() else {
            return self
                .place_and_start(row, view, instances, status, 1, deadline, ttl, now)
                .await;
        };
        let attempt = u32::try_from(latest.attempt).unwrap_or(1);
        let (exit_code, finished_at) = match latest.phase.as_str() {
            PHASE_PENDING | PHASE_RUNNING => {
                let driver = self.driver_of(&latest).await.map_err(|reason| {
                    PlaneError::NodeUnavailable {
                        workload_id: row.id,
                        node_id: latest.node_id.unwrap_or(0),
                        reason,
                    }
                })?;
                let handle = handle_of(&latest)?;
                let observed = driver
                    .inspect(&handle)
                    .await
                    .map_err(driver_error(row.id, "inspect the instance"))?;
                match observed.state {
                    InstanceState::Missing if latest.phase == PHASE_PENDING => {
                        // The row was written but the container never was:
                        // the previous process stopped before creating it.
                        return self
                            .launch(row, view, status, latest, Some(deadline), ttl, &driver, now)
                            .await;
                    }
                    InstanceState::Created if driver.reconciled_by_node() => {
                        // Assigned to a node that has not reported it yet.
                        return self
                            .await_node(row, status, &latest, &handle, Some(deadline), now)
                            .await;
                    }
                    InstanceState::Created => {
                        // Created but never started.
                        if let Err(error) = driver.start(&handle).await {
                            return self
                                .launch_failed(
                                    row,
                                    view,
                                    instances,
                                    status,
                                    &latest,
                                    &driver,
                                    &handle,
                                    "StartFailed",
                                    error,
                                    now,
                                    ttl,
                                )
                                .await;
                        }
                        return self
                            .mark_started(
                                row,
                                status,
                                &latest,
                                handle,
                                Some(deadline),
                                &driver,
                                now,
                            )
                            .await;
                    }
                    InstanceState::Running => {
                        return self
                            .mark_started(
                                row,
                                status,
                                &latest,
                                handle,
                                Some(deadline),
                                &driver,
                                now,
                            )
                            .await;
                    }
                    InstanceState::Exited => {
                        let finished = observed.finished_at.unwrap_or(now);
                        let phase = if observed.exit_code == Some(0) {
                            PHASE_SUCCEEDED
                        } else {
                            PHASE_FAILED
                        };
                        self.finish_instance(latest.id, phase, observed.exit_code, finished)
                            .await?;
                        (observed.exit_code, finished)
                    }
                    InstanceState::Missing => {
                        self.finish_instance(latest.id, PHASE_FAILED, None, now)
                            .await?;
                        let message = format!(
                            "attempt {attempt}: container '{}' disappeared while running",
                            handle.name
                        );
                        events::record(
                            self.db.as_ref(),
                            row.id,
                            Some(latest.id),
                            "InstanceLost",
                            &message,
                            now,
                        )
                        .await?;
                        (None, now)
                    }
                }
            }
            PHASE_SUCCEEDED => (
                latest.exit_code.or(Some(0)),
                latest.finished_at.unwrap_or(now),
            ),
            _ => (latest.exit_code, latest.finished_at.unwrap_or(now)),
        };

        status.ready_instances = 0;
        status.exit_code = exit_code;
        if exit_code == Some(0) {
            let message = format!("attempt {attempt} exited 0");
            status.set_condition(
                ConditionKind::Running,
                ConditionStatus::False,
                "Completed",
                &message,
                now,
            );
            status.set_condition(
                ConditionKind::Completed,
                ConditionStatus::True,
                "Succeeded",
                &message,
                now,
            );
            status.phase = WorkloadPhase::Succeeded;
            events::record(
                self.db.as_ref(),
                row.id,
                Some(latest.id),
                "Completed",
                &message,
                now,
            )
            .await?;
            return self
                .conclude(row, view, instances, status, now, finished_at + ttl, None)
                .await;
        }
        let exited = match exit_code {
            Some(code) => format!("exited {code}"),
            None => "was lost".to_string(),
        };
        if attempt <= retry_limit && now >= deadline {
            // The attempt ended inside the deadline, but no time is left for
            // another one.
            let message = format!(
                "attempt {attempt} {exited}; its maximum run time of {timeout_secs}s left no \
                 time for attempt {}",
                attempt + 1
            );
            status.set_condition(
                ConditionKind::Running,
                ConditionStatus::False,
                "DeadlineExceeded",
                &message,
                now,
            );
            fail(&mut status, "DeadlineExceeded", &message, now);
            events::record(
                self.db.as_ref(),
                row.id,
                Some(latest.id),
                "DeadlineExceeded",
                &message,
                now,
            )
            .await?;
            return self
                .conclude(row, view, instances, status, now, finished_at + ttl, None)
                .await;
        }
        if attempt <= retry_limit {
            let retry_at = finished_at + backoff(attempt);
            if now < retry_at {
                let message = format!(
                    "attempt {attempt} {exited}; attempt {} of {} starts at {}",
                    attempt + 1,
                    retry_limit + 1,
                    retry_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                );
                status.set_condition(
                    ConditionKind::Running,
                    ConditionStatus::False,
                    "BackOff",
                    &message,
                    now,
                );
                status.phase = WorkloadPhase::Running;
                if self.write_status(&row, &status, None).await? {
                    events::record(
                        self.db.as_ref(),
                        row.id,
                        Some(latest.id),
                        "BackOff",
                        &message,
                        now,
                    )
                    .await?;
                }
                return Ok(Some(retry_at.min(deadline)));
            }
            // The failed attempt's container goes before the next starts:
            // a run holds one container at a time.
            let handle = handle_of(&latest)?;
            let removed = match self.driver_of(&latest).await {
                Ok(driver) => driver
                    .remove(&handle, true)
                    .await
                    .map_err(driver_error(row.id, "remove the failed attempt")),
                Err(reason) => Err(PlaneError::NodeUnavailable {
                    workload_id: row.id,
                    node_id: latest.node_id.unwrap_or(0),
                    reason,
                }),
            };
            if let Err(error) = removed {
                if latest.node_id.is_none() {
                    return Err(error);
                }
                // A worker removes a replaced attempt itself.
                tracing::warn!(workload_id = row.id, instance_id = latest.id, "{error}");
            }
            return self
                .place_and_start(
                    row,
                    view,
                    Vec::new(),
                    status,
                    attempt + 1,
                    deadline,
                    ttl,
                    now,
                )
                .await;
        }
        let message =
            format!("attempt {attempt} {exited}; the retry limit of {retry_limit} is reached");
        status.set_condition(
            ConditionKind::Running,
            ConditionStatus::False,
            "BackoffLimitExceeded",
            &message,
            now,
        );
        fail(&mut status, "BackoffLimitExceeded", &message, now);
        events::record(
            self.db.as_ref(),
            row.id,
            Some(latest.id),
            "BackoffLimitExceeded",
            &message,
            now,
        )
        .await?;
        self.conclude(row, view, instances, status, now, finished_at + ttl, None)
            .await
    }

    /// Whether `instance` already exited at or before `deadline`. A finished
    /// row answers from its recorded `finished_at`: the instance's result
    /// is persisted before the workload's conclusion, so a crash between
    /// the two writes, or a node's exit report ingested while no pass ran,
    /// leaves a finished row under a running workload, and its result must
    /// stand. An unfinished row asks its runtime. Anything it cannot prove
    /// (the node is unreachable, no exit time is known) is `false`, so the
    /// deadline applies.
    async fn exited_by(
        &self,
        workload_id: i64,
        instance: &workload_instances::Model,
        deadline: DateTime<Utc>,
    ) -> bool {
        if is_finished(instance) {
            // Strictly before: the deadline path itself finishes an
            // instance at the pass time, which can equal the deadline.
            return instance.finished_at.is_some_and(|at| at < deadline);
        }
        let (Ok(driver), Ok(handle)) = (self.driver_of(instance).await, handle_of(instance)) else {
            return false;
        };
        match driver.inspect(&handle).await {
            Ok(observed) => {
                matches!(observed.state, InstanceState::Exited)
                    && observed.finished_at.is_some_and(|at| at <= deadline)
            }
            Err(error) => {
                tracing::warn!(
                    workload_id,
                    instance_id = instance.id,
                    "inspecting the instance past its deadline failed: {error}"
                );
                false
            }
        }
    }

    /// A long-running workload (one deployment replica, ADR-055 Phase 5):
    /// create its one instance and report it. The runtime restarts a
    /// crashed process in place (its restart policy), as the legacy
    /// deployer's containers are; the plane never replaces a replica on its
    /// own, because the product owns the revision's lifecycle (blue/green,
    /// sleep, retirement). The owner deletes the workload to retire it.
    async fn drive_long_running(
        &self,
        row: workloads::Model,
        view: WorkloadView,
        instances: Vec<workload_instances::Model>,
        now: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>, PlaneError> {
        let mut status = view.status.clone();
        let ttl = Duration::seconds(i64::from(DEFAULT_JOB_TTL_SECS));
        // Strict pinning (§8). A replica pinned to one worker (the deploy
        // job placed it) runs there through the placement's driver for that
        // node; one pinned anywhere this plane cannot reach stays
        // unscheduled with the reason, never quietly on node 0.
        let pinned = view.spec.placement.pinned_nodes();
        let worker = match pinned {
            Some([node_id]) if *node_id != crate::scheduler::CONTROL_PLANE_NODE_ID => {
                Some(*node_id)
            }
            _ => None,
        };
        let unserved = match (&pinned, worker, &self.placement) {
            (Some(nodes), None, _) if !nodes.contains(&crate::scheduler::CONTROL_PLANE_NODE_ID) => {
                Some(format!(
                    "pinned to node(s) {nodes:?}{}; a replica is pinned to exactly one node, and \
                     this plane cannot choose among several",
                    pin_cause(&view.spec)
                ))
            }
            (_, Some(node_id), None) => Some(format!(
                "pinned to node {node_id}{}, and this plane places nothing on workers (it has \
                 no node placement)",
                pin_cause(&view.spec)
            )),
            _ => None,
        };
        if let Some(message) = unserved {
            // A volume's node is a pin the product cannot drop: say so.
            let reason = if view.spec.volumes.is_empty() {
                "PinnedNodeNotServed"
            } else {
                "PinnedByVolume"
            };
            status.set_condition(
                ConditionKind::Scheduled,
                ConditionStatus::False,
                reason,
                &message,
                now,
            );
            status.phase = WorkloadPhase::Pending;
            if self.write_status(&row, &status, None).await? {
                events::record(self.db.as_ref(), row.id, None, reason, &message, now).await?;
            }
            return Ok(None);
        }
        let latest = instances.last().cloned();
        let placed = match (&latest, worker, &self.placement) {
            // A live instance keeps the driver it was created by.
            (Some(latest), _, _)
                if latest.phase == PHASE_PENDING || latest.phase == PHASE_RUNNING =>
            {
                self.driver_of(latest).await.map(|d| (latest.node_id, d))
            }
            (_, Some(node_id), Some(placement)) => match placement.place_on(&view, node_id).await {
                Ok(Placed::Node { driver, .. }) => Ok((Some(node_id), driver)),
                Ok(Placed::ControlPlane) => self
                    .control_plane_driver_for(view.spec.isolation)
                    .map(|d| (None, d)),
                Err(message) => Err(message),
            },
            // On the control plane, the driver of the isolation the spec
            // asks for (a microVM through Firecracker, ADR-055 Phase 10).
            _ => self
                .control_plane_driver_for(view.spec.isolation)
                .map(|d| (None, d)),
        };
        let (node_id, driver) = match placed {
            Ok(placed) => placed,
            Err(message) => {
                status.set_condition(
                    ConditionKind::Scheduled,
                    ConditionStatus::False,
                    "NoEligibleNode",
                    &message,
                    now,
                );
                fail(&mut status, "NoEligibleNode", &message, now);
                events::record(
                    self.db.as_ref(),
                    row.id,
                    None,
                    "NoEligibleNode",
                    &message,
                    now,
                )
                .await?;
                return self
                    .conclude(row, view, instances, status, now, now + ttl, None)
                    .await;
            }
        };
        set_scheduled(&mut status, node_id, now);
        set_limit_condition(&mut status, &view.spec.resources, &driver, now);

        let Some(latest) = latest else {
            return self
                .start_attempt(row, view, status, 1, None, ttl, &driver, node_id, now)
                .await;
        };
        let attempt = latest.attempt;
        if latest.phase != PHASE_PENDING && latest.phase != PHASE_RUNNING {
            // Finished instance rows of a live long-running workload only
            // exist after a conclusion; nothing to drive.
            return Ok(None);
        }
        let handle = handle_of(&latest)?;
        let observed = driver
            .inspect(&handle)
            .await
            .map_err(driver_error(row.id, "inspect the instance"))?;
        let poll = Duration::from_std(self.config.resync_interval).unwrap_or(Duration::minutes(5));
        match observed.state {
            InstanceState::Missing if latest.phase == PHASE_PENDING => {
                self.launch(row, view, status, latest, None, ttl, &driver, now)
                    .await
            }
            InstanceState::Created if driver.reconciled_by_node() => {
                // Assigned to a node that has not reported it yet.
                self.await_node(row, status, &latest, &handle, None, now)
                    .await
            }
            InstanceState::Created => {
                if let Err(error) = driver.start(&handle).await {
                    return self
                        .launch_failed(
                            row,
                            view,
                            instances,
                            status,
                            &latest,
                            &driver,
                            &handle,
                            "StartFailed",
                            error,
                            now,
                            ttl,
                        )
                        .await;
                }
                self.mark_started(row, status, &latest, handle, None, &driver, now)
                    .await
            }
            InstanceState::Running => {
                self.mark_started(row, status, &latest, handle, None, &driver, now)
                    .await
            }
            InstanceState::Exited => {
                // Stopped and not restarted by its restart policy: the owner
                // stopped it (scale to zero, an environment put to sleep) or
                // it crashed past the policy. The owner decides; the plane
                // never replaces a deployment replica behind its back, and
                // picks it up again when the owner starts it.
                let message = format!(
                    "attempt {attempt} ('{}') is stopped (exit code {:?}); it runs again when \
                     its owner starts it",
                    handle.name, observed.exit_code
                );
                status.ready_instances = 0;
                status.exit_code = observed.exit_code;
                status.set_condition(
                    ConditionKind::Running,
                    ConditionStatus::False,
                    "Stopped",
                    &message,
                    now,
                );
                status.set_condition(
                    ConditionKind::Ready,
                    ConditionStatus::False,
                    "Stopped",
                    &message,
                    now,
                );
                status.phase = WorkloadPhase::Running;
                if self.write_status(&row, &status, None).await? {
                    events::record(
                        self.db.as_ref(),
                        row.id,
                        Some(latest.id),
                        "Stopped",
                        &message,
                        now,
                    )
                    .await?;
                }
                Ok(Some(now + poll))
            }
            InstanceState::Missing => {
                // Removed outside the plane (the legacy retire path, an
                // operator): the replica is gone. Never recreated: a
                // retired revision must not come back.
                self.finish_instance(latest.id, PHASE_FAILED, None, now)
                    .await?;
                let message = format!(
                    "attempt {attempt}: container '{}' was removed outside the plane; the \
                     replica is not recreated",
                    handle.name
                );
                status.set_condition(
                    ConditionKind::Running,
                    ConditionStatus::False,
                    "InstanceRemoved",
                    &message,
                    now,
                );
                fail(&mut status, "InstanceRemoved", &message, now);
                events::record(
                    self.db.as_ref(),
                    row.id,
                    Some(latest.id),
                    "InstanceRemoved",
                    &message,
                    now,
                )
                .await?;
                self.conclude(row, view, instances, status, now, now + ttl, None)
                    .await
            }
        }
    }

    /// The control plane's own driver, or why there is none.
    pub(crate) fn control_plane_driver(&self) -> Result<Arc<dyn RuntimeDriver>, String> {
        self.driver
            .0
            .clone()
            .ok_or_else(|| NO_CONTROL_PLANE_DRIVER.to_string())
    }

    /// The driver of an instance already placed.
    pub(crate) async fn driver_of(
        &self,
        instance: &workload_instances::Model,
    ) -> Result<Arc<dyn RuntimeDriver>, String> {
        match instance.node_id {
            None => self.local_driver_named(&instance.driver),
            Some(node_id) => match &self.placement {
                Some(placement) => placement.driver_for(node_id, &instance.driver).await,
                None => Err(format!(
                    "instance {} runs on node {node_id}, but this plane places nothing on \
                     workers",
                    instance.id
                )),
            },
        }
    }

    /// The node of a new attempt and its driver.
    async fn place(
        &self,
        view: &WorkloadView,
    ) -> Result<(Option<i32>, Arc<dyn RuntimeDriver>), String> {
        let pinned = view.spec.placement.pinned_nodes();
        let placed = match &self.placement {
            None => Placed::ControlPlane,
            Some(placement) => placement.place(view).await?,
        };
        // Strict pinning (§8): a placement outside the pin is refused, never
        // run (a backup job must run where its database's volume is).
        let placed_on = match &placed {
            Placed::ControlPlane => crate::scheduler::CONTROL_PLANE_NODE_ID,
            Placed::Node { node_id, .. } => *node_id,
        };
        if let Some(pinned) = pinned.filter(|nodes| !nodes.contains(&placed_on)) {
            return Err(format!(
                "workload {} is pinned to node(s) {pinned:?}{}, but {}",
                view.id,
                pin_cause(&view.spec),
                if self.placement.is_none() {
                    "this plane places attempts only on the control plane (node 0)".to_string()
                } else {
                    format!("its placement chose node {placed_on}")
                }
            ));
        }
        match placed {
            Placed::ControlPlane => Ok((None, self.control_plane_driver_for(view.spec.isolation)?)),
            Placed::Node {
                node_id,
                node_name,
                driver,
            } => {
                tracing::info!(
                    workload_id = view.id,
                    node_id,
                    node = %node_name,
                    driver = %driver.capabilities().name,
                    "placed the workload's next attempt on a worker"
                );
                Ok((Some(node_id), driver))
            }
        }
    }

    /// Place attempt `attempt`, then start it; or fail the run with
    /// `NoEligibleNode` and why.
    #[allow(clippy::too_many_arguments)]
    async fn place_and_start(
        &self,
        row: workloads::Model,
        view: WorkloadView,
        instances: Vec<workload_instances::Model>,
        mut status: WorkloadStatus,
        attempt: u32,
        deadline: DateTime<Utc>,
        ttl: Duration,
        now: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>, PlaneError> {
        match self.place(&view).await {
            Ok((node_id, driver)) => {
                set_scheduled(&mut status, node_id, now);
                set_limit_condition(&mut status, &view.spec.resources, &driver, now);
                self.start_attempt(
                    row,
                    view,
                    status,
                    attempt,
                    Some(deadline),
                    ttl,
                    &driver,
                    node_id,
                    now,
                )
                .await
            }
            Err(message) => {
                status.set_condition(
                    ConditionKind::Scheduled,
                    ConditionStatus::False,
                    "NoEligibleNode",
                    &message,
                    now,
                );
                fail(&mut status, "NoEligibleNode", &message, now);
                events::record(
                    self.db.as_ref(),
                    row.id,
                    None,
                    "NoEligibleNode",
                    &message,
                    now,
                )
                .await?;
                self.conclude(row, view, instances, status, now, now + ttl, None)
                    .await
            }
        }
    }

    /// Insert attempt `attempt`'s row, then create and start it. The row
    /// comes first, so a crash between the two leaves a row the next pass
    /// finishes, never a container nothing tracks.
    #[allow(clippy::too_many_arguments)]
    async fn start_attempt(
        &self,
        row: workloads::Model,
        view: WorkloadView,
        status: WorkloadStatus,
        attempt: u32,
        deadline: Option<DateTime<Utc>>,
        ttl: Duration,
        driver: &Arc<dyn RuntimeDriver>,
        node_id: Option<i32>,
        now: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>, PlaneError> {
        let name = instance_name(
            &self.config.scope,
            view.spec.owner.kind.as_str(),
            row.id,
            attempt,
        );
        let handle = InstanceHandle {
            driver: driver.capabilities().name.clone(),
            id: name.clone(),
            name,
        };
        let handle_json = serde_json::to_value(&handle).map_err(|e| PlaneError::Encode {
            workload: row.name.clone(),
            field: "instance handle",
            reason: e.to_string(),
        })?;
        let instance = workload_instances::ActiveModel {
            workload_id: Set(row.id),
            revision: Set(row.generation),
            attempt: Set(i32::try_from(attempt).unwrap_or(i32::MAX)),
            node_id: Set(node_id),
            driver: Set(handle.driver.clone()),
            handle: Set(handle_json),
            phase: Set(PHASE_PENDING.to_string()),
            ready: Set(false),
            conditions: Set(serde_json::json!([])),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await
        .map_err(PlaneError::db("inserting the instance row", row.id))?;
        self.launch(row, view, status, instance, deadline, ttl, driver, now)
            .await
    }

    /// Prepare, create and start the container of `instance`.
    #[allow(clippy::too_many_arguments)]
    async fn launch(
        &self,
        row: workloads::Model,
        view: WorkloadView,
        mut status: WorkloadStatus,
        instance: workload_instances::Model,
        deadline: Option<DateTime<Utc>>,
        ttl: Duration,
        driver: &Arc<dyn RuntimeDriver>,
        now: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>, PlaneError> {
        let attempt = instance.attempt;
        let handle = handle_of(&instance)?;
        let spec = &view.spec;

        let prepared = match driver.prepare(&spec.artifact).await {
            Ok(prepared) => prepared,
            Err(error) => {
                let message = format!("attempt {attempt} could not prepare its image: {error}");
                status.set_condition(
                    ConditionKind::ArtifactReady,
                    ConditionStatus::False,
                    "ArtifactPullFailed",
                    &message,
                    now,
                );
                status.set_condition(
                    ConditionKind::Running,
                    ConditionStatus::False,
                    "ArtifactNotReady",
                    &message,
                    now,
                );
                fail(&mut status, "ArtifactPullFailed", &message, now);
                self.finish_instance(instance.id, PHASE_FAILED, None, now)
                    .await?;
                events::record(
                    self.db.as_ref(),
                    row.id,
                    Some(instance.id),
                    "ArtifactPullFailed",
                    &message,
                    now,
                )
                .await?;
                return self
                    .conclude(row, view.clone(), Vec::new(), status, now, now + ttl, None)
                    .await;
            }
        };
        status.set_condition(
            ConditionKind::ArtifactReady,
            ConditionStatus::True,
            "Prepared",
            format!(
                "image '{}' is present on {}",
                prepared.local_ref,
                match instance.node_id {
                    None => "the control plane".to_string(),
                    Some(node_id) => format!("node {node_id}"),
                }
            ),
            now,
        );

        let resolved = match env::resolve_with_staged(&spec.env, self.env.as_ref(), &|import| {
            self.staged(import)
        })
        .await
        {
            Ok(resolved) => resolved,
            Err(reason) => {
                let message =
                    format!("attempt {attempt} could not resolve its environment: {reason}");
                status.set_condition(
                    ConditionKind::Running,
                    ConditionStatus::False,
                    "EnvironmentUnavailable",
                    &message,
                    now,
                );
                fail(&mut status, "EnvironmentUnavailable", &message, now);
                self.finish_instance(instance.id, PHASE_FAILED, None, now)
                    .await?;
                events::record(
                    self.db.as_ref(),
                    row.id,
                    Some(instance.id),
                    "EnvironmentUnavailable",
                    &message,
                    now,
                )
                .await?;
                return self
                    .conclude(row, view.clone(), Vec::new(), status, now, now + ttl, None)
                    .await;
            }
        };
        self.redactors
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(row.id, resolved.redactor.clone());

        let template = self.instance_template(
            spec,
            row.id,
            instance.revision,
            u32::try_from(attempt).unwrap_or(0),
            &handle.name,
        );
        let security = template.security;
        let attach = self.local_attachments(&template.attachments);
        let mut instance_spec = template.into_spec(prepared.local_ref.clone(), resolved.env);
        instance_spec.secret_files = resolved.secret_files;
        let created = match driver.create(&instance_spec, &attach).await {
            Ok(created) => created,
            Err(error) => {
                return self
                    .launch_failed(
                        row,
                        view,
                        Vec::new(),
                        status,
                        &instance,
                        driver,
                        &handle,
                        "CreateFailed",
                        error,
                        now,
                        ttl,
                    )
                    .await;
            }
        };
        let created_json = serde_json::to_value(&created).map_err(|e| PlaneError::Encode {
            workload: row.name.clone(),
            field: "instance handle",
            reason: e.to_string(),
        })?;
        workload_instances::Entity::update_many()
            .col_expr(
                workload_instances::Column::Handle,
                Expr::value(created_json),
            )
            .filter(workload_instances::Column::Id.eq(instance.id))
            .exec(self.db.as_ref())
            .await
            .map_err(PlaneError::db("recording the instance handle", row.id))?;
        if let Err(error) = driver.start(&created).await {
            return self
                .launch_failed(
                    row,
                    view,
                    Vec::new(),
                    status,
                    &instance,
                    driver,
                    &created,
                    "StartFailed",
                    error,
                    now,
                    ttl,
                )
                .await;
        }
        if driver.reconciled_by_node() {
            // The create was desired state only: the node has not even read
            // the row yet. Its report, not this call, makes it running.
            events::record(
                self.db.as_ref(),
                row.id,
                Some(instance.id),
                "Assigned",
                &format!(
                    "attempt {attempt} was assigned to {} as '{}' ({})",
                    node_label(instance.node_id),
                    created.name,
                    security.as_str()
                ),
                now,
            )
            .await?;
            return self
                .await_node(row, status, &instance, &created, deadline, now)
                .await;
        }
        events::record(
            self.db.as_ref(),
            row.id,
            Some(instance.id),
            "Started",
            &format!(
                "attempt {attempt} started as '{}' ({})",
                created.name,
                security.as_str()
            ),
            now,
        )
        .await?;
        self.mark_started(row, status, &instance, created, deadline, driver, now)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn launch_failed(
        &self,
        row: workloads::Model,
        view: WorkloadView,
        instances: Vec<workload_instances::Model>,
        mut status: WorkloadStatus,
        instance: &workload_instances::Model,
        driver: &Arc<dyn RuntimeDriver>,
        handle: &InstanceHandle,
        reason: &str,
        error: DriverError,
        now: DateTime<Utc>,
        ttl: Duration,
    ) -> Result<Option<DateTime<Utc>>, PlaneError> {
        if let Err(cleanup) = driver.remove(handle, true).await {
            tracing::warn!(
                workload_id = row.id,
                instance = %handle.name,
                "could not remove an instance that failed to launch: {cleanup}"
            );
        }
        let message = format!("attempt {}: {error}", instance.attempt);
        status.set_condition(
            ConditionKind::Running,
            ConditionStatus::False,
            reason,
            &message,
            now,
        );
        fail(&mut status, reason, &message, now);
        self.finish_instance(instance.id, PHASE_FAILED, None, now)
            .await?;
        events::record(
            self.db.as_ref(),
            row.id,
            Some(instance.id),
            reason,
            &message,
            now,
        )
        .await?;
        self.conclude(row, view, instances, status, now, now + ttl, None)
            .await
    }

    /// `instance` is assigned to a node whose runtime does the driver's
    /// work (ADR-055 §7) and has not reported it running yet. The row
    /// stays `pending`, so the node creates it when its stream delivers
    /// it, and the workload says what it waits for. The node's report
    /// moves the row to running (with its published ports) and wakes the
    /// reconciler, whose next pass marks the workload running.
    async fn await_node(
        &self,
        row: workloads::Model,
        mut status: WorkloadStatus,
        instance: &workload_instances::Model,
        handle: &InstanceHandle,
        deadline: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>, PlaneError> {
        let message = format!(
            "attempt {} ('{}') is assigned to {}; waiting for that node to report it running",
            instance.attempt,
            handle.name,
            node_label(instance.node_id)
        );
        status.set_condition(
            ConditionKind::Running,
            ConditionStatus::False,
            "WaitingForNode",
            &message,
            now,
        );
        status.ready_instances = 0;
        status.exit_code = None;
        status.phase = WorkloadPhase::Pending;
        self.write_status(&row, &status, None).await?;
        // The report's ingest wakes the reconciler; the poll bounds a
        // report it missed.
        let poll = Duration::from_std(self.config.poll_interval).unwrap_or(Duration::seconds(5));
        Ok(Some(match deadline {
            Some(deadline) => deadline.min(now + poll),
            None => now + poll,
        }))
    }

    /// The instance runs: record it and wait for its exit.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn mark_started(
        &self,
        row: workloads::Model,
        mut status: WorkloadStatus,
        instance: &workload_instances::Model,
        handle: InstanceHandle,
        deadline: Option<DateTime<Utc>>,
        driver: &Arc<dyn RuntimeDriver>,
        now: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>, PlaneError> {
        if instance.phase != PHASE_RUNNING {
            workload_instances::Entity::update_many()
                .col_expr(
                    workload_instances::Column::Phase,
                    Expr::value(PHASE_RUNNING),
                )
                .col_expr(workload_instances::Column::StartedAt, Expr::value(now))
                .filter(workload_instances::Column::Id.eq(instance.id))
                .exec(self.db.as_ref())
                .await
                .map_err(PlaneError::db("marking the instance running", row.id))?;
        }
        status.set_condition(
            ConditionKind::Running,
            ConditionStatus::True,
            "Started",
            format!(
                "attempt {} is running as '{}'",
                instance.attempt, handle.name
            ),
            now,
        );
        status.ready_instances = 1;
        status.exit_code = None;
        status.phase = WorkloadPhase::Running;
        if matches!(
            view_lifetime(&row),
            Some(Lifetime::LongRunning { .. } | Lifetime::ScaleToZero { .. })
        ) {
            // No readiness probe is run by the plane yet: the product gates
            // traffic on its own health check (blue/green stays in the
            // product), so a running instance is reported ready with why.
            status.set_condition(
                ConditionKind::Ready,
                ConditionStatus::True,
                "NoProbe",
                "running; the plane runs no readiness probe, the owner checks health",
                now,
            );
        }
        self.write_status(&row, &status, None).await?;
        self.watch(driver, instance.id, handle);
        let waits = self.driver_waits.load(Ordering::Relaxed);
        let poll = Duration::from_std(self.config.poll_interval).unwrap_or(Duration::seconds(5));
        Ok(match (deadline, waits) {
            (Some(deadline), true) => Some(deadline),
            (Some(deadline), false) => Some(deadline.min(now + poll)),
            (None, true) => None,
            (None, false) => Some(now + poll),
        })
    }

    /// Write the finished status (with its expiry, in the same statement,
    /// so a finished run always has one), then do the finished work.
    #[allow(clippy::too_many_arguments)]
    async fn conclude(
        &self,
        mut row: workloads::Model,
        mut view: WorkloadView,
        instances: Vec<workload_instances::Model>,
        status: WorkloadStatus,
        now: DateTime<Utc>,
        expires_at: DateTime<Utc>,
        tail: Option<String>,
    ) -> Result<Option<DateTime<Utc>>, PlaneError> {
        let expires_at = row.expires_at.unwrap_or(expires_at);
        self.write_status(&row, &status, Some(expires_at)).await?;
        tracing::info!(
            workload_id = row.id,
            name = %row.name,
            phase = status.phase.as_str(),
            "workload finished"
        );
        row.status = encode_status(&row.name, &status)?;
        row.phase = status.phase.as_str().to_string();
        row.expires_at = Some(expires_at);
        view.status = status;
        view.expires_at = Some(expires_at);
        self.reconcile_finished(row, view, instances, now, tail)
            .await
    }

    /// A finished workload: finalizers, then its time-to-live.
    async fn reconcile_finished(
        &self,
        row: workloads::Model,
        view: WorkloadView,
        instances: Vec<workload_instances::Model>,
        now: DateTime<Utc>,
        tail: Option<String>,
    ) -> Result<Option<DateTime<Utc>>, PlaneError> {
        let poll = Duration::from_std(self.config.poll_interval).unwrap_or(Duration::seconds(5));
        let mut requeue = None;
        let mut row = row;
        if !row.finalizers.is_empty() {
            let remaining = self.run_finalizers(&row, view, &instances, tail).await?;
            if !remaining.is_empty() {
                requeue = Some(now + poll);
            }
            row.finalizers = remaining;
        }
        let expires_at = match row.expires_at {
            Some(at) => at,
            None => {
                // Finished before expiries were recorded with the status.
                let ttl = match view_spec_ttl(&row) {
                    Some(ttl) => ttl,
                    None => Duration::seconds(i64::from(DEFAULT_JOB_TTL_SECS)),
                };
                let at = instances.last().and_then(|i| i.finished_at).unwrap_or(now) + ttl;
                workloads::Entity::update_many()
                    .col_expr(workloads::Column::ExpiresAt, Expr::value(at))
                    .filter(workloads::Column::Id.eq(row.id))
                    .exec(self.db.as_ref())
                    .await
                    .map_err(PlaneError::db("recording the expiry", row.id))?;
                at
            }
        };
        if expires_at > now {
            return Ok(earliest(requeue, Some(expires_at)));
        }
        workloads::Entity::update_many()
            .col_expr(workloads::Column::DeletedAt, Expr::value(now))
            .filter(workloads::Column::Id.eq(row.id))
            .filter(workloads::Column::DeletedAt.is_null())
            .exec(self.db.as_ref())
            .await
            .map_err(PlaneError::db("expiring the finished workload", row.id))?;
        tracing::info!(workload_id = row.id, name = %row.name, "finished workload expired");
        row.deleted_at = Some(now);
        let view = view_from(row.clone(), instances.last())?;
        self.reconcile_deleted(row, view, instances, now).await
    }

    /// A deleted workload: remove its instances, fail it if it had not
    /// finished, run its finalizers, then remove the row.
    async fn reconcile_deleted(
        &self,
        row: workloads::Model,
        mut view: WorkloadView,
        instances: Vec<workload_instances::Model>,
        now: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>, PlaneError> {
        let poll = Duration::from_std(self.config.poll_interval).unwrap_or(Duration::seconds(5));
        let mut tail = None;
        for instance in &instances {
            let driver = match self.driver_of(instance).await {
                Ok(driver) => driver,
                Err(reason) => {
                    // Node 0 without a runtime has nothing to remove. A
                    // worker's instance goes with its row only when the
                    // worker reads its stream; otherwise the cleanup waits
                    // for the worker's driver.
                    if let Some(node_id) = instance.node_id {
                        self.removal_left_to_node(
                            instance,
                            None,
                            PlaneError::NodeUnavailable {
                                workload_id: row.id,
                                node_id,
                                reason: format!(
                                    "the deleted workload's instance could not be removed: \
                                     {reason}"
                                ),
                            },
                        )
                        .await?;
                        if !is_finished(instance) {
                            self.finish_instance(instance.id, PHASE_FAILED, None, now)
                                .await?;
                        }
                    }
                    continue;
                }
            };
            let handle = handle_of(instance)?;
            if tail.is_none() && !row.finalizers.is_empty() && instances.last() == Some(instance) {
                tail = self.output_tail(&driver, &view, &handle).await;
            }
            if let Err(error) = driver.remove(&handle, true).await {
                let error = PlaneError::Driver {
                    workload_id: row.id,
                    operation: "remove the deleted workload's instance",
                    source: error,
                };
                // Deleting the workload cascades to its instance rows: only
                // a worker that reads its stream learns of the removal from
                // that, so any other runtime's cleanup is retried first.
                self.removal_left_to_node(instance, Some(&driver), error)
                    .await?;
            }
            if !is_finished(instance) {
                self.finish_instance(instance.id, PHASE_FAILED, None, now)
                    .await?;
            }
        }
        let mut status = decode_status(&row)?;
        if !status.phase.is_terminal() {
            status.phase = WorkloadPhase::Failed;
            status.ready_instances = 0;
            if status.condition(ConditionKind::Completed).is_none() {
                status.set_condition(
                    ConditionKind::Completed,
                    ConditionStatus::False,
                    "Deleted",
                    "the workload was deleted before it finished",
                    now,
                );
            }
            let json = encode_status(&row.name, &status)?;
            workloads::Entity::update_many()
                .col_expr(workloads::Column::Status, Expr::value(json))
                .col_expr(workloads::Column::Phase, Expr::value(status.phase.as_str()))
                .filter(workloads::Column::Id.eq(row.id))
                .exec(self.db.as_ref())
                .await
                .map_err(PlaneError::db("failing the deleted workload", row.id))?;
            self.status_tx.send_modify(|seq| *seq += 1);
            view.status = status;
        }
        let remaining = if row.finalizers.is_empty() {
            Vec::new()
        } else {
            self.run_finalizers(&row, view, &instances, tail).await?
        };
        if !remaining.is_empty() {
            return Ok(Some(now + poll));
        }
        workloads::Entity::delete_by_id(row.id)
            .exec(self.db.as_ref())
            .await
            .map_err(PlaneError::db("removing the deleted workload", row.id))?;
        self.redactors
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&row.id);
        self.status_tx.send_modify(|seq| *seq += 1);
        tracing::info!(workload_id = row.id, name = %row.name, "workload removed");
        Ok(None)
    }

    /// Run the workload's finalizers; returns those still pending.
    async fn run_finalizers(
        &self,
        row: &workloads::Model,
        mut view: WorkloadView,
        instances: &[workload_instances::Model],
        tail: Option<String>,
    ) -> Result<Vec<String>, PlaneError> {
        view.finalizers = row.finalizers.clone();
        view.output_tail = match tail {
            Some(tail) => Some(tail),
            None => match instances.last() {
                Some(latest) if row.deleted_at.is_none() => match self.driver_of(latest).await {
                    Ok(driver) => {
                        let handle = handle_of(latest)?;
                        self.output_tail(&driver, &view, &handle).await
                    }
                    Err(_) => None,
                },
                _ => None,
            },
        };
        if let Some(latest) = instances.last() {
            view.attempt = u32::try_from(latest.attempt).unwrap_or(0);
            view.finished_at = latest.finished_at.or(view.finished_at);
        }
        let mut remaining = Vec::new();
        for name in &row.finalizers {
            match self.finalizers.get(name) {
                Some(finalizer) => match finalizer.finalize(&view).await {
                    Ok(()) => {
                        tracing::debug!(workload_id = row.id, finalizer = %name, "finalizer done");
                    }
                    Err(reason) => {
                        tracing::warn!(
                            workload_id = row.id,
                            finalizer = %name,
                            "finalizer failed, retrying on a later pass: {reason}"
                        );
                        remaining.push(name.clone());
                    }
                },
                None => {
                    tracing::warn!(
                        workload_id = row.id,
                        finalizer = %name,
                        "finalizer is not registered on this plane; the workload waits for it"
                    );
                    remaining.push(name.clone());
                }
            }
        }
        if remaining != row.finalizers {
            workloads::Entity::update_many()
                .col_expr(
                    workloads::Column::Finalizers,
                    Expr::value(remaining.clone()),
                )
                .filter(workloads::Column::Id.eq(row.id))
                .exec(self.db.as_ref())
                .await
                .map_err(PlaneError::db("removing finished finalizers", row.id))?;
        }
        Ok(remaining)
    }

    /// Write `status` if it differs from what is stored. Returns whether
    /// it wrote. Only a live row is written: a concurrent delete wins.
    pub(crate) async fn write_status(
        &self,
        row: &workloads::Model,
        status: &WorkloadStatus,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<bool, PlaneError> {
        let json = encode_status(&row.name, status)?;
        let phase = status.phase.as_str();
        let expiry_changed = expires_at.is_some() && expires_at != row.expires_at;
        if json == row.status
            && row.phase == phase
            && row.observed_generation == row.generation
            && !expiry_changed
        {
            return Ok(false);
        }
        let mut update = workloads::Entity::update_many()
            .col_expr(workloads::Column::Status, Expr::value(json))
            .col_expr(workloads::Column::Phase, Expr::value(phase))
            .col_expr(
                workloads::Column::ObservedGeneration,
                Expr::value(row.generation),
            );
        if let Some(at) = expires_at {
            update = update.col_expr(workloads::Column::ExpiresAt, Expr::value(at));
        }
        update
            .filter(workloads::Column::Id.eq(row.id))
            .filter(workloads::Column::DeletedAt.is_null())
            .exec(self.db.as_ref())
            .await
            .map_err(PlaneError::db("writing status", row.id))?;
        self.status_tx.send_modify(|seq| *seq += 1);
        Ok(true)
    }

    pub(crate) async fn finish_instance(
        &self,
        instance_id: i64,
        phase: &str,
        exit_code: Option<i64>,
        finished_at: DateTime<Utc>,
    ) -> Result<(), PlaneError> {
        workload_instances::Entity::update_many()
            .col_expr(workload_instances::Column::Phase, Expr::value(phase))
            .col_expr(workload_instances::Column::Ready, Expr::value(false))
            .col_expr(workload_instances::Column::ExitCode, Expr::value(exit_code))
            .col_expr(
                workload_instances::Column::FinishedAt,
                Expr::value(finished_at),
            )
            .filter(workload_instances::Column::Id.eq(instance_id))
            .exec(self.db.as_ref())
            .await
            .map_err(PlaneError::db(
                "recording the instance exit",
                format!("instance {instance_id}"),
            ))?;
        Ok(())
    }

    /// `instance` could not be stopped or removed (`error`): whether the
    /// caller may go ahead and fail or delete its row anyway (`Ok`), or
    /// must keep it and retry on a later pass (`Err(error)`).
    ///
    /// Only a worker whose node runtime reads the instance rows (the
    /// stream, [`RuntimeDriver::reconciled_by_node`]) acts on the row
    /// itself: it stops a failed instance and removes one whose row is
    /// gone. Any other runtime (node 0, a worker driven through its
    /// `/agent/*` API) never sees the row again, so going ahead would leave
    /// its instance running forever. `driver` is `None` when the worker's
    /// driver cannot be built now; the row then says which driver created
    /// the instance. The one bounded give-up: a worker that no longer
    /// exists cannot remove anything, so its instance is forgotten, and
    /// that is logged.
    async fn removal_left_to_node(
        &self,
        instance: &workload_instances::Model,
        driver: Option<&Arc<dyn RuntimeDriver>>,
        error: PlaneError,
    ) -> Result<(), PlaneError> {
        let Some(node_id) = instance.node_id else {
            return Err(error);
        };
        let streamed = match driver {
            Some(driver) => driver.reconciled_by_node(),
            None => instance.driver == crate::node::STREAM_DRIVER,
        };
        if streamed {
            tracing::warn!(
                workload_id = instance.workload_id,
                instance_id = instance.id,
                node_id,
                "{error}; node {node_id} reads its state stream and acts on the instance row \
                 itself"
            );
            return Ok(());
        }
        // Without a placement nothing here can tell; keep retrying.
        let exists = match &self.placement {
            Some(placement) => placement.node_exists(node_id).await.map_err(|reason| {
                PlaneError::NodeUnavailable {
                    workload_id: instance.workload_id,
                    node_id,
                    reason: format!(
                        "{error}; and whether node {node_id} still exists could not be read: \
                         {reason}"
                    ),
                }
            })?,
            None => true,
        };
        if !exists {
            tracing::warn!(
                workload_id = instance.workload_id,
                instance_id = instance.id,
                node_id,
                "{error}; node {node_id} no longer exists, so nothing can remove instance {} \
                 there and it is forgotten",
                instance.id
            );
            return Ok(());
        }
        tracing::warn!(
            workload_id = instance.workload_id,
            instance_id = instance.id,
            node_id,
            "{error}; node {node_id} does not read its state stream, so the instance row is \
             kept and its removal retried on a later pass"
        );
        Err(error)
    }

    /// Wait for the instance's exit in the background and wake the
    /// reconciler when it happens. One watcher per instance.
    fn watch(&self, driver: &Arc<dyn RuntimeDriver>, instance_id: i64, handle: InstanceHandle) {
        if !self.driver_waits.load(Ordering::Relaxed) {
            return;
        }
        let mut watchers = self
            .watchers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        watchers.retain(|_, task| !task.is_finished());
        if watchers.contains_key(&instance_id) {
            return;
        }
        let driver = Arc::clone(driver);
        let notify = Arc::clone(&self.notify);
        let waits = Arc::clone(&self.driver_waits);
        let poll = self.config.poll_interval;
        let task = tokio::spawn(async move {
            match driver.wait(&handle).await {
                Ok(()) => {}
                Err(DriverError::Unsupported { .. }) => {
                    waits.store(false, Ordering::Relaxed);
                }
                Err(error) => {
                    tracing::warn!(instance = %handle.name, "waiting for the instance's exit failed: {error}");
                    // Do not turn a failing wait into a hot loop.
                    tokio::time::sleep(poll).await;
                }
            }
            notify.notify_one();
        });
        watchers.insert(instance_id, task);
    }

    /// The last lines of an instance's output, redacted. `None` when the
    /// output, or what to redact from it, is unavailable: an unredacted
    /// tail is never returned.
    async fn output_tail(
        &self,
        driver: &Arc<dyn RuntimeDriver>,
        view: &WorkloadView,
        handle: &InstanceHandle,
    ) -> Option<String> {
        let known = self
            .redactors
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&view.id)
            .cloned();
        let redactor = match known {
            Some(redactor) => redactor,
            None => match env::resolve_with_staged(&view.spec.env, self.env.as_ref(), &|import| {
                self.staged(import)
            })
            .await
            {
                Ok(resolved) => resolved.redactor,
                Err(reason) => {
                    tracing::debug!(workload_id = view.id, "output tail skipped: {reason}");
                    return None;
                }
            },
        };
        let lines = self.config.output_tail_lines;
        let stream = driver
            .logs(
                handle,
                LogQuery {
                    since: None,
                    tail: Some(lines),
                    follow: false,
                    resume: false,
                },
            )
            .await
            .ok()?;
        let chunks = tokio::time::timeout(
            LOGS_TIMEOUT,
            stream.take(lines as usize * 4).collect::<Vec<_>>(),
        )
        .await
        .ok()?;
        let mut text = String::new();
        for chunk in chunks.into_iter().flatten() {
            text.push_str(&String::from_utf8_lossy(&chunk.line));
        }
        Some(tail_of(&redactor.redact(&text), OUTPUT_TAIL_BYTES))
    }

    /// Remove containers carrying this plane's labels that no instance row
    /// tracks: what a crash between a delete and its cleanup leaves.
    async fn sweep_orphans(&self, driver: &Arc<dyn RuntimeDriver>) -> usize {
        let selector = BTreeMap::from([
            (labels::PLANE.to_string(), self.config.scope.clone()),
            (
                labels::MANAGED_BY.to_string(),
                labels::MANAGED_BY_VALUE.to_string(),
            ),
        ]);
        let listed = match driver.list(&selector).await {
            Ok(listed) => listed,
            Err(DriverError::Unsupported { .. }) => {
                tracing::debug!("driver cannot list instances; orphan cleanup skipped");
                return 0;
            }
            Err(error) => {
                tracing::warn!(scope = %self.config.scope, "orphan cleanup skipped: {error}");
                return 0;
            }
        };
        if listed.is_empty() {
            return 0;
        }
        let key = |labels_of: &BTreeMap<String, String>| -> Option<(i64, i32)> {
            let workload = labels_of.get(workload_labels::WORKLOAD_ID)?.parse().ok()?;
            let attempt = labels_of.get(labels::ATTEMPT)?.parse().ok()?;
            Some((workload, attempt))
        };
        let ids: Vec<i64> = listed
            .iter()
            .filter_map(|l| key(&l.labels).map(|(w, _)| w))
            .collect();
        let tracked: HashSet<(i64, i32)> = if ids.is_empty() {
            HashSet::new()
        } else {
            match workload_instances::Entity::find()
                .filter(workload_instances::Column::WorkloadId.is_in(ids))
                .all(self.db.as_ref())
                .await
            {
                Ok(rows) => rows
                    .into_iter()
                    .map(|i| (i.workload_id, i.attempt))
                    .collect(),
                Err(error) => {
                    tracing::error!("orphan cleanup could not read instances: {error}");
                    return 0;
                }
            }
        };
        let mut removed = 0;
        for instance in listed {
            let tracked_by = key(&instance.labels);
            if tracked_by.is_some_and(|k| tracked.contains(&k)) {
                continue;
            }
            match driver.remove(&instance.handle, true).await {
                Ok(()) => {
                    removed += 1;
                    tracing::info!(
                        instance = %instance.handle.name,
                        workload = ?tracked_by.map(|(w, _)| w),
                        "removed an orphaned instance: no instance row tracks it"
                    );
                }
                Err(error) => tracing::warn!(
                    instance = %instance.handle.name,
                    "could not remove an orphaned instance: {error}"
                ),
            }
        }
        removed
    }
}

/// Where an instance runs, for messages.
fn node_label(node_id: Option<i32>) -> String {
    match node_id {
        None => "the control plane".to_string(),
        Some(node_id) => format!("node {node_id}"),
    }
}

pub(crate) fn set_scheduled(status: &mut WorkloadStatus, node_id: Option<i32>, now: DateTime<Utc>) {
    let (reason, message) = match node_id {
        None => (
            "ControlPlane",
            "runs on the control plane (node 0)".to_string(),
        ),
        Some(node_id) => ("Node", format!("runs on node {node_id}")),
    };
    status.set_condition(
        ConditionKind::Scheduled,
        ConditionStatus::True,
        reason,
        message,
        now,
    );
}

/// The lifetime of the stored spec, when it decodes.
fn view_lifetime(row: &workloads::Model) -> Option<Lifetime> {
    serde_json::from_value::<temps_workload::WorkloadSpec>(row.spec.clone())
        .ok()
        .map(|spec| spec.lifetime)
}

/// `LimitEnforced` (ADR-055 §5): a limit is always applied, and one the
/// driver cannot apply is reported with the reason, never ignored silently.
/// Not set when the spec sets no limit.
pub(crate) fn set_limit_condition(
    status: &mut WorkloadStatus,
    resources: &Resources,
    driver: &Arc<dyn RuntimeDriver>,
    now: DateTime<Utc>,
) {
    let Some(limits) = resources.limits else {
        return;
    };
    let capabilities = driver.capabilities();
    let support: LimitSupport = capabilities.limits;
    let mut unenforced = Vec::new();
    if limits.cpu_millis.is_some() && !support.cpu {
        unenforced.push(format!(
            "{}: CPU limits are not supported",
            capabilities.name
        ));
    }
    if limits.memory_mb.is_some() && !support.memory {
        unenforced.push(format!(
            "{}: memory limits are not supported",
            capabilities.name
        ));
    }
    if limits.pids.is_some() && !support.pids {
        unenforced.push(format!(
            "{}: process limits are not supported",
            capabilities.name
        ));
    }
    if limits.disk_io_mbps.is_some() && !support.disk_io {
        unenforced.push(format!(
            "{}: block I/O bandwidth limits are not supported",
            capabilities.name
        ));
    }
    if limits.network_mbps.is_some() && !support.network {
        unenforced.push(format!(
            "{}: network bandwidth limits are not supported",
            capabilities.name
        ));
    }
    if let Some(disk) = limits.disk_mb.filter(|_| !support.disk) {
        unenforced.push(format!(
            "{}: the {disk} MB disk limit needs an XFS/overlay2 project quota on this host, \
             which this driver does not apply",
            capabilities.name
        ));
    }
    if unenforced.is_empty() {
        status.set_condition(
            ConditionKind::LimitEnforced,
            ConditionStatus::True,
            "Applied",
            "every configured limit is applied by the runtime",
            now,
        );
    } else {
        status.set_condition(
            ConditionKind::LimitEnforced,
            ConditionStatus::False,
            "LimitUnsupported",
            unenforced.join("; "),
            now,
        );
    }
}

fn view_spec_ttl(row: &workloads::Model) -> Option<Duration> {
    let spec: temps_workload::WorkloadSpec = serde_json::from_value(row.spec.clone()).ok()?;
    match spec.lifetime {
        Lifetime::RunToCompletion {
            ttl_secs_after_finished,
            ..
        } => ttl_secs_after_finished.map(|s| Duration::seconds(i64::from(s))),
        _ => None,
    }
}

/// Why a workload is pinned, for messages: its volumes, when it mounts any.
fn pin_cause(spec: &temps_workload::WorkloadSpec) -> String {
    if spec.volumes.is_empty() {
        return String::new();
    }
    let names: Vec<String> = spec
        .volumes
        .iter()
        .map(|m| format!("`{}`", m.volume))
        .collect();
    format!(
        " by {} {}",
        if names.len() == 1 {
            "volume"
        } else {
            "volumes"
        },
        names.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{
        apply_request, deployment_spec, job_spec, job_spec_with, plane, plane_with, test_db,
        FakeDriver, RecordingFinalizer,
    };
    use crate::NodeDriver;
    use temps_workload::RestartPolicy;

    #[test]
    fn backoff_doubles_from_ten_seconds_to_six_minutes() {
        assert_eq!(backoff(1), Duration::seconds(10));
        assert_eq!(backoff(2), Duration::seconds(20));
        assert_eq!(backoff(3), Duration::seconds(40));
        assert_eq!(backoff(10), Duration::seconds(360));
        assert_eq!(backoff(u32::MAX), Duration::seconds(360));
    }

    #[test]
    fn the_output_tail_keeps_whole_last_lines() {
        let text = "first line\nsecond line\nthird line\n";
        assert_eq!(tail_of(text, 100), text);
        assert_eq!(tail_of(text, 15), "third line\n");
        assert_eq!(tail_of("é".repeat(10).as_str(), 5).chars().count(), 2);
    }

    #[tokio::test]
    async fn a_successful_run_finishes_then_expires_and_is_removed() {
        let Some(db) = test_db().await else { return };
        let (plane, driver, _audit) = plane_with(&db);
        let id = plane
            .apply(apply_request(
                10,
                "ok-run",
                job_spec_with("app:1", 600, 0, Some(30)),
            ))
            .await
            .expect("applied");
        let now = Utc::now();
        plane.reconcile_at(now, false).await;
        let running = driver.running();
        assert_eq!(running.len(), 1, "one instance started");
        let view = plane.get(id).await.expect("read");
        assert_eq!(view.status.phase, WorkloadPhase::Running);
        assert_eq!(view.attempt, 1);
        assert_eq!(view.observed_generation, view.generation);

        // A pass with nothing changed writes nothing.
        let seq = view.change_seq;
        plane.reconcile_at(now, false).await;
        assert_eq!(plane.get(id).await.expect("read").change_seq, seq);

        driver.exit(&running[0], 0);
        plane.reconcile_at(now, false).await;
        let view = plane.get(id).await.expect("read");
        assert_eq!(view.status.phase, WorkloadPhase::Succeeded);
        assert_eq!(view.status.exit_code, Some(0));
        let completed = view
            .status
            .condition(ConditionKind::Completed)
            .expect("completed");
        assert_eq!(completed.status, ConditionStatus::True);
        let expires = view.expires_at.expect("an expiry");
        assert!(
            driver.exists(&running[0]),
            "kept until its TTL for its logs"
        );

        plane
            .reconcile_at(expires + Duration::seconds(1), false)
            .await;
        assert!(!driver.exists(&running[0]), "removed at its TTL");
        assert!(matches!(
            plane.get(id).await,
            Err(PlaneError::NotFound { workload_id }) if workload_id == id
        ));
    }

    #[tokio::test]
    async fn failed_attempts_are_retried_up_to_the_limit_with_backoff() {
        let Some(db) = test_db().await else { return };
        let (plane, driver, _audit) = plane_with(&db);
        let id = plane
            .apply(apply_request(
                11,
                "retry-run",
                job_spec_with("app:1", 3_600, 2, None),
            ))
            .await
            .expect("applied");
        plane.reconcile_at(Utc::now(), false).await;
        for attempt in 1..=3u32 {
            let running = driver.running();
            assert_eq!(running.len(), 1, "attempt {attempt} runs alone");
            assert!(running[0].ends_with(&format!("-a{attempt}")), "{running:?}");
            driver.exit(&running[0], 3);
            // Observed at once; the next attempt waits for its back-off.
            plane.reconcile_at(Utc::now(), false).await;
            let view = plane.get(id).await.expect("read");
            if attempt < 3 {
                let running_condition = view
                    .status
                    .condition(ConditionKind::Running)
                    .expect("running condition");
                assert_eq!(running_condition.reason, "BackOff");
                assert!(driver.running().is_empty(), "nothing runs during back-off");
                plane
                    .reconcile_at(Utc::now() + Duration::seconds(400), false)
                    .await;
                assert!(
                    !driver.exists(&running[0]),
                    "the failed attempt was removed"
                );
            } else {
                assert_eq!(view.status.phase, WorkloadPhase::Failed);
                assert_eq!(view.status.exit_code, Some(3));
                let completed = view
                    .status
                    .condition(ConditionKind::Completed)
                    .expect("completed");
                assert_eq!(completed.status, ConditionStatus::False);
                assert_eq!(completed.reason, "BackoffLimitExceeded");
                assert!(
                    completed.message.contains("retry limit of 2"),
                    "{}",
                    completed.message
                );
            }
        }
        assert_eq!(driver.created().len(), 3);
    }

    #[tokio::test]
    async fn a_run_past_its_maximum_run_time_is_stopped_removed_and_failed() {
        let Some(db) = test_db().await else { return };
        let (plane, driver, _audit) = plane_with(&db);
        let id = plane
            .apply(apply_request(
                12,
                "slow-run",
                job_spec_with("app:1", 60, 3, None),
            ))
            .await
            .expect("applied");
        let created_at = plane.get(id).await.expect("read").created_at;
        plane.reconcile_at(Utc::now(), false).await;
        let running = driver.running();
        assert_eq!(running.len(), 1);
        plane
            .reconcile_at(created_at + Duration::seconds(61), false)
            .await;
        assert!(!driver.exists(&running[0]));
        let view = plane.get(id).await.expect("read");
        assert_eq!(view.status.phase, WorkloadPhase::Failed);
        let completed = view
            .status
            .condition(ConditionKind::Completed)
            .expect("completed");
        assert_eq!(completed.reason, "DeadlineExceeded");
        assert!(completed.message.contains("60s"), "{}", completed.message);
        // The retry limit does not restart a run past its deadline.
        assert_eq!(driver.created().len(), 1);
    }

    #[tokio::test]
    async fn a_run_that_exited_in_time_is_not_failed_by_a_late_pass() {
        let Some(db) = test_db().await else { return };
        let (plane, driver, _audit) = plane_with(&db);
        let id = plane
            .apply(apply_request(
                16,
                "cron-run",
                job_spec_with("app:1", 60, 0, None),
            ))
            .await
            .expect("applied");
        let created_at = plane.get(id).await.expect("read").created_at;
        plane.reconcile_at(Utc::now(), false).await;
        let running = driver.running();
        assert_eq!(running.len(), 1);
        // Exits 0 while no pass runs (Temps is stopped); the next pass comes
        // after the deadline.
        driver.exit(&running[0], 0);
        plane
            .reconcile_at(created_at + Duration::seconds(70), false)
            .await;
        let view = plane.get(id).await.expect("read");
        assert_eq!(view.status.phase, WorkloadPhase::Succeeded);
        assert_eq!(view.status.exit_code, Some(0));
        let completed = view
            .status
            .condition(ConditionKind::Completed)
            .expect("completed");
        assert_eq!(completed.reason, "Succeeded");
        assert!(driver.exists(&running[0]), "kept for its logs");
    }

    #[tokio::test]
    async fn a_failed_attempt_with_no_time_left_is_not_retried_past_the_deadline() {
        let Some(db) = test_db().await else { return };
        let (plane, driver, _audit) = plane_with(&db);
        let id = plane
            .apply(apply_request(
                17,
                "late-retry",
                job_spec_with("app:1", 60, 2, None),
            ))
            .await
            .expect("applied");
        let created_at = plane.get(id).await.expect("read").created_at;
        plane.reconcile_at(Utc::now(), false).await;
        let running = driver.running();
        driver.exit(&running[0], 3);
        plane
            .reconcile_at(created_at + Duration::seconds(70), false)
            .await;
        let view = plane.get(id).await.expect("read");
        assert_eq!(view.status.phase, WorkloadPhase::Failed);
        assert_eq!(view.status.exit_code, Some(3), "the real exit is kept");
        let completed = view
            .status
            .condition(ConditionKind::Completed)
            .expect("completed");
        assert_eq!(completed.reason, "DeadlineExceeded");
        assert!(
            completed.message.contains("exited 3"),
            "{}",
            completed.message
        );
        assert_eq!(driver.created().len(), 1, "no attempt past the deadline");
    }

    #[tokio::test]
    async fn without_a_driver_the_run_fails_with_no_eligible_node() {
        let Some(db) = test_db().await else { return };
        let (plane, _audit) = plane(&db, NodeDriver(None), Vec::new());
        let id = plane
            .apply(apply_request(13, "nowhere", job_spec("app:1")))
            .await
            .expect("applied");
        plane.reconcile_at(Utc::now(), false).await;
        let view = plane.get(id).await.expect("read");
        assert_eq!(view.status.phase, WorkloadPhase::Failed);
        let scheduled = view
            .status
            .condition(ConditionKind::Scheduled)
            .expect("scheduled");
        assert_eq!(scheduled.status, ConditionStatus::False);
        assert_eq!(scheduled.reason, "NoEligibleNode");
    }

    #[tokio::test]
    async fn finalizers_see_the_result_and_hold_deletion_until_they_succeed() {
        let Some(db) = test_db().await else { return };
        let driver = FakeDriver::new();
        driver.set_output("starting\nAPI_TOKEN=tok-abcdef123\ndone\n");
        let finalizer = RecordingFinalizer::new("test.temps.sh/record", 1);
        let (plane, _audit) = plane(
            &db,
            NodeDriver(Some(driver.clone())),
            vec![finalizer.clone()],
        );
        let mut request = apply_request(14, "final-run", job_spec_with("app:1", 600, 0, Some(1)));
        request.finalizers = vec!["test.temps.sh/record".into()];
        request
            .spec
            .env
            .vars
            .insert("API_TOKEN".into(), "tok-abcdef123".into());
        let id = plane.apply(request).await.expect("applied");
        let now = Utc::now();
        plane.reconcile_at(now, false).await;
        let running = driver.running();
        driver.exit(&running[0], 0);
        // The finalizer fails once: the workload keeps it.
        plane.reconcile_at(now, false).await;
        assert_eq!(
            plane.get(id).await.expect("read").finalizers,
            vec!["test.temps.sh/record".to_string()]
        );
        // Past the TTL, deletion waits for the finalizer, which now succeeds.
        plane.reconcile_at(now + Duration::seconds(5), false).await;
        let seen = finalizer.seen();
        let last = seen.last().expect("finalized");
        assert_eq!(last.status.phase, WorkloadPhase::Succeeded);
        assert_eq!(last.attempt, 1);
        let tail = last.output_tail.clone().expect("a tail");
        assert!(tail.contains("done"), "{tail}");
        assert!(
            !tail.contains("tok-abcdef123"),
            "secret values are redacted: {tail}"
        );
        plane.reconcile_at(now + Duration::seconds(6), false).await;
        assert!(matches!(
            plane.get(id).await,
            Err(PlaneError::NotFound { .. })
        ));
        assert!(!driver.exists(&running[0]));
    }

    #[tokio::test]
    async fn deleting_a_running_workload_stops_it_and_fails_it_with_the_reason() {
        let Some(db) = test_db().await else { return };
        let driver = FakeDriver::new();
        let finalizer = RecordingFinalizer::new("test.temps.sh/record", 0);
        let (plane, _audit) = plane(
            &db,
            NodeDriver(Some(driver.clone())),
            vec![finalizer.clone()],
        );
        let mut request = apply_request(15, "replaced-run", job_spec("app:1"));
        request.finalizers = vec!["test.temps.sh/record".into()];
        let id = plane.apply(request).await.expect("applied");
        plane.reconcile_at(Utc::now(), false).await;
        let running = driver.running();
        plane
            .delete(id, "Replaced", "a newer run replaced this one")
            .await
            .expect("deleted");
        plane.reconcile_at(Utc::now(), false).await;
        assert!(!driver.exists(&running[0]));
        let seen = finalizer.seen();
        let last = seen.last().expect("finalized");
        assert_eq!(last.status.phase, WorkloadPhase::Failed);
        let completed = last
            .status
            .condition(ConditionKind::Completed)
            .expect("completed");
        assert_eq!(completed.reason, "Replaced");
        assert!(matches!(
            plane.get(id).await,
            Err(PlaneError::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn a_row_written_before_a_crash_is_created_on_the_next_pass() {
        let Some(db) = test_db().await else { return };
        let (plane, driver, _audit) = plane_with(&db);
        let id = plane
            .apply(apply_request(16, "half-started", job_spec("app:1")))
            .await
            .expect("applied");
        plane.reconcile_at(Utc::now(), false).await;
        let running = driver.running();
        // The container vanishes and the row is put back to pending, as if
        // the process died between writing the row and creating it.
        driver.forget(&running[0]);
        workload_instances::Entity::update_many()
            .col_expr(
                workload_instances::Column::Phase,
                Expr::value(PHASE_PENDING),
            )
            .filter(workload_instances::Column::WorkloadId.eq(id))
            .exec(&*db.db)
            .await
            .expect("reset");
        plane.reconcile_at(Utc::now(), false).await;
        assert_eq!(driver.running(), running, "the same attempt was created");
        assert_eq!(plane.get(id).await.expect("read").attempt, 1);
    }

    #[tokio::test]
    async fn the_resync_removes_only_this_planes_untracked_instances() {
        let Some(db) = test_db().await else { return };
        let (plane, driver, _audit) = plane_with(&db);
        let id = plane
            .apply(apply_request(17, "tracked", job_spec("app:1")))
            .await
            .expect("applied");
        plane.reconcile_at(Utc::now(), false).await;
        let tracked = driver.running();
        let scope = plane.config().scope.clone();
        driver.insert_listed(
            "orphan-of-this-plane",
            &[
                (labels::PLANE, scope.as_str()),
                (labels::MANAGED_BY, labels::MANAGED_BY_VALUE),
                (workload_labels::WORKLOAD_ID, "999999"),
                (labels::ATTEMPT, "1"),
            ],
        );
        driver.insert_listed(
            "another-planes-instance",
            &[
                (labels::PLANE, "someone-else"),
                (labels::MANAGED_BY, labels::MANAGED_BY_VALUE),
                (workload_labels::WORKLOAD_ID, "999999"),
                (labels::ATTEMPT, "1"),
            ],
        );
        let outcome = plane.reconcile_at(Utc::now(), true).await;
        assert_eq!(outcome.orphans_removed, 1);
        assert!(!driver.exists("orphan-of-this-plane"));
        assert!(driver.exists("another-planes-instance"));
        assert!(driver.exists(&tracked[0]));
        assert_eq!(plane.get(id).await.expect("read").attempt, 1);
    }

    /// ADR-055 Phase 10, Firecracker for deployments: a `micro_vm` replica
    /// runs through the node-0 driver that provides it, never the primary
    /// one; and is refused with the reason when none does.
    #[tokio::test]
    async fn a_micro_vm_replica_runs_on_the_driver_that_provides_it() {
        let Some(db) = test_db().await else { return };
        let primary = FakeDriver::new();
        let vm = FakeDriver::with_isolation("fake-vm", temps_workload::Isolation::MicroVm);
        let (plane_without_vm, _) = plane(&db, NodeDriver(Some(primary.clone())), Vec::new());
        let audit = Arc::new(crate::testing::RecordingAudit::default());
        let with_vm = Arc::new(
            WorkloadPlane::new(
                db.db.clone(),
                NodeDriver(Some(primary.clone())),
                Arc::new(crate::NoEnvSource),
                audit,
                crate::PlaneConfig::new(format!(
                    "t{}",
                    &uuid::Uuid::new_v4().simple().to_string()[..8]
                )),
            )
            .with_control_plane_driver(vm.clone()),
        );
        assert_eq!(
            with_vm.control_plane_isolations(),
            vec![
                temps_workload::Isolation::Process,
                temps_workload::Isolation::MicroVm
            ]
        );

        let mut spec = deployment_spec("app:1");
        spec.isolation = temps_workload::Isolation::MicroVm;
        let id = with_vm
            .apply(apply_request(42, "app-vm-r1", spec.clone()))
            .await
            .expect("applied");
        with_vm.reconcile_at(Utc::now(), false).await;
        assert_eq!(vm.running().len(), 1, "the microVM driver runs it");
        assert!(primary.running().is_empty(), "never the container driver");
        let view = with_vm.get(id).await.expect("read");
        assert_eq!(view.status.phase, WorkloadPhase::Running);
        // Later operations find the same driver by the instance's name.
        let (handle, _) = with_vm
            .instance_status(id)
            .await
            .expect("inspect")
            .expect("an instance");
        assert_eq!(handle.driver, "fake-vm");

        let refused = plane_without_vm
            .apply(apply_request(43, "app-vm-r2", spec))
            .await
            .expect("admitted: placement decides");
        plane_without_vm.reconcile_at(Utc::now(), false).await;
        let view = plane_without_vm.get(refused).await.expect("read");
        assert_eq!(view.status.phase, WorkloadPhase::Failed);
        let scheduled = view
            .status
            .condition(ConditionKind::Scheduled)
            .expect("scheduled condition");
        assert_eq!(scheduled.reason, "NoEligibleNode");
        assert!(
            scheduled.message.contains("micro_vm isolation")
                && scheduled.message.contains("drivers: fake"),
            "{}",
            scheduled.message
        );
        assert!(
            primary.created().is_empty(),
            "nothing fell back to a container"
        );
    }

    #[tokio::test]
    async fn instances_created_here_get_the_cluster_resolver() {
        let Some(db) = test_db().await else { return };
        let driver = FakeDriver::new();
        let resolver = Arc::new(std::sync::RwLock::new(None::<String>));
        let slot = resolver.clone();
        let (plane, _audit) = crate::testing::plane_configured(
            &db,
            NodeDriver(Some(driver.clone())),
            Vec::new(),
            move |config| {
                config.cluster_dns = Some(temps_workload::ResolverSource::new(move || {
                    slot.read()
                        .ok()
                        .and_then(|ip| ip.clone())
                        .map(|ip| vec![ip, "1.1.1.1".to_string()])
                }))
            },
        );

        // Before the resolver is up the runtime's default is kept.
        plane
            .apply(apply_request(201, "before-dns", deployment_spec("app:1")))
            .await
            .expect("applied");
        plane.reconcile_at(Utc::now(), false).await;
        *resolver.write().expect("slot") = Some("10.0.0.53".to_string());
        plane
            .apply(apply_request(202, "after-dns", deployment_spec("app:1")))
            .await
            .expect("applied");
        plane.reconcile_at(Utc::now(), false).await;

        let resolvers: Vec<Option<Vec<String>>> = driver
            .created()
            .into_iter()
            .map(|(_, attach)| attach.network.dns)
            .collect();
        assert_eq!(
            resolvers,
            vec![
                None,
                Some(vec!["10.0.0.53".to_string(), "1.1.1.1".to_string()])
            ],
            "the resolver is read at each create"
        );
    }

    #[tokio::test]
    async fn workloads_past_a_full_batch_of_running_replicas_are_still_driven() {
        let Some(db) = test_db().await else { return };
        let driver = FakeDriver::new();
        let (plane, _audit) = crate::testing::plane_configured(
            &db,
            NodeDriver(Some(driver.clone())),
            Vec::new(),
            |config| config.pass_limit = 3,
        );
        let mut ids = Vec::new();
        for n in 0..5 {
            let id = plane
                .apply(apply_request(
                    100 + n,
                    &format!("replica-{n}"),
                    deployment_spec("app:1"),
                ))
                .await
                .expect("applied");
            ids.push(id);
        }
        let now = Utc::now();
        let first = plane.reconcile_at(now, false).await;
        assert_eq!(driver.running().len(), 3, "one batch started");
        assert_eq!(first.requeue_at, Some(now), "a full batch asks for more");

        let second = plane.reconcile_at(now, false).await;
        assert_eq!(
            driver.running().len(),
            5,
            "the rows after the first batch are created"
        );
        assert!(
            second.requeue_at.is_some_and(|at| at > now),
            "a wrapped batch of stable replicas is not re-read at once: {second:?}"
        );

        // Deleting the highest id, behind a full batch of running replicas,
        // is reached within one round.
        let last = *ids.last().expect("five ids");
        plane
            .delete(last, "Retired", "the deployment was replaced")
            .await
            .expect("deleted");
        for _ in 0..3 {
            plane.reconcile_at(now, false).await;
        }
        assert_eq!(driver.running().len(), 4, "the deleted replica stopped");
        assert!(matches!(
            plane.get(last).await,
            Err(PlaneError::NotFound { workload_id }) if workload_id == last
        ));
    }

    #[tokio::test]
    async fn a_deployment_replica_runs_until_its_owner_retires_it_and_is_never_recreated() {
        let Some(db) = test_db().await else { return };
        let (plane, driver, _audit) = plane_with(&db);
        let mut spec = deployment_spec("app:1");
        spec.resources.limits = Some(temps_workload::ResourceAmounts {
            memory_mb: Some(256),
            disk_mb: Some(1024),
            disk_io_mbps: Some(10),
            network_mbps: Some(5),
            ..Default::default()
        });
        let id = plane
            .apply(apply_request(42, "app-r1", spec))
            .await
            .expect("applied");
        let now = Utc::now();
        plane.reconcile_at(now, false).await;
        let running = driver.running();
        assert_eq!(running.len(), 1, "one instance started");
        let created = driver.created();
        let (instance, _) = &created[0];
        assert_eq!(instance.ports.len(), 1);
        assert_eq!(instance.ports[0].container_port, 3000);
        assert_eq!(
            instance.ports[0].host_ip, "127.0.0.1",
            "never all interfaces"
        );
        assert_eq!(instance.restart, RestartPolicy::UnlessStopped);
        assert_eq!(instance.limits.memory_bytes, Some(256 * 1024 * 1024));

        let view = plane.get(id).await.expect("read");
        assert_eq!(view.status.phase, WorkloadPhase::Running);
        let ready = view.status.condition(ConditionKind::Ready).expect("ready");
        assert_eq!(ready.status, ConditionStatus::True);
        assert_eq!(ready.reason, "NoProbe");
        // The fake driver applies no limit: each one is named, never ignored.
        let limit = view
            .status
            .condition(ConditionKind::LimitEnforced)
            .expect("limit condition");
        assert_eq!(limit.status, ConditionStatus::False);
        assert_eq!(limit.reason, "LimitUnsupported");
        assert!(
            limit.message.contains("1024 MB disk limit"),
            "{}",
            limit.message
        );
        assert!(limit.message.contains("memory"), "{}", limit.message);
        assert!(
            limit.message.contains("block I/O bandwidth"),
            "{}",
            limit.message
        );
        assert!(
            limit.message.contains("network bandwidth"),
            "{}",
            limit.message
        );
        assert_eq!(
            created[0].0.limits.disk_io_bytes_per_sec,
            Some(10 * 1024 * 1024)
        );
        assert_eq!(
            created[0].0.limits.network_bytes_per_sec,
            Some(5 * 1024 * 1024)
        );

        // Stopped by its owner (an environment put to sleep): reported, kept,
        // not replaced.
        driver.exit(&running[0], 0);
        plane.reconcile_at(now, false).await;
        let view = plane.get(id).await.expect("read");
        assert_eq!(view.status.phase, WorkloadPhase::Running);
        let state = view
            .status
            .condition(ConditionKind::Running)
            .expect("running");
        assert_eq!(state.status, ConditionStatus::False);
        assert_eq!(state.reason, "Stopped");
        assert!(driver.exists(&running[0]), "a stopped replica is kept");
        assert_eq!(driver.created().len(), 1, "and not replaced");

        // Removed outside the plane (the legacy retire path): the workload
        // fails with why, and nothing is recreated.
        driver.forget(&running[0]);
        plane.reconcile_at(now + Duration::seconds(30), false).await;
        let view = plane.get(id).await.expect("read");
        assert_eq!(view.status.phase, WorkloadPhase::Failed);
        let state = view
            .status
            .condition(ConditionKind::Running)
            .expect("running");
        assert_eq!(state.reason, "InstanceRemoved");
        plane.reconcile_at(now + Duration::seconds(60), false).await;
        assert_eq!(
            driver.created().len(),
            1,
            "a retired replica never comes back"
        );
    }

    #[tokio::test]
    async fn a_replica_pinned_to_a_worker_stays_unscheduled_with_the_reason() {
        let Some(db) = test_db().await else { return };
        let (plane, driver, _audit) = plane_with(&db);
        let mut spec = deployment_spec("app:1");
        spec.placement.nodes = Some(vec![3]);
        let id = plane
            .apply(apply_request(42, "app-pinned", spec))
            .await
            .expect("applied");
        plane.reconcile_at(Utc::now(), false).await;
        assert!(
            driver.created().is_empty(),
            "never quietly placed on node 0"
        );
        let view = plane.get(id).await.expect("read");
        assert!(!view.status.phase.is_terminal(), "pending, not failed");
        let scheduled = view
            .status
            .condition(ConditionKind::Scheduled)
            .expect("scheduled");
        assert_eq!(scheduled.status, ConditionStatus::False);
        assert_eq!(scheduled.reason, "PinnedNodeNotServed");
        assert!(
            scheduled.message.contains("node 3"),
            "{}",
            scheduled.message
        );
    }

    /// ADR-055 §2: the instance's success is persisted before the
    /// workload's conclusion. A process that dies between the two writes
    /// leaves a running workload with a succeeded instance; a pass after
    /// the deadline must conclude it Succeeded, not DeadlineExceeded.
    #[tokio::test]
    async fn a_success_persisted_before_a_crash_is_kept_by_a_pass_past_the_deadline() {
        let Some(db) = test_db().await else { return };
        let (plane, driver, _audit) = plane_with(&db);
        let id = plane
            .apply(apply_request(
                18,
                "crashed-conclude",
                job_spec_with("app:1", 60, 2, None),
            ))
            .await
            .expect("applied");
        let created_at = plane.get(id).await.expect("read").created_at;
        plane.reconcile_at(Utc::now(), false).await;
        assert_eq!(driver.running().len(), 1);
        let instance = workload_instances::Entity::find()
            .filter(workload_instances::Column::WorkloadId.eq(id))
            .one(&*db.db)
            .await
            .expect("read")
            .expect("an instance");
        // `finish_instance` ran (a worker's exit report writes the same
        // row), then the process stopped before `conclude`.
        plane
            .finish_instance(
                instance.id,
                PHASE_SUCCEEDED,
                Some(0),
                created_at + Duration::seconds(30),
            )
            .await
            .expect("finished");
        assert_eq!(
            plane.get(id).await.expect("read").status.phase,
            WorkloadPhase::Running
        );

        plane
            .reconcile_at(created_at + Duration::seconds(90), false)
            .await;
        let view = plane.get(id).await.expect("read");
        assert_eq!(view.status.phase, WorkloadPhase::Succeeded);
        assert_eq!(view.status.exit_code, Some(0));
        let completed = view
            .status
            .condition(ConditionKind::Completed)
            .expect("completed");
        assert_eq!(completed.reason, "Succeeded");
        assert_eq!(driver.created().len(), 1, "no retry of a succeeded run");
    }

    /// Places every attempt on worker 5. `stream` picks how the worker is
    /// driven: through its state stream, or through a runtime that acts
    /// only when called (a worker without the `state_stream` capability).
    struct OnWorkerFive {
        rpc: Arc<FakeDriver>,
        stream: bool,
        db: Arc<sea_orm::DatabaseConnection>,
        reachable: std::sync::atomic::AtomicBool,
        registered: std::sync::atomic::AtomicBool,
    }

    impl OnWorkerFive {
        fn new(db: &temps_database::test_utils::TestDatabase, stream: bool) -> Arc<Self> {
            Arc::new(Self {
                rpc: FakeDriver::new(),
                stream,
                db: db.db.clone(),
                reachable: std::sync::atomic::AtomicBool::new(true),
                registered: std::sync::atomic::AtomicBool::new(true),
            })
        }

        fn plane(
            self: &Arc<Self>,
            db: &temps_database::test_utils::TestDatabase,
        ) -> Arc<WorkloadPlane> {
            Arc::new(
                WorkloadPlane::new(
                    db.db.clone(),
                    NodeDriver(Some(FakeDriver::new())),
                    Arc::new(crate::NoEnvSource),
                    Arc::new(crate::testing::RecordingAudit::default()),
                    crate::PlaneConfig::new(format!(
                        "t{}",
                        &uuid::Uuid::new_v4().simple().to_string()[..8]
                    )),
                )
                .with_placement(self.clone()),
            )
        }

        fn driver(&self) -> Arc<dyn RuntimeDriver> {
            if self.stream {
                Arc::new(crate::node::StreamNodeDriver::new(
                    self.db.clone(),
                    5,
                    self.rpc.clone(),
                ))
            } else {
                self.rpc.clone()
            }
        }
    }

    #[async_trait::async_trait]
    impl crate::node::Placement for OnWorkerFive {
        async fn place(&self, _: &WorkloadView) -> Result<Placed, String> {
            Ok(Placed::Node {
                node_id: 5,
                node_name: "worker-5".into(),
                driver: self.driver(),
            })
        }

        async fn driver_for(
            &self,
            node_id: i32,
            _: &str,
        ) -> Result<Arc<dyn RuntimeDriver>, String> {
            if self.reachable.load(Ordering::Relaxed) {
                Ok(self.driver())
            } else {
                Err(format!("node {node_id}'s agent client could not be built"))
            }
        }

        async fn node_exists(&self, _: i32) -> Result<bool, String> {
            Ok(self.registered.load(Ordering::Relaxed))
        }
    }

    async fn instance_rows(db: &temps_database::test_utils::TestDatabase, id: i64) -> usize {
        workload_instances::Entity::find()
            .filter(workload_instances::Column::WorkloadId.eq(id))
            .all(&*db.db)
            .await
            .expect("read")
            .len()
    }

    /// A worker driven through calls never sees the instance row again
    /// once the workload is deleted: a removal that failed is retried, and
    /// the workload and its instance row are kept until it succeeds.
    #[tokio::test]
    async fn a_failed_removal_on_a_worker_without_the_stream_keeps_the_deletion_pending() {
        let Some(db) = test_db().await else { return };
        let worker = OnWorkerFive::new(&db, false);
        let plane = worker.plane(&db);
        let id = plane
            .apply(apply_request(19, "rpc-run", job_spec("app:1")))
            .await
            .expect("applied");
        plane.reconcile_at(Utc::now(), false).await;
        let running = worker.rpc.running();
        assert_eq!(running.len(), 1, "the worker's runtime created it");

        worker.rpc.set_unreachable(true);
        plane
            .delete(id, "Replaced", "a newer run replaced this one")
            .await
            .expect("deleted");
        for _ in 0..2 {
            let outcome = plane.reconcile_at(Utc::now(), false).await;
            assert!(outcome.failed >= 1, "the failed removal is reported");
            assert!(worker.rpc.exists(&running[0]), "still on the worker");
            let view = plane.get(id).await.expect("kept, pending deletion");
            assert!(view.deleted_at.is_some());
            assert_eq!(instance_rows(&db, id).await, 1, "the row is kept");
        }

        worker.rpc.set_unreachable(false);
        plane.reconcile_at(Utc::now(), false).await;
        assert!(!worker.rpc.exists(&running[0]), "removed on the retry");
        assert!(matches!(
            plane.get(id).await,
            Err(PlaneError::NotFound { .. })
        ));
        assert_eq!(instance_rows(&db, id).await, 0);
    }

    /// The same when the worker's driver cannot be built at all, until the
    /// node itself is deleted: then nothing can remove the instance, and
    /// the deletion goes ahead (the one bounded give-up).
    #[tokio::test]
    async fn an_unreachable_worker_without_the_stream_holds_the_deletion_until_deregistered() {
        let Some(db) = test_db().await else { return };
        let worker = OnWorkerFive::new(&db, false);
        let plane = worker.plane(&db);
        let id = plane
            .apply(apply_request(20, "rpc-gone", job_spec("app:1")))
            .await
            .expect("applied");
        plane.reconcile_at(Utc::now(), false).await;
        assert_eq!(worker.rpc.running().len(), 1);

        worker.reachable.store(false, Ordering::Relaxed);
        plane
            .delete(id, "Replaced", "a newer run replaced this one")
            .await
            .expect("deleted");
        plane.reconcile_at(Utc::now(), false).await;
        assert!(plane.get(id).await.is_ok(), "kept while the node exists");
        assert_eq!(instance_rows(&db, id).await, 1);

        worker.registered.store(false, Ordering::Relaxed);
        plane.reconcile_at(Utc::now(), false).await;
        assert!(matches!(
            plane.get(id).await,
            Err(PlaneError::NotFound { .. })
        ));
    }

    /// A worker that reads its stream removes the instance when its row
    /// goes, so the deletion never waits for its driver.
    #[tokio::test]
    async fn a_stream_worker_learns_of_the_deletion_from_the_row() {
        let Some(db) = test_db().await else { return };
        let worker = OnWorkerFive::new(&db, true);
        let plane = worker.plane(&db);
        let id = plane
            .apply(apply_request(21, "stream-run", job_spec("app:1")))
            .await
            .expect("applied");
        plane.reconcile_at(Utc::now(), false).await;
        assert_eq!(instance_rows(&db, id).await, 1);

        worker.reachable.store(false, Ordering::Relaxed);
        plane
            .delete(id, "Replaced", "a newer run replaced this one")
            .await
            .expect("deleted");
        plane.reconcile_at(Utc::now(), false).await;
        assert!(matches!(
            plane.get(id).await,
            Err(PlaneError::NotFound { .. })
        ));
        assert_eq!(instance_rows(&db, id).await, 0);
    }

    /// Past its deadline, a run on a worker driven through calls stays
    /// unfinished until its instance is actually stopped and removed.
    #[tokio::test]
    async fn a_run_past_its_deadline_on_a_worker_without_the_stream_waits_for_the_removal() {
        let Some(db) = test_db().await else { return };
        let worker = OnWorkerFive::new(&db, false);
        let plane = worker.plane(&db);
        let id = plane
            .apply(apply_request(
                22,
                "rpc-slow",
                job_spec_with("app:1", 60, 0, None),
            ))
            .await
            .expect("applied");
        let created_at = plane.get(id).await.expect("read").created_at;
        plane.reconcile_at(Utc::now(), false).await;
        let running = worker.rpc.running();
        assert_eq!(running.len(), 1);

        worker.rpc.set_unreachable(true);
        plane
            .reconcile_at(created_at + Duration::seconds(61), false)
            .await;
        assert!(worker.rpc.exists(&running[0]));
        assert!(
            !plane
                .get(id)
                .await
                .expect("read")
                .status
                .phase
                .is_terminal(),
            "not failed while its instance is still on the worker"
        );

        worker.rpc.set_unreachable(false);
        plane
            .reconcile_at(created_at + Duration::seconds(62), false)
            .await;
        assert!(!worker.rpc.exists(&running[0]));
        let view = plane.get(id).await.expect("read");
        assert_eq!(view.status.phase, WorkloadPhase::Failed);
        assert_eq!(
            view.status
                .condition(ConditionKind::Completed)
                .expect("completed")
                .reason,
            "DeadlineExceeded"
        );
    }
}
