use super::permissions::{Permission, Role};
use serde::{Deserialize, Serialize};
use temps_entities::deployment_tokens::DeploymentTokenPermission;
use temps_entities::users;
use utoipa::ToSchema;

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
            source: AuthSource::Session { user },
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
        token_id: i32,
        token_name: String,
        permissions: Vec<DeploymentTokenPermission>,
    ) -> Self {
        Self {
            user: None, // Deployment tokens don't have an associated user
            source: AuthSource::DeploymentToken {
                project_id,
                environment_id,
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
        // Deployment tokens don't have standard Permission checks
        // They use DeploymentTokenPermission instead
        if self.is_deployment_token() {
            return false;
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

    pub fn api_key_info(&self) -> Option<(String, i32)> {
        match &self.source {
            AuthSource::ApiKey {
                key_name, key_id, ..
            } => Some((key_name.clone(), *key_id)),
            _ => None,
        }
    }

    /// Get deployment token info if this is a deployment token auth
    pub fn deployment_token_info(&self) -> Option<(i32, Option<i32>, i32, String)> {
        match &self.source {
            AuthSource::DeploymentToken {
                project_id,
                environment_id,
                token_id,
                token_name,
                ..
            } => Some((*project_id, *environment_id, *token_id, token_name.clone())),
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

    /// Get the user, returning an error if this is a deployment token auth
    /// Use this for handlers that require a user
    pub fn require_user(&self) -> Result<&users::Model, &'static str> {
        self.user
            .as_ref()
            .ok_or("This endpoint requires user authentication. Deployment tokens are not allowed.")
    }
}
