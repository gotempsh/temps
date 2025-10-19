use sea_orm::DatabaseConnection;
use temps_core::UtcDateTime;
use std::sync::Arc;

use super::error_analytics_service::{ErrorAnalyticsService, ErrorDashboardStats};
use super::error_crud_service::ErrorCRUDService;
use super::error_ingestion_service::ErrorIngestionService;
use super::types::*;

/// Facade service that coordinates all error tracking functionality
///
/// This is the main service that applications should use. It delegates
/// to specialized services for different concerns:
/// - Ingestion: Processing and fingerprinting errors
/// - CRUD: Reading and updating error data
/// - Analytics: Statistics and metrics
pub struct ErrorTrackingService {
    pub ingestion: ErrorIngestionService,
    pub crud: ErrorCRUDService,
    pub analytics: ErrorAnalyticsService,
}

impl ErrorTrackingService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            ingestion: ErrorIngestionService::new(db.clone()),
            crud: ErrorCRUDService::new(db.clone()),
            analytics: ErrorAnalyticsService::new(db),
        }
    }

    // Convenience methods that delegate to specialized services

    /// Process a new error event (delegates to ingestion service)
    pub async fn process_error_event(
        &self,
        error_data: CreateErrorEventData,
    ) -> Result<i32, ErrorTrackingError> {
        self.ingestion.process_error_event(error_data).await
    }

    /// List error groups (delegates to CRUD service)
    pub async fn list_error_groups(
        &self,
        project_id: i32,
        page: Option<u64>,
        page_size: Option<u64>,
        status_filter: Option<String>,
        environment_id: Option<i32>,
        sort_by: Option<String>,
        sort_order: Option<String>,
    ) -> Result<(Vec<ErrorGroupDomain>, u64), ErrorTrackingError> {
        self.crud
            .list_error_groups(
                project_id,
                page,
                page_size,
                status_filter,
                environment_id,
                sort_by,
                sort_order,
            )
            .await
    }

    /// Get error group by ID (delegates to CRUD service)
    pub async fn get_error_group(
        &self,
        group_id: i32,
        project_id: i32,
    ) -> Result<ErrorGroupDomain, ErrorTrackingError> {
        self.crud.get_error_group(group_id, project_id).await
    }

    /// Update error group status (delegates to CRUD service)
    pub async fn update_error_group_status(
        &self,
        group_id: i32,
        project_id: i32,
        status: String,
        assigned_to: Option<String>,
    ) -> Result<(), ErrorTrackingError> {
        self.crud
            .update_error_group_status(group_id, project_id, status, assigned_to)
            .await
    }

    /// List error events (delegates to CRUD service)
    pub async fn list_error_events(
        &self,
        group_id: i32,
        project_id: i32,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<ErrorEventDomain>, u64), ErrorTrackingError> {
        self.crud
            .list_error_events(group_id, project_id, page, page_size)
            .await
    }

    /// Get error statistics (delegates to analytics service)
    pub async fn get_error_stats(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<ErrorGroupStats, ErrorTrackingError> {
        self.analytics
            .get_error_stats(project_id, environment_id)
            .await
    }

    /// Get error time series (delegates to analytics service)
    pub async fn get_error_time_series(
        &self,
        project_id: i32,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        interval: &str,
    ) -> Result<Vec<ErrorTimeSeriesPoint>, ErrorTrackingError> {
        self.analytics
            .get_error_time_series(project_id, start_time, end_time, interval)
            .await
    }

    /// Get dashboard stats (delegates to analytics service)
    pub async fn get_dashboard_stats(
        &self,
        project_id: i32,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        environment_id: Option<i32>,
        compare_to_previous: bool,
    ) -> Result<ErrorDashboardStats, ErrorTrackingError> {
        self.analytics
            .get_dashboard_stats(
                project_id,
                start_time,
                end_time,
                environment_id,
                compare_to_previous,
            )
            .await
    }

    /// Check if project has error groups (delegates to CRUD service)
    pub async fn has_error_groups(&self, project_id: i32) -> Result<bool, ErrorTrackingError> {
        self.crud.has_error_groups(project_id).await
    }

    /// Get a specific error event by ID (delegates to CRUD service)
    pub async fn get_error_event(
        &self,
        event_id: i64,
        group_id: i32,
        project_id: i32,
    ) -> Result<ErrorEventDomain, ErrorTrackingError> {
        self.crud
            .get_error_event_by_ids(event_id, group_id, project_id)
            .await
    }
}
