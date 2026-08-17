//! Background sampler that periodically runs [`NodeMetricsCollector`] and
//! persists its points to the metrics store as control-plane node metrics.
//!
//! Mirrors the scheduling pattern of temps-proxy's
//! `ProxyMetricsSampler`: re-reads the scrape interval each cycle so
//! operators can tune `monitoring.scrape_interval_secs` without a restart,
//! and a failed write is logged and dropped rather than retried inline — the
//! next cycle's point simply supersedes it. `NodeMetricsCollector` itself was
//! already implemented and unit-tested, but nothing scheduled it: this is the
//! missing piece that actually gets `node.*` metrics (cpu/memory/disk/fd
//! percent) flowing into the store so `monitoring_alert_rules` on `node_id`
//! have data to evaluate.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, warn};

use crate::collector::node::NodeMetricsCollector;
use crate::collector::{Collector, CollectorConfig};
use crate::store::{MetricsStore, SourceKind};

/// Synthetic node ID representing the control plane. Mirrors
/// `CONTROL_PLANE_NODE_ID` in temps-proxy's metrics sampler and
/// temps-deployments' nodes handler.
pub const CONTROL_PLANE_NODE_ID: i32 = 0;

/// Floor for the sampling interval, matching the validation floor for
/// `monitoring.scrape_interval_secs` in temps-config.
const MIN_SAMPLE_INTERVAL_SECS: u64 = 15;

/// Fallback when settings cannot be read (e.g. transient DB outage).
const DEFAULT_SAMPLE_INTERVAL_SECS: u64 = 30;

/// Periodically collects node-level system metrics (CPU, memory, disk, file
/// descriptors) and flushes them to the metrics store.
pub struct NodeMetricsSampler {
    collector: NodeMetricsCollector,
    store: Arc<dyn MetricsStore>,
    config_service: Arc<temps_config::ConfigService>,
    /// Filesystem path whose disk usage is monitored — the collector's
    /// `CollectorConfig::connection_string` field is repurposed to carry
    /// this, mirroring `NodeMetricsCollector`'s own doc comment.
    data_dir: PathBuf,
}

impl NodeMetricsSampler {
    pub fn new(
        store: Arc<dyn MetricsStore>,
        config_service: Arc<temps_config::ConfigService>,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            collector: NodeMetricsCollector::new(),
            store,
            config_service,
            data_dir,
        }
    }

    /// Run the sampling loop forever. Spawn on a dedicated background task or
    /// thread-local runtime (mirrors the proxy metrics sampler pattern).
    pub async fn run(self) {
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
                        "NodeMetricsSampler: cannot read settings ({e}); using default interval"
                    );
                    DEFAULT_SAMPLE_INTERVAL_SECS
                }
            };
            tokio::time::sleep(Duration::from_secs(interval_secs)).await;
            self.sample_once().await;
        }
    }

    /// One collect → write cycle. Extracted from [`Self::run`] so
    /// integration tests can drive discrete cycles against a real store.
    pub async fn sample_once(&self) {
        let config = CollectorConfig::new(
            CONTROL_PLANE_NODE_ID,
            SourceKind::Node,
            self.data_dir.to_string_lossy().to_string(),
        )
        .with_node_id(CONTROL_PLANE_NODE_ID);

        let points = match self.collector.collect(&config).await {
            Ok(points) => points,
            Err(e) => {
                warn!("NodeMetricsSampler: collection failed (non-fatal): {e}");
                return;
            }
        };

        if points.is_empty() {
            return;
        }

        if let Err(e) = self.store.write_batch(points).await {
            // Non-fatal: the next cycle's point supersedes it, same
            // graceful-degradation contract as ProxyMetricsSampler.
            warn!("NodeMetricsSampler: write_batch failed (non-fatal): {e}");
        }
    }
}
