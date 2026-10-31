// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Turns a worker's heartbeat `capacity` snapshot into `node.*` metric points.
//!
//! The control plane samples itself with [`crate::NodeMetricsCollector`] and
//! stores the result under `node_id = 0`. Workers cannot be sampled from here,
//! but every heartbeat already carries a `capacity` JSON (built by the agent's
//! `CapacitySampler` in `temps-node-agent`). Converting that
//! snapshot into the *same metric names* the control plane uses means the
//! existing `/nodes/{id}/metrics*` endpoints, the monitoring page and the
//! alert evaluator work for a worker with no further changes, and it works
//! with agents that were deployed before this existed.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::Semaphore;
use tracing::warn;

use crate::store::{MetricKind, MetricPoint, MetricsStore, SourceKind};

/// The series a heartbeat writes. A worker's metrics report
/// ([`crate::worker_report`]) carries the same host gauges, and the control
/// plane keeps only the heartbeat's copy, so each series has one writer and
/// agents older than the report still fill it.
pub const CAPACITY_METRIC_NAMES: &[&str] = &[
    "node.cpu_percent",
    "node.memory_used_bytes",
    "node.memory_total_bytes",
    "node.memory_percent",
    "node.disk_used_bytes",
    "node.disk_total_bytes",
    "node.disk_percent",
];

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
    record_heartbeat(store, node_id, capacity, Vec::new(), time).await
}

/// [`record_capacity`] plus `extra` points from the same heartbeat (its disk
/// health, say), written in the same batch.
async fn record_heartbeat(
    store: &dyn MetricsStore,
    node_id: i32,
    capacity: &serde_json::Value,
    extra: Vec<MetricPoint>,
    time: DateTime<Utc>,
) -> usize {
    let mut points = capacity_to_points(node_id, capacity, time);
    points.extend(extra);
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

/// Most heartbeat metric writes that may be in flight at once.
///
/// A heartbeat arrives every 30 s per node, so writes only overlap when one
/// takes longer than that, and then in proportion to `nodes x latency / 30 s`.
/// The cap keeps a slow or stalled metrics store from turning that into an
/// unbounded pile of tasks and open store requests. 32 is far above what a
/// healthy store needs (a fleet of 32 nodes writing at the same instant) and
/// small enough to be a rounding error on a 4 GB host.
pub const MAX_IN_FLIGHT_CAPACITY_WRITES: usize = 32;

/// Longest a single capacity write may hold one of those slots.
///
/// Neither metrics backend is guaranteed to enforce a deadline of its own, so
/// without this a store that accepts the connection and then stalls would
/// keep every slot occupied forever and every later sample would be skipped.
/// A write takes milliseconds when the store is healthy; the next heartbeat
/// arrives 30 s later, so 10 s leaves ample room before it matters.
pub const CAPACITY_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// Samples skipped because the cap was reached, since the process started.
static SKIPPED_CAPACITY_WRITES: AtomicU64 = AtomicU64::new(0);

fn write_limit() -> &'static Arc<Semaphore> {
    static LIMIT: OnceLock<Arc<Semaphore>> = OnceLock::new();
    LIMIT.get_or_init(|| Arc::new(Semaphore::new(MAX_IN_FLIGHT_CAPACITY_WRITES)))
}

/// Record one heartbeat's capacity in a background task, never waiting.
///
/// Returns `false` when the sample was skipped because
/// [`MAX_IN_FLIGHT_CAPACITY_WRITES`] writes are already running. Skipping is
/// the saturation policy: the node's next heartbeat, 30 s later, supersedes
/// the lost sample, so the cost is at worst a one-point gap in a chart, and
/// the alternative (queueing) would grow memory without bound. A write that
/// outlives [`CAPACITY_WRITE_TIMEOUT`] is abandoned and its slot freed, so a
/// stalled store cannot hold the cap indefinitely. Skips are
/// counted and logged on a doubling schedule (1st, 2nd, 4th, 8th, ...) so a
/// stalled store does not also flood the log.
///
/// Must be called from within a Tokio runtime.
pub fn spawn_record_capacity(
    store: Arc<dyn MetricsStore>,
    node_id: i32,
    capacity: serde_json::Value,
) -> bool {
    spawn_record_capacity_with(write_limit().clone(), store, node_id, capacity)
}

/// [`spawn_record_capacity`] with `extra` points from the same heartbeat
/// (the worker's disk health) stored in the same write, under the same cap
/// and deadline, so they never cost the heartbeat a second store round trip.
pub fn spawn_record_heartbeat(
    store: Arc<dyn MetricsStore>,
    node_id: i32,
    capacity: serde_json::Value,
    extra: Vec<MetricPoint>,
) -> bool {
    spawn_record_bounded(
        write_limit().clone(),
        CAPACITY_WRITE_TIMEOUT,
        store,
        node_id,
        capacity,
        extra,
    )
}

fn spawn_record_capacity_with(
    limit: Arc<Semaphore>,
    store: Arc<dyn MetricsStore>,
    node_id: i32,
    capacity: serde_json::Value,
) -> bool {
    spawn_record_capacity_bounded(limit, CAPACITY_WRITE_TIMEOUT, store, node_id, capacity)
}

fn spawn_record_capacity_bounded(
    limit: Arc<Semaphore>,
    deadline: Duration,
    store: Arc<dyn MetricsStore>,
    node_id: i32,
    capacity: serde_json::Value,
) -> bool {
    spawn_record_bounded(limit, deadline, store, node_id, capacity, Vec::new())
}

fn spawn_record_bounded(
    limit: Arc<Semaphore>,
    deadline: Duration,
    store: Arc<dyn MetricsStore>,
    node_id: i32,
    capacity: serde_json::Value,
    extra: Vec<MetricPoint>,
) -> bool {
    let Ok(permit) = limit.try_acquire_owned() else {
        let skipped = SKIPPED_CAPACITY_WRITES.fetch_add(1, Ordering::Relaxed) + 1;
        if skipped.is_power_of_two() {
            warn!(
                node_id,
                skipped_total = skipped,
                max_in_flight = MAX_IN_FLIGHT_CAPACITY_WRITES,
                "Skipped a heartbeat metrics write for node {node_id}: {MAX_IN_FLIGHT_CAPACITY_WRITES} \
                 writes are already in flight, so the metrics store is slower than the heartbeat rate"
            );
        }
        return false;
    };
    tokio::spawn(async move {
        // Held for the whole write; released when the task ends, including
        // when the deadline abandons a write the store never completes.
        let _permit = permit;
        let write = record_heartbeat(store.as_ref(), node_id, &capacity, extra, Utc::now());
        if tokio::time::timeout(deadline, write).await.is_err() {
            warn!(
                node_id,
                timeout_ms = deadline.as_millis() as u64,
                "Abandoned the heartbeat metrics write for node {node_id}: the metrics store did not \
                 answer within {}s, so this sample is lost and its slot was freed",
                deadline.as_secs_f32()
            );
        }
    });
    true
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
        // The list the worker report defers to is exactly what is written.
        assert_eq!(names(&points), CAPACITY_METRIC_NAMES);
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
        /// When set, every write waits for a permit from this semaphore, so a
        /// test can hold writes in flight and release them on demand.
        gate: Option<Arc<Semaphore>>,
    }

    #[async_trait]
    impl MetricsStore for SpyStore {
        async fn write_batch(&self, points: Vec<MetricPoint>) -> Result<(), MetricsError> {
            if let Some(gate) = &self.gate {
                gate.acquire()
                    .await
                    .map_err(|_| MetricsError::NotImplemented)?
                    .forget();
            }
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
            gate: None,
        }
    }

    #[tokio::test]
    async fn heartbeat_extra_points_share_the_capacity_write() {
        let store = spy(false);
        let mut extra = capacity_to_points(9, &json!({ "cpu_percent": 1.0 }), Utc::now());
        extra[0].name = "node.raid_degraded".to_string();
        let written = record_heartbeat(
            &store,
            9,
            &json!({ "cpu_percent": 5.0 }),
            extra.clone(),
            Utc::now(),
        )
        .await;
        assert_eq!(written, 2, "capacity and disk health in one batch");
        assert_eq!(
            names(&store.written.lock().expect("spy store mutex")),
            vec!["node.cpu_percent", "node.raid_degraded"]
        );

        // A heartbeat without usable capacity still stores its extra points.
        let store = spy(false);
        assert_eq!(
            record_heartbeat(&store, 9, &json!({}), extra, Utc::now()).await,
            1
        );
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

    fn gated_store() -> (Arc<SpyStore>, Arc<Semaphore>) {
        let gate = Arc::new(Semaphore::new(0));
        let store = Arc::new(SpyStore {
            written: Mutex::new(Vec::new()),
            failing: false,
            gate: Some(gate.clone()),
        });
        (store, gate)
    }

    /// Wait (bounded) until `done` holds, yielding to the spawned writers.
    async fn eventually(mut done: impl FnMut() -> bool) {
        for _ in 0..200 {
            if done() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("condition not reached within 2s");
    }

    #[tokio::test]
    async fn writes_beyond_the_cap_are_skipped_not_queued() {
        let (store, gate) = gated_store();
        let dyn_store: Arc<dyn MetricsStore> = store.clone();
        let limit = Arc::new(Semaphore::new(2));
        let beat = json!({ "cpu_percent": 5.0 });

        // Two writes fill the cap and stay in flight (the store is gated).
        assert!(spawn_record_capacity_with(
            limit.clone(),
            dyn_store.clone(),
            1,
            beat.clone()
        ));
        assert!(spawn_record_capacity_with(
            limit.clone(),
            dyn_store.clone(),
            2,
            beat.clone()
        ));
        // The third is dropped immediately and never reaches the store.
        assert!(!spawn_record_capacity_with(
            limit.clone(),
            dyn_store.clone(),
            3,
            beat.clone()
        ));

        gate.add_permits(10);
        eventually(|| store.written.lock().expect("spy store mutex").len() == 2).await;
        let written = store.written.lock().expect("spy store mutex");
        let mut nodes: Vec<i32> = written.iter().map(|p| p.source_id).collect();
        nodes.sort_unstable();
        assert_eq!(nodes, [1, 2], "the skipped sample must not be written");
    }

    #[tokio::test]
    async fn capacity_is_released_when_a_write_finishes() {
        let (store, gate) = gated_store();
        let dyn_store: Arc<dyn MetricsStore> = store.clone();
        let limit = Arc::new(Semaphore::new(1));
        let beat = json!({ "cpu_percent": 5.0 });

        assert!(spawn_record_capacity_with(
            limit.clone(),
            dyn_store.clone(),
            1,
            beat.clone()
        ));
        assert!(!spawn_record_capacity_with(
            limit.clone(),
            dyn_store.clone(),
            1,
            beat.clone()
        ));

        gate.add_permits(10);
        eventually(|| limit.available_permits() == 1).await;

        assert!(
            spawn_record_capacity_with(limit.clone(), dyn_store, 1, beat),
            "a finished write must free its slot"
        );
    }

    #[tokio::test]
    async fn a_failing_store_still_releases_its_slot() {
        let store = Arc::new(SpyStore {
            written: Mutex::new(Vec::new()),
            failing: true,
            gate: None,
        });
        let dyn_store: Arc<dyn MetricsStore> = store;
        let limit = Arc::new(Semaphore::new(1));

        assert!(spawn_record_capacity_with(
            limit.clone(),
            dyn_store,
            1,
            json!({ "cpu_percent": 5.0 })
        ));
        eventually(|| limit.available_permits() == 1).await;
    }

    #[tokio::test]
    async fn a_hung_write_is_abandoned_and_frees_its_slot() {
        // The gate is never opened, so the store never answers.
        let (store, _gate) = gated_store();
        let dyn_store: Arc<dyn MetricsStore> = store.clone();
        let limit = Arc::new(Semaphore::new(1));
        let beat = json!({ "cpu_percent": 5.0 });

        assert!(spawn_record_capacity_bounded(
            limit.clone(),
            Duration::from_millis(50),
            dyn_store.clone(),
            1,
            beat.clone()
        ));
        assert!(
            !spawn_record_capacity_bounded(
                limit.clone(),
                Duration::from_millis(50),
                dyn_store.clone(),
                1,
                beat.clone()
            ),
            "the hung write still holds the slot until its deadline"
        );

        eventually(|| limit.available_permits() == 1).await;

        assert!(
            spawn_record_capacity_bounded(
                limit.clone(),
                Duration::from_millis(50),
                dyn_store,
                1,
                beat
            ),
            "a timed-out write must free its slot for the next heartbeat"
        );
        assert!(
            store.written.lock().expect("spy store mutex").is_empty(),
            "nothing was written by the abandoned writes"
        );
    }
}
