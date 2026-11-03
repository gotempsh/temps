// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build a [`DownloadRepoJob`] from its `deployment_jobs` row.

use super::super::is_concrete_commit_ref;
use super::{FromJobRecord, JobRecord, WorkflowExecutionError};
use crate::jobs::{DownloadRepoBuilder, DownloadRepoJob};
use async_trait::async_trait;

#[async_trait]
impl FromJobRecord for DownloadRepoJob {
    const JOB_TYPES: &'static [&'static str] = &["DownloadRepoJob"];

    async fn from_record(record: &JobRecord<'_>) -> Result<Self, WorkflowExecutionError> {
        let JobRecord {
            service,
            deployment,
            db_job,
            ..
        } = *record;

        let config = record.config()?;

        let repo_owner = config
            .get("repo_owner")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                WorkflowExecutionError::InvalidJobConfig("repo_owner missing".to_string())
            })?;

        let repo_name = config
            .get("repo_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                WorkflowExecutionError::InvalidJobConfig("repo_name missing".to_string())
            })?;

        // Check if this is a public repo
        let is_public_repo = config
            .get("is_public_repo")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Get git_url for public repos
        let git_url = config
            .get("git_url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Get connection_id for private repos (optional for public repos)
        let connection_id = config
            .get("git_provider_connection_id")
            .and_then(|v| v.as_i64())
            .map(|v| v as i32);

        // Validate: public repos need git_url, private repos need connection_id
        if is_public_repo && git_url.is_none() {
            return Err(WorkflowExecutionError::InvalidJobConfig(
                "git_url is required for public repositories".to_string(),
            ));
        }
        if !is_public_repo && connection_id.is_none() {
            return Err(WorkflowExecutionError::InvalidJobConfig(
                "git_provider_connection_id is required for private repositories".to_string(),
            ));
        }

        // Get branch_ref from job config (set by workflow planner based on deployment)
        // Fallback to project.main_branch if not specified
        let branch_ref = config
            .get("branch_ref")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| deployment.branch_ref.clone().unwrap_or("main".to_string()));

        // Get tag_ref from job config (for tag-based deployments)
        let tag_ref = config
            .get("tag_ref")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Get commit_sha from job config (for specific commit
        // deployments). An empty string is "no commit", never a ref —
        // older deployments stored '' for manual triggers.
        let commit_sha = config
            .get("commit_sha")
            .and_then(|v| v.as_str())
            .filter(|s| is_concrete_commit_ref(s))
            .map(|s| s.to_string());

        let mut builder = DownloadRepoBuilder::new()
            .job_id(db_job.job_id.clone())
            .repo_owner(repo_owner.to_string())
            .repo_name(repo_name.to_string())
            .is_public_repo(is_public_repo)
            .branch_ref(branch_ref)
            .log_id(db_job.log_id.clone())
            .log_service(service.log_service.clone())
            .db(service.db.clone());

        // Add git_url for public repos
        if let Some(url) = git_url {
            builder = builder.git_url(url);
        }

        // Add connection_id for private repos
        if let Some(id) = connection_id {
            builder = builder.git_provider_connection_id(id);
        }

        // Add tag_ref if present (highest priority in checkout)
        if let Some(tag) = tag_ref {
            builder = builder.tag_ref(tag);
        }

        // Add commit_sha if present (second priority in checkout)
        if let Some(commit) = commit_sha {
            builder = builder.commit_sha(commit);
        }

        if let Some(directory) = config.get("directory").and_then(|v| v.as_str()) {
            builder = builder.project_directory(directory.to_string());
        }
        if config
            .get("pull_only_root_directory")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            builder = builder.pull_only_root_directory(true);
        }

        let job = builder.build(service.git_provider.clone())?;

        Ok(job)
    }
}
