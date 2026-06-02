//! Prometheus text-exposition scraping helpers.
//!
//! This module provides a small, dependency-light Prometheus scraper used by
//! collectors that expose a `/metrics`-style endpoint (currently the S3 / MinIO
//! collector's `/minio/v2/metrics/cluster` endpoint, which speaks the
//! [Prometheus text exposition format]).
//!
//! It is deliberately *not* a full Prometheus client: it parses the subset of
//! the text format we need (`# HELP` / `# TYPE` comments are ignored, sample
//! lines are parsed into a flat lookup keyed by metric name), and maps a
//! curated set of metric names onto [`MetricPoint`]s via a static
//! [`MetricMapping`] table.
//!
//! ## Counter semantics
//!
//! Consistent with the rest of the collector framework, counter metrics are
//! returned with their **raw cumulative value**. Delta computation is performed
//! by the caller (`MetricsScraper`).
//!
//! ## Value normalization
//!
//! Prometheus permits `NaN`, `+Inf`, `-Inf`, and `Inf` as sample values.
//! [`parse_value`] normalizes any non-finite value to `0.0` so they never reach
//! the time-series store (where they would corrupt aggregates).
//!
//! [Prometheus text exposition format]: https://prometheus.io/docs/instrumenting/exposition_formats/

use std::collections::HashMap;
use std::time::Duration;

use chrono::Utc;
use tracing::warn;

use super::CollectorConfig;
use crate::store::{MetricKind, MetricPoint};

/// A parsed Prometheus exposition response.
///
/// Maps each metric *name* (without labels) to the value of its **first**
/// observed sample line. Label sets are intentionally collapsed — the metrics
/// we map (cluster-wide totals) are not labelled per-instance, so taking the
/// first sample is sufficient and keeps the parser allocation-light.
#[derive(Debug, Clone, Default)]
pub struct PrometheusMetrics {
    values: HashMap<String, f64>,
}

impl PrometheusMetrics {
    /// Return the value for `name`, if present.
    pub fn get(&self, name: &str) -> Option<f64> {
        self.values.get(name).copied()
    }

    /// Number of distinct metric names parsed.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether no samples were parsed.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// A mapping from a Prometheus metric name to a Temps [`MetricPoint`] name and
/// [`MetricKind`].
#[derive(Debug, Clone)]
pub struct MetricMapping {
    /// The metric name as it appears in the Prometheus exposition output.
    pub prom_name: &'static str,
    /// The dotted Temps metric name to emit (e.g. `"s3.total_size_bytes"`).
    pub temps_name: &'static str,
    /// Whether the emitted point is a gauge or a counter delta.
    pub kind: MetricKind,
}

/// Curated MinIO / RustFS cluster metrics exposed at
/// `/minio/v2/metrics/cluster`.
///
/// Only metrics we actively chart are mapped; everything else in the scrape is
/// ignored. Names match the MinIO Prometheus exporter output.
pub const RUSTFS_METRICS: &[MetricMapping] = &[
    MetricMapping {
        prom_name: "minio_cluster_capacity_usable_total_bytes",
        temps_name: "s3.capacity_usable_total_bytes",
        kind: MetricKind::Gauge,
    },
    MetricMapping {
        prom_name: "minio_cluster_capacity_usable_free_bytes",
        temps_name: "s3.capacity_usable_free_bytes",
        kind: MetricKind::Gauge,
    },
    MetricMapping {
        prom_name: "minio_cluster_usage_total_bytes",
        temps_name: "s3.total_size_bytes",
        kind: MetricKind::Gauge,
    },
    MetricMapping {
        prom_name: "minio_cluster_usage_object_total",
        temps_name: "s3.object_count",
        kind: MetricKind::Gauge,
    },
    MetricMapping {
        prom_name: "minio_cluster_bucket_total",
        temps_name: "s3.bucket_count",
        kind: MetricKind::Gauge,
    },
    MetricMapping {
        prom_name: "minio_cluster_nodes_online_total",
        temps_name: "s3.nodes_online",
        kind: MetricKind::Gauge,
    },
    MetricMapping {
        prom_name: "minio_cluster_nodes_offline_total",
        temps_name: "s3.nodes_offline",
        kind: MetricKind::Gauge,
    },
    // RustFS may emit the same metrics under a rustfs_* prefix instead of
    // minio_* — include both so either deployment is covered.
    MetricMapping {
        prom_name: "rustfs_cluster_capacity_usable_total_bytes",
        temps_name: "s3.capacity_usable_total_bytes",
        kind: MetricKind::Gauge,
    },
    MetricMapping {
        prom_name: "rustfs_cluster_capacity_usable_free_bytes",
        temps_name: "s3.capacity_usable_free_bytes",
        kind: MetricKind::Gauge,
    },
    MetricMapping {
        prom_name: "rustfs_cluster_usage_total_bytes",
        temps_name: "s3.total_size_bytes",
        kind: MetricKind::Gauge,
    },
    MetricMapping {
        prom_name: "rustfs_cluster_usage_object_total",
        temps_name: "s3.object_count",
        kind: MetricKind::Gauge,
    },
    MetricMapping {
        prom_name: "rustfs_cluster_bucket_total",
        temps_name: "s3.bucket_count",
        kind: MetricKind::Gauge,
    },
    MetricMapping {
        prom_name: "rustfs_cluster_nodes_online_total",
        temps_name: "s3.nodes_online",
        kind: MetricKind::Gauge,
    },
    MetricMapping {
        prom_name: "rustfs_cluster_nodes_offline_total",
        temps_name: "s3.nodes_offline",
        kind: MetricKind::Gauge,
    },
];

/// Scrape a Prometheus text-exposition endpoint and parse the response.
///
/// `headers` are optional `(name, value)` pairs added to the request — used for
/// bearer tokens on protected `/metrics` endpoints. Pass `None` for an
/// unauthenticated scrape.
///
/// The whole request is bounded by `timeout`; a slow or unreachable endpoint
/// returns `Err` rather than blocking the scrape loop. The reqwest client is
/// also built with the same timeout as a defensive backstop.
///
/// # Errors
///
/// Returns `Err(String)` on connection failure, non-success HTTP status,
/// timeout, or body-read failure. Callers decide whether that is fatal — the
/// S3 collector treats it as non-fatal (the endpoint may not exist).
pub async fn scrape_prometheus(
    url: &str,
    headers: Option<&[(&str, &str)]>,
    timeout: Duration,
) -> Result<PrometheusMetrics, String> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .unwrap_or_default();

    let mut request = client.get(url);
    if let Some(pairs) = headers {
        for (name, value) in pairs {
            request = request.header(*name, *value);
        }
    }

    let send = async {
        let response = request
            .send()
            .await
            .map_err(|e| format!("Prometheus scrape request to {url} failed: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            return Err(format!(
                "Prometheus scrape to {url} returned non-success status {status}"
            ));
        }

        response
            .text()
            .await
            .map_err(|e| format!("Failed to read Prometheus response body from {url}: {e}"))
    };

    let body = tokio::time::timeout(timeout, send)
        .await
        .map_err(|_| format!("Prometheus scrape to {url} timed out after {timeout:?}"))??;

    Ok(parse_prometheus_text(&body))
}

/// Parse a Prometheus text-exposition body into a [`PrometheusMetrics`] lookup.
///
/// Comment lines (`# HELP`, `# TYPE`, and any other `#`-prefixed line) and blank
/// lines are skipped. For each sample line, the metric *name* (the token before
/// any `{` label block or whitespace) is extracted and associated with its
/// parsed value. The first sample for a given name wins; subsequent samples with
/// the same name (different label sets) are ignored.
fn parse_prometheus_text(body: &str) -> PrometheusMetrics {
    let mut values: HashMap<String, f64> = HashMap::new();

    for raw_line in body.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((name, value)) = parse_sample_line(line) else {
            warn!(line = %line, "skipping malformed Prometheus sample line");
            continue;
        };

        // First sample for a metric name wins.
        values.entry(name).or_insert(value);
    }

    PrometheusMetrics { values }
}

/// Parse a single Prometheus sample line into `(name, value)`.
///
/// Handles both labelled (`name{label="v"} 1.0`) and unlabelled
/// (`name 1.0`) forms, and tolerates an optional trailing timestamp
/// (`name 1.0 1700000000000`), which Prometheus permits but we discard.
fn parse_sample_line(line: &str) -> Option<(String, f64)> {
    // Split off the metric identifier (name plus optional `{labels}`) from the
    // value (and optional timestamp) by the last whitespace-separated tokens.
    // The identifier may contain spaces inside the label block, so we locate
    // the value by scanning from the right.
    let mut tokens = line.rsplitn(3, char::is_whitespace);
    let last = tokens.next()?;
    let second_last = tokens.next();
    let rest = tokens.next();

    // Determine which token is the value vs. an optional trailing timestamp.
    // Forms:
    //   "name value"                 -> rest=None,  second_last=Some(ident), last=value
    //   "name value timestamp"       -> rest=Some(ident), second_last=Some(value), last=timestamp
    let (identifier, value_str) = match (rest, second_last) {
        (Some(ident), Some(value)) => (ident, value),
        (None, Some(ident)) => (ident, last),
        _ => return None,
    };

    let name = metric_name_from_identifier(identifier)?;
    let value = parse_value(value_str);
    Some((name, value))
}

/// Extract the bare metric name from an identifier, stripping any label block.
///
/// `minio_cluster_bucket_total{server="a"}` -> `minio_cluster_bucket_total`
/// `minio_cluster_bucket_total`             -> `minio_cluster_bucket_total`
fn metric_name_from_identifier(identifier: &str) -> Option<String> {
    let name = match identifier.find('{') {
        Some(idx) => &identifier[..idx],
        None => identifier,
    };
    let name = name.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Parse a Prometheus sample value, normalizing non-finite values to `0.0`.
///
/// Prometheus permits the special tokens `NaN`, `+Inf`, `-Inf`, and `Inf`.
/// Any value that parses to a non-finite `f64` (or fails to parse) is coerced
/// to `0.0` so it never corrupts downstream aggregates.
pub fn parse_value(s: &str) -> f64 {
    let trimmed = s.trim();
    match trimmed.parse::<f64>() {
        Ok(v) if v.is_finite() => v,
        // `NaN`, `+Inf`, `-Inf`, `Inf`, or a non-finite parse result.
        _ => 0.0,
    }
}

/// Map a parsed Prometheus scrape onto [`MetricPoint`]s using `mappings`.
///
/// Only metric names present in both the scrape and the mapping table produce a
/// point; everything else is dropped. The `config` supplies `source_id`,
/// `environment`, `node_id`, and the engine label (always `"s3"` for the MinIO
/// path — the only current consumer).
pub fn apply_mappings(
    metrics: &PrometheusMetrics,
    mappings: &[MetricMapping],
    config: &CollectorConfig,
) -> Vec<MetricPoint> {
    let now = Utc::now();
    let mut points = Vec::new();

    for mapping in mappings {
        let Some(value) = metrics.get(mapping.prom_name) else {
            continue;
        };

        points.push(MetricPoint {
            time: now,
            source_kind: config.source_kind.clone(),
            source_id: config.source_id,
            name: mapping.temps_name.to_string(),
            value,
            kind: mapping.kind.clone(),
            engine: Some("s3".to_string()),
            environment: config.environment.clone(),
            node_id: config.node_id,
            labels: HashMap::new(),
        });
    }

    points
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SourceKind;

    const SAMPLE: &str = "\
# HELP minio_cluster_capacity_usable_total_bytes Total usable capacity online in the cluster
# TYPE minio_cluster_capacity_usable_total_bytes gauge
minio_cluster_capacity_usable_total_bytes 1.073741824e+11
# HELP minio_cluster_usage_total_bytes Total cluster usage in bytes
# TYPE minio_cluster_usage_total_bytes gauge
minio_cluster_usage_total_bytes{server=\"node-1\"} 5368709120
minio_cluster_usage_object_total 4096
minio_cluster_bucket_total 12
minio_cluster_nodes_online_total 4 1700000000000
minio_cluster_nodes_offline_total NaN
some_unmapped_metric 99
";

    fn make_config(source_id: i32) -> CollectorConfig {
        CollectorConfig {
            source_id,
            source_kind: SourceKind::Database,
            connection_string: "us-east-1|key|secret|http://localhost:9000".to_string(),
            environment: Some("production".to_string()),
            node_id: Some(3),
            timeout: Duration::from_secs(5),
        }
    }

    // ── parse_value ───────────────────────────────────────────────────────────

    #[test]
    fn test_parse_value_plain() {
        assert_eq!(parse_value("42"), 42.0);
        assert_eq!(parse_value("1.618"), 1.618);
        assert_eq!(parse_value("1.073741824e+11"), 1.073741824e11);
    }

    #[test]
    fn test_parse_value_nan_and_inf_normalized() {
        assert_eq!(parse_value("NaN"), 0.0);
        assert_eq!(parse_value("+Inf"), 0.0);
        assert_eq!(parse_value("-Inf"), 0.0);
        assert_eq!(parse_value("Inf"), 0.0);
    }

    #[test]
    fn test_parse_value_garbage_normalized() {
        assert_eq!(parse_value("not-a-number"), 0.0);
        assert_eq!(parse_value(""), 0.0);
    }

    // ── parse_sample_line ─────────────────────────────────────────────────────

    #[test]
    fn test_parse_sample_line_unlabelled() {
        let (name, value) = parse_sample_line("minio_cluster_bucket_total 12").expect("parses");
        assert_eq!(name, "minio_cluster_bucket_total");
        assert_eq!(value, 12.0);
    }

    #[test]
    fn test_parse_sample_line_labelled() {
        let (name, value) =
            parse_sample_line("minio_cluster_usage_total_bytes{server=\"node-1\"} 5368709120")
                .expect("parses");
        assert_eq!(name, "minio_cluster_usage_total_bytes");
        assert_eq!(value, 5_368_709_120.0);
    }

    #[test]
    fn test_parse_sample_line_with_timestamp() {
        let (name, value) =
            parse_sample_line("minio_cluster_nodes_online_total 4 1700000000000").expect("parses");
        assert_eq!(name, "minio_cluster_nodes_online_total");
        assert_eq!(value, 4.0);
    }

    // ── parse_prometheus_text ─────────────────────────────────────────────────

    #[test]
    fn test_parse_text_skips_comments_and_blanks() {
        let metrics = parse_prometheus_text(SAMPLE);
        assert_eq!(metrics.get("minio_cluster_bucket_total"), Some(12.0));
        assert_eq!(
            metrics.get("minio_cluster_usage_object_total"),
            Some(4096.0)
        );
        // Labelled sample's bare name is captured.
        assert_eq!(
            metrics.get("minio_cluster_usage_total_bytes"),
            Some(5_368_709_120.0)
        );
        // Timestamp is discarded; value preserved.
        assert_eq!(metrics.get("minio_cluster_nodes_online_total"), Some(4.0));
        // NaN normalized to 0.0.
        assert_eq!(metrics.get("minio_cluster_nodes_offline_total"), Some(0.0));
    }

    #[test]
    fn test_parse_text_first_sample_wins() {
        let body = "dup_metric 1\ndup_metric 2\n";
        let metrics = parse_prometheus_text(body);
        assert_eq!(metrics.get("dup_metric"), Some(1.0));
    }

    // ── apply_mappings ────────────────────────────────────────────────────────

    #[test]
    fn test_apply_mappings_maps_known_metrics() {
        let metrics = parse_prometheus_text(SAMPLE);
        let config = make_config(7);
        let points = apply_mappings(&metrics, RUSTFS_METRICS, &config);

        let names: Vec<&str> = points.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"s3.capacity_usable_total_bytes"));
        assert!(names.contains(&"s3.total_size_bytes"));
        assert!(names.contains(&"s3.object_count"));
        assert!(names.contains(&"s3.bucket_count"));
        assert!(names.contains(&"s3.nodes_online"));
        assert!(names.contains(&"s3.nodes_offline"));

        // Unmapped Prometheus metrics never produce a point.
        assert!(!names.contains(&"some_unmapped_metric"));
    }

    #[test]
    fn test_apply_mappings_propagates_config_fields() {
        let metrics = parse_prometheus_text(SAMPLE);
        let config = make_config(7);
        let points = apply_mappings(&metrics, RUSTFS_METRICS, &config);

        for p in &points {
            assert_eq!(p.source_id, 7);
            assert_eq!(p.engine.as_deref(), Some("s3"));
            assert_eq!(p.environment.as_deref(), Some("production"));
            assert_eq!(p.node_id, Some(3));
            assert_eq!(p.kind, MetricKind::Gauge);
        }
    }

    #[test]
    fn test_apply_mappings_value_correctness() {
        let metrics = parse_prometheus_text(SAMPLE);
        let config = make_config(1);
        let points = apply_mappings(&metrics, RUSTFS_METRICS, &config);

        let bucket = points
            .iter()
            .find(|p| p.name == "s3.bucket_count")
            .expect("bucket_count mapped");
        assert_eq!(bucket.value, 12.0);

        let offline = points
            .iter()
            .find(|p| p.name == "s3.nodes_offline")
            .expect("nodes_offline mapped");
        // NaN normalized to 0.0 upstream in parse_value.
        assert_eq!(offline.value, 0.0);
    }

    #[test]
    fn test_apply_mappings_empty_when_no_match() {
        let metrics = parse_prometheus_text("totally_unrelated_metric 5\n");
        let config = make_config(1);
        let points = apply_mappings(&metrics, RUSTFS_METRICS, &config);
        assert!(points.is_empty());
    }
}
