//! Z-score based anomaly detector with time-aware baselines.

use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error};

use chrono::{Datelike, Timelike};

use crate::storage::OtelStorage;

/// Configuration for the anomaly detector.
#[derive(Debug, Clone)]
pub struct AnomalyDetectorConfig {
    /// Lookback window in days for baseline computation.
    pub lookback_days: i32,
    /// Z-score threshold for anomaly detection.
    pub z_score_threshold: f64,
    /// Consecutive anomalous 1-minute buckets required for sustained anomaly.
    pub sustained_minutes: usize,
    /// R² threshold for drift detection.
    pub drift_r_squared_threshold: f64,
    /// Minutes to check for co-occurring anomalies for correlation.
    pub correlation_window_minutes: i32,
    /// Minutes to look back for deploy correlation.
    pub deploy_correlation_minutes: i32,
}

impl Default for AnomalyDetectorConfig {
    fn default() -> Self {
        Self {
            lookback_days: 14,
            z_score_threshold: 3.0,
            sustained_minutes: 8,
            drift_r_squared_threshold: 0.7,
            correlation_window_minutes: 5,
            deploy_correlation_minutes: 30,
        }
    }
}

/// Detected anomaly before correlation and insight generation.
#[derive(Debug, Clone)]
pub struct DetectedAnomaly {
    pub project_id: i32,
    pub service_name: String,
    pub environment: Option<String>,
    pub metric_name: String,
    pub z_score: f64,
    pub current_value: f64,
    pub baseline_avg: f64,
    pub baseline_stddev: f64,
    pub anomaly_type: AnomalyType,
    pub detected_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AnomalyType {
    /// Single metric exceeds Z-score threshold for sustained period.
    Sustained,
    /// Metric shows gradual drift (linear regression).
    Drift,
}

/// Anomaly detector that runs periodically.
pub struct AnomalyDetector {
    storage: Arc<dyn OtelStorage>,
    config: AnomalyDetectorConfig,
}

impl AnomalyDetector {
    pub fn new(storage: Arc<dyn OtelStorage>, config: AnomalyDetectorConfig) -> Self {
        Self { storage, config }
    }

    /// Start the periodic anomaly detection loop.
    pub async fn start(self: Arc<Self>, project_ids: Vec<i32>) {
        let mut interval = tokio::time::interval(Duration::from_secs(60));

        loop {
            interval.tick().await;

            for &project_id in &project_ids {
                match self.detect_anomalies(project_id).await {
                    Ok(anomalies) => {
                        if !anomalies.is_empty() {
                            debug!(project_id, count = anomalies.len(), "Detected anomalies");
                        }
                    }
                    Err(e) => {
                        error!(
                            project_id,
                            error = %e,
                            "Anomaly detection failed"
                        );
                    }
                }
            }
        }
    }

    /// Run anomaly detection for a single project.
    pub async fn detect_anomalies(
        &self,
        project_id: i32,
    ) -> Result<Vec<DetectedAnomaly>, crate::error::OtelError> {
        let metric_names = self.storage.list_metric_names(project_id).await?;
        let mut anomalies = Vec::new();

        for metric_name in &metric_names {
            // Get baseline data
            let baselines = self
                .storage
                .get_metric_baseline(
                    project_id,
                    "", // all services aggregated
                    metric_name,
                    None,
                    self.config.lookback_days,
                )
                .await?;

            if baselines.is_empty() {
                continue;
            }

            // Get recent 1-minute aggregates
            let recent = self
                .storage
                .get_recent_minute_aggregates(
                    project_id,
                    "",
                    metric_name,
                    None,
                    self.config.sustained_minutes as i32 + 2,
                )
                .await?;

            if recent.len() < self.config.sustained_minutes {
                continue;
            }

            // Find the matching baseline for current time
            let now = chrono::Utc::now();
            let current_hour = now.hour() as i32;
            let current_dow = now.weekday().num_days_from_sunday() as i32;

            let baseline = baselines
                .iter()
                .find(|b| b.hour_of_day == current_hour && b.day_of_week == current_dow);

            let Some(baseline) = baseline else {
                continue;
            };

            if baseline.stddev_value == 0.0 || baseline.sample_count < 10 {
                continue;
            }

            // Check for sustained anomaly
            let sustained_count = recent
                .iter()
                .rev()
                .take(self.config.sustained_minutes)
                .filter(|m| {
                    let z = (m.avg_value - baseline.avg_value).abs() / baseline.stddev_value;
                    z > self.config.z_score_threshold
                })
                .count();

            if sustained_count >= self.config.sustained_minutes {
                let latest = recent.last().unwrap();
                let z_score = (latest.avg_value - baseline.avg_value).abs() / baseline.stddev_value;

                anomalies.push(DetectedAnomaly {
                    project_id,
                    service_name: String::new(), // aggregated
                    environment: None,
                    metric_name: metric_name.clone(),
                    z_score,
                    current_value: latest.avg_value,
                    baseline_avg: baseline.avg_value,
                    baseline_stddev: baseline.stddev_value,
                    anomaly_type: AnomalyType::Sustained,
                    detected_at: chrono::Utc::now(),
                });
            }

            // Check for gradual drift using linear regression
            if recent.len() >= 10 {
                let (slope, r_squared) = linear_regression(&recent);
                if r_squared > self.config.drift_r_squared_threshold && slope.abs() > 0.01 {
                    let latest = recent.last().unwrap();
                    let z_score =
                        (latest.avg_value - baseline.avg_value).abs() / baseline.stddev_value;

                    anomalies.push(DetectedAnomaly {
                        project_id,
                        service_name: String::new(),
                        environment: None,
                        metric_name: metric_name.clone(),
                        z_score,
                        current_value: latest.avg_value,
                        baseline_avg: baseline.avg_value,
                        baseline_stddev: baseline.stddev_value,
                        anomaly_type: AnomalyType::Drift,
                        detected_at: chrono::Utc::now(),
                    });
                }
            }
        }

        Ok(anomalies)
    }
}

/// Simple linear regression returning (slope, R²).
fn linear_regression(points: &[crate::storage::MinuteAggregate]) -> (f64, f64) {
    let n = points.len() as f64;
    if n < 2.0 {
        return (0.0, 0.0);
    }

    let mut sum_x = 0.0;
    let mut sum_y = 0.0;
    let mut sum_xy = 0.0;
    let mut sum_x2 = 0.0;
    let mut sum_y2 = 0.0;

    for (i, p) in points.iter().enumerate() {
        let x = i as f64;
        let y = p.avg_value;
        sum_x += x;
        sum_y += y;
        sum_xy += x * y;
        sum_x2 += x * x;
        sum_y2 += y * y;
    }

    let denominator = n * sum_x2 - sum_x * sum_x;
    if denominator == 0.0 {
        return (0.0, 0.0);
    }

    let slope = (n * sum_xy - sum_x * sum_y) / denominator;

    // R² calculation
    let y_mean = sum_y / n;
    let ss_tot = sum_y2 - n * y_mean * y_mean;
    if ss_tot == 0.0 {
        return (slope, 1.0);
    }

    let ss_res: f64 = points
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let predicted = (slope * i as f64) + (sum_y - slope * sum_x) / n;
            (p.avg_value - predicted).powi(2)
        })
        .sum();

    let r_squared = 1.0 - (ss_res / ss_tot);

    (slope, r_squared.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MinuteAggregate;
    use chrono::Utc;

    #[test]
    fn test_linear_regression_perfect_line() {
        let points: Vec<MinuteAggregate> = (0..10)
            .map(|i| MinuteAggregate {
                bucket: Utc::now(),
                avg_value: i as f64 * 2.0 + 1.0,
                count: 100,
            })
            .collect();

        let (slope, r_squared) = linear_regression(&points);
        assert!((slope - 2.0).abs() < 0.01);
        assert!((r_squared - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_linear_regression_flat_line() {
        let points: Vec<MinuteAggregate> = (0..10)
            .map(|_| MinuteAggregate {
                bucket: Utc::now(),
                avg_value: 5.0,
                count: 100,
            })
            .collect();

        let (slope, r_squared) = linear_regression(&points);
        assert!(slope.abs() < 0.01);
        // R² is 1.0 for a flat line (no variance, perfect "fit")
        assert!((r_squared - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_linear_regression_empty() {
        let points: Vec<MinuteAggregate> = Vec::new();
        let (slope, r_squared) = linear_regression(&points);
        assert_eq!(slope, 0.0);
        assert_eq!(r_squared, 0.0);
    }
}
