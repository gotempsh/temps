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

// Internal struct for SQL query results
#[derive(Debug, Default)]
struct MetricPercentiles {
    avg: Option<f32>,
    p75: Option<f32>,
    p90: Option<f32>,
    p95: Option<f32>,
    p99: Option<f32>,
}

pub struct PerformanceService {
    db: Arc<DatabaseConnection>,
}

impl PerformanceService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    pub async fn get_metrics(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
    ) -> Result<PerformanceMetricsResponse, PerformanceError> {
        // Get all metrics for the project and date range
        let mut query = performance_metrics::Entity::find()
            .filter(performance_metrics::Column::ProjectId.eq(project_id))
            .filter(performance_metrics::Column::RecordedAt.between(start_date, end_date));

        if let Some(env_id) = environment_id {
            query = query.filter(performance_metrics::Column::EnvironmentId.eq(env_id));
        }

        if let Some(dep_id) = deployment_id {
            query = query.filter(performance_metrics::Column::DeploymentId.eq(dep_id));
        }

        let metrics = query.all(self.db.as_ref()).await?;

        // Calculate simple averages and percentiles
        let ttfb_values: Vec<f32> = metrics.iter().filter_map(|m| m.ttfb).collect();
        let lcp_values: Vec<f32> = metrics.iter().filter_map(|m| m.lcp).collect();
        let fid_values: Vec<f32> = metrics.iter().filter_map(|m| m.fid).collect();
        let fcp_values: Vec<f32> = metrics.iter().filter_map(|m| m.fcp).collect();
        let cls_values: Vec<f32> = metrics.iter().filter_map(|m| m.cls).collect();
        let inp_values: Vec<f32> = metrics.iter().filter_map(|m| m.inp).collect();

        let ttfb = Self::calculate_stats(&ttfb_values);
        let lcp = Self::calculate_stats(&lcp_values);
        let fid = Self::calculate_stats(&fid_values);
        let fcp = Self::calculate_stats(&fcp_values);
        let cls = Self::calculate_stats(&cls_values);
        let inp = Self::calculate_stats(&inp_values);

        Ok(PerformanceMetricsResponse {
            ttfb: ttfb.avg,
            lcp: lcp.avg,
            fid: fid.avg,
            fcp: fcp.avg,
            cls: cls.avg,
            inp: inp.avg,

            ttfb_p75: ttfb.p75,
            lcp_p75: lcp.p75,
            fid_p75: fid.p75,
            fcp_p75: fcp.p75,
            cls_p75: cls.p75,
            inp_p75: inp.p75,

            ttfb_p90: ttfb.p90,
            lcp_p90: lcp.p90,
            fid_p90: fid.p90,
            fcp_p90: fcp.p90,
            cls_p90: cls.p90,
            inp_p90: inp.p90,

            ttfb_p95: ttfb.p95,
            lcp_p95: lcp.p95,
            fid_p95: fid.p95,
            fcp_p95: fcp.p95,
            cls_p95: cls.p95,
            inp_p95: inp.p95,

            ttfb_p99: ttfb.p99,
            lcp_p99: lcp.p99,
            fid_p99: fid.p99,
            fcp_p99: fcp.p99,
            cls_p99: cls.p99,
            inp_p99: inp.p99,
        })
    }

    pub async fn get_metrics_over_time(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
    ) -> Result<MetricsOverTimeResponse, PerformanceError> {
        // Get all metrics for percentile calculations
        let mut percentile_query = performance_metrics::Entity::find()
            .filter(performance_metrics::Column::ProjectId.eq(project_id))
            .filter(performance_metrics::Column::RecordedAt.between(start_date, end_date));

        if let Some(env_id) = environment_id {
            percentile_query =
                percentile_query.filter(performance_metrics::Column::EnvironmentId.eq(env_id));
        }

        if let Some(dep_id) = deployment_id {
            percentile_query =
                percentile_query.filter(performance_metrics::Column::DeploymentId.eq(dep_id));
        }

        let all_metrics = percentile_query.all(self.db.as_ref()).await?;

        // Calculate percentiles
        let ttfb_values: Vec<f32> = all_metrics.iter().filter_map(|m| m.ttfb).collect();
        let lcp_values: Vec<f32> = all_metrics.iter().filter_map(|m| m.lcp).collect();
        let fid_values: Vec<f32> = all_metrics.iter().filter_map(|m| m.fid).collect();
        let fcp_values: Vec<f32> = all_metrics.iter().filter_map(|m| m.fcp).collect();
        let cls_values: Vec<f32> = all_metrics.iter().filter_map(|m| m.cls).collect();
        let inp_values: Vec<f32> = all_metrics.iter().filter_map(|m| m.inp).collect();

        let ttfb = Self::calculate_stats(&ttfb_values);
        let lcp = Self::calculate_stats(&lcp_values);
        let fid = Self::calculate_stats(&fid_values);
        let fcp = Self::calculate_stats(&fcp_values);
        let cls = Self::calculate_stats(&cls_values);
        let inp = Self::calculate_stats(&inp_values);

        // Get time series data
        let mut time_query = performance_metrics::Entity::find()
            .filter(performance_metrics::Column::ProjectId.eq(project_id))
            .filter(performance_metrics::Column::RecordedAt.between(start_date, end_date))
            .order_by_asc(performance_metrics::Column::RecordedAt);

        if let Some(env_id) = environment_id {
            time_query = time_query.filter(performance_metrics::Column::EnvironmentId.eq(env_id));
        }

        if let Some(dep_id) = deployment_id {
            time_query = time_query.filter(performance_metrics::Column::DeploymentId.eq(dep_id));
        }

        let metrics = time_query.all(self.db.as_ref()).await?;

        let mut result = MetricsOverTimeResponse {
            timestamps: Vec::new(),
            ttfb: Vec::new(),
            lcp: Vec::new(),
            fid: Vec::new(),
            fcp: Vec::new(),
            cls: Vec::new(),
            inp: Vec::new(),
            // Single values for percentiles
            ttfb_p75: ttfb.p75,
            lcp_p75: lcp.p75,
            fid_p75: fid.p75,
            fcp_p75: fcp.p75,
            cls_p75: cls.p75,
            inp_p75: inp.p75,
            ttfb_p90: ttfb.p90,
            lcp_p90: lcp.p90,
            fid_p90: fid.p90,
            fcp_p90: fcp.p90,
            cls_p90: cls.p90,
            inp_p90: inp.p90,
            ttfb_p95: ttfb.p95,
            lcp_p95: lcp.p95,
            fid_p95: fid.p95,
            fcp_p95: fcp.p95,
            cls_p95: cls.p95,
            inp_p95: inp.p95,
            ttfb_p99: ttfb.p99,
            lcp_p99: lcp.p99,
            fid_p99: fid.p99,
            fcp_p99: fcp.p99,
            cls_p99: cls.p99,
            inp_p99: inp.p99,
        };

        for metric in metrics {
            result.timestamps.push(metric.recorded_at.to_rfc3339());
            result.ttfb.push(metric.ttfb);
            result.lcp.push(metric.lcp);
            result.fid.push(metric.fid);
            result.fcp.push(metric.fcp);
            result.cls.push(metric.cls);
            result.inp.push(metric.inp);
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

        // Add path filter for non-path grouping to exclude static files
        if !matches!(group_by, GroupBy::Path) {
            where_conditions.push("rl.is_static_file = false".to_string());
        }

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
            LEFT JOIN request_logs rl ON (
                pm.project_id = rl.project_id AND
                pm.session_id = rl.session_id AND
                ABS(EXTRACT(EPOCH FROM (pm.recorded_at - rl.started_at::timestamp))) <= 300
            )
            LEFT JOIN ip_geolocations ig ON rl.ip_address = ig.ip_address
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

    fn calculate_stats(values: &[f32]) -> MetricPercentiles {
        if values.is_empty() {
            return MetricPercentiles::default();
        }

        let mut sorted = values.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let len = sorted.len();
        let avg = sorted.iter().sum::<f32>() / len as f32;
        let p75 = sorted.get((len * 75) / 100).copied();
        let p90 = sorted.get((len * 90) / 100).copied();
        let p95 = sorted.get((len * 95) / 100).copied();
        let p99 = sorted.get((len * 99) / 100).copied();

        MetricPercentiles {
            avg: Some(avg),
            p75,
            p90,
            p95,
            p99,
        }
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
        info!("Checking if performance metrics exist for project: {}", project_id);

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
