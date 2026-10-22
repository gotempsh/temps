// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The plane's API: apply, wait, read, delete.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, EntityTrait, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect, Statement,
};
use temps_core::AuditLogger;
use temps_entities::{workload_instances, workloads};
use temps_workload::{
    ConditionKind, ConditionStatus, RuntimeDriver, WorkloadPhase, WorkloadSpec, WorkloadStatus,
};
use temps_workload_env::Redactor;
use tokio::sync::{watch, Notify};
use tokio::task::JoinHandle;

use crate::admission::{admit, AdmissionInput, AdmissionPolicy};
use crate::audit::AdmissionRefusedAudit;
use crate::env::EnvSource;
use crate::error::PlaneError;
use crate::events;

/// The product object that owns a workload: `cron/7`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Owner {
    pub kind: String,
    pub id: i64,
}

impl Owner {
    pub fn new(kind: impl Into<String>, id: i64) -> Self {
        Self {
            kind: kind.into(),
            id,
        }
    }
}

impl fmt::Display for Owner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.kind, self.id)
    }
}

/// One `apply`.
#[derive(Debug, Clone)]
pub struct ApplyRequest {
    pub owner: Owner,
    /// Unique per owner among live workloads; applying the same name again
    /// with the same spec is a no-op.
    pub name: String,
    pub spec: WorkloadSpec,
    /// The project the workload runs for, for the ADR-045 grant.
    pub project_slug: Option<String>,
    /// Finalizers the workload is created with. Each must be registered on
    /// the plane ([`WorkloadPlane::with_finalizer`]) or deletion waits.
    pub finalizers: Vec<String>,
}

/// How the plane is configured. Built once at startup.
#[derive(Debug, Clone)]
pub struct PlaneConfig {
    /// Value of the `sh.temps.plane` label on every instance this plane
    /// creates, and the prefix of their names. Orphan cleanup only touches
    /// instances with this value.
    pub scope: String,
    /// The network instances boot on (`temps_core::NETWORK_NAME`).
    pub network: Option<String>,
    pub admission: AdmissionPolicy,
    /// The slow resync: a full pass (orphan cleanup, event pruning) at
    /// least this often.
    pub resync_interval: Duration,
    /// How often a running instance is inspected when the driver cannot
    /// report exits, and how soon a failed pass is retried.
    pub poll_interval: Duration,
    /// Lines of output kept for a finished run.
    pub output_tail_lines: u32,
}

impl PlaneConfig {
    pub fn new(scope: impl Into<String>) -> Self {
        Self {
            scope: scope.into(),
            network: None,
            admission: AdmissionPolicy::default(),
            resync_interval: Duration::from_secs(300),
            poll_interval: Duration::from_secs(5),
            output_tail_lines: 50,
        }
    }
}

/// The runtime driver of the control plane (node 0), registered by `temps
/// serve`. `None` when this host has no runtime: workloads then fail with
/// `NoEligibleNode` instead of waiting.
#[derive(Clone, Default)]
pub struct NodeDriver(pub Option<Arc<dyn RuntimeDriver>>);

impl fmt::Debug for NodeDriver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(driver) => write!(f, "NodeDriver({})", driver.capabilities().name),
            None => f.write_str("NodeDriver(none)"),
        }
    }
}

/// A workload as the plane reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkloadView {
    pub id: i64,
    pub owner: Owner,
    pub name: String,
    pub spec: WorkloadSpec,
    pub status: WorkloadStatus,
    pub generation: i64,
    pub observed_generation: i64,
    pub change_seq: i64,
    pub finalizers: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    /// The latest attempt (instance) number; 0 before the first started.
    pub attempt: u32,
    /// When the latest attempt finished.
    pub finished_at: Option<DateTime<Utc>>,
    /// The last lines of the latest attempt's output, with secret values
    /// redacted. Only filled for finalizers; `None` when unavailable.
    pub output_tail: Option<String>,
}

/// Cleanup a product runs before a workload of its is removed (a run
/// history row, a role drop). The plane calls it once the workload is
/// finished or deleted, and removes the finalizer when it returns `Ok`;
/// on `Err` it retries on a later pass. Must be idempotent.
#[async_trait]
pub trait Finalizer: Send + Sync {
    fn name(&self) -> &str;
    async fn finalize(&self, workload: &WorkloadView) -> Result<(), String>;
}

/// The running reconciler. Dropping it leaves the loop running;
/// [`ReconcilerHandle::abort`] stops it and every exit watcher at once,
/// which is what a crash looks like to the next plane.
pub struct ReconcilerHandle {
    pub(crate) task: JoinHandle<()>,
    pub(crate) plane: Arc<WorkloadPlane>,
}

impl ReconcilerHandle {
    pub fn abort(self) {
        self.task.abort();
        self.plane.abort_watchers();
    }
}

pub struct WorkloadPlane {
    pub(crate) db: Arc<DatabaseConnection>,
    pub(crate) driver: NodeDriver,
    pub(crate) env: Arc<dyn EnvSource>,
    pub(crate) audit: Arc<dyn AuditLogger>,
    pub(crate) config: PlaneConfig,
    pub(crate) finalizers: BTreeMap<String, Arc<dyn Finalizer>>,
    /// Wakes the reconciler: on apply, on delete, on an instance exit.
    pub(crate) notify: Arc<Notify>,
    /// Bumped on every status write, for `apply_and_wait`.
    pub(crate) status_tx: watch::Sender<u64>,
    pub(crate) watchers: Mutex<HashMap<i64, JoinHandle<()>>>,
    /// Redactors of runs started by this process, by workload id.
    pub(crate) redactors: Mutex<HashMap<i64, Redactor>>,
    /// Cleared when the driver reports it cannot wait for exits.
    pub(crate) driver_waits: Arc<AtomicBool>,
    pub(crate) started: AtomicBool,
}

impl fmt::Debug for WorkloadPlane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkloadPlane")
            .field("scope", &self.config.scope)
            .field("driver", &self.driver)
            .field("finalizers", &self.finalizers.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl WorkloadPlane {
    pub fn new(
        db: Arc<DatabaseConnection>,
        driver: NodeDriver,
        env: Arc<dyn EnvSource>,
        audit: Arc<dyn AuditLogger>,
        config: PlaneConfig,
    ) -> Self {
        Self {
            db,
            driver,
            env,
            audit,
            config,
            finalizers: BTreeMap::new(),
            notify: Arc::new(Notify::new()),
            status_tx: watch::channel(0).0,
            watchers: Mutex::new(HashMap::new()),
            redactors: Mutex::new(HashMap::new()),
            driver_waits: Arc::new(AtomicBool::new(true)),
            started: AtomicBool::new(false),
        }
    }

    pub fn with_finalizer(mut self, finalizer: Arc<dyn Finalizer>) -> Self {
        self.finalizers
            .insert(finalizer.name().to_string(), finalizer);
        self
    }

    pub fn config(&self) -> &PlaneConfig {
        &self.config
    }

    /// Ask the reconciler for a pass.
    pub fn wake(&self) {
        self.notify.notify_one();
    }

    /// Start the reconciler. Only once per plane: a second call is refused,
    /// so a host never runs two reconcilers over the same objects.
    pub fn start(self: &Arc<Self>) -> Result<ReconcilerHandle, PlaneError> {
        if self.started.swap(true, Ordering::SeqCst) {
            return Err(PlaneError::ReconcilerAlreadyStarted {
                scope: self.config.scope.clone(),
            });
        }
        let task = tokio::spawn(crate::reconciler::run(Arc::clone(self)));
        Ok(ReconcilerHandle {
            task,
            plane: Arc::clone(self),
        })
    }

    pub(crate) fn abort_watchers(&self) {
        let mut watchers = self
            .watchers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (_, watcher) in watchers.drain() {
            watcher.abort();
        }
    }

    /// Admit `request.spec` and store it. Returns the workload id.
    pub async fn apply(&self, request: ApplyRequest) -> Result<i64, PlaneError> {
        let ApplyRequest {
            owner,
            name,
            spec,
            project_slug,
            finalizers,
        } = request;
        let db = self.db.as_ref();
        let existing = workloads::Entity::find()
            .filter(workloads::Column::OwnerKind.eq(owner.kind.as_str()))
            .filter(workloads::Column::OwnerId.eq(owner.id))
            .filter(workloads::Column::Name.eq(name.as_str()))
            .filter(workloads::Column::DeletedAt.is_null())
            .one(db)
            .await
            .map_err(PlaneError::db("reading the workload to apply", &name))?;
        let active = workloads::Entity::find()
            .filter(workloads::Column::OwnerKind.eq(owner.kind.as_str()))
            .filter(workloads::Column::OwnerId.eq(owner.id))
            .filter(workloads::Column::Name.ne(name.as_str()))
            .filter(workloads::Column::DeletedAt.is_null())
            .filter(workloads::Column::Phase.is_not_in([
                WorkloadPhase::Succeeded.as_str(),
                WorkloadPhase::Failed.as_str(),
            ]))
            .count(db)
            .await
            .map_err(PlaneError::db(
                "counting the owner's active workloads",
                &name,
            ))?;

        let admitted = match admit(
            &self.config.admission,
            AdmissionInput {
                spec,
                name: &name,
                owner: &owner,
                project_slug: project_slug.as_deref(),
                active_for_owner: u32::try_from(active).unwrap_or(u32::MAX),
            },
        ) {
            Ok(spec) => spec,
            Err(refusal) => {
                tracing::warn!(
                    workload = %refusal.workload,
                    owner = %refusal.owner,
                    rule = %refusal.rule,
                    "workload refused at admission: {}",
                    refusal.message
                );
                if let Err(error) = self
                    .audit
                    .create_audit_log(&AdmissionRefusedAudit::from(&refusal))
                    .await
                {
                    tracing::error!(
                        workload = %refusal.workload,
                        "failed to audit-log an admission refusal: {error}"
                    );
                }
                return Err(refusal.into());
            }
        };
        let profile = admitted.security.map(|p| p.as_str()).unwrap_or("none");
        let spec_json = serde_json::to_value(&admitted).map_err(|e| PlaneError::Encode {
            workload: name.clone(),
            field: "spec",
            reason: e.to_string(),
        })?;

        if let Some(row) = existing {
            if row.spec == spec_json {
                return Ok(row.id);
            }
            workloads::Entity::update_many()
                .col_expr(workloads::Column::Spec, Expr::value(spec_json))
                .col_expr(
                    workloads::Column::Generation,
                    Expr::col(workloads::Column::Generation).add(1),
                )
                .filter(workloads::Column::Id.eq(row.id))
                .exec(db)
                .await
                .map_err(PlaneError::db("updating the workload spec", row.id))?;
            events::record(
                db,
                row.id,
                None,
                "SpecUpdated",
                &format!("generation {} admitted as {profile}", row.generation + 1),
                Utc::now(),
            )
            .await?;
            self.wake();
            return Ok(row.id);
        }

        let inserted = db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO workloads (owner_kind, owner_id, name, spec, finalizers) \
                 VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT (owner_kind, owner_id, name) WHERE deleted_at IS NULL DO NOTHING \
                 RETURNING id",
                vec![
                    owner.kind.as_str().into(),
                    owner.id.into(),
                    name.as_str().into(),
                    spec_json.into(),
                    finalizers.into(),
                ],
            ))
            .await
            .map_err(PlaneError::db("inserting the workload", &name))?;
        let id = match inserted {
            Some(row) => row
                .try_get::<i64>("", "id")
                .map_err(PlaneError::db("reading the inserted workload id", &name))?,
            // Another apply of the same name won the race: it is that one.
            None => workloads::Entity::find()
                .filter(workloads::Column::OwnerKind.eq(owner.kind.as_str()))
                .filter(workloads::Column::OwnerId.eq(owner.id))
                .filter(workloads::Column::Name.eq(name.as_str()))
                .filter(workloads::Column::DeletedAt.is_null())
                .one(db)
                .await
                .map_err(PlaneError::db(
                    "reading the concurrently applied workload",
                    &name,
                ))?
                .map(|row| row.id)
                .ok_or_else(|| PlaneError::Database {
                    operation: "inserting the workload",
                    workload: name.clone(),
                    source: sea_orm::DbErr::RecordNotInserted,
                })?,
        };
        events::record(
            db,
            id,
            None,
            "Admitted",
            &format!("{owner} workload '{name}' admitted as {profile}"),
            Utc::now(),
        )
        .await?;
        tracing::info!(workload_id = id, owner = %owner, name = %name, profile, "workload applied");
        self.wake();
        Ok(id)
    }

    /// Apply, then wait until `condition` is `True`. A workload that finishes
    /// (or is deleted) first returns [`PlaneError::ConditionFailed`]; the
    /// timeout returns [`PlaneError::WaitTimedOut`]. Both carry the
    /// condition's reason and message.
    pub async fn apply_and_wait(
        &self,
        request: ApplyRequest,
        condition: ConditionKind,
        timeout: Duration,
    ) -> Result<WorkloadView, PlaneError> {
        let id = self.apply(request).await?;
        self.wait_for(id, condition, timeout).await
    }

    /// Wait until `condition` of workload `id` is `True`.
    pub async fn wait_for(
        &self,
        id: i64,
        condition: ConditionKind,
        timeout: Duration,
    ) -> Result<WorkloadView, PlaneError> {
        let started = tokio::time::Instant::now();
        let deadline = started + timeout;
        let mut changes = self.status_tx.subscribe();
        loop {
            let view = self.get(id).await?;
            let current = view.status.condition(condition);
            if current.is_some_and(|c| c.status == ConditionStatus::True) {
                return Ok(view);
            }
            let (status, reason, message) = explain(&view.status, condition);
            if view.status.phase.is_terminal() || view.deleted_at.is_some() {
                return Err(PlaneError::ConditionFailed {
                    workload_id: id,
                    name: view.name,
                    condition,
                    status,
                    reason,
                    message,
                });
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(PlaneError::WaitTimedOut {
                    workload_id: id,
                    name: view.name,
                    condition,
                    waited_ms: u64::try_from(now.duration_since(started).as_millis())
                        .unwrap_or(u64::MAX),
                    reason,
                    message,
                });
            }
            // A status write wakes us; the poll bounds a write made by
            // another process.
            let wait = (deadline - now).min(Duration::from_secs(1));
            match tokio::time::timeout(wait, changes.changed()).await {
                Ok(Ok(())) | Err(_) => {}
                Ok(Err(_)) => tokio::time::sleep(wait).await,
            }
        }
    }

    /// The workload `id`, deleted or not.
    pub async fn get(&self, id: i64) -> Result<WorkloadView, PlaneError> {
        let row = workloads::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await
            .map_err(PlaneError::db("reading the workload", id))?
            .ok_or(PlaneError::NotFound { workload_id: id })?;
        let latest = workload_instances::Entity::find()
            .filter(workload_instances::Column::WorkloadId.eq(id))
            .order_by_desc(workload_instances::Column::Attempt)
            .limit(1)
            .one(self.db.as_ref())
            .await
            .map_err(PlaneError::db("reading the latest instance", id))?;
        view_from(row, latest.as_ref())
    }

    /// Live workloads of `owner` that have not finished, oldest first.
    pub async fn list_active(&self, owner: &Owner) -> Result<Vec<WorkloadView>, PlaneError> {
        let rows = workloads::Entity::find()
            .filter(workloads::Column::OwnerKind.eq(owner.kind.as_str()))
            .filter(workloads::Column::OwnerId.eq(owner.id))
            .filter(workloads::Column::DeletedAt.is_null())
            .filter(workloads::Column::Phase.is_not_in([
                WorkloadPhase::Succeeded.as_str(),
                WorkloadPhase::Failed.as_str(),
            ]))
            .order_by_asc(workloads::Column::Id)
            .all(self.db.as_ref())
            .await
            .map_err(PlaneError::db(
                "listing the owner's active workloads",
                owner,
            ))?;
        rows.into_iter().map(|row| view_from(row, None)).collect()
    }

    /// The workload's events, newest first.
    pub async fn events(&self, id: i64) -> Result<Vec<temps_workload::WorkloadEvent>, PlaneError> {
        events::list(self.db.as_ref(), id).await
    }

    /// Delete workload `id`: its instances are stopped and removed, its
    /// finalizers run, then the row goes. An unfinished workload fails
    /// with `reason`. Deleting a deleted workload is a no-op.
    pub async fn delete(&self, id: i64, reason: &str, message: &str) -> Result<(), PlaneError> {
        let db = self.db.as_ref();
        let row = workloads::Entity::find_by_id(id)
            .one(db)
            .await
            .map_err(PlaneError::db("reading the workload to delete", id))?
            .ok_or(PlaneError::NotFound { workload_id: id })?;
        if row.deleted_at.is_some() {
            return Ok(());
        }
        let now = Utc::now();
        let mut status = decode_status(&row)?;
        if !status.phase.is_terminal() {
            status.set_condition(
                ConditionKind::Completed,
                ConditionStatus::False,
                reason,
                message,
                now,
            );
            status.ready_instances = 0;
            status.phase = WorkloadPhase::Terminating;
        }
        let status_json = encode_status(&row.name, &status)?;
        workloads::Entity::update_many()
            .col_expr(workloads::Column::DeletedAt, Expr::value(now))
            .col_expr(workloads::Column::Status, Expr::value(status_json))
            .col_expr(workloads::Column::Phase, Expr::value(status.phase.as_str()))
            .filter(workloads::Column::Id.eq(id))
            .exec(db)
            .await
            .map_err(PlaneError::db("marking the workload deleted", id))?;
        events::record(db, id, None, reason, message, now).await?;
        self.status_tx.send_modify(|seq| *seq += 1);
        self.wake();
        Ok(())
    }
}

/// The condition's status, reason and message; when it is not set, the
/// first `False` condition, which is usually why.
fn explain(status: &WorkloadStatus, condition: ConditionKind) -> (ConditionStatus, String, String) {
    if let Some(c) = status.condition(condition) {
        return (c.status, c.reason.clone(), c.message.clone());
    }
    if let Some(c) = status
        .conditions
        .iter()
        .find(|c| c.status == ConditionStatus::False)
    {
        return (
            ConditionStatus::Unknown,
            c.reason.clone(),
            format!("{:?} is False: {}", c.kind, c.message),
        );
    }
    (
        ConditionStatus::Unknown,
        "NotObserved".to_string(),
        format!("the plane has not reported {condition:?} yet"),
    )
}

pub(crate) fn decode_status(row: &workloads::Model) -> Result<WorkloadStatus, PlaneError> {
    serde_json::from_value(row.status.clone()).map_err(|e| PlaneError::Corrupt {
        workload_id: row.id,
        field: "status",
        reason: e.to_string(),
    })
}

pub(crate) fn encode_status(
    name: &str,
    status: &WorkloadStatus,
) -> Result<serde_json::Value, PlaneError> {
    serde_json::to_value(status).map_err(|e| PlaneError::Encode {
        workload: name.to_string(),
        field: "status",
        reason: e.to_string(),
    })
}

pub(crate) fn view_from(
    row: workloads::Model,
    latest: Option<&workload_instances::Model>,
) -> Result<WorkloadView, PlaneError> {
    let spec: WorkloadSpec =
        serde_json::from_value(row.spec.clone()).map_err(|e| PlaneError::Corrupt {
            workload_id: row.id,
            field: "spec",
            reason: e.to_string(),
        })?;
    let status = decode_status(&row)?;
    Ok(WorkloadView {
        id: row.id,
        owner: Owner::new(row.owner_kind, row.owner_id),
        name: row.name,
        spec,
        status,
        generation: row.generation,
        observed_generation: row.observed_generation,
        change_seq: row.change_seq,
        finalizers: row.finalizers,
        created_at: row.created_at,
        deleted_at: row.deleted_at,
        expires_at: row.expires_at,
        attempt: latest
            .map(|i| u32::try_from(i.attempt).unwrap_or(0))
            .unwrap_or(0),
        finished_at: latest.and_then(|i| i.finished_at),
        output_tail: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{apply_request, job_spec, job_spec_with, plane_with, test_db};

    #[tokio::test]
    async fn applying_the_same_spec_twice_is_one_workload() {
        let Some(db) = test_db().await else { return };
        let (plane, _driver, _audit) = plane_with(&db);
        let first = plane
            .apply(apply_request(1, "run-a", job_spec("app:1")))
            .await
            .expect("applied");
        let again = plane
            .apply(apply_request(1, "run-a", job_spec("app:1")))
            .await
            .expect("applied");
        assert_eq!(first, again);
        let view = plane.get(first).await.expect("read");
        assert_eq!(view.generation, 1);

        let third = plane
            .apply(apply_request(1, "run-a", job_spec("app:2")))
            .await
            .expect("applied");
        assert_eq!(third, first);
        let view = plane.get(first).await.expect("read");
        assert_eq!(view.generation, 2);
        assert!(view.change_seq > 0);
    }

    #[tokio::test]
    async fn a_refusal_is_typed_and_audit_logged() {
        let Some(db) = test_db().await else { return };
        let (plane, _driver, audit) = plane_with(&db);
        let mut spec = job_spec("app:1");
        spec.security = Some(temps_workload::SecurityProfile::HostPrivileged);
        let error = plane
            .apply(apply_request(2, "run-b", spec))
            .await
            .expect_err("refused");
        let PlaneError::Admission(refusal) = error else {
            panic!("expected an admission error, got {error}");
        };
        assert_eq!(refusal.rule, crate::AdmissionRule::SecurityProfile);
        assert_eq!(refusal.workload, "run-b");
        let logged = audit.operations();
        assert_eq!(logged.len(), 1);
        assert!(
            logged[0].starts_with("WORKLOAD_ADMISSION_REFUSED"),
            "{}",
            logged[0]
        );
        assert!(logged[0].contains("security_profile"), "{}", logged[0]);
    }

    #[tokio::test]
    async fn the_quota_counts_the_owners_unfinished_workloads() {
        let Some(db) = test_db().await else { return };
        let (plane, _driver, _audit) = plane_with(&db);
        for i in 0..5 {
            plane
                .apply(apply_request(4, &format!("run-{i}"), job_spec("app:1")))
                .await
                .expect("applied");
        }
        let error = plane
            .apply(apply_request(4, "run-5", job_spec("app:1")))
            .await
            .expect_err("over quota");
        assert!(
            matches!(&error, PlaneError::Admission(r) if r.rule == crate::AdmissionRule::Quota),
            "{error}"
        );
        // Another owner is not affected.
        plane
            .apply(apply_request(5, "run-0", job_spec("app:1")))
            .await
            .expect("applied");
    }

    #[tokio::test]
    async fn apply_and_wait_fails_with_the_reason_of_a_finished_run() {
        let Some(db) = test_db().await else { return };
        let (plane, driver, _audit) = plane_with(&db);
        driver.fail_prepare(true);
        let id = plane
            .apply(apply_request(3, "run-c", job_spec("app:1")))
            .await
            .expect("applied");
        plane.reconcile_at(Utc::now(), false).await;
        let error = plane
            .wait_for(id, ConditionKind::Ready, Duration::from_millis(200))
            .await
            .expect_err("failed");
        match error {
            PlaneError::ConditionFailed {
                reason, message, ..
            } => {
                assert_eq!(reason, "ArtifactPullFailed");
                assert!(message.contains("app:1"), "{message}");
            }
            other => panic!("expected ConditionFailed, got {other}"),
        }
    }

    #[tokio::test]
    async fn apply_and_wait_times_out_with_the_condition_reason() {
        let Some(db) = test_db().await else { return };
        let (plane, driver, _audit) = plane_with(&db);
        let id = plane
            .apply(apply_request(
                6,
                "run-d",
                job_spec_with("app:1", 600, 1, None),
            ))
            .await
            .expect("applied");
        plane.reconcile_at(Utc::now(), false).await;
        let running = driver.running();
        driver.exit(&running[0], 1);
        // Observed; the retry waits for its back-off.
        plane.reconcile_at(Utc::now(), false).await;
        let error = plane
            .wait_for(id, ConditionKind::Running, Duration::from_millis(150))
            .await
            .expect_err("timed out");
        let PlaneError::WaitTimedOut {
            reason,
            message,
            condition,
            workload_id,
            ..
        } = error
        else {
            panic!("expected WaitTimedOut, got {error}");
        };
        assert_eq!(workload_id, id);
        assert_eq!(condition, ConditionKind::Running);
        assert_eq!(reason, "BackOff");
        assert!(message.contains("attempt 1 exited 1"), "{message}");

        // A condition never reported says so.
        let pending = plane
            .apply(apply_request(6, "run-e", job_spec("app:1")))
            .await
            .expect("applied");
        let error = plane
            .apply_and_wait(
                apply_request(6, "run-e", job_spec("app:1")),
                ConditionKind::Completed,
                Duration::from_millis(100),
            )
            .await
            .expect_err("timed out");
        assert!(
            matches!(&error, PlaneError::WaitTimedOut { reason, workload_id, .. }
                if reason == "NotObserved" && *workload_id == pending),
            "{error}"
        );
    }
}
