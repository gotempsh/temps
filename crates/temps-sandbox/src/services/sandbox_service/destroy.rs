// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Destroying a sandbox: the row, its container (on the control plane or a
//! worker) and its host work directory.

use super::*;

/// How long destroying one sandbox on a worker may take before the call
/// gives up and reports the node unreachable (the row is kept).
const WORKER_DESTROY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// Consecutive unanswered destroys after which an eviction writes off a
/// node that did answer earlier: one full round of concurrent calls.
pub(super) const EVICTION_UNANSWERED_LIMIT: usize = EVICTION_CONCURRENCY;

/// Shared by the destroys of one eviction. The node is treated as down, and
/// the remaining container calls are skipped instead of each waiting out
/// the deadline, when it fails as unreachable or times out and either has
/// never answered or has now failed [`EVICTION_UNANSWERED_LIMIT`] calls in a
/// row (it froze part-way). A single slow call from a node that is
/// answering does not write it off. Rows are marked destroyed either way.
pub(super) struct EvictionGate {
    deadline: std::time::Duration,
    node_down: std::sync::atomic::AtomicBool,
    node_answered: std::sync::atomic::AtomicBool,
    unanswered_in_a_row: std::sync::atomic::AtomicUsize,
}

impl EvictionGate {
    pub(super) fn new(deadline: std::time::Duration) -> Self {
        Self {
            deadline,
            node_down: std::sync::atomic::AtomicBool::new(false),
            node_answered: std::sync::atomic::AtomicBool::new(false),
            unanswered_in_a_row: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub(super) fn is_down(&self) -> bool {
        self.node_down.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(super) fn answered(&self) {
        self.node_answered
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.unanswered_in_a_row
            .store(0, std::sync::atomic::Ordering::Relaxed);
    }

    /// The node failed to answer one call.
    pub(super) fn unanswered(&self) {
        let in_a_row = self
            .unanswered_in_a_row
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        let never_answered = !self
            .node_answered
            .load(std::sync::atomic::Ordering::Relaxed);
        if never_answered || in_a_row >= EVICTION_UNANSWERED_LIMIT {
            self.node_down
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// How [`SandboxService::destroy_row`] treats a node that does not answer.
pub(super) enum DestroyMode<'a> {
    /// One sandbox destroyed on request. If its worker node is unreachable
    /// or does not answer within the deadline, the row is kept and the
    /// caller gets [`SandboxError::NodeUnreachable`]: marking it destroyed
    /// would leave a running container that nothing tracks any more.
    Single {
        worker_deadline: std::time::Duration,
    },
    /// Part of evicting a node. The row is marked destroyed whatever the
    /// node does, and the outcome says whether the container is gone.
    Evict(&'a EvictionGate),
}

/// What happened to a sandbox's container when its row was destroyed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ContainerOutcome {
    /// The provider destroyed it, or it was already gone.
    Destroyed,
    /// The provider could not confirm it; the container may still be
    /// running on its node. Carries the reason.
    Unconfirmed(String),
}

/// The container-name suffix for a standalone sandbox: the `public_id` with
/// its `sbx_` prefix stripped.
///
/// The same label the preview hostname embeds, so the gateway can
/// DNS-resolve `temps-sandbox-<label>` straight from the URL — and, because
/// the Docker provider derives the home volume name from the container
/// name, the thing that decides which volume this sandbox owns. Extracted
/// so `sandbox_service_and_provider_agree_on_volume_naming` can pin the
/// cross-crate half of that agreement; the original leak was exactly two
/// places deriving one identity independently.
pub(super) fn container_label_for(public_id_value: &str) -> &str {
    public_id_value
        .strip_prefix(public_id::PUBLIC_ID_PREFIX)
        .unwrap_or(public_id_value)
}

/// Shell command an operator runs on a node to remove a sandbox's leftover
/// containers (the sandbox and its egress proxy both carry its container
/// name), for when an eviction could not confirm removing them. A no-op if
/// they are already gone (the node may have removed them and lost the
/// reply). Worker nodes run Linux, so `xargs -r` is available.
pub fn node_cleanup_command(public_id_value: &str) -> String {
    format!(
        "docker ps -aq --filter name={}{} | xargs -r docker rm -f",
        temps_sandbox_providers::docker::SANDBOX_NAME_PREFIX,
        container_label_for(public_id_value)
    )
}

/// Which host directory (if any) `destroy_sandbox` may recursively delete
/// for a given public id.
///
/// Split out from `remove_work_dir` so the guard is unit-testable without
/// standing up a whole service. `data_root.join(s)` silently resolves
/// absolute paths and `..` segments, so a public id of `../../etc` would
/// aim a recursive delete outside the data dir entirely. Ids are generated
/// internally today, but "the caller is trusted" is not something a
/// `remove_dir_all` should rely on — accept only the exact `sbx_<16 hex>`
/// shape `public_id::generate` produces.
pub(super) fn work_dir_to_remove(data_root: &Path, public_id_value: &str) -> Option<PathBuf> {
    if !public_id::is_valid(public_id_value) {
        return None;
    }
    Some(data_root.join(public_id_value))
}

impl SandboxService {
    /// Stop + destroy a sandbox. Aborts any background jobs, asks the
    /// provider to tear down the container + volumes, and marks the
    /// DB row "destroyed".
    ///
    /// Agent-run sandboxes (`agent_run_id` set) are special-cased: their
    /// container is named `temps-sandbox-<run_id>`, so the standalone
    /// registry (which recovers by `public_id`) would miss it, flip the
    /// row to "destroyed", and leave the real container running. While
    /// the run is active we refuse with [`SandboxError::ManagedByAgentRun`]
    /// (destroying the sandbox under a live run would break it); once the
    /// run is terminal we route through `release_for_agent_run`, which
    /// targets the container by run id.
    pub async fn destroy_sandbox(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<(), SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        self.destroy_row(
            row,
            false,
            DestroyMode::Single {
                worker_deadline: WORKER_DESTROY_DEADLINE,
            },
        )
        .await
        .map(|_| ())
    }

    /// Destroy one sandbox row, whoever owns it (callers check ownership).
    /// `force` also destroys a sandbox still managed by a live agent run —
    /// only for operator actions such as [`Self::evict_node`]. `mode` bounds
    /// how long a worker's container destroy may take and decides what an
    /// unreachable worker means (see [`DestroyMode`]). Otherwise the row is
    /// marked destroyed even when the container destroy fails, and the
    /// outcome says whether the container is confirmed gone.
    pub(super) async fn destroy_row(
        &self,
        row: sandboxes::Model,
        force: bool,
        mode: DestroyMode<'_>,
    ) -> Result<ContainerOutcome, SandboxError> {
        let public_id_value = row.public_id.as_str();
        if let Some(run_id) = row.agent_run_id {
            let run = agent_runs::Entity::find_by_id(run_id)
                .one(self.db.as_ref())
                .await?;
            let terminal = run_status_is_terminal(run.as_ref().map(|r| r.status.as_str()));
            if !terminal && !force {
                return Err(SandboxError::ManagedByAgentRun {
                    sandbox_id: public_id_value.to_string(),
                    run_id,
                });
            }
            self.jobs.abort_all(row.id).await;
            return self
                .release_for_agent_run(run_id, None)
                .await
                .map(|_| ContainerOutcome::Destroyed);
        }

        self.jobs.abort_all(row.id).await;
        let destroyed = match mode {
            DestroyMode::Single { .. } if row.node_id.is_none() => {
                self.registry.destroy(row.id, public_id_value).await
            }
            DestroyMode::Single { worker_deadline } => {
                let unreachable = match tokio::time::timeout(
                    worker_deadline,
                    self.registry.destroy(row.id, public_id_value),
                )
                .await
                {
                    Ok(Err(
                        e @ (AgentError::SandboxNodeUnavailable { .. }
                        | AgentError::SandboxProviderUnavailable { .. }),
                    )) => e.to_string(),
                    Ok(result) => return self.finish_destroy(&row, result).await,
                    Err(_) => format!("it did not answer within {}s", worker_deadline.as_secs()),
                };
                return Err(self.node_unreachable(&row, &unreachable).await);
            }
            DestroyMode::Evict(gate) if gate.is_down() => {
                Err(AgentError::SandboxProviderUnavailable {
                    provider: "eviction".to_string(),
                    reason: "the node did not answer earlier destroys in this eviction; \
                         skipped the container call"
                        .to_string(),
                })
            }
            DestroyMode::Evict(gate) => {
                match tokio::time::timeout(
                    gate.deadline,
                    self.registry.destroy(row.id, public_id_value),
                )
                .await
                {
                    // A worker answering 503 (its Docker is down) did reply,
                    // but the rest of its destroys would fail the same way,
                    // so it counts as unanswered too.
                    Ok(Err(
                        e @ (AgentError::SandboxNodeUnavailable { .. }
                        | AgentError::SandboxProviderUnavailable { .. }),
                    )) => {
                        gate.unanswered();
                        Err(e)
                    }
                    Ok(result) => {
                        gate.answered();
                        result
                    }
                    Err(_) => {
                        gate.unanswered();
                        Err(AgentError::SandboxProviderUnavailable {
                            provider: "eviction".to_string(),
                            reason: format!(
                                "the node did not answer within {}s",
                                gate.deadline.as_secs()
                            ),
                        })
                    }
                }
            }
        };
        self.finish_destroy(&row, destroyed).await
    }

    /// The node hosting `row` could not destroy its container. The row is
    /// left as it is; the error names the node and says what to do.
    async fn node_unreachable(&self, row: &sandboxes::Model, reason: &str) -> SandboxError {
        let node = match row.node_id {
            Some(id) => placement::node_names(self.db.as_ref(), [id])
                .await
                .ok()
                .and_then(|mut names| names.remove(&id))
                .unwrap_or_else(|| format!("id {id}")),
            None => placement::CONTROL_PLANE_NAME.to_string(),
        };
        tracing::warn!(
            sandbox_id = %row.public_id,
            node = %node,
            "not destroying sandbox: its node is unreachable ({reason}); the row is kept"
        );
        SandboxError::NodeUnreachable {
            sandbox_id: row.public_id.clone(),
            node,
            reason: bounded_reason(reason),
        }
    }

    /// Mark `row` destroyed after its container destroy was attempted, and
    /// clean up what belongs to it.
    async fn finish_destroy(
        &self,
        row: &sandboxes::Model,
        destroyed: Result<(), AgentError>,
    ) -> Result<ContainerOutcome, SandboxError> {
        let public_id_value = row.public_id.as_str();
        let outcome = match destroyed {
            Ok(()) => ContainerOutcome::Destroyed,
            Err(e) => {
                // Even if the container destroy failed, mark the row
                // destroyed — otherwise the user is stuck with a zombie
                // they can't delete. Log the provider error loudly, and
                // report it so callers don't claim the container is gone.
                tracing::error!(
                    "Provider destroy failed for sandbox {} (internal {}): {} — marking row destroyed anyway",
                    public_id_value,
                    row.id,
                    e
                );
                ContainerOutcome::Unconfirmed(bounded_reason(&e.to_string()))
            }
        };
        // Remove the work dir even when the provider destroy failed. There
        // is no background sweep to fall back on — this call is the only
        // thing that ever frees the directory, so skipping it here means
        // leaking it permanently, which is the bug this fixes. The tradeoff
        // is that a failed destroy may leave a container still running with
        // this directory bind-mounted at /workspace, and the delete then
        // races that writer. Deleting is still the right call: the user
        // explicitly asked for the sandbox to be gone, and the row is
        // marked destroyed either way.
        self.remove_work_dir(public_id_value).await;
        self.record_event(row.id, "destroyed", None).await;
        self.mark_destroyed(row.id).await?;

        // Standalone snapshots may outlive their deleted sandbox without
        // retaining an internal identity. Managed application snapshots keep
        // immutable provenance so they can never become visible through the
        // generic snapshot API after the compute row is destroyed.
        if !row.name.starts_with(APPLICATION_WORKSPACE_NAME_PREFIX) {
            if let Some(ref snap_svc) = self.snapshot_service {
                if let Err(e) = snap_svc.nullify_source_sandbox(row.id).await {
                    tracing::warn!(
                        sandbox_id = %public_id_value,
                        internal_id = row.id,
                        "destroy: failed to nullify source_sandbox_id on snapshots: {}",
                        e
                    );
                }
            }
        }

        Ok(outcome)
    }

    /// Delete the host-side `/workspace` directory backing a destroyed
    /// sandbox.
    ///
    /// `create_sandbox` makes `data_root/<public_id>` and bind-mounts it
    /// into the container; nothing used to remove it, so every
    /// create+destroy cycle left the full working tree (node_modules, build
    /// output, cloned repos — routinely hundreds of MB) on the host
    /// forever. Deliberately best-effort and non-fatal: the sandbox is
    /// already gone from the user's point of view, and failing the API call
    /// over leftover bytes would strand the row instead.
    ///
    /// Only called for standalone sandboxes. Agent-run sandboxes get their
    /// work dir from the executor, which owns that directory's lifecycle.
    pub(super) async fn remove_work_dir(&self, public_id_value: &str) {
        let Some(work_dir) = work_dir_to_remove(&self.data_root, public_id_value) else {
            tracing::warn!(
                "Refusing to remove sandbox work dir for malformed public id {:?}",
                public_id_value
            );
            return;
        };
        match tokio::fs::remove_dir_all(&work_dir).await {
            Ok(()) => tracing::debug!("Removed sandbox work dir {}", work_dir.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(
                "Failed to remove sandbox work dir {}: {} — disk space stays claimed",
                work_dir.display(),
                e
            ),
        }
    }
}
