// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::permissions::{Permission, Role};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Response containing all available permissions for frontend validation
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AvailablePermissions {
    /// All available permissions in the system
    pub permissions: Vec<PermissionInfo>,
    /// All available roles
    pub roles: Vec<RoleInfo>,
}

/// Information about a single permission
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PermissionInfo {
    /// The permission identifier (e.g., "projects:read")
    pub name: String,
    /// Human-readable description of the permission
    pub description: String,
    /// Category of the permission (e.g., "Projects", "Deployments")
    pub category: String,
}

/// Information about a role
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RoleInfo {
    /// The role identifier (e.g., "admin")
    pub name: String,
    /// Human-readable description of the role
    pub description: String,
    /// Permissions included in this role
    pub permissions: Vec<String>,
}

impl PermissionInfo {
    pub fn from_permission(perm: &Permission) -> Self {
        let name = perm.to_string();
        let parts: Vec<&str> = name.split(':').collect();
        let category = parts.first().unwrap_or(&"general").to_string();

        let description = match perm {
            Permission::ProjectsRead => "View projects and their details",
            Permission::ProjectsWrite => "Modify existing projects",
            Permission::ProjectsDelete => "Delete projects",
            Permission::ProjectsCreate => "Create new projects",
            Permission::DeploymentsRead => "View deployments and their status",
            Permission::DeploymentsWrite => "Modify deployment configurations",
            Permission::DeploymentsDelete => "Delete deployments",
            Permission::DeploymentsCreate => "Create new deployments",
            Permission::DomainsRead => "View domain configurations",
            Permission::DomainsWrite => "Modify domain settings",
            Permission::DomainsDelete => "Delete domains",
            Permission::DomainsCreate => "Add new domains",
            Permission::EnvironmentsRead => "View environment variables and settings",
            Permission::EnvironmentsWrite => "Modify environment configurations",
            Permission::EnvironmentsDelete => "Delete environments",
            Permission::EnvironmentsCreate => "Create new environments",
            Permission::AnalyticsRead => "View analytics and metrics",
            Permission::AnalyticsWrite => "Modify analytics settings",
            Permission::UsersRead => "View user information",
            Permission::UsersWrite => "Modify user settings",
            Permission::UsersDelete => "Delete users",
            Permission::UsersCreate => "Create new users",
            Permission::SystemAdmin => "Full system administration access",
            Permission::SystemRead => "View system configuration",
            Permission::SecretsRead => "Reveal plaintext credentials and environment values",
            Permission::ApiKeysRead => "View API keys",
            Permission::ApiKeysWrite => "Modify API keys",
            Permission::ApiKeysDelete => "Delete API keys",
            Permission::ApiKeysCreate => "Create new API keys",
            Permission::AuditRead => "View audit logs",
            Permission::BackupsRead => "View backup configurations",
            Permission::BackupsWrite => "Modify backup settings",
            Permission::BackupsDelete => "Delete backups",
            Permission::BackupsCreate => "Create new backups",
            // Add descriptions for any new permissions
            _ => "Permission for this resource",
        }
        .to_string();

        PermissionInfo {
            name: name.clone(),
            description,
            category: category.to_uppercase(),
        }
    }
}

impl RoleInfo {
    pub fn from_role(role: &Role) -> Self {
        let description = match role {
            Role::Admin => "Full administrative access to all resources",
            Role::PlatformAdmin => {
                "Platform administration (users, settings, system) without deploy access to projects or deployments"
            }
            Role::User => {
                "Manage every existing and future project without user or system administration"
            }
            Role::Reader => "Read-only access to resources",
            Role::ApiReader => "Read-only API access",
            Role::Custom => "Custom role with specific permissions",
            Role::MetricsIngest => "Token for infrastructure metrics ingest (si_ prefix)",
        }
        .to_string();

        let permissions = role.permissions().iter().map(|p| p.to_string()).collect();

        RoleInfo {
            name: role.to_string(),
            description,
            permissions,
        }
    }
}

/// Get all available permissions and roles for frontend
pub fn get_available_permissions() -> AvailablePermissions {
    let permissions = Permission::all()
        .iter()
        .map(PermissionInfo::from_permission)
        .collect();

    let roles = Role::all().iter().map(RoleInfo::from_role).collect();

    AvailablePermissions { permissions, roles }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn role_catalog_exposes_the_exact_permission_set() {
        let catalog = get_available_permissions();
        let user = catalog
            .roles
            .iter()
            .find(|role| role.name == "user")
            .expect("user role is present");
        let expected: Vec<String> = Role::User
            .permissions()
            .iter()
            .map(ToString::to_string)
            .collect();

        assert_eq!(user.permissions, expected);
        assert!(user
            .description
            .contains("every existing and future project"));
    }

    #[test]
    fn predefined_roles_do_not_report_duplicate_permissions() {
        for role in Role::all() {
            let unique: HashSet<_> = role.permissions().iter().collect();
            assert_eq!(
                unique.len(),
                role.permissions().len(),
                "role {role} contains duplicate permissions"
            );
        }
    }
}
