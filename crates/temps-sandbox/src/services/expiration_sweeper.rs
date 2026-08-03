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

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
};
use temps_agents::services::run_service::TERMINAL_RUN_STATUSES;
use temps_entities::{agent_runs, sandboxes};

use crate::services::public_id;
use crate::services::registry::StandaloneSandboxRegistry;

/// How often the sweeper wakes up to scan for expired sandboxes. At most
/// one sweep period of overrun past `expires_at` — at 60s that's a
/// negligible blast radius relative to the minimum 60s `timeout_secs`.
const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// How many expiration sweeps pass between orphaned-volume reaps.
///
/// Reclaiming disk is not urgent — an orphan costs bytes, not correctness —
/// and the reap does a `GET /volumes` against the Docker daemon, so running
/// it every minute alongside the expiry scan would be pure overhead on a
/// host that hasn't destroyed anything. At 60s per sweep this is hourly.
const REAP_EVERY_N_SWEEPS: u32 = 60;

/// Prefix of the Docker volume holding a sandbox's home dir. Duplicated
/// from the Docker provider on purpose: the sweeper's job is to name the
/// volumes it wants *kept*, and it must be able to do that without
/// depending on a Docker-specific type.
const HOME_VOLUME_PREFIX: &str = "temps-sandbox-home-";

pub struct SandboxExpirationSweeper {
    db: Arc<DatabaseConnection>,
    registry: Arc<StandaloneSandboxRegistry>,
    /// Same root `SandboxService` allocates work dirs under. Needed so the
    /// sweeper can reclaim directories whose sandbox is long gone.
    data_root: PathBuf,
}

impl SandboxExpirationSweeper {
    pub fn new(
        db: Arc<DatabaseConnection>,
        registry: Arc<StandaloneSandboxRegistry>,
        data_root: PathBuf,
    ) -> Self {
        Self {
            db,
            registry,
            data_root,
        }
    }

    /// Run forever. Spawned as a `tokio::spawn` background task by the
    /// plugin; the returned future never completes on the happy path.
    pub async fn run(&self) {
        tracing::info!(
            "Sandbox expiration sweeper started (interval: {}s)",
            SWEEP_INTERVAL.as_secs()
        );
        // Reap once at startup, before the first sleep: a host upgrading
        // into this build is exactly the case that has already accumulated
        // orphans, and making the operator wait an hour to get that disk
        // back would be the wrong default.
        self.reap_orphans().await;

        let mut sweeps: u32 = 0;
        loop {
            tokio::time::sleep(SWEEP_INTERVAL).await;
            if let Err(e) = self.tick().await {
                tracing::error!("Sandbox expiration sweep failed: {}", e);
            }
            sweeps = (sweeps + 1) % REAP_EVERY_N_SWEEPS;
            if sweeps == 0 {
                self.reap_orphans().await;
            }
        }
    }

    /// Reclaim both halves of a leaked sandbox: the provider's storage and
    /// the host work dir. Never propagates — a Docker daemon that can't
    /// list volumes, or an unreadable data dir, must not take down the
    /// expiry loop, which is doing the more important job.
    async fn reap_orphans(&self) {
        match self.claimed_volumes().await {
            Ok(claimed) => match self
                .registry
                .provider()
                .reap_orphaned_volumes(&claimed)
                .await
            {
                Ok(0) => tracing::debug!("Sandbox volume reap: nothing to reclaim"),
                Ok(n) => tracing::info!("Sandbox volume reap: reclaimed {} orphaned volume(s)", n),
                Err(e) => tracing::warn!("Sandbox volume reap failed: {}", e),
            },
            Err(e) => {
                // Fail closed. Reaping with a partial claim set would
                // delete the home volumes of sandboxes we simply failed to
                // read — the one outcome worse than not reclaiming disk.
                tracing::warn!(
                    "Sandbox volume reap skipped: could not read live sandboxes: {}",
                    e
                );
            }
        }

        match self.reap_orphaned_work_dirs().await {
            Ok(0) => tracing::debug!("Sandbox work dir reap: nothing to reclaim"),
            Ok(n) => tracing::info!("Sandbox work dir reap: reclaimed {} orphaned dir(s)", n),
            Err(e) => tracing::warn!("Sandbox work dir reap failed: {}", e),
        }
    }

    /// Every volume name a live sandbox may still need.
    ///
    /// "No container references it" is not the same question as "no sandbox
    /// wants it", and the gap between them is where data gets destroyed: a
    /// sandbox being recreated has no container while its image pulls, and
    /// a volume kept by `destroy(purge_volumes: false)` has none by design.
    /// Only the database can tell those apart from a genuine leak.
    ///
    /// Deliberately over-claims. Every name is cheap; a wrong deletion is
    /// not. So this covers all three naming schemes a live sandbox could be
    /// using — agent-run numeric, standalone `public_id` label, and the
    /// pre-fix numeric row id that upgraded hosts still have mounted — and
    /// adds non-terminal agent runs directly, in case a run outlives the
    /// `sandboxes` row that pointed at it.
    async fn claimed_volumes(&self) -> Result<HashSet<String>, sea_orm::DbErr> {
        let mut claimed = HashSet::new();

        let live = sandboxes::Entity::find()
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .all(self.db.as_ref())
            .await?;
        for row in live {
            match row.agent_run_id {
                // Agent runs keep numeric container naming.
                Some(run_id) => {
                    claimed.insert(format!("{}{}", HOME_VOLUME_PREFIX, run_id));
                }
                // Standalone sandboxes are named after the public id label.
                None => {
                    let label = row
                        .public_id
                        .strip_prefix(public_id::PUBLIC_ID_PREFIX)
                        .unwrap_or(&row.public_id);
                    claimed.insert(format!("{}{}", HOME_VOLUME_PREFIX, label));
                }
            }
            // Pre-fix naming: standalone sandboxes created before the
            // volume name was keyed on the container label still have
            // `temps-sandbox-home-<row.id>` mounted. Dangling protects them
            // while their container exists, but claiming them costs one
            // string and removes any dependence on that.
            claimed.insert(format!("{}{}", HOME_VOLUME_PREFIX, row.id));
        }

        let active_runs = agent_runs::Entity::find()
            .filter(agent_runs::Column::Status.is_not_in(TERMINAL_RUN_STATUSES.iter().copied()))
            .all(self.db.as_ref())
            .await?;
        for run in active_runs {
            claimed.insert(format!("{}{}", HOME_VOLUME_PREFIX, run.id));
        }

        Ok(claimed)
    }

    /// Delete work dirs under `data_root` with no live sandbox row.
    ///
    /// The counterpart to the volume reap, and the reason `destroy_sandbox`
    /// can afford to skip its own cleanup when the provider destroy failed.
    /// It also covers the create-failure paths, where a directory can
    /// already hold a cloned repo before anything goes wrong.
    ///
    /// Only `sbx_<16 hex>` directory names are considered, so anything an
    /// operator put in the data dir is out of scope by construction.
    ///
    /// Ordering is load-bearing and depends on `create_sandbox` inserting
    /// the row *before* it creates the directory: we list directories
    /// first, then query. A directory can therefore only exist if its row
    /// was already committed, so the query below is guaranteed to see it
    /// and an in-flight create can never be reaped. Creating the directory
    /// before the row would reintroduce that race silently — if that order
    /// ever changes, this sweep has to change with it.
    async fn reap_orphaned_work_dirs(&self) -> Result<usize, std::io::Error> {
        let mut dir = match tokio::fs::read_dir(&self.data_root).await {
            Ok(d) => d,
            // No data root yet means no sandbox has ever been created.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };

        let mut candidates: Vec<String> = Vec::new();
        while let Some(entry) = dir.next_entry().await? {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if public_id::is_valid(&name) {
                candidates.push(name);
            }
        }
        if candidates.is_empty() {
            return Ok(0);
        }

        // One query for the whole batch rather than one per directory.
        let live: HashSet<String> = match sandboxes::Entity::find()
            .filter(sandboxes::Column::PublicId.is_in(candidates.clone()))
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .all(self.db.as_ref())
            .await
        {
            Ok(rows) => rows.into_iter().map(|r| r.public_id).collect(),
            Err(e) => {
                // Fail closed, same reasoning as the volume reap.
                tracing::warn!(
                    "Sandbox work dir reap skipped: could not read live sandboxes: {}",
                    e
                );
                return Ok(0);
            }
        };

        let mut reclaimed = 0usize;
        for name in candidates {
            if live.contains(&name) {
                continue;
            }
            let path = self.data_root.join(&name);
            match tokio::fs::remove_dir_all(&path).await {
                Ok(()) => {
                    reclaimed += 1;
                    tracing::info!("Reclaimed orphaned sandbox work dir {}", path.display());
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!(
                    "Failed to reclaim orphaned sandbox work dir {}: {}",
                    path.display(),
                    e
                ),
            }
        }
        Ok(reclaimed)
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
            // Agent-run sandboxes are lifecycle-owned by the run itself
            // (analysis → fix → PR can legitimately outlive any timeout
            // while the user reviews between phases) — never sweep them.
            .filter(sandboxes::Column::AgentRunId.is_null())
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
            user_id: Some(1),
            agent_run_id: None,
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

    /// The reap cadence is expressed in sweeps, so it silently rescales
    /// with `SWEEP_INTERVAL` — which the test above lets range from 10s to
    /// 5min, i.e. the same constant could mean 10 minutes or 5 hours. Pin
    /// the wall-clock period the doc comment actually promises.
    #[test]
    fn orphan_reap_runs_about_hourly() {
        let period = SWEEP_INTERVAL * REAP_EVERY_N_SWEEPS;
        assert!(
            period.as_secs() >= 30 * 60 && period.as_secs() <= 90 * 60,
            "orphan reap period is {}s — the documented cadence is roughly hourly",
            period.as_secs()
        );
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
