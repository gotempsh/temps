//! Background evaluator for first-class metric alert rules.
//!
//! On a fixed interval (~30s) the evaluator scans every enabled rule, queries the
//! latest aggregated bucket for the rule's metric over its window via the same
//! `OtelService::query_metrics` path the explorer uses, compares the value against
//! the threshold, and tracks an `ok <-> firing` state machine. A breach only
//! transitions `ok -> firing` after it has persisted for `for_duration_secs`.
//!
//! Firing/resolving reuses `temps_monitoring::AlarmService` (the same path the
//! monitoring evaluator uses): it inserts the alarm row, enforces a cooldown,
//! fans out the notification to Slack/webhook/email, and emits a `Job::AlarmFired`.
//! We do NOT build a new notifier.
//!
//! Resilience: a query failure for one rule logs and continues; the loop never
//! panics. `for_duration` is approximated with an in-memory `breach_start` map
//! (lost on restart, matching the monitoring evaluator's approach), but
//! `last_state`/`last_value`/`last_evaluated_at` are persisted so the UI badge
//! survives restarts.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::json;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use temps_monitoring::{AlarmService, AlarmSeverity, AlarmType, FireAlarmRequest};

use crate::detectors::{
    AnomalyParams, BandModel, Comparator, DetectionConfig, StaticParams, DEFAULT_LOOKBACK_DAYS,
    MIN_BAND_SCALE, MIN_BASELINE_SAMPLES,
};
use crate::services::metric_alert_service::MetricAlertService;
use crate::services::OtelService;
use crate::types::{MetricAggregation, MetricBucket, MetricQuery};
use temps_entities::metric_alert_rules::Model as AlertRule;

/// How often the evaluator scans enabled rules.
const EVAL_INTERVAL_SECS: u64 = 30;

/// How long a per-rule anomaly baseline is cached before refetching from
/// storage. The band moves slowly, so the hot 30s tick scores the current value
/// against the cached baseline instead of re-querying the full lookback window.
const BASELINE_REFRESH_SECS: u64 = 3600;

/// Hard cap on the optional AI summary call (ADR-021 Tier 2). Bounds the extra
/// latency added to a fire when the feature is enabled; on timeout the
/// deterministic Tier-1 message is kept.
const AI_SUMMARY_TIMEOUT: Duration = Duration::from_secs(4);

/// The state transition the evaluator should apply for a single rule this cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertTransition {
    /// Not breaching, was already ok: nothing to do.
    StayOk,
    /// Newly breaching (or still breaching) but `for_duration` not yet elapsed.
    StartBreach,
    /// Breach has persisted long enough: transition ok -> firing.
    FireNow,
    /// Already firing and still breaching: nothing to do.
    StayFiring,
    /// Was firing, now recovered: transition firing -> ok.
    Resolve,
}

/// Pure state-machine function (no DB, no I/O) — unit-testable.
///
/// `prev_state` is the persisted `last_state` (`ok|firing|unknown`).
/// `breaching` is the result of [`compare`]. `breach_elapsed_secs` is how long the
/// current breach has persisted (0 if not breaching). Once the breach has
/// persisted for `>= for_duration_secs`, `ok -> firing` fires.
pub fn evaluate_transition(
    prev_state: &str,
    breaching: bool,
    breach_elapsed_secs: u64,
    for_duration_secs: u64,
) -> AlertTransition {
    let was_firing = prev_state == "firing";
    if breaching {
        if was_firing {
            AlertTransition::StayFiring
        } else if breach_elapsed_secs >= for_duration_secs {
            AlertTransition::FireNow
        } else {
            AlertTransition::StartBreach
        }
    } else if was_firing {
        AlertTransition::Resolve
    } else {
        AlertTransition::StayOk
    }
}

/// The value to evaluate for a rule against its threshold.
///
/// For percentile aggregations on a histogram metric, the quantile is computed
/// from the explicit bucket layout (`histogram_summary`) — the server's scalar
/// `value` for a percentile is the quantile of the synthetic per-point means,
/// not the true distribution. All other cases use the server-computed `value`.
pub(crate) fn value_for_rule(latest: &MetricBucket, aggregation: MetricAggregation) -> f64 {
    if let MetricAggregation::Quantile(q) = aggregation {
        if let Some(hs) = &latest.histogram_summary {
            if !hs.bounds.is_empty() {
                return histogram_quantile(&hs.bounds, &hs.bucket_counts, q);
            }
        }
    }
    latest.value
}

/// Quantile from an explicit-bucket histogram via linear interpolation within
/// the bucket crossing the target rank (same method as the frontend). `bounds`
/// are ascending upper bounds; `counts` has length `bounds.len() + 1`.
fn histogram_quantile(bounds: &[f64], counts: &[u64], q: f64) -> f64 {
    let total: u64 = counts.iter().sum();
    if total == 0 {
        return 0.0;
    }
    let target = q.clamp(0.0, 1.0) * total as f64;
    let mut cumulative: u64 = 0;
    for (i, &c) in counts.iter().enumerate() {
        let next = cumulative + c;
        if next as f64 >= target && c > 0 {
            let lower = if i == 0 { 0.0 } else { bounds[i - 1] };
            let upper = if i < bounds.len() {
                bounds[i]
            } else {
                bounds.last().copied().unwrap_or(lower)
            };
            let within = (target - cumulative as f64) / c as f64;
            return lower + (upper - lower) * within;
        }
        cumulative = next;
    }
    bounds.last().copied().unwrap_or(0.0)
}

/// A number formatted compactly for chart labels (100000 -> "100.0k").
fn fmt_compact(v: f64) -> String {
    let a = v.abs();
    if a >= 1e9 {
        format!("{:.1}B", v / 1e9)
    } else if a >= 1e6 {
        format!("{:.1}M", v / 1e6)
    } else if a >= 1e3 {
        format!("{:.1}k", v / 1e3)
    } else if a >= 10.0 {
        format!("{:.0}", v)
    } else {
        format!("{:.2}", v)
    }
}

/// Escape text for inline SVG `<text>` content.
fn svg_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Format a UTC timestamp for an x-axis tick. `with_date` prepends the calendar
/// day (used on the left tick) so a chart read days later is still unambiguous;
/// otherwise just `HH:MM`. Zero-padded specifiers only, for cross-platform output.
fn fmt_axis_time(t: DateTime<Utc>, with_date: bool) -> String {
    if with_date {
        t.format("%b %d %H:%M").to_string()
    } else {
        t.format("%H:%M").to_string()
    }
}

// --- Plain-language notification text (ADR-021 Tier 1) -----------------------
//
// The alert notification leads with a humanized sentence built deterministically
// from the figures the detector already computed. No AI, no I/O — pure functions,
// unit-tested. The exact statistics stay in the alarm `metadata` (and the email
// DETAILS table), so nothing is lost for the math-minded.

/// Past-tense verb describing how the rule's aggregation reduced the window,
/// e.g. `avg` -> "averaged", so the sentence reads naturally.
fn agg_verb(aggregation: &str) -> &'static str {
    match aggregation.to_ascii_lowercase().as_str() {
        "avg" | "average" | "mean" => "averaged",
        "max" | "maximum" => "peaked at",
        "min" | "minimum" => "bottomed out at",
        "sum" | "total" => "totaled",
        "count" => "counted",
        _ => "was", // percentiles / anything else
    }
}

/// The rule window in human units, e.g. "over the last minute" / "over the last
/// 5 minutes" / "over the last hour".
fn window_phrase(window_secs: i64) -> String {
    let w = window_secs.max(1);
    let (n, unit) = if w % 3600 == 0 {
        (w / 3600, "hour")
    } else if w % 60 == 0 {
        (w / 60, "minute")
    } else {
        (w, "second")
    };
    if n == 1 {
        format!("over the last {unit}")
    } else {
        format!("over the last {n} {unit}s")
    }
}

/// A multiplier label that drops a redundant decimal, e.g. 4.13 -> "4×",
/// 4.25 -> "4.2×".
fn times_label(ratio: f64) -> String {
    if (ratio - ratio.round()).abs() < 0.15 {
        format!("{:.0}×", ratio)
    } else {
        format!("{:.1}×", ratio)
    }
}

/// How far past the expected band the value sits, in plain words. `ratio` is
/// `|z| / deviations` — `> 1` means breaching; bigger means further out.
fn outside_phrase(z_abs: f64, deviations: f64) -> &'static str {
    let ratio = if deviations > 0.0 {
        z_abs / deviations
    } else {
        z_abs
    };
    if ratio >= 4.0 {
        "far outside"
    } else if ratio >= 1.5 {
        "well outside"
    } else {
        "just outside"
    }
}

/// Magnitude relative to baseline, e.g. "about 4× the normal 18" (above) or
/// "about 28% of the normal 18" (below). Falls back to an absolute delta when
/// the baseline is too close to zero for a ratio to mean anything.
fn magnitude_phrase(value: f64, center: f64, above: bool) -> String {
    if center.abs() >= 1.0 {
        if above {
            format!(
                "about {} the normal {}",
                times_label(value / center),
                fmt_compact(center)
            )
        } else {
            let pct = (value / center * 100.0).round();
            format!("about {:.0}% of the normal {}", pct, fmt_compact(center))
        }
    } else {
        let delta = (value - center).abs();
        let dir = if above { "above" } else { "below" };
        format!(
            "{} {} the usual ~{}",
            fmt_compact(delta),
            dir,
            fmt_compact(center)
        )
    }
}

/// Plain-language summary of an anomaly breach.
///
/// e.g. "guestbook.activity.level is unusually high — it averaged 76 over the
/// last minute, about 4× the normal 18. That's far outside the expected range
/// (13–23)."
#[allow(clippy::too_many_arguments)]
fn humanize_anomaly(
    metric: &str,
    aggregation: &str,
    value: f64,
    center: f64,
    scale: f64,
    z: f64,
    deviations: f64,
    window_secs: i64,
) -> String {
    let above = value >= center;
    let direction = if above { "high" } else { "low" };
    let band_lo = center - deviations * scale;
    let band_hi = center + deviations * scale;
    format!(
        "{} is unusually {} — it {} {} {}, {}. That's {} the expected range ({}–{}).",
        metric,
        direction,
        agg_verb(aggregation),
        fmt_compact(value),
        window_phrase(window_secs),
        magnitude_phrase(value, center, above),
        outside_phrase(z.abs(), deviations),
        fmt_compact(band_lo),
        fmt_compact(band_hi),
    )
}

/// Plain-language summary of a static-threshold breach.
///
/// e.g. "guestbook.list.requests averaged 5 over the last minute, above the 100
/// threshold."
fn humanize_static(
    metric: &str,
    aggregation: &str,
    value: f64,
    comparator: Comparator,
    threshold: f64,
    window_secs: i64,
) -> String {
    let above = matches!(comparator, Comparator::Gt | Comparator::Gte);
    let side = if above { "above" } else { "below" };
    format!(
        "{} {} {} {}, {} the {} threshold.",
        metric,
        agg_verb(aggregation),
        fmt_compact(value),
        window_phrase(window_secs),
        side,
        fmt_compact(threshold),
    )
}

/// System prompt for the AI alert summary — constrain to one grounded sentence
/// and forbid the two failure modes that would make a summary worse than the
/// template: invented causes and invented numbers.
const ALERT_SUMMARY_SYSTEM: &str = "You are an observability assistant writing a one-line alert summary for an \
on-call engineer. Rewrite the given metric-alert facts as a single clear, plain-English sentence of at most \
30 words. Convey what the metric is doing, the magnitude, and that it is outside its expected range. Do NOT \
speculate about causes, do NOT give recommendations, and do NOT invent numbers beyond those given. Reply with \
only the sentence.";

/// Build the best-effort AI summary request (ADR-022) from the rule, the fire
/// metadata (which already carries the computed figures), and the deterministic
/// message. Feeds only structured facts — no raw series — and grounds the model
/// with the deterministic sentence. Returns `None` when the essentials (a
/// `value`) are absent.
fn alert_summary_request(
    rule: &AlertRule,
    metadata: &serde_json::Value,
    deterministic_summary: &str,
) -> Option<temps_ai::AiRequest> {
    let num = |k: &str| metadata.get(k).and_then(serde_json::Value::as_f64);
    let value = num("value")?;
    let mut facts = String::new();
    facts.push_str(&format!("Metric: {}\n", rule.metric_name));
    facts.push_str(&format!("Aggregation: {}\n", rule.aggregation));
    facts.push_str(&format!(
        "Detection: {}\n",
        metadata
            .get("detection_kind")
            .and_then(|v| v.as_str())
            .unwrap_or("static")
    ));
    facts.push_str(&format!("Current value: {value:.3}\n"));
    if let Some(c) = num("baseline_center") {
        facts.push_str(&format!("Baseline (normal) value: {c:.3}\n"));
        if let (Some(s), Some(d)) = (num("baseline_scale"), num("deviations")) {
            facts.push_str(&format!(
                "Expected range: {:.3} to {:.3}\n",
                c - d * s,
                c + d * s
            ));
        }
    }
    if let (Some(t), Some(cmp)) = (
        num("threshold"),
        metadata.get("comparator").and_then(|v| v.as_str()),
    ) {
        facts.push_str(&format!("Threshold: {cmp} {t:.3}\n"));
    }
    facts.push_str(&format!("Window seconds: {}\n", rule.window_secs));
    facts.push_str(&format!("Severity: {}\n", rule.severity));
    facts.push_str(&format!(
        "Deterministic summary for reference: {deterministic_summary}"
    ));

    Some(temps_ai::AiRequest {
        purpose: "alert.summary".to_string(),
        project_id: Some(rule.project_id),
        system: Some(ALERT_SUMMARY_SYSTEM.to_string()),
        prompt: facts,
        max_tokens: Some(160),
        temperature: Some(0.2),
        ..Default::default()
    })
}

/// Inputs for the alert email chart.
struct AlertChart<'a> {
    values: &'a [f64],
    /// Bucket timestamps (UTC) parallel to `values`; drives the x-axis time ticks.
    times: &'a [DateTime<Utc>],
    /// Anomaly expected band (lower, upper) — shaded amber, labelled.
    band: Option<(f64, f64)>,
    /// Static threshold + whether a breach is ABOVE it: dashed line, breach side
    /// lightly shaded red, with `threshold_label`.
    threshold: Option<(f64, bool)>,
    threshold_label: String,
    /// The firing value — drawn as a red marker line with `value_label`.
    value: f64,
    value_label: String,
}

/// Render a Datadog-style inline-SVG chart for an alert email: the recent series
/// (blue), the breach threshold / expected band shaded and labelled, the firing
/// value marked with a red line + label, the evaluation moment marked, and y-axis
/// labels. Light theme, self-contained (inline attributes only) so it renders in
/// mail clients that allow inline SVG (Mailpit, Apple Mail); clients that strip
/// it just drop the image and keep the email's text summary + detail table.
fn render_alert_chart_svg(chart: &AlertChart) -> String {
    let values = chart.values;
    const W: f64 = 560.0;
    const H: f64 = 180.0;
    const ML: f64 = 46.0; // left margin (y labels)
    const MR: f64 = 12.0;
    const MT: f64 = 14.0;
    const MB: f64 = 26.0; // bottom margin (room for x-axis time ticks)
    let xr = W - MR; // right edge of plot
    let yb = H - MB; // bottom edge of plot

    let mut lo = values.iter().copied().fold(f64::INFINITY, f64::min);
    let mut hi = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if let Some((bl, bu)) = chart.band {
        lo = lo.min(bl);
        hi = hi.max(bu);
    }
    if let Some((t, _)) = chart.threshold {
        lo = lo.min(t);
        hi = hi.max(t);
    }
    lo = lo.min(chart.value);
    hi = hi.max(chart.value);
    if !lo.is_finite() || !hi.is_finite() {
        return String::new();
    }
    if (hi - lo).abs() < f64::EPSILON {
        hi = lo + 1.0;
        lo -= 1.0;
    }
    let span = hi - lo;
    lo -= span * 0.08;
    hi += span * 0.08;

    let pw = W - ML - MR;
    let ph = H - MT - MB;
    let n = values.len();
    let x_at = |i: usize| -> f64 {
        if n <= 1 {
            ML
        } else {
            ML + pw * (i as f64) / ((n - 1) as f64)
        }
    };
    let y_at = |v: f64| -> f64 { MT + ph * (1.0 - (v - lo) / (hi - lo)) };

    let mut s = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="100%" style="max-width:560px;display:block;background:#ffffff;border:1px solid #e5e7eb;border-radius:8px;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Arial,sans-serif;">"##
    );

    // Gridlines + y labels at lo / mid / hi.
    let tx = ML - 6.0;
    for frac in [0.0_f64, 0.5, 1.0] {
        let v = lo + span * frac + span * 0.08 * (1.0 - 2.0 * frac);
        let y = y_at(v);
        let yt = y + 3.0;
        let label = fmt_compact(v);
        s.push_str(&format!(
            r##"<line x1="{ML}" y1="{y:.1}" x2="{xr}" y2="{y:.1}" stroke="#f1f5f9" stroke-width="1"/><text x="{tx:.1}" y="{yt:.1}" text-anchor="end" font-size="9" fill="#9ca3af">{label}</text>"##
        ));
    }

    // Anomaly expected band (amber) + label.
    if let Some((bl, bu)) = chart.band {
        let yt = y_at(bu);
        let bh = (y_at(bl) - yt).max(0.0);
        let lx = ML + 4.0;
        let ly = yt + 11.0;
        let lbl = format!("expected {}–{}", fmt_compact(bl), fmt_compact(bu));
        s.push_str(&format!(
            r##"<rect x="{ML}" y="{yt:.1}" width="{pw:.1}" height="{bh:.1}" fill="#f59e0b" fill-opacity="0.13"/><text x="{lx:.1}" y="{ly:.1}" font-size="9" fill="#b45309">{lbl}</text>"##
        ));
    }

    // Static breach zone (red) + threshold dashed line + label.
    if let Some((t, breach_above)) = chart.threshold {
        let yt = y_at(t);
        let zy = if breach_above { MT } else { yt };
        let zh = if breach_above {
            (yt - MT).max(0.0)
        } else {
            (yb - yt).max(0.0)
        };
        let lx = ML + 4.0;
        let ly = yt - 4.0;
        let lbl = svg_escape(&chart.threshold_label);
        s.push_str(&format!(
            r##"<rect x="{ML}" y="{zy:.1}" width="{pw:.1}" height="{zh:.1}" fill="#ef4444" fill-opacity="0.07"/><line x1="{ML}" y1="{yt:.1}" x2="{xr}" y2="{yt:.1}" stroke="#ef4444" stroke-width="1" stroke-dasharray="4 3"/><text x="{lx:.1}" y="{ly:.1}" font-size="9" fill="#b91c1c">{lbl}</text>"##
        ));
    }

    // Firing-value marker line (red) + label.
    let vy = y_at(chart.value);
    let vly = vy - 4.0;
    let vlbl = format!(
        "{}: {}",
        svg_escape(&chart.value_label),
        fmt_compact(chart.value)
    );
    s.push_str(&format!(
        r##"<line x1="{ML}" y1="{vy:.1}" x2="{xr}" y2="{vy:.1}" stroke="#dc2626" stroke-width="1" stroke-opacity="0.55"/><text x="{xr}" y="{vly:.1}" text-anchor="end" font-size="9" font-weight="600" fill="#dc2626">{vlbl}</text>"##
    ));

    // Evaluation marker (vertical, at the latest point).
    let ex = x_at(n - 1);
    s.push_str(&format!(
        r##"<line x1="{ex:.1}" y1="{MT}" x2="{ex:.1}" y2="{yb}" stroke="#cbd5e1" stroke-width="1"/>"##
    ));

    // Metric line + latest point.
    let pts: String = values
        .iter()
        .enumerate()
        .map(|(i, &v)| format!("{:.1},{:.1}", x_at(i), y_at(v)))
        .collect::<Vec<_>>()
        .join(" ");
    s.push_str(&format!(
        r##"<polyline points="{pts}" fill="none" stroke="#3b82f6" stroke-width="2" stroke-linejoin="round" stroke-linecap="round"/>"##
    ));
    if let Some(&last) = values.last() {
        let cy = y_at(last);
        s.push_str(&format!(
            r##"<circle cx="{ex:.1}" cy="{cy:.1}" r="3" fill="#dc2626"/>"##
        ));
    }

    // X-axis time ticks: start (with date), middle, and end (the eval moment,
    // marked UTC so the window is unambiguous). The series is index-linear, so
    // each tick is placed at — and labelled with — its actual bucket timestamp.
    let times = chart.times;
    if times.len() == n && n >= 2 {
        let ty = yb + 12.0;
        let mid = n / 2;
        let last = n - 1;
        let lt = svg_escape(&fmt_axis_time(times[0], true));
        s.push_str(&format!(
            r##"<text x="{ML}" y="{ty:.1}" text-anchor="start" font-size="9" fill="#9ca3af">{lt}</text>"##
        ));
        if mid != 0 && mid != last {
            let mx = x_at(mid);
            let mt = svg_escape(&fmt_axis_time(times[mid], false));
            s.push_str(&format!(
                r##"<text x="{mx:.1}" y="{ty:.1}" text-anchor="middle" font-size="9" fill="#9ca3af">{mt}</text>"##
            ));
        }
        let rt = svg_escape(&format!("{} UTC", fmt_axis_time(times[last], false)));
        s.push_str(&format!(
            r##"<text x="{xr}" y="{ty:.1}" text-anchor="end" font-size="9" fill="#9ca3af">{rt}</text>"##
        ));
    }

    s.push_str("</svg>");
    s
}

/// Map a rule severity string to an `AlarmSeverity`.
fn map_severity(severity: &str) -> AlarmSeverity {
    match severity {
        "critical" => AlarmSeverity::Critical,
        "info" => AlarmSeverity::Info,
        _ => AlarmSeverity::Warning,
    }
}

/// A per-rule anomaly baseline cached across evaluator ticks. `values` are
/// `(bucket_timestamp, aggregated_value)` over the rule's lookback window,
/// fetched through the same aggregation path as the scored point.
struct CachedBaseline {
    fetched_at: Instant,
    values: Vec<(DateTime<Utc>, f64)>,
}

/// The result of evaluating an anomaly rule against its baseline band.
struct AnomalyEval {
    breaching: bool,
    center: f64,
    scale: f64,
}

/// What to put on a fired alarm — built by the detector branch so `fire` stays
/// detector-agnostic.
struct FireDetails {
    title: String,
    message: String,
    metadata: serde_json::Value,
}

impl FireDetails {
    fn static_breach(rule: &AlertRule, value: f64, p: &StaticParams) -> Self {
        FireDetails {
            title: format!("Metric threshold breached: {}", rule.name),
            message: humanize_static(
                &rule.metric_name,
                &rule.aggregation,
                value,
                p.comparator,
                p.threshold,
                i64::from(rule.window_secs),
            ),
            metadata: json!({
                "rule_id": rule.id,
                "metric_name": rule.metric_name,
                "aggregation": rule.aggregation,
                "value": value,
                "threshold": p.threshold,
                "comparator": p.comparator.symbol(),
                "detection_kind": "static",
                "window_secs": rule.window_secs,
                "for_duration_secs": rule.for_duration_secs,
                "source": "otel_metric_alert",
            }),
        }
    }

    fn anomaly_breach(rule: &AlertRule, value: f64, p: &AnomalyParams, ev: &AnomalyEval) -> Self {
        let z = (value - ev.center) / ev.scale.max(MIN_BAND_SCALE);
        FireDetails {
            title: format!("Metric anomaly: {}", rule.name),
            message: humanize_anomaly(
                &rule.metric_name,
                &rule.aggregation,
                value,
                ev.center,
                ev.scale,
                z,
                p.deviations,
                i64::from(rule.window_secs),
            ),
            metadata: json!({
                "rule_id": rule.id,
                "metric_name": rule.metric_name,
                "aggregation": rule.aggregation,
                "value": value,
                "baseline_center": ev.center,
                "baseline_scale": ev.scale,
                "z_score": z,
                "deviations": p.deviations,
                "algorithm": format!("{:?}", p.algorithm).to_lowercase(),
                "seasonality": format!("{:?}", p.seasonality).to_lowercase(),
                "detection_kind": "anomaly",
                "window_secs": rule.window_secs,
                "for_duration_secs": rule.for_duration_secs,
                "source": "otel_metric_anomaly",
            }),
        }
    }
}

/// Background task that evaluates enabled metric alert rules and fires/resolves
/// notifications via the reused alarm system.
pub struct MetricAlertEvaluator {
    alert_service: Arc<MetricAlertService>,
    otel_service: Arc<OtelService>,
    alarm_service: Arc<AlarmService>,
    /// rule_id -> when the current breach started (for `for_duration` tracking).
    breach_start: Arc<RwLock<HashMap<i32, Instant>>>,
    /// rule_id -> alarm_id, for resolving the matching alarm on recovery.
    firing: Arc<RwLock<HashMap<i32, i32>>>,
    /// rule_id -> cached anomaly baseline (refreshed every `BASELINE_REFRESH_SECS`).
    baseline_cache: Arc<RwLock<HashMap<i32, CachedBaseline>>>,
    /// DB handle, used only to read the per-project AI-summary opt-in toggle.
    db: Arc<sea_orm::DatabaseConnection>,
    /// ADR-022: optional general AI foundation. `None` (default) keeps the
    /// deterministic Tier-1 text; when present (and the project opts in) it is
    /// called inside a timeout on each fire and may replace the lead sentence,
    /// never block or fail the alert.
    ai: Option<Arc<dyn temps_ai::AiService>>,
}

impl MetricAlertEvaluator {
    pub fn new(
        alert_service: Arc<MetricAlertService>,
        otel_service: Arc<OtelService>,
        alarm_service: Arc<AlarmService>,
        db: Arc<sea_orm::DatabaseConnection>,
        ai: Option<Arc<dyn temps_ai::AiService>>,
    ) -> Self {
        Self {
            alert_service,
            otel_service,
            alarm_service,
            breach_start: Arc::new(RwLock::new(HashMap::new())),
            firing: Arc::new(RwLock::new(HashMap::new())),
            baseline_cache: Arc::new(RwLock::new(HashMap::new())),
            db,
            ai,
        }
    }

    /// Whether AI summarization is opted in for this project
    /// (`projects.ai_alert_summaries_enabled = true`). Feature-level gate; the
    /// foundation's `is_available` is the separate capability gate.
    async fn ai_summaries_enabled(&self, project_id: i32) -> bool {
        use sea_orm::EntityTrait;
        matches!(
            temps_entities::projects::Entity::find_by_id(project_id)
                .one(self.db.as_ref())
                .await,
            Ok(Some(p)) if p.ai_alert_summaries_enabled == Some(true)
        )
    }

    /// Run the evaluator loop forever. Skips the immediate first tick.
    pub async fn run(&self) {
        info!("Starting OTel metric alert evaluator");
        let mut interval = tokio::time::interval(Duration::from_secs(EVAL_INTERVAL_SECS));
        interval.tick().await; // discard the immediate first tick
        loop {
            interval.tick().await;
            if let Err(e) = self.run_cycle().await {
                error!(error = %e, "OTel metric alert evaluation cycle failed");
            }
        }
    }

    /// One evaluation cycle: load enabled rules, evaluate each independently.
    async fn run_cycle(&self) -> Result<(), crate::error::OtelError> {
        let rules = self.alert_service.list_enabled().await?;
        debug!(rule_count = rules.len(), "Evaluating metric alert rules");

        // Prune transient per-rule state for rules that are no longer enabled,
        // so deleted/disabled rules don't leak breach timers or baseline caches.
        let live: HashSet<i32> = rules.iter().map(|r| r.id).collect();
        self.breach_start
            .write()
            .await
            .retain(|id, _| live.contains(id));
        self.baseline_cache
            .write()
            .await
            .retain(|id, _| live.contains(id));

        for rule in rules {
            let rule_id = rule.id;
            // A single rule's failure must never abort the loop.
            if let Err(e) = self.evaluate_rule(rule).await {
                error!(rule_id, error = %e, "Metric alert rule evaluation failed (continuing)");
            }
        }
        Ok(())
    }

    /// Evaluate one rule: query its latest aggregated value, compare, run the
    /// state machine, fire/resolve as needed, and persist the observed state.
    async fn evaluate_rule(&self, rule: AlertRule) -> Result<(), crate::error::OtelError> {
        let now = Utc::now();
        let window = chrono::Duration::seconds(rule.window_secs.max(1) as i64);
        let aggregation = MetricAggregation::parse(&rule.aggregation);
        let query = MetricQuery {
            project_id: rule.project_id,
            metric_name: Some(rule.metric_name.clone()),
            start_time: Some(now - window),
            end_time: Some(now),
            bucket_interval: Some(format!("{}s", rule.window_secs.max(1))),
            // The window may straddle two epoch-aligned buckets; fetch both (the
            // query returns them ASC) and take the latest below. A limit of 1
            // would return the OLDEST bucket, evaluating stale data.
            limit: Some(2),
            aggregation,
            ..Default::default()
        };

        let buckets = self.otel_service.query_metrics(query).await?;

        // No data in the window: PRESERVE the current state and any open alarm.
        // Do NOT flip a firing rule to `unknown` (which would orphan its alarm),
        // do NOT clobber last_value, and do NOT reset the breach timer — just
        // record that we evaluated.
        let Some(latest) = buckets.last() else {
            if let Err(e) = self
                .alert_service
                .persist_evaluation(rule.id, &rule.last_state, rule.last_value, now)
                .await
            {
                warn!(rule_id = rule.id, error = %e, "Failed to persist no-data evaluation");
            }
            return Ok(());
        };

        // For histogram percentile rules, compute the quantile from the bucket
        // layout — the server's scalar `value` is the percentile of synthetic
        // per-point means, not the true distribution (same reason the explorer
        // recomputes percentiles client-side).
        let value = value_for_rule(latest, aggregation);

        // Decode the typed detector. A corrupt blob fails this rule's cycle (the
        // outer loop logs and continues) rather than evaluating it incorrectly.
        let config = DetectionConfig::from_value(&rule.detection_config)?;
        let (breaching, fire_details) = match &config {
            DetectionConfig::Static(p) => (
                p.comparator.breaches(value, p.threshold),
                FireDetails::static_breach(&rule, value, p),
            ),
            DetectionConfig::Anomaly(p) => {
                match self
                    .anomaly_eval(&rule, latest.bucket, value, p, now)
                    .await?
                {
                    Some(ev) => {
                        let details = FireDetails::anomaly_breach(&rule, value, p, &ev);
                        (ev.breaching, details)
                    }
                    // Insufficient/degenerate baseline: preserve state, neither
                    // fire nor resolve (no spurious alerts on thin history).
                    None => {
                        if let Err(e) = self
                            .alert_service
                            .persist_evaluation(rule.id, &rule.last_state, rule.last_value, now)
                            .await
                        {
                            warn!(rule_id = rule.id, error = %e, "Failed to persist insufficient-baseline evaluation");
                        }
                        return Ok(());
                    }
                }
            }
            // Other detectors are typed/schema-present but not yet evaluable
            // (creation is rejected by the service). Defensive guard for any rule
            // that predates support: preserve state and skip without firing.
            other => {
                debug!(
                    rule_id = rule.id,
                    kind = other.kind_str(),
                    "Skipping rule: detector kind not yet evaluable"
                );
                if let Err(e) = self
                    .alert_service
                    .persist_evaluation(rule.id, &rule.last_state, rule.last_value, now)
                    .await
                {
                    warn!(rule_id = rule.id, error = %e, "Failed to persist skipped evaluation");
                }
                return Ok(());
            }
        };

        // Track breach duration (in-memory, approximated like temps-monitoring).
        let breach_elapsed_secs = if breaching {
            let mut starts = self.breach_start.write().await;
            let start = starts.entry(rule.id).or_insert_with(Instant::now);
            start.elapsed().as_secs()
        } else {
            0
        };

        let transition = evaluate_transition(
            &rule.last_state,
            breaching,
            breach_elapsed_secs,
            rule.for_duration_secs.max(0) as u64,
        );

        let next_state = match transition {
            AlertTransition::StayOk | AlertTransition::StartBreach => "ok",
            AlertTransition::StayFiring => "firing",
            AlertTransition::FireNow => {
                self.fire(&rule, fire_details).await;
                "firing"
            }
            AlertTransition::Resolve => {
                self.resolve(&rule).await;
                self.clear_breach(rule.id).await;
                "ok"
            }
        };

        if let Err(e) = self
            .alert_service
            .persist_evaluation(rule.id, next_state, Some(value), now)
            .await
        {
            warn!(rule_id = rule.id, error = %e, "Failed to persist rule evaluation");
        }
        Ok(())
    }

    /// Evaluate an anomaly rule's current `value` against a baseline band.
    ///
    /// Returns `Ok(None)` when the baseline is insufficient or degenerate
    /// (too few samples even after the seasonal→global fallback, or a flat band)
    /// — the caller then preserves state rather than firing. The band is the
    /// robust median+MAD of the lookback buckets in the scored point's seasonal
    /// cell, computed through the SAME aggregation as the scored point so counter
    /// rates and histogram percentiles compare like-for-like.
    async fn anomaly_eval(
        &self,
        rule: &AlertRule,
        scored_ts: DateTime<Utc>,
        value: f64,
        p: &AnomalyParams,
        now: DateTime<Utc>,
    ) -> Result<Option<AnomalyEval>, crate::error::OtelError> {
        // Baseline strictly before the scored bucket so the current (possibly
        // anomalous) window can't contaminate its own band.
        let baseline: Vec<(DateTime<Utc>, f64)> = self
            .baseline_values(rule, p, now)
            .await?
            .into_iter()
            .filter(|(ts, _)| *ts < scored_ts)
            .collect();

        // The SAME BandModel the preview uses, so production and preview agree.
        let band = BandModel::from_baseline(&baseline, p.seasonality, MIN_BASELINE_SAMPLES);
        if band.samples < MIN_BASELINE_SAMPLES {
            debug!(
                rule_id = rule.id,
                samples = band.samples,
                "Anomaly: insufficient baseline, preserving state"
            );
            return Ok(None);
        }
        match band.breaches(scored_ts, value, p.deviations, p.direction) {
            Some(breaching) => {
                let (center, scale) = band.band_for(scored_ts).unwrap_or((value, 0.0));
                Ok(Some(AnomalyEval {
                    breaching,
                    center,
                    scale,
                }))
            }
            None => {
                // Flat baseline → no meaningful band; preserve rather than fire on
                // every wobble (the divide-by-zero / always-firing case).
                debug!(
                    rule_id = rule.id,
                    "Anomaly: degenerate (flat) baseline, preserving state"
                );
                Ok(None)
            }
        }
    }

    /// Fetch (and cache) the per-rule baseline values over its lookback window.
    ///
    /// Cached for `BASELINE_REFRESH_SECS` so the hot tick scores against the
    /// cached buckets instead of re-querying weeks of data. Values are mapped
    /// through `value_for_rule` so each baseline bucket is the same quantity as
    /// the scored point (rate for counters, percentile for histograms).
    async fn baseline_values(
        &self,
        rule: &AlertRule,
        p: &AnomalyParams,
        now: DateTime<Utc>,
    ) -> Result<Vec<(DateTime<Utc>, f64)>, crate::error::OtelError> {
        {
            let cache = self.baseline_cache.read().await;
            if let Some(c) = cache.get(&rule.id) {
                if c.fetched_at.elapsed().as_secs() < BASELINE_REFRESH_SECS {
                    return Ok(c.values.clone());
                }
            }
        }

        let lookback_days = p
            .baseline_lookback_days
            .unwrap_or(DEFAULT_LOOKBACK_DAYS)
            .clamp(1, 90);
        let aggregation = MetricAggregation::parse(&rule.aggregation);
        let query = MetricQuery {
            project_id: rule.project_id,
            metric_name: Some(rule.metric_name.clone()),
            start_time: Some(now - chrono::Duration::days(lookback_days as i64)),
            end_time: Some(now),
            bucket_interval: Some(format!("{}s", rule.window_secs.max(1))),
            limit: None,
            aggregation,
            ..Default::default()
        };
        let buckets = self.otel_service.query_metrics(query).await?;
        let values: Vec<(DateTime<Utc>, f64)> = buckets
            .iter()
            .map(|b| (b.bucket, value_for_rule(b, aggregation)))
            .collect();

        self.baseline_cache.write().await.insert(
            rule.id,
            CachedBaseline {
                fetched_at: Instant::now(),
                values: values.clone(),
            },
        );
        Ok(values)
    }

    /// Fire an alarm via the reused alarm system and remember the alarm id.
    async fn fire(&self, rule: &AlertRule, details: FireDetails) {
        // Best-effort: render a compact chart of the recent series for the email
        // (Datadog-style). Carried as a reserved `_chart_svg` metadata key that
        // only the email renderer reads; never blocks or fails the fire.
        let mut metadata = details.metadata;
        if let Some(svg) = self.chart_svg_for(rule, &metadata).await {
            if let serde_json::Value::Object(map) = &mut metadata {
                map.insert("_chart_svg".to_string(), serde_json::Value::String(svg));
            }
        }

        // ADR-022: best-effort AI enrichment of the lead sentence via the general
        // AI foundation. Bounded by `AI_SUMMARY_TIMEOUT`; any timeout / error /
        // empty reply keeps the deterministic Tier-1 text. Gated on the project's
        // opt-in toggle AND the foundation reporting AI is configured.
        let mut message = details.message;
        if let Some(ai) = &self.ai {
            if self.ai_summaries_enabled(rule.project_id).await && ai.is_available().await {
                if let Some(req) = alert_summary_request(rule, &metadata, &message) {
                    let fut = temps_ai::complete_text(ai.as_ref(), req);
                    if let Ok(Some(text)) = tokio::time::timeout(AI_SUMMARY_TIMEOUT, fut).await {
                        let text = text.trim();
                        if !text.is_empty() {
                            if let serde_json::Value::Object(map) = &mut metadata {
                                map.insert("ai_summary".to_string(), serde_json::Value::Bool(true));
                            }
                            message = text.to_string();
                        }
                    }
                }
            }
        }

        let request = FireAlarmRequest {
            project_id: rule.project_id,
            environment_id: None,
            deployment_id: None,
            container_id: None,
            service_id: None,
            alarm_type: AlarmType::DeploymentMetricThreshold,
            severity: map_severity(&rule.severity),
            title: details.title,
            message,
            metadata: Some(metadata),
        };
        match self.alarm_service.fire_alarm(request).await {
            Ok(Some(alarm_id)) => {
                info!(rule_id = rule.id, alarm_id, "OTel metric alert fired");
                self.firing.write().await.insert(rule.id, alarm_id);
            }
            Ok(None) => {
                // Suppressed by cooldown — still mark firing so we resolve later.
                debug!(
                    rule_id = rule.id,
                    "OTel metric alert fire suppressed by cooldown"
                );
            }
            Err(e) => {
                error!(rule_id = rule.id, error = %e, "Failed to fire OTel metric alert");
            }
        }
    }

    /// Render a compact chart SVG of the metric's recent series for the alert
    /// email, reading the expected band / threshold from the fire metadata.
    /// Returns None on any hiccup (too little data, query error) — the email
    /// simply omits the chart.
    async fn chart_svg_for(
        &self,
        rule: &AlertRule,
        metadata: &serde_json::Value,
    ) -> Option<String> {
        // Show ~60 windows of context around the breach.
        let window = rule.window_secs.max(60);
        let now = Utc::now();
        let query = MetricQuery {
            project_id: rule.project_id,
            metric_name: Some(rule.metric_name.clone()),
            start_time: Some(now - chrono::Duration::seconds(window as i64 * 60)),
            end_time: Some(now),
            bucket_interval: Some(format!("{}s", window)),
            limit: Some(120),
            aggregation: MetricAggregation::parse(&rule.aggregation),
            ..Default::default()
        };
        let buckets = self.otel_service.query_metrics(query).await.ok()?;
        if buckets.len() < 2 {
            return None;
        }
        let agg = MetricAggregation::parse(&rule.aggregation);
        let values: Vec<f64> = buckets.iter().map(|b| value_for_rule(b, agg)).collect();
        let times: Vec<DateTime<Utc>> = buckets.iter().map(|b| b.bucket).collect();

        // Expected band: anomaly rules carry center/scale/deviations; static
        // rules carry a single threshold line.
        let band = match (
            metadata.get("baseline_center").and_then(|v| v.as_f64()),
            metadata.get("baseline_scale").and_then(|v| v.as_f64()),
            metadata.get("deviations").and_then(|v| v.as_f64()),
        ) {
            (Some(center), Some(scale), Some(dev)) => {
                Some((center - dev * scale, center + dev * scale))
            }
            _ => None,
        };
        let threshold = metadata.get("threshold").and_then(|v| v.as_f64()).map(|t| {
            // ">"/"≥" → breach above the line; "<"/"≤" → breach below.
            let breach_above = metadata
                .get("comparator")
                .and_then(|v| v.as_str())
                .map(|c| c.contains('>'))
                .unwrap_or(true);
            (t, breach_above)
        });
        let threshold_label = match (
            threshold,
            metadata.get("comparator").and_then(|v| v.as_str()),
        ) {
            (Some((t, _)), Some(cmp)) => format!("{} {}", cmp, fmt_compact(t)),
            _ => String::new(),
        };
        let value = metadata
            .get("value")
            .and_then(|v| v.as_f64())
            .or_else(|| values.last().copied())
            .unwrap_or(0.0);
        let w = rule.window_secs.max(1);
        let window_label = if w % 3600 == 0 {
            format!("{}h", w / 3600)
        } else if w % 60 == 0 {
            format!("{}m", w / 60)
        } else {
            format!("{}s", w)
        };
        let value_label = format!("last {} {}", window_label, rule.aggregation);

        Some(render_alert_chart_svg(&AlertChart {
            values: &values,
            times: &times,
            band,
            threshold,
            threshold_label,
            value,
            value_label,
        }))
    }

    /// Resolve the alarm previously fired for this rule, if any.
    async fn resolve(&self, rule: &AlertRule) {
        let alarm_id = self.firing.write().await.remove(&rule.id);
        if let Some(alarm_id) = alarm_id {
            if let Err(e) = self
                .alarm_service
                .resolve_alarm(alarm_id, rule.project_id)
                .await
            {
                error!(
                    rule_id = rule.id,
                    alarm_id,
                    error = %e,
                    "Failed to resolve OTel metric alert"
                );
            } else {
                info!(rule_id = rule.id, alarm_id, "OTel metric alert resolved");
            }
        }
    }

    /// Drop any in-flight breach timer for a rule.
    async fn clear_breach(&self, rule_id: i32) {
        self.breach_start.write().await.remove(&rule_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transition_ok_to_firing_requires_for_duration() {
        // Breaching but not long enough yet -> StartBreach (stays effectively ok).
        let t = evaluate_transition("ok", true, 30, 120);
        assert_eq!(t, AlertTransition::StartBreach);

        // Breach has persisted past for_duration -> FireNow.
        let t = evaluate_transition("ok", true, 120, 120);
        assert_eq!(t, AlertTransition::FireNow);

        let t = evaluate_transition("ok", true, 200, 120);
        assert_eq!(t, AlertTransition::FireNow);
    }

    #[test]
    fn test_transition_firing_to_ok() {
        // Was firing, no longer breaching -> Resolve.
        let t = evaluate_transition("firing", false, 0, 120);
        assert_eq!(t, AlertTransition::Resolve);
    }

    #[test]
    fn test_transition_stay_states() {
        // Not breaching, was ok -> StayOk.
        assert_eq!(
            evaluate_transition("ok", false, 0, 120),
            AlertTransition::StayOk
        );
        // Breaching, already firing -> StayFiring (no re-fire).
        assert_eq!(
            evaluate_transition("firing", true, 999, 120),
            AlertTransition::StayFiring
        );
        // Unknown previous state behaves like ok.
        assert_eq!(
            evaluate_transition("unknown", false, 0, 120),
            AlertTransition::StayOk
        );
        assert_eq!(
            evaluate_transition("unknown", true, 5, 120),
            AlertTransition::StartBreach
        );
    }

    #[test]
    fn test_map_severity() {
        assert!(matches!(map_severity("critical"), AlarmSeverity::Critical));
        assert!(matches!(map_severity("info"), AlarmSeverity::Info));
        assert!(matches!(map_severity("warning"), AlarmSeverity::Warning));
        assert!(matches!(
            map_severity("anything-else"),
            AlarmSeverity::Warning
        ));
    }

    #[test]
    fn test_chart_svg_renders_x_axis_time_ticks() {
        use chrono::TimeZone;
        let t0 = Utc.with_ymd_and_hms(2026, 6, 27, 9, 30, 0).unwrap();
        let times: Vec<DateTime<Utc>> = (0..5).map(|i| t0 + chrono::Duration::minutes(i)).collect();
        let values = vec![18.0, 19.0, 90.0, 92.0, 95.0];
        let svg = render_alert_chart_svg(&AlertChart {
            values: &values,
            times: &times,
            band: Some((16.0, 20.0)),
            threshold: None,
            threshold_label: String::new(),
            value: 95.0,
            value_label: "last 1m avg".to_string(),
        });
        // Start tick carries the date; middle tick is bare HH:MM; end tick (the
        // eval moment) carries the UTC marker so the window can't be misread.
        assert!(svg.contains("Jun 27 09:30"), "missing dated start tick");
        assert!(svg.contains("09:32"), "missing middle tick");
        assert!(svg.contains("09:34 UTC"), "missing UTC-marked end tick");
    }

    #[test]
    fn test_window_phrase() {
        assert_eq!(window_phrase(60), "over the last minute");
        assert_eq!(window_phrase(300), "over the last 5 minutes");
        assert_eq!(window_phrase(3600), "over the last hour");
        assert_eq!(window_phrase(7200), "over the last 2 hours");
        assert_eq!(window_phrase(45), "over the last 45 seconds");
    }

    #[test]
    fn test_humanize_anomaly_high() {
        // The screenshot case: value 76.2, baseline 18.5 ± 1.67, band ±3σ.
        let z = (76.223 - 18.467) / 1.672_f64.max(MIN_BAND_SCALE);
        let msg = humanize_anomaly(
            "guestbook.activity.level",
            "avg",
            76.223,
            18.467,
            1.672,
            z,
            3.0,
            60,
        );
        assert!(
            msg.contains("guestbook.activity.level is unusually high"),
            "{msg}"
        );
        assert!(msg.contains("averaged 76"), "{msg}");
        assert!(msg.contains("over the last minute"), "{msg}");
        assert!(msg.contains("about 4× the normal 18"), "{msg}");
        assert!(msg.contains("far outside the expected range"), "{msg}");
        // No statistician's notation in the human sentence.
        assert!(!msg.contains('σ'), "should not leak sigma: {msg}");
    }

    #[test]
    fn test_humanize_anomaly_low() {
        let z = (5.0 - 18.467) / 1.672_f64.max(MIN_BAND_SCALE);
        let msg = humanize_anomaly("svc.queue.depth", "avg", 5.0, 18.467, 1.672, z, 3.0, 300);
        assert!(msg.contains("is unusually low"), "{msg}");
        assert!(msg.contains("% of the normal 18"), "{msg}");
        assert!(msg.contains("over the last 5 minutes"), "{msg}");
    }

    #[test]
    fn test_humanize_static_above_and_below() {
        let above = humanize_static(
            "guestbook.list.requests",
            "avg",
            150.0,
            Comparator::Gte,
            100.0,
            60,
        );
        assert!(above.contains("averaged 150"), "{above}");
        assert!(above.contains("above the 100 threshold"), "{above}");

        let below = humanize_static("cache.hit.ratio", "min", 0.4, Comparator::Lt, 0.9, 60);
        assert!(below.contains("bottomed out at"), "{below}");
        assert!(below.contains("below the"), "{below}");
    }

    #[test]
    fn test_histogram_quantile() {
        // bounds [10,50,100], counts [2,3,4,1] (total 10, cum 2/5/9/10).
        let bounds = [10.0, 50.0, 100.0];
        let counts = [2u64, 3, 4, 1];
        assert!((histogram_quantile(&bounds, &counts, 0.5) - 50.0).abs() < 1e-9);
        assert!((histogram_quantile(&bounds, &counts, 0.9) - 100.0).abs() < 1e-9);
        // mid-bucket interpolation: bounds [10,20], counts [1,2,1] -> p50 = 15.
        assert!((histogram_quantile(&[10.0, 20.0], &[1, 2, 1], 0.5) - 15.0).abs() < 1e-9);
        // empty histogram -> 0.
        assert_eq!(histogram_quantile(&bounds, &[0, 0, 0, 0], 0.95), 0.0);
    }
}
