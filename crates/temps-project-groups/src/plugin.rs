// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use sea_orm::DatabaseConnection;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

/// Clear the `secrets` column of non-running instantiations that finished more
/// than `retention` ago. Secrets are only needed while steps are running; once
/// an instantiation succeeds or fails they serve no purpose and holding them
/// indefinitely is a liability.
async fn purge_stale_instantiation_secrets(
    db: Arc<DatabaseConnection>,
    retention: std::time::Duration,
) {
    use sea_orm::ConnectionTrait;
    let interval_secs = retention.as_secs();
    match db
        .execute(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!(
                "UPDATE template_instantiations \
                 SET secrets = NULL, updated_at = NOW() \
                 WHERE secrets IS NOT NULL \
                   AND status NOT IN ('pending', 'running') \
                   AND updated_at < NOW() - INTERVAL '{interval_secs} seconds'"
            ),
        ))
        .await
    {
        Ok(result) => {
            let count = result.rows_affected();
            if count > 0 {
                tracing::info!(count, "Purged secrets from stale instantiations");
            }
        }
        Err(error) => {
            tracing::warn!(%error, "Failed to purge stale instantiation secrets");
        }
    }
}

use crate::env_vars::GroupEnvVarService;
use crate::handlers::{router, ProjectGroupsApiDoc, ProjectGroupsAppState};
use crate::instantiation::InstantiationEngine;
use crate::instantiation_handlers::{
    router as instantiation_router, InstantiationApiDoc, InstantiationAppState,
};
use crate::repository::GroupRepositoryService;
use crate::routes::GroupRouteService;
use crate::service::ProjectGroupService;

pub struct ProjectGroupsPlugin;

impl ProjectGroupsPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ProjectGroupsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for ProjectGroupsPlugin {
    fn name(&self) -> &'static str {
        "project-groups"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<DatabaseConnection>();
            let encryption = context.require_service::<temps_core::EncryptionService>();
            context.register_service(Arc::new(ProjectGroupService::new(db.clone())));
            context.register_service(Arc::new(GroupEnvVarService::new(db, encryption)));
            tracing::debug!("project-groups: services registered");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Resolved here rather than in `register_services`: the access
        // checker, project service and git provider manager are registered by
        // other plugins, and every plugin's
        // services exist by the time routes are configured.
        let repository = Arc::new(GroupRepositoryService::new(
            context.require_service::<sea_orm::DatabaseConnection>(),
            context.require_service::<temps_projects::ProjectService>(),
            context.require_service::<temps_git::GitProviderManager>(),
        ));
        let db = context.require_service::<sea_orm::DatabaseConnection>();
        let projects = context.require_service::<temps_projects::ProjectService>();
        let git = context.require_service::<temps_git::GitProviderManager>();
        let audit = context.require_service::<dyn temps_core::AuditLogger>();
        let external_services =
            context.require_service::<temps_providers::ExternalServiceManager>();
        let sensitive_action_authorizer =
            context.require_service::<dyn temps_core::SensitiveActionAuthorizer>();
        let state = Arc::new(ProjectGroupsAppState {
            service: context.require_service::<ProjectGroupService>(),
            env_vars: context.require_service::<GroupEnvVarService>(),
            repository,
            routes: Arc::new(GroupRouteService::new(db.clone())),
            audit: audit.clone(),
            project_access_checker: context.get_service::<dyn temps_core::ProjectAccessChecker>(),
        });

        let creator = temps_projects::template_creation::TemplateServiceCreator {
            project_service: projects.clone(),
            config_service: context.require_service::<temps_config::ConfigService>(),
            public_hostname_resolver: context
                .get_service::<dyn temps_core::PublicHostnameResolver>()
                .unwrap_or_else(|| {
                    Arc::new(temps_core::StandardHostnameResolver)
                        as Arc<dyn temps_core::PublicHostnameResolver>
                }),
            audit_service: audit.clone(),
            telemetry: context
                .get_service::<dyn temps_core::telemetry::TelemetryReporter>()
                .unwrap_or_else(|| Arc::new(temps_core::telemetry::NoopTelemetryReporter)),
        };
        let engine = InstantiationEngine::new(
            db.clone(),
            context.require_service::<temps_core::EncryptionService>(),
            state.service.clone(),
            state.env_vars.clone(),
            state.repository.clone(),
            state.routes.clone(),
            creator,
            external_services.clone(),
            git,
            sensitive_action_authorizer.clone(),
            audit.clone(),
        );
        let instantiation_state = Arc::new(InstantiationAppState {
            groups: state.clone(),
            engine,
            templates: context.require_service::<temps_core::templates::TemplateService>(),
            projects,
            external_services,
            sensitive_action_authorizer,
        });
        // Periodically clear secrets from non-running instantiations that are
        // more than 7 days old. They are only needed while steps run; keeping
        // them longer is a liability. Runs hourly; any transient failure is
        // logged and the next tick retries.
        let purge_db = db.clone();
        tokio::spawn(async move {
            let retention = std::time::Duration::from_secs(7 * 24 * 3600);
            let interval = std::time::Duration::from_secs(3600);
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                purge_stale_instantiation_secrets(purge_db.clone(), retention).await;
            }
        });

        Some(PluginRoutes::new(
            router(state).merge(instantiation_router(instantiation_state)),
        ))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        let mut schema = <ProjectGroupsApiDoc as OpenApiTrait>::openapi();
        schema.merge(<InstantiationApiDoc as OpenApiTrait>::openapi());
        Some(schema)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_name_is_stable() {
        assert_eq!(ProjectGroupsPlugin::new().name(), "project-groups");
    }
}
