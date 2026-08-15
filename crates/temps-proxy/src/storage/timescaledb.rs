//! TimescaleDB (PostgreSQL) implementation of [`ProxyLogStorage`].
//!
//! This is the DEFAULT backend and preserves the exact prior behaviour: every
//! read method delegates to the existing [`ProxyLogService`] query logic, and
//! the write path reproduces the prior multi-row batch INSERT against the
//! `proxy_logs` hypertable. No behaviour changes — this is purely the original
//! code relocated behind the trait.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, QueryResult, Statement, TransactionTrait,
    Value,
};
use temps_core::UtcDateTime;
use temps_entities::proxy_logs;
use tracing::debug;

use super::ProxyLogStorage;
use crate::handler::proxy_logs::ProxyLogsQuery;
use crate::service::proxy_log_service::{
    AiAgentBreakdownRow, AiAgentTimelineRow, AiPageBreakdownRow, AiStatusBreakdownRow,
    AiTimelineGroupBy, CreateProxyLogRequest, ProjectHealthSummary, ProxyLogService,
    ProxyLogServiceError, StatsFilters, TimeBucketStats,
};
use crate::traffic_aggregation::{
    TrafficAggregationRequest, TrafficAggregationResponse, TrafficAggregationRow, TrafficDimension,
    TrafficDimensionValue, TrafficFilter, TrafficFilterOperator, TrafficMetric,
    TrafficMetricValues, TrafficOrderField, TrafficSortDirection,
};

/// The exact column list (and order) used by the prior `batch_insert`. Kept
/// verbatim so the INSERT is byte-for-byte identical to the original hard-coded
/// path. NOTE: `trace_id` and `error_group_id` are intentionally omitted here to
/// match the original behaviour (they exist on the entity but were never written
/// by the batch path).
const INSERT_COLUMNS: [&str; 35] = [
    "timestamp",
    "method",
    "path",
    "query_string",
    "host",
    "status_code",
    "response_time_ms",
    "request_source",
    "is_system_request",
    "routing_status",
    "project_id",
    "environment_id",
    "deployment_id",
    "session_id",
    "visitor_id",
    "container_id",
    "upstream_host",
    "error_message",
    "client_ip",
    "user_agent",
    "referrer",
    "request_id",
    "ip_geolocation_id",
    "browser",
    "browser_version",
    "operating_system",
    "device_type",
    "is_bot",
    "bot_name",
    "request_size_bytes",
    "response_size_bytes",
    "cache_status",
    "request_headers",
    "response_headers",
    "created_date",
];

/// TimescaleDB-backed proxy log storage.
///
/// Holds the same primitives the prior code used (`db` + `ip_service`) and an
/// internal [`ProxyLogService`] used purely to relay read queries — that struct
/// is a thin wrapper over the same two `Arc`s, so this introduces no extra
/// connections or state. Writes are performed directly here via the original
/// multi-row INSERT.
pub struct TimescaleDbProxyLogStore {
    db: Arc<DatabaseConnection>,
    /// Internal relay used to dispatch read queries to the unchanged
    /// `ProxyLogService` implementation. Cheap to hold (two `Arc` clones).
    reader: ProxyLogService,
}

impl TimescaleDbProxyLogStore {
    pub fn new(db: Arc<DatabaseConnection>, ip_service: Arc<temps_geo::IpAddressService>) -> Self {
        let reader = ProxyLogService::new(db.clone(), ip_service);
        Self { db, reader }
    }

    /// Multi-row batch INSERT, identical to the original
    /// `ProxyLogBatchWriter::batch_insert`. Entries are expected to be already
    /// enriched by the caller.
    async fn batch_insert(&self, entries: &[CreateProxyLogRequest]) -> Result<(), sea_orm::DbErr> {
        if entries.is_empty() {
            return Ok(());
        }

        let cols_per_row = INSERT_COLUMNS.len();

        let mut sql = format!(
            "INSERT INTO proxy_logs ({}) VALUES ",
            INSERT_COLUMNS.join(", ")
        );

        let mut params: Vec<sea_orm::Value> = Vec::with_capacity(entries.len() * cols_per_row);
        let now = chrono::Utc::now();

        for (i, entry) in entries.iter().enumerate() {
            if i > 0 {
                sql.push_str(", ");
            }
            let offset = i * cols_per_row;
            sql.push('(');
            for j in 0..cols_per_row {
                if j > 0 {
                    sql.push_str(", ");
                }
                sql.push_str(&format!("${}", offset + j + 1));
            }
            sql.push(')');

            let created_date = now.date_naive();

            params.push(now.into()); // timestamp
            params.push(entry.method.clone().into()); // method
            params.push(entry.path.clone().into()); // path
            params.push(entry.query_string.clone().into()); // query_string
            params.push(entry.host.clone().into()); // host
            params.push(entry.status_code.into()); // status_code
            params.push(entry.response_time_ms.into()); // response_time_ms
            params.push(entry.request_source.clone().into()); // request_source
            params.push(entry.is_system_request.into()); // is_system_request
            params.push(entry.routing_status.clone().into()); // routing_status
            params.push(entry.project_id.into()); // project_id
            params.push(entry.environment_id.into()); // environment_id
            params.push(entry.deployment_id.into()); // deployment_id
            params.push(entry.session_id.into()); // session_id
            params.push(entry.visitor_id.into()); // visitor_id
            params.push(entry.container_id.clone().into()); // container_id
            params.push(entry.upstream_host.clone().into()); // upstream_host
            params.push(entry.error_message.clone().into()); // error_message
            params.push(entry.client_ip.clone().into()); // client_ip
            params.push(entry.user_agent.clone().into()); // user_agent
            params.push(entry.referrer.clone().into()); // referrer
            params.push(entry.request_id.clone().into()); // request_id
            params.push(entry.ip_geolocation_id.into()); // ip_geolocation_id
            params.push(entry.browser.clone().into()); // browser
            params.push(entry.browser_version.clone().into()); // browser_version
            params.push(entry.operating_system.clone().into()); // operating_system
            params.push(entry.device_type.clone().into()); // device_type
            params.push(entry.is_bot.into()); // is_bot
            params.push(entry.bot_name.clone().into()); // bot_name
            params.push(entry.request_size_bytes.into()); // request_size_bytes
            params.push(entry.response_size_bytes.into()); // response_size_bytes
            params.push(entry.cache_status.clone().into()); // cache_status
            params.push(entry.request_headers.clone().into()); // request_headers
            params.push(entry.response_headers.clone().into()); // response_headers
            params.push(created_date.into()); // created_date
        }

        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                params,
            ))
            .await?;

        debug!("Batch inserted {} proxy log entries", entries.len());
        Ok(())
    }
}

#[async_trait]
impl ProxyLogStorage for TimescaleDbProxyLogStore {
    async fn write_batch(
        &self,
        entries: Vec<CreateProxyLogRequest>,
    ) -> Result<(), ProxyLogServiceError> {
        self.batch_insert(&entries).await?;
        Ok(())
    }

    async fn aggregate_traffic(
        &self,
        project_id: i32,
        request: TrafficAggregationRequest,
    ) -> Result<TrafficAggregationResponse, ProxyLogServiceError> {
        request.validate()?;
        let (sql, values, count_sql) = build_traffic_query(project_id, &request)?;
        let count_values = values.clone();
        let transaction = self.db.begin().await?;
        transaction
            .execute(Statement::from_string(
                DatabaseBackend::Postgres,
                "SET LOCAL statement_timeout = '15s'".to_string(),
            ))
            .await?;
        let rows = transaction
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                values,
            ))
            .await?;
        let empty_page_total = if rows.is_empty() {
            let row = transaction
                .query_one(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    count_sql,
                    count_values,
                ))
                .await?;
            row.map(|row| row.try_get::<i64>("", "total_groups"))
                .transpose()?
                .map(|value| value.max(0) as u64)
        } else {
            None
        };
        transaction.commit().await?;
        let mut response = traffic_response_from_rows(&request, rows)?;
        if let Some(total_groups) = empty_page_total {
            response.total_groups = total_groups;
            response.total_pages = total_groups.div_ceil(request.page_size);
        }
        Ok(response)
    }

    async fn list_with_filters(
        &self,
        start_date: Option<UtcDateTime>,
        end_date: Option<UtcDateTime>,
        filters: ProxyLogsQuery,
        page: u64,
        page_size: u64,
    ) -> Result<(Vec<proxy_logs::Model>, u64), ProxyLogServiceError> {
        self.reader
            .list_with_filters(start_date, end_date, filters, page, page_size)
            .await
    }

    async fn list_page(
        &self,
        start_date: Option<UtcDateTime>,
        end_date: Option<UtcDateTime>,
        filters: ProxyLogsQuery,
        limit: u64,
    ) -> Result<Vec<proxy_logs::Model>, ProxyLogServiceError> {
        self.reader
            .list_page(start_date, end_date, filters, limit)
            .await
    }

    async fn get_by_id(
        &self,
        id: i32,
        timestamp: Option<UtcDateTime>,
    ) -> Result<Option<proxy_logs::Model>, ProxyLogServiceError> {
        self.reader.get_by_id(id, timestamp).await
    }

    async fn get_by_request_id(
        &self,
        request_id: &str,
        timestamp: Option<UtcDateTime>,
    ) -> Result<Option<proxy_logs::Model>, ProxyLogServiceError> {
        self.reader.get_by_request_id(request_id, timestamp).await
    }

    async fn get_today_count(
        &self,
        filters: Option<StatsFilters>,
    ) -> Result<i64, ProxyLogServiceError> {
        self.reader.get_today_count(filters).await
    }

    async fn get_time_bucket_stats(
        &self,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        bucket_interval: String,
        filters: Option<StatsFilters>,
    ) -> Result<Vec<TimeBucketStats>, ProxyLogServiceError> {
        self.reader
            .get_time_bucket_stats(start_time, end_time, bucket_interval, filters)
            .await
    }

    async fn get_projects_health_summary(
        &self,
        project_ids: &[i32],
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        is_bot: Option<bool>,
    ) -> Result<Vec<ProjectHealthSummary>, ProxyLogServiceError> {
        self.reader
            .get_projects_health_summary(project_ids, start_time, end_time, is_bot)
            .await
    }

    async fn get_ai_agent_breakdown(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        path: Option<String>,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        limit: u64,
    ) -> Result<Vec<AiAgentBreakdownRow>, ProxyLogServiceError> {
        self.reader
            .get_ai_agent_breakdown(
                project_id,
                environment_id,
                path,
                start_time,
                end_time,
                limit,
            )
            .await
    }

    async fn get_ai_page_breakdown(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        path: Option<String>,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        limit: u64,
    ) -> Result<Vec<AiPageBreakdownRow>, ProxyLogServiceError> {
        self.reader
            .get_ai_page_breakdown(
                project_id,
                environment_id,
                path,
                start_time,
                end_time,
                limit,
            )
            .await
    }

    async fn get_ai_agent_timeline(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        bucket_interval: String,
        group_by: AiTimelineGroupBy,
    ) -> Result<Vec<AiAgentTimelineRow>, ProxyLogServiceError> {
        self.reader
            .get_ai_agent_timeline(
                project_id,
                environment_id,
                start_time,
                end_time,
                bucket_interval,
                group_by,
            )
            .await
    }

    async fn get_ai_status_breakdown(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
    ) -> Result<Vec<AiStatusBreakdownRow>, ProxyLogServiceError> {
        self.reader
            .get_ai_status_breakdown(project_id, environment_id, start_time, end_time)
            .await
    }
}

fn pg_dimension_expression(dimension: TrafficDimension) -> &'static str {
    match dimension {
        TrafficDimension::ClientIp => "NULLIF(client_ip, '')",
        TrafficDimension::Method => "method",
        TrafficDimension::Path => "path",
        TrafficDimension::Host => "host",
        TrafficDimension::StatusCode => "status_code",
        TrafficDimension::StatusClass => {
            "CASE WHEN status_code BETWEEN 100 AND 599 THEN (status_code / 100)::text || 'xx' ELSE 'other' END"
        }
        TrafficDimension::EnvironmentId => "environment_id",
        TrafficDimension::DeploymentId => "deployment_id",
        TrafficDimension::RequestSource => "request_source",
        TrafficDimension::IsBot => "is_bot",
        TrafficDimension::Browser => "NULLIF(browser, '')",
        TrafficDimension::OperatingSystem => "NULLIF(operating_system, '')",
        TrafficDimension::DeviceType => "NULLIF(device_type, '')",
        TrafficDimension::CacheStatus => "NULLIF(cache_status, '')",
    }
}

fn metric_alias(metric: TrafficMetric) -> &'static str {
    match metric {
        TrafficMetric::Requests => "requests",
        TrafficMetric::Errors => "errors",
        TrafficMetric::ErrorRate => "error_rate",
        TrafficMetric::LatencyAvg => "latency_avg_ms",
        TrafficMetric::LatencyMin => "latency_min_ms",
        TrafficMetric::LatencyMax => "latency_max_ms",
        TrafficMetric::LatencyP50 => "latency_p50_ms",
        TrafficMetric::LatencyP95 => "latency_p95_ms",
        TrafficMetric::LatencyP99 => "latency_p99_ms",
        TrafficMetric::UniqueIps => "unique_ips",
        TrafficMetric::UniquePaths => "unique_paths",
        TrafficMetric::LastSeen => "last_seen",
    }
}

fn pg_metric_expression(metric: TrafficMetric) -> &'static str {
    match metric {
        TrafficMetric::Requests => "COUNT(*)::bigint",
        TrafficMetric::Errors => "COUNT(*) FILTER (WHERE status_code >= 400)::bigint",
        TrafficMetric::ErrorRate => {
            "COUNT(*) FILTER (WHERE status_code >= 400)::double precision / NULLIF(COUNT(*), 0)::double precision"
        }
        TrafficMetric::LatencyAvg => "AVG(response_time_ms::double precision)",
        TrafficMetric::LatencyMin => "MIN(response_time_ms)::double precision",
        TrafficMetric::LatencyMax => "MAX(response_time_ms)::double precision",
        TrafficMetric::LatencyP50 => {
            "PERCENTILE_CONT(0.50) WITHIN GROUP (ORDER BY response_time_ms) FILTER (WHERE response_time_ms IS NOT NULL)"
        }
        TrafficMetric::LatencyP95 => {
            "PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY response_time_ms) FILTER (WHERE response_time_ms IS NOT NULL)"
        }
        TrafficMetric::LatencyP99 => {
            "PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY response_time_ms) FILTER (WHERE response_time_ms IS NOT NULL)"
        }
        TrafficMetric::UniqueIps => "COUNT(DISTINCT NULLIF(client_ip, ''))::bigint",
        TrafficMetric::UniquePaths => "COUNT(DISTINCT path)::bigint",
        TrafficMetric::LastSeen => "MAX(timestamp)",
    }
}

fn null_metric_expression(metric: TrafficMetric) -> &'static str {
    match metric {
        TrafficMetric::Requests
        | TrafficMetric::Errors
        | TrafficMetric::UniqueIps
        | TrafficMetric::UniquePaths => "NULL::bigint",
        TrafficMetric::LastSeen => "NULL::timestamptz",
        _ => "NULL::double precision",
    }
}

fn bind_filter_value(
    dimension: TrafficDimension,
    raw: &str,
) -> Result<Value, ProxyLogServiceError> {
    match dimension {
        TrafficDimension::StatusCode => raw.parse::<i16>().map(Value::from).map_err(|_| {
            ProxyLogServiceError::InvalidFilter(format!(
                "status_code filter value '{raw}' is not a valid HTTP status"
            ))
        }),
        TrafficDimension::EnvironmentId | TrafficDimension::DeploymentId => {
            raw.parse::<i32>().map(Value::from).map_err(|_| {
                ProxyLogServiceError::InvalidFilter(format!(
                    "{} filter value '{raw}' is not a valid integer",
                    dimension.api_name()
                ))
            })
        }
        TrafficDimension::IsBot => raw.parse::<bool>().map(Value::from).map_err(|_| {
            ProxyLogServiceError::InvalidFilter(format!(
                "is_bot filter value '{raw}' must be true or false"
            ))
        }),
        _ => Ok(raw.to_owned().into()),
    }
}

fn escape_like(raw: &str) -> String {
    raw.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn append_pg_filter(
    filter: &TrafficFilter,
    conditions: &mut Vec<String>,
    values: &mut Vec<Value>,
) -> Result<(), ProxyLogServiceError> {
    let expression = pg_dimension_expression(filter.dimension);
    let mut push_value = |raw: &str| -> Result<String, ProxyLogServiceError> {
        values.push(bind_filter_value(filter.dimension, raw)?);
        Ok(format!("${}", values.len()))
    };

    let condition = match filter.operator {
        TrafficFilterOperator::Eq | TrafficFilterOperator::NotEq => {
            let placeholder = push_value(&filter.values[0])?;
            let operator = if filter.operator == TrafficFilterOperator::Eq {
                "="
            } else {
                "<>"
            };
            format!("{expression} {operator} {placeholder}")
        }
        TrafficFilterOperator::Contains | TrafficFilterOperator::StartsWith => {
            let escaped = escape_like(&filter.values[0]);
            let pattern = if filter.operator == TrafficFilterOperator::Contains {
                format!("%{escaped}%")
            } else {
                format!("{escaped}%")
            };
            let placeholder = push_value(&pattern)?;
            format!("CAST({expression} AS TEXT) ILIKE {placeholder} ESCAPE '\\'")
        }
        TrafficFilterOperator::In => {
            let placeholders = filter
                .values
                .iter()
                .map(|raw| push_value(raw))
                .collect::<Result<Vec<_>, _>>()?;
            format!("{expression} IN ({})", placeholders.join(", "))
        }
    };
    conditions.push(condition);
    Ok(())
}

fn build_traffic_query(
    project_id: i32,
    request: &TrafficAggregationRequest,
) -> Result<(String, Vec<Value>, String), ProxyLogServiceError> {
    let mut values = vec![
        project_id.into(),
        request.start_time.into(),
        request.end_time.into(),
    ];
    let mut conditions = vec![
        "project_id = $1".to_string(),
        "timestamp >= $2".to_string(),
        "timestamp < $3".to_string(),
    ];
    if let Some(environment_id) = request.environment_id {
        values.push(environment_id.into());
        conditions.push(format!("environment_id = ${}", values.len()));
    }
    if !request.include_synthetic {
        conditions.push("request_source <> 'temps_monitor'".to_string());
        // Covers rows written before monitor traffic was classified at ingest.
        conditions.push("COALESCE(user_agent, '') NOT LIKE 'Temps-Status-Monitor/%'".to_string());
    }
    for filter in &request.filters {
        append_pg_filter(filter, &mut conditions, &mut values)?;
    }

    let mut dimension_selects = Vec::with_capacity(4);
    let mut groups = Vec::with_capacity(request.dimensions.len());
    for index in 0..4 {
        if let Some(dimension) = request.dimensions.get(index).copied() {
            let expression = pg_dimension_expression(dimension);
            dimension_selects.push(format!("CAST({expression} AS TEXT) AS d{index}"));
            groups.push(expression);
        } else {
            dimension_selects.push(format!("NULL::text AS d{index}"));
        }
    }

    let all_metrics = [
        TrafficMetric::Requests,
        TrafficMetric::Errors,
        TrafficMetric::ErrorRate,
        TrafficMetric::LatencyAvg,
        TrafficMetric::LatencyMin,
        TrafficMetric::LatencyMax,
        TrafficMetric::LatencyP50,
        TrafficMetric::LatencyP95,
        TrafficMetric::LatencyP99,
        TrafficMetric::UniqueIps,
        TrafficMetric::UniquePaths,
        TrafficMetric::LastSeen,
    ];
    let metric_selects = all_metrics.map(|metric| {
        let expression = if request.metrics.contains(&metric) {
            pg_metric_expression(metric)
        } else {
            null_metric_expression(metric)
        };
        format!("{expression} AS {}", metric_alias(metric))
    });

    let mut orders = request
        .order_by
        .iter()
        .map(|order| {
            let field = match order.field {
                TrafficOrderField::Dimension(dimension) => {
                    let index = request
                        .dimensions
                        .iter()
                        .position(|candidate| *candidate == dimension)
                        .unwrap_or(0);
                    format!("d{index}")
                }
                TrafficOrderField::Metric(metric) => metric_alias(metric).to_string(),
            };
            let direction = match order.direction {
                TrafficSortDirection::Asc => "ASC",
                TrafficSortDirection::Desc => "DESC",
            };
            format!("{field} {direction} NULLS LAST")
        })
        .collect::<Vec<_>>();
    if orders.is_empty() {
        let metric = request
            .metrics
            .first()
            .copied()
            .unwrap_or(TrafficMetric::Requests);
        orders.push(format!("{} DESC NULLS LAST", metric_alias(metric)));
    }
    for index in 0..request.dimensions.len() {
        let prefix = format!("d{index} ");
        if !orders.iter().any(|order| order.starts_with(&prefix)) {
            orders.push(format!("d{index} ASC NULLS LAST"));
        }
    }
    let order = orders.join(", ");
    let offset = (request.page - 1).saturating_mul(request.page_size);
    let selects = dimension_selects
        .into_iter()
        .chain(metric_selects)
        .collect::<Vec<_>>()
        .join(",\n                ");
    let sql = format!(
        "SELECT {selects}, COUNT(*) OVER()::bigint AS total_groups\n\
         FROM proxy_logs\n\
         WHERE {conditions}\n\
         {group_by}\n\
         ORDER BY {order}\n\
         LIMIT {limit} OFFSET {offset}",
        conditions = conditions.join(" AND "),
        group_by = if groups.is_empty() {
            String::new()
        } else {
            format!("GROUP BY {}", groups.join(", "))
        },
        limit = request.page_size,
    );
    let count_sql = if groups.is_empty() {
        "SELECT 1::bigint AS total_groups".to_string()
    } else {
        format!(
            "SELECT COUNT(*)::bigint AS total_groups FROM (SELECT 1 FROM proxy_logs WHERE {conditions} GROUP BY {groups}) grouped",
            conditions = conditions.join(" AND "),
            groups = groups.join(", "),
        )
    };
    Ok((sql, values, count_sql))
}

fn traffic_response_from_rows(
    request: &TrafficAggregationRequest,
    rows: Vec<QueryResult>,
) -> Result<TrafficAggregationResponse, ProxyLogServiceError> {
    let total_groups = rows
        .first()
        .map(|row| row.try_get::<i64>("", "total_groups"))
        .transpose()?
        .unwrap_or(0)
        .max(0) as u64;
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        let mut dimensions = Vec::with_capacity(request.dimensions.len());
        for (index, dimension) in request.dimensions.iter().copied().enumerate() {
            dimensions.push(TrafficDimensionValue {
                dimension,
                value: row.try_get("", &format!("d{index}"))?,
            });
        }
        result.push(TrafficAggregationRow {
            dimensions,
            metrics: TrafficMetricValues {
                requests: row.try_get("", "requests")?,
                errors: row.try_get("", "errors")?,
                error_rate: row.try_get("", "error_rate")?,
                latency_avg_ms: row.try_get("", "latency_avg_ms")?,
                latency_min_ms: row.try_get("", "latency_min_ms")?,
                latency_max_ms: row.try_get("", "latency_max_ms")?,
                latency_p50_ms: row.try_get("", "latency_p50_ms")?,
                latency_p95_ms: row.try_get("", "latency_p95_ms")?,
                latency_p99_ms: row.try_get("", "latency_p99_ms")?,
                unique_ips: row.try_get("", "unique_ips")?,
                unique_paths: row.try_get("", "unique_paths")?,
                last_seen: row.try_get("", "last_seen")?,
            },
        });
    }
    let total_pages = total_groups.div_ceil(request.page_size);
    Ok(TrafficAggregationResponse {
        rows: result,
        total_groups,
        page: request.page,
        page_size: request.page_size,
        total_pages,
        dimensions: request.dimensions.clone(),
        metrics: request.metrics.clone(),
        synthetic_excluded: !request.include_synthetic,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traffic_aggregation::TrafficOrderBy;
    use chrono::Utc;

    fn make_store() -> TimescaleDbProxyLogStore {
        unsafe { std::env::set_var("TEMPS_GEO_MOCK", "true") };
        let db = Arc::new(DatabaseConnection::default());
        let geoip =
            Arc::new(temps_geo::GeoIpService::new().expect("Failed to create mock GeoIpService"));
        let ip_service = Arc::new(temps_geo::IpAddressService::new(db.clone(), geoip));
        TimescaleDbProxyLogStore::new(db, ip_service)
    }

    #[tokio::test]
    async fn test_batch_insert_empty_is_noop() {
        let store = make_store();
        // Empty batch must short-circuit without touching the (disconnected) db.
        let result = store.batch_insert(&[]).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_insert_columns_are_35() {
        // Regression guard: the prior batch_insert wrote exactly these 35
        // columns. trace_id / error_group_id are intentionally excluded.
        assert_eq!(INSERT_COLUMNS.len(), 35);
        assert!(!INSERT_COLUMNS.contains(&"trace_id"));
        assert!(!INSERT_COLUMNS.contains(&"error_group_id"));
        assert!(!INSERT_COLUMNS.contains(&"id"));
    }

    #[test]
    fn test_store_constructs() {
        let _store = make_store();
    }

    #[test]
    fn traffic_query_supports_multi_dimension_metrics_and_excludes_monitor() {
        let mut request = TrafficAggregationRequest {
            start_time: chrono::Utc::now() - chrono::Duration::hours(1),
            end_time: chrono::Utc::now(),
            environment_id: Some(3),
            dimensions: vec![TrafficDimension::ClientIp, TrafficDimension::Path],
            metrics: vec![
                TrafficMetric::Requests,
                TrafficMetric::LatencyMin,
                TrafficMetric::LatencyMax,
                TrafficMetric::ErrorRate,
            ],
            filters: vec![TrafficFilter {
                dimension: TrafficDimension::Path,
                operator: TrafficFilterOperator::Eq,
                values: vec!["/api/health' OR TRUE --".to_string()],
            }],
            order_by: vec![TrafficOrderBy {
                field: TrafficOrderField::Metric(TrafficMetric::ErrorRate),
                direction: TrafficSortDirection::Asc,
            }],
            include_synthetic: false,
            page: 1,
            page_size: 20,
        };
        let (sql, values, _count_sql) = build_traffic_query(7, &request).expect("valid query");
        assert!(sql.contains("CAST(NULLIF(client_ip, '') AS TEXT) AS d0"));
        assert!(sql.contains("CAST(path AS TEXT) AS d1"));
        assert!(sql.contains("MIN(response_time_ms)::double precision"));
        assert!(sql.contains("MAX(response_time_ms)::double precision"));
        assert!(sql.contains("request_source <> 'temps_monitor'"));
        assert!(sql.contains("Temps-Status-Monitor/%"));
        assert!(sql
            .contains("ORDER BY error_rate ASC NULLS LAST, d0 ASC NULLS LAST, d1 ASC NULLS LAST"));
        assert!(!sql.contains("OR TRUE"), "filter values must remain bound");
        assert_eq!(values.len(), 5);

        request.order_by[0].direction = TrafficSortDirection::Desc;
        let (sql, _, _) = build_traffic_query(7, &request).expect("valid descending query");
        assert!(sql
            .contains("ORDER BY error_rate DESC NULLS LAST, d0 ASC NULLS LAST, d1 ASC NULLS LAST"));
    }

    #[tokio::test]
    async fn timescale_traffic_aggregation_returns_drilldowns_and_excludes_monitor() {
        use temps_database::test_utils::TestDatabase;

        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                eprintln!("Skipping Timescale traffic aggregation test: {error}");
                return;
            }
        };
        let db = test_db.connection_arc().clone();
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO projects (id, name, repo_name, repo_owner, directory, main_branch, \
             preset, created_at, updated_at, slug) VALUES \
             (7001, 'traffic-aggregation', 'repo', 'owner', '.', 'main', 'nodejs', now(), now(), \
              'traffic-aggregation')",
            Vec::<Value>::new(),
        ))
        .await
        .expect("insert traffic aggregation project");

        unsafe { std::env::set_var("TEMPS_GEO_MOCK", "true") };
        let geoip = Arc::new(
            temps_geo::GeoIpService::new().expect("create mock GeoIpService for aggregation test"),
        );
        let ip_service = Arc::new(temps_geo::IpAddressService::new(db.clone(), geoip));
        let store = TimescaleDbProxyLogStore::new(db, ip_service);

        let make_entry = |request_id: &str,
                          path: &str,
                          ip: &str,
                          status_code: i16,
                          response_time_ms: i32,
                          request_source: &str,
                          user_agent: &str| CreateProxyLogRequest {
            method: "GET".to_string(),
            path: path.to_string(),
            query_string: None,
            host: "example.test".to_string(),
            status_code,
            response_time_ms: Some(response_time_ms),
            request_source: request_source.to_string(),
            is_system_request: request_source == "temps_monitor",
            routing_status: "routed".to_string(),
            project_id: Some(7001),
            environment_id: None,
            deployment_id: None,
            session_id: None,
            visitor_id: None,
            container_id: None,
            upstream_host: None,
            error_message: None,
            client_ip: Some(ip.to_string()),
            user_agent: Some(user_agent.to_string()),
            referrer: None,
            request_id: request_id.to_string(),
            ip_geolocation_id: None,
            browser: None,
            browser_version: None,
            operating_system: None,
            device_type: None,
            is_bot: Some(false),
            bot_name: None,
            request_size_bytes: None,
            response_size_bytes: None,
            cache_status: None,
            request_headers: None,
            response_headers: None,
            trace_id: None,
            error_group_id: None,
            visitor_uuid: None,
            session_uuid: None,
        };
        store
            .write_batch(vec![
                make_entry(
                    "traffic-fast",
                    "/api/users",
                    "203.0.113.10",
                    200,
                    10,
                    "proxy",
                    "Mozilla/5.0",
                ),
                make_entry(
                    "traffic-slow-error",
                    "/api/users",
                    "203.0.113.10",
                    500,
                    90,
                    "proxy",
                    "Mozilla/5.0",
                ),
                make_entry(
                    "traffic-monitor",
                    "/api/health",
                    "203.0.113.99",
                    200,
                    5,
                    "temps_monitor",
                    "Temps-Status-Monitor/1.0",
                ),
            ])
            .await
            .expect("insert Timescale traffic fixtures");

        let request = TrafficAggregationRequest {
            start_time: Utc::now() - chrono::Duration::minutes(2),
            end_time: Utc::now() + chrono::Duration::minutes(2),
            environment_id: None,
            dimensions: vec![
                TrafficDimension::ClientIp,
                TrafficDimension::Path,
                TrafficDimension::Method,
            ],
            metrics: vec![
                TrafficMetric::Requests,
                TrafficMetric::Errors,
                TrafficMetric::ErrorRate,
                TrafficMetric::LatencyMin,
                TrafficMetric::LatencyAvg,
                TrafficMetric::LatencyMax,
            ],
            filters: Vec::new(),
            order_by: vec![TrafficOrderBy {
                field: TrafficOrderField::Metric(TrafficMetric::Requests),
                direction: TrafficSortDirection::Desc,
            }],
            include_synthetic: false,
            page: 1,
            page_size: 20,
        };
        let response = store
            .aggregate_traffic(7001, request.clone())
            .await
            .expect("aggregate Timescale traffic");

        assert_eq!(response.total_groups, 1);
        assert!(response.synthetic_excluded);
        let row = response.rows.first().expect("customer traffic row");
        assert_eq!(row.dimensions[0].value.as_deref(), Some("203.0.113.10"));
        assert_eq!(row.dimensions[1].value.as_deref(), Some("/api/users"));
        assert_eq!(row.dimensions[2].value.as_deref(), Some("GET"));
        assert_eq!(row.metrics.requests, Some(2));
        assert_eq!(row.metrics.errors, Some(1));
        assert_eq!(row.metrics.error_rate, Some(0.5));
        assert_eq!(row.metrics.latency_min_ms, Some(10.0));
        assert_eq!(row.metrics.latency_avg_ms, Some(50.0));
        assert_eq!(row.metrics.latency_max_ms, Some(90.0));

        let empty_page = store
            .aggregate_traffic(7001, TrafficAggregationRequest { page: 2, ..request })
            .await
            .expect("aggregate out-of-range Timescale traffic page");
        assert!(empty_page.rows.is_empty());
        assert_eq!(empty_page.total_groups, 1);
        assert_eq!(empty_page.total_pages, 1);
    }
}
