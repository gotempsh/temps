//! Backtest / band preview for alert rules.
//!
//! Replays a metric over a time range against the SAME detector logic the
//! evaluator uses, so a "would this have fired?" preview can never diverge from
//! what production would actually do — the band comes from [`BandModel`] and
//! static thresholds go through [`Comparator::breaches`], the same call the
//! evaluator makes. Values are mapped through the same `value_for_rule` so
//! counter rates / histogram percentiles compare like-for-like.

use chrono::{DateTime, Duration, Utc};

use crate::detectors::{
    AnomalyParams, BandModel, Comparator, DEFAULT_LOOKBACK_DAYS, MIN_BASELINE_SAMPLES,
};
use crate::error::OtelError;
use crate::services::metric_alert_evaluator::value_for_rule;
use crate::services::OtelService;
use crate::types::{MetricAggregation, MetricQuery};

/// Hard cap on buckets scored in one backtest.
///
/// The range and bucket width are caller-supplied and every bucket becomes a
/// point in the response, so without a ceiling an over-wide request turns into
/// an unbounded hypertable scan and an enormous response body. 10k points is
/// far more than any chart renders or any judgement about noisiness needs.
pub const MAX_PREVIEW_POINTS: u64 = 10_000;

/// One point in the backtest: the value, the band around it, and whether it
/// breached.
pub struct PreviewPoint {
    pub bucket: DateTime<Utc>,
    pub value: f64,
    pub lower: f64,
    pub upper: f64,
    pub breaching: bool,
}

/// The result of replaying a metric against an anomaly band.
pub struct AnomalyPreview {
    pub points: Vec<PreviewPoint>,
    pub breach_count: i64,
    pub baseline_samples: i64,
    /// Whether the baseline had enough samples to be trustworthy.
    pub sufficient: bool,
}

/// Compute the anomaly band over `[start, end]` plus which points would breach.
#[allow(clippy::too_many_arguments)]
pub async fn compute_anomaly_preview(
    otel: &OtelService,
    project_id: i32,
    metric_name: &str,
    aggregation_str: &str,
    window_secs: i32,
    params: &AnomalyParams,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<AnomalyPreview, OtelError> {
    let aggregation = MetricAggregation::parse(aggregation_str);
    let interval = format!("{}s", window_secs.max(1));

    // 1. Baseline over the lookback ending at `end`, mapped through the rule's
    //    aggregation so each baseline bucket is the same quantity as the points.
    let lookback_days = params
        .baseline_lookback_days
        .unwrap_or(DEFAULT_LOOKBACK_DAYS)
        .clamp(1, 90);
    let baseline_buckets = otel
        .query_metrics(MetricQuery {
            project_id,
            metric_name: Some(metric_name.to_string()),
            start_time: Some(end - Duration::days(lookback_days as i64)),
            end_time: Some(end),
            bucket_interval: Some(interval.clone()),
            limit: Some(MAX_PREVIEW_POINTS),
            aggregation,
            ..Default::default()
        })
        .await?;
    let baseline: Vec<(DateTime<Utc>, f64)> = baseline_buckets
        .iter()
        .map(|b| (b.bucket, value_for_rule(b, aggregation)))
        .collect();
    let band = BandModel::from_baseline(&baseline, params.seasonality, MIN_BASELINE_SAMPLES);

    // 2. Display series over [start, end], scored against the band.
    let display = otel
        .query_metrics(MetricQuery {
            project_id,
            metric_name: Some(metric_name.to_string()),
            start_time: Some(start),
            end_time: Some(end),
            bucket_interval: Some(interval),
            limit: Some(MAX_PREVIEW_POINTS),
            aggregation,
            ..Default::default()
        })
        .await?;

    let mut points = Vec::with_capacity(display.len());
    let mut breach_count = 0i64;
    for b in &display {
        let value = value_for_rule(b, aggregation);
        let (lower, upper, breaching) = match band.band_for(b.bucket) {
            Some((center, scale)) => {
                let breaching = band
                    .breaches(b.bucket, value, params.deviations, params.direction)
                    .unwrap_or(false);
                (
                    center - params.deviations * scale,
                    center + params.deviations * scale,
                    breaching,
                )
            }
            // No usable band at this point — draw nothing, never breach.
            None => (value, value, false),
        };
        if breaching {
            breach_count += 1;
        }
        points.push(PreviewPoint {
            bucket: b.bucket,
            value,
            lower,
            upper,
            breaching,
        });
    }

    Ok(AnomalyPreview {
        points,
        breach_count,
        baseline_samples: band.samples as i64,
        sufficient: band.samples >= MIN_BASELINE_SAMPLES,
    })
}

/// Backtest a **static threshold** over `[start, end]`.
///
/// "Would this have fired?" matters at least as much for a static rule as for
/// an anomaly band — arguably more, because the threshold is a number a person
/// (or a model) picked, with nothing but judgement behind it. Answering it
/// before the rule is created is what separates a grounded threshold from a
/// guess, and it is the only way to notice that a proposed rule would have
/// fired on every single bucket, or never once.
///
/// Breaches go through [`Comparator::breaches`] — the same call the evaluator
/// makes — so the preview cannot drift from production behaviour.
///
/// `lower`/`upper` are both the threshold: a static rule has no band, and
/// repeating the constant keeps one response shape for both detector families
/// (a flat line is also exactly what a chart wants to draw).
#[allow(clippy::too_many_arguments)]
pub async fn compute_static_preview(
    otel: &OtelService,
    project_id: i32,
    metric_name: &str,
    aggregation_str: &str,
    window_secs: i32,
    comparator: Comparator,
    threshold: f64,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<AnomalyPreview, OtelError> {
    let aggregation = MetricAggregation::parse(aggregation_str);
    let interval = format!("{}s", window_secs.max(1));

    let display = otel
        .query_metrics(MetricQuery {
            project_id,
            metric_name: Some(metric_name.to_string()),
            start_time: Some(start),
            end_time: Some(end),
            bucket_interval: Some(interval),
            limit: Some(MAX_PREVIEW_POINTS),
            aggregation,
            ..Default::default()
        })
        .await?;

    let mut points = Vec::with_capacity(display.len());
    let mut breach_count = 0i64;
    for b in &display {
        let value = value_for_rule(b, aggregation);
        let breaching = comparator.breaches(value, threshold);
        if breaching {
            breach_count += 1;
        }
        points.push(PreviewPoint {
            bucket: b.bucket,
            value,
            lower: threshold,
            upper: threshold,
            breaching,
        });
    }

    // A static threshold needs no baseline, so there is nothing that could be
    // "insufficient" — but an empty range still cannot answer the question, and
    // reporting that as a confident "would never have fired" would be a lie.
    let sample_count = points.len() as i64;
    Ok(AnomalyPreview {
        points,
        breach_count,
        baseline_samples: sample_count,
        sufficient: sample_count > 0,
    })
}
