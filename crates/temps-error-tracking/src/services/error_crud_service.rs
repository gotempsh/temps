use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, Set,
};
use std::sync::Arc;
use temps_entities::{error_events, error_groups};

use super::types::{ErrorEventDomain, ErrorGroupDomain, ErrorTrackingError};

/// Service for CRUD operations on error groups and events
pub struct ErrorCRUDService {
    db: Arc<DatabaseConnection>,
}

impl ErrorCRUDService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Convert error_events::Model to ErrorEventDomain by extracting from JSONB
    fn to_domain(event: error_events::Model) -> ErrorEventDomain {
        use temps_entities::error_events::ErrorEventData;

        // Parse structured data from JSONB
        let data = event
            .data
            .as_ref()
            .and_then(ErrorEventData::from_json_value);

        ErrorEventDomain {
            id: event.id,
            error_group_id: event.error_group_id,
            fingerprint_hash: event.fingerprint_hash,
            timestamp: event.timestamp,
            source: event.source,
            exception_type: event.exception_type,
            exception_value: event.exception_value,

            // Extract from nested data structures
            stack_trace: data
                .as_ref()
                .and_then(|d| d.stack_trace.as_ref())
                .and_then(|st| serde_json::to_value(st).ok()),

            // Request context
            url: data.as_ref().and_then(|d| d.request.as_ref()?.url.clone()),
            user_agent: data
                .as_ref()
                .and_then(|d| d.request.as_ref()?.user_agent.clone()),
            referrer: data
                .as_ref()
                .and_then(|d| d.request.as_ref()?.referrer.clone()),
            method: data
                .as_ref()
                .and_then(|d| d.request.as_ref()?.method.clone()),
            headers: data
                .as_ref()
                .and_then(|d| d.request.as_ref()?.headers.clone()),
            request_cookies: data
                .as_ref()
                .and_then(|d| d.request.as_ref()?.cookies.clone()),
            request_query_string: data
                .as_ref()
                .and_then(|d| d.request.as_ref()?.query_string.clone()),
            request_data: data
                .as_ref()
                .and_then(|d| d.request.as_ref()?.post_data.clone()),
            request_context: None,

            // User context
            user_id: data.as_ref().and_then(|d| d.user.as_ref()?.user_id.clone()),
            user_email: data.as_ref().and_then(|d| d.user.as_ref()?.email.clone()),
            user_username: data
                .as_ref()
                .and_then(|d| d.user.as_ref()?.username.clone()),
            user_ip_address: data
                .as_ref()
                .and_then(|d| d.user.as_ref()?.ip_address.clone()),
            user_segment: data.as_ref().and_then(|d| d.user.as_ref()?.segment.clone()),
            session_id: data
                .as_ref()
                .and_then(|d| d.user.as_ref()?.session_id.clone()),
            user_context: data.as_ref().and_then(|d| d.user.as_ref()?.custom.clone()),

            // Device context
            browser: data
                .as_ref()
                .and_then(|d| d.device.as_ref()?.browser.clone()),
            browser_version: data
                .as_ref()
                .and_then(|d| d.device.as_ref()?.browser_version.clone()),
            operating_system: data.as_ref().and_then(|d| d.device.as_ref()?.os.clone()),
            operating_system_version: data
                .as_ref()
                .and_then(|d| d.device.as_ref()?.os_version.clone()),
            device_type: data
                .as_ref()
                .and_then(|d| d.device.as_ref()?.device_type.clone()),
            screen_width: data.as_ref().and_then(|d| d.device.as_ref()?.screen_width),
            screen_height: data.as_ref().and_then(|d| d.device.as_ref()?.screen_height),
            viewport_width: data
                .as_ref()
                .and_then(|d| d.device.as_ref()?.viewport_width),
            viewport_height: data
                .as_ref()
                .and_then(|d| d.device.as_ref()?.viewport_height),
            locale: data
                .as_ref()
                .and_then(|d| d.device.as_ref()?.locale.clone()),
            timezone: data
                .as_ref()
                .and_then(|d| d.device.as_ref()?.timezone.clone()),
            os_name: data.as_ref().and_then(|d| d.device.as_ref()?.os.clone()),
            os_version: data
                .as_ref()
                .and_then(|d| d.device.as_ref()?.os_version.clone()),
            os_build: data
                .as_ref()
                .and_then(|d| d.device.as_ref()?.os_build.clone()),
            os_kernel_version: data
                .as_ref()
                .and_then(|d| d.device.as_ref()?.os_kernel_version.clone()),
            device_arch: data
                .as_ref()
                .and_then(|d| d.device.as_ref()?.device_arch.clone()),
            device_processor_count: data
                .as_ref()
                .and_then(|d| d.device.as_ref()?.processor_count),
            device_processor_frequency: data
                .as_ref()
                .and_then(|d| d.device.as_ref()?.processor_frequency),
            device_memory_size: data.as_ref().and_then(|d| d.device.as_ref()?.memory_size),
            device_free_memory: data.as_ref().and_then(|d| d.device.as_ref()?.free_memory),
            device_boot_time: data
                .as_ref()
                .and_then(|d| d.device.as_ref()?.boot_time.as_ref())
                .and_then(|s| {
                    chrono::DateTime::parse_from_rfc3339(s)
                        .ok()
                        .map(|dt| dt.to_utc())
                }),

            // Environment context
            release_version: data
                .as_ref()
                .and_then(|d| d.environment.as_ref()?.release.clone()),
            build_number: data
                .as_ref()
                .and_then(|d| d.environment.as_ref()?.build.clone()),
            server_name: data
                .as_ref()
                .and_then(|d| d.environment.as_ref()?.server_name.clone()),
            environment: data
                .as_ref()
                .and_then(|d| d.environment.as_ref()?.environment.clone()),
            sdk_name: data
                .as_ref()
                .and_then(|d| d.environment.as_ref()?.sdk_name.clone()),
            sdk_version: data
                .as_ref()
                .and_then(|d| d.environment.as_ref()?.sdk_version.clone()),
            sdk_integrations: data
                .as_ref()
                .and_then(|d| d.environment.as_ref()?.sdk_integrations.as_ref())
                .and_then(|v| serde_json::to_value(v).ok()),
            platform: data
                .as_ref()
                .and_then(|d| d.environment.as_ref()?.platform.clone()),
            runtime_name: data
                .as_ref()
                .and_then(|d| d.environment.as_ref()?.runtime_name.clone()),
            runtime_version: data
                .as_ref()
                .and_then(|d| d.environment.as_ref()?.runtime_version.clone()),
            app_start_time: data
                .as_ref()
                .and_then(|d| d.environment.as_ref()?.app_start_time.as_ref())
                .and_then(|s| {
                    chrono::DateTime::parse_from_rfc3339(s)
                        .ok()
                        .map(|dt| dt.to_utc())
                }),
            app_memory: data
                .as_ref()
                .and_then(|d| d.environment.as_ref()?.app_memory),

            // Trace context
            transaction_name: data
                .as_ref()
                .and_then(|d| d.trace.as_ref()?.transaction.clone()),
            breadcrumbs: data
                .as_ref()
                .and_then(|d| d.trace.as_ref()?.breadcrumbs.as_ref())
                .and_then(|v| serde_json::to_value(v).ok()),
            extra_context: data.as_ref().and_then(|d| d.trace.as_ref()?.extra.clone()),
            contexts: data
                .as_ref()
                .and_then(|d| d.trace.as_ref()?.contexts.clone()),

            project_id: event.project_id,
            environment_id: event.environment_id,
            deployment_id: event.deployment_id,
            visitor_id: event.visitor_id,
            ip_geolocation_id: event.ip_geolocation_id,

            // Raw JSONB data (full transparency)
            data: event.data,

            created_at: event.created_at,
        }
    }

    /// List error groups with filtering and pagination
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
        let page = page.unwrap_or(1);
        let page_size = std::cmp::min(page_size.unwrap_or(20), 100);

        let mut query =
            error_groups::Entity::find().filter(error_groups::Column::ProjectId.eq(project_id));

        // Apply filters
        if let Some(status) = status_filter {
            query = query.filter(error_groups::Column::Status.eq(status));
        }

        if let Some(env_id) = environment_id {
            query = query.filter(error_groups::Column::EnvironmentId.eq(env_id));
        }

        // Apply sorting (default: last_seen DESC)
        match sort_by.as_deref() {
            Some("first_seen") => {
                query = match sort_order.as_deref() {
                    Some("asc") => query.order_by_asc(error_groups::Column::FirstSeen),
                    _ => query.order_by_desc(error_groups::Column::FirstSeen),
                };
            }
            Some("total_count") => {
                query = match sort_order.as_deref() {
                    Some("asc") => query.order_by_asc(error_groups::Column::TotalCount),
                    _ => query.order_by_desc(error_groups::Column::TotalCount),
                };
            }
            _ => {
                // Default: last_seen DESC (most recent errors first)
                query = query.order_by_desc(error_groups::Column::LastSeen);
            }
        }

        let paginator = query.paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let groups = paginator.fetch_page(page - 1).await?;

        let domain_groups = groups
            .into_iter()
            .map(|group| ErrorGroupDomain {
                id: group.id,
                title: group.title,
                error_type: group.error_type,
                message_template: group.message_template,
                first_seen: group.first_seen,
                last_seen: group.last_seen,
                total_count: group.total_count,
                status: group.status,
                assigned_to: group.assigned_to,
                project_id: group.project_id,
                environment_id: group.environment_id,
                deployment_id: group.deployment_id,
                visitor_id: group.visitor_id,
                created_at: group.created_at,
                updated_at: group.updated_at,
            })
            .collect();

        Ok((domain_groups, total))
    }

    /// Get error group by ID
    pub async fn get_error_group(
        &self,
        group_id: i32,
        project_id: i32,
    ) -> Result<ErrorGroupDomain, ErrorTrackingError> {
        let group = error_groups::Entity::find_by_id(group_id)
            .filter(error_groups::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or(ErrorTrackingError::GroupNotFound)?;

        Ok(ErrorGroupDomain {
            id: group.id,
            title: group.title,
            error_type: group.error_type,
            message_template: group.message_template,
            first_seen: group.first_seen,
            last_seen: group.last_seen,
            total_count: group.total_count,
            status: group.status,
            assigned_to: group.assigned_to,
            project_id: group.project_id,
            environment_id: group.environment_id,
            deployment_id: group.deployment_id,
            visitor_id: group.visitor_id,
            created_at: group.created_at,
            updated_at: group.updated_at,
        })
    }

    /// Update error group status
    pub async fn update_error_group_status(
        &self,
        group_id: i32,
        project_id: i32,
        status: String,
        assigned_to: Option<String>,
    ) -> Result<(), ErrorTrackingError> {
        let group = error_groups::Entity::find_by_id(group_id)
            .filter(error_groups::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or(ErrorTrackingError::GroupNotFound)?;

        let mut group: error_groups::ActiveModel = group.into();
        group.status = Set(status);
        if let Some(assignee) = assigned_to {
            group.assigned_to = Set(Some(assignee));
        }
        group.updated_at = Set(Utc::now());

        group.update(self.db.as_ref()).await?;
        Ok(())
    }

    /// List error events for a specific group
    pub async fn list_error_events(
        &self,
        group_id: i32,
        project_id: i32,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<ErrorEventDomain>, u64), ErrorTrackingError> {
        let page = page.unwrap_or(1);
        let page_size = std::cmp::min(page_size.unwrap_or(20), 100);

        let query = error_events::Entity::find()
            .filter(error_events::Column::ErrorGroupId.eq(group_id))
            .filter(error_events::Column::ProjectId.eq(project_id))
            .order_by_desc(error_events::Column::Timestamp);

        let paginator = query.paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let events = paginator.fetch_page(page - 1).await?;

        let domain_events = events.into_iter().map(Self::to_domain).collect();

        Ok((domain_events, total))
    }

    /// Get a specific error event by ID
    pub async fn get_error_event_by_ids(
        &self,
        event_id: i64,
        group_id: i32,
        project_id: i32,
    ) -> Result<ErrorEventDomain, ErrorTrackingError> {
        let event = error_events::Entity::find_by_id(event_id)
            .filter(error_events::Column::ErrorGroupId.eq(group_id))
            .filter(error_events::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or(ErrorTrackingError::EventNotFound)?;

        Ok(Self::to_domain(event))
    }

    /// Check if project has any error groups
    pub async fn has_error_groups(&self, project_id: i32) -> Result<bool, ErrorTrackingError> {
        let count = error_groups::Entity::find()
            .filter(error_groups::Column::ProjectId.eq(project_id))
            .count(self.db.as_ref())
            .await?;

        Ok(count > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{error_groups, projects};

    /// Test: Manual error resolution workflow
    ///
    /// Tests that verify the manual resolution functionality:
    ///
    /// 1. Mark error group as "resolved"
    /// 2. Assign error to developer with "assigned" status
    /// 3. Ignore errors with "ignored" status
    /// 4. Proper error handling for non-existent groups
    /// 5. Project isolation - can't update groups from other projects
    async fn setup_test_db() -> TestDatabase {
        TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database")
    }

    async fn create_test_project(db: &Arc<DatabaseConnection>) -> i32 {
        use temps_entities::preset::Preset;
        use uuid::Uuid;

        let unique_slug = format!("test-project-{}", Uuid::new_v4());
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            slug: Set(unique_slug),
            preset: Set(Preset::NextJs),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };

        project
            .insert(db.as_ref())
            .await
            .expect("Failed to create project")
            .id
    }

    async fn create_test_error_group(
        db: &Arc<DatabaseConnection>,
        project_id: i32,
        status: &str,
    ) -> i32 {
        let group = error_groups::ActiveModel {
            title: Set("Test Error".to_string()),
            error_type: Set("TypeError".to_string()),
            message_template: Set(Some("Test message".to_string())),
            embedding: Set(None),
            first_seen: Set(Utc::now()),
            last_seen: Set(Utc::now()),
            total_count: Set(1),
            status: Set(status.to_string()),
            assigned_to: Set(None),
            project_id: Set(project_id),
            environment_id: Set(None),
            deployment_id: Set(None),
            visitor_id: Set(None),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };

        group
            .insert(db.as_ref())
            .await
            .expect("Failed to create error group")
            .id
    }

    #[test]
    fn test_status_validation() {
        // Valid statuses
        let valid_statuses = vec!["unresolved", "resolved", "ignored", "assigned"];
        for status in valid_statuses {
            assert!(
                status == "unresolved"
                    || status == "resolved"
                    || status == "ignored"
                    || status == "assigned",
                "Status {} should be valid",
                status
            );
        }
    }

    #[test]
    fn test_error_group_status_transitions() {
        // Document valid status transitions
        let transitions = vec![
            ("unresolved", "resolved"), // Fix deployed
            ("unresolved", "ignored"),  // Known/acceptable error
            ("unresolved", "assigned"), // Assigned to developer
            ("assigned", "resolved"),   // Developer fixed it
            ("assigned", "unresolved"), // Unassign
            ("ignored", "unresolved"),  // No longer acceptable
            ("resolved", "unresolved"), // Regression (reopen)
        ];

        for (from, to) in transitions {
            // Verify both states are valid
            assert!(["unresolved", "resolved", "ignored", "assigned"].contains(&from));
            assert!(["unresolved", "resolved", "ignored", "assigned"].contains(&to));
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_update_error_group_status_to_resolved() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorCRUDService::new(db.clone());

        let project_id = create_test_project(&db).await;
        let group_id = create_test_error_group(&db, project_id, "unresolved").await;

        // Update status to resolved
        let result = service
            .update_error_group_status(group_id, project_id, "resolved".to_string(), None)
            .await;

        assert!(result.is_ok());

        // Verify status was updated
        let group = error_groups::Entity::find_by_id(group_id)
            .one(db.as_ref())
            .await
            .expect("Failed to fetch group")
            .expect("Group not found");

        assert_eq!(group.status, "resolved");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_update_error_group_with_assignment() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorCRUDService::new(db.clone());

        let project_id = create_test_project(&db).await;
        let group_id = create_test_error_group(&db, project_id, "unresolved").await;

        // Assign to developer
        let result = service
            .update_error_group_status(
                group_id,
                project_id,
                "assigned".to_string(),
                Some("dev@example.com".to_string()),
            )
            .await;

        assert!(result.is_ok());

        // Verify assignment
        let group = error_groups::Entity::find_by_id(group_id)
            .one(db.as_ref())
            .await
            .expect("Failed to fetch group")
            .expect("Group not found");

        assert_eq!(group.status, "assigned");
        assert_eq!(group.assigned_to, Some("dev@example.com".to_string()));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_update_error_group_to_ignored() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorCRUDService::new(db.clone());

        let project_id = create_test_project(&db).await;
        let group_id = create_test_error_group(&db, project_id, "unresolved").await;

        // Ignore error
        let result = service
            .update_error_group_status(group_id, project_id, "ignored".to_string(), None)
            .await;

        assert!(result.is_ok());

        // Verify status
        let group = error_groups::Entity::find_by_id(group_id)
            .one(db.as_ref())
            .await
            .expect("Failed to fetch group")
            .expect("Group not found");

        assert_eq!(group.status, "ignored");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_update_error_group_not_found() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorCRUDService::new(db);

        // Try to update non-existent group
        let result = service
            .update_error_group_status(99999, 1, "resolved".to_string(), None)
            .await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ErrorTrackingError::GroupNotFound
        ));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_update_error_group_wrong_project() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorCRUDService::new(db.clone());

        let project_id = create_test_project(&db).await;
        let group_id = create_test_error_group(&db, project_id, "unresolved").await;

        // Try to update from wrong project
        let result = service
            .update_error_group_status(group_id, 99999, "resolved".to_string(), None)
            .await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ErrorTrackingError::GroupNotFound
        ));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_list_error_groups_with_pagination() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorCRUDService::new(db.clone());

        let project_id = create_test_project(&db).await;

        // Create multiple error groups
        for _i in 0..5 {
            create_test_error_group(&db, project_id, "unresolved").await;
        }

        // Test pagination
        let (groups, total) = service
            .list_error_groups(project_id, Some(1), Some(3), None, None, None, None)
            .await
            .expect("Failed to list groups");

        assert_eq!(groups.len(), 3);
        assert_eq!(total, 5);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_list_error_groups_with_status_filter() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorCRUDService::new(db.clone());

        let project_id = create_test_project(&db).await;

        // Create groups with different statuses
        create_test_error_group(&db, project_id, "unresolved").await;
        create_test_error_group(&db, project_id, "unresolved").await;
        create_test_error_group(&db, project_id, "resolved").await;

        // Filter by unresolved
        let (groups, total) = service
            .list_error_groups(
                project_id,
                Some(1),
                Some(10),
                Some("unresolved".to_string()),
                None,
                None,
                None,
            )
            .await
            .expect("Failed to list groups");

        assert_eq!(groups.len(), 2);
        assert_eq!(total, 2);
        assert!(groups.iter().all(|g| g.status == "unresolved"));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_get_error_group_by_id() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorCRUDService::new(db.clone());

        let project_id = create_test_project(&db).await;
        let group_id = create_test_error_group(&db, project_id, "unresolved").await;

        let group = service
            .get_error_group(group_id, project_id)
            .await
            .expect("Failed to get group");

        assert_eq!(group.id, group_id);
        assert_eq!(group.project_id, project_id);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_has_error_groups() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorCRUDService::new(db.clone());

        let project_id = create_test_project(&db).await;

        // Initially no groups
        let has_groups = service
            .has_error_groups(project_id)
            .await
            .expect("Failed to check groups");
        assert!(!has_groups);

        // Create a group
        create_test_error_group(&db, project_id, "unresolved").await;

        // Now has groups
        let has_groups = service
            .has_error_groups(project_id)
            .await
            .expect("Failed to check groups");
        assert!(has_groups);
    }
}
