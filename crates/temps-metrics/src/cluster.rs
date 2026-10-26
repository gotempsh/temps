// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Combining node series into one "all nodes" view (ADR 061).
//!
//! Every node writes the same `node.*` / `proxy.*` series under its own id.
//! The console's "All nodes" view reads each node's series over the same
//! window and step — so their buckets line up — and folds them per bucket by
//! what the metric means:
//!
//! | metric | all nodes |
//! |---|---|
//! | request counters, cumulative I/O counters, byte and descriptor counts | sum |
//! | `proxy.error_rate_percent`, `node.memory_percent`, `node.disk_percent`, `node.fd_percent` | ratio of the summed parts |
//! | latencies (`*_ms`) | the slowest node |
//! | everything else (`node.cpu_percent`, load averages) | mean of the nodes that reported |
//!
//! A ratio of sums, not a mean of ratios: one idle node answering its only
//! request with a 502 must not read as a 50% cluster error rate. Latency
//! percentiles cannot be merged from per-node percentiles, so the view shows
//! the slowest node rather than a number nobody measured.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};

use crate::store::{is_delta_counter, is_monotonic_counter};

/// How one metric's per-node values fold into the cluster value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClusterAggregate {
    Sum,
    Mean,
    Max,
    /// `scale × Σ numerator / Σ denominator`, read from two other series.
    Ratio {
        numerator: &'static str,
        denominator: &'static str,
        scale: f64,
    },
}

/// The fold for `metric`. See the module docs for the table.
pub fn cluster_aggregate(metric: &str) -> ClusterAggregate {
    match metric {
        "proxy.error_rate_percent" => ClusterAggregate::Ratio {
            numerator: "proxy.requests_5xx",
            denominator: "proxy.requests",
            scale: 100.0,
        },
        "node.memory_percent" => ClusterAggregate::Ratio {
            numerator: "node.memory_used_bytes",
            denominator: "node.memory_total_bytes",
            scale: 100.0,
        },
        "node.disk_percent" => ClusterAggregate::Ratio {
            numerator: "node.disk_used_bytes",
            denominator: "node.disk_total_bytes",
            scale: 100.0,
        },
        "node.fd_percent" => ClusterAggregate::Ratio {
            numerator: "node.fd_allocated",
            denominator: "node.fd_max",
            scale: 100.0,
        },
        m if is_delta_counter(m)
            || is_monotonic_counter(m)
            || m.ends_with("_bytes")
            || matches!(
                m,
                "node.fd_allocated"
                    | "node.fd_max"
                    | "node.process_open_fds"
                    | "node.process_fd_limit"
            ) =>
        {
            ClusterAggregate::Sum
        }
        m if m.ends_with("_ms") => ClusterAggregate::Max,
        _ => ClusterAggregate::Mean,
    }
}

type Series = Vec<(DateTime<Utc>, Option<f64>)>;

fn fold(values: &[f64], how: ClusterAggregate) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    match how {
        ClusterAggregate::Sum => Some(values.iter().sum()),
        ClusterAggregate::Max => values.iter().copied().reduce(f64::max),
        // A ratio's own series only reaches here through `combine_latest`
        // when one of its parts is missing; the mean is the honest fallback.
        ClusterAggregate::Mean | ClusterAggregate::Ratio { .. } => {
            Some(values.iter().sum::<f64>() / values.len() as f64)
        }
    }
}

/// Fold per-node series bucket by bucket. A bucket where no node has a value
/// stays `None`, so the chart still shows the gap.
pub fn combine_series(series: &[Series], how: ClusterAggregate) -> Series {
    let mut buckets: BTreeMap<DateTime<Utc>, Vec<f64>> = BTreeMap::new();
    for s in series {
        for (time, value) in s {
            let slot = buckets.entry(*time).or_default();
            if let Some(v) = value {
                slot.push(*v);
            }
        }
    }
    buckets
        .into_iter()
        .map(|(time, values)| (time, fold(&values, how)))
        .collect()
}

/// `scale × numerator / denominator` per bucket of two already-combined
/// series. Undefined (`None`) where either part is missing or the
/// denominator is zero.
pub fn ratio_series(numerator: &Series, denominator: &Series, scale: f64) -> Series {
    let num: BTreeMap<DateTime<Utc>, Option<f64>> = numerator.iter().copied().collect();
    let mut out: BTreeMap<DateTime<Utc>, Option<f64>> = BTreeMap::new();
    for (time, den) in denominator {
        let value = match (num.get(time).copied().flatten(), den) {
            (Some(n), Some(d)) if *d > 0.0 => Some(scale * n / d),
            _ => None,
        };
        out.insert(*time, value);
    }
    for time in num.keys() {
        out.entry(*time).or_insert(None);
    }
    out.into_iter().collect()
}

/// Fold each node's latest values into one map, metric by metric.
pub fn combine_latest(per_node: &[HashMap<String, f64>]) -> HashMap<String, f64> {
    let mut values: HashMap<&str, Vec<f64>> = HashMap::new();
    for node in per_node {
        for (name, value) in node {
            if value.is_finite() {
                values.entry(name.as_str()).or_default().push(*value);
            }
        }
    }
    let mut out: HashMap<String, f64> = HashMap::new();
    for (name, vals) in &values {
        let how = cluster_aggregate(name);
        if let ClusterAggregate::Ratio {
            numerator,
            denominator,
            scale,
        } = how
        {
            let num = values
                .get(numerator)
                .and_then(|v| fold(v, ClusterAggregate::Sum));
            let den = values
                .get(denominator)
                .and_then(|v| fold(v, ClusterAggregate::Sum));
            if let (Some(n), Some(d)) = (num, den) {
                if d > 0.0 {
                    out.insert(name.to_string(), scale * n / d);
                    continue;
                }
            }
        }
        if let Some(v) = fold(vals, how) {
            out.insert(name.to_string(), v);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(i: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + i * 60, 0).unwrap()
    }

    #[test]
    fn folds_follow_metric_meaning() {
        assert_eq!(cluster_aggregate("proxy.requests"), ClusterAggregate::Sum);
        assert_eq!(
            cluster_aggregate("proxy.requests_5xx"),
            ClusterAggregate::Sum
        );
        assert_eq!(
            cluster_aggregate("node.network_rx_bytes_total"),
            ClusterAggregate::Sum
        );
        assert_eq!(
            cluster_aggregate("node.memory_used_bytes"),
            ClusterAggregate::Sum
        );
        assert_eq!(
            cluster_aggregate("proxy.request_duration_p95_ms"),
            ClusterAggregate::Max
        );
        assert_eq!(
            cluster_aggregate("node.cpu_percent"),
            ClusterAggregate::Mean
        );
        assert!(matches!(
            cluster_aggregate("proxy.error_rate_percent"),
            ClusterAggregate::Ratio { .. }
        ));
        assert!(matches!(
            cluster_aggregate("node.disk_percent"),
            ClusterAggregate::Ratio { .. }
        ));
    }

    #[test]
    fn combine_keeps_gaps_and_folds_present_values() {
        let a = vec![(t(0), Some(1.0)), (t(1), None), (t(2), Some(4.0))];
        let b = vec![(t(0), Some(3.0)), (t(1), None), (t(2), None)];
        let summed = combine_series(&[a.clone(), b.clone()], ClusterAggregate::Sum);
        assert_eq!(
            summed,
            vec![(t(0), Some(4.0)), (t(1), None), (t(2), Some(4.0))]
        );
        let mean = combine_series(&[a.clone(), b.clone()], ClusterAggregate::Mean);
        assert_eq!(mean[0].1, Some(2.0));
        assert_eq!(mean[2].1, Some(4.0));
        let max = combine_series(&[a, b], ClusterAggregate::Max);
        assert_eq!(max[0].1, Some(3.0));
    }

    #[test]
    fn error_rate_is_a_ratio_of_sums_not_a_mean_of_rates() {
        // Node A: 1000 requests, 0 errors. Node B: 1 request, 1 error.
        let requests = combine_series(
            &[vec![(t(0), Some(1000.0))], vec![(t(0), Some(1.0))]],
            ClusterAggregate::Sum,
        );
        let errors = combine_series(
            &[vec![(t(0), Some(0.0))], vec![(t(0), Some(1.0))]],
            ClusterAggregate::Sum,
        );
        let rate = ratio_series(&errors, &requests, 100.0);
        let value = rate[0].1.unwrap();
        assert!((value - 100.0 / 1001.0).abs() < 1e-9, "{value}");
    }

    #[test]
    fn ratio_is_undefined_without_a_denominator() {
        let rate = ratio_series(
            &vec![(t(0), Some(0.0)), (t(1), Some(1.0))],
            &vec![(t(0), Some(0.0)), (t(1), None)],
            100.0,
        );
        assert_eq!(rate, vec![(t(0), None), (t(1), None)]);
    }

    #[test]
    fn latest_sums_capacity_and_recomputes_percentages() {
        let a = HashMap::from([
            ("node.memory_used_bytes".to_string(), 2.0),
            ("node.memory_total_bytes".to_string(), 8.0),
            ("node.memory_percent".to_string(), 25.0),
            ("node.cpu_percent".to_string(), 10.0),
        ]);
        let b = HashMap::from([
            ("node.memory_used_bytes".to_string(), 6.0),
            ("node.memory_total_bytes".to_string(), 8.0),
            ("node.memory_percent".to_string(), 75.0),
            ("node.cpu_percent".to_string(), 30.0),
        ]);
        let all = combine_latest(&[a, b]);
        assert_eq!(all["node.memory_used_bytes"], 8.0);
        assert_eq!(all["node.memory_total_bytes"], 16.0);
        assert_eq!(all["node.memory_percent"], 50.0);
        assert_eq!(all["node.cpu_percent"], 20.0);
    }

    #[test]
    fn latest_ratio_falls_back_to_the_mean_without_its_parts() {
        let a = HashMap::from([("node.disk_percent".to_string(), 20.0)]);
        let b = HashMap::from([("node.disk_percent".to_string(), 40.0)]);
        assert_eq!(combine_latest(&[a, b])["node.disk_percent"], 30.0);
    }
}
