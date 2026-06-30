//! `MonitoringPlugin` — registers `AlarmService` in the shared service
//! registry and wires the alarms HTTP routes (ADR-025 Phase 1).
//!
//! ## Wiring decision
//!
//! Previously `AlarmService` was constructed ad-hoc in `console.rs` and passed
//! directly to the background loops (outage detection, container health, alert
//! evaluator). It was never placed in the service registry, so handlers could
//! not reach it.
//!
//! `MonitoringPlugin::register_services` now creates the **single**
//! `Arc<AlarmService>` and registers it in the service registry.
//! `console.rs` is updated to call
//! `service_context.get_service::<AlarmService>()` instead of constructing
//! its own instance, guaranteeing one object is shared between the HTTP
//! handlers and the background monitoring loops.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::alarm_service::AlarmService;
use crate::handlers::{AlarmAppState, AlarmsApiDoc};

/// Plugin that exposes the existing `AlarmService` over HTTP.
pub struct MonitoringPlugin;

impl MonitoringPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MonitoringPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for MonitoringPlugin {
    fn name(&self) -> &'static str {
        "monitoring"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<sea_orm::DatabaseConnection>();

            // NotificationService is required; the plugin panics at startup
            // (via `require_service`) when it hasn't been registered yet.
            let notification_service =
                context.require_service::<dyn temps_core::notifications::NotificationService>();
            let job_queue = context.require_service::<dyn temps_core::JobQueue>();

            let alarm_service = Arc::new(AlarmService::new(db, notification_service, job_queue));

            // Register so both:
            //   1. Background loops (outage/container-health/evaluator) can get it
            //      via `service_context.get_service::<AlarmService>()`.
            //   2. `configure_routes` below can require it.
            context.register_service(alarm_service);

            tracing::debug!("MonitoringPlugin: AlarmService registered");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let alarm_service = context.require_service::<AlarmService>();
        let audit_service = context.require_service::<dyn temps_core::AuditLogger>();

        let app_state = Arc::new(AlarmAppState {
            alarm_service,
            audit_service,
        });

        let router = crate::handlers::alarm_handlers::configure_routes().with_state(app_state);
        Some(PluginRoutes::new(router))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<AlarmsApiDoc as OpenApiTrait>::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_monitoring_plugin_name() {
        assert_eq!(MonitoringPlugin::new().name(), "monitoring");
    }

    #[test]
    fn test_monitoring_plugin_default() {
        let p = MonitoringPlugin;
        assert_eq!(p.name(), "monitoring");
    }
}
