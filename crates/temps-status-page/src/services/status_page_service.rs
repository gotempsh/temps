// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::DatabaseConnection;
use std::sync::Arc;
use temps_config::ConfigService;
use temps_core::JobQueue;
use tracing::info;

use super::incident_service::IncidentService;
use super::monitor_service::MonitorService;
use super::types::{MonitorStatus, StatusPageError, StatusPageOverview};

/// Service for managing the overall status page
pub struct StatusPageService {
    monitor_service: Arc<MonitorService>,
    incident_service: Arc<IncidentService>,
}

impl StatusPageService {
    /// Without a job queue: `create_monitor` calls made through this instance
    /// never emit `MonitorCreated`, so a monitor created via it waits for the
    /// next periodic health-check cycle (up to 60s, unaligned to creation
    /// time) instead of getting an immediate first check. Prefer
    /// `with_job_queue` wherever the caller has one -- see its doc comment.
    pub fn new(db: Arc<DatabaseConnection>, config_service: Arc<ConfigService>) -> Self {
        let monitor_service = Arc::new(MonitorService::new(db.clone(), config_service));
        let incident_service = Arc::new(IncidentService::new(db));

        Self {
            monitor_service,
            incident_service,
        }
    }

    /// Real constructor for production use: plugin.rs must build exactly ONE
    /// `MonitorService` and share it between this (which the HTTP route
    /// handlers call through) and its own job-listener loop
    /// (`StatusPagePlugin::process_jobs`) via `monitor_service_arc()`.
    ///
    /// A prior version of plugin.rs built two SEPARATE `MonitorService`
    /// instances -- this one via `StatusPageService::new` (no job queue) for
    /// the HTTP handlers, and another via `MonitorService::with_job_queue`
    /// only for the job-listener loop. `POST /projects/{id}/monitors` (the
    /// real, user-facing create path) went through the job-queue-less
    /// instance, so `emit_monitor_created` silently no-opped for every
    /// API-created monitor -- only the internal auto-provisioning path
    /// (`ensure_monitor_for_environment`, called from the job listener's own
    /// instance) ever got the "checked immediately" behavior
    /// `start_scheduler`'s doc comment promises. Found live while building
    /// e2e coverage: an explicitly-created monitor's current-status stayed
    /// "unknown" for a full periodic cycle instead of flipping to
    /// "operational" within seconds like the auto-created one did.
    pub fn with_job_queue(
        db: Arc<DatabaseConnection>,
        config_service: Arc<ConfigService>,
        job_queue: Arc<dyn JobQueue>,
    ) -> Self {
        let monitor_service = Arc::new(MonitorService::with_job_queue(
            db.clone(),
            config_service,
            job_queue,
        ));
        let incident_service = Arc::new(IncidentService::new(db));

        Self {
            monitor_service,
            incident_service,
        }
    }

    pub fn monitor_service(&self) -> &MonitorService {
        &self.monitor_service
    }

    /// The shared `Arc` backing `monitor_service()` -- see `with_job_queue`'s
    /// doc comment for why callers that also need their own long-lived
    /// handle (e.g. a job-listener loop) must reuse this instance rather than
    /// constructing a second `MonitorService`.
    pub fn monitor_service_arc(&self) -> Arc<MonitorService> {
        self.monitor_service.clone()
    }

    pub fn incident_service(&self) -> &IncidentService {
        &self.incident_service
    }

    /// Create a monitor when a new environment is created
    pub async fn on_environment_created(
        &self,
        project_id: i32,
        environment_id: i32,
        environment_name: &str,
    ) -> Result<(), StatusPageError> {
        self.monitor_service
            .ensure_monitor_for_environment(project_id, environment_id, environment_name)
            .await?;

        info!(
            "Created monitor for new environment {} ({}) in project {}",
            environment_name, environment_id, project_id
        );

        Ok(())
    }

    /// Get overview of status page for a project/environment
    pub async fn get_status_overview(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<StatusPageOverview, StatusPageError> {
        // Get all monitors
        let monitors = self
            .monitor_service
            .list_monitors(project_id, environment_id)
            .await?;

        // Get status for each monitor
        let mut monitor_statuses = Vec::new();
        for monitor in monitors {
            match self.monitor_service.get_monitor_status(monitor.id).await {
                Ok(status) => monitor_statuses.push(status),
                Err(e) => {
                    tracing::error!(
                        "get_monitor_status failed for monitor {} -- status overview will show it as unknown: {:?}",
                        monitor.id,
                        e
                    );
                    // If we can't get status, create a default one
                    monitor_statuses.push(MonitorStatus {
                        monitor,
                        current_status: "unknown".to_string(),
                        uptime_percentage: 0.0,
                        avg_response_time_ms: None,
                    });
                }
            }
        }

        // Get recent incidents
        let recent_incidents = self
            .incident_service
            .get_recent_incidents(project_id, environment_id, Some(5))
            .await?;

        // Determine overall status based on monitors and active incidents
        let overall_status = self.calculate_overall_status(&monitor_statuses, &recent_incidents);

        Ok(StatusPageOverview {
            status: overall_status,
            monitors: monitor_statuses,
            recent_incidents,
        })
    }

    /// Calculate overall system status
    fn calculate_overall_status(
        &self,
        monitors: &[MonitorStatus],
        recent_incidents: &[super::types::IncidentResponse],
    ) -> String {
        // Check for active critical incidents
        if recent_incidents
            .iter()
            .any(|i| i.status != "resolved" && i.severity == "critical")
        {
            return "major_outage".to_string();
        }

        // Check for active major incidents
        if recent_incidents
            .iter()
            .any(|i| i.status != "resolved" && i.severity == "major")
        {
            return "partial_outage".to_string();
        }

        // Check monitor statuses. health_check_service.rs writes the finer-grained
        // "major_outage"/"partial_outage" (not the plain "down"/"degraded" this used
        // to compare against), so a fully-down monitor was silently invisible here
        // unless an incident also happened to exist to catch it via the severity
        // checks above.
        let down_count = monitors
            .iter()
            .filter(|m| matches!(m.current_status.as_str(), "down" | "major_outage"))
            .count();

        let degraded_count = monitors
            .iter()
            .filter(|m| matches!(m.current_status.as_str(), "degraded" | "partial_outage"))
            .count();

        if down_count > 0 {
            return "partial_outage".to_string();
        }

        if degraded_count > 0 || recent_incidents.iter().any(|i| i.status != "resolved") {
            return "degraded_performance".to_string();
        }

        "operational".to_string()
    }
}
