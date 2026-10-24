// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Turns a worker's heartbeat `capacity` snapshot into `node.*` metric points.
//!
//! The control plane samples itself with [`crate::NodeMetricsCollector`] and
//! stores the result under `node_id = 0`. Workers cannot be sampled from here,
//! but every heartbeat already carries a `capacity` JSON (built by the agent's
//! `CapacitySampler` in `temps-agent`). Converting that
//! snapshot into the *same metric names* the control plane uses means the
//! existing `/nodes/{id}/metrics*` endpoints, the monitoring page and the
//! alert evaluator work for a worker with no further changes, and it works
//! with agents that were deployed before this existed.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use tracing::warn;

use crate::store::{MetricKind, MetricPoint, MetricsStore, SourceKind};

/// Build the gauge points for one heartbeat.
///
/// Only values that are present and meaningful are emitted, so a gap in the
/// series always means "the node did not report", never "the node reported
/// zero":
/// - a missing or non-finite field is skipped,
/// - memory and disk percentages need a non-zero total (an agent that could
///   not find a root mount reports `0`/`0`),
/// - `cpu_percent` is clamped to `0..=100`.
///
/// `capacity` is node-supplied and therefore untrusted; nothing here panics on
/// an unexpected shape, it simply yields fewer (or no) points.
pub fn capacity_to_points(
    node_id: i32,
    capacity: &serde_json::Value,
    time: DateTime<Utc>,
) -> Vec<MetricPoint> {
    let mut points = Vec::with_capacity(7);
    let mut push = |name: &str, value: f64| {
        points.push(MetricPoint {
            time,
            source_kind: SourceKind::Node,
            source_id: node_id,
            name: name.to_string(),
            value,
            kind: MetricKind::Gauge,
            engine: None,
            environment: None,
            node_id: Some(node_id),
            labels: HashMap::new(),
        });
    };

    if let Some(cpu) = number(capacity, "cpu_percent") {
        push("node.cpu_percent", cpu.clamp(0.0, 100.0));
    }

    if let (Some(used), Some(total)) = (
        number(capacity, "memory_used_bytes"),
        number(capacity, "memory_total_bytes"),
    ) {
        if total > 0.0 {
            push("node.memory_used_bytes", used);
            push("node.memory_total_bytes", total);
            push("node.memory_percent", percent(used, total));
        }
    }

    if let (Some(used), Some(total)) = (
        number(capacity, "disk_used_bytes"),
        number(capacity, "disk_total_bytes"),
    ) {
        if total > 0.0 {
            push("node.disk_used_bytes", used);
            push("node.disk_total_bytes", total);
            push("node.disk_percent", percent(used, total));
        }
    }

    points
}

/// Persist the points for one heartbeat. Returns how many were written.
///
/// Failure is deliberately non-fatal: a heartbeat is a liveness signal and
/// must not be rejected because the metrics store is briefly unavailable. The
/// next beat (30 s later) supersedes the lost sample, which at worst leaves a
/// one-point gap in a chart.
pub async fn record_capacity(
    store: &dyn MetricsStore,
    node_id: i32,
    capacity: &serde_json::Value,
    time: DateTime<Utc>,
) -> usize {
    let points = capacity_to_points(node_id, capacity, time);
    let count = points.len();
    if count == 0 {
        return 0;
    }
    match store.write_batch(points).await {
        Ok(()) => count,
        Err(e) => {
            warn!(
                node_id,
                points = count,
                error = %e,
                "Failed to store heartbeat metrics for node {node_id}; the next heartbeat will supersede them"
            );
            0
        }
    }
}

fn number(capacity: &serde_json::Value, key: &str) -> Option<f64> {
    capacity
        .get(key)
        .and_then(serde_json::Value::as_f64)
        .filter(|v| v.is_finite() && *v >= 0.0)
}

fn percent(used: f64, total: f64) -> f64 {
    ((used / total) * 100.0).clamp(0.0, 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn names(points: &[MetricPoint]) -> Vec<&str> {
        points.iter().map(|p| p.name.as_str()).collect()
    }

    fn value(points: &[MetricPoint], name: &str) -> f64 {
        points
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("missing metric {name}"))
            .value
    }

    #[test]
    fn full_capacity_yields_the_same_names_the_control_plane_uses() {
        let points = capacity_to_points(
            7,
            &json!({
                "cpu_percent": 42.5,
                "memory_used_bytes": 2_u64 * 1024 * 1024 * 1024,
                "memory_total_bytes": 8_u64 * 1024 * 1024 * 1024,
                "disk_used_bytes": 30_u64,
                "disk_total_bytes": 120_u64,
            }),
            Utc::now(),
        );

        assert_eq!(
            names(&points),
            [
                "node.cpu_percent",
                "node.memory_used_bytes",
                "node.memory_total_bytes",
                "node.memory_percent",
                "node.disk_used_bytes",
                "node.disk_total_bytes",
                "node.disk_percent",
            ]
        );
        assert_eq!(value(&points, "node.cpu_percent"), 42.5);
        assert_eq!(value(&points, "node.memory_percent"), 25.0);
        assert_eq!(value(&points, "node.disk_percent"), 25.0);
    }

    #[test]
    fn points_are_scoped_to_the_reporting_node() {
        let points = capacity_to_points(7, &json!({ "cpu_percent": 1.0 }), Utc::now());

        assert_eq!(points.len(), 1);
        assert_eq!(points[0].source_kind, SourceKind::Node);
        assert_eq!(points[0].source_id, 7);
        assert_eq!(points[0].node_id, Some(7));
        assert_eq!(points[0].kind, MetricKind::Gauge);
    }

    #[test]
    fn zero_totals_emit_no_percentages_so_a_gap_is_never_read_as_zero_usage() {
        let points = capacity_to_points(
            3,
            &json!({
                "memory_used_bytes": 0,
                "memory_total_bytes": 0,
                "disk_used_bytes": 0,
                "disk_total_bytes": 0,
            }),
            Utc::now(),
        );

        assert!(points.is_empty(), "got {:?}", names(&points));
    }

    #[test]
    fn cpu_and_percentages_are_clamped_to_a_valid_range() {
        let points = capacity_to_points(
            3,
            &json!({
                "cpu_percent": 480.0,
                "memory_used_bytes": 900,
                "memory_total_bytes": 100,
            }),
            Utc::now(),
        );

        assert_eq!(value(&points, "node.cpu_percent"), 100.0);
        assert_eq!(value(&points, "node.memory_percent"), 100.0);
    }

    #[test]
    fn malformed_capacity_never_panics_and_yields_nothing() {
        for capacity in [
            json!(null),
            json!([]),
            json!("cpu_percent"),
            json!({}),
            json!({ "cpu_percent": "high", "memory_total_bytes": null }),
            json!({ "cpu_percent": -5.0 }),
        ] {
            assert!(
                capacity_to_points(1, &capacity, Utc::now()).is_empty(),
                "capacity {capacity} should not produce points"
            );
        }
    }

    #[test]
    fn partial_capacity_emits_only_what_was_reported() {
        let points = capacity_to_points(
            1,
            &json!({ "cpu_percent": 10.0, "memory_used_bytes": 5 }),
            Utc::now(),
        );

        assert_eq!(names(&points), ["node.cpu_percent"]);
    }

    use crate::error::MetricsError;
    use crate::store::{LabelledMetric, LatestByLabelQuery, LatestQuery, RangeQuery};
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// Records writes, or fails every write when `failing`. Reads are never
    /// reached by `record_capacity`.
    struct SpyStore {
        written: Mutex<Vec<MetricPoint>>,
        failing: bool,
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

    fn spy(failing: bool) -> SpyStore {
        SpyStore {
            written: Mutex::new(Vec::new()),
            failing,
        }
    }

    #[tokio::test]
    async fn record_capacity_writes_one_batch_for_the_node() {
        let store = spy(false);
        let written = record_capacity(
            &store,
            9,
            &json!({ "cpu_percent": 5.0, "memory_used_bytes": 1, "memory_total_bytes": 4 }),
            Utc::now(),
        )
        .await;

        assert_eq!(written, 4);
        let points = store.written.lock().expect("spy store mutex");
        assert!(points
            .iter()
            .all(|p| p.source_id == 9 && p.node_id == Some(9)));
    }

    #[tokio::test]
    async fn record_capacity_swallows_store_failures() {
        let store = spy(true);
        let written = record_capacity(&store, 9, &json!({ "cpu_percent": 5.0 }), Utc::now()).await;

        assert_eq!(written, 0);
    }

    #[tokio::test]
    async fn record_capacity_skips_the_store_when_there_is_nothing_to_write() {
        let store = spy(true);
        // A failing store would be reached if an empty batch were sent.
        let written = record_capacity(&store, 9, &json!({}), Utc::now()).await;

        assert_eq!(written, 0);
        assert!(store.written.lock().expect("spy store mutex").is_empty());
    }
}
