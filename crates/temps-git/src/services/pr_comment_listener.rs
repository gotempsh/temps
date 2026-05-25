//! Background listener that turns deployment lifecycle events into
//! sticky PR/MR preview comments.
//!
//! Vercel-style: when a deployment for a branch with an open PR moves
//! through created → succeeded/failed, we post or update a single
//! comment that always reflects the latest state. The comment is keyed
//! by a hidden HTML marker so subsequent updates edit-in-place.
//!
//! All failures here degrade gracefully — a missing git connection,
//! missing PR, or provider API outage produces a warn log, never an
//! application-level error. Deployments must not be affected by
//! comment-posting issues.

use sea_orm::{DatabaseConnection, EntityTrait};
use std::sync::Arc;
use temps_core::{Job, JobQueue};
use temps_entities::deployments;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use super::pr_commenter::{CommentPhase, PrCommenter, PreviewCommentContext};

/// Short SHA helper. GitHub-style 7-char abbreviation.
fn short_sha(commit: Option<&String>) -> String {
    commit
        .map(|s| s.chars().take(7).collect::<String>())
        .unwrap_or_else(|| "unknown".to_string())
}

pub struct PrCommentListener {
    commenter: Arc<dyn PrCommenter>,
    db: Arc<DatabaseConnection>,
    queue: Arc<dyn JobQueue>,
    running: Arc<RwLock<bool>>,
    task_handle: Arc<RwLock<Option<JoinHandle<()>>>>,
}

impl PrCommentListener {
    pub fn new(
        commenter: Arc<dyn PrCommenter>,
        db: Arc<DatabaseConnection>,
        queue: Arc<dyn JobQueue>,
    ) -> Self {
        Self {
            commenter,
            db,
            queue,
            running: Arc::new(RwLock::new(false)),
            task_handle: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut running = self.running.write().await;
        if *running {
            return Ok(());
        }
        *running = true;
        drop(running);

        info!("Starting PR preview-comment listener");

        let mut receiver = self.queue.subscribe();
        let commenter = self.commenter.clone();
        let db = self.db.clone();
        let running = self.running.clone();

        let handle = tokio::spawn(async move {
            while *running.read().await {
                match receiver.recv().await {
                    Ok(job) => {
                        if let Err(e) = Self::handle_job(&commenter, &db, &job).await {
                            warn!("PR comment listener: failed to handle job: {}", e);
                        }
                    }
                    Err(e) => {
                        debug!("PR comment listener queue recv error: {}", e);
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                }
            }
            info!("PR preview-comment listener stopped");
        });

        *self.task_handle.write().await = Some(handle);
        Ok(())
    }

    pub async fn stop(&self) {
        *self.running.write().await = false;
        if let Some(handle) = self.task_handle.write().await.take() {
            let _ = handle.await;
        }
    }

    async fn handle_job(
        commenter: &Arc<dyn PrCommenter>,
        db: &Arc<DatabaseConnection>,
        job: &Job,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        match job {
            Job::DeploymentCreated(e) => {
                let branch = match e.branch.clone() {
                    Some(b) if !b.is_empty() => b,
                    _ => return Ok(()),
                };
                let ctx = PreviewCommentContext {
                    project_id: e.project_id,
                    environment_id: e.environment_id,
                    branch,
                    phase: CommentPhase::Started {
                        commit_short_sha: short_sha(e.commit_sha.as_ref()),
                    },
                };
                if let Err(err) = commenter.upsert_preview_comment(ctx).await {
                    warn!(
                        deployment_id = e.deployment_id,
                        "PR comment (Started) failed: {}", err
                    );
                }
            }
            Job::DeploymentSucceeded(e) => {
                // Branch isn't in the event payload — fetch from deployment row.
                let deployment = match deployments::Entity::find_by_id(e.deployment_id)
                    .one(db.as_ref())
                    .await
                {
                    Ok(Some(d)) => d,
                    Ok(None) => {
                        debug!(
                            "PR comment listener: deployment {} not found, skipping",
                            e.deployment_id
                        );
                        return Ok(());
                    }
                    Err(err) => {
                        error!(
                            "PR comment listener: failed to load deployment {}: {}",
                            e.deployment_id, err
                        );
                        return Ok(());
                    }
                };
                let branch = match deployment.branch_ref.clone() {
                    Some(b) if !b.is_empty() => b,
                    _ => return Ok(()),
                };
                let env_url = match e.url.clone() {
                    Some(u) => u,
                    None => {
                        debug!(
                            "PR comment listener: deployment {} succeeded but no URL — skipping",
                            e.deployment_id
                        );
                        return Ok(());
                    }
                };
                let ctx = PreviewCommentContext {
                    project_id: e.project_id,
                    environment_id: e.environment_id,
                    branch,
                    phase: CommentPhase::Ready {
                        commit_short_sha: short_sha(deployment.commit_sha.as_ref()),
                        env_url,
                        deployment_url: None,
                    },
                };
                if let Err(err) = commenter.upsert_preview_comment(ctx).await {
                    warn!(
                        deployment_id = e.deployment_id,
                        "PR comment (Ready) failed: {}", err
                    );
                }
            }
            Job::DeploymentFailed(e) => {
                let deployment = match deployments::Entity::find_by_id(e.deployment_id)
                    .one(db.as_ref())
                    .await
                {
                    Ok(Some(d)) => d,
                    Ok(None) => return Ok(()),
                    Err(_) => return Ok(()),
                };
                let branch = match deployment.branch_ref.clone() {
                    Some(b) if !b.is_empty() => b,
                    _ => return Ok(()),
                };
                let ctx = PreviewCommentContext {
                    project_id: e.project_id,
                    environment_id: e.environment_id,
                    branch,
                    phase: CommentPhase::Failed {
                        commit_short_sha: short_sha(deployment.commit_sha.as_ref()),
                        deployment_url: None,
                    },
                };
                if let Err(err) = commenter.upsert_preview_comment(ctx).await {
                    warn!(
                        deployment_id = e.deployment_id,
                        "PR comment (Failed) failed: {}", err
                    );
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::pr_commenter::PrCommenterError;
    use super::*;
    use async_trait::async_trait;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::Mutex;
    use temps_core::{
        DeploymentCreatedJob, DeploymentFailedJob, DeploymentReadyJob, DeploymentSucceededJob,
    };

    #[test]
    fn short_sha_truncates_long_commit() {
        let long = "abcdef1234567890".to_string();
        assert_eq!(short_sha(Some(&long)), "abcdef1");
    }

    #[test]
    fn short_sha_handles_short_commit() {
        let short = "abc".to_string();
        assert_eq!(short_sha(Some(&short)), "abc");
    }

    #[test]
    fn short_sha_handles_missing_commit() {
        assert_eq!(short_sha(None), "unknown");
    }

    /// PrCommenter test double that records every context it's called with
    /// so individual tests can assert which phase the listener produced.
    struct RecordingCommenter {
        calls: Mutex<Vec<PreviewCommentContext>>,
    }

    impl RecordingCommenter {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
            })
        }

        fn calls(&self) -> Vec<PreviewCommentContext> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl PrCommenter for RecordingCommenter {
        async fn upsert_preview_comment(
            &self,
            ctx: PreviewCommentContext,
        ) -> Result<(), PrCommenterError> {
            self.calls.lock().unwrap().push(ctx);
            Ok(())
        }
    }

    fn empty_db() -> Arc<sea_orm::DatabaseConnection> {
        Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection())
    }

    fn deployment_with_branch(
        id: i32,
        branch: Option<&str>,
        commit: Option<&str>,
    ) -> temps_entities::deployments::Model {
        let now = chrono::Utc::now();
        temps_entities::deployments::Model {
            id,
            project_id: 1,
            environment_id: 1,
            created_at: now,
            updated_at: now,
            slug: format!("d-{id}"),
            state: "completed".to_string(),
            metadata: None,
            deploying_at: None,
            ready_at: None,
            started_at: None,
            finished_at: None,
            context_vars: None,
            branch_ref: branch.map(|s| s.to_string()),
            tag_ref: None,
            commit_sha: commit.map(|s| s.to_string()),
            commit_message: None,
            commit_author: None,
            commit_json: None,
            cancelled_reason: None,
            static_dir_location: None,
            screenshot_location: None,
            image_name: None,
            deployment_config: None,
            promoted_from_deployment_id: None,
        }
    }

    #[tokio::test]
    async fn created_event_with_branch_triggers_started_comment() {
        let recorder = RecordingCommenter::new();
        let commenter: Arc<dyn PrCommenter> = recorder.clone();
        let db = empty_db();

        let job = Job::DeploymentCreated(DeploymentCreatedJob {
            deployment_id: 10,
            project_id: 42,
            environment_id: 7,
            environment_name: "preview".into(),
            commit_sha: Some("abcdef1234567890".into()),
            branch: Some("feature/x".into()),
        });

        PrCommentListener::handle_job(&commenter, &db, &job)
            .await
            .unwrap();

        let calls = recorder.calls();

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].project_id, 42);
        assert_eq!(calls[0].environment_id, 7);
        assert_eq!(calls[0].branch, "feature/x");
        match &calls[0].phase {
            CommentPhase::Started { commit_short_sha } => {
                assert_eq!(commit_short_sha, "abcdef1");
            }
            _ => panic!("expected Started, got {:?}", calls[0].phase),
        }
    }

    #[tokio::test]
    async fn created_event_without_branch_skips_comment() {
        let recorder = RecordingCommenter::new();
        let commenter: Arc<dyn PrCommenter> = recorder.clone();
        let db = empty_db();

        let job = Job::DeploymentCreated(DeploymentCreatedJob {
            deployment_id: 10,
            project_id: 42,
            environment_id: 7,
            environment_name: "preview".into(),
            commit_sha: Some("abc".into()),
            branch: None,
        });

        PrCommentListener::handle_job(&commenter, &db, &job)
            .await
            .unwrap();

        let calls = recorder.calls();
        assert!(calls.is_empty(), "expected no comment, got {:?}", calls);
    }

    #[tokio::test]
    async fn created_event_with_empty_branch_skips_comment() {
        let recorder = RecordingCommenter::new();
        let commenter: Arc<dyn PrCommenter> = recorder.clone();
        let db = empty_db();

        let job = Job::DeploymentCreated(DeploymentCreatedJob {
            deployment_id: 10,
            project_id: 42,
            environment_id: 7,
            environment_name: "preview".into(),
            commit_sha: Some("abc".into()),
            branch: Some(String::new()),
        });

        PrCommentListener::handle_job(&commenter, &db, &job)
            .await
            .unwrap();

        let calls = recorder.calls();
        assert!(calls.is_empty());
    }

    #[tokio::test]
    async fn succeeded_event_with_url_and_branch_triggers_ready_comment() {
        let recorder = RecordingCommenter::new();
        let commenter: Arc<dyn PrCommenter> = recorder.clone();
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![deployment_with_branch(
                    10,
                    Some("feature/x"),
                    Some("abcdef1234"),
                )]])
                .into_connection(),
        );

        let job = Job::DeploymentSucceeded(DeploymentSucceededJob {
            deployment_id: 10,
            project_id: 42,
            environment_id: 7,
            environment_name: "preview".into(),
            commit_sha: Some("abcdef1234".into()),
            url: Some("https://feature-x.preview.temps.app".into()),
            health_check_path: None,
        });

        PrCommentListener::handle_job(&commenter, &db, &job)
            .await
            .unwrap();

        let calls = recorder.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].branch, "feature/x");
        match &calls[0].phase {
            CommentPhase::Ready {
                commit_short_sha,
                env_url,
                deployment_url,
            } => {
                assert_eq!(commit_short_sha, "abcdef1");
                assert_eq!(env_url, "https://feature-x.preview.temps.app");
                assert!(deployment_url.is_none());
            }
            _ => panic!("expected Ready, got {:?}", calls[0].phase),
        }
    }

    #[tokio::test]
    async fn succeeded_event_without_url_skips_comment() {
        let recorder = RecordingCommenter::new();
        let commenter: Arc<dyn PrCommenter> = recorder.clone();
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![deployment_with_branch(
                    10,
                    Some("feature/x"),
                    Some("abc"),
                )]])
                .into_connection(),
        );

        let job = Job::DeploymentSucceeded(DeploymentSucceededJob {
            deployment_id: 10,
            project_id: 42,
            environment_id: 7,
            environment_name: "preview".into(),
            commit_sha: Some("abc".into()),
            url: None,
            health_check_path: None,
        });

        PrCommentListener::handle_job(&commenter, &db, &job)
            .await
            .unwrap();

        let calls = recorder.calls();
        assert!(calls.is_empty());
    }

    #[tokio::test]
    async fn succeeded_event_with_missing_deployment_is_noop() {
        let recorder = RecordingCommenter::new();
        let commenter: Arc<dyn PrCommenter> = recorder.clone();
        // Empty query result -> deployment not found
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<temps_entities::deployments::Model>::new()])
                .into_connection(),
        );

        let job = Job::DeploymentSucceeded(DeploymentSucceededJob {
            deployment_id: 10,
            project_id: 42,
            environment_id: 7,
            environment_name: "preview".into(),
            commit_sha: Some("abc".into()),
            url: Some("https://x.example.com".into()),
            health_check_path: None,
        });

        PrCommentListener::handle_job(&commenter, &db, &job)
            .await
            .unwrap();

        let calls = recorder.calls();
        assert!(calls.is_empty());
    }

    #[tokio::test]
    async fn failed_event_with_branch_triggers_failed_comment() {
        let recorder = RecordingCommenter::new();
        let commenter: Arc<dyn PrCommenter> = recorder.clone();
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![deployment_with_branch(
                    10,
                    Some("feature/x"),
                    Some("abcdef1234"),
                )]])
                .into_connection(),
        );

        let job = Job::DeploymentFailed(DeploymentFailedJob {
            deployment_id: 10,
            project_id: 42,
            environment_id: 7,
            environment_name: "preview".into(),
            error_message: Some("build failed".into()),
        });

        PrCommentListener::handle_job(&commenter, &db, &job)
            .await
            .unwrap();

        let calls = recorder.calls();
        assert_eq!(calls.len(), 1);
        match &calls[0].phase {
            CommentPhase::Failed {
                commit_short_sha,
                deployment_url,
            } => {
                assert_eq!(commit_short_sha, "abcdef1");
                assert!(deployment_url.is_none());
            }
            _ => panic!("expected Failed, got {:?}", calls[0].phase),
        }
    }

    #[tokio::test]
    async fn unrelated_event_is_ignored() {
        let recorder = RecordingCommenter::new();
        let commenter: Arc<dyn PrCommenter> = recorder.clone();
        let db = empty_db();

        // DeploymentReady is not one of the events the listener subscribes to.
        let job = Job::DeploymentReady(DeploymentReadyJob {
            deployment_id: 10,
            project_id: 42,
            environment_id: 7,
            environment_name: "preview".into(),
            url: Some("https://x".into()),
        });

        PrCommentListener::handle_job(&commenter, &db, &job)
            .await
            .unwrap();

        let calls = recorder.calls();
        assert!(calls.is_empty());
    }
}
