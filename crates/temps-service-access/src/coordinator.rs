// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Control-plane orchestration for explicit operator approvals.
//! No queued intention is reported as connected; only exact worker ACKs write
//! an applied version and the original signed policy lease deadline.
mod assignments;
mod batches;

use crate::{
    authority::{AuthorityError, AuthorityStore},
    delivery::{DeliveryClaim, DeliveryError, DeliveryQueue},
    error::AccessError,
    planner::{PlanError, PolicyPlanner},
    service::{AuthorizedGrant, FreshConsent, ServiceAccessService},
    transport::{StoredTarget, TransportError, WorkerTransport},
};
use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QuerySelect, TransactionTrait,
};
use std::sync::Arc;
use temps_core::{AuditLogger, AuditOperation, EncryptionService, ProjectAccessChecker};
use temps_entities::{
    sandbox_access_delivery::DeliveryOperation, sandbox_access_runtimes as runtimes, sandboxes,
};
use tokio::sync::{Notify, Semaphore};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum CoordinatorError {
    #[error("Cannot {operation} service access for sandbox {sandbox_id}: {reason}")]
    Invalid {
        sandbox_id: String,
        operation: &'static str,
        reason: &'static str,
    },
    #[error("Cannot {operation} service access for sandbox {sandbox_id}: database failed")]
    Database {
        sandbox_id: String,
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },
    #[error(transparent)]
    Access(#[from] AccessError),
    #[error(transparent)]
    Authority(#[from] AuthorityError),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error(transparent)]
    Delivery(#[from] DeliveryError),
}
fn database(
    id: impl ToString,
    operation: &'static str,
    source: sea_orm::DbErr,
) -> CoordinatorError {
    CoordinatorError::Database {
        sandbox_id: id.to_string(),
        operation,
        source,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum DispatchOutcome {
    Applied,
    Retired,
    Superseded,
    Contended,
}

pub struct ServiceAccessCoordinator {
    db: Arc<DatabaseConnection>,
    service: Arc<ServiceAccessService>,
    transport: Arc<WorkerTransport>,
    authority: AuthorityStore,
    planner: PolicyPlanner,
    queue: DeliveryQueue,
    audit: Arc<dyn AuditLogger>,
    admission: Semaphore,
    dispatch_slots: Semaphore,
    planning: Semaphore,
    wake: Notify,
    batches: batches::RenewalBatches,
}
impl ServiceAccessCoordinator {
    pub fn new(
        db: Arc<DatabaseConnection>,
        encryption: Arc<EncryptionService>,
        audit: Arc<dyn AuditLogger>,
        service: Arc<ServiceAccessService>,
        transport: Arc<WorkerTransport>,
        checker: Option<Arc<dyn ProjectAccessChecker>>,
        github: Arc<dyn crate::github::GithubCredentialResolver>,
    ) -> Self {
        Self {
            authority: AuthorityStore::new(db.clone(), encryption.clone(), audit.clone()),
            planner: PolicyPlanner::new(db.clone(), encryption, checker, github),
            queue: DeliveryQueue::new(db.clone()),
            db,
            service,
            transport,
            audit,
            admission: Semaphore::new(2),
            dispatch_slots: Semaphore::new(crate::transport::DISPATCH_CONCURRENCY),
            planning: Semaphore::new(2),
            wake: Notify::new(),
            batches: batches::RenewalBatches::default(),
        }
    }

    pub(crate) fn with_project_access_binding(
        mut self,
        binding: Arc<crate::planner::ProjectAccessBinding>,
    ) -> Self {
        self.planner.bind_project_access(binding);
        self
    }

    /// The caller must first authorize both effective project and sandbox write
    /// permissions, and audit the new grant. Identity is resolved host-side,
    /// then rechecked transactionally before fresh consent is persisted.
    pub async fn connect(
        &self,
        consent: FreshConsent,
    ) -> Result<AuthorizedGrant, CoordinatorError> {
        consent.validate()?;
        let _permit = self.admit(&consent.sandbox_id)?;
        let mut query = sandboxes::Entity::find()
            .filter(sandboxes::Column::PublicId.eq(&consent.sandbox_id))
            .filter(sandboxes::Column::ProjectId.eq(consent.project_id));
        if !consent.admin {
            query = query.filter(sandboxes::Column::UserId.eq(consent.user_id));
        }
        let sandbox = query
            .one(self.db.as_ref())
            .await
            .map_err(|e| database(&consent.sandbox_id, "resolve approved sandbox", e))?
            .ok_or_else(|| AccessError::NotFound {
                kind: "sandbox",
                id: consent.sandbox_id.clone(),
                project_id: consent.project_id,
            })?;
        let observed = self.transport.inspect(&sandbox).await?;
        self.authority
            .register(observed.node_id, &observed.worker)
            .await?;
        let grant = self.service.authorize_grant(consent, observed).await?;
        self.wake.notify_waiters();
        Ok(grant)
    }

    /// Issue one immutable Vault session using a single authenticated worker
    /// observation. The service commits every selected grant atomically.
    pub async fn connect_session(
        &self,
        project_id: i32,
        vault_id: &str,
        user_id: i32,
        admin: bool,
        request: crate::session_types::CreateAccessSession,
    ) -> Result<crate::session_types::AccessSessionResponse, CoordinatorError> {
        crate::service::sessions::validate(&request)?;
        let sandbox_id = request.consumer.sandbox_id();
        let _permit = self.admit(sandbox_id)?;
        let mut query = sandboxes::Entity::find()
            .filter(sandboxes::Column::PublicId.eq(sandbox_id))
            .filter(sandboxes::Column::ProjectId.eq(project_id));
        if !admin {
            query = query.filter(sandboxes::Column::UserId.eq(user_id));
        }
        let sandbox = query
            .one(self.db.as_ref())
            .await
            .map_err(|e| database(sandbox_id, "resolve session consumer", e))?
            .ok_or_else(|| AccessError::NotFound {
                kind: "sandbox",
                id: sandbox_id.into(),
                project_id,
            })?;
        let observed = self.transport.inspect(&sandbox).await?;
        self.authority
            .register(observed.node_id, &observed.worker)
            .await?;
        let session = self
            .service
            .authorize_session(project_id, vault_id, user_id, admin, request, observed)
            .await?;
        self.wake.notify_waiters();
        Ok(session)
    }

    /// One bounded command. A failed attempt preserves its authorization and
    /// releases its durable claim with backoff.
    pub async fn dispatch_one(&self) -> Result<Option<DispatchOutcome>, CoordinatorError> {
        self.dispatch_next().await?.transpose()
    }

    /// Distinguish a processed failure from an empty queue or a claim failure:
    /// dispatchers must drain other workers immediately after a failed attempt.
    pub(crate) async fn dispatch_next(
        &self,
    ) -> Result<Option<Result<DispatchOutcome, CoordinatorError>>, CoordinatorError> {
        let _slot = self
            .dispatch_slots
            .try_acquire()
            .map_err(|_| CoordinatorError::Invalid {
                sandbox_id: "delivery queue".into(),
                operation: "dispatch",
                reason: "four deliveries are already in progress; retry later",
            })?;
        let (mut claims, had_candidates) = self.queue.claim_with_progress(1).await?;
        let Some(claim) = claims.pop() else {
            return Ok(had_candidates.then_some(Ok(DispatchOutcome::Contended)));
        };
        let result = self.deliver(&claim).await;
        if result.is_err() {
            self.queue.retry(&claim).await?;
        }
        Ok(Some(result))
    }

    /// Bounded maintenance tick. Authorization changes wake their worker in the
    /// database; periodic rechecks renew only still-authorized short leases.
    pub async fn maintain(&self) -> Result<crate::delivery::ScheduledBatch, CoordinatorError> {
        let batch = self.queue.schedule_due(16).await?;
        self.queue.prune_completed().await?;
        if batch.queued > 0 {
            self.wake.notify_waiters();
        }
        Ok(batch)
    }

    pub(crate) async fn maintain_assignments(&self) -> Result<usize, CoordinatorError> {
        let assignments = self.service.due_assignments().await?;
        let count = assignments.processed;
        let mut groups = std::collections::BTreeMap::new();
        for assignment in assignments.assignments {
            groups
                .entry((assignment.project_id, assignment.sandbox_id))
                .or_insert_with(Vec::new)
                .push(assignment);
        }
        assignments::run_groups(groups.into_values().collect(), |group| async move {
            let Some(first) = group.first() else { return; };
            let result = async {
                let Some(sandbox) = self.service.assignment_sandbox(first).await? else {
                    return Ok::<_, CoordinatorError>(());
                };
                let observed = self.transport.inspect(&sandbox).await?;
                self.authority.register(observed.node_id, &observed.worker).await?;
                for assignment in &group {
                    if let Err(error) = self.service.renew_assignment(
                        &assignment.id, assignment.project_id, assignment.sandbox_id, observed.clone(),
                    ).await {
                        tracing::warn!(assignment_id=%assignment.id, %error, "Service access renewal unavailable");
                    }
                }
                Ok(())
            }.await;
            if let Err(error) = result {
                tracing::warn!(sandbox_id=first.sandbox_id, %error, "Service access renewal unavailable; existing short leases fail closed");
            }
        }).await;
        Ok(count)
    }

    pub(crate) async fn wait_for_work(&self) {
        tokio::select! {
            _ = self.wake.notified() => {},
            _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {},
        }
    }

    fn admit(&self, id: &str) -> Result<tokio::sync::SemaphorePermit<'_>, CoordinatorError> {
        self.admission
            .try_acquire()
            .map_err(|_| CoordinatorError::Invalid {
                sandbox_id: id.into(),
                operation: "schedule authorization",
                reason: "two operations are already in progress; retry later",
            })
    }

    async fn deliver(&self, claim: &DeliveryClaim) -> Result<DispatchOutcome, CoordinatorError> {
        let command = claim.command();
        if command.operation == DeliveryOperation::Retire {
            self.transport
                .retire(&StoredTarget::from_command(command)?)
                .await?;
            self.queue.complete(claim).await?;
            return Ok(DispatchOutcome::Retired);
        }
        let runtime = runtimes::Entity::find_by_id(command.runtime_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| database(command.runtime_id, "read pending incarnation", e))?;
        if runtime.as_ref().is_none_or(|row| {
            row.closed_at.is_some() || row.desired_version > command.desired_version
        }) {
            self.queue.supersede(claim).await?;
            return Ok(DispatchOutcome::Superseded);
        }
        let boot_id = command
            .worker_boot_id
            .ok_or_else(|| CoordinatorError::Invalid {
                sandbox_id: command.runtime_id.to_string(),
                operation: "resolve pending authority",
                reason: "legacy saved intention has no worker authority",
            })?;
        // A reclaimed command may have reached the worker before its ACK was
        // persisted. Allocate a fresh binding version before any further I/O.
        if command.attempts > 1 {
            let tx = self
                .db
                .begin()
                .await
                .map_err(|e| database(command.runtime_id, "begin delivery recovery", e))?;
            DeliveryQueue::advance(
                &tx,
                command.runtime_id,
                command.desired_version,
                DeliveryOperation::Reconcile,
            )
            .await?;
            tx.commit()
                .await
                .map_err(|e| database(command.runtime_id, "commit delivery recovery", e))?;
            self.queue.supersede(claim).await?;
            return Ok(DispatchOutcome::Superseded);
        }
        // Planning/signing stays limited to two operations. No planning or
        // consent permit survives the worker RPC; four issued policies can be
        // in flight (bounded protocol payloads), with only two slow RPCs.
        let planning = self
            .planning
            .acquire()
            .await
            .map_err(|_| CoordinatorError::Invalid {
                sandbox_id: command.runtime_id.to_string(),
                operation: "plan delivery",
                reason: "policy planning admission is closed",
            })?;
        if let Some(batch) = self
            .batches
            .take(boot_id, command.runtime_id, command.desired_version)
        {
            if self.batch_is_current(&batch).await? {
                drop(planning);
                return self.apply_batch_binding(claim, batch, true).await;
            }
        }
        let mut plan = self.planner.prepare(boot_id).await?;
        if !plan.retire.is_empty() {
            for retired in &plan.retire {
                self.retire_runtime(retired.id, retired.desired_version, "authorization_ended")
                    .await?;
            }
            if plan.retire.iter().any(|row| row.id == command.runtime_id) {
                self.queue.supersede(claim).await?;
                return Ok(DispatchOutcome::Superseded);
            }
            // Retirement advances the authority revision. Never sign the old
            // read after writing it; repeat once, with the same bounded scope.
            plan = self.planner.prepare(boot_id).await?;
        }
        let selected = plan
            .bindings
            .iter()
            .find(|binding| binding.plan.target().incarnation_id == command.runtime_id);
        if !plan.retire.is_empty()
            || selected.is_none_or(|binding| {
                i64::try_from(binding.plan.version()).ok() != Some(command.desired_version)
            })
        {
            return Err(CoordinatorError::Invalid {
                sandbox_id: command.runtime_id.to_string(),
                operation: "deliver current policy",
                reason: "authorization changed during planning; retry with the current version",
            });
        }
        let revision = plan.expected_revision;
        let issued = self
            .authority
            .issue(plan.node_id, &plan.worker, revision, plan.bindings)
            .await?;
        drop(planning);
        self.apply_batch_binding(
            claim,
            batches::RenewalBatch::new(boot_id, revision, issued),
            false,
        )
        .await
    }

    async fn apply_batch_binding(
        &self,
        claim: &DeliveryClaim,
        mut batch: batches::RenewalBatch,
        reference: bool,
    ) -> Result<DispatchOutcome, CoordinatorError> {
        let command = claim.command();
        let target = StoredTarget::from_command(command)?;
        // Delivery failures do not revoke consent. A retry allocates a fresh
        // version and rechecks authorization; the signed lease fails closed
        // meanwhile. Only observed authorization changes can retire a runtime.
        self.transport
            .apply(
                &target,
                batch.issued.snapshot_version(),
                batch
                    .issued
                    .binding_delivery(command.runtime_id, reference)?,
            )
            .await?;
        let revision = batch.revision;
        let deadline = batch.issued.lease_expires_at();
        let grant_ids = batch.issued.grant_ids(command.runtime_id).to_vec();
        batch.delivered.insert(command.runtime_id);
        // Publish after the exact worker ACK, before receipt persistence releases
        // the worker claim. Another dispatcher can then reuse this same snapshot.
        // A lost receipt cannot extend its lease; retries still advance versions.
        self.batches.put(batch);
        self.queue
            .complete_applied(claim, revision, deadline, &grant_ids)
            .await?;
        Ok(DispatchOutcome::Applied)
    }

    async fn retire_runtime(
        &self,
        id: Uuid,
        expected_version: i64,
        reason: &'static str,
    ) -> Result<(), CoordinatorError> {
        let tx = self
            .db
            .begin()
            .await
            .map_err(|e| database(id, "begin retirement", e))?;
        let runtime = runtimes::Entity::find_by_id(id)
            .lock_exclusive()
            .one(&tx)
            .await
            .map_err(|e| database(id, "lock retirement", e))?;
        let Some(runtime) = runtime.filter(|row| row.closed_at.is_none()) else {
            return Ok(());
        };
        if expected_version != runtime.desired_version {
            return Err(DeliveryError::RuntimeChanged {
                runtime_id: id,
                version: runtime.desired_version,
            }
            .into());
        }
        DeliveryQueue::advance(&tx, id, runtime.desired_version, DeliveryOperation::Retire).await?;
        tx.commit()
            .await
            .map_err(|e| database(id, "commit retirement", e))?;
        self.wake.notify_waiters();
        if let Err(error) = self
            .audit
            .create_audit_log(&RetirementAudit {
                runtime_id: id,
                reason,
            })
            .await
        {
            tracing::warn!(runtime_id=%id, %error, "Cannot audit service-access retirement");
        }
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct RetirementAudit {
    runtime_id: Uuid,
    reason: &'static str,
}
impl AuditOperation for RetirementAudit {
    fn operation_type(&self) -> String {
        "RETIRE_SANDBOX_ACCESS_AUTHORIZATION".into()
    }
    fn user_id(&self) -> Option<i32> {
        None
    }
    fn ip_address(&self) -> Option<String> {
        None
    }
    fn user_agent(&self) -> &str {
        "temps-service-access"
    }
    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

#[cfg(test)]
pub(crate) mod tests;
