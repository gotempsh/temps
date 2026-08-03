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
}

impl SensitiveAction {
    /// Stable identifier suitable for API responses, policy matching, and
    /// audit records.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CreateApiKey => "create_api_key",
            Self::RotateApiKey { .. } => "rotate_api_key",
            Self::DeleteEnvironment { .. } => "delete_environment",
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
    }
}
