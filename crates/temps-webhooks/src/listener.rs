// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Webhook event listener that subscribes to deployment events from the job queue.

use crate::events::{
    BackupPayload, DeploymentPayload, WebhookEvent, WebhookEventType, WebhookPayload,
};
use crate::service::WebhookService;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use std::sync::Arc;
use temps_core::{Job, JobQueue};
use temps_entities::{deployments, external_service_backups, project_services, projects};
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

/// Project resolved for a backup's underlying service. Unlike a deployment,
/// which belongs to exactly one project, a backup is scoped to a service
/// and schedule: `external_service_backups` links the backup to a service,
/// and `project_services` links that service to project(s). Since webhooks
/// are strictly project-scoped (`webhooks.project_id`), a backup whose
/// service maps to zero or more than one project has nowhere unambiguous
/// to deliver to, so [`WebhookEventListener::fetch_backup_context`] returns
/// `None` in both cases and the webhook is skipped, same as "no webhooks
/// configured for this project".
struct BackupContext {
    project_id: i32,
    project_name: String,
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

    /// Resolve the single project a backup's underlying service belongs to,
    /// for enriching and scoping backup webhook payloads. Returns `None`
    /// (and logs why) when the backup isn't linked to a service, or the
    /// service isn't linked to exactly one project -- see [`BackupContext`].
    async fn fetch_backup_context(
        db: &DatabaseConnection,
        backup_id: i32,
    ) -> Option<BackupContext> {
        // `.all()`, not `.one()`: `backup_id` carries no uniqueness constraint
        // on `external_service_backups`, and the authorization model for
        // reading a backup's own children already treats "producer services"
        // as a set that must resolve unambiguously (see
        // `require_services_access` in temps-backup). An unordered `LIMIT 1`
        // here would let an arbitrary producer's project win a race if a
        // backup ever gains a second child row; failing closed on more than
        // one distinct service matches that existing model instead.
        let service_backups = match external_service_backups::Entity::find()
            .filter(external_service_backups::Column::BackupId.eq(backup_id))
            .all(db)
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                warn!(
                    "Failed to fetch external_service_backups for backup {}: {}",
                    backup_id, e
                );
                return None;
            }
        };
        let service_ids: std::collections::BTreeSet<i32> =
            service_backups.iter().map(|sb| sb.service_id).collect();
        let service_id = match service_ids.len() {
            0 => {
                debug!(
                    "Backup {} has no external_service_backups row; skipping backup webhook",
                    backup_id
                );
                return None;
            }
            1 => *service_ids.iter().next().expect("checked len == 1 above"),
            count => {
                debug!(
                    "Backup {} has {} distinct producer services; webhook project resolution \
                     requires exactly one, so skipping",
                    backup_id, count
                );
                return None;
            }
        };

        let project_links = match project_services::Entity::find()
            .filter(project_services::Column::ServiceId.eq(service_id))
            .all(db)
            .await
        {
            Ok(links) => links,
            Err(e) => {
                warn!(
                    "Failed to fetch project_services for service {} (backup {}): {}",
                    service_id, backup_id, e
                );
                return None;
            }
        };

        let project_id = match project_links.as_slice() {
            [link] => link.project_id,
            [] => {
                debug!(
                    "Service {} (backup {}) is not linked to any project; skipping backup webhook",
                    service_id, backup_id
                );
                return None;
            }
            links => {
                debug!(
                    "Service {} (backup {}) is linked to {} projects; webhooks are project-scoped so skipping",
                    service_id, backup_id, links.len()
                );
                return None;
            }
        };

        let project_name = match projects::Entity::find_by_id(project_id).one(db).await {
            Ok(Some(p)) => p.name,
            _ => String::new(),
        };

        Some(BackupContext {
            project_id,
            project_name,
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
            Job::BackupStarted(event) => {
                debug!(
                    "Processing BackupStarted event for backup {}",
                    event.backup_id
                );
                Self::trigger_backup_webhook(
                    webhook_service,
                    db,
                    WebhookEventType::BackupStarted,
                    event.backup_id,
                    event.engine.clone(),
                    "started".to_string(),
                    None,
                    None,
                    None,
                    Some(chrono::Utc::now()),
                    None,
                )
                .await?;
            }
            Job::BackupCompleted(event) => {
                debug!(
                    "Processing BackupCompleted event for backup {}",
                    event.backup_id
                );
                Self::trigger_backup_webhook(
                    webhook_service,
                    db,
                    WebhookEventType::BackupCompleted,
                    event.backup_id,
                    event.engine.clone(),
                    "completed".to_string(),
                    Some(event.s3_location.clone()),
                    event.size_bytes,
                    None,
                    None,
                    Some(chrono::Utc::now()),
                )
                .await?;
            }
            Job::BackupFailed(event) => {
                debug!(
                    "Processing BackupFailed event for backup {}",
                    event.backup_id
                );
                Self::trigger_backup_webhook(
                    webhook_service,
                    db,
                    WebhookEventType::BackupFailed,
                    event.backup_id,
                    event.engine.clone(),
                    "failed".to_string(),
                    None,
                    None,
                    Some(bound_error_message(&event.error_message)),
                    None,
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

    /// Trigger a webhook for a backup lifecycle event. Unlike deployments,
    /// a backup has no direct project association, so this resolves one via
    /// [`Self::fetch_backup_context`] first and skips delivery entirely
    /// (not an error) when that resolution is ambiguous.
    #[allow(clippy::too_many_arguments)]
    async fn trigger_backup_webhook(
        webhook_service: &WebhookService,
        db: &DatabaseConnection,
        event_type: WebhookEventType,
        backup_id: i32,
        engine: String,
        status: String,
        s3_location: Option<String>,
        size_bytes: Option<i64>,
        error_message: Option<String>,
        started_at: Option<chrono::DateTime<chrono::Utc>>,
        finished_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let Some(ctx) = Self::fetch_backup_context(db, backup_id).await else {
            debug!(
                "Skipping {} webhook for backup {}: no single project resolved",
                event_type, backup_id
            );
            return Ok(());
        };

        let payload = WebhookPayload::Backup(BackupPayload {
            backup_id,
            engine,
            project_id: Some(ctx.project_id),
            project_name: Some(ctx.project_name.clone()),
            status,
            s3_location,
            size_bytes,
            error_message,
            started_at,
            finished_at,
        });

        let webhook_event = WebhookEvent::new(event_type, Some(ctx.project_id), payload);

        debug!(
            "📤 Triggering webhooks for event: {:?}",
            webhook_event.event_type
        );

        match webhook_service.trigger_event(webhook_event).await {
            Ok(results) => {
                let success_count = results.iter().filter(|r| r.success).count();
                let total_count = results.len();

                if total_count > 0 {
                    info!(
                        "✅ Triggered {} webhooks for backup {} (project {}), {} succeeded",
                        total_count, backup_id, ctx.project_id, success_count
                    );
                }
                Ok(())
            }
            Err(e) => {
                error!(
                    "❌ Failed to trigger webhooks for backup {}: {}",
                    backup_id, e
                );
                Err(Box::new(e))
            }
        }
    }
}

/// Failure reasons on this path come from raw engine stderr, which is not
/// scrubbed of credentials at every call site (`s3_mirror`'s own reason
/// string is the one place that already had to special-case this). Webhook
/// URLs are operator-configured and this is the first path that ships that
/// text to one, so bound it defensively -- this caps exposure, it does not
/// replace fixing redaction at the source.
const MAX_ERROR_MESSAGE_LEN: usize = 500;

fn bound_error_message(message: &str) -> String {
    if message.len() <= MAX_ERROR_MESSAGE_LEN {
        return message.to_string();
    }
    let mut truncated: String = message.chars().take(MAX_ERROR_MESSAGE_LEN).collect();
    truncated.push_str(" [truncated]");
    truncated
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
