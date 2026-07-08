use std::sync::Arc;

use temps_core::AuditLogger;

use crate::services::{LogMetadataService, LogSearchService, RetentionService, TailService};

/// Shared state for log aggregator HTTP handlers
pub struct LogAggregatorAppState {
    pub search_service: Arc<LogSearchService>,
    pub metadata_service: Arc<LogMetadataService>,
    pub tail_service: Arc<TailService>,
    pub retention_service: Arc<RetentionService>,
    pub audit_service: Arc<dyn AuditLogger>,
    /// Optional checker for team-based project access (human sessions only).
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

pub async fn create_log_aggregator_app_state(
    search_service: Arc<LogSearchService>,
    metadata_service: Arc<LogMetadataService>,
    tail_service: Arc<TailService>,
    retention_service: Arc<RetentionService>,
    audit_service: Arc<dyn AuditLogger>,
) -> Arc<LogAggregatorAppState> {
    Arc::new(LogAggregatorAppState {
        search_service,
        metadata_service,
        tail_service,
        retention_service,
        audit_service,
        project_access_checker: None,
    })
}
