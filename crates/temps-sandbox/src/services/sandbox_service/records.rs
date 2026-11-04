// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Conditional row updates and the sandbox event log.

use super::*;

impl SandboxService {
    /// Write `changes` to `row` only while its status still matches
    /// `expect`. Lifecycle calls read the row, wait on the provider (tens of
    /// seconds on a worker node), then write it; a destroy or node eviction
    /// that lands in between wins, and the sandbox is reported gone
    /// (`NotFound`) instead of being written back to life. If `expect` names
    /// a status and the row has meanwhile reached `settled` (the expiry sweep
    /// stopped it while a pause was in flight), that row is the result. Any
    /// other status is an `InvalidState` for `operation`.
    pub(super) async fn update_row_if(
        &self,
        row: &sandboxes::Model,
        changes: sandboxes::ActiveModel,
        expect: Expect<'_>,
        settled: Option<&str>,
        operation: &str,
    ) -> Result<sandboxes::Model, SandboxError> {
        if let Some(updated) = row_status::update_when(self.db.as_ref(), changes, expect).await? {
            return Ok(updated);
        }
        let current = sandboxes::Entity::find_by_id(row.id)
            .one(self.db.as_ref())
            .await?;
        match current {
            Some(current) if settled == Some(current.status.as_str()) => Ok(current),
            Some(current) if current.status != row_status::DESTROYED => {
                Err(SandboxError::InvalidState {
                    sandbox_id: row.public_id.clone(),
                    state: current.status,
                    operation: operation.to_string(),
                })
            }
            _ => {
                tracing::info!(
                    sandbox_id = %row.public_id,
                    internal_id = row.id,
                    node_id = ?row.node_id,
                    operation,
                    "sandbox was destroyed while the {operation} was in flight; \
                     leaving it destroyed"
                );
                Err(SandboxError::NotFound {
                    sandbox_id: row.public_id.clone(),
                })
            }
        }
    }

    /// [`Self::update_row_if`] for a row whose compute was just (re)created.
    /// If the row was destroyed meanwhile, the new compute is removed too,
    /// so no container outlives its row.
    pub(super) async fn update_new_compute_row(
        &self,
        row: &sandboxes::Model,
        changes: sandboxes::ActiveModel,
        operation: &str,
    ) -> Result<sandboxes::Model, SandboxError> {
        match self
            .update_row_if(row, changes, Expect::Live, None, operation)
            .await
        {
            Err(gone @ SandboxError::NotFound { .. }) => {
                self.remove_failed_create(row, &format!("it was destroyed during a {operation}"))
                    .await;
                Err(gone)
            }
            other => other,
        }
    }

    pub(super) async fn mark_destroyed(&self, id: i32) -> Result<(), SandboxError> {
        let now = Utc::now();
        let active = sandboxes::ActiveModel {
            id: Set(id),
            status: Set("destroyed".to_string()),
            last_activity_at: Set(now),
            ..Default::default()
        };
        active.update(self.db.as_ref()).await?;
        Ok(())
    }

    /// Put what the driver facade reported when it placed the sandbox on its
    /// timeline (ADR-055 §5): a requested limit the driver could not apply
    /// (`LimitEnforced = False`, with the reason) or a passed-over driver.
    /// Nothing is recorded when there is nothing to say, which is always the
    /// case outside `enforce` mode.
    pub(super) async fn record_driver_conditions(
        &self,
        sandbox_id: i32,
        handle: &temps_sandbox_providers::SandboxHandle,
    ) {
        if handle.conditions.is_empty() {
            return;
        }
        let conditions = match serde_json::to_value(&handle.conditions) {
            Ok(value) => value,
            Err(e) => {
                tracing::warn!(
                    "failed to encode driver conditions for sandbox {} ({}): {}",
                    sandbox_id,
                    handle.sandbox_name,
                    e
                );
                return;
            }
        };
        self.record_event(
            sandbox_id,
            "driver_conditions",
            Some(serde_json::json!({ "conditions": conditions })),
        )
        .await;
    }

    /// Append one operation to a sandbox's timeline. Best-effort — a failed
    /// insert is logged, never fatal to the operation it records.
    pub(super) async fn record_event(
        &self,
        sandbox_id: i32,
        event_type: &str,
        detail: Option<serde_json::Value>,
    ) {
        let active = sandbox_events::ActiveModel {
            sandbox_id: Set(sandbox_id),
            event_type: Set(event_type.to_string()),
            detail: Set(detail),
            created_at: Set(Utc::now()),
            ..Default::default()
        };
        if let Err(e) = active.insert(self.db.as_ref()).await {
            tracing::warn!(
                "failed to record '{}' event for sandbox {}: {}",
                event_type,
                sandbox_id,
                e
            );
        }
    }

    /// The operations timeline for a sandbox, newest first. Ownership is
    /// enforced by resolving the sandbox through `find_by_public_id`.
    pub async fn list_events(
        &self,
        public_id_value: &str,
        user_id: i32,
        limit: u64,
    ) -> Result<Vec<sandbox_events::Model>, SandboxError> {
        // Bounded — the timeline is append-only and a UI only ever shows the
        // most recent slice. Cap the page so a long-lived sandbox can't force
        // an unbounded scan.
        let limit = limit.clamp(1, 500);
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        let events = sandbox_events::Entity::find()
            .filter(sandbox_events::Column::SandboxId.eq(row.id))
            .order_by_desc(sandbox_events::Column::CreatedAt)
            .order_by_desc(sandbox_events::Column::Id)
            .limit(limit)
            .all(self.db.as_ref())
            .await?;
        Ok(events)
    }

    /// Persist the effective backend (and, when the request didn't specify
    /// one, the resolved image) a sandbox runs on.
    pub(super) async fn record_backend(
        &self,
        id: i32,
        backend: temps_sandbox_providers::SandboxBackend,
        effective_image: Option<String>,
    ) -> Result<(), SandboxError> {
        let mut active = sandboxes::ActiveModel {
            id: Set(id),
            backend: Set(Some(backend.to_string())),
            ..Default::default()
        };
        if let Some(image) = effective_image {
            active.image = Set(Some(image));
        }
        active.update(self.db.as_ref()).await?;
        Ok(())
    }
}
