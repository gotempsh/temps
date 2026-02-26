//! Tail-based trace sampler.
//!
//! Sampling decisions:
//! - 100% of error traces (any span has ERROR status)
//! - 100% of traces exceeding P95 latency threshold
//! - 1% of remaining traces

use crate::types::{SpanRecord, SpanStatusCode};

/// Tail-based sampler configuration.
#[derive(Debug, Clone)]
pub struct SamplerConfig {
    /// Percentage of non-error, non-slow traces to keep (0.0 - 1.0).
    pub base_sample_rate: f64,
    /// P95 latency threshold in ms. Traces exceeding this are always kept.
    pub latency_threshold_ms: f64,
}

impl Default for SamplerConfig {
    fn default() -> Self {
        Self {
            base_sample_rate: 1.0,        // 100% — keep all traces by default
            latency_threshold_ms: 1000.0, // default 1s until P95 is computed
        }
    }
}

/// Tail-based trace sampler.
pub struct TraceSampler {
    config: SamplerConfig,
}

impl TraceSampler {
    pub fn new(config: SamplerConfig) -> Self {
        Self { config }
    }

    /// Update the P95 latency threshold (called periodically from the anomaly detector).
    pub fn update_latency_threshold(&mut self, p95_ms: f64) {
        self.config.latency_threshold_ms = p95_ms;
    }

    /// Apply tail-based sampling to a batch of spans.
    ///
    /// Groups spans by trace_id and makes per-trace sampling decisions:
    /// - Keep 100% of traces with any ERROR span
    /// - Keep 100% of traces exceeding P95 latency
    /// - Keep `base_sample_rate`% of remaining traces
    ///
    /// Returns (kept_spans, sampled_out_count).
    pub fn sample(&self, spans: Vec<SpanRecord>) -> (Vec<SpanRecord>, u64) {
        if spans.is_empty() {
            return (Vec::new(), 0);
        }

        // Group spans by trace_id
        let mut traces: std::collections::HashMap<String, Vec<SpanRecord>> =
            std::collections::HashMap::new();
        for span in spans {
            traces.entry(span.trace_id.clone()).or_default().push(span);
        }

        let mut kept = Vec::new();
        let mut sampled_out: u64 = 0;

        for (_trace_id, trace_spans) in traces {
            let decision = self.decide_trace(&trace_spans);

            match decision {
                SampleDecision::Keep => {
                    kept.extend(trace_spans);
                }
                SampleDecision::Drop => {
                    sampled_out += trace_spans.len() as u64;
                }
            }
        }

        (kept, sampled_out)
    }

    fn decide_trace(&self, spans: &[SpanRecord]) -> SampleDecision {
        // Rule 1: Always keep error traces
        if spans.iter().any(|s| s.status_code == SpanStatusCode::Error) {
            return SampleDecision::Keep;
        }

        // Rule 2: Always keep traces exceeding P95 latency
        let max_duration = spans.iter().map(|s| s.duration_ms).fold(0.0f64, f64::max);

        if max_duration > self.config.latency_threshold_ms {
            return SampleDecision::Keep;
        }

        // Rule 3: Probabilistic sampling of remaining traces
        // Use trace_id hash for deterministic sampling
        let hash = simple_hash(&spans[0].trace_id);
        let sample_threshold = (self.config.base_sample_rate * u64::MAX as f64) as u64;

        if hash < sample_threshold {
            SampleDecision::Keep
        } else {
            SampleDecision::Drop
        }
    }
}

#[derive(Debug, PartialEq)]
enum SampleDecision {
    Keep,
    Drop,
}

/// Simple deterministic hash for sampling decisions.
fn simple_hash(s: &str) -> u64 {
    let mut hash: u64 = 5381;
    for byte in s.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(byte as u64);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;
    use chrono::Utc;
    use std::collections::BTreeMap;

    fn make_span(trace_id: &str, status: SpanStatusCode, duration_ms: f64) -> SpanRecord {
        SpanRecord {
            project_id: 1,
            deployment_id: None,
            resource: ResourceInfo::default(),
            trace_id: trace_id.to_string(),
            span_id: "span1".to_string(),
            parent_span_id: None,
            name: "test".to_string(),
            kind: SpanKind::Server,
            start_time: Utc::now(),
            end_time: Utc::now(),
            duration_ms,
            status_code: status,
            status_message: String::new(),
            attributes: BTreeMap::new(),
            events: Vec::new(),
        }
    }

    #[test]
    fn test_error_traces_always_kept() {
        let sampler = TraceSampler::new(SamplerConfig {
            base_sample_rate: 0.0, // Keep 0% of normal traces
            latency_threshold_ms: 10000.0,
        });

        let spans = vec![make_span("trace1", SpanStatusCode::Error, 10.0)];
        let (kept, _) = sampler.sample(spans);
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn test_slow_traces_always_kept() {
        let sampler = TraceSampler::new(SamplerConfig {
            base_sample_rate: 0.0,
            latency_threshold_ms: 100.0,
        });

        let spans = vec![make_span("trace1", SpanStatusCode::Ok, 200.0)];
        let (kept, _) = sampler.sample(spans);
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn test_normal_traces_sampled() {
        let sampler = TraceSampler::new(SamplerConfig {
            base_sample_rate: 0.0,
            latency_threshold_ms: 10000.0,
        });

        let spans = vec![make_span("trace1", SpanStatusCode::Ok, 10.0)];
        let (kept, sampled_out) = sampler.sample(spans);
        // With 0% sample rate, all normal traces should be dropped
        assert_eq!(kept.len(), 0);
        assert_eq!(sampled_out, 1);
    }

    #[test]
    fn test_empty_input() {
        let sampler = TraceSampler::new(SamplerConfig::default());
        let (kept, sampled_out) = sampler.sample(Vec::new());
        assert_eq!(kept.len(), 0);
        assert_eq!(sampled_out, 0);
    }
}
