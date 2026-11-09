// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Durable control-plane delivery, not a worker readiness signal. Claims are
//! bounded, leased and fenced. Callers must authenticate the worker and verify
//! its enforcement receipt before completing a command. No credentials are
//! persisted in this queue, and no database operation runs on the proxy path.
mod schedule;
use std::sync::Arc;

use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, DatabaseTransaction, DbErr, EntityTrait,
    Statement, TransactionTrait,
};
use temps_entities::sandbox_access_delivery::{self as delivery, DeliveryOperation};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum DeliveryError {
    #[error("Invalid sandbox access delivery {operation}: {reason}")]
    Invalid {
        operation: &'static str,
        reason: &'static str,
    },
    #[error("Cannot advance sandbox access runtime {runtime_id} from version {version}: runtime is missing, closed or changed")]
    RuntimeChanged { runtime_id: Uuid, version: i64 },
    #[error(
        "Cannot complete sandbox access command {command_id}: delivery claim expired or changed"
    )]
    StaleClaim { command_id: i64 },
    #[error("Cannot {operation} sandbox access delivery for {subject}: database failed")]
    Database {
        operation: &'static str,
        subject: String,
        #[source]
        source: DbErr,
    },
}

fn database(operation: &'static str, subject: impl ToString, source: DbErr) -> DeliveryError {
    DeliveryError::Database {
        operation,
        subject: subject.to_string(),
        source,
    }
}

/// Only constructed after claiming a command from durable storage. Its fields
/// cannot be replaced by worker-provided identifiers in a receipt.
#[derive(Debug)]
pub struct DeliveryClaim {
    command: delivery::Model,
    claim_id: Uuid,
}

impl DeliveryClaim {
    pub fn command(&self) -> &delivery::Model {
        &self.command
    }
}

pub struct DeliveryQueue {
    db: Arc<DatabaseConnection>,
}

impl DeliveryQueue {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Must share the transaction that changes grant intent. CAS prevents a
    /// stale operation from replacing a newer policy; rollback also removes the
    /// queued command. Retiring closes the incarnation to all later grants.
    pub async fn advance(
        txn: &DatabaseTransaction,
        runtime_id: Uuid,
        expected_version: i64,
        operation: DeliveryOperation,
    ) -> Result<i64, DeliveryError> {
        if runtime_id.is_nil() || expected_version < 0 || expected_version == i64::MAX {
            return Err(DeliveryError::Invalid {
                operation: "advance runtime",
                reason: "runtime ID must be non-nil and version must allow an increment",
            });
        }
        let retire = operation == DeliveryOperation::Retire;
        let row = txn.query_one(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "WITH changed AS (UPDATE sandbox_access_runtimes SET desired_version=desired_version+1, updated_at=now(), closed_at=CASE WHEN $3 THEN now() ELSE closed_at END WHERE id=$1 AND desired_version=$2 AND closed_at IS NULL RETURNING id,node_id,desired_version,authority_id,provider_identity,provider_backend,provider_name) INSERT INTO sandbox_access_delivery (runtime_id,node_id,desired_version,operation,worker_boot_id,provider_identity,provider_backend,provider_name) SELECT id,node_id,desired_version,CASE WHEN $3 THEN 'retire' ELSE 'reconcile' END,authority_id,CASE WHEN authority_id IS NOT NULL THEN provider_identity ELSE NULL END,provider_backend,provider_name FROM changed RETURNING desired_version",
            [runtime_id.into(),expected_version.into(),retire.into()])).await
            .map_err(|e| database("advance runtime", runtime_id, e))?
            .ok_or(DeliveryError::RuntimeChanged { runtime_id, version: expected_version })?;
        row.try_get("", "desired_version")
            .map_err(|e| database("decode advanced version", runtime_id, e))
    }

    /// At most 32 commands, with a fixed 30-second lease. A crashed dispatcher
    /// needs no repair job: the next claim reclaims its expired work. Claims are
    /// safe across concurrent dispatchers through SKIP LOCKED. The DB clock is
    /// authoritative; a worker cannot extend a claim or grant deadline.
    pub async fn claim(&self, limit: u32) -> Result<Vec<DeliveryClaim>, DeliveryError> {
        if !(1..=32).contains(&limit) {
            return Err(DeliveryError::Invalid {
                operation: "claim commands",
                reason: "batch size must be between 1 and 32",
            });
        }
        let claim_id = Uuid::new_v4();
        let rows = delivery::Entity::find().from_raw_sql(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "WITH next AS (SELECT id FROM sandbox_access_delivery WHERE completed_at IS NULL AND available_at<=now() AND (claimed_until IS NULL OR claimed_until<=now()) ORDER BY (operation='retire') DESC,available_at,id LIMIT $1 FOR UPDATE SKIP LOCKED) UPDATE sandbox_access_delivery AS d SET claim_id=$2, claimed_until=now()+interval '30 seconds', attempts=LEAST(d.attempts,2147483646)+1 FROM next WHERE d.id=next.id RETURNING d.*",
            [i64::from(limit).into(),claim_id.into()])).all(self.db.as_ref()).await
            .map_err(|e| database("claim commands", "pending queue", e))?;
        Ok(rows
            .into_iter()
            .map(|command| DeliveryClaim { command, claim_id })
            .collect())
    }

    /// Persist an authenticated enforcement receipt. A delayed receipt can
    /// complete its own command but cannot advance a newer desired version.
    /// Call only after the assigned worker confirms installation or a cut; an
    /// RPC delivery/queue acknowledgement is not sufficient.
    pub async fn complete(&self, claim: &DeliveryClaim) -> Result<(), DeliveryError> {
        self.complete_receipt(claim, None, None).await
    }

    /// Records the signed lease deadline only after the transport has verified
    /// an exact binding/snapshot ACK. Never derive this from the receive time.
    pub async fn complete_applied(
        &self,
        claim: &DeliveryClaim,
        expected_revision: i64,
        lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), DeliveryError> {
        let now = chrono::Utc::now();
        if claim.command.operation != DeliveryOperation::Reconcile
            || expected_revision < 0
            || lease_expires_at <= now
            || lease_expires_at
                > now
                    + chrono::Duration::seconds(i64::from(
                        temps_service_access_runtime::MAX_POLICY_LEASE_SECONDS,
                    ))
        {
            return Err(DeliveryError::Invalid {
                operation: "record applied lease",
                reason: "a reconcile receipt requires a current signed lease of at most 30 seconds",
            });
        }
        self.complete_receipt(claim, Some(lease_expires_at), Some(expected_revision))
            .await
    }

    async fn complete_receipt(
        &self,
        claim: &DeliveryClaim,
        lease_expires_at: Option<chrono::DateTime<chrono::Utc>>,
        expected_revision: Option<i64>,
    ) -> Result<(), DeliveryError> {
        let command = &claim.command;
        let txn = self
            .db
            .begin()
            .await
            .map_err(|e| database("begin receipt", command.id, e))?;
        // Match mutation/deletion lock order: runtime first, then command.
        // The runtime may already have been deleted; its tombstone still needs
        // delivery, so absence does not make a retirement receipt invalid.
        txn.query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id FROM sandbox_access_runtimes WHERE id=$1 FOR UPDATE",
            [command.runtime_id.into()],
        ))
        .await
        .map_err(|e| database("lock receipt runtime", command.runtime_id, e))?;
        let changed = txn.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "UPDATE sandbox_access_delivery SET completed_at=now(),claim_id=NULL,claimed_until=NULL WHERE id=$1 AND claim_id=$2 AND completed_at IS NULL AND claimed_until>clock_timestamp()",
            [command.id.into(),claim.claim_id.into()])).await
            .map_err(|e| database("complete command", command.id, e))?;
        if changed.rows_affected() != 1 {
            return Err(DeliveryError::StaleClaim {
                command_id: command.id,
            });
        }
        let applied = txn.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "UPDATE sandbox_access_runtimes SET applied_version=$2,lease_expires_at=$5,updated_at=now() WHERE id=$1 AND desired_version=$2 AND node_id IS NOT DISTINCT FROM $3 AND (($4 AND closed_at IS NOT NULL) OR (NOT $4 AND closed_at IS NULL)) AND ($6::bigint IS NULL OR ($5>clock_timestamp() AND EXISTS (SELECT 1 FROM sandbox_access_authorities a WHERE a.id=sandbox_access_runtimes.authority_id AND a.closed_at IS NULL AND a.desired_revision=$6)))",
            [command.runtime_id.into(),command.desired_version.into(),command.node_id.into(),(command.operation==DeliveryOperation::Retire).into(),lease_expires_at.into(),expected_revision.into()])).await
            .map_err(|e| database("record applied version", command.runtime_id, e))?;
        if expected_revision.is_some() && applied.rows_affected() != 1 {
            return Err(DeliveryError::RuntimeChanged {
                runtime_id: command.runtime_id,
                version: command.desired_version,
            });
        }
        txn.commit()
            .await
            .map_err(|e| database("commit receipt", command.id, e))?;
        tracing::debug!(command_id=command.id,runtime_id=%command.runtime_id,version=command.desired_version,"Sandbox service access enforcement receipt recorded");
        Ok(())
    }

    /// An obsolete reconcile needs no delivery. This NEVER records enforcement
    /// or skips a retirement: its exact old target must still be cut on worker.
    pub async fn supersede(&self, claim: &DeliveryClaim) -> Result<(), DeliveryError> {
        if claim.command.operation != DeliveryOperation::Reconcile {
            return Err(DeliveryError::Invalid {
                operation: "supersede command",
                reason: "retirement requires a worker cut receipt",
            });
        }
        let command = &claim.command;
        let tx = self
            .db
            .begin()
            .await
            .map_err(|e| database("begin supersession", command.id, e))?;
        let row = tx.query_one(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "SELECT desired_version,closed_at FROM sandbox_access_runtimes WHERE id=$1 FOR UPDATE", [command.runtime_id.into()]))
            .await.map_err(|e| database("lock superseded runtime", command.runtime_id, e))?;
        if let Some(row) = row {
            let version: i64 = row
                .try_get("", "desired_version")
                .map_err(|e| database("read superseded version", command.runtime_id, e))?;
            let closed: Option<chrono::DateTime<chrono::Utc>> = row
                .try_get("", "closed_at")
                .map_err(|e| database("read superseded state", command.runtime_id, e))?;
            if version <= command.desired_version && closed.is_none() {
                return Err(DeliveryError::RuntimeChanged {
                    runtime_id: command.runtime_id,
                    version: command.desired_version,
                });
            }
        }
        let changed = tx.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "UPDATE sandbox_access_delivery SET completed_at=now(),claim_id=NULL,claimed_until=NULL WHERE id=$1 AND claim_id=$2 AND completed_at IS NULL AND claimed_until>clock_timestamp()",
            [command.id.into(),claim.claim_id.into()])).await.map_err(|e| database("supersede command", command.id, e))?;
        if changed.rows_affected() != 1 {
            return Err(DeliveryError::StaleClaim {
                command_id: command.id,
            });
        }
        tx.commit()
            .await
            .map_err(|e| database("commit supersession", command.id, e))
    }

    /// Schedule a transient delivery failure with exponential backoff, capped
    /// at five minutes. A lease lost to another dispatcher cannot be released.
    /// Permanent policy failures must instead retire the affected incarnation.
    pub async fn retry(&self, claim: &DeliveryClaim) -> Result<(), DeliveryError> {
        let delay = (1_i64 << claim.command.attempts.clamp(1, 9)).min(300);
        let changed = self.db.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "UPDATE sandbox_access_delivery SET claim_id=NULL,claimed_until=NULL,available_at=now()+($3::bigint*interval '1 second') WHERE id=$1 AND claim_id=$2 AND completed_at IS NULL AND claimed_until>clock_timestamp()",
            [claim.command.id.into(),claim.claim_id.into(),delay.into()])).await
            .map_err(|e| database("reschedule command", claim.command.id, e))?;
        if changed.rows_affected() != 1 {
            return Err(DeliveryError::StaleClaim {
                command_id: claim.command.id,
            });
        }
        tracing::warn!(command_id=claim.command.id,runtime_id=%claim.command.runtime_id,retry_after_seconds=delay,"Sandbox service access delivery scheduled for retry");
        Ok(())
    }
}

#[cfg(test)]
mod tests;
