// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sandboxes owned by agent runs: create, release and orphan cleanup.

use super::*;

/// Reject standalone lifecycle ops (`pause`/`resume`/`restart`/`resize`)
/// on rows attributed to an agent run. Those containers are named
/// `temps-sandbox-<run_id>` — not by the row's public id — so the
/// standalone registry's name-based recovery would miss them and the DB
/// row would drift from the real container state. The run owns the
/// lifecycle; the user must act on the run instead.
pub(super) fn ensure_not_agent_run(row: &sandboxes::Model) -> Result<(), SandboxError> {
    if let Some(run_id) = row.agent_run_id {
        return Err(SandboxError::ManagedByAgentRun {
            sandbox_id: row.public_id.clone(),
            run_id,
        });
    }
    Ok(())
}

/// Is the owning agent run finished (or gone)? `None` means the run row
/// no longer exists — nothing owns the sandbox anymore, so treat it as
/// terminal and let cleanup proceed. Shared by `destroy_sandbox`'s
/// agent-run branch and `release_orphaned_agent_run_sandboxes`.
pub(super) fn run_status_is_terminal(status: Option<&str>) -> bool {
    status
        .map(|s| TERMINAL_RUN_STATUSES.contains(&s))
        .unwrap_or(true)
}

impl SandboxService {
    // ── Agent-run sandboxes ──────────────────────────────────────────────

    /// Release every non-destroyed sandbox row attributed to an agent run
    /// that is already terminal (or whose run row no longer exists).
    ///
    /// Called by the plugin on startup, *after* the agents plugin's
    /// `AgentRunService::recover_stuck_runs` (register phase) has failed
    /// every run that was in flight when the server died. Without this,
    /// those runs' `sandboxes` rows stay "running" forever: the standalone
    /// recovery scan and the expiration sweeper both skip agent-run rows
    /// by design, and the run itself will never call `release` again —
    /// zombie row, possibly a leaked container.
    ///
    /// Per-run failures are logged and skipped so one bad row can't block
    /// cleanup of the rest. Returns the number of runs whose sandboxes
    /// were released.
    pub async fn release_orphaned_agent_run_sandboxes(&self) -> Result<usize, SandboxError> {
        let rows = sandboxes::Entity::find()
            .filter(sandboxes::Column::AgentRunId.is_not_null())
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .all(self.db.as_ref())
            .await?;
        if rows.is_empty() {
            return Ok(0);
        }

        // One lookup for all runs (no per-row query), deduped: a run can
        // in principle have several stale rows, and `release_for_agent_run`
        // already sweeps every row for its run.
        let mut run_ids: Vec<i32> = rows.iter().filter_map(|r| r.agent_run_id).collect();
        run_ids.sort_unstable();
        run_ids.dedup();
        let runs = agent_runs::Entity::find()
            .filter(agent_runs::Column::Id.is_in(run_ids.iter().copied()))
            .all(self.db.as_ref())
            .await?;
        let status_by_run: HashMap<i32, String> =
            runs.into_iter().map(|r| (r.id, r.status)).collect();

        let mut released = 0usize;
        for run_id in run_ids {
            let terminal = run_status_is_terminal(status_by_run.get(&run_id).map(|s| s.as_str()));
            if !terminal {
                // Legitimately active run (e.g. re-triggered right after
                // restart) — its sandbox is alive and owned; leave it.
                continue;
            }
            match self.release_for_agent_run(run_id, None).await {
                Ok(()) => released += 1,
                Err(e) => {
                    tracing::warn!(
                        "orphaned agent-run sandbox cleanup: failed to release sandbox(es) \
                         for terminal agent run {}: {}",
                        run_id,
                        e
                    );
                }
            }
        }
        if released > 0 {
            tracing::info!(
                "Released sandboxes for {} terminal agent run(s) on startup",
                released
            );
        }
        Ok(released)
    }

    /// Create a sandbox for an agent run (autofixer / workflow agent) as a
    /// first-class `sandboxes` row, then create the container through the
    /// shared provider. Called by the agents' `SandboxRegistry` via the
    /// `RunSandboxService` seam (see `temps_sandbox_providers::managed`).
    ///
    /// Unlike standalone sandboxes, the container keeps the historical
    /// `temps-sandbox-<run_id>` naming (no `container_name_override`) so
    /// run recovery after a server restart keeps working, and the handle is
    /// keyed by `run_id` in the agents' registry — NOT by this row's `id`.
    /// The row is bookkeeping + API visibility; the agent run owns the
    /// lifecycle (the expiration sweeper skips rows with `agent_run_id`).
    pub async fn create_for_agent_run(
        &self,
        config: SandboxCreateConfig,
    ) -> Result<temps_sandbox_providers::SandboxHandle, SandboxError> {
        let run_id = config.run_id;

        // A run can recreate its sandbox after a dead container — retire
        // any previous non-destroyed row for the same run first so the API
        // never shows two "running" sandboxes for one run.
        let stale = sandboxes::Entity::find()
            .filter(sandboxes::Column::AgentRunId.eq(run_id))
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .all(self.db.as_ref())
            .await?;
        for row in stale {
            self.mark_destroyed(row.id).await.ok();
            self.record_event(
                row.id,
                "destroyed",
                Some(serde_json::json!({ "reason": "superseded by new container for agent run" })),
            )
            .await;
        }

        let public_id_value = public_id::generate();
        let now = Utc::now();
        let timeout = config
            .idle_timeout
            .as_secs()
            .clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS);
        let active = sandboxes::ActiveModel {
            public_id: Set(public_id_value.clone()),
            user_id: Set(config.owner_user_id),
            agent_run_id: Set(Some(run_id)),
            name: Set(format!("temps-sandbox-{}", run_id)),
            status: Set("running".to_string()),
            image: Set(config.image.clone()),
            work_dir: Set(temps_sandbox_providers::SANDBOX_WORK_DIR.to_string()),
            timeout_secs: Set(timeout as i32),
            metadata: Set(Some(serde_json::json!({ "source": "agent_run" }))),
            created_at: Set(now),
            last_activity_at: Set(now),
            expires_at: Set(now + chrono::Duration::seconds(timeout as i64)),
            preview_access: Set(crate::services::preview_access::initial_access(false)),
            ..Default::default()
        };
        let row = active.insert(self.db.as_ref()).await?;

        let handle = match self.registry.provider_arc().create(config).await {
            Ok(h) => h,
            Err(e) => {
                // Roll the row over to destroyed so the API doesn't show a
                // zombie "running" sandbox for a container that never started.
                self.mark_destroyed(row.id).await.ok();
                return Err(SandboxError::CreateFailed {
                    user_id: 0,
                    reason: format!("agent run {}: {}", run_id, e),
                });
            }
        };

        if let Err(e) = self
            .record_backend(
                row.id,
                handle.backend,
                (!handle.image.is_empty()).then(|| handle.image.clone()),
            )
            .await
        {
            tracing::warn!(
                "failed to record backend/image for agent-run sandbox {} (run {}): {}",
                public_id_value,
                run_id,
                e
            );
        }

        self.record_event(
            row.id,
            "created",
            Some(serde_json::json!({
                "agent_run_id": run_id,
                "backend": handle.backend.to_string(),
                "image": handle.image,
            })),
        )
        .await;
        self.record_driver_conditions(row.id, &handle).await;

        tracing::info!(
            "Created agent-run sandbox {} (internal {}) for run {}",
            public_id_value,
            row.id,
            run_id
        );

        Ok(handle)
    }

    /// Destroy the container backing an agent run's sandbox and mark its
    /// row(s) destroyed. `handle` is the agents' registry cached handle
    /// when available; otherwise the container is recovered by run id.
    pub async fn release_for_agent_run(
        &self,
        run_id: i32,
        handle: Option<&temps_sandbox_providers::SandboxHandle>,
    ) -> Result<(), SandboxError> {
        let provider = self.registry.provider_arc();
        let resolved = match handle {
            Some(h) => Some(h.clone()),
            None => provider.recover(run_id).await.unwrap_or_else(|e| {
                tracing::warn!(
                    "release_for_agent_run: recover for run {} failed: {}",
                    run_id,
                    e
                );
                None
            }),
        };
        if let Some(h) = resolved {
            // Agent runs are ephemeral — purge the home volume too.
            if let Err(e) = provider.destroy(&h, true).await {
                tracing::warn!(
                    "Failed to destroy agent-run sandbox {} for run {}: {}",
                    h.sandbox_name,
                    run_id,
                    e
                );
            }
        }

        let rows = sandboxes::Entity::find()
            .filter(sandboxes::Column::AgentRunId.eq(run_id))
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .all(self.db.as_ref())
            .await?;
        for row in rows {
            self.mark_destroyed(row.id).await?;
            self.record_event(
                row.id,
                "destroyed",
                Some(serde_json::json!({ "agent_run_id": run_id })),
            )
            .await;
        }
        Ok(())
    }
}
