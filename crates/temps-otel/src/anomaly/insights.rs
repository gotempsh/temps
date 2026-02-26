//! Insight generation from correlated anomalies.
//!
//! Correlates co-occurring anomalies on the same service within a 5-minute
//! window into a single insight, and correlates with deploy events.

use std::sync::Arc;
use tracing::info;

use super::detector::{AnomalyType, DetectedAnomaly};
use crate::storage::OtelStorage;
use crate::types::*;

/// Generates insights from detected anomalies.
pub struct InsightGenerator {
    storage: Arc<dyn OtelStorage>,
}

impl InsightGenerator {
    pub fn new(storage: Arc<dyn OtelStorage>) -> Self {
        Self { storage }
    }

    /// Process detected anomalies into insights.
    ///
    /// 1. Correlates co-occurring anomalies on the same service.
    /// 2. Correlates with recent deploy events.
    /// 3. Generates human-readable descriptions.
    /// 4. Stores the insights.
    pub async fn process_anomalies(
        &self,
        anomalies: Vec<DetectedAnomaly>,
    ) -> Result<Vec<Insight>, crate::error::OtelError> {
        if anomalies.is_empty() {
            return Ok(Vec::new());
        }

        let project_id = anomalies[0].project_id;

        // Get recent deploys for correlation
        let deploys = self.storage.get_recent_deploys(project_id, 30).await?;

        // Group anomalies by service for correlation
        let mut by_service: std::collections::HashMap<String, Vec<&DetectedAnomaly>> =
            std::collections::HashMap::new();
        for anomaly in &anomalies {
            by_service
                .entry(anomaly.service_name.clone())
                .or_default()
                .push(anomaly);
        }

        let mut insights = Vec::new();

        for (service_name, service_anomalies) in &by_service {
            // Determine severity based on Z-score and count
            let max_z = service_anomalies
                .iter()
                .map(|a| a.z_score)
                .fold(0.0f64, f64::max);

            let severity = if max_z > 6.0 || service_anomalies.len() > 3 {
                InsightSeverity::Critical
            } else if max_z > 4.0 || service_anomalies.len() > 2 {
                InsightSeverity::High
            } else if max_z > 3.0 {
                InsightSeverity::Medium
            } else {
                InsightSeverity::Low
            };

            // Correlate with deploy events
            let correlated_deploy = deploys.first().map(|d| d.deployment_id);

            // Generate human-readable description
            let title = generate_title(service_anomalies);
            let description = generate_description(service_anomalies, deploys.first());

            let metric_name = if service_anomalies.len() == 1 {
                Some(service_anomalies[0].metric_name.clone())
            } else {
                None // Multiple metrics involved
            };

            let now = chrono::Utc::now();
            let insight = Insight {
                id: 0, // Set by DB
                project_id,
                environment: service_anomalies
                    .first()
                    .and_then(|a| a.environment.clone()),
                service_name: service_name.clone(),
                severity,
                status: InsightStatus::Active,
                title,
                description,
                metric_name,
                correlated_deploy_id: correlated_deploy,
                anomaly_ids: Vec::new(), // IDs are set after storage
                started_at: service_anomalies
                    .iter()
                    .map(|a| a.detected_at)
                    .min()
                    .unwrap_or(now),
                resolved_at: None,
                created_at: now,
                updated_at: now,
            };

            let id = self.storage.upsert_insight(&insight).await?;
            let mut stored = insight;
            stored.id = id;
            insights.push(stored);
        }

        if !insights.is_empty() {
            info!(
                project_id,
                count = insights.len(),
                "Generated insights from anomalies"
            );
        }

        Ok(insights)
    }
}

fn generate_title(anomalies: &[&DetectedAnomaly]) -> String {
    if anomalies.len() == 1 {
        let a = anomalies[0];
        match a.anomaly_type {
            AnomalyType::Sustained => {
                format!("{} is anomalous (Z-score: {:.1})", a.metric_name, a.z_score)
            }
            AnomalyType::Drift => {
                format!("{} shows gradual drift", a.metric_name)
            }
        }
    } else {
        format!("{} correlated anomalies detected", anomalies.len())
    }
}

fn generate_description(
    anomalies: &[&DetectedAnomaly],
    deploy: Option<&crate::storage::DeployEvent>,
) -> String {
    let mut desc = String::new();

    for a in anomalies {
        let direction = if a.current_value > a.baseline_avg {
            "above"
        } else {
            "below"
        };

        let change_pct = if a.baseline_avg != 0.0 {
            ((a.current_value - a.baseline_avg) / a.baseline_avg * 100.0).abs()
        } else {
            0.0
        };

        desc.push_str(&format!(
            "- **{}** is {:.1}% {} baseline (current: {:.2}, baseline: {:.2} ± {:.2}, Z-score: {:.1})\n",
            a.metric_name, change_pct, direction,
            a.current_value, a.baseline_avg, a.baseline_stddev, a.z_score
        ));

        if a.anomaly_type == AnomalyType::Drift {
            desc.push_str("  *Pattern: gradual drift detected via linear regression*\n");
        }
    }

    if let Some(deploy) = deploy {
        desc.push_str(&format!(
            "\n**Possible cause:** Deployment #{} completed {} ago",
            deploy.deployment_id,
            humanize_duration(chrono::Utc::now() - deploy.deployed_at)
        ));
    }

    desc
}

fn humanize_duration(duration: chrono::Duration) -> String {
    let minutes = duration.num_minutes();
    if minutes < 1 {
        "just now".to_string()
    } else if minutes < 60 {
        format!("{}m", minutes)
    } else {
        format!("{}h {}m", minutes / 60, minutes % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anomaly::detector::AnomalyType;

    #[test]
    fn test_generate_title_single_sustained() {
        let anomaly = DetectedAnomaly {
            project_id: 1,
            service_name: "web".into(),
            environment: None,
            metric_name: "http_request_duration_seconds".into(),
            z_score: 4.5,
            current_value: 2.0,
            baseline_avg: 0.5,
            baseline_stddev: 0.1,
            anomaly_type: AnomalyType::Sustained,
            detected_at: chrono::Utc::now(),
        };

        let title = generate_title(&[&anomaly]);
        assert!(title.contains("http_request_duration_seconds"));
        assert!(title.contains("4.5"));
    }

    #[test]
    fn test_generate_title_multiple() {
        let a1 = DetectedAnomaly {
            project_id: 1,
            service_name: "web".into(),
            environment: None,
            metric_name: "cpu".into(),
            z_score: 3.0,
            current_value: 0.9,
            baseline_avg: 0.5,
            baseline_stddev: 0.1,
            anomaly_type: AnomalyType::Sustained,
            detected_at: chrono::Utc::now(),
        };
        let a2 = DetectedAnomaly {
            metric_name: "memory".into(),
            ..a1.clone()
        };

        let title = generate_title(&[&a1, &a2]);
        assert!(title.contains("2 correlated"));
    }

    #[test]
    fn test_humanize_duration() {
        assert_eq!(humanize_duration(chrono::Duration::seconds(30)), "just now");
        assert_eq!(humanize_duration(chrono::Duration::minutes(5)), "5m");
        assert_eq!(humanize_duration(chrono::Duration::minutes(90)), "1h 30m");
    }
}
