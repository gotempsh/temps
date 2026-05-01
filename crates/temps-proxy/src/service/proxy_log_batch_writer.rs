use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use super::proxy_log_service::CreateProxyLogRequest;
use crate::crawler_detector::CrawlerDetector;

/// Maximum number of log entries buffered in the channel before backpressure kicks in.
/// At ~4 KB per entry, 8192 entries = ~32 MB maximum memory usage.
const CHANNEL_CAPACITY: usize = 8192;

/// Maximum number of rows per batch INSERT statement.
const MAX_BATCH_SIZE: usize = 200;

/// How long to wait for more entries before flushing a partial batch.
const FLUSH_INTERVAL: Duration = Duration::from_millis(500);

/// A handle for sending log entries to the batch writer.
/// Cloning this handle is cheap (Arc'd channel sender).
#[derive(Clone)]
pub struct ProxyLogBatchHandle {
    sender: mpsc::Sender<CreateProxyLogRequest>,
}

impl ProxyLogBatchHandle {
    /// Send a log entry to the batch writer.
    /// Applies backpressure when the channel is full (blocks the caller briefly).
    /// Returns false if the batch writer has been shut down.
    pub async fn send(&self, request: CreateProxyLogRequest) -> bool {
        self.sender.send(request).await.is_ok()
    }

    /// Try to send without blocking. Returns false if the channel is full or closed.
    /// Use this in contexts where you cannot afford to wait (e.g., fail_to_proxy).
    pub fn try_send(&self, request: CreateProxyLogRequest) -> bool {
        self.sender.try_send(request).is_ok()
    }
}

/// Background batch writer that collects proxy log entries and inserts them
/// in batches using multi-row INSERT statements for high throughput.
pub struct ProxyLogBatchWriter {
    db: Arc<DatabaseConnection>,
    ip_service: Arc<temps_geo::IpAddressService>,
    receiver: mpsc::Receiver<CreateProxyLogRequest>,
}

impl ProxyLogBatchWriter {
    /// Create a new batch writer and return the handle for sending entries.
    pub fn new(
        db: Arc<DatabaseConnection>,
        ip_service: Arc<temps_geo::IpAddressService>,
    ) -> (ProxyLogBatchHandle, Self) {
        let (sender, receiver) = mpsc::channel(CHANNEL_CAPACITY);
        let handle = ProxyLogBatchHandle { sender };
        let writer = Self {
            db,
            ip_service,
            receiver,
        };
        (handle, writer)
    }

    /// Run the batch writer loop. This consumes self and runs until the channel is closed.
    /// Should be spawned as a background task.
    pub async fn run(mut self) {
        info!(
            "Proxy log batch writer started (capacity={}, batch_size={}, flush_interval={}ms)",
            CHANNEL_CAPACITY,
            MAX_BATCH_SIZE,
            FLUSH_INTERVAL.as_millis()
        );

        let mut batch: Vec<CreateProxyLogRequest> = Vec::with_capacity(MAX_BATCH_SIZE);

        loop {
            // Wait for first entry or channel close
            let entry = if batch.is_empty() {
                match self.receiver.recv().await {
                    Some(entry) => entry,
                    None => {
                        info!("Proxy log batch writer shutting down (channel closed)");
                        break;
                    }
                }
            } else {
                // We have a partial batch -- wait up to FLUSH_INTERVAL for more entries
                match tokio::time::timeout(FLUSH_INTERVAL, self.receiver.recv()).await {
                    Ok(Some(entry)) => entry,
                    Ok(None) => {
                        // Channel closed, flush remaining
                        if !batch.is_empty() {
                            self.flush_batch(&mut batch).await;
                        }
                        info!("Proxy log batch writer shutting down (channel closed)");
                        break;
                    }
                    Err(_) => {
                        // Timeout -- flush what we have
                        self.flush_batch(&mut batch).await;
                        continue;
                    }
                }
            };

            batch.push(entry);

            // Drain as many entries as available without blocking (up to batch size)
            while batch.len() < MAX_BATCH_SIZE {
                match self.receiver.try_recv() {
                    Ok(entry) => batch.push(entry),
                    Err(_) => break,
                }
            }

            // Flush if batch is full
            if batch.len() >= MAX_BATCH_SIZE {
                self.flush_batch(&mut batch).await;
            }
        }
    }

    async fn flush_batch(&self, batch: &mut Vec<CreateProxyLogRequest>) {
        if batch.is_empty() {
            return;
        }

        let count = batch.len();
        debug!("Flushing batch of {} proxy log entries", count);

        // Enrich entries (UA parsing, bot detection, geo lookup) off the hot path
        for entry in batch.iter_mut() {
            self.enrich_entry(entry).await;
        }

        // Build and execute batch INSERT
        if let Err(e) = self.batch_insert(batch).await {
            error!(
                "Failed to batch insert {} proxy log entries: {:?}",
                count, e
            );
        }

        batch.clear();
    }

    async fn enrich_entry(&self, entry: &mut CreateProxyLogRequest) {
        // Parse user agent if not already parsed
        if entry.browser.is_none() {
            if let Some(ref ua_string) = entry.user_agent {
                let parser = woothee::parser::Parser::new();
                if let Some(ua) = parser.parse(ua_string) {
                    entry.browser = Some(ua.name.to_string());
                    entry.browser_version = Some(ua.version.to_string());
                    entry.operating_system = Some(ua.os.to_string());
                    entry.device_type = match ua.category {
                        "smartphone" | "mobilephone" => Some("mobile".to_string()),
                        "pc" => Some("desktop".to_string()),
                        _ => Some(ua.category.to_string()),
                    };
                }
            }
        }

        // Detect bots/crawlers if not already detected
        if entry.is_bot.is_none() {
            if let Some(ref ua_string) = entry.user_agent {
                let crawler_name = CrawlerDetector::get_crawler_name(Some(ua_string));
                entry.is_bot = Some(crawler_name.is_some());
                entry.bot_name = crawler_name;
            }
        }

        // Enrich with IP geolocation if not provided
        if entry.ip_geolocation_id.is_none() {
            if let Some(ref client_ip) = entry.client_ip {
                match self.ip_service.get_or_create_ip(client_ip).await {
                    Ok(geo) => entry.ip_geolocation_id = Some(geo.id),
                    Err(e) => {
                        warn!("Failed to geolocate IP {}: {:?}", client_ip, e);
                    }
                }
            }
        }
    }

    async fn batch_insert(&self, entries: &[CreateProxyLogRequest]) -> Result<(), sea_orm::DbErr> {
        if entries.is_empty() {
            return Ok(());
        }

        // Build a multi-row INSERT statement:
        // INSERT INTO proxy_logs (col1, col2, ...) VALUES ($1, $2, ...), ($N+1, $N+2, ...), ...
        let columns = [
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
        let cols_per_row = columns.len();

        let mut sql = format!("INSERT INTO proxy_logs ({}) VALUES ", columns.join(", "));

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

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_ip_service(db: Arc<DatabaseConnection>) -> Arc<temps_geo::IpAddressService> {
        // Use mock GeoIP service for tests (no MaxMind database file needed)
        unsafe { std::env::set_var("TEMPS_GEO_MOCK", "true") };
        let geoip_service =
            Arc::new(temps_geo::GeoIpService::new().expect("Failed to create GeoIpService"));
        Arc::new(temps_geo::IpAddressService::new(db, geoip_service))
    }

    #[tokio::test]
    async fn test_batch_handle_send_and_receive() {
        let db = Arc::new(DatabaseConnection::default());
        let ip_service = create_test_ip_service(db.clone());
        let (handle, mut writer) = ProxyLogBatchWriter::new(db, ip_service);

        let request = CreateProxyLogRequest {
            method: "GET".to_string(),
            path: "/".to_string(),
            query_string: None,
            host: "example.com".to_string(),
            status_code: 200,
            response_time_ms: Some(42),
            request_source: "proxy".to_string(),
            is_system_request: false,
            routing_status: "routed".to_string(),
            project_id: Some(1),
            environment_id: Some(1),
            deployment_id: Some(1),
            session_id: None,
            visitor_id: None,
            container_id: None,
            upstream_host: None,
            error_message: None,
            client_ip: Some("127.0.0.1".to_string()),
            user_agent: Some("test-agent".to_string()),
            referrer: None,
            request_id: "req-123".to_string(),
            ip_geolocation_id: None,
            browser: None,
            browser_version: None,
            operating_system: None,
            device_type: None,
            is_bot: None,
            bot_name: None,
            request_size_bytes: None,
            response_size_bytes: None,
            cache_status: None,
            request_headers: None,
            response_headers: None,
            trace_id: None,
            error_group_id: None,
        };

        // Send should succeed
        assert!(handle.send(request).await);

        // Verify the entry is in the channel
        let received = writer.receiver.try_recv();
        assert!(received.is_ok());
        assert_eq!(received.unwrap().path, "/");
    }

    #[tokio::test]
    async fn test_batch_handle_try_send() {
        let db = Arc::new(DatabaseConnection::default());
        let ip_service = create_test_ip_service(db.clone());
        let (handle, _writer) = ProxyLogBatchWriter::new(db, ip_service);

        let request = CreateProxyLogRequest {
            method: "GET".to_string(),
            path: "/test".to_string(),
            query_string: None,
            host: "example.com".to_string(),
            status_code: 503,
            response_time_ms: Some(100),
            request_source: "proxy".to_string(),
            is_system_request: false,
            routing_status: "error".to_string(),
            project_id: None,
            environment_id: None,
            deployment_id: None,
            session_id: None,
            visitor_id: None,
            container_id: None,
            upstream_host: None,
            error_message: Some("upstream timeout".to_string()),
            client_ip: None,
            user_agent: None,
            referrer: None,
            request_id: "req-456".to_string(),
            ip_geolocation_id: None,
            browser: None,
            browser_version: None,
            operating_system: None,
            device_type: None,
            is_bot: None,
            bot_name: None,
            request_size_bytes: None,
            response_size_bytes: None,
            cache_status: None,
            request_headers: None,
            response_headers: None,
            trace_id: None,
            error_group_id: None,
        };

        // try_send should succeed when channel has capacity
        assert!(handle.try_send(request));
    }

    #[tokio::test]
    async fn test_batch_handle_closed_channel() {
        let db = Arc::new(DatabaseConnection::default());
        let ip_service = create_test_ip_service(db.clone());
        let (handle, writer) = ProxyLogBatchWriter::new(db, ip_service);

        // Drop the writer (closes the receiver end)
        drop(writer);

        let request = CreateProxyLogRequest {
            method: "GET".to_string(),
            path: "/".to_string(),
            query_string: None,
            host: "example.com".to_string(),
            status_code: 200,
            response_time_ms: None,
            request_source: "proxy".to_string(),
            is_system_request: false,
            routing_status: "routed".to_string(),
            project_id: None,
            environment_id: None,
            deployment_id: None,
            session_id: None,
            visitor_id: None,
            container_id: None,
            upstream_host: None,
            error_message: None,
            client_ip: None,
            user_agent: None,
            referrer: None,
            request_id: "req-789".to_string(),
            ip_geolocation_id: None,
            browser: None,
            browser_version: None,
            operating_system: None,
            device_type: None,
            is_bot: None,
            bot_name: None,
            request_size_bytes: None,
            response_size_bytes: None,
            cache_status: None,
            request_headers: None,
            response_headers: None,
            trace_id: None,
            error_group_id: None,
        };

        // Send should fail because the writer (receiver) is dropped
        assert!(!handle.send(request).await);
    }

    #[tokio::test]
    async fn test_enrich_entry_parses_user_agent() {
        let db = Arc::new(DatabaseConnection::default());
        let ip_service = create_test_ip_service(db.clone());
        let (_, writer) = ProxyLogBatchWriter::new(db, ip_service);

        let mut entry = CreateProxyLogRequest {
            method: "GET".to_string(),
            path: "/".to_string(),
            query_string: None,
            host: "example.com".to_string(),
            status_code: 200,
            response_time_ms: None,
            request_source: "proxy".to_string(),
            is_system_request: false,
            routing_status: "routed".to_string(),
            project_id: None,
            environment_id: None,
            deployment_id: None,
            session_id: None,
            visitor_id: None,
            container_id: None,
            upstream_host: None,
            error_message: None,
            client_ip: None,
            user_agent: Some(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
                    .to_string(),
            ),
            referrer: None,
            request_id: "req-test".to_string(),
            ip_geolocation_id: None,
            browser: None,
            browser_version: None,
            operating_system: None,
            device_type: None,
            is_bot: None,
            bot_name: None,
            request_size_bytes: None,
            response_size_bytes: None,
            cache_status: None,
            request_headers: None,
            response_headers: None,
            trace_id: None,
            error_group_id: None,
        };

        writer.enrich_entry(&mut entry).await;

        // Browser should be detected
        assert!(entry.browser.is_some());
        assert!(entry.operating_system.is_some());
        assert!(entry.device_type.is_some());
        // Should not be detected as bot
        assert_eq!(entry.is_bot, Some(false));
        assert!(entry.bot_name.is_none());
    }

    #[tokio::test]
    async fn test_enrich_entry_detects_bot() {
        let db = Arc::new(DatabaseConnection::default());
        let ip_service = create_test_ip_service(db.clone());
        let (_, writer) = ProxyLogBatchWriter::new(db, ip_service);

        let mut entry = CreateProxyLogRequest {
            method: "GET".to_string(),
            path: "/".to_string(),
            query_string: None,
            host: "example.com".to_string(),
            status_code: 200,
            response_time_ms: None,
            request_source: "proxy".to_string(),
            is_system_request: false,
            routing_status: "routed".to_string(),
            project_id: None,
            environment_id: None,
            deployment_id: None,
            session_id: None,
            visitor_id: None,
            container_id: None,
            upstream_host: None,
            error_message: None,
            client_ip: None,
            user_agent: Some("Googlebot/2.1 (+http://www.google.com/bot.html)".to_string()),
            referrer: None,
            request_id: "req-bot".to_string(),
            ip_geolocation_id: None,
            browser: None,
            browser_version: None,
            operating_system: None,
            device_type: None,
            is_bot: None,
            bot_name: None,
            request_size_bytes: None,
            response_size_bytes: None,
            cache_status: None,
            request_headers: None,
            response_headers: None,
            trace_id: None,
            error_group_id: None,
        };

        writer.enrich_entry(&mut entry).await;

        assert_eq!(entry.is_bot, Some(true));
    }
}
