//! Git Plugin implementation for the Temps plugin system
//!
//! This plugin provides Git provider management functionality including:
//! - Git provider and connection management
//! - Repository synchronization and listing
//! - OAuth flows for Git providers
//! - Repository preset detection

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_config::ConfigService;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use temps_core::AuditLogger;
use temps_core::{EncryptionService, JobQueue};
use tracing;
use utoipa::{openapi::OpenApi, OpenApi as OpenApiTrait};

use crate::handlers::{self, GitProvidersApiDoc, PublicRepositoriesApiDoc};
use crate::services::{
    git_provider_manager::GitProviderManager, github::GithubAppService,
    repository::RepositoryService,
};

/// Git Plugin for managing Git provider integrations
pub struct GitPlugin;

impl Default for GitPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl GitPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl TempsPlugin for GitPlugin {
    fn name(&self) -> &'static str {
        "git"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Get required dependencies from the service registry
            // Note: We need the concrete database type, not the trait object
            let db = context.require_service::<temps_database::DbConnection>();
            let encryption_service = context.require_service::<EncryptionService>();
            let config_service = context.require_service::<ConfigService>();
            let audit_service = context.require_service::<dyn AuditLogger>();
            let queue_service = context.require_service::<dyn JobQueue>();

            // Create RepositoryService
            let repository_service = Arc::new(RepositoryService::new(db.clone()));
            context.register_service(repository_service.clone());

            // Create GitProviderManager with dependencies
            let git_provider_manager = Arc::new(GitProviderManager::new(
                db.clone(),
                encryption_service.clone(),
                queue_service.clone(),
                config_service.clone(),
            ));
            context.register_service(git_provider_manager.clone());

            // Register as trait for other plugins to use
            let git_provider_trait: Arc<dyn crate::GitProviderManagerTrait> =
                git_provider_manager.clone();
            context.register_service(git_provider_trait);

            // Reset all git provider connections syncing flags to false at startup
            {
                use sea_orm::EntityTrait;
                use temps_entities::git_provider_connections;

                let result = git_provider_connections::Entity::update_many()
                    .col_expr(
                        git_provider_connections::Column::Syncing,
                        sea_orm::sea_query::Expr::value(false),
                    )
                    .exec(db.as_ref())
                    .await;

                match result {
                    Ok(res) => {
                        tracing::debug!(
                            "Reset syncing flag for {} git provider connections",
                            res.rows_affected
                        );
                    }
                    Err(e) => {
                        tracing::error!("Failed to reset git provider syncing flags: {}", e);
                    }
                }
            }

            // Create GithubAppService
            let github_service = Arc::new(GithubAppService::new(
                db.clone(),
                queue_service.clone(),
                git_provider_manager.clone(),
            ));
            context.register_service(github_service.clone());

            // Create cache manager
            let cache_manager = Arc::new(crate::services::cache::GitProviderCacheManager::new());

            // Register the GitAppState for route handlers
            let git_app_state = crate::handlers::types::create_git_app_state(
                repository_service,
                git_provider_manager,
                config_service,
                audit_service,
                github_service,
                cache_manager,
            );
            context.register_plugin_state("git", git_app_state);

            tracing::debug!("Git plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get the GitAppState from plugin context
        let git_app_state = context
            .get_plugin_state::<crate::handlers::types::GitAppState>("git")
            .expect("GitAppState should be available");

        // Configure routes using the existing route configuration
        let router = handlers::configure_routes().with_state(git_app_state);

        Some(PluginRoutes::new(router))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        let mut schema = GitProvidersApiDoc::openapi();
        schema.merge(PublicRepositoriesApiDoc::openapi());
        Some(schema)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    use temps_core::QueueError;
    use temps_core::{Job, JobReceiver};

    // Mock implementations for testing
    #[allow(dead_code)]
    struct MockConfigService;
    #[allow(dead_code)]
    struct MockAuditService;

    #[allow(dead_code)]
    struct MockJobQueue;

    #[async_trait]
    impl JobQueue for MockJobQueue {
        async fn send(&self, _job: Job) -> Result<(), QueueError> {
            Ok(())
        }

        fn subscribe(&self) -> Box<dyn JobReceiver> {
            todo!("Not needed for plugin test")
        }
    }

    #[tokio::test]
    async fn test_git_plugin_name() {
        let git_plugin = GitPlugin::new();
        assert_eq!(git_plugin.name(), "git");
    }

    #[test]
    fn test_git_plugin_openapi_schema() {
        let git_plugin = GitPlugin::new();
        let schema = git_plugin.openapi_schema();
        assert!(schema.is_some(), "Git plugin should provide OpenAPI schema");

        let schema = schema.unwrap();
        // The actual title comes from the GitProvidersApiDoc
        assert!(!schema.info.title.is_empty());
    }

    // Note: Full service registration test would require more complex setup
    // since it depends on the actual database connection and other concrete services.
    // For now, we test that the plugin can be instantiated and provides the expected interface.
}
