// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! [`ProjectMetricsSink`] over the OTEL metrics store.
//!
//! Metrics the control plane produces for a project (durable workflows,
//! ADR-103 §10) land in the same store as the project's OTLP metrics, so
//! ordinary metric alert rules and dashboards read them. Samples arrive in
//! batches from an interval flush, never per operation; they are attributed
//! to the `temps` service so they never mix with an application's own.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use temps_core::{ProjectMetricKind, ProjectMetricSample, ProjectMetricsError, ProjectMetricsSink};

use crate::storage::OtelStorage;
use crate::types::{AggregationTemporality, MetricPoint, MetricType, ResourceInfo};

/// `service.name` of control-plane samples.
pub const CONTROL_PLANE_SERVICE: &str = "temps";

pub struct OtelProjectMetricsSink {
    storage: Arc<dyn OtelStorage>,
}

impl OtelProjectMetricsSink {
    pub fn new(storage: Arc<dyn OtelStorage>) -> Self {
        Self { storage }
    }
}

fn to_point(sample: ProjectMetricSample) -> MetricPoint {
    let resource = ResourceInfo {
        service_name: CONTROL_PLANE_SERVICE.to_string(),
        ..ResourceInfo::default()
    };
    let attributes: BTreeMap<String, String> = sample
        .labels
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect();
    let metric_type = match sample.kind {
        ProjectMetricKind::Gauge => MetricType::Gauge,
        ProjectMetricKind::DeltaCounter => MetricType::Sum,
    };
    let mut point = MetricPoint::skeleton(
        sample.project_id,
        None,
        resource,
        sample.name.to_string(),
        metric_type,
        sample.unit.to_string(),
        sample.timestamp,
        attributes,
    );
    point.value = Some(sample.value);
    if sample.kind == ProjectMetricKind::DeltaCounter {
        point.temporality = Some(AggregationTemporality::Delta);
        point.is_monotonic = Some(true);
    }
    point
}

#[async_trait]
impl ProjectMetricsSink for OtelProjectMetricsSink {
    async fn record(&self, samples: Vec<ProjectMetricSample>) -> Result<(), ProjectMetricsError> {
        if samples.is_empty() {
            return Ok(());
        }
        let count = samples.len();
        let points = samples.into_iter().map(to_point).collect();
        self.storage
            .store_metrics(points)
            .await
            .map(|_| ())
            .map_err(|error| ProjectMetricsError {
                count,
                reason: error.to_string(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::MockOtelStorage;

    fn sample(kind: ProjectMetricKind, value: f64) -> ProjectMetricSample {
        ProjectMetricSample {
            project_id: 12,
            name: "temps.workflow.runs_started",
            kind,
            unit: "{run}",
            value,
            labels: vec![
                ("environment_id", "3".to_string()),
                ("workflow", "send-invoice".to_string()),
            ],
            timestamp: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn samples_are_stored_as_project_metrics_of_the_temps_service() {
        let storage = Arc::new(MockOtelStorage::new());
        let sink = OtelProjectMetricsSink::new(storage.clone());
        sink.record(vec![
            sample(ProjectMetricKind::DeltaCounter, 4.0),
            sample(ProjectMetricKind::Gauge, 9.0),
        ])
        .await
        .unwrap();

        let stored = storage.stored_metrics();
        assert_eq!(stored.len(), 2);
        let counter = &stored[0];
        assert_eq!(counter.project_id, 12);
        assert_eq!(counter.resource.service_name, CONTROL_PLANE_SERVICE);
        assert_eq!(counter.metric_name, "temps.workflow.runs_started");
        assert_eq!(counter.metric_type, MetricType::Sum);
        assert_eq!(counter.temporality, Some(AggregationTemporality::Delta));
        assert_eq!(counter.is_monotonic, Some(true));
        assert_eq!(counter.value, Some(4.0));
        assert_eq!(
            counter.attributes.get("workflow").map(String::as_str),
            Some("send-invoice")
        );
        assert_eq!(
            counter.attributes.get("environment_id").map(String::as_str),
            Some("3")
        );
        let gauge = &stored[1];
        assert_eq!(gauge.metric_type, MetricType::Gauge);
        assert_eq!(gauge.temporality, None);
        assert_eq!(gauge.value, Some(9.0));
    }

    #[tokio::test]
    async fn an_empty_batch_touches_no_storage() {
        let storage = Arc::new(MockOtelStorage::new());
        let sink = OtelProjectMetricsSink::new(storage.clone());
        sink.record(Vec::new()).await.unwrap();
        assert_eq!(storage.store_metrics_call_count(), 0);
    }

    #[tokio::test]
    async fn a_storage_failure_reports_the_batch_size_and_reason() {
        let storage = Arc::new(MockOtelStorage::new());
        *storage.fail_store_metrics.lock().unwrap() = Some("disk full".to_string());
        let sink = OtelProjectMetricsSink::new(storage.clone());
        let error = sink
            .record(vec![sample(ProjectMetricKind::Gauge, 1.0)])
            .await
            .unwrap_err();
        assert_eq!(error.count, 1);
        assert!(error.reason.contains("disk full"), "{}", error.reason);
    }
}
