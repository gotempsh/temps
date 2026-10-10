// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! On-demand screenshot capture for an existing deployment
//! (`POST …/deployments/{id}/operations` with `take_screenshot`).
//!
//! The request only *starts* the capture. It is recorded as `pending` and the
//! capture runs in the background; the record becomes `completed` (with the
//! stored image's location) or `failed` (with the reason) when it ends.
//! Nothing is reported as successful until an image has been captured,
//! validated and recorded on the deployment.

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use chrono::Utc;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use temps_config::ConfigService;
use temps_core::UtcDateTime;
use temps_database::DbConnection;
use temps_entities::{deployments, prelude::Deployments};
use temps_screenshots::ScreenshotServiceTrait;
use thiserror::Error;
use tracing::{error, info, warn};

use super::external_deployment::{
    DeploymentOperation, DeploymentOperationDetails, DeploymentScreenshotCapture,
    ExternalDeploymentManager, OperationResult, OperationStatus,
};
use crate::jobs::{capture_deployment_screenshot, DeploymentScreenshotError};

/// Console page where screenshots are enabled and their provider chosen.
pub const SCREENSHOT_SETTINGS_PATH: &str = "/settings#screenshots";

/// Hard ceiling for one on-demand capture. Providers bound page loads
/// themselves; this guarantees a `pending` record always ends, so a hung
/// provider cannot block later captures of the same deployment forever.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Error)]
pub enum ScreenshotOperationError {
    #[error("Deployment {deployment_id} not found in project {project_id}")]
    DeploymentNotFound { project_id: i32, deployment_id: i32 },

    #[error(
        "Screenshots are disabled on this instance, so deployment {deployment_id} cannot be captured. \
         Enable them under Settings → Screenshots ({SCREENSHOT_SETTINGS_PATH})"
    )]
    Disabled { deployment_id: i32 },

    #[error(
        "Screenshot provider '{provider}' is not available, so deployment {deployment_id} cannot be captured: {reason}"
    )]
    ProviderUnavailable {
        deployment_id: i32,
        provider: &'static str,
        reason: String,
    },

    #[error("A screenshot of deployment {deployment_id} is already being captured (started {started_at})")]
    AlreadyRunning {
        deployment_id: i32,
        started_at: String,
    },

    #[error("Failed to record the screenshot operation for deployment {deployment_id}: {reason}")]
    Record { deployment_id: i32, reason: String },

    #[error("Failed to look up deployment {deployment_id} in project {project_id}: {source}")]
    Database {
        project_id: i32,
        deployment_id: i32,
        #[source]
        source: sea_orm::DbErr,
    },
}

/// Captures running right now, by deployment ID, with when each started.
///
/// Kept apart from the operation history on purpose: history is trimmed and
/// gains newer records while a capture runs, so its latest entry cannot prove
/// whether a capture is still in progress.
type InFlightCaptures = Arc<Mutex<HashMap<i32, UtcDateTime>>>;

fn lock_in_flight(in_flight: &InFlightCaptures) -> MutexGuard<'_, HashMap<i32, UtcDateTime>> {
    // Every critical section is a single insert or remove, so a panic while
    // holding the lock cannot leave the map half-updated; keep using it.
    in_flight
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A deployment's claim on its single capture slot. Dropping it frees the
/// slot, and it is moved into the background task, so the slot is held until
/// that task ends -- however it ends.
struct CaptureSlot {
    in_flight: InFlightCaptures,
    deployment_id: i32,
}

impl Drop for CaptureSlot {
    fn drop(&mut self) {
        lock_in_flight(&self.in_flight).remove(&self.deployment_id);
    }
}

pub struct ScreenshotOperationService {
    db: Arc<DbConnection>,
    screenshot_service: Arc<dyn ScreenshotServiceTrait>,
    config_service: Arc<ConfigService>,
    operations: Arc<ExternalDeploymentManager>,
    in_flight: InFlightCaptures,
}

impl ScreenshotOperationService {
    pub fn new(
        db: Arc<DbConnection>,
        screenshot_service: Arc<dyn ScreenshotServiceTrait>,
        config_service: Arc<ConfigService>,
        operations: Arc<ExternalDeploymentManager>,
    ) -> Self {
        Self {
            db,
            screenshot_service,
            config_service,
            operations,
            in_flight: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Validate that a capture can run, record it as `pending` and start it in
    /// the background. Returns the `pending` record.
    ///
    /// When screenshots are disabled or the provider is unusable, the request
    /// fails with the reason *and* a `failed` record is kept, so the operation
    /// status endpoint explains the outcome too.
    pub async fn start(
        &self,
        project_id: i32,
        deployment_id: i32,
    ) -> Result<OperationResult, ScreenshotOperationError> {
        Deployments::find_by_id(deployment_id)
            .filter(deployments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await
            .map_err(|source| ScreenshotOperationError::Database {
                project_id,
                deployment_id,
                source,
            })?
            .ok_or(ScreenshotOperationError::DeploymentNotFound {
                project_id,
                deployment_id,
            })?;

        // Claim the slot before probing the provider: the local browser probe
        // launches Chrome and takes seconds, and a concurrent request must be
        // told a capture is running rather than probe and record a failure
        // over it. Every early return below frees the slot.
        let slot = self.claim(deployment_id)?;

        if !self.screenshot_service.is_enabled().await {
            let err = ScreenshotOperationError::Disabled { deployment_id };
            self.record_failure(project_id, deployment_id, &err.to_string());
            return Err(err);
        }

        if let Err(reason) = self.screenshot_service.check_provider_availability().await {
            let err = ScreenshotOperationError::ProviderUnavailable {
                deployment_id,
                provider: self.screenshot_service.provider_name(),
                reason: reason.to_string(),
            };
            self.record_failure(project_id, deployment_id, &err.to_string());
            return Err(err);
        }

        let pending = operation_record(
            OperationStatus::Pending,
            format!(
                "Capturing a screenshot of deployment {} with provider '{}'",
                deployment_id,
                self.screenshot_service.provider_name()
            ),
            DeploymentOperationDetails::new(project_id, deployment_id.to_string()),
        );
        self.operations
            .record_operation(project_id, &deployment_id.to_string(), pending.clone())
            .map_err(|reason| ScreenshotOperationError::Record {
                deployment_id,
                reason,
            })?;

        let db = self.db.clone();
        let screenshot_service = self.screenshot_service.clone();
        let config_service = self.config_service.clone();
        let operations = self.operations.clone();
        tokio::spawn(async move {
            let result = tokio::time::timeout(
                CAPTURE_TIMEOUT,
                capture_deployment_screenshot(
                    deployment_id,
                    screenshot_service.as_ref(),
                    config_service.as_ref(),
                    db.as_ref(),
                ),
            )
            .await
            .unwrap_or_else(|_| {
                Err(DeploymentScreenshotError::TimedOut {
                    deployment_id,
                    timeout_secs: CAPTURE_TIMEOUT.as_secs(),
                })
            });

            let mut details =
                DeploymentOperationDetails::new(project_id, deployment_id.to_string());
            let record = match result {
                Ok(captured) => {
                    info!(
                        deployment_id,
                        project_id,
                        screenshot_location = %captured.screenshot_location,
                        "On-demand screenshot captured"
                    );
                    let message = format!(
                        "Screenshot of deployment {} captured and saved to {}",
                        deployment_id, captured.screenshot_location
                    );
                    details.screenshot = Some(DeploymentScreenshotCapture {
                        url: captured.url,
                        screenshot_location: captured.screenshot_location,
                        captured_at: captured.captured_at,
                    });
                    operation_record(OperationStatus::Completed, message, details)
                }
                Err(e) => {
                    warn!(
                        deployment_id,
                        project_id, "On-demand screenshot failed: {}", e
                    );
                    operation_record(OperationStatus::Failed, e.to_string(), details)
                }
            };

            if let Err(e) =
                operations.record_operation(project_id, &deployment_id.to_string(), record)
            {
                error!(
                    deployment_id,
                    project_id, "Failed to record the outcome of a screenshot capture: {}", e
                );
            }
            // Free the slot only after the outcome is readable, so a new
            // request never starts before this one's result is recorded.
            drop(slot);
        });

        Ok(pending)
    }

    /// Take `deployment_id`'s capture slot, or report the capture holding it.
    fn claim(&self, deployment_id: i32) -> Result<CaptureSlot, ScreenshotOperationError> {
        let mut in_flight = lock_in_flight(&self.in_flight);
        match in_flight.entry(deployment_id) {
            Entry::Occupied(running) => Err(ScreenshotOperationError::AlreadyRunning {
                deployment_id,
                started_at: running.get().to_rfc3339(),
            }),
            Entry::Vacant(slot) => {
                slot.insert(Utc::now());
                Ok(CaptureSlot {
                    in_flight: self.in_flight.clone(),
                    deployment_id,
                })
            }
        }
    }

    fn record_failure(&self, project_id: i32, deployment_id: i32, message: &str) {
        let record = operation_record(
            OperationStatus::Failed,
            message.to_string(),
            DeploymentOperationDetails::new(project_id, deployment_id.to_string()),
        );
        if let Err(e) =
            self.operations
                .record_operation(project_id, &deployment_id.to_string(), record)
        {
            error!(
                deployment_id,
                project_id, "Failed to record a rejected screenshot operation: {}", e
            );
        }
    }
}

fn operation_record(
    status: OperationStatus,
    message: String,
    data: DeploymentOperationDetails,
) -> OperationResult {
    OperationResult {
        operation: DeploymentOperation::TakeScreenshot,
        status,
        success: status == OperationStatus::Completed,
        message,
        data,
        executed_at: Utc::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use temps_screenshots::{ScreenshotError, ScreenshotResult};

    /// Configurable fake provider. `capture_and_save` counts its calls so a
    /// test can prove whether a capture was actually attempted.
    struct FakeScreenshots {
        enabled: bool,
        availability: Result<(), String>,
        capture: Result<(), String>,
        captures: AtomicUsize,
    }

    impl FakeScreenshots {
        fn new(
            enabled: bool,
            availability: Result<(), String>,
            capture: Result<(), String>,
        ) -> Self {
            Self {
                enabled,
                availability,
                capture,
                captures: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl ScreenshotServiceTrait for FakeScreenshots {
        async fn capture_and_save(&self, _url: &str, filename: &str) -> ScreenshotResult<PathBuf> {
            self.captures.fetch_add(1, Ordering::SeqCst);
            match &self.capture {
                Ok(()) => Ok(PathBuf::from("/tmp/static").join(filename)),
                Err(reason) => Err(ScreenshotError::InvalidImage {
                    url: "http://app.local".to_string(),
                    reason: reason.clone(),
                }),
            }
        }

        async fn capture(&self, _url: &str) -> ScreenshotResult<Vec<u8>> {
            Err(ScreenshotError::ProviderNotConfigured)
        }

        async fn is_enabled(&self) -> bool {
            self.enabled
        }

        fn provider_name(&self) -> &'static str {
            "local-headless-chrome"
        }

        async fn is_provider_available(&self) -> bool {
            self.availability.is_ok()
        }

        async fn check_provider_availability(&self) -> ScreenshotResult<()> {
            self.availability
                .clone()
                .map_err(ScreenshotError::ChromeError)
        }
    }

    fn deployment(id: i32, project_id: i32) -> deployments::Model {
        deployments::Model {
            id,
            project_id,
            environment_id: 1,
            slug: format!("deploy-{}", id),
            state: "deployed".to_string(),
            metadata: None,
            deploying_at: None,
            ready_at: None,
            started_at: Some(chrono::Utc::now()),
            finished_at: Some(chrono::Utc::now()),
            context_vars: None,
            branch_ref: Some("main".to_string()),
            tag_ref: None,
            commit_sha: None,
            commit_message: None,
            commit_author: None,
            commit_json: None,
            cancelled_reason: None,
            static_dir_location: None,
            screenshot_location: None,
            image_name: None,
            deployment_config: None,
            promoted_from_deployment_id: None,
            upload_request_id: None,
            docker_socket_mounted: false,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn config_service(db: Arc<DbConnection>) -> Arc<ConfigService> {
        let server_config = Arc::new(temps_config::ServerConfig {
            address: "127.0.0.1:3000".to_string(),
            database_url: "postgres://test".to_string(),
            tls_address: None,
            console_address: "127.0.0.1:0".to_string(),
            console_admin_address: None,
            admin_allowed_ips: Vec::new(),
            admin_allowed_hosts: Vec::new(),
            admin_trust_forwarded_for: false,
            docker_extra_networks: Vec::new(),
            data_dir: PathBuf::from("/tmp/temps-test"),
            auth_secret: "test-secret".to_string(),
            encryption_key: "test-key".to_string(),
            api_base_url: "/api".to_string(),
            postgres_max_connections: None,
            postgres_min_connections: None,
            postgres_connect_timeout_secs: None,
            postgres_acquire_timeout_secs: None,
            postgres_idle_timeout_secs: None,
            postgres_max_lifetime_secs: None,
            clickhouse_url: None,
            clickhouse_database: None,
            clickhouse_user: None,
            clickhouse_password: None,
        });
        Arc::new(ConfigService::new(server_config, db))
    }

    fn service(
        db: MockDatabase,
        screenshots: Arc<FakeScreenshots>,
        operations: Arc<ExternalDeploymentManager>,
    ) -> ScreenshotOperationService {
        let db = Arc::new(db.into_connection());
        ScreenshotOperationService::new(db.clone(), screenshots, config_service(db), operations)
    }

    /// Poll until the background capture of deployment 7 in project 3 ends.
    async fn outcome(operations: &ExternalDeploymentManager) -> OperationResult {
        for _ in 0..250 {
            let current = operations
                .get_latest_operation(3, "7", &DeploymentOperation::TakeScreenshot)
                .expect("the pending record is readable while the capture runs");
            if current.status != OperationStatus::Pending {
                return current;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the background capture of deployment 7 never finished");
    }

    /// Ownership lookup, then the URL resolution the capture performs
    /// (deployment row, then the settings row -- empty means defaults).
    fn capture_reaches_the_provider() -> MockDatabase {
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![deployment(7, 3)]])
            .append_query_results(vec![vec![deployment(7, 3)]])
            .append_query_results(vec![Vec::<temps_entities::settings::Model>::new()])
    }

    #[tokio::test]
    async fn disabled_screenshots_fail_with_setup_path_and_never_capture() {
        let screenshots = Arc::new(FakeScreenshots::new(false, Ok(()), Ok(())));
        let operations = Arc::new(ExternalDeploymentManager::new());
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![deployment(7, 3)]]);

        let err = service(db, screenshots.clone(), operations.clone())
            .start(3, 7)
            .await
            .expect_err("disabled screenshots must not report success");

        assert!(matches!(
            err,
            ScreenshotOperationError::Disabled { deployment_id: 7 }
        ));
        assert!(err.to_string().contains(SCREENSHOT_SETTINGS_PATH));
        assert_eq!(screenshots.captures.load(Ordering::SeqCst), 0);

        let latest = operations
            .get_latest_operation(3, "7", &DeploymentOperation::TakeScreenshot)
            .expect("the rejection is readable from the status endpoint");
        assert_eq!(latest.status, OperationStatus::Failed);
        assert!(!latest.success);
        assert_eq!(latest.data, DeploymentOperationDetails::new(3, "7"));
    }

    #[tokio::test]
    async fn unusable_browser_fails_with_the_provider_reason() {
        let screenshots = Arc::new(FakeScreenshots::new(
            true,
            Err("Chrome availability check timed out after 10 seconds".to_string()),
            Ok(()),
        ));
        let operations = Arc::new(ExternalDeploymentManager::new());
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![deployment(7, 3)]]);

        let err = service(db, screenshots.clone(), operations.clone())
            .start(3, 7)
            .await
            .expect_err("an unusable browser must not report success");

        let message = err.to_string();
        assert!(message.contains("local-headless-chrome"), "{message}");
        assert!(message.contains("timed out after 10 seconds"), "{message}");
        assert_eq!(screenshots.captures.load(Ordering::SeqCst), 0);
        let latest = operations
            .get_latest_operation(3, "7", &DeploymentOperation::TakeScreenshot)
            .unwrap();
        assert_eq!(latest.status, OperationStatus::Failed);
    }

    #[tokio::test]
    async fn deployment_of_another_project_is_not_found() {
        let screenshots = Arc::new(FakeScreenshots::new(true, Ok(()), Ok(())));
        let operations = Arc::new(ExternalDeploymentManager::new());
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<deployments::Model>::new()]);

        let err = service(db, screenshots.clone(), operations.clone())
            .start(99, 7)
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            ScreenshotOperationError::DeploymentNotFound {
                project_id: 99,
                deployment_id: 7
            }
        ));
        assert!(operations.get_operations(99, "7").is_empty());
        assert!(operations.get_operations(3, "7").is_empty());
    }

    #[tokio::test]
    async fn capture_starts_pending_and_ends_failed_with_the_capture_error() {
        let screenshots = Arc::new(FakeScreenshots::new(
            true,
            Ok(()),
            Err("the provider returned no data".to_string()),
        ));
        let operations = Arc::new(ExternalDeploymentManager::new());

        let pending = service(
            capture_reaches_the_provider(),
            screenshots.clone(),
            operations.clone(),
        )
        .start(3, 7)
        .await
        .expect("an available provider starts the capture");
        assert_eq!(pending.status, OperationStatus::Pending);
        assert!(!pending.success);

        let latest = outcome(&operations).await;
        assert_eq!(latest.status, OperationStatus::Failed);
        assert!(!latest.success);
        assert_eq!(screenshots.captures.load(Ordering::SeqCst), 1);
        assert!(
            latest.message.contains("the provider returned no data"),
            "{}",
            latest.message
        );
        assert_eq!(latest.data.screenshot, None);
    }

    #[tokio::test]
    async fn successful_capture_completes_with_the_stored_location() {
        let screenshots = Arc::new(FakeScreenshots::new(true, Ok(()), Ok(())));
        let operations = Arc::new(ExternalDeploymentManager::new());
        let mut captured = deployment(7, 3);
        captured.screenshot_location = Some("screenshots/deployment-7.png".to_string());
        // Then the reload before the update, and the UPDATE ... RETURNING row.
        let db = capture_reaches_the_provider()
            .append_query_results(vec![vec![deployment(7, 3)]])
            .append_query_results(vec![vec![captured]]);

        let pending = service(db, screenshots.clone(), operations.clone())
            .start(3, 7)
            .await
            .expect("an available provider starts the capture");
        assert_eq!(pending.status, OperationStatus::Pending);

        let latest = outcome(&operations).await;
        assert_eq!(latest.status, OperationStatus::Completed, "{latest:?}");
        assert!(latest.success);
        let screenshot = latest
            .data
            .screenshot
            .expect("a completed capture reports where it was stored");
        assert!(
            screenshot
                .screenshot_location
                .starts_with("screenshots/deployment-7-")
                && screenshot.screenshot_location.ends_with(".png"),
            "{}",
            screenshot.screenshot_location
        );
        assert_eq!(screenshots.captures.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn second_request_is_rejected_before_probing_the_provider() {
        // A probe would fail, so reaching it would surface ProviderUnavailable.
        let screenshots = Arc::new(FakeScreenshots::new(
            true,
            Err("probe must not run while a capture is in flight".to_string()),
            Ok(()),
        ));
        let operations = Arc::new(ExternalDeploymentManager::new());
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![deployment(7, 3)]]);
        let service = service(db, screenshots.clone(), operations.clone());
        let _running = service.claim(7).expect("first capture takes the slot");

        let err = service.start(3, 7).await.unwrap_err();

        assert!(matches!(
            err,
            ScreenshotOperationError::AlreadyRunning {
                deployment_id: 7,
                ..
            }
        ));
        assert_eq!(screenshots.captures.load(Ordering::SeqCst), 0);
        // The rejected request leaves no record that could hide the running one.
        assert!(operations.get_operations(3, "7").is_empty());
    }

    #[tokio::test]
    async fn running_capture_stays_guarded_whatever_the_history_says() {
        let screenshots = Arc::new(FakeScreenshots::new(true, Ok(()), Ok(())));
        let operations = Arc::new(ExternalDeploymentManager::new());
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![deployment(7, 3)]]);
        let service = service(db, screenshots.clone(), operations.clone());
        let _running = service.claim(7).expect("first capture takes the slot");

        // A failed record on top, then enough other operations to trim every
        // screenshot record out of the bounded history.
        service.record_failure(3, 7, "a rejected request");
        for _ in 0..60 {
            operations
                .record_operation(
                    3,
                    "7",
                    OperationResult {
                        operation: DeploymentOperation::MarkComplete,
                        status: OperationStatus::Completed,
                        success: true,
                        message: "recorded".to_string(),
                        data: DeploymentOperationDetails::new(3, "7"),
                        executed_at: Utc::now(),
                    },
                )
                .unwrap();
        }
        assert!(operations
            .get_latest_operation(3, "7", &DeploymentOperation::TakeScreenshot)
            .is_none());

        let err = service.start(3, 7).await.unwrap_err();

        assert!(matches!(
            err,
            ScreenshotOperationError::AlreadyRunning {
                deployment_id: 7,
                ..
            }
        ));
        assert_eq!(screenshots.captures.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn rejected_requests_free_the_slot() {
        let screenshots = Arc::new(FakeScreenshots::new(
            true,
            Err("no browser".to_string()),
            Ok(()),
        ));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![deployment(7, 3)]])
            .append_query_results(vec![vec![deployment(7, 3)]]);
        let service = service(db, screenshots, Arc::new(ExternalDeploymentManager::new()));

        for _ in 0..2 {
            let err = service.start(3, 7).await.unwrap_err();
            assert!(
                matches!(err, ScreenshotOperationError::ProviderUnavailable { .. }),
                "{err}"
            );
        }
        assert!(service.claim(7).is_ok());
    }

    #[tokio::test]
    async fn finished_capture_frees_the_slot_after_recording_its_outcome() {
        let screenshots = Arc::new(FakeScreenshots::new(
            true,
            Ok(()),
            Err("the provider returned no data".to_string()),
        ));
        let operations = Arc::new(ExternalDeploymentManager::new());
        let service = service(
            capture_reaches_the_provider(),
            screenshots,
            operations.clone(),
        );

        service.start(3, 7).await.expect("capture starts");
        assert_eq!(outcome(&operations).await.status, OperationStatus::Failed);

        for _ in 0..250 {
            if service.claim(7).is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the slot was never freed after the capture ended");
    }
}
