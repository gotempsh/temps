// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deployment-related traits and types

use async_trait::async_trait;
use thiserror::Error;

/// Failure while removing application containers before their owning database
/// records are deleted.
#[derive(Debug, Error)]
pub enum ContainerCleanupError {
    #[error(
        "Failed to list containers for project {project_id}, environment {environment_id:?}: {reason}"
    )]
    Discovery {
        project_id: i32,
        environment_id: Option<i32>,
        reason: String,
    },

    #[error(
        "Failed to prepare container '{container_id}' on node {node_id:?} for deletion from project {project_id}, environment {environment_id}: {reason}"
    )]
    Prepare {
        project_id: i32,
        environment_id: i32,
        container_id: String,
        node_id: Option<i32>,
        reason: String,
    },

    #[error(
        "Failed to remove container '{container_id}' on node {node_id:?} from project {project_id}, environment {environment_id}: {reason}"
    )]
    Removal {
        project_id: i32,
        environment_id: i32,
        container_id: String,
        node_id: Option<i32>,
        reason: String,
    },

    /// The container's node can't be reached, so it can't be confirmed gone. Not a
    /// failure of Temps, and retrying once the node is back finishes the removal.
    #[error(
        "Cannot remove container '{container_id}' from project {project_id}, environment \
         {environment_id}: node {node_id:?} can't be reached, so nothing was deleted. Retry \
         when the node is back, or remove the node first: {reason}"
    )]
    NodeUnreachable {
        project_id: i32,
        environment_id: i32,
        container_id: String,
        node_id: Option<i32>,
        reason: String,
    },

    #[error(
        "Removed container '{container_id}' on node {node_id:?}, but failed to finalize its database record for project {project_id}, environment {environment_id}: {reason}"
    )]
    Finalize {
        project_id: i32,
        environment_id: i32,
        container_id: String,
        node_id: Option<i32>,
        reason: String,
    },
}

/// Removes application containers before project/environment records are
/// deleted. Implemented by `temps-deployments` and consumed through the
/// service registry to avoid crate dependency cycles.
#[async_trait]
pub trait DeploymentContainerCleaner: Send + Sync {
    /// Remove every active application container owned by a project.
    async fn cleanup_project_containers(
        &self,
        project_id: i32,
    ) -> Result<u64, ContainerCleanupError>;

    /// Remove every active application container owned by an environment.
    async fn cleanup_environment_containers(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<u64, ContainerCleanupError>;
}

/// Trait for cancelling deployments for an environment
///
/// This trait is used to avoid circular dependencies between temps-environments
/// and temps-deployments crates. The DeploymentService in temps-deployments
/// implements this trait.
#[async_trait]
pub trait DeploymentCanceller: Send + Sync {
    /// Cancel all active deployments for a project.
    async fn cancel_all_project_deployments(
        &self,
        project_id: i32,
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>>;

    /// Cancel all active deployments for an environment
    /// Returns the number of deployments cancelled
    async fn cancel_all_environment_deployments(
        &self,
        environment_id: i32,
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>>;
}

/// Removes source/static archives owned by a project before its database rows
/// are deleted.
///
/// The trait lives in `temps-core` so the projects crate can require cleanup
/// without depending on the deployments crate. Implementations must be
/// idempotent: retrying after a partial cleanup must safely continue.
#[async_trait]
pub trait ProjectArchiveCleaner: Send + Sync {
    /// Remove all persisted upload archives known to belong to `project_id`.
    /// A failure must leave the project row intact so callers can retry.
    async fn cleanup_project_archives(
        &self,
        project_id: i32,
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>>;
}

/// An image a project's builds recorded producing: the `image_tag` (and
/// `image_id`, when recorded) of a successful `BuildImageJob`, the same
/// record image retention trusts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectImage {
    pub reference: String,
    pub image_id: Option<String>,
}

/// What deleting a project will try to remove from its runtimes, read while
/// its build records still exist (they go with the project row).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectImagePlan {
    pub project_id: i32,
    /// The project's built images that no other project references.
    pub images: Vec<ProjectImage>,
    /// Runtimes that may hold them: `None` is this host, `Some(id)` a worker
    /// node that built them or ran the project's containers.
    pub nodes: Vec<Option<i32>>,
    /// References kept because a deployment or build of another project
    /// names the same tag or image ID.
    pub shared: Vec<String>,
}

/// What [`ProjectImageCleaner::remove_project_images`] did. Counts are per
/// image per runtime; an image a runtime never had is not counted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct ProjectImageCleanupReport {
    pub removed: u64,
    /// Kept for safety: shared with another project, used by a container,
    /// the tag now naming another image, or refused by Docker.
    pub skipped: u64,
    /// Removals that failed on a runtime that answered.
    pub failed: u64,
    /// Runtimes that could not be asked at all (unreachable node, agent too
    /// old to remove images); their copies stay on that host.
    pub unreachable_runtimes: u64,
}

/// Why the images built for a project could not be read before deleting it.
#[derive(Debug, Error)]
pub enum ProjectImageCleanupError {
    /// The source is the implementation's own error (a database error), kept
    /// whole rather than flattened, behind a box so this crate does not
    /// depend on the database layer.
    #[error(
        "Failed to read the images built for project {project_id} before deleting it: {source}"
    )]
    Plan {
        project_id: i32,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// Removes the images built for a project when it is deleted. Implemented by
/// `temps-deployments`, consumed through the service registry like
/// [`DeploymentContainerCleaner`].
///
/// Best-effort by design: [`Self::remove_project_images`] cannot fail, so an
/// image that cannot be removed never blocks or rolls back a deletion.
/// Anything left behind is labelled with its project and is removed by the
/// nightly sweep of images whose project no longer exists.
#[async_trait]
pub trait ProjectImageCleaner: Send + Sync {
    /// Read the project's built images and the runtimes holding them. Call
    /// before the project row is deleted: the records cascade away with it.
    async fn plan_project_image_cleanup(
        &self,
        project_id: i32,
    ) -> Result<ProjectImagePlan, ProjectImageCleanupError>;

    /// Remove the planned images from each runtime, each only if no
    /// container there was created from it. Logs every image kept or failed.
    async fn remove_project_images(&self, plan: &ProjectImagePlan) -> ProjectImageCleanupReport;
}

/// The result of a [`DeploymentGate`] check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDecision {
    /// The deployment may proceed.
    Allow,
    /// The deployment must not proceed yet. `reason` is an opaque,
    /// human-readable string supplied by whatever implemented the gate —
    /// OSS displays it as-is without needing to understand its meaning.
    Block { reason: String },
}

/// Generic, gate-agnostic extension point for pausing a deployment before it
/// transitions to `PipelineStatus::Running`.
///
/// OSS itself never implements this trait and has no concept of what a gate
/// represents — it just asks "is this deployment allowed to proceed?" and, if
/// not, leaves the deployment in its current (pre-`Running`) status until a
/// [`crate::DeploymentGateRecheckJob`] arrives asking it to check again.
///
/// A plugin (e.g. one implementing manual deployment approvals) registers
/// an implementation via the service registry — `context.register_service(gate)`
/// — only when appropriate (e.g. gated by its own licensing/config check).
/// Core looks it up with an OPTIONAL `get_service::<dyn DeploymentGate>()`
/// (never `require_service`), so a plugin-free binary deploys
/// unconditionally when no gate is registered. This is the ONE extension
/// point for this whole class of feature — a future gate concept (budget
/// limits, security-scan results, etc.) reuses this same trait rather than
/// requiring a new core status or migration; if it needs to combine
/// multiple independent conditions, the plugin's implementation composes
/// them internally before answering.
#[async_trait]
pub trait DeploymentGate: Send + Sync {
    /// Returns [`GateDecision::Allow`] if the deployment may proceed.
    /// Errors are treated as [`GateDecision::Block`] by the caller
    /// (fail-closed) — a broken gate must never fail open.
    async fn check(
        &self,
        project_id: i32,
        environment_name: &str,
        deployment_id: &str,
    ) -> Result<GateDecision, Box<dyn std::error::Error + Send + Sync>>;
}
