// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project-scoped metric samples produced by the control plane itself.
//!
//! Project alarm rules (`metric_alert_rules`) are evaluated over the OTEL
//! metrics store, keyed by project. A feature that wants its own metrics to be
//! alertable with ordinary rules (ADR-106 §10) writes them through this sink;
//! `temps-otel` implements it. The producer aggregates in memory and calls
//! [`ProjectMetricsSink::record`] on an interval, never per operation.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;

/// How a sample's value is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectMetricKind {
    /// A point-in-time value (queue depth, stuck runs).
    Gauge,
    /// The increase since the previous sample of the same series (a delta).
    DeltaCounter,
}

/// One sample. Labels must have bounded cardinality: never a run, request or
/// user identifier.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectMetricSample {
    pub project_id: i32,
    /// Dotted name, for example `temps.workflow.runs_started`.
    pub name: &'static str,
    pub kind: ProjectMetricKind,
    pub unit: &'static str,
    pub value: f64,
    pub labels: Vec<(&'static str, String)>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Error)]
#[error("Could not store {count} project metric samples: {reason}")]
pub struct ProjectMetricsError {
    pub count: usize,
    pub reason: String,
}

#[async_trait]
pub trait ProjectMetricsSink: Send + Sync {
    /// Store a batch. Best effort: a failure is logged by the caller and the
    /// batch is dropped, never retried without bound.
    async fn record(&self, samples: Vec<ProjectMetricSample>) -> Result<(), ProjectMetricsError>;
}

/// A sink that stores nothing, for binaries and tests without a metrics store.
pub struct NoopProjectMetricsSink;

#[async_trait]
impl ProjectMetricsSink for NoopProjectMetricsSink {
    async fn record(&self, _samples: Vec<ProjectMetricSample>) -> Result<(), ProjectMetricsError> {
        Ok(())
    }
}
