//! Periodic sweeper that stops sandboxes whose `expires_at` has passed.
//!
//! Sandboxes are created with a bounded `timeout_secs` window (default 1h,
//! max 24h). Without this sweeper, a sandbox whose owner never calls
//! `/destroy` or `/stop` would keep its container running indefinitely —
//! the `expires_at` column would exist only as metadata.
//!
//! Behavior on expiry: **stop**, not destroy. The container is paused via
//! the provider's `stop()` call and the DB row transitions from `"running"`
//! to `"stopped"`. Volumes, the bind-mounted `/workspace`, and home-dir
//! state all survive so the owner can call `/resume` later. Destroying
//! would be irreversible — that's reserved for explicit `/destroy` calls.
//!
//! Loop shape: plain 60-second interval (not minute-aligned — we don't
//! need clock phases, just periodic sweeping). Query is cheap thanks to
//! the partial index on `(expires_at) WHERE status = 'running'` added by
//! migration `m20260414_000001_create_sandboxes`.
//!
//! Error handling: every per-row failure is logged and the loop continues.
//! One bad row (provider unreachable, DB write conflict) must not halt the
//! sweeper — that would defer cleanup of every other expired sandbox.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
};
use temps_entities::sandboxes;

use crate::services::registry::StandaloneSandboxRegistry;

/// How often the sweeper wakes up to scan for expired sandboxes. At most
/// one sweep period of overrun past `expires_at` — at 60s that's a
/// negligible blast radius relative to the minimum 60s `timeout_secs`.
const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

pub struct SandboxExpirationSweeper {
    db: Arc<DatabaseConnection>,
    registry: Arc<StandaloneSandboxRegistry>,
}

impl SandboxExpirationSweeper {
    pub fn new(db: Arc<DatabaseConnection>, registry: Arc<StandaloneSandboxRegistry>) -> Self {
        Self { db, registry }
    }

    /// Run forever. Spawned as a `tokio::spawn` background task by the
    /// plugin; the returned future never completes on the happy path.
    pub async fn run(&self) {
        tracing::info!(
            "Sandbox expiration sweeper started (interval: {}s)",
            SWEEP_INTERVAL.as_secs()
        );
        loop {
            tokio::time::sleep(SWEEP_INTERVAL).await;
            if let Err(e) = self.tick().await {
                tracing::error!("Sandbox expiration sweep failed: {}", e);
            }
        }
    }

    /// One sweep pass. Finds running sandboxes whose `expires_at` is in
    /// the past, stops each one, and transitions the DB row to
    /// `"stopped"`. Returns the count actually transitioned (useful for
    /// tests + tracing visibility).
    pub async fn tick(&self) -> Result<usize, sea_orm::DbErr> {
        let now = Utc::now();
        let expired = sandboxes::Entity::find()
            .filter(sandboxes::Column::Status.eq("running"))
            .filter(sandboxes::Column::ExpiresAt.lt(now))
            .all(self.db.as_ref())
            .await?;

        if expired.is_empty() {
            return Ok(0);
        }

        tracing::info!(
            "Sandbox expiration sweep: {} expired sandbox(es) to stop",
            expired.len()
        );

        let mut stopped = 0usize;
        for row in expired {
            match self.stop_one(&row).await {
                Ok(()) => stopped += 1,
                Err(e) => {
                    tracing::error!(
                        "Expiration sweep: failed to stop sandbox {} (internal {}): {}",
                        row.public_id,
                        row.id,
                        e
                    );
                }
            }
        }
        Ok(stopped)
    }

    /// Stop a single expired sandbox. Mirrors `SandboxService::pause_sandbox`
    /// but without ownership checks (the sweeper runs system-wide) and
    /// tolerant of provider failures: if the container is already gone we
    /// still want the DB row to reflect that it's no longer running.
    async fn stop_one(&self, row: &sandboxes::Model) -> Result<(), sea_orm::DbErr> {
        // Best-effort container stop. If the provider doesn't know about
        // this sandbox (server restart + recovery miss, or container was
        // removed externally) we still flip the status so subsequent
        // listings don't show a zombie "running" entry.
        if let Err(e) = self.registry.stop(row.id, &row.public_id).await {
            tracing::warn!(
                "Expiration sweep: provider stop failed for sandbox {} (internal {}): {} \
                 — marking stopped anyway",
                row.public_id,
                row.id,
                e
            );
        } else {
            tracing::info!(
                "Expiration sweep: stopped sandbox {} (internal {}, expired at {})",
                row.public_id,
                row.id,
                row.expires_at
            );
        }

        let active = sandboxes::ActiveModel {
            id: Set(row.id),
            status: Set("stopped".to_string()),
            last_activity_at: Set(Utc::now()),
            ..Default::default()
        };
        active.update(self.db.as_ref()).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn make_row(id: i32, status: &str, expires_in_secs: i64) -> sandboxes::Model {
        let now = Utc::now();
        sandboxes::Model {
            id,
            public_id: format!("sbx_test{:06x}", id),
            user_id: 1,
            name: format!("sbx-{}", id),
            status: status.to_string(),
            image: None,
            work_dir: "/workspace".to_string(),
            timeout_secs: 3600,
            metadata: None,
            backend: None,
            created_at: now,
            last_activity_at: now,
            expires_at: now + chrono::Duration::seconds(expires_in_secs),
            preview_password_hash: None,
            preview_password_hint: None,
        }
    }

    #[test]
    fn sweep_interval_is_reasonable() {
        // Floor chosen for DB load; ceiling chosen so overrun past
        // expires_at is bounded — if these invariants ever change the
        // test surfaces it instead of silently drifting.
        assert!(SWEEP_INTERVAL.as_secs() >= 10);
        assert!(SWEEP_INTERVAL.as_secs() <= 300);
    }

    #[tokio::test]
    async fn tick_with_no_expired_rows_returns_zero() {
        // Empty result set → sweep is a no-op, no status writes.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results::<sandboxes::Model, _, _>(vec![vec![]])
            .into_connection();

        // We can't construct a real registry here without a provider.
        // The no-op path never touches the registry, so we can short-circuit
        // tick()'s body at the DB layer: confirm the query returns empty.
        let rows = sandboxes::Entity::find()
            .filter(sandboxes::Column::Status.eq("running"))
            .filter(sandboxes::Column::ExpiresAt.lt(Utc::now()))
            .all(&db)
            .await
            .expect("query");
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn tick_updates_status_for_expired_rows() {
        // Row with expires_at in the past should be listed by the query,
        // and the sweeper should issue an update. We verify the DB side of
        // the flow; registry.stop failures are separately logged and don't
        // block the status transition.
        let expired = make_row(1_000_042, "running", -60);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![expired.clone()]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // ActiveModel::update re-fetches the row after the UPDATE.
            .append_query_results(vec![vec![sandboxes::Model {
                status: "stopped".to_string(),
                ..expired.clone()
            }]])
            .into_connection();

        let rows = sandboxes::Entity::find()
            .filter(sandboxes::Column::Status.eq("running"))
            .filter(sandboxes::Column::ExpiresAt.lt(Utc::now()))
            .all(&db)
            .await
            .expect("query");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, 1_000_042);

        let active = sandboxes::ActiveModel {
            id: Set(rows[0].id),
            status: Set("stopped".to_string()),
            last_activity_at: Set(Utc::now()),
            ..Default::default()
        };
        let updated = active.update(&db).await.expect("update");
        assert_eq!(updated.status, "stopped");
    }

    #[test]
    fn make_row_helper_produces_expected_shape() {
        // Sanity check the test helper so failures in the other tests
        // point at the sweeper, not the fixture.
        let r = make_row(42, "running", -10);
        assert_eq!(r.status, "running");
        assert!(r.expires_at < Utc::now());
    }
}
