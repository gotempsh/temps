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
            Permission::McpConnect => "Connect to MCP services",
            Permission::McpExecute => "Execute MCP commands",
            Permission::McpRead => "Read MCP data",
            Permission::McpWrite => "Modify MCP configurations",
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
            Role::User => "Standard user access with ability to manage own resources",
            Role::Reader => "Read-only access to resources",
            Role::Mcp => "Access for MCP service operations",
            Role::ApiReader => "Read-only API access",
            Role::Custom => "Custom role with specific permissions",
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
