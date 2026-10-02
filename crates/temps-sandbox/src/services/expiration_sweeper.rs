// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Periodic sweeper that suspends sandbox compute whose `expires_at` has passed.
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
//! Durable standalone workspaces participate in idle suspension too:
//! "persistent" means their files survive, not that their compute consumes
//! resources forever.
//!
//! Temps-managed application workspaces (identified by the
//! `ai-application:` name prefix) are deliberately excluded. Their compute is
//! part of the application runtime and must remain available until an explicit
//! application pause or stop request changes its lifecycle state. Other
//! workspace-class sandboxes continue to use the normal idle deadline.
//!
//! `expires_at` is an *idle* deadline: `SandboxService::touch` pushes it
//! forward on every exec and filesystem operation, so a sandbox in active
//! use is never swept. Only genuine inactivity reaches this loop.
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
    ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Select,
};
use temps_entities::sandboxes;

use crate::services::registry::StandaloneSandboxRegistry;
use crate::services::row_status::{self, Expect};

/// What one expiry stop did to its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopOutcome {
    /// The row went from `running` to `stopped`.
    Stopped,
    /// The sandbox's worker node did not answer: the row stays `running`
    /// and the next sweep retries.
    NodeUnreachable,
    /// The row was no longer `running` when the stop finished (destroyed,
    /// evicted or paused meanwhile); it is left as it is.
    Superseded,
}

/// How often the sweeper wakes up to scan for expired sandboxes. At most
/// one sweep period of overrun past `expires_at` — at 60s that's a
/// negligible blast radius relative to the minimum 60s `timeout_secs`.
const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// How long the sweep waits for a worker node to stop one sandbox. Past it,
/// the node is treated as unreachable: the sandbox stays `running` and the
/// next sweep retries.
const WORKER_STOP_DEADLINE: Duration = Duration::from_secs(30);

/// Application workspace compute is lifecycle-managed through the application
/// API. The generic idle sweeper must not override that desired state.
const APPLICATION_WORKSPACE_NAME_PATTERN: &str = "ai-application:%";

fn expired_sandboxes(now: chrono::DateTime<Utc>) -> Select<sandboxes::Entity> {
    sandboxes::Entity::find()
        .filter(sandboxes::Column::Status.eq("running"))
        .filter(sandboxes::Column::ExpiresAt.lt(now))
        // Agent-run sandboxes are lifecycle-owned by the run itself
        // (analysis → fix → PR can legitimately outlive any timeout
        // while the user reviews between phases) — never sweep them.
        .filter(sandboxes::Column::AgentRunId.is_null())
        // Application workspaces are an always-on application resource.
        // Explicit application pause/stop controls remain responsible for
        // changing their state.
        .filter(sandboxes::Column::Name.not_like(APPLICATION_WORKSPACE_NAME_PATTERN))
}

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
        let expired = expired_sandboxes(now).all(self.db.as_ref()).await?;

        if expired.is_empty() {
            return Ok(0);
        }

        tracing::info!(
            "Sandbox expiration sweep: {} expired sandbox(es) to stop",
            expired.len()
        );

        Ok(sweep(expired, |row| async move { self.stop_one(&row).await }).await)
    }

    /// Stop a single expired sandbox. Mirrors `SandboxService::pause_sandbox`
    /// but without ownership checks (the sweeper runs system-wide) and
    /// tolerant of provider failures: if the container is already gone we
    /// still want the DB row to reflect that it's no longer running.
    /// The row is only moved to `stopped` if it is still `running`: a destroy
    /// or node eviction that landed while the stop waited on the provider
    /// wins, and the row is not resurrected.
    async fn stop_one(&self, row: &sandboxes::Model) -> Result<StopOutcome, sea_orm::DbErr> {
        // Best-effort container stop. If the provider doesn't know about
        // this sandbox (server restart + recovery miss, or container was
        // removed externally) we still flip the status so subsequent
        // listings don't show a zombie "running" entry.
        //
        // Except when a worker node can't be reached (ADR-048): the
        // container is most likely still running there, so the row stays
        // `running` and the next sweep retries. Control-plane sandboxes
        // keep the old behaviour.
        let stopped = if row.node_id.is_some() {
            // A worker that accepts connections but never answers would hold
            // this sequential sweep for the provider's lifecycle timeout,
            // delaying expiry for every sandbox in the cluster.
            tokio::time::timeout(
                WORKER_STOP_DEADLINE,
                self.registry.stop(row.id, &row.public_id),
            )
            .await
            .unwrap_or_else(|_| {
                Err(
                    temps_agents::error::AgentError::SandboxProviderUnavailable {
                        provider: "expiration sweep".to_string(),
                        reason: format!(
                            "the node did not answer within {}s",
                            WORKER_STOP_DEADLINE.as_secs()
                        ),
                    },
                )
            })
        } else {
            self.registry.stop(row.id, &row.public_id).await
        };
        match stopped {
            Err(e) if leave_running(row, &e) => {
                tracing::warn!(
                    "Expiration sweep: sandbox {} (internal {}) is on an unavailable node; \
                     leaving it running and retrying next sweep: {}",
                    row.public_id,
                    row.id,
                    e
                );
                return Ok(StopOutcome::NodeUnreachable);
            }
            Err(e) => tracing::warn!(
                "Expiration sweep: provider stop failed for sandbox {} (internal {}): {} \
                 — marking stopped anyway",
                row.public_id,
                row.id,
                e
            ),
            Ok(()) => tracing::info!(
                "Expiration sweep: stopped sandbox {} (internal {}, expired at {})",
                row.public_id,
                row.id,
                row.expires_at
            ),
        }

        mark_stopped(self.db.as_ref(), row).await
    }
}

/// Move an expired row to `stopped`, unless it left `running` meanwhile.
async fn mark_stopped<C: sea_orm::ConnectionTrait>(
    db: &C,
    row: &sandboxes::Model,
) -> Result<StopOutcome, sea_orm::DbErr> {
    let changes = sandboxes::ActiveModel {
        id: Set(row.id),
        status: Set("stopped".to_string()),
        last_activity_at: Set(Utc::now()),
        ..Default::default()
    };
    match row_status::update_when(db, changes, Expect::Status("running")).await? {
        Some(_) => Ok(StopOutcome::Stopped),
        None => {
            tracing::info!(
                sandbox_id = %row.public_id,
                internal_id = row.id,
                "Expiration sweep: sandbox left 'running' while it was being stopped \
                 (destroyed, evicted or paused meanwhile); not marking it stopped"
            );
            Ok(StopOutcome::Superseded)
        }
    }
}

/// Stop each expired row with `stop`, and return how many were moved to
/// `stopped`. One hung worker costs one
/// deadline per sweep, not one per sandbox: once a row is left running
/// because its node did not answer, that node's other rows wait for the
/// next sweep.
async fn sweep<F, Fut>(expired: Vec<sandboxes::Model>, stop: F) -> usize
where
    F: Fn(sandboxes::Model) -> Fut,
    Fut: std::future::Future<Output = Result<StopOutcome, sea_orm::DbErr>>,
{
    let mut stopped = 0usize;
    let mut unreachable = UnreachableNodes::default();
    for row in expired {
        if unreachable.skips(&row) {
            tracing::debug!(
                "Expiration sweep: skipping sandbox {} — its node did not answer this sweep",
                row.public_id
            );
            continue;
        }
        match stop(row.clone()).await {
            Ok(StopOutcome::Stopped) => stopped += 1,
            // Only an unreachable worker leaves a row running.
            Ok(StopOutcome::NodeUnreachable) => unreachable.record(&row),
            Ok(StopOutcome::Superseded) => {}
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
    stopped
}

/// Worker nodes that failed to answer during one sweep.
#[derive(Default)]
struct UnreachableNodes(std::collections::HashSet<i32>);

impl UnreachableNodes {
    fn record(&mut self, row: &sandboxes::Model) {
        if let Some(node_id) = row.node_id {
            self.0.insert(node_id);
        }
    }

    fn skips(&self, row: &sandboxes::Model) -> bool {
        row.node_id.is_some_and(|id| self.0.contains(&id))
    }
}

/// Whether a failed stop should leave the row `running` for the next sweep:
/// only when the sandbox is on a worker that could not be reached, as
/// opposed to the sandbox itself failing.
fn leave_running(row: &sandboxes::Model, e: &temps_agents::error::AgentError) -> bool {
    row.node_id.is_some()
        && matches!(
            e,
            temps_agents::error::AgentError::SandboxNodeUnavailable { .. }
                | temps_agents::error::AgentError::SandboxProviderUnavailable { .. }
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn make_row(id: i32, status: &str, expires_in_secs: i64) -> sandboxes::Model {
        let now = Utc::now();
        sandboxes::Model {
            id,
            node_id: None,
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
            lifecycle: "ephemeral".to_string(),
            project_id: None,
            source_repo_url: None,
        }
    }

    #[test]
    fn only_unreachable_worker_sandboxes_are_left_running() {
        use temps_agents::error::AgentError;
        let unreachable = || AgentError::SandboxProviderUnavailable {
            provider: "node 'worker-1'".into(),
            reason: "connection refused".into(),
        };
        let failed = AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: "x".into(),
            reason: "no such container".into(),
        };
        let worker = sandboxes::Model {
            node_id: Some(3),
            ..make_row(1, "running", -60)
        };
        let local = make_row(2, "running", -60);

        assert!(leave_running(&worker, &unreachable()));
        // The sandbox itself failed: the row is marked stopped as before.
        assert!(!leave_running(&worker, &failed));
        // A control plane whose own Docker is down keeps the old behaviour.
        assert!(!leave_running(&local, &unreachable()));
    }

    #[test]
    fn an_unreachable_node_only_skips_its_own_sandboxes() {
        let on = |node_id: Option<i32>, id: i32| sandboxes::Model {
            node_id,
            ..make_row(id, "running", -60)
        };
        let mut unreachable = UnreachableNodes::default();
        unreachable.record(&on(Some(3), 1));

        assert!(unreachable.skips(&on(Some(3), 2)), "same hung node: wait");
        assert!(
            !unreachable.skips(&on(Some(4), 3)),
            "other workers still sweep"
        );
        assert!(
            !unreachable.skips(&on(None, 4)),
            "the control plane still sweeps"
        );
        // Recording a control-plane row never blocks anything.
        unreachable.record(&on(None, 5));
        assert!(!unreachable.skips(&on(None, 6)));
    }

    /// A worker that does not answer is tried once per sweep: its other
    /// expired sandboxes are skipped (left running for the next sweep),
    /// while other nodes' and the control plane's are still stopped.
    #[tokio::test]
    async fn a_sweep_tries_an_unreachable_node_once() {
        let on = |id: i32, node: Option<i32>| sandboxes::Model {
            node_id: node,
            ..make_row(id, "running", -60)
        };
        let rows = vec![
            on(1, Some(2)),
            on(2, Some(2)),
            on(3, None),
            on(4, Some(5)),
            on(5, Some(2)),
        ];
        let tried = std::sync::Mutex::new(Vec::new());

        let stopped = sweep(rows, |row| {
            tried.lock().expect("tried").push(row.id);
            // Node 2 never answers: the sweep sees the row left running.
            let outcome = if row.node_id == Some(2) {
                StopOutcome::NodeUnreachable
            } else {
                StopOutcome::Stopped
            };
            async move { Ok::<_, sea_orm::DbErr>(outcome) }
        })
        .await;

        assert_eq!(stopped, 2, "the control plane's and node 5's");
        assert_eq!(*tried.lock().expect("tried"), vec![1, 3, 4]);
    }

    /// A stop that times out is reported as the provider being unavailable,
    /// which leaves a worker's row running rather than marking it stopped.
    #[test]
    fn a_timed_out_worker_stop_leaves_the_row_running() {
        let timed_out = temps_agents::error::AgentError::SandboxProviderUnavailable {
            provider: "expiration sweep".to_string(),
            reason: "the node did not answer within 30s".to_string(),
        };
        let worker = sandboxes::Model {
            node_id: Some(2),
            ..make_row(1, "running", -60)
        };
        assert!(leave_running(&worker, &timed_out));
        assert!(!leave_running(&make_row(2, "running", -60), &timed_out));
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
        let rows = expired_sandboxes(Utc::now()).all(&db).await.expect("query");
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn tick_updates_status_for_expired_rows() {
        // Row with expires_at in the past is listed by the query, and the
        // sweeper moves it to `stopped` while it is still `running`.
        let expired = make_row(1_000_042, "running", -60);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![expired.clone()]])
            // The conditional UPDATE ... RETURNING returns the updated row.
            .append_query_results(vec![vec![sandboxes::Model {
                status: "stopped".to_string(),
                ..expired.clone()
            }]])
            .into_connection();

        let rows = expired_sandboxes(Utc::now()).all(&db).await.expect("query");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, 1_000_042);

        let outcome = mark_stopped(&db, &rows[0]).await.expect("update");
        assert_eq!(outcome, StopOutcome::Stopped);
        let sql = db
            .into_transaction_log()
            .iter()
            .flat_map(|t| t.statements())
            .map(ToString::to_string)
            .find(|sql| sql.starts_with("UPDATE"))
            .expect("status update");
        assert!(
            sql.contains(r#""status" = 'running'"#),
            "the stop must only apply to a row that is still running: {sql}"
        );
    }

    /// A destroy or node eviction that lands while the sweep waits on a
    /// worker leaves the row `destroyed`. The sweep must not write
    /// `stopped` over it: the row would block removing the node and point at
    /// a container that no longer exists.
    #[tokio::test]
    async fn a_row_destroyed_during_the_stop_is_not_resurrected() {
        let expired = sandboxes::Model {
            node_id: Some(3),
            ..make_row(1_000_044, "running", -60)
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // The conditional UPDATE matched no row.
            .append_query_results(vec![Vec::<sandboxes::Model>::new()])
            .into_connection();

        let outcome = mark_stopped(&db, &expired).await.expect("no error");

        assert_eq!(outcome, StopOutcome::Superseded);
    }

    /// A superseded row is neither counted as stopped nor treated as an
    /// unreachable node (the node's other rows are still swept).
    #[tokio::test]
    async fn a_superseded_row_does_not_skip_its_node() {
        let on = |id: i32| sandboxes::Model {
            node_id: Some(2),
            ..make_row(id, "running", -60)
        };
        let tried = std::sync::Mutex::new(Vec::new());

        let stopped = sweep(vec![on(1), on(2)], |row| {
            tried.lock().expect("tried").push(row.id);
            let outcome = if row.id == 1 {
                StopOutcome::Superseded
            } else {
                StopOutcome::Stopped
            };
            async move { Ok::<_, sea_orm::DbErr>(outcome) }
        })
        .await;

        assert_eq!(stopped, 1);
        assert_eq!(*tried.lock().expect("tried"), vec![1, 2]);
    }

    #[test]
    fn make_row_helper_produces_expected_shape() {
        // Sanity check the test helper so failures in the other tests
        // point at the sweeper, not the fixture.
        let r = make_row(42, "running", -10);
        assert_eq!(r.status, "running");
        assert!(r.expires_at < Utc::now());
    }

    #[tokio::test]
    async fn expiry_query_excludes_managed_application_workspaces() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results::<sandboxes::Model, _, _>(vec![vec![]])
            .into_connection();

        let rows = expired_sandboxes(Utc::now())
            .all(&db)
            .await
            .expect("expiration query");
        assert!(rows.is_empty());

        let sql = format!("{:?}", db.into_transaction_log());
        assert!(
            sql.contains("ai-application:%") && sql.contains("NOT LIKE"),
            "expiration query must exclude Temps-managed application workspaces: {sql}"
        );
    }

    #[tokio::test]
    async fn expiry_query_still_includes_ordinary_durable_workspaces() {
        let mut workspace = make_row(1_000_043, "running", -60);
        workspace.lifecycle = "workspace".to_string();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![workspace]])
            .into_connection();

        let rows = expired_sandboxes(Utc::now()).all(&db).await.expect("query");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].lifecycle, "workspace");
    }
}
