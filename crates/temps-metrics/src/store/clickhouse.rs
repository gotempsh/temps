use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::collections::HashMap;

use crate::error::MetricsError;
use crate::store::{
    LabelledMetric, LatestByLabelQuery, LatestQuery, MetricPoint, MetricsStore, RangeQuery,
    SourceKind,
};

/// Stub ClickHouse metrics store. Not yet implemented.
///
/// All methods return [`MetricsError::NotImplemented`]. This type exists so
/// the ClickHouse backend can be wired into the binary without code changes —
/// only the storage layer needs to be filled in.
pub struct ClickhouseMetricsStore;

impl ClickhouseMetricsStore {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ClickhouseMetricsStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MetricsStore for ClickhouseMetricsStore {
    async fn write_batch(&self, _points: Vec<MetricPoint>) -> Result<(), MetricsError> {
        Err(MetricsError::NotImplemented)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{LatestQuery, MetricKind, MetricPoint, RangeQuery, SourceKind};
    use chrono::{Duration, Utc};
    use std::collections::HashMap;

    fn stub_store() -> ClickhouseMetricsStore {
        ClickhouseMetricsStore::new()
    }

    #[tokio::test]
    async fn test_write_batch_returns_not_implemented() {
        let store = stub_store();
        let point = MetricPoint {
            time: Utc::now(),
            source_kind: SourceKind::Database,
            source_id: 1,
            name: "pg.connections_active".to_string(),
            value: 10.0,
            kind: MetricKind::Gauge,
            engine: None,
            environment: None,
            node_id: None,
            labels: HashMap::new(),
        };
        let result = store.write_batch(vec![point]).await;
        assert!(matches!(result, Err(MetricsError::NotImplemented)));
    }

    #[tokio::test]
    async fn test_query_range_returns_not_implemented() {
        let store = stub_store();
        let result = store
            .query_range(RangeQuery {
                source_kind: SourceKind::Database,
                source_id: 1,
                name: "pg.connections_active".to_string(),
                from: Utc::now() - Duration::hours(1),
                to: Utc::now(),
                step: Duration::seconds(30),
                monotonic: false,
            })
            .await;
        assert!(matches!(result, Err(MetricsError::NotImplemented)));
    }

    #[tokio::test]
    async fn test_query_latest_returns_not_implemented() {
        let store = stub_store();
        let result = store
            .query_latest(LatestQuery {
                source_kind: SourceKind::Database,
                source_id: 1,
                names: vec!["pg.connections_active".to_string()],
            })
            .await;
        assert!(matches!(result, Err(MetricsError::NotImplemented)));
    }

    #[tokio::test]
    async fn test_prune_returns_not_implemented() {
        let store = stub_store();
        let result = store.prune(Utc::now()).await;
        assert!(matches!(result, Err(MetricsError::NotImplemented)));
    }
}
