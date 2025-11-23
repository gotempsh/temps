//! Webhook event listener that subscribes to deployment events from the job queue.

use crate::events::{DeploymentPayload, WebhookEvent, WebhookEventType, WebhookPayload};
use crate::service::WebhookService;
use std::sync::Arc;
use temps_core::{Job, JobQueue};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{debug, error, info};

/// Webhook event listener that processes deployment lifecycle events
pub struct WebhookEventListener {
    webhook_service: Arc<WebhookService>,
    queue: Arc<dyn JobQueue>,
    running: Arc<RwLock<bool>>,
    task_handle: Arc<RwLock<Option<JoinHandle<()>>>>,
}

impl WebhookEventListener {
    /// Create a new webhook event listener
    pub fn new(webhook_service: Arc<WebhookService>, queue: Arc<dyn JobQueue>) -> Self {
        Self {
            webhook_service,
            queue,
            running: Arc::new(RwLock::new(false)),
            task_handle: Arc::new(RwLock::new(None)),
        }
    }

    /// Start listening to deployment events from the queue
    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut running = self.running.write().await;
        if *running {
            info!("✅ Webhook event listener already running");
            return Ok(()); // Already running
        }
        *running = true;
        drop(running);

        info!("🚀 Starting webhook event listener");

        // Subscribe to deployment events
        let mut receiver = self.queue.subscribe();
        let webhook_service = self.webhook_service.clone();
        let running = self.running.clone();

        // Spawn background task to process jobs
        let handle = tokio::spawn(async move {
            info!("✅ Webhook listener task started and listening for events");
            let mut event_count = 0;
            while *running.read().await {
                match receiver.recv().await {
                    Ok(job) => {
                        event_count += 1;
                        debug!("📨 Received job #{} from queue: {}", event_count, job);
                        if let Err(e) = Self::process_job(&webhook_service, &job).await {
                            error!("❌ Failed to process job #{}: {}", event_count, e);
                        }
                    }
                    Err(e) => {
                        error!("⚠️ Failed to receive job from queue: {}", e);
                        // Continue loop to keep trying
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    }
                }
            }
            info!(
                "🛑 Webhook event listener task stopped after processing {} events",
                event_count
            );
        });

        *self.task_handle.write().await = Some(handle);

        info!("✅ Webhook event listener started successfully");
        Ok(())
    }

    /// Stop the event listener
    pub async fn stop(&self) {
        let mut running = self.running.write().await;
        *running = false;
        drop(running);

        // Wait for task to complete
        if let Some(handle) = self.task_handle.write().await.take() {
            let _ = handle.await;
        }

        info!("Stopped webhook event listener");
    }

    /// Check if the listener is running
    pub async fn is_running(&self) -> bool {
        *self.running.read().await
    }

    /// Process a single job
    async fn process_job(
        webhook_service: &WebhookService,
        job: &Job,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        match job {
            Job::DeploymentCreated(event) => {
                debug!(
                    "Processing DeploymentCreated event for deployment {}",
                    event.deployment_id
                );
                Self::trigger_webhook(
                    webhook_service,
                    WebhookEventType::DeploymentCreated,
                    event.project_id,
                    event.deployment_id,
                    event.environment_name.clone(),
                    event.branch.clone(),
                    event.commit_sha.clone(),
                    None, // No URL yet
                    "created".to_string(),
                    None, // No error
                    None, // Not started yet
                    None, // Not finished yet
                )
                .await?;
            }
            Job::DeploymentSucceeded(event) => {
                debug!(
                    "Processing DeploymentSucceeded event for deployment {}",
                    event.deployment_id
                );
                Self::trigger_webhook(
                    webhook_service,
                    WebhookEventType::DeploymentSucceeded,
                    event.project_id,
                    event.deployment_id,
                    event.environment_name.clone(),
                    None, // Branch not in succeeded event
                    event.commit_sha.clone(),
                    event.url.clone(),
                    "succeeded".to_string(),
                    None, // No error
                    None, // TODO: Get started_at from database
                    Some(chrono::Utc::now()),
                )
                .await?;
            }
            Job::DeploymentFailed(event) => {
                debug!(
                    "Processing DeploymentFailed event for deployment {}",
                    event.deployment_id
                );
                Self::trigger_webhook(
                    webhook_service,
                    WebhookEventType::DeploymentFailed,
                    event.project_id,
                    event.deployment_id,
                    event.environment_name.clone(),
                    None, // Branch not in failed event
                    None, // Commit not in failed event
                    None, // No URL on failure
                    "failed".to_string(),
                    event.error_message.clone(),
                    None, // TODO: Get started_at from database
                    Some(chrono::Utc::now()),
                )
                .await?;
            }
            Job::DeploymentCancelled(event) => {
                debug!(
                    "Processing DeploymentCancelled event for deployment {}",
                    event.deployment_id
                );
                Self::trigger_webhook(
                    webhook_service,
                    WebhookEventType::DeploymentCancelled,
                    event.project_id,
                    event.deployment_id,
                    event.environment_name.clone(),
                    None, // Branch not in cancelled event
                    None, // Commit not in cancelled event
                    None, // No URL
                    "cancelled".to_string(),
                    None, // No error
                    None, // TODO: Get started_at from database
                    Some(chrono::Utc::now()),
                )
                .await?;
            }
            Job::DeploymentReady(event) => {
                debug!(
                    "Processing DeploymentReady event for deployment {}",
                    event.deployment_id
                );
                Self::trigger_webhook(
                    webhook_service,
                    WebhookEventType::DeploymentReady,
                    event.project_id,
                    event.deployment_id,
                    event.environment_name.clone(),
                    None, // Branch not in ready event
                    None, // Commit not in ready event
                    event.url.clone(),
                    "ready".to_string(),
                    None, // No error
                    None, // TODO: Get started_at from database
                    Some(chrono::Utc::now()),
                )
                .await?;
            }
            _ => {
                // Ignore other job types
                return Ok(());
            }
        }

        Ok(())
    }

    /// Trigger a webhook for a deployment event
    #[allow(clippy::too_many_arguments)]
    async fn trigger_webhook(
        webhook_service: &WebhookService,
        event_type: WebhookEventType,
        project_id: i32,
        deployment_id: i32,
        environment_name: String,
        branch: Option<String>,
        commit_sha: Option<String>,
        url: Option<String>,
        status: String,
        error_message: Option<String>,
        started_at: Option<chrono::DateTime<chrono::Utc>>,
        finished_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        debug!(
            "🔗 Creating webhook payload for deployment {} (project {}, event: {:?})",
            deployment_id, project_id, event_type
        );

        let payload = WebhookPayload::Deployment(DeploymentPayload {
            deployment_id,
            project_id,
            project_name: String::new(), // TODO: Fetch from database
            environment: environment_name.clone(),
            branch: branch.clone(),
            commit_sha: commit_sha.clone(),
            commit_message: None, // TODO: Fetch from git provider or database
            url: url.clone(),
            status: status.clone(),
            error_message: error_message.clone(),
            started_at,
            finished_at,
        });

        let webhook_event = WebhookEvent::new(event_type, Some(project_id), payload);

        debug!(
            "📤 Triggering webhooks for event: {:?}",
            webhook_event.event_type
        );

        match webhook_service.trigger_event(webhook_event).await {
            Ok(results) => {
                let success_count = results.iter().filter(|r| r.success).count();
                let total_count = results.len();

                if total_count == 0 {
                    debug!(
                        "⚠️ No webhooks found for project {} (may not have any configured)",
                        project_id
                    );
                } else {
                    info!(
                        "✅ Triggered {} webhooks for deployment {} (project {}), {} succeeded",
                        total_count, deployment_id, project_id, success_count
                    );
                    for result in &results {
                        if result.success {
                            info!(
                                "  ✓ Webhook {} delivered successfully (status: {})",
                                result.webhook_id,
                                result.status_code.unwrap_or(0)
                            );
                        } else {
                            error!(
                                "  ✗ Webhook {} delivery failed: {}",
                                result.webhook_id,
                                result.error_message.as_deref().unwrap_or("unknown error")
                            );
                        }
                    }
                }
                Ok(())
            }
            Err(e) => {
                error!(
                    "❌ Failed to trigger webhooks for deployment {}: {}",
                    deployment_id, e
                );
                Err(Box::new(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_listener_lifecycle() {
        // Create mock services
        let db = Arc::new(sea_orm::Database::connect("sqlite::memory:").await.unwrap());
        let encryption_service = Arc::new(
            temps_core::EncryptionService::new(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
        );
        let webhook_service = Arc::new(WebhookService::new(db.clone(), encryption_service));
        let queue = Arc::new(temps_queue::BroadcastQueueService::new(100)) as Arc<dyn JobQueue>;

        let listener = WebhookEventListener::new(webhook_service, queue);

        // Test initial state
        assert!(!listener.is_running().await);

        // Start listener
        listener.start().await.unwrap();
        assert!(listener.is_running().await);

        // Stop listener
        listener.stop().await;
        assert!(!listener.is_running().await);
    }
}
