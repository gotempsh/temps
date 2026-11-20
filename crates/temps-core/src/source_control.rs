// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The repository a project deploys from, seen from outside `temps-git`.
//!
//! Error tracking (ADR 111, D5) opens issues in that repository and reads
//! which commits touched a file, through the git provider connection the
//! project already uses for deployments. `temps-git` implements this trait;
//! a binary without it has no source control and callers say so.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;

/// Providers whose issues and commits this trait can reach.
pub const ISSUE_PROVIDERS: [&str; 2] = ["github", "gitlab"];

#[derive(Debug, Error)]
pub enum SourceControlError {
    #[error("Project {project_id} is not connected to a repository through a git provider")]
    NotConnected { project_id: i32 },
    #[error("Project {project_id} deploys from a {provider} repository; issues and commits are read from GitHub and GitLab only")]
    Unsupported { project_id: i32, provider: String },
    #[error("{provider} refused {operation} on {repository} for project {project_id} (HTTP {status}): the connection's token lacks the permission or was revoked")]
    Forbidden {
        project_id: i32,
        provider: String,
        repository: String,
        operation: &'static str,
        status: u16,
    },
    #[error("{provider} has no {what} in {repository} for project {project_id}")]
    NotFound {
        project_id: i32,
        provider: String,
        repository: String,
        what: String,
    },
    #[error("{provider} failed {operation} on {repository} for project {project_id}: {reason}")]
    Provider {
        project_id: i32,
        provider: String,
        repository: String,
        operation: &'static str,
        reason: String,
    },
    #[error("Source control lookup for project {project_id} failed: {reason}")]
    Lookup { project_id: i32, reason: String },
}

/// The repository of a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRepository {
    /// `github`, `gitlab`, or another provider type.
    pub provider: String,
    /// `owner/name`.
    pub repository: String,
    /// The repository's page, e.g. `https://github.com/owner/name`.
    pub web_url: String,
    /// The branch deployments use.
    pub default_branch: String,
}

impl ProjectRepository {
    /// Whether issues and commits of this repository can be reached.
    pub fn supported(&self) -> bool {
        ISSUE_PROVIDERS.contains(&self.provider.as_str())
    }
}

/// Whether an issue is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueState {
    Open,
    Closed,
}

impl IssueState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
        }
    }
}

/// An issue in the project's repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryIssue {
    /// The issue number (GitLab's `iid`).
    pub number: i64,
    pub url: String,
    pub title: String,
    pub state: IssueState,
}

/// A commit that touched a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryCommit {
    pub sha: String,
    /// The first line of the message.
    pub summary: String,
    pub author_name: Option<String>,
    pub authored_at: Option<DateTime<Utc>>,
    pub url: String,
}

#[async_trait]
pub trait ProjectSourceControl: Send + Sync {
    /// The project's repository, or `None` when it has no git provider
    /// connection (a public URL, an upload, an image).
    async fn repository(
        &self,
        project_id: i32,
    ) -> Result<Option<ProjectRepository>, SourceControlError>;

    /// Open an issue in the project's repository.
    async fn create_issue(
        &self,
        project_id: i32,
        title: &str,
        body: &str,
    ) -> Result<RepositoryIssue, SourceControlError>;

    /// Read an issue of the project's repository.
    async fn issue(
        &self,
        project_id: i32,
        number: i64,
    ) -> Result<RepositoryIssue, SourceControlError>;

    /// The newest commits, at most `limit`, that touched `path` in the
    /// history of `at` (a commit or branch; the default branch when `None`).
    async fn commits_touching(
        &self,
        project_id: i32,
        path: &str,
        at: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RepositoryCommit>, SourceControlError>;
}
