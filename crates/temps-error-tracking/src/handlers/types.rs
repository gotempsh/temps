use std::sync::Arc;
use crate::services::ErrorTrackingService;


#[derive(Clone)]
pub struct AppState {
    pub error_tracking_service: Arc<ErrorTrackingService>,
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
}
