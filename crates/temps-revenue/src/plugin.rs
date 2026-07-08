//! Plugin wiring for temps-revenue.
//!
//! Registers three services and exposes two route trees:
//!   * Authenticated management routes under `/api/...`
//!   * Unauthenticated webhook ingestion routes under `/api/webhooks/...`

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::Router;
use sea_orm::DatabaseConnection;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use temps_core::{AuditLogger, EncryptionService};
use tracing::debug;
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::handlers::{
    configure_management_routes, configure_public_routes, ManagementState, PublicState,
    RevenueApiDoc,
};
use crate::providers::ProviderRegistry;
use crate::service::{
    RevenueAnalyticsService, RevenueImportService, RevenueIngestionService,
    RevenueIntegrationService,
};

pub struct RevenuePlugin;

impl RevenuePlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RevenuePlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for RevenuePlugin {
    fn name(&self) -> &'static str {
        "revenue"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<DatabaseConnection>();
            let encryption = context.require_service::<EncryptionService>();
            let audit = context.require_service::<dyn AuditLogger>();

            let providers = ProviderRegistry::default_registry();

            let integrations = Arc::new(RevenueIntegrationService::new(
                db.clone(),
                encryption,
                providers.clone(),
            ));
            let analytics = Arc::new(RevenueAnalyticsService::new(db.clone()));
            let ingestion = Arc::new(RevenueIngestionService::new(
                db.clone(),
                integrations.clone(),
                providers,
            ));
            let import = Arc::new(RevenueImportService::new(db.clone(), integrations.clone()));

            let management_state = Arc::new(ManagementState::new(
                integrations.clone(),
                analytics.clone(),
                import.clone(),
                audit,
                None,
            ));
            let public_state = Arc::new(PublicState::new(ingestion.clone()));

            context.register_service(integrations);
            context.register_service(analytics);
            context.register_service(ingestion);
            context.register_service(import);
            context.register_service(management_state);
            context.register_service(public_state);

            debug!("Revenue plugin services registered");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let integrations = context.require_service::<RevenueIntegrationService>();
        let analytics = context.require_service::<RevenueAnalyticsService>();
        let import = context.require_service::<RevenueImportService>();
        let audit = context.require_service::<dyn AuditLogger>();
        let project_access_checker = context.get_service::<dyn temps_core::ProjectAccessChecker>();
        let state = Arc::new(ManagementState::new(
            integrations,
            analytics,
            import,
            audit,
            project_access_checker,
        ));
        let router: Router = configure_management_routes().with_state(state);
        Some(PluginRoutes::new(router))
    }

    fn configure_public_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let state = context.require_service::<PublicState>();
        let router: Router = configure_public_routes().with_state(state);
        Some(PluginRoutes::new(router))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<RevenueApiDoc as OpenApiTrait>::openapi())
    }
}
