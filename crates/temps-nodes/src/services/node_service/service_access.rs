// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{NodeError, NodeService};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseTransaction, Statement};

impl NodeService {
    /// Called under the node-removal transaction's node lock, after all live
    /// workloads have been excluded. Foreign keys prevent new registrations or
    /// runtimes from racing removal. Keep runtime-before-authority lock order,
    /// matching consent and the renewal scheduler, without loading IDs into RAM.
    pub(super) async fn remove_service_access_on(
        tx: &DatabaseTransaction,
        node_id: i32,
    ) -> Result<(), NodeError> {
        for sql in [
            "SELECT id FROM sandbox_access_runtimes WHERE node_id=$1 ORDER BY id FOR UPDATE",
            // Destroyed sandboxes can retain both open and closed incarnations.
            // Their authorizations cascade; the delete trigger emits retirement
            // tombstones and advances the authority before its keys are removed.
            "DELETE FROM sandbox_access_runtimes WHERE node_id=$1",
            "DELETE FROM sandbox_access_authorities WHERE node_id=$1",
            // The node is no longer managed: retries cannot reach its worker.
            // Include tombstones left by earlier project deletion and invalidate
            // in-flight claims. Retain target/history rather than re-home NULL
            // node IDs to the control plane. This is not a worker revocation ACK;
            // any already-issued policy remains bounded by its signed lease.
            "UPDATE sandbox_access_delivery SET completed_at=now(),claim_id=NULL,claimed_until=NULL WHERE node_id=$1 AND completed_at IS NULL",
        ] {
            tx.execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                [node_id.into()],
            ))
            .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
