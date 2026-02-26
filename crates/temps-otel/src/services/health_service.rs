//! Pre-computed health summary service.
//!
//! Runs every 60 seconds to compute per-environment health summaries
//! from OTel metrics and traces.

use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error};

use crate::storage::OtelStorage;
use crate::types::*;

/// Service that pre-computes health summaries for the monitoring page.
pub struct HealthComputeService {
    storage: Arc<dyn OtelStorage>,
}

impl HealthComputeService {
    pub fn new(storage: Arc<dyn OtelStorage>) -> Self {
        Self { storage }
    }

    /// Start the periodic health summary computation loop.
    pub async fn start(self: Arc<Self>, project_ids: Vec<i32>) {
        let mut interval = tokio::time::interval(Duration::from_secs(60));

        loop {
            interval.tick().await;

            for &project_id in &project_ids {
                if let Err(e) = self.compute_project_health(project_id).await {
                    error!(
                        project_id,
                        error = %e,
                        "Failed to compute health summary"
                    );
                }
            }
        }
    }

    async fn compute_project_health(&self, project_id: i32) -> Result<(), crate::error::OtelError> {
        // Get distinct service names from recent metrics
        let _metric_names = self.storage.list_metric_names(project_id).await?;

        // Get unique services from recent spans
        let recent_spans = self
            .storage
            .query_spans(TraceQuery {
                project_id,
                start_time: Some(chrono::Utc::now() - chrono::Duration::minutes(5)),
                limit: Some(1000),
                ..Default::default()
            })
            .await?;

        let mut services: std::collections::HashSet<String> = std::collections::HashSet::new();
        for span in &recent_spans {
            services.insert(span.resource.service_name.clone());
        }

        for service_name in &services {
            let summary = self
                .compute_service_health(project_id, service_name, &recent_spans)
                .await?;
            self.storage.store_health_summary(&summary).await?;
        }

        debug!(
            project_id,
            services = services.len(),
            "Computed health summaries"
        );
        Ok(())
    }

    async fn compute_service_health(
        &self,
        project_id: i32,
        service_name: &str,
        recent_spans: &[SpanRecord],
    ) -> Result<HealthSummary, crate::error::OtelError> {
        let service_spans: Vec<&SpanRecord> = recent_spans
            .iter()
            .filter(|s| s.resource.service_name == service_name)
            .collect();

        let total_spans = service_spans.len() as f64;
        let error_spans = service_spans
            .iter()
            .filter(|s| s.status_code == SpanStatusCode::Error)
            .count() as f64;

        let error_rate = if total_spans > 0.0 {
            error_spans / total_spans
        } else {
            0.0
        };

        // Compute P95 latency
        let p95 = self
            .storage
            .get_p95_latency(project_id, service_name, 5)
            .await
            .unwrap_or(0.0);

        // Determine health status
        let status = if error_rate > 0.5 || total_spans == 0.0 {
            HealthStatus::Down
        } else if error_rate > 0.05 || p95 > 5000.0 {
            HealthStatus::Degraded
        } else {
            HealthStatus::Healthy
        };

        let uptime_pct = if total_spans > 0.0 {
            ((total_spans - error_spans) / total_spans) * 100.0
        } else {
            0.0
        };

        // Get latest deploy from recent deploys
        let deploys = self.storage.get_recent_deploys(project_id, 60 * 24).await?;
        let last_deploy = deploys.first();

        Ok(HealthSummary {
            project_id,
            environment_id: None,
            service_name: service_name.to_string(),
            status,
            uptime_pct,
            error_rate,
            p95_latency_ms: p95,
            cpu_usage_pct: 0.0, // Populated from host metrics when available
            memory_usage_pct: 0.0,
            last_deploy_id: last_deploy.map(|d| d.deployment_id),
            last_deploy_at: last_deploy.map(|d| d.deployed_at),
            computed_at: chrono::Utc::now(),
        })
    }
}
