// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Edge analytics — local SQLite via Sea-ORM + query API.
//!
//! Every request produces an `EdgeRequestEvent` that is batched and inserted
//! into a local SQLite database. The edge exposes query methods that the Temps
//! dashboard can call to visualize per-asset, per-domain, time-series analytics.
//!
//! Data stays at the edge. The origin queries edge nodes on demand.

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectOptions, ConnectionTrait, Database,
    DatabaseBackend, DatabaseConnection, EntityTrait, FromQueryResult, QueryFilter, Statement,
};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::entities;

// ---------------------------------------------------------------------------
// Event types
// ---------------------------------------------------------------------------

/// A single edge request event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeRequestEvent {
    pub timestamp: DateTime<Utc>,
    pub domain: String,
    pub path: String,
    pub method: String,
    pub status: u16,
    /// "HIT", "MISS", "BYPASS", "ERROR"
    pub cache_status: String,
    pub bytes_sent: u64,
    /// Origin fetch latency in ms (0 for cache hits)
    pub origin_latency_ms: f64,
    pub region: Option<String>,
    pub is_immutable: bool,
}

/// Helper to build an event from proxy context.
#[allow(clippy::too_many_arguments)]
pub fn make_event(
    domain: &str,
    path: &str,
    method: &str,
    status: u16,
    cache_status: &str,
    bytes_sent: u64,
    origin_latency_ms: f64,
    region: Option<&str>,
    is_immutable: bool,
) -> EdgeRequestEvent {
    EdgeRequestEvent {
        timestamp: Utc::now(),
        domain: domain.to_string(),
        path: path.to_string(),
        method: method.to_string(),
        status,
        cache_status: cache_status.to_string(),
        bytes_sent,
        origin_latency_ms,
        region: region.map(|s| s.to_string()),
        is_immutable,
    }
}

// ---------------------------------------------------------------------------
// Analytics store (Sea-ORM + SQLite)
// ---------------------------------------------------------------------------

const CHANNEL_CAPACITY: usize = 16384;
const MAX_BATCH_SIZE: usize = 500;
const FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// The local analytics store backed by SQLite via Sea-ORM.
pub struct AnalyticsStore {
    db: DatabaseConnection,
}

impl AnalyticsStore {
    /// Connect to (or create) the SQLite analytics database and run migrations.
    pub async fn open(db_path: &Path) -> Result<Self, sea_orm::DbErr> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let url = format!("sqlite:{}?mode=rwc", db_path.display());
        let mut opts = ConnectOptions::new(&url);
        opts.max_connections(1).sqlx_logging(false);

        let db = Database::connect(opts).await?;

        // Run migrations
        use sea_orm_migration::MigratorTrait;
        crate::migrations::Migrator::up(&db, None).await?;

        // Performance pragmas
        db.execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            "PRAGMA journal_mode = WAL".to_string(),
        ))
        .await?;
        db.execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            "PRAGMA synchronous = NORMAL".to_string(),
        ))
        .await?;

        info!("Edge analytics SQLite opened at {:?}", db_path);
        Ok(Self { db })
    }

    /// Open an in-memory database (for testing).
    pub async fn open_in_memory() -> Result<Self, sea_orm::DbErr> {
        let db = Database::connect("sqlite::memory:").await?;

        use sea_orm_migration::MigratorTrait;
        crate::migrations::Migrator::up(&db, None).await?;

        Ok(Self { db })
    }

    /// Insert a batch of events.
    pub async fn insert_batch(&self, events: &[EdgeRequestEvent]) -> Result<usize, sea_orm::DbErr> {
        let mut count = 0;
        for e in events {
            let model = entities::ActiveModel {
                id: Default::default(),
                timestamp: Set(e.timestamp.to_rfc3339()),
                domain: Set(e.domain.clone()),
                path: Set(e.path.clone()),
                method: Set(e.method.clone()),
                status: Set(e.status as i32),
                cache_status: Set(e.cache_status.clone()),
                bytes_sent: Set(e.bytes_sent as i64),
                origin_latency_ms: Set(e.origin_latency_ms),
                region: Set(e.region.clone()),
                is_immutable: Set(e.is_immutable),
            };
            model.insert(&self.db).await?;
            count += 1;
        }
        Ok(count)
    }

    /// Delete events older than `retention_days`.
    pub async fn prune(&self, retention_days: i64) -> Result<u64, sea_orm::DbErr> {
        let cutoff = Utc::now() - chrono::Duration::days(retention_days);
        let cutoff_str = cutoff.to_rfc3339();

        let result = entities::Entity::delete_many()
            .filter(entities::Column::Timestamp.lt(cutoff_str))
            .exec(&self.db)
            .await?;

        if result.rows_affected > 0 {
            info!("Pruned {} old edge analytics records", result.rows_affected);
        }
        Ok(result.rows_affected)
    }

    /// Get the database connection (for the query API).
    pub fn db(&self) -> &DatabaseConnection {
        &self.db
    }

    // -----------------------------------------------------------------------
    // Query methods — called by the edge Axum API
    // -----------------------------------------------------------------------

    /// Overview stats for a time range.
    pub async fn query_overview(
        &self,
        since: &str,
        until: &str,
    ) -> Result<OverviewResponse, sea_orm::DbErr> {
        let row = OverviewRaw::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT
                COUNT(*) as total_requests,
                COALESCE(SUM(CASE WHEN cache_status = 'HIT' THEN 1 ELSE 0 END), 0) as cache_hits,
                COALESCE(SUM(CASE WHEN cache_status = 'MISS' THEN 1 ELSE 0 END), 0) as cache_misses,
                COALESCE(SUM(CASE WHEN cache_status = 'BYPASS' THEN 1 ELSE 0 END), 0) as cache_bypasses,
                COALESCE(SUM(CASE WHEN cache_status = 'HIT' THEN bytes_sent ELSE 0 END), 0) as bytes_from_cache,
                COALESCE(SUM(CASE WHEN cache_status = 'MISS' THEN bytes_sent ELSE 0 END), 0) as bytes_from_origin,
                CAST(COALESCE(AVG(CASE WHEN cache_status = 'MISS' THEN origin_latency_ms END), 0) AS REAL) as avg_origin_latency_ms,
                COUNT(DISTINCT domain) as unique_domains
             FROM edge_requests
             WHERE timestamp >= $1 AND timestamp <= $2",
            [since.into(), until.into()],
        ))
        .one(&self.db)
        .await?;

        match row {
            Some(r) => {
                let cacheable = r.cache_hits + r.cache_misses;
                let cache_hit_rate = if cacheable > 0 {
                    r.cache_hits as f64 / cacheable as f64
                } else {
                    0.0
                };
                let total_bytes = r.bytes_from_cache + r.bytes_from_origin;
                let bandwidth_savings_rate = if total_bytes > 0 {
                    r.bytes_from_cache as f64 / total_bytes as f64
                } else {
                    0.0
                };
                Ok(OverviewResponse {
                    total_requests: r.total_requests as u64,
                    cache_hits: r.cache_hits as u64,
                    cache_misses: r.cache_misses as u64,
                    cache_bypasses: r.cache_bypasses as u64,
                    cache_hit_rate,
                    bytes_from_cache: r.bytes_from_cache as u64,
                    bytes_from_origin: r.bytes_from_origin as u64,
                    bandwidth_savings_rate,
                    avg_origin_latency_ms: r.avg_origin_latency_ms,
                    unique_domains: r.unique_domains as u64,
                })
            }
            None => Ok(OverviewResponse::default()),
        }
    }

    /// Per-domain breakdown.
    pub async fn query_domains(
        &self,
        since: &str,
        until: &str,
        limit: u32,
    ) -> Result<Vec<DomainRow>, sea_orm::DbErr> {
        let rows = DomainRaw::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT
                domain,
                COUNT(*) as requests,
                SUM(CASE WHEN cache_status = 'HIT' THEN 1 ELSE 0 END) as hits,
                SUM(CASE WHEN cache_status = 'MISS' THEN 1 ELSE 0 END) as misses,
                SUM(bytes_sent) as total_bytes,
                CAST(COALESCE(AVG(CASE WHEN cache_status = 'MISS' THEN origin_latency_ms END), 0) AS REAL) as avg_latency
             FROM edge_requests
             WHERE timestamp >= $1 AND timestamp <= $2
             GROUP BY domain
             ORDER BY requests DESC
             LIMIT $3",
            [since.into(), until.into(), (limit as i64).into()],
        ))
        .all(&self.db)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| {
                let cacheable = r.hits + r.misses;
                DomainRow {
                    domain: r.domain,
                    requests: r.requests as u64,
                    cache_hits: r.hits as u64,
                    cache_misses: r.misses as u64,
                    cache_hit_rate: if cacheable > 0 {
                        r.hits as f64 / cacheable as f64
                    } else {
                        0.0
                    },
                    total_bytes: r.total_bytes as u64,
                    avg_origin_latency_ms: r.avg_latency,
                }
            })
            .collect())
    }

    /// Top assets by request count.
    pub async fn query_top_assets(
        &self,
        since: &str,
        until: &str,
        limit: u32,
    ) -> Result<Vec<AssetRow>, sea_orm::DbErr> {
        let rows = AssetRaw::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT domain, path, COUNT(*) as requests,
                    SUM(CASE WHEN cache_status = 'HIT' THEN 1 ELSE 0 END) as cache_hits,
                    SUM(bytes_sent) as total_bytes
             FROM edge_requests
             WHERE timestamp >= $1 AND timestamp <= $2
             GROUP BY domain, path ORDER BY requests DESC LIMIT $3",
            [since.into(), until.into(), (limit as i64).into()],
        ))
        .all(&self.db)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| AssetRow {
                domain: r.domain,
                path: r.path,
                requests: r.requests as u64,
                cache_hits: r.cache_hits as u64,
                total_bytes: r.total_bytes as u64,
            })
            .collect())
    }

    /// Time series (bucketed by interval).
    pub async fn query_timeseries(
        &self,
        since: &str,
        until: &str,
        bucket_minutes: u32,
    ) -> Result<Vec<TimeseriesRow>, sea_orm::DbErr> {
        let sql = format!(
            "SELECT
                strftime('%Y-%m-%dT%H:', timestamp) ||
                    printf('%02d', (CAST(strftime('%M', timestamp) AS INTEGER) / {b}) * {b}) ||
                    ':00Z' as bucket,
                COUNT(*) as requests,
                COALESCE(SUM(CASE WHEN cache_status = 'HIT' THEN 1 ELSE 0 END), 0) as hits,
                COALESCE(SUM(CASE WHEN cache_status = 'MISS' THEN 1 ELSE 0 END), 0) as misses,
                COALESCE(SUM(bytes_sent), 0) as bytes_sent,
                CAST(COALESCE(AVG(CASE WHEN cache_status = 'MISS' THEN origin_latency_ms END), 0) AS REAL) as avg_latency
             FROM edge_requests
             WHERE timestamp >= $1 AND timestamp <= $2
             GROUP BY bucket ORDER BY bucket",
            b = bucket_minutes
        );

        let rows = TimeseriesRaw::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            &sql,
            [since.into(), until.into()],
        ))
        .all(&self.db)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| TimeseriesRow {
                bucket: r.bucket,
                requests: r.requests as u64,
                cache_hits: r.hits as u64,
                cache_misses: r.misses as u64,
                bytes_sent: r.bytes_sent as u64,
                avg_origin_latency_ms: r.avg_latency,
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Raw query result structs (for FromQueryResult)
// ---------------------------------------------------------------------------

#[derive(Debug, FromQueryResult)]
struct OverviewRaw {
    total_requests: i64,
    cache_hits: i64,
    cache_misses: i64,
    cache_bypasses: i64,
    bytes_from_cache: i64,
    bytes_from_origin: i64,
    avg_origin_latency_ms: f64,
    unique_domains: i64,
}

#[derive(Debug, FromQueryResult)]
struct DomainRaw {
    domain: String,
    requests: i64,
    hits: i64,
    misses: i64,
    total_bytes: i64,
    avg_latency: f64,
}

#[derive(Debug, FromQueryResult)]
struct AssetRaw {
    domain: String,
    path: String,
    requests: i64,
    cache_hits: i64,
    total_bytes: i64,
}

#[derive(Debug, FromQueryResult)]
struct TimeseriesRaw {
    bucket: String,
    requests: i64,
    hits: i64,
    misses: i64,
    bytes_sent: i64,
    avg_latency: f64,
}

// ---------------------------------------------------------------------------
// Public query response types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OverviewResponse {
    pub total_requests: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub cache_bypasses: u64,
    pub cache_hit_rate: f64,
    pub bytes_from_cache: u64,
    pub bytes_from_origin: u64,
    pub bandwidth_savings_rate: f64,
    pub avg_origin_latency_ms: f64,
    pub unique_domains: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainRow {
    pub domain: String,
    pub requests: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub cache_hit_rate: f64,
    pub total_bytes: u64,
    pub avg_origin_latency_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetRow {
    pub domain: String,
    pub path: String,
    pub requests: u64,
    pub cache_hits: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeseriesRow {
    pub bucket: String,
    pub requests: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub bytes_sent: u64,
    pub avg_origin_latency_ms: f64,
}

// ---------------------------------------------------------------------------
// Write pipeline: channel → batch insert
// ---------------------------------------------------------------------------

/// Handle for recording events from the proxy hot path. Cheap to clone.
#[derive(Clone)]
pub struct EdgeAnalyticsHandle {
    pub(crate) tx: mpsc::Sender<EdgeRequestEvent>,
}

impl EdgeAnalyticsHandle {
    /// Record a request event. Non-blocking — drops if channel is full.
    pub fn record(&self, event: EdgeRequestEvent) {
        if self.tx.try_send(event).is_err() {
            debug!("Edge analytics channel full, event dropped");
        }
    }
}

/// Background writer that batches events and inserts into SQLite.
pub struct EdgeAnalyticsWriter {
    rx: mpsc::Receiver<EdgeRequestEvent>,
    store: Arc<AnalyticsStore>,
}

/// Create the analytics pipeline.
/// Returns a handle (for the proxy), a writer (to spawn), and the store (for queries).
pub async fn create_analytics_pipeline(
    db_path: &Path,
) -> Result<
    (
        EdgeAnalyticsHandle,
        EdgeAnalyticsWriter,
        Arc<AnalyticsStore>,
    ),
    sea_orm::DbErr,
> {
    let store = Arc::new(AnalyticsStore::open(db_path).await?);
    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);

    let handle = EdgeAnalyticsHandle { tx };
    let writer = EdgeAnalyticsWriter {
        rx,
        store: store.clone(),
    };

    Ok((handle, writer, store))
}

impl EdgeAnalyticsWriter {
    /// Run the batch insert loop. Call in `tokio::spawn`.
    pub async fn run(mut self) {
        let mut batch: Vec<EdgeRequestEvent> = Vec::with_capacity(MAX_BATCH_SIZE);
        let mut flush_interval = tokio::time::interval(FLUSH_INTERVAL);
        let mut prune_interval = tokio::time::interval(std::time::Duration::from_secs(3600));

        loop {
            tokio::select! {
                event = self.rx.recv() => {
                    match event {
                        Some(e) => {
                            batch.push(e);
                            if batch.len() >= MAX_BATCH_SIZE {
                                self.flush(&mut batch).await;
                            }
                        }
                        None => {
                            if !batch.is_empty() {
                                self.flush(&mut batch).await;
                            }
                            return;
                        }
                    }
                }
                _ = flush_interval.tick() => {
                    if !batch.is_empty() {
                        self.flush(&mut batch).await;
                    }
                }
                _ = prune_interval.tick() => {
                    if let Err(e) = self.store.prune(7).await {
                        warn!("Failed to prune analytics: {}", e);
                    }
                }
            }
        }
    }

    async fn flush(&self, batch: &mut Vec<EdgeRequestEvent>) {
        let events: Vec<EdgeRequestEvent> = std::mem::take(batch);
        let count = events.len();
        match self.store.insert_batch(&events).await {
            Ok(n) => debug!("Inserted {} edge analytics events", n),
            Err(e) => error!("Failed to insert {} analytics events: {}", count, e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_serialization() {
        let event = make_event(
            "app.example.com",
            "/_next/static/chunks/main-abc.js",
            "GET",
            200,
            "HIT",
            45000,
            0.0,
            Some("ap-southeast"),
            true,
        );
        let json = serde_json::to_string(&event).unwrap();
        let parsed: EdgeRequestEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.domain, "app.example.com");
        assert_eq!(parsed.cache_status, "HIT");
        assert_eq!(parsed.bytes_sent, 45000);
    }

    #[tokio::test]
    async fn test_store_insert_and_query_overview() {
        let store = AnalyticsStore::open_in_memory().await.unwrap();

        let events = vec![
            make_event(
                "a.com", "/main.js", "GET", 200, "HIT", 5000, 0.0, None, true,
            ),
            make_event("a.com", "/app.js", "GET", 200, "HIT", 3000, 0.0, None, true),
            make_event(
                "a.com", "/data", "GET", 200, "MISS", 1000, 150.0, None, false,
            ),
            make_event("b.com", "/", "GET", 200, "BYPASS", 2000, 0.0, None, false),
        ];
        store.insert_batch(&events).await.unwrap();

        let overview = store
            .query_overview("2000-01-01T00:00:00Z", "2099-01-01T00:00:00Z")
            .await
            .unwrap();
        assert_eq!(overview.total_requests, 4);
        assert_eq!(overview.cache_hits, 2);
        assert_eq!(overview.cache_misses, 1);
        assert_eq!(overview.cache_bypasses, 1);
        assert_eq!(overview.bytes_from_cache, 8000);
        assert_eq!(overview.bytes_from_origin, 1000);
        assert_eq!(overview.unique_domains, 2);
    }

    #[tokio::test]
    async fn test_store_query_domains() {
        let store = AnalyticsStore::open_in_memory().await.unwrap();

        let events = vec![
            make_event("a.com", "/1.js", "GET", 200, "HIT", 100, 0.0, None, true),
            make_event("a.com", "/2.js", "GET", 200, "HIT", 100, 0.0, None, true),
            make_event("b.com", "/1.js", "GET", 200, "MISS", 100, 50.0, None, true),
        ];
        store.insert_batch(&events).await.unwrap();

        let domains = store
            .query_domains("2000-01-01T00:00:00Z", "2099-01-01T00:00:00Z", 10)
            .await
            .unwrap();
        assert_eq!(domains.len(), 2);
        assert_eq!(domains[0].domain, "a.com");
        assert_eq!(domains[0].requests, 2);
    }

    #[tokio::test]
    async fn test_store_query_top_assets() {
        let store = AnalyticsStore::open_in_memory().await.unwrap();

        let mut events = Vec::new();
        for _ in 0..10 {
            events.push(make_event(
                "a.com", "/hot.js", "GET", 200, "HIT", 100, 0.0, None, true,
            ));
        }
        for _ in 0..3 {
            events.push(make_event(
                "a.com", "/warm.js", "GET", 200, "HIT", 100, 0.0, None, true,
            ));
        }
        store.insert_batch(&events).await.unwrap();

        let assets = store
            .query_top_assets("2000-01-01T00:00:00Z", "2099-01-01T00:00:00Z", 10)
            .await
            .unwrap();
        assert_eq!(assets[0].path, "/hot.js");
        assert_eq!(assets[0].requests, 10);
    }

    #[tokio::test]
    async fn test_store_prune() {
        let store = AnalyticsStore::open_in_memory().await.unwrap();

        let mut old = make_event("a.com", "/old.js", "GET", 200, "HIT", 100, 0.0, None, true);
        old.timestamp = Utc::now() - chrono::Duration::days(30);
        let new = make_event("a.com", "/new.js", "GET", 200, "HIT", 100, 0.0, None, true);

        store.insert_batch(&[old, new]).await.unwrap();
        let deleted = store.prune(7).await.unwrap();
        assert_eq!(deleted, 1);
    }
}
