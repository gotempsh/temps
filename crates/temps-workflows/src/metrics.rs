// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Workflow metrics (ADR-103 §14), recorded in memory and flushed in batches
//! to the project metrics interface every [`FLUSH_INTERVAL`].
//!
//! Labels are bounded: project, environment, workflow name and outcome, never
//! a run id. The number of series held between flushes is capped at
//! [`MAX_SERIES`]; past it new workflow names fold into the [`OVERFLOW`]
//! workflow label, so a tenant creating thousands of workflow names cannot
//! grow the control plane's memory.
//!
//! Recording takes a short mutex: it happens per run transition and per
//! journal write, on the control plane, never per proxied request.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use chrono::{DateTime, Utc};
use temps_core::{ProjectMetricKind, ProjectMetricSample, ProjectMetricsSink};

use crate::store::{JournalSize, QueueStat};

pub const FLUSH_INTERVAL: Duration = Duration::from_secs(30);
pub const MAX_SERIES: usize = 5_000;
pub const OVERFLOW: &str = "_other";
/// Overflow series (one per metric, project and environment) allowed past
/// the cap before samples are dropped.
const OVERFLOW_HEADROOM: usize = 64;

pub const RUNS_STARTED: &str = "temps.workflow.runs_started";
pub const RUNS_FINISHED: &str = "temps.workflow.runs_finished";
pub const STEPS: &str = "temps.workflow.steps";
pub const STEP_DURATION_AVG: &str = "temps.workflow.step_duration_avg";
pub const STEP_DURATION_MAX: &str = "temps.workflow.step_duration_max";
pub const INVOCATION_FAILURES: &str = "temps.workflow.invocation_failures";
pub const QUEUE_DEPTH: &str = "temps.workflow.queue_depth";
pub const QUEUE_AGE: &str = "temps.workflow.queue_age";
pub const STUCK_RUNS: &str = "temps.workflow.stuck_runs";
pub const STORAGE_BYTES: &str = "temps.workflow.storage_bytes";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SeriesKey {
    name: &'static str,
    project_id: i32,
    environment_id: i32,
    workflow: String,
    outcome: &'static str,
}

#[derive(Debug, Default, Clone, Copy)]
struct Durations {
    count: u64,
    sum_ms: u64,
    max_ms: u64,
}

#[derive(Debug, Default)]
struct Pending {
    counters: HashMap<SeriesKey, u64>,
    durations: HashMap<SeriesKey, Durations>,
    dropped: u64,
}

impl Pending {
    fn series(&self) -> usize {
        self.counters.len() + self.durations.len()
    }
}

pub struct WorkflowMetrics {
    pending: Mutex<Pending>,
    sink: Arc<dyn ProjectMetricsSink>,
}

impl WorkflowMetrics {
    pub fn new(sink: Arc<dyn ProjectMetricsSink>) -> Self {
        Self {
            pending: Mutex::new(Pending::default()),
            sink,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Pending> {
        // A panic while holding the lock leaves counters, not invariants.
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The series to record into: `key` while there is room, else the
    /// overflow series of its name; `None` (counted as dropped) when even
    /// the overflow series cannot be added.
    fn admit(
        pending: &mut Pending,
        key: SeriesKey,
        present: impl Fn(&Pending, &SeriesKey) -> bool,
    ) -> Option<SeriesKey> {
        if present(pending, &key) || pending.series() < MAX_SERIES {
            return Some(key);
        }
        let overflow = SeriesKey {
            workflow: OVERFLOW.to_string(),
            ..key
        };
        if present(pending, &overflow) || pending.series() < MAX_SERIES + OVERFLOW_HEADROOM {
            return Some(overflow);
        }
        pending.dropped += 1;
        None
    }

    fn count(
        &self,
        name: &'static str,
        project_id: i32,
        environment_id: i32,
        workflow: &str,
        outcome: &'static str,
    ) {
        let key = SeriesKey {
            name,
            project_id,
            environment_id,
            workflow: workflow.to_string(),
            outcome,
        };
        let mut pending = self.lock();
        if let Some(key) = Self::admit(&mut pending, key, |p, k| p.counters.contains_key(k)) {
            *pending.counters.entry(key).or_insert(0) += 1;
        }
    }

    pub fn run_started(&self, project_id: i32, environment_id: i32, workflow: &str) {
        self.count(RUNS_STARTED, project_id, environment_id, workflow, "");
    }

    /// `outcome`: `completed`, `failed` or `cancelled`.
    pub fn run_finished(
        &self,
        project_id: i32,
        environment_id: i32,
        workflow: &str,
        outcome: &'static str,
    ) {
        self.count(RUNS_FINISHED, project_id, environment_id, workflow, outcome);
    }

    /// `reason`: a fixed category (`connect`, `timeout`, `http_status`,
    /// `manifest`, `lease_expired`, ...), never an error message.
    pub fn invocation_failed(
        &self,
        project_id: i32,
        environment_id: i32,
        workflow: &str,
        reason: &'static str,
    ) {
        self.count(
            INVOCATION_FAILURES,
            project_id,
            environment_id,
            workflow,
            reason,
        );
    }

    /// A journal entry reached a final status; `outcome` is its status.
    pub fn step_finished(
        &self,
        project_id: i32,
        environment_id: i32,
        workflow: &str,
        outcome: &'static str,
        duration_ms: Option<i64>,
    ) {
        self.count(STEPS, project_id, environment_id, workflow, outcome);
        let Some(duration_ms) = duration_ms.and_then(|ms| u64::try_from(ms).ok()) else {
            return;
        };
        let key = SeriesKey {
            name: STEP_DURATION_AVG,
            project_id,
            environment_id,
            workflow: workflow.to_string(),
            outcome: "",
        };
        let mut pending = self.lock();
        let Some(key) = Self::admit(&mut pending, key, |p, k| p.durations.contains_key(k)) else {
            return;
        };
        let entry = pending.durations.entry(key).or_default();
        entry.count += 1;
        entry.sum_ms = entry.sum_ms.saturating_add(duration_ms);
        entry.max_ms = entry.max_ms.max(duration_ms);
    }

    /// Take everything recorded since the last call, as samples.
    fn drain(&self, now: DateTime<Utc>) -> (Vec<ProjectMetricSample>, u64) {
        let pending = std::mem::take(&mut *self.lock());
        let mut samples = Vec::with_capacity(pending.series() * 2);
        for (key, value) in pending.counters {
            samples.push(sample(
                &key,
                key.name,
                ProjectMetricKind::DeltaCounter,
                "{count}",
                value as f64,
                now,
            ));
        }
        for (key, durations) in pending.durations {
            if durations.count == 0 {
                continue;
            }
            samples.push(sample(
                &key,
                STEP_DURATION_AVG,
                ProjectMetricKind::Gauge,
                "ms",
                durations.sum_ms as f64 / durations.count as f64,
                now,
            ));
            samples.push(sample(
                &key,
                STEP_DURATION_MAX,
                ProjectMetricKind::Gauge,
                "ms",
                durations.max_ms as f64,
                now,
            ));
        }
        (samples, pending.dropped)
    }

    /// Flush counters plus the queue gauges and (when given) storage sizes.
    pub async fn flush(&self, queues: &[QueueStat], storage: Option<&[JournalSize]>) {
        let now = Utc::now();
        let (mut samples, dropped) = self.drain(now);
        for queue in queues.iter().take(MAX_SERIES) {
            let key = SeriesKey {
                name: QUEUE_DEPTH,
                project_id: queue.project_id,
                environment_id: queue.environment_id,
                workflow: queue.workflow_name.clone(),
                outcome: "",
            };
            samples.push(sample(
                &key,
                QUEUE_DEPTH,
                ProjectMetricKind::Gauge,
                "{run}",
                queue.depth as f64,
                now,
            ));
            let age = queue
                .oldest_due_at
                .map(|due| (now - due).num_milliseconds().max(0) as f64 / 1000.0)
                .unwrap_or(0.0);
            samples.push(sample(
                &key,
                QUEUE_AGE,
                ProjectMetricKind::Gauge,
                "s",
                age,
                now,
            ));
            samples.push(sample(
                &key,
                STUCK_RUNS,
                ProjectMetricKind::Gauge,
                "{run}",
                queue.stuck as f64,
                now,
            ));
        }
        for size in storage.unwrap_or_default().iter().take(MAX_SERIES) {
            samples.push(ProjectMetricSample {
                project_id: size.project_id,
                name: STORAGE_BYTES,
                kind: ProjectMetricKind::Gauge,
                unit: "By",
                value: size.bytes as f64,
                labels: vec![("environment_id", size.environment_id.to_string())],
                timestamp: now,
            });
        }
        if dropped > 0 {
            tracing::warn!(
                dropped,
                max_series = MAX_SERIES,
                "workflow metrics exceeded their series cap; samples were dropped"
            );
        }
        if samples.is_empty() {
            return;
        }
        if let Err(error) = self.sink.record(samples).await {
            tracing::warn!(%error, "could not store workflow metrics; this batch is dropped");
        }
    }
}

fn sample(
    key: &SeriesKey,
    name: &'static str,
    kind: ProjectMetricKind,
    unit: &'static str,
    value: f64,
    now: DateTime<Utc>,
) -> ProjectMetricSample {
    let mut labels = vec![
        ("environment_id", key.environment_id.to_string()),
        ("workflow", key.workflow.clone()),
    ];
    if !key.outcome.is_empty() {
        labels.push(("outcome", key.outcome.to_string()));
    }
    ProjectMetricSample {
        project_id: key.project_id,
        name,
        kind,
        unit,
        value,
        labels,
        timestamp: now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use temps_core::ProjectMetricsError;

    #[derive(Default)]
    pub(crate) struct CapturingSink {
        pub samples: Mutex<Vec<ProjectMetricSample>>,
    }

    #[async_trait]
    impl ProjectMetricsSink for CapturingSink {
        async fn record(
            &self,
            samples: Vec<ProjectMetricSample>,
        ) -> Result<(), ProjectMetricsError> {
            self.samples.lock().unwrap().extend(samples);
            Ok(())
        }
    }

    fn find<'a>(
        samples: &'a [ProjectMetricSample],
        name: &str,
        workflow: &str,
    ) -> Vec<&'a ProjectMetricSample> {
        samples
            .iter()
            .filter(|s| {
                s.name == name
                    && s.labels
                        .iter()
                        .any(|(k, v)| *k == "workflow" && v == workflow)
            })
            .collect()
    }

    #[tokio::test]
    async fn counters_are_deltas_with_bounded_labels_and_reset_on_flush() {
        let sink = Arc::new(CapturingSink::default());
        let metrics = WorkflowMetrics::new(sink.clone());
        metrics.run_started(1, 2, "orders");
        metrics.run_started(1, 2, "orders");
        metrics.run_finished(1, 2, "orders", "completed");
        metrics.step_finished(1, 2, "orders", "completed", Some(10));
        metrics.step_finished(1, 2, "orders", "completed", Some(30));
        metrics.flush(&[], None).await;

        let samples = sink.samples.lock().unwrap().clone();
        let started = find(&samples, RUNS_STARTED, "orders");
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].value, 2.0);
        assert_eq!(started[0].kind, ProjectMetricKind::DeltaCounter);
        assert_eq!(find(&samples, STEP_DURATION_AVG, "orders")[0].value, 20.0);
        assert_eq!(find(&samples, STEP_DURATION_MAX, "orders")[0].value, 30.0);
        let finished = find(&samples, RUNS_FINISHED, "orders");
        assert!(finished[0]
            .labels
            .contains(&("outcome", "completed".to_string())));
        // No label carries a run id: only these keys exist.
        for sample in &samples {
            for (key, _) in &sample.labels {
                assert!(["environment_id", "workflow", "outcome"].contains(key));
            }
        }

        sink.samples.lock().unwrap().clear();
        metrics.flush(&[], None).await;
        assert!(sink.samples.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn series_past_the_cap_fold_into_the_overflow_label() {
        let sink = Arc::new(CapturingSink::default());
        let metrics = WorkflowMetrics::new(sink.clone());
        for index in 0..(MAX_SERIES + 10) {
            metrics.run_started(1, 2, &format!("wf-{index}"));
        }
        metrics.flush(&[], None).await;
        let samples = sink.samples.lock().unwrap().clone();
        assert!(samples.len() <= MAX_SERIES + 1);
        let overflow = find(&samples, RUNS_STARTED, OVERFLOW);
        assert_eq!(overflow.len(), 1);
        assert_eq!(overflow[0].value, 10.0);
    }

    #[tokio::test]
    async fn queue_gauges_report_depth_age_and_stuck_runs() {
        let sink = Arc::new(CapturingSink::default());
        let metrics = WorkflowMetrics::new(sink.clone());
        let queues = [QueueStat {
            project_id: 1,
            environment_id: 2,
            workflow_name: "orders".into(),
            depth: 4,
            oldest_due_at: Some(Utc::now() - chrono::Duration::seconds(30)),
            stuck: 1,
        }];
        let storage = [JournalSize {
            project_id: 1,
            environment_id: 2,
            bytes: 2048,
        }];
        metrics.flush(&queues, Some(&storage)).await;
        let samples = sink.samples.lock().unwrap().clone();
        assert_eq!(find(&samples, QUEUE_DEPTH, "orders")[0].value, 4.0);
        assert!(find(&samples, QUEUE_AGE, "orders")[0].value >= 29.0);
        assert_eq!(find(&samples, STUCK_RUNS, "orders")[0].value, 1.0);
        let storage = samples
            .iter()
            .find(|s| s.name == STORAGE_BYTES)
            .expect("storage gauge");
        assert_eq!(storage.value, 2048.0);
    }
}
