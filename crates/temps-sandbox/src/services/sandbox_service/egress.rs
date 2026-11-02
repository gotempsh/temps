// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Applying, storing and re-applying a sandbox's egress policy.

use super::*;

/// `reason` of the `stopped` event recorded when a started sandbox is
/// stopped again because its restricted egress policy could not be
/// re-applied.
pub const EGRESS_POLICY_REAPPLY_FAILED: &str = "egress_policy_reapply_failed";

impl SandboxService {
    /// Replace the sandbox's egress policy: which outside destinations it
    /// may reach.
    ///
    /// The backend applies the policy first and the row is written only
    /// once it has, so the stored policy is never one the sandbox does not
    /// enforce. If that write then fails (the sandbox was destroyed
    /// meanwhile, or the database is unavailable), the previous policy is
    /// applied again before the error is returned. A running Docker sandbox
    /// gets a new egress proxy, which cuts its outbound connections for a
    /// second or two; a stopped one keeps the policy for its next start.
    pub async fn set_egress_policy(
        &self,
        public_id_value: &str,
        user_id: i32,
        policy: temps_ai_agents::sandbox::SandboxEgressPolicy,
    ) -> Result<sandboxes::Model, SandboxError> {
        let policy = policy
            .normalized()
            .map_err(|error| SandboxError::Validation {
                message: format!("egress policy for sandbox {public_id_value}: {error}"),
            })?;
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        ensure_not_agent_run(&row)?;
        if row.status != "running" && row.status != "stopped" {
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: row.status.clone(),
                operation: "change its egress policy".into(),
            });
        }
        // Serializes with every other change to this sandbox's compute
        // (restart, resize, a resume re-applying its policy), so two updates
        // cannot leave the row naming one policy and the sandbox enforcing
        // the other.
        let _serialized = self.registry.begin_planned_downtime(row.id).await;
        let previous = row.egress_policy.0.clone();
        self.registry
            .set_egress_policy(row.id, public_id_value, &policy)
            .await
            .map_err(|e| from_agent_error(public_id_value, e))?;
        let changes = sandboxes::ActiveModel {
            id: Set(row.id),
            egress_policy: Set(sandboxes::StoredEgressPolicy(policy.clone())),
            last_activity_at: Set(Utc::now()),
            ..Default::default()
        };
        let updated = match self
            .update_row_if(&row, changes, Expect::Live, None, "egress policy update")
            .await
        {
            Ok(updated) => updated,
            Err(error) => {
                if let Err(rollback) = self
                    .registry
                    .set_egress_policy(row.id, public_id_value, &previous)
                    .await
                {
                    tracing::error!(
                        sandbox_id = %public_id_value,
                        internal_id = row.id,
                        applied_mode = policy.mode.as_str(),
                        stored_mode = previous.mode.as_str(),
                        "egress policy was applied but not stored ({error}), and restoring \
                         the stored policy failed too ({rollback}); the sandbox enforces a \
                         policy its row does not show until the policy is set again"
                    );
                }
                return Err(error);
            }
        };
        tracing::info!(
            sandbox_id = %public_id_value,
            internal_id = row.id,
            from_mode = previous.mode.as_str(),
            to_mode = policy.mode.as_str(),
            rules = policy.rules.len(),
            "sandbox egress policy updated"
        );
        self.record_event(
            row.id,
            "egress_policy_updated",
            Some(serde_json::json!({
                "from_mode": previous.mode.as_str(),
                "to_mode": policy.mode.as_str(),
                "rules": policy.rules.len(),
            })),
        )
        .await;
        Ok(updated)
    }

    /// Make a sandbox that has just started enforce its stored egress
    /// policy. The backend carries the policy across a stop (Docker keeps it
    /// on the parked egress proxy), so this is normally a no-op; it puts the
    /// policy back on a sandbox whose proxy was lost while it was stopped.
    /// A restricted sandbox that cannot get its policy back is stopped again
    /// rather than left running with wider egress than its owner allowed.
    pub(super) async fn enforce_stored_egress_policy(
        &self,
        row: &sandboxes::Model,
    ) -> Result<(), SandboxError> {
        // `internet` is what a backend without a stored policy enforces, so
        // only a restricted policy can need re-applying. A concurrent change
        // to restricted is applied by `set_egress_policy` itself.
        if !row.egress_policy.0.is_restricted() {
            return Ok(());
        }
        let _serialized = self.registry.begin_planned_downtime(row.id).await;
        // Re-read under the lock: a concurrent `set_egress_policy` may have
        // replaced the policy since `row` was loaded.
        let Some(current) = sandboxes::Entity::find_by_id(row.id)
            .one(self.db.as_ref())
            .await?
        else {
            return Ok(());
        };
        let policy = current.egress_policy.0;
        let Err(error) = self
            .registry
            .set_egress_policy(row.id, &row.public_id, &policy)
            .await
        else {
            return Ok(());
        };
        tracing::error!(
            sandbox_id = %row.public_id,
            internal_id = row.id,
            "could not re-apply the restricted egress policy of a started sandbox ({error}); \
             stopping it so it does not run with wider egress"
        );
        self.jobs.abort_all(row.id).await;
        match self.registry.stop(row.id, &row.public_id).await {
            Ok(()) | Err(temps_ai_agents::error::AgentError::SandboxNotFound { .. }) => {
                self.mark_stopped_for_egress_failure(row, &error).await;
            }
            Err(stop) => {
                // The compute may still be running; a `stopped` row would hide
                // it, so the row keeps its status and the error says why.
                tracing::error!(
                    sandbox_id = %row.public_id,
                    internal_id = row.id,
                    "could not stop sandbox after failing to re-apply its egress policy: {stop}"
                );
            }
        }
        Err(from_agent_error(&row.public_id, error))
    }

    /// Record that the service stopped a sandbox because its stored egress
    /// policy could not be re-applied: the row moves to `stopped` (unless it
    /// was destroyed meanwhile) and a `stopped` event carries the reason, so
    /// the console can say why the sandbox is not running.
    async fn mark_stopped_for_egress_failure(
        &self,
        row: &sandboxes::Model,
        cause: &temps_ai_agents::error::AgentError,
    ) {
        let changes = sandboxes::ActiveModel {
            id: Set(row.id),
            status: Set("stopped".to_string()),
            last_activity_at: Set(Utc::now()),
            ..Default::default()
        };
        match row_status::update_when(self.db.as_ref(), changes, Expect::Live).await {
            Ok(Some(_)) => {
                self.record_event(
                    row.id,
                    "stopped",
                    Some(serde_json::json!({
                        "reason": EGRESS_POLICY_REAPPLY_FAILED,
                        "detail": format!(
                            "the sandbox's restricted egress policy could not be re-applied \
                             after it started, so it was stopped rather than run with wider \
                             egress: {cause}"
                        ),
                    })),
                )
                .await;
            }
            Ok(None) => {}
            Err(db_error) => {
                tracing::error!(
                    sandbox_id = %row.public_id,
                    internal_id = row.id,
                    error = %db_error,
                    "could not mark sandbox stopped after failing to re-apply its egress policy"
                );
            }
        }
    }
}
