// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::Serialize;
use temps_core::AuditOperation;

#[derive(Debug, Clone, Serialize)]
pub struct AuditContext {
    pub user_id: i32,
    pub ip_address: Option<String>,
    pub user_agent: String,
}

// Add this after the other audit structs
#[derive(Debug, Clone, Serialize)]
pub struct PipelineTriggeredAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub project_slug: String,
    pub environment_id: i32,
    pub environment_slug: String,
    pub branch: Option<String>,
    pub tag: Option<String>,
    pub commit: Option<String>,
}

impl AuditOperation for PipelineTriggeredAudit {
    fn operation_type(&self) -> String {
        "PIPELINE_TRIGGERED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

/// A repository was created on a git provider through one of the user's
/// connections. Records who, where, and the visibility chosen, never a token.
#[derive(Debug, Clone, Serialize)]
pub struct GitRepositoryCreatedAudit {
    pub context: AuditContext,
    pub connection_id: i32,
    pub repository_full_name: String,
    pub private: bool,
}

impl AuditOperation for GitRepositoryCreatedAudit {
    fn operation_type(&self) -> String {
        "GIT_REPOSITORY_CREATED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

/// Files were committed to a repository through one of the user's connections.
/// Records where and how much, never file contents or a token.
#[derive(Debug, Clone, Serialize)]
pub struct GitFilesCommittedAudit {
    pub context: AuditContext,
    pub connection_id: i32,
    pub repository_full_name: String,
    pub branch: String,
    pub commit_sha: String,
    pub file_count: usize,
}

impl AuditOperation for GitFilesCommittedAudit {
    fn operation_type(&self) -> String {
        "GIT_FILES_COMMITTED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

/// A pull request was opened on a repository through one of the user's connections.
#[derive(Debug, Clone, Serialize)]
pub struct GitPullRequestOpenedAudit {
    pub context: AuditContext,
    pub connection_id: i32,
    pub repository_full_name: String,
    pub head_branch: String,
    pub base_branch: String,
    pub pull_request_number: i32,
}

impl AuditOperation for GitPullRequestOpenedAudit {
    fn operation_type(&self) -> String {
        "GIT_PULL_REQUEST_OPENED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

#[derive(Serialize)]
pub(super) struct GitConnectionReauthorizationAudit {
    pub context: AuditContext,
    pub connection_id: i32,
    pub provider_id: i32,
    pub completed: bool,
}

impl AuditOperation for GitConnectionReauthorizationAudit {
    fn operation_type(&self) -> String {
        if self.completed {
            "GIT_CONNECTION_REAUTHORIZED"
        } else {
            "GIT_CONNECTION_REAUTHORIZATION_STARTED"
        }
        .to_string()
    }
    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }
    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }
    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }
    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

/// An operator started creating a GitHub App from a manifest, which is what
/// lets the callback accept the resulting code. Records who and from where,
/// never the state that was issued.
#[derive(Debug, Clone, Serialize)]
pub struct GithubAppSetupStartedAudit {
    pub context: AuditContext,
}

impl AuditOperation for GithubAppSetupStartedAudit {
    fn operation_type(&self) -> String {
        "GITHUB_APP_SETUP_STARTED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

/// A GitHub App was created from a manifest and registered as a git provider.
/// The callback carries no session, so the operator is the one the
/// server-issued setup state was issued to. Records the provider and the app,
/// never the app's credentials.
#[derive(Debug, Clone, Serialize)]
pub struct GithubAppCreatedAudit {
    pub context: AuditContext,
    pub provider_id: i32,
    pub app_id: i32,
    pub app_name: String,
}

impl AuditOperation for GithubAppCreatedAudit {
    fn operation_type(&self) -> String {
        "GITHUB_APP_CREATED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}
