//! Trait for resolving integration-sourced environment variables.
//!
//! This trait avoids a circular dependency between `temps-environments` (which
//! owns manual env vars) and `temps-providers` (which owns external-service
//! connection strings). `temps-providers` implements the trait; the providers
//! plugin registers it as a service, and `temps-environments` injects it into
//! its AppState to build the resolved (manual + integration) env-var view.

use async_trait::async_trait;
use std::collections::HashMap;

/// A single integration-sourced env var attached to a linked external service.
#[derive(Debug, Clone)]
pub struct IntegrationEnvVar {
    pub key: String,
    pub value: String,
}

/// Metadata about an external service that contributes env vars to a project.
#[derive(Debug, Clone)]
pub struct IntegrationServiceInfo {
    pub service_id: i32,
    pub service_name: String,
    pub service_type: String,
    pub service_slug: Option<String>,
    pub service_updated_at: String,
}

/// One linked integration and the env vars it produces for a given project.
#[derive(Debug, Clone)]
pub struct ProjectIntegrationEnvVars {
    pub service: IntegrationServiceInfo,
    pub variables: Vec<IntegrationEnvVar>,
}

/// Resolves integration-sourced environment variables for a project.
///
/// Implementations should return every env var contributed by every external
/// service linked to the project. The caller (env-var service) merges these
/// with manual env vars, tracks conflicts, and tags each entry with its
/// source before returning the resolved view to the UI.
#[async_trait]
pub trait ProjectEnvVarsProvider: Send + Sync {
    /// Collect env vars contributed by every external service linked to the
    /// given project. Returns one entry per linked service (even if that
    /// service produced zero variables) so the UI can list integrations that
    /// are attached but currently empty.
    ///
    /// When `environment_id` is `Some`, the returned env vars are the
    /// side-effect-free preview of what a deployment in that environment
    /// would receive (per-tenant DB names, bucket names, etc.). When
    /// `None`, the static admin-level values are returned — same legacy
    /// behavior as before this argument existed.
    async fn get_project_integration_env_vars(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<Vec<ProjectIntegrationEnvVars>, Box<dyn std::error::Error + Send + Sync>>;
}

/// Flatten the per-service shape into a key-indexed map for quick lookups.
///
/// When two services produce the same key, the service processed later wins —
/// callers that care about conflicts should iterate the original vec instead.
pub fn flatten_integration_env_vars(
    services: &[ProjectIntegrationEnvVars],
) -> HashMap<String, (IntegrationServiceInfo, String)> {
    let mut out = HashMap::new();
    for svc in services {
        for var in &svc.variables {
            out.insert(var.key.clone(), (svc.service.clone(), var.value.clone()));
        }
    }
    out
}
