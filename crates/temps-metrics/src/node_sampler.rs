// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::MetricsError;
    use crate::store::{LabelledMetric, LatestByLabelQuery, LatestQuery, MetricPoint, RangeQuery};
    use async_trait::async_trait;
    use chrono::{DateTime, Utc};
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Mutex;

    /// A `MetricsStore` that records what it was handed, or fails every write
    /// when constructed with `failing`. Only `write_batch` is exercised by the
    /// sampler; the read methods are unreachable here and return `NotImplemented`.
    struct SpyStore {
        written: Mutex<Vec<MetricPoint>>,
        failing: bool,
    }

    impl SpyStore {
        fn recording() -> Self {
            Self {
                written: Mutex::new(Vec::new()),
                failing: false,
            }
        }

        fn failing() -> Self {
            Self {
                written: Mutex::new(Vec::new()),
                failing: true,
            }
        }

        fn points(&self) -> Vec<MetricPoint> {
            self.written.lock().expect("spy store mutex").clone()
        }
    }

    #[async_trait]
    impl MetricsStore for SpyStore {
        async fn write_batch(&self, points: Vec<MetricPoint>) -> Result<(), MetricsError> {
            if self.failing {
                return Err(MetricsError::NotImplemented);
            }
            self.written.lock().expect("spy store mutex").extend(points);
            Ok(())
        }

        async fn query_range(
            &self,
            _filter: RangeQuery,
        ) -> Result<Vec<(DateTime<Utc>, f64)>, MetricsError> {
            Err(MetricsError::NotImplemented)
        }

        async fn query_latest(
            &self,
            _filter: LatestQuery,
        ) -> Result<HashMap<String, f64>, MetricsError> {
            Err(MetricsError::NotImplemented)
        }

        async fn query_latest_by_label(
            &self,
            _filter: LatestByLabelQuery,
        ) -> Result<Vec<LabelledMetric>, MetricsError> {
            Err(MetricsError::NotImplemented)
        }

        async fn latest_timestamp(
            &self,
            _source_kind: SourceKind,
            _source_id: i32,
        ) -> Result<Option<DateTime<Utc>>, MetricsError> {
            Err(MetricsError::NotImplemented)
        }

        async fn prune(&self, _older_than: DateTime<Utc>) -> Result<u64, MetricsError> {
            Err(MetricsError::NotImplemented)
        }
    }

    /// `ConfigService` needs a `DatabaseConnection`, but `sample_once` never
    /// reads settings (only `run`'s interval lookup does), so a `MockDatabase`
    /// with no queued results is sufficient and keeps these tests hermetic.
    fn test_config_service() -> Arc<temps_config::ConfigService> {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:3000".to_string(),
                "postgresql://unused@localhost/unused".to_string(),
                None,
                None,
            )
            .expect("test ServerConfig"),
        );
        Arc::new(temps_config::ConfigService::new(config, db))
    }

    #[tokio::test]
    async fn sample_once_writes_control_plane_node_scoped_points() {
        let store = Arc::new(SpyStore::recording());
        let sampler = NodeMetricsSampler::new(
            store.clone() as Arc<dyn MetricsStore>,
            test_config_service(),
            PathBuf::from("/tmp"),
        );

        sampler.sample_once().await;

        let points = store.points();
        // Disk metrics come from statvfs and are available on every unix, so
        // at least one point must land even where /proc is absent (macOS).
        assert!(
            !points.is_empty(),
            "expected at least the statvfs-backed disk points"
        );

        for p in &points {
            assert_eq!(p.source_kind, SourceKind::Node, "metric {}", p.name);
            assert_eq!(p.source_id, CONTROL_PLANE_NODE_ID, "metric {}", p.name);
            assert_eq!(p.node_id, Some(CONTROL_PLANE_NODE_ID), "metric {}", p.name);
            assert!(p.name.starts_with("node."), "unexpected name {}", p.name);
        }
    }

    /// The fd metrics are the point of this sampler, so assert they actually
    /// reach the store — but only where the kernel interfaces backing them
    /// exist, matching the collector's documented graceful degradation.
    #[tokio::test]
    async fn sample_once_writes_fd_metrics_where_proc_is_available() {
        let store = Arc::new(SpyStore::recording());
        let sampler = NodeMetricsSampler::new(
            store.clone() as Arc<dyn MetricsStore>,
            test_config_service(),
            PathBuf::from("/tmp"),
        );

        sampler.sample_once().await;

        let names: Vec<String> = store.points().into_iter().map(|p| p.name).collect();

        if Path::new("/proc/sys/fs/file-nr").exists() {
            assert!(names.iter().any(|n| n == "node.fd_percent"));
        }
        if Path::new("/proc/self/fd").exists() {
            assert!(names.iter().any(|n| n == "node.process_open_fds"));
        }
    }

    /// A store outage must not propagate: the sampler logs and drops the batch
    /// so the loop keeps running and the next cycle supersedes it. Mirrors
    /// `ProxyMetricsSampler`'s contract.
    #[tokio::test]
    async fn sample_once_swallows_write_failures() {
        let store = Arc::new(SpyStore::failing());
        let sampler = NodeMetricsSampler::new(
            store.clone() as Arc<dyn MetricsStore>,
            test_config_service(),
            PathBuf::from("/tmp"),
        );

        // Must return normally rather than panicking or propagating.
        sampler.sample_once().await;
        assert!(store.points().is_empty());
    }

    /// An unreadable `data_dir` yields no disk points; on a platform without
    /// `/proc` that leaves nothing to write, and the sampler must short-circuit
    /// rather than hand the store an empty batch.
    #[tokio::test]
    async fn sample_once_skips_write_when_nothing_collected() {
        let store = Arc::new(SpyStore::recording());
        let sampler = NodeMetricsSampler::new(
            store.clone() as Arc<dyn MetricsStore>,
            test_config_service(),
            PathBuf::from("/this/path/does/not/exist/ever"),
        );

        sampler.sample_once().await;

        // Where /proc exists the fd/cpu/memory points still land; where it
        // doesn't, the batch is empty and no write should have happened.
        if !Path::new("/proc").exists() {
            assert!(
                store.points().is_empty(),
                "no collectable metrics should mean no write"
            );
        }
    }
}
