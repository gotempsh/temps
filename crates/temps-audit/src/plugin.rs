use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use temps_core::{AuditLogger, AuditLoggerSlot};
use utoipa::OpenApi;

use crate::{handlers, AuditService};

/// Audit Plugin for managing audit logs and user action tracking
pub struct AuditPlugin;

impl AuditPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AuditPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for AuditPlugin {
    fn name(&self) -> &'static str {
        "audit"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Get required dependencies from the service registry
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let ip_address_service = context.require_service::<temps_geo::IpAddressService>();

            // Create AuditService
            let audit_service = Arc::new(AuditService::new(db.clone(), ip_address_service.clone()));
            context.register_service(audit_service.clone());

            // At the recorder's hard ceiling of 17 rows/minute, at most 1,020
            // permission-denial rows are created per hour. The bounded 2,048-row
            // hourly prune therefore cannot be outrun by allowed production
            // writes, without ever issuing an unbounded delete transaction.
            let retention_service = audit_service.clone();
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(
                    crate::services::audit_service::PERMISSION_DENIED_PRUNE_INTERVAL,
                );
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                ticker.tick().await;
                loop {
                    ticker.tick().await;
                    let cutoff = chrono::Utc::now()
                        - chrono::Duration::days(
                            crate::services::audit_service::PERMISSION_DENIED_RETENTION_DAYS,
                        );
                    match retention_service
                        .prune_permission_denied_before(
                            cutoff,
                            crate::services::audit_service::PERMISSION_DENIED_PRUNE_BATCH_SIZE,
                        )
                        .await
                    {
                        Ok(rows_deleted) => tracing::debug!(
                            rows_deleted,
                            retention_days =
                                crate::services::audit_service::PERMISSION_DENIED_RETENTION_DAYS,
                            "pruned expired permission-denial audit rows"
                        ),
                        Err(error) => tracing::warn!(
                            error = %error,
                            retention_days = crate::services::audit_service::PERMISSION_DENIED_RETENTION_DAYS,
                            batch_size = crate::services::audit_service::PERMISSION_DENIED_PRUNE_BATCH_SIZE,
                            "permission-denial audit retention pass failed; will retry"
                        ),
                    }
                }
            });

            let initial_logger: Arc<dyn AuditLogger> = audit_service.clone();
            let audit_slot = Arc::new(AuditLoggerSlot::new(initial_logger));
            context.register_service(audit_slot.clone());
            let audit_trait: Arc<dyn AuditLogger> = audit_slot;
            context.register_service(audit_trait);

            tracing::debug!("Audit plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let audit_service = context
            .get_service::<AuditService>()
            .expect("AuditService must be registered before configuring routes");

        let app_state = Arc::new(handlers::types::AppState { audit_service });

        let routes = handlers::handlers::configure_routes().with_state(app_state);

        Some(PluginRoutes::new(routes))
    }

    fn openapi_schema(&self) -> Option<utoipa::openapi::OpenApi> {
        Some(handlers::handlers::AuditApiDoc::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_audit_plugin_name() {
        let audit_plugin = AuditPlugin::new();
        assert_eq!(audit_plugin.name(), "audit");
    }

    #[tokio::test]
    async fn test_audit_plugin_default() {
        let audit_plugin = AuditPlugin;
        assert_eq!(audit_plugin.name(), "audit");
    }
}
