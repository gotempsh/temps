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
}

impl Default for ChFanoutConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(1),
            batch_size: 10_000,
            max_attempts: 10,
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
    pub async fn run(self) {
        info!(
            backend = "clickhouse",
            poll_interval_ms = self.config.poll_interval.as_millis() as u64,
            batch_size = self.config.batch_size,
            "ch_fanout worker starting"
        );

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
            }
        }

        info!("ch_fanout worker stopped");
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

        // 3. Push to CH via the typed Inserter.
        let mut inserter = self.ch.insert::<ChEventRow>("events").map_err(|e| {
            ChFanoutError::ClickHouseInsert {
                first_event_id: first_id,
                reason: format!("inserter setup failed: {e}"),
            }
        })?;

        let row_count = rows.len();
        for r in rows {
            inserter
                .write(&row_to_ch(&r))
                .await
                .map_err(|e| ChFanoutError::ClickHouseInsert {
                    first_event_id: first_id,
                    reason: format!("write failed: {e}"),
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
/// — empty string is the canonical "no value" sentinel.
fn row_to_ch(m: &temps_entities::events::Model) -> ChEventRow {
    use temps_core::DBDateTime;

    fn opt(s: &Option<String>) -> String {
        s.clone().unwrap_or_default()
    }

    fn ts_millis(ts: &DBDateTime) -> i64 {
        ts.timestamp_millis()
    }

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
