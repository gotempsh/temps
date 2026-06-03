use sea_orm::DatabaseConnection;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use super::proxy_log_service::CreateProxyLogRequest;
use crate::crawler_detector::CrawlerDetector;
use crate::storage::ProxyLogStorage;

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

/// Background batch writer that collects proxy log entries and persists them in
/// batches via the configured [`ProxyLogStorage`] backend (TimescaleDB multi-row
/// INSERT by default, ClickHouse when `TEMPS_CLICKHOUSE_*` is configured).
///
/// This task runs OFF the Pingora hot path — the hot path only ever does a
/// non-blocking `try_send` into the bounded channel. Persistence is fail-open:
/// if the backend errors, the batch is logged and dropped, never blocking live
/// traffic and never panicking.
pub struct ProxyLogBatchWriter {
    /// Database connection retained only for IP enrichment (`ip_service` owns
    /// the actual DB work). The proxy-log rows are written via `storage`.
    #[allow(dead_code)]
    db: Arc<DatabaseConnection>,
    ip_service: Arc<temps_geo::IpAddressService>,
    /// Pluggable persistence backend. Reproduces the prior TimescaleDB batch
    /// INSERT, or routes to ClickHouse when enabled.
    storage: Arc<dyn ProxyLogStorage>,
    receiver: mpsc::Receiver<CreateProxyLogRequest>,
}

impl ProxyLogBatchWriter {
    /// Create a new batch writer and return the handle for sending entries.
    ///
    /// `storage` is the persistence backend selected by
    /// [`crate::storage::build_proxy_log_storage`] from the server config — the
    /// same backend the HTTP read handlers use, so writes and reads always agree
    /// on where proxy logs live.
    pub fn new(
        db: Arc<DatabaseConnection>,
        ip_service: Arc<temps_geo::IpAddressService>,
        storage: Arc<dyn ProxyLogStorage>,
    ) -> (ProxyLogBatchHandle, Self) {
        let (sender, receiver) = mpsc::channel(CHANNEL_CAPACITY);
        let handle = ProxyLogBatchHandle { sender };
        let writer = Self {
            db,
            ip_service,
            storage,
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

        // Persist via the configured storage backend. FAIL OPEN: on backend
        // error we log and drop the batch — we never block live traffic and
        // never panic (this task is off the Pingora hot path, but the backend
        // could be down, e.g. ClickHouse unreachable).
        if let Err(e) = self.storage.write_batch(std::mem::take(batch)).await {
            error!(
                "Failed to persist {} proxy log entries (dropping batch): {:?}",
                count, e
            );
        }

        // `std::mem::take` already emptied the batch; clear() is a defensive
        // no-op in case write_batch short-circuits before taking ownership.
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

        // Detect bots/crawlers if not already detected. AI-agent detection runs
        // first so the canonical agent name (e.g. `GPTBot`, `ClaudeBot`) is
        // stored in `bot_name` — `CrawlerDetector` only returns a loose UA
        // substring (e.g. `ClaudeBot/1.0` -> `"Bot/"`), which never matches the
        // AI-agent analytics taxonomy. This is the live ingest path, so without
        // this the AI Agents page stays empty. Mirrors `ProxyLogService`.
        if entry.is_bot.is_none() {
            if let Some(ref ua_string) = entry.user_agent {
                if let Some(ai) = crate::ai_agent_detector::detect(Some(ua_string)) {
                    entry.is_bot = Some(true);
                    entry.bot_name = Some(ai.agent.to_string());
                } else {
                    let crawler_name = CrawlerDetector::get_crawler_name(Some(ua_string));
                    entry.is_bot = Some(crawler_name.is_some());
                    entry.bot_name = crawler_name;
                }
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

    /// Build the default (TimescaleDB) storage backend for batch-writer tests.
    /// These tests never call `write_batch` against a live DB — they exercise
    /// the channel + enrichment paths — so a disconnected store is fine.
    fn create_test_storage(
        db: Arc<DatabaseConnection>,
        ip_service: Arc<temps_geo::IpAddressService>,
    ) -> Arc<dyn ProxyLogStorage> {
        Arc::new(crate::storage::TimescaleDbProxyLogStore::new(
            db, ip_service,
        ))
    }

    #[tokio::test]
    async fn test_batch_handle_send_and_receive() {
        let db = Arc::new(DatabaseConnection::default());
        let ip_service = create_test_ip_service(db.clone());
        let storage = create_test_storage(db.clone(), ip_service.clone());
        let (handle, mut writer) = ProxyLogBatchWriter::new(db, ip_service, storage);

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
        let storage = create_test_storage(db.clone(), ip_service.clone());
        let (handle, _writer) = ProxyLogBatchWriter::new(db, ip_service, storage);

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
        let storage = create_test_storage(db.clone(), ip_service.clone());
        let (handle, writer) = ProxyLogBatchWriter::new(db, ip_service, storage);

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
        let storage = create_test_storage(db.clone(), ip_service.clone());
        let (_, writer) = ProxyLogBatchWriter::new(db, ip_service, storage);

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
        let storage = create_test_storage(db.clone(), ip_service.clone());
        let (_, writer) = ProxyLogBatchWriter::new(db, ip_service, storage);

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

    /// Regression test for the AI Agents analytics page showing no agents:
    /// the live ingest path (this batch writer) must classify AI crawlers with
    /// their CANONICAL taxonomy name (e.g. `ClaudeBot`), not the loose
    /// `CrawlerDetector` substring (e.g. `Bot/`). The analytics query filters
    /// `bot_name = ANY(known_agents)`, so a substring never matches and the
    /// page stays empty. See `ai_agent_detector`.
    #[tokio::test]
    async fn test_enrich_entry_detects_ai_agent_canonical_name() {
        let db = Arc::new(DatabaseConnection::default());
        let ip_service = create_test_ip_service(db.clone());
        let storage = create_test_storage(db.clone(), ip_service.clone());
        let (_, writer) = ProxyLogBatchWriter::new(db, ip_service, storage);

        // (user_agent, expected canonical bot_name) across several providers.
        let cases = [
            (
                "Mozilla/5.0 (compatible; ClaudeBot/1.0; +https://www.anthropic.com)",
                "ClaudeBot",
            ),
            (
                "Mozilla/5.0 (compatible; OAI-SearchBot/1.3; +https://openai.com/searchbot)",
                "OAI-SearchBot",
            ),
            (
                "Mozilla/5.0 (compatible; PerplexityBot/1.0; +https://perplexity.ai/bot)",
                "PerplexityBot",
            ),
            ("CCBot/2.0 (https://commoncrawl.org/faq/)", "CCBot"),
            (
                "meta-externalagent/1.1 (+https://developers.facebook.com/docs/sharing/webmasters/crawler)",
                "Meta-ExternalAgent",
            ),
        ];

        for (ua, expected) in cases {
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
                user_agent: Some(ua.to_string()),
                referrer: None,
                request_id: "req-ai".to_string(),
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

            assert_eq!(
                entry.is_bot,
                Some(true),
                "UA should be flagged as bot: {ua}"
            );
            assert_eq!(
                entry.bot_name.as_deref(),
                Some(expected),
                "UA {ua} must classify as canonical `{expected}`, not a CrawlerDetector substring"
            );
        }
    }
}
