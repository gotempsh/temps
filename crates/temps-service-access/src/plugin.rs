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

pub struct ServiceAccessPlugin;

impl ServiceAccessPlugin {
    pub fn new() -> Self {
        Self
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
            let db = context.require_service::<DatabaseConnection>();
            context.register_service(Arc::new(ServiceAccessService::new(
                db,
                context.require_service::<temps_core::EncryptionService>(),
                Arc::new(crate::broker::UnavailableTokenlessBroker),
            )));

            debug!("Sandbox service access plugin services registered successfully");
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
