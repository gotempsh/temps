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

    /// Get proxy logs with comprehensive filters and pagination
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
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests would go here - similar to request_log_service tests
}
