//! Typed, polymorphic detector definitions for metric alert rules.
//!
//! A rule's detector lives in `metric_alert_rules.detection_config` (jsonb). The
//! raw `serde_json::Value` exists ONLY on the sea-orm entity column; every
//! service and DTO layer uses the typed [`DetectionConfig`] here — mirroring the
//! sanctioned `temps_revenue::providers::ProviderConfig` +
//! `revenue_integrations.config` precedent. This satisfies the "never expose
//! untyped `serde_json::Value` on the API surface" rule: the API surface is
//! 100% typed; only the storage column is a `Value`.
//!
//! Forward-compatibility is the whole point: a new detector family
//! (anomaly→EWMA→forecast→outlier→auto-watch) is a **new enum variant + a
//! `validate` arm + an evaluator branch + `openapi-ts` regen** — never a
//! migration. `detection_kind` is a plain `String` column (not a Postgres enum),
//! so a new kind also needs no `ALTER TYPE`.
//!
//! Tagging follows the `ProviderConfig` precedent *verbatim*: internally tagged
//! by `kind`, with NO `#[schema(discriminator)]` (which is a compile error
//! alongside `serde(tag)` in utoipa 5.x). utoipa + hey-api render this as a
//! usable `(StaticParams & { kind: 'static' }) | …` TS discriminated union.

use std::collections::HashMap;

use chrono::{DateTime, Datelike, Timelike, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::error::OtelError;

fn default_deviations() -> f64 {
    3.0
}
fn default_pct_anomalous() -> f64 {
    1.0
}

/// Floor applied to a band's scale before dividing, so a (near-)flat baseline
/// can't produce an infinite z-score. A scale below this is treated as
/// degenerate by the evaluator (insufficient baseline → preserve state).
pub const MIN_BAND_SCALE: f64 = 1e-9;

/// Minimum baseline samples for a trustworthy band. Below this the band is not
/// used (preserve state). Shared by the evaluator and the preview endpoint.
pub const MIN_BASELINE_SAMPLES: usize = 8;

/// Default anomaly baseline lookback (days) when a rule doesn't set one.
pub const DEFAULT_LOOKBACK_DAYS: i32 = 14;

/// The robust band center + scale for an anomaly detector, from baseline values:
/// `(median, MAD · 1.4826)`. The 1.4826 factor makes MAD a consistent estimator
/// of the standard deviation for normal data, so `deviations` keeps the same
/// "sigmas" meaning as Datadog's `bounds`. Returns `None` for an empty baseline.
pub fn robust_band(values: &[f64]) -> Option<(f64, f64)> {
    if values.is_empty() {
        return None;
    }
    let center = median(values);
    let deviations: Vec<f64> = values.iter().map(|v| (v - center).abs()).collect();
    let mad = median(&deviations);
    Some((center, mad * 1.4826))
}

/// Median of `values` (ignoring non-finite entries). Returns 0.0 if empty.
fn median(values: &[f64]) -> f64 {
    let mut v: Vec<f64> = values.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Whether `value` deviates from the band `(center, scale)` by more than
/// `deviations` scaled units, honouring `direction`. `scale` is floored by
/// [`MIN_BAND_SCALE`] so a flat band can't divide by zero (the evaluator treats
/// a degenerate scale as insufficient before calling this — this is defence).
pub fn anomaly_breaches(
    value: f64,
    center: f64,
    scale: f64,
    deviations: f64,
    direction: Direction,
) -> bool {
    let z = (value - center) / scale.max(MIN_BAND_SCALE);
    match direction {
        Direction::Both => z.abs() > deviations,
        Direction::Above => z > deviations,
        Direction::Below => -z > deviations,
    }
}

/// A precomputed anomaly band: per-seasonal-cell robust `(center, scale)` bands
/// plus a global fallback, built once from baseline values and then queried per
/// timestamp. Shared by the evaluator (scores the current point) and the preview
/// endpoint (scores a whole range) so they can never diverge.
pub struct BandModel {
    seasonality: Seasonality,
    /// cell id -> (center, scale), only for cells with enough samples.
    cells: HashMap<i64, (f64, f64)>,
    /// Global band over all baseline values, used when a cell is too thin.
    global: Option<(f64, f64)>,
    /// Total baseline samples (for the insufficiency check).
    pub samples: usize,
}

impl BandModel {
    /// Build from `(timestamp, value)` baseline pairs. A seasonal cell gets its
    /// own band only when it has `>= min_cell_samples`; otherwise that cell falls
    /// back to the global band (cold-start behaviour).
    pub fn from_baseline(
        values: &[(DateTime<Utc>, f64)],
        seasonality: Seasonality,
        min_cell_samples: usize,
    ) -> Self {
        let mut by_cell: HashMap<i64, Vec<f64>> = HashMap::new();
        for (ts, v) in values {
            by_cell
                .entry(season_cell(*ts, seasonality))
                .or_default()
                .push(*v);
        }
        let mut cells = HashMap::new();
        for (cell, vals) in &by_cell {
            if vals.len() >= min_cell_samples {
                if let Some(band) = robust_band(vals) {
                    cells.insert(*cell, band);
                }
            }
        }
        let all: Vec<f64> = values.iter().map(|(_, v)| *v).collect();
        BandModel {
            seasonality,
            cells,
            global: robust_band(&all),
            samples: values.len(),
        }
    }

    /// The `(center, scale)` band that applies at `ts` — its seasonal cell's band
    /// if that cell was dense enough, else the global band.
    pub fn band_for(&self, ts: DateTime<Utc>) -> Option<(f64, f64)> {
        let cell = season_cell(ts, self.seasonality);
        self.cells.get(&cell).copied().or(self.global)
    }

    /// Whether `value` at `ts` breaches the band. `None` when there is no usable
    /// band (no baseline, or a degenerate/flat scale) — the caller then preserves
    /// state rather than firing.
    pub fn breaches(
        &self,
        ts: DateTime<Utc>,
        value: f64,
        deviations: f64,
        direction: Direction,
    ) -> Option<bool> {
        let (center, scale) = self.band_for(ts)?;
        if scale < MIN_BAND_SCALE {
            return None;
        }
        Some(anomaly_breaches(
            value, center, scale, deviations, direction,
        ))
    }
}

/// A comparable "seasonal cell" id for `ts` under `seasonality`. Baseline buckets
/// are filtered to those sharing the scored bucket's cell, so e.g. a Tuesday-2pm
/// value is compared only against historical Tuesday-2pm values.
pub fn season_cell(ts: DateTime<Utc>, seasonality: Seasonality) -> i64 {
    match seasonality {
        // One global cell — the band spans the whole lookback.
        Seasonality::None => 0,
        // Pattern repeats each hour: cell = minute-of-hour.
        Seasonality::Hourly => ts.minute() as i64,
        // Pattern repeats each day: cell = hour-of-day.
        Seasonality::Daily => ts.hour() as i64,
        // Pattern repeats each week: cell = (weekday, hour-of-day).
        Seasonality::Weekly => ts.weekday().num_days_from_monday() as i64 * 24 + ts.hour() as i64,
    }
}

/// The typed detector definition stored (as jsonb) in
/// `metric_alert_rules.detection_config`.
///
/// Today only [`DetectionConfig::Static`] is evaluable; the other variants are
/// schema-present (so the SDK/UI and storage are already future-shaped) but
/// rejected by [`DetectionConfig::validate`] until their evaluator lands. Each is
/// then enabled code-only, with no schema migration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DetectionConfig {
    /// v0 (shipping): static threshold comparison of the aggregated value.
    Static(StaticParams),
    /// Seasonal anomaly band (basic/agile/robust/ewma share this variant — the
    /// algorithm is a field, not a new kind). Creation rejected until evaluated.
    Anomaly(AnomalyParams),
    /// Predict a future threshold breach (capacity planning). Stub.
    Forecast(ForecastParams),
    /// Cross-series population outlier (one host misbehaving vs its peers). Stub.
    Outlier(OutlierParams),
    /// Watchdog-style self-tuning auto-watch (engine picks bounds). Stub.
    AutoWatch(AutoWatchParams),
}

/// Static threshold detector: compare the aggregated `value` against `threshold`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct StaticParams {
    /// How `value` is compared against `threshold`.
    pub comparator: Comparator,
    /// The threshold the aggregated value is compared against.
    pub threshold: f64,
}

/// Seasonal anomaly-band detector parameters (stub — not yet evaluated).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AnomalyParams {
    /// Baseline model. `robust` is the default (seasonal, stable, flags level
    /// shifts); `ewma`/`agile` adopt level shifts; `basic` is non-seasonal.
    #[serde(default)]
    pub algorithm: AnomalyAlgorithm,
    /// Band width in robust standard deviations (Datadog's `bounds`).
    #[serde(default = "default_deviations")]
    pub deviations: f64,
    /// Which side(s) of the band a deviation must be on to count.
    #[serde(default)]
    pub direction: Direction,
    /// Seasonality model for the baseline.
    #[serde(default)]
    pub seasonality: Seasonality,
    /// Fraction (0..=1) of points in the window that must be anomalous to fire.
    #[serde(default = "default_pct_anomalous")]
    pub pct_anomalous: f64,
    /// How far back to build the baseline. `None` = an evaluator default.
    #[serde(default)]
    pub baseline_lookback_days: Option<i32>,
}

/// Forecast detector parameters (stub — not yet evaluated).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ForecastParams {
    #[serde(default)]
    pub algorithm: ForecastAlgorithm,
    /// How far ahead to project before checking the breach condition.
    pub forecast_horizon_secs: i32,
    #[serde(default = "default_deviations")]
    pub deviations: f64,
    /// Comparator + threshold the *forecast* is checked against.
    pub comparator: Comparator,
    pub threshold: f64,
}

/// Outlier (cross-series population) detector parameters (stub — not evaluated).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct OutlierParams {
    #[serde(default)]
    pub algorithm: OutlierAlgorithm,
    /// Sensitivity; higher tolerates larger spread before flagging.
    #[serde(default = "default_deviations")]
    pub tolerance: f64,
    /// Label key defining the peer population compared across series (e.g. `host`).
    pub peer_group_key: String,
}

/// Auto-watch (Watchdog-style) detector parameters (stub — not evaluated).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AutoWatchParams {
    /// The engine self-tunes the band; the user supplies only the direction.
    #[serde(default)]
    pub direction: Direction,
}

/// Comparator for static/forecast threshold detectors. Serializes to the
/// keyword forms `gt|gte|lt|lte` (NOT the SQL operators used by
/// `temps-monitoring::compare`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Comparator {
    #[default]
    Gt,
    Gte,
    Lt,
    Lte,
}

impl Comparator {
    /// Whether `value` breaches `threshold` under this comparator.
    pub fn breaches(self, value: f64, threshold: f64) -> bool {
        match self {
            Comparator::Gt => value > threshold,
            Comparator::Gte => value >= threshold,
            Comparator::Lt => value < threshold,
            Comparator::Lte => value <= threshold,
        }
    }

    /// A human-readable operator symbol for alarm messages.
    pub fn symbol(self) -> &'static str {
        match self {
            Comparator::Gt => ">",
            Comparator::Gte => ">=",
            Comparator::Lt => "<",
            Comparator::Lte => "<=",
        }
    }
}

/// Which side(s) of an anomaly band count as a deviation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    #[default]
    Both,
    Above,
    Below,
}

/// Seasonality model for an anomaly baseline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Seasonality {
    #[default]
    None,
    Hourly,
    Daily,
    Weekly,
}

/// Anomaly baseline algorithm. Adding one (e.g. a new robust variant) is a
/// code-only enum addition — no migration, since it lives inside the blob.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AnomalyAlgorithm {
    #[default]
    Robust,
    Basic,
    Agile,
    Ewma,
}

/// Forecast model family.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ForecastAlgorithm {
    #[default]
    Linear,
    Seasonal,
}

/// Outlier detection algorithm.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutlierAlgorithm {
    #[default]
    Dbscan,
    ScaledDbscan,
    Mad,
    ScaledMad,
}

impl DetectionConfig {
    /// The `detection_kind` column value mirroring this variant's serde tag.
    pub fn kind_str(&self) -> &'static str {
        match self {
            DetectionConfig::Static(_) => "static",
            DetectionConfig::Anomaly(_) => "anomaly",
            DetectionConfig::Forecast(_) => "forecast",
            DetectionConfig::Outlier(_) => "outlier",
            DetectionConfig::AutoWatch(_) => "auto_watch",
        }
    }

    /// An infallible fallback used when a *stored* blob fails to deserialize.
    /// Only reachable on DB corruption — every write is validated and round-
    /// tripped through this type, so a persisted blob is always well-formed.
    pub fn default_static() -> Self {
        DetectionConfig::Static(StaticParams {
            comparator: Comparator::Gt,
            threshold: 0.0,
        })
    }

    /// Deserialize from the stored jsonb value (typed error on malformed input).
    pub fn from_value(value: &serde_json::Value) -> Result<Self, OtelError> {
        serde_json::from_value(value.clone()).map_err(|e| OtelError::Validation {
            message: format!("invalid detection_config: {e}"),
        })
    }

    /// Serialize to a jsonb value for persistence.
    pub fn to_value(&self) -> Result<serde_json::Value, OtelError> {
        serde_json::to_value(self).map_err(|e| OtelError::Validation {
            message: format!("failed to serialize detection_config: {e}"),
        })
    }

    /// Validate the detector's invariants. `static` and `anomaly` (robust/basic
    /// band) are evaluable; `forecast`/`outlier`/`auto_watch` (and the
    /// agile/ewma anomaly algorithms) are typed, schema-present stubs rejected
    /// here until their evaluator lands — enabling each later is code-only,
    /// never a migration.
    pub fn validate(&self) -> Result<(), OtelError> {
        match self {
            DetectionConfig::Static(p) => {
                if !p.threshold.is_finite() {
                    return Err(OtelError::Validation {
                        message: "threshold must be a finite number".to_string(),
                    });
                }
                Ok(())
            }
            DetectionConfig::Anomaly(p) => {
                if !p.deviations.is_finite() || p.deviations <= 0.0 {
                    return Err(OtelError::Validation {
                        message: "anomaly deviations must be a finite number > 0".to_string(),
                    });
                }
                if !(p.pct_anomalous > 0.0 && p.pct_anomalous <= 1.0) {
                    return Err(OtelError::Validation {
                        message: "anomaly pct_anomalous must be in (0, 1]".to_string(),
                    });
                }
                if let Some(days) = p.baseline_lookback_days {
                    if !(1..=90).contains(&days) {
                        return Err(OtelError::Validation {
                            message: "anomaly baseline_lookback_days must be between 1 and 90"
                                .to_string(),
                        });
                    }
                }
                match p.algorithm {
                    // v1 implements a robust seasonal MAD band; `basic` is the
                    // same math with `seasonality=none`.
                    AnomalyAlgorithm::Robust | AnomalyAlgorithm::Basic => Ok(()),
                    AnomalyAlgorithm::Agile | AnomalyAlgorithm::Ewma => {
                        Err(OtelError::Validation {
                            message: "anomaly algorithm 'agile'/'ewma' is not yet supported \
                                      (use 'robust' or 'basic')"
                                .to_string(),
                        })
                    }
                }
            }
            other => Err(OtelError::Validation {
                message: format!("detector kind '{}' is not yet supported", other.kind_str()),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_static_round_trip() {
        let cfg = DetectionConfig::Static(StaticParams {
            comparator: Comparator::Gte,
            threshold: 500.0,
        });
        let v = cfg.to_value().unwrap();
        // Internally tagged: the discriminator + the variant's fields are flat.
        assert_eq!(v["kind"], "static");
        assert_eq!(v["comparator"], "gte");
        assert_eq!(v["threshold"], 500.0);
        let back = DetectionConfig::from_value(&v).unwrap();
        assert_eq!(back, cfg);
        assert_eq!(back.kind_str(), "static");
    }

    #[test]
    fn test_anomaly_round_trip_with_defaults() {
        // A minimal anomaly blob (only the required-by-shape fields) fills the
        // rest from serde defaults — proving additive fields are forward-compat.
        let v = serde_json::json!({ "kind": "anomaly" });
        let cfg = DetectionConfig::from_value(&v).unwrap();
        match &cfg {
            DetectionConfig::Anomaly(p) => {
                assert_eq!(p.algorithm, AnomalyAlgorithm::Robust);
                assert_eq!(p.deviations, 3.0);
                assert_eq!(p.direction, Direction::Both);
                assert_eq!(p.seasonality, Seasonality::None);
                assert_eq!(p.pct_anomalous, 1.0);
                assert_eq!(p.baseline_lookback_days, None);
            }
            _ => panic!("expected anomaly"),
        }
        assert_eq!(cfg.kind_str(), "anomaly");
    }

    #[test]
    fn test_validate_static_ok() {
        let cfg = DetectionConfig::Static(StaticParams {
            comparator: Comparator::Lt,
            threshold: 1.0,
        });
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_validate_static_rejects_non_finite_threshold() {
        let cfg = DetectionConfig::Static(StaticParams {
            comparator: Comparator::Gt,
            threshold: f64::INFINITY,
        });
        assert!(matches!(cfg.validate(), Err(OtelError::Validation { .. })));
    }

    #[test]
    fn test_validate_anomaly_ok_and_rejections() {
        // Default anomaly (robust) is now evaluable.
        let anomaly =
            DetectionConfig::from_value(&serde_json::json!({ "kind": "anomaly" })).unwrap();
        assert!(anomaly.validate().is_ok());

        // agile/ewma algorithms are typed but not yet implemented → rejected.
        let ewma = DetectionConfig::from_value(
            &serde_json::json!({ "kind": "anomaly", "algorithm": "ewma" }),
        )
        .unwrap();
        assert!(matches!(ewma.validate(), Err(OtelError::Validation { .. })));

        // Bad hyperparameters are rejected.
        let bad_dev = DetectionConfig::from_value(
            &serde_json::json!({ "kind": "anomaly", "deviations": 0.0 }),
        )
        .unwrap();
        assert!(matches!(
            bad_dev.validate(),
            Err(OtelError::Validation { .. })
        ));
        let bad_pct = DetectionConfig::from_value(
            &serde_json::json!({ "kind": "anomaly", "pct_anomalous": 1.5 }),
        )
        .unwrap();
        assert!(matches!(
            bad_pct.validate(),
            Err(OtelError::Validation { .. })
        ));
        let bad_lookback = DetectionConfig::from_value(
            &serde_json::json!({ "kind": "anomaly", "baseline_lookback_days": 9999 }),
        )
        .unwrap();
        assert!(matches!(
            bad_lookback.validate(),
            Err(OtelError::Validation { .. })
        ));
    }

    #[test]
    fn test_validate_other_kinds_still_rejected() {
        // forecast/outlier/auto_watch remain typed-but-unsupported stubs.
        let auto = DetectionConfig::AutoWatch(AutoWatchParams::default());
        assert!(matches!(auto.validate(), Err(OtelError::Validation { .. })));
        let outlier = DetectionConfig::from_value(
            &serde_json::json!({ "kind": "outlier", "peer_group_key": "host" }),
        )
        .unwrap();
        assert!(matches!(
            outlier.validate(),
            Err(OtelError::Validation { .. })
        ));
    }

    #[test]
    fn test_robust_band() {
        // Symmetric data: median 30, abs-devs [20,10,0,10,20] → MAD 10 → scale 14.826.
        let (center, scale) = robust_band(&[10.0, 20.0, 30.0, 40.0, 50.0]).unwrap();
        assert!((center - 30.0).abs() < 1e-9);
        assert!((scale - 14.826).abs() < 1e-3);
        // A flat baseline has zero scale (the evaluator treats this as degenerate).
        let (c, s) = robust_band(&[5.0, 5.0, 5.0]).unwrap();
        assert_eq!(c, 5.0);
        assert_eq!(s, 0.0);
        assert!(robust_band(&[]).is_none());
    }

    #[test]
    fn test_anomaly_breaches_direction() {
        // center 100, scale 10, deviations 3 → band [70, 130].
        assert!(anomaly_breaches(140.0, 100.0, 10.0, 3.0, Direction::Both));
        assert!(anomaly_breaches(50.0, 100.0, 10.0, 3.0, Direction::Both));
        assert!(!anomaly_breaches(120.0, 100.0, 10.0, 3.0, Direction::Both));
        // Above only catches high excursions.
        assert!(anomaly_breaches(140.0, 100.0, 10.0, 3.0, Direction::Above));
        assert!(!anomaly_breaches(50.0, 100.0, 10.0, 3.0, Direction::Above));
        // Below only catches low excursions.
        assert!(anomaly_breaches(50.0, 100.0, 10.0, 3.0, Direction::Below));
        assert!(!anomaly_breaches(140.0, 100.0, 10.0, 3.0, Direction::Below));
        // A flat band (scale 0) is floored, not a divide-by-zero panic.
        assert!(anomaly_breaches(5.0001, 5.0, 0.0, 3.0, Direction::Both));
    }

    #[test]
    fn test_season_cell() {
        use chrono::TimeZone;
        // 2026-06-23 is a Tuesday, 14:35 UTC.
        let t = Utc.with_ymd_and_hms(2026, 6, 23, 14, 35, 0).unwrap();
        assert_eq!(season_cell(t, Seasonality::None), 0);
        assert_eq!(season_cell(t, Seasonality::Hourly), 35); // minute-of-hour
        assert_eq!(season_cell(t, Seasonality::Daily), 14); // hour-of-day
                                                            // Tuesday = num_days_from_monday 1 → 1*24 + 14 = 38.
        assert_eq!(season_cell(t, Seasonality::Weekly), 38);
    }

    #[test]
    fn test_band_model() {
        use chrono::TimeZone;
        // Non-seasonal: one global band over all values.
        let vals: Vec<(DateTime<Utc>, f64)> = (0..12)
            .map(|i| {
                (
                    Utc.with_ymd_and_hms(2026, 6, 1, i, 0, 0).unwrap(),
                    100.0 + (i as f64 % 7.0 - 3.0) * 5.0,
                )
            })
            .collect();
        let band = BandModel::from_baseline(&vals, Seasonality::None, 8);
        assert_eq!(band.samples, 12);
        let any_ts = Utc.with_ymd_and_hms(2026, 6, 2, 5, 0, 0).unwrap();
        let (center, scale) = band.band_for(any_ts).expect("global band");
        assert!((center - 100.0).abs() < 10.0);
        assert!(scale > 0.0);
        // A value far outside the band breaches; one near the center does not.
        assert_eq!(
            band.breaches(any_ts, 100_000.0, 3.0, Direction::Both),
            Some(true)
        );
        assert_eq!(
            band.breaches(any_ts, center, 3.0, Direction::Both),
            Some(false)
        );

        // A flat baseline → degenerate band → breaches returns None (preserve).
        let flat: Vec<(DateTime<Utc>, f64)> = (0..10)
            .map(|i| (Utc.with_ymd_and_hms(2026, 6, 1, i, 0, 0).unwrap(), 42.0))
            .collect();
        let flat_band = BandModel::from_baseline(&flat, Seasonality::None, 8);
        assert_eq!(flat_band.breaches(any_ts, 99.0, 3.0, Direction::Both), None);
    }

    #[test]
    fn test_comparator_breaches() {
        assert!(Comparator::Gt.breaches(600.0, 500.0));
        assert!(!Comparator::Gt.breaches(500.0, 500.0));
        assert!(Comparator::Gte.breaches(500.0, 500.0));
        assert!(Comparator::Lt.breaches(400.0, 500.0));
        assert!(!Comparator::Lt.breaches(500.0, 500.0));
        assert!(Comparator::Lte.breaches(500.0, 500.0));
    }

    #[test]
    fn test_unknown_kind_hard_fails() {
        // An unknown top-level kind must NOT silently degrade — it fails to parse.
        let v = serde_json::json!({ "kind": "telepathy", "threshold": 1.0 });
        assert!(DetectionConfig::from_value(&v).is_err());
    }

    #[test]
    fn test_default_static_fallback() {
        let cfg = DetectionConfig::default_static();
        assert_eq!(cfg.kind_str(), "static");
        assert!(cfg.validate().is_ok());
    }
}
