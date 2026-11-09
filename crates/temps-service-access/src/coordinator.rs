// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Control-plane orchestration for explicit operator approvals.
//! No queued intention is reported as connected; only exact worker ACKs write
//! an applied version and the original signed policy lease deadline.
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
    wake: Notify,
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
            wake: Notify::new(),
        }
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

    /// One bounded command. The caller schedules subsequent ticks; there is no
    /// unbounded background task or network operation inside a DB transaction.
    pub async fn dispatch_one(&self) -> Result<Option<DispatchOutcome>, CoordinatorError> {
        let _permit = self.admit("delivery queue")?;
        let Some(claim) = self.queue.claim(1).await?.pop() else {
            return Ok(None);
        };
        match self.deliver(&claim).await {
            Ok(result) => Ok(Some(result)),
            Err(error) => {
                if let Err(retry_error) = self.queue.retry(&claim).await {
                    tracing::warn!(command_id=claim.command().id, %retry_error, "Cannot reschedule service-access delivery");
                }
                Err(error)
            }
        }
    }

    /// Bounded maintenance tick. Authorization changes wake their worker in the
    /// database; periodic rechecks renew only still-authorized short leases.
    pub async fn maintain(&self) -> Result<u64, CoordinatorError> {
        let queued = self.queue.schedule_due(16).await?;
        self.queue.prune_completed().await?;
        if queued > 0 {
            self.wake.notify_waiters();
        }
        Ok(queued)
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
        let mut plan = self.planner.prepare(boot_id).await?;
        if !plan.retire.is_empty() {
            for retired in &plan.retire {
                self.retire_runtime(
                    retired.id,
                    Some(retired.desired_version),
                    "authorization_ended",
                )
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
        let grant_ids: Vec<String> = selected
            .map(|binding| {
                binding
                    .plan
                    .grants()
                    .iter()
                    .map(|grant| grant.grant_id().to_string())
                    .collect()
            })
            .unwrap_or_default();
        let revision = plan.expected_revision;
        let issued = self
            .authority
            .issue(plan.node_id, &plan.worker, revision, plan.bindings)
            .await?;
        let target = StoredTarget::from_command(command)?;
        if let Err(error) = self
            .transport
            .apply(
                &target,
                issued.snapshot_version(),
                issued.delivery(command.runtime_id)?,
            )
            .await
        {
            if !matches!(error, TransportError::Busy { .. }) {
                // A timed-out/failed apply may have partially installed policy.
                // Invalidate all consent for this incarnation and durably cut
                // it. Reconnecting requires a new host incarnation and consent;
                // retry must never infer success from a queue or socket write.
                self.retire_runtime(command.runtime_id, None, "delivery_unconfirmed")
                    .await?;
            }
            return Err(error.into());
        }
        self.queue
            .complete_applied(claim, revision, issued.lease_expires_at(), &grant_ids)
            .await?;
        Ok(DispatchOutcome::Applied)
    }

    async fn retire_runtime(
        &self,
        id: Uuid,
        expected_version: Option<i64>,
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
        if expected_version.is_some_and(|version| version != runtime.desired_version) {
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
