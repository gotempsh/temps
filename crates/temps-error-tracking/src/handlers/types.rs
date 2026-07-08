use crate::services::{ErrorAlertService, ErrorTrackingService};
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub error_tracking_service: Arc<ErrorTrackingService>,
    pub alert_service: Arc<ErrorAlertService>,
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
    /// Optional checker for team-based project access (human sessions only).
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}
