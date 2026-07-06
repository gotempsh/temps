use chrono::Utc;
use moka::future::Cache;
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use super::proxy_log_service::CreateProxyLogRequest;
use crate::crawler_detector::CrawlerDetector;
use crate::storage::ProxyLogStorage;
use crate::traits::FirstVisitAttribution;

/// Maximum number of log entries buffered in the channel before backpressure kicks in.
/// At ~4 KB per entry, 8192 entries = ~32 MB maximum memory usage.
const CHANNEL_CAPACITY: usize = 8192;

/// Maximum number of rows per batch INSERT statement.
const MAX_BATCH_SIZE: usize = 200;

/// Maximum tracking events per batch (visitors + sessions are lighter weight).
const MAX_TRACKING_BATCH_SIZE: usize = 512;

/// How long to wait before flushing a partial batch.
const FLUSH_INTERVAL: Duration = Duration::from_millis(500);

/// Emit a shed warning once per this many dropped entries (per handle kind),
/// so overload produces a handful of log lines instead of one per request.
const DROP_LOG_EVERY: u64 = 10_000;

/// TTL for the UUID → i32 id cache. Entries that haven't been accessed for
/// this duration are evicted; the next flush will re-populate from the DB.
const CACHE_TTL: Duration = Duration::from_secs(3600); // 1 hour

// ── TrackingBatchHandle ──────────────────────────────────────────────────────

/// A handle for enqueuing visitor/session tracking events to the background writer.
/// Cloning this handle is cheap (Arc'd channel sender).
#[derive(Clone)]
pub struct TrackingBatchHandle {
    sender: mpsc::Sender<TrackingEvent>,
    dropped: Arc<AtomicU64>,
}

impl TrackingBatchHandle {
    /// Enqueue a tracking event. Non-blocking — drops the event if the channel
    /// is full (fail-open, load-shedding), matching the proxy-log handle.
    pub fn send(&self, event: TrackingEvent) {
        if self.sender.try_send(event).is_err() {
            let total = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            if total == 1 || total.is_multiple_of(DROP_LOG_EVERY) {
                warn!(
                    dropped_total = total,
                    "Tracking batch channel full — shedding visitor/session events"
                );
            }
        }
    }

    /// Total tracking events shed since startup.
    pub fn dropped_total(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

// ── TrackingEvent ────────────────────────────────────────────────────────────

/// All data needed to asynchronously upsert a visitor row and a session row.
/// Created in-proxy (no DB round-trip) and shipped to the batch writer.
#[derive(Debug, Clone)]
pub struct TrackingEvent {
    /// Visitor UUID from the stateless cookie codec.
    pub visitor_uuid: String,
    /// Session UUID from the stateless cookie codec.
    pub session_uuid: String,
    pub project_id: i32,
    pub environment_id: i32,
    /// Timestamp of this page view (used as `last_seen` for the visitor and
    /// `last_accessed_at` for the session).
    pub last_seen: chrono::DateTime<Utc>,
    /// Raw client IP (used for geo-enrichment in the batch writer, not hot path).
    pub client_ip: Option<String>,
    pub user_agent: Option<String>,
    pub is_crawler: bool,
    pub crawler_name: Option<String>,
    /// `true` when a brand-new session was created by the cookie codec.
    pub is_new_session: bool,
    // ── Session attribution ─────────────────────────────────────────────────
    pub session_referrer: Option<String>,
    pub session_referrer_hostname: Option<String>,
    pub session_utm_source: Option<String>,
    pub session_utm_medium: Option<String>,
    pub session_utm_campaign: Option<String>,
    pub session_utm_content: Option<String>,
    pub session_utm_term: Option<String>,
    pub session_channel: Option<String>,
    // ── First-visit attribution ─────────────────────────────────────────────
    /// Stored ONLY when the visitor is NEW (ON CONFLICT does not overwrite these).
    pub attribution: FirstVisitAttribution,
}

// ── ProxyLogBatchHandle ──────────────────────────────────────────────────────

/// A handle for sending log entries to the batch writer.
/// Cloning this handle is cheap (Arc'd channel sender).
#[derive(Clone)]
pub struct ProxyLogBatchHandle {
    sender: mpsc::Sender<CreateProxyLogRequest>,
    dropped: Arc<AtomicU64>,
}

impl ProxyLogBatchHandle {
    /// Enqueue a log entry without ever blocking or queueing outside the
    /// bounded channel. When the writer can't keep up the entry is DROPPED
    /// (load-shedding, Cloudflare-style): access logs degrade under overload
    /// so proxy memory stays flat. Never `.await`-send from the hot path and
    /// never wrap this in `tokio::spawn` — parked send futures each pin the
    /// full request struct and become an unbounded queue in the task list.
    pub fn send_or_drop(&self, request: CreateProxyLogRequest) {
        if self.sender.try_send(request).is_err() {
            let total = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            if total == 1 || total.is_multiple_of(DROP_LOG_EVERY) {
                warn!(
                    dropped_total = total,
                    capacity = CHANNEL_CAPACITY,
                    "Proxy log channel full — shedding access-log entries"
                );
            }
        }
    }

    /// Total proxy-log entries shed since startup.
    pub fn dropped_total(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

// ── ProxyLogBatchWriter ──────────────────────────────────────────────────────

/// Background batch writer that:
/// 1. Consumes [`TrackingEvent`]s — upserts visitors + sessions, populates the
///    UUID→i32 moka cache.
/// 2. Consumes [`CreateProxyLogRequest`]s — enriches entries (UA/geo/bot) off the
///    hot path and persists them via the configured [`ProxyLogStorage`] backend.
///    Proxy-log rows get `visitor_id`/`session_id` FK values populated from the
///    cache when available.
///
/// Both channels are consumed by a single `tokio::select!` loop so there is only
/// one writer task. Persistence is fail-open: errors are logged and the batch is
/// dropped; live traffic is never blocked and this task never panics.
pub struct ProxyLogBatchWriter {
    /// Database connection for raw visitor/session upserts and geo-IP enrichment.
    #[allow(dead_code)]
    db: Arc<DatabaseConnection>,
    ip_service: Arc<temps_geo::IpAddressService>,
    /// Pluggable persistence backend for proxy_logs.
    storage: Arc<dyn ProxyLogStorage>,
    /// Receiver for proxy log entries.
    receiver: mpsc::Receiver<CreateProxyLogRequest>,
    /// Receiver for visitor/session tracking events.
    tracking_receiver: mpsc::Receiver<TrackingEvent>,
    /// visitor_uuid → visitor i32 id
    visitor_id_cache: Cache<String, i32>,
    /// session_uuid → session i32 id
    session_id_cache: Cache<String, i32>,
}

impl ProxyLogBatchWriter {
    /// Create a new batch writer and return the handles for sending entries.
    ///
    /// Returns `(ProxyLogBatchHandle, TrackingBatchHandle, Self)`.
    pub fn new(
        db: Arc<DatabaseConnection>,
        ip_service: Arc<temps_geo::IpAddressService>,
        storage: Arc<dyn ProxyLogStorage>,
    ) -> (ProxyLogBatchHandle, TrackingBatchHandle, Self) {
        let (sender, receiver) = mpsc::channel(CHANNEL_CAPACITY);
        let (tracking_sender, tracking_receiver) = mpsc::channel(CHANNEL_CAPACITY);

        let log_handle = ProxyLogBatchHandle {
            sender,
            dropped: Arc::new(AtomicU64::new(0)),
        };
        let tracking_handle = TrackingBatchHandle {
            sender: tracking_sender,
            dropped: Arc::new(AtomicU64::new(0)),
        };

        let visitor_id_cache = Cache::builder()
            .time_to_live(CACHE_TTL)
            .max_capacity(100_000)
            .build();
        let session_id_cache = Cache::builder()
            .time_to_live(CACHE_TTL)
            .max_capacity(100_000)
            .build();

        let writer = Self {
            db,
            ip_service,
            storage,
            receiver,
            tracking_receiver,
            visitor_id_cache,
            session_id_cache,
        };
        (log_handle, tracking_handle, writer)
    }

    /// Run the batch writer loop. This consumes self and runs until both channels
    /// are closed. Should be spawned as a background task.
    pub async fn run(mut self) {
        info!(
            "Proxy log batch writer started (capacity={}, batch_size={}, flush_interval={}ms)",
            CHANNEL_CAPACITY,
            MAX_BATCH_SIZE,
            FLUSH_INTERVAL.as_millis()
        );

        let mut proxy_log_batch: Vec<CreateProxyLogRequest> = Vec::with_capacity(MAX_BATCH_SIZE);
        let mut tracking_batch: Vec<TrackingEvent> = Vec::with_capacity(MAX_TRACKING_BATCH_SIZE);
        let mut interval = tokio::time::interval(FLUSH_INTERVAL);
        // Skip the first (immediate) tick so we don't flush an empty batch on startup.
        interval.reset();

        loop {
            tokio::select! {
                // Proxy log channel
                result = self.receiver.recv() => {
                    match result {
                        Some(entry) => {
                            proxy_log_batch.push(entry);
                            // Drain additional entries without blocking
                            while proxy_log_batch.len() < MAX_BATCH_SIZE {
                                match self.receiver.try_recv() {
                                    Ok(e) => proxy_log_batch.push(e),
                                    Err(_) => break,
                                }
                            }
                            if proxy_log_batch.len() >= MAX_BATCH_SIZE {
                                self.flush_all(&mut tracking_batch, &mut proxy_log_batch).await;
                            }
                        }
                        None => {
                            // Channel closed — flush and exit
                            self.flush_all(&mut tracking_batch, &mut proxy_log_batch).await;
                            info!("Proxy log batch writer shutting down (log channel closed)");
                            return;
                        }
                    }
                }
                // Tracking channel
                result = self.tracking_receiver.recv() => {
                    match result {
                        Some(event) => {
                            tracking_batch.push(event);
                            // Drain additional events without blocking
                            while tracking_batch.len() < MAX_TRACKING_BATCH_SIZE {
                                match self.tracking_receiver.try_recv() {
                                    Ok(e) => tracking_batch.push(e),
                                    Err(_) => break,
                                }
                            }
                        }
                        None => {
                            // Tracking channel closed (unusual) — keep running for proxy logs
                            debug!("Tracking channel closed");
                        }
                    }
                }
                // Periodic flush timer
                _ = interval.tick() => {
                    if !tracking_batch.is_empty() || !proxy_log_batch.is_empty() {
                        self.flush_all(&mut tracking_batch, &mut proxy_log_batch).await;
                    }
                }
            }
        }
    }

    /// Flush tracking events first (to populate the cache), then proxy log entries
    /// (so FK ids are available from the cache).
    async fn flush_all(
        &self,
        tracking_batch: &mut Vec<TrackingEvent>,
        proxy_log_batch: &mut Vec<CreateProxyLogRequest>,
    ) {
        if !tracking_batch.is_empty() {
            self.flush_tracking_batch(tracking_batch).await;
        }
        if !proxy_log_batch.is_empty() {
            self.flush_batch(proxy_log_batch).await;
        }
    }

    /// Upsert all unique visitors and sessions from the tracking batch.
    /// Populates the moka cache with UUID→i32 mappings for use by flush_batch.
    async fn flush_tracking_batch(&self, batch: &mut Vec<TrackingEvent>) {
        if batch.is_empty() {
            return;
        }

        // Deduplicate by visitor_uuid, keeping the event with the latest last_seen.
        let mut visitor_map: HashMap<String, TrackingEvent> = HashMap::new();
        for event in batch.iter() {
            visitor_map
                .entry(event.visitor_uuid.clone())
                .and_modify(|existing| {
                    if event.last_seen > existing.last_seen {
                        *existing = event.clone();
                    }
                })
                .or_insert_with(|| event.clone());
        }

        // Deduplicate by session_uuid, keeping the event with the latest last_seen.
        let mut session_map: HashMap<String, TrackingEvent> = HashMap::new();
        for event in batch.iter() {
            session_map
                .entry(event.session_uuid.clone())
                .and_modify(|existing| {
                    if event.last_seen > existing.last_seen {
                        *existing = event.clone();
                    }
                })
                .or_insert_with(|| event.clone());
        }

        debug!(
            "Flushing tracking batch: {} unique visitors, {} unique sessions",
            visitor_map.len(),
            session_map.len()
        );

        // Step 1: Resolve geo-IP for each unique visitor (off hot path).
        // We collect ip_address_id here so the upsert SQL can include it.
        let mut visitor_ip_ids: HashMap<String, Option<i32>> = HashMap::new();
        for event in visitor_map.values() {
            let ip_id = if let Some(ref ip) = event.client_ip {
                match self.ip_service.get_or_create_ip(ip).await {
                    Ok(geo) => Some(geo.id),
                    Err(e) => {
                        warn!("Failed to geolocate IP {}: {:?}", ip, e);
                        None
                    }
                }
            } else {
                None
            };
            visitor_ip_ids.insert(event.visitor_uuid.clone(), ip_id);
        }

        // Step 2: Upsert each unique visitor.
        // ON CONFLICT (visitor_id, project_id) → only update last_seen.
        // first_* attribution columns are never touched on conflict.
        for event in visitor_map.values() {
            let ip_id = visitor_ip_ids.get(&event.visitor_uuid).copied().flatten();
            match self.upsert_visitor(event, ip_id).await {
                Ok(visitor_db_id) => {
                    self.visitor_id_cache
                        .insert(event.visitor_uuid.clone(), visitor_db_id)
                        .await;
                }
                Err(e) => {
                    error!("Failed to upsert visitor {}: {:?}", event.visitor_uuid, e);
                }
            }
        }

        // Step 3: Upsert each unique session (needs visitor i32 id).
        for event in session_map.values() {
            let visitor_db_id = self.visitor_id_cache.get(&event.visitor_uuid).await;
            match self.upsert_session(event, visitor_db_id).await {
                Ok(session_db_id) => {
                    self.session_id_cache
                        .insert(event.session_uuid.clone(), session_db_id)
                        .await;
                }
                Err(e) => {
                    error!("Failed to upsert session {}: {:?}", event.session_uuid, e);
                }
            }
        }

        batch.clear();
    }

    /// Upsert a visitor row. Returns the DB `id` (i32).
    async fn upsert_visitor(
        &self,
        event: &TrackingEvent,
        ip_address_id: Option<i32>,
    ) -> Result<i32, sea_orm::DbErr> {
        let stmt = Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"INSERT INTO visitor (
                visitor_id, project_id, environment_id,
                first_seen, last_seen,
                user_agent, ip_address_id, is_crawler, crawler_name, has_activity,
                first_referrer, first_referrer_hostname, first_channel,
                first_utm_source, first_utm_medium, first_utm_campaign
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, false,
                $10, $11, $12, $13, $14, $15
            )
            ON CONFLICT (visitor_id, project_id) DO UPDATE SET
                last_seen = EXCLUDED.last_seen
            RETURNING id"#,
            [
                event.visitor_uuid.clone().into(),
                event.project_id.into(),
                event.environment_id.into(),
                event.last_seen.into(),
                event.last_seen.into(),
                event.user_agent.clone().into(),
                ip_address_id.into(),
                event.is_crawler.into(),
                event.crawler_name.clone().into(),
                event.attribution.referrer.clone().into(),
                event.attribution.referrer_hostname.clone().into(),
                event.attribution.channel.clone().into(),
                event.attribution.utm_source.clone().into(),
                event.attribution.utm_medium.clone().into(),
                event.attribution.utm_campaign.clone().into(),
            ],
        );

        let row = self.db.query_one(stmt).await?.ok_or_else(|| {
            sea_orm::DbErr::RecordNotFound("visitor upsert returned no row".into())
        })?;
        row.try_get::<i32>("", "id")
            .map_err(|e| sea_orm::DbErr::Type(format!("visitor.id: {e:?}")))
    }

    /// Upsert a session row. Returns the DB `id` (i32).
    async fn upsert_session(
        &self,
        event: &TrackingEvent,
        visitor_db_id: Option<i32>,
    ) -> Result<i32, sea_orm::DbErr> {
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
                last_accessed_at = EXCLUDED.last_accessed_at
            RETURNING id"#,
            [
                event.session_uuid.clone().into(),
                event.last_seen.into(),
                event.last_seen.into(),
                visitor_db_id.into(),
                event.session_referrer.clone().into(),
                event.session_referrer_hostname.clone().into(),
                event.session_utm_source.clone().into(),
                event.session_utm_medium.clone().into(),
                event.session_utm_campaign.clone().into(),
                event.session_utm_content.clone().into(),
                event.session_utm_term.clone().into(),
                event.session_channel.clone().into(),
            ],
        );

        let row = self.db.query_one(stmt).await?.ok_or_else(|| {
            sea_orm::DbErr::RecordNotFound("session upsert returned no row".into())
        })?;
        row.try_get::<i32>("", "id")
            .map_err(|e| sea_orm::DbErr::Type(format!("session.id: {e:?}")))
    }

    async fn flush_batch(&self, batch: &mut Vec<CreateProxyLogRequest>) {
        if batch.is_empty() {
            return;
        }

        let count = batch.len();
        debug!("Flushing batch of {} proxy log entries", count);

        // Enrich entries (UA parsing, bot detection, geo lookup) off the hot path.
        // Also resolve UUID → i32 FKs from the moka cache.
        for entry in batch.iter_mut() {
            self.enrich_entry(entry).await;
            self.resolve_tracking_ids(entry).await;
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

    /// Resolve visitor_uuid / session_uuid → i32 ids from the moka cache,
    /// backfilling the FK columns so proxy_log rows are linkable to visitor/session rows.
    async fn resolve_tracking_ids(&self, entry: &mut CreateProxyLogRequest) {
        if entry.visitor_id.is_none() {
            if let Some(ref uuid) = entry.visitor_uuid {
                if let Some(id) = self.visitor_id_cache.get(uuid).await {
                    entry.visitor_id = Some(id);
                }
            }
        }
        if entry.session_id.is_none() {
            if let Some(ref uuid) = entry.session_uuid {
                if let Some(id) = self.session_id_cache.get(uuid).await {
                    entry.session_id = Some(id);
                }
            }
        }
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

    fn make_test_log_request(path: &str) -> CreateProxyLogRequest {
        CreateProxyLogRequest {
            method: "GET".to_string(),
            path: path.to_string(),
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
            visitor_uuid: None,
            session_uuid: None,
            container_id: None,
            upstream_host: None,
            error_message: None,
            // None by default so enrich_entry tests don't hit the disconnected
            // MockDatabase via ip_service.get_or_create_ip().
            client_ip: None,
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
        }
    }

    #[tokio::test]
    async fn test_batch_handle_send_and_receive() {
        let db = Arc::new(DatabaseConnection::default());
        let ip_service = create_test_ip_service(db.clone());
        let storage = create_test_storage(db.clone(), ip_service.clone());
        let (handle, _tracking_handle, mut writer) =
            ProxyLogBatchWriter::new(db, ip_service, storage);

        let request = make_test_log_request("/");

        // Enqueue should not shed when the channel has capacity
        handle.send_or_drop(request);
        assert_eq!(handle.dropped_total(), 0);

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
        let (handle, _tracking_handle, _writer) = ProxyLogBatchWriter::new(db, ip_service, storage);

        let request = make_test_log_request("/test");

        // send_or_drop should not shed when channel has capacity
        handle.send_or_drop(request);
        assert_eq!(handle.dropped_total(), 0);
    }

    #[tokio::test]
    async fn test_batch_handle_closed_channel() {
        let db = Arc::new(DatabaseConnection::default());
        let ip_service = create_test_ip_service(db.clone());
        let storage = create_test_storage(db.clone(), ip_service.clone());
        let (handle, _tracking_handle, writer) = ProxyLogBatchWriter::new(db, ip_service, storage);

        // Drop the writer (closes the receiver end)
        drop(writer);

        let request = make_test_log_request("/");

        // Enqueue counts a shed entry because the writer (receiver) is dropped
        handle.send_or_drop(request);
        assert_eq!(handle.dropped_total(), 1);
    }

    #[tokio::test]
    async fn test_tracking_handle_send() {
        let db = Arc::new(DatabaseConnection::default());
        let ip_service = create_test_ip_service(db.clone());
        let storage = create_test_storage(db.clone(), ip_service.clone());
        let (_handle, tracking_handle, mut writer) =
            ProxyLogBatchWriter::new(db, ip_service, storage);

        let event = TrackingEvent {
            visitor_uuid: "test-visitor-uuid".to_string(),
            session_uuid: "test-session-uuid".to_string(),
            project_id: 1,
            environment_id: 1,
            last_seen: Utc::now(),
            client_ip: None,
            user_agent: None,
            is_crawler: false,
            crawler_name: None,
            is_new_session: false,
            session_referrer: None,
            session_referrer_hostname: None,
            session_utm_source: None,
            session_utm_medium: None,
            session_utm_campaign: None,
            session_utm_content: None,
            session_utm_term: None,
            session_channel: None,
            attribution: FirstVisitAttribution::default(),
        };

        tracking_handle.send(event);

        // Verify the event is in the tracking channel
        let received = writer.tracking_receiver.try_recv();
        assert!(received.is_ok());
        assert_eq!(received.unwrap().visitor_uuid, "test-visitor-uuid");
    }

    #[tokio::test]
    async fn test_tracking_dedup_keeps_latest() {
        let db = Arc::new(DatabaseConnection::default());
        let ip_service = create_test_ip_service(db.clone());
        let storage = create_test_storage(db.clone(), ip_service.clone());
        let (_handle, tracking_handle, mut writer) =
            ProxyLogBatchWriter::new(db, ip_service, storage);

        let now = Utc::now();
        let earlier = now - chrono::Duration::seconds(10);

        // Send two events for the same visitor: earlier first, then later
        tracking_handle.send(TrackingEvent {
            visitor_uuid: "same-visitor".to_string(),
            session_uuid: "sess-a".to_string(),
            project_id: 1,
            environment_id: 1,
            last_seen: earlier,
            client_ip: None,
            user_agent: None,
            is_crawler: false,
            crawler_name: None,
            is_new_session: true,
            session_referrer: None,
            session_referrer_hostname: None,
            session_utm_source: None,
            session_utm_medium: None,
            session_utm_campaign: None,
            session_utm_content: None,
            session_utm_term: None,
            session_channel: Some("Direct".to_string()),
            attribution: FirstVisitAttribution {
                referrer: Some("first-visit".to_string()),
                ..Default::default()
            },
        });
        tracking_handle.send(TrackingEvent {
            visitor_uuid: "same-visitor".to_string(),
            session_uuid: "sess-a".to_string(),
            project_id: 1,
            environment_id: 1,
            last_seen: now,
            client_ip: None,
            user_agent: None,
            is_crawler: false,
            crawler_name: None,
            is_new_session: false,
            session_referrer: None,
            session_referrer_hostname: None,
            session_utm_source: None,
            session_utm_medium: None,
            session_utm_campaign: None,
            session_utm_content: None,
            session_utm_term: None,
            session_channel: Some("Direct".to_string()),
            attribution: FirstVisitAttribution {
                referrer: Some("second-visit".to_string()),
                ..Default::default()
            },
        });

        // Collect the two events into a batch
        let mut batch = vec![
            writer.tracking_receiver.recv().await.unwrap(),
            writer.tracking_receiver.recv().await.unwrap(),
        ];

        // Simulate the dedup logic from flush_tracking_batch
        let mut visitor_map: std::collections::HashMap<String, TrackingEvent> =
            std::collections::HashMap::new();
        for event in batch.iter() {
            visitor_map
                .entry(event.visitor_uuid.clone())
                .and_modify(|existing| {
                    if event.last_seen > existing.last_seen {
                        *existing = event.clone();
                    }
                })
                .or_insert_with(|| event.clone());
        }
        batch.clear();

        assert_eq!(visitor_map.len(), 1, "dedup should keep exactly one entry");
        let kept = visitor_map.get("same-visitor").unwrap();
        assert_eq!(
            kept.last_seen, now,
            "dedup should keep the event with the LATEST last_seen"
        );
        // The latest event should be used; attribution.referrer is "second-visit"
        assert_eq!(kept.attribution.referrer.as_deref(), Some("second-visit"));
    }

    #[tokio::test]
    async fn test_enrich_entry_parses_user_agent() {
        let db = Arc::new(DatabaseConnection::default());
        let ip_service = create_test_ip_service(db.clone());
        let storage = create_test_storage(db.clone(), ip_service.clone());
        let (_, _, writer) = ProxyLogBatchWriter::new(db, ip_service, storage);

        let mut entry = CreateProxyLogRequest {
            user_agent: Some(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
                    .to_string(),
            ),
            ..make_test_log_request("/")
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
        let (_, _, writer) = ProxyLogBatchWriter::new(db, ip_service, storage);

        let mut entry = CreateProxyLogRequest {
            user_agent: Some("Googlebot/2.1 (+http://www.google.com/bot.html)".to_string()),
            ..make_test_log_request("/")
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
        let (_, _, writer) = ProxyLogBatchWriter::new(db, ip_service, storage);

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
                user_agent: Some(ua.to_string()),
                ..make_test_log_request("/")
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

    /// Attribution tests ported from services.rs — verify that the ON CONFLICT
    /// upsert correctly stores first_* attribution on creation and never
    /// overwrites it on subsequent visits.
    #[cfg(feature = "integration")]
    mod attribution_integration {
        use super::*;
        use sea_orm::{ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter};
        use temps_database::test_utils::TestDatabase;
        use temps_entities::{environments, projects, visitor};

        async fn setup_project(db: &Arc<DatabaseConnection>) -> (i32, i32) {
            let project = projects::ActiveModel {
                name: Set("Attribution Test".to_string()),
                slug: Set("attr-test".to_string()),
                repo_name: Set("repo".to_string()),
                repo_owner: Set("owner".to_string()),
                directory: Set(".".to_string()),
                main_branch: Set("main".to_string()),
                preset: Set(temps_entities::preset::Preset::Nixpacks),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await
            .unwrap();

            let environment = environments::ActiveModel {
                project_id: Set(project.id),
                name: Set("production".to_string()),
                slug: Set("production".to_string()),
                subdomain: Set("attr-test".to_string()),
                host: Set("attr-test.example.com".to_string()),
                upstreams: Set(temps_entities::upstream_config::UpstreamList::default()),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await
            .unwrap();

            (project.id, environment.id)
        }

        #[tokio::test]
        async fn test_first_referrer_stored_on_new_visitor() {
            let test_db = TestDatabase::with_migrations().await.unwrap();
            let db = test_db.connection_arc().clone();
            let ip_service = {
                let geoip = Arc::new(temps_geo::GeoIpService::Mock(
                    temps_geo::MockGeoIpService::new(),
                ));
                Arc::new(temps_geo::IpAddressService::new(db.clone(), geoip))
            };
            let storage = Arc::new(crate::storage::TimescaleDbProxyLogStore::new(
                db.clone(),
                ip_service.clone(),
            ));
            let (_, _, writer) = ProxyLogBatchWriter::new(db.clone(), ip_service, storage);

            let (project_id, environment_id) = setup_project(&db).await;
            let visitor_uuid = uuid::Uuid::new_v4().to_string();

            let event = TrackingEvent {
                visitor_uuid: visitor_uuid.clone(),
                session_uuid: uuid::Uuid::new_v4().to_string(),
                project_id,
                environment_id,
                last_seen: Utc::now(),
                client_ip: None,
                user_agent: None,
                is_crawler: false,
                crawler_name: None,
                is_new_session: true,
                session_referrer: None,
                session_referrer_hostname: None,
                session_utm_source: None,
                session_utm_medium: None,
                session_utm_campaign: None,
                session_utm_content: None,
                session_utm_term: None,
                session_channel: None,
                attribution: FirstVisitAttribution {
                    referrer: Some("https://www.google.com/search?q=temps".to_string()),
                    referrer_hostname: Some("www.google.com".to_string()),
                    channel: Some("Organic Search".to_string()),
                    utm_source: None,
                    utm_medium: None,
                    utm_campaign: None,
                },
            };

            writer.upsert_visitor(&event, None).await.unwrap();

            let row = visitor::Entity::find()
                .filter(visitor::Column::VisitorId.eq(&visitor_uuid))
                .one(db.as_ref())
                .await
                .unwrap()
                .expect("visitor row must exist");

            assert_eq!(
                row.first_referrer.as_deref(),
                Some("https://www.google.com/search?q=temps")
            );
            assert_eq!(
                row.first_referrer_hostname.as_deref(),
                Some("www.google.com")
            );
            assert_eq!(row.first_channel.as_deref(), Some("Organic Search"));
        }

        #[tokio::test]
        async fn test_returning_visitor_first_referrer_not_overwritten() {
            let test_db = TestDatabase::with_migrations().await.unwrap();
            let db = test_db.connection_arc().clone();
            let ip_service = {
                let geoip = Arc::new(temps_geo::GeoIpService::Mock(
                    temps_geo::MockGeoIpService::new(),
                ));
                Arc::new(temps_geo::IpAddressService::new(db.clone(), geoip))
            };
            let storage = Arc::new(crate::storage::TimescaleDbProxyLogStore::new(
                db.clone(),
                ip_service.clone(),
            ));
            let (_, _, writer) = ProxyLogBatchWriter::new(db.clone(), ip_service, storage);

            let (project_id, environment_id) = setup_project(&db).await;
            let visitor_uuid = uuid::Uuid::new_v4().to_string();

            // First visit: Google
            let first_event = TrackingEvent {
                visitor_uuid: visitor_uuid.clone(),
                session_uuid: uuid::Uuid::new_v4().to_string(),
                project_id,
                environment_id,
                last_seen: Utc::now() - chrono::Duration::seconds(60),
                client_ip: None,
                user_agent: None,
                is_crawler: false,
                crawler_name: None,
                is_new_session: true,
                session_referrer: None,
                session_referrer_hostname: None,
                session_utm_source: None,
                session_utm_medium: None,
                session_utm_campaign: None,
                session_utm_content: None,
                session_utm_term: None,
                session_channel: None,
                attribution: FirstVisitAttribution {
                    referrer: Some("https://www.google.com/search?q=temps".to_string()),
                    referrer_hostname: Some("www.google.com".to_string()),
                    channel: Some("Organic Search".to_string()),
                    utm_source: None,
                    utm_medium: None,
                    utm_campaign: None,
                },
            };
            writer.upsert_visitor(&first_event, None).await.unwrap();

            // Second visit: Twitter (should NOT overwrite first_*)
            let second_event = TrackingEvent {
                visitor_uuid: visitor_uuid.clone(),
                session_uuid: uuid::Uuid::new_v4().to_string(),
                project_id,
                environment_id,
                last_seen: Utc::now(),
                client_ip: None,
                user_agent: None,
                is_crawler: false,
                crawler_name: None,
                is_new_session: false,
                session_referrer: None,
                session_referrer_hostname: None,
                session_utm_source: None,
                session_utm_medium: None,
                session_utm_campaign: None,
                session_utm_content: None,
                session_utm_term: None,
                session_channel: None,
                attribution: FirstVisitAttribution {
                    referrer: Some("https://twitter.com/someone/status/123".to_string()),
                    referrer_hostname: Some("twitter.com".to_string()),
                    channel: Some("Organic Social".to_string()),
                    utm_source: Some("twitter".to_string()),
                    utm_medium: None,
                    utm_campaign: None,
                },
            };
            writer.upsert_visitor(&second_event, None).await.unwrap();

            let row = visitor::Entity::find()
                .filter(visitor::Column::VisitorId.eq(&visitor_uuid))
                .one(db.as_ref())
                .await
                .unwrap()
                .expect("visitor row must exist");

            assert_eq!(
                row.first_referrer.as_deref(),
                Some("https://www.google.com/search?q=temps"),
                "first_referrer must NOT be overwritten"
            );
            assert_eq!(
                row.first_channel.as_deref(),
                Some("Organic Search"),
                "first_channel must NOT be overwritten"
            );
        }
    }
}
