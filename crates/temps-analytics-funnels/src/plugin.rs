// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use tracing::debug;

/// Funnels analytics plugin
pub struct FunnelsPlugin;

impl Default for FunnelsPlugin {
    fn default() -> Self {
        Self
    }
}

impl TempsPlugin for FunnelsPlugin {
    fn name(&self) -> &'static str {
        "funnels"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            debug!("Registering funnels services");

            let db = context.require_service::<sea_orm::DatabaseConnection>();

            let funnel_service = Arc::new(crate::services::FunnelService::new(db.clone()));
            context.register_service(funnel_service);
            context.register_service(Arc::new(crate::goals::AnalyticsGoalService::new(db)));

            debug!("Funnels services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let funnel_service = context.get_service::<crate::services::FunnelService>()?;

        let project_access_checker = context.get_service::<dyn temps_core::ProjectAccessChecker>();
        let goals_state = Arc::new(crate::goals::AnalyticsGoalsState {
            service: context.require_service::<crate::goals::AnalyticsGoalService>(),
            audit: context.require_service::<dyn temps_core::AuditLogger>(),
            project_access_checker: project_access_checker.clone(),
        });
        let routes = crate::handlers::handler::configure_routes()
            .with_state(Arc::new(crate::handlers::types::AppState {
                funnel_service,
                project_access_checker,
            }))
            .merge(crate::goals::routes().with_state(goals_state));

        Some(PluginRoutes::new(routes))
    }

    fn openapi_schema(&self) -> Option<utoipa::openapi::OpenApi> {
        let mut doc = <crate::handlers::handler::FunnelApiDoc as utoipa::OpenApi>::openapi();
        doc.merge(<crate::goals::AnalyticsGoalsApiDoc as utoipa::OpenApi>::openapi());
        Some(doc)
    }
}
