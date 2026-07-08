//! Plugin wiring for temps-observability.
//!
//! Registers a single `ObservabilityState` and exposes the merged
//! `/projects/{id}/observe/events*` route tree under the authenticated
//! API surface. No public/webhook routes.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::Router;
use sea_orm::DatabaseConnection;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use tracing::debug;
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::handlers::{configure_observability_routes, ObservabilityApiDoc, ObservabilityState};
use crate::service::ObservabilityService;

pub struct ObservabilityPlugin;

impl ObservabilityPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ObservabilityPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for ObservabilityPlugin {
    fn name(&self) -> &'static str {
        "observability"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<DatabaseConnection>();
            let service = Arc::new(ObservabilityService::new(db));

            context.register_service(service);

            debug!("Observability plugin services registered");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let service = context.require_service::<ObservabilityService>();
        let project_access_checker = context.get_service::<dyn temps_core::ProjectAccessChecker>();
        let state = Arc::new(ObservabilityState {
            service,
            project_access_checker,
        });
        let router: Router = configure_observability_routes().with_state(state);
        Some(PluginRoutes::new(router))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<ObservabilityApiDoc as OpenApiTrait>::openapi())
    }
}
