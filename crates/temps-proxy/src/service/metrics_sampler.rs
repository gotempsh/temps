//! Background sampler that persists proxy hot-path metrics.
//!
//! Owns the only I/O in the proxy metrics pipeline: every interval it
//! snapshots the lock-free [`ProxyMetrics`] counters, computes the delta since
//! the previous snapshot, and writes the resulting points to the metrics store
//! (`SourceKind::Node`, control-plane node) in a single `write_batch` call.
//!
//! Storage cost is constant regardless of request rate: ~11 points per
//! interval, whether the proxy served 10 or 10 million requests. The store
//! write happens on this background task only — never on the Pingora request
//! path. A failed write is logged and dropped; the counters keep accumulating
//! so the next successful cycle re-converges (with one wider delta).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tracing::{debug, warn};

use temps_metrics::{MetricKind, MetricPoint, MetricsStore, SourceKind};

use crate::metrics::{MetricsSnapshot, ProxyMetrics};

/// Synthetic node ID representing the control plane, where the proxy runs.
/// Mirrors `CONTROL_PLANE_NODE_ID` in `temps-deployments`' nodes handler so
/// `GET /nodes/0/metrics` returns these points.
pub const CONTROL_PLANE_NODE_ID: i32 = 0;

/// Floor for the sampling interval, matching the validation floor for
/// `monitoring.scrape_interval_secs` in temps-config.
const MIN_SAMPLE_INTERVAL_SECS: u64 = 15;

/// Fallback when settings cannot be read (e.g. transient DB outage).
const DEFAULT_SAMPLE_INTERVAL_SECS: u64 = 30;

/// Periodically flushes [`ProxyMetrics`] deltas to the metrics store.
pub struct ProxyMetricsSampler {
    metrics: Arc<ProxyMetrics>,
    store: Arc<dyn MetricsStore>,
    config_service: Arc<temps_config::ConfigService>,
}

impl ProxyMetricsSampler {
    pub fn new(
        metrics: Arc<ProxyMetrics>,
        store: Arc<dyn MetricsStore>,
        config_service: Arc<temps_config::ConfigService>,
    ) -> Self {
        Self {
            metrics,
            store,
            config_service,
        }
    }

    /// Run the sampling loop forever. Spawn on a dedicated background task or
    /// thread-local runtime (mirrors the proxy-log batch writer pattern).
    pub async fn run(self) {
        let mut last_snapshot = MetricsSnapshot::default();

        loop {
            // Re-read the interval each cycle so operators can tune
            // monitoring.scrape_interval_secs at runtime without a restart.
            let interval_secs = match self.config_service.get_settings().await {
                Ok(settings) => settings
                    .monitoring
                    .scrape_interval_secs
                    .max(MIN_SAMPLE_INTERVAL_SECS),
                Err(e) => {
                    debug!(
                        "ProxyMetricsSampler: cannot read settings ({e}); using default interval"
                    );
                    DEFAULT_SAMPLE_INTERVAL_SECS
                }
            };
            tokio::time::sleep(Duration::from_secs(interval_secs)).await;
            self.sample_once(&mut last_snapshot).await;
        }
    }

    /// One snapshot → delta → write cycle. Extracted from [`Self::run`] so
    /// integration tests can drive discrete cycles against a real store.
    pub async fn sample_once(&self, last_snapshot: &mut MetricsSnapshot) {
        let snapshot = self.metrics.snapshot();
        let delta = snapshot.delta_since(last_snapshot);
        *last_snapshot = snapshot;

        let points = build_points(&delta.samples());
        if points.is_empty() {
            return;
        }

        if let Err(e) = self.store.write_batch(points).await {
            // Non-fatal: counters keep accumulating; the next successful
            // cycle writes a wider delta and the series re-converges.
            warn!("ProxyMetricsSampler: write_batch failed (non-fatal): {e}");
        }
    }
}

/// Select the metrics store backend the proxy writes to.
///
/// Mirrors the console's selection exactly (settings `monitoring.store` +
/// `TEMPS_CLICKHOUSE_*` availability) so proxy writes land in the same store
/// the console's read handlers and alert evaluator query — critical in the
/// split proxy/console topology where the store is the only meeting point.
/// ClickHouse table migrations are owned by the console; if the proxy writes
/// before they ran, the per-cycle write fails non-fatally and retries next
/// interval.
pub async fn build_metrics_store(
    config_service: &Arc<temps_config::ConfigService>,
    config: &temps_config::ServerConfig,
    db: Arc<temps_database::DbConnection>,
) -> Arc<dyn MetricsStore> {
    use temps_core::MetricsStoreKind;
    use temps_metrics::{ClickHouseMetricsConfig, ClickhouseMetricsStore, TimescaleMetricsStore};

    let kind = match config_service.get_settings().await {
        Ok(settings) => settings.monitoring.store,
        Err(e) => {
            warn!("ProxyMetricsSampler: cannot read monitoring settings ({e}); using TimescaleDB");
            MetricsStoreKind::TimescaleDb
        }
    };

    match kind {
        MetricsStoreKind::ClickHouse if config_service.is_clickhouse_enabled() => {
            // is_clickhouse_enabled() guarantees the fields are Some.
            Arc::new(ClickhouseMetricsStore::new(ClickHouseMetricsConfig::new(
                config.clickhouse_url.clone().unwrap_or_default(),
                config.clickhouse_database.clone().unwrap_or_default(),
                config.clickhouse_user.clone().unwrap_or_default(),
                config.clickhouse_password.clone().unwrap_or_default(),
            )))
        }
        MetricsStoreKind::ClickHouse | MetricsStoreKind::TimescaleDb => {
            Arc::new(TimescaleMetricsStore::new(db))
        }
    }
}

/// Convert interval samples into store points for the control-plane node.
fn build_points(samples: &[crate::metrics::ProxySample]) -> Vec<MetricPoint> {
    let now = Utc::now();
    samples
        .iter()
        .map(|s| MetricPoint {
            time: now,
            source_kind: SourceKind::Node,
            source_id: CONTROL_PLANE_NODE_ID,
            name: s.name.to_string(),
            value: s.value,
            kind: if s.is_counter {
                MetricKind::Counter
            } else {
                MetricKind::Gauge
            },
            engine: Some("proxy".to_string()),
            environment: None,
            node_id: Some(CONTROL_PLANE_NODE_ID),
            labels: HashMap::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{RequestDestination, METRIC_REQUESTS, METRIC_REQUESTS_5XX};

    #[test]
    fn test_build_points_maps_samples_to_node_points() {
        let metrics = ProxyMetrics::default();
        metrics.record(200, 12, None, RequestDestination::Project);
        metrics.record(502, 340, None, RequestDestination::Console);
        let delta = metrics.snapshot().delta_since(&MetricsSnapshot::default());

        let points = build_points(&delta.samples());
        assert!(!points.is_empty());

        for p in &points {
            assert_eq!(p.source_kind, SourceKind::Node);
            assert_eq!(p.source_id, CONTROL_PLANE_NODE_ID);
            assert_eq!(p.node_id, Some(CONTROL_PLANE_NODE_ID));
            assert_eq!(p.engine.as_deref(), Some("proxy"));
            assert!(p.name.starts_with("proxy."), "unexpected name {}", p.name);
        }

        let requests = points
            .iter()
            .find(|p| p.name == METRIC_REQUESTS)
            .expect("requests point present");
        assert_eq!(requests.value, 2.0);
        assert_eq!(requests.kind, MetricKind::Counter);

        let errors = points
            .iter()
            .find(|p| p.name == METRIC_REQUESTS_5XX)
            .expect("5xx point present");
        assert_eq!(errors.value, 1.0);
    }

    #[test]
    fn test_build_points_empty_samples() {
        assert!(build_points(&[]).is_empty());
    }

    /// Full pipeline integration: record on the hot-path counters → sampler
    /// cycle → TimescaleDB store → read back via the same `query_latest` the
    /// alert evaluator and (via `query_range`) the `/nodes/{id}/metrics`
    /// endpoint use. Skips gracefully when no test Postgres is available,
    /// per the repo's Docker-test convention.
    #[tokio::test]
    async fn test_sampler_pipeline_end_to_end_against_real_store() {
        use temps_database::test_utils::TestDatabase;
        use temps_metrics::{LatestQuery, TimescaleMetricsStore};

        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Test database not available, skipping: {e}");
                return;
            }
        };
        let db = test_db.connection_arc().clone();

        let metrics = Arc::new(ProxyMetrics::default());
        let store: Arc<dyn MetricsStore> = Arc::new(TimescaleMetricsStore::new(db.clone()));
        let config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:3000".to_string(),
                "postgresql://unused@localhost/unused".to_string(),
                None,
                None,
            )
            .expect("test ServerConfig"),
        );
        let config_service = Arc::new(temps_config::ConfigService::new(config, db));
        let sampler =
            ProxyMetricsSampler::new(Arc::clone(&metrics), Arc::clone(&store), config_service);

        // ── Cycle 1: mixed traffic ────────────────────────────────────────
        metrics.record(200, 100, Some(80), RequestDestination::Project);
        metrics.record(200, 40, Some(30), RequestDestination::Project);
        metrics.record(404, 5, None, RequestDestination::Console);
        metrics.record(502, 60, Some(55), RequestDestination::Project);

        let mut last_snapshot = MetricsSnapshot::default();
        sampler.sample_once(&mut last_snapshot).await;

        let latest = store
            .query_latest(LatestQuery {
                source_kind: SourceKind::Node,
                source_id: CONTROL_PLANE_NODE_ID,
                names: vec![],
            })
            .await
            .expect("query_latest after first cycle");

        assert_eq!(latest["proxy.requests"], 4.0);
        assert_eq!(latest["proxy.requests_2xx"], 2.0);
        assert_eq!(latest["proxy.requests_4xx"], 1.0);
        assert_eq!(latest["proxy.requests_5xx"], 1.0);
        // Destination partition read back intact.
        assert_eq!(latest["proxy.requests_project"], 3.0);
        assert_eq!(latest["proxy.requests_console"], 1.0);
        assert_eq!(latest["proxy.requests_other"], 0.0);
        // Error rate = 1 of 4.
        assert_eq!(latest["proxy.error_rate_percent"], 25.0);
        // Latency split: backend avg (80+30+55)/3, self avg (20+10+5)/3.
        assert!((latest["proxy.upstream_duration_avg_ms"] - 55.0).abs() < 1e-9);
        assert!((latest["proxy.self_duration_avg_ms"] - (35.0 / 3.0)).abs() < 1e-9);

        // ── Cycle 2: only new traffic must be reported ────────────────────
        metrics.record(200, 10, Some(8), RequestDestination::Project);
        sampler.sample_once(&mut last_snapshot).await;

        let latest = store
            .query_latest(LatestQuery {
                source_kind: SourceKind::Node,
                source_id: CONTROL_PLANE_NODE_ID,
                names: vec![
                    "proxy.requests".to_string(),
                    "proxy.requests_5xx".to_string(),
                ],
            })
            .await
            .expect("query_latest after second cycle");
        assert_eq!(
            latest["proxy.requests"], 1.0,
            "second cycle must be delta-only"
        );
        assert_eq!(latest["proxy.requests_5xx"], 0.0);

        // ── Cycle 3: idle interval still writes zero counters ─────────────
        sampler.sample_once(&mut last_snapshot).await;
        let latest = store
            .query_latest(LatestQuery {
                source_kind: SourceKind::Node,
                source_id: CONTROL_PLANE_NODE_ID,
                names: vec!["proxy.requests".to_string()],
            })
            .await
            .expect("query_latest after idle cycle");
        assert_eq!(
            latest["proxy.requests"], 0.0,
            "idle interval draws a zero, not a gap"
        );
    }
}
