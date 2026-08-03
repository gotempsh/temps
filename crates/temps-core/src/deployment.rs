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
