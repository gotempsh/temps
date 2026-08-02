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
        _context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // ObservabilityService is constructed in `configure_routes` (which
            // runs after EVERY plugin's registration phase) because it needs
            // ProxyLogService and the OTel storage backend, and ProxyPlugin
            // registers AFTER this plugin. Registering a half-wired service
            // here would freeze the Postgres-only read path that broke the
            // Observe page on ClickHouse-enabled servers.
            debug!("Observability plugin: services resolved at route configuration");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let db = context.require_service::<DatabaseConnection>();
        // Backend-dispatching readers registered by ProxyPlugin / OtelPlugin.
        // Both select ClickHouse vs TimescaleDB at startup from
        // `TEMPS_CLICKHOUSE_*`; the Observe feed must read through them so it
        // sees the same store the ingest paths write to.
        let proxy_logs =
            context.require_service::<temps_proxy::service::proxy_log_service::ProxyLogService>();
        let otel = context.require_service::<dyn temps_otel::storage::OtelStorage>();
        let service = Arc::new(ObservabilityService::new(db, proxy_logs, otel));
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
