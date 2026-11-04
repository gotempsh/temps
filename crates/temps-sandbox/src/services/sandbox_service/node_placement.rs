// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Placement on worker nodes (ADR-048): node listings, the allow-list and
//! evicting a node's sandboxes.

use super::*;

/// Page size bounds for [`SandboxService::node_sandboxes`].
pub const NODE_SANDBOXES_DEFAULT_PAGE_SIZE: u64 = 20;
pub const NODE_SANDBOXES_MAX_PAGE_SIZE: u64 = 100;
/// Highest page a listing accepts, so `page * page_size` can't overflow in
/// the query. Pages past the data are empty anyway.
pub(super) const MAX_PAGE: u64 = 1_000_000;

/// A live sandbox on a node, with its owner (operator view).
#[derive(Debug, Clone)]
pub struct NodeSandbox {
    pub summary: SandboxSummary,
    pub owner_user_id: Option<i32>,
    pub owner_email: Option<String>,
}

/// Result of [`SandboxService::evict_node`].
#[derive(Debug, Clone)]
pub struct NodeEviction {
    pub node: placement::PlacementNode,
    /// Public ids of the sandboxes whose rows were destroyed, including
    /// those in `containers_unconfirmed`.
    pub destroyed: Vec<String>,
    /// Destroyed sandboxes whose container the node did not confirm
    /// removing (unreachable, timed out, or the destroy failed), with the
    /// reason. The container may still be running on the node.
    pub containers_unconfirmed: Vec<(String, String)>,
    /// Sandboxes that could not be destroyed, with the reason. Retrying the
    /// eviction picks them up again; destroyed ones are not revisited.
    pub failed: Vec<(String, String)>,
    /// Owners (user ids) of the destroyed sandboxes, sorted and unique, so
    /// the audit record says whose sandboxes the operator removed.
    pub owner_user_ids: Vec<i32>,
}

/// How many sandboxes an eviction destroys at once.
pub(super) const EVICTION_CONCURRENCY: usize = 8;

/// How long an eviction waits for one container destroy. A node that
/// accepts connections but never answers (overloaded, frozen) would
/// otherwise hold every call for the provider's full lifecycle timeout.
pub(super) const EVICTION_DESTROY_DEADLINE: std::time::Duration =
    std::time::Duration::from_secs(30);

/// Page and page size for [`SandboxService::node_sandboxes`], clamped to
/// what is served (and echoed back, so a client sees what it got).
pub(super) fn node_sandboxes_page(page: Option<u64>, page_size: Option<u64>) -> (u64, u64) {
    (
        page.unwrap_or(1).clamp(1, MAX_PAGE),
        page_size
            .unwrap_or(NODE_SANDBOXES_DEFAULT_PAGE_SIZE)
            .clamp(1, NODE_SANDBOXES_MAX_PAGE_SIZE),
    )
}

/// Sort an eviction's per-sandbox results into destroyed rows (confirmed
/// or not), rows whose container the node never confirmed removing (with
/// the reason), and rows that could not be destroyed (with the error).
/// Each list is sorted by sandbox id.
#[allow(clippy::type_complexity)]
pub(super) fn split_eviction_outcomes(
    outcomes: Vec<(String, Result<ContainerOutcome, SandboxError>)>,
) -> (Vec<String>, Vec<(String, String)>, Vec<(String, String)>) {
    let mut destroyed = Vec::with_capacity(outcomes.len());
    let mut containers_unconfirmed = Vec::new();
    let mut failed = Vec::new();
    for (public_id, outcome) in outcomes {
        match outcome {
            Ok(ContainerOutcome::Destroyed) => destroyed.push(public_id),
            Ok(ContainerOutcome::Unconfirmed(reason)) => {
                containers_unconfirmed.push((public_id.clone(), reason));
                destroyed.push(public_id);
            }
            Err(e) => failed.push((public_id, bounded_reason(&e.to_string()))),
        }
    }
    destroyed.sort();
    containers_unconfirmed.sort();
    failed.sort();
    (destroyed, containers_unconfirmed, failed)
}

/// Owners of the `destroyed` sandboxes, sorted and unique. `owners` maps
/// each evicted sandbox's public id to its owner.
pub(super) fn eviction_owners(
    destroyed: &[String],
    owners: &HashMap<String, Option<i32>>,
) -> Vec<i32> {
    let ids: std::collections::BTreeSet<i32> = destroyed
        .iter()
        .filter_map(|id| owners.get(id).copied().flatten())
        .collect();
    ids.into_iter().collect()
}

/// Longest reason kept from a provider or worker error.
pub(super) const MAX_REASON_CHARS: usize = 512;

/// Make an error reason safe to hand back to an API client or print in a
/// terminal: a worker node controls part of this text, so drop control
/// characters (terminal escapes included) and cap the length.
pub(crate) fn bounded_reason(reason: &str) -> String {
    let mut out: String = reason
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_REASON_CHARS)
        .collect();
    if reason.chars().count() > MAX_REASON_CHARS {
        out.push('…');
    }
    out
}

/// Result of [`SandboxService::node_sandboxes`].
#[derive(Debug, Clone)]
pub struct NodeSandboxes {
    pub node: placement::PlacementNode,
    /// One page, newest first.
    pub sandboxes: Vec<NodeSandbox>,
    /// Every live sandbox on the node, across all pages.
    pub total: u64,
    /// The page and page size actually served, after clamping.
    pub page: u64,
    pub page_size: u64,
}

impl SandboxService {
    // ── Placement (ADR-048) ─────────────────────────────────────────────

    async fn placement_policy(&self) -> Result<placement::PlacementPolicy, SandboxError> {
        // Read the allow-list on its own: the whole-settings getter falls
        // back to defaults when any field is malformed, which would read as
        // "every node is allowed". This fails closed instead.
        let allowed_node_ids = self
            .platform_config
            .get_sandbox_allowed_node_ids()
            .await
            .map_err(|source| SandboxError::PlacementSettings {
                operation: "load",
                source,
            })?;
        Ok(placement::PlacementPolicy {
            allowed_node_ids,
            control_plane_runs_no_workloads: self.control_plane_runs_no_workloads,
        })
    }

    /// Every worker, with those being evicted marked unplaceable.
    async fn placement_workers(&self) -> Result<Vec<placement::WorkerNode>, SandboxError> {
        let mut workers = placement::load_workers(self.db.as_ref()).await?;
        placement::mark_evicting(&mut workers, &self.evicting_nodes.snapshot());
        Ok(workers)
    }

    /// Sandboxes `user_id` created through the API that are not destroyed.
    /// Agent runs, managed application workspaces and the sandboxes the
    /// platform creates for AI chat or autofix are not marked as API-created
    /// and belong to the platform, not to the user's quota. Sandboxes created
    /// before the mark existed are not counted.
    pub(super) async fn live_standalone_sandboxes<C: ConnectionTrait>(
        conn: &C,
        user_id: i32,
    ) -> Result<u64, SandboxError> {
        sandboxes::Entity::find()
            .filter(sandboxes::Column::UserId.eq(user_id))
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .filter(Expr::cust_with_values(
                "\"sandboxes\".\"metadata\"->>'source' = $1",
                [API_SANDBOX_SOURCE],
            ))
            .count(conn)
            .await
            .map_err(SandboxError::from)
    }

    /// Node a new sandbox runs on: `None` = control plane.
    pub(super) async fn place_sandbox(
        &self,
        req: &CreateSandboxRequest,
    ) -> Result<Option<i32>, SandboxError> {
        let requested = req.node.as_deref().map(placement::RequestedNode::parse);
        // Managed AI application workspaces live in a control-plane
        // directory; they cannot be placed elsewhere.
        if req.host_work_dir_override.is_some() {
            // ADR 063: that directory is on a host that runs nothing.
            if self.control_plane_runs_no_workloads {
                return Err(SandboxError::no_local_host("An application workspace"));
            }
            return match requested {
                None | Some(placement::RequestedNode::ControlPlane) => Ok(None),
                Some(_) => Err(SandboxError::Validation {
                    message: "application workspaces always run on the control plane".into(),
                }),
            };
        }
        let policy = self.placement_policy().await?;
        let control_plane_allowed = policy.allows(placement::CONTROL_PLANE_NODE_ID);
        // The default path — no node asked for, control plane allowed — is
        // every single-node install; it needs no node lookups at all.
        let (workers, live) = match &requested {
            None if control_plane_allowed => (Vec::new(), HashMap::new()),
            Some(placement::RequestedNode::ControlPlane) => (Vec::new(), HashMap::new()),
            None => (
                self.placement_workers().await?,
                placement::live_counts(self.db.as_ref()).await?,
            ),
            Some(_) => (self.placement_workers().await?, HashMap::new()),
        };
        let candidates = placement::candidates(&policy, &workers, &live, requested.as_ref())?;
        // A chosen worker is asked whether it can run a sandbox (its agent
        // may predate sandbox support, or its Docker may be down) before a
        // row is written for it. The control plane is never asked.
        placement::settle(candidates, self.node_probe.as_ref()).await
    }

    /// Whether `requested` names the control plane's own `nodes` row (role
    /// `control-plane`), which [`placement::classify`] leaves out of the
    /// worker list. Only asked when a lookup missed.
    async fn is_control_plane_row(
        &self,
        requested: &placement::RequestedNode,
    ) -> Result<bool, SandboxError> {
        let query = match requested {
            placement::RequestedNode::Id(id) => nodes::Entity::find_by_id(*id),
            placement::RequestedNode::Name(name) => {
                nodes::Entity::find().filter(nodes::Column::Name.eq(name.as_str()))
            }
            placement::RequestedNode::ControlPlane => return Ok(false),
        };
        Ok(query
            .one(self.db.as_ref())
            .await?
            .is_some_and(|row| placement::is_control_plane_role(&row.role)))
    }

    /// Every node with its sandbox placement state (control plane first).
    pub async fn placement_nodes(&self) -> Result<Vec<placement::PlacementNode>, SandboxError> {
        let policy = self.placement_policy().await?;
        let workers = self.placement_workers().await?;
        let live = placement::live_counts(self.db.as_ref()).await?;
        let mut nodes = placement::describe(&policy, &workers, &live);
        if let Some(control_plane) = nodes.iter_mut().find(|n| n.is_control_plane) {
            control_plane.backends = self.control_plane_backends();
        }
        Ok(nodes)
    }

    /// What the control plane's own provider can run, with the setup step
    /// for each backend it cannot. Registration is decided at startup, so
    /// this is a read of in-memory state plus, for an unavailable
    /// microsandbox, the host probe that names what is missing.
    pub(super) fn control_plane_backends(&self) -> Vec<placement::NodeBackend> {
        let provider = self.registry.provider_arc();
        temps_sandbox_providers::SELECTABLE_SANDBOX_BACKENDS
            .into_iter()
            .map(|backend| {
                let available = provider.supports_backend(backend);
                placement::NodeBackend {
                    backend,
                    available,
                    reason: (!available).then(|| backend_unavailable_message(backend)),
                }
            })
            .collect()
    }

    /// Live sandboxes on one node, from every owner (the operator view on
    /// the node page, ADR-048). `node` takes the same forms as
    /// `create --node`: a worker name or id, or `control-plane` / `0`.
    pub async fn node_sandboxes(
        &self,
        node: &str,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<NodeSandboxes, SandboxError> {
        let (page, page_size) = node_sandboxes_page(page, page_size);
        let mut requested = placement::RequestedNode::parse(node);
        let listing = self.placement_nodes().await?;
        // The node page addresses every node by its `nodes` row id,
        // including the control plane's own row, which placement knows only
        // as `0` / `control-plane`.
        if placement::find_node(listing.clone(), &requested).is_none()
            && self.is_control_plane_row(&requested).await?
        {
            requested = placement::RequestedNode::ControlPlane;
        }
        let found = placement::find_node(listing, &requested).ok_or_else(|| {
            SandboxError::NodeNotFound {
                node: node.to_string(),
            }
        })?;

        let mut query = sandboxes::Entity::find().filter(sandboxes::Column::Status.ne("destroyed"));
        query = if found.is_control_plane {
            query.filter(sandboxes::Column::NodeId.is_null())
        } else {
            query.filter(sandboxes::Column::NodeId.eq(found.id))
        };
        let paginator = query
            .order_by_desc(sandboxes::Column::CreatedAt)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let rows = paginator
            .fetch_page(temps_core::config::page_index(page))
            .await?;

        let owner_ids: std::collections::HashSet<i32> =
            rows.iter().filter_map(|r| r.user_id).collect();
        let owners: HashMap<i32, String> = if owner_ids.is_empty() {
            HashMap::new()
        } else {
            users::Entity::find()
                .filter(users::Column::Id.is_in(owner_ids))
                .all(self.db.as_ref())
                .await?
                .into_iter()
                .map(|u| (u.id, u.email))
                .collect()
        };

        let sandboxes = rows
            .iter()
            .map(|row| {
                let mut summary = SandboxSummary::from(row);
                summary.node_name = found.name.clone();
                NodeSandbox {
                    summary,
                    owner_user_id: row.user_id,
                    owner_email: row.user_id.and_then(|id| owners.get(&id).cloned()),
                }
            })
            .collect();
        Ok(NodeSandboxes {
            node: found,
            sandboxes,
            total,
            page,
            page_size,
        })
    }

    /// Destroy every live sandbox on a worker node, whoever owns it — the
    /// operator's way to clear a node before removing it, including a node
    /// that is gone for good (ADR-048). Containers are destroyed best-effort:
    /// an unreachable node cannot answer, and its rows are marked destroyed
    /// regardless so the node can be removed. Every sandbox is attempted
    /// even if some fail, and the result says which ones did.
    pub async fn evict_node(&self, node: &str) -> Result<NodeEviction, SandboxError> {
        use futures::StreamExt;

        let requested = placement::RequestedNode::parse(node);
        if requested == placement::RequestedNode::ControlPlane {
            return Err(SandboxError::Validation {
                message: "the control plane's sandboxes cannot be evicted; destroy them \
                          individually with `bunx @temps-sdk/cli sandbox destroy <id>`"
                    .to_string(),
            });
        }
        let found = placement::find_node(self.placement_nodes().await?, &requested)
            .filter(|n| !n.is_control_plane)
            .ok_or_else(|| SandboxError::NodeNotFound {
                node: node.to_string(),
            })?;
        // Cordon the node for the whole eviction: placement skips it, so no
        // new sandbox lands on it behind the eviction's back, and a second
        // eviction of it is refused instead of racing this one.
        let in_progress = self.evicting_nodes.begin(found.id).ok_or_else(|| {
            SandboxError::NodeEvictionInProgress {
                node: found.name.clone(),
            }
        })?;
        tracing::info!(
            node_id = found.id,
            node = %found.name,
            "sandbox placement: evicting every sandbox on the node"
        );
        let rows = sandboxes::Entity::find()
            .filter(sandboxes::Column::NodeId.eq(found.id))
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .all(self.db.as_ref())
            .await?;
        let owners: HashMap<String, Option<i32>> = rows
            .iter()
            .map(|r| (r.public_id.clone(), r.user_id))
            .collect();
        let gate = EvictionGate::new(EVICTION_DESTROY_DEADLINE);
        let gate = &gate;
        let outcomes: Vec<(String, Result<ContainerOutcome, SandboxError>)> =
            futures::stream::iter(rows)
                .map(|row| async move {
                    let public_id = row.public_id.clone();
                    (
                        public_id,
                        self.destroy_row(row, true, DestroyMode::Evict(gate)).await,
                    )
                })
                .buffer_unordered(EVICTION_CONCURRENCY)
                .collect()
                .await;
        let (destroyed, containers_unconfirmed, failed) = split_eviction_outcomes(outcomes);
        let owner_user_ids = eviction_owners(&destroyed, &owners);
        // The node takes sandboxes again before it is reported.
        drop(in_progress);
        // Report the node as it is now, not as it was before the destroys.
        // Best effort: sandboxes are already destroyed, so a failure here
        // must not turn the eviction into an error (the handler would then
        // skip its audit record).
        let node = match self.placement_nodes().await {
            Ok(nodes) => placement::find_node(nodes, &placement::RequestedNode::Id(found.id))
                .unwrap_or(found),
            Err(e) => {
                tracing::warn!(
                    node_id = found.id,
                    node = %found.name,
                    error = %e,
                    "sandbox placement: could not refresh the node after evicting it; \
                     reporting it as it was before the eviction"
                );
                found
            }
        };
        Ok(NodeEviction {
            node,
            destroyed,
            containers_unconfirmed,
            failed,
            owner_user_ids,
        })
    }

    /// Current allow-list (`None` = all nodes).
    /// Removed nodes are dropped from the list: their ids can never come
    /// back (node ids are not reused), and a stale id would make every later
    /// save fail validation.
    pub async fn allowed_node_ids(&self) -> Result<Option<Vec<i32>>, SandboxError> {
        let allowed = self.placement_policy().await?.allowed_node_ids;
        if allowed.is_none() {
            return Ok(None);
        }
        let workers = placement::load_workers(self.db.as_ref()).await?;
        Ok(placement::existing_node_ids(allowed, &workers))
    }

    /// Replace the allow-list. `None` = allow every node. Existing sandboxes
    /// are unaffected: the list only governs where new sandboxes go.
    pub async fn set_allowed_node_ids(
        &self,
        allowed: Option<Vec<i32>>,
    ) -> Result<Option<Vec<i32>>, SandboxError> {
        if let Some(ids) = allowed.as_ref() {
            let workers = placement::load_workers(self.db.as_ref()).await?;
            placement::validate_allowed_ids(ids, &workers)?;
        }
        let normalized = allowed.map(|mut ids| {
            ids.sort_unstable();
            ids
        });
        self.platform_config
            .set_sandbox_allowed_node_ids(normalized.clone())
            .await
            .map_err(|source| SandboxError::PlacementSettings {
                operation: "save",
                source,
            })?;
        Ok(normalized)
    }

    /// Replace the `node-<id>` placeholders from `From<&Model>` with real
    /// node names (one query for the whole batch).
    pub async fn fill_node_names(&self, summaries: &mut [SandboxSummary]) {
        self.mark_creates_in_progress(summaries);
        let ids: std::collections::HashSet<i32> =
            summaries.iter().filter_map(|s| s.node_id).collect();
        if ids.is_empty() {
            return;
        }
        match placement::node_names(self.db.as_ref(), ids).await {
            Ok(names) => {
                for s in summaries.iter_mut() {
                    if let Some(name) = s.node_id.and_then(|id| names.get(&id)) {
                        s.node_name = name.clone();
                    }
                }
            }
            Err(e) => tracing::warn!("resolve sandbox node names: {}", e),
        }
    }
}
