// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use temps_core::{Job, JobQueue, JobReceiver};
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::routes::status_page::{create_router, StatusPageApiDoc, StatusPageAppState};
use crate::services::{HealthCheckService, MonitorService, StatusPageService};

/// Status Page Plugin for monitoring and incident management
pub struct StatusPagePlugin;

const MAX_CONCURRENT_POST_DEPLOY_CHECKS: usize = 10;

fn try_reserve_post_deploy_check(
    semaphore: &Arc<tokio::sync::Semaphore>,
) -> Option<tokio::sync::OwnedSemaphorePermit> {
    semaphore.clone().try_acquire_owned().ok()
}

impl StatusPagePlugin {
    pub fn new() -> Self {
        Self
    }

    /// Process jobs related to project and environment lifecycle
    async fn process_jobs(
        mut receiver: Box<dyn JobReceiver>,
        monitor_service: Arc<MonitorService>,
        health_check_service: Arc<HealthCheckService>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let post_deploy_checks = Arc::new(tokio::sync::Semaphore::new(
            MAX_CONCURRENT_POST_DEPLOY_CHECKS,
        ));
        loop {
            match receiver.recv().await {
                Ok(job) => {
                    match job {
                        Job::ProjectCreated(job) => {
                            tracing::info!("Received ProjectCreated job: {:?}", job);
                            // For project creation, we don't create monitors yet
                            // We wait for environments to be created
                        }
                        Job::ProjectDeleted(job) => {
                            tracing::info!("Received ProjectDeleted job: {:?}", job);
                            // When a project is deleted, all monitors for its environments
                            // will be deleted when environments are deleted
                        }
                        Job::EnvironmentCreated(job) => {
                            tracing::info!("Received EnvironmentCreated job: {:?}", job);
                            // Create a default monitor for the new environment
                            match monitor_service
                                .ensure_monitor_for_environment(
                                    job.project_id,
                                    job.environment_id,
                                    &job.environment_name,
                                )
                                .await
                            {
                                Ok(monitor) => {
                                    tracing::info!(
                                        "Created monitor {} for environment {} ({})",
                                        monitor.id,
                                        job.environment_id,
                                        job.environment_name
                                    );
                                }
                                Err(e) => {
                                    tracing::error!(
                                        "Failed to create monitor for environment {} ({}): {:?}",
                                        job.environment_id,
                                        job.environment_name,
                                        e
                                    );
                                }
                            }
                        }
                        Job::DeploymentSucceeded(job) => {
                            // Persist the deployment's health path before checking so
                            // the first post-deploy result reflects the service's real
                            // health endpoint rather than a stale `/` probe.
                            tracing::info!(
                                health_path = ?job.health_check_path,
                                environment_id = job.environment_id,
                                project_id = job.project_id,
                                "Synchronizing managed monitor path with successful deployment"
                            );
                            if let Err(e) = monitor_service
                                .update_managed_check_path_for_environment(
                                    job.project_id,
                                    job.environment_id,
                                    job.health_check_path.as_deref(),
                                )
                                .await
                            {
                                tracing::error!(
                                    "Failed to synchronize managed monitor check_path for environment {}: {:?}",
                                    job.environment_id,
                                    e
                                );
                            }

                            // Existing monitors only receive MonitorCreated once, so
                            // relying on the 60-second scheduler leaves a successful
                            // deployment showing the previous Down/Unknown state. The
                            // HTTP probe must not block this shared lifecycle-event
                            // receiver, and the non-blocking permit prevents deployment
                            // bursts from creating an unbounded task backlog. If all
                            // permits are busy, the regular scheduler remains the
                            // eventual fallback.
                            if let Some(permit) = try_reserve_post_deploy_check(&post_deploy_checks)
                            {
                                let health_check_service = health_check_service.clone();
                                tokio::spawn(async move {
                                    let _permit = permit;
                                    match health_check_service
                                        .check_monitors_for_environment(
                                            job.project_id,
                                            job.environment_id,
                                        )
                                        .await
                                    {
                                        Ok(checked) => tracing::info!(
                                            checked,
                                            environment_id = job.environment_id,
                                            deployment_id = job.deployment_id,
                                            "Recomputed monitor status after successful deployment"
                                        ),
                                        Err(e) => tracing::error!(
                                            environment_id = job.environment_id,
                                            deployment_id = job.deployment_id,
                                            error = ?e,
                                            "Failed to recompute monitor status after successful deployment"
                                        ),
                                    }
                                });
                            } else {
                                tracing::warn!(
                                    environment_id = job.environment_id,
                                    deployment_id = job.deployment_id,
                                    max_concurrent_checks = MAX_CONCURRENT_POST_DEPLOY_CHECKS,
                                    "Skipped immediate monitor recomputation because all post-deploy check slots are busy"
                                );
                            }
                        }
                        Job::EnvironmentDeleted(job) => {
                            tracing::info!("Received EnvironmentDeleted job: {:?}", job);
                            // Delete all monitors associated with the environment
                            match monitor_service
                                .list_monitors(job.project_id, Some(job.environment_id))
                                .await
                            {
                                Ok(monitors) => {
                                    for monitor in monitors {
                                        if let Err(e) =
                                            monitor_service.delete_monitor(monitor.id).await
                                        {
                                            tracing::error!(
                                                "Failed to delete monitor {} for environment {}: {:?}",
                                                monitor.id,
                                                job.environment_id,
                                                e
                                            );
                                        } else {
                                            tracing::info!(
                                                "Deleted monitor {} for deleted environment {}",
                                                monitor.id,
                                                job.environment_id
                                            );
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::error!(
                                        "Failed to list monitors for environment {}: {:?}",
                                        job.environment_id,
                                        e
                                    );
                                }
                            }
                        }
                        _ => {
                            // Ignore other job types
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Error receiving job: {:?}", e);
                    // Continue processing instead of breaking
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
            }
        }
    }
}

impl Default for StatusPagePlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for StatusPagePlugin {
    fn name(&self) -> &'static str {
        "status-page"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let config_service = context.require_service::<temps_config::ConfigService>();
            let queue_service = context.require_service::<dyn JobQueue>();

            // Register status page service. ONE MonitorService instance (with a
            // job queue) backs both this and the job-listener loop below via
            // monitor_service_arc() -- see StatusPageService::with_job_queue's
            // doc comment for the bug two separate instances caused.
            let status_page_service = Arc::new(StatusPageService::with_job_queue(
                db.clone(),
                config_service.clone(),
                queue_service.clone(),
            ));
            context.register_service(status_page_service.clone());
            let monitor_service = status_page_service.monitor_service_arc();

            // Create health check service with mandatory ConfigService and JobQueue
            // The service will emit StatusCheckCompleted jobs for outage detection
            let health_check_service = Arc::new(
                HealthCheckService::new(db.clone(), config_service, queue_service.clone())
                    .map_err(|error| PluginError::PluginRegistrationFailed {
                        plugin_name: "status-page".to_string(),
                        error: error.to_string(),
                    })?,
            );
            context.register_service(health_check_service.clone());

            // Start the health check scheduler with job receiver for realtime monitor creation
            let scheduler_service = health_check_service.clone();
            let scheduler_job_receiver = queue_service.subscribe();
            tokio::spawn(async move {
                scheduler_service
                    .start_scheduler(scheduler_job_receiver)
                    .await;
            });

            // Start job listener for project/environment lifecycle events
            let job_receiver = queue_service.subscribe();
            let monitor_service_clone = monitor_service.clone();
            let health_check_service_clone = health_check_service.clone();
            tokio::spawn(async move {
                tracing::debug!("Starting status page job listener");
                if let Err(e) = Self::process_jobs(
                    job_receiver,
                    monitor_service_clone,
                    health_check_service_clone,
                )
                .await
                {
                    tracing::error!("Status page job processor error: {}", e);
                }
            });

            tracing::debug!("Status page plugin services registered, health check scheduler started, and job listener active");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let status_page_service = context.require_service::<StatusPageService>();
        let telemetry = context
            .get_service::<dyn temps_core::TelemetryReporter>()
            .unwrap_or_else(|| Arc::new(temps_core::telemetry::NoopTelemetryReporter));

        let project_access_checker = context.get_service::<dyn temps_core::ProjectAccessChecker>();
        let audit_service = context.require_service::<dyn temps_core::AuditLogger>();

        struct AppState {
            status_page_service: Arc<StatusPageService>,
            telemetry: Arc<dyn temps_core::TelemetryReporter>,
            audit_service: Arc<dyn temps_core::AuditLogger>,
            project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
        }

        impl StatusPageAppState for AppState {
            fn status_page_service(&self) -> &StatusPageService {
                &self.status_page_service
            }

            fn telemetry(&self) -> &Arc<dyn temps_core::TelemetryReporter> {
                &self.telemetry
            }

            fn audit_service(&self) -> &Arc<dyn temps_core::AuditLogger> {
                &self.audit_service
            }

            fn project_access_checker(&self) -> Option<Arc<dyn temps_core::ProjectAccessChecker>> {
                self.project_access_checker.clone()
            }
        }

        let app_state = Arc::new(AppState {
            status_page_service,
            telemetry,
            audit_service,
            project_access_checker,
        });

        let routes = create_router().with_state(app_state);
        Some(PluginRoutes::new(routes))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<StatusPageApiDoc as OpenApiTrait>::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_status_page_plugin_name() {
        let plugin = StatusPagePlugin::new();
        assert_eq!(plugin.name(), "status-page");
    }

    #[tokio::test]
    async fn test_status_page_plugin_default() {
        let plugin = StatusPagePlugin;
        assert_eq!(plugin.name(), "status-page");
    }

    #[test]
    fn post_deploy_monitor_checks_are_reserved_without_waiting_and_bounded() {
        let semaphore = Arc::new(tokio::sync::Semaphore::new(2));
        let first = try_reserve_post_deploy_check(&semaphore)
            .expect("the first post-deploy check should reserve a slot");
        let second = try_reserve_post_deploy_check(&semaphore)
            .expect("the second post-deploy check should reserve a slot");

        assert!(
            try_reserve_post_deploy_check(&semaphore).is_none(),
            "reservation must return immediately instead of queueing another check"
        );

        drop(first);
        assert!(try_reserve_post_deploy_check(&semaphore).is_some());
        drop(second);
    }
}
