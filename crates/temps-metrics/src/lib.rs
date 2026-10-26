// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

pub mod cluster;
pub mod collector;
pub mod error;
pub mod node_capacity;
pub mod node_sampler;
pub mod proxy;
pub mod scraper;
pub mod store;
pub mod worker_report;

// Re-exports for convenient use by downstream crates.
pub use collector::mariadb::MariaDbCollector;
pub use collector::mongodb::MongoCollector;
pub use collector::node::{NodeMetricsCollector, NodeMetricsScope, HOST_METRIC_NAMES};
pub use collector::postgres::PostgresCollector;
pub use collector::redis::RedisCollector;
pub use collector::s3::S3Collector;
pub use collector::{Collector, CollectorConfig};
pub use error::MetricsError;
pub use node_sampler::NodeMetricsSampler;
pub use scraper::{ClusterPrimaryConnection, ClusterPrimaryConnector, MetricsScraper};
pub use store::buffered::{BufferConfig, BufferedMetricsStore};
pub use store::clickhouse::{ChMetricRow, ClickHouseMetricsConfig, ClickhouseMetricsStore};
pub use store::clickhouse_migrations;
pub use store::timescale::{validate_metric_name, TimescaleMetricsStore};
pub use store::{
    clamp_step, duration_to_step, fill_range_gaps, gap_fill_for, gap_fill_step, is_delta_counter,
    is_monotonic_counter, range_to_step, GapFill, LabelledMetric, LatestByLabelQuery, LatestQuery,
    MetricKind, MetricPoint, MetricsStore, RangeQuery, SourceKind, DELTA_COUNTER_METRICS,
};
pub use worker_report::{
    accept_report, decode_report, AcceptedReport, HostSample, WorkerBaseline, WorkerMetricsReport,
    WorkerMetricsStatusBoard, WorkerReportError, WorkerReportState, WorkerReportStatus,
    WORKER_METRICS_PATH,
};
