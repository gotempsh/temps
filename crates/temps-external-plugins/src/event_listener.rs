//! Plugin event listener — subscribes to the job queue and delivers
//! platform events to external plugins over their Unix domain sockets.
//!
//! This mirrors the [`WebhookEventListener`] pattern from `temps-webhooks`:
//! subscribe to the broadcast `JobQueue`, map `Job` variants to `PluginEvent`
//! structs, and POST them to each plugin's `/_events` endpoint.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use hyper::Request;
use hyper_util::rt::TokioIo;
use temps_core::external_plugin::{PluginEvent, PluginManifest, PLUGIN_EVENTS_PATH};
use temps_core::{Job, JobQueue};
use tokio::net::UnixStream;
use tokio::sync::{watch, RwLock};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::manager::ExternalPluginManager;

/// Listener that subscribes to the platform job queue and delivers events
/// to external plugins that declared event subscriptions in their manifest.
pub struct PluginEventListener {
    manager: Arc<ExternalPluginManager>,
    queue: Arc<dyn JobQueue>,
    /// Cancellation signal: send `true` to stop the listener loop immediately.
    stop_tx: watch::Sender<bool>,
    task_handle: Arc<RwLock<Option<JoinHandle<()>>>>,
}

/// A resolved plugin target: the info needed to deliver an event.
struct PluginTarget {
    name: String,
    socket_path: PathBuf,
    auth_secret: String,
}

impl PluginEventListener {
    /// Create a new plugin event listener.
    pub fn new(manager: Arc<ExternalPluginManager>, queue: Arc<dyn JobQueue>) -> Self {
        let (stop_tx, _) = watch::channel(false);
        Self {
            manager,
            queue,
            stop_tx,
            task_handle: Arc::new(RwLock::new(None)),
        }
    }

    /// Start listening to events from the queue.
    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // If already running (stop_tx has active receivers from a previous start), skip.
        if self.task_handle.read().await.is_some() {
            return Ok(());
        }

        info!("Starting plugin event listener");

        let mut receiver = self.queue.subscribe();
        let manager = self.manager.clone();
        let mut stop_rx = self.stop_tx.subscribe();

        let handle = tokio::spawn(async move {
            info!("Plugin event listener task started");
            let mut event_count: u64 = 0;
            loop {
                tokio::select! {
                    result = receiver.recv() => {
                        match result {
                            Ok(job) => {
                                if let Some(plugin_event) = Self::job_to_plugin_event(&job) {
                                    event_count += 1;
                                    debug!(
                                        event_id = %plugin_event.id,
                                        event_type = %plugin_event.event_type,
                                        "Delivering plugin event #{}", event_count
                                    );
                                    Self::deliver_to_plugins(&manager, &plugin_event).await;
                                }
                            }
                            Err(e) => {
                                error!("Failed to receive job from queue: {}", e);
                                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                            }
                        }
                    }
                    _ = stop_rx.changed() => {
                        // stop_tx sent a signal — exit the loop immediately.
                        break;
                    }
                }
            }
            info!(
                "Plugin event listener stopped after processing {} events",
                event_count
            );
        });

        *self.task_handle.write().await = Some(handle);
        info!("Plugin event listener started successfully");
        Ok(())
    }

    /// Stop the event listener.
    ///
    /// Signals the background task via a watch channel and awaits its
    /// completion. This returns immediately (no longer blocked waiting
    /// for the next job from the broadcast queue).
    pub async fn stop(&self) {
        // Signal the loop to break out of the select!
        let _ = self.stop_tx.send(true);

        if let Some(handle) = self.task_handle.write().await.take() {
            let _ = handle.await;
        }

        info!("Stopped plugin event listener");
    }

    /// Check if the listener is running.
    pub async fn is_running(&self) -> bool {
        self.task_handle.read().await.is_some()
    }

    /// Map a `Job` to a `PluginEvent`, returning `None` for jobs that
    /// don't correspond to plugin-deliverable events.
    fn job_to_plugin_event(job: &Job) -> Option<PluginEvent> {
        let now = chrono::Utc::now();
        let id = uuid::Uuid::new_v4().to_string();

        match job {
            Job::DeploymentCreated(e) => Some(PluginEvent {
                id,
                event_type: "deployment.created".to_string(),
                timestamp: now,
                project_id: Some(e.project_id),
                data: serde_json::json!({
                    "deployment_id": e.deployment_id,
                    "project_id": e.project_id,
                    "environment_id": e.environment_id,
                    "environment_name": e.environment_name,
                    "commit_sha": e.commit_sha,
                    "branch": e.branch,
                }),
            }),
            Job::DeploymentSucceeded(e) => Some(PluginEvent {
                id,
                event_type: "deployment.succeeded".to_string(),
                timestamp: now,
                project_id: Some(e.project_id),
                data: serde_json::json!({
                    "deployment_id": e.deployment_id,
                    "project_id": e.project_id,
                    "environment_id": e.environment_id,
                    "environment_name": e.environment_name,
                    "commit_sha": e.commit_sha,
                    "url": e.url,
                }),
            }),
            Job::DeploymentFailed(e) => Some(PluginEvent {
                id,
                event_type: "deployment.failed".to_string(),
                timestamp: now,
                project_id: Some(e.project_id),
                data: serde_json::json!({
                    "deployment_id": e.deployment_id,
                    "project_id": e.project_id,
                    "environment_id": e.environment_id,
                    "environment_name": e.environment_name,
                    "error_message": e.error_message,
                }),
            }),
            Job::DeploymentCancelled(e) => Some(PluginEvent {
                id,
                event_type: "deployment.cancelled".to_string(),
                timestamp: now,
                project_id: Some(e.project_id),
                data: serde_json::json!({
                    "deployment_id": e.deployment_id,
                    "project_id": e.project_id,
                    "environment_id": e.environment_id,
                    "environment_name": e.environment_name,
                }),
            }),
            Job::DeploymentReady(e) => Some(PluginEvent {
                id,
                event_type: "deployment.ready".to_string(),
                timestamp: now,
                project_id: Some(e.project_id),
                data: serde_json::json!({
                    "deployment_id": e.deployment_id,
                    "project_id": e.project_id,
                    "environment_id": e.environment_id,
                    "environment_name": e.environment_name,
                    "url": e.url,
                }),
            }),
            Job::ProjectCreated(e) => Some(PluginEvent {
                id,
                event_type: "project.created".to_string(),
                timestamp: now,
                project_id: Some(e.project_id),
                data: serde_json::json!({
                    "project_id": e.project_id,
                    "project_name": e.project_name,
                }),
            }),
            Job::ProjectDeleted(e) => Some(PluginEvent {
                id,
                event_type: "project.deleted".to_string(),
                timestamp: now,
                project_id: Some(e.project_id),
                data: serde_json::json!({
                    "project_id": e.project_id,
                    "project_name": e.project_name,
                }),
            }),
            Job::DomainCreated(e) => Some(PluginEvent {
                id,
                event_type: "domain.created".to_string(),
                timestamp: now,
                project_id: Some(e.project_id),
                data: serde_json::json!({
                    "domain_id": e.domain_id,
                    "project_id": e.project_id,
                    "domain_name": e.domain_name,
                }),
            }),
            Job::DomainProvisioned(e) => Some(PluginEvent {
                id,
                event_type: "domain.provisioned".to_string(),
                timestamp: now,
                project_id: Some(e.project_id),
                data: serde_json::json!({
                    "domain_id": e.domain_id,
                    "project_id": e.project_id,
                    "domain_name": e.domain_name,
                }),
            }),
            _ => None,
        }
    }

    /// Deliver an event to all plugins that subscribe to its event type.
    ///
    /// Prefers the WebSocket channel for delivery (lower latency, already
    /// connected).  Falls back to `POST /_events` over HTTP if the channel
    /// is unavailable or dead.
    async fn deliver_to_plugins(manager: &ExternalPluginManager, event: &PluginEvent) {
        let targets = Self::resolve_targets(manager, &event.event_type).await;

        if targets.is_empty() {
            debug!(
                event_type = %event.event_type,
                "No plugins subscribed to this event type"
            );
            return;
        }

        for target in &targets {
            // Try the WebSocket channel first (faster, already connected)
            if manager.send_event_via_channel(&target.name, event).await {
                debug!(
                    plugin = %target.name,
                    event_type = %event.event_type,
                    event_id = %event.id,
                    "Event delivered to plugin via channel"
                );
                continue;
            }

            // Fall back to HTTP POST /_events
            if let Err(e) =
                Self::deliver_event(&target.socket_path, &target.auth_secret, event).await
            {
                warn!(
                    plugin = %target.name,
                    event_type = %event.event_type,
                    event_id = %event.id,
                    "Failed to deliver event to plugin: {}", e
                );
            } else {
                debug!(
                    plugin = %target.name,
                    event_type = %event.event_type,
                    event_id = %event.id,
                    "Event delivered to plugin via HTTP fallback"
                );
            }
        }
    }

    /// Resolve which plugins subscribe to a given event type and gather
    /// their socket paths.
    async fn resolve_targets(
        manager: &ExternalPluginManager,
        event_type: &str,
    ) -> Vec<PluginTarget> {
        let manifests = manager.manifests().await;
        let mut targets = Vec::new();

        for manifest in manifests {
            if Self::manifest_subscribes_to(&manifest, event_type) {
                if let (Some(socket_path), Some(auth_secret)) = (
                    manager.socket_path_for(&manifest.name).await,
                    manager.auth_secret_for(&manifest.name).await,
                ) {
                    targets.push(PluginTarget {
                        name: manifest.name.clone(),
                        socket_path,
                        auth_secret,
                    });
                }
            }
        }

        targets
    }

    /// Check whether a plugin manifest subscribes to a given event type.
    ///
    /// Supports exact match (e.g., "deployment.succeeded") and wildcard
    /// prefixes (e.g., "deployment.*" matches all deployment events).
    fn manifest_subscribes_to(manifest: &PluginManifest, event_type: &str) -> bool {
        for subscribed in &manifest.events {
            if subscribed == event_type {
                return true;
            }
            // Support wildcard: "deployment.*" matches "deployment.created", etc.
            if let Some(prefix) = subscribed.strip_suffix(".*") {
                if let Some(event_prefix) = event_type.split('.').next() {
                    if prefix == event_prefix {
                        return true;
                    }
                }
            }
            // Support "*" to match all events
            if subscribed == "*" {
                return true;
            }
        }
        false
    }

    /// POST a `PluginEvent` to a plugin's `/_events` endpoint over Unix socket.
    async fn deliver_event(
        socket_path: &PathBuf,
        auth_secret: &str,
        event: &PluginEvent,
    ) -> Result<(), String> {
        let body =
            serde_json::to_vec(event).map_err(|e| format!("Failed to serialize event: {}", e))?;

        let stream = UnixStream::connect(socket_path).await.map_err(|e| {
            format!(
                "Cannot connect to plugin socket {}: {}",
                socket_path.display(),
                e
            )
        })?;

        let io = TokioIo::new(stream);

        let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
            .await
            .map_err(|e| format!("HTTP handshake failed: {}", e))?;

        tokio::spawn(async move {
            if let Err(e) = conn.await {
                debug!("Plugin event delivery connection closed: {}", e);
            }
        });

        let request = Request::builder()
            .method(hyper::Method::POST)
            .uri(PLUGIN_EVENTS_PATH)
            .header(hyper::header::CONTENT_TYPE, "application/json")
            .header(hyper::header::HOST, "localhost")
            .header(
                temps_core::external_plugin::headers::AUTH_SIGNATURE,
                auth_secret,
            )
            .header("x-temps-request-id", uuid::Uuid::new_v4().to_string())
            .body(Body::from(body))
            .map_err(|e| format!("Failed to build request: {}", e))?;

        let response = sender
            .send_request(request)
            .await
            .map_err(|e| format!("Failed to send event: {}", e))?;

        let status = response.status();
        if !status.is_success() {
            return Err(format!(
                "Plugin returned HTTP {} for event {}",
                status, event.event_type
            ));
        }

        Ok(())
    }
}

impl Drop for PluginEventListener {
    fn drop(&mut self) {
        // Signal stop so the task exits its select! loop
        let _ = self.stop_tx.send(true);
        match self.task_handle.try_write() {
            Ok(mut guard) => {
                if let Some(handle) = guard.take() {
                    handle.abort();
                }
            }
            Err(_) => {
                // Lock is held — task will be cleaned up when Arc is dropped.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_core::external_plugin::PluginManifest;

    #[test]
    fn test_manifest_subscribes_to_exact_match() {
        let manifest = PluginManifest::builder("test", "1.0.0")
            .event("deployment.succeeded")
            .event("project.created")
            .build();

        assert!(PluginEventListener::manifest_subscribes_to(
            &manifest,
            "deployment.succeeded"
        ));
        assert!(PluginEventListener::manifest_subscribes_to(
            &manifest,
            "project.created"
        ));
        assert!(!PluginEventListener::manifest_subscribes_to(
            &manifest,
            "deployment.failed"
        ));
        assert!(!PluginEventListener::manifest_subscribes_to(
            &manifest,
            "domain.created"
        ));
    }

    #[test]
    fn test_manifest_subscribes_to_wildcard() {
        let manifest = PluginManifest::builder("test", "1.0.0")
            .event("deployment.*")
            .build();

        assert!(PluginEventListener::manifest_subscribes_to(
            &manifest,
            "deployment.created"
        ));
        assert!(PluginEventListener::manifest_subscribes_to(
            &manifest,
            "deployment.succeeded"
        ));
        assert!(PluginEventListener::manifest_subscribes_to(
            &manifest,
            "deployment.failed"
        ));
        assert!(!PluginEventListener::manifest_subscribes_to(
            &manifest,
            "project.created"
        ));
    }

    #[test]
    fn test_manifest_subscribes_to_star_all() {
        let manifest = PluginManifest::builder("test", "1.0.0").event("*").build();

        assert!(PluginEventListener::manifest_subscribes_to(
            &manifest,
            "deployment.created"
        ));
        assert!(PluginEventListener::manifest_subscribes_to(
            &manifest,
            "project.deleted"
        ));
        assert!(PluginEventListener::manifest_subscribes_to(
            &manifest,
            "domain.provisioned"
        ));
    }

    #[test]
    fn test_manifest_no_events_matches_nothing() {
        let manifest = PluginManifest::builder("test", "1.0.0").build();

        assert!(!PluginEventListener::manifest_subscribes_to(
            &manifest,
            "deployment.created"
        ));
    }

    #[test]
    fn test_job_to_plugin_event_deployment_created() {
        let job = Job::DeploymentCreated(temps_core::DeploymentCreatedJob {
            deployment_id: 42,
            project_id: 7,
            environment_id: 1,
            environment_name: "production".to_string(),
            commit_sha: Some("abc123".to_string()),
            branch: Some("main".to_string()),
        });

        let event = PluginEventListener::job_to_plugin_event(&job).unwrap();
        assert_eq!(event.event_type, "deployment.created");
        assert_eq!(event.project_id, Some(7));
        assert_eq!(event.data["deployment_id"], 42);
        assert_eq!(event.data["environment_name"], "production");
        assert_eq!(event.data["commit_sha"], "abc123");
        assert_eq!(event.data["branch"], "main");
    }

    #[test]
    fn test_job_to_plugin_event_deployment_succeeded() {
        let job = Job::DeploymentSucceeded(temps_core::DeploymentSucceededJob {
            deployment_id: 42,
            project_id: 7,
            environment_id: 1,
            environment_name: "production".to_string(),
            commit_sha: Some("abc123".to_string()),
            url: Some("https://app.example.com".to_string()),
            health_check_path: None,
        });

        let event = PluginEventListener::job_to_plugin_event(&job).unwrap();
        assert_eq!(event.event_type, "deployment.succeeded");
        assert_eq!(event.data["url"], "https://app.example.com");
    }

    #[test]
    fn test_job_to_plugin_event_deployment_failed() {
        let job = Job::DeploymentFailed(temps_core::DeploymentFailedJob {
            deployment_id: 42,
            project_id: 7,
            environment_id: 1,
            environment_name: "staging".to_string(),
            error_message: Some("OOM killed".to_string()),
        });

        let event = PluginEventListener::job_to_plugin_event(&job).unwrap();
        assert_eq!(event.event_type, "deployment.failed");
        assert_eq!(event.data["error_message"], "OOM killed");
    }

    #[test]
    fn test_job_to_plugin_event_project_created() {
        let job = Job::ProjectCreated(temps_core::ProjectCreatedJob {
            project_id: 10,
            project_name: "my-app".to_string(),
        });

        let event = PluginEventListener::job_to_plugin_event(&job).unwrap();
        assert_eq!(event.event_type, "project.created");
        assert_eq!(event.project_id, Some(10));
        assert_eq!(event.data["project_name"], "my-app");
    }

    #[test]
    fn test_job_to_plugin_event_project_deleted() {
        let job = Job::ProjectDeleted(temps_core::ProjectDeletedJob {
            project_id: 10,
            project_name: "my-app".to_string(),
        });

        let event = PluginEventListener::job_to_plugin_event(&job).unwrap();
        assert_eq!(event.event_type, "project.deleted");
    }

    #[test]
    fn test_job_to_plugin_event_domain_created() {
        let job = Job::DomainCreated(temps_core::DomainCreatedJob {
            domain_id: 5,
            project_id: 7,
            domain_name: "example.com".to_string(),
        });

        let event = PluginEventListener::job_to_plugin_event(&job).unwrap();
        assert_eq!(event.event_type, "domain.created");
        assert_eq!(event.data["domain_name"], "example.com");
    }

    #[test]
    fn test_job_to_plugin_event_domain_provisioned() {
        let job = Job::DomainProvisioned(temps_core::DomainProvisionedJob {
            domain_id: 5,
            project_id: 7,
            domain_name: "example.com".to_string(),
        });

        let event = PluginEventListener::job_to_plugin_event(&job).unwrap();
        assert_eq!(event.event_type, "domain.provisioned");
    }

    #[test]
    fn test_job_to_plugin_event_unrelated_job_returns_none() {
        let job = Job::RenewCertificate(temps_core::RenewCertificateJob {
            domain: "example.com".to_string(),
        });

        assert!(PluginEventListener::job_to_plugin_event(&job).is_none());
    }

    #[test]
    fn test_job_to_plugin_event_deployment_cancelled() {
        let job = Job::DeploymentCancelled(temps_core::DeploymentCancelledJob {
            deployment_id: 42,
            project_id: 7,
            environment_id: 1,
            environment_name: "staging".to_string(),
        });

        let event = PluginEventListener::job_to_plugin_event(&job).unwrap();
        assert_eq!(event.event_type, "deployment.cancelled");
    }

    #[test]
    fn test_job_to_plugin_event_deployment_ready() {
        let job = Job::DeploymentReady(temps_core::DeploymentReadyJob {
            deployment_id: 42,
            project_id: 7,
            environment_id: 1,
            environment_name: "production".to_string(),
            url: Some("https://app.example.com".to_string()),
        });

        let event = PluginEventListener::job_to_plugin_event(&job).unwrap();
        assert_eq!(event.event_type, "deployment.ready");
        assert_eq!(event.data["url"], "https://app.example.com");
    }

    #[test]
    fn test_plugin_event_has_uuid_id() {
        let job = Job::ProjectCreated(temps_core::ProjectCreatedJob {
            project_id: 1,
            project_name: "test".to_string(),
        });

        let event = PluginEventListener::job_to_plugin_event(&job).unwrap();
        // UUID format: 8-4-4-4-12 hex chars
        assert_eq!(event.id.len(), 36);
        assert!(event.id.contains('-'));
    }
}
