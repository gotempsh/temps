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

    /// A ClickHouse operation failed. `operation` names the storage method
    /// (e.g. `list_with_filters`, `write_batch`) so logs/responses can identify
    /// exactly which read/write path hit the backend error. Not a `#[from]` of
    /// `clickhouse::error::Error` because we always want the operation context.
    #[error("ClickHouse error during {operation}: {reason}")]
    ClickHouse { operation: String, reason: String },
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
    /// Visitor UUID from the stateless cookie codec; used by the batch writer
    /// to resolve the UUID → i32 FK at flush time via the moka cache.
    pub visitor_uuid: Option<String>,
    /// Session UUID from the stateless cookie codec; used analogously.
    pub session_uuid: Option<String>,
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
    /// W3C `traceparent` trace_id (32 hex chars) extracted from inbound
    /// headers. `None` when the client didn't send `traceparent`. Lets the
    /// unified Observe view join this row with child OTel spans, runtime
    /// log lines, and any captured exceptions for the same trace.
    pub trace_id: Option<String>,
    /// Set by the deployment runtime (or async stamping) when this request
    /// produced a captured exception. Lets the Observe row deep-link to the
    /// error group without an extra query.
    pub error_group_id: Option<i32>,
}

pub struct ProxyLogService {
    db: Arc<DatabaseConnection>,
    ip_service: Arc<temps_geo::IpAddressService>,
    /// Optional alternative storage backend. When `Some`, every read method that
    /// the HTTP handlers call dispatches to this trait object (the ClickHouse
    /// backend) instead of running the inline TimescaleDB query. When `None`
    /// (the default), the inline TimescaleDB logic runs unchanged —
    /// byte-for-byte identical to the prior behaviour.
    ///
    /// IMPORTANT: this is ALWAYS `None` for the `ProxyLogService` that the
    /// [`crate::storage::TimescaleDbProxyLogStore`] holds internally as its read
    /// relay, so the TimescaleDB trait impl never recurses back into a dispatch.
    storage: Option<Arc<dyn crate::storage::ProxyLogStorage>>,
}

impl ProxyLogService {
    /// Construct a service that talks directly to TimescaleDB (the default).
    pub fn new(db: Arc<DatabaseConnection>, ip_service: Arc<temps_geo::IpAddressService>) -> Self {
        Self {
            db,
            ip_service,
            storage: None,
        }
    }

    /// Construct a service that dispatches every read through the supplied
    /// storage backend (used to serve handlers from ClickHouse when
    /// `TEMPS_CLICKHOUSE_*` is configured). The `db` / `ip_service` are still
    /// held so any non-dispatched path (e.g. `create`) keeps working, but the
    /// list / lookup / stats methods delegate to `storage`.
    pub fn with_storage(
        db: Arc<DatabaseConnection>,
        ip_service: Arc<temps_geo::IpAddressService>,
        storage: Arc<dyn crate::storage::ProxyLogStorage>,
    ) -> Self {
        Self {
            db,
            ip_service,
            storage: Some(storage),
        }
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

        // Detect bots/crawlers if not already detected. AI-agent detection runs
        // first so the canonical agent name (e.g. `GPTBot`, `ClaudeBot`) wins
        // over the looser substring `CrawlerDetector` would otherwise return.
        // That keeps `GROUP BY bot_name` aggregations stable on the analytics
        // side and avoids a second `user_agent ILIKE` scan per request.
        if request.is_bot.is_none() {
            if let Some(ref ua_string) = request.user_agent {
                if let Some(ai) = crate::ai_agent_detector::detect(Some(ua_string)) {
                    request.is_bot = Some(true);
                    request.bot_name = Some(ai.agent.to_string());
                } else {
                    let crawler_name =
                        crate::crawler_detector::CrawlerDetector::get_crawler_name(Some(ua_string));
                    request.is_bot = Some(crawler_name.is_some());
                    request.bot_name = crawler_name;
                }
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
            trace_id: Set(request.trace_id),
            error_group_id: Set(request.error_group_id),
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
        if let Some(storage) = &self.storage {
            return storage
                .list_with_filters(start_date, end_date, filters, page, page_size)
                .await;
        }

        let mut query = proxy_logs::Entity::find();

        // Whether any narrowing predicate is set. When nothing is filtered,
        // the pagination total is just the whole-table row count, which on
        // this hypertable we can read from planner stats in microseconds via
        // `approximate_row_count` instead of scanning every chunk with
        // `COUNT(*)`. Sort/pagination fields do not narrow the result set, so
        // they are excluded. Computed up front, before the blocks below
        // consume the owned filter fields.
        let has_filters = start_date.is_some()
            || end_date.is_some()
            || filters.project_id.is_some()
            || filters.environment_id.is_some()
            || filters.deployment_id.is_some()
            || filters.session_id.is_some()
            || filters.visitor_id.is_some()
            || filters.method.is_some()
            || filters.host.is_some()
            || filters.path.is_some()
            || filters.client_ip.is_some()
            || filters.status_code.is_some()
            || filters.response_time_min.is_some()
            || filters.response_time_max.is_some()
            || filters.routing_status.is_some()
            || filters.request_source.is_some()
            || filters.is_system_request.is_some()
            || filters.user_agent.is_some()
            || filters.browser.is_some()
            || filters.operating_system.is_some()
            || filters.device_type.is_some()
            || filters.is_bot.is_some()
            || filters.bot_name.is_some()
            || filters.ai_provider.is_some()
            || filters.ai_agent.is_some()
            || filters.is_ai_agent.is_some()
            || filters.request_size_min.is_some()
            || filters.request_size_max.is_some()
            || filters.response_size_min.is_some()
            || filters.response_size_max.is_some()
            || filters.cache_status.is_some()
            || filters.container_id.is_some()
            || filters.upstream_host.is_some()
            || filters.has_error.is_some();

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

        // AI agent filters use the canonical agent names persisted at ingest
        // time. `bot_name` was set by `ai_agent_detector::detect`, so equality
        // matches are sufficient — no SQL regex needed.
        if let Some(agent) = filters.ai_agent {
            query = query
                .filter(proxy_logs::Column::IsBot.eq(true))
                .filter(proxy_logs::Column::BotName.eq(agent));
        }
        if let Some(provider) = filters.ai_provider {
            let agents_for_provider: Vec<String> = crate::ai_agent_detector::known_agents()
                .iter()
                .filter(|(_, m)| m.provider.eq_ignore_ascii_case(&provider))
                .map(|(_, m)| m.agent.to_string())
                .collect();
            if agents_for_provider.is_empty() {
                // Unknown provider — return no rows rather than the entire table.
                query = query.filter(proxy_logs::Column::Id.eq(-1));
            } else {
                query = query
                    .filter(proxy_logs::Column::IsBot.eq(true))
                    .filter(proxy_logs::Column::BotName.is_in(agents_for_provider));
            }
        }
        if let Some(true) = filters.is_ai_agent {
            let known: Vec<String> = crate::ai_agent_detector::known_agents()
                .iter()
                .map(|(_, m)| m.agent.to_string())
                .collect();
            query = query
                .filter(proxy_logs::Column::IsBot.eq(true))
                .filter(proxy_logs::Column::BotName.is_in(known));
        } else if let Some(false) = filters.is_ai_agent {
            let known: Vec<String> = crate::ai_agent_detector::known_agents()
                .iter()
                .map(|(_, m)| m.agent.to_string())
                .collect();
            query = query.filter(proxy_logs::Column::BotName.is_not_in(known));
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
        let (total, _) = temps_database::count_for_pagination(
            self.db.as_ref(),
            "proxy_logs",
            has_filters,
            || async { paginator.num_items().await },
        )
        .await?;
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
            ai_provider: None,
            ai_agent: None,
            is_ai_agent: None,
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

    /// Get a single proxy log by ID.
    ///
    /// `proxy_logs` is a TimescaleDB hypertable partitioned by `timestamp`
    /// (1-day chunks, compressed after 7 days), so a bare `WHERE id = $1`
    /// cannot exclude any chunk and must decompress every compressed chunk —
    /// observed as multi-second lookups. When the caller knows the row's event
    /// time (the list endpoint returns it), a ±1-day bound reduces the lookup
    /// to the couple of chunks that can contain the row. Without a timestamp,
    /// the recent uncompressed window is probed first and the unbounded scan
    /// only runs as a last resort so bare deep-links keep resolving.
    pub async fn get_by_id(
        &self,
        id: i32,
        timestamp: Option<UtcDateTime>,
    ) -> Result<Option<proxy_logs::Model>, ProxyLogServiceError> {
        if let Some(storage) = &self.storage {
            return storage.get_by_id(id, timestamp).await;
        }

        if let Some(ts) = timestamp {
            let log = proxy_logs::Entity::find()
                .filter(proxy_logs::Column::Id.eq(id))
                .filter(proxy_logs::Column::Timestamp.gte(ts - chrono::Duration::days(1)))
                .filter(proxy_logs::Column::Timestamp.lte(ts + chrono::Duration::days(1)))
                .one(self.db.as_ref())
                .await?;
            return Ok(log);
        }

        // Compression policy compresses chunks older than 7 days; probing the
        // uncompressed window first keeps the common case (recent log, no
        // timestamp supplied) off the decompression path.
        let recent_cutoff = Utc::now() - chrono::Duration::days(7);
        let log = proxy_logs::Entity::find()
            .filter(proxy_logs::Column::Id.eq(id))
            .filter(proxy_logs::Column::Timestamp.gte(recent_cutoff))
            .one(self.db.as_ref())
            .await?;
        if log.is_some() {
            return Ok(log);
        }

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
        if let Some(storage) = &self.storage {
            return storage.get_by_request_id(request_id).await;
        }
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
        if let Some(storage) = &self.storage {
            return storage.get_today_count(filters).await;
        }
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
        if let Some(storage) = &self.storage {
            return storage
                .get_time_bucket_stats(start_time, end_time, bucket_interval, filters)
                .await;
        }
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

        // Pass bucket_interval as a parameterized value to prevent SQL injection
        let bucket_param_index = param_index;

        let sql = format!(
            r#"
            SELECT
                bucket::timestamptz as bucket,
                COALESCE(count, 0) as request_count,
                COALESCE(avg_response_time, 0)::float8 as avg_response_time_ms,
                COALESCE(error_count, 0) as error_count,
                COALESCE(total_request_bytes, 0) as total_request_bytes,
                COALESCE(total_response_bytes, 0) as total_response_bytes
            FROM (
                SELECT
                    time_bucket_gapfill(${}::interval, timestamp) AS bucket,
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
            bucket_param_index, where_clause
        );

        // Execute raw SQL query
        let db_backend = sea_orm::DatabaseBackend::Postgres;

        // Build values vec for parameterized query
        let mut values: Vec<sea_orm::Value> = vec![start_time.into(), end_time.into()];

        // Add filter values
        if let Some(ref f) = filters {
            Self::add_filter_values(&mut values, f);
        }

        // Add bucket_interval as parameterized value
        values.push(bucket_interval.into());

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

    /// Get health summaries for multiple projects in a single query
    pub async fn get_projects_health_summary(
        &self,
        project_ids: &[i32],
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        is_bot: Option<bool>,
    ) -> Result<Vec<ProjectHealthSummary>, ProxyLogServiceError> {
        if let Some(storage) = &self.storage {
            return storage
                .get_projects_health_summary(project_ids, start_time, end_time, is_bot)
                .await;
        }
        if project_ids.is_empty() {
            return Ok(vec![]);
        }

        // Build placeholders for project IDs ($3, $4, $5, ...)
        let placeholders: Vec<String> = project_ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("${}", i + 3))
            .collect();
        let placeholders_str = placeholders.join(", ");

        let (bot_clause, bot_value) = match is_bot {
            Some(flag) => (
                format!(" AND is_bot = ${}", project_ids.len() + 3),
                Some(flag),
            ),
            None => (String::new(), None),
        };

        let sql = format!(
            r#"
            SELECT
                project_id,
                COALESCE(COUNT(*), 0) as total_requests,
                COALESCE(SUM(CASE WHEN status_code >= 500 THEN 1 ELSE 0 END), 0) as total_errors,
                COALESCE(AVG(response_time_ms)::float8, 0) as avg_response_time_ms
            FROM proxy_logs
            WHERE timestamp >= $1
              AND timestamp < $2
              AND project_id IN ({}){}
            GROUP BY project_id
            "#,
            placeholders_str, bot_clause
        );

        let db_backend = sea_orm::DatabaseBackend::Postgres;
        let mut values: Vec<sea_orm::Value> = vec![start_time.into(), end_time.into()];
        for &id in project_ids {
            values.push(id.into());
        }
        if let Some(flag) = bot_value {
            values.push(flag.into());
        }

        let stmt = sea_orm::Statement::from_sql_and_values(db_backend, &sql, values);
        let results = self.db.query_all(stmt).await?;

        // Build a map from query results
        let mut summaries: std::collections::HashMap<i32, ProjectHealthSummary> =
            std::collections::HashMap::new();

        for row in &results {
            let project_id: i32 = row.try_get("", "project_id").unwrap_or(0);
            let total_requests: i64 = row.try_get("", "total_requests").unwrap_or(0);
            let total_errors: i64 = row.try_get("", "total_errors").unwrap_or(0);
            let avg_response_time_ms: f64 = row.try_get("", "avg_response_time_ms").unwrap_or(0.0);

            let error_rate = if total_requests > 0 {
                (total_errors as f64 / total_requests as f64) * 100.0
            } else {
                0.0
            };

            let status = if total_requests == 0 {
                "unknown".to_string()
            } else if error_rate > 50.0 {
                "down".to_string()
            } else if error_rate > 10.0 {
                "degraded".to_string()
            } else {
                "healthy".to_string()
            };

            summaries.insert(
                project_id,
                ProjectHealthSummary {
                    project_id,
                    total_requests,
                    total_errors,
                    avg_response_time_ms: (avg_response_time_ms * 10.0).round() / 10.0,
                    error_rate: (error_rate * 10.0).round() / 10.0,
                    status,
                },
            );
        }

        // Include projects with no data as "unknown"
        let result: Vec<ProjectHealthSummary> = project_ids
            .iter()
            .map(|&id| {
                summaries.remove(&id).unwrap_or(ProjectHealthSummary {
                    project_id: id,
                    total_requests: 0,
                    total_errors: 0,
                    avg_response_time_ms: 0.0,
                    error_rate: 0.0,
                    status: "unknown".to_string(),
                })
            })
            .collect();

        Ok(result)
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
        if let Some(ref class) = filters.status_code_class {
            if let Some((min, max)) = Self::status_class_range(class) {
                query = query.filter(proxy_logs::Column::StatusCode.gte(min));
                query = query.filter(proxy_logs::Column::StatusCode.lt(max));
            }
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
        if let Some(ref class) = filters.status_code_class {
            if let Some((min, max)) = Self::status_class_range(class) {
                where_clauses.push(format!(
                    "status_code >= ${} AND status_code < ${}",
                    param_index,
                    *param_index + 1
                ));
                *param_index += 2;
                let _ = (min, max); // used via add_filter_values
            }
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
        if let Some(has_project) = filters.has_project {
            where_clauses.push(if has_project {
                "project_id IS NOT NULL".to_string()
            } else {
                "project_id IS NULL".to_string()
            });
            // no parameterized value — the predicate is fully in SQL
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
        if let Some(ref class) = filters.status_code_class {
            if let Some((min, max)) = Self::status_class_range(class) {
                values.push(min.into());
                values.push(max.into());
            }
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

    pub(crate) fn is_valid_interval(interval: &str) -> bool {
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

    /// Aggregate AI-agent traffic for a project over a time window.
    ///
    /// `bot_name` is the canonical agent name written at ingest time by
    /// [`crate::ai_agent_detector::detect`], so this is a cheap `GROUP BY`
    /// scoped to rows the detector already classified. Each row is mapped back
    /// to its provider in Rust so the response carries the logo-ready
    /// `(provider, agent, count)` triple the UI needs.
    pub async fn get_ai_agent_breakdown(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        path: Option<String>,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        limit: u64,
    ) -> Result<Vec<AiAgentBreakdownRow>, ProxyLogServiceError> {
        if let Some(storage) = &self.storage {
            return storage
                .get_ai_agent_breakdown(
                    project_id,
                    environment_id,
                    path,
                    start_time,
                    end_time,
                    limit,
                )
                .await;
        }
        let known: Vec<&str> = crate::ai_agent_detector::known_agents()
            .iter()
            .map(|(_, m)| m.agent)
            .collect();

        if known.is_empty() {
            return Ok(vec![]);
        }

        // Pre-filter with `is_bot = true AND bot_name IN (...)` so the planner
        // can use the existing (timestamp DESC) index on the hypertable instead
        // of scanning everything.
        let mut where_clauses: Vec<String> = vec![
            "timestamp >= $1".to_string(),
            "timestamp < $2".to_string(),
            "is_bot = true".to_string(),
            "bot_name IS NOT NULL".to_string(),
        ];
        let mut values: Vec<sea_orm::Value> = vec![start_time.into(), end_time.into()];
        let mut next_idx = 3i32;

        if let Some(pid) = project_id {
            where_clauses.push(format!("project_id = ${}", next_idx));
            values.push(pid.into());
            next_idx += 1;
        }
        if let Some(eid) = environment_id {
            where_clauses.push(format!("environment_id = ${}", next_idx));
            values.push(eid.into());
            next_idx += 1;
        }
        // Exact-path filter so the UI can drill a single page row down into the
        // per-agent counts that crawled THAT page.
        if let Some(ref p) = path {
            where_clauses.push(format!("path = ${}", next_idx));
            values.push(p.clone().into());
            next_idx += 1;
        }

        // Bind known agent names as an `ANY($N::text[])` so the SQL is stable
        // even as we grow the taxonomy. The list comes from
        // `ai_agent_detector::known_agents`, so it cannot contain anything
        // user-supplied.
        where_clauses.push(format!("bot_name = ANY(${}::text[])", next_idx));
        let agents_owned: Vec<String> = known.iter().map(|s| (*s).to_owned()).collect();
        values.push(agents_owned.into());

        let sql = format!(
            r#"
            SELECT bot_name,
                   COUNT(*)::bigint AS request_count,
                   COUNT(DISTINCT client_ip)::bigint AS unique_ips,
                   MAX(timestamp)::timestamptz AS last_seen
            FROM proxy_logs
            WHERE {where}
            GROUP BY bot_name
            ORDER BY request_count DESC
            LIMIT {limit}
            "#,
            where = where_clauses.join(" AND "),
            limit = std::cmp::min(limit, 100),
        );

        let stmt = sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            &sql,
            values,
        );
        let results = self.db.query_all(stmt).await?;

        // Map agent name -> (provider, purpose) once so we don't walk the
        // taxonomy for every row.
        let agent_index: std::collections::HashMap<
            &'static str,
            &'static crate::ai_agent_detector::AiAgentMatch,
        > = crate::ai_agent_detector::known_agents()
            .iter()
            .map(|(_, m)| (m.agent, m))
            .collect();

        let rows = results
            .into_iter()
            .filter_map(|row| {
                let agent: String = row.try_get("", "bot_name").ok()?;
                let meta = agent_index.get(agent.as_str())?;
                Some(AiAgentBreakdownRow {
                    provider: meta.provider.to_string(),
                    agent: meta.agent.to_string(),
                    purpose: meta.purpose.as_str().to_string(),
                    request_count: row.try_get("", "request_count").unwrap_or(0),
                    unique_ips: row.try_get("", "unique_ips").unwrap_or(0),
                    last_seen: row
                        .try_get::<chrono::DateTime<Utc>>("", "last_seen")
                        .ok()
                        .map(|d| d.to_rfc3339()),
                })
            })
            .collect();

        Ok(rows)
    }

    /// Aggregate which pages AI agents crawled most over a time window.
    ///
    /// Same pre-filter as [`Self::get_ai_agent_breakdown`] (`is_bot = true AND
    /// bot_name IN (known)`), but grouped by `path`. Each row carries the total
    /// request count, the number of *distinct* AI agents that hit the page, and
    /// the last time any agent touched it — so the UI can answer "what content
    /// are the bots most interested in, and how broadly?".
    pub async fn get_ai_page_breakdown(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        path: Option<String>,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        limit: u64,
    ) -> Result<Vec<AiPageBreakdownRow>, ProxyLogServiceError> {
        if let Some(storage) = &self.storage {
            return storage
                .get_ai_page_breakdown(
                    project_id,
                    environment_id,
                    path,
                    start_time,
                    end_time,
                    limit,
                )
                .await;
        }
        let known: Vec<&str> = crate::ai_agent_detector::known_agents()
            .iter()
            .map(|(_, m)| m.agent)
            .collect();

        if known.is_empty() {
            return Ok(vec![]);
        }

        let mut where_clauses: Vec<String> = vec![
            "timestamp >= $1".to_string(),
            "timestamp < $2".to_string(),
            "is_bot = true".to_string(),
            "bot_name IS NOT NULL".to_string(),
        ];
        let mut values: Vec<sea_orm::Value> = vec![start_time.into(), end_time.into()];
        let mut next_idx = 3i32;

        if let Some(pid) = project_id {
            where_clauses.push(format!("project_id = ${}", next_idx));
            values.push(pid.into());
            next_idx += 1;
        }
        if let Some(eid) = environment_id {
            where_clauses.push(format!("environment_id = ${}", next_idx));
            values.push(eid.into());
            next_idx += 1;
        }
        // Exact-path filter so callers can ask "how many AI agents hit THIS
        // page?" and get a single precise row regardless of `limit`.
        if let Some(ref p) = path {
            where_clauses.push(format!("path = ${}", next_idx));
            values.push(p.clone().into());
            next_idx += 1;
        }

        where_clauses.push(format!("bot_name = ANY(${}::text[])", next_idx));
        let agents_owned: Vec<String> = known.iter().map(|s| (*s).to_owned()).collect();
        values.push(agents_owned.into());

        let sql = format!(
            r#"
            SELECT path,
                   COUNT(*)::bigint AS request_count,
                   COUNT(DISTINCT bot_name)::bigint AS agent_count,
                   MAX(timestamp)::timestamptz AS last_seen
            FROM proxy_logs
            WHERE {where}
            GROUP BY path
            ORDER BY request_count DESC
            LIMIT {limit}
            "#,
            where = where_clauses.join(" AND "),
            limit = std::cmp::min(limit, 100),
        );

        let stmt = sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            &sql,
            values,
        );
        let results = self.db.query_all(stmt).await?;

        let rows = results
            .into_iter()
            .map(|row| AiPageBreakdownRow {
                path: row.try_get("", "path").unwrap_or_default(),
                request_count: row.try_get("", "request_count").unwrap_or(0),
                agent_count: row.try_get("", "agent_count").unwrap_or(0),
                last_seen: row
                    .try_get::<chrono::DateTime<Utc>>("", "last_seen")
                    .ok()
                    .map(|d| d.to_rfc3339()),
            })
            .collect();

        Ok(rows)
    }

    /// Pages breakdown for a *single* AI agent over a time window.
    ///
    /// Groups `proxy_logs` by `path` but pre-filters to one canonical
    /// `bot_name` (e.g. `"ChatGPT-User"`), so the caller can answer "which
    /// pages did ChatGPT users visit, and how many times?". The agent name is
    /// validated against `ai_agent_detector::known_agents` so unknown strings
    /// return an empty result rather than a table scan.
    pub async fn get_ai_agent_pages(
        &self,
        agent: &str,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        limit: u64,
    ) -> Result<Vec<AiAgentPageRow>, ProxyLogServiceError> {
        let known = crate::ai_agent_detector::known_agents();
        if !known.iter().any(|(_, m)| m.agent == agent) {
            return Ok(vec![]);
        }

        let mut where_clauses: Vec<String> = vec![
            "timestamp >= $1".to_string(),
            "timestamp < $2".to_string(),
            "is_bot = true".to_string(),
            "bot_name = $3".to_string(),
        ];
        let mut values: Vec<sea_orm::Value> =
            vec![start_time.into(), end_time.into(), agent.to_owned().into()];
        let mut next_idx = 4i32;

        if let Some(pid) = project_id {
            where_clauses.push(format!("project_id = ${}", next_idx));
            values.push(pid.into());
            next_idx += 1;
        }
        if let Some(eid) = environment_id {
            where_clauses.push(format!("environment_id = ${}", next_idx));
            values.push(eid.into());
            next_idx += 1;
        }
        let _ = next_idx;

        let sql = format!(
            r#"
            SELECT path,
                   COUNT(*)::bigint AS request_count,
                   COUNT(DISTINCT client_ip)::bigint AS unique_ips,
                   MAX(timestamp)::timestamptz AS last_seen
            FROM proxy_logs
            WHERE {where}
            GROUP BY path
            ORDER BY request_count DESC
            LIMIT {limit}
            "#,
            where = where_clauses.join(" AND "),
            limit = std::cmp::min(limit, 100),
        );

        let stmt = sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            &sql,
            values,
        );
        let results = self.db.query_all(stmt).await?;

        let rows = results
            .into_iter()
            .map(|row| AiAgentPageRow {
                path: row.try_get("", "path").unwrap_or_default(),
                request_count: row.try_get("", "request_count").unwrap_or(0),
                unique_ips: row.try_get("", "unique_ips").unwrap_or(0),
                last_seen: row
                    .try_get::<chrono::DateTime<Utc>>("", "last_seen")
                    .ok()
                    .map(|d| d.to_rfc3339()),
            })
            .collect();

        Ok(rows)
    }

    /// Time-bucketed AI-agent request counts, split by provider or by agent.
    ///
    /// Same pre-filter as [`Self::get_ai_agent_breakdown`] (`is_bot = true AND
    /// bot_name IN (known)`) so the planner uses the `(timestamp DESC)` index,
    /// but the result is grouped by `time_bucket(interval, timestamp)` and the
    /// raw `bot_name`. The caller passes the bucket interval (validated against
    /// the same allowlist as [`Self::get_time_bucket_stats`]) and chooses the
    /// grouping dimension via `group_by` (`"provider"` or `"agent"`); provider
    /// roll-up is done in Rust off the agent taxonomy so the SQL stays a single
    /// stable shape. Each row is `(bucket, key, count)` — the UI pivots these
    /// into one stacked series per key, mirroring the traffic time-series chart.
    pub async fn get_ai_agent_timeline(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        bucket_interval: String,
        group_by: AiTimelineGroupBy,
    ) -> Result<Vec<AiAgentTimelineRow>, ProxyLogServiceError> {
        if let Some(storage) = &self.storage {
            return storage
                .get_ai_agent_timeline(
                    project_id,
                    environment_id,
                    start_time,
                    end_time,
                    bucket_interval,
                    group_by,
                )
                .await;
        }
        if !Self::is_valid_interval(&bucket_interval) {
            return Err(ProxyLogServiceError::InvalidFilter(format!(
                "Invalid bucket interval: {}",
                bucket_interval
            )));
        }

        let known: Vec<&str> = crate::ai_agent_detector::known_agents()
            .iter()
            .map(|(_, m)| m.agent)
            .collect();

        if known.is_empty() {
            return Ok(vec![]);
        }

        let mut where_clauses: Vec<String> = vec![
            "timestamp >= $1".to_string(),
            "timestamp < $2".to_string(),
            "is_bot = true".to_string(),
            "bot_name IS NOT NULL".to_string(),
        ];
        let mut values: Vec<sea_orm::Value> = vec![start_time.into(), end_time.into()];
        let mut next_idx = 3i32;

        if let Some(pid) = project_id {
            where_clauses.push(format!("project_id = ${}", next_idx));
            values.push(pid.into());
            next_idx += 1;
        }
        if let Some(eid) = environment_id {
            where_clauses.push(format!("environment_id = ${}", next_idx));
            values.push(eid.into());
            next_idx += 1;
        }

        let agents_param_idx = next_idx;
        where_clauses.push(format!("bot_name = ANY(${}::text[])", agents_param_idx));
        let agents_owned: Vec<String> = known.iter().map(|s| (*s).to_owned()).collect();
        values.push(agents_owned.into());
        next_idx += 1;

        // The bucket interval is bound next, then the window bounds.
        // We build the full bucket spine with `generate_series` and LEFT JOIN
        // the real per-agent counts onto it (the same approach the analytics
        // hourly-visits query uses). We deliberately do NOT use
        // `time_bucket_gapfill` here: it fails to fill gaps when the data spans
        // only a single bucket, which is exactly the edge case we hit. The
        // `generate_series` spine guarantees EVERY bucket in the window is
        // present regardless. The spine yields one row per
        // (bucket, agent-with-data); empty buckets appear once with a NULL agent
        // and zero count, which the frontend uses purely to keep the x-axis
        // continuous. Payload stays lean: it's (buckets-with-data × agents) +
        // (empty buckets × 1), not buckets × all known agents.
        let bucket_param_idx = next_idx;
        values.push(bucket_interval.into());
        next_idx += 1;
        let gap_start_idx = next_idx;
        values.push(start_time.into());
        next_idx += 1;
        let gap_end_idx = next_idx;
        values.push(end_time.into());

        let sql = format!(
            r#"
            WITH spine AS (
                SELECT generate_series(
                    time_bucket(${bucket}::interval, ${gstart}::timestamptz),
                    time_bucket(${bucket}::interval, ${gend}::timestamptz),
                    ${bucket}::interval
                ) AS bucket
            ),
            data AS (
                SELECT
                    time_bucket(${bucket}::interval, timestamp) AS bucket,
                    bot_name AS agent,
                    COUNT(*)::bigint AS request_count
                FROM proxy_logs
                WHERE {where}
                GROUP BY bucket, bot_name
            )
            SELECT
                s.bucket AS bucket,
                d.agent AS agent,
                COALESCE(d.request_count, 0)::bigint AS request_count
            FROM spine s
            LEFT JOIN data d ON d.bucket = s.bucket
            ORDER BY s.bucket ASC
            "#,
            bucket = bucket_param_idx,
            gstart = gap_start_idx,
            gend = gap_end_idx,
            where = where_clauses.join(" AND "),
        );

        let stmt = sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            &sql,
            values,
        );
        let results = self.db.query_all(stmt).await?;

        // Map agent -> provider once so provider roll-up doesn't re-walk the
        // taxonomy per row.
        let agent_index: std::collections::HashMap<
            &'static str,
            &'static crate::ai_agent_detector::AiAgentMatch,
        > = crate::ai_agent_detector::known_agents()
            .iter()
            .map(|(_, m)| (m.agent, m))
            .collect();

        // Aggregate into (bucket, key) buckets. When grouping by provider we sum
        // every agent that maps to the same provider within a bucket. We also
        // record every spine bucket — including the LEFT-JOIN empty ones whose
        // agent is NULL — so the result carries the full x-axis and the chart
        // doesn't collapse gaps.
        let mut acc: std::collections::HashMap<(String, String), i64> =
            std::collections::HashMap::new();
        let mut all_buckets: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for row in results {
            let bucket: chrono::DateTime<Utc> = match row.try_get("", "bucket") {
                Ok(b) => b,
                Err(_) => continue,
            };
            let bucket_iso = bucket.to_rfc3339();
            all_buckets.insert(bucket_iso.clone());

            // Empty spine buckets have a NULL agent — they contribute to the
            // x-axis but carry no series count.
            let agent: String = match row.try_get::<Option<String>>("", "agent") {
                Ok(Some(a)) => a,
                _ => continue,
            };
            let count: i64 = row.try_get("", "request_count").unwrap_or(0);

            let key = match group_by {
                AiTimelineGroupBy::Agent => agent.clone(),
                AiTimelineGroupBy::Provider => agent_index
                    .get(agent.as_str())
                    .map(|m| m.provider.to_string())
                    .unwrap_or_else(|| agent.clone()),
            };

            *acc.entry((bucket_iso, key)).or_insert(0) += count;
        }

        let mut rows: Vec<AiAgentTimelineRow> = acc
            .into_iter()
            .map(|((bucket, key), count)| AiAgentTimelineRow {
                bucket,
                key,
                request_count: count,
            })
            .collect();

        // Emit a zero-count marker for every bucket that had no AI traffic so the
        // frontend sees the complete time axis. `key` is empty for these; the UI
        // treats an empty key purely as an x-axis placeholder.
        let buckets_with_data: std::collections::HashSet<&str> =
            rows.iter().map(|r| r.bucket.as_str()).collect();
        let empty_markers: Vec<AiAgentTimelineRow> = all_buckets
            .iter()
            .filter(|b| !buckets_with_data.contains(b.as_str()))
            .map(|b| AiAgentTimelineRow {
                bucket: b.clone(),
                key: String::new(),
                request_count: 0,
            })
            .collect();
        rows.extend(empty_markers);

        // Stable ordering: bucket ASC, then key, so the frontend pivot is
        // deterministic and series colours stay put across refreshes.
        rows.sort_by(|a, b| a.bucket.cmp(&b.bucket).then_with(|| a.key.cmp(&b.key)));

        Ok(rows)
    }

    /// HTTP status-class breakdown for AI-agent traffic.
    ///
    /// Same pre-filter as the AI agent breakdown (`is_bot = true AND bot_name IN
    /// (known)`), grouped by status class (`2xx`, `3xx`, `4xx`, `5xx`, `other`).
    /// Surfaces whether crawlers are getting served (`2xx`) or hitting broken /
    /// blocked pages (`4xx`/`5xx`) — a content-health signal the request tables
    /// don't make obvious.
    pub async fn get_ai_status_breakdown(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
    ) -> Result<Vec<AiStatusBreakdownRow>, ProxyLogServiceError> {
        if let Some(storage) = &self.storage {
            return storage
                .get_ai_status_breakdown(project_id, environment_id, start_time, end_time)
                .await;
        }
        let known: Vec<&str> = crate::ai_agent_detector::known_agents()
            .iter()
            .map(|(_, m)| m.agent)
            .collect();

        if known.is_empty() {
            return Ok(vec![]);
        }

        let mut where_clauses: Vec<String> = vec![
            "timestamp >= $1".to_string(),
            "timestamp < $2".to_string(),
            "is_bot = true".to_string(),
            "bot_name IS NOT NULL".to_string(),
        ];
        let mut values: Vec<sea_orm::Value> = vec![start_time.into(), end_time.into()];
        let mut next_idx = 3i32;

        if let Some(pid) = project_id {
            where_clauses.push(format!("project_id = ${}", next_idx));
            values.push(pid.into());
            next_idx += 1;
        }
        if let Some(eid) = environment_id {
            where_clauses.push(format!("environment_id = ${}", next_idx));
            values.push(eid.into());
            next_idx += 1;
        }

        where_clauses.push(format!("bot_name = ANY(${}::text[])", next_idx));
        let agents_owned: Vec<String> = known.iter().map(|s| (*s).to_owned()).collect();
        values.push(agents_owned.into());

        // Bucket the raw status into a class label in SQL so the result is a
        // small fixed set of rows regardless of how many distinct codes appear.
        let sql = format!(
            r#"
            SELECT
                CASE
                    WHEN status_code >= 200 AND status_code < 300 THEN '2xx'
                    WHEN status_code >= 300 AND status_code < 400 THEN '3xx'
                    WHEN status_code >= 400 AND status_code < 500 THEN '4xx'
                    WHEN status_code >= 500 AND status_code < 600 THEN '5xx'
                    ELSE 'other'
                END AS status_class,
                COUNT(*)::bigint AS request_count
            FROM proxy_logs
            WHERE {where}
            GROUP BY status_class
            ORDER BY request_count DESC
            "#,
            where = where_clauses.join(" AND "),
        );

        let stmt = sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            &sql,
            values,
        );
        let results = self.db.query_all(stmt).await?;

        let rows = results
            .into_iter()
            .map(|row| AiStatusBreakdownRow {
                status_class: row.try_get("", "status_class").unwrap_or_default(),
                request_count: row.try_get("", "request_count").unwrap_or(0),
            })
            .collect();

        Ok(rows)
    }

    /// Convert a status code class like "2xx", "3xx", "4xx", "5xx" to a (min, max) range.
    /// Returns None if the class is not recognized.
    fn status_class_range(class: &str) -> Option<(i16, i16)> {
        match class {
            "1xx" => Some((100, 200)),
            "2xx" => Some((200, 300)),
            "3xx" => Some((300, 400)),
            "4xx" => Some((400, 500)),
            "5xx" => Some((500, 600)),
            _ => None,
        }
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
    /// Filter by status code class (e.g. "2xx", "3xx", "4xx", "5xx")
    pub status_code_class: Option<String>,
    pub routing_status: Option<String>,
    pub request_source: Option<String>,
    pub is_bot: Option<bool>,
    pub device_type: Option<String>,
    /// When true, only count requests that matched a project (project_id IS NOT NULL).
    /// Used by the health dashboard so totals match the per-project cards.
    pub has_project: Option<bool>,
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

/// One row in the AI-agent analytics breakdown. `agent` is the canonical
/// crawler name (e.g. `GPTBot`, `Claude-User`), `provider` is the vendor used
/// for grouping + logos. The UI mirrors the browsers card and ranks by
/// `request_count`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AiAgentBreakdownRow {
    pub provider: String,
    pub agent: String,
    pub purpose: String,
    pub request_count: i64,
    pub unique_ips: i64,
    /// Last-seen timestamp in RFC3339 format, or `None` if no rows matched.
    #[schema(example = "2026-05-29T12:00:00Z")]
    pub last_seen: Option<String>,
}

/// One row in the AI-crawled-pages breakdown. `agent_count` is the number of
/// *distinct* AI agents that hit this path, so the UI can show both how heavily
/// and how broadly a page is being crawled.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AiPageBreakdownRow {
    pub path: String,
    pub request_count: i64,
    pub agent_count: i64,
    /// Last-seen timestamp in RFC3339 format, or `None` if no rows matched.
    #[schema(example = "2026-05-29T12:00:00Z")]
    pub last_seen: Option<String>,
}

/// One row in the pages-by-agent breakdown. Returned by
/// [`ProxyLogService::get_ai_agent_pages`] for a single named agent.
/// `unique_ips` counts distinct client IPs that hit this path via that agent
/// (same definition as the per-agent unique-IPs in [`AiAgentBreakdownRow`]).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AiAgentPageRow {
    pub path: String,
    pub request_count: i64,
    pub unique_ips: i64,
    /// Last-seen timestamp in RFC3339 format, or `None` if no rows matched.
    #[schema(example = "2026-05-29T12:00:00Z")]
    pub last_seen: Option<String>,
}

/// Dimension to split the AI-agent timeline by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiTimelineGroupBy {
    /// One series per vendor (OpenAI, Anthropic, Perplexity, …).
    Provider,
    /// One series per canonical agent (GPTBot, ClaudeBot, …).
    Agent,
}

/// One point in the AI-agent timeline: the request count for a single
/// (`bucket`, `key`) pair, where `key` is a provider or agent name depending on
/// the requested grouping. The UI pivots these into one stacked series per
/// `key` across the shared bucket x-axis.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AiAgentTimelineRow {
    /// Bucket start in RFC3339 format.
    #[schema(example = "2026-05-29T12:00:00Z")]
    pub bucket: String,
    /// Provider or agent name this count belongs to.
    #[schema(example = "OpenAI")]
    pub key: String,
    pub request_count: i64,
}

/// One row in the AI-agent HTTP status breakdown: the request count for a
/// status class (`2xx`/`3xx`/`4xx`/`5xx`/`other`) across crawler traffic.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AiStatusBreakdownRow {
    /// Status class label.
    #[schema(example = "2xx")]
    pub status_class: String,
    pub request_count: i64,
}

/// Health summary for a single project (last 1 hour)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProjectHealthSummary {
    pub project_id: i32,
    /// Total requests in the period
    pub total_requests: i64,
    /// Total server errors (status >= 500) in the period
    pub total_errors: i64,
    /// Average response time in ms
    pub avg_response_time_ms: f64,
    /// Error rate as a percentage (0-100)
    pub error_rate: f64,
    /// Health status: "healthy", "degraded", "down", "unknown"
    pub status: String,
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
            status_code_class: None,
            routing_status: Some("routed".to_string()),
            request_source: Some("proxy".to_string()),
            is_bot: Some(false),
            device_type: Some("desktop".to_string()),
            has_project: None,
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
            status_code_class: None,
            routing_status: Some("routed".to_string()),
            request_source: Some("proxy".to_string()),
            is_bot: Some(false),
            device_type: Some("desktop".to_string()),
            has_project: None,
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
        assert!(filters.status_code_class.is_none());
        assert!(filters.routing_status.is_none());
        assert!(filters.request_source.is_none());
        assert!(filters.is_bot.is_none());
        assert!(filters.device_type.is_none());
    }

    // Note: Integration tests that require a database connection should be added
    // to test get_today_count and get_time_bucket_stats methods.
    // These would need a test database setup with sample data.
}
