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

    fn user_id(&self) -> i32 {
        self.context.user_id
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

    fn user_id(&self) -> i32 {
        self.context.user_id
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

    fn user_id(&self) -> i32 {
        self.context.user_id
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

    fn user_id(&self) -> i32 {
        self.context.user_id
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

    fn user_id(&self) -> i32 {
        self.context.user_id
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

    fn user_id(&self) -> i32 {
        self.context.user_id
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

    fn user_id(&self) -> i32 {
        self.context.user_id
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

    fn user_id(&self) -> i32 {
        self.context.user_id
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

    fn user_id(&self) -> i32 {
        self.context.user_id
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

    fn user_id(&self) -> i32 {
        self.context.user_id
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

    fn user_id(&self) -> i32 {
        self.context.user_id
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

    fn user_id(&self) -> i32 {
        self.context.user_id
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

    fn user_id(&self) -> i32 {
        self.context.user_id
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

    fn user_id(&self) -> i32 {
        self.context.user_id
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

    fn user_id(&self) -> i32 {
        self.context.user_id
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

    fn user_id(&self) -> i32 {
        self.context.user_id
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

            fn user_id(&self) -> i32 {
                self.context.user_id
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
