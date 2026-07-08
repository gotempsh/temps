use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use temps_core::{Job, JobQueue, JobReceiver};
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::providers::sentry::SentryProvider;
use crate::sentry::{DSNService, SentryIngestionService};
use crate::services::{ErrorAlertService, ErrorTrackingService, SourceMapService};

/// Error Tracking Plugin for capturing and managing application errors
pub struct ErrorTrackingPlugin;

impl ErrorTrackingPlugin {
    pub fn new() -> Self {
        Self
    }

    /// Process project lifecycle jobs (create default alert rules)
    async fn process_jobs(
        mut receiver: Box<dyn JobReceiver>,
        alert_service: Arc<ErrorAlertService>,
    ) {
        loop {
            match receiver.recv().await {
                Ok(job) => {
                    if let Job::ProjectCreated(job) = job {
                        tracing::info!(
                            "Creating default error alert rules for project {} ({})",
                            job.project_id,
                            job.project_name
                        );
                        match alert_service.create_default_rules(job.project_id).await {
                            Ok(rules) => {
                                tracing::info!(
                                    "Created {} default alert rule(s) for project {}",
                                    rules.len(),
                                    job.project_id
                                );
                            }
                            Err(e) => {
                                tracing::error!(
                                    "Failed to create default alert rules for project {}: {}",
                                    job.project_id,
                                    e
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Error receiving job in error tracking plugin: {:?}", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
            }
        }
    }

    /// Periodically clean up old alert fire records
    async fn cleanup_loop(alert_service: Arc<ErrorAlertService>) {
        let retention_days = 30;
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(6 * 3600)).await;
            alert_service.cleanup_old_fires(retention_days).await;
        }
    }
}

impl Default for ErrorTrackingPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for ErrorTrackingPlugin {
    fn name(&self) -> &'static str {
        "error-tracking"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<sea_orm::DatabaseConnection>();

            // Register source map service
            let source_map_service = Arc::new(SourceMapService::new(db.clone()));
            context.register_service(source_map_service.clone());

            // Register alert service
            let alert_service = Arc::new(ErrorAlertService::new(db.clone()));
            context.register_service(alert_service.clone());

            // Register core error tracking service (with source map support)
            let error_tracking_service = Arc::new(ErrorTrackingService::new(db.clone()));
            error_tracking_service.set_source_map_service(source_map_service);

            // Wire up notification callback if NotificationService is available
            if let Some(notification_service) =
                context.get_service::<temps_notifications::services::NotificationService>()
            {
                tracing::info!("Error tracking: notification callback wired successfully");
                let ns = notification_service.clone();
                error_tracking_service.set_notification_callback(Arc::new(move |alert| {
                    let ns = ns.clone();
                    Box::pin(async move {
                        use temps_notifications::types::{Notification, NotificationPriority};
                        let priority = match alert.priority.as_str() {
                            "Low" => NotificationPriority::Low,
                            "Normal" => NotificationPriority::Normal,
                            "Critical" => NotificationPriority::Critical,
                            _ => NotificationPriority::High,
                        };

                        // Build a clean subject: "ReferenceError: foo is not defined"
                        let subject = alert.group_title.clone();

                        // Build rich notification with metadata
                        let mut notification = Notification::new(subject, alert.message.clone())
                            .with_priority(priority)
                            .with_metadata("trigger", alert.trigger_type.clone())
                            .with_metadata("rule", alert.rule_name.clone())
                            .with_metadata("error_type", alert.error_type.clone())
                            .with_metadata("occurrences", alert.total_count.to_string())
                            .with_metadata("first_seen", alert.first_seen.clone())
                            .with_metadata("last_seen", alert.last_seen.clone());

                        if let Some(ref project) = alert.project_name {
                            notification = notification.with_metadata("project", project.clone());
                        }
                        if let Some(ref env) = alert.environment_name {
                            notification = notification.with_metadata("environment", env.clone());
                        }

                        if let Err(e) = ns.send_notification(notification).await {
                            tracing::error!("Failed to send error alert notification: {}", e);
                        }
                    })
                }));
            } else {
                tracing::warn!("Error tracking: NotificationService not found — alert notifications will be disabled");
            }

            // Wire up autopilot callback if JobQueue is available
            if let Some(queue) = context.get_service::<dyn JobQueue>() {
                tracing::info!("Error tracking: autopilot callback wired successfully");
                let q = queue.clone();
                error_tracking_service.set_autopilot_callback(Arc::new(move |alert| {
                    let q = q.clone();
                    Box::pin(async move {
                        if let Err(e) = q
                            .send(Job::AutopilotTrigger(
                                temps_core::jobs::AutopilotTriggerJob {
                                    project_id: alert.project_id,
                                    trigger_type: alert.trigger_type.clone(),
                                    trigger_source_id: Some(alert.group_id),
                                    trigger_source_type: Some("error_group".to_string()),
                                    error_group_id: Some(alert.group_id),
                                },
                            ))
                            .await
                        {
                            tracing::error!("Failed to send autopilot trigger: {}", e);
                        }
                    })
                }));
            } else {
                tracing::warn!(
                    "Error tracking: JobQueue not found — autopilot triggers will be disabled"
                );
            }

            context.register_service(error_tracking_service.clone());

            // Register Sentry-specific services
            let dsn_service = Arc::new(DSNService::new(db.clone()));
            context.register_service(dsn_service.clone());

            let sentry_ingestion_service = Arc::new(SentryIngestionService::new(
                error_tracking_service.clone(),
                dsn_service.clone(),
            ));
            context.register_service(sentry_ingestion_service);

            let sentry_provider = Arc::new(SentryProvider::new(dsn_service.clone()));
            context.register_service(sentry_provider);

            // Start job listener for project lifecycle events (auto-create default alert rules)
            if let Some(queue_service) = context.get_service::<dyn JobQueue>() {
                let job_receiver = queue_service.subscribe();
                let alert_service_for_jobs = alert_service.clone();
                tokio::spawn(async move {
                    tracing::debug!("Starting error tracking job listener");
                    Self::process_jobs(job_receiver, alert_service_for_jobs).await;
                });

                // Start periodic cleanup of old alert fire records
                let alert_service_for_cleanup = alert_service.clone();
                tokio::spawn(async move {
                    Self::cleanup_loop(alert_service_for_cleanup).await;
                });
            }

            tracing::debug!(
                "Error tracking plugin services registered successfully (including Sentry)"
            );
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let error_tracking_service = context.require_service::<ErrorTrackingService>();
        let alert_service = context.require_service::<ErrorAlertService>();
        let audit_service = context.require_service::<dyn temps_core::AuditLogger>();
        let config_service = context.require_service::<temps_config::ConfigService>();
        let dsn_service = context.require_service::<DSNService>();
        let source_map_service = context.require_service::<SourceMapService>();

        let project_access_checker = context.get_service::<dyn temps_core::ProjectAccessChecker>();

        // Admin: error tracking dashboard + alert rules
        let error_tracking_state = Arc::new(crate::handlers::types::AppState {
            error_tracking_service: error_tracking_service.clone(),
            alert_service: alert_service.clone(),
            audit_service: audit_service.clone(),
            project_access_checker: project_access_checker.clone(),
        });
        let error_tracking_routes =
            crate::handlers::handler::configure_routes().with_state(error_tracking_state.clone());
        let alert_rules_routes =
            crate::handlers::alert_rules_handler::configure_alert_rules_routes()
                .with_state(error_tracking_state);

        // Admin: DSN management
        let dsn_state = Arc::new(crate::sentry::dsn_handlers::DSNAppState {
            dsn_service: dsn_service.clone(),
            audit_service: audit_service.clone(),
            config_service: config_service.clone(),
        });
        let dsn_routes = crate::sentry::dsn_handlers::configure_dsn_routes().with_state(dsn_state);

        // Admin: source map management
        let source_map_state = Arc::new(crate::handlers::source_map_handlers::SourceMapAppState {
            source_map_service: source_map_service.clone(),
            audit_service: audit_service.clone(),
            project_access_checker,
        });
        let source_map_routes = crate::handlers::source_map_handlers::configure_source_map_routes()
            .with_state(source_map_state);

        let routes = error_tracking_routes
            .merge(alert_rules_routes)
            .merge(dsn_routes)
            .merge(source_map_routes);

        Some(PluginRoutes::new(routes))
    }

    fn configure_public_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let error_tracking_service = context.require_service::<ErrorTrackingService>();
        let audit_service = context.require_service::<dyn temps_core::AuditLogger>();
        let sentry_provider = context.require_service::<SentryProvider>();
        let source_map_service = context.require_service::<SourceMapService>();

        // Public: Sentry/OTLP ingestion (called by apps with DSN tokens)
        let ip_address_service = context.get_service::<temps_geo::IpAddressService>();
        let sentry_db = context.get_service::<sea_orm::DatabaseConnection>();
        let telemetry = context
            .get_service::<dyn temps_core::telemetry::TelemetryReporter>()
            .unwrap_or_else(|| Arc::new(temps_core::telemetry::NoopTelemetryReporter));

        let sentry_state = Arc::new(crate::sentry::handlers::AppState {
            sentry_provider: sentry_provider.clone(),
            error_tracking_service: error_tracking_service.clone(),
            audit_service: audit_service.clone(),
            ip_address_service,
            db: sentry_db,
            telemetry,
        });
        let sentry_routes = crate::sentry::handlers::configure_routes().with_state(sentry_state);

        // Public: sentry-cli compatible source map upload (used by CI/CD with DSN auth)
        let db = context.require_service::<sea_orm::DatabaseConnection>();
        let sentry_compat_state = Arc::new(
            crate::handlers::sentry_compat_handlers::SentryCompatAppState {
                source_map_service: source_map_service.clone(),
                db,
            },
        );
        let sentry_compat_routes =
            crate::handlers::sentry_compat_handlers::configure_sentry_compat_routes()
                .with_state(sentry_compat_state);

        let routes = sentry_routes.merge(sentry_compat_routes);
        Some(PluginRoutes::new(routes))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        // Get base error tracking schema
        let mut schema = <crate::handlers::handler::ErrorTrackingApiDoc as OpenApiTrait>::openapi();

        // Merge alert rules routes schema
        let alert_rules_schema =
            <crate::handlers::alert_rules_handler::AlertRulesApiDoc as OpenApiTrait>::openapi();
        schema.paths.paths.extend(alert_rules_schema.paths.paths);
        if let Some(components) = &alert_rules_schema.components {
            if let Some(base_components) = &mut schema.components {
                base_components.schemas.extend(components.schemas.clone());
            }
        }

        // Merge Sentry ingestion routes schema
        let sentry_schema = <crate::sentry::handlers::ApiDoc as OpenApiTrait>::openapi();
        schema.paths.paths.extend(sentry_schema.paths.paths);
        if let Some(components) = &sentry_schema.components {
            if let Some(base_components) = &mut schema.components {
                base_components.schemas.extend(components.schemas.clone());
            }
        }

        // Merge DSN management routes schema
        let dsn_schema = <crate::sentry::dsn_handlers::DSNApiDoc as OpenApiTrait>::openapi();
        schema.paths.paths.extend(dsn_schema.paths.paths);
        if let Some(components) = &dsn_schema.components {
            if let Some(base_components) = &mut schema.components {
                base_components.schemas.extend(components.schemas.clone());
            }
        }

        // Merge source map routes schema
        let source_map_schema =
            <crate::handlers::source_map_handlers::SourceMapApiDoc as OpenApiTrait>::openapi();
        schema.paths.paths.extend(source_map_schema.paths.paths);
        if let Some(components) = &source_map_schema.components {
            if let Some(base_components) = &mut schema.components {
                base_components.schemas.extend(components.schemas.clone());
            }
        }

        // Merge sentry-cli compatibility routes schema
        let sentry_compat_schema =
            <crate::handlers::sentry_compat_handlers::SentryCompatApiDoc as OpenApiTrait>::openapi(
            );
        schema.paths.paths.extend(sentry_compat_schema.paths.paths);
        if let Some(components) = &sentry_compat_schema.components {
            if let Some(base_components) = &mut schema.components {
                base_components.schemas.extend(components.schemas.clone());
            }
        }

        Some(schema)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_error_tracking_plugin_name() {
        let plugin = ErrorTrackingPlugin::new();
        assert_eq!(plugin.name(), "error-tracking");
    }

    #[tokio::test]
    async fn test_error_tracking_plugin_default() {
        let plugin = ErrorTrackingPlugin;
        assert_eq!(plugin.name(), "error-tracking");
    }
}
