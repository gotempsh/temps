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
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};
use utoipa::ToSchema;

use temps_monitoring::{AlarmService, AlarmSeverity, AlarmStatus, AlarmType, FireAlarmRequest};

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

/// Row cap for the grouped (per-series) metrics query. The stores order buckets
/// ascending and truncate at the limit, so this must comfortably exceed
/// `series_count * buckets_in_window` for the latest tick to be fully present.
/// With `bucket_interval == window_secs` the window spans ~2 buckets, so 2000 rows
/// covers up to ~1000 concurrent series — far beyond the 100-series hard cap.
/// Cardinality past this is an accepted Phase-3 limitation (see ADR §risks).
const SERIES_QUERY_LIMIT: u64 = 2000;

/// A single series' scalar evaluation for a dynamic rule this tick.
struct SeriesPoint {
    /// Raw label pairs from the bucket's `series_key`, in store column order.
    key: Vec<(String, String)>,
    /// Deterministic, order-independent string key: `(rule_id, key_str)` is the
    /// per-series state-machine key.
    key_str: String,
    /// Human-readable `k=v, k2=v2` label (sorted) for alarm titles.
    label: String,
    /// The value the rule evaluates for this series (`value_for_rule`).
    value: f64,
}

/// Deterministic, order-independent serialization of a series' label pairs, so the
/// per-series state key is stable regardless of the order the store returned
/// columns. A `BTreeMap` sorts by key before serializing.
fn series_key_string(key: &[(String, String)]) -> String {
    let map: std::collections::BTreeMap<&str, &str> =
        key.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    serde_json::to_string(&map).unwrap_or_default()
}

/// A human-readable `key=value, key2=value2` label (sorted by key for stability).
fn series_label(key: &[(String, String)]) -> String {
    let mut pairs: Vec<&(String, String)> = key.iter().collect();
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The `baseline_cache` key for a rule's anomaly baseline query. The
/// aggregate/rule-level baseline uses an empty (`""`) discriminator — preserving
/// the exact cache entry/behaviour of a pre-per-series aggregate anomaly rule —
/// while a per-series baseline is discriminated by that series' own label pairs
/// (order-independent via [`series_key_string`]), so each series caches and
/// refreshes a band scoped strictly to its own data and can never be contaminated
/// by another series' cache entry. Pure/testable — no I/O.
fn baseline_cache_key(rule_id: i32, extra_filters: &[(String, String)]) -> (i32, String) {
    let discriminator = if extra_filters.is_empty() {
        String::new()
    } else {
        series_key_string(extra_filters)
    };
    (rule_id, discriminator)
}

/// Pick the bucket with the largest `|value_for_rule|` — the collapse-to-max
/// value used when `group_by` is set but `dynamic_alerts = false` ("alert if ANY
/// series breaches"). Pure/testable. Returns `None` for an empty slice.
fn loudest_bucket<'a>(
    buckets: &[&'a MetricBucket],
    aggregation: MetricAggregation,
) -> Option<&'a MetricBucket> {
    buckets.iter().copied().max_by(|a, b| {
        value_for_rule(a, aggregation)
            .abs()
            .partial_cmp(&value_for_rule(b, aggregation).abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

/// Cardinality guard: rank series by `|value|` descending (deterministic tie-break
/// by `key_str`) and keep only the top `max_series`. Returns `(kept, dropped_count)`.
/// Pure/testable — no I/O, mirroring `evaluate_transition`.
fn rank_and_cap_series(
    mut points: Vec<SeriesPoint>,
    max_series: usize,
) -> (Vec<SeriesPoint>, usize) {
    points.sort_by(|a, b| {
        b.value
            .abs()
            .partial_cmp(&a.value.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.key_str.cmp(&b.key_str))
    });
    let cap = max_series.max(1);
    if points.len() > cap {
        let dropped = points.len() - cap;
        points.truncate(cap);
        (points, dropped)
    } else {
        (points, 0)
    }
}

/// Whether the `index`-th of `total_firing` series firing in the same tick
/// should get the expensive best-effort chart/AI enrichment. At or below
/// `grouped_threshold` (the rule's per-rule `grouped_notification_threshold`)
/// every series enriches; above it, only the first (arbitrary order) does, to
/// avoid N redundant expensive calls in a cardinality spike.
fn should_enrich(index: usize, total_firing: usize, grouped_threshold: usize) -> bool {
    total_firing <= grouped_threshold || index == 0
}

/// Max series named in a grouped digest notification's message before collapsing
/// the remainder into "and N more". The full set is always available in the
/// per-series alarm rows and the `firing_series` API regardless of this cap — this
/// only bounds the human-readable digest text so it stays scannable in a spike.
const DIGEST_MESSAGE_MAX_LISTED: usize = 10;

/// Build the `(title, message)` for one grouped digest notification from the
/// series that fired this tick (`(series_label, value)` in fire order). Sent when
/// more than the rule's `grouped_notification_threshold` series breach at once, so
/// the channels get ONE combined message instead of N. Series labels can
/// themselves contain `", "` (multi-key group-bys), so entries are newline-
/// separated to stay unambiguous; up to [`DIGEST_MESSAGE_MAX_LISTED`] are named,
/// with an "and N more" tail beyond that. Pure/testable — no I/O.
fn build_digest_notification(metric_name: &str, fired: &[(String, f64)]) -> (String, String) {
    let title = format!("{} series of {} breached", fired.len(), metric_name);
    let mut lines: Vec<String> = fired
        .iter()
        .take(DIGEST_MESSAGE_MAX_LISTED)
        .map(|(label, value)| format!("{label} ({})", fmt_compact(*value)))
        .collect();
    if fired.len() > DIGEST_MESSAGE_MAX_LISTED {
        lines.push(format!(
            "and {} more",
            fired.len() - DIGEST_MESSAGE_MAX_LISTED
        ));
    }
    (title, lines.join("\n"))
}

/// One series' persisted state snapshot for a dynamic rule (ADR-026 follow-up):
/// the state after the latest tick, the value evaluated this tick, and the open
/// alarm id (when firing). Serialized into the `series_states` jsonb column keyed
/// by the human-readable [`series_label`]; the alert response decodes it back.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SeriesStateEntry {
    /// `firing` or `ok` for this series after the latest tick.
    pub state: String,
    /// The value the rule evaluated for this series this tick.
    pub value: f64,
    /// The open alarm's id when the series is firing; `null` when ok.
    pub alarm_id: Option<i32>,
}

/// Build the persisted per-series snapshot for a dynamic rule this tick, keyed by
/// each kept series' human-readable [`series_label`]. A kept series is `firing`
/// (carrying its open `alarm_id`) when present in `firing` (key_str -> alarm_id,
/// the FINAL firing state for the rule after this tick's fires/resolves), else
/// `ok`. Only kept series appear, so series dropped by the cap or absent from the
/// query don't linger. Pure/testable — no I/O.
fn build_series_states(
    kept: &[SeriesPoint],
    firing: &HashMap<String, i32>,
) -> HashMap<String, SeriesStateEntry> {
    kept.iter()
        .map(|p| {
            let entry = match firing.get(&p.key_str) {
                Some(alarm_id) => SeriesStateEntry {
                    state: "firing".to_string(),
                    value: p.value,
                    alarm_id: Some(*alarm_id),
                },
                None => SeriesStateEntry {
                    state: "ok".to_string(),
                    value: p.value,
                    alarm_id: None,
                },
            };
            (p.label.clone(), entry)
        })
        .collect()
}

/// Decode a rule's stored `label_filters`/`group_by` jsonb columns into the
/// scoping fields a `MetricQuery` needs. Malformed/legacy jsonb decodes to
/// empty, same as `#[serde(default)]` would on write.
fn rule_query_scope(rule: &AlertRule) -> (Vec<(String, String)>, Vec<String>) {
    let label_filters = serde_json::from_value(rule.label_filters.clone()).unwrap_or_default();
    let group_by = serde_json::from_value(rule.group_by.clone()).unwrap_or_default();
    (label_filters, group_by)
}

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

/// Escape text for inline SVG `<text>` element content only — `"`/`'` are
/// intentionally left unescaped since only `&`/`<`/`>` are special in element
/// content. Do not reuse this for an SVG attribute value (e.g. `<text x="...">`),
/// which also needs quotes escaped.
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

/// Which detector a dynamic (per-series) rule runs this tick, borrowed from the
/// decoded `DetectionConfig`. Both variants drive one independent state machine
/// per series; they differ only in how each series' breach is decided and what
/// alarm payload a fire carries (a static threshold vs. that series' own anomaly
/// band). Forecast/outlier/auto_watch are NOT representable here — they remain
/// genuinely unsupported per-series and fall back to the collapse path.
///
/// `Copy` (it holds only shared references), so it threads through the dynamic
/// evaluator by value without cloning the underlying params.
#[derive(Clone, Copy)]
enum DynamicDetector<'a> {
    Static(&'a StaticParams),
    Anomaly(&'a AnomalyParams),
}

/// One kept series' breach decision for a dynamic tick, produced by
/// [`MetricAlertEvaluator::series_decisions`] before the shared state machine
/// runs. `preserve` (anomaly-only) means the series' per-series baseline was
/// insufficient/degenerate this tick, so the state machine must leave it entirely
/// untouched — neither fire, resolve, nor reset its breach timer — mirroring the
/// aggregate anomaly `None` preserve-state path. `details` is `Some` exactly when
/// `breaching` is true (the payload is only ever needed on a fire), so a
/// non-breaching or preserved series carries none.
struct SeriesDecision {
    breaching: bool,
    preserve: bool,
    details: Option<FireDetails>,
}

/// An open per-series alarm: the series' raw label pairs plus its alarm id.
/// Stored by `(rule_id, series_key)` so per-series resolve + the `firing_series`
/// snapshot need no re-parsing of the string key.
type FiringSeries = (Vec<(String, String)>, i32);

/// The open per-series alarms in a `firing_series` snapshot that belong to
/// `rule_id`, as `((rule_id, series_key), alarm_id)`. This is the per-series work
/// [`MetricAlertEvaluator::resolve_all_for_rule`] performs before dropping a
/// deleted rule's state: exactly these alarms must be resolved so none is left
/// orphaned as `firing`. Pure/testable — isolates the "match the first element of
/// a `(rule_id, series_key)` composite key" logic from the surrounding
/// `AlarmService` I/O (other rules' entries are excluded and left untouched).
fn series_alarms_to_resolve(
    firing_series: &HashMap<(i32, String), FiringSeries>,
    rule_id: i32,
) -> Vec<((i32, String), i32)> {
    firing_series
        .iter()
        .filter(|((rid, _), _)| *rid == rule_id)
        .map(|((rid, ks), (_, alarm_id))| ((*rid, ks.clone()), *alarm_id))
        .collect()
}

/// Decode one alarm row's metadata into a `firing_series` entry, for the
/// startup reload (see `load_firing_series_from_db`). Returns `None` for any
/// alarm that isn't a dynamic per-series alarm, or whose metadata is missing
/// the fields this evaluator itself always writes on fire — malformed/foreign
/// metadata is skipped rather than treated as an error, since other alarm
/// sources share the same `alarms` table.
fn parse_dynamic_alarm_metadata(
    alarm_id: i32,
    metadata: Option<&serde_json::Value>,
) -> Option<((i32, String), FiringSeries)> {
    let metadata = metadata?;
    if metadata.get("is_dynamic").and_then(|v| v.as_bool()) != Some(true) {
        return None;
    }
    let rule_id = metadata.get("rule_id").and_then(|v| v.as_i64())? as i32;
    let series_key = metadata
        .get("series_key")
        .and_then(|v| serde_json::from_value::<Vec<(String, String)>>(v.clone()).ok())?;
    let key_str = series_key_string(&series_key);
    Some(((rule_id, key_str), (series_key, alarm_id)))
}

/// Background task that evaluates enabled metric alert rules and fires/resolves
/// notifications via the reused alarm system.
pub struct MetricAlertEvaluator {
    alert_service: Arc<MetricAlertService>,
    otel_service: Arc<OtelService>,
    alarm_service: Arc<AlarmService>,
    /// Cooldown-free `AlarmService` used ONLY for dynamic per-series firing.
    ///
    /// Per-series alarms all share `alarm_type=DeploymentMetricThreshold` with
    /// null deployment/container/service, so `AlarmService`'s DB cooldown key
    /// (which cannot see the series) would collapse every series in a project into
    /// one alarm per cooldown window. The evaluator's per-series state machine
    /// (`firing_series`) already guarantees exactly-once firing per series until it
    /// resolves, so the DB cooldown is redundant here and disabling it is safe.
    alarm_service_dynamic: Arc<AlarmService>,
    /// rule_id -> when the current breach started (for `for_duration` tracking).
    breach_start: Arc<RwLock<HashMap<i32, Instant>>>,
    /// rule_id -> alarm_id, for resolving the matching alarm on recovery.
    firing: Arc<RwLock<HashMap<i32, i32>>>,
    /// (rule_id, series_key) -> when the current per-series breach started. The
    /// dynamic analogue of `breach_start`, kept separate so static rules keep their
    /// existing untouched code path (ADR-026 Phase 3).
    breach_start_series: Arc<RwLock<HashMap<(i32, String), Instant>>>,
    /// (rule_id, series_key) -> the open per-series alarm ([`FiringSeries`]).
    firing_series: Arc<RwLock<HashMap<(i32, String), FiringSeries>>>,
    /// `(rule_id, series discriminator)` -> cached anomaly baseline (refreshed
    /// every `BASELINE_REFRESH_SECS`). The discriminator is `""` for the
    /// aggregate/rule-level baseline (the pre-per-series behaviour) and the
    /// series' label pairs (via [`baseline_cache_key`]) for a dynamic per-series
    /// baseline, so each series caches an independent band scoped to its own data.
    baseline_cache: Arc<RwLock<HashMap<(i32, String), CachedBaseline>>>,
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
        alarm_service_dynamic: Arc<AlarmService>,
        db: Arc<sea_orm::DatabaseConnection>,
        ai: Option<Arc<dyn temps_ai::AiService>>,
    ) -> Self {
        Self {
            alert_service,
            otel_service,
            alarm_service,
            alarm_service_dynamic,
            breach_start: Arc::new(RwLock::new(HashMap::new())),
            firing: Arc::new(RwLock::new(HashMap::new())),
            breach_start_series: Arc::new(RwLock::new(HashMap::new())),
            firing_series: Arc::new(RwLock::new(HashMap::new())),
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

    /// Repopulate `firing_series` from the DB on startup so a restart doesn't
    /// orphan open per-series alarms (ADR-026 Phase 3 §Open questions Q3).
    /// Mirrors `temps_monitoring::AlertEvaluator::load_firing_alarms_from_db`.
    ///
    /// Only dynamic-alert alarms (`metadata.is_dynamic == true`) are restored
    /// here — `breach_start_series` is intentionally NOT restored (same
    /// accepted trade-off as the pre-existing aggregate `breach_start`: a
    /// restart resets the `for_duration` timer, but never orphans an alarm),
    /// so a still-breaching series re-arms its timer rather than instantly
    /// re-firing, while a since-recovered series resolves on the next tick.
    async fn load_firing_series_from_db(&self) {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use temps_entities::alarms;

        let rows = match alarms::Entity::find()
            .filter(alarms::Column::AlarmType.eq(AlarmType::DeploymentMetricThreshold.as_str()))
            .filter(
                sea_orm::Condition::any()
                    .add(alarms::Column::Status.eq(AlarmStatus::Firing.as_str()))
                    .add(alarms::Column::Status.eq(AlarmStatus::Acknowledged.as_str())),
            )
            .all(self.db.as_ref())
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, "Failed to load firing per-series alarms from DB on startup");
                return;
            }
        };

        let mut restored = 0usize;
        let mut guard = self.firing_series.write().await;
        for alarm in rows {
            if let Some((key, value)) =
                parse_dynamic_alarm_metadata(alarm.id, alarm.metadata.as_ref())
            {
                guard.insert(key, value);
                restored += 1;
            }
        }
        if restored > 0 {
            info!(
                restored,
                "Restored open per-series alarms into firing_series on startup"
            );
        }
    }

    /// Run the evaluator loop forever. Skips the immediate first tick.
    pub async fn run(&self) {
        info!("Starting OTel metric alert evaluator");
        self.load_firing_series_from_db().await;
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
    ///
    /// `pub` so integration tests can drive a single deterministic cycle (the
    /// `run()` loop's timer isn't test-friendly); production only calls it from
    /// within `run()`.
    pub async fn run_cycle(&self) -> Result<(), crate::error::OtelError> {
        let rules = self.alert_service.list_enabled().await?;
        debug!(rule_count = rules.len(), "Evaluating metric alert rules");

        // Prune transient per-rule state for rules that are no longer enabled,
        // so deleted/disabled rules don't leak breach timers or baseline caches.
        let live: HashSet<i32> = rules.iter().map(|r| r.id).collect();
        self.breach_start
            .write()
            .await
            .retain(|id, _| live.contains(id));
        // Keyed by `(rule_id, series discriminator)`; retain by the rule id so all
        // of a live rule's per-series baselines survive and a dead rule's are dropped.
        self.baseline_cache
            .write()
            .await
            .retain(|(id, _), _| live.contains(id));
        // Same prune for the per-series maps, keyed by (rule_id, series_key).
        self.breach_start_series
            .write()
            .await
            .retain(|(id, _), _| live.contains(id));
        self.firing_series
            .write()
            .await
            .retain(|(id, _), _| live.contains(id));

        for rule in rules {
            let rule_id = rule.id;
            // A single rule's failure must never abort the loop.
            if let Err(e) = self.evaluate_rule(rule).await {
                error!(rule_id, error = %e, "Metric alert rule evaluation failed (continuing)");
            }
        }
        Ok(())
    }

    /// Evaluate one rule. Dispatches on `group_by`: an empty `group_by` (the
    /// default, every existing rule) runs the unchanged single-aggregate path; a
    /// set `group_by` runs the grouped path (collapse-to-max or, when
    /// `dynamic_alerts`, per-series). ADR-026 Phase 3.
    async fn evaluate_rule(&self, rule: AlertRule) -> Result<(), crate::error::OtelError> {
        let (_, group_by) = rule_query_scope(&rule);
        if group_by.is_empty() {
            return self.evaluate_aggregate_rule(rule).await;
        }
        self.evaluate_grouped_rule(rule, group_by).await
    }

    /// The single-aggregate path (unchanged from before Phase 3): query the latest
    /// bucket for the whole (optionally label-filtered) metric and evaluate it.
    async fn evaluate_aggregate_rule(
        &self,
        rule: AlertRule,
    ) -> Result<(), crate::error::OtelError> {
        let now = Utc::now();
        let window = chrono::Duration::seconds(rule.window_secs.max(1) as i64);
        let aggregation = MetricAggregation::parse(&rule.aggregation);
        let (label_filters, _) = rule_query_scope(&rule);
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
            label_filters,
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

        self.evaluate_aggregate_bucket(&rule, latest, aggregation, now)
            .await
    }

    /// Detection + state machine + persist for a single already-selected latest
    /// bucket. Shared by the ungrouped path and the grouped collapse-to-max
    /// (`dynamic_alerts = false`) path — both produce ONE aggregate alarm per rule,
    /// tracked by the existing `breach_start`/`firing` maps.
    async fn evaluate_aggregate_bucket(
        &self,
        rule: &AlertRule,
        latest: &MetricBucket,
        aggregation: MetricAggregation,
        now: DateTime<Utc>,
    ) -> Result<(), crate::error::OtelError> {
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
                FireDetails::static_breach(rule, value, p),
            ),
            DetectionConfig::Anomaly(p) => {
                // Aggregate/rule-level anomaly: no per-series scoping (`&[]`).
                match self
                    .anomaly_eval(rule, latest.bucket, value, p, now, &[])
                    .await?
                {
                    Some(ev) => {
                        let details = FireDetails::anomaly_breach(rule, value, p, &ev);
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
                self.fire(rule, fire_details).await;
                "firing"
            }
            AlertTransition::Resolve => {
                self.resolve(rule.id, rule.project_id).await;
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

    /// The grouped path (ADR-026 Phase 3): query one series per distinct
    /// `group_by` label-set, then either collapse to the single loudest series
    /// (`dynamic_alerts = false`) or run one state machine per series
    /// (`dynamic_alerts = true`, static detectors only).
    async fn evaluate_grouped_rule(
        &self,
        rule: AlertRule,
        group_by: Vec<String>,
    ) -> Result<(), crate::error::OtelError> {
        let now = Utc::now();
        let window = chrono::Duration::seconds(rule.window_secs.max(1) as i64);
        let aggregation = MetricAggregation::parse(&rule.aggregation);
        let (label_filters, _) = rule_query_scope(&rule);
        let config = DetectionConfig::from_value(&rule.detection_config)?;

        let query = MetricQuery {
            project_id: rule.project_id,
            metric_name: Some(rule.metric_name.clone()),
            start_time: Some(now - window),
            end_time: Some(now),
            bucket_interval: Some(format!("{}s", rule.window_secs.max(1))),
            limit: Some(SERIES_QUERY_LIMIT),
            aggregation,
            label_filters,
            group_by,
            ..Default::default()
        };
        let buckets = self.otel_service.query_metrics(query).await?;

        // No data anywhere in the window: preserve state (aggregate view), exactly
        // like the ungrouped no-data path.
        let Some(latest_ts) = buckets.iter().map(|b| b.bucket).max() else {
            if let Err(e) = self
                .alert_service
                .persist_evaluation(rule.id, &rule.last_state, rule.last_value, now)
                .await
            {
                warn!(rule_id = rule.id, error = %e, "Failed to persist no-data grouped evaluation");
            }
            return Ok(());
        };

        // The buckets at the latest tick — one per distinct series.
        let latest_buckets: Vec<&MetricBucket> =
            buckets.iter().filter(|b| b.bucket == latest_ts).collect();

        // Per-series ("dynamic") firing supports static and (robust/basic) anomaly
        // detectors. Forecast/outlier/auto_watch remain genuinely unsupported
        // per-series: the service rejects those combinations at creation, but a
        // rule could predate that guard, so defensively fall back to the collapse
        // path (and log) rather than running an unsupported per-series detector.
        let dynamic_detector = if rule.dynamic_alerts {
            match &config {
                DetectionConfig::Static(p) => Some(DynamicDetector::Static(p)),
                DetectionConfig::Anomaly(p) => Some(DynamicDetector::Anomaly(p)),
                other => {
                    warn!(
                        rule_id = rule.id,
                        kind = other.kind_str(),
                        "dynamic_alerts is unsupported for this detector kind; collapsing to a single aggregate alarm"
                    );
                    None
                }
            }
        } else {
            None
        };

        let Some(detector) = dynamic_detector else {
            // Collapse to the single series with the largest |value| at the latest
            // tick and run the EXISTING aggregate detection/state-machine/persist,
            // i.e. "alert if ANY series breaches" without per-series fan-out.
            let Some(latest) = loudest_bucket(&latest_buckets, aggregation) else {
                // latest_ts existed, so latest_buckets is non-empty. Preserve state
                // rather than panic if that invariant is ever violated.
                return Ok(());
            };
            return self
                .evaluate_aggregate_bucket(&rule, latest, aggregation, now)
                .await;
        };

        // Dynamic per-series firing. `latest_ts` is the scored bucket for every
        // series this tick (all kept buckets share it), used as the anomaly
        // detector's `scored_ts` so each series' baseline excludes the current
        // (possibly anomalous) bucket.
        self.evaluate_dynamic_series(
            &rule,
            detector,
            &latest_buckets,
            aggregation,
            latest_ts,
            now,
        )
        .await
    }

    /// Run one independent state machine per series for a dynamic rule, for either
    /// a static-threshold or an anomaly detector (dispatched by `detector`).
    ///
    /// Ranks the latest tick's series by `|value|`, keeps the top `max_series`,
    /// and drives breach-duration + `evaluate_transition` per kept series keyed by
    /// `(rule_id, series_key)`. Series that dropped out of the kept set but are
    /// still in `firing_series` resolve independently. The rule row's
    /// `last_state`/`last_value` hold the aggregate view (firing if any series is
    /// open; `last_value` = the loudest series' value).
    ///
    /// For the anomaly detector each kept series is scored against its OWN
    /// per-series baseline (see [`Self::series_decisions`]); a series with an
    /// insufficient/degenerate per-series baseline is `preserve`d (left untouched
    /// this tick), exactly like the aggregate anomaly path's `None`.
    async fn evaluate_dynamic_series(
        &self,
        rule: &AlertRule,
        detector: DynamicDetector<'_>,
        latest_buckets: &[&MetricBucket],
        aggregation: MetricAggregation,
        scored_ts: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<(), crate::error::OtelError> {
        // One evaluation point per series at the latest tick.
        let points: Vec<SeriesPoint> = latest_buckets
            .iter()
            .map(|b| {
                let key = b.series_key.clone().unwrap_or_default();
                SeriesPoint {
                    key_str: series_key_string(&key),
                    label: series_label(&key),
                    value: value_for_rule(b, aggregation),
                    key,
                }
            })
            .collect();

        // Cardinality guard: only the top `max_series` by |value| are tracked.
        let max_series = rule.max_series.max(1) as usize;
        let (kept, dropped) = rank_and_cap_series(points, max_series);
        if dropped > 0 {
            warn!(
                rule_id = rule.id,
                dropped,
                max_series,
                "Dynamic alert cardinality cap hit; dropping lowest-|value| series this tick"
            );
        }

        // Per-series breach decisions for the kept set ONLY — anomaly baselines are
        // computed strictly for kept series (never for series beyond the cap), each
        // scoped to that series' own labels. Computed up front so a per-series
        // baseline query error fails the whole tick cleanly (outer loop logs +
        // continues) before any partial fire/resolve is applied.
        let decisions = self
            .series_decisions(rule, &detector, &kept, scored_ts, now)
            .await?;

        // Snapshot the current firing series for this rule: key_str -> (key, alarm_id).
        let firing_snapshot: HashMap<String, FiringSeries> = {
            let firing = self.firing_series.read().await;
            firing
                .iter()
                .filter(|((rid, _), _)| *rid == rule.id)
                .map(|((_, ks), v)| (ks.clone(), v.clone()))
                .collect()
        };
        let kept_keys: HashSet<&str> = kept.iter().map(|p| p.key_str.as_str()).collect();

        // Resolve any series that WAS firing but is absent from this tick's kept
        // set (recovered, disappeared from the query, or dropped out of the top-N).
        for (ks, (_, alarm_id)) in &firing_snapshot {
            if !kept_keys.contains(ks.as_str()) {
                self.resolve_series(rule.id, rule.project_id, *alarm_id)
                    .await;
                self.firing_series
                    .write()
                    .await
                    .remove(&(rule.id, ks.clone()));
                self.breach_start_series
                    .write()
                    .await
                    .remove(&(rule.id, ks.clone()));
            }
        }

        // Per-series state machine over the kept set (each carrying its detector
        // decision). Collect fires (with their pre-built payload) / resolves.
        let for_duration = rule.for_duration_secs.max(0) as u64;
        let mut to_fire: Vec<(&SeriesPoint, FireDetails)> = Vec::new();
        let mut to_resolve: Vec<(String, i32)> = Vec::new();
        for (p, decision) in kept.iter().zip(decisions) {
            // Anomaly series with an insufficient/degenerate per-series baseline:
            // preserve this series' current state this tick — do NOT fire, resolve,
            // or touch its breach timer — mirroring the aggregate anomaly `None`
            // path (no spurious alerts on thin per-series history).
            if decision.preserve {
                continue;
            }
            let key = (rule.id, p.key_str.clone());
            let breaching = decision.breaching;
            let prev_state = if firing_snapshot.contains_key(&p.key_str) {
                "firing"
            } else {
                "ok"
            };
            let breach_elapsed_secs = if breaching {
                let mut starts = self.breach_start_series.write().await;
                let start = starts.entry(key.clone()).or_insert_with(Instant::now);
                start.elapsed().as_secs()
            } else {
                self.breach_start_series.write().await.remove(&key);
                0
            };
            match evaluate_transition(prev_state, breaching, breach_elapsed_secs, for_duration) {
                // `details` is `Some` exactly when breaching, and FireNow implies
                // breaching, so this always carries a payload; the `if let` is a
                // defensive no-op otherwise.
                AlertTransition::FireNow => {
                    if let Some(details) = decision.details {
                        to_fire.push((p, details));
                    }
                }
                AlertTransition::Resolve => {
                    if let Some((_, alarm_id)) = firing_snapshot.get(&p.key_str) {
                        to_resolve.push((p.key_str.clone(), *alarm_id));
                    }
                }
                AlertTransition::StayOk
                | AlertTransition::StartBreach
                | AlertTransition::StayFiring => {}
            }
        }

        // Resolve recovered series.
        for (ks, alarm_id) in to_resolve {
            self.resolve_series(rule.id, rule.project_id, alarm_id)
                .await;
            self.firing_series
                .write()
                .await
                .remove(&(rule.id, ks.clone()));
            self.breach_start_series
                .write()
                .await
                .remove(&(rule.id, ks));
        }

        // Fire newly-breaching series. Notification grouping fallback: when more
        // than the rule's `grouped_notification_threshold` series fire in the same
        // tick, every series still gets its own alarm ROW (needed for per-series
        // resolve + the Phase-4 "Firing instances" UI), but the N individual
        // notifications collapse into ONE combined digest — otherwise a cardinality
        // spike (e.g. 50 series at once) floods every channel with 50 messages.
        let total_firing = to_fire.len();
        let grouped_threshold = rule.grouped_notification_threshold.max(1) as usize;
        if total_firing > grouped_threshold {
            // Grouped path: persist each alarm SILENTLY (no individual
            // notification, no per-series enrichment — a digest renders no
            // per-series charts) and send one digest afterwards.
            let mut fired: Vec<(String, f64, i32)> = Vec::new();
            for (p, details) in to_fire {
                if let Some(alarm_id) = self.fire_series(rule, p, details, false, false).await {
                    self.firing_series
                        .write()
                        .await
                        .insert((rule.id, p.key_str.clone()), (p.key.clone(), alarm_id));
                    fired.push((p.label.clone(), p.value, alarm_id));
                }
            }
            if !fired.is_empty() {
                self.send_series_digest(rule, &fired).await;
            }
        } else {
            // Non-grouped path (≤ threshold): unchanged — each series fires
            // individually (notifying) with full best-effort enrichment.
            for (i, (p, details)) in to_fire.into_iter().enumerate() {
                let enrich = should_enrich(i, total_firing, grouped_threshold);
                if let Some(alarm_id) = self.fire_series(rule, p, details, enrich, true).await {
                    self.firing_series
                        .write()
                        .await
                        .insert((rule.id, p.key_str.clone()), (p.key.clone(), alarm_id));
                }
            }
        }

        // Snapshot the FINAL firing state for this rule after this tick's
        // fires/resolves (key_str -> alarm_id). Non-kept firing series were already
        // resolved+removed above, so every remaining entry corresponds to a kept
        // series — this drives both the aggregate `any_firing` view and the
        // per-series `series_states` snapshot below.
        let final_firing: HashMap<String, i32> = {
            let firing = self.firing_series.read().await;
            firing
                .iter()
                .filter(|((rid, _), _)| *rid == rule.id)
                .map(|((_, ks), (_, alarm_id))| (ks.clone(), *alarm_id))
                .collect()
        };

        // Persist the aggregate rule-row view (firing if ANY series is now open;
        // last_value = the loudest series' value at this tick), PLUS the full
        // per-series snapshot and this tick's cardinality-cap drop count.
        let any_firing = !final_firing.is_empty();
        let agg_value = kept
            .iter()
            .max_by(|a, b| {
                a.value
                    .abs()
                    .partial_cmp(&b.value.abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|p| p.value);
        let next_state = if any_firing { "firing" } else { "ok" };
        let series_states = serde_json::to_value(build_series_states(&kept, &final_firing))
            .unwrap_or_else(|_| json!({}));
        if let Err(e) = self
            .alert_service
            .persist_dynamic_evaluation(
                rule.id,
                next_state,
                agg_value,
                now,
                series_states,
                dropped as i32,
            )
            .await
        {
            warn!(rule_id = rule.id, error = %e, "Failed to persist dynamic rule evaluation");
        }
        Ok(())
    }

    /// Compute each kept series' breach decision + pre-built fire payload for this
    /// tick, dispatching on the detector. The result is parallel to `kept`.
    ///
    /// The static branch is synchronous and detector-parameter-only. The anomaly
    /// branch scores each series against its OWN per-series baseline — passing that
    /// series' label pairs (`&point.key`) as the baseline query's `extra_filters`,
    /// so the band is scoped strictly to that series and can never be contaminated
    /// by another series' data. A `None` from `anomaly_eval` (insufficient/
    /// degenerate per-series baseline — expected more often for a narrow series
    /// slice than in the aggregate case) is surfaced as `preserve = true`, telling
    /// the state machine to leave that series untouched this tick. `details` is
    /// built only when the series is actually breaching (the sole case a fire
    /// needs it), so a non-breaching or preserved series carries `None`.
    async fn series_decisions(
        &self,
        rule: &AlertRule,
        detector: &DynamicDetector<'_>,
        kept: &[SeriesPoint],
        scored_ts: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<Vec<SeriesDecision>, crate::error::OtelError> {
        let mut decisions = Vec::with_capacity(kept.len());
        for point in kept {
            let decision = match *detector {
                DynamicDetector::Static(params) => {
                    let breaching = params.comparator.breaches(point.value, params.threshold);
                    SeriesDecision {
                        breaching,
                        preserve: false,
                        details: breaching
                            .then(|| FireDetails::static_breach(rule, point.value, params)),
                    }
                }
                DynamicDetector::Anomaly(params) => {
                    match self
                        .anomaly_eval(rule, scored_ts, point.value, params, now, &point.key)
                        .await?
                    {
                        Some(ev) => SeriesDecision {
                            breaching: ev.breaching,
                            preserve: false,
                            details: ev.breaching.then(|| {
                                FireDetails::anomaly_breach(rule, point.value, params, &ev)
                            }),
                        },
                        None => SeriesDecision {
                            breaching: false,
                            preserve: true,
                            details: None,
                        },
                    }
                }
            };
            decisions.push(decision);
        }
        Ok(decisions)
    }

    /// Evaluate an anomaly rule's current `value` against a baseline band.
    ///
    /// Returns `Ok(None)` when the baseline is insufficient or degenerate
    /// (too few samples even after the seasonal→global fallback, or a flat band)
    /// — the caller then preserves state rather than firing. The band is the
    /// robust median+MAD of the lookback buckets in the scored point's seasonal
    /// cell, computed through the SAME aggregation as the scored point so counter
    /// rates and histogram percentiles compare like-for-like.
    ///
    /// `extra_filters` scopes the baseline to a subset of the metric: `&[]` for the
    /// aggregate/rule-level baseline (unchanged behaviour), or a dynamic series' own
    /// label pairs so the band is computed strictly from THAT series' history.
    async fn anomaly_eval(
        &self,
        rule: &AlertRule,
        scored_ts: DateTime<Utc>,
        value: f64,
        p: &AnomalyParams,
        now: DateTime<Utc>,
        extra_filters: &[(String, String)],
    ) -> Result<Option<AnomalyEval>, crate::error::OtelError> {
        // Baseline strictly before the scored bucket so the current (possibly
        // anomalous) window can't contaminate its own band.
        let baseline: Vec<(DateTime<Utc>, f64)> = self
            .baseline_values(rule, p, now, extra_filters)
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

    /// Fetch (and cache) the baseline values over the rule's lookback window,
    /// scoped by `extra_filters`.
    ///
    /// Cached for `BASELINE_REFRESH_SECS` so the hot tick scores against the
    /// cached buckets instead of re-querying weeks of data. Values are mapped
    /// through `value_for_rule` so each baseline bucket is the same quantity as
    /// the scored point (rate for counters, percentile for histograms).
    ///
    /// `extra_filters` is `&[]` for the aggregate/rule-level baseline (cache key
    /// discriminator `""`, today's behaviour untouched) or a dynamic series' own
    /// label pairs, which are AND-combined onto the rule's `label_filters` — the EXACT
    /// same merge `chart_svg_with` does — and used as the cache-key discriminator
    /// (via [`baseline_cache_key`]) so each series' baseline is cached and
    /// refreshed independently, scoped strictly to that series.
    async fn baseline_values(
        &self,
        rule: &AlertRule,
        p: &AnomalyParams,
        now: DateTime<Utc>,
        extra_filters: &[(String, String)],
    ) -> Result<Vec<(DateTime<Utc>, f64)>, crate::error::OtelError> {
        let cache_key = baseline_cache_key(rule.id, extra_filters);
        {
            let cache = self.baseline_cache.read().await;
            if let Some(c) = cache.get(&cache_key) {
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
        let (mut label_filters, _) = rule_query_scope(rule);
        label_filters.extend(extra_filters.iter().cloned());
        let query = MetricQuery {
            project_id: rule.project_id,
            metric_name: Some(rule.metric_name.clone()),
            start_time: Some(now - chrono::Duration::days(lookback_days as i64)),
            end_time: Some(now),
            bucket_interval: Some(format!("{}s", rule.window_secs.max(1))),
            limit: None,
            aggregation,
            label_filters,
            ..Default::default()
        };
        let buckets = self.otel_service.query_metrics(query).await?;
        let values: Vec<(DateTime<Utc>, f64)> = buckets
            .iter()
            .map(|b| (b.bucket, value_for_rule(b, aggregation)))
            .collect();

        self.baseline_cache.write().await.insert(
            cache_key,
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

    /// Fire ONE per-series alarm for a dynamic rule via the cooldown-free
    /// `alarm_service_dynamic`, returning the alarm id when created.
    ///
    /// `base` is the detector-built payload (static-threshold or per-series
    /// anomaly) the caller constructed for THIS series' breach — `fire_series`
    /// stays detector-agnostic and only overlays the series identity onto it: the
    /// series label into the alarm title (`"{title} [{label}]"`) and
    /// `metadata.{series_key,series_label,is_dynamic}` (the "Firing instances" UI
    /// and the SVG-escaping chart path both depend on `series_label` being
    /// present). `enrich` gates the two expensive best-effort steps (series-scoped
    /// chart SVG + AI summary); the caller passes `false` for all-but-the-first
    /// firing series once more than the rule's `grouped_notification_threshold`
    /// fire in the same tick. `notify` gates the alarm's individual notification:
    /// `true` for the per-series path (`fire_alarm`), `false` for the grouped path
    /// (`fire_alarm_silent`) where one combined digest is sent instead. The alarm
    /// ROW + `Job::AlarmFired` are persisted either way.
    async fn fire_series(
        &self,
        rule: &AlertRule,
        point: &SeriesPoint,
        base: FireDetails,
        enrich: bool,
        notify: bool,
    ) -> Option<i32> {
        let title = format!("{} [{}]", base.title, point.label);
        let mut metadata = base.metadata;
        if let serde_json::Value::Object(map) = &mut metadata {
            map.insert(
                "series_key".to_string(),
                serde_json::to_value(&point.key).unwrap_or(serde_json::Value::Null),
            );
            map.insert(
                "series_label".to_string(),
                serde_json::Value::String(point.label.clone()),
            );
            map.insert("is_dynamic".to_string(), serde_json::Value::Bool(true));
        }
        let mut message = base.message;

        if enrich {
            // Best-effort chart scoped to THIS series' data (see `chart_svg_with`).
            // Any series-derived text drawn into the SVG is escaped by the renderer.
            if let Some(svg) = self
                .chart_svg_with(rule, &metadata, &point.key, Some(&point.label))
                .await
            {
                if let serde_json::Value::Object(map) = &mut metadata {
                    map.insert("_chart_svg".to_string(), serde_json::Value::String(svg));
                }
            }
            // ADR-022: best-effort AI enrichment, identical policy to `fire`.
            if let Some(ai) = &self.ai {
                if self.ai_summaries_enabled(rule.project_id).await && ai.is_available().await {
                    if let Some(req) = alert_summary_request(rule, &metadata, &message) {
                        let fut = temps_ai::complete_text(ai.as_ref(), req);
                        if let Ok(Some(text)) = tokio::time::timeout(AI_SUMMARY_TIMEOUT, fut).await
                        {
                            let text = text.trim();
                            if !text.is_empty() {
                                if let serde_json::Value::Object(map) = &mut metadata {
                                    map.insert(
                                        "ai_summary".to_string(),
                                        serde_json::Value::Bool(true),
                                    );
                                }
                                message = text.to_string();
                            }
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
            title,
            message,
            metadata: Some(metadata),
        };
        let fired = if notify {
            self.alarm_service_dynamic.fire_alarm(request).await
        } else {
            self.alarm_service_dynamic.fire_alarm_silent(request).await
        };
        match fired {
            Ok(Some(alarm_id)) => {
                info!(
                    rule_id = rule.id,
                    alarm_id,
                    series = %point.label,
                    "OTel per-series metric alert fired"
                );
                Some(alarm_id)
            }
            Ok(None) => {
                debug!(
                    rule_id = rule.id,
                    series = %point.label,
                    "OTel per-series metric alert fire suppressed"
                );
                None
            }
            Err(e) => {
                error!(
                    rule_id = rule.id,
                    series = %point.label,
                    error = %e,
                    "Failed to fire OTel per-series metric alert"
                );
                None
            }
        }
    }

    /// Send ONE combined digest notification for a cardinality spike (more than
    /// the rule's `grouped_notification_threshold` series fired this tick). Each
    /// `fired` entry (`(series_label, value, alarm_id)`) already has its own alarm
    /// row (persisted via `fire_alarm_silent`); this replaces the N individual
    /// notifications with a single message. The full per-series detail is carried
    /// in `metadata.fired_series` so a JSON-consuming webhook still gets everything.
    /// Best-effort — `send_digest_notification` never fails the tick.
    async fn send_series_digest(&self, rule: &AlertRule, fired: &[(String, f64, i32)]) {
        let pairs: Vec<(String, f64)> = fired
            .iter()
            .map(|(label, value, _)| (label.clone(), *value))
            .collect();
        let (title, message) = build_digest_notification(&rule.metric_name, &pairs);

        let fired_series: Vec<serde_json::Value> = fired
            .iter()
            .map(|(label, value, alarm_id)| {
                json!({
                    "series_label": label,
                    "value": value,
                    "alarm_id": alarm_id,
                })
            })
            .collect();
        let metadata = json!({
            "rule_id": rule.id,
            "source": "otel_metric_alert_digest",
            "metric_name": rule.metric_name,
            "fired_series": fired_series,
        });

        self.alarm_service_dynamic
            .send_digest_notification(
                rule.project_id,
                map_severity(&rule.severity),
                title,
                message,
                Some(metadata),
            )
            .await;
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
        self.chart_svg_with(rule, metadata, &[], None).await
    }

    /// Backing impl for [`Self::chart_svg_for`], extended for dynamic per-series
    /// charts: `extra_filters` (the series' own label pairs) are AND-combined onto the
    /// rule's `label_filters` so the chart shows THAT series, and `series_label`,
    /// when present, is drawn into the value label — escaped by
    /// `render_alert_chart_svg` since series values are untrusted.
    async fn chart_svg_with(
        &self,
        rule: &AlertRule,
        metadata: &serde_json::Value,
        extra_filters: &[(String, String)],
        series_label: Option<&str>,
    ) -> Option<String> {
        // Show ~60 windows of context around the breach.
        let window = rule.window_secs.max(60);
        let now = Utc::now();
        let (mut label_filters, _) = rule_query_scope(rule);
        label_filters.extend(extra_filters.iter().cloned());
        let query = MetricQuery {
            project_id: rule.project_id,
            metric_name: Some(rule.metric_name.clone()),
            start_time: Some(now - chrono::Duration::seconds(window as i64 * 60)),
            end_time: Some(now),
            bucket_interval: Some(format!("{}s", window)),
            limit: Some(120),
            aggregation: MetricAggregation::parse(&rule.aggregation),
            label_filters,
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
        // Series label (if any) is passed raw here; `render_alert_chart_svg`
        // applies `svg_escape` to `value_label` before embedding, so untrusted
        // series text is escaped exactly once (no double-encoding).
        let value_label = match series_label {
            Some(sl) => format!("{sl} · last {window_label} {}", rule.aggregation),
            None => format!("last {window_label} {}", rule.aggregation),
        };

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

    /// Resolve the aggregate alarm previously fired for this rule, if any, and
    /// drop its `firing` entry. Takes `(rule_id, project_id)` rather than a full
    /// `AlertRule` so it can also be driven by [`Self::resolve_all_for_rule`] on
    /// delete, where the row is about to disappear — resolving an alarm needs only
    /// the id and the project scope `AlarmService::resolve_alarm` enforces, not the
    /// metric/threshold fields.
    async fn resolve(&self, rule_id: i32, project_id: i32) {
        let alarm_id = self.firing.write().await.remove(&rule_id);
        if let Some(alarm_id) = alarm_id {
            if let Err(e) = self.alarm_service.resolve_alarm(alarm_id, project_id).await {
                error!(
                    rule_id,
                    alarm_id,
                    error = %e,
                    "Failed to resolve OTel metric alert"
                );
            } else {
                info!(rule_id, alarm_id, "OTel metric alert resolved");
            }
        }
    }

    /// Resolve one per-series alarm (dynamic rules) via the cooldown-free service.
    /// The caller removes the `firing_series`/`breach_start_series` entries. Takes
    /// `(rule_id, project_id)` rather than a full `AlertRule` for the same reason
    /// as [`Self::resolve`] — so [`Self::resolve_all_for_rule`] can drive it on a
    /// delete where the row is disappearing.
    async fn resolve_series(&self, rule_id: i32, project_id: i32, alarm_id: i32) {
        if let Err(e) = self
            .alarm_service_dynamic
            .resolve_alarm(alarm_id, project_id)
            .await
        {
            error!(
                rule_id,
                alarm_id,
                error = %e,
                "Failed to resolve OTel per-series metric alert"
            );
        } else {
            info!(rule_id, alarm_id, "OTel per-series metric alert resolved");
        }
    }

    /// Drop any in-flight breach timer for a rule.
    async fn clear_breach(&self, rule_id: i32) {
        self.breach_start.write().await.remove(&rule_id);
    }

    /// Resolve every open alarm (aggregate or per-series) for a rule and drop its
    /// in-memory evaluator state, so deleting a rule with open alarms doesn't
    /// orphan them. Call this BEFORE deleting the rule row — once the row is
    /// gone, `evaluate_rule` will never run for it again to resolve anything.
    ///
    /// Safe to call when the rule has no open alarms and when it was never
    /// evaluated (never entered any map): every resolve is fire-and-forget (logs
    /// on failure via [`Self::resolve`]/[`Self::resolve_series`], never
    /// propagates), and every map removal is a no-op on an absent key — so this
    /// can neither panic nor return an error that would block the delete.
    pub async fn resolve_all_for_rule(&self, rule_id: i32, project_id: i32) {
        // Aggregate alarm (static rules + the collapse-to-max grouped path):
        // `resolve` removes the `firing` entry and resolves its alarm if one was
        // open; `clear_breach` drops the aggregate breach timer. Both no-op when
        // the rule was never firing.
        self.resolve(rule_id, project_id).await;
        self.clear_breach(rule_id).await;

        // Per-series (dynamic) alarms: snapshot the rule's open series under a read
        // lock, drop the lock, then resolve each independently. An empty snapshot
        // (no open per-series alarms, or the rule was never evaluated) is a no-op.
        let to_resolve = {
            let firing_series = self.firing_series.read().await;
            series_alarms_to_resolve(&firing_series, rule_id)
        };
        for (_, alarm_id) in to_resolve {
            self.resolve_series(rule_id, project_id, alarm_id).await;
        }

        // Drop every remaining per-series map entry for this rule — the alarms just
        // resolved plus any breaching-but-not-yet-fired series that only reached
        // `breach_start_series`. Both maps are keyed by `(rule_id, series_key)`, so
        // retain by the first tuple element to leave other rules untouched.
        self.breach_start_series
            .write()
            .await
            .retain(|(rid, _), _| *rid != rule_id);
        self.firing_series
            .write()
            .await
            .retain(|(rid, _), _| *rid != rule_id);
    }

    /// Snapshot the currently-open per-series alarms for `rule_id` as
    /// `(series_key_pairs, alarm_id)`, powering the read handlers' `firing_series`
    /// response field (ADR-026 Phase 3). Reads an in-memory map — no DB.
    pub async fn firing_series_for(&self, rule_id: i32) -> Vec<FiringSeries> {
        self.firing_series
            .read()
            .await
            .iter()
            .filter(|((rid, _), _)| *rid == rule_id)
            .map(|(_, (key, alarm_id))| (key.clone(), *alarm_id))
            .collect()
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

    // ── Dynamic (per-series) helpers ────────────────────────────────

    fn point(key: &[(&str, &str)], value: f64) -> SeriesPoint {
        let key: Vec<(String, String)> = key
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        SeriesPoint {
            key_str: series_key_string(&key),
            label: series_label(&key),
            value,
            key,
        }
    }

    #[test]
    fn test_series_key_string_is_order_independent() {
        // The same label set in different column orders must produce the SAME key,
        // so the per-series state machine is stable regardless of store column order.
        let a = series_key_string(&[
            ("endpoint".to_string(), "/checkout".to_string()),
            ("region".to_string(), "eu-west".to_string()),
        ]);
        let b = series_key_string(&[
            ("region".to_string(), "eu-west".to_string()),
            ("endpoint".to_string(), "/checkout".to_string()),
        ]);
        assert_eq!(a, b);
        // Distinct value sets produce distinct keys.
        let c = series_key_string(&[
            ("endpoint".to_string(), "/cart".to_string()),
            ("region".to_string(), "eu-west".to_string()),
        ]);
        assert_ne!(a, c);
    }

    #[test]
    fn test_series_label_is_sorted() {
        let label = series_label(&[
            ("region".to_string(), "eu-west".to_string()),
            ("endpoint".to_string(), "/checkout".to_string()),
        ]);
        assert_eq!(label, "endpoint=/checkout, region=eu-west");
    }

    // ── baseline_cache_key (per-series baseline scoping) ────────────
    //
    // The async caching around `baseline_values` needs a full evaluator (real
    // OtelService), so only the KEY COMPUTATION — the correctness-critical part
    // (a series' baseline must never share a cache slot with another series' or
    // with the aggregate baseline) — is extracted here as a pure function and
    // tested in isolation. The refresh-window reuse and the label-filter merge are
    // verified by reading (they mirror the aggregate path and `chart_svg_with`).

    #[test]
    fn test_baseline_cache_key_aggregate_uses_empty_discriminator() {
        // The aggregate/rule-level baseline (`&[]`) keeps the exact pre-per-series
        // slot: `(rule_id, "")`. This is what preserves today's aggregate anomaly
        // cache behaviour byte-for-byte.
        assert_eq!(baseline_cache_key(42, &[]), (42, String::new()));
    }

    #[test]
    fn test_baseline_cache_key_per_series_is_distinct_from_aggregate() {
        // A per-series baseline must NOT land in the aggregate slot — otherwise a
        // series would score against (or overwrite) the whole-metric baseline.
        let series = vec![("endpoint".to_string(), "/checkout".to_string())];
        let key = baseline_cache_key(42, &series);
        assert_eq!(key.0, 42);
        assert_ne!(
            key.1,
            String::new(),
            "per-series discriminator must be non-empty"
        );
        assert_ne!(key, baseline_cache_key(42, &[]));
    }

    #[test]
    fn test_baseline_cache_key_distinct_series_do_not_collide() {
        // The crux of the no-cross-contamination guarantee: two different series
        // under the SAME rule get two different cache slots, so one series' band is
        // never computed from or overwritten by another's.
        let a = baseline_cache_key(42, &[("endpoint".to_string(), "/checkout".to_string())]);
        let b = baseline_cache_key(42, &[("endpoint".to_string(), "/cart".to_string())]);
        assert_ne!(a, b);
    }

    #[test]
    fn test_baseline_cache_key_is_series_order_independent() {
        // Same label set in different column orders → same slot, so a series reuses
        // its cached band regardless of the order the store returned its columns.
        let forward = baseline_cache_key(
            42,
            &[
                ("endpoint".to_string(), "/checkout".to_string()),
                ("region".to_string(), "eu-west".to_string()),
            ],
        );
        let reverse = baseline_cache_key(
            42,
            &[
                ("region".to_string(), "eu-west".to_string()),
                ("endpoint".to_string(), "/checkout".to_string()),
            ],
        );
        assert_eq!(forward, reverse);
    }

    #[test]
    fn test_baseline_cache_key_distinct_rules_do_not_collide() {
        // Same series labels under different rules stay in separate slots.
        let series = vec![("endpoint".to_string(), "/checkout".to_string())];
        assert_ne!(
            baseline_cache_key(1, &series),
            baseline_cache_key(2, &series)
        );
        // And the aggregate slots of two rules are distinct too.
        assert_ne!(baseline_cache_key(1, &[]), baseline_cache_key(2, &[]));
    }

    #[test]
    fn test_rank_and_cap_series_keeps_top_by_abs_value() {
        // Mixed signs: ranking is by |value|, so -900 outranks +100.
        let points = vec![
            point(&[("endpoint", "/a")], 100.0),
            point(&[("endpoint", "/b")], -900.0),
            point(&[("endpoint", "/c")], 500.0),
            point(&[("endpoint", "/d")], 10.0),
        ];
        let (kept, dropped) = rank_and_cap_series(points, 2);
        assert_eq!(dropped, 2);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].value, -900.0);
        assert_eq!(kept[1].value, 500.0);
    }

    #[test]
    fn test_rank_and_cap_series_under_cap_drops_nothing() {
        let points = vec![
            point(&[("endpoint", "/a")], 5.0),
            point(&[("endpoint", "/b")], 9.0),
        ];
        let (kept, dropped) = rank_and_cap_series(points, 20);
        assert_eq!(dropped, 0);
        assert_eq!(kept.len(), 2);
        // Still sorted by |value| descending.
        assert_eq!(kept[0].value, 9.0);
    }

    #[test]
    fn test_rank_and_cap_series_deterministic_tie_break() {
        // Equal |value| entries break the tie by key_str so the kept set is stable
        // regardless of input order.
        let forward = vec![point(&[("k", "a")], 7.0), point(&[("k", "b")], 7.0)];
        let reverse = vec![point(&[("k", "b")], 7.0), point(&[("k", "a")], 7.0)];
        let (kf, _) = rank_and_cap_series(forward, 1);
        let (kr, _) = rank_and_cap_series(reverse, 1);
        assert_eq!(kf.len(), 1);
        assert_eq!(kf[0].key_str, kr[0].key_str);
    }

    #[test]
    fn test_loudest_bucket_picks_max_abs() {
        // The collapse-to-max value used when dynamic_alerts=false + group_by set.
        let now = Utc::now();
        let b1 = MetricBucket::scalar(now, 100.0, 0.0, 100.0, 1);
        let b2 = MetricBucket::scalar(now, -750.0, -750.0, 0.0, 1);
        let b3 = MetricBucket::scalar(now, 300.0, 0.0, 300.0, 1);
        let refs = vec![&b1, &b2, &b3];
        let loudest = loudest_bucket(&refs, MetricAggregation::Avg).unwrap();
        // -750 has the largest |value|, so it's chosen to feed the aggregate path.
        assert_eq!(loudest.value, -750.0);
        // Empty slice yields None.
        assert!(loudest_bucket(&[], MetricAggregation::Avg).is_none());
    }

    // ── build_series_states / last_dropped_series_count ─────────────

    #[test]
    fn test_build_series_states_marks_firing_and_ok() {
        // Both evaluated series appear: the firing one carries its alarm id, the
        // recovered-to-ok one is present with a null alarm id.
        let kept = vec![
            point(&[("method", "GET")], 12.5),
            point(&[("method", "POST")], 3.1),
        ];
        let mut firing: HashMap<String, i32> = HashMap::new();
        firing.insert(kept[0].key_str.clone(), 259);

        let states = build_series_states(&kept, &firing);
        assert_eq!(states.len(), 2);

        let get = states.get("method=GET").expect("GET entry present");
        assert_eq!(get.state, "firing");
        assert_eq!(get.value, 12.5);
        assert_eq!(get.alarm_id, Some(259));

        let post = states.get("method=POST").expect("POST entry present");
        assert_eq!(post.state, "ok");
        assert_eq!(post.value, 3.1);
        assert_eq!(post.alarm_id, None);
    }

    #[test]
    fn test_build_series_states_serializes_to_expected_json_shape() {
        // The persisted jsonb matches the ADR-026 follow-up shape exactly, keyed by
        // the human-readable series_label (NOT the internal series_key_string).
        let kept = vec![
            point(&[("method", "GET")], 12.5),
            point(&[("method", "POST")], 3.1),
        ];
        let mut firing: HashMap<String, i32> = HashMap::new();
        firing.insert(kept[0].key_str.clone(), 259);

        let value = serde_json::to_value(build_series_states(&kept, &firing)).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "method=GET": {"state": "firing", "value": 12.5, "alarm_id": 259},
                "method=POST": {"state": "ok", "value": 3.1, "alarm_id": null},
            })
        );
    }

    #[test]
    fn test_build_series_states_excludes_series_not_in_kept() {
        // A series present in the firing map but absent from `kept` (dropped by the
        // cardinality cap or gone from the query) must NOT linger in the snapshot —
        // it reflects only what was actually evaluated this tick.
        let kept = vec![point(&[("method", "GET")], 12.5)];
        let mut firing: HashMap<String, i32> = HashMap::new();
        firing.insert(kept[0].key_str.clone(), 259);
        let stale = point(&[("method", "DELETE")], 99.0);
        firing.insert(stale.key_str.clone(), 300);

        let states = build_series_states(&kept, &firing);
        assert_eq!(states.len(), 1);
        assert!(states.contains_key("method=GET"));
        assert!(!states.contains_key("method=DELETE"));
    }

    #[test]
    fn test_dropped_count_reflects_latest_tick() {
        // The persisted last_dropped_series_count is exactly rank_and_cap_series's
        // dropped output for THIS tick — recomputed each tick, never accumulated.
        let over_cap = vec![
            point(&[("e", "/a")], 100.0),
            point(&[("e", "/b")], 90.0),
            point(&[("e", "/c")], 80.0),
            point(&[("e", "/d")], 70.0),
        ];
        let (_, dropped) = rank_and_cap_series(over_cap, 2);
        assert_eq!(
            dropped as i32, 2,
            "two series above the cap are dropped this tick"
        );

        // A later tick under the cap reports 0, proving the count is not cumulative.
        let under_cap = vec![point(&[("e", "/a")], 100.0)];
        let (_, dropped_next) = rank_and_cap_series(under_cap, 2);
        assert_eq!(
            dropped_next as i32, 0,
            "latest tick under cap reports 0, not 2"
        );
    }

    // ── should_enrich (per-rule grouped_notification_threshold) ─────

    /// The prior hardcoded default, now a per-rule column; used to keep these
    /// pure-function tests grounded on the same behaviour that shipped.
    const DEFAULT_GROUPED_THRESHOLD: usize = 5;

    #[test]
    fn test_should_enrich_single_fire_always_enriches() {
        assert!(should_enrich(0, 1, DEFAULT_GROUPED_THRESHOLD));
    }

    #[test]
    fn test_should_enrich_at_threshold_all_enrich() {
        // At exactly the threshold every series enriches.
        for i in 0..DEFAULT_GROUPED_THRESHOLD {
            assert!(
                should_enrich(i, DEFAULT_GROUPED_THRESHOLD, DEFAULT_GROUPED_THRESHOLD),
                "index {i} should enrich when total == threshold"
            );
        }
    }

    #[test]
    fn test_should_enrich_above_threshold_only_first() {
        // One past the threshold: only index 0 enriches; every other index does not.
        let total = DEFAULT_GROUPED_THRESHOLD + 1;
        assert!(should_enrich(0, total, DEFAULT_GROUPED_THRESHOLD));
        for i in 1..total {
            assert!(
                !should_enrich(i, total, DEFAULT_GROUPED_THRESHOLD),
                "index {i} should NOT enrich when total ({total}) > threshold"
            );
        }
    }

    #[test]
    fn test_should_enrich_large_count_only_first() {
        // With a large count well above the threshold, only index 0 enriches.
        assert!(should_enrich(0, 50, DEFAULT_GROUPED_THRESHOLD));
        assert!(!should_enrich(1, 50, DEFAULT_GROUPED_THRESHOLD));
        assert!(!should_enrich(49, 50, DEFAULT_GROUPED_THRESHOLD));
    }

    #[test]
    fn test_should_enrich_respects_custom_threshold() {
        // A higher per-rule threshold enriches more series before falling back to
        // first-only; a threshold of 1 enriches only the first past a single fire.
        assert!(
            should_enrich(9, 10, 20),
            "under a threshold of 20, all enrich"
        );
        assert!(should_enrich(0, 10, 1));
        assert!(
            !should_enrich(1, 10, 1),
            "threshold of 1 enriches only index 0"
        );
    }

    // ── build_digest_notification (grouped-notification digest) ─────

    /// `n` distinct `(series_label, value)` pairs, one per synthetic endpoint.
    fn digest_series(n: usize) -> Vec<(String, f64)> {
        (0..n)
            .map(|i| (format!("endpoint=/svc{i}"), (i as f64) + 0.5))
            .collect()
    }

    #[test]
    fn test_build_digest_title_counts_all_fired_series() {
        let fired = digest_series(3);
        let (title, message) = build_digest_notification("guestbook.request.duration", &fired);
        assert_eq!(title, "3 series of guestbook.request.duration breached");
        // Under the cap: every series is named, no "and N more" tail.
        assert_eq!(message.lines().count(), 3);
        assert!(message.contains("endpoint=/svc0"));
        assert!(message.contains("endpoint=/svc2"));
        assert!(
            !message.contains("more"),
            "no tail under the cap: {message}"
        );
    }

    #[test]
    fn test_build_digest_message_lists_all_at_exactly_cap() {
        // Exactly DIGEST_MESSAGE_MAX_LISTED series: all listed, still no tail.
        let fired = digest_series(DIGEST_MESSAGE_MAX_LISTED);
        let (title, message) = build_digest_notification("m", &fired);
        assert_eq!(
            title,
            format!("{} series of m breached", DIGEST_MESSAGE_MAX_LISTED)
        );
        assert_eq!(message.lines().count(), DIGEST_MESSAGE_MAX_LISTED);
        assert!(
            !message.contains("more"),
            "no tail at exactly the cap: {message}"
        );
    }

    #[test]
    fn test_build_digest_message_truncates_beyond_cap() {
        // 12 series (the ADR example): the title counts all 12, but the message
        // names only the first 10 and collapses the remaining 2 into "and 2 more".
        let fired = digest_series(12);
        let (title, message) = build_digest_notification("guestbook.request.duration", &fired);
        assert_eq!(title, "12 series of guestbook.request.duration breached");
        // 10 named series + 1 tail line.
        assert_eq!(message.lines().count(), DIGEST_MESSAGE_MAX_LISTED + 1);
        assert!(message.contains("and 2 more"), "{message}");
        // The first 10 are named; the 11th/12th are folded into the tail, not named.
        assert!(message.contains("endpoint=/svc9"));
        assert!(!message.contains("endpoint=/svc10"), "{message}");
        assert!(!message.contains("endpoint=/svc11"), "{message}");
    }

    #[test]
    fn test_build_digest_message_is_newline_separated_for_multi_key_labels() {
        // A multi-key group-by's series_label contains ", " itself, so entries MUST
        // be newline-separated (one series per line) to stay unambiguous.
        let fired = vec![
            ("endpoint=/checkout, region=eu-west".to_string(), 500.0),
            ("endpoint=/cart, region=us-east".to_string(), 750.0),
        ];
        let (_title, message) = build_digest_notification("http.latency", &fired);
        assert_eq!(message.lines().count(), 2, "one line per series: {message}");
        let first = message.lines().next().expect("first line present");
        assert!(
            first.starts_with("endpoint=/checkout, region=eu-west ("),
            "{first}"
        );
    }

    // ── rule_query_scope ────────────────────────────────────────────

    fn make_rule_with_scope(
        label_filters: serde_json::Value,
        group_by: serde_json::Value,
    ) -> AlertRule {
        AlertRule {
            id: 1,
            project_id: 1,
            name: "test-rule".to_string(),
            metric_name: "test.metric".to_string(),
            aggregation: "avg".to_string(),
            detection_kind: "static".to_string(),
            detection_config: serde_json::json!({"kind":"static","comparator":">","threshold":100.0}),
            label_filters,
            group_by,
            dynamic_alerts: false,
            max_series: 20,
            grouped_notification_threshold: 5,
            window_secs: 60,
            for_duration_secs: 0,
            severity: "warning".to_string(),
            enabled: true,
            last_state: "ok".to_string(),
            last_value: None,
            series_states: serde_json::json!({}),
            last_dropped_series_count: 0,
            last_evaluated_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn test_rule_query_scope_populated() {
        let rule = make_rule_with_scope(
            serde_json::json!([["endpoint", "/checkout"], ["region", "eu-west"]]),
            serde_json::json!(["endpoint", "region"]),
        );
        let (label_filters, group_by) = rule_query_scope(&rule);
        assert_eq!(
            label_filters,
            vec![
                ("endpoint".to_string(), "/checkout".to_string()),
                ("region".to_string(), "eu-west".to_string()),
            ]
        );
        assert_eq!(group_by, vec!["endpoint".to_string(), "region".to_string()]);
    }

    #[test]
    fn test_rule_query_scope_empty_decodes_to_empty_vecs() {
        let rule = make_rule_with_scope(serde_json::json!([]), serde_json::json!([]));
        let (label_filters, group_by) = rule_query_scope(&rule);
        assert!(label_filters.is_empty());
        assert!(group_by.is_empty());
    }

    #[test]
    fn test_rule_query_scope_malformed_decodes_to_empty() {
        // Wrong shape jsonb in either column must decode to empty rather than panic.
        let rule = make_rule_with_scope(
            serde_json::json!("not-an-array"),
            serde_json::json!({"this": "is-wrong"}),
        );
        let (label_filters, group_by) = rule_query_scope(&rule);
        assert!(
            label_filters.is_empty(),
            "malformed label_filters should decode to empty"
        );
        assert!(
            group_by.is_empty(),
            "malformed group_by should decode to empty"
        );
    }

    #[test]
    fn test_parse_dynamic_alarm_metadata_restores_firing_series_entry() {
        let metadata = serde_json::json!({
            "is_dynamic": true,
            "rule_id": 7,
            "series_key": [["method", "GET"]],
            "series_label": "method=GET",
        });
        let ((rule_id, key_str), (series_key, alarm_id)) =
            parse_dynamic_alarm_metadata(251, Some(&metadata)).expect("should parse");
        assert_eq!(rule_id, 7);
        assert_eq!(alarm_id, 251);
        assert_eq!(series_key, vec![("method".to_string(), "GET".to_string())]);
        assert_eq!(key_str, series_key_string(&series_key));
    }

    #[test]
    fn test_parse_dynamic_alarm_metadata_skips_non_dynamic_alarm() {
        let metadata = serde_json::json!({
            "rule_id": 7,
            "detection_kind": "static",
        });
        assert!(parse_dynamic_alarm_metadata(251, Some(&metadata)).is_none());
    }

    #[test]
    fn test_parse_dynamic_alarm_metadata_skips_missing_metadata() {
        assert!(parse_dynamic_alarm_metadata(251, None).is_none());
    }

    #[test]
    fn test_parse_dynamic_alarm_metadata_skips_missing_rule_id() {
        let metadata = serde_json::json!({
            "is_dynamic": true,
            "series_key": [["method", "GET"]],
        });
        assert!(parse_dynamic_alarm_metadata(251, Some(&metadata)).is_none());
    }

    #[test]
    fn test_parse_dynamic_alarm_metadata_skips_malformed_series_key() {
        let metadata = serde_json::json!({
            "is_dynamic": true,
            "rule_id": 7,
            "series_key": "not-an-array-of-pairs",
        });
        assert!(parse_dynamic_alarm_metadata(251, Some(&metadata)).is_none());
    }

    // ── series_alarms_to_resolve (resolve_all_for_rule per-series planning) ──
    //
    // resolve_all_for_rule itself is async and needs a full evaluator (real
    // AlarmService/OtelService), which this module deliberately doesn't
    // construct — its map-partitioning decision ("which per-series alarms belong
    // to rule X, keyed by the first element of a (rule_id, series_key) composite
    // key") is extracted here as a pure function and tested in isolation. The
    // resolve fan-out and the RwLock map removals around it are verified by
    // reading, not exercised by a unit test.

    /// A `firing_series`-shaped entry:
    /// `((rule_id, series_key_string), (key_pairs, alarm_id))`.
    fn firing_entry(
        rule_id: i32,
        key: &[(&str, &str)],
        alarm_id: i32,
    ) -> ((i32, String), FiringSeries) {
        let key: Vec<(String, String)> = key
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        ((rule_id, series_key_string(&key)), (key, alarm_id))
    }

    #[test]
    fn test_series_alarms_to_resolve_selects_only_the_rules_series() {
        // Two rules share the map; only rule 7's open series are selected, so
        // resolve_all_for_rule(7, ..) never touches rule 9's alarm.
        let mut firing: HashMap<(i32, String), FiringSeries> = HashMap::new();
        let (k, v) = firing_entry(7, &[("endpoint", "/a")], 101);
        firing.insert(k, v);
        let (k, v) = firing_entry(7, &[("endpoint", "/b")], 102);
        firing.insert(k, v);
        let (k, v) = firing_entry(9, &[("endpoint", "/a")], 200);
        firing.insert(k, v);

        let selected = series_alarms_to_resolve(&firing, 7);
        let mut alarm_ids: Vec<i32> = selected.iter().map(|(_, alarm_id)| *alarm_id).collect();
        alarm_ids.sort_unstable();
        assert_eq!(
            alarm_ids,
            vec![101, 102],
            "both of rule 7's alarms, only those"
        );
        // Every selected key carries rule_id 7 (never 9): the match is on the first
        // element of the (rule_id, series_key) key, not the whole tuple.
        assert!(selected.iter().all(|((rid, _), _)| *rid == 7));
    }

    #[test]
    fn test_series_alarms_to_resolve_absent_rule_is_empty() {
        // A rule with no entries (never evaluated / already resolved) yields an
        // empty plan — resolve_all_for_rule is then a pure no-op for the series maps.
        let mut firing: HashMap<(i32, String), FiringSeries> = HashMap::new();
        let (k, v) = firing_entry(9, &[("endpoint", "/a")], 200);
        firing.insert(k, v);
        assert!(series_alarms_to_resolve(&firing, 7).is_empty());

        // An empty map is safe too (rule never fired anything).
        let empty: HashMap<(i32, String), FiringSeries> = HashMap::new();
        assert!(series_alarms_to_resolve(&empty, 7).is_empty());
    }

    #[test]
    fn test_series_alarms_to_resolve_returns_matching_keys() {
        // The returned keys are the exact map keys for the rule's series, so the
        // caller could remove precisely those (resolve_all_for_rule uses a broader
        // retain, but the selection must still be key-accurate).
        let mut firing: HashMap<(i32, String), FiringSeries> = HashMap::new();
        let (k, v) = firing_entry(5, &[("method", "GET")], 42);
        firing.insert(k.clone(), v);
        let selected = series_alarms_to_resolve(&firing, 5);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].0, k);
        assert_eq!(selected[0].1, 42);
    }
}
