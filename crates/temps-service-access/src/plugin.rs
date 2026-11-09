// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use sea_orm::DatabaseConnection;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use temps_core::AuditLogger;
use tracing::debug;
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::handlers::{configure_routes, AccessAppState, ServiceAccessApiDoc};
use crate::service::ServiceAccessService;
use crate::{
    coordinator::ServiceAccessCoordinator, reconciler::ServiceAccessReconciler,
    transport::WorkerTransport,
};

pub struct ServiceAccessPlugin;

impl ServiceAccessPlugin {
    pub fn new() -> Self {
        Self
    }
    pub(crate) fn register_with_credentials(
        &self,
        context: &ServiceRegistrationContext,
        github: Arc<dyn crate::github::GithubCredentialResolver>,
    ) {
        let db = context.require_service::<DatabaseConnection>();
        let encryption = context.require_service::<temps_core::EncryptionService>();
        let service = Arc::new(ServiceAccessService::new(
            Arc::clone(&db),
            Arc::clone(&encryption),
            Arc::new(crate::broker::EmbeddedTokenlessBroker),
        ));
        let transport = Arc::new(WorkerTransport::new(
            context.require_service::<dyn temps_sandbox_providers::SandboxProvider>(),
        ));
        context.register_service(Arc::new(ServiceAccessCoordinator::new(
            db,
            encryption,
            context.require_service::<dyn AuditLogger>(),
            Arc::clone(&service),
            transport,
            context.get_service::<dyn temps_core::ProjectAccessChecker>(),
            github,
        )));
        context.register_service(service);
        context.register_service(Arc::new(ServiceAccessReconciler::default()));

        debug!("Sandbox service access plugin services registered successfully");
    }
}

impl Default for ServiceAccessPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for ServiceAccessPlugin {
    fn name(&self) -> &'static str {
        "service-access"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let github = Arc::new(crate::github::ProjectGithubCredentials::new(
                context.require_service::<dyn temps_git::services::git_provider_manager_trait::GitProviderManagerTrait>(),
            ));
            self.register_with_credentials(context, github);
            Ok(())
        })
    }

    fn initialize_plugin_services<'a>(
        &'a self,
        context: &'a PluginContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            context
                .require_service::<ServiceAccessReconciler>()
                .start(context.require_service::<ServiceAccessCoordinator>());
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let service = context.require_service::<ServiceAccessService>();
        let audit_service = context.require_service::<dyn AuditLogger>();
        // Optional team-based access checker (present only when a plugin
        // registers one); `None` in plain OSS, where the guard is a no-op.
        let project_access_checker = context.get_service::<dyn temps_core::ProjectAccessChecker>();

        let app_state = Arc::new(AccessAppState {
            coordinator: context.require_service::<ServiceAccessCoordinator>(),
            service,
            audit_service,
            project_access_checker,
        });

        Some(PluginRoutes::new(configure_routes().with_state(app_state)))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<ServiceAccessApiDoc as OpenApiTrait>::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openapi_schema_exposes_all_service_access_operations() {
        let schema = ServiceAccessPlugin::new()
            .openapi_schema()
            .expect("service-access plugin must expose an OpenAPI schema");
        for (path, method, operation_id) in [
            (
                "/projects/{project_id}/sandboxes/{sandbox_id}/service-grants/approve",
                "POST",
                "approve_service_grant",
            ),
            (
                "/projects/{project_id}/access-services/{service_id}",
                "GET",
                "get_access_service",
            ),
            (
                "/projects/{project_id}/service-access/capability",
                "GET",
                "access_capability",
            ),
            (
                "/projects/{project_id}/access-services",
                "GET",
                "list_access_services",
            ),
            (
                "/projects/{project_id}/access-services",
                "POST",
                "create_access_service",
            ),
            (
                "/projects/{project_id}/access-services/{service_id}",
                "PUT",
                "update_access_service",
            ),
            (
                "/projects/{project_id}/sandboxes/{sandbox_id}/service-grants",
                "GET",
                "list_service_grants",
            ),
            (
                "/projects/{project_id}/sandboxes/{sandbox_id}/service-grants",
                "POST",
                "request_service_grant",
            ),
            (
                "/projects/{project_id}/sandboxes/{sandbox_id}/service-grants/{grant_id}/revoke",
                "POST",
                "revoke_service_grant",
            ),
        ] {
            let item = schema
                .paths
                .paths
                .get(path)
                .unwrap_or_else(|| panic!("missing OpenAPI path: {path}"));
            let operation = match method {
                "GET" => item.get.as_ref(),
                "POST" => item.post.as_ref(),
                "PUT" => item.put.as_ref(),
                _ => panic!("unsupported test method: {method}"),
            }
            .unwrap_or_else(|| panic!("missing OpenAPI operation: {method} {path}"));
            assert_eq!(
                operation.operation_id.as_deref(),
                Some(operation_id),
                "{method} {path}"
            );
        }
    }
}
