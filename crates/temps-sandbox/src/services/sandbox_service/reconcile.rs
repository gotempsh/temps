// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! VM liveness: the startup reconciliation of running rows and marking a
//! lost VM stopped.

use super::*;

/// How many VM liveness checks the startup reconciliation runs at once.
const VM_RECONCILE_CONCURRENCY: usize = 8;

/// How long the startup reconciliation waits for one VM's liveness check.
/// Longer than a vsock ping, shorter than anything an operator would call a
/// hung start.
const VM_RECONCILE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// How many ids the startup reconciliation names in its summary.
pub(super) const VM_RECONCILE_REPORTED_IDS: usize = 50;

/// Rows loaded per page by [`SandboxService::reconcile_all_running_vms`]:
/// the most rows the reconciliation holds at once, however many sandboxes the
/// instance has.
const VM_RECONCILE_PAGE_SIZE: u64 = 100;

/// What the startup reconciliation found, for the one summary log line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct VmReconcileReport {
    /// Running rows examined.
    pub checked: usize,
    /// Rows whose VM answered: left alone.
    pub alive: usize,
    /// Public ids of rows moved from `running` to `stopped` because their VM
    /// is not running, capped at [`VM_RECONCILE_REPORTED_IDS`] so the report
    /// stays small however many VMs a restart took down. Each move also has
    /// its own `vm_lost` event and log line.
    pub marked_stopped: Vec<String>,
    /// How many rows were moved to `stopped` (the full count behind
    /// `marked_stopped`).
    pub marked_stopped_total: usize,
    /// Rows whose VM is down but whose row was left as it was: a managed
    /// application workspace (which reports a lost runtime as `recovering`),
    /// a row that changed since it was read, or a failed database write.
    pub not_marked: usize,
    /// Rows left as they were because the check neither confirmed nor ruled
    /// out a live VM (error or timeout).
    pub unverified: usize,
}

impl VmReconcileReport {
    /// Fold the report of another page into this one.
    fn absorb(&mut self, page: VmReconcileReport) {
        self.checked += page.checked;
        self.alive += page.alive;
        self.marked_stopped_total += page.marked_stopped_total;
        self.not_marked += page.not_marked;
        self.unverified += page.unverified;
        let room = VM_RECONCILE_REPORTED_IDS.saturating_sub(self.marked_stopped.len());
        self.marked_stopped
            .extend(page.marked_stopped.into_iter().take(room));
    }

    /// Whether every row examined has a confirmed live VM.
    pub fn all_alive(&self) -> bool {
        self.alive == self.checked
    }
}

/// Whether the startup reconciliation may judge this row. Firecracker and
/// microsandbox VMMs are started by Temps alone, so a down one is final.
/// Docker containers carry `unless-stopped`: after a host reboot dockerd may
/// still be bringing them back when Temps starts, so a down container proves
/// nothing yet and is left to the check on first access.
pub fn reconcilable_at_startup(row: &sandboxes::Model) -> bool {
    matches!(row.backend.as_deref(), Some("firecracker" | "microsandbox"))
}

/// One row's liveness verdict in [`SandboxService::reconcile_running_vms`].
enum VmCheck {
    Alive,
    Down(temps_ai_agents::error::AgentError),
    Unknown(String),
}

impl SandboxService {
    // ── Helpers shared with the exec/fs modules ──────────────────────────

    /// A provider handle for a sandbox that must be running, for exec, files
    /// and terminals. Maps provider errors to typed [`SandboxError`]s and, when
    /// the provider reports the VM is down while the row still says `running`,
    /// reconciles the row to `stopped` so listings stop lying and
    /// `sandbox resume` has something to resume.
    pub(crate) async fn live_handle(
        &self,
        row: &sandboxes::Model,
    ) -> Result<temps_ai_agents::sandbox::SandboxHandle, SandboxError> {
        match self.registry.probe(row.id, &row.public_id).await {
            VmProbe::Alive(handle) => Ok(handle),
            VmProbe::Down(error) => {
                self.mark_vm_lost(row, &error, "access").await;
                Err(from_agent_error(&row.public_id, error))
            }
            VmProbe::Unknown(error) => Err(from_agent_error(&row.public_id, error)),
        }
    }

    /// Move a `running` row whose VM the provider reports as down to `stopped`,
    /// leaving `expires_at` and the rest of the row untouched, and record a
    /// `vm_lost` event naming the cause. Conditional on the row still being
    /// `running` *and* untouched since `row` was read, so it never overwrites a
    /// concurrent pause or destroy, nor a resume that landed after the probe
    /// (a resume moves `last_activity_at`).
    /// Returns whether this call changed the row. Failures are logged, not
    /// raised: the caller is already reporting the underlying problem.
    pub(crate) async fn mark_vm_lost(
        &self,
        row: &sandboxes::Model,
        cause: &temps_ai_agents::error::AgentError,
        source: &'static str,
    ) -> bool {
        // Managed application workspaces report a lost runtime as the derived
        // status `recovering`, which is what makes their own recovery (and the
        // data-network reconciliation, which only visits `running | recovering`
        // rows) run. Stopping the row would hide that.
        if row.status != "running" || managed_application_id_from_name(&row.name).is_some() {
            return false;
        }
        // The verdict was reached from a probe that may have run while the
        // control plane itself had the VM down (snapshot capture, restart,
        // disk resize); those start it again and leave the row `running`.
        // Take the claim that excludes them, then ask again: the first answer
        // may describe a stop that has since finished.
        let Some(_unplanned) = self.registry.observe_unplanned(row.id) else {
            tracing::debug!(
                sandbox_id = %row.public_id,
                internal_id = row.id,
                source,
                "VM is down for a planned stop; not marking the sandbox lost"
            );
            return false;
        };
        match tokio::time::timeout(
            VM_RECONCILE_DEADLINE,
            self.registry.probe(row.id, &row.public_id),
        )
        .await
        {
            Ok(VmProbe::Down(_)) => {}
            Ok(VmProbe::Alive(_)) | Ok(VmProbe::Unknown(_)) | Err(_) => return false,
        }
        let changes = sandboxes::ActiveModel {
            id: Set(row.id),
            status: Set("stopped".to_string()),
            ..Default::default()
        };
        match row_status::update_when(
            self.db.as_ref(),
            changes,
            Expect::RunningSince(row.last_activity_at),
        )
        .await
        {
            Ok(Some(_)) => {
                // Only once the row really moved: a stale copy of the row must
                // not kill the jobs of a sandbox that was resumed meanwhile.
                self.jobs.abort_all(row.id).await;
                tracing::warn!(
                    sandbox_id = %row.public_id,
                    internal_id = row.id,
                    backend = ?row.backend,
                    node_id = ?row.node_id,
                    source,
                    reason = %cause,
                    "Sandbox row said running but its VM is not; marked stopped (resume it to boot the VM again)"
                );
                self.record_event(
                    row.id,
                    "vm_lost",
                    Some(serde_json::json!({
                        "source": source,
                        "backend": row.backend,
                        "reason": cause.to_string(),
                    })),
                )
                .await;
                true
            }
            Ok(None) => false,
            Err(error) => {
                tracing::error!(
                    sandbox_id = %row.public_id,
                    internal_id = row.id,
                    error = %error,
                    "Could not mark a sandbox whose VM is not running as stopped"
                );
                false
            }
        }
    }

    /// Startup reconciliation: for every control-plane VM sandbox row that
    /// says `running`, ask the provider whether its VM really is, and move the
    /// ones that are not to `stopped`. Rows are read in id-ordered pages of
    /// [`VM_RECONCILE_PAGE_SIZE`], each page checked before the next is read,
    /// so memory stays constant however many sandboxes exist and slow checks
    /// cannot pile rows up. A page that cannot be read ends the run early
    /// (logged); the lazy check on first access covers whatever was not seen.
    pub async fn reconcile_all_running_vms(&self) -> VmReconcileReport {
        self.reconcile_running_vms_paged(VM_RECONCILE_PAGE_SIZE)
            .await
    }

    pub(super) async fn reconcile_running_vms_paged(&self, page_size: u64) -> VmReconcileReport {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};

        let mut report = VmReconcileReport::default();
        let mut after_id = 0;
        loop {
            let page = sandboxes::Entity::find()
                .filter(sandboxes::Column::Status.eq("running"))
                .filter(sandboxes::Column::NodeId.is_null())
                .filter(sandboxes::Column::AgentRunId.is_null())
                .filter(sandboxes::Column::Backend.is_in(["firecracker", "microsandbox"]))
                .filter(sandboxes::Column::Id.gt(after_id))
                .order_by_asc(sandboxes::Column::Id)
                .limit(page_size)
                .all(self.db.as_ref())
                .await;
            let rows = match page {
                Ok(rows) => rows,
                Err(error) => {
                    tracing::warn!(
                        after_id,
                        error = %error,
                        "Could not read the next page of running sandboxes for VM reconciliation; stopping early"
                    );
                    break;
                }
            };
            let Some(last) = rows.last() else { break };
            after_id = last.id;
            let full_page = u64::try_from(rows.len()).is_ok_and(|n| n >= page_size);
            report.absorb(self.reconcile_running_vms(rows).await);
            if !full_page {
                break;
            }
        }
        report
    }

    /// Check one bounded page of rows (see [`Self::reconcile_all_running_vms`]),
    /// with bounded concurrency and a per-sandbox deadline so a wedged VM
    /// cannot stall startup. A liveness check that fails without proving the
    /// VM is down (error, timeout) leaves the row alone: the lazy check on
    /// first access decides later.
    pub async fn reconcile_running_vms(&self, rows: Vec<sandboxes::Model>) -> VmReconcileReport {
        use futures::StreamExt;

        let total = rows.len();
        // Verdicts are consumed as they arrive: only the bounded number of
        // in-flight checks is ever held, not one result per row.
        let mut verdicts = futures::stream::iter(rows)
            .map(|row| async move {
                let check = match tokio::time::timeout(
                    VM_RECONCILE_DEADLINE,
                    self.registry.probe(row.id, &row.public_id),
                )
                .await
                {
                    Ok(VmProbe::Alive(_)) => VmCheck::Alive,
                    Ok(VmProbe::Down(error)) => VmCheck::Down(error),
                    Ok(VmProbe::Unknown(error)) => VmCheck::Unknown(error.to_string()),
                    Err(_) => VmCheck::Unknown(format!(
                        "liveness check did not answer within {}s",
                        VM_RECONCILE_DEADLINE.as_secs()
                    )),
                };
                (row, check)
            })
            .buffer_unordered(VM_RECONCILE_CONCURRENCY);

        let mut report = VmReconcileReport {
            checked: total,
            ..Default::default()
        };
        while let Some((row, check)) = verdicts.next().await {
            match check {
                VmCheck::Alive => report.alive += 1,
                VmCheck::Down(cause) => {
                    if self.mark_vm_lost(&row, &cause, "startup").await {
                        report.marked_stopped_total += 1;
                        if report.marked_stopped.len() < VM_RECONCILE_REPORTED_IDS {
                            report.marked_stopped.push(row.public_id.clone());
                        }
                    } else {
                        report.not_marked += 1;
                    }
                }
                VmCheck::Unknown(reason) => {
                    tracing::warn!(
                        sandbox_id = %row.public_id,
                        internal_id = row.id,
                        backend = ?row.backend,
                        reason = %reason,
                        "Could not tell whether a running sandbox's VM is alive at startup; leaving its row as is"
                    );
                    report.unverified += 1;
                }
            }
        }
        report
    }
}
