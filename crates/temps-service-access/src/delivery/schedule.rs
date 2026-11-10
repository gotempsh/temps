// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use sea_orm::{ColumnTrait, FromQueryResult, QueryFilter, QueryOrder, QuerySelect};
use temps_entities::sandbox_access_runtimes as runtimes;

#[derive(FromQueryResult)]
struct DueWorker {
    id: Uuid,
}

impl DeliveryQueue {
    /// Indexed due-worker scan, not a scan of every sandbox. Changes wake the
    /// authority in the same transaction; active workers recheck every 10s for
    /// their 30s leases. Pending versions are retained rather than multiplied.
    pub async fn schedule_due(&self, limit: u32) -> Result<ScheduledBatch, DeliveryError> {
        if !(1..=16).contains(&limit) {
            return Err(DeliveryError::Invalid {
                operation: "schedule due workers",
                reason: "worker batch must contain 1 to 16 entries",
            });
        }
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            self.schedule_due_inner(limit),
        )
        .await
        .map_err(|_| DeliveryError::Invalid {
            operation: "schedule due workers",
            reason: "database scheduling deadline exceeded",
        })?
    }

    async fn schedule_due_inner(&self, limit: u32) -> Result<ScheduledBatch, DeliveryError> {
        let due = DueWorker::find_by_statement(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "SELECT id FROM sandbox_access_authorities WHERE closed_at IS NULL AND next_reconcile_at<=now() ORDER BY next_reconcile_at,id LIMIT $1", [i64::from(limit).into()]))
            .all(self.db.as_ref()).await.map_err(|e| database("find due workers", "reconciliation queue", e))?;
        let processed = due.len();
        let mut scheduled = 0;
        for worker in due {
            scheduled += self.schedule_worker(worker.id).await?;
        }
        Ok(ScheduledBatch {
            processed,
            queued: scheduled,
        })
    }

    async fn schedule_worker(&self, id: Uuid) -> Result<u64, DeliveryError> {
        let tx = self
            .db
            .begin()
            .await
            .map_err(|e| database("begin worker scheduling", id, e))?;
        // Match consent/retirement lock order: runtime(s), then authority.
        // Locking the authority before a runtime would deadlock against a
        // concurrent consent transaction whose trigger updates that authority.
        let runtime_ids = runtimes::Entity::find()
            .filter(runtimes::Column::AuthorityId.eq(id))
            .filter(runtimes::Column::ClosedAt.is_null())
            .select_only()
            .column(runtimes::Column::Id)
            .order_by_asc(runtimes::Column::Id)
            .limit((temps_service_access_runtime::MAX_WORKER_BINDINGS + 1) as u64)
            .lock_exclusive()
            .into_tuple::<Uuid>()
            .all(&tx)
            .await
            .map_err(|e| database("lock worker incarnations", id, e))?;
        // Legacy overflow must still reach the planner, which retires stale or
        // excess incarnations instead of stopping renewal for the whole worker.
        let authority = tx.query_one(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "SELECT id FROM sandbox_access_authorities WHERE id=$1 AND closed_at IS NULL AND next_reconcile_at<=clock_timestamp() FOR UPDATE", [id.into()]))
            .await.map_err(|e| database("lock due worker", id, e))?;
        if authority.is_none() {
            return Ok(0);
        }
        // One statement for all eligible bindings. No per-runtime mutation or
        // service lookup. Already-pending commands compile fresh authorization
        // at delivery time; they need neither another version nor another row.
        let queued = tx.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            r#"WITH changed AS (
                UPDATE sandbox_access_runtimes r SET desired_version=desired_version+1,updated_at=now()
                WHERE authority_id=$1 AND r.id=ANY($2::uuid[]) AND closed_at IS NULL AND desired_version=applied_version AND desired_version<9223372036854775807
                  AND NOT EXISTS (SELECT 1 FROM sandbox_access_delivery d WHERE d.runtime_id=r.id AND d.desired_version=r.desired_version AND d.completed_at IS NULL)
                RETURNING id,node_id,desired_version,authority_id,provider_identity,provider_backend,provider_name
            ) INSERT INTO sandbox_access_delivery (runtime_id,node_id,desired_version,operation,worker_boot_id,provider_identity,provider_backend,provider_name)
                SELECT id,node_id,desired_version,'reconcile',authority_id,provider_identity,provider_backend,provider_name FROM changed"#,
            [id.into(), runtime_ids.into()])).await.map_err(|e| database("queue worker renewal", id, e))?;
        tx.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "UPDATE sandbox_access_authorities SET next_reconcile_at=CASE WHEN EXISTS (SELECT 1 FROM sandbox_access_runtimes r WHERE r.authority_id=$1 AND r.closed_at IS NULL) THEN now()+interval '10 seconds' ELSE NULL END WHERE id=$1", [id.into()]))
            .await.map_err(|e| database("persist next recheck", id, e))?;
        tx.commit()
            .await
            .map_err(|e| database("commit worker scheduling", id, e))?;
        Ok(queued.rows_affected())
    }

    /// Completed outbox rows are diagnostic history, not authorization state.
    /// Keep 15 minutes, remove at most 256 per tick, never discard pending cuts.
    pub async fn prune_completed(&self) -> Result<u64, DeliveryError> {
        let deleted = self.db.execute(Statement::from_string(DatabaseBackend::Postgres,
            "DELETE FROM sandbox_access_delivery WHERE id IN (SELECT id FROM sandbox_access_delivery WHERE completed_at<now()-interval '15 minutes' ORDER BY completed_at,id LIMIT 256 FOR UPDATE SKIP LOCKED)"))
            .await.map_err(|e| database("prune completed commands", "delivery history", e))?;
        Ok(deleted.rows_affected())
    }
}
