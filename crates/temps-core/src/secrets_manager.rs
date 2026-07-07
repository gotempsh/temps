//! OSS extension point for deploy-time secrets injection.
//!
//! This trait avoids a circular dependency between `temps-deployments` (which
//! orchestrates workflow planning) and any future EE crate that implements a
//! secrets-manager integration. `temps-deployments` depends on `temps-core`
//! (not on the EE crate); the EE plugin registers an implementation and
//! `DeploymentsPlugin` retrieves it via the optional service registry.
//!
//! The design is modeled on [`crate::env_vars_provider::ProjectEnvVarsProvider`]:
//! one file per trait, same error-boxing style, same file-per-trait convention.

use async_trait::async_trait;
use std::collections::HashMap;

/// OSS extension point for deployer-time secret injection.
///
/// OSS never implements this trait. An EE plugin registers a concrete
/// implementation via `context.register_service(resolver)` only when the
/// `SecretsManagerIntegrations` feature is licensed.
/// `DeploymentsPlugin` retrieves it with
/// `context.get_service::<dyn SecretsManagerResolver>()`
/// (never `require_service`) inside `initialize_plugin_services`, so the OSS
/// binary is a strict no-op when nothing is registered.
///
/// # Fail-closed contract
///
/// Errors returned here are **fail-closed**: `WorkflowPlanner` treats any
/// `Err` as a hard deployment failure and surfaces the error in the
/// deployment log. An implementation MUST return `Err` when a bound secret
/// cannot be retrieved rather than returning an empty or placeholder value.
/// "Fail-open" behaviour (silently omitting a binding, returning an empty
/// string, or falling back to a stale cached value) is explicitly prohibited.
#[async_trait]
pub trait SecretsManagerResolver: Send + Sync {
    /// Resolve every secret bound to this project+environment combination
    /// and return them as a flat `env_key → plaintext_value` map.
    ///
    /// Unlike [`crate::env_vars_provider::ProjectEnvVarsProvider::get_project_integration_env_vars`]
    /// which accepts `Option<i32>` for `environment_id`, this method always
    /// receives a concrete environment: deploy-time injection always operates
    /// in a specific environment.
    ///
    /// # Fail-closed contract
    ///
    /// If any single binding cannot be resolved (provider unreachable,
    /// path not found, authentication failure), return `Err`. The caller
    /// (`WorkflowPlanner::gather_environment_variables`) will surface the
    /// error in the deployment log and abort. Never return `Ok` with a
    /// partial map that silently omits a binding.
    async fn resolve_secrets_for_deployment(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<HashMap<String, String>, Box<dyn std::error::Error + Send + Sync>>;
}
