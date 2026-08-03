//! Feature-flag plugin registration.
//!
//! No containers, no background tasks, no external service: flags are rows in
//! the control-plane database, so this plugin registers a service and some
//! routes and is done.

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

use crate::handlers::{configure_routes, FlagsApiDoc, FlagsAppState};
use crate::services::FlagService;

pub struct FlagsPlugin;

impl FlagsPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FlagsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for FlagsPlugin {
    fn name(&self) -> &'static str {
        "flags"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<DatabaseConnection>();
            context.register_service(Arc::new(FlagService::new(db)));

            debug!("Feature flag plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let flag_service = context.require_service::<FlagService>();
        let audit_service = context.require_service::<dyn AuditLogger>();
        // Optional team-based access checker (present only when a plugin
        // registers one); `None` in plain OSS, where the guard is a no-op.
        let project_access_checker = context.get_service::<dyn temps_core::ProjectAccessChecker>();

        let app_state = Arc::new(FlagsAppState {
            flag_service,
            audit_service,
            project_access_checker,
        });

        Some(PluginRoutes::new(configure_routes().with_state(app_state)))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<FlagsApiDoc as OpenApiTrait>::openapi())
    }
}
