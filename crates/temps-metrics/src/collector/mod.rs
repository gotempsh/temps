//! Metric collector trait and configuration types.
//!
//! Each collector is responsible for connecting to a single external service
//! and returning a batch of [`MetricPoint`]s on demand. Collectors are
//! stateless with respect to connections — they open a fresh connection per
//! [`Collector::collect`] call so the scraper loop can freely time-out and
//! retry without worrying about stale connection state.
//!
//! Counter delta computation (current − previous) is the responsibility of
//! the `MetricsScraper`, not the collector. Collectors always return raw
//! cumulative values for counter metrics and indicate the kind via
//! [`MetricKind::Counter`]. The scraper tracks `last_values` and converts
//! cumulative readings into deltas before calling [`MetricsStore::write_batch`].

pub mod mongodb;
pub mod node;
pub mod postgres;
pub mod prometheus;
pub mod redis;
pub mod s3;

use async_trait::async_trait;
use std::time::Duration;

use crate::error::MetricsError;
use crate::store::{MetricPoint, SourceKind};

/// Configuration passed to every [`Collector::collect`] call.
///
/// All fields are cloneable so the scraper can hand a copy to each collector
/// without sharing ownership.
#[derive(Debug, Clone)]
pub struct CollectorConfig {
    /// Primary-key ID of the entity in the table indicated by `source_kind`.
    pub source_id: i32,
    /// Which entity table `source_id` refers to.
    pub source_kind: SourceKind,
    /// Connection string for the target service (e.g. a libpq DSN or a
    /// mongodb:// URI).
    pub connection_string: String,
    /// Deployment environment name, forwarded into every metric's labels
    /// (e.g. `"production"`, `"staging"`).
    pub environment: Option<String>,
    /// Node ID, forwarded into every metric's `node_id` field when set.
    pub node_id: Option<i32>,
    /// Per-collection timeout. Any operation that exceeds this deadline causes
    /// the collector to return an empty `Vec` and log a warning rather than
    /// propagating the error — the scraper loop must not block on a slow host.
    pub timeout: Duration,
}

impl CollectorConfig {
    /// Convenience constructor with a fixed 5-second timeout (the recommended
    /// production default).
    pub fn new(
        source_id: i32,
        source_kind: SourceKind,
        connection_string: impl Into<String>,
    ) -> Self {
        Self {
            source_id,
            source_kind,
            connection_string: connection_string.into(),
            environment: None,
            node_id: None,
            timeout: Duration::from_secs(5),
        }
    }

    /// Override the default timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Attach a deployment environment label.
    pub fn with_environment(mut self, env: impl Into<String>) -> Self {
        self.environment = Some(env.into());
        self
    }

    /// Attach a node identifier.
    pub fn with_node_id(mut self, node_id: i32) -> Self {
        self.node_id = Some(node_id);
        self
    }
}

/// A source-specific metrics collector.
///
/// Implementors must be `Send + Sync` so they can be held inside an `Arc` and
/// shared across async tasks.
///
/// # Contract
///
/// - `collect()` **must not panic** on connection failures or slow responses.
///   Return `Ok(vec![])` and emit a `tracing::warn!` instead.
/// - `collect()` **must** respect `config.timeout` — abort the entire
///   collection if the deadline fires.
/// - Returned [`MetricPoint`]s for counter metrics carry **raw cumulative
///   values**. The caller (scraper) is responsible for delta computation.
#[async_trait]
pub trait Collector: Send + Sync {
    /// Connect to the service described by `config`, run all metric queries,
    /// and return the resulting points.
    ///
    /// On any transient error (connection refused, timeout, auth failure) the
    /// implementation logs a warning and returns `Ok(vec![])`. Only a
    /// structural programming error (e.g. a bug in the SQL template) should
    /// return `Err(MetricsError)`.
    async fn collect(&self, config: &CollectorConfig) -> Result<Vec<MetricPoint>, MetricsError>;

    /// Short lowercase identifier for this engine (e.g. `"postgres"`,
    /// `"mongodb"`). Used in metric labels and log messages.
    fn engine(&self) -> &'static str;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SourceKind;

    #[test]
    fn collector_config_defaults() {
        let cfg = CollectorConfig::new(42, SourceKind::Database, "postgres://localhost/mydb");
        assert_eq!(cfg.source_id, 42);
        assert_eq!(cfg.timeout, Duration::from_secs(5));
        assert!(cfg.environment.is_none());
        assert!(cfg.node_id.is_none());
    }

    #[test]
    fn collector_config_builder() {
        let cfg = CollectorConfig::new(1, SourceKind::Database, "conn")
            .with_timeout(Duration::from_secs(10))
            .with_environment("production")
            .with_node_id(7);
        assert_eq!(cfg.timeout, Duration::from_secs(10));
        assert_eq!(cfg.environment.as_deref(), Some("production"));
        assert_eq!(cfg.node_id, Some(7));
    }
}
