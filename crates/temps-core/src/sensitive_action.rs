// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Extension point for authorizing security-sensitive control-plane actions.
//!
//! Domain crates classify an operation with [`SensitiveAction`] and ask the
//! registered [`SensitiveActionAuthorizer`] for a decision before mutating
//! state. The authorizer owns policy; callers do not need to know whether a
//! decision came from recent identity verification, the principal type, or a
//! stricter installation-specific rule.

use async_trait::async_trait;
use thiserror::Error;

/// A control-plane operation whose impact warrants an additional policy check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SensitiveAction {
    CreateApiKey,
    RotateApiKey {
        api_key_id: i32,
    },
    DeleteEnvironment {
        project_id: i32,
        environment_id: i32,
    },
    DrainNode {
        node_id: i32,
    },
    CreateOidcProvider,
    UpdateOidcProvider {
        provider_id: i32,
    },
    CreateOidcRoleMapping {
        provider_id: i32,
    },
    DeleteOidcRoleMapping {
        mapping_id: i32,
    },
    AssignRole {
        user_id: i32,
    },
    UpdateAccountEmail,
    RestoreExternalService {
        service_id: i32,
    },
    DeleteBackup {
        backup_id: String,
    },
    RollbackPgUpgrade {
        service_id: i32,
        upgrade_id: i32,
    },
    DeleteTeam {
        team_id: i32,
    },
    DeleteDomain {
        domain: String,
    },
    DeleteGitProvider {
        provider_id: i32,
    },
    DeleteGitConnection {
        connection_id: i32,
    },
    RotateDeploymentToken {
        project_id: i32,
        token_id: i32,
    },
    DeleteDeploymentToken {
        project_id: i32,
        token_id: i32,
    },
    /// Retroactively backfill retention-days on already-ingested telemetry
    /// rows (spans, proxy_logs) for a project down to the configured policy.
    /// This is a hard-to-reverse purge of historical data (EE retention
    /// policies, ADR 0017 §7).
    RetentionRetroactiveApply {
        project_id: i32,
    },
}

impl SensitiveAction {
    /// Stable identifier suitable for API responses, policy matching, and
    /// audit records.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CreateApiKey => "create_api_key",
            Self::RotateApiKey { .. } => "rotate_api_key",
            Self::DeleteEnvironment { .. } => "delete_environment",
            Self::DrainNode { .. } => "drain_node",
            Self::CreateOidcProvider => "create_oidc_provider",
            Self::UpdateOidcProvider { .. } => "update_oidc_provider",
            Self::CreateOidcRoleMapping { .. } => "create_oidc_role_mapping",
            Self::DeleteOidcRoleMapping { .. } => "delete_oidc_role_mapping",
            Self::AssignRole { .. } => "assign_role",
            Self::UpdateAccountEmail => "update_account_email",
            Self::RestoreExternalService { .. } => "restore_external_service",
            Self::DeleteBackup { .. } => "delete_backup",
            Self::RollbackPgUpgrade { .. } => "rollback_pg_upgrade",
            Self::DeleteTeam { .. } => "delete_team",
            Self::DeleteDomain { .. } => "delete_domain",
            Self::DeleteGitProvider { .. } => "delete_git_provider",
            Self::DeleteGitConnection { .. } => "delete_git_connection",
            Self::RotateDeploymentToken { .. } => "rotate_deployment_token",
            Self::DeleteDeploymentToken { .. } => "delete_deployment_token",
            Self::RetentionRetroactiveApply { .. } => "retention_retroactive_apply",
        }
    }
}

/// Authentication principal requesting a sensitive operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SensitiveActionPrincipal {
    UserSession {
        user_id: i32,
        session_id: i32,
        mfa_enabled: bool,
    },
    ApiKey {
        user_id: i32,
        key_id: i32,
    },
    CliToken {
        user_id: i32,
    },
    DeploymentToken {
        token_id: i32,
    },
}

/// Result of evaluating an action against the active policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SensitiveActionDecision {
    Allow,
    RequireVerification {
        /// Whether the user must enroll an MFA method before verification can
        /// succeed. This lets clients present an actionable setup path.
        mfa_setup_required: bool,
    },
    Deny {
        reason: String,
    },
}

/// Infrastructure failure while evaluating a sensitive action.
#[derive(Debug, Error)]
#[error("Failed to authorize sensitive action '{action}': {reason}")]
pub struct SensitiveActionAuthorizationError {
    pub action: &'static str,
    pub reason: String,
}

/// Policy boundary for sensitive control-plane mutations.
///
/// Implementations are registered as `Arc<dyn SensitiveActionAuthorizer>` in
/// the service registry. Callers fail closed when evaluation returns an error.
#[async_trait]
pub trait SensitiveActionAuthorizer: Send + Sync {
    async fn authorize(
        &self,
        action: &SensitiveAction,
        principal: &SensitiveActionPrincipal,
    ) -> Result<SensitiveActionDecision, SensitiveActionAuthorizationError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_identifiers_are_stable_and_resource_independent() {
        assert_eq!(SensitiveAction::CreateApiKey.as_str(), "create_api_key");
        assert_eq!(
            SensitiveAction::RotateApiKey { api_key_id: 13 }.as_str(),
            "rotate_api_key"
        );
        assert_eq!(
            SensitiveAction::DeleteEnvironment {
                project_id: 7,
                environment_id: 11,
            }
            .as_str(),
            "delete_environment"
        );
        assert_eq!(
            SensitiveAction::DrainNode { node_id: 3 }.as_str(),
            "drain_node"
        );
        assert_eq!(
            SensitiveAction::CreateOidcProvider.as_str(),
            "create_oidc_provider"
        );
        assert_eq!(
            SensitiveAction::UpdateOidcProvider { provider_id: 1 }.as_str(),
            "update_oidc_provider"
        );
        assert_eq!(
            SensitiveAction::CreateOidcRoleMapping { provider_id: 1 }.as_str(),
            "create_oidc_role_mapping"
        );
        assert_eq!(
            SensitiveAction::DeleteOidcRoleMapping { mapping_id: 1 }.as_str(),
            "delete_oidc_role_mapping"
        );
        assert_eq!(
            SensitiveAction::AssignRole { user_id: 1 }.as_str(),
            "assign_role"
        );
        assert_eq!(
            SensitiveAction::UpdateAccountEmail.as_str(),
            "update_account_email"
        );
        assert_eq!(
            SensitiveAction::RestoreExternalService { service_id: 1 }.as_str(),
            "restore_external_service"
        );
        assert_eq!(
            SensitiveAction::DeleteBackup {
                backup_id: "b1".to_string()
            }
            .as_str(),
            "delete_backup"
        );
        assert_eq!(
            SensitiveAction::RollbackPgUpgrade {
                service_id: 1,
                upgrade_id: 2,
            }
            .as_str(),
            "rollback_pg_upgrade"
        );
        assert_eq!(
            SensitiveAction::DeleteTeam { team_id: 1 }.as_str(),
            "delete_team"
        );
        assert_eq!(
            SensitiveAction::DeleteDomain {
                domain: "example.com".to_string()
            }
            .as_str(),
            "delete_domain"
        );
        assert_eq!(
            SensitiveAction::DeleteGitProvider { provider_id: 1 }.as_str(),
            "delete_git_provider"
        );
        assert_eq!(
            SensitiveAction::DeleteGitConnection { connection_id: 1 }.as_str(),
            "delete_git_connection"
        );
        assert_eq!(
            SensitiveAction::RotateDeploymentToken {
                project_id: 1,
                token_id: 2,
            }
            .as_str(),
            "rotate_deployment_token"
        );
        assert_eq!(
            SensitiveAction::DeleteDeploymentToken {
                project_id: 1,
                token_id: 2,
            }
            .as_str(),
            "delete_deployment_token"
        );
        assert_eq!(
            SensitiveAction::RetentionRetroactiveApply { project_id: 42 }.as_str(),
            "retention_retroactive_apply"
        );
    }

    /// The retention-retroactive-apply variant must round-trip through
    /// `as_str()` independent of which `project_id` it carries — the
    /// identifier is used for policy matching and audit records, so it must
    /// not vary with the resource the action targets.
    #[test]
    fn retention_retroactive_apply_identifier_is_resource_independent() {
        assert_eq!(
            SensitiveAction::RetentionRetroactiveApply { project_id: 1 }.as_str(),
            SensitiveAction::RetentionRetroactiveApply { project_id: 999 }.as_str(),
        );
    }
}
