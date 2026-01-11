use chrono::Utc;
use sea_orm::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::UtcDateTime;
use temps_entities::proxy_logs;
use thiserror::Error;
use utoipa::ToSchema;

#[derive(Error, Debug)]
pub enum ProxyLogServiceError {
    #[error("Database error")]
    DatabaseError(#[from] sea_orm::DbErr),

    #[error("Invalid filter parameters: {0}")]
    InvalidFilter(String),
}

/// Response model for proxy logs
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProxyLogResponse {
    pub id: i32,
    pub timestamp: String,
    pub method: String,
    pub path: String,
    pub query_string: Option<String>,
    pub host: String,
    pub status_code: i16,
    pub response_time_ms: Option<i32>,
    pub request_source: String,
    pub is_system_request: bool,
    pub routing_status: String,
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub session_id: Option<i32>,
    pub visitor_id: Option<i32>,
    pub container_id: Option<String>,
    pub upstream_host: Option<String>,
    pub error_message: Option<String>,
    pub client_ip: Option<String>,
    pub user_agent: Option<String>,
    pub referrer: Option<String>,
    pub request_id: String,
    pub ip_geolocation_id: Option<i32>,
    pub browser: Option<String>,
    pub browser_version: Option<String>,
    pub operating_system: Option<String>,
    pub device_type: Option<String>,
    pub is_bot: Option<bool>,
    pub bot_name: Option<String>,
    pub request_size_bytes: Option<i64>,
    pub response_size_bytes: Option<i64>,
    pub cache_status: Option<String>,
}

impl From<proxy_logs::Model> for ProxyLogResponse {
    fn from(model: proxy_logs::Model) -> Self {
        Self {
            id: model.id,
            timestamp: model.timestamp.to_rfc3339(),
            method: model.method,
            path: model.path,
            query_string: model.query_string,
            host: model.host,
            status_code: model.status_code,
            response_time_ms: model.response_time_ms,
            request_source: model.request_source,
            is_system_request: model.is_system_request,
            routing_status: model.routing_status,
            project_id: model.project_id,
            environment_id: model.environment_id,
            deployment_id: model.deployment_id,
            session_id: model.session_id,
            visitor_id: model.visitor_id,
            container_id: model.container_id,
            upstream_host: model.upstream_host,
            error_message: model.error_message,
            client_ip: model.client_ip,
            user_agent: model.user_agent,
            referrer: model.referrer,
            request_id: model.request_id,
            ip_geolocation_id: model.ip_geolocation_id,
            browser: model.browser,
            browser_version: model.browser_version,
            operating_system: model.operating_system,
            device_type: model.device_type,
            is_bot: model.is_bot,
            bot_name: model.bot_name,
            request_size_bytes: model.request_size_bytes,
            response_size_bytes: model.response_size_bytes,
            cache_status: model.cache_status,
        }
    }
}

/// Request to create a proxy log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateProxyLogRequest {
    pub method: String,
    pub path: String,
    pub query_string: Option<String>,
    pub host: String,
    pub status_code: i16,
    pub response_time_ms: Option<i32>,
    pub request_source: String,
    pub is_system_request: bool,
    pub routing_status: String,
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub session_id: Option<i32>,
    pub visitor_id: Option<i32>,
    pub container_id: Option<String>,
    pub upstream_host: Option<String>,
    pub error_message: Option<String>,
    pub client_ip: Option<String>,
    pub user_agent: Option<String>,
    pub referrer: Option<String>,
    pub request_id: String,
    pub ip_geolocation_id: Option<i32>,
    pub browser: Option<String>,
    pub browser_version: Option<String>,
    pub operating_system: Option<String>,
    pub device_type: Option<String>,
    pub is_bot: Option<bool>,
    pub bot_name: Option<String>,
    pub request_size_bytes: Option<i64>,
    pub response_size_bytes: Option<i64>,
    pub cache_status: Option<String>,
    pub request_headers: Option<serde_json::Value>,
    pub response_headers: Option<serde_json::Value>,
}

pub struct ProxyLogService {
    db: Arc<DatabaseConnection>,
    ip_service: Arc<temps_geo::IpAddressService>,
}

impl ProxyLogService {
    pub fn new(db: Arc<DatabaseConnection>, ip_service: Arc<temps_geo::IpAddressService>) -> Self {
        Self { db, ip_service }
    }

    /// Create a new proxy log entry asynchronously
    pub async fn create(
        &self,
        mut request: CreateProxyLogRequest,
    ) -> Result<proxy_logs::Model, ProxyLogServiceError> {
        let now = Utc::now();
        let created_date = now.date_naive();

        // Enrich with IP geolocation if not provided
        if request.ip_geolocation_id.is_none() {
            if let Some(ref client_ip) = request.client_ip {
                if let Ok(geolocation_id) = self.ip_service.get_or_create_ip(client_ip).await {
                    request.ip_geolocation_id = Some(geolocation_id.id);
                }
            }
        }

        // Parse user agent if not already parsed
        if request.browser.is_none() {
            if let Some(ref ua_string) = request.user_agent {
                let parser = woothee::parser::Parser::new();
                if let Some(ua) = parser.parse(ua_string) {
                    request.browser = Some(ua.name.to_string());
                    request.browser_version = Some(ua.version.to_string());
                    request.operating_system = Some(ua.os.to_string());
                    request.device_type = match ua.category {
                        "smartphone" => Some("mobile".to_string()),
                        "mobilephone" => Some("mobile".to_string()),
                        "pc" => Some("desktop".to_string()),
                        _ => Some(ua.category.to_string()),
                    };
                }
            }
        }

        // Detect bots/crawlers if not already detected
        if request.is_bot.is_none() {
            if let Some(ref ua_string) = request.user_agent {
                let crawler_name =
                    crate::crawler_detector::CrawlerDetector::get_crawler_name(Some(ua_string));
                request.is_bot = Some(crawler_name.is_some());
                request.bot_name = crawler_name;
            }
        }

        let new_log = proxy_logs::ActiveModel {
            timestamp: Set(now),
            method: Set(request.method),
            path: Set(request.path),
            query_string: Set(request.query_string),
            host: Set(request.host),
            status_code: Set(request.status_code),
            response_time_ms: Set(request.response_time_ms),
            request_source: Set(request.request_source),
            is_system_request: Set(request.is_system_request),
            routing_status: Set(request.routing_status),
            project_id: Set(request.project_id),
            environment_id: Set(request.environment_id),
            deployment_id: Set(request.deployment_id),
            session_id: Set(request.session_id),
            visitor_id: Set(request.visitor_id),
            container_id: Set(request.container_id),
            upstream_host: Set(request.upstream_host),
            error_message: Set(request.error_message),
            client_ip: Set(request.client_ip),
            user_agent: Set(request.user_agent),
            referrer: Set(request.referrer),
            request_id: Set(request.request_id),
            ip_geolocation_id: Set(request.ip_geolocation_id),
            browser: Set(request.browser),
            browser_version: Set(request.browser_version),
            operating_system: Set(request.operating_system),
            device_type: Set(request.device_type),
            is_bot: Set(request.is_bot),
            bot_name: Set(request.bot_name),
            request_size_bytes: Set(request.request_size_bytes),
            response_size_bytes: Set(request.response_size_bytes),
            cache_status: Set(request.cache_status),
            request_headers: Set(request.request_headers),
            response_headers: Set(request.response_headers),
            created_date: Set(created_date),
            ..Default::default()
        };

        let result = new_log.insert(self.db.as_ref()).await?;
        Ok(result)
    }

    /// Get proxy logs with filters and pagination
    pub async fn list_with_filters(
        &self,
        start_date: Option<UtcDateTime>,
        end_date: Option<UtcDateTime>,
        filters: crate::handler::proxy_logs::ProxyLogsQuery,
        page: u64,
        page_size: u64,
    ) -> Result<(Vec<proxy_logs::Model>, u64), ProxyLogServiceError> {
        let mut query = proxy_logs::Entity::find();

        // Project/Environment/Deployment filters
        if let Some(pid) = filters.project_id {
            query = query.filter(proxy_logs::Column::ProjectId.eq(pid));
        }
        if let Some(eid) = filters.environment_id {
            query = query.filter(proxy_logs::Column::EnvironmentId.eq(eid));
        }
        if let Some(did) = filters.deployment_id {
            query = query.filter(proxy_logs::Column::DeploymentId.eq(did));
        }
        if let Some(sid) = filters.session_id {
            query = query.filter(proxy_logs::Column::SessionId.eq(sid));
        }
        if let Some(vid) = filters.visitor_id {
            query = query.filter(proxy_logs::Column::VisitorId.eq(vid));
        }

        // Date range filters
        if let Some(start_date) = start_date {
            query = query.filter(proxy_logs::Column::Timestamp.gte(start_date));
        }
        if let Some(end_date) = end_date {
            query = query.filter(proxy_logs::Column::Timestamp.lte(end_date));
        }

        // Request filters
        if let Some(method) = filters.method {
            query = query.filter(proxy_logs::Column::Method.eq(method));
        }
        if let Some(host) = filters.host {
            query = query.filter(proxy_logs::Column::Host.contains(&host));
        }
        if let Some(path) = filters.path {
            query = query.filter(proxy_logs::Column::Path.contains(&path));
        }
        if let Some(ip) = filters.client_ip {
            query = query.filter(proxy_logs::Column::ClientIp.eq(ip));
        }

        // Response filters
        if let Some(code) = filters.status_code {
            query = query.filter(proxy_logs::Column::StatusCode.eq(code));
        }
        if let Some(min_time) = filters.response_time_min {
            query = query.filter(proxy_logs::Column::ResponseTimeMs.gte(min_time));
        }
        if let Some(max_time) = filters.response_time_max {
            query = query.filter(proxy_logs::Column::ResponseTimeMs.lte(max_time));
        }

        // Routing filters
        if let Some(status) = filters.routing_status {
            query = query.filter(proxy_logs::Column::RoutingStatus.eq(status));
        }
        if let Some(source) = filters.request_source {
            query = query.filter(proxy_logs::Column::RequestSource.eq(source));
        }
        if let Some(is_system) = filters.is_system_request {
            query = query.filter(proxy_logs::Column::IsSystemRequest.eq(is_system));
        }

        // User agent filters
        if let Some(ua) = filters.user_agent {
            query = query.filter(proxy_logs::Column::UserAgent.contains(&ua));
        }
        if let Some(browser) = filters.browser {
            query = query.filter(proxy_logs::Column::Browser.eq(browser));
        }
        if let Some(os) = filters.operating_system {
            query = query.filter(proxy_logs::Column::OperatingSystem.eq(os));
        }
        if let Some(device) = filters.device_type {
            query = query.filter(proxy_logs::Column::DeviceType.eq(device));
        }

        // Bot filters
        if let Some(is_bot) = filters.is_bot {
            query = query.filter(proxy_logs::Column::IsBot.eq(is_bot));
        }
        if let Some(bot_name) = filters.bot_name {
            query = query.filter(proxy_logs::Column::BotName.contains(&bot_name));
        }

        // Size filters
        if let Some(min_req_size) = filters.request_size_min {
            query = query.filter(proxy_logs::Column::RequestSizeBytes.gte(min_req_size));
        }
        if let Some(max_req_size) = filters.request_size_max {
            query = query.filter(proxy_logs::Column::RequestSizeBytes.lte(max_req_size));
        }
        if let Some(min_res_size) = filters.response_size_min {
            query = query.filter(proxy_logs::Column::ResponseSizeBytes.gte(min_res_size));
        }
        if let Some(max_res_size) = filters.response_size_max {
            query = query.filter(proxy_logs::Column::ResponseSizeBytes.lte(max_res_size));
        }

        // Cache filters
        if let Some(cache_status) = filters.cache_status {
            query = query.filter(proxy_logs::Column::CacheStatus.eq(cache_status));
        }

        // Container filters
        if let Some(container_id) = filters.container_id {
            query = query.filter(proxy_logs::Column::ContainerId.eq(container_id));
        }
        if let Some(upstream_host) = filters.upstream_host {
            query = query.filter(proxy_logs::Column::UpstreamHost.contains(&upstream_host));
        }

        // Error filter
        if let Some(has_error) = filters.has_error {
            if has_error {
                query = query.filter(proxy_logs::Column::ErrorMessage.is_not_null());
            } else {
                query = query.filter(proxy_logs::Column::ErrorMessage.is_null());
            }
        }

        // Sorting - support both snake_case and alternative naming
        let sort_col = match filters.sort_by.as_deref() {
            Some("timestamp") | None => proxy_logs::Column::Timestamp,
            Some("response_time") | Some("response_time_ms") => proxy_logs::Column::ResponseTimeMs,
            Some("status_code") => proxy_logs::Column::StatusCode,
            Some("method") => proxy_logs::Column::Method,
            Some("host") => proxy_logs::Column::Host,
            Some("path") => proxy_logs::Column::Path,
            Some("request_size") | Some("request_size_bytes") => {
                proxy_logs::Column::RequestSizeBytes
            }
            Some("response_size") | Some("response_size_bytes") => {
                proxy_logs::Column::ResponseSizeBytes
            }
            Some("client_ip") => proxy_logs::Column::ClientIp,
            Some("routing_status") => proxy_logs::Column::RoutingStatus,
            Some("project_id") => proxy_logs::Column::ProjectId,
            Some("environment_id") => proxy_logs::Column::EnvironmentId,
            Some("deployment_id") => proxy_logs::Column::DeploymentId,
            Some("request_source") => proxy_logs::Column::RequestSource,
            Some("browser") => proxy_logs::Column::Browser,
            Some("operating_system") => proxy_logs::Column::OperatingSystem,
            Some("device_type") => proxy_logs::Column::DeviceType,
            Some("is_bot") => proxy_logs::Column::IsBot,
            Some("is_system_request") => proxy_logs::Column::IsSystemRequest,
            _ => proxy_logs::Column::Timestamp,
        };

        query = match filters.sort_order.as_deref() {
            Some("asc") => query.order_by_asc(sort_col),
            _ => query.order_by_desc(sort_col),
        };

        let paginator = query.paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let items = paginator.fetch_page(page - 1).await?;

        Ok((items, total))
    }

    /// Legacy method - kept for backward compatibility
    #[allow(clippy::too_many_arguments)]
    pub async fn list(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        routing_status: Option<String>,
        status_code: Option<i16>,
        request_source: Option<String>,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<proxy_logs::Model>, u64), ProxyLogServiceError> {
        let filters = crate::handler::proxy_logs::ProxyLogsQuery {
            project_id,
            environment_id,
            deployment_id: None,
            session_id: None,
            visitor_id: None,
            start_date: None,
            end_date: None,
            method: None,
            host: None,
            path: None,
            client_ip: None,
            status_code,
            response_time_min: None,
            response_time_max: None,
            routing_status,
            request_source,
            is_system_request: None,
            user_agent: None,
            browser: None,
            operating_system: None,
            device_type: None,
            is_bot: None,
            bot_name: None,
            request_size_min: None,
            request_size_max: None,
            response_size_min: None,
            response_size_max: None,
            cache_status: None,
            container_id: None,
            upstream_host: None,
            has_error: None,
            page,
            page_size,
            sort_by: None,
            sort_order: None,
        };

        self.list_with_filters(
            None,
            None,
            filters,
            page.unwrap_or(1),
            std::cmp::min(page_size.unwrap_or(20), 100),
        )
        .await
    }

    /// Get a single proxy log by ID
    pub async fn get_by_id(
        &self,
        id: i32,
    ) -> Result<Option<proxy_logs::Model>, ProxyLogServiceError> {
        let log = proxy_logs::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?;
        Ok(log)
    }

    /// Get proxy logs by request ID (for tracing)
    pub async fn get_by_request_id(
        &self,
        request_id: &str,
    ) -> Result<Option<proxy_logs::Model>, ProxyLogServiceError> {
        let log = proxy_logs::Entity::find()
            .filter(proxy_logs::Column::RequestId.eq(request_id))
            .one(self.db.as_ref())
            .await?;
        Ok(log)
    }

    /// Get today's request count
    pub async fn get_today_count(
        &self,
        filters: Option<StatsFilters>,
    ) -> Result<i64, ProxyLogServiceError> {
        let today_start = Utc::now().date_naive().and_hms_opt(0, 0, 0).unwrap();
        let today_start = chrono::DateTime::<Utc>::from_naive_utc_and_offset(today_start, Utc);

        let mut query = proxy_logs::Entity::find();
        query = query.filter(proxy_logs::Column::Timestamp.gte(today_start));

        // Apply filters
        if let Some(filters) = filters {
            query = Self::apply_stats_filters(query, filters);
        }

        let count = query.count(self.db.as_ref()).await?;
        Ok(count as i64)
    }

    /// Get time-bucketed statistics
    pub async fn get_time_bucket_stats(
        &self,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        bucket_interval: String, // e.g., "1 hour", "1 day", "5 minutes"
        filters: Option<StatsFilters>,
    ) -> Result<Vec<TimeBucketStats>, ProxyLogServiceError> {
        // Validate bucket interval
        if !Self::is_valid_interval(&bucket_interval) {
            return Err(ProxyLogServiceError::InvalidFilter(format!(
                "Invalid bucket interval: {}",
                bucket_interval
            )));
        }

        // Build the base WHERE clause for filters
        let mut where_clauses = vec!["timestamp >= $1".to_string(), "timestamp < $2".to_string()];
        let mut param_index = 3;

        if let Some(ref f) = filters {
            Self::build_filter_sql(f, &mut param_index, &mut where_clauses);
        }

        let where_clause = if where_clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", where_clauses.join(" AND "))
        };

        // Build the TimescaleDB query with time_bucket_gapfill
        let sql = format!(
            r#"
            SELECT
                bucket::timestamptz as bucket,
                COALESCE(count, 0) as request_count,
                COALESCE(avg_response_time, 0) as avg_response_time_ms,
                COALESCE(error_count, 0) as error_count,
                COALESCE(total_request_bytes, 0) as total_request_bytes,
                COALESCE(total_response_bytes, 0) as total_response_bytes
            FROM (
                SELECT
                    time_bucket_gapfill('{}', timestamp) AS bucket,
                    COUNT(*) as count,
                    AVG(response_time_ms) as avg_response_time,
                    SUM(CASE WHEN status_code >= 400 THEN 1 ELSE 0 END) as error_count,
                    SUM(request_size_bytes) as total_request_bytes,
                    SUM(response_size_bytes) as total_response_bytes
                FROM proxy_logs
                {}
                GROUP BY bucket
            ) sub
            ORDER BY bucket ASC
            "#,
            bucket_interval, where_clause
        );

        // Execute raw SQL query
        let db_backend = sea_orm::DatabaseBackend::Postgres;

        // Build values vec for parameterized query
        let mut values: Vec<sea_orm::Value> = vec![start_time.into(), end_time.into()];

        // Add filter values
        if let Some(ref f) = filters {
            Self::add_filter_values(&mut values, f);
        }

        let stmt = sea_orm::Statement::from_sql_and_values(db_backend, &sql, values);

        let results = self.db.query_all(stmt).await?;

        // Parse results
        let stats = results
            .iter()
            .map(|row| {
                let bucket: chrono::DateTime<Utc> = row.try_get("", "bucket").unwrap_or(start_time);
                let request_count: i64 = row.try_get("", "request_count").unwrap_or(0);
                let avg_response_time_ms: f64 =
                    row.try_get("", "avg_response_time_ms").unwrap_or(0.0);
                let error_count: i64 = row.try_get("", "error_count").unwrap_or(0);
                let total_request_bytes: i64 = row.try_get("", "total_request_bytes").unwrap_or(0);
                let total_response_bytes: i64 =
                    row.try_get("", "total_response_bytes").unwrap_or(0);

                TimeBucketStats {
                    bucket: bucket.to_rfc3339(),
                    request_count,
                    avg_response_time_ms,
                    error_count,
                    total_request_bytes,
                    total_response_bytes,
                }
            })
            .collect();

        Ok(stats)
    }

    // Helper methods for filtering
    fn apply_stats_filters(
        mut query: Select<proxy_logs::Entity>,
        filters: StatsFilters,
    ) -> Select<proxy_logs::Entity> {
        if let Some(method) = filters.method {
            query = query.filter(proxy_logs::Column::Method.eq(method));
        }
        if let Some(ip) = filters.client_ip {
            query = query.filter(proxy_logs::Column::ClientIp.eq(ip));
        }
        if let Some(project_id) = filters.project_id {
            query = query.filter(proxy_logs::Column::ProjectId.eq(project_id));
        }
        if let Some(environment_id) = filters.environment_id {
            query = query.filter(proxy_logs::Column::EnvironmentId.eq(environment_id));
        }
        if let Some(deployment_id) = filters.deployment_id {
            query = query.filter(proxy_logs::Column::DeploymentId.eq(deployment_id));
        }
        if let Some(host) = filters.host {
            query = query.filter(proxy_logs::Column::Host.eq(host));
        }
        if let Some(status_code) = filters.status_code {
            query = query.filter(proxy_logs::Column::StatusCode.eq(status_code));
        }
        if let Some(routing_status) = filters.routing_status {
            query = query.filter(proxy_logs::Column::RoutingStatus.eq(routing_status));
        }
        if let Some(request_source) = filters.request_source {
            query = query.filter(proxy_logs::Column::RequestSource.eq(request_source));
        }
        if let Some(is_bot) = filters.is_bot {
            query = query.filter(proxy_logs::Column::IsBot.eq(is_bot));
        }
        if let Some(device_type) = filters.device_type {
            query = query.filter(proxy_logs::Column::DeviceType.eq(device_type));
        }
        query
    }

    fn build_filter_sql(
        filters: &StatsFilters,
        param_index: &mut i32,
        where_clauses: &mut Vec<String>,
    ) -> String {
        if filters.method.is_some() {
            where_clauses.push(format!("method = ${}", param_index));
            *param_index += 1;
        }
        if filters.client_ip.is_some() {
            where_clauses.push(format!("client_ip = ${}", param_index));
            *param_index += 1;
        }
        if filters.project_id.is_some() {
            where_clauses.push(format!("project_id = ${}", param_index));
            *param_index += 1;
        }
        if filters.environment_id.is_some() {
            where_clauses.push(format!("environment_id = ${}", param_index));
            *param_index += 1;
        }
        if filters.deployment_id.is_some() {
            where_clauses.push(format!("deployment_id = ${}", param_index));
            *param_index += 1;
        }
        if filters.host.is_some() {
            where_clauses.push(format!("host = ${}", param_index));
            *param_index += 1;
        }
        if filters.status_code.is_some() {
            where_clauses.push(format!("status_code = ${}", param_index));
            *param_index += 1;
        }
        if filters.routing_status.is_some() {
            where_clauses.push(format!("routing_status = ${}", param_index));
            *param_index += 1;
        }
        if filters.request_source.is_some() {
            where_clauses.push(format!("request_source = ${}", param_index));
            *param_index += 1;
        }
        if filters.is_bot.is_some() {
            where_clauses.push(format!("is_bot = ${}", param_index));
            *param_index += 1;
        }
        if filters.device_type.is_some() {
            where_clauses.push(format!("device_type = ${}", param_index));
            *param_index += 1;
        }
        String::new()
    }

    fn add_filter_values(values: &mut Vec<sea_orm::Value>, filters: &StatsFilters) {
        if let Some(ref method) = filters.method {
            values.push(method.clone().into());
        }
        if let Some(ref ip) = filters.client_ip {
            values.push(ip.clone().into());
        }
        if let Some(project_id) = filters.project_id {
            values.push(project_id.into());
        }
        if let Some(environment_id) = filters.environment_id {
            values.push(environment_id.into());
        }
        if let Some(deployment_id) = filters.deployment_id {
            values.push(deployment_id.into());
        }
        if let Some(ref host) = filters.host {
            values.push(host.clone().into());
        }
        if let Some(status_code) = filters.status_code {
            values.push(status_code.into());
        }
        if let Some(ref routing_status) = filters.routing_status {
            values.push(routing_status.clone().into());
        }
        if let Some(ref request_source) = filters.request_source {
            values.push(request_source.clone().into());
        }
        if let Some(is_bot) = filters.is_bot {
            values.push(is_bot.into());
        }
        if let Some(ref device_type) = filters.device_type {
            values.push(device_type.clone().into());
        }
    }

    fn is_valid_interval(interval: &str) -> bool {
        // Valid PostgreSQL interval formats
        let valid_units = [
            "microseconds",
            "milliseconds",
            "seconds",
            "minutes",
            "hours",
            "days",
            "weeks",
            "months",
            "years",
            "microsecond",
            "millisecond",
            "second",
            "minute",
            "hour",
            "day",
            "week",
            "month",
            "year",
        ];

        // Split interval into parts (e.g., "1 hour" -> ["1", "hour"])
        let parts: Vec<&str> = interval.split_whitespace().collect();
        if parts.len() != 2 {
            return false;
        }

        // Verify first part is a number
        if parts[0].parse::<u32>().is_err() {
            return false;
        }

        // Verify second part is a valid unit
        valid_units.contains(&parts[1])
    }
}

/// Filters for statistics queries
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, Default)]
pub struct StatsFilters {
    pub method: Option<String>,
    pub client_ip: Option<String>,
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub host: Option<String>,
    pub status_code: Option<i16>,
    pub routing_status: Option<String>,
    pub request_source: Option<String>,
    pub is_bot: Option<bool>,
    pub device_type: Option<String>,
}

/// Time bucket statistics response
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TimeBucketStats {
    /// Bucket timestamp in RFC3339 format
    #[schema(example = "2025-10-23T12:00:00Z")]
    pub bucket: String,
    /// Total number of requests in this bucket
    pub request_count: i64,
    /// Average response time in milliseconds
    pub avg_response_time_ms: f64,
    /// Number of errors (status >= 400)
    pub error_count: i64,
    /// Total request bytes
    pub total_request_bytes: i64,
    /// Total response bytes
    pub total_response_bytes: i64,
}

/// Today's stats response
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TodayStatsResponse {
    /// Total requests today
    pub total_requests: i64,
    /// Date for which stats are returned
    #[schema(example = "2025-10-23")]
    pub date: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_interval_valid_formats() {
        // Valid intervals with common time units (singular)
        assert!(ProxyLogService::is_valid_interval("1 hour"));
        assert!(ProxyLogService::is_valid_interval("1 day"));
        assert!(ProxyLogService::is_valid_interval("1 minute"));
        assert!(ProxyLogService::is_valid_interval("1 second"));
        assert!(ProxyLogService::is_valid_interval("1 week"));
        assert!(ProxyLogService::is_valid_interval("1 month"));
        assert!(ProxyLogService::is_valid_interval("1 year"));

        // Valid intervals with plural forms
        assert!(ProxyLogService::is_valid_interval("5 hours"));
        assert!(ProxyLogService::is_valid_interval("7 days"));
        assert!(ProxyLogService::is_valid_interval("10 minutes"));
        assert!(ProxyLogService::is_valid_interval("30 seconds"));
        assert!(ProxyLogService::is_valid_interval("2 weeks"));
        assert!(ProxyLogService::is_valid_interval("3 months"));
        assert!(ProxyLogService::is_valid_interval("2 years"));

        // Valid intervals with microseconds and milliseconds
        assert!(ProxyLogService::is_valid_interval("1 microsecond"));
        assert!(ProxyLogService::is_valid_interval("1 millisecond"));
        assert!(ProxyLogService::is_valid_interval("100 microseconds"));
        assert!(ProxyLogService::is_valid_interval("500 milliseconds"));

        // Valid intervals with large numbers
        assert!(ProxyLogService::is_valid_interval("100 hours"));
        assert!(ProxyLogService::is_valid_interval("365 days"));
    }

    #[test]
    fn test_is_valid_interval_invalid_formats() {
        // Invalid: wrong number of parts
        assert!(!ProxyLogService::is_valid_interval("1"));
        assert!(!ProxyLogService::is_valid_interval("hour"));
        assert!(!ProxyLogService::is_valid_interval("1 2 hours"));
        assert!(!ProxyLogService::is_valid_interval(""));

        // Invalid: non-numeric value
        assert!(!ProxyLogService::is_valid_interval("one hour"));
        assert!(!ProxyLogService::is_valid_interval("x hours"));
        assert!(!ProxyLogService::is_valid_interval("1.5 hours"));

        // Invalid: unknown time unit
        assert!(!ProxyLogService::is_valid_interval("1 fortnight"));
        assert!(!ProxyLogService::is_valid_interval("1 decade"));
        assert!(!ProxyLogService::is_valid_interval("1 century"));
        assert!(!ProxyLogService::is_valid_interval("1 unknown"));

        // Invalid: special characters
        assert!(!ProxyLogService::is_valid_interval("1; DROP TABLE"));
        assert!(!ProxyLogService::is_valid_interval("1' OR '1'='1"));
    }

    #[test]
    fn test_build_filter_sql_no_filters() {
        let filters = StatsFilters::default();
        let mut where_clauses = Vec::new();
        let mut param_index = 1;

        ProxyLogService::build_filter_sql(&filters, &mut param_index, &mut where_clauses);

        // No filters means no WHERE clauses added
        assert_eq!(where_clauses.len(), 0);
        assert_eq!(param_index, 1); // Parameter index unchanged
    }

    #[test]
    fn test_build_filter_sql_single_filter() {
        let filters = StatsFilters {
            method: Some("GET".to_string()),
            ..Default::default()
        };
        let mut where_clauses = Vec::new();
        let mut param_index = 1;

        ProxyLogService::build_filter_sql(&filters, &mut param_index, &mut where_clauses);

        assert_eq!(where_clauses.len(), 1);
        assert_eq!(where_clauses[0], "method = $1");
        assert_eq!(param_index, 2); // Incremented by 1
    }

    #[test]
    fn test_build_filter_sql_multiple_filters() {
        let filters = StatsFilters {
            method: Some("POST".to_string()),
            client_ip: Some("127.0.0.1".to_string()),
            project_id: Some(123),
            status_code: Some(200),
            ..Default::default()
        };
        let mut where_clauses = Vec::new();
        let mut param_index = 3; // Start at 3 (after start_time and end_time)

        ProxyLogService::build_filter_sql(&filters, &mut param_index, &mut where_clauses);

        assert_eq!(where_clauses.len(), 4);
        assert!(where_clauses.contains(&"method = $3".to_string()));
        assert!(where_clauses.contains(&"client_ip = $4".to_string()));
        assert!(where_clauses.contains(&"project_id = $5".to_string()));
        assert!(where_clauses.contains(&"status_code = $6".to_string()));
        assert_eq!(param_index, 7); // Incremented by 4
    }

    #[test]
    fn test_build_filter_sql_all_filters() {
        let filters = StatsFilters {
            method: Some("GET".to_string()),
            client_ip: Some("192.168.1.1".to_string()),
            project_id: Some(1),
            environment_id: Some(2),
            deployment_id: Some(3),
            host: Some("example.com".to_string()),
            status_code: Some(404),
            routing_status: Some("routed".to_string()),
            request_source: Some("proxy".to_string()),
            is_bot: Some(false),
            device_type: Some("desktop".to_string()),
        };
        let mut where_clauses = Vec::new();
        let mut param_index = 1;

        ProxyLogService::build_filter_sql(&filters, &mut param_index, &mut where_clauses);

        // Should have 11 filters
        assert_eq!(where_clauses.len(), 11);
        assert_eq!(param_index, 12); // Incremented by 11
    }

    #[test]
    fn test_add_filter_values_no_filters() {
        let filters = StatsFilters::default();
        let mut values: Vec<sea_orm::Value> = vec![];

        ProxyLogService::add_filter_values(&mut values, &filters);

        // No filters means no values added
        assert_eq!(values.len(), 0);
    }

    #[test]
    fn test_add_filter_values_single_filter() {
        let filters = StatsFilters {
            method: Some("GET".to_string()),
            ..Default::default()
        };
        let mut values: Vec<sea_orm::Value> = vec![];

        ProxyLogService::add_filter_values(&mut values, &filters);

        assert_eq!(values.len(), 1);
    }

    #[test]
    fn test_add_filter_values_multiple_filters() {
        let filters = StatsFilters {
            method: Some("POST".to_string()),
            client_ip: Some("127.0.0.1".to_string()),
            project_id: Some(123),
            status_code: Some(200),
            ..Default::default()
        };
        let mut values: Vec<sea_orm::Value> = vec![];

        ProxyLogService::add_filter_values(&mut values, &filters);

        assert_eq!(values.len(), 4);
    }

    #[test]
    fn test_add_filter_values_all_filters() {
        let filters = StatsFilters {
            method: Some("GET".to_string()),
            client_ip: Some("192.168.1.1".to_string()),
            project_id: Some(1),
            environment_id: Some(2),
            deployment_id: Some(3),
            host: Some("example.com".to_string()),
            status_code: Some(404),
            routing_status: Some("routed".to_string()),
            request_source: Some("proxy".to_string()),
            is_bot: Some(false),
            device_type: Some("desktop".to_string()),
        };
        let mut values: Vec<sea_orm::Value> = vec![];

        ProxyLogService::add_filter_values(&mut values, &filters);

        // Should have 11 values
        assert_eq!(values.len(), 11);
    }

    #[test]
    fn test_filter_values_and_sql_consistency() {
        // This test ensures that build_filter_sql and add_filter_values
        // maintain the same order and count
        let filters = StatsFilters {
            method: Some("POST".to_string()),
            project_id: Some(100),
            status_code: Some(500),
            is_bot: Some(true),
            ..Default::default()
        };

        let mut where_clauses = Vec::new();
        let mut param_index = 1;
        ProxyLogService::build_filter_sql(&filters, &mut param_index, &mut where_clauses);

        let mut values: Vec<sea_orm::Value> = vec![];
        ProxyLogService::add_filter_values(&mut values, &filters);

        // Number of WHERE clauses should match number of values
        assert_eq!(where_clauses.len(), values.len());
        assert_eq!(where_clauses.len(), 4);
    }

    #[test]
    fn test_stats_filters_default() {
        let filters = StatsFilters::default();

        assert!(filters.method.is_none());
        assert!(filters.client_ip.is_none());
        assert!(filters.project_id.is_none());
        assert!(filters.environment_id.is_none());
        assert!(filters.deployment_id.is_none());
        assert!(filters.host.is_none());
        assert!(filters.status_code.is_none());
        assert!(filters.routing_status.is_none());
        assert!(filters.request_source.is_none());
        assert!(filters.is_bot.is_none());
        assert!(filters.device_type.is_none());
    }

    // Note: Integration tests that require a database connection should be added
    // to test get_today_count and get_time_bucket_stats methods.
    // These would need a test database setup with sample data.
}
