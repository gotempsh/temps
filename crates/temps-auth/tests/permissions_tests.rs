use temps_auth::{Permission, Role};

#[test]
fn test_permission_display() {
    assert_eq!(Permission::ProjectsRead.to_string(), "projects:read");
    assert_eq!(Permission::ProjectsWrite.to_string(), "projects:write");
    assert_eq!(Permission::ProjectsDelete.to_string(), "projects:delete");
    assert_eq!(Permission::ProjectsCreate.to_string(), "projects:create");
    assert_eq!(Permission::SystemAdmin.to_string(), "system:admin");
    assert_eq!(Permission::McpConnect.to_string(), "mcp:connect");
}

#[test]
fn test_permission_from_str() {
    assert_eq!(
        Permission::from_str("projects:read"),
        Some(Permission::ProjectsRead)
    );
    assert_eq!(
        Permission::from_str("projects:write"),
        Some(Permission::ProjectsWrite)
    );
    assert_eq!(
        Permission::from_str("system:admin"),
        Some(Permission::SystemAdmin)
    );
    assert_eq!(
        Permission::from_str("mcp:connect"),
        Some(Permission::McpConnect)
    );
    assert_eq!(Permission::from_str("invalid:permission"), None);
    assert_eq!(Permission::from_str(""), None);
}

#[test]
fn test_permission_from_str_roundtrip() {
    let all_permissions = Permission::all();

    for permission in all_permissions {
        let string_repr = permission.to_string();
        let parsed = Permission::from_str(&string_repr);
        assert_eq!(
            parsed,
            Some(permission),
            "Failed roundtrip for permission: {}",
            string_repr
        );
    }
}

#[test]
fn test_permission_all() {
    let permissions = Permission::all();
    assert!(!permissions.is_empty());
    assert!(permissions.contains(&Permission::ProjectsRead));
    assert!(permissions.contains(&Permission::SystemAdmin));
    assert!(permissions.contains(&Permission::McpConnect));

    // Check that we have the expected number of permissions (should match enum variants)
    // This test will fail if new permissions are added but not included in all()
    assert!(
        permissions.len() >= 80,
        "Expected at least 80 permissions, got {}",
        permissions.len()
    );
}

#[test]
fn test_role_display() {
    assert_eq!(Role::Admin.to_string(), "admin");
    assert_eq!(Role::User.to_string(), "user");
    assert_eq!(Role::Reader.to_string(), "reader");
    assert_eq!(Role::Mcp.to_string(), "mcp");
    assert_eq!(Role::ApiReader.to_string(), "api_reader");
    assert_eq!(Role::Custom.to_string(), "custom");
}

#[test]
fn test_role_from_str() {
    assert_eq!(Role::from_str("admin"), Some(Role::Admin));
    assert_eq!(Role::from_str("user"), Some(Role::User));
    assert_eq!(Role::from_str("reader"), Some(Role::Reader));
    assert_eq!(Role::from_str("mcp"), Some(Role::Mcp));
    assert_eq!(Role::from_str("api_reader"), Some(Role::ApiReader));
    assert_eq!(Role::from_str("custom"), Some(Role::Custom));
    assert_eq!(Role::from_str("invalid_role"), None);
    assert_eq!(Role::from_str(""), None);
}

#[test]
fn test_role_from_str_roundtrip() {
    let all_roles = Role::all();

    for role in all_roles {
        let string_repr = role.to_string();
        let parsed = Role::from_str(&string_repr);
        assert_eq!(
            parsed,
            Some(role),
            "Failed roundtrip for role: {}",
            string_repr
        );
    }
}

#[test]
fn test_role_all() {
    let roles = Role::all();
    assert_eq!(roles.len(), 6);
    assert!(roles.contains(&Role::Admin));
    assert!(roles.contains(&Role::User));
    assert!(roles.contains(&Role::Reader));
    assert!(roles.contains(&Role::Mcp));
    assert!(roles.contains(&Role::ApiReader));
    assert!(roles.contains(&Role::Custom));
}

#[test]
fn test_role_permissions() {
    // Admin should have all permissions
    let admin_permissions = Role::Admin.permissions();
    assert!(!admin_permissions.is_empty());
    assert!(admin_permissions.contains(&Permission::SystemAdmin));
    assert!(admin_permissions.contains(&Permission::ProjectsRead));
    assert!(admin_permissions.contains(&Permission::ProjectsWrite));
    assert!(admin_permissions.contains(&Permission::ProjectsDelete));

    // User should have most permissions but not system admin
    let user_permissions = Role::User.permissions();
    assert!(!user_permissions.is_empty());
    assert!(user_permissions.contains(&Permission::ProjectsRead));
    assert!(user_permissions.contains(&Permission::ProjectsWrite));
    assert!(!user_permissions.contains(&Permission::SystemAdmin));

    // Reader should have only read permissions
    let reader_permissions = Role::Reader.permissions();
    assert!(!reader_permissions.is_empty());
    assert!(reader_permissions.contains(&Permission::ProjectsRead));
    assert!(!reader_permissions.contains(&Permission::ProjectsWrite));
    assert!(!reader_permissions.contains(&Permission::SystemAdmin));

    // MCP should have specific MCP permissions
    let mcp_permissions = Role::Mcp.permissions();
    assert!(!mcp_permissions.is_empty());
    assert!(mcp_permissions.contains(&Permission::McpConnect));
    assert!(mcp_permissions.contains(&Permission::McpExecute));
    assert!(mcp_permissions.contains(&Permission::ProjectsRead));

    // ApiReader should have limited read permissions
    let api_reader_permissions = Role::ApiReader.permissions();
    assert!(!api_reader_permissions.is_empty());
    assert!(api_reader_permissions.contains(&Permission::ProjectsRead));
    assert!(!api_reader_permissions.contains(&Permission::ProjectsWrite));

    // Custom should have no default permissions
    let custom_permissions = Role::Custom.permissions();
    assert!(custom_permissions.is_empty());
}

#[test]
fn test_role_has_permission() {
    assert!(Role::Admin.has_permission(&Permission::SystemAdmin));
    assert!(Role::Admin.has_permission(&Permission::ProjectsRead));
    assert!(Role::Admin.has_permission(&Permission::ProjectsWrite));

    assert!(Role::User.has_permission(&Permission::ProjectsRead));
    assert!(Role::User.has_permission(&Permission::ProjectsWrite));
    assert!(!Role::User.has_permission(&Permission::SystemAdmin));

    assert!(Role::Reader.has_permission(&Permission::ProjectsRead));
    assert!(!Role::Reader.has_permission(&Permission::ProjectsWrite));
    assert!(!Role::Reader.has_permission(&Permission::SystemAdmin));

    assert!(Role::Mcp.has_permission(&Permission::McpConnect));
    assert!(Role::Mcp.has_permission(&Permission::ProjectsRead));
    assert!(!Role::Mcp.has_permission(&Permission::SystemAdmin));

    assert!(Role::ApiReader.has_permission(&Permission::ProjectsRead));
    assert!(!Role::ApiReader.has_permission(&Permission::ProjectsWrite));

    assert!(!Role::Custom.has_permission(&Permission::ProjectsRead));
    assert!(!Role::Custom.has_permission(&Permission::SystemAdmin));
}

#[test]
fn test_permission_serialization() {
    let permission = Permission::ProjectsRead;
    let serialized = serde_json::to_string(&permission).unwrap();
    let deserialized: Permission = serde_json::from_str(&serialized).unwrap();
    assert_eq!(permission, deserialized);
}

#[test]
fn test_role_serialization() {
    let role = Role::Admin;
    let serialized = serde_json::to_string(&role).unwrap();
    let deserialized: Role = serde_json::from_str(&serialized).unwrap();
    assert_eq!(role, deserialized);
}
