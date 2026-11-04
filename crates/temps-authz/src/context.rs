// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

/// The standard permissions that stand behind a deployment-token permission.
///
/// This is the inverse of the mapping in [`AuthContext::has_permission`]: a
/// token holding `permission` can do what these standard permissions allow, so
/// whoever hands it out must hold all of them. `FullAccess` expands to every
/// other token permission. `EventsWrite` is not checked anywhere today, so
/// nothing stands behind it.
pub fn standard_permissions_behind(permission: &DeploymentTokenPermission) -> Vec<Permission> {
    use DeploymentTokenPermission as Token;
    match permission {
        Token::VisitorsEnrich => vec![Permission::AnalyticsWrite],
        Token::EmailsSend => vec![Permission::EmailsSend],
        Token::AnalyticsRead => vec![Permission::AnalyticsRead],
        Token::EventsWrite => vec![],
        Token::ErrorsRead => vec![Permission::ErrorTrackingRead],
        Token::AiGatewayExecute => vec![Permission::AiGatewayExecute],
        Token::FlagsRead => vec![Permission::FlagsRead],
        Token::BlobRead => vec![Permission::BlobRead],
        Token::BlobWrite => vec![Permission::BlobWrite],
        Token::BlobDelete => vec![Permission::BlobDelete],
        Token::KvRead => vec![Permission::KvRead],
        Token::KvWrite => vec![Permission::KvWrite],
        Token::KvDelete => vec![Permission::KvDelete],
        Token::FullAccess => Token::all()
            .iter()
            .filter(|each| **each != Token::FullAccess)
            .flat_map(standard_permissions_behind)
            .collect(),
    }
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

    /// The first standard permission this principal lacks that is needed to
    /// hand `permission` to a deployment token, or `None` when the grant is
    /// within its own permissions.
    ///
    /// A deployment token must never carry more than the principal that
    /// created or edited it, the same ceiling API keys have.
    pub fn missing_permission_to_grant(
        &self,
        permission: &DeploymentTokenPermission,
    ) -> Option<Permission> {
        standard_permissions_behind(permission)
            .into_iter()
            .find(|required| !self.has_permission(required))
    }

    pub fn has_role(&self, role: &Role) -> bool {
        &self.effective_role == role
    }

    pub fn is_admin(&self) -> bool {
        self.has_role(&Role::Admin)
    }

    /// Whether this principal runs the *instance*, not merely some resource in
    /// it.
    ///
    /// A permission answers "may you do this kind of thing"; this answers "is
    /// this whole installation yours". They are different questions, and a
    /// handful of operations need the second one: anything whose blast radius is
    /// instance-wide by construction — every project at once, or the safety
    /// margin on a guard that spends the instance's money — has no path
    /// parameter for a project-scoped guard to narrow, so a project-scoped token
    /// or a per-project role must not reach it at all.
    ///
    /// Defined once, here, rather than re-derived at each call site: three
    /// separate copies of `is_admin() || has_role(PlatformAdmin)` is three
    /// chances for one of them to drift into a weaker bar than the operation it
    /// is standing in front of, which is precisely the class of bug this
    /// predicate exists to prevent.
    pub fn is_instance_admin(&self) -> bool {
        self.is_admin() || self.has_role(&Role::PlatformAdmin)
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

    fn test_user() -> users::Model {
        let now = chrono::Utc::now();
        users::Model {
            id: 42,
            name: "Test User".to_string(),
            email: "user42@example.com".to_string(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn user_role_cannot_grant_token_permissions_it_does_not_hold() {
        let user = AuthContext::new_session(test_user(), Role::User);
        for denied in [
            DeploymentTokenPermission::VisitorsEnrich,
            DeploymentTokenPermission::BlobDelete,
            DeploymentTokenPermission::KvDelete,
            DeploymentTokenPermission::FullAccess,
        ] {
            assert!(
                user.missing_permission_to_grant(&denied).is_some(),
                "a user must not be able to grant {}",
                denied.as_str()
            );
        }
        assert_eq!(
            user.missing_permission_to_grant(&DeploymentTokenPermission::VisitorsEnrich),
            Some(Permission::AnalyticsWrite)
        );
    }

    #[test]
    fn user_role_can_grant_token_permissions_it_holds() {
        let user = AuthContext::new_session(test_user(), Role::User);
        for allowed in [
            DeploymentTokenPermission::AnalyticsRead,
            DeploymentTokenPermission::EmailsSend,
            DeploymentTokenPermission::ErrorsRead,
            DeploymentTokenPermission::EventsWrite,
            DeploymentTokenPermission::AiGatewayExecute,
            DeploymentTokenPermission::FlagsRead,
            DeploymentTokenPermission::BlobRead,
            DeploymentTokenPermission::BlobWrite,
            DeploymentTokenPermission::KvRead,
            DeploymentTokenPermission::KvWrite,
        ] {
            assert_eq!(
                user.missing_permission_to_grant(&allowed),
                None,
                "a user holds what {} needs",
                allowed.as_str()
            );
        }
    }

    #[test]
    fn admin_can_grant_every_token_permission() {
        let admin = AuthContext::new_session(test_user(), Role::Admin);
        for permission in DeploymentTokenPermission::all() {
            assert_eq!(
                admin.missing_permission_to_grant(&permission),
                None,
                "an admin holds what {} needs",
                permission.as_str()
            );
        }
    }

    #[test]
    fn custom_api_key_can_grant_only_the_permissions_it_holds() {
        let key = AuthContext::new_api_key(
            test_user(),
            None,
            Some(vec![Permission::BlobRead, Permission::KvRead]),
            "ci".to_string(),
            7,
        );
        assert_eq!(
            key.missing_permission_to_grant(&DeploymentTokenPermission::BlobRead),
            None
        );
        assert_eq!(
            key.missing_permission_to_grant(&DeploymentTokenPermission::BlobWrite),
            Some(Permission::BlobWrite)
        );
        assert!(key
            .missing_permission_to_grant(&DeploymentTokenPermission::FullAccess)
            .is_some());
    }

    #[test]
    fn grant_requirements_match_what_a_token_can_use() {
        // Every token permission that gates something must map to the standard
        // permission `has_permission` accepts for it, so the ceiling cannot
        // drift from the mapping tokens are checked against. `errors:read` and
        // `events:write` are not checked anywhere, so they are left out.
        for permission in DeploymentTokenPermission::all() {
            if matches!(
                permission,
                DeploymentTokenPermission::FullAccess
                    | DeploymentTokenPermission::ErrorsRead
                    | DeploymentTokenPermission::EventsWrite
            ) {
                continue;
            }
            let token = deployment_token_ctx(7, vec![permission.clone()]);
            for required in standard_permissions_behind(&permission) {
                assert!(
                    token.has_permission(&required),
                    "a {} token should satisfy {}",
                    permission.as_str(),
                    required
                );
            }
        }
    }

    #[test]
    fn every_permission_a_token_can_use_is_a_grant_requirement() {
        // The other direction: if a token can satisfy a standard permission,
        // whoever hands out the token permission that satisfies it must be
        // required to hold it. A requirement dropped from
        // `standard_permissions_behind` would otherwise let a caller grant
        // more than it holds without any test noticing.
        let full = deployment_token_ctx(7, vec![DeploymentTokenPermission::FullAccess]);
        let usable: Vec<Permission> = Permission::all()
            .into_iter()
            .filter(|permission| full.has_permission(permission))
            .collect();
        assert!(!usable.is_empty(), "a full-access token can use something");

        for permission in usable {
            let granted_by: Vec<DeploymentTokenPermission> = DeploymentTokenPermission::all()
                .into_iter()
                .filter(|candidate| *candidate != DeploymentTokenPermission::FullAccess)
                .filter(|candidate| {
                    deployment_token_ctx(7, vec![candidate.clone()]).has_permission(&permission)
                })
                .collect();
            assert!(
                !granted_by.is_empty(),
                "{permission} is usable by a full-access token, so some token permission must grant it"
            );
            for candidate in granted_by {
                assert!(
                    standard_permissions_behind(&candidate).contains(&permission),
                    "granting {} gives a token {permission}, so it must require it",
                    candidate.as_str()
                );
            }
            assert!(
                standard_permissions_behind(&DeploymentTokenPermission::FullAccess)
                    .contains(&permission),
                "full access gives a token {permission}, so it must require it"
            );
        }
    }
}
