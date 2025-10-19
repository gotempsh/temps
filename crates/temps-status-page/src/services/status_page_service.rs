use sea_orm::DatabaseConnection;
use std::sync::Arc;
use temps_config::ConfigService;
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
    pub fn new(db: Arc<DatabaseConnection>, config_service: Arc<ConfigService>) -> Self {
        let monitor_service = Arc::new(MonitorService::new(db.clone(), config_service));
        let incident_service = Arc::new(IncidentService::new(db));

        Self {
            monitor_service,
            incident_service,
        }
    }

    pub fn monitor_service(&self) -> &MonitorService {
        &self.monitor_service
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
                Err(_) => {
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

        // Check monitor statuses
        let down_count = monitors
            .iter()
            .filter(|m| m.current_status == "down")
            .count();

        let degraded_count = monitors
            .iter()
            .filter(|m| m.current_status == "degraded")
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

