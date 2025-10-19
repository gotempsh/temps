use super::permissions::{Role, Permission};
use temps_entities::users;
use serde::{Deserialize, Serialize};
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
    Session { user: users::Model },
    CliToken { user: users::Model },
    ApiKey { 
        user: users::Model, 
        role: Option<Role>,  // None for custom permissions
        permissions: Option<Vec<Permission>>,  // Some for custom permissions
        key_name: String,
        key_id: i32,
    },
}

// Schema version for OpenAPI documentation
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub enum AuthSourceSchema {
    Session { user: UserSchema },
    CliToken { user: UserSchema },
    ApiKey { 
        user: UserSchema, 
        role: Option<Role>,
        permissions: Option<Vec<Permission>>,
        key_name: String,
        key_id: i32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthContext {
    pub user: users::Model,
    pub source: AuthSource,
    pub effective_role: Role,
    pub custom_permissions: Option<Vec<Permission>>,  // Some for custom permissions
}

// Schema version for OpenAPI documentation
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AuthContextSchema {
    pub user: UserSchema,
    pub source: AuthSourceSchema,
    pub effective_role: Role,
    pub custom_permissions: Option<Vec<Permission>>,
}

impl AuthContext {
    pub fn new_session(user: users::Model, role: Role) -> Self {
        Self {
            user: user.clone(),
            source: AuthSource::Session { user },
            effective_role: role,
            custom_permissions: None,
        }
    }

    pub fn new_cli_token(user: users::Model, role: Role) -> Self {
        Self {
            user: user.clone(),
            source: AuthSource::CliToken { user },
            effective_role: role,
            custom_permissions: None,
        }
    }

    pub fn new_api_key(user: users::Model, role: Option<Role>, permissions: Option<Vec<Permission>>, key_name: String, key_id: i32) -> Self {
        Self {
            user: user.clone(),
            source: AuthSource::ApiKey { 
                user, 
                role: role.clone(),
                permissions: permissions.clone(),
                key_name,
                key_id,
            },
            effective_role: role.unwrap_or(Role::Custom),
            custom_permissions: permissions,
        }
    }

    pub fn has_permission(&self, permission: &Permission) -> bool {
        // Check custom permissions first
        if let Some(ref permissions) = self.custom_permissions {
            return permissions.contains(permission);
        }
        
        // Fall back to role-based permissions
        self.effective_role.has_permission(permission)
    }

    pub fn has_role(&self, role: &Role) -> bool {
        &self.effective_role == role
    }

    pub fn is_admin(&self) -> bool {
        self.has_role(&Role::Admin)
    }

    pub fn user_id(&self) -> i32 {
        self.user.id
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

    pub fn api_key_info(&self) -> Option<(String, i32)> {
        match &self.source {
            AuthSource::ApiKey { key_name, key_id, .. } => Some((key_name.clone(), *key_id)),
            _ => None,
        }
    }
}