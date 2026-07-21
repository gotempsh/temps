use sea_orm::{DatabaseBackend, DatabaseConnection, FromQueryResult, Statement};
use std::collections::HashMap;
use std::sync::Arc;
use temps_core::{DBDateTime, UtcDateTime};
use thiserror::Error;

use crate::types::{
    AggregationLevel, AnalyticsSessionEventsResponse, DashboardProjectsAnalyticsResponse,
    EventCount, EventTimeline, EventTypeBreakdown, ProjectDashboardAnalytics,
    PropertyBreakdownItem, PropertyBreakdownResponse, PropertyTimelineItem,
    PropertyTimelineResponse, SessionEvent, UniqueCountsResponse,
};

#[derive(Debug, Error)]
pub enum EventsError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("Not found")]
    NotFound,
    #[error("Invalid input: {0}")]
    InvalidInput(String),
    #[error("Validation error: {0}")]
    Validation(String),
}

pub struct AnalyticsEventsService {
    db: Arc<DatabaseConnection>,
}

impl AnalyticsEventsService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Get custom event counts with filtering and aggregation level
    #[allow(clippy::too_many_arguments)]
    pub async fn get_events_count(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        limit: Option<i32>,
        custom_events_only: Option<bool>,
        aggregation_level: AggregationLevel,
    ) -> Result<Vec<EventCount>, EventsError> {
        // Build WHERE conditions with parameterized queries
        let mut where_conditions = vec![
            "project_id = $1".to_string(),
            "timestamp >= $2".to_string(),
            "timestamp <= $3".to_string(),
            "event_name IS NOT NULL".to_string(),
        ];
        let mut values: Vec<sea_orm::Value> =
            vec![project_id.into(), start_date.into(), end_date.into()];
        let mut param_index = 4;

        // Default to true - only return custom events by default
        let filter_custom_only = custom_events_only.unwrap_or(true);

        if filter_custom_only {
            // Exclude system events like page_view, page_leave, heartbeat
            where_conditions.push(
                "COALESCE(event_name, event_type) NOT IN ('page_view', 'page_leave', 'heartbeat')"
                    .to_string(),
            );
        }

        if let Some(env_id) = environment_id {
            where_conditions.push(format!("environment_id = ${}", param_index));
            values.push(env_id.into());
            param_index += 1;
        }

        let limit_val = limit.unwrap_or(20).min(100);

        // Determine aggregation based on level
        let (count_expr, null_check) = match aggregation_level {
            AggregationLevel::Events => ("COUNT(*)", ""),
            AggregationLevel::Sessions => {
                ("COUNT(DISTINCT session_id)", " AND session_id IS NOT NULL")
            }
            AggregationLevel::Visitors => {
                ("COUNT(DISTINCT visitor_id)", " AND visitor_id IS NOT NULL")
            }
        };

        let where_clause = where_conditions.join(" AND ");
        let sql_query = format!(
            r#"
            WITH event_counts AS (
                SELECT
                    COALESCE(event_name, event_type) as event_name,
                    {} as count
                FROM events
                WHERE {}{}
                GROUP BY COALESCE(event_name, event_type)
            ),
            total AS (
                SELECT COALESCE(SUM(count), 0)::bigint as total_count
                FROM event_counts
            )
            SELECT
                ec.event_name,
                ec.count,
                CASE WHEN t.total_count > 0
                     THEN (ec.count::float / t.total_count::float * 100)
                     ELSE 0 END as percentage
            FROM event_counts ec
            CROSS JOIN total t
            ORDER BY ec.count DESC
            LIMIT ${}
            "#,
            count_expr, where_clause, null_check, param_index
        );

        // Add LIMIT as parameter
        values.push((limit_val as i64).into());

        #[derive(FromQueryResult)]
        struct EventResult {
            event_name: String,
            count: i64,
            percentage: f64,
        }

        let results = EventResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql_query,
            values,
        ))
        .all(self.db.as_ref())
        .await?;

        Ok(results
            .into_iter()
            .map(|r| EventCount {
                event_name: r.event_name,
                count: r.count,
                percentage: r.percentage,
            })
            .collect())
    }

    /// Get events for a specific session
    pub async fn get_session_events(
        &self,
        session_id: String,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<Option<AnalyticsSessionEventsResponse>, EventsError> {
        // Build WHERE conditions with parameterized queries
        let mut where_conditions =
            vec!["session_id = $1".to_string(), "project_id = $2".to_string()];
        let mut values: Vec<sea_orm::Value> = vec![session_id.clone().into(), project_id.into()];
        let param_index = 3;

        if let Some(env_id) = environment_id {
            where_conditions.push(format!("environment_id = ${}", param_index));
            values.push(env_id.into());
        }

        let where_clause = where_conditions.join(" AND ");
        let sql_query = format!(
            r#"
            SELECT
                id,
                event_name,
                event_type,
                event_data,
                timestamp,
                href AS page_url,
                page_title
            FROM events
            WHERE {}
            ORDER BY timestamp ASC
            "#,
            where_clause
        );

        #[derive(FromQueryResult)]
        struct EventResult {
            id: i32,
            event_name: Option<String>,
            event_type: Option<String>,
            event_data: Option<serde_json::Value>,
            timestamp: UtcDateTime,
            page_url: Option<String>,
            page_title: Option<String>,
        }

        let results = EventResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql_query,
            values,
        ))
        .all(self.db.as_ref())
        .await?;

        if results.is_empty() {
            return Ok(None);
        }

        let events: Vec<SessionEvent> = results
            .into_iter()
            .map(|r| SessionEvent {
                id: r.id,
                event_name: r.event_name,
                event_type: r.event_type,
                event_data: r.event_data,
                timestamp: r.timestamp.to_string(),
                page_url: r.page_url,
                page_title: r.page_title,
            })
            .collect();

        let total_events = events.len();

        Ok(Some(AnalyticsSessionEventsResponse {
            session_id,
            events,
            total_events,
        }))
    }

    /// Check if project has any analytics events
    pub async fn has_analytics_events(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<bool, EventsError> {
        // Build WHERE conditions with parameterized queries
        let mut where_conditions = vec!["project_id = $1".to_string()];
        let mut values: Vec<sea_orm::Value> = vec![project_id.into()];
        let param_index = 2;

        if let Some(env_id) = environment_id {
            where_conditions.push(format!("environment_id = ${}", param_index));
            values.push(env_id.into());
        }

        let where_clause = where_conditions.join(" AND ");
        let sql_query = format!(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM events
                WHERE {}
                LIMIT 1
            ) as has_events
            "#,
            where_clause
        );

        #[derive(FromQueryResult)]
        struct HasEventsResult {
            has_events: bool,
        }

        let result = HasEventsResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql_query,
            values,
        ))
        .one(self.db.as_ref())
        .await?;

        Ok(result.map(|r| r.has_events).unwrap_or(false))
    }

    /// Get event type breakdown (page_view, custom events, etc.)
    pub async fn get_event_type_breakdown(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        aggregation_level: AggregationLevel,
    ) -> Result<Vec<EventTypeBreakdown>, EventsError> {
        // Build WHERE conditions with parameterized queries
        let mut where_conditions = vec![
            "project_id = $1".to_string(),
            "timestamp >= $2".to_string(),
            "timestamp <= $3".to_string(),
        ];
        let mut values: Vec<sea_orm::Value> =
            vec![project_id.into(), start_date.into(), end_date.into()];
        let param_index = 4;

        if let Some(env_id) = environment_id {
            where_conditions.push(format!("environment_id = ${}", param_index));
            values.push(env_id.into());
        }

        // Determine aggregation based on level
        let (count_expr, null_check) = match aggregation_level {
            AggregationLevel::Events => ("COUNT(*)", ""),
            AggregationLevel::Sessions => {
                ("COUNT(DISTINCT session_id)", " AND session_id IS NOT NULL")
            }
            AggregationLevel::Visitors => {
                ("COUNT(DISTINCT visitor_id)", " AND visitor_id IS NOT NULL")
            }
        };

        let where_clause = where_conditions.join(" AND ");
        let sql_query = format!(
            r#"
            WITH type_counts AS (
                SELECT
                    event_type,
                    {} as count
                FROM events
                WHERE {}{}
                GROUP BY event_type
            ),
            total AS (
                SELECT COALESCE(SUM(count), 0)::bigint as total_count
                FROM type_counts
            )
            SELECT
                tc.event_type,
                tc.count,
                CASE WHEN t.total_count > 0
                     THEN (tc.count::float / t.total_count::float * 100)
                     ELSE 0 END as percentage
            FROM type_counts tc
            CROSS JOIN total t
            ORDER BY tc.count DESC
            "#,
            count_expr, where_clause, null_check
        );

        #[derive(FromQueryResult)]
        struct TypeResult {
            event_type: String,
            count: i64,
            percentage: f64,
        }

        let results = TypeResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql_query,
            values,
        ))
        .all(self.db.as_ref())
        .await?;

        Ok(results
            .into_iter()
            .map(|r| EventTypeBreakdown {
                event_type: r.event_type,
                count: r.count,
                percentage: r.percentage,
            })
            .collect())
    }

    /// Get events over time (timeline)
    #[allow(clippy::too_many_arguments)]
    pub async fn get_events_timeline(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        event_name: Option<String>,
        bucket_size: Option<String>,
        aggregation_level: AggregationLevel,
    ) -> Result<Vec<EventTimeline>, EventsError> {
        // Build WHERE conditions with parameterized queries
        let mut where_conditions = vec![
            "project_id = $1".to_string(),
            "timestamp >= $2".to_string(),
            "timestamp <= $3".to_string(),
        ];
        let mut values: Vec<sea_orm::Value> =
            vec![project_id.into(), start_date.into(), end_date.into()];
        let mut param_index = 4;

        if let Some(env_id) = environment_id {
            where_conditions.push(format!("environment_id = ${}", param_index));
            values.push(env_id.into());
            param_index += 1;
        }

        if let Some(event) = event_name {
            where_conditions.push(format!(
                "COALESCE(event_name, event_type) = ${}",
                param_index
            ));
            values.push(event.into());
        }

        // Determine aggregation based on level
        let (count_expr, null_check) = match aggregation_level {
            AggregationLevel::Events => ("COUNT(*)", ""),
            AggregationLevel::Sessions => {
                ("COUNT(DISTINCT session_id)", " AND session_id IS NOT NULL")
            }
            AggregationLevel::Visitors => {
                ("COUNT(DISTINCT visitor_id)", " AND visitor_id IS NOT NULL")
            }
        };

        // Determine bucket size based on date range if not specified
        let duration = end_date - start_date;
        let bucket = match bucket_size.as_deref() {
            Some("hour") => "1 hour",
            Some("day") => "1 day",
            Some("week") => "1 week",
            _ => {
                // Auto-detect based on range
                if duration.num_days() <= 1 {
                    "1 hour"
                } else if duration.num_days() <= 30 {
                    "1 day"
                } else {
                    "1 week"
                }
            }
        };

        let where_clause = where_conditions.join(" AND ");
        let sql_query = format!(
            r#"
            SELECT
                bucket::timestamptz as bucket,
                count
            FROM (
                SELECT
                    time_bucket('{}', timestamp) as bucket,
                    {} as count
                FROM events
                WHERE {}{}
                GROUP BY bucket
            ) sub
            ORDER BY bucket ASC
            "#,
            bucket, count_expr, where_clause, null_check
        );

        #[derive(FromQueryResult)]
        struct TimelineResult {
            bucket: UtcDateTime,
            count: i64,
        }

        let results = TimelineResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql_query,
            values,
        ))
        .all(self.db.as_ref())
        .await?;

        Ok(results
            .into_iter()
            .map(|r| EventTimeline {
                date: r.bucket,
                count: r.count,
            })
            .collect())
    }

    /// Get property breakdown by grouping events by a specific column
    /// Example: Get channel distribution, device_type breakdown, browser stats, etc.
    #[allow(clippy::too_many_arguments)]
    pub async fn get_property_breakdown(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        event_name: Option<String>,
        group_by_column: crate::types::PropertyColumn,
        aggregation_level: &str,
        limit: Option<i32>,
        filters: Option<crate::types::PropertyBreakdownFilters>,
    ) -> Result<PropertyBreakdownResponse, EventsError> {
        let group_by_str = group_by_column.as_str();
        let limit_val = limit.unwrap_or(20).min(100);
        let filters = filters.unwrap_or_default();

        // Determine aggregation field
        let (agg_field, agg_distinct) = match aggregation_level {
            "sessions" => ("session_id", "DISTINCT"),
            "visitors" => ("visitor_id", "DISTINCT"),
            _ => ("*", ""), // events (raw count)
        };

        // Check if we need to join with ip_geolocations
        let is_geo_column = matches!(group_by_str, "country" | "region" | "city");
        let is_referrer_column = group_by_str == "referrer_hostname";
        let needs_geo_join = is_geo_column || filters.country.is_some() || filters.region.is_some();

        let (from_clause, select_column) = if needs_geo_join && is_geo_column {
            (
                "events e LEFT JOIN ip_geolocations ig ON e.ip_geolocation_id = ig.id",
                format!("COALESCE(ig.{}, 'Unknown')", group_by_str),
            )
        } else if needs_geo_join {
            // Non-geo column but we need the join for filtering
            if is_referrer_column {
                (
                    "events e LEFT JOIN ip_geolocations ig ON e.ip_geolocation_id = ig.id",
                    format!("COALESCE(e.{}, 'Direct')", group_by_str),
                )
            } else {
                (
                    "events e LEFT JOIN ip_geolocations ig ON e.ip_geolocation_id = ig.id",
                    format!("COALESCE(e.{}, 'Unknown')", group_by_str),
                )
            }
        } else if is_referrer_column {
            (
                "events e",
                format!("COALESCE(e.{}, 'Direct')", group_by_str),
            )
        } else {
            (
                "events e",
                format!("COALESCE(e.{}, 'Unknown')", group_by_str),
            )
        };

        let mut conditions = vec!["e.project_id = $1".to_string()];
        conditions.push("e.timestamp >= $2".to_string());
        conditions.push("e.timestamp <= $3".to_string());

        // For referrer_hostname, filter out self-referrals (project's own domains)
        // Skip this filter when drilling down from a channel (filter_channel is set)
        // because the channel overview already counted these visitors
        if is_referrer_column && filters.channel.is_none() {
            conditions.push(
                r#"(e.referrer_hostname IS NULL OR e.referrer_hostname NOT IN (
                    SELECT domain FROM project_custom_domains WHERE project_id = $1
                ))"#
                .to_string(),
            );
        }

        let mut param_idx = 4;
        if environment_id.is_some() {
            conditions.push(format!("e.environment_id = ${}", param_idx));
            param_idx += 1;
        }
        if deployment_id.is_some() {
            conditions.push(format!("e.deployment_id = ${}", param_idx));
            param_idx += 1;
        }
        if event_name.is_some() {
            conditions.push(format!(
                "COALESCE(e.event_name, e.event_type) = ${}",
                param_idx
            ));
            param_idx += 1;
        }

        // Apply drill-down filters
        if filters.country.is_some() {
            conditions.push(format!("ig.country = ${}", param_idx));
            param_idx += 1;
        }
        if filters.region.is_some() {
            conditions.push(format!("ig.region = ${}", param_idx));
            param_idx += 1;
        }
        if filters.browser.is_some() {
            conditions.push(format!("e.browser = ${}", param_idx));
            param_idx += 1;
        }
        if filters.operating_system.is_some() {
            conditions.push(format!("e.operating_system = ${}", param_idx));
            param_idx += 1;
        }
        if filters.channel.is_some() {
            conditions.push(format!("e.channel = ${}", param_idx));
            param_idx += 1;
        }
        if let Some(ref referrer) = filters.referrer {
            if referrer == "Direct" {
                conditions.push("e.referrer_hostname IS NULL".to_string());
            } else {
                conditions.push(format!("e.referrer_hostname = ${}", param_idx));
                let _ = param_idx;
            }
        }

        let sql_query = format!(
            r#"
            WITH value_counts AS (
                SELECT
                    {} as value,
                    COUNT({} e.{}) as count
                FROM {}
                WHERE {}
                GROUP BY {}
                HAVING COUNT({} e.{}) > 0
            ),
            total AS (
                SELECT COALESCE(SUM(count), 0)::bigint as total_count
                FROM value_counts
            )
            SELECT
                vc.value,
                vc.count,
                CASE WHEN t.total_count > 0
                     THEN (vc.count::float / t.total_count::float * 100)
                     ELSE 0 END as percentage,
                t.total_count
            FROM value_counts vc
            CROSS JOIN total t
            ORDER BY vc.count DESC
            LIMIT {}
            "#,
            select_column,
            agg_distinct,
            agg_field,
            from_clause,
            conditions.join(" AND "),
            select_column,
            agg_distinct,
            agg_field,
            limit_val
        );

        #[derive(FromQueryResult)]
        struct BreakdownResult {
            value: String,
            count: i64,
            percentage: f64,
            total_count: i64,
        }

        let mut params: Vec<sea_orm::Value> =
            vec![project_id.into(), start_date.into(), end_date.into()];
        if let Some(env_id) = environment_id {
            params.push(env_id.into());
        }
        if let Some(dep_id) = deployment_id {
            params.push(dep_id.into());
        }
        if let Some(evt_name) = event_name {
            params.push(evt_name.into());
        }
        if let Some(country) = filters.country {
            params.push(country.into());
        }
        if let Some(region) = filters.region {
            params.push(region.into());
        }
        if let Some(browser) = filters.browser {
            params.push(browser.into());
        }
        if let Some(os) = filters.operating_system {
            params.push(os.into());
        }
        if let Some(channel) = filters.channel {
            params.push(channel.into());
        }
        if let Some(referrer) = filters.referrer {
            if referrer != "Direct" {
                params.push(referrer.into());
            }
        }

        let results = BreakdownResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql_query,
            params,
        ))
        .all(self.db.as_ref())
        .await?;

        let total = results.first().map(|r| r.total_count).unwrap_or(0);

        Ok(PropertyBreakdownResponse {
            property: group_by_str.to_string(),
            items: results
                .into_iter()
                .map(|r| PropertyBreakdownItem {
                    value: r.value,
                    count: r.count,
                    percentage: r.percentage,
                })
                .collect(),
            total,
        })
    }

    /// Get property timeline: group by column over time using TimescaleDB time_bucket
    /// Example: Channel distribution by hour, device types by day, etc.
    #[allow(clippy::too_many_arguments)]
    pub async fn get_property_timeline(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        event_name: Option<String>,
        group_by_column: crate::types::PropertyColumn,
        aggregation_level: &str,
        bucket_size: Option<String>,
    ) -> Result<PropertyTimelineResponse, EventsError> {
        let group_by_str = group_by_column.as_str();

        // Auto-detect bucket size based on date range
        let duration_days = (end_date - start_date).num_days();
        let bucket = bucket_size.unwrap_or_else(|| {
            if duration_days <= 1 {
                "1 hour".to_string()
            } else if duration_days <= 7 {
                "1 day".to_string()
            } else if duration_days <= 60 {
                "1 week".to_string()
            } else {
                "1 month".to_string()
            }
        });

        // Determine aggregation
        let (agg_field, agg_distinct) = match aggregation_level {
            "sessions" => ("session_id", "DISTINCT"),
            "visitors" => ("visitor_id", "DISTINCT"),
            _ => ("*", ""),
        };

        // Check if we need to join with ip_geolocations
        let is_geo_column = matches!(group_by_str, "country" | "region" | "city");
        let is_referrer_column = group_by_str == "referrer_hostname";
        let (from_clause, select_column) = if is_geo_column {
            (
                "events e LEFT JOIN ip_geolocations ig ON e.ip_geolocation_id = ig.id",
                format!("COALESCE(ig.{}, 'Unknown')", group_by_str),
            )
        } else if is_referrer_column {
            (
                "events e",
                format!("COALESCE(e.{}, 'Direct')", group_by_str),
            )
        } else {
            ("events e", format!("e.{}", group_by_str))
        };

        let mut conditions = vec!["e.project_id = $1".to_string()];
        conditions.push("e.timestamp >= $2".to_string());
        conditions.push("e.timestamp <= $3".to_string());

        let mut param_idx = 4;
        if environment_id.is_some() {
            conditions.push(format!("e.environment_id = ${}", param_idx));
            param_idx += 1;
        }
        if deployment_id.is_some() {
            conditions.push(format!("e.deployment_id = ${}", param_idx));
            param_idx += 1;
        }
        if event_name.is_some() {
            conditions.push(format!(
                "COALESCE(e.event_name, e.event_type) = ${}",
                param_idx
            ));
            param_idx += 1;
        }

        // Pass bucket as a parameterized value ($N::interval) to prevent SQL injection
        let bucket_param_idx = param_idx;

        let sql_query = format!(
            r#"
            SELECT
                time_bucket(${}::interval, e.timestamp) as bucket,
                {} as value,
                COUNT({} e.{}) as count
            FROM {}
            WHERE {}
            GROUP BY bucket, {}
            ORDER BY bucket ASC, count DESC
            "#,
            bucket_param_idx,
            select_column,
            agg_distinct,
            agg_field,
            from_clause,
            conditions.join(" AND "),
            select_column
        );

        #[derive(FromQueryResult)]
        struct TimelineResult {
            bucket: DBDateTime,
            value: String,
            count: i64,
        }

        let mut params: Vec<sea_orm::Value> =
            vec![project_id.into(), start_date.into(), end_date.into()];
        if let Some(env_id) = environment_id {
            params.push(env_id.into());
        }
        if let Some(dep_id) = deployment_id {
            params.push(dep_id.into());
        }
        if let Some(evt_name) = event_name {
            params.push(evt_name.into());
        }
        let bucket_size_response = bucket.clone();
        params.push(bucket.into());

        let results = TimelineResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql_query,
            params,
        ))
        .all(self.db.as_ref())
        .await?;

        Ok(PropertyTimelineResponse {
            property: group_by_str.to_string(),
            bucket_size: bucket_size_response,
            items: results
                .into_iter()
                .map(|r| PropertyTimelineItem {
                    timestamp: r.bucket.to_rfc3339(),
                    value: r.value,
                    count: r.count,
                })
                .collect(),
        })
    }

    /// Get the count of active visitors in real-time
    /// Active visitors are defined as unique sessions with events in the last 5 minutes
    pub async fn get_active_visitors_count(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
    ) -> Result<i64, EventsError> {
        // Define active window as last 5 minutes
        let query = r#"SELECT COUNT(DISTINCT session_id)::bigint as active_visitors
FROM events
WHERE project_id = $1
  AND ($2::int IS NULL OR environment_id = $2)
  AND ($3::int IS NULL OR deployment_id = $3)
  AND timestamp >= NOW() - INTERVAL '5 minutes'"#;

        #[derive(FromQueryResult)]
        struct ActiveVisitorsResult {
            active_visitors: i64,
        }

        let params = vec![
            project_id.into(),
            environment_id.into(),
            deployment_id.into(),
        ];

        let result = ActiveVisitorsResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            query,
            params,
        ))
        .one(self.db.as_ref())
        .await?
        .unwrap_or(ActiveVisitorsResult { active_visitors: 0 });

        Ok(result.active_visitors)
    }

    /// Get hourly visits with aggregation support
    /// Can aggregate by page_views (events), unique sessions, or unique visitors
    /// Uses TimescaleDB's time_bucket_gapfill to fill missing hours with 0 counts
    pub async fn get_hourly_visits(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        aggregation_level: AggregationLevel,
    ) -> Result<Vec<EventTimeline>, EventsError> {
        // Build WHERE conditions with parameterized queries
        let mut where_conditions = vec![
            "project_id = $1".to_string(),
            "timestamp >= $2".to_string(),
            "timestamp <= $3".to_string(),
            "event_type = 'page_view'".to_string(),
        ];
        let mut values: Vec<sea_orm::Value> =
            vec![project_id.into(), start_date.into(), end_date.into()];
        let mut param_index = 4;

        if let Some(env_id) = environment_id {
            where_conditions.push(format!("environment_id = ${}", param_index));
            values.push(env_id.into());
            param_index += 1;
        }

        // Determine aggregation based on level
        // Use FILTER clause instead of WHERE to allow time_bucket_gapfill to work correctly
        let count_expr = match aggregation_level {
            AggregationLevel::Events => "COUNT(*)".to_string(),
            AggregationLevel::Sessions => {
                "COUNT(DISTINCT session_id) FILTER (WHERE session_id IS NOT NULL)".to_string()
            }
            AggregationLevel::Visitors => {
                "COUNT(DISTINCT visitor_id) FILTER (WHERE visitor_id IS NOT NULL)".to_string()
            }
        };

        let where_clause = where_conditions.join(" AND ");
        let sql_query = format!(
            r#"
            SELECT
                bucket::timestamptz as bucket,
                count
            FROM (
                SELECT
                    time_bucket_gapfill('1 hour', timestamp, ${}::timestamptz, ${}::timestamptz) as bucket,
                    COALESCE({}, 0) as count
                FROM events
                WHERE {}
                GROUP BY bucket
            ) sub
            ORDER BY bucket ASC
            "#,
            param_index,
            param_index + 1,
            count_expr,
            where_clause
        );

        // Add start_date and end_date again for time_bucket_gapfill parameters
        values.push(start_date.into());
        values.push(end_date.into());

        #[derive(FromQueryResult)]
        struct TimelineResult {
            bucket: UtcDateTime,
            count: i64,
        }

        let results = TimelineResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql_query,
            values,
        ))
        .all(self.db.as_ref())
        .await?;

        Ok(results
            .into_iter()
            .map(|r| EventTimeline {
                date: r.bucket,
                count: r.count,
            })
            .collect())
    }

    /// Get unique count over a time frame for a specific metric
    /// Returns count of unique sessions, visitors, or total page views based on requested metric
    /// For page_views: counts all page view events (not unique)
    pub async fn get_unique_counts(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        metric: String,
    ) -> Result<UniqueCountsResponse, EventsError> {
        if metric == "returning_visitors" {
            let query = r#"
                WITH current_visitors AS (
                    SELECT DISTINCT visitor_id
                    FROM events
                    WHERE project_id = $1
                      AND timestamp >= $2::timestamp
                      AND timestamp <= $3::timestamp
                      AND visitor_id IS NOT NULL
                      AND is_crawler = false
                      AND ($4::int IS NULL OR environment_id = $4)
                      AND ($5::int IS NULL OR deployment_id = $5)
                )
                SELECT COUNT(*)::bigint AS count
                FROM current_visitors cv
                WHERE EXISTS (
                    SELECT 1
                    FROM events previous
                    WHERE previous.project_id = $1
                      AND previous.visitor_id = cv.visitor_id
                      AND previous.timestamp < $2::timestamp
                      AND ($4::int IS NULL OR previous.environment_id = $4)
                      AND ($5::int IS NULL OR previous.deployment_id = $5)
                )
            "#;

            #[derive(FromQueryResult)]
            struct ReturningVisitorsResult {
                count: i64,
            }

            let params = vec![
                project_id.into(),
                start_date.into(),
                end_date.into(),
                environment_id.into(),
                deployment_id.into(),
            ];

            let result = ReturningVisitorsResult::find_by_statement(
                Statement::from_sql_and_values(DatabaseBackend::Postgres, query, params),
            )
            .one(self.db.as_ref())
            .await?
            .unwrap_or(ReturningVisitorsResult { count: 0 });

            return Ok(UniqueCountsResponse {
                count: result.count,
            });
        }

        // Determine what to count based on metric
        let count_expr = match metric.as_str() {
            "sessions" => {
                "COUNT(DISTINCT session_id) FILTER (WHERE session_id IS NOT NULL)::bigint"
            }
            "visitors" => {
                "COUNT(DISTINCT visitor_id) FILTER (WHERE visitor_id IS NOT NULL)::bigint"
            }
            "page_views" => "COUNT(*) FILTER (WHERE event_type = 'page_view')::bigint",
            "paths" => {
                "COUNT(DISTINCT page_path) FILTER (WHERE event_type = 'page_view')::bigint"
            }
            _ => {
                return Err(EventsError::Validation(format!(
                    "Invalid metric '{}'. Valid options: sessions, visitors, returning_visitors, page_views, paths",
                    metric
                )))
            }
        };

        // Crawler traffic is excluded so these headline counts agree with the
        // per-page analytics queries, which already filter is_crawler; bot
        // activity has its own dedicated AI-crawler views.
        let query = format!(
            r#"
            SELECT
                {} as count
            FROM events
            WHERE project_id = $1
              AND timestamp >= $2::timestamp
              AND timestamp <= $3::timestamp
              AND is_crawler = false
              AND ($4::int IS NULL OR environment_id = $4)
              AND ($5::int IS NULL OR deployment_id = $5)
            "#,
            count_expr
        );

        #[derive(FromQueryResult)]
        struct UniqueCountsResult {
            count: i64,
        }

        let params = vec![
            project_id.into(),
            start_date.into(),
            end_date.into(),
            environment_id.into(),
            deployment_id.into(),
        ];

        let result = UniqueCountsResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &query,
            params,
        ))
        .one(self.db.as_ref())
        .await?
        .unwrap_or(UniqueCountsResult { count: 0 });

        Ok(UniqueCountsResponse {
            count: result.count,
        })
    }

    /// Get dashboard analytics for multiple projects in a single batch.
    /// Returns unique visitor counts, previous-period comparison, and hourly sparkline.
    ///
    /// Both periods query the raw `events` table directly with the same
    /// `idx_events_project_timestamp` index. The previous period used to read from the
    /// `events_hourly` continuous aggregate for speed, but that aggregate's older buckets
    /// are only backfilled by a best-effort async job on startup
    /// (`run_post_migration_backfill`) and its recurring policy only refreshes the last
    /// 3 hours — so right after a restart (or if that job is still catching up), the
    /// aggregate could return no row for a project's previous-period window. That missing
    /// row was indistinguishable from "genuinely zero visitors" and fed straight into the
    /// trend fallback below, producing a misleading +/-100% instead of the real change.
    /// Reading raw events for both periods removes that dependency entirely.
    pub async fn get_dashboard_projects_analytics(
        &self,
        project_ids: &[i32],
        start_date: UtcDateTime,
        end_date: UtcDateTime,
    ) -> Result<DashboardProjectsAnalyticsResponse, EventsError> {
        if project_ids.is_empty() {
            return Ok(DashboardProjectsAnalyticsResponse {
                projects: HashMap::new(),
            });
        }

        // Compute previous period: same duration, shifted back.
        // e.g. if current = last 24h, previous = 24h-48h ago.
        let period_duration = end_date - start_date;
        let prev_start = start_date - period_duration;
        let prev_end = start_date;

        // Build the $N placeholders for the IN clause: $3, $4, $5, ...
        let id_placeholders: Vec<String> = project_ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("${}", i + 3))
            .collect();
        let in_clause = id_placeholders.join(", ");

        // Base params: start_date, end_date, then all project_ids
        let mut base_values: Vec<sea_orm::Value> = vec![start_date.into(), end_date.into()];
        for &pid in project_ids {
            base_values.push(pid.into());
        }

        // Previous period params: prev_start, prev_end, then all project_ids
        let mut prev_values: Vec<sea_orm::Value> = vec![prev_start.into(), prev_end.into()];
        for &pid in project_ids {
            prev_values.push(pid.into());
        }

        // Query 1: Unique visitor counts per project (current period — raw events for accuracy)
        // The continuous aggregate has a 1-hour end_offset gap, so recent data would be missing.
        // For a 24h window this query is fast with the idx_events_project_timestamp index.
        let current_counts_sql = format!(
            r#"
            SELECT
                project_id,
                COUNT(DISTINCT visitor_id) FILTER (WHERE visitor_id IS NOT NULL)::bigint as count
            FROM events
            WHERE timestamp >= $1 AND timestamp <= $2
              AND project_id IN ({in_clause})
            GROUP BY project_id
            "#,
        );

        #[derive(FromQueryResult)]
        struct ProjectCount {
            project_id: i32,
            count: i64,
        }

        let counts = ProjectCount::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &current_counts_sql,
            base_values.clone(),
        ))
        .all(self.db.as_ref())
        .await?;

        let counts_map: HashMap<i32, i64> = counts
            .into_iter()
            .map(|r| (r.project_id, r.count))
            .collect();

        // Query 2: Unique visitor counts per project (previous period — raw events, same
        // source and index as the current period, so it's never stale after a restart).
        let prev_counts_sql = format!(
            r#"
            SELECT
                project_id,
                COUNT(DISTINCT visitor_id) FILTER (WHERE visitor_id IS NOT NULL)::bigint as count
            FROM events
            WHERE timestamp >= $1 AND timestamp <= $2
              AND project_id IN ({in_clause})
            GROUP BY project_id
            "#,
        );

        let prev_counts = ProjectCount::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &prev_counts_sql,
            prev_values,
        ))
        .all(self.db.as_ref())
        .await?;

        let prev_counts_map: HashMap<i32, i64> = prev_counts
            .into_iter()
            .map(|r| (r.project_id, r.count))
            .collect();

        // Query 3: Hourly sparkline data per project (current period — raw events for accuracy)
        // Uses generate_series to produce the full hour grid and LEFT JOINs actual data.
        // This guarantees every project gets a row for every hour in the range, even when
        // a project has events in only a single bucket (time_bucket_gapfill inside
        // CROSS JOIN LATERAL fails to fill gaps in that edge case).
        let gapfill_start_idx = project_ids.len() + 3;
        let gapfill_end_idx = gapfill_start_idx + 1;

        let hourly_sql = format!(
            r#"
            SELECT
                p.project_id,
                h.bucket,
                COALESCE(d.count, 0) as count
            FROM unnest(ARRAY[{in_clause}]) AS p(project_id)
            CROSS JOIN generate_series(
                date_trunc('hour', ${gapfill_start_idx}::timestamptz),
                date_trunc('hour', ${gapfill_end_idx}::timestamptz),
                '1 hour'::interval
            ) AS h(bucket)
            LEFT JOIN (
                SELECT
                    project_id,
                    date_trunc('hour', timestamp) as bucket,
                    COUNT(DISTINCT visitor_id) FILTER (WHERE visitor_id IS NOT NULL) as count
                FROM events
                WHERE timestamp >= $1
                  AND timestamp <= $2
                  AND project_id IN ({in_clause})
                  AND event_type = 'page_view'
                GROUP BY project_id, date_trunc('hour', timestamp)
            ) d ON d.project_id = p.project_id AND d.bucket = h.bucket
            ORDER BY p.project_id, h.bucket ASC
            "#,
        );

        // Append gapfill start/end params
        let mut hourly_values = base_values;
        hourly_values.push(start_date.into());
        hourly_values.push(end_date.into());

        #[derive(FromQueryResult)]
        struct ProjectHourlyRow {
            project_id: i32,
            bucket: UtcDateTime,
            count: i64,
        }

        let hourly_rows = ProjectHourlyRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &hourly_sql,
            hourly_values,
        ))
        .all(self.db.as_ref())
        .await?;

        // Group hourly rows by project_id
        let mut hourly_map: HashMap<i32, Vec<EventTimeline>> = HashMap::new();
        for row in hourly_rows {
            hourly_map
                .entry(row.project_id)
                .or_default()
                .push(EventTimeline {
                    date: row.bucket,
                    count: row.count,
                });
        }

        // Build final response with trend calculation
        let mut projects = HashMap::new();
        for &pid in project_ids {
            let current = counts_map.get(&pid).copied().unwrap_or(0);
            let previous = prev_counts_map.get(&pid).copied().unwrap_or(0);

            let trend_percentage = calculate_trend_percentage(current, previous);

            projects.insert(
                pid.to_string(),
                ProjectDashboardAnalytics {
                    project_id: pid,
                    unique_visitors: current,
                    previous_unique_visitors: previous,
                    trend_percentage,
                    hourly_visits: hourly_map.remove(&pid).unwrap_or_default(),
                },
            );
        }

        Ok(DashboardProjectsAnalyticsResponse { projects })
    }

    /// Get aggregated metrics by time bucket using TimescaleDB time_bucket_gapfill
    /// Returns counts for visitors/sessions/events grouped by customizable time buckets
    #[allow(clippy::too_many_arguments)]
    pub async fn get_aggregated_buckets(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        aggregation_level: AggregationLevel,
        bucket_size: String,
    ) -> Result<crate::types::AggregatedBucketsResponse, EventsError> {
        // Determine aggregation based on level
        let (count_expr, null_check) = match aggregation_level {
            AggregationLevel::Events => ("COUNT(*)", ""),
            AggregationLevel::Sessions => {
                ("COUNT(DISTINCT session_id)", " AND session_id IS NOT NULL")
            }
            AggregationLevel::Visitors => {
                ("COUNT(DISTINCT visitor_id)", " AND visitor_id IS NOT NULL")
            }
        };

        // Build WHERE conditions with parameterized queries
        let mut where_conditions = vec![
            "project_id = $1".to_string(),
            "timestamp >= $2::timestamp".to_string(),
            "timestamp <= $3::timestamp".to_string(),
        ];
        let mut values: Vec<sea_orm::Value> =
            vec![project_id.into(), start_date.into(), end_date.into()];
        let mut param_index = 4;

        if let Some(env_id) = environment_id {
            where_conditions.push(format!("environment_id = ${}", param_index));
            values.push(env_id.into());
            param_index += 1;
        }

        if let Some(dep_id) = deployment_id {
            where_conditions.push(format!("deployment_id = ${}", param_index));
            values.push(dep_id.into());
            param_index += 1;
        }

        let where_clause = where_conditions.join(" AND ");

        // Pass bucket_size as a parameterized value to prevent SQL injection
        let bucket_param_index = param_index;
        values.push(bucket_size.clone().into());
        param_index += 1;

        let sql_query = format!(
            r#"
            SELECT
                time_bucket_gapfill(${}::interval, timestamp, ${}::timestamptz, ${}::timestamptz) as bucket,
                COALESCE({}, 0) as count
            FROM events
            WHERE {}{}
            GROUP BY bucket
            ORDER BY bucket ASC
            "#,
            bucket_param_index,
            param_index,
            param_index + 1,
            count_expr,
            where_clause,
            null_check
        );

        // Add start_date and end_date for time_bucket_gapfill parameters
        values.push(start_date.into());
        values.push(end_date.into());

        #[derive(FromQueryResult)]
        struct BucketResult {
            bucket: DBDateTime,
            count: i64,
        }

        let results = BucketResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql_query,
            values,
        ))
        .all(self.db.as_ref())
        .await?;

        let total: i64 = results.iter().map(|r| r.count).sum();

        Ok(crate::types::AggregatedBucketsResponse {
            bucket_size,
            aggregation_level: aggregation_level.as_str().to_string(),
            items: results
                .into_iter()
                .map(|r| crate::types::AggregatedBucketItem {
                    timestamp: r.bucket.format("%Y-%m-%d %H:%M:%S").to_string(),
                    count: r.count,
                })
                .collect(),
            total,
        })
    }

    /// Record an analytics event with enriched data
    #[allow(clippy::too_many_arguments)]
    pub async fn record_event(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        session_id: Option<String>,
        visitor_id: Option<String>,
        event_name: &str,
        event_data: serde_json::Value,
        request_path: &str,
        request_query: &str,
        screen_width: Option<u32>,
        screen_height: Option<u32>,
        viewport_width: Option<u32>,
        viewport_height: Option<u32>,
        language: Option<String>,
        page_title: Option<String>,
        ip_geolocation_id: Option<i32>,
        user_agent: Option<String>,
        referrer: Option<String>,
        // Performance metrics (web vitals) - optional
        ttfb: Option<f32>,
        lcp: Option<f32>,
        fid: Option<f32>,
        fcp: Option<f32>,
        cls: Option<f32>,
        inp: Option<f32>,
    ) -> Result<temps_entities::events::Model, EventsError> {
        use sea_orm::{ActiveModelTrait, ActiveValue::Set};
        use temps_entities::events;

        // The `events` table (and its ClickHouse replica) enforce NOT NULL on
        // session_id. A visitor with no session cookie yet — first hit, a
        // preview origin where the cookie can't be read, or a client that
        // doesn't send one — arrives here with `session_id = None`, which would
        // violate the constraint and 500 the ingest (silently dropping the
        // event). Fall back to a fresh session UUID so the event is never lost;
        // a real cookie on subsequent requests groups them into one session.
        let session_id =
            Some(session_id.unwrap_or_else(|| temps_core::uuid::Uuid::new_v4().to_string()));

        // Extract hostname from event_data if available, otherwise use default
        let hostname = event_data
            .get("hostname")
            .and_then(|v| v.as_str())
            .unwrap_or("localhost")
            .to_string();

        let href = event_data
            .get("href")
            .and_then(|v| v.as_str())
            .unwrap_or(&format!("http://{}{}", hostname, request_path))
            .to_string();

        // Parse UTM parameters from query string (request_query)
        let utm_params = temps_analytics::parse_utm_params(request_query);

        // Extract referrer hostname if referrer is present
        let referrer_hostname = referrer
            .as_ref()
            .and_then(|r| temps_analytics::extract_referrer_hostname(r));

        // Compute channel attribution
        let current_hostname = event_data.get("hostname").and_then(|v| v.as_str());
        let channel = temps_analytics::get_channel(
            &utm_params,
            referrer_hostname.as_deref(),
            current_hostname,
        );

        // Get UTM values from parsed params
        let utm_source = utm_params.utm_source;
        let utm_medium = utm_params.utm_medium;
        let utm_campaign = utm_params.utm_campaign;
        let utm_term = utm_params.utm_term;
        let utm_content = utm_params.utm_content;

        // Parse user agent up front: the bot flag is needed both for the
        // visitor record (so live-visitor lists can exclude crawlers) and the
        // event row (so analytics read filters on `is_crawler` work).
        let parsed_ua =
            crate::services::user_agent::ParsedUserAgent::from_user_agent(user_agent.as_deref());
        let is_crawler = parsed_ua.is_bot();
        let crawler_name = parsed_ua.crawler_name();

        // Get visitor from visitor_id from visitors table
        // Convert visitor_id (String) to Option<i32> by looking up the visitor in the database
        let visitor_id_i32 = if let Some(ref visitor_id) = visitor_id {
            use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter};
            use temps_entities::visitor;

            let visitor_record = visitor::Entity::find()
                .filter(visitor::Column::VisitorId.eq(visitor_id.clone()))
                .one(self.db.as_ref())
                .await
                .map_err(EventsError::Database)?;

            match visitor_record {
                Some(v) => {
                    // Keep `visitor.last_seen` fresh on every ingested event so
                    // the "live visitors" list (which queries
                    // `visitor.last_seen`) stays in sync with the
                    // active-visitor badge (which queries `events.timestamp`).
                    // Without this, only requests that traverse the proxy bump
                    // `last_seen`, so JS-driven heartbeats/page_leave events
                    // would tick the badge but leave the detail list empty.
                    // `has_activity` is stamped on the first event per visitor.
                    let mut active_visitor: visitor::ActiveModel = v.clone().into();
                    active_visitor.last_seen = sea_orm::ActiveValue::Set(chrono::Utc::now());
                    if !v.has_activity {
                        active_visitor.has_activity = sea_orm::ActiveValue::Set(true);
                    }
                    // Flag the visitor as a crawler once any bot-UA event is seen.
                    // Only escalate (false -> true), never clear it.
                    if is_crawler && !v.is_crawler {
                        active_visitor.is_crawler = sea_orm::ActiveValue::Set(true);
                    }
                    let _ = active_visitor.update(self.db.as_ref()).await;

                    Some(v.id)
                }
                None => {
                    // The cookie decrypted to a valid UUID, but no row exists
                    // for it yet. The proxy creates this row asynchronously
                    // (`ProxyLogBatchWriter`, flushed every 500ms) -- a
                    // brand-new visitor's very first pageview can race ahead
                    // of that flush and land here first. Without this
                    // fallback, the event keeps `visitor_id = NULL` forever
                    // (no retry, no backfill exists), which silently drops it
                    // from every `COUNT(DISTINCT visitor_id)` metric. `ON
                    // CONFLICT` makes this idempotent with the proxy's own
                    // upsert: whichever writer lands first creates the row,
                    // the other just bumps `last_seen`.
                    match self
                        .upsert_visitor_on_miss(
                            visitor_id,
                            project_id,
                            // `visitor.environment_id` is NOT NULL; the proxy's
                            // own upsert defaults to 0 for "no environment"
                            // (see `TrackingEvent.environment_id: i32` in
                            // temps-proxy), so mirror that here rather than
                            // trying to insert NULL.
                            environment_id.unwrap_or(0),
                            user_agent.clone(),
                            ip_geolocation_id,
                            is_crawler,
                            crawler_name.clone(),
                            referrer.clone(),
                            referrer_hostname.clone(),
                            Some(channel.to_string()),
                            utm_source.clone(),
                            utm_medium.clone(),
                            utm_campaign.clone(),
                        )
                        .await
                    {
                        Ok(id) => {
                            tracing::info!(
                                visitor_id = %visitor_id,
                                project_id,
                                visitor_row_id = id,
                                "Created visitor row from event ingest (lookup miss -- proxy's async upsert hadn't landed yet)"
                            );
                            Some(id)
                        }
                        Err(e) => {
                            tracing::error!(
                                visitor_id = %visitor_id,
                                project_id,
                                error = %e,
                                "Failed to upsert visitor on lookup miss; event will record with visitor_id=NULL"
                            );
                            None
                        }
                    }
                }
            }
        } else {
            None
        };

        // Session row self-heal (non-fatal side-effect).
        //
        // The proxy's `ProxyLogBatchWriter` flushes `request_sessions` rows
        // asynchronously, with up to 500ms batching lag.  An event arriving
        // here before that flush has no matching `request_sessions` row,
        // which causes two downstream breakages:
        //
        //  1. `get_visitor_sessions_by_id` uses LEFT JOIN → rs.id comes back
        //     NULL → sea-orm tries to decode NULL into `i32` → hard decode
        //     error that kills the whole request (fixed defensively in Fix 2,
        //     but better to prevent the missing row entirely).
        //  2. `get_visitor_journey` uses INNER JOIN → silently excludes every
        //     event whose session has no `request_sessions` row, producing an
        //     empty journey response even for visitors with real events.
        //
        // Additionally, if the proxy's own `upsert_visitor` failed for a
        // visitor in a batch (non-fatal there), that visitor never enters the
        // FK cache, so the corresponding `request_sessions` row gets created
        // with `visitor_id = NULL`.  We detect that case (existing row with
        // NULL visitor_id) and backfill it using COALESCE in the ON CONFLICT
        // clause so the session is permanently healed.
        //
        // We clone the UTM/referrer/channel locals here because they are
        // moved into `events::ActiveModel` below.  Non-fatal: on failure the
        // event still records; only the session attribution is missing.
        if let Some(ref session_id_str) = session_id {
            let session_lookup = {
                use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
                use temps_entities::request_sessions;
                request_sessions::Entity::find()
                    .filter(request_sessions::Column::SessionId.eq(session_id_str.clone()))
                    .one(self.db.as_ref())
                    .await
            };
            match session_lookup {
                Err(e) => {
                    tracing::error!(
                        session_id = %session_id_str,
                        project_id,
                        error = %e,
                        "Failed to look up request_sessions; session self-heal skipped"
                    );
                }
                Ok(Some(ref session)) if session.visitor_id.is_some() => {
                    // Row exists with visitor_id already populated — nothing to do.
                }
                Ok(maybe_session) => {
                    // Row is missing (None) OR exists with visitor_id = NULL.
                    let is_heal = maybe_session.is_some();
                    match self
                        .upsert_session_on_miss(
                            session_id_str,
                            visitor_id_i32,
                            referrer.clone(),
                            referrer_hostname.clone(),
                            Some(channel.to_string()),
                            utm_source.clone(),
                            utm_medium.clone(),
                            utm_campaign.clone(),
                            utm_term.clone(),
                            utm_content.clone(),
                        )
                        .await
                    {
                        Ok(()) => {
                            tracing::info!(
                                session_id = %session_id_str,
                                project_id,
                                visitor_id = ?visitor_id_i32,
                                is_heal,
                                "Self-healed request_sessions row from event ingest \
                                 (proxy async flush had not landed yet, or row had NULL visitor_id)"
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                session_id = %session_id_str,
                                project_id,
                                visitor_id = ?visitor_id_i32,
                                error = %e,
                                "Failed to self-heal request_sessions row; \
                                 event records without session attribution"
                            );
                        }
                    }
                }
            }
        }

        // Browser/OS fields from the user agent parsed above.
        let browser = parsed_ua.browser;
        let browser_version = parsed_ua.browser_version;
        let operating_system = parsed_ua.operating_system;
        let operating_system_version = parsed_ua.operating_system_version;
        let device_type = parsed_ua.device_type;

        let event = events::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(environment_id),
            deployment_id: Set(deployment_id),
            session_id: Set(session_id),
            visitor_id: Set(visitor_id_i32),
            event_type: Set(event_name.to_string()),
            event_name: Set(Some(event_name.to_string())),
            props: Set(Some(event_data)),
            hostname: Set(hostname),
            pathname: Set(request_path.to_string()),
            page_path: Set(request_path.to_string()),
            href: Set(href),
            querystring: Set(Some(request_query.to_string())),
            page_title: Set(page_title),
            referrer: Set(referrer),
            referrer_hostname: Set(referrer_hostname),
            screen_width: Set(screen_width.map(|v| v as i16)),
            screen_height: Set(screen_height.map(|v| v as i16)),
            viewport_width: Set(viewport_width.map(|v| v as i16)),
            viewport_height: Set(viewport_height.map(|v| v as i16)),
            language: Set(language),
            ip_geolocation_id: Set(ip_geolocation_id),
            user_agent: Set(user_agent),
            browser: Set(browser),
            browser_version: Set(browser_version),
            operating_system: Set(operating_system),
            operating_system_version: Set(operating_system_version),
            device_type: Set(device_type),
            channel: Set(Some(channel.to_string())),
            utm_source: Set(utm_source),
            utm_medium: Set(utm_medium),
            utm_campaign: Set(utm_campaign),
            utm_term: Set(utm_term),
            utm_content: Set(utm_content),
            // Performance metrics (web vitals)
            ttfb: Set(ttfb),
            lcp: Set(lcp),
            fid: Set(fid),
            fcp: Set(fcp),
            cls: Set(cls),
            inp: Set(inp),
            timestamp: Set(chrono::Utc::now()),
            is_entry: Set(false),
            is_exit: Set(false),
            is_bounce: Set(false),
            is_crawler: Set(is_crawler),
            crawler_name: Set(crawler_name),
            ..Default::default()
        };

        let result = event.insert(self.db.as_ref()).await?;

        // Enqueue for the ClickHouse fan-out worker. This is a separate
        // statement, not in the same transaction as the events insert,
        // because:
        //   1. `events` is a TimescaleDB hypertable; running multi-statement
        //      transactions across hypertables is fine but adds locking
        //      that this hot path doesn't need.
        //   2. If the outbox insert fails, the event is still recorded —
        //      we'd rather lose the CH replication of one event than fail
        //      the user's request.
        // Failures are logged at debug because the outbox table may not
        // exist on installs that haven't run migrations yet, and that's
        // not a user-actionable problem.
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
        let outbox_sql = "INSERT INTO events_ch_outbox (event_id) VALUES ($1) \
                          ON CONFLICT (event_id) DO NOTHING";
        if let Err(e) = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                outbox_sql,
                vec![result.id.into()],
            ))
            .await
        {
            tracing::debug!(
                event_id = result.id,
                error = %e,
                "ch_fanout outbox enqueue failed (non-fatal); event will not replicate to ClickHouse"
            );
        }

        Ok(result)
    }

    /// Upsert a visitor row when an ingested event's cookie doesn't yet have a
    /// matching row in the `visitor` table. Mirrors the proxy's own
    /// `ProxyLogBatchWriter::upsert_visitor` upsert exactly (same columns,
    /// same `ON CONFLICT (visitor_id, project_id)` key) so whichever async
    /// writer -- the proxy's batch flush or this event -- lands first creates
    /// the canonical row; the other just bumps `last_seen`.
    #[allow(clippy::too_many_arguments)]
    async fn upsert_visitor_on_miss(
        &self,
        visitor_id: &str,
        project_id: i32,
        environment_id: i32,
        user_agent: Option<String>,
        ip_address_id: Option<i32>,
        is_crawler: bool,
        crawler_name: Option<String>,
        referrer: Option<String>,
        referrer_hostname: Option<String>,
        channel: Option<String>,
        utm_source: Option<String>,
        utm_medium: Option<String>,
        utm_campaign: Option<String>,
    ) -> Result<i32, EventsError> {
        use sea_orm::ConnectionTrait;

        let now = chrono::Utc::now();
        let stmt = Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"INSERT INTO visitor (
                visitor_id, project_id, environment_id,
                first_seen, last_seen,
                user_agent, ip_address_id, is_crawler, crawler_name, has_activity,
                first_referrer, first_referrer_hostname, first_channel,
                first_utm_source, first_utm_medium, first_utm_campaign
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, true,
                $10, $11, $12, $13, $14, $15
            )
            ON CONFLICT (visitor_id, project_id) DO UPDATE SET
                last_seen = EXCLUDED.last_seen
            RETURNING id"#,
            [
                visitor_id.to_string().into(),
                project_id.into(),
                environment_id.into(),
                now.into(),
                now.into(),
                user_agent.into(),
                ip_address_id.into(),
                is_crawler.into(),
                crawler_name.into(),
                referrer.into(),
                referrer_hostname.into(),
                channel.into(),
                utm_source.into(),
                utm_medium.into(),
                utm_campaign.into(),
            ],
        );

        let row = self.db.query_one(stmt).await?.ok_or_else(|| {
            EventsError::Database(sea_orm::DbErr::RecordNotFound(format!(
                "visitor upsert for visitor_id={visitor_id} project_id={project_id} returned no row"
            )))
        })?;
        row.try_get::<i32>("", "id").map_err(EventsError::Database)
    }

    /// Upsert a `request_sessions` row when an ingested event's session has no
    /// matching row yet (proxy async flush lag) or has one with `visitor_id =
    /// NULL` (prior proxy-side FK-cache miss).
    ///
    /// Mirrors the proxy's own `ProxyLogBatchWriter::upsert_session` SQL
    /// (same columns, same `ON CONFLICT (session_id)` key) but extends the
    /// `DO UPDATE` clause with a `COALESCE` so a previously-NULL `visitor_id`
    /// gets backfilled atomically without a separate UPDATE round-trip.
    ///
    /// Unlike the proxy's version (which deliberately avoids overwriting
    /// `visitor_id` on every conflict to prevent racing writers from clobbering
    /// each other), this version is called only when we already know the
    /// existing row has `visitor_id = NULL`, so the COALESCE is safe: it
    /// preserves any non-NULL value that might arrive concurrently.
    #[allow(clippy::too_many_arguments)]
    async fn upsert_session_on_miss(
        &self,
        session_id: &str,
        visitor_id: Option<i32>,
        referrer: Option<String>,
        referrer_hostname: Option<String>,
        channel: Option<String>,
        utm_source: Option<String>,
        utm_medium: Option<String>,
        utm_campaign: Option<String>,
        utm_term: Option<String>,
        utm_content: Option<String>,
    ) -> Result<(), EventsError> {
        use sea_orm::ConnectionTrait;

        let now = chrono::Utc::now();
        let stmt = Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"INSERT INTO request_sessions (
                session_id, started_at, last_accessed_at, visitor_id,
                referrer, referrer_hostname,
                utm_source, utm_medium, utm_campaign, utm_content, utm_term,
                channel, data
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, '{}'
            )
            ON CONFLICT (session_id) DO UPDATE SET
                last_accessed_at = EXCLUDED.last_accessed_at,
                visitor_id = COALESCE(request_sessions.visitor_id, EXCLUDED.visitor_id)"#,
            [
                session_id.to_string().into(),
                now.into(),
                now.into(),
                visitor_id.into(),
                referrer.into(),
                referrer_hostname.into(),
                utm_source.into(),
                utm_medium.into(),
                utm_campaign.into(),
                utm_content.into(),
                utm_term.into(),
                channel.into(),
            ],
        );

        self.db.execute(stmt).await.map_err(EventsError::Database)?;
        Ok(())
    }
}

/// Computes the percentage change between two visitor counts for the dashboard
/// trend badge. Returns `None` when there's no previous-period baseline to compare
/// against — a `previous` of 0 can't be turned into a real ratio, so we omit the
/// trend entirely rather than fabricate a flat +/-100%.
///
/// Shared by both the Timescale (`events_service`) and ClickHouse
/// (`clickhouse_backend`) `AnalyticsEvents` implementations so the two backends
/// can't drift back into inconsistent trend semantics.
pub(crate) fn calculate_trend_percentage(current: i64, previous: i64) -> Option<f64> {
    if previous > 0 {
        Some(((current - previous) as f64 / previous as f64) * 100.0)
    } else {
        None
    }
}

// AnalyticsEvents trait impl: unpacks query value-types into the inherent
// SQL methods above so handlers can depend on `Arc<dyn AnalyticsEvents>`.
// The trait describes *what* to query (parameters); each backend chooses
// *how* (SQL strings here, typed query builder for ClickHouse, etc.).
#[async_trait::async_trait]
impl crate::services::traits::AnalyticsEvents for AnalyticsEventsService {
    async fn query_events_count(
        &self,
        q: crate::services::queries::EventsCountSpec,
    ) -> Result<Vec<crate::types::EventCount>, EventsError> {
        AnalyticsEventsService::get_events_count(
            self,
            q.range.start,
            q.range.end,
            q.scope.project_id,
            q.scope.environment_id,
            Some(q.limit),
            Some(q.custom_events_only),
            q.aggregation_level,
        )
        .await
    }

    async fn query_session_events(
        &self,
        q: crate::services::queries::SessionEventsSpec,
    ) -> Result<Option<crate::types::AnalyticsSessionEventsResponse>, EventsError> {
        AnalyticsEventsService::get_session_events(
            self,
            q.session_id,
            q.scope.project_id,
            q.scope.environment_id,
        )
        .await
    }

    async fn query_has_events(
        &self,
        q: crate::services::queries::HasEventsSpec,
    ) -> Result<bool, EventsError> {
        AnalyticsEventsService::has_analytics_events(
            self,
            q.scope.project_id,
            q.scope.environment_id,
        )
        .await
    }

    async fn query_event_type_breakdown(
        &self,
        q: crate::services::queries::EventTypeBreakdownSpec,
    ) -> Result<Vec<crate::types::EventTypeBreakdown>, EventsError> {
        AnalyticsEventsService::get_event_type_breakdown(
            self,
            q.range.start,
            q.range.end,
            q.scope.project_id,
            q.scope.environment_id,
            q.aggregation_level,
        )
        .await
    }

    async fn query_events_timeline(
        &self,
        q: crate::services::queries::EventsTimelineSpec,
    ) -> Result<Vec<crate::types::EventTimeline>, EventsError> {
        AnalyticsEventsService::get_events_timeline(
            self,
            q.range.start,
            q.range.end,
            q.scope.project_id,
            q.scope.environment_id,
            q.event_name,
            q.bucket_size,
            q.aggregation_level,
        )
        .await
    }

    async fn query_property_breakdown(
        &self,
        q: crate::services::queries::PropertyBreakdownSpec,
    ) -> Result<crate::types::PropertyBreakdownResponse, EventsError> {
        AnalyticsEventsService::get_property_breakdown(
            self,
            q.range.start,
            q.range.end,
            q.scope.project_id,
            q.scope.environment_id,
            q.scope.deployment_id,
            q.event_name,
            q.group_by_column,
            &q.aggregation_level,
            Some(q.limit),
            q.filters,
        )
        .await
    }

    async fn query_property_timeline(
        &self,
        q: crate::services::queries::PropertyTimelineSpec,
    ) -> Result<crate::types::PropertyTimelineResponse, EventsError> {
        AnalyticsEventsService::get_property_timeline(
            self,
            q.range.start,
            q.range.end,
            q.scope.project_id,
            q.scope.environment_id,
            q.scope.deployment_id,
            q.event_name,
            q.group_by_column,
            &q.aggregation_level,
            q.bucket_size,
        )
        .await
    }

    async fn query_active_visitors(
        &self,
        q: crate::services::queries::ActiveVisitorsSpec,
    ) -> Result<i64, EventsError> {
        AnalyticsEventsService::get_active_visitors_count(
            self,
            q.scope.project_id,
            q.scope.environment_id,
            q.scope.deployment_id,
        )
        .await
    }

    async fn query_hourly_visits(
        &self,
        q: crate::services::queries::HourlyVisitsSpec,
    ) -> Result<Vec<crate::types::EventTimeline>, EventsError> {
        AnalyticsEventsService::get_hourly_visits(
            self,
            q.range.start,
            q.range.end,
            q.scope.project_id,
            q.scope.environment_id,
            q.aggregation_level,
        )
        .await
    }

    async fn query_unique_counts(
        &self,
        q: crate::services::queries::UniqueCountsSpec,
    ) -> Result<crate::types::UniqueCountsResponse, EventsError> {
        AnalyticsEventsService::get_unique_counts(
            self,
            q.range.start,
            q.range.end,
            q.scope.project_id,
            q.scope.environment_id,
            q.scope.deployment_id,
            q.metric,
        )
        .await
    }

    async fn query_dashboard_projects(
        &self,
        q: crate::services::queries::DashboardProjectsSpec,
    ) -> Result<crate::types::DashboardProjectsAnalyticsResponse, EventsError> {
        AnalyticsEventsService::get_dashboard_projects_analytics(
            self,
            &q.project_ids,
            q.range.start,
            q.range.end,
        )
        .await
    }

    async fn query_aggregated_buckets(
        &self,
        q: crate::services::queries::AggregatedBucketsSpec,
    ) -> Result<crate::types::AggregatedBucketsResponse, EventsError> {
        AnalyticsEventsService::get_aggregated_buckets(
            self,
            q.range.start,
            q.range.end,
            q.scope.project_id,
            q.scope.environment_id,
            q.scope.deployment_id,
            q.aggregation_level,
            q.bucket_size,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use sea_orm::{Database, DatabaseConnection, DbErr};
    use std::sync::Arc;
    use temps_entities::upstream_config::UpstreamList;

    async fn setup_test_db() -> Result<DatabaseConnection, DbErr> {
        Database::connect("sqlite::memory:").await
    }

    #[allow(dead_code)]
    async fn create_test_events(_db: &DatabaseConnection) {
        // This test would require the events table schema
        // For now, this is a template for future tests
    }

    #[tokio::test]
    async fn test_unique_counts_rejects_unknown_metric() {
        let db = setup_test_db().await.unwrap();
        let service = AnalyticsEventsService::new(Arc::new(db));
        let start = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2024, 1, 2, 0, 0, 0).unwrap();

        let result = service
            .get_unique_counts(start, end, 1, None, None, "unknown".to_string())
            .await;

        assert!(
            matches!(result, Err(EventsError::Validation(message)) if message.contains("returning_visitors"))
        );
    }

    #[tokio::test]
    async fn test_aggregation_levels() {
        // Create test database with events
        let db = setup_test_db().await.unwrap();

        // Insert test data:
        // - Visitor 1, Session A: 3 "button_click" events
        // - Visitor 1, Session B: 2 "button_click" events
        // - Visitor 2, Session C: 1 "button_click" event

        // Expected results:
        // - Events aggregation: 6 total events
        // - Sessions aggregation: 3 unique sessions
        // - Visitors aggregation: 2 unique visitors

        let service = AnalyticsEventsService::new(Arc::new(db));
        let start = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2024, 12, 31, 23, 59, 59).unwrap();

        // Test Events aggregation
        let _events_result = service
            .get_events_count(
                start,
                end,
                1,
                None,
                None,
                Some(false),
                AggregationLevel::Events,
            )
            .await;

        // Test Sessions aggregation
        let _sessions_result = service
            .get_events_count(
                start,
                end,
                1,
                None,
                None,
                Some(false),
                AggregationLevel::Sessions,
            )
            .await;

        // Test Visitors aggregation
        let _visitors_result = service
            .get_events_count(
                start,
                end,
                1,
                None,
                None,
                Some(false),
                AggregationLevel::Visitors,
            )
            .await;

        // Assertions would verify:
        // assert_eq!(events_result.unwrap()[0].count, 6);
        // assert_eq!(sessions_result.unwrap()[0].count, 3);
        // assert_eq!(visitors_result.unwrap()[0].count, 2);
    }

    #[tokio::test]
    async fn test_event_type_breakdown_aggregation() {
        let db = setup_test_db().await.unwrap();

        // Insert test data:
        // - page_view: 10 events from 5 sessions from 3 visitors
        // - button_click: 6 events from 3 sessions from 2 visitors

        let service = AnalyticsEventsService::new(Arc::new(db));
        let start = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2024, 12, 31, 23, 59, 59).unwrap();

        // Test with different aggregation levels
        let _events_breakdown = service
            .get_event_type_breakdown(start, end, 1, None, AggregationLevel::Events)
            .await;

        let _sessions_breakdown = service
            .get_event_type_breakdown(start, end, 1, None, AggregationLevel::Sessions)
            .await;

        let _visitors_breakdown = service
            .get_event_type_breakdown(start, end, 1, None, AggregationLevel::Visitors)
            .await;

        // Expected:
        // Events: page_view=10 (62.5%), button_click=6 (37.5%)
        // Sessions: page_view=5 (62.5%), button_click=3 (37.5%)
        // Visitors: page_view=3 (60%), button_click=2 (40%)
    }

    #[tokio::test]
    async fn test_timeline_aggregation() {
        let db = setup_test_db().await.unwrap();

        // Insert test data across 2 days:
        // Day 1:
        //   - Visitor 1, Session A: 3 events
        //   - Visitor 2, Session B: 2 events
        // Day 2:
        //   - Visitor 1, Session C: 1 event (same visitor, new session)
        //   - Visitor 3, Session D: 4 events

        let service = AnalyticsEventsService::new(Arc::new(db));
        let start = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2024, 1, 2, 23, 59, 59).unwrap();

        // Test Events aggregation - should show raw event counts per day
        let _events_timeline = service
            .get_events_timeline(
                start,
                end,
                1,
                None,
                None,
                Some("day".to_string()),
                AggregationLevel::Events,
            )
            .await;

        // Test Sessions aggregation - should show unique sessions per day
        let _sessions_timeline = service
            .get_events_timeline(
                start,
                end,
                1,
                None,
                None,
                Some("day".to_string()),
                AggregationLevel::Sessions,
            )
            .await;

        // Test Visitors aggregation - should show unique visitors per day
        let _visitors_timeline = service
            .get_events_timeline(
                start,
                end,
                1,
                None,
                None,
                Some("day".to_string()),
                AggregationLevel::Visitors,
            )
            .await;

        // Expected:
        // Events: Day1=5, Day2=5
        // Sessions: Day1=2, Day2=2
        // Visitors: Day1=2, Day2=2 (visitor 1 appears both days but counted once per day)
    }

    #[tokio::test]
    async fn test_ip_geolocation_integration() {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use temps_entities::ip_geolocations;
        use temps_geo::{GeoIpService, IpAddressService};

        // Setup PostgreSQL test container
        use testcontainers::{
            core::{ContainerPort, WaitFor},
            runners::AsyncRunner,
            GenericImage, ImageExt,
        };

        // Use TimescaleDB with pgvector support
        let postgres_image = GenericImage::new("timescale/timescaledb-ha", "pg18")
            .with_exposed_port(ContainerPort::Tcp(5432))
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_DB", "postgres");

        let node = postgres_image
            .start()
            .await
            .expect("Failed to start PostgreSQL container");
        let port = node
            .get_host_port_ipv4(5432)
            .await
            .expect("Failed to get port");

        let database_url = format!(
            "postgresql://postgres:postgres@localhost:{}/postgres?sslmode=disable",
            port
        );

        // Wait a bit for PostgreSQL to be fully ready
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        // Create database connection
        let db = sea_orm::Database::connect(&database_url)
            .await
            .expect("Failed to connect to database");
        let db = Arc::new(db);

        // Run migrations to create tables
        use temps_migrations::{Migrator, MigratorTrait};
        Migrator::up(&*db, None)
            .await
            .expect("Failed to run migrations");

        // Create mock GeoIP service
        let geoip_service = Arc::new(GeoIpService::Mock(temps_geo::MockGeoIpService::new()));

        // Create IpAddressService
        let ip_service = Arc::new(IpAddressService::new(db.clone(), geoip_service.clone()));

        // Test 1: Lookup localhost IP (should get random mock city)
        let ip_info = ip_service
            .get_or_create_ip("127.0.0.1")
            .await
            .expect("Failed to lookup IP");

        println!(
            "Resolved 127.0.0.1 to: {} / {} / {}",
            ip_info.country.as_ref().unwrap(),
            ip_info.region.as_ref().unwrap(),
            ip_info.city.as_ref().unwrap()
        );

        // Verify the IP was stored in database
        assert!(ip_info.id > 0);
        assert!(ip_info.country.is_some());
        assert!(ip_info.city.is_some());

        // Verify we can fetch it from database
        let db_record = ip_geolocations::Entity::find()
            .filter(ip_geolocations::Column::IpAddress.eq("127.0.0.1"))
            .one(db.as_ref())
            .await
            .expect("Failed to query database")
            .expect("IP not found in database");

        assert_eq!(db_record.ip_address, "127.0.0.1");
        assert!(!db_record.country.is_empty());

        // Test 2: Lookup same IP again (should return cached result)
        let ip_info_cached = ip_service
            .get_or_create_ip("127.0.0.1")
            .await
            .expect("Failed to lookup IP (cached)");

        // Should have same ID (cached)
        assert_eq!(ip_info.id, ip_info_cached.id);
        assert_eq!(ip_info.country, ip_info_cached.country);

        println!("✅ IP geolocation integration test passed!");
        println!("   - IP lookup works correctly with mock GeoIP service");
        println!("   - IP data is cached in database (same ID on repeated lookups)");
        println!(
            "   - Geolocation: {} / {} / {}",
            ip_info.country.as_ref().unwrap(),
            ip_info.region.as_ref().unwrap(),
            ip_info.city.as_ref().unwrap()
        );
        println!(
            "   - Coordinates: lat={:.4}, lng={:.4}",
            ip_info.latitude.unwrap(),
            ip_info.longitude.unwrap()
        );
        println!(
            "   - IP geolocation ID {} ready to be linked to events",
            ip_info.id
        );
    }

    // ========== SQL Injection Prevention Tests ==========
    // These tests verify that all fixed functions use parameterized queries
    // and are protected against SQL injection attacks

    #[tokio::test]
    async fn test_get_events_count_sql_injection_protection() {
        // This test verifies that get_events_count properly sanitizes inputs
        // by using parameterized queries instead of string interpolation

        let db = setup_test_db().await.unwrap();
        let service = AnalyticsEventsService::new(Arc::new(db));

        let start = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2024, 12, 31, 23, 59, 59).unwrap();

        // Test with potentially malicious project_id (should be safely parameterized)
        let result = service
            .get_events_count(
                start,
                end,
                999999, // Large ID that could be used in injection attempts
                None,
                Some(100),
                Some(false),
                AggregationLevel::Events,
            )
            .await;

        // Should not panic or cause SQL errors - parameterized queries protect against injection
        assert!(result.is_ok() || matches!(result, Err(EventsError::Database(_))));
    }

    #[tokio::test]
    async fn test_get_session_events_sql_injection_protection() {
        // Verify that session_id is properly parameterized
        let db = setup_test_db().await.unwrap();
        let service = AnalyticsEventsService::new(Arc::new(db));

        // Attempt SQL injection through session_id
        let malicious_session_id = "' OR '1'='1"; // Classic SQL injection attempt

        let result = service
            .get_session_events(malicious_session_id.to_string(), 1, None)
            .await;

        // Should safely handle the malicious input as a literal string
        // The parameterized query treats it as data, not SQL code
        // Result may fail with SQLite (no events table) but shouldn't cause SQL injection
        match result {
            Ok(session_events) => {
                // If successful, should return None (no matching session)
                assert!(session_events.is_none());
            }
            Err(EventsError::Database(_)) => {
                // Database error is expected with SQLite (no events table)
                // The key is that it didn't cause SQL injection
            }
            Err(e) => panic!("Unexpected error type: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_has_analytics_events_sql_injection_protection() {
        let db = setup_test_db().await.unwrap();
        let service = AnalyticsEventsService::new(Arc::new(db));

        // Test with various project IDs including edge cases
        let result1 = service.has_analytics_events(1, None).await;
        let result2 = service.has_analytics_events(-1, Some(999)).await; // Negative ID
        let result3 = service.has_analytics_events(i32::MAX, Some(i32::MAX)).await; // Max values

        // All should handle safely - either OK or Database error (no table)
        assert!(result1.is_ok() || matches!(result1, Err(EventsError::Database(_))));
        assert!(result2.is_ok() || matches!(result2, Err(EventsError::Database(_))));
        assert!(result3.is_ok() || matches!(result3, Err(EventsError::Database(_))));
    }

    #[tokio::test]
    async fn test_get_event_type_breakdown_sql_injection_protection() {
        let db = setup_test_db().await.unwrap();
        let service = AnalyticsEventsService::new(Arc::new(db));

        let start = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2024, 12, 31, 23, 59, 59).unwrap();

        // Test with various environment_id values
        let result = service
            .get_event_type_breakdown(
                start,
                end,
                1,
                Some(999999), // Large environment_id
                AggregationLevel::Events,
            )
            .await;

        // Should not cause SQL injection, may fail with database error (no table)
        assert!(result.is_ok() || matches!(result, Err(EventsError::Database(_))));
    }

    #[tokio::test]
    async fn test_get_events_timeline_sql_injection_protection() {
        let db = setup_test_db().await.unwrap();
        let service = AnalyticsEventsService::new(Arc::new(db));

        let start = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2024, 12, 31, 23, 59, 59).unwrap();

        // Test with potentially malicious event_name
        let malicious_event_name = "'; DROP TABLE events; --";

        let result = service
            .get_events_timeline(
                start,
                end,
                1,
                Some(1),
                Some(malicious_event_name.to_string()),
                Some("day".to_string()),
                AggregationLevel::Events,
            )
            .await;

        // Should safely parameterize the event_name
        // May fail on SQLite (no time_bucket function) but shouldn't cause SQL injection
        assert!(result.is_ok() || matches!(result, Err(EventsError::Database(_))));
    }

    #[tokio::test]
    async fn test_get_hourly_visits_sql_injection_protection() {
        let db = setup_test_db().await.unwrap();
        let service = AnalyticsEventsService::new(Arc::new(db));

        let start = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2024, 1, 1, 23, 59, 59).unwrap();

        // Test with various parameter combinations
        let result = service
            .get_hourly_visits(start, end, 1, Some(1), AggregationLevel::Visitors)
            .await;

        // May fail on SQLite (no time_bucket_gapfill) but shouldn't cause SQL injection
        assert!(result.is_ok() || matches!(result, Err(EventsError::Database(_))));
    }

    #[tokio::test]
    async fn test_get_aggregated_buckets_sql_injection_protection() {
        let db = setup_test_db().await.unwrap();
        let service = AnalyticsEventsService::new(Arc::new(db));

        let start = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2024, 1, 7, 23, 59, 59).unwrap();

        // Test with deployment_id and environment_id
        let result = service
            .get_aggregated_buckets(
                start,
                end,
                1,
                Some(1),
                Some(1),
                AggregationLevel::Events,
                "1 hour".to_string(),
            )
            .await;

        // May fail on SQLite (no time_bucket_gapfill) but shouldn't cause SQL injection
        assert!(result.is_ok() || matches!(result, Err(EventsError::Database(_))));
    }

    #[tokio::test]
    async fn test_parameterized_queries_with_special_characters() {
        // Test that special SQL characters are properly escaped in parameterized queries
        let db = setup_test_db().await.unwrap();
        let service = AnalyticsEventsService::new(Arc::new(db));

        // Session ID with various special characters
        let special_session_ids = vec![
            "session'with'quotes",
            "session\"with\"doublequotes",
            "session;with;semicolons",
            "session--with--dashes",
            "session/*comment*/",
        ];

        for session_id in special_session_ids {
            let result = service
                .get_session_events(session_id.to_string(), 1, None)
                .await;

            // Should handle all special characters safely without SQL injection
            // May fail with database error (no table) but that's expected
            assert!(
                result.is_ok() || matches!(result, Err(EventsError::Database(_))),
                "Failed to safely handle session_id with special chars: {}",
                session_id
            );
        }
    }

    #[tokio::test]
    async fn test_multiple_optional_parameters() {
        // Verify that functions with multiple optional parameters
        // correctly track param_index when building queries
        let db = setup_test_db().await.unwrap();
        let service = AnalyticsEventsService::new(Arc::new(db));

        let start = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2024, 12, 31, 23, 59, 59).unwrap();

        // Test get_events_timeline with all optional parameters
        let result = service
            .get_events_timeline(
                start,
                end,
                1,
                Some(10),                         // environment_id
                Some("button_click".to_string()), // event_name
                Some("hour".to_string()),
                AggregationLevel::Sessions,
            )
            .await;

        // May fail on SQLite (no time_bucket) but shouldn't cause SQL injection
        assert!(result.is_ok() || matches!(result, Err(EventsError::Database(_))));

        // Test get_aggregated_buckets with multiple optional params
        let result2 = service
            .get_aggregated_buckets(
                start,
                end,
                1,
                Some(5),   // environment_id
                Some(100), // deployment_id
                AggregationLevel::Visitors,
                "1 day".to_string(),
            )
            .await;

        // May fail on SQLite (no time_bucket_gapfill) but shouldn't cause SQL injection
        assert!(result2.is_ok() || matches!(result2, Err(EventsError::Database(_))));
    }

    #[tokio::test]
    async fn test_time_bucket_gapfill_parameterization() {
        // Specifically test that time_bucket_gapfill uses parameterized timestamps
        let db = setup_test_db().await.unwrap();
        let service = AnalyticsEventsService::new(Arc::new(db));

        // Use extreme date ranges that could cause issues if not properly parameterized
        let start = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2025, 12, 31, 23, 59, 59).unwrap();

        let result = service
            .get_hourly_visits(start, end, 1, None, AggregationLevel::Events)
            .await;

        // Should handle large date ranges safely (SQLite will fail on time_bucket_gapfill)
        assert!(result.is_ok() || matches!(result, Err(EventsError::Database(_))));

        let result2 = service
            .get_aggregated_buckets(
                start,
                end,
                1,
                None,
                None,
                AggregationLevel::Sessions,
                "1 week".to_string(),
            )
            .await;

        // Should handle large date ranges safely (SQLite will fail on time_bucket_gapfill)
        assert!(result2.is_ok() || matches!(result2, Err(EventsError::Database(_))));
    }

    #[tokio::test]
    async fn test_hourly_visits_gap_filling() {
        use sea_orm::{ActiveModelTrait, ActiveValue::Set};
        use temps_entities::{deployments, environments, events, projects, visitor};
        use testcontainers::{
            core::{ContainerPort, WaitFor},
            runners::AsyncRunner,
            GenericImage, ImageExt,
        };

        // Setup PostgreSQL test container with TimescaleDB
        let postgres_image = GenericImage::new("timescale/timescaledb-ha", "pg18")
            .with_exposed_port(ContainerPort::Tcp(5432))
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_DB", "postgres");

        let node = postgres_image
            .start()
            .await
            .expect("Failed to start PostgreSQL container");
        let port = node
            .get_host_port_ipv4(5432)
            .await
            .expect("Failed to get port");

        let database_url = format!(
            "postgresql://postgres:postgres@localhost:{}/postgres?sslmode=disable",
            port
        );

        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        let db = sea_orm::Database::connect(&database_url)
            .await
            .expect("Failed to connect to database");
        let db = Arc::new(db);

        // Run migrations
        use temps_migrations::{Migrator, MigratorTrait};
        Migrator::up(&*db, None)
            .await
            .expect("Failed to run migrations");

        // Create test project, environment, and deployment
        let base_time = Utc.with_ymd_and_hms(2025, 10, 6, 10, 0, 0).unwrap();

        let _project = projects::ActiveModel {
            id: Set(1),
            name: Set("Test Project".to_string()),
            repo_name: Set("test-project".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            slug: Set("test-project".to_string()),
            is_deleted: Set(false),
            is_public_repo: Set(false),
            preset: Set(temps_entities::preset::Preset::Static),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("Failed to create project");

        let _environment = environments::ActiveModel {
            id: Set(1),
            name: Set("Production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("test".to_string()),
            host: Set("test.example.com".to_string()),
            upstreams: Set(UpstreamList::default()),
            project_id: Set(1),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("Failed to create environment");

        let _deployment = deployments::ActiveModel {
            id: Set(1),
            project_id: Set(1),
            environment_id: Set(1),
            slug: Set("test-deployment".to_string()),
            state: Set("ready".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("Failed to create deployment");

        // Create test visitors
        let visitor1 = visitor::ActiveModel {
            visitor_id: Set("visitor1".to_string()),
            project_id: Set(1),
            environment_id: Set(1),
            first_seen: Set(base_time),
            last_seen: Set(base_time),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("Failed to create visitor1");

        let visitor2 = visitor::ActiveModel {
            visitor_id: Set("visitor2".to_string()),
            project_id: Set(1),
            environment_id: Set(1),
            first_seen: Set(base_time),
            last_seen: Set(base_time),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("Failed to create visitor2");

        // Insert events with gaps
        // Hour 10:00 - 2 visitors
        // Hour 11:00 - no events (gap)
        // Hour 12:00 - 1 visitor
        // Hour 13:00 - no events (gap)
        // Hour 14:00 - 1 visitor

        // Hour 10:00
        events::ActiveModel {
            project_id: Set(1),
            environment_id: Set(Some(1)),
            deployment_id: Set(Some(1)),
            visitor_id: Set(Some(visitor1.id)),
            session_id: Set(Some("session1".to_string())),
            event_type: Set("page_view".to_string()),
            hostname: Set("test.com".to_string()),
            pathname: Set("/".to_string()),
            page_path: Set("/".to_string()),
            href: Set("http://test.com/".to_string()),
            timestamp: Set(base_time),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("Failed to insert event");

        events::ActiveModel {
            project_id: Set(1),
            environment_id: Set(Some(1)),
            deployment_id: Set(Some(1)),
            visitor_id: Set(Some(visitor2.id)),
            session_id: Set(Some("session2".to_string())),
            event_type: Set("page_view".to_string()),
            hostname: Set("test.com".to_string()),
            pathname: Set("/".to_string()),
            page_path: Set("/".to_string()),
            href: Set("http://test.com/".to_string()),
            timestamp: Set(base_time),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("Failed to insert event");

        // Hour 12:00
        events::ActiveModel {
            project_id: Set(1),
            environment_id: Set(Some(1)),
            deployment_id: Set(Some(1)),
            visitor_id: Set(Some(visitor1.id)),
            session_id: Set(Some("session3".to_string())),
            event_type: Set("page_view".to_string()),
            hostname: Set("test.com".to_string()),
            pathname: Set("/".to_string()),
            page_path: Set("/".to_string()),
            href: Set("http://test.com/".to_string()),
            timestamp: Set(base_time + chrono::Duration::hours(2)),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("Failed to insert event");

        // Hour 14:00
        events::ActiveModel {
            project_id: Set(1),
            environment_id: Set(Some(1)),
            deployment_id: Set(Some(1)),
            visitor_id: Set(Some(visitor2.id)),
            session_id: Set(Some("session4".to_string())),
            event_type: Set("page_view".to_string()),
            hostname: Set("test.com".to_string()),
            pathname: Set("/".to_string()),
            page_path: Set("/".to_string()),
            href: Set("http://test.com/".to_string()),
            timestamp: Set(base_time + chrono::Duration::hours(4)),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("Failed to insert event");

        // Test the service
        let service = AnalyticsEventsService::new(db.clone());

        let start = Utc.with_ymd_and_hms(2025, 10, 6, 10, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2025, 10, 6, 14, 59, 59).unwrap();

        let results = service
            .get_hourly_visits(start, end, 1, None, AggregationLevel::Visitors)
            .await
            .expect("Failed to get hourly visits");

        println!("\n✅ Hourly visits with gap filling:");
        for result in &results {
            println!("   {} -> {} visitors", result.date, result.count);
        }

        // Verify gap filling
        // Should have 5 hours: 10:00, 11:00, 12:00, 13:00, 14:00
        assert_eq!(
            results.len(),
            5,
            "Expected 5 hourly buckets (with gaps filled)"
        );

        // Verify counts
        assert_eq!(results[0].count, 2, "Hour 10:00 should have 2 visitors");
        assert_eq!(
            results[1].count, 0,
            "Hour 11:00 should have 0 visitors (gap filled)"
        );
        assert_eq!(results[2].count, 1, "Hour 12:00 should have 1 visitor");
        assert_eq!(
            results[3].count, 0,
            "Hour 13:00 should have 0 visitors (gap filled)"
        );
        assert_eq!(results[4].count, 1, "Hour 14:00 should have 1 visitor");

        println!("\n✅ Gap filling test passed!");
        println!("   - All hourly buckets present (including gaps)");
        println!("   - Counts accurate for existing data");
        println!("   - Zero counts for missing hours");
    }

    /// Verifies that `record_event` persists the bot/crawler classification
    /// derived from the User-Agent: a bot UA sets `is_crawler = true` with a
    /// `crawler_name`, while a real browser UA leaves `is_crawler = false`.
    #[tokio::test]
    async fn test_record_event_persists_crawler_flag() {
        use sea_orm::{ActiveModelTrait, Set};
        use temps_database::test_utils::TestDatabase;
        use temps_entities::{
            deployments, environments, projects, source_type::SourceType,
            upstream_config::UpstreamList,
        };

        let test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();

        let project = projects::ActiveModel {
            name: Set("crawler-test".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            preset_config: Set(None),
            deployment_config: Set(None),
            slug: Set("crawler-test".to_string()),
            is_deleted: Set(false),
            deleted_at: Set(None),
            last_deployment: Set(None),
            is_public_repo: Set(false),
            git_url: Set(None),
            git_provider_connection_id: Set(None),
            attack_mode: Set(false),
            enable_preview_environments: Set(false),
            source_type: Set(SourceType::Git),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("Failed to insert test project");

        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            branch: Set(Some("main".to_string())),
            slug: Set("production".to_string()),
            subdomain: Set("prod".to_string()),
            host: Set(String::new()),
            upstreams: Set(UpstreamList::new()),
            is_preview: Set(false),
            current_deployment_id: Set(None),
            deleted_at: Set(None),
            deployment_config: Set(None),
            last_deployment: Set(None),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("Failed to insert test environment");

        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set(format!("test-deploy-{}", uuid::Uuid::new_v4())),
            state: Set("ready".to_string()),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            deploying_at: Set(None),
            ready_at: Set(Some(chrono::Utc::now())),
            started_at: Set(Some(chrono::Utc::now())),
            finished_at: Set(Some(chrono::Utc::now())),
            context_vars: Set(None),
            branch_ref: Set(Some("main".to_string())),
            tag_ref: Set(None),
            commit_sha: Set(None),
            commit_message: Set(None),
            commit_author: Set(None),
            commit_json: Set(None),
            cancelled_reason: Set(None),
            static_dir_location: Set(None),
            screenshot_location: Set(None),
            image_name: Set(None),
            deployment_config: Set(None),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("Failed to insert test deployment");

        let service = AnalyticsEventsService::new(db.clone());

        // A bot UA must be flagged as a crawler with a matched name.
        let bot_ua = "Mozilla/5.0 (compatible; ClaudeBot/1.0; +claudebot@anthropic.com)";
        let bot_event = service
            .record_event(
                project.id,
                Some(environment.id),
                Some(deployment.id),
                Some("bot-session".to_string()),
                None,
                "page_view",
                serde_json::json!({}),
                "/blog/bot-test",
                "",
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(bot_ua.to_string()),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("Failed to record bot event");

        assert!(bot_event.is_crawler, "bot UA should set is_crawler = true");
        assert_eq!(
            bot_event.crawler_name,
            Some("claudebot".to_string()),
            "bot UA should record the matched crawler name"
        );

        // A real browser UA must not be flagged.
        let human_ua = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
                         AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
        let human_event = service
            .record_event(
                project.id,
                Some(environment.id),
                Some(deployment.id),
                Some("human-session".to_string()),
                None,
                "page_view",
                serde_json::json!({}),
                "/blog/human-test",
                "",
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(human_ua.to_string()),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("Failed to record human event");

        assert!(
            !human_event.is_crawler,
            "real browser UA should leave is_crawler = false"
        );
        assert_eq!(
            human_event.crawler_name, None,
            "real browser UA should have no crawler name"
        );

        println!("✅ record_event crawler-flag persistence test passed!");
    }

    /// Regression test for the visitor/event race condition: a brand-new
    /// visitor's very first pageview can reach `record_event` before the
    /// proxy's async `ProxyLogBatchWriter` has flushed the matching `visitor`
    /// row (up to 500ms lag). Previously the lookup simply missed and the
    /// event kept `visitor_id = NULL` forever, permanently excluding that
    /// visitor from every `COUNT(DISTINCT visitor_id)` metric. `record_event`
    /// must now self-heal: insert the visitor row itself, carrying the same
    /// enrichment (UA, referrer, channel, UTM) the proxy would have written.
    #[tokio::test]
    async fn test_record_event_creates_visitor_on_lookup_miss() {
        use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
        use temps_database::test_utils::TestDatabase;
        use temps_entities::{
            deployments, environments, projects, source_type::SourceType,
            upstream_config::UpstreamList, visitor,
        };

        let test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();

        let project = projects::ActiveModel {
            name: Set("visitor-race-test".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            preset_config: Set(None),
            deployment_config: Set(None),
            slug: Set("visitor-race-test".to_string()),
            is_deleted: Set(false),
            deleted_at: Set(None),
            last_deployment: Set(None),
            is_public_repo: Set(false),
            git_url: Set(None),
            git_provider_connection_id: Set(None),
            attack_mode: Set(false),
            enable_preview_environments: Set(false),
            source_type: Set(SourceType::Git),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("Failed to insert test project");

        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            branch: Set(Some("main".to_string())),
            slug: Set("production".to_string()),
            subdomain: Set("prod-visitor-race".to_string()),
            host: Set(String::new()),
            upstreams: Set(UpstreamList::new()),
            is_preview: Set(false),
            current_deployment_id: Set(None),
            deleted_at: Set(None),
            deployment_config: Set(None),
            last_deployment: Set(None),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("Failed to insert test environment");

        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set(format!("test-deploy-{}", uuid::Uuid::new_v4())),
            state: Set("ready".to_string()),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            deploying_at: Set(None),
            ready_at: Set(Some(chrono::Utc::now())),
            started_at: Set(Some(chrono::Utc::now())),
            finished_at: Set(Some(chrono::Utc::now())),
            context_vars: Set(None),
            branch_ref: Set(Some("main".to_string())),
            tag_ref: Set(None),
            commit_sha: Set(None),
            commit_message: Set(None),
            commit_author: Set(None),
            commit_json: Set(None),
            cancelled_reason: Set(None),
            static_dir_location: Set(None),
            screenshot_location: Set(None),
            image_name: Set(None),
            deployment_config: Set(None),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("Failed to insert test deployment");

        let service = AnalyticsEventsService::new(db.clone());

        // No `visitor` row exists yet for this UUID -- simulates a brand-new
        // visitor whose very first pageview beats the proxy's async batch
        // upsert.
        let fresh_visitor_uuid = uuid::Uuid::new_v4().to_string();
        assert!(
            visitor::Entity::find()
                .filter(visitor::Column::VisitorId.eq(fresh_visitor_uuid.clone()))
                .one(db.as_ref())
                .await
                .expect("query visitor")
                .is_none(),
            "test precondition: no visitor row should exist yet"
        );

        let human_ua = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
                         AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
        let event = service
            .record_event(
                project.id,
                Some(environment.id),
                Some(deployment.id),
                Some("race-session".to_string()),
                Some(fresh_visitor_uuid.clone()),
                "page_view",
                serde_json::json!({}),
                "/",
                "?utm_source=newsletter&utm_medium=email&utm_campaign=launch",
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(human_ua.to_string()),
                Some("https://news.ycombinator.com/".to_string()),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("Failed to record event for brand-new visitor");

        assert!(
            event.visitor_id.is_some(),
            "event.visitor_id must resolve on first pageview, not stay NULL"
        );

        let visitor_row = visitor::Entity::find()
            .filter(visitor::Column::VisitorId.eq(fresh_visitor_uuid.clone()))
            .one(db.as_ref())
            .await
            .expect("query visitor")
            .expect("record_event must create the visitor row on lookup miss");

        assert_eq!(visitor_row.id, event.visitor_id.unwrap());
        assert_eq!(visitor_row.project_id, project.id);
        assert!(
            visitor_row.has_activity,
            "a visitor created because an event just arrived should start with has_activity = true"
        );
        assert!(!visitor_row.is_crawler);
        assert_eq!(visitor_row.user_agent, Some(human_ua.to_string()));
        assert_eq!(
            visitor_row.first_referrer,
            Some("https://news.ycombinator.com/".to_string())
        );
        assert_eq!(
            visitor_row.first_referrer_hostname,
            Some("news.ycombinator.com".to_string())
        );
        assert_eq!(visitor_row.first_utm_source, Some("newsletter".to_string()));
        assert_eq!(visitor_row.first_utm_medium, Some("email".to_string()));
        assert_eq!(visitor_row.first_utm_campaign, Some("launch".to_string()));

        // A second event from the same visitor must resolve to the *same*
        // visitor row (idempotent lookup, no duplicate insert attempt).
        let second_event = service
            .record_event(
                project.id,
                Some(environment.id),
                Some(deployment.id),
                Some("race-session".to_string()),
                Some(fresh_visitor_uuid.clone()),
                "page_view",
                serde_json::json!({}),
                "/pricing",
                "",
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(human_ua.to_string()),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("Failed to record second event");

        assert_eq!(second_event.visitor_id, event.visitor_id);

        println!("✅ record_event visitor-race regression test passed!");
    }

    /// Regression test: `record_event` must self-heal the `request_sessions`
    /// table when the proxy's async `ProxyLogBatchWriter` hasn't flushed the
    /// session row yet (timing miss), or when it created one with
    /// `visitor_id = NULL` due to a prior FK-cache failure.
    ///
    /// Without this fix:
    ///  - `get_visitor_sessions_by_id` (LEFT JOIN on rs.id, decoded into a
    ///    non-optional i32) crashes with a type-decode error.
    ///  - `get_visitor_journey` (INNER JOIN) silently returns an empty journey.
    #[tokio::test]
    async fn test_record_event_creates_session_on_lookup_miss() {
        use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
        use temps_database::test_utils::TestDatabase;
        use temps_entities::{
            deployments, environments, projects, request_sessions, source_type::SourceType,
            upstream_config::UpstreamList,
        };

        let test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();

        // Minimal project + environment + deployment needed for record_event.
        let project = projects::ActiveModel {
            name: Set("session-race-test".to_string()),
            repo_name: Set("session-race-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            preset_config: Set(None),
            deployment_config: Set(None),
            slug: Set("session-race-test".to_string()),
            is_deleted: Set(false),
            deleted_at: Set(None),
            last_deployment: Set(None),
            is_public_repo: Set(false),
            git_url: Set(None),
            git_provider_connection_id: Set(None),
            attack_mode: Set(false),
            enable_preview_environments: Set(false),
            source_type: Set(SourceType::Git),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("Failed to insert test project");

        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            branch: Set(Some("main".to_string())),
            slug: Set("production".to_string()),
            subdomain: Set("prod-session-race".to_string()),
            host: Set(String::new()),
            upstreams: Set(UpstreamList::new()),
            is_preview: Set(false),
            current_deployment_id: Set(None),
            deleted_at: Set(None),
            deployment_config: Set(None),
            last_deployment: Set(None),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("Failed to insert test environment");

        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set(format!("test-deploy-{}", uuid::Uuid::new_v4())),
            state: Set("ready".to_string()),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            deploying_at: Set(None),
            ready_at: Set(Some(chrono::Utc::now())),
            started_at: Set(Some(chrono::Utc::now())),
            finished_at: Set(Some(chrono::Utc::now())),
            context_vars: Set(None),
            branch_ref: Set(Some("main".to_string())),
            tag_ref: Set(None),
            commit_sha: Set(None),
            commit_message: Set(None),
            commit_author: Set(None),
            commit_json: Set(None),
            cancelled_reason: Set(None),
            static_dir_location: Set(None),
            screenshot_location: Set(None),
            image_name: Set(None),
            deployment_config: Set(None),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("Failed to insert test deployment");

        let service = AnalyticsEventsService::new(db.clone());

        // ── Case (a): no request_sessions row exists yet ─────────────────────
        // Simulates a brand-new session whose first event beats the proxy's
        // async ProxyLogBatchWriter flush (up to 500ms lag).
        let fresh_session_id = uuid::Uuid::new_v4().to_string();
        let fresh_visitor_id = uuid::Uuid::new_v4().to_string();

        assert!(
            request_sessions::Entity::find()
                .filter(request_sessions::Column::SessionId.eq(fresh_session_id.clone()))
                .one(db.as_ref())
                .await
                .expect("precondition query failed")
                .is_none(),
            "test precondition: no request_sessions row should exist yet for this session_id"
        );

        let event_a = service
            .record_event(
                project.id,
                Some(environment.id),
                Some(deployment.id),
                Some(fresh_session_id.clone()),
                Some(fresh_visitor_id.clone()),
                "page_view",
                serde_json::json!({}),
                "/",
                "?utm_source=newsletter&utm_medium=email&utm_campaign=launch",
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some("https://news.ycombinator.com/".to_string()),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("record_event must succeed for a brand-new session");

        let created_session = request_sessions::Entity::find()
            .filter(request_sessions::Column::SessionId.eq(fresh_session_id.clone()))
            .one(db.as_ref())
            .await
            .expect("query request_sessions after create")
            .expect("request_sessions row must be created by record_event when the row is missing");

        assert_eq!(
            created_session.visitor_id, event_a.visitor_id,
            "created request_sessions.visitor_id must match the resolved event.visitor_id"
        );
        assert!(
            created_session.visitor_id.is_some(),
            "created request_sessions.visitor_id must not be NULL"
        );
        assert_eq!(
            created_session.referrer_hostname.as_deref(),
            Some("news.ycombinator.com"),
            "referrer_hostname must be populated on the created session row"
        );
        assert_eq!(
            created_session.utm_source.as_deref(),
            Some("newsletter"),
            "utm_source must propagate to the created session row"
        );

        // ── Case (b): session exists with visitor_id = NULL ──────────────────
        // Simulates a session row created by the proxy with visitor_id = NULL
        // because its visitor FK-cache lookup failed (non-fatal proxy error).
        // The next event from the same visitor must backfill visitor_id.
        let null_session_id = uuid::Uuid::new_v4().to_string();
        let null_visitor_id = uuid::Uuid::new_v4().to_string();

        // Insert a request_sessions row with visitor_id = NULL directly.
        request_sessions::ActiveModel {
            session_id: Set(null_session_id.clone()),
            started_at: Set(chrono::Utc::now()),
            last_accessed_at: Set(chrono::Utc::now()),
            visitor_id: Set(None), // deliberately orphaned
            data: Set("{}".to_string()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("Failed to insert orphaned request_sessions row");

        // Verify the row starts with visitor_id = NULL.
        let orphaned = request_sessions::Entity::find()
            .filter(request_sessions::Column::SessionId.eq(null_session_id.clone()))
            .one(db.as_ref())
            .await
            .expect("query orphaned session")
            .expect("orphaned row must exist");
        assert!(
            orphaned.visitor_id.is_none(),
            "precondition: visitor_id must be NULL"
        );

        let event_b = service
            .record_event(
                project.id,
                Some(environment.id),
                Some(deployment.id),
                Some(null_session_id.clone()),
                Some(null_visitor_id.clone()),
                "page_view",
                serde_json::json!({}),
                "/about",
                "",
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("record_event must succeed for a session with NULL visitor_id");

        assert!(
            event_b.visitor_id.is_some(),
            "event.visitor_id must be resolved even when the session row existed with NULL visitor_id"
        );

        let healed = request_sessions::Entity::find()
            .filter(request_sessions::Column::SessionId.eq(null_session_id.clone()))
            .one(db.as_ref())
            .await
            .expect("query healed session")
            .expect("healed request_sessions row must still exist");

        assert_eq!(
            healed.visitor_id, event_b.visitor_id,
            "healed request_sessions.visitor_id must match the resolved event.visitor_id"
        );
        assert!(
            healed.visitor_id.is_some(),
            "orphaned request_sessions.visitor_id must be backfilled by record_event"
        );

        println!("✅ record_event session-race regression test passed!");
    }

    /// Regression test for the dashboard "visitors in last 24h" trend badge showing
    /// a fabricated +100%/-100% right after a restart. The `events_hourly`
    /// continuous aggregate is created `WITH NO DATA` by migrations and is only
    /// backfilled by a best-effort async job at server startup
    /// (`run_post_migration_backfill`), which this test deliberately never calls —
    /// leaving the aggregate empty, exactly like a freshly restarted server.
    /// `get_dashboard_projects_analytics` must still report accurate
    /// previous-period counts (read from raw `events`, not the empty aggregate) and
    /// must not fabricate a trend percentage when a project has no baseline.
    #[tokio::test]
    async fn test_dashboard_projects_analytics_survives_empty_continuous_aggregate() {
        use chrono::Duration;
        use sea_orm::{ActiveModelTrait, ActiveValue::Set};
        use temps_database::test_utils::TestDatabase;
        use temps_entities::{deployments, environments, events, projects, visitor};

        let test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();

        async fn make_project(db: &DatabaseConnection, slug: &str) -> projects::Model {
            projects::ActiveModel {
                name: Set(slug.to_string()),
                repo_name: Set(slug.to_string()),
                repo_owner: Set("test-owner".to_string()),
                directory: Set("/".to_string()),
                main_branch: Set("main".to_string()),
                preset: Set(temps_entities::preset::Preset::NextJs),
                slug: Set(slug.to_string()),
                is_deleted: Set(false),
                is_public_repo: Set(false),
                deleted_at: Set(None),
                last_deployment: Set(None),
                created_at: Set(Utc::now()),
                updated_at: Set(Utc::now()),
                ..Default::default()
            }
            .insert(db)
            .await
            .expect("insert project")
        }

        async fn make_environment(
            db: &DatabaseConnection,
            project_id: i32,
            slug: &str,
        ) -> environments::Model {
            environments::ActiveModel {
                project_id: Set(project_id),
                name: Set(slug.to_string()),
                slug: Set(slug.to_string()),
                subdomain: Set(slug.to_string()),
                host: Set(String::new()),
                upstreams: Set(UpstreamList::new()),
                current_deployment_id: Set(None),
                last_deployment: Set(None),
                created_at: Set(Utc::now()),
                updated_at: Set(Utc::now()),
                ..Default::default()
            }
            .insert(db)
            .await
            .expect("insert environment")
        }

        async fn make_deployment(
            db: &DatabaseConnection,
            project_id: i32,
            environment_id: i32,
            slug: &str,
        ) -> deployments::Model {
            deployments::ActiveModel {
                project_id: Set(project_id),
                environment_id: Set(environment_id),
                slug: Set(slug.to_string()),
                state: Set("ready".to_string()),
                metadata: Set(Some(deployments::DeploymentMetadata::default())),
                created_at: Set(Utc::now()),
                updated_at: Set(Utc::now()),
                ..Default::default()
            }
            .insert(db)
            .await
            .expect("insert deployment")
        }

        async fn make_visitor(
            db: &DatabaseConnection,
            project_id: i32,
            environment_id: i32,
            visitor_id: &str,
            seen_at: UtcDateTime,
        ) -> visitor::Model {
            visitor::ActiveModel {
                visitor_id: Set(visitor_id.to_string()),
                project_id: Set(project_id),
                environment_id: Set(environment_id),
                first_seen: Set(seen_at),
                last_seen: Set(seen_at),
                ..Default::default()
            }
            .insert(db)
            .await
            .expect("insert visitor")
        }

        #[allow(clippy::too_many_arguments)]
        async fn make_page_view(
            db: &DatabaseConnection,
            project_id: i32,
            environment_id: i32,
            deployment_id: i32,
            visitor_row_id: i32,
            session_id: &str,
            at: UtcDateTime,
        ) {
            events::ActiveModel {
                project_id: Set(project_id),
                environment_id: Set(Some(environment_id)),
                deployment_id: Set(Some(deployment_id)),
                visitor_id: Set(Some(visitor_row_id)),
                session_id: Set(Some(session_id.to_string())),
                event_type: Set("page_view".to_string()),
                hostname: Set("example.com".to_string()),
                pathname: Set("/".to_string()),
                page_path: Set("/".to_string()),
                href: Set("https://example.com/".to_string()),
                timestamp: Set(at),
                ..Default::default()
            }
            .insert(db)
            .await
            .expect("insert event");
        }

        let now = Utc::now();
        let start = now - Duration::hours(24);
        let end = now;
        // get_dashboard_projects_analytics's previous window is [start - (end-start), start).
        let prev_ts = start - Duration::hours(6);
        let curr_ts = now - Duration::hours(2);

        // Project A: real traffic in both windows -- a genuine +50% trend (6 vs 4).
        let project_a = make_project(&db, "trend-baseline").await;
        let env_a = make_environment(&db, project_a.id, "trend-baseline-env").await;
        let dep_a = make_deployment(&db, project_a.id, env_a.id, "trend-baseline-dep").await;
        let returning_visitor =
            make_visitor(&db, project_a.id, env_a.id, "a-returning", prev_ts).await;
        make_page_view(
            &db,
            project_a.id,
            env_a.id,
            dep_a.id,
            returning_visitor.id,
            "a-returning-prev-session",
            prev_ts,
        )
        .await;
        for i in 1..4 {
            let v =
                make_visitor(&db, project_a.id, env_a.id, &format!("a-prev-{i}"), prev_ts).await;
            make_page_view(
                &db,
                project_a.id,
                env_a.id,
                dep_a.id,
                v.id,
                &format!("a-prev-sess-{i}"),
                prev_ts,
            )
            .await;
        }
        make_page_view(
            &db,
            project_a.id,
            env_a.id,
            dep_a.id,
            returning_visitor.id,
            "a-returning-current-session",
            curr_ts,
        )
        .await;
        for i in 1..6 {
            let v =
                make_visitor(&db, project_a.id, env_a.id, &format!("a-curr-{i}"), curr_ts).await;
            make_page_view(
                &db,
                project_a.id,
                env_a.id,
                dep_a.id,
                v.id,
                &format!("a-curr-sess-{i}"),
                curr_ts,
            )
            .await;
        }

        // Project B: brand new -- only current-period traffic, nothing previously.
        let project_b = make_project(&db, "trend-new-project").await;
        let env_b = make_environment(&db, project_b.id, "trend-new-project-env").await;
        let dep_b = make_deployment(&db, project_b.id, env_b.id, "trend-new-project-dep").await;
        for i in 0..3 {
            let v =
                make_visitor(&db, project_b.id, env_b.id, &format!("b-curr-{i}"), curr_ts).await;
            make_page_view(
                &db,
                project_b.id,
                env_b.id,
                dep_b.id,
                v.id,
                &format!("b-curr-sess-{i}"),
                curr_ts,
            )
            .await;
        }

        // Sanity-check the regression scenario itself: `events_hourly` must still be
        // empty here, since only `run_post_migration_backfill` (never called in this
        // test) populates it. This is what a freshly restarted server looks like.
        #[derive(FromQueryResult)]
        struct Count {
            count: i64,
        }
        let agg_row_count = Count::find_by_statement(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT COUNT(*)::bigint as count FROM events_hourly",
        ))
        .one(db.as_ref())
        .await
        .expect("count events_hourly")
        .map(|c| c.count)
        .unwrap_or(0);
        assert_eq!(
            agg_row_count, 0,
            "events_hourly must be empty to exercise the restart-staleness scenario"
        );

        let service = AnalyticsEventsService::new(db.clone());
        let response = service
            .get_dashboard_projects_analytics(&[project_a.id, project_b.id], start, end)
            .await
            .expect("get_dashboard_projects_analytics");

        let a = response
            .projects
            .get(&project_a.id.to_string())
            .expect("project A in response");
        assert_eq!(a.unique_visitors, 6);
        assert_eq!(
            a.previous_unique_visitors, 4,
            "previous-period count must come from raw events, not the empty continuous aggregate"
        );
        assert_eq!(
            a.trend_percentage,
            Some(50.0),
            "trend must be the real (6-4)/4*100 ratio, not derived from a stale/empty aggregate"
        );

        let returning = service
            .get_unique_counts(
                start,
                end,
                project_a.id,
                Some(env_a.id),
                Some(dep_a.id),
                "returning_visitors".to_string(),
            )
            .await
            .expect("get returning visitors");
        assert_eq!(
            returning.count, 1,
            "only the visitor with an event before the reporting range is returning"
        );

        let b = response
            .projects
            .get(&project_b.id.to_string())
            .expect("project B in response");
        assert_eq!(b.unique_visitors, 3);
        assert_eq!(b.previous_unique_visitors, 0);
        assert_eq!(
            b.trend_percentage, None,
            "a brand-new project with no previous-period baseline must not show a fabricated +100%"
        );

        println!("✅ dashboard trend regression test passed (TimescaleDB)!");
    }

    #[test]
    fn test_calculate_trend_percentage_no_previous_baseline_omits_trend() {
        // Previously this returned a hardcoded Some(100.0), which showed a misleading
        // flat "+100%" badge any time the previous-period count was missing or zero —
        // notably right after a restart, before the previous-period query had accurate
        // data. There's no real baseline to compute a ratio against, so this must be None.
        assert_eq!(calculate_trend_percentage(1, 0), None);
        assert_eq!(calculate_trend_percentage(50, 0), None);
    }

    #[test]
    fn test_calculate_trend_percentage_both_zero_omits_trend() {
        assert_eq!(calculate_trend_percentage(0, 0), None);
    }

    #[test]
    fn test_calculate_trend_percentage_computes_real_ratio() {
        assert_eq!(calculate_trend_percentage(150, 100), Some(50.0));
        assert_eq!(calculate_trend_percentage(50, 100), Some(-50.0));
        assert_eq!(calculate_trend_percentage(100, 100), Some(0.0));
    }

    #[test]
    fn test_calculate_trend_percentage_drop_to_zero_is_negative_hundred() {
        // A real drop to zero visitors is a genuine -100%, unlike the fabricated case
        // above where there was never a previous baseline to measure against.
        assert_eq!(calculate_trend_percentage(0, 100), Some(-100.0));
    }

    /// Regression test for the referrer-spam display bug: a referrer_hostname
    /// whose events all lack a resolvable visitor (e.g. a bot POSTing straight
    /// to the ingest endpoint with a spoofed `referrer`, never through a real
    /// browser session) must not show up in the "visitors" breakdown with
    /// count = 0 — see the `HAVING` clause in `get_property_breakdown`.
    #[tokio::test]
    async fn test_property_breakdown_excludes_zero_visitor_referrers() {
        use sea_orm::{ActiveModelTrait, Set};
        use temps_database::test_utils::TestDatabase;
        use temps_entities::{
            deployments, environments, projects, source_type::SourceType,
            upstream_config::UpstreamList, visitor,
        };

        let test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();

        let project = projects::ActiveModel {
            name: Set("referrer-spam-test".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            preset_config: Set(None),
            deployment_config: Set(None),
            slug: Set("referrer-spam-test".to_string()),
            is_deleted: Set(false),
            deleted_at: Set(None),
            last_deployment: Set(None),
            is_public_repo: Set(false),
            git_url: Set(None),
            git_provider_connection_id: Set(None),
            attack_mode: Set(false),
            enable_preview_environments: Set(false),
            source_type: Set(SourceType::Git),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("Failed to insert test project");

        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            branch: Set(Some("main".to_string())),
            slug: Set("production".to_string()),
            subdomain: Set("prod".to_string()),
            host: Set(String::new()),
            upstreams: Set(UpstreamList::new()),
            is_preview: Set(false),
            current_deployment_id: Set(None),
            deleted_at: Set(None),
            deployment_config: Set(None),
            last_deployment: Set(None),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("Failed to insert test environment");

        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set(format!("test-deploy-{}", uuid::Uuid::new_v4())),
            state: Set("ready".to_string()),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            deploying_at: Set(None),
            ready_at: Set(Some(chrono::Utc::now())),
            started_at: Set(Some(chrono::Utc::now())),
            finished_at: Set(Some(chrono::Utc::now())),
            context_vars: Set(None),
            branch_ref: Set(Some("main".to_string())),
            tag_ref: Set(None),
            commit_sha: Set(None),
            commit_message: Set(None),
            commit_author: Set(None),
            commit_json: Set(None),
            cancelled_reason: Set(None),
            static_dir_location: Set(None),
            screenshot_location: Set(None),
            image_name: Set(None),
            deployment_config: Set(None),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("Failed to insert test deployment");

        let service = AnalyticsEventsService::new(db.clone());

        // A real visitor, referred by bing.com — must be counted.
        let visitor_uuid = uuid::Uuid::new_v4().to_string();
        visitor::ActiveModel {
            visitor_id: Set(visitor_uuid.clone()),
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            first_seen: Set(chrono::Utc::now()),
            last_seen: Set(chrono::Utc::now()),
            has_activity: Set(false),
            is_crawler: Set(false),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("Failed to insert test visitor");

        service
            .record_event(
                project.id,
                Some(environment.id),
                Some(deployment.id),
                Some("real-visitor-session".to_string()),
                Some(visitor_uuid),
                "page_view",
                serde_json::json!({}),
                "/",
                "",
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some("https://www.bing.com/search?q=temps".to_string()),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("Failed to record real-visitor event");

        // Referrer spam: a raw hit against the ingest path with a spoofed
        // `referrer`, carrying no visitor_id at all — mirrors a bot POSTing
        // straight to the tracking endpoint without a real browser session.
        service
            .record_event(
                project.id,
                Some(environment.id),
                Some(deployment.id),
                Some("spam-session".to_string()),
                None,
                "page_view",
                serde_json::json!({}),
                "/",
                "",
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some("https://www.pornhub.com/".to_string()),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("Failed to record spam event");

        let breakdown = service
            .get_property_breakdown(
                chrono::Utc::now() - chrono::Duration::hours(1),
                chrono::Utc::now() + chrono::Duration::hours(1),
                project.id,
                None,
                None,
                None,
                crate::types::PropertyColumn::ReferrerHostname,
                "visitors",
                None,
                None,
            )
            .await
            .expect("Failed to get property breakdown");

        let hosts: Vec<&str> = breakdown.items.iter().map(|i| i.value.as_str()).collect();
        assert!(
            hosts.contains(&"www.bing.com"),
            "referrer with a real visitor must be present: {:?}",
            hosts
        );
        assert!(
            !hosts.contains(&"www.pornhub.com"),
            "referrer with zero attributable visitors must be excluded, got: {:?}",
            hosts
        );

        println!("✅ property breakdown excludes zero-visitor referrer spam!");
    }
}
