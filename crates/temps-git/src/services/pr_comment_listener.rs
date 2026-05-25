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
    use super::*;

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
}
