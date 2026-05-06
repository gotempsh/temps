//! ClickHouse fan-out worker.
//!
//! Polls the `events_ch_outbox` table for undelivered rows, batches them,
//! pushes them to ClickHouse, and marks them delivered. Lives in its own
//! task so the synchronous PG insert path is never blocked on CH
//! availability.
//!
//! The worker is always compiled in. The plugin layer only spawns it when
//! `ServerConfig::is_clickhouse_enabled()` returns true (i.e. the operator
//! set `TEMPS_CLICKHOUSE_*` env vars). Operators do not need to rebuild
//! Temps with a feature flag to enable ClickHouse.
//!
//! Behavior:
//! - **Fail open**: if CH is down, rows queue indefinitely. The worker
//!   logs and retries on the next poll cycle. PG ingestion is unaffected.
//! - **At-least-once**: CH dedupe relies on `ReplacingMergeTree(_version)`
//!   keyed by `event_id`, so retries are safe.
//! - **Skip orphans**: if an outbox row references an event that's been
//!   retention-dropped, mark it delivered without sending.
//! - **Bounded backlog visibility**: the worker logs `claimed`/`pushed`
//!   counts; pair with `temps-monitoring` for alerting.

use std::sync::Arc;
use std::time::Duration;

use sea_orm::DatabaseConnection;
use tokio::sync::Notify;
use tracing::{debug, info, warn};

/// Configuration for the fan-out worker.
#[derive(Debug, Clone)]
pub struct ChFanoutConfig {
    /// How often to poll the outbox when no work is available.
    pub poll_interval: Duration,
    /// Max rows fetched and pushed per batch. ClickHouse prefers larger
    /// batches; 10k is a safe default for a single CH replica.
    pub batch_size: u32,
    /// Max attempts before a row is marked dead-lettered (logged + skipped).
    pub max_attempts: i32,
    /// Delete delivered outbox rows older than this. Without this the
    /// outbox grows unbounded — at 100 events/s an install accumulates
    /// ~8.6M rows/day.
    pub retention: Duration,
    /// How often to run the retention sweep (delete delivered rows older
    /// than `retention`). Cheaper than per-poll cleanup.
    pub retention_sweep_interval: Duration,
    /// How often to scan for dead-lettered rows (`attempts >= max_attempts`)
    /// and warn-log their count so operators can act.
    pub deadletter_scan_interval: Duration,
}

impl Default for ChFanoutConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(1),
            batch_size: 10_000,
            max_attempts: 10,
            // 7d covers most realistic CH outage windows; operators
            // who need a longer replay can extend.
            retention: Duration::from_secs(7 * 24 * 60 * 60),
            retention_sweep_interval: Duration::from_secs(60 * 60),
            deadletter_scan_interval: Duration::from_secs(5 * 60),
        }
    }
}

/// Worker handle. Spawn `run()` on a tokio task; signal `shutdown_handle()`
/// to stop gracefully (current batch finishes first).
pub struct ChFanoutWorker {
    db: Arc<DatabaseConnection>,
    ch: Arc<::clickhouse::Client>,
    config: ChFanoutConfig,
    shutdown: Arc<Notify>,
}

impl ChFanoutWorker {
    pub fn new(
        db: Arc<DatabaseConnection>,
        ch: Arc<::clickhouse::Client>,
        config: ChFanoutConfig,
    ) -> Self {
        Self {
            db,
            ch,
            config,
            shutdown: Arc::new(Notify::new()),
        }
    }

    pub fn shutdown_handle(&self) -> Arc<Notify> {
        self.shutdown.clone()
    }

    /// Run the worker loop until shutdown.
    ///
    /// Three concurrent timers feed `tokio::select!`:
    /// - **poll**: claim+push a batch every `poll_interval`.
    /// - **retention**: delete delivered rows older than `retention` every
    ///   `retention_sweep_interval`. Without this the outbox grows
    ///   unbounded.
    /// - **dead-letter scan**: count rows where `attempts >= max_attempts`
    ///   every `deadletter_scan_interval` and warn-log if non-zero so
    ///   operators see the problem in their log aggregator.
    ///
    /// Shutdown is cooperative: the current batch finishes before exit.
    pub async fn run(self) {
        info!(
            backend = "clickhouse",
            poll_interval_ms = self.config.poll_interval.as_millis() as u64,
            batch_size = self.config.batch_size,
            retention_secs = self.config.retention.as_secs(),
            "ch_fanout worker starting"
        );

        let mut retention_tick = tokio::time::interval(self.config.retention_sweep_interval);
        // Skip the first tick so we don't sweep on startup before the
        // first poll has even run.
        retention_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        retention_tick.tick().await;

        let mut deadletter_tick = tokio::time::interval(self.config.deadletter_scan_interval);
        deadletter_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        deadletter_tick.tick().await;

        loop {
            tokio::select! {
                _ = self.shutdown.notified() => {
                    info!("ch_fanout worker received shutdown signal");
                    break;
                }
                _ = tokio::time::sleep(self.config.poll_interval) => {
                    if let Err(e) = self.process_one_batch().await {
                        warn!(error = %e, "ch_fanout batch failed; will retry");
                    }
                }
                _ = retention_tick.tick() => {
                    if let Err(e) = self.sweep_retention().await {
                        warn!(error = %e, "ch_fanout retention sweep failed");
                    }
                }
                _ = deadletter_tick.tick() => {
                    if let Err(e) = self.scan_deadletters().await {
                        warn!(error = %e, "ch_fanout dead-letter scan failed");
                    }
                }
            }
        }

        info!("ch_fanout worker stopped");
    }

    /// Delete outbox rows that were successfully delivered more than
    /// `config.retention` ago. Runs in its own statement, not the
    /// process_one_batch hot path.
    async fn sweep_retention(&self) -> Result<(), ChFanoutError> {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        let secs = self.config.retention.as_secs() as i64;
        let sql = "DELETE FROM events_ch_outbox \
                   WHERE delivered_at IS NOT NULL \
                     AND delivered_at < NOW() - ($1 * INTERVAL '1 second')";
        let result = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                vec![secs.into()],
            ))
            .await?;
        let n = result.rows_affected();
        if n > 0 {
            debug!(rows = n, "ch_fanout retention swept delivered outbox rows");
        }
        Ok(())
    }

    /// Count rows that hit the retry ceiling (`attempts >= max_attempts`)
    /// without being delivered. These are stuck — log a warning so
    /// operators can investigate. We do NOT auto-delete them: better for
    /// an operator to see them and decide.
    async fn scan_deadletters(&self) -> Result<(), ChFanoutError> {
        use sea_orm::{DatabaseBackend, FromQueryResult, Statement};

        #[derive(FromQueryResult)]
        struct Counted {
            n: i64,
        }

        let sql = "SELECT COUNT(*)::bigint AS n \
                   FROM events_ch_outbox \
                   WHERE delivered_at IS NULL AND attempts >= $1";
        let row = Counted::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            vec![self.config.max_attempts.into()],
        ))
        .one(self.db.as_ref())
        .await?;
        if let Some(c) = row {
            if c.n > 0 {
                warn!(
                    deadletter_count = c.n,
                    max_attempts = self.config.max_attempts,
                    "ch_fanout has dead-lettered rows; investigate ClickHouse connectivity \
                     or row mapping. These will not be retried automatically. \
                     Inspect via: SELECT * FROM events_ch_outbox \
                     WHERE delivered_at IS NULL AND attempts >= max_attempts;"
                );
            }
        }
        Ok(())
    }

    async fn process_one_batch(&self) -> Result<(), ChFanoutError> {
        use sea_orm::{DatabaseBackend, FromQueryResult, Statement};

        // 1. Claim a batch with FOR UPDATE SKIP LOCKED so multiple workers
        //    (one per worker node) don't fight over the same rows. The
        //    ORDER BY enqueued_at keeps delivery roughly FIFO.
        let claim_sql = r#"
            UPDATE events_ch_outbox
            SET attempts = attempts + 1
            WHERE event_id IN (
                SELECT event_id
                FROM events_ch_outbox
                WHERE delivered_at IS NULL AND attempts < $1
                ORDER BY enqueued_at
                LIMIT $2
                FOR UPDATE SKIP LOCKED
            )
            RETURNING event_id
            "#;

        #[derive(FromQueryResult)]
        struct ClaimedRow {
            event_id: i64,
        }

        let claimed: Vec<i64> = ClaimedRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            claim_sql,
            vec![
                self.config.max_attempts.into(),
                (self.config.batch_size as i64).into(),
            ],
        ))
        .all(self.db.as_ref())
        .await?
        .into_iter()
        .map(|r| r.event_id)
        .collect();

        if claimed.is_empty() {
            return Ok(());
        }

        debug!(count = claimed.len(), "ch_fanout claimed batch");

        // 2. Load the actual event rows. If any are missing (retention
        //    drop or manual deletion), they're orphans — mark delivered
        //    without sending.
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use temps_entities::events;

        let rows = events::Entity::find()
            .filter(events::Column::Id.is_in(claimed.clone()))
            .all(self.db.as_ref())
            .await?;

        let found_ids: std::collections::HashSet<i64> = rows.iter().map(|r| r.id).collect();
        let orphans: Vec<i64> = claimed
            .iter()
            .copied()
            .filter(|id| !found_ids.contains(id))
            .collect();
        if !orphans.is_empty() {
            warn!(
                orphan_count = orphans.len(),
                "ch_fanout skipping orphaned outbox rows"
            );
            self.mark_delivered(&orphans).await?;
        }

        if rows.is_empty() {
            return Ok(());
        }

        let first_id = rows.first().map(|r| r.id).unwrap_or(0);

        // 3. Resolve geolocations once for the whole batch so country /
        //    region / city land on the CH row directly. Missing geo rows
        //    (deleted, or `ip_geolocation_id IS NULL`) become empty strings.
        use temps_entities::ip_geolocations;
        let geo_ids: Vec<i32> = rows.iter().filter_map(|r| r.ip_geolocation_id).collect();
        let geo_map: std::collections::HashMap<i32, ip_geolocations::Model> = if geo_ids.is_empty()
        {
            std::collections::HashMap::new()
        } else {
            ip_geolocations::Entity::find()
                .filter(ip_geolocations::Column::Id.is_in(geo_ids))
                .all(self.db.as_ref())
                .await?
                .into_iter()
                .map(|m| (m.id, m))
                .collect()
        };

        // 4. Push to CH via the typed Inserter.
        let mut inserter = self.ch.insert::<ChEventRow>("events").map_err(|e| {
            ChFanoutError::ClickHouseInsert {
                first_event_id: first_id,
                reason: format!("inserter setup failed: {e}"),
            }
        })?;

        let row_count = rows.len();
        for r in rows {
            let geo = r.ip_geolocation_id.and_then(|id| geo_map.get(&id));
            inserter.write(&row_to_ch(&r, geo)).await.map_err(|e| {
                ChFanoutError::ClickHouseInsert {
                    first_event_id: first_id,
                    reason: format!("write failed: {e}"),
                }
            })?;
        }
        inserter
            .end()
            .await
            .map_err(|e| ChFanoutError::ClickHouseInsert {
                first_event_id: first_id,
                reason: format!("end failed: {e}"),
            })?;

        debug!(count = row_count, "ch_fanout pushed batch to clickhouse");

        // 4. Mark delivered. If this fails after CH succeeded, the rows
        //    will be retried — CH dedupe via ReplacingMergeTree handles it.
        let delivered_ids: Vec<i64> = found_ids.into_iter().collect();
        self.mark_delivered(&delivered_ids).await?;

        Ok(())
    }

    /// Mark a list of event_ids delivered. Called for both successful
    /// pushes and orphan-skips.
    async fn mark_delivered(&self, ids: &[i64]) -> Result<(), ChFanoutError> {
        if ids.is_empty() {
            return Ok(());
        }
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        // ANY($1) is the libpq idiom for "id IN (list)".
        let sql = "UPDATE events_ch_outbox SET delivered_at = NOW() WHERE event_id = ANY($1)";
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                vec![ids.to_vec().into()],
            ))
            .await?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ChFanoutError {
    #[error("Database error in ch_fanout: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error(
        "ClickHouse insert failed for batch starting at outbox event_id {first_event_id}: {reason}"
    )]
    ClickHouseInsert { first_event_id: i64, reason: String },
}

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

/// ClickHouse-side row shape. Field order and types match the `events` DDL
/// in `migrations/clickhouse/0001_events.sql` exactly. The `clickhouse`
/// crate's `Row` derive does positional binary serialization, so this
/// must stay in lockstep with the DDL.
#[derive(::clickhouse::Row, serde::Serialize)]
struct ChEventRow {
    event_id: i64,
    project_id: i32,
    environment_id: Option<i32>,
    deployment_id: Option<i32>,
    session_id: String,
    visitor_id: Option<i32>,
    timestamp: i64,
    hostname: String,
    pathname: String,
    page_path: String,
    href: String,
    querystring: String,
    page_title: String,
    referrer: String,
    referrer_hostname: String,
    event_type: String,
    event_name: String,
    props: String,
    user_agent: String,
    browser: String,
    browser_version: String,
    operating_system: String,
    operating_system_version: String,
    device_type: String,
    screen_width: Option<i16>,
    screen_height: Option<i16>,
    viewport_width: Option<i16>,
    viewport_height: Option<i16>,
    ip_geolocation_id: Option<i32>,
    country: String,
    region: String,
    city: String,
    channel: String,
    utm_source: String,
    utm_medium: String,
    utm_campaign: String,
    utm_term: String,
    utm_content: String,
    ttfb: Option<f32>,
    lcp: Option<f32>,
    fid: Option<f32>,
    fcp: Option<f32>,
    cls: Option<f32>,
    inp: Option<f32>,
    is_entry: u8,
    is_exit: u8,
    is_bounce: u8,
    is_crawler: u8,
    time_on_page: Option<i32>,
    session_page_number: Option<i32>,
    scroll_depth: Option<i32>,
    clicks: Option<i32>,
    language: String,
    crawler_name: String,
}

/// Map a Postgres `events::Model` into the `ChEventRow` shape. `Option<String>`
/// becomes `""` because CH's `LowCardinality(String)` is non-null in our DDL
/// — empty string is the canonical "no value" sentinel. The `geo` parameter
/// is the matching `ip_geolocations` row (looked up by `m.ip_geolocation_id`)
/// so country/region/city land on the CH row directly — no cross-database
/// join at query time.
fn row_to_ch(
    m: &temps_entities::events::Model,
    geo: Option<&temps_entities::ip_geolocations::Model>,
) -> ChEventRow {
    use temps_core::DBDateTime;

    fn opt(s: &Option<String>) -> String {
        s.clone().unwrap_or_default()
    }

    fn ts_millis(ts: &DBDateTime) -> i64 {
        ts.timestamp_millis()
    }

    let (country, region, city) = match geo {
        Some(g) => (
            g.country.clone(),
            g.region.clone().unwrap_or_default(),
            g.city.clone().unwrap_or_default(),
        ),
        None => (String::new(), String::new(), String::new()),
    };

    ChEventRow {
        event_id: m.id,
        project_id: m.project_id,
        environment_id: m.environment_id,
        deployment_id: m.deployment_id,
        session_id: m.session_id.clone().unwrap_or_default(),
        visitor_id: m.visitor_id,
        timestamp: ts_millis(&m.timestamp),
        hostname: m.hostname.clone(),
        pathname: m.pathname.clone(),
        page_path: m.page_path.clone(),
        href: m.href.clone(),
        querystring: opt(&m.querystring),
        page_title: opt(&m.page_title),
        referrer: opt(&m.referrer),
        referrer_hostname: opt(&m.referrer_hostname),
        event_type: m.event_type.clone(),
        event_name: opt(&m.event_name),
        props: m.props.as_ref().map(|v| v.to_string()).unwrap_or_default(),
        user_agent: opt(&m.user_agent),
        browser: opt(&m.browser),
        browser_version: opt(&m.browser_version),
        operating_system: opt(&m.operating_system),
        operating_system_version: opt(&m.operating_system_version),
        device_type: opt(&m.device_type),
        screen_width: m.screen_width,
        screen_height: m.screen_height,
        viewport_width: m.viewport_width,
        viewport_height: m.viewport_height,
        ip_geolocation_id: m.ip_geolocation_id,
        country,
        region,
        city,
        channel: opt(&m.channel),
        utm_source: opt(&m.utm_source),
        utm_medium: opt(&m.utm_medium),
        utm_campaign: opt(&m.utm_campaign),
        utm_term: opt(&m.utm_term),
        utm_content: opt(&m.utm_content),
        ttfb: m.ttfb,
        lcp: m.lcp,
        fid: m.fid,
        fcp: m.fcp,
        cls: m.cls,
        inp: m.inp,
        is_entry: m.is_entry as u8,
        is_exit: m.is_exit as u8,
        is_bounce: m.is_bounce as u8,
        is_crawler: m.is_crawler as u8,
        time_on_page: m.time_on_page,
        session_page_number: m.session_page_number,
        scroll_depth: m.scroll_depth,
        clicks: m.clicks,
        language: opt(&m.language),
        crawler_name: opt(&m.crawler_name),
    }
}
