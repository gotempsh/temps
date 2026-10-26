// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Worker metrics reports (ADR 061).
//!
//! A worker's edge proxy and its host are measured on the worker, but only the
//! control plane writes the metrics store. The control plane pulls a
//! [`WorkerMetricsReport`] from each worker's agent over the authenticated
//! agent channel ([`WORKER_METRICS_PATH`]) on the monitoring scrape interval
//! and turns it into store points here.
//!
//! Everything a worker says is treated as untrusted input:
//!
//! - **Bounded size.** The control plane reads at most [`MAX_REPORT_BYTES`];
//!   [`decode_report`] refuses anything larger before parsing it.
//! - **Fixed names.** Proxy traffic arrives as a [`MetricsSnapshot`] — fixed
//!   arrays of counters, no names at all — and becomes exactly the series the
//!   control plane's own proxy writes. Host samples are kept only when their
//!   name is in [`HOST_METRIC_NAMES`], at most once each, and only with a
//!   finite, non-negative value (percentages no higher than 100). No labels,
//!   engines or environments are accepted from the wire.
//! - **One writer per series.** CPU, memory and disk already arrive in every
//!   heartbeat ([`CAPACITY_METRIC_NAMES`]); the report's copies of those are
//!   left out rather than stored twice under the same name.
//! - **Control-plane time.** A report carries no timestamp. Every point is
//!   stamped with the control plane's receive time, so a worker with a wrong
//!   clock cannot move its samples, backfill history or write into the future.
//! - **Counters survive a lost pull.** The worker serves cumulative counters;
//!   the control plane keeps the previous snapshot per node and stores the
//!   difference, so a failed pull only widens the next interval. An agent
//!   restart is recognised by its new `instance_id`, and the new process's
//!   counters (which started at zero) are taken whole.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::collector::node::HOST_METRIC_NAMES;
use crate::node_capacity::CAPACITY_METRIC_NAMES;
use crate::proxy::{proxy_points, MetricsSnapshot};
use crate::store::{MetricKind, MetricPoint, SourceKind};

/// Agent route the control plane pulls a [`WorkerMetricsReport`] from.
pub const WORKER_METRICS_PATH: &str = "/agent/metrics";

/// Report format version. Bumped only for a change an older control plane
/// would misread; additive fields keep the version (missing fields read as
/// zero, unknown ones are ignored).
pub const WORKER_METRICS_VERSION: u32 = 1;

/// Largest report body the control plane reads. A full report is ~2 KiB.
pub const MAX_REPORT_BYTES: usize = 16 * 1024;

/// Longest `instance_id` accepted. The agent sends a UUID (36 characters).
pub const MAX_INSTANCE_ID_LEN: usize = 64;

/// Rejected host sample names kept for the log line, so one misbehaving
/// report cannot make the control plane log an unbounded list.
const MAX_REJECTED_EXAMPLES: usize = 5;

/// Longest rejected name kept for the log line.
const MAX_REJECTED_NAME_LEN: usize = 64;

/// What a worker's agent serves at [`WORKER_METRICS_PATH`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkerMetricsReport {
    /// [`WORKER_METRICS_VERSION`] of the agent that produced it.
    pub version: u32,
    /// Random per agent process. A new value means the agent restarted and
    /// its proxy counters started over from zero.
    pub instance_id: String,
    /// Cumulative edge-proxy counters since the agent process started.
    #[serde(default)]
    pub proxy: MetricsSnapshot,
    /// Host samples taken when the report was requested, from the node
    /// collector in host scope.
    #[serde(default)]
    pub host: Vec<HostSample>,
}

/// One host measurement. No timestamp on purpose: the control plane stamps it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostSample {
    pub name: String,
    pub value: f64,
}

/// Why a worker report was refused as a whole.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum WorkerReportError {
    #[error(
        "Metrics report from node {node_id} is {size} bytes, over the {max}-byte bound; \
         nothing from it was stored"
    )]
    TooLarge {
        node_id: i32,
        size: usize,
        max: usize,
    },
    #[error("Metrics report from node {node_id} could not be decoded: {reason}")]
    Decode { node_id: i32, reason: String },
    #[error(
        "Metrics report from node {node_id} uses format version {version}; this control plane \
         reads version {supported}. Upgrade the control plane or the agent so they match"
    )]
    UnsupportedVersion {
        node_id: i32,
        version: u32,
        supported: u32,
    },
    #[error(
        "Metrics report from node {node_id} has an instance id of {len} characters \
         (empty or over {max}); nothing from it was stored"
    )]
    InvalidInstance {
        node_id: i32,
        len: usize,
        max: usize,
    },
}

/// Size-check and decode a report body. The size check comes first so an
/// oversized body is never parsed.
pub fn decode_report(node_id: i32, body: &[u8]) -> Result<WorkerMetricsReport, WorkerReportError> {
    if body.len() > MAX_REPORT_BYTES {
        return Err(WorkerReportError::TooLarge {
            node_id,
            size: body.len(),
            max: MAX_REPORT_BYTES,
        });
    }
    let report: WorkerMetricsReport =
        serde_json::from_slice(body).map_err(|e| WorkerReportError::Decode {
            node_id,
            reason: e.to_string(),
        })?;
    if report.version != WORKER_METRICS_VERSION {
        return Err(WorkerReportError::UnsupportedVersion {
            node_id,
            version: report.version,
            supported: WORKER_METRICS_VERSION,
        });
    }
    let len = report.instance_id.chars().count();
    if len == 0 || len > MAX_INSTANCE_ID_LEN {
        return Err(WorkerReportError::InvalidInstance {
            node_id,
            len,
            max: MAX_INSTANCE_ID_LEN,
        });
    }
    Ok(report)
}

/// The previous proxy snapshot of one worker, kept by the control plane
/// between pulls.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkerBaseline {
    instance_id: String,
    proxy: MetricsSnapshot,
}

/// The points to store from one accepted report, and what was left out.
#[derive(Debug, Clone, Default)]
pub struct AcceptedReport {
    pub points: Vec<MetricPoint>,
    /// Host samples refused (unknown name, duplicate, or out-of-range value).
    pub rejected: usize,
    /// Up to a few of the refused names, truncated, for the log line.
    pub rejected_examples: Vec<String>,
    /// `true` when no proxy points were written because this is the first
    /// report seen from the node: it only established the baseline.
    pub baseline_only: bool,
}

/// Turn a decoded report into store points for `node_id`, every one stamped
/// `received_at` (the control plane's clock), and advance `baseline`.
pub fn accept_report(
    node_id: i32,
    report: WorkerMetricsReport,
    baseline: &mut Option<WorkerBaseline>,
    received_at: DateTime<Utc>,
) -> AcceptedReport {
    let mut accepted = AcceptedReport::default();

    // ── Proxy: cumulative snapshot → per-interval delta ──────────────────
    let delta = match baseline.as_ref() {
        // First contact since this control plane started: nothing to
        // difference against yet.
        None => None,
        Some(prev) if prev.instance_id == report.instance_id => {
            Some(report.proxy.delta_since(&prev.proxy))
        }
        // The agent restarted: its counters began at zero, so everything
        // they hold happened since.
        Some(_) => Some(report.proxy.delta_since(&MetricsSnapshot::default())),
    };
    match delta {
        Some(delta) => accepted
            .points
            .extend(proxy_points(&delta.samples(), node_id, received_at)),
        None => accepted.baseline_only = true,
    }
    *baseline = Some(WorkerBaseline {
        instance_id: report.instance_id,
        proxy: report.proxy,
    });

    // ── Host: allow-listed names, once each, sane values ─────────────────
    let mut seen: HashSet<&'static str> = HashSet::new();
    for sample in report.host {
        // The heartbeat writes these; a current agent sending them too is
        // expected, not a refusal.
        if CAPACITY_METRIC_NAMES.contains(&sample.name.as_str()) {
            continue;
        }
        let known = HOST_METRIC_NAMES
            .iter()
            .copied()
            .find(|name| *name == sample.name);
        let accepted_name = known
            .filter(|name| host_value_ok(name, sample.value))
            .filter(|name| seen.insert(*name));
        match accepted_name {
            Some(name) => accepted.points.push(MetricPoint {
                time: received_at,
                source_kind: SourceKind::Node,
                source_id: node_id,
                name: name.to_string(),
                value: sample.value,
                kind: MetricKind::Gauge,
                engine: None,
                environment: None,
                node_id: Some(node_id),
                labels: HashMap::new(),
            }),
            None => {
                accepted.rejected += 1;
                if accepted.rejected_examples.len() < MAX_REJECTED_EXAMPLES {
                    accepted
                        .rejected_examples
                        .push(sample.name.chars().take(MAX_REJECTED_NAME_LEN).collect());
                }
            }
        }
    }

    accepted
}

/// Host values are counts, bytes or percentages: finite, never negative, and
/// a percentage never above 100.
fn host_value_ok(name: &str, value: f64) -> bool {
    value.is_finite() && value >= 0.0 && (!name.ends_with("_percent") || value <= 100.0)
}

// ── Reporting status ─────────────────────────────────────────────────────────

/// Where a worker stands in reporting metrics to the control plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerReportState {
    /// The last pull was stored.
    Reporting,
    /// The agent has no metrics endpoint: it predates this feature.
    AgentTooOld,
    /// The agent could not be reached, or did not answer in time.
    Unreachable,
    /// The agent refused the control plane's credentials.
    Rejected,
    /// The agent answered something the control plane could not accept
    /// (oversized, undecodable, or another format version).
    Invalid,
}

/// The latest reporting outcome for one worker.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkerReportStatus {
    pub state: WorkerReportState,
    /// When the control plane last tried to pull.
    pub last_attempt_at: DateTime<Utc>,
    /// When a report from this worker was last stored.
    pub last_report_at: Option<DateTime<Utc>>,
    /// What went wrong, for a state other than `Reporting`.
    pub detail: Option<String>,
}

/// In-memory reporting status of every worker, written by the control
/// plane's collector and read by the metrics API.
#[derive(Debug, Default)]
pub struct WorkerMetricsStatusBoard {
    inner: std::sync::RwLock<HashMap<i32, WorkerReportStatus>>,
}

impl WorkerMetricsStatusBoard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a stored report.
    pub fn record_report(&self, node_id: i32, at: DateTime<Utc>) {
        if let Ok(mut map) = self.inner.write() {
            map.insert(
                node_id,
                WorkerReportStatus {
                    state: WorkerReportState::Reporting,
                    last_attempt_at: at,
                    last_report_at: Some(at),
                    detail: None,
                },
            );
        }
    }

    /// Record a failed pull. Returns the state the node was in before, so the
    /// caller can log a transition once instead of every interval.
    pub fn record_failure(
        &self,
        node_id: i32,
        state: WorkerReportState,
        detail: impl Into<String>,
        at: DateTime<Utc>,
    ) -> Option<WorkerReportState> {
        let Ok(mut map) = self.inner.write() else {
            return None;
        };
        let previous = map.get(&node_id).cloned();
        map.insert(
            node_id,
            WorkerReportStatus {
                state,
                last_attempt_at: at,
                last_report_at: previous.as_ref().and_then(|p| p.last_report_at),
                detail: Some(detail.into()),
            },
        );
        previous.map(|p| p.state)
    }

    /// Previous state of `node_id`, if any.
    pub fn state_of(&self, node_id: i32) -> Option<WorkerReportState> {
        self.inner
            .read()
            .ok()
            .and_then(|map| map.get(&node_id).map(|s| s.state))
    }

    /// Forget nodes that are no longer pulled (removed or offline).
    pub fn retain(&self, node_ids: &HashSet<i32>) {
        if let Ok(mut map) = self.inner.write() {
            map.retain(|id, _| node_ids.contains(id));
        }
    }

    /// Every known worker's status, sorted by node id.
    pub fn snapshot(&self) -> Vec<(i32, WorkerReportStatus)> {
        let mut out: Vec<(i32, WorkerReportStatus)> = self
            .inner
            .read()
            .map(|map| map.iter().map(|(id, s)| (*id, s.clone())).collect())
            .unwrap_or_default();
        out.sort_by_key(|(id, _)| *id);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::{ProxyMetrics, RequestDestination, METRIC_REQUESTS, METRIC_REQUESTS_5XX};
    use chrono::TimeZone;

    fn plane_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 23, 12, 0, 0).unwrap()
    }

    fn report(
        instance: &str,
        metrics: &ProxyMetrics,
        host: Vec<HostSample>,
    ) -> WorkerMetricsReport {
        WorkerMetricsReport {
            version: WORKER_METRICS_VERSION,
            instance_id: instance.to_string(),
            proxy: metrics.snapshot(),
            host,
        }
    }

    fn sample(name: &str, value: f64) -> HostSample {
        HostSample {
            name: name.to_string(),
            value,
        }
    }

    fn value_of(points: &[MetricPoint], name: &str) -> Option<f64> {
        points.iter().find(|p| p.name == name).map(|p| p.value)
    }

    #[test]
    fn every_point_is_stamped_with_the_control_plane_receive_time_for_the_node() {
        let metrics = ProxyMetrics::default();
        let mut baseline = None;
        accept_report(7, report("a", &metrics, vec![]), &mut baseline, plane_now());
        metrics.record(200, 10, Some(5), RequestDestination::Project, false);

        let received_at = plane_now() + chrono::Duration::seconds(30);
        let accepted = accept_report(
            7,
            report("a", &metrics, vec![sample("node.load_avg_1m", 0.5)]),
            &mut baseline,
            received_at,
        );

        assert!(!accepted.points.is_empty());
        for p in &accepted.points {
            assert_eq!(p.time, received_at, "{}", p.name);
            assert_eq!(p.source_kind, SourceKind::Node, "{}", p.name);
            assert_eq!(p.source_id, 7, "{}", p.name);
            assert_eq!(p.node_id, Some(7), "{}", p.name);
            assert!(p.labels.is_empty(), "{}", p.name);
            assert!(p.environment.is_none(), "{}", p.name);
        }
    }

    #[test]
    fn the_wire_format_carries_no_timestamp_a_worker_could_set() {
        // A worker that adds its own clock reading is ignored, not obeyed.
        let body = br#"{"version":1,"instance_id":"a","time":"2099-01-01T00:00:00Z",
            "host":[{"name":"node.load_avg_1m","value":5,"time":"1999-01-01T00:00:00Z"}]}"#;
        let decoded = decode_report(3, body).expect("unknown fields are ignored");
        let mut baseline = None;
        let accepted = accept_report(3, decoded, &mut baseline, plane_now());
        assert_eq!(accepted.points.len(), 1);
        assert_eq!(accepted.points[0].time, plane_now());
    }

    #[test]
    fn first_report_only_sets_the_baseline_then_deltas_follow() {
        let metrics = ProxyMetrics::default();
        metrics.record(200, 1, None, RequestDestination::Project, false);
        let mut baseline = None;

        let first = accept_report(1, report("a", &metrics, vec![]), &mut baseline, plane_now());
        assert!(first.baseline_only);
        assert!(
            first.points.is_empty(),
            "traffic before the first pull has no interval"
        );

        metrics.record(200, 1, None, RequestDestination::Project, false);
        metrics.record(503, 1, None, RequestDestination::Project, false);
        let second = accept_report(1, report("a", &metrics, vec![]), &mut baseline, plane_now());
        assert!(!second.baseline_only);
        assert_eq!(value_of(&second.points, METRIC_REQUESTS), Some(2.0));
        assert_eq!(value_of(&second.points, METRIC_REQUESTS_5XX), Some(1.0));
    }

    #[test]
    fn a_lost_pull_widens_the_next_interval_instead_of_losing_traffic() {
        let metrics = ProxyMetrics::default();
        let mut baseline = None;
        accept_report(1, report("a", &metrics, vec![]), &mut baseline, plane_now());
        metrics.record(200, 1, None, RequestDestination::Project, false);
        // This report never reaches the control plane.
        let _lost = report("a", &metrics, vec![]);
        metrics.record(200, 1, None, RequestDestination::Project, false);
        let next = accept_report(1, report("a", &metrics, vec![]), &mut baseline, plane_now());
        assert_eq!(value_of(&next.points, METRIC_REQUESTS), Some(2.0));
    }

    #[test]
    fn an_agent_restart_takes_the_new_counters_whole() {
        let before = ProxyMetrics::default();
        for _ in 0..10 {
            before.record(200, 1, None, RequestDestination::Project, false);
        }
        let mut baseline = None;
        accept_report(1, report("a", &before, vec![]), &mut baseline, plane_now());

        // New process: counters start over, and three requests arrived.
        let after = ProxyMetrics::default();
        for _ in 0..3 {
            after.record(200, 1, None, RequestDestination::Project, false);
        }
        let next = accept_report(1, report("b", &after, vec![]), &mut baseline, plane_now());
        assert_eq!(value_of(&next.points, METRIC_REQUESTS), Some(3.0));
    }

    #[test]
    fn host_samples_are_limited_to_known_names_once_each_with_sane_values() {
        let metrics = ProxyMetrics::default();
        let mut baseline = None;
        let accepted = accept_report(
            2,
            report(
                "a",
                &metrics,
                vec![
                    sample("node.load_avg_1m", 0.4),
                    // Duplicate: the first one wins.
                    sample("node.load_avg_1m", 9.0),
                    sample("node.fd_allocated", 1024.0),
                    // Not a host series.
                    sample("node.process_open_fds", 10.0),
                    sample("anything.else", 1.0),
                    // Out of range.
                    sample("node.fd_percent", 250.0),
                    sample("node.network_rx_bytes_total", -1.0),
                ],
            ),
            &mut baseline,
            plane_now(),
        );
        let names: Vec<&str> = accepted.points.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["node.load_avg_1m", "node.fd_allocated"]);
        assert_eq!(value_of(&accepted.points, "node.load_avg_1m"), Some(0.4));
        assert_eq!(accepted.rejected, 5);
        assert_eq!(accepted.rejected_examples.len(), 5);
    }

    #[test]
    fn heartbeat_series_are_left_to_the_heartbeat() {
        let metrics = ProxyMetrics::default();
        let mut baseline = None;
        let host = CAPACITY_METRIC_NAMES
            .iter()
            .map(|name| sample(name, 10.0))
            .chain([sample("node.disk_read_bytes_total", 4096.0)])
            .collect();
        let accepted = accept_report(2, report("a", &metrics, host), &mut baseline, plane_now());
        let names: Vec<&str> = accepted.points.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["node.disk_read_bytes_total"]);
        assert_eq!(accepted.rejected, 0);
        // Every heartbeat series is one a worker host may report, so the
        // skip above is what keeps them single-writer.
        assert!(CAPACITY_METRIC_NAMES
            .iter()
            .all(|name| HOST_METRIC_NAMES.contains(name)));
    }

    #[test]
    fn rejected_examples_are_bounded_in_count_and_length() {
        let metrics = ProxyMetrics::default();
        let mut baseline = None;
        let host = (0..200)
            .map(|i| sample(&format!("x{}{}", i, "y".repeat(500)), 1.0))
            .collect();
        let accepted = accept_report(2, report("a", &metrics, host), &mut baseline, plane_now());
        assert!(accepted.points.is_empty());
        assert_eq!(accepted.rejected, 200);
        assert_eq!(accepted.rejected_examples.len(), MAX_REJECTED_EXAMPLES);
        assert!(accepted
            .rejected_examples
            .iter()
            .all(|n| n.chars().count() <= MAX_REJECTED_NAME_LEN));
    }

    #[test]
    fn proxy_points_use_only_the_control_plane_series_names() {
        let metrics = ProxyMetrics::default();
        let mut baseline = None;
        accept_report(1, report("a", &metrics, vec![]), &mut baseline, plane_now());
        metrics.record(200, 10, Some(5), RequestDestination::Project, false);
        metrics.record(101, 1_000, Some(2), RequestDestination::Project, true);
        let accepted = accept_report(1, report("a", &metrics, vec![]), &mut baseline, plane_now());
        for p in &accepted.points {
            assert!(p.name.starts_with("proxy."), "{}", p.name);
            assert_eq!(p.engine.as_deref(), Some("proxy"));
        }
    }

    #[test]
    fn oversized_bodies_are_refused_before_parsing() {
        let body = vec![b' '; MAX_REPORT_BYTES + 1];
        assert!(matches!(
            decode_report(4, &body),
            Err(WorkerReportError::TooLarge { node_id: 4, .. })
        ));
    }

    #[test]
    fn other_versions_and_bad_instance_ids_are_refused() {
        assert!(matches!(
            decode_report(4, br#"{"version":2,"instance_id":"a"}"#),
            Err(WorkerReportError::UnsupportedVersion { version: 2, .. })
        ));
        assert!(matches!(
            decode_report(4, br#"{"version":1,"instance_id":""}"#),
            Err(WorkerReportError::InvalidInstance { len: 0, .. })
        ));
        let long = format!(r#"{{"version":1,"instance_id":"{}"}}"#, "a".repeat(65));
        assert!(matches!(
            decode_report(4, long.as_bytes()),
            Err(WorkerReportError::InvalidInstance { len: 65, .. })
        ));
        assert!(matches!(
            decode_report(4, b"not json"),
            Err(WorkerReportError::Decode { .. })
        ));
    }

    #[test]
    fn a_snapshot_with_a_wrong_shape_is_a_decode_error() {
        // Five status classes, never more: an extra bucket is not a new series.
        let body = br#"{"version":1,"instance_id":"a","proxy":{"status_classes":[1,2,3,4,5,6]}}"#;
        assert!(matches!(
            decode_report(4, body),
            Err(WorkerReportError::Decode { .. })
        ));
    }

    #[test]
    fn a_report_round_trips_through_json() {
        let metrics = ProxyMetrics::default();
        metrics.record(200, 12, Some(3), RequestDestination::Project, false);
        let original = report("a", &metrics, vec![sample("node.cpu_percent", 3.5)]);
        let body = serde_json::to_vec(&original).unwrap();
        assert!(
            body.len() < MAX_REPORT_BYTES / 2,
            "a full report is {} bytes",
            body.len()
        );
        assert_eq!(decode_report(1, &body).unwrap(), original);
    }

    #[test]
    fn status_board_keeps_the_last_report_time_across_failures() {
        let board = WorkerMetricsStatusBoard::new();
        let t0 = plane_now();
        board.record_report(5, t0);
        let t1 = t0 + chrono::Duration::seconds(30);
        let previous = board.record_failure(5, WorkerReportState::Unreachable, "timed out", t1);
        assert_eq!(previous, Some(WorkerReportState::Reporting));

        let snapshot = board.snapshot();
        assert_eq!(snapshot.len(), 1);
        let (id, status) = &snapshot[0];
        assert_eq!(*id, 5);
        assert_eq!(status.state, WorkerReportState::Unreachable);
        assert_eq!(status.last_report_at, Some(t0));
        assert_eq!(status.last_attempt_at, t1);

        board.retain(&HashSet::new());
        assert!(board.snapshot().is_empty());
    }
}
