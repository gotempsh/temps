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

/// The provider's base URL as it may be audited: any username or password
/// embedded in it (`https://user:secret@git.example.com`) is masked, because
/// audit details are stored in plain text.
pub(crate) fn audited_base_url(base_url: Option<&str>) -> Option<String> {
    base_url.map(temps_core::url_validation::redact_url_password)
}

/// A git provider was created with a credential, or had one replaced.
///
/// Records which provider, how it authenticates, which kinds of credential
/// were set and whether the AI assistant issued the request. Never a value:
/// `credentials_set` holds field names only.
#[derive(Debug, Clone, Serialize)]
pub struct GitProviderCredentialAudit {
    #[serde(skip)]
    pub action: &'static str,
    pub context: AuditContext,
    pub provider_id: i32,
    pub provider_name: String,
    pub provider_type: String,
    pub auth_method: String,
    pub base_url: Option<String>,
    pub credentials_set: Vec<&'static str>,
    pub via_ai_assistant: bool,
}

impl AuditOperation for GitProviderCredentialAudit {
    fn operation_type(&self) -> String {
        self.action.to_string()
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

#[cfg(test)]
mod credential_audit_tests {
    use super::*;

    #[test]
    fn a_base_url_is_audited_without_embedded_credentials() {
        let audited = audited_base_url(Some("https://someone:fixture-secret@git.example.com/api"))
            .expect("a base URL stays recorded");
        assert!(!audited.contains("fixture-secret"), "{audited}");
        assert!(!audited.contains("someone"), "{audited}");
        assert!(audited.contains("git.example.com"), "{audited}");
        assert_eq!(
            audited_base_url(Some("https://git.example.com")).as_deref(),
            Some("https://git.example.com/")
        );
        assert_eq!(audited_base_url(None), None);
    }

    #[test]
    fn provider_credential_audit_names_fields_not_values() {
        let audit = GitProviderCredentialAudit {
            action: "GIT_PROVIDER_CREDENTIALS_UPDATED",
            context: AuditContext {
                user_id: 4,
                ip_address: Some("192.0.2.10".into()),
                user_agent: "Mozilla/5.0 (via Temps AI assistant)".into(),
            },
            provider_id: 9,
            provider_name: "Team Git".into(),
            provider_type: "gitlab".into(),
            auth_method: "pat".into(),
            base_url: Some("https://git.example.com".into()),
            credentials_set: vec!["token"],
            via_ai_assistant: true,
        };
        assert_eq!(audit.operation_type(), "GIT_PROVIDER_CREDENTIALS_UPDATED");
        let json: serde_json::Value =
            serde_json::from_str(&AuditOperation::serialize(&audit).expect("serializes"))
                .expect("json");
        assert_eq!(json["provider_id"], 9);
        assert_eq!(json["credentials_set"][0], "token");
        assert_eq!(json["via_ai_assistant"], true);
        assert!(json.get("action").is_none());
    }
}
