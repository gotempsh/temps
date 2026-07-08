//! Hot-path proxy metrics.
//!
//! [`ProxyMetrics`] is a set of lock-free atomic counters updated on every
//! request. Recording is a handful of `Relaxed` atomic adds — no locks, no
//! allocation, no I/O — so it is safe at 100k+ req/s. A background sampler
//! ([`crate::service::metrics_sampler::ProxyMetricsSampler`]) snapshots the
//! counters on an interval, computes deltas, and writes them to the metrics
//! store as `SourceKind::Node` points, where the existing
//! `GET /nodes/{id}/metrics` endpoint and the alert evaluator can read them.
//!
//! Cardinality is bounded by construction: status codes collapse to five
//! classes and durations to a fixed bucket ladder. No per-request attribute
//! (path, host, project) ever becomes a metric dimension — per-project traffic
//! breakdowns come from the proxy request logs instead.

use std::sync::atomic::{AtomicU64, Ordering};

/// Upper bounds (inclusive, in milliseconds) of the duration histogram
/// buckets. Requests slower than the last bound land in an overflow bucket.
pub const DURATION_BUCKETS_MS: [u64; 10] = [5, 10, 25, 50, 100, 250, 500, 1000, 2500, 5000];

/// Bucket count including the overflow bucket.
const NUM_BUCKETS: usize = DURATION_BUCKETS_MS.len() + 1;

/// Number of status classes tracked: 1xx, 2xx, 3xx, 4xx, 5xx.
const NUM_CLASSES: usize = 5;

/// Metric names emitted by [`MetricsDelta::samples`]. Deliberately avoid the
/// `_total`/`_count` suffixes: the read path treats those as raw cumulative
/// counters (`is_monotonic_counter`), but we store pre-computed deltas.
pub const METRIC_REQUESTS: &str = "proxy.requests";
pub const METRIC_REQUESTS_1XX: &str = "proxy.requests_1xx";
pub const METRIC_REQUESTS_2XX: &str = "proxy.requests_2xx";
pub const METRIC_REQUESTS_3XX: &str = "proxy.requests_3xx";
pub const METRIC_REQUESTS_4XX: &str = "proxy.requests_4xx";
pub const METRIC_REQUESTS_5XX: &str = "proxy.requests_5xx";
pub const METRIC_ERROR_RATE: &str = "proxy.error_rate_percent";
pub const METRIC_DURATION_AVG: &str = "proxy.request_duration_avg_ms";
pub const METRIC_DURATION_P50: &str = "proxy.request_duration_p50_ms";
pub const METRIC_DURATION_P95: &str = "proxy.request_duration_p95_ms";
pub const METRIC_DURATION_P99: &str = "proxy.request_duration_p99_ms";

/// Lock-free per-process proxy request counters.
///
/// All mutation goes through [`ProxyMetrics::record`]; all reads go through
/// [`ProxyMetrics::snapshot`]. `Relaxed` ordering is sufficient — each counter
/// is independent and the sampler only needs eventually-consistent totals.
#[derive(Debug, Default)]
pub struct ProxyMetrics {
    /// Requests by status class; index = (status / 100) - 1, clamped to 5xx.
    status_classes: [AtomicU64; NUM_CLASSES],
    /// Non-cumulative per-bucket duration counts (last = overflow).
    duration_buckets: [AtomicU64; NUM_BUCKETS],
    /// Sum of all observed durations in milliseconds.
    duration_sum_ms: AtomicU64,
}

impl ProxyMetrics {
    /// Record one completed request. Hot path: 3 relaxed atomic adds.
    pub fn record(&self, status_code: u16, elapsed_ms: u64) {
        let class = match status_code {
            100..=199 => 0,
            200..=299 => 1,
            300..=399 => 2,
            400..=499 => 3,
            // 5xx and anything malformed counts as a server error.
            _ => 4,
        };
        self.status_classes[class].fetch_add(1, Ordering::Relaxed);

        let bucket = DURATION_BUCKETS_MS
            .iter()
            .position(|bound| elapsed_ms <= *bound)
            .unwrap_or(NUM_BUCKETS - 1);
        self.duration_buckets[bucket].fetch_add(1, Ordering::Relaxed);
        self.duration_sum_ms
            .fetch_add(elapsed_ms, Ordering::Relaxed);
    }

    /// Read a consistent-enough view of all counters.
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            status_classes: std::array::from_fn(|i| self.status_classes[i].load(Ordering::Relaxed)),
            duration_buckets: std::array::from_fn(|i| {
                self.duration_buckets[i].load(Ordering::Relaxed)
            }),
            duration_sum_ms: self.duration_sum_ms.load(Ordering::Relaxed),
        }
    }
}

/// A point-in-time copy of the counters, used for delta computation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetricsSnapshot {
    status_classes: [u64; NUM_CLASSES],
    duration_buckets: [u64; NUM_BUCKETS],
    duration_sum_ms: u64,
}

impl MetricsSnapshot {
    /// Compute the per-interval delta `self - prev`.
    ///
    /// Saturating: if a counter appears to have gone backwards (only possible
    /// with a mismatched baseline), the delta clamps to zero instead of
    /// producing a garbage spike.
    pub fn delta_since(&self, prev: &MetricsSnapshot) -> MetricsDelta {
        MetricsDelta {
            status_classes: std::array::from_fn(|i| {
                self.status_classes[i].saturating_sub(prev.status_classes[i])
            }),
            duration_buckets: std::array::from_fn(|i| {
                self.duration_buckets[i].saturating_sub(prev.duration_buckets[i])
            }),
            duration_sum_ms: self.duration_sum_ms.saturating_sub(prev.duration_sum_ms),
        }
    }
}

/// One sample ready to be turned into a `MetricPoint` by the sampler.
#[derive(Debug, Clone, PartialEq)]
pub struct ProxySample {
    pub name: &'static str,
    pub value: f64,
    /// `true` = per-interval counter delta, `false` = gauge.
    pub is_counter: bool,
}

/// Counter deltas for one sampling interval.
#[derive(Debug, Clone, Default)]
pub struct MetricsDelta {
    status_classes: [u64; NUM_CLASSES],
    duration_buckets: [u64; NUM_BUCKETS],
    duration_sum_ms: u64,
}

impl MetricsDelta {
    /// Total requests in the interval.
    pub fn total_requests(&self) -> u64 {
        self.status_classes.iter().sum()
    }

    /// Build the samples to persist for this interval.
    ///
    /// Request counters are always emitted (a zero is meaningful — it draws a
    /// flat line instead of a gap). Duration/error-rate gauges are only
    /// emitted when at least one request completed, so idle intervals don't
    /// drag averages to zero.
    pub fn samples(&self) -> Vec<ProxySample> {
        let total = self.total_requests();
        let mut samples = vec![
            ProxySample {
                name: METRIC_REQUESTS,
                value: total as f64,
                is_counter: true,
            },
            ProxySample {
                name: METRIC_REQUESTS_1XX,
                value: self.status_classes[0] as f64,
                is_counter: true,
            },
            ProxySample {
                name: METRIC_REQUESTS_2XX,
                value: self.status_classes[1] as f64,
                is_counter: true,
            },
            ProxySample {
                name: METRIC_REQUESTS_3XX,
                value: self.status_classes[2] as f64,
                is_counter: true,
            },
            ProxySample {
                name: METRIC_REQUESTS_4XX,
                value: self.status_classes[3] as f64,
                is_counter: true,
            },
            ProxySample {
                name: METRIC_REQUESTS_5XX,
                value: self.status_classes[4] as f64,
                is_counter: true,
            },
        ];

        if total > 0 {
            samples.push(ProxySample {
                name: METRIC_ERROR_RATE,
                value: (self.status_classes[4] as f64 / total as f64) * 100.0,
                is_counter: false,
            });
            samples.push(ProxySample {
                name: METRIC_DURATION_AVG,
                value: self.duration_sum_ms as f64 / total as f64,
                is_counter: false,
            });
            samples.push(ProxySample {
                name: METRIC_DURATION_P50,
                value: self.percentile(0.50),
                is_counter: false,
            });
            samples.push(ProxySample {
                name: METRIC_DURATION_P95,
                value: self.percentile(0.95),
                is_counter: false,
            });
            samples.push(ProxySample {
                name: METRIC_DURATION_P99,
                value: self.percentile(0.99),
                is_counter: false,
            });
        }

        samples
    }

    /// Estimate the q-th percentile (0.0..=1.0) from the bucket histogram.
    ///
    /// Uses linear interpolation within the bucket that contains the target
    /// rank. The overflow bucket has no upper bound; it reports twice the last
    /// finite bound (a deliberate, documented over-estimate — better to alarm
    /// high than to hide tail latency).
    fn percentile(&self, q: f64) -> f64 {
        let total: u64 = self.duration_buckets.iter().sum();
        if total == 0 {
            return 0.0;
        }
        let target = (q * total as f64).ceil().max(1.0) as u64;

        let mut cumulative: u64 = 0;
        for (i, count) in self.duration_buckets.iter().enumerate() {
            if *count == 0 {
                continue;
            }
            let prev_cumulative = cumulative;
            cumulative += count;
            if cumulative >= target {
                let lower = if i == 0 {
                    0
                } else {
                    DURATION_BUCKETS_MS[i - 1]
                };
                let upper = if i < DURATION_BUCKETS_MS.len() {
                    DURATION_BUCKETS_MS[i]
                } else {
                    // Overflow bucket: no true upper bound.
                    DURATION_BUCKETS_MS[DURATION_BUCKETS_MS.len() - 1] * 2
                };
                let within = (target - prev_cumulative) as f64 / *count as f64;
                return lower as f64 + within * (upper - lower) as f64;
            }
        }
        // Unreachable when total > 0, but keep a sane fallback.
        DURATION_BUCKETS_MS[DURATION_BUCKETS_MS.len() - 1] as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_classifies_status_codes() {
        let m = ProxyMetrics::default();
        m.record(101, 1);
        m.record(200, 1);
        m.record(204, 1);
        m.record(301, 1);
        m.record(404, 1);
        m.record(500, 1);
        m.record(503, 1);
        // Malformed status counts as 5xx.
        m.record(0, 1);

        let s = m.snapshot();
        assert_eq!(s.status_classes, [1, 2, 1, 1, 3]);
    }

    #[test]
    fn test_record_buckets_durations() {
        let m = ProxyMetrics::default();
        m.record(200, 0); // <= 5ms bucket
        m.record(200, 5); // <= 5ms bucket (inclusive bound)
        m.record(200, 6); // <= 10ms bucket
        m.record(200, 99_999); // overflow bucket

        let s = m.snapshot();
        assert_eq!(s.duration_buckets[0], 2);
        assert_eq!(s.duration_buckets[1], 1);
        assert_eq!(s.duration_buckets[NUM_BUCKETS - 1], 1);
        assert_eq!(s.duration_sum_ms, 5 + 6 + 99_999);
    }

    #[test]
    fn test_delta_since_subtracts_baseline() {
        let m = ProxyMetrics::default();
        m.record(200, 10);
        let first = m.snapshot();

        m.record(200, 10);
        m.record(500, 200);
        let second = m.snapshot();

        let delta = second.delta_since(&first);
        assert_eq!(delta.total_requests(), 2);
        assert_eq!(delta.status_classes[1], 1);
        assert_eq!(delta.status_classes[4], 1);
        assert_eq!(delta.duration_sum_ms, 210);
    }

    #[test]
    fn test_delta_saturates_instead_of_underflowing() {
        let m = ProxyMetrics::default();
        m.record(200, 10);
        let later = m.snapshot();
        let m2 = ProxyMetrics::default();
        m2.record(200, 1);
        m2.record(200, 1);
        let earlier_but_bigger = m2.snapshot();

        let delta = later.delta_since(&earlier_but_bigger);
        // 1 - 2 clamps to 0 rather than wrapping.
        assert_eq!(delta.status_classes[1], 0);
    }

    #[test]
    fn test_samples_idle_interval_emits_only_counters() {
        let delta = MetricsDelta::default();
        let samples = delta.samples();
        assert_eq!(samples.len(), 6);
        assert!(samples.iter().all(|s| s.is_counter && s.value == 0.0));
    }

    #[test]
    fn test_samples_active_interval_emits_gauges() {
        let m = ProxyMetrics::default();
        m.record(200, 10);
        m.record(500, 30);
        let delta = m.snapshot().delta_since(&MetricsSnapshot::default());

        let samples = delta.samples();
        let get = |name: &str| {
            samples
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("missing sample {name}"))
                .value
        };

        assert_eq!(get(METRIC_REQUESTS), 2.0);
        assert_eq!(get(METRIC_REQUESTS_2XX), 1.0);
        assert_eq!(get(METRIC_REQUESTS_5XX), 1.0);
        assert_eq!(get(METRIC_ERROR_RATE), 50.0);
        assert_eq!(get(METRIC_DURATION_AVG), 20.0);
        // Gauge samples must not be flagged as counters.
        assert!(
            !samples
                .iter()
                .find(|s| s.name == METRIC_ERROR_RATE)
                .expect("error rate present")
                .is_counter
        );
    }

    #[test]
    fn test_percentile_single_bucket_interpolates() {
        let m = ProxyMetrics::default();
        // 100 requests all in the (10, 25] bucket.
        for _ in 0..100 {
            m.record(200, 20);
        }
        let delta = m.snapshot().delta_since(&MetricsSnapshot::default());

        let p50 = delta.percentile(0.50);
        let p99 = delta.percentile(0.99);
        // Interpolated within bucket bounds (10, 25].
        assert!(p50 > 10.0 && p50 <= 25.0, "p50 = {p50}");
        assert!(p99 > p50 && p99 <= 25.0, "p99 = {p99}");
    }

    #[test]
    fn test_percentile_spread_orders_correctly() {
        let m = ProxyMetrics::default();
        for _ in 0..90 {
            m.record(200, 3); // fast
        }
        for _ in 0..9 {
            m.record(200, 400); // slow
        }
        m.record(200, 9000); // overflow tail
        let delta = m.snapshot().delta_since(&MetricsSnapshot::default());

        let p50 = delta.percentile(0.50);
        let p95 = delta.percentile(0.95);
        let p99 = delta.percentile(0.99);
        // rank 100 of 100 -> overflow bucket, reported above the last bound.
        let p100 = delta.percentile(1.0);
        assert!(p50 <= 5.0, "p50 = {p50}");
        assert!(p95 > 250.0 && p95 <= 500.0, "p95 = {p95}");
        // rank 99 of 100 is the last request in the (250, 500] bucket.
        assert!(p99 > 250.0 && p99 <= 500.0, "p99 = {p99}");
        assert!(p100 > 5000.0, "p100 = {p100}");
        assert!(p50 <= p95 && p95 <= p99 && p99 <= p100);
    }

    #[test]
    fn test_percentile_empty_is_zero() {
        assert_eq!(MetricsDelta::default().percentile(0.99), 0.0);
    }
}
