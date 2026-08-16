use super::permissions::{Permission, Role};
use serde::{Deserialize, Serialize};
use temps_entities::deployment_tokens::DeploymentTokenPermission;
use temps_entities::users;
use utoipa::ToSchema;

/// Info extracted from a deployment token auth source.
#[derive(Debug, Clone)]
pub struct DeploymentTokenInfo {
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub token_id: i32,
    pub token_name: String,
}

// Simplified user schema for OpenAPI documentation
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UserSchema {
    pub id: i32,
    pub email: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuthSource {
    Session {
        user: users::Model,
        /// Database identity of the authenticated browser session. Kept out
        /// of serialized contexts because it is an internal authorization
        /// handle, not API response data.
        #[serde(skip)]
        session_id: Option<i32>,
    },
    CliToken {
        user: users::Model,
    },
    ApiKey {
        user: users::Model,
        role: Option<Role>,                   // None for custom permissions
        permissions: Option<Vec<Permission>>, // Some for custom permissions
        key_name: String,
        key_id: i32,
    },
    /// Deployment token for machine-to-machine API access
    /// Used by deployed applications to access Temps APIs
    DeploymentToken {
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        token_id: i32,
        token_name: String,
        permissions: Vec<DeploymentTokenPermission>,
    },
}

// Schema version for OpenAPI documentation
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub enum AuthSourceSchema {
    Session {
        user: UserSchema,
    },
    CliToken {
        user: UserSchema,
    },
    ApiKey {
        user: UserSchema,
        role: Option<Role>,
        permissions: Option<Vec<Permission>>,
        key_name: String,
        key_id: i32,
    },
    DeploymentToken {
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        token_id: i32,
        token_name: String,
        permissions: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthContext {
    /// User associated with this auth context (None for deployment tokens)
    pub user: Option<users::Model>,
    pub source: AuthSource,
    pub effective_role: Role,
    pub custom_permissions: Option<Vec<Permission>>, // Some for custom permissions
    /// Deployment token permissions (separate from user permissions)
    pub deployment_token_permissions: Option<Vec<DeploymentTokenPermission>>,
}

// Schema version for OpenAPI documentation
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AuthContextSchema {
    pub user: Option<UserSchema>,
    pub source: AuthSourceSchema,
    pub effective_role: Role,
    pub custom_permissions: Option<Vec<Permission>>,
    pub deployment_token_permissions: Option<Vec<String>>,
}

impl AuthContext {
    pub fn new_session(user: users::Model, role: Role) -> Self {
        Self {
            user: Some(user.clone()),
            source: AuthSource::Session {
                user,
                session_id: None,
            },
            effective_role: role,
            custom_permissions: None,
            deployment_token_permissions: None,
        }
    }

    /// Construct a browser-session context backed by a persisted session row.
    /// Production middleware uses this constructor so sensitive-action checks
    /// can scope elevation to this device. `new_session` remains convenient for
    /// unit tests that do not exercise session-bound policy.
    pub fn new_persisted_session(user: users::Model, role: Role, session_id: i32) -> Self {
        Self {
            user: Some(user.clone()),
            source: AuthSource::Session {
                user,
                session_id: Some(session_id),
            },
            effective_role: role,
            custom_permissions: None,
            deployment_token_permissions: None,
        }
    }

    pub fn new_cli_token(user: users::Model, role: Role) -> Self {
        Self {
            user: Some(user.clone()),
            source: AuthSource::CliToken { user },
            effective_role: role,
            custom_permissions: None,
            deployment_token_permissions: None,
        }
    }

    pub fn new_api_key(
        user: users::Model,
        role: Option<Role>,
        permissions: Option<Vec<Permission>>,
        key_name: String,
        key_id: i32,
    ) -> Self {
        Self {
            user: Some(user.clone()),
            source: AuthSource::ApiKey {
                user,
                role: role.clone(),
                permissions: permissions.clone(),
                key_name,
                key_id,
            },
            effective_role: role.unwrap_or(Role::Custom),
            custom_permissions: permissions,
            deployment_token_permissions: None,
        }
    }

    /// Create auth context for a deployment token
    /// Deployment tokens are associated with projects, not users
    pub fn new_deployment_token(
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        token_id: i32,
        token_name: String,
        permissions: Vec<DeploymentTokenPermission>,
    ) -> Self {
        Self {
            user: None, // Deployment tokens don't have an associated user
            source: AuthSource::DeploymentToken {
                project_id,
                environment_id,
                deployment_id,
                token_id,
                token_name,
                permissions: permissions.clone(),
            },
            effective_role: Role::Custom, // Use Custom role for deployment tokens
            custom_permissions: None,
            deployment_token_permissions: Some(permissions),
        }
    }

    pub fn has_permission(&self, permission: &Permission) -> bool {
        // Deployment tokens are project-scoped machine credentials, not
        // control-plane principals. They may satisfy only the small set of
        // standard permissions that are explicitly mapped to deployment-token
        // permissions below; even deployment-token FullAccess must not become
        // blanket access to unrelated admin APIs such as UsersWrite or
        // SettingsWrite. Endpoints intended specifically for deployment tokens
        // should use has_deployment_permission instead.
        if self.is_deployment_token() {
            let Some(ref dt_permissions) = self.deployment_token_permissions else {
                return false;
            };

            let required_dt_perm = match permission {
                Permission::AnalyticsRead => DeploymentTokenPermission::AnalyticsRead,
                Permission::AnalyticsWrite => DeploymentTokenPermission::VisitorsEnrich,
                Permission::EmailsSend => DeploymentTokenPermission::EmailsSend,
                Permission::AiGatewayExecute => DeploymentTokenPermission::AiGatewayExecute,
                Permission::FlagsRead => DeploymentTokenPermission::FlagsRead,
                Permission::BlobRead => DeploymentTokenPermission::BlobRead,
                Permission::BlobWrite => DeploymentTokenPermission::BlobWrite,
                Permission::BlobDelete => DeploymentTokenPermission::BlobDelete,
                Permission::KvRead => DeploymentTokenPermission::KvRead,
                Permission::KvWrite => DeploymentTokenPermission::KvWrite,
                Permission::KvDelete => DeploymentTokenPermission::KvDelete,
                _ => return false,
            };

            return dt_permissions
                .iter()
                .any(|dt_permission| dt_permission.grants(&required_dt_perm));
        }

        // Check custom permissions first
        if let Some(ref permissions) = self.custom_permissions {
            return permissions.contains(permission);
        }

        // Fall back to role-based permissions
        self.effective_role.has_permission(permission)
    }

    /// Check if this deployment token has a specific deployment token permission
    pub fn has_deployment_permission(&self, permission: &DeploymentTokenPermission) -> bool {
        if let Some(ref permissions) = self.deployment_token_permissions {
            // FullAccess grants everything
            if permissions.contains(&DeploymentTokenPermission::FullAccess) {
                return true;
            }
            return permissions.contains(permission);
        }
        false
    }

    pub fn has_role(&self, role: &Role) -> bool {
        &self.effective_role == role
    }

    pub fn is_admin(&self) -> bool {
        self.has_role(&Role::Admin)
    }

    /// Get the user ID if available
    /// Returns None for deployment tokens
    pub fn user_id(&self) -> i32 {
        self.user.as_ref().map(|u| u.id).unwrap_or(0)
    }

    /// Get the user ID as Option
    pub fn user_id_opt(&self) -> Option<i32> {
        self.user.as_ref().map(|u| u.id)
    }

    pub fn is_api_key(&self) -> bool {
        matches!(self.source, AuthSource::ApiKey { .. })
    }

    pub fn is_session(&self) -> bool {
        matches!(self.source, AuthSource::Session { .. })
    }

    pub fn is_cli_token(&self) -> bool {
        matches!(self.source, AuthSource::CliToken { .. })
    }

    pub fn is_deployment_token(&self) -> bool {
        matches!(self.source, AuthSource::DeploymentToken { .. })
    }

    /// Convert the authentication context into the stable principal shape used
    /// by sensitive-action policy implementations.
    pub fn sensitive_action_principal(&self) -> Option<temps_core::SensitiveActionPrincipal> {
        match &self.source {
            AuthSource::Session {
                user,
                session_id: Some(session_id),
            } => Some(temps_core::SensitiveActionPrincipal::UserSession {
                user_id: user.id,
                session_id: *session_id,
                mfa_enabled: user.mfa_enabled,
            }),
            AuthSource::Session {
                session_id: None, ..
            } => None,
            AuthSource::ApiKey { user, key_id, .. } => {
                Some(temps_core::SensitiveActionPrincipal::ApiKey {
                    user_id: user.id,
                    key_id: *key_id,
                })
            }
            AuthSource::CliToken { user } => {
                Some(temps_core::SensitiveActionPrincipal::CliToken { user_id: user.id })
            }
            AuthSource::DeploymentToken { token_id, .. } => {
                Some(temps_core::SensitiveActionPrincipal::DeploymentToken {
                    token_id: *token_id,
                })
            }
        }
    }

    pub fn session_id(&self) -> Option<i32> {
        match &self.source {
            AuthSource::Session { session_id, .. } => *session_id,
            AuthSource::CliToken { .. }
            | AuthSource::ApiKey { .. }
            | AuthSource::DeploymentToken { .. } => None,
        }
    }

    pub fn api_key_info(&self) -> Option<(String, i32)> {
        match &self.source {
            AuthSource::ApiKey {
                key_name, key_id, ..
            } => Some((key_name.clone(), *key_id)),
            _ => None,
        }
    }

    /// Get deployment token info if this is a deployment token auth
    pub fn deployment_token_info(&self) -> Option<DeploymentTokenInfo> {
        match &self.source {
            AuthSource::DeploymentToken {
                project_id,
                environment_id,
                deployment_id,
                token_id,
                token_name,
                ..
            } => Some(DeploymentTokenInfo {
                project_id: *project_id,
                environment_id: *environment_id,
                deployment_id: *deployment_id,
                token_id: *token_id,
                token_name: token_name.clone(),
            }),
            _ => None,
        }
    }

    /// Get the project ID for deployment tokens
    pub fn project_id(&self) -> Option<i32> {
        match &self.source {
            AuthSource::DeploymentToken { project_id, .. } => Some(*project_id),
            _ => None,
        }
    }

    /// Get the deployment ID for deployment tokens
    pub fn deployment_id(&self) -> Option<i32> {
        match &self.source {
            AuthSource::DeploymentToken { deployment_id, .. } => *deployment_id,
            _ => None,
        }
    }

    /// Get the user, returning an error if this is a deployment token auth
    /// Use this for handlers that require a user
    pub fn require_user(&self) -> Result<&users::Model, &'static str> {
        self.user
            .as_ref()
            .ok_or("This endpoint requires user authentication. Deployment tokens are not allowed.")
    }

    /// Whether this auth context is permitted to act on `project_id`.
    ///
    /// A deployment token is bound to exactly one project at issuance. It must
    /// only ever touch that project. This is the tenant boundary that
    /// `permission_guard!` alone does NOT enforce: the guard proves the caller
    /// holds an explicitly mapped permission, not that the resource is theirs.
    ///
    /// For user/API-key/CLI auth this returns `true` (project-level ACLs for
    /// human principals are an Enterprise/RBAC concern handled elsewhere); the
    /// check exists specifically to stop a project-scoped machine credential
    /// from reaching another tenant's resources (cross-project IDOR).
    ///
    /// Handlers should prefer the [`project_scope_guard!`] macro, which turns a
    /// failure into an RFC 7807 403 response.
    pub fn is_scoped_to_project(&self, project_id: i32) -> bool {
        match self.project_id() {
            // Deployment token: must match its bound project exactly.
            Some(token_project_id) => token_project_id == project_id,
            // Not a deployment token (user/API key/CLI/session): no per-project
            // confinement at this layer.
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deployment_token_ctx(
        project_id: i32,
        permissions: Vec<DeploymentTokenPermission>,
    ) -> AuthContext {
        AuthContext::new_deployment_token(
            project_id,
            None,
            None,
            1,
            "test-token".to_string(),
            permissions,
        )
    }

    #[test]
    fn deployment_token_storage_permissions_map_to_standard_permissions() {
        let blob_read = deployment_token_ctx(7, vec![DeploymentTokenPermission::BlobRead]);
        assert!(blob_read.has_permission(&Permission::BlobRead));
        assert!(!blob_read.has_permission(&Permission::BlobWrite));

        let kv_write = deployment_token_ctx(7, vec![DeploymentTokenPermission::KvWrite]);
        assert!(kv_write.has_permission(&Permission::KvWrite));
        assert!(!kv_write.has_permission(&Permission::KvDelete));
    }

    #[test]
    fn deployment_token_is_scoped_to_its_own_project() {
        let ctx = deployment_token_ctx(7, vec![DeploymentTokenPermission::FullAccess]);
        assert!(ctx.is_scoped_to_project(7));
    }

    #[test]
    fn deployment_token_rejected_for_other_project_even_with_full_access() {
        let ctx = deployment_token_ctx(7, vec![DeploymentTokenPermission::FullAccess]);
        assert!(
            !ctx.is_scoped_to_project(8),
            "a project-7 token must not be scoped to project 8"
        );
    }

    #[test]
    fn deployment_token_full_access_does_not_grant_control_plane_permissions() {
        let ctx = deployment_token_ctx(7, vec![DeploymentTokenPermission::FullAccess]);

        assert!(!ctx.has_permission(&Permission::UsersWrite));
        assert!(!ctx.has_permission(&Permission::SettingsWrite));
        assert!(!ctx.has_permission(&Permission::DeploymentTokensCreate));
    }

    #[test]
    fn deployment_token_full_access_only_grants_explicitly_mapped_standard_permissions() {
        let ctx = deployment_token_ctx(7, vec![DeploymentTokenPermission::FullAccess]);

        assert!(ctx.has_permission(&Permission::AnalyticsRead));
        assert!(ctx.has_permission(&Permission::AnalyticsWrite));
    }

    #[test]
    fn deployment_token_full_access_grants_emails_send() {
        // Deployed apps use their injected deployment token to call POST /emails;
        // FullAccess must keep satisfying EmailsSend.
        let ctx = deployment_token_ctx(7, vec![DeploymentTokenPermission::FullAccess]);
        assert!(ctx.has_permission(&Permission::EmailsSend));
    }

    #[test]
    fn deployment_token_emails_send_permission_grants_emails_send() {
        let ctx = deployment_token_ctx(7, vec![DeploymentTokenPermission::EmailsSend]);
        assert!(ctx.has_permission(&Permission::EmailsSend));
        // ...but a narrow emails:send token must not gain analytics access.
        assert!(!ctx.has_permission(&Permission::AnalyticsRead));
        assert!(!ctx.has_permission(&Permission::AnalyticsWrite));
    }

    #[test]
    fn deployment_token_without_emails_send_is_denied_emails_send() {
        let ctx = deployment_token_ctx(7, vec![DeploymentTokenPermission::AnalyticsRead]);
        assert!(!ctx.has_permission(&Permission::EmailsSend));
    }

    #[test]
    fn deployment_token_full_access_grants_ai_gateway_execute() {
        // Deployed apps use their injected deployment token to call the AI
        // gateway; the default auto-minted token carries FullAccess, so it
        // must satisfy AiGatewayExecute — same contract as EmailsSend.
        let ctx = deployment_token_ctx(7, vec![DeploymentTokenPermission::FullAccess]);
        assert!(ctx.has_permission(&Permission::AiGatewayExecute));
    }

    #[test]
    fn deployment_token_ai_gateway_permission_grants_only_ai_gateway() {
        let ctx = deployment_token_ctx(7, vec![DeploymentTokenPermission::AiGatewayExecute]);
        assert!(ctx.has_permission(&Permission::AiGatewayExecute));
        // ...but a narrow ai_gateway:execute token must not gain other access.
        assert!(!ctx.has_permission(&Permission::EmailsSend));
        assert!(!ctx.has_permission(&Permission::AnalyticsRead));
    }

    #[test]
    fn deployment_token_without_ai_gateway_is_denied_ai_gateway_execute() {
        let ctx = deployment_token_ctx(7, vec![DeploymentTokenPermission::AnalyticsRead]);
        assert!(!ctx.has_permission(&Permission::AiGatewayExecute));
        // AI gateway read/write management APIs stay control-plane only,
        // even for FullAccess tokens.
        let full = deployment_token_ctx(7, vec![DeploymentTokenPermission::FullAccess]);
        assert!(!full.has_permission(&Permission::AiGatewayRead));
        assert!(!full.has_permission(&Permission::AiGatewayWrite));
    }

    #[test]
    fn deployment_token_rejected_for_other_project_with_narrow_permission() {
        let ctx = deployment_token_ctx(7, vec![DeploymentTokenPermission::AnalyticsRead]);
        assert!(ctx.is_scoped_to_project(7));
        assert!(!ctx.is_scoped_to_project(8));
    }
}
