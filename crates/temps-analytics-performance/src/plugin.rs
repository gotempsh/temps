// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::handlers::{configure_public_routes, configure_routes, AppState};
use crate::services::service::PerformanceService;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use tracing::debug;

/// Performance analytics plugin
pub struct PerformancePlugin;

impl Default for PerformancePlugin {
    fn default() -> Self {
        Self
    }
}

impl TempsPlugin for PerformancePlugin {
    fn name(&self) -> &'static str {
        "performance"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            debug!("Registering performance services");

            let db = context.require_service::<sea_orm::DatabaseConnection>();

            let performance_service = Arc::new(PerformanceService::new(db));
            context.register_service(performance_service);

            debug!("Performance services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let performance_service = context.require_service::<PerformanceService>();
        let route_table = context.require_service::<temps_routes::CachedPeerTable>();
        let ip_address_service = context.require_service::<temps_geo::IpAddressService>();
        let project_access_checker = context.get_service::<dyn temps_core::ProjectAccessChecker>();

        let routes = configure_routes().with_state(Arc::new(AppState {
            performance_service,
            route_table,
            ip_address_service,
            project_access_checker,
            ingest_key_service: context
                .require_service::<temps_analytics::AnalyticsIngestKeyService>(),
            ingest_rate_limiter: context
                .require_service::<temps_analytics::AnalyticsIngestRateLimiter>(),
        }));

        Some(PluginRoutes::new(routes))
    }

    fn configure_public_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let performance_service = context.require_service::<PerformanceService>();
        let route_table = context.require_service::<temps_routes::CachedPeerTable>();
        let ip_address_service = context.require_service::<temps_geo::IpAddressService>();

        // Public ingest routes resolve project/environment/deployment from the
        // route table (by Host header) or from an ADR-040 ingest key, never
        // from a caller-supplied project_id, so there is no project scope to
        // check here.
        let routes = configure_public_routes().with_state(Arc::new(AppState {
            performance_service,
            route_table,
            ip_address_service,
            project_access_checker: None,
            ingest_key_service: context
                .require_service::<temps_analytics::AnalyticsIngestKeyService>(),
            ingest_rate_limiter: context
                .require_service::<temps_analytics::AnalyticsIngestRateLimiter>(),
        }));

        Some(PluginRoutes::new(routes))
    }

    fn openapi_schema(&self) -> Option<utoipa::openapi::OpenApi> {
        Some(<crate::handlers::PerformanceApiDoc as utoipa::OpenApi>::openapi())
    }
}
