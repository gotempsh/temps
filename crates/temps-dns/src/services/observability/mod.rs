// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Internal DNS observability on the control plane (ADR-095).
//!
//! [`DnsObservabilityService::ingest`] takes one resolver's report (or why
//! there is none) and stores it: the resolver's state in
//! `dns_resolver_reports`, its new log entries in `dns_query_log`
//! (attributed to the asking container's project, environment or database),
//! and its counter deltas in the metrics store as `dns.*` node series. The
//! same code serves the workers (pulled by the console) and the control
//! plane's own resolver (reported by the proxy process, node 0).
//!
//! The read side lists resolvers, records, queries and traffic for the
//! console, and gives the DNS alert monitor its failure counts.

mod ingest;
mod read;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use sea_orm::{DatabaseConnection, DbErr};
use temps_metrics::MetricsStore;
use temps_node_dns::observe::DnsCounters;

pub use ingest::{
    metric_points, validate_report, IngestSummary, ReportOutcome, ReportState, DNS_METRIC_NAMES,
    MAX_DNS_REPORT_BYTES,
};
pub use read::{
    FailureCount, OutcomeCounts, QueryLogFilter, QueryLogRow, RecordFilter, RecordRow,
    TrafficBucket, TrafficSeries, DNS_ALARM_TYPES,
};

/// The control plane's own resolver reports as node 0, which has no
/// `nodes` row.
pub const CONTROL_PLANE_NODE_ID: i32 = 0;

/// How long the query log keeps entries.
pub const DNS_QUERY_LOG_RETENTION_HOURS: i64 = 24;

/// The most rows the query log keeps, whatever their age.
pub const DNS_QUERY_LOG_MAX_ROWS: u64 = 50_000;

#[derive(Debug, thiserror::Error)]
pub enum DnsObservabilityError {
    #[error("Database error while {operation}: {source}")]
    Database {
        operation: &'static str,
        #[source]
        source: DbErr,
    },
    #[error("Metrics store error while {operation}: {reason}")]
    Metrics {
        operation: &'static str,
        reason: String,
    },
    #[error("Invalid {field}: {reason}")]
    InvalidFilter { field: &'static str, reason: String },
}

impl DnsObservabilityError {
    fn db(operation: &'static str) -> impl FnOnce(DbErr) -> Self {
        move |source| Self::Database { operation, source }
    }
}

/// Counters last read from one resolver instance, so the next read stores
/// only what changed.
struct Baseline {
    instance_id: String,
    counters: DnsCounters,
}

pub struct DnsObservabilityService {
    db: Arc<DatabaseConnection>,
    metrics: Arc<dyn MetricsStore>,
    /// Per node. In memory: a restart starts new baselines and stores no
    /// delta for that pull, as ADR-061 does.
    baselines: Mutex<HashMap<i32, Baseline>>,
}

impl DnsObservabilityService {
    pub fn new(db: Arc<DatabaseConnection>, metrics: Arc<dyn MetricsStore>) -> Self {
        Self {
            db,
            metrics,
            baselines: Mutex::new(HashMap::new()),
        }
    }
}
