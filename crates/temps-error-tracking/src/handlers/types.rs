use crate::services::ErrorTrackingService;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub error_tracking_service: Arc<ErrorTrackingService>,
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
}
