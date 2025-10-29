use chrono::{DateTime, Utc};
use sea_orm::{
    ColumnTrait, DatabaseBackend, DatabaseConnection, EntityTrait, FromQueryResult, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect, Statement,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::UtcDateTime;
use temps_entities::{error_events, error_groups};

use super::types::{ErrorGroupDomain, ErrorGroupStats, ErrorTimeSeriesPoint, ErrorTrackingError};

/// Service for error analytics, statistics, and time series data
pub struct ErrorAnalyticsService {
    db: Arc<DatabaseConnection>,
}

/// Comprehensive error dashboard statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDashboardStats {
    // Total errors count
    pub total_errors: i64,
    pub total_errors_previous_period: i64,
    pub total_errors_change_percent: f64,

    // Error groups (unique error signatures)
    pub error_groups: i64,
    pub error_groups_previous_period: i64,

    // Error rate
    pub error_rate: f64,     // Percentage of requests with errors
    pub total_requests: i64, // Total requests in period (if available)

    // Time period info
    pub start_time: UtcDateTime,
    pub end_time: UtcDateTime,
    pub comparison_start_time: Option<UtcDateTime>,
    pub comparison_end_time: Option<UtcDateTime>,
}

impl ErrorAnalyticsService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Get basic error statistics for a project
    pub async fn get_error_stats(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<ErrorGroupStats, ErrorTrackingError> {
        let mut query =
            error_groups::Entity::find().filter(error_groups::Column::ProjectId.eq(project_id));

        if let Some(env_id) = environment_id {
            query = query.filter(error_groups::Column::EnvironmentId.eq(env_id));
        }

        let groups = query.all(self.db.as_ref()).await?;

        let total = groups.len() as i64;
        let unresolved = groups.iter().filter(|g| g.status == "unresolved").count() as i64;
        let resolved = groups.iter().filter(|g| g.status == "resolved").count() as i64;
        let ignored = groups.iter().filter(|g| g.status == "ignored").count() as i64;

        Ok(ErrorGroupStats {
            total_groups: total,
            unresolved_groups: unresolved,
            resolved_groups: resolved,
            ignored_groups: ignored,
        })
    }

    /// Get error time series data with gap filling
    /// Uses TimescaleDB's time_bucket_gapfill to return all buckets between start and end time,
    /// filling missing buckets with 0 counts
    pub async fn get_error_time_series(
        &self,
        project_id: i32,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        bucket: &str, // e.g., "1h", "15m", "1d", "1 hour", "30 minutes"
    ) -> Result<Vec<ErrorTimeSeriesPoint>, ErrorTrackingError> {
        // Normalize bucket format to PostgreSQL interval format
        let interval = Self::normalize_bucket_interval(bucket);

        // Use time_bucket_gapfill to fill missing time buckets with 0 counts
        // This ensures the frontend always gets a complete time series
        // Note: time_bucket_gapfill requires subquery pattern to avoid "no top level" error
        let sql = r#"
            SELECT
                bucket::timestamptz as timestamp,
                count
            FROM (
                SELECT
                    time_bucket_gapfill($1, timestamp, $2::timestamptz, $3::timestamptz) as bucket,
                    COALESCE(COUNT(*), 0) as count
                FROM error_events
                WHERE project_id = $4
                    AND timestamp >= $2::timestamptz
                    AND timestamp <= $3::timestamptz
                GROUP BY bucket
            ) sub
            ORDER BY timestamp ASC
            "#;

        let results = ErrorTimeSeriesPoint::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            vec![
                interval.into(),
                start_time.into(),
                end_time.into(),
                project_id.into(),
            ],
        ))
        .all(self.db.as_ref())
        .await?;

        Ok(results)
    }

    /// Normalize bucket interval to PostgreSQL interval format
    /// Accepts: "1h", "15m", "1d", "1 hour", "30 minutes", etc.
    fn normalize_bucket_interval(bucket: &str) -> String {
        // Trim whitespace first
        let bucket = bucket.trim();

        // Already in PostgreSQL interval format (e.g., "1 hour", "30 minutes")
        if bucket.contains(" ") {
            return bucket.to_string();
        }

        // Parse shorthand notation (e.g., "1h", "15m", "1d")
        let bucket = bucket.to_lowercase();

        // Common patterns - validate format before parsing
        if bucket.ends_with('h') {
            let hours = bucket.trim_end_matches('h');
            if hours.parse::<u32>().is_ok() {
                return format!("{} hour", hours);
            }
        } else if bucket.ends_with('m') {
            let minutes = bucket.trim_end_matches('m');
            if minutes.parse::<u32>().is_ok() {
                return format!("{} minute", minutes);
            }
        } else if bucket.ends_with('d') {
            let days = bucket.trim_end_matches('d');
            if days.parse::<u32>().is_ok() {
                return format!("{} day", days);
            }
        } else if bucket.ends_with('w') {
            let weeks = bucket.trim_end_matches('w');
            if weeks.parse::<u32>().is_ok() {
                return format!("{} week", weeks);
            }
        }

        // Default to 1 hour if format is unrecognized
        "1 hour".to_string()
    }

    /// Get comprehensive dashboard statistics
    pub async fn get_dashboard_stats(
        &self,
        project_id: i32,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        environment_id: Option<i32>,
        compare_to_previous: bool,
    ) -> Result<ErrorDashboardStats, ErrorTrackingError> {
        // Calculate previous period if comparison is requested
        let (comparison_start, comparison_end) = if compare_to_previous {
            let duration = end_time.signed_duration_since(start_time);
            let comparison_end_time = start_time - chrono::Duration::seconds(1);
            let comparison_start_time = comparison_end_time - duration;
            (Some(comparison_start_time), Some(comparison_end_time))
        } else {
            (None, None)
        };

        // Build base query
        let mut query = error_events::Entity::find()
            .filter(error_events::Column::ProjectId.eq(project_id))
            .filter(error_events::Column::Timestamp.gte(start_time))
            .filter(error_events::Column::Timestamp.lte(end_time));

        if let Some(env_id) = environment_id {
            query = query.filter(error_events::Column::EnvironmentId.eq(env_id));
        }

        // Get total errors count
        let total_errors = query.clone().count(self.db.as_ref()).await? as i64;

        // Get unique error groups
        #[derive(FromQueryResult)]
        struct GroupCount {
            count: i64,
        }

        let groups_sql = if let Some(_env_id) = environment_id {
            r#"
                SELECT COUNT(DISTINCT error_group_id) as count
                FROM error_events
                WHERE project_id = $1
                    AND timestamp >= $2
                    AND timestamp <= $3
                    AND environment_id = $4
                "#
            .to_string()
        } else {
            r#"
                SELECT COUNT(DISTINCT error_group_id) as count
                FROM error_events
                WHERE project_id = $1
                    AND timestamp >= $2
                    AND timestamp <= $3
                "#
            .to_string()
        };

        let group_count_result = if let Some(env_id) = environment_id {
            GroupCount::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                groups_sql,
                vec![
                    project_id.into(),
                    start_time.into(),
                    end_time.into(),
                    env_id.into(),
                ],
            ))
            .one(self.db.as_ref())
            .await?
        } else {
            GroupCount::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                groups_sql,
                vec![project_id.into(), start_time.into(), end_time.into()],
            ))
            .one(self.db.as_ref())
            .await?
        };

        let error_groups = group_count_result.map(|r| r.count).unwrap_or(0);

        // Get previous period stats if comparison is enabled
        let (total_errors_previous, error_groups_previous) =
            if let (Some(comp_start), Some(comp_end)) = (comparison_start, comparison_end) {
                let mut prev_query = error_events::Entity::find()
                    .filter(error_events::Column::ProjectId.eq(project_id))
                    .filter(error_events::Column::Timestamp.gte(comp_start))
                    .filter(error_events::Column::Timestamp.lte(comp_end));

                if let Some(env_id) = environment_id {
                    prev_query = prev_query.filter(error_events::Column::EnvironmentId.eq(env_id));
                }

                let prev_total = prev_query.count(self.db.as_ref()).await? as i64;

                // Get previous period error groups count
                let prev_groups_sql = if environment_id.is_some() {
                    r#"
                        SELECT COUNT(DISTINCT error_group_id) as count
                        FROM error_events
                        WHERE project_id = $1
                            AND timestamp >= $2
                            AND timestamp <= $3
                            AND environment_id = $4
                        "#
                    .to_string()
                } else {
                    r#"
                        SELECT COUNT(DISTINCT error_group_id) as count
                        FROM error_events
                        WHERE project_id = $1
                            AND timestamp >= $2
                            AND timestamp <= $3
                        "#
                    .to_string()
                };

                let prev_group_count_result = if let Some(env_id) = environment_id {
                    GroupCount::find_by_statement(Statement::from_sql_and_values(
                        DatabaseBackend::Postgres,
                        prev_groups_sql,
                        vec![
                            project_id.into(),
                            comp_start.into(),
                            comp_end.into(),
                            env_id.into(),
                        ],
                    ))
                    .one(self.db.as_ref())
                    .await?
                } else {
                    GroupCount::find_by_statement(Statement::from_sql_and_values(
                        DatabaseBackend::Postgres,
                        prev_groups_sql,
                        vec![project_id.into(), comp_start.into(), comp_end.into()],
                    ))
                    .one(self.db.as_ref())
                    .await?
                };

                let prev_groups = prev_group_count_result.map(|r| r.count).unwrap_or(0);

                (prev_total, prev_groups)
            } else {
                (0i64, 0i64)
            };

        // Calculate change percentage
        let total_errors_change_percent = if total_errors_previous > 0 {
            ((total_errors - total_errors_previous) as f64 / total_errors_previous as f64) * 100.0
        } else if total_errors > 0 {
            100.0
        } else {
            0.0
        };

        // Calculate total requests and error rate from proxy logs
        let total_requests_sql = if let Some(env_id) = environment_id {
            Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"
                    SELECT COUNT(*) as count
                    FROM proxy_logs
                    WHERE project_id = $1
                        AND environment_id = $2
                        AND timestamp >= $3
                        AND timestamp <= $4
                "#,
                vec![
                    project_id.into(),
                    env_id.into(),
                    start_time.into(),
                    end_time.into(),
                ],
            )
        } else {
            Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"
                    SELECT COUNT(*) as count
                    FROM proxy_logs
                    WHERE project_id = $1
                        AND timestamp >= $2
                        AND timestamp <= $3
                "#,
                vec![project_id.into(), start_time.into(), end_time.into()],
            )
        };

        let total_requests_result = GroupCount::find_by_statement(total_requests_sql)
            .one(self.db.as_ref())
            .await?;
        let total_requests = total_requests_result.map(|r| r.count).unwrap_or(0);

        // Calculate error rate (percentage of requests that resulted in 5xx errors)
        let error_rate = if total_requests > 0 {
            (total_errors as f64 / total_requests as f64) * 100.0
        } else {
            0.0
        };

        Ok(ErrorDashboardStats {
            total_errors,
            total_errors_previous_period: total_errors_previous,
            total_errors_change_percent,
            error_groups,
            error_groups_previous_period: error_groups_previous,
            error_rate,
            total_requests,
            start_time,
            end_time,
            comparison_start_time: comparison_start,
            comparison_end_time: comparison_end,
        })
    }

    /// List error groups with advanced filtering
    pub async fn list_error_groups_filtered(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        status_filter: Option<String>,
        start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
        limit: Option<u64>,
    ) -> Result<Vec<ErrorGroupDomain>, ErrorTrackingError> {
        let limit = std::cmp::min(limit.unwrap_or(20), 100);

        let mut query =
            error_groups::Entity::find().filter(error_groups::Column::ProjectId.eq(project_id));

        if let Some(env_id) = environment_id {
            query = query.filter(error_groups::Column::EnvironmentId.eq(env_id));
        }

        if let Some(status) = status_filter {
            query = query.filter(error_groups::Column::Status.eq(status));
        }

        if let Some(start) = start_time {
            query = query.filter(error_groups::Column::LastSeen.gte(start));
        }

        if let Some(end) = end_time {
            query = query.filter(error_groups::Column::LastSeen.lte(end));
        }

        query = query
            .order_by_desc(error_groups::Column::LastSeen)
            .limit(limit);

        let groups = query.all(self.db.as_ref()).await?;

        Ok(groups
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
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_bucket_interval_shorthand() {
        // Test shorthand notation
        assert_eq!(
            ErrorAnalyticsService::normalize_bucket_interval("1h"),
            "1 hour"
        );
        assert_eq!(
            ErrorAnalyticsService::normalize_bucket_interval("2h"),
            "2 hour"
        );
        assert_eq!(
            ErrorAnalyticsService::normalize_bucket_interval("15m"),
            "15 minute"
        );
        assert_eq!(
            ErrorAnalyticsService::normalize_bucket_interval("30m"),
            "30 minute"
        );
        assert_eq!(
            ErrorAnalyticsService::normalize_bucket_interval("1d"),
            "1 day"
        );
        assert_eq!(
            ErrorAnalyticsService::normalize_bucket_interval("7d"),
            "7 day"
        );
        assert_eq!(
            ErrorAnalyticsService::normalize_bucket_interval("1w"),
            "1 week"
        );
    }

    #[test]
    fn test_normalize_bucket_interval_postgresql_format() {
        // Test PostgreSQL interval format (should be returned as-is)
        assert_eq!(
            ErrorAnalyticsService::normalize_bucket_interval("1 hour"),
            "1 hour"
        );
        assert_eq!(
            ErrorAnalyticsService::normalize_bucket_interval("30 minutes"),
            "30 minutes"
        );
        assert_eq!(
            ErrorAnalyticsService::normalize_bucket_interval("1 day"),
            "1 day"
        );
    }

    #[test]
    fn test_normalize_bucket_interval_case_insensitive() {
        // Test case insensitivity
        assert_eq!(
            ErrorAnalyticsService::normalize_bucket_interval("1H"),
            "1 hour"
        );
        assert_eq!(
            ErrorAnalyticsService::normalize_bucket_interval("15M"),
            "15 minute"
        );
        assert_eq!(
            ErrorAnalyticsService::normalize_bucket_interval("1D"),
            "1 day"
        );
    }

    #[test]
    fn test_normalize_bucket_interval_whitespace() {
        // Test with whitespace
        assert_eq!(
            ErrorAnalyticsService::normalize_bucket_interval("  1h  "),
            "1 hour"
        );
        assert_eq!(
            ErrorAnalyticsService::normalize_bucket_interval(" 15m "),
            "15 minute"
        );
    }

    #[test]
    fn test_normalize_bucket_interval_default() {
        // Test unrecognized format defaults to 1 hour
        assert_eq!(
            ErrorAnalyticsService::normalize_bucket_interval("invalid"),
            "1 hour"
        );
        assert_eq!(
            ErrorAnalyticsService::normalize_bucket_interval(""),
            "1 hour"
        );
    }
}
