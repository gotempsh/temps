// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseBackend, DatabaseConnection, EntityTrait,
    FromQueryResult, PaginatorTrait, QueryFilter, QueryOrder, Set, Statement,
};
use std::collections::HashMap;
use std::sync::Arc;
use temps_entities::{deployments, error_events, error_groups};

use super::types::{
    ErrorEventDomain, ErrorGroupDeploymentSummary, ErrorGroupDomain, ErrorTrackingError,
};

/// Aggregate counts of events and distinct affected users for one error group in a time window.
#[derive(FromQueryResult)]
struct ErrorGroupRangeAggregate {
    error_group_id: i32,
    events_in_range: i64,
    affected_users: i64,
}

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

    /// List error groups with filtering and pagination.
    ///
    /// When both `start_date` and `end_date` are supplied, only groups that have at least one
    /// `error_events` row with `timestamp` in `[start_date, end_date]` are returned.  The query
    /// against `error_events` is always time-bounded so it uses the hypertable indexes safely.
    ///
    /// The returned `ErrorGroupDomain` items will have `events_in_range` and `affected_users`
    /// set (for the current page only) when a date range is provided; they are `None` otherwise.
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
        start_date: Option<DateTime<Utc>>,
        end_date: Option<DateTime<Utc>>,
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

        // Apply time-range filter: restrict to groups that have at least one event in the window.
        // The inner query is always time-bounded to keep hypertable lookups safe.
        let date_range = match (start_date, end_date) {
            (Some(start), Some(end)) => Some((start, end)),
            _ => None,
        };

        if let Some((start, end)) = date_range {
            let candidate_ids = self
                .get_candidate_group_ids(project_id, start, end, environment_id)
                .await?;
            if candidate_ids.is_empty() {
                return Ok((vec![], 0));
            }
            query = query.filter(error_groups::Column::Id.is_in(candidate_ids));
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

        let mut domain_groups: Vec<ErrorGroupDomain> = groups
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
                events_in_range: None,
                affected_users: None,
                deployment: None,
            })
            .collect();

        // Compute per-group windowed aggregates for the current page only.
        // Only runs when a date range was requested, never unbounded against error_events.
        if let Some((start, end)) = date_range {
            let group_ids: Vec<i32> = domain_groups.iter().map(|g| g.id).collect();
            if !group_ids.is_empty() {
                let aggregates = self
                    .compute_range_aggregates(project_id, &group_ids, start, end, environment_id)
                    .await?;
                let agg_map: HashMap<i32, (i64, i64)> = aggregates
                    .into_iter()
                    .map(|a| (a.error_group_id, (a.events_in_range, a.affected_users)))
                    .collect();
                for group in &mut domain_groups {
                    let (events, users) = agg_map.get(&group.id).copied().unwrap_or((0, 0));
                    group.events_in_range = Some(events);
                    group.affected_users = Some(users);
                }
            }
        }

        // Resolve `deployment_id` -> commit info for the current page only. `deployments` is a
        // small regular table (not a hypertable), and the lookup is bounded by page size, so
        // this is independent of the date-range filter above.
        let deployment_ids: Vec<i32> = domain_groups
            .iter()
            .filter_map(|g| g.deployment_id)
            .collect();
        if !deployment_ids.is_empty() {
            let deployment_map = self.resolve_deployments(&deployment_ids).await?;
            for group in &mut domain_groups {
                if let Some(dep_id) = group.deployment_id {
                    group.deployment = deployment_map.get(&dep_id).cloned();
                }
            }
        }

        Ok((domain_groups, total))
    }

    /// Batch-resolve deployment IDs to lightweight commit summaries. Not N+1 — one query for
    /// however many distinct deployment IDs appear on the current page (bounded by page size).
    async fn resolve_deployments(
        &self,
        deployment_ids: &[i32],
    ) -> Result<HashMap<i32, ErrorGroupDeploymentSummary>, ErrorTrackingError> {
        let rows = deployments::Entity::find()
            .filter(deployments::Column::Id.is_in(deployment_ids.to_vec()))
            .all(self.db.as_ref())
            .await?;

        Ok(rows
            .into_iter()
            .map(|d| {
                (
                    d.id,
                    ErrorGroupDeploymentSummary {
                        id: d.id,
                        commit_hash: d.commit_sha,
                        commit_message: d.commit_message,
                        branch: d.branch_ref,
                    },
                )
            })
            .collect())
    }

    /// Return the set of `error_group_id` values that have at least one event in the given
    /// time window for the project (and optionally a specific environment).
    ///
    /// Always time-bounded — safe to call against the TimescaleDB hypertable.
    async fn get_candidate_group_ids(
        &self,
        project_id: i32,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        environment_id: Option<i32>,
    ) -> Result<Vec<i32>, ErrorTrackingError> {
        #[derive(FromQueryResult)]
        struct ErrorGroupIdRow {
            error_group_id: i32,
        }

        let rows = if let Some(env_id) = environment_id {
            ErrorGroupIdRow::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"
                    SELECT DISTINCT error_group_id
                    FROM error_events
                    WHERE project_id = $1
                        AND timestamp >= $2
                        AND timestamp <= $3
                        AND environment_id = $4
                "#,
                vec![project_id.into(), start.into(), end.into(), env_id.into()],
            ))
            .all(self.db.as_ref())
            .await?
        } else {
            ErrorGroupIdRow::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"
                    SELECT DISTINCT error_group_id
                    FROM error_events
                    WHERE project_id = $1
                        AND timestamp >= $2
                        AND timestamp <= $3
                "#,
                vec![project_id.into(), start.into(), end.into()],
            ))
            .all(self.db.as_ref())
            .await?
        };

        Ok(rows.into_iter().map(|r| r.error_group_id).collect())
    }

    /// Compute `events_in_range` and `affected_users` for a specific set of group IDs within
    /// the given time window.  Runs a single aggregation query — not N+1 per group.
    ///
    /// `COUNT(DISTINCT visitor_id)` inherently skips NULLs in PostgreSQL.
    async fn compute_range_aggregates(
        &self,
        project_id: i32,
        group_ids: &[i32],
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        environment_id: Option<i32>,
    ) -> Result<Vec<ErrorGroupRangeAggregate>, ErrorTrackingError> {
        if group_ids.is_empty() {
            return Ok(vec![]);
        }

        // Build individual placeholders starting at $4 for group IDs.
        // Page size is capped at 100, so this list is always bounded.
        let id_placeholders: String = group_ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("${}", i + 4))
            .collect::<Vec<_>>()
            .join(", ");

        let mut params: Vec<sea_orm::Value> = vec![project_id.into(), start.into(), end.into()];
        for id in group_ids {
            params.push((*id).into());
        }

        let sql = if let Some(env_id) = environment_id {
            let env_placeholder = format!("${}", group_ids.len() + 4);
            params.push(env_id.into());
            format!(
                r#"
                    SELECT
                        error_group_id,
                        COUNT(*) AS events_in_range,
                        COUNT(DISTINCT visitor_id) AS affected_users
                    FROM error_events
                    WHERE project_id = $1
                        AND timestamp >= $2
                        AND timestamp <= $3
                        AND error_group_id IN ({})
                        AND environment_id = {}
                    GROUP BY error_group_id
                "#,
                id_placeholders, env_placeholder
            )
        } else {
            format!(
                r#"
                    SELECT
                        error_group_id,
                        COUNT(*) AS events_in_range,
                        COUNT(DISTINCT visitor_id) AS affected_users
                    FROM error_events
                    WHERE project_id = $1
                        AND timestamp >= $2
                        AND timestamp <= $3
                        AND error_group_id IN ({})
                    GROUP BY error_group_id
                "#,
                id_placeholders
            )
        };

        ErrorGroupRangeAggregate::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            params,
        ))
        .all(self.db.as_ref())
        .await
        .map_err(ErrorTrackingError::Database)
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

        let deployment = if let Some(dep_id) = group.deployment_id {
            self.resolve_deployments(&[dep_id]).await?.remove(&dep_id)
        } else {
            None
        };

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
            events_in_range: None,
            affected_users: None,
            deployment,
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
        match assigned_to {
            Some(ref s) if s.is_empty() => {
                // Empty string is the sentinel for "clear the assignment"
                group.assigned_to = Set(None);
            }
            Some(assignee) => {
                group.assigned_to = Set(Some(assignee));
            }
            None => {
                // Field omitted — leave the existing value untouched
            }
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
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
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

    #[tokio::test]
    async fn test_global_errors_pagination_and_access() {
        use sea_orm::ConnectionTrait;
        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                eprintln!("Skipping global errors database test: {error}");
                return;
            }
            Err(error) => panic!("Global errors test database setup failed: {error}"),
        };
        let db = test_db.connection_arc();
        let service = ErrorCRUDService::new(db.clone());
        let a = create_test_project(&db).await;
        let b = create_test_project(&db).await;
        let first = create_test_error_group(&db, a, "unresolved").await;
        let second = create_test_error_group(&db, b, "unresolved").await;
        let start = Utc::now() - chrono::Duration::hours(1);
        for (group, project) in [(first, a), (second, b)] {
            db.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
                "INSERT INTO error_events (error_group_id, project_id, timestamp, fingerprint_hash, exception_type, source) VALUES ($1, $2, NOW(), $3, 'TypeError', 'test')",
                vec![group.into(), project.into(), format!("group-{group}").into()],
            )).await.expect("insert error event");
        }
        let end = Utc::now() + chrono::Duration::seconds(1);
        let (page1, total) = service
            .list_global_error_groups(None, &[], 1, 1, None, None, start, end)
            .await
            .expect("global first page");
        let (page2, total2) = service
            .list_global_error_groups(None, &[], 2, 1, None, None, start, end)
            .await
            .expect("global second page");
        assert_eq!((total, total2), (2, 2));
        assert_ne!(page1[0].project_id, page2[0].project_id);
        assert_eq!(page1[0].events_in_range, 1);
        let (visible, count) = service
            .list_global_error_groups(None, &[b], 1, 20, None, None, start, end)
            .await
            .expect("hidden projects");
        assert_eq!(count, 1);
        assert_eq!(visible[0].project_id, a);
        let (scoped, count) = service
            .list_global_error_groups(Some(b), &[], 1, 20, None, None, start, end)
            .await
            .expect("token scope");
        assert_eq!(count, 1);
        assert_eq!(scoped[0].project_id, b);
        let (filtered, count) = service
            .list_global_error_groups(None, &[], 1, 20, Some("resolved"), None, start, end)
            .await
            .expect("status filter");
        assert_eq!(count, 0);
        assert!(filtered.is_empty());
        let (_, count) = service
            .list_global_error_groups(
                None,
                &[],
                1,
                20,
                None,
                Some("no-matching-issue"),
                start,
                end,
            )
            .await
            .expect("global search");
        assert_eq!(count, 0);
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
            .list_error_groups(
                project_id,
                Some(1),
                Some(3),
                None,
                None,
                None,
                None,
                None,
                None,
            )
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

/// One issue in the instance ledger, with identity resolved in the same query.
#[derive(Debug, FromQueryResult)]
pub struct GlobalErrorGroup {
    pub id: i32,
    pub title: String,
    pub error_type: String,
    pub status: String,
    pub assigned_to: Option<String>,
    pub project_id: i32,
    pub project_name: String,
    pub project_slug: String,
    pub environment_name: Option<String>,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub total_count: i64,
    pub events_in_range: i64,
    pub affected_users: i64,
}

impl ErrorCRUDService {
    /// Control-plane query: pagination and access filtering happen before fetching rows.
    /// Event aggregates cover only the returned page and a bounded time window.
    #[allow(clippy::too_many_arguments)]
    pub async fn list_global_error_groups(
        &self,
        project_id: Option<i32>,
        hidden_projects: &[i32],
        page: u64,
        page_size: u64,
        status: Option<&str>,
        search: Option<&str>,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<(Vec<GlobalErrorGroup>, u64), ErrorTrackingError> {
        use sea_orm::ConnectionTrait;
        let (filter, mut values) =
            global_error_filter(project_id, hidden_projects, status, search, start, end);
        let count_sql = format!("SELECT COUNT(*) AS total FROM error_groups g JOIN projects p ON p.id = g.project_id WHERE {filter}");
        let count = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                count_sql,
                values.clone(),
            ))
            .await?
            .ok_or_else(|| {
                ErrorTrackingError::Validation("Global error count returned no result".into())
            })?;
        let total: i64 = count.try_get("", "total")?;
        values.push(page_size.into());
        let limit = values.len();
        values.push(((page - 1) * page_size).into());
        let offset = values.len();
        let sql = format!(
            r#"
            WITH page AS (
                SELECT g.*, p.name AS project_name, p.slug AS project_slug
                FROM error_groups g JOIN projects p ON p.id = g.project_id
                WHERE {filter}
                ORDER BY g.last_seen DESC, g.id DESC LIMIT ${limit} OFFSET ${offset}
            )
            SELECT g.id, g.title, g.error_type, g.status, g.assigned_to, g.project_id,
                   g.project_name, g.project_slug, env.name AS environment_name,
                   g.first_seen, g.last_seen, g.total_count::bigint AS total_count,
                   counts.events_in_range, counts.affected_users
            FROM page g
            LEFT JOIN environments env ON env.id = g.environment_id AND env.project_id = g.project_id
            CROSS JOIN LATERAL (
                SELECT COUNT(*) AS events_in_range, COUNT(DISTINCT e.visitor_id) AS affected_users
                FROM error_events e WHERE e.error_group_id = g.id AND e.project_id = g.project_id
                AND e.timestamp >= $1 AND e.timestamp <= $2
            ) counts
            ORDER BY g.last_seen DESC, g.id DESC
        "#
        );
        let rows = GlobalErrorGroup::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            values,
        ))
        .all(self.db.as_ref())
        .await?;
        Ok((rows, total as u64))
    }
}

fn global_error_filter(
    project_id: Option<i32>,
    hidden_projects: &[i32],
    status: Option<&str>,
    search: Option<&str>,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> (String, Vec<sea_orm::Value>) {
    let mut values: Vec<sea_orm::Value> = vec![start.into(), end.into()];
    let mut conditions = vec!["p.is_deleted = FALSE AND EXISTS (SELECT 1 FROM error_events e WHERE e.error_group_id = g.id AND e.project_id = g.project_id AND e.timestamp >= $1 AND e.timestamp <= $2)".to_string()];
    if let Some(id) = project_id {
        values.push(id.into());
        conditions.push(format!("g.project_id = ${}", values.len()));
    }
    // One array parameter, never one parameter or request per hidden project.
    if !hidden_projects.is_empty() {
        values.push(hidden_projects.to_vec().into());
        conditions.push(format!("NOT (g.project_id = ANY(${}))", values.len()));
    }
    if let Some(status) = status {
        values.push(status.to_string().into());
        conditions.push(format!("g.status = ${}", values.len()));
    }
    if let Some(search) = search.filter(|s| !s.trim().is_empty()) {
        values.push(
            format!(
                "%{}%",
                search
                    .trim()
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            )
            .into(),
        );
        let index = values.len();
        conditions.push(format!("(g.title ILIKE ${index} OR g.error_type ILIKE ${index} OR p.name ILIKE ${index} OR p.slug ILIKE ${index})"));
    }
    (conditions.join(" AND "), values)
}

#[cfg(test)]
mod global_error_tests {
    use super::*;

    #[tokio::test]
    async fn global_errors_postgres_pagination_and_scope() {
        use sea_orm::{ConnectOptions, ConnectionTrait, Database};
        let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
            eprintln!("Skipping PostgreSQL integration: TEST_DATABASE_URL is not configured");
            return;
        };
        let mut options = ConnectOptions::new(url);
        options.max_connections(1).min_connections(1);
        let db = Arc::new(Database::connect(options).await.unwrap());
        db.execute_unprepared(r#"
            CREATE TEMP TABLE projects(id int, name text, slug text, is_deleted bool DEFAULT false);
            CREATE TEMP TABLE environments(id int, project_id int, name text);
            CREATE TEMP TABLE error_groups(id int, title text, error_type text, status text, assigned_to text,
                project_id int, environment_id int, first_seen timestamptz, last_seen timestamptz, total_count bigint);
            CREATE TEMP TABLE error_events(error_group_id int, project_id int, timestamp timestamptz, visitor_id int);
            INSERT INTO projects SELECT i, 'Project '||i, 'project-'||i, false FROM generate_series(1,105) i;
            INSERT INTO environments VALUES(1,1,'production');
            INSERT INTO error_groups SELECT i, 'Failure '||i, 'RuntimeError', 'unresolved', NULL,
                i, 1, '2026-01-01T01:00:00Z', '2026-01-01T01:00:00Z'::timestamptz+i*interval '1 minute', 1
                FROM generate_series(1,105) i;
            INSERT INTO error_events SELECT i,i,'2026-01-01T01:00:00Z',i FROM generate_series(1,105) i;
            INSERT INTO error_events VALUES(104,105,'2026-01-01T01:00:00Z',999);
        "#).await.unwrap();
        let service = ErrorCRUDService::new(db);
        let start = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let end = start + chrono::Duration::days(1);
        let (rows, total) = service
            .list_global_error_groups(None, &[105], 1, 2, None, None, start, end)
            .await
            .unwrap();
        assert_eq!(total, 104);
        assert_eq!(
            rows.iter().map(|row| row.project_id).collect::<Vec<_>>(),
            vec![104, 103]
        );
        assert_eq!(
            rows[0].events_in_range, 1,
            "cross-project events must not contaminate counts"
        );
        assert!(
            rows[0].environment_name.is_none(),
            "cross-project environment must not leak"
        );
        let (rows, total) = service
            .list_global_error_groups(None, &[105], 52, 2, None, None, start, end)
            .await
            .unwrap();
        assert_eq!(total, 104);
        assert_eq!(
            rows.iter().map(|row| row.project_id).collect::<Vec<_>>(),
            vec![2, 1]
        );
        let (rows, total) = service
            .list_global_error_groups(
                Some(1),
                &[],
                1,
                20,
                Some("unresolved"),
                Some("project-1"),
                start,
                end,
            )
            .await
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(rows[0].environment_name.as_deref(), Some("production"));
        let (rows, total) = service
            .list_global_error_groups(Some(105), &[105], 1, 20, None, None, start, end)
            .await
            .unwrap();
        assert_eq!(total, 0);
        assert!(rows.is_empty());
    }

    #[test]
    fn scope_and_hidden_projects_apply_before_pagination() {
        let (sql, values) = global_error_filter(
            Some(7),
            &[8, 9],
            Some("unresolved"),
            Some("50%_off"),
            Utc::now(),
            Utc::now(),
        );
        assert!(sql.contains("g.project_id = $3"));
        assert!(sql.contains("NOT (g.project_id = ANY($4))"));
        assert!(sql.contains("g.status = $5"));
        assert!(sql.contains("p.slug ILIKE $6"));
        assert_eq!(values.len(), 6);
        assert_eq!(values[5], sea_orm::Value::from("%50\\%\\_off%"));
        assert!(sql.contains("e.project_id = g.project_id"));
        assert!(sql.contains("e.timestamp >= $1 AND e.timestamp <= $2"));
    }
}
