use anyhow::Result;
use serde::Serialize;
use temps_core::{AuditContext, AuditOperation};

// Login audit
#[derive(Debug, Clone, Serialize)]
pub struct LoginAudit {
    pub context: AuditContext,
    pub success: bool,
    pub login_method: String,
}

/// Recorded when a user completes a successful login while at least one
/// other session for their account is already active (non-expired). This is
/// purely observational -- it does NOT block the login (bherila/temps#24) --
/// but gives an auditor/operator a trail to spot suspicious concurrent
/// access (e.g. a stolen session token being used from a second location).
#[derive(Debug, Clone, Serialize)]
pub struct ConcurrentSessionDetectedAudit {
    pub context: AuditContext,
    pub login_method: String,
    /// Number of other active sessions that already existed for this user
    /// at the moment this login completed (not counting the new session).
    pub existing_active_session_count: u64,
}

// User management audits
#[derive(Debug, Clone, Serialize)]
pub struct UserCreatedAudit {
    pub context: AuditContext,
    pub target_user_id: i32,
    pub username: String,
    pub assigned_roles: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserUpdatedAudit {
    pub context: AuditContext,
    pub target_user_id: i32,
    pub username: String,
    pub new_values: UpdatedFields,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdatedFields {
    pub email: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserDeletedAudit {
    pub context: AuditContext,
    pub target_user_id: i32,
    pub username: String,
    pub email: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserRestoredAudit {
    pub context: AuditContext,
    pub target_user_id: i32,
    pub username: String,
    pub email: String,
    pub name: String,
}

// Role management audits
#[derive(Debug, Clone, Serialize)]
pub struct RoleAssignedAudit {
    pub context: AuditContext,
    pub username: String,
    pub target_user_id: i32,
    pub role: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoleRemovedAudit {
    pub context: AuditContext,
    pub username: String,
    pub target_user_id: i32,
    pub role: String,
}

// MFA audits
#[derive(Debug, Clone, Serialize)]
pub struct MfaEnabledAudit {
    pub context: AuditContext,
    pub username: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MfaDisabledAudit {
    pub context: AuditContext,
    pub username: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MfaVerifiedAudit {
    pub context: AuditContext,
    pub username: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogoutAudit {
    pub context: AuditContext,
    pub username: String,
}

// Password reset audit (out-of-band email-link flow)
#[derive(Debug, Clone, Serialize)]
pub struct PasswordResetAudit {
    pub context: AuditContext,
    pub username: String,
}

// In-app password change for an authenticated user. `other_sessions_revoked`
// reflects whether the operator opted to invalidate every other session on
// submit; useful for auditors trying to reconstruct "did this user lose
// access on every device on date X" after a credential rotation.
#[derive(Debug, Clone, Serialize)]
pub struct PasswordChangedAudit {
    pub context: AuditContext,
    pub username: String,
    pub other_sessions_revoked: bool,
}

// Email verification audit
#[derive(Debug, Clone, Serialize)]
pub struct EmailVerifiedAudit {
    pub context: AuditContext,
    pub username: String,
    pub email: String,
}

// API key creation audit. Custom-permission keys are the highest-impact
// variant -- capturing role_type and the granted permission strings lets an
// auditor spot an over-broad grant after the fact without needing to
// correlate against the key's current (mutable) state.
#[derive(Debug, Clone, Serialize)]
pub struct ApiKeyCreatedAudit {
    pub context: AuditContext,
    pub api_key_id: i32,
    pub api_key_name: String,
    pub role_type: String,
    pub permissions: Option<Vec<String>>,
}

// API key rotation audit. Rotation invalidates the old secret and mints a new
// one for the same key record -- an auditor needs the key's id/name to tie
// this event to the credential's lifecycle without seeing the secret itself.
#[derive(Debug, Clone, Serialize)]
pub struct ApiKeyRotatedAudit {
    pub context: AuditContext,
    pub api_key_id: i32,
    pub api_key_name: String,
}

// OIDC provider configuration audits. SSO provider config is one of
// the highest-impact settings in the system — it controls who can log
// in and with what role — so every mutation gets a row. The diff-shape
// payloads (PATCH only carries deltas, DELETE only IDs) are
// intentional: an auditor reconstructing "who changed what when"
// shouldn't have to read source to interpret the entry.
#[derive(Debug, Clone, Serialize)]
pub struct OidcProviderCreatedAudit {
    pub context: AuditContext,
    pub provider_id: i32,
    pub name: String,
    pub issuer_url: String,
    pub template: String,
    pub enabled: bool,
    pub jit_provisioning: bool,
    /// Records whether the provider was created with the
    /// `email_verified` claim gate disabled. Worth a dedicated field
    /// in the audit row because it's the single most security-relevant
    /// knob on an OIDC provider — an auditor reviewing "why did a
    /// non-Okta user get an account auto-linked?" should be able to
    /// answer it from this payload alone.
    pub trust_idp_email: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct OidcProviderUpdatedAudit {
    pub context: AuditContext,
    pub provider_id: i32,
    pub name: String,
    /// Names of fields included in the PATCH. We do NOT log the new
    /// values (issuer URLs, client IDs) here — the provider row
    /// itself is the source of truth post-change; the audit row just
    /// needs to prove that someone touched it. Critically, this list
    /// reveals whether `client_secret` was rotated, which is the
    /// single most interesting forensic question.
    pub fields_changed: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OidcProviderDeletedAudit {
    pub context: AuditContext,
    pub provider_id: i32,
    pub name: String,
    pub issuer_url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OidcRoleMappingCreatedAudit {
    pub context: AuditContext,
    pub provider_id: i32,
    pub mapping_id: i32,
    pub idp_group: String,
    pub role: String,
    pub priority: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct OidcRoleMappingDeletedAudit {
    pub context: AuditContext,
    pub mapping_id: i32,
}

// Implement AuditOperation for each struct
impl AuditOperation for LoginAudit {
    fn operation_type(&self) -> String {
        format!("LOGIN_{}", if self.success { "SUCCESS" } else { "FAILURE" })
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
    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

impl AuditOperation for ConcurrentSessionDetectedAudit {
    fn operation_type(&self) -> String {
        "CONCURRENT_SESSION_DETECTED".to_string()
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

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

impl AuditOperation for UserCreatedAudit {
    fn operation_type(&self) -> String {
        "USER_CREATED".to_string()
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

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

impl AuditOperation for RoleAssignedAudit {
    fn operation_type(&self) -> String {
        "ROLE_ASSIGNED".to_string()
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

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

impl AuditOperation for RoleRemovedAudit {
    fn operation_type(&self) -> String {
        "ROLE_REMOVED".to_string()
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

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

impl AuditOperation for UserUpdatedAudit {
    fn operation_type(&self) -> String {
        "USER_UPDATED".to_string()
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

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

impl AuditOperation for UserDeletedAudit {
    fn operation_type(&self) -> String {
        "USER_DELETED".to_string()
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

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

impl AuditOperation for UserRestoredAudit {
    fn operation_type(&self) -> String {
        "USER_RESTORED".to_string()
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

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

impl AuditOperation for MfaEnabledAudit {
    fn operation_type(&self) -> String {
        "MFA_ENABLED".to_string()
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

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

impl AuditOperation for MfaDisabledAudit {
    fn operation_type(&self) -> String {
        "MFA_DISABLED".to_string()
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

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

impl AuditOperation for MfaVerifiedAudit {
    fn operation_type(&self) -> String {
        "MFA_VERIFIED".to_string()
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

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

impl AuditOperation for LogoutAudit {
    fn operation_type(&self) -> String {
        "USER_LOGOUT".to_string()
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

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

impl AuditOperation for PasswordResetAudit {
    fn operation_type(&self) -> String {
        "PASSWORD_RESET".to_string()
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

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

impl AuditOperation for PasswordChangedAudit {
    fn operation_type(&self) -> String {
        "PASSWORD_CHANGED".to_string()
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

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

impl AuditOperation for EmailVerifiedAudit {
    fn operation_type(&self) -> String {
        "EMAIL_VERIFIED".to_string()
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

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

impl AuditOperation for ApiKeyCreatedAudit {
    fn operation_type(&self) -> String {
        "API_KEY_CREATED".to_string()
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

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

impl AuditOperation for ApiKeyRotatedAudit {
    fn operation_type(&self) -> String {
        "API_KEY_ROTATED".to_string()
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

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

// Shared boilerplate for the five OIDC admin audits below. Each just
// forwards through `self.context` and emits its own operation type.
macro_rules! impl_oidc_audit_op {
    ($ty:ty, $op:literal) => {
        impl AuditOperation for $ty {
            fn operation_type(&self) -> String {
                $op.to_string()
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

            fn serialize(&self) -> Result<String> {
                serde_json::to_string(self)
                    .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
            }
        }
    };
}

impl_oidc_audit_op!(OidcProviderCreatedAudit, "OIDC_PROVIDER_CREATED");
impl_oidc_audit_op!(OidcProviderUpdatedAudit, "OIDC_PROVIDER_UPDATED");
impl_oidc_audit_op!(OidcProviderDeletedAudit, "OIDC_PROVIDER_DELETED");
impl_oidc_audit_op!(OidcRoleMappingCreatedAudit, "OIDC_ROLE_MAPPING_CREATED");
impl_oidc_audit_op!(OidcRoleMappingDeletedAudit, "OIDC_ROLE_MAPPING_DELETED");

/// Recorded when a login attempt is rejected before a session exists.
///
/// Unlike most audit events the actor may be unknown (an attempt against an
/// email with no account), so this deliberately does not embed an
/// [`AuditContext`]: `user_id` is optional, and for unknown accounts the
/// audit row's user reference stays NULL while `attempted_email` preserves
/// the identity the caller claimed.
#[derive(Debug, Clone, Serialize)]
pub struct LoginFailedAudit {
    /// Account the attempt resolved to, when it resolved at all.
    pub user_id: Option<i32>,
    /// Identity the caller claimed (lowercased email as submitted).
    pub attempted_email: String,
    pub ip_address: Option<String>,
    pub user_agent: String,
    pub login_method: String,
    /// Why the attempt was rejected, e.g. "invalid_credentials",
    /// "mfa_required_for_role", "session_creation_failed", "internal_error".
    pub reason: String,
}

impl AuditOperation for LoginFailedAudit {
    fn operation_type(&self) -> String {
        "LOGIN_FAILURE".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        self.user_id
    }

    fn ip_address(&self) -> Option<String> {
        self.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

/// Recorded when an MFA challenge verification is rejected (wrong or expired
/// code, or an invalid/expired challenge session). The account is not always
/// resolvable at this point, so the actor is optional like [`LoginFailedAudit`].
#[derive(Debug, Clone, Serialize)]
pub struct MfaVerificationFailedAudit {
    /// Account the challenge belonged to, when it could be resolved.
    pub user_id: Option<i32>,
    pub ip_address: Option<String>,
    pub user_agent: String,
    /// Why verification was rejected, e.g. "invalid_code",
    /// "invalid_or_expired_session", or "missing_session_cookie".
    pub reason: String,
}

impl AuditOperation for MfaVerificationFailedAudit {
    fn operation_type(&self) -> String {
        "MFA_VERIFICATION_FAILED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        self.user_id
    }

    fn ip_address(&self) -> Option<String> {
        self.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

#[cfg(test)]
mod failure_audit_tests {
    use super::*;

    #[test]
    fn test_login_failed_audit_unknown_account_has_no_actor() {
        let audit = LoginFailedAudit {
            user_id: None,
            attempted_email: "nobody@example.com".to_string(),
            ip_address: Some("203.0.113.9".to_string()),
            user_agent: "test-agent".to_string(),
            login_method: "password".to_string(),
            reason: "invalid_credentials".to_string(),
        };
        assert_eq!(audit.operation_type(), "LOGIN_FAILURE");
        assert_eq!(audit.user_id(), None);
        let json = AuditOperation::serialize(&audit).expect("LoginFailedAudit serializes");
        assert!(json.contains("nobody@example.com"));
        assert!(json.contains("invalid_credentials"));
    }

    #[test]
    fn test_login_failed_audit_known_account_keeps_actor() {
        let audit = LoginFailedAudit {
            user_id: Some(7),
            attempted_email: "admin@example.com".to_string(),
            ip_address: None,
            user_agent: "test-agent".to_string(),
            login_method: "password".to_string(),
            reason: "mfa_required_for_role".to_string(),
        };
        assert_eq!(audit.user_id(), Some(7));
    }

    #[test]
    fn test_mfa_verification_failed_audit_shape() {
        let audit = MfaVerificationFailedAudit {
            user_id: None,
            ip_address: Some("203.0.113.9".to_string()),
            user_agent: "test-agent".to_string(),
            reason: "invalid_code".to_string(),
        };
        assert_eq!(audit.operation_type(), "MFA_VERIFICATION_FAILED");
        assert_eq!(audit.user_id(), None);
    }

    #[test]
    fn test_existing_context_audits_keep_known_actor() {
        let audit = LoginAudit {
            context: AuditContext {
                user_id: 42,
                ip_address: None,
                user_agent: "test-agent".to_string(),
            },
            success: true,
            login_method: "password".to_string(),
        };
        assert_eq!(audit.user_id(), Some(42));
    }
}
