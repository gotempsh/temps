//! Deployment-related traits and types

use async_trait::async_trait;

/// Trait for cancelling deployments for an environment
///
/// This trait is used to avoid circular dependencies between temps-environments
/// and temps-deployments crates. The DeploymentService in temps-deployments
/// implements this trait.
#[async_trait]
pub trait DeploymentCanceller: Send + Sync {
    /// Cancel all active deployments for an environment
    /// Returns the number of deployments cancelled
    async fn cancel_all_environment_deployments(
        &self,
        environment_id: i32,
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>>;
}
