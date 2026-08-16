use sea_orm::{ActiveModelTrait, DatabaseConnection, EntityTrait, Set};
use std::sync::Arc;
use temps_core::UtcDateTime;
use temps_entities::{environments, error_groups, projects};
use tokio::sync::OnceCell;

use super::error_alert_service::{AlertNotification, ErrorAlertService};
use super::error_analytics_service::{ErrorAnalyticsService, ErrorDashboardStats};
use super::error_crud_service::ErrorCRUDService;
use super::error_ingestion_service::ErrorIngestionService;
use super::source_map_service::SourceMapService;
use super::types::*;

/// Callback for sending alert notifications through the notification system
pub type NotificationCallback =
    Arc<dyn Fn(AlertNotification) -> futures::future::BoxFuture<'static, ()> + Send + Sync>;

/// Facade service that coordinates all error tracking functionality
///
/// This is the main service that applications should use. It delegates
/// to specialized services for different concerns:
/// - Ingestion: Processing and fingerprinting errors
/// - CRUD: Reading and updating error data
/// - Analytics: Statistics and metrics
/// - Source maps: Symbolicating minified stack traces
/// - Alerts: Evaluating alert rules and triggering notifications
pub struct ErrorTrackingService {
    db: Arc<DatabaseConnection>,
    pub ingestion: ErrorIngestionService,
    pub crud: ErrorCRUDService,
    pub analytics: ErrorAnalyticsService,
    pub alerts: ErrorAlertService,
    source_map_service: OnceCell<Arc<SourceMapService>>,
    notification_callback: OnceCell<NotificationCallback>,
    autopilot_callback: OnceCell<NotificationCallback>,
}

impl ErrorTrackingService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db: db.clone(),
            ingestion: ErrorIngestionService::new(db.clone()),
            crud: ErrorCRUDService::new(db.clone()),
            analytics: ErrorAnalyticsService::new(db.clone()),
            alerts: ErrorAlertService::new(db),
            source_map_service: OnceCell::new(),
            notification_callback: OnceCell::new(),
            autopilot_callback: OnceCell::new(),
        }
    }

    /// Set the source map service for symbolication support.
    /// This is called after construction since SourceMapService and ErrorTrackingService
    /// are created independently in the plugin registration.
    pub fn set_source_map_service(&self, service: Arc<SourceMapService>) {
        let _ = self.source_map_service.set(service);
    }

    /// Set a callback for sending alert notifications.
    /// This is called during plugin initialization to wire up with NotificationService.
    pub fn set_notification_callback(&self, callback: NotificationCallback) {
        let _ = self.notification_callback.set(callback);
    }

    /// Set a callback for autopilot triggers.
    /// This is called during plugin initialization to wire up with the autopilot job queue.
    /// Only notifications with trigger_type "new_issue" or "regression" are forwarded.
    pub fn set_autopilot_callback(&self, callback: NotificationCallback) {
        let _ = self.autopilot_callback.set(callback);
    }

    /// Send notifications for alert results
    async fn send_alert_notifications(&self, notifications: Vec<AlertNotification>) {
        if notifications.is_empty() {
            tracing::debug!("No alert notifications to send");
            return;
        }
        if let Some(callback) = self.notification_callback.get() {
            tracing::info!("Sending {} alert notification(s)", notifications.len());
            for notification in &notifications {
                tracing::info!(
                    "Firing alert: rule='{}' group='{}' priority='{}'",
                    notification.rule_name,
                    notification.group_title,
                    notification.priority
                );
                callback(notification.clone()).await;
            }
        } else {
            tracing::warn!(
                "Alert notifications generated ({}) but no notification callback is set",
                notifications.len()
            );
        }

        // Fire autopilot callback for new_issue and regression triggers
        if let Some(autopilot_cb) = self.autopilot_callback.get() {
            for notification in &notifications {
                if notification.trigger_type == "new_issue"
                    || notification.trigger_type == "regression"
                {
                    autopilot_cb(notification.clone()).await;
                }
            }
        }
    }

    /// Load an error group by ID
    async fn load_group(&self, group_id: i32) -> Option<error_groups::Model> {
        error_groups::Entity::find_by_id(group_id)
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten()
    }

    // Convenience methods that delegate to specialized services

    /// Process a new error event.
    /// If a source map service is configured and the event has a release version,
    /// stack traces will be symbolicated before storage.
    /// After ingestion, evaluates alert rules and sends notifications.
    pub async fn process_error_event(
        &self,
        mut error_data: CreateErrorEventData,
    ) -> Result<i32, ErrorTrackingError> {
        // Symbolicate stack traces if source maps are available
        if let Some(sm_service) = self.source_map_service.get() {
            if let Err(e) = sm_service.symbolicate_error_event(&mut error_data).await {
                tracing::warn!(
                    "Source map symbolication failed (continuing without): {}",
                    e
                );
            }
        }

        let has_user_context = error_data.user_id.is_some()
            || error_data.user_email.is_some()
            || error_data.visitor_id.is_some();

        // Check if a group already exists for this fingerprint (before ingestion)
        // Capture the pre-ingestion status for regression detection
        let fingerprint = self.ingestion.generate_fingerprint(&error_data);
        let existing_group_id = self
            .ingestion
            .find_group_by_fingerprint_public(&fingerprint, error_data.project_id)
            .await;

        // Capture pre-ingestion status so regression detection works correctly
        // (ingestion doesn't change status, but we need the status before any re-open)
        let pre_ingestion_status = if let Some(gid) = existing_group_id {
            self.load_group(gid)
                .await
                .map(|g| g.status.clone())
                .unwrap_or_default()
        } else {
            String::new()
        };

        let project_id = error_data.project_id;
        let group_id = self.ingestion.process_error_event(error_data).await?;

        // Evaluate alert rules (fire-and-forget, don't fail the ingestion)
        if let Some(group) = self.load_group(group_id).await {
            let is_new_group = existing_group_id.is_none();
            let is_regression =
                pre_ingestion_status == "resolved" || pre_ingestion_status == "ignored";

            tracing::debug!(
                "Evaluating alert rules for group {} (project={}, is_new={}, is_regression={})",
                group_id,
                group.project_id,
                is_new_group,
                is_regression
            );

            // Re-open the group if it was resolved/ignored (regression)
            if is_regression {
                tracing::info!(
                    "Regression detected: group {} was '{}', re-opening to 'unresolved'",
                    group_id,
                    pre_ingestion_status
                );
                if let Err(e) = self.reopen_group(group_id).await {
                    tracing::error!("Failed to re-open group {}: {}", group_id, e);
                }
            }

            let notifications = if is_new_group {
                self.alerts
                    .evaluate_new_group(&group, has_user_context)
                    .await
            } else {
                self.alerts
                    .evaluate_event_added_with_status(
                        &group,
                        has_user_context,
                        &pre_ingestion_status,
                    )
                    .await
            };

            // Enrich notifications with project/environment names and send
            let enriched = self.enrich_notifications(notifications, project_id).await;
            self.send_alert_notifications(enriched).await;
        }

        Ok(group_id)
    }

    /// List error groups (delegates to CRUD service)
    #[allow(clippy::too_many_arguments)]
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

    /// Update error group status (delegates to CRUD service).
    /// Evaluates status change alert rules after the update.
    pub async fn update_error_group_status(
        &self,
        group_id: i32,
        project_id: i32,
        status: String,
        assigned_to: Option<String>,
    ) -> Result<(), ErrorTrackingError> {
        self.crud
            .update_error_group_status(group_id, project_id, status.clone(), assigned_to)
            .await?;

        // Evaluate status change alert rules
        if let Some(group) = self.load_group(group_id).await {
            let notifications = self.alerts.evaluate_status_change(&group, &status).await;
            let enriched = self.enrich_notifications(notifications, project_id).await;
            self.send_alert_notifications(enriched).await;
        }

        Ok(())
    }

    /// List error events (delegates to CRUD service)
    pub async fn list_error_events(
        &self,
        group_id: i32,
        project_id: i32,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<ErrorEventDomain>, u64), ErrorTrackingError> {
        let (mut events, total) = self
            .crud
            .list_error_events(group_id, project_id, page, page_size)
            .await?;

        if let Some(sm_service) = self.source_map_service.get() {
            for event in &mut events {
                if let Some(data) = &mut event.data {
                    sm_service.symbolicate_stored_event(project_id, data).await;
                }
            }
        }

        Ok((events, total))
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

    /// Get a specific error event by ID (delegates to CRUD service).
    ///
    /// Performs on-the-fly symbolication if:
    /// - A source map service is configured
    /// - The event has stored sentry data with a release version
    /// - The stack frames haven't been symbolicated yet
    pub async fn get_error_event(
        &self,
        event_id: i64,
        group_id: i32,
        project_id: i32,
    ) -> Result<ErrorEventDomain, ErrorTrackingError> {
        let mut event = self
            .crud
            .get_error_event_by_ids(event_id, group_id, project_id)
            .await?;

        // On-the-fly symbolication: resolve stack frames using stored source maps
        if let Some(sm_service) = self.source_map_service.get() {
            if let Some(data) = &mut event.data {
                sm_service.symbolicate_stored_event(project_id, data).await;
            }
        }

        Ok(event)
    }

    /// Re-open a resolved/ignored error group back to unresolved (regression)
    async fn reopen_group(&self, group_id: i32) -> Result<(), ErrorTrackingError> {
        let group = error_groups::Entity::find_by_id(group_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ErrorTrackingError::GroupNotFound)?;

        let mut update: error_groups::ActiveModel = group.into();
        update.status = Set("unresolved".to_string());
        update.updated_at = Set(chrono::Utc::now());
        update.update(self.db.as_ref()).await?;
        Ok(())
    }

    /// Enrich alert notifications with project/environment names for better emails
    async fn enrich_notifications(
        &self,
        notifications: Vec<AlertNotification>,
        project_id: i32,
    ) -> Vec<AlertNotification> {
        if notifications.is_empty() {
            return notifications;
        }

        // Look up project name and slug (slug is used to build deep links in notifications)
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten();
        let project_slug = project.as_ref().map(|p| p.slug.clone());
        let project_name = project
            .map(|p| p.name)
            .unwrap_or_else(|| format!("Project {}", project_id));

        // Collect unique environment IDs to resolve
        let env_ids: std::collections::HashSet<i32> = notifications
            .iter()
            .filter_map(|n| n.environment_id)
            .collect();

        // Resolve environment names in batch
        let mut env_names: std::collections::HashMap<i32, String> =
            std::collections::HashMap::new();
        for env_id in env_ids {
            if let Some(name) = self.resolve_environment_name(env_id).await {
                env_names.insert(env_id, name);
            }
        }

        notifications
            .into_iter()
            .map(|mut n| {
                n.project_name = Some(project_name.clone());
                n.project_slug = project_slug.clone();
                if let Some(env_id) = n.environment_id {
                    n.environment_name = env_names.get(&env_id).cloned();
                }
                n
            })
            .collect()
    }

    async fn resolve_environment_name(&self, environment_id: i32) -> Option<String> {
        environments::Entity::find_by_id(environment_id)
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten()
            .map(|e| e.name)
    }
}
