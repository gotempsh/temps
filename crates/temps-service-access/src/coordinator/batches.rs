// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Reuse a worker sweep's already-signed policy, never renew its lease from cache.
//! Two entries bound retained policy/session data to about 16 MiB. Taking an
//! entry transfers ownership: no mutex or DB connection spans worker I/O. Cache
//! loss, process restart, or competing dispatchers simply require fresh planning.
use super::*;
use crate::authority::IssuedPolicy;
use sea_orm::{DatabaseBackend, FromQueryResult, Statement};
use std::{
    collections::{HashSet, VecDeque},
    sync::Mutex,
    time::{Duration, Instant},
};

pub(super) struct RenewalBatch {
    pub boot_id: Uuid,
    pub revision: i64,
    pub issued: IssuedPolicy,
    pub delivered: HashSet<Uuid>,
    created: Instant,
}
impl RenewalBatch {
    pub fn new(boot_id: Uuid, revision: i64, issued: IssuedPolicy) -> Self {
        Self {
            boot_id,
            revision,
            issued,
            delivered: HashSet::new(),
            created: Instant::now(),
        }
    }
    fn live(&self) -> bool {
        self.created.elapsed() < Duration::from_secs(10)
            && self.issued.lease_expires_at() > chrono::Utc::now() + chrono::Duration::seconds(5)
            && self.delivered.len() < self.issued.binding_count()
    }
}
#[derive(Default)]
pub(super) struct RenewalBatches(Mutex<VecDeque<RenewalBatch>>);
impl RenewalBatches {
    pub fn take(&self, boot: Uuid, runtime: Uuid, version: i64) -> Option<RenewalBatch> {
        let mut entries = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let index = entries.iter().position(|b| b.boot_id == boot)?;
        entries.remove(index).filter(|b| {
            b.live()
                && !b.delivered.contains(&runtime)
                && b.issued.binding_version(runtime) == u64::try_from(version).ok()
        })
    }
    pub fn put(&self, batch: RenewalBatch) {
        let mut entries = self.0.lock().unwrap_or_else(|e| e.into_inner());
        entries.retain(|b| b.boot_id != batch.boot_id && b.live());
        if batch.live() {
            while entries.len() >= 2 {
                entries.pop_front();
            }
            entries.push_back(batch);
        }
    }
}
#[derive(FromQueryResult)]
struct Current {
    current: bool,
}
impl ServiceAccessCoordinator {
    pub(super) async fn batch_is_current(
        &self,
        batch: &RenewalBatch,
    ) -> Result<bool, CoordinatorError> {
        let version = i64::try_from(batch.issued.snapshot_version()).map_err(|_| {
            CoordinatorError::Invalid {
                sandbox_id: batch.boot_id.to_string(),
                operation: "reuse worker policy",
                reason: "snapshot version exceeds database bounds",
            }
        })?;
        let current = Current::find_by_statement(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "SELECT EXISTS(SELECT 1 FROM sandbox_access_authorities WHERE id=$1 AND desired_revision=$2 AND snapshot_version=$3 AND closed_at IS NULL) AS current",
            [batch.boot_id.into(), batch.revision.into(), version.into()])).one(self.db.as_ref()).await
            .map_err(|e| database(batch.boot_id, "fence reused worker policy", e))?;
        Ok(batch.live() && current.is_some_and(|row| row.current))
    }
}
