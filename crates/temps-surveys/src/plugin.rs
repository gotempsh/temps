// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Survey plugin registration.
//!
//! No containers and no background tasks: definitions are control-plane rows
//! and answers are analytics events, so this plugin registers a service and
//! its routes.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use sea_orm::DatabaseConnection;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use temps_core::AuditLogger;
use temps_flags::handlers::rate_limit::SlotRateLimiter;
use temps_flags::FlagService;
use tracing::debug;
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::handlers::{
    configure_public_routes, configure_routes, PublicSurveysState, SurveysApiDoc, SurveysAppState,
};
use crate::services::SurveyService;

pub struct SurveysPlugin;

impl SurveysPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SurveysPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for SurveysPlugin {
    fn name(&self) -> &'static str {
        "surveys"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<DatabaseConnection>();
            context.register_service(Arc::new(SurveyService::new(db)));
            debug!("Survey plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let state = Arc::new(SurveysAppState {
            survey_service: context.require_service::<SurveyService>(),
            audit_service: context.require_service::<dyn AuditLogger>(),
            project_access_checker: context.get_service::<dyn temps_core::ProjectAccessChecker>(),
        });
        Some(PluginRoutes::new(configure_routes().with_state(state)))
    }

    /// `/api/_temps/surveys`: called by browsers on customers' sites, tenant
    /// resolved from `Host`.
    fn configure_public_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let state = Arc::new(PublicSurveysState {
            survey_service: context.require_service::<SurveyService>(),
            // Survey flag conditions evaluate against the flags plugin's
            // in-memory sets; the flags plugin registers before this one.
            flag_service: context.require_service::<FlagService>(),
            route_table: context.require_service::<temps_routes::CachedPeerTable>(),
            geoip: context.get_service::<temps_geo::GeoIpService>(),
            project_limiter: SlotRateLimiter::new(),
            address_limiter: SlotRateLimiter::new(),
        });
        Some(PluginRoutes::new(
            configure_public_routes().with_state(state),
        ))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<SurveysApiDoc as OpenApiTrait>::openapi())
    }
}
