// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Retires a branch's preview environment once the branch is deleted or its
//! pull request is closed or merged (`Job::PreviewBranchClosed`), so previews
//! stop holding containers after the work they showed is done.

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use temps_core::{
    AuditLogger, AuditOperation, Job, JobReceiver, PreviewBranchCloseReason,
    PreviewBranchClosedJob, QueueError,
};
use tokio::sync::Semaphore;
use tracing::{error, info, warn};

use super::environment_teardown::{EnvironmentTeardownError, EnvironmentTeardownService};

/// Teardowns that may run at once. Each can wait minutes on an unreachable
/// worker; more would only contend for the same database and Docker hosts.
const MAX_CONCURRENT_TEARDOWNS: usize = 4;

/// Recorded for each preview environment retired because its branch closed.
#[derive(Debug, Clone, Serialize)]
pub struct PreviewEnvironmentRetiredAudit {
    pub project_id: i32,
    pub environment_id: i32,
    pub environment_name: String,
    pub branch: String,
    pub reason: PreviewBranchCloseReason,
}

impl AuditOperation for PreviewEnvironmentRetiredAudit {
    fn operation_type(&self) -> String {
        "PREVIEW_ENVIRONMENT_RETIRED".to_string()
    }

    // A webhook did this, not a user.
    fn user_id(&self) -> Option<i32> {
        None
    }

    fn ip_address(&self) -> Option<String> {
        None
    }

    fn user_agent(&self) -> &str {
        "temps preview cleanup"
    }

    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

pub struct PreviewCleanupService {
    teardown: Arc<EnvironmentTeardownService>,
    audit: Arc<dyn AuditLogger>,
    slots: Arc<Semaphore>,
}

impl PreviewCleanupService {
    pub fn new(teardown: Arc<EnvironmentTeardownService>, audit: Arc<dyn AuditLogger>) -> Self {
        Self {
            teardown,
            audit,
            slots: Arc::new(Semaphore::new(MAX_CONCURRENT_TEARDOWNS)),
        }
    }

    /// Retire previews for every `PreviewBranchClosed` job until the queue
    /// closes.
    pub async fn run(self: Arc<Self>, mut receiver: Box<dyn JobReceiver>) {
        loop {
            match receiver.recv().await {
                Ok(Job::PreviewBranchClosed(job)) => {
                    let service = Arc::clone(&self);
                    tokio::spawn(async move {
                        // Errors are logged where they happen.
                        let _ = service.retire(job).await;
                    });
                }
                Ok(_) => {}
                Err(QueueError::ChannelClosed) => {
                    info!("Job queue closed, stopping preview cleanup");
                    return;
                }
                Err(error) => {
                    warn!(%error, "Preview cleanup failed to receive a job");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    }

    /// Tear down the previews of the job's branch and audit each one.
    /// Returns how many were retired.
    pub async fn retire(
        &self,
        job: PreviewBranchClosedJob,
    ) -> Result<usize, EnvironmentTeardownError> {
        // The semaphore is never closed, so acquiring cannot fail.
        let _slot = self.slots.acquire().await.ok();

        let previews = self
            .teardown
            .tear_down_branch_previews(job.project_id, &job.branch)
            .await
            .inspect_err(|error| {
                error!(
                    project_id = job.project_id,
                    branch = %job.branch,
                    reason = %job.reason,
                    %error,
                    "Failed to retire preview environments of a closed branch"
                )
            })?;

        for preview in &previews {
            info!(
                project_id = job.project_id,
                environment_id = preview.id,
                branch = %job.branch,
                reason = %job.reason,
                "Retired preview environment"
            );
            let audit = PreviewEnvironmentRetiredAudit {
                project_id: job.project_id,
                environment_id: preview.id,
                environment_name: preview.name.clone(),
                branch: job.branch.clone(),
                reason: job.reason,
            };
            if let Err(error) = self.audit.create_audit_log(&audit).await {
                error!(
                    project_id = job.project_id,
                    environment_id = preview.id,
                    %error,
                    "Failed to audit a retired preview environment"
                );
            }
        }

        Ok(previews.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::environment_service::EnvironmentService;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::Mutex;
    use temps_core::retry::RetryConfig;
    use temps_core::{ContainerCleanupError, DeploymentCanceller, DeploymentContainerCleaner};
    use temps_entities::environments;

    const PROJECT_ID: i32 = 3;
    const PREVIEW_ID: i32 = 7;

    fn preview() -> environments::Model {
        environments::Model {
            id: PREVIEW_ID,
            project_id: PROJECT_ID,
            name: "feature-login".to_string(),
            slug: "feature-login".to_string(),
            subdomain: "project-feature-login".to_string(),
            branch: Some("feature/login".to_string()),
            host: String::new(),
            upstreams: temps_entities::upstream_config::UpstreamList::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            last_deployment: None,
            current_deployment_id: Some(11),
            deleted_at: None,
            deployment_config: None,
            is_preview: true,
            protected: false,
            sleeping: false,
            attack_mode: None,
            shield_mode: None,
            shield_exempt_paths: Default::default(),
            force_https: None,
            last_activity_at: None,
            ingress_mode: Default::default(),
            workload_mode: Default::default(),
        }
    }

    fn fenced() -> environments::Model {
        let mut fenced = preview();
        fenced.deleted_at = Some(chrono::Utc::now());
        fenced.current_deployment_id = None;
        fenced
    }

    /// Query results for one live preview found and soft-deleted: the branch
    /// lookup, the locked read, the delivery bindings and the update.
    fn database_retiring_one_preview() -> MockDatabase {
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![preview()]])
            .append_query_results(vec![vec![preview()]])
            .append_query_results(vec![
                Vec::<temps_entities::domain_delivery_bindings::Model>::new(),
            ])
            .append_query_results(vec![vec![fenced()]])
    }

    /// Records the environments it was asked to stop; container cleanup fails
    /// with each queued error before succeeding.
    #[derive(Default)]
    struct RecordingDeployments {
        cancelled: Mutex<Vec<i32>>,
        cleaned: Mutex<Vec<(i32, i32)>>,
        cleanup_failures: Mutex<Vec<ContainerCleanupError>>,
    }

    #[temps_core::async_trait::async_trait]
    impl DeploymentCanceller for RecordingDeployments {
        async fn cancel_all_project_deployments(
            &self,
            _project_id: i32,
        ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
            unreachable!("previews are torn down per environment")
        }

        async fn cancel_all_environment_deployments(
            &self,
            environment_id: i32,
        ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
            self.cancelled.lock().unwrap().push(environment_id);
            Ok(1)
        }
    }

    #[temps_core::async_trait::async_trait]
    impl DeploymentContainerCleaner for RecordingDeployments {
        async fn cleanup_project_containers(
            &self,
            _project_id: i32,
        ) -> Result<u64, ContainerCleanupError> {
            unreachable!("previews are torn down per environment")
        }

        async fn cleanup_environment_containers(
            &self,
            project_id: i32,
            environment_id: i32,
        ) -> Result<u64, ContainerCleanupError> {
            self.cleaned
                .lock()
                .unwrap()
                .push((project_id, environment_id));
            match self.cleanup_failures.lock().unwrap().pop() {
                Some(error) => Err(error),
                None => Ok(1),
            }
        }
    }

    #[derive(Default)]
    struct RecordingAudit {
        operations: Mutex<Vec<(String, serde_json::Value)>>,
    }

    #[temps_core::async_trait::async_trait]
    impl AuditLogger for RecordingAudit {
        async fn create_audit_log(
            &self,
            operation: &dyn AuditOperation,
        ) -> temps_core::AuditResult<()> {
            let details = serde_json::from_str(&operation.serialize()?).unwrap();
            assert_eq!(operation.user_id(), None);
            self.operations
                .lock()
                .unwrap()
                .push((operation.operation_type(), details));
            Ok(())
        }
    }

    fn cleanup(
        db: MockDatabase,
        deployments: Arc<RecordingDeployments>,
        audit: Arc<RecordingAudit>,
    ) -> PreviewCleanupService {
        let server_config = temps_config::ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            "postgres://localhost/test".to_string(),
            None,
            None,
        )
        .unwrap();
        let config_service = Arc::new(temps_config::ConfigService::new(
            Arc::new(server_config),
            Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        ));
        let environments = Arc::new(EnvironmentService::new(
            Arc::new(db.into_connection()),
            config_service,
        ));
        let teardown =
            EnvironmentTeardownService::new(environments, deployments.clone(), deployments)
                .with_retry(RetryConfig::new(3).with_base_delay(Duration::ZERO));
        PreviewCleanupService::new(Arc::new(teardown), audit)
    }

    fn job(reason: PreviewBranchCloseReason) -> PreviewBranchClosedJob {
        PreviewBranchClosedJob {
            project_id: PROJECT_ID,
            branch: "feature/login".to_string(),
            reason,
        }
    }

    #[tokio::test]
    async fn a_merged_branch_retires_its_preview_and_audits_it() {
        let deployments = Arc::new(RecordingDeployments::default());
        let audit = Arc::new(RecordingAudit::default());
        let service = cleanup(
            database_retiring_one_preview(),
            deployments.clone(),
            audit.clone(),
        );

        let retired = service
            .retire(job(PreviewBranchCloseReason::PullRequestMerged {
                number: 12,
            }))
            .await
            .unwrap();

        assert_eq!(retired, 1);
        assert_eq!(*deployments.cancelled.lock().unwrap(), vec![PREVIEW_ID]);
        assert_eq!(
            *deployments.cleaned.lock().unwrap(),
            vec![(PROJECT_ID, PREVIEW_ID)]
        );
        let operations = audit.operations.lock().unwrap();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].0, "PREVIEW_ENVIRONMENT_RETIRED");
        assert_eq!(
            operations[0].1,
            serde_json::json!({
                "project_id": PROJECT_ID,
                "environment_id": PREVIEW_ID,
                "environment_name": "feature-login",
                "branch": "feature/login",
                "reason": { "kind": "pull_request_merged", "number": 12 }
            })
        );
    }

    #[tokio::test]
    async fn a_branch_without_a_preview_retires_nothing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<environments::Model>::new()]);
        let deployments = Arc::new(RecordingDeployments::default());
        let audit = Arc::new(RecordingAudit::default());
        let service = cleanup(db, deployments.clone(), audit.clone());

        let retired = service
            .retire(job(PreviewBranchCloseReason::BranchDeleted))
            .await
            .unwrap();

        assert_eq!(retired, 0);
        assert!(deployments.cancelled.lock().unwrap().is_empty());
        assert!(deployments.cleaned.lock().unwrap().is_empty());
        assert!(audit.operations.lock().unwrap().is_empty());
    }

    /// An unreachable worker is waited out: the retry finds the preview
    /// already fenced and finishes removing its containers.
    #[tokio::test]
    async fn an_unreachable_worker_is_retried_until_cleanup_finishes() {
        let db = database_retiring_one_preview().append_query_results(vec![vec![fenced()]]);
        let deployments = Arc::new(RecordingDeployments::default());
        deployments
            .cleanup_failures
            .lock()
            .unwrap()
            .push(ContainerCleanupError::NodeUnreachable {
                project_id: PROJECT_ID,
                environment_id: PREVIEW_ID,
                container_id: "container-1".to_string(),
                node_id: Some(2),
                reason: "connection refused".to_string(),
            });
        let audit = Arc::new(RecordingAudit::default());
        let service = cleanup(db, deployments.clone(), audit.clone());

        let retired = service
            .retire(job(PreviewBranchCloseReason::BranchDeleted))
            .await
            .unwrap();

        assert_eq!(retired, 1);
        assert_eq!(deployments.cleaned.lock().unwrap().len(), 2);
        assert_eq!(audit.operations.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_failed_container_removal_is_reported_not_retried() {
        let deployments = Arc::new(RecordingDeployments::default());
        deployments
            .cleanup_failures
            .lock()
            .unwrap()
            .push(ContainerCleanupError::Removal {
                project_id: PROJECT_ID,
                environment_id: PREVIEW_ID,
                container_id: "container-1".to_string(),
                node_id: None,
                reason: "device busy".to_string(),
            });
        let audit = Arc::new(RecordingAudit::default());
        let service = cleanup(
            database_retiring_one_preview(),
            deployments.clone(),
            audit.clone(),
        );

        let error = service
            .retire(job(PreviewBranchCloseReason::PullRequestClosed {
                number: 4,
            }))
            .await
            .unwrap_err();

        assert!(
            error.to_string().contains("container-1") && error.to_string().contains("device busy"),
            "{error}"
        );
        assert_eq!(deployments.cleaned.lock().unwrap().len(), 1);
        assert!(audit.operations.lock().unwrap().is_empty());
    }
}
