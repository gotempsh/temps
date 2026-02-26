use anyhow::Result;
use sea_orm::{
    prelude::*, ActiveModelTrait, DatabaseBackend, FromQueryResult, QueryFilter, QueryOrder, Set,
    Statement,
};
use serde::Serialize;
use std::sync::Arc;
use temps_core::UtcDateTime;
use temps_entities::{performance_metrics, request_sessions, visitor};
use tracing::info;
use utoipa::ToSchema;
use woothee::parser::Parser;

#[derive(Debug)]
pub enum PerformanceError {
    DatabaseError(String),
    ProjectNotFound,
    Other(String),
}

impl From<sea_orm::DbErr> for PerformanceError {
    fn from(err: sea_orm::DbErr) -> Self {
        PerformanceError::DatabaseError(err.to_string())
    }
}

/// Configuration for recording performance metrics
#[derive(Debug, Clone)]
pub struct RecordPerformanceMetricsConfig {
    pub project_id: i32,
    pub environment_id: i32,
    pub deployment_id: i32,
    pub session_id: Option<String>,
    pub visitor_id: Option<String>,
    pub ip_address_id: Option<i32>,
    pub ttfb: Option<f32>,
    pub lcp: Option<f32>,
    pub fid: Option<f32>,
    pub fcp: Option<f32>,
    pub cls: Option<f32>,
    pub inp: Option<f32>,
    pub pathname: Option<String>,
    pub query: Option<String>,
    pub host: Option<String>,
    pub user_agent: Option<String>,
    pub screen_width: Option<i16>,
    pub screen_height: Option<i16>,
    pub viewport_width: Option<i16>,
    pub viewport_height: Option<i16>,
    pub language: Option<String>,
}

/// Configuration for updating performance metrics
#[derive(Debug, Clone)]
pub struct UpdatePerformanceMetricsConfig {
    pub project_id: i32,
    pub environment_id: i32,
    pub deployment_id: i32,
    pub session_id: Option<String>,
    pub visitor_id: Option<String>,
    pub cls: Option<f32>,
    pub inp: Option<f32>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PerformanceMetricsResponse {
    // Base metrics
    pub ttfb: Option<f32>,
    pub lcp: Option<f32>,
    pub fid: Option<f32>,
    pub fcp: Option<f32>,
    pub cls: Option<f32>,
    pub inp: Option<f32>,

    // P75 percentiles
    pub ttfb_p75: Option<f32>,
    pub lcp_p75: Option<f32>,
    pub fid_p75: Option<f32>,
    pub fcp_p75: Option<f32>,
    pub cls_p75: Option<f32>,
    pub inp_p75: Option<f32>,

    // P90 percentiles
    pub ttfb_p90: Option<f32>,
    pub lcp_p90: Option<f32>,
    pub fid_p90: Option<f32>,
    pub fcp_p90: Option<f32>,
    pub cls_p90: Option<f32>,
    pub inp_p90: Option<f32>,

    // P95 percentiles
    pub ttfb_p95: Option<f32>,
    pub lcp_p95: Option<f32>,
    pub fid_p95: Option<f32>,
    pub fcp_p95: Option<f32>,
    pub cls_p95: Option<f32>,
    pub inp_p95: Option<f32>,

    // P99 percentiles
    pub ttfb_p99: Option<f32>,
    pub lcp_p99: Option<f32>,
    pub fid_p99: Option<f32>,
    pub fcp_p99: Option<f32>,
    pub cls_p99: Option<f32>,
    pub inp_p99: Option<f32>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MetricsOverTimeResponse {
    pub timestamps: Vec<String>,
    // Time series data
    pub ttfb: Vec<Option<f32>>,
    pub lcp: Vec<Option<f32>>,
    pub fid: Vec<Option<f32>>,
    pub fcp: Vec<Option<f32>>,
    pub cls: Vec<Option<f32>>,
    pub inp: Vec<Option<f32>>,

    // Single values for percentiles
    pub ttfb_p75: Option<f32>,
    pub lcp_p75: Option<f32>,
    pub fid_p75: Option<f32>,
    pub fcp_p75: Option<f32>,
    pub cls_p75: Option<f32>,
    pub inp_p75: Option<f32>,

    pub ttfb_p90: Option<f32>,
    pub lcp_p90: Option<f32>,
    pub fid_p90: Option<f32>,
    pub fcp_p90: Option<f32>,
    pub cls_p90: Option<f32>,
    pub inp_p90: Option<f32>,

    pub ttfb_p95: Option<f32>,
    pub lcp_p95: Option<f32>,
    pub fid_p95: Option<f32>,
    pub fcp_p95: Option<f32>,
    pub cls_p95: Option<f32>,
    pub inp_p95: Option<f32>,

    pub ttfb_p99: Option<f32>,
    pub lcp_p99: Option<f32>,
    pub fid_p99: Option<f32>,
    pub fcp_p99: Option<f32>,
    pub cls_p99: Option<f32>,
    pub inp_p99: Option<f32>,
}

#[derive(Debug, Clone)]
pub enum GroupBy {
    Path,
    Country,
    DeviceType,
    Browser,
    OperatingSystem,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GroupedPageMetric {
    pub group_key: String,
    pub lcp: Option<f32>,
    pub cls: Option<f32>,
    pub inp: Option<f32>,
    pub fcp: Option<f32>,
    pub ttfb: Option<f32>,
    pub events: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GroupedPageMetricsResponse {
    pub groups: Vec<GroupedPageMetric>,
    pub total_events: i64,
    pub grouped_by: String,
}

pub struct PerformanceService {
    db: Arc<DatabaseConnection>,
}

impl PerformanceService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Translate frontend device type filter values ("desktop", "mobile") to the
    /// woothee category values stored in the database ("pc", "smartphone", "mobilephone").
    fn woothee_device_types(device_type: &str) -> Vec<String> {
        match device_type.to_lowercase().as_str() {
            "desktop" => vec!["pc".to_string()],
            "mobile" => vec!["smartphone".to_string(), "mobilephone".to_string()],
            // Pass through as-is for any other value (e.g. direct woothee categories)
            other => vec![other.to_string()],
        }
    }

    /// Build the WHERE clause and params for percentile/time-series SQL queries.
    fn build_percentile_where(
        project_id: i32,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        device_type: Option<String>,
    ) -> (String, Vec<sea_orm::Value>) {
        let mut conditions = vec![
            "project_id = $1".to_string(),
            "recorded_at >= $2".to_string(),
            "recorded_at <= $3".to_string(),
        ];
        let mut params: Vec<sea_orm::Value> =
            vec![project_id.into(), start_date.into(), end_date.into()];
        let mut idx = 3;

        if let Some(env_id) = environment_id {
            idx += 1;
            conditions.push(format!("environment_id = ${}", idx));
            params.push(env_id.into());
        }

        if let Some(dep_id) = deployment_id {
            idx += 1;
            conditions.push(format!("deployment_id = ${}", idx));
            params.push(dep_id.into());
        }

        if let Some(ref device) = device_type {
            let categories = Self::woothee_device_types(device);
            let placeholders: Vec<String> = categories
                .iter()
                .map(|c| {
                    idx += 1;
                    params.push(c.clone().into());
                    format!("${}", idx)
                })
                .collect();
            conditions.push(format!("device_type IN ({})", placeholders.join(", ")));
        }

        (conditions.join(" AND "), params)
    }

    pub async fn get_metrics(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        device_type: Option<String>,
    ) -> Result<PerformanceMetricsResponse, PerformanceError> {
        let (where_clause, params) = Self::build_percentile_where(
            project_id,
            start_date,
            end_date,
            environment_id,
            deployment_id,
            device_type,
        );

        // Use SQL percentile_cont to compute all stats in a single query — no data loaded into Rust
        let sql = format!(
            r#"
            SELECT
                AVG(ttfb)::float4 as ttfb_avg,
                AVG(lcp)::float4 as lcp_avg,
                AVG(fid)::float4 as fid_avg,
                AVG(fcp)::float4 as fcp_avg,
                AVG(cls)::float4 as cls_avg,
                AVG(inp)::float4 as inp_avg,
                (percentile_cont(0.75) WITHIN GROUP (ORDER BY ttfb))::float4 as ttfb_p75,
                (percentile_cont(0.75) WITHIN GROUP (ORDER BY lcp))::float4 as lcp_p75,
                (percentile_cont(0.75) WITHIN GROUP (ORDER BY fid))::float4 as fid_p75,
                (percentile_cont(0.75) WITHIN GROUP (ORDER BY fcp))::float4 as fcp_p75,
                (percentile_cont(0.75) WITHIN GROUP (ORDER BY cls))::float4 as cls_p75,
                (percentile_cont(0.75) WITHIN GROUP (ORDER BY inp))::float4 as inp_p75,
                (percentile_cont(0.90) WITHIN GROUP (ORDER BY ttfb))::float4 as ttfb_p90,
                (percentile_cont(0.90) WITHIN GROUP (ORDER BY lcp))::float4 as lcp_p90,
                (percentile_cont(0.90) WITHIN GROUP (ORDER BY fid))::float4 as fid_p90,
                (percentile_cont(0.90) WITHIN GROUP (ORDER BY fcp))::float4 as fcp_p90,
                (percentile_cont(0.90) WITHIN GROUP (ORDER BY cls))::float4 as cls_p90,
                (percentile_cont(0.90) WITHIN GROUP (ORDER BY inp))::float4 as inp_p90,
                (percentile_cont(0.95) WITHIN GROUP (ORDER BY ttfb))::float4 as ttfb_p95,
                (percentile_cont(0.95) WITHIN GROUP (ORDER BY lcp))::float4 as lcp_p95,
                (percentile_cont(0.95) WITHIN GROUP (ORDER BY fid))::float4 as fid_p95,
                (percentile_cont(0.95) WITHIN GROUP (ORDER BY fcp))::float4 as fcp_p95,
                (percentile_cont(0.95) WITHIN GROUP (ORDER BY cls))::float4 as cls_p95,
                (percentile_cont(0.95) WITHIN GROUP (ORDER BY inp))::float4 as inp_p95,
                (percentile_cont(0.99) WITHIN GROUP (ORDER BY ttfb))::float4 as ttfb_p99,
                (percentile_cont(0.99) WITHIN GROUP (ORDER BY lcp))::float4 as lcp_p99,
                (percentile_cont(0.99) WITHIN GROUP (ORDER BY fid))::float4 as fid_p99,
                (percentile_cont(0.99) WITHIN GROUP (ORDER BY fcp))::float4 as fcp_p99,
                (percentile_cont(0.99) WITHIN GROUP (ORDER BY cls))::float4 as cls_p99,
                (percentile_cont(0.99) WITHIN GROUP (ORDER BY inp))::float4 as inp_p99
            FROM performance_metrics
            WHERE {}
            "#,
            where_clause
        );

        #[derive(FromQueryResult)]
        struct PercentileRow {
            ttfb_avg: Option<f32>,
            lcp_avg: Option<f32>,
            fid_avg: Option<f32>,
            fcp_avg: Option<f32>,
            cls_avg: Option<f32>,
            inp_avg: Option<f32>,
            ttfb_p75: Option<f32>,
            lcp_p75: Option<f32>,
            fid_p75: Option<f32>,
            fcp_p75: Option<f32>,
            cls_p75: Option<f32>,
            inp_p75: Option<f32>,
            ttfb_p90: Option<f32>,
            lcp_p90: Option<f32>,
            fid_p90: Option<f32>,
            fcp_p90: Option<f32>,
            cls_p90: Option<f32>,
            inp_p90: Option<f32>,
            ttfb_p95: Option<f32>,
            lcp_p95: Option<f32>,
            fid_p95: Option<f32>,
            fcp_p95: Option<f32>,
            cls_p95: Option<f32>,
            inp_p95: Option<f32>,
            ttfb_p99: Option<f32>,
            lcp_p99: Option<f32>,
            fid_p99: Option<f32>,
            fcp_p99: Option<f32>,
            cls_p99: Option<f32>,
            inp_p99: Option<f32>,
        }

        let row = PercentileRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            params,
        ))
        .one(self.db.as_ref())
        .await?;

        let row = row.unwrap_or(PercentileRow {
            ttfb_avg: None,
            lcp_avg: None,
            fid_avg: None,
            fcp_avg: None,
            cls_avg: None,
            inp_avg: None,
            ttfb_p75: None,
            lcp_p75: None,
            fid_p75: None,
            fcp_p75: None,
            cls_p75: None,
            inp_p75: None,
            ttfb_p90: None,
            lcp_p90: None,
            fid_p90: None,
            fcp_p90: None,
            cls_p90: None,
            inp_p90: None,
            ttfb_p95: None,
            lcp_p95: None,
            fid_p95: None,
            fcp_p95: None,
            cls_p95: None,
            inp_p95: None,
            ttfb_p99: None,
            lcp_p99: None,
            fid_p99: None,
            fcp_p99: None,
            cls_p99: None,
            inp_p99: None,
        });

        Ok(PerformanceMetricsResponse {
            ttfb: row.ttfb_avg,
            lcp: row.lcp_avg,
            fid: row.fid_avg,
            fcp: row.fcp_avg,
            cls: row.cls_avg,
            inp: row.inp_avg,
            ttfb_p75: row.ttfb_p75,
            lcp_p75: row.lcp_p75,
            fid_p75: row.fid_p75,
            fcp_p75: row.fcp_p75,
            cls_p75: row.cls_p75,
            inp_p75: row.inp_p75,
            ttfb_p90: row.ttfb_p90,
            lcp_p90: row.lcp_p90,
            fid_p90: row.fid_p90,
            fcp_p90: row.fcp_p90,
            cls_p90: row.cls_p90,
            inp_p90: row.inp_p90,
            ttfb_p95: row.ttfb_p95,
            lcp_p95: row.lcp_p95,
            fid_p95: row.fid_p95,
            fcp_p95: row.fcp_p95,
            cls_p95: row.cls_p95,
            inp_p95: row.inp_p95,
            ttfb_p99: row.ttfb_p99,
            lcp_p99: row.lcp_p99,
            fid_p99: row.fid_p99,
            fcp_p99: row.fcp_p99,
            cls_p99: row.cls_p99,
            inp_p99: row.inp_p99,
        })
    }

    pub async fn get_metrics_over_time(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        device_type: Option<String>,
    ) -> Result<MetricsOverTimeResponse, PerformanceError> {
        let (where_clause, params) = Self::build_percentile_where(
            project_id,
            start_date,
            end_date,
            environment_id,
            deployment_id,
            device_type,
        );

        // Single SQL query: percentiles + time-bucketed averages in one round-trip.
        // The percentiles CTE computes aggregates without loading rows into Rust.
        // The time_series CTE uses time_bucket (TimescaleDB) for hourly averages.
        let sql = format!(
            r#"
            WITH percentiles AS (
                SELECT
                    AVG(ttfb)::float4 as ttfb_avg,
                    AVG(lcp)::float4 as lcp_avg,
                    AVG(fid)::float4 as fid_avg,
                    AVG(fcp)::float4 as fcp_avg,
                    AVG(cls)::float4 as cls_avg,
                    AVG(inp)::float4 as inp_avg,
                    (percentile_cont(0.75) WITHIN GROUP (ORDER BY ttfb))::float4 as ttfb_p75,
                    (percentile_cont(0.75) WITHIN GROUP (ORDER BY lcp))::float4 as lcp_p75,
                    (percentile_cont(0.75) WITHIN GROUP (ORDER BY fid))::float4 as fid_p75,
                    (percentile_cont(0.75) WITHIN GROUP (ORDER BY fcp))::float4 as fcp_p75,
                    (percentile_cont(0.75) WITHIN GROUP (ORDER BY cls))::float4 as cls_p75,
                    (percentile_cont(0.75) WITHIN GROUP (ORDER BY inp))::float4 as inp_p75,
                    (percentile_cont(0.90) WITHIN GROUP (ORDER BY ttfb))::float4 as ttfb_p90,
                    (percentile_cont(0.90) WITHIN GROUP (ORDER BY lcp))::float4 as lcp_p90,
                    (percentile_cont(0.90) WITHIN GROUP (ORDER BY fid))::float4 as fid_p90,
                    (percentile_cont(0.90) WITHIN GROUP (ORDER BY fcp))::float4 as fcp_p90,
                    (percentile_cont(0.90) WITHIN GROUP (ORDER BY cls))::float4 as cls_p90,
                    (percentile_cont(0.90) WITHIN GROUP (ORDER BY inp))::float4 as inp_p90,
                    (percentile_cont(0.95) WITHIN GROUP (ORDER BY ttfb))::float4 as ttfb_p95,
                    (percentile_cont(0.95) WITHIN GROUP (ORDER BY lcp))::float4 as lcp_p95,
                    (percentile_cont(0.95) WITHIN GROUP (ORDER BY fid))::float4 as fid_p95,
                    (percentile_cont(0.95) WITHIN GROUP (ORDER BY fcp))::float4 as fcp_p95,
                    (percentile_cont(0.95) WITHIN GROUP (ORDER BY cls))::float4 as cls_p95,
                    (percentile_cont(0.95) WITHIN GROUP (ORDER BY inp))::float4 as inp_p95,
                    (percentile_cont(0.99) WITHIN GROUP (ORDER BY ttfb))::float4 as ttfb_p99,
                    (percentile_cont(0.99) WITHIN GROUP (ORDER BY lcp))::float4 as lcp_p99,
                    (percentile_cont(0.99) WITHIN GROUP (ORDER BY fid))::float4 as fid_p99,
                    (percentile_cont(0.99) WITHIN GROUP (ORDER BY fcp))::float4 as fcp_p99,
                    (percentile_cont(0.99) WITHIN GROUP (ORDER BY cls))::float4 as cls_p99,
                    (percentile_cont(0.99) WITHIN GROUP (ORDER BY inp))::float4 as inp_p99
                FROM performance_metrics
                WHERE {where_clause}
            ),
            time_series AS (
                SELECT
                    time_bucket('1 hour', recorded_at) as bucket,
                    AVG(ttfb)::float4 as ttfb,
                    AVG(lcp)::float4 as lcp,
                    AVG(fid)::float4 as fid,
                    AVG(fcp)::float4 as fcp,
                    AVG(cls)::float4 as cls,
                    AVG(inp)::float4 as inp
                FROM performance_metrics
                WHERE {where_clause}
                GROUP BY bucket
                ORDER BY bucket ASC
            )
            SELECT
                ts.bucket as "timestamp",
                ts.ttfb, ts.lcp, ts.fid, ts.fcp, ts.cls, ts.inp,
                p.ttfb_p75, p.lcp_p75, p.fid_p75, p.fcp_p75, p.cls_p75, p.inp_p75,
                p.ttfb_p90, p.lcp_p90, p.fid_p90, p.fcp_p90, p.cls_p90, p.inp_p90,
                p.ttfb_p95, p.lcp_p95, p.fid_p95, p.fcp_p95, p.cls_p95, p.inp_p95,
                p.ttfb_p99, p.lcp_p99, p.fid_p99, p.fcp_p99, p.cls_p99, p.inp_p99
            FROM time_series ts
            CROSS JOIN percentiles p
            "#,
            where_clause = where_clause
        );

        // The CTE references the same WHERE twice, but with the same param positions.
        // PostgreSQL allows re-use of $N placeholders, so we pass params once.

        #[derive(FromQueryResult)]
        struct OverTimeRow {
            timestamp: UtcDateTime,
            ttfb: Option<f32>,
            lcp: Option<f32>,
            fid: Option<f32>,
            fcp: Option<f32>,
            cls: Option<f32>,
            inp: Option<f32>,
            ttfb_p75: Option<f32>,
            lcp_p75: Option<f32>,
            fid_p75: Option<f32>,
            fcp_p75: Option<f32>,
            cls_p75: Option<f32>,
            inp_p75: Option<f32>,
            ttfb_p90: Option<f32>,
            lcp_p90: Option<f32>,
            fid_p90: Option<f32>,
            fcp_p90: Option<f32>,
            cls_p90: Option<f32>,
            inp_p90: Option<f32>,
            ttfb_p95: Option<f32>,
            lcp_p95: Option<f32>,
            fid_p95: Option<f32>,
            fcp_p95: Option<f32>,
            cls_p95: Option<f32>,
            inp_p95: Option<f32>,
            ttfb_p99: Option<f32>,
            lcp_p99: Option<f32>,
            fid_p99: Option<f32>,
            fcp_p99: Option<f32>,
            cls_p99: Option<f32>,
            inp_p99: Option<f32>,
        }

        let rows = OverTimeRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            params,
        ))
        .all(self.db.as_ref())
        .await?;

        let mut result = MetricsOverTimeResponse {
            timestamps: Vec::with_capacity(rows.len()),
            ttfb: Vec::with_capacity(rows.len()),
            lcp: Vec::with_capacity(rows.len()),
            fid: Vec::with_capacity(rows.len()),
            fcp: Vec::with_capacity(rows.len()),
            cls: Vec::with_capacity(rows.len()),
            inp: Vec::with_capacity(rows.len()),
            // Percentiles are the same for every row; take from first (or None if empty)
            ttfb_p75: None,
            lcp_p75: None,
            fid_p75: None,
            fcp_p75: None,
            cls_p75: None,
            inp_p75: None,
            ttfb_p90: None,
            lcp_p90: None,
            fid_p90: None,
            fcp_p90: None,
            cls_p90: None,
            inp_p90: None,
            ttfb_p95: None,
            lcp_p95: None,
            fid_p95: None,
            fcp_p95: None,
            cls_p95: None,
            inp_p95: None,
            ttfb_p99: None,
            lcp_p99: None,
            fid_p99: None,
            fcp_p99: None,
            cls_p99: None,
            inp_p99: None,
        };

        if let Some(first) = rows.first() {
            result.ttfb_p75 = first.ttfb_p75;
            result.lcp_p75 = first.lcp_p75;
            result.fid_p75 = first.fid_p75;
            result.fcp_p75 = first.fcp_p75;
            result.cls_p75 = first.cls_p75;
            result.inp_p75 = first.inp_p75;
            result.ttfb_p90 = first.ttfb_p90;
            result.lcp_p90 = first.lcp_p90;
            result.fid_p90 = first.fid_p90;
            result.fcp_p90 = first.fcp_p90;
            result.cls_p90 = first.cls_p90;
            result.inp_p90 = first.inp_p90;
            result.ttfb_p95 = first.ttfb_p95;
            result.lcp_p95 = first.lcp_p95;
            result.fid_p95 = first.fid_p95;
            result.fcp_p95 = first.fcp_p95;
            result.cls_p95 = first.cls_p95;
            result.inp_p95 = first.inp_p95;
            result.ttfb_p99 = first.ttfb_p99;
            result.lcp_p99 = first.lcp_p99;
            result.fid_p99 = first.fid_p99;
            result.fcp_p99 = first.fcp_p99;
            result.cls_p99 = first.cls_p99;
            result.inp_p99 = first.inp_p99;
        }

        for row in &rows {
            result.timestamps.push(row.timestamp.to_rfc3339());
            result.ttfb.push(row.ttfb);
            result.lcp.push(row.lcp);
            result.fid.push(row.fid);
            result.fcp.push(row.fcp);
            result.cls.push(row.cls);
            result.inp.push(row.inp);
        }

        Ok(result)
    }

    pub async fn get_grouped_page_metrics(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        group_by: GroupBy,
    ) -> Result<GroupedPageMetricsResponse, PerformanceError> {
        // Determine the grouping field and column
        let (group_field, group_by_name) = match group_by {
            GroupBy::Path => ("rl.request_path", "path"),
            GroupBy::Country => ("COALESCE(ig.country, 'Unknown')", "country"),
            GroupBy::DeviceType => (
                "CASE WHEN rl.is_mobile = true THEN 'Mobile' ELSE 'Desktop' END",
                "device_type",
            ),
            GroupBy::Browser => ("COALESCE(rl.browser, 'Unknown')", "browser"),
            GroupBy::OperatingSystem => (
                "COALESCE(rl.operating_system, 'Unknown')",
                "operating_system",
            ),
        };

        // Build the base query with proper grouping
        let mut where_conditions = vec![
            format!("pm.project_id = ${}", 1),
            format!("pm.recorded_at >= ${}", 2),
            format!("pm.recorded_at <= ${}", 3),
            "pm.is_crawler = false".to_string(),
        ];
        let mut params: Vec<sea_orm::Value> =
            vec![project_id.into(), start_date.into(), end_date.into()];
        let mut param_count = 3;

        // Add optional filters
        if let Some(env_id) = environment_id {
            param_count += 1;
            where_conditions.push(format!("pm.environment_id = ${}", param_count));
            params.push(env_id.into());
        }

        if let Some(dep_id) = deployment_id {
            param_count += 1;
            where_conditions.push(format!("pm.deployment_id = ${}", param_count));
            params.push(dep_id.into());
        }

        // Note: proxy_logs doesn't have is_static_file field
        // Removed static file filter as it's not available in proxy_logs

        // Optimized query using TimescaleDB features with flexible grouping
        let query = format!(
            r#"
            SELECT
                {} as group_key,
                AVG(pm.lcp) as lcp,
                AVG(pm.cls) as cls,
                AVG(pm.inp) as inp,
                AVG(pm.fcp) as fcp,
                AVG(pm.ttfb) as ttfb,
                COUNT(*) as events
            FROM performance_metrics pm
            LEFT JOIN proxy_logs pl ON (
                pm.project_id = pl.project_id AND
                pm.session_id = pl.session_id AND
                ABS(EXTRACT(EPOCH FROM (pm.recorded_at - pl.timestamp))) <= 300
            )
            LEFT JOIN ip_geolocations ig ON pl.client_ip = ig.ip_address
            WHERE {}
            GROUP BY {}
            HAVING COUNT(*) >= 1
            ORDER BY events DESC, group_key
            LIMIT 100
            "#,
            group_field,
            where_conditions.join(" AND "),
            group_field
        );

        info!("Executing grouped page metrics query: {}", query);

        // Execute query using TimescaleDB-optimized aggregation
        #[derive(FromQueryResult)]
        struct GroupedMetricResult {
            group_key: Option<String>,
            lcp: Option<f32>,
            cls: Option<f32>,
            inp: Option<f32>,
            fcp: Option<f32>,
            ttfb: Option<f32>,
            events: i64,
        }

        let results = GroupedMetricResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &query,
            params,
        ))
        .all(self.db.as_ref())
        .await
        .map_err(|e| {
            PerformanceError::DatabaseError(format!(
                "Failed to execute grouped page metrics query: {}",
                e
            ))
        })?;

        let groups: Vec<GroupedPageMetric> = results
            .into_iter()
            .filter_map(|r| {
                r.group_key.map(|key| GroupedPageMetric {
                    group_key: key,
                    lcp: r.lcp,
                    cls: r.cls,
                    inp: r.inp,
                    fcp: r.fcp,
                    ttfb: r.ttfb,
                    events: r.events,
                })
            })
            .collect();

        let total_events = groups.iter().map(|g| g.events).sum();

        Ok(GroupedPageMetricsResponse {
            groups,
            total_events,
            grouped_by: group_by_name.to_string(),
        })
    }

    /// Record performance metrics from client
    pub async fn record_performance_metrics(
        &self,
        config: RecordPerformanceMetricsConfig,
    ) -> Result<(), PerformanceError> {
        info!(
            "Recording performance metrics for project: {}, session: {:?}, visitor: {:?}",
            config.project_id, config.session_id, config.visitor_id
        );

        // Parse User-Agent header using woothee
        let parser = Parser::new();
        let (browser, browser_version, operating_system, operating_system_version, device_type) =
            if let Some(ua_str) = config.user_agent.as_deref() {
                if let Some(result) = parser.parse(ua_str) {
                    let browser = if result.name != "UNKNOWN" {
                        Some(result.name.to_string())
                    } else {
                        None
                    };
                    let browser_version = if !result.version.is_empty() {
                        Some(result.version.to_string())
                    } else {
                        None
                    };
                    let operating_system = if result.os != "UNKNOWN" {
                        Some(result.os.to_string())
                    } else {
                        None
                    };
                    let operating_system_version =
                        if !result.os_version.is_empty() && result.os_version != "UNKNOWN" {
                            Some(result.os_version.to_string())
                        } else {
                            None
                        };
                    let device_type = if result.category != "UNKNOWN" {
                        Some(result.category.to_string())
                    } else {
                        None
                    };
                    (
                        browser,
                        browser_version,
                        operating_system,
                        operating_system_version,
                        device_type,
                    )
                } else {
                    (None, None, None, None, None)
                }
            } else {
                (None, None, None, None, None)
            };

        // Look up session_id in request_sessions table
        let session_id_i32 = if let Some(sess_id) = config.session_id {
            request_sessions::Entity::find()
                .filter(request_sessions::Column::SessionId.eq(&sess_id))
                .one(self.db.as_ref())
                .await?
                .map(|s| s.id)
        } else {
            None
        };

        // Look up visitor_id in visitor table
        let visitor_id_i32 = if let Some(vis_id) = config.visitor_id {
            visitor::Entity::find()
                .filter(visitor::Column::VisitorId.eq(&vis_id))
                .one(self.db.as_ref())
                .await?
                .map(|v| v.id)
        } else {
            None
        };

        let metric = performance_metrics::ActiveModel {
            id: sea_orm::NotSet,
            project_id: Set(config.project_id),
            environment_id: Set(config.environment_id),
            deployment_id: Set(config.deployment_id),
            session_id: Set(session_id_i32),
            visitor_id: Set(visitor_id_i32),
            ip_address_id: Set(config.ip_address_id),
            ttfb: Set(config.ttfb),
            lcp: Set(config.lcp),
            fid: Set(config.fid),
            fcp: Set(config.fcp),
            cls: Set(config.cls),
            inp: Set(config.inp),
            recorded_at: Set(chrono::Utc::now()),
            is_crawler: Set(false),
            pathname: Set(config.pathname),
            query: Set(config.query),
            host: Set(config.host),
            browser: Set(browser),
            browser_version: Set(browser_version),
            operating_system: Set(operating_system),
            operating_system_version: Set(operating_system_version),
            device_type: Set(device_type),
            screen_width: Set(config.screen_width),
            screen_height: Set(config.screen_height),
            viewport_width: Set(config.viewport_width),
            viewport_height: Set(config.viewport_height),
            language: Set(config.language),
        };

        metric.insert(self.db.as_ref()).await?;

        Ok(())
    }

    /// Update performance metrics (for late-loading metrics like CLS, INP)
    pub async fn update_performance_metrics(
        &self,
        config: UpdatePerformanceMetricsConfig,
    ) -> Result<(), PerformanceError> {
        info!(
            "Updating late metrics for project: {}, session: {:?}, visitor: {:?}",
            config.project_id, config.session_id, config.visitor_id
        );

        // Look up session_id in request_sessions table
        let session_id_i32 = if let Some(sess_id) = config.session_id {
            request_sessions::Entity::find()
                .filter(request_sessions::Column::SessionId.eq(&sess_id))
                .one(self.db.as_ref())
                .await?
                .map(|s| s.id)
        } else {
            None
        };

        // Look up visitor_id in visitor table
        let visitor_id_i32 = if let Some(vis_id) = config.visitor_id {
            visitor::Entity::find()
                .filter(visitor::Column::VisitorId.eq(&vis_id))
                .one(self.db.as_ref())
                .await?
                .map(|v| v.id)
        } else {
            None
        };

        // Find the most recent metric for this session/visitor
        let mut query = performance_metrics::Entity::find()
            .filter(performance_metrics::Column::ProjectId.eq(config.project_id))
            .filter(performance_metrics::Column::EnvironmentId.eq(config.environment_id))
            .filter(performance_metrics::Column::DeploymentId.eq(config.deployment_id))
            .order_by_desc(performance_metrics::Column::RecordedAt);

        if let Some(sess_id) = session_id_i32 {
            query = query.filter(performance_metrics::Column::SessionId.eq(sess_id));
        }

        if let Some(vis_id) = visitor_id_i32 {
            query = query.filter(performance_metrics::Column::VisitorId.eq(vis_id));
        }

        let metric = query
            .one(self.db.as_ref())
            .await?
            .ok_or(PerformanceError::Other(
                "Metric not found for update".to_string(),
            ))?;

        let mut metric: performance_metrics::ActiveModel = metric.into();

        if let Some(cls_value) = config.cls {
            metric.cls = Set(Some(cls_value));
        }

        if let Some(inp_value) = config.inp {
            metric.inp = Set(Some(inp_value));
        }

        metric.update(self.db.as_ref()).await?;

        Ok(())
    }

    /// Check if performance metrics exist for a project
    pub async fn has_metrics(&self, project_id: i32) -> Result<bool, PerformanceError> {
        info!(
            "Checking if performance metrics exist for project: {}",
            project_id
        );

        let count = performance_metrics::Entity::find()
            .filter(performance_metrics::Column::ProjectId.eq(project_id))
            .count(self.db.as_ref())
            .await?;

        Ok(count > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    #[tokio::test]
    async fn test_has_metrics_returns_true_when_metrics_exist() {
        // Create mock database that returns count > 0
        // The count query returns a tuple with the count value
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[maplit::btreemap! {
                "num_items" => sea_orm::Value::BigInt(Some(5)),
            }]])
            .into_connection();

        let service = PerformanceService::new(Arc::new(db));
        let result = service.has_metrics(1).await;

        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[tokio::test]
    async fn test_has_metrics_returns_false_when_no_metrics_exist() {
        // Create mock database that returns count = 0
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[maplit::btreemap! {
                "num_items" => sea_orm::Value::BigInt(Some(0)),
            }]])
            .into_connection();

        let service = PerformanceService::new(Arc::new(db));
        let result = service.has_metrics(1).await;

        assert!(result.is_ok());
        assert!(!result.unwrap());
    }
}
