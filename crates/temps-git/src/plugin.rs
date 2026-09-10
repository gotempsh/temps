// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
    connection_health::ConnectionHealthService, git_provider_manager::GitProviderManager,
    github::GithubAppService, repository::RepositoryService,
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
            context.register_service(git_provider_trait.clone());

            // PR preview commenter — turns deployment lifecycle events into
            // sticky PR/MR comments. Failures never block deploys (see
            // GitPrCommenter::upsert_preview_comment).
            let pr_commenter: Arc<dyn crate::PrCommenter> =
                Arc::new(crate::GitPrCommenter::new(db.clone(), git_provider_trait));
            context.register_service(pr_commenter.clone());

            // Background listener: subscribes to DeploymentCreated /
            // DeploymentSucceeded / DeploymentFailed and upserts the
            // sticky comment for each phase. Self-contained — doesn't
            // require any wiring in the deployment crate beyond the
            // existing event broadcast.
            let pr_comment_listener = Arc::new(
                crate::services::pr_comment_listener::PrCommentListener::new(
                    pr_commenter,
                    db.clone(),
                    queue_service.clone(),
                ),
            );
            context.register_service(pr_comment_listener.clone());

            tokio::spawn({
                let listener = pr_comment_listener.clone();
                async move {
                    if let Err(e) = listener.start().await {
                        tracing::error!("Failed to start PR comment listener: {}", e);
                    }
                }
            });

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

            let console_base_url = {
                let server_config = config_service.get_server_config();
                format!("http://{}", server_config.console_address)
            };

            // AlarmService isn't registered yet at this point (Monitoring
            // registers after Git) — wired in via `initialize_plugin_services`
            // once every plugin's Phase 1 has completed.
            let connection_health_service = Arc::new(ConnectionHealthService::new(
                db.clone(),
                git_provider_manager.clone(),
                github_service.clone(),
                console_base_url,
            ));
            context.register_service(connection_health_service.clone());

            // Daily git connection health sweep. The first tick fires
            // immediately so freshly-started servers get a baseline before the
            // 24h window elapses.
            let sweep_service = connection_health_service.clone();
            tokio::spawn(async move {
                // Give the rest of the platform a moment to finish booting
                // (DB pools warm, notification providers loaded). A 5-minute
                // delay is imperceptible at a 24h cadence.
                tokio::time::sleep(std::time::Duration::from_secs(300)).await;

                let mut tick = tokio::time::interval(std::time::Duration::from_secs(24 * 60 * 60));
                loop {
                    tick.tick().await;
                    match sweep_service.run_health_checks_for_all().await {
                        Ok(outcomes) => {
                            tracing::info!(
                                checked = outcomes.len(),
                                transitions = outcomes.iter().filter(|o| o.transitioned).count(),
                                "Daily git connection health sweep complete"
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                error = %e,
                                "Daily git connection health sweep failed; will retry next tick"
                            );
                        }
                    }
                }
            });

            // Register the GitAppState for route handlers
            let telemetry = context
                .get_service::<dyn temps_core::telemetry::TelemetryReporter>()
                .unwrap_or_else(|| Arc::new(temps_core::telemetry::NoopTelemetryReporter));

            // Central sensitive-action policy (MFA step-up), used to gate
            // destructive git provider and connection operations like delete.
            let sensitive_action_authorizer =
                context.require_service::<dyn temps_core::SensitiveActionAuthorizer>();

            let git_app_state = crate::handlers::types::create_git_app_state(
                repository_service,
                git_provider_manager,
                config_service,
                audit_service,
                github_service,
                cache_manager,
                connection_health_service,
                telemetry,
                sensitive_action_authorizer,
            );
            context.register_plugin_state("git", git_app_state);

            tracing::debug!("Git plugin services registered successfully");
            Ok(())
        })
    }

    fn initialize_plugin_services<'a>(
        &'a self,
        context: &'a PluginContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(alarm_service) =
                context.get_service::<temps_monitoring::alarm_service::AlarmService>()
            {
                let connection_health_service =
                    context.require_service::<ConnectionHealthService>();
                connection_health_service.set_alarm_service(alarm_service);
                tracing::debug!("AlarmService wired into git connection health service");
            } else {
                tracing::warn!(
                    "AlarmService not available - git connection health alarms will be skipped"
                );
            }
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get the GitAppState from plugin context
        let git_app_state = context
            .get_plugin_state::<crate::handlers::types::GitAppState>("git")
            .expect("GitAppState should be available");

        // Rebind the authorizer here rather than trust the one captured in
        // `register_services`: an EE/custom `SensitiveActionAuthorizer` may
        // be registered by a plugin later in registration order, and
        // last-write-wins service registration means the earliest-registered
        // instance otherwise wins silently. `configure_routes` runs only
        // after every plugin's `register_services` has completed, so
        // re-resolving here always sees the final policy — same pattern as
        // AuthPlugin's `with_sensitive_action_authorizer`.
        let git_app_state = Arc::new(crate::handlers::types::GitAppState {
            sensitive_action_authorizer: context
                .require_service::<dyn temps_core::SensitiveActionAuthorizer>(),
            ..(*git_app_state).clone()
        });

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
