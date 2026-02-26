//! Webhook event listener that subscribes to deployment events from the job queue.

use crate::events::{DeploymentPayload, WebhookEvent, WebhookEventType, WebhookPayload};
use crate::service::WebhookService;
use sea_orm::{DatabaseConnection, EntityTrait};
use std::sync::Arc;
use temps_core::{Job, JobQueue};
use temps_entities::{deployments, projects};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

/// Deployment context data fetched from the database to enrich webhook payloads.
struct DeploymentContext {
    project_name: String,
    branch: Option<String>,
    commit_sha: Option<String>,
    commit_message: Option<String>,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Webhook event listener that processes deployment lifecycle events
pub struct WebhookEventListener {
    webhook_service: Arc<WebhookService>,
    db: Arc<DatabaseConnection>,
    queue: Arc<dyn JobQueue>,
    running: Arc<RwLock<bool>>,
    task_handle: Arc<RwLock<Option<JoinHandle<()>>>>,
}

impl WebhookEventListener {
    /// Create a new webhook event listener
    pub fn new(
        webhook_service: Arc<WebhookService>,
        db: Arc<DatabaseConnection>,
        queue: Arc<dyn JobQueue>,
    ) -> Self {
        Self {
            webhook_service,
            db,
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
        let db = self.db.clone();
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
                        if let Err(e) = Self::process_job(&webhook_service, &db, &job).await {
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

    /// Fetch deployment context from the database to enrich webhook payloads.
    async fn fetch_deployment_context(
        db: &DatabaseConnection,
        deployment_id: i32,
        project_id: i32,
    ) -> Option<DeploymentContext> {
        // Fetch deployment record
        let deployment = match deployments::Entity::find_by_id(deployment_id).one(db).await {
            Ok(Some(d)) => d,
            Ok(None) => {
                warn!(
                    "Deployment {} not found in database for webhook enrichment",
                    deployment_id
                );
                return None;
            }
            Err(e) => {
                warn!(
                    "Failed to fetch deployment {} for webhook enrichment: {}",
                    deployment_id, e
                );
                return None;
            }
        };

        // Fetch project name
        let project_name = match projects::Entity::find_by_id(project_id).one(db).await {
            Ok(Some(p)) => p.name,
            _ => String::new(),
        };

        Some(DeploymentContext {
            project_name,
            branch: deployment.branch_ref.clone(),
            commit_sha: deployment.commit_sha.clone(),
            commit_message: deployment.commit_message.clone(),
            started_at: deployment.started_at,
        })
    }

    /// Process a single job
    async fn process_job(
        webhook_service: &WebhookService,
        db: &DatabaseConnection,
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
                    db,
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
                let ctx =
                    Self::fetch_deployment_context(db, event.deployment_id, event.project_id).await;
                Self::trigger_webhook(
                    webhook_service,
                    db,
                    WebhookEventType::DeploymentSucceeded,
                    event.project_id,
                    event.deployment_id,
                    event.environment_name.clone(),
                    ctx.as_ref().and_then(|c| c.branch.clone()),
                    event
                        .commit_sha
                        .clone()
                        .or_else(|| ctx.as_ref().and_then(|c| c.commit_sha.clone())),
                    event.url.clone(),
                    "succeeded".to_string(),
                    None,
                    ctx.as_ref().and_then(|c| c.started_at),
                    Some(chrono::Utc::now()),
                )
                .await?;
            }
            Job::DeploymentFailed(event) => {
                debug!(
                    "Processing DeploymentFailed event for deployment {}",
                    event.deployment_id
                );
                let ctx =
                    Self::fetch_deployment_context(db, event.deployment_id, event.project_id).await;
                Self::trigger_webhook(
                    webhook_service,
                    db,
                    WebhookEventType::DeploymentFailed,
                    event.project_id,
                    event.deployment_id,
                    event.environment_name.clone(),
                    ctx.as_ref().and_then(|c| c.branch.clone()),
                    ctx.as_ref().and_then(|c| c.commit_sha.clone()),
                    None, // No URL on failure
                    "failed".to_string(),
                    event.error_message.clone(),
                    ctx.as_ref().and_then(|c| c.started_at),
                    Some(chrono::Utc::now()),
                )
                .await?;
            }
            Job::DeploymentCancelled(event) => {
                debug!(
                    "Processing DeploymentCancelled event for deployment {}",
                    event.deployment_id
                );
                let ctx =
                    Self::fetch_deployment_context(db, event.deployment_id, event.project_id).await;
                Self::trigger_webhook(
                    webhook_service,
                    db,
                    WebhookEventType::DeploymentCancelled,
                    event.project_id,
                    event.deployment_id,
                    event.environment_name.clone(),
                    ctx.as_ref().and_then(|c| c.branch.clone()),
                    ctx.as_ref().and_then(|c| c.commit_sha.clone()),
                    None,
                    "cancelled".to_string(),
                    None,
                    ctx.as_ref().and_then(|c| c.started_at),
                    Some(chrono::Utc::now()),
                )
                .await?;
            }
            Job::DeploymentReady(event) => {
                debug!(
                    "Processing DeploymentReady event for deployment {}",
                    event.deployment_id
                );
                let ctx =
                    Self::fetch_deployment_context(db, event.deployment_id, event.project_id).await;
                Self::trigger_webhook(
                    webhook_service,
                    db,
                    WebhookEventType::DeploymentReady,
                    event.project_id,
                    event.deployment_id,
                    event.environment_name.clone(),
                    ctx.as_ref().and_then(|c| c.branch.clone()),
                    ctx.as_ref().and_then(|c| c.commit_sha.clone()),
                    event.url.clone(),
                    "ready".to_string(),
                    None,
                    ctx.as_ref().and_then(|c| c.started_at),
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
        db: &DatabaseConnection,
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

        // Fetch deployment context for enrichment (project_name, commit_message)
        let ctx = Self::fetch_deployment_context(db, deployment_id, project_id).await;

        let payload = WebhookPayload::Deployment(DeploymentPayload {
            deployment_id,
            project_id,
            project_name: ctx
                .as_ref()
                .map(|c| c.project_name.clone())
                .unwrap_or_default(),
            environment: environment_name.clone(),
            branch: branch.clone(),
            commit_sha: commit_sha.clone(),
            commit_message: ctx.as_ref().and_then(|c| c.commit_message.clone()),
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

impl Drop for WebhookEventListener {
    fn drop(&mut self) {
        // Abort the background task if it's still running.
        // We can't call the async stop() from Drop, but we can abort the handle
        // which will cause the spawned task to be cancelled immediately.
        // try_write() is synchronous and won't block — it fails if the lock is held.
        match self.task_handle.try_write() {
            Ok(mut guard) => {
                if let Some(handle) = guard.take() {
                    handle.abort();
                }
            }
            Err(_) => {
                // Lock is held — this is rare during Drop. The task will be cleaned
                // up when the Arc<RwLock> is fully dropped and the JoinHandle is dropped.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a test listener with mock services
    async fn create_test_listener() -> WebhookEventListener {
        let db = Arc::new(sea_orm::Database::connect("sqlite::memory:").await.unwrap());
        let encryption_service = Arc::new(
            temps_core::EncryptionService::new(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
        );
        let webhook_service = Arc::new(WebhookService::new(db.clone(), encryption_service));
        let (queue_service, _receiver) =
            temps_queue::BroadcastQueueService::create_broadcast_channel(100);
        let queue = Arc::new(queue_service) as Arc<dyn JobQueue>;

        WebhookEventListener::new(webhook_service, db.clone(), queue)
    }

    #[tokio::test]
    async fn test_listener_lifecycle() {
        let listener = create_test_listener().await;

        // Test initial state
        assert!(!listener.is_running().await);

        // Start listener
        listener.start().await.unwrap();
        assert!(listener.is_running().await);

        // Stop listener
        listener.stop().await;
        assert!(!listener.is_running().await);
    }

    #[tokio::test]
    async fn test_listener_drop_when_not_started() {
        let listener = create_test_listener().await;

        // Dropping an unstarted listener should not panic
        drop(listener);
    }

    #[tokio::test]
    async fn test_listener_drop_aborts_running_task() {
        let listener = create_test_listener().await;

        listener.start().await.unwrap();
        assert!(listener.is_running().await);

        // Capture the abort handle before dropping
        let handle = {
            let guard = listener.task_handle.read().await;
            guard.as_ref().unwrap().abort_handle()
        };

        // Drop the listener — Drop impl should abort the background task
        drop(listener);

        // Give tokio a tick to process the abort
        tokio::task::yield_now().await;

        assert!(
            handle.is_finished(),
            "Background task should be aborted after Drop"
        );
    }

    #[tokio::test]
    async fn test_listener_double_start_is_noop() {
        let listener = create_test_listener().await;

        listener.start().await.unwrap();
        assert!(listener.is_running().await);

        // Starting again should succeed without error
        listener.start().await.unwrap();
        assert!(listener.is_running().await);

        listener.stop().await;
    }

    #[tokio::test]
    async fn test_listener_stop_when_not_started_is_safe() {
        let listener = create_test_listener().await;

        // Stopping an unstarted listener should not panic
        listener.stop().await;
        assert!(!listener.is_running().await);
    }
}
